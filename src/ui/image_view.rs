//! Per-frame layout snapshot and post-render overlay for image blocks.
//!
//! The renderer still emits plain lines for each `Block::ImageBlock` — an `[Image: alt]`
//! placeholder plus NBSP padding — and this module adds two passes on top:
//! [`build_snapshots`] records the screen rect each visible image will paint into, and
//! [`paint_images`] renders the cached protocol onto it.  Without a detected image protocol the
//! second pass is a no-op and the placeholder stays visible.

use std::ops::Range;

use ratatui::buffer::{Buffer as TuiBuf, Cell, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;
use ratatui_image::{Resize, ResizeEncodeRender};

use crate::diff::DiffState;
use crate::editor::EditorState;
use crate::image::{
    paint_halfblocks_partial, ImageCache, NativePaint, SignedPosition, SlicedImage,
};
use crate::terminal::ImageProtocol;

/// Per-frame geometry for one visible image block, in terminal cells relative to the document
/// area's origin.  Valid only for the frame it was built on.
#[derive(Debug, Clone)]
pub struct ImageLayoutSnapshot {
    /// Virtual-block index in the current `ParsedDoc::source_map`.
    pub block_idx: usize,
    /// Alt text for the fallback placeholder.  Consumed only by tests; the live placeholder
    /// path is in `ui::rendered_view`.
    #[allow(dead_code)]
    pub alt: String,
    /// URL as written in the source; the key into `EditorState::images`.
    pub url: String,
    /// The reserved image area, viewport-relative.  Its size is **stable** across scrolls
    /// (`image_max_height` rows × `area.width`) while `y` moves, so the rect may overflow the
    /// viewport — check it against the document area before painting.
    pub rect: Rect,
    /// Intended top, document-area-relative, staying *negative* for an image scrolled off the
    /// top so `paint_images` can tell partial visibility from full.
    pub natural_top: isize,
}

/// What a `(col, row)` click falls on inside an image block.  Used only by this module's
/// tests today; kept for when click-on-image affordances land.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageHit {
    Body { block_idx: usize },
}

impl ImageLayoutSnapshot {
    /// Visible byte range of the image block's `![alt](url)` source line.  Tests only.
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

/// One snapshot per `Block::ImageBlock` whose reserved rows intersect the viewport.
///
/// `scroll` is the visual rows skipped at the top; `area` is the document area, excluding the
/// status / hint bars.  The returned `rect` always carries the image's **full** reserved size
/// even when partly scrolled off, keeping the dimensions stable across scrolls so
/// `paint_images` can reuse the cached `StatefulProtocol` encoding.
///
/// The cached wrapper refreshes `snapshots` in place only when the key (`scroll`, `area`,
/// `parsed_version`) changes, so idle redraws don't pay the O(lines × images) scan.
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

    for (ordinal, info) in state.parsed.image_blocks.iter().enumerate() {
        let rendered_range = state
            .parsed
            .source_map
            .rendered_lines_for_block(info.block_idx);
        if rendered_range.is_empty() {
            continue;
        }
        // Where the block's top row lands on screen.  `visual_rows_before` is O(1) once the
        // per-frame cache is warm; a loop here would re-measure every preceding line per tick.
        let block_top = state.parsed.visual_rows_before(rendered_range.start, width);
        let y_offset: isize = block_top as isize - scroll as isize;

        let reserved = rendered_range.end.saturating_sub(rendered_range.start) as isize;
        // A `$$...$$` block mid raw-reveal, with the preview on, reserves a
        // live-preview band for the formula PLUS `raw_rows` source lines
        // (see `ImageReveal::preview_rows`).  The formula paints in the
        // band at the block's TOP — the same rows it occupied before the
        // reveal, so the image doesn't jump — and the renderer paints the
        // editable source in the rows below.  So the image rect keeps the
        // block's top edge and shrinks to the band height (`reserved` minus
        // the source rows painted beneath).  With the preview off the image
        // is suppressed entirely (see `EditorView`) and this loop never
        // reaches it while revealed.
        let source_rows_below = match state.image_reveal.as_ref() {
            Some(reveal)
                if reveal.preview_rows > 0
                    && reveal.ordinal == ordinal
                    && reveal.url == info.url =>
            {
                reveal.rows as isize
            }
            _ => 0,
        };
        let image_top = area.y as isize + y_offset;
        let image_bottom = image_top + reserved - source_rows_below;
        let viewport_top = area.y as isize;
        let viewport_bottom = (area.y as isize) + area.height as isize;
        // Not even one row intersects the viewport.
        if image_bottom <= viewport_top || image_top >= viewport_bottom {
            continue;
        }

        // Clamp y to u16; `paint_images` refuses an uncropped rect that doesn't fully fit.
        let rect_y = image_top.max(0).min(u16::MAX as isize) as u16;
        out.push(ImageLayoutSnapshot {
            block_idx: info.block_idx,
            alt: info.alt.clone(),
            url: info.url.clone(),
            rect: Rect {
                x: area.x,
                y: rect_y,
                width: area.width,
                height: (reserved - source_rows_below).max(0).min(u16::MAX as isize) as u16,
            },
            natural_top: image_top,
        });
    }
    out
}

/// Diff-mode counterpart of [`build_snapshots_cached`].
///
/// Keys on `DiffState::layout_version`, not `EditorState::parsed_version`: the geometry comes
/// from the diff layout, and the editor's parse tracks a different document.
pub fn build_diff_snapshots_cached(
    diff: &DiffState,
    area: Rect,
    scroll: usize,
    snapshots: &mut Vec<ImageLayoutSnapshot>,
    cache_key: &mut Option<(usize, Rect, u64)>,
) {
    let key = (scroll, area, diff.layout_version());
    if *cache_key == Some(key) {
        return;
    }
    *snapshots = build_diff_snapshots(diff, area, scroll);
    *cache_key = Some(key);
}

