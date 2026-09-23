//! Paint-phase probes shared by the headless layout and native full-window tests.
//! Headless NoopTextSystem ignores font-run lengths, so only the native smoke
//! test can catch GPUI's stale truncation runs. Keep headless checks for geometry.
#![allow(clippy::unwrap_used)]
#[cfg(test)]
use crate::HerdrWindow;
#[cfg(test)]
use crate::{LiveState, WheelAccumulator};
use gpui::{
    App, Bounds, ElementId, Global, GlobalElementId, InspectorElementId, LayoutId, Pixels,
    SharedString, TextLayout, Window, prelude::*, px,
};
#[cfg(test)]
use gpui::{Context, Entity, Modifiers, Task, point, size};
use herdr_client::protocol::*;
#[cfg(test)]
use herdr_client::{ConnectOptions, ConnectTarget};
#[cfg(test)]
use std::sync::Arc;

#[derive(Default)]
struct TextProbes(std::collections::BTreeMap<String, (Bounds<Pixels>, String, Pixels)>);
impl Global for TextProbes {}

#[derive(Default)]
pub(crate) struct PaintedProbes(pub std::collections::BTreeMap<String, PaintedText>);
impl Global for PaintedProbes {}

#[derive(Debug)]
#[cfg_attr(not(feature = "integration-test"), allow(dead_code))]
pub(crate) struct PaintedText {
    pub bounds: Bounds<Pixels>,
    pub mask: Bounds<Pixels>,
    pub cached: String,
    pub glyph_text: String,
    pub width: Pixels,
    pub clipped: bool,
}

// Delegate every phase to the production SharedString element. Native checks
// inspect the glyph stream used by paint, not just the cached backing string.
pub(crate) struct ProbeText(pub SharedString);

impl IntoElement for ProbeText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for ProbeText {
    type RequestLayoutState = TextLayout;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, TextLayout) {
        self.0.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut TextLayout,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.prepaint(id, inspector_id, bounds, state, window, cx);
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut TextLayout,
        prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0
            .paint(id, inspector_id, bounds, state, prepaint, window, cx);
        let width = state
            .line_layout_for_index(0)
            .unwrap()
            .unwrapped_layout
            .width;
        cx.default_global::<TextProbes>().0.insert(
            self.0.to_string(),
            (state.bounds(), state.wrapped_text(), width),
        );
        if !cx.has_global::<PaintedProbes>() {
            return;
        }
        // Inspect the actual native glyph stream consumed by WrappedLine::paint.
        // Its backing string can be longer than the shaped font runs (GPUI 0.2.2).
        let line = state.line_layout_for_index(0).unwrap();
        let layout = &line.unwrapped_layout;
        let text = state.text();
        let mask = window.content_mask().bounds;
        let mut glyph_text = String::new();
        let mut clipped = !line.wrap_boundaries.is_empty();
        let baseline = bounds.origin.y
            + (state.line_height() - layout.ascent - layout.descent) / 2.
            + layout.ascent;
        for run in &layout.runs {
            for glyph in &run.glyphs {
                let ch = text[glyph.index..].chars().next().unwrap();
                let ink = cx
                    .text_system()
                    .typographic_bounds(run.font_id, layout.font_size, ch)
                    .unwrap();
                let left = bounds.origin.x + glyph.position.x + ink.origin.x;
                let right = left + ink.size.width;
                let top = baseline + glyph.position.y - ink.bottom();
                let bottom = top + ink.size.height;
                if ink.size.width > px(0.) && ink.size.height > px(0.) {
                    clipped |= left < mask.left()
                        || right > mask.right()
                        || top < mask.top()
                        || bottom > mask.bottom();
                }
                // Resolve the ID independently, not merely its cached source index.
                let expected = window.text_system().shape_line(
                    ch.to_string().into(),
                    layout.font_size,
                    &[window.text_style().to_run(ch.len_utf8())],
                    None,
                );
                assert_eq!(
                    expected.runs[0].glyphs[0].id, glyph.id,
                    "painted glyph ID for {ch:?}"
                );
                glyph_text.push(ch);
            }
        }
        // These extra fixture rows leave the original smoke/performance labels
        // untouched. Check their native glyphs whenever the whole row is visible.
        if matches!(
            self.0.as_ref(),
            "sidebar-child" | "sidebar-child-with-a-long-readable-branch-name"
        ) && bounds.top() >= mask.top()
            && bounds.bottom() <= mask.bottom()
        {
            let parent = &cx.global::<TextProbes>().0["agent-launcher"].0;
            assert_eq!(
                bounds.left(),
                parent.left() + px(super::CHILD_INDENT - super::ICON_RESERVE)
            );
            assert_eq!(
                bounds.size.width,
                px(super::LABEL_WIDTH - super::CHILD_INDENT - super::ARROW_RESERVE)
            );
            assert_eq!(mask.size.width, bounds.size.width);
            assert_eq!(glyph_text, state.wrapped_text());
            assert!(!clipped, "child glyphs clipped: {glyph_text}");
            if self.0.as_ref() == "sidebar-child" {
                assert_eq!(glyph_text, "sidebar-child");
            } else {
                assert!(glyph_text.starts_with("sidebar-child"));
                assert!(glyph_text.ends_with('\u{2026}'));
                // Truncation fills the column it was given, whatever the indent.
                assert!(width > bounds.size.width - px(12.), "{width:?}");
            }
            eprintln!("SIDEBAR child verified: {glyph_text}");
        }
        cx.default_global::<PaintedProbes>()
            .0
            .entry(self.0.to_string())
            .or_insert(PaintedText {
                bounds,
                mask,
                cached: state.wrapped_text(),
                glyph_text,
                width,
                clipped,
            });
    }
}

#[cfg(test)]
struct SidebarFixture(Entity<HerdrWindow>);

#[cfg(test)]
impl Render for SidebarFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.0.clone()
    }
}

pub(crate) fn snapshot(workspace_count: usize) -> ClientShellSnapshot {
    serde_json::from_value(serde_json::json!({
        "boot_id": "layout-test", "revision": 1,
        "update_install_command": "", "latest_release_notes_available": false,
        "integration_updates_available": false, "worktree_directory": "",
        "tab_bar_right": [], "tab_bar_right_separator": "", "agent_order": [],
        // A workspace always has at least one tab; two here so the agents panel
        // has a tab label to show, as it does against a live daemon.
        "tabs": (0..2).map(|i| serde_json::json!({
            "tab_id": format!("t{i}"), "workspace_id": "w0", "number": i + 1,
            "label": format!("tab {}", i + 1), "custom_label": false,
            "zoomed": false, "focused": i == 0, "agent_status": "working"
        })).collect::<Vec<_>>(),
        "panes": [], "commands": [],
        "workspaces": (0..workspace_count).map(|i| serde_json::json!({
            "workspace_id": format!("w{i}"), "active_tab_id": "t0", "new_workspace_cwd": "/tmp",
            "number": i + 1,
            "label": match i { 0 => "herdr", 1 => "herdr-gpui-sidebar-rendering-regression-investigation", 3..=5 => "agent-launcher", _ => "another workspace" },
            "custom_label": false,
            "branch": match i { 0 => "main", 2 => "1256789", 3 => "develop", 4 => "worktree/sidebar-child", 5 => "worktree/sidebar-child-with-a-long-readable-branch-name", _ => "fix/sidebar-label-width-and-overflow-regression" },
            "worktree": if (3..=5).contains(&i) { serde_json::json!({
                "key": "/fixture/agent-launcher/.git", "label": "agent-launcher", "is_linked_worktree": i != 3
            }) } else { serde_json::Value::Null },
            "tokens": [], "focused": i == 0, "agent_status": "working"
        })).collect::<Vec<_>>(),
        "agents": (["review", "Investigate sidebar rendering and verify long agent labels"].into_iter().enumerate().map(|(i, name)| serde_json::json!({
            "pane_id": format!("p{i}"), "workspace_id": if i == 0 { "w0" } else { "w1" },
            "tab_id": if i == 0 { "t0" } else { "none" },
            "name": name, "display_agent": if i == 0 { "Claude Code" } else { "agent" }, "agent": "claude",
            "agent_status": "working", "state_change_seq": 0, "state_labels": [],
            "tokens": [], "focused": false
        })).collect::<Vec<_>>())
    })).unwrap()
}

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

