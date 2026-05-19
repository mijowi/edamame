# Fix selection highlight on formatted lines

## Context

On a rendered line containing inline markup (`**bold**`, `*italic*`, `_under_`,
`~~strike~~`, `==highlight==`, `` `code` ``, `[text](url)`, images), the
renderer collapses the markup characters but the selection-highlight painter
maps raw byte columns to rendered cell columns **1:1** for the common
non-list / non-table case.  Result: while the line is still rendered, the
selection highlight is drawn at the wrong cells; when the line de-renders
(jitter-suppression reveal), the highlight visibly snaps to its correct
position.  The final selection range is correct — only the *visible* range
during render is wrong.

The user also observed the cursor appearing to shift on de-render: same root
cause.  We are **not** decoupling the cursor from the selection's active end —
that would break shift+arrow extension and click-drag-extend, both of which
rely on the cursor-at-active-end model.  Fixing the column map fixes both
visible symptoms.

A working per-line rendered↔raw character map already exists in
`src/editor/mouse_ops/coord.rs::rendered_to_raw_char_map` (lines 688-804).
It re-parses the line via `pulldown_cmark::Parser::into_offset_iter()` and
returns `Vec<usize>` of raw-char-index per rendered-char-index.  Today it is
called only at click time and is recomputed on every click.  The fix is to:

1. promote it to a shared utility that exposes both directions,
2. cache the per-raw-line map on `ParsedDoc` (already invalidated on every
   parse), and
3. use it in the selection painter's fallback path.

## Approach

### Step 1 — Extract `InlineColMap` into a small module

Create `src/markdown/inline_col_map.rs` (re-export from `src/markdown.rs`):

```rust
pub struct InlineColMap {
    rendered_to_raw: Vec<usize>, // raw char idx per rendered char idx
    raw_to_rendered: Vec<usize>, // rendered char idx per raw char idx
    rendered_len: usize,
    raw_len: usize,
}

impl InlineColMap {
    pub fn build(raw_line: &str) -> Self { /* moved logic */ }
    pub fn rendered_to_raw(&self, rendered_char: usize) -> usize;
    pub fn raw_to_rendered(&self, raw_char: usize) -> usize;
}
```

Move the body of `rendered_to_raw_char_map` (and its private `CharMapWalk`
helper) here.  Build the inverse map in the same walk — the existing walker
already emits the rendered-char-idx → raw-char-idx pairs needed to populate
both vectors.

Out-of-range queries clamp to the end (`raw_len` / `rendered_len`) — the same
clamping the current callers expect.

### Step 2 — Cache one `InlineColMap` per raw line on `ParsedDoc`

In `src/document/parsed_doc.rs`, add `inline_maps: Vec<OnceCell<InlineColMap>>`
(or equivalent lazy structure), one entry per raw line, populated alongside the
existing per-line state on `ParsedDoc::build`.

Expose `ParsedDoc::inline_map(raw_line_idx) -> &InlineColMap`.  Lazy
construction is fine — a frame may paint selection over a handful of lines, not
the whole doc.  Cache lives only as long as the current parse; it is dropped
and rebuilt when `ParsedDoc` re-parses on edit.

### Step 3 — Use the cached map in the click path

In `src/editor/mouse_ops/coord.rs`:
- Replace the call to local `rendered_to_raw_char_map(...)` inside
  `non_table_click_to_raw_col` (~line 484) with
  `state.parsed.inline_map(raw_line_idx).rendered_to_raw(...)`.
- Delete the now-unused local `rendered_to_raw_char_map` and `CharMapWalk`.

This removes the per-click re-parse work as a bonus.

### Step 4 — Use the cached map in the selection painter (the actual fix)

In `src/ui/rendered_view/paint.rs::paint_selection_overlay`
(lines 200-253), replace the **1:1 fallback** branch with a call into the
cached map:

```rust
let map = state.parsed.inline_map(raw_line_idx);
let sel_rendered_start = map.raw_to_rendered(sel_raw_start_char);
let sel_rendered_end   = map.raw_to_rendered(sel_raw_end_char);
```

The list and table branches stay as-is (their helpers already account for
their own prefix offsets); the generic fallback is the one that was 1:1.

If `paint_preview_selection` in `src/ui/preview.rs` also hits the same kind of
formatted line (selections in Preview mode), apply the same substitution
there.  Verify during implementation; if preview selection is stored in
*rendered* coordinates (`VisualSelection { (line, col) }` per exploration),
preview may already be correct and need no change.

### Step 5 — Decide on cursor-column rendering on formatted lines

While a formatted line is still rendered (before `RAW_REVEAL_DELAY` elapses),
the cursor indicator drawn by `RenderedView` at `(cursor_col, cursor_row)`
should also use `raw_to_rendered(cursor_raw_col)` so it sits at the correct
visual cell.  This kills the second symptom (cursor "shifting" on de-render).
Touchpoint: wherever `RenderedView` computes the cursor indicator column
during the reveal-delay window — confirm in `src/ui/rendered_view/` and apply
the same map.

### Step 6 — Tests

Unit tests in `src/markdown/inline_col_map.rs`:
- `**Bold text**` — rendered "Bold text" (9 chars), raw 13 chars; round-trip
  endpoints and the boundary just after the `**`.
- `*Italic*`, `_under_`, `~~strike~~`, `==hi==` — each delimiter family.
- `**_Bold and italic_**` — nested.
- `` `code` `` — backtick handling (renderer adds space padding: `" code "`).
- `[text](url)` — bracket + url collapse to just `text`.
- A line mixing all of the above (the example from the user's report).

Integration test in `tests/mouse.rs` (or a new `tests/selection.rs`):
- Drag-select across `**Bold text** | *Italic*` in Rendered mode; assert that
  `Selection` byte range AND the painted rendered-cell range agree by
  rendering the view with `TestBackend` and inspecting cells under the
  selection style.

Manual smoke: open a doc with the report's example line, drag a selection
across it in Rendered mode (cursor not yet on the line so it stays
rendered), confirm the highlight matches the dragged cells, then move the
cursor onto the line and confirm there is no visible snap on de-render.

## Critical files

- `src/markdown/inline_col_map.rs` *(new — extracted from `coord.rs`)*
- `src/markdown.rs` — add `pub mod inline_col_map; pub use ...;`
- `src/document/parsed_doc.rs` — add per-line lazy cache + accessor
- `src/editor/mouse_ops/coord.rs` — use cached map; delete local copy
- `src/ui/rendered_view/paint.rs::paint_selection_overlay` — replace 1:1
  fallback with cached map
- `src/ui/rendered_view/*` — cursor indicator during reveal-delay (Step 5)
- `src/ui/preview.rs::paint_preview_selection` — verify, fix if needed
- `tests/selection.rs` *(new)* or `tests/mouse.rs` — integration coverage

## Out of scope

- Decoupling cursor from selection active end (rejected — see Context).
- AST redesign to carry source byte ranges on every `Inline` node — the
  `OffsetIter` re-parse per line is cheap relative to a frame and lets us
  keep the AST clean.
- Reference-style links / autolinks correctness (separate Phase 8 concern
  per CLAUDE.md).

## Verification

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test --lib inline_col_map
cargo test --test mouse           # or tests/selection.rs if added
cargo test                        # full sweep, including snapshots
```

Then manually:

```bash
cargo run -- demo.md   # demo.md containing the user's example line
```

Drag-select across the formatted line in Rendered mode with the cursor
parked on a different line.  The highlight should hug the visible glyphs
exactly.  Move the cursor onto the line and watch for any visible jump as
the line de-renders — there should be none.
