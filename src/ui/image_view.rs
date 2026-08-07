//! `ImageView` — per-frame layout snapshot and post-render overlay for
//! image blocks.
//!
//! Analogous to `ui::table_view`: the renderer still emits plain rendered
//! lines for each `Block::ImageBlock` (an `[Image: alt]` placeholder on
//! row 0 plus NBSP padding for the reserved area), and this module adds
//! two passes on top of the normal line-render loop:
//!
//!   1. **`build_snapshots`** — walks the visible rendered lines and
//!      returns one snapshot per visible image block, recording the
//!      screen rect the image will paint into.
//!   2. **`paint_images`** — for each snapshot, builds or reuses the
//!      cached `StatefulProtocol` from `EditorState::images` and calls
//!      `StatefulImage::render` onto the rect.  No-op for terminals
//!      without a detected image protocol — the `[Image: alt]`
//!      placeholder from the renderer stays visible in that case.

use std::ops::Range;

use ratatui::buffer::{Buffer as TuiBuf, Cell};
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui_image::{Resize, ResizeEncodeRender};

use crate::editor::EditorState;
use crate::image::{paint_halfblocks_partial, ImageCache, NativePaint};
use crate::terminal::ImageProtocol;

/// Per-frame geometry for one visible image block.  Screen coordinates
/// are in terminal cells, relative to the document area's origin.  Only
/// valid for the frame on which the snapshot was built.
#[derive(Debug, Clone)]
pub struct ImageLayoutSnapshot {
    /// Virtual-block index in the current `ParsedDoc::source_map`.
    pub block_idx: usize,
    /// Alt text, used for the fallback placeholder when the image can't
    /// be rendered. Currently consumed only by tests; the live placeholder
    /// path is in `ui::rendered_view`.
    #[allow(dead_code)]
    pub alt: String,
    /// URL as it appears in the Markdown source.  The key into
    /// `EditorState::images`.
    pub url: String,
    /// Screen rect (viewport-relative) occupied by the reserved image
    /// area.  Size is **stable** across scrolls: `width = area.width`,
    /// `height = image_max_height`.  Position (`y`) moves with scroll;
    /// the rect may overflow the viewport bounds — callers should check
    /// against the document area before painting so the image doesn't
    /// overwrite neighbouring widgets.
    pub rect: Rect,
    /// Intended top of the image in document-area-relative coordinates,
    /// including any negative offset for an image whose top has scrolled
    /// past the viewport.  Used by `paint_images` to decide whether the
    /// full rect is within the area.
    pub natural_top: isize,
}

/// What a `(col, row)` click falls on inside an image block.
///
/// Used by tests in this module; production code uses other hit-test
/// routines.  Kept as a concrete type so the surface is stable for when
/// click-on-image affordances land.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageHit {
    Body { block_idx: usize },
}

impl ImageLayoutSnapshot {
    /// Return the visible byte range of the image block's source (the
    /// `![alt](url)` line).  Used by tests in this module.
    #[allow(dead_code)]
    pub fn source_range(&self, state: &EditorState) -> Option<Range<usize>> {
        state
            .parsed
            .source_map
            .original_range_for_block(self.block_idx)
    }

    /// Used by tests in this module.
    #[allow(dead_code)]
    pub fn hit_test(&self, col: u16, row: u16) -> Option<ImageHit> {
        if col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
        {
            Some(ImageHit::Body {
                block_idx: self.block_idx,
            })
        } else {
            None
        }
    }
}

/// Scan the visible lines for `Block::ImageBlock`s and produce one
/// snapshot per image whose reserved rows intersect the viewport.
///
/// `scroll` is the number of visual rows skipped at the top; matches
/// `state.scroll` in rendered-edit mode or the preview's own scroll
/// offset.  `area` is the document area (editor content region — not
/// including status/hint bars).
///
/// The returned `rect` always has the image's **full** reserved size
/// (`image_max_height` rows × `area.width` cols), regardless of whether
/// part of it is scrolled off-screen.  This keeps the rect dimensions
/// stable across scrolls so `paint_images` can reuse the cached
/// `StatefulProtocol` encoding.  The rect's `y` may be negative
/// (represented as saturating to 0 within `area.y` bounds); callers
/// should check `rect` fits fully inside `area` before painting.
/// Refresh `snapshots` in place when the cache key (`scroll`, `area`,
/// `parsed_version`) differs from the previous frame's, otherwise leave
/// both the vector and the key untouched.  Caches the geometry scan so
/// idle redraws and non-layout-affecting events don't pay the
/// O(lines × images) cost every frame.
pub fn build_snapshots_cached(
    state: &EditorState,
    area: Rect,
    scroll: usize,
    snapshots: &mut Vec<ImageLayoutSnapshot>,
    cache_key: &mut Option<(usize, Rect, u64)>,
) {
    let key = (scroll, area, state.parsed_version);
    if *cache_key == Some(key) {
        return;
    }
    *snapshots = build_snapshots(state, area, scroll);
    *cache_key = Some(key);
}

