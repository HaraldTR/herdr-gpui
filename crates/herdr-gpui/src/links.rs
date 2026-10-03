//! Links the daemon reads from its own terminal state: `pane.link.resolve`
//! names every cell a link covers, including the rows a soft-wrapped URL
//! continues onto, and `pane.link.activate` lets a plugin `[[link_handlers]]`
//! entry claim a click before this client opens the address itself.
//!
//! Both are optional. A daemon that does not advertise them keeps the
//! row-local detector in `terminal::links`, which needs no round trip.

use crate::terminal::{InputTarget, wheel_target};
use herdr_client::{ClientEvent, Method, protocol::PaneSurfaceFrame, protocol::SurfaceRect};
use serde::Deserialize;
use serde_json::{Value, json};

/// The resolve and activate requests in flight on one connection. Their
/// responses stop here instead of reaching the live state, so a hover that
/// went stale is never reported as a connection error. Like the live inbox,
/// a reconnect replaces the whole mailbox, so a late answer cannot land in the
/// next connection.
#[derive(Default)]
pub(crate) struct LinkInbox {
    resolving: Option<Pending>,
    activating: Option<Pending>,
}

struct Pending {
    id: String,
    result: Option<crate::Result<Value>>,
}

/// Which of the two link requests a mailbox slot holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkRequest {
    Resolve,
    Activate,
}

impl LinkRequest {
    fn method(self) -> Method {
        match self {
            Self::Resolve => Method::PaneLinkResolve,
            Self::Activate => Method::PaneLinkActivate,
        }
    }
}

impl LinkRequest {
    /// Whether the daemon advertised this request when it welcomed us.
    pub(crate) fn advertised_in(self, methods: &[String]) -> bool {
        self.method().advertised_in(methods)
    }
}

impl LinkInbox {
    fn slot(&mut self, request: LinkRequest) -> &mut Option<Pending> {
        match request {
            LinkRequest::Resolve => &mut self.resolving,
            LinkRequest::Activate => &mut self.activating,
        }
    }

    /// Records the answers to this mailbox's own requests, passing every
    /// other event on.
    pub(crate) fn apply(&mut self, event: ClientEvent) -> Option<ClientEvent> {
        if let ClientEvent::Disconnected { .. } = &event {
            for pending in [&mut self.resolving, &mut self.activating]
                .into_iter()
                .flatten()
            {
                pending
                    .result
                    .get_or_insert(Err(crate::Error::NotConnected));
            }
        }
        let (id, error) = match &event {
            ClientEvent::Response { request_id, .. } => (request_id, false),
            ClientEvent::CommandRejected {
                request_id: Some(request_id),
                ..
            } => (request_id, true),
            _ => return Some(event),
        };
        let Some(pending) = [&mut self.resolving, &mut self.activating]
            .into_iter()
            .flatten()
            .find(|pending| pending.id == *id && pending.result.is_none())
        else {
            return Some(event);
        };
        pending.result = Some(match event {
            ClientEvent::Response { response, .. } if !error => Ok(response),
            ClientEvent::CommandRejected { reason, .. } => Err(crate::Error::Client(reason)),
            _ => Err(crate::Error::NotConnected),
        });
        None
    }

    /// Queues one request while holding the mailbox, so even an immediate
    /// rejection finds it registered. Each kind has a single slot: a second
    /// request waits until the first is answered and taken. The worker
    /// rejects a method the daemon did not advertise.
    pub(crate) fn request(
        &mut self,
        handle: &herdr_client::ClientHandle,
        request: LinkRequest,
        cell: &LinkCell,
    ) -> crate::Result<String> {
        if self.slot(request).is_some() {
            return Err(crate::Error::ConnectionBusy);
        }
        let id = handle.request(&cell.boot, request.method(), cell.params())?;
        *self.slot(request) = Some(Pending {
            id: id.clone(),
            result: None,
        });
        Ok(id)
    }

    /// The answer to request `id`, freeing its slot. `None` while it is
    /// still in flight, or when the slot belongs to another request.
    pub(crate) fn take(&mut self, request: LinkRequest, id: &str) -> Option<crate::Result<Value>> {
        let slot = self.slot(request);
        if slot
            .as_ref()
            .is_none_or(|pending| pending.id != id || pending.result.is_none())
        {
            return None;
        }
        slot.take()?.result
    }
}

