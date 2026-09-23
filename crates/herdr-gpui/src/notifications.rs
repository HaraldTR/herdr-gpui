//! Native delivery of semantic events only. Publish all windows with `prepare`
//! before the delivery/tick pass, and refresh registration on activation changes.
//! Call `reset` synchronously on selection changes, before changing endpoints.
use crate::{
    connection::ConnectionBridge,
    herdr_settings::{Settings, ToastDelivery, ToastPosition},
    state::{CapturedNotification, ConnectionStatus, LiveState, NOTIFICATION_CAPACITY},
};
use gpui::{Div, div, prelude::*, px, rgb};
use herdr_client::protocol::{
    AgentStatus, ClientShellSnapshot, SemanticNotification, SemanticNotificationKind,
    SemanticNotificationSound,
};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const TOAST_CAPACITY: usize = 4;
const TOAST_LIFETIME: Duration = Duration::from_secs(6);
const MAX_DAEMONS: usize = 32;
const MAX_WINDOWS: usize = 128;
const MAX_BOOT_BYTES: usize = 256;

static EXTERNAL: OnceLock<Arc<Mutex<ExternalRegistry>>> = OnceLock::new();

struct Registration {
    cancel: Weak<AtomicBool>,
    connection: Weak<AtomicBool>,
    inbox: Weak<Mutex<LiveState>>,
    active: bool,
    since: Instant,
}

impl Registration {
    fn live(&self) -> bool {
        self.cancel
            .upgrade()
            .is_some_and(|cancel| !cancel.load(Ordering::Acquire))
            && self
                .connection
                .upgrade()
                .is_some_and(|connection| connection.load(Ordering::Acquire))
            && self.inbox.strong_count() != 0
    }
}

struct Owner {
    cancel: Weak<AtomicBool>,
    since: Instant,
}

struct DaemonDelivery {
    boot: String,
    windows: Vec<Registration>,
    owner: Option<Owner>,
    in_flight: Weak<()>,
}

/// Only boot identities and weak lifecycle references are global. Notification
/// bodies, titles, agents, and target IDs remain in bounded window-local queues.
#[derive(Default)]
struct ExternalRegistry {
    daemons: Vec<DaemonDelivery>,
    overflow: bool,
}

enum Claim {
    Granted(Arc<()>),
    Busy,
    Suppressed,
}

impl ExternalRegistry {
    fn prune(&mut self) {
        for daemon in &mut self.daemons {
            daemon.windows.retain(Registration::live);
        }
        self.daemons
            .retain(|daemon| !daemon.windows.is_empty() || daemon.in_flight.strong_count() != 0);
        if self.daemons.is_empty() {
            self.overflow = false;
        }
    }

    fn register(&mut self, boot: &str, registration: Registration) {
        self.prune();
        if boot.len() > MAX_BOOT_BYTES {
            self.overflow = true;
            return;
        }
        if let Some(daemon) = self.daemons.iter_mut().find(|daemon| daemon.boot == boot)
            && let Some(window) = daemon
                .windows
                .iter_mut()
                .find(|window| window.cancel.ptr_eq(&registration.cancel))
        {
            window.active = registration.active;
            return;
        }
        if self
            .daemons
            .iter()
            .map(|daemon| daemon.windows.len())
            .sum::<usize>()
            >= MAX_WINDOWS
        {
            self.overflow = true;
            return;
        }
        if let Some(daemon) = self.daemons.iter_mut().find(|daemon| daemon.boot == boot) {
            daemon.windows.push(registration);
        } else if self.daemons.len() < MAX_DAEMONS {
            self.daemons.push(DaemonDelivery {
                boot: boot.into(),
                windows: vec![registration],
                owner: None,
                in_flight: Weak::new(),
            });
        } else {
            self.overflow = true;
        }
    }

    fn allowed(
        &mut self,
        boot: &str,
        cancel: &Arc<AtomicBool>,
        event: &SemanticNotification,
        received: Instant,
        now: Instant,
    ) -> bool {
        self.prune();
        // Missing a registration could hide an active target. Capacity failure
        // therefore disables external delivery until registrations drain.
        if self.overflow {
            return false;
        }
        let Some(daemon) = self.daemons.iter_mut().find(|daemon| daemon.boot == boot) else {
            return false;
        };
        if !daemon.owner.as_ref().is_some_and(|owner| {
            daemon
                .windows
                .iter()
                .any(|window| window.cancel.ptr_eq(&owner.cancel))
        }) {
            let Some(window) = daemon
                .windows
                .iter()
                .find(|window| window.active)
                .or_else(|| daemon.windows.first())
            else {
                return false;
            };
            // Stable ownership avoids duplicate effects when focus changes while
            // different socket readers are still draining the same broadcast.
            // A successor must discard captures predating the handoff.
            let since = if daemon.owner.is_some() {
                now
            } else {
                window.since
            };
            daemon.owner = Some(Owner {
                cancel: window.cancel.clone(),
                since,
            });
        }
        if !daemon.owner.as_ref().is_some_and(|owner| {
            owner.cancel.ptr_eq(&Arc::downgrade(cancel)) && received >= owner.since
        }) {
            return false;
        }
        for window in daemon.windows.iter().filter(|window| window.active) {
            let Some(inbox) = window.inbox.upgrade() else {
                continue;
            };
            // Fail closed on contention; never beep behind an active view whose
            // current focus cannot be inspected without blocking the UI thread.
            let Ok(state) = inbox.try_lock() else {
                return false;
            };
            if state.status == ConnectionStatus::Connected
                && state.snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.boot_id == boot && target_focused(event, snapshot)
                })
            {
                return false;
            }
        }
        true
    }

    fn claim(
        &mut self,
        boot: &str,
        cancel: &Arc<AtomicBool>,
        event: &SemanticNotification,
        received: Instant,
        now: Instant,
    ) -> Claim {
        if !self.allowed(boot, cancel, event, received, now) {
            return Claim::Suppressed;
        }
        let Some(daemon) = self.daemons.iter_mut().find(|daemon| daemon.boot == boot) else {
            return Claim::Suppressed;
        };
        if daemon.in_flight.strong_count() != 0 {
            return Claim::Busy;
        }
        // Reserve before spawning: old/new owners cannot race helper processes
        // across a reset, even while the old worker is still cancelling.
        let permit = Arc::new(());
        daemon.in_flight = Arc::downgrade(&permit);
        Claim::Granted(permit)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("native notification I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("native notification backend exited unsuccessfully")]
    Backend,
    #[error("native notification backend timed out")]
    Timeout,
    #[error("native notifications are unsupported on this platform")]
    Unsupported,
}

struct Toast {
    event: CapturedNotification,
    expires: Instant,
}

// Prepared policy keeps the queue reducer testable without loading user config
// or running an OS backend. Production always derives it from shared Settings.
struct Policy<F> {
    delivery: ToastDelivery,
    position: ToastPosition,
    delay_seconds: u64,
    sound: F,
}

struct Worker {
    result: mpsc::Receiver<Result<(), Error>>,
    cancelled: Arc<AtomicBool>,
}

pub(crate) struct Notifications {
    external: Arc<Mutex<ExternalRegistry>>,
    inbox: Weak<Mutex<LiveState>>,
    selection: u64,
    selected_since: Instant,
    boot: Option<String>,
    pending: VecDeque<CapturedNotification>,
    sounds: VecDeque<CapturedNotification>,
    toasts: VecDeque<Toast>,
    cancel: Arc<AtomicBool>,
    worker: Option<Worker>,
    pub(crate) error: Option<Error>,
    error_expires: Option<Instant>,
    position: ToastPosition,
    delivery: ToastDelivery,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            external: EXTERNAL.get_or_init(Arc::default).clone(),
            inbox: Weak::new(),
            selection: 0,
            selected_since: Instant::now(),
            boot: None,
            pending: VecDeque::new(),
            sounds: VecDeque::new(),
            toasts: VecDeque::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            worker: None,
            error: None,
            error_expires: None,
            position: ToastPosition::default(),
            delivery: ToastDelivery::Off,
        }
    }
}

