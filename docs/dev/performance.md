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

Measured 2026-10-04 on macOS 15.7.5 (24G624), `rustc 1.98.0 (88d9e12ae 2026-08-18)`, criterion 0.8.2, mimalloc 0.1.52. Each figure is the median of three full-suite runs' criterion means, in ms. The three runs agreed within ~2% on most cases; the median absorbs the two that moved ~9% (`render_only/tables/20000`, `full_pipeline/nested/5000`).

**Steady-state edit** (`full_pipeline_memoized`):

| Corpus | 1k | 5k | 20k |
|---|---|---|---|
| `prose` | 0.555 | 2.74 | 11.6 |
| `lists` | 0.456 | 2.27 | 9.51 |
| `tables` | 0.609 | 3.00 | 13.8 |
| `code` | 0.218 | 0.626 | 2.27 |
| `math` | 0.366 | 1.73 | 7.03 |
| `nested` | 0.296 | 1.47 | 6.86 |
| `mixed` | 0.401 | 1.90 | 8.23 |

Every corpus stays under 8 ms at 5k, with room to spare — the slowest, `tables`, takes 3.00 ms. At 20k no corpus exceeds the 16 ms budget: `code`, `nested` and `math` stay inside 8 ms, and `prose`, `lists`, `tables` and `mixed` sit in the marginal band.

**Cold open** (`full_pipeline`):

| Corpus | 1k | 5k | 20k |
|---|---|---|---|
| `prose` | 0.546 | 2.74 | 11.5 |
| `lists` | 0.458 | 2.26 | 9.45 |
| `tables` | 1.78 | 9.08 | 41.5 |
| `code` | 3.46 | 17.1 | 69.1 |
| `math` | 0.365 | 1.73 | 7.04 |
| `nested` | 5.62 | 29.3 | 119 |
| `mixed` | 0.786 | 3.87 | 16.5 |

`code` and `nested` exceed 16 ms from 5k; `tables` is comfortably inside it there, at 9.08 ms. At 20k, `tables`, `code`, `nested` and `mixed` take longer than one frame, while `prose`, `lists` and `math` stay inside it.

**Stage breakdown at 20k:**

`other` = `full − (parse_merged + render_only)`: post-passes, virtual blank-line blocks, `SourceMap` and anchors. It is derived from three separately sampled figures, so small residuals carry little signal — though every one is positive this time. `render_only/tables/20000` remains the least stable stage figure, measuring 28.4, 31.0 and 30.1 ms across the three runs while `full_pipeline/tables/20000` held within 1% of 41.5 ms, so `tables`' split is approximate.

| Corpus | full | `parse_merged` | `render_only` | other | Dominant |
|---|---|---|---|---|---|
| `prose` | 11.5 | 7.74 | 2.75 | 0.981 | parse (67.5%) |
| `lists` | 9.45 | 5.47 | 2.73 | 1.25 | parse (57.9%) |
| `tables` | 41.5 | 8.43 | 30.1 | 3.03 | render (72.4%) |
| `code` | 69.1 | 0.358 | 68.0 | 0.732 | render (98.4%) |
| `math` | 7.04 | 2.03 | 0.979 | 4.03 | other (57.3%) |
| `nested` | 119 | 3.03 | 115 | 0.730 | render (96.8%) |
| `mixed` | 16.5 | 4.53 | 10.3 | 1.64 | render (62.7%) |

`mixed` across 1k / 5k / 20k: full 0.786 / 3.87 / 16.5, `parse_merged` 0.231 / 1.11 / 4.53, `render_only` 0.493 / 2.48 / 10.3, other 0.0619 / 0.278 / 1.64.

