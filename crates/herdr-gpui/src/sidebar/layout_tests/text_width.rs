use super::*;

#[gpui::test]
fn sidebar_allocates_text_width(cx: &mut gpui::TestAppContext) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        crate::bind_keys(cx);
        // Deliberately do not call HerdrWindow::new: it connects and starts polling.
        let view = cx.new(|cx| fixture_window(window, cx));
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        SidebarFixture(view)
    });
    let result = check_sidebar(fixture, cx);
    assert!(result.is_ok(), "sidebar layout failed: {result:#?}");
}

#[cfg(test)]
fn check_sidebar(fixture: Entity<SidebarFixture>, cx: &mut gpui::VisualTestContext) -> Result<()> {
    use anyhow::Context as _;
    use gpui::{Modifiers, MouseButton, MouseDownEvent, point};
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = full_draw(window, cx);
    });

    cx.update(|_, cx| {
        for (input, (bounds, rendered, width)) in &cx.global::<TextProbes>().0 {
            eprintln!(
                "text {input:?}: bounds={bounds:?}, rendered={rendered:?}, glyph width={width:?}"
            );
            assert!(
                *width <= bounds.size.width,
                "glyphs must fit the allocation"
            );
        }
        // Each part of an agent's line is painted on its own, so the tab can
        // stay muted beside its workspace.
        for input in ["herdr", "main", "tab 1", "Claude Code"] {
            let (bounds, rendered, _) = &cx.global::<TextProbes>().0[input];
            assert_eq!(
                rendered, input,
                "short label must not ellipsize: {bounds:?}"
            );
        }
        for input in [
            "herdr-gpui-sidebar-rendering-regression-investigation",
            "fix/sidebar-label-width-and-overflow-regression",
        ] {
            let (bounds, rendered, width) = &cx.global::<TextProbes>().0[input];
            assert!(bounds.size.width > px(150.));
            assert!(*width > px(150.), "long labels must use available width");
            assert_eq!(bounds.size.height, px(16.));
            assert!(
                rendered.ends_with('\u{2026}'),
                "long label must ellipsize: {rendered:?}"
            );
            let prefix = rendered.trim_end_matches('\u{2026}');
            assert!(prefix.len() > 10 && input.starts_with(prefix));
            assert!(rendered.len() < input.len());
            assert!(!rendered.contains('\n'));
        }
    });

    let sidebar = cx.debug_bounds("sidebar").unwrap();
    let spaces = cx.debug_bounds("spaces-scroll").unwrap();
    let agents = cx.debug_bounds("agents-scroll").unwrap();
    assert_eq!(sidebar.size.width, px(232.));
    let icon = cx.debug_bounds("github-herdr").unwrap();
    let title = cx.debug_bounds("name-herdr").unwrap();
    let detail = cx.debug_bounds("detail-herdr").unwrap();
    assert_eq!(icon.size, size(px(12.), px(12.)));
    assert_eq!(title.left(), icon.right() + px(6.));
    assert_eq!(icon.left(), detail.left());
    assert_eq!(title.right(), detail.right());
    assert!(cx.debug_bounds("github-agent-launcher").is_some());
    assert!(cx.debug_bounds("github-sidebar-child").is_none());
    assert!(cx.debug_bounds("github-review").is_none());
    let footer = cx.debug_bounds("device-footer").unwrap();
    assert!(spaces.size.height + footer.size.height / 2. > px(200.));
    assert!(agents.size.height + footer.size.height / 2. > px(200.));
    assert!(agents.bottom() <= footer.top());
    let parent = cx.debug_bounds("name-agent-launcher").unwrap();
    for (name, detail) in [
        ("name-sidebar-child", "detail-sidebar-child"),
        (
            "name-sidebar-child-with-a-long-readable-branch-name",
            "detail-sidebar-child-with-a-long-readable-branch-name",
        ),
    ] {
        let name = cx.debug_bounds(name).unwrap();
        let detail = cx.debug_bounds(detail).unwrap();
        assert_eq!(
            name.left(),
            parent.left() + px(super::super::CHILD_INDENT - super::super::ICON_RESERVE)
        );
        assert_eq!(
            name.size.width,
            px(super::super::LABEL_WIDTH
                - super::super::CHILD_INDENT
                - super::super::ARROW_RESERVE)
        );
        assert_eq!(name.right(), parent.right());
        assert_eq!(detail.size.width, name.size.width);
        assert_eq!(name.size.height, px(16.));
    }

    for (row, column, name, detail) in [
        ("row-herdr", "column-herdr", "name-herdr", "detail-herdr"),
        (
            "row-herdr-gpui-sidebar-rendering-regression-investigation",
            "column-herdr-gpui-sidebar-rendering-regression-investigation",
            "name-herdr-gpui-sidebar-rendering-regression-investigation",
            "detail-herdr-gpui-sidebar-rendering-regression-investigation",
        ),
        (
            "row-agent-p0",
            "column-agent-p0",
            "name-agent-p0",
            "detail-agent-p0",
        ),
        (
            "row-agent-p1",
            "column-agent-p1",
            "name-agent-p1",
            "detail-agent-p1",
        ),
    ] {
        let row_bounds = cx.debug_bounds(row).unwrap();
        let column_bounds = cx.debug_bounds(column).unwrap();
        let name_bounds = cx.debug_bounds(name).unwrap();
        let detail_bounds = cx.debug_bounds(detail).unwrap();
        eprintln!(
            "{row}: row={row_bounds:?}, column={column_bounds:?}, name={name_bounds:?}, detail={detail_bounds:?}"
        );
        assert!(name_bounds.size.width > px(150.), "{name}: {name_bounds:?}");
        assert!(
            detail_bounds.size.width > px(150.),
            "{detail}: {detail_bounds:?}"
        );
        assert_eq!(name_bounds.size.height, px(16.), "single-line name");
        assert_eq!(detail_bounds.size.height, px(16.), "single-line detail");
        assert_eq!(row_bounds.size.height, px(40.));
        assert!(name_bounds.right() <= sidebar.right() - px(12.));
        assert!(detail_bounds.right() <= sidebar.right() - px(12.));
        assert!(
            row_bounds.bottom() <= sidebar.bottom(),
            "visible initial rows"
        );
    }

    // Drag beyond the divider, then back to a narrower allocation. Text must
    // be remeasured in both directions rather than retaining truncated runs.
    for target in [400., 160., 480.] {
        let divider = cx.debug_bounds("sidebar-resize").unwrap();
        let start = divider.center();
        let old_width = cx.debug_bounds("sidebar").unwrap().size.width;
        let end = point(start.x + px(target) - old_width, start.y);
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| {
            fixture.update(cx, |_, cx| cx.notify());
            let _ = full_draw(window, cx);
        });
        assert_eq!(cx.debug_bounds("sidebar").unwrap().size.width, px(target));
        let label = cx.debug_bounds("name-herdr").unwrap();
        assert_eq!(
            label.size.width,
            px(super::super::LABEL_WIDTH + target - 232. - super::super::ICON_RESERVE)
        );
        let parent = cx.debug_bounds("name-agent-launcher").unwrap();
        let child = cx.debug_bounds("name-sidebar-child").unwrap();
        assert_eq!(
            parent.size.width,
            label.size.width - px(super::super::ARROW_RESERVE)
        );
        assert_eq!(
            child.size.width,
            parent.size.width - px(super::super::CHILD_INDENT) + px(super::super::ICON_RESERVE)
        );
        assert_eq!(child.right(), parent.right());
        cx.update(|_, cx| {
            for (text, (bounds, rendered, glyph_width)) in &cx.global::<TextProbes>().0 {
                // GPUI rounds available text width to physical pixels.
                assert!(*glyph_width <= bounds.size.width + px(1.), "width={target}, text={text:?}, rendered={rendered:?}, bounds={bounds:?}, glyphs={glyph_width:?}");
            }
        });
        cx.simulate_mouse_move(point(px(600.), start.y), None, Modifiers::default());
        cx.update(|window, cx| {
            fixture.update(cx, |_, cx| cx.notify());
            let _ = full_draw(window, cx);
        });
        assert_eq!(cx.debug_bounds("sidebar").unwrap().size.width, px(target));
    }
    cx.simulate_resize(size(px(640.), px(600.)));
    cx.update(|window, cx| {
        let _ = full_draw(window, cx);
    });
    assert_eq!(cx.debug_bounds("sidebar").unwrap().size.width, px(400.));
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.update(|window, cx| {
        let _ = full_draw(window, cx);
    });
    assert_eq!(cx.debug_bounds("sidebar").unwrap().size.width, px(480.));

    let position = cx.debug_bounds("sidebar-resize").unwrap().center();
    cx.simulate_event(MouseDownEvent {
        position,
        button: MouseButton::Left,
        click_count: 2,
        ..Default::default()
    });
    cx.update(|window, cx| {
        fixture.update(cx, |_, cx| cx.notify());
        let _ = full_draw(window, cx);
    });
    assert_eq!(cx.debug_bounds("sidebar").unwrap().size.width, px(232.));

    let view = cx.update(|_, cx| fixture.read(cx).0.clone());
    let before = cx.update(|_, cx| {
        view.update(cx, |view, _| {
            let snapshot = Arc::make_mut(view.live.snapshot.as_mut().unwrap());
            snapshot.focused_workspace_id = Some("w4".into());
            for workspace in &mut snapshot.workspaces {
                workspace.focused = workspace.workspace_id == "w4";
            }
            view.marked = "selection must survive toggle".into();
            snapshot.clone()
        })
    });
    for collapsed in [true, false] {
        let arrow = cx.debug_bounds("collapse-3").unwrap();
        cx.simulate_click(arrow.center(), Default::default());
        cx.update(|window, cx| {
            cx.default_global::<TextProbes>().0.clear();
            window.refresh();
            full_draw(window, cx).clear(cx);
            let view = view.read(cx);
            assert_eq!(view.live.snapshot.as_deref(), Some(&before));
            assert_eq!(view.marked, "selection must survive toggle");
            assert!(cx.global::<TextProbes>().0.contains_key(if collapsed {
                "\u{25b8}"
            } else {
                "\u{25be}"
            }));
            assert_eq!(view.collapsed_repos.contains(REPO_KEY), collapsed);
            assert_eq!(
                !cx.global::<TextProbes>().0.contains_key("sidebar-child"),
                collapsed
            );
        });
    }
    let menu = cx.debug_bounds("sidebar-menu").unwrap();
    cx.simulate_click(menu.center(), Default::default());
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
    });
    assert!(cx.debug_bounds("menu-panel").is_some());
    assert!(cx.debug_bounds("menu-reload GUI config").is_some());
    crate::menu::workspace_tests::check_menu_interactions(&view, cx);
    cx.simulate_keystrokes("down down enter");
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Keybinds));
    });
    let panel = cx.debug_bounds("menu-panel").unwrap();
    assert_eq!(panel.size.width, px(480.));
    assert_eq!(panel.center(), point(px(400.), px(300.)));
    let first_description = cx.debug_bounds("description-New Workspace").unwrap();
    for (keys, label) in [
        ("keys-New Workspace", "description-New Workspace"),
        ("keys-New Tab", "description-New Tab"),
        ("keys-Split Right", "description-Split Right"),
        ("keys-Split Down", "description-Split Down"),
    ] {
        let keys = cx.debug_bounds(keys).unwrap();
        let label = cx.debug_bounds(label).unwrap();
        assert!(keys.right() < label.left());
        assert_eq!(label.left(), first_description.left());
        assert!(label.right() < panel.right());
    }
    cx.simulate_resize(size(px(360.), px(240.)));
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
    });
    let panel = cx.debug_bounds("menu-panel").unwrap();
    assert_eq!(panel.size.width, px(328.));
    assert!(panel.size.height <= px(208.));
    assert_eq!(panel.center(), point(px(180.), px(120.)));
    let header = cx.debug_bounds("keybinds-header").unwrap();
    let footer = cx.debug_bounds("keybinds-footer").unwrap();
    let body = cx.debug_bounds("keybinds-body").unwrap();
    assert!(body.size.height > px(0.));
    assert!(header.bottom() <= body.top());
    assert!(body.bottom() <= footer.top());
    assert!(footer.bottom() <= panel.bottom());
    let first_row = cx.debug_bounds("shortcut-New Workspace").unwrap();
    cx.simulate_keystrokes("pagedown");
    cx.update(|window, cx| full_draw(window, cx).clear(cx));
    assert!(cx.debug_bounds("shortcut-New Workspace").unwrap().top() < first_row.top());
    assert_eq!(cx.debug_bounds("keybinds-header").unwrap(), header);
    assert_eq!(cx.debug_bounds("keybinds-footer").unwrap(), footer);
    let close = cx.debug_bounds("keybinds-close").unwrap();
    cx.simulate_click(close.center(), Default::default());
    cx.update(|window, cx| {
        assert!(view.read(cx).menu.page.is_none());
        assert!(view.read(cx).focus.is_focused(window));
        view.update(cx, |view, cx| view.open_keybinds(window, cx));
        full_draw(window, cx).clear(cx);
    });
    assert_eq!(
        cx.debug_bounds("shortcut-New Workspace").unwrap(),
        first_row
    );
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        assert!(view.read(cx).menu.page.is_none());
    });
    cx.simulate_click(menu.center(), Default::default());
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
    });
    cx.simulate_click(point(px(700.), px(500.)), Default::default());
    cx.update(|_, cx| assert!(view.read(cx).menu.page.is_none()));

    // Exercise the actual right-click overlay and platform text handler, without a daemon.
    cx.update(|_, cx| {
        view.update(cx, |view, _| {
            view.live.status = crate::state::ConnectionStatus::Connected;
        })
    });
    let parent = cx.debug_bounds("row-agent-launcher").unwrap();
    cx.simulate_mouse_down(parent.center(), MouseButton::Right, Default::default());
    cx.simulate_mouse_up(parent.center(), MouseButton::Right, Default::default());
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Workspace));
        assert_eq!(view.read(cx).live.snapshot.as_deref(), Some(&before));
    });
    assert!(cx.debug_bounds("workspace-menu-Close group").is_some());
    assert!(cx.debug_bounds("workspace-menu-New worktree").is_some());
    // Actions share the session picker's trailing 14px icon in a 24px slot.
    for (row, icon, text) in [
        (
            "workspace-menu-Rename",
            "workspace-menu-icon-Rename",
            "workspace-menu-label-Rename",
        ),
        (
            "workspace-menu-Close group",
            "workspace-menu-icon-Close group",
            "workspace-menu-label-Close group",
        ),
        (
            "workspace-menu-New worktree",
            "workspace-menu-icon-New worktree",
            "workspace-menu-label-New worktree",
        ),
        (
            "workspace-menu-Open worktree...",
            "workspace-menu-icon-Open worktree...",
            "workspace-menu-label-Open worktree...",
        ),
    ] {
        let label = row;
        let row = cx.debug_bounds(row).unwrap();
        let icon = cx.debug_bounds(icon).unwrap();
        assert_eq!(icon.size, size(px(14.), px(14.)), "{label}");
        assert_eq!(row.right() - icon.right(), px(13.), "{label}");
        let text = cx.debug_bounds(text).unwrap();
        assert!(
            text.right() <= icon.left(),
            "{label}: label must precede icon"
        );
        assert!(
            (icon.center().y - row.center().y).abs() <= px(1.),
            "{label}"
        );
    }
    crate::menu::workspace_tests::check_menu_interactions(&view, cx);
    // PR data is fixture-only: no daemon, local Git, or GitHub calls in layout tests.
    crate::menu::workspace_tests::check_pr_fences(&view, cx);
    for width in [320., 800.] {
        cx.simulate_resize(size(px(width), px(600.)));
        for state in 0..5 {
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.menu.pr.clear();
                    view.menu.pr.loading = state == 0;
                    if state >= 2 {
                        view.menu.github = crate::github::Auth::connected_fixture();
                        view.menu.pr.value = Some(crate::pull_request::fixture().unwrap());
                    }
                    if state == 3 {
                        view.menu.pr.message = Some("Authentication unavailable".into());
                    }
                    cx.notify();
                });
                full_draw(window, cx).clear(cx);
            });
            let panel = cx.debug_bounds("menu-panel").unwrap();
            assert!(panel.left() >= px(0.) && panel.right() <= px(width));
            assert!(panel.bottom() <= px(600.));
            let open_row = cx.debug_bounds("workspace-menu-Open worktree...").unwrap();
            // Preserve the content budget apart from the action row and target header.
            let row_height = cx.update(|_, cx| px(view.read(cx).config.ui.line_height() + 12.));
            let header_height = cx
                .debug_bounds("workspace-menu-header")
                .unwrap()
                .size
                .height
                + px(4.);
            assert!((open_row.size.height - row_height).abs() <= px(1.));
            assert!(
                panel.size.height < px(320.) + row_height + header_height,
                "PR menu should size to its content: {panel:?}"
            );
            assert!(cx.debug_bounds("workspace-pr").is_some());
            if state >= 2 {
                let title = cx.debug_bounds("workspace-pr-title").unwrap();
                assert!(title.left() >= panel.left() && title.right() <= panel.right());
            }
        }
    }
    cx.update(|_, cx| {
        view.update(cx, |view, _| view.menu.pr.clear());
    });
    for dialog in [false, true] {
        if dialog {
            cx.simulate_keystrokes("down enter");
        }
        for anchor in [
            point(px(200.), px(400.)),
            point(px(795.), px(595.)),
            point(px(-10.), px(-20.)),
        ] {
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.menu.anchor = anchor;
                    cx.notify();
                });
                full_draw(window, cx).clear(cx);
            });
            let panel = cx.debug_bounds("menu-panel").unwrap();
            assert_eq!(panel.size.width, px(if dialog { 420. } else { 340. }));
            if dialog {
                // A dialog is a modal decision, so it centres on the window and
                // ignores the anchor the row menu was opened from.
                let offset = panel.center() - point(px(400.), px(300.));
                assert!(
                    offset.x.abs() <= px(1.) && offset.y.abs() <= px(1.),
                    "{anchor:?}: {panel:?}"
                );
            } else {
                let expected = |position: Pixels, extent: Pixels, viewport: Pixels| {
                    if position + extent > viewport {
                        (viewport - extent - px(12.)).round()
                    } else if position < px(0.) {
                        px(12.)
                    } else {
                        position.round()
                    }
                };
                assert_eq!(panel.left(), expected(anchor.x, panel.size.width, px(800.)));
                assert_eq!(panel.top(), expected(anchor.y, panel.size.height, px(600.)));
            }
            assert!(panel.right() <= px(800.) && panel.bottom() <= px(600.));
        }
    }
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.menu.anchor = parent.center();
            cx.notify();
        });
        full_draw(window, cx).clear(cx);
    });
    cx.simulate_input("\u{65e5}\u{672c}\u{1f600}");
    cx.update(|window, cx| {
        use gpui::EntityInputHandler;
        view.update(cx, |view, cx| {
            assert_eq!(
                view.menu.input.as_ref().unwrap().text,
                "\u{65e5}\u{672c}\u{1f600}"
            );
            view.replace_and_mark_text_in_range(Some(2..4), "\u{304b}", Some(1..1), window, cx);
            assert_eq!(view.marked_text_range(window, cx), Some(2..3));
            assert_eq!(
                view.selected_text_range(false, window, cx).unwrap().range,
                3..3
            );
            view.replace_text_in_range(None, "\u{6f22}", window, cx);
            assert_eq!(
                view.menu.input.as_ref().unwrap().text,
                "\u{65e5}\u{672c}\u{6f22}"
            );
            assert!(view.marked.is_empty());
            view.command(crate::controls::Command::Workspace, window, cx);
            assert!(view.local_error.is_none());
        });
        full_draw(window, cx).clear(cx);
        view.update(cx, |view, cx| {
            let bounds = view
                .bounds_for_range(3..3, Bounds::default(), window, cx)
                .unwrap();
            assert!(
                view.menu
                    .input
                    .as_ref()
                    .unwrap()
                    .bounds
                    .contains(&bounds.origin)
            );
        });
    });
    cx.simulate_keystrokes("enter");
    cx.update(|_, cx| {
        // No handle: a queue failure must preserve the draft, not claim success.
        assert!(
            view.read(cx).menu.page
                == Some(crate::menu::Page::Dialog(
                    crate::menu::WorkspaceAction::Rename
                ))
        );
    });
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("   ");
    cx.simulate_keystrokes("enter");
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        assert_eq!(view.read(cx).menu.input.as_ref().unwrap().text, "   ");
        assert!(
            view.read(cx).menu.page
                == Some(crate::menu::Page::Dialog(
                    crate::menu::WorkspaceAction::Rename
                ))
        );
    });
    assert!(cx.debug_bounds("dialog-error").is_some());
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        assert!(view.read(cx).menu.page.is_none());
        assert!(view.read(cx).menu.input.is_none());
        assert!(view.read(cx).focus.is_focused(window));
    });

    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.config.sidebar.size = 24.;
            view.marked = "composition".into();
            view.open_keybinds(window, cx);
            assert!(view.marked.is_empty());
        });
        full_draw(window, cx).clear(cx);
        assert!(!view.read(cx).focus.is_focused(window));
    });
    let line_height = cx.update(|_, cx| super::super::line_height(&view.read(cx).config.sidebar));
    assert_eq!(
        cx.debug_bounds("row-herdr").unwrap().size.height,
        px(2. * line_height + 8.)
    );
    assert_eq!(
        cx.debug_bounds("name-herdr").unwrap().size.height,
        px(line_height)
    );
    let title = cx.debug_bounds("name-herdr").unwrap();
    let detail = cx.debug_bounds("detail-herdr").unwrap();
    let icon = cx.debug_bounds("github-herdr").unwrap();
    assert_eq!(title.bottom(), detail.top());
    assert_eq!(detail.size.height, px(line_height));
    assert_eq!(
        title.size.width,
        px(super::super::LABEL_WIDTH - super::super::ICON_RESERVE)
    );
    assert_eq!(title.right(), detail.right());
    assert_eq!(icon.center().y, title.center().y);
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));
    cx.simulate_keystrokes("cmd-/");
    let shortcut_search = cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        let search = view.read(cx).menu.keybinds_search.as_ref().unwrap().clone();
        assert!(search.read(cx).focus.is_focused(window));
        search
    });
    cx.simulate_input("pane zoom");
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        assert_eq!(shortcut_search.read(cx).text(), "pane zoom");
    });
    assert!(cx.debug_bounds("shortcut-Toggle Pane Zoom").is_some());
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("no-shortcut-matches-xyz");
    cx.update(|window, cx| full_draw(window, cx).clear(cx));
    assert!(cx.debug_bounds("keybinds-empty").is_some());
    cx.simulate_keystrokes("cmd-w");
    cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(crate::menu::Page::Keybinds)));
    cx.simulate_keystrokes("escape cmd-/");
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        let search = view.read(cx).menu.keybinds_search.as_ref().unwrap().clone();
        assert!(search.read(cx).text().is_empty());
        search.update(cx, |input, cx| {
            gpui::EntityInputHandler::replace_and_mark_text_in_range(
                input,
                None,
                "pane",
                Some(4..4),
                window,
                cx,
            )
        });
    });
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Keybinds));
        let search = view.read(cx).menu.keybinds_search.as_ref().unwrap().clone();
        search.update(cx, |input, cx| {
            gpui::EntityInputHandler::unmark_text(input, window, cx)
        });
    });
    cx.simulate_keystrokes("escape");
    // Exercise the retained modal, not the standalone Settings command.
    cx.update(|window, cx| {
        view.update(cx, |view, cx| view.open_preferences_fixture(window, cx));
    });
    // General has enough content to exercise the independent body scroll.
    cx.simulate_keystrokes("shift-tab");
    cx.simulate_resize(size(px(360.), px(240.)));
    cx.update(|window, cx| full_draw(window, cx).clear(cx));
    let header = cx.debug_bounds("preferences-header").unwrap();
    let footer = cx.debug_bounds("preferences-footer").unwrap();
    let body = cx.debug_bounds("preferences-body").unwrap();
    let usage_row = cx.debug_bounds("preferences-show-usage").unwrap();
    assert!(body.size.height > px(0.));
    assert!(header.bottom() <= body.top());
    assert!(body.bottom() <= footer.top());
    cx.simulate_keystrokes("pagedown");
    cx.update(|window, cx| full_draw(window, cx).clear(cx));
    assert!(cx.debug_bounds("preferences-show-usage").unwrap().top() < usage_row.top());
    assert_eq!(cx.debug_bounds("preferences-header").unwrap(), header);
    assert_eq!(cx.debug_bounds("preferences-footer").unwrap(), footer);
    let close = cx.debug_bounds("preferences-close").unwrap();
    cx.simulate_click(close.center(), Default::default());
    cx.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));
    for width in [320., 640., 1200.] {
        cx.simulate_resize(size(px(width), px(400.)));
        for state in 0..5 {
            cx.update(|window, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string("unchanged".into()));
                cx.default_global::<PaintedProbes>().0.clear();
                view.update(cx, |view, cx| {
                    view.github_fixture(state == 1, window, cx);
                    if state == 2 {
                        view.menu.github.failed = true;
                        view.menu.github.message =
                            Some("GitHub code expired. Sign in again. ".repeat(40));
                    } else if state == 3 {
                        view.menu.github = crate::github::Auth::connected_fixture();
                    } else if state == 4 {
                        view.menu.github = crate::github::Auth::requesting_fixture();
                    }
                });
                full_draw(window, cx).clear(cx);
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().as_deref(),
                    Some("unchanged")
                );
                assert_eq!(
                    cx.global::<PaintedProbes>().0.contains_key("Sign out (D)"),
                    state == 3
                );
            });
            let panel = cx.debug_bounds("menu-panel").unwrap();
            assert!(panel.left() >= px(0.) && panel.right() <= px(width));
            assert!(panel.bottom() <= px(400.));
            assert!(panel.size.width <= px(400.));
            assert!(cx.debug_bounds("github-close").is_none());
            let close = cx.debug_bounds("github-header-close").unwrap();
            assert!(close.top() >= panel.top() && close.bottom() <= panel.bottom());
            if state == 3 {
                assert!(panel.size.height <= px(230.));
            }
            let footer = cx.debug_bounds("github-footer");
            let body = cx.debug_bounds("github-body").unwrap();
            assert!(body.size.height > px(0.));
            // A pending request offers no footer actions, so none is drawn.
            assert_eq!(footer.is_none(), state == 4);
            if let Some(footer) = footer {
                // Content-sized layouts can round adjacent edges to half pixels.
                assert!(body.bottom() <= footer.top() + px(1.));
                assert!(footer.bottom() <= panel.bottom() + px(1.));
            } else {
                assert!(body.bottom() <= panel.bottom() + px(1.));
            }
            if state == 1 {
                let code = cx.debug_bounds("github-device-code").unwrap();
                assert!(code.left() >= panel.left() && code.right() <= panel.right());
                let copy = cx.debug_bounds("github-copy").unwrap();
                cx.simulate_click(copy.center(), Default::default());
                cx.update(|_, cx| {
                    assert_eq!(
                        cx.read_from_clipboard().unwrap().text().as_deref(),
                        Some("ABCD-1234")
                    );
                    assert!(view.read(cx).menu.github.copied());
                });
                cx.simulate_keystrokes("tab enter");
                cx.update(|_, cx| assert!(view.read(cx).menu.github.copied()));
                cx.simulate_keystrokes("cmd-c");
                let open = cx.debug_bounds("github-open").unwrap();
                cx.simulate_click(open.center(), Default::default());
                assert_eq!(cx.opened_url().as_deref(), Some(crate::github::VERIFY_URL));
            } else if state == 2 {
                let status = cx.debug_bounds("github-status").unwrap();
                cx.simulate_keystrokes("pagedown");
                cx.update(|window, cx| full_draw(window, cx).clear(cx));
                assert!(cx.debug_bounds("github-status").unwrap().top() < status.top());
                assert_eq!(cx.debug_bounds("github-footer"), footer);
            }
            cx.simulate_keystrokes("c escape");
            cx.update(|window, cx| {
                assert!(view.read(cx).menu.page.is_none());
                assert!(view.read(cx).focus.is_focused(window));
                assert!(view.read(cx).menu.github.code().is_none());
                assert!(!view.read(cx).menu.github.copied());
            });
        }
    }
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_preferences_fixture(window, cx);
            view.select_settings_tab(crate::settings_panel::Tab::Theme, window, cx)
        });
    });
    cx.update(|window, cx| full_draw(window, cx).clear(cx));
    let choose_theme = cx.debug_bounds("preferences-choose-theme").unwrap();
    cx.simulate_click(choose_theme.center(), Default::default());
    cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(crate::menu::Page::Themes)));
    cx.simulate_keystrokes("escape");

    let search = cx.update(|window, cx| {
        view.update(cx, |view, cx| view.open_theme_picker(window, cx));
        full_draw(window, cx).clear(cx);
        let search = view.read(cx).menu.themes.as_ref().unwrap().search.clone();
        assert!(search.read(cx).focus.is_focused(window));
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("catppuccin mocha".into()));
        search
    });
    cx.simulate_keystrokes("cmd-v");
    cx.run_until_parked();
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        assert_eq!(search.read(cx).text(), "catppuccin mocha");
        assert!(view.read(cx).marked.is_empty());
    });
    assert!(cx.debug_bounds("theme-name-Catppuccin Mocha").is_some());
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx).menu.themes.as_ref().unwrap().filtered,
            ["Catppuccin Mocha"]
        );
    });
    cx.simulate_keystrokes("cmd-a n o r d");
    cx.run_until_parked();
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        assert_eq!(search.read(cx).text(), "nord");
        assert!(
            view.read(cx)
                .menu
                .themes
                .as_ref()
                .unwrap()
                .filtered
                .iter()
                .all(|name| name.to_lowercase().contains("nord"))
        );
    });
    cx.simulate_keystrokes("cmd-a");
    cx.update(|_, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("no-such-theme-xyz".into()))
    });
    cx.simulate_keystrokes("cmd-v");
    cx.run_until_parked();
    cx.update(|window, cx| full_draw(window, cx).clear(cx));
    assert!(cx.debug_bounds("theme-empty").is_some());
    // Enter with no results must neither write a config nor dismiss the picker.
    cx.simulate_keystrokes("down enter");
    cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(crate::menu::Page::Themes)));
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        assert!(view.read(cx).focus.is_focused(window));
        view.update(cx, |view, cx| view.open_theme_picker(window, cx));
        full_draw(window, cx).clear(cx);
        assert!(search.read(cx).text().is_empty());
    });
    cx.update(|window, cx| {
        search.update(cx, |search, cx| {
            gpui::EntityInputHandler::replace_and_mark_text_in_range(
                search,
                None,
                "Nord",
                Some(4..4),
                window,
                cx,
            );
        });
        full_draw(window, cx).clear(cx);
    });
    cx.simulate_keystrokes("enter");
    cx.update(|_, cx| {
        assert!(
            view.read(cx).menu.page == Some(crate::menu::Page::Themes),
            "IME confirmation must not apply a theme"
        );
    });
    cx.update(|window, cx| {
        search.update(cx, |search, cx| {
            gpui::EntityInputHandler::unmark_text(search, window, cx)
        });
    });
    cx.simulate_keystrokes("escape");

    cx.simulate_keystrokes("cmd-shift-p");
    let palette_search = cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Palette));
        view.read(cx).menu.palette.as_ref().unwrap().search.clone()
    });
    // Bound native commands must not fire while a search field has focus.
    cx.simulate_keystrokes("cmd-b");
    cx.update(|_, cx| assert!(view.read(cx).sidebar_visible));
    cx.simulate_input("toggle sidebar");
    cx.update(|_, cx| assert_eq!(palette_search.read(cx).text(), "toggle sidebar"));
    cx.simulate_keystrokes("enter");
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        assert!(!view.read(cx).sidebar_visible);
        assert!(view.read(cx).menu.page.is_none());
        assert!(view.read(cx).focus.is_focused(window));
    });
    cx.simulate_keystrokes("cmd-b");
    cx.update(|window, cx| {
        view.update(cx, |view, cx| view.open_preferences_fixture(window, cx));
    });
    cx.update(|_, cx| {
        assert!(view.read(cx).sidebar_visible);
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Preferences));
    });
    cx.simulate_keystrokes("escape cmd-p");
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Palette));
        let search = &view.read(cx).menu.palette.as_ref().unwrap().search;
        assert!(search.read(cx).text().is_empty());
    });
    cx.simulate_input("no-workspace-matches-xyz");
    cx.simulate_keystrokes("enter");
    cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(crate::menu::Page::Palette)));
    cx.simulate_keystrokes("escape");

    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.live.snapshot = Some(Arc::new(
                serde_json::from_str(include_str!(
                    "../../../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
                ))
                .unwrap(),
            ));
            cx.notify();
        });
    });
    cx.simulate_keystrokes("cmd-w");
    cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(crate::menu::Page::ConfirmClose)));
    cx.simulate_keystrokes("enter");
    cx.update(|_, cx| {
        assert!(
            view.read(cx).menu.page.is_none(),
            "Enter defaults to Cancel"
        )
    });
    cx.simulate_keystrokes("cmd-shift-w tab enter");
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        assert!(
            view.read(cx).menu.page == Some(crate::menu::Page::ConfirmClose),
            "disconnected confirmation stays open with error"
        );
        let view = view.read(cx);
        assert!(
            view.endpoints[view.selected_endpoint]
                .connection
                .handle
                .is_none()
        );
    });
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));

    let before_install = cx.update(|_, cx| view.read(cx).live.snapshot.clone());
    cx.update(|window, cx| {
        view.update(cx, |view, cx| view.show_install_modal(window, cx));
    });
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        let view = view.read(cx);
        assert!(view.menu.page == Some(crate::menu::Page::Install));
        assert!(!view.live.missing_installation);
        assert_eq!(view.live.snapshot, before_install);
    });
    assert!(cx.debug_bounds("menu-install").is_some());
    assert!(cx.debug_bounds("menu-dismiss").is_some());
    cx.simulate_keystrokes("escape");
    cx.update(|_, cx| assert!(view.read(cx).menu.page.is_none()));

    // Fixtures have no updater worker, and unavailable updates use the shared panel.
    let updater_before = cx.update(|_, cx| view.read(cx).updater.state().clone());
    assert!(matches!(updater_before, crate::updater::State::Disabled(_)));
    cx.update(|window, cx| window.dispatch_action(Box::new(crate::CheckForUpdates), cx));
    assert!(cx.pending_prompt().is_none());
    cx.update(|window, cx| {
        full_draw(window, cx).clear(cx);
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::AppUpdate));
        assert_eq!(view.read(cx).live.snapshot, before_install);
    });
    assert!(cx.debug_bounds("app-update-action").is_none());
    let releases = cx
        .debug_bounds("app-update-releases")
        .context("update releases bounds")?;
    cx.simulate_click(releases.center(), Default::default());
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/penso/herdr-gpui/releases")
    );
    let close = cx
        .debug_bounds("app-update-close")
        .context("update close bounds")?;
    cx.simulate_click(close.center(), Default::default());
    cx.update(|window, cx| {
        let view = view.read(cx);
        assert!(view.menu.page.is_none());
        assert!(view.focus.is_focused(window));
        assert_eq!(view.updater.state(), &updater_before);
    });
    for (width, height) in [(320., 360.), (320., 600.), (480., 600.), (800., 600.)] {
        cx.simulate_resize(size(px(width), px(height)));
        cx.update(|window, cx| window.dispatch_action(Box::new(crate::ShowUpdatePreview), cx));
        for ready in [false, true] {
            cx.update(|window, cx| {
                full_draw(window, cx).clear(cx);
                let view = view.read(cx);
                assert_eq!(view.updater.state(), &updater_before);
                assert_eq!(view.live.snapshot, before_install);
                assert_eq!(
                    view.update_preview,
                    Some(if ready {
                        crate::updater::State::Ready {
                            version: "9999.0.0".into(),
                        }
                    } else {
                        crate::updater::State::Available {
                            version: "9999.0.0".into(),
                        }
                    })
                );
            });
            let panel = cx
                .debug_bounds("app-update-panel")
                .context("update panel bounds")?;
            let action = cx
                .debug_bounds("app-update-action")
                .context("update action bounds")?;
            let header = cx
                .debug_bounds("app-update-header")
                .context("update header bounds")?;
            let close = cx
                .debug_bounds("app-update-close")
                .context("update close bounds")?;
            assert_eq!(close.right(), header.right() - px(16.));
            assert!(close.left() > header.center().x);
            assert!(close.top() >= header.top() && close.bottom() <= header.bottom());
            assert!(header.bottom() < action.top());
            let body = cx
                .debug_bounds("app-update-body")
                .context("update body bounds")?;
            let footer = cx
                .debug_bounds("app-update-footer")
                .context("update footer bounds")?;
            let current = cx
                .debug_bounds("app-update-current-version")
                .context("current version bounds")?;
            let latest = cx
                .debug_bounds("app-update-latest-version")
                .context("latest version bounds")?;
            assert_eq!(current.left(), latest.left());
            assert_eq!(current.right(), latest.right());
            assert!(current.bottom() < latest.top());
            assert_eq!(header.left(), panel.left());
            assert_eq!(header.right(), panel.right());
            assert!(body.top() >= header.bottom());
            assert!((footer.top() - body.bottom()).abs() <= px(1.));
            assert!(panel.top() >= px(0.) && panel.bottom() <= px(height));
            assert!(action.top() >= footer.top() && action.bottom() <= footer.bottom());
            assert!(panel.left() >= px(0.) && panel.right() <= px(width));
            assert!(action.left() >= panel.left() && action.right() <= panel.right());
            assert!(action.top() >= panel.top() && action.bottom() <= panel.bottom());
            cx.simulate_click(action.center(), Default::default());
        }
        cx.update(|_, cx| {
            let view = view.read(cx);
            assert!(view.menu.page.is_none());
            assert!(view.update_preview.is_none());
            assert_eq!(view.updater.state(), &updater_before);
        });
        assert!(cx.pending_prompt().is_none());
    }
    // The same panel is reachable without native menus, including on Linux.
    cx.update(|window, cx| {
        view.update(cx, |view, cx| view.open_menu(window, cx));
        full_draw(window, cx).clear(cx);
    });
    let updates = cx
        .debug_bounds("menu-app updates")
        .context("app updates menu bounds")?;
    assert!(cx.debug_bounds("menu-preview app update").is_some());
    cx.simulate_click(updates.center(), Default::default());
    cx.update(|_, cx| {
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::AppUpdate));
        assert!(view.read(cx).update_preview.is_none());
    });
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| window.dispatch_action(Box::new(crate::ShowUpdatePreview), cx));
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        let view = view.read(cx);
        assert!(view.menu.page.is_none());
        assert!(view.update_preview.is_none());
        assert!(view.focus.is_focused(window));
        assert_eq!(view.updater.state(), &updater_before);
    });
    // Exercise the real status bar without starting a daemon connection.
    view.update(cx, |view, cx| {
        view.marked = "composition ".repeat(100);
        view.local_error = Some("long connection error ".repeat(100));
        cx.notify();
    });
    for width in [480., 800.] {
        cx.simulate_resize(size(px(width), px(600.)));
        cx.update(|window, cx| full_draw(window, cx).clear(cx));
        let status = cx.debug_bounds("connection-status").unwrap();
        let report = cx.debug_bounds("report-issue").unwrap();
        assert!(report.size.width >= px(33.));
        assert!(report.left() >= status.left());
        assert!(report.right() <= status.right());
        assert!(report.top() >= status.top());
        assert!(report.bottom() <= status.bottom());
        let version = cx
            .debug_bounds("status-version")
            .context("status version bounds")?;
        assert!(version.size.width > px(0.));
        assert!(version.left() >= report.right());
        assert!(version.right() <= status.right());
        assert!(version.top() >= status.top());
        assert!(version.bottom() <= status.bottom());
        let theme = cx.debug_bounds("status-theme").unwrap();
        let keybinds = cx.debug_bounds("status-keybinds").unwrap();
        assert!(theme.left() >= status.left());
        assert!(theme.right() <= keybinds.left());
        assert!(keybinds.right() <= report.left());
        for button in [theme, keybinds] {
            assert!(button.size.width > px(0.));
            assert!(button.top() >= status.top());
            assert!(button.bottom() <= status.bottom());
        }
        cx.simulate_click(report.center(), Default::default());
        assert_eq!(
            cx.opened_url().as_deref(),
            Some(
                format!(
                    "https://github.com/penso/herdr-gpui/issues/new?template=bug_report.yml&version={}",
                    crate::APP_VERSION.replace('+', "%2B"),
                )
                .as_str()
            )
        );
    }
    cx.update(|_, cx| cx.default_global::<PaintedProbes>().check())
}