#[gpui::test]
fn multi_host_rows_scope_duplicate_ids_and_keep_agents_when_host_collapses(
    cx: &mut gpui::TestAppContext,
) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture_window(window, cx);
            view.live.snapshot = Some(Arc::new(snapshot(1)));
            let mut remote = crate::endpoint::Endpoint::new(
                "ssh:test".into(),
                "Remote".into(),
                ConnectTarget::Ssh {
                    target: "unused".into(),
                    session: "default".into(),
                },
                true,
            );
            remote.live.snapshot = view.live.snapshot.clone();
            let remote_snapshot = Arc::make_mut(remote.live.snapshot.as_mut().unwrap());
            remote_snapshot.workspaces[0].label = "remote workspace".into();
            remote_snapshot.workspaces[0].branch = Some("remote branch".into());
            view.endpoints.push(remote);
            view
        });
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        SidebarFixture(view)
    });
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    for selector in [
        "host-local",
        "host-ssh:test",
        "workspace-local-w0",
        "workspace-ssh:test-w0",
        "agent-local-p0",
        "agent-ssh:test-p0",
        "github-herdr",
        "github-remote workspace",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "missing {selector}");
    }
    for (icon, title) in [
        ("github-herdr", "name-herdr"),
        ("github-remote workspace", "name-remote workspace"),
    ] {
        let icon = cx.debug_bounds(icon).unwrap();
        let title = cx.debug_bounds(title).unwrap();
        assert_eq!(icon.size, size(px(12.), px(12.)));
        assert_eq!(title.left(), icon.right() + px(6.));
        assert_eq!(
            title.size.width,
            px(super::LABEL_WIDTH - super::ICON_RESERVE)
        );
    }
    fixture.update(cx, |fixture, cx| {
        fixture.0.update(cx, |view, cx| {
            view.endpoints[1].collapsed = true;
            cx.notify();
        });
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        cx.default_global::<TextProbes>().0.clear();
        window.refresh();
        let _ = window.draw(cx);
        // The remote workspace row folds away -- its branch goes with it -- while
        // its agent keeps naming the host it runs on.
        assert!(!cx.global::<TextProbes>().0.contains_key("remote branch"));
        for part in ["Remote", "remote workspace", "tab 1"] {
            assert!(
                cx.global::<TextProbes>().0.contains_key(part),
                "{part}: {:?}",
                cx.global::<TextProbes>().0.keys()
            );
        }
    });
    assert!(cx.debug_bounds("workspace-local-w0").is_some());
    assert!(cx.debug_bounds("agent-ssh:test-p0").is_some());
}

#[cfg(test)]
pub(crate) fn fixture_window(window: &mut Window, cx: &mut Context<HerdrWindow>) -> HerdrWindow {
    HerdrWindow {
        updater: crate::updater::Updater::default(),
        update_preview: None,
        removal: None,
        config: Default::default(),
        theme: Default::default(),
        config_load: None,
        git: Default::default(),
        sidebar_visible: true,
        endpoints: vec![crate::endpoint::Endpoint::new(
            crate::endpoint::LOCAL.into(),
            "Local".into(),
            ConnectTarget::Socket("/unused-layout-test.sock".into()),
            true,
        )],
        selected_endpoint: 0,
        selection_epoch: 0,
        catalog: crate::endpoint::Catalog::new(&ConnectTarget::Socket(
            "/unused-layout-test.sock".into(),
        )),
        activation_deadline: None,
        pending_navigation: None,
        pending_releases: Vec::new(),
        selected_generation: 0,
        live: {
            let mut live = LiveState::default();
            live.snapshot = Some(Arc::new(snapshot(40)));
            live
        },
        focus: cx.focus_handle(),
        options: ConnectOptions::default(),
        last_queued_options: None,
        active: false,
        sent_focus: None,
        bounds: Bounds::default(),
        title: crate::WINDOW_TITLE.to_owned(),
        cell_width: 9.,
        hovered_terminal_link: false,
        pressed_terminal_link: None,
        presentation: Default::default(),
        painter: Default::default(),
        marked: String::new(),
        hover: None,
        hover_menu: None,
        local_error: None,
        menu: crate::menu::MenuState::new(cx),
        settings: Default::default(),
        integrations: Default::default(),
        notifications: Default::default(),
        install_warning_shown: false,
        collapsed_repos: Default::default(),
        wheel: WheelAccumulator::default(),
        sidebar_width: None,
        sidebar_drag: None,
        sidebar_preferences: None,
        sidebar_modified: false,
        agent_sort: Default::default(),
        agent_sort_modified: false,
        avatars: None,
        #[cfg(feature = "integration-test")]
        input_probe: crate::smoke::InputProbe::default(),
        sidebar_scroll: Default::default(),
        sidebar_revealed: Default::default(),
        _poll: Task::ready(()),
        _activation: cx.observe_window_activation(window, |_, _, _| {}),
        _appearance: cx.observe_window_appearance(window, |this, _, cx| {
            this.apply_shared_theme(cx);
            cx.notify();
        }),
    }
}

#[gpui::test]
fn palette_rejects_changed_endpoint_epoch_or_generation(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::bind_keys(cx);
        fixture_window(window, cx)
    });
    for reconnect in [false, true] {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.open_palette(false, window, cx));
            window.draw(cx).clear();
        });
        cx.simulate_input("toggle sidebar");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear());
        // Selection paints as a row: the fill spans the list, not the label.
        let row = cx.debug_bounds("palette-row-0").unwrap();
        let status = cx.debug_bounds("palette-status").unwrap();
        assert_eq!(row.size.width, status.size.width);
        assert_eq!(row.left(), status.left());
        view.update(cx, |view, _| {
            assert!(view.menu_target_current());
            if reconnect {
                view.endpoints[view.selected_endpoint].generation += 1;
            } else {
                view.selection_epoch += 1;
            }
            assert!(!view.menu_target_current());
        });
        cx.simulate_keystrokes("enter");
        cx.update(|_, cx| {
            assert!(view.read(cx).sidebar_visible);
            assert!(view.read(cx).menu.page == Some(crate::menu::Page::Palette));
        });
        cx.simulate_keystrokes("escape");
    }
}

