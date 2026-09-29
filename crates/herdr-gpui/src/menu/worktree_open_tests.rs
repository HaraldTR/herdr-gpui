#![allow(clippy::unwrap_used)]

use super::{
    MenuState, Page, WorkspaceAction, WorkspaceMenuAction, WorkspaceTarget, worktree_open::Picker,
};
use crate::{
    sidebar::layout_tests::{fixture_window, snapshot},
    state::ConnectionStatus,
};
use gpui::{AppContext, Modifiers, TestAppContext, px, size};
use serde_json::{Value, json};

fn listing() -> Value {
    json!({"result": {"type": "worktree_list", "source": {
        "repo_key": "/remote/repo/.git", "repo_name": "repo", "source_workspace_id": "w3"
    }, "worktrees": [
        {"path": "/remote/repo", "branch": "main", "label": "parent", "is_bare": false,
         "is_prunable": false, "is_detached": false, "open_workspace_id": "w3"},
        {"path": "/remote/checkout with spaces ", "label": "detached", "is_bare": false,
         "is_prunable": false, "is_detached": true},
        {"path": "/remote/bare", "label": "bare", "is_bare": true,
         "is_prunable": false, "is_detached": false},
        {"path": "/remote/prunable", "label": "prunable", "is_bare": false,
         "is_prunable": true, "is_detached": false}
    ]}})
}

fn pending(menu: &mut MenuState, cx: &mut gpui::App) {
    let picker = menu
        .worktree_open
        .get_or_insert_with(|| Picker::new(cx.new(crate::search_input::SearchInput::new)));
    picker.pending = Some("list".into());
    picker.entries.clear();
    picker.filter(picker.search.read(cx).text());
    menu.error = None;
}

#[gpui::test]
fn open_worktree_listing_is_correlated_atomic_bounded_and_fallible(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let mut menu = MenuState::new(cx);
        pending(&mut menu, cx);
        menu.apply_worktree_list_response("other", Ok(listing()));
        assert!(menu.worktree_open.as_ref().unwrap().entries.is_empty());
        assert!(menu.worktree_open.as_ref().unwrap().pending.is_some());
        menu.apply_worktree_list_response("list", Ok(listing()));
        let picker = menu.worktree_open.as_ref().unwrap();
        assert_eq!(picker.entries.len(), 2);
        assert_eq!(picker.entries[1].path, "/remote/checkout with spaces ");
        assert!(picker.pending.is_none());
        let mut malformed = listing();
        malformed["result"]["worktrees"][1]["is_bare"] = json!("false");
        let mut duplicate = listing();
        duplicate["result"]["worktrees"][1]["path"] = json!("/remote/repo");
        let mut oversized = listing();
        oversized["result"]["worktrees"] =
            json!(vec![listing()["result"]["worktrees"][0].clone(); 513]);
        let mut long_path = listing();
        long_path["result"]["worktrees"][0]["path"] = json!("x".repeat(8193));
        let mut missing_source = listing();
        missing_source["result"]["source"] = Value::Null;
        let mut empty_source_key = listing();
        empty_source_key["result"]["source"]["repo_key"] = json!("");
        let mut oversized_source = listing();
        oversized_source["result"]["source"]["repo_key"] = json!("x".repeat(8193));
        for response in [
            malformed,
            duplicate,
            oversized,
            long_path,
            missing_source,
            empty_source_key,
            oversized_source,
            json!({}),
            json!({"result":{"type":"worktree_list","worktrees":[{}]}}),
            json!({"error":{"code":"denied","message":"repository is untrusted"}}),
        ] {
            pending(&mut menu, cx);
            menu.apply_worktree_list_response("list", Ok(response));
            assert!(menu.error.is_some());
            let picker = menu.worktree_open.as_ref().unwrap();
            assert!(picker.entries.is_empty() && picker.pending.is_none());
        }
        pending(&mut menu, cx);
        menu.apply_worktree_list_response(
            "list",
            Err(std::sync::Arc::new(crate::Error::Client(
                herdr_client::Error::UnsupportedMethod,
            ))),
        );
        assert_eq!(
            menu.error.as_deref(),
            Some("method not advertised by endpoint")
        );
        pending(&mut menu, cx);
        menu.apply_worktree_list_response(
            "list",
            Ok(json!({"result":{"type":"worktree_list","source":listing()["result"]["source"],"worktrees":[]}})),
        );
        assert!(menu.error.is_none());
        assert!(menu.worktree_open.as_ref().unwrap().entries.is_empty());
        pending(&mut menu, cx);
        menu.reset();
        menu.apply_worktree_list_response("list", Ok(listing()));
        assert!(menu.worktree_open.is_none() && menu.error.is_none());
    });
}

