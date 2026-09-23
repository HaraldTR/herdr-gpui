//! Selecting painted cells with the pointer, and the text a release copies.
//!
//! Selection is client-local: the daemon owns the terminal, and nothing here
//! sends input to it or asks it for content. Cells are addressed in the grid of
//! the frame that paints them, so a pane selection never leaves its inner rect
//! and a popup selection never reaches the panes underneath it.

use super::{HIDDEN, InputTarget, popup_origin, wheel_target};
use crate::error::{Error, Result};
use herdr_client::protocol::PaneSurfaceFrame;
use std::ops::Range;

/// Terminal content is untrusted and one cell's symbol carries as many bytes as
/// it likes, so a copy is bounded rather than trusted to be screen-sized.
const MAX_SELECTION_BYTES: usize = 4 << 20;

/// Which half of a cell the pointer sat in. Anchoring on the half, as terminal
/// emulators do, is what makes a single cell selectable while a plain click,
/// which never leaves the half it started in, selects nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    Left,
    Right,
}

/// A pointer position on the cell grid. Ordered by reading order, so the two
/// ends of a drag sort into a start and an end whichever way it was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Edge {
    row: u16,
    column: u16,
    side: Side,
}

/// The cells one target owns, in its own frame's grid, and where that grid is
/// painted relative to the canvas origin.
struct Region {
    columns: Range<u16>,
    rows: Range<u16>,
    origin: (f32, f32),
}

impl Region {
    /// The cell edge under a pointer, clamped into the region so that dragging
    /// out of it selects up to its boundary instead of ending the gesture.
    fn edge(&self, x: f32, y: f32, cell_width: f32, cell_height: f32) -> Option<Edge> {
        if !x.is_finite()
            || !y.is_finite()
            || !cell_width.is_finite()
            || !cell_height.is_finite()
            || cell_width <= 0.
            || cell_height <= 0.
            || self.columns.is_empty()
            || self.rows.is_empty()
        {
            return None;
        }
        let (last_row, last_column) = (self.rows.end - 1, self.columns.end - 1);
        let row = ((y - self.origin.1) / cell_height).floor();
        if row < f32::from(self.rows.start) {
            return Some(Edge {
                row: self.rows.start,
                column: self.columns.start,
                side: Side::Left,
            });
        }
        if row > f32::from(last_row) {
            return Some(Edge {
                row: last_row,
                column: last_column,
                side: Side::Right,
            });
        }
        let row = row as u16;
        let column = (x - self.origin.0) / cell_width;
        if column < f32::from(self.columns.start) {
            return Some(Edge {
                row,
                column: self.columns.start,
                side: Side::Left,
            });
        }
        if column >= f32::from(self.columns.end) {
            return Some(Edge {
                row,
                column: last_column,
                side: Side::Right,
            });
        }
        Some(Edge {
            row,
            column: column.floor() as u16,
            side: if column.fract() < 0.5 {
                Side::Left
            } else {
                Side::Right
            },
        })
    }
}

/// Share popup isolation and pane bounds with input and link resolution.
fn region(
    surface: &PaneSurfaceFrame,
    target: &InputTarget,
    cell_width: f32,
    cell_height: f32,
) -> Option<Region> {
    match target {
        InputTarget::Popup(terminal_id) => {
            let popup = surface
                .popup
                .as_ref()
                .filter(|popup| popup.terminal_id == *terminal_id)?;
            let origin = popup_origin(&surface.frame, &popup.frame, cell_width, cell_height);
            Some(Region {
                columns: 0..popup.frame.width,
                rows: 0..popup.frame.height,
                origin: (f32::from(origin.x), f32::from(origin.y)),
            })
        }
        // A popup covers the panes: what was selected under it is off screen.
        InputTarget::Pane(_) if surface.popup.is_some() => None,
        InputTarget::Pane(pane_id) => {
            let rect = surface
                .panes
                .iter()
                .find(|pane| pane.pane_id == *pane_id)?
                .inner_rect;
            Some(Region {
                columns: rect.x..rect.x.saturating_add(rect.width),
                rows: rect.y..rect.y.saturating_add(rect.height),
                origin: (0., 0.),
            })
        }
    }
}

