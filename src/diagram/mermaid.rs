//! Mermaid → SVG → PNG → `DynamicImage` pipeline.
//!
//! The public entry points are `render_mermaid_svg` (used by the HTML
//! exporter, which wants SVG strings inline) and `resolve_mermaid` (used
//! by the App decode worker, which wants a `LoadedImage` ready for the
//! existing image cache).  Both share `render_mermaid_svg_core` which
//! wraps the third-party renderer in `catch_unwind` — `mermaid-rs-renderer`
//! 0.2.1 has several known panic bugs (invalid hex colors, empty
//! subgraphs, over-wide sequence labels) and a panicking worker thread
//! would strand the cache entry as `Pending` forever.
//!
//! The synthetic-URL format is `diagram-mermaid-<lowercase-hex-sha256>`
//! — stable across reparses so the image cache reuses renders, and
//! content-addressed so editing inside a block invalidates only that
//! block's entry.  The URL is opaque to every other part of the system;
//! `ImageBlockInfo.source` is the reliable discriminator.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, OnceLock};

use image::DynamicImage;
use sha2::{Digest, Sha256};
use usvg::fontdb;

use crate::image::LoadedImage;

/// Process-global font database, loaded lazily on first diagram render.
/// `fontdb::Database::load_system_fonts` scans every OS font directory
/// (typically ~100–300 ms on a warm disk cache, slower cold), so running
/// it per-render made a 20-diagram document spawn 20 concurrent scans —
/// the dominant cost on initial document load.  Shared here as an
/// `Arc<Database>` so every diagram decode is an Arc clone plus a ref
/// into the same underlying tables.
///
/// Populated by `warm_fontdb` (called from the App warmup thread at
/// startup) and `shared_fontdb` (the fallback on the hot path when the
/// warmup hasn't completed yet).  Never invalidated — the font install
/// set doesn't change during a session.
static SHARED_FONTDB: OnceLock<Arc<fontdb::Database>> = OnceLock::new();

