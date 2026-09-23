use crate::{HerdrWindow, fonts::StyledFont};
use gpui::{prelude::*, *};

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::Action;
    use gpui::TestAppContext;

    #[gpui::test]
    fn connected_panel_is_content_sized_with_only_header_close(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        for (width, height) in [(320., 400.), (640., 780.), (1200., 1000.)] {
            cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(height)));
            cx.update(|window, cx| {
                cx.default_global::<crate::sidebar::layout_tests::PaintedProbes>()
                    .0
                    .clear();
                view.update(cx, |view, cx| {
                    view.github_fixture(false, window, cx);
                    view.menu.github = crate::github::Auth::connected_fixture();
                });
                window.draw(cx).clear();
            });
            let panel = cx.debug_bounds("menu-panel").unwrap();
            assert!(panel.size.width <= gpui::px(400.));
            assert!(panel.size.height <= gpui::px(230.));
            assert!(cx.debug_bounds("github-status").is_none());
            assert!(cx.debug_bounds("github-close").is_none());
            assert!(cx.debug_bounds("github-header-close").is_some());
            // The mark is vector, so it is sharp at the header's own size, and
            // the avatar slot beside it stays square for a round crop.
            let mark = cx.debug_bounds("github-mark").unwrap();
            let avatar = cx.debug_bounds("github-avatar").unwrap();
            assert_eq!(mark.size, gpui::size(gpui::px(28.), gpui::px(28.)));
            assert_eq!(avatar.size, gpui::size(gpui::px(40.), gpui::px(40.)));
            cx.simulate_keystrokes("tab tab enter");
            cx.update(|window, cx| {
                assert!(view.read(cx).menu.page.is_none());
                assert!(view.read(cx).menu.github.connected());
                assert!(view.read(cx).focus.is_focused(window));
                cx.default_global::<crate::sidebar::layout_tests::PaintedProbes>()
                    .check()
                    .unwrap();
            });
        }
    }

    #[gpui::test]
    fn actions_follow_state_and_focus_cannot_become_signout(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.github_fixture(false, window, cx));
        });
        cx.simulate_keystrokes("d o cmd-c");
        assert!(cx.opened_url().is_none());
        cx.update(|_, cx| {
            assert!(!view.read(cx).menu.github.busy());
            assert!(cx.read_from_clipboard().is_none());
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.github_fixture(true, window, cx));
        });
        cx.simulate_keystrokes("tab tab");
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                assert!(view.menu.github_selected == Some(Action::Open));
                view.menu.github = crate::github::Auth::connected_fixture();
                cx.notify();
            });
        });
        cx.simulate_keystrokes("enter");
        cx.update(|_, cx| assert!(view.read(cx).menu.github.connected()));
        assert!(cx.opened_url().is_none());
        cx.simulate_keystrokes("d");
        cx.update(|_, cx| {
            assert!(!view.read(cx).menu.github.connected());
            assert!(view.read(cx).menu.github.busy());
        });
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Copy,
    Open,
    Start,
    SignOut,
    Close,
}

impl Action {
    fn id(self) -> &'static str {
        match self {
            Self::Copy => "github-copy",
            Self::Open => "github-open",
            Self::Start => "github-start",
            Self::SignOut => "github-sign-out",
            Self::Close => "github-header-close",
        }
    }
}

impl HerdrWindow {
    fn github_actions(&self) -> Vec<Action> {
        let auth = &self.menu.github;
        let mut actions = Vec::new();
        if auth.code().is_some() {
            actions.extend([Action::Copy, Action::Open]);
        } else if !auth.busy() && !auth.loading_profile() && !auth.connected() {
            actions.push(Action::Start);
        }
        if auth.can_sign_out() && !auth.busy() && !auth.loading_profile() {
            actions.push(Action::SignOut);
        }
        actions.push(Action::Close);
        actions
    }

