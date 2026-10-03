//! Selecting terminal cells with the pointer and copying them. The gesture
//! never sends input to the daemon and writes the clipboard only when the user
//! releases a selection they made or asks to copy one still highlighted. A drag held past a pane's top or bottom
//! scrolls the pane, and a selection reaching rows the pane no longer shows is
//! read back with `pane.selection.read`; everything else is copied from the
//! painted cells.

use super::HerdrWindow;
use crate::{scrollback::Inbox, terminal::Selection};
use gpui::{ClipboardItem, Context, Pixels, Point};
use herdr_client::{
    Method,
    scrollback::{ScrollbackResponse, SelectionReadParams},
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// How often a drag held past a pane's edge scrolls it by another step.
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);

/// What a drag needs beyond the selection itself: where the pointer last was,
/// so the selection can follow the pane as it scrolls under a still pointer,
/// and the copy the daemon is reading for a released selection.
#[derive(Default)]
pub(crate) struct Follow {
    pointer: Option<Point<Pixels>>,
    scrolled: Option<Instant>,
    read: Option<(Arc<Mutex<Inbox>>, String)>,
}

impl HerdrWindow {
    /// Starts a selection under the pointer, discarding the previous one. A
    /// double click starts on the link or word under it and a triple click on
    /// its row. A press that lands outside the painted cells only clears.
    pub(crate) fn begin_selection(
        &mut self,
        position: Point<Pixels>,
        clicks: usize,
        cx: &mut Context<Self>,
    ) {
        // A click hands the terminal back from copy mode.
        self.leave_copy_mode(cx);
        let cleared = self.selection.take().is_some();
        if let Some(surface) = self.selectable_surface(position) {
            let (x, y) = Self::terminal_offset(self.bounds, position);
            self.selection = Selection::begin(
                surface,
                x,
                y,
                self.cell_width,
                self.config.terminal.line_height(),
                clicks,
            );
        }
        if cleared || self.selection.is_some() {
            cx.notify();
        }
    }

    /// Follows the pointer while the button that started the drag is down.
    /// Returns whether a drag is in progress, so the caller can keep the
    /// gesture to itself.
    pub(crate) fn extend_selection(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let (x, y) = Self::terminal_offset(self.bounds, position);
        let cell_width = self.cell_width;
        let cell_height = self.config.terminal.line_height();
        let (Some(selection), Some(surface)) = (&mut self.selection, &self.live.surface) else {
            return false;
        };
        if !selection.dragging() {
            return false;
        }
        self.selection_follow.pointer = Some(position);
        if selection.extend(surface, x, y, cell_width, cell_height) {
            cx.notify();
        }
        true
    }

    /// Herdr's shared `ui.copy_on_select`, on until the shared config loads.
    pub(super) fn copy_on_select(&self) -> bool {
        self.settings
            .shared
            .as_ref()
            .is_none_or(|shared| shared.copy_on_select)
    }

    /// Ends a drag: what it chose goes to the clipboard and the flash says so.
    /// The highlight stays while `keep_selection_after_copy` is on, so tools
    /// that read the selection find it and Cmd-C copies it again. With Herdr's
    /// `copy_on_select` off, nothing is copied yet and the highlight stays for
    /// [`Self::copy_retained_selection`]. Returns
    /// whether the release belonged to the selection, since a press that
    /// chose no cells is still the click that opens a link under the pointer.
    pub(crate) fn release_selection(&mut self, cx: &mut Context<Self>) -> bool {
        if !self
            .selection
            .as_mut()
            .is_some_and(|selection| selection.release())
        {
            return false;
        }
        self.selection_follow.pointer = None;
        let selected = !self.selection_is_empty();
        if selected && !self.copy_on_select() {
            cx.notify();
            return true;
        }
        self.finish_copy(selected, cx);
        selected
    }

    /// Whether a released selection is still highlighted, waiting for an
    /// explicit copy or kept after one.
    pub(crate) fn selection_retained(&self) -> bool {
        self.selection
            .as_ref()
            .is_some_and(|selection| !selection.dragging())
            && !self.selection_is_empty()
    }

    /// Copies a selection kept on release, as Herdr's Ctrl-C or Cmd-C does,
    /// and clears it unless `keep_selection_after_copy` is on. `false` when no
    /// released selection is waiting.
    pub(crate) fn copy_retained_selection(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.selection_retained() {
            return false;
        }
        self.finish_copy(true, cx);
        true
    }

    /// Drops a selection kept on release, as any other key does in Herdr.
    pub(crate) fn clear_retained_selection(&mut self, cx: &mut Context<Self>) {
        if self
            .selection
            .as_ref()
            .is_some_and(|selection| !selection.dragging())
        {
            self.selection = None;
            cx.notify();
        }
    }

