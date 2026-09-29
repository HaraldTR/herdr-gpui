//! The Teleport dialog: choose a destination, review, and follow the move.
//!
//! Each phase runs on its own named thread, never the UI thread, and reports
//! through a channel the window drains on its tick. Dismissing the dialog
//! cancels discovery and review; a move in progress keeps going and reports
//! its outcome as a flash, because stopping halfway would strand work.

use super::{
    error::{Error, Step},
    job::{
        self, Action, Candidate, Destination, Discovery, HostRepositories, Outcome, Review, Source,
    },
    launch::{Work, command_line},
    marks::{Destination as MarkDestination, Mark},
    provision::Arrival,
    remote::MatchReason,
};
use crate::{HerdrWindow, NavigationTarget, menu::Page, window::Flash};
use gpui::{prelude::*, *};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

enum Event {
    Discovered(Result<Discovery, Error>),
    Reviewed(Result<Review, Error>),
    Step(Step),
    Moved(Result<Outcome, Error>),
}

enum Stage {
    Discovering,
    Choosing(Discovery),
    Reviewing(Candidate),
    Ready(Candidate, Box<Review>),
    Moving(Candidate, Option<Step>),
    Failed(String),
}

/// How long the window keeps steering to a teleported workspace.
const FOLLOW_FOR: Duration = Duration::from_secs(20);
const FOLLOW_RETRY: Duration = Duration::from_millis(750);

/// The workspace a finished teleport should end up focused on.
pub(crate) struct Follow {
    endpoint_id: String,
    workspace_id: String,
    until: Instant,
    next: Option<Instant>,
}

impl Follow {
    pub(crate) fn new(endpoint_id: String, workspace_id: String) -> Self {
        Self {
            endpoint_id,
            workspace_id,
            until: Instant::now() + FOLLOW_FOR,
            next: None,
        }
    }
}

pub(crate) struct Teleport {
    source: Source,
    label: String,
    /// Other connected hosts searched; none means nowhere to go yet.
    hosts: usize,
    stage: Stage,
    selected: usize,
    cancelled: Arc<AtomicBool>,
    events: mpsc::Receiver<Event>,
}

impl Drop for Teleport {
    fn drop(&mut self) {
        // A move is never abandoned halfway; see the module comment.
        if !self.moving() {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

/// Run `work` on a dedicated thread with a fresh cancellation flag.
fn spawn(
    work: impl FnOnce(&mpsc::Sender<Event>, &AtomicBool) + Send + 'static,
) -> (Arc<AtomicBool>, mpsc::Receiver<Event>) {
    let (sender, receiver) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let spawned = std::thread::Builder::new()
        .name("herdr-teleport".into())
        .spawn(move || work(&sender, &flag));
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not start the teleport worker");
    }
    (cancelled, receiver)
}

impl Teleport {
    pub(crate) fn start(source: Source, label: String, hosts: Vec<HostRepositories>) -> Self {
        let hosts_searched = hosts.len();
        let job_source = source.clone();
        let (cancelled, events) = spawn(move |sender, cancelled| {
            let _ = sender.send(Event::Discovered(job::discover(
                &job_source,
                hosts,
                cancelled,
            )));
        });
        Self {
            source,
            label,
            hosts: hosts_searched,
            stage: Stage::Discovering,
            selected: 0,
            cancelled,
            events,
        }
    }

    /// A dialog already showing `candidates`, with no worker behind it.
    #[cfg(test)]
    pub(crate) fn choosing(source: Source, candidates: Vec<Candidate>) -> Self {
        let (_, events) = mpsc::channel();
        Self {
            source,
            label: "feature".into(),
            hosts: 1,
            stage: Stage::Choosing(Discovery {
                candidates,
                unreachable: Vec::new(),
            }),
            selected: 0,
            cancelled: Arc::new(AtomicBool::new(false)),
            events,
        }
    }

    #[cfg(test)]
    pub(crate) fn reviewing(&self) -> bool {
        matches!(self.stage, Stage::Reviewing(_))
    }

    pub(crate) fn moving(&self) -> bool {
        matches!(self.stage, Stage::Moving(..))
    }

    fn restart(&mut self, work: impl FnOnce(&mpsc::Sender<Event>, &AtomicBool) + Send + 'static) {
        self.cancelled.store(true, Ordering::Release);
        let (cancelled, events) = spawn(work);
        self.cancelled = cancelled;
        self.events = events;
    }

    fn review(&mut self) {
        let Stage::Choosing(discovery) = &self.stage else {
            return;
        };
        let Some(candidate) = discovery.candidates.get(self.selected).cloned() else {
            return;
        };
        let source = self.source.clone();
        let target = candidate.clone();
        self.restart(move |sender, cancelled| {
            let _ = sender.send(Event::Reviewed(job::review(&source, &target, cancelled)));
        });
        self.stage = Stage::Reviewing(candidate);
    }

    fn teleport(&mut self) {
        let Stage::Ready(candidate, review) = &self.stage else {
            return;
        };
        let (candidate, review) = (candidate.clone(), review.clone());
        let source = self.source.clone();
        let target = candidate.clone();
        self.restart(move |sender, cancelled| {
            let result = job::run(
                &source,
                &target,
                &review,
                |step| {
                    let _ = sender.send(Event::Step(step));
                },
                cancelled,
            );
            let _ = sender.send(Event::Moved(result));
        });
        self.stage = Stage::Moving(candidate, None);
    }

    /// Apply finished work. Returns a completed move's outcome once.
    fn poll(&mut self) -> (bool, Option<Result<Outcome, String>>) {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            changed = true;
            match event {
                Event::Discovered(Ok(discovery)) => {
                    self.selected = 0;
                    self.stage = Stage::Choosing(discovery);
                }
                Event::Reviewed(Ok(review)) => {
                    if let Stage::Reviewing(candidate) = &self.stage {
                        self.stage = Stage::Ready(candidate.clone(), Box::new(review));
                    }
                }
                Event::Step(step) => {
                    if let Stage::Moving(_, current) = &mut self.stage {
                        *current = Some(step);
                    }
                }
                Event::Moved(result) => {
                    let result = result.map_err(|error| error.to_string());
                    if let Err(error) = &result {
                        self.stage = Stage::Failed(error.clone());
                    }
                    return (true, Some(result));
                }
                Event::Discovered(Err(error)) | Event::Reviewed(Err(error)) => {
                    self.stage = Stage::Failed(error.to_string());
                }
            }
        }
        (changed, None)
    }

    fn destination_label(&self) -> Option<&str> {
        match &self.stage {
            Stage::Reviewing(c) | Stage::Ready(c, _) | Stage::Moving(c, _) => Some(&c.place.label),
            _ => None,
        }
    }
}

fn reason_text(reason: MatchReason) -> &'static str {
    match reason {
        MatchReason::Origin => "Same origin remote",
        MatchReason::Remote => "Shares a remote",
        MatchReason::Name => "Same name only; remotes differ",
    }
}

