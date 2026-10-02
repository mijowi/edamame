//! Parse/render pipeline benchmarks — see docs/dev/performance.md.
//!
//! Measures the eager full-document work done by `refresh_parsed()`:
//!   - `full_pipeline`      — `ParsedDoc::build_with_overrides` end to end, no cache
//!   - `full_pipeline_memoized` — same, with a warm `RenderCache` and one
//!     block changed per build (the steady-state edit cost)
//!   - `parse_merged`       — `parse_raw_with_ranges`, the single parse pass
//!   - `render_only`        — `Renderer::render_with_counts` over a pre-parsed AST
//!   - `visual_cache_build` — cold `VisualRowCache` rebuild (the resize cost)
//!
//! Source-map + anchors + post-pass cost is derived afterwards as
//! `full_pipeline − (parse_merged + render_only)`.
//!
//! The pre-release regression check runs only the end-to-end groups
//! (`full_pipeline`, `full_pipeline_memoized`, `visual_cache_build`); the stage
//! groups are for diagnosing a regression once one shows.  See
//! docs/dev/releasing.md.

use std::time::Duration;

use criterion::measurement::WallTime;
use criterion::{
    criterion_group, criterion_main, BenchmarkGroup, BenchmarkId, Criterion, SamplingMode,
};
use std::hint::black_box;

use edamame::config::Theme;
use edamame::document::ParsedDoc;
use edamame::markdown::highlight;
use edamame::markdown::parser::parse_raw;
use edamame::markdown::{parse_raw_with_ranges, RenderCache, Renderer};

/// The allocator `main.rs` ships with.  A bench is its own binary, so without this it would
/// measure the system allocator — on Linux, up to 4× slower on the memoized path.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// ── Corpus generators ──────────────────────────────────────────────────────
//
// Each generator appends a fixed repeating unit until the document reaches
// the target source-line count.  Deterministic; no images, mermaid diagrams,
// or rendered math (those paths involve async decode workers the pipeline
// benches don't exercise).  The `math` corpus does contain `$$...$$` blocks,
// but they measure only the synchronous parse + promotion work — the RaTeX
// raster is produced later by the decode worker, off this path.

fn fill(target_lines: usize, unit: impl FnMut(usize, &mut String)) -> String {
    fill_from(String::new(), target_lines, unit)
}

/// [`fill`] after a fixed prefix.  Counts only the newlines each unit appends:
/// recounting the whole document per unit made generation quadratic, which cost
/// seconds per 20k corpus and a minute per 100k one.
fn fill_from(
    mut s: String,
    target_lines: usize,
    mut unit: impl FnMut(usize, &mut String),
) -> String {
    let newlines = |t: &str| t.bytes().filter(|&b| b == b'\n').count();
    let mut lines = newlines(&s);
    let mut i = 0;
    while lines < target_lines {
        let start = s.len();
        unit(i, &mut s);
        lines += newlines(&s[start..]);
        i += 1;
    }
    s
}

fn prose(target: usize) -> String {
    fill(target, |_, s| {
        s.push_str(
            "This is a paragraph with **bold text** and a [link](https://example.com/page) in it.\n\
             It continues with some *italic* prose and `inline code` to exercise the inlines.\n\n",
        );
    })
}

fn lists(target: usize) -> String {
    fill(target, |_, s| {
        s.push_str(
            "- [ ] top-level task with **bold** text\n\
             \x20 - [x] nested completed item\n\
             \x20   - deeper plain item with a [link](https://example.com)\n\
             - second top-level item with `code`\n\
             \x20 1. ordered nested one\n\
             \x20 2. ordered nested two\n\n",
        );
    })
}

fn tables(target: usize) -> String {
    fill(target, |_, s| {
        s.push_str("| Name | Type | Description | Default |\n");
        s.push_str("| --- | --- | --- | --- |\n");
        for r in 0..6 {
            s.push_str(&format!(
                "| option{r} | string | the option number {r} with some text | `none` |\n"
            ));
        }
        s.push('\n');
    })
}

fn code(target: usize) -> String {
    fill(target, |i, s| {
        s.push_str("```rust\n");
        s.push_str(&format!("fn example_{i}(x: usize) -> usize {{\n"));
        s.push_str("    let y = x.saturating_add(1);\n");
        s.push_str("    // a comment line inside the fenced block\n");
        s.push_str("    let z = y.checked_mul(2).unwrap_or(usize::MAX);\n");
        s.push('\n');
        s.push_str("    z + y\n");
        s.push_str("}\n```\n\n");
    })
}

