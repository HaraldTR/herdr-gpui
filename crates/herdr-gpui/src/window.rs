//! The window entity: the state one GPUI window owns for one client of one
//! daemon, and the poll task that folds worker results into it. Behavior is
//! split by responsibility across the submodules below; the fields live here
//! because every one of them describes this window's own presentation state.

mod commands;
mod input;
mod lifecycle;
mod render;

#[cfg(all(test, feature = "integration-test"))]
mod resize_tests;
#[cfg(test)]
mod tests;

#[cfg(feature = "integration-test")]
use crate::smoke;
use crate::{
    WINDOW_TITLE, avatars, config, endpoint, git, log_window, menu,
    navigation::OwnedNavigationTarget, preferences, presentation::Presentation, sidebar,
    state::LiveState, terminal::WheelAccumulator, terminal_painter, updater,
};
use gpui::{prelude::*, *};
use herdr_client::{ConnectOptions, ConnectTarget};
#[cfg(feature = "integration-test")]
use std::sync::Arc;
use std::time::Duration;

pub(crate) struct HerdrWindow {
    pub(crate) updater: updater::Updater,
    pub(crate) update_preview: Option<updater::State>,
    pub(crate) config: config::Config,
    pub(crate) theme: config::Theme,
    pub(crate) config_load: Option<Task<()>>,
    pub(crate) settings: crate::settings_panel::SettingsPanel,
    pub(crate) integrations: crate::integrations::Integrations,
    pub(crate) notifications: crate::notifications::Notifications,
    pub(crate) endpoints: Vec<endpoint::Endpoint>,
    pub(crate) selected_endpoint: usize,
    pub(crate) selection_epoch: u64,
    pub(crate) catalog: endpoint::Catalog,
    pub(crate) activation_deadline: Option<std::time::Instant>,
    pub(crate) pending_navigation: Option<OwnedNavigationTarget>,
    pub(crate) pending_releases: Vec<endpoint::Release>,
    pub(crate) selected_generation: u64,
    pub(crate) live: LiveState,
    pub(crate) focus: FocusHandle,
    pub(crate) options: ConnectOptions,
    pub(crate) last_queued_options: Option<ConnectOptions>,
    pub(crate) active: bool,
    pub(crate) sent_focus: Option<bool>,
    pub(crate) bounds: Bounds<Pixels>,
    /// Last title pushed to the OS, so the window is renamed only when it changes.
    pub(crate) title: String,
    pub(crate) cell_width: f32,
    pub(crate) hovered_terminal_link: bool,
    pub(crate) pressed_terminal_link: Option<(String, Point<Pixels>)>,
    /// The frame on screen, kept across the gap between two projections.
    pub(crate) presentation: Presentation,
    pub(crate) painter: std::rc::Rc<std::cell::RefCell<terminal_painter::TerminalPainter>>,
    pub(crate) marked: String,
    /// The sidebar row the pointer is resting on, waiting to open its menu.
    pub(crate) hover: Option<sidebar::HoverRest>,
    /// The menu that resting opened, which the pointer closes by leaving it.
    pub(crate) hover_menu: Option<sidebar::HoverMenu>,
    pub(crate) local_error: Option<String>,
    pub(crate) menu: menu::MenuState,
    /// A `worktree.remove` queued after its dialog closed.
    pub(crate) removal: Option<menu::Removal>,
    pub(crate) git: git::Git,
    pub(crate) install_warning_shown: bool,
    pub(crate) collapsed_repos: std::collections::HashSet<String>,
    pub(crate) sidebar_visible: bool,
    pub(crate) wheel: WheelAccumulator,
    pub(crate) sidebar_width: Option<f32>,
    pub(crate) sidebar_drag: Option<(f32, f32)>,
    pub(crate) sidebar_preferences: Option<preferences::Preferences>,
    pub(crate) sidebar_modified: bool,
    pub(crate) agent_sort: preferences::AgentSort,
    /// Keeps a toggle made before the stored chrome arrives from being undone.
    pub(crate) agent_sort_modified: bool,
    pub(crate) avatars: Option<avatars::Avatars>,
    #[cfg(feature = "integration-test")]
    pub(crate) input_probe: smoke::InputProbe,
    /// Spaces and agents lists, in that order.
    pub(crate) sidebar_scroll: [ScrollHandle; 2],
    /// The row each list has scrolled into view, so a new selection is revealed
    /// while the user's own scrolling of an unchanged one is left alone.
    pub(crate) sidebar_revealed: [std::cell::Cell<Option<usize>>; 2],
    pub(crate) _poll: Task<()>,
    pub(crate) _activation: Subscription,
    pub(crate) _appearance: Subscription,
}