#[cfg(test)]
fn check_sidebar(
    fixture: Entity<SidebarFixture>,
    cx: &mut gpui::VisualTestContext,
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use gpui::{Modifiers, MouseButton, MouseDownEvent, point};
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
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
    assert!(spaces.size.height > px(200.));
    assert!(agents.size.height > px(200.));
    assert!(agents.bottom() <= sidebar.bottom());
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
            parent.left() + px(super::CHILD_INDENT - super::ICON_RESERVE)
        );
        assert_eq!(
            name.size.width,
            px(super::LABEL_WIDTH - super::CHILD_INDENT - super::ARROW_RESERVE)
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
            let _ = window.draw(cx);
        });
        assert_eq!(cx.debug_bounds("sidebar").unwrap().size.width, px(target));
        let label = cx.debug_bounds("name-herdr").unwrap();
        assert_eq!(
            label.size.width,
            px(super::LABEL_WIDTH + target - 232. - super::ICON_RESERVE)
        );
        let parent = cx.debug_bounds("name-agent-launcher").unwrap();
        let child = cx.debug_bounds("name-sidebar-child").unwrap();
        assert_eq!(
            parent.size.width,
            label.size.width - px(super::ARROW_RESERVE)
        );
        assert_eq!(
            child.size.width,
            parent.size.width - px(super::CHILD_INDENT) + px(super::ICON_RESERVE)
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
            let _ = window.draw(cx);
        });
        assert_eq!(cx.debug_bounds("sidebar").unwrap().size.width, px(target));
    }
    cx.simulate_resize(size(px(640.), px(600.)));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(cx.debug_bounds("sidebar").unwrap().size.width, px(400.));
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
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
        let _ = window.draw(cx);
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
            window.draw(cx).clear();
            let view = view.read(cx);
            assert_eq!(view.live.snapshot.as_deref(), Some(&before));
            assert_eq!(view.marked, "selection must survive toggle");
            assert!(cx.global::<TextProbes>().0.contains_key(if collapsed {
                "\u{25b8}"
            } else {
                "\u{25be}"
            }));
            assert_eq!(
                view.collapsed_repos
                    .contains("/fixture/agent-launcher/.git"),
                collapsed
            );
            assert_eq!(
                !cx.global::<TextProbes>().0.contains_key("sidebar-child"),
                collapsed
            );
        });
    }
    let menu = cx.debug_bounds("sidebar-menu").unwrap();
    cx.simulate_click(menu.center(), Default::default());
    cx.update(|window, cx| {
        window.draw(cx).clear();
    });
    assert!(cx.debug_bounds("menu-panel").is_some());
    assert!(cx.debug_bounds("menu-reload GUI config").is_some());
    crate::menu::workspace_tests::check_menu_interactions(&view, cx);
    cx.simulate_keystrokes("down down enter");
    cx.update(|window, cx| {
        window.draw(cx).clear();
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
        window.draw(cx).clear();
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
    cx.update(|window, cx| window.draw(cx).clear());
    assert!(cx.debug_bounds("shortcut-New Workspace").unwrap().top() < first_row.top());
    assert_eq!(cx.debug_bounds("keybinds-header").unwrap(), header);
    assert_eq!(cx.debug_bounds("keybinds-footer").unwrap(), footer);
    let close = cx.debug_bounds("keybinds-close").unwrap();
    cx.simulate_click(close.center(), Default::default());
    cx.update(|window, cx| {
        assert!(view.read(cx).menu.page.is_none());
        assert!(view.read(cx).focus.is_focused(window));
        view.update(cx, |view, cx| view.open_keybinds(window, cx));
        window.draw(cx).clear();
    });
    assert_eq!(
        cx.debug_bounds("shortcut-New Workspace").unwrap(),
        first_row
    );
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        window.draw(cx).clear();
        assert!(view.read(cx).menu.page.is_none());
    });
    cx.simulate_click(menu.center(), Default::default());
    cx.update(|window, cx| {
        window.draw(cx).clear();
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
        window.draw(cx).clear();
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Workspace));
        assert_eq!(view.read(cx).live.snapshot.as_deref(), Some(&before));
    });
    assert!(cx.debug_bounds("workspace-menu-Close group").is_some());
    assert!(cx.debug_bounds("workspace-menu-New worktree").is_some());
    // Every action is labelled and pictured, with the icon left of its label.
    for (row, icon) in [
        ("workspace-menu-Rename", "workspace-menu-icon-Rename"),
        (
            "workspace-menu-Close group",
            "workspace-menu-icon-Close group",
        ),
        (
            "workspace-menu-New worktree",
            "workspace-menu-icon-New worktree",
        ),
    ] {
        let label = row;
        let row = cx.debug_bounds(row).unwrap();
        let icon = cx.debug_bounds(icon).unwrap();
        assert_eq!(icon.size, size(px(14.), px(14.)), "{label}");
        assert!(icon.left() >= row.left(), "{label}");
        assert!(icon.right() <= row.right(), "{label}");
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
                window.draw(cx).clear();
            });
            let panel = cx.debug_bounds("menu-panel").unwrap();
            assert!(panel.left() >= px(0.) && panel.right() <= px(width));
            assert!(panel.bottom() <= px(600.));
            assert!(
                panel.size.height < px(320.),
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
                window.draw(cx).clear();
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
        window.draw(cx).clear();
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
        window.draw(cx).clear();
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
        window.draw(cx).clear();
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
        window.draw(cx).clear();
        assert!(!view.read(cx).focus.is_focused(window));
    });
    let line_height = cx.update(|_, cx| super::line_height(&view.read(cx).config.sidebar));
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
        px(super::LABEL_WIDTH - super::ICON_RESERVE)
    );
    assert_eq!(title.right(), detail.right());
    assert_eq!(icon.center().y, title.center().y);
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));
    cx.simulate_keystrokes("cmd-/");
    let shortcut_search = cx.update(|window, cx| {
        window.draw(cx).clear();
        let search = view.read(cx).menu.keybinds_search.as_ref().unwrap().clone();
        assert!(search.read(cx).focus.is_focused(window));
        search
    });
    cx.simulate_input("pane zoom");
    cx.update(|window, cx| {
        window.draw(cx).clear();
        assert_eq!(shortcut_search.read(cx).text(), "pane zoom");
    });
    assert!(cx.debug_bounds("shortcut-Toggle Pane Zoom").is_some());
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("no-shortcut-matches-xyz");
    cx.update(|window, cx| window.draw(cx).clear());
    assert!(cx.debug_bounds("keybinds-empty").is_some());
    cx.simulate_keystrokes("cmd-w");
    cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(crate::menu::Page::Keybinds)));
    cx.simulate_keystrokes("escape cmd-/");
    cx.update(|window, cx| {
        window.draw(cx).clear();
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
    cx.simulate_keystrokes("escape cmd-,");
    // General has enough content to exercise the independent body scroll.
    cx.simulate_keystrokes("shift-tab");
    cx.simulate_resize(size(px(360.), px(240.)));
    cx.update(|window, cx| window.draw(cx).clear());
    let header = cx.debug_bounds("preferences-header").unwrap();
    let footer = cx.debug_bounds("preferences-footer").unwrap();
    let body = cx.debug_bounds("preferences-body").unwrap();
    let theme_row = cx.debug_bounds("preferences-show-agents").unwrap();
    assert!(body.size.height > px(0.));
    assert!(header.bottom() <= body.top());
    assert!(body.bottom() <= footer.top());
    cx.simulate_keystrokes("pagedown");
    cx.update(|window, cx| window.draw(cx).clear());
    assert!(cx.debug_bounds("preferences-show-agents").unwrap().top() < theme_row.top());
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
                window.draw(cx).clear();
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
            let footer = cx.debug_bounds("github-footer").unwrap();
            let body = cx.debug_bounds("github-body").unwrap();
            assert!(body.size.height > px(0.));
            if state != 4 {
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
                cx.update(|window, cx| window.draw(cx).clear());
                assert!(cx.debug_bounds("github-status").unwrap().top() < status.top());
                assert_eq!(cx.debug_bounds("github-footer").unwrap(), footer);
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
    cx.simulate_keystrokes("cmd-,");
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.select_settings_tab(crate::settings_panel::Tab::Theme, window, cx)
        });
    });
    cx.update(|window, cx| window.draw(cx).clear());
    let choose_theme = cx.debug_bounds("preferences-choose-theme").unwrap();
    cx.simulate_click(choose_theme.center(), Default::default());
    cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(crate::menu::Page::Themes)));
    cx.simulate_keystrokes("escape");

    let search = cx.update(|window, cx| {
        view.update(cx, |view, cx| view.open_theme_picker(window, cx));
        window.draw(cx).clear();
        let search = view.read(cx).menu.themes.as_ref().unwrap().search.clone();
        assert!(search.read(cx).focus.is_focused(window));
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("catppuccin mocha".into()));
        search
    });
    cx.simulate_keystrokes("cmd-v");
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear();
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
        window.draw(cx).clear();
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
    cx.update(|window, cx| window.draw(cx).clear());
    assert!(cx.debug_bounds("theme-empty").is_some());
    // Enter with no results must neither write a config nor dismiss the picker.
    cx.simulate_keystrokes("down enter");
    cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(crate::menu::Page::Themes)));
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        assert!(view.read(cx).focus.is_focused(window));
        view.update(cx, |view, cx| view.open_theme_picker(window, cx));
        window.draw(cx).clear();
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
        window.draw(cx).clear();
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
        window.draw(cx).clear();
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
        window.draw(cx).clear();
        assert!(!view.read(cx).sidebar_visible);
        assert!(view.read(cx).menu.page.is_none());
        assert!(view.read(cx).focus.is_focused(window));
    });
    cx.simulate_keystrokes("cmd-b cmd-,");
    cx.update(|_, cx| {
        assert!(view.read(cx).sidebar_visible);
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Preferences));
    });
    cx.simulate_keystrokes("escape cmd-p");
    cx.update(|window, cx| {
        window.draw(cx).clear();
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
                    "../../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
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
        window.draw(cx).clear();
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
        window.draw(cx).clear();
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
        window.draw(cx).clear();
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
                window.draw(cx).clear();
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
        window.draw(cx).clear();
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
        cx.update(|window, cx| window.draw(cx).clear());
        let status = cx.debug_bounds("connection-status").unwrap();
        let report = cx.debug_bounds("report-issue").unwrap();
        assert!(report.size.width > px(50.));
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
    Ok(())
}

#[gpui::test]
fn the_sidebar_follows_the_selection_without_undoing_manual_scrolling(
    cx: &mut gpui::TestAppContext,
) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture_window(window, cx));
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        SidebarFixture(view)
    });
    let view = cx.update(|_, cx| fixture.read(cx).0.clone());
    // The window paints while the connection is still awaiting its first snapshot.
    let mut snapshot = cx
        .update(|_, cx| view.update(cx, |view, _| view.live.snapshot.take()))
        .unwrap();
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear());
    // The fixture's grouped worktrees stay contiguous, so w30 is the 31st row.
    const ROW: usize = 30;
    {
        let snapshot = Arc::make_mut(&mut snapshot);
        snapshot.focused_workspace_id = Some("w30".into());
        for workspace in &mut snapshot.workspaces {
            workspace.focused = workspace.workspace_id == "w30";
        }
    }
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.live.snapshot = Some(snapshot);
            cx.notify();
        })
    });
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    cx.update(|_, cx| {
        let view = view.read(cx);
        let spaces = &view.sidebar_scroll[0];
        let offset = spaces.offset().y;
        let row = spaces.bounds_for_item(ROW).unwrap();
        assert!(offset < px(0.), "focused workspace must scroll into view");
        assert!(row.top() + offset >= spaces.bounds().top(), "{row:?}");
        assert!(row.bottom() + offset <= spaces.bounds().bottom(), "{row:?}");
        // The fixture focuses no agent, so that list must stay where it was.
        assert_eq!(view.sidebar_scroll[1].offset().y, px(0.));
    });
    // While the selection holds, later frames must not fight manual scrolling.
    cx.update(|window, cx| {
        view.read(cx).sidebar_scroll[0].set_offset(point(px(0.), px(0.)));
        window.refresh();
        window.draw(cx).clear();
    });
    cx.update(|_, cx| {
        assert_eq!(view.read(cx).sidebar_scroll[0].offset().y, px(0.));
    });
    // A new selection is revealed in turn, from wherever the list now sits.
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            let snapshot = Arc::make_mut(view.live.snapshot.as_mut().unwrap());
            snapshot.focused_workspace_id = Some("w20".into());
            for workspace in &mut snapshot.workspaces {
                workspace.focused = workspace.workspace_id == "w20";
            }
            cx.notify();
        })
    });
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    cx.update(|_, cx| {
        let view = view.read(cx);
        let spaces = &view.sidebar_scroll[0];
        let offset = spaces.offset().y;
        let row = spaces.bounds_for_item(20).unwrap();
        assert!(offset < px(0.), "a new selection must scroll into view");
        assert!(row.top() + offset >= spaces.bounds().top(), "{row:?}");
        assert!(row.bottom() + offset <= spaces.bounds().bottom(), "{row:?}");
    });
}

