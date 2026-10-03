//! Pixels for the images a pane surface places.
//!
//! Herdr sends an image's bytes once, in the first surface that places it, and
//! afterwards only references it by key until no placement or retained entry
//! names it. A consumer that coalesces surfaces would miss those bytes, so the
//! worker, which sees every surface in order, keeps them here and publishes
//! the set alongside the surfaces. Bytes are untrusted: each asset must match
//! its key exactly, and counts and total bytes are bounded.

use crate::protocol::{
    PaneSurfaceFrame, SurfaceGraphicsAsset, SurfaceGraphicsAssetKey, SurfaceGraphicsFormat,
    SurfaceGraphicsScene,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Herdr's own cap on placements in one scene.
pub const MAX_PLACEMENTS: usize = 4_096;
/// Kitty and Ghostty refuse images wider or taller than this.
pub const MAX_IMAGE_SIDE: u32 = 10_000;
/// Distinct images kept for one connection.
pub const MAX_IMAGES: usize = 256;
/// Encoded bytes kept for one connection. Herdr keeps at most 64 MiB of
/// off-screen images; the rest covers what is on screen.
pub const MAX_IMAGE_BYTES: usize = 256 * 1024 * 1024;

/// Unique for the life of the process, so a cache can tell two deliveries of
/// the same key apart, even from different connections or boots.
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// One image's encoded bytes, exactly as the key describes them.
#[derive(Clone, Debug)]
pub struct SurfaceImage {
    serial: u64,
    data: Arc<[u8]>,
}

impl SurfaceImage {
    fn new(data: Vec<u8>) -> Self {
        Self {
            serial: NEXT_SERIAL.fetch_add(1, Ordering::Relaxed),
            data: Arc::from(data),
        }
    }

    pub fn serial(&self) -> u64 {
        self.serial
    }

    pub fn data(&self) -> &Arc<[u8]> {
        &self.data
    }
}

/// The images a connection's latest surfaces may place, by asset key.
#[derive(Clone, Debug, Default)]
pub struct SurfaceImages {
    images: HashMap<SurfaceGraphicsAssetKey, SurfaceImage>,
}

impl SurfaceImages {
    pub fn get(&self, key: &SurfaceGraphicsAssetKey) -> Option<&SurfaceImage> {
        self.images.get(key)
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

/// Collects valid assets, skipping any whose bytes disagree with their key.
impl FromIterator<SurfaceGraphicsAsset> for SurfaceImages {
    fn from_iter<I: IntoIterator<Item = SurfaceGraphicsAsset>>(assets: I) -> Self {
        let images = assets
            .into_iter()
            .filter(|asset| valid_asset(&asset.key, &asset.data))
            .map(|asset| (asset.key, SurfaceImage::new(asset.data)))
            .collect();
        Self { images }
    }
}

/// Whether `data` is exactly what `key` promises.
pub fn valid_asset(key: &SurfaceGraphicsAssetKey, data: &[u8]) -> bool {
    let sides = 1..=MAX_IMAGE_SIDE;
    if data.is_empty()
        || u64::try_from(data.len()).ok() != Some(key.data_len)
        || !sides.contains(&key.image_width)
        || !sides.contains(&key.image_height)
    {
        return false;
    }
    let pixels = u64::from(key.image_width) * u64::from(key.image_height);
    match key.format {
        SurfaceGraphicsFormat::Rgb => pixels.checked_mul(3) == Some(key.data_len),
        SurfaceGraphicsFormat::Rgba => pixels.checked_mul(4) == Some(key.data_len),
        // Decoded later, under its own limits.
        SurfaceGraphicsFormat::Png => true,
    }
}

fn referenced(scene: &SurfaceGraphicsScene) -> HashSet<SurfaceGraphicsAssetKey> {
    scene
        .placements
        .iter()
        .map(|placement| &placement.asset)
        .chain(&scene.retained_assets)
        .cloned()
        .collect()
}

/// The worker's image bytes for one connection.
#[derive(Default)]
pub(crate) struct ImageStore {
    images: HashMap<SurfaceGraphicsAssetKey, SurfaceImage>,
    bytes: usize,
    /// Keys the newest received surface references.
    latest: HashSet<SurfaceGraphicsAssetKey>,
    /// Keys the newest emitted surface references; its consumer may still
    /// paint them while a newer surface waits for its snapshot.
    shown: HashSet<SurfaceGraphicsAssetKey>,
}

impl ImageStore {
    /// Moves a received surface's asset bytes into the store, leaving the
    /// surface with metadata only. Returns whether the published set changed.
    pub(crate) fn receive(&mut self, surface: &mut PaneSurfaceFrame) -> bool {
        let scene = &mut surface.graphics;
        if scene.placements.len() > MAX_PLACEMENTS {
            tracing::debug!(
                category = "surface_images",
                count = scene.placements.len(),
                "dropped placements over the limit"
            );
            scene.placements.truncate(MAX_PLACEMENTS);
        }
        let assets = std::mem::take(&mut scene.assets);
        self.latest = referenced(scene);
        let mut changed = self.prune();
        for asset in assets {
            if self.images.contains_key(&asset.key) || !self.latest.contains(&asset.key) {
                continue;
            }
            if !valid_asset(&asset.key, &asset.data) {
                tracing::debug!(category = "surface_images", "dropped an invalid image");
                continue;
            }
            if self.images.len() >= MAX_IMAGES
                || asset.data.len() > MAX_IMAGE_BYTES.saturating_sub(self.bytes)
            {
                tracing::debug!(category = "surface_images", "dropped an image over budget");
                continue;
            }
            self.bytes += asset.data.len();
            self.images.insert(asset.key, SurfaceImage::new(asset.data));
            changed = true;
        }
        changed
    }

    /// Records that the newest received surface was emitted. Returns whether
    /// the published set changed.
    pub(crate) fn show(&mut self) -> bool {
        self.shown.clone_from(&self.latest);
        self.prune()
    }

    pub(crate) fn published(&self) -> Arc<SurfaceImages> {
        Arc::new(SurfaceImages {
            images: self.images.clone(),
        })
    }

    fn prune(&mut self) -> bool {
        let before = self.images.len();
        let (latest, shown) = (&self.latest, &self.shown);
        let mut freed = 0;
        self.images.retain(|key, image| {
            let keep = latest.contains(key) || shown.contains(key);
            if !keep {
                freed += image.data.len();
            }
            keep
        });
        self.bytes -= freed;
        self.images.len() != before
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::protocol::{
        FrameData, SurfaceGraphicsPlacement, SurfaceGraphicsSource, SurfaceGraphicsTarget,
    };

    fn key(image_id: u32, format: SurfaceGraphicsFormat) -> SurfaceGraphicsAssetKey {
        let data_len = match format {
            SurfaceGraphicsFormat::Rgb => 2 * 2 * 3,
            SurfaceGraphicsFormat::Rgba => 2 * 2 * 4,
            SurfaceGraphicsFormat::Png => 8,
        };
        SurfaceGraphicsAssetKey {
            source: SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Pane {
                    pane_id: "p1".into(),
                },
                image_id,
            },
            image_width: 2,
            image_height: 2,
            format,
            data_len,
            data_fingerprint: u64::from(image_id),
        }
    }

    fn asset(key: &SurfaceGraphicsAssetKey) -> SurfaceGraphicsAsset {
        SurfaceGraphicsAsset {
            key: key.clone(),
            data: vec![7; key.data_len as usize],
        }
    }

    fn placement(key: &SurfaceGraphicsAssetKey) -> SurfaceGraphicsPlacement {
        SurfaceGraphicsPlacement {
            asset: key.clone(),
            logical_placement_id: 1,
            x: 0,
            y: 0,
            cols: 2,
            rows: 1,
            source_x: 0,
            source_y: 0,
            source_width: 0,
            source_height: 0,
            x_offset: 0,
            y_offset: 0,
            z: 0,
            scrollback_offset: 0,
        }
    }

    fn surface(scene: SurfaceGraphicsScene) -> PaneSurfaceFrame {
        PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 1,
            surface_revision: 1,
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
            graphics: scene,
        }
    }

    fn placed(keys: &[&SurfaceGraphicsAssetKey], with_bytes: bool) -> PaneSurfaceFrame {
        surface(SurfaceGraphicsScene {
            assets: if with_bytes {
                keys.iter().map(|key| asset(key)).collect()
            } else {
                vec![]
            },
            placements: keys.iter().map(|key| placement(key)).collect(),
            retained_assets: vec![],
        })
    }

    #[test]
    fn assets_must_match_their_key_exactly() {
        let rgba = key(1, SurfaceGraphicsFormat::Rgba);
        assert!(valid_asset(&rgba, &[0; 16]));
        assert!(!valid_asset(&rgba, &[0; 15]));
        assert!(!valid_asset(&rgba, &[]));
        let mut lying = rgba.clone();
        lying.data_len = 12;
        assert!(!valid_asset(&lying, &[0; 12]));
        let rgb = key(2, SurfaceGraphicsFormat::Rgb);
        assert!(valid_asset(&rgb, &[0; 12]));
        let png = key(3, SurfaceGraphicsFormat::Png);
        assert!(valid_asset(&png, &[0; 8]));
        let mut wide = png.clone();
        wide.image_width = MAX_IMAGE_SIDE + 1;
        assert!(!valid_asset(&wide, &[0; 8]));
        let mut empty = png;
        empty.image_height = 0;
        assert!(!valid_asset(&empty, &[0; 8]));
        // Overflowing pixel counts are rejected rather than wrapped.
        let mut huge = key(4, SurfaceGraphicsFormat::Rgba);
        huge.image_width = MAX_IMAGE_SIDE;
        huge.image_height = MAX_IMAGE_SIDE;
        assert!(!valid_asset(&huge, &[0; 16]));
    }

    #[test]
    fn bytes_outlive_the_surface_that_carried_them() {
        let mut store = ImageStore::default();
        let a = key(1, SurfaceGraphicsFormat::Rgba);
        let mut first = placed(&[&a], true);
        assert!(store.receive(&mut first));
        assert!(
            first.graphics.assets.is_empty(),
            "bytes move out of the surface"
        );
        let serial = store.published().get(&a).unwrap().serial();
        // Later surfaces only name the key.
        let mut next = placed(&[&a], false);
        assert!(!store.receive(&mut next));
        assert!(!store.show());
        assert_eq!(store.published().get(&a).unwrap().serial(), serial);
        // A resend of a key already held changes nothing.
        let mut resent = placed(&[&a], true);
        assert!(!store.receive(&mut resent));
        assert_eq!(store.published().get(&a).unwrap().serial(), serial);
    }

    #[test]
    fn unreferenced_and_invalid_assets_are_dropped() {
        let mut store = ImageStore::default();
        let (a, b) = (
            key(1, SurfaceGraphicsFormat::Rgba),
            key(2, SurfaceGraphicsFormat::Rgba),
        );
        let mut frame = placed(&[&a], true);
        frame.graphics.assets.push(asset(&b));
        let mut bad = asset(&a);
        bad.data.pop();
        frame.graphics.assets.insert(0, bad);
        store.receive(&mut frame);
        let images = store.published();
        assert_eq!(images.len(), 1);
        assert_eq!(images.get(&a).unwrap().data().len(), 16);
        assert!(images.get(&b).is_none());
    }

    #[test]
    fn retained_keys_keep_their_bytes_until_released() {
        let mut store = ImageStore::default();
        let a = key(1, SurfaceGraphicsFormat::Rgba);
        store.receive(&mut placed(&[&a], true));
        store.show();
        let mut offscreen = surface(SurfaceGraphicsScene {
            retained_assets: vec![a.clone()],
            ..Default::default()
        });
        assert!(!store.receive(&mut offscreen));
        assert!(!store.show());
        assert!(store.published().get(&a).is_some());
        assert!(!store.receive(&mut surface(SurfaceGraphicsScene::default())));
        assert!(store.show());
        assert!(store.published().is_empty());
        assert_eq!(store.bytes, 0);
    }

    #[test]
    fn a_shown_surface_keeps_its_images_until_its_successor_is_shown() {
        let mut store = ImageStore::default();
        let (a, b) = (
            key(1, SurfaceGraphicsFormat::Rgba),
            key(2, SurfaceGraphicsFormat::Rgba),
        );
        store.receive(&mut placed(&[&a], true));
        store.show();
        // A surface waiting for its snapshot replaces `a` with `b`; whatever
        // is on screen still places `a`.
        assert!(store.receive(&mut placed(&[&b], true)));
        let images = store.published();
        assert!(images.get(&a).is_some() && images.get(&b).is_some());
        assert!(store.show());
        let images = store.published();
        assert!(images.get(&a).is_none() && images.get(&b).is_some());
    }

    #[test]
    fn counts_and_bytes_are_bounded() {
        let mut store = ImageStore::default();
        let keys: Vec<_> = (0..MAX_IMAGES as u32 + 4)
            .map(|id| key(id, SurfaceGraphicsFormat::Rgba))
            .collect();
        let mut frame = placed(&keys.iter().collect::<Vec<_>>(), true);
        store.receive(&mut frame);
        assert_eq!(store.published().len(), MAX_IMAGES);

        // Charge the store as if it already held nearly its whole budget.
        let mut store = ImageStore {
            bytes: MAX_IMAGE_BYTES - 20,
            ..Default::default()
        };
        let (a, b) = (
            key(1, SurfaceGraphicsFormat::Rgba),
            key(2, SurfaceGraphicsFormat::Rgba),
        );
        store.receive(&mut placed(&[&a, &b], true));
        assert_eq!(store.published().len(), 1);
        assert_eq!(store.bytes, MAX_IMAGE_BYTES - 4);

        let a = key(1, SurfaceGraphicsFormat::Rgba);
        let mut frame = placed(&[&a], false);
        frame.graphics.placements = vec![placement(&a); MAX_PLACEMENTS + 1];
        store.receive(&mut frame);
        assert_eq!(frame.graphics.placements.len(), MAX_PLACEMENTS);
    }

    #[test]
    fn collected_images_keep_only_valid_assets() {
        let (a, b) = (
            key(1, SurfaceGraphicsFormat::Rgba),
            key(2, SurfaceGraphicsFormat::Rgb),
        );
        let mut bad = asset(&b);
        bad.data.push(0);
        let images: SurfaceImages = [asset(&a), bad].into_iter().collect();
        assert_eq!(images.len(), 1);
        assert!(images.get(&a).is_some());
    }

    #[test]
    fn serials_tell_deliveries_of_one_key_apart() {
        let a = key(1, SurfaceGraphicsFormat::Rgba);
        let mut first = ImageStore::default();
        first.receive(&mut placed(&[&a], true));
        let mut second = ImageStore::default();
        second.receive(&mut placed(&[&a], true));
        assert_ne!(
            first.published().get(&a).unwrap().serial(),
            second.published().get(&a).unwrap().serial()
        );
    }
}
