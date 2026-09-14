//! Kitty *direct placement*: paint the visible band of an image by re-placing an
//! already-transmitted image with a source rectangle.
//!
//! The band is a **placement parameter**, not part of an encoding, so moving it costs one
//! short escape — no re-encode, no re-transmit, no `clear_area` flash.  That is what makes
//! this route cheaper than cropping and re-sending ([M3]), and it is the only route for a
//! terminal that places an already-transmitted image but does not implement the `U=1`
//! unicode-placeholder extension — `WezTerm` today, which is why `ratatui-image`'s Kitty
//! backend (placeholders only, and `pub(crate)`) cannot serve it.
//!
//! Every function here is a pure function of its arguments, so the escape formats and the
//! band's pixel geometry are unit-tested without a terminal.  See
//! `docs/dev/plans/image-partial-rendering.md` § "Design (M4 — direct placement)".
//!
//! [M3]: ../../docs/dev/plans/image-partial-rendering.md

use std::fmt::Write;

use base64::Engine as _;
use image::DynamicImage;
use ratatui::layout::Size;

/// Kitty's per-command base64 payload limit.  Chunking mirrors upstream's `transmit_virtual`
/// so the two backends put the same bytes on the wire, minus `U=1`.
const CHARS_PER_CHUNK: usize = 4096;
const CHUNK_SIZE: usize = (CHARS_PER_CHUNK / 4) * 3;

/// One image's geometry at one cell size: the cells it occupies and the size of one cell in
/// pixels.
///
/// The bitmap the caller resized is exactly `cells * font` pixels, because upstream's
/// [`Resize::resize`] pads to a whole number of cells — which is what makes the band's source
/// rectangle a multiple of the cell height, and therefore exact.  Deriving the pixel size
/// instead of carrying it keeps the two from ever disagreeing.
///
/// [`Resize::resize`]: ratatui_image::Resize::resize
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// The cell size the image was resized to.
    pub cells: Size,
    /// One cell, in pixels.
    pub font: (u16, u16),
}

impl Geometry {
    pub fn new(cells: Size, font: (u16, u16)) -> Self {
        Self { cells, font }
    }

    /// The resized bitmap's pixel dimensions.
    pub fn pixels(&self) -> (u32, u32) {
        (
            u32::from(self.cells.width) * u32::from(self.font.0),
            u32::from(self.cells.height) * u32::from(self.font.1),
        )
    }
}

/// The image id for `url` **at one geometry**.
///
/// Derived rather than allocated, so a rebuild re-transmits into the same slot instead of leaving
/// the previous image resident — strictly better than the random id per build that M1 inherits.
///
/// The geometry is part of the hash because an id names *one stored bitmap*: the same image at two
/// sizes is two bitmaps, and a shared id would have the later transmit overwrite the earlier one's
/// pixels.  Two blocks showing the same image at the same size therefore share one transmit — which
/// is the point — and still get their own placements ([`placement_id`]).
///
/// Zero is avoided because it means "no id" to the placement path.
pub fn image_id(url: &str, geometry: Geometry) -> u32 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    geometry.cells.width.hash(&mut hasher);
    geometry.cells.height.hash(&mut hasher);
    geometry.font.hash(&mut hasher);
    match hasher.finish() as u32 {
        0 => 1,
        id => id,
    }
}

/// The placement id for the block at `block_idx`.
///
/// One id per block is what makes replacement safe in both directions.  Placements are keyed by
/// `(image_id, placement_id)` — in WezTerm's store and in kitty — and a re-place with the same pair
/// *replaces* rather than adds, so: the same image shown twice gets two placements and both stay
/// visible (a constant id would have the second replace the first), and a block that moves is
/// re-placed under its own id, replacing its old position instead of leaving a copy behind.
///
/// Non-zero, since a placement id of zero is the protocol's "no id" case.  A document with more
/// than `u32::MAX` blocks would collide — unreachable, and the symptom would be one image missing,
/// not corruption.
pub fn placement_id(block_idx: usize) -> u32 {
    (block_idx as u32).saturating_add(1)
}

