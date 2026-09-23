#![allow(clippy::unwrap_used)]

use super::{
    Page, WorkspaceAction,
    worktree_source::{Tab, item_request},
};
use crate::{
    HerdrWindow,
    repo_items::{Item, Kind, Origin},
    sidebar,
};
use gpui::{Entity, VisualTestContext};

fn origin() -> Origin {
    Origin {
        owner: "penso".into(),
        repo: "herdr-gpui".into(),
    }
}

fn items() -> Vec<Item> {
    vec![
        Item {
            kind: Kind::PullRequest,
            number: 48,
            title: "Centre the worktree dialog".into(),
            url: "https://github.com/penso/herdr-gpui/pull/48".into(),
            author: "penso".into(),
            head: Some("worktree/rapid-forest".into()),
            fork_owner: None,
            draft: false,
        },
        Item {
            kind: Kind::PullRequest,
            number: 51,
            title: "Fork contribution".into(),
            url: "https://github.com/penso/herdr-gpui/pull/51".into(),
            author: "outsider".into(),
            head: Some("patch-1".into()),
            fork_owner: Some("outsider".into()),
            draft: true,
        },
        Item {
            kind: Kind::Issue,
            number: 1255,
            title: "bug: agent end message is empty".into(),
            url: "https://github.com/penso/herdr-gpui/issues/1255".into(),
            author: "penso".into(),
            head: None,
            fork_owner: None,
            draft: false,
        },
    ]
}

/// Open the new worktree dialog on the repository workspace, already on `tab`
/// and with a listing in hand, so the tabs never reach GitHub or the disk.
fn open_dialog(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext, connected: bool, tab: Tab) {
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
            snapshot.workspaces = sidebar::layout_tests::snapshot(7).workspaces;
            view.live.status = crate::state::ConnectionStatus::Connected;
            // The listing reads a checkout, so it is owned-local-socket only.
            view.live.local_daemon_peer = true;
            view.menu.reset();
            view.menu.github = if connected {
                crate::github::Auth::connected_fixture()
            } else {
                Default::default()
            };
            view.open_workspace_menu("w3", Default::default(), window, cx);
            view.open_workspace_dialog(WorkspaceAction::NewWorktree, window, cx);
            if connected {
                let source = view.menu.worktree.as_mut().unwrap();
                source.install(origin(), items());
                view.select_worktree_tab(tab, window, cx);
            }
        });
    });
    draw(cx);
}

/// GPUI double-buffers frames and their debug bounds, so a selector that was
/// painted two frames ago is still readable. Draw both buffers before asking
/// whether something is on screen.
fn draw(cx: &mut VisualTestContext) {
    for _ in 0..2 {
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
    }
}

fn tab(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) -> Tab {
    cx.update(|_, cx| view.read(cx).menu.worktree.as_ref().unwrap().tab)
}

/// Neither listing exists without an account, so those tabs do not move and
/// the dialog stays the branch form it has always been.
#[gpui::test]
fn github_tabs_are_inert_until_an_account_is_connected(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(sidebar::layout_tests::fixture_window);
    cx.simulate_resize(gpui::size(gpui::px(900.), gpui::px(700.)));
    open_dialog(&view, cx, false, Tab::Branch);
    for selector in ["worktree-tab-new", "worktree-tab-PR", "worktree-tab-issues"] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector}");
    }
    assert!(cx.debug_bounds("worktree-tabs-note").is_some());
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.select_worktree_tab(Tab::Items(Kind::PullRequest), window, cx);
        });
    });
    draw(cx);
    assert_eq!(tab(&view, cx), Tab::Branch);
    // The branch field and its checkout preview are still what the dialog
    // shows, and no listing was requested for an account that does not exist.
    assert!(cx.debug_bounds("dialog-input").is_some());
    assert!(cx.debug_bounds("dialog-checkout").is_some());
    assert!(cx.debug_bounds("worktree-row-0").is_none());
    cx.update(|_, cx| {
        let source = view.read(cx).menu.worktree.as_ref().unwrap();
        assert!(!source.lookup.loading && source.lookup.message.is_none());
    });
}

