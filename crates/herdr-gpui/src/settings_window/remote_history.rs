//! Herdr's pane history switch on each connected SSH host. Every host's
//! daemon reads its own config, so each switch reads and writes that host's
//! file over SSH on a background worker, independently of local saves.

use super::SettingsWindow;
use crate::herdr_settings::RemotePaneHistory;
use gpui::{AsyncApp, Context, WeakEntity};
use herdr_client::{ConnectTarget, ScriptHost};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(super) enum HostState {
    Loading,
    Ready(RemotePaneHistory),
    /// Holds the switch at its last read value until the write reconciles.
    Saving(RemotePaneHistory),
    /// Display text with the causes, such as the SSH failure, prepared once.
    Failed(String),
}

pub(super) struct Host {
    pub target: String,
    pub label: String,
    pub state: HostState,
    generation: u64,
    cancel: Arc<AtomicBool>,
}

impl Drop for Host {
    /// A host that disconnects, is reloaded, or leaves with the window stops
    /// its SSH command. A write is a single rename, so it lands whole or not.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

/// Work for a background worker, fenced by `generation` so a late result
/// cannot overwrite a newer read of the same host.
#[derive(Debug)]
pub(super) struct Job {
    pub target: String,
    pub generation: u64,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub(super) struct RemoteHistory {
    hosts: Vec<Host>,
    generation: u64,
}

impl RemoteHistory {
    pub fn hosts(&self) -> &[Host] {
        &self.hosts
    }

    fn host(&mut self, target: String, label: String) -> (Host, Job) {
        self.generation = self.generation.wrapping_add(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let job = Job {
            target: target.clone(),
            generation: self.generation,
            cancel: cancel.clone(),
        };
        let host = Host {
            target,
            label,
            state: HostState::Loading,
            generation: self.generation,
            cancel,
        };
        (host, job)
    }

    /// Follows `connected` (label, target) pairs in order, one entry per
    /// target. New hosts start loading; with `reload`, so do all others
    /// except one mid-save, whose own completion re-reads it.
    pub fn sync(
        &mut self,
        connected: impl IntoIterator<Item = (String, String)>,
        reload: bool,
    ) -> Vec<Job> {
        let mut previous = std::mem::take(&mut self.hosts);
        let mut jobs = Vec::new();
        for (label, target) in connected {
            if self.hosts.iter().any(|host| host.target == target) {
                continue;
            }
            let kept = previous
                .iter()
                .position(|host| host.target == target)
                .map(|index| previous.swap_remove(index))
                .filter(|host| !reload || matches!(host.state, HostState::Saving(_)));
            let host = match kept {
                Some(mut host) => {
                    host.label = label;
                    host
                }
                None => {
                    let (host, job) = self.host(target, label);
                    jobs.push(job);
                    host
                }
            };
            self.hosts.push(host);
        }
        jobs
    }

    /// Applies a finished read or save. `false` when the host has since gone
    /// or been read again.
    pub fn finish(
        &mut self,
        target: &str,
        generation: u64,
        result: crate::Result<RemotePaneHistory>,
    ) -> bool {
        let Some(host) = self
            .hosts
            .iter_mut()
            .find(|host| host.target == target && host.generation == generation)
        else {
            return false;
        };
        host.state = match result {
            Ok(history) => HostState::Ready(history),
            Err(error) => HostState::Failed(describe(&error)),
        };
        true
    }

    /// Starts a write from a settled read. A host still loading, saving, or
    /// failed has nothing current to compare against.
    pub fn begin_save(&mut self, target: &str) -> Option<(RemotePaneHistory, Job)> {
        let index = self.hosts.iter().position(|host| host.target == target)?;
        let HostState::Ready(history) = &self.hosts[index].state else {
            return None;
        };
        let history = history.clone();
        let label = self.hosts[index].label.clone();
        let (mut host, job) = self.host(target.to_owned(), label);
        host.state = HostState::Saving(history.clone());
        self.hosts[index] = host;
        Some((history, job))
    }
}

impl SettingsWindow {
    /// Connected SSH endpoints as (label, target), one per host: sessions on
    /// one host share its config.
    fn connected_ssh_hosts(&self, cx: &Context<Self>) -> Vec<(String, String)> {
        let Some(source) = self.source.upgrade() else {
            return Vec::new();
        };
        let mut hosts: Vec<(String, String)> = Vec::new();
        for endpoint in &source.read(cx).endpoints {
            if let ConnectTarget::Ssh { target, .. } = &endpoint.connection.target
                && endpoint.live.status.is_connected()
                && !hosts.iter().any(|(_, known)| known == target)
            {
                hosts.push((endpoint.label.clone(), target.clone()));
            }
        }
        hosts
    }

