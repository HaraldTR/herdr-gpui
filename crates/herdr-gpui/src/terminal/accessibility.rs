//! The terminal text that assistive technology reads: screen readers, and
//! selection tools that ask the focused element for its selected text
//! (`AXSelectedText` on macOS) instead of the clipboard.
//!
//! One pane or popup is exposed at a time: the one holding a selection, or,
//! without one, the popup covering the panes, else the focused pane. Each row
//! it shows becomes an AccessKit text run, read as a copy reads it: concealed
//! cells as blanks, a wide grapheme once, and no trailing padding. Only rows on
//! screen are exposed, so a selection reaching into scrollback reads its
//! visible part. The text is built while GPUI prepaints, and only while an
//! assistive client has activated accessibility.

use super::{
    InputTarget,
    selection::{self, Region, Selection},
};
use gpui::{
    A11ySubtreeBuilder,
    accesskit::{Node, NodeId, Rect, Role, TextDirection, TextPosition, TextSelection},
};
use herdr_client::protocol::{FrameData, PaneSurfaceFrame};

/// AccessKit measures each character in one byte of UTF-8 length. Terminal
/// content is untrusted and a cell's grapheme may be longer than that, so such
/// a cell reads as a replacement character instead.
const OVERLONG: &str = "\u{fffd}";

/// The cells of one painted row that read as text.
#[derive(Debug, PartialEq)]
struct Line {
    /// The grid row the line paints on.
    row: u16,
    text: String,
    /// The UTF-8 length of each character, one per grapheme.
    lengths: Vec<u8>,
    /// Each character's first column, counted from the region's first column,
    /// and the columns it covers.
    cells: Vec<(u16, u16)>,
}

impl Line {
    /// Reads `row` of `frame` within the region's columns. `None` when the
    /// frame no longer holds the row.
    fn read(frame: &FrameData, region: &Region, row: u16) -> Option<Self> {
        if row >= frame.height || region.columns.end > frame.width {
            return None;
        }
        let offset = usize::from(row) * usize::from(frame.width);
        let cells = frame.cells.get(
            offset + usize::from(region.columns.start)..offset + usize::from(region.columns.end),
        )?;
        let mut line = Self {
            row,
            text: String::new(),
            lengths: Vec::new(),
            cells: Vec::new(),
        };
        // Characters up to the last one that is not a blank: a terminal pads
        // short lines with blanks the user never typed.
        let mut kept = 0;
        for (column, span, cell) in selection::graphemes(cells) {
            let symbol = selection::shown(cell);
            let (symbol, length) = match u8::try_from(symbol.len()) {
                Ok(length) => (symbol, length),
                Err(_) => (OVERLONG, OVERLONG.len() as u8),
            };
            line.text.push_str(symbol);
            line.lengths.push(length);
            // Both fit: the region's columns are `u16`.
            line.cells.push((column as u16, span as u16));
            if !symbol.trim_end().is_empty() {
                kept = line.lengths.len();
            }
        }
        let padding: usize = line.lengths[kept..].iter().copied().map(usize::from).sum();
        line.text.truncate(line.text.len() - padding);
        line.lengths.truncate(kept);
        line.cells.truncate(kept);
        Some(line)
    }

    /// The character before which a cell edge at `column` falls. A wide
    /// grapheme counts as before an edge that cuts through it, as a copy
    /// starting on its continuation leaves it out and one ending there keeps
    /// it.
    fn character(&self, column: u16) -> usize {
        self.cells.partition_point(|&(start, _)| start < column)
    }
}

/// A position between two characters of the exposed text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Caret {
    line: usize,
    character: usize,
}

/// The exposed rows of one pane or popup, and the part of them selected.
#[derive(Debug, PartialEq)]
pub(crate) struct Transcript {
    lines: Vec<Line>,
    selection: Option<(Caret, Caret)>,
    /// The region's top-left corner, from the terminal canvas origin, and the
    /// size of a cell, in logical pixels.
    origin: (f32, f32),
    width: f32,
    cell: (f32, f32),
}