/// Cells the pointer has chosen in one pane or popup. The region is resolved
/// against the surface on every use, so a pane that shrank, closed, or was
/// covered by a popup neither paints nor copies stale cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    target: InputTarget,
    anchor: Edge,
    head: Edge,
    dragging: bool,
}

impl Selection {
    /// Starts a drag at the pointer, or `None` where no pane or popup paints.
    pub(crate) fn begin(
        surface: &PaneSurfaceFrame,
        x: f32,
        y: f32,
        cell_width: f32,
        cell_height: f32,
    ) -> Option<Self> {
        let target = wheel_target(surface, x, y, cell_width, cell_height)?.target;
        let edge = region(surface, &target, cell_width, cell_height)?.edge(
            x,
            y,
            cell_width,
            cell_height,
        )?;
        Some(Self {
            target,
            anchor: edge,
            head: edge,
            dragging: true,
        })
    }

    pub(crate) fn dragging(&self) -> bool {
        self.dragging
    }

    /// Follows the pointer. `false` when nothing about the selection changed.
    pub(crate) fn extend(
        &mut self,
        surface: &PaneSurfaceFrame,
        x: f32,
        y: f32,
        cell_width: f32,
        cell_height: f32,
    ) -> bool {
        let Some(edge) = region(surface, &self.target, cell_width, cell_height)
            .and_then(|region| region.edge(x, y, cell_width, cell_height))
        else {
            return false;
        };
        std::mem::replace(&mut self.head, edge) != edge
    }

    /// Ends the drag. `false` when the gesture had already finished.
    pub(crate) fn release(&mut self) -> bool {
        std::mem::replace(&mut self.dragging, false)
    }

    /// True for a selection that paints on the composite frame of the panes.
    pub(crate) fn in_panes(&self) -> bool {
        matches!(self.target, InputTarget::Pane(_))
    }

    /// True for a selection that paints on this popup's own frame.
    pub(crate) fn in_popup(&self, terminal_id: &str) -> bool {
        matches!(&self.target, InputTarget::Popup(id) if id == terminal_id)
    }

    /// The selected cells of each row, in the grid of the frame that paints
    /// them. Empty while the pointer has not left the half-cell it started in.
    pub(crate) fn rows(
        &self,
        surface: &PaneSurfaceFrame,
        cell_width: f32,
        cell_height: f32,
    ) -> impl Iterator<Item = (u16, Range<u16>)> {
        region(surface, &self.target, cell_width, cell_height)
            .into_iter()
            .flat_map(|region| self.spans(region))
    }

    fn spans(&self, region: Region) -> impl Iterator<Item = (u16, Range<u16>)> {
        let (start, end) = if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        let Region { columns, rows, .. } = region;
        let last = end.row.min(rows.end.saturating_sub(1));
        let mut next = Some(start.row.max(rows.start));
        std::iter::from_fn(move || {
            while let Some(row) = next.filter(|row| *row <= last) {
                next = row.checked_add(1);
                // The ends cut their own row at the pointer; the rows between
                // them run the full width of the region.
                let edge = |edge: Edge| match edge.side {
                    Side::Left => edge.column,
                    Side::Right => edge.column.saturating_add(1),
                };
                let first = if row == start.row {
                    edge(start).max(columns.start)
                } else {
                    columns.start
                };
                let stop = if row == end.row {
                    edge(end).min(columns.end)
                } else {
                    columns.end
                };
                if first < stop {
                    return Some((row, first..stop));
                }
            }
            None
        })
    }