impl Notifications {
    /// Cancels queued work without joining a process/worker on the UI thread.
    pub(crate) fn reset(&mut self) {
        self.selected_since = Instant::now();
        self.cancel.store(true, Ordering::Release);
        if let Ok(mut external) = self.external.try_lock() {
            external.prune();
        }
        self.cancel = Arc::new(AtomicBool::new(false));
        self.pending.clear();
        self.sounds.clear();
        self.toasts.clear();
        self.error = None;
        self.error_expires = None;
        // Keep the receiver until the cancelled worker exits: at most one worker.
    }

    /// Publish every live window before draining any notification queues, and on
    /// window activation changes. Returns whether the connection fence changed.
    /// `tick` also calls this, but cannot discover other windows on its own.
    pub(crate) fn prepare(
        &mut self,
        bridge: &ConnectionBridge,
        live: &LiveState,
        selection: u64,
        active: bool,
        now: Instant,
    ) -> bool {
        let boot = live.snapshot.as_ref().map(|s| s.boot_id.as_str());
        let changed = !Weak::ptr_eq(&self.inbox, &Arc::downgrade(&bridge.inbox))
            || self.selection != selection
            || self.boot.as_deref() != boot;
        if changed {
            let selected_since = self.selected_since;
            self.reset();
            if self.selection == selection {
                self.selected_since = selected_since;
            }
            self.inbox = Arc::downgrade(&bridge.inbox);
            self.selection = selection;
            self.boot = boot.map(str::to_owned);
        }
        if live.status == ConnectionStatus::Connected {
            if let Some(boot) = boot
                && let Ok(mut external) = self.external.try_lock()
            {
                external.register(
                    boot,
                    Registration {
                        cancel: Arc::downgrade(&self.cancel),
                        connection: Arc::downgrade(&bridge.notification_active),
                        inbox: self.inbox.clone(),
                        active,
                        since: self.selected_since.min(now),
                    },
                );
            }
        } else {
            self.reset();
        }
        changed
    }

    fn external_allowed(&self, event: &CapturedNotification, now: Instant) -> bool {
        self.external.try_lock().is_ok_and(|mut external| {
            external.allowed(
                &event.boot,
                &self.cancel,
                &event.notification,
                event.received,
                now,
            )
        })
    }

    /// Returns whether presentation changed. No blocking I/O or process waits.
    /// Unselected endpoint inboxes must be drained/discarded by the caller.
    pub(crate) fn tick(
        &mut self,
        bridge: &ConnectionBridge,
        live: &LiveState,
        selection: u64,
        active: bool,
        settings: &Settings,
        now: Instant,
    ) -> bool {
        self.tick_with_policy(
            bridge,
            live,
            selection,
            active,
            Policy {
                delivery: settings.toast_delivery,
                position: settings.toast_position,
                delay_seconds: settings.toast_delay_seconds,
                sound: |event: &SemanticNotification| {
                    sound_path(
                        event,
                        |agent| settings.sound_allowed(agent),
                        |request| settings.sound_path(request),
                    )
                },
            },
            now,
        )
    }

