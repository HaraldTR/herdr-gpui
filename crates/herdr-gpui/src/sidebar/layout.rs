//! Density and style policy shared by sidebar rows, headings, and their label
//! budgets. A layout name pairs one of each, so every density has a rounded
//! variant without a type per combination.

use super::{CHILD_INDENT, LABEL_GAP, ROW_PADDING, STATUS_WIDTH, cell::RowState, row::RowLift};
use crate::config::{Density, LayoutMode, Style, Theme};
use gpui::{
    Div, InteractiveElement, ParentElement, Styled, div, prelude::FluentBuilder, px, rgb, rgba,
};

pub(super) trait SidebarDensity {
    fn padding(&self) -> f32;
    fn gap(&self) -> f32;
    fn row_padding(&self) -> f32;
    fn child_indent(&self) -> f32;
    fn workspace_details(&self) -> bool;
    fn child_details(&self) -> bool {
        self.workspace_details()
    }
    fn pr_counts(&self) -> bool {
        self.workspace_details()
    }
    fn header_padding(&self) -> f32;
    fn host_padding(&self) -> f32;
    fn footer_padding(&self) -> f32;

    fn tree_gutter(&self) -> f32 {
        self.padding() + STATUS_WIDTH + self.gap()
    }
}

pub(super) struct Comfortable;

impl SidebarDensity for Comfortable {
    fn padding(&self) -> f32 {
        ROW_PADDING
    }
    fn gap(&self) -> f32 {
        LABEL_GAP
    }
    fn row_padding(&self) -> f32 {
        4.
    }
    fn child_indent(&self) -> f32 {
        CHILD_INDENT
    }
    fn workspace_details(&self) -> bool {
        true
    }
    fn header_padding(&self) -> f32 {
        6.
    }
    fn host_padding(&self) -> f32 {
        8.
    }
    fn footer_padding(&self) -> f32 {
        5.
    }
}

/// TUI-like density: branch lines on roots, single-line worktree children,
/// and two-line agents without extra vertical padding between rows.
pub(super) struct Normal;

impl SidebarDensity for Normal {
    fn padding(&self) -> f32 {
        8.
    }
    fn gap(&self) -> f32 {
        6.
    }
    fn row_padding(&self) -> f32 {
        0.
    }
    fn child_indent(&self) -> f32 {
        STATUS_WIDTH + self.gap() + 8.
    }
    fn workspace_details(&self) -> bool {
        true
    }
    fn child_details(&self) -> bool {
        false
    }
    fn pr_counts(&self) -> bool {
        false
    }
    fn header_padding(&self) -> f32 {
        4.
    }
    fn host_padding(&self) -> f32 {
        2.
    }
    fn footer_padding(&self) -> f32 {
        3.
    }
}

pub(super) struct Compact;

impl SidebarDensity for Compact {
    fn padding(&self) -> f32 {
        6.
    }
    fn gap(&self) -> f32 {
        4.
    }
    fn row_padding(&self) -> f32 {
        0.
    }
    fn child_indent(&self) -> f32 {
        STATUS_WIDTH + self.gap() + 8.
    }
    fn workspace_details(&self) -> bool {
        false
    }
    fn header_padding(&self) -> f32 {
        2.
    }
    fn host_padding(&self) -> f32 {
        0.
    }
    fn footer_padding(&self) -> f32 {
        2.
    }
}

/// How a focused or hovered row is marked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Highlight {
    /// The theme's active color across the whole highlight.
    Fill,
    /// A faint foreground wash inside a brighter border, so the border carries
    /// the selection while the label stays legible.
    Outline,
}

/// How section headings spell their label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HeaderCase {
    Lower,
    Title,
}

/// Row shape, independent of density. Spacing scales with the density it is
/// paired with, so a compact rounded sidebar stays tighter than a normal one.
pub(super) trait SidebarStyle {
    /// Horizontal space between the sidebar's edges and a row's highlight.
    fn row_inset(&self, density: &dyn SidebarDensity) -> f32;
    /// Vertical space between neighbouring highlights, split above and below.
    fn row_spacing(&self, density: &dyn SidebarDensity) -> f32;
    /// Vertical padding added inside a row so text clears its highlight edge.
    fn row_padding(&self, density: &dyn SidebarDensity) -> f32;
    fn radius(&self) -> f32;
    fn highlight(&self) -> Highlight;
    fn tree_lines(&self) -> bool;
    fn header_case(&self) -> HeaderCase;
}

