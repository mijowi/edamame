//! LaTeX display math → SVG → `DynamicImage` pipeline.
//!
//! [`resolve_latex`] is the public entry point (used by the App decode
//! worker, same contract as [`super::mermaid::resolve_mermaid`]).  The
//! pipeline is pure Rust — no node, no system TeX: RaTeX parses the LaTeX,
//! lays it out in display style, flattens to a display list, and serializes
//! an SVG.  We emit **`<text>`** (not embedded glyph outlines): RaTeX names
//! its faces `font-family="KaTeX_*"`, and the shared fontdb — which carries
//! the bundled KaTeX TTFs, see [`crate::image::svg`] — resolves them during
//! the rasterize step, exactly how the mermaid path renders.  That keeps a
//! single font subsystem rather than the parallel `ab_glyph` stack the
//! `standalone`/`embed-fonts` features would pull in.
//!
//! RaTeX 0.1.x is pre-1.0 with known panic bugs, so the render is wrapped in
//! `catch_unwind` like the mermaid renderer.  The shared cache-key URL
//! scheme, [`DiagramSource`](super::common::DiagramSource) and
//! [`DiagramError`] live in [`super::common`].

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::image::{rasterize_svg, LoadedImage, SvgScaleMode, SvgSizing};

use super::common::{panic_message, DiagramError};

/// Maximum LaTeX source length we will attempt to render.  RaTeX has no
/// internal bound, so an over-cap formula gets the same clean failure →
/// placeholder path as an over-cap mermaid diagram.
const MAX_LATEX_SOURCE_BYTES: usize = 64 * 1024;

/// Cell-height → RaTeX `font_size` conversion factor.
///
/// Two unit mismatches stand between the terminal's cell *height* (pixels)
/// and the value RaTeX expects (user units per em):
///
/// * RaTeX emits its SVG labelled in `pt`, and usvg rasterizes CSS `pt` at
///   96 dpi — one `pt` becomes 96/72 px.
/// * A terminal cell spans more than one text em: ascent + descent plus
///   the terminal's configured line height ≈ 1.2–1.4 em in practice.
///
/// The `1 / (96/72 × 1.25)` term lands the formula's x-height on one text
/// em (1.25 is the middle of the common 1.2–1.4 line-height range); the
/// leading `1.25` then sets display math ~25% larger than the surrounding
/// body text, the way typeset display equations are conventionally set —
/// prominent, and large enough to keep subscripts and superscripts legible
/// in the terminal.  Exactness is impossible without font metrics from the
/// terminal, so the factor targets a look within ±10% across terminals.
const LATEX_EM_TO_CELL: f64 = 1.25 / (96.0 / 72.0 * 1.25);

/// Internal margin baked into every formula's SVG, as a fraction of its em
/// (RaTeX's `padding` is in the same user units as the glyph coordinates,
/// so an em-relative value scales with the formula).  Keeps glyphs off the
/// image edge so a formula never rubs against the frame — visible in the
/// HTML export, where the figure sits on white, and honoured in-app once
/// the margin flattens onto the document background.  Because it lives in
/// the shared [`render_latex_svg`] output, the in-app raster and the export
/// PNG get the identical margin.
const LATEX_PADDING_EMS: f64 = 0.3;

/// Render a LaTeX display-math source all the way to a `LoadedImage`,
/// suitable for dropping straight into the image cache — same contract as
/// [`super::mermaid::resolve_mermaid`].
///
/// Sizing: the SVG's user-unit scale is RaTeX's `font_size` (em units).
/// We derive it from the terminal's cell pixel height via [`LATEX_EM_TO_CELL`]
/// so the formula's x-height matches body text (not one full cell), drop
/// RaTeX's default padding (a fixed frame dwarfing the glyphs), then
/// rasterize with `SvgScaleMode::Natural` (downscale only) so a formula
/// wider than the column shrinks to fit but never balloons — unlike
/// Mermaid, a formula has a meaningful natural size.  The rasterized image
/// is then fitted to the cell grid ([`fit_latex_to_cell_grid`]): the glyphs
/// are painted in `fg` (the theme's text colour) and flattened onto `bg`
/// (the document background) so dark themes stay legible and scrolling
/// doesn't smear a transparent formula black.
pub fn resolve_latex(
    url: String,
    source: &str,
    max_cells: Option<(u16, u16)>,
    font_size: Option<(u16, u16)>,
    fg: [u8; 4],
    bg: [u8; 4],
) -> Result<LoadedImage, DiagramError> {
    let svg = render_latex_svg(source, fg, font_size)?;
    let image = rasterize_svg(
        &svg,
        SvgSizing {
            envelope: max_cells,
            font_size,
            mode: SvgScaleMode::Natural,
        },
        None, // transparent — fitted / flattened onto `bg` below
    )
    .map_err(DiagramError::from)?;
    Ok(LoadedImage {
        url,
        image: fit_latex_to_cell_grid(image, font_size, bg),
        scratch: None,
        sliced: None,
        direct: None,
    })
}

