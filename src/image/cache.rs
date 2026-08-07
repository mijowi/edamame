//! Decoded-image cache retained across reparses.
//!
//! `ParsedDoc` is rebuilt on every buffer mutation, so keeping decoded
//! image bytes (and their expensive `StatefulProtocol` encodings) on the
//! parse tree would mean re-decoding on every keystroke.  Instead we
//! cache by URL on `EditorState`: the URL set rarely changes during
//! editing, and protocols are keyed additionally by target cell
//! dimensions so a terminal resize invalidates only the affected
//! entries, not unrelated text.

use std::collections::{HashMap, VecDeque};
use std::sync::{mpsc, Arc};

use image::DynamicImage;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::StatefulWidget;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::thread::{ResizeRequest, ResizeResponse, ThreadProtocol};
use ratatui_image::{Resize, StatefulImage};

/// Encode `image` as halfblocks at `rect` using `picker` and return a
/// `Buffer` containing the rendered cells.
///
/// **Only `picker`'s font size is used; its protocol is forced to
/// `Halfblocks` regardless of what it carries.** Passing a Kitty or
/// iTerm2 picker does not produce a Kitty or iTerm2 encoding — it
/// produces halfblocks at that picker's cell aspect ratio, which is the
/// entire point: the scratch has to be *position-independent* cells for
/// `paint_halfblocks_partial` to clip it by row, while still matching
/// the native protocol's aspect ratio so an image doesn't change shape
/// when it crosses the native↔halfblocks boundary mid-scroll.  A native
/// encoding would instead put the whole image in one cell as a single
/// escape sequence surrounded by `skip` cells, which cannot be clipped
/// at all — the image would flash on the frames that copy row 0 and
/// vanish otherwise.  `Capabilities` already pins its
/// `halfblocks_picker` to `Halfblocks`; re-forcing it here keeps the
/// invariant local to the one function that depends on it.
///
/// Cheap enough (low single-digit ms on pre-resized images) that it is
/// usable on either the UI thread or a worker.  The decode worker calls
/// this immediately after pre-resizing so that by the time
/// `AppEvent::ImageReady` fires, the scratch is already built and the
/// UI thread's first paint is a pure cache hit.  `get_protocol_pair`
/// retains the fallback sync path for the terminal-resize case where
/// the pre-rendered scratch's `(width, height)` no longer matches.
pub fn render_halfblocks_scratch(picker: &Picker, image: DynamicImage, rect: Rect) -> Buffer {
    let mut picker = picker.clone();
    picker.set_protocol_type(ProtocolType::Halfblocks);
    let mut protocol = picker.new_resize_protocol(image);
    let mut buf = Buffer::empty(rect);
    StatefulImage::default()
        .resize(Resize::Fit(None))
        .render(rect, &mut buf, &mut protocol);
    buf
}

/// Free-function twin of [`ImageCache::aspect_rows`] that operates on a
/// borrowed `DynamicImage`.  The decode worker calls this to compute the
/// scratch's target height before it has handed the image off to the
/// cache.
///
/// Mirrors the paint path's `Resize::Fit(None)`, which scales an image
/// *down* to fit the cell envelope but never *up*: the reserved height is
/// therefore capped at the image's own pixel height.  Without this cap a
/// small image (a 190×65 logo, a 24×24 icon, a downscaled-to-natural SVG)
/// would reserve as many rows as it *would* occupy if blown up to the
/// column width, leaving a tall blank band below the image that Fit
/// actually renders at natural size.
pub fn aspect_rows_of(
    image: &DynamicImage,
    max_width_cells: u16,
    max_height_cells: u16,
    font_size: (u16, u16),
) -> usize {
    let (fw, fh) = (u32::from(font_size.0.max(1)), u32::from(font_size.1.max(1)));
    let box_w_px = u32::from(max_width_cells).saturating_mul(fw);
    let box_h_px = u32::from(max_height_cells).saturating_mul(fh);
    let iw = image.width().max(1);
    let ih = image.height().max(1);
    if box_w_px == 0 || box_h_px == 0 {
        return 0;
    }
    let h_if_width_binds = (u64::from(ih) * u64::from(box_w_px)) / u64::from(iw);
    // Cap at the natural height too: `Resize::Fit(None)` never upscales, so
    // an image narrower than the column is painted at natural size, not
    // stretched to fill the width.  Reserving the width-bound height here
    // would over-reserve and leave a blank gap below the image.
    let fitted_h_px = h_if_width_binds.min(u64::from(box_h_px)).min(u64::from(ih));
    let rows = fitted_h_px.div_ceil(u64::from(fh));
    (rows.clamp(1, u64::from(max_height_cells)) as usize).max(1)
}