impl Transcript {
    /// Reads what the terminal shows. `None` when no pane or popup paints.
    pub(crate) fn read(
        surface: &PaneSurfaceFrame,
        selection: Option<&Selection>,
        cell_width: f32,
        cell_height: f32,
    ) -> Option<Self> {
        let selection = selection.filter(|selection| {
            selection
                .rows(surface, cell_width, cell_height)
                .next()
                .is_some()
        });
        let target = match selection {
            Some(selection) => selection.target().clone(),
            None => shown_target(surface)?,
        };
        let region = selection::region(surface, &target, cell_width, cell_height)?;
        let frame = selection::frame(surface, &target)?;
        let lines = region
            .rows
            .clone()
            .map(|row| Line::read(frame, &region, row))
            .collect::<Option<Vec<_>>>()?;
        let selection = selection.and_then(|selection| {
            let mut rows = selection.rows(surface, cell_width, cell_height);
            let first = rows.next()?;
            let last = rows.last().unwrap_or_else(|| first.clone());
            let caret = |row: u16, column: u16| {
                let line = lines.iter().position(|line| line.row == row)?;
                let character = lines[line].character(column - region.columns.start);
                Some(Caret { line, character })
            };
            Some((caret(first.0, first.1.start)?, caret(last.0, last.1.end)?))
        });
        Some(Self {
            lines,
            selection,
            origin: (
                region.origin.0 + f32::from(region.columns.start) * cell_width,
                region.origin.1,
            ),
            width: f32::from(region.columns.end - region.columns.start) * cell_width,
            cell: (cell_width, cell_height),
        })
    }

    /// Adds the rows as text runs under the terminal's node, and the selection
    /// to that node. `origin` is the terminal canvas origin in logical pixels
    /// and `scale` the window's scale factor: AccessKit bounds are in device
    /// pixels from the window's top-left corner, like the terminal node's own.
    pub(crate) fn expose(&self, builder: &mut A11ySubtreeBuilder, origin: (f32, f32), scale: f32) {
        let ids: Vec<NodeId> = (0..self.lines.len())
            .map(|line| builder.synthetic_node_id(line))
            .collect();
        for (&id, node) in ids.iter().zip(self.runs(origin, scale)) {
            builder.push_child(id, node);
        }
        if let Some(selection) = self.text_selection(&ids) {
            builder.parent_node().set_text_selection(selection);
        }
    }

    /// One text run per row. Every row but the last ends with the line break
    /// a copy puts between rows.
    fn runs(&self, origin: (f32, f32), scale: f32) -> impl Iterator<Item = Node> + '_ {
        let (cell_width, cell_height) = self.cell;
        let last = self.lines.len().saturating_sub(1);
        self.lines.iter().enumerate().map(move |(index, line)| {
            let x = (origin.0 + self.origin.0) * scale;
            let y = (origin.1 + self.origin.1 + f32::from(line.row) * cell_height) * scale;
            let mut node = Node::new(Role::TextRun);
            node.set_bounds(Rect {
                x0: f64::from(x),
                y0: f64::from(y),
                x1: f64::from(x + self.width * scale),
                y1: f64::from(y + cell_height * scale),
            });
            node.set_text_direction(TextDirection::LeftToRight);
            let mut text = line.text.clone();
            let mut lengths = line.lengths.clone();
            let mut positions: Vec<f32> = line
                .cells
                .iter()
                .map(|&(column, _)| f32::from(column) * cell_width * scale)
                .collect();
            let mut widths: Vec<f32> = line
                .cells
                .iter()
                .map(|&(_, span)| f32::from(span) * cell_width * scale)
                .collect();
            if index < last {
                // The break sits where the row's text ends and paints nothing.
                let end = line.cells.last().map_or(0, |&(column, span)| column + span);
                text.push('\n');
                lengths.push(1);
                positions.push(f32::from(end) * cell_width * scale);
                widths.push(0.);
            }
            node.set_value(text);
            node.set_character_lengths(lengths);
            node.set_character_positions(positions);
            node.set_character_widths(widths);
            node
        })
    }

    fn text_selection(&self, ids: &[NodeId]) -> Option<TextSelection> {
        let (anchor, focus) = self.selection?;
        let position = |caret: Caret| TextPosition {
            node: ids[caret.line],
            character_index: caret.character,
        };
        Some(TextSelection {
            anchor: position(anchor),
            focus: position(focus),
        })
    }
}