/// One pane cell and the content it was read from. The daemon refuses a
/// request whose pane has since changed, scrolled, or moved, and this client
/// drops an answer for a cell the pointer has left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkCell {
    boot: String,
    pane_id: String,
    inner_rect: SurfaceRect,
    content_revision: u64,
    offset_from_bottom: Option<u64>,
    /// Viewport row and column within the pane's inner rect.
    row: u16,
    col: u16,
}

impl LinkCell {
    /// The pane cell under a point in surface pixels. A popup covers every
    /// pane, and the daemon resolves pane content only, so a popup yields
    /// nothing here and keeps the local detector.
    pub(crate) fn at(
        surface: &PaneSurfaceFrame,
        x: f32,
        y: f32,
        cell_width: f32,
        cell_height: f32,
    ) -> Option<Self> {
        let InputTarget::Pane(pane_id) =
            wheel_target(surface, x, y, cell_width, cell_height)?.target
        else {
            return None;
        };
        let pane = surface.panes.iter().find(|pane| pane.pane_id == pane_id)?;
        // An odd revision is the daemon's write-in-progress marker, which it
        // would refuse as stale.
        if pane.content_revision % 2 != 0 {
            return None;
        }
        let rect = pane.inner_rect;
        let col = ((x / cell_width).floor() as u16).checked_sub(rect.x)?;
        let row = ((y / cell_height).floor() as u16).checked_sub(rect.y)?;
        (col < rect.width && row < rect.height).then(|| Self {
            boot: surface.boot_id.clone(),
            pane_id,
            inner_rect: rect,
            content_revision: pane.content_revision,
            offset_from_bottom: pane.scroll.map(|scroll| scroll.offset_from_bottom),
            row,
            col,
        })
    }

    /// Whether both cells read the same pane content, wherever in it they are.
    pub(crate) fn same_content(&self, other: &Self) -> bool {
        self.boot == other.boot
            && self.pane_id == other.pane_id
            && self.inner_rect == other.inner_rect
            && self.content_revision == other.content_revision
            && self.offset_from_bottom == other.offset_from_bottom
    }

    /// Whether `surface` still shows the content this cell was read from.
    pub(crate) fn current(&self, surface: &PaneSurfaceFrame) -> bool {
        surface.boot_id == self.boot
            && surface.popup.is_none()
            && surface.panes.iter().any(|pane| {
                pane.pane_id == self.pane_id
                    && pane.inner_rect == self.inner_rect
                    && pane.content_revision == self.content_revision
                    && pane.scroll.map(|scroll| scroll.offset_from_bottom)
                        == self.offset_from_bottom
            })
    }

    fn params(&self) -> Value {
        json!({
            "pane_id": self.pane_id,
            "viewport_row": self.row,
            "col": self.col,
            "content_revision": self.content_revision,
            "offset_from_bottom": self.offset_from_bottom,
        })
    }
}

/// One row of a resolved link, in the pane's viewport. Both columns are
/// inclusive, as the daemon reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) struct LinkRegion {
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
}

impl LinkRegion {
    fn contains(&self, cell: &LinkCell) -> bool {
        self.row == cell.row && (self.start_col..=self.end_col).contains(&cell.col)
    }
}

/// A resolved link and the cell it was resolved for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedLink {
    pub cell: LinkCell,
    pub regions: Vec<LinkRegion>,
}

