//! The collapsed sidebar's compact rail, as Herdr's default
//! `ui.sidebar_collapsed_mode = "compact"` draws it: per host a header mark,
//! then a status mark per workspace and per agent. Every mark navigates where
//! its expanded row would, and its tooltip names what the narrow column cannot.

use super::{
    agent_name,
    agents::{Indicators, agent_place, status_indicator, status_text},
    cell::RowState,
    label_text,
    layout::{self, SidebarLook},
    line_height, sorted_agents, visible_workspace_entries,
    workspaces::workspace_label,
};
use crate::{
    Command, HerdrWindow, NavigationTarget,
    config::{FontConfig, Theme},
    fonts::StyledFont,
    herdr_settings::SidebarCollapsedMode,
};
use gpui::{prelude::*, *};
use herdr_client::protocol::AgentStatus;

/// Wide enough for a two-digit workspace number beside a symbol indicator.
pub(crate) const RAIL_WIDTH: f32 = 48.;
const HOST_MARK: f32 = 22.;
const AGENT_ICON: f32 = 14.;
const MARK_GAP: f32 = 4.;

/// What the sidebar column shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SidebarMode {
    Expanded,
    Rail,
    Hidden,
}

impl SidebarMode {
    /// The column's width, under the same terminal reserve the expanded
    /// sidebar keeps, so a narrow window never loses its terminal to it.
    pub(crate) fn width(self, preferred: Option<f32>, window_width: f32) -> Option<f32> {
        match self {
            Self::Expanded => Some(super::sidebar_width(preferred, window_width)),
            // A window too narrow for any rail gets none rather than a border.
            Self::Rail => Some(RAIL_WIDTH.min(window_width - 240.)).filter(|width| *width > 0.),
            Self::Hidden => None,
        }
    }
}

impl HerdrWindow {
    /// Collapsing follows the local daemon's `ui.sidebar_collapsed_mode`,
    /// also while an SSH host is shown, as Herdr keeps presentation local.
    /// Before shared settings load, Herdr's default rail applies.
    pub(crate) fn sidebar_mode(&self) -> SidebarMode {
        if self.sidebar_visible {
            return SidebarMode::Expanded;
        }
        match self
            .settings
            .shared
            .as_ref()
            .map_or(SidebarCollapsedMode::Compact, |shared| {
                shared.sidebar_collapsed_mode
            }) {
            SidebarCollapsedMode::Compact => SidebarMode::Rail,
            SidebarCollapsedMode::Hidden => SidebarMode::Hidden,
        }
    }

    pub(crate) fn toggle_sidebar(&mut self) {
        self.sidebar_visible = !self.sidebar_visible;
        self.sidebar_start_pending = false;
    }

    /// Applies `ui.sidebar_start_collapsed` once, from the first shared
    /// settings to load. Later reloads, and a toggle made before the first
    /// load arrives, leave the user's choice alone, as Herdr reads the setting
    /// at startup only.
    pub(crate) fn apply_sidebar_start(&mut self) {
        if !std::mem::take(&mut self.sidebar_start_pending) {
            return;
        }
        if self
            .settings
            .shared
            .as_ref()
            .is_some_and(|shared| shared.sidebar_start_collapsed)
        {
            self.sidebar_visible = false;
        }
    }

