//! Daemon status text in the native header.

use crate::{
    LiveState,
    config::{FontConfig, Theme},
    fonts::StyledFont,
    notifications::safe_text,
    state::ConnectionStatus,
};
use gpui::{prelude::*, *};
use herdr_client::protocol::ClientShellSnapshot;
use std::ops::Range;

const MAX_SEGMENTS: usize = 32;
const MAX_TEXT_CHARS: usize = 128;
const MAX_SEPARATOR_CHARS: usize = 32;

#[derive(Default)]
struct StatusText {
    text: String,
    accents: Vec<Range<usize>>,
}

impl StatusText {
    fn snapshot(snapshot: &ClientShellSnapshot) -> Self {
        let mut result = Self::default();
        let separator = safe_text(&snapshot.tab_bar_right_separator, MAX_SEPARATOR_CHARS);
        for segment in snapshot.tab_bar_right.iter().take(MAX_SEGMENTS) {
            let text = safe_text(&segment.text, MAX_TEXT_CHARS);
            if text.trim().is_empty() {
                continue;
            }
            if !result.text.is_empty() {
                result.text.push_str(&separator);
            }
            let start = result.text.len();
            result.text.push_str(&text);
            if segment.accent {
                result.accents.push(start..result.text.len());
            }
        }
        if snapshot.tab_bar_right.len() > MAX_SEGMENTS {
            result.text.push('…');
        }
        result
    }

    fn live(live: &LiveState) -> Self {
        live.snapshot
            .as_deref()
            .filter(|_| live.status == ConnectionStatus::Connected)
            .map(Self::snapshot)
            .unwrap_or_default()
    }
}

