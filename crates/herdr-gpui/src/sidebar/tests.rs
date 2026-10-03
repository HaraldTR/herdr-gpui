#![allow(clippy::unwrap_used)]

use super::{
    STATUS_DOT_UNKNOWN, STATUS_WIDTH, agent_name,
    agents::{
        Indicators, agent_labels, agent_place, status_indicator, status_style, status_symbol,
    },
    layout_tests,
    render::header,
    row::first_text,
    workspace_label,
    workspaces::workspace_entries,
};
use crate::{
    config::{FontConfig, Theme},
    contrast::Contrast,
    herdr_settings::IndicatorStyle,
};
use herdr_client::protocol::{
    AgentStatus, ClientShellAgent, ClientShellSnapshot, ClientShellWorkspace,
};

#[test]
fn section_headings_use_the_configured_sidebar_font_size() {
    use gpui::{Styled, px};
    for size in [12., 16., 20.] {
        let font = FontConfig {
            family: "Menlo".into(),
            size,
            fallbacks: None,
        };
        for label in ["spaces", "agents"] {
            let mut heading = header(
                label,
                &font,
                &Theme::default(),
                super::layout::for_mode(Default::default()),
            );
            assert_eq!(heading.text_style().font_size, Some(px(size).into()));
        }
    }
}

#[test]
fn hierarchy_uses_git_metadata_and_emits_each_workspace_once() {
    let mut workspaces = layout_tests::snapshot(7).workspaces;
    for workspace in &mut workspaces {
        workspace.worktree = None;
        workspace.label = "same label".into();
        workspace.branch = Some("main".into());
    }
    for (index, key, linked) in [
        (0, "/repo/.git", true),
        (2, "/repo/.git", false),
        (3, "/orphan/.git", true),
        (4, "/repo/.git", true),
        (5, "/other/.git", false),
        (6, "/orphan/.git", true),
    ] {
        workspaces[index].worktree = Some(herdr_client::protocol::ClientShellWorktree {
            key: key.into(),
            label: "same repo name".into(),
            is_linked_worktree: linked,
        });
    }
    workspaces[2].branch = Some("develop".into());
    assert_eq!(
        workspace_entries(&workspaces),
        vec![
            (2, false),
            (0, true),
            (4, true),
            (1, false),
            (3, false),
            (5, false),
            (6, false),
        ]
    );
    workspaces[2].worktree = None;
    assert_eq!(
        workspace_entries(&workspaces),
        (0..7).map(|i| (i, false)).collect::<Vec<_>>()
    );
    assert!(workspace_entries(&[]).is_empty());
}

fn worktree(key: &str, linked: bool) -> Option<herdr_client::protocol::ClientShellWorktree> {
    Some(herdr_client::protocol::ClientShellWorktree {
        key: key.into(),
        label: "repo".into(),
        is_linked_worktree: linked,
    })
}

/// Like the TUI, every checkout that is not a linked worktree is a parent:
/// two plain checkouts of one repository never nest, and only a linked
/// worktree turns a repository into a group.
#[test]
fn every_plain_checkout_is_a_parent_and_only_linked_worktrees_form_groups() {
    let mut workspaces = layout_tests::snapshot(6).workspaces;
    for workspace in &mut workspaces {
        workspace.worktree = None;
    }
    workspaces[0].worktree = worktree("/repo/.git", false);
    workspaces[2].worktree = worktree("/repo/.git", false);
    workspaces[4].worktree = worktree("/repo/.git", false);
    let flat: Vec<_> = (0..6).map(|i| (i, false)).collect();
    assert_eq!(workspace_entries(&workspaces), flat);
    let none = std::collections::HashSet::new();
    assert!(
        super::visible_workspace_entries(&workspaces, &none)
            .iter()
            .all(|entry| entry.2.is_none())
    );

    workspaces[5].worktree = worktree("/repo/.git", true);
    assert_eq!(
        workspace_entries(&workspaces),
        vec![
            (0, false),
            (2, false),
            (4, false),
            (5, true),
            (1, false),
            (3, false)
        ]
    );
    // Each parent folds the group; folding hides only linked worktrees.
    for collapsed in [none.clone(), ["/repo/.git".to_owned()].into()] {
        let entries = super::visible_workspace_entries(&workspaces, &collapsed);
        let groups: Vec<_> = entries
            .iter()
            .filter(|entry| entry.2.as_deref() == Some("/repo/.git"))
            .map(|entry| entry.0)
            .collect();
        assert_eq!(groups, [0, 2, 4]);
        assert_eq!(
            entries.iter().any(|entry| entry.0 == 5),
            collapsed.is_empty()
        );
        assert_eq!(entries.len(), 5 + usize::from(collapsed.is_empty()));
    }
}

