//! SVG → `DynamicImage` rasterization (usvg → resvg/tiny_skia → PNG → decode), shared by the
//! SVG-file image loader and the Mermaid diagram pipeline.  The two differ in only two policies:
//!
//! * **Scale mode** ([`SvgScaleMode`]).  A user's `.svg` file has a meaningful natural size, so it
//!   is only ever downscaled and then displayed 1:1, which is crisp by construction.  A Mermaid
//!   diagram's dimensions come from layout heuristics, so it scales either way to fill.
//! * **Background** ([`rasterize_svg`]'s `background`).  Mermaid SVGs are transparent but meant for
//!   a light page, so that path fills white; a user's SVG keeps its transparency.
//!
//! The process-wide [`shared_fontdb`] lives here because font-database loading dominates the cost
//! of both paths.

use std::sync::{Arc, OnceLock};

use image::DynamicImage;
use usvg::fontdb;

/// Process-global font database, loaded lazily.  `load_system_fonts` scans every OS font directory
/// (~100–300 ms warm), so per-render loading made a 20-diagram document spawn 20 concurrent scans.
/// Populated by [`warm_fontdb`] at startup, or by [`shared_fontdb`] if a render beats it; never
/// invalidated, since the installed fonts don't change mid-session.
static SHARED_FONTDB: OnceLock<Arc<fontdb::Database>> = OnceLock::new();

/// The shared `fontdb::Database`, loading system fonts and the bundled KaTeX fonts on first call.
/// Later calls are lock-free `Arc` clones from any thread.
fn shared_fontdb() -> Arc<fontdb::Database> {
    SHARED_FONTDB
        .get_or_init(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            pin_generic_families(&mut db);
            register_bundled_katex_fonts(&mut db);
            Arc::new(db)
        })
        .clone()
}

/// Point the CSS generic families at a face that is actually loaded.
///
/// `fontdb::Database::new()` seeds the generics with Windows/macOS names ("Arial", …), and
/// `load_system_fonts` only overrides them when fontdb's partial fontconfig parser resolves the
/// config's `<alias>` blocks — which on some distros (Debian 13) it does not, leaving `sans-serif`
/// pointing at the absent "Arial".  Two paths need a *resolvable* generic: mermaid emits
/// `font-family="…,sans-serif"`, and RaTeX renders `\text{…}` as `sans-serif`; unresolved, usvg
/// drops the glyphs and the text vanishes.  Pinning each generic to the first present candidate
/// closes both gaps (a missing list is a no-op); usvg then falls back per-glyph across the db for
/// characters that face lacks, so no CJK family needs pinning here.
fn pin_generic_families(db: &mut fontdb::Database) {
    fn first_present<'a>(db: &fontdb::Database, candidates: &[&'a str]) -> Option<&'a str> {
        candidates.iter().copied().find(|name| {
            db.query(&fontdb::Query {
                families: &[fontdb::Family::Name(name)],
                ..Default::default()
            })
            .is_some()
        })
    }
    if let Some(f) = first_present(
        db,
        &[
            "DejaVu Sans",
            "Noto Sans",
            "Liberation Sans",
            "Arial",
            "Helvetica",
        ],
    ) {
        db.set_sans_serif_family(f);
    }
    if let Some(f) = first_present(
        db,
        &[
            "DejaVu Serif",
            "Noto Serif",
            "Liberation Serif",
            "Times New Roman",
        ],
    ) {
        db.set_serif_family(f);
    }
    if let Some(f) = first_present(
        db,
        &[
            "DejaVu Sans Mono",
            "Noto Sans Mono",
            "Liberation Mono",
            "Courier New",
        ],
    ) {
        db.set_monospace_family(f);
    }
}