    /// The selected text, with rows separated by a newline. Padding is trimmed
    /// from rows selected through to the region's right edge, where a terminal
    /// pads short lines with blanks the user never typed.
    pub(crate) fn text(
        &self,
        surface: &PaneSurfaceFrame,
        cell_width: f32,
        cell_height: f32,
    ) -> Result<String> {
        let region =
            region(surface, &self.target, cell_width, cell_height).ok_or(Error::SelectionStale)?;
        let frame = match &self.target {
            InputTarget::Popup(_) => &surface.popup.as_ref().ok_or(Error::SelectionStale)?.frame,
            InputTarget::Pane(_) => &surface.frame,
        };
        let edge = region.columns.end;
        let mut text = String::new();
        let mut line = String::new();
        for (index, (row, columns)) in self.spans(region).enumerate() {
            if row >= frame.height {
                return Err(Error::SelectionStale);
            }
            let offset = usize::from(row) * usize::from(frame.width);
            let cells = frame
                .cells
                .get(offset + usize::from(columns.start)..offset + usize::from(columns.end))
                .ok_or(Error::SelectionStale)?;
            if index > 0 {
                text.push('\n');
            }
            line.clear();
            for cell in cells {
                // Wide graphemes carry their text in the first cell only, and
                // concealed cells copy as blanks: what the screen does not show
                // must not reach the clipboard.
                if cell.skip {
                    continue;
                }
                let symbol = if cell.modifier & HIDDEN != 0 || cell.symbol.is_empty() {
                    " "
                } else {
                    &cell.symbol
                };
                if text.len() + line.len() + symbol.len() > MAX_SELECTION_BYTES {
                    return Err(Error::SelectionSize);
                }
                line.push_str(symbol);
            }
            text.push_str(if columns.end >= edge {
                line.trim_end()
            } else {
                &line
            });
        }
        Ok(text)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::error::Error;
    use herdr_client::protocol::{
        CellData, ClientShellPopupSurface, FrameData, PaneSurfacePane, SurfaceRect,
    };

    const CELL_WIDTH: f32 = 10.;
    const CELL_HEIGHT: f32 = 20.;

    fn frame(text: &str, width: u16, height: u16) -> FrameData {
        let mut symbols = text.chars();
        FrameData {
            width,
            height,
            cells: (0..usize::from(width) * usize::from(height))
                .map(|_| CellData {
                    symbol: symbols.next().unwrap_or(' ').to_string(),
                    fg: 0,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                })
                .collect(),
            cursor: None,
            hyperlinks: vec![],
            graphics: vec![],
        }
    }

    fn surface(text: &str) -> PaneSurfaceFrame {
        let rect = SurfaceRect {
            x: 0,
            y: 0,
            width: 10,
            height: 3,
        };
        PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: frame(text, 10, 3),
            splits: vec![],
            popup: None,
            graphics: Default::default(),
            panes: vec![PaneSurfacePane {
                pane_id: "pane".into(),
                content_revision: 1,
                rect,
                inner_rect: rect,
                scrollbar_rect: None,
                scroll: None,
                focused: true,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                alternate_screen_active: false,
                pixel_width: 100,
                pixel_height: 60,
            }],
        }
    }

    fn drag(surface: &PaneSurfaceFrame, from: (f32, f32), to: (f32, f32)) -> Selection {
        let mut selection =
            Selection::begin(surface, from.0, from.1, CELL_WIDTH, CELL_HEIGHT).unwrap();
        selection.extend(surface, to.0, to.1, CELL_WIDTH, CELL_HEIGHT);
        selection
    }

    fn text(surface: &PaneSurfaceFrame, selection: &Selection) -> String {
        selection.text(surface, CELL_WIDTH, CELL_HEIGHT).unwrap()
    }