pub fn build_snapshots(state: &EditorState, area: Rect, scroll: usize) -> Vec<ImageLayoutSnapshot> {
    let mut out = Vec::new();
    if area.height == 0 {
        return out;
    }
    let width = area.width as usize;
    let total_rows = state.parsed.total_visual_rows(width);
    if scroll >= total_rows {
        return out;
    }

    for info in &state.parsed.image_blocks {
        let rendered_range = state
            .parsed
            .source_map
            .rendered_lines_for_block(info.block_idx);
        if rendered_range.is_empty() {
            continue;
        }
        // Sum visual rows for lines `[scroll, rendered_range.start)` —
        // that's where the block's top row lands on screen.  Lines may
        // wrap; short placeholder-only lines do not, but preceding
        // content might.  Resolved via `ParsedDoc::visual_rows_before`
        // which is O(1) after the per-frame cache is populated; the
        // historical loop here re-invoked `visual_rows_for_line` for
        // every preceding line on every scroll tick.
        let block_top = state.parsed.visual_rows_before(rendered_range.start, width);
        let y_offset: isize = block_top as isize - scroll as isize;

        let reserved = rendered_range.end.saturating_sub(rendered_range.start) as isize;
        let image_top = area.y as isize + y_offset;
        let image_bottom = image_top + reserved;
        let viewport_top = area.y as isize;
        let viewport_bottom = (area.y as isize) + area.height as isize;
        // Skip entirely when not even a single row intersects the viewport.
        if image_bottom <= viewport_top || image_top >= viewport_bottom {
            continue;
        }

        // Clamp y to u16 for the Rect; paint_images will refuse to
        // render if the (uncropped) rect doesn't fully fit.
        let rect_y = image_top.max(0).min(u16::MAX as isize) as u16;
        out.push(ImageLayoutSnapshot {
            block_idx: info.block_idx,
            alt: info.alt.clone(),
            url: info.url.clone(),
            rect: Rect {
                x: area.x,
                y: rect_y,
                width: area.width,
                height: reserved.max(0).min(u16::MAX as isize) as u16,
            },
            natural_top: image_top,
        });
    }
    out
}

/// Bundle of inputs the `paint_images` pass needs.  Grouped into a
/// struct so the function signature stays readable — everything here is
/// already held by the App or the `EditorView` at call time.
pub struct PaintContext<'a> {
    /// Document area that image rects are relative to.
    pub area: Rect,
    /// Destination frame buffer.
    pub buf: &'a mut TuiBuf,
    /// Per-image protocol cache; mutated on cold-path (new url/size).
    pub images: &'a mut ImageCache,
    /// Native-protocol picker (e.g. Kitty / Sixel / iTerm2 / Halfblocks).
    pub native_picker: Option<&'a ratatui_image::picker::Picker>,
    /// Halfblocks-only picker — built from the same font-size as
    /// `native_picker`; used for the position-independent partial-render
    /// fallback.
    pub halfblocks_picker: Option<&'a ratatui_image::picker::Picker>,
    /// Detected native protocol (used to short-circuit the halfblocks
    /// fallback when the native is Kitty, which handles scrolling
    /// without re-encode).
    pub native_protocol: Option<ImageProtocol>,
    /// True while the scroll position has changed within the quiesce
    /// window (`App::is_scrolling`).  During this window, non-Kitty
    /// protocols fall back to halfblocks even when the image is fully
    /// visible — avoids per-frame re-encode flicker on scroll.
    pub is_scrolling: bool,
    /// True while a modal is open over the editor.  Forces every image
    /// to render via halfblocks so the buffer-based dim sweep
    /// (`ui::dim::dim_area`) actually recesses the image alongside the
    /// rest of the document — native graphics protocols (Sixel / Kitty
    /// / iTerm2) write past the ratatui cell buffer and would otherwise
    /// stay at full brightness behind the modal.
    pub modal_open: bool,
    /// Block index to skip (cursor's block during raw-reveal).
    pub suppress_block_idx: Option<usize>,
    /// Theme background color used (a) to clear the reserved rect
    /// before the protocol paints over it and (b) to substitute for
    /// `Color::Reset` cells produced by the halfblocks renderer.
    /// Without (b), letter-box cells in the scratch buffer would punch
    /// through to the terminal's own background — visible as `Reset`
    /// bands while scrolling and around any partially-visible image.
    pub bg: Color,
}