/// Edge-to-edge rows, square highlights, and tree lines tying worktrees to
/// their repository.
pub(super) struct Flat;

impl SidebarStyle for Flat {
    fn row_inset(&self, _: &dyn SidebarDensity) -> f32 {
        0.
    }
    fn row_spacing(&self, _: &dyn SidebarDensity) -> f32 {
        0.
    }
    fn row_padding(&self, _: &dyn SidebarDensity) -> f32 {
        0.
    }
    fn radius(&self) -> f32 {
        0.
    }
    fn highlight(&self) -> Highlight {
        Highlight::Fill
    }
    fn tree_lines(&self) -> bool {
        true
    }
    fn header_case(&self) -> HeaderCase {
        HeaderCase::Lower
    }
}

/// Inset rows with rounded, outlined highlights. Worktrees keep their indent
/// but drop tree lines, which would break across the gaps between rows.
pub(super) struct Rounded;

impl Rounded {
    fn trim(density: &dyn SidebarDensity) -> f32 {
        (density.gap() / 3.).round()
    }
}

impl SidebarStyle for Rounded {
    fn row_inset(&self, density: &dyn SidebarDensity) -> f32 {
        density.gap()
    }
    fn row_spacing(&self, density: &dyn SidebarDensity) -> f32 {
        2. * Self::trim(density)
    }
    fn row_padding(&self, density: &dyn SidebarDensity) -> f32 {
        Self::trim(density)
    }
    fn radius(&self) -> f32 {
        crate::config::corners::CONTROL
    }
    fn highlight(&self) -> Highlight {
        Highlight::Outline
    }
    fn tree_lines(&self) -> bool {
        false
    }
    fn header_case(&self) -> HeaderCase {
        HeaderCase::Title
    }
}

/// The density and style a layout name selects, and the geometry derived from
/// both. Painting, hit testing, and label budgets all read these numbers.
#[derive(Clone, Copy)]
pub(super) struct SidebarLook {
    pub(super) density: &'static dyn SidebarDensity,
    pub(super) style: &'static dyn SidebarStyle,
}

/// Group every row joins, so its highlight layer can follow the row's hover.
/// GPUI resolves a group name to the innermost member, so rows can share it.
pub(super) const ROW_GROUP: &str = "sidebar-row";

impl SidebarLook {
    pub(super) fn inset(&self) -> f32 {
        self.style.row_inset(self.density)
    }

    pub(super) fn spacing(&self) -> f32 {
        self.style.row_spacing(self.density)
    }

    /// Vertical padding between a row's highlight edge and its text.
    pub(super) fn row_padding(&self) -> f32 {
        self.density.row_padding() + self.style.row_padding(self.density)
    }

    /// Where row content starts: the highlight's inset plus the density's own
    /// padding inside it. Headings and footers use it too, so they align.
    pub(super) fn content_x(&self) -> f32 {
        self.inset() + self.density.padding()
    }

    /// Width a row's contents can use, after both edges' insets and padding.
    /// The extra pixel is the sidebar's divider.
    pub(super) fn content_width(&self, width: f32) -> f32 {
        width - 1. - 2. * self.content_x()
    }

    /// A row's full height: its content, the padding around it, and its share
    /// of the spacing to the neighbouring rows.
    pub(super) fn row_height(&self, content: f32) -> f32 {
        content + 2. * self.density.row_padding() + self.chrome_height()
    }

    /// Height the style adds around any row, the host rows included.
    pub(super) fn chrome_height(&self) -> f32 {
        2. * self.style.row_padding(self.density) + self.spacing()
    }

    pub(super) fn tree_gutter(&self) -> f32 {
        self.inset() + self.density.tree_gutter()
    }

