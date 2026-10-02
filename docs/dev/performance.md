# Performance — the parse/render pipeline

The one hot path in edamame is the eager, full-document work an edit triggers. This page records its budget, what each stage costs, the two optimizations that must not be undone, and the known ceilings.

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

`benches/pipeline.rs` generates deterministic documents in-process at **1k / 5k / 20k source lines** in seven mixes. Cost per block varies enormously across them; keep them stable so future measurements stay comparable.

| Corpus | Composition | Stresses |
|---|---|---|
| `prose` | Paragraphs with bold/links | Inline rendering, virtual blank-line blocks, reflow |
| `lists` | Deep nested lists with checkboxes | List post-pass and rendering |
| `tables` | Many medium tables | Table column measurement (`table_layout`) |
| `code` | Fenced `rust` blocks | Syntax highlighting, NBSP padding, cheap inlines |
| `math` | Prose with inline `$…$` + stacked `$$…$$` blocks | Math delimiter scan, per-formula display-math promotion/split |
| `nested` | Lists wrapping `rust` code, blockquotes wrapping tables | The cache's subtree-gate (`is_cache_worthy`): expensive content inside a cheap container must stay cached |
| `mixed` | Blend + headings + footnotes | Anchors, source map, everything |

`math` measures only the synchronous parse + promotion: a `$$…$$` block becomes a `Block::ImageBlock` whose RaTeX raster is produced later by the async decode worker, off this path like every image and mermaid diagram. `nested` guards the cache-worthy gate (below): if containers of expensive content stop being cached, its cold→memoized gap collapses.

Harness details that matter for reproducibility:

- **Grammars are warmed on the bench thread first** (`warm_grammars` → `highlight::warm_inline`). Highlighting is eventually-consistent in the live app ([syntax-highlighting.md](syntax-highlighting.md)), so without it the `code` and `mixed` numbers would mix the highlighted and plain paths.
- **`full_pipeline_memoized` alternates between two sources differing in one character**, so every build is a warm cache with exactly one changed block — the steady-state edit cost. `full_pipeline` is the cold-open / whole-document-paste cost.
- **`build_doc` runs with paragraph reflow on** (the shipped default); `render_only` sets the same flag so the derived `other` residual stays honest.
- **Every group uses flat sampling** (`SamplingMode::Flat`). Criterion's default linear sampling runs 1 + 2 + … + 10 = 55 iterations at sample size 10 no matter how slow one is, so the slow cases overran their 2 s budget many times over and the suite took ~15 min on an Intel Core Ultra 7 258V laptop. Flat sampling needs only 10. It changes how samples are taken, not what is measured.
- **The noise threshold is 5%** (criterion's default is 1%). Laptop run-to-run noise is several percent, so at 1% most cases come back "changed"; see [Checking a release for regressions](#checking-a-release-for-regressions).
- **`visual_cache_build`'s width cycling forces a cold rebuild every call.**
- **The pre-merge two-pass benches are gone.** `parse_offsets` and `parse_ast` timed the two separate passes that `parse_merged` replaced. They proved the merge, which a test now pins (below), and one of them timed a function the app no longer runs.
- **There is no 100k size.** Scaling is linear at every size measured, so it added no signal, and its cases were about half the suite's runtime.

`cargo bench --bench pipeline` to reproduce.

## Results

### Apple M3 (macOS)

Measured 2026-10-02 on macOS 15.7.5 (24G624), `rustc 1.98.0 (88d9e12ae 2026-08-18)`, criterion 0.8.2. Each figure is the median of three full-suite runs' criterion means, in ms. A single run let a few slow samples inflate one case's mean by 22%, and the median of three absorbs that.

**Steady-state edit** (`full_pipeline_memoized`):

| Corpus | 1k | 5k | 20k |
|---|---|---|---|
| `prose` | 0.859 | 4.27 | 20.4 |
| `lists` | 0.715 | 3.64 | 16.2 |
| `tables` | 0.974 | 5.33 | 24.8 |
| `code` | 0.289 | 0.989 | 4.04 |
| `math` | 0.448 | 2.25 | 9.67 |
| `nested` | 0.441 | 2.20 | 10.6 |
| `mixed` | 0.636 | 3.03 | 13.5 |

Every corpus stays under 8 ms at 5k. At 20k, `prose`, `lists` and `tables` exceed the 16 ms budget, and `math`, `nested` and `mixed` are in the marginal band.

**Cold open** (`full_pipeline`):

| Corpus | 1k | 5k | 20k |
|---|---|---|---|
| `prose` | 0.809 | 4.27 | 20.0 |
| `lists` | 0.686 | 3.63 | 15.9 |
| `tables` | 2.92 | 15.8 | 66.6 |
| `code` | 3.97 | 20.0 | 81.0 |
| `math` | 0.451 | 2.24 | 9.82 |
| `nested` | 6.36 | 32.9 | 136 |
| `mixed` | 1.23 | 6.24 | 26.7 |

`code` and `nested` exceed 16 ms from 5k, and `tables` is just under it there. At 20k, every corpus except `lists` and `math` takes longer than one frame.

**Stage breakdown at 20k:**

`other` = `full − (parse_merged + render_only)`: post-passes, virtual blank-line blocks, `SourceMap` and anchors. It is derived from three separately sampled figures, so a small negative residual (`code`'s) is measurement noise. `tables`' larger one comes from `render_only/tables/20000`, which measured 51.3, 61.8 and 57.9 ms across the three runs, while `full_pipeline/tables/20000` held at 66.6 ms. That one stage figure is unstable, so its share is approximate.

| Corpus | full | `parse_merged` | `render_only` | other | Dominant |
|---|---|---|---|---|---|
| `prose` | 20.0 | 12.8 | 5.18 | 2.06 | parse (63.9%) |
| `lists` | 15.9 | 9.22 | 4.47 | 2.22 | parse (58.0%) |
| `tables` | 66.6 | 14.3 | 57.9 | −5.59 | render (86.9%) |
| `code` | 81.0 | 0.424 | 80.8 | −0.229 | render (99.8%) |
| `math` | 9.82 | 2.65 | 1.70 | 5.47 | other (55.7%) |
| `nested` | 136 | 4.69 | 130 | 1.41 | render (95.5%) |
| `mixed` | 26.7 | 7.16 | 17.0 | 2.54 | render (63.7%) |

`mixed` across 1k / 5k / 20k: full 1.23 / 6.24 / 26.7, `parse_merged` 0.342 / 1.76 / 7.16, `render_only` 0.822 / 4.11 / 17.0, other 0.0659 / 0.373 / 2.54.

Parse alone takes 12.8 ms for `prose` and 14.3 ms for `tables` at 20k, most of the 16 ms budget before any rendering. Memoization cannot remove that cost (see [Known ceilings](#known-ceilings)).

**Memoization** (change from `full_pipeline` to `full_pipeline_memoized` at 20k):

| Corpus | Change |
|---|---|
| `prose` | +1.78% |
| `lists` | +1.76% |
| `tables` | −62.8% |
| `code` | −95.0% |
| `math` | −1.55% |
| `nested` | −92.2% |
| `mixed` | −49.6% |

Memoization brings `code` (81.0 → 4.04 ms), `nested` (136 → 10.6 ms) and `mixed` (26.7 → 13.5 ms) under the budget at 20k. `tables` (66.6 → 24.8 ms) still exceeds it there, because most of its remaining cost is the uncached parse.

**Resize** (`visual_cache_build`, `mixed`):

| 1k | 5k | 20k |
|---|---|---|
| 1.38 | 6.42 | 27.7 |

A rebuild fits in one frame at 5k, but at 20k it takes 27.7 ms, nearly two frames. It runs once per quiesced resize, not once per keystroke (see [Known ceilings](#known-ceilings)).

What these numbers show:

- **Scaling is close to linear** in every corpus and measured stage: 20× the lines costs 20–25× the time, except memoized `code`, at 14.0× because fixed per-build overhead dominates at 1k. `mixed`'s derived `other` residual grows faster (39× from 1k to 20k), but at 1k it is 0.0659 ms, the small difference of three ~1 ms figures, so that ratio carries little signal.
- **`prose` and `lists` are parse-bound; `tables`, `code`, `nested` and `mixed` are render-bound.** Syntax highlighting makes `code` render-bound. Its parse is nearly free (0.424 ms at 20k), since a fence is one AST node. `nested` has the same profile at higher cost.
- **`math` is the one `other`-bound corpus** (55.7%): the per-formula `$$` scan, image-block promotion, and source-map / anchor derivation over many short blocks.
- **Memoization pays off where a re-render costs far more than a hash-and-clone lookup.** At 20k it saves 95.0% for `code`, 92.2% for `nested`, 62.8% for `tables` and 49.6% for `mixed`. The cheap mixes never enter the cache (the cache-worthy gate, [below](#the-two-optimizations-and-why-they-must-not-be-undone)), so their memoized cost stays within ±1.8% of their cold cost. `nested` shows the gate walking *into* containers.

## The two optimizations, and why they must not be undone

- **One parse, not two.** `parse_raw_with_ranges` collects top-level byte ranges from a `parse_offsets::RangeTracker` observing the same offset-iterator events the AST builder consumes, so blocks and ranges stay 1:1 *by construction*. Splitting them back into two passes costs a full extra parse per reparse.
- **Block-level render memoization.** `RenderCache` maps an unchanged block to a clone of its lines instead of a re-render; this is what makes table- and code-heavy documents editable at all. Its correctness rules (AST-value keying, the `RenderSettings` fingerprint, `ImageBlock` exclusion) are in [editing-model.md](editing-model.md). Two cost properties:
  - **Keys hash with `FxHasher` (`rustc-hash`), not SipHash.** The keys are local document content, so DoS resistance buys nothing, and SipHash is slow on the many small `write_*` calls a nested `Block` makes. (seahash benches *slower* than SipHash here — it is tuned for whole byte buffers.)
  - **Only cache-worthy blocks are stored** (`render_cache::is_cache_worthy`): a `Table`/`CodeBlock`, or a `List`/`BlockQuote` containing one. Cheap blocks re-render for less than a lookup costs. The gate is a subtree walk, not a match on the outer kind, so an expensive block inside a cheap container stays cached.

Both are asserted: `merged_parse_matches_two_pass_parse` (`src/markdown/parser.rs`) pins the merged parse to the two-pass pairing; `cached_render_matches_uncached` plus the eviction, settings-invalidation, syntax-toggle and image-bypass tests (`src/markdown/renderer.rs`) pin cached output to uncached; `cheap_blocks_bypass_cache` and `is_cache_worthy_follows_nested_expensive_content` pin the gate.

## Known ceilings

Facts about the current design, not tasks.

- **The full-document parse floor.** The single parse is O(document) and cannot be memoized (7.16 ms for `parse_merged/mixed/20000` on the Apple M3) — and dominates prose and lists. Only incremental reparsing removes it, which must handle the non-local effects of fences, setext headings, lists and footnote definitions; a separate project.
- **Clone-on-hit.** A hit still clones the block's `Vec<Line>`; on cached (expensive) blocks that is a small share of the render. Removing it means sharing lines as `Arc<[Line]>`, which changes `ParsedDoc::lines`' type and ripples through every view — worth it only if very large table-/code-heavy documents matter.
- **Resize.** The `visual_cache_build` rebuild exceeds a frame from roughly 12k lines (6.42 ms at 5k and 27.7 ms at 20k on the Apple M3), but fires only on a width change behind the 80 ms `RESIZE_QUIESCE` window (`app::frame_timer`) — one rebuild per quiesced drag. Leave it unless live-resize jank shows up.
- **`parse_offsets::top_level_block_ranges` is off the edit path** — it survives only as the oracle in `merged_parse_matches_two_pass_parse` (the diff subsystem uses the sibling `block_ranges_by`).

## Checking a release for regressions

Run the end-to-end groups before every release (the step lives in [releasing.md](releasing.md)) and compare them against the previous release's saved criterion baseline. One sampling run is enough: `--save-baseline` stores it under the new version's name, and `--load-baseline` compares that stored data against the old one without re-running:

```bash
F='^(full_pipeline|visual_cache)'
cargo bench --bench pipeline -- --noplot "$F" --save-baseline v0.1.5
cargo bench --bench pipeline -- --noplot "$F" --load-baseline v0.1.5 --baseline v0.1.4
```

- **The baseline lives in `target/criterion/`** on the machine that produced it, so one machine has to carry the check from release to release, and `cargo clean` deletes it. With no baseline from the previous release (the first check, a new machine, a `cargo clean`), the simplest course is to skip that release's comparison and only save. When the comparison matters, rebuild the baseline from the previous tag with `git worktree add ../edamame-prev v0.1.4`, then `cargo bench … --save-baseline v0.1.4` from inside it. Point `CARGO_TARGET_DIR` at the main checkout's `target/` so the baseline lands where the current tree's run will look, and expect a full rebuild, since the old tag's dependencies differ.
- **Re-save the previous release's baseline after a toolchain or OS upgrade**, and keep the machine in the same power state both times (plugged in, same power profile). Either one shifts every number and reads as a regression or an improvement across the board.
- **A "regressed" line is a lead, not a verdict.** On a laptop, unchanged code can move ±5–8% between runs. Confirm a suspect by running the two builds' bench binaries (`target/release/deps/pipeline-*`) back to back, a few rounds each, filtered to the suspect case. Drift affects both builds alike that way.
- **`--noplot` skips criterion's HTML charts**, about a fifth of the run time. Drop it when you want the charts to look into a regression.
- **The stage groups** (`parse_merged`, `render_only`) **are diagnostics.** Run them when an end-to-end case regresses, to find the stage that moved.

## When to re-measure

Re-run the benches and update the tables when changing the pipeline: a new render pass or block kind, table layout or the inline renderer, a new `RenderSettings` field, or how highlighting is parsed or capped. Note the machine.