    pub(crate) fn render_rail(
        &self,
        indicators: Indicators,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let width = SidebarMode::Rail
            .width(None, f32::from(window.viewport_size().width))
            .unwrap_or(0.);
        let look = layout::for_mode(self.config.layout.mode);
        let font = &self.config.sidebar;
        let theme = &self.theme;
        let hint = Hint::new(theme);
        let multi = self.endpoints.len() > 1;
        let split = self.sidebar_split.unwrap_or(0.5).clamp(0.1, 0.9);
        let mut spaces = div()
            .id("rail-spaces")
            .debug_selector(|| "rail-spaces".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        let mut agents = div()
            .id("rail-agents")
            .debug_selector(|| "rail-agents".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        for (endpoint_index, endpoint) in self.endpoints.iter().enumerate() {
            if !self.device_visible(&endpoint.id) {
                continue;
            }
            let selected = endpoint_index == self.selected_endpoint;
            let endpoint_id = endpoint.id.clone();
            let initial = host_initial(&endpoint.label);
            if multi {
                let select_id = endpoint_id.clone();
                let menu_id = endpoint_id.clone();
                let connected = endpoint.live.status.is_connected();
                let text = if endpoint.collapsed {
                    format!("{} (collapsed)", endpoint.label)
                } else {
                    endpoint.label.clone()
                };
                spaces = spaces.child(
                    rail_row(
                        &format!("rail-host-{endpoint_id}"),
                        RowState {
                            selected,
                            ..RowState::default()
                        },
                        font,
                        look,
                        theme,
                    )
                    .child(
                        div()
                            .relative()
                            .size(px(HOST_MARK))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(crate::config::corners::SMALL))
                            .border_1()
                            .border_color(rgb(theme.active))
                            .text_color(rgb(if endpoint.enabled {
                                theme.foreground
                            } else {
                                theme.muted
                            }))
                            .child(label_text(&initial))
                            .child(
                                div()
                                    .debug_selector(|| format!("rail-host-status-{endpoint_id}"))
                                    .absolute()
                                    .right(px(-2.))
                                    .bottom(px(-2.))
                                    .size(px(6.))
                                    .rounded_full()
                                    .bg(rgb(if connected {
                                        crate::menu::online(theme)
                                    } else {
                                        theme.muted
                                    })),
                            ),
                    )
                    .id(SharedString::from(format!("rail-host-{endpoint_id}")))
                    .debug_selector(|| format!("rail-host-{endpoint_id}"))
                    .cursor_pointer()
                    .tooltip(hint.with(text))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_host_menu(&menu_id, event.position, window, cx);
                            this.menu.opening_right_click =
                                this.menu.page == Some(crate::menu::Page::Host);
                        }),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_endpoint(&select_id, cx);
                        window.focus(&this.focus, cx);
                    })),
                );
            }
            if multi && endpoint.collapsed {
                continue;
            }
            let live = if selected { &self.live } else { &endpoint.live };
            let Some(snapshot) = &live.snapshot else {
                continue;
            };
            let collapsed_repos = if endpoint_index == 0 {
                &self.collapsed_repos
            } else {
                &endpoint.collapsed_repos
            };
            let host = multi.then_some(endpoint.label.as_str());
            for (index, indented, _) in
                visible_workspace_entries(&snapshot.workspaces, collapsed_repos)
            {
                let workspace = &snapshot.workspaces[index];
                let id = workspace.workspace_id.clone();
                let key = format!("rail-workspace-{endpoint_id}-{id}");
                let navigate_endpoint = endpoint_id.clone();
                let context_endpoint = endpoint_id.clone();
                let context_id = id.clone();
                let focused = selected && workspace.focused;
                spaces = spaces.child(
                    rail_row(
                        &key,
                        RowState {
                            selected: focused,
                            ..RowState::default()
                        },
                        font,
                        look,
                        theme,
                    )
                    .gap(px(MARK_GAP))
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(if focused {
                                theme.foreground
                            } else {
                                theme.muted
                            }))
                            .child(label_text(&workspace.number.to_string())),
                    )
                    .child(indicator(workspace.agent_status, font, indicators))
                    .id(SharedString::from(key.clone()))
                    .debug_selector(|| key)
                    .cursor_pointer()
                    .tooltip(hint.with(describe(
                        host,
                        workspace_label(workspace, indented),
                        None,
                        workspace.agent_status,
                    )))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            if this.navigate_endpoint(
                                &context_endpoint,
                                NavigationTarget::Workspace(&context_id),
                                cx,
                            ) {
                                this.open_workspace_menu(&context_id, event.position, window, cx);
                                this.menu.opening_right_click =
                                    this.menu.page == Some(crate::menu::Page::Workspace);
                            }
                        }),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate_endpoint(
                            &navigate_endpoint,
                            NavigationTarget::Workspace(&id),
                            cx,
                        );
                        window.focus(&this.focus, cx);
                    })),
                );
            }
            if !self.config.show_agents {
                continue;
            }
            for agent in sorted_agents(snapshot, self.agent_sort) {
                let id = agent.pane_id.clone();
                let key = format!("rail-agent-{endpoint_id}-{id}");
                let navigate_endpoint = endpoint_id.clone();
                let focused = selected && agent.focused;
                let place = agent_place(agent, snapshot);
                let place = place.map(|(workspace, tab)| match tab {
                    Some(tab) => format!("{workspace} / {tab}"),
                    None => workspace.to_owned(),
                });
                agents = agents.child(
                    rail_row(
                        &key,
                        RowState {
                            selected: focused,
                            ..RowState::default()
                        },
                        font,
                        look,
                        theme,
                    )
                    .gap(px(MARK_GAP))
                    // Herdr's rail prefixes each agent with its machine's
                    // initial once more than one host shares the list.
                    .when(multi, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_color(rgb(theme.muted))
                                .child(label_text(&initial)),
                        )
                    })
                    .child(
                        svg()
                            .debug_selector(|| format!("rail-agent-icon-{id}"))
                            .path(
                                crate::icons::AgentIcon::from_identity(agent.agent.as_deref())
                                    .path(),
                            )
                            .size(px(AGENT_ICON.min(font.size + 1.)))
                            .flex_none()
                            .text_color(rgb(if focused {
                                theme.foreground
                            } else {
                                theme.muted
                            })),
                    )
                    .child(indicator(agent.agent_status, font, indicators))
                    .id(SharedString::from(key.clone()))
                    .debug_selector(|| key)
                    .cursor_pointer()
                    .tooltip(hint.with(describe(
                        host,
                        agent_name(agent),
                        place.as_deref(),
                        agent.agent_status,
                    )))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate_endpoint(&navigate_endpoint, NavigationTarget::Pane(&id), cx);
                        window.focus(&this.focus, cx);
                    })),
                );
            }
        }
        div()
            .id("sidebar-rail")
            .debug_selector(|| "sidebar-rail".into())
            .relative()
            .w(px(width))
            .flex_none()
            .h_full()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .text_font(font)
            .text_size(px(font.size))
            .line_height(px(line_height(font)))
            .text_color(rgb(theme.foreground))
            .bg(rgb(theme.surface))
            .border_r_1()
            .border_color(rgb(theme.active))
            .child(
                rail_button("rail-expand", font, look, theme)
                    .tooltip(hint.with("Expand sidebar"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.command(Command::ToggleSidebar, window, cx);
                    }))
                    .child(crate::titlebar::sidebar_glyph(false, theme)),
            )
            .child(
                div()
                    .debug_selector(|| "rail-spaces-section".into())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .map(|mut section| {
                        section.style().flex_grow =
                            Some(if self.config.show_agents { split } else { 1. });
                        section
                    })
                    .min_h_0()
                    .overflow_hidden()
                    .child(spaces),
            )
            .when(self.config.show_agents, |rail| {
                rail.child(div().flex_none().h(px(1.)).bg(rgb(theme.active)))
                    .child(
                        div()
                            .debug_selector(|| "rail-agents-section".into())
                            .flex()
                            .flex_col()
                            .flex_1()
                            .map(|mut section| {
                                section.style().flex_grow = Some(1. - split);
                                section
                            })
                            .min_h_0()
                            .overflow_hidden()
                            .child(agents),
                    )
            })
            .child(
                rail_button("rail-new-workspace", font, look, theme)
                    .text_color(rgb(theme.muted))
                    .hover(|s| s.text_color(rgb(theme.foreground)))
                    .tooltip(hint.with("New workspace"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.command(Command::Workspace, window, cx);
                    }))
                    .child(label_text("+")),
            )
    }
}