pub(super) fn render(live: &LiveState, font: &FontConfig, theme: &Theme) -> Div {
    let status = StatusText::live(live);
    let accent = HighlightStyle {
        color: Some(rgb(theme.text_on(theme.primary())).into()),
        background_color: Some(rgb(theme.primary()).into()),
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    div()
        .debug_selector(|| "titlebar-status".into())
        .size_full()
        .flex()
        .items_center()
        .pr(px(8.))
        .overflow_hidden()
        .text_font(font)
        .text_size(px(font.size.min(20.)))
        .text_color(rgb(theme.muted))
        .child(
            div()
                .w_full()
                .min_w_0()
                .truncate()
                .text_align(TextAlign::Right)
                .child(
                    StyledText::new(status.text)
                        .with_highlights(status.accents.into_iter().map(|range| (range, accent))),
                ),
        )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use core::prelude::v1::test;
    use herdr_client::protocol::ClientShellTabStatusSegment;

    fn text() -> StatusText {
        let mut snapshot = crate::sidebar::layout_tests::snapshot(1);
        snapshot.tab_bar_right_separator = " | ".into();
        snapshot.tab_bar_right = vec![
            ClientShellTabStatusSegment {
                text: "Usage 👩‍💻 25%".into(),
                accent: false,
            },
            ClientShellTabStatusSegment {
                text: "".into(),
                accent: false,
            },
            ClientShellTabStatusSegment {
                text: "ZOOM".into(),
                accent: true,
            },
        ];
        StatusText::snapshot(&snapshot)
    }

    #[test]
    fn snapshot_preserves_order_separator_and_accent_ranges() {
        let text = text();
        assert_eq!(text.text, "Usage 👩‍💻 25% | ZOOM");
        assert_eq!(text.accents.len(), 1);
        assert_eq!(&text.text[text.accents[0].clone()], "ZOOM");
        let mut empty = crate::sidebar::layout_tests::snapshot(0);
        empty.tab_bar_right.clear();
        assert!(StatusText::snapshot(&empty).text.is_empty());
    }

    #[test]
    fn remote_text_is_bounded_without_controls() {
        let mut snapshot = crate::sidebar::layout_tests::snapshot(0);
        snapshot.tab_bar_right_separator = "|\r".repeat(1000);
        snapshot.tab_bar_right = vec![
            ClientShellTabStatusSegment {
                text: "x\n".repeat(1000),
                accent: true
            };
            100
        ];
        let text = StatusText::snapshot(&snapshot).text;
        assert!(!text.chars().any(char::is_control));
        assert!(
            text.chars().count()
                <= MAX_SEGMENTS * MAX_TEXT_CHARS + (MAX_SEGMENTS - 1) * MAX_SEPARATOR_CHARS + 1
        );
    }

    #[gpui::test]
    fn usage_display_switches_apply_independently_on_reload(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        view.update(cx, |view, cx| {
            view.live.status = ConnectionStatus::Connected;
            let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
            snapshot.tab_bar_right = vec![ClientShellTabStatusSegment {
                text: "Usage 25%".into(),
                accent: false,
            }];
            cx.notify();
        });
        cx.simulate_resize(size(px(1000.), px(900.)));
        cx.run_until_parked();
        for (topbar, inline) in [
            (true, true),
            (false, true),
            (true, false),
            (false, false),
            (true, true),
        ] {
            view.update(cx, |view, cx| {
                let mut config = view.config.clone();
                config.usage.show = false;
                config.usage.topbar = topbar;
                config.usage.inline = inline;
                config.sidebar_layout = toml::from_str(
                    r#"
                        [agents]
                        rows = [["workspace"], ["agent"], ["state_text"]]
                        [spaces]
                        rows = [["workspace"], ["state_text"], ["branch"]]
                    "#,
                )
                .unwrap();
                let theme = view.theme.clone();
                view.load_gui_config_with(move || Ok((config, theme)), cx);
            });
            cx.run_until_parked();
            assert_eq!(cx.debug_bounds("titlebar-status").is_some(), topbar);
            assert!(cx.debug_bounds("titlebar-avatar").is_some());
            assert_eq!(cx.debug_bounds("line-herdr-2").is_some(), inline);
            assert_eq!(cx.debug_bounds("line-agent-p0-2").is_some(), inline);
        }
    }

    #[gpui::test]
    fn header_keeps_controls_reachable(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.live.status = ConnectionStatus::Connected;
                let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
                snapshot.tab_bar_right = vec![ClientShellTabStatusSegment {
                    text: "Account usage 25% weekly 10%".into(),
                    accent: false,
                }];
                view.git = crate::git::Git::fixture(
                    crate::pull_request::Input {
                        checkout: None,
                        repo_key: "/fixture/.git".into(),
                        branch: "main".into(),
                    },
                    crate::git::Status::default(),
                );
                cx.notify();
            })
        });
        for width in [1200., 360., 240., 1200.] {
            cx.simulate_resize(size(px(width), px(600.)));
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
            let git = cx.debug_bounds("titlebar-git").unwrap();
            let avatar = cx.debug_bounds("titlebar-avatar").unwrap();
            assert!(git.right() <= avatar.left());
            assert_eq!(avatar.right(), px(width - 6.));
            let status = cx.debug_bounds("titlebar-status").unwrap();
            // Clear of the leading controls, whatever room the platform
            // leaves before them.
            assert!(status.left() >= cx.debug_bounds("toggle-sidebar").unwrap().right());
            assert!(status.right() <= git.left());
        }
    }

    #[test]
    fn endpoint_status_replaces_and_disconnect_clears() {
        let mut live = LiveState::default();
        live.status = ConnectionStatus::Connected;
        let mut remote = crate::sidebar::layout_tests::snapshot(1);
        remote.tab_bar_right = vec![ClientShellTabStatusSegment {
            text: "Remote usage".into(),
            accent: false,
        }];
        live.apply(herdr_client::ClientEvent::Snapshot(std::sync::Arc::new(
            remote,
        )));
        assert_eq!(StatusText::live(&live).text, "Remote usage");
        live.apply(herdr_client::ClientEvent::Disconnected {
            reason: herdr_client::Error::Disconnected.to_string(),
        });
        assert!(StatusText::live(&live).text.is_empty());
    }
}
