//! Drawing the diff, unified or side by side, and the switch between them.
//! Every line cell starts a note on its own row, so a side-by-side change is
//! noted on the side the user clicked.
use super::Layout;
use crate::browser::TabId;
use crate::{
    HerdrWindow,
    config::Theme,
    review::{
        diff::{Diff, Kind, Row, SplitRow},
        highlight::Token,
    },
};
use gpui::{prelude::*, *};

/// The old side's share of a side-by-side row until it is dragged, and how
/// narrow either side may get.
pub(super) const EVEN_SPLIT: f32 = 0.5;
const MIN_SIDE: f32 = 0.2;

/// How a row of `kind` is marked and coloured.
fn look(theme: &Theme, kind: Kind) -> (&'static str, Option<Rgba>, u32) {
    let tint = |color: u32| rgba((color << 8) | 0x2c);
    match kind {
        Kind::Added => ("+", Some(tint(theme.palette[2])), theme.foreground),
        Kind::Removed => ("-", Some(tint(theme.palette[1])), theme.foreground),
        Kind::Context => (" ", None, theme.foreground),
        Kind::Hunk | Kind::Meta => ("", None, theme.muted),
        Kind::File => ("", Some(rgb(theme.active)), theme.foreground),
    }
}

/// A row's text; a file header names its file and how it changed.
fn content(diff: &Diff, row: &Row) -> String {
    if row.kind != Kind::File {
        return row.text.clone();
    }
    let name = diff.files.get(row.file).cloned().unwrap_or_default();
    if row.text.is_empty() {
        name
    } else {
        format!("{name} ({})", row.text)
    }
}

/// A token's colour in this theme: the terminal palette's, made readable
/// on the panel and its add and remove tints.
fn token_colour(theme: &Theme, token: Token) -> Rgba {
    rgb(match token {
        Token::Comment => theme.muted,
        Token::String => theme.ink(theme.palette[2]),
        Token::Number | Token::Constant => theme.ink(theme.palette[3]),
        Token::Keyword => theme.ink(theme.palette[5]),
        Token::Type => theme.ink(theme.palette[6]),
        Token::Function => theme.ink(theme.palette[4]),
    })
}

/// A row's code, coloured where its syntax is known.
fn code(theme: &Theme, diff: &Diff, row: &Row) -> AnyElement {
    let text = content(diff, row);
    if row.spans.is_empty() {
        return text.into_any_element();
    }
    let highlights: Vec<_> = row
        .spans
        .iter()
        .filter(|span| span.end <= text.len())
        .map(|span| {
            (
                span.start..span.end,
                HighlightStyle {
                    color: Some(token_colour(theme, span.token).into()),
                    ..HighlightStyle::default()
                },
            )
        })
        .collect();
    StyledText::new(text)
        .with_highlights(highlights)
        .into_any_element()
}

fn number(theme: &Theme, value: Option<u32>) -> Div {
    div()
        .flex_none()
        .w(px(44.))
        .pr_1()
        .flex()
        .justify_end()
        .text_color(rgb(theme.muted))
        .child(value.map(|value| value.to_string()).unwrap_or_default())
}

