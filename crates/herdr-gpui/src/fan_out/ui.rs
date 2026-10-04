//! The fan-out dialog: write a prompt and pick agents, follow the launch,
//! then compare the lanes and keep one.
//!
//! Host work runs on named threads, never the UI thread, and reports through
//! a channel the window drains on its tick. Closing the dialog stops the
//! agent lookup and change reads, but a launch or removal keeps going: stopping
//! halfway would strand checkouts. A launched fan-out stays on the window so
//! its comparison can be reopened from the workspace menu.

use super::{
    error::Error,
    job::{self, Checkout, Progress, Report, Request},
    plan::{self, DiffStat, Lane, MAX_LANES, Picks},
};
use crate::{
    HerdrWindow,
    icons::AgentIcon,
    menu::Page,
    progress::{self, Progress as Bar},
    teleport::{AgentKind, Follow, Host},
    window::Flash,
};
use gpui::{prelude::*, *};
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
enum LaneState {
    Waiting,
    CreatingWorktree,
    StartingAgent,
    Prompting,
    Running,
    /// Display text of the failure; the typed error was logged.
    Failed(String),
}

impl LaneState {
    fn label(&self) -> &str {
        match self {
            Self::Waiting => "Waiting",
            Self::CreatingWorktree => "Creating worktree",
            Self::StartingAgent => "Starting agent",
            Self::Prompting => "Sending prompt",
            Self::Running => "Prompted",
            Self::Failed(error) => error,
        }
    }

    fn settled(&self) -> bool {
        matches!(self, Self::Running | Self::Failed(_))
    }
}

struct LaneView {
    lane: Lane,
    state: LaneState,
    checkout: Option<Checkout>,
    stats: Option<DiffStat>,
}

enum Stage {
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
    origin: Origin,
    stage: Stage,
    picks: Picks,
    prompt: String,
    lanes: Vec<LaneView>,
    /// The commit every lane branched from, once the first one resolved it.
    base: Option<String>,
    error: Option<String>,
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
    fn stop_probe(&mut self) {
        self.probe.store(true, Ordering::Release);
        self.probing = false;
        self.next_refresh = None;
    }