/// What a destination row says: where the worktree goes, and how.
fn destination_text(candidate: &Candidate) -> (String, String) {
    match &candidate.destination {
        Destination::Open { repository, reason } => {
            (repository.label.clone(), reason_text(*reason).to_owned())
        }
        Destination::Reclaim { repository, .. } => (
            repository.label.clone(),
            "Brings the work back to the checkout it left".to_owned(),
        ),
        Destination::Arrive(Arrival::Existing { path, .. }) => {
            (path.clone(), "Opens the checkout already there".to_owned())
        }
        Destination::Arrive(Arrival::Clone { path }) => (
            path.clone(),
            match candidate.origin {
                Some(_) => "Clones the repository there first".to_owned(),
                None => "Copies the repository there first".to_owned(),
            },
        ),
    }
}

fn name_only(candidate: &Candidate) -> bool {
    matches!(
        candidate.destination,
        Destination::Open {
            reason: MatchReason::Name,
            ..
        }
    )
}

fn describe(work: &Work, action: &Action) -> String {
    let program = work.program().unwrap_or("shell");
    match action {
        Action::Shell => "Shell".to_owned(),
        Action::Resume(agent) => format!("{}: resume session", agent.binary()),
        Action::Handoff { to, .. } if to.binary() == program => {
            format!("{program}: handoff note, then start again")
        }
        Action::Handoff { to, .. } => {
            format!("{program}: handoff note, continued by {}", to.binary())
        }
        Action::Start(argv) => format!("Start {}", command_line(argv)),
        Action::Run(argv) => format!("Run {}", command_line(argv)),
        Action::Missing(program) => format!("{program}: not installed there, skipped"),
    }
}

