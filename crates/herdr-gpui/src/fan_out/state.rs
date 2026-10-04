//! A fan-out's lanes and the host work behind them: what was picked, how far
//! each lane got, and what each changed.
//!
//! Host work runs on named threads, never the UI thread, and reports through
//! a channel drained on the window's tick. A launch or removal is cancelled
//! only when the fan-out is dropped with its window; the agent lookup and
//! change reads stop when the dialog closes.

use super::{
    error::Error,
    job::{self, Checkout, Progress, Report, Request},
    plan::{self, DiffStat, Lane, Picks},
};
use crate::teleport::{AgentKind, Host};
use herdr_client::protocol::{AgentStatus, ClientShellSnapshot};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// How often an open comparison rereads every lane's changes.
const REFRESH_EVERY: Duration = Duration::from_secs(10);

/// Where a fan-out runs, captured when the dialog opens.
#[derive(Debug, Clone)]
pub(crate) struct Origin {
    pub(crate) endpoint_id: String,
    pub(crate) endpoint_label: String,
    pub(crate) host: Host,
    /// The main checkout's workspace, which new worktrees are created through.
    pub(crate) workspace_id: String,
    pub(crate) repo_label: String,
    /// The ref lanes branch from: the linked checkout's branch, or `HEAD`.
    pub(crate) base: String,
}

enum Event {
    Installed(Result<Vec<AgentKind>, Error>),
    Report(Report),
    Launched,
    Stats(Result<Vec<Option<DiffStat>>, Error>),
    Removed(Vec<(usize, Result<(), Error>)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LaneState {
    Waiting,
    CreatingWorktree,
    StartingAgent,
    Prompting,
    Running,
    /// Display text of the failure; the typed error was logged.
    Failed(String),
}

impl LaneState {
    pub(super) fn label(&self) -> &str {
        match self {
            Self::Waiting => "Waiting",
            Self::CreatingWorktree => "Creating worktree",
            Self::StartingAgent => "Starting agent",
            Self::Prompting => "Sending prompt",
            Self::Running => "Prompted",
            Self::Failed(error) => error,
        }
    }

    pub(super) fn settled(&self) -> bool {
        matches!(self, Self::Running | Self::Failed(_))
    }
}

pub(super) struct LaneView {
    pub(super) lane: Lane,
    pub(super) state: LaneState,
    pub(super) checkout: Option<Checkout>,
    pub(super) stats: Option<DiffStat>,
}

pub(super) enum Stage {
    /// Agents found on the host, still being looked up, or the lookup's
    /// failure.
    Compose(Option<Result<Vec<AgentKind>, String>>),
    Launching,
    Compare,
    /// Asking before the other lanes are removed.
    Confirm(usize),
    Removing(usize),
}

pub(crate) struct FanOut {
    pub(super) origin: Origin,
    pub(super) stage: Stage,
    pub(super) picks: Picks,
    pub(super) prompt: String,
    pub(super) lanes: Vec<LaneView>,
    /// The commit every lane branched from, once the first one resolved it.
    base: Option<String>,
    pub(super) error: Option<String>,
    sender: mpsc::Sender<Event>,
    events: mpsc::Receiver<Event>,
    /// Cancels the launch or removal only when the window goes away.
    work: Arc<AtomicBool>,
    /// Cancels the agent lookup or a change read when the dialog closes.
    probe: Arc<AtomicBool>,
    probing: bool,
    next_refresh: Option<Instant>,
}

impl Drop for FanOut {
    fn drop(&mut self) {
        self.work.store(true, Ordering::Release);
        self.probe.store(true, Ordering::Release);
    }
}

/// Run `work` on a named background thread.
fn spawn(work: impl FnOnce() + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name("herdr-fan-out".into())
        .spawn(work);
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not start the fan-out worker");
    }
}

impl FanOut {
    /// Opens on the prompt at once, looking up the host's agents meanwhile.
    pub(crate) fn start(origin: Origin) -> Self {
        let (sender, events) = mpsc::channel();
        let mut fan_out = Self {
            origin,
            stage: Stage::Compose(None),
            picks: Picks::default(),
            prompt: String::new(),
            lanes: Vec::new(),
            base: None,
            error: None,
            sender,
            events,
            work: Arc::new(AtomicBool::new(false)),
            probe: Arc::new(AtomicBool::new(false)),
            probing: false,
            next_refresh: None,
        };
        let host = fan_out.origin.host.clone();
        fan_out.probe(move |cancelled| Event::Installed(job::installed(&host, cancelled)));
        fan_out
    }