#[gpui::test]
fn worktree_rows_wear_their_cached_pull_request(cx: &mut gpui::TestAppContext) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture_window(window, cx));
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        SidebarFixture(view)
    });
    let view = cx.update(|_, cx| fixture.read(cx).0.clone());
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear());
    // Rows w3..w5 are the fixture's worktree group; w4 is a linked checkout.
    let bare = cx.debug_bounds("name-sidebar-child").unwrap();
    assert!(cx.debug_bounds("pr-sidebar-child").is_none());
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            // A standalone checkout too, to compare with a collapsible group row.
            let snapshot = Arc::make_mut(view.live.snapshot.as_mut().unwrap());
            snapshot.workspaces[0].worktree = Some(ClientShellWorktree {
                key: "/fixture/solo/.git".into(),
                label: "solo".into(),
                is_linked_worktree: false,
            });
            let now = std::time::Instant::now();
            for (key, branch, number, state, additions, deletions) in [
                (
                    "/fixture/agent-launcher/.git",
                    "worktree/sidebar-child",
                    7,
                    "MERGED",
                    23,
                    342,
                ),
                ("/fixture/solo/.git", "main", 9, "OPEN", 4, 5),
                // The group's own head, so a row carries arrow and badge both.
                ("/fixture/agent-launcher/.git", "develop", 11, "OPEN", 1, 2),
            ] {
                let mut value = crate::pull_request::fixture().unwrap();
                value.number = number;
                value.state = crate::pull_request::State::from(state.to_owned());
                value.additions = additions;
                value.deletions = deletions;
                view.menu.pr_cache.seed(
                    crate::pull_request::Input {
                        checkout: None,
                        repo_key: key.into(),
                        branch: branch.into(),
                    },
                    value,
                    now,
                );
            }
            cx.notify();
        })
    });
    cx.update(|window, cx| {
        cx.default_global::<TextProbes>().0.clear();
        window.refresh();
        window.draw(cx).clear();
    });
    let badge = cx.debug_bounds("pr-sidebar-child").unwrap();
    let row = cx.debug_bounds("row-sidebar-child").unwrap();
    let name = cx.debug_bounds("name-sidebar-child").unwrap();
    // The badge takes its column from the label, inside the row.
    assert!(badge.right() <= row.right());
    assert!(name.right() <= badge.left());
    assert!(name.size.width < bare.size.width);
    // The tree gutter sits under the parent's label and stops at the child's
    // own dot: lines never reach the text on either side.
    let gutter = cx.debug_bounds("tree-sidebar-child").unwrap();
    let parent_column = cx.debug_bounds("column-agent-launcher").unwrap();
    let child_column = cx.debug_bounds("column-sidebar-child").unwrap();
    assert_eq!(gutter.left(), parent_column.left());
    assert!(gutter.right() <= child_column.left() - px(super::STATUS_WIDTH));
    // Badges hug the row's inner edge, whether or not the row can collapse and
    // whether or not an arrow is drawn in front of them.
    let solo = cx.debug_bounds("pr-herdr").unwrap();
    let head = cx.debug_bounds("pr-agent-launcher").unwrap();
    let arrow = cx.debug_bounds("collapse-3").unwrap();
    for right in [solo.right(), head.right(), badge.right()] {
        assert_eq!(right, row.right() - px(12.), "badges must be flush right");
    }
    assert!(arrow.right() <= head.left(), "{arrow:?} {head:?}");
    cx.update(|_, cx| {
        let probes = &cx.global::<TextProbes>().0;
        for text in ["#7", "+23", "-342", "#9", "+4", "-5"] {
            assert!(
                probes.contains_key(text),
                "missing {text}: {:?}",
                probes.keys()
            );
        }
    });
}

