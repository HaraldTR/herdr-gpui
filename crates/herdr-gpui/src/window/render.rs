//! Painting the window from prepared state. Render reads bounded caches and
//! the latest projection only: it never queries the daemon, touches disk, or
//! starts a process.

use super::HerdrWindow;
use crate::{
    APP_VERSION, CheckForUpdates, RunCommand, ShowHerdrNotDetected, ShowUpdatePreview, TAB_HEIGHT,
    TAB_WIDTH, controls::Command, fonts::StyledFont, navigation::NavigationTarget,
    state::ConnectionStatus, terminal::*, worktree_banner,
};
use gpui::{prelude::*, *};
use herdr_client::ConnectOptions;
use std::time::Duration;

impl Render for HerdrWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.restore_menu_focus(window);
        let font = self.config.terminal.font();
        let cell_height = self.config.terminal.line_height();
        self.painter.borrow_mut().set_appearance(
            self.config.terminal.size,
            cell_height,
            self.theme.clone(),
        );
        self.cell_width = self.painter.borrow_mut().cell_width(&font, window, cx);
        let sidebar = self.render_sidebar(window, cx);
        let mut tabs = div()
            .id("tabs")
            .flex()
            .flex_none()
            .h(px((self.config.tabs.size * 1.6 + 4.).max(TAB_HEIGHT)))
            .text_font(&self.config.tabs)
            .text_size(px(self.config.tabs.size))
            .overflow_x_scroll()
            .bg(rgb(self.theme.surface))
            .text_color(rgb(self.theme.foreground))
            .items_center();
        if let Some(snapshot) = &self.live.snapshot {
            for tab in snapshot
                .tabs
                .iter()
                .filter(|t| Some(&t.workspace_id) == snapshot.focused_workspace_id.as_ref())
            {
                let id = tab.tab_id.clone();
                let context_id = id.clone();
                let close_id = id.clone();
                // Selected tabs carry the theme's accent, so the choice reads as
                // primary rather than as the hover tint used elsewhere; the rest
                // recede into the strip, as they do in the reference UI.
                let (background, text) = if tab.focused {
                    let background = self.theme.primary_wash();
                    (background, self.theme.text_on(background))
                } else {
                    (self.theme.surface, self.theme.muted)
                };
                tabs = tabs.child(
                    div()
                        .id(SharedString::from(format!("tab-{id}")))
                        .debug_selector({
                            let id = id.clone();
                            move || format!("tab-{id}")
                        })
                        .pl(px(12.))
                        // The close button hugs the tab's inner right edge, well
                        // clear of the label it would otherwise crowd.
                        .pr(px(3.))
                        .py(px(2.))
                        // Even cells divided by a single rule, as in the reference UI.
                        .min_w(px(TAB_WIDTH))
                        .border_r_1()
                        .border_color(rgb(self.theme.active))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .cursor_pointer()
                        .bg(rgb(background))
                        .text_color(rgb(text))
                        .child(tab.label.clone())
                        .child(
                            div()
                                .id("close-tab")
                                .debug_selector({
                                    let id = id.clone();
                                    move || format!("close-tab-{id}")
                                })
                                .size(px(18.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(3.))
                                .hover(move |s| s.bg(rgba((text << 8) | 0x24)))
                                .child(
                                    svg()
                                        .path("icons/close.svg")
                                        .debug_selector({
                                            let id = id.clone();
                                            move || format!("close-tab-icon-{id}")
                                        })
                                        .size(px(12.))
                                        .text_color(rgb(text)),
                                )
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.open_tab_close(&close_id, window, cx);
                                })),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_tab_menu(&context_id, event.position, window, cx);
                            }),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.navigate(NavigationTarget::Tab(&id), cx);
                            window.focus(&this.focus);
                        })),
                );
            }
        }
        // Paints the frame on screen, which during a focus change is the one
        // presented before it: the terminal area never blanks between two
        // projections. What the client knows to be current stays in `live`.
        let surface = self.presentation.frame(&self.live);
        let entity = cx.entity();
        let paint_entity = entity.clone();
        let focus = self.focus.clone();
        let cell_width = self.cell_width;
        let painter = self.painter.clone();
        self.hovered_terminal_link = self.terminal_link_at(window.mouse_position()).is_some();
        let terminal = div()
            .id("terminal")
            .when(self.hovered_terminal_link, |terminal| {
                terminal.cursor_pointer()
            })
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let hovered = this.terminal_link_at(event.position).is_some();
                if hovered != this.hovered_terminal_link {
                    this.hovered_terminal_link = hovered;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(Self::open_terminal_link))
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .bg(rgb(self.theme.background))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.pressed_terminal_link = this
                        .terminal_link_at(event.position)
                        .map(|url| (url, event.position));
                    if this.menu.page.is_some() {
                        return;
                    }
                    if this.pressed_terminal_link.is_some() {
                        cx.stop_propagation();
                        return;
                    }
                    window.focus(&this.focus);
                    if let Some(surface) = &this.live.surface
                        && surface.popup.is_none()
                    {
                        let col = ((event.position.x - this.bounds.origin.x).to_f64()
                            / this.cell_width as f64)
                            .floor() as u16;
                        let row = ((event.position.y - this.bounds.origin.y).to_f64()
                            / this.config.terminal.line_height() as f64)
                            .floor() as u16;
                        let pane = surface
                            .panes
                            .iter()
                            .find(|p| {
                                col >= p.rect.x
                                    && col < p.rect.x.saturating_add(p.rect.width)
                                    && row >= p.rect.y
                                    && row < p.rect.y.saturating_add(p.rect.height)
                            })
                            .map(|p| p.pane_id.clone());
                        if let Some(id) = pane {
                            this.navigate(NavigationTarget::Pane(&id), cx);
                        }
                    }
                }),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |this, _| {
                            this.bounds = bounds;
                            this.options = ConnectOptions {
                                surface_size: viewport(
                                    bounds.size.width.to_f64() as f32,
                                    bounds.size.height.to_f64() as f32,
                                    cell_width,
                                    cell_height,
                                ),
                                cell_width_px: cell_width.round().max(1.) as u32,
                                cell_height_px: cell_height.round().max(1.) as u32,
                            };
                            this.resize();
                        });
                    },
                    move |bounds, _, window, cx| {
                        // Capture movement outside the terminal too, before any
                        // element can stop propagation of a drag-away event.
                        let entity = paint_entity.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                entity.update(cx, |this, _| {
                                    if this.pressed_terminal_link.as_ref().is_some_and(
                                        |(_, position)| {
                                            (event.position.x - position.x).abs() > px(4.)
                                                || (event.position.y - position.y).abs() > px(4.)
                                        },
                                    ) {
                                        this.pressed_terminal_link = None;
                                    }
                                });
                            }
                        });
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, paint_entity.clone()),
                            cx,
                        );
                        if let Some(surface) = &surface {
                            painter.borrow_mut().paint_frame(
                                &surface.frame,
                                bounds.origin,
                                cell_width,
                                &font,
                                window,
                                cx,
                            );
                            if let Some(popup) = &surface.popup {
                                let offset = popup_origin(
                                    &surface.frame,
                                    &popup.frame,
                                    cell_width,
                                    cell_height,
                                );
                                painter.borrow_mut().paint_frame(
                                    &popup.frame,
                                    bounds.origin + offset,
                                    cell_width,
                                    &font,
                                    window,
                                    cx,
                                );
                            }
                        }
                    },
                )
                .size_full(),
            );
        let status = self.live.status_text(self.local_error.as_deref());
        div()
            .on_action(cx.listener(|this, action: &RunCommand, window, cx| {
                this.command(action.command, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowHerdrNotDetected, window, cx| {
                this.show_install_modal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CheckForUpdates, window, cx| {
                this.open_app_update(false, window, cx);
                this.updater.check();
            }))
            .on_action(cx.listener(|this, _: &ShowUpdatePreview, window, cx| {
                this.open_app_update(true, window, cx);
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(self.theme.background))
            .text_color(rgb(self.theme.foreground))
            .text_font(&self.config.ui)
            .text_size(px(self.config.ui.size))
            .child(self.render_titlebar(cx))
            .children(worktree_banner::render(
                env!("HERDR_BUILD_WORKTREE") == "1",
                env!("HERDR_BUILD_BRANCH"),
                env!("HERDR_BUILD_PR"),
            ))
            .child(
                div()
                    .debug_selector(|| "window-body".into())
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.sidebar_visible, |row| row.child(sidebar))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .bg(rgb(self.theme.surface))
                                    .text_color(rgb(self.theme.foreground))
                                    // Tabs size to their content and shrink when the
                                    // row is full, so the button sits after the last
                                    // tab instead of at the far right of the window.
                                    .child(tabs.flex_shrink().min_w_0())
                                    .child(
                                        div()
                                            .id("new-tab")
                                            .debug_selector(|| "new-tab".into())
                                            .w(px(34.))
                                            .min_h(px(TAB_HEIGHT))
                                            .border_r_1()
                                            .border_color(rgb(self.theme.active))
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .hover(|s| s.bg(rgb(self.theme.active)))
                                            .child(
                                                svg()
                                                    .path("icons/plus.svg")
                                                    .debug_selector(|| "new-tab-icon".into())
                                                    .size(px(14.))
                                                    // Quiet like the unselected tabs beside it.
                                                    .text_color(rgb(self.theme.muted)),
                                            )
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.command(Command::Tab, window, cx)
                                            })),
                                    ),
                            )
                            .child(terminal),
                    ),
            )
            .child(
                div()
                    .id("connection-status")
                    .debug_selector(|| "connection-status".into())
                    .flex()
                    .flex_none()
                    .h(px((self.config.ui.size * 1.5 + 4.).max(22.)))
                    .overflow_hidden()
                    .items_center()
                    .gap(px(6.))
                    .px_3()
                    .bg(rgb(self.theme.surface))
                    .text_color(rgb(self.theme.foreground))
                    .child(
                        if matches!(self.live.status, ConnectionStatus::StartingDaemon) {
                            div()
                                .size(px(8.))
                                .flex_none()
                                .rounded_full()
                                .bg(rgb(self.theme.palette[3]))
                                .with_animation(
                                    "daemon-starting-loader",
                                    Animation::new(Duration::from_secs(1)).repeat(),
                                    |dot, delta| {
                                        dot.opacity(
                                            0.3 + 0.7 * (delta * std::f32::consts::PI).sin(),
                                        )
                                    },
                                )
                                .into_any_element()
                        } else {
                            div()
                                .size(px(6.))
                                .flex_none()
                                .rounded_full()
                                .bg(rgb(if self.live.status.is_connected() {
                                    self.theme.palette[2]
                                } else {
                                    self.theme.palette[1]
                                }))
                                .into_any_element()
                        },
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(status),
                    )
                    .when(!self.marked.is_empty(), |d| {
                        d.child(
                            div()
                                .min_w_0()
                                .max_w(px(160.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(format!("Composing: {}", self.marked)),
                        )
                    })
                    .child(
                        div()
                            .id("status-theme")
                            .debug_selector(|| "status-theme".into())
                            .flex_none()
                            .px_2()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(self.theme.active)))
                            .child("Theme")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_theme_picker(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id("status-keybinds")
                            .debug_selector(|| "status-keybinds".into())
                            .flex_none()
                            .px_2()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(self.theme.active)))
                            .child("? Keybinds")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_keybinds(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id("report-issue")
                            .debug_selector(|| "report-issue".into())
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .px_2()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(self.theme.active)))
                            .child(
                                div()
                                    .size(px(12.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .border_1()
                                    .border_color(rgb(self.theme.foreground))
                                    .child(
                                        div()
                                            .size(px(3.))
                                            .rounded_full()
                                            .bg(rgb(self.theme.foreground)),
                                    ),
                            )
                            .child("Report issue")
                            .on_click(|_, _, cx| {
                                cx.open_url(&format!(
                                    "https://github.com/penso/herdr-gpui/issues/new?template=bug_report.yml&version={}",
                                    APP_VERSION.replace('+', "%2B"),
                                ));
                            }),
                    )
                    .child(
                        div()
                            .id("status-version")
                            .debug_selector(|| "status-version".into())
                            .flex_none()
                            .whitespace_nowrap()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(self.theme.active)))
                            // A waiting update is the one status here worth
                            // interrupting for, so it takes the accent color
                            // the rest of the chrome reserves for chosen rows.
                            .text_color(rgb(if self.updater.update_available() {
                                self.theme.primary()
                            } else {
                                self.theme.muted
                            }))
                            .child(if self.updater.update_available() {
                                "Update available"
                            } else {
                                APP_VERSION
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_app_update(false, window, cx);
                            })),
                    ),
            )
            .child(self.notifications.render_notifications(&self.theme))
            .when(self.menu.page.is_some(), |root| {
                root.child(self.render_menu(window, cx))
            })
    }
}
