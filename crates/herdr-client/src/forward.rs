//! Local forwards of a saved SSH host's listening ports, one `ssh -N -L` child
//! per forward. A worker thread owns each child: it picks the local port, waits
//! until SSH listens there, and reports when the child ends. Nothing reconnects;
//! a forward that ends stays ended until someone starts it again. Callers name
//! the remote port, so this knows nothing of how that port was found.
use crate::{Error, Result, catalog::validate_target};
use std::num::NonZeroU16;
#[cfg(unix)]
use std::{
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc},
    thread,
    time::{Duration, Instant},
};

/// What a forward's worker reports: at most one `Listening`, then at most one
/// `Ended`. A forward stopped by its owner reports nothing more.
#[derive(Debug)]
pub enum ForwardEvent {
    /// SSH accepts connections on `127.0.0.1:<local_port>`.
    Listening { local_port: u16 },
    /// The child exited, or never listened; it has been killed and reaped.
    Ended(Error),
}

/// The local port tried first for `remote`: the same number, except that a
/// privileged port moves up by 10000 (80 becomes 10080) so no local privilege
/// is needed. Orca's port forwarding remaps the same way.
pub fn preferred_local_port(remote: NonZeroU16) -> u16 {
    match remote.get() {
        port @ ..1024 => port + 10000,
        port => port,
    }
}

/// SSH connects within `ConnectTimeout` and authenticates without prompting,
/// so a forward that has not listened by now never will.
#[cfg(unix)]
const READY_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(unix)]
const STARTING_POLL: Duration = Duration::from_millis(50);
#[cfg(unix)]
const LISTENING_POLL: Duration = Duration::from_millis(250);
#[cfg(unix)]
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// The child, shared so its owner can kill it at once while the worker alone
/// waits for it. Neither side holds the lock across a blocking call.
#[cfg(unix)]
#[derive(Default)]
struct Slot {
    stopped: bool,
    child: Option<Child>,
}

#[cfg(unix)]
fn lock(slot: &Mutex<Slot>) -> MutexGuard<'_, Slot> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One running forward. Dropping it stops the forward.
#[cfg(unix)]
pub struct PortForward {
    slot: Arc<Mutex<Slot>>,
    events: mpsc::Receiver<ForwardEvent>,
}

/// No SSH child is ever spawned on Windows, so this type has no values.
#[cfg(windows)]
pub enum PortForward {}

#[cfg(unix)]
impl PortForward {
    /// Forwards `remote_port` on `target`'s loopback to a local port. Returns
    /// at once: choosing the port and running SSH happen on a worker thread.
    pub fn start(target: &str, remote_port: NonZeroU16) -> Result<Self> {
        validate_target(target)?;
        let target = target.to_owned();
        Self::start_with(remote_port, READY_TIMEOUT, move |local_port| {
            command(&target, local_port, remote_port.get())
        })
    }

    fn start_with(
        remote_port: NonZeroU16,
        ready_timeout: Duration,
        command: impl FnOnce(u16) -> Command + Send + 'static,
    ) -> Result<Self> {
        let slot = Arc::new(Mutex::new(Slot::default()));
        // Two events at most, so the worker never blocks on a slow owner.
        let (sender, events) = mpsc::sync_channel(2);
        let shared = slot.clone();
        thread::Builder::new()
            .name("herdr-port-forward".into())
            .spawn(move || {
                if let Err(error) = run(&shared, remote_port, ready_timeout, command, &sender) {
                    let _ = sender.try_send(ForwardEvent::Ended(error));
                }
            })?;
        Ok(Self { slot, events })
    }

    /// The worker's next report, without waiting.
    pub fn try_event(&self) -> Option<ForwardEvent> {
        self.events.try_recv().ok()
    }

    /// Kills the child without waiting for it; the worker reaps it. Safe on
    /// the UI thread and while quitting, when the worker may never run again.
    pub fn stop(&self) {
        let mut slot = lock(&self.slot);
        slot.stopped = true;
        if let Some(child) = &mut slot.child {
            // A child the worker already reaped is not signalled again.
            let _ = child.kill();
        }
    }
}

#[cfg(unix)]
impl Drop for PortForward {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
impl PortForward {
    pub fn start(target: &str, _remote_port: NonZeroU16) -> Result<Self> {
        validate_target(target)?;
        Err(Error::SshUnsupported)
    }

    pub fn try_event(&self) -> Option<ForwardEvent> {
        match *self {}
    }