impl HerdrWindow {
    fn prepare_notifications(&mut self, cx: &mut Context<Self>) {
        let endpoint = &self.endpoints[self.selected_endpoint];
        if let Ok(live) = endpoint.connection.inbox.try_lock()
            && self.notifications.prepare(
                &endpoint.connection,
                &live,
                self.selection_epoch,
                self.active,
                std::time::Instant::now(),
            )
        {
            cx.notify();
        }
    }

    pub(crate) fn new(
        target: ConnectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
        #[cfg(feature = "integration-test")] sidebar_test: bool,
    ) -> Self {
        #[cfg(feature = "integration-test")]
        let target = if sidebar_test {
            ConnectTarget::Socket("/unused-sidebar-fixture.sock".into())
        } else {
            target
        };
        let focus = cx.focus_handle();
        window.focus(&focus);
        let timer = cx.background_executor().clone();
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                timer.timer(Duration::from_millis(16)).await;
                if this
                    .update_in(cx, |this, window, cx| {
                        if this.updater.poll() {
                            match this.updater.commit_restart() {
                                Ok(true) => {
                                    cx.quit();
                                    return;
                                }
                                Ok(false) => {}
                                Err(error) => eprintln!("App update restart failed: {error}"),
                            }
                            cx.notify();
                        }
                        if this.avatars.as_mut().is_some_and(|avatars| avatars.poll()) {
                            cx.notify();
                        }
                        if let Some(chrome) =
                            this.sidebar_preferences.as_mut().and_then(|p| p.loaded())
                        {
                            if !this.sidebar_modified {
                                this.sidebar_width = chrome.sidebar_width;
                            }
                            if !this.agent_sort_modified {
                                this.agent_sort = chrome.agent_sort;
                            }
                            cx.notify();
                        }
                        let old_pane = this
                            .live
                            .snapshot
                            .as_ref()
                            .and_then(|s| s.focused_pane_id.clone());
                        this.poll_endpoints(cx);
                        this.poll_integrations(cx);
                        // Register every main window before any external effect,
                        // even when another window's config is still loading.
                        this.prepare_notifications(cx);
                        for handle in cx.windows() {
                            if handle != window.window_handle()
                                && let Some(handle) = handle.downcast::<HerdrWindow>()
                            {
                                let _ = handle.update(cx, |other, _, cx| {
                                    other.prepare_notifications(cx);
                                });
                            }
                        }
                        if this.settings.task.is_none() {
                            let mut reload = false;
                            for endpoint in &this.endpoints {
                                reload |= endpoint.connection.take_settings_reload();
                            }
                            if reload {
                                this.load_shared_settings(cx);
                            }
                        }
                        for (index, endpoint) in this.endpoints.iter().enumerate() {
                            if index != this.selected_endpoint {
                                drop(endpoint.connection.take_notifications());
                            }
                        }
                        let endpoint = &this.endpoints[this.selected_endpoint];
                        if let Some(settings) = &this.settings.shared {
                            if this.notifications.tick(
                                &endpoint.connection,
                                &endpoint.live,
                                this.selection_epoch,
                                this.active,
                                settings,
                                std::time::Instant::now(),
                            ) {
                                cx.notify();
                            }
                        } else {
                            drop(endpoint.connection.take_notifications());
                        }
                        this.update_workspace_dialog(window, cx);
                        this.poll_worktree_source(cx);
                        this.poll_hover_menu(std::time::Instant::now(), window, cx);
                        this.poll_tab_rename(window, cx);
                        if old_pane
                            != this
                                .live
                                .snapshot
                                .as_ref()
                                .and_then(|s| s.focused_pane_id.clone())
                        {
                            this.marked.clear();
                        }
                        if this.update_workspace_pr() {
                            cx.notify();
                        }
                        if this.update_git() {
                            cx.notify();
                        }
                        if this.live.missing_installation && !this.install_warning_shown {
                            this.install_warning_shown = true;
                            this.show_install_modal(window, cx);
                        }
                        this.resize();
                        this.report_focus();
                        this.sync_window_title(window);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut this = Self {
            updater: updater::Updater::default(),
            update_preview: None,
            config: config::Config::default(),
            theme: config::Theme::default(),
            config_load: None,
            settings: Default::default(),
            integrations: Default::default(),
            notifications: Default::default(),
            catalog: endpoint::Catalog::new(&target),
            endpoints: vec![endpoint::Endpoint::new(
                endpoint::LOCAL.into(),
                "Local".into(),
                target,
                true,
            )],
            selected_endpoint: 0,
            selection_epoch: 0,
            activation_deadline: None,
            pending_navigation: None,
            pending_releases: Vec::new(),
            selected_generation: 0,
            live: LiveState::default(),
            focus,
            options: ConnectOptions::default(),
            last_queued_options: None,
            active: window.is_window_active(),
            sent_focus: None,
            bounds: Bounds::default(),
            title: WINDOW_TITLE.to_owned(),
            cell_width: 9.,
            hovered_terminal_link: false,
            pressed_terminal_link: None,
            presentation: Default::default(),
            painter: Default::default(),
            marked: String::new(),
            hover: None,
            hover_menu: None,
            local_error: None,
            menu: menu::MenuState::new(cx),
            removal: None,
            git: git::Git::default(),
            install_warning_shown: false,
            collapsed_repos: Default::default(),
            sidebar_visible: true,
            wheel: WheelAccumulator::default(),
            sidebar_width: None,
            sidebar_drag: None,
            sidebar_preferences: None,
            sidebar_modified: false,
            agent_sort: preferences::AgentSort::default(),
            agent_sort_modified: false,
            avatars: None,
            #[cfg(feature = "integration-test")]
            input_probe: smoke::InputProbe::default(),
            sidebar_scroll: Default::default(),
            sidebar_revealed: Default::default(),
            _poll: poll,
            _activation: cx.observe_window_activation(window, |this, window, cx| {
                this.active = window.is_window_active();
                this.prepare_notifications(cx);
                this.report_focus();
                cx.notify();
            }),
            _appearance: cx.observe_window_appearance(window, |this, _, cx| {
                this.apply_shared_theme(cx);
                cx.notify();
            }),
        };
        #[cfg(feature = "integration-test")]
        if sidebar_test {
            this._poll = Task::ready(());
            this.live.snapshot = Some(Arc::new(sidebar::layout_tests::snapshot(40)));
            this.endpoints[0].live = this.live.clone();
            if let Ok(mut inbox) = this.endpoints[0].connection.inbox.lock() {
                *inbox = this.live.clone();
            }
            return this;
        }
        this.sidebar_preferences = this.endpoints[0]
            .connection
            .target
            .socket_path()
            .ok()
            .map(|path| preferences::Preferences::new(&path));
        this.avatars = Some(avatars::Avatars::new());
        this.reconnect();
        log_window::set_appearance(&this.config, &this.theme, cx);
        this.load_gui_config(cx);
        this.load_shared_settings(cx);
        this
    }
}