    #[test]
    fn a_press_without_a_drag_selects_nothing_and_half_a_cell_selects_it() {
        let s = surface("abcdefghij");
        for (from, to) in [
            ((1., 1.), (1., 1.)),
            ((1., 1.), (4., 9.)),
            ((6., 1.), (9., 19.)),
        ] {
            let selection = drag(&s, from, to);
            assert_eq!(selection.rows(&s, CELL_WIDTH, CELL_HEIGHT).count(), 0);
            assert_eq!(text(&s, &selection), "");
        }
        // Crossing the middle of the first cell takes that cell, whichever way
        // the drag was made.
        for (from, to) in [((1., 1.), (6., 1.)), ((6., 1.), (1., 1.))] {
            let selection = drag(&s, from, to);
            assert_eq!(
                selection
                    .rows(&s, CELL_WIDTH, CELL_HEIGHT)
                    .collect::<Vec<_>>(),
                vec![(0, 0..1)]
            );
            assert_eq!(text(&s, &selection), "a");
        }
        assert_eq!(text(&s, &drag(&s, (1., 1.), (26., 1.))), "abc");
        assert_eq!(text(&s, &drag(&s, (26., 1.), (1., 1.))), "abc");
    }

    #[test]
    fn multi_row_selections_span_the_region_and_trim_only_padded_rows() {
        let mut s = surface("hi");
        for (index, symbol) in [(10, "x"), (11, "y"), (12, "z")] {
            s.frame.cells[index].symbol = symbol.into();
        }
        // Rows carried through to the right edge lose the terminal's padding;
        // the row where the pointer stopped keeps the blanks it selected.
        assert_eq!(text(&s, &drag(&s, (1., 1.), (36., 21.))), "hi\nxyz ");
        assert_eq!(
            drag(&s, (1., 1.), (36., 21.))
                .rows(&s, CELL_WIDTH, CELL_HEIGHT)
                .collect::<Vec<_>>(),
            vec![(0, 0..10), (1, 0..4)]
        );
        // A blank row was still selected, so it is still copied.
        assert_eq!(text(&s, &drag(&s, (1., 1.), (96., 41.))), "hi\nxyz\n");
        // Dragging above or below the region reaches its first and last cell.
        assert_eq!(text(&s, &drag(&s, (1., 1.), (36., 1e6))), "hi\nxyz\n");
        assert_eq!(text(&s, &drag(&s, (36., 21.), (1., -1e6))), "hi\nxyz ");
        // Dragging past a side clamps to the row's own bounds, not the next row.
        assert_eq!(text(&s, &drag(&s, (1., 1.), (1e6, 1.))), "hi");
        assert_eq!(text(&s, &drag(&s, (96., 21.), (-1e6, 21.))), "xyz");
    }

    #[test]
    fn selections_stay_inside_the_pane_that_owns_them() {
        let mut s = surface("abcdefghij");
        let mut right = s.panes[0].clone();
        right.pane_id = "right".into();
        right.rect.x = 5;
        right.inner_rect.x = 5;
        right.inner_rect.width = 5;
        s.panes[0].inner_rect.width = 5;
        s.panes.push(right);
        // A drag that leaves the pane stops at its edge instead of reading the
        // neighbor's cells out of the shared composite frame.
        let selection = drag(&s, (1., 1.), (1e6, 1.));
        assert_eq!(text(&s, &selection), "abcde");
        let selection = drag(&s, (51., 1.), (1e6, 41.));
        assert_eq!(
            selection
                .rows(&s, CELL_WIDTH, CELL_HEIGHT)
                .collect::<Vec<_>>(),
            vec![(0, 5..10), (1, 5..10), (2, 5..10)]
        );
        // The two blank rows below were selected too, so they are copied.
        assert_eq!(text(&s, &selection), "fghij\n\n");
        // A pane that closed, shrank away, or hid behind a popup copies nothing.
        let selection = drag(&s, (1., 1.), (36., 1.));
        for hidden in [true, false] {
            let mut s = s.clone();
            if hidden {
                s.popup = Some(Box::new(ClientShellPopupSurface {
                    terminal_id: "popup".into(),
                    title: String::new(),
                    width: None,
                    height: None,
                    frame: frame("popup", 5, 2),
                    mouse_reporting: false,
                    sgr_pixel_mouse: false,
                    pixel_width: 50,
                    pixel_height: 40,
                }));
            } else {
                s.panes.remove(0);
            }
            assert!(matches!(
                selection.text(&s, CELL_WIDTH, CELL_HEIGHT),
                Err(Error::SelectionStale)
            ));
            assert_eq!(selection.rows(&s, CELL_WIDTH, CELL_HEIGHT).count(), 0);
            assert!(
                !selection
                    .clone()
                    .extend(&s, 56., 1., CELL_WIDTH, CELL_HEIGHT)
            );
        }
        assert_eq!(text(&s, &selection), "abcd");
    }

