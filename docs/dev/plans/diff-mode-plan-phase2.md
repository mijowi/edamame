# Diff Mode Plan Phase 2 — Hybrid rendered diff view (future)

This section is design notes for the follow-up phase, not a
commitment. It captures the analysis so the trail isn't lost between
Phase 1 shipping and Phase 2 starting. A full design doc will be
written when Phase 2 is scheduled.

### Goal

In Phase 1, both sides of every hunk render as raw markdown source. In
Phase 2, each hunk renders as **rendered markdown** on both sides:
tables show as grids, headings show with their hierarchy styling, code
fences show with their fenced highlighting, lists show indented. The
user gets a true side-by-side rendered comparison. Raw substitution
only kicks in for the focused new-side block when the user enters Edit
sub-mode, mirroring the existing `RenderedView` hybrid behavior.

### Architecture sketch

- **Two `ParsedDoc`s** on `DiffState`: `parsed_old: ParsedDoc` parsed
  from `old_rope`, `parsed_new: ParsedDoc` parsed from `new_rope`.
  Both refresh via the existing deferred-reparse machinery; `new` is
  the one that updates during in-diff edits.
- **Block-aligned hunks.** `similar` still produces line-level hunks
  initially. A post-process pass **snaps each hunk's boundaries to
  block boundaries on both sides**: find the enclosing block on the
  old side, find the enclosing block on the new side, expand the
  hunk to cover both blocks whole. Net result is that every hunk
  corresponds to a (zero-or-more old blocks, zero-or-more new blocks)
  pair, and rendering each side is just "render those blocks via
  `Renderer`."
  - Edge case: blocks don't always align across the two parses. Adding
    `#` to a paragraph line splits one block into two on the new side.
    The snap algorithm must handle 1-old-block ↔ N-new-blocks and
    N-old-blocks ↔ 1-new-block fan-outs cleanly.
- **`DiffView` rendering loop.** For each visible visual row, walk in
  order: unchanged region (lines borrowed from `parsed_new`), then for
  each hunk emit old-side rendered lines with `diff_delete_line` bg,
  then new-side rendered lines with `diff_add_line` bg.
- **Edit sub-mode hybrid.** When the focused hunk is in Edit, the
  focused new-side block(s) get raw substitution exactly like
  `RenderedView`'s cursor-block path. The same `RAW_REVEAL_DELAY` (120
  ms) applies. Old side remains rendered.
- **Inline word highlighting** is preserved only for **text-only
  blocks** (paragraph, heading, blockquote). For these blocks,
  `Renderer` gains a source-byte-to-render-char index map per block;
  the inline word-diff result is then mapped through that index to
  produce per-span bg overrides on the rendered `Line`. For
  non-text-only blocks (table, code, list, etc.), inline highlighting
  is dropped — the user reads the structural diff at the row / cell /
  line level instead (see below).

### Row-level table diffing (rendered view)

Row-level table diffing is implemented in Phase 1 at the raw-line
level (§3a). Phase 2 upgrades the *rendering* of those per-row hunks
to use the table grid view:

1. When the block-snap algorithm encounters two table blocks at the
   same hunk position, instead of treating them as a monolithic
   replace, it **enters a sub-diff** over the rows.
2. Run `similar::TextDiff::from_iter` over `old_table.rows` vs
   `new_table.rows`, where each row's cells are concatenated for
   the diff key (e.g. `"cell1|cell2|cell3"`).
3. Render the **shared header and shared rows** once (no diff
   coloring); render **changed rows** stacked with old-row above
   (`diff_delete_line` bg) and new-row below (`diff_add_line` bg);
   render **inserted rows** with `diff_add_line` bg only; render
   **deleted rows** with `diff_delete_line` bg only.
4. Each changed/added/deleted row becomes its own `Hunk` for
   decision purposes — the user accepts/rejects per row rather than
   per table. (`Decision` granularity stays the same; we just create
   more, smaller hunks at table-diff time.)
5. **Within a changed row**, run a cell-level diff: cells that differ
   get the inline-changed bg (`diff_add_inline` / `diff_delete_inline`),
   identical cells render normally. Cell content that's text-only can
   further be diffed at the word level inside the cell.

This pattern generalizes:

- **List items** can use the same "rows of the same block, sub-diffed
  via `similar`" pattern. Each list item becomes a sub-hunk.
- **Code fences** can sub-diff lines within the fence (similar's
  default behavior, but rendered with the code-block style retained
  on unchanged lines).
- **Definition lists** (if/when supported) would sub-diff term/def
  pairs.

The general abstraction is: a block type can opt into a `sub_diff`
strategy that returns a `Vec<SubHunk>` instead of being treated
monolithically. The diff engine asks each pair-of-blocks for its
sub-diff, defaulting to "treat as one hunk" for blocks that don't
implement it.

### Risks / unknowns to investigate at Phase 2 design time

1. **Renderer source-to-render index map.** `Renderer` doesn't expose
   this today. The cleanest approach is probably to emit, alongside
   each rendered `Line`, a `Vec<(source_byte, render_char_idx)>`
   checkpoint list per block. Cost: small per-block allocation;
   benefit: precise inline highlighting.
2. **Block-snap algorithm correctness.** N-to-M fan-outs are subtle
   when both sides re-block (e.g. an entire section restructures).
   Worth writing the algorithm against a snapshot test corpus before
   wiring it into the view.
3. **Table-row diffing with merged cells, multi-line cells, or
   row-spanning constructs.** Standard markdown tables don't allow
   row spans, but GFM extensions sometimes do; we need to confirm
   the parser's behavior and decide whether to fall back to
   monolithic diff for un-rowsplittable tables.
4. **Cursor placement when Edit sub-mode focuses a rendered block.**
   The existing `RenderedView` swaps to raw on cursor entry; in diff
   mode that's still the right behavior, but the *old* side stays
   rendered the whole time, which means there's no symmetry between
   what the user sees on the two sides during editing. Worth a UX
   review.
5. **Performance.** Two `ParsedDoc`s and a per-block sub-diff over
   tables doubles parse cost and adds table-cell-diff cost. Both
   should still be sub-frame for typical markdown but should be
   measured on `docs/plan.md` and a synthetic 100-row table.
6. **Config gating.** Phase 2 may want a config toggle
   (`[editor] diff_render_mode = "raw" | "rendered"`) so users on
   small terminals or minimal-color setups can stay on Phase 1's raw
   view. Default would be `"rendered"` once Phase 2 is solid.

### Out of scope for Phase 2

- **Semantic diff** (e.g. detecting a moved block as a "move" rather
  than a delete+insert). `similar` doesn't do this; doable as a
  post-process but adds a lot of complexity for marginal benefit.
- **Three-way merge** (e.g. simultaneous on-disk edits and in-buffer
  edits during a diff review). Phase 1's "single newer event
  overwrites" model is preserved.
- **Visual diff of images, mermaid diagrams, or other embedded media.**
  Treated as opaque-block-equality (same source bytes ⇒ same content,
  diff-rendered as whole-block stacked).
