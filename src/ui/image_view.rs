//! `ImageView` — per-frame layout snapshot and post-render overlay for
//! Phase 7 image blocks.
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

use ratatui::buffer::Buffer as TuiBuf;
use ratatui::layout::Rect;
use ratatui::widgets::StatefulWidget;
use ratatui_image::{Resize, StatefulImage};

use crate::editor::EditorState;
use crate::image::{paint_halfblocks_partial, ImageCache};
use crate::terminal::ImageProtocol;
use crate::ui::line_render;

/// Per-frame geometry for one visible image block.  Screen coordinates
/// are in terminal cells, relative to the document area's origin.  Only
/// valid for the frame on which the snapshot was built.
#[derive(Debug, Clone)]
pub struct ImageLayoutSnapshot {
    /// Virtual-block index in the current `ParsedDoc::source_map`.
    pub block_idx: usize,
    /// Alt text, used for the fallback placeholder when the image can't
    /// be rendered.
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
/// Phase 7 only distinguishes "click landed on an image" from "click
/// landed elsewhere"; Phase 8+ may grow variants for expand / open /
/// copy affordances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageHit {
    Body { block_idx: usize },
}

impl ImageLayoutSnapshot {
    /// Return the visible byte range of the image block's source (the
    /// `![alt](url)` line).  Useful for selection / copy routines that
    /// need to know what source span this block represents.
    pub fn source_range(&self, state: &EditorState) -> Option<Range<usize>> {
        state
            .parsed
            .source_map
            .original_range_for_block(self.block_idx)
    }

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
/// `scroll` is the number of rendered lines skipped at the top; matches
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
pub fn build_snapshots(state: &EditorState, area: Rect, scroll: usize) -> Vec<ImageLayoutSnapshot> {
    let mut out = Vec::new();
    if area.height == 0 {
        return out;
    }
    let width = area.width as usize;
    let total = state.parsed.line_count();
    if scroll >= total {
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
        // content might.
        let mut y_offset: isize = 0;
        let end = rendered_range.start.min(total);
        if scroll < end {
            for idx in scroll..end {
                if let Some(line) = state.parsed.lines.get(idx) {
                    y_offset += line_render::visual_rows_for_line(line, width).max(1) as isize;
                }
            }
        } else if scroll > rendered_range.start {
            // Block starts above the viewport — negative y_offset.
            // Approximate one rendered line per unit (image-block
            // placeholder rows never wrap, so this is exact for them).
            y_offset = -(scroll as isize - rendered_range.start as isize);
        }

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
    /// Block index to skip (cursor's block during raw-reveal).
    pub suppress_block_idx: Option<usize>,
}

/// Render each image onto its reserved rect, overlaying the `[Image: alt]`
/// placeholder emitted by the text renderer.
///
/// Decision per snapshot (see "Rule set" in the follow-up plan):
///
/// | Image state                                             | Protocol used |
/// |---------------------------------------------------------|---------------|
/// | Fully visible AND not scrolling                         | native        |
/// | Fully visible AND scrolling, native is Kitty            | native        |
/// | Fully visible AND scrolling, native is Sixel/iTerm2     | halfblocks    |
/// | Partially visible (any protocol / scroll state)         | halfblocks    |
///
/// Halfblocks-only terminals collapse the first two rules (native IS
/// halfblocks), so there's only one protocol and `ProtocolPair::halfblocks`
/// is `None` — the "native" branch draws the halfblocks encoding.
pub fn paint_images(snapshots: &[ImageLayoutSnapshot], ctx: PaintContext) {
    if ctx.native_picker.is_none() || ctx.native_protocol.is_none() {
        return;
    }
    let viewport_top = ctx.area.y as isize;
    let viewport_bottom = (ctx.area.y as isize) + ctx.area.height as isize;
    let native_is_kitty = ctx.native_protocol == Some(ImageProtocol::KittyGraphics);
    let native_is_halfblocks = ctx.native_protocol == Some(ImageProtocol::Halfblocks);

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

        // Build / fetch the protocol pair for this image at its stable
        // reserved dimensions.  Pair construction is one-time per (url,
        // w, h); subsequent frames hit the cache.
        let Some(pair) = ctx.images.get_protocol_pair(
            &snap.url,
            snap.rect.width,
            snap.rect.height,
            ctx.native_picker,
            ctx.halfblocks_picker,
        ) else {
            // Decode pending / failed — placeholder stays visible.
            continue;
        };

        // Decide which protocol to use this frame.  When native IS
        // halfblocks the rule collapses (there's no second encoding).
        let use_native =
            native_is_halfblocks || (fully_visible && (!ctx.is_scrolling || native_is_kitty));

        if use_native {
            // Full-rect native render.  Only reachable when the image is
            // fully visible OR native is halfblocks (halfblocks can
            // render partial by itself, but we take the full-rect path
            // for simplicity — the "partial" path is for rows clipped
            // against a non-full rect, not for short buffer writes).
            if fully_visible {
                StatefulImage::default().resize(Resize::Fit(None)).render(
                    snap.rect,
                    ctx.buf,
                    &mut pair.native,
                );
            } else {
                // Not fully visible but native IS halfblocks — use the
                // partial helper on the native encoding since it's
                // position-independent.
                paint_partial(&mut pair.native, snap, &ctx.area, ctx.buf);
            }
            continue;
        }

        // Halfblocks fallback for partial visibility or mid-scroll on
        // non-Kitty terminals.  If we have no separate halfblocks
        // encoding (shouldn't happen once native_is_halfblocks is
        // already handled above, but be defensive), the placeholder
        // stays.
        let Some(halfblocks) = pair.halfblocks.as_mut() else {
            continue;
        };
        paint_partial(halfblocks, snap, &ctx.area, ctx.buf);
    }
}

/// Copy the halfblocks encoding of `snap` into `buf`, clipping to the
/// portion that lies inside `area`.  Used for both partial-visibility
/// fallback and mid-scroll fallback on Sixel/iTerm2 terminals.
fn paint_partial(
    protocol: &mut ratatui_image::protocol::StatefulProtocol,
    snap: &ImageLayoutSnapshot,
    area: &Rect,
    buf: &mut TuiBuf,
) {
    let viewport_top = area.y as isize;
    let viewport_bottom = (area.y as isize) + area.height as isize;
    let top = snap.natural_top;
    let bottom = top + snap.rect.height as isize;
    // How many image rows above the viewport top.
    let clip_top = if top < viewport_top {
        (viewport_top - top) as u16
    } else {
        0
    };
    // Visible height of the image inside the viewport.
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
    paint_halfblocks_partial(protocol, full_rect, clip_top, dst_rect, buf);
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
}
