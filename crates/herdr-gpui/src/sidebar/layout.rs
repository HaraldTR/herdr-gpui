//! Density policy shared by sidebar rows, headings, and their label budgets.

use super::{CHILD_INDENT, LABEL_GAP, ROW_PADDING, STATUS_WIDTH};
use crate::config::LayoutMode;

pub(super) trait SidebarLayout {
    fn padding(&self) -> f32;
    fn gap(&self) -> f32;
    fn row_padding(&self) -> f32;
    fn child_indent(&self) -> f32;
    fn workspace_details(&self) -> bool;
    fn header_padding(&self) -> f32;
    fn host_padding(&self) -> f32;
    fn footer_padding(&self) -> f32;

    fn tree_gutter(&self) -> f32 {
        self.padding() + STATUS_WIDTH + self.gap()
    }
}

pub(super) struct Normal;

impl SidebarLayout for Normal {
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

pub(super) struct Compact;

impl SidebarLayout for Compact {
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

pub(super) fn for_mode(mode: LayoutMode) -> &'static dyn SidebarLayout {
    match mode {
        LayoutMode::Normal => &Normal,
        LayoutMode::Compact => &Compact,
    }
}
