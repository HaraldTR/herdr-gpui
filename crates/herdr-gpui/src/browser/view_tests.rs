#![allow(clippy::unwrap_used)]

use super::{Store, WebUrl, view::scope};
#[cfg(unix)]
use crate::control::{Placed, Target};
use crate::{
    HerdrWindow,
    sidebar::layout_tests::{fixture_window, full_draw, snapshot},
};
use gpui::{Entity, VisualTestContext};
use std::sync::Arc;

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| full_draw(window, cx).clear(cx));
}

fn url(value: &str) -> WebUrl {
    WebUrl::try_from(value).unwrap()
}

/// The fixture window, showing workspace `w0` as a connected daemon would.
fn window(cx: &mut gpui::TestAppContext) -> (Entity<HerdrWindow>, &mut VisualTestContext) {
    cx.add_window_view(|window, cx| {
        let mut view = fixture_window(window, cx);
        let mut shown = snapshot(40);
        shown.focused_workspace_id = Some("w0".into());
        shown.focused_tab_id = Some("t0".into());
        view.live.snapshot = Some(Arc::new(shown));
        view
    })
}

/// Needs a build that shows pages: elsewhere a new tab opens nothing.
#[cfg(any(target_os = "macos", windows))]
mod embedded {
    use super::*;
    use crate::controls::Command;

    /// How many browser tabs the app holds; test IDs start at zero.
    fn tab_count(cx: &mut VisualTestContext) -> usize {
        cx.update(|_, cx| {
            cx.try_global::<Store>().map_or(0, |store| {
                (0..64)
                    .filter(|id| store.get(crate::browser::TabId::test(*id)).is_some())
                    .count()
            })
        })
    }

    #[gpui::test]
    fn a_browser_tab_covers_the_terminal_until_it_closes(cx: &mut gpui::TestAppContext) {
        let (view, cx) = window(cx);
        draw(cx);
        assert!(cx.debug_bounds("terminal").is_some());
        assert!(cx.debug_bounds("browser").is_none());

        // A blank tab needs no page, so this runs without a native web view.
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.command(Command::NewBrowserTab, window, cx)
            });
        });
        draw(cx);
        assert!(cx.debug_bounds("browser-tab-0").is_some());
        assert!(cx.debug_bounds("browser").is_some());
        assert!(cx.debug_bounds("browser-address").is_some());
        assert!(cx.debug_bounds("browser-placeholder").is_some());
        assert!(
            cx.debug_bounds("terminal").is_none(),
            "the page replaces it"
        );

        // Clicking a Herdr tab brings its terminal back; the browser tab stays.
        let herdr_tab = cx.debug_bounds("tab-t0").unwrap();
        cx.simulate_click(herdr_tab.center(), gpui::Modifiers::none());
        draw(cx);
        assert!(cx.debug_bounds("terminal").is_some());
        assert!(cx.debug_bounds("browser-tab-0").is_some());

        // Close Tab closes the page rather than asking about the Herdr tab.
        let browser_tab = cx.debug_bounds("browser-tab-0").unwrap();
        cx.simulate_click(browser_tab.center(), gpui::Modifiers::none());
        draw(cx);
        assert!(cx.debug_bounds("browser").is_some());
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.command(Command::CloseTab, window, cx));
        });
        draw(cx);
        assert!(cx.debug_bounds("browser-tab-0").is_none());
        assert!(cx.debug_bounds("terminal").is_some());
        view.read_with(cx, |view, _| assert!(view.menu.page.is_none()));
        assert_eq!(tab_count(cx), 0);
    }
}

/// Opens a request's tab without switching to it, so no native page is made.
#[cfg(unix)]
fn request(
    view: &Entity<HerdrWindow>,
    cx: &mut VisualTestContext,
    daemon: Option<&str>,
    workspace: Option<&str>,
    strict: bool,
) -> Option<Placed> {
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            let target = Target {
                daemon: daemon.map(std::path::Path::new),
                workspace,
            };
            view.open_requested_browser_tab(
                &target,
                strict,
                &url("http://localhost:3000/"),
                false,
                window,
                cx,
            )
        })
    })
}

#[cfg(unix)]
#[gpui::test]
fn requests_open_tabs_only_in_a_workspace_the_window_shows(cx: &mut gpui::TestAppContext) {
    let (view, cx) = window(cx);
    let socket = view.read_with(cx, |view, _| {
        view.endpoints[0]
            .connection
            .target
            .socket_path()
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    });
    // No named workspace: the one the window shows.
    assert!(matches!(
        request(&view, cx, None, None, true),
        Some(Placed::Opened { workspace_id }) if workspace_id == "w0"
    ));
    assert!(matches!(
        request(&view, cx, None, Some("w1"), true),
        Some(Placed::Opened { workspace_id }) if workspace_id == "w1"
    ));
    assert!(request(&view, cx, None, Some("w_missing"), true).is_none());
    // Another daemon's socket never matches strictly, but its workspace ID
    // still finds the window once socket spellings are ignored.
    assert!(
        request(
            &view,
            cx,
            Some("/elsewhere/herdr-client.sock"),
            Some("w0"),
            true
        )
        .is_none()
    );
    assert!(
        request(
            &view,
            cx,
            Some("/elsewhere/herdr-client.sock"),
            Some("w0"),
            false
        )
        .is_some()
    );
    assert!(request(&view, cx, Some("/elsewhere/herdr-client.sock"), None, false).is_none());
    if let Some(socket) = socket {
        assert!(request(&view, cx, Some(&socket), Some("w0"), true).is_some());
    }
    // Opened without focus: the terminal stays in front.
    draw(cx);
    assert!(cx.debug_bounds("terminal").is_some());
    assert!(cx.debug_bounds("browser-tab-0").is_some());
}

#[gpui::test]
fn tabs_of_a_closed_workspace_are_forgotten_but_a_restart_keeps_them(
    cx: &mut gpui::TestAppContext,
) {
    let (view, cx) = window(cx);
    let tab_scope = view.read_with(cx, |view, _| scope(&view.endpoints[0]));
    let open = |cx: &mut VisualTestContext, workspace: &str| {
        cx.update(|_, cx| {
            Store::update(cx, |store| {
                store.open(tab_scope.clone(), workspace, Some(url("https://a.test/")))
            })
            .unwrap()
        })
    };
    let (kept, closed) = (open(cx, "w0"), open(cx, "w2"));
    let exists =
        |cx: &mut VisualTestContext, id| cx.update(|_, cx| cx.global::<Store>().get(id).is_some());
    let poll = |view: &Entity<HerdrWindow>, cx: &mut VisualTestContext, boot: &str, count| {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let mut next = snapshot(count);
                next.boot_id = boot.into();
                next.focused_workspace_id = Some("w0".into());
                view.live.snapshot = Some(Arc::new(next));
                view.poll_browser(window, cx);
            })
        });
    };
    poll(&view, cx, "boot-1", 3);
    // A daemon that restarted without w2 proves nothing about w2.
    poll(&view, cx, "boot-2", 2);
    assert!(exists(cx, closed));
    poll(&view, cx, "boot-2", 3);
    // The same daemon dropping w2 means it was closed.
    poll(&view, cx, "boot-2", 2);
    assert!(!exists(cx, closed));
    assert!(exists(cx, kept));
}