/// The daemon keeps an unavailable checkout's saved repository membership
/// across a restart (Herdr #4770). The sidebar groups from that membership
/// alone, so a checkout without a branch or a reachable directory stays in
/// its group, under any parent of its repository.
#[test]
fn unavailable_checkouts_keep_their_saved_group() {
    let mut workspaces = layout_tests::snapshot(7).workspaces;
    for index in [4, 5] {
        workspaces[index].branch = None;
        workspaces[index].new_workspace_cwd = "/missing/checkout".into();
    }
    let expected = vec![
        (0, false),
        (1, false),
        (2, false),
        (3, false),
        (4, true),
        (5, true),
        (6, false),
    ];
    assert_eq!(workspace_entries(&workspaces), expected);
    assert_eq!(workspace_label(&workspaces[4], true), "agent-launcher");
    // An unavailable parent is still a parent.
    workspaces[3].branch = None;
    workspaces[3].new_workspace_cwd = "/missing/main".into();
    assert_eq!(workspace_entries(&workspaces), expected);
    // Without any parent open, restored linked worktrees stand alone.
    workspaces[3].worktree = None;
    assert_eq!(
        workspace_entries(&workspaces),
        (0..7).map(|i| (i, false)).collect::<Vec<_>>()
    );
}

#[test]
fn collapse_uses_repository_identity_without_mutating_selection() {
    let mut workspaces = layout_tests::snapshot(7).workspaces;
    workspaces[4].focused = true;
    let before = workspaces.clone();
    let collapsed = std::collections::HashSet::from([layout_tests::REPO_KEY.into()]);
    let entries = super::visible_workspace_entries(&workspaces, &collapsed);
    assert_eq!(
        entries.iter().map(|entry| entry.0).collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 6]
    );
    assert_eq!(entries.iter().filter(|entry| entry.2.is_some()).count(), 1);
    assert_eq!(workspaces, before);
    workspaces[3].label = "renamed".into();
    assert_eq!(
        super::visible_workspace_entries(&workspaces, &collapsed).len(),
        5
    );
    workspaces.remove(5);
    workspaces.remove(4);
    assert!(
        super::visible_workspace_entries(&workspaces, &collapsed)
            .iter()
            .all(|entry| entry.2.is_none())
    );
    workspaces.remove(3);
    assert_eq!(
        super::visible_workspace_entries(&workspaces, &collapsed).len(),
        4
    );
}

#[test]
fn child_labels_follow_upstream_custom_label_and_branch_rules() {
    let mut workspace = layout_tests::snapshot(1).workspaces.remove(0);
    workspace.branch = Some("worktree/fix-sidebar".into());
    assert_eq!(workspace_label(&workspace, true), "fix-sidebar");
    assert_eq!(workspace_label(&workspace, false), "herdr");
    workspace.custom_label = true;
    assert_eq!(workspace_label(&workspace, true), "herdr");
    workspace.custom_label = false;
    workspace.branch = None;
    assert_eq!(workspace_label(&workspace, true), "herdr");
}