    /// Whether the user is still composing, so nothing has been created.
    pub(crate) fn composing(&self) -> bool {
        matches!(self.stage, Stage::Compose(_))
    }

    /// Whether host work that must not be abandoned is running.
    pub(crate) fn busy(&self) -> bool {
        matches!(self.stage, Stage::Launching | Stage::Removing(_))
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn base_for_test(&self) -> &str {
        &self.origin.base
    }

    /// Replace the host lookup with `kinds`.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn agents_for_test(&mut self, kinds: Vec<AgentKind>) {
        self.stop_probe();
        self.stage = Stage::Compose(Some(Ok(kinds)));
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn picked_for_test(&self) -> usize {
        self.picks.total()
    }

    fn probe(&mut self, work: impl FnOnce(&AtomicBool) -> Event + Send + 'static) {
        self.probe.store(true, Ordering::Release);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.probe = cancelled.clone();
        self.probing = true;
        let sender = self.sender.clone();
        spawn(move || {
            let _ = sender.send(work(&cancelled));
        });
    }

    /// Stop the agent lookup or change read, as when the dialog closes.
    pub(super) fn stop_probe(&mut self) {
        self.probe.store(true, Ordering::Release);
        self.probing = false;
        self.next_refresh = None;
    }

    pub(super) fn toggle(&mut self, kind: AgentKind, add: bool) -> bool {
        if !self.composing() {
            return false;
        }
        if add {
            self.picks.add(kind)
        } else {
            self.picks.remove(kind)
        }
    }

    /// Why the launch cannot start yet, if it cannot.
    pub(super) fn not_ready(&self, prompt: &str) -> Option<&'static str> {
        match &self.stage {
            Stage::Compose(Some(Ok(_))) => {}
            _ => return Some("Waiting for the agent list"),
        }
        if prompt.trim().is_empty() {
            return Some("Write a prompt");
        }
        if self.picks.total() == 0 {
            return Some("Pick at least one agent");
        }
        None
    }

    /// Start every lane. `seed` names the branches; see [`plan::lanes`].
    pub(super) fn launch(&mut self, prompt: &str, seed: u64) -> bool {
        if self.not_ready(prompt).is_some() {
            return false;
        }
        self.stop_probe();
        let lanes = plan::lanes(&self.picks, seed);
        self.prompt = prompt.trim().to_owned();
        self.lanes = lanes
            .iter()
            .map(|lane| LaneView {
                lane: lane.clone(),
                state: LaneState::Waiting,
                checkout: None,
                stats: None,
            })
            .collect();
        let request = Request {
            host: self.origin.host.clone(),
            workspace_id: self.origin.workspace_id.clone(),
            base: self.origin.base.clone(),
            prompt: self.prompt.clone(),
            lanes,
        };
        let (sender, cancelled) = (self.sender.clone(), self.work.clone());
        spawn(move || {
            let report = |report| {
                let _ = sender.send(Event::Report(report));
            };
            job::launch(&request, &report, &cancelled);
            let _ = sender.send(Event::Launched);
        });
        self.stage = Stage::Launching;
        true
    }

    fn checkouts(&self) -> Vec<String> {
        self.lanes
            .iter()
            .map(|lane| {
                lane.checkout
                    .as_ref()
                    .map_or_else(String::new, |c| c.path.clone())
            })
            .collect()
    }

    /// Reread every lane's changes when due and nothing else is reading.
    pub(super) fn refresh(&mut self, now: Instant) -> bool {
        let Some(base) = self.base.clone() else {
            return false;
        };
        if self.probing
            || !matches!(self.stage, Stage::Compare | Stage::Confirm(_))
            || self.next_refresh.is_some_and(|next| now < next)
        {
            return false;
        }
        self.next_refresh = Some(now + REFRESH_EVERY);
        let (host, checkouts) = (self.origin.host.clone(), self.checkouts());
        self.probe(move |cancelled| Event::Stats(job::stats(&host, &checkouts, &base, cancelled)));
        true
    }

    /// Remove every lane but `winner` that has a checkout.
    pub(super) fn keep(&mut self, winner: usize) -> bool {
        let Stage::Confirm(confirmed) = self.stage else {
            return false;
        };
        if confirmed != winner {
            return false;
        }
        self.stop_probe();
        let doomed = self.doomed(winner);
        let (host, sender, cancelled) = (
            self.origin.host.clone(),
            self.sender.clone(),
            self.work.clone(),
        );
        spawn(move || {
            let removed = doomed
                .into_iter()
                .map(|(index, workspace)| (index, job::remove(&host, &workspace, &cancelled)))
                .collect();
            let _ = sender.send(Event::Removed(removed));
        });
        self.error = None;
        self.stage = Stage::Removing(winner);
        true
    }

