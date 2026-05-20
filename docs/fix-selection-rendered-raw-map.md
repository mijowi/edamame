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

The user also observed the cursor appearing to shift on de-render: same root cause. Fixing the column map fixes both visible symptoms.

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
}

impl InlineColMap {
    pub fn build(raw_line: &str) -> Self;

    /// Raw char index for a rendered char column.  Clamps to `raw_len`.
    /// Always returns a value (the click handler's existing contract).
    pub fn rendered_to_raw(&self, rendered_char: usize) -> usize;

    /// Rendered char index for a raw char column.  Always returns a value.
    ///
    /// Semantics (preserved verbatim from `paragraph_raw_col_to_rendered_col`):
    /// returns the smallest rendered idx whose raw position is `>= raw_col`.
    /// When `raw_col` lands on a marker byte (the `[` of `[link]`, the `*`
    /// of `**bold**`), this is the rendered idx immediately after the
    /// marker — matching the click handler's parking column.
    ///
    /// Does NOT check well-formedness — the caller is responsible for
    /// verifying the map is valid for this line before trusting the result.
    /// Use `raw_to_rendered_checked` when the caller has the actual
    /// rendered char count available.
    pub fn raw_to_rendered(&self, raw_char: usize) -> usize;

    /// Same as `raw_to_rendered`, but returns `None` when the walker's
    /// `rendered_len` doesn't match `actual_rendered_count`.  This is the
    /// well-formedness check: headings, blockquotes, and list-marker
    /// prefixes add rendered glyphs the walker can't see, causing the
    /// counts to diverge.  Callers must fall back to 1:1 when this
    /// returns `None`.
    ///
    /// Mirrors the existing safety check in
    /// `paragraph_raw_col_to_rendered_col` so list/heading/blockquote
    /// lines reliably fall back.
    pub fn raw_to_rendered_checked(
        &self,
        raw_char: usize,
        actual_rendered_count: usize,
    ) -> Option<usize>;

    pub fn rendered_len(&self) -> usize;
    pub fn raw_len(&self) -> usize;
}
```

Move the body of `rendered_to_raw_char_map` and its `CharMapWalk` helper
(currently private in `coord.rs`) into the new module.  `CharMapWalk`
stays module-private.  Build the inverse map in the same walk: each
`push_*` call already knows both the rendered char index it's emitting and
the raw char index it maps to — record both vectors at once.

**`raw_to_rendered` is a dense vector** of length `raw_len + 1`, indexed
directly by raw char index (no binary search needed).  During the walk,
entries for marker bytes (raw chars that have no rendered counterpart) are
forward-filled with the rendered index of the next visible character.
After the walk completes, any trailing unfilled entries (past the last
emitted char) are filled with `rendered_len`.  `raw_to_rendered(&self,
raw_char)` is then a simple `self.raw_to_rendered[raw_char.min(self.raw_len)]`
index lookup.  Out-of-range queries clamp to `rendered_len` / `raw_len`.

### Step 2 — Cache one `InlineColMap` per buffer line on `ParsedDoc`

Cache key: **buffer line index** (zero-based, matching
`Buffer::contents().split('\n')`).  This is unambiguous and matches how the
painter ends up identifying raw lines (`raw_line_idx` inside a block,
combined with the block's starting buffer line).

In `src/document/parsed_doc.rs`:

- Add `inline_maps: Vec<std::cell::OnceCell<InlineColMap>>`, length =
  `buffer.line_count()`, populated lazily.  Use `std::cell::OnceCell`
  (stable since Rust 1.70), NOT `std::sync::OnceLock` — `ParsedDoc` is
  single-threaded (it already uses `RefCell` for its visual-row cache)
  and `OnceLock`'s atomic overhead is unnecessary.
- Add `pub fn inline_map(&self, buffer_line_idx: usize, raw_line: &str)
  -> &InlineColMap`.  Caller passes the raw line text; on first call for
  a given `buffer_line_idx`, `OnceCell::get_or_init` builds the map from
  this text.  On subsequent calls, the cached map is returned and
  `raw_line` is only used for a debug assertion (see below).
- On `ParsedDoc::build`, allocate the vector; do not pre-populate.  Cache
  lives only as long as the current parse; it is dropped and rebuilt when
  `ParsedDoc` re-parses on edit.

**Canonical line source.** Both call sites (paint path and click path)
must derive `raw_line` the same way: from the block's byte range sliced
out of `Buffer::contents()`, then `split('\n')` to get the line within
the block.  This matches the paint path's existing `block_text` /
`raw_lines` decomposition.  The click path currently gets `line_text`
from `Buffer::line()` which may include a trailing `\n` — strip it
before passing to `inline_map` so the char count agrees.  Add a
`debug_assert_eq!(raw_line.chars().count(), cached.raw_len())` inside
`inline_map` that fires when a subsequent caller passes a different
line than the one the map was built from, catching drift loudly.

**`parsed_dirty` handling.** During the window between an edit and the
next parse, byte offsets after the edit shift but `inline_maps` is still
keyed to the *pre-edit* line layout.  The painter already short-circuits
on `parsed_dirty` (see `paint.rs:148-157` — `source.get(..).unwrap_or("")`
+ "skip selection painting on this block for one frame"), so the stale
cache is never queried in that window.  No additional invalidation needed
beyond the debug assertion above.

### Step 3 — Use the cached map in the click path

In `src/editor/mouse_ops/coord.rs`:

- Replace the call to local `rendered_to_raw_char_map(...)` inside
  `non_table_click_to_raw_col` (`coord.rs:484`) with
  `state.parsed.inline_map(buffer_line_idx, line_text).rendered_to_raw(...)`.
- Delete `paragraph_raw_col_to_rendered_col` (`coord.rs:634`) — its
  single production caller is in `src/ui/rendered_view.rs:745` and will
  be replaced in Step 5.  Also remove the re-export at
  `src/editor/mouse_ops.rs:16` (`pub use coord::paragraph_raw_col_to_rendered_col;`).
- Delete the now-unused local `rendered_to_raw_char_map` and `CharMapWalk`.
- **Migrate tests:** The round-trip tests in `src/editor/mouse_ops.rs`
  (the `#[cfg(test)]` block exercising `paragraph_raw_col_to_rendered_col`
  and `rendered_to_raw_char_map`, ~lines 845–1010) must be moved to the
  new `src/markdown/inline_col_map.rs` test module.  Adapt them to use
  `InlineColMap::raw_to_rendered` / `InlineColMap::rendered_to_raw`
  instead of the deleted functions.  Preserve the same test vectors.

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

