//! Images a pane places through Herdr's graphics scene.
//!
//! The worker hands over each image's encoded bytes once; this module turns
//! them into textures off the UI thread, keeps a bounded set of them, and maps
//! each placement onto the same cell grid the text uses. Bytes are untrusted:
//! decoding is capped in pixels and memory, and textures are scaled down to a
//! size every GPU atlas accepts.

use crate::{Error, Result};
use gpui::{App, Bounds, Pixels, RenderImage, Window, point, px, size};
use herdr_client::{
    MAX_IMAGE_SIDE, SurfaceImage, SurfaceImages,
    protocol::{
        SurfaceGraphicsAssetKey, SurfaceGraphicsFormat, SurfaceGraphicsPlacement,
        SurfaceGraphicsSource, SurfaceGraphicsTarget,
    },
};
use image::{Frame, ImageFormat, ImageReader, Limits, RgbaImage, imageops::FilterType};
use std::{
    collections::HashMap,
    io::Cursor,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

/// Decoded bytes one PNG may allocate.
const MAX_DECODE_BYTES: u64 = 128 * 1024 * 1024;
/// Textures are scaled to fit this side, which every GPUI atlas accepts.
const MAX_TEXTURE_SIDE: u32 = 4_096;
/// Decoded texture bytes kept per window.
const MAX_TEXTURE_BYTES: usize = 256 * 1024 * 1024;
/// Textures, failures, and decodes in flight kept per window.
const MAX_ENTRIES: usize = 128;
/// Decodes running at once; further images wait for a later paint.
const MAX_DECODES: usize = 2;
/// A texture no paint used for this long is released.
const IDLE: Duration = Duration::from_secs(60);

/// Kitty draws `z < 0` below text and anything else above it.
pub(crate) fn below_text(z: i32) -> bool {
    z < 0
}

/// Which frame's placements to paint: the main grid's or one popup's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageTarget<'a> {
    Main,
    Popup(&'a str),
}

impl ImageTarget<'_> {
    fn places(self, source: &SurfaceGraphicsSource) -> bool {
        match (self, source) {
            (
                Self::Main,
                SurfaceGraphicsSource::Terminal {
                    target: SurfaceGraphicsTarget::Pane { .. },
                    ..
                }
                | SurfaceGraphicsSource::PaneLayer { .. },
            ) => true,
            (
                Self::Popup(id),
                SurfaceGraphicsSource::Terminal {
                    target: SurfaceGraphicsTarget::Popup { terminal_id },
                    ..
                },
            ) => id == terminal_id,
            _ => false,
        }
    }
}

/// The placements one frame paints, with the pixels they refer to.
#[derive(Clone, Copy)]
pub(crate) struct PlacedImages<'a> {
    pub(crate) placements: &'a [SurfaceGraphicsPlacement],
    pub(crate) images: &'a SurfaceImages,
    pub(crate) target: ImageTarget<'a>,
}

impl<'a> PlacedImages<'a> {
    fn resolved(self) -> impl Iterator<Item = (&'a SurfaceGraphicsPlacement, &'a SurfaceImage)> {
        self.placements
            .iter()
            .filter(move |placement| self.target.places(&placement.asset.source))
            .filter_map(move |placement| Some((placement, self.images.get(&placement.asset)?)))
    }
}

/// Where a placement lands: the visible part, and the whole image positioned
/// so its source rectangle fills that part.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ImageGeometry {
    pub(crate) visible: Bounds<Pixels>,
    pub(crate) image: Bounds<Pixels>,
}

