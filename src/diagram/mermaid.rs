//! Mermaid → SVG → PNG → `DynamicImage` pipeline.  [`render_mermaid_svg`] serves the HTML
//! exporter; [`resolve_mermaid`] serves the App decode worker.
//!
//! Every call into the third-party renderer is wrapped in `catch_unwind`: `mermaid-rs-renderer`
//! 0.2.x has known panic bugs (invalid hex colors, empty subgraphs, over-wide sequence labels)
//! and a panicking worker thread would strand the cache entry as `Pending` forever.
//!
//! The shared cache-key URL scheme, [`DiagramSource`](super::common::DiagramSource), and
//! [`DiagramError`] live in [`super::common`]; the LaTeX-math backend in [`super::math`].

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::image::{rasterize_svg, LoadedImage, SvgScaleMode, SvgSizing};

use super::common::{panic_message, DiagramError};

/// Pre-populate the shared fontdb (which lives in `crate::image::svg`, shared with the SVG-file
/// rasterizer and the math backend) and mermaid-rs-renderer's own font cache, off the hot path.
/// Called by the App warmup thread at startup.
pub fn warm_fontdb() {
    crate::image::svg::warm_fontdb();
    // Best-effort, so a known upstream panic must not escape.  The guard keeps the process panic
    // hook from restoring the terminal out from under the running TUI; the diagram is a literal,
    // so here the hazard is the hook, not the input.  See `terminal::panic_guard`.
    let _expected = crate::terminal::ExpectedPanic::new();
    let _ = catch_unwind(|| {
        let _ = mermaid_rs_renderer::render("flowchart TD\nA-->B\n");
    });
}

/// The renderer has no internal length, node-count, or timeout bound, so a pathological diagram
/// can drive the decode worker to OOM.  An over-cap block falls back to the plain code block.
const MAX_MERMAID_SOURCE_BYTES: usize = 64 * 1024;

/// Render a mermaid source to SVG, wrapping any panic or error in a [`DiagramError`].
pub fn render_mermaid_svg(source: &str) -> Result<String, DiagramError> {
    if source.len() > MAX_MERMAID_SOURCE_BYTES {
        return Err(DiagramError::RenderFailed(format!(
            "mermaid source too large: {} bytes (max {MAX_MERMAID_SOURCE_BYTES})",
            source.len()
        )));
    }
    // Tells the process panic hook this one is caught, so it neither restores the terminal out
    // from under a running TUI nor prints the payload through it.  Scoped to the `catch_unwind`
    // alone: a guard still live afterwards would silence a panic nobody catches.
    let outcome = {
        let _expected = crate::terminal::ExpectedPanic::new();
        catch_unwind(AssertUnwindSafe(|| mermaid_rs_renderer::render(source)))
    }
    .map_err(|payload| DiagramError::RenderFailed(format!("panic: {}", panic_message(&payload))))?;
    outcome.map_err(|e| DiagramError::RenderFailed(format!("{e:#}")))
}

/// Render a mermaid source all the way to a `LoadedImage` for the image cache.
///
/// * `url` — the synthetic cache key the caller already computed; carried on the result so the
///   main-thread lookup resolves to the right entry.
/// * `max_cells` / `font_size` — target cell envelope, converted to pixels so the pixmap is never
///   larger than the terminal can display.  `None` keeps the SVG's natural resolution.
pub fn resolve_mermaid(
    url: String,
    source: &str,
    max_cells: Option<(u16, u16)>,
    font_size: Option<(u16, u16)>,
) -> Result<LoadedImage, DiagramError> {
    let svg = render_mermaid_svg(source)?;
    // Diagrams have no meaningful natural size, so fill the envelope either way.  White
    // background because mermaid SVGs are transparent but meant to be read on a light page.
    let image = rasterize_svg(
        &svg,
        SvgSizing {
            envelope: max_cells,
            font_size,
            mode: SvgScaleMode::Fill,
        },
        Some([255, 255, 255, 255]),
    )
    .map_err(DiagramError::from)?;
    Ok(LoadedImage {
        url,
        image,
        scratch: None,
        sliced: None,
        direct: None,
    })
}

#[cfg(test)]
mod tests {
    use image::DynamicImage;

    use super::*;

    // Compile-time check: the render result must be `Send` for the decode worker.
    #[test]
    fn resolve_mermaid_result_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Result<LoadedImage, DiagramError>>();
    }

    #[test]
    fn oversized_mermaid_source_is_rejected_before_render() {
        // Must error out *without* reaching the renderer, so this test needs no fonts.
        let huge = format!("flowchart TD\n{}", "A-->B\n".repeat(20_000));
        assert!(huge.len() > 64 * 1024);
        let err = render_mermaid_svg(&huge).unwrap_err();
        assert!(matches!(err, DiagramError::RenderFailed(_)));
    }

    // Non-deterministic across font installs, so this is a "does it render at all" check only.
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

    // Envelope scaling is exercised in `crate::image::svg`, where `rasterize_svg` lives.

    // Counterfactual for `mermaid_live_throughput` below: per-render cost when each call does its
    // own `load_system_fonts()`, the path the shared fontdb replaced.
    #[test]
    #[ignore = "requires system fonts; counterfactual benchmark only"]
    fn mermaid_live_throughput_unshared_fontdb() {
        // A fresh SVG parse per call with its own fontdb, mirroring the pre-fix path.
        fn rasterize_unshared(svg: &str) -> Result<DynamicImage, DiagramError> {
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
                // Render, then rasterize with an *unshared* fontdb.
                if let Ok(svg) = mermaid_rs_renderer::render(src) {
                    let _ = rasterize_unshared(&svg);
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

    // Hot-loop benchmark.  With the shared fontdb the per-iteration cost stays constant; before
    // it, each iteration paid a fresh `load_system_fonts` (~100–300 ms).
    #[test]
    #[ignore = "requires system fonts; exercises live mermaid-rs-renderer"]
    fn mermaid_live_throughput() {
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

    // Canary: malformed mermaid input must yield an `Err`, never unwind out of the closure.
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
            // Either variant is acceptable; not unwinding is the point.
            let _ = result;
        }
    }
}
