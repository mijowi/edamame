# Performance benchmark plan — parse/render pipeline

Status: planned (2026-06-10). Prerequisite for any lazy/incremental rendering work.

## Background

A codebase review (June 2026) established that the draw layer is already viewport-limited in all modes (Preview, Rendered, Raw, Diff), but the parse → render → source-map pipeline is fully eager: every line-crossing edit (and every deferred flush of an in-line typing burst) runs `EditorState::refresh_parsed()` (`src/editor/state.rs:652`), which:

1. Parses the whole document twice with pulldown-cmark — once for byte offsets (`src/markdown/parse_offsets.rs`), once for the AST (`src/markdown/parser.rs`).
2. Runs the post-pass walks (list splitting, image/diagram/comment promotion).
3. Renders **all** blocks to styled `Vec<Line<'static>>` via `Renderer::render_with_counts()`.
4. Rebuilds the full `SourceMap`, heading/footnote anchors, and blank-line virtual blocks.

Separately, a width change rebuilds the `VisualRowCache` prefix sum over all lines (`src/document/parsed_doc.rs::ensure_visual_rows`).

Before implementing block-level render memoization (or anything more invasive), we need data on (a) whether `refresh_parsed()` actually exceeds the interactive budget at realistic document sizes, and (b) which stage dominates. The optimization chosen depends directly on (b).

## Budget

The frame throttle is 16 ms (`src/app/frame_timer.rs::MIN_FRAME_INTERVAL`). For editing to feel jank-free, a line-crossing keystroke must complete `refresh_parsed()` + one draw inside one frame interval. Working thresholds:

- `refresh_parsed()` ≤ **8 ms** at the largest "realistic" size → fine, leaves headroom for draw.
- 8–16 ms → marginal; optimize the dominant stage.
- &gt; 16 ms → visible jank on every Enter keypress; optimization warranted.

## Test corpus

Generate synthetic documents with a small script (`benches/gen_corpus.rs` or a shell script writing to `benches/corpus/`, gitignored). Sizes: **1k, 5k, 20k, 100k source lines**. Content mixes, because cost per block varies enormously:

| Corpus | Composition | Stresses |
|---|---|---|
| `prose` | Paragraphs with light inline styling (bold/links), blank-line separated | Baseline; inline rendering, virtual blank-line blocks |
| `lists` | Deep nested lists with checkboxes | Post-pass list splitting, list rendering |
| `tables` | Many medium tables | Table column measurement (`table_layout`) |
| `code` | Fenced code blocks | NBSP padding path, cheap inlines |
| `mixed` | Realistic blend + headings + footnotes | Anchors, source map, everything |

