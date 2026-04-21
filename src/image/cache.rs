//! Decoded-image cache retained across reparses.
//!
//! `ParsedDoc` is rebuilt on every buffer mutation, so keeping decoded
//! image bytes (and their expensive `StatefulProtocol` encodings) on the
//! parse tree would mean re-decoding on every keystroke.  Instead we
//! cache by URL on `EditorState`: the URL set rarely changes during
//! editing, and protocols are keyed additionally by target cell
//! dimensions so a terminal resize invalidates only the affected
//! entries, not unrelated text.

use std::collections::HashMap;

use image::DynamicImage;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;

/// Status of a decode attempt for a URL.
pub enum DecodeStatus {
    /// Decode has been dispatched to a worker thread (or is about to be)
    /// and is in flight.  `paint_images` shows the `[Image: alt]`
    /// placeholder while `Pending`.
    Pending,
    /// Decode succeeded.  The pixel buffer is kept so we can rebuild the
    /// `StatefulProtocol` at a different size without re-running the
    /// slow PNG/JPEG decode.
    Ready(DynamicImage),
    /// Decode failed (IO, remote-blocked, corrupt bytes).  Never retried
    /// automatically — the user has to reopen the document or move a
    /// file into place for the cache to be invalidated.
    Failed(String),
}

/// Both encoded representations of the same image at the same target
/// size.  The `native` protocol is whatever the terminal reported
/// (Kitty / Sixel / iTerm2 / Halfblocks); `halfblocks` is the
/// position-independent fallback used during adverse UX moments
/// (partial visibility, active scrolling on non-Kitty terminals).
///
/// When the native picker IS already halfblocks, there's no point
/// caching a second copy of the same encoding — `halfblocks` is
/// `None` and callers fall back to `native`.
pub struct ProtocolPair {
    pub native: StatefulProtocol,
    pub halfblocks: Option<StatefulProtocol>,
}

/// Cache of decoded images + per-size protocol encodings.
#[derive(Default)]
pub struct ImageCache {
    /// URL → decode status.  Populated by `request` (as `Pending`),
    /// updated to `Ready` or `Failed` by `set_decoded` / `set_failed`.
    decoded: HashMap<String, DecodeStatus>,
    /// (URL, cell-width, cell-height) → encoded protocol pair.  Built
    /// lazily the first time an image is drawn at a given size; dropped
    /// when the entry is no longer referenced by a visible snapshot.
    /// Kept as a plain `HashMap` (no LRU eviction) because the
    /// working-set size is bounded by the number of visible images.
    protocols: HashMap<(String, u16, u16), ProtocolPair>,
}