#[test]
fn open_worktree_targets_parent_and_keeps_exact_daemon_path() {
    let mut snapshot = snapshot(7);
    snapshot.workspaces[3].worktree = None;
    let target = WorkspaceTarget::new(&snapshot, &snapshot.workspaces[3]);
    let path = "/remote/checkout with spaces ";
    assert_eq!(
        target
            .request(&snapshot, WorkspaceAction::OpenWorktree, path)
            .unwrap(),
        (
            herdr_client::Method::WorktreeOpen,
            json!({"workspace_id":"w3","path":path,"focus":true,"trust_repository":false})
        )
    );
    assert!(
        target
            .request(&snapshot, WorkspaceAction::OpenWorktree, "")
            .is_err()
    );
    assert!(
        WorkspaceTarget::new(&snapshot, &snapshot.workspaces[4])
            .request(&snapshot, WorkspaceAction::OpenWorktree, path)
            .is_err()
    );
    snapshot.workspaces[3].branch = None;
    assert!(
        target
            .request(&snapshot, WorkspaceAction::OpenWorktree, path)
            .is_err()
    );
}

#[gpui::test]
fn open_worktree_picker_menu_keyboard_mouse_and_narrow_layout(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(fixture_window);
    for (width, height) in [(320., 400.), (1000., 800.)] {
        cx.simulate_resize(size(px(width), px(height)));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.dismiss_menu(window, cx);
                view.live.status = ConnectionStatus::Connected;
                view.open_workspace_menu("w3", Default::default(), window, cx);
            });
            window.draw(cx).clear(cx);
        });
        let row = cx.debug_bounds("workspace-menu-Open worktree...").unwrap();
        cx.simulate_click(row.center(), Modifiers::default());
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert_eq!(
                    view.menu.page,
                    Some(Page::Dialog(WorkspaceAction::OpenWorktree))
                );
                assert!(view.menu.input.is_none());
                pending(&mut view.menu, cx);
                view.menu
                    .apply_worktree_list_response("list", Ok(listing()));
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        cx.simulate_keystrokes("down");
        cx.update(|_, cx| {
            assert_eq!(
                view.read(cx).menu.worktree_open.as_ref().unwrap().selected,
                1
            )
        });
        cx.simulate_keystrokes("cmd-n cmd-t");
        cx.update(|_, cx| {
            assert_eq!(
                view.read(cx).menu.page,
                Some(Page::Dialog(WorkspaceAction::OpenWorktree))
            );
            assert!(view.read(cx).menu.error.is_none());
        });
        let panel = cx.debug_bounds("menu-panel").unwrap();
        let submit = cx.debug_bounds("dialog-submit").unwrap();
        let search = cx.debug_bounds("open-worktree-search").unwrap();
        assert!((panel.center().x - px(width / 2.)).abs() <= px(1.));
        assert!((panel.center().y - px(height / 2.)).abs() <= px(1.));
        assert!(panel.contains(&search.origin) && search.right() <= panel.right());
        assert!(panel.left() >= px(0.) && panel.right() <= px(width));
        assert!(panel.bottom() <= px(height) && submit.bottom() <= panel.bottom());
        let row = cx.debug_bounds("open-worktree-row-0").unwrap();
        cx.simulate_click(row.center(), Modifiers::default());
        cx.update(|_, cx| {
            let view = view.read(cx);
            assert_eq!(view.menu.worktree_open.as_ref().unwrap().selected, 0);
            // The disconnected fixture cannot queue, and leaves a visible error.
            assert!(view.menu.error.is_some());
            assert!(view.menu.creation.is_none());
        });
        cx.simulate_keystrokes("escape");
        cx.update(|_, cx| assert!(view.read(cx).menu.page.is_none()));
    }
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            for id in ["w3", "w4"] {
                view.open_workspace_menu(id, Default::default(), window, cx);
                let actions = view.workspace_menu_actions();
                assert_eq!(
                    actions.contains(&WorkspaceMenuAction::Dialog(WorkspaceAction::NewWorktree)),
                    actions.contains(&WorkspaceMenuAction::Dialog(WorkspaceAction::OpenWorktree))
                );
                view.dismiss_menu(window, cx);
            }
        });
    });
}

