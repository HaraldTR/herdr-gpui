//! Which frame the terminal area paints, across the gap between projections.
//!
//! The daemon projects one surface per client, for the focused pane tree alone,
//! so changing space costs a round trip: the client drops its surface when it
//! fences the focus change, and the replacement arrives milliseconds later. The
//! same gap opens whenever a snapshot revision lands before the surface of that
//! revision. Painting nothing in those gaps flashes the empty background between
//! two pictures; a multiplexer redrawing a cell grid never blanks between them.
//! Keeping the last presented frame on screen until its replacement is ready
//! removes the flash without pretending the old frame is current state.
//!
//! Retained cells are presentation only. Hit testing, input routing, and IME
//! placement keep reading `LiveState::surface`, so a retained frame can never
//! aim a click or a keystroke at a pane the client has already left.

use crate::state::LiveState;
use herdr_client::{SurfaceImages, protocol::PaneSurfaceFrame};
use std::sync::Arc;

/// A frame with the pixels for the images it places.
#[derive(Clone)]
pub(crate) struct Picture {
    pub(crate) frame: Arc<PaneSurfaceFrame>,
    pub(crate) images: Arc<SurfaceImages>,
}

#[derive(Default)]
pub(crate) struct Presentation {
    presented: Option<Arc<PaneSurfaceFrame>>,
    /// The pixels the presented frame was shown with. A retained frame keeps
    /// them, since the connection's own set follows the newest surface.
    images: Arc<SurfaceImages>,
    #[cfg(feature = "integration-test")]
    pub(crate) probe: Probe,
}

impl Presentation {
    /// The frame to paint now, recorded as what the window shows: the ready
    /// surface when the client has one, and otherwise the frame last presented
    /// for as long as it can still stand for this window's content.
    pub(crate) fn frame(&mut self, live: &LiveState) -> Option<Arc<PaneSurfaceFrame>> {
        match live.surface.clone().filter(|_| live.surface_ready()) {
            Some(ready) => {
                self.presented = Some(ready);
                self.images = live.surface_images.clone();
            }
            None if !self.retainable(live) => self.clear(),
            None => {
                #[cfg(feature = "integration-test")]
                {
                    self.probe.retained += 1;
                }
            }
        }
        #[cfg(feature = "integration-test")]
        if self.presented.is_none() {
            self.probe.blank += 1;
        }
        self.presented.clone()
    }

    /// A frame from another boot, or from a connection that is no longer up,
    /// stands for nothing this window can still claim to be showing.
    fn retainable(&self, live: &LiveState) -> bool {
        let Some(presented) = &self.presented else {
            return false;
        };
        live.status.is_connected()
            && live
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.boot_id == presented.boot_id)
    }

    /// Forget the picture. Another connection's window is not this one's, so a
    /// reconnect, a detach, or a switch of endpoint starts from an empty area.
    pub(crate) fn clear(&mut self) {
        self.presented = None;
        self.images = Default::default();
    }

    /// The frame to paint now, as `frame` chooses it, with its images.
    pub(crate) fn picture(&mut self, live: &LiveState) -> Option<Picture> {
        let frame = self.frame(live)?;
        Some(Picture {
            frame,
            images: self.images.clone(),
        })
    }
}

/// Paint outcomes the native smoke driver reads to prove a space switch never
/// blanks the terminal area.
#[cfg(feature = "integration-test")]
#[derive(Clone, Copy, Debug, Default)]
pub struct Probe {
    /// Renders that painted no frame at all: an empty terminal area.
    pub blank: u64,
    /// Renders that repainted the presented frame while the next was in flight.
    pub retained: u64,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::state::ConnectionStatus;
    use herdr_client::protocol::{ClientShellSnapshot, FrameData};

    fn snapshot(boot: &str, revision: u64) -> Arc<ClientShellSnapshot> {
        let mut snapshot = crate::sidebar::layout_tests::snapshot(1);
        snapshot.boot_id = boot.into();
        snapshot.revision = revision;
        Arc::new(snapshot)
    }

    fn surface(boot: &str, revision: u64) -> Arc<PaneSurfaceFrame> {
        Arc::new(PaneSurfaceFrame {
            boot_id: boot.into(),
            projection_revision: revision,
            surface_revision: revision,
            frame: FrameData {
                cells: vec![],
                width: 0,
                height: 0,
                cursor: None,
                hyperlinks: vec![],
                graphics: vec![],
            },
            panes: vec![],
            splits: vec![],
            popup: None,
            graphics: Default::default(),
        })
    }

    /// `LiveState` keeps private fields, so the fixtures below are built by
    /// assignment rather than by struct update syntax.
    fn connected(boot: &str, revision: u64) -> LiveState {
        let mut live = LiveState::default();
        live.status = ConnectionStatus::Connected;
        live.snapshot = Some(snapshot(boot, revision));
        live.surface = Some(surface(boot, revision));
        live
    }

    #[test]
    fn a_pending_projection_keeps_the_frame_already_presented() {
        let mut presentation = Presentation::default();
        let live = connected("boot", 7);
        let first = live.surface.clone().unwrap();
        assert!(Arc::ptr_eq(&presentation.frame(&live).unwrap(), &first));

        // The focus fence drops the surface; the snapshot keeps its boot.
        let mut pending = connected("boot", 7);
        pending.surface = None;
        assert!(Arc::ptr_eq(&presentation.frame(&pending).unwrap(), &first));

        // A snapshot ahead of its surface is the same gap, not a new picture.
        let mut ahead = connected("boot", 7);
        ahead.snapshot = Some(snapshot("boot", 8));
        assert!(Arc::ptr_eq(&presentation.frame(&ahead).unwrap(), &first));

        let next = connected("boot", 8);
        let replacement = next.surface.clone().unwrap();
        assert!(Arc::ptr_eq(
            &presentation.frame(&next).unwrap(),
            &replacement
        ));
    }

    #[test]
    fn nothing_is_presented_for_another_boot_a_lost_connection_or_after_clearing() {
        let mut presentation = Presentation::default();
        assert!(presentation.frame(&connected("boot", 1)).is_some());
        let mut rebooted = connected("boot", 1);
        rebooted.snapshot = Some(snapshot("other-boot", 1));
        rebooted.surface = None;
        assert!(presentation.frame(&rebooted).is_none());

        assert!(presentation.frame(&connected("boot", 1)).is_some());
        let mut disconnected = connected("boot", 1);
        disconnected.surface = None;
        disconnected.status = ConnectionStatus::Disconnected;
        assert!(presentation.frame(&disconnected).is_none());

        assert!(presentation.frame(&connected("boot", 1)).is_some());
        presentation.clear();
        let mut gap = connected("boot", 1);
        gap.surface = None;
        assert!(presentation.frame(&gap).is_none());
    }
}