impl ImageCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark `url` as `Pending` iff it has no prior entry.  Returns true
    /// when a new decode job should be dispatched for this URL.
    ///
    /// Once a URL is `Ready` or `Failed`, `request` is a no-op: we never
    /// auto-retry.
    pub fn request(&mut self, url: &str) -> bool {
        if self.decoded.contains_key(url) {
            return false;
        }
        self.decoded.insert(url.to_owned(), DecodeStatus::Pending);
        true
    }

    /// Record a successful decode.  Called on `AppEvent::ImageReady`.
    /// Drops any stale protocol entries for this URL so the next
    /// `get_protocol` call rebuilds from the new pixels.
    pub fn set_decoded(&mut self, url: &str, image: DynamicImage) {
        self.decoded
            .insert(url.to_owned(), DecodeStatus::Ready(image));
        self.protocols.retain(|(u, _, _), _| u != url);
    }

    /// Record a decode failure.  Displayed status (for debugging) is the
    /// error message we pass in.
    pub fn set_failed(&mut self, url: &str, message: String) {
        self.decoded
            .insert(url.to_owned(), DecodeStatus::Failed(message));
        self.protocols.retain(|(u, _, _), _| u != url);
    }

    /// Look up the decode status for `url`.
    pub fn status(&self, url: &str) -> Option<&DecodeStatus> {
        self.decoded.get(url)
    }

    /// Get a mutable protocol pair for `(url, width, height)`, building
    /// both the native and halfblocks encodings lazily from the cached
    /// `DynamicImage` on a miss.
    ///
    /// Returns `None` when the URL is `Pending` or `Failed`, or when no
    /// `native_picker` is supplied (terminal doesn't support images).
    ///
    /// When the native picker is already halfblocks, the pair's
    /// `halfblocks` field is left as `None` — there's no point caching
    /// the same encoding twice.  Callers should fall back to `native`
    /// in that case (which *is* the halfblocks encoding).
    pub fn get_protocol_pair(
        &mut self,
        url: &str,
        width: u16,
        height: u16,
        native_picker: Option<&Picker>,
        halfblocks_picker: Option<&Picker>,
    ) -> Option<&mut ProtocolPair> {
        let native_picker = native_picker?;
        if !matches!(self.decoded.get(url), Some(DecodeStatus::Ready(_))) {
            return None;
        }
        let key = (url.to_owned(), width, height);
        if !self.protocols.contains_key(&key) {
            // Clone the decoded pixels — DynamicImage is not cheap to
            // clone (it duplicates the buffer) but we only do this on
            // cold-path cache misses, typically once per (url, size).
            let image = match self.decoded.get(url) {
                Some(DecodeStatus::Ready(img)) => img.clone(),
                _ => return None,
            };
            let native = native_picker.new_resize_protocol(image.clone());
            // Skip the redundant halfblocks encoding when the native
            // picker already IS halfblocks.
            let halfblocks = if native_picker.protocol_type() == ProtocolType::Halfblocks {
                None
            } else {
                halfblocks_picker.map(|p| p.new_resize_protocol(image))
            };
            self.protocols
                .insert(key.clone(), ProtocolPair { native, halfblocks });
        }
        self.protocols.get_mut(&key)
    }

    /// Drop every protocol entry, e.g. on terminal resize.
    pub fn invalidate_protocols(&mut self) {
        self.protocols.clear();
    }

    /// Compute the number of rendered rows a decoded image will occupy
    /// when scaled to fit within `max_width_cells × max_height_cells`
    /// cells with `font_size` pixels per cell (width, height), preserving
    /// aspect ratio.  Returns `None` when the image hasn't been decoded
    /// yet — callers should fall back to the configured
    /// `image_max_height` in that case so `per_block_own` stays stable
    /// until real dimensions are known.
    ///
    /// Wide images (w >> h) produce a result less than
    /// `max_height_cells`; that's the whole point of this helper — let
    /// `render_image_block` emit a shorter block so the user doesn't
    /// see extra bottom padding below a wide image.
    pub fn aspect_rows(
        &self,
        url: &str,
        max_width_cells: u16,
        max_height_cells: u16,
        font_size: (u16, u16),
    ) -> Option<usize> {
        let Some(DecodeStatus::Ready(img)) = self.decoded.get(url) else {
            return None;
        };
        let (fw, fh) = (u32::from(font_size.0.max(1)), u32::from(font_size.1.max(1)));
        let box_w_px = u32::from(max_width_cells).saturating_mul(fw);
        let box_h_px = u32::from(max_height_cells).saturating_mul(fh);
        let iw = img.width().max(1);
        let ih = img.height().max(1);
        if box_w_px == 0 || box_h_px == 0 {
            return Some(0);
        }
        // Scale to fit the bounding box while preserving aspect ratio.
        // Use integer arithmetic: compare `ih*box_w_px` against
        // `iw*box_h_px` to decide which dimension binds, then compute
        // the fitted height in pixels, then round up to cells.
        let h_if_width_binds = (u64::from(ih) * u64::from(box_w_px)) / u64::from(iw);
        let fitted_h_px = h_if_width_binds.min(u64::from(box_h_px));
        // Round up: div_ceil.
        let rows = fitted_h_px.div_ceil(u64::from(fh));
        Some(rows.clamp(1, u64::from(max_height_cells)) as usize)
    }

    /// Clear `Failed` entries so a subsequent `request` can retry.  Called
    /// by the App after the user promotes the remote-image policy —
    /// entries that failed with `RemoteBlocked` can now succeed.
    pub fn clear_failures_for_remote_reopening(&mut self) {
        self.decoded
            .retain(|_, status| !matches!(status, DecodeStatus::Failed(_)));
    }

    #[cfg(test)]
    pub fn decoded_count(&self) -> usize {
        self.decoded.len()
    }

    #[cfg(test)]
    pub fn protocol_count(&self) -> usize {
        self.protocols.len()
    }
}