#[test]
fn text_fallback_skips_missing_and_blank_metadata() {
    assert_eq!(first_text([None, Some(" \t"), Some(" main ")], ""), "main");
    assert_eq!(first_text([None, Some("")], ""), "");
    assert_eq!(first_text([Some(" ")], "workspace"), "workspace");
}

#[test]
fn agent_rows_name_their_place_then_their_agent() {
    let mut snapshot = layout_tests::snapshot(1);
    let agent = &mut snapshot.agents[0];
    agent.workspace_id = "w0".into();
    agent.tab_id = "t0".into();
    agent.display_agent = Some("Claude Code".into());
    agent.name = Some("review".into());
    agent.agent = Some("claude".into());
    agent.title = Some("Fix sidebar".into());
    let agent = snapshot.agents[0].clone();
    // Host first when there is one, then the workspace, then the tab. Only
    // the workspace is primary; upstream mutes what sits around it.
    fn labels<'a>(
        snapshot: &'a ClientShellSnapshot,
        host: Option<&'a str>,
    ) -> (Vec<(&'a str, bool)>, &'a str) {
        let agent = &snapshot.agents[0];
        agent_labels(agent_name(agent), agent_place(agent, snapshot), host)
    }
    assert_eq!(
        labels(&snapshot, None),
        (vec![("herdr", true), ("tab 1", false)], "Claude Code")
    );
    assert_eq!(
        labels(&snapshot, Some("remote")),
        (
            vec![("remote", false), ("herdr", true), ("tab 1", false)],
            "Claude Code"
        )
    );
    // One unnamed tab is noise, so only its workspace shows.
    snapshot.tabs.retain(|tab| tab.tab_id == "t0");
    assert_eq!(labels(&snapshot, None).0, vec![("herdr", true)]);
    snapshot.tabs[0].custom_label = true;
    assert_eq!(
        labels(&snapshot, None).0,
        vec![("herdr", true), ("tab 1", false)]
    );
    // The agent name falls back through the same order as upstream.
    for (display, name, kind, title, expected) in [
        (None, Some("review"), Some("claude"), None, "review"),
        (None, None, Some("claude"), Some("Fix sidebar"), "claude"),
        (None, None, None, Some("Fix sidebar"), "Fix sidebar"),
        (None, None, None, None, "agent"),
    ] {
        snapshot.agents[0] = ClientShellAgent {
            display_agent: display.map(str::to_owned),
            name: name.map(str::to_owned),
            agent: kind.map(str::to_owned),
            title: title.map(str::to_owned),
            ..agent.clone()
        };
        assert_eq!(labels(&snapshot, None).1, expected);
    }
    // Without its workspace the agent names the row itself.
    snapshot.workspaces.clear();
    assert_eq!(labels(&snapshot, None), (vec![("agent", true)], ""));
}

#[test]
fn rows_weight_and_dim_their_text_like_upstream() {
    use super::row::{RowKind, row_text};
    use gpui::FontWeight;
    let theme = Theme::default();
    // Agents stay bold whether or not they are the current row; a workspace
    // earns bold only while focused, and hands its branch the accent then.
    for (kind, focused, weight, name, detail) in [
        (
            RowKind::Agent(crate::icons::AgentIcon::Generic),
            false,
            FontWeight::BOLD,
            theme.subtext(),
            theme.muted,
        ),
        (
            RowKind::Agent(crate::icons::AgentIcon::Generic),
            true,
            FontWeight::BOLD,
            theme.foreground,
            theme.muted,
        ),
        (
            RowKind::Workspace,
            false,
            FontWeight::NORMAL,
            theme.subtext(),
            theme.muted,
        ),
        (
            RowKind::Workspace,
            true,
            FontWeight::BOLD,
            theme.foreground,
            theme.primary(),
        ),
    ] {
        assert_eq!(row_text(kind, focused, &theme), (name, weight, detail));
    }
    // Subtext sits between the muted detail and the focused name.
    let brightness = |color: u32| (color >> 16) + ((color >> 8) & 255) + (color & 255);
    assert!(brightness(theme.muted) < brightness(theme.subtext()));
    assert!(brightness(theme.subtext()) < brightness(theme.foreground));
}