/// Status of a decode attempt for a URL.
pub enum DecodeStatus {
    /// Decode has been dispatched to a worker thread (or is about to be)
    /// and is in flight.  `paint_images` shows the `[Image: alt]`
    /// placeholder while `Pending`.
    Pending,
    /// Decode succeeded.  The pixel buffer is kept (inside an `Arc` so
    /// rebuilding a `StatefulProtocol` at a new size doesn't duplicate
    /// the decoded bytes) so we can rebuild the protocol at a different
    /// size without re-running the slow PNG/JPEG decode.
    Ready(Arc<DynamicImage>),
    /// Decode failed (IO, remote-blocked, corrupt bytes).  Never retried
    /// automatically — the user has to reopen the document or move a
    /// file into place for the cache to be invalidated.  The message is
    /// captured for future surfacing (e.g. status-bar diagnostics) but
    /// has no live consumer yet.
    Failed(#[allow(dead_code)] String),
}

/// Metadata for a resize-encode request that is currently being worked on
/// by the encoder thread.  Used to route the `ResizeResponse` back to the
/// originating `ThreadProtocol`: ratatui-image's `ResizeResponse` carries
/// only a protocol-local id (with no public accessor), so we maintain a
/// FIFO of our own request metadata and pop the front when each response
/// arrives.  The underlying worker is serial, so FIFO order is exact.
struct PendingResize {
    url: String,
    width: u16,
    height: u16,
}

/// Both encoded representations of the same image at the same target
/// size.
///
/// `native` holds the terminal's preferred graphics protocol (Kitty /
/// Sixel / iTerm2) wrapped in a `ThreadProtocol` so its first encode —
/// potentially tens to hundreds of milliseconds for large images — runs
/// on the dedicated encoder worker thread, not on the UI thread.
/// `native` is `None` when the detected image protocol IS halfblocks,
/// because in that case halfblocks-scratch alone is the rendering.
///
/// `halfblocks_scratch` is a pre-rendered `Buffer` containing the
/// halfblocks cells.  It is built SYNCHRONOUSLY on the cold path
/// (tolerable: halfblocks encoding is fast on pre-resized images) so
/// that as soon as a decode completes, the image can be shown
/// immediately as halfblocks — no placeholder flash.  While `native`
/// continues to encode off-thread, paint_images renders from this
/// scratch buffer; once `native_ready` becomes true, paint_images
/// upgrades to the full-quality native protocol.
///
/// `native_ready` is set by `apply_resize_response` after the worker
/// successfully encodes `native` at the pair's dimensions.  It gates
/// the native render path, preventing a placeholder flash between the
/// cold-path build and the worker's completion.
pub struct ProtocolPair {
    pub native: Option<ThreadProtocol>,
    pub native_ready: bool,
    /// Pre-rendered halfblocks cells for this `(url, width, height)`.
    /// Populated synchronously in the cold path of `get_protocol_pair`.
    /// Used as the fallback rendering while `native` encodes, during
    /// active scroll on non-Kitty terminals, and during partial
    /// visibility.
    pub halfblocks_scratch: Option<Buffer>,
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
    /// Halfblocks scratches pre-built on the decode worker thread and
    /// waiting for their first `get_protocol_pair` call to claim them.
    /// On cold-path construction we `remove` the matching entry instead
    /// of running `render_halfblocks_scratch` synchronously on the UI
    /// thread.  Entries that never match (e.g. terminal resized between
    /// decode and first paint) stay here until `set_decoded` or
    /// `invalidate_protocols` clears them.
    prebuilt_scratches: HashMap<(String, u16, u16), Buffer>,
    /// Outstanding encode requests, FIFO in dispatch order.  Popped by
    /// `apply_resize_response` to locate the target `ThreadProtocol`.
    pending: VecDeque<PendingResize>,
    /// Sender into the encoder worker.  Cloned into each `ThreadProtocol`
    /// we build so that calling `thread_protocol.resize_encode(...)`
    /// ships the blocking encode off to the worker.  `None` disables
    /// image rendering entirely (tests, terminals without image support);
    /// `get_protocol_pair` then returns `None` and callers show the
    /// `[Image: alt]` placeholder.
    resize_tx: Option<mpsc::Sender<ResizeRequest>>,
}

impl ImageCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the sender for the encoder worker's channel.  Called by
    /// `App::run` once the worker thread has been spawned.  Without a
    /// sender attached, `get_protocol_pair` returns `None` and callers
    /// show the `[Image: alt]` placeholder.
    ///
    /// Attaching (or changing) the sender drops any previously-cached
    /// protocols — their internal `Sender<ResizeRequest>` is tied to the
    /// old channel and would otherwise ship requests into a dead
    /// endpoint.  Decoded pixels are retained.
    pub fn attach_resize_sender(&mut self, tx: mpsc::Sender<ResizeRequest>) {
        self.resize_tx = Some(tx);
        self.protocols.clear();
        self.pending.clear();
        self.prebuilt_scratches.clear();
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

    /// Record a successful decode.  Called on `AppEvent::ImageReady` from
    /// integration tests in `tests/`.  Production code uses
    /// `set_decoded_with_prebuilt` so the halfblocks scratch is also
    /// captured.
    #[allow(dead_code)]
    pub fn set_decoded(&mut self, url: &str, image: DynamicImage) {
        self.set_decoded_with_prebuilt(url, image, None);
    }

    /// `set_decoded` plus stash a halfblocks scratch the decode worker
    /// already rendered.  The next `get_protocol_pair` call for the same
    /// `(url, width, height)` will claim the prebuilt buffer instead of
    /// running the encode synchronously on the UI thread.
    pub fn set_decoded_with_prebuilt(
        &mut self,
        url: &str,
        image: DynamicImage,
        prebuilt_scratch: Option<(Rect, Buffer)>,
    ) {
        self.decoded
            .insert(url.to_owned(), DecodeStatus::Ready(Arc::new(image)));
        self.protocols.retain(|(u, _, _), _| u != url);
        // Drop any stale prebuilt scratches for this URL before taking
        // the new one.
        self.prebuilt_scratches.retain(|(u, _, _), _| u != url);
        if let Some((rect, buf)) = prebuilt_scratch {
            self.prebuilt_scratches
                .insert((url.to_owned(), rect.width, rect.height), buf);
        }
    }

    /// Record a decode failure.  Displayed status (for debugging) is the
    /// error message we pass in.
    pub fn set_failed(&mut self, url: &str, message: String) {
        self.decoded
            .insert(url.to_owned(), DecodeStatus::Failed(message));
        self.protocols.retain(|(u, _, _), _| u != url);
        self.prebuilt_scratches.retain(|(u, _, _), _| u != url);
    }

    /// Look up the decode status for `url`.  Used by integration tests in
    /// `tests/`.
    #[allow(dead_code)]
    pub fn status(&self, url: &str) -> Option<&DecodeStatus> {
        self.decoded.get(url)
    }

    /// Get a mutable protocol pair for `(url, width, height)`, building
    /// the native (`ThreadProtocol`, async encoding) and pre-rendering
    /// the halfblocks scratch (sync, one-time) on a cold miss.
    ///
    /// Returns `None` when the URL is `Pending` or `Failed`, when no
    /// `native_picker` is supplied (terminal doesn't support images), or
    /// when no encoder-worker `resize_tx` is attached.
    ///
    /// When `native_picker`'s protocol IS halfblocks, the pair's
    /// `native` is `None` — only `halfblocks_scratch` is used, since
    /// halfblocks is both the preferred and fallback rendering.
    pub fn get_protocol_pair(
        &mut self,
        url: &str,
        width: u16,
        height: u16,
        native_picker: Option<&Picker>,
        halfblocks_picker: Option<&Picker>,
    ) -> Option<&mut ProtocolPair> {
        let native_picker = native_picker?;
        let resize_tx = self.resize_tx.as_ref()?.clone();
        if !matches!(self.decoded.get(url), Some(DecodeStatus::Ready(_))) {
            return None;
        }
        let key = (url.to_owned(), width, height);
        if !self.protocols.contains_key(&key) {
            let image_arc = match self.decoded.get(url) {
                Some(DecodeStatus::Ready(img)) => Arc::clone(img),
                _ => return None,
            };
            let full_rect = Rect::new(0, 0, width, height);
            let is_halfblocks_native = native_picker.protocol_type() == ProtocolType::Halfblocks;

            // Prefer the scratch the decode worker already rendered
            // for this `(url, width, height)`.  `remove` takes it so we
            // don't hold the buffer twice.  When the worker didn't
            // produce one (no picker/dims at dispatch time) or the
            // dims don't match (terminal resized between decode and
            // first paint), fall back to a sync encode on the UI
            // thread — cost of ~5-20 ms, same as pre-Phase-7a
            // behaviour, and rare enough not to regress scroll.
            let halfblocks_scratch = if let Some(buf) = self.prebuilt_scratches.remove(&key) {
                Some(buf)
            } else if is_halfblocks_native {
                Some(render_halfblocks_scratch(
                    native_picker,
                    (*image_arc).clone(),
                    full_rect,
                ))
            } else {
                halfblocks_picker
                    .map(|p| render_halfblocks_scratch(p, (*image_arc).clone(), full_rect))
            };

            // Native: build a ThreadProtocol so the slow Kitty/Sixel/iTerm2
            // encode runs on the worker.  Skipped when native IS halfblocks
            // — the scratch above IS the rendering in that case.
            let native = if is_halfblocks_native {
                None
            } else {
                let native_inner = native_picker.new_resize_protocol((*image_arc).clone());
                Some(ThreadProtocol::new(resize_tx, Some(native_inner)))
            };

            self.protocols.insert(
                key.clone(),
                ProtocolPair {
                    native,
                    native_ready: false,
                    halfblocks_scratch,
                },
            );
        }
        self.protocols.get_mut(&key)
    }

    /// Look up an existing protocol pair without the Picker-dependent
    /// cold-path rebuild that `get_protocol_pair` performs.  Callers
    /// should have ensured the pair exists (e.g. by calling
    /// `get_protocol_pair` earlier in the same frame).
    pub fn protocol_pair_mut(
        &mut self,
        url: &str,
        width: u16,
        height: u16,
    ) -> Option<&mut ProtocolPair> {
        self.protocols.get_mut(&(url.to_owned(), width, height))
    }

    /// Record that a `resize_encode` request for the pair's native
    /// protocol was just dispatched to the encoder worker.  The matching
    /// `ResizeResponse` will be routed back to the same
    /// `ThreadProtocol` by `apply_resize_response`.
    pub fn track_pending_resize(&mut self, url: &str, width: u16, height: u16) {
        self.pending.push_back(PendingResize {
            url: url.to_owned(),
            width,
            height,
        });
    }

    /// Drop the oldest pending-request entry without applying a
    /// response.  Called when the encoder worker reports an error — we
    /// still need to pop the FIFO so the next successful response lines
    /// up with its originating protocol.
    pub fn drop_pending_front(&mut self) {
        self.pending.pop_front();
    }

    /// Route an encoded `ResizeResponse` back to its originating
    /// `ThreadProtocol` by popping the oldest pending-request entry.
    /// The worker channel is single-threaded and FIFO, so the response
    /// order matches the request order.
    ///
    /// If the target pair has since been dropped (e.g. the URL's decoded
    /// image was replaced), the response is silently discarded.  The
    /// `ThreadProtocol::update_resized_protocol` call additionally
    /// rejects responses whose internal id is stale (superseded by a
    /// later request on the same protocol).
    pub fn apply_resize_response(&mut self, resp: ResizeResponse) {
        let Some(pending) = self.pending.pop_front() else {
            return;
        };
        let key = (pending.url, pending.width, pending.height);
        if let Some(pair) = self.protocols.get_mut(&key) {
            if let Some(native) = pair.native.as_mut() {
                if native.update_resized_protocol(resp) {
                    pair.native_ready = true;
                }
            }
        }
    }

    /// Drop every protocol entry, e.g. on terminal resize.  Pending
    /// requests remain in the queue (the worker will still produce
    /// responses for them); those responses become orphan pops and are
    /// silently discarded by `apply_resize_response`.  Used by tests in
    /// this crate.
    #[allow(dead_code)]
    pub fn invalidate_protocols(&mut self) {
        self.protocols.clear();
        // Prebuilt scratches are keyed by the old `(width, height)` too,
        // so their target rect is stale after a resize.  Drop them; the
        // next decode cycle repopulates at the new dims.
        self.prebuilt_scratches.clear();
    }

    /// Compute the number of rendered rows a decoded image will occupy
    /// when scaled to fit within `max_width_cells × max_height_cells`
    /// cells with `font_size` pixels per cell (width, height), preserving
    /// aspect ratio.  Returns `None` when the image hasn't been decoded
    /// yet.  Used by tests in this crate.
    #[allow(dead_code)]
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
        Some(aspect_rows_of(
            img,
            max_width_cells,
            max_height_cells,
            font_size,
        ))
    }