/// Geometry for the images visible in a diff review's *clean* regions.
///
/// Mirrors [`build_snapshots`] but takes its row arithmetic from the diff layout's
/// `VisualRowCache`: in diff mode `scroll` counts diff visual rows, preceded by raw hunk rows
/// the editor's parse knows nothing about.  An image in a *changed* region has no
/// `ContextRendered` row, so it yields no snapshot and reserves nothing — as intended.
///
/// The `isize` arithmetic is load-bearing for the same reason as in [`build_snapshots`].
pub fn build_diff_snapshots(
    diff: &DiffState,
    area: Rect,
    scroll: usize,
) -> Vec<ImageLayoutSnapshot> {
    let mut out = Vec::new();
    if area.height == 0 {
        return out;
    }
    let Some(parsed) = diff.parsed_new.as_ref() else {
        return out;
    };
    let width = area.width as usize;
    diff.with_layout_index(width, |_lines, rc, index| {
        if scroll >= rc.total() {
            return;
        }
        for info in &parsed.image_blocks {
            let rendered_range = parsed.source_map.rendered_lines_for_block(info.block_idx);
            if rendered_range.is_empty() {
                continue;
            }
            // No `ContextRendered` entry ⇒ the block sits in a raw region.
            let (Some(&first), Some(&last)) = (
                index.get(&rendered_range.start),
                index.get(&(rendered_range.end - 1)),
            ) else {
                continue;
            };
            let block_top = rc.before(first);
            let y_offset: isize = block_top as isize - scroll as isize;
            // Measured the way the row cache does, so the rect matches the reserved rows.
            let reserved = rc.before(last + 1).saturating_sub(block_top) as isize;
            let image_top = area.y as isize + y_offset;
            let image_bottom = image_top + reserved;
            let viewport_top = area.y as isize;
            let viewport_bottom = (area.y as isize) + area.height as isize;
            if image_bottom <= viewport_top || image_top >= viewport_bottom {
                continue;
            }
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
    });
    out
}

/// Inputs for the [`paint_images`] pass, grouped so the signature stays readable.
pub struct PaintContext<'a> {
    /// Document area that image rects are relative to.
    pub area: Rect,
    /// Destination frame buffer.
    pub buf: &'a mut TuiBuf,
    /// Per-image protocol cache; mutated on cold-path (new url/size).
    pub images: &'a mut ImageCache,
    /// Native-protocol picker (e.g. Kitty / Sixel / iTerm2 / Halfblocks).
    pub native_picker: Option<&'a ratatui_image::picker::Picker>,
    /// Halfblocks-only picker, same font size as `native_picker`; the position-independent
    /// fallback.
    pub halfblocks_picker: Option<&'a ratatui_image::picker::Picker>,
    /// Detected native protocol.
    pub native_protocol: Option<ImageProtocol>,
    /// Inside the post-scroll quiesce window, during which every protocol falls back to
    /// halfblocks to avoid per-frame re-encode flicker.
    pub is_scrolling: bool,
    /// A modal is open: force halfblocks so the buffer-based dim sweep recesses the image too.
    /// Native protocols write past the ratatui cell buffer and would stay at full brightness.
    pub modal_open: bool,
    /// Block index to skip (cursor's block during raw-reveal).
    pub suppress_block_idx: Option<usize>,
    /// Theme background: clears the reserved rect before painting, and substitutes for the
    /// halfblocks renderer's `Color::Reset` cells — without which letter-box cells punch
    /// through to the terminal's own background as visible bands.
    pub bg: Color,
}

/// Render each image onto its reserved rect, over the `[Image: alt]` placeholder.
///
/// The cache builds the halfblocks scratch synchronously, so a fallback is always available;
/// `native` is encoded off-thread and gated on `pair.native_ready`.  Per snapshot:
///
/// | Image state                                       | Rendering          |
/// |---------------------------------------------------|--------------------|
/// | Native picker IS halfblocks                       | scratch            |
/// | Kitty or Sixel, at rest, no modal                 | the visible band   |
/// | Native not ready yet                              | scratch            |
/// | Fully visible, not scrolling                      | native             |
/// | Scrolling, or a modal is open                     | scratch            |
/// | Partially visible, protocol with no band          | scratch            |
///
/// The band is one interface with two protocol backends: Kitty addresses image rows with unicode
/// placeholders, Sixel re-slices its 6-px bands, and neither re-encodes per band.  The scratch path
/// is a cell-copy from the pre-rendered `Buffer` on the pair, so it costs O(rect area) with no
/// encoding.
///
/// **`fully_visible` does not apply to a band protocol.** Kitty carries every image row and
/// addresses them by index, Sixel carries every band and re-emits only the visible ones, so a
/// clipped image is a slice of what is already encoded rather than a reason to downgrade — which is
/// the whole fix for images going blurry the moment they are not fully on screen.  The scroll and
/// modal gates do still apply, for the reasons on each branch below.
pub fn paint_images(snapshots: &[ImageLayoutSnapshot], ctx: PaintContext) {
    if ctx.native_picker.is_none() || ctx.native_protocol.is_none() {
        return;
    }
    let viewport_top = ctx.area.y as isize;
    let viewport_bottom = (ctx.area.y as isize) + ctx.area.height as isize;
    // The terminal renders by placing an already-transmitted image, so the pair's `kitty_direct`
    // is the rendering and no encoded payload is wanted.
    let direct = ctx.native_protocol == Some(ImageProtocol::KittyDirect);
    // The `(image_id, placement_id)` pairs placed on this frame, reconciled against the last
    // frame's at the end.
    let mut placed: Vec<(u32, u32)> = Vec::new();

    for snap in snapshots {
        if Some(snap.block_idx) == ctx.suppress_block_idx {
            continue;
        }
        let top = snap.natural_top;
        let bottom = top + snap.rect.height as isize;
        // No overlap with the viewport at all.
        if bottom <= viewport_top || top >= viewport_bottom {
            continue;
        }
        let fully_visible = top >= viewport_top && bottom <= viewport_bottom;

        // Cold path builds the scratch synchronously plus a ThreadProtocol for the in-flight
        // native encode.
        if ctx
            .images
            .get_protocol_pair(
                &snap.url,
                snap.rect.width,
                snap.rect.height,
                ctx.native_picker,
                ctx.halfblocks_picker,
                direct,
            )
            .is_none()
        {
            continue;
        }

        // Without this, any reserved cell the image doesn't write to — letter-boxing, the
        // padding right of a narrow image — keeps the `[Image: alt]` placeholder visible
        // behind the image.
        clear_visible_reserved_rect(snap, &ctx.area, ctx.buf, ctx.bg);

        // Direct placement paints the same band as Kitty's row addressing, under the same modal
        // gate — `dim_area` cannot recess an image that writes past the cell buffer — but **not**
        // under the scroll gate.  A placement is one short escape naming a new source rectangle,
        // while the halfblocks fallback it would fall back to writes every cell of the band: on
        // this side of the wire a moving image is *cheaper* to keep placing than to downgrade, and
        // what the scroll gate exists to avoid is the terminal re-compositing the image at its new
        // cell position, which is WezTerm's cost to pay and is being measured.  It is also what
        // *deletes* the placement: an id not placed on a frame is reconciled away below.
        if direct && !ctx.modal_open {
            if let Some(id) = paint_direct_placement(ctx.images, snap, &ctx.area, ctx.buf) {
                placed.push(id);
                continue;
            }
        }

        // Kitty and Sixel paint the visible band instead, which is what keeps a clipped image
        // sharp.  Both still yield to the scroll window (the gate below) and to an open modal:
        // Kitty's placeholders re-composite wherever they move, so painting them on every scroll
        // frame is the lag the scratch window exists to avoid, and `dim_area` cannot recess an
        // image that writes past the cell buffer.  Sixel's band is re-emitted rather than
        // re-encoded, but the terminal still re-rasterises the payload, so it pays the same gate.
        if matches!(
            ctx.native_protocol,
            Some(ImageProtocol::KittyGraphics | ImageProtocol::Sixel)
        ) && !ctx.is_scrolling
            && !ctx.modal_open
            && paint_sliced(ctx.images, snap, &ctx.area, ctx.buf)
        {
            continue;
        }

        // During scroll every protocol falls back to halfblocks, band protocols included — Ghostty
        // and other Kitty-compatible terminals re-composite at each new cell position, the dominant
        // source of scroll lag on image-heavy documents.  Halfblocks are position-independent, so
        // ratatui's diff emits only changed cells.  Native re-engages once `SCROLL_QUIESCE`
        // elapses; for a band protocol that is the band above, which returns with no re-encode.
        let use_native = fully_visible && !ctx.is_scrolling && !ctx.modal_open;

        if use_native {
            paint_native(ctx.images, snap, ctx.buf, ctx.bg);
        } else {
            paint_scratch_partial(ctx.images, snap, &ctx.area, ctx.buf, ctx.bg);
        }
    }

    if direct {
        // A placement is anchored to *screen cells*, so one that is no longer painted has to be
        // deleted or it stays on screen while the document moves out from under it.  The set
        // difference is over ids rather than rects, which is also what catches a block that was
        // edited away: no snapshot mentions it any more, so nothing else could notice.
        ctx.images.reconcile_placements(&placed);
        let deletes = ctx.images.take_pending_deletes();
        if !deletes.is_empty() {
            write_control_escapes(ctx.buf, &ctx.area, &deletes);
        }
    }
}