const STATUSES: [AgentStatus; 5] = [
    AgentStatus::Working,
    AgentStatus::Blocked,
    AgentStatus::Done,
    AgentStatus::Idle,
    AgentStatus::Unknown,
];

#[test]
fn status_colors_keep_upstream_literals_where_they_already_read() {
    // Upstream's default palette (Catppuccin Mocha), which its status dots use
    // whatever terminal colors are loaded.
    for (status, color) in [
        (AgentStatus::Working, 0xf9e2af),
        (AgentStatus::Blocked, 0xf38ba8),
        (AgentStatus::Done, 0x94e2d5),
        (AgentStatus::Idle, 0xa6e3a1),
    ] {
        for name in ["Default", "Nord", "Dracula", "Catppuccin Mocha"] {
            let mut theme = Theme::builtin(name).unwrap();
            assert_eq!(status_style(status, &theme).2, color, "{name}");
            theme.palette.fill(0x123456);
            let indicators = Indicators::new(None, false, &theme);
            assert_eq!(indicators.style, IndicatorStyle::Dots);
            assert_eq!(indicators.color(status), color);
        }
    }
    let mocha = Theme::builtin("Catppuccin Mocha").unwrap();
    let unknown = status_style(AgentStatus::Unknown, &mocha).2;
    let channels = |color: u32| [16, 8, 0].map(|shift| ((color >> shift) & 255) as i32);
    for (lifted, upstream) in channels(unknown).into_iter().zip(channels(0x6c7086)) {
        assert!((0..=16).contains(&(lifted - upstream)), "{unknown:06x}");
    }
}

#[test]
fn status_symbols_match_upstream_without_changing_status_colors() {
    let mut indicators = Indicators::new(None, false, &Theme::default());
    indicators.style = IndicatorStyle::Symbols;
    for (status, symbol) in [
        (AgentStatus::Working, "\u{25d0}"),
        (AgentStatus::Blocked, "\u{d7}"),
        (AgentStatus::Done, "\u{2713}"),
        (AgentStatus::Idle, "\u{25cb}"),
        (AgentStatus::Unknown, "\u{b7}"),
    ] {
        assert_eq!(status_symbol(status), symbol);
        assert_eq!(
            indicators.color(status),
            status_style(status, &Theme::default()).2
        );
    }
}

#[test]
fn status_slots_are_fixed_for_each_style_and_font_size() {
    use gpui::{Styled, px};
    for size in [6., 8., 12., 12.5, 16., 20., 32.] {
        let font = FontConfig {
            family: "Menlo".into(),
            size,
            fallbacks: None,
        };
        for style in [IndicatorStyle::Dots, IndicatorStyle::Symbols] {
            let mut indicators = Indicators::new(None, false, &Theme::default());
            indicators.style = style;
            let width = match style {
                IndicatorStyle::Dots => STATUS_WIDTH,
                IndicatorStyle::Symbols => size.ceil().max(STATUS_WIDTH),
            };
            assert_eq!(indicators.width(&font), width);
            for status in [
                AgentStatus::Working,
                AgentStatus::Blocked,
                AgentStatus::Done,
                AgentStatus::Idle,
                AgentStatus::Unknown,
            ] {
                let mut slot = status_indicator(status, &font, indicators);
                assert_eq!(slot.style().size.width, Some(px(width).into()));
                if style == IndicatorStyle::Symbols {
                    assert_eq!(slot.text_style().font_size, Some(px(size).into()));
                }
            }
        }
    }
}