/// What the terminal shows without a selection: a popup covers the panes, and
/// otherwise the focused pane is the one being read and typed into.
fn shown_target(surface: &PaneSurfaceFrame) -> Option<InputTarget> {
    if let Some(popup) = &surface.popup {
        return Some(InputTarget::Popup(popup.terminal_id.clone()));
    }
    surface
        .panes
        .iter()
        .find(|pane| pane.focused)
        .or_else(|| surface.panes.first())
        .map(|pane| InputTarget::Pane(pane.pane_id.clone()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::terminal::HIDDEN;
    use herdr_client::protocol::{CellData, PaneSurfacePane, SurfaceRect};

    const CELL_WIDTH: f32 = 10.;
    const CELL_HEIGHT: f32 = 20.;

    fn cell(symbol: &str) -> CellData {
        CellData {
            symbol: symbol.into(),
            fg: 0,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        }
    }

    /// A frame whose rows are `rows`, one cell per character, padded with
    /// blanks to `width`.
    fn frame(rows: &[&str], width: u16) -> FrameData {
        let cells = rows
            .iter()
            .flat_map(|row| {
                let mut symbols: Vec<String> = row.chars().map(String::from).collect();
                symbols.resize(usize::from(width), " ".into());
                symbols
            })
            .map(|symbol| cell(&symbol))
            .collect();
        FrameData {
            width,
            height: rows.len() as u16,
            cells,
            cursor: None,
            hyperlinks: vec![],
            graphics: vec![],
        }
    }

    fn pane(id: &str, x: u16, width: u16, height: u16, focused: bool) -> PaneSurfacePane {
        let rect = SurfaceRect {
            x,
            y: 0,
            width,
            height,
        };
        PaneSurfacePane {
            pane_id: id.into(),
            content_revision: 1,
            rect,
            inner_rect: rect,
            scrollbar_rect: None,
            scroll: None,
            focused,
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            alternate_screen_active: false,
            pixel_width: width.into(),
            pixel_height: height.into(),
        }
    }

    /// Two panes side by side, `left` focused, each five columns wide.
    fn surface(rows: &[&str]) -> PaneSurfaceFrame {
        PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: frame(rows, 10),
            splits: vec![],
            popup: None,
            graphics: Default::default(),
            panes: vec![
                pane("left", 0, 5, rows.len() as u16, true),
                pane("right", 5, 5, rows.len() as u16, false),
            ],
        }
    }

    fn drag(surface: &PaneSurfaceFrame, from: (f32, f32), to: (f32, f32)) -> Selection {
        let mut selection =
            Selection::begin(surface, from.0, from.1, CELL_WIDTH, CELL_HEIGHT, 1).unwrap();
        selection.extend(surface, to.0, to.1, CELL_WIDTH, CELL_HEIGHT);
        selection.release();
        selection
    }

    fn read(surface: &PaneSurfaceFrame, selection: Option<&Selection>) -> Transcript {
        Transcript::read(surface, selection, CELL_WIDTH, CELL_HEIGHT).unwrap()
    }

    /// The text of every run, joined as a reader joins them.
    fn text(transcript: &Transcript) -> String {
        transcript
            .runs((0., 0.), 1.)
            .map(|node| node.value().unwrap().to_owned())
            .collect()
    }

    /// The selected text, as a reader resolves the selection's carets.
    fn selected(transcript: &Transcript) -> String {
        let (start, end) = transcript.selection.unwrap();
        let runs: Vec<Node> = transcript.runs((0., 0.), 1.).collect();
        let offset = |caret: Caret| {
            let before: usize = runs[..caret.line]
                .iter()
                .map(|node| node.value().unwrap().len())
                .sum();
            let within: usize = runs[caret.line].character_lengths()[..caret.character]
                .iter()
                .copied()
                .map(usize::from)
                .sum();
            before + within
        };
        let all: String = runs.iter().map(|node| node.value().unwrap()).collect();
        all[offset(start)..offset(end)].to_owned()
    }

    #[test]
    fn without_a_selection_the_focused_pane_reads_without_padding() {
        let s = surface(&["ab   right", "  c  x"]);
        let transcript = read(&s, None);
        assert_eq!(text(&transcript), "ab\n  c");
        assert_eq!(transcript.selection, None);

        let mut s = s;
        s.panes[0].focused = false;
        s.panes[1].focused = true;
        assert_eq!(text(&read(&s, None)), "right\nx");
    }

    #[test]
    fn a_selection_is_read_from_its_own_pane_and_matches_the_copy() {
        let s = surface(&["abcd right", "efgh other"]);
        // From the middle of "b" in the left pane to past "g" on the next row.
        let selection = drag(&s, (16., 1.), (39., 21.));
        let transcript = read(&s, Some(&selection));
        let copy = selection.text(&s, CELL_WIDTH, CELL_HEIGHT).unwrap();
        assert_eq!(copy, "cd\nefgh");
        assert_eq!(selected(&transcript), copy);

        // A selection in the unfocused pane is the one exposed.
        let selection = drag(&s, (56., 1.), (89., 1.));
        let transcript = read(&s, Some(&selection));
        assert_eq!(text(&transcript), "right\nother");
        assert_eq!(selected(&transcript), "igh");
    }

    #[test]
    fn a_selection_that_chose_no_cell_exposes_the_focused_pane_unselected() {
        let s = surface(&["abcd right"]);
        let selection = drag(&s, (51., 1.), (52., 1.));
        let transcript = read(&s, Some(&selection));
        assert_eq!(text(&transcript), "abcd");
        assert_eq!(transcript.selection, None);
    }

    #[test]
    fn a_selection_through_the_padding_stops_at_the_text() {
        let s = surface(&["ab   right", "cd"]);
        let selection = drag(&s, (1., 1.), (49., 1.));
        let transcript = read(&s, Some(&selection));
        assert_eq!(selected(&transcript), "ab");
        assert_eq!(
            transcript.selection.unwrap().1,
            Caret {
                line: 0,
                character: 2
            }
        );
    }

    #[test]
    fn wide_concealed_and_overlong_cells_read_as_a_copy_does() {
        let mut s = surface(&["xxxxx"]);
        let cells = &mut s.frame.cells;
        cells[0] = cell("界");
        cells[1] = CellData {
            skip: true,
            ..cell(" ")
        };
        cells[2] = CellData {
            modifier: HIDDEN,
            ..cell("s")
        };
        cells[3] = cell(&"e\u{301}".repeat(200));
        cells[4] = cell("");
        let transcript = read(&s, None);
        let run = transcript.runs((0., 0.), 1.).next().unwrap();
        assert_eq!(run.value(), Some("界 \u{fffd}"));
        assert_eq!(run.character_lengths(), [3, 1, 3]);
        assert_eq!(run.character_positions(), Some(&[0., 20., 30.][..]));
        assert_eq!(run.character_widths(), Some(&[20., 10., 20.][..]));
    }

    #[test]
    fn a_selection_starting_on_a_wide_continuation_leaves_the_grapheme_out() {
        let mut s = surface(&["xxyz"]);
        s.frame.cells[0] = cell("界");
        s.frame.cells[1] = cell(" ");
        let selection = drag(&s, (11., 1.), (39., 1.));
        let copy = selection.text(&s, CELL_WIDTH, CELL_HEIGHT).unwrap();
        assert_eq!(copy, "yz");
        assert_eq!(selected(&read(&s, Some(&selection))), copy);
    }

    #[test]
    fn runs_are_placed_on_the_cells_in_device_pixels() {
        let s = surface(&["ab   cd", "e"]);
        let selection = drag(&s, (51., 1.), (69., 1.));
        let transcript = read(&s, Some(&selection));
        let runs: Vec<Node> = transcript.runs((100., 40.), 2.).collect();
        let bounds = runs[1].bounds().unwrap();
        // The right pane starts five cells in; the second row one cell down.
        assert_eq!((bounds.x0, bounds.y0), (300., 120.));
        assert_eq!((bounds.x1, bounds.y1), (400., 160.));
        // The first row's break sits after its text and paints nothing.
        assert_eq!(runs[0].value(), Some("cd\n"));
        assert_eq!(runs[0].character_positions(), Some(&[0., 20., 40.][..]));
        assert_eq!(runs[0].character_widths(), Some(&[20., 20., 0.][..]));
    }

    #[test]
    fn a_popup_is_exposed_instead_of_the_panes_it_covers() {
        use herdr_client::protocol::ClientShellPopupSurface;
        let mut s = surface(&["under", "under"]);
        s.popup = Some(Box::new(ClientShellPopupSurface {
            terminal_id: "popup".into(),
            title: String::new(),
            width: None,
            height: None,
            frame: frame(&["top"], 4),
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            pixel_width: 40,
            pixel_height: 20,
        }));
        assert_eq!(text(&read(&s, None)), "top");
    }
}