/// Paint the on-screen band of an image through its sliced protocol — Kitty's row addressing or
/// Sixel's band slicing — returning whether it painted.  `false` means no band backend is cached for
/// this geometry — the terminal speaks neither protocol, its build failed, or the prebuilt has not
/// arrived — and the caller falls back to the scratch.
///
/// The two protocols differ in *how* the band reaches the terminal, not in how it is computed, and
/// `SlicedImage` hides the difference: Kitty addresses image rows through its placeholder grid, so
/// the band costs a different starting row and nothing is re-sent; Sixel splices the payload's 6-px
/// bands and re-emits the payload, so the band costs a shorter symbol on the same cell.  Either way
/// `SlicedImage` derives it from the `area` it is handed plus a signed position: the band as `area`
/// and `-skip` as the position make it paint exactly the rows below the clip and drop the rest, with
/// no re-encode.
///
/// Note it treats `area` as the *clipping window*, not as the document rect.  That is also why a
/// shrunken reserved rect — the `$$...$$` live-preview band — needs no mechanism of its own.
///
/// Sixel needs no re-send accounting either: its payload *is* the cell's symbol, so ratatui's diff
/// emits it exactly when the band changes, and the terminal holds the rasterised rows in its text
/// buffer in between (Windows Terminal stores one `ImageSlice` per row and clips with the viewport).
fn paint_sliced(
    images: &mut ImageCache,
    snap: &ImageLayoutSnapshot,
    area: &Rect,
    buf: &mut TuiBuf,
) -> bool {
    let pair = match images.protocol_pair_mut(&snap.url, snap.rect.width, snap.rect.height) {
        Some(pair) => pair,
        None => return false,
    };
    let Some(sliced) = pair.sliced.as_ref() else {
        return false;
    };
    let Some((skip, dst)) = image_band(
        snap.rect.x,
        snap.rect.width,
        snap.natural_top,
        snap.rect.height,
        area,
    ) else {
        // No overlap with the viewport: nothing to paint, but nothing left for the caller either.
        return true;
    };
    // `SignedPosition` carries an `i16`.  A skip that large needs `images.max_height` in the tens
    // of thousands — pathological, but wrapping the cast would paint the *wrong rows* silently, so
    // fall back to the scratch instead.
    let Ok(skip) = i16::try_from(skip) else {
        return false;
    };
    SlicedImage::new(sliced, SignedPosition { x: 0, y: -skip }).render(dst, buf);
    true
}

/// A cell whose symbol carries an escape sequence rather than text, so `Buffer::diff` must treat it
/// as one column: the diff advances by `cell_width()`, and an escape-laden symbol measures tens of
/// columns of printable base64.  Without this, the row after such a cell loses those columns from
/// the update.
const UNIT_WIDTH: ratatui::buffer::CellDiffOption =
    ratatui::buffer::CellDiffOption::ForcedWidth(std::num::NonZeroU16::new(1).unwrap());

/// Paint the on-screen band of an image by *placing* it, returning the block's
/// `(image_id, placement_id)` when it painted.
///
/// `None` means the caller should paint the scratch instead: no pair at this geometry, no
/// direct-placement backend on it, or a band that does not meet the viewport at all.
///
/// The band is one placement — `a=p` with a source rectangle — so exactly one cell carries a
/// symbol.  The transmit rides it on the image's first placement and is *taken* rather than
/// cloned, because a payload is megabytes and the next frame would otherwise repeat them.  The
/// placement id is the block's, so a document showing the same image twice gets two placements
/// (both visible) and a block that moves replaces its own previous placement rather than a
/// sibling's.
///
/// Every other cell of the band is marked `Skip`: they have been blanked by
/// [`clear_visible_reserved_rect`], but emitting those blanks would paint a background over the
/// image this placement is drawing.  What keeps *stale* glyphs out from under it — the
/// `[Image: alt]` placeholder, above all — is the erase sweep the symbol begins with, on the
/// terminal side, which no buffer-side trick could do: a glyph draws above an image.
fn paint_direct_placement(
    images: &mut ImageCache,
    snap: &ImageLayoutSnapshot,
    area: &Rect,
    buf: &mut TuiBuf,
) -> Option<(u32, u32)> {
    let pair = images.protocol_pair_mut(&snap.url, snap.rect.width, snap.rect.height)?;
    let direct = pair.kitty_direct.as_mut()?;
    let (skip, dst) = image_band(
        snap.rect.x,
        snap.rect.width,
        snap.natural_top,
        snap.rect.height,
        area,
    )?;

    // The carrier cell has to exist before the transmit is taken: taking it and then failing to
    // write the symbol would drop the payload for good.
    let cell = buf.cell_mut((dst.x, dst.y))?;
    let placement_id = crate::image::kitty_direct::placement_id(snap.block_idx);
    let transmit = direct.transmit.take();
    let symbol = crate::image::kitty_direct::place(
        direct.id,
        placement_id,
        direct.geometry,
        skip,
        dst,
        transmit.as_deref(),
    );
    cell.set_symbol(&symbol).set_diff_option(UNIT_WIDTH);

    for row in 0..dst.height {
        for col in 0..dst.width {
            if row == 0 && col == 0 {
                continue;
            }
            if let Some(cell) = buf.cell_mut((dst.x + col, dst.y + row)) {
                cell.set_diff_option(CellDiffOption::Skip);
            }
        }
    }
    Some((direct.id, placement_id))
}