    pub fn stop(&self) {
        match *self {}
    }
}

#[cfg(unix)]
fn run(
    slot: &Mutex<Slot>,
    remote_port: NonZeroU16,
    ready_timeout: Duration,
    command: impl FnOnce(u16) -> Command,
    events: &mpsc::SyncSender<ForwardEvent>,
) -> Result<()> {
    if lock(slot).stopped {
        return Ok(());
    }
    let local_port = free_local_port(preferred_local_port(remote_port))?;
    // Stderr can carry banners or secrets; nothing reads it.
    let child = command(local_port)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Error::ForwardSpawn)?;
    // A stop that came while spawning is seen by the first check below.
    lock(slot).child = Some(child);
    let deadline = Instant::now() + ready_timeout;
    let mut listening = false;
    let ended = loop {
        let status = {
            let mut slot = lock(slot);
            let Slot { stopped, child } = &mut *slot;
            match child {
                Some(child) if !*stopped => child.try_wait(),
                _ => break None,
            }
        };
        match status {
            Ok(Some(status)) => break Some(Error::ForwardExit(status)),
            Ok(None) => {}
            Err(error) => break Some(Error::Io(error)),
        }
        if !listening {
            if accepts(local_port) {
                listening = true;
                let _ = events.try_send(ForwardEvent::Listening { local_port });
            } else if Instant::now() >= deadline {
                break Some(Error::ForwardTimeout);
            }
        }
        thread::sleep(if listening {
            LISTENING_POLL
        } else {
            STARTING_POLL
        });
    };
    let child = lock(slot).child.take();
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
    if let Some(error) = ended {
        let _ = events.try_send(ForwardEvent::Ended(error));
    }
    Ok(())
}

/// `preferred` when nothing holds it on loopback, otherwise a port the system
/// picks. Another process may still take it before SSH does; the readiness
/// probe then times out rather than reporting someone else's listener, since
/// SSH keeps running after a failed bind (see `command`).
#[cfg(unix)]
fn free_local_port(preferred: u16) -> Result<u16> {
    if TcpListener::bind((Ipv4Addr::LOCALHOST, preferred)).is_ok() {
        return Ok(preferred);
    }
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(Error::ForwardLocalPort)
}

/// Whether something accepts connections on the local port. Connecting, unlike
/// binding, cannot race SSH's own bind. SSH opens the remote side for this
/// connection, and the remote server sees it close at once.
#[cfg(unix)]
fn accepts(local_port: u16) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, local_port)),
        PROBE_TIMEOUT,
    )
    .is_ok()
}