#[gpui::test]
fn worktree_rows_mark_uncommitted_work(cx: &mut gpui::TestAppContext) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture_window(window, cx));
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        SidebarFixture(view)
    });
    let view = cx.update(|_, cx| fixture.read(cx).0.clone());
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear());
    let clean_label = cx.debug_bounds("name-sidebar-child").unwrap();
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            let now = std::time::Instant::now();
            let input = |branch: &str| crate::pull_request::Input {
                checkout: None,
                repo_key: "/fixture/agent-launcher/.git".into(),
                branch: branch.into(),
            };
            // A checkout with a pull request and uncommitted work, and one that
            // only has uncommitted work.
            view.menu.pr_cache.seed(
                input("worktree/sidebar-child"),
                crate::pull_request::fixture().unwrap(),
                now,
            );
            view.git
                .seed_probe(input("worktree/sidebar-child"), true, now);
            view.git.seed_probe(input("develop"), true, now);
            // Answered and clean: no mark, and no column reserved for one.
            view.git.seed_probe(
                input("worktree/sidebar-child-with-a-long-readable-branch-name"),
                false,
                now,
            );
            cx.notify();
        })
    });
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    let row = cx.debug_bounds("row-sidebar-child").unwrap();
    let badge = cx.debug_bounds("pr-sidebar-child").unwrap();
    let dot = cx.debug_bounds("dirty-sidebar-child").unwrap();
    // The mark leads the badge column, still flush against the row's edge.
    assert!(badge.left() <= dot.left() && dot.right() <= badge.right());
    assert_eq!(badge.right(), row.right() - px(12.));
    assert!(cx.debug_bounds("name-sidebar-child").unwrap().right() <= badge.left());
    assert!(clean_label.size.width > cx.debug_bounds("name-sidebar-child").unwrap().size.width);
    // A dirty checkout without a pull request still earns the column.
    let head = cx.debug_bounds("pr-agent-launcher").unwrap();
    let head_dot = cx.debug_bounds("dirty-agent-launcher").unwrap();
    assert_eq!(
        head.right(),
        cx.debug_bounds("row-agent-launcher").unwrap().right() - px(12.)
    );
    assert!(head.left() <= head_dot.left() && head_dot.right() <= head.right());
    assert!(
        cx.debug_bounds("dirty-sidebar-child-with-a-long-readable-branch-name")
            .is_none(),
        "a clean checkout is not marked"
    );
}

#[gpui::test]
fn the_workspace_menu_folds_and_unfolds_a_worktree_group(cx: &mut gpui::TestAppContext) {
    use gpui::{Modifiers, MouseButton};
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        crate::bind_keys(cx);
        let view = cx.new(|cx| fixture_window(window, cx));
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        SidebarFixture(view)
    });
    let view = cx.update(|_, cx| fixture.read(cx).0.clone());
    cx.update(|_, cx| {
        view.update(cx, |view, _| {
            view.live.status = crate::state::ConnectionStatus::Connected;
        })
    });
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear());
    for (item, icon, collapsed) in [
        (
            "workspace-menu-Collapse group",
            "workspace-menu-icon-Collapse group",
            true,
        ),
        (
            "workspace-menu-Expand group",
            "workspace-menu-icon-Expand group",
            false,
        ),
    ] {
        let parent = cx.debug_bounds("row-agent-launcher").unwrap();
        cx.simulate_mouse_down(parent.center(), MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(parent.center(), MouseButton::Right, Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear();
            assert!(view.read(cx).menu.page == Some(crate::menu::Page::Workspace));
        });
        let row = cx
            .debug_bounds(item)
            .unwrap_or_else(|| panic!("missing {item}"));
        assert!(cx.debug_bounds(icon).is_some(), "missing {icon}");
        cx.simulate_click(row.center(), Modifiers::default());
        cx.update(|window, cx| {
            cx.default_global::<TextProbes>().0.clear();
            window.refresh();
            window.draw(cx).clear();
            let view = view.read(cx);
            // Folding is the client's own view of the list, not a daemon request.
            assert!(view.menu.page.is_none(), "{item} left the menu open");
            assert_eq!(
                view.collapsed_repos
                    .contains("/fixture/agent-launcher/.git"),
                collapsed
            );
            assert_eq!(
                !cx.global::<TextProbes>().0.contains_key("sidebar-child"),
                collapsed,
                "{item} did not change the visible children"
            );
        });
    }
}

#[test]
fn child_gutter_lines_land_on_whole_device_pixels() {
    use crate::sidebar::row::{RowTree, tree_lines};
    use gpui::{Bounds, point, size};
    let font = crate::config::FontConfig {
        family: "Menlo".into(),
        size: 12.,
        fallbacks: None,
    };
    for scale in [1., 2., 3.] {
        let row = Bounds::new(point(px(0.), px(244.)), size(px(231.), px(40.)));
        let device = |value: Pixels| f32::from(value) * scale;
        let whole = |value: Pixels| (device(value) - device(value).round()).abs() < 0.001;
        for tree in [RowTree::Child, RowTree::LastChild] {
            let [trunk, tick] = tree_lines(row, tree, &font, scale);
            // Both lines carry the same weight and start on the device grid, so
            // neither is drawn thinner or blurrier than the other.
            assert!(
                (trunk.size.width - tick.size.height).abs() < px(0.01),
                "{scale}"
            );
            assert!(
                (device(trunk.size.width) - scale.round().max(1.)).abs() < 0.01,
                "{scale}"
            );
            for edge in [trunk.left(), trunk.top(), tick.left(), tick.top()] {
                assert!(whole(edge), "{scale}: {edge:?}");
            }
            // The trunk hugs the gutter's leading edge, the tick crosses to the
            // dot at its far edge; neither strays into the label beyond.
            assert_eq!(trunk.left(), tick.left(), "{scale}");
            assert_eq!(trunk.left(), row.left(), "{scale}");
            assert_eq!(tick.right(), row.right(), "{scale}");
            // The tick meets the status dot's middle row.
            let middle = row.top() + px(4. + super::line_height(&font) / 2.);
            assert!(
                (tick.center().y - middle).abs() <= px(1. / scale),
                "{scale}"
            );
            // Only a row with a sibling below carries the trunk to the bottom.
            match tree {
                RowTree::Child => assert_eq!(trunk.bottom(), row.bottom(), "{scale}"),
                _ => assert_eq!(trunk.bottom(), tick.bottom(), "{scale}"),
            }
            assert_eq!(trunk.top(), row.top(), "{scale}");
        }
    }
}