/// The one-time transmit: the whole image as raw RGBA (`f=32,t=d`), in base64 chunks.
///
/// No `U=1`: a virtual placement exists only to be driven by unicode placeholders, and this
/// backend places explicitly.  Without it the transmit stores the image and displays nothing
/// until [`place`] asks for it, which is exactly the lifecycle wanted.
pub fn transmit(id: u32, image: &DynamicImage) -> String {
    let rgba = image.to_rgba8();
    let bytes = rgba.as_raw();
    let (width, height) = (image.width(), image.height());
    let chunk_count = bytes.len().div_ceil(CHUNK_SIZE);

    let mut out = String::with_capacity(bytes.len() * 4 / 3 + chunk_count * 48);
    if chunk_count == 0 {
        // A zero-pixel image has nothing to send; `place` would then place nothing, so the
        // caller is expected to have refused this image already.
        return out;
    }

    for (i, chunk) in bytes.chunks(CHUNK_SIZE).enumerate() {
        out.push_str("\x1b_Gq=2,");
        if i == 0 {
            write!(out, "i={id},a=t,f=32,t=d,s={width},v={height},").unwrap();
        }
        // `m=0` means the payload is complete.
        let more = u8::from(i + 1 < chunk_count);
        write!(out, "m={more};").unwrap();
        base64::engine::general_purpose::STANDARD.encode_string(chunk, &mut out);
        out.push_str("\x1b\\");
    }
    out
}

/// The cell symbol that puts the band of `geometry` at `dst`, which is its on-screen rect.
///
/// `placement_id` is this block's ([`placement_id`]) and `id` names the stored image; together they
/// are the terminal's key for the placement, so re-placing replaces this block's previous placement
/// and nothing else's.
///
/// `skip` is how many of the image's rows sit above `dst`, so the source rectangle starts at
/// `skip * cell_height` — the band's own rows, cropped by the terminal out of what it already
/// holds.  `transmit` is carried here when the image has not been sent yet, in the same cell
/// and *before* the placement, so the placement never precedes the data it needs.
///
/// The symbol begins by absolutely positioning the cursor and ends one cell to the right of
/// where it started: `ratatui-crossterm` tracks a *cell* coordinate, not a display column, so
/// a symbol that moved the cursor and left it elsewhere would corrupt the next cell that gets
/// no `MoveTo`.
pub fn place(
    id: u32,
    placement_id: u32,
    geometry: Geometry,
    skip: u16,
    dst: ratatui::layout::Rect,
    transmit: Option<&str>,
) -> String {
    let (pixels_w, _) = geometry.pixels();
    let font_h = u32::from(geometry.font.1);
    let src_y = u32::from(skip) * font_h;
    let src_h = u32::from(dst.height) * font_h;

    let mut out =
        String::with_capacity(transmit.map_or(0, str::len) + usize::from(dst.height) * 16 + 96);

    // 1 — erase the band on the terminal, one ECH sweep per row.  `[Image: alt]` is a *glyph*
    // and glyphs draw above images, so it has to be erased rather than covered; and a buffer
    // cell marked `Skip` cannot do it, because such a cell is dropped from the update stream
    // whether or not its content changed.
    for row in 0..dst.height {
        write!(
            out,
            "\x1b[{};{}H\x1b[{}X",
            dst.y + row + 1,
            dst.x + 1,
            dst.width
        )
        .unwrap();
    }

    // 2 — the one-time transmit rides the first placement.
    if let Some(transmit) = transmit {
        out.push_str(transmit);
    }

    // 3 — the placement: source rectangle `x,y,w,h` in image pixels, extent `c,r` in cells,
    // `C=1` so the terminal leaves the cursor alone.
    write!(
        out,
        "\x1b[{};{}H\x1b_Gq=2,i={id},p={placement_id},a=p,x=0,y={src_y},w={pixels_w},h={src_h},c={},r={},C=1\x1b\\",
        dst.y + 1,
        dst.x + 1,
        dst.width,
        dst.height
    )
    .unwrap();

    // 4 — leave the cursor one cell along, which is where the backend's `last_pos` bookkeeping
    // believes it is.  Moving past the right margin is clamped by the terminal, and harmless.
    write!(out, "\x1b[{};{}H", dst.y + 1, dst.x + 2).unwrap();
    out
}