/// Hand queued placement deletes to the terminal through a cell that is certain to be emitted.
///
/// A delete is an escape with no visual effect, so it needs a cell whose symbol reaches the terminal
/// *this* frame — and the frame it is queued on is by definition one that stopped painting the image
/// it belongs to.  The document area's first cell is the one cell such a frame always has: appending
/// changes its symbol, and a changed, non-skipped cell is always emitted.  [`UNIT_WIDTH`] keeps that
/// from costing anything in the diff, and the escapes neither draw nor move the cursor, so the
/// cell's own text is untouched.
fn write_control_escapes(buf: &mut TuiBuf, area: &Rect, deletes: &[(u32, u32)]) {
    let Some(cell) = buf.cell_mut((area.x, area.y)) else {
        return;
    };
    let mut symbol = String::with_capacity(cell.symbol().len() + deletes.len() * 48);
    symbol.push_str(cell.symbol());
    for (id, placement_id) in deletes {
        symbol.push_str(&crate::image::kitty_direct::delete_placement(
            *id,
            *placement_id,
        ));
    }
    cell.set_symbol(&symbol).set_diff_option(UNIT_WIDTH);
}

/// The on-screen band of a reserved image rect: how many of the image's rows sit above it, and the
/// screen rect it paints into.  `None` when there is no overlap at all.
///
/// `natural_top` is the image's top in document-area coordinates and is negative once it has
/// scrolled out; `height` is the *reserved* height, which `build_snapshots` deliberately keeps at
/// full size even when the rect runs off the screen.  Both numbers a protocol needs to paint a
/// slice come out of here: the destination rect supplies the width and the visible row count, and
/// the skip says which image row to start at (`SlicedImage` derives the matching drop from the
/// rect's height, so the two are consistent by construction).
fn image_band(
    x: u16,
    width: u16,
    natural_top: isize,
    height: u16,
    area: &Rect,
) -> Option<(u16, Rect)> {
    let viewport_top = area.y as isize;
    let viewport_bottom = viewport_top + area.height as isize;
    let top = natural_top.max(viewport_top);
    let bottom = (natural_top + height as isize).min(viewport_bottom);
    if bottom <= top {
        return None;
    }
    let skip = (top - natural_top) as u16;
    Some((skip, Rect::new(x, top as u16, width, (bottom - top) as u16)))
}

/// Blank the on-screen slice of `snap.rect` so the `[Image: alt]` placeholder can't bleed
/// through letter-box or trailing cells the protocol leaves untouched.
///
/// Two constraints: call it only AFTER the protocol-pair check, or a cleared rect with no
/// overlay leaves a blank square instead of the loading placeholder; and clear only cells
/// overlapping `area`, since a snap scrolled off the top has `natural_top < area.y` and the
/// cells above belong to other widgets.
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

/// Render the pair's native `ThreadProtocol` into `buf`, shipping a resize-encode to the worker
/// on the cold path.  Falls back to the halfblocks scratch while `native_ready` is false, so
/// the user never sees a placeholder flash.
///
/// When the previous frame already transmitted this exact image at this exact rect, the rect is
/// marked `skip` rather than re-rendered — see [`NativePaint`] for why that matters.
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
        // Reusable only if the *previous* frame left this exact encoding at this exact rect.
        // A one-frame gap means something else painted here, so the terminal lost the image.
        let already_on_screen = pair.last_native_paint
            == Some(NativePaint {
                rect: snap.rect,
                generation,
                frame: frame.wrapping_sub(1),
            });

        // The terminal's preferred protocol IS halfblocks, so scratch is the rendering.
        let Some(native) = pair.native.as_mut() else {
            pair.last_native_paint = None;
            if let Some(scratch) = pair.halfblocks_scratch.as_ref() {
                paint_halfblocks_partial(scratch, full_rect, 0, snap.rect, buf, bg);
            }
            return;
        };

        // `resize_encode` *takes* the inner StatefulProtocol and sends it to the worker, so
        // `render` is a silent no-op while the response is in flight.  ratatui-image 11 wants
        // a `Size` here, not a `Rect`; `Rect: Into<Size>` drops the origin.
        let new_size = native.needs_resize(&resize, snap.rect.into());
        let needs = new_size.is_some();
        if let Some(new_size) = new_size {
            native.resize_encode(&resize, new_size);
        }
        // `native_ready` latches on the first encode and is never cleared, so on a frame that
        // dispatches a *re*-encode the inner protocol is away at the worker and `render` would
        // draw nothing over a just-blanked rect.  `protocol_type()` is `None` exactly when the
        // protocol is away, so it is the precise "can render right now" test.
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

/// Mark every cell of `rect` skipped so ratatui emits nothing there this frame, leaving the
/// previous frame's native image on screen.  The cells underneath are already blanked by
/// `clear_visible_reserved_rect`, so a later unskipped frame diffs blank-vs-payload.
///
/// **This makes the frame buffer deliberately lie** about a region the terminal is showing an
/// image in.  What stops a ghost image is that ratatui's hand-written `impl PartialEq for Cell`
/// compares `skip` alongside symbol and style: the moment a frame stops skipping, blank-skipped
/// and blank-unskipped cells compare unequal and the blanks are emitted, erasing the image when
/// it scrolls away into empty space.  That upstream detail is load-bearing, so
/// `skipped_rect_still_diffs_against_the_same_cells_unskipped` pins it.
fn mark_rect_skipped(rect: Rect, buf: &mut TuiBuf) {
    for y in rect.y..rect.y.saturating_add(rect.height) {
        for x in rect.x..rect.x.saturating_add(rect.width) {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_diff_option(CellDiffOption::Skip);
            }
        }
    }
}