/// The KaTeX font binaries bundled with the crate (SIL OFL 1.1 — licence
/// in `src/assets/katex/OFL.txt`, provenance in `FONT_NOTICE.txt`).
/// These are the faces RaTeX's `<text>` SVG output names (`KaTeX_Main`,
/// `KaTeX_Math`, …), so registering them makes the shared fontdb
/// self-sufficient for math rendering on machines without KaTeX
/// installed.  Paths are manifest-relative so the list is independent of
/// this file's depth in the source tree.
const KATEX_FONT_FILES: &[(&str, &[u8])] = &[
    (
        "KaTeX_AMS-Regular.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_AMS-Regular.ttf"
        )),
    ),
    (
        "KaTeX_Caligraphic-Bold.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Caligraphic-Bold.ttf"
        )),
    ),
    (
        "KaTeX_Caligraphic-Regular.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Caligraphic-Regular.ttf"
        )),
    ),
    (
        "KaTeX_Fraktur-Bold.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Fraktur-Bold.ttf"
        )),
    ),
    (
        "KaTeX_Fraktur-Regular.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Fraktur-Regular.ttf"
        )),
    ),
    (
        "KaTeX_Main-Bold.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Main-Bold.ttf"
        )),
    ),
    (
        "KaTeX_Main-BoldItalic.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Main-BoldItalic.ttf"
        )),
    ),
    (
        "KaTeX_Main-Italic.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Main-Italic.ttf"
        )),
    ),
    (
        "KaTeX_Main-Regular.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Main-Regular.ttf"
        )),
    ),
    (
        "KaTeX_Math-BoldItalic.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Math-BoldItalic.ttf"
        )),
    ),
    (
        "KaTeX_Math-Italic.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Math-Italic.ttf"
        )),
    ),
    (
        "KaTeX_SansSerif-Bold.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_SansSerif-Bold.ttf"
        )),
    ),
    (
        "KaTeX_SansSerif-Italic.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_SansSerif-Italic.ttf"
        )),
    ),
    (
        "KaTeX_SansSerif-Regular.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_SansSerif-Regular.ttf"
        )),
    ),
    (
        "KaTeX_Script-Regular.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Script-Regular.ttf"
        )),
    ),
    (
        "KaTeX_Size1-Regular.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Size1-Regular.ttf"
        )),
    ),
    (
        "KaTeX_Size2-Regular.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Size2-Regular.ttf"
        )),
    ),
    (
        "KaTeX_Size3-Regular.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Size3-Regular.ttf"
        )),
    ),
    (
        "KaTeX_Size4-Regular.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Size4-Regular.ttf"
        )),
    ),
    (
        "KaTeX_Typewriter-Regular.ttf",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/katex/KaTeX_Typewriter-Regular.ttf"
        )),
    ),
];

/// Register every bundled KaTeX face into `db` (zero-copy binary
/// sources).  Called once when the shared fontdb initialises; idempotent
/// by construction.
fn register_bundled_katex_fonts(db: &mut fontdb::Database) {
    use std::sync::Arc;
    // The face family name comes from the font binary itself, so the table
    // key is unused here — it only documents which file each entry is.
    for (_name, bytes) in KATEX_FONT_FILES {
        let _ = db.load_font_source(fontdb::Source::Binary(Arc::new(*bytes)));
    }
}

/// Pre-populate the shared fontdb off the hot path.  Idempotent and thread-safe.
pub fn warm_fontdb() {
    let _ = shared_fontdb();
}

/// Whether an SVG may be scaled up to fill the envelope, or only down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgScaleMode {
    /// Downscale only, for a user `.svg` whose natural size is meaningful.
    Natural,
    /// Scale either way to fill the envelope, for synthetic diagrams.
    Fill,
}

/// How an SVG's natural size maps onto the cell envelope.  A `None` envelope or font size keeps
/// the natural size verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SvgSizing {
    pub envelope: Option<(u16, u16)>,
    pub font_size: Option<(u16, u16)>,
    pub mode: SvgScaleMode,
}

impl SvgSizing {
    /// The envelope in pixels, or `None` when no finite envelope applies.
    fn envelope_px(self) -> Option<(u32, u32)> {
        let (envelope, font_size) = (self.envelope?, self.font_size?);
        let max_w = u32::from(envelope.0).saturating_mul(u32::from(font_size.0));
        let max_h = u32::from(envelope.1).saturating_mul(u32::from(font_size.1));
        (max_w != 0 && max_h != 0).then_some((max_w, max_h))
    }

