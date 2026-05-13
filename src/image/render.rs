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
//! `paint_halfblocks_partial` exploits this: it takes a pre-rendered
//! scratch `Buffer` (built synchronously by `ImageCache` when the pair
//! is first constructed) and copies only the visible rows into the
//! destination buffer.  The encoding work has already been done by the
//! time this function is called — each frame is only cell-copies.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

/// Copy a pre-rendered halfblocks `scratch` buffer into `buf`, clipped
/// to the visible `dst_rect` and offset vertically by `src_y_offset`
/// image rows.
///
/// Parameters:
///
/// * `scratch` — a `Buffer` containing the halfblocks cells for the
///   full image at `full_rect`'s dimensions.  Built by `ImageCache` on
///   cold path; reused across every frame.
/// * `full_rect` — the image's natural rectangle (stable across frames):
///   `width = image_max_width`, `height = image_max_height`, origin
///   `(0, 0)`.
/// * `src_y_offset` — how many image rows have scrolled past the top.
///   When the image's top is above the viewport, this is positive;
///   when fully visible, it's zero.
/// * `dst_rect` — the on-screen region to paint into.  Cells outside
///   `buf`'s own area are silently dropped (ratatui's `cell_mut` returns
///   `None` on out-of-bounds positions).
/// * `buf` — the destination `Buffer` (the frame buffer supplied to
///   every `render` call).
/// * `bg` — theme background color used wherever the scratch cell has
///   `Color::Reset` for its background.  ratatui_image's halfblocks
///   renderer leaves `Reset` for letter-box cells around an
///   aspect-mismatched image and for fully transparent input pixels;
///   without this substitution those cells would punch through to the
///   terminal's own background instead of the document's themed
///   background — visible as dark bands while scrolling and around any
///   partially-visible image.
///
/// This function does **not** bounds-check `src_y_offset` against
/// `full_rect.height`; passing an offset that leaves fewer rows than
/// `dst_rect.height` produces blank cells at the bottom, which is the
/// desired behaviour when an image is scrolling off the bottom of the
/// viewport.
pub fn paint_halfblocks_partial(
    scratch: &Buffer,
    full_rect: Rect,
    src_y_offset: u16,
    dst_rect: Rect,
    buf: &mut Buffer,
    bg: Color,
) {
    if full_rect.width == 0 || full_rect.height == 0 || dst_rect.width == 0 || dst_rect.height == 0
    {
        return;
    }

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
            let mut copied = src_cell.clone();
            // Substitute the theme bg wherever the scratch cell carries
            // `Reset` (letter-box, transparent pixels, the lower half of
            // an `▀` cell whose bottom pixel was transparent).  The fg is
            // left alone — the halfblocks glyph's `▀` color is the image's
            // top-pixel color and must be preserved.
            if copied.bg == Color::Reset {
                copied.bg = bg;
            }
            if let Some(dst_cell) = buf.cell_mut((dst_rect.x + dx, dst_rect.y + dy)) {
                *dst_cell = copied;
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
    use ratatui::widgets::StatefulWidget;
    use ratatui_image::picker::Picker;
    use ratatui_image::{Resize, StatefulImage};

    /// Build a halfblocks scratch buffer the same way `ImageCache` does
    /// on cold path — render a uniform image through a halfblocks
    /// picker into a Buffer sized to `rect`.
    fn halfblocks_scratch(rect: Rect) -> Buffer {
        let picker = Picker::from_fontsize((1, 2));
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            u32::from(rect.width) * 2,
            u32::from(rect.height) * 4,
            image::Rgba([40, 80, 120, 255]),
        ));
        let mut protocol = picker.new_resize_protocol(img);
        let mut buf = Buffer::empty(rect);
        StatefulImage::default()
            .resize(Resize::Fit(None))
            .render(rect, &mut buf, &mut protocol);
        buf
    }

    /// `src_y_offset = k` should map destination row `dy` to source row
    /// `k + dy`.  This is the core invariant the scroll-clipping path
    /// depends on.
    #[test]
    fn positive_src_offset_shifts_source_rows() {
        let full = Rect::new(0, 0, 8, 6);
        let scratch = halfblocks_scratch(full);

        let mut dst_buf = Buffer::empty(Rect::new(0, 0, 8, 3));
        paint_halfblocks_partial(
            &scratch,
            full,
            2,
            Rect::new(0, 0, 8, 3),
            &mut dst_buf,
            Color::Reset,
        );
        for dy in 0..3u16 {
            for dx in 0..8u16 {
                let dst_cell = dst_buf.cell((dx, dy)).unwrap();
                let src_cell = scratch.cell((dx, 2 + dy)).unwrap();
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
        let scratch = halfblocks_scratch(full);
        // A 4×2 destination buffer with a 6×3 write rect at origin — the
        // extra 2 cols + 1 row must not panic.
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        paint_halfblocks_partial(
            &scratch,
            full,
            0,
            Rect::new(0, 0, 6, 3),
            &mut buf,
            Color::Reset,
        );
    }

    /// When `src_y_offset` leaves fewer image rows than the destination
    /// needs, the trailing rows are left untouched (still default).
    #[test]
    fn src_offset_past_image_leaves_dst_default() {
        let full = Rect::new(0, 0, 8, 4);
        let scratch = halfblocks_scratch(full);
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 6));
        paint_halfblocks_partial(
            &scratch,
            full,
            5,
            Rect::new(0, 0, 8, 6),
            &mut buf,
            Color::Reset,
        );
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
        let scratch = halfblocks_scratch(Rect::new(0, 0, 4, 4));
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 4));
        paint_halfblocks_partial(
            &scratch,
            Rect::new(0, 0, 0, 4),
            0,
            Rect::new(0, 0, 4, 4),
            &mut buf,
            Color::Reset,
        );
        paint_halfblocks_partial(
            &scratch,
            Rect::new(0, 0, 4, 4),
            0,
            Rect::new(0, 0, 0, 4),
            &mut buf,
            Color::Reset,
        );
        // No panic, buf still default.
        for y in 0..4u16 {
            for x in 0..4u16 {
                assert_eq!(buf.cell((x, y)).unwrap().symbol(), " ");
            }
        }
    }
}
