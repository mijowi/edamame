# Performance — the parse/render pipeline

Contributor-facing reference for the one hot path in edamame: the eager, full-document work an edit triggers. It records the budget that path is held to, what each stage costs, why the two optimizations in place exist, and which ceilings are known and deliberately unfixed.

The history — the branch A–E decision tree these numbers were originally gathered to choose between, and the pre-optimization measurements — is in [`plans/archive/perf-benchmark-plan.md`](plans/archive/perf-benchmark-plan.md). This page carries only what is still true.

## What runs on an edit

Every line-crossing edit — and every deferred flush of an in-line typing burst — calls `EditorState::refresh_parsed()`, which rebuilds the whole document:

1. **Parse.** One pulldown-cmark pass (`markdown::parser::parse_raw_with_ranges`) yielding the AST *and* top-level byte ranges together.
2. **Post-passes.** List blank annotation, image/diagram/comment promotion.
3. **Render.** Every top-level block to styled `Vec<Line<'static>>` (`Renderer::render_with_counts_cached`), memoized per block.
4. **Derive.** `SourceMap`, heading/footnote anchors, blank-line virtual blocks.

Separately, a width change rebuilds the `VisualRowCache` prefix sum over all lines (`ParsedDoc::ensure_visual_rows`).

The draw layer is *not* on this list: it is viewport-limited in every mode (Preview, Rendered, Raw, Diff), so document size does not enter it.

## The budget

The frame throttle is 16 ms (`app::frame_timer::MIN_FRAME_INTERVAL`, ~60 fps). For editing to feel jank-free, a line-crossing keystroke must finish `refresh_parsed()` *and* one draw inside one interval:

| `refresh_parsed()` | Verdict |
|---|---|
| ≤ 8 ms | fine — leaves headroom for the draw |
| 8–16 ms | marginal; optimize the dominant stage |
| > 16 ms | visible jank on every Enter keypress |

## The corpus

`benches/pipeline.rs` generates its documents in-process — deterministic, no on-disk corpus — at **1k / 5k / 20k / 100k source lines** in five mixes. The mixes exist because cost per block varies enormously; keep them stable, since they are what makes a future measurement comparable to the ones below.

| Corpus | Composition | Stresses |
|---|---|---|
| `prose` | Paragraphs with bold/links | Inline rendering, virtual blank-line blocks |
| `lists` | Deep nested lists with checkboxes | List post-pass and rendering |
| `tables` | Many medium tables | Table column measurement (`table_layout`) |
| `code` | Fenced `rust` blocks | Syntax highlighting, NBSP padding, cheap inlines |
| `mixed` | Blend + headings + footnotes | Anchors, source map, everything |

Two details of the harness matter for reproducibility:

- **Grammars are warmed on the bench thread first** (`warm_grammars`, calling `highlight::warm_inline`). Highlighting is eventually-consistent in the live app — a cold grammar renders plain while a background worker compiles it — so without the warm call the `code` and `mixed` numbers would be a coin-toss mixture of the highlighted and plain paths.
- **`full_pipeline_memoized` alternates between two source variants differing in one character**, so every build is a warm cache with exactly one changed block. That is the steady-state edit cost; `full_pipeline` is the cold-open / paste-whole-document cost.

`cargo bench --bench pipeline` to reproduce.

## Results

Run 2026-08-22 on an Apple M3 (8 cores, macOS 15.7.5), rustc 1.96.1, release profile, criterion 0.5, sample size 10. Times are criterion means, all from one run. These supersede the 2026-06-10 figures in the archived plan, which predate syntax highlighting and were taken on different hardware — compare shapes, not ratios, across the two.

### Steady-state edit — `full_pipeline_memoized`

What a line-crossing keystroke costs in the live editor: warm `RenderCache`, one block changed.

