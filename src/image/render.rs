//! Halfblocks partial-render helper for Phase 7's progressive-enhancement
//! scheme.
//!
//! Native terminal-graphics protocols (Sixel, Kitty, iTerm2) encode the
//! whole image into a single escape sequence; partial rendering (clipping
//! top/bottom rows when the image scrolls through the viewport) requires
//! re-encoding, which is slow enough to cause visible lag on every
//! scrolled frame.  Halfblocks, by contrast, encodes each cell as a
//! single `(upper, lower, char)` triple with no absolute coordinates —
//! cells are **position-independent** and can be cell-copied from one
//! buffer to another without any encoding work.
//!
//! `paint_halfblocks_partial` exploits this: it renders the full image
//! into a scratch buffer (one encode, reused across frames by
//! `StatefulProtocol`'s internal cache when the rect is stable) and
//! copies only the visible rows into the destination buffer.  The
//! technique only works for halfblocks-encoded cells; callers guarantee
//! that by passing a halfblocks-built `StatefulProtocol`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::StatefulWidget;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{Resize, StatefulImage};

/// Render a halfblocks-encoded `protocol` into `buf`, clipped to the
/// visible `dst_rect` and offset vertically by `src_y_offset` image
/// rows.
///
/// Parameters:
///
/// * `protocol` — a `StatefulProtocol` built from the halfblocks
///   `Picker`.  Calling with a Kitty/Sixel/iTerm2 protocol is a bug; the
///   produced cells carry encoded escape sequences that only work when
///   placed at the exact coordinates the protocol computed for them.
/// * `full_rect` — the image's natural rectangle (stable across frames):
///   `width = image_max_width`, `height = image_max_height`, origin
///   `(0, 0)`.  We render into a scratch buffer of this size so the
///   halfblocks encoder always sees the same target, which keeps its
///   internal caches warm.
/// * `src_y_offset` — how many image rows have scrolled past the top.
///   When the image's top is above the viewport, this is positive;
///   when fully visible, it's zero.
/// * `dst_rect` — the on-screen region to paint into.  Cells outside
///   `buf`'s own area are silently dropped (ratatui's `cell_mut` returns
///   `None` on out-of-bounds positions).
/// * `buf` — the destination `Buffer` (the frame buffer supplied to
///   every `render` call).
///
/// This function does **not** bounds-check `src_y_offset` against
/// `full_rect.height`; passing an offset that leaves fewer rows than
/// `dst_rect.height` produces blank cells at the bottom, which is the
/// desired behaviour when an image is scrolling off the bottom of the
/// viewport.
pub fn paint_halfblocks_partial(
    protocol: &mut StatefulProtocol,
    full_rect: Rect,
    src_y_offset: u16,
    dst_rect: Rect,
    buf: &mut Buffer,
) {
    if full_rect.width == 0 || full_rect.height == 0 || dst_rect.width == 0 || dst_rect.height == 0
    {
        return;
    }
    // Render the full image into a scratch buffer at (0, 0).  Using the
    // same full-rect every frame means `StatefulProtocol` hits its no-re-
    // encode fast path after the first call.
    let scratch_rect = Rect::new(0, 0, full_rect.width, full_rect.height);
    let mut scratch = Buffer::empty(scratch_rect);
    StatefulImage::default()
        .resize(Resize::Fit(None))
        .render(scratch_rect, &mut scratch, protocol);

    // Cell-copy the clipped portion into the destination.  Bounds on
    // `scratch` clip when `src_y_offset + dy` exceeds `full_rect.height`
    // (returns `None` from `cell`); bounds on `buf` clip when `dst_rect`
    // extends past the frame edge — both are safe no-ops.
    for dy in 0..dst_rect.height {
        let src_y = src_y_offset.saturating_add(dy);
        if src_y >= full_rect.height {
            break;
        }
        for dx in 0..dst_rect.width {
            if dx >= full_rect.width {
                break;
            }
            let Some(src_cell) = scratch.cell((dx, src_y)) else {
                continue;
            };
            let src_cell = src_cell.clone();
            if let Some(dst_cell) = buf.cell_mut((dst_rect.x + dx, dst_rect.y + dy)) {
                *dst_cell = src_cell;
            }
        }
    }
}

#[cfg(test)]
// `Picker::from_fontsize` is deprecated in ratatui-image 9; we use it
// here because `Picker::halfblocks()` hardcodes a font size that
// produces different cell counts from what these tests pin.
#[allow(deprecated)]
mod tests {
    use super::*;

    use image::DynamicImage;
    use ratatui_image::picker::Picker;