#[gpui::test]
fn open_worktree_search_filters_automatically_and_preserves_path_selection(
    cx: &mut TestAppContext,
) {
    let (view, cx) = cx.add_window_view(fixture_window);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.live.status = ConnectionStatus::Connected;
            view.open_workspace_menu("w3", gpui::point(px(750.), px(550.)), window, cx);
            view.open_workspace_dialog(WorkspaceAction::OpenWorktree, window, cx);
            pending(&mut view.menu, cx);
            assert!(
                view.menu
                    .worktree_open
                    .as_ref()
                    .unwrap()
                    .search
                    .read(cx)
                    .focus
                    .is_focused(window)
            );
        });
        window.draw(cx).clear(cx);
    });
    // Queries typed before the asynchronous listing arrives apply to that listing.
    cx.simulate_input("SPACES");
    cx.run_until_parked();
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            let mut response = listing();
            response["result"]["worktrees"][1]["label"] = json!("D\u{e9}tached");
            view.menu.apply_worktree_list_response("list", Ok(response));
            assert_eq!(view.menu.worktree_open.as_ref().unwrap().filtered, [1]);
            cx.notify();
        });
    });
    for (query, indices) in [
        ("MAIN", vec![0]),
        ("parent", vec![0]),
        ("spaces", vec![1]),
        ("D\u{c9}T", vec![1]),
        ("no such checkout", vec![]),
        ("", vec![0, 1]),
    ] {
        cx.simulate_keystrokes("cmd-a backspace");
        cx.simulate_input(query);
        cx.run_until_parked();
        cx.update(|_, cx| {
            let picker = view.read(cx).menu.worktree_open.as_ref().unwrap();
            assert_eq!(picker.filtered, indices, "{query}");
            assert_eq!(picker.selected, 0);
        });
        if indices.is_empty() {
            for _ in 0..2 {
                cx.update(|window, cx| window.draw(cx).clear(cx));
            }
            assert!(cx.debug_bounds("open-worktree-no-matches").is_some());
            cx.update(|_, cx| {
                assert!(
                    !view
                        .read(cx)
                        .menu
                        .worktree_open
                        .as_ref()
                        .unwrap()
                        .entries
                        .is_empty()
                )
            });
            cx.simulate_keystrokes("up down enter");
            cx.update(|_, cx| assert!(view.read(cx).menu.creation.is_none()));
        }
    }
    cx.simulate_keystrokes("down");
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx).menu.worktree_open.as_ref().unwrap().selected,
            1
        )
    });
    cx.simulate_input("SPACES");
    cx.run_until_parked();
    cx.simulate_keystrokes("up down");
    cx.update(|_, cx| {
        let view = view.read(cx);
        let picker = view.menu.worktree_open.as_ref().unwrap();
        assert_eq!(picker.selected, 0);
        assert_eq!(
            picker.entry(picker.selected).unwrap().path,
            "/remote/checkout with spaces "
        );
        let (_, params) = view
            .menu
            .target
            .as_ref()
            .unwrap()
            .request(
                view.live.snapshot.as_ref().unwrap(),
                WorkspaceAction::OpenWorktree,
                &picker.entry(picker.selected).unwrap().path,
            )
            .unwrap();
        assert_eq!(params["path"], "/remote/checkout with spaces ");
    });
    cx.simulate_keystrokes("enter");
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx).menu.error.as_deref(),
            Some("Not connected to a daemon.")
        )
    });
}

