//! Decoded-image cache retained across reparses; see docs/dev/media-export.md.
//!
//! `ParsedDoc` is rebuilt on every buffer mutation, so decoded bytes and their expensive
//! `StatefulProtocol` encodings live on `EditorState` keyed by URL instead.  Protocols are keyed
//! additionally by target cell dimensions, so a resize invalidates only the affected entries.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use image::DynamicImage;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::widgets::StatefulWidget;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::SlicedProtocol;
use ratatui_image::thread::{ResizeRequest, ResizeResponse, ThreadProtocol};
use ratatui_image::{Resize, StatefulImage};

use super::kitty_direct::{self, Geometry};

/// Encode `image` as halfblocks at `rect`, returning the rendered cells.
///
/// **Only `picker`'s font size is used; the protocol is forced to `Halfblocks`.** The scratch
/// must be position-independent cells so `paint_halfblocks_partial` can clip it by row, while
/// keeping the native protocol's aspect ratio so the image doesn't change shape crossing the
/// native↔halfblocks boundary mid-scroll.  A native encoding puts the whole image in one cell as
/// a single escape sequence, which cannot be clipped at all.
///
/// Cheap (low single-digit ms on pre-resized images), so either thread may call it; the decode
/// worker does so right after pre-resizing, leaving the UI thread's first paint a cache hit.
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

/// Whether `protocol` paints a band through [`SlicedProtocol`]: the two terminals whose band is a
/// *slice of what was already encoded* rather than a fresh encode per band — Kitty, whose
/// placeholders address image rows, and Sixel, whose bands are cut out of the payload at render
/// time.
///
/// One predicate rather than two `matches!`es, because [`build_sliced`]'s gate and the cold path's
/// claim of the prebuilt have to agree: a protocol that builds a band but is not recognized here
/// would build it, drop it, and paint the scratch anyway.
fn is_band_protocol(protocol: ProtocolType) -> bool {
    matches!(protocol, ProtocolType::Kitty | ProtocolType::Sixel)
}

/// Build the band protocol for `image` at `rect`, or `None` when `picker` speaks a protocol whose
/// band cannot be expressed this way, or the build fails.
///
/// Two protocols get their band from `SlicedProtocol`, for opposite reasons.  Kitty's unicode
/// placeholders address image rows by index, so the whole image is transmitted once and *any* row
/// range can be painted afterwards with no re-encode.  Sixel has no image store at all — every
/// sequence is drawn where it is sent — so its band is a re-slice of the encoded 6-px bands: still
/// no re-encode, only a shorter payload.  Either way a partially scrolled image stays at native
/// fidelity instead of falling back to halfblocks.  See
/// `docs/dev/plans/image-partial-rendering.md`.
///
/// The protocol-type check is not a formality: `SlicedProtocol::new_with_resize` dispatches on the
/// picker, so an iTerm2 or halfblocks picker would silently produce a different backend — one PNG
/// per text row, or a row copy — which `paint_images` would then paint as if it were a band.
///
/// Build it off the UI thread.  Kitty formats the entire transmit string synchronously out of raw
/// RGBA rather than PNG (`f=32,t=d`), so a full-width image is megabytes of base64; Sixel
/// re-encodes the pixels and splits the result into bands.  Both are far more work than the
/// halfblocks scratch this sits beside.
pub fn build_sliced(picker: &Picker, image: &DynamicImage, rect: Rect) -> Option<SlicedProtocol> {
    if !is_band_protocol(picker.protocol_type()) {
        return None;
    }
    match SlicedProtocol::new_with_resize(
        picker,
        image.clone(),
        Size::new(rect.width, rect.height),
        Resize::Fit(None),
    ) {
        Ok(sliced) => Some(sliced),
        Err(err) => {
            tracing::debug!(
                target: "image", %err,
                "band protocol build failed; the image will paint as halfblocks",
            );
            None
        }
    }
}

/// One image's Kitty *direct placement* backend at one cell size.
///
/// The band is a placement parameter rather than part of an encoding, so this holds nothing
/// that changes as the image scrolls: the transmit is written once, and every later frame is
/// one `a=p` escape naming a source rectangle.  That is what keeps a clipped image sharp
/// without a re-encode — and, unlike the iTerm2 route, without a re-send flash.
///
/// Built on the decode worker beside the scratch and the sliced protocol, for the same reason:
/// the resize plus the base64 of a raw-RGBA payload is far too much work for the UI thread.
pub struct DirectPlacement {
    /// The terminal-side image id, stable per URL so a rebuild re-transmits into the same slot
    /// instead of leaving the previous image resident.
    pub id: u32,
    /// The resized bitmap's geometry — its cell size, and one cell in pixels.
    pub geometry: Geometry,
    /// The one-time transmit, `Some` until the first placement has carried it.  Taken rather
    /// than cloned: a payload is megabytes, and writing it twice would double the frame.
    pub transmit: Option<String>,
}

/// Build the direct-placement backend for `image` at `rect`, or `None` when the geometry is
/// empty or the resize produced nothing to send.
///
/// The resize is deliberately the **same call the Kitty and iTerm2 paths make** — `Fit(None)`
/// with the default nearest filter — so an image does not change appearance crossing between
/// backends.  It also pads to a whole number of cells, which is what makes the band's source
/// rectangle an exact multiple of the cell height.
///
/// `url` supplies the image id ([`kitty_direct::image_id`]), so the id is a property of the
/// image rather than of this build.
pub fn build_direct_placement(
    url: &str,
    font_size: (u16, u16),
    image: &DynamicImage,
    rect: Rect,
) -> Option<DirectPlacement> {
    if rect.width == 0 || rect.height == 0 {
        return None;
    }
    let cells = Size::new(rect.width, rect.height);
    let font = ratatui_image::FontSize::new(font_size.0.max(1), font_size.1.max(1));
    // `None` background: the letterbox padding stays transparent, so what shows through is the
    // terminal's own background rather than a colour guessed from the theme.
    let resized = Resize::Fit(None).resize(image, font, cells, None);
    let geometry = Geometry::new(cells, font_size);
    let id = kitty_direct::image_id(url, geometry);
    let transmit = kitty_direct::transmit(id, &resized);
    if transmit.is_empty() {
        return None;
    }
    Some(DirectPlacement {
        id,
        geometry,
        transmit: Some(transmit),
    })
}