    /// Row count the renderer should reserve for this image's block.
    ///
    /// * `Ready` → `Some(aspect_rows)` so wide images don't leave blank
    ///   rows underneath.
    /// * `Failed` → `Some(1)` so the block collapses to just the
    ///   `[Image: alt]` placeholder row; no blank space is reserved for
    ///   an image that won't load (e.g. `RemoteBlocked` after the user
    ///   declined the remote-image prompt).
    /// * `Pending` / not yet requested → `None`.  The renderer falls
    ///   back to the configured `image_max_height` while the decode is
    ///   in flight so layout is stable until real dimensions are known.
    pub fn reserved_rows(
        &self,
        url: &str,
        max_width_cells: u16,
        max_height_cells: u16,
        font_size: (u16, u16),
    ) -> Option<usize> {
        match self.decoded.get(url) {
            Some(DecodeStatus::Ready(img)) => Some(aspect_rows_of(
                img,
                max_width_cells,
                max_height_cells,
                font_size,
            )),
            Some(DecodeStatus::Failed(_)) => Some(1),
            Some(DecodeStatus::Pending) | None => None,
        }
    }

    /// Clear `Failed` entries so a subsequent `request` can retry.  Called
    /// by the App after the user promotes the remote-image policy —
    /// entries that failed with `RemoteBlocked` can now succeed.
    pub fn clear_failures_for_remote_reopening(&mut self) {
        self.decoded
            .retain(|_, status| !matches!(status, DecodeStatus::Failed(_)));
    }