#[gpui::test]
fn open_worktree_search_composition_and_reopen_are_isolated(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler;
    let (view, cx) = cx.add_window_view(fixture_window);
    let search = cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.live.status = ConnectionStatus::Connected;
            view.open_workspace_menu("w3", Default::default(), window, cx);
            view.open_workspace_dialog(WorkspaceAction::OpenWorktree, window, cx);
            pending(&mut view.menu, cx);
            view.menu
                .apply_worktree_list_response("list", Ok(listing()));
            view.menu.worktree_open.as_ref().unwrap().search.clone()
        })
    });
    cx.update(|window, cx| {
        search.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "remote", Some(0..6), window, cx)
        });
        window.draw(cx).clear(cx);
    });
    // Each key starts mid-composition; the test platform simulates Enter committing it.
    for key in ["up", "down", "enter", "escape", "cmd-n", "cmd-t"] {
        cx.update(|window, cx| {
            search.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(
                    Some(0..input.text().encode_utf16().count()),
                    "remote",
                    Some(0..6),
                    window,
                    cx,
                )
            });
        });
        cx.simulate_keystrokes(key);
        cx.update(|_, cx| {
            assert_eq!(
                view.read(cx).menu.page,
                Some(Page::Dialog(WorkspaceAction::OpenWorktree)),
                "{key}"
            )
        });
    }
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            assert_eq!(
                view.menu.page,
                Some(Page::Dialog(WorkspaceAction::OpenWorktree))
            );
            assert_eq!(view.menu.worktree_open.as_ref().unwrap().selected, 0);
            assert!(search.read(cx).is_composing());
            view.submit_workspace_dialog(window, cx);
            assert!(view.menu.creation.is_none() && view.menu.error.is_none());
            assert!(view.marked.is_empty());
            assert!(view.menu.input.is_none());
        });
        search.update(cx, |input, cx| {
            input.replace_text_in_range(None, "remote", window, cx)
        });
    });
    cx.simulate_keystrokes("down");
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx).menu.worktree_open.as_ref().unwrap().selected,
            1
        )
    });
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            assert!(view.menu.page.is_none());
            assert!(view.focus.is_focused(window));
            view.open_workspace_menu("w3", Default::default(), window, cx);
            view.open_workspace_dialog(WorkspaceAction::OpenWorktree, window, cx);
            pending(&mut view.menu, cx);
            view.menu
                .apply_worktree_list_response("list", Ok(listing()));
        });
        // The old input entity is alive in this test, but its subscription is not.
        search.update(cx, |input, cx| {
            input.replace_text_in_range(Some(0..6), "spaces", window, cx)
        });
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        let view = view.read(cx);
        let picker = view.menu.worktree_open.as_ref().unwrap();
        assert!(picker.search.read(cx).text().is_empty());
        assert!(picker.search.read(cx).focus.is_focused(window));
        assert_eq!(picker.filtered, [0, 1]);
    });
}

#[gpui::test]
fn open_worktree_search_keeps_a_bounded_scrollable_centered_viewport(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(fixture_window);
    for (width, height) in [(320., 400.), (1000., 800.)] {
        cx.simulate_resize(size(px(width), px(height)));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.dismiss_menu(window, cx);
                view.live.status = ConnectionStatus::Connected;
                view.open_workspace_menu("w3", gpui::point(px(15.), px(20.)), window, cx);
                view.open_workspace_dialog(WorkspaceAction::OpenWorktree, window, cx);
                pending(&mut view.menu, cx);
                let entries: Vec<_> = (0..100)
                    .map(|index| {
                        json!({
                            "path": format!("/remote/{index}/{}", "long path ".repeat(40)),
                            "label": "long label ".repeat(40), "branch": format!("branch-{index}"),
                            "is_bare":false, "is_prunable":false, "is_detached":false
                        })
                    })
                    .collect();
                view.menu.apply_worktree_list_response(
                    "list",
                    Ok(json!({"result":{
                        "type":"worktree_list", "source":listing()["result"]["source"], "worktrees":entries
                    }})),
                );
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        let panel = cx.debug_bounds("menu-panel").unwrap();
        let search = cx.debug_bounds("open-worktree-search").unwrap();
        let list = cx.debug_bounds("open-worktree-list").unwrap();
        let footer = cx.debug_bounds("dialog-footer").unwrap();
        assert!((panel.center().x - px(width / 2.)).abs() <= px(1.));
        assert!((panel.center().y - px(height / 2.)).abs() <= px(1.));
        // Up wraps to the final filtered entry and scrolls it into view.
        cx.simulate_keystrokes("up");
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let last = cx.debug_bounds("open-worktree-row-99").unwrap();
        // Uniform-list scroll offsets round to physical pixels.
        assert!(
            last.top() >= search.bottom() - px(1.) && last.bottom() <= list.bottom() + px(1.),
            "last={last:?}, search={search:?}, list={list:?}"
        );
        assert!(last.left() >= panel.left() && last.right() <= panel.right());
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.menu.error = Some("Long endpoint error. ".repeat(100));
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert_eq!(cx.debug_bounds("open-worktree-search").unwrap(), search);
        let status = cx.debug_bounds("open-worktree-status").unwrap();
        assert_eq!(cx.debug_bounds("dialog-footer").unwrap(), footer);
        let submit = cx.debug_bounds("dialog-submit").unwrap();
        assert!(status.bottom() <= submit.top() && submit.bottom() <= panel.bottom());
    }
}

