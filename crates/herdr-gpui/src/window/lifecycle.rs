//! Keeping the window's view of the daemon current: resending geometry only
//! when it actually changed, renaming the OS window after the focused space,
//! and reporting focus to the authoritative inbox as well as the wire.

use super::HerdrWindow;
use crate::{WINDOW_TITLE, sidebar};
use gpui::Window;

impl HerdrWindow {
    pub(crate) fn resize(&mut self) {
        if self.last_queued_options == Some(self.options) {
            return;
        }
        if let (Some(handle), Some(snapshot)) = (
            &self.endpoints[self.selected_endpoint].connection.handle,
            &self.live.snapshot,
        ) {
            match handle.resize(&snapshot.boot_id, self.options) {
                Ok(()) => self.last_queued_options = Some(self.options),
                Err(error) => self.local_error = Some(format!("Resize: {error}")),
            }
        }
    }

    /// macOS lists every window in the Window menu by title. Windows onto the
    /// same daemon are told apart by the space each one is showing.
    pub(crate) fn sync_window_title(&mut self, window: &mut Window) {
        let title = self
            .live
            .snapshot
            .as_ref()
            .and_then(|snapshot| {
                let focused = snapshot.focused_workspace_id.as_deref()?;
                snapshot
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.workspace_id == focused)
            })
            .map_or_else(
                || WINDOW_TITLE.to_owned(),
                |workspace| {
                    format!(
                        "{WINDOW_TITLE} \u{2014} {}",
                        sidebar::workspace_label(workspace, false)
                    )
                },
            );
        if self.title != title {
            window.set_window_title(&title);
            self.title = title;
        }
    }

    pub(crate) fn report_focus(&mut self) {
        // The first positive report acknowledges the daemon's active tab. Wait
        // until its surface is ready, but do not flap focus on later frame gaps.
        let focused = self.active
            && self.endpoints[self.selected_endpoint].surface_requested()
            && (self.sent_focus == Some(true) || self.input_ready());
        // Update the authoritative event inbox, not just the rendered clone.
        if let Ok(mut state) = self.endpoints[self.selected_endpoint]
            .connection
            .inbox
            .try_lock()
        {
            state.set_outer_focus(self.active && self.input_ready());
        }
        if self.sent_focus == Some(focused) {
            return;
        }
        if let (Some(handle), Some(snapshot)) = (
            &self.endpoints[self.selected_endpoint].connection.handle,
            &self.live.snapshot,
        ) && handle.set_focus(&snapshot.boot_id, focused).is_ok()
        {
            self.sent_focus = Some(focused);
        }
    }
}