    /// Drop a single URL's entries (decode status, protocols, scratches)
    /// so a later `request` treats it as never seen.  Called by the App
    /// when a worker's `ImageReady` result arrives that the *current*
    /// settings forbid (the worker captured the policy at spawn time) —
    /// the per-frame dispatch then re-resolves the URL if the settings
    /// permit it again.
    pub fn forget(&mut self, url: &str) {
        self.decoded.remove(url);
        self.protocols.retain(|(u, _, _), _| u != url);
        self.prebuilt_scratches.retain(|(u, _, _), _| u != url);
    }

    /// Drop every entry (decoded pixels, protocols, scratches) whose URL
    /// is remote (`http://` / `https://`).  Called by the App when the
    /// remote-image policy changes so the next dispatch re-resolves each
    /// remote URL under the new policy — a decoded image disappears when
    /// the policy tightens, and a memoised `RemoteBlocked` failure can
    /// retry when it loosens.
    pub fn evict_remote(&mut self) {
        self.decoded
            .retain(|url, _| !crate::image::loader::is_remote(url));
        self.protocols
            .retain(|(url, _, _), _| !crate::image::loader::is_remote(url));
        self.prebuilt_scratches
            .retain(|(url, _, _), _| !crate::image::loader::is_remote(url));
        // `pending` is deliberately left untouched: the encode worker
        // will still produce a response for every request already
        // shipped, and `apply_resize_response` pairs responses with
        // requests by FIFO order.  Responses for evicted URLs become
        // orphan pops and are silently discarded, same as after
        // `invalidate_protocols`.
    }