#[gpui::test]
fn the_sidebar_menu_stays_clear_of_the_window_chrome(cx: &mut gpui::TestAppContext) {
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        crate::bind_keys(cx);
        let view = cx.new(|cx| fixture_window(window, cx));
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        SidebarFixture(view)
    });
    let view = cx.update(|_, cx| fixture.read(cx).0.clone());
    let chrome = px(crate::titlebar::HEIGHT
        + crate::worktree_banner::reserved(env!("HERDR_BUILD_WORKTREE") == "1"));
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.run_until_parked();
    // An anchor near the top leaves no room above it, one near the footer
    // plenty; either way the panel stays between the chrome and the bottom.
    for anchor in [chrome + px(100.), px(560.)] {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.menu.anchor = point(px(120.), anchor);
                view.open_menu(window, cx);
            });
            window.draw(cx).clear();
            assert!(view.read(cx).menu.page == Some(crate::menu::Page::Menu));
        });
        let panel = cx.debug_bounds("menu-panel").unwrap();
        // A margin from the chrome and the bottom edge, so a clamped list is
        // visibly a list that scrolls rather than one cut off by the frame.
        assert!(
            panel.top() >= chrome + px(8.),
            "anchor {anchor:?}: {panel:?}"
        );
        assert!(panel.bottom() <= px(592.), "anchor {anchor:?}: {panel:?}");
        // Whatever the room, the list keeps enough height to scroll through.
        assert!(panel.size.height >= px(60.), "anchor {anchor:?}: {panel:?}");
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| window.draw(cx).clear());
    }
}

#[gpui::test]
fn the_agents_header_toggles_between_grouped_and_priority(cx: &mut gpui::TestAppContext) {
    use crate::preferences::AgentSort;
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        crate::bind_keys(cx);
        let view = cx.new(|cx| fixture_window(window, cx));
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        SidebarFixture(view)
    });
    let view = cx.update(|_, cx| fixture.read(cx).0.clone());
    // The second agent wants attention; only priority floats it to the top.
    cx.update(|_, cx| {
        view.update(cx, |view, _| {
            let snapshot = Arc::make_mut(view.live.snapshot.as_mut().unwrap());
            snapshot.agents[0].state_change_seq = 9;
            snapshot.agents[1].agent_status = AgentStatus::Blocked;
            snapshot.agents[1].state_change_seq = 1;
        })
    });
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear());
    let (first, second) = ("row-agent-p0", "row-agent-p1");
    let sort = cx.debug_bounds("agents-sort").unwrap();
    let header = cx.debug_bounds("sidebar").unwrap();
    // The label ends at the sidebar's inner edge, opposite the "agents" title.
    assert_eq!(sort.right(), header.right() - px(13.));
    for (expected, top) in [(AgentSort::Grouped, first), (AgentSort::Priority, second)] {
        cx.update(|_, cx| {
            assert_eq!(view.read(cx).agent_sort, expected);
            let probes = &cx.global::<TextProbes>().0;
            assert!(
                probes.contains_key(expected.to_string().as_str()),
                "{:?}",
                probes.keys()
            );
        });
        let (a, b) = (
            cx.debug_bounds(first).unwrap(),
            cx.debug_bounds(second).unwrap(),
        );
        let ordered = if top == first {
            a.top() < b.top()
        } else {
            b.top() < a.top()
        };
        assert!(ordered, "{expected:?}: {a:?} {b:?}");
        cx.simulate_click(sort.center(), Default::default());
        cx.update(|window, cx| {
            cx.default_global::<TextProbes>().0.clear();
            window.refresh();
            window.draw(cx).clear();
        });
    }
    // Toggling twice returns to the stored default without a daemon request.
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert_eq!(view.agent_sort, AgentSort::Grouped);
        assert!(view.agent_sort_modified);
    });
}

/// Resting the pointer on a workspace opens the menu its right click opens,
/// once, and only after the pointer has both moved and settled. The behavior
/// is opt-in, so the test turns its feature flag on.
#[gpui::test]
fn resting_on_a_workspace_opens_its_menu_once(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::bind_keys(cx);
        let mut view = fixture_window(window, cx);
        view.live.status = crate::state::ConnectionStatus::Connected;
        view.active = true;
        view.config.features.sidebar_hover_menu = true;
        view
    });
    cx.simulate_resize(size(px(900.), px(700.)));
    cx.update(|window, cx| window.draw(cx).clear());
    let row = cx.debug_bounds("row-herdr").unwrap().center();
    let settle = |view: &Entity<HerdrWindow>,
                  cx: &mut gpui::VisualTestContext,
                  elapsed: std::time::Duration| {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.poll_hover_menu(std::time::Instant::now() + elapsed, window, cx);
            });
            window.draw(cx).clear();
        });
    };

    // Entering the row alone is not a rest: the pointer has not moved yet.
    cx.simulate_mouse_move(row, None, Modifiers::default());
    assert!(
        view.read_with(cx, |view, _| view.hover.is_some()),
        "row armed"
    );
    settle(&view, cx, super::HOVER_MENU_DELAY);
    assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));

    // Drifting inside the row restarts the dwell rather than opening early.
    cx.simulate_mouse_move(row + point(px(8.), px(0.)), None, Modifiers::default());
    settle(&view, cx, std::time::Duration::ZERO);
    assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));
    settle(&view, cx, super::HOVER_MENU_DELAY / 2);
    assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));

    settle(&view, cx, super::HOVER_MENU_DELAY);
    view.read_with(cx, |view, _| {
        assert_eq!(view.menu.page, Some(crate::menu::Page::Workspace));
        assert_eq!(crate::menu::workspace_tests::target_id(view), Some("w0"));
        // The same one-shot intent, spent: nothing is left armed behind it.
        assert!(view.hover.is_none());
    });

    // Moving inside the popup keeps it: it is the menu the pointer asked for.
    let panel = cx.debug_bounds("menu-panel").unwrap();
    cx.simulate_mouse_move(panel.center(), None, Modifiers::default());
    settle(&view, cx, super::HOVER_MENU_DELAY);
    view.read_with(cx, |view, _| {
        assert_eq!(view.menu.page, Some(crate::menu::Page::Workspace));
        assert!(view.hover_menu.as_ref().is_some_and(|open| open.inside));
    });

    // Leaving it closes it, with no click anywhere.
    cx.simulate_mouse_move(
        point(panel.right() + px(40.), panel.bottom() + px(40.)),
        None,
        Modifiers::default(),
    );
    settle(&view, cx, std::time::Duration::ZERO);
    view.read_with(cx, |view, _| {
        assert!(view.menu.page.is_none());
        assert!(view.hover_menu.is_none());
    });

    // Leaving a menu for a row above it keeps that row's dwell, so the pointer
    // can walk up the list from one menu to the next without a click.
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_workspace_menu("w0", point(px(20.), px(20.)), window, cx);
            view.hover_menu = Some(super::HoverMenu {
                position: window.mouse_position() + point(px(60.), px(60.)),
                inside: false,
            });
            view.hover_workspace("w1", true, window);
            view.poll_hover_menu(std::time::Instant::now(), window, cx);
            assert!(view.menu.page.is_none());
            assert!(view.hover.is_some(), "the next row keeps its dwell");
            assert!(view.hover_menu.is_none());
        });
        window.draw(cx).clear();
    });

    // A menu opened any other way is not the pointer's to close.
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_workspace_menu("w0", point(px(20.), px(20.)), window, cx);
        });
        window.draw(cx).clear();
    });
    cx.simulate_mouse_move(point(px(700.), px(600.)), None, Modifiers::default());
    settle(&view, cx, super::HOVER_MENU_DELAY);
    view.read_with(cx, |view, _| {
        assert_eq!(view.menu.page, Some(crate::menu::Page::Workspace));
        assert!(view.hover_menu.is_none());
    });
    cx.simulate_keystrokes("escape");

    // Dismissing must not let a still pointer reopen the menu.
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));
    for _ in 0..3 {
        settle(&view, cx, super::HOVER_MENU_DELAY);
        assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));
    }

    // Scrolling slides another row under the pointer without a hover event of
    // its own, so the row it entered can no longer speak for what it covers.
    let away = point(px(700.), px(400.));
    cx.simulate_mouse_move(away, None, Modifiers::default());
    cx.simulate_mouse_move(row, None, Modifiers::default());
    cx.simulate_mouse_move(row + point(px(4.), px(4.)), None, Modifiers::default());
    assert!(
        view.read_with(cx, |view, _| view.hover.is_some()),
        "row armed"
    );
    cx.update(|_, cx| {
        view.read(cx).sidebar_scroll[0].set_offset(point(px(0.), px(-40.)));
    });
    settle(&view, cx, super::HOVER_MENU_DELAY);
    view.read_with(cx, |view, _| {
        assert!(view.menu.page.is_none());
        assert!(view.hover.is_none());
    });

    // An inactive window keeps its menus closed under the same pointer.
    cx.update(|_, cx| {
        view.update(cx, |view, _| view.active = false);
        view.read(cx).sidebar_scroll[0].set_offset(point(px(0.), px(0.)));
    });
    cx.simulate_mouse_move(away, None, Modifiers::default());
    cx.simulate_mouse_move(row, None, Modifiers::default());
    cx.simulate_mouse_move(row + point(px(0.), px(6.)), None, Modifiers::default());
    assert!(
        view.read_with(cx, |view, _| view.hover.is_some()),
        "row armed"
    );
    settle(&view, cx, super::HOVER_MENU_DELAY);
    assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));
}