/// Maps a placement onto the grid at `origin`, as Herdr's own client does:
/// the leading pixel offsets shrink the image inside its cells, and the
/// source rectangle (zero meaning the whole image) is stretched over the
/// rest. Offsets arrive in the whole-pixel cell size this client reported.
pub(crate) fn geometry(
    placement: &SurfaceGraphicsPlacement,
    origin: gpui::Point<Pixels>,
    cell: (f32, f32),
    grid: Bounds<Pixels>,
) -> Option<ImageGeometry> {
    let key = &placement.asset;
    let (cell_width, cell_height) = cell;
    let span = |start: u16, cells: u32, offset: u32, cell: f32| {
        let reported = cell.round().max(1.);
        let length = cells as f32 * cell;
        let offset = (offset as f32).min(cells as f32 * reported - 1.).max(0.) * cell / reported;
        (f32::from(start) * cell + offset, length - offset)
    };
    let source = |start: u32, length: u32, image: u32| {
        let length = if length == 0 { image } else { length };
        let length = length.min(image.checked_sub(start)?);
        (length > 0).then_some((start as f32, length as f32))
    };
    let (x, width) = span(placement.x, placement.cols, placement.x_offset, cell_width);
    let (y, height) = span(placement.y, placement.rows, placement.y_offset, cell_height);
    let (source_x, source_width) =
        source(placement.source_x, placement.source_width, key.image_width)?;
    let (source_y, source_height) = source(
        placement.source_y,
        placement.source_height,
        key.image_height,
    )?;
    if width <= 0. || height <= 0. {
        return None;
    }
    let target = Bounds::new(origin + point(px(x), px(y)), size(px(width), px(height)));
    let visible = target.intersect(&grid);
    if visible.size.width <= Pixels::ZERO || visible.size.height <= Pixels::ZERO {
        return None;
    }
    let (scale_x, scale_y) = (width / source_width, height / source_height);
    let image = Bounds::new(
        target.origin - point(px(source_x * scale_x), px(source_y * scale_y)),
        size(
            px(key.image_width as f32 * scale_x),
            px(key.image_height as f32 * scale_y),
        ),
    );
    Some(ImageGeometry { visible, image })
}

/// Decodes `data` into the BGRA texture GPUI uploads, scaled to fit
/// `MAX_TEXTURE_SIDE`. The placement keeps mapping onto the key's size.
pub(crate) fn decode(key: &SurfaceGraphicsAssetKey, data: &[u8]) -> Result<RgbaImage> {
    if !herdr_client::valid_asset(key, data) {
        return Err(Error::PaneImageLimit);
    }
    let (width, height) = (key.image_width, key.image_height);
    let pixels = match key.format {
        SurfaceGraphicsFormat::Rgb => RgbaImage::from_raw(
            width,
            height,
            data.chunks_exact(3)
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 0xff])
                .collect(),
        ),
        SurfaceGraphicsFormat::Rgba => RgbaImage::from_raw(width, height, data.to_vec()),
        SurfaceGraphicsFormat::Png => {
            let mut reader = ImageReader::with_format(Cursor::new(data), ImageFormat::Png);
            let mut limits = Limits::default();
            limits.max_image_width = Some(MAX_IMAGE_SIDE);
            limits.max_image_height = Some(MAX_IMAGE_SIDE);
            limits.max_alloc = Some(MAX_DECODE_BYTES);
            reader.limits(limits);
            Some(
                reader
                    .decode()
                    .map_err(Error::PaneImageDecode)?
                    .into_rgba8(),
            )
        }
    };
    let mut pixels = pixels.ok_or(Error::PaneImageLimit)?;
    let (width, height) = pixels.dimensions();
    if width > MAX_TEXTURE_SIDE || height > MAX_TEXTURE_SIDE {
        let scale = f64::from(MAX_TEXTURE_SIDE) / f64::from(width.max(height));
        let fit = |side: u32| ((f64::from(side) * scale).round() as u32).clamp(1, MAX_TEXTURE_SIDE);
        pixels = image::imageops::resize(&pixels, fit(width), fit(height), FilterType::Triangle);
    }
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(pixels)
}

enum State {
    Decoding,
    Ready(Arc<RenderImage>),
    Failed,
}

struct Entry {
    state: State,
    bytes: usize,
    used: Instant,
    tick: u64,
}

