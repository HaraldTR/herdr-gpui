//! Links under the pointer. While the link modifier is held over a pane, the
//! daemon is asked which cells the link there covers, so a URL that wraps
//! onto the next row is underlined and opened whole. A click goes to the
//! daemon first when it offers activation, which lets a plugin link handler
//! claim it; otherwise, or when nothing claims it, this client opens the
//! address itself. Daemons without these methods keep the row-local detector.

use super::HerdrWindow;
use crate::{
    browser::WebUrl,
    links::{LinkCell, LinkInbox, LinkRequest, ResolvedLink, activation_fallback},
};
use gpui::{Context, Modifiers, Pixels, Point, Task, Window};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

/// Pointer movement is coalesced: at most one resolve leaves per delay, for
/// wherever the pointer is when it fires.
const RESOLVE_DELAY: Duration = Duration::from_millis(40);

#[derive(Default)]
pub(crate) struct DaemonLinks {
    /// The pane cell under the pointer while the link modifier is held.
    hover: Option<LinkCell>,
    /// The latest answer. It stays shown while the pointer moves along the
    /// link and the pane content it was read from is unchanged.
    resolved: Option<ResolvedLink>,
    resolving: Option<InFlight<LinkCell>>,
    activating: Option<InFlight<PendingActivation>>,
    delay: Option<Task<()>>,
}

/// A request in the mailbox it was queued on. A reconnect or another
/// endpoint replaces that mailbox, which retires the request unanswered.
struct InFlight<T> {
    id: String,
    inbox: Arc<Mutex<LinkInbox>>,
    context: T,
}

impl<T> InFlight<T> {
    /// The answer once it has arrived, or `Err` once it never can.
    fn poll(&self, current: &Arc<Mutex<LinkInbox>>, request: LinkRequest) -> PollResult {
        if !Arc::ptr_eq(&self.inbox, current) {
            return PollResult::Retired;
        }
        match self
            .inbox
            .try_lock()
            .ok()
            .and_then(|mut inbox| inbox.take(request, &self.id))
        {
            Some(result) => PollResult::Answered(result),
            None => PollResult::Waiting,
        }
    }
}

enum PollResult {
    Waiting,
    Retired,
    Answered(crate::Result<serde_json::Value>),
}

struct PendingActivation {
    /// What the row-local detector read at the click, opened when the
    /// daemon neither handles nor reads a web address there.
    fallback: Option<WebUrl>,
    in_tab: bool,
}

/// A press that may become a click on a link.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PressedLink {
    /// The address the row-local detector reads there, if any.
    pub(crate) url: Option<String>,
    /// The pane cell the daemon is asked to activate, when it can.
    pub(crate) cell: Option<LinkCell>,
    pub(crate) position: Point<Pixels>,
}

impl PressedLink {
    /// Whether a release reads the same link as this press did.
    fn same_link(&self, release: &Self) -> bool {
        self.url == release.url
            && match (&self.cell, &release.cell) {
                (Some(pressed), Some(released)) => pressed.same_content(released),
                (pressed, released) => pressed.is_none() && released.is_none(),
            }
    }
}

impl HerdrWindow {
    fn link_cell_at(&self, position: Point<Pixels>) -> Option<LinkCell> {
        if self.menu.page.is_some()
            || !self.live.surface_ready()
            || !self.bounds.contains(&position)
        {
            return None;
        }
        LinkCell::at(
            self.live.surface.as_deref()?,
            f32::from(position.x - self.bounds.origin.x),
            f32::from(position.y - self.bounds.origin.y),
            self.cell_width,
            self.config.terminal.line_height(),
        )
    }

    /// The resolved link under the pointer, while it still reads the live
    /// content it was resolved from.
    pub(crate) fn hovered_daemon_link(&self) -> Option<&ResolvedLink> {
        let hover = self.links.hover.as_ref()?;
        let link = self
            .links
            .resolved
            .as_ref()
            .filter(|link| link.covers(hover))?;
        link.cell
            .current(self.live.surface.as_deref()?)
            .then_some(link)
    }