/// Free-function twin of [`ImageCache::aspect_rows`] over a borrowed image, for the decode
/// worker's scratch-height calculation.
///
/// Mirrors the paint path's `Resize::Fit(None)`, which scales down but never up, so the height is
/// capped at the image's own pixel height — without that cap a small image reserves the rows it
/// *would* fill at column width, leaving a blank band below it.
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
    let fitted_h_px = h_if_width_binds.min(u64::from(box_h_px)).min(u64::from(ih));
    let rows = fitted_h_px.div_ceil(u64::from(fh));
    (rows.clamp(1, u64::from(max_height_cells)) as usize).max(1)
}

/// Status of a decode attempt for a URL.
pub enum DecodeStatus {
    /// Decode in flight; `paint_images` shows the `[Image: alt]` placeholder meanwhile.
    Pending,
    /// Decode succeeded.  The pixels are kept in an `Arc` so a protocol can be rebuilt at a new
    /// size without re-running the slow PNG/JPEG decode or duplicating the bytes.
    Ready(Arc<DynamicImage>),
    /// Decode failed (IO, remote-blocked, corrupt bytes).  Never retried automatically.  The
    /// message is captured for future surfacing but has no consumer yet.
    Failed(#[allow(dead_code)] String),
}

/// Metadata for an in-flight resize-encode request.  ratatui-image's `ResizeResponse` carries
/// only a protocol-local id with no public accessor, so responses are routed back by keeping our
/// own FIFO — exact, because the worker is serial.
struct PendingResize {
    url: String,
    width: u16,
    height: u16,
}

/// Record of a native-protocol transmission that is *still on screen*, so an unchanged image at
/// an unchanged rect can be marked `skip` instead of re-rendered.
///
/// iTerm2 re-delivers the whole PNG on every render, writing the entire base64 payload into one
/// cell's `symbol`.  Re-emitting that is doubly wrong: the escape starts with an ECH sweep, so the
/// terminal blanks and redraws (a flash); and `Buffer::diff` carries
/// `invalidated = max(symbol.width(), invalidated) - 1` forward, so a 100 000-column payload
/// symbol forces **every** later cell — including another image's payload — to re-emit each
/// frame, which reads as a ~2 Hz flicker.
///
/// A record is honored only on the *immediately* following frame, so any frame that paints the
/// scratch there, suppresses the block, or scrolls it off screen invalidates it automatically.
/// Kitty and Sixel need none of it — both render through `paint_sliced`, whose payload lives in
/// the cell and is therefore diffed like any other content — but the bookkeeping is protocol-blind
/// and costs them nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativePaint {
    /// Screen rect the escape was rendered at.
    pub rect: Rect,
    /// `ProtocolPair::native_generation` at render time.
    pub generation: u64,
    /// `ImageCache::frame_seq` at render time.
    pub frame: u64,
}

/// Both encoded representations of one image at one target size.
///
/// `native` is the terminal's graphics protocol (Kitty / Sixel / iTerm2) in a `ThreadProtocol`,
/// so its first encode — up to hundreds of milliseconds — runs on the encoder worker.  It is
/// `None` when the detected protocol IS halfblocks, since the scratch is then the rendering.
///
/// `halfblocks_scratch` is built synchronously on the cold path (fast on pre-resized images) so a
/// finished decode shows immediately with no placeholder flash; paint upgrades to `native` once
/// `native_ready` is set by `apply_resize_response`.
pub struct ProtocolPair {
    pub native: Option<ThreadProtocol>,
    pub native_ready: bool,
    /// Bumped whenever the worker hands back freshly encoded native bytes; with the screen rect
    /// it identifies "the bytes currently on screen" for [`Self::last_native_paint`].
    ///
    /// Defense in depth: the branch that bumps it also clears `last_native_paint`, so
    /// `paint_native` never sees a mismatch today — it exists so a future path that swaps the
    /// bytes *without* clearing the record can't license a stale skip.
    pub native_generation: u64,
    /// The last frame the native escape was written on, and what it carried.  See [`NativePaint`].
    pub last_native_paint: Option<NativePaint>,
    /// Pre-rendered halfblocks cells for this `(url, width, height)`: the fallback rendering while
    /// `native` encodes, during scroll, and during partial visibility on a protocol that has no
    /// band of its own.
    pub halfblocks_scratch: Option<Buffer>,
    /// The band backend for this `(url, width, height)` — Kitty's row-addressed protocol, or
    /// Sixel's band-sliced one — when the terminal speaks either.  Unlike `native` this is not
    /// threaded: the build is long (megabytes of base64 for Kitty, a full re-encode for Sixel) but
    /// happens once, on the decode worker, so by the time a pair exists it is already there.
    ///
    /// `paint_images` paints every such image through this, at whatever band is on screen, and
    /// does not consult `native` or the scratch.  `None` for every other protocol, for a terminal
    /// whose build failed, and while the prebuilt has not arrived.
    pub sliced: Option<SlicedProtocol>,
    /// The direct-placement backend for this `(url, width, height)`, when the terminal places an
    /// already-transmitted image with a source rectangle.
    ///
    /// Mutually exclusive with `sliced`: a terminal that renders unicode placeholders gets that,
    /// one that does not gets this.  As with `sliced`, `native` stays `None` — there is no encoded
    /// payload to thread, and the iTerm2 route's per-frame re-send (and its flash) is exactly what
    /// this backend exists to avoid.
    pub kitty_direct: Option<DirectPlacement>,
}