/// Prepare a formula image for the terminal's cell grid: symmetric
/// breathing room, opaque flatten onto the document background, then
/// vertical centring to an exact whole number of cells.
///
/// The editor reserves image rows in whole cells (`aspect_rows_of` =
/// `ceil(pixels / cell_height)`), and `paint_images` fits the image into
/// the reserved rect **downward-only, flush to the top**.  Without the
/// centring step, the rounding slack between the image's pixel height
/// and the reserved whole-cell height would land entirely below the
/// image as a letter-box gap — the "only blank below the formula" look.
/// Padding up to `rows × cell_height` with the background colour and
/// centring the content in it turns that slack into symmetric top/bottom
/// margins instead, so a formula's vertical rhythm reads like a text
/// line's.
fn fit_latex_to_cell_grid(
    image: image::DynamicImage,
    font_size: Option<(u16, u16)>,
    bg: [u8; 4],
) -> image::DynamicImage {
    let image = add_formula_breathing_room(image, font_size);
    let image = flatten_to_background(image, bg);
    center_on_cell_grid(image, font_size, bg)
}

/// Pad `image` (already opaque, background-coloured) vertically so its
/// height is an exact multiple of the terminal cell height, content
/// centred: `extra = rows × cell_h - height` split equally above and
/// below.
fn center_on_cell_grid(
    image: image::DynamicImage,
    font_size: Option<(u16, u16)>,
    bg: [u8; 4],
) -> image::DynamicImage {
    use image::{GenericImageView, ImageBuffer, Rgba};
    let cell_h = u32::from(font_size.map_or(16, |(_, h)| h.max(1)));
    let (w, h) = image.dimensions();
    let rows = h.div_ceil(cell_h).max(1);
    let total = rows * cell_h;
    if total <= h {
        return image;
    }
    let extra = total - h;
    let top = extra / 2;
    let mut canvas = ImageBuffer::from_pixel(w, total, Rgba([bg[0], bg[1], bg[2], 255]));
    image::imageops::overlay(&mut canvas, &image, 0, i64::from(top));
    image::DynamicImage::ImageRgba8(canvas)
}

/// Composite a formula image onto the document background colour,
/// replacing transparency with opaque `bg`.
///
/// Two consumers need an opaque image:
///
/// * **Halfblocks (active scroll / partial visibility)** encode each cell
///   through `to_rgb8()`, which *drops the alpha channel* — a transparent
///   pixel's RGB is read as-is, and formula transparency is
///   `Rgba([0,0,0,0])`, i.e. black.  With the native protocol idling
///   during scroll (`paint_images` falls back to the halfblocks scratch
///   while `is_scrolling`), every transparent region around a formula
///   painted black — the "black blob while scrolling" bug.  Flattened
///   onto the document `bg`, those regions encode as `bg`, which is
///   exactly what the native composite shows.
/// * **The letter-box / trailing margin** a formula's rect reserves
///   (the rect spans the whole column): the same reasoning — encode as
///   `bg`, not black.
///
/// Result is visually identical to the transparent composite whenever the
/// terminal honours alpha (native kitty/iTerm2/sixel paths), because the
/// cells beneath the image are painted with the same document `bg`.
fn flatten_to_background(image: image::DynamicImage, bg: [u8; 4]) -> image::DynamicImage {
    use image::GenericImageView;
    let [br, bg_, bb, ba] = bg;
    let (w, h) = image.dimensions();
    let mut out = image::ImageBuffer::new(w, h);
    let src = image.to_rgba8();
    for (x, y, px) in src.enumerate_pixels() {
        let [r, g, b, a] = px.0;
        // Straight alpha-over: the formula PNG is transparent or fully
        // opaque in practice (no partial coverage), but stay exact for
        // antialiased glyph edges.
        let alpha = f32::from(a) / 255.0;
        let ba_ = f32::from(ba) / 255.0;
        let mix = |s: u8, d: u8| {
            (f32::from(s) * alpha + f32::from(d) * ba_ * (1.0 - alpha)).round() as u8
        };
        out.put_pixel(
            x,
            y,
            image::Rgba([mix(r, br), mix(g, bg_), mix(b, bb), 255]),
        );
    }
    image::DynamicImage::ImageRgba8(out)
}