    fn finish_copy(&mut self, selected: bool, cx: &mut Context<Self>) {
        // A kept selection stays readable by selection tools and screen
        // readers until the next click or keystroke. Otherwise the gesture is
        // over either way: nothing stays highlighted behind it.
        let keep = selected && self.config.keep_selection_after_copy;
        if selected && self.read_offscreen_selection() {
            if !keep {
                self.selection = None;
            }
            cx.notify();
            return;
        }
        let copied = selected && self.copy_selection(cx);
        if !keep {
            self.selection = None;
        }
        if copied && self.config.clipboard_toast.enabled {
            self.show_flash(super::Flash::success("copied to clipboard"), cx);
        }
        cx.notify();
    }

    /// Asks the daemon for a selection that reaches rows the pane does not
    /// show. `false` when the painted cells hold all of it.
    fn read_offscreen_selection(&mut self) -> bool {
        let cell_height = self.config.terminal.line_height();
        let (Some(selection), Some(surface)) = (&self.selection, self.live.surface.as_deref())
        else {
            return false;
        };
        let Some((pane_id, range)) =
            selection.offscreen_range(surface, self.cell_width, cell_height)
        else {
            return false;
        };
        let params = SelectionReadParams {
            pane_id: pane_id.to_owned(),
            anchor: range.start,
            cursor: range.end,
            // An explicit selection reads the live terminal: output since the
            // frame on screen must not refuse the copy.
            content_revision: None,
        };
        let connection = &self.endpoints[self.selected_endpoint].connection;
        let result = if !self.live.supports_selection_read {
            Err(crate::Error::SelectionOffscreen)
        } else if let (Some(handle), Some(snapshot)) = (&connection.handle, &self.live.snapshot) {
            let inbox = connection.scrollback.clone();
            let sent = inbox
                .try_lock()
                .map_err(|_| crate::Error::ConnectionBusy)
                .and_then(|mut mailbox| {
                    Ok(mailbox.send(|| handle.read_selection(&snapshot.boot_id, &params))?)
                });
            sent.map(|request| (inbox, request))
        } else {
            Err(crate::Error::NotConnected)
        };
        match result {
            Ok((inbox, request)) => self.await_selection_read(inbox, request),
            Err(error) => self.local_error = Some(format!("Selection not copied: {error}")),
        }
        true
    }

    /// Copies what `request` reads once it comes back. A read still waiting
    /// is given up: the newer selection is the one the user wants.
    pub(super) fn await_selection_read(&mut self, inbox: Arc<Mutex<Inbox>>, request: String) {
        if let Some((inbox, request)) = self.selection_follow.read.replace((inbox, request))
            && let Ok(mut inbox) = inbox.try_lock()
        {
            inbox.discard(&request);
        }
    }

    /// Runs every tick while a drag or a daemon read is under way: scrolls a
    /// pane the pointer is held past, keeps the selection under the pointer
    /// as the pane moves, and copies a read that has come back.
    pub(crate) fn follow_selection(&mut self, cx: &mut Context<Self>) {
        self.finish_selection_read(cx);
        let Some(pointer) = self.selection_follow.pointer else {
            return;
        };
        if !self.selection.as_ref().is_some_and(Selection::dragging) {
            self.selection_follow.pointer = None;
            return;
        }
        // The pane may have scrolled under a pointer that has not moved.
        self.extend_selection(pointer, cx);
        let now = Instant::now();
        if self.live.drag_request.is_some()
            || self
                .selection_follow
                .scrolled
                .is_some_and(|last| now.saturating_duration_since(last) < AUTOSCROLL_INTERVAL)
        {
            return;
        }
        let Some((pane_id, offset)) = self.selection_autoscroll(pointer) else {
            return;
        };
        let connection = &self.endpoints[self.selected_endpoint].connection;
        let (Some(handle), Some(snapshot)) = (&connection.handle, &self.live.snapshot) else {
            return;
        };
        // One scroll in flight at a time, registered like a scrollbar drag's.
        let Ok(mut state) = connection.inbox.try_lock() else {
            return;
        };
        match handle.request(
            &snapshot.boot_id,
            Method::PaneScroll,
            serde_json::json!({"pane_id": pane_id, "offset_from_bottom": offset}),
        ) {
            Ok(request) => {
                state.drag_request = Some(request.clone());
                self.live.drag_request = Some(request);
                self.selection_follow.scrolled = Some(now);
            }
            Err(error) => {
                drop(state);
                self.local_error = Some(format!("Scroll not sent: {error}"));
                self.selection_follow.pointer = None;
                cx.notify();
            }
        }
    }