/// Cache of decoded images + per-size protocol encodings.
#[derive(Default)]
pub struct ImageCache {
    /// URL → decode status.
    decoded: HashMap<String, DecodeStatus>,
    /// (URL, cell-width, cell-height) → encoded protocol pair, built lazily on first draw at that
    /// size.  A plain `HashMap` with no LRU: the working set is bounded by the visible images.
    protocols: HashMap<(String, u16, u16), ProtocolPair>,
    /// Halfblocks scratches pre-built on the decode worker, awaiting the `get_protocol_pair` call
    /// that claims them.  Entries that never match (terminal resized between decode and first
    /// paint) stay until `set_decoded` or `invalidate_protocols` clears them.
    prebuilt_scratches: HashMap<(String, u16, u16), Buffer>,
    /// The band counterpart: the row-addressed (Kitty) or band-sliced (Sixel) protocol, pre-built
    /// on the decode worker, claimed by the same `get_protocol_pair` call and stale on the same
    /// events.
    prebuilt_sliced: HashMap<(String, u16, u16), SlicedProtocol>,
    /// The direct-placement counterpart, claimed and staled the same way.
    prebuilt_direct: HashMap<(String, u16, u16), DirectPlacement>,
    /// `(image_id, placement_id)` pairs the terminal is showing, as of the last painted frame.
    ///
    /// Carried across frames because the snapshots of a frame that *stops* painting an image do
    /// not mention it — and a placement is anchored to screen cells, so one that is no longer
    /// painted has to be deleted explicitly or it stays behind while the document moves.  The pair
    /// is what makes that precise: one stored image can be placed by several blocks at once, and a
    /// block's index can move when the document is edited, so an id alone would delete a placement
    /// another block is still using.
    live_placements: HashSet<(u32, u32)>,
    /// Deletes owed to the terminal, waiting for a cell that will be emitted to carry them.
    pending_deletes: Vec<(u32, u32)>,
    /// Outstanding encode requests, FIFO in dispatch order.
    pending: VecDeque<PendingResize>,
    /// Sender into the encoder worker, cloned into each `ThreadProtocol`.  `None` disables image
    /// rendering entirely (tests, terminals without image support).
    resize_tx: Option<mpsc::Sender<ResizeRequest>>,
    /// Monotonic frame counter, bumped once per `terminal.draw`.  See [`NativePaint`].
    frame_seq: u64,
}

