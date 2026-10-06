//! Cell-grid geometry shared by every rectangle and path the painter draws on
//! the grid, so neighboring primitives meet on the same device pixel.
use crate::config::Theme;
use crate::terminal::cell_colors;
use gpui::{Bounds, Pixels, Point, Size, Window, size};
use herdr_client::protocol::FrameData;

pub(super) fn background_extent(
    grid: Size<Pixels>,
    available: Size<Pixels>,
    cell: Size<Pixels>,
) -> Size<Pixels> {
    let extend = |grid, available, cell| {
        if available > grid && available - grid < cell {
            available
        } else {
            grid
        }
    };
    size(
        extend(grid.width, available.width, cell.width),
        extend(grid.height, available.height, cell.height),
    )
}

/// The remainder below the grid continues the last row only when that row is
/// one background color, as a full-screen app's usually is. A mixed row, such
/// as a colored prompt, would grow segments taller than their separator caps.
pub(super) fn last_row_is_uniform(frame: &FrameData, theme: &Theme) -> bool {
    let Some(last) = usize::from(frame.height).checked_sub(1) else {
        return false;
    };
    let width = usize::from(frame.width);
    let start = last * width;
    let mut colors = frame
        .cells
        .get(start..(start + width).min(frame.cells.len()))
        .unwrap_or_default()
        .iter()
        .map(|cell| cell_colors(cell, theme).1);
    colors
        .next()
        .is_some_and(|first| colors.all(|color| color == first))
}

/// Snaps a rectangle's absolute grid corners before `Bounds` forms its size.
/// Rebuilding a far edge as left + width can cross a half-device-pixel rounding
/// tie, leaving a seam or overlap against the neighboring span's near edge.
pub(super) fn grid_rect(
    window: &Window,
    origin: Point<Pixels>,
    near: Point<Pixels>,
    far: Point<Pixels>,
) -> Bounds<Pixels> {
    let (near, far) = grid_corners(window, origin, near, far);
    Bounds::from_corners(near, far)
}

/// The snapped corners themselves, for geometry that is not a `Bounds`.
pub(super) fn grid_corners(
    window: &Window,
    origin: Point<Pixels>,
    near: Point<Pixels>,
    far: Point<Pixels>,
) -> (Point<Pixels>, Point<Pixels>) {
    (
        window.pixel_snap_point(origin + near),
        window.pixel_snap_point(origin + far),
    )
}