    /// The pane under a pane selection and the offset one step further in the
    /// direction the pointer is held past its edge, faster the further out.
    fn selection_autoscroll(&self, pointer: Point<Pixels>) -> Option<(String, u64)> {
        let selection = self.selection.as_ref()?;
        let pane_id = selection.pane_id()?;
        let surface = self.live.surface.as_deref()?;
        let pane = surface.panes.iter().find(|pane| pane.pane_id == pane_id)?;
        let scroll = pane.scroll?;
        let cell_height = self.config.terminal.line_height();
        let (_, y) = Self::terminal_offset(self.bounds, pointer);
        let top = f32::from(pane.inner_rect.y) * cell_height;
        let bottom = top + f32::from(pane.inner_rect.height) * cell_height;
        let rows = |distance: f32| ((distance / cell_height).ceil() as u64).clamp(1, 5);
        let offset = if y < top {
            scroll
                .offset_from_bottom
                .saturating_add(rows(top - y))
                .min(scroll.max_offset_from_bottom)
        } else if y >= bottom {
            scroll
                .offset_from_bottom
                .saturating_sub(rows(y - bottom + 1.))
        } else {
            return None;
        };
        (offset != scroll.offset_from_bottom).then(|| (pane_id.to_owned(), offset))
    }

    /// Copies a selection the daemon has read back, once.
    fn finish_selection_read(&mut self, cx: &mut Context<Self>) {
        let Some((inbox, request)) = &self.selection_follow.read else {
            return;
        };
        let answer = match inbox.try_lock() {
            Ok(mut inbox) => inbox.take(request),
            Err(_) => return,
        };
        let Some(answer) = answer else {
            // A replaced connection never answers its old mailbox.
            if !Arc::ptr_eq(
                inbox,
                &self.endpoints[self.selected_endpoint].connection.scrollback,
            ) {
                self.selection_follow.read = None;
            }
            return;
        };
        self.selection_follow.read = None;
        match answer {
            Ok(ScrollbackResponse::PaneSelection(selection))
                if selection.text.len() <= crate::terminal::MAX_SELECTION_BYTES =>
            {
                cx.write_to_clipboard(ClipboardItem::new_string(selection.text));
                if self.config.clipboard_toast.enabled {
                    self.show_flash(super::Flash::success("copied to clipboard"), cx);
                }
            }
            Ok(ScrollbackResponse::PaneSelection(_)) => {
                self.local_error = Some(format!(
                    "Selection not copied: {}",
                    crate::Error::SelectionSize
                ));
            }
            Ok(_) => {
                self.local_error = Some(format!(
                    "Selection not copied: {}",
                    herdr_client::Error::ResponseType
                ));
            }
            Err(error) => self.local_error = Some(format!("Selection not copied: {error}")),
        }
        cx.notify();
    }

    /// Writes the current selection to the clipboard, reporting whether the
    /// text got there.
    fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let text = {
            let (Some(selection), Some(surface)) = (&self.selection, &self.live.surface) else {
                return false;
            };
            selection.text(surface, self.cell_width, self.config.terminal.line_height())
        };
        match text {
            Ok(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            Err(error) => {
                self.local_error = Some(format!("Selection not copied: {error}"));
                false
            }
        }
    }

    /// Whether the selection covers no cell the client can still show, either
    /// because the pointer never left the half-cell it pressed in or because
    /// the surface behind it is gone.
    fn selection_is_empty(&self) -> bool {
        let (Some(selection), Some(surface)) = (&self.selection, &self.live.surface) else {
            return true;
        };
        !self.live.surface_ready()
            || selection
                .rows(surface, self.cell_width, self.config.terminal.line_height())
                .next()
                .is_none()
    }

    /// The surface a press at `position` may select from, if the terminal area
    /// is showing one. Hit testing reads the live surface, never the frame a
    /// gap may still be presenting.
    fn selectable_surface(
        &self,
        position: Point<Pixels>,
    ) -> Option<&herdr_client::protocol::PaneSurfaceFrame> {
        if self.menu.page.is_some()
            || !self.live.surface_ready()
            || !self.bounds.contains(&position)
        {
            return None;
        }
        self.live.surface.as_deref()
    }

