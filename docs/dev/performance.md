# Performance — the parse/render pipeline

The one hot path in edamame is the eager, full-document work an edit triggers. This page records its budget, what each stage costs, the two optimizations that must not be undone, and the known ceilings.

## What runs on an edit

Every line-crossing edit — and every deferred flush of an in-line typing burst — calls `EditorState::refresh_parsed()`, which rebuilds the whole document:

1. **Parse.** One pulldown-cmark pass (`markdown::parser::parse_document`) yielding the AST *and* top-level byte ranges together.
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
- **Every group uses flat sampling** (`SamplingMode::Flat`): 10 iterations per case instead of linear sampling's 55, which made the slow cases overrun their 2 s budget many times over. It changes how samples are taken, not what is measured.
- **The noise threshold is 5%** (criterion's default is 1%), since laptop run-to-run noise is several percent.
- **`visual_cache_build`'s width cycling forces a cold rebuild every call.**

`cargo bench --bench pipeline` to reproduce; on a hybrid CPU, pin it ([below](#checking-a-release-for-regressions)).

## Results

### Apple M3 (macOS)

Measured 2026-10-04 on macOS 15.7.5 (24G624), `rustc 1.98.0 (88d9e12ae 2026-08-18)`, criterion 0.8.2, mimalloc 0.1.52, before row provenance: expect today's figures up to ~10% higher on edits (see [Cost of row provenance](#cost-of-row-provenance)). Each figure is the median of three full-suite runs' criterion means, in ms. The three runs agreed within ~2% on most cases; the median absorbs the two that moved ~9% (`render_only/tables/20000`, `full_pipeline/nested/5000`).

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

`other` = `full − (parse_merged + render_only)`: post-passes, virtual blank-line blocks, `SourceMap` and anchors. It is derived from three separately sampled figures, so small residuals carry little signal. `render_only/tables/20000` is the least stable stage figure (28.4–31.0 ms across runs), so `tables`' split is approximate.

| Corpus | full | `parse_merged` | `render_only` | other | Dominant |
|---|---|---|---|---|---|
| `prose` | 11.5 | 7.74 | 2.75 | 0.981 | parse (67.5%) |
| `lists` | 9.45 | 5.47 | 2.73 | 1.25 | parse (57.9%) |
| `tables` | 41.5 | 8.43 | 30.1 | 3.03 | render (72.4%) |
| `code` | 69.1 | 0.358 | 68.0 | 0.732 | render (98.4%) |
| `math` | 7.04 | 2.03 | 0.979 | 4.03 | other (57.3%) |
| `nested` | 119 | 3.03 | 115 | 0.730 | render (96.8%) |
| `mixed` | 16.5 | 4.53 | 10.3 | 1.64 | render (62.7%) |

Parse alone takes 7.74 ms for `prose` and 8.43 ms for `tables` at 20k, about half the 16 ms budget before any rendering. Memoization cannot remove that cost (see [Known ceilings](#known-ceilings)). Memoization brings every corpus under the budget at 20k, `tables` (41.5 → 13.8 ms) into the marginal band, since most of its remaining cost is the uncached parse.

**Resize** (`visual_cache_build`, `mixed`): 1.14 / 5.60 / 18.7 ms at 1k / 5k / 20k. It fits a frame at 5k and just misses one at 20k, once per quiesced resize, not per keystroke (see [Known ceilings](#known-ceilings)).

What these numbers show (M3 figures; the Linux shapes match):

- **Scaling is close to linear** in every corpus and measured stage: 20× the lines costs 19–23× the time, except memoized `code`, at 10.4× because fixed per-build overhead dominates at 1k.
- **`prose` and `lists` are parse-bound; `tables`, `code`, `nested` and `mixed` are render-bound.** Syntax highlighting makes `code` render-bound. Its parse is nearly free (0.358 ms at 20k), since a fence is one AST node. `nested` has the same profile at higher cost.
- **`math` is the one `other`-bound corpus** (57.3%): the per-formula `$$` scan, image-block promotion, and source-map / anchor derivation over many short blocks.
- **Memoization pays off where a re-render costs far more than a hash-and-clone lookup.** At 20k it saves 96.7% for `code`, 94.2% for `nested`, 66.7% for `tables` and 50.2% for `mixed`. The cheap mixes never enter the cache (the cache-worthy gate, [below](#the-two-optimizations-and-why-they-must-not-be-undone)), so their memoized cost stays within ±1.1% of their cold cost. `nested` shows the gate walking *into* containers.

### Intel Core Ultra 7 258V (Linux)

Measured 2026-10-27 at `develop` 3bef292 with the v0.1.7 release bump on Debian 13 (Linux 7.1.8), `rustc 1.99.0 (b940084d7 2026-09-28)`, criterion 0.8.2, mimalloc 0.1.52, on AC power with the `power-saver` profile, pinned to CPU 3. Each figure is the median of three full-suite runs' criterion means, in ms. The runs agreed within 1.8% on the median case; in eight cases one run hit a slow stretch (12–101% high), which the median discards.

**Steady-state edit** (`full_pipeline_memoized`):

| Corpus | 1k | 5k | 20k |
|---|---|---|---|
| `prose` | 1.99 | 11.0 | 47.4 |
| `lists` | 1.66 | 9.21 | 39.3 |
| `tables` | 2.47 | 13.3 | 56.2 |
| `code` | 0.596 | 1.87 | 7.98 |
| `math` | 1.33 | 7.31 | 31.1 |
| `nested` | 0.997 | 5.44 | 26.0 |
| `mixed` | 1.49 | 7.86 | 34.5 |

Every corpus stays inside the 16 ms budget at 5k; `tables`, `prose` and `lists` are in the marginal band, and `mixed` is just under it. At 20k only `code` stays inside it, just under 8 ms.

**Cold open** (`full_pipeline`):

| Corpus | 1k | 5k | 20k |
|---|---|---|---|
| `prose` | 1.98 | 10.9 | 47.0 |
| `lists` | 1.68 | 9.15 | 39.3 |
| `tables` | 5.10 | 27.6 | 117 |
| `code` | 9.14 | 47.1 | 187 |
| `math` | 1.36 | 7.71 | 32.8 |
| `nested` | 15.7 | 80.7 | 332 |
| `mixed` | 2.43 | 13.0 | 55.7 |

**Stage breakdown at 20k** (`code`'s `render_only` measured slightly above its `full`, so its residual is noise around zero):

| Corpus | full | `parse_merged` | `render_only` | other | Dominant |
|---|---|---|---|---|---|
| `prose` | 47.0 | 35.7 | 8.75 | 2.58 | parse (75.9%) |
| `lists` | 39.3 | 26.4 | 9.41 | 3.49 | parse (67.2%) |
| `tables` | 117 | 42.8 | 71.9 | 2.34 | render (61.4%) |
| `code` | 187 | 1.96 | 188 | ≈ 0 | render (~100%) |
| `math` | 32.8 | 9.89 | 3.09 | 19.8 | other (60.4%) |
| `nested` | 332 | 14.7 | 314 | 3.49 | render (94.5%) |
| `mixed` | 55.7 | 22.5 | 29.0 | 4.19 | render (52.1%) |

**Memoization** at 20k saves 95.7% for `code`, 92.2% for `nested`, 52.0% for `tables`, 38.1% for `mixed` and 5.3% for `math`; `prose` and `lists` stay within 1% of their cold cost.

**Resize** (`visual_cache_build`, `mixed`): 3.63 / 17.0 / 54.6 ms at 1k / 5k / 20k.

**Against v0.1.6: no significant change.** The release check compared these runs with a `v0.1.6` baseline rebuilt on the same machine, toolchain and core. All 63 cases, stage groups included, are within −5.8% to +3.5%, inside run-to-run noise; no budget verdict changes. The one pipeline-adjacent change between the tags, a correctness fix in `inline_col_map` and `row_map` for reflowed emphasis, doesn't register.

#### Cost of row provenance

Row provenance records each leaf's source positions during the parse and each rendered row's origin during the render. Its cost was measured as user-space instructions per iteration on a pinned P-core (`perf stat -e cpu_core/instructions/u`), comparing the tree before the plan (`6cf3e85`) with the finished one, same toolchain, since wall-clock noise would hide most of these deltas. Each figure is (count at N iterations − count at 0) / N, each count the median of five runs; the noise floor is about ±1.5%.

| Corpus (20k) | `parse_merged` | `render_only` | `full_pipeline` | `full_pipeline_memoized` |
|---|---|---|---|---|
| `prose` | +8.3% | +0.1% | +4.2% | +3.0% |
| `lists` | +15.4% | +22.9% | +7.4% | +6.3% |
| `tables` | +9.9% | −1.0% | +2.2% | +9.7% |
| `code` | +47.9% (of an 8.5M-instruction parse) | +0.2% | +0.2% | +5.3% |
| `math` | +16.7% | +0.6% | +10.7% | +10.6% |
| `nested` | +16.8% | +0.1% | +0.5% | +0.4% |
| `mixed` | +12.7% | +1.3% | +2.7% | +8.2% |

- **The cost is the parse, plus rendering lists.** Recording positions adds 8–17% to every parse. List rendering records the most row origins per line; everything else renders within noise of before.
- **An edit pays more of it than a cold open.** A cold open of `tables`, `code` or `nested` is dominated by rendering, which barely moved; a memoized edit re-renders one block but still re-parses the whole document, so the parse increase passes straight through.
- **Nested reflow costs nothing here** (every end-to-end case within ±1.6%). Its added work is per cursor move, outside the pipeline.
- **Wall-clock time moves a few points more than instructions** (+6–19% on parse-bound edits against v0.1.5), as cycles usually run. The one budget change: `lists` at 5k moved from fine into the marginal band.
- **The cost was accepted (2026-10-07).** It is a fixed share of the parse, not one that grows with document size, and it pays for one source of truth for row ↔ source mapping. Compact `SrcLines` storage and the other reductions are in the plan; shrinking `Block` back from 112 bytes didn't help. Only incremental reparsing would remove the parse share (see [Known ceilings](#known-ceilings)).

### Linux compared with the M3

Both are measured with mimalloc, but the M3 figures predate row provenance, so the ratios below include its 8–17% parse cost on the Linux side.

- **Linux is 2.6–4.7× slower on cold opens.** Render-bound corpora (`tables`, `code`, `nested`, `mixed`) sit at 2.6–3.4×; parse- and `other`-bound ones (`prose`, `lists`, `math`) at 3.6–4.7×, widening with size. The shapes hold: the same corpora are parse-, render- and `other`-bound, by similar shares, and memoization saves about as much.
- **Parsing is where Linux lags most:** `parse_merged` is 4.6–5.5× the M3 figure at 20k, `render_only` 2.4–3.4×.

**Why the binary uses mimalloc.** Under glibc's allocator, memoized `code` and `nested` scaled superlinearly on Linux (6.8× and 9.0× from 5k to 20k), and memoization saved only 84% and 78%. A profile put 51–55% of `full_pipeline_memoized/code` inside `malloc`/`free`: each hit clones the block's `Vec<Line>` ([clone-on-hit](#known-ceilings)), one `String` per span, and dropping the previous build frees them all, at a per-allocation cost that grows with the heap. mimalloc took 20k from 39 to 9.19 ms (raising glibc's `tcache_count` reached only 27), and cut 14–73% from every steady-state edit and 11–37% from every cold open. On the M3 it cut 18–44% and 11–43%. Every memoized corpus now grows 4.2–4.8× from 5k to 20k on both machines.

## The two optimizations, and why they must not be undone

- **One parse, not two.** `parse_raw_with_ranges` collects top-level byte ranges from a `parse_offsets::RangeTracker` observing the same offset-iterator events the AST builder consumes, so blocks and ranges stay 1:1 *by construction*. Splitting them back into two passes costs a full extra parse per reparse.
- **Block-level render memoization.** `RenderCache` maps an unchanged block to a clone of its lines instead of a re-render; this is what makes table- and code-heavy documents editable at all. Its correctness rules (AST-value keying, the `RenderSettings` fingerprint, `ImageBlock` exclusion) are in [editing-model.md](editing-model.md). Three cost properties:
  - **Keys hash with `FxHasher` (`rustc-hash`), not SipHash.** The keys are local document content, so DoS resistance buys nothing, and SipHash is slow on the many small `write_*` calls a nested `Block` makes. (seahash benches *slower* than SipHash here — it is tuned for whole byte buffers.)
  - **Each block is hashed once per build.** `render_cache::BlockMap` wraps a `hashbrown::HashTable`, not a `HashMap`: the render loop computes `BlockMap::hash` once and passes it to the lookup in this build's map, the `take` from the previous build's, and the insert. A `HashMap` hit hashed the deep `Block` key twice (`remove_entry`, then `insert`). No test can pin this; only the memoized benches would show it being undone.
  - **Only cache-worthy blocks are stored** (`render_cache::is_cache_worthy`): a `Table`/`CodeBlock`, or a `List`/`BlockQuote` containing one. Cheap blocks re-render for less than a lookup costs. The gate is a subtree walk, not a match on the outer kind, so an expensive block inside a cheap container stays cached.

Both are asserted: `merged_parse_matches_two_pass_parse` (`src/markdown/parser.rs`) pins the merged parse to the two-pass pairing; `cached_render_matches_uncached` plus the eviction, settings-invalidation, syntax-toggle and image-bypass tests (`src/markdown/renderer.rs`) pin cached output to uncached; `cheap_blocks_bypass_cache` and `is_cache_worthy_follows_nested_expensive_content` pin the gate.

## Known ceilings

Facts about the current design, not tasks.

- **The full-document parse floor.** The single parse is O(document) and cannot be memoized (`parse_merged/mixed/20000`: 4.53 ms on the M3, 22.5 ms on Linux) — and dominates prose and lists. Only incremental reparsing removes it, which must handle the non-local effects of fences, setext headings, lists and footnote definitions; a separate project.
- **Clone-on-hit.** A hit still clones the block's `Vec<Line>`; on cached (expensive) blocks that is a small share of the render. The allocator decides what that costs: under glibc's it was half of a 20k memoized build on Linux, and mimalloc (see `Cargo.toml`) brought it back to a small share. Removing it means sharing lines as `Arc<[Line]>`, which changes `ParsedDoc::lines`' type and ripples through every view — worth it only if very large table-/code-heavy documents matter.
- **Resize re-renders the whole document cold.** The width is part of the `RenderSettings` fingerprint, so `EditorState::set_viewport_width` → `refresh_parsed` clears the render cache: a `full_pipeline` cost plus the `visual_cache_build` rebuild, about 119 + 19 ms for a 20k-line `nested` document on the M3 and 332 + 55 ms on Linux. The rebuild alone exceeds a frame from roughly 17k lines on the M3 and 5k on Linux. Both run once per quiesced drag, behind the 80 ms `RESIZE_QUIESCE` window (`app::frame_timer`). Anything else in the fingerprint triggers the same re-render: a theme change, a grammar finishing its warm-up (`App::tick_syntax_warm`), and a switch into or out of Raw mode (reflow). In code-heavy documents nearly all of it is syntax highlighting, which depends only on each block's language and source. Leave it unless resize or theme-switch lag shows up.
- **Recording source positions is a per-parse cost** (`parser::stream::EventStream`; see [editing-model.md](editing-model.md)): 8–17% of every parse and 23% of rendering a list ([Cost of row provenance](#cost-of-row-provenance)). It is kept small three ways: a paragraph's recorder looks only at events that can begin a line; `SrcLines` stores the common shape (uniform column, at most two chrome lines) inline, so a common leaf allocates nothing and hashes in a few words; and line lookups step forward from the last one, since events arrive in source order.
- **`parse_offsets::top_level_block_ranges` is off the edit path** — it survives only as the oracle in `merged_parse_matches_two_pass_parse` (the diff subsystem uses the sibling `block_ranges_by`).

## Checking a release for regressions

Before every release (the step lives in [releasing.md](releasing.md)), bench the previous release's tag and the current tree in the same session, then compare. Running both builds together means they share a toolchain, power state, temperature and background load, so a difference between them is the code's, not a change in the machine since some earlier day. The previous tag is built from a worktree into this checkout's `target/`, which reuses most of the dependency builds. `scripts/release.sh bench` runs exactly this (previous tag found automatically, `PREV=` and `CORE=` override it and the pinned core, worktree removed even on failure); the manual equivalent:

```bash
PREV=0.1.6
F='^(full_pipeline|visual_cache)'
git diff --quiet "v$PREV" -- benches/ || echo "bench harness changed since v$PREV"
H='^criterion *=|^\[\[bench\]\]$|^name = "criterion"$'
for f in Cargo.toml Cargo.lock; do
    [[ $(git show "v$PREV:$f" | grep -E -A2 "$H") == "$(grep -E -A2 "$H" "$f")" ]] ||
        echo "bench harness changed in $f since v$PREV"
done
T="$PWD/target"
git worktree add --detach ../edamame-prev "v$PREV"
(cd ../edamame-prev && CARGO_TARGET_DIR="$T" cargo bench --bench pipeline --no-run)
cargo bench --bench pipeline --no-run
(cd ../edamame-prev && CARGO_TARGET_DIR="$T" taskset -c 3 cargo bench --bench pipeline -- --noplot "$F" --save-baseline "v$PREV")
taskset -c 3 cargo bench --bench pipeline -- --noplot "$F" --baseline "v$PREV"
git worktree remove ../edamame-prev
```

The last bench run measures the current tree and reports each case's change against the `v$PREV` run just saved.

- **Pin both runs to CPU 3.** The Linux laptop is a hybrid CPU (performance cores 0–3, low-power efficiency cores 4–7), and the scheduler, especially under `power-saver`, moves an unpinned bench thread between the two partway through a run. One case measured 7.8 ms on a performance core and 27 ms on an efficiency core, and an unpinned run lands in between as a bimodal sample set that criterion reports as a large change: `v0.1.6` → `v0.1.7` read +84% on `full_pipeline_memoized/mixed/5000` unpinned and −0.4% pinned. CPU 3 boosts highest (4.8 GHz, as does CPU 1) and serves fewer interrupts than CPU 0, and the cores differ by a few percent, so pin both runs to the same one. The `--no-run` builds keep `taskset` from confining the compile too. macOS can't pin, so on Apple silicon keep the machine idle and treat a bimodal `target/criterion/<case>/new/sample.json` as a scheduler artifact.
- **A "regressed" line is a lead, not a verdict.** A shared session removes day-to-day drift, not minute-to-minute: even pinned, the core slows for stretches of a run, and one pinned full-suite run had 8 of 63 cases off by 7–110%, all within noise when rechecked. Confirm a suspect by running the two builds' bench binaries (`target/release/deps/pipeline-*`; `ls -t` lists the last built first) back to back, a few rounds each, filtered to the suspect case and pinned, so drift affects both alike.
- **If the harness changed since the tag** (a "harness changed" line prints), the two runs time different corpora or settings and can't be compared; nor can they if the old tag no longer builds with the current toolchain. Skip the check.
- **`--noplot` skips criterion's HTML charts**, about a fifth of the run time.
- **The stage groups** (`parse_merged`, `render_only`) **are diagnostics.** Run them on both builds when an end-to-end case regresses, to find the stage that moved.

## When to re-measure

Re-run the benches and update the tables when changing the pipeline: a new render pass or block kind, table layout or the inline renderer, a new `RenderSettings` field, or how highlighting is parsed or capped. Note the machine.