    fn halfblocks_protocol(w: u32, h: u32) -> StatefulProtocol {
        // Picker::from_fontsize defaults to ProtocolType::Halfblocks.
        let picker = Picker::from_fontsize((1, 2));
        // A 4-channel RGBA image; pixel content doesn't matter — we only
        // care that encoding produces non-default cells we can compare
        // against.
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            w,
            h,
            image::Rgba([40, 80, 120, 255]),
        ));
        picker.new_resize_protocol(img)
    }

    /// With `src_y_offset = 0` and `dst_rect.height = full_rect.height`,
    /// the helper should produce cell-for-cell identical output to a
    /// direct `StatefulImage::render` into the same destination.
    #[test]
    fn src_offset_zero_matches_direct_render() {
        let mut protocol_helper = halfblocks_protocol(16, 16);
        let mut protocol_direct = halfblocks_protocol(16, 16);

        let full = Rect::new(0, 0, 8, 4);
        let mut buf_helper = Buffer::empty(Rect::new(0, 0, 8, 4));
        let mut buf_direct = Buffer::empty(Rect::new(0, 0, 8, 4));

        paint_halfblocks_partial(
            &mut protocol_helper,
            full,
            0,
            Rect::new(0, 0, 8, 4),
            &mut buf_helper,
        );
        StatefulImage::default().resize(Resize::Fit(None)).render(
            full,
            &mut buf_direct,
            &mut protocol_direct,
        );

        // Cells should match — halfblocks is position-independent.
        for y in 0..4u16 {
            for x in 0..8u16 {
                let h = buf_helper.cell((x, y)).unwrap();
                let d = buf_direct.cell((x, y)).unwrap();
                assert_eq!(h.symbol(), d.symbol(), "symbol mismatch at ({x},{y})");
                assert_eq!(h.style(), d.style(), "style mismatch at ({x},{y})");
            }
        }
    }

    /// `src_y_offset = k` should map destination row `dy` to source row
    /// `k + dy`.  This is the core invariant the scroll-clipping path
    /// depends on.
    #[test]
    fn positive_src_offset_shifts_source_rows() {
        let full = Rect::new(0, 0, 8, 6);
        // Build a reference frame of the full image.
        let mut protocol_ref = halfblocks_protocol(16, 24);
        let mut full_buf = Buffer::empty(Rect::new(0, 0, 8, 6));
        StatefulImage::default().resize(Resize::Fit(None)).render(
            full,
            &mut full_buf,
            &mut protocol_ref,
        );

        // Paint only rows 2..5 of the image, into destination rows 0..3.
        let mut protocol = halfblocks_protocol(16, 24);
        let mut dst_buf = Buffer::empty(Rect::new(0, 0, 8, 3));
        paint_halfblocks_partial(&mut protocol, full, 2, Rect::new(0, 0, 8, 3), &mut dst_buf);
        for dy in 0..3u16 {
            for dx in 0..8u16 {
                let dst_cell = dst_buf.cell((dx, dy)).unwrap();
                let src_cell = full_buf.cell((dx, 2 + dy)).unwrap();
                assert_eq!(dst_cell.symbol(), src_cell.symbol());
                assert_eq!(dst_cell.style(), src_cell.style());
            }
        }
    }

    /// When the destination rect extends past the destination buffer's
    /// own area, cells outside the buffer are silently dropped (ratatui
    /// returns `None` from `cell_mut`).  The helper must not panic.
    #[test]
    fn destination_clipping_silently_drops_out_of_bounds_cells() {
        let full = Rect::new(0, 0, 8, 4);
        let mut protocol = halfblocks_protocol(16, 16);
        // A 4×2 destination buffer with a 6×3 write rect at origin — the
        // extra 2 cols + 1 row must not panic.
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        paint_halfblocks_partial(&mut protocol, full, 0, Rect::new(0, 0, 6, 3), &mut buf);
    }

    /// When `src_y_offset` leaves fewer image rows than the destination
    /// needs, the trailing rows are left untouched (still default).
    #[test]
    fn src_offset_past_image_leaves_dst_default() {
        let full = Rect::new(0, 0, 8, 4);
        let mut protocol = halfblocks_protocol(16, 16);
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 6));
        paint_halfblocks_partial(&mut protocol, full, 5, Rect::new(0, 0, 8, 6), &mut buf);
        // src_y_offset 5 > full.height 4 → nothing should have been
        // written; every cell stays default.
        for y in 0..6u16 {
            for x in 0..8u16 {
                let c = buf.cell((x, y)).unwrap();
                assert_eq!(c.symbol(), " ");
            }
        }
    }

    /// Zero-area inputs are a no-op.
    #[test]
    fn zero_area_is_noop() {
        let mut protocol = halfblocks_protocol(16, 16);
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 4));
        paint_halfblocks_partial(
            &mut protocol,
            Rect::new(0, 0, 0, 4),
            0,
            Rect::new(0, 0, 4, 4),
            &mut buf,
        );
        paint_halfblocks_partial(
            &mut protocol,
            Rect::new(0, 0, 4, 4),
            0,
            Rect::new(0, 0, 0, 4),
            &mut buf,
        );
        // No panic, buf still default.
        for y in 0..4u16 {
            for x in 0..4u16 {
                assert_eq!(buf.cell((x, y)).unwrap().symbol(), " ");
            }
        }
    }
}