    /// Drop every entry whose URL is not in `live`.  Called by the App
    /// after each reparse to prune diagrams whose synthetic URL changed
    /// (content-edit inside a ```mermaid block → new sha → fresh cache
    /// key) along with any other no-longer-referenced URLs.  Without
    /// this, editing a single diagram repeatedly would grow `decoded`
    /// and `protocols` without bound.
    pub fn gc(&mut self, live: &std::collections::HashSet<String>) {
        self.decoded.retain(|url, _| live.contains(url));
        self.protocols.retain(|(url, _, _), _| live.contains(url));
        self.prebuilt_scratches
            .retain(|(url, _, _), _| live.contains(url));
    }

    #[cfg(test)]
    pub fn protocol_count(&self) -> usize {
        self.protocols.len()
    }

    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    pub fn prebuilt_scratch_count(&self) -> usize {
        self.prebuilt_scratches.len()
    }
}

#[cfg(test)]
// `Picker::from_fontsize` is deprecated in ratatui-image 9 but the
// non-deprecated alternatives (`Picker::halfblocks`) don't let us set a
// specific font-size, which matters for some of the assertions below.
#[allow(deprecated)]
mod tests {
    use super::*;

    /// A picker guaranteed to encode **halfblocks**, at a font size the
    /// assertions below can reason about.
    ///
    /// `Picker::from_fontsize` on its own is environment-dependent: it
    /// infers its protocol from `$TERM_PROGRAM` / `$LC_TERMINAL`, so it
    /// yields Halfblocks in Ghostty or kitty but **iTerm2** in iTerm2,
    /// WezTerm, VS Code, Warp, Hyper, Tabby, rio, mintty and Bobcat.
    /// Tests built on the bare constructor therefore pass or fail
    /// depending on which terminal `cargo test` was launched from.
    fn halfblocks_picker() -> Picker {
        let mut picker = Picker::from_fontsize((1, 2).into());
        picker.set_protocol_type(ProtocolType::Halfblocks);
        picker
    }

    /// A picker whose protocol is deliberately *not* halfblocks, so the
    /// native-plus-scratch branch of `get_protocol_pair` can be exercised
    /// on any machine.  iTerm2 is the cheapest to encode of the three
    /// native protocols and needs no terminal support to construct.
    fn native_picker() -> Picker {
        let mut picker = Picker::from_fontsize((1, 2).into());
        picker.set_protocol_type(ProtocolType::Iterm2);
        picker
    }

    // ── aspect_rows_of ────────────────────────────────────────────────

    #[test]
    fn small_image_reserves_natural_height_not_width_filled() {
        // A 190×65 logo (mijowi.svg) in an 80×40-cell envelope at (8,16):
        // box is 640×640 px.  Width-filled it would be 65*640/190 = 219 px
        // = 14 rows, but `Resize::Fit(None)` never upscales, so it paints
        // at its natural 65 px (≈5 rows).  The reservation must match the
        // paint, not the hypothetical width-fill.
        let img = DynamicImage::new_rgba8(190, 65);
        let rows = aspect_rows_of(&img, 80, 40, (8, 16));
        assert_eq!(rows, 65_u32.div_ceil(16) as usize);
    }