`buffer_line_idx` is computed as `block_start_buffer_line + raw_line_idx`.
`block_start_buffer_line` is derived from `block_range.start` (a byte
offset) via the rope's byte-to-line conversion chain:

```rust
let block_start_char = editor.buffer.rope().byte_to_char(block_range.start);
let block_start_buffer_line = editor.buffer.char_to_line(block_start_char);
let buffer_line_idx = block_start_buffer_line + raw_line_idx;
```

`Buffer::char_to_line` already exists; `Buffer::rope()` already exposes
the underlying `ropey::Rope` (at `buffer.rs:135`).  `block_range.start`
always falls on a valid byte boundary (and specifically a line boundary)
because `ParsedDoc::build` splits blocks at newlines and the block byte
range starts at the first byte of its first line.  Note: during the
`parsed_dirty` window, `block_range.start` may be stale — but the
painter's existing `source.get(..).unwrap_or("")` guard (paint.rs:149-157)
means this code path is unreachable when offsets are invalid.

#### 4b. List-item branch (composes inline collapse with marker offset)

The current `list_raw_col_to_rendered_col` (in
`src/ui/rendered_view/list_marker.rs`) only shifts past the marker.
After it returns the shifted column, also apply the inline map to the
*content portion* of the raw line (the slice after the marker) so that
inline markup within the list item content is also collapsed correctly.

- Get `raw_marker_width` (chars) from `raw_list_marker_char_width(raw_line)`
  (in `list_marker.rs:33`) and `rendered_marker_width` from
  `rendered_list_marker_char_width(line)` (in `list_marker.rs:70`).
- Use the full-line cached `InlineColMap` (from Step 2) rather than
  building a separate sub-line map.  The content-relative rendered column
  is derived by offsetting into the full map:
  `content_rendered_col = map.raw_to_rendered(raw_col) - map.raw_to_rendered(raw_marker_width)`.
  This avoids a redundant parse and leverages the existing per-line cache.
- Translate `start_raw_col` / `end_raw_col` from absolute raw chars into
  content-relative chars: `content_raw_col = raw_col.saturating_sub(raw_marker_width)`.
  If the raw col falls within the marker, clamp to `rendered_marker_width`.
- For the well-formedness check, compute `actual_content_rendered_count`
  as the rendered line's total char count minus `rendered_marker_width`.
  Use `raw_to_rendered_checked` on the full-line map with
  `actual_rendered_count = actual_content_rendered_count + rendered_marker_width`
  (i.e. the total rendered char count of the line).  On `Some(rendered_col)`,
  the final rendered col is `rendered_col` directly (the full-line map
  already accounts for the marker).  On `None`, fall back to the existing
  list-only marker-shift mapping (current behavior).

This is the only branch where the plan adds materially new code beyond
"swap call site for cache hit".  Keep it behind the same `if let (Some,
Some)` pattern the existing list branch uses so the failure mode is just
"current behavior".

#### 4c. Table branch (composes inside each cell)

`table_raw_col_to_rendered_col` walks pipes to find the rendered column
for a raw column within a cell.  Inline markup inside a cell (e.g.
`| **bold** |`) currently goes 1:1 within the cell.  After locating the
cell, build an inline map for the cell's raw content and translate the
intra-cell raw col → intra-cell rendered col, then add the cell's
rendered start column.  Same well-formed fallback as 4b.

