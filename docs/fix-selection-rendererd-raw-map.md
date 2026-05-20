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

### What already exists

A working per-line rendered↔raw character map already exists in
`src/editor/mouse_ops/coord.rs`:

- `rendered_to_raw_char_map(raw_line)` at `coord.rs:688` — returns
  `Vec<usize>` of raw-char-index per rendered-char-index (length
  `rendered_count + 1`).  Walks `pulldown_cmark::Parser::into_offset_iter()`.
  Handles `**bold**`, `*italic*`, `_under_`, `~~strike~~`, `==highlight==`,
  `` `code` `` (renderer adds space padding), `[text](url)`, soft/hard
  breaks.
- `paragraph_raw_col_to_rendered_col(raw_line, rendered_line, raw_col)` at
  `coord.rs:634` — the **inverse** lookup, already used by the
  `RenderedView` jitter-delay cursor overlay.  Returns `None` (caller falls
  back to 1:1) when the map's rendered count doesn't match the actual
  rendered line — headings (`## `), list items, and blockquotes prepend
  rendered glyphs that aren't in the raw text, so the count diverges.
- Two call sites of `rendered_to_raw_char_map`: one inside
  `non_table_click_to_raw_col` (`coord.rs:484`) for click handling, and one
  inside `paragraph_raw_col_to_rendered_col` (`coord.rs:644`) for the cursor
  overlay.  Both re-parse on every call.

The fix is to:

1. promote the rendered↔raw map to a shared utility with a documented
   contract for both directions,
2. cache it per raw line on `ParsedDoc` (already invalidated on every
   parse), and
3. use it in the selection painter's generic-paragraph fallback path —
   AND inside the list/table branches' inner-content mapping so inline
   markup inside list items / table cells also lines up.

### What this fix does NOT cover (descope, explicit)

The existing walker's "rendered count must equal actual rendered line count"
contract means `raw_to_rendered` returns `None` for:

- **Headings** (`# `, `## `, etc.) — the renderer emits a rendered heading
  glyph or styled prefix that has no raw counterpart.  Selection on a
  heading line keeps the current 1:1 fallback.
- **Blockquote bodies** — the rendered `▎ ` (or equivalent) prefix isn't in
  the raw text.  1:1 fallback retained.
- **List item content above the marker** — handled by
  `list_raw_col_to_rendered_col` for the marker offset only, *plus* the new
  inline map for the post-marker content (Step 4b).  If the new inline map
  also returns `None` (e.g. heading-in-list, which CommonMark allows in
  some flavours), the existing list-only mapping is the fallback.
- **Table cells** — cell-by-cell pipe mapping is preserved; the new inline
  map composes inside each cell's content (Step 4c).  Fallback is the
  existing pipe-only map.