#[gpui::test]
fn open_worktree_rows_fill_the_list_and_header_escape_dismisses(cx: &mut TestAppContext) {
    for width in [320., 1000.] {
        let (view, cx) = cx.add_window_view(fixture_window);
        cx.simulate_resize(size(px(width), px(600.)));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.live.status = ConnectionStatus::Connected;
                view.open_workspace_menu("w3", Default::default(), window, cx);
                view.open_workspace_dialog(WorkspaceAction::OpenWorktree, window, cx);
                pending(&mut view.menu, cx);
                let mut response = listing();
                response["result"]["worktrees"][0]["path"] = json!("/a");
                response["result"]["worktrees"][0]["branch"] = json!("a");
                response["result"]["worktrees"][1]["path"] =
                    json!(format!("/b/{}", "long path/".repeat(100)));
                response["result"]["worktrees"][1]["label"] = json!("long branch ".repeat(100));
                view.menu.apply_worktree_list_response("list", Ok(response));
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        let panel = cx.debug_bounds("menu-panel").unwrap();
        let list = cx.debug_bounds("open-worktree-list").unwrap();
        let footer = cx.debug_bounds("dialog-footer").unwrap();
        // The idle picker spends all space below search on rows, not a hint block.
        assert!(cx.debug_bounds("open-worktree-status").is_none());
        assert!((list.bottom() - footer.top()).abs() <= px(1.));
        assert_eq!(
            list.size.width,
            cx.debug_bounds("open-worktree-search").unwrap().size.width
        );
        let mut status_right = None;
        for (row, status) in [
            ("open-worktree-row-0", "open-worktree-row-status-0"),
            ("open-worktree-row-1", "open-worktree-row-status-1"),
        ] {
            let row = cx.debug_bounds(row).unwrap();
            let status = cx.debug_bounds(status).unwrap();
            assert_eq!(row.left(), list.left());
            assert_eq!(row.size.width, list.size.width);
            assert_eq!(status.right(), row.right() - px(16.));
            assert!(row.contains(&status.origin) && status.bottom() <= row.bottom());
            if let Some(right) = status_right {
                assert_eq!(status.right(), right);
            }
            status_right = Some(status.right());
        }
        let header = cx.debug_bounds("dialog-header").unwrap();
        let title = cx.debug_bounds("dialog-title").unwrap();
        let escape = cx.debug_bounds("open-worktree-escape").unwrap();
        assert!(title.right() <= escape.left());
        assert_eq!(escape.right(), header.right() - px(16.));
        assert!(header.contains(&escape.origin) && escape.bottom() <= header.bottom());
        assert!(cx.debug_bounds("dialog-cancel").is_some());
        assert!(cx.debug_bounds("dialog-submit").is_some());
        // Errors and pending work still have a bounded status area above the footer.
        for waiting in [false, true] {
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.menu.error = (!waiting).then(|| "Endpoint error. ".repeat(100));
                    view.menu.creation = waiting.then(|| "open".into());
                    cx.notify();
                });
                window.draw(cx).clear(cx);
            });
            let status = cx.debug_bounds("open-worktree-status").unwrap();
            let diagnostic = cx
                .debug_bounds(if waiting {
                    "dialog-waiting"
                } else {
                    "dialog-error"
                })
                .unwrap();
            assert!(diagnostic.top() >= status.top());
            assert!(status.size.height > px(0.) && status.bottom() <= footer.top());
            assert_eq!(cx.debug_bounds("menu-panel").unwrap(), panel);
            assert_eq!(cx.debug_bounds("dialog-footer").unwrap(), footer);
            assert_eq!(cx.debug_bounds("open-worktree-escape").unwrap(), escape);
        }
        cx.simulate_click(escape.center(), Modifiers::default());
        cx.update(|window, cx| {
            let view = view.read(cx);
            assert!(view.menu.page.is_none());
            assert!(view.menu.worktree_open.is_none() && view.menu.creation.is_none());
            assert!(view.focus.is_focused(window));
            assert!(view.pending_navigation.is_none());
        });
    }
}