    fn tick_with_policy(
        &mut self,
        bridge: &ConnectionBridge,
        live: &LiveState,
        selection: u64,
        active: bool,
        policy: Policy<impl Fn(&SemanticNotification) -> Option<PathBuf>>,
        now: Instant,
    ) -> bool {
        let mut changed = self.prepare(bridge, live, selection, active, now);
        if self.delivery != policy.delivery || self.position != policy.position {
            self.toasts.clear();
            changed = true;
        }
        self.delivery = policy.delivery;
        self.position = policy.position;
        if self.error_expires.is_some_and(|expires| now >= expires) {
            self.error = None;
            self.error_expires = None;
            changed = true;
        }
        if let Some(worker) = &self.worker {
            match worker.result.try_recv() {
                Ok(result) => {
                    if !worker.cancelled.load(Ordering::Acquire) {
                        self.error = result.err();
                        self.error_expires = self.error.as_ref().map(|_| now + TOAST_LIFETIME);
                    }
                    self.worker = None;
                    changed = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.worker = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let incoming = bridge.take_notifications();
        let Some(snapshot) = live
            .snapshot
            .as_ref()
            .filter(|_| live.status == ConnectionStatus::Connected)
        else {
            changed |= !self.toasts.is_empty();
            self.reset();
            return changed;
        };
        for event in incoming {
            if event.received < self.selected_since || event.boot != snapshot.boot_id {
                continue;
            }
            let external = self.external_allowed(&event, now);
            if external
                && event.notification.sound.is_some()
                && (policy.sound)(&event.notification).is_some()
            {
                if self.sounds.len() == NOTIFICATION_CAPACITY {
                    self.sounds.pop_front();
                }
                self.sounds.push_back(event.clone());
            }
            if self.delivery == ToastDelivery::Herdr
                || (external && self.delivery == ToastDelivery::System && cfg!(target_os = "macos"))
            {
                if self.pending.len() == NOTIFICATION_CAPACITY {
                    self.pending.pop_front();
                }
                self.pending.push_back(event);
            }
        }
        let previous = self.toasts.len();
        self.toasts.retain(|toast| {
            now < toast.expires && eligible(&toast.event.notification, snapshot, active)
        });
        changed |= previous != self.toasts.len();
        // Audio has its own bounded queue: toast delay and rendering never wait
        // for afplay/osascript. Re-evaluate settings and state at dispatch time.
        self.sounds.retain(|event| {
            event.boot == snapshot.boot_id
                && now.saturating_duration_since(event.received) < TOAST_LIFETIME
                && self.external.try_lock().is_ok_and(|mut external| {
                    external.allowed(
                        &event.boot,
                        &self.cancel,
                        &event.notification,
                        event.received,
                        now,
                    )
                })
                && (policy.sound)(&event.notification).is_some()
        });
        for _ in 0..self.sounds.len() {
            if self.worker.is_some() {
                break;
            }
            let Some(event) = self.sounds.pop_front() else {
                break;
            };
            if !self.external_allowed(&event, now) {
                continue;
            }
            if awaiting_snapshot(&event, snapshot, now) {
                self.sounds.push_back(event);
                continue;
            }
            if !eligible(&event.notification, snapshot, active) {
                continue;
            }
            if let Some(sound) = (policy.sound)(&event.notification) {
                if !self.deliver(bridge, &event, Some(sound), false, now) {
                    self.sounds.push_back(event);
                }
                changed = true;
            }
        }
        // Scan all events so an immediate Custom toast cannot be held behind a
        // delayed agent toast, nor an in-app toast behind a backend worker.
        for _ in 0..self.pending.len() {
            let Some(event) = self.pending.pop_front() else {
                break;
            };
            if self.delivery == ToastDelivery::System && !self.external_allowed(&event, now) {
                continue;
            }
            let delay = if event.notification.kind == SemanticNotificationKind::Custom {
                Duration::ZERO
            } else {
                Duration::from_secs(policy.delay_seconds.min(3600))
            };
            // Drop stale bursts rather than replaying them after a blocked UI.
            if now.saturating_duration_since(event.received) > delay + TOAST_LIFETIME
                || event.boot != snapshot.boot_id
            {
                continue;
            }
            if now.saturating_duration_since(event.received) < delay
                || awaiting_snapshot(&event, snapshot, now)
                || (self.delivery == ToastDelivery::System && self.worker.is_some())
            {
                self.pending.push_back(event);
                continue;
            }
            if !eligible(&event.notification, snapshot, active) {
                continue;
            }
            if self.delivery == ToastDelivery::System {
                if !self.deliver(bridge, &event, None, true, now) {
                    self.pending.push_back(event);
                }
                changed = true;
                continue;
            }
            if self.delivery == ToastDelivery::Herdr {
                if self.toasts.len() == TOAST_CAPACITY {
                    self.toasts.pop_front();
                }
                self.toasts.push_back(Toast {
                    event,
                    expires: now + TOAST_LIFETIME,
                });
                changed = true;
            }
        }
        changed
    }

    fn deliver(
        &mut self,
        bridge: &ConnectionBridge,
        event: &CapturedNotification,
        sound: Option<PathBuf>,
        system: bool,
        now: Instant,
    ) -> bool {
        let claim = self.external.try_lock().map(|mut external| {
            external.claim(
                &event.boot,
                &self.cancel,
                &event.notification,
                event.received,
                now,
            )
        });
        let permit = match claim {
            Ok(Claim::Granted(permit)) => permit,
            Ok(Claim::Busy) | Err(_) => return false,
            Ok(Claim::Suppressed) => return true,
        };
        let job = Delivery {
            external: self.external.clone(),
            permit: Some(permit),
            received: event.received,
            notification: event.notification.clone(),
            sound,
            system,
            cancel: self.cancel.clone(),
            connection: bridge.notification_active.clone(),
            inbox: self.inbox.clone(),
            boot: event.boot.clone(),
        };
        let (send, receive) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("herdr-notification".into())
            .spawn(move || {
                let _ = send.send(job.run());
            }) {
            Ok(_) => {
                self.worker = Some(Worker {
                    result: receive,
                    cancelled: self.cancel.clone(),
                })
            }
            Err(error) => {
                self.error = Some(error.into());
                self.error_expires = Some(now + TOAST_LIFETIME);
            }
        }
        true
    }

    /// Overlay inside a relatively positioned root. No focus, clipboard, or navigation effects.
    pub(crate) fn render_notifications(&self, theme: &crate::config::Theme) -> Div {
        let mut root = div()
            .absolute()
            .w(px(320.))
            .max_w_full()
            .max_h_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap_2();
        root = match self.position {
            ToastPosition::TopLeft => root.top_4().left_4(),
            ToastPosition::TopRight => root.top_4().right_4(),
            ToastPosition::BottomLeft => root.bottom_4().left_4(),
            ToastPosition::BottomRight => root.bottom_4().right_4(),
        };
        for toast in &self.toasts {
            root = root.child(
                div()
                    .p_3()
                    .rounded_md()
                    .max_h(px(160.))
                    .overflow_hidden()
                    .bg(rgb(theme.surface))
                    .text_color(rgb(theme.foreground))
                    .child(div().child(plain(&toast.event.notification.title)))
                    .children(
                        toast
                            .event
                            .notification
                            .body
                            .as_deref()
                            .map(|body| div().text_sm().child(plain(body))),
                    ),
            );
        }
        if let Some(message) = self.status() {
            root = root.child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(theme.surface))
                    .text_color(rgb(theme.foreground))
                    .text_sm()
                    .child(message),
            );
        }
        root
    }

    pub(crate) fn status(&self) -> Option<String> {
        // Unsupported modes are explained in Settings, not a permanent overlay.
        self.error.as_ref().map(ToString::to_string)
    }
}

impl Drop for Notifications {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Ok(mut external) = self.external.try_lock() {
            external.prune();
        }
    }
}

fn plain(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(2048)
        .collect()
}

fn sound_path(
    event: &SemanticNotification,
    allowed: impl FnOnce(Option<&str>) -> bool,
    configured: impl FnOnce(bool) -> Option<PathBuf>,
) -> Option<PathBuf> {
    let sound = event.sound?;
    if !allowed(event.agent.as_deref()) {
        return None;
    }
    let request = sound == SemanticNotificationSound::Request;
    configured(request).or_else(|| cfg!(target_os = "macos").then(|| default_sound(request)))
}