/// Render each image onto its reserved rect, overlaying the `[Image: alt]`
/// placeholder emitted by the text renderer.
///
/// The cache builds the halfblocks scratch synchronously on cold path,
/// so a halfblocks rendering of every decoded image is always available
/// as a fallback.  `native` (Kitty / Sixel / iTerm2) is encoded off-
/// thread by the worker and gated on `pair.native_ready`; until that
/// flag flips, we render halfblocks.
///
/// Decision per snapshot:
///
/// | Image state                                       | Rendering  |
/// |---------------------------------------------------|------------|
/// | Native picker IS halfblocks                       | scratch    |
/// | Native not ready yet                              | scratch    |
/// | Fully visible, not scrolling                      | native     |
/// | Fully visible, scrolling, native is Kitty         | native     |
/// | Fully visible, scrolling, native is Sixel/iTerm2  | scratch    |
/// | Partially visible (any state)                     | scratch    |
///
/// The halfblocks scratch path is a cell-copy from the pre-rendered
/// `Buffer` held on the pair, so per-frame cost is O(rect area) with no
/// encoding work.
pub fn paint_images(snapshots: &[ImageLayoutSnapshot], ctx: PaintContext) {
    if ctx.native_picker.is_none() || ctx.native_protocol.is_none() {
        return;
    }
    let viewport_top = ctx.area.y as isize;
    let viewport_bottom = (ctx.area.y as isize) + ctx.area.height as isize;

    for snap in snapshots {
        if Some(snap.block_idx) == ctx.suppress_block_idx {
            continue;
        }
        let top = snap.natural_top;
        let bottom = top + snap.rect.height as isize;
        // Skip anything that has no overlap with the viewport at all.
        if bottom <= viewport_top || top >= viewport_bottom {
            continue;
        }
        let fully_visible = top >= viewport_top && bottom <= viewport_bottom;

        // Ensure the protocol pair exists for this (url, w, h).  Cold
        // path builds the halfblocks scratch synchronously and a
        // ThreadProtocol for the native encode that is still in flight.
        if ctx
            .images
            .get_protocol_pair(
                &snap.url,
                snap.rect.width,
                snap.rect.height,
                ctx.native_picker,
                ctx.halfblocks_picker,
            )
            .is_none()
        {
            continue;
        }

        // Clear the visible portion of the reserved rect before the
        // protocol paints over it.  The text renderer emits an
        // `[Image: alt]` placeholder on row 0 plus NBSP padding; without
        // this clear, any cell of the reserved rect that the image
        // doesn't write to (letter-boxed area for non-square aspect,
        // halfblocks' position-dependent cell data, the trailing
        // padding right of a narrow image on row 0) keeps the
        // placeholder text visible behind the image — a "label
        // peeking out from behind the image" bug.  The clear is a
        // no-op for rows already blank.
        clear_visible_reserved_rect(snap, &ctx.area, ctx.buf, ctx.bg);

        // During active scroll, ALL protocols fall back to halfblocks.
        // Earlier revisions exempted Kitty here on the theory that its
        // virtual-placement protocol handles scroll without re-encoding,
        // but Ghostty (and apparently other Kitty-compatible terminals)
        // still re-composites the image at each new cell position — the
        // dominant source of scroll lag on image-heavy documents.
        // Halfblocks are position-independent cell content; ratatui's
        // diff emits only the changed cells, and the terminal treats them
        // like any other text.  The native protocol re-engages once
        // `SCROLL_QUIESCE` elapses (150 ms of no scroll input).
        let use_native = fully_visible && !ctx.is_scrolling && !ctx.modal_open;

        if use_native {
            paint_native(ctx.images, snap, ctx.buf, ctx.bg);
        } else {
            paint_scratch_partial(ctx.images, snap, &ctx.area, ctx.buf, ctx.bg);
        }
    }
}

/// Overwrite every cell of the on-screen slice of `snap.rect` with a
/// default (blank) cell, so any `[Image: alt]` placeholder text emitted
/// by the line renderer does not bleed through letter-box or trailing
/// cells left untouched by the image protocol.
///
/// Correctness constraints:
/// * Must be called AFTER the protocol-pair existence check — we only
///   want to clear when we're actually about to paint an image.  A
///   cleared rect with no overlay would leave a blank square instead of
///   the loading-state `[Image: alt]` placeholder.
/// * Clears only cells that overlap the document `area` — cells outside
///   belong to other widgets (status bar, hint line) and must be left
///   alone.  The vertical intersection matters because a snap whose top
///   is scrolled off-screen has `natural_top < area.y`.
fn clear_visible_reserved_rect(
    snap: &ImageLayoutSnapshot,
    area: &Rect,
    buf: &mut TuiBuf,
    bg: Color,
) {
    let viewport_top = area.y as isize;
    let viewport_bottom = viewport_top + area.height as isize;
    let top = snap.natural_top.max(viewport_top);
    let bottom = (snap.natural_top + snap.rect.height as isize).min(viewport_bottom);
    if bottom <= top {
        return;
    }
    let y_start = top as u16;
    let y_end = bottom as u16;
    let x_start = snap.rect.x;
    let x_end = snap.rect.x.saturating_add(snap.rect.width);
    for y in y_start..y_end {
        for x in x_start..x_end {
            if let Some(cell) = buf.cell_mut((x, y)) {
                *cell = Cell::default();
                cell.set_bg(bg);
            }
        }
    }
}