/// Remove one block's placement, keeping the image data — `d=i` (lowercase) is placements-only, so
/// an image that scrolls back into view is re-placed for free.
///
/// This is only needed where nothing else removes it.  A placement is anchored to *screen
/// cells*, so scrolled content moves out from under it; the data-deleting form (`d=I`) would
/// also free the stored image, but the resident-image leak it would fix is the one M1 already
/// documents, and it would cost a re-transmit on every scroll back.
///
/// Both ids matter: the same stored image can be placed by several blocks at once, and this must
/// remove exactly one of them.
pub fn delete_placement(id: u32, placement_id: u32) -> String {
    format!("\x1b_Gq=2,i={id},p={placement_id},a=d,d=i\x1b\\")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn geometry() -> Geometry {
        // 40 cells wide, 30 tall, 8x16 px per cell.
        Geometry::new(Size::new(40, 30), (8, 16))
    }

    #[test]
    fn pixels_are_the_cell_size_times_the_font() {
        assert_eq!(geometry().pixels(), (320, 480));
    }

    #[test]
    fn the_image_id_is_per_url_and_geometry_and_never_zero() {
        let small = geometry();
        let wider = Geometry::new(Size::new(80, 30), (8, 16));
        assert_eq!(
            image_id("https://example.com/a.png", small),
            image_id("https://example.com/a.png", small)
        );
        assert_ne!(
            image_id("https://example.com/a.png", small),
            image_id("https://example.com/b.png", small)
        );
        // One id names one stored *bitmap*: the same image at another size must not share one, or
        // whichever transmit lands last would overwrite the other's pixels.
        assert_ne!(
            image_id("https://example.com/a.png", small),
            image_id("https://example.com/a.png", wider)
        );
        // A mismatched font is a different pixel grid at the same cell size.
        assert_ne!(
            image_id("a.png", small),
            image_id("a.png", Geometry::new(small.cells, (9, 16)))
        );
        // Whatever it hashes to, it must not be the "no id" value the placement path skips.
        assert_ne!(image_id("", small), 0);
        assert_ne!(image_id("some/path.png", small), 0);
    }

    #[test]
    fn the_placement_id_is_per_block_and_never_zero() {
        // Zero is the protocol's "no id", so the first block must not land on it.
        assert_ne!(placement_id(0), 0);
        assert_ne!(placement_id(1), 0);
        // Two blocks showing one image must not share a placement id: they would replace each
        // other, and the second block's position would show nothing.
        assert_ne!(placement_id(0), placement_id(1));
        assert_ne!(placement_id(7), placement_id(8));
    }

    #[test]
    fn the_transmit_chunks_and_marks_every_chunk_but_the_last() {
        // 1 pixel = 4 bytes of RGBA, so a single command.
        let image = DynamicImage::ImageRgba8(image::ImageBuffer::new(1, 1));
        let seq = transmit(7, &image);
        assert!(
            seq.starts_with("\x1b_Gq=2,i=7,a=t,f=32,t=d,s=1,v=1,m=0;"),
            "{seq:?}"
        );
        assert!(seq.ends_with("\x1b\\"), "{seq:?}");
        assert_eq!(seq.matches("\x1b_G").count(), 1);

        // Big enough to need two: 3072 payload bytes per chunk, 4 bytes per pixel.
        let image = DynamicImage::ImageRgba8(image::ImageBuffer::new(1024, 1));
        let seq = transmit(7, &image);
        assert_eq!(seq.matches("\x1b_G").count(), 2);
        assert_eq!(seq.matches("m=1;").count(), 1);
        assert_eq!(seq.matches("m=0;").count(), 1);
        // The id, format and dimensions are stated once, not per chunk.
        assert_eq!(seq.matches("a=t").count(), 1);
        assert_eq!(seq.matches("s=1024,v=1").count(), 1);
    }

    #[test]
    fn a_zero_pixel_image_transmits_nothing() {
        let image = DynamicImage::ImageRgba8(image::ImageBuffer::new(0, 0));
        assert!(transmit(7, &image).is_empty());
    }

    #[test]
    fn the_band_is_the_source_rectangle_of_the_full_width() {
        // Rows 5..9 of a 30-row image, at 16 px per row: source y = 80, height = 64.
        let dst = Rect::new(3, 11, 40, 4);
        let symbol = place(9, 2, geometry(), 5, dst, None);
        assert!(
            symbol.contains(",x=0,y=80,w=320,h=64,c=40,r=4,"),
            "the source rect must be the band, in image pixels: {symbol:?}"
        );
    }

    #[test]
    fn the_escape_erases_each_row_before_placing_and_restores_the_cursor() {
        let dst = Rect::new(3, 11, 40, 3);
        let symbol = place(9, 2, geometry(), 0, dst, None);

        // One ECH sweep per row, absolutely positioned, and all of them *before* the placement:
        // the placeholder glyph would otherwise be drawn over the image.
        let first_place = symbol.find("a=p").expect("a placement");
        for row in 0..3u16 {
            let erase = format!("\x1b[{};4H\x1b[40X", 12 + row);
            let at = symbol
                .find(&erase)
                .unwrap_or_else(|| panic!("missing the erase sweep for row {row}: {symbol:?}"));
            assert!(at < first_place, "erase sweeps must precede the placement");
        }

        // The cursor ends one cell right of the band's first cell, where `ratatui-crossterm`'s
        // `last_pos` believes it is.
        assert!(symbol.ends_with("\x1b[12;5H"), "{symbol:?}");
        // And `C=1` keeps the terminal from moving it while placing.
        assert!(symbol.contains(",C=1\x1b\\"), "{symbol:?}");
    }

    #[test]
    fn the_transmit_is_carried_once_and_before_the_placement() {
        let dst = Rect::new(0, 0, 40, 2);
        let symbol = place(
            9,
            2,
            geometry(),
            0,
            dst,
            Some("\x1b_Gq=2,i=9,a=t,m=0;AAAA\x1b\\"),
        );
        let transmit_at = symbol.find("a=t").expect("the transmit");
        let place_at = symbol.find("a=p").expect("the placement");
        assert!(
            transmit_at < place_at,
            "data must precede the placement that needs it"
        );

        // With no transmit to carry, the placement stands alone.
        let re_placed = place(9, 2, geometry(), 0, dst, None);
        assert!(!re_placed.contains("a=t"), "{re_placed:?}");
        assert!(re_placed.contains("a=p"), "{re_placed:?}");
    }

    #[test]
    fn the_delete_names_one_placement_and_keeps_the_image_data() {
        let seq = delete_placement(9, 3);
        // Placements-only (`d=i`, lowercase) for one (image, placement) pair.
        assert!(seq.contains("a=d"), "{seq:?}");
        assert!(seq.contains("d=i"), "{seq:?}");
        assert!(seq.contains("i=9"), "{seq:?}");
        assert!(seq.contains("p=3"), "{seq:?}");
        // `d=I` would drop the data too; that is deliberately not what this emits, so that an
        // image scrolling back into view is re-placed for free.
        assert!(!seq.contains("d=I"), "{seq:?}");
    }
}