#[gpui::test]
fn open_worktree_replies_cannot_revive_dismissed_or_stale_pickers(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(fixture_window);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            let original = view.live.snapshot.clone();
            for (change, opening) in (0..7).flat_map(|change| [false, true].map(|opening| (change, opening))) {
                view.live.snapshot = original.clone();
                view.live.status = ConnectionStatus::Connected;
                view.open_workspace_menu("w3", Default::default(), window, cx);
                view.open_workspace_dialog(WorkspaceAction::OpenWorktree, window, cx);
                pending(&mut view.menu, cx);
                view.live.dialog_response = Some(("list".into(), Some(Ok(listing()))));
                if opening {
                    view.menu.apply_worktree_list_response("list", Ok(listing()));
                    view.menu.creation = Some("open".into());
                    view.live.dialog_response = Some(("open".into(), Some(Ok(json!({"result":{
                        "type":"worktree_opened", "workspace":{"workspace_id":"w6"}
                    }})))));
                }
                match change {
                    0 => view.dismiss_menu(window, cx),
                    1 => view.endpoints[0].generation += 1,
                    2 => view.selection_epoch += 1,
                    3 => std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap()).boot_id = "replacement".into(),
                    4 => std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap()).workspaces.retain(|w| w.workspace_id != "w3"),
                    5 => view.live.status = ConnectionStatus::Disconnected,
                    _ => std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap()).workspaces[3].worktree = None,
                }
                view.update_workspace_dialog(window, cx);
                assert!(view.menu.page.is_none());
                assert!(view.menu.worktree_open.is_none());
                assert!(view.pending_navigation.is_none());
            }
            view.live.snapshot = original;
            view.live.status = ConnectionStatus::Connected;
            view.open_workspace_menu("w3", Default::default(), window, cx);
            view.open_workspace_dialog(WorkspaceAction::OpenWorktree, window, cx);
            pending(&mut view.menu, cx);
            view.menu.apply_worktree_list_response("list", Ok(listing()));
            for response in [json!({"error":{"code":"open_failed","message":"checkout vanished"}}),
                json!({"result":{"type":"worktree_created","workspace":{"workspace_id":"wrong"}}}),
                json!({"result":{"type":"worktree_opened","workspace":{}}})] {
                view.menu.creation = Some("open".into());
                view.live.dialog_response = Some(("open".into(), Some(Ok(response))));
                view.update_workspace_dialog(window, cx);
                assert!(view.menu.error.is_some());
                assert!(view.menu.creation.is_none());
                assert!(view.pending_navigation.is_none());
            }
            view.menu.creation = Some("open".into());
            let opened = json!({"result":{"type":"worktree_opened","workspace":{"workspace_id":"w6"},"already_open":true}});
            view.live.dialog_response = Some(("old-open".into(), Some(Ok(opened.clone()))));
            view.update_workspace_dialog(window, cx);
            assert_eq!(view.menu.creation.as_deref(), Some("open"));
            view.collapsed_repos.insert(crate::sidebar::layout_tests::REPO_KEY.into());
            view.live.dialog_response = Some(("open".into(), Some(Ok(opened))));
            view.update_workspace_dialog(window, cx);
            assert!(view.menu.page.is_none());
            assert!(view.collapsed_repos.is_empty());
            assert_eq!(view.pending_navigation, Some(crate::NavigationTarget::Workspace("w6".into())));
        });
    });
}