impl ImageCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the encoder worker's sender, from `App::run` once the worker is spawned.
    ///
    /// Doing so drops every cached protocol: their internal sender is tied to the old channel and
    /// would ship requests into a dead endpoint.  Decoded pixels are retained.
    pub fn attach_resize_sender(&mut self, tx: mpsc::Sender<ResizeRequest>) {
        self.resize_tx = Some(tx);
        self.protocols.clear();
        self.pending.clear();
        self.prebuilt_scratches.clear();
        self.prebuilt_sliced.clear();
        self.prebuilt_direct.clear();
    }

    /// Whether an encoder-worker sender has been attached.  Exists for the App-level test that a
    /// document swap re-attaches it — without one every image paints as a placeholder.
    pub fn has_resize_sender(&self) -> bool {
        self.resize_tx.is_some()
    }

    // ── Native-transmission bookkeeping ───────────────────────────────

    /// Advance the frame counter.  Driven from `App::draw_frame`, *not* the paint pass: Raw and
    /// Diff modes draw without painting images, and counting only painted frames would make a
    /// pre-Raw transmission look adjacent to the first frame back in Rendered.
    pub fn begin_frame(&mut self) {
        self.frame_seq = self.frame_seq.wrapping_add(1);
    }

    pub fn frame_seq(&self) -> u64 {
        self.frame_seq
    }

    /// Forget every recorded native transmission, forcing a re-transmit on the next paint.  For
    /// anything that invalidates the screen outside the paint pass: a resize, or the
    /// `terminal.clear()` after the external editor returns.
    pub fn invalidate_native_paints(&mut self) {
        for pair in self.protocols.values_mut() {
            pair.last_native_paint = None;
        }
    }

    // ── Direct-placement bookkeeping ──────────────────────────────────

    /// Record the placements painted on this frame and queue a delete for every one that was
    /// live before and is not now.
    ///
    /// Called once per frame that painted, with the `(image_id, placement_id)` pairs placed on it —
    /// the set difference is what catches a block that was edited away or navigated off, which no
    /// later snapshot mentions.
    pub fn reconcile_placements(&mut self, placed: &[(u32, u32)]) {
        let placed: HashSet<(u32, u32)> = placed.iter().copied().collect();
        for pair in self.live_placements.difference(&placed) {
            self.pending_deletes.push(*pair);
        }
        self.live_placements = placed;
    }

    /// Take the queued deletes, for the caller to write into a cell that will be emitted.
    ///
    /// Taken rather than read: the escapes are carried by a cell whose symbol changes only
    /// because they were appended, so leaving them in place would stop the diff from emitting
    /// the next frame's carrier at all.
    pub fn take_pending_deletes(&mut self) -> Vec<(u32, u32)> {
        std::mem::take(&mut self.pending_deletes)
    }

    /// Number of deletes waiting for a carrier.  Used by tests.
    #[allow(dead_code)]
    pub fn pending_deletes(&self) -> usize {
        self.pending_deletes.len()
    }

    /// Mark `url` as `Pending` iff it has no prior entry, returning true when a decode job should
    /// be dispatched.  A `Ready` or `Failed` URL is a no-op: there is no auto-retry.
    pub fn request(&mut self, url: &str) -> bool {
        if self.decoded.contains_key(url) {
            return false;
        }
        self.decoded.insert(url.to_owned(), DecodeStatus::Pending);
        true
    }

    /// Record a successful decode.  Used by tests; production goes through
    /// [`Self::set_decoded_with_prebuilt`] so the halfblocks scratch is captured too.
    #[allow(dead_code)]
    pub fn set_decoded(&mut self, url: &str, image: DynamicImage) {
        self.set_decoded_with_prebuilt(url, image, None, None, None);
    }

    /// [`Self::set_decoded`] plus a halfblocks scratch the decode worker already rendered; the
    /// next `get_protocol_pair` at the same dims claims it instead of encoding on the UI thread.
    pub fn set_decoded_with_prebuilt(
        &mut self,
        url: &str,
        image: DynamicImage,
        prebuilt_scratch: Option<(Rect, Buffer)>,
        prebuilt_sliced: Option<(Rect, SlicedProtocol)>,
        prebuilt_direct: Option<(Rect, DirectPlacement)>,
    ) {
        self.decoded
            .insert(url.to_owned(), DecodeStatus::Ready(Arc::new(image)));
        self.protocols.retain(|(u, _, _), _| u != url);
        self.prebuilt_scratches.retain(|(u, _, _), _| u != url);
        self.prebuilt_sliced.retain(|(u, _, _), _| u != url);
        self.prebuilt_direct.retain(|(u, _, _), _| u != url);
        if let Some((rect, buf)) = prebuilt_scratch {
            self.prebuilt_scratches
                .insert((url.to_owned(), rect.width, rect.height), buf);
        }
        if let Some((rect, sliced)) = prebuilt_sliced {
            self.prebuilt_sliced
                .insert((url.to_owned(), rect.width, rect.height), sliced);
        }
        if let Some((rect, direct)) = prebuilt_direct {
            self.prebuilt_direct
                .insert((url.to_owned(), rect.width, rect.height), direct);
        }
    }

    /// Record a decode failure.
    pub fn set_failed(&mut self, url: &str, message: String) {
        self.decoded
            .insert(url.to_owned(), DecodeStatus::Failed(message));
        self.protocols.retain(|(u, _, _), _| u != url);
        self.prebuilt_scratches.retain(|(u, _, _), _| u != url);
        self.prebuilt_sliced.retain(|(u, _, _), _| u != url);
        self.prebuilt_direct.retain(|(u, _, _), _| u != url);
    }

    /// Look up the decode status for `url`.  Used by integration tests.
    #[allow(dead_code)]
    pub fn status(&self, url: &str) -> Option<&DecodeStatus> {
        self.decoded.get(url)
    }

    /// Mutable protocol pair for `(url, width, height)`, building the async native protocol and
    /// the sync halfblocks scratch on a cold miss.
    ///
    /// `None` when the URL is `Pending` or `Failed`, when no `native_picker` is supplied (no image
    /// support), or when no `resize_tx` is attached.  When the native protocol IS halfblocks the
    /// pair's `native` stays `None` — the scratch is both the preferred and fallback rendering.
    /// On Kitty and Sixel it stays `None` too: `sliced` is the rendering there, and because it
    /// carries the whole image, any visible band can be painted from it without a re-encode.
    ///
    /// `direct` says the terminal renders by placing an already-transmitted image with a source
    /// rectangle, so the pair's `kitty_direct` is the rendering and nothing is encoded for the
    /// worker.  It is a bool rather than an `ImageProtocol` because `image` sits below
    /// `terminal` in the layer order and must not name its types.
    pub fn get_protocol_pair(
        &mut self,
        url: &str,
        width: u16,
        height: u16,
        native_picker: Option<&Picker>,
        halfblocks_picker: Option<&Picker>,
        direct: bool,
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

            // Prefer the worker's prebuilt scratch, taken by `remove` so it isn't held twice.
            let halfblocks_scratch = if let Some(buf) = self.prebuilt_scratches.remove(&key) {
                Some(buf)
            } else {
                // Cold-path fallback: the prebuilt is missing, or keyed to other dims.  A resize
                // is the usual cause — `on_resize` does not clear this map and `request` is a
                // no-op once a URL is decoded, so nothing ever re-derives it.  Timed rather than
                // assumed; see `docs/dev/plans/image-partial-rendering.md` § Rebuild triggers.
                let sync_picker = if is_halfblocks_native {
                    Some(native_picker)
                } else {
                    halfblocks_picker
                };
                let started = Instant::now();
                let buf = sync_picker
                    .map(|p| render_halfblocks_scratch(p, (*image_arc).clone(), full_rect));
                tracing::debug!(
                    target: "image",
                    url = %key.0,
                    width,
                    height,
                    micros = started.elapsed().as_micros() as u64,
                    "halfblocks scratch built synchronously (prebuilt missed)",
                );
                buf
            };

            // The band protocol, claimed the same way.  A miss here is a long synchronous build —
            // for Kitty the transmit string is megabytes of base64, for Sixel a full re-encode —
            // the same accepted cost as the scratch above, and reachable only when the geometry
            // changed since the decode, since the renderer's reserved height is otherwise exactly
            // the height the dispatch built at.  `!direct` because a kitty/Ghostty picker routed to
            // direct placement still reports `Kitty` here, and its rendering is `kitty_direct`
            // below — building the sliced backend too would be a megabytes payload nothing paints.
            let is_band_native = is_band_protocol(native_picker.protocol_type()) && !direct;
            let sliced = if is_band_native {
                match self.prebuilt_sliced.remove(&key) {
                    Some(sliced) => Some(sliced),
                    None => {
                        let started = Instant::now();
                        let built = build_sliced(native_picker, &image_arc, full_rect);
                        tracing::debug!(
                            target: "image",
                            url = %key.0,
                            width,
                            height,
                            micros = started.elapsed().as_micros() as u64,
                            ok = built.is_some(),
                            "band protocol built synchronously (prebuilt missed)",
                        );
                        built
                    }
                }
            } else {
                None
            };

            // The direct-placement backend, claimed the same way.  Its build is a resize plus the
            // base64 of a raw-RGBA payload — the same order as the scratch above, and the same
            // accepted synchronous cost on a geometry change.
            let kitty_direct = if direct {
                match self.prebuilt_direct.remove(&key) {
                    Some(built) => Some(built),
                    None => {
                        let started = Instant::now();
                        let font = native_picker.font_size();
                        let built = build_direct_placement(
                            &key.0,
                            (font.width, font.height),
                            &image_arc,
                            full_rect,
                        );
                        tracing::debug!(
                            target: "image",
                            url = %key.0,
                            width,
                            height,
                            micros = started.elapsed().as_micros() as u64,
                            ok = built.is_some(),
                            "direct placement built synchronously (prebuilt missed)",
                        );
                        built
                    }
                }
            } else {
                None
            };

            // A ThreadProtocol runs the slow native encode on the worker.  Three cases skip it: the
            // native protocol IS halfblocks, where the scratch above is the rendering; Kitty and
            // Sixel, whose rendering is the band above — for those a threaded encode would be a
            // second copy of the same bytes, never read; and direct placement, which has no
            // encoded payload at all.
            let native = if is_halfblocks_native || is_band_native || direct {
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
                    native_generation: 0,
                    last_native_paint: None,
                    halfblocks_scratch,
                    sliced,
                    kitty_direct,
                },
            );
        }
        self.protocols.get_mut(&key)
    }

    /// Look up an existing pair without `get_protocol_pair`'s Picker-dependent cold-path
    /// rebuild.  Callers should have ensured it exists earlier in the same frame.
    pub fn protocol_pair_mut(
        &mut self,
        url: &str,
        width: u16,
        height: u16,
    ) -> Option<&mut ProtocolPair> {
        self.protocols.get_mut(&(url.to_owned(), width, height))
    }

    /// Record a `resize_encode` request dispatched to the encoder worker, so
    /// [`Self::apply_resize_response`] can route its response back.
    pub fn track_pending_resize(&mut self, url: &str, width: u16, height: u16) {
        self.pending.push_back(PendingResize {
            url: url.to_owned(),
            width,
            height,
        });
    }

    /// Drop the oldest pending entry without applying a response — for a worker error, where the
    /// FIFO must still advance so the next response lines up with its originating protocol.
    pub fn drop_pending_front(&mut self) {
        self.pending.pop_front();
    }

    /// Route an encoded `ResizeResponse` back to its originating `ThreadProtocol` by popping the
    /// oldest pending entry; the worker is serial, so the orders match.
    ///
    /// A response whose pair has since been dropped is discarded, and
    /// `update_resized_protocol` additionally rejects responses superseded by a later request.
    pub fn apply_resize_response(&mut self, resp: ResizeResponse) {
        let Some(pending) = self.pending.pop_front() else {
            return;
        };
        let key = (pending.url, pending.width, pending.height);
        if let Some(pair) = self.protocols.get_mut(&key) {
            if let Some(native) = pair.native.as_mut() {
                if native.update_resized_protocol(resp) {
                    pair.native_ready = true;
                    // New bytes: whatever is on screen is stale, so the next paint must transmit.
                    pair.native_generation = pair.native_generation.wrapping_add(1);
                    pair.last_native_paint = None;
                }
            }
        }
    }

    /// Drop every protocol entry, e.g. on terminal resize.  Pending requests stay queued; their
    /// responses become orphan pops that `apply_resize_response` discards.  Used by tests.
    #[allow(dead_code)]
    pub fn invalidate_protocols(&mut self) {
        self.protocols.clear();
        // Prebuilt scratches are keyed by the old dims, so they are stale after a resize.
        self.prebuilt_scratches.clear();
        self.prebuilt_sliced.clear();
        self.prebuilt_direct.clear();
    }

    /// Rows a decoded image occupies when fitted into `max_width_cells × max_height_cells` at
    /// `font_size` pixels per cell.  `None` for anything but a `Ready` decode — both `Pending`
    /// *and* `Failed` answer `None`, unlike [`Self::reserved_rows`], which collapses `Failed` to
    /// the single placeholder row.  The math preview band relies on that difference: an invalid
    /// formula (a `Failed` decode) must hold the block's last resolved height rather than snap to
    /// one row mid-typing, so it asks here, not through `reserved_rows`.
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

    /// Rows the renderer should reserve for this image's block: the fitted height when `Ready`,
    /// `Some(1)` when `Failed` (collapsing to the placeholder row), and `None` while `Pending`,
    /// where the renderer falls back to `image_max_height` so layout stays stable.
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

    /// Clear `Failed` entries so a later `request` retries — for when the user promotes the
    /// remote-image policy and a `RemoteBlocked` failure could now succeed.
    pub fn clear_failures_for_remote_reopening(&mut self) {
        self.decoded
            .retain(|_, status| !matches!(status, DecodeStatus::Failed(_)));
    }

    /// Drop one URL's entries so a later `request` treats it as never seen.  Used when a
    /// worker's result arrives that the *current* settings forbid (the worker captured the policy
    /// at spawn time); the per-frame dispatch re-resolves it if the settings permit again.
    pub fn forget(&mut self, url: &str) {
        self.decoded.remove(url);
        self.protocols.retain(|(u, _, _), _| u != url);
        self.prebuilt_scratches.retain(|(u, _, _), _| u != url);
        self.prebuilt_sliced.retain(|(u, _, _), _| u != url);
        self.prebuilt_direct.retain(|(u, _, _), _| u != url);
    }

    /// Drop every entry for a remote URL, so the next dispatch re-resolves it under a changed
    /// remote-image policy: decoded images disappear when it tightens, memoized `RemoteBlocked`
    /// failures retry when it loosens.
    pub fn evict_remote(&mut self) {
        self.decoded
            .retain(|url, _| !crate::image::loader::is_remote(url));
        self.protocols
            .retain(|(url, _, _), _| !crate::image::loader::is_remote(url));
        self.prebuilt_scratches
            .retain(|(url, _, _), _| !crate::image::loader::is_remote(url));
        self.prebuilt_sliced
            .retain(|(url, _, _), _| !crate::image::loader::is_remote(url));
        self.prebuilt_direct
            .retain(|(url, _, _), _| !crate::image::loader::is_remote(url));
        // `pending` is deliberately untouched: responses for evicted URLs become orphan pops,
        // which is what keeps the FIFO pairing correct.
    }

    /// Drop every entry whose URL is not in `live`, run after each reparse.  Editing inside a
    /// mermaid block mints a new synthetic URL each time, so without this the maps grow unbounded.
    pub fn gc(&mut self, live: &std::collections::HashSet<String>) {
        self.decoded.retain(|url, _| live.contains(url));
        self.protocols.retain(|(url, _, _), _| live.contains(url));
        self.prebuilt_scratches
            .retain(|(url, _, _), _| live.contains(url));
        self.prebuilt_sliced
            .retain(|(url, _, _), _| live.contains(url));
        self.prebuilt_direct
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

    #[cfg(test)]
    pub fn prebuilt_sliced_count(&self) -> usize {
        self.prebuilt_sliced.len()
    }

    #[cfg(test)]
    pub fn prebuilt_direct_count(&self) -> usize {
        self.prebuilt_direct.len()
    }
}