/// Preferences lists every feature flag, in both states: the config file is
/// the only place a flag is turned on.
#[gpui::test]
fn preferences_list_feature_flags(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::bind_keys(cx);
        fixture_window(window, cx)
    });
    cx.simulate_resize(size(px(900.), px(1200.)));
    for enabled in [false, true] {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.config.features.sidebar_hover_menu = enabled;
                view.open_preferences(window, cx);
                view.select_settings_tab(crate::settings_panel::Tab::General, window, cx);
            });
            window.draw(cx).clear();
        });
        let body = cx.debug_bounds("preferences-body").unwrap();
        for (id, label, _) in crate::preferences::feature_rows(&Default::default()) {
            let bounds = cx.debug_bounds(id).unwrap_or_else(|| panic!("{label} row"));
            // Other settings can place feature flags below the initial viewport.
            cx.update(|window, cx| {
                let scroll = &view.read(cx).menu.preferences_scroll;
                scroll.set_offset(
                    scroll.offset() + point(px(0.), body.center().y - bounds.center().y),
                );
                window.refresh();
                window.draw(cx).clear();
            });
            let bounds = cx.debug_bounds(id).unwrap_or_else(|| panic!("{label} row"));
            assert!(
                body.contains(&bounds.center()),
                "{label} row outside the body"
            );
        }
    }
}

#[gpui::test]
fn preferences_tabs_stay_on_one_row_and_font_input_keeps_native_focus(
    cx: &mut gpui::TestAppContext,
) {
    use crate::settings_panel::Tab;
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::bind_keys(cx);
        fixture_window(window, cx)
    });
    cx.simulate_resize(size(px(320.), px(400.)));
    cx.update(|window, cx| {
        window.focus(&view.read(cx).focus);
        window.draw(cx).clear();
    });
    cx.simulate_keystrokes("cmd-,");
    // Opening the modal changes the focus tree; paint it before routing the next key.
    cx.update(|window, cx| {
        window.draw(cx).clear();
        assert!(view.read(cx).menu.page == Some(crate::menu::Page::Preferences));
        assert!(view.read(cx).menu.focus.is_focused(window));
    });
    cx.simulate_keystrokes("shift-tab");
    cx.update(|window, cx| {
        assert_eq!(view.read(cx).settings.tab, Tab::General);
        window.draw(cx).clear();
    });
    let panel = cx.debug_bounds("menu-panel").unwrap();
    let tabs = cx.debug_bounds("preferences-tabs").unwrap();
    assert!(tabs.left() >= panel.left() && tabs.right() <= panel.right());
    let first = cx.debug_bounds("preferences-tab-Theme").unwrap();
    for selector in [
        "preferences-tab-Theme",
        "preferences-tab-Indicators",
        "preferences-tab-Sound",
        "preferences-tab-Toasts",
        "preferences-tab-Integrations",
        "preferences-tab-Font",
        "preferences-tab-General",
    ] {
        let bounds = cx.debug_bounds(selector).unwrap();
        assert_eq!(bounds.top(), first.top(), "tabs must never wrap");
        assert_eq!(bounds.size.height, first.size.height);
    }
    let selected = cx.debug_bounds("preferences-tab-General").unwrap();
    assert!(selected.left() >= tabs.left() && selected.right() <= tabs.right());
    assert!(view.read_with(cx, |view, _| view.settings.tabs_scroll.offset().x < px(0.)));
    cx.simulate_keystrokes("tab");
    cx.update(|window, cx| {
        assert_eq!(view.read(cx).settings.tab, Tab::Theme);
        view.update(cx, |view, cx| {
            view.select_settings_tab(Tab::Font, window, cx)
        });
        window.draw(cx).clear();
    });
    assert!(cx.debug_bounds("preferences-font-terminal").is_some());
    let input = cx.debug_bounds("font-input-sidebar").unwrap();
    cx.simulate_click(input.center(), Default::default());
    cx.simulate_keystrokes("cmd-a M e n l o tab");
    cx.update(|_, cx| assert_eq!(view.read(cx).settings.tab, Tab::Font));
    // Escape first leaves the editor, then Tab resumes section navigation.
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("tab");
    cx.update(|_, cx| assert_eq!(view.read(cx).settings.tab, Tab::General));
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));
}

#[gpui::test]
fn preferences_tabs_fit_desktop_and_shared_details_only_appear_in_general(
    cx: &mut gpui::TestAppContext,
) {
    use crate::settings_panel::Tab;
    let (view, cx) = cx.add_window_view(fixture_window);
    cx.simulate_resize(size(px(1200.), px(850.)));
    for font_size in [12., 16.] {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.config.ui.size = font_size;
                view.menu.page = Some(crate::menu::Page::Preferences);
                view.select_settings_tab(Tab::Theme, window, cx);
            });
            window.draw(cx).clear();
        });
        let tabs = cx.debug_bounds("preferences-tabs").unwrap();
        let first = cx.debug_bounds("preferences-tab-Theme").unwrap();
        let last = cx.debug_bounds("preferences-tab-General").unwrap();
        assert_eq!(first.top(), last.top());
        assert!(first.left() >= tabs.left() && last.right() <= tabs.right());
    }
    for tab in [
        Tab::Theme,
        Tab::Indicators,
        Tab::Sound,
        Tab::Toasts,
        Tab::General,
    ] {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.select_settings_tab(tab, window, cx));
            window.draw(cx).clear();
        });
        assert_eq!(
            cx.debug_bounds("preferences-shared-path").is_some(),
            tab == Tab::General,
        );
        assert_eq!(
            cx.debug_bounds("preferences-reload-shared").is_some(),
            tab == Tab::General,
        );
    }
}

/// Without its feature flag, a resting pointer arms nothing and opens nothing:
/// only a right click still opens a space's menu.
#[gpui::test]
fn resting_on_a_workspace_opens_nothing_by_default(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::bind_keys(cx);
        let mut view = fixture_window(window, cx);
        view.live.status = crate::state::ConnectionStatus::Connected;
        view.active = true;
        view
    });
    assert!(!crate::config::Config::default().features.sidebar_hover_menu);
    cx.simulate_resize(size(px(900.), px(700.)));
    cx.update(|window, cx| window.draw(cx).clear());
    let row = cx.debug_bounds("row-herdr").unwrap().center();

    cx.simulate_mouse_move(row, None, Modifiers::default());
    cx.simulate_mouse_move(row + point(px(6.), px(0.)), None, Modifiers::default());
    assert!(
        view.read_with(cx, |view, _| view.hover.is_none()),
        "row armed"
    );
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            // A stale rest from before the flag was turned off still expires.
            view.hover_workspace("w0", true, window);
            view.poll_hover_menu(
                std::time::Instant::now() + super::HOVER_MENU_DELAY * 2,
                window,
                cx,
            );
            assert!(view.menu.page.is_none());
            assert!(view.hover.is_none());
            assert!(view.hover_menu.is_none());
        });
        window.draw(cx).clear();
    });
}