#[gpui::test]
fn open_worktree_parent_establishment_requires_pending_source_identity_and_fences(
    cx: &mut TestAppContext,
) {
    use herdr_client::protocol::ClientShellWorktree;
    let (view, cx) = cx.add_window_view(fixture_window);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            let original = view.live.snapshot.clone();
            for change in [
                "matching",
                "not-pending",
                "repo",
                "label",
                "linked",
                "source-workspace",
                "branch",
                "boot",
                "removed",
                "generation",
                "selection",
                "disconnected",
                "existing",
            ] {
                view.menu.reset();
                view.pending_navigation = None;
                view.live.snapshot = original.clone();
                view.live.status = ConnectionStatus::Connected;
                if change != "existing" {
                    std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap()).workspaces[3]
                        .worktree = None;
                }
                view.open_workspace_menu("w3", Default::default(), window, cx);
                view.open_workspace_dialog(WorkspaceAction::OpenWorktree, window, cx);
                pending(&mut view.menu, cx);
                let mut response = listing();
                if change == "source-workspace" {
                    response["result"]["source"]["source_workspace_id"] = json!("other");
                }
                view.menu.apply_worktree_list_response("list", Ok(response));
                assert!(view.menu.error.is_none());
                if change != "not-pending" {
                    view.menu.creation = Some("open".into());
                }
                let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
                let mut membership = ClientShellWorktree {
                    key: "/remote/repo/.git".into(),
                    label: "repo".into(),
                    is_linked_worktree: false,
                };
                match change {
                    "repo" => membership.key = "/other/repo/.git".into(),
                    "label" => membership.label = "other".into(),
                    "linked" => membership.is_linked_worktree = true,
                    "branch" => snapshot.workspaces[3].branch = Some("different".into()),
                    "boot" => snapshot.boot_id = "restarted".into(),
                    _ => {}
                }
                snapshot.workspaces[3].worktree = Some(membership);
                if change == "removed" {
                    snapshot.workspaces.remove(3);
                }
                match change {
                    "generation" => view.endpoints[0].generation += 1,
                    "selection" => view.selection_epoch += 1,
                    "disconnected" => view.live.status = ConnectionStatus::Disconnected,
                    _ => {}
                }
                // A snapshot delivered before the response must retain the pending
                // open, but an unrelated response must still not navigate.
                let opened =
                    json!({"result":{"type":"worktree_opened","workspace":{"workspace_id":"w6"}}});
                view.live.dialog_response = Some(("unrelated".into(), Some(Ok(opened.clone()))));
                view.update_workspace_dialog(window, cx);
                assert!(view.pending_navigation.is_none(), "{change}");
                if change == "matching" {
                    assert_eq!(view.menu.creation.as_deref(), Some("open"));
                    view.live.dialog_response = Some(("open".into(), Some(Ok(opened))));
                    view.update_workspace_dialog(window, cx);
                    assert_eq!(
                        view.pending_navigation,
                        Some(crate::NavigationTarget::Workspace("w6".into()))
                    );
                }
                assert!(view.menu.page.is_none(), "{change}");
            }
        });
    });
}