/// Cell-copy the halfblocks scratch into `buf`, clipped to `area`.  Used whenever native isn't
/// appropriate this frame.
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
    // These cells land over any native transmission, so the terminal no longer holds it.
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
    use crate::diff::DiffState;
    use crate::document::Buffer;
    use crate::editor::EditorState;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    // ── Native re-transmission suppression ───────────────────────────
    //
    // iTerm2 and Sixel put the whole base64 PNG in one cell's `symbol`, and `Buffer::diff`
    // treats that symbol's display width as an invalidation run — so one image forces every
    // later cell, a second image's payload included, back into the diff every frame.  These
    // tests pin the suppression that stops it; see `image::cache::NativePaint`.

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

        /// An iTerm2 picker — the protocol whose `render` delivers the full PNG every time.
        /// Stamped rather than probed, so the test is terminal-independent.
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

        /// The protocol whose placeholders address image rows by index — the whole reason a
        /// partially visible image can stay at native fidelity.
        #[allow(deprecated)]
        fn kitty_picker() -> Picker {
            let mut picker = Picker::from_fontsize((1, 2).into());
            picker.set_protocol_type(ProtocolType::Kitty);
            picker
        }

        /// The protocol with no image store at all: a sixel sequence is drawn where it is sent, so
        /// the band has to be carried in the payload.  Windows Terminal 1.22+, foot, and xterm with
        /// sixel enabled are the terminals this stands in for.
        #[allow(deprecated)]
        fn sixel_picker() -> Picker {
            let mut picker = Picker::from_fontsize((1, 2).into());
            picker.set_protocol_type(ProtocolType::Sixel);
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

        /// As [`snap`], but with a signed top, the way `build_snapshots` produces one: the rect's
        /// `y` saturates at 0 while `natural_top` keeps the negative offset.
        fn snap_at(url: &str, natural_top: isize, height: u16) -> ImageLayoutSnapshot {
            ImageLayoutSnapshot {
                block_idx: 0,
                alt: url.into(),
                url: url.into(),
                rect: Rect::new(0, natural_top.max(0) as u16, AREA.width, height),
                natural_top,
            }
        }

        /// As [`snap_at`], but for a *given* block: two blocks of one document is exactly what
        /// makes two placements of one stored image distinguishable.
        fn snap_block(
            url: &str,
            block_idx: usize,
            natural_top: isize,
            height: u16,
        ) -> ImageLayoutSnapshot {
            ImageLayoutSnapshot {
                block_idx,
                ..snap_at(url, natural_top, height)
            }
        }

        struct Harness {
            images: ImageCache,
            rx: mpsc::Receiver<ratatui_image::thread::ResizeRequest>,
            native: Picker,
            halfblocks: Picker,
            protocol: ImageProtocol,
            /// A modal forces the scratch so the dim sweep can recess the image; only the tests
            /// that pin that gate raise it.
            modal_open: bool,
        }

        impl Harness {
            fn new(urls: &[&str]) -> Self {
                Self::with_native(urls, iterm2_picker(), ImageProtocol::ITerm2)
            }

            /// A harness whose terminal speaks Kitty, so `paint_images` takes the row-addressed
            /// band path rather than the `fully_visible` gate.
            fn kitty(urls: &[&str]) -> Self {
                Self::with_native(urls, kitty_picker(), ImageProtocol::KittyGraphics)
            }

            /// A harness whose terminal speaks Sixel, so `paint_images` takes the band path through
            /// a payload re-slice rather than through row addressing.
            fn sixel(urls: &[&str]) -> Self {
                Self::with_native(urls, sixel_picker(), ImageProtocol::Sixel)
            }

            /// A harness for the terminals that place an already-transmitted image instead of
            /// rendering unicode placeholders — WezTerm, which the probe reports as iTerm2 and
            /// which is therefore the picker here.
            fn direct(urls: &[&str]) -> Self {
                Self::with_native(urls, iterm2_picker(), ImageProtocol::KittyDirect)
            }

            fn with_native(urls: &[&str], native: Picker, protocol: ImageProtocol) -> Self {
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
                    native,
                    halfblocks: halfblocks_picker(),
                    protocol,
                    modal_open: false,
                }
            }

            /// Stand in for the encoder worker: run every queued resize-encode synchronously.
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

            /// [`Self::frame`] with `suppress_block_idx` set — the raw-reveal path, which
            /// paints nothing over the block's rect.
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
                    native_protocol: Some(self.protocol),
                    is_scrolling: scrolling,
                    modal_open: self.modal_open,
                    suppress_block_idx,
                    bg: Color::Reset,
                };
                paint_images(snaps, ctx);
                buf
            }
        }

        /// True when the cell at `rect`'s origin carries an iTerm2 inline-image escape — the
        /// whole PNG was handed to the terminal this frame.
        fn transmitted(buf: &TuiBuf, rect: Rect) -> bool {
            buf.cell((rect.x, rect.y))
                .is_some_and(|c| c.symbol().contains("]1337;File="))
        }

        /// True when the cell carries a Kitty placeholder run: the image is placed by those
        /// characters themselves, not by an escape written somewhere else.
        fn placed(buf: &TuiBuf, rect: Rect) -> bool {
            buf.cell((rect.x, rect.y))
                .is_some_and(|c| c.symbol().contains('\u{10EEEE}'))
        }

        /// True when the cell carries the Kitty transmit escape, i.e. the raw payload went over on
        /// this frame.  `_Gq=2` survives tmux passthrough wrapping, so it holds either way.
        fn transmitted_kitty(buf: &TuiBuf, rect: Rect) -> bool {
            buf.cell((rect.x, rect.y))
                .is_some_and(|c| c.symbol().contains("_Gq=2"))
        }

        /// The sixel payload written into `rect`'s first cell — the whole escape, `clear_area`
        /// sweep included.  Panics when the cell holds a glyph instead, which is what the
        /// halfblocks scratch writes, so it doubles as "the band path ran".
        fn sixel_payload(buf: &TuiBuf, rect: Rect) -> String {
            let symbol = symbol_at(buf, rect);
            assert!(symbol.contains("\x1bP"), "not a sixel payload: {symbol:?}");
            symbol
        }

        /// The sixel bands a payload carries — the unit the band arithmetic works in.  Sixel's
        /// row separator is `-`, so the band count is the count of non-empty segments after the
        /// DCS introducer, and a payload that carried the whole image would count all of them.
        fn sixel_bands(payload: &str) -> usize {
            payload
                .split_once("\x1bP")
                .expect("a sixel payload")
                .1
                .split('-')
                .filter(|band| !band.is_empty())
                .count()
        }

        /// The symbol written into `rect`'s first cell.
        fn symbol_at(buf: &TuiBuf, rect: Rect) -> String {
            buf.cell((rect.x, rect.y))
                .map(|c| c.symbol().to_owned())
                .unwrap_or_default()
        }

        /// A `key=` value from the *placement* command in `symbol`.
        ///
        /// The placement is not the first `_G` command in a symbol that also carries the transmit,
        /// so it is found by `a=p` rather than by position.
        fn placement_value(symbol: &str, key: &str) -> Option<String> {
            let wanted = format!("{key}=");
            symbol
                .split("\x1b_G")
                .filter(|command| command.contains("a=p"))
                .flat_map(|command| command.split(';').next().unwrap_or_default().split(','))
                .find_map(|pair| pair.strip_prefix(&wanted).map(str::to_owned))
        }

        /// The escapes carried by the document area's first cell, which is where a queued delete
        /// rides: the placement's own cell may not be painted on the frame the delete is owed.
        fn carried_escapes(buf: &TuiBuf) -> String {
            symbol_at(buf, Rect::new(AREA.x, AREA.y, 1, 1))
        }

        /// The band a protocol has to paint: which image row the visible slice starts at, and
        /// where it lands.  This is the arithmetic the fix rests on — with the reserved rect held
        /// at full size across scrolls, it is the only thing that changes as the image moves.
        #[test]
        fn image_band_reports_the_visible_slice() {
            let area = Rect::new(0, 0, 20, 20);
            let band = |top: isize, height: u16| image_band(0, 20, top, height, &area);

            // Fully visible: the whole rect, nothing skipped.
            assert_eq!(band(4, 6), Some((0, Rect::new(0, 4, 20, 6))));
            // Top clipped by three rows.
            assert_eq!(band(-3, 10), Some((3, Rect::new(0, 0, 20, 7))));
            // Bottom clipped: four of the ten rows are on screen, none skipped.
            assert_eq!(band(16, 10), Some((0, Rect::new(0, 16, 20, 4))));
            // Taller than the viewport: five rows skipped, the rest clamped to it.
            assert_eq!(band(-5, 40), Some((5, Rect::new(0, 0, 20, 20))));
            // Entirely off either edge: no band at all.
            assert_eq!(band(-30, 10), None);
            assert_eq!(band(40, 6), None);

            // A viewport that does not start at row zero: both the skip and the destination are
            // measured from the document area, not from the top of the screen.
            let offset = Rect::new(0, 5, 20, 10);
            assert_eq!(
                image_band(2, 20, 8, 6, &offset),
                Some((0, Rect::new(2, 8, 20, 6)))
            );
            assert_eq!(
                image_band(2, 20, 2, 6, &offset),
                Some((3, Rect::new(2, 5, 20, 3)))
            );
        }

        /// The regression the change exists for: a clipped image paints at native fidelity rather
        /// than dropping to the halfblocks scratch, and banding does not re-send the payload.
        #[test]
        fn kitty_paints_a_clipped_image_as_a_band() {
            let whole = vec![snap_at("a.png", 0, 8)];
            let clipped = vec![snap_at("a.png", -3, 8)];
            let mut h = Harness::kitty(&["a.png"]);

            let first = h.frame(&whole, false);
            assert!(placed(&first, whole[0].rect), "kitty must place the image");
            assert!(
                transmitted_kitty(&first, whole[0].rect),
                "the first paint carries the payload"
            );

            let band = h.frame(&clipped, false);
            assert!(
                placed(&band, clipped[0].rect),
                "a clipped image must still be placed natively"
            );
            assert!(
                !transmitted_kitty(&band, clipped[0].rect),
                "banding must not re-send the payload the terminal already holds"
            );
        }

        /// The band path yields to the scroll window and to a modal.  Both gates are deliberate:
        /// Kitty's placeholders re-composite wherever they move, so painting them every scroll
        /// frame is the lag the scratch window exists to avoid, and `dim_area` cannot recess an
        /// image that writes past the cell buffer.
        #[test]
        fn kitty_yields_the_band_while_scrolling_and_under_a_modal() {
            // Fully visible on purpose: with the rect entirely on screen, only the gate under test
            // can explain a scratch paint.  The old `fully_visible` requirement is deliberately
            // gone for Kitty; these two are not.
            let whole = vec![snap_at("a.png", 0, 8)];
            let mut h = Harness::kitty(&["a.png"]);
            assert!(
                placed(&h.frame(&whole, false), whole[0].rect),
                "the band should paint with both gates open"
            );

            assert!(
                !placed(&h.frame(&whole, true), whole[0].rect),
                "scrolling must fall back to the scratch"
            );

            h.modal_open = true;
            assert!(
                !placed(&h.frame(&whole, false), whole[0].rect),
                "an open modal must fall back to the scratch"
            );
        }

        /// The same fix for Sixel, whose band cannot be a parameter of anything: a sixel sequence
        /// is drawn where it is sent and stored per text row by the terminal, so the payload itself
        /// has to be re-sliced.  Windows Terminal is the terminal this was written for — it answers
        /// DA1 with sixel support (and speaks no Kitty graphics at all), so before this it was the
        /// one terminal where a partly visible image could only be the coarse halfblocks mosaic.
        #[test]
        fn sixel_paints_a_clipped_image_as_a_band() {
            let whole = vec![snap_at("a.png", 0, 20)];
            let top_clipped = vec![snap_at("a.png", -3, 20)];
            // The permanent case the issue opened with: a reserved rect taller than the viewport
            // (`images.max_height`), where `fully_visible` could never be true.
            let taller_than_the_viewport = vec![snap_at("a.png", 0, 40)];
            let mut h = Harness::sixel(&["a.png"]);

            let full = sixel_payload(&h.frame(&whole, false), whole[0].rect);

            // Three rows scrolled off the top: the payload starts three rows into the image.
            let clipped = sixel_payload(&h.frame(&top_clipped, false), top_clipped[0].rect);
            assert_ne!(
                clipped, full,
                "the band must be re-sliced for the clip, not the whole payload again"
            );

            // The payload is the *visible window*, not the reserved rect: a 40-row image that shows
            // 20 rows carries the same bands as the same image fully on screen — and nowhere near
            // the bands its own 40-row encoding has.
            let tall = sixel_payload(
                &h.frame(&taller_than_the_viewport, false),
                taller_than_the_viewport[0].rect,
            );
            assert_eq!(
                sixel_bands(&tall),
                sixel_bands(&full),
                "the band is the visible rows, not the reserved height"
            );

            // Sixel's band is re-emitted rather than re-encoded, but the terminal still
            // re-rasterises the payload, so the scroll window applies here too.
            assert!(
                !symbol_at(&h.frame(&whole, true), whole[0].rect).contains("\x1bP"),
                "scrolling must fall back to the scratch"
            );
        }

        /// The whole point of the direct-placement route: the band is a *source rectangle* on an
        /// image the terminal already holds, so a clipped frame re-places it without re-sending
        /// anything — and the source offset is the skipped rows, in image pixels.
        #[test]
        fn direct_placement_bands_without_resending() {
            let whole = vec![snap_at("a.png", 0, 8)];
            let mut h = Harness::direct(&["a.png"]);

            let first = h.frame(&whole, false);
            let symbol = symbol_at(&first, whole[0].rect);
            assert!(
                symbol.contains("a=p"),
                "the image must be placed: {symbol:?}"
            );
            assert!(
                symbol.contains("a=t"),
                "the first paint carries the payload"
            );
            assert!(
                symbol.contains(",x=0,y=0,w=20,h=16,c=20,r=8,"),
                "a fully visible band is the whole image: {symbol:?}"
            );

            // Three rows scrolled off the top, at 2 px per row: source y = 6, and five of the
            // image's eight rows are on screen.
            let clipped = vec![snap_at("a.png", -3, 8)];
            let band = h.frame(&clipped, false);
            let symbol = symbol_at(&band, clipped[0].rect);
            assert!(
                symbol.contains(",x=0,y=6,w=20,h=10,c=20,r=5,"),
                "the band must be a source rectangle of the visible rows: {symbol:?}"
            );
            assert!(
                !symbol.contains("a=t"),
                "banding must not re-send the payload the terminal already holds"
            );
        }

        /// A placement is anchored to screen cells, so one that stops being painted has to be
        /// deleted — and the delete needs a cell that reaches the terminal on that very frame.
        #[test]
        fn an_unpainted_direct_placement_is_deleted() {
            let snaps = vec![snap_at("a.png", 0, 8)];
            let mut h = Harness::direct(&["a.png"]);
            assert!(
                symbol_at(&h.frame(&snaps, false), snaps[0].rect).contains("a=p"),
                "the placement is live after the first frame"
            );

            // The block is gone: edited away, navigated off, or its decode failed.  No snapshot
            // mentions it any more, which is exactly what the id-set difference catches.
            let gone = h.frame(&[], false);
            assert!(
                !symbol_at(&gone, snaps[0].rect).contains("a=p"),
                "nothing placed the image"
            );
            assert!(
                carried_escapes(&gone).contains("a=d"),
                "the frame that stops placing must delete the placement: {:?}",
                carried_escapes(&gone)
            );
            assert_eq!(
                h.images.pending_deletes(),
                0,
                "the queue drains on the frame it is filled"
            );

            // And nothing is queued again once there is nothing left to remove.
            let again = h.frame(&[], false);
            assert!(
                !carried_escapes(&again).contains("a=d"),
                "one delete is enough: {:?}",
                carried_escapes(&again)
            );
        }

        /// A moving image stays sharp: direct placement keeps painting while scrolling, because a
        /// placement is one short escape where the halfblocks fallback is a write per cell.
        #[test]
        fn direct_placement_keeps_painting_while_scrolling() {
            let snaps = vec![snap_at("a.png", 0, 8)];
            let mut h = Harness::direct(&["a.png"]);
            h.frame(&snaps, false);

            let scrolled = h.frame(&snaps, true);
            assert!(
                symbol_at(&scrolled, snaps[0].rect).contains("a=p"),
                "scrolling must not downgrade a direct placement to halfblocks: {:?}",
                symbol_at(&scrolled, snaps[0].rect)
            );
            assert!(
                !symbol_at(&scrolled, snaps[0].rect).contains("a=t"),
                "and must not re-send the payload"
            );

            // The band moves with the scroll, so the placement names the new source rows: two rows
            // up at 2 px per row is a source offset of 4.
            let moved = vec![snap_at("a.png", -2, 8)];
            let band = h.frame(&moved, true);
            assert!(
                symbol_at(&band, moved[0].rect).contains(",y=4,"),
                "the source rect follows the band: {:?}",
                symbol_at(&band, moved[0].rect)
            );
        }

        /// The modal gate stays: `dim_area` recesses the image by writing over its cells, which it
        /// cannot do to a placement the terminal is compositing on top.
        #[test]
        fn direct_placement_still_yields_to_a_modal() {
            let snaps = vec![snap_at("a.png", 0, 8)];
            let mut h = Harness::direct(&["a.png"]);
            h.frame(&snaps, false);

            h.modal_open = true;
            let dimmed = h.frame(&snaps, false);
            assert!(
                !symbol_at(&dimmed, snaps[0].rect).contains("a=p"),
                "an open modal must fall back to the scratch"
            );
            assert!(
                carried_escapes(&dimmed).contains("a=d"),
                "and take the placement with it"
            );
        }

        /// The same image in two blocks is **one** stored image and **two** placements.  Sharing
        /// the transmit is the point — identical pixels — but sharing the placement id would let
        /// the second replace the first, leaving one of the two blocks showing nothing.
        #[test]
        fn one_image_in_two_blocks_shares_the_data_and_not_the_placement() {
            let snaps = vec![snap_block("a.png", 0, 0, 8), snap_block("a.png", 1, 12, 8)];
            let mut h = Harness::direct(&["a.png"]);
            let frame = h.frame(&snaps, false);

            let first = symbol_at(&frame, snaps[0].rect);
            let second = symbol_at(&frame, snaps[1].rect);
            assert!(
                first.contains("a=p") && second.contains("a=p"),
                "both blocks must place the image: {first:?} / {second:?}"
            );

            assert_eq!(
                placement_value(&first, "i"),
                placement_value(&second, "i"),
                "one stored image, so one id — the second block reuses the first's data"
            );
            assert_ne!(
                placement_value(&first, "p"),
                placement_value(&second, "p"),
                "two placements, or the second replaces the first"
            );
            assert!(
                first.contains("a=t") ^ second.contains("a=t"),
                "the payload goes over exactly once"
            );
            // Each block places its own band: the second sits twelve rows further down.
            assert_eq!(placement_value(&first, "y").as_deref(), Some("0"));
            assert_eq!(placement_value(&second, "y").as_deref(), Some("0"));
            assert!(
                second.contains(&format!("\x1b[{};1H", snaps[1].rect.y + 1)),
                "{second:?}"
            );
        }

        /// When a block stops being painted, only *its* placement is deleted — a sibling block
        /// showing the same image keeps the data, and keeps its own placement.
        #[test]
        fn deleting_one_of_two_placements_leaves_the_other() {
            let snaps = vec![snap_block("a.png", 0, 0, 8), snap_block("a.png", 1, 12, 8)];
            let mut h = Harness::direct(&["a.png"]);
            let frame = h.frame(&snaps, false);
            let survivor = placement_value(&symbol_at(&frame, snaps[1].rect), "p")
                .expect("the second block placed it");

            // The first block's snapshot goes away; the second stays.
            let gone = h.frame(&snaps[1..], false);
            let carried = carried_escapes(&gone);
            assert!(
                carried.contains("a=d"),
                "its placement must be deleted: {carried:?}"
            );
            let deleted = carried
                .split("\x1b_G")
                .filter(|command| command.contains("a=d"))
                .flat_map(|command| command.split(';').next().unwrap_or_default().split(','))
                .find_map(|pair| pair.strip_prefix("p=").map(str::to_owned))
                .expect("a placement id");
            assert_ne!(
                deleted, survivor,
                "deleting the gone block's placement must spare the surviving one"
            );
            assert!(
                symbol_at(&gone, snaps[1].rect).contains("a=p"),
                "and the survivor is still placed"
            );
        }

        #[test]
        fn two_native_images_transmit_once_then_go_quiet() {
            let snaps = vec![snap("a.png", 0, 6), snap("b.png", 8, 6)];
            let mut h = Harness::new(&["a.png", "b.png"]);

            // Frame 1 ships the encodes; the protocols are away, so it paints halfblocks.
            h.frame(&snaps, false);
            h.drain_encoder();

            // Frame 2 transmits: both payloads land in the buffer.
            let transmit = h.frame(&snaps, false);
            assert!(
                transmitted(&transmit, snaps[0].rect),
                "first image should carry its base64 payload"
            );
            assert!(
                transmitted(&transmit, snaps[1].rect),
                "second image should carry its base64 payload"
            );

            // Idle redraws: nothing may be re-sent, or iTerm2 blanks and repaints each image
            // (the ~2 Hz flicker this guards).
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

            // A scroll frame paints halfblocks over the region, so the terminal loses it…
            let scratch = h.frame(&snaps, true);
            assert!(!transmitted(&scratch, snaps[0].rect));

            // …and the next settled frame must send it again.
            let resent = h.frame(&snaps, false);
            assert!(
                transmitted(&resent, snaps[0].rect),
                "native paint after a scratch frame must retransmit"
            );
        }

        /// For paths that leave the rect *unpainted* — a suppressed block, an off-screen image
        /// — the suppression rests on frame adjacency alone: the `[Image: alt]` placeholder
        /// lands there instead, so the next native frame must re-send.  Pinned separately from
        /// the scratch case, which `paint_scratch_partial` clears explicitly and which would
        /// still pass without the `frame` field.
        #[test]
        fn a_suppressed_frame_forces_the_next_native_frame_to_retransmit() {
            let snaps = vec![snap("a.png", 0, 6)];
            let mut h = Harness::new(&["a.png"]);
            h.frame(&snaps, false);
            h.drain_encoder();
            assert!(transmitted(&h.frame(&snaps, false), snaps[0].rect));
            // Settled: the next frame skips rather than re-sending.
            assert!(!transmitted(&h.frame(&snaps, false), snaps[0].rect));

            // Raw-reveal on the image's own block paints nothing and leaves the record.
            let suppressed = h.frame_suppressing(&snaps, false, Some(snaps[0].block_idx));
            assert!(!transmitted(&suppressed, snaps[0].rect));

            assert!(
                transmitted(&h.frame(&snaps, false), snaps[0].rect),
                "native paint after a suppressed frame must retransmit"
            );
        }

        /// The skip marking makes the buffer claim blank rows while the terminal shows an
        /// image; nothing would erase it if a later blank frame diffed clean against the
        /// skipped one — exactly what happens when the image scrolls into empty space.  What
        /// saves it is that ratatui's `impl PartialEq for Cell` compares `skip`, so the blanks
        /// are emitted.  An upstream detail, asserted directly.
        #[test]
        fn skipped_rect_still_diffs_against_the_same_cells_unskipped() {
            let snaps = vec![snap("a.png", 0, 6)];
            let mut h = Harness::new(&["a.png"]);
            h.frame(&snaps, false);
            h.drain_encoder();
            h.frame(&snaps, false);
            let skipped = h.frame(&snaps, false);
            assert!(!transmitted(&skipped, snaps[0].rect));

            // The image is gone and its rows are empty space: no snapshots, nothing painted.
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

            // A resize / `terminal.clear()` wipes the screen; the record must not survive it.
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

        // Identical inputs preserve the key.
        build_snapshots_cached(&state, area, 0, &mut snapshots, &mut key);
        assert_eq!(key, populated_key);
        assert_eq!(snapshots.len(), 1);

        // A scroll change invalidates and repopulates.
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
        // `rect.height` is ALWAYS the full reserved size regardless of what fits;
        // `paint_images` refuses when it doesn't, which keeps the cached encoding stable.
        let src = "![big](big.png)\n";
        let state = state_from(src, 20);
        let area = Rect::new(0, 0, 20, 5);
        let snaps = build_snapshots(&state, area, 0);
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].rect.height, 20);
    }

    // ── Diff-mode snapshots ──────────────────────────────────────────

    /// A review of `old` → `new` with the new-side parse installed, at the query width.
    fn diff_from(old: &str, new: &str, image_max_height: usize) -> DiffState {
        let mut diff = DiffState::new(old, new).expect("non-empty diff");
        let parsed = crate::document::ParsedDoc::build(new, theme(), true, image_max_height);
        diff.set_rendered_parse(Some(parsed));
        diff
    }

    #[test]
    fn diff_snapshot_matches_the_rows_the_layout_reserved() {
        // The change is in the paragraph below, so the image block stays clean and renders.
        let old = "Intro.\n\n![cat](cat.png)\n\nTail.\n";
        let new = "Intro.\n\n![cat](cat.png)\n\nTAIL!\n";
        let diff = diff_from(old, new, 4);
        let area = Rect::new(0, 0, 20, 30);
        let snaps = build_diff_snapshots(&diff, area, 0);
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].url, "cat.png");
        assert_eq!(snaps[0].rect.height, 4);
        // `rect.y` is the diff visual row of the block's first rendered line.
        let expected = diff.with_layout_index(area.width as usize, |_lines, rc, index| {
            let parsed = diff.parsed_new.as_ref().expect("parse installed");
            let block = parsed
                .image_blocks
                .first()
                .expect("one image block")
                .block_idx;
            let range = parsed.source_map.rendered_lines_for_block(block);
            rc.before(index[&range.start])
        });
        assert_eq!(snaps[0].rect.y as usize, expected);
    }

    #[test]
    fn a_changed_image_block_yields_no_diff_snapshot() {
        // A changed image is in a raw region and reserves no rows.
        let old = "Intro.\n\n![cat](cat.png)\n\nTail.\n";
        let new = "Intro.\n\n![cat](other.png)\n\nTail.\n";
        let diff = diff_from(old, new, 4);
        let area = Rect::new(0, 0, 20, 30);
        assert!(build_diff_snapshots(&diff, area, 0).is_empty());
    }

    #[test]
    fn a_partly_scrolled_diff_snapshot_keeps_a_negative_natural_top() {
        // Saturating at 0 would make `paint_images` treat a half-scrolled image as full.
        let old = "Intro.\n\n![cat](cat.png)\n\nTail.\n";
        let new = "Intro.\n\n![cat](cat.png)\n\nTAIL!\n";
        let diff = diff_from(old, new, 6);
        let area = Rect::new(0, 0, 20, 30);
        let top = build_diff_snapshots(&diff, area, 0)[0].rect.y as usize;
        let snaps = build_diff_snapshots(&diff, area, top + 2);
        assert_eq!(snaps.len(), 1);
        assert!(snaps[0].natural_top < 0, "{:?}", snaps[0]);
        assert_eq!(snaps[0].rect.height, 6, "reserved height stays full");
    }

    #[test]
    fn no_diff_snapshots_without_a_rendered_parse() {
        let diff = DiffState::new(
            "Intro.\n\n![cat](cat.png)\n\nTail.\n",
            "Intro.\n\n![cat](cat.png)\n\nTAIL!\n",
        )
        .expect("non-empty diff");
        let area = Rect::new(0, 0, 20, 30);
        assert!(build_diff_snapshots(&diff, area, 0).is_empty());
    }

    #[test]
    fn build_diff_snapshots_cached_reuses_output_when_key_matches() {
        let old = "Intro.\n\n![cat](cat.png)\n\nTail.\n";
        let new = "Intro.\n\n![cat](cat.png)\n\nTAIL!\n";
        let diff = diff_from(old, new, 4);
        let area = Rect::new(0, 0, 20, 30);
        let mut snapshots = Vec::new();
        let mut key = None;
        build_diff_snapshots_cached(&diff, area, 0, &mut snapshots, &mut key);
        assert_eq!(snapshots.len(), 1);
        let populated = key;
        build_diff_snapshots_cached(&diff, area, 0, &mut snapshots, &mut key);
        assert_eq!(key, populated);
        build_diff_snapshots_cached(&diff, area, 3, &mut snapshots, &mut key);
        assert_ne!(key, populated);
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

    /// Stand-in for the `[Image: alt]` placeholder: fills `area` so the clear is observable.
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
        // The regression test for placeholder text peeking out from behind a narrow image.
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
        // natural_top = -2, so the top two reserved rows are above the viewport; the clear
        // must touch only in-viewport cells.
        let area = Rect::new(0, 5, 30, 10);
        let mut buf = pre_populate_buf(area, 'X');
        // Top at row 3 (two above area.y=5), height 6 → visible rows 5..9.
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
        // Reserved rows [-10, -6) don't overlap the area, so nothing is cleared.
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