/// What a paint should do about one image.
pub(crate) enum Lookup {
    Ready(Arc<RenderImage>),
    /// Start decoding it; the entry now counts as in flight.
    Decode,
    /// Decoding, failed, or no room to start yet.
    Wait,
}

type Finished = Arc<Mutex<Vec<(u64, Result<RgbaImage>)>>>;

/// Textures for one window, keyed by the serial of the bytes they came from,
/// so a key delivered again (another connection or boot) never reuses a stale
/// texture. Evicted textures leave GPUI's atlas at the next paint.
#[derive(Default)]
pub(crate) struct ImageCache {
    entries: HashMap<u64, Entry>,
    bytes: usize,
    decoding: usize,
    tick: u64,
    finished: Finished,
}

impl ImageCache {
    /// Starts a paint: applies finished decodes and returns textures to
    /// release from the atlas.
    pub(crate) fn begin(&mut self, now: Instant) -> Vec<Arc<RenderImage>> {
        self.tick += 1;
        let finished =
            std::mem::take(&mut *self.finished.lock().unwrap_or_else(PoisonError::into_inner));
        for (serial, result) in finished {
            self.finish(serial, result.map(|pixels| Arc::new(texture(pixels))));
        }
        self.evict(now)
    }

    pub(crate) fn lookup(&mut self, serial: u64, now: Instant) -> Lookup {
        if let Some(entry) = self.entries.get_mut(&serial) {
            entry.used = now;
            entry.tick = self.tick;
            return match &entry.state {
                State::Ready(texture) => Lookup::Ready(texture.clone()),
                State::Decoding | State::Failed => Lookup::Wait,
            };
        }
        if self.decoding >= MAX_DECODES || self.entries.len() >= MAX_ENTRIES {
            return Lookup::Wait;
        }
        self.decoding += 1;
        self.entries.insert(
            serial,
            Entry {
                state: State::Decoding,
                bytes: 0,
                used: now,
                tick: self.tick,
            },
        );
        Lookup::Decode
    }

    pub(crate) fn finish(&mut self, serial: u64, result: Result<Arc<RenderImage>>) {
        let Some(entry) = self
            .entries
            .get_mut(&serial)
            .filter(|entry| matches!(entry.state, State::Decoding))
        else {
            return;
        };
        self.decoding -= 1;
        match result {
            Ok(texture) => {
                let dimensions = texture.size(0);
                entry.bytes = (dimensions.width.0.max(0) as usize)
                    .saturating_mul(dimensions.height.0.max(0) as usize)
                    .saturating_mul(4);
                self.bytes += entry.bytes;
                entry.state = State::Ready(texture);
            }
            Err(error) => {
                tracing::debug!(category = "pane_images", %error, "image not shown");
                entry.state = State::Failed;
            }
        }
    }