    #[test]
    fn a_popup_is_selected_in_its_own_grid_and_the_panes_under_it_are_not() {
        let mut s = surface("abcdefghij");
        s.popup = Some(Box::new(ClientShellPopupSurface {
            terminal_id: "popup".into(),
            title: String::new(),
            width: None,
            height: None,
            frame: frame("popup", 4, 2),
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            pixel_width: 40,
            pixel_height: 40,
        }));
        // The popup is centered: three columns and half a row of offset.
        assert!(Selection::begin(&s, 1., 1., CELL_WIDTH, CELL_HEIGHT).is_none());
        let selection = drag(&s, (31., 11.), (66., 31.));
        assert!(selection.in_popup("popup") && !selection.in_panes());
        assert!(!selection.in_popup("other"));
        assert_eq!(
            selection
                .rows(&s, CELL_WIDTH, CELL_HEIGHT)
                .collect::<Vec<_>>(),
            vec![(0, 0..4), (1, 0..4)]
        );
        assert_eq!(text(&s, &selection), "popu\np");
        // The popup's terminal changing identity leaves nothing to copy.
        let mut replaced = s.clone();
        replaced.popup.as_mut().unwrap().terminal_id = "other".into();
        assert!(matches!(
            selection.text(&replaced, CELL_WIDTH, CELL_HEIGHT),
            Err(Error::SelectionStale)
        ));
    }

    #[test]
    fn copied_text_hides_concealed_cells_joins_wide_ones_and_stays_bounded() {
        let mut s = surface("ab\u{754c}");
        s.frame.cells[3].skip = true;
        s.frame.cells[1].modifier = HIDDEN;
        let selection = drag(&s, (1., 1.), (36., 1.));
        assert_eq!(text(&s, &selection), "a \u{754c}");
        s.frame.cells[0].symbol = String::new();
        assert_eq!(text(&s, &selection), "  \u{754c}");
        s.frame.cells[2].symbol = "a".repeat(MAX_SELECTION_BYTES + 1);
        assert!(matches!(
            selection.text(&s, CELL_WIDTH, CELL_HEIGHT),
            Err(Error::SelectionSize)
        ));
    }

    #[test]
    fn invalid_geometry_and_a_dragged_release_are_rejected() {
        let s = surface("abcdefghij");
        for invalid in [0., -1., f32::NAN, f32::INFINITY] {
            assert!(Selection::begin(&s, 1., 1., invalid, CELL_HEIGHT).is_none());
            assert!(Selection::begin(&s, 1., 1., CELL_WIDTH, invalid).is_none());
        }
        for outside in [-1., f32::NAN, f32::INFINITY] {
            assert!(Selection::begin(&s, outside, 1., CELL_WIDTH, CELL_HEIGHT).is_none());
            assert!(Selection::begin(&s, 1., outside, CELL_WIDTH, CELL_HEIGHT).is_none());
        }
        let mut selection = Selection::begin(&s, 1., 1., CELL_WIDTH, CELL_HEIGHT).unwrap();
        for invalid in [f32::NAN, f32::INFINITY] {
            assert!(!selection.extend(&s, invalid, 1., CELL_WIDTH, CELL_HEIGHT));
            assert!(!selection.extend(&s, 1., invalid, CELL_WIDTH, CELL_HEIGHT));
        }
        assert!(selection.dragging());
        assert!(selection.release());
        assert!(!selection.dragging());
        assert!(!selection.release());
        // A finished selection still describes the same cells.
        assert!(selection.extend(&s, 46., 1., CELL_WIDTH, CELL_HEIGHT));
        assert_eq!(text(&s, &selection), "abcde");
    }
}
