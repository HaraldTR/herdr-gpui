//! TCP ports each workspace's processes listen on, for the sidebar and the
//! status bar, where clicking one opens it in a browser tab.
//!
//! Every host has its own worker thread scanning it every few seconds through
//! one long-lived `sh`: a local one for this machine, the host's own over SSH
//! for a remote one. The UI thread only starts and stops workers and takes
//! their answers on later ticks, so a slow `lsof` or an unreachable host never
//! blocks rendering or delays another host.

mod render;
mod scan;

#[cfg(test)]
mod tests;

use crate::{
    Error, Result,
    system_load::Stop,
    usage::{Host, Shell},
};
pub(crate) use render::chips;
pub(crate) use scan::{Port, Ports, parse};
use std::{
    collections::HashMap,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

/// Ports open and close at human pace; `lsof` is not free on a busy machine.
const INTERVAL: Duration = Duration::from_secs(5);
/// A host that could not be scanned is tried again after this long.
const RETRY: Duration = Duration::from_secs(30);
const STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// Hosts are few; an unbounded endpoint list still cannot start more workers.
const HOST_LIMIT: usize = 16;

/// The shell a host is scanned through.
fn open(host: &Host) -> Result<Shell> {
    match host {
        Host::Local => local_shell(),
        Host::Ssh(target) => Shell::connect(target),
    }
    .map_err(failed)
}

#[cfg(unix)]
fn local_shell() -> Result<Shell> {
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg("-s")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    Shell::start(command)
}

/// Windows has neither `ss` nor `lsof`, and its own tools are not wired up.
#[cfg(windows)]
fn local_shell() -> Result<Shell> {
    Err(Error::ListeningPortsUnsupported)
}

fn scan(shell: &mut Shell) -> Result<Ports> {
    let output = shell.run(scan::COMMAND, STEP_TIMEOUT).map_err(failed)?;
    parse(&output.stdout)
}

fn failed(error: Error) -> Error {
    Error::ListeningPorts(Box::new(error))
}

struct Worker {
    stop: Arc<Stop>,
    results: mpsc::Receiver<Result<Ports>>,
}

impl Drop for Worker {
    /// The thread ends after its current scan; nothing waits for it here.
    fn drop(&mut self) {
        self.stop.stop();
    }
}

fn spawn(host: Host) -> Option<Worker> {
    let stop = Arc::new(Stop::default());
    let (sender, results) = mpsc::sync_channel(2);
    let shared = stop.clone();
    let spawned = thread::Builder::new()
        .name("herdr-listening-ports".into())
        .spawn(move || run(&host, &shared, &sender));
    match spawned {
        Ok(_) => Some(Worker { stop, results }),
        Err(error) => {
            tracing::warn!(category = "listening-ports", %error, "could not start a port scanner");
            None
        }
    }
}

fn run(host: &Host, stop: &Stop, results: &mpsc::SyncSender<Result<Ports>>) {
    let mut shell: Option<Shell> = None;
    loop {
        let opened = match shell.take() {
            Some(shell) => Ok(shell),
            None => open(host),
        };
        let result = opened.and_then(|mut opened| {
            let ports = scan(&mut opened);
            // A failed step leaves the shell unusable; a missing tool does not.
            if !matches!(ports, Err(Error::ListeningPorts(_))) {
                shell = Some(opened);
            }
            ports
        });
        let due = Instant::now() + if result.is_ok() { INTERVAL } else { RETRY };
        match results.try_send(result) {
            // The UI is behind; it will take the next one.
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
            Err(mpsc::TrySendError::Disconnected(_)) => return,
        }
        if !stop.wait(due) {
            return;
        }
    }
}

/// One host's latest scan.
#[derive(Default)]
struct Reading {
    ports: Ports,
    /// Why the latest scan failed, as last logged; the last good ports stay
    /// shown.
    error: Option<String>,
}

impl Reading {
    /// Whether anything shown changed.
    fn apply(&mut self, result: Result<Ports>) -> bool {
        match result {
            Ok(ports) => {
                self.error = None;
                if ports == self.ports {
                    return false;
                }
                self.ports = ports;
                true
            }
            Err(error) => {
                // The display boundary: the cause says what went wrong.
                let mut text = error.to_string();
                let mut source = std::error::Error::source(&error);
                while let Some(cause) = source {
                    text.push(' ');
                    text.push_str(&cause.to_string());
                    source = cause.source();
                }
                // Nothing shown changes; the reason is logged once, not every retry.
                if self.error.as_deref() != Some(text.as_str()) {
                    tracing::warn!(category = "listening-ports", error = %text, "could not scan listening ports");
                    self.error = Some(text);
                }
                false
            }
        }
    }
}

#[derive(Default)]
struct Monitor {
    /// None once its thread could not start or has ended.
    worker: Option<Worker>,
    reading: Reading,
}

/// Every scanned host's listening ports, keyed by host.
#[derive(Default)]
pub(crate) struct ListeningPorts {
    monitors: HashMap<Host, Monitor>,
}

impl ListeningPorts {
    /// The ports `workspace` listens on, lowest first.
    pub fn get(&self, host: &Host, workspace: &str) -> &[Port] {
        self.monitors
            .get(host)
            .and_then(|monitor| monitor.reading.ports.get(workspace))
            .map_or(&[], Vec::as_slice)
    }

    /// Shows `ports` for `host` as if a scan had found them, with no worker.
    #[cfg(test)]
    pub(crate) fn seed(&mut self, host: Host, ports: Ports) {
        let reading = Reading { ports, error: None };
        self.monitors.insert(
            host,
            Monitor {
                worker: None,
                reading,
            },
        );
    }

    /// Scans exactly `hosts` (at most [`HOST_LIMIT`]): starts workers for new
    /// ones, stops and forgets the rest, and takes finished scans. Returns
    /// whether anything shown changed.
    pub fn poll(&mut self, hosts: impl IntoIterator<Item = Host>) -> bool {
        let mut wanted: Vec<Host> = Vec::new();
        for host in hosts {
            if wanted.len() == HOST_LIMIT {
                break;
            }
            // Several sessions on one machine share its scan.
            if !wanted.contains(&host) {
                wanted.push(host);
            }
        }
        let mut changed = false;
        self.monitors.retain(|host, monitor| {
            let keep = wanted.contains(host);
            changed |= !keep && !monitor.reading.ports.is_empty();
            keep
        });
        for host in wanted {
            self.monitors
                .entry(host)
                .or_insert_with_key(|host| Monitor {
                    worker: spawn(host.clone()),
                    reading: Reading::default(),
                });
        }
        for monitor in self.monitors.values_mut() {
            while let Some(worker) = &monitor.worker {
                match worker.results.try_recv() {
                    Ok(result) => changed |= monitor.reading.apply(result),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => monitor.worker = None,
                }
            }
        }
        changed
    }
}