#[test]
fn symbol_rows_keep_layout_density_and_expand_child_indent() {
    use super::row::{RowIcon, RowKind, RowTree, row};
    use crate::config::LayoutMode;
    use gpui::{Styled, px};

    let font = FontConfig {
        family: "Menlo".into(),
        size: 20.,
        fallbacks: None,
    };
    let theme = Theme::default();
    for mode in [
        LayoutMode::default(),
        LayoutMode::Classic {
            density: crate::config::Density::Compact,
            style: crate::config::Style::Flat,
        },
    ] {
        let layout = super::layout::for_mode(mode);
        for style in [IndicatorStyle::Dots, IndicatorStyle::Symbols] {
            let mut indicators = Indicators::new(None, false, &theme);
            indicators.style = style;
            for kind in [
                RowKind::Workspace,
                RowKind::Agent(crate::icons::AgentIcon::Generic),
            ] {
                let mut row = row(
                    "density",
                    &[("child", true)],
                    "branch",
                    kind,
                    AgentStatus::Working,
                    indicators,
                    false,
                    super::cell::RowState::default(),
                    RowTree::LastChild,
                    true,
                    RowIcon::None,
                    None,
                    None,
                    None,
                    None,
                    &[],
                    &super::cell::RowContext {
                        indicators,
                        font: &font,
                        theme: &theme,
                        look: layout,
                        width: 160.,
                        host: None,
                    },
                );
                assert_eq!(
                    row.style().padding.left,
                    Some(
                        px(layout.content_x()
                            + layout.density.child_indent()
                            + indicators.width(&font)
                            - STATUS_WIDTH)
                        .into()
                    )
                );
                let lines = if layout.density.child_details() || matches!(kind, RowKind::Agent(_)) {
                    2.
                } else {
                    1.
                };
                assert_eq!(
                    row.style().size.height,
                    Some(px(layout.row_height(super::line_height(&font) * lines)).into())
                );
            }
        }
    }
}

#[test]
fn status_colors_reach_the_contrast_setting_on_every_builtin_theme() {
    for name in Theme::BUILTIN_NAMES {
        for contrast in [Contrast::Standard, Contrast::High] {
            let theme = Theme::builtin(name).unwrap().with_contrast(contrast);
            for status in STATUSES {
                let color = status_style(status, &theme).2;
                for background in [theme.background, theme.surface, theme.active] {
                    let ratio = crate::contrast::ratio(color, background);
                    assert!(
                        ratio >= contrast.mark_ratio(),
                        "{name} {contrast:?} {status:?} on {background:06x}: {ratio}"
                    );
                }
            }
        }
    }
    // Light chrome darkens the pastels rather than keeping upstream's literals.
    let latte = Theme::builtin("Catppuccin Latte").unwrap();
    let mocha = Theme::builtin("Catppuccin Mocha").unwrap();
    for status in STATUSES {
        assert_ne!(
            status_style(status, &latte).2,
            status_style(status, &mocha).2
        );
    }
}

#[test]
fn status_shapes_match_upstream_dots_and_wire_casing() {
    let snapshot = layout_tests::snapshot(1);
    for (wire, status) in [
        ("idle", AgentStatus::Idle),
        ("working", AgentStatus::Working),
        ("blocked", AgentStatus::Blocked),
        ("done", AgentStatus::Done),
        ("unknown", AgentStatus::Unknown),
    ] {
        let mut value = serde_json::to_value(&snapshot.workspaces[0]).unwrap();
        value["agent_status"] = wire.into();
        let workspace: ClientShellWorkspace = serde_json::from_value(value).unwrap();
        assert_eq!(workspace.agent_status, status);
        let mut value = serde_json::to_value(&snapshot.agents[0]).unwrap();
        value["agent_status"] = wire.into();
        let agent: ClientShellAgent = serde_json::from_value(value).unwrap();
        assert_eq!(agent.agent_status, status);
        assert_eq!(serde_json::to_value(status).unwrap(), wire);
        let theme = Theme::default();
        let (diameter, filled, color) = status_style(status, &theme);
        assert_eq!(
            color,
            theme.ink(match status {
                AgentStatus::Working => 0xf9e2af,
                AgentStatus::Blocked => 0xf38ba8,
                AgentStatus::Done => 0x94e2d5,
                AgentStatus::Idle => 0xa6e3a1,
                AgentStatus::Unknown => 0x6c7086,
            })
        );
        assert_eq!(filled, status != AgentStatus::Idle);
        assert_eq!(
            diameter,
            if status == AgentStatus::Unknown {
                STATUS_DOT_UNKNOWN
            } else {
                STATUS_WIDTH
            }
        );
    }
}