impl ResolvedLink {
    /// The link the daemon resolved for `cell`. Anything malformed, out of
    /// the pane, or not covering the asked-for cell is no link: the daemon's
    /// answer is untrusted data, and a hover may simply show nothing.
    pub(crate) fn from_response(cell: LinkCell, response: &Value) -> Self {
        #[derive(Deserialize)]
        struct Resolved {
            regions: Vec<LinkRegion>,
        }
        let rect = cell.inner_rect;
        let regions = response
            .get("error")
            .is_none_or(Value::is_null)
            .then(|| response.get("result"))
            .flatten()
            .filter(|result| result["type"] == "pane_link_resolved")
            .and_then(|result| Resolved::deserialize(result).ok())
            .map(|resolved| resolved.regions)
            .filter(|regions| {
                regions.len() <= usize::from(rect.height)
                    && regions.iter().all(|region| {
                        region.row < rect.height
                            && region.start_col <= region.end_col
                            && region.end_col < rect.width
                    })
                    && regions.iter().any(|region| region.contains(&cell))
            })
            .unwrap_or_default();
        Self { cell, regions }
    }

    /// Whether `cell` reads the same content and lies on this link.
    pub(crate) fn covers(&self, cell: &LinkCell) -> bool {
        self.cell.same_content(cell) && self.regions.iter().any(|region| region.contains(cell))
    }

    /// The covered cells as `(row, columns)` in the surface frame's grid,
    /// the shape the painter takes.
    pub(crate) fn frame_rows(&self) -> impl Iterator<Item = (u16, std::ops::Range<u16>)> + '_ {
        let rect = self.cell.inner_rect;
        self.regions.iter().map(move |region| {
            (
                rect.y.saturating_add(region.row),
                rect.x.saturating_add(region.start_col)
                    ..rect.x.saturating_add(region.end_col).saturating_add(1),
            )
        })
    }
}

