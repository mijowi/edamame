# Nested paragraph reflow — extending reflow to list items, blockquotes, and beyond

Status: **SUPERSEDED (2026-10-06)** by [`row-provenance.md`](row-provenance.md) except for its
consumer work. §A (nested ranges) landed as row-provenance Phase 1, and §B–§C (per-line metadata,
prefix-aware mapping) as Phases 2–4, generalized beyond reflow. What remains — `Flow` origins for
list-item, quote and footnote paragraphs, `EffectiveRows` splicing one flow inside a multi-row
block, the reveal re-drawing container chrome on stacked raw lines, and the battery in
[Testing](#testing) — is row-provenance Phase 7, which deletes this doc when it lands.

Original status: **DESIGN (2026-09-08).** Not yet scheduled. Follow-on to
[`paragraph-reflow.md`](paragraph-reflow.md), which landed reflow for **top-level paragraphs only**
(phase 5, 2026-09-07) and deferred nested blocks as future work. This doc records why the deferral
was principled and what a correct extension actually requires, so the analysis exists before the
work is scheduled. Sibling context: [`editing-model.md`](../editing-model.md) (the invariant reflow
removes), [`input.md`](../input.md) (mouse mapping), [`blockquotes.md`](../blockquotes.md) and the
list rendering path.

## Goal

Make a soft line break inside a **nested** paragraph reflow the same way a top-level paragraph
already does: a lone `\n` becomes a space and the paragraph wraps to the available width as one
flow. The nesting cases, in rough order of value and difficulty:

- **Blockquote paragraphs** — a `> ` (rendered `▎ `) prefix, uniform on every line.
- **List-item paragraphs** — a marker leader (`  1.  ` / `  •  `) on the first line and a
  continuation indent on the rest; loose vs. tight items; task-list checkboxes.
- **Nested combinations** — a paragraph inside a list inside a blockquote, whose prefix is the
  composition of the enclosing prefixes.
- **Footnote-definition bodies** — same shape as a list item (a marker plus a hanging indent).

Semantics unchanged from the top-level case: a **blank line still separates blocks**, and a **hard
break** (`Inline::HardBreak`) still forces a row split. Only soft breaks collapse.

## Why this is not a one-line change

Flipping the renderer's `top_level` flag so nested paragraphs *render* reflowed is trivial and
tempting — and wrong on its own. The rendering would look right while every rendered→source
consumer silently mismaps inside those blocks: clicks land on the wrong byte, the gutter mislabels,
selection paints the wrong cells, and the raw reveal breaks. That is strictly worse than the current
"nested paragraphs don't reflow." The reason is two load-bearing assumptions that hold **only** for
top-level paragraphs today, plus a third mapping gap:

### 1. Byte ranges exist only at depth zero

`markdown::parse_offsets` is a **depth-0** `RangeTracker`: it consumes pulldown-cmark's
`(Event, byte_range)` offset iterator but records a range only while `depth == 0`. The AST
(`markdown/ast.rs`) carries **no byte spans on nested `Block`s** — a `Block::Paragraph` inside a
`Block::List` or `Block::BlockQuote` has no buffer range anywhere in the pipeline. Every reflow
consumer maps rendered↔source *through* `SourceMap`, which is built from those depth-0 ranges and is
therefore **top-level-block-granular**: `real_block_for_byte` / `block_for_byte` resolve a byte to
the enclosing `List` or `BlockQuote`, never to the inner paragraph.

The single reflow gate encodes exactly this:

```rust
// document/parsed_doc.rs
pub fn is_reflowed_paragraph_at(&self, byte: usize) -> bool {
    self.reflow_paragraphs
        && matches!(self.real_block_for_byte(byte), Some(Block::Paragraph { .. }))  // top-level only
        && { let r = self.source_map.rendered_lines_for_byte(byte); r.end - r.start == 1 }
}
```

A nested paragraph fails the `matches!` (its real block is the `List`/`BlockQuote`), so it never
enters any reflow path — by construction.

**Good news, established while writing this doc:** pulldown-cmark's offset iterator already carries
byte ranges for nested events; the depth-0 filter *discards* them by choice, not necessity. So the
nested ranges are recoverable without reparsing — the work is plumbing, not new parsing.

### 2. "A reflowed block is exactly one rendered line"

The consumers rely on a reflowed paragraph collapsing to a **single** rendered logical line — hence
the `r.end - r.start == 1` check above, `EffectiveRows` splicing one rendered line's worth of rows,
and the mouse/overlay treating the block as one flow. A `List` with three reflowed items is **one
`Block::List` producing three rendered lines**; the single-line gate doesn't fit, and neither does
the "one block ↔ one flow line" arithmetic. Nested reflow is inherently *many* reflowed logical
lines under one top-level block.

### 3. Rendered rows carry a prefix the column map doesn't model

`markdown::inline_col_map::InlineColMap` maps raw-char-col ↔ rendered-char-col for a paragraph's
*inline content*. A nested rendered row is `prefix + content`:

- blockquote: `▎ ` (2 cells), uniform per line (`render_blockquote`);
- list: a leader `  1.  ` on the first line, a continuation indent of the leader's width on wraps
  (`render_list_item`); task items add a checkbox.

So the mapping a consumer needs is `rendered (row, col) → strip prefix → InlineColMap → raw col →
buffer byte`, and the prefix width is **row-dependent** (leader vs. continuation) and
**nesting-dependent** (composed prefixes). None of the consumers model a prefix today because a
top-level paragraph has none.

### Consequence

Correct nested reflow needs, together: (a) byte ranges for nested paragraphs, (b) a mapping keyed
per **rendered line** rather than per top-level block, and (c) prefix-aware column translation.
There is no shortcut that delivers correct interaction without all three. This is the "phase-4
epicenter" the parent plan warned about, now multiplied across block types.

## Proposed architecture

The organizing idea: **stop reconstructing the reflow mapping from block lookups; have the renderer
emit it per rendered line, at the moment it reflows.** The renderer is the only component that knows,
for a given output row, which buffer bytes produced it and what prefix it prepended. Recording that
as it emits removes every downstream re-derivation — the exact drift class
[`editing-model.md`](../editing-model.md) keeps warning about ("four callers must agree and must
never re-derive").

### A. Nested byte ranges (parser)

Extend the offset scan to record paragraph ranges at `depth > 0`, not just rules/leaves at depth 0.
Two viable shapes; recommend the second:

- **A1 — widen `RangeTracker`.** Have it keep nested `Paragraph` ranges alongside the depth-0 set.
  Rejected: it conflates two different index spaces (the depth-0 spine the `SourceMap` is built on,
  and the nested overlay) into one structure whose invariants then serve two masters.
- **A2 — a parallel `paragraph_ranges(source) -> Vec<Range<usize>>` scan** (recommend). A second,
  independent walk of the offset iterator that records **every** `Paragraph` open/close range at any
  depth, in document order. Independent of the `SourceMap` spine, so it can't perturb it. Cheap (one
  more pass over events already produced) and trivially testable against fixtures.

These raw ranges are the seam between "a rendered reflowed row" and "the buffer bytes it covers."

### B. Per-rendered-line reflow metadata (renderer → `ParsedDoc`)

Today `render_with_counts` returns `Vec<Line>` plus per-block line counts. Extend the reflow branch
of `render_paragraph` to also emit, for each reflowed logical line it produces, a small record:

```rust
struct ReflowedLine {
    rendered_line: usize,       // index into parsed.lines
    source: Range<usize>,       // buffer byte range of this logical line's raw source
    prefix_cols: usize,         // cells before content on the FIRST row (leader / bar)
    cont_cols: usize,           // cells of indent on CONTINUATION (wrapped) rows
}
```

Store `Vec<ReflowedLine>` on `ParsedDoc` (parse-stable, render-cached like `parsed.lines`). The
renderer already threads `indent_prefix` and now `top_level`; it also needs the current paragraph's
byte range, supplied from the §A scan by position (the *k*-th paragraph the renderer visits is the
*k*-th range — paragraphs are emitted in document order, so a running index aligns them; a debug
assertion can pin the alignment).

This table **replaces** `is_reflowed_paragraph_at` as the gate: a rendered line is "reflowed" iff it
appears in the table, and the record carries everything the consumers need (byte range + prefix
widths). The top-level case becomes the degenerate record `prefix_cols == cont_cols == 0` — so the
existing top-level paths collapse into the general one rather than staying a parallel branch.

### C. Prefix-aware column mapping

Generalize the raw↔rendered translation the consumers share into one helper keyed by a
`ReflowedLine`:

```
rendered (row, col)
  → subtract prefix (prefix_cols on row 0, else cont_cols)
  → InlineColMap over the record's raw source (soft breaks → spaces, as today)
  → raw char col
  → buffer byte via the record's `source.start`
```

and its inverse for painting/cursor placement. The soft-break whitespace collapse stays **only** in
`InlineColMap` (one owner, per the parent plan). This helper is the single derivation all five
consumers call; it lives where the top-level composition lives today (`editor`, reachable from both
`ui::line_render` and `markdown::InlineColMap`).

## Consumer rewiring

Each consumer moves from "top-level `Block::Paragraph`, one flow line" to "any rendered line present
in the `ReflowedLine` table," via the §C helper. The single shared derivation stays single.

| Consumer | File | Change |
|---|---|---|
| Gate | `document/parsed_doc.rs` | `is_reflowed_paragraph_at(byte)` → `reflowed_line_at(rendered_idx) -> Option<&ReflowedLine>`. Byte-keyed callers look up via `rendered_lines_for_byte` then the table. |
| `EffectiveRows` | `editor/effective_rows.rs` | Already width- and range-driven; feed it the revealed paragraph's `source` range and prefix from the record instead of `raw_block_cursor` + a top-level assumption. The reveal can now span *one nested paragraph within* a multi-line block, so the spliced rendered range is a single line inside the `List`/`BlockQuote`, not the whole block. |
| Reveal loop | `ui/rendered_view.rs` | The stacked-raw branch keys on the record, and each stacked raw line is painted **with its prefix** (bar / leader / continuation indent) so the revealed source lines still read as quoted / listed. This is the largest new surface — the raw reveal of a nested paragraph must reproduce the enclosing chrome. |
| Gutter | `editor/state_source_lines.rs` | `source_line_at_visual_row` already routes through `EffectiveRows`; the first-writer-wins label uses the record's `source.start` line. Continuation rows stay unnumbered. |
| Mouse | `editor/mouse_ops/coord.rs` | Both the flow branch and the revealed-stack branch subtract the record's prefix before the column map; the raw byte comes from `source.start`, not the block start. |
| Overlay | `ui/rendered_view/paint.rs` | Intersect the selection with the record's `source` range (not the whole block), map through §C, offset the painted columns by the prefix. |

`char_offset_at_visual_row` (`state_viewport.rs`) and `rendered_cursor_visual_row` (`state.rs`)
already speak `RowHit::Raw/Rendered`; they need the raw-line→byte resolution to use the record's
range rather than the cursor block's.

## Phasing

Each phase is independently valuable and testable; land incrementally, snapshots with the code.

1. **Nested range scan (§A2) + proptests.** Pure function; no wiring. Proptest that every recorded
   range is a valid char-boundary slice and that ranges nest correctly (a child's range ⊂ its
   parent's). (~0.5 day)
2. **`ReflowedLine` table (§B) + the §C helper, top-level rewired onto them.** No new behavior:
   prove the generalized path reproduces today's top-level reflow exactly (all existing reflow tests
   green, zero snapshot churn), with the table degenerate-prefix for top-level. De-risks the
   generalization before any nested block reflows. (~1–1.5 days)
3. **Blockquotes.** Uniform `▎ ` prefix — the simplest nesting. Enable reflow for blockquote
   paragraphs; the reveal loop learns to re-prefix stacked raw lines with the bar. Full mouse /
   gutter / selection / reveal round-trip tests. (~1 day)
4. **List items.** Leader + continuation indent + task checkboxes + loose/tight spacing. The
   row-dependent prefix (`prefix_cols` vs `cont_cols`) earns its keep here. Footnote-definition
   bodies fall out (same shape). (~1.5–2 days)
5. **Nested combinations + docs.** Composed prefixes (list-in-quote); confirm the prefix widths
   compose. Update `editing-model.md`, `docs/editing.md`, and the config note (reflow now covers
   nested prose). (~0.5 day)

Rough total: **4.5–6 focused days**, risk concentrated in phases 3–4 (the reveal re-prefixing and
the row-dependent list prefix).

## Testing

- **Range scan**: proptest char-boundary validity and parent/child nesting on generated
  list/quote/nested fixtures.
- **Generalization (phase 2)**: the entire existing top-level reflow suite must pass unchanged,
  asserting the degenerate-prefix record reproduces today's behavior bit for bit.
- **Round-trips per nesting level**: cursor / click at a screen cell → byte → back, inside a
  reflowed blockquote and list paragraph, across a soft break, mid-word wrap, and with markup
  spanning a soft break — the parent plan's battery, re-run per prefix shape.
- **Reveal re-prefixing**: `TestBackend` cases asserting a revealed blockquote paragraph's stacked
  raw lines each still carry `▎ `, and a revealed list paragraph carries its leader on line 0 and
  the continuation indent after.
- **Gutter**: a reflowed nested paragraph numbers its first source line only; continuation and
  folded soft-break lines stay unnumbered.
- `--no-fail-fast` throughout; `cargo insta review` for any snapshot changes (expect churn in phases
  3–4 where nested fixtures first reflow).

## Risks and open questions

- **Reveal chrome is the sharp edge.** Reproducing the bar / leader / continuation indent on
  *revealed raw* lines is new work with no top-level precedent — the top-level reveal has no prefix.
  If re-prefixing proves fiddly, an intermediate option is to reveal a nested paragraph *without* its
  chrome (plain raw lines, no bar/leader) — honest about the bytes, but visually detached from its
  container. Recommend against; note it as a fallback.
- **Prefix-width source of truth.** `prefix_cols`/`cont_cols` must be measured the *same* way the
  renderer pads (cells, not chars — wide glyphs and the list leader's alignment). Deriving them in
  the renderer at emit time (§B) rather than recomputing avoids a second measurement drifting from
  the first — the whole point of the emit-time table.
- **Paragraph↔range alignment.** §B aligns the renderer's *k*-th visited paragraph with the *k*-th
  scanned range positionally. Frontmatter, HTML comments (zero-line blocks), and promoted image
  blocks must not desync the count — the scan and the renderer must agree on what counts as a
  paragraph. A debug assertion comparing the record's rendered content against `parsed.lines` at
  wire-up catches a desync early.
- **Do lazy `InlineColMap`s still fit?** `ParsedDoc` caches per-*buffer-line* inline maps; a nested
  reflowed paragraph wants a per-*paragraph* map (multi-line). The parent plan built these ad hoc in
  the consumers; consider memoizing per `(paragraph range, width)` alongside the render cache if the
  per-frame build shows up in the frame budget (see [`performance.md`](../performance.md)).
- **Scope creep to tables / code.** Tables and code blocks are deliberately *not* prose and must
  never reflow; the `ReflowedLine` table only ever holds `Block::Paragraph` output, which keeps them
  out by construction.
