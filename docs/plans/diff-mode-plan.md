# File-change detection and inline diff mode

A plan for adding (a) a filesystem watcher that detects on-disk edits to
the open file, and (b) a "diff mode" overlay where the user reviews,
edits, accepts, or rejects each change inline before the merged result
becomes the new buffer.

**Core objective.** Diff mode exists to **surface every external change
to the open file for review before it overwrites what the user is
looking at** — it is *not* limited to reconciling unsaved-edit
conflicts. The motivating workflow is collaboration with an AI agent (or
any external tool) that rewrites the document on disk: the user wants to
see, hunk by hunk, what changed, regardless of whether their own buffer
had unsaved edits at the time. **The editor must never silently reload
the buffer from disk and discard the prior content** — silently swapping
in the new version is exactly the thing diff mode is meant to prevent. A
dirty buffer adds a *conflict* on top of this (the external change races
the user's unsaved edits), which is why the dirty path additionally
offers the reconciliation choices in §8; but a clean buffer is *not* a
license to skip review — it just means there is no unsaved work to
protect, so review can begin immediately without the conflict prompt.
See §11a for the correction to the initial-change dispatch that this
objective requires.

## Phasing

The work ships in two phases:

- **Phase 1 (this doc, §1–§14):** raw-only diff view. All structural
  work — watcher, hunk computation, decision flow, sub-modes, clamped
  editing, undo/redo, modals, theming, autosave gating. Diff content
  is rendered as raw markdown source on both sides. Tables are diffed
  by row: each changed/added/deleted row becomes its own hunk so the
  user reviews table changes row by row rather than as a monolithic
  block (see §3a).
- **Phase 2 (§16):** hybrid rendered diff view. Each diff hunk shows
  rendered markdown (tables as grids, code as fenced blocks, headings
  styled, lists indented) on both sides; raw substitution kicks in
  only for the focused new-side block when the user enters Edit
  sub-mode. Includes row-level table diffing (each table row becomes
  its own hunk) and per-block inline word highlighting for text-only
  blocks.

Phase 2 is significantly more complex and touches `ParsedDoc`,
`SourceMap`, `Renderer`, and `TableView` — surfaces the input-lag plan
is already modifying. Phasing avoids merge conflicts and lets the
structural pieces from Phase 1 settle before adding the rendering
machinery on top.

## Goals

- **Surface every external change for review.** Any on-disk change that
  actually differs from what the user currently has in memory enters
  diff review — clean buffer or dirty. The buffer is never silently
  reloaded/overwritten. (The only changes that bypass review are genuine
  no-ops: our own save echo, and writes that leave disk byte-identical
  to the live buffer — see §2's two filters.)
- Detect on-disk changes to the open file, debounced over a 200 ms quiet window.
- Enter a dedicated `Mode::Diff` with strong visual signalling (status-bar / hint-bar color shift, `DIFF` mode badge, first-time explanatory modal).
- Render stacked inline diffs: deleted lines above, added lines below, with word-level inline highlights within changed pairs.
- Let the user cycle through changes, accept/reject each one individually, accept-all / reject-all, skip changes, and edit added content before accepting.
- In-diff *edits* (Edit sub-mode) are undo/redo-able through `Action::Undo` / `Action::Redo`. Hunk *decisions* are deliberately **not** undoable — a mis-press is recovered by navigating back (`Tab` / `Shift-Tab`) and re-deciding, or via `DiffResetHunk`; the only navigation-irrecoverable case (`DiffAcceptAll` / `DiffRejectAll`) is guarded by a confirmation modal instead.
- After all changes are resolved, automatically swap the merged result into the live buffer and exit diff mode.
- Disable autosave and `Action::Save` (`Ctrl-S` keybind and 'Save file' action in command palette) while in diff mode to avoid clobbering the on-disk file mid-review.
- Architect the watcher so a future multi-tab refactor only has to swap `Option<Box<dyn FileWatcher>>` for a per-tab map.

## Decisions (already confirmed)

   | Question | Decision |
|---|---|
| On-disk change while buffer is **clean** | Enter diff review directly — **never** silent reload (§11a) |
| On-disk change while buffer is dirty | Warning modal, then enter diff |
| Autosave / Ctrl-S in diff mode | Both disabled |
| Diff highlighting granularity | Line-level + word-level inline |
| Multi-tab scope today | `FileWatcher` trait, single instance today |
| Accept/reject undo/redo | **Dropped** — recover via navigation + re-decide / `DiffResetHunk`; bulk flips guarded by a confirm modal (CP4, §6, §14) |
| In-diff edit undo/redo | **Required** (Edit sub-mode, CP6, see §6) |

## 1. New dependencies

```toml
notify = "8"
similar = "2"
```

- `notify` — cross-platform watcher (used by cargo-watch, mdbook, helix). Pin to the current major (8.x); 6→7→8 reshaped the `RecommendedWatcher` constructor and `Event` types, so the `NotifyWatcher` impl is written against 8's API directly rather than against a stale tutorial. Default features include backends for Linux (inotify), macOS (FSEvents), and Windows (ReadDirectoryChanges). We feed events into our existing `mpsc::Sender<AppEvent>` rather than using `crossbeam-channel`.
- `similar` — line-level + inline word-level diff in one crate (`TextDiff::from_lines`, `InlineChange`).

## 2. Watcher subsystem

```
src/watcher.rs                    # facade
src/watcher/file_watcher.rs       # FileWatcher trait + NotifyWatcher impl
src/watcher/debounce.rs           # 200 ms debouncer
```

```rust
pub trait FileWatcher: Send {
    fn watch(&mut self, path: &Path) -> Result<()>;
    fn unwatch(&mut self) -> Result<()>;
}
```

- One impl today: `NotifyWatcher` wraps `notify::RecommendedWatcher`.
- The watcher worker thread accumulates events for 200 ms of quiet, then reads the file from disk and pushes a single `AppEvent::FileChanged { path, contents }` onto the existing mpsc. Reading happens on the worker, never on the main loop.
- A `paused: Arc<AtomicBool>` mirrors `read_paused`. The external-editor flow flips it true before suspend and false after re-entry, then drains queued `FileChanged` events. The reconciliation read is **performed by the watcher worker thread, not on the main thread.** After setting `paused = false`, the external-editor flow calls `FileWatcher::force_reconcile()`, which signals the watcher worker (via a dedicated `force_reconcile` channel or an `AtomicBool` the worker polls between events) to perform a one-shot disk read and push one fresh `FileChanged { path, contents }` onto the mpsc, bypassing the watcher's 200 ms debounce window and its event filtering. The main thread does not block on disk I/O — it returns to the event loop immediately and picks up the resulting `FileChanged` event like any other. The watcher worker stays the single owner of disk reads for this file. The reconciliation read bypasses the debounce because the debouncer's purpose is to coalesce rapid successive OS events, and a single forced read after resume has no rapid-fire concern. `force_reconcile()` serves only this external-editor resume flow — mid-review disk changes are folded in live (§11b), not via a forced re-read.
- `notify::Event` variants other than `Modify` and `Create` are ignored. In particular, `Remove` events (file deletion) do not trigger diff mode — deleted-file handling is out of scope and will be addressed separately.
- **Own-write filter (content-hash).** A timestamp-based filter is unreliable on slow filesystems (NFS, SSHFS) where write-then-inotify latency can exceed 500 ms and on fast ones where rapid successive saves overlap the window. Instead we keep a content hash of the last-observed-on-disk file in memory and compare incoming `FileChanged` payloads against it. Concretely:
  - `App` carries `last_disk_hash: Option<u64>`, computed via `seahash` (zero-dependency, very fast — sub-µs for typical markdown files).
  - **Hash is stamped from three sources, all routed through the same `set_disk_hash(bytes)` helper:**
    1. Initial file load (stamp from the bytes just loaded).
    2. Every successful save — `App::save_buffer()` (the single call site for `Buffer::save_file()`, see Q1 below) stamps after `save_file()` returns `Ok`.
    3. Every accepted `AppEvent::FileChanged { contents }` — when an incoming event survives the filter (i.e. hash differs), the event-loop arm stamps the new hash *before* dispatching the change to the dirty-check / diff-entry flow.
  - The event-loop arm that handles `AppEvent::FileChanged { contents }` computes `seahash::hash(contents.as_bytes())` and drops the event iff it equals `last_disk_hash`. A match means the on-disk bytes are byte-identical to what we last observed there — either our own save echo, or a no-op write by an external tool. Either way: nothing to reconcile.
  - **Second filter: skip diff entry when disk matches the live buffer.** Even after the own-write filter accepts the event, if `seahash(contents) == seahash(editor.buffer.contents())`, no diff would be produced (disk and in-memory buffer are byte-identical). In that case stamp `last_disk_hash` to the new value and return without entering diff mode or showing the dirty-conflict modal. This is the canonical "file was modified on disk but ends up byte-equal to what I have in memory" case (e.g. an external tool re-saved the file unchanged, or the user's edits happened to converge with an external edit). Skipping diff entry here also means `DiffState::new` is never called with a hunk list that would be empty — `enter_diff_mode` can safely assume `hunks.len() >= 1`, and `focused_id` is initialized to `hunks[0].id`. **This filter is the *only* legitimate reason to skip review on an accepted event — it fires only when disk and the live buffer are byte-identical (nothing to show). A clean buffer whose disk content actually differs does NOT skip review; it enters diff mode like any other change (§11a). "Clean" is not a no-op condition — "disk == buffer" is.**
  - Because the hash tracks "what was last on disk" (not "what we last wrote"), the false-positive case I worried about earlier — external writer reverts disk to a prior state we'd already observed — does not arise. After the prior state was observed, `last_disk_hash` was updated to that state's hash; subsequent events that re-arrive at the same hash are correctly dropped (we already showed the user what disk looks like at that hash).
  - The hash field is `Option<u64>` only for the very brief window between `App::new()` and the initial file load. After load it is always `Some` for any open file.

**Q1 resolution — where does `Action::Save` and `last_disk_hash` live?**
The hash filter state lives on `App` (option a from review). `Action::Save` is hoisted out of `edit_ops::apply` into `App::handle_app_action` (`src/app/actions.rs`), which routes through a single new helper `App::save_buffer()` — the only call site for `Buffer::save_file()` post-this-change. Autosave (`src/app/autosave.rs`) and the §6 post-merge save path also call `App::save_buffer()`. Option (b) — moving `last_disk_hash` onto `EditorState` / `Buffer` — was considered and rejected: `Buffer` has no business knowing about own-write filtering, and `EditorState` would have to reach back into the watcher-event flow on `App` to be useful.

## 3. Diff subsystem

```
src/diff.rs                       # facade
src/diff/engine.rs                # compute(old, new, &mut ids) -> HunkComputation
src/diff/state.rs                 # DiffState (vestigial CP3 DiffHistory placeholder dropped in CP4 ✅)
src/diff/hunk.rs                  # Hunk, HunkKind, InlineSpan, Decision, HunkId
src/diff/layout.rs                # DiffVisualLine model + per-width row cache (§5)
src/diff/history.rs               # DiffHistory: per-diff Edit-text undo stack (CP6; Edit sub-mode only)
```

```rust
/// Stable per-hunk identifier. Monotonically allocated from
/// `DiffState::ids` (a `HunkIdAllocator`) at `DiffState::new` (initial
/// pass) and for every fresh hunk produced by post-edit recomputation
/// (§6 rule 2). IDs are never reused.
pub struct HunkId(pub u64);

pub struct DiffState {
    pub old_rope: Rope,            // pre-change in-memory buffer
    pub new_buffer: Buffer,        // current working copy (starts =
                                   //   on-disk content; user edits
                                   //   mutate this buffer's rope).
                                   //   Wrapped in a Buffer (no `path`)
                                   //   so `apply_delta`'s diff-mode
                                   //   branch can call the same
                                   //   `Buffer::insert`/`remove` methods
                                   //   (§4a). Throughout this doc
                                   //   `new_rope` is shorthand for
                                   //   `new_buffer.rope()`.
    pub cursor: Cursor,            // { offset, preferred_col }, cursor
                                   //   into `new_rope`, used only
                                   //   while DiffSubMode::Edit is active
                                   //   (in Review it is positioned at
                                   //   the start of the focused hunk's
                                   //   new-side range but ignored for
                                   //   rendering — focus is shown by the
                                   //   focused/unfocused background
                                   //   intensity, not a gutter, §5)
    pub hunks: Vec<Hunk>,          // recomputed after every mutation
    pub focused_id: HunkId,        // single source of truth for "which
                                   //   hunk is the user looking at"; an
                                   //   id (not an index) so it survives
                                   //   post-edit hunk-list reshapes
    pub decisions: Vec<Decision>,  // parallel to `hunks`; keyed by index
    pub history: DiffHistory,      // see §6
    pub uneven_table_fallback: bool, // ≥1 table couldn't be row-split
                                   //   (uneven cell counts, §3a) — drives
                                   //   the entry hint
    pub(crate) ids: HunkIdAllocator, // monotonic HunkId allocator
    layout: RefCell<DiffLayoutCache>, // lazily-built flat visual-line
                                   //   list + per-width row-count cache
                                   //   (see §5, src/diff/layout.rs)
    // CP6 adds: pub sub_mode: DiffSubMode (§4 sub-modes)
}

pub enum Decision { Pending, Accepted, Rejected }

pub struct Hunk {
    pub id: HunkId,
    pub old_lines: Range<usize>,   // line indices into old_rope
    pub new_lines: Range<usize>,   // line indices into new_rope
    pub inline: Vec<InlineSpan>,   // per-line word-level deltas
    pub kind: HunkKind,            // Replace | Insert | Delete
}
```

`DiffState::resolved_rope(&self) -> Option<Rope>` walks `hunks` in
order, picking the old-side or new-side line range per `decisions[i]`.
It returns `None` when any decision is still `Pending` (rather than a
typed error or a panic), so a misuse flashes a status hint instead of
crashing the TUI; the resolution path treats `None` as "not yet
resolvable" and bails. When every decision becomes non-`Pending`, a
`DiffResolveConfirmModal` is shown (see §8). On confirmation, the App
calls `resolved_rope()`, swaps the result into `editor.buffer` via
`Buffer::set_rope`, clears `editor.diff`, and exits diff mode. CP3
resets `editor.history` to empty; CP4 instead replaces it with a
single merge-revert entry (see §6).

**Public setters required.** This swap-in-place needs APIs that don't
exist today; both are introduced as part of this work:

- `Buffer::set_rope(&mut self, rope: Rope)` — replaces the rope while
  preserving `path`. **Must bump `Buffer::version`** so source-map /
  parsed-doc invalidation fires correctly; without that, downstream
  consumers keep using the pre-swap parse. Any internal per-rope
  caches on `Buffer` are also cleared. Used by the resolution path
  to swap in the merged rope; the caller must follow up with
  `EditorState::refresh_parsed()` so `ParsedDoc` and `SourceMap`
  rebuild against the new rope before the next render.
- `History::reset_with(&mut self, delta: EditDelta)` — clears both
  `undo_stack` and `redo_stack`, then pushes `delta` as the sole
  undo entry. Used by the resolution path to seed the synthetic
  merge-revert entry described in §6.

## 3a. Row-level table diffing

Even in Phase 1's raw view, tables are diffed by row rather than
as monolithic blocks. The engine pre-scans both `old_rope` and
`new_rope` once for table extents using **the real markdown parser** —
`pulldown-cmark` is already a dependency, and `Tag::Table` /
`TagEnd::Table` events carry authoritative byte ranges via the
parser's offset iterator. The scan is performed by a new shared
helper in `src/markdown/parse_offsets.rs`:

```rust
/// Abstract over the three event kinds we care about for block scanning,
/// so the shared scanner can pair starts and ends cleanly and dispatch
/// leaf events without exposing pulldown-cmark's `Tag`/`TagEnd`
/// asymmetry to callers.
///
/// Verified against pulldown-cmark 0.13 (the version pinned in
/// `Cargo.toml`): `Tag::HtmlBlock`/`TagEnd::HtmlBlock` is the wrapped
/// form, and `Event::Html(_)` is the leaf block-level form that
/// arrives without a surrounding `Tag`. `Event::InlineHtml(_)` is
/// inline-level by definition and is **not** a block — it does not
/// appear in `BlockKind`. The `HtmlLeaf` variant exists solely so the
/// shared scanner can record block-level HTML emitted as a leaf event
/// (the parser does this for some constructs); add a match arm in the
/// scanner for `Event::Html(_)` that records the event's offset range
/// when `depth == 0` and `keep(BlockKind::HtmlLeaf)`.
pub enum BlockKind {
    Paragraph, Heading, CodeBlock, BlockQuote, List, Table, HtmlBlock,
    Rule, HtmlLeaf,
}

pub fn block_ranges_by<F>(source: &str, mut keep: F) -> Vec<Range<usize>>
where
    F: FnMut(BlockKind) -> bool,
{ /* depth-tracked walk over the offset iterator: push on Start when
     keep(kind), pop on matching End, record range when depth returns
     to zero; for leaf events (Rule, HtmlInline) record directly when
     keep(kind) and depth==0; trailing-newline handling identical to
     today's advance_past_newline. */ }
```

The existing `top_level_block_ranges()` is reimplemented as
`block_ranges_by(source, |k| matches!(k, BlockKind::Paragraph | ::Heading | ::CodeBlock | ::BlockQuote | ::List | ::Table | ::HtmlBlock | ::Rule | ::HtmlLeaf))`,
and the diff engine calls it with `|k| k == BlockKind::Table`. A
thin wrapper `diff::engine::table_extents(source: &str) ->
Vec<TableExtent>` converts the returned byte ranges to line ranges
and wraps each in a `TableExtent`. The `BlockKind` abstraction
sidesteps the `Tag` / `TagEnd` start-end asymmetry that made the
original `keep: FnMut(&Tag<'_>) -> bool` proposal impossible to use
for depth tracking (you can't ask `TagEnd::Table` "are you a kept
tag?" because `TagEnd` carries no `Tag`); mapping both sides into a
single `BlockKind` enum first restores the symmetry.

**Line-range convention (applies to `TableExtent` AND to every
`Hunk`'s `old_lines` / `new_lines`).** All line ranges in the diff
subsystem use **half-open** `[start_line, end_line)` semantics,
where `start_line = rope.byte_to_line(byte_start)` and `end_line =
rope.byte_to_line(byte_end_exclusive)`. `byte_end_exclusive` is the
byte offset *after* the last byte of the range (i.e. one past the
trailing newline of the final line).

**Ropey's `byte_to_line` at the file's last byte.** In `ropey`,
`byte_to_line(rope.len_bytes())` returns `rope.len_lines() - 1` in
*both* the trailing-newline and no-trailing-newline cases — but the
index points at different things:

- **File ends with `\n`** (the common case for markdown):
  `len_lines()` counts the empty trailing line, so
  `byte_to_line(len_bytes()) == len_lines() - 1` points at that
  empty line.  Iterating `start..end_line` then correctly covers
  all content lines and excludes the empty trailing one.
- **File ends without `\n`**: `byte_to_line(len_bytes())` points
  at the *last content line* — so an `end_line` derived this way
  is **off by one** and a `start..end_line` iteration would miss
  that final line.  This is a known limitation pinned by
  `tests/diff_engine.rs::ropey_line_range_invariants`.  In Phase 1
  we accept it because edamame opens markdown files which
  conventionally end in `\n`; revisit if no-trailing-newline files
  become a first-class input.

A hunk is row-split only when it is **fully contained** within a
`TableExtent` on *both* sides — i.e. `extent.start_line <=
hunk.lines.start && hunk.lines.end <= extent.end_line` for the
old-side extent against `old_lines` and the new-side extent against
`new_lines` (`engine::find_extent_idx`). Containment, not mere
overlap, is required: `split_table_hunk` re-diffs the *whole* table
extent, so a hunk that straddles the table boundary (also covering
non-table lines above or below) would have its out-of-extent lines
silently dropped from the row-diff output — losing a reviewable
change and corrupting the merge. A straddling hunk therefore stays
a single monolithic line-level hunk instead.

This avoids the regex-based detector's false positives on
code-block content and correctly handles edge cases (empty cells,
escape sequences, alignment markers) that a hand-rolled regex
would miss.

When `compute` produces a `Replace`, `Insert`, or `Delete` hunk
that is contained (per the test above) in a `TableExtent` on both
sides, the hunk enters a sub-diff. Because the row-diff runs over
the *full table extent* (not just the hunk's lines), a hunk that
only spans data rows — with the header / separator outside the hunk
as context — still triggers row-level diffing of the whole table.

**Tables that fragment across the diff.** Several contained hunks of
one old-side table can map to *different* new-side extents when the
table splits into two on the new side (a fresh header + separator
appears mid-table, so the lower fragment parses as its own table).
A single extent re-diff cannot represent two new tables, so such an
old extent is detected (`engine`'s per-extent `NiMap::Conflict`
pre-scan) and left un-split: its contained hunks pass through as
line-level hunks rather than being silently dropped. Regression:
`table_split_into_two_keeps_both_changes_reviewable`. A table that
maps to exactly one new-side extent (`NiMap::One`) is row-split once
and the other contained hunks for that extent are dropped, so its
rows aren't emitted twice.

1. Take the contiguous old-side and new-side table-row ranges of the
   *whole containing extent* (not just the triggering hunk's lines).
2. **Column-count guard (every row, both sides).** Scan every row
   on each side (header, separator, and all data rows) and tally
   cell counts. If *any* row's cell count differs from any other
   on the same side, or if the per-side maximum cell counts differ
   between sides, abort row-level sub-diff and leave the hunk as
   a single whole-table `Replace`. Header-only checking is
   insufficient because markdown allows data rows with fewer cells
   than the header (legal — trailing cells default to empty), and
   per-row accept/reject across a cell-count boundary can produce
   a merged table where data rows reference a different column
   count than the header. Bailing on any mismatch — not just
   header mismatch — avoids the footgun and the user reviews the
   table as a unit. When the guard trips, fire a transient hint
   "Table has uneven row widths — reviewing as a unit" so the
   user understands why this table was not row-diffed (otherwise
   the monolithic replace reads like a bug). If every row on both
   sides has the same cell count, proceed.
3. Run `similar::TextDiff::from_lines` over the extent's rows.
4. Split the single `Replace` hunk into per-row hunks. **Maximum
   granularity is per-row, not per-cell** — sub-row cell-level diffing
   is deferred to Phase 2. **Neighboring changed rows are coalesced
   into a single hunk** rather than emitted as one-hunk-per-row: a run
   of consecutive non-context rows (any mix of `Replace`, `Insert`,
   `Delete`) becomes one hunk spanning the run. Runs are broken only
   by unchanged context rows. Each resulting hunk gets its own
   `HunkId` and `Decision`.

The header row and separator row (`|---|---|`) participate in the
row-level diff like any other row. If both sides have identical
headers, they appear as context; if the header changed (e.g. a column
was added), the header rows are part of the leading hunk in the run.

**Resolution semantics for table sub-hunks.** Row sub-hunks within a
single table extent are resolved together by row order — when
`resolved_rope()` walks the hunk list, it emits the chosen rows in
their original positional order so the merged table remains
syntactically valid. The separator row, if unchanged on both sides,
is always emitted exactly once at its canonical position regardless
of which adjacent row-hunks are accepted or rejected. If the
separator itself is part of a hunk (e.g. column count changed), the
chosen side's separator is emitted.

**Performance.** The table-extent scan is a single pass of
`pulldown-cmark`'s event iterator filtered to `Tag::Table` /
`TagEnd::Table` — not a full `ParsedDoc::build`. For a typical
markdown file this is sub-millisecond and negligible next to the
`similar` line-diff that follows it.

This gives the user per-row accept/reject granularity on table
changes without requiring the Phase 2 rendered-table machinery.
In raw view the rows display as `| cell | cell |` source lines with
diff-colored backgrounds — legible for review, if not pretty.

## 4. EditorState integration

```rust
pub struct EditorState {
    // ...existing fields...
    pub diff: Option<DiffState>,
}
```

- New `Mode::Diff` variant added to `editor::mode::Mode`. The invariant is `mode == Mode::Diff ⟺ diff.is_some()`, kept consistent by `enter_diff_mode()` / `exit_diff_mode()` helpers. `enter_diff_mode` initializes `DiffState::sub_mode = DiffSubMode::Review` and `focused_id = hunks[0].id`; it requires `hunks.len() >= 1` (callers must guard with the buffer-vs-disk hash check in §2 before invoking). The redundancy is intentional — existing `match state.mode { … }` dispatch in status-bar, hint-line, and `preview_safe_action` is cheaper to extend with one arm than to thread `Option<&DiffState>` everywhere.
- Edits in diff mode mutate `diff.new_rope` and `diff.cursor`, **not** `editor.buffer` / `editor.cursor`. After each edit the hunk list is recomputed (cheap — `similar` line-diff over a typical markdown file is sub-millisecond).
- `EditorState` gains `pub pre_diff_scroll: usize`. `enter_diff_mode()` writes it from the current `scroll` then resets `scroll = 0`; `exit_diff_mode()` reads it back into `scroll`. It is consumed at exit and otherwise inert.

### §4a Edit dispatch

**Current mutation surface (verified against the codebase).** Today,
the central mutation point for all editing is
`EditorState::apply_delta(delta: EditDelta)` in
`src/editor/state.rs:625`. Nearly every edit handler goes through it,
either directly or via the helper functions `insert_text(state, &str)`
and `delete_selection(state, sel)` in `src/editor/edit_ops.rs`, both
of which build an `EditDelta` and call `state.apply_delta()`.
`apply_delta` atomically:

1. Removes `delta.removed` from `self.buffer` at `delta.offset`.
2. Inserts `delta.inserted` into `self.buffer` at `delta.offset`.
3. Calls `self.history.record(delta)`.
4. Sets `self.cursor.offset` to `delta.redo_cursor()`.
5. Sets `self.dirty = true`.
6. If the edit crosses a line boundary: calls `self.refresh_parsed()`
   and `self.update_cursor_block()`.
7. Otherwise: sets `self.parsed_dirty = true` and bumps
   `self.parsed_version`.

No handler calls `Buffer::insert_char()` directly — all
handler-driven mutations route through `apply_delta`. `Buffer`'s
public mutation API is `insert(offset, text)`, `insert_char(offset,
char)`, `remove(start, end)`, and `remove_char(offset)`, but the
higher-level edit helpers (`insert_text`, `delete_selection`, and
direct `apply_delta` calls) are the only paths used by handlers. The only code paths that bypass
`apply_delta` are `History::undo` / `History::redo`, which apply the
inverse delta to the buffer internally (see "Undo / Redo" below).

**Approach — mode-aware `apply_delta`.** Make `apply_delta` itself
branch on diff mode. When `self.diff` is `Some` and sub-mode is
`Edit`, `apply_delta` routes the edit to `diff.new_buffer` /
`diff.history` / `diff.cursor` and triggers hunk recomputation
instead of the normal side effects. This means:

- The existing helper functions (`insert_text`, `delete_selection`,
  and the ~20 direct `state.apply_delta(EditDelta{...})` call sites)
  work unchanged — they still call `state.apply_delta()`, which now
  does the right thing automatically.
- No mechanical rewrite of every handler across four modules. The
  helpers that wrap `apply_delta` are untouched.

```rust
pub(crate) fn apply_delta(&mut self, delta: EditDelta) {
    // Invariant: mode == Mode::Diff ⟺ diff.is_some(). Surfaces a
    // desync the first time enter_diff_mode/exit_diff_mode is
    // bypassed (or one is flipped without the other).
    debug_assert_eq!(
        self.mode == Mode::Diff,
        self.diff.is_some(),
        "Mode::Diff and self.diff.is_some() must agree",
    );
    if let Some(ref mut diff) = self.diff {
        if diff.sub_mode == DiffSubMode::Edit {
            diff.apply_edit(delta);
            return;
        }
        // Review sub-mode: apply_delta should never be called.
        // Cursor-motion and decision actions don't go through
        // apply_delta; text-input handlers are gated upstream
        // by the diff-mode dispatch arm in App::dispatch_action.
        debug_assert!(
            false,
            "apply_delta called in DiffSubMode::Review — text edits \
             should be gated by App::dispatch_action's diff arm",
        );
        return;
    }
    // ... existing logic (buffer, history, cursor, dirty,
    //     refresh_parsed, update_cursor_block) unchanged ...
}
```

`DiffState::apply_edit(delta)` performs the diff-mode equivalent:

1. Removes/inserts on `self.new_buffer` (same buffer ops as the
   main path).
2. Wraps `delta` into `DiffOp::Edit { delta }` and pushes onto
   `self.history` (the `DiffHistory`).
3. Sets `self.cursor.offset` to `delta.redo_cursor()`.
4. Triggers hunk re-computation (§6 "HunkId stability").

**Side effects NOT fired in diff mode:** `self.dirty` is not set on
`EditorState` (the main buffer hasn't changed). `self.parsed_dirty` /
`self.refresh_parsed()` / `self.update_cursor_block()` are not called
(they operate on the main `ParsedDoc`, not the diff content — the
diff view renders raw lines from `new_buffer`'s rope directly).

**`DiffState::new_buffer` type.** `new_rope` is wrapped in a
`Buffer` (no `path`, used purely as a rope+API holder) so the same
`Buffer::insert` / `Buffer::remove` methods are available on both
sides. `DiffState::new_rope` becomes `pub new_buffer: Buffer`.

**`EditTarget` accessor for Undo/Redo and read access.** The
`apply_delta` routing handles all write-path mutations. For the two
code paths that bypass `apply_delta` — Undo/Redo (which call
`history.undo(&mut buffer)` directly) — and for read access to the
active buffer, `EditorState` exposes an accessor:

```rust
pub struct EditTarget<'a> {
    pub buffer: &'a mut Buffer,
    pub cursor: &'a mut Cursor,
    pub history: ActiveHistory<'a>,
}

pub enum ActiveHistory<'a> {
    Main(&'a mut History),
    Diff(&'a mut DiffHistory),
}

impl EditorState {
    /// Returns the active edit target: the buffer / cursor / history
    /// that mutating actions should target. Valid in Normal mode and
    /// in `DiffSubMode::Edit`. **Panics (debug) / `unreachable!()`s
    /// (release-ish via `debug_assert!` + early return path) when
    /// called in `Mode::Diff` Review** — Review does not perform
    /// text edits and there is no coherent buffer to return there
    /// (the main buffer and `DiffHistory` belong to different ropes).
    /// Review performs no undoable mutation at all (decisions are not
    /// recorded in `DiffHistory`), so it never needs an edit target.
    pub fn edit_target(&mut self) -> EditTarget<'_> {
        match &mut self.diff {
            Some(d) if d.sub_mode == DiffSubMode::Edit => EditTarget {
                buffer:  &mut d.new_buffer,
                cursor:  &mut d.cursor,
                history: ActiveHistory::Diff(&mut d.history),
            },
            Some(_) => {
                debug_assert!(false, "edit_target() called in DiffSubMode::Review");
                // Release fallback: route to main buffer with main
                // history so we never hand back a mismatched
                // (buffer, history) pair. Any actual mutation here
                // is a programming error caught by the debug_assert.
                EditTarget {
                    buffer:  &mut self.buffer,
                    cursor:  &mut self.cursor,
                    history: ActiveHistory::Main(&mut self.history),
                }
            }
            None => EditTarget {
                buffer:  &mut self.buffer,
                cursor:  &mut self.cursor,
                history: ActiveHistory::Main(&mut self.history),
            },
        }
    }
}
```

`edit_target().buffer` returns `&mut Buffer`, which derefs to
`&Buffer` for read access. Handlers that read from the active buffer
(e.g. `state.buffer.len_chars()`, `state.buffer.contents()`) should
go through `edit_target().buffer` when in diff Edit mode so they read
from `new_buffer` rather than the main buffer. In practice, most
read-only call sites that matter in diff Edit mode are inside the
same helpers that already go through `apply_delta` (e.g.
`insert_text` reads `state.cursor.offset` and
`state.buffer.slice_to_string` before building the delta). Table
navigation (`cursor_in_table`, `table_move_horizontal`) and list
navigation (`list_move_horizontal`) read from `state.buffer` directly.
These are guarded against diff mode by an explicit early-return in
`cursor_in_table()`:

```rust
pub(super) fn cursor_in_table(state: &EditorState) -> bool {
    if state.mode == Mode::Diff { return false; }
    current_table(state).is_some()
}
```

This guard prevents the `InsertTab` handler (which internally calls
`cursor_in_table` before dispatching to `table_next_cell`) from
accidentally triggering table cell navigation in diff Edit mode.
The same guard covers `TablePrevCell`, which also calls
`cursor_in_table` before dispatching. The reason for the table
guard is the `Tab` / `Shift-Tab` collision: those keys are reserved
for hunk navigation in Review and for ordinary indentation in Edit,
so column-cycling must not fire.

**List continuation, indent, and renumber are disabled in diff
Edit.** A guard mirroring `cursor_in_table()` is added to
`current_list()` (and any other list-edit entry point that reads
from `state.buffer` to decide what to do):

```rust
pub(super) fn current_list(state: &EditorState) -> Option<...> {
    if state.mode == Mode::Diff { return None; }
    // existing logic
}
```

The reason is correctness, not policy. List-edit code in
`src/editor/list_edit/` reads the buffer to find the existing list
marker, indent depth, and ordinal, then builds an `EditDelta` based
on what it found. In diff Edit the writes route correctly to
`new_buffer` via `apply_delta`, but the *reads* still hit
`state.buffer` (the main buffer, unchanged since diff entry), so a
list line that only exists inside `new_buffer` would be invisible
to the list-edit code, producing a no-op or — worse — a delta keyed
to the wrong marker. Rather than rewrite every read site in
`edit_ops` to go through `edit_target().buffer`, we accept that
list affordances are unavailable in diff Edit: pressing `Enter`
inserts a bare newline (no marker continuation), `Tab` /
`Shift-Tab` insert/remove a literal indent at the cursor, and the
user maintains list markers by hand if they need to. The same
applies to checkbox-toggling helpers (`toggle_checkbox_at_cursor`)
which also read from `state.buffer`.

A user who needs full list-editing affordances inside a replacement
hunk can resolve the diff first (accept what they want, reject the
rest), then edit the merged buffer normally. Lifting this to full
read-routing through `edit_target()` is a future improvement
called out in §16 polish.

**Mouse policy in diff mode.** Rather than thread `edit_target()`
through every mouse handler in `src/editor/mouse_ops/`, **mouse
interactions in diff mode are constrained to the bare minimum
needed for hunk editing.** Any mouse handler that reads
`state.buffer` to do something more than "set the text cursor" is
disabled in `Mode::Diff` by an early-return guard at its entry
point. Concretely:

| Function | File | Diff-mode behavior | Why |
|---|---|---|---|
| `mouse_ops::apply` | `mouse_ops.rs` | Dispatches based on action; the table below covers each branch. | Top-level entry. |
| `selection::handle_click` (cursor placement) | `mouse_ops/selection.rs` | **Edit only. ** In Edit, places the text cursor within the focused hunk's new-side range (clamps via `clamp_to_focused_hunk`, §5). In Review, no-op (Review has no text cursor). | Cursor outside the hunk would break the clamp invariant; cursor in Review is meaningless. |
| `selection::handle_drag` | `mouse_ops/selection.rs` | **Edit only.** Extends selection within the focused hunk; drag past the hunk edge clamps to the edge (does not flash, mouse drags routinely cross the edge). In Review, no-op. | Same rationale. |
| `selection::select_word_at_cursor`, `select_line_at_cursor` (double/triple click) | `mouse_ops/selection.rs` | **Edit only**, clamped to hunk. Review: no-op. | Reads `state.buffer` to walk word/line boundaries — must read `new_buffer` in Edit; meaningless in Review. The simplest correct path is to route the `&state.buffer` reads through `edit_target().buffer` only in these two functions (they are the cleanest candidates because they don't transitively call into list/table code). |
| `selection::scroll_by_mouse`, `set_scroll_absolute` | `mouse_ops.rs` | Allowed in both sub-modes; reads `EditorState::scroll` only, not the buffer. | Already buffer-agnostic. |
| `checkbox::toggle_checkbox_at` | `mouse_ops/checkbox.rs` | **No-op in `Mode::Diff` (both sub-modes).** Early-return guard at function top. | Reads main buffer, would route mutation to wrong buffer (or to `new_buffer` with a delta computed against the wrong source — silent corruption). User can toggle by editing the `[ ]` glyph directly in Edit. |
| `links::hovered_link_target`, `link_at_offset` | `mouse_ops/links.rs` | **No-op in `Mode::Diff` (both sub-modes).** Early-return / return `None`. | Link hit-testing scans `state.buffer.contents()`; clicks on rendered links would be ambiguous (which side of the hunk did you click?) and following a link mid-review is out of scope. |
| `table_drag::*` (column-divider drag for table resize via packed-comment hints) | `mouse_ops/table_drag.rs` | **No-op in `Mode::Diff` (both sub-modes).** Early-return guard. | Table column resize mutates the table's packed-comment hint via `state.buffer` reads + an edit — both buffers wrong in diff mode, and the diff view doesn't render tables as grids in Phase 1. |
| `coord::click_to_char_offset` and friends | `mouse_ops/coord.rs` | Allowed; in Edit sub-mode the returned offset is then passed through `clamp_to_focused_hunk` by the caller. | Coord translation reads `state.buffer` for line layout — in Edit it must read `new_buffer` instead, so its `&EditorState` parameter is replaced with the active buffer reference via `edit_target().buffer` at the two call sites that matter (click → cursor, drag → selection extend). |

**Implementation.** Add a single helper `fn diff_blocks_mouse_op(state: &EditorState) -> bool { state.mode == Mode::Diff }` (or inline the check). At the top of each function listed as "No-op in `Mode::Diff`", insert `if state.mode == Mode::Diff { return /* default */; }`. For the selection / coord functions that need to read `new_buffer` in Edit, route the `state.buffer` reads through `edit_target().buffer` (this is the only place outside `apply_delta` where a buffer read needs to be sub-mode-aware).

**List-edit mouse paths.** The `src/editor/list_edit/` module has no mouse entry points — it is invoked only by `edit_ops` action handlers, which already go through the `apply_delta` path and the `current_list()` guard added above. No additional mouse-side guards are needed for list-edit.

**Test coverage.** `tests/mouse.rs` gains a `Mode::Diff` test matrix: click on a checkbox in a `Mode::Diff` editor → no toggle; click on a link → no follow; drag across hunk boundary in Edit → selection clamps to hunk edge; click outside the focused hunk in Edit → cursor snaps to nearest in-hunk offset (or no-op, per implementation choice — pin whichever in the test).

**Undo / Redo in diff mode.** The Undo/Redo handlers
(`edit_ops.rs:497–511`) bypass `apply_delta` and call
`state.history.undo(&mut state.buffer)` directly. In diff mode the
dispatch splits on sub-mode: **Review is a no-op** (decisions are
deliberately non-undoable — recover a mis-press by navigating back and
re-deciding, or with `DiffResetHunk`), while Edit drives `DiffHistory`
over the full `(buffer, cursor, history)` triple:

> **CP-scope note.** The snippet below is the **CP6 end-state**: the
> `DiffSubMode` / `edit_target()` / `ActiveHistory` machinery it uses
> does not exist until CP6. **Review never undoes** — there is no
> `DiffHistory` of decisions (that design was dropped; the accidental
> bulk-flip case is guarded by the CP4 accept-all/reject-all confirm
> modal instead). Until CP6 lands, `Undo` / `Redo` in `Mode::Diff` are
> plain no-ops, gated through `diff_safe_action` +
> `dispatch_diff_action` (`src/app/actions.rs`), never reaching the
> `edit_ops.rs` arm shown here. Read the `DiffSubMode::Edit` arm only
> when implementing CP6; the `Review` arm stays a no-op.

```rust
Action::Undo => {
    match state.mode {
        Mode::Diff => {
            let sub_mode = state.diff.as_ref().unwrap().sub_mode;
            match sub_mode {
                // Decisions are non-undoable; nothing to do.
                DiffSubMode::Review => {}
                DiffSubMode::Edit => {
                    let t = state.edit_target();
                    let ActiveHistory::Diff(dh) = t.history else {
                        unreachable!("Edit sub-mode always yields Diff history");
                    };
                    match dh.undo(t.buffer, t.cursor) {
                        UndoResult::Edit => {
                            // hunk recompute already triggered inside
                            // DiffHistory::undo for Edit ops
                        }
                        UndoResult::Empty => {}
                    }
                }
            }
        }
        _ => {
            // Normal mode — existing logic
            let t = state.edit_target();
            let ActiveHistory::Main(h) = t.history else {
                unreachable!("non-Diff mode always yields Main history");
            };
            if let Some(offset) = h.undo(t.buffer) {
                t.cursor.offset = offset.min(t.buffer.len_chars());
                state.refresh_parsed();
                state.ensure_cursor_visible(viewport_height, viewport_width);
            }
        }
    }
}
```

`DiffHistory` (CP6) records `DiffOp::Edit` entries only — never
decisions — so its `undo` always carries a buffer mutation. Review
holds no history at all, so the type system never has to reconcile a
`buffer` and `history` that belong to different ropes.

`DiffHistory::undo` returns an `UndoResult` enum indicating whether an
entry was popped (`Edit`) or the stack was empty (`Empty`). For `Edit`,
the inverse delta is applied to `new_buffer`, the cursor is
repositioned, and hunk recomputation fires. `Redo` is symmetric.

**`enter_edit_if_preview` guard.** Many handlers in `edit_ops.rs`
begin with `enter_edit_if_preview(state, viewport_height)`, which
transitions from Preview to Rendered mode by overwriting
`state.mode`. In `Mode::Diff` this call must be a no-op — it would
corrupt state by switching mode away from `Diff`. The fix is a
single guard inside `enter_edit_if_preview` itself:

```rust
fn enter_edit_if_preview(state: &mut EditorState, viewport_height: usize) {
    if state.mode == Mode::Preview {
        sync_cursor_to_scroll(state, viewport_height);
        state.mode = Mode::Rendered;
        state.visual_selection = None;
    }
    // Mode::Diff is never Preview, so the guard is a no-op.
    // No explicit check needed — the `== Preview` test is sufficient.
}
```

Because the function already checks `state.mode == Mode::Preview` and
`Mode::Diff` is never `Preview`, no additional guard is required —
the existing check is already safe. All call sites in `edit_ops.rs`
and the table/list helpers are covered by this existing guard.

**Handlers that need special-casing in diff Edit mode:**

1. **Cursor-motion handlers** (`MoveUp`/`MoveDown`/`MoveLeft`/etc.):
   in `DiffSubMode::Edit` they clamp to the focused hunk's new-side
   line range (§4). Clamping happens in the handler after the move
   computation but before the cursor write — a single
   `clamp_to_hunk(&mut cursor, focused_hunk, rope)` call.
2. **Boundary-crossing deletes** (`Backspace` at first char,
   `Delete` at last char of the focused range): the handler checks
   the range edge before applying and no-ops with a status flash if
   so (§4).
3. **`Paste`**: in `DiffSubMode::Edit`, paste contents are inserted
   at the cursor like any other text, but the post-insert clamp
   ensures the cursor doesn't escape; if the pasted text contains
   newlines that expand the hunk, that is allowed (§4 "Newlines
   expand the hunk").

Non-edit handlers (selection, scroll, search) continue to read
`state.buffer` / `state.cursor` directly because in diff mode they
target the active sub-view (selection / search in Edit refer to
`new_rope`, in Review they are unused). Each non-edit handler is
gated by `match state.mode` at the call site as today.

### Sub-modes within `Mode::Diff`

Mirroring the existing top-level Mode architecture, diff mode has two
sub-modes tracked by a `DiffSubMode` field on `DiffState`:

```rust
pub enum DiffSubMode {
    Review,
    Edit,
}
```

- **`Review`** (default on entry). No active text cursor. Focus is on the currently selected hunk, indicated by its stronger add/delete background fill (non-focused hunks recede to a fainter wash) and a decision divider that gains a `>` caret and, while pending, an inline `Accept [y] · Reject [n]` prompt (the glyphs sourced from the shared `diff_keys` table). Decision keys (`y` / `n` / `Shift-Y` / `Shift-N`) work as bare keys because no text is being typed. Hunk navigation: `Tab` / `Shift-Tab`. Entering Edit: `Enter` or `i`. Exiting diff: `Esc`, gated on full resolution — a no-op while hunks are still pending, and opens the `DiffResolveConfirmModal` once every hunk is decided (see §8/§9).

- **`Edit`**. Normal text editing, **hard-clamped to the currently
  focused hunk's new-side line range**. The text cursor is visible
  inside that range and cannot leave it via cursor-motion actions
  (`MoveUp` / `MoveDown` / `MoveLeft` / `MoveRight` / `MoveHome` /
  `MoveEnd` / `MoveDocStart` / `MoveDocEnd` etc. clamp to the
  hunk-range boundaries; attempts past the edge flash a status hint
  "Esc to leave hunk"). **Boundary-crossing deletes are also
  no-ops**: `Backspace` at the first char of the hunk's new-side
  range and `Delete` at the last char flash the same hint and leave
  the rope unchanged — the clamping invariant applies to all
  mutations, not just cursor motion. Other hunks — including unchanged
  context lines and other add/delete hunks — are unreachable from
  within Edit; the only way to edit a different hunk is `Esc` →
  `Tab` / `Shift-Tab` → `Enter`. Inserting newlines is explicitly
  allowed and **expands** the focused hunk's range; the hunk grows
  downward as the new-side line count increases, and subsequent hunks
  shift down by the net delta. The hunk re-computation after each edit
  re-snaps the (now larger) focused hunk so its `HunkId` is preserved
  (§6). Decision keys (`y` / `n` etc.) are just characters in Edit —
  to make a decision the user must `Esc` first. Bulk-decision keys
  likewise have no special meaning in Edit. `Esc` returns to Review;
  edits are retained (already applied to `new_rope`).

  **Editing a Delete hunk.** A `HunkKind::Delete` hunk has an empty
  new-side line range. Pressing `DiffEnterEdit` on a Delete hunk
  converts it into a replacement: a blank line is inserted into
  `new_rope` at the hunk's position, the hunk becomes
  `HunkKind::Replace`, and the cursor is placed on that blank line.
  From the user's perspective this means "I want to write something
  to replace the deleted text." If the user then deletes the blank
  line back to empty and presses `Esc`, the hunk reverts to
  `HunkKind::Delete` on the next re-computation.

  This blank-line insertion is **not** recorded as a `DiffOp::Edit`
  — it is a side effect of `DiffEnterEdit` itself. `DiffExitEdit`
  inspects the focused new-side range on exit: if it consists solely
  of the single blank line that `DiffEnterEdit` synthesized, it is
  removed from `new_rope` before the sub-mode transition, so the
  hunk cleanly reverts to `Delete`. This check is **purely a
  content check at exit time**, not a "did the user type?" history
  check — so the user-types-then-Ctrl-Z-back-to-blank-then-Esc
  sequence also reverts cleanly to `Delete` (the new-side range is
  back to the single synthetic blank, regardless of intervening
  history). If the user typed anything that survives to Esc time
  (even just whitespace beyond the synthetic blank), the line stays
  and the hunk remains `Replace`. Treating the synthesis as part of
  the sub-mode transition rather than as an `Edit` op keeps `Ctrl-Z`
  semantics clean: undoing inside Edit reverses *user* typing only,
  never the entry into Edit itself.

The status-bar mode badge renders `DIFF` in Review and `DIFF·EDIT` in
Edit so the active sub-mode is always visible. **Adjacent to the
badge, the status bar shows a progress indicator** —
`<resolved>/<total>` (e.g. `4/7`) where `resolved` is the number of
hunks whose `Decision != Pending` and `total` is `hunks.len()`. It
counts *up* as the user works: `0/n` on entry, climbing to `n/n` once
every hunk is resolved (at which point the `DiffResolveConfirmModal`
fires). The count-up form reads as a progress meter — how far through
the review the user is — rather than an ambiguous countdown. It still
gives a feedback loop when skipping: `DiffNext` without a decision
leaves the hunk `Pending`, so the count stalls rather than advancing,
signalling that a hunk is being deferred rather than acted on. The
counter updates after every decision, edit-driven recompute, and
undo/redo. Rendered with `theme.status_bar_diff` so it inherits the
diff-mode bar color. Backed by `DiffState::resolved_count()` and
surfaced via `StatusBarState::diff_progress: Option<(resolved,
total)>`.

> **Focus de-emphasis — partially shipped.** The base view now
> de-saturates the add/delete backgrounds on *non-focused* hunks: they
> use `theme.diff_add_line_unfocused` / `diff_delete_line_unfocused`
> (a 0.12 blend vs. the focused hunk's 0.30), so the focused hunk's
> color stands out among several changes without any per-line
> foreground override. This resolves the second open question below
> ("de-saturate the bg on non-focused hunks?" — yes) in preference to
> the originally-sketched `theme.diff_unfocused_dim` per-line fg
> approach.
>
> Still deferred: dimming the *context* (unchanged) text outside the
> focused hunk, and whether such dimming should apply in Edit only
> (focusing the *editable* region) or also in Review (focusing the
> *decision* region). Defer until we can evaluate whether the
> focused/unfocused bg split already draws the eye enough on its own.

## 5. Rendering

New `src/ui/diff_view.rs`, a `StatefulWidget` similar to `PreviewView`,
**raw-only** (no markdown rendering — interleaving two `ParsedDoc`s for
old and new content is significant complexity for marginal gain; the
strong status/hint coloring + `DIFF` badge gives the mode signal).

```rust
pub struct DiffView<'a> {
    pub diff: &'a DiffState,
    pub theme: &'a Theme,
    pub viewport_width: usize,
}

pub struct DiffViewState {}   // empty: the flat visual-line list lives
                              // on DiffState's layout cache (see below)
```

`DiffView` borrows `&DiffState` (which carries `old_rope`,
`new_buffer`, `hunks`, `decisions`, `focused_id`, `cursor`, and
`sub_mode`) and `&Theme`. The `EditorView` dispatch passes these
from `state.diff.as_ref().unwrap()` and `state.theme`. `DiffViewState`
is stored on `EditorViewState` alongside the existing `PreviewState` /
`RenderedViewState` / `RawViewState`, but it is now **empty**: the flat
visual-line list and the per-width row-count cache live on `DiffState`
itself (`src/diff/layout.rs`, behind a `RefCell`), built once per
layout version and shared by the renderer and the scroll arithmetic.
See the visual-row model below.

For each visible visual row, the widget emits a `Line<'static>` with:

- **Unchanged context** — borrowed from `new_rope`, no `Line.style`.
- **Delete-side lines** — from `old_rope`. `Line.style` is `theme.diff_delete_line` when the line belongs to the focused hunk, else the weaker `theme.diff_delete_line_unfocused`.
- **Add-side lines** — from `new_rope`. `Line.style` is `theme.diff_add_line` (focused) or `theme.diff_add_line_unfocused` (non-focused), with per-`Span` overrides on the inline-changed word ranges. The inline style is *also* focus-aware: a focused hunk uses `theme.diff_add_inline` / `diff_delete_inline` (saturated, darkened-toward-`bg`, bold), while a non-focused hunk uses the muted `theme.diff_add_inline_unfocused` / `diff_delete_inline_unfocused` (a surface-derived tint, no bold) so the changed word recedes with the rest of the dimmed hunk instead of popping. The focused full-line fill is pulled a shade darker (blended toward `bg`, see §9) so the saturated inline-change highlight has enough contrast to read against the surrounding row.
- **Decision divider** — a synthetic line (no backing rope line) emitted at the old/new boundary of every hunk via `DiffLineSource::Decision`, so it sits below a delete-only hunk and above an insert-only one. Its text is focus-aware (`decision_divider_text()` in `src/ui/diff_view.rs`): a **non-focused** divider is the bare checkbox plus a resolved label (`[ ]` while Pending, `[Y] Accepted`, `[N] Rejected`); the **focused** divider prepends a `>` caret and, while Pending, spells the keys inline — `> [ ] Accept [y] · Reject [n]` — with the `[y]` / `[n]` glyphs pulled from the shared `diff_keys` table via `crate::input::diff_hint` so the prompt can never name a key the handler doesn't honor. The base label glyphs (`[ ]` / `[Y]` / `[N]` / "Accepted" / "Rejected") still come from `decision_line_text()` in `src/diff/layout.rs`. Every divider also ends with a `(i/n)` position counter (1-based hunk index in document order over the total hunk count) — a separate span dimmed with `Modifier::DIM` (and the inherited bold cleared) so the index reads as quiet metadata; `DIM` rather than a muted color keeps it recessive in monochrome themes too. The counter is a *position* index, distinct from the status bar's `resolved/total` *progress* chip. The divider is a single-row status strip: it renders with `wrap = false` and is pinned to one row in the layout row cache, so the longer focused prompt never perturbs scroll math when focus moves. The background is a **neutral chrome surface**, not a `secondary` tint, so the colored foregrounds keep full contrast: the **focused** divider uses the per-state `theme.diff_decision_pending` / `diff_decision_accepted` / `diff_decision_rejected` on the heavier `surface_elevated` (set as the line's base style so the trailing-cell fill paints the whole row) plus an added bold — the focused-Pending fg is `secondary` so the inline prompt pops, the resolved states keep their green/red hue. A **non-focused** divider sits on the lighter `surface` so it recedes a step: `theme.diff_decision_unfocused` (muted fg, no bold) while Pending, and for `Accepted` / `Rejected` `build_line` keeps that background but swaps in the per-state green/red hue and adds `DIM`, so a resolved unfocused divider still signals its decision by color while staying quieter than the focused one. The hue is derived from the focused style (not the palette), so monochrome themes — where the focused style carries no color — keep a plain `DIM` strip and let the label text convey the state.
- **Stacked order** — old (delete) lines first, then the decision divider, then new (add) lines.
- **Focused hunk** — there is *no* gutter glyph or focus bar. Focus is shown by background intensity alone: the focused hunk's add/delete lines use `theme.diff_add_line` / `diff_delete_line` (stronger fill, itself darkened toward `bg` — see §9), while every non-focused hunk uses `theme.diff_add_line_unfocused` / `diff_delete_line_unfocused` (a fainter wash that recedes). The focused hunk's decision divider is additionally bolded and carries the `>` caret + inline prompt (see the Decision divider entry above). All lines start at column 0; the focused/unfocused background split — not a gutter — is the focus indicator for the add/delete lines, and the caret marks the focused divider even when those lines have scrolled out of view.

`render_line_from_visual` already propagates `line.style` across the
trailing cells (the same mechanism code blocks use), so a single
`Line.style` gives the full-width bg fill without changes to
`line_render`.

**Long-line wrapping.** Add-side and delete-side lines that exceed
the viewport width wrap via the same word-aware wrap used by
`PreviewView` / `RenderedView` (`line_render::visual_rows_of_str`).
Each wrapped sub-row inherits the parent line's bg style so the
diff color is unbroken across the wrap. `DiffVisualLine` therefore
represents one *logical* line; the renderer expands it into one or
more visual rows at paint time, and `EditorState.scroll` indexes
visual rows, not `DiffVisualLine` entries — mirroring how
`PreviewView` already indexes wrapped visual rows. The scroll
upper bound becomes `total_visual_rows - 1` after expansion.

Keyboard scroll remains 1 line / step (per existing memory note); mouse
wheel honours the configured step.

### Visual-row model

Unlike `PreviewView` / `RenderedView` / `RawView`, where visual rows
correspond 1:1 to lines from a single rope, `DiffView` interleaves
lines from two ropes and inserts stacked hunk pairs. The widget
materializes a flat sequence of `DiffVisualLine` entries covering the
entire document:

```rust
pub struct DiffVisualLine {
    pub source: DiffLineSource,
    pub rope_line: usize,
    pub hunk_idx: Option<usize>,
}

pub enum DiffLineSource {
    Context,     // unchanged line, borrowed from new_rope
    OldDelete,   // delete-side line, borrowed from old_rope
    NewAdd,      // add-side line, borrowed from new_rope
    Decision,    // synthetic divider carrying the accept/reject
                 //   checkbox at the old/new boundary; no backing
                 //   rope line (text derived from the live Decision)
}
```

The sequence is built by walking hunks in order. Between hunks,
emit `Context` lines from `new_rope`. For each hunk, emit
`OldDelete` lines (from `old_rope[hunk.old_lines]`), the synthetic
`Decision` divider, then `NewAdd` lines (from
`new_rope[hunk.new_lines]`). For `HunkKind::Insert` there are no
`OldDelete` lines; for `HunkKind::Delete` there are no `NewAdd`
lines; the `Decision` divider is always emitted, so it lands below a
delete-only hunk and above an insert-only one.

`DiffView` renders the visible window of this sequence. The full
sequence is **built once and cached on `DiffState`'s layout cache**
(`src/diff/layout.rs`), not rebuilt per frame: the layout is
invariant for a review (decisions and focus don't change the line set
or its wrapping), so it is memoised behind a `RefCell` together with a
small LRU of per-width row-count prefix sums that answer
total-row / scroll-position queries in `O(1)` / `O(log N)`. A CP6 Edit
that reshapes the hunk list calls `DiffState::invalidate_layout()` to
force a rebuild. The build is `O(total lines in file)` — negligible
for typical markdown files, and now paid at most once per (layout
version, width) rather than every event-loop iteration.

**Scroll reuse.** Diff mode reuses `EditorState.scroll` — the field
is already documented as "scroll offset in visual rows for the active
mode" and carries no rope-line-specific semantics. In diff mode its
value is an index into the `DiffVisualLine` vec.
`Action::ScrollUp` / `ScrollDown` in diff mode increment/decrement
this field within `[0, visual_lines.len().saturating_sub(1)]`; mouse
scroll routes through the same field with the configured step.
`enter_diff_mode()` saves the pre-diff scroll value (so `RawView` /
`RenderedView` resume where they left off) and resets `scroll` to 0;
`exit_diff_mode()` restores the saved value.

**Edit-mode cursor mapping.** `Cursor` stays as the existing
`{ offset: usize, preferred_col: usize }` type used everywhere else
in the editor — no `rope_line` field is added. In Edit sub-mode
the cursor offset is an index into `new_rope` (via the diff-Edit
branch of `apply_delta`, §4a). To find the cursor's visual row,
compute `line = new_rope.char_to_line(cursor.offset)` and scan the
`DiffVisualLine` vec for the entry with `source == NewAdd` and
`rope_line == line`. The reverse mapping (click on a visual row →
rope offset) uses the same vec.

**Cursor motion semantics.** Diff-Edit cursor motion uses the
same algorithm as the rest of the editor and **respects
`EditorConfig::visual_line_nav`**. When `visual_line_nav = true`
(default), `MoveUp` / `MoveDown` use `move_up_visual` /
`move_down_visual` (visual rows, accounting for word-wrap); when
`false`, they step by rope lines. Diff-Edit does *not* impose a
different motion algorithm — it adds only the clamp described next.

1. **Hunk clamp expressed in char  offsets via `char_to_line`.**
   Define the focused hunk's allowed offset range as the set of
   char offsets `o` for which `new_rope.char_to_line(o)` falls
   within the focused hunk's `new_lines` half-open range. The
   clamp is implemented as:

   ```rust
   fn clamp_to_focused_hunk(
       new_rope: &Rope,
       focused: &Hunk,
       candidate: usize,
   ) -> usize {
       let max_offset = new_rope.len_chars();
       let candidate = candidate.min(max_offset);
       let line = new_rope.char_to_line(candidate);
       if line < focused.new_lines.start {
           // Snap to first offset of the focused hunk's first line.
           new_rope.line_to_char(focused.new_lines.start)
       } else if line >= focused.new_lines.end {
           // Snap to the last in-range offset — i.e. the position
           // immediately before the trailing newline of the hunk's
           // last line (line index `end - 1`). For a half-open range
           // [start, end), that offset is `line_to_char(end) - 1` if
           // the line `end - 1` ends with a newline (the normal
           // case), and `line_to_char(end)` (= `len_chars`) if the
           // hunk's last line is the file's final line with no
           // trailing newline. We compute the candidate as
           // `line_to_char(end).saturating_sub(1)` and then re-check
           // which line it falls on: if it dropped into line
           // `end - 1` we're good; if `line_to_char(end) == 0` (only
           // possible when end == 0, an empty hunk, which shouldn't
           // be focusable in Edit) we fall back to the hunk start.
           let snap = new_rope
               .line_to_char(focused.new_lines.end)
               .saturating_sub(1)
               .min(max_offset);
           // Guard against the no-trailing-newline EOF case where
           // line_to_char(end) already equals len_chars and the - 1
           // step puts us on the last character of the final line,
           // which is in-range — that's the intended result.
           snap.max(new_rope.line_to_char(focused.new_lines.start))
       } else {
           candidate
       }
   }
   ```

   After every cursor-motion computation, the handler calls `clamp_to_focused_hunk(...)`. If the clamped offset differs from the *pre-move* offset, the user moved within the hunk (OK). If the clamped offset equals the *pre-move* offset (i.e. the move tried to escape and was snapped back to where it started), flash "Esc to leave hunk" and write the (unchanged) clamped offset.

   This single clamp handles all motion actions — `MoveUp` /
   `MoveDown` (visual or rope-line, per the setting), `MoveLeft` /
   `MoveRight`, `MoveHome` / `MoveEnd`, `MoveDocStart` /
   `MoveDocEnd`, word motion — because each writes the cursor
   through the same path.

2. **Wrapped-row interaction (visual nav).** With
   `visual_line_nav = true`, `MoveDown` from a wrapped sub-row of
   the last new-side rope line may land on the next rope line
   (outside range) — `char_to_line(candidate) >= new_lines.end` →
   clamped, flash, no cursor motion. `MoveDown` from a wrapped
   sub-row of the *next-to-last* in-range line to a different
   sub-row of the *last* in-range line is in-range — accepted, no
   flash. `MoveUp` from the first new-side line's first sub-row
   is the symmetric boundary. Because the clamp uses
   `char_to_line` (rope-line resolution) the visual-row geometry
   is irrelevant to the clamp itself; the visual-nav algorithm
   computes the candidate offset and the clamp accepts or rejects
   it by rope-line membership.

3. **Rope-line nav.** With `visual_line_nav = false`, the same
   clamp applies. There is no separate code path.

Because the focused hunk's new-side rope lines correspond to a
contiguous run of `NewAdd` visual rows in the `DiffVisualLine` vec,
no offset within the clamped range can map onto `OldDelete` or
`Context` rows.

## 6. Undo / redo of in-diff edits + the resolution checkpoint

> **Scope note (decision undo dropped).** An earlier draft of this
> section also designed undo/redo of *decisions* (a `DiffOp::Decision`
> / `DiffOp::BulkDecision` stack recording every accept/reject/reset).
> That was dropped: decisions are recovered by navigation (`Tab` /
> `Shift-Tab` + re-decide) and `DiffResetHunk`, and the one case
> navigation can't recover — an accidental `DiffAcceptAll` /
> `DiffRejectAll` overwriting a mix of prior decisions — is guarded by
> a confirmation modal instead (CP4, §14). `DiffHistory` therefore
> records **only** in-diff *edits* (CP6), and `Action::Undo` /
> `Action::Redo` are no-ops in Review. The remainder of this section
> covers the two things that *are* implemented: Edit-text undo (CP6)
> and the single resolution-checkpoint history entry (CP4).

The one undoable mutation inside diff mode is editing the text of a
focused hunk (Edit sub-mode, CP6): it mutates `new_rope`, which then
forces a hunk re-computation that may shift later hunk indices.

### `DiffHistory`

A per-diff undo stack scoped to `DiffState`, independent of the main
`History` stack, created in CP6 with the Edit sub-mode:

```rust
pub struct DiffHistory {
    past: Vec<DiffOp>,
    future: Vec<DiffOp>,
}

pub enum DiffOp {
    /// A text edit applied to `new_rope` inside a focused hunk.
    /// Reuses the existing `EditDelta` type from `document::history`
    /// so the same insert/delete primitives are reused.
    /// `delta.offset` is an absolute `new_rope` char offset.
    /// Undo applies the inverse delta at the same absolute offset,
    /// then re-runs hunk computation.
    Edit { delta: EditDelta },
}
```

(`DiffOp` is a single-variant enum today rather than a struct so the
Edit-vs-future-op distinction stays explicit and the `match` sites
read uniformly; collapse it to a struct only if no second variant ever
appears.)

`DiffHistory::record` reuses the main `History::try_merge` logic
verbatim. Today `try_merge` is a private free function at
`src/document/history.rs:129`; as part of this work it is promoted
to `pub(crate) fn try_merge(top: &mut EditDelta, new: &EditDelta) -> bool`
in the same module so both `History::record` and
`DiffHistory::record` can call it. Do not duplicate the function
— the two copies will silently drift the first time merge rules
are tweaked. **At the same time, correct the stale docstrings on
`History::record` (`history.rs:50-52`) and `try_merge`
(`history.rs:125-127`):** both currently claim that only
"alphanumeric" single-char inserts/deletes merge, but the
implementation actually merges any contiguous single-char edit
regardless of character class. The docstrings have drifted from
the code; promoting `try_merge` to `pub(crate)` is the right time
to fix them so the next reader is not misled. See §12. The semantics: any contiguous single-char
insert (resp. delete) merges into the previous delta regardless of
character class — alphanumeric, punctuation, whitespace all merge as
long as the new edit is contiguous with the previous one. This means
that undoing a stretch of typing in Edit sub-mode reverses one
"contiguous run" at a time, not one character at a time.
`DiffExitEdit` (Esc out of Edit) breaks the merge cursor — so a
typing-Esc-Enter-typing sequence produces two separate undo groups.
This matches user intuition that "leaving the typing context"
terminates the word group. Entering Edit on a *different* hunk
(Esc → Tab → Enter) likewise starts a fresh merge group because the
sub-mode transitions (DiffExitEdit then DiffEnterEdit) both break the
merge cursor. (Decisions are not recorded in `DiffHistory` at all, so
accepting/rejecting a hunk between two typing runs has no effect on
the edit-merge cursor either way.)

### `HunkId` stability

`HunkId` is allocated monotonically at `DiffState::new`. Across
re-computations triggered by in-diff edits, IDs are preserved by
**old-side overlap matching**:

1. The engine re-runs `similar` over the (unchanged) `old_rope` and
   the (mutated) `new_rope`. For each newly-emitted hunk `n`, compute
   the overlap (in old-side lines) with each prior-pass hunk `p`:
   `overlap = max(0, min(n.old_lines.end, p.old_lines.end) - max(n.old_lines.start, p.old_lines.start))`.
2. **Match rule:** `n` inherits the `HunkId` of the prior-pass hunk
   with the largest non-zero overlap. Ties (equal overlap to two
   priors) break by smallest `p.old_lines.start`. Zero overlap with
   every prior → fresh `HunkId` with `Decision::Pending`.
3. A prior-pass hunk that no `n` matches is considered dropped; its
   `Decision` is discarded.
4. **No stale-id concern in the undo stack.** `DiffHistory` records
   only `DiffOp::Edit`, whose `delta.offset` is an absolute `new_rope`
   offset — it carries no `hunk_id`. So there is no "op references a
   hunk that no longer exists" case to skip; the matching rules here
   exist to **preserve decisions** across a recompute (reconciliation
   §11b and post-edit recompute), not to validate undo entries.
5. **Hunk merging.** An in-diff edit can cause two previously separate
   hunks to merge (e.g., adding lines between them until the context
   gap disappears). The merged hunk overlaps both priors; per rule
   (2) it inherits the id of the prior with the larger overlap, and
   the other prior's `Decision` is dropped per rule (3). In the rare
   exact-tie case the smaller-`old_lines.start` rule deterministically
   picks one. The merged hunk's `Decision` carries over from the
   inherited prior (it is *not* forcibly reset to `Pending`); this
   keeps undo coherent and matches the user's intuition that "the
   hunk I had accepted is still mostly the same hunk."
6. **The focused hunk** does not need a special rule — by
   construction it had non-zero old-side overlap with itself before
   the edit (only `new_rope` mutated) and is by far the largest
   overlap candidate, so rule (2) preserves its id. `DiffState`
   tracks `focused_id: HunkId` (not an index) across recomputations
   so the "user is typing in this hunk" pointer survives even if
   the hunk's vec index shifts.

### Recording

Every in-diff edit (Edit sub-mode, CP6) pushes a `DiffOp::Edit` onto
`past` and clears `future`, with the same contiguous-run merging as the
main `History::record` flow. **Decisions are not recorded** — accept /
reject / reset / accept-all / reject-all mutate `decisions` directly
and are not undoable (a mis-press is recovered by navigation +
re-decide or `DiffResetHunk`; the bulk case is guarded by the CP4
confirm modal, §14).

### Undo / redo dispatch

`Action::Undo` / `Action::Redo` in diff mode route to
`DiffHistory::undo` / `redo` (Edit ops only) instead of
`editor.history`; in Review they are no-ops. The main history is
paused while in diff mode — its only post-diff entry is the single
coarse merge-revert entry recorded when diff mode exits (see
"Resolution checkpoint" below).

Undo/redo of edits never leaves diff mode. To leave, the user either
decides all hunks and confirms the `DiffResolveConfirmModal` (§8), or
triggers `Action::DiffExit`.

### Edit-then-decision interaction

When an `Edit` op shifts hunk offsets, the decision attached to each
hunk follows it by `HunkId` across the recompute (the matching rules
above preserve decisions even though they aren't undoable). Undoing an
`Edit` reverses the rope mutation and re-runs hunk computation,
restoring the old hunk boundaries; the preserved decisions ride along.

### Resolution checkpoint

When the user confirms the `DiffResolveConfirmModal` (§8), the
resolved rope is swapped into `editor.buffer`. The merge-revert undo
is recorded as a **single synthetic `EditDelta`** representing the
forward "we replaced the pre-merge buffer with the merged buffer"
operation. Per `EditDelta` semantics (`removed` = was-there,
`inserted` = now-there; `History::undo` removes `inserted` and
reinserts `removed`):

```rust
EditDelta {
    offset:   0,
    removed:  old_rope.to_string(),   // pre-merge full buffer (will be restored on undo)
    inserted: resolved_text.clone(),  // post-merge full buffer (currently in editor.buffer)
}
```

This reuses the existing delta primitive — no new `HistoryEntry`
variant. `editor.history` is **cleared and replaced** with this
single entry as the sole `undo_stack` element and an empty
`redo_stack` (via the new `History::reset_with`). One `Action::Undo`
pops it: `History::undo` removes `inserted` (the merged text) and
reinserts `removed` (the pre-merge text), restoring the pre-merge
buffer. A subsequent `Action::Redo` re-applies the delta, restoring
the merged buffer. Any new edit after resolution clears `redo_stack`
as usual, so the redo path is only available until the user types
something new.

Memory cost is ~2× file size on the stack for this one entry —
acceptable for typical markdown files; if file size becomes a
concern later, this entry can be special-cased into a snapshot
variant without touching the surrounding contract.

**Post-resolution cursor placement.** Immediately after the
resolution swap (before any user input), `editor.cursor.offset` is
deterministically set to `0` (start of the merged buffer) and
`preferred_col` reset to `0`. Rationale: the cursor's pre-swap
offset was an index into `new_rope` (which no longer exists as the
buffer); naïvely keeping it would either land out-of-bounds (longer
new_rope), in arbitrary post-merge content (shorter merged result),
or — at best — at a meaningless position from the user's
perspective. Document-start is predictable and gives the user a
known anchor from which to navigate to whichever resolved region
they want to inspect. The resolution path follows the swap with
`EditorState::refresh_parsed()` so the viewport renders against
the merged rope, then `EditorState::ensure_cursor_visible(...)` to
scroll the viewport to the top.

Cursor on undo of the merge-revert entry lands at the end of the
restored pre-merge buffer (`EditDelta::undo_cursor() = offset +
removed.chars().count() = old_rope.chars().count()`), then clamped
to buffer length by the existing undo dispatch. This is acceptable:
landing at the document end after a "revert the whole merge" is no
more surprising than any other coarse undo. The post-undo undo
dispatch refreshes parsed state against the restored rope.

In-diff *edit* undo/redo is bidirectional via `DiffHistory` while the
review is open (CP6); decisions are not undoable. Once the user
resolves, `DiffHistory` is dropped and the main `History` takes over
with the single merge-revert entry described above.

`editor.dirty` is set to `true` after resolution **only if the
resolved rope differs from the on-disk content**. A cheap
byte-length check followed by a rope-equality compare against
`new_rope` (which by construction equals the on-disk content at
diff-mode entry, modulo any in-Edit-sub-mode edits) is sufficient:
if the user accepted every hunk and made no edits, the resolved
rope equals `new_rope` byte-for-byte and no save is needed. If the
user rejected every hunk and made no edits, the resolved rope
equals `old_rope` — which may or may not equal disk, so the disk
comparison is still the authoritative check. In the common
mixed-decision case the resolved rope differs from both and
`dirty = true` is correct.

If a redundant save does fire (e.g. due to a stale dirty flag or a
post-resolution edit that happens to cancel out), the watcher's
**own-write filter** (see §2 and §11) drops the resulting
`FileChanged` echo, so it is harmless — but the dirty flag should
not be set unconditionally because doing so triggers autosave (and
the modified-indicator in the status bar) for no-op resolutions.

Upon resolution and exit from diff mode, flash a success transient "Diff resolved" hint.

## 7. Theme additions

`Palette::diff_add` / `Palette::diff_delete` already exist as
`Color`s reserved for this feature. Promote them to full `Theme`
style fields and add the diff-mode signalling slots:

```rust
pub struct Theme {
    // ...
    pub diff_add_line: Style,          // focused-hunk add fill
    pub diff_delete_line: Style,       // focused-hunk delete fill
    pub diff_add_line_unfocused: Style,    // weaker tint for non-focused hunks
    pub diff_delete_line_unfocused: Style,
    pub diff_add_inline: Style,        // darkened bg, bold (word-level)
    pub diff_delete_inline: Style,
    pub diff_add_inline_unfocused: Style,    // muted word-level tint, non-focused
    pub diff_delete_inline_unfocused: Style,
    pub diff_decision_pending: Style,  // `[ ]` checkbox on the divider (bg fill)
    pub diff_decision_accepted: Style, // `[Y] Accepted` (bg fill)
    pub diff_decision_rejected: Style, // `[N] Rejected` (bg fill)
    pub diff_decision_unfocused: Style, // muted divider, non-focused (all states)
    pub status_mode_diff: Style,
    pub status_bar_diff: Style,        // saturated bg
    pub hint_bar_diff: Style,
}
```

All derive from existing `Palette` slots in `Theme::from_palette`;
all overridable via `ThemeFile` TOML (`StyleSpec` mechanism).

**Derivation recipes (default; user TOML overrides each
independently):**

| Slot | Derivation | Notes |
|---|---|---|
| `diff_add_line` | `Style::default().bg(blend(blend(surface, diff_add, 0.42), bg, 0.30))` | Focused-hunk add fill; readable behind normal text fg. The `0.42` blend toward `diff_add` is then pulled `0.30` back toward `bg` so the focused row sits a shade darker than the saturated `diff_add_inline` highlight — that contrast keeps within-line changes legible. |
| `diff_delete_line` | `Style::default().bg(blend(blend(surface, diff_delete, 0.42), bg, 0.30))` | Same idea, delete side. |
| `diff_add_line_unfocused` | `Style::default().bg(blend(surface, diff_add, 0.07))` — a much weaker tint than the 0.42 of `diff_add_line`. | Non-focused add hunks recede so the focused hunk's color stands out. |
| `diff_delete_line_unfocused` | `Style::default().bg(blend(surface, diff_delete, 0.07))` (analogous). | Non-focused delete hunks, delete side. |
| `diff_add_inline` | `Style::default().bg(blend(diff_add, bg, 0.35)).add_modifier(Modifier::BOLD)` | Darkened toward bg + bold for word-level highlights inside a *focused* add line, so light text keeps contrast. |
| `diff_delete_inline` | `Style::default().bg(blend(diff_delete, bg, 0.35)).add_modifier(Modifier::BOLD)` | Darkened toward bg + bold for word-level highlights inside a *focused* delete line. |
| `diff_add_inline_unfocused` | `Style::default().bg(blend(surface, diff_add, 0.20))` (no bold) | Word-level add highlight on a *non-focused* hunk — a surface-derived tint (like the `_line_unfocused` washes) at 0.20 vs. the 0.07 line wash, so the changed word reads as a slightly deeper patch within the faint hunk instead of popping. |
| `diff_delete_inline_unfocused` | `Style::default().bg(blend(surface, diff_delete, 0.20))` (no bold) | Same idea, delete side. |
| `diff_decision_pending` | `Style::default().fg(palette.text_muted).bg(blend(surface, secondary, 0.28))` | The bare `[ ]` checkbox while the hunk is undecided — `secondary`-tinted bg fills the whole divider row; muted fg so resolved states read as the change. |
| `diff_decision_accepted` | `Style::default().fg(palette.diff_add).bg(blend(surface, secondary, 0.28)).add_modifier(Modifier::BOLD)` | `[Y] Accepted` divider; green fg + bold echoes the add side, on the shared `secondary` strip. |
| `diff_decision_rejected` | `Style::default().fg(palette.diff_delete).bg(blend(surface, secondary, 0.28)).add_modifier(Modifier::BOLD)` | `[N] Rejected` divider; red fg + bold echoes the delete side, on the shared `secondary` strip. The focused hunk's divider gets an extra `BOLD` at render time. The bg is set as the line's base style so the trailing-cell fill spans the full row width. The three per-state styles above apply only to the *focused* hunk. |
| `diff_decision_unfocused` | `Style::default().fg(palette.text_muted).bg(blend(surface, secondary, 0.10))` | Single muted divider for *non-focused* hunks — a fainter `secondary` strip (0.10 vs. the focused 0.28), muted fg, no bold, no per-state hue. The `[Y]`/`[N]` glyph and label still convey the decision, so the divider can recede uniformly with the rest of the unfocused hunk. |
| `status_mode_diff` | `Style::default().fg(palette.surface).bg(palette.warning).add_modifier(Modifier::BOLD)` | `DIFF` / `DIFF·EDIT` badge — reuses the existing `warning` palette slot so it pops against the normal-mode badge color. |
| `status_bar_diff` | `Style::default().fg(palette.text).bg(blend(surface, diff_add, 0.22))` | Status line (bottom) gets a muted green wash, mirroring the adds-below stacking. A tint, not a fill, so the bar reads as "diff" without being mistaken for an in-document hunk and without sacrificing text legibility. |
| `hint_bar_diff` | `Style::default().fg(palette.text).bg(blend(surface_elevated, diff_delete, 0.22))` | Hint line (top) gets a muted red wash, mirroring the deletes-above stacking. |

The muted/mixed variants can be computed at theme-build time
(`Theme::from_palette`) via a small `blend(fg: Color, bg: Color, t: f32) -> Color`
helper; no new `Palette` slots are needed unless a theme author
wants explicit control, in which case they can override the derived
style directly in `ThemeFile`. `blend()` only operates on
`Color::Rgb` variants — if either input is `Indexed`, `Reset`, or a
named variant, blend cannot interpolate and falls back to the
`REVERSED` modifier (matching the monochrome path) so the diff
coloring is still visible. Built-in themes always use `Rgb`, so the
fallback only fires for user-authored themes with non-Rgb palette
slots.

**Monochrome fallback** (`Theme::from_palette(monochrome=true)`):
the focused `_line` bg slots become `Style::default().add_modifier(REVERSED)`
(swap fg/bg on the whole line), while the `_line_unfocused` slots use
`DIM` — so the three tiers (context plain / unfocused dim / focused
reversed) stay distinct without color; the focused `_inline` slots add
`BOLD` on top of `REVERSED`, while the `_inline_unfocused` slots use plain
`DIM` (matching the dimmed unfocused line). The decision divider has no
color to fall back on, so the `>` caret, the inline prompt text, the
resolved labels, and render-time `BOLD` carry the state without hue:
`diff_decision_pending` is `DIM` (the focused divider's caret + bold +
spelled-out prompt still distinguish it from the bare unfocused `[ ]`),
and `diff_decision_accepted` / `diff_decision_rejected` are `BOLD` (the
"Accepted" / "Rejected" text disambiguates the two); `diff_decision_unfocused`
is `DIM`, matching the unfocused-line tier. The status/hint diff slots fall back to
`REVERSED + BOLD` so the mode shift is still visible without color.

## 8. Modals

### `DiffIntroModal`

First-time explanatory modal. Uses the standard `ModalView` widget
(not the custom welcome-modal blit approach — we don't need pill
rows or embedded theme buttons). Title: "File changed on disk".
Body explains the stacked-line indicator and lists the diff-mode
keybindings. A `[x] Don't show this again` checkbox sits in the footer
row alongside `Continue`, joining the normal focus cycle (Tab /
Shift-Tab / arrows move focus; Enter / Space toggle vs. confirm; both
are clickable). It is a **bare** footer button — `ModalButton::bare`,
rendered *without* the `[ … ]` wrapper that `ModalView` applies to
ordinary buttons — so the `[ ]`/`[x]` glyph reads as the checkbox
itself rather than as `[ [x] … ]`, mirroring the welcome modal's
toggle. (The shared `button_row` carries the per-button `bracketed`
flag; bare buttons skip the bracket wrapper but keep the same centring,
gap, and click hit-testing.) `Continue` confirms; `Esc` also dismisses
(`dismissable: true`) — this
modal is purely informational and requires no decision from the user,
so blocking dismissal would be needlessly hostile. Dismissing without
toggling the checkbox keeps `show_diff_intro = true`; toggling and
then dismissing (via either `Continue` or `Esc`) persists the opt-out.

Opt-out persisted as `EditorConfig::show_diff_intro: bool = true`
in `~/.config/edamame/config.toml`, via `save_config_with_flash`.
Settings overlay row added under `[editor]`.

**Only one intro modal at a time.** A clean buffer stays clean during
diff review, so a second (or third) external overwrite re-enters
`enter_diff_mode` (the clean-buffer branch in `handle_file_changed`,
§11a) and recomputes the diff against the freshest disk contents.
Without a guard each re-entry would push another identical
`DiffIntroModal`, forcing the user to dismiss one per overwrite. The
push is therefore guarded on `!modal_stack.contains::<DiffIntroModal>()`
so at most one is ever on the stack. (The re-entry itself is kept — it
mirrors the dirty-conflict path's "always reconcile against the latest
disk state" behavior; only the duplicate modal is suppressed.)

### `DirtyConflictModal`

Shown *before* `DiffIntroModal` when the buffer is dirty on
file-change. Body:

> "The file has changed on disk, but you have unsaved edits in this
> buffer. How would you like to reconcile them?"

`ModalKind::Warning`, `dismissable: false` (any of the four buttons
must be chosen — there is no neutral default). Four buttons:

| Button | Action |
|---|---|
| `[Merge]` | Enter diff mode (`DiffState::new(old = buffer_text, new = on_disk_text)`). Primary / default-focused button. |
| `[Save a copy]` | Open the **existing `save_copy` modal** (`src/app/modal/save_copy.rs`) with `<stem>.local.<ext>` pre-filled as the suggested path so the user sees and can edit the destination before confirming — matches existing edamame "save a copy" UX, doesn't silently invent filenames. The save_copy modal **pushes atop** the `DirtyConflictModal` on the `ModalStack` (preserving it underneath). On the modal's confirm, write the current buffer to the chosen path, then reload the on-disk file into `editor.buffer`. Flash "Buffer saved to `<path>`" on success. If the user cancels the save_copy modal, it pops off the stack and the `DirtyConflictModal` is still there — no state change. Note: this loses the opportunity to merge — the user is saying "save my edits aside and load the disk version." |
| `[Discard & reload]` | Drop the in-memory buffer, load the on-disk file, clear `editor.history`. Destructive — this option requires a second confirmation modal ("Discard your unsaved edits? They cannot be recovered."). |
| `[Keep buffer]` | Do nothing. The buffer remains dirty; the next save overwrites the on-disk changes. Equivalent to the old "Cancel". |

`Discard & reload` is the fourth option you asked about — included
because users occasionally want exactly that behavior (the on-disk
version is canonical, my edits were experimental), but gated behind
a confirmation because it's the only destructive choice.

### `DiffResolveConfirmModal`

`ModalKind::Normal`, `dismissable: true`. Title: "Apply merged
result?". Body: a summary line showing the decision counts (e.g. "3
accepted, 1 rejected, 1 edited"). Buttons:

| Button | Action |
|---|---|
| `[Apply]` | Trigger resolution: call `resolved_rope()`, swap into `editor.buffer`, record the merge-revert entry (§6), exit diff mode, flash "Diff resolved". Primary / default-focused button. |
| `[Keep reviewing]` | Dismiss the modal and return to diff Review with all decisions intact. The user can undo decisions, change their mind, and re-trigger the modal (re-decide a hunk so the final-resolution path fires again, or press `Esc` while everything is resolved). |

`Esc` dismisses (equivalent to `[Keep reviewing]`). This is safe
because the user's decisions and edits are preserved — nothing is
lost by dismissing. The modal provides a confirmation gate that
prevents accidental resolution and gives the user a moment to review
the summary before committing.

**Exactly two entry points (`App::check_diff_resolution`).** The modal
is opened only when every hunk is decided *and* one of these fires:

1. **Resolving the final hunk as an action** — a decision
   (`DiffAcceptHunk` / `DiffRejectHunk`, via the deferred post-decision
   advance) or a bulk `DiffAcceptAll` / `DiffRejectAll` that leaves
   nothing `Pending`. The trigger is the *act* of deciding, not merely
   being in a resolved state.
2. **`Esc` (`DiffExit`) while already fully resolved** — `Esc` is gated
   on full resolution, so it opens the modal only once every hunk is
   decided; with pending hunks it is a no-op (see §9).

Hunk navigation (`DiffNext` / `DiffPrev`) deliberately never opens it:
tabbing among already-decided hunks must not pop the modal.

## 9. Actions and keymap

```rust
Action::DiffNext,
Action::DiffPrev,
Action::DiffAcceptHunk,
Action::DiffRejectHunk,
Action::DiffAcceptAll,
Action::DiffRejectAll,
Action::DiffResetHunk,    // reset focused hunk's decision → Pending
Action::DiffEnterEdit,    // Review → Edit on the focused hunk
Action::DiffExitEdit,     // Edit → Review (no decision implied)
Action::DiffExit,         // request exit of diff mode entirely
```

There is no separate "skip" action — pressing `DiffNext` without
making a decision leaves the current hunk's `Decision` as `Pending`,
which is exactly what "skip" would mean. Conflating them avoids a
redundant keybind. `DiffResetHunk` is distinct from skip: skip leaves
an *undecided* hunk `Pending` and moves on, whereas reset returns an
*already-decided* hunk to `Pending` without moving focus.

### Review sub-mode default binds

| Default key | Action |
|---|---|
| `Tab` / `Shift-Tab` | `DiffNext` / `DiffPrev` |
| `y` | `DiffAcceptHunk` (accept current hunk and advance) |
| `n` | `DiffRejectHunk` (reject current hunk and advance) |
| `Shift-Y` | `DiffAcceptAll` (accept *every* hunk; prompts a confirm modal first since it overrides any prior decisions — CP4) |
| `Shift-N` | `DiffRejectAll` (reject *every* hunk; prompts a confirm modal first since it overrides any prior decisions — CP4) |
| `Backspace` | `DiffResetHunk` (reset focused hunk's decision back to *Pending*) |
| `Enter` or `i` | `DiffEnterEdit` (enter Edit sub-mode on the focused hunk) |
| *(bound to `Action::Undo` / `Action::Redo`)* | no-op in Review (decisions are non-undoable); routed to `DiffHistory` only in Edit sub-mode (CP6) |
| `Esc` | `DiffExit` — gated on full resolution (see below); no-op while any hunk is pending |

`y` / `n` over `a` / `r` follows the convention established by `git
add -p`, `jj split`, and most terminal accept/reject prompts. With
`Tab` / `Shift-Tab` for navigation, `y` / `n` are unambiguous bare
keys — there is no double-duty. The on-screen decision indicators
reinforce the keys: a resolved hunk shows `[Y] Accepted` / `[N]
Rejected` (§5), so the checkbox glyph spells the same yes/no answer
the `y` / `n` keys record (`[ ]` while still Pending).

### Edit sub-mode binds

In Edit, the active key map is the **standard editor keymap** for the
focused hunk's clamped range, with three differences:

| Default key | Action |
|---|---|
| `Esc` | `DiffExitEdit` (exit Edit, return to Review) |
| `Tab` / `Shift-Tab` | standard `InsertTab` / list indent. **Table-cell navigation via `Tab`/`Shift-Tab` is disabled across all of `Mode::Diff` (both Review and Edit)** by the `cursor_in_table()` guard (§4a). Tab/Shift-Tab are reserved for hunk navigation in Review and for ordinary indentation in Edit; the table-mode column-cycling behavior available outside diff mode does not fire here, even when the cursor is inside a table-extent hunk's new-side range. |
| *(bound to `Action::Undo` / `Action::Redo`)* | undo / redo (routed to `DiffHistory`, including `Edit` ops) |

All other keys are normal editing. Decision keys (`y` / `n` /
`Shift-Y` / `Shift-N`) are just characters in Edit and insert as
typed.

Cursor-motion actions are clamped to the focused hunk's new-side line
range (§4). Newlines are allowed and expand the hunk downward.
**Boundary-crossing deletes** (`Action::DeleteCharBack` at the first
char of the hunk, `Action::DeleteCharForward` at the last char of the
hunk) are no-ops. Any attempt to move or delete
past the range edge flashes a status hint ("Esc to leave hunk") and
the cursor stays put.

### Hint line content

`hint_line_for` (`src/ui/bottom_region.rs`) gains a `Mode::Diff` arm
that further dispatches on the current `DiffSubMode`. Both sub-mode
hint sets are built using `chords_from(keymap, &entries)` — the same
mechanism as existing hint lines — so the displayed key labels
reflect the user's actual keybindings and stay correct after rebinding.

- **Review hint set (actions):** `DiffExit` "Exit" (*only when every hunk is resolved* — leads the row) · `DiffNext` "Next" · `DiffPrev` "Prev" · `DiffAcceptHunk` "Accept" · `DiffRejectHunk` "Reject" · `DiffAcceptAll` "Accept all" · `DiffRejectAll` "Reject all" · `DiffEnterEdit` "Edit" · `Undo` "Undo"
- **Edit hint set (actions):** `DiffExitEdit` "Done" · `Undo` "Undo" · `Newline` "Newline" · `DeleteCharBack` "Delete"

Both sets render against `theme.hint_bar_diff` so the strong
diff-mode color is preserved across both sub-modes. If the hint set
is wider than the terminal, it silently overflows (truncated on the
right) — matching existing behavior for all other modes.

### `Esc` in Review (current behavior)

`Esc` (`DiffExit`) is **gated on full resolution** — diff mode cannot
be exited while any hunk is still pending. It branches:

- **Some hunks still `Pending`:** no-op, plus an info flash ("Resolve
  every hunk before exiting diff mode"). Nothing is discarded and the
  buffer is untouched; the user must decide every hunk first. (`Quit`
  / `Ctrl+Q` remains the abandon-everything path — but it too warns
  first via `DiffQuitConfirmModal` before discarding the review; see
  §10.)
- **Every hunk decided:** open the `DiffResolveConfirmModal` (entry
  point 2 of the resolve flow, §8). A fully reviewed diff is applied
  via an explicit `[Apply]` choice — that is the exit. From the modal,
  `[Keep reviewing]` / `Esc` returns to Review; to leave a resolved
  diff *without* applying any change, `Shift-N` (reject all) then
  `[Apply]` reproduces the original text.

This makes resolution mandatory: the user reviews every hunk before
the buffer can change, and a stray `Esc` can neither silently discard
a half-finished review nor silently apply a finished one.

`Esc` in Edit sub-mode is intercepted *before* this path and simply
returns to Review — it never directly triggers diff exit.

The `Esc Exit` hint in the Review hint row is likewise gated: it is
shown only once every hunk is resolved, and leads the row (first
chord) so the now-available exit is the most prominent affordance.

> **Deferred:** a dedicated `DiffExitConfirmModal` (a `[Keep reviewing]`
> / `[Discard]` warning) so a user with a *partly* reviewed diff can
> deliberately abandon it *without quitting the whole app*. Today the
> only abandon path short of finishing the review is `Quit` (which now
> warns via `DiffQuitConfirmModal`, §10, then exits the app); revisit
> once in-diff edits (CP6) make a half-finished review more valuable.

**Modal precedence.** If any modal is open (theme picker, command
palette, intro modal, the resolve-confirm modal itself, etc.), modal
`Esc` dismissal takes precedence: the topmost modal closes and the
`DiffExit` handling does not fire. The `Esc` → `DiffExit` path only
runs when no modal is currently open and the sub-mode is `Review`.

### Keybinding overlay

A new `"Diff Review"` section is added to `CATEGORIES` in
`src/ui/keybinds_overlay/categories.rs`, following the existing
pattern. The section surfaces the diff-mode Review actions so
users can discover and rebind them:

```rust
(
    "Diff Review",
    &[
        (Action::DiffNext, "Next hunk"),
        (Action::DiffPrev, "Prev hunk"),
        (Action::DiffAcceptHunk, "Accept hunk"),
        (Action::DiffRejectHunk, "Reject hunk"),
        (Action::DiffAcceptAll, "Accept all"),
        (Action::DiffRejectAll, "Reject all"),
        (Action::DiffResetHunk, "Reset hunk"),
        (Action::DiffEnterEdit, "Edit hunk"),
        (Action::DiffExitEdit, "Exit edit"),
        (Action::DiffExit, "Exit diff"),
    ],
),
```

The section appears after the existing "Table" section in the
overlay's display order. Because review actions aren't in the runtime
`KeyMap` (they live in the `diff_keys` table — see §10), the overlay's
chord cell for these rows is populated from `diff_hint()` rather than
`KeyMap::first_key_for`; without that fallback every "Diff Review" row
would render with a blank key. Rebinding them from the overlay is inert
until CP6 moves the table into the layered keymap.

## 10. Autosave + Ctrl-S in diff mode

- `App::tick_autosave` early-returns when `editor.diff.is_some()`. The autosave deadline is suppressed from `next_deadline` so the main loop doesn't wake spuriously. Otherwise it routes through `App::save_buffer()` (§2), inheriting the own-write hash stamp automatically — autosave never calls `Buffer::save_file()` directly.
- **Layering.** `DefaultHandler` (in `src/input/mode_handler/default.rs`)
  remains a pure **key → `Option<Action>` resolver**: it looks up
  the active `KeyMap` and returns whatever `Action` the keypress
  resolves to (or `None`). It does **not** call into `edit_ops` and
  **does not branch on diff sub-mode for action effects** — its only
  diff-related responsibility is selecting *which* `KeyMap` to consult
  (see the per-sub-mode keymap layer below). All action dispatch
  lives in a single unified entry point: today there are two — the
  keystroke arm of `App::run` and `App::dispatch_palette_action` —
  and this work **unifies them into `App::dispatch_action`** (rename
  of `dispatch_palette_action` at `src/app/actions.rs:336`; the
  keystroke arm in `App::run` is collapsed to call
  `self.dispatch_action(action, ...)` instead of inlining the
  `handle_app_action` / `edit_ops::apply` flow). One dispatcher,
  one place to add the `Mode::Diff` arm.

  `DefaultHandler` gains a **per-sub-mode keymap layer** because
  Review needs bare `y` / `n` / `Tab` / `Shift-Tab` bindings that
  collide with text input, and Edit needs `Esc` → `DiffExitEdit`.
  The handler reads `state.mode` and
  `state.diff.as_ref().map(|d| d.sub_mode)` to pick which `KeyMap`
  variant to consult — Review uses a `KeyMap` derived from the
  Review keybind set (§9), Edit uses the standard `KeyMap` with
  the `Esc` override applied first. Keymap-selection is still
  pure key → `Option<Action>`; everything past that point is `App`'s
  responsibility.

- **Action dispatch in `App`.** Inside `App::dispatch_action` (the
  unified dispatcher), gate the `edit_ops::apply` fallthrough on
  diff mode:

  ```rust
  // Inside App::dispatch_action (renamed from
  // dispatch_palette_action), replacing the
  // `edit_ops::apply(...)` line on the fallthrough path
  // (src/app/actions.rs:345):
  let quit = match self.editor.mode {
      Mode::Diff => {
          let sub_mode = self.editor.diff.as_ref().unwrap().sub_mode;
          match sub_mode {
              DiffSubMode::Review => match action {
                  Action::DiffNext
                  | Action::DiffPrev
                  | Action::DiffAcceptHunk
                  | Action::DiffRejectHunk
                  | Action::DiffAcceptAll
                  | Action::DiffRejectAll
                  | Action::DiffEnterEdit
                  | Action::DiffExit
                  | Action::Undo
                  | Action::Redo => edit_ops::apply(
                      &mut self.editor, action.clone(), doc_height, doc_width,
                  ),
                  ref other => match diff_safe_action(other) {
                      Some(safe) => edit_ops::apply(
                          &mut self.editor, safe.clone(), doc_height, doc_width,
                      ),
                      None => false, // silently dropped
                  },
              },
              DiffSubMode::Edit => {
                  // Override Esc → DiffExitEdit before dispatch.
                  // (The same override applied by the per-sub-mode
                  // keymap layer above; this is a backstop for code
                  // paths that synthesize Esc-as-action directly,
                  // e.g. the modal close-fallthrough.)
                  let action = match action {
                      Action::ExitToPreview => Action::DiffExitEdit,
                      a => a,
                  };
                  // All other keys (InsertTab, cursor motion, text
                  // insertion) flow through edit_ops::apply.
                  // EditorState::apply_delta routes mutations to
                  // new_buffer + DiffHistory in DiffSubMode::Edit
                  // (§4a); cursor_in_table returns false in
                  // Mode::Diff (§4a); cursor-motion handlers clamp
                  // to the focused hunk (§4).
                  edit_ops::apply(&mut self.editor, action, doc_height, doc_width)
              }
          }
      }
      _ => edit_ops::apply(&mut self.editor, action.clone(), doc_height, doc_width),
  };
  ```

  The `Mode::Diff` arm fires *after* `handle_app_action` returns
  `false`, so app-level actions (open modal, follow link, palette
  open, etc.) still short-circuit first.

- **`Action::Save` is hoisted out of `edit_ops::apply` into `App`.**
  `edit_ops::apply`'s current `Action::Save => { state.buffer.save_file() ... }`
  arm (`src/editor/edit_ops.rs:598`) is removed. Instead,
  `App::handle_app_action` adds an `Action::Save` arm that calls a
  new `App::save_buffer()` helper. `save_buffer()` is the single
  call site for `Buffer::save_file()` across the application —
  `App::handle_app_action(Action::Save)`, the post-merge resolution
  path, and autosave (`src/app/autosave.rs`) all go through it.
  `save_buffer()` calls `editor.buffer.save_file()`, on `Ok` sets
  `editor.dirty = false`, stamps `self.last_disk_hash` via `set_disk_hash(bytes)` (§2), and
  returns. This is also where the "Resolve diff to save" flash
  fires in diff mode (early-return before touching the buffer).
- A new `diff_safe_action(action) -> Option<Action>` helper (mirroring `preview_safe_action` in `src/input/mode_handler/default.rs`) gates every action while in diff mode. **The policy is default-deny:** any `Action` not in the explicit allowlist below returns `None` and is silently dropped. This is the inverse of `preview_safe_action`'s default-allow policy — Preview is "non-destructive editor mode," Diff is "structured review mode where most editor operations are meaningless or actively unsafe."

  **Allowlist (returns `Some(action)`):**

  | Category | Actions | Notes |
  |---|---|---|
  | Diff control | `DiffNext`, `DiffPrev`, `DiffAcceptHunk`, `DiffRejectHunk`, `DiffAcceptAll`, `DiffRejectAll`, `DiffEnterEdit`, `DiffExitEdit`, `DiffExit` | Core. |
  | Diff edit-content (Edit sub-mode only) | `MoveLeft`, `MoveRight`, `MoveUp`, `MoveDown`, `MoveWordLeft`, `MoveWordRight`, `MoveHome`, `MoveEnd`, `MoveDocStart`, `MoveDocEnd`, `MoveLineUp`/`MoveLineDown` if defined, `InsertChar`, `InsertNewline`, `InsertTab`, `DeleteCharBack`, `DeleteCharForward`, `DeleteWordBack`, `DeleteWordForward`, `Paste`, `Cut`, `Copy`, `SelectAll`, selection-extend variants | Clamped to focused hunk (§4, §5). Cut/Copy/SelectAll operate on `new_buffer`. `Action::Undo` / `Action::Redo` route to `DiffHistory` (§6). |
  | Saves | `Action::SaveCopy` | Writes to a different path; never touches the in-flight diff. `Action::Save` is also `Some(Save)` so `App::save_buffer()` can fire the "Resolve diff to save" flash from its diff-mode early-return (§2) — the flash is **not** fired by `diff_safe_action` itself. |
  | Scroll | `ScrollUp`, `ScrollDown`, `ScrollPageUp`, `ScrollPageDown`, `ScrollHome`, `ScrollEnd`, `ScrollLeft`, `ScrollRight` if defined | View-only; do not modify state. |
  | Read-only overlays | `OpenSettings`, `OpenKeybinds`, `ShowMarkdownCheatSheet`, `OpenConfigFolder`, `ShowCommandPalette`, `SwitchTheme`, `CreateCustomTheme`, `ShowTerminalCapabilities`, `ShowCapabilityNotice` and similar info modals | These don't mutate the buffer or change mode away from `Diff`. |
  | Lifecycle | `Action::Quit` (with diff-aware guard, see below) | |

  **Denylist (returns `None`, silently dropped):** everything else, including but not limited to: `Save` *content-mutating fallback* (handled separately above), `Open`, `OpenRecent`, `NewFile`, `NavigateBack`, `NavigateForward`, `SwitchMode` / `TogglePreviewMode` / `ToggleRawMode` (would break the `mode == Diff ⟺ diff.is_some()` invariant), `Export*`, `InsertTable`, `InsertImage`, `InsertLink`, `InsertHorizontalRule`, `ToggleCheckbox`, `ToggleBold`/`ToggleItalic`/`ToggleStrikethrough`/`ToggleCode` (would route through the markdown formatting helpers which read from `state.buffer`, §4a), `IndentList`/`OutdentList`/`ContinueList`/`RenumberList`, `TableInsertRow`/`TableInsertColumn`/`TableDeleteRow`/`TableDeleteColumn`/`TableNextCell`/`TablePrevCell` (table navigation already blocked by the `cursor_in_table` guard, §4a — explicit denial here belt-and-braces), `Find`/`FindNext`/`FindPrev`/`Replace` (search across two ropes is meaningful but out of Phase 1 scope), `OpenFile`-class actions. The default-deny rule is the source of truth; this list is the predictable result of applying it.

  **Sub-mode refinement.** Some allowlisted actions are valid only in one sub-mode:

  - **Edit-content actions** (`MoveLeft`/`Right`/`Up`/`Down`/word/home/end, `InsertChar`, `InsertNewline`, `InsertTab`, `DeleteCharBack`/`Forward`, `Paste`, `Cut`, `Copy`, `SelectAll`, selection extends): `Some` in `DiffSubMode::Edit`, `None` in `DiffSubMode::Review` (Review has no text cursor — hunk navigation uses `Tab`/`Shift-Tab` bound to `DiffNext`/`DiffPrev`, not arrow keys; `MoveDocStart`/`MoveDocEnd` are also dropped in Review because they have no Review-meaningful target).
  - **Decision actions** (`DiffAcceptHunk`, `DiffRejectHunk`, `DiffAcceptAll`, `DiffRejectAll`): `Some` in Review, `None` in Edit (decision keys are just characters in Edit per §4a).
  - **`DiffEnterEdit`**: Review only. **`DiffExitEdit`**: Edit only.

  `diff_safe_action` takes `(action, sub_mode)` and applies these refinements. The implementation is a flat match producing `Option<Action>` — readable, exhaustive (compile error on new `Action` variants), and easy to audit.

  **`Action::Quit` guard.** `Quit` is allowlisted, but in diff mode it is **not** dispatched through the generic dirty-buffer quit guard — `dispatch_action` checks `Mode::Diff` *before* the `editor.dirty` quit check, so the diff path always wins. The reason: `editor.dirty` reflects pre-diff buffer state and the standard guard's `[Save]` path would persist the wrong (pre-merge) contents. Instead, `dispatch_diff_action(Action::Quit)` opens a diff-specific `DiffQuitConfirmModal` (`ModalKind::Warning`, body "You are reviewing changes from disk. Quitting now discards the review and every decision you've made.", buttons `[Keep reviewing]` default / `[Discard & quit]`). A review is always unapplied work, so the warning fires whenever `editor.diff.is_some()` — the user can't accidentally drop a review (or its decisions) with a stray `Ctrl+Q`. `[Discard & quit]` calls `exit_diff_mode_discarding()` (reverting to `old_rope`) and sets `should_quit`; `[Keep reviewing]` / `Esc` returns to the review. The push is guarded so a repeated `Quit` doesn't stack a second copy.

- The command palette filters its visible entries through `diff_safe_action` while in diff mode so blocked actions are not even offered (palette-invoked theme switching, settings, keybinds remain available).

## 11. File-change events while already in diff mode

> **Superseded by §11b — see there for the shipped design.**

This section originally specified a *deferred single-slot queue*: a
`FileChanged` arriving while `editor.diff.is_some()` would be recorded
as a flag (contents dropped as potentially stale), and only after the
user finished the current review would the App call
`FileWatcher::force_reconcile()` to re-read disk and re-enter the
diff flow. That mechanism was never implemented and is rejected: it
forces the user to finish reviewing a now-stale diff and then
re-review from scratch.

§11b replaces it with **live decision-preserving reconciliation** — a
mid-review `FileChanged` is folded into the existing `DiffState`
immediately, carrying forward decisions on unchanged hunks. The
`force_reconcile()` watcher primitive survives, but only for the
external-editor resume flow (§2), not for any in-diff deferral.

## 11a. Correction — clean buffers must enter diff review, not silently reload

**Status: ✅ implemented (shipped after CP3, ahead of CP4–CP6).** This
correction was briefly scheduled as a trailing "CP6," but it depends
only on CP3 (the clean path's `enter_diff_mode` is wired there) and is
a small, self-contained dispatch fix, so it landed early and is no
longer tracked as a numbered checkpoint. Checkpoints 2 and 3 shipped the
initial-change dispatch with a clean-buffer branch that *silently
reloads the buffer from disk* (`App::reload_buffer_from_disk` in
`src/app/file_changed.rs`). That contradicts the core objective stated
at the top of this document: diff mode exists to surface **every**
external change for review, and the editor must **never** silently
overwrite the buffer with the on-disk version. The motivating case —
an AI agent rewrites the document and the user wants to see what
changed — is precisely the clean-buffer case (the user hasn't typed
anything since their last save), so silently reloading throws away the
entire point of the feature.

Note this also resolves a latent internal contradiction in the plan:
§11 already specifies that the **re-entry** path after resolution does
"if clean, enter diff mode directly," while the **initial** path
(CP2/CP3) silently reloads. The two paths must agree; this section
makes the initial path match §11.

### The decision tree, corrected

`App::handle_file_changed` keeps both no-op filters unchanged — they
suppress events that genuinely have nothing to review — and changes
only the final clean-buffer branch:

1. **Wrong-path drop** (unchanged): event for a file we no longer edit
   → return.
2. **Own-write filter** (unchanged): `incoming_hash == last_disk_hash`
   → return. Our own save echo, or an external no-op rewrite.
3. **Buffer-vs-disk no-op filter** (unchanged): `incoming_hash ==
   seahash(buffer.contents())` → stamp `last_disk_hash` and return.
   Disk is byte-identical to what the user already has; there is
   nothing to show. **This is the only "skip review" case.**
4. **Stamp `last_disk_hash`** (unchanged).
5. **Dispatch (changed):**
   - **Dirty buffer** (unchanged): push `DirtyConflictModal`
     (or refresh the carried bytes on an already-open conflict-modal
     stack, exactly as today). The conflict prompt still belongs here
     because the user has unsaved edits at stake and needs the
     `[Save a copy]` / `[Discard & reload]` / `[Keep buffer]` escape
     hatches alongside `[Merge]`.
   - **Clean buffer (was: `reload_buffer_from_disk`; now: enter diff
     directly):** call `self.enter_diff_mode(change.contents)`. No
     conflict prompt — there is no unsaved work to reconcile — but the
     change is still reviewed hunk by hunk. `enter_diff_mode` already
     diffs `old = buffer.contents()` (which, for a clean buffer, equals
     the last-saved / previously-observed disk content) against
     `new = on_disk`, shows `DiffIntroModal` on first run (guarded so a
     repeated overwrite while already in diff review never stacks a
     second intro modal — see §8), and falls back to a "No differences
     to review" flash if `DiffState::new` returns `None` (cannot happen
     after filter 3, but the guard is retained defensively).

### Concrete code changes

- **`src/app/file_changed.rs`**
  - In `handle_file_changed`, replace the clean-buffer
    `self.reload_buffer_from_disk(change.contents)` call with
    `self.enter_diff_mode(change.contents)`.
  - Update the module-level doc comment: the decision-tree description
    currently ends the clean branch with "Clean buffer → reload from
    disk silently." Change it to "Clean buffer → enter diff review
    directly (never silent reload — see §11a of the plan)." Also drop
    the "CP2 stops here / CP3 wires up `[Merge]`" note now that the
    objective is corrected.
  - `reload_buffer_from_disk` is **kept** — it still backs the dirty
    modal's `[Discard & reload]` (after its confirmation sub-modal)
    and `[Save a copy]` (write edits aside, then load the disk
    version) buttons. Both are explicit, user-confirmed choices, not
    silent actions. Update its doc comment to remove "Used by the
    silent-reload path (clean buffer)" and state that its callers are
    those two dirty-modal flows.

- **Tests (`src/app/file_changed.rs` `#[cfg(test)]`)**
  - `external_change_with_clean_buffer_reloads_silently` is now
    **wrong by name and intent** — rewrite it as
    `external_change_with_clean_buffer_enters_diff`: seed a clean
    buffer, push a differing `FileChanged`, assert `app.editor.diff`
    is `Some` (or `app.editor.mode == Mode::Diff`) and that the buffer
    was **not** overwritten (it still holds the pre-change content
    until the user resolves). Assert `last_disk_hash` was stamped to
    the incoming bytes.
  - Keep `disk_equal_to_buffer_skips_modal_and_stamps_hash` and
    `own_write_echo_is_dropped` unchanged — filters 2 and 3 still
    short-circuit.
  - `external_change_with_dirty_buffer_opens_modal` and the
    child-modal refresh tests are unchanged.
  - Add a test that a clean-buffer change which is byte-identical to
    disk (filter 3) does **not** enter diff mode, to lock the
    "disk == buffer is the no-op, clean is not" distinction.

- **`DiffIntroModal` first-run copy.** The intro modal's title is
  "File changed on disk" (§8), which already reads correctly for the
  clean-entry case. No copy change required, but confirm the body text
  does not assume the user has unsaved edits.

### Why the dirty path keeps its modal

A clean buffer has nothing to lose, so review starts immediately. A
dirty buffer has two divergent sets of edits (the user's unsaved work
and the external write); `[Merge]` enters the same diff review, but the
other three buttons exist so the user can instead set their work aside
(`[Save a copy]`), abandon it for the disk version (`[Discard &
reload]`), or defer (`[Keep buffer]`). Those choices are meaningless
when the buffer is clean, which is why the clean path skips straight to
diff. Both paths honor the invariant: **the buffer is never replaced
without the user seeing the change first.**

### Non-goal / deferred

No opt-out config is added: per the objective, auto-reviewing every
external change is the intended always-on behavior, not a preference.
If a future user genuinely wants silent auto-reload of clean buffers
(vim's `autoread`-style behavior), that would be an explicit opt-in
setting (`editor.auto_reload_clean = false` by default) — deferred and
out of scope here; the default must remain "always review."

## 11b. Correction — live decision-preserving reconciliation while in diff mode (supersedes §11)

**Status: ✅ implemented (CP5).** Supersedes §11's deferred-queue
design and the wholesale-reset behavior that previously shipped.

### The problem

§11 specified that a `FileChanged` arriving while `editor.diff.is_some()`
is *queued* (flag only, contents dropped) and reconciled only *after* the
user finishes the current review. That mechanism was never implemented.
What ships today is worse: `App::handle_file_changed`
(`src/app/file_changed.rs`) has **no `editor.diff.is_some()` branch at
all**. Because the main buffer stays clean during review (edits target
`diff.new_buffer`, and §4a's `apply_delta` does not set `dirty`), a second
external write falls through to the clean branch and calls
`enter_diff_mode(new_contents)` again, which **replaces `editor.diff`
wholesale** (`EditorState::enter_diff_mode`, `src/editor/state.rs`). Every
accept/reject decision, the focused hunk, any in-Edit text, the scroll
position, and the diff undo stack are **silently discarded**, and the
review restarts at `0/n`.

Neither behavior is what we want. Deferring (§11) forces the user to
finish reviewing a now-stale diff and then re-review from scratch;
wholesale reset throws their work away. The correct behavior — specified
here — is to **fold the new disk state into the live review immediately,
carrying forward every decision the user already made on hunks that did
not change.**

### Desired behavior

When a fresh disk write arrives mid-review:

1. Recompute the diff between the (invariant) `old_rope` and the **new**
   disk contents.
2. Carry each prior decision forward onto the hunk it still applies to,
   **but only when that hunk's new-side content is byte-identical to what
   the user already reviewed.** A hunk whose new-side target the external
   write changed resets to `Pending` — the user must re-review the
   now-different change, because they never saw it.
3. Drop decisions for hunks that no longer exist (the external write
   reverted that region to match `old_rope`, so there is nothing left to
   decide there).
4. Keep focus on the same hunk if it survives; otherwise land on the
   first still-`Pending` hunk.
5. If the new disk contents are byte-identical to `old_rope` (every change
   was reverted), no hunks remain — exit diff mode cleanly.
6. Flash a transient hint so the change — and any reset hunks — is never
   silent.

### Why old-side overlap is the right matching key

`old_rope` is invariant for the entire life of a review (§3, §6): both the
initial diff and every recompute run against the same pre-change text. So
the old-side line range is a stable anchor — an external write only
changes the *new* side. Two hunks "are the same hunk" when their old-side
ranges overlap most (ties break by smallest `old_lines.start`); this is
exactly the §6 rule-2 matching already relied on for in-diff edits. Note
that the §6 matching algorithm is **not yet implemented in code** (it is
CP6 Edit-sub-mode machinery); this CP5 section delivers its matching
primitive for the first time, and CP6's post-edit recompute then reuses
it.

### The crucial difference from §6's in-diff recompute

§6's recompute is triggered by the **user** editing the new side, so
carrying a decision across the reshape "matches the user's intuition that
the hunk I accepted is still mostly the same hunk" (§6 rule 5) — keeping
the decision is correct *because the user made the change.*

An external write is the opposite: the new-side content changed **without
the user's knowledge.** Carrying an `Accepted` decision across a new-side
change would silently accept content the user never saw — a correctness
hazard. Hence the extra gate in step 2: **carry the decision only when the
matched hunk's new-side text is unchanged; otherwise reset to `Pending`.**
That gate is the one thing this section adds on top of the §6 matching
algorithm.

### Algorithm

A new method `DiffState::reconcile_with_disk(&mut self, new_disk: &str) ->
ReconcileOutcome`:

```rust
pub enum ReconcileOutcome {
    /// Hunks remain; still reviewing. `reset` counts hunks whose decision
    /// was dropped back to Pending because their new-side target changed
    /// (drives the flash wording).
    StillReviewing { reset: usize },
    /// new_disk == old_rope: nothing differs anymore. Caller exits diff.
    NoChangesRemain,
}

pub fn reconcile_with_disk(&mut self, new_disk: &str) -> ReconcileOutcome {
    // Snapshot prior state before we overwrite anything.
    let prior_hunks     = std::mem::take(&mut self.hunks);
    let prior_decisions = std::mem::take(&mut self.decisions);
    let prior_new_rope  = self.new_buffer.rope().clone();
    let prior_focused   = self.focused_id;

    let old = self.old_rope.to_string();
    let computation = compute(&old, new_disk, &mut self.ids); // reuse id allocator
    let mut hunks = computation.hunks;
    if hunks.is_empty() {
        return ReconcileOutcome::NoChangesRemain;
    }
    let new_rope = Rope::from_str(new_disk);

    let mut decisions = vec![Decision::Pending; hunks.len()];
    let mut reset = 0;
    for (i, h) in hunks.iter_mut().enumerate() {
        // §6 rule-2 overlap match against the prior hunk list.
        if let Some(p) = match_by_old_overlap(h, &prior_hunks) {
            h.id = prior_hunks[p].id;            // inherit the stable id
            let same_new = hunk_new_side_text(h, &new_rope)
                        == hunk_new_side_text(&prior_hunks[p], &prior_new_rope);
            if same_new {
                decisions[i] = prior_decisions[p];        // carry the decision
            } else if prior_decisions[p] != Decision::Pending {
                reset += 1;                               // changed target → re-review
            }
        }
    }

    self.new_buffer.set_rope(new_rope);   // Buffer::set_rope (§3)
    self.hunks = hunks;
    self.decisions = decisions;
    self.uneven_table_fallback = computation.uneven_table_fallback;

    // Keep focus if it survived; else first pending, else first hunk.
    self.focused_id = if self.hunks.iter().any(|h| h.id == prior_focused) {
        prior_focused
    } else {
        self.first_pending_id().unwrap_or(self.hunks[0].id)
    };

    // An external reshape invalidates the in-diff undo history — the ropes
    // those ops reference no longer match disk. Clear it; the external
    // change is a hard checkpoint, not an undoable step.
    self.history = DiffHistory::default();
    self.invalidate_layout();
    ReconcileOutcome::StillReviewing { reset }
}
```

Two new reusable helpers:

- `match_by_old_overlap(hunk, priors: &[Hunk]) -> Option<usize>` in
  `src/diff/engine.rs` — the §6 rule-2 overlap match (largest old-side
  overlap; ties → smallest `old_lines.start`). First concrete
  implementation of the §6 matching primitive; CP6's post-edit recompute
  calls the same function. **Insert hunks** have an empty old-side range,
  so they can't be matched by overlap length; they are instead anchored
  by their insertion *position* (a candidate Insert matches a prior
  Insert at the same old-side line). Without this, an accepted/rejected
  pure insertion would lose its decision on every subsequent external
  write — the common AI-collaboration case — which was observed in
  testing and fixed (regression: `reconcile_preserves_accepted_insertion`,
  `match_by_old_overlap_anchors_inserts_by_position`).
- `hunk_new_side_text(hunk, rope) -> String` (or a borrowed slice) — the
  hunk's new-side lines from a rope. A `Delete` hunk (empty new-side
  range) yields `""`.

### App wiring

`App::handle_file_changed` (`src/app/file_changed.rs`) gains a diff-mode
branch **before the buffer-vs-disk filter**, because in diff mode
`editor.buffer` is the pre-diff original (`== old_rope`); the existing
filter 2 would otherwise short-circuit a disk-reverts-to-original event as
a "no-op" when in diff mode that event actually means "collapse the review
and exit."

```rust
let incoming_hash = seahash::hash(change.contents.as_bytes());

// 1. Own-write / no-change echo (unchanged).
if self.last_disk_hash == Some(incoming_hash) { return; }

// 2. Already reviewing: fold the new disk state into the live review.
//    Must precede the buffer-vs-disk filter (in diff mode that filter's
//    "disk == buffer" means "changes reverted", not "no-op").
if self.editor.diff.is_some() {
    self.last_disk_hash = Some(incoming_hash);   // stamp-before-dispatch
    self.reconcile_diff_with_disk(change.contents);
    return;
}

// 3. Buffer-vs-disk short-circuit + dirty/clean dispatch (unchanged; only
//    reached when NOT already in diff mode).
...
```

```rust
fn reconcile_diff_with_disk(&mut self, new_disk: String) {
    let outcome = self.editor.diff.as_mut()
        .expect("guarded by diff.is_some()")
        .reconcile_with_disk(&new_disk);
    match outcome {
        ReconcileOutcome::StillReviewing { reset } => {
            self.editor.pending_focus_scroll = true;  // re-center on focus next frame
            self.flash(
                if reset > 0 {
                    "File changed on disk — updated hunks reset for review"
                } else {
                    "File changed on disk — review updated"
                },
                MessageKind::Info,
            );
        }
        ReconcileOutcome::NoChangesRemain => {
            self.editor.exit_diff_mode();   // restores pre_diff_scroll, clears diff
            self.flash("On-disk changes reverted — nothing to review", MessageKind::Info);
        }
    }
}
```

The reconcile path never calls `enter_diff_mode`, so no `DiffIntroModal` is
pushed (correct — we are already in diff). The own-write hash is stamped
before dispatch, matching the existing stamp-before-dispatch pattern in
`file_changed.rs`.

### Edge cases and semantics

- **Decision reverted externally.** If the user `Accepted` a change and the
  external tool then reverts that region to match `old_rope`, the hunk
  disappears and its decision is dropped. On resolution that region is
  plain context (= `old_rope`), so the merged result reflects current
  disk, not the vanished accept. Intended: a diff reconciles the pre-diff
  buffer with *current* disk; there is nothing to decide about a change
  that no longer exists.
- **In-Edit text is discarded.** Replacing `new_buffer` with the latest
  disk drops any unsaved Edit-sub-mode text. Acceptable: Edit is CP6 (not
  yet implemented), and the external write is authoritative for the new
  side. When CP6 lands, `reconcile_with_disk` must additionally force
  `sub_mode` back to `Review` and reposition the cursor, since the Edit
  cursor pointed into the now-replaced rope. (A future refinement could
  three-way-merge in-flight edits; out of scope.)
- **Edit-undo stack cleared (CP6).** Once CP6 adds `DiffHistory` for
  Edit-text undo, an external rope swap is a hard break for it; clearing
  `DiffHistory` on reconcile is the simple, safe choice. (CP5 has no
  `DiffHistory` to clear — decision undo was never implemented.)
- **Focus.** `focused_id` is preserved when the hunk survives; otherwise
  focus moves to the first still-`Pending` hunk (better than the first
  hunk — it lands the user on something needing attention).
  `pending_focus_scroll` re-centers the viewport next frame.
- **Progress chip.** The §4 `resolved/total` chip updates automatically:
  carried decisions keep `resolved` high, reset hunks lower it, so the
  user sees the count tick backward by exactly the number of hunks the
  external write disturbed — an honest signal.
- **Adjacent-insertion merge resets the decision (safe-but-coarse).**
  Decisions are per-hunk. If the user accepts an insertion and the
  external write then adds content on a line *immediately adjacent* to
  it (no unchanged line between), the line-diff coalesces both into one
  Insert hunk whose new-side text now includes the unreviewed addition.
  The new-side-text gate therefore resets it to `Pending` — correctly,
  since carrying the accept forward would silently accept content the
  user never saw. The boundary is exact: an insertion separated from the
  accepted one by **any** unchanged line (above or below) keeps its
  decision; only directly-touching insertions merge and reset. In
  markdown this is narrow — paragraphs, list items, and headings are
  blank-line-separated (the blank line is unchanged context), so the
  merge bites only on tightly-packed lines (consecutive code-block lines,
  un-spaced list items). Preserving a partial accept across a merge would
  require **sub-hunk (per-line) decision granularity**, which ripples
  through resolution, rendering, navigation, and accept/reject semantics
  and partly undercuts the "re-review changed content" guarantee — out of
  scope for Phase 1; revisit alongside the §16 Phase-2 hunk-granularity
  work if it proves worthwhile. (Pure insertions that land *elsewhere*
  are preserved — see the insert-anchor matching in `match_by_old_overlap`
  and the `reconcile_preserves_accepted_insertion` regression.)

### Tests

- `src/diff/state.rs` units:
  - `reconcile_preserves_decision_on_unchanged_hunk` — two hunks, accept
    h0 / reject h1, reconcile with disk that changes only h1's region: h0
    keeps `Accepted` and its `HunkId`; h1 → `Pending`; outcome
    `StillReviewing { reset: 1 }`.
  - `reconcile_resets_decision_when_new_side_changes` — accept a hunk,
    reconcile with disk whose new-side target for that region differs →
    `Pending`.
  - `reconcile_drops_vanished_hunk` — reconcile with disk that reverts one
    hunk's region to `old_rope` → that hunk gone, its decision discarded,
    others intact.
  - `reconcile_collapses_to_no_changes` — reconcile with `new_disk ==
    old_rope` → `NoChangesRemain`.
  - `reconcile_focus_survives_or_falls_back` — focused hunk survives →
    focus kept; focused hunk vanishes → focus first pending.
- `src/diff/engine.rs` unit: `match_by_old_overlap` largest-overlap +
  tie-break.
- `src/app/file_changed.rs` units:
  - `external_change_in_diff_preserves_decisions` — enter diff, decide a
    hunk, drive a second change through `handle_file_changed`: decision
    preserved, still in diff mode, flash recorded, `DiffState` not
    wholesale-reset.
  - `external_revert_in_diff_exits_diff` — enter diff, push a change whose
    contents equal the original buffer → exits diff mode, buffer
    untouched.
- **Repurpose** the existing
  `reentering_diff_mode_does_not_stack_a_second_intro_modal`
  (`src/app/actions.rs`): production re-entry now routes through reconcile,
  not a second `enter_diff_mode`. Keep it as a unit test of
  `enter_diff_mode`'s modal guard, and add the `handle_file_changed`-driven
  preservation test above as the realistic path.

### Supersedes

This section replaces: §11's queued-event / `force_reconcile`-after-
resolution mechanism; the originally-planned "event queue" scope item and
the queued-event re-entry tests in §13; and the single-slot queued-event
field on `App` (§12). The watcher's `force_reconcile()` primitive remains for the
external-editor flow (§2) — only the in-diff deferral is removed.

### Files touched

- `src/app/file_changed.rs` — the `diff.is_some()` branch (before
  filter 2) + `reconcile_diff_with_disk` + tests.
- `src/diff/state.rs` — `reconcile_with_disk`, `ReconcileOutcome`,
  `first_pending_id` helper, history clear + tests.
- `src/diff/engine.rs` — `match_by_old_overlap` + `hunk_new_side_text`
  (or place the latter in `state`/`layout`) + test.
- Reuses, unchanged: `compute` + `HunkIdAllocator` (`engine.rs`),
  `DiffState::ids` / `focused_idx` / `invalidate_layout`
  (`state.rs`, `layout.rs`), `Buffer::set_rope` (§3),
  `EditorState::exit_diff_mode` + `pending_focus_scroll` (`state.rs`),
  `App::flash` (`flash.rs`).

## 12. Files touched

**New files:**

- `Cargo.toml` (dep additions)
- `src/watcher.rs` + `src/watcher/file_watcher.rs` + `src/watcher/debounce.rs`
- `src/diff.rs` + `src/diff/engine.rs` + `src/diff/state.rs` + `src/diff/hunk.rs` + `src/diff/history.rs`
- `src/ui/diff_view.rs`
- `src/app/modal/diff_intro.rs`
- `src/app/modal/dirty_conflict.rs`
- `src/app/modal/dirty_conflict_discard_confirm.rs` (second-step
  confirmation for the `Discard & reload` option)
- ~~`src/app/modal/diff_exit_confirm.rs` (second-step confirmation when
  user `Esc`'s out of an in-progress diff review)~~ — **deferred** (§9):
  `Esc` is gated on full resolution, so there is no partly-reviewed
  abandon path to confirm until in-diff edits (CP6) make one worthwhile.
- `src/app/modal/diff_resolve_confirm.rs` (confirmation gate before
  applying the merged result when all hunks are decided)
- `src/app/modal/diff_bulk_confirm.rs` (CP4 — "Are you sure?" gate for
  `DiffAcceptAll` / `DiffRejectAll`; near copy of `diff_resolve_confirm.rs`)
- `src/app/modal/diff_quit_confirm.rs` (warn-before-discard gate when
  `Quit` fires during an in-progress review)
- `tests/watcher.rs`
- `tests/diff_engine.rs`
- `tests/diff_history.rs`
- `tests/diff_view.rs`

**Modified files:**

- `Cargo.toml` — add `seahash` for the own-write content-hash filter (§2)
- `src/app.rs` — `AppEvent::FileChanged`, `watcher` field, `diff_paused` flag, `last_disk_hash: Option<u64>` field (§2). *(No queued-event single-slot field — §11b reconciles mid-review changes in place rather than queuing them.)*
- `src/app/event_loop.rs` — file-change arm; own-write filter (drop incoming `FileChanged` when `seahash(contents) == last_disk_hash`, otherwise stamp the new hash before dispatching); watcher pause/resume + `force_reconcile()` call in the external-editor flow (the only `force_reconcile` caller — mid-review changes reconcile live per §11b, not via re-entry); deadline integration
- `src/app/actions.rs` — rename `dispatch_palette_action` → `dispatch_action` (unified dispatcher used by both keystroke arm in `App::run` and palette path); new `App::save_buffer()` helper (the single call site for `Buffer::save_file()`); `App::handle_app_action` gains an `Action::Save` arm that routes through it; `dispatch_action` gets the `Mode::Diff` dispatch arm wrapping `edit_ops::apply` (§10); `set_disk_hash(bytes)` helper called from save / initial load / accepted FileChanged (§2)
- `src/app/run.rs` (or wherever `App::run`'s keystroke arm lives) — collapse the inlined `handle_app_action` + `edit_ops::apply` flow to a single `self.dispatch_action(action, doc_height, doc_width)` call
- `src/app/autosave.rs` — early-return when in diff mode; routes saves through `App::save_buffer()` instead of dispatching `Action::Save` via `edit_ops::apply`
- `src/document/buffer.rs` — add `Buffer::set_rope(&mut self, rope: Rope)` setter (preserves `path`, bumps `version`, clears per-rope caches); used by §6 resolution swap. **No change to `save_file()` itself** — the own-write hash is stamped by `App::save_buffer()` after a successful `save_file()` call. `Buffer` does not know about hash filtering.
- `src/document/history.rs` — add `History::reset_with(&mut self, delta: EditDelta)` setter; promote the private `try_merge(top, new) -> bool` function to `pub(crate)` so `DiffHistory::record` (in `src/diff/history.rs`) can reuse it without duplication (§6); fix the stale "alphanumeric" docstrings on `History::record` and `try_merge` to match the implementation (any contiguous single-char edit merges)
- `src/input/mode_handler/default.rs` — per-sub-mode keymap selection in `Mode::Diff` (Review keybind set vs. standard keymap with `Esc` → `DiffExitEdit` override in Edit). Remains a pure key → `Option<Action>` resolver; all action dispatch lives in `App::dispatch_action` (§10)
- `src/editor/state.rs` — `diff: Option<DiffState>` field, `pre_diff_scroll: usize` field, enter/exit helpers, mode-aware `apply_delta` branch (§4a), `edit_target()` accessor for Undo/Redo and active-buffer reads
- `src/editor/mode.rs` — `Mode::Diff` variant
- `src/editor/edit_ops.rs` — diff actions (`DiffEnterEdit`/`DiffExitEdit`/etc.), in-hunk clamping (via `focused_offset_range`, §5), boundary-crossing delete no-ops, Undo/Redo diff-mode routing via `edit_target()`. **`Action::Save` arm removed** — saves are now `App::save_buffer()` only (§2, §10)
- `src/editor/table_edit_ops.rs` — `Mode::Diff` early-return guard in `cursor_in_table()` (§4a)
- `src/editor/list_edit/parse.rs` — `Mode::Diff` early-return guard in `current_list()` and any other entry point that reads `state.buffer` (§4a); list continuation, indent / outdent, and renumber are disabled inside diff Edit because their reads would target the wrong buffer
- `src/editor/mouse_ops/checkbox.rs` — `Mode::Diff` early-return guard in `toggle_checkbox_at` for the same reason (reads from main buffer, mutates `new_buffer` — would corrupt) (§4a mouse policy)
- `src/editor/mouse_ops/links.rs` — `Mode::Diff` early-return in `hovered_link_target` and `link_at_offset` (§4a mouse policy)
- `src/editor/mouse_ops/table_drag.rs` — `Mode::Diff` early-return guard at function top (§4a mouse policy)
- `src/editor/mouse_ops/selection.rs` — `Mode::Diff` Review-sub-mode no-op for cursor placement / drag / word-select / line-select; in Edit sub-mode, route `state.buffer` reads through `edit_target().buffer` in `select_word_at_cursor` / `select_line_at_cursor`; clamp click/drag offsets to the focused hunk via `clamp_to_focused_hunk` (§5, §4a mouse policy)
- `src/editor/mouse_ops/coord.rs` — call sites that translate click/drag coordinates to rope offsets read from the active buffer (via `edit_target().buffer` in Edit, no-op in Review); no behavior change outside diff mode (§4a mouse policy)
- `src/editor/table_edit.rs` — untouched; its writes route through `apply_delta`, and the `cursor_in_table` guard already blocks its entry points in diff mode
- `src/markdown/parse_offsets.rs` — add `BlockKind` enum + `block_ranges_by(source, keep: FnMut(BlockKind) -> bool)` shared scanner; reimplement `top_level_block_ranges` in terms of it (§3a)
- `src/config/keymap.rs` — new `Action` variants and default binds
- `src/config/sections.rs` — `EditorConfig::show_diff_intro`
- `src/config/theme.rs` — new style fields and `Theme::from_palette` derivations
- `src/ui/editor_view.rs` — dispatch to `DiffView` when `diff.is_some()`
- `src/ui/status_bar.rs` — `Mode::Diff` mode badge + colored bar
- `src/ui/bottom_region.rs` — `Mode::Diff` hint set + colored hint bar
- `src/ui/settings_overlay/rows.rs` — `show_diff_intro` toggle row
- `src/ui/keybinds_overlay/categories.rs` — new "Diff Review" section

## 13. Tests

- **Watcher** (`tests/watcher.rs`): with a `tempfile`, write the file, wait, mutate it twice within 200 ms, assert at least one `FileChanged` event arrives with the latest contents (exact event count is OS-dependent — do not assert "exactly one" in the tempfile test). Use a `FakeFileWatcher` for debounce-count assertions and non-tempfile cases to avoid filesystem flakiness.
- **Diff engine** (`tests/diff_engine.rs`): snapshot tests (`insta::assert_debug_snapshot!`) over a handful of old/new pairs, including pure insert, pure delete, replace, multi-hunk, and inline-word-only changes.
- **Diff history** (`tests/diff_history.rs`, CP6): sequence of edit / undo / redo asserts (Edit sub-mode only — decisions are not recorded). Includes the hunk-id stability case where an edit reshapes the hunk list.
- **Accept-all / reject-all confirm gate** (`tests/ui.rs`, CP4): `DiffAcceptAll` opens the confirm modal; `[Yes]` applies the bulk decision; `[No]` / `Esc` dismisses with all prior per-hunk decisions intact.
- **Merge-revert history entry** (`tests/editing.rs`, CP4): after resolving a diff, `editor.history` holds exactly one undo step; one `Undo` restores the pre-merge buffer, `Redo` re-applies the merged result.
- **Diff view rendering** (`tests/diff_view.rs`): `TestBackend` snapshot tests for the stacked old-above-new layout, the decision divider's placement at the old/new boundary (delete-only / insert-only / replace), the resolved-label text, and the inline word highlight overlay.
- **Modal flow** (extend `tests/ui.rs`): `DirtyConflictModal` → `DiffIntroModal` → diff view; opt-out via the checkbox sets `show_diff_intro = false` in the loaded config.
- **Single intro modal on re-entry** (`src/app/actions.rs` unit test): call `enter_diff_mode` twice in succession on a clean buffer (simulating two external overwrites) and assert `modal_stack.count::<DiffIntroModal>() == 1` — re-entry must not stack a second intro modal (§8).
- **Autosave gating** (extend `tests/editing.rs`): set `editor.diff = Some(...)`, advance the autosave clock past `autosave_idle_ms`, assert no save fires.
- **Own-write echo suppression** (`tests/watcher.rs`): with the content-hash filter (§2), call `App::save_buffer()`, then push a `FileChanged` whose contents equal the just-saved bytes; assert it is dropped. Push a second `FileChanged` whose contents differ by one byte; assert it is delivered. Cover the initial-load case: open a file, immediately push a `FileChanged` with identical contents, assert it is dropped (matches the just-loaded hash).
- ~~**Queued-event single-slot replace**~~ **Superseded by §11b** — there is no queue. Replaced by the reconciliation tests in §11b: `reconcile_*` units in `src/diff/state.rs` and `external_change_in_diff_preserves_decisions` / `external_revert_in_diff_exits_diff` in `src/app/file_changed.rs` (a mid-review `FileChanged` recomputes in place, preserving decisions on unchanged hunks).
- **Boundary-crossing delete no-ops** (extend `tests/diff_history.rs` or in `tests/editing.rs`): enter `DiffSubMode::Edit` on a hunk; position the cursor at the first char of the focused new-side range and send `Action::DeleteCharBack` — assert the rope is unchanged, the cursor stays put, and a status flash is recorded. Repeat at the last char with `Action::DeleteCharForward`. Repeat in the middle of the range to assert the normal case still works.
- **Cursor clamp on MoveUp / MoveDown** (extend `tests/editing.rs`): enter Edit on a multi-line hunk; from the first new-side line send `Action::MoveUp` — assert `cursor.offset` is unchanged and the flash fires. From the last new-side line send `Action::MoveDown` — same. Send `MoveUp` / `MoveDown` from interior lines to assert ordinary motion still works. Also assert `MoveLeft` at the range's first offset and `MoveRight` at the range's last offset clamp identically. Run the test matrix with `visual_line_nav = true` and `false` to cover both motion algorithms (§5).
- **Ropey line-range invariants** (`tests/diff_engine.rs`): a small unit test that pins the ropey behavior the line-range convention in §3a relies on. Construct two ropes — one with a trailing newline, one without — and assert that `rope.byte_to_line(rope.len_bytes())` returns `rope.len_lines() - 1` in *both* cases, plus the corresponding `len_lines()` values (3 for `"a\nb\n"`, 2 for `"a\nb"`). The two cases yield the same numeric formula but point at different lines (empty trailing line vs. last content line); see §3a for the consequence. The whole half-open line-range scheme rides on this; pin it as documentation in case ropey ever changes the edge.
- **Hunk-merge id stability** (extend `tests/diff_history.rs`, CP6): construct an `old_rope` / `new_rope` pair that produces two distinct hunks; set a decision on the first hunk (`Accepted`); enter Edit on the second hunk and insert enough lines that the inter-hunk context gap disappears so the two hunks merge under the next recomputation. Assert: (a) the merged hunk's `HunkId` equals the prior with larger old-side overlap (§6 rule 5); (b) the merged hunk's `Decision` is the inherited prior's, carried across the recompute (not forcibly reset to `Pending`); (c) the dropped prior's `Decision` is silently discarded. (Decisions are not in the undo stack, so there is no stale-`hunk_id`-on-undo case to assert — see §6 rule 4.)

## 14. Implementation checkpoints

Phase 1 is broken into six checkpoints. At each boundary, all tests
pass and the app builds and runs. Each checkpoint is a self-contained
PR-sized unit.

**Current status.** CP1, CP2, CP3, CP4, CP5, and the §11a clean-buffer
correction are shipped; **CP6 is not yet implemented.** The
§11a correction was originally scheduled as a trailing "CP6"; it
depends only on CP3 and landed early, so it is no longer tracked as a
numbered checkpoint (see §11a). What ships today is the full Review
flow (decide / accept-all / reject-all / resolve), raw stacked
rendering with the layout cache, the clean-and-dirty entry paths, the
quit-confirm guard, the accept-all/reject-all confirmation gate (CP4),
the single merge-revert undo entry written on diff exit (CP4), and
live decision-preserving mid-review reconciliation (§11b / CP5) — but
**no Edit sub-mode (CP6).**
Decisions are deliberately **not** undoable in Review — a mis-press is
recovered by navigating back (`Tab` / `Shift-Tab`) and re-deciding, or
via `DiffResetHunk`. The one bulk action that navigation can't recover
(`DiffAcceptAll` / `DiffRejectAll` overwriting a mix of prior
decisions) is guarded by a confirmation modal instead (CP4). In-diff
undo/redo exists only for the Edit sub-mode (CP6) and operates on
`new_rope` text, never on decisions; `Action::Undo` / `Action::Redo`
are no-ops in Review.

### Checkpoint 1 — Dispatcher unification + Save hoist (no watcher, no diff) ✅ DONE

**New files:** *(none)*

**Modified files:** `src/app/actions.rs` (rename `dispatch_palette_action` → `dispatch_action`; new `App::save_buffer()` helper — the single call site for `Buffer::save_file()` going forward; `Action::Save` arm in `handle_app_action` routes through `save_buffer()`), `src/app/event_loop.rs` (the keystroke arm lives in `dispatch_single_key`, not a separate `src/app/run.rs` — collapsed to `self.dispatch_action(...)`), `src/app/autosave.rs` (route saves through `App::save_buffer()` — the original code already bypassed `edit_ops::apply` and called `Buffer::save_file()` directly, so the change is a substitution rather than a dispatch reroute), `src/editor/edit_ops.rs` (remove `Action::Save` arm — saves are now `App::save_buffer()` only), `src/app/modal/command_palette.rs` (rename call site + tests), `tests/editing.rs` + `tests/palette.rs` (drop `edit_ops::apply(Action::Save)` from existing tests since Save no longer routes through `edit_ops::apply`; add App-level unit tests for the unified dispatch in `src/app/actions.rs` and `src/app/modal/command_palette.rs` instead, since `make_app` isn't exposed to integration tests).

**Scope:**
- **Rename `App::dispatch_palette_action` → `App::dispatch_action`** and collapse the keystroke arm in `App::run` to call it. Single unified dispatcher with no behavior change.
- **Hoist `Action::Save` out of `edit_ops::apply` into `App::save_buffer()`** — single call site for `Buffer::save_file()`. Autosave routes through it. No new hash filtering yet; this is a pure refactor that prepares the call site for CP2's hash stamp.

**Tests:** extend `tests/editing.rs` to assert (a) a palette-invoked Save and a keystroke-invoked Save go through the same `App::dispatch_action` path; (b) autosave routes through `App::save_buffer()`; (c) the existing Save snapshot tests still pass.

**Verifiable live:** application behavior is unchanged from main. This is the foundation for CP2 and is shippable on its own as a refactor PR.

### Checkpoint 2 — Watcher + DirtyConflictModal (no diff mode) ✅ DONE

**New files:** `src/watcher.rs`, `src/watcher/file_watcher.rs`, `src/watcher/debounce.rs`, `src/app/modal/dirty_conflict.rs`, `src/app/modal/dirty_conflict_discard_confirm.rs`, `tests/watcher.rs`

**Modified files:** `Cargo.toml` (add `notify = "8"`, `seahash`), `src/app.rs` (`AppEvent::FileChanged`, `watcher` field, `last_disk_hash: Option<u64>` field), `src/app/event_loop.rs` (file-change arm, own-write hash filter using `last_disk_hash`, hash-stamp on accepted events, watcher pause/resume + `force_reconcile()` around external editor), `src/app/actions.rs` (add `App::set_disk_hash()` helper; `App::save_buffer()` from CP1 gains the hash stamp on successful save)

**Scope:**
- `FileWatcher` trait + `NotifyWatcher` impl with 200 ms debounce and `force_reconcile()` (§2). Watcher worker performs all disk reads.
- `AppEvent::FileChanged` variant; event-loop arm that handles it.
- Own-write content-hash filter: `App::last_disk_hash`, stamped from three sources (initial load, successful save, accepted incoming `FileChanged`); consulted by the event-loop file-change arm (§2).
- Clean buffer → silent reload from disk. **⚠️ Superseded by §11a — this is now recognized as wrong; the clean path must enter diff review, not silently reload. Fixed by the §11a correction (shipped after CP3).**
- Dirty buffer → `DirtyConflictModal` with three working buttons: `[Save a copy]`, `[Discard & reload]` (with confirmation sub-modal), `[Keep buffer]`. The `[Merge]` button flashes "Diff mode coming soon" — it is wired in Checkpoint 3.
- Watcher pause/resume + `force_reconcile()` around external-editor suspend.

**Tests:** `tests/watcher.rs` (FakeFileWatcher debounce + tempfile integration + own-write hash echo suppression + stamp-on-accept behavior), modal button flows in `tests/ui.rs`.

**Verifiable live:** edit the open file in another editor; the modal appears with three working actions.

**CP2 deviations from the plan as written:**
- A `src/app/file_changed.rs` module hosts the file-change dispatch logic (`App::handle_file_changed`, `App::reload_buffer_from_disk`) instead of inlining it into `src/app/event_loop.rs`.  The event-loop arm is a one-line call into the new module; the dispatch decision tree (hash filter → no-diff short-circuit → dirty-check) lives in its own file alongside its tests.
- `[Save a copy]` from the dirty-conflict modal is implemented via a dedicated `DirtyConflictSaveCopyModal` (wraps `SaveCopyState` + `SaveCopyView`) rather than pushing the existing `SaveCopyModal`.  The dedicated variant carries the on-disk contents through the save path so the post-save reload is byte-identical to the watcher payload (no disk re-read race) and the parent `DirtyConflictModal` is popped on success.
- Watcher worker exposes events on a `mpsc::Sender<WatchedChange>` rather than `mpsc::Sender<AppEvent>`; a small bridge thread in `App::spawn_event_threads` forwards them as `AppEvent::FileChanged`.  Keeps the watcher unit-testable without an `App`.
- External-editor pause is implemented by calling `unwatch()` on suspend and `watch()` + `force_reconcile()` on resume (rather than a separate paused flag on the watcher).  Effect is identical: no organic events reach the main thread while the editor is in flight, and the forced reconcile picks up any external edits the editor made.

### Checkpoint 3 — Diff engine + raw DiffView + Review decisions (no edits, no undo) ✅ DONE

**New files:** `src/diff.rs`, `src/diff/engine.rs`, `src/diff/state.rs`, `src/diff/hunk.rs`, `src/ui/diff_view.rs`, `src/app/modal/diff_intro.rs`, `src/app/modal/diff_resolve_confirm.rs`, `tests/diff_engine.rs`, `tests/diff_view.rs`

**Modified files:** `Cargo.toml` (add `similar`), `src/editor/mode.rs` (`Mode::Diff`), `src/editor/state.rs` (`diff: Option<DiffState>`, enter/exit helpers), `src/config/keymap.rs` (new `Action` variants + default binds), `src/config/theme.rs` (diff style slots), `src/config/sections.rs` (`show_diff_intro`), `src/ui/editor_view.rs` (dispatch to `DiffView`), `src/ui/status_bar.rs` (`DIFF` badge + colored bar), `src/ui/bottom_region.rs` (`Mode::Diff` hint set), `src/ui/settings_overlay/rows.rs` (`show_diff_intro` toggle), `src/ui/keybinds_overlay/categories.rs` ("Diff Review" section), `src/app/autosave.rs` (early-return in diff mode), `src/input/mode_handler/default.rs` (diff-mode action dispatch, Save flash)

**Scope:**
- `compute` engine with line-level + inline word-level diff, including row-level table sub-diff (§3, §3a).
- `DiffState` (without a real `DiffHistory` — the `history` field is
  a `DiffHistory { past: vec![], future: vec![] }` placeholder and
  `record()` is never called in CP3. `Action::Undo` / `Action::Redo`
  in Review stay no-ops permanently; the placeholder is dropped in CP4
  and `DiffHistory` is reintroduced for Edit-text undo in CP6).
- `Mode::Diff` variant; `EditorState::enter_diff_mode()` / `exit_diff_mode()`.
- `DiffView` raw rendering with `DiffVisualLine` model (§5): stacked old/new with a synthetic decision divider at the boundary; focus shown by focused/unfocused background intensity (no gutter).
- `DiffIntroModal` with opt-out checkbox (§8).
- Wire `DirtyConflictModal`'s `[Merge]` button to enter diff mode.
- Actions: `DiffNext`, `DiffPrev`, `DiffAcceptHunk`, `DiffRejectHunk`, `DiffAcceptAll`, `DiffRejectAll`, `DiffExit`. Review sub-mode only — no `DiffSubMode` enum yet. `DiffEnterEdit` and `DiffExitEdit` are added to the `Action` enum and keymap in this checkpoint (consistent with the "fully defined upfront" convention) but are explicit no-ops in the dispatch — they are wired in Checkpoint 6.
- Theme additions (§7), status bar + hint bar diff coloring.
- Keybinding overlay "Diff Review" section (§9).
- Autosave + `Action::Save` gated in diff mode; `SaveCopy` allowed (§10).
- Resolution: when all decisions are non-`Pending`, show `DiffResolveConfirmModal` (§8). On confirmation, swap resolved rope into buffer, flash "Diff resolved", exit diff mode. Dismissing the modal returns to Review with all decisions intact. **In CP3, resolution writes nothing to `editor.history`.** The merge-revert entry described in §6 is introduced in CP4.

**Tests:** `tests/diff_engine.rs` (snapshot tests), `tests/diff_view.rs` (TestBackend snapshots), modal intro flow in `tests/ui.rs`, autosave gating in `tests/editing.rs`.

**Verifiable live:** full review-and-decide flow works; accept/reject each hunk, accept-all/reject-all; `Undo` is a no-op (no history yet).

**CP3 deviations from the plan as written:**
- The "per-sub-mode keymap layer" called for in §10 is implemented as a hard-coded table (`DIFF_REVIEW_BINDINGS` in `src/input/mode_handler/diff_keys.rs`) rather than a layered `KeyMap` lookup — Review keys must win over the global keymap (`Tab` → `InsertTab`), and Edit's layered keymap doesn't land until CP6.  This table is the **single source of truth** for review-mode bindings: `diff_action_for()` drives the input handler (`default.rs::diff_review_handle` now just delegates to it), and `diff_hint()` feeds every place that *displays* a review key — the bottom-bar hint row, the keybinds-overlay "Diff Review" section, the focused decision-divider prompt, and the diff-intro modal — so behavior and the advertised glyphs can't drift.  (Originally CP3 hard-coded the mapping inline in `diff_review_handle` and re-spelled the glyphs in each consumer; that duplication was consolidated into this table.)  CP6 will rework the table into a proper layered keymap when the Edit/Review split lands and rebinding becomes meaningful.
- `App::dispatch_diff_action` is the diff-mode equivalent of `edit_ops::apply` — it is invoked from `dispatch_action` after `diff_safe_action` filters the action.  This is cleaner than threading every diff action through `edit_ops::apply` (where it would need to access App state to push modals / flash hints) and matches the "App owns diff-mode dispatch" framing from §10.
- `Action::Esc` in diff Review wires straight to `Action::DiffExit` (via `diff_review_handle`).  `DiffExit` is gated on full resolution: a no-op (plus an info flash) while any hunk is still pending, or the `DiffResolveConfirmModal` once everything is decided (§8/§9).  Diff mode therefore can't be left via `Esc` until the review is complete; `Quit` (`Ctrl+Q`) is the abandon-everything path and now warns first via `DiffQuitConfirmModal` (§10) before discarding the review and quitting.  A dedicated `DiffExitConfirmModal` (deliberate-discard for the *pending* case, without quitting the app) remains deferred.
- The diff-Review hint row in the bottom bar sources its chord glyphs from `diff_hint()` (the shared `diff_keys` table) rather than `chords_from(keymap, ...)`, since review keys aren't in the runtime `KeyMap`.  The labels (`Next`, `Accept`, …) stay local to the hint row — only the glyph is single-sourced.  When CP6 moves the table to a layered keymap, the hint row switches to the keymap-driven helpers.
- `EditorState::exit_diff_mode` returns the editor to `Mode::Rendered` rather than restoring the pre-diff mode.  In CP3 this is safe because the only entry path is from `DirtyConflictModal::[Merge]`, and the user typically wants to return to editing after resolving.  If a future path enters diff mode from Preview / Raw, this should be revisited.
- Several files shipped in CP3 that the lists above omit: `src/diff/layout.rs` (new — the `DiffVisualLine` model + per-width row-count cache on `DiffState`, the "Possible Improvements" memoisation item brought forward), `src/app/diff_advance.rs` (new — the deferred post-decision focus advance, so a resolved checkbox is visible before focus jumps), `src/document/buffer.rs` (`Buffer::set_rope`, used by the resolution swap — §3 attributes this to §6 but it was needed in CP3), `src/app/actions.rs` (`dispatch_diff_action`, resolution path), `src/app/file_changed.rs` (wire `[Merge]` → `enter_diff_mode`), and `EditorState::pre_diff_scroll` / `pending_focus_scroll` fields on `src/editor/state.rs`.

### Checkpoint 4 — Accept-all / reject-all confirmation gate + diff-exit history entry ✅ DONE

**New files:** `src/app/modal/diff_bulk_confirm.rs` (the "Are you sure?" gate for `DiffAcceptAll` / `DiffRejectAll` — a near copy of the existing `DiffResolveConfirmModal` / `DiffQuitConfirmModal` "Are you sure?" modals)

**Modified files:** `src/app/actions.rs` (route `DiffAcceptAll` / `DiffRejectAll` through the new confirm modal in `dispatch_diff_action`; the merge-revert `reset_with` in `apply_diff_resolution`), `src/document/history.rs` (`History::reset_with`; promote `try_merge` to `pub(crate)` and fix its stale docstrings per §6), `src/diff/state.rs` (drop the vestigial CP3 `DiffHistory` placeholder if still present — decision undo is not implemented; `DiffHistory` proper lands with the Edit sub-mode in CP6), `tests/ui.rs` (bulk-confirm modal flow), `tests/editing.rs` (merge-revert history entry)

**Scope:**
- **Accept-all / reject-all confirmation modal.** `DiffAcceptAll` / `DiffRejectAll` no longer apply immediately; each pushes a dismissable confirm modal ("Apply *accept* / *reject* to all remaining hunks?") with `[Yes]` / `[No]`. Confirming applies the bulk decision; dismissing (`Esc` / `[No]`) returns to Review with all decisions intact. This is the replacement for decision undo/redo in the one case navigation can't recover — an accidental bulk flip that overwrites a mix of prior per-hunk decisions. Single-hunk mistakes are recovered by `Tab` / `Shift-Tab` + re-decide or `DiffResetHunk`, so no per-decision undo is added. Model the new modal on `DiffResolveConfirmModal` (same `ModalView` + struct-field `kind` / `dismissable` pattern, §8 / §"Modals, overlays" architectural notes).
- **Resolution checkpoint / diff-exit history entry:** introduce the single synthetic merge-revert `EditDelta` and the `History::reset_with(EditDelta)` setter (§6). After CP4, exiting diff mode leaves `editor.history` with that single entry as its sole undo step (replacing the CP3 behavior where exit wrote nothing to history). One `Ctrl-Z` from normal editing mode then reverts the whole merge; one `Ctrl-Y` re-applies it.
- **No decision undo/redo.** `Action::Undo` / `Action::Redo` stay no-ops in Review (the CP3 behavior is retained, not extended). The `DiffHistory` / `DiffOp` decision machinery sketched in earlier drafts is dropped; `DiffHistory` is (re)introduced for Edit-text undo only in CP6.
- **Esc / exit handling already shipped (§9) and is out of CP4 scope.** `Action::DiffExit` is gated on full resolution: pending hunks → no-op + flash; all-resolved → `DiffResolveConfirmModal`. The dedicated `DiffExitConfirmModal` for deliberately abandoning a *partly*-reviewed diff is **deferred** (§9) — CP4 does not add it.

**Tests:** bulk-confirm modal flow in `tests/ui.rs` (accept-all opens the modal; `[Yes]` applies, `[No]` / `Esc` leaves decisions intact); merge-revert entry in `tests/editing.rs` (after resolution `editor.history` holds exactly one undo step; one `Undo` restores the pre-merge buffer, `Redo` re-applies the merge).

**Verifiable live:** press accept-all / reject-all → confirmation prompt appears; confirm to apply, dismiss to keep current decisions. After resolving a diff, `Ctrl-Z` from normal mode reverts the entire merge and `Ctrl-Y` re-applies it.

**CP4 deviations from the plan as written:**
- **Tests live in unit-test modules, not the integration files the plan named.** `tests/ui.rs` and `tests/editing.rs` are integration tests that import only the public `edamame::` API; the App-level diff flows (`enter_diff_mode`, `dispatch_diff_action`, `apply_diff_resolution`, `apply_diff_bulk_decision`) all depend on the crate-private `app::test_utils::make_app`, mirroring the note already in `tests/watcher.rs` ("App-level semantics are exercised by unit tests"). So: the bulk-confirm flow (open / dismiss-keeps-decisions / confirm-overrides-and-resolves) and the merge-revert entry (`undo_depth == 1`; `Undo` restores the pre-merge buffer, `Redo` re-applies) are unit tests in `src/app/actions.rs`; the modal's own key handling is unit-tested in `src/app/modal/diff_bulk_confirm.rs`; and the `History::reset_with` primitive is unit-tested in `src/document/history.rs`.
- **The CP3 `DiffHistory` / `DiffOp::Placeholder` placeholder was fully removed** (struct, enum, the `DiffState::history` field, and its init), not merely emptied — the diff facade / module docs were updated to match. `DiffHistory` proper is reintroduced in `src/diff/history.rs` in CP6.
- **`History::record`'s docstring needed no change** — it had already been corrected (post-PR1) to describe contiguity-based merging; only `try_merge`'s docstring was stale and was rewritten. `try_merge` is now `pub(crate)` for CP6 reuse.
- **`DiffBulkConfirmModal` defaults focus to `[Yes]`** (index 0), matching `DiffResolveConfirmModal`'s "default to the action" convention: the user explicitly pressed accept-all / reject-all, so confirming is one Enter; an accidental flip is still caught by `Esc` / `[No]`. The modal is `ModalKind::Warning` (it overrides prior decisions). It carries the `Decision` to apply and renders an accept- vs reject-specific prompt.
- **Double-push guard added.** `open_diff_bulk_confirm` is a no-op when a `DiffBulkConfirmModal` is already on the stack (mirrors the `DiffQuitConfirmModal` guard), so a held / repeated `Shift-Y` can't stack duplicates.

### Checkpoint 5 — Live mid-review reconciliation (§11b) ✅ DONE

**New files:** *(none)*

**Modified files:** `src/app/file_changed.rs` (the `diff.is_some()` reconciliation branch, ordered before filter 2, + `reconcile_diff_with_disk` per §11b — replaces the §11 deferred-queue design), `src/diff/state.rs` (`reconcile_with_disk` + `ReconcileOutcome` + `first_pending_id` per §11b), `src/diff/engine.rs` (`match_by_old_overlap` + `hunk_new_side_text` per §11b), `tests/diff_view.rs` + `src/app/file_changed.rs` `#[cfg(test)]` (reconciliation tests)

**Scope:**
- Implement §11b: an external write that lands while the user is mid-review recomputes the diff **in place** instead of wholesale-resetting it. Decisions on hunks the write did not touch are preserved (matched by old-side overlap); only hunks the write actually changed reset to `Pending`.
- `DiffState::reconcile_with_disk` + `ReconcileOutcome` + `first_pending_id` helper.
- `match_by_old_overlap` + `hunk_new_side_text` in the engine — the §6 `HunkId`-stability matching primitive, implemented for the first time here; CP6's post-edit recompute reuses it.
- An external revert (disk contents == original buffer) exits diff mode with the buffer untouched.
- Supersedes §11's queued-event re-entry entirely (no queue, no after-resolution re-entry); see §11b "Supersedes".

**Tests:** `reconcile_*` units; `external_change_in_diff_preserves_decisions`; `external_revert_in_diff_exits_diff` (see §11b "Tests").

**Verifiable live:** while reviewing, edit the file again in another editor / have an agent rewrite it — the diff recomputes in place, decisions on unchanged hunks survive, and only the newly-changed hunks reset to pending.

**CP5 deviations from the plan as written:**
- **Reconciliation tests live in `src/app/file_changed.rs` `#[cfg(test)]`, not `tests/diff_view.rs`.** `tests/diff_view.rs` is an integration test importing only the public `edamame::` API, while the `handle_file_changed`-driven flow depends on the crate-private `app::test_utils::make_app` (same constraint noted for CP2/CP4). So `external_change_in_diff_preserves_decisions` and `external_revert_in_diff_exits_diff` are unit tests alongside the other `file_changed` tests; the pure `reconcile_*` cases and `match_by_old_overlap` are unit tests in `src/diff/state.rs` / `src/diff/engine.rs`.
- **No `DiffHistory` clear in `reconcile_with_disk`.** The §11b code snippet ends with `self.history = DiffHistory::default()`, but `DiffHistory` and the `DiffState::history` field were fully removed in CP4 (decision undo was dropped) and don't return until CP6. CP5 therefore has nothing to clear — the method only calls `invalidate_layout()`. A comment marks where CP6 must reinstate the history clear.
- **`hunk_new_side_text` lives in `src/diff/engine.rs`** (the plan offered `state`/`layout` as alternatives) next to `match_by_old_overlap`, since both are the shared §6 matching primitive.
- **The diff-mode branch is numbered "filter 2" in `handle_file_changed`** (inserted between the own-write filter and the former buffer-vs-disk filter, which shifts to "3"); the module doc-comment decision tree was renumbered to match. Functionally identical to "before filter 2" as the plan specified.
- **`reconcile_diff_with_disk` sets `self.needs_draw = true`** after dispatching (the plan snippet omitted it), matching `enter_diff_mode`'s convention so the reconcile repaints without waiting for a keypress.
- **`reentering_diff_mode_does_not_stack_a_second_intro_modal` was left unchanged** rather than "repurposed": it already drives `enter_diff_mode` directly (not via `handle_file_changed`), so it was already a pure unit test of the modal guard. The realistic `handle_file_changed`-driven preservation test was added separately as planned.

### Checkpoint 6 — Edit sub-mode + clamped editing + Edit undo

**New files:** `src/diff/history.rs` (`DiffHistory` / `DiffOp::Edit`, word-group merging — the file declared in §3; created here, since decision undo was dropped from CP4), `tests/diff_history.rs`

**Modified files:** `src/diff/state.rs` (`DiffSubMode`, sub-mode tracking, `DiffState::apply_edit`), `src/diff/hunk.rs` (`HunkId` stability logic), `src/editor/state.rs` (mode-aware `apply_delta` branch, `edit_target()` accessor), `src/editor/edit_ops.rs` (in-hunk edit gating, boundary-crossing delete no-ops, `DiffEnterEdit`/`DiffExitEdit`, Undo/Redo diff-mode routing), `src/ui/diff_view.rs` (Edit cursor rendering), `src/ui/status_bar.rs` (`DIFF·EDIT` badge), `src/ui/bottom_region.rs` (Edit hint set), `src/config/keymap.rs` (`DiffEnterEdit`/`DiffExitEdit` default binds)

**Scope:**
- `DiffSubMode::Review` / `DiffSubMode::Edit` enum and sub-mode dispatch.
- Mode-aware `apply_delta` branch in `EditorState` and `DiffState::apply_edit` (§4a). `edit_target()` accessor for Undo/Redo routing and active-buffer reads.
- `DiffEnterEdit` / `DiffExitEdit` actions. `DiffEnterEdit` on a `Delete` hunk converts it to `Replace` by inserting a blank line (§4).
- Hard-clamped cursor motion within the focused hunk's new-side range.
- Boundary-crossing delete no-ops (`Backspace` at start, `Delete` at end) with status flash.
- Newline insertion expands the focused hunk downward.
- Hunk re-computation with `HunkId` stability: focused hunk preserved by construction, others matched by old-side range (§6). Reuses the `match_by_old_overlap` primitive landed in CP5.
  - **Efficiency note — `hunk_new_side_text`.** The CP5 reconcile reuses `engine::hunk_new_side_text` (carry-vs-reset gate), and the CP6 post-edit recompute will call it on *every keystroke* to re-establish per-hunk new-side identity. Its current form does one transient heap allocation per line (`rope.line(idx).to_string()` into `out`). That's negligible at reconcile cadence (external write only) but becomes a per-frame cost in Edit. When wiring the recompute, switch it to the allocation-free `for chunk in rope.line(idx).chunks() { out.push_str(chunk); }` form — `RopeSlice::chunks()` yields `&str` segments straight into `out`, no intermediate `String`. Also consider whether the recompute needs to re-derive the whole new-side text at all, or can diff against the focused hunk's known range incrementally.
- `DiffHistory` + `DiffOp::Edit { delta: EditDelta }` with word-group merging (§6). This is the first time `DiffHistory` exists for real — it records Edit-text ops only, never decisions.
- Undo/Redo diff-mode dispatch (Edit sub-mode only): Edit undo applies the inverse delta + recomputes hunks (§4a, §6). Review decisions remain non-undoable.
- `DIFF·EDIT` status badge, Edit hint set.

**Tests:** `tests/diff_history.rs` (Edit op + word-group merge + undo/redo sequences), hunk-id stability across edits, boundary-clamp assertions, delete-hunk-to-replace conversion.

**Verifiable live:** enter Edit on an add hunk, type replacement text, Esc back to Review, accept; Ctrl-Z reverses word-groups; enter Edit on a delete hunk, type replacement, accept.

---

## Possible Improvements:
  - ~~The DiffViewState::visual_lines cache is rebuilt every frame; a later checkpoint could memoize on hunks change.~~ ✅ Done: the flat visual-line list and a per-width row-count cache now live on `DiffState` (`src/diff/layout.rs`, behind a `RefCell`), built once per layout version and shared by the renderer and the scroll arithmetic. `DiffState::invalidate_layout()` forces a rebuild after the CP6 Edit sub-mode reshapes the hunk list.