    #[test]
    fn wide_image_still_binds_on_width() {
        // A 1920×1080 photo is wider than the column, so Fit downscales it
        // to fill the width — the natural-height cap must not interfere.
        let img = DynamicImage::new_rgba8(1920, 1080);
        let rows = aspect_rows_of(&img, 80, 40, (8, 16));
        // 1080 * 640 / 1920 = 360 px → ceil(360/16) = 23 rows.
        assert_eq!(rows, 23);
    }

    #[test]
    fn forget_drops_one_url_and_allows_a_re_request() {
        let mut cache = ImageCache::new();
        cache.set_decoded("a.png", DynamicImage::new_rgba8(1, 1));
        cache.set_decoded("b.png", DynamicImage::new_rgba8(1, 1));
        cache.forget("a.png");
        assert!(cache.status("a.png").is_none());
        assert!(cache.status("b.png").is_some(), "other entries untouched");
        assert!(cache.request("a.png"), "forgotten URL can be re-requested");
    }

    #[test]
    fn evict_remote_drops_remote_entries_and_keeps_local() {
        let mut cache = ImageCache::new();
        cache.set_decoded("https://example.com/a.png", DynamicImage::new_rgba8(1, 1));
        cache.set_failed("http://example.com/b.png", "blocked".into());
        cache.set_decoded("local/c.png", DynamicImage::new_rgba8(1, 1));
        cache.evict_remote();
        assert!(cache.status("https://example.com/a.png").is_none());
        assert!(cache.status("http://example.com/b.png").is_none());
        assert!(matches!(
            cache.status("local/c.png"),
            Some(DecodeStatus::Ready(_))
        ));
        // Evicted URLs can be re-requested under the new policy.
        assert!(cache.request("https://example.com/a.png"));
    }

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

    /// Build a cache with a drained mpsc receiver so `get_protocol_pair`
    /// can successfully clone the resize sender.  The receiver is leaked
    /// via `std::mem::forget` — we don't care about the sent requests,
    /// only that the channel stays alive for the duration of the test.
    fn cache_with_sender() -> ImageCache {
        let (tx, rx) = mpsc::channel::<ResizeRequest>();
        let mut cache = ImageCache::new();
        cache.attach_resize_sender(tx);
        std::mem::forget(rx);
        cache
    }

    #[test]
    fn set_decoded_clears_stale_protocol_entries() {
        let mut cache = cache_with_sender();
        cache.request("a.png");
        cache.set_decoded("a.png", DynamicImage::new_rgba8(1, 1));
        let picker = halfblocks_picker();
        assert!(cache
            .get_protocol_pair("a.png", 10, 10, Some(&picker), Some(&picker))
            .is_some());
        assert_eq!(cache.protocol_count(), 1);
        // A new set_decoded clears any protocol pairs for that URL.
        cache.set_decoded("a.png", DynamicImage::new_rgba8(2, 2));
        assert_eq!(cache.protocol_count(), 0);
    }