| Corpus | 1k lines | 5k | 20k | 100k |
|---|---|---|---|---|
| `prose` | 0.82 ms | 3.85 ms | 18.4 ms | 98.0 ms |
| `lists` | 0.97 ms | 5.05 ms | 23.0 ms | 122.7 ms |
| `tables` | 1.06 ms | 5.76 ms | 27.6 ms | 145.5 ms |
| `code` | 0.34 ms | 1.17 ms | 4.77 ms | 32.1 ms |
| `mixed` | 0.67 ms | 3.15 ms | 13.7 ms | 81.1 ms |

Against the budget: **every mix is inside one frame at 5k lines**, and `mixed` still is at 20k (13.7 ms — marginal, past the 8 ms working target). At 20k `prose`, `lists` and `tables` exceed a frame; at 100k every mix does. Scaling is linear throughout — no stage is accidentally quadratic.

### Cold open — `full_pipeline`

Opening a document, or pasting one wholesale: no cache, everything rendered.

| Corpus | 1k lines | 5k | 20k | 100k |
|---|---|---|---|---|
| `prose` | 0.84 ms | 4.31 ms | 20.5 ms | 105.5 ms |
| `lists` | 0.76 ms | 3.81 ms | 16.0 ms | 88.5 ms |
| `tables` | 3.46 ms | 18.4 ms | 76.0 ms | 389.2 ms |
| `code` | 4.03 ms | 20.4 ms | 82.6 ms | 415.5 ms |
| `mixed` | 1.41 ms | 6.83 ms | 29.8 ms | 154.3 ms |

No keystroke waits on a cold open, so the frame budget does not apply to this table the way it does to the one above — but this is what a whole-document paste costs, and it is the number to watch when adding work to the renderer.

### Stage breakdown at 20k lines