/// Draw one preview state in a freshly opened panel and return its bounds.
#[cfg(test)]
fn draw_update_state(
    cx: &mut gpui::VisualTestContext,
    view: &Entity<HerdrWindow>,
    state: &crate::updater::State,
) -> (Bounds<Pixels>, Option<Bounds<Pixels>>) {
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_app_update(false, window, cx);
            view.update_preview = Some(state.clone());
            cx.notify();
        });
        window.draw(cx).clear();
    });
    let panel = cx.debug_bounds("app-update-panel").unwrap();
    (panel, cx.debug_bounds("app-update-action"))
}

// `debug_bounds` keeps the last frame that drew an element, so a state that
// must show no button is only provable before any button has been drawn.
#[gpui::test]
fn a_homebrew_upgrade_in_progress_offers_nothing_to_interrupt(cx: &mut gpui::TestAppContext) {
    use crate::updater::State;
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        crate::bind_keys(cx);
        let view = cx.new(|cx| fixture_window(window, cx));
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        SidebarFixture(view)
    });
    let view = cx.update(|_, cx| fixture.read(cx).0.clone());
    // Homebrew output is arbitrary length; a long line must not burst the panel.
    let states = [
        State::Upgrading {
            detail: "==> Downloading ".to_owned() + &"herdr".repeat(24),
        },
        State::Restarting,
    ];
    for (width, height) in [(320., 360.), (800., 600.)] {
        cx.simulate_resize(size(px(width), px(height)));
        for state in &states {
            let (panel, action) = draw_update_state(cx, &view, state);
            assert!(action.is_none(), "{state:?} cannot be interrupted");
            assert!(
                panel.left() >= px(0.) && panel.right() <= px(width),
                "{state:?}: {panel:?}"
            );
            assert!(
                panel.top() >= px(0.) && panel.bottom() <= px(height),
                "{state:?}: {panel:?}"
            );
            cx.update(|window, cx| {
                view.update(cx, |view, cx| view.dismiss_menu(window, cx));
                window.draw(cx).clear();
            });
        }
    }
}

#[gpui::test]
fn the_homebrew_update_states_stay_inside_the_panel(cx: &mut gpui::TestAppContext) {
    use crate::updater::State;
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        crate::bind_keys(cx);
        let view = cx.new(|cx| fixture_window(window, cx));
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        SidebarFixture(view)
    });
    let view = cx.update(|_, cx| fixture.read(cx).0.clone());
    let disabled = cx.update(|_, cx| view.read(cx).updater.state().clone());
    let states = [
        State::Homebrew {
            version: "9999.0.0".into(),
        },
        State::Restart {
            version: "9999.0.0".into(),
        },
    ];
    for (width, height) in [(320., 360.), (320., 600.), (800., 600.)] {
        cx.simulate_resize(size(px(width), px(height)));
        for state in &states {
            let (panel, action) = draw_update_state(cx, &view, state);
            let action = action.unwrap();
            let footer = cx.debug_bounds("app-update-footer").unwrap();
            assert!(
                panel.left() >= px(0.) && panel.right() <= px(width),
                "{state:?}: {panel:?}"
            );
            assert!(
                panel.top() >= px(0.) && panel.bottom() <= px(height),
                "{state:?}: {panel:?}"
            );
            assert!(
                action.left() >= panel.left() && action.right() <= panel.right(),
                "{state:?}: {action:?}"
            );
            assert!(
                action.top() >= footer.top() && action.bottom() <= footer.bottom(),
                "{state:?}: {action:?}"
            );
            cx.simulate_click(action.center(), Default::default());
            // A preview click must never reach the real update service.
            cx.update(|_, cx| assert_eq!(view.read(cx).updater.state(), &disabled));
            cx.update(|window, cx| {
                view.update(cx, |view, cx| view.dismiss_menu(window, cx));
                window.draw(cx).clear();
            });
        }
    }
}

#[cfg(test)]
#[gpui::test]
fn hiding_agents_reclaims_sidebar_height(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(fixture_window);
    for width in [800., 360.] {
        cx.simulate_resize(size(px(width), px(600.)));
        let mut visible_height = px(0.);
        for show_agents in [true, false, true] {
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.config.show_agents = show_agents;
                    cx.notify();
                });
                cx.default_global::<TextProbes>().0.clear();
                window.refresh();
                window.draw(cx).clear();
            });
            cx.update(|_, cx| {
                assert_eq!(
                    cx.global::<TextProbes>().0.contains_key("Claude Code"),
                    show_agents
                );
            });
            let spaces = cx.debug_bounds("spaces-scroll").unwrap();
            if show_agents {
                visible_height = spaces.size.height;
            } else {
                assert!(spaces.size.height > visible_height + px(100.));
            }
            assert!(cx.debug_bounds("sidebar-menu").is_some());
            assert!(cx.debug_bounds("sidebar-resize").is_some());
        }
    }
}

#[cfg(test)]
#[gpui::test]
fn hiding_agents_preserves_scrolled_multi_endpoint_lists(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut view = fixture_window(window, cx);
        let snapshot = Arc::make_mut(view.live.snapshot.as_mut().unwrap());
        snapshot.agents = (0..8)
            .map(|i| {
                let mut agent = snapshot.agents[0].clone();
                agent.pane_id = format!("p{i}");
                agent
            })
            .collect();
        let mut remote = crate::endpoint::Endpoint::new(
            "ssh:test".into(),
            "Remote".into(),
            ConnectTarget::Socket("/unused-remote-layout-test.sock".into()),
            true,
        );
        remote.live.snapshot = view.live.snapshot.clone();
        for agent in &mut Arc::make_mut(remote.live.snapshot.as_mut().unwrap()).agents {
            agent.display_agent = Some("Remote Agent".into());
        }
        view.endpoints.push(remote);
        view
    });
    for width in [800., 360.] {
        cx.simulate_resize(size(px(width), px(600.)));
        cx.update(|window, cx| {
            window.draw(cx).clear();
            for scroll in &view.read(cx).sidebar_scroll {
                scroll.set_offset(point(px(0.), px(-40.)));
            }
            window.refresh();
            window.draw(cx).clear();
        });
        let spaces_height = cx.debug_bounds("spaces-scroll").unwrap().size.height;
        for show_agents in [false, true] {
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.config.show_agents = show_agents;
                    cx.notify();
                });
                cx.default_global::<TextProbes>().0.clear();
                window.refresh();
                window.draw(cx).clear();
                for label in ["Claude Code", "Remote Agent"] {
                    assert_eq!(
                        cx.global::<TextProbes>().0.contains_key(label),
                        show_agents,
                        "{label}"
                    );
                }
                let view = view.read(cx);
                for scroll in &view.sidebar_scroll {
                    assert_eq!(scroll.offset(), point(px(0.), px(-40.)));
                }
                assert_eq!(view.selected_endpoint, 0);
                assert_eq!(view.live.snapshot.as_ref().unwrap().agents.len(), 8);
                assert_eq!(
                    view.endpoints[1]
                        .live
                        .snapshot
                        .as_ref()
                        .unwrap()
                        .agents
                        .len(),
                    8
                );
            });
            let height = cx.debug_bounds("spaces-scroll").unwrap().size.height;
            if show_agents {
                assert_eq!(height, spaces_height);
                for selector in ["agent-local-p0", "agent-ssh:test-p0"] {
                    assert!(cx.debug_bounds(selector).is_some());
                }
            } else {
                assert!(height > spaces_height + px(100.));
            }
        }
    }
}