    /// Cheap when nothing changed: the source notifies on every update, and
    /// this repaints Settings only when its host list does.
    pub(super) fn sync_remote_history(&mut self, reload: bool, cx: &mut Context<Self>) {
        let hosts = self.connected_ssh_hosts(cx);
        let unchanged = !reload
            && hosts.len() == self.remote_history.hosts.len()
            && hosts
                .iter()
                .zip(&self.remote_history.hosts)
                .all(|((label, target), host)| *label == host.label && *target == host.target);
        if unchanged {
            return;
        }
        for job in self.remote_history.sync(hosts, reload) {
            let task = cx.background_executor().spawn(async move {
                let result = RemotePaneHistory::load(ScriptHost::Ssh(&job.target), &job.cancel);
                (job, result)
            });
            cx.spawn(async move |this, cx| {
                let (job, result) = task.await;
                let _ = this.update(cx, |this, cx| {
                    if this
                        .remote_history
                        .finish(&job.target, job.generation, result)
                    {
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    pub(super) fn save_remote_history(
        &mut self,
        target: &str,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((history, job)) = self.remote_history.begin_save(target) else {
            return;
        };
        let task = cx.background_executor().spawn(async move {
            let result = history.save(ScriptHost::Ssh(&job.target), enabled, &job.cancel);
            (job, result)
        });
        let source = self.source.clone();
        cx.spawn(async move |this, cx| {
            let (job, result) = task.await;
            // The daemon learns of the edit even when Settings has closed.
            let status = match &result {
                Ok(_) => queue_reload(&source, &job.target, cx),
                Err(_) => None,
            };
            let _ = this.update(cx, |this, cx| {
                if this
                    .remote_history
                    .finish(&job.target, job.generation, result)
                {
                    if let Some(status) = status
                        && !this.saving
                    {
                        this.status = Some(status);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
}

/// The error and its causes. A wrapper that displays its source verbatim
/// would repeat it, so a cause equal to the text before it is skipped.
fn describe(error: &(dyn std::error::Error + 'static)) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in std::iter::successors(Some(error), |cause| cause.source()) {
        let text = cause.to_string();
        if parts.last() != Some(&text) && !text.is_empty() {
            parts.push(text);
        }
    }
    parts.join(": ")
}

/// Asks every connected daemon on `target` to reload its config. Queued is
/// not acknowledged, and the status says so.
fn queue_reload(
    source: &WeakEntity<crate::HerdrWindow>,
    target: &str,
    cx: &mut AsyncApp,
) -> Option<String> {
    source
        .update(cx, |source, _| {
            // Every session on the host reads the same config file.
            let mut status = None;
            for endpoint in &source.endpoints {
                let ConnectTarget::Ssh { target: host, .. } = &endpoint.connection.target else {
                    continue;
                };
                let (Some(handle), Some(snapshot)) =
                    (&endpoint.connection.handle, &endpoint.live.snapshot)
                else {
                    continue;
                };
                if host != target || !endpoint.live.status.is_connected() {
                    continue;
                }
                let queued = handle.request(
                    &snapshot.boot_id,
                    herdr_client::Method::ServerReloadConfig,
                    serde_json::json!({}),
                );
                // A failure outranks any later success in the status line.
                if !matches!(&status, Some(Err(_))) {
                    status = Some(queued.map(|_| ()));
                }
            }
            status.map(|queued| match queued {
                Ok(()) => "Saved; host daemon reload queued (not acknowledged)".to_owned(),
                Err(error) => format!("Saved; host daemon reload not queued: {error}"),
            })
        })
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected(targets: &[&str]) -> Vec<(String, String)> {
        targets
            .iter()
            .map(|target| (format!("label-{target}"), (*target).to_owned()))
            .collect()
    }

    fn history(enabled: bool) -> anyhow::Result<RemotePaneHistory> {
        Ok(RemotePaneHistory::parse_text(Some(format!(
            "[experimental]\npane_history = {enabled}\n"
        )))?)
    }

    #[test]
    fn failures_show_their_ssh_cause_once() {
        let error: crate::Error =
            crate::herdr_settings::Error::Remote(herdr_client::Error::ScriptTimeout).into();
        assert_eq!(
            describe(&error),
            "could not reach the host's Herdr config: host script made no progress before its deadline"
        );
    }

    #[test]
    fn sync_loads_new_hosts_once_and_drops_disconnected_ones() {
        let mut remote = RemoteHistory::default();
        let jobs = remote.sync(connected(&["a", "b", "a"]), false);
        assert_eq!(
            jobs.iter()
                .map(|job| job.target.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(remote.sync(connected(&["a", "b"]), false).is_empty());

        let dropped = jobs[1].cancel.clone();
        assert!(remote.sync(connected(&["a"]), false).is_empty());
        assert!(
            dropped.load(Ordering::Acquire),
            "a disconnected host stops its SSH command"
        );
        assert!(!jobs[0].cancel.load(Ordering::Acquire));
        assert_eq!(remote.hosts().len(), 1);
    }

    #[test]
    fn late_results_cannot_overwrite_a_newer_read() -> anyhow::Result<()> {
        let mut remote = RemoteHistory::default();
        let first = remote.sync(connected(&["a"]), false).remove(0);
        let second = remote.sync(connected(&["a"]), true).remove(0);
        assert!(first.cancel.load(Ordering::Acquire));
        assert!(!remote.finish("a", first.generation, Ok(history(true)?)));
        assert!(matches!(remote.hosts()[0].state, HostState::Loading));
        assert!(remote.finish("a", second.generation, Ok(history(false)?)));
        assert!(matches!(
            &remote.hosts()[0].state,
            HostState::Ready(history) if !history.enabled
        ));
        Ok(())
    }

    #[test]
    fn saves_start_only_from_a_settled_read_and_survive_reload() -> anyhow::Result<()> {
        let mut remote = RemoteHistory::default();
        let load = remote.sync(connected(&["a"]), false).remove(0);
        assert!(remote.begin_save("a").is_none(), "still loading");
        assert!(remote.finish("a", load.generation, Ok(history(false)?)));

        let Some((from, save)) = remote.begin_save("a") else {
            anyhow::bail!("a ready host saves");
        };
        assert!(!from.enabled);
        assert!(remote.begin_save("a").is_none(), "one write at a time");
        // A reload while the write runs leaves it alone.
        assert!(remote.sync(connected(&["a"]), true).is_empty());
        assert!(!save.cancel.load(Ordering::Acquire));
        assert!(!remote.finish("a", load.generation, Ok(history(false)?)));
        assert!(remote.finish(
            "a",
            save.generation,
            Err(crate::herdr_settings::Error::RemoteConflict.into())
        ));
        assert!(matches!(
            &remote.hosts()[0].state,
            HostState::Failed(text) if text == "the host's Herdr config changed; reload before saving"
        ));
        assert!(
            remote.begin_save("a").is_none(),
            "a failed host reloads first"
        );
        assert_eq!(remote.sync(connected(&["a"]), true).len(), 1);
        Ok(())
    }
}