`other` = `full − (parse_merged + render_only)`: post-passes, virtual blank-line blocks, `SourceMap`, anchors. `parse_offsets` and `parse_ast` are the pre-merge baselines, kept for comparison only — neither runs in the pipeline any more. Small residuals (`code`'s is slightly negative) are measurement noise.

| Corpus | full | `parse_merged` | `render_only` | other | dominant | (`parse_offsets` / `parse_ast`) |
|---|---|---|---|---|---|---|
| `prose` | 20.5 ms | 13.0 ms | 5.47 ms | 2.03 ms | **parse 63%** | 4.56 / 12.7 ms |
| `lists` | 16.0 ms | 9.37 ms | 4.71 ms | 1.96 ms | **parse 58%** | 2.96 / 9.03 ms |
| `tables` | 76.0 ms | 14.9 ms | 59.3 ms | 1.72 ms | **render 78%** | 4.42 / 13.8 ms |
| `code` | 82.6 ms | 0.49 ms | 82.9 ms | ~0 | **render ~100%** | 0.32 / 0.44 ms |
| `mixed` | 29.8 ms | 7.66 ms | 20.1 ms | 2.03 ms | **render 67%** | 2.86 / 7.24 ms |

`mixed` stage scaling across 1k / 5k / 20k / 100k is linear: `parse_merged` 0.35 / 1.82 / 7.66 / 43.2 ms, `render_only` 0.94 / 4.77 / 20.1 / 103.5 ms.

Two things changed shape since the pre-highlighting measurements:

- **`code` is now the most expensive corpus to render cold**, where it used to be the cheapest mix in the table. Its parse is nearly free (0.49 ms — a fenced block is one AST node), so syntax highlighting is essentially the entire pipeline there. It is also the corpus the render cache helps most (82.6 → 4.77 ms at 20k), because a code block's AST is unchanged by edits to other blocks and the highlighter is never re-entered for it.
- **`tables` is no longer the single worst case**, though table column measurement is still the second-largest line item and the worst *steady-state* one.

### Where memoization helps, and where it doesn't

Change in the 20k figure, `full_pipeline` → `full_pipeline_memoized`:

| Corpus | Change |
|---|---|
| `code` | −94% |
| `tables` | −64% |
| `mixed` | −54% |
| `prose` | −10% |
| `lists` | **+44%** |

`lists` is a reproducible regression, not noise (confirmed on a repeat run). The corpus's blocks are large nested-list ASTs that are cheap to render — 4.71 ms for the whole document — but expensive to *look up*, since the cache hashes the entire `Block` value on every query and then clones the cached lines back out. Where re-rendering costs less than hashing plus cloning, the cache is a net loss. It stays because the mixes that resemble real documents — `mixed`, `tables`, `code` — gain far more than `lists` loses, and no real document is wall-to-wall deep nested lists. If it is ever revisited, the fix is the one the clone-on-hit ceiling below names.

### Resize — `visual_cache_build`

Cold prefix-sum rebuild on the `mixed` corpus: 1.42 / 7.01 / 23.9 / 138.3 ms at 1k / 5k / 20k / 100k. Over one frame from roughly 20k lines, but it fires only on a width change and is already behind the 80 ms `RESIZE_QUIESCE` window — leave it alone unless live resize jank shows up.

## The two optimizations, and why they must not be undone

- **One parse, not two.** The pipeline used to parse the document twice — once for byte offsets, once for the AST. `parse_raw_with_ranges` collects the ranges from a `parse_offsets::RangeTracker` observing the same offset-iterator events the AST builder consumes, so blocks and ranges stay 1:1 *by construction* rather than by a second pass agreeing with the first. Re-splitting them costs a full extra parse per reparse.
- **Block-level render memoization.** `RenderCache` (owned by `EditorState`, threaded into every `refresh_parsed`) keys rendered lines by the `Block` AST value plus a render-settings fingerprint, so an unchanged block costs a clone of its lines instead of a re-render. This is what makes table-heavy documents editable at all — table column measurement dominated everything else. Keying by AST rather than source bytes is what keeps live table-width drags and post-pass promotions correct.

Both claims are asserted, not just documented:

- `merged_parse_matches_two_pass_parse` (`src/markdown/parser.rs`) pins the merged parse to the old two-pass pairing.
- `cached_render_matches_uncached`, plus the eviction, settings-invalidation, syntax-toggle and image-bypass tests (`src/markdown/renderer.rs`), pin cached rendering to uncached output.

## Known ceilings

These are recorded as facts about the current design, not as tasks.

- **The full-document parse floor.** The single parse is still O(document) and cannot be memoized the way rendering is — 7.7 ms at 20k `mixed`, 43.2 ms at 100k. It is the dominant cost for prose and lists and the floor under everything else. Only region-limited / incremental reparsing removes it — reparsing the edited block and its neighbors, with care around fences, setext headings, lists and footnote definitions, all of which have non-local effects. That is a separate project, explicitly out of scope here.
- **Clone-on-hit, and hash-on-lookup.** A cache hit hashes the whole `Block` to find its entry and then clones its `Vec<Line>` into the output — together about 4.0 ms of the 13.7 ms steady-state cost at 20k `mixed`, and more than the entire render it replaces on `lists`. Sharing lines as `Arc<[Line]>` (and/or keying on a cheaper block identity) would change `ParsedDoc::lines`' type and ripple through every view; worth it only if very large or list-heavy documents matter in practice.
- **Resize.** `visual_cache_build` is the cold prefix-sum rebuild a width change forces (23.9 ms at 20k `mixed`). It exceeds a frame on large documents, but fires only on resize and sits behind the 80 ms `RESIZE_QUIESCE` window (`app::frame_timer`), so it is left alone.
- **`parse_offsets::top_level_block_ranges` is off the edit path entirely.** It survives as the pre-merge baseline the bench measures and as the oracle in `merged_parse_matches_two_pass_parse`; the diff subsystem uses the sibling `block_ranges_by`, not this. Its cost is not an editing cost.

## When to re-measure

Re-run the benches and update the tables above when changing anything on the pipeline: a new render pass or block kind, a change to table layout or the inline renderer, a new `RenderSettings` field (which invalidates the whole cache when it changes), or a change to how highlighting is parsed or capped. Note the machine — these numbers are only comparable within one.
