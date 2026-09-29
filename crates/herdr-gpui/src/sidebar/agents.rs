//! The agents list: how it is sorted, and how each agent's place and status
//! are labelled. Status comes from the daemon's snapshot, never from guessing
//! at terminal output.

use super::{STATUS_DOT_UNKNOWN, STATUS_WIDTH, first_text, label_text, line_height};
use crate::{
    HerdrWindow,
    config::{FontConfig, Theme},
};
use gpui::{prelude::*, *};
use herdr_client::protocol::{AgentStatus, ClientShellAgent, ClientShellSnapshot};

pub(super) fn agents_sort(window: &HerdrWindow, cx: &mut Context<HerdrWindow>) -> Stateful<Div> {
    let theme = &window.theme;
    let view = window
        .live
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.agent_view_label.clone());
    let label = view
        .clone()
        .unwrap_or_else(|| window.agent_sort.to_string());
    div()
        .id("agents-sort")
        .debug_selector(|| "agents-sort".into())
        .flex_none()
        .min_w_0()
        .truncate()
        .text_color(rgb(theme.muted))
        .when(view.is_none(), |sort| {
            sort.cursor_pointer()
                .hover(|style| style.text_color(rgb(theme.foreground)))
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.agent_sort = this.agent_sort.toggled();
                    this.agent_sort_modified = true;
                    this.save_chrome();
                    cx.notify();
                }))
        })
        .child(label_text(&label))
}

/// Attention first, then the most recent change, as upstream orders it.
pub(super) fn status_priority(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Blocked => 4,
        AgentStatus::Done => 3,
        AgentStatus::Working => 2,
        AgentStatus::Idle => 1,
        AgentStatus::Unknown => 0,
    }
}

/// The agents of one endpoint in the order the panel paints them.
pub(super) fn sorted_agents(
    agents: &[ClientShellAgent],
    sort: crate::preferences::AgentSort,
) -> Vec<&ClientShellAgent> {
    let mut ordered: Vec<_> = agents.iter().collect();
    if sort == crate::preferences::AgentSort::Priority {
        ordered.sort_by_key(|agent| {
            (
                std::cmp::Reverse(status_priority(agent.agent_status)),
                std::cmp::Reverse(agent.state_change_seq),
            )
        });
    }
    ordered
}

/// What an agent is called wherever it is listed.
pub(crate) fn agent_name(agent: &ClientShellAgent) -> &str {
    first_text(
        [
            agent.display_agent.as_deref(),
            agent.name.as_deref(),
            agent.agent.as_deref(),
            agent.title.as_deref(),
        ],
        "agent",
    )
}

/// Where an agent runs: its workspace, and its tab when that earns a place,
/// which upstream decides by the workspace having several tabs or the user
/// naming it. `None` once the agent's workspace has gone.
pub(super) fn agent_place<'a>(
    agent: &ClientShellAgent,
    snapshot: &'a ClientShellSnapshot,
) -> Option<(&'a str, Option<&'a str>)> {
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)?;
    let tabs = snapshot
        .tabs
        .iter()
        .filter(|tab| tab.workspace_id == agent.workspace_id)
        .count();
    let tab = snapshot
        .tabs
        .iter()
        .find(|tab| tab.tab_id == agent.tab_id)
        .filter(|tab| tabs > 1 || tab.custom_label)
        .map(|tab| tab.label.as_str());
    Some((workspace.label.as_str(), tab))
}

/// Upstream's default agent rows: host, workspace and tab on the first line,
/// the agent itself on the second. A pane whose workspace has gone leaves the
/// agent to name the row.
pub(super) fn agent_labels<'a>(
    name: &'a str,
    place: Option<(&'a str, Option<&'a str>)>,
    host: Option<&'a str>,
) -> (Vec<(&'a str, bool)>, &'a str) {
    let Some((workspace, tab)) = place else {
        return (vec![(name, true)], "");
    };
    // Only the workspace carries the row's weight: upstream paints the host and
    // tab around it in its secondary color.
    let segments = [(host, false), (Some(workspace), true), (tab, false)]
        .into_iter()
        .filter_map(|(text, primary)| Some((text?, primary)))
        .filter(|(text, _)| !text.is_empty())
        .collect();
    (segments, name)
}

// Match the expanded upstream shell order, including orphaned linked worktrees.

pub(super) fn status_indicator(status: AgentStatus, font: &FontConfig, theme: &Theme) -> Div {
    // Upstream dots: working/blocked/done filled, idle hollow, unknown a small dot.
    div()
        .size(px(STATUS_WIDTH))
        .mt(px((line_height(font) - STATUS_WIDTH) / 2.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(status_dot(status, theme))
}

/// The status dot alone, sized by its status, for callers that place it
/// themselves.
pub(crate) fn status_dot(status: AgentStatus, theme: &Theme) -> Div {
    let (diameter, filled, color) = status_style(status, theme);
    div()
        .flex_none()
        .size(px(diameter))
        .rounded_full()
        .border_1()
        .border_color(rgb(color))
        .when(filled, |dot| dot.bg(rgb(color)))
}

/// The word the daemon's `state_text` token shows for a status when its
/// sidebar config asks for it. Lowercase, matching the daemon's status names
/// and what the terminal client prints.
pub(super) fn status_text(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Idle => "idle",
        AgentStatus::Unknown => "unknown",
    }
}

/// Keep status hues independent of ANSI slots, but use darker colors on light
/// chrome so small dots and status words remain visible.
pub(super) fn status_style(status: AgentStatus, theme: &Theme) -> (f32, bool, u32) {
    let channel = |shift: u32| ((theme.surface >> shift) & 255) as f32 / 255.;
    let light = 0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0) > 0.5;
    if light {
        return match status {
            AgentStatus::Working => (STATUS_WIDTH, true, 0x875b00),
            AgentStatus::Blocked => (STATUS_WIDTH, true, 0xb52249),
            AgentStatus::Done => (STATUS_WIDTH, true, 0x08796b),
            AgentStatus::Idle => (STATUS_WIDTH, false, 0x34752a),
            AgentStatus::Unknown => (STATUS_DOT_UNKNOWN, true, 0x626880),
        };
    }
    match status {
        AgentStatus::Working => (STATUS_WIDTH, true, 0xf9e2af),
        AgentStatus::Blocked => (STATUS_WIDTH, true, 0xf38ba8),
        AgentStatus::Done => (STATUS_WIDTH, true, 0x94e2d5),
        AgentStatus::Idle => (STATUS_WIDTH, false, 0xa6e3a1),
        AgentStatus::Unknown => (STATUS_DOT_UNKNOWN, true, 0x6c7086),
    }
}
