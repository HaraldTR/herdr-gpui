#![allow(clippy::unwrap_used)]

use super::{Location, Store, WebUrl, view::scope};
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

fn url(value: &str) -> Location {
    Location::Web {
        url: WebUrl::try_from(value).unwrap(),
    }
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
        assert!(cx.debug_bounds("annotations").is_none());

        // Annotating opens the notes panel beside the page.
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.toggle_annotating(crate::browser::TabId::test(0), window, cx)
            });
        });
        draw(cx);
        assert!(cx.debug_bounds("annotations").is_some());
        assert!(cx.debug_bounds("browser-annotate").is_some());

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

/// Splitting needs pages, so these run where a build shows them. Blank tabs
/// need no native page, so none is created.
#[cfg(any(target_os = "macos", windows))]
mod split {
    use super::*;
    use crate::{
        browser::{Content, Side, TabId},
        controls::Command,
    };

    fn content(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) -> [Content; 2] {
        cx.update(|_, cx| Side::BOTH.map(|side| view.read(cx).side_content(side, cx)))
    }

    fn run(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext, command: Command) {
        cx.update(|window, cx| view.update(cx, |view, cx| view.command(command, window, cx)));
        draw(cx);
    }

    #[gpui::test]
    fn splitting_puts_a_page_beside_the_terminal(cx: &mut gpui::TestAppContext) {
        let (view, cx) = window(cx);
        draw(cx);
        assert!(cx.debug_bounds("right-side").is_none());
        let split = cx.debug_bounds("split-editor").unwrap();
        cx.simulate_click(split.center(), gpui::Modifiers::none());
        draw(cx);
        // With no browser tab yet, the right side opens a blank one.
        let page = TabId::test(0);
        assert_eq!(content(&view, cx), [Content::Terminal, Content::Page(page)]);
        assert!(cx.debug_bounds("terminal").is_some());
        assert!(cx.debug_bounds("right-browser").is_some());
        assert!(cx.debug_bounds("split-divider").is_some());
        // Both strips list the same tabs; only the right one splits.
        for selector in [
            "tab-t0",
            "browser-tab-0",
            "right-tab-t0",
            "right-browser-tab-0",
        ] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector}");
        }
        assert!(cx.debug_bounds("split-editor").is_none());
        assert!(cx.debug_bounds("right-split-editor").is_some());
        assert!(cx.debug_bounds("tab-actions").is_some());
        assert!(cx.debug_bounds("right-tab-actions").is_some());
        let left = cx.debug_bounds("side").unwrap();
        let right = cx.debug_bounds("right-side").unwrap();
        assert!(left.right() <= right.left());

        // Choosing on the right what the left shows swaps the two.
        let terminal_tab = cx.debug_bounds("right-tab-t0").unwrap();
        cx.simulate_click(terminal_tab.center(), gpui::Modifiers::none());
        draw(cx);
        assert_eq!(content(&view, cx), [Content::Page(page), Content::Terminal]);
        assert!(cx.debug_bounds("browser").is_some());
        let terminal = cx.debug_bounds("terminal").unwrap();
        assert!(terminal.left() >= right.left());
        let browser_tab = cx.debug_bounds("right-browser-tab-0").unwrap();
        cx.simulate_click(browser_tab.center(), gpui::Modifiers::none());
        draw(cx);
        assert_eq!(content(&view, cx), [Content::Terminal, Content::Page(page)]);

        // Closing the split closes its right side; the page stays a tab.
        run(&view, cx, Command::ToggleSplitEditor);
        assert!(view.read_with(cx, |view, _| view.split().is_none()));
        assert_eq!(content(&view, cx), [Content::Terminal, Content::Empty]);
        assert!(cx.debug_bounds("right-side").is_none());
        assert!(cx.debug_bounds("terminal").is_some());
        assert!(cx.debug_bounds("browser-tab-0").is_some());

        // Splitting from a page moves it right and brings the terminal back.
        let browser_tab = cx.debug_bounds("browser-tab-0").unwrap();
        cx.simulate_click(browser_tab.center(), gpui::Modifiers::none());
        draw(cx);
        assert_eq!(content(&view, cx), [Content::Page(page), Content::Empty]);
        run(&view, cx, Command::ToggleSplitEditor);
        assert_eq!(content(&view, cx), [Content::Terminal, Content::Page(page)]);
        // A split whose left side has nothing folds into the right one.
        cx.update(|_, cx| view.update(cx, |view, cx| view.show_terminal_on(Side::Right, cx)));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.activate_side(Side::Left, window, cx))
        });
        assert_eq!(content(&view, cx), [Content::Page(page), Content::Terminal]);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.close_browser_tab(page, window, cx))
        });
        assert_eq!(content(&view, cx), [Content::Empty, Content::Terminal]);
        run(&view, cx, Command::CloseTab);
        assert!(view.read_with(cx, |view, _| view.split().is_none()));
        assert_eq!(content(&view, cx), [Content::Terminal, Content::Empty]);
    }

    #[gpui::test]
    fn closing_a_page_moves_its_side_to_a_neighbour(cx: &mut gpui::TestAppContext) {
        let (view, cx) = window(cx);
        draw(cx);
        run(&view, cx, Command::ToggleSplitEditor);
        run(&view, cx, Command::NewBrowserTab);
        run(&view, cx, Command::NewBrowserTab);
        let [first, second, third] = [0, 1, 2].map(TabId::test);
        assert_eq!(
            content(&view, cx),
            [Content::Terminal, Content::Page(third)]
        );
        // The page on the left moves right when the right side asks for it.
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.show_browser_tab_on(Side::Left, second, window, cx)
            })
        });
        assert_eq!(
            content(&view, cx),
            [Content::Page(second), Content::Page(third)]
        );
        view.read_with(cx, |view, _| assert_eq!(view.active_side(), Side::Left));
        // The keyboard follows a press on a side.
        let right = cx.debug_bounds("right-browser").unwrap();
        cx.simulate_mouse_down(
            right.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::none(),
        );
        cx.simulate_mouse_up(
            right.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::none(),
        );
        view.read_with(cx, |view, _| assert_eq!(view.active_side(), Side::Right));

        // The right side's page closes to the one before it the left does
        // not show.
        run(&view, cx, Command::CloseTab);
        assert_eq!(
            content(&view, cx),
            [Content::Page(second), Content::Page(first)]
        );
        run(&view, cx, Command::CloseTab);
        assert_eq!(content(&view, cx), [Content::Page(second), Content::Empty]);
        assert!(cx.debug_bounds("right-empty-side").is_some());
        // The left side's covered terminal comes back when its page closes.
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.close_browser_tab(second, window, cx))
        });
        draw(cx);
        assert_eq!(content(&view, cx), [Content::Terminal, Content::Empty]);
        // Close Tab on an empty side closes the split.
        run(&view, cx, Command::CloseTab);
        assert!(view.read_with(cx, |view, _| view.split().is_none()));
        view.read_with(cx, |view, _| assert!(view.menu.page.is_none()));
        assert!(cx.debug_bounds("terminal").is_some());
    }

    #[gpui::test]
    fn the_divider_drags_within_bounds(cx: &mut gpui::TestAppContext) {
        let (view, cx) = window(cx);
        draw(cx);
        run(&view, cx, Command::ToggleSplitEditor);
        let divider = cx.debug_bounds("split-divider").unwrap();
        let before = cx.debug_bounds("side").unwrap();
        let target = divider.center() - gpui::point(gpui::px(100.), gpui::px(0.));
        cx.simulate_mouse_down(
            divider.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::none(),
        );
        cx.simulate_mouse_move(
            target,
            Some(gpui::MouseButton::Left),
            gpui::Modifiers::none(),
        );
        cx.simulate_mouse_move(
            target,
            Some(gpui::MouseButton::Left),
            gpui::Modifiers::none(),
        );
        cx.simulate_mouse_up(target, gpui::MouseButton::Left, gpui::Modifiers::none());
        draw(cx);
        let after = cx.debug_bounds("side").unwrap();
        assert!(after.size.width < before.size.width - gpui::px(50.));
        view.read_with(cx, |view, _| {
            let ratio = view.split().unwrap().ratio();
            assert!(ratio > 0.1 && ratio < 0.5, "{ratio}");
        });
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
                pane: Some("w0:p1"),
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
    // The same pane showing the same page again got its tab back each time:
    // one tab in w0 and one in w1.
    cx.update(|_, cx| {
        let store = cx.global::<Store>();
        assert_eq!(store.opened_by("w0:p1").count(), 2);
    });
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
                store.open(
                    tab_scope.clone(),
                    workspace,
                    Some(url("https://a.test/")),
                    None,
                )
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

/// Notes need a page to annotate, which Linux builds do not show.
#[cfg(any(target_os = "macos", windows))]
mod notes {
    use super::*;

    const PICK: &str = r##"{"kind":"pick","target":{"kind":"element","selector":"#save","tag":"button","text":"Save","html":"<button id=\"save\">Save</button>"}}"##;

    /// A snapshot where pane `w0:p1` runs an agent with `status`.
    fn with_agent(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext, status: &str) {
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                let mut shown: serde_json::Value =
                    serde_json::to_value(view.live.snapshot.as_deref().unwrap()).unwrap();
                shown["panes"] = serde_json::json!([{
                    "pane_id": "w0:p1", "workspace_id": "w0", "tab_id": "t0", "label": null,
                    "cwd": null, "foreground_cwd": null, "focused": true,
                    "right_click_passthrough": false
                }]);
                shown["agents"] = serde_json::json!([{
                    "pane_id": "w0:p1", "workspace_id": "w0", "tab_id": "t0", "name": "claude",
                    "display_agent": "Claude Code", "agent": "claude", "title": null,
                    "terminal_title": null, "terminal_title_stripped": null,
                    "agent_status": status, "state_change_seq": 0, "state_labels": [],
                    "tokens": [], "focused": true
                }]);
                view.live.snapshot = Some(Arc::new(serde_json::from_value(shown).unwrap()));
            });
        });
    }

    /// Opens a page the way an agent in pane `w0:p1` would, and writes a note on
    /// its Save button.
    fn noted_tab(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) -> crate::browser::Tab {
        let tab_scope = view.read_with(cx, |view, _| scope(&view.endpoints[0]));
        let tab = cx.update(|_, cx| {
            let id = Store::update(cx, |store| {
                store.open(
                    tab_scope,
                    "w0",
                    Some(url("http://localhost:3000/")),
                    Some("w0:p1".into()),
                )
            })
            .unwrap();
            cx.global::<Store>().get(id).cloned().unwrap()
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                // A page's posts are ignored until the user starts annotating.
                view.page_posted(tab.id, PICK, window, cx);
                assert!(!view.browser.annotations.open(tab.id));
                view.toggle_annotating(tab.id, window, cx);
                view.page_posted(tab.id, "not json", window, cx);
                view.page_posted(tab.id, PICK, window, cx);
                let input = view.browser.annotations.input.clone();
                input.update(cx, |input, cx| input.set_text_selected("Make it blue", cx));
                view.add_note(tab.id, window, cx);
                assert_eq!(view.browser.annotations.queued(tab.id), 1);
            });
        });
        tab
    }

    fn kept(cx: &mut VisualTestContext) -> Option<String> {
        cx.update(|_, cx| {
            cx.default_global::<crate::browser::Feedback>()
                .take("w0:p1")
        })
    }

    #[gpui::test]
    fn notes_reach_the_agent_that_opened_the_page(cx: &mut gpui::TestAppContext) {
        let (view, cx) = window(cx);

        // The agent's pane is not in this window: the notes wait for it.
        let tab = noted_tab(&view, cx);
        cx.update(|_, cx| view.update(cx, |view, cx| view.send_notes(&tab, cx)));
        let text = kept(cx).unwrap();
        assert!(text.contains("On <button> at `#save`"), "{text}");
        assert!(text.contains("Note: Make it blue"), "{text}");
        assert!(kept(cx).is_none(), "taken once");

        // An agent waiting in `browser feedback --wait` gets them directly.
        let tab = noted_tab(&view, cx);
        cx.update(|_, cx| {
            cx.default_global::<crate::browser::Feedback>()
                .set_waiting(vec!["w0:p1".into()]);
            view.update(cx, |view, cx| view.send_notes(&tab, cx));
            cx.default_global::<crate::browser::Feedback>()
                .set_waiting(Vec::new());
        });
        assert!(kept(cx).is_some());

        // A working agent's pane is typed into once it is idle; this fixture has
        // no connection, so the paste fails and the notes are kept instead.
        with_agent(&view, cx, "working");
        let tab = noted_tab(&view, cx);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.send_notes(&tab, cx);
                assert_eq!(view.browser.annotations.delivering(), 1);
                view.poll_deliveries(cx);
                assert_eq!(view.browser.annotations.delivering(), 1, "held while busy");
            });
        });
        assert!(kept(cx).is_none());
        with_agent(&view, cx, "idle");
        cx.update(|_, cx| view.update(cx, |view, cx| view.poll_deliveries(cx)));
        view.read_with(cx, |view, _| {
            assert_eq!(view.browser.annotations.delivering(), 0);
            assert_eq!(view.browser.annotations.queued(tab.id), 0);
        });
        assert!(kept(cx).is_some_and(|text| text.contains("Make it blue")));
    }

    #[gpui::test]
    fn a_pane_without_an_agent_is_never_typed_into(cx: &mut gpui::TestAppContext) {
        let (view, cx) = window(cx);
        with_agent(&view, cx, "idle");
        // The agent exited: its pane is back at a shell, where Enter would
        // run the pasted notes.
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                let mut shown = (*view.live.snapshot.clone().unwrap()).clone();
                shown.agents.clear();
                view.live.snapshot = Some(Arc::new(shown));
            });
        });
        let tab = noted_tab(&view, cx);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.send_notes(&tab, cx);
                view.poll_deliveries(cx);
                assert_eq!(view.browser.annotations.delivering(), 0);
            });
        });
        assert!(kept(cx).is_some_and(|text| text.contains("Make it blue")));
    }

    #[gpui::test]
    fn an_agent_asking_a_question_is_not_typed_into(cx: &mut gpui::TestAppContext) {
        let (view, cx) = window(cx);
        with_agent(&view, cx, "blocked");
        let tab = noted_tab(&view, cx);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.send_notes(&tab, cx);
                // Deadline not reached: still held.
                view.poll_deliveries(cx);
                assert_eq!(view.browser.annotations.delivering(), 1);
            });
        });
        // The pane closing sends them to `browser feedback` rather than nowhere.
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                let mut shown = (*view.live.snapshot.clone().unwrap()).clone();
                shown.panes.clear();
                shown.agents.clear();
                view.live.snapshot = Some(Arc::new(shown));
                view.poll_deliveries(cx);
                assert_eq!(view.browser.annotations.delivering(), 0);
            });
        });
        assert!(kept(cx).is_some());
    }
}