    /// The scale factor to apply to a `natural_w × natural_h` SVG.
    fn scale_for(self, natural_w: u32, natural_h: u32) -> f32 {
        let Some((max_w_px, max_h_px)) = self.envelope_px() else {
            return 1.0;
        };
        let fit = (max_w_px as f32 / natural_w as f32).min(max_h_px as f32 / natural_h as f32);
        let fit = if fit.is_finite() && fit > 0.0 {
            fit
        } else {
            1.0
        };
        match self.mode {
            SvgScaleMode::Natural => fit.min(1.0),
            SvgScaleMode::Fill => fit,
        }
    }
}

/// Longest side any rasterized SVG may occupy.  Catches the degenerate aspect ratios an area
/// budget alone lets through — a 1 × 10⁹ SVG fits [`MAX_RASTER_PIXELS`] yet is unallocatable.
const MAX_RASTER_DIM: u32 = 8_192;

/// Hard ceiling on pixmap area, applied **whether or not a cell envelope is supplied**.
///
/// The envelope bounds a caller that knows its cell geometry, but the image loader may pass none
/// (`max_cells: None`), and then a document-supplied SVG's declared size is the pixmap size — a
/// few bytes of `width="1e6"` would otherwise be an unbounded allocation.  4 M pixels is 16 MB of
/// RGBA, ample for any image a terminal shows.  Over-budget SVGs are scaled down rather than
/// refused.
const MAX_RASTER_PIXELS: u64 = 4_000_000;

/// The factor fitting a `px_w × px_h` pixmap inside both [`MAX_RASTER_DIM`] and
/// [`MAX_RASTER_PIXELS`], aspect preserved; never above `1.0`, and exactly `1.0` in the common
/// case.
///
/// Takes the *rounded* pixel dimensions rather than natural size × scale, so flooring the result
/// stays within budget — the `ceil` deriving those dimensions would otherwise push a budget-fitting
/// scale back over the line by a pixel per axis.
fn budget_shrink(px_w: u32, px_h: u32) -> f64 {
    let (w, h) = (f64::from(px_w), f64::from(px_h));
    let mut shrink = 1.0f64;
    let longest = w.max(h);
    if longest > f64::from(MAX_RASTER_DIM) {
        shrink = f64::from(MAX_RASTER_DIM) / longest;
    }
    let area = (w * shrink) * (h * shrink);
    if area > MAX_RASTER_PIXELS as f64 {
        shrink *= (MAX_RASTER_PIXELS as f64 / area).sqrt();
    }
    shrink
}

/// Errors from the SVG pipeline.  Owned `String` messages, not source-chained errors, so the type
/// stays `Send + Sync` for the App's mpsc channel.
#[derive(Debug, thiserror::Error)]
pub enum SvgError {
    #[error("svg parse failed: {0}")]
    Parse(String),
    #[error("svg raster failed: {0}")]
    Raster(String),
    #[error("svg png decode failed: {0}")]
    Decode(String),
}

/// usvg options shared by every SVG parse: the shared fontdb, and a string resolver that refuses
/// every path/URL `<image href>`.
///
/// usvg's default string resolver *reads local files*, so an untrusted SVG could pull arbitrary
/// on-disk files into the render — an exfil channel once that SVG reaches an HTML export.  Only the
/// path/URL branch is neutralized; embedded `data:` images still resolve.
fn usvg_options() -> usvg::Options<'static> {
    let mut opt = usvg::Options {
        fontdb: shared_fontdb(),
        ..Default::default()
    };
    opt.image_href_resolver.resolve_string = Box::new(|_href, _opts| None);
    opt
}