    /// Drops idle entries, then the least recently used ones while over
    /// budget. Decodes in flight and anything this paint used stay.
    fn evict(&mut self, now: Instant) -> Vec<Arc<RenderImage>> {
        let mut released = Vec::new();
        let mut remove = |entries: &mut HashMap<u64, Entry>, bytes: &mut usize, serial| {
            if let Some(entry) = entries.remove(&serial) {
                *bytes -= entry.bytes;
                if let State::Ready(texture) = entry.state {
                    released.push(texture);
                }
            }
        };
        let idle: Vec<u64> = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                !matches!(entry.state, State::Decoding) && now.duration_since(entry.used) >= IDLE
            })
            .map(|(serial, _)| *serial)
            .collect();
        for serial in idle {
            remove(&mut self.entries, &mut self.bytes, serial);
        }
        while self.bytes > MAX_TEXTURE_BYTES || self.entries.len() > MAX_ENTRIES {
            let Some(serial) = self
                .entries
                .iter()
                .filter(|(_, entry)| {
                    !matches!(entry.state, State::Decoding) && entry.tick < self.tick
                })
                .min_by_key(|(_, entry)| entry.tick)
                .map(|(serial, _)| *serial)
            else {
                break;
            };
            remove(&mut self.entries, &mut self.bytes, serial);
        }
        released
    }

    /// Starts a paint and drops the textures it evicts from the atlas. A
    /// paint with placements does this as it prepares them; one without must
    /// call it, or the textures of images that went away stay resident.
    pub(crate) fn release_idle(&mut self, window: &mut Window) {
        for texture in self.begin(Instant::now()) {
            let _ = window.drop_image(texture);
        }
    }

    /// Resolves the textures and geometry for one frame's placements,
    /// starting background decodes for images not yet decoded. Placements
    /// whose texture is not ready are skipped this paint; a finished decode
    /// refreshes the window.
    pub(crate) fn prepare(
        &mut self,
        images: PlacedImages<'_>,
        origin: gpui::Point<Pixels>,
        cell: (f32, f32),
        grid: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<(i32, ImageGeometry, Arc<RenderImage>)> {
        self.release_idle(window);
        let now = Instant::now();
        let mut placed = Vec::new();
        for (placement, image) in images.resolved() {
            let Some(geometry) = geometry(placement, origin, cell, grid) else {
                continue;
            };
            match self.lookup(image.serial(), now) {
                Lookup::Ready(texture) => placed.push((placement.z, geometry, texture)),
                Lookup::Decode => self.spawn(image, &placement.asset, window, cx),
                Lookup::Wait => {}
            }
        }
        // Lower z paints first; equal z keeps the scene's order.
        placed.sort_by_key(|(z, ..)| *z);
        placed
    }

    fn spawn(
        &self,
        image: &SurfaceImage,
        key: &SurfaceGraphicsAssetKey,
        window: &Window,
        cx: &App,
    ) {
        let (serial, data, key) = (image.serial(), image.data().clone(), key.clone());
        let finished = self.finished.clone();
        let decoded = cx
            .background_executor()
            .spawn(async move { decode(&key, &data) });
        window
            .spawn(cx, async move |cx| {
                let result = decoded.await;
                finished
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((serial, result));
                let _ = cx.update(|window, _| window.refresh());
            })
            .detach();
    }
}