    fn github_action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if !self.github_actions().contains(&action) {
            return;
        }
        match action {
            Action::Copy => {
                if let Some(code) = self.menu.github.copy_code() {
                    cx.write_to_clipboard(ClipboardItem::new_string(code.to_owned()));
                }
            }
            Action::Open => cx.open_url(crate::github::VERIFY_URL),
            Action::Start => self.menu.github.start(&self.config),
            Action::SignOut => {
                self.menu.pr_cache.clear();
                self.menu.pr.clear();
                self.menu.github.sign_out();
            }
            Action::Close => self.dismiss_menu(window, cx),
        }
        cx.notify();
    }

    pub(super) fn github_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.prevent_default();
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let actions = self.github_actions();
        if key == "tab" && !modifiers.control && !modifiers.platform && !modifiers.alt {
            let selected = self
                .menu
                .github_selected
                .and_then(|action| actions.iter().position(|candidate| *candidate == action));
            self.menu.github_selected = Some(
                actions[match selected {
                    None if modifiers.shift => actions.len() - 1,
                    None => 0,
                    Some(index) => {
                        (index
                            + if modifiers.shift {
                                actions.len() - 1
                            } else {
                                1
                            })
                            % actions.len()
                    }
                }],
            );
            if self.menu.github_selected == Some(Action::Copy) {
                self.menu.github_scroll.set_offset(Point::default());
            }
        } else if key == "c"
            && modifiers.platform
            && !modifiers.shift
            && !modifiers.alt
            && !modifiers.control
        {
            self.github_action(Action::Copy, window, cx);
        } else if modifiers == Modifiers::default() {
            let action = match key {
                "escape" => Some(Action::Close),
                "c" => {
                    if self.menu.github.busy() {
                        self.menu.github.cancel();
                    }
                    None
                }
                "o" => Some(Action::Open),
                "s" => Some(Action::Start),
                "d" => Some(Action::SignOut),
                "enter" | "space" => self.menu.github_selected,
                "up" | "down" | "pageup" | "pagedown" => {
                    let scroll = &self.menu.github_scroll;
                    let distance = if key.starts_with("page") {
                        scroll.bounds().size.height * 0.8
                    } else {
                        px(self.config.ui.line_height() * 3.)
                    };
                    scroll.set_offset(
                        scroll.offset()
                            + point(
                                px(0.),
                                distance * if key.ends_with("up") { 1. } else { -1. },
                            ),
                    );
                    None
                }
                _ => None,
            };
            if let Some(action) = action {
                self.github_action(action, window, cx);
            }
        }
        cx.notify();
    }

    fn github_button(
        &self,
        action: Action,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = &self.theme;
        let primary = matches!(action, Action::Open | Action::Start);
        let accent = rgb(theme.foreground).blend(rgba((theme.palette[4] << 8) | 0x70));
        let selected = self.menu.github_selected == Some(action);
        div()
            .id(action.id())
            .debug_selector(move || action.id().into())
            .flex_none()
            .px(px(12.))
            .py(px(7.))
            .rounded(px(5.))
            .border_1()
            .border_color(if selected {
                rgb(theme.foreground)
            } else {
                rgb(theme.active)
            })
            .bg(if primary {
                accent
            } else {
                rgb(theme.background)
            })
            .text_color(if primary {
                rgb(theme.background)
            } else {
                rgb(theme.foreground)
            })
            .font_weight(if primary {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            })
            .cursor_pointer()
            .hover(|style| style.border_color(accent))
            .child(crate::sidebar::label_text(label))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.github_action(action, window, cx);
            }))
    }

    pub(super) fn render_github_auth(&self, cx: &mut Context<Self>) -> Div {
        let auth = &self.menu.github;
        let theme = &self.theme;
        let font = &self.config.ui;
        let mut body = div()
            .id("github-body")
            .debug_selector(|| "github-body".into())
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.menu.github_scroll)
            .p(px(16.));
        if let Some(profile) = &auth.profile {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .size(px(40.))
                            .flex_none()
                            .rounded_full()
                            .overflow_hidden()
                            // Round the image itself: GPUI 0.2.2 clips overflow
                            // to the box, not to its corner radii.
                            .debug_selector(|| "github-avatar".into())
                            .child(match &profile.avatar {
                                Some(image) => img(image.clone())
                                    .size_full()
                                    .rounded_full()
                                    .into_any_element(),
                                None => crate::sidebar::github_mark(theme.muted)
                                    .size_full()
                                    .into_any_element(),
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_color(rgb(theme.palette[2])).child("Connected"))
                            .child(
                                div()
                                    .id("github-account")
                                    .overflow_x_scroll()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("@{}", profile.login)),
                            ),
                    ),
            );
        }
        if let Some(code) = auth.code() {
            body = body
                .child(div().mb(px(12.)).child("1. Copy your one-time code"))
                .child(
                    div()
                        .p(px(16.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(rgb(theme.active))
                        .bg(rgb(theme.background))
                        .child(
                            div().id("github-code-scroll").overflow_x_scroll().child(
                                div()
                                    .debug_selector(|| "github-device-code".into())
                                    .text_font(&self.config.terminal)
                                    .text_size(px(24.))
                                    .line_height(px(32.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(crate::sidebar::label_text(code)),
                            ),
                        )
                        .child(div().mt(px(12.)).child(self.github_button(
                            Action::Copy,
                            if auth.copied() { "Copied" } else { "Copy code" },
                            cx,
                        ))),
                )
                .child(div().mt(px(16.)).child("2. Open GitHub and paste the code"))
                .child(
                    div()
                        .mt(px(4.))
                        .text_color(rgb(theme.muted))
                        .child("github.com/login/device"),
                );
        }
        let message = auth.message.clone().unwrap_or_else(|| {
            if auth.loading_profile() {
                "Checking GitHub account..."
            } else if auth.connected() {
                "Your account is ready for pull request lookups."
            } else {
                "Sign in securely in your browser to view pull requests. No GitHub CLI required."
            }
            .into()
        });
        if !auth.connected() || auth.failed {
            body = body.child(
                div()
                    .debug_selector(|| "github-status".into())
                    .mt(px(16.))
                    .p(px(10.))
                    .rounded(px(5.))
                    .bg(rgb(theme.background))
                    .text_color(rgb(if auth.failed {
                        theme.palette[1]
                    } else {
                        theme.muted
                    }))
                    .child(message),
            );
        }
        if let Some(note) = auth.store().note(auth.connected()) {
            let (color, text) = match note {
                crate::github::Note::Warning(text) => (theme.palette[1], text),
                crate::github::Note::Info(text) => (theme.muted, text),
            };
            body = body.child(div().mt(px(12.)).text_color(rgb(color)).child(text));
        }
        let mut footer = div()
            .debug_selector(|| "github-footer".into())
            .flex_none()
            .p(px(12.))
            .border_t_1()
            .border_color(rgb(theme.active))
            .flex()
            .flex_wrap()
            .justify_end()
            .gap(px(8.));
        let mut has_footer = false;
        for action in self
            .github_actions()
            .into_iter()
            .filter(|action| !matches!(action, Action::Copy | Action::Close))
        {
            has_footer = true;
            let label = match action {
                Action::Open => "Open GitHub (O)",
                Action::Start if auth.failed => "Try again (S)",
                Action::Start => "Sign in (S)",
                Action::SignOut => "Sign out (D)",
                Action::Copy | Action::Close => continue,
            };
            footer = footer.child(self.github_button(action, label, cx));
        }
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(px(12.))
                    .p(px(16.))
                    .border_b_1()
                    .border_color(rgb(theme.active))
                    .child(
                        crate::sidebar::github_mark(theme.foreground)
                            .debug_selector(|| "github-mark".into())
                            .size(px(28.)),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            div()
                                .text_size(px(font.size * 1.35))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("GitHub"),
                        ),
                    )
                    .child(
                        div()
                            .id("github-header-close")
                            .debug_selector(|| "github-header-close".into())
                            .cursor_pointer()
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(4.))
                            .when(self.menu.github_selected == Some(Action::Close), |style| {
                                style.bg(rgb(theme.active))
                            })
                            .hover(|style| style.bg(rgb(theme.active)))
                            .child(crate::sidebar::label_text("Close"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.dismiss_menu(window, cx)),
                            ),
                    ),
            )
            .child(body)
            .when(has_footer, |panel| panel.child(footer))
    }
}