impl HerdrWindow {
    pub(crate) fn poll_teleport(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.teleport_marks.poll() {
            cx.notify();
        }
        self.follow_teleport(cx);
        let open = self.menu.page == Some(Page::Teleport);
        let Some(teleport) = &mut self.teleport else {
            return;
        };
        // Closing the dialog abandons discovery and review, but not a move.
        if !open && !teleport.moving() {
            self.teleport = None;
            return;
        }
        let (changed, finished) = teleport.poll();
        let Some(result) = finished else {
            if changed {
                cx.notify();
            }
            return;
        };
        let destination = teleport.destination_label().unwrap_or_default().to_owned();
        let source = teleport.source.clone();
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) if !open => {
                self.teleport = None;
                self.show_flash(Flash::warning(format!("Teleport failed: {error}")), cx);
                return;
            }
            Err(_) => return cx.notify(),
        };
        self.teleport = None;
        if open {
            self.dismiss_menu(window, cx);
        }
        for warning in &outcome.warnings {
            tracing::warn!(%warning, "teleport");
        }
        // The checkout left behind is marked; the one the work came back to,
        // if any, no longer is.
        self.teleport_marks
            .remove(&outcome.endpoint_id, &outcome.repo_key, &outcome.branch);
        self.teleport_marks.add(Mark {
            endpoint: source.place.endpoint_id.clone(),
            repo_key: source.repo_key.clone(),
            branch: outcome.branch.clone(),
            destination: MarkDestination {
                endpoint: outcome.endpoint_id.clone(),
                label: destination.clone(),
                repo_key: outcome.repo_key.clone(),
                workspace_id: outcome.workspace_id.clone(),
            },
        });
        let flash = match outcome.warnings.first() {
            None => Flash::success(format!("Teleported to {destination}")),
            Some(first) => Flash::warning(format!(
                "Teleported to {destination} with {} warning(s): {first}",
                outcome.warnings.len()
            )),
        };
        self.show_flash(flash, cx);
        self.teleport_follow = Some(Follow::new(outcome.endpoint_id, outcome.workspace_id));
        self.follow_teleport(cx);
    }

    /// Keep steering to the teleported workspace until it is focused: the
    /// host may still be connecting, its snapshot may not list the workspace
    /// yet, or a menu may be open. Gives up after a short while, so it never
    /// fights the user for long.
    fn follow_teleport(&mut self, cx: &mut Context<Self>) {
        let Some(follow) = &mut self.teleport_follow else {
            return;
        };
        let now = Instant::now();
        if now >= follow.until {
            self.teleport_follow = None;
            return;
        }
        if self.menu.page.is_some() || follow.next.is_some_and(|next| now < next) {
            return;
        }
        follow.next = Some(now + FOLLOW_RETRY);
        let (endpoint, workspace) = (follow.endpoint_id.clone(), follow.workspace_id.clone());
        if self.endpoints[self.selected_endpoint].id != endpoint {
            self.navigate_endpoint(&endpoint, NavigationTarget::Workspace(&workspace), cx);
            return;
        }
        let Some(snapshot) = &self.live.snapshot else {
            return;
        };
        if snapshot.focused_workspace_id.as_deref() == Some(workspace.as_str()) {
            self.teleport_follow = None;
            return;
        }
        let listed = snapshot
            .workspaces
            .iter()
            .any(|w| w.workspace_id == workspace);
        if listed && self.pending_navigation.is_none() {
            self.navigate(NavigationTarget::Workspace(&workspace), cx);
        }
    }

    pub(crate) fn teleport_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.prevent_default();
        let Some(teleport) = &mut self.teleport else {
            self.dismiss_menu(window, cx);
            return;
        };
        match event.keystroke.key.as_str() {
            "escape" => self.dismiss_menu(window, cx),
            "up" | "down" => {
                if let Stage::Choosing(discovery) = &teleport.stage
                    && !discovery.candidates.is_empty()
                {
                    let count = discovery.candidates.len();
                    teleport.selected = if event.keystroke.key == "up" {
                        (teleport.selected + count - 1) % count
                    } else {
                        (teleport.selected + 1) % count
                    };
                    cx.notify();
                }
            }
            "enter" => self.submit_teleport(window, cx),
            _ => {}
        }
    }

    fn submit_teleport(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(teleport) = &mut self.teleport else {
            return;
        };
        match teleport.stage {
            Stage::Choosing(_) => teleport.review(),
            Stage::Ready(..) => teleport.teleport(),
            Stage::Failed(_) => {
                self.teleport = None;
                self.dismiss_menu(window, cx);
                return;
            }
            _ => return,
        }
        cx.notify();
    }

    pub(crate) fn render_teleport(&self, cx: &mut Context<Self>) -> Div {
        let Some(teleport) = &self.teleport else {
            return div();
        };
        let theme = &self.theme;
        let font = &self.config.ui;
        let muted = rgb(theme.muted);
        let danger = crate::menu::danger(theme);
        let line = |text: String| div().min_w_0().child(text);
        let mut body = div().flex().flex_col().gap(px(8.)).px(px(16.)).py(px(12.));
        let (primary, armed) = match &teleport.stage {
            Stage::Discovering => {
                body = body.child(
                    line("Looking for this repository on your other hosts...".into())
                        .text_color(muted),
                );
                ("Review", false)
            }
            Stage::Choosing(discovery) => {
                if teleport.hosts == 0 {
                    body = body.child(line(
                        "No other host is connected. Connect one from Devices, then try again."
                            .into(),
                    ));
                } else if discovery.candidates.is_empty() {
                    body = body.child(line(
                        "No other connected host has this repository open. Open its main checkout there first.".into(),
                    ));
                }
                for (index, candidate) in discovery.candidates.iter().enumerate() {
                    let (title, detail) = destination_text(candidate);
                    let selected = index == teleport.selected;
                    body = body.child(
                        div()
                            .id(("teleport-candidate", index))
                            .debug_selector(move || format!("teleport-candidate-{index}"))
                            .px(px(10.))
                            .py(px(6.))
                            .rounded(px(crate::config::corners::CONTROL))
                            .cursor_pointer()
                            .when(selected, |row| row.bg(rgb(theme.active)))
                            .hover(|row| row.bg(rgb(theme.active)))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .min_w_0()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .flex_none()
                                            .child(candidate.place.label.clone()),
                                    )
                                    .child(div().truncate().child(title)),
                            )
                            .child(
                                div()
                                    .text_color(if name_only(candidate) { danger } else { muted })
                                    .child(detail),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                if let Some(teleport) = &mut this.teleport {
                                    teleport.selected = index;
                                }
                                this.submit_teleport(window, cx);
                            })),
                    );
                }
                for (host, error) in &discovery.unreachable {
                    body = body.child(line(format!("{host}: {error}")).text_color(danger));
                }
                ("Review", !discovery.candidates.is_empty())
            }
            Stage::Reviewing(candidate) => {
                body = body.child(
                    line(format!(
                        "Checking {} and the running programs...",
                        candidate.place.label
                    ))
                    .text_color(muted),
                );
                ("Teleport", false)
            }
            Stage::Ready(candidate, review) => {
                let state = &review.state;
                body = body
                    .child({
                        let (title, detail) = destination_text(candidate);
                        line(format!("To {} · {title}. {detail}.", candidate.place.label))
                    })
                    .child(line(format!(
                        "Branch {} · {} unpushed commit(s) · {} changed file(s), {} untracked",
                        review.branch, state.unpushed, state.changed, state.untracked
                    )));
                if review.reason == Some(MatchReason::Name) {
                    body = body.child(
                        line(
                            "Matched by name only: this repository's remotes differ there.".into(),
                        )
                        .text_color(danger),
                    );
                }
                for (index, tab) in review.tabs.iter().enumerate() {
                    let title = tab.label.clone().map_or_else(
                        || format!("Tab {}", index + 1),
                        |label| format!("Tab {}: {label}", index + 1),
                    );
                    body = body.child(
                        div()
                            .pt(px(4.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    );
                    for pane in &tab.panes {
                        let missing = matches!(pane.action, Action::Missing(_));
                        body = body.child(
                            div()
                                .pl(px(12.))
                                .truncate()
                                .when(missing, |row| row.text_color(danger))
                                .child(describe(&pane.work, &pane.action)),
                        );
                    }
                }
                body = body.child(line(
                    "Ignored files such as .env stay behind. The workspace here then closes; its checkout stays and can be reopened with Open worktree.".into(),
                ).text_color(muted));
                ("Teleport", true)
            }
            Stage::Moving(candidate, step) => {
                body = body
                    .child(line(format!("Moving to {}...", candidate.place.label)))
                    .child(
                        line(step.map_or("Starting", Step::label).to_owned())
                            .debug_selector(|| "teleport-step".into())
                            .text_color(muted),
                    )
                    .child(
                        line("Closing this dialog does not stop the move.".into())
                            .text_color(muted),
                    );
                ("Moving...", false)
            }
            Stage::Failed(error) => {
                body = body
                    .child(
                        line(error.clone())
                            .debug_selector(|| "teleport-error".into())
                            .text_color(danger),
                    )
                    .child(
                        line(
                            "The source workspace closes only once the destination is ready."
                                .into(),
                        )
                        .text_color(muted),
                    );
                ("Close", true)
            }
        };
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
                            .path("icons/teleport.svg")
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
                                    .child("Teleport"),
                            )
                            .child(div().truncate().text_color(muted).child(format!(
                                "{} from {}",
                                teleport.label, teleport.source.place.label
                            ))),
                    ),
            )
            .child(
                div()
                    .id("teleport-body")
                    .max_h(px(420.))
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
                        button("teleport-cancel")
                            .border_color(rgb(theme.active))
                            .hover(|button| button.bg(rgb(theme.active)))
                            .child(if teleport.moving() { "Hide" } else { "Cancel" })
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.dismiss_menu(window, cx);
                            })),
                    )
                    .child(
                        button("teleport-submit")
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
                                    |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.submit_teleport(window, cx);
                                    },
                                ))
                            }),
                    ),
            )
    }
}