Reference-style links, autolinks, and images are deferred (the walker
doesn't emit map entries for them today — Phase 8 concern per CLAUDE.md).

## Approach

### Step 1 — Extract `InlineColMap` into a small module

Create `src/markdown/inline_col_map.rs` (re-export from `src/markdown.rs`):

```rust
pub struct InlineColMap {
    rendered_to_raw: Vec<usize>, // raw char idx per rendered char idx (len = rendered_len + 1)
    raw_to_rendered: Vec<usize>, // rendered char idx per raw char idx (len = raw_len + 1)
    rendered_len: usize,
    raw_len: usize,
    /// True when the walker's rendered_count + 1 == rendered_to_raw.len();
    /// false means the line has a rendered prefix the walker can't see
    /// (heading / blockquote / list-marker) and callers must fall back.
    well_formed: bool,
}

impl InlineColMap {
    pub fn build(raw_line: &str) -> Self;

    /// Raw char index for a rendered char column.  Clamps to `raw_len`.
    /// Always returns a value (the click handler's existing contract).
    pub fn rendered_to_raw(&self, rendered_char: usize) -> usize;

    /// Rendered char index for a raw char column.
    ///
    /// Semantics (preserved verbatim from `paragraph_raw_col_to_rendered_col`):
    /// returns the smallest rendered idx whose raw position is `>= raw_col`.
    /// When `raw_col` lands on a marker byte (the `[` of `[link]`, the `*`
    /// of `**bold**`), this is the rendered idx immediately after the
    /// marker — matching the click handler's parking column.
    ///
    /// Returns `None` when the map is not well-formed against the actual
    /// rendered line — caller must fall back to a 1:1 mapping.  This case
    /// must be checked by also asking the caller to pass the actual
    /// rendered char count of the line (see `raw_to_rendered_checked`).
    pub fn raw_to_rendered(&self, raw_char: usize) -> Option<usize>;

    /// Same as `raw_to_rendered`, but additionally requires the caller's
    /// `actual_rendered_count` to match the walker's count.  Mirrors the
    /// existing safety check in `paragraph_raw_col_to_rendered_col` so
    /// list/heading/blockquote lines reliably fall back.
    pub fn raw_to_rendered_checked(
        &self,
        raw_char: usize,
        actual_rendered_count: usize,
    ) -> Option<usize>;

    pub fn rendered_len(&self) -> usize;
    pub fn raw_len(&self) -> usize;
    pub fn well_formed(&self) -> bool;
}
```

Move the body of `rendered_to_raw_char_map` and its `CharMapWalk` helper
(currently `pub(super)` in `coord.rs`) into the new module.  `CharMapWalk`
becomes module-private.  Build the inverse map in the same walk: each
`push_*` call already knows both the rendered char index it's emitting and
the raw char index it maps to — record both vectors at once.

For raw char indices that fall in gaps (marker bytes), `raw_to_rendered`
returns the rendered idx of the next-emitted character (binary search over
`rendered_to_raw` for `>= raw_col`).  Out-of-range queries clamp to
`rendered_len` / `raw_len`.

### Step 2 — Cache one `InlineColMap` per buffer line on `ParsedDoc`

Cache key: **buffer line index** (zero-based, matching
`Buffer::contents().split('\n')`).  This is unambiguous and matches how the
painter ends up identifying raw lines (`raw_line_idx` inside a block,
combined with the block's starting buffer line).

In `src/document/parsed_doc.rs`:

- Add `inline_maps: Vec<OnceCell<InlineColMap>>`, length =
  `buffer.line_count()`, populated lazily.
- Add `pub fn inline_map(&self, buffer_line_idx: usize, raw_line: &str)
  -> &InlineColMap`.  Caller passes the raw line text (it already has it
  to hand in both the click and paint paths), so we don't re-slice the
  rope here.  The `OnceCell` ensures we build once per line per parse.
- On `ParsedDoc::build`, allocate the vector; do not pre-populate.  Cache
  lives only as long as the current parse; it is dropped and rebuilt when
  `ParsedDoc` re-parses on edit.

**`parsed_dirty` handling.** During the window between an edit and the
next parse, byte offsets after the edit shift but `inline_maps` is still
keyed to the *pre-edit* line layout.  The painter already short-circuits
on `parsed_dirty` (see `paint.rs:148-157` — `source.get(..).unwrap_or("")`
+ "skip selection painting on this block for one frame"), so the stale
cache is never queried in that window.  No additional invalidation needed,
but add a debug assertion in `inline_map` that the caller's `raw_line`
char count matches the cached map's `raw_len` so a future regression is
caught loudly.

### Step 3 — Use the cached map in the click path

In `src/editor/mouse_ops/coord.rs`:

- Replace the call to local `rendered_to_raw_char_map(...)` inside
  `non_table_click_to_raw_col` (`coord.rs:484`) with
  `state.parsed.inline_map(buffer_line_idx, line_text).rendered_to_raw(...)`.
- Replace the body of `paragraph_raw_col_to_rendered_col` (`coord.rs:634`)
  with a call to `raw_to_rendered_checked` against the cached map, or
  delete the function entirely and inline the call at its single use site
  in `RenderedView` (Step 5).
- Delete the now-unused local `rendered_to_raw_char_map` and `CharMapWalk`.

This removes the per-click and per-frame re-parse work as a bonus.

### Step 4 — Use the cached map in the selection painter (the actual fix)

In `src/ui/rendered_view/paint.rs::paint_selection_overlay`
(`paint.rs:116-264`):

#### 4a. Generic-paragraph branch (the current 1:1 fallback, lines 247-249)

```rust
let map = editor.parsed.inline_map(buffer_line_idx, raw_line);
let actual_rendered = line.spans.iter().map(|s| s.content.chars().count()).sum::<usize>();
let (rs, re) = match (
    map.raw_to_rendered_checked(start_raw_col, actual_rendered),
    map.raw_to_rendered_checked(end_raw_col,   actual_rendered),
) {
    (Some(rs), Some(re)) => (rs, re),
    _ => (start_raw_col, end_raw_col), // existing 1:1 fallback
};
```

`buffer_line_idx` is computed as `block_start_buffer_line + raw_line_idx`
where `block_start_buffer_line` is derived from `block_range.start` via
`Buffer::line_of_offset` (or equivalent — confirm the helper name during
implementation).

#### 4b. List-item branch (composes inline collapse with marker offset)

The current `list_raw_col_to_rendered_col` only shifts past the marker.
After it returns `(content_raw_col_offset, marker_rendered_width)`, run
the inline map on the *content portion* of the raw line (the slice after
the marker) and add the marker width:

- Build a sub-line `content_raw = &raw_line[marker_end_byte..]` and
  request `inline_map_for_content(buffer_line_idx, marker_end_byte,
  content_raw)`.  This requires a small per-line *secondary* cache keyed
  by `(buffer_line_idx, marker_end_byte)`, OR — simpler — just build the
  content map on demand without caching (one parse per selection paint of
  a list line is acceptable; the click path can do the same).  Pick the
  uncached path first and revisit if profiling shows it.
- Translate `start_raw_col` / `end_raw_col` from absolute raw chars into
  raw-chars-within-content, look up via the content map, then add
  `marker_rendered_width`.  If the content map is not well-formed, fall
  back to the existing list-only mapping.

This is the only branch where the plan adds materially new code beyond
"swap call site for cache hit".  Keep it behind the same `if let (Some,
Some)` pattern the existing list branch uses so the failure mode is just
"current behaviour".

#### 4c. Table branch (composes inside each cell)

`table_raw_col_to_rendered_col` walks pipes to find the rendered column
for a raw column within a cell.  Inline markup inside a cell (e.g.
`| **bold** |`) currently goes 1:1 within the cell.  After locating the
cell, build an inline map for the cell's raw content and translate the
intra-cell raw col → intra-cell rendered col, then add the cell's
rendered start column.  Same well-formed fallback as 4b.

If implementation finds this materially complicates `table_layout` code,
defer 4c to a follow-up and call it out in **Out of scope** below.

#### 4d. Preview view

`paint_preview_selection` in `src/ui/preview.rs` — verify during
implementation.  If `VisualSelection` is already stored in *rendered*
coordinates, no change.  If it's in raw coordinates and hits the same
generic-paragraph 1:1 path, apply the 4a substitution there too.

### Step 5 — Unify cursor-indicator column with the cached map

`paragraph_raw_col_to_rendered_col` (`coord.rs:634`) already exists
specifically for the jitter-delay cursor overlay so the indicator lands
at the same visual column the click handler placed it.  Its single
caller is somewhere in `src/ui/rendered_view/` — find it via
`grep -rn paragraph_raw_col_to_rendered_col src/`.

Replace that call with
`editor.parsed.inline_map(buffer_line_idx, raw_line)
    .raw_to_rendered_checked(cursor_raw_col, actual_rendered)`.
Then delete `paragraph_raw_col_to_rendered_col` from `coord.rs`.

This kills the second symptom (cursor "shifting" on de-render) on
paragraph lines.  Heading / blockquote / list lines retain their existing
behaviour because `raw_to_rendered_checked` returns `None` and the caller
falls back to the same 1:1 path it uses today.

### Step 6 — Tests

Unit tests in `src/markdown/inline_col_map.rs`:

- Walker correctness — round-trip the rendered and raw endpoints, plus
  the boundary just before/after the first marker, on each:
  - `**Bold text**` — rendered "Bold text" (9 chars), raw 13 chars
  - `*Italic*`, `_under_`, `~~strike~~`, `==hi==`
  - `**_Bold and italic_**` — nested
  - `` `code` `` — backtick → space padding
  - `[text](url)` — bracket + url collapse
  - A line mixing all of the above (the user's report example)
- Well-formedness sentinels — assert `well_formed() == false` and
  `raw_to_rendered_checked()` returns `None` for:
  - `# heading`, `## heading`
  - `> blockquoted text`
  - `- list item` (the marker glyph alone — content portion is well-formed
    when sliced past the marker, exercised by Step 4b's content-map path)
- Marker-byte input — `raw_to_rendered_checked` on the raw col of the `[`
  of `[link]` returns the rendered col of `l` (next emitted char), not
  the col where `[` would have been if uncollapsed.

Integration test in `tests/mouse.rs` (or new `tests/selection.rs`):

- Drag-select across `**Bold text** | *Italic*` in Rendered mode with the
  cursor parked on a different line (so the target line stays rendered).
  Assert the `Selection` byte range AND the painted rendered-cell range
  agree by rendering through `TestBackend` and inspecting cells under the
  selection style.
- A list item line `- A **bold** item` — drag across `bold` and assert
  the painted cells cover the rendered `bold` glyphs, not the raw
  `**bold**` span (covers 4b).
- A heading line `## A **bold** title` — drag across `bold` and assert
  the current 1:1 fallback is unchanged (no regression).

Manual smoke:

```bash
cargo run -- demo.md  # demo.md contains the user's example formatted line
```

Click on a *different* line first (so the target stays rendered), then
drag a selection across the formatted line in Rendered mode.  Confirm the
highlight hugs the visible glyphs exactly.  Move the cursor onto the
formatted line and confirm there is no visible jump as the line
de-renders.

## Critical files

- `src/markdown/inline_col_map.rs` *(new — extracted from `coord.rs`)*
- `src/markdown.rs` — `pub mod inline_col_map; pub use ...;`
- `src/document/parsed_doc.rs` — per-line lazy cache + accessor
- `src/editor/mouse_ops/coord.rs` — use cached map; **delete**
  `rendered_to_raw_char_map`, `CharMapWalk`, AND
  `paragraph_raw_col_to_rendered_col`
- `src/ui/rendered_view/paint.rs::paint_selection_overlay` — Steps 4a,
  4b, (4c)
- `src/ui/rendered_view/*` — cursor-indicator call site (Step 5);
  find via grep before editing
- `src/ui/preview.rs::paint_preview_selection` — verify, apply 4a if
  applicable
- `tests/selection.rs` *(new)* or `tests/mouse.rs` — integration coverage

## Out of scope

- Decoupling cursor from selection active end (rejected — see Context).
- AST redesign to carry source byte ranges on every `Inline` node — the
  `OffsetIter` re-parse per line is cheap relative to a frame and lets us
  keep the AST clean.
- Heading / blockquote selection precision — these retain the existing
  1:1 fallback (see "What this fix does NOT cover").
- Reference-style links / autolinks / images (Phase 8 concern per
  CLAUDE.md — the walker doesn't emit map entries for them today).
- Step 4c (tables) MAY be deferred to a follow-up if implementation
  reveals it materially complicates `table_layout`.  In that case
  selection inside table cells with inline markup keeps the current
  cell-level 1:1 behaviour — call this out in the PR description.

## Verification

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test --lib inline_col_map
cargo test --test mouse           # or tests/selection.rs if added
cargo test                        # full sweep, including snapshots
```

Then the manual smoke described in Step 6.