// Native defaults are macOS system sounds, not copies of upstream Herdr assets.
fn default_sound(request: bool) -> PathBuf {
    PathBuf::from(if request {
        "/System/Library/Sounds/Glass.aiff"
    } else {
        "/System/Library/Sounds/Ping.aiff"
    })
}

/// Background-only: invalid/unreadable/undecodable custom audio falls back to
/// the OS sound. The player receives only trusted local settings/default paths.
fn play_sound(
    path: &Path,
    fallback: &Path,
    mut play: impl FnMut(&Path) -> Result<(), Error>,
) -> Result<(), Error> {
    if path == fallback {
        return play(fallback);
    }
    if path.is_absolute() && std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        match play(path) {
            Ok(()) => return Ok(()),
            Err(Error::Io(_) | Error::Backend) => {}
            Err(error) => return Err(error),
        }
    }
    play(fallback)
}

fn awaiting_snapshot(
    event: &CapturedNotification,
    snapshot: &ClientShellSnapshot,
    now: Instant,
) -> bool {
    if now.saturating_duration_since(event.received) >= Duration::from_secs(1) {
        return false;
    }
    let notification = &event.notification;
    let Some(pane) = notification.pane_id.as_deref() else {
        return false;
    };
    if !matches!(
        notification.kind,
        SemanticNotificationKind::Finished | SemanticNotificationKind::NeedsAttention
    ) {
        return false;
    }
    // Attention/completion evidence can precede its projection on the same stream.
    // Keep it briefly, but never deliver until a current snapshot confirms it.
    snapshot
        .agents
        .iter()
        .find(|agent| agent.pane_id == pane)
        .is_none_or(|agent| agent.agent_status == AgentStatus::Working)
}

fn eligible(event: &SemanticNotification, snapshot: &ClientShellSnapshot, active: bool) -> bool {
    let workspace = event.workspace_id.as_deref();
    let tab = event.tab_id.as_deref();
    let pane = event.pane_id.as_deref();
    if workspace.is_some_and(|id| !snapshot.workspaces.iter().any(|w| w.workspace_id == id))
        || tab.is_some_and(|id| {
            !snapshot
                .tabs
                .iter()
                .any(|t| t.tab_id == id && workspace.is_none_or(|w| t.workspace_id == w))
        })
        || pane.is_some_and(|id| {
            !snapshot.panes.iter().any(|p| {
                p.pane_id == id
                    && workspace.is_none_or(|w| p.workspace_id == w)
                    && tab.is_none_or(|t| p.tab_id == t)
            })
        })
    {
        return false;
    }
    // A semantic completion is advisory, not independent completion evidence.
    // Validate again after delay and on the worker before any external effect.
    let agent = pane.and_then(|id| snapshot.agents.iter().find(|agent| agent.pane_id == id));
    match event.kind {
        SemanticNotificationKind::Finished
            if agent.is_none_or(|agent| agent.agent_status != AgentStatus::Done) =>
        {
            return false;
        }
        SemanticNotificationKind::NeedsAttention
            if pane.is_some()
                && agent.is_none_or(|agent| agent.agent_status != AgentStatus::Blocked) =>
        {
            return false;
        }
        _ => {}
    }
    !(active && target_focused(event, snapshot))
}

fn target_focused(event: &SemanticNotification, snapshot: &ClientShellSnapshot) -> bool {
    if let Some(id) = event.pane_id.as_deref() {
        snapshot.focused_pane_id.as_deref() == Some(id)
    } else if let Some(id) = event.tab_id.as_deref() {
        snapshot.focused_tab_id.as_deref() == Some(id)
    } else if let Some(id) = event.workspace_id.as_deref() {
        snapshot.focused_workspace_id.as_deref() == Some(id)
    } else {
        false
    }
}

struct Delivery {
    external: Arc<Mutex<ExternalRegistry>>,
    permit: Option<Arc<()>>,
    received: Instant,
    notification: SemanticNotification,
    sound: Option<PathBuf>,
    system: bool,
    cancel: Arc<AtomicBool>,
    connection: Arc<AtomicBool>,
    inbox: Weak<Mutex<LiveState>>,
    boot: String,
}

impl Delivery {
    fn current(&self) -> bool {
        !self.cancel.load(Ordering::Acquire)
            && self.connection.load(Ordering::Acquire)
            && self.external.try_lock().is_ok_and(|mut external| {
                external.allowed(
                    &self.boot,
                    &self.cancel,
                    &self.notification,
                    self.received,
                    Instant::now(),
                )
            })
            && self.inbox.upgrade().is_some_and(|inbox| {
                inbox.lock().is_ok_and(|state| {
                    state.status == ConnectionStatus::Connected
                        && state.snapshot.as_ref().is_some_and(|s| {
                            s.boot_id == self.boot && eligible(&self.notification, s, false)
                        })
                })
            })
    }

    fn run(self) -> Result<(), Error> {
        if !self.current() {
            return Ok(());
        }
        if !cfg!(target_os = "macos") {
            return Err(Error::Unsupported);
        }
        if self.system {
            // Remote text is argv data to a fixed script, never executable source.
            let mut command = std::process::Command::new("/usr/bin/osascript");
            command.args(["-e", "on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv)\nend run", "--"])
                .arg(plain(&self.notification.title))
                .arg(self.notification.body.as_deref().map(plain).unwrap_or_default());
            self.execute(command)?;
        }
        if let Some(path) = &self.sound {
            let fallback =
                default_sound(self.notification.sound == Some(SemanticNotificationSound::Request));
            play_sound(path, &fallback, |path| {
                let mut command = std::process::Command::new("/usr/bin/afplay");
                command.arg(path);
                self.execute(command)
            })?;
        }
        Ok(())
    }