/// With an account, a GitHub tab replaces the branch form with its listing.
#[gpui::test]
fn a_connected_account_turns_the_dialog_into_a_picker(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(sidebar::layout_tests::fixture_window);
    cx.simulate_resize(gpui::size(gpui::px(900.), gpui::px(700.)));
    open_dialog(&view, cx, true, Tab::Items(Kind::Issue));
    assert!(cx.debug_bounds("worktree-tabs-note").is_none());
    assert_eq!(tab(&view, cx), Tab::Items(Kind::Issue));
    assert!(cx.debug_bounds("worktree-row-0").is_some());
    assert!(cx.debug_bounds("worktree-status").is_some());
    // A picked row creates its own checkout, so this tab has no branch to type
    // and no submit button; cancelling is still the way out.
    assert!(cx.debug_bounds("dialog-input").is_none());
    assert!(cx.debug_bounds("dialog-submit").is_none());
    assert!(cx.debug_bounds("dialog-cancel").is_some());
    // Rows stay inside the panel rather than growing it past the window.
    let panel = cx.debug_bounds("menu-panel").unwrap();
    let row = cx.debug_bounds("worktree-row-0").unwrap();
    let status = cx.debug_bounds("worktree-status").unwrap();
    assert!(panel.contains(&row.origin) && row.right() <= panel.right());
    assert!(row.bottom() <= status.top());
    assert!(status.bottom() <= panel.bottom());
    assert!(panel.bottom() <= gpui::px(700.));
}

/// Each tab lists only its own kind, the search narrows it as it is typed, and
/// none of that typing reaches the branch draft behind the tab.
#[gpui::test]
fn searching_a_listing_filters_its_own_rows_only(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(sidebar::layout_tests::fixture_window);
    cx.simulate_resize(gpui::size(gpui::px(900.), gpui::px(700.)));
    open_dialog(&view, cx, true, Tab::Branch);
    let branch = cx.update(|_, cx| view.read(cx).menu.input.as_ref().unwrap().text.clone());

    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.select_worktree_tab(Tab::Items(Kind::PullRequest), window, cx);
        });
    });
    draw(cx);
    let numbers = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            let source = view.read(cx).menu.worktree.as_ref().unwrap();
            (0..source.filtered.len())
                .filter_map(|row| source.item(row).map(|item| item.number))
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(numbers(cx), vec![48, 51]);

    cx.simulate_input("fork");
    draw(cx);
    assert_eq!(numbers(cx), vec![51]);
    // Searching an author works as well as searching a title or a number.
    cx.update(|_, cx| {
        let search = view.read(cx).menu.worktree.as_ref().unwrap().search.clone();
        search.update(cx, |input, cx| input.clear(cx));
    });
    cx.simulate_input("penso");
    draw(cx);
    assert_eq!(numbers(cx), vec![48]);

    // The issue tab shares the one listing but shows only issues.
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.select_worktree_tab(Tab::Items(Kind::Issue), window, cx);
        });
    });
    draw(cx);
    assert_eq!(numbers(cx), vec![1255]);

    // Everything typed went to the search field, not the branch behind it.
    assert_eq!(
        cx.update(|_, cx| view.read(cx).menu.input.as_ref().unwrap().text.clone()),
        branch
    );
}

/// What a picked row asks the daemon for: a pull request checks its own head
/// branch out, an issue gets a new branch named after it, and both name the
/// checkout for what it is for.
#[gpui::test]
fn a_picked_row_names_the_branch_base_and_label_it_creates(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(sidebar::layout_tests::fixture_window);
    open_dialog(&view, cx, true, Tab::Items(Kind::PullRequest));
    cx.update(|_, cx| {
        let view = view.read(cx);
        let target = view.menu.target.as_ref().unwrap();
        let snapshot = view.live.snapshot.as_ref().unwrap();
        let items = items();

        let (method, params) = item_request(target, snapshot, &items[0]).unwrap();
        assert_eq!(method, herdr_client::Method::WorktreeCreate);
        assert_eq!(params["branch"], "worktree/rapid-forest");
        // An existing head may live only on the remote, so the base names the
        // tracking ref rather than the source workspace's HEAD.
        assert_eq!(params["base"], "origin/worktree/rapid-forest");
        assert_eq!(params["label"], "#48 Centre the worktree dialog");
        assert_eq!(params["trust_repository"], false);

        let (_, params) = item_request(target, snapshot, &items[2]).unwrap();
        assert_eq!(params["branch"], "1255-bug-agent-end-message-is-empty");
        // An issue's branch is new, so it starts from the dialog's own base.
        assert_eq!(params["base"], "HEAD");
        assert_eq!(params["label"], "#1255 bug: agent end message is empty");
    });
}