/// Render the pair's native `ThreadProtocol` into `buf`, shipping a
/// resize-encode request to the worker on the cold path and tracking it
/// on the cache's pending FIFO.  If the native protocol isn't yet
/// encoded (`native_ready == false`), falls back to the halfblocks
/// scratch so the user never sees a placeholder flash.
///
/// When the previous frame already transmitted this exact image at this
/// exact rect, the rect is marked `skip` instead of re-rendered so the
/// payload isn't sent again — see [`NativePaint`] for why that matters.
fn paint_native(images: &mut ImageCache, snap: &ImageLayoutSnapshot, buf: &mut TuiBuf, bg: Color) {
    let resize = Resize::Fit(None);
    let frame = images.frame_seq();
    let needs_encode = {
        let pair = match images.protocol_pair_mut(&snap.url, snap.rect.width, snap.rect.height) {
            Some(p) => p,
            None => return,
        };
        let full_rect = Rect::new(0, 0, snap.rect.width, snap.rect.height);
        let generation = pair.native_generation;
        // Reusable only if the previous frame left this exact encoding
        // at this exact rect on screen.  A one-frame gap means something
        // else painted here (scratch, a suppressed block, nothing at
        // all), so the terminal no longer holds the image.
        let already_on_screen = pair.last_native_paint
            == Some(NativePaint {
                rect: snap.rect,
                generation,
                frame: frame.wrapping_sub(1),
            });

        // No native (terminal's preferred protocol IS halfblocks) —
        // scratch IS the rendering.
        let Some(native) = pair.native.as_mut() else {
            pair.last_native_paint = None;
            if let Some(scratch) = pair.halfblocks_scratch.as_ref() {
                paint_halfblocks_partial(scratch, full_rect, 0, snap.rect, buf, bg);
            }
            return;
        };

        // Ship an encode request if needed.  ThreadProtocol::resize_encode
        // *takes* the inner StatefulProtocol and sends it to the worker;
        // render() is a silent no-op for as long as the response is in
        // flight.  ratatui-image 11 takes a `Size` (size-without-position)
        // here rather than a `Rect`; `Rect: Into<Size>` drops the origin.
        let new_size = native.needs_resize(&resize, snap.rect.into());
        let needs = new_size.is_some();
        if let Some(new_size) = new_size {
            native.resize_encode(&resize, new_size);
        }
        // `native_ready` alone is not enough to license a native render:
        // it latches on the first successful encode and is never cleared,
        // so on any frame that dispatches a *re*-encode (terminal resize,
        // a changed reserved height) the inner protocol has just been
        // moved to the worker and `render` would draw nothing at all —
        // over a rect `clear_visible_reserved_rect` just blanked, leaving
        // an empty hole until the response lands.  `protocol_type()`
        // returns `None` exactly when the inner protocol is away, so it
        // is the precise test for "can render right now".
        let inner_present = native.protocol_type().is_some();
        if pair.native_ready && inner_present {
            if already_on_screen {
                mark_rect_skipped(snap.rect, buf);
            } else {
                native.render(snap.rect, buf);
            }
            pair.last_native_paint = Some(NativePaint {
                rect: snap.rect,
                generation,
                frame,
            });
        } else {
            pair.last_native_paint = None;
            if let Some(scratch) = pair.halfblocks_scratch.as_ref() {
                paint_halfblocks_partial(scratch, full_rect, 0, snap.rect, buf, bg);
            }
        }
        needs
    };
    if needs_encode {
        images.track_pending_resize(&snap.url, snap.rect.width, snap.rect.height);
    }
}

/// Mark every cell of `rect` as skipped so `ratatui` emits nothing for
/// the region on this frame, leaving whatever the terminal already has
/// there — the previous frame's native image — undisturbed.
///
/// `clear_visible_reserved_rect` has already blanked these cells, which
/// is what a `skip` cell should carry: if a later frame stops skipping,
/// the diff sees a blank-vs-payload change and re-transmits.
///
/// **This makes the frame buffer deliberately lie** — it records blanks
/// over a region the terminal is actually showing an image in.  What
/// keeps that from stranding a ghost image is a property of ratatui's
/// hand-written `impl PartialEq for Cell`: it compares the `skip` flag
/// alongside symbol and style.  So the moment a frame stops skipping
/// these cells, they compare unequal to the skipped ones and are emitted
/// — even when both are blank.  That is what erases the image when its
/// rows scroll away into empty space, where a symbol-and-style-only
/// comparison would diff clean and leave the picture on screen.  The
/// dependency is load-bearing and lives upstream, so
/// `skipped_rect_still_diffs_against_the_same_cells_unskipped` pins it.
fn mark_rect_skipped(rect: Rect, buf: &mut TuiBuf) {
    for y in rect.y..rect.y.saturating_add(rect.height) {
        for x in rect.x..rect.x.saturating_add(rect.width) {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_skip(true);
            }
        }
    }
}