Also include one or two **real documents** (e.g. this repo's `CLAUDE.md` concatenated to ~10k lines) as a sanity check that synthetic results transfer.

## Tier 1 — Criterion micro-benchmarks (primary)

Add `criterion` as a dev-dependency with a `[[bench]]` target (`benches/pipeline.rs`, `harness = false`). Criterion is the standard, stable choice; benches run in release mode by default, which is essential — the dev profile here has debug settings that would distort results.

Benchmark functions, each across the size × mix matrix:

1. **`full_pipeline`** — `ParsedDoc::build_with_overrides(...)` end to end with a no-op image override, default `Theme`, width 100. This is the headline number compared against the budget.
2. **`parse_offsets`** — `parse_offsets::top_level_block_ranges()` alone.
3. **`parse_ast`** — `markdown::parser::parse_raw()` alone (the second full parse).
4. **`render_only`** — `Renderer::render_with_counts()` over a pre-parsed `Vec<Block>` (parse once in setup, clone blocks per iteration; measure and subtract clone cost, or use `iter_batched`).
5. **`source_map_and_anchors`** — derived as `full_pipeline − (offsets + ast + render)`, or benchmarked directly if the build function is split (see instrumentation note below).
6. **`visual_row_cache_build`** — `ensure_visual_rows()` cold, per width, against the rendered lines of each corpus (the resize cost).

Note: stages 2–4 may require making a couple of internal functions `pub` (or `pub` behind a `#[doc(hidden)]`/`bench` feature) so the bench target can reach them. Prefer exposing existing functions over restructuring; the crate already ships as a library.

Record results (mean per size/mix) in a table appended to this document.

## Tier 2 — in-app instrumentation (validation)

Micro-benchmarks miss allocator pressure and cache effects of the live app. Add `tracing` spans (no-ops unless `[dev] logging = true`, per the existing logging policy — never `println!`):

- One span around `refresh_parsed()` as a whole.
- Child spans around the parse, render, and source-map stages inside `ParsedDoc::build_with_overrides` (this likely means threading the timing into `build_with_overrides` or splitting it into named stage functions — a refactor worth doing anyway if memoization follows).
- One span around the draw call in the event loop.

Manual protocol: open each 20k corpus file, enable dev logging, then (a) hold Enter for ~5 s mid-document, (b) type a burst on one line then press an arrow key (deferred-flush path), (c) resize the terminal repeatedly. Inspect span timings in the log. This confirms the criterion numbers hold under real editing and catches anything the micro-benches structurally can't see (e.g. the per-frame snapshot rebuilds keyed on `parsed_version`).

## Decision tree — what to do per outcome

Run Tier 1 first; Tier 2 only to validate whichever branch the numbers point to.

**A. `full_pipeline` ≤ 8 ms even at 20k lines (and 100k is tolerable).**
No optimization work. Document the measured ceiling in `docs/overview.md` ("smooth up to ~N lines"), keep the bench target for regression checks, and stop. Re-evaluate only if a user-visible jank report arrives with a real document.

**B. Render dominates (expected outcome).**
Implement **block-level render memoization**: cache rendered lines per block keyed by (block source bytes hash, width, theme/striping inputs); on reparse, re-render only blocks whose source slice changed and splice cached lines for the rest. The full `Vec<Line>` and `SourceMap` stay intact, so none of the hybrid-editing invariants (virtual blank-line blocks, `per_block_own` vs extended ranges) are disturbed. Re-run the benches after; success = `full_pipeline` on a one-block edit drops to near `parse_offsets + parse_ast` cost.

**C. Parse dominates.**
First, merge the two full parses into one `into_offset_iter()` pass that yields offsets and events together — a contained change with an expected ~2× parse cut. Re-benchmark. If parse still blows the budget after merging, the remaining options are region-limited reparsing (reparse only the edited block ± neighbors, with care around fences/setext/lists/footnotes that have non-local effects) — treat that as a separate, larger project with its own plan.

**D. SourceMap / anchors / post-passes dominate.**
Defer what isn't needed at edit time: heading/footnote anchors behind `OnceCell` (same pattern as `InlineColMap`), and audit the post-passes for accidental quadratic behavior (cheap walks should not dominate; if one does, it's likely a bug, not an architecture problem — fix it directly).

**E. `visual_row_cache_build` is the only outlier (resize jank, edits fine).**
Leave the edit pipeline alone. Options, in order: raise the 80 ms resize-quiesce window; rebuild the cache incrementally from the first changed line instead of from scratch; or compute wrap counts for the visible region eagerly and backfill the rest on idle. Only pursue if Tier 2 shows real resize jank — resize is rare and already quiesced.

**F. Costs scale super-linearly with document size in any stage.**
Whatever the absolute numbers, super-linear scaling indicates an accidental O(n²) (e.g. repeated slicing or per-block scans over the whole source). Profile that stage with `cargo flamegraph` on the 100k corpus and fix the specific hotspot before considering any architectural change — an algorithmic fix may make options B–D unnecessary.

Outcomes can combine (e.g. B + C): handle the dominant stage first, re-benchmark, and only then decide whether the second stage still matters against the budget.

## Deliverables checklist

