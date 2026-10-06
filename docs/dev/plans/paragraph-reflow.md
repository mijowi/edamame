# Paragraph reflow — dropping the 1:1 source-line ↔ rendered-row invariant

Status: **IMPLEMENTED (phases 1–5 complete, 2026-09-07).** Reflow is on by default
(`config.editor.reflow`) in Preview and Rendered; the deep-dive now lives in
[`editing-model.md`](../editing-model.md) ("Prose reflow breaks the 1:1 …") and the user page
[`docs/editing.md`](../../editing.md) ("Paragraphs reflow …"). Its row-mapping references (`sub_lines_in_block` and the per-kind chains) are historical: [`row-provenance.md`](row-provenance.md) replaced them with recorded row origins read through `document::row_map`. Originally written 2026-09-05 as design, not a commitment, for
**Design A′** (see the design fork below) so the analysis isn't lost before the work is
scheduled. Sibling context: [`docs/dev/editing-model.md`](../editing-model.md) (the
invariant this plan removes), [`docs/dev/input.md`](../input.md),
[`docs/dev/performance.md`](../performance.md).

## Goal

A soft line break inside a paragraph (a lone `\n` — source hard-wrapped for a legible
width) is **not** semantic Markdown: CommonMark collapses it to a space. Today edamame
renders every soft break on its own visual row, so a paragraph wrapped at 80 columns
shows as a stack of ragged short rows instead of reflowing to fill the viewport. This
plan makes prose paragraphs **reflow**: soft breaks become spaces and the paragraph
wraps to the viewport as one flow, in every mode.

Unchanged semantics:

- **A blank line still separates blocks** — already handled by pulldown-cmark; nothing to do.
- **A hard break stays a hard break** — `Inline::HardBreak` (trailing two spaces or a
  backslash) still forces a row split even under reflow. It is semantic; a soft break is not.

## The design fork, and why A′

Three coherent designs were considered for what the cursor's own block looks like in
`Mode::Rendered`, where edamame reveals the cursor's block as raw Markdown so you edit
real source:

- **A′ — line-for-line raw reveal, variable block height.** The rendered (non-cursor)
  form reflows; the cursor's block reveals to its real source lines exactly as today.
  Because the reflowed form and the raw form have different heights, the block's height
  *changes* when the cursor enters/leaves it. The visible jump is mitigated by anchoring
  the cursor's screen row across the transition.
- **B′ — reflowed raw reveal.** The cursor's block shows raw Markdown but reflowed, so
  height stays roughly constant. **Rejected: deceptive** — a reflowed "raw" view is not
  raw; the soft breaks the user put in the source are invisible, so the raw-reveal stops
  telling the truth about the bytes on disk.
- **C′ — de-reflow only the cursor's source line.** Rejected: visually incoherent (one
  line yanked out of a flowing paragraph) and the mixed reflowed+raw mapping is the most
  complex of the three.

**This plan implements A′.** It keeps the raw reveal honest — you always edit the real
source, line for line — at the cost of a reveal-dependent document height, which is the
work described below.

## What A′ actually requires

The load-bearing fact (established 2026-09-05): **today's reveal is height-neutral.** The
reveal loop in `ui::rendered_view` only repaints existing rows in place and never changes
`total_rendered = parsed.lines.len()`. Big-H1 reveals `# Title` on sub-row 0 and *blanks*
the other three rows (`rendered_view.rs:277-296`); mermaid pads post-source rows with NBSP
(`rendered_view.rs:371-378`). This works only because for every non-1:1 block today,
raw-line-count ≤ rendered-row-count, so there is always somewhere to paint the raw lines
and the surplus is padded.

A reflowed paragraph breaks that inequality the other way: 6 short source lines can reflow
to 2 wide rendered rows, so the raw form is *taller* than the rendered form. Height-neutral
padding cannot express that. So A′'s central change is: **the document's visual-row layout
(and therefore scroll, gutter, and mouse mapping) must become reveal-aware** — the cursor's
block contributes its *raw* lines' heights while revealed and its *rendered* (reflowed)
lines' heights otherwise.