    fn execute(&self, mut command: std::process::Command) -> Result<(), Error> {
        if !self.current() {
            return Ok(());
        }
        let mut child = command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return if status.success() {
                        Ok(())
                    } else {
                        Err(Error::Backend)
                    };
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.into());
                }
            }
            let current = self.current();
            if !current || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return if current { Err(Error::Timeout) } else { Ok(()) };
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Delivery {
    fn drop(&mut self) {
        self.permit = None;
        if let Ok(mut external) = self.external.try_lock() {
            external.prune();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use herdr_client::{ConnectTarget, protocol::SemanticNotificationKind};

    fn event() -> SemanticNotification {
        SemanticNotification {
            kind: SemanticNotificationKind::Custom,
            title: "Done".into(),
            body: None,
            sound: Some(SemanticNotificationSound::Done),
            agent: None,
            workspace_id: None,
            tab_id: None,
            pane_id: None,
            position: None,
        }
    }

    #[test]
    fn target_validation_and_focus_suppression() {
        let snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(
            "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
        ))
        .unwrap();
        let mut event = event();
        assert!(eligible(&event, &snapshot, true));
        event.pane_id = snapshot.focused_pane_id.clone();
        assert!(!eligible(&event, &snapshot, true));
        assert!(eligible(&event, &snapshot, false));
        event.tab_id = Some("missing".into());
        assert!(!eligible(&event, &snapshot, false));
        event.tab_id = None;
        event.pane_id = Some("missing".into());
        assert!(!eligible(&event, &snapshot, false));
    }

    #[test]
    fn cancelled_or_stale_delivery_never_reaches_a_backend() {
        let mut bridge = ConnectionBridge::new(ConnectTarget::Socket("/unused.sock".into()));
        let snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(
            "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
        ))
        .unwrap();
        {
            let mut state = bridge.inbox.lock().unwrap();
            state.status = ConnectionStatus::Connected;
            state.snapshot = Some(Arc::new(snapshot.clone()));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let external = Arc::new(Mutex::new(ExternalRegistry::default()));
        let received = Instant::now();
        external.lock().unwrap().register(
            &snapshot.boot_id,
            Registration {
                cancel: Arc::downgrade(&cancel),
                connection: Arc::downgrade(&bridge.notification_active),
                inbox: Arc::downgrade(&bridge.inbox),
                active: false,
                since: received,
            },
        );
        let job = Delivery {
            external,
            permit: None,
            received,
            notification: event(),
            sound: None,
            system: false,
            cancel: cancel.clone(),
            connection: bridge.notification_active.clone(),
            inbox: Arc::downgrade(&bridge.inbox),
            boot: snapshot.boot_id,
        };
        assert!(job.current());
        cancel.store(true, Ordering::Release);
        assert!(!job.current());
        cancel.store(false, Ordering::Release);
        bridge.detach(false);
        assert!(!job.current());
        assert!(job.run().is_ok());
    }

    #[test]
    fn terminal_mode_has_no_permanent_overlay_and_text_drops_controls() {
        let mut notifications = Notifications::default();
        notifications.delivery = ToastDelivery::Terminal;
        assert!(notifications.status().is_none());
        assert_eq!(plain("hello\u{1b}\u{7}\nworld"), "helloworld");
        assert_eq!(plain(&"x".repeat(4096)).len(), 2048);
    }

    #[test]
    fn sound_uses_semantic_kind_and_local_policy_only() {
        let mut event = event();
        event.agent = Some("claude".into());
        event.title = "/remote/sound; command".into();
        assert_eq!(
            sound_path(&event, |_| false, |_| panic!("disabled sound")),
            None
        );
        for (kind, request) in [
            (SemanticNotificationSound::Done, false),
            (SemanticNotificationSound::Request, true),
        ] {
            event.sound = Some(kind);
            assert_eq!(
                sound_path(
                    &event,
                    |agent| {
                        assert_eq!(agent, Some("claude"));
                        true
                    },
                    |is_request| {
                        assert_eq!(is_request, request);
                        Some("/trusted/local.aiff".into())
                    }
                ),
                Some("/trusted/local.aiff".into())
            );
        }
        event.sound = None;
        assert_eq!(
            sound_path(
                &event,
                |_| panic!("no sound hint"),
                |_| panic!("no sound hint")
            ),
            None
        );
    }

    fn fixture() -> (Notifications, ConnectionBridge, LiveState, Instant) {
        let mut notifications = Notifications::default();
        notifications.external = Arc::default();
        let bridge = ConnectionBridge::new(ConnectTarget::Socket("/unused.sock".into()));
        let mut live = LiveState::default();
        live.status = ConnectionStatus::Connected;
        live.snapshot = Some(Arc::new(
            serde_json::from_str(include_str!(
                "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
            ))
            .unwrap(),
        ));
        *bridge.inbox.lock().unwrap() = live.clone();
        (notifications, bridge, live, Instant::now())
    }

    fn silent(delay_seconds: u64) -> Policy<impl Fn(&SemanticNotification) -> Option<PathBuf>> {
        Policy {
            delivery: ToastDelivery::Herdr,
            position: ToastPosition::BottomRight,
            delay_seconds,
            sound: |_: &SemanticNotification| None,
        }
    }

    fn capture(bridge: &ConnectionBridge, notification: SemanticNotification, now: Instant) {
        let mut state = bridge.inbox.lock().unwrap();
        let boot = state.snapshot.as_ref().unwrap().boot_id.clone();
        state.notifications.push_back(CapturedNotification {
            boot,
            received: now,
            notification,
        });
    }

    fn attention() -> SemanticNotification {
        let mut event = event();
        event.kind = SemanticNotificationKind::NeedsAttention;
        event.pane_id = Some("w1:p1".into());
        event.sound = Some(SemanticNotificationSound::Request);
        event
    }

    #[test]
    fn tick_sound_policy_is_immediate_but_agent_toast_waits() {
        let (mut notifications, bridge, live, now) = fixture();
        capture(&bridge, attention(), now);
        let evaluated = std::cell::Cell::new(0);
        notifications.tick_with_policy(
            &bridge,
            &live,
            0,
            false,
            Policy {
                sound: |_: &SemanticNotification| {
                    evaluated.set(evaluated.get() + 1);
                    None
                },
                delivery: ToastDelivery::Herdr,
                position: ToastPosition::BottomRight,
                delay_seconds: 3,
            },
            now,
        );
        assert_eq!(evaluated.get(), 1);
        assert_eq!(notifications.pending.len(), 1);
        assert!(notifications.toasts.is_empty());
        assert!(notifications.sounds.is_empty());
        notifications.tick_with_policy(
            &bridge,
            &live,
            0,
            false,
            silent(3),
            now + Duration::from_secs(2),
        );
        assert!(notifications.toasts.is_empty());
        notifications.tick_with_policy(
            &bridge,
            &live,
            0,
            false,
            silent(3),
            now + Duration::from_secs(3),
        );
        assert_eq!(notifications.toasts.len(), 1);
        assert!(notifications.pending.is_empty());
        assert!(notifications.worker.is_none());
    }

    #[test]
    fn tick_custom_bypasses_delayed_head_and_busy_worker() {
        let (mut notifications, bridge, live, now) = fixture();
        notifications.tick_with_policy(&bridge, &live, 0, false, silent(3), now);
        let (_send, receive) = mpsc::sync_channel(1);
        notifications.worker = Some(Worker {
            result: receive,
            cancelled: notifications.cancel.clone(),
        });
        capture(&bridge, attention(), now);
        capture(&bridge, event(), now);
        notifications.tick_with_policy(&bridge, &live, 0, false, silent(3), now);
        assert_eq!(notifications.toasts.len(), 1);
        assert_eq!(
            notifications.toasts[0].event.notification.kind,
            SemanticNotificationKind::Custom
        );
        assert_eq!(notifications.pending.len(), 1);
        notifications.tick_with_policy(
            &bridge,
            &live,
            0,
            false,
            silent(3),
            now + Duration::from_secs(3),
        );
        assert_eq!(notifications.toasts.len(), 2);
        assert!(notifications.pending.is_empty());
    }

    #[test]
    fn tick_queues_and_visible_toasts_are_bounded_and_expire() {
        let (mut notifications, bridge, live, now) = fixture();
        notifications.tick_with_policy(&bridge, &live, 0, false, silent(60), now);
        let (_send, receive) = mpsc::sync_channel(1);
        notifications.worker = Some(Worker {
            result: receive,
            cancelled: notifications.cancel.clone(),
        });
        for _ in 0..3 {
            for _ in 0..NOTIFICATION_CAPACITY {
                capture(&bridge, attention(), now);
            }
            notifications.tick_with_policy(
                &bridge,
                &live,
                0,
                false,
                Policy {
                    delivery: ToastDelivery::Herdr,
                    position: ToastPosition::BottomRight,
                    delay_seconds: 60,
                    sound: |_: &SemanticNotification| Some("/never-played.aiff".into()),
                },
                now,
            );
        }
        assert_eq!(notifications.pending.len(), NOTIFICATION_CAPACITY);
        assert_eq!(notifications.sounds.len(), NOTIFICATION_CAPACITY);
        for _ in 0..NOTIFICATION_CAPACITY {
            capture(&bridge, event(), now);
        }
        notifications.tick_with_policy(&bridge, &live, 0, false, silent(60), now);
        assert_eq!(notifications.toasts.len(), TOAST_CAPACITY);
        notifications.tick_with_policy(&bridge, &live, 0, false, silent(60), now + TOAST_LIFETIME);
        assert!(notifications.toasts.is_empty());
        assert!(notifications.sounds.is_empty());
    }

    #[test]
    fn tick_drops_stale_agent_status_and_focused_toasts() {
        let (mut notifications, bridge, mut live, now) = fixture();
        capture(&bridge, attention(), now);
        notifications.tick_with_policy(&bridge, &live, 0, false, silent(2), now);
        Arc::make_mut(live.snapshot.as_mut().unwrap()).agents[0].agent_status =
            AgentStatus::Working;
        notifications.tick_with_policy(
            &bridge,
            &live,
            0,
            false,
            silent(2),
            now + Duration::from_secs(2),
        );
        assert!(notifications.pending.is_empty() && notifications.toasts.is_empty());
        let mut completion = attention();
        completion.kind = SemanticNotificationKind::Finished;
        assert!(!eligible(
            &completion,
            live.snapshot.as_ref().unwrap(),
            false
        ));
        Arc::make_mut(live.snapshot.as_mut().unwrap()).agents[0].agent_status = AgentStatus::Done;
        assert!(eligible(
            &completion,
            live.snapshot.as_ref().unwrap(),
            false
        ));
        assert!(!eligible(
            &completion,
            live.snapshot.as_ref().unwrap(),
            true
        ));
        completion.pane_id = None;
        assert!(!eligible(
            &completion,
            live.snapshot.as_ref().unwrap(),
            false
        ));
    }

    #[test]
    fn tick_reset_cancels_queues_and_ignores_old_errors() {
        let (mut notifications, bridge, live, now) = fixture();
        capture(&bridge, attention(), now);
        notifications.tick_with_policy(&bridge, &live, 0, false, silent(60), now);
        let (send, receive) = mpsc::sync_channel(1);
        let cancelled = notifications.cancel.clone();
        notifications.worker = Some(Worker {
            result: receive,
            cancelled: cancelled.clone(),
        });
        notifications.reset();
        assert!(cancelled.load(Ordering::Acquire));
        assert!(notifications.pending.is_empty() && notifications.sounds.is_empty());
        send.send(Err(Error::Backend)).unwrap();
        // Old captures from the newly selected host must not be replayed.
        capture(&bridge, event(), now);
        notifications.tick_with_policy(&bridge, &live, 1, false, silent(60), Instant::now());
        assert!(notifications.worker.is_none() && notifications.error.is_none());
        assert!(notifications.toasts.is_empty() && notifications.pending.is_empty());
    }

    #[test]
    fn tick_actionable_errors_expire() {
        let (mut notifications, bridge, live, now) = fixture();
        notifications.tick_with_policy(&bridge, &live, 0, false, silent(0), now);
        let (send, receive) = mpsc::sync_channel(1);
        notifications.worker = Some(Worker {
            result: receive,
            cancelled: notifications.cancel.clone(),
        });
        send.send(Err(Error::Backend)).unwrap();
        notifications.tick_with_policy(&bridge, &live, 0, false, silent(0), now);
        assert!(notifications.status().is_some());
        notifications.tick_with_policy(&bridge, &live, 0, false, silent(0), now + TOAST_LIFETIME);
        assert!(notifications.status().is_none());
    }

    #[test]
    fn tick_completion_waits_briefly_for_done_projection_without_false_delivery() {
        let (mut notifications, bridge, mut live, now) = fixture();
        let mut completion = attention();
        completion.kind = SemanticNotificationKind::Finished;
        Arc::make_mut(live.snapshot.as_mut().unwrap()).agents[0].agent_status =
            AgentStatus::Working;
        capture(&bridge, completion.clone(), now);
        notifications.tick_with_policy(&bridge, &live, 0, false, silent(0), now);
        assert_eq!(notifications.pending.len(), 1);
        assert!(notifications.toasts.is_empty());
        Arc::make_mut(live.snapshot.as_mut().unwrap()).agents[0].agent_status = AgentStatus::Done;
        notifications.tick_with_policy(
            &bridge,
            &live,
            0,
            false,
            silent(0),
            now + Duration::from_millis(50),
        );
        assert!(notifications.pending.is_empty());
        assert_eq!(notifications.toasts.len(), 1);
        Arc::make_mut(live.snapshot.as_mut().unwrap()).agents[0].agent_status =
            AgentStatus::Working;
        capture(&bridge, completion, now);
        notifications.tick_with_policy(
            &bridge,
            &live,
            0,
            false,
            silent(0),
            now + Duration::from_secs(1),
        );
        assert!(notifications.pending.is_empty() && notifications.toasts.is_empty());
        assert!(notifications.sounds.is_empty() && notifications.worker.is_none());
    }

    #[test]
    fn invalid_custom_audio_uses_os_fallback_without_running_player() {
        let fallback = default_sound(true);
        assert_eq!(fallback, Path::new("/System/Library/Sounds/Glass.aiff"));
        assert_eq!(
            default_sound(false),
            Path::new("/System/Library/Sounds/Ping.aiff")
        );
        play_sound(Path::new("relative.aiff"), &fallback, |path| {
            assert_eq!(path, fallback);
            Ok(())
        })
        .unwrap();
        let temporary = tempfile::tempdir().unwrap();
        play_sound(&temporary.path().join("missing.aiff"), &fallback, |path| {
            assert_eq!(path, fallback);
            Ok(())
        })
        .unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut calls = 0;
        play_sound(file.path(), &fallback, |path| {
            calls += 1;
            if calls == 1 {
                assert_eq!(path, file.path());
                Err(Error::Backend)
            } else {
                assert_eq!(path, fallback);
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(calls, 2);
    }

    fn blocked_worker(notifications: &mut Notifications) -> mpsc::SyncSender<Result<(), Error>> {
        let (send, result) = mpsc::sync_channel(1);
        notifications.worker = Some(Worker {
            result,
            cancelled: notifications.cancel.clone(),
        });
        send
    }

    // Only used with a blocked mock worker, a held global permit, or unconfirmed
    // state. No test permits this policy to launch an actual player.
    fn audible(
        delivery: ToastDelivery,
    ) -> Policy<impl Fn(&SemanticNotification) -> Option<PathBuf>> {
        Policy {
            delivery,
            position: ToastPosition::BottomRight,
            delay_seconds: 0,
            sound: |_: &SemanticNotification| Some("/never-played.aiff".into()),
        }
    }

    #[test]
    fn attention_sound_waits_for_blocked_projection_and_times_out() {
        for confirm in [false, true] {
            let (mut notifications, bridge, mut live, now) = fixture();
            Arc::make_mut(live.snapshot.as_mut().unwrap()).agents[0].agent_status =
                AgentStatus::Working;
            *bridge.inbox.lock().unwrap() = live.clone();
            capture(&bridge, attention(), now);
            notifications.tick_with_policy(
                &bridge,
                &live,
                0,
                false,
                audible(ToastDelivery::Herdr),
                now,
            );
            assert_eq!(notifications.sounds.len(), 1);
            assert!(notifications.worker.is_none() && notifications.toasts.is_empty());
            if confirm {
                Arc::make_mut(live.snapshot.as_mut().unwrap()).agents[0].agent_status =
                    AgentStatus::Blocked;
                *bridge.inbox.lock().unwrap() = live.clone();
                let Claim::Granted(_permit) = notifications.external.lock().unwrap().claim(
                    "boot-v1",
                    &notifications.cancel,
                    &attention(),
                    now,
                    now,
                ) else {
                    panic!("owner must claim");
                };
                notifications.tick_with_policy(
                    &bridge,
                    &live,
                    0,
                    false,
                    audible(ToastDelivery::Herdr),
                    now + Duration::from_millis(50),
                );
                assert_eq!(notifications.toasts.len(), 1);
                assert_eq!(
                    notifications.sounds.len(),
                    1,
                    "confirmed sound waits only for the held backend permit"
                );
                assert!(!awaiting_snapshot(
                    &notifications.sounds[0],
                    live.snapshot.as_ref().unwrap(),
                    now + Duration::from_millis(50)
                ));
                assert!(eligible(
                    &notifications.sounds[0].notification,
                    live.snapshot.as_ref().unwrap(),
                    false
                ));
            } else {
                notifications.tick_with_policy(
                    &bridge,
                    &live,
                    0,
                    false,
                    audible(ToastDelivery::Herdr),
                    now + Duration::from_secs(1),
                );
                assert!(notifications.sounds.is_empty() && notifications.pending.is_empty());
                assert!(notifications.toasts.is_empty());
            }
            assert!(notifications.worker.is_none());
        }
    }

    #[test]
    fn same_broadcast_has_one_active_owner_even_when_inactive_window_ticks_first() {
        let (mut inactive, bridge_a, live_a, _) = fixture();
        let (mut active, bridge_b, live_b, now) = fixture();
        active.external = inactive.external.clone();
        inactive.prepare(&bridge_a, &live_a, 0, false, now);
        active.prepare(&bridge_b, &live_b, 0, true, now);
        let _blocked_a = blocked_worker(&mut inactive);
        let _blocked_b = blocked_worker(&mut active);
        capture(&bridge_a, event(), now);
        capture(&bridge_b, event(), now);
        inactive.tick_with_policy(
            &bridge_a,
            &live_a,
            0,
            false,
            audible(ToastDelivery::System),
            now,
        );
        active.tick_with_policy(
            &bridge_b,
            &live_b,
            0,
            true,
            audible(ToastDelivery::System),
            now,
        );
        assert!(inactive.sounds.is_empty() && inactive.pending.is_empty());
        assert_eq!(active.sounds.len(), 1);
        assert_eq!(active.pending.len(), usize::from(cfg!(target_os = "macos")));
        let mut registry = active.external.lock().unwrap();
        assert!(matches!(
            registry.claim("boot-v1", &inactive.cancel, &event(), now, now),
            Claim::Suppressed
        ));
        let Claim::Granted(permit) = registry.claim("boot-v1", &active.cancel, &event(), now, now)
        else {
            panic!("active owner");
        };
        assert!(matches!(
            registry.claim("boot-v1", &active.cancel, &event(), now, now),
            Claim::Busy
        ));
        drop(permit);
    }

    #[test]
    fn active_nonowner_suppresses_every_external_copy_but_toasts_stay_local() {
        let (mut owner, bridge_a, live_a, _) = fixture();
        let (mut active, bridge_b, live_b, now) = fixture();
        active.external = owner.external.clone();
        owner.prepare(&bridge_a, &live_a, 0, false, now);
        assert!(owner.external.lock().unwrap().allowed(
            "boot-v1",
            &owner.cancel,
            &event(),
            now,
            now
        ));
        active.prepare(&bridge_b, &live_b, 0, true, now);
        let _blocked_a = blocked_worker(&mut owner);
        let _blocked_b = blocked_worker(&mut active);
        for delivery in [ToastDelivery::System, ToastDelivery::Herdr] {
            capture(&bridge_a, attention(), now);
            capture(&bridge_b, attention(), now);
            owner.tick_with_policy(&bridge_a, &live_a, 0, false, audible(delivery), now);
            active.tick_with_policy(&bridge_b, &live_b, 0, true, audible(delivery), now);
            assert!(owner.sounds.is_empty() && active.sounds.is_empty());
            assert!(owner.pending.is_empty() && active.pending.is_empty());
            assert!(active.toasts.is_empty());
        }
        assert_eq!(
            owner.toasts.len(),
            1,
            "inactive window keeps its own in-app toast"
        );
        // Focus changes do not transfer ownership and replay a delayed copy.
        assert!(owner.external.lock().unwrap().allowed(
            "boot-v1",
            &owner.cancel,
            &event(),
            now,
            now
        ));
        assert!(!active.external.lock().unwrap().allowed(
            "boot-v1",
            &active.cancel,
            &event(),
            now,
            now
        ));
    }

    #[test]
    fn handoff_discards_backlog_and_cannot_overlap_old_backend() {
        let (mut first, bridge_a, live_a, _) = fixture();
        let (mut next, bridge_b, live_b, now) = fixture();
        next.external = first.external.clone();
        first.prepare(&bridge_a, &live_a, 0, false, now);
        next.prepare(&bridge_b, &live_b, 0, false, now);
        let Claim::Granted(old_permit) =
            first
                .external
                .lock()
                .unwrap()
                .claim("boot-v1", &first.cancel, &event(), now, now)
        else {
            panic!("initial owner");
        };
        first.reset();
        let mut registry = next.external.lock().unwrap();
        let handoff = now + Duration::from_millis(1);
        assert!(!registry.allowed("boot-v1", &next.cancel, &event(), now, handoff));
        assert!(matches!(
            registry.claim("boot-v1", &next.cancel, &event(), handoff, handoff),
            Claim::Busy
        ));
        drop(old_permit);
        assert!(matches!(
            registry.claim("boot-v1", &next.cancel, &event(), handoff, handoff),
            Claim::Granted(_)
        ));
        assert_eq!(registry.daemons[0].windows.len(), 1);
        drop(registry);
        let registry = next.external.clone();
        drop(next);
        assert!(registry.lock().unwrap().daemons.is_empty());
    }

    #[test]
    fn worker_rechecks_focus_in_other_window_before_external_effects() {
        let (mut owner, bridge_a, live_a, _) = fixture();
        let (mut active, bridge_b, live_b, now) = fixture();
        active.external = owner.external.clone();
        owner.prepare(&bridge_a, &live_a, 0, false, now);
        active.prepare(&bridge_b, &live_b, 0, false, now);
        let Claim::Granted(permit) =
            owner
                .external
                .lock()
                .unwrap()
                .claim("boot-v1", &owner.cancel, &attention(), now, now)
        else {
            panic!("initial owner");
        };
        let job = Delivery {
            external: owner.external.clone(),
            permit: Some(permit),
            received: now,
            notification: attention(),
            sound: None,
            system: true,
            cancel: owner.cancel.clone(),
            connection: bridge_a.notification_active.clone(),
            inbox: Arc::downgrade(&bridge_a.inbox),
            boot: "boot-v1".into(),
        };
        assert!(job.current());
        active.prepare(&bridge_b, &live_b, 0, true, now);
        assert!(!job.current());
        assert!(job.run().is_ok(), "suppressed before any OS process");
        assert_eq!(
            owner.external.lock().unwrap().daemons[0]
                .in_flight
                .strong_count(),
            0
        );
    }

    #[test]
    fn registry_is_bounded_and_drop_releases_registrations() {
        for distinct_boots in [false, true] {
            let registry: Arc<Mutex<ExternalRegistry>> = Arc::default();
            let limit = if distinct_boots {
                MAX_DAEMONS
            } else {
                MAX_WINDOWS
            };
            let mut windows = Vec::new();
            for index in 0..=limit {
                let (mut notifications, bridge, mut live, now) = fixture();
                notifications.external = registry.clone();
                if distinct_boots {
                    Arc::make_mut(live.snapshot.as_mut().unwrap()).boot_id =
                        format!("boot-{index}");
                    *bridge.inbox.lock().unwrap() = live.clone();
                }
                notifications.prepare(&bridge, &live, 0, false, now);
                windows.push((notifications, bridge));
            }
            {
                let registry = registry.lock().unwrap();
                assert!(registry.daemons.len() <= MAX_DAEMONS);
                assert_eq!(
                    registry
                        .daemons
                        .iter()
                        .map(|daemon| daemon.windows.len())
                        .sum::<usize>(),
                    limit
                );
                assert!(registry.overflow);
            }
            drop(windows);
            let registry = registry.lock().unwrap();
            assert!(registry.daemons.is_empty() && !registry.overflow);
        }
    }

    #[test]
    fn daemon_boots_have_independent_ownership_and_focus() {
        let (mut first, bridge_a, live_a, _) = fixture();
        let (mut other, bridge_b, mut live_b, now) = fixture();
        other.external = first.external.clone();
        Arc::make_mut(live_b.snapshot.as_mut().unwrap()).boot_id = "other-boot".into();
        *bridge_b.inbox.lock().unwrap() = live_b.clone();
        first.prepare(&bridge_a, &live_a, 0, false, now);
        other.prepare(&bridge_b, &live_b, 0, true, now);
        let mut registry = first.external.lock().unwrap();
        assert!(registry.allowed("boot-v1", &first.cancel, &attention(), now, now));
        assert!(!registry.allowed("other-boot", &other.cancel, &attention(), now, now));
        let Claim::Granted(_first) = registry.claim("boot-v1", &first.cancel, &event(), now, now)
        else {
            panic!("first boot");
        };
        let Claim::Granted(_other) =
            registry.claim("other-boot", &other.cancel, &event(), now, now)
        else {
            panic!("independent boot");
        };
    }
}