/// Cell-copy the halfblocks scratch into `buf`, clipping to the visible
/// portion of `area`.  Used whenever native isn't appropriate for this
/// frame (scrolling + non-Kitty, partial visibility, or native still
/// pre-encoding).
fn paint_scratch_partial(
    images: &mut ImageCache,
    snap: &ImageLayoutSnapshot,
    area: &Rect,
    buf: &mut TuiBuf,
    bg: Color,
) {
    let pair = match images.protocol_pair_mut(&snap.url, snap.rect.width, snap.rect.height) {
        Some(p) => p,
        None => return,
    };
    // Halfblock cells land on top of whatever native transmission was
    // there; the terminal no longer holds the image.
    pair.last_native_paint = None;
    let Some(scratch) = pair.halfblocks_scratch.as_ref() else {
        return;
    };

    let viewport_top = area.y as isize;
    let viewport_bottom = (area.y as isize) + area.height as isize;
    let top = snap.natural_top;
    let bottom = top + snap.rect.height as isize;
    let clip_top = if top < viewport_top {
        (viewport_top - top) as u16
    } else {
        0
    };
    let visible_top = top.max(viewport_top);
    let visible_bottom = bottom.min(viewport_bottom);
    if visible_bottom <= visible_top {
        return;
    }
    let visible_height = (visible_bottom - visible_top) as u16;
    let full_rect = Rect::new(0, 0, snap.rect.width, snap.rect.height);
    let dst_rect = Rect::new(
        snap.rect.x,
        visible_top as u16,
        snap.rect.width,
        visible_height,
    );
    paint_halfblocks_partial(scratch, full_rect, clip_top, dst_rect, buf, bg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::Buffer;
    use crate::editor::EditorState;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    // ── Native re-transmission suppression ───────────────────────────
    //
    // The iTerm2 and Sixel protocols put the whole base64 PNG in one
    // cell's `symbol`, and `Buffer::diff` treats that symbol's display
    // width as an invalidation run — so one image forces every later
    // cell, including a second image's payload, back into the diff on
    // every frame.  These tests pin the suppression that stops it; see
    // `image::cache::NativePaint`.

    mod native_reuse {
        use std::sync::mpsc;

        use image::{DynamicImage, RgbaImage};
        use ratatui::style::Color;
        use ratatui_image::picker::{Picker, ProtocolType};

        use super::super::*;
        use crate::terminal::ImageProtocol;

        const AREA: Rect = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 20,
        };

        /// An iTerm2 picker — the protocol whose `render` delivers the
        /// full PNG every time.  Constructed by stamping the protocol
        /// rather than probing, so the test is terminal-independent.
        #[allow(deprecated)]
        fn iterm2_picker() -> Picker {
            let mut picker = Picker::from_fontsize((1, 2).into());
            picker.set_protocol_type(ProtocolType::Iterm2);
            picker
        }

        #[allow(deprecated)]
        fn halfblocks_picker() -> Picker {
            let mut picker = Picker::from_fontsize((1, 2).into());
            picker.set_protocol_type(ProtocolType::Halfblocks);
            picker
        }

        fn snap(url: &str, top: u16, height: u16) -> ImageLayoutSnapshot {
            ImageLayoutSnapshot {
                block_idx: 0,
                alt: url.into(),
                url: url.into(),
                rect: Rect::new(0, top, AREA.width, height),
                natural_top: top as isize,
            }
        }

        struct Harness {
            images: ImageCache,
            rx: mpsc::Receiver<ratatui_image::thread::ResizeRequest>,
            native: Picker,
            halfblocks: Picker,
        }

        impl Harness {
            fn new(urls: &[&str]) -> Self {
                let (tx, rx) = mpsc::channel();
                let mut images = ImageCache::new();
                images.attach_resize_sender(tx);
                for url in urls {
                    images.request(url);
                    images.set_decoded(
                        url,
                        DynamicImage::ImageRgba8(RgbaImage::from_pixel(
                            64,
                            64,
                            image::Rgba([10, 200, 90, 255]),
                        )),
                    );
                }
                Self {
                    images,
                    rx,
                    native: iterm2_picker(),
                    halfblocks: halfblocks_picker(),
                }
            }

            /// Stand in for the encoder worker: perform every queued
            /// resize-encode synchronously and route the responses back.
            fn drain_encoder(&mut self) {
                while let Ok(req) = self.rx.try_recv() {
                    let resp = req.resize_encode().expect("encode succeeds");
                    self.images.apply_resize_response(resp);
                }
            }

            /// Draw one frame and return the resulting buffer.
            fn frame(&mut self, snaps: &[ImageLayoutSnapshot], scrolling: bool) -> TuiBuf {
                self.frame_suppressing(snaps, scrolling, None)
            }

            /// As [`Self::frame`], but with `suppress_block_idx` set —
            /// the raw-reveal path, which skips the block entirely
            /// rather than painting anything over its rect.
            fn frame_suppressing(
                &mut self,
                snaps: &[ImageLayoutSnapshot],
                scrolling: bool,
                suppress_block_idx: Option<usize>,
            ) -> TuiBuf {
                self.images.begin_frame();
                let mut buf = TuiBuf::empty(AREA);
                let ctx = PaintContext {
                    area: AREA,
                    buf: &mut buf,
                    images: &mut self.images,
                    native_picker: Some(&self.native),
                    halfblocks_picker: Some(&self.halfblocks),
                    native_protocol: Some(ImageProtocol::ITerm2),
                    is_scrolling: scrolling,
                    modal_open: false,
                    suppress_block_idx,
                    bg: Color::Reset,
                };
                paint_images(snaps, ctx);
                buf
            }
        }

        /// True when the cell at `rect`'s origin carries an iTerm2
        /// inline-image escape — i.e. the whole PNG was just handed to
        /// the terminal on this frame.
        fn transmitted(buf: &TuiBuf, rect: Rect) -> bool {
            buf.cell((rect.x, rect.y))
                .is_some_and(|c| c.symbol().contains("]1337;File="))
        }

        #[test]
        fn two_native_images_transmit_once_then_go_quiet() {
            let snaps = vec![snap("a.png", 0, 6), snap("b.png", 8, 6)];
            let mut h = Harness::new(&["a.png", "b.png"]);

            // Frame 1 builds the pairs and ships the encodes; the
            // protocols are away at the worker, so this frame paints
            // halfblocks.
            h.frame(&snaps, false);
            h.drain_encoder();

            // Frame 2 is the transmission: both payloads land in the
            // buffer.
            let transmit = h.frame(&snaps, false);
            assert!(
                transmitted(&transmit, snaps[0].rect),
                "first image should carry its base64 payload"
            );
            assert!(
                transmitted(&transmit, snaps[1].rect),
                "second image should carry its base64 payload"
            );

            // Frames 3 and 4 are idle redraws — a cursor blink, say.
            // Nothing may be re-sent, or iTerm2 blanks and repaints each
            // image (the ~2 Hz flicker this guards).
            let idle_a = h.frame(&snaps, false);
            let idle_b = h.frame(&snaps, false);
            for s in &snaps {
                assert!(
                    !transmitted(&idle_a, s.rect),
                    "idle frame re-sent the payload for {}",
                    s.url
                );
            }
            assert!(
                idle_a.diff(&idle_b).is_empty(),
                "two consecutive idle frames must produce no terminal output"
            );
        }

        #[test]
        fn a_scratch_frame_forces_the_next_native_frame_to_retransmit() {
            let snaps = vec![snap("a.png", 0, 6)];
            let mut h = Harness::new(&["a.png"]);
            h.frame(&snaps, false);
            h.drain_encoder();
            let transmit = h.frame(&snaps, false);
            assert!(transmitted(&transmit, snaps[0].rect));

            // A scroll frame paints halfblock cells over the region, so
            // the terminal no longer holds the image…
            let scratch = h.frame(&snaps, true);
            assert!(!transmitted(&scratch, snaps[0].rect));

            // …and the next settled frame must send it again rather than
            // skip onto a rect that now shows halfblocks.
            let resent = h.frame(&snaps, false);
            assert!(
                transmitted(&resent, snaps[0].rect),
                "native paint after a scratch frame must retransmit"
            );
        }

        /// The suppression relies on the frame-adjacency rule alone for
        /// any path that leaves the rect *unpainted* — `paint_images`
        /// `continue`s past a suppressed block without touching the
        /// record, and an off-screen image never reaches `paint_native`
        /// at all.  The renderer's `[Image: alt]` placeholder lands over
        /// the region on such a frame, so the terminal no longer holds
        /// the image and the next native frame must send it again.
        ///
        /// Distinct from the scratch case above, which
        /// `paint_scratch_partial` clears explicitly — this one would
        /// still pass if the `frame` field were dropped from the
        /// comparison, so it is pinned separately.
        #[test]
        fn a_suppressed_frame_forces_the_next_native_frame_to_retransmit() {
            let snaps = vec![snap("a.png", 0, 6)];
            let mut h = Harness::new(&["a.png"]);
            h.frame(&snaps, false);
            h.drain_encoder();
            assert!(transmitted(&h.frame(&snaps, false), snaps[0].rect));
            // Settled: the next frame skips rather than re-sending.
            assert!(!transmitted(&h.frame(&snaps, false), snaps[0].rect));

            // Raw-reveal on the image's own block: nothing is painted
            // over the rect, and the record is left untouched.
            let suppressed = h.frame_suppressing(&snaps, false, Some(snaps[0].block_idx));
            assert!(!transmitted(&suppressed, snaps[0].rect));

            assert!(
                transmitted(&h.frame(&snaps, false), snaps[0].rect),
                "native paint after a suppressed frame must retransmit"
            );
        }

        /// The skip marking makes the frame buffer claim the image's
        /// rows are blank while the terminal is still showing the image.
        /// Nothing would ever erase that image if a later blank frame
        /// diffed clean against the skipped one — which is exactly what
        /// happens when the image scrolls away into empty space below
        /// the end of the document.
        ///
        /// What saves it is that ratatui's `impl PartialEq for Cell`
        /// compares `skip` as a field, so blank-skipped and
        /// blank-unskipped cells are unequal and the blanks are emitted.
        /// That is an upstream implementation detail this module depends
        /// on, so assert it directly.
        #[test]
        fn skipped_rect_still_diffs_against_the_same_cells_unskipped() {
            let snaps = vec![snap("a.png", 0, 6)];
            let mut h = Harness::new(&["a.png"]);
            h.frame(&snaps, false);
            h.drain_encoder();
            h.frame(&snaps, false);
            let skipped = h.frame(&snaps, false);
            assert!(!transmitted(&skipped, snaps[0].rect));

            // The image is gone and its rows are now empty document
            // space: no snapshots, so nothing paints over the rect.
            let mut blank = TuiBuf::empty(AREA);
            for y in 0..AREA.height {
                for x in 0..AREA.width {
                    if let Some(cell) = blank.cell_mut((x, y)) {
                        cell.set_bg(Color::Reset);
                    }
                }
            }
            let cleared: Vec<_> = skipped
                .diff(&blank)
                .into_iter()
                .filter(|(x, y, _)| snaps[0].rect.contains((*x, *y).into()))
                .collect();
            assert_eq!(
                cleared.len(),
                (snaps[0].rect.width * snaps[0].rect.height) as usize,
                "every skipped cell must re-emit once it stops being skipped, \
                 otherwise the image is stranded on screen"
            );
        }

        #[test]
        fn invalidate_native_paints_forces_a_retransmit() {
            let snaps = vec![snap("a.png", 0, 6)];
            let mut h = Harness::new(&["a.png"]);
            h.frame(&snaps, false);
            h.drain_encoder();
            h.frame(&snaps, false);
            assert!(!transmitted(&h.frame(&snaps, false), snaps[0].rect));

            // A resize / `terminal.clear()` wipes the screen behind our
            // back; the record must not survive it.
            h.images.invalidate_native_paints();
            assert!(transmitted(&h.frame(&snaps, false), snaps[0].rect));
        }
    }

    fn state_from(src: &str, image_max_height: usize) -> EditorState {
        EditorState::new_with_config(Buffer::from_str(src), theme(), true, true, image_max_height)
    }

    #[test]
    fn build_snapshots_produces_one_snapshot_per_visible_image() {
        let src = "Intro.\n\n![cat](cat.png)\n\n![dog](dog.png)\n\nFin.\n";
        let state = state_from(src, 4);
        let area = Rect::new(0, 0, 20, 30);
        let snaps = build_snapshots(&state, area, 0);
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0].url, "cat.png");
        assert_eq!(snaps[1].url, "dog.png");
        // Each snapshot reserves `image_max_height` rows.
        assert_eq!(snaps[0].rect.height, 4);
        assert_eq!(snaps[1].rect.height, 4);
    }

    #[test]
    fn build_snapshots_cached_reuses_output_when_key_matches() {
        let src = "Intro.\n\n![cat](cat.png)\n\nOutro.\n";
        let state = state_from(src, 4);
        let area = Rect::new(0, 0, 20, 30);
        let mut snapshots = Vec::new();
        let mut key = None;
        build_snapshots_cached(&state, area, 0, &mut snapshots, &mut key);
        assert_eq!(snapshots.len(), 1);
        let populated_key = key;

        // Identical inputs → key preserved, snapshots still one entry.
        build_snapshots_cached(&state, area, 0, &mut snapshots, &mut key);
        assert_eq!(key, populated_key);
        assert_eq!(snapshots.len(), 1);

        // Scroll change → cache invalidates and repopulates.
        build_snapshots_cached(&state, area, 10, &mut snapshots, &mut key);
        assert_ne!(key, populated_key);
    }

    #[test]
    fn build_snapshots_skips_scrolled_off_blocks() {
        let src = "![a](a.png)\n\n![b](b.png)\n";
        let state = state_from(src, 3);
        let area = Rect::new(0, 0, 20, 30);
        // Scroll past the first image (3 rows) plus a blank gap line (1 row) = 4.
        let snaps = build_snapshots(&state, area, 4);
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].url, "b.png");
    }

    #[test]
    fn build_snapshots_keeps_full_reserved_height_for_overflow() {
        // Post-fix invariant: snap.rect.height is ALWAYS the full reserved
        // size (image_max_height), regardless of what fits in the
        // viewport.  paint_images then refuses to paint when the full
        // reserved rect doesn't fit, which is what keeps the cached
        // StatefulProtocol encoding stable across scrolls.
        let src = "![big](big.png)\n";
        let state = state_from(src, 20);
        let area = Rect::new(0, 0, 20, 5);
        let snaps = build_snapshots(&state, area, 0);
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].rect.height, 20);
    }

    #[test]
    fn hit_test_matches_inside_rect_only() {
        let snap = ImageLayoutSnapshot {
            block_idx: 3,
            alt: "x".into(),
            url: "x.png".into(),
            rect: Rect::new(2, 4, 10, 6),
            natural_top: 4,
        };
        assert_eq!(snap.hit_test(2, 4), Some(ImageHit::Body { block_idx: 3 }));
        assert_eq!(snap.hit_test(11, 9), Some(ImageHit::Body { block_idx: 3 }));
        assert_eq!(snap.hit_test(1, 4), None);
        assert_eq!(snap.hit_test(12, 9), None);
        assert_eq!(snap.hit_test(2, 10), None);
    }

    #[test]
    fn no_snapshots_for_empty_area() {
        let src = "![a](a.png)\n";
        let state = state_from(src, 3);
        let area = Rect::new(0, 0, 20, 0);
        assert!(build_snapshots(&state, area, 0).is_empty());
    }

    // ── clear_visible_reserved_rect ──────────────────────────────────

    fn snap_with_top(natural_top: isize, width: u16, height: u16) -> ImageLayoutSnapshot {
        ImageLayoutSnapshot {
            block_idx: 0,
            alt: "mermaid diagram".into(),
            url: "diagram-mermaid-deadbeef".into(),
            rect: Rect {
                x: 0,
                y: natural_top.max(0).min(u16::MAX as isize) as u16,
                width,
                height,
            },
            natural_top,
        }
    }

    /// Stand-in for the `[Image: alt]` placeholder text the line renderer
    /// emits on row 0 of an image block.  Pre-populates every cell in
    /// `area` with `placeholder_ch` so the clear's effect is observable.
    fn pre_populate_buf(area: Rect, placeholder_ch: char) -> TuiBuf {
        let mut buf = TuiBuf::empty(area);
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(placeholder_ch);
                }
            }
        }
        buf
    }

    #[test]
    fn clear_rect_blanks_every_cell_of_visible_reserved_area() {
        // Reserved rect fully inside the area — every cell should become
        // default (space).  This is the regression test for the "label
        // peeking out from behind the image" bug: without the clear, a
        // narrow image protocol leaves the placeholder text visible
        // where the image doesn't paint.
        let area = Rect::new(0, 0, 30, 20);
        let mut buf = pre_populate_buf(area, 'X');
        let snap = snap_with_top(2, 30, 4);
        clear_visible_reserved_rect(&snap, &area, &mut buf, Color::Reset);
        for y in 0..20u16 {
            for x in 0..30u16 {
                let expected = if (2..6).contains(&y) { ' ' } else { 'X' };
                assert_eq!(
                    buf.cell((x, y)).unwrap().symbol(),
                    expected.to_string(),
                    "cell ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn clear_rect_clips_to_area_when_scrolled_off_top() {
        // natural_top = -2: the top two rows of the reserved rect are
        // above the viewport.  The clear must only touch in-viewport
        // cells, leaving buf cells outside the area untouched (in this
        // test they're simulated by a smaller area).
        let area = Rect::new(0, 5, 30, 10);
        // Pre-populate the buf with Xs at every cell the buf knows about.
        let mut buf = pre_populate_buf(area, 'X');
        // snap top at row 3 (two above area.y=5); reserved height 6 →
        // visible rows 5..9.
        let snap = snap_with_top(3, 30, 6);
        clear_visible_reserved_rect(&snap, &area, &mut buf, Color::Reset);
        for y in 5..15u16 {
            for x in 0..30u16 {
                let expected = if (5..9).contains(&y) { ' ' } else { 'X' };
                assert_eq!(
                    buf.cell((x, y)).unwrap().symbol(),
                    expected.to_string(),
                    "cell ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn clear_rect_noop_when_snap_fully_above_area() {
        // natural_top = -10, height = 4 → reserved rows [-10, -6), no
        // overlap with area at y=0.  Nothing should be cleared.
        let area = Rect::new(0, 0, 10, 5);
        let mut buf = pre_populate_buf(area, 'X');
        let snap = snap_with_top(-10, 10, 4);
        clear_visible_reserved_rect(&snap, &area, &mut buf, Color::Reset);
        for y in 0..5u16 {
            for x in 0..10u16 {
                assert_eq!(buf.cell((x, y)).unwrap().symbol(), "X", "cell ({x},{y})");
            }
        }
    }
}