- [x] `benches/pipeline.rs` criterion target + corpus generator (generators are in the bench file itself; no on-disk corpus needed)
- [x] `criterion` dev-dependency and `[[bench]]` entry in `Cargo.toml`
- [ ] Tracing spans around `refresh_parsed()` stages (Tier 2 — deferred; Tier 1 results were decisive enough to pick a branch)
- [x] Results table appended below, with machine noted
- [x] Decision recorded (which branch of the tree, link to follow-up plan if any)

## Results

Run 2026-06-10 on Intel Core Ultra 7 258V (x86_64 Linux), rustc 1.94.1, release profile, criterion 0.5, sample size 10. Times are criterion means. `cargo bench --bench pipeline` to reproduce.

### `full_pipeline` — `ParsedDoc::build_with_overrides`, width 100

| Corpus | 1k lines | 5k | 20k | 100k |
|---|---|---|---|---|
| prose | 1.36 ms | 8.00 ms | 34.5 ms | 232 ms |
| lists | 1.35 ms | 6.67 ms | 29.8 ms | 171 ms |
| tables | 5.14 ms | 25.5 ms | 101 ms | 527 ms |
| code | 0.46 ms | 2.28 ms | 9.3 ms | 48 ms |
| mixed | 2.20 ms | 10.1 ms | 41.2 ms | 203 ms |

Against the budget (≤ 8 ms fine, > 16 ms jank): everything is fine at 1k; **tables already blow the budget at 5k**; at 20k every mix except `code` is over 16 ms; at 100k the pipeline takes 0.2–0.5 s per line-crossing keystroke. Scaling is approximately linear (prose is mildly super-linear at 100k, ~1.7× over proportional — worth a flamegraph look during follow-up, but not an O(n²) blowup). Outcome F (super-linear) does not apply.

### Stage breakdown at 20k lines

"other" = full − (offsets + ast + render): post-passes, virtual blank-line blocks, SourceMap, anchors. Small negative residuals are measurement noise.

| Corpus | full | parse_offsets | parse_ast | render_only | other | dominant |
|---|---|---|---|---|---|---|
| prose | 34.5 ms | 7.7 ms | 20.1 ms | 7.8 ms | ~0 | **parse 80%** |
| lists | 29.8 ms | 5.8 ms | 13.8 ms | 7.7 ms | ~2.6 ms | **parse 66%** |
| tables | 101 ms | 7.8 ms | 21.2 ms | 71.8 ms | ~0 | **render 71%** |
| code | 9.3 ms | 0.5 ms | 0.8 ms | 7.4 ms | ~0.7 ms | **render 79%** |
| mixed | 41.2 ms | 4.6 ms | 11.2 ms | 23.8 ms | ~1.6 ms | **render 58%** |

`mixed` scaling for the stages is linear (e.g. parse_ast: 0.51 / 2.6 / 11.2 / 55.9 ms across the four sizes; render_only: 1.3 / 5.9 / 23.8 / 118 ms).

### `visual_cache_build` — cold prefix-sum rebuild (resize cost), mixed corpus

1.2 ms / 6.0 ms / 23.0 ms / 116 ms at 1k / 5k / 20k / 100k. Over one frame budget from ~20k lines, but it only fires on width change and is already behind the 80 ms resize quiesce — branch E stands: leave it alone unless live resize jank shows up in Tier 2.

### Decision

**Outcome B + C, in that order.** Render dominates the realistic mixes (tables, code, mixed) and is the single largest line item overall — `render_only/tables/20k` at 71.8 ms dwarfs everything else, almost certainly table column measurement. Parse dominates prose/lists, and notably `parse_ast` (the second full pulldown-cmark pass) costs consistently ~2.6× `parse_offsets`.

Plan of attack:

1. **Block-level render memoization (branch B)** — collapses `render_only` to near zero on a one-block edit. Expected effect at 20k mixed: 41 ms → ~17 ms (the parse floor plus residue).
2. **Merge the two parses (branch C, first step)** — removes the `parse_offsets` pass (~4.6 ms at 20k mixed, 7.7 ms prose). Combined with step 1, expected 20k mixed cost ≈ `parse_ast` alone, ~11 ms — inside the frame budget, marginal but acceptable.
3. **Known remaining ceiling**: after both steps the full-document `parse_ast` floor remains (~56 ms at 100k). Documents beyond roughly 30–40k lines will still jank on line-crossing edits. Region-limited / incremental reparsing is the only fix for that tier; treat it as a separate project and only if such documents matter in practice.

