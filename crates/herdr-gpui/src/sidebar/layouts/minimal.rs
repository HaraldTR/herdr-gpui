//! The least a row can show: its status and its name. Spacing, highlight, and
//! headings follow the configured density and style, so a narrow sidebar
//! keeps every workspace on one line.

use super::{
    super::{
        ARROW_RESERVE,
        cell::{AgentRow, RowContext, RowLayout, RowState, WorkspaceRow},
        line_height,
        row::{RowKind, RowTree, left_behind, row_text},
    },
    parts::{self, Line},
};
use crate::config::Theme;
use gpui::{prelude::*, *};

pub(in super::super) struct Minimal;

/// A row one line tall, laid out by the density and style like Herdr's own.
fn shell(key: &str, state: RowState, indent: f32, line: Line<'_>, cx: &RowContext<'_>) -> Div {
    let look = cx.look;
    div()
        .debug_selector(|| format!("row-{key}"))
        .relative()
        .w_full()
        .flex_none()
        .h(px(look.row_height(line_height(cx.font))))
        .pl(px(look.content_x() + indent))
        .flex()
        .items_center()
        .cursor_pointer()
        .map(|row| look.mark(row, key, state, cx.theme))
        .child(line.into_div())
}

fn name(key: &str, kind: RowKind, state: RowState, teleported: bool, theme: &Theme) -> Div {
    let (color, weight, _) = row_text(kind, state.selected, theme);
    div()
        .debug_selector(|| format!("name-{key}"))
        .font_weight(weight)
        .text_color(rgb(if teleported {
            left_behind(color, theme)
        } else {
            color
        }))
}

impl RowLayout for Minimal {
    fn workspace(&self, row: WorkspaceRow<'_>, state: RowState, cx: &RowContext<'_>) -> Div {
        let theme = cx.theme;
        let density = cx.look.density;
        let indent = if row.tree == RowTree::None {
            0.
        } else {
            density.child_indent()
        };
        // Pull requests and uncommitted work stay off these rows, but a
        // teleported checkout is a copy left behind, so it keeps its mark.
        let teleported = row.badge.as_ref().is_some_and(|badge| badge.teleported);
        let mark = (line_height(cx.font) * 0.75).round().min(15.);
        let line = Line::new(cx.look.content_width(cx.width) - indent, density.gap())
            .fixed(
                cx.indicators.width(cx.font),
                parts::status(row.status(), row.removing, cx),
            )
            .fill(
                name(row.label, RowKind::Workspace, state, teleported, theme),
                row.label,
            )
            .when(teleported, |line| {
                line.fixed(mark, parts::teleported(row.label, mark, theme))
            })
            .when_some(row.fold, |line, fold| {
                let width = ARROW_RESERVE - density.gap();
                line.fixed(width, fold.element(theme).w(px(width)).text_size(px(16.)))
            });
        shell(row.label, state, indent, line, cx)
    }

    fn agent(&self, agent: AgentRow<'_>, state: RowState, cx: &RowContext<'_>) -> Div {
        let (theme, font) = (cx.theme, cx.font);
        let gap = cx.look.density.gap();
        let kind = RowKind::Agent(agent.icon);
        let (color, _, _) = row_text(kind, state.selected, theme);
        let icon = line_height(font).min(12.);
        let line = Line::new(cx.look.content_width(cx.width), gap)
            .fixed(
                cx.indicators.width(font),
                parts::status(agent.status, false, cx),
            )
            .fixed(icon, parts::icon(agent.icon.path(), icon, color))
            .fill(name(&agent.key, kind, state, false, theme), agent.name)
            .when_some(agent.status_text.as_deref(), |line, text| {
                line.label(
                    div()
                        .debug_selector(|| format!("status-{}", agent.key))
                        .text_color(rgb(cx.indicators.color(agent.status))),
                    text,
                    parts::glyph_at(font, font.size),
                    0.5,
                )
            });
        shell(&agent.key, state, 0., line, cx)
    }
}
