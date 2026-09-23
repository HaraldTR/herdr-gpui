//! The sidebar: spaces and agents, the rows that show them, and the hover
//! menu a resting pointer opens.

mod agents;
mod hover;
mod layout;
mod metrics;
mod render;
mod row;
mod workspaces;

#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "integration-test"))]
pub(crate) mod layout_tests;

#[cfg(all(feature = "integration-test", target_os = "macos"))]
pub(crate) mod native_tests;

pub(crate) use {
    hover::{HoverMenu, HoverRest},
    metrics::{ARROW_RESERVE, HOST_ARROW_WIDTH, HOST_GAP, ICON_RESERVE, LABEL_GAP},
    row::{compact, github_mark, label_text},
    workspaces::workspace_label,
};

#[cfg(any(test, feature = "integration-test"))]
pub(crate) use metrics::LABEL_WIDTH;

use agents::{agents_sort, sorted_agents, status_indicator};
use metrics::*;
use row::{RowBadge, first_text};
use workspaces::visible_workspace_entries;

#[derive(Clone, Copy)]
pub(crate) enum SidebarDrag {
    Width { start: f32, width: f32 },
    Split,
}