/// Picking a row is the whole action, and it is the only one running: a fork is
/// refused before the daemon is asked for anything, a pull request holds the
/// dialog while its branch is fetched, and a refusal releases the row again.
#[gpui::test]
fn picking_a_row_creates_its_checkout(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(sidebar::layout_tests::fixture_window);
    cx.simulate_resize(gpui::size(gpui::px(900.), gpui::px(700.)));
    open_dialog(&view, cx, true, Tab::Items(Kind::PullRequest));
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            // The fork row says why it cannot be picked instead of failing once
            // the daemon is asked for a branch `origin` does not carry.
            view.create_from_repo_item(1, cx);
            assert!(
                view.menu
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("comes from the fork outsider"),
                "{:?}",
                view.menu.error
            );
            assert!(view.menu.creation.is_none());
            assert!(view.menu.worktree.as_ref().unwrap().pending.is_none());

            view.create_from_repo_item(0, cx);
            let source = view.menu.worktree.as_ref().unwrap();
            // The dialog holds the row it is creating, so a second pick cannot
            // start a second checkout while the first is still running.
            assert!(source.busy());
            assert_eq!(source.pending.as_ref().unwrap().number, 48);
            assert!(source.lookup.loading);
            assert!(view.menu.error.is_none());
            assert_eq!(
                view.menu.page,
                Some(Page::Dialog(WorkspaceAction::NewWorktree))
            );
        });
    });

    // A refusal releases the row so the listing can be used again.
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.menu.creation = Some("create".into());
            view.apply_creation_response(
                Ok(serde_json::json!({"error":{"code":"worktree_create_failed","message":"branch already checked out"}})),
                window,
                cx,
            );
            let source = view.menu.worktree.as_ref().unwrap();
            assert!(!source.busy());
            assert!(
                view.menu
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("branch already checked out")
            );
            assert_eq!(
                view.menu.page,
                Some(Page::Dialog(WorkspaceAction::NewWorktree))
            );
        });
    });
}

/// Tab moves between the dialog's tabs, and the listing owns the keys that
/// drive its rows rather than leaking them to the branch form.
#[gpui::test]
fn keys_move_between_tabs_and_through_the_rows(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(sidebar::layout_tests::fixture_window);
    cx.simulate_resize(gpui::size(gpui::px(900.), gpui::px(700.)));
    open_dialog(&view, cx, true, Tab::Branch);
    assert_eq!(tab(&view, cx), Tab::Branch);
    cx.simulate_keystrokes("tab");
    assert_eq!(tab(&view, cx), Tab::Items(Kind::PullRequest));
    cx.simulate_keystrokes("tab");
    assert_eq!(tab(&view, cx), Tab::Items(Kind::Issue));
    // The strip wraps, and shift-tab walks it the other way.
    cx.simulate_keystrokes("tab");
    assert_eq!(tab(&view, cx), Tab::Branch);
    cx.simulate_keystrokes("shift-tab");
    assert_eq!(tab(&view, cx), Tab::Items(Kind::Issue));

    cx.simulate_keystrokes("shift-tab");
    draw(cx);
    let selected = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| view.read(cx).menu.worktree.as_ref().unwrap().selected)
    };
    assert_eq!(selected(cx), 0);
    cx.simulate_keystrokes("down");
    assert_eq!(selected(cx), 1);
    // The rows wrap, as the other pickers' do.
    cx.simulate_keystrokes("down");
    assert_eq!(selected(cx), 0);
    cx.simulate_keystrokes("up");
    assert_eq!(selected(cx), 1);

    // Escape still closes the dialog from a listing.
    cx.simulate_keystrokes("escape");
    cx.update(|_, cx| assert!(view.read(cx).menu.page.is_none()));
}