Two things stay stable and are *not* touched, which bounds the blast radius:

- **`parsed.lines` and rendered-line indices remain the parse-stable spine.** Reflow changes
  what the renderer emits (fewer lines per paragraph), but `parsed.lines` is still built once
  per parse, cursor-independent, and render-cached. We never make it depend on cursor position.
- **`SourceMap` is block-granular** (`document/source_map.rs`) — it never claims a specific
  rendered row maps to a specific source line, so its whole method surface survives.
- **The `VisualRowCache` layer** (`document/visual_cache.rs`) already maps one rendered line
  to many *visual* rows via wrap; it is agnostic to the 1:1 invariant. A′ layers a reveal
  patch over it rather than replacing it.

## Architecture

### 1. Reflow the renderer output

`Renderer::render_paragraph` (`markdown/renderer.rs:533-566`) currently splits inlines at
`HardBreak | SoftBreak` so every source break gets its own row. Change it: split only at
`HardBreak`; treat `SoftBreak` as a space within a segment and let `line_render`'s existing
word-aware wrap flow the segment. Gate it behind a new `RenderSettings.reflow_paragraphs`
field so the setting participates in the render-cache fingerprint (per
[`editing-model.md`](../editing-model.md): "a new Renderer knob that changes output must go
into `RenderSettings`"). `per_block_own` for a reflowed paragraph then falls out for free —
`ParsedDoc::build` counts the lines the renderer actually produced.

This is the whole of the *rendered* (non-cursor, and all of Preview) behavior. Landed alone
it is the Preview-only reflow, useful on its own and low risk.

### 2. The effective-rows overlay (the crux)

Introduce a per-frame view that answers the viewport's visual-row questions with the reveal
patch applied. Call it `EffectiveRows` (new; likely `editor::effective_rows`), constructed
cheaply each frame from:

- the base `ParsedDoc` rendered caches (`total_visual_rows`, `visual_rows_before`,
  `line_at_visual_row`),
- the revealed block's rendered-line range `[B.start, B.end)`,
- that block's raw source lines and their per-line wrap counts at the current width.

It presents a virtual row sequence: rendered lines from `parsed.lines` everywhere, except the
revealed block's `N` rendered lines are expanded into its `M` raw lines. All arithmetic is a
delta on the base cache — no full rebuild, no clone of `parsed.lines`:

- `total_visual_rows = base_total − Σ(rendered wrap of block rows) + Σ(raw wrap of block rows)`
- `visual_rows_before(row)` — base up to `B.start`; inside/after the block, substitute the raw
  prefix sum.
- `line_at_visual_row(v)` — outside the block, delegate to base and return a rendered-line hit;
  inside, return a **raw-line** hit `(raw_line_idx, sub)`.

When no block is revealed (Preview, or the pre-reveal-delay window), `EffectiveRows` is the
identity over the base cache.

Rewire the callers in `state_viewport.rs` and `state.rs` from `self.parsed.*` to
`self.effective_rows(width).*`:

- `total_visual_rows_for_mode` (`state_viewport.rs:93-103`)
- `rendered_line_at_visual_row` / `char_offset_at_visual_row` (`state_viewport.rs:125-220`)
- `rendered_cursor_visual_row` and `visual_rows_before` (`state.rs:1023-1027`)

Note the existing subtlety this also *fixes*: `rendered_cursor_visual_row` sums *rendered*
wrap for the rows before the cursor (`visual_rows_before`) but takes the cursor's own sub-row
from the *buffer* line (`cursor_sub_line_in_rendered`, `state.rs:1036-1046`). Today that is
only approximately right when a revealed block's earlier raw lines wrap differently from their
rendered forms; with `EffectiveRows` the "rows before" come from raw wrap inside the revealed
block, so it becomes exact.

### 3. Restructure the reveal loop

`RenderedView::render`'s `while vis_y < height` loop (`rendered_view.rs:245-640`) maps each
rendered index `virtual_idx` to one raw line via `sub = virtual_idx - cursor_block_lines.start`.
That identity dies under A′ (M raw ≠ N rendered). Restructure so that when the walk reaches the
cursor block, it emits the block's raw lines as a unit — iterate the `M` raw lines directly,
advancing `vis_y` by each raw line's wrapped height — then resumes at `cursor_block_lines.end`
in rendered space. The setext / mermaid / big-H1 / wrapped-table branches are unaffected (they
already reveal ≤ their rendered height and keep the in-place mapping); only the general prose
branch (`rendered_view.rs:433-510`) moves into the new block-unit path. `first_sub_row` /
`skip_rows` handling must cover a viewport whose top lands mid-way through the revealed block's
raw lines — `EffectiveRows::line_at_visual_row` supplies the `(raw_line, sub)` for that.

### 4. Cursor-row anchoring (the jump mitigation)

The height change fires when `cursor_block_revealed()` (`state_cursor_block.rs:60-74`) toggles —
on entry, after the 120 ms `RAW_REVEAL_DELAY`; on exit, immediately. Both are driven by the
frame timer, not a keypress, so the anchor must be applied wherever the per-frame reveal state
is observed.

Mechanism, applied in the pre-render step next to `ensure_cursor_visible`
(`state_viewport.rs:106-117`):

1. Store `prev_reveal: Option<(block_idx, bool)>` and `prev_cursor_screen_row: usize` on
   `EditorState`, updated every frame.
2. On the frame where the cursor block's reveal state **changed** since last frame, do not run
   normal `ensure_cursor_visible`. Instead re-anchor: recompute the cursor's visual row against
   the *new* `EffectiveRows`, then
   `set_rendered_scroll_for_screen_row(prev_cursor_screen_row, width)`
   (`state.rs:1029-1031` — it already sets `scroll` so the cursor sits at a target row). The
   cursor has not moved (timer-driven), so its screen row is preserved exactly; content above
   the cursor stays put, and only content below the cursor shifts by the height delta.
3. Otherwise run `ensure_cursor_visible` as today.

Edge cases:

- **Near the top of the document**, re-anchoring may want `scroll < 0`; clamp to 0 and let the
  cursor drift down by the shortfall (unavoidable, and mild).
- **Exit is immediate** (no delay), so the un-reveal frame re-anchors symmetrically.
- **A move that changes block in one step** (e.g. a click) resolves as old-block-unreveal then
  new-block-reveal-after-delay; the same per-block transition logic covers it because
  `prev_reveal` carries the block index.

## Consumer rewiring

Beyond the viewport layer, the sub-block mappings that equate a rendered sub-row with a source
line index switch to position projection. The single shared derivation stays single (per
[`editing-model.md`](../editing-model.md): "four callers must agree and must never re-derive"):

| Consumer | File | Change |
|---|---|---|
| `sub_lines_in_block` / `cursor_sub_line_in_block` / `cursor_rendered_line_idx` | `editor/state.rs:1050-1230` | Prose branch stops counting `rendered_before`; the reflowed block is expanded to its raw lines by `EffectiveRows`, so the cursor's rendered row is a raw-line lookup. Table/mermaid/code branches unchanged (they already consult real rendered lines — the pattern being generalized). |
| Gutter `build_source_line_map` / `source_line_at_visual_row` | `editor/state_source_lines.rs` | Inside a reflowed *non-cursor* block, label each reflowed row with the source line of its first character (row-start byte → line). Needs the reflowed-block byte↔row map (below). Continuation rows stay blank as today. |
| Selection/search overlay `paint_byte_range_overlay` | `ui/rendered_view/paint.rs:155-164` | Replace `raw_line_idx = sub_idx_in_block` with a byte-range → `(row, col-span)` query. |
| Mouse `mouse_ops::coord` | `editor/mouse_ops/coord.rs:312-559` | `revealed_cursor_line`, `revealed_raw_row_count`, and raw-line-from-sub-row invert through the reflowed-block map / `EffectiveRows`. |
| Vertical motion `move_up_visual` / `move_down_visual` | `editor/state_cursor_visual.rs:81-157` | Today they wrap a single *buffer line*. Inside a reflowed *rendered* (non-cursor) block the cursor is never present, so motion only meets reflow when crossing *out of* the revealed cursor block into a reflowed neighbor — route the neighbor's wrap through the reflowed-paragraph wrap, not per-buffer-line. This is the subtle-bug epicenter ([`editing-model.md:31`](../editing-model.md)): the wrap used for motion and the wrap used for painting must be the same. |

### The reflowed-block byte↔row map

The gutter, mouse, and overlay need, for a reflowed *rendered* block, a bidirectional map
between a display `(row, col)` and a buffer byte. Build it by composing two existing pieces
across the soft-break-joined paragraph:

- `markdown::inline_col_map::InlineColMap` (`markdown/inline_col_map.rs`) already maps
  raw-char-col ↔ rendered-char-col for one line, collapsing markup and already handling
  `SoftBreak`/`HardBreak` via `push_break`. Extend `build` to accept the paragraph's whole
  multi-line raw source, emitting one rendered space per soft break — exactly the reflow rule.
- `line_render::visual_rows_of_chars` / `sub_line_of_col` map the joined rendered string to
  wrap `(row, col)`.

Compose: `buffer byte → raw col → rendered col → (row, col)` and inverse. Memoize per
`(block value, width)` alongside the render cache. The **soft-break whitespace collapse**
(`text  \n  more` → one rendered space) must live only here — a second copy anywhere
reintroduces the drift class the model doc keeps warning about.

## Phasing

Land incrementally; each phase is independently valuable and testable.

1. **Preview-only reflow.** ✅ **DONE (2026-09-06).** `RenderSettings.reflow_paragraphs` + the
   `render_paragraph` branch (§1). Reflow applies in `Mode::Preview` only (no cursor, no reveal).
   Ships value, front-loads the large-but-mechanical snapshot churn where it is lowest risk. (~0.5 day)

   Implementation notes:
   - `Renderer` grows a `reflow_paragraphs` flag (`with_reflow_paragraphs`), carried into the
     `RenderSettings` fingerprint. `render_paragraph` splits at `HardBreak` only when the flag is
     set; a `SoftBreak` then stays in its segment and `render_inline` renders it as a space (that
     arm already existed), so wrapping falls to `line_render` downstream — the renderer emits one
     *logical* line per hard-break-delimited segment.
   - `ParsedDoc::build_with_overrides` takes a `reflow_paragraphs: bool` (all call sites updated,
     incl. `build`, benches, and tests).
   - **Preview-only gating** lives in `EditorState`, not the renderer, because Preview and Rendered
     share one mode-independent `parsed.lines` spine. `refresh_parsed` passes `mode == Mode::Preview`
     and records it in a new `parsed_reflow` field; `EditorState::sync_reflow_for_mode` (called each
     frame from `App::prepare_viewport`, beside `set_viewport_width`) reparses when the mode crosses
     the Preview boundary. Diff-mode parse passes `false`. This mode gate is temporary scaffolding —
     phase 5 removes it when Rendered reflows too.
   - **No user config toggle** was added: reflow is purely mode-driven for now. A `editor.reflow`
     option (and phase 5's "flip the default") can layer on later without disturbing this wiring.
   - **Zero snapshot churn** in fact: no existing snapshot fixture hard-wraps prose inside a
     paragraph, so reflow is a no-op on all of them. The anticipated churn will land in phase 5
     when Rendered mode (which the editing/UI snapshots exercise) starts reflowing.
   - Tests added: four renderer-level (`markdown::renderer::tests::{soft_breaks_split_rows_by_default,
     reflow_joins_soft_breaks_into_one_flow, reflow_keeps_hard_breaks,
     reflow_emits_one_logical_line_wider_than_viewport}`) and one end-to-end
     (`editor::state::tests::reflow_applies_in_preview_only_and_reparses_on_mode_switch`).
2. **Reflowed-block byte↔row map + proptests.** ✅ **DONE (2026-09-06); later removed as unused.**
   The `ReflowMap` primitive was built and proptested here, but phases 3–5 composed
   `InlineColMap` with each consumer's own wrap layer directly (the wrap always turned out to be
   the caller's), so nothing ever wired `ReflowMap` in. It was deleted rather than left as dead
   code; the notes below describe what it was. Pure function; proptest
   the byte↔`(row,col)` round-trips (multi-line paragraph, markup across a soft break, wrapping
   link text, footnote refs, smart-punctuation contractions at row edges). No wiring yet. (~1–1.5 days)

   Implementation notes:
   - New module `editor::reflow_map` (`src/editor/reflow_map.rs`) with a pure `ReflowMap` type. It
     composes the two existing pieces exactly as `mouse_ops::coord` already does for a wrapped
     table cell: `InlineColMap::build` (raw↔rendered char column, and the **only** place the
     soft-break→space collapse lives — pulldown emits the `SoftBreak` and `push_break` records one
     rendered space) with `line_render::visual_rows_of_chars` + `sub_line_of_col` (rendered column
     → wrapped `(row, col)`). API: `build(raw_source, rendered, width, indent)`,
     `raw_char_to_row_col`, `row_col_to_raw_char`, `rows`/`row_count`.
   - **Scope is one reflowed *logical line*** (the run between hard breaks). A hard break still
     splits a paragraph into separate rendered lines (phase 1), so block-level composition across
     hard-break segments — and the segment-local-char ↔ buffer-byte conversion — is left to the
     phase-3 wiring, which knows the block's byte range. The map works in raw *char* columns (as
     `InlineColMap` does); byte↔char is the caller's seam.
   - **1:1 fallback** mirrors `InlineColMap::raw_to_rendered_checked`: when the collapse map's
     rendered length disagrees with the painted rendered length (multi-char smart punctuation like
     `...`→`…`, or a prefixed line the walker can't see), the map degrades to a 1:1 raw↔rendered
     identity rather than panicking. Tested to still round-trip display cells.
   - **`editor` is the right home**: it already reaches into both `ui::line_render`
     (`state_cursor_visual`) and `markdown::InlineColMap` (`state`, `mouse_ops::coord`), and every
     phase-3 consumer lives in `editor`. Memoization per `(block value, width)` is deferred to the
     wiring phases (noted in the type's doc).
   - **Chosen not to modify `InlineColMap::build`**: the plan suggested "extend `build` to accept
     the whole multi-line raw source", but `build` already re-parses through pulldown and so
     already handles a multi-line paragraph correctly (soft breaks → `SoftBreak` → one space). No
     change was needed; `ReflowMap` just calls it with the segment's whole text.
   - Tests: 6 unit (`plain_multiline_paragraph_joins_and_wraps`, `soft_break_becomes_one_space`,
     `markup_across_a_soft_break_projects_exactly`, `link_text_wraps_and_round_trips`,
     `footnote_reference_across_the_flow_round_trips`,
     `multi_char_smart_punct_falls_back_but_still_round_trips`) + 1 proptest
     (`display_cell_round_trips`, 200 cases: words joined by spaces/soft-breaks, several widths,
     asserting every visible display cell → raw char → same cell). Rendered strings come from the
     real `Renderer` with reflow on, so the tests pin the true composition, not a hand-written
     rendered form.
3. **Gutter + mouse + overlay** ✅ **DONE (2026-09-06).** Onto the map, reveal still
   line-for-line. These read the reflowed *rendered* form and are independent of the reveal
   restructure. (~1 day)

   Implementation notes:
   - The reflowed blocks these consumers meet are the **Preview**-mode paragraphs from phase 1
     (reflow is still Preview-only until phase 5), so this phase fixes live Preview bugs. An
     exploration pass established that **Preview *selection*** (click via `rendered_click_to_line_col`,
     paint via `preview::paint_preview_selection`) already works in rendered coordinates and needed
     no change. The three that broke:
   - **Single shared gate**: `ParsedDoc::is_reflowed_paragraph_at(byte)` (+ a stored
     `reflow_paragraphs` flag), resolved through `real_block_for_byte` so it never indexes the
     blank-line-inflated `blocks` space. All three consumers branch on it, so they agree on which
     blocks lost the 1:1 correspondence.
   - **Gutter** (`state_source_lines::build_source_line_map`): a soft-break-only reflowed paragraph
     has `block_own == 1`, so `sub_lines_in_block`'s existing cap already returns all-zeros — no
     change needed there (the phase-4 `sub_lines_in_block` rewire is for the *revealed cursor*
     block, a different case). The only fix was **first-writer-wins** for reflowed paragraphs so the
     single flow row is labeled with its *first* source line, not (via the usual last-writer rule)
     its last. Folded soft-break lines go unnumbered, exactly like a wrapped long line today.
   - **Mouse** (`coord::rendered_sub_line_to_offset`): a reflow branch resolves the click's wrap
     with the existing `click_to_rendered_char_idx` (cell-width- and wrap-aware), then turns the
     rendered column into a raw char through a **block-wide `InlineColMap`** and adds it to the
     block's start char offset. The wrap layer is the caller's; only the raw↔rendered column layer
     is new.
   - **Overlay** (`rendered_view::paint::paint_byte_range_overlay`): a reflow branch intersects the
     selection with the **whole block** (not one raw line), maps the raw-column span through a
     block-wide `InlineColMap` (`raw_to_rendered_checked`, with a 1:1 fallback), and hands the
     rendered-column span to the unchanged `paint_cols_on_line`, which wraps it.
   - **Key simplification vs. the plan**: phase 2's `ReflowMap` (which owns the wrap composition)
     was *not* needed here — for all three consumers the wrap is already handled by their existing
     callers, so they only needed the joined-block `InlineColMap` layer (which `build` produces for
     multi-line input directly). `ReflowMap` went unused in phase 4 too and was later removed (see phase 2).
   - **Scope**: handles the common soft-break-only paragraph (one logical rendered line). A
     paragraph containing a *hard* break renders as multiple logical lines; those extra segments
     fall through to the existing per-raw-line path (bounded, graceful, rare) — revisit in the
     phase-5 snapshot review.
   - Tests: gutter (`reflowed_paragraph_labels_its_first_source_line`), mouse
     (`coord::tests::click_in_reflowed_paragraph_maps_across_source_lines`), overlay
     (`search::preview_reflowed_paragraph_highlights_match_across_soft_break`, a real `TestBackend`
     render asserting highlighted cells).
4. **`EffectiveRows` + reveal-loop restructure + cursor-row anchoring + `sub_lines_in_block`
   rewire** (§2–4). 🟡 **CORE DONE (2026-09-07); consumer rewirings remain.** The irreversible,
   highest-risk step; done last, on a proven primitive. (~2–2.5 days)

   Everything in this phase is **dark in production**: reflow stays Preview-only until phase 5, so
   the new paths only fire behind the phase-5 switch. A test seam exposes them:
   `EditorState::reflow_in_rendered` (+ `set_reflow_in_rendered`), read through a new
   `EditorState::want_reflow()` that both `refresh_parsed` and `sync_reflow_for_mode` now use in
   place of the hard-coded `mode == Preview`. Off by default; phase 5 flips it and wires it to
   config.

   **Done and tested:**
   - **`EffectiveRows`** (`editor::effective_rows`): the per-frame overlay from §2. Presents the
     document's visual rows with the revealed block's `N` rendered lines replaced by its `M` raw
     lines (each wrapped at the width), as a pure delta over the base `VisualRowCache` — no rebuild.
     `total_visual_rows`, `line_at_visual_row` (→ `RowHit::Rendered`/`Raw`), `raw_line_visual_row`,
     and accessors. Identity unless a *reflowed* cursor block is revealed in Rendered mode.
     4 brute-force tests (identity + reveal, several widths, wrapping raw lines).
   - **`EditorState::effective_rows(width)`**: constructs it, gated on
     `mode == Rendered && cursor_block_revealed() && is_reflowed_paragraph_at(cursor)` (Preview
     never reveals, so it stays identity there).
   - **Arithmetic rewiring (§2)**: `total_visual_rows_for_mode` and `rendered_cursor_visual_row`
     now route through `effective_rows`, so scroll, `ensure_cursor_visible`, and scrollbar sizing
     count the raw expansion. (`cursor_sub_line_in_rendered` already wraps the cursor's *buffer*
     line — its raw line — so it supplies the sub-row unchanged.)
   - **Reveal-loop restructure (§3)**: an additive first branch in `RenderedView::render` emits the
     reflowed cursor block's raw lines stacked (the block is one rendered line, so `virtual_idx`
     then steps straight past it); the loop start is reflow-aware via `effective_rows` so a viewport
     opening mid-block starts on the right raw line. The big-H1/setext/mermaid/wrapped-table
     branches are untouched. TestBackend test: a `**bold**`-bearing soft-broken paragraph reveals
     its raw lines stacked.
   - **Cursor-row anchoring (§4)**: `EditorState::anchor_reflow_reveal`, called per frame from
     `App::prepare_viewport` (modeled on `sync_image_reveal`), holds the cursor's screen row across
     the reveal/un-reveal height change. Gated on `reflow_in_rendered`, so inert in production.
     Test drives the `RAW_REVEAL_DELAY` toggle and asserts the screen row is held.

   **Remaining (all dark until phase 5, all mechanical applications of `EffectiveRows`, so folded
   into phase 5 when Rendered reflow is enabled and can be validated visually/interactively):**
   - **Gutter** `source_line_at_visual_row` → route through `effective_rows` so a revealed reflowed
     block numbers each raw line (a `RowHit::Raw` → block's first source line + `raw_line`).
   - **`char_offset_at_visual_row`** → `RowHit::Raw` should resolve to that raw line's start byte
     (scroll-/click-to-visual-row precision inside the revealed block).
   - **Mouse** `coord::rendered_sub_line_to_offset` + `walk_rendered_rows` → make the two-layer walk
     count the revealed block's raw rows (the intricate one; the plan's `revealed_cursor_line` /
     `revealed_raw_row_count` row). Clicks *inside* a revealed reflowed block in Rendered mode.
   - **`sub_lines_in_block` prose branch**: `rendered_cursor_visual_row` was made exact by going
     through `effective_rows` directly, bypassing it; `cursor_rendered_line_idx` (which still feeds
     it) is fine for the single-rendered-line reflowed block, but the prose branch should still be
     reconciled with the reveal expansion for completeness.
   - **Perf**: `effective_rows` calls `raw_block_cursor` (one alloc) when revealed; the per-visual-
     row consumers (gutter) should hoist one `effective_rows` per frame rather than build per row.
5. **Enable reflow in `Mode::Rendered`, flip the default, regenerate snapshots, rewrite the
   `editing-model.md` "rendered rows stay 1:1" bullet and the user `docs/editing.md`.**
   ✅ **DONE (2026-09-07).** (~0.5–1 day)

   Implementation notes:
   - **Config + default flip**: new `config.editor.reflow` (default `true`), documented in the
     annotated `config/config.toml`. The phase-4 `reflow_in_rendered` seam became the master
     `EditorState::reflow` flag (default on), gating both Preview and Rendered via `want_reflow`;
     `set_reflow` + `configure_new_editor` wire the config in, and a **Reflow paragraphs** settings-
     overlay toggle (live-updated via `apply_live_update`) lets users turn it off.
   - **Finished the phase-4 consumer rewirings** (were dark, now live): the gutter
     (`source_line_at_visual_row`), `char_offset_at_visual_row`, and the mouse walk
     (`revealed_raw_row_count` totals the stacked raw lines; `rendered_sub_line_to_offset` gained a
     revealed-stack branch) all route the revealed block through `EffectiveRows`. `sub_lines_in_block`
     itself was **not** rewired — `rendered_cursor_visual_row` now goes through `EffectiveRows`
     directly, and `cursor_rendered_line_idx` is correct as-is for a single-rendered-line reflowed
     block — so the "single derivation" stays single.
   - **The trailing-blank fix**: a paragraph's byte range absorbs the blank line after it, which
     owns its own rendered row, so the reveal uses `revealed_source_lines` (trailing blanks
     stripped) everywhere it stacks/counts raw lines — `EffectiveRows`, the reveal loop, and the two
     mouse paths agree, and a degenerate one-line paragraph stays height-neutral.
   - **Scope decision (answers the open question)**: reflow applies to **top-level paragraphs
     only** — list-item and blockquote paragraphs do *not* reflow yet, because the consumers key on
     a top-level `Block::Paragraph`. Threaded via a `top_level` flag on `render_block`. Extending to
     nested paragraphs (with consumer support) is future work.
   - **Snapshot churn**: none. No committed snapshot fixture hard-wraps prose, so turning Rendered
     reflow on changed no `.snap`. The churn instead surfaced as a handful of Rendered-mode
     behavior tests written against the 1:1 model; each was updated — either to the reflowed
     expectation or, where the test's subject is orthogonal to reflow (scroll-on-wrap, the
     per-source-line overlay/gutter paths), by disabling reflow in that one test (those paths stay
     live for reflow-off and hard-break/list/blockquote paragraphs).
   - Docs: `editing-model.md` 1:1 bullet amended + a new "Prose reflow …" bullet; user
     `docs/editing.md` gained a "Paragraphs reflow …" note; `config/config.toml` documents `reflow`.

   **Known follow-ups** (small, non-blocking): list/blockquote paragraph reflow; hoist the per-frame
   `effective_rows` build for the per-visual-row gutter consumer (it calls `raw_block_cursor` when a
   block is revealed); a viewport scrolled to *start inside* a revealed reflowed block uses the base
   (not effective) start in `walk_rendered_rows` (mouse) — a rare edge, the paint is authoritative.

Rough total: **6–7 focused days**, risk concentrated in phase 4.

## Testing

- **Effective-rows arithmetic**: unit tests that `total_visual_rows`, `visual_rows_before`,
  and `line_at_visual_row` agree with a brute-force expansion, revealed and not, at several
  widths.
- **Anchoring**: `TestBackend` cases toggling reveal (drive the delay timer) and asserting the
  cursor's screen row is unchanged across the transition, including at the top-of-document clamp.
- **Cursor + click round-trips** in a reflowed block: place cursor / click at a screen cell →
  byte → back, for multi-line paragraphs, mid-word wrap, markup spanning a soft break
  (`**bold\nacross**`), a wrapping link, and a hard break (must still split).
- **`--no-fail-fast`** as always; regenerate snapshots with `cargo insta review` and commit them
  with the code.

## Risks and open questions

- **Perf**: `EffectiveRows` must stay allocation-light per frame (delta arithmetic over the base
  cache, only the revealed block's raw wrap computed). Verify against the frame budget in
  [`performance.md`](../performance.md); the revealed block is small, so this should be cheap.
- **The jump is mitigated, not eliminated**: content *below* the cursor still shifts on
  reveal/un-reveal. Anchoring keeps the cursor and everything above it still. If the residual
  shift proves distracting, a short animation or a reveal-on-any-motion (no delay) could be
  considered, but that is out of scope here.
- **Very long paragraphs**: a paragraph whose raw form is taller than the viewport, revealed
  with the cursor near its end, must scroll correctly — covered by `EffectiveRows::line_at_visual_row`
  handling a viewport top that lands inside the revealed block.
- **Open question for review**: should reflow also apply to soft breaks inside a list item's
  paragraph and inside a blockquote? Both flow through the same inline path; the plan assumes
  yes, but the snapshot review in phase 5 is where that is confirmed visually.