/// Pad a formula image with transparent rows above and below so its
/// vertical rhythm matches the text grid.
///
/// Text lines carry their own inter-line gap: a terminal cell is taller
/// than the glyph box (WezTerm line-height 1.15, most fonts 1.2+), so two
/// text lines leave roughly half a cell's worth of background between
/// glyph boxes on each side.  A rendered formula has no such built-in
/// margin — `paint_images` overlays it flush against the reserved cell
/// rect's top edge — so formulas and images sit visually tighter against
/// their neighbours than text does.  ~1/10 of the cell height per side
/// restores the look of an ordinary line gap.  The extra rows are
/// transparent, so layout, aspect-row accounting (`aspect_rows_of`) and
/// the block's reserved height all follow automatically from the new
/// dimensions.
fn add_formula_breathing_room(
    image: image::DynamicImage,
    font_size: Option<(u16, u16)>,
) -> image::DynamicImage {
    use image::imageops::overlay;
    use image::{GenericImageView, ImageBuffer, Rgba};
    let cell_h = u32::from(font_size.map_or(16, |(_, h)| h.max(1)));
    let pad = (cell_h / 10).clamp(1, 6);
    let (w, h) = image.dimensions();
    let mut canvas = ImageBuffer::from_pixel(w, h + 2 * pad, Rgba([0, 0, 0, 0]));
    overlay(&mut canvas, &image.to_rgba8(), 0, i64::from(pad));
    image::DynamicImage::ImageRgba8(canvas)
}

/// Render a LaTeX display-math source to an SVG string, wrapping any panic
/// in a [`DiagramError`] (RaTeX 0.1.x can panic on pathological input —
/// same defence as the mermaid renderer).  Enforces the same
/// [`MAX_LATEX_SOURCE_BYTES`] cap as [`resolve_latex`].
///
/// This is the shared entry point for both the TUI raster path (via
/// [`resolve_latex`]) and the HTML exporter, which rasterizes the returned
/// SVG to a PNG rather than inlining it — the exact parallel to
/// [`super::mermaid::render_mermaid_svg`].
///
/// * `fg` — glyph colour as RGBA.  The TUI passes the theme's text colour;
///   the exporter passes opaque black for a light document background.
/// * `font_size` — the terminal cell's `(width, height)` in pixels; the
///   cell *height* drives RaTeX's `font_size` through [`LATEX_EM_TO_CELL`]
///   so the formula's x-height matches the surrounding body text.  `None`
///   (the exporter's case) falls back to a 16 px cell.
pub fn render_latex_svg(
    source: &str,
    fg: [u8; 4],
    font_size: Option<(u16, u16)>,
) -> Result<String, DiagramError> {
    if source.len() > MAX_LATEX_SOURCE_BYTES {
        return Err(DiagramError::RenderFailed(format!(
            "latex source too large: {} bytes (max {MAX_LATEX_SOURCE_BYTES})",
            source.len()
        )));
    }
    let outcome = {
        let _expected = crate::terminal::ExpectedPanic::new();
        catch_unwind(AssertUnwindSafe(|| {
            render_latex_svg_inner(source, fg, font_size)
        }))
    }
    .map_err(|payload| {
        DiagramError::RenderFailed(format!("latex render panic: {}", panic_message(&payload)))
    })?;
    outcome.map_err(|e| DiagramError::RenderFailed(format!("{e:#}")))
}