    fn terminal_offset(bounds: gpui::Bounds<Pixels>, position: Point<Pixels>) -> (f32, f32) {
        (
            f32::from(position.x - bounds.origin.x),
            f32::from(position.y - bounds.origin.y),
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::sidebar::layout_tests::fixture_window;
    use gpui::{Modifiers, MouseButton, TestAppContext, point, px};
    use herdr_client::protocol::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn surface(rows: &[&str], width: u16) -> PaneSurfaceFrame {
        let height = rows.len() as u16;
        let rect = SurfaceRect {
            x: 0,
            y: 0,
            width,
            height,
        };
        PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: FrameData {
                width,
                height,
                cells: rows
                    .iter()
                    .flat_map(|row| {
                        let mut symbols = row.chars();
                        (0..width).map(move |_| CellData {
                            symbol: symbols.next().unwrap_or(' ').to_string(),
                            fg: 0,
                            bg: 0,
                            modifier: 0,
                            skip: false,
                            hyperlink: None,
                        })
                    })
                    .collect(),
                cursor: None,
                hyperlinks: vec![],
                graphics: vec![],
            },
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

    /// A drag across the painted cells copies what it covered when the button
    /// comes up, keeps it highlighted, and says so; a press alone leaves the
    /// clipboard alone and clears the highlight.
    #[gpui::test]
    fn dragging_copies_on_release_keeps_the_highlight_and_reports(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            let mut frame = surface(&["hello there", "second row"], 12);
            let snapshot = view.live.snapshot.as_ref().unwrap();
            frame.boot_id = snapshot.boot_id.clone();
            frame.projection_revision = snapshot.revision;
            view.live.surface = Some(Arc::new(frame));
            view
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let (origin, cell) = view.read_with(cx, |view, _| {
            (
                view.bounds.origin,
                (view.cell_width, view.config.terminal.line_height()),
            )
        });
        let at = |column: f32, row: f32| -> Point<Pixels> {
            origin + point(px(column * cell.0), px(row * cell.1))
        };

        cx.simulate_mouse_down(at(0., 0.), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(at(5., 0.), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(at(5., 0.), MouseButton::Left, Modifiers::default());
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some("hello".into())
        );
        let expires = view.read_with(cx, |view, _| {
            assert!(view.selection_retained(), "the release keeps the highlight");
            view.flash.clone().expect("the release reports the copy").1
        });
        assert!(cx.update(|_, _| expires) > Instant::now());
        assert!(cx.debug_bounds("flash").is_some());

        // A drag over two rows keeps the rows apart and drops the padding the
        // terminal added to the row it carried through to the edge.
        cx.simulate_mouse_down(at(6., 0.), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(at(6., 1.), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(at(6., 1.), MouseButton::Left, Modifiers::default());
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some("there\nsecond".into())
        );

        // A press with no drag selects nothing, so neither the clipboard nor
        // the flash reports one.
        view.update(cx, |view, _| view.flash = None);
        cx.simulate_click(at(2., 0.), Modifiers::default());
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some("there\nsecond".into())
        );
        view.read_with(cx, |view, _| {
            assert!(view.selection.is_none());
            assert!(view.flash.is_none());
        });

        // The flash retires on its own once its two seconds are up. Whether it
        // is still painted is state, not layout: gpui keeps every debug bound
        // a frame ever registered, so a removed element still has one.
        cx.simulate_mouse_down(at(0., 0.), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(at(5., 0.), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(at(5., 0.), MouseButton::Left, Modifiers::default());
        view.update(cx, |view, _| {
            let (flash, expires) = view.flash.clone().expect("a copy reports itself");
            assert_eq!(flash, crate::window::Flash::success("copied to clipboard"));
            assert!(!view.tick_flash(expires - Duration::from_nanos(1)));
            assert!(view.flash.is_some());
            assert!(view.tick_flash(expires));
            assert!(view.flash.is_none());
            assert!(!view.tick_flash(expires));
        });
    }

    /// With Herdr's `copy_on_select` off, a release keeps the highlight and
    /// leaves the clipboard alone until Cmd-C or Ctrl-C, as in Herdr's TUI;
    /// any other key drops it. The copy keeps the highlight too.
    #[gpui::test]
    fn without_copy_on_select_a_release_keeps_the_selection_for_an_explicit_copy(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            let mut frame = surface(&["hello there", "second row"], 12);
            let snapshot = view.live.snapshot.as_ref().unwrap();
            frame.boot_id = snapshot.boot_id.clone();
            frame.projection_revision = snapshot.revision;
            view.live.surface = Some(Arc::new(frame));
            view.settings.shared = Some(
                crate::herdr_settings::Settings::parse_text("[ui]\ncopy_on_select = false")
                    .unwrap(),
            );
            view
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let (origin, cell) = view.read_with(cx, |view, _| {
            (
                view.bounds.origin,
                (view.cell_width, view.config.terminal.line_height()),
            )
        });
        let at = |column: f32, row: f32| -> Point<Pixels> {
            origin + point(px(column * cell.0), px(row * cell.1))
        };
        let clipboard = |cx: &mut gpui::VisualTestContext| {
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
        };
        let select = |cx: &mut gpui::VisualTestContext| {
            cx.simulate_mouse_down(at(0., 0.), MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_move(at(5., 0.), MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(at(5., 0.), MouseButton::Left, Modifiers::default());
        };
        let copy_available = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
                window.is_action_available(&crate::actions::Copy, cx)
            })
        };
        cx.write_to_clipboard(ClipboardItem::new_string("before".into()));

        select(cx);
        assert_eq!(clipboard(cx).as_deref(), Some("before"));
        view.read_with(cx, |view, _| {
            assert!(view.selection_retained());
            assert!(view.flash.is_none());
        });
        assert!(copy_available(cx));

        // Another key drops the highlight without copying.
        cx.simulate_keystrokes("x");
        view.read_with(cx, |view, _| assert!(view.selection.is_none()));
        assert_eq!(clipboard(cx).as_deref(), Some("before"));
        assert!(!copy_available(cx));

        for keystroke in ["cmd-c", "ctrl-c"] {
            cx.write_to_clipboard(ClipboardItem::new_string("before".into()));
            select(cx);
            cx.simulate_keystrokes(keystroke);
            assert_eq!(clipboard(cx).as_deref(), Some("hello"), "{keystroke}");
            view.read_with(cx, |view, _| {
                assert!(view.selection_retained(), "{keystroke}");
                assert!(view.flash.is_some(), "{keystroke}");
            });
        }

        // The Edit menu's Copy takes a kept selection too.
        cx.write_to_clipboard(ClipboardItem::new_string("before".into()));
        select(cx);
        cx.update(|window, cx| window.dispatch_action(Box::new(crate::actions::Copy), cx));
        assert_eq!(clipboard(cx).as_deref(), Some("hello"));
        view.read_with(cx, |view, _| assert!(view.selection_retained()));

        // A press with no drag keeps nothing.
        cx.simulate_click(at(2., 0.), Modifiers::default());
        view.read_with(cx, |view, _| assert!(view.selection.is_none()));
    }

    /// A selection the release copied stays for Cmd-C and the Edit menu,
    /// while Ctrl-C still reaches the pane and drops it, as any other key or
    /// click does. With `keep_selection_after_copy` off, the release clears it.
    #[gpui::test]
    fn a_copied_selection_stays_until_the_next_key_or_click(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            let mut frame = surface(&["hello there", "second row"], 12);
            let snapshot = view.live.snapshot.as_ref().unwrap();
            frame.boot_id = snapshot.boot_id.clone();
            frame.projection_revision = snapshot.revision;
            view.live.surface = Some(Arc::new(frame));
            view
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let (origin, cell) = view.read_with(cx, |view, _| {
            (
                view.bounds.origin,
                (view.cell_width, view.config.terminal.line_height()),
            )
        });
        let at = |column: f32, row: f32| -> Point<Pixels> {
            origin + point(px(column * cell.0), px(row * cell.1))
        };
        let clipboard = |cx: &mut gpui::VisualTestContext| {
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
        };
        let select = |cx: &mut gpui::VisualTestContext| {
            cx.simulate_mouse_down(at(0., 0.), MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_move(at(5., 0.), MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(at(5., 0.), MouseButton::Left, Modifiers::default());
        };
        let retained = |cx: &mut gpui::VisualTestContext| {
            view.read_with(cx, |view, _| view.selection_retained())
        };

        select(cx);
        assert_eq!(clipboard(cx).as_deref(), Some("hello"));
        assert!(retained(cx));
        for copy in ["cmd-c", "edit"] {
            cx.write_to_clipboard(ClipboardItem::new_string("before".into()));
            if copy == "edit" {
                cx.update(|window, cx| {
                    window.refresh();
                    window.draw(cx).clear(cx);
                    assert!(window.is_action_available(&crate::actions::Copy, cx));
                    window.dispatch_action(Box::new(crate::actions::Copy), cx);
                });
            } else {
                cx.simulate_keystrokes(copy);
            }
            assert_eq!(clipboard(cx).as_deref(), Some("hello"), "{copy}");
            assert!(retained(cx), "{copy}");
        }

        // Ctrl-C belongs to the pane once the release has copied.
        cx.write_to_clipboard(ClipboardItem::new_string("before".into()));
        cx.simulate_keystrokes("ctrl-c");
        assert_eq!(clipboard(cx).as_deref(), Some("before"));
        assert!(!retained(cx));

        select(cx);
        cx.simulate_click(at(8., 1.), Modifiers::default());
        assert!(!retained(cx));

        view.update(cx, |view, _| view.config.keep_selection_after_copy = false);
        select(cx);
        assert_eq!(clipboard(cx).as_deref(), Some("hello"));
        view.read_with(cx, |view, _| assert!(view.selection.is_none()));
    }

    #[gpui::test]
    fn chinese_mouse_selection_copies_exact_text_only_on_release(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            // Daemon-style wide cells: ordinary blank continuations, skip=false.
            let mut frame = surface(&["你 好 世 界 ", "A你  B"], 12);
            let snapshot = view.live.snapshot.as_ref().unwrap();
            frame.boot_id = snapshot.boot_id.clone();
            frame.projection_revision = snapshot.revision;
            view.live.surface = Some(Arc::new(frame));
            view
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let (origin, width, height) = view.read_with(cx, |view, _| {
            (
                view.bounds.origin,
                view.cell_width,
                view.config.terminal.line_height(),
            )
        });
        let at =
            |column: f32, row: f32| origin + point(px(column * width), px((row + 0.5) * height));
        for (from, to, row, expected) in [
            (0.1, 7.9, 0., "你好世界"),
            (7.9, 0.1, 0., "你好世界"),
            (0.1, 8.9, 0., "你好世界 "),
            (0.1, 4.9, 1., "A你 B"),
        ] {
            cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("before".into())));
            cx.simulate_mouse_down(at(from, row), MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_move(at(to, row), MouseButton::Left, Modifiers::default());
            assert_eq!(
                cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
                Some("before".into())
            );
            cx.simulate_mouse_up(at(to, row), MouseButton::Left, Modifiers::default());
            assert_eq!(
                cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
                Some(expected.into())
            );
            assert!(view.read_with(cx, |view, _| view.selection_retained()));
        }
    }

    /// A link is a destination for a click and text for a drag: the same
    /// press must be able to become either one.
    #[gpui::test]
    fn dragging_across_a_link_copies_it_instead_of_opening_it(cx: &mut TestAppContext) {
        let url = "https://example.com/x";
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            let mut frame = surface(&[url], 24);
            let snapshot = view.live.snapshot.as_ref().unwrap();
            frame.boot_id = snapshot.boot_id.clone();
            frame.projection_revision = snapshot.revision;
            view.live.surface = Some(Arc::new(frame));
            view
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let (origin, width) = view.read_with(cx, |view, _| (view.bounds.origin, view.cell_width));
        let at = |column: f32| origin + point(px(column * width), px(10.));

        cx.simulate_mouse_down(at(0.), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(at(20.6), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(at(20.6), MouseButton::Left, Modifiers::default());
        assert!(cx.opened_url().is_none());
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some(url.into())
        );

        // The press that never left its half-cell is still the click that opens
        // the link, and it copies nothing.
        cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("kept".into())));
        cx.simulate_click(at(1.), Modifiers::default());
        assert_eq!(cx.opened_url().as_deref(), Some(url));
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some("kept".into())
        );
    }

    #[gpui::test]
    fn application_mouse_takes_precedence_and_shift_keeps_copy_and_links(cx: &mut TestAppContext) {
        let url = "https://example.com/x";
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            let mut frame = surface(&[url], 24);
            let snapshot = view.live.snapshot.as_ref().unwrap();
            frame.boot_id = snapshot.boot_id.clone();
            frame.projection_revision = snapshot.revision;
            frame.panes[0].mouse_reporting = true;
            view.live.surface = Some(Arc::new(frame));
            view
        });
        cx.update(|window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string("kept".into()));
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let (origin, width) = view.read_with(cx, |view, _| (view.bounds.origin, view.cell_width));
        let at = |column: f32| origin + point(px(column * width), px(10.));
        cx.simulate_mouse_down(at(0.), MouseButton::Left, Modifiers::default());
        view.read_with(cx, |view, _| {
            assert!(view.selection.is_none());
            assert!(view.pressed_terminal_link.is_none());
        });
        cx.simulate_mouse_move(at(20.6), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(at(20.6), MouseButton::Left, Modifiers::default());
        cx.simulate_click(at(1.), Modifiers::default());
        assert!(cx.opened_url().is_none());
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some("kept".into())
        );
        cx.simulate_mouse_down(at(1.), MouseButton::Right, Modifiers::default());
        assert!(view.read_with(cx, |view, _| view.menu.page.is_none()));
        cx.simulate_mouse_up(at(1.), MouseButton::Right, Modifiers::default());

        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        cx.simulate_mouse_down(at(0.), MouseButton::Left, shift);
        assert!(view.read_with(cx, |view, _| view.selection.is_some()));
        cx.simulate_mouse_move(at(20.6), MouseButton::Left, shift);
        cx.simulate_mouse_up(at(20.6), MouseButton::Left, shift);
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some(url.into())
        );
        assert!(cx.opened_url().is_none());
        cx.simulate_click(at(1.), shift);
        assert_eq!(cx.opened_url().as_deref(), Some(url));
    }

    #[gpui::test]
    fn external_file_drag_cancels_local_selection_without_copying(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            let mut frame = surface(&["hello there"], 12);
            let snapshot = view.live.snapshot.as_ref().unwrap();
            frame.boot_id = snapshot.boot_id.clone();
            frame.projection_revision = snapshot.revision;
            view.live.surface = Some(Arc::new(frame));
            view
        });
        cx.update(|window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string("kept".into()));
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let (origin, width) = view.read_with(cx, |view, _| (view.bounds.origin, view.cell_width));
        let at = |column: f32| origin + point(px(column * width), px(10.));
        cx.simulate_mouse_down(at(0.), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(at(5.), MouseButton::Left, Modifiers::default());
        assert!(view.read_with(cx, |view, _| view.selection.is_some()));
        cx.simulate_event(gpui::FileDropEvent::Entered {
            position: at(5.),
            paths: gpui::ExternalPaths::default(),
        });
        assert!(view.read_with(cx, |view, _| view.selection.is_none()));
        cx.simulate_event(gpui::FileDropEvent::Submit { position: at(5.) });
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some("kept".into())
        );
    }

    /// The flash obeys the resolved clipboard-toast settings: turned off, a
    /// copy still happens silently, and each position puts it where it says.
    #[gpui::test]
    fn the_flash_follows_the_clipboard_toast_configuration(cx: &mut TestAppContext) {
        use crate::config::ClipboardToastPosition::*;
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            let mut frame = surface(&["configured"], 12);
            let snapshot = view.live.snapshot.as_ref().unwrap();
            frame.boot_id = snapshot.boot_id.clone();
            frame.projection_revision = snapshot.revision;
            view.live.surface = Some(Arc::new(frame));
            view
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let (origin, width) = view.read_with(cx, |view, _| (view.bounds.origin, view.cell_width));
        let at = |column: f32| origin + point(px(column * width), px(10.));
        let drag = |view: &gpui::Entity<HerdrWindow>, cx: &mut gpui::VisualTestContext| {
            cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("stale".into())));
            cx.simulate_mouse_down(at(0.), MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_move(at(10.), MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(at(10.), MouseButton::Left, Modifiers::default());
            view.read_with(cx, |view, _| view.flash.is_some())
        };

        view.update(cx, |view, _| view.config.clipboard_toast.enabled = false);
        assert!(!drag(&view, cx), "a silent copy is still a copy");
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some("configured".into())
        );

        // Each corner lands where it says, measured against the terminal area.
        view.update(cx, |view, _| view.config.clipboard_toast.enabled = true);
        let bounds = view.read_with(cx, |view, _| view.bounds);
        let mut seen = Vec::new();
        for position in [
            TopLeft,
            TopCenter,
            TopRight,
            BottomLeft,
            BottomCenter,
            BottomRight,
        ] {
            view.update(cx, |view, _| {
                view.config.clipboard_toast.position = position
            });
            assert!(drag(&view, cx));
            let flash = cx.debug_bounds("flash").expect("the flash paints");
            let top = matches!(position, TopLeft | TopCenter | TopRight);
            assert_eq!(
                flash.origin.y - bounds.origin.y < bounds.size.height / 2.,
                top,
                "{position:?}"
            );
            let left = flash.origin.x - bounds.origin.x;
            let right = bounds.size.width - (left + flash.size.width);
            match position {
                TopLeft | BottomLeft => assert!(left < right, "{position:?}"),
                TopRight | BottomRight => assert!(right < left, "{position:?}"),
                TopCenter | BottomCenter => {
                    assert!((left - right).abs() <= px(1.), "{position:?}")
                }
            }
            assert!(
                !seen.contains(&(flash.origin.x, flash.origin.y)),
                "{position:?}"
            );
            seen.push((flash.origin.x, flash.origin.y));
        }
    }

    /// A menu page holds the whole gesture: nothing is selected, copied, or
    /// reported while one is up.
    #[gpui::test]
    fn a_menu_page_holds_the_gesture(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            let mut frame = surface(&["copied text"], 12);
            let snapshot = view.live.snapshot.as_ref().unwrap();
            frame.boot_id = snapshot.boot_id.clone();
            frame.projection_revision = snapshot.revision;
            view.live.surface = Some(Arc::new(frame));
            view
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let (origin, cell) = view.read_with(cx, |view, _| {
            (
                view.bounds.origin,
                (view.cell_width, view.config.terminal.line_height()),
            )
        });
        let at = |column: f32| origin + point(px(column * cell.0), px(0.5 * cell.1));

        cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("kept".into())));
        view.update(cx, |view, cx| {
            view.menu.page = Some(crate::menu::Page::Menu);
            view.begin_selection(at(0.), 1, cx);
            assert!(view.selection.is_none());
            assert!(!view.extend_selection(at(6.), cx));
            assert!(!view.release_selection(cx));
            assert!(view.flash.is_none());
            view.menu.page = None;
        });
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some("kept".into())
        );

        // The same drag with the menu gone copies and reports.
        cx.simulate_mouse_down(at(0.), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(at(6.), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(at(6.), MouseButton::Left, Modifiers::default());
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some("copied".into())
        );
        view.read_with(cx, |view, _| assert!(view.flash.is_some()));
    }

    /// A double click copies the word under it and a triple click its row,
    /// through the same release that copies a drag.
    #[gpui::test]
    fn double_and_triple_clicks_copy_the_word_and_the_row(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            let mut frame = surface(&["cat src/lib.rs now", "next"], 20);
            let snapshot = view.live.snapshot.as_ref().unwrap();
            frame.boot_id = snapshot.boot_id.clone();
            frame.projection_revision = snapshot.revision;
            view.live.surface = Some(Arc::new(frame));
            view
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let (origin, cell) = view.read_with(cx, |view, _| {
            (
                view.bounds.origin,
                (view.cell_width, view.config.terminal.line_height()),
            )
        });
        let position = origin + point(px(6.5 * cell.0), px(0.5 * cell.1));
        let clipboard = |cx: &mut gpui::VisualTestContext| {
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
        };
        for (click_count, expected) in [(2, "src/lib.rs"), (3, "cat src/lib.rs now")] {
            cx.simulate_event(gpui::MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count,
                first_mouse: false,
            });
            cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
            assert_eq!(clipboard(cx), Some(expected.into()));
            view.read_with(cx, |view, _| assert!(view.selection_retained()));
        }
    }

    /// A drag held above a scrollable pane scrolls it one request at a time,
    /// and a selection that ends up reaching rows off the screen is read
    /// from the daemon on release instead of from the painted cells.
    #[gpui::test]
    fn a_drag_past_the_pane_scrolls_and_copies_through_the_daemon(cx: &mut TestAppContext) {
        use crate::window::MockPeer;
        use serde_json::{Value, json};
        let mut peer = MockPeer::advertising(&["pane.scroll", "pane.selection.read"]);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            peer.prepare(&mut view);
            view.live.supports_selection_read = true;
            let frame = surface(&["x"; 24], 80);
            let live = Arc::make_mut(view.live.surface.as_mut().unwrap());
            live.frame = frame.frame;
            live.panes[0].mouse_reporting = false;
            live.panes[0].scroll = Some(PaneSurfaceScrollMetrics {
                offset_from_bottom: 0,
                max_offset_from_bottom: 100,
                viewport_rows: 24,
            });
            view
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let (origin, cell) = view.read_with(cx, |view, _| {
            (
                view.bounds.origin,
                (view.cell_width, view.config.terminal.line_height()),
            )
        });
        let at = |column: f32, row: f32| origin + point(px(column * cell.0), px(row * cell.1));
        let next_request = |peer: &mut MockPeer| -> Value {
            loop {
                if let ClientMessage::ClientShellEndpointRequest { request, .. } = peer.receive() {
                    let request: Value = serde_json::from_str(&request).unwrap();
                    if matches!(
                        request["method"].as_str(),
                        Some("pane.scroll" | "pane.selection.read")
                    ) {
                        return request;
                    }
                    let id = request["id"].as_str().unwrap();
                    peer.respond("boot-v1", id, &json!({"id": id, "result": {"type": "ok"}}));
                }
            }
        };

        // Press on row 5 (content row 105) and hold the pointer two rows
        // above the pane.
        cx.simulate_mouse_down(at(3.2, 5.5), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(at(3.2, -1.5), MouseButton::Left, Modifiers::default());
        view.update(cx, |view, cx| view.follow_selection(cx));
        let scroll = next_request(&mut peer);
        assert_eq!(scroll["method"], "pane.scroll");
        assert_eq!(
            scroll["params"],
            json!({"pane_id": "w1:p1", "offset_from_bottom": 2})
        );
        // The next step waits for this one's answer.
        view.update(cx, |view, cx| {
            view.selection_follow.scrolled = None;
            view.follow_selection(cx);
            assert!(view.live.drag_request.is_some());
        });

        // The daemon answers, having scrolled the pane well up by now: the
        // selection follows the still pointer to the new top row, 45 rows
        // above where it started.
        let id = scroll["id"].as_str().unwrap();
        peer.respond("boot-v1", id, &json!({"id": id, "result": {"type": "ok"}}));
        view.update(cx, |view, cx| {
            view.live.drag_request = None;
            let live = Arc::make_mut(view.live.surface.as_mut().unwrap());
            live.panes[0].scroll.as_mut().unwrap().offset_from_bottom = 40;
            view.selection_follow.scrolled = None;
            view.follow_selection(cx);
        });
        // Still held above the pane, it keeps going from where the pane is.
        let again = next_request(&mut peer);
        assert_eq!(again["params"]["offset_from_bottom"], 42);
        let id = again["id"].as_str().unwrap();
        peer.respond("boot-v1", id, &json!({"id": id, "result": {"type": "ok"}}));
        cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("before".into())));
        cx.simulate_mouse_up(at(3.2, -1.5), MouseButton::Left, Modifiers::default());
        let read = next_request(&mut peer);
        assert_eq!(read["method"], "pane.selection.read");
        assert_eq!(
            read["params"],
            json!({
                "pane_id": "w1:p1",
                "anchor": {"row": 60, "col": 0},
                "cursor": {"row": 105, "col": 2},
            })
        );
        view.read_with(cx, |view, _| {
            assert!(view.selection.as_ref().is_some_and(|s| !s.dragging()))
        });

        // The answer, delivered as the connection's reader does, is copied.
        let id = read["id"].as_str().unwrap();
        let event = peer.respond(
            "boot-v1",
            id,
            &json!({"id": id, "result": {"type": "pane_selection",
                "pane_id": "w1:p1", "text": "from the history"}}),
        );
        let inbox = view.read_with(cx, |view, _| {
            view.endpoints[0].connection.scrollback.clone()
        });
        assert!(inbox.lock().unwrap().apply(event).is_none());
        view.update(cx, |view, cx| view.follow_selection(cx));
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some("from the history".into())
        );
        view.read_with(cx, |view, _| {
            assert!(view.flash.is_some());
            assert!(view.selection_follow.read.is_none());
        });
    }
}