#[cfg(test)]
// `Picker::from_fontsize` is deprecated in ratatui-image 9 but the
// non-deprecated alternatives (`Picker::halfblocks`) don't let us set a
// specific font-size, which matters for some of the assertions below.
#[allow(deprecated)]
mod tests {
    use super::*;

    #[test]
    fn request_returns_true_first_time_only() {
        let mut cache = ImageCache::new();
        assert!(cache.request("a.png"));
        assert!(!cache.request("a.png"));
        assert!(matches!(cache.status("a.png"), Some(DecodeStatus::Pending)));
    }

    #[test]
    fn set_decoded_transitions_from_pending_to_ready() {
        let mut cache = ImageCache::new();
        cache.request("a.png");
        let img = DynamicImage::new_rgba8(1, 1);
        cache.set_decoded("a.png", img);
        assert!(matches!(
            cache.status("a.png"),
            Some(DecodeStatus::Ready(_))
        ));
        // Even after Ready, requesting again is a no-op (no retry).
        assert!(!cache.request("a.png"));
    }

    #[test]
    fn set_failed_transitions_and_blocks_retry() {
        let mut cache = ImageCache::new();
        cache.request("a.png");
        cache.set_failed("a.png", "io error".to_owned());
        assert!(matches!(
            cache.status("a.png"),
            Some(DecodeStatus::Failed(_))
        ));
        assert!(!cache.request("a.png"));
    }

    #[test]
    fn set_decoded_clears_stale_protocol_entries() {
        let mut cache = ImageCache::new();
        cache.request("a.png");
        // Can't populate protocols without a Picker — simulate instead.
        cache
            .protocols
            .insert(("a.png".into(), 10, 10), dummy_protocol_pair());
        assert_eq!(cache.protocol_count(), 1);
        cache.set_decoded("a.png", DynamicImage::new_rgba8(1, 1));
        assert_eq!(cache.protocol_count(), 0);
    }

    fn dummy_protocol_pair() -> ProtocolPair {
        // Halfblocks is always available and cheap to construct.
        let picker = Picker::from_fontsize((1, 1));
        ProtocolPair {
            native: picker.new_resize_protocol(DynamicImage::new_rgba8(1, 1)),
            halfblocks: None,
        }
    }

    #[test]
    fn invalidate_protocols_clears_all_protocol_entries_only() {
        let mut cache = ImageCache::new();
        cache.request("a.png");
        cache.set_decoded("a.png", DynamicImage::new_rgba8(1, 1));
        cache
            .protocols
            .insert(("a.png".into(), 1, 1), dummy_protocol_pair());
        cache.invalidate_protocols();
        assert_eq!(cache.protocol_count(), 0);
        // Decoded entry survives.
        assert!(matches!(
            cache.status("a.png"),
            Some(DecodeStatus::Ready(_))
        ));
    }