While in the renderer, also check `render_only/tables` for redundant per-row column measurement — a targeted fix there may be cheap and is worth doing alongside step 1.

## Post-optimization results (2026-06-10, same machine)

Both steps implemented:

1. **Merged parse** — `parse_raw_with_ranges` (src/markdown/parser.rs) collects top-level byte ranges via a `RangeTracker` observing the same offset-iterator events the AST builder consumes; `ParsedDoc::build_with_overrides` makes one pulldown-cmark pass instead of two.
2. **Block-level render memoization** — `RenderCache` (src/markdown/render_cache.rs), owned by `EditorState`, keyed by `Block` AST value + render-settings fingerprint; unchanged blocks reuse their rendered lines, evicted by document membership per build. `ImageBlock`s bypass the cache (row counts track the decode cache).

New benchmark group `full_pipeline_memoized` measures the steady-state edit: warm cache, one block changed per build — this is what a line-crossing keystroke costs in the live editor after the first reparse. `parse_merged` measures the new single parse pass.

### Steady-state edit cost (`full_pipeline` before → `full_pipeline_memoized` after)

| Corpus | 5k | 20k | 100k |
|---|---|---|---|
| prose | 8.0 → 6.9 ms (−14%) | 34.5 → 30.3 ms (−12%) | 232 → 181 ms (−22%) |
| lists | 6.7 → 5.4 ms (−19%) | 29.8 → 24.0 ms (−19%) | 171 → 132 ms (−23%) |
| tables | 25.5 → 10.2 ms (**−60%**) | 101 → 44.5 ms (**−56%**) | 527 → 265 ms (−50%) |
| code | 2.3 → 1.1 ms (−51%) | 9.3 → 5.2 ms (−44%) | 48 → 38 ms (−21%) |
| mixed | 10.1 → 5.3 ms (**−48%**) | 41.2 → 23.6 ms (−43%) | 203 → 124 ms (−39%) |

The cache-free `full_pipeline` numbers (cold open, paste-whole-document) also improved ~5–13% across the board from the parse merge alone (e.g. mixed/20k 41.2 → 38.7 ms).

Against the budget: at 5k lines every mix is now inside one frame, including tables (25.5 → 10.2 ms — the pre-optimization budget-buster). At 20k, mixed sits at 23.6 ms and tables at 44.5 ms — better, but line-crossing edits still exceed one frame on documents that size.

### Where the remaining time goes

- **The full-document parse floor.** `parse_merged` costs 13.3 ms at 20k mixed (21.7 ms prose) — single-pass, but still O(document). This is now the dominant cost for prose/lists and the floor for everything; it is why prose barely improved. As recorded in the decision: only region-limited / incremental reparsing removes this, and that is a separate project.
- **Clone-on-hit.** Cache hits clone their `Vec<Line>` into the output (≈10 ms of the 23.6 ms at 20k mixed beyond the parse). A follow-up could share lines via `Arc<[Line]>` instead of cloning, but that changes `ParsedDoc::lines`' type and ripples through every view — only worth it if 20k+ documents matter in practice.
- `parse_offsets` (standalone, now used only by the diff subsystem's table scan) regressed ~6–10% from the `RangeTracker` closure indirection — harmless, it is no longer on the edit path.

### Verification

- `parse_raw_with_ranges` is asserted equivalent to the old two-pass pairing (`merged_parse_matches_two_pass_parse` in src/markdown/parser.rs).
- Cached rendering is asserted output-identical to uncached, plus eviction / settings-invalidation / image-bypass tests (src/markdown/renderer.rs).
- Full suite: 2,500+ tests pass; clippy `--all-targets -D warnings` and `cargo fmt --check` clean.