fn math(target: usize) -> String {
    fill(target, |i, s| {
        // Prose with inline `$...$`, then two stacked `$$...$$` display blocks.
        // The stacked pair (no blank line between them) is one pulldown paragraph
        // that post_pass splits per formula by scanning for `$$` delimiters —
        // the O(document) promotion path this corpus exists to exercise.
        s.push_str(&format!(
            "Row {i}: the sum for $n = {i}$ and inline $ax^2 + bx + c = 0$ nearby.\n\n"
        ));
        s.push_str(
            "$$\n\\sum_{k=1}^{n} k = \\frac{n(n+1)}{2}\n$$\n\
             $$\nx = \\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}\n$$\n\n",
        );
    })
}

fn nested(target: usize) -> String {
    fill(target, |i, s| {
        // A list item wrapping a fenced `rust` block, and a blockquote wrapping a
        // table — the two shapes where an expensive block hides inside a cheap
        // container.  The render cache must keep caching the *outer* block (#35 §2
        // `is_cache_worthy` walks into it); a gate that skipped lists/blockquotes
        // by kind would re-highlight and re-measure this nested content on every
        // keystroke.  The gap between this corpus's cold and memoized numbers is
        // the guard: it collapses if the nested content stops being cached.
        s.push_str(&format!(
            "- step {i} with **bold** text\n\n  ```rust\n  fn step_{i}(x: usize) -> usize {{ x + {i} }}\n  ```\n\n"
        ));
        s.push_str(&format!(
            "> | Field | Value |\n> | --- | --- |\n> | id | {i} |\n> | ok | yes |\n\n"
        ));
    })
}

fn mixed(target: usize) -> String {
    fill_from(String::from("# Document Title\n\n"), target, |i, s| {
        s.push_str(&format!("## Section {i}\n\n"));
        s.push_str(&prose(2));
        s.push_str(&lists(6));
        s.push_str(&format!(
            "Some text with a footnote reference.[^fn{i}]\n\n[^fn{i}]: The footnote definition text for section {i}.\n\n"
        ));
        s.push_str(&tables(8));
        s.push_str(&code(8));
    })
}

/// Compile the corpus grammars on this thread before measuring.
///
/// Highlighting is eventually-consistent in the live app: a cold grammar
/// renders plain and a background worker compiles it, so a bench that
/// didn't warm first would measure a mixture of both paths depending on
/// how much of the warm-up phase the worker happened to win.  `code` and
/// `mixed` are `rust` fences; the call is idempotent.
fn warm_grammars() {
    highlight::warm_inline(Some("rust"));
}

/// A named corpus generator: (mix name, source generator).
type Corpus = (&'static str, fn(usize) -> String);

/// No 100k size: scaling is linear (performance.md), so it added no signal, and
/// its cases took about half the suite's runtime.
const SIZES: &[usize] = &[1_000, 5_000, 20_000];
const MIXES: &[Corpus] = &[
    ("prose", prose),
    ("lists", lists),
    ("tables", tables),
    ("code", code),
    ("math", math),
    ("nested", nested),
    ("mixed", mixed),
];

/// Stage benches run on every mix at this size, and on `mixed` at every size.
const STAGE_PIVOT_SIZE: usize = 20_000;

// ── Pipeline under test ────────────────────────────────────────────────────

/// Mirrors the arguments `EditorState::refresh_parsed` passes: blank-line
/// preservation on, realistic image ceiling, striping + big-H1 +
/// syntax highlighting on (the costlier paths), width 100, diagrams and
/// display math promoted, paragraph reflow on (the shipped default), no
/// live overrides.
fn build_doc(source: &str, theme: &Theme) -> ParsedDoc {
    ParsedDoc::build_with_overrides(
        source, theme, true, 20, None, None, true, 100, true, true, true, true, None,
    )
}

/// `build_doc` with the block-level render cache threaded through, as
/// `EditorState::refresh_parsed` does in the live editor.
fn build_doc_cached(source: &str, theme: &Theme, cache: &mut RenderCache) -> ParsedDoc {
    ParsedDoc::build_with_overrides(
        source,
        theme,
        true,
        20,
        None,
        None,
        true,
        100,
        true,
        true,
        true,
        true,
        Some(cache),
    )
}

/// Copy of `source` with a single ASCII character near the middle changed —
/// alters exactly one block, simulating the steady state of an edit burst
/// being reparsed.
fn altered_variant(source: &str) -> String {
    let mid = source.len() / 2;
    let idx = source[..mid].rfind('o').expect("corpus contains 'o'");
    let mut s = String::with_capacity(source.len());
    s.push_str(&source[..idx]);
    s.push('0');
    s.push_str(&source[idx + 1..]);
    s
}

// ── Benchmarks ─────────────────────────────────────────────────────────────

/// A benchmark group with flat sampling.  Criterion's default linear sampling
/// runs 1 + 2 + … + `sample_size` iterations (55 at size 10) however slow one
/// iteration is, so a 20k cold build ran for many times its 2 s budget.  Flat
/// sampling runs the same count per sample (10 minimum) and is what criterion
/// recommends for long-running routines; it changes how samples are taken,
/// not what is measured.
fn group<'a>(c: &'a mut Criterion, name: &str) -> BenchmarkGroup<'a, WallTime> {
    let mut g = c.benchmark_group(name);
    g.sampling_mode(SamplingMode::Flat);
    g
}