/// The address this client opens itself after a click the daemon was asked
/// to activate: nothing when a plugin handled it, otherwise the daemon's own
/// reading of the link, which may span wrapped rows, and failing that the
/// row-local `fallback`. Only bounded web addresses are ever opened.
pub(crate) fn activation_fallback(
    response: crate::Result<Value>,
    fallback: Option<crate::browser::WebUrl>,
) -> Option<crate::browser::WebUrl> {
    let Ok(response) = response else {
        return fallback;
    };
    if response.get("error").is_some_and(|error| !error.is_null()) {
        return fallback;
    }
    let result = &response["result"];
    if result["type"] != "pane_link_activated" {
        return fallback;
    }
    if result["handled"] == true {
        return None;
    }
    result["url"]
        .as_str()
        .and_then(|url| crate::browser::WebUrl::try_from(url).ok())
        .or(fallback)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use herdr_client::protocol::{
        FrameData, PaneSurfacePane, PaneSurfaceScrollMetrics, SurfaceGraphicsScene,
        endpoint::EndpointServerWelcome,
    };

    fn rect(x: u16, y: u16, width: u16, height: u16) -> SurfaceRect {
        SurfaceRect {
            x,
            y,
            width,
            height,
        }
    }

    fn surface(content_revision: u64, offset: u64) -> PaneSurfaceFrame {
        PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: FrameData {
                width: 40,
                height: 10,
                cells: vec![],
                cursor: None,
                hyperlinks: vec![],
                graphics: vec![],
            },
            panes: vec![PaneSurfacePane {
                pane_id: "p1".into(),
                content_revision,
                rect: rect(0, 0, 40, 10),
                inner_rect: rect(2, 1, 30, 8),
                scrollbar_rect: None,
                scroll: Some(PaneSurfaceScrollMetrics {
                    offset_from_bottom: offset,
                    max_offset_from_bottom: 100,
                    viewport_rows: 8,
                }),
                focused: true,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                alternate_screen_active: false,
                pixel_width: 0,
                pixel_height: 0,
            }],
            splits: Vec::new(),
            popup: None,
            graphics: SurfaceGraphicsScene::default(),
        }
    }

    /// The cell at surface column `x`, row `y`, with 10x20 pixel cells.
    fn cell(surface: &PaneSurfaceFrame, x: u16, y: u16) -> Option<LinkCell> {
        LinkCell::at(
            surface,
            f32::from(x) * 10. + 5.,
            f32::from(y) * 20. + 5.,
            10.,
            20.,
        )
    }

    fn resolved(regions: Value) -> Value {
        json!({"id": "gpui-1", "result": {"type": "pane_link_resolved", "regions": regions}})
    }

    fn welcome(methods: &[&str]) -> ClientEvent {
        ClientEvent::Connected(EndpointServerWelcome {
            generation: 1,
            server_version: "test".into(),
            snapshot_codec: String::new(),
            surface_codec: String::new(),
            input_codec: String::new(),
            blob_codec: String::new(),
            methods: methods.iter().map(|method| (*method).into()).collect(),
            capabilities: Vec::new(),
            error: None,
        })
    }

    #[test]
    fn cells_are_pane_relative_and_carry_the_content_they_read() {
        let surface = surface(4, 3);
        let target = cell(&surface, 5, 3).unwrap();
        assert_eq!((target.row, target.col), (2, 3));
        assert_eq!(
            target.params(),
            json!({
                "pane_id": "p1", "viewport_row": 2, "col": 3,
                "content_revision": 4, "offset_from_bottom": 3,
            })
        );
        // Borders and the area outside the pane are not pane cells.
        assert!(cell(&surface, 1, 3).is_none());
        assert!(cell(&surface, 5, 0).is_none());
        assert!(cell(&surface, 32, 3).is_none());
        // A write in progress is not a revision the daemon would answer for.
        assert!(cell(&self::surface(5, 3), 5, 3).is_none());
    }

    #[test]
    fn a_cell_goes_stale_when_its_pane_changes_scrolls_or_is_covered() {
        let target = cell(&surface(4, 3), 5, 3).unwrap();
        assert!(target.current(&surface(4, 3)));
        assert!(!target.current(&surface(6, 3)));
        assert!(!target.current(&surface(4, 2)));
        let mut moved = surface(4, 3);
        moved.panes[0].inner_rect = rect(2, 1, 29, 8);
        assert!(!target.current(&moved));
        let mut other_boot = surface(4, 3);
        other_boot.boot_id = "next".into();
        assert!(!target.current(&other_boot));
        assert!(target.same_content(&cell(&surface(4, 3), 9, 6).unwrap()));
        assert!(!target.same_content(&cell(&surface(6, 3), 5, 3).unwrap()));
    }

    #[test]
    fn a_wrapped_link_covers_every_row_it_continues_onto() {
        let surface = surface(4, 0);
        let target = cell(&surface, 5, 3).unwrap();
        let link = ResolvedLink::from_response(
            target,
            &resolved(json!([
                {"row": 2, "start_col": 1, "end_col": 29},
                {"row": 3, "start_col": 0, "end_col": 6},
            ])),
        );
        assert_eq!(link.regions.len(), 2);
        // The continuation on the next row is part of the same link.
        assert!(link.covers(&cell(&surface, 4, 4).unwrap()));
        assert!(!link.covers(&cell(&surface, 9, 4).unwrap()));
        assert!(!link.covers(&cell(&surface, 2, 3).unwrap()));
        // Painted in frame coordinates, past the pane's inner origin, with
        // the inclusive end column made exclusive.
        assert_eq!(
            link.frame_rows().collect::<Vec<_>>(),
            vec![(3, 3..32), (4, 2..9)]
        );
    }

    #[test]
    fn untrusted_resolutions_that_leave_the_pane_or_miss_the_cell_are_no_link() {
        let target = cell(&surface(4, 0), 5, 3).unwrap();
        for response in [
            resolved(json!([{"row": 2, "start_col": 0, "end_col": 30}])),
            resolved(json!([{"row": 8, "start_col": 0, "end_col": 3}])),
            resolved(json!([{"row": 2, "start_col": 5, "end_col": 4}])),
            resolved(json!([{"row": 2, "start_col": 4, "end_col": 9}])),
            resolved(Value::Array(vec![
                json!({"row": 2, "start_col": 0, "end_col": 9});
                9
            ])),
            resolved(json!([])),
            resolved(json!("nonsense")),
            json!({"id": "gpui-1", "result": {"type": "pane_link_activated", "handled": false}}),
            json!({"id": "gpui-1", "error": {"code": "stale_content", "message": "changed"}}),
            json!({"id": "gpui-1"}),
        ] {
            let link = ResolvedLink::from_response(target.clone(), &response);
            assert!(link.regions.is_empty(), "{response}");
        }
    }

    #[test]
    fn a_handled_click_opens_nothing_and_a_declined_one_opens_only_web_addresses() {
        let local = || crate::browser::WebUrl::try_from("https://local.example/").ok();
        let activated = |result: Value| Ok(json!({"id": "gpui-2", "result": result}));
        let opened = |response, fallback| activation_fallback(response, fallback).map(String::from);
        assert_eq!(
            opened(
                activated(
                    json!({"type": "pane_link_activated", "url": "https://x.example/", "handled": true})
                ),
                local()
            ),
            None
        );
        assert_eq!(
            opened(
                activated(
                    json!({"type": "pane_link_activated", "url": "https://wrapped.example/a/b", "handled": false})
                ),
                local()
            ),
            Some("https://wrapped.example/a/b".into())
        );
        // A destination this client must never open falls back to the local
        // reading of the row, and to nothing without one.
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "x-man-page://ls",
        ] {
            let declined =
                || activated(json!({"type": "pane_link_activated", "url": url, "handled": false}));
            assert_eq!(
                opened(declined(), local()),
                Some("https://local.example/".into())
            );
            assert_eq!(opened(declined(), None), None);
        }
        for failed in [
            activated(json!({"type": "pane_link_activated", "handled": false})),
            Ok(json!({"id": "gpui-2", "error": {"code": "stale_content", "message": "moved"}})),
            Err(crate::Error::NotConnected),
        ] {
            assert_eq!(
                opened(failed, local()),
                Some("https://local.example/".into())
            );
        }
    }

    #[test]
    fn link_methods_are_advertised_separately() {
        let methods = ["pane.link.resolve".to_string()];
        assert!(LinkRequest::Resolve.advertised_in(&methods));
        assert!(!LinkRequest::Activate.advertised_in(&methods));
    }

    #[test]
    fn the_mailbox_keeps_its_answers_and_passes_everything_else_on() {
        let mut inbox = LinkInbox::default();
        assert!(inbox.apply(welcome(&["pane.link.resolve"])).is_some());
        inbox.resolving = Some(Pending {
            id: "gpui-1".into(),
            result: None,
        });
        let other = ClientEvent::Response {
            request_id: "gpui-9".into(),
            response: json!({"id": "gpui-9", "error": {"code": "x", "message": "y"}}),
        };
        assert!(inbox.apply(other).is_some());
        assert!(inbox.take(LinkRequest::Resolve, "gpui-1").is_none());
        // A stale-content error is the hover's own business, not the window's.
        let stale = ClientEvent::Response {
            request_id: "gpui-1".into(),
            response: json!({"id": "gpui-1", "error": {"code": "stale_content", "message": "changed"}}),
        };
        assert!(inbox.apply(stale).is_none());
        // Another request's id does not claim the answer or free the slot.
        assert!(inbox.take(LinkRequest::Resolve, "gpui-2").is_none());
        assert!(inbox.take(LinkRequest::Activate, "gpui-1").is_none());
        assert!(inbox.take(LinkRequest::Resolve, "gpui-1").unwrap().is_ok());
        assert!(inbox.resolving.is_none());
    }

    #[test]
    fn rejection_and_disconnection_answer_the_pending_request() {
        let mut inbox = LinkInbox {
            activating: Some(Pending {
                id: "gpui-3".into(),
                result: None,
            }),
            ..Default::default()
        };
        let rejected = ClientEvent::CommandRejected {
            request_id: Some("gpui-3".into()),
            reason: herdr_client::Error::UnsupportedMethod,
        };
        assert!(inbox.apply(rejected).is_none());
        assert!(matches!(
            inbox.take(LinkRequest::Activate, "gpui-3"),
            Some(Err(crate::Error::Client(
                herdr_client::Error::UnsupportedMethod
            )))
        ));
        inbox.resolving = Some(Pending {
            id: "gpui-4".into(),
            result: None,
        });
        let disconnected = ClientEvent::Disconnected {
            reason: "gone".into(),
        };
        assert!(inbox.apply(disconnected).is_some());
        assert!(matches!(
            inbox.take(LinkRequest::Resolve, "gpui-4"),
            Some(Err(crate::Error::NotConnected))
        ));
    }
}