#[cfg(test)]
// `Picker::from_fontsize` is deprecated in ratatui-image 9, but `Picker::halfblocks` can't set a
// specific font size, which several assertions below depend on.
#[allow(deprecated)]
mod tests {
    use super::*;

    /// A picker guaranteed to encode **halfblocks** at a known font size.  The bare
    /// `Picker::from_fontsize` infers its protocol from `$TERM_PROGRAM` / `$LC_TERMINAL`, so
    /// tests built on it pass or fail depending on the terminal `cargo test` ran from.
    fn halfblocks_picker() -> Picker {
        let mut picker = Picker::from_fontsize((1, 2).into());
        picker.set_protocol_type(ProtocolType::Halfblocks);
        picker
    }

    /// A deliberately non-halfblocks picker, so the native-plus-scratch branch is exercisable on
    /// any machine.  iTerm2 is the cheapest native protocol to encode and needs no real support.
    fn native_picker() -> Picker {
        let mut picker = Picker::from_fontsize((1, 2).into());
        picker.set_protocol_type(ProtocolType::Iterm2);
        picker
    }

    /// A picker whose placeholders address image rows, so `build_sliced` produces a backend for it.
    fn kitty_picker() -> Picker {
        let mut picker = Picker::from_fontsize((1, 2).into());
        picker.set_protocol_type(ProtocolType::Kitty);
        picker
    }

