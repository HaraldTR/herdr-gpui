//! The agents list: how it is sorted, and how each agent's place and status
//! are labelled. Status comes from the daemon's snapshot, never from guessing
//! at terminal output.

use super::{STATUS_DOT_UNKNOWN, STATUS_WIDTH, first_text, label_text, line_height};
use crate::{
    HerdrWindow,
    config::FontConfig,
    herdr_settings::{IndicatorStyle, Settings},
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

/// Upstream's default agent rows: host, workspace and tab on the first line,
/// the agent itself on the second. The tab only earns its place when the
/// workspace has more than one or the user named it, as upstream decides.
pub(super) fn agent_labels<'a>(
    agent: &'a ClientShellAgent,
    snapshot: &'a ClientShellSnapshot,
    host: Option<&'a str>,
) -> (Vec<(&'a str, bool)>, &'a str) {
    let name = first_text(
        [
            agent.display_agent.as_deref(),
            agent.name.as_deref(),
            agent.agent.as_deref(),
            agent.title.as_deref(),
        ],
        "agent",
    );
    // A pane whose workspace has gone leaves the agent to name the row.
    let Some(workspace) = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)
        .map(|workspace| workspace.label.as_str())
    else {
        return (vec![(name, true)], "");
    };
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
    // Only the workspace carries the row's weight: upstream paints the host and
    // tab around it in its secondary color.
    let segments = [(host, false), (Some(workspace), true), (tab, false)]
        .into_iter()
        .filter_map(|(text, primary)| Some((text?, primary)))
        .filter(|(text, _)| !text.is_empty())
        .collect();
    (segments, name)
}

/// Resolve the shared palette once per sidebar render, not once per row.
#[derive(Clone, Copy)]
pub(super) struct Indicators {
    pub(super) style: IndicatorStyle,
    colors: [u32; 5],
}

impl Indicators {
    pub(super) fn new(settings: Option<&Settings>, light: bool) -> Self {
        Self {
            style: settings.map_or(IndicatorStyle::Dots, |settings| settings.indicators),
            colors: [
                AgentStatus::Unknown,
                AgentStatus::Idle,
                AgentStatus::Working,
                AgentStatus::Done,
                AgentStatus::Blocked,
            ]
            .map(|status| {
                settings.map_or_else(
                    || status_style(status).2,
                    |settings| settings.status_color(status, light),
                )
            }),
        }
    }

    pub(super) fn color(self, status: AgentStatus) -> u32 {
        self.colors[match status {
            AgentStatus::Unknown => 0,
            AgentStatus::Idle => 1,
            AgentStatus::Working => 2,
            AgentStatus::Done => 3,
            AgentStatus::Blocked => 4,
        }]
    }

    pub(super) fn width(self, font: &FontConfig) -> f32 {
        match self.style {
            IndicatorStyle::Dots => STATUS_WIDTH,
            IndicatorStyle::Symbols => font.size.ceil().max(STATUS_WIDTH),
        }
    }
}

pub(super) fn status_symbol(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working => "\u{25d0}",
        AgentStatus::Blocked => "\u{d7}",
        AgentStatus::Done => "\u{2713}",
        AgentStatus::Idle => "\u{25cb}",
        AgentStatus::Unknown => "\u{b7}",
    }
}

pub(super) fn status_indicator(
    status: AgentStatus,
    font: &FontConfig,
    indicators: Indicators,
) -> Div {
    // Upstream dots: working/blocked/done filled, idle hollow, unknown a small dot.
    let (diameter, filled, _) = status_style(status);
    let color = indicators.color(status);
    let symbol = indicators.style == IndicatorStyle::Symbols;
    let height = if symbol {
        line_height(font)
    } else {
        STATUS_WIDTH
    };
    div()
        .w(px(indicators.width(font)))
        .h(px(height))
        .mt(px((line_height(font) - height) / 2.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .when(symbol, |slot| {
            slot.overflow_hidden()
                .text_size(px(font.size))
                .line_height(px(line_height(font)))
                .text_color(rgb(color))
                .child(status_symbol(status))
        })
        .when(!symbol, |slot| {
            slot.child(
                div()
                    .size(px(diameter))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(color))
                    .when(filled, |dot| dot.bg(rgb(color))),
            )
        })
}

/// Upstream draws status from its own palette, defaulting to Catppuccin Mocha,
/// and never from the terminal's ANSI colors. Matching those literals keeps a
/// dot the same color in both clients whatever terminal theme is loaded, where
/// ANSI slots would drift: Xcode Dark paints its cyan purple.
pub(super) fn status_style(status: AgentStatus) -> (f32, bool, u32) {
    match status {
        AgentStatus::Working => (STATUS_WIDTH, true, 0xf9e2af),
        AgentStatus::Blocked => (STATUS_WIDTH, true, 0xf38ba8),
        AgentStatus::Done => (STATUS_WIDTH, true, 0x94e2d5),
        AgentStatus::Idle => (STATUS_WIDTH, false, 0xa6e3a1),
        AgentStatus::Unknown => (STATUS_DOT_UNKNOWN, true, 0x6c7086),
    }
}

#[cfg(test)]
mod tests {
    use super::{Indicators, status_indicator};
    use crate::{config::FontConfig, herdr_settings::IndicatorStyle};
    use gpui::{Styled, rgb};
    use herdr_client::protocol::AgentStatus;

    #[test]
    fn prepared_custom_palette_keeps_each_authoritative_status_color() {
        let font = FontConfig {
            family: "Menlo".into(),
            size: 16.,
            fallbacks: None,
        };
        for style in [IndicatorStyle::Dots, IndicatorStyle::Symbols] {
            let indicators = Indicators {
                style,
                colors: [0x112233, 0x223344, 0x334455, 0x445566, 0x556677],
            };
            for (status, expected) in [
                (AgentStatus::Unknown, 0x112233),
                (AgentStatus::Idle, 0x223344),
                (AgentStatus::Working, 0x334455),
                (AgentStatus::Done, 0x445566),
                (AgentStatus::Blocked, 0x556677),
            ] {
                assert_eq!(indicators.color(status), expected);
                if style == IndicatorStyle::Symbols {
                    let mut slot = status_indicator(status, &font, indicators);
                    assert_eq!(
                        slot.text_style().as_ref().and_then(|text| text.color),
                        Some(rgb(expected).into())
                    );
                }
            }
        }
    }
}