    pub(super) fn header_label(&self, label: &'static str) -> String {
        match self.style.header_case() {
            HeaderCase::Lower => label.to_owned(),
            HeaderCase::Title => {
                let mut chars = label.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().chain(chars).collect()
                })
            }
        }
    }

    /// Joins `row` to the hover group its highlight follows.
    pub(super) fn hover_group<E: InteractiveElement>(&self, row: E) -> E {
        row.group(ROW_GROUP)
    }

    /// The layer a row paints its focus and hover highlight on. It is
    /// absolutely positioned, so a border or radius never changes the row's
    /// measured geometry, and it must be the row's first child so content
    /// paints above it.
    pub(super) fn highlight(&self, key: &str, state: RowState, theme: &Theme) -> Div {
        let RowState {
            selected: focused,
            highlighted,
            ..
        } = state;
        let inset = px(self.inset());
        let edge = px(self.spacing() / 2.);
        let layer = div()
            .debug_selector(|| format!("highlight-{key}"))
            .absolute()
            .left(inset)
            .right(inset)
            .top(edge)
            .bottom(edge)
            .rounded(px(self.style.radius()));
        match self.style.highlight() {
            Highlight::Fill => {
                let active = theme.active;
                layer
                    .when(focused || highlighted, |layer| layer.bg(rgb(active)))
                    .group_hover(ROW_GROUP, move |s| s.bg(rgb(active)))
            }
            Highlight::Outline => {
                let wash = |alpha: u32| rgba((theme.foreground << 8) | alpha);
                let (hover, selected, border) = (wash(0x0d), wash(0x14), wash(0x40));
                layer
                    .border_1()
                    .border_color(rgba(0))
                    .when(focused, |layer| layer.bg(selected).border_color(border))
                    .when(!focused && highlighted, |layer| layer.bg(hover))
                    .when(!focused, |layer| {
                        layer.group_hover(ROW_GROUP, move |s| s.bg(hover))
                    })
            }
        }
    }
}

impl SidebarLook {
    /// A row's state layer and hover wiring together: the lifted card while
    /// it is carried, and no hover while a carried row passes over it.
    pub(super) fn mark(&self, row: Div, key: &str, state: RowState, theme: &Theme) -> Div {
        match state.lift {
            RowLift::Resting => self
                .hover_group(row)
                .child(self.highlight(key, state, theme)),
            RowLift::Passed => row.child(self.highlight(key, state, theme)),
            RowLift::Lifted => row.child(self.lifted(key, state.selected, theme)),
        }
    }

    /// The highlight layer as a card carried over the list: opaque so the rows
    /// it passes stay hidden, shadowed rather than colored so it reads the same
    /// in every theme, and pulled in from edge-to-edge rows so it looks lifted.
    /// Only the layer changes, so the row's contents stay where they were.
    pub(super) fn lifted(&self, key: &str, focused: bool, theme: &Theme) -> Div {
        let inset = px(self.inset().max(LIFT_INSET));
        let edge = px(self.spacing() / 2.);
        let wash = |alpha: u32| rgba((theme.foreground << 8) | alpha);
        div()
            .debug_selector(|| format!("highlight-{key}"))
            .absolute()
            .left(inset)
            .right(inset)
            .top(edge)
            .bottom(edge)
            .rounded(px(self.style.radius().max(LIFT_RADIUS)))
            .bg(rgb(match self.style.highlight() {
                Highlight::Fill if focused => theme.active,
                _ => theme.surface,
            }))
            .when(self.style.highlight() == Highlight::Outline, |card| {
                card.border_1()
                    .border_color(wash(if focused { 0x40 } else { 0x20 }))
            })
            .shadow_lg()
    }
}

/// How far a lifted card pulls in from rows that run edge to edge.
const LIFT_INSET: f32 = 6.;
/// The least rounding a lifted card gets, even from square rows.
const LIFT_RADIUS: f32 = 4.;

pub(super) fn for_mode(mode: LayoutMode) -> SidebarLook {
    SidebarLook {
        density: match mode.density() {
            Density::Normal => &Normal,
            Density::Compact => &Compact,
            Density::Comfortable => &Comfortable,
        },
        style: match mode.style() {
            Style::Flat => &Flat,
            Style::Rounded => &Rounded,
        },
    }
}