Parse alone takes 7.74 ms for `prose` and 8.43 ms for `tables` at 20k, about half the 16 ms budget before any rendering. Memoization cannot remove that cost (see [Known ceilings](#known-ceilings)).

**Memoization** (change from `full_pipeline` to `full_pipeline_memoized` at 20k):

| Corpus | Change |
|---|---|
| `prose` | +1.1% |
| `lists` | +0.7% |
| `tables` | −66.7% |
| `code` | −96.7% |
| `math` | −0.1% |
| `nested` | −94.2% |
| `mixed` | −50.2% |

Memoization brings every corpus under the budget at 20k: `code` (69.1 → 2.27 ms), `nested` (119 → 6.86 ms), `mixed` (16.5 → 8.23 ms) and `tables` (41.5 → 13.8 ms), the last of these into the marginal band, since most of its remaining cost is the uncached parse.

**Resize** (`visual_cache_build`, `mixed`):

| 1k | 5k | 20k |
|---|---|---|
| 1.14 | 5.60 | 18.7 |

A rebuild fits in one frame at 5k, but at 20k it takes 18.7 ms, just over one. It runs once per quiesced resize, not once per keystroke (see [Known ceilings](#known-ceilings)).

**The system allocator compared with mimalloc.** The figures above replace a set measured on the same machine with the system allocator, and every case got faster — nothing regressed. Steady-state edits fell 18–44%, cold opens 11–43%, `parse_merged` 16–41% and `render_only` 11–48% (both at 20k), and resizes 13–32%. The gain grows with document size in every steady-state corpus (`tables`: −37.5% at 1k, −44.2% at 20k), which is the signature of an allocator whose per-allocation cost rises with heap size. It is smallest where little is allocated per unit of work — `code`'s and `nested`'s cold opens (−13–15% / −11–12%) are dominated by syntect tokenizing, and `math` is `other`-bound (−18% to −28%).

What these numbers show:

- **Scaling is close to linear** in every corpus and measured stage: 20× the lines costs 19–23× the time, except memoized `code`, at 10.4× because fixed per-build overhead dominates at 1k. `mixed`'s derived `other` residual grows faster (26× from 1k to 20k), but at 1k it is 0.0619 ms, the small difference of three sub-millisecond figures, so that ratio carries little signal.
- **`prose` and `lists` are parse-bound; `tables`, `code`, `nested` and `mixed` are render-bound.** Syntax highlighting makes `code` render-bound. Its parse is nearly free (0.358 ms at 20k), since a fence is one AST node. `nested` has the same profile at higher cost.
- **`math` is the one `other`-bound corpus** (57.3%): the per-formula `$$` scan, image-block promotion, and source-map / anchor derivation over many short blocks.
- **Memoization pays off where a re-render costs far more than a hash-and-clone lookup.** At 20k it saves 96.7% for `code`, 94.2% for `nested`, 66.7% for `tables` and 50.2% for `mixed`. The cheap mixes never enter the cache (the cache-worthy gate, [below](#the-two-optimizations-and-why-they-must-not-be-undone)), so their memoized cost stays within ±1.1% of their cold cost. `nested` shows the gate walking *into* containers.

### Intel Core Ultra 7 258V (Linux)

Measured 2026-10-02 on Debian 13 (Linux 7.1.8), `rustc 1.98.0 (88d9e12ae 2026-08-18)`, criterion 0.8.2, mimalloc 0.1.52, on AC power with the `power-saver` profile. The end-to-end figures are the median of three runs' criterion means, in ms. The stage figures (`parse_merged`, `render_only`) come from one run, since the release subset doesn't include them.

**Steady-state edit** (`full_pipeline_memoized`):

| Corpus | 1k | 5k | 20k |
|---|---|---|---|
| `prose` | 1.88 | 10.0 | 43.3 |
| `lists` | 1.44 | 7.94 | 34.9 |
| `tables` | 2.19 | 11.9 | 49.5 |
| `code` | 0.608 | 2.01 | 9.19 |
| `math` | 1.18 | 6.36 | 27.3 |
| `nested` | 0.928 | 5.34 | 33.9 |
| `mixed` | 1.31 | 7.03 | 31.6 |

Every corpus stays inside the 16 ms budget at 5k; `tables` and `prose` are in the marginal band there. At 20k only `code` stays inside it (marginal), and every other corpus exceeds it.

**Cold open** (`full_pipeline`):

| Corpus | 1k | 5k | 20k |
|---|---|---|---|
| `prose` | 1.83 | 9.92 | 42.7 |
| `lists` | 1.43 | 7.72 | 34.7 |
| `tables` | 5.07 | 27.8 | 121 |
| `code` | 9.50 | 47.6 | 193 |
| `math` | 1.18 | 6.38 | 26.9 |
| `nested` | 16.0 | 82.8 | 333 |
| `mixed` | 2.34 | 12.4 | 53.7 |

**Stage breakdown at 20k:**

| Corpus | full | `parse_merged` | `render_only` | other | Dominant |
|---|---|---|---|---|---|
| `prose` | 42.7 | 32.4 | 8.43 | 1.88 | parse (75.9%) |
| `lists` | 34.7 | 22.6 | 10.4 | 1.73 | parse (65.0%) |
| `tables` | 121 | 36.1 | 87.8 | −2.73 | render (72.4%) |
| `code` | 193 | 1.35 | 188 | 3.71 | render (97.4%) |
| `math` | 26.9 | 8.28 | 2.72 | 15.9 | other (59.1%) |
| `nested` | 333 | 12.3 | 317 | 4.00 | render (95.1%) |
| `mixed` | 53.7 | 19.2 | 30.8 | 3.74 | render (57.3%) |

`mixed` across 1k / 5k / 20k: full 2.34 / 12.4 / 53.7, `parse_merged` 0.846 / 4.42 / 19.2, `render_only` 1.32 / 6.91 / 30.8.

**Memoization** (change from `full_pipeline` to `full_pipeline_memoized` at 20k):

| Corpus | Change |
|---|---|
| `prose` | +1.4% |
| `lists` | +0.6% |
| `tables` | −59.2% |
| `code` | −95.2% |
| `math` | +1.3% |
| `nested` | −89.8% |
| `mixed` | −41.3% |

**Resize** (`visual_cache_build`, `mixed`):

| 1k | 5k | 20k |
|---|---|---|
| 3.75 | 16.9 | 53.5 |

### Linux compared with the M3

Both sections are measured with mimalloc, so the ratios below are a hardware comparison.

- **Linux is ~2.7–3.7× slower on cold opens** and ~2.8–4.0× on steady-state edits at 1k and 5k. The M3's shapes hold: the same corpora are parse-, render- and `other`-bound, by similar shares, and memoization saves about as much (95.2% / 89.8% for `code` / `nested`, against 96.7% / 94.2%).
- **Parsing is where Linux lags most.** `parse_merged` is 3.8–4.3× the M3 figure at 20k, a tight band across every corpus, while `render_only` is 2.7–3.8× — so the parse-bound corpora (`prose`, `lists`) are the ones whose gap widens with size.
- **Memoized `nested` still scales a little worse than linear.** It grows 6.3× from 5k to 20k (M3: 4.7×), while every other corpus grows 4.2–4.6×. It is minor, but worth watching.

**Why the binary uses mimalloc.** Under glibc's allocator, memoized `code` and `nested` scaled superlinearly on Linux: 6.8× and 9.0× from 5k to 20k, reaching 8.5× and 7.7× the M3 figure at 20k, and memoization saved only 84% and 78%. A profile of `full_pipeline_memoized/code` put 51–55% of the time inside glibc's `malloc`/`free`, at 5k and 20k alike. Each hit clones the block's `Vec<Line>` ([clone-on-hit](#known-ceilings)), one `String` per span, and dropping the previous build frees them all. That churn costs more per allocation as the heap grows. Raising glibc's `tcache_count` only got 20k from 39 to 27 ms. mimalloc took it to 9.19 ms, and against the glibc figures it cut 14–73% from every steady-state edit, 11–37% from every cold open, 11–42% from rendering, 15–24% from parsing, and 6–13% from resizes.

## The two optimizations, and why they must not be undone

- **One parse, not two.** `parse_raw_with_ranges` collects top-level byte ranges from a `parse_offsets::RangeTracker` observing the same offset-iterator events the AST builder consumes, so blocks and ranges stay 1:1 *by construction*. Splitting them back into two passes costs a full extra parse per reparse.
- **Block-level render memoization.** `RenderCache` maps an unchanged block to a clone of its lines instead of a re-render; this is what makes table- and code-heavy documents editable at all. Its correctness rules (AST-value keying, the `RenderSettings` fingerprint, `ImageBlock` exclusion) are in [editing-model.md](editing-model.md). Two cost properties:
  - **Keys hash with `FxHasher` (`rustc-hash`), not SipHash.** The keys are local document content, so DoS resistance buys nothing, and SipHash is slow on the many small `write_*` calls a nested `Block` makes. (seahash benches *slower* than SipHash here — it is tuned for whole byte buffers.)
  - **Only cache-worthy blocks are stored** (`render_cache::is_cache_worthy`): a `Table`/`CodeBlock`, or a `List`/`BlockQuote` containing one. Cheap blocks re-render for less than a lookup costs. The gate is a subtree walk, not a match on the outer kind, so an expensive block inside a cheap container stays cached.

Both are asserted: `merged_parse_matches_two_pass_parse` (`src/markdown/parser.rs`) pins the merged parse to the two-pass pairing; `cached_render_matches_uncached` plus the eviction, settings-invalidation, syntax-toggle and image-bypass tests (`src/markdown/renderer.rs`) pin cached output to uncached; `cheap_blocks_bypass_cache` and `is_cache_worthy_follows_nested_expensive_content` pin the gate.

## Known ceilings

Facts about the current design, not tasks.

- **The full-document parse floor.** The single parse is O(document) and cannot be memoized (4.53 ms for `parse_merged/mixed/20000` on the Apple M3) — and dominates prose and lists. Only incremental reparsing removes it, which must handle the non-local effects of fences, setext headings, lists and footnote definitions; a separate project.
- **Clone-on-hit.** A hit still clones the block's `Vec<Line>`; on cached (expensive) blocks that is a small share of the render. The allocator decides what that costs: under glibc's it was half of a 20k memoized build on Linux, and mimalloc (see `Cargo.toml`) brought it back to a small share. Removing it means sharing lines as `Arc<[Line]>`, which changes `ParsedDoc::lines`' type and ripples through every view — worth it only if very large table-/code-heavy documents matter.
- **Resize re-renders the whole document cold.** A width change reaches `EditorState::set_viewport_width`, which calls `refresh_parsed`. The width is part of the `RenderSettings` fingerprint, so the render cache is cleared and every block re-renders: a `full_pipeline` (cold-open) cost, plus the `visual_cache_build` prefix-sum rebuild. For a 20k-line `nested` document that is about 119 + 19 ms on the Apple M3 and 333 + 54 ms on Linux. The `visual_cache_build` part alone exceeds a frame from roughly 17k lines. Both run once per quiesced drag, behind the 80 ms `RESIZE_QUIESCE` window (`app::frame_timer`). The same full re-render follows anything else in the fingerprint: a theme change, a grammar finishing its warm-up (`App::tick_syntax_warm` bumps the highlight generation), and a switch into or out of Raw mode (reflow). In code-heavy documents nearly all of that cost is syntax highlighting, which depends only on each block's language and source. Leave it unless resize or theme-switch lag shows up.
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