fn texture(pixels: RgbaImage) -> RenderImage {
    RenderImage::new(vec![Frame::new(pixels)])
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use herdr_client::protocol::SurfaceGraphicsTarget;

    fn key(format: SurfaceGraphicsFormat, width: u32, height: u32) -> SurfaceGraphicsAssetKey {
        let channels = match format {
            SurfaceGraphicsFormat::Rgb => 3,
            _ => 4,
        };
        SurfaceGraphicsAssetKey {
            source: SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Pane {
                    pane_id: "p1".into(),
                },
                image_id: 1,
            },
            image_width: width,
            image_height: height,
            format,
            data_len: u64::from(width * height * channels),
            data_fingerprint: 0,
        }
    }

    fn placement(key: SurfaceGraphicsAssetKey) -> SurfaceGraphicsPlacement {
        SurfaceGraphicsPlacement {
            asset: key,
            logical_placement_id: 1,
            x: 2,
            y: 1,
            cols: 4,
            rows: 2,
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

    fn grid(width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(100.), px(50.)), size(px(width), px(height)))
    }

    #[test]
    fn raw_pixels_become_bgra_with_opaque_rgb() {
        let rgb = decode(&key(SurfaceGraphicsFormat::Rgb, 1, 1), &[1, 2, 3]).unwrap();
        assert_eq!(rgb.as_raw(), &[3, 2, 1, 255]);
        let rgba = decode(&key(SurfaceGraphicsFormat::Rgba, 1, 1), &[1, 2, 3, 4]).unwrap();
        assert_eq!(rgba.as_raw(), &[3, 2, 1, 4]);
        // Bytes that disagree with their key are refused, never reinterpreted.
        assert!(matches!(
            decode(&key(SurfaceGraphicsFormat::Rgba, 1, 1), &[1, 2, 3]),
            Err(Error::PaneImageLimit)
        ));
    }

    #[test]
    fn png_is_decoded_under_limits_and_garbage_is_an_error() {
        let mut png = Vec::new();
        RgbaImage::from_raw(2, 1, vec![10, 20, 30, 40, 50, 60, 70, 80])
            .unwrap()
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        let mut asset = key(SurfaceGraphicsFormat::Png, 2, 1);
        asset.data_len = png.len() as u64;
        let decoded = decode(&asset, &png).unwrap();
        assert_eq!(decoded.as_raw(), &[30, 20, 10, 40, 70, 60, 50, 80]);
        asset.data_len = 4;
        assert!(matches!(
            decode(&asset, b"nope"),
            Err(Error::PaneImageDecode(_))
        ));
    }

    #[test]
    fn oversized_images_are_scaled_to_the_texture_limit() {
        let (width, height) = (MAX_TEXTURE_SIDE * 2, 2);
        let asset = key(SurfaceGraphicsFormat::Rgba, width, height);
        let data = vec![255; asset.data_len as usize];
        let decoded = decode(&asset, &data).unwrap();
        assert_eq!(decoded.dimensions(), (MAX_TEXTURE_SIDE, 1));
    }

    #[test]
    fn placements_cover_their_cells_on_the_text_grid() {
        let placed = placement(key(SurfaceGraphicsFormat::Rgba, 40, 20));
        let origin = point(px(100.), px(50.));
        let found = geometry(&placed, origin, (10., 20.), grid(1000., 1000.)).unwrap();
        let expected = Bounds::new(point(px(120.), px(70.)), size(px(40.), px(40.)));
        assert_eq!(found.visible, expected);
        assert_eq!(found.image, expected);
    }

    #[test]
    fn source_rectangles_and_offsets_map_like_herdr() {
        let mut placed = placement(key(SurfaceGraphicsFormat::Rgba, 40, 20));
        // Bottom half of the image, shrunk by a 4px leading offset at a
        // fractional 7.5px cell the client reported as 8px.
        placed.source_y = 10;
        placed.source_height = 10;
        placed.x_offset = 4;
        let origin = point(px(100.), px(50.));
        let found = geometry(&placed, origin, (7.5, 20.), grid(1000., 1000.)).unwrap();
        let left = 100. + 2. * 7.5 + 4. * 7.5 / 8.;
        let width = 4. * 7.5 - 4. * 7.5 / 8.;
        assert_eq!(
            found.visible,
            Bounds::new(point(px(left), px(70.)), size(px(width), px(40.)))
        );
        // Ten source rows fill forty pixels, so the whole image is twice as
        // tall and starts forty pixels above.
        assert_eq!(
            found.image,
            Bounds::new(point(px(left), px(30.)), size(px(width), px(80.)))
        );
    }

    #[test]
    fn placements_are_clipped_to_their_grid_and_bad_sources_skipped() {
        let mut placed = placement(key(SurfaceGraphicsFormat::Rgba, 40, 20));
        placed.cols = 1_000_000;
        let origin = point(px(100.), px(50.));
        let found = geometry(&placed, origin, (10., 20.), grid(100., 100.)).unwrap();
        assert_eq!(found.visible.right(), px(200.));
        placed.cols = 4;
        placed.x = 50;
        assert!(geometry(&placed, origin, (10., 20.), grid(100., 100.)).is_none());
        let mut outside = placement(key(SurfaceGraphicsFormat::Rgba, 40, 20));
        outside.source_x = 40;
        assert!(geometry(&outside, origin, (10., 20.), grid(1000., 1000.)).is_none());
        outside.source_x = 0;
        outside.rows = 0;
        assert!(geometry(&outside, origin, (10., 20.), grid(1000., 1000.)).is_none());
    }

    #[test]
    fn targets_select_main_or_their_own_popup() {
        let pane = key(SurfaceGraphicsFormat::Rgba, 1, 1);
        let mut popup = pane.clone();
        popup.source = SurfaceGraphicsSource::Terminal {
            target: SurfaceGraphicsTarget::Popup {
                terminal_id: "t".into(),
            },
            image_id: 1,
        };
        assert!(ImageTarget::Main.places(&pane.source));
        assert!(!ImageTarget::Main.places(&popup.source));
        assert!(ImageTarget::Popup("t").places(&popup.source));
        assert!(!ImageTarget::Popup("other").places(&popup.source));
        assert!(!ImageTarget::Popup("t").places(&pane.source));
    }

    fn ready(cache: &mut ImageCache, serial: u64, side: u32, now: Instant) {
        assert!(matches!(cache.lookup(serial, now), Lookup::Decode));
        cache.finish(serial, Ok(Arc::new(texture(RgbaImage::new(side, side)))));
    }

    #[test]
    fn decodes_are_bounded_and_failures_are_not_retried() {
        let mut cache = ImageCache::default();
        let now = Instant::now();
        cache.begin(now);
        for serial in 0..MAX_DECODES as u64 {
            assert!(matches!(cache.lookup(serial, now), Lookup::Decode));
        }
        assert!(matches!(cache.lookup(99, now), Lookup::Wait));
        assert!(matches!(cache.lookup(0, now), Lookup::Wait));
        cache.finish(0, Err(Error::PaneImageLimit));
        cache.finish(1, Ok(Arc::new(texture(RgbaImage::new(1, 1)))));
        assert!(matches!(cache.lookup(0, now), Lookup::Wait));
        assert!(matches!(cache.lookup(1, now), Lookup::Ready(_)));
        // A late result for an unknown serial is ignored.
        cache.finish(7, Ok(Arc::new(texture(RgbaImage::new(1, 1)))));
        assert!(matches!(cache.lookup(99, now), Lookup::Decode));
    }

    #[test]
    fn finished_background_decodes_land_on_the_next_paint() {
        let mut cache = ImageCache::default();
        let now = Instant::now();
        assert!(matches!(cache.lookup(1, now), Lookup::Decode));
        cache
            .finished
            .lock()
            .unwrap()
            .push((1, Ok(RgbaImage::new(2, 2))));
        assert!(cache.begin(now).is_empty());
        assert!(matches!(cache.lookup(1, now), Lookup::Ready(_)));
        assert_eq!(cache.bytes, 16);
    }

    #[test]
    fn textures_are_released_when_idle_or_over_budget() {
        let mut cache = ImageCache::default();
        let start = Instant::now();
        cache.begin(start);
        ready(&mut cache, 1, 1, start);
        ready(&mut cache, 2, 1, start);
        cache.begin(start + IDLE / 2);
        assert!(matches!(
            cache.lookup(2, start + IDLE / 2),
            Lookup::Ready(_)
        ));
        let released = cache.begin(start + IDLE);
        assert_eq!(released.len(), 1, "only the idle texture leaves");
        assert!(cache.entries.contains_key(&2) && !cache.entries.contains_key(&1));
        assert_eq!(cache.bytes, 4);

        // Over budget, the least recently used goes first; this paint's stay.
        // Each texture is charged a quarter of the budget without allocating it.
        let mut cache = ImageCache::default();
        let quarter = MAX_TEXTURE_BYTES / 4;
        for serial in 0..5 {
            cache.begin(start);
            ready(&mut cache, serial, 1, start);
            let entry = cache.entries.get_mut(&serial).unwrap();
            cache.bytes += quarter - entry.bytes;
            entry.bytes = quarter;
        }
        let released = cache.begin(start);
        assert_eq!(released.len(), 1);
        assert!(!cache.entries.contains_key(&0));
        assert!(cache.bytes <= MAX_TEXTURE_BYTES);
    }
}