/// Unwrapped RaTeX pipeline: parse → layout (display style) → display list
/// → `<text>` SVG.
fn render_latex_svg_inner(
    source: &str,
    fg: [u8; 4],
    font_size: Option<(u16, u16)>,
) -> Result<String, ratex_parser::error::ParseError> {
    use ratex_layout::layout_options::LayoutOptions;
    use ratex_layout::{layout, to_display_list};
    use ratex_parser::parse;
    use ratex_svg::{render_to_svg, SvgOptions};
    use ratex_types::color::Color;

    let ast = parse(source)?;
    let [r, g, b, a] = fg;
    let options = LayoutOptions::default().with_color(Color::new(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        f32::from(a) / 255.0,
    ));
    let lbox = layout(&ast, &options);
    let display_list = to_display_list(&lbox);
    // One RaTeX em per ~0.6 cell-height pixel (see `LATEX_EM_TO_CELL`): the
    // formula's x-height then matches the surrounding body text.  Fall back
    // to a 16 px cell (a common terminal default) when unknown.
    let em = font_size.map_or(16.0, |(_, h)| f64::from(h.max(1))) * LATEX_EM_TO_CELL;
    // Text output, not embedded glyph outlines: `embed_glyphs = false`
    // emits `<text font-family="KaTeX_*">`, and the shared fontdb (which
    // carries the bundled KaTeX faces, see `image::svg`) resolves those
    // families during the rasterize step — exactly how the mermaid path
    // renders.  Single font subsystem; no standalone/embed-fonts features
    // and their parallel ab_glyph stack.
    let svg = render_to_svg(
        &display_list,
        &SvgOptions {
            embed_glyphs: false,
            font_size: em,
            // RaTeX's default padding (a fixed 10 user units per side) is a
            // large, scale-invariant frame next to body-sized text.  We
            // replace it with an em-relative margin (`LATEX_PADDING_EMS`)
            // so glyphs keep a small, proportional gap from the image edge
            // — the padding the HTML export and the in-app raster share.
            // The breathing-room step adds the extra text-line gap on top.
            padding: em * LATEX_PADDING_EMS,
            ..SvgOptions::default()
        },
    );
    Ok(svg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Display math must rasterize to a real image through the same
    /// `LoadedImage` contract as mermaid.  The KaTeX faces are bundled
    /// into the shared fontdb (`image::svg`), so this needs no system
    /// fonts and runs in CI — the render path's regression guard.
    #[test]
    fn latex_display_math_renders_to_loaded_image() {
        let loaded = resolve_latex(
            "test".into(),
            r"x^2 + y^2 = z^2",
            Some((80, 24)),
            Some((8, 16)),
            [0xcc, 0xcc, 0xcc, 255],
            [0x1a, 0x1a, 0x1a, 255],
        )
        .expect("trivial display math should render");
        assert!(loaded.image.width() > 0);
        assert!(loaded.image.height() > 0);
    }

    /// The rendered formula's pixel height must track the terminal cell
    /// font size (16 px cell → roughly one text line), not balloon to the
    /// whole image envelope — the bug where display math rendered huge.
    /// Bundled fonts → runs in CI.
    #[test]
    fn latex_image_height_tracks_cell_font_size() {
        // Envelope is 40 cells tall but a single-line formula must come
        // out near one cell (16 px) tall — Natural mode, no fill.
        let loaded = resolve_latex(
            "test".into(),
            r"x^2 + y^2 = z^2",
            Some((80, 40)),
            Some((8, 16)),
            [0xcc, 0xcc, 0xcc, 255],
            [0x1a, 0x1a, 0x1a, 255],
        )
        .expect("display math should render");
        // Natural-mode raster keeps the SVG's own size: with the em bump
        // (`LATEX_EM_TO_CELL`) and the em-relative internal padding
        // (`LATEX_PADDING_EMS`), a single line of math at a 16 px cell
        // rasterizes to a few tens of pixels and then rounds up to a whole
        // number of cells — a couple of cells tall, nowhere near the 640 px
        // a full-envelope fill of the 40-cell envelope would produce.
        let h = loaded.image.height();
        assert!(
            (16..=64).contains(&h),
            "single-line formula should be a few cells tall, got {h}px"
        );
    }

    /// The breathing-room pad scales with the reported cell height and
    /// keeps the formula's pixels centred vertically inside it (no
    /// content shift, only transparent margin added top and bottom).
    #[test]
    fn breathing_room_pads_transparent_rows_above_and_below() {
        use image::GenericImageView;
        use image::Rgba;
        // 4×8 solid-red image, cell height 30 → pad 3 rows each side.
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            4,
            8,
            Rgba([200, 0, 0, 255]),
        ));
        let padded = add_formula_breathing_room(img, Some((8, 30)));
        assert_eq!(padded.dimensions(), (4, 8 + 2 * 3));
        let rgba = padded.to_rgba8();
        // Top pad rows transparent, first content row red.
        assert_eq!(rgba.get_pixel(0, 0).0[3], 0);
        assert_eq!(rgba.get_pixel(0, 2).0[3], 0);
        assert_eq!(rgba.get_pixel(0, 3).0, [200, 0, 0, 255]);
        // Bottom pad rows transparent, last content row red.
        // content 3..11, bottom pad 11..14 (height 8+2*3).
        assert_eq!(rgba.get_pixel(0, 8 + 3).0[3], 0); // first bottom pad
        assert_eq!(rgba.get_pixel(0, 8 + 2 * 3 - 1).0[3], 0); // last row
        assert_eq!(rgba.get_pixel(0, 8 + 3 - 1).0, [200, 0, 0, 255]); // last content
    }

    /// Unknown cell size falls back to a 16 px cell (pad 1).
    #[test]
    fn breathing_room_falls_back_to_a_default_cell_height() {
        let img = image::DynamicImage::new_rgba8(2, 2);
        let padded = add_formula_breathing_room(img, None);
        assert_eq!(padded.height(), 2 + 2);
    }

    /// Transparent formula pixels must flatten onto the document
    /// background, never survive as black — halfblocks encode via
    /// `to_rgb8()`, which reads a transparent pixel's RGB as-is
    /// (`Rgba([0,0,0,0])` → black).
    #[test]
    fn flatten_composites_transparency_onto_the_document_background() {
        use image::{GenericImageView, Rgba};
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(2, 1, |x, _| {
            if x == 0 {
                Rgba([0, 0, 0, 0])
            } else {
                Rgba([200, 0, 0, 255])
            }
        }));
        let out = flatten_to_background(img, [10, 20, 30, 255]);
        let rgba = out.to_rgba8();
        assert_eq!(
            rgba.get_pixel(0, 0).0,
            [10, 20, 30, 255],
            "transparent → bg"
        );
        assert_eq!(
            rgba.get_pixel(1, 0).0,
            [200, 0, 0, 255],
            "opaque content kept"
        );
        assert_eq!(rgba.get_pixel(0, 0).0[3], 255, "output fully opaque");
        assert_eq!(out.dimensions(), (2, 1), "dimensions unchanged");
    }

    /// Rounding slack between a formula's pixel height and the reserved
    /// whole-cell rows must split above AND below the content (vertical
    /// centring) — never all below, which would read as a gap only under
    /// the formula.
    #[test]
    fn cell_grid_centring_splits_rounding_slack_evenly() {
        use image::{GenericImageView, Rgba};
        // 2×10 opaque red; cell height 30 → ceil(10/30)=1 row = 30px →
        // extra 20px, 10 above and 10 below.
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            2,
            10,
            Rgba([200, 0, 0, 255]),
        ));
        let out = center_on_cell_grid(img, Some((8, 30)), [10, 20, 30, 255]);
        assert_eq!(out.dimensions(), (2, 30));
        let rgba = out.to_rgba8();
        assert_eq!(rgba.get_pixel(0, 0).0, [10, 20, 30, 255], "top pad = bg");
        assert_eq!(rgba.get_pixel(0, 9).0, [10, 20, 30, 255], "top half slack");
        assert_eq!(
            rgba.get_pixel(0, 10).0,
            [200, 0, 0, 255],
            "content starts at 10"
        );
        assert_eq!(
            rgba.get_pixel(0, 19).0,
            [200, 0, 0, 255],
            "content ends at 19"
        );
        assert_eq!(
            rgba.get_pixel(0, 20).0,
            [10, 20, 30, 255],
            "bottom pad = bg"
        );
        assert_eq!(
            rgba.get_pixel(0, 29).0,
            [10, 20, 30, 255],
            "bottom half slack"
        );
    }

    /// An image that already fills its cells exactly is untouched.
    #[test]
    fn cell_grid_centring_is_a_noop_on_exact_multiples() {
        let img = image::DynamicImage::new_rgba8(4, 60); // 2 rows of 30
        let out = center_on_cell_grid(img, Some((8, 30)), [0, 0, 0, 255]);
        assert_eq!(out.height(), 60);
    }
}