    fn toggle(&mut self, kind: AgentKind, add: bool) -> bool {
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
    fn not_ready(&self, prompt: &str) -> Option<&'static str> {
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
    fn launch(&mut self, prompt: &str, seed: u64) -> bool {
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
    fn refresh(&mut self, now: Instant) -> bool {
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
    fn keep(&mut self, winner: usize) -> bool {
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
    fn doomed(&self, winner: usize) -> Vec<(usize, String)> {
        self.lanes
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != winner)
            .filter_map(|(index, lane)| Some((index, lane.checkout.as_ref()?.workspace_id.clone())))
            .collect()
    }

    /// Apply finished work. Returns the kept lane's workspace once every
    /// other lane is gone.
    fn poll(&mut self) -> (bool, Option<String>) {
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
fn agent_status(snapshot: Option<&ClientShellSnapshot>, workspace_id: &str) -> &'static str {
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
fn seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_micros().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

impl HerdrWindow {
    /// The snapshot of the host a fan-out runs on, if it is still connected.
    fn fan_out_snapshot(&self, endpoint_id: &str) -> Option<&ClientShellSnapshot> {
        let (index, endpoint) = self
            .endpoints
            .iter()
            .enumerate()
            .find(|(_, endpoint)| endpoint.id == endpoint_id)?;
        let live = if index == self.selected_endpoint {
            &self.live
        } else {
            &endpoint.live
        };
        live.snapshot
            .as_deref()
            .filter(|_| live.status.is_connected())
    }

    pub(crate) fn poll_fan_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.menu.page == Some(Page::FanOut);
        let Some(fan_out) = &mut self.fan_out else {
            return;
        };
        if !open {
            // Nothing was created yet, so there is nothing to come back to.
            if fan_out.composing() {
                self.fan_out = None;
                return;
            }
            fan_out.stop_probe();
        }
        let was_launching = matches!(fan_out.stage, Stage::Launching);
        let (mut changed, kept) = fan_out.poll();
        if open {
            changed |= fan_out.refresh(Instant::now());
        }
        if was_launching && !open && matches!(fan_out.stage, Stage::Compare) {
            let running = fan_out
                .lanes
                .iter()
                .filter(|lane| lane.state == LaneState::Running)
                .count();
            let total = fan_out.lanes.len();
            let text = format!(
                "Fan-out: {running} of {total} agent(s) prompted. Compare them from the workspace menu."
            );
            let flash = if running == total {
                Flash::success(text)
            } else {
                Flash::warning(text)
            };
            self.show_flash(flash, cx);
            return cx.notify();
        }
        if let Some(workspace) = kept {
            let endpoint = fan_out.origin.endpoint_id.clone();
            self.fan_out = None;
            if open {
                self.dismiss_menu(window, cx);
            }
            self.show_flash(Flash::success("Kept one lane; removed the others"), cx);
            self.teleport_follow = Some(Follow::new(endpoint, workspace));
            return;
        }
        if changed {
            cx.notify();
        }
    }

    pub(crate) fn fan_out_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.prevent_default();
        match event.keystroke.key.as_str() {
            "escape" => self.fan_out_back(window, cx),
            "enter" => self.submit_fan_out(cx),
            _ => {}
        }
    }

    /// Escape steps back out of a confirmation before it closes the dialog.
    fn fan_out_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(fan_out) = &mut self.fan_out
            && let Stage::Confirm(_) = fan_out.stage
        {
            fan_out.stage = Stage::Compare;
            fan_out.error = None;
            cx.notify();
            return;
        }
        self.dismiss_menu(window, cx);
    }

    fn submit_fan_out(&mut self, cx: &mut Context<Self>) {
        let prompt = self
            .menu
            .input
            .as_ref()
            .map(|input| input.text.clone())
            .unwrap_or_default();
        let Some(fan_out) = &mut self.fan_out else {
            return;
        };
        match fan_out.stage {
            Stage::Compose(_) => {
                if fan_out.launch(&prompt, seed()) {
                    // The prompt is sent; the comparison takes the keyboard.
                    self.menu.input = None;
                }
            }
            Stage::Confirm(winner) => {
                fan_out.keep(winner);
            }
            _ => return,
        }
        cx.notify();
    }

    fn open_fan_out_lane(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(fan_out) = &self.fan_out else {
            return;
        };
        let Some(checkout) = fan_out.lanes.get(index).and_then(|l| l.checkout.as_ref()) else {
            return;
        };
        let follow = Follow::new(
            fan_out.origin.endpoint_id.clone(),
            checkout.workspace_id.clone(),
        );
        self.dismiss_menu(window, cx);
        self.teleport_follow = Some(follow);
    }

    pub(crate) fn render_fan_out(&self, cx: &mut Context<Self>) -> Div {
        let Some(fan_out) = &self.fan_out else {
            return div();
        };
        let theme = &self.theme;
        let font = &self.config.ui;
        let muted = rgb(theme.muted);
        let danger = crate::menu::danger(theme);
        let line = |text: String| div().min_w_0().child(text);
        let small = |id: &'static str, index: usize| {
            div()
                .id((id, index))
                .debug_selector(move || format!("{id}-{index}"))
                .px(px(8.))
                .py(px(2.))
                .rounded(px(crate::config::corners::CONTROL))
                .border_1()
                .border_color(rgb(theme.active))
                .cursor_pointer()
                .hover(|button| button.bg(rgb(theme.active)))
        };
        let prompt = self
            .menu
            .input
            .as_ref()
            .map(|input| input.text.as_str())
            .unwrap_or_default();
        let mut body = div().flex().flex_col().gap(px(8.)).px(px(16.)).py(px(12.));
        let (primary, armed) = match &fan_out.stage {
            Stage::Compose(installed) => {
                body = body.child(
                    line(format!(
                        "Each agent gets its own new worktree from {}, then the same prompt.",
                        fan_out.origin.base
                    ))
                    .text_color(muted),
                );
                if self.menu.input.is_some() {
                    body = body.child(self.render_dialog_input(cx));
                }
                match installed {
                    None => {
                        body = body
                            .child(progress::bar(
                                "fan-out-progress",
                                Bar::Busy,
                                crate::menu::accent(theme).into(),
                                rgb(theme.active).into(),
                            ))
                            .child(
                                line("Looking for installed agents...".into()).text_color(muted),
                            );
                    }
                    Some(Err(error)) => {
                        body = body.child(
                            line(error.clone())
                                .debug_selector(|| "fan-out-error".into())
                                .text_color(danger),
                        );
                    }
                    Some(Ok(kinds)) if kinds.is_empty() => {
                        body = body.child(
                            line(format!(
                                "No supported agent is installed on {}.",
                                fan_out.origin.endpoint_label
                            ))
                            .text_color(danger),
                        );
                    }
                    Some(Ok(kinds)) => {
                        body = body.child(line(format!(
                            "Agents · {} of {MAX_LANES}",
                            fan_out.picks.total()
                        )));
                        for (index, kind) in kinds.iter().copied().enumerate() {
                            let count = fan_out.picks.count(kind);
                            body = body.child(
                                div()
                                    .id(("fan-out-agent", index))
                                    .debug_selector(move || format!("fan-out-agent-{index}"))
                                    .px(px(10.))
                                    .py(px(4.))
                                    .rounded(px(crate::config::corners::CONTROL))
                                    .when(count > 0, |row| row.bg(rgb(theme.active)))
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .child(
                                        svg()
                                            .path(
                                                AgentIcon::from_identity(Some(kind.name())).path(),
                                            )
                                            .size(px(14.))
                                            .flex_none()
                                            .text_color(rgb(theme.foreground)),
                                    )
                                    .child(div().flex_1().min_w_0().truncate().child(kind.name()))
                                    .when(count > 0, |row| {
                                        row.child(small("fan-out-less", index).child("−").on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                if let Some(fan_out) = &mut this.fan_out
                                                    && fan_out.toggle(kind, false)
                                                {
                                                    cx.notify();
                                                }
                                            }),
                                        ))
                                        .child(
                                            div()
                                                .debug_selector(move || {
                                                    format!("fan-out-count-{index}")
                                                })
                                                .min_w(px(16.))
                                                .flex()
                                                .justify_center()
                                                .child(count.to_string()),
                                        )
                                    })
                                    .child(small("fan-out-more", index).child("+").on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            if let Some(fan_out) = &mut this.fan_out
                                                && fan_out.toggle(kind, true)
                                            {
                                                cx.notify();
                                            }
                                        }),
                                    )),
                            );
                        }
                    }
                }
                let lanes = fan_out.picks.total();
                let label = match lanes {
                    0 | 1 => "Start agent".to_owned(),
                    lanes => format!("Start {lanes} agents"),
                };
                (label, fan_out.not_ready(prompt).is_none())
            }
            Stage::Launching | Stage::Compare | Stage::Confirm(_) | Stage::Removing(_) => {
                let snapshot = self.fan_out_snapshot(&fan_out.origin.endpoint_id);
                let comparing = !matches!(fan_out.stage, Stage::Launching);
                body = body.child(
                    line(format!("“{}”", fan_out.prompt))
                        .debug_selector(|| "fan-out-prompt".into())
                        .text_color(muted),
                );
                if !comparing {
                    let settled = fan_out.lanes.iter().filter(|l| l.state.settled()).count();
                    body = body.child(progress::bar(
                        "fan-out-progress",
                        Bar::Working(settled as f32 / fan_out.lanes.len().max(1) as f32),
                        crate::menu::accent(theme).into(),
                        rgb(theme.active).into(),
                    ));
                }
                for (index, lane) in fan_out.lanes.iter().enumerate() {
                    let failed = matches!(lane.state, LaneState::Failed(_));
                    let status = match (&lane.state, &lane.checkout) {
                        (LaneState::Running, Some(checkout)) if comparing => {
                            agent_status(snapshot, &checkout.workspace_id).to_owned()
                        }
                        (state, _) => state.label().to_owned(),
                    };
                    let changes = match (&lane.checkout, lane.stats) {
                        (None, _) => None,
                        (Some(_), Some(stats)) => Some(stats.summary()),
                        (Some(_), None) if comparing => Some("Reading changes...".to_owned()),
                        (Some(_), None) => None,
                    };
                    let winner = matches!(
                        fan_out.stage,
                        Stage::Confirm(kept) | Stage::Removing(kept) if kept == index
                    );
                    let actionable =
                        lane.checkout.is_some() && matches!(fan_out.stage, Stage::Compare);
                    body = body.child(
                        div()
                            .id(("fan-out-lane", index))
                            .debug_selector(move || format!("fan-out-lane-{index}"))
                            .px(px(10.))
                            .py(px(6.))
                            .rounded(px(crate::config::corners::CONTROL))
                            .when(winner, |row| row.bg(rgb(theme.active)))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                svg()
                                    .path(
                                        AgentIcon::from_identity(Some(lane.lane.kind.name()))
                                            .path(),
                                    )
                                    .size(px(16.))
                                    .flex_none()
                                    .text_color(rgb(theme.foreground)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .truncate()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(lane.lane.branch.clone()),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .debug_selector(move || {
                                                format!("fan-out-status-{index}")
                                            })
                                            .text_color(if failed { danger } else { muted })
                                            .child(status),
                                    )
                                    .when_some(changes, |column, changes| {
                                        column.child(
                                            div()
                                                .truncate()
                                                .debug_selector(move || {
                                                    format!("fan-out-changes-{index}")
                                                })
                                                .child(changes),
                                        )
                                    }),
                            )
                            .when(actionable, |row| {
                                row.child(small("fan-out-open", index).child("Open").on_click(
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_fan_out_lane(index, window, cx);
                                    }),
                                ))
                                .child(
                                    small("fan-out-keep", index).child("Keep").on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            if let Some(fan_out) = &mut this.fan_out {
                                                fan_out.stage = Stage::Confirm(index);
                                                fan_out.error = None;
                                                cx.notify();
                                            }
                                        }),
                                    ),
                                )
                            }),
                    );
                }
                match fan_out.stage {
                    Stage::Launching => {
                        body = body.child(
                            line("Closing this dialog does not stop the launch.".into())
                                .text_color(muted),
                        );
                        ("Starting...".to_owned(), false)
                    }
                    Stage::Compare => {
                        body = body.child(
                            line("Keep the lane you prefer to remove the others.".into())
                                .text_color(muted),
                        );
                        ("Keep...".to_owned(), false)
                    }
                    Stage::Confirm(winner) => {
                        let others = fan_out.doomed(winner).len();
                        body = body.child(
                            line(format!(
                                "Removes the other {others} worktree(s) and their uncommitted changes. Their branches stay, with any commits."
                            ))
                            .debug_selector(|| "fan-out-confirm".into())
                            .text_color(danger),
                        );
                        (format!("Remove {others} other(s)"), true)
                    }
                    _ => {
                        body = body.child(progress::bar(
                            "fan-out-progress",
                            Bar::Busy,
                            crate::menu::accent(theme).into(),
                            rgb(theme.active).into(),
                        ));
                        ("Removing...".to_owned(), false)
                    }
                }
            }
        };
        if let Some(error) = &fan_out.error {
            body = body.child(
                line(error.clone())
                    .debug_selector(|| "fan-out-error".into())
                    .text_color(danger),
            );
        }
        let button = |id: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px(px(12.))
                .py(px(6.))
                .rounded(px(crate::config::corners::CONTROL))
                .border_1()
                .cursor_pointer()
        };
        let dismiss = match fan_out.stage {
            Stage::Compose(_) => "Cancel",
            Stage::Confirm(_) => "Back",
            // Work under way carries on; closing only hides it.
            _ if fan_out.busy() => "Hide",
            _ => "Close",
        };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(16.))
                    .py(px(12.))
                    .border_b_1()
                    .border_color(rgb(theme.active))
                    .child(
                        svg()
                            .path("icons/split.svg")
                            .size(px(16.))
                            .flex_none()
                            .text_color(muted),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(font.size * 1.35))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Fan out prompt"),
                            )
                            .child(div().truncate().text_color(muted).child(format!(
                                "{} on {}",
                                fan_out.origin.repo_label, fan_out.origin.endpoint_label
                            ))),
                    ),
            )
            .child(
                div()
                    .id("fan-out-body")
                    .max_h(px(460.))
                    .overflow_y_scroll()
                    .child(body),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .px(px(16.))
                    .py(px(12.))
                    .border_t_1()
                    .border_color(rgb(theme.active))
                    .child(
                        button("fan-out-cancel")
                            .border_color(rgb(theme.active))
                            .hover(|button| button.bg(rgb(theme.active)))
                            .child(dismiss)
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.fan_out_back(window, cx);
                            })),
                    )
                    .child(
                        button("fan-out-submit")
                            .border_color(if armed {
                                rgb(theme.foreground)
                            } else {
                                rgb(theme.active)
                            })
                            .text_color(if armed { rgb(theme.foreground) } else { muted })
                            .child(primary)
                            .when(!armed, |button| {
                                button.opacity(0.4).cursor(CursorStyle::OperationNotAllowed)
                            })
                            .when(armed, |button| {
                                button.bg(rgb(theme.active)).on_click(cx.listener(
                                    |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.submit_fan_out(cx);
                                    },
                                ))
                            }),
                    ),
            )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::fan_out::error::Step;
    // `gpui::*` also exports a `test` attribute; these are plain unit tests.
    use core::prelude::v1::test;

    fn origin() -> Origin {
        Origin {
            endpoint_id: "local".into(),
            endpoint_label: "This Mac".into(),
            host: crate::teleport::host_for(&herdr_client::ConnectTarget::Local).unwrap(),
            workspace_id: "w1".into(),
            repo_label: "repo".into(),
            base: "HEAD".into(),
        }
    }

    /// A fan-out whose host work is never started: events are fed by hand.
    fn idle() -> FanOut {
        let (sender, events) = mpsc::channel();
        FanOut {
            origin: origin(),
            stage: Stage::Compose(Some(Ok(vec![AgentKind::Claude, AgentKind::Codex]))),
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
        }
    }

    fn checkout(n: usize) -> Checkout {
        Checkout {
            workspace_id: format!("w{n}"),
            path: format!("/tmp/lane-{n}"),
            pane_id: format!("p{n}"),
        }
    }

    /// Three lanes launched, without running the launch.
    fn launched() -> FanOut {
        let mut fan_out = idle();
        fan_out.picks.add(AgentKind::Claude);
        fan_out.picks.add(AgentKind::Codex);
        fan_out.picks.add(AgentKind::Claude);
        fan_out.lanes = plan::lanes(&fan_out.picks, 7)
            .into_iter()
            .map(|lane| LaneView {
                lane,
                state: LaneState::Waiting,
                checkout: None,
                stats: None,
            })
            .collect();
        fan_out.stage = Stage::Launching;
        fan_out
    }

    fn send(fan_out: &FanOut, event: Event) {
        fan_out.sender.send(event).unwrap();
    }

    #[test]
    fn launch_needs_the_agent_list_a_prompt_and_a_pick() {
        let mut fan_out = idle();
        fan_out.stage = Stage::Compose(None);
        assert_eq!(fan_out.not_ready("go"), Some("Waiting for the agent list"));
        fan_out.stage = Stage::Compose(Some(Ok(vec![AgentKind::Claude])));
        assert_eq!(fan_out.not_ready("  "), Some("Write a prompt"));
        assert_eq!(fan_out.not_ready("go"), Some("Pick at least one agent"));
        assert!(fan_out.toggle(AgentKind::Claude, true));
        assert_eq!(fan_out.not_ready("go"), None);
        fan_out.stage = Stage::Compose(Some(Err("no".into())));
        assert!(!fan_out.launch("go", 1), "a failed lookup cannot launch");
        assert!(fan_out.composing());
    }

    #[test]
    fn lane_progress_settles_into_a_comparison() {
        let mut fan_out = launched();
        send(
            &fan_out,
            Event::Report(Report::Lane(0, Progress::CreatingWorktree)),
        );
        send(
            &fan_out,
            Event::Report(Report::Lane(0, Progress::Created(checkout(0)))),
        );
        send(&fan_out, Event::Report(Report::Base("c0ffee".into())));
        send(&fan_out, Event::Report(Report::Lane(0, Progress::Running)));
        send(
            &fan_out,
            Event::Report(Report::Lane(
                1,
                Progress::Failed(Error::Script {
                    step: Step::CreateWorktree,
                    source: herdr_client::Error::ScriptTimeout,
                }),
            )),
        );
        send(
            &fan_out,
            Event::Report(Report::Lane(2, Progress::Created(checkout(2)))),
        );
        send(
            &fan_out,
            Event::Report(Report::Lane(2, Progress::Prompting)),
        );
        // A report for a lane that does not exist is ignored.
        send(&fan_out, Event::Report(Report::Lane(9, Progress::Running)));
        assert_eq!(fan_out.poll(), (true, None));
        assert!(fan_out.busy());
        assert_eq!(fan_out.base.as_deref(), Some("c0ffee"));
        assert_eq!(fan_out.lanes[0].state, LaneState::Running);
        assert_eq!(fan_out.lanes[0].checkout, Some(checkout(0)));
        assert!(
            matches!(&fan_out.lanes[1].state, LaneState::Failed(e) if e.starts_with("creating the worktree failed"))
        );
        assert_eq!(fan_out.lanes[1].checkout, None);

        send(&fan_out, Event::Launched);
        fan_out.poll();
        assert!(matches!(fan_out.stage, Stage::Compare));
        assert_eq!(
            fan_out.lanes[2].state,
            LaneState::Failed("Stopped".into()),
            "a lane the launch never settled is not left looking busy"
        );
        assert!(!fan_out.busy());
    }

    #[test]
    fn stats_land_on_their_lanes_and_reads_do_not_overlap() {
        let mut fan_out = launched();
        fan_out.stage = Stage::Compare;
        let now = Instant::now();
        assert!(
            !fan_out.refresh(now),
            "nothing to compare before the base is known"
        );
        fan_out.base = Some("c0ffee".into());
        fan_out.probing = true;
        assert!(!fan_out.refresh(now), "one read at a time");
        fan_out.probing = false;
        fan_out.next_refresh = Some(now + Duration::from_secs(1));
        assert!(!fan_out.refresh(now), "not before it is due");

        let stat = DiffStat {
            files: 1,
            additions: 2,
            ..DiffStat::default()
        };
        fan_out.probing = true;
        send(&fan_out, Event::Stats(Ok(vec![Some(stat), None, None])));
        fan_out.poll();
        assert!(!fan_out.probing);
        assert_eq!(fan_out.lanes[0].stats, Some(stat));
        assert_eq!(fan_out.lanes[1].stats, None);
    }

    #[test]
    fn keeping_a_lane_needs_its_confirmation_and_removes_only_the_others() {
        let mut fan_out = launched();
        for (index, lane) in fan_out.lanes.iter_mut().enumerate() {
            lane.checkout = (index != 1).then(|| checkout(index));
            lane.state = LaneState::Running;
        }
        fan_out.stage = Stage::Compare;
        assert!(!fan_out.keep(0), "keeping asks first");
        fan_out.stage = Stage::Confirm(2);
        assert!(!fan_out.keep(0), "only the confirmed lane is kept");
        assert_eq!(
            fan_out.doomed(2),
            [(0, "w0".to_owned())],
            "lane 1 never got a checkout, and the winner stays"
        );
        // The removal's answer is fed by hand instead of running it.
        fan_out.stage = Stage::Removing(2);
        assert!(fan_out.busy());
        send(&fan_out, Event::Removed(vec![(0, Ok(()))]));
        assert_eq!(fan_out.poll(), (true, Some("w2".into())));
    }

    #[test]
    fn a_failed_removal_returns_to_the_confirmation_without_removed_lanes() {
        let mut fan_out = launched();
        for (index, lane) in fan_out.lanes.iter_mut().enumerate() {
            lane.checkout = Some(checkout(index));
        }
        fan_out.stage = Stage::Removing(2);
        send(
            &fan_out,
            Event::Removed(vec![
                (0, Ok(())),
                (
                    1,
                    Err(Error::Script {
                        step: Step::Remove,
                        source: herdr_client::Error::ScriptTimeout,
                    }),
                ),
            ]),
        );
        assert_eq!(fan_out.poll(), (true, None));
        assert_eq!(fan_out.lanes.len(), 2);
        assert!(
            matches!(fan_out.stage, Stage::Confirm(1)),
            "the winner moved up"
        );
        assert_eq!(
            fan_out.lanes[1].checkout.as_ref().unwrap().workspace_id,
            "w2"
        );
        assert!(
            fan_out
                .error
                .as_deref()
                .unwrap()
                .starts_with("1 worktree(s) could not be removed")
        );
    }

    #[test]
    fn agent_status_comes_from_the_daemon_snapshot() {
        let mut snapshot = crate::sidebar::layout_tests::snapshot(2);
        let id = snapshot.workspaces[1].workspace_id.clone();
        snapshot.workspaces[1].agent_status = AgentStatus::Blocked;
        assert_eq!(agent_status(Some(&snapshot), &id), "Needs input");
        assert_eq!(agent_status(Some(&snapshot), "gone"), "Closed");
        assert_eq!(agent_status(None, &id), "Host offline");
    }
}