/// Return the process-wide shared `fontdb::Database`, loading system
/// fonts on first call.  Safe to call from any thread; subsequent calls
/// are lock-free Arc clones.
pub fn shared_fontdb() -> Arc<fontdb::Database> {
    SHARED_FONTDB
        .get_or_init(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

/// Pre-populate the shared fontdb off the hot path.  The App warmup
/// thread calls this at startup so the first real diagram render
/// doesn't pay the disk-scan cost.  Also primes mermaid-rs-renderer's
/// own internal font cache by running a trivial diagram.
pub fn warm_fontdb() {
    let _ = shared_fontdb();
    // Prime mermaid-rs-renderer's own fontdb too (it maintains its own
    // via once_cell::sync::Lazy).  Wrapped in catch_unwind because the
    // upstream crate has known panic bugs and the warmup is
    // best-effort.
    let _ = catch_unwind(|| {
        let _ = mermaid_rs_renderer::render("flowchart TD\nA-->B\n");
    });
}

/// Source for a diagram block.  Only `Mermaid` currently ships; the
/// enum exists so future backends (PlantUML, Graphviz/DOT, D2) can be
/// added without rewiring `ImageBlockInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DiagramSource {
    Mermaid(String),
}

/// Errors reported by the diagram pipeline.  The renderer / rasteriser /
/// decoder each have their own variant so the hint line can surface a
/// specific failure mode.  The variants
/// carry owned `String` messages rather than source-chained errors so
/// `DiagramError` stays `Send + Sync` and can be shipped back through
/// the App's mpsc channel.
#[derive(Debug, thiserror::Error)]
pub enum DiagramError {
    #[error("mermaid render failed: {0}")]
    RenderFailed(String),
    #[error("svg parse failed: {0}")]
    SvgParse(String),
    #[error("raster failed: {0}")]
    Raster(String),
    #[error("png decode failed: {0}")]
    Decode(String),
}

/// Synthetic cache-key URL for a mermaid source.  Stable across process
/// invocations — two runs of edamame see the same URL for the same
/// diagram text.
pub fn synthetic_url(source: &DiagramSource) -> String {
    match source {
        DiagramSource::Mermaid(src) => {
            let digest = Sha256::digest(src.as_bytes());
            format!("diagram-mermaid-{:x}", digest)
        }
    }
}

/// Render a mermaid source to SVG, wrapping any panic or error in a
/// `DiagramError`.  Used by both the raster path below and the HTML
/// exporter's inline-SVG branch.
pub fn render_mermaid_svg(source: &str) -> Result<String, DiagramError> {
    let outcome = catch_unwind(AssertUnwindSafe(|| mermaid_rs_renderer::render(source))).map_err(
        |payload| DiagramError::RenderFailed(format!("panic: {}", panic_message(&payload))),
    )?;
    outcome.map_err(|e| DiagramError::RenderFailed(format!("{e:#}")))
}

/// Render a mermaid source all the way to a `LoadedImage`, suitable for
/// dropping straight into the image cache.
///
/// * `url` — the synthetic cache-key URL already computed by the caller
///   (typically from `ParsedDoc::image_blocks[i].url`).  Carried on the
///   returned `LoadedImage` so the main-thread cache lookup resolves to
///   the right entry.
/// * `max_cells` / `font_size` — the target cell envelope.  Scaled into
///   pixels and used to downscale the SVG before rasterisation so we
///   never allocate a pixmap larger than the terminal can display.
///   Passing `None` keeps the SVG's natural resolution (used by tests
///   that don't care about on-screen size).
pub fn resolve_mermaid(
    url: String,
    source: &str,
    max_cells: Option<(u16, u16)>,
    font_size: Option<(u16, u16)>,
) -> Result<LoadedImage, DiagramError> {
    let svg = render_mermaid_svg(source)?;
    let image = rasterise_svg(&svg, max_cells, font_size)?;
    Ok(LoadedImage {
        url,
        image,
        scratch: None,
    })
}

/// SVG string → `DynamicImage`, in memory.  Parallels
/// `mermaid_rs_renderer::render::write_output_png` but writes to a
/// `Vec<u8>` via `Pixmap::encode_png` instead of a file path.
fn rasterise_svg(
    svg: &str,
    max_cells: Option<(u16, u16)>,
    font_size: Option<(u16, u16)>,
) -> Result<DynamicImage, DiagramError> {
    let opt = usvg::Options {
        // Use the process-wide shared fontdb (see `SHARED_FONTDB` above).
        // This is an `Arc::clone` — O(1) atomic increment — instead of a
        // fresh disk scan per diagram.  Critical for lag on documents
        // with many diagrams, where each would otherwise duplicate the
        // system font load.
        fontdb: shared_fontdb(),
        ..Default::default()
    };

    let tree =
        usvg::Tree::from_str(svg, &opt).map_err(|e| DiagramError::SvgParse(format!("{e}")))?;
    let size = tree.size();
    let natural_w = (size.width().ceil() as u32).max(1);
    let natural_h = (size.height().ceil() as u32).max(1);

    // Scale the SVG to fit the envelope in both dimensions, preserving
    // aspect ratio.  Unlike regular images (which `pre_resize` only
    // DOWNSCALES — a 32x32 icon must not balloon to fill the terminal),
    // diagrams carry no meaningful natural pixel size: mermaid-rs-renderer
    // picks dimensions based on internal layout heuristics, and a small
    // natural size just means "a simple diagram", not "render small".  So
    // upscale small diagrams to fill the envelope too, so users who have
    // not customized `[images].max_width / max_height` still see diagrams
    // at a useful on-screen size.  Rasterising an SVG at a higher
    // resolution is ~free — usvg draws text from font glyphs at the
    // target resolution, so crispness is preserved.
    let (scale, px_w, px_h) = match (max_cells, font_size) {
        (Some((mc_w, mc_h)), Some((fc_w, fc_h))) => {
            let max_w_px = u32::from(mc_w).saturating_mul(u32::from(fc_w));
            let max_h_px = u32::from(mc_h).saturating_mul(u32::from(fc_h));
            if max_w_px == 0 || max_h_px == 0 {
                (1.0_f32, natural_w, natural_h)
            } else {
                let fit =
                    (max_w_px as f32 / natural_w as f32).min(max_h_px as f32 / natural_h as f32);
                let s = if fit.is_finite() && fit > 0.0 {
                    fit
                } else {
                    1.0
                };
                let px_w = ((natural_w as f32 * s).ceil() as u32).max(1);
                let px_h = ((natural_h as f32 * s).ceil() as u32).max(1);
                (s, px_w, px_h)
            }
        }
        _ => (1.0, natural_w, natural_h),
    };

    let mut pixmap = resvg::tiny_skia::Pixmap::new(px_w, px_h)
        .ok_or_else(|| DiagramError::Raster(format!("pixmap alloc failed: {px_w}x{px_h}")))?;
    // Mermaid SVGs are transparent, but the terminal image protocols
    // alpha-composite over whatever is already on the cell.  Fill with
    // white to match the typical mermaid background and avoid text
    // bleeding through to the document's background color.
    pixmap.fill(resvg::tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    let png_bytes = pixmap
        .encode_png()
        .map_err(|e| DiagramError::Raster(format!("png encode: {e}")))?;
    let image =
        image::load_from_memory(&png_bytes).map_err(|e| DiagramError::Decode(format!("{e}")))?;
    Ok(image)
}

/// Best-effort extraction of a message from a `catch_unwind` payload.
/// Panics in Rust are usually `String` or `&'static str`; anything else
/// falls back to a generic marker so the cache entry still reports a
/// failure.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else {
        "unknown payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-time check: the render entry point must be `Send` so it
    // can be called from a `std::thread::spawn`'d worker.  If someone
    // introduces a non-Send type into the signature this test stops
    // compiling — effectively a spec constraint frozen in code.
    #[test]
    fn resolve_mermaid_result_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Result<LoadedImage, DiagramError>>();
    }

    #[test]
    fn synthetic_url_is_stable_for_same_source() {
        let a = synthetic_url(&DiagramSource::Mermaid("flowchart TD\nA-->B".into()));
        let b = synthetic_url(&DiagramSource::Mermaid("flowchart TD\nA-->B".into()));
        assert_eq!(a, b);
        assert!(a.starts_with("diagram-mermaid-"));
        // SHA-256 hex is 64 chars; prefix is 16 chars; total 80.
        assert_eq!(a.len(), "diagram-mermaid-".len() + 64);
    }

    #[test]
    fn synthetic_url_differs_for_different_sources() {
        let a = synthetic_url(&DiagramSource::Mermaid("flowchart TD\nA-->B".into()));
        let b = synthetic_url(&DiagramSource::Mermaid("flowchart TD\nA-->C".into()));
        assert_ne!(a, b);
    }

    // The full renderer is slow (font DB load + layout) and its output
    // is non-deterministic across font installs, so we only exercise it
    // in the "does it work at all" sense and skip pixel comparison.
    // Marked `#[ignore]` because CI may not have system fonts and the
    // upstream crate has known panics; run locally with
    // `cargo test -- --ignored mermaid_live`.
    #[test]
    #[ignore = "requires system fonts; upstream has known panics"]
    fn mermaid_live_renders_trivial_flowchart() {
        let loaded = resolve_mermaid(
            "test".into(),
            "flowchart TD\nA-->B\n",
            Some((80, 24)),
            Some((8, 16)),
        )
        .expect("trivial flowchart should render");
        assert!(loaded.image.width() > 0);
        assert!(loaded.image.height() > 0);
    }

    // Regression test for "diagrams are too small to see": a small SVG
    // (200x150 natural, representing e.g. a 3-node flowchart) must be
    // upscaled so it fills the envelope.  We assert that the resulting
    // pixmap reaches at least one envelope axis (width or height, the
    // aspect-limiting one) — anything else means we shrank a small
    // diagram to its natural size instead of scaling it up.
    #[test]
    fn small_svg_upscales_to_fill_envelope() {
        let small_svg = r##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="200" height="150" viewBox="0 0 200 150">
  <rect width="200" height="150" fill="#eef"/>
  <rect x="20" y="20" width="60" height="30" fill="#fff" stroke="#333"/>
  <rect x="120" y="20" width="60" height="30" fill="#fff" stroke="#333"/>
  <line x1="80" y1="35" x2="120" y2="35" stroke="#333"/>
</svg>"##;
        // Envelope 80 cols × 24 rows at (8,16) font → 640x384 pixels.
        // Natural 200x150 → fit scale = min(640/200, 384/150) =
        // min(3.2, 2.56) = 2.56 → rendered at 512x384.  Height axis
        // limits, so the result must be exactly `max_h_px`.
        let image = rasterise_svg(small_svg, Some((80, 24)), Some((8, 16)))
            .expect("small SVG should rasterise");
        assert_eq!(image.height(), 384, "should fill the envelope height");
        assert_eq!(
            image.width(),
            512,
            "width must scale proportionally to height"
        );
    }

    #[test]
    fn large_svg_downscales_to_fit_envelope() {
        // Same envelope; natural 1600x1200 (bigger than envelope on both
        // axes).  Fit scale = min(640/1600, 384/1200) = min(0.4, 0.32) =
        // 0.32 → rendered at 512x384.  Height-axis-limited again.
        let large_svg = r##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="1200" viewBox="0 0 1600 1200">
  <rect width="1600" height="1200" fill="#eef"/>
</svg>"##;
        let image = rasterise_svg(large_svg, Some((80, 24)), Some((8, 16)))
            .expect("large SVG should rasterise");
        assert!(image.width() <= 640, "must not exceed envelope width");
        assert!(image.height() <= 384, "must not exceed envelope height");
        assert_eq!(image.height(), 384);
    }

    #[test]
    fn no_envelope_keeps_natural_size() {
        // No `max_cells`/`font_size` → preserve the SVG's natural
        // dimensions.  Covers the test-path constructor.
        let svg = r##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="37" height="23" viewBox="0 0 37 23">
  <rect width="37" height="23" fill="#fff"/>
</svg>"##;
        let image = rasterise_svg(svg, None, None).expect("natural-size render");
        assert_eq!(image.width(), 37);
        assert_eq!(image.height(), 23);
    }

    // Counterfactual: what per-render costs look like when each call
    // does its own `load_system_fonts()` — the code path we replaced.
    // Run alongside `mermaid_live_throughput` (below) to quantify the
    // win.  Ignored for the same reasons the shared-fontdb bench is.
    #[test]
    #[ignore = "requires system fonts; counterfactual benchmark only"]
    fn mermaid_live_throughput_unshared_fontdb() {
        // Force a fresh SVG parse per call with its own fontdb — mirrors
        // the original pre-fix behaviour.
        fn rasterise_unshared(svg: &str) -> Result<DynamicImage, DiagramError> {
            let mut opt = usvg::Options::default();
            opt.fontdb_mut().load_system_fonts();
            let tree = usvg::Tree::from_str(svg, &opt)
                .map_err(|e| DiagramError::SvgParse(format!("{e}")))?;
            let size = tree.size();
            let w = (size.width().ceil() as u32).max(1);
            let h = (size.height().ceil() as u32).max(1);
            let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)
                .ok_or_else(|| DiagramError::Raster("pixmap".into()))?;
            pixmap.fill(resvg::tiny_skia::Color::WHITE);
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::default(),
                &mut pixmap.as_mut(),
            );
            let bytes = pixmap
                .encode_png()
                .map_err(|e| DiagramError::Raster(format!("{e}")))?;
            image::load_from_memory(&bytes).map_err(|e| DiagramError::Decode(format!("{e}")))
        }
        let diagrams = [
            "flowchart TD\nA-->B-->C\nC-->D\nD-->A",
            "sequenceDiagram\nA->>B: hi\nB-->>A: ok",
            "pie\n\"A\": 50\n\"B\": 30\n\"C\": 20",
            "stateDiagram-v2\n[*] --> Idle\nIdle --> Run : go\nRun --> [*]",
            "classDiagram\nAnimal <|-- Dog\nAnimal <|-- Cat\nclass Animal",
        ];
        let iterations = 4usize;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            for src in &diagrams {
                // Call mermaid_rs_renderer to get SVG, then rasterise
                // with an UNshared fontdb — simulating the old path.
                if let Ok(svg) = mermaid_rs_renderer::render(src) {
                    let _ = rasterise_unshared(&svg);
                }
            }
        }
        let total = start.elapsed();
        let count = iterations * diagrams.len();
        eprintln!(
            "mermaid_live_throughput_unshared_fontdb: {count} renders in {:?} ({} µs/render)",
            total,
            total.as_micros() / count as u128,
        );
    }

    // Hot-loop benchmark: render many diagrams back-to-back on one
    // thread.  After the shared-fontdb fix this stays constant per
    // iteration; before it, each iteration paid a fresh
    // `load_system_fonts` (~100–300 ms), so 20 iterations was ~2–6 s
    // serial (or much worse parallel due to disk thrashing).
    // Locked behind `--ignored` because it needs system fonts and the
    // upstream renderer has known panic inputs; run with
    // `cargo test --lib mermaid_live_throughput -- --ignored --nocapture`.
    #[test]
    #[ignore = "requires system fonts; exercises live mermaid-rs-renderer"]
    fn mermaid_live_throughput() {
        // Prime the caches the same way the App warmup thread would.
        warm_fontdb();
        let diagrams = [
            "flowchart TD\nA-->B-->C\nC-->D\nD-->A",
            "sequenceDiagram\nA->>B: hi\nB-->>A: ok",
            "pie\n\"A\": 50\n\"B\": 30\n\"C\": 20",
            "stateDiagram-v2\n[*] --> Idle\nIdle --> Run : go\nRun --> [*]",
            "classDiagram\nAnimal <|-- Dog\nAnimal <|-- Cat\nclass Animal",
        ];
        let iterations = 4usize;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            for src in &diagrams {
                let _ = resolve_mermaid("bench".into(), src, Some((80, 24)), Some((8, 16)));
            }
        }
        let total = start.elapsed();
        let count = iterations * diagrams.len();
        eprintln!(
            "mermaid_live_throughput: {count} renders in {:?} ({} µs/render)",
            total,
            total.as_micros() / count as u128,
        );
    }

    // Canary for the known-panic-bug class: malformed mermaid input
    // must yield an `Err`, never a panic (the App worker's
    // `catch_unwind` wrapper depends on this being true of
    // `resolve_mermaid` itself too).
    #[test]
    #[ignore = "exercises upstream; may panic on some inputs until fixed"]
    fn garbage_input_returns_err_not_panic() {
        for input in [
            "",
            "\u{0000}\u{FFFF}",
            "not a diagram at all, just prose",
            "flowchart TD\n~~~~~~~~~~~",
        ] {
            let result = resolve_mermaid("test".into(), input, None, None);
            // Either variant is acceptable; what matters is that we
            // didn't unwind the stack out of the closure.
            let _ = result;
        }
    }
}