/// A rail row: centered marks over the active layout's highlight, so the rail
/// keeps the expanded rows' look in every sidebar layout.
fn rail_row(
    key: &str,
    state: RowState,
    font: &FontConfig,
    look: SidebarLook,
    theme: &Theme,
) -> Div {
    look.mark(
        div()
            .relative()
            .flex_none()
            .h(px(look.row_height(line_height(font).max(HOST_MARK))))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden(),
        key,
        state,
        theme,
    )
}

fn rail_button(
    id: &'static str,
    font: &FontConfig,
    look: SidebarLook,
    theme: &Theme,
) -> Stateful<Div> {
    rail_row(id, RowState::default(), font, look, theme)
        .id(id)
        .debug_selector(move || id.into())
        .cursor_pointer()
}

/// The indicator centered on its line, whichever style draws it.
fn indicator(status: AgentStatus, font: &FontConfig, indicators: Indicators) -> Div {
    div()
        .flex_none()
        .h(px(line_height(font)))
        .child(status_indicator(status, font, indicators))
}

/// The host's initial, as Herdr marks a machine in its rail.
pub(super) fn host_initial(label: &str) -> String {
    label
        .trim()
        .chars()
        .next()
        .map_or_else(|| "?".into(), |initial| initial.to_uppercase().collect())
}