/// Re-serialize an SVG through usvg's tree, for embedding in an HTML export as vector art.
///
/// The output can only express what usvg's tree models — shapes, paths, gradients, patterns,
/// masks, filters, and embedded raster images — so `<script>`, `foreignObject`, `on*=` handlers,
/// external references, and CSS are gone by construction, not by filtering.  Text is written as
/// paths (`WriteOptions::preserve_text` stays `false`), which is what makes a RaTeX formula
/// display at all outside edamame: its `<text font-family="KaTeX_*">` names fonts a browser does
/// not have, and the glyph outlines come from the shared fontdb, where the KaTeX faces are
/// registered.
pub fn normalize_svg(svg: &str) -> Result<String, SvgError> {
    let tree =
        usvg::Tree::from_str(svg, &usvg_options()).map_err(|e| SvgError::Parse(format!("{e}")))?;
    Ok(tree.to_string(&usvg::WriteOptions::default()))
}

/// Rasterize an SVG string into a `DynamicImage`.  `background` paints the pixmap before drawing
/// (the Mermaid path passes white); `None` keeps the SVG's own transparency for the renderer to
/// composite over the document background.
pub fn rasterize_svg(
    svg: &str,
    sizing: SvgSizing,
    background: Option<[u8; 4]>,
) -> Result<DynamicImage, SvgError> {
    let tree =
        usvg::Tree::from_str(svg, &usvg_options()).map_err(|e| SvgError::Parse(format!("{e}")))?;
    let size = tree.size();
    let natural_w = (size.width().ceil() as u32).max(1);
    let natural_h = (size.height().ceil() as u32).max(1);

    let mut scale = sizing.scale_for(natural_w, natural_h);
    let mut px_w = ((natural_w as f32 * scale).ceil() as u32).max(1);
    let mut px_h = ((natural_h as f32 * scale).ceil() as u32).max(1);
    // Clamp so f32 ceiling rounding can't overshoot the envelope by a pixel, which keeps the
    // loader's subsequent `pre_resize` a true no-op.
    if let Some((max_w_px, max_h_px)) = sizing.envelope_px() {
        px_w = px_w.min(max_w_px).max(1);
        px_h = px_h.min(max_h_px).max(1);
    }
    // Bound the allocation even without an envelope (see `MAX_RASTER_PIXELS`).  The shrink folds
    // into `scale` as well as the dimensions: `scale` is the render transform, so shrinking the
    // pixmap alone would crop the drawing rather than fit it.
    let shrink = budget_shrink(px_w, px_h);
    if shrink < 1.0 {
        scale = (f64::from(scale) * shrink) as f32;
        px_w = ((f64::from(px_w) * shrink).floor() as u32).max(1);
        px_h = ((f64::from(px_h) * shrink).floor() as u32).max(1);
    }

    let mut pixmap = resvg::tiny_skia::Pixmap::new(px_w, px_h)
        .ok_or_else(|| SvgError::Raster(format!("pixmap alloc failed: {px_w}x{px_h}")))?;
    // Terminal image protocols alpha-composite over the cell, so a transparent SVG meant for a
    // light page needs the fill or the document background bleeds through its text.
    if let Some([r, g, b, a]) = background {
        pixmap.fill(resvg::tiny_skia::Color::from_rgba8(r, g, b, a));
    }
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    let png_bytes = pixmap
        .encode_png()
        .map_err(|e| SvgError::Raster(format!("png encode: {e}")))?;
    // `docs/dev/security-invariants.md`'s "decode through `ImageReader` + `Limits`" rule covers
    // *external* bytes.  These are the PNG encoded one line above, from a pixmap already bounded by
    // the envelope and `MAX_RASTER_PIXELS`.  Don't copy this call to a site fed by a file, a
    // socket, or a document.
    image::load_from_memory(&png_bytes).map_err(|e| SvgError::Decode(format!("{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn natural(envelope: Option<(u16, u16)>) -> SvgSizing {
        SvgSizing {
            envelope,
            font_size: Some((8, 16)),
            mode: SvgScaleMode::Natural,
        }
    }

    fn fill(envelope: Option<(u16, u16)>) -> SvgSizing {
        SvgSizing {
            envelope,
            font_size: Some((8, 16)),
            mode: SvgScaleMode::Fill,
        }
    }

    // ── Bundled KaTeX fonts ──────────────────────────────────────────

    /// The shared fontdb must carry every KaTeX_* family that
    /// ratex-svg's `<text>` output names (Main, Math, AMS, Caligraphic,
    /// Fraktur, SansSerif, Script, Size1..4, Typewriter).  LaTeX
    /// rendering is a text-SVG + shared-fontdb pipeline, so a family
    /// that fails to resolve would render with a silent system-font
    /// fallback — glyphs in the wrong face, or missing.  This test is
    /// the guard rail against RaTeX renaming its internal face table.
    #[test]
    fn bundled_katex_families_resolve_in_the_shared_fontdb() {
        let db = shared_fontdb();
        let families: std::collections::HashSet<String> = db
            .faces()
            .filter_map(|f| f.families.first().map(|(name, _)| name.clone()))
            .collect();
        for family in [
            "KaTeX_Main",
            "KaTeX_Math",
            "KaTeX_AMS",
            "KaTeX_Caligraphic",
            "KaTeX_Fraktur",
            "KaTeX_SansSerif",
            "KaTeX_Script",
            "KaTeX_Size1",
            "KaTeX_Size2",
            "KaTeX_Size3",
            "KaTeX_Size4",
            "KaTeX_Typewriter",
        ] {
            assert!(
                families.contains(family),
                "KaTeX family {family} must be registered in the shared fontdb (faces: {families:?})"
            );
        }
    }

    // ── Natural sizing (SVG files) ────────────────────────────────────

    #[test]
    fn natural_small_svg_is_not_upscaled() {
        // 200×150 natural inside a 640×384 px envelope: downscale-only means it stays put.
        let svg = r##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="200" height="150" viewBox="0 0 200 150">
  <rect width="200" height="150" fill="#eef"/>
</svg>"##;
        let image = rasterize_svg(svg, natural(Some((80, 24))), None).expect("rasterize");
        assert_eq!(image.width(), 200);
        assert_eq!(image.height(), 150);
    }

    #[test]
    fn natural_large_svg_downscales_to_fit_envelope() {
        // 1600×1200 into 640×384: scale 0.32 → 512×384, height-limited.
        let svg = r##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="1200" viewBox="0 0 1600 1200">
  <rect width="1600" height="1200" fill="#eef"/>
</svg>"##;
        let image = rasterize_svg(svg, natural(Some((80, 24))), None).expect("rasterize");
        assert!(image.width() <= 640);
        assert!(image.height() <= 384);
        assert_eq!(image.height(), 384);
    }

    #[test]
    fn natural_preserves_transparency_when_no_background() {
        let svg = r##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10" viewBox="0 0 10 10"></svg>"##;
        let image = rasterize_svg(svg, natural(None), None).expect("rasterize");
        let rgba = image.to_rgba8();
        assert_eq!(rgba.get_pixel(0, 0)[3], 0, "corner must stay transparent");
    }

    // ── Fill sizing (diagrams) ────────────────────────────────────────

    #[test]
    fn fill_small_svg_upscales_to_envelope() {
        // The same small SVG, but Fill scales up by 2.56 → 512×384, height-limited.
        let svg = r##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="200" height="150" viewBox="0 0 200 150">
  <rect width="200" height="150" fill="#eef"/>
</svg>"##;
        let image = rasterize_svg(svg, fill(Some((80, 24))), None).expect("rasterize");
        assert_eq!(image.height(), 384, "should fill the envelope height");
        assert_eq!(image.width(), 512);
    }

    #[test]
    fn fill_with_white_background_is_opaque() {
        let svg = r##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10" viewBox="0 0 10 10"></svg>"##;
        let image = rasterize_svg(svg, fill(None), Some([255, 255, 255, 255])).expect("rasterize");
        let rgba = image.to_rgba8();
        let px = rgba.get_pixel(0, 0);
        assert_eq!(px[3], 255, "white fill must be opaque");
        assert_eq!([px[0], px[1], px[2]], [255, 255, 255]);
    }

    #[test]
    fn no_envelope_keeps_natural_size() {
        let svg = r##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="37" height="23" viewBox="0 0 37 23">
  <rect width="37" height="23" fill="#fff"/>
</svg>"##;
        let image = rasterize_svg(svg, natural(None), None).expect("rasterize");
        assert_eq!(image.width(), 37);
        assert_eq!(image.height(), 23);
    }

    // ── Absolute pixmap budget ────────────────────────────────────────

    /// `budget_shrink` applied as `rasterize_svg` applies it, so the assertions below test the
    /// dimensions actually allocated.
    fn shrunk(px_w: u32, px_h: u32) -> (u64, u64) {
        let shrink = budget_shrink(px_w, px_h);
        if shrink >= 1.0 {
            return (u64::from(px_w), u64::from(px_h));
        }
        (
            ((f64::from(px_w) * shrink).floor() as u64).max(1),
            ((f64::from(px_h) * shrink).floor() as u64).max(1),
        )
    }

    #[test]
    fn budget_shrink_leaves_ordinary_sizes_alone() {
        assert_eq!(budget_shrink(800, 600), 1.0);
        assert_eq!(budget_shrink(512, 384), 1.0);
    }

    #[test]
    fn budget_shrink_caps_area_with_the_aspect_ratio_intact() {
        // 12 M px, three times the budget → shrink by sqrt(1/3).
        let (w, h) = shrunk(4000, 3000);
        assert!(w * h <= MAX_RASTER_PIXELS, "area {} over budget", w * h);
        let aspect = w as f64 / h as f64;
        assert!((aspect - 4.0 / 3.0).abs() < 1e-2, "aspect {aspect} drifted");
    }

    #[test]
    fn budget_shrink_caps_the_longest_side_of_a_sliver() {
        // 400 K px is well inside the area budget; only the dimension cap catches this.
        let (w, h) = shrunk(100_000, 4);
        assert!(w <= u64::from(MAX_RASTER_DIM), "width {w} over the dim cap");
        assert!(h >= 1, "the short axis must not floor away to zero");
        assert!(w * h <= MAX_RASTER_PIXELS);
    }

    #[test]
    fn oversized_svg_without_an_envelope_is_scaled_into_the_budget() {
        // The `export::html` Mermaid path passes no envelope, so this budget is all that stands
        // between a crafted document and an unbounded pixmap allocation.
        let svg = r##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="4000" height="3000" viewBox="0 0 4000 3000">
  <rect width="4000" height="3000" fill="#eef"/>
</svg>"##;
        let image = rasterize_svg(svg, natural(None), None).expect("rasterize");
        let (w, h) = (u64::from(image.width()), u64::from(image.height()));
        assert!(
            w * h <= MAX_RASTER_PIXELS,
            "{w}x{h} = {} px exceeds the {MAX_RASTER_PIXELS} px budget",
            w * h
        );
        assert!(w <= u64::from(MAX_RASTER_DIM) && h <= u64::from(MAX_RASTER_DIM));
        assert!(w > h, "the 4:3 aspect ratio must survive the clamp");
    }

    #[test]
    fn image_href_to_local_file_is_not_loaded() {
        // The default usvg resolver would read this PNG and paint it red; ours drops the element,
        // leaving the pixel transparent.
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("secret.png");
        let buf = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
        image::DynamicImage::ImageRgba8(buf).save(&png).unwrap();

        let svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10" viewBox="0 0 10 10"><image x="0" y="0" width="10" height="10" xlink:href="{}"/></svg>"##,
            png.display()
        );
        let image = rasterize_svg(&svg, natural(None), None).expect("rasterize");
        let rgba = image.to_rgba8();
        assert_eq!(
            rgba.get_pixel(5, 5)[3],
            0,
            "a local-file <image href> must not be loaded into the render"
        );
    }

    #[test]
    fn malformed_svg_returns_parse_error() {
        let err = rasterize_svg("not an svg at all", natural(None), None).unwrap_err();
        assert!(matches!(err, SvgError::Parse(_)));
    }
}