    /// A picker whose band is a re-slice of the encoded 6-px bands — Windows Terminal 1.22+ and
    /// the other sixel terminals, which have no image store and so no band parameter either.
    fn sixel_picker() -> Picker {
        let mut picker = Picker::from_fontsize((1, 2).into());
        picker.set_protocol_type(ProtocolType::Sixel);
        picker
    }

    // ── aspect_rows_of ────────────────────────────────────────────────

    #[test]
    fn small_image_reserves_natural_height_not_width_filled() {
        // A 190×65 logo in a 640×640 px box: width-filled it would be 14 rows, but Fit never
        // upscales, so it paints at its natural 65 px and must reserve only that.
        let img = DynamicImage::new_rgba8(190, 65);
        let rows = aspect_rows_of(&img, 80, 40, (8, 16));
        assert_eq!(rows, 65_u32.div_ceil(16) as usize);
    }

    #[test]
    fn wide_image_still_binds_on_width() {
        // Wider than the column, so Fit downscales to the width and the natural-height cap must
        // not interfere: 1080 * 640 / 1920 = 360 px → ceil(360/16) = 23 rows.
        let img = DynamicImage::new_rgba8(1920, 1080);
        let rows = aspect_rows_of(&img, 80, 40, (8, 16));
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

    /// A cache whose resize channel stays alive (the receiver is leaked, since only the
    /// channel's liveness matters) so `get_protocol_pair` can clone the sender.
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
            .get_protocol_pair("a.png", 10, 10, Some(&picker), Some(&picker), false)
            .is_some());
        assert_eq!(cache.protocol_count(), 1);
        cache.set_decoded("a.png", DynamicImage::new_rgba8(2, 2));
        assert_eq!(cache.protocol_count(), 0);
    }

    #[test]
    fn set_decoded_with_prebuilt_stashes_scratch_for_matching_dims() {
        let mut cache = cache_with_sender();
        cache.request("a.png");
        let rect = Rect::new(0, 0, 8, 4);
        let prebuilt = Buffer::empty(rect);
        cache.set_decoded_with_prebuilt(
            "a.png",
            DynamicImage::new_rgba8(8, 4),
            Some((rect, prebuilt)),
            None,
            None,
        );
        assert_eq!(cache.prebuilt_scratch_count(), 1);

        let picker = halfblocks_picker();
        let pair = cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker), false)
            .expect("pair for ready image");
        assert!(pair.halfblocks_scratch.is_some());
        // Prebuilt map was drained.
        assert_eq!(cache.prebuilt_scratch_count(), 0);
    }

    #[test]
    fn set_decoded_with_prebuilt_falls_back_to_sync_on_mismatched_dims() {
        // A request at different dims (terminal resized between decode and first paint) misses
        // the prebuilt entry, runs the sync render, and leaves the prebuilt in place.
        let mut cache = cache_with_sender();
        cache.request("a.png");
        let prebuilt_rect = Rect::new(0, 0, 8, 4);
        let prebuilt = Buffer::empty(prebuilt_rect);
        cache.set_decoded_with_prebuilt(
            "a.png",
            DynamicImage::new_rgba8(8, 4),
            Some((prebuilt_rect, prebuilt)),
            None,
            None,
        );

        let picker = halfblocks_picker();
        let pair = cache
            .get_protocol_pair("a.png", 16, 4, Some(&picker), Some(&picker), false)
            .expect("pair for ready image");
        assert!(pair.halfblocks_scratch.is_some());
        // The un-claimed prebuilt remains, for a future paint at matching dims.
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
            None,
            None,
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
            .get_protocol_pair("a.png", 1, 1, Some(&picker), Some(&picker), false)
            .expect("pair for ready image");
        cache.invalidate_protocols();
        assert_eq!(cache.protocol_count(), 0);
        assert!(matches!(
            cache.status("a.png"),
            Some(DecodeStatus::Ready(_))
        ));
    }

    #[test]
    fn protocol_pair_from_halfblocks_native_skips_native_thread_protocol() {
        let mut cache = cache_with_sender();
        cache.request("a.png");
        cache.set_decoded("a.png", DynamicImage::new_rgba8(4, 4));
        let picker = halfblocks_picker();
        let pair = cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker), false)
            .expect("pair for ready image");
        assert!(pair.native.is_none());
        assert!(pair.halfblocks_scratch.is_some());
    }

    #[test]
    fn protocol_pair_with_non_halfblocks_native_builds_both() {
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
                false,
            )
            .expect("pair for ready image");
        assert!(pair.native.is_some(), "native encode shipped off-thread");
        assert!(pair.halfblocks_scratch.is_some(), "fallback scratch built");
    }

    /// Guard for the iTerm2 scroll bug: a native picker must still yield halfblock *cells*, or
    /// the whole image lands in one unclippable escape sequence and flickers during scroll.
    #[test]
    fn scratch_holds_halfblock_cells_even_from_a_native_picker() {
        let rect = Rect::new(0, 0, 8, 4);
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            16,
            16,
            image::Rgba([40, 80, 120, 255]),
        ));
        let buf = render_halfblocks_scratch(&native_picker(), img, rect);
        // Halfblocks paints the color into *every* cell; a native encode would put one escape
        // sequence in cell (0, 0) and leave the rest default-and-skipped.
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
        assert!(cache
            .get_protocol_pair("a.png", 8, 4, None, None, false)
            .is_none());
    }

    // ── Band protocols (Kitty row addressing, Sixel band slicing) ─────

    #[test]
    fn build_sliced_answers_for_the_band_protocols_only() {
        let image = DynamicImage::new_rgba8(8, 8);
        let rect = Rect::new(0, 0, 4, 2);
        assert!(build_sliced(&kitty_picker(), &image, rect).is_some());
        assert!(build_sliced(&sixel_picker(), &image, rect).is_some());
        // The sliced backend dispatches on the picker, so a non-band picker would silently
        // produce a different one — a list of one PNG per text row for iTerm2, a row copy for
        // halfblocks — and `paint_images` would then paint it as if it were a band.
        assert!(build_sliced(&halfblocks_picker(), &image, rect).is_none());
        assert!(build_sliced(&native_picker(), &image, rect).is_none());
    }

    #[test]
    fn kitty_prebuilt_is_claimed_at_matching_dims_and_skips_the_threaded_protocol() {
        let mut cache = cache_with_sender();
        cache.request("a.png");
        let picker = kitty_picker();
        let rect = Rect::new(0, 0, 8, 4);
        let sliced = build_sliced(&picker, &DynamicImage::new_rgba8(8, 8), rect).expect("sliced");
        cache.set_decoded_with_prebuilt(
            "a.png",
            DynamicImage::new_rgba8(8, 4),
            None,
            Some((rect, sliced)),
            None,
        );
        assert_eq!(cache.prebuilt_sliced_count(), 1);

        let pair = cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker), false)
            .expect("pair for ready image");
        assert!(pair.sliced.is_some(), "the prebuilt was claimed");
        assert!(
            pair.native.is_none(),
            "the band is the rendering; a threaded protocol would duplicate the payload"
        );
        assert_eq!(cache.prebuilt_sliced_count(), 0, "the prebuilt was drained");
    }

    /// The Sixel side of the same contract: a sixel terminal gets the band backend and no threaded
    /// encode, which is what makes a partly visible image sharp on Windows Terminal instead of a
    /// halfblocks mosaic.
    #[test]
    fn sixel_prebuilt_is_claimed_before_it_becomes_a_threaded_encode() {
        let mut cache = cache_with_sender();
        cache.request("a.png");
        let picker = sixel_picker();
        let rect = Rect::new(0, 0, 8, 4);
        let sliced = build_sliced(&picker, &DynamicImage::new_rgba8(8, 8), rect).expect("sliced");
        cache.set_decoded_with_prebuilt(
            "a.png",
            DynamicImage::new_rgba8(8, 4),
            None,
            Some((rect, sliced)),
            None,
        );

        let pair = cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker), false)
            .expect("pair for ready image");
        assert!(pair.sliced.is_some(), "the prebuilt was claimed");
        assert!(
            pair.native.is_none(),
            "a second sixel encode on the worker would never be read"
        );
        assert_eq!(cache.prebuilt_sliced_count(), 0, "the prebuilt was drained");
    }

    /// A kitty/Ghostty picker routed to direct placement still reports `Kitty`, so the `!direct`
    /// guard is what stops it building the sliced backend too — its rendering is `kitty_direct`, and
    /// a second megabytes payload would be built on the worker and never painted.
    #[test]
    fn the_direct_route_builds_the_placement_and_not_the_sliced_backend() {
        let mut cache = cache_with_sender();
        cache.request("a.png");
        cache.set_decoded("a.png", DynamicImage::new_rgba8(8, 8));
        let picker = kitty_picker();
        let pair = cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker), true)
            .expect("pair for ready image");
        assert!(
            pair.kitty_direct.is_some(),
            "direct placement is the rendering"
        );
        assert!(
            pair.sliced.is_none(),
            "the sliced backend must not be built under direct placement"
        );
        assert!(
            pair.native.is_none(),
            "no threaded encode for direct placement"
        );
    }

    /// An unclaimed direct-placement prebuilt must be reaped like its sibling maps.  `gc` is the
    /// unbounded-growth guard for the churning URLs of a mermaid block, and each entry pins a
    /// megabytes-sized transmit string, so a leak here is the costliest of the three.
    #[test]
    fn gc_reaps_an_unclaimed_prebuilt_direct() {
        let mut cache = cache_with_sender();
        cache.request("a.png");
        let rect = Rect::new(0, 0, 8, 4);
        let direct = build_direct_placement("a.png", (1, 2), &DynamicImage::new_rgba8(8, 8), rect)
            .expect("direct placement");
        cache.set_decoded_with_prebuilt(
            "a.png",
            DynamicImage::new_rgba8(8, 4),
            None,
            None,
            Some((rect, direct)),
        );
        assert_eq!(cache.prebuilt_direct_count(), 1);

        cache.gc(&std::collections::HashSet::new());
        assert_eq!(
            cache.prebuilt_direct_count(),
            0,
            "a URL no longer live must drop its prebuilt direct placement"
        );
    }

    #[test]
    fn get_protocol_pair_returns_none_without_resize_sender() {
        // With no sender (the default before `App::run` spawns the worker), the pair must be
        // `None` rather than a `ThreadProtocol` over a dead channel.
        let mut cache = ImageCache::new();
        cache.request("a.png");
        cache.set_decoded("a.png", DynamicImage::new_rgba8(1, 1));
        let picker = halfblocks_picker();
        assert!(cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker), false)
            .is_none());
    }

    #[test]
    fn get_protocol_pair_returns_none_for_pending() {
        let mut cache = cache_with_sender();
        cache.request("a.png");
        let picker = halfblocks_picker();
        assert!(cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker), false)
            .is_none());
    }

    #[test]
    fn apply_resize_response_pops_pending_fifo_even_when_target_gone() {
        // A pair invalidated before its response returns must still pop the FIFO, or later
        // responses route to the wrong protocol.
        let mut cache = cache_with_sender();
        cache.request("a.png");
        cache.set_decoded("a.png", DynamicImage::new_rgba8(4, 4));
        let picker = halfblocks_picker();
        cache
            .get_protocol_pair("a.png", 8, 4, Some(&picker), Some(&picker), false)
            .expect("pair built");
        cache.track_pending_resize("a.png", 8, 4);
        assert_eq!(cache.pending.len(), 1);
        cache.invalidate_protocols();
        // A `ResizeResponse` can't be constructed in a test, so assert the pending state directly.
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
        // 1600×400 in an 800×480 px box: width binds at 200 px → ceil(200/20) = 10 rows.
        let mut cache = ImageCache::new();
        cache.request("wide.png");
        cache.set_decoded("wide.png", DynamicImage::new_rgba8(1600, 400));
        assert_eq!(cache.aspect_rows("wide.png", 80, 24, (10, 20)), Some(10));
    }

    #[test]
    fn aspect_rows_tall_image_clamps_to_max_height() {
        // Height binds and overflows max_height, so the row count clamps.
        let mut cache = ImageCache::new();
        cache.request("tall.png");
        cache.set_decoded("tall.png", DynamicImage::new_rgba8(400, 1600));
        assert_eq!(cache.aspect_rows("tall.png", 80, 24, (10, 20)), Some(24));
    }

    #[test]
    fn aspect_rows_square_image_kept_at_natural_height() {
        // Width-fill would give 800 px, but Fit never upscales past the image's own 400 px →
        // ceil(400/20) = 20 rows.
        let mut cache = ImageCache::new();
        cache.request("sq.png");
        cache.set_decoded("sq.png", DynamicImage::new_rgba8(400, 400));
        assert_eq!(cache.aspect_rows("sq.png", 80, 24, (10, 20)), Some(20));
    }

    #[test]
    fn aspect_rows_small_image_reserves_natural_height() {
        // Width-fill would give 8 rows; Fit paints the 2 px image at natural size, so 1 row.
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