/// A rail mark's tooltip: what the expanded row would have spelled out.
pub(super) fn describe(
    host: Option<&str>,
    name: &str,
    place: Option<&str>,
    status: AgentStatus,
) -> SharedString {
    let mut text = String::new();
    if let Some(host) = host {
        text.push_str(host);
        text.push_str(": ");
    }
    text.push_str(name);
    if let Some(place) = place {
        text.push_str(" in ");
        text.push_str(place);
    }
    if status != AgentStatus::Unknown {
        text.push_str(" (");
        text.push_str(status_text(status));
        text.push(')');
    }
    text.into()
}

#[derive(Clone, Copy)]
struct Hint {
    foreground: u32,
    surface: u32,
}

impl Hint {
    fn new(theme: &Theme) -> Self {
        Self {
            foreground: theme.foreground,
            surface: theme.surface,
        }
    }

    fn with(
        self,
        text: impl Into<SharedString>,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let text = text.into();
        move |_, cx| {
            cx.new(|_| crate::usage::Hint {
                text: text.clone(),
                foreground: self.foreground,
                surface: self.surface,
            })
            .into()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{
        config::LayoutMode,
        herdr_settings::Settings,
        sidebar::layout_tests::{SidebarFixture, fixture_window, full_draw, snapshot},
    };
    use core::prelude::v1::test;
    use gpui::{Modifiers, TestAppContext, VisualTestContext, size};
    use herdr_client::ConnectTarget;
    use std::sync::Arc;

    fn rail_window(
        cx: &mut TestAppContext,
        hosts: usize,
    ) -> (Entity<HerdrWindow>, &mut VisualTestContext) {
        let (fixture, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                let mut view = fixture_window(window, cx);
                view.live.snapshot = Some(Arc::new(snapshot(6)));
                view.endpoints[0].live = view.live.clone();
                for host in 1..hosts {
                    let mut remote = crate::endpoint::Endpoint::new(
                        format!("ssh:host{host}"),
                        format!("build{host}"),
                        ConnectTarget::Ssh {
                            target: "unused".into(),
                            session: "default".into(),
                        },
                        true,
                    );
                    remote.live.snapshot = Some(Arc::new(snapshot(2)));
                    view.endpoints.push(remote);
                }
                view.sidebar_visible = false;
                view
            });
            cx.observe(&view, |_, _, cx| cx.notify()).detach();
            SidebarFixture(view)
        });
        let view = cx.update(|_, cx| fixture.read(cx).0.clone());
        cx.simulate_resize(size(px(900.), px(600.)));
        cx.run_until_parked();
        cx.update(|window, cx| full_draw(window, cx).clear(cx));
        (view, cx)
    }

    fn set_shared(view: &Entity<HerdrWindow>, text: &str, cx: &mut VisualTestContext) {
        view.update(cx, |view, cx| {
            view.settings.shared = Some(Settings::parse_text(text).unwrap());
            cx.notify();
        });
        cx.update(|window, cx| full_draw(window, cx).clear(cx));
    }

    #[gpui::test]
    fn collapsed_sidebar_is_a_rail_in_every_layout_that_navigates(cx: &mut TestAppContext) {
        let (view, cx) = rail_window(cx, 1);
        for mode in LayoutMode::ALL {
            view.update(cx, |view, cx| {
                view.config.layout.mode = mode;
                view.pending_navigation = None;
                cx.notify();
            });
            cx.update(|window, cx| full_draw(window, cx).clear(cx));
            assert!(cx.debug_bounds("sidebar").is_none(), "{mode:?}");
            let rail = cx.debug_bounds("sidebar-rail").unwrap();
            assert_eq!(rail.size.width, px(RAIL_WIDTH), "{mode:?}");
            let terminal = cx.debug_bounds("terminal").unwrap();
            assert!(terminal.left() >= rail.right(), "{mode:?}");
            // Every workspace, and every agent, keeps a mark inside the rail.
            for selector in [
                "rail-expand",
                "rail-workspace-local-w0",
                "rail-workspace-local-w5",
                "rail-agent-local-p0",
                "rail-agent-local-p1",
                "rail-new-workspace",
            ] {
                let mark = cx.debug_bounds(selector).unwrap_or_else(|| {
                    panic!("{mode:?}: missing {selector}");
                });
                assert!(
                    mark.left() >= rail.left() && mark.right() <= rail.right(),
                    "{mode:?} {selector}: {mark:?} outside {rail:?}"
                );
            }
            // No host header for a single host.
            assert!(cx.debug_bounds("rail-host-local").is_none());
            let workspace = cx.debug_bounds("rail-workspace-local-w3").unwrap();
            cx.simulate_click(workspace.center(), Modifiers::default());
            view.read_with(cx, |view, _| {
                assert_eq!(
                    view.pending_navigation,
                    Some(NavigationTarget::Workspace("w3".into())),
                    "{mode:?}"
                );
            });
            cx.update(|window, cx| full_draw(window, cx).clear(cx));
            let agent = cx.debug_bounds("rail-agent-local-p1").unwrap();
            cx.simulate_click(agent.center(), Modifiers::default());
            view.read_with(cx, |view, _| {
                assert_eq!(
                    view.pending_navigation,
                    Some(NavigationTarget::Pane("p1".into())),
                    "{mode:?}"
                );
                assert!(!view.sidebar_visible);
            });
        }
        // The rail's own control expands the sidebar again.
        let expand = cx.debug_bounds("rail-expand").unwrap();
        cx.simulate_click(expand.center(), Modifiers::default());
        cx.update(|window, cx| full_draw(window, cx).clear(cx));
        assert!(view.read_with(cx, |view, _| view.sidebar_visible));
        assert!(cx.debug_bounds("sidebar").is_some());
        assert!(cx.debug_bounds("sidebar-rail").is_none());
    }

    #[gpui::test]
    fn rail_follows_shared_collapsed_mode(cx: &mut TestAppContext) {
        let (view, cx) = rail_window(cx, 1);
        let rail_right = cx.debug_bounds("sidebar-rail").unwrap().right();
        set_shared(&view, "[ui]\nsidebar_collapsed_mode = 'hidden'\n", cx);
        assert!(cx.debug_bounds("sidebar-rail").is_none());
        assert!(cx.debug_bounds("sidebar").is_none());
        // Hidden gives the rail's room and the sidebar gap back to the terminal.
        let terminal = cx.debug_bounds("terminal").unwrap();
        assert!(terminal.left() < rail_right);
        set_shared(&view, "[ui]\nsidebar_collapsed_mode = 'compact'\n", cx);
        assert!(cx.debug_bounds("sidebar-rail").is_some());
        // Expanding ignores the collapsed mode.
        view.update(cx, |view, cx| {
            view.toggle_sidebar();
            cx.notify();
        });
        cx.update(|window, cx| full_draw(window, cx).clear(cx));
        assert!(cx.debug_bounds("sidebar").is_some());
    }

    #[gpui::test]
    fn rail_shows_a_header_per_host_and_skips_collapsed_hosts(cx: &mut TestAppContext) {
        let (view, cx) = rail_window(cx, 3);
        let rail = cx.debug_bounds("sidebar-rail").unwrap();
        for selector in [
            "rail-host-local",
            "rail-host-ssh:host1",
            "rail-host-ssh:host2",
            "rail-host-status-ssh:host1",
            "rail-workspace-local-w0",
            "rail-workspace-ssh:host1-w0",
            "rail-workspace-ssh:host2-w1",
            "rail-agent-ssh:host2-p0",
        ] {
            let mark = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing {selector}"));
            assert!(mark.right() <= rail.right(), "{selector}");
        }
        // Hosts list in order, each above its own workspaces.
        let first = cx.debug_bounds("rail-host-ssh:host1").unwrap().top();
        let second = cx.debug_bounds("rail-host-ssh:host2").unwrap().top();
        let workspace = cx.debug_bounds("rail-workspace-ssh:host1-w1").unwrap();
        assert!(first < workspace.top() && workspace.top() < second);
        view.update(cx, |view, cx| {
            view.endpoints[1].collapsed = true;
            cx.notify();
        });
        cx.update(|window, cx| full_draw(window, cx).clear(cx));
        assert!(cx.debug_bounds("rail-host-ssh:host1").is_some());
        assert!(cx.debug_bounds("rail-workspace-ssh:host1-w0").is_none());
        assert!(cx.debug_bounds("rail-agent-ssh:host1-p0").is_none());
        assert!(cx.debug_bounds("rail-workspace-ssh:host2-w0").is_some());
        // A host header selects its host, as the expanded header does, even
        // while its rows are folded away.
        let host = cx.debug_bounds("rail-host-ssh:host1").unwrap();
        cx.simulate_click(host.center(), Modifiers::default());
        view.read_with(cx, |view, _| {
            assert_eq!(view.endpoints[view.selected_endpoint].id, "ssh:host1");
        });
    }

    #[gpui::test]
    fn narrow_windows_keep_the_terminal_reserve(cx: &mut TestAppContext) {
        let (_, cx) = rail_window(cx, 2);
        for (width, rail) in [(900., RAIL_WIDTH), (270., 30.), (200., 0.)] {
            cx.simulate_resize(size(px(width), px(400.)));
            cx.update(|window, cx| full_draw(window, cx).clear(cx));
            assert_eq!(
                cx.debug_bounds("sidebar-rail")
                    .map_or(px(0.), |bounds| bounds.size.width),
                px(rail),
                "{width}"
            );
        }
    }

    #[gpui::test]
    fn start_collapsed_applies_once_and_yields_to_an_earlier_toggle(cx: &mut TestAppContext) {
        let (view, cx) = rail_window(cx, 1);
        let collapsed = Settings::parse_text("[ui]\nsidebar_start_collapsed = true\n").unwrap();
        view.update(cx, |view, _| {
            // A fresh window stays expanded until the first shared load.
            view.sidebar_visible = true;
            view.sidebar_start_pending = true;
            view.settings.shared = Some(Settings::parse_text("").unwrap());
            view.apply_sidebar_start();
            assert!(view.sidebar_visible);
            // Only the first load counts; a later edit waits for a restart.
            view.settings.shared = Some(collapsed.clone());
            view.apply_sidebar_start();
            assert!(view.sidebar_visible);

            view.sidebar_start_pending = true;
            view.apply_sidebar_start();
            assert!(!view.sidebar_visible);
            // Expanding afterwards sticks across reloads.
            view.toggle_sidebar();
            view.apply_sidebar_start();
            assert!(view.sidebar_visible);

            // Toggling before the settings arrive keeps the user's choice.
            view.sidebar_start_pending = true;
            view.toggle_sidebar();
            view.toggle_sidebar();
            view.apply_sidebar_start();
            assert!(view.sidebar_visible);
        });
    }

    #[test]
    fn tooltips_name_host_place_and_status() {
        assert_eq!(
            describe(
                Some("build"),
                "review",
                Some("herdr / tab 2"),
                AgentStatus::Blocked
            ),
            "build: review in herdr / tab 2 (blocked)"
        );
        assert_eq!(describe(None, "herdr", None, AgentStatus::Unknown), "herdr");
        assert_eq!(host_initial("  ssh box"), "S");
        assert_eq!(host_initial(""), "?");
        assert_eq!(host_initial("éclair"), "É");
    }
}