    /// The lanes besides `winner` that have a checkout to remove, with
    /// their workspaces.
    pub(super) fn doomed(&self, winner: usize) -> Vec<(usize, String)> {
        self.lanes
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != winner)
            .filter_map(|(index, lane)| Some((index, lane.checkout.as_ref()?.workspace_id.clone())))
            .collect()
    }

    /// Apply finished work. Returns the kept lane's workspace once every
    /// other lane is gone.
    pub(super) fn poll(&mut self) -> (bool, Option<String>) {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            changed = true;
            match event {
                Event::Installed(result) => {
                    self.probing = false;
                    if let Stage::Compose(installed) = &mut self.stage {
                        *installed = Some(result.map_err(|error| {
                            tracing::warn!(%error, "fan-out agent lookup");
                            error.to_string()
                        }));
                    }
                }
                Event::Report(Report::Base(commit)) => self.base = Some(commit),
                Event::Report(Report::Lane(index, progress)) => {
                    let Some(lane) = self.lanes.get_mut(index) else {
                        continue;
                    };
                    lane.state = match progress {
                        Progress::CreatingWorktree => LaneState::CreatingWorktree,
                        Progress::Created(checkout) => {
                            lane.checkout = Some(checkout);
                            continue;
                        }
                        Progress::StartingAgent => LaneState::StartingAgent,
                        Progress::Prompting => LaneState::Prompting,
                        Progress::Running => LaneState::Running,
                        Progress::Failed(error) => {
                            tracing::warn!(%error, branch = %lane.lane.branch, "fan-out lane");
                            LaneState::Failed(error.to_string())
                        }
                    };
                }
                Event::Launched => {
                    for lane in &mut self.lanes {
                        if !lane.state.settled() {
                            lane.state = LaneState::Failed("Stopped".to_owned());
                        }
                    }
                    self.stage = Stage::Compare;
                    self.next_refresh = None;
                }
                Event::Stats(result) => {
                    self.probing = false;
                    match result {
                        Ok(stats) => {
                            for (lane, stats) in self.lanes.iter_mut().zip(stats) {
                                lane.stats = stats;
                            }
                        }
                        Err(Error::Cancelled) => {}
                        Err(error) => tracing::warn!(%error, "fan-out change read"),
                    }
                }
                Event::Removed(results) => {
                    let Stage::Removing(winner) = self.stage else {
                        continue;
                    };
                    let mut failures = Vec::new();
                    let mut removed = Vec::new();
                    for (index, result) in results {
                        match result {
                            Ok(()) => removed.push(index),
                            Err(error) => {
                                tracing::warn!(%error, "fan-out removal");
                                failures.push(error.to_string());
                            }
                        }
                    }
                    if failures.is_empty() {
                        let workspace = self.lanes.get(winner).and_then(|lane| {
                            lane.checkout.as_ref().map(|c| c.workspace_id.clone())
                        });
                        return (true, workspace);
                    }
                    let mut index = 0;
                    let mut kept = winner;
                    self.lanes.retain(|_| {
                        let keep = !removed.contains(&index);
                        if !keep && index < winner {
                            kept -= 1;
                        }
                        index += 1;
                        keep
                    });
                    self.error = Some(format!(
                        "{} worktree(s) could not be removed: {}",
                        failures.len(),
                        failures[0]
                    ));
                    self.stage = Stage::Confirm(kept);
                    self.next_refresh = None;
                }
            }
        }
        (changed, None)
    }
}

fn status_text(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "Idle",
        AgentStatus::Working => "Working",
        AgentStatus::Blocked => "Needs input",
        AgentStatus::Done => "Done",
        AgentStatus::Unknown => "Unknown",
    }
}

/// What a compared lane's agent is doing, from the daemon's own snapshot.
pub(super) fn agent_status(
    snapshot: Option<&ClientShellSnapshot>,
    workspace_id: &str,
) -> &'static str {
    let Some(snapshot) = snapshot else {
        return "Host offline";
    };
    snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == workspace_id)
        .map_or("Closed", |workspace| status_text(workspace.agent_status))
}

/// Branch names are seeded from the clock, as new worktree names are.
pub(super) fn seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_micros().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