If implementation finds this materially complicates `table_layout` code,
defer 4c to a follow-up and call it out in **Out of scope** below.

#### 4d. Preview view — no change needed

`paint_preview_selection` in `src/ui/preview.rs` receives `start_col` /
`end_col` from `VisualSelection`, which stores `(rendered_line_idx,
char_col)` — already in rendered coordinates (`src/document/selection.rs:22-25`).
No raw-to-rendered translation is involved, so this path is unaffected.

### Step 5 — Unify cursor-indicator column with the cached map

`paragraph_raw_col_to_rendered_col` (`coord.rs:634`) already exists
specifically for the jitter-delay cursor overlay so the indicator lands
at the same visual column the click handler placed it.  Its single
production caller is in `src/ui/rendered_view.rs:745`.

Replace that call with
`editor.parsed.inline_map(buffer_line_idx, raw_line)
    .raw_to_rendered_checked(cursor_raw_col, actual_rendered)`.
The function itself is deleted in Step 3 (along with its re-export and
test callers).

This kills the second symptom (cursor "shifting" on de-render) on
paragraph lines.  Heading / blockquote / list lines retain their existing
behavior because `raw_to_rendered_checked` returns `None` and the caller
falls back to the same 1:1 path it uses today.

**Ordering note:** Steps 3, 4, and 5 all depend on Steps 1 and 2 but are independent of each other — they touch different files (`coord.rs`, `paint.rs`, `rendered_view.rs` respectively) with no mutual dependencies.  They can be implemented in parallel or in any order.

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
- Well-formedness sentinels — assert `raw_to_rendered_checked()` returns
  `None` (rendered count mismatch) for:
  - `# heading`, `## heading`
  - `> blockquoted text`
  - `- list item` (the marker glyph alone — content portion is well-formed
    when sliced past the marker, exercised by Step 4b's content-map path)
- Marker-byte input — `raw_to_rendered_checked` on the raw col of the `[`
  of `[link]` returns the rendered col of `l` (next emitted char), not
  the col where `[` would have been if uncollapsed.

Manual smoke:

```bash
cargo run -- demo.md  # demo.md contains the user's example formatted line
```

Click on a *different* line first (so the target stays rendered), then
drag a selection across the formatted line in Rendered mode.  Confirm the
highlight hugs the visible glyphs exactly.  Move the cursor onto the
formatted line and confirm there is no visible jump as the line
de-renders.

Additional manual cases (cover scenarios that are impractical to assert
via `TestBackend` without new test infrastructure):

- Drag-select across `**Bold text** *Italic*` — highlight should track
  the rendered glyphs, not the raw markup positions.
- Drag across `bold` inside `- A **bold** item` — highlight should cover
  the rendered `bold` glyphs, not the raw `**bold**` span (covers 4b).
- Drag across `bold` inside `## A **bold** title` — confirm the existing
  1:1 fallback is unchanged (no regression).

## Critical files

- `src/markdown/inline_col_map.rs` *(new — extracted from `coord.rs`)*
- `src/markdown.rs` — `pub mod inline_col_map; pub use ...;`
- `src/document/parsed_doc.rs` — per-line lazy cache + accessor
- `src/editor/mouse_ops/coord.rs` — use cached map; **delete**
  `rendered_to_raw_char_map`, `CharMapWalk`, AND
  `paragraph_raw_col_to_rendered_col`
- `src/ui/rendered_view/paint.rs::paint_selection_overlay` — Steps 4a,
  4b, (4c)
- `src/ui/rendered_view.rs:745` — cursor-indicator call site (Step 5)
- `src/ui/preview.rs::paint_preview_selection` — no change needed (already
  in rendered coordinates)

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
  cell-level 1:1 behavior — call this out in the PR description.

## Implementation order (dependency graph)

```
Step 1 (InlineColMap module)
    │
    ▼
Step 2 (ParsedDoc cache)
    │
    ├──────────────┬──────────────┐
    ▼              ▼              ▼
Step 3          Step 4a/4b/4c  Step 5
(click path)   (paint.rs)     (cursor overlay)
    │              │              │
    └──────────────┴──────────────┘
                   │
                   ▼
             Step 6 (tests)
```

- Steps 1 → 2 are serial prerequisites.
- Steps 3, 4 (all sub-parts), and 5 are independent leaves — they touch
  different files (`coord.rs` + `mouse_ops.rs`, `paint.rs`,
  `rendered_view.rs`) with no mutual dependencies.  They can be
  implemented in parallel or in any order.
- Step 6 (new unit tests in `inline_col_map.rs`) depends on Step 1 only.
  The migrated tests from `mouse_ops.rs` (part of Step 3) should be done
  alongside or after Step 3.

## Verification

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test --lib inline_col_map
cargo test                        # full sweep, including snapshots
```

Then the manual smoke described in Step 6.