/// The forward's own connection policy: noninteractive like the bridge's, but
/// never through a `ControlPath` master, which would take over the forward and
/// keep it after this child is killed. `ExitOnForwardFailure` stays off so a
/// `LocalForward` or `RemoteForward` from the user's config that is already in
/// use elsewhere cannot end this forward; readiness is probed instead.
#[cfg(unix)]
fn command(target: &str, local_port: u16, remote_port: u16) -> Command {
    let mut command = Command::new("ssh");
    command.args([
        "-N",
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "NumberOfPasswordPrompts=0",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ConnectionAttempts=1",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=4",
        "-o",
        "ForwardX11=no",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPath=none",
        "-o",
        "ExitOnForwardFailure=no",
        "-L",
    ]);
    command
        .arg(format!("127.0.0.1:{local_port}:localhost:{remote_port}"))
        .args(["--", target]);
    command
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::{ffi::OsStr, path::PathBuf};

    const WAIT: Duration = Duration::from_secs(10);

    fn port(port: u16) -> NonZeroU16 {
        NonZeroU16::new(port).unwrap()
    }

    /// A port nothing else in these tests prefers, so one test's stand-in
    /// listener cannot answer another's readiness probe.
    fn unused_port() -> NonZeroU16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        port(listener.local_addr().unwrap().port())
    }

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    fn next_event(forward: &PortForward) -> ForwardEvent {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(event) = forward.try_event() {
                return event;
            }
            assert!(Instant::now() < deadline, "no event from the forward");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("herdr-forward-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    /// The pid the fake SSH wrote, once it has.
    fn child_pid(file: &std::path::Path) -> i32 {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(pid) = std::fs::read_to_string(file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "fake ssh never started");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn running(pid: i32) -> bool {
        Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    fn wait_gone(pid: i32) {
        let deadline = Instant::now() + WAIT;
        while running(pid) {
            assert!(Instant::now() < deadline, "the forward's child outlived it");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn privileged_ports_move_up_and_others_keep_their_number() {
        assert_eq!(preferred_local_port(port(80)), 10080);
        assert_eq!(preferred_local_port(port(1023)), 11023);
        assert_eq!(preferred_local_port(port(1024)), 1024);
        assert_eq!(preferred_local_port(port(3000)), 3000);
        assert_eq!(preferred_local_port(port(u16::MAX)), u16::MAX);
    }

    #[test]
    fn a_held_local_port_falls_back_to_one_the_system_picks() {
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let taken = held.local_addr().unwrap().port();
        let chosen = free_local_port(taken).unwrap();
        assert_ne!(chosen, taken);
        drop(held);
        let free = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert_eq!(free_local_port(free).unwrap(), free);
    }

    #[test]
    fn the_command_forwards_loopback_only_and_never_through_a_master() {
        let command = command("penso@box", 13000, 3000);
        assert_eq!(command.get_program(), "ssh");
        let args: Vec<&OsStr> = command.get_args().collect();
        let position = |arg: &str| args.iter().position(|a| *a == arg).unwrap();
        assert_eq!(args[0], "-N");
        assert_eq!(
            args[position("-L") + 1],
            "127.0.0.1:13000:localhost:3000",
            "binds loopback only, never a wildcard address"
        );
        for option in [
            "BatchMode=yes",
            "StrictHostKeyChecking=yes",
            "ControlMaster=no",
            "ControlPath=none",
            "ExitOnForwardFailure=no",
        ] {
            assert!(args.contains(&OsStr::new(option)), "{option}");
        }
        // It would clear the forward itself along with the config's.
        assert!(!args.contains(&OsStr::new("ClearAllForwardings=yes")));
        // The target follows `--`, so it can never be read as an option.
        assert_eq!(&args[args.len() - 2..], ["--", "penso@box"]);
    }

    #[test]
    fn option_like_targets_are_refused_before_anything_runs() {
        assert!(matches!(
            PortForward::start("-oProxyCommand=x", port(3000)),
            Err(Error::InvalidSshTarget)
        ));
    }

    /// Listening is reported once something accepts on the chosen port, and
    /// stopping kills the child, whose worker then reaps it.
    #[test]
    fn a_listening_forward_reports_its_port_and_stop_kills_the_child() {
        let dir = scratch("listen");
        let pid_file = dir.join("pid");
        let listener = Arc::new(Mutex::new(None));
        let holder = listener.clone();
        let script = format!("echo $$ > '{}'; exec sleep 30", pid_file.display());
        let forward = PortForward::start_with(unused_port(), WAIT, move |local_port| {
            // Stands in for SSH's own listener on the chosen port.
            *holder.lock().unwrap() =
                Some(TcpListener::bind((Ipv4Addr::LOCALHOST, local_port)).unwrap());
            shell(&script)
        })
        .unwrap();
        let ForwardEvent::Listening { local_port } = next_event(&forward) else {
            panic!("expected Listening");
        };
        let bound = listener
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .local_addr()
            .unwrap();
        assert_eq!(bound.port(), local_port);
        let pid = child_pid(&pid_file);
        assert!(running(pid));
        drop(forward);
        wait_gone(pid);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_child_that_exits_ends_the_forward_without_a_retry() {
        let dir = scratch("exit");
        let count = dir.join("count");
        let script = format!("echo run >> '{}'; exit 255", count.display());
        let forward =
            PortForward::start_with(unused_port(), WAIT, move |_| shell(&script)).unwrap();
        let ForwardEvent::Ended(Error::ForwardExit(status)) = next_event(&forward) else {
            panic!("expected ForwardExit");
        };
        assert_eq!(status.code(), Some(255));
        thread::sleep(Duration::from_millis(200));
        assert!(forward.try_event().is_none());
        assert_eq!(std::fs::read_to_string(&count).unwrap(), "run\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_forward_that_never_listens_times_out_and_kills_the_child() {
        let dir = scratch("timeout");
        let pid_file = dir.join("pid");
        let script = format!("echo $$ > '{}'; exec sleep 30", pid_file.display());
        let forward =
            PortForward::start_with(unused_port(), Duration::from_millis(300), move |_| {
                shell(&script)
            })
            .unwrap();
        assert!(matches!(
            next_event(&forward),
            ForwardEvent::Ended(Error::ForwardTimeout)
        ));
        wait_gone(child_pid(&pid_file));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_child_that_cannot_start_ends_the_forward_with_its_cause() {
        let forward = PortForward::start_with(unused_port(), WAIT, |_| {
            Command::new("/nonexistent/herdr-test-ssh")
        })
        .unwrap();
        let ForwardEvent::Ended(error) = next_event(&forward) else {
            panic!("expected Ended");
        };
        assert!(matches!(error, Error::ForwardSpawn(_)));
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(std::error::Error::source(&error).is_some());
    }
}