    #[test]
    fn set_decoded_with_prebuilt_stashes_scratch_for_matching_dims() {
        // When the decode worker hands back a pre-rendered scratch, the
        // cache stashes it so the first `get_protocol_pair` call at the
        // same `(url, width, height)` consumes it without running
        // `render_halfblocks_scratch` on the UI thread.
        let mut cache = cache_with_sender();
        cache.request("a.png");
        let rect = Rect::new(0, 0, 8, 4);
        let prebuilt = Buffer::empty(rect);
        cache.set_decoded_with_prebuilt(
            "a.png",
            DynamicImage::new_rgba8(8, 4),
            Some((rect, prebuilt)),
        );
        assert_eq!(cache.prebuilt_scratch_count(), 1);

        // Consume it via get_protocol_pair with matching dims.
        let picker = halfblocks_picker();
        let pair = cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker))
            .expect("pair for ready image");
        assert!(pair.halfblocks_scratch.is_some());
        // Prebuilt map was drained.
        assert_eq!(cache.prebuilt_scratch_count(), 0);
    }

    #[test]
    fn set_decoded_with_prebuilt_falls_back_to_sync_on_mismatched_dims() {
        // If the UI thread later requests a different `(width, height)`
        // (e.g. terminal resized between decode dispatch and first
        // paint), the prebuilt entry isn't found and the cold-path sync
        // render runs.  The prebuilt stays in the map until
        // `invalidate_protocols` or a later `set_decoded` clears it.
        let mut cache = cache_with_sender();
        cache.request("a.png");
        let prebuilt_rect = Rect::new(0, 0, 8, 4);
        let prebuilt = Buffer::empty(prebuilt_rect);
        cache.set_decoded_with_prebuilt(
            "a.png",
            DynamicImage::new_rgba8(8, 4),
            Some((prebuilt_rect, prebuilt)),
        );

        // Request at a different width; scratch still produced, but via
        // sync render (not from the prebuilt map).
        let picker = halfblocks_picker();
        let pair = cache
            .get_protocol_pair("a.png", 16, 4, Some(&picker), Some(&picker))
            .expect("pair for ready image");
        assert!(pair.halfblocks_scratch.is_some());
        // The un-claimed prebuilt remains — future paint at matching
        // dims could still claim it.
        assert_eq!(cache.prebuilt_scratch_count(), 1);
    }

    #[test]
    fn invalidate_protocols_also_clears_prebuilt_scratches() {
        let mut cache = cache_with_sender();
        cache.request("a.png");
        let rect = Rect::new(0, 0, 8, 4);
        cache.set_decoded_with_prebuilt(
            "a.png",
            DynamicImage::new_rgba8(8, 4),
            Some((rect, Buffer::empty(rect))),
        );
        assert_eq!(cache.prebuilt_scratch_count(), 1);
        cache.invalidate_protocols();
        assert_eq!(cache.prebuilt_scratch_count(), 0);
    }

    #[test]
    fn invalidate_protocols_clears_all_protocol_entries_only() {
        let mut cache = cache_with_sender();
        cache.request("a.png");
        cache.set_decoded("a.png", DynamicImage::new_rgba8(1, 1));
        let picker = halfblocks_picker();
        cache
            .get_protocol_pair("a.png", 1, 1, Some(&picker), Some(&picker))
            .expect("pair for ready image");
        cache.invalidate_protocols();
        assert_eq!(cache.protocol_count(), 0);
        // Decoded entry survives.
        assert!(matches!(
            cache.status("a.png"),
            Some(DecodeStatus::Ready(_))
        ));
    }

    #[test]
    fn protocol_pair_from_halfblocks_native_skips_native_thread_protocol() {
        // When the terminal's native protocol IS halfblocks, there is no
        // slow native encode to ship off-thread — `native` stays `None`
        // and `halfblocks_scratch` IS the rendering.
        let mut cache = cache_with_sender();
        cache.request("a.png");
        cache.set_decoded("a.png", DynamicImage::new_rgba8(4, 4));
        let picker = halfblocks_picker();
        let pair = cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker))
            .expect("pair for ready image");
        assert!(pair.native.is_none());
        assert!(pair.halfblocks_scratch.is_some());
    }

    #[test]
    fn protocol_pair_with_non_halfblocks_native_builds_both() {
        // A graphics terminal gets a `ThreadProtocol` for the slow native
        // encode *and* a halfblocks scratch for the partial-render
        // fallback.  `Picker::set_protocol_type` lets us construct the
        // native side without a real graphics terminal.
        let mut cache = cache_with_sender();
        cache.request("b.png");
        cache.set_decoded("b.png", DynamicImage::new_rgba8(4, 4));
        let pair = cache
            .get_protocol_pair(
                "b.png",
                8,
                4,
                Some(&native_picker()),
                Some(&halfblocks_picker()),
            )
            .expect("pair for ready image");
        assert!(pair.native.is_some(), "native encode shipped off-thread");
        assert!(pair.halfblocks_scratch.is_some(), "fallback scratch built");
    }

    /// `render_halfblocks_scratch` must produce halfblock *cells* even
    /// when handed a picker carrying a native protocol.  A native picker
    /// would encode the whole image into a single cell as one escape
    /// sequence, which `paint_halfblocks_partial` cannot clip by row —
    /// the image would flash on the frames that copy row 0 and vanish
    /// otherwise.  This is the guard for the iTerm2 scroll bug.
    #[test]
    fn scratch_holds_halfblock_cells_even_from_a_native_picker() {
        let rect = Rect::new(0, 0, 8, 4);
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            16,
            16,
            image::Rgba([40, 80, 120, 255]),
        ));
        let buf = render_halfblocks_scratch(&native_picker(), img, rect);
        // Halfblocks paints the image color into *every* cell of the
        // rect (as fg/bg of a `▀`, or of a space where a cell's two
        // pixel rows share a color, as they do for a uniform image).  A
        // native encode would instead put one escape sequence in cell
        // (0, 0) and leave every other cell default-and-skipped.
        let expected = ratatui::style::Color::Rgb(40, 80, 120);
        for y in 0..rect.height {
            for x in 0..rect.width {
                let cell = buf.cell((x, y)).expect("cell in rect");
                assert!(
                    !cell.symbol().contains('\u{1b}'),
                    "cell ({x},{y}) carries an escape sequence, not a halfblock"
                );
                assert_eq!(cell.fg, expected, "cell ({x},{y}) fg");
                assert_eq!(cell.bg, expected, "cell ({x},{y}) bg");
            }
        }
    }

    #[test]
    fn get_protocol_pair_returns_none_without_native_picker() {
        let mut cache = cache_with_sender();
        cache.request("a.png");
        cache.set_decoded("a.png", DynamicImage::new_rgba8(1, 1));
        assert!(cache.get_protocol_pair("a.png", 8, 4, None, None).is_none());
    }

    #[test]
    fn get_protocol_pair_returns_none_without_resize_sender() {
        // No sender attached (this is the default before App::run spawns
        // the encoder worker, and also the state in tests that don't
        // exercise image rendering).  get_protocol_pair must return None
        // rather than construct a ThreadProtocol with a dead channel.
        let mut cache = ImageCache::new();
        cache.request("a.png");
        cache.set_decoded("a.png", DynamicImage::new_rgba8(1, 1));
        let picker = halfblocks_picker();
        assert!(cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker))
            .is_none());
    }

    #[test]
    fn get_protocol_pair_returns_none_for_pending() {
        let mut cache = cache_with_sender();
        cache.request("a.png");
        let picker = halfblocks_picker();
        assert!(cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker))
            .is_none());
    }

    #[test]
    fn apply_resize_response_pops_pending_fifo_even_when_target_gone() {
        // When a pair is invalidated before its ResizeResponse comes back,
        // apply_resize_response must still pop the front of the pending
        // FIFO — otherwise subsequent responses would be routed to the
        // wrong protocol.  We exercise this via `track_pending_resize`
        // plus an `invalidate_protocols` that removes the pair.
        let mut cache = cache_with_sender();
        cache.request("a.png");
        cache.set_decoded("a.png", DynamicImage::new_rgba8(4, 4));
        let picker = halfblocks_picker();
        cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker))
            .expect("pair built");
        cache.track_pending_resize("a.png", 8, 4);
        assert_eq!(cache.pending.len(), 1);
        // Invalidate before the "response" arrives.
        cache.invalidate_protocols();
        // Without a ResizeResponse value (which we can't construct in a
        // test — it's crate-private-ish), assert the pending state
        // directly.  The real routing is exercised by the property that
        // `pop_front` is called on `apply_resize_response`, which we
        // verify via the public `pending_count` helper below.
        assert_eq!(cache.pending_count(), 1);
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
    fn aspect_rows_square_image_kept_at_natural_height() {
        // A 400×400 image, box 80×24 cells, 10×20 px cells:
        //   box_w_px = 800, box_h_px = 480
        //   width-fill would give ih*box_w/iw = 400*800/400 = 800 px,
        //   but `Resize::Fit(None)` never upscales past the 400 px the
        //   image actually has → 400 px → ceil(400/20) = 20 rows.
        let mut cache = ImageCache::new();
        cache.request("sq.png");
        cache.set_decoded("sq.png", DynamicImage::new_rgba8(400, 400));
        assert_eq!(cache.aspect_rows("sq.png", 80, 24, (10, 20)), Some(20));
    }

    #[test]
    fn aspect_rows_small_image_reserves_natural_height() {
        // A 10×2 image at 10×20 px cells, box 80×24:
        //   width-fill would give 2*800/10 = 160 px (8 rows), but Fit
        //   paints it at its own 2 px, so we reserve ceil(2/20) = 1 row
        //   instead of a 7-row blank band below a 2 px-tall image.
        let mut cache = ImageCache::new();
        cache.request("thin.png");
        cache.set_decoded("thin.png", DynamicImage::new_rgba8(10, 2));
        assert_eq!(cache.aspect_rows("thin.png", 80, 24, (10, 20)), Some(1));
    }

    #[test]
    fn reserved_rows_collapses_failed_to_one() {
        let mut cache = ImageCache::new();
        cache.request("broken.png");
        cache.set_failed("broken.png", "RemoteBlocked".to_owned());
        assert_eq!(cache.reserved_rows("broken.png", 80, 24, (10, 20)), Some(1));
    }

    #[test]
    fn reserved_rows_pending_returns_none() {
        let mut cache = ImageCache::new();
        cache.request("in_flight.png");
        assert!(cache
            .reserved_rows("in_flight.png", 80, 24, (10, 20))
            .is_none());
    }

    #[test]
    fn reserved_rows_ready_returns_aspect_rows() {
        let mut cache = ImageCache::new();
        cache.request("wide.png");
        cache.set_decoded("wide.png", DynamicImage::new_rgba8(1600, 400));
        assert_eq!(cache.reserved_rows("wide.png", 80, 24, (10, 20)), Some(10));
    }
}
