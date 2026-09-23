//! Laying out the sidebar: the two lists, their headings, and the drag handle
//! that resizes the panel. Render works from prepared state and the bounded
//! caches only.

use super::{
    ARROW_RESERVE, HOST_ARROW_WIDTH, HOST_GAP, SidebarDrag,
    agents::{Indicators, agent_labels},
    agents_sort, label_text,
    layout::{self, SidebarLayout},
    line_height,
    row::first_text,
    row::{RowIcon, RowKind, RowTree, row},
    sidebar_width, sorted_agents, visible_workspace_entries,
    workspaces::{workspace_badge, workspace_label},
};
use crate::{
    Command, HerdrWindow, NavigationTarget,
    config::{FontConfig, Theme},
    fonts::StyledFont,
};
use gpui::{prelude::*, *};

impl HerdrWindow {
    pub(crate) fn render_sidebar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let width = sidebar_width(self.sidebar_width, f32::from(window.viewport_size().width));
        let split = self.sidebar_split.unwrap_or(0.5).clamp(0.1, 0.9);
        let layout = layout::for_mode(self.config.layout.mode);
        let padding = layout.padding();
        let gap = layout.gap();
        // Hide secondary status in narrow windows, retaining useful host label space.
        let show_host_status = width >= 200.;
        let host_label_width = (width
            - 1.
            - 2. * padding
            - HOST_ARROW_WIDTH
            - HOST_GAP
            - if show_host_status { HOST_GAP + 67. } else { 0. })
        .max(0.);
        let view = cx.entity().downgrade();
        let font = &self.config.sidebar;
        let theme = &self.theme;
        let indicators = Indicators::new(
            self.settings.shared.as_ref(),
            matches!(
                cx.window_appearance(),
                WindowAppearance::Light | WindowAppearance::VibrantLight
            ),
        );
        let mut spaces = div()
            .id("spaces-scroll")
            .debug_selector(|| "spaces-scroll".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        let mut agents = div()
            .id("agents-scroll")
            .debug_selector(|| "agents-scroll".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        spaces = spaces.track_scroll(&self.sidebar_scroll[0]);
        agents = agents.track_scroll(&self.sidebar_scroll[1]);
        let multi = self.endpoints.len() > 1;
        let mut agent_count = 0;
        // Child positions of the highlighted rows, for the one-time reveal below.
        // Agent rows are counted by `agent_count`, which indexes that list.
        let mut space_rows = 0usize;
        let mut highlighted = [None; 2];
        for (endpoint_index, endpoint) in self.endpoints.iter().enumerate() {
            let selected = endpoint_index == self.selected_endpoint;
            let endpoint_id = endpoint.id.clone();
            if multi {
                let collapse_id = endpoint_id.clone();
                let select_id = endpoint_id.clone();
                spaces = spaces.child(
                    div()
                        .id(SharedString::from(format!("host-{endpoint_id}")))
                        .debug_selector(|| format!("host-{endpoint_id}"))
                        .h(px(line_height(font) + 2. * layout.host_padding()))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(HOST_GAP))
                        .px(px(padding))
                        .when(selected, |row| row.bg(rgb(theme.active)))
                        .text_color(rgb(if endpoint.enabled {
                            theme.foreground
                        } else {
                            theme.muted
                        }))
                        .cursor_pointer()
                        .child(
                            div()
                                .id(SharedString::from(format!("collapse-host-{endpoint_id}")))
                                .w(px(HOST_ARROW_WIDTH))
                                .flex_none()
                                .child(label_text(if endpoint.collapsed {
                                    "\u{25b8}"
                                } else {
                                    "\u{25be}"
                                }))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    if let Some(endpoint) =
                                        this.endpoints.iter_mut().find(|e| e.id == collapse_id)
                                    {
                                        endpoint.collapsed = !endpoint.collapsed;
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                // As with workspace labels, avoid zero-basis text measurement.
                                .w(px(host_label_width))
                                .flex_none()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .w(px(host_label_width))
                                        .truncate()
                                        .child(label_text(&endpoint.label)),
                                ),
                        )
                        .when(show_host_status, |row| {
                            row.child(
                                div()
                                    .w(px(67.))
                                    .flex_none()
                                    .text_right()
                                    .text_size(px(font.size * 0.75))
                                    .text_color(rgb(theme.muted))
                                    .child(endpoint.status()),
                            )
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_endpoint(&select_id, cx);
                            window.focus(&this.focus);
                        })),
                );
                space_rows += 1;
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
            let entries = visible_workspace_entries(&snapshot.workspaces, collapsed_repos);
            // A child closes the group when no child follows it.
            let closes: Vec<bool> = (0..entries.len())
                .map(|position| {
                    entries[position].1 && !entries.get(position + 1).is_some_and(|next| next.1)
                })
                .collect();
            for (position, (index, indented, group)) in entries.into_iter().enumerate() {
                if multi && endpoint.collapsed {
                    break;
                }
                let workspace = &snapshot.workspaces[index];
                if selected && workspace.focused {
                    highlighted[0] = Some(space_rows);
                }
                space_rows += 1;
                let id = workspace.workspace_id.clone();
                let context_id = id.clone();
                let hover_id = id.clone();
                let context_endpoint = endpoint_id.clone();
                let navigate_endpoint = endpoint_id.clone();
                let collapse_endpoint = endpoint_id.clone();
                let reserve_arrow = group.is_some() || indented;
                let tree = match (indented, closes[position]) {
                    (false, _) => RowTree::None,
                    (true, false) => RowTree::Child,
                    (true, true) => RowTree::LastChild,
                };
                let arrow = group.map(|key| {
                    let collapsed = collapsed_repos.contains(&key);
                    div()
                        .id(SharedString::from(format!("collapse-{endpoint_id}-{id}")))
                        .debug_selector(move || format!("collapse-{index}"))
                        .w(px(ARROW_RESERVE - gap))
                        .h(px(
                            line_height(font) * if layout.workspace_details() { 2. } else { 1. }
                        ))
                        .flex_none()
                        .text_size(px(16.))
                        .text_color(rgb(theme.muted))
                        .hover(|s| s.text_color(rgb(theme.foreground)))
                        .child(label_text(if collapsed { "\u{25b8}" } else { "\u{25be}" }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            let collapsed = if collapse_endpoint == crate::endpoint::LOCAL {
                                &mut this.collapsed_repos
                            } else if let Some(endpoint) = this
                                .endpoints
                                .iter_mut()
                                .find(|e| e.id == collapse_endpoint)
                            {
                                &mut endpoint.collapsed_repos
                            } else {
                                return;
                            };
                            if !collapsed.remove(&key) {
                                collapsed.insert(key.clone());
                            }
                            cx.notify();
                        }))
                });
                let label = workspace_label(workspace, indented);
                spaces = spaces.child(
                    row(
                        label,
                        &[(label, true)],
                        first_text([workspace.branch.as_deref()], ""),
                        RowKind::Workspace,
                        workspace.agent_status,
                        indicators,
                        selected && workspace.focused,
                        tree,
                        reserve_arrow,
                        width,
                        if indented {
                            RowIcon::None
                        } else {
                            self.avatars
                                .as_ref()
                                .filter(|_| endpoint_index == 0)
                                .and_then(|avatars| avatars.image(&workspace.new_workspace_cwd))
                                .map_or(RowIcon::Mark, RowIcon::Avatar)
                        },
                        arrow,
                        workspace_badge(workspace, &self.menu.pr_cache, &self.git, theme),
                        layout,
                        (font, theme),
                    )
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
                    .when(
                        self.menu.page == Some(crate::menu::Page::Workspace),
                        |row| {
                            let view = cx.entity().downgrade();
                            let endpoint = endpoint_id.clone();
                            let workspace = id.clone();
                            row.child(
                                canvas(
                                    |_, _, _| (),
                                    move |bounds, _, window, _| {
                                        let bounds =
                                            bounds.intersect(&window.content_mask().bounds);
                                        window.on_mouse_event(
                                            move |event: &MouseDownEvent, phase, window, cx| {
                                                // The overlay dismisses first in bubble order. Use
                                                // clipped row geometry because it occludes our hitbox.
                                                if phase == DispatchPhase::Bubble
                                                    && event.button == MouseButton::Right
                                                    && bounds.contains(&event.position)
                                                {
                                                    let _ = view.update(cx, |this, cx| {
                                                        cx.stop_propagation();
                                                        if this.navigate_endpoint(
                                                            &endpoint,
                                                            NavigationTarget::Workspace(&workspace),
                                                            cx,
                                                        ) {
                                                            this.open_workspace_menu(
                                                                &workspace,
                                                                event.position,
                                                                window,
                                                                cx,
                                                            );
                                                            this.menu.opening_right_click = this
                                                                .menu
                                                                .page
                                                                == Some(
                                                                    crate::menu::Page::Workspace,
                                                                );
                                                        }
                                                    });
                                                }
                                            },
                                        );
                                    },
                                )
                                .absolute()
                                .inset_0()
                                .size_full(),
                            )
                        },
                    )
                    .id(SharedString::from(format!("workspace-{endpoint_id}-{id}")))
                    .when(multi, |row| {
                        row.debug_selector(|| format!("workspace-{endpoint_id}-{id}"))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate_endpoint(
                            &navigate_endpoint,
                            NavigationTarget::Workspace(&id),
                            cx,
                        );
                        window.focus(&this.focus);
                    }))
                    // Only the selected endpoint's rows arm the hover menu:
                    // another endpoint's menu would have to select it first, and
                    // resting the pointer must not switch which daemon is shown.
                    // The feature is opt-in, so rows stay unarmed without it.
                    .when(
                        selected && self.config.features.sidebar_hover_menu,
                        |row| {
                            row.on_hover(cx.listener(move |this, hovered: &bool, window, _| {
                                this.hover_workspace(&hover_id, *hovered, window);
                            }))
                        },
                    ),
                );
            }
            if !self.config.show_agents {
                continue;
            }
            for agent in sorted_agents(&snapshot.agents, self.agent_sort) {
                if selected && agent.focused {
                    highlighted[1] = Some(agent_count);
                }
                agent_count += 1;
                let id = agent.pane_id.clone();
                let navigate_endpoint = endpoint_id.clone();
                let host = (multi && endpoint_id != crate::endpoint::LOCAL)
                    .then_some(endpoint.label.as_str());
                let (name, detail) = agent_labels(agent, snapshot, host);
                agents = agents.child(
                    row(
                        &format!("agent-{id}"),
                        &name,
                        detail,
                        RowKind::Agent,
                        agent.agent_status,
                        indicators,
                        selected && agent.focused,
                        RowTree::None,
                        false,
                        width,
                        RowIcon::None,
                        None,
                        None,
                        layout,
                        (font, theme),
                    )
                    .id(SharedString::from(format!("agent-{endpoint_id}-{id}")))
                    .when(multi, |row| {
                        row.debug_selector(|| format!("agent-{endpoint_id}-{id}"))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate_endpoint(&navigate_endpoint, NavigationTarget::Pane(&id), cx);
                        window.focus(&this.focus);
                    })),
                );
            }
        }
        // Follow the selection, but only once a frame has measured the viewport:
        // the handle resolves the request against the previous frame's bounds, so
        // an unmeasured list would scroll to a meaningless offset. Recording what
        // was revealed keeps later frames from undoing the user's own scrolling.
        for (list, row) in highlighted.iter().enumerate() {
            let Some(row) = *row else { continue };
            if self.sidebar_revealed[list].get() != Some(row)
                && self.sidebar_scroll[list].bounds().size.height > px(0.)
            {
                self.sidebar_scroll[list].scroll_to_item(row);
                self.sidebar_revealed[list].set(Some(row));
            }
        }
        if agent_count == 0 {
            agents = agents.child(
                div()
                    .px(px(padding))
                    .text_color(rgb(theme.muted))
                    .truncate()
                    .child("no agents"),
            );
        }
        div()
            .id("sidebar")
            .debug_selector(|| "sidebar".into())
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
            // Zero flex bases keep long workspace lists from displacing agents.
            .child(
                div()
                    .debug_selector(|| "spaces-section".into())
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
                    .child(header("spaces", font, theme, layout))
                    .child(spaces)
                    .child(
                        div()
                            .flex_none()
                            .h(px(line_height(font) + 2. * layout.footer_padding()))
                            .px(px(padding))
                            .flex()
                            .items_center()
                            // Menu hugs the sidebar's edge, as in the terminal client.
                            .justify_between()
                            .text_color(rgb(theme.muted))
                            .gap(px(20.))
                            .child(
                                div()
                                    .id("new-workspace")
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(rgb(theme.foreground)))
                                    .child("new")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.command(Command::Workspace, window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .id("sidebar-menu")
                                    .debug_selector(|| "sidebar-menu".into())
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(rgb(theme.foreground)))
                                    .child(label_text("menu"))
                                    .on_click(cx.listener(
                                        |this, event: &ClickEvent, window, cx| {
                                            this.menu.anchor = event.position();
                                            this.open_menu(window, cx);
                                        },
                                    )),
                            ),
                    ),
            )
            .when(self.config.show_agents, |sidebar| {
                sidebar
                    .child(
                        div()
                            .id("sidebar-split-resize")
                            .debug_selector(|| "sidebar-split-resize".into())
                            .h(px(6.))
                            .flex_none()
                            .cursor(CursorStyle::ResizeUpDown)
                            .border_t_1()
                            .border_color(rgb(theme.active))
                            .hover(|s| s.bg(rgba(0x78a9ff44)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.sidebar_split_modified = true;
                                    if event.click_count == 2 {
                                        this.sidebar_drag = None;
                                        this.sidebar_split = None;
                                        this.save_chrome();
                                    } else {
                                        this.sidebar_drag = Some(SidebarDrag::Split);
                                    }
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "agents-section".into())
                            .flex()
                            .flex_col()
                            .flex_1()
                            .map(|mut section| {
                                section.style().flex_grow = Some(1. - split);
                                section
                            })
                            .min_h_0()
                            .overflow_hidden()
                            .child(
                                header("agents", font, theme, layout)
                                    .justify_between()
                                    .child(agents_sort(self, cx)),
                            )
                            .child(agents),
                    )
            })
            .child(
                div()
                    .id("sidebar-resize")
                    .debug_selector(|| "sidebar-resize".into())
                    .absolute()
                    .right_0()
                    .top_0()
                    .h_full()
                    .w(px(6.))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .hover(|s| s.bg(rgba(0x78a9ff44)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.sidebar_modified = true;
                            if event.click_count == 2 {
                                this.sidebar_drag = None;
                                this.sidebar_width = None;
                                this.save_sidebar_width();
                            } else {
                                this.sidebar_drag = Some(SidebarDrag::Width {
                                    start: f32::from(event.position.x),
                                    width,
                                });
                            }
                            cx.notify();
                        }),
                    ),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        // Capture globally so dragging continues outside the narrow divider,
                        // and terminal handlers never receive the resize gesture's release.
                        let moving = view.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                            if phase == DispatchPhase::Capture {
                                let _ = moving.update(cx, |this, cx| {
                                    if let Some(drag) = this.sidebar_drag {
                                        match drag {
                                            SidebarDrag::Width { start, width } => {
                                                this.sidebar_width = Some(sidebar_width(
                                                    Some(
                                                        width + f32::from(event.position.x) - start,
                                                    ),
                                                    f32::from(window.viewport_size().width),
                                                ));
                                            }
                                            SidebarDrag::Split => {
                                                let height =
                                                    (f32::from(bounds.size.height) - 6.).max(1.);
                                                this.sidebar_split = Some(
                                                    ((f32::from(
                                                        event.position.y - bounds.origin.y,
                                                    ) - 3.)
                                                        / height)
                                                        .clamp(0.1, 0.9),
                                                );
                                            }
                                        }
                                        cx.stop_propagation();
                                        cx.notify();
                                    }
                                });
                            }
                        });
                        let released = view.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture && event.button == MouseButton::Left
                            {
                                let _ = released.update(cx, |this, cx| {
                                    if this.sidebar_drag.take().is_some() {
                                        this.save_chrome();
                                        cx.stop_propagation();
                                        cx.notify();
                                    }
                                });
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
    }
}

pub(super) fn header(
    label: &'static str,
    font: &FontConfig,
    theme: &Theme,
    layout: &dyn SidebarLayout,
) -> Div {
    div()
        .flex_none()
        .h(px(line_height(font) + 2. * layout.header_padding()))
        .px(px(layout.padding()))
        .flex()
        .items_center()
        .text_size(px(font.size))
        .text_color(rgb(theme.muted))
        .child(label)
}
