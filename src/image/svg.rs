//! SVG → `DynamicImage` rasterization, shared by the SVG-file image
//! loader and the Mermaid diagram pipeline.
//!
//! Both consumers need the same core machinery — parse an SVG string with
//! `usvg`, rasterize it with `resvg`/`tiny_skia`, and decode the result
//! back into a `DynamicImage` for the image cache — but differ in two
//! policies, expressed via [`SvgSizing`]:
//!
//! * **Scale mode** ([`SvgScaleMode`]).  A user's `.svg` *file* has a
//!   meaningful natural size (a badge, an icon), so it is only ever
//!   *downscaled* to fit the cell envelope — `Resize::Fit` then displays
//!   it 1:1, which is crisp by construction.  A Mermaid *diagram* carries
//!   no meaningful natural size (the renderer picks dimensions from
//!   layout heuristics), so it is scaled *up or down* to fill the
//!   envelope.
//! * **Background** ([`rasterize_svg`]'s `background` argument).  Mermaid
//!   SVGs are transparent but meant to be read on a light page, so the
//!   diagram path fills white.  A user's SVG keeps its transparency
//!   (`None`) and composites over the document background like a
//!   transparent PNG.
//!
//! The process-wide [`shared_fontdb`] lives here because both paths parse
//! SVGs and font-database loading (`load_system_fonts`) is the dominant
//! cost — sharing one `Arc<Database>` turns every render after the first
//! into an atomic refcount bump instead of a fresh disk scan.

use std::sync::{Arc, OnceLock};

use image::DynamicImage;
use usvg::fontdb;

/// Process-global font database, loaded lazily on first SVG render.
/// `fontdb::Database::load_system_fonts` scans every OS font directory
/// (typically ~100–300 ms on a warm disk cache, slower cold), so running
/// it per-render made a 20-diagram document spawn 20 concurrent scans —
/// the dominant cost on initial document load.  Shared here as an
/// `Arc<Database>` so every SVG decode is an Arc clone plus a ref into
/// the same underlying tables.
///
/// Populated by [`warm_fontdb`] (called off the hot path at startup) and
/// [`shared_fontdb`] (the fallback on the hot path when the warmup hasn't
/// completed yet).  Never invalidated — the font install set doesn't
/// change during a session.
static SHARED_FONTDB: OnceLock<Arc<fontdb::Database>> = OnceLock::new();

/// Return the process-wide shared `fontdb::Database`, loading system
/// fonts on first call.  Safe to call from any thread; subsequent calls
/// are lock-free Arc clones.
fn shared_fontdb() -> Arc<fontdb::Database> {
    SHARED_FONTDB
        .get_or_init(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

/// Pre-populate the shared fontdb off the hot path so the first real SVG
/// render doesn't pay the disk-scan cost.  Idempotent and thread-safe.
pub fn warm_fontdb() {
    let _ = shared_fontdb();
}

/// Whether an SVG may be scaled up to fill the envelope, or only down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgScaleMode {
    /// Downscale only — never balloon a small SVG to fill the envelope;
    /// its natural size is meaningful.  Used for user `.svg` files.
    Natural,
    /// Scale up or down to fill the envelope.  Used for synthetic
    /// diagrams (Mermaid), which have no meaningful natural size.
    Fill,
}

/// How an SVG's natural size maps onto the target cell envelope.  A
/// `None` envelope or font size keeps the natural size verbatim (used by
/// tests that don't care about on-screen size).
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

/// Errors from the SVG rasterization pipeline.  Carry owned `String`
/// messages rather than source-chained errors so the type stays
/// `Send + Sync` and can be shipped back through the App's mpsc channel.
#[derive(Debug, thiserror::Error)]
pub enum SvgError {
    #[error("svg parse failed: {0}")]
    Parse(String),
    #[error("svg raster failed: {0}")]
    Raster(String),
    #[error("svg png decode failed: {0}")]
    Decode(String),
}

/// Rasterize an SVG string into a `DynamicImage`.
///
/// * `sizing` — how the SVG's natural size maps onto the cell envelope
///   (see [`SvgSizing`]).
/// * `background` — an optional opaque RGBA fill applied before drawing.
///   `Some([r, g, b, a])` paints the pixmap with that color first (the
///   Mermaid path passes white); `None` keeps the SVG's own
///   transparency, which the image renderer composites over the document
///   background like a transparent PNG.
pub fn rasterize_svg(
    svg: &str,
    sizing: SvgSizing,
    background: Option<[u8; 4]>,
) -> Result<DynamicImage, SvgError> {
    let opt = usvg::Options {
        // Use the process-wide shared fontdb (see `SHARED_FONTDB`).  This
        // is an `Arc::clone` — O(1) atomic increment — instead of a fresh
        // disk scan per render.
        fontdb: shared_fontdb(),
        ..Default::default()
    };

    let tree = usvg::Tree::from_str(svg, &opt).map_err(|e| SvgError::Parse(format!("{e}")))?;
    let size = tree.size();
    let natural_w = (size.width().ceil() as u32).max(1);
    let natural_h = (size.height().ceil() as u32).max(1);

    let scale = sizing.scale_for(natural_w, natural_h);
    let mut px_w = ((natural_w as f32 * scale).ceil() as u32).max(1);
    let mut px_h = ((natural_h as f32 * scale).ceil() as u32).max(1);
    // Clamp to the envelope so f32 ceiling rounding can never overshoot it
    // by a pixel — keeps the loader's subsequent `pre_resize` a true
    // no-op (it only ever downscales past the envelope).
    if let Some((max_w_px, max_h_px)) = sizing.envelope_px() {
        px_w = px_w.min(max_w_px).max(1);
        px_h = px_h.min(max_h_px).max(1);
    }

    let mut pixmap = resvg::tiny_skia::Pixmap::new(px_w, px_h)
        .ok_or_else(|| SvgError::Raster(format!("pixmap alloc failed: {px_w}x{px_h}")))?;
    // Paint an opaque background first when requested.  Terminal image
    // protocols alpha-composite over whatever is already on the cell, so
    // a transparent SVG meant for a light page (Mermaid) needs the fill
    // to avoid the document background bleeding through its text.
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

    // ── Natural sizing (SVG files) ────────────────────────────────────

    #[test]
    fn natural_small_svg_is_not_upscaled() {
        // 200×150 natural, envelope 80×24 at (8,16) → 640×384 px.  Natural
        // fits comfortably, so it must stay at its own size (downscale
        // only): a small icon does not balloon to fill the terminal.
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
        // 1600×1200 natural > envelope 640×384.  Fit scale =
        // min(640/1600, 384/1200) = 0.32 → 512×384 (height-axis limited).
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
        // A 10×10 SVG that draws nothing leaves every pixel transparent.
        let svg = r##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10" viewBox="0 0 10 10"></svg>"##;
        let image = rasterize_svg(svg, natural(None), None).expect("rasterize");
        let rgba = image.to_rgba8();
        assert_eq!(rgba.get_pixel(0, 0)[3], 0, "corner must stay transparent");
    }

    // ── Fill sizing (diagrams) ────────────────────────────────────────

    #[test]
    fn fill_small_svg_upscales_to_envelope() {
        // Same 200×150 small SVG, but Fill must scale it up:
        // min(640/200, 384/150) = 2.56 → 512×384 (height limits).
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

    #[test]
    fn malformed_svg_returns_parse_error() {
        let err = rasterize_svg("not an svg at all", natural(None), None).unwrap_err();
        assert!(matches!(err, SvgError::Parse(_)));
    }
}