/// The slot a note's number shows in, empty without one.
fn mark_slot(theme: &Theme, mark: Option<usize>) -> Div {
    div()
        .flex_none()
        .w(px(20.))
        .flex()
        .justify_center()
        .when_some(mark, |slot, mark| {
            slot.child(
                div()
                    .size(px(16.))
                    .rounded_full()
                    .bg(rgb(theme.palette[3]))
                    .text_color(rgb(theme.text_on(theme.palette[3])))
                    .text_size(px(10.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(mark.to_string()),
            )
        })
}

/// Which line number a cell shows: the old one on the left of a
/// side-by-side row, otherwise the new one where there is one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Numbers {
    Both,
    Old,
    New,
}

impl HerdrWindow {
    /// One row of the diff, drawn as a cell that notes row `index` when it
    /// can take a note. `id` keeps cells of one list row distinct.
    fn review_cell(
        &self,
        id: TabId,
        index: usize,
        element: ElementId,
        numbers: Numbers,
        line_height: f32,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = &self.theme;
        let cell = div()
            .id(element)
            .h(px(line_height))
            .flex()
            .items_center()
            .whitespace_nowrap()
            .overflow_hidden();
        let Some((review, loaded)) = self
            .reviews
            .get(&id)
            .and_then(|review| Some((review, review.loaded()?)))
        else {
            return cell;
        };
        let diff = &loaded.diff;
        let Some(row) = diff.rows.get(index) else {
            return cell;
        };
        let (sign, background, text) = look(theme, row.kind);
        let noteable = matches!(
            row.kind,
            Kind::File | Kind::Added | Kind::Removed | Kind::Context
        );
        // An unchanged line shows on both sides; its number shows once.
        let mark = review
            .marks
            .get(&index)
            .copied()
            .filter(|_| !(row.kind == Kind::Context && numbers == Numbers::Old));
        cell.when_some(background, |cell, background| cell.bg(background))
            .when(review.draft == Some(index), |cell| {
                cell.bg(rgb(theme.active))
            })
            .text_color(rgb(text))
            .when(noteable, |cell| {
                cell.cursor_pointer()
                    .hover(|cell| cell.bg(rgb(theme.active)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.begin_review_note(id, index, window, cx);
                    }))
            })
            .child(mark_slot(theme, mark))
            .when(row.kind != Kind::File, |line| {
                let line = match numbers {
                    Numbers::Both => line
                        .child(number(theme, row.old))
                        .child(number(theme, row.new)),
                    Numbers::Old => line.child(number(theme, row.old)),
                    Numbers::New => line.child(number(theme, row.new)),
                };
                line.child(div().flex_none().w(px(16.)).child(sign))
            })
            .when(row.kind == Kind::File, |line| {
                line.font_weight(FontWeight::SEMIBOLD).gap_1()
            })
            .child(div().min_w_0().child(code(theme, diff, row)))
    }

    /// The list's rows in `range`, in the review's layout.
    pub(super) fn review_rows(
        &mut self,
        id: TabId,
        range: std::ops::Range<usize>,
        line_height: f32,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(review) = self.reviews.get(&id) else {
            return Vec::new();
        };
        let Some(loaded) = review.loaded().cloned() else {
            return Vec::new();
        };
        let diff = &loaded.diff;
        let theme = &self.theme;
        range
            .filter_map(|position| {
                let element = match review.layout {
                    Layout::Unified => {
                        let index = position;
                        diff.rows.get(index)?;
                        self.review_cell(
                            id,
                            index,
                            ("review-row", index).into(),
                            Numbers::Both,
                            line_height,
                            cx,
                        )
                        .debug_selector(move || format!("review-row-{index}"))
                        // Tints and the file header span the list, not the text.
                        .w_full()
                        .into_any_element()
                    }
                    Layout::Split => match *review.split.get(position)? {
                        SplitRow::Across(index) => self
                            .review_cell(
                                id,
                                index,
                                ("review-row", index).into(),
                                Numbers::Both,
                                line_height,
                                cx,
                            )
                            .debug_selector(move || format!("review-row-{index}"))
                            .w_full()
                            .into_any_element(),
                        SplitRow::Sides { left, right } => {
                            let side =
                                |this: &Self,
                                 index: Option<usize>,
                                 numbers: Numbers,
                                 cx: &mut Context<Self>| {
                                    let half = match index {
                                        Some(index) => this
                                            .review_cell(
                                                id,
                                                index,
                                                (
                                                    if numbers == Numbers::Old {
                                                        "review-left"
                                                    } else {
                                                        "review-right"
                                                    },
                                                    index,
                                                )
                                                    .into(),
                                                numbers,
                                                line_height,
                                                cx,
                                            )
                                            .debug_selector(move || {
                                                let side = if numbers == Numbers::Old {
                                                    "left"
                                                } else {
                                                    "right"
                                                };
                                                format!("review-{side}-{index}")
                                            }),
                                        // Nothing on this side: a quiet gap.
                                        None => div()
                                            .id(("review-gap", position))
                                            .h(px(line_height))
                                            .bg(rgba((theme.active << 8) | 0x60)),
                                    };
                                    half.flex_1().min_w_0()
                                };
                            div()
                                .id(("review-split", position))
                                .w_full()
                                .flex()
                                .child(
                                    side(self, left, Numbers::Old, cx)
                                        .flex_none()
                                        .w(relative(review.split_ratio)),
                                )
                                .child(
                                    side(self, right, Numbers::New, cx)
                                        .border_l_1()
                                        .border_color(rgb(theme.active)),
                                )
                                .into_any_element()
                        }
                    },
                };
                Some(element)
            })
            .collect()
    }

    /// Moves the line between the sides to the pointer at `x`, within the
    /// list laid out at `bounds`; whether it moved.
    pub(super) fn drag_review_split(
        &mut self,
        id: TabId,
        bounds: Bounds<Pixels>,
        x: Pixels,
    ) -> bool {
        let Some(review) = self.reviews.get_mut(&id) else {
            return false;
        };
        let width = f32::from(bounds.size.width);
        if width <= 0. {
            return false;
        }
        let ratio = (f32::from(x - bounds.left()) / width).clamp(MIN_SIDE, 1. - MIN_SIDE);
        if (ratio - review.split_ratio).abs() < f32::EPSILON {
            return false;
        }
        review.split_ratio = ratio;
        true
    }

    /// Back to even sides.
    pub(super) fn reset_review_split(&mut self, id: TabId) {
        if let Some(review) = self.reviews.get_mut(&id) {
            review.split_ratio = EVEN_SPLIT;
        }
    }

    /// Shows the diff unified or side by side; notes and scroll stay.
    pub(crate) fn set_review_layout(&mut self, id: TabId, layout: Layout, cx: &mut Context<Self>) {
        if let Some(review) = self.reviews.get_mut(&id) {
            review.layout = layout;
        }
        cx.notify();
    }

    /// The two layout icons in the header.
    pub(super) fn render_review_layout(
        &self,
        id: TabId,
        current: Layout,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = &self.theme;
        let (foreground, surface) = (theme.foreground, theme.surface);
        let button =
            |name: &'static str, icon: &'static str, hint: &'static str, layout: Layout| {
                let chosen = layout == current;
                div()
                    .id(name)
                    .debug_selector(move || name.into())
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(crate::config::corners::CONTROL))
                    .cursor_pointer()
                    .when(chosen, |button| button.bg(rgb(theme.active)))
                    .hover(|button| button.bg(rgb(theme.active)))
                    .child(svg().path(icon).size(px(14.)).text_color(rgb(if chosen {
                        theme.foreground
                    } else {
                        theme.muted
                    })))
                    .tooltip(move |_, cx| {
                        cx.new(|_| crate::usage::Hint {
                            text: hint.into(),
                            foreground,
                            surface,
                        })
                        .into()
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_review_layout(id, layout, cx);
                    }))
            };
        div()
            .flex()
            .flex_none()
            .gap_1()
            .child(button(
                "review-layout-unified",
                "icons/diff-unified.svg",
                "Unified",
                Layout::Unified,
            ))
            .child(button(
                "review-layout-split",
                "icons/diff-split.svg",
                "Side by side",
                Layout::Split,
            ))
    }
}