fn bench_full_pipeline(c: &mut Criterion) {
    warm_grammars();
    let theme = Theme::default();
    let mut g = group(c, "full_pipeline");
    for (mix, gen) in MIXES {
        for &size in SIZES {
            let source = gen(size);
            g.bench_with_input(BenchmarkId::new(*mix, size), &source, |b, src| {
                b.iter(|| build_doc(black_box(src), &theme));
            });
        }
    }
    g.finish();
}

fn bench_full_pipeline_memoized(c: &mut Criterion) {
    warm_grammars();
    let theme = Theme::default();
    let mut g = group(c, "full_pipeline_memoized");
    for &(mix, gen) in MIXES {
        for &size in SIZES {
            let source = gen(size);
            let altered = altered_variant(&source);
            g.bench_function(BenchmarkId::new(mix, size), |b| {
                // Warm the cache, then alternate between the two variants:
                // every build sees exactly one changed block (cache miss)
                // and reuses rendered lines for everything else.
                let mut cache = RenderCache::default();
                let _ = build_doc_cached(&source, &theme, &mut cache);
                let mut flip = false;
                b.iter(|| {
                    flip = !flip;
                    let src = if flip { &altered } else { &source };
                    build_doc_cached(black_box(src), &theme, &mut cache)
                });
            });
        }
    }
    g.finish();
}

fn stage_pairs() -> Vec<(Corpus, usize)> {
    let mut pairs = Vec::new();
    for &(mix, gen) in MIXES {
        for &size in SIZES {
            if size == STAGE_PIVOT_SIZE || mix == "mixed" {
                pairs.push(((mix, gen), size));
            }
        }
    }
    pairs
}

fn bench_parse_merged(c: &mut Criterion) {
    warm_grammars();
    let mut g = group(c, "parse_merged");
    for ((mix, gen), size) in stage_pairs() {
        let source = gen(size);
        g.bench_with_input(BenchmarkId::new(mix, size), &source, |b, src| {
            b.iter(|| parse_raw_with_ranges(black_box(src)));
        });
    }
    g.finish();
}

fn bench_render_only(c: &mut Criterion) {
    warm_grammars();
    let theme = Theme::default();
    let mut g = group(c, "render_only");
    for ((mix, gen), size) in stage_pairs() {
        let source = gen(size);
        let blocks = parse_raw(&source);
        g.bench_with_input(BenchmarkId::new(mix, size), &blocks, |b, blocks| {
            // Mirrors `build_doc`'s settings — highlighting included, or
            // the `code` and `mixed` numbers understate the real render
            // and the derived "other" residual goes negative.
            let renderer = Renderer::new(&theme)
                .with_viewport_width(100)
                .with_image_max_height(20)
                .with_row_striping(true)
                .with_big_h1(true)
                .with_syntax_highlighting(true)
                .with_reflow_paragraphs(true);
            b.iter(|| renderer.render_with_counts(black_box(blocks)));
        });
    }
    g.finish();
}

fn bench_visual_cache(c: &mut Criterion) {
    warm_grammars();
    let theme = Theme::default();
    let mut g = group(c, "visual_cache_build");
    for &size in SIZES {
        let source = mixed(size);
        let doc = build_doc(&source, &theme);
        g.bench_with_input(BenchmarkId::new("mixed", size), &doc, |b, doc| {
            // Cycle three widths so every query misses the 2-entry LRU and
            // forces a cold prefix-sum rebuild — the terminal-resize cost.
            let mut i = 0usize;
            b.iter(|| {
                let w = [60, 90, 120][i % 3];
                i += 1;
                black_box(doc.total_visual_rows(w))
            });
        });
    }
    g.finish();
}

fn config() -> Criterion {
    Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        // Run-to-run noise on a laptop is several percent, so criterion's 1%
        // default flags most cases as changed; at 5% the "regressed" lines
        // are the ones worth a second look.
        .noise_threshold(0.05)
}

criterion_group! {
    name = benches;
    config = config();
    targets = bench_full_pipeline, bench_full_pipeline_memoized, bench_parse_merged,
              bench_render_only, bench_visual_cache
}
criterion_main!(benches);