    /// The cell at `position` when it lies on the hovered resolved link.
    pub(crate) fn daemon_link_at(&self, position: Point<Pixels>) -> Option<LinkCell> {
        let cell = self.link_cell_at(position)?;
        self.hovered_daemon_link()?.covers(&cell).then_some(cell)
    }

    /// Follows the pointer and the link modifier, asking the daemon about a
    /// cell no earlier answer covers.
    pub(crate) fn hover_link(
        &mut self,
        position: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let cell = (modifiers.secondary() && self.live.supports_link_resolve)
            .then(|| self.link_cell_at(position))
            .flatten();
        if cell == self.links.hover {
            return;
        }
        let shown = self.hovered_daemon_link().cloned();
        self.links.hover = cell;
        if self.hovered_daemon_link() != shown.as_ref() {
            cx.notify();
        }
        self.schedule_link_resolve(cx);
    }

    /// The hovered cell when no answer, in flight or arrived, covers it.
    fn unresolved_hover(&self) -> Option<&LinkCell> {
        let hover = self.links.hover.as_ref()?;
        let answered = self
            .links
            .resolved
            .as_ref()
            .is_some_and(|link| link.cell == *hover || link.covers(hover));
        (!answered).then_some(hover)
    }

    fn schedule_link_resolve(&mut self, cx: &mut Context<Self>) {
        if self.links.delay.is_some()
            || self.links.resolving.is_some()
            || self.unresolved_hover().is_none()
        {
            return;
        }
        self.links.delay = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESOLVE_DELAY).await;
            // The window may have closed while waiting; nothing is owed then.
            let _ = this.update(cx, |this, cx| {
                this.links.delay = None;
                this.send_link_resolve(cx);
            });
        }));
    }

    fn send_link_resolve(&mut self, cx: &mut Context<Self>) {
        if self.links.resolving.is_some() {
            return;
        }
        let Some(cell) = self.unresolved_hover().cloned() else {
            return;
        };
        let connection = &self.endpoints[self.selected_endpoint].connection;
        match connection.request_link(LinkRequest::Resolve, &cell) {
            Ok(id) => {
                self.links.resolving = Some(InFlight {
                    id,
                    inbox: connection.links.clone(),
                    context: cell,
                });
            }
            // The event reader holds the mailbox for a moment; try again.
            Err(crate::Error::ConnectionBusy) => self.schedule_link_resolve(cx),
            // Hover is optional: a cell that cannot be asked about shows no
            // link and is not asked about again.
            Err(error) => {
                tracing::debug!(%error, "Link resolve not sent");
                self.links.resolved = Some(ResolvedLink {
                    cell,
                    regions: Vec::new(),
                });
            }
        }
    }

    /// Folds link answers into the window, and follows a pane that changed
    /// or scrolled under a still pointer.
    pub(crate) fn poll_links(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.endpoints[self.selected_endpoint]
            .connection
            .links
            .clone();
        if let Some(flight) = &self.links.resolving {
            match flight.poll(&current, LinkRequest::Resolve) {
                PollResult::Waiting => {}
                PollResult::Retired => self.links.resolving = None,
                PollResult::Answered(result) => {
                    let cell = flight.context.clone();
                    self.links.resolving = None;
                    let shown = self.hovered_daemon_link().cloned();
                    self.links.resolved = Some(match result {
                        Ok(response) => ResolvedLink::from_response(cell, &response),
                        Err(_) => ResolvedLink {
                            cell,
                            regions: Vec::new(),
                        },
                    });
                    if self.hovered_daemon_link() != shown.as_ref() {
                        cx.notify();
                    }
                }
            }
        }
        if let Some(flight) = &self.links.activating {
            match flight.poll(&current, LinkRequest::Activate) {
                PollResult::Waiting => {}
                // The daemon may already have run a handler; opening the
                // address as well could act on one click twice.
                PollResult::Retired => self.links.activating = None,
                PollResult::Answered(result) => {
                    let in_tab = flight.context.in_tab;
                    let fallback = flight.context.fallback.clone();
                    self.links.activating = None;
                    if let Some(url) = activation_fallback(result, fallback) {
                        self.open_web_link(url, in_tab, window, cx);
                    }
                }
            }
        }
        self.hover_link(window.mouse_position(), window.modifiers(), cx);
        self.schedule_link_resolve(cx);
    }

    /// The link a press at `position` is on, by either reading.
    pub(crate) fn terminal_link_press(&self, position: Point<Pixels>) -> Option<PressedLink> {
        let url = self.terminal_link_at(position);
        let daemon = self.daemon_link_at(position);
        if url.is_none() && daemon.is_none() {
            return None;
        }
        // A plugin handler may claim any link the daemon can read, not only
        // one the pointer hovered with the modifier held.
        let cell = self
            .live
            .supports_link_activate
            .then(|| daemon.or_else(|| self.link_cell_at(position)))
            .flatten();
        Some(PressedLink {
            url,
            cell,
            position,
        })
    }

    /// Opens the link a click released on, if it is the one it pressed.
    pub(crate) fn activate_terminal_link(
        &mut self,
        pressed: &PressedLink,
        release: Point<Pixels>,
        in_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(release) = self.terminal_link_press(release) else {
            return false;
        };
        if !pressed.same_link(&release) {
            return false;
        }
        let fallback = release
            .url
            .as_deref()
            .and_then(|url| WebUrl::try_from(url).ok());
        if let Some(cell) = &release.cell {
            // One click at a time: a second while the first is unanswered
            // would race its handler.
            if self.links.activating.is_some() {
                return true;
            }
            let connection = &self.endpoints[self.selected_endpoint].connection;
            match connection.request_link(LinkRequest::Activate, cell) {
                Ok(id) => {
                    self.links.activating = Some(InFlight {
                        id,
                        inbox: connection.links.clone(),
                        context: PendingActivation { fallback, in_tab },
                    });
                    return true;
                }
                Err(error) => tracing::debug!(%error, "Link activation not sent"),
            }
        }
        if let Some(url) = fallback {
            self.open_web_link(url, in_tab, window, cx);
        }
        true
    }

    /// Opens a web address in a browser tab or the system browser, as the
    /// settings and the click's modifiers chose.
    fn open_web_link(
        &mut self,
        url: WebUrl,
        in_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if in_tab && crate::browser::EMBEDDED {
            self.open_browser_tab(Some(url), window, cx);
        } else {
            cx.open_url(&String::from(url));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::{sidebar::layout_tests::fixture_window, state::ConnectionStatus, window::MockPeer};
    use gpui::{Entity, VisualTestContext, point, px};
    use herdr_client::protocol::{
        CellData, ClientShellSnapshot, FrameData, PaneSurfaceFrame, PaneSurfacePane, SurfaceRect,
    };
    use serde_json::json;

    const URL: &str = "https://example.com/wrapped/path";

    fn snapshot() -> ClientShellSnapshot {
        serde_json::from_str(include_str!(
            "../../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
        ))
        .unwrap()
    }

    /// A 20-column pane whose URL soft-wraps onto its second row, which the
    /// row-local detector does not read as a link.
    fn wrapped(content_revision: u64) -> Arc<PaneSurfaceFrame> {
        let snapshot = snapshot();
        let (width, height) = (20, 4);
        let mut symbols = URL.chars();
        let rect = SurfaceRect {
            x: 0,
            y: 0,
            width,
            height,
        };
        Arc::new(PaneSurfaceFrame {
            boot_id: snapshot.boot_id,
            projection_revision: snapshot.revision,
            surface_revision: 1,
            frame: FrameData {
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
            },
            panes: vec![PaneSurfacePane {
                pane_id: "w1:p1".into(),
                content_revision,
                rect,
                inner_rect: rect,
                scrollbar_rect: None,
                scroll: None,
                focused: true,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                alternate_screen_active: false,
                pixel_width: 200,
                pixel_height: 80,
            }],
            splits: vec![],
            popup: None,
            graphics: Default::default(),
        })
    }

    fn window<'a>(
        peer: &MockPeer,
        supported: bool,
        cx: &'a mut gpui::TestAppContext,
    ) -> (Entity<HerdrWindow>, &'a mut VisualTestContext) {
        let handle = peer.client.handle.clone();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = fixture_window(window, cx);
            view.live.snapshot = Some(Arc::new(snapshot()));
            view.live.surface = Some(wrapped(2));
            view.live.status = ConnectionStatus::Connected;
            view.live.supports_link_resolve = supported;
            view.live.supports_link_activate = supported;
            view.endpoints[view.selected_endpoint].connection.handle = Some(handle);
            view
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        (view, cx)
    }

    /// The middle of pane cell (`col`, `row`).
    fn at(
        view: &Entity<HerdrWindow>,
        cx: &mut VisualTestContext,
        col: u16,
        row: u16,
    ) -> Point<Pixels> {
        view.read_with(cx, |view, _| {
            let height = view.config.terminal.line_height();
            view.bounds.origin
                + point(
                    px((f32::from(col) + 0.5) * view.cell_width),
                    px((f32::from(row) + 0.5) * height),
                )
        })
    }

    /// Answers `request` with `result`, moves the client's event into the
    /// link mailbox as the bridge's reader thread does, and lets the window
    /// fold it in.
    fn answer(
        peer: &mut MockPeer,
        view: &Entity<HerdrWindow>,
        cx: &mut VisualTestContext,
        request: &serde_json::Value,
        result: serde_json::Value,
    ) {
        let id = request["id"].as_str().unwrap();
        let event = peer.respond(
            &snapshot().boot_id,
            id,
            &json!({"id": id, "result": result}),
        );
        let links = view.read_with(cx, |view, _| {
            view.endpoints[view.selected_endpoint]
                .connection
                .links
                .clone()
        });
        assert!(links.lock().unwrap().apply(event).is_none());
        cx.update(|window, cx| view.update(cx, |view, cx| view.poll_links(window, cx)));
    }

    fn resolve_after_delay(cx: &mut VisualTestContext) {
        cx.executor().advance_clock(RESOLVE_DELAY * 2);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_wrapped_link_is_resolved_underlined_and_activated_by_the_daemon(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut peer = MockPeer::advertising(&["pane.link.resolve", "pane.link.activate"]);
        let (view, cx) = window(&peer, true, cx);
        let continuation = at(&view, cx, 2, 1);
        view.read_with(cx, |view, _| {
            assert!(view.terminal_link_at(continuation).is_none());
        });

        // Movement within the delay leaves one request, for the latest cell.
        let elsewhere = at(&view, cx, 9, 2);
        cx.simulate_mouse_move(elsewhere, None, Modifiers::secondary_key());
        cx.simulate_mouse_move(continuation, None, Modifiers::secondary_key());
        resolve_after_delay(cx);
        let request = peer.request();
        assert_eq!(request["method"], "pane.link.resolve");
        assert_eq!(
            request["params"],
            json!({
                "pane_id": "w1:p1", "viewport_row": 1, "col": 2,
                "content_revision": 2, "offset_from_bottom": null,
            })
        );
        answer(
            &mut peer,
            &view,
            cx,
            &request,
            json!({"type": "pane_link_resolved", "regions": [
                {"row": 0, "start_col": 0, "end_col": 19},
                {"row": 1, "start_col": 0, "end_col": 11},
            ]}),
        );
        view.read_with(cx, |view, _| {
            let link = view.hovered_daemon_link().unwrap();
            assert_eq!(
                link.frame_rows().collect::<Vec<_>>(),
                vec![(0, 0..20), (1, 0..12)]
            );
            assert!(view.terminal_link_hovered(continuation, Modifiers::secondary_key()));
            assert!(view.link_modifier_held(continuation, Modifiers::secondary_key()));
        });

        // Moving along the link asks nothing new. A click goes to the daemon;
        // a plugin handler that claims it leaves nothing to open here.
        let first_row = at(&view, cx, 5, 0);
        cx.simulate_mouse_move(first_row, None, Modifiers::secondary_key());
        resolve_after_delay(cx);
        cx.simulate_click(continuation, Modifiers::secondary_key());
        let request = peer.request();
        assert_eq!(request["method"], "pane.link.activate");
        assert_eq!(request["params"]["viewport_row"], 1);
        assert_eq!(request["params"]["col"], 2);
        answer(
            &mut peer,
            &view,
            cx,
            &request,
            json!({"type": "pane_link_activated", "url": URL, "handled": true}),
        );
        assert!(cx.opened_url().is_none());

        // When nothing claims it, the whole wrapped address opens here.
        cx.simulate_click(continuation, Modifiers::secondary_key());
        let request = peer.request();
        assert_eq!(request["method"], "pane.link.activate");
        assert!(cx.opened_url().is_none());
        answer(
            &mut peer,
            &view,
            cx,
            &request,
            json!({"type": "pane_link_activated", "url": URL, "handled": false}),
        );
        assert_eq!(cx.opened_url().as_deref(), Some(URL));

        // New pane content hides the old answer and asks again, for the
        // content now shown.
        view.update(cx, |view, _| view.live.surface = Some(wrapped(4)));
        cx.update(|window, cx| view.update(cx, |view, cx| view.poll_links(window, cx)));
        view.read_with(cx, |view, _| assert!(view.hovered_daemon_link().is_none()));
        resolve_after_delay(cx);
        let request = peer.request();
        assert_eq!(request["method"], "pane.link.resolve");
        assert_eq!(request["params"]["content_revision"], 4);
        assert_eq!(request["params"]["viewport_row"], 1);

        // Releasing the modifier drops the hover; its answer arriving later
        // shows nothing.
        cx.simulate_mouse_move(continuation, None, Modifiers::default());
        answer(
            &mut peer,
            &view,
            cx,
            &request,
            json!({"type": "pane_link_resolved", "regions": [
                {"row": 1, "start_col": 0, "end_col": 11},
            ]}),
        );
        view.read_with(cx, |view, _| {
            assert!(view.hovered_daemon_link().is_none());
            assert!(!view.terminal_link_hovered(continuation, Modifiers::default()));
        });
    }

    #[gpui::test]
    fn a_daemon_without_link_methods_keeps_the_local_detector(cx: &mut gpui::TestAppContext) {
        let peer = MockPeer::new();
        let (view, cx) = window(&peer, false, cx);
        let link = at(&view, cx, 2, 0);
        cx.simulate_mouse_move(link, None, Modifiers::secondary_key());
        resolve_after_delay(cx);
        view.read_with(cx, |view, _| {
            assert!(view.links.hover.is_none() && view.links.resolving.is_none());
        });
        // The row-local reading does not guess where a wrapped address
        // ends, so neither row is a link and nothing opens.
        let continuation = at(&view, cx, 2, 1);
        for position in [link, continuation] {
            view.read_with(cx, |view, _| {
                assert!(!view.terminal_link_hovered(position, Modifiers::secondary_key()));
                assert!(view.terminal_link_press(position).is_none());
            });
            cx.simulate_click(position, Modifiers::secondary_key());
        }
        assert!(cx.opened_url().is_none());
    }

    #[gpui::test]
    fn a_replaced_connection_retires_link_requests_unanswered(cx: &mut gpui::TestAppContext) {
        let mut peer = MockPeer::advertising(&["pane.link.resolve", "pane.link.activate"]);
        let (view, cx) = window(&peer, true, cx);
        let continuation = at(&view, cx, 2, 1);
        cx.simulate_mouse_move(continuation, None, Modifiers::secondary_key());
        resolve_after_delay(cx);
        assert_eq!(peer.request()["method"], "pane.link.resolve");
        view.update(cx, |view, _| {
            assert!(view.links.resolving.is_some());
            view.endpoints[view.selected_endpoint].connection.links = Arc::default();
            view.links.activating = Some(InFlight {
                id: "old".into(),
                inbox: Arc::default(),
                context: PendingActivation {
                    fallback: WebUrl::try_from(URL).ok(),
                    in_tab: false,
                },
            });
        });
        cx.update(|window, cx| view.update(cx, |view, cx| view.poll_links(window, cx)));
        view.read_with(cx, |view, _| {
            assert!(view.links.activating.is_none());
            assert!(view.hovered_daemon_link().is_none());
        });
        // The daemon may have run a handler for the old click, so nothing opens.
        assert!(cx.opened_url().is_none());
    }
}