#[test]
fn cells_hand_their_state_and_data_to_the_layout() {
    use super::{
        cell::{AgentRow, Cell, RowContext, RowData, RowLayout, RowState, WorkspaceRow},
        layout::for_mode,
        row::{RowIcon, RowTree},
    };
    use gpui::{Div, div};
    use std::cell::RefCell;

    /// Records what each call was given instead of drawing it.
    #[derive(Default)]
    struct Recorder(RefCell<Vec<(String, RowState)>>);

    impl RowLayout for Recorder {
        fn workspace(&self, row: WorkspaceRow<'_>, state: RowState, _: &RowContext<'_>) -> Div {
            self.0.borrow_mut().push((row.label.to_owned(), state));
            div()
        }
        fn agent(&self, row: AgentRow<'_>, state: RowState, _: &RowContext<'_>) -> Div {
            self.0.borrow_mut().push((row.key, state));
            div()
        }
    }

    let snapshot = layout_tests::snapshot(1);
    let (font, theme) = (crate::config::Config::default().sidebar, Theme::default());
    let cx = RowContext {
        indicators: Indicators::new(None, false, &theme),
        font: &font,
        theme: &theme,
        look: for_mode(Default::default()),
        width: 232.,
        host: None,
    };
    let recorder = Recorder::default();
    let workspace = || {
        RowData::Workspace(WorkspaceRow {
            workspace: &snapshot.workspaces[0],
            label: "herdr",
            tree: RowTree::None,
            icon: RowIcon::None,
            fold: None,
            grouped: false,
            badge: None,
            removing: false,
            status: snapshot.workspaces[0].agent_status,
            lines: vec![],
        })
    };
    let _ = Cell::new(&recorder, workspace(), &cx).row();
    let _ = Cell::new(&recorder, workspace(), &cx).selected(true).row();
    let _ = Cell::new(
        &recorder,
        RowData::Agent(AgentRow {
            key: "agent-p0".into(),
            name: "Claude Code",
            icon: crate::icons::AgentIcon::Generic,
            status: AgentStatus::Working,
            place: None,
            status_text: None,
            lines: vec![],
        }),
        &cx,
    )
    .highlighted(true)
    .row();
    let state = |selected, highlighted| RowState {
        selected,
        highlighted,
        ..RowState::default()
    };
    assert_eq!(
        recorder.0.into_inner(),
        vec![
            ("herdr".into(), state(false, false)),
            ("herdr".into(), state(true, false)),
            ("agent-p0".into(), state(false, true)),
        ]
    );
}

#[test]
fn upstream_counts_show_only_drift_and_size_to_what_they_print() {
    use super::row::Upstream;
    // In sync, no upstream, or a daemon config without `git_status`.
    assert_eq!(Upstream::new(None), None);
    assert_eq!(Upstream::new(Some((0, 0))), None);
    // `↓18`, `↑2`, and `↑2 ↓18` at one unit per glyph.
    assert_eq!(Upstream::new(Some((0, 18))).unwrap().width(1.), 3.);
    assert_eq!(Upstream::new(Some((2, 0))).unwrap().width(1.), 2.);
    assert_eq!(Upstream::new(Some((2, 18))).unwrap().width(1.), 6.);
    assert_eq!(Upstream::new(Some((2, 18))).unwrap().width(7.5), 45.);
}