#[gpui::test]
fn open_worktree_branch_only_parent_membership_preserves_correlated_navigation(
    cx: &mut TestAppContext,
) {
    use gpui::AppContext;
    use herdr_client::{
        ClientEvent, ConnectOptions, ConnectTarget, Method, Stream, connect_with_connector,
        protocol::{endpoint::*, *},
    };
    use std::time::Duration;
    let (stream, mut server) = Stream::pair().unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    server
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let client = connect_with_connector(
        ConnectTarget::Socket("/unused".into()),
        ConnectOptions::default(),
        true,
        move |_, _| Ok(stream),
    )
    .unwrap();
    assert!(matches!(
        read_message(&mut server, MAX_FRAME_SIZE).unwrap(),
        ClientMessage::EndpointControl { .. }
    ));
    let mut welcome: Value = serde_json::from_str(include_str!(
        "../../../herdr-protocol/tests/fixtures/endpoint-welcome-v1.json"
    ))
    .unwrap();
    welcome["methods"] = json!(["worktree.list", "worktree.open", "workspace.focus"]);
    let snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(
        "../../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
    ))
    .unwrap();
    for (kind, data) in [
        (ENDPOINT_WELCOME_KIND, welcome.to_string()),
        (
            ENDPOINT_SNAPSHOT_KIND,
            serde_json::to_string(&snapshot).unwrap(),
        ),
    ] {
        write_message(
            &mut server,
            &ServerMessage::EndpointControl {
                kind: kind.into(),
                data,
            },
            MAX_GRAPHICS_FRAME_SIZE,
        )
        .unwrap();
    }
    let connected = client.events.recv_timeout(Duration::from_secs(3)).unwrap();
    let snapshot_event = client.events.recv_timeout(Duration::from_secs(3)).unwrap();
    // No terminal render tree: it would enqueue unrelated resize requests.
    struct Fixture(gpui::Entity<crate::HerdrWindow>);
    impl gpui::Render for Fixture {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
        }
    }
    let (fixture, cx) =
        cx.add_window_view(|window, cx| Fixture(cx.new(|cx| fixture_window(window, cx))));
    let view = fixture.update(cx, |fixture, _| fixture.0.clone());
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.live.apply(connected);
            view.live.apply(snapshot_event);
            std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap()).workspaces =
                crate::sidebar::layout_tests::snapshot(7).workspaces;
            std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap()).workspaces[3].worktree =
                None;
            view.endpoints[0].connection.handle = Some(client.handle.clone());
            *view.endpoints[0].connection.inbox.lock().unwrap() = view.live.clone();
            view.open_workspace_menu("w3", Default::default(), window, cx);
            view.open_workspace_dialog(WorkspaceAction::OpenWorktree, window, cx);
            assert!(view.menu.error.is_none());
            assert!(view.menu.worktree_open.as_ref().unwrap().pending.is_some());
            view.collapsed_repos.insert("/remote/repo/.git".into());
        });
    });
    for (method, response) in [
        (Method::WorktreeList, listing()),
        (
            Method::WorktreeOpen,
            json!({"result":{"type":"worktree_opened","workspace":{"workspace_id":"returned-workspace"}}}),
        ),
    ] {
        let ClientMessage::ClientShellEndpointRequest { request, .. } =
            read_message(&mut server, MAX_FRAME_SIZE).unwrap()
        else {
            panic!("expected worktree request")
        };
        let request: Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["method"], method.as_str());
        let mut params = json!({"workspace_id":"w3","trust_repository":false});
        if method == Method::WorktreeOpen {
            params["path"] = json!("/remote/checkout with spaces ");
            params["focus"] = json!(true);
        }
        assert_eq!(request["params"], params);
        let id = request["id"].as_str().unwrap();
        let mut response = response;
        response["id"] = json!(id);
        write_message(
            &mut server,
            &ServerMessage::ClientShellEndpointResponseChunk {
                boot_id: snapshot.boot_id.clone(),
                request_id: id.into(),
                final_chunk: true,
                data: serde_json::to_vec(&response).unwrap(),
            },
            MAX_GRAPHICS_FRAME_SIZE,
        )
        .unwrap();
        let event = client.events.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(matches!(event, ClientEvent::Response { .. }));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                if method == Method::WorktreeOpen {
                    // worktree.open establishes membership before its success response.
                    // The UI mailbox can coalesce both into one update.
                    let mut established = (**view.live.snapshot.as_ref().unwrap()).clone();
                    established.revision += 1;
                    established.workspaces[3].worktree = Some(ClientShellWorktree {
                        key: "/remote/repo/.git".into(),
                        label: "repo".into(),
                        is_linked_worktree: false,
                    });
                    view.endpoints[0]
                        .connection
                        .inbox
                        .lock()
                        .unwrap()
                        .apply(ClientEvent::Snapshot(std::sync::Arc::new(established)));
                }
                view.endpoints[0]
                    .connection
                    .inbox
                    .lock()
                    .unwrap()
                    .apply(event);
                view.live = view.endpoints[0].connection.take_update().unwrap();
                view.update_workspace_dialog(window, cx);
                if method == Method::WorktreeList {
                    let picker = view.menu.worktree_open.as_mut().unwrap();
                    assert_eq!(picker.entries.len(), 2);
                    picker.filter("SPACES");
                    assert_eq!(picker.filtered, [1]);
                    assert_eq!(picker.selected, 0);
                    view.submit_workspace_dialog(window, cx);
                    let pending = view.menu.creation.clone();
                    assert!(pending.is_some());
                    view.submit_workspace_dialog(window, cx);
                    assert_eq!(view.menu.creation, pending);
                } else {
                    assert!(view.menu.page.is_none());
                    assert!(!view.collapsed_repos.contains("/remote/repo/.git"));
                    assert_eq!(
                        view.pending_navigation,
                        Some(crate::NavigationTarget::Workspace(
                            "returned-workspace".into()
                        ))
                    );
                }
            });
        });
    }
    // FIFO marker proves duplicate submission did not queue a second open.
    client.handle.set_focus(&snapshot.boot_id, false).unwrap();
    assert!(matches!(
        read_message(&mut server, MAX_FRAME_SIZE).unwrap(),
        ClientMessage::ClientShellFocus { focused: false }
    ));
    client.handle.disconnect();
}