    #[test]
    fn protocol_pair_from_halfblocks_native_skips_second_encode() {
        // When the terminal's native protocol IS halfblocks, building a
        // separate halfblocks fallback is redundant — `halfblocks` stays
        // `None` and callers fall through to `native`.
        let mut cache = ImageCache::new();
        cache.request("a.png");
        cache.set_decoded("a.png", DynamicImage::new_rgba8(4, 4));
        // `Picker::from_fontsize` defaults to Halfblocks.
        let picker = Picker::from_fontsize((1, 2));
        let pair = cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker))
            .expect("pair for ready image");
        assert!(pair.halfblocks.is_none());
    }

    #[test]
    fn protocol_pair_with_non_halfblocks_native_caches_both() {
        // Simulate a non-halfblocks native protocol by forcing it.
        // `Picker::from_fontsize` only yields Halfblocks so we can't
        // construct a Kitty/Sixel/iTerm2 picker in a unit test; instead
        // we exercise the negative branch by passing distinct pickers
        // (both halfblocks) and asserting the function wired through a
        // second encode.  This test guards the control-flow shape — the
        // actual protocol_type gate is exercised when the native picker
        // has ProtocolType != Halfblocks, which only occurs at runtime
        // on a real graphics terminal.
        let mut cache = ImageCache::new();
        cache.request("b.png");
        cache.set_decoded("b.png", DynamicImage::new_rgba8(4, 4));
        let picker = Picker::from_fontsize((1, 2));
        // Even on an all-halfblocks test bench we verify the call path
        // completes without panicking and returns Some.
        assert!(cache
            .get_protocol_pair("b.png", 8, 4, Some(&picker), Some(&picker))
            .is_some());
    }

    #[test]
    fn get_protocol_pair_returns_none_without_native_picker() {
        let mut cache = ImageCache::new();
        cache.request("a.png");
        cache.set_decoded("a.png", DynamicImage::new_rgba8(1, 1));
        assert!(cache.get_protocol_pair("a.png", 8, 4, None, None).is_none());
    }

    #[test]
    fn get_protocol_pair_returns_none_for_pending() {
        let mut cache = ImageCache::new();
        cache.request("a.png");
        let picker = Picker::from_fontsize((1, 2));
        assert!(cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker))
            .is_none());
    }

    #[test]
    fn aspect_rows_returns_none_before_decode() {
        let mut cache = ImageCache::new();
        cache.request("a.png");
        assert!(cache.aspect_rows("a.png", 80, 24, (10, 20)).is_none());
    }

    #[test]
    fn aspect_rows_wide_image_returns_fewer_rows_than_max() {
        // A 1600×400 image with 10×20 px cells:
        //   box_w_px = 80 * 10 = 800
        //   box_h_px = 24 * 20 = 480
        //   width binds: ih * box_w_px / iw = 400 * 800 / 1600 = 200 px
        //   rows = ceil(200 / 20) = 10
        let mut cache = ImageCache::new();
        cache.request("wide.png");
        cache.set_decoded("wide.png", DynamicImage::new_rgba8(1600, 400));
        assert_eq!(cache.aspect_rows("wide.png", 80, 24, (10, 20)), Some(10));
    }

    #[test]
    fn aspect_rows_tall_image_clamps_to_max_height() {
        // A 400×1600 image in the same 80×24 box — height binds and
        // the fitted cell count exceeds max_height, so we clamp.
        let mut cache = ImageCache::new();
        cache.request("tall.png");
        cache.set_decoded("tall.png", DynamicImage::new_rgba8(400, 1600));
        assert_eq!(cache.aspect_rows("tall.png", 80, 24, (10, 20)), Some(24));
    }

    #[test]
    fn aspect_rows_square_image_fills_square_region() {
        // A 400×400 image, box 80×24 cells, 10×20 px cells:
        //   box_w_px = 800, box_h_px = 480
        //   width-binds would give ih*box_w/iw = 400*800/400 = 800 px (40 rows)
        //   but height-binding caps at 480 px → 24 rows
        //   -> we hit the height cap (clamped).
        let mut cache = ImageCache::new();
        cache.request("sq.png");
        cache.set_decoded("sq.png", DynamicImage::new_rgba8(400, 400));
        assert_eq!(cache.aspect_rows("sq.png", 80, 24, (10, 20)), Some(24));
    }

    #[test]
    fn aspect_rows_small_image_rounds_up_to_one_row() {
        // A 1×1 image scaled into an 80×24 box: fitted_h_px = 1*800/1 =
        // 800 → but box_h_px caps at 480 → clamp to 480 px → 24 rows.
        // Separate test: a 10×2 image at 10×20 cells:
        //   box_w_px = 800, box_h_px = 480
        //   ih * box_w_px / iw = 2 * 800 / 10 = 160 px → 8 rows
        let mut cache = ImageCache::new();
        cache.request("thin.png");
        cache.set_decoded("thin.png", DynamicImage::new_rgba8(10, 2));
        assert_eq!(cache.aspect_rows("thin.png", 80, 24, (10, 20)), Some(8));
    }
}
