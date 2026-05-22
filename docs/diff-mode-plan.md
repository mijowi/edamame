# File-change detection and inline diff mode

A plan for adding (a) a filesystem watcher that detects on-disk edits to
the open file, and (b) a "diff mode" overlay where the user reviews,
edits, accepts, or rejects each change inline before the merged result
becomes the new buffer.

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

- Detect on-disk changes to the open file, debounced over a 200 ms quiet window.
- Enter a dedicated `Mode::Diff` with strong visual signalling (status-bar / hint-bar color shift, `DIFF` mode badge, first-time explanatory modal).
- Render stacked inline diffs: deleted lines above, added lines below, with word-level inline highlights within changed pairs.
- Let the user cycle through changes, accept/reject each one individually, accept-all / reject-all, skip changes, and edit added content before accepting.
- All accept / reject / skip / edit decisions inside diff mode are undo/redo-able through `Action::Undo` / `Action::Redo`.
- After all changes are resolved, automatically swap the merged result into the live buffer and exit diff mode.
- Disable autosave and `Action::Save` (`Ctrl-S` keybind and 'Save file' action in command palette) while in diff mode to avoid clobbering the on-disk file mid-review.
- Architect the watcher so a future multi-tab refactor only has to swap `Option<Box<dyn FileWatcher>>` for a per-tab map.

## Decisions (already confirmed)

   | Question | Decision |
|---|---|
| On-disk change while buffer is dirty | Warning modal, then enter diff |
| Autosave / Ctrl-S in diff mode | Both disabled |
| Diff highlighting granularity | Line-level + word-level inline |
| Multi-tab scope today | `FileWatcher` trait, single instance today |
| Accept/reject undo/redo | **Required, see §6** |

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
- A `paused: Arc<AtomicBool>` mirrors `read_paused`. The external-editor flow flips it true before suspend and false after re-entry, then drains queued `FileChanged` events. The reconciliation read is **performed by the watcher worker thread, not on the main thread.** After setting `paused = false`, the external-editor flow calls `FileWatcher::force_reconcile()`, which signals the watcher worker (via a dedicated `force_reconcile` channel or an `AtomicBool` the worker polls between events) to perform a one-shot disk read and push one fresh `FileChanged { path, contents }` onto the mpsc, bypassing the watcher's 200 ms debounce window and its event filtering. The main thread does not block on disk I/O — it returns to the event loop immediately and picks up the resulting `FileChanged` event like any other. The watcher worker stays the single owner of disk reads for this file. The reconciliation read bypasses the debounce because the debouncer's purpose is to coalesce rapid successive OS events, and a single forced read after resume has no rapid-fire concern. This matches the worker-thread model used for queued-event re-entry in §11.
- `notify::Event` variants other than `Modify` and `Create` are ignored. In particular, `Remove` events (file deletion) do not trigger diff mode — deleted-file handling is out of scope and will be addressed separately.
- **Own-write filter (content-hash).** A timestamp-based filter is unreliable on slow filesystems (NFS, SSHFS) where write-then-inotify latency can exceed 500 ms and on fast ones where rapid successive saves overlap the window. Instead we keep a content hash of the last-observed-on-disk file in memory and compare incoming `FileChanged` payloads against it. Concretely:
  - `App` carries `last_disk_hash: Option<u64>`, computed via `seahash` (zero-dependency, very fast — sub-µs for typical markdown files).
  - **Hash is stamped from three sources, all routed through the same `set_disk_hash(bytes)` helper:**
    1. Initial file load (stamp from the bytes just loaded).
    2. Every successful save — `App::save_buffer()` (the single call site for `Buffer::save_file()`, see Q1 below) stamps after `save_file()` returns `Ok`.
    3. Every accepted `AppEvent::FileChanged { contents }` — when an incoming event survives the filter (i.e. hash differs), the event-loop arm stamps the new hash *before* dispatching the change to the dirty-check / diff-entry flow.
  - The event-loop arm that handles `AppEvent::FileChanged { contents }` computes `seahash::hash(contents.as_bytes())` and drops the event iff it equals `last_disk_hash`. A match means the on-disk bytes are byte-identical to what we last observed there — either our own save echo, or a no-op write by an external tool. Either way: nothing to reconcile.
  - **Second filter: skip diff entry when disk matches the live buffer.** Even after the own-write filter accepts the event, if `seahash(contents) == seahash(editor.buffer.contents())`, no diff would be produced (disk and in-memory buffer are byte-identical). In that case stamp `last_disk_hash` to the new value and return without entering diff mode or showing the dirty-conflict modal. This is the canonical "file was modified on disk but ends up byte-equal to what I have in memory" case (e.g. an external tool re-saved the file unchanged, or the user's edits happened to converge with an external edit). Skipping diff entry here also means `DiffState::new` is never called with a hunk list that would be empty — `enter_diff_mode` can safely assume `hunks.len() >= 1`, and `focused_id` is initialized to `hunks[0].id`.
  - Because the hash tracks "what was last on disk" (not "what we last wrote"), the false-positive case I worried about earlier — external writer reverts disk to a prior state we'd already observed — does not arise. After the prior state was observed, `last_disk_hash` was updated to that state's hash; subsequent events that re-arrive at the same hash are correctly dropped (we already showed the user what disk looks like at that hash).
  - The hash field is `Option<u64>` only for the very brief window between `App::new()` and the initial file load. After load it is always `Some` for any open file.

**Q1 resolution — where does `Action::Save` and `last_disk_hash` live?**
The hash filter state lives on `App` (option a from review). `Action::Save` is hoisted out of `edit_ops::apply` into `App::handle_app_action` (`src/app/actions.rs`), which routes through a single new helper `App::save_buffer()` — the only call site for `Buffer::save_file()` post-this-change. Autosave (`src/app/autosave.rs`) and the §6 post-merge save path also call `App::save_buffer()`. Option (b) — moving `last_disk_hash` onto `EditorState` / `Buffer` — was considered and rejected: `Buffer` has no business knowing about own-write filtering, and `EditorState` would have to reach back into the watcher-event flow on `App` to be useful.

## 3. Diff subsystem

```
src/diff.rs                       # facade
src/diff/engine.rs                # compute_hunks(old: &str, new: &str)
src/diff/state.rs                 # DiffState
src/diff/hunk.rs                  # Hunk, HunkKind, InlineSpan, Decision
src/diff/history.rs               # DiffHistory: per-diff undo/redo stack
```

```rust
/// Stable per-hunk identifier. Monotonically allocated from
/// `DiffState::next_hunk_id` at `DiffState::new` (initial pass) and
/// for every fresh hunk produced by post-edit recomputation
/// (§6 rule 2). IDs are never reused.
pub struct HunkId(u64);

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
                                   //   rendering — the gutter glyph is
                                   //   the focus indicator)
    pub hunks: Vec<Hunk>,          // recomputed after every mutation
    pub focused_id: HunkId,        // single source of truth for "which
                                   //   hunk is the user looking at"; an
                                   //   id (not an index) so it survives
                                   //   post-edit hunk-list reshapes
    pub decisions: Vec<Decision>,  // parallel to `hunks`; keyed by index
    pub history: DiffHistory,      // see §6
    next_hunk_id: u64,             // monotonic allocator for HunkId
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

`DiffState::resolved_rope()` walks `hunks` in order, picking the
old-side or new-side line range per `decisions[i]`. It is only called
when every decision is non-`Pending` — calling it with any `Pending`
decisions is a programming error; the function `debug_assert!`s and
returns `Err(DiffError::PendingDecisions)` in release so a misuse
flashes a status hint instead of crashing the TUI. When every decision becomes
non-`Pending`, a `DiffResolveConfirmModal` is shown (see §8). On
confirmation, the App calls `resolved_rope()`, swaps the result
into `editor.buffer`, clears `editor.diff`, replaces
`editor.history` with a single merge-revert entry (see §6), and
exits diff mode.

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
trailing newline of the final line). For a range that is the last
block in the file with no trailing newline, pulldown-cmark's
`TagEnd::*` offset still points one past the last byte; in `ropey`,
calling `byte_to_line(rope.len_bytes())` returns
`rope.len_lines().saturating_sub(1)` if the file ends without a
newline, and `rope.len_lines()` if it does — both are valid
half-open `end_line` values and no special-casing is needed in
either case. A hunk's old-side or new-side line range intersects a
`TableExtent` iff
`hunk.lines.start < extent.end_line && extent.start_line <
hunk.lines.end` (standard half-open overlap test).

This avoids the regex-based detector's false positives on
code-block content and correctly handles edge cases (empty cells,
escape sequences, alignment markers) that a hand-rolled regex
would miss.

When `compute_hunks` produces a `Replace`, `Insert`, or `Delete`
hunk whose old-side and/or new-side line range intersects a
detected `TableExtent` on the respective side, the hunk enters a
sub-diff. Crucially the test is against the *full table extent*,
not against the hunk's contents alone — so a hunk that only spans
data rows (with the separator outside the hunk as context) still
triggers row-level diffing.

1. Identify the contiguous old-side and new-side table-row ranges
   within the hunk.
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
3. Run `similar::TextDiff::from_lines` over just those rows.
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
    /// Use `decision_history_mut()` instead for Decision/BulkDecision
    /// undo/redo in Review.
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

    /// Accessor for Decision/BulkDecision undo/redo paths, valid in
    /// either diff sub-mode. Returns `None` outside `Mode::Diff`.
    /// Does not return a buffer or cursor — Decision ops do not
    /// mutate the rope, so there is no risk of routing a write to
    /// the wrong buffer.
    pub fn decision_history_mut(&mut self) -> Option<&mut DiffHistory> {
        self.diff.as_mut().map(|d| &mut d.history)
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
| `selection::handle_click` (cursor placement) | `mouse_ops/selection.rs` | **Edit only.** In Edit, places the text cursor within the focused hunk's new-side range (clamps via `clamp_to_focused_hunk`, §5). In Review, no-op (Review has no text cursor). | Cursor outside the hunk would break the clamp invariant; cursor in Review is meaningless. |
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
dispatch splits on sub-mode because Review only touches
`DiffHistory` (no buffer mutation) while Edit needs the full
`(buffer, cursor, history)` triple:

```rust
Action::Undo => {
    match state.mode {
        Mode::Diff => {
            let sub_mode = state.diff.as_ref().unwrap().sub_mode;
            match sub_mode {
                DiffSubMode::Review => {
                    // Decision/BulkDecision undo only — no buffer
                    // mutation, no cursor move, no hunk recompute
                    // (decisions are independent of the rope).
                    let dh = state.decision_history_mut().unwrap();
                    let _ = dh.undo_decision_only();
                    // `undo_decision_only` skips DiffOp::Edit entries
                    // (Edit ops should never appear in the stack
                    // while in Review — Edit ops are only pushed in
                    // DiffSubMode::Edit). debug_assert! enforces this.
                }
                DiffSubMode::Edit => {
                    let t = state.edit_target();
                    let ActiveHistory::Diff(dh) = t.history else {
                        unreachable!("Edit sub-mode always yields Diff history");
                    };
                    match dh.undo(t.buffer, t.cursor) {
                        UndoResult::Decision => {}        // no buffer mutation
                        UndoResult::BulkDecision => {}    // no buffer mutation
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

`DiffHistory` gains a `undo_decision_only()` method that pops only
`Decision` / `BulkDecision` entries from the stack and refuses
`Edit` entries (debug-asserting they shouldn't be on the top while
in Review). The split avoids the previous design's ambiguous
`EditTarget` return in Review (where `buffer` and `history`
belonged to different ropes); now Review never touches a buffer
at all, and the type system reflects that.

`DiffHistory::undo` returns an `UndoResult` enum indicating which
variant was popped, so the handler knows whether to expect buffer
mutations. For `Decision` / `BulkDecision`, only the `decisions` vec
changes — no buffer mutation, no hunk recomputation. For `Edit`, the
inverse delta is applied to `new_buffer`, the cursor is repositioned,
and hunk recomputation fires. `Redo` is symmetric.

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

- **`Review`** (default on entry). No active text cursor. Focus is on
  the currently selected hunk, indicated by the `>` gutter glyph.
  Decision keys (`y` / `n` / `Shift-Y` / `Shift-N`) work as bare keys
  because no text is being typed. Hunk navigation: `Tab` /
  `Shift-Tab`. Entering Edit: `Enter` or `i`. Exiting diff: `Esc`
  (opens `DiffExitConfirmModal` if any decisions are pending).

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
badge, the status bar shows a pending-count indicator** —
`<pending>/<total>` (e.g. `3/7`) where `pending` is the number of
hunks whose `Decision == Pending` and `total` is `hunks.len()`. This
gives the user a feedback loop when skipping (since `DiffNext`
without a decision leaves the hunk as `Pending`, the indicator is
the only signal that a hunk is being deferred rather than acted on).
The counter updates after every decision, edit-driven recompute, and
undo/redo. Rendered with `theme.status_bar_diff` so it inherits the
diff-mode bar color.

> **Deferred polish — focus dimming.** A future enhancement could dim
> all document text outside the currently focused hunk (in Review,
> Edit, or both) to push the user's attention onto the active change.
> Implementation-wise this would be a per-line foreground-color
> override applied by `DiffView` to non-focused lines, gated by a
> `theme.diff_unfocused_dim: Style` slot. Defer until the base diff
> view ships and we can evaluate whether the strong bg colors already
> draw the eye enough on their own. The two natural questions to
> answer at that point: should dimming apply in Edit only (focusing
> the *editable* region) or also in Review (focusing the *decision*
> region)? And is per-line dimming sufficient, or should we also
> de-saturate the diff_add / diff_delete backgrounds on non-focused
> hunks?

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

pub struct DiffViewState {
    pub visual_lines: Vec<DiffVisualLine>,
}
```

`DiffView` borrows `&DiffState` (which carries `old_rope`,
`new_buffer`, `hunks`, `decisions`, `focused_id`, `cursor`, and
`sub_mode`) and `&Theme`. The `EditorView` dispatch passes these
from `state.diff.as_ref().unwrap()` and `state.theme`. `DiffViewState`
is stored on `EditorViewState` alongside the existing `PreviewState` /
`RenderedViewState` / `RawViewState` and rebuilt after every hunk
re-computation.

For each visible visual row, the widget emits a `Line<'static>` with:

- **Unchanged context** — borrowed from `new_rope`, no `Line.style`.
- **Delete-side lines** — from `old_rope`, `Line.style = Style::default().bg(theme.diff_delete_line.bg)`.
- **Add-side lines** — from `new_rope`, `Line.style = Style::default().bg(theme.diff_add_line.bg)`, with per-`Span` overrides on the inline-changed word ranges using `theme.diff_add_inline` / `diff_delete_inline` (brighter bg + bold).
- **Stacked order** — old lines first, then new lines (per spec)
- **Focused hunk** — `>` gutter glyph using `theme.diff_cursor_gutter` on the first line of the hunk. Decision indicator (`[ ]` Pending, `[✓]` Accepted, `[x]` Rejected) rendered on the first new-side line of the hunk (or first old-side line for `HunkKind::Delete` hunks). The checkmark/x glyphs differentiate decided hunks from the unresolved `[ ]` at a glance; `[✓]` for accept follows the checkbox convention already used in the editor (`[x]` for checked items), and `[x]` for reject reads as "crossed out" / "nope". The asymmetry (Unicode `✓` vs ASCII `x`) is intentional — the accept glyph is the less common decision path to draw the eye, while `x` is visually heavier and reads as a clear rejection.

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
}
```

The sequence is built by walking hunks in order. Between hunks,
emit `Context` lines from `new_rope`. For each hunk, emit
`OldDelete` lines (from `old_rope[hunk.old_lines]`), then `NewAdd`
lines (from `new_rope[hunk.new_lines]`). For `HunkKind::Insert`
there are no `OldDelete` lines; for `HunkKind::Delete` there are no
`NewAdd` lines.

`DiffView` renders the visible window of this sequence.
`DiffViewState` caches the full sequence and rebuilds it after every
hunk re-computation. The rebuild is `O(total lines in file)` — for
typical markdown files this is negligible, but if profiling shows it
matters (e.g. very large files with many hunks edited rapidly), a
future optimization could lazily compute only the visible window
plus a small margin, keyed off `scroll` and viewport height.

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

   After every cursor-motion computation, the handler calls
   `clamp_to_focused_hunk(...)`. If the clamped offset differs from
   the *pre-move* offset, the user moved within the hunk (OK). If
   the clamped offset equals the *pre-move* offset (i.e. the move
   tried to escape and was snapped back to where it started),
   flash "Esc to leave hunk" and write the (unchanged) clamped
   offset.

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

## 6. Undo / redo of accept-reject decisions and in-diff edits

The design must accommodate both *kinds* of mutation that can happen inside diff mode:

1. Toggling a hunk decision (`Pending → Accepted`, `Pending →  Rejected`, `Accepted → Pending` via undo, etc.).
2. Editing the text inside an added hunk (mutates `new_rope`, which  then forces a hunk re-computation that may shift later hunk  indices).

### `DiffHistory`

A per-diff undo stack scoped to `DiffState`, independent of the main
`History` stack:

```rust
pub struct DiffHistory {
    past: Vec<DiffOp>,
    future: Vec<DiffOp>,
}

pub enum DiffOp {
    /// Change a single hunk decision.
    /// `hunk_id` is a stable per-hunk identifier, NOT the hunk's
    /// current vec index — in-diff edits can shift indices.
    Decision {
        hunk_id: HunkId,
        before: Decision,
        after: Decision,
    },
    /// Bulk decision flip (accept-all / reject-all).
    /// Stored as a `(HunkId, Decision)` map so that one undo
    /// restores exactly the pre-bulk state in one step, *and*
    /// remains coherent after in-diff edits re-shape the hunk
    /// list. Hunk ids in `before` that no longer exist at undo
    /// time are silently dropped (same stale-id rule as
    /// `DiffOp::Decision`).
    BulkDecision {
        before: Vec<(HunkId, Decision)>,
        after: Decision,
    },
    /// A text edit applied to `new_rope` inside a focused hunk.
    /// Reuses the existing `EditDelta` type from `document::history`
    /// so the same insert/delete primitives are reused.
    /// `delta.offset` is an absolute `new_rope` char offset.
    /// Undo applies the inverse delta at the same absolute offset,
    /// then re-runs hunk computation.
    Edit { delta: EditDelta },
}
```

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
"contiguous run" at a time, not one character at a time. Decision and BulkDecision ops are never
merged. Any non-`Edit` op (`Decision`, `BulkDecision`) also breaks
an in-progress word-group merge of `Edit` ops — if the user types in
Edit, Escs to Review and accepts a hunk, then re-enters Edit and
types more, the two typing sequences are separate undo groups.
`DiffExitEdit` itself (Esc out of Edit) **also** breaks the merge
cursor — so a typing-Esc-Enter-typing sequence produces two separate
undo groups even with no intervening Decision op. This matches user
intuition that "leaving the typing context" terminates the word
group. Entering Edit on a *different* hunk (Esc → Tab → Enter)
always starts a fresh merge group because the sub-mode transitions
(DiffExitEdit then DiffEnterEdit) both break the merge cursor.

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
4. **Stale `Decision` / `BulkDecision` undo:** any `hunk_id` in a
   recorded op that no longer matches a current hunk is silently
   skipped during undo/redo. The op itself is still consumed from
   the stack so future redo behaves consistently — it just becomes a
   partial no-op for the stale ids.
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

Every accept/reject/skip/bulk and every in-diff edit pushes a
`DiffOp` onto `past` and clears `future`. Identical to the main
`History::record` flow.

### Undo / redo dispatch

`Action::Undo` / `Action::Redo` in diff mode route to
`DiffHistory::undo` / `redo` instead of `editor.history`. The main
history is paused while in diff mode — its only post-diff entry is the
single coarse "Resolved diff" event recorded when diff mode exits.

After undo/redo, if the new state has at least one `Pending`
decision, diff mode stays open; if the user undoes back to the empty
state (all `Pending`, no edits), diff mode stays open (the watcher's
"file changed" trigger has not gone away). To leave diff mode the
user must either decide all hunks and confirm the
`DiffResolveConfirmModal` (§8), or trigger `Action::DiffExit`.

### Edit-then-decision interaction

When an `Edit` op shifts hunk offsets, subsequent decisions reference
the post-edit hunk by `HunkId`. Undoing an `Edit` reverses the rope
mutation and re-runs hunk computation, restoring the old hunk
boundaries. Undoing a `Decision` does not re-run hunk computation;
it just flips `decisions[i]`.

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

In-diff undo/redo is fully bidirectional via `DiffHistory` while the
review is open. Once the user resolves, `DiffHistory` is dropped and
the main `History` takes over with the single merge-revert entry
described above.

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
    pub diff_add_line: Style,
    pub diff_delete_line: Style,
    pub diff_add_inline: Style,        // brighter bg, bold
    pub diff_delete_inline: Style,
    pub diff_cursor_gutter: Style,
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
| `diff_add_line` | `Style::default().bg(palette.diff_add_muted)` where `diff_add_muted` is a 30 %-saturated mix of `palette.diff_add` and `palette.surface`. | Subtle full-row bg fill; readable behind normal text fg. |
| `diff_delete_line` | `Style::default().bg(palette.diff_delete_muted)` (analogous mix). | Same idea, delete side. |
| `diff_add_inline` | `Style::default().bg(palette.diff_add).add_modifier(Modifier::BOLD)` | Saturated bg + bold for word-level highlights inside an add line. |
| `diff_delete_inline` | `Style::default().bg(palette.diff_delete).add_modifier(Modifier::BOLD)` | Saturated bg + bold for word-level highlights inside a delete line. |
| `diff_cursor_gutter` | `Style::default().fg(palette.primary).add_modifier(Modifier::BOLD)` | `>` glyph marking the focused hunk. |
| `status_mode_diff` | `Style::default().fg(palette.surface).bg(palette.warning).add_modifier(Modifier::BOLD)` | `DIFF` / `DIFF·EDIT` badge — reuses the existing `warning` palette slot so it pops against the normal-mode badge color. |
| `status_bar_diff` | `Style::default().fg(palette.surface).bg(palette.warning)` | Whole status bar shifts color in diff mode so the user can never miss the mode change. |
| `hint_bar_diff` | `Style::default().fg(palette.surface).bg(palette.warning_muted)` where `warning_muted` is a 60 %-saturated mix of `warning` and `surface`. | Hint bar matches status bar's hue but with a softer bg so the hint text stays readable. |

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
the `_line` bg slots become `Style::default().add_modifier(REVERSED)`
(swap fg/bg on the whole line); the `_inline` slots add `BOLD` on
top of `REVERSED`; `diff_cursor_gutter` stays as bold-`>`. The
status/hint diff slots fall back to `REVERSED + BOLD` so the mode
shift is still visible without color.

## 8. Modals

### `DiffIntroModal`

First-time explanatory modal. Uses the standard `ModalView` widget
(not the custom welcome-modal blit approach — we don't need pill
rows or embedded theme buttons). Title: "File changed on disk".
Body explains the stacked-line indicator and lists the diff-mode
keybindings. `[x] Don't show this again` checkbox rendered as a
body line, toggled with Space (mirrors welcome modal). `Continue`
button confirms; `Esc` also dismisses (`dismissable: true`) — this
modal is purely informational and requires no decision from the user,
so blocking dismissal would be needlessly hostile. Dismissing without
toggling the checkbox keeps `show_diff_intro = true`; toggling and
then dismissing (via either `Continue` or `Esc`) persists the opt-out.

Opt-out persisted as `EditorConfig::show_diff_intro: bool = true`
in `~/.config/edamame/config.toml`, via `save_config_with_flash`.
Settings overlay row added under `[editor]`.

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

Shown when the last `Pending` decision becomes non-`Pending` (i.e.
all hunks have been decided). `ModalKind::Normal`, `dismissable:
true`. Title: "Apply merged result?". Body: a summary line showing
the decision counts (e.g. "3 accepted, 1 rejected, 1 edited").
Buttons:

| Button | Action |
|---|---|
| `[Apply]` | Trigger resolution: call `resolved_rope()`, swap into `editor.buffer`, record the merge-revert entry (§6), exit diff mode, flash "Diff resolved". Primary / default-focused button. |
| `[Keep reviewing]` | Dismiss the modal and return to diff Review with all decisions intact. The user can undo decisions, change their mind, and re-trigger the modal by re-deciding the last hunk. |

`Esc` dismisses (equivalent to `[Keep reviewing]`). This is safe
because the user's decisions and edits are preserved — nothing is
lost by dismissing. The modal provides a confirmation gate that
prevents accidental resolution from a mis-pressed `y` / `n` on the
last hunk, and gives the user a moment to review the summary before
committing.

## 9. Actions and keymap

```rust
Action::DiffNext,
Action::DiffPrev,
Action::DiffAcceptHunk,
Action::DiffRejectHunk,
Action::DiffAcceptAll,
Action::DiffRejectAll,
Action::DiffEnterEdit,    // Review → Edit on the focused hunk
Action::DiffExitEdit,     // Edit → Review (no decision implied)
Action::DiffExit,         // request exit of diff mode entirely
```

There is no separate "skip" action — pressing `DiffNext` without
making a decision leaves the current hunk's `Decision` as `Pending`,
which is exactly what "skip" would mean. Conflating them avoids a
redundant keybind.

### Review sub-mode default binds

| Default key | Action |
|---|---|
| `Tab` / `Shift-Tab` | `DiffNext` / `DiffPrev` |
| `y` | `DiffAcceptHunk` (accept current hunk and advance) |
| `n` | `DiffRejectHunk` (reject current hunk and advance) |
| `Shift-Y` | `DiffAcceptAll` (accept all remaining `Pending` hunks) |
| `Shift-N` | `DiffRejectAll` (reject all remaining `Pending` hunks) |
| `Enter` or `i` | `DiffEnterEdit` (enter Edit sub-mode on the focused hunk) |
| *(bound to `Action::Undo` / `Action::Redo`)* | undo / redo (routed to `DiffHistory`) |
| `Esc` | `DiffExit` (see below) |

`y` / `n` over `a` / `r` follows the convention established by `git
add -p`, `jj split`, and most terminal accept/reject prompts. With
`Tab` / `Shift-Tab` for navigation, `y` / `n` are unambiguous bare
keys — there is no double-duty. The on-screen decision indicators
themselves use `[✓]` / `[x]` / `[ ]` glyphs (§5), not `[Y]`/`[N]`.

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

- **Review hint set (actions):** `DiffNext` "Next" · `DiffPrev` "Prev" · `DiffAcceptHunk` "Accept" · `DiffRejectHunk` "Reject" · `DiffAcceptAll` "Accept all" · `DiffRejectAll` "Reject all" · `DiffEnterEdit` "Edit" · `Undo` "Undo" · `DiffExit` "Exit"
- **Edit hint set (actions):** `DiffExitEdit` "Done" · `Undo` "Undo" · `Newline` "Newline" · `DeleteCharBack` "Delete"

Both sets render against `theme.hint_bar_diff` so the strong
diff-mode color is preserved across both sub-modes. If the hint set
is wider than the terminal, it silently overflows (truncated on the
right) — matching existing behavior for all other modes.

### `Esc` with unresolved or un-applied changes (Review only)

`Esc` in Review always opens `DiffExitConfirmModal` (`ModalKind::
Warning`, dismissable, buttons `[Keep reviewing]` default +
`[Discard]`). The body text varies by state:

- **Some hunks still `Pending`:** "You have unresolved changes.
  Discard them and exit diff mode?"
- **All hunks decided but resolution not yet confirmed** (the
  `DiffResolveConfirmModal` is open or was dismissed): "You have
  unapplied decisions. Discard them and exit diff mode?"

`[Discard]` reverts `editor.buffer` to the cached `old_rope`, clears
`editor.diff` and `editor.history`, and dismisses the resolve modal
if it's open. `[Keep reviewing]` dismisses the exit-confirm modal
with no other state change; the resolve modal, if it had been open,
stays open underneath.

This gives the user an always-available escape hatch — they never
get stuck in a state where they can't exit without re-deciding
hunks they already decided.

`Esc` in Edit sub-mode is intercepted *before* this path and simply
returns to Review — it never directly triggers diff exit.

**Modal precedence.** If any modal is open (theme picker, command
palette, intro modal, exit-confirm modal itself, etc.), modal `Esc`
dismissal takes precedence: the topmost modal closes and the
diff-exit confirm flow does not fire. The diff-exit confirm path
only runs when no modal is currently open and the sub-mode is
`Review`.

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
        (Action::DiffEnterEdit, "Edit hunk"),
        (Action::DiffExitEdit, "Exit edit"),
        (Action::DiffExit, "Exit diff"),
    ],
),
```

The section appears after the existing "Table" section in the
overlay's display order.

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

  **`Action::Quit` guard.** `Quit` is allowlisted, but it is **not** dispatched directly through `edit_ops::apply` in diff mode. Instead, `App::handle_app_action(Action::Quit)` gains a diff-aware guard: if `editor.diff.is_some()` and (any decision is non-`Pending` OR `DiffHistory::past` contains a `DiffOp::Edit`), the same `DiffExitConfirmModal` from §9 is opened (body reads "You have unresolved changes. Discard them and quit?"; buttons `[Keep reviewing]` default / `[Discard and quit]`). `[Discard and quit]` reverts to `old_rope`, exits diff mode, and re-dispatches `Action::Quit`. If no decisions are pending and no in-diff edits exist, `Quit` falls through to the normal dirty-buffer-Quit path. The reason: `editor.dirty` reflects pre-diff buffer state and does not capture in-diff work, so the standard dirty-buffer guard would silently lose the entire review.

- The command palette filters its visible entries through `diff_safe_action` while in diff mode so blocked actions are not even offered (palette-invoked theme switching, settings, keybinds remain available).

## 11. File-change events while already in diff mode

Any `AppEvent::FileChanged` received while `editor.diff.is_some()`
is queued (single-slot — newer overwrites older) on `App`. The queued
event is a flag only (path, no cached contents — the contents on the
incoming event are dropped because they may be stale by the time the
user finishes the review). After diff resolution completes, if a
queued flag exists, the App calls `FileWatcher::force_reconcile()`
(the same primitive used in the external-editor flow, §2). The
**watcher worker thread** performs the disk read off the main thread
and pushes a fresh `AppEvent::FileChanged { path, contents }` onto
the mpsc, which the event-loop arm picks up like any other change
event. This keeps the potentially slow disk read out of the UI
thread and reuses the same code path the watcher uses for organic
events. The newly-arrived event is diffed
against the just-merged buffer, and:

- If hunks are **non-empty**, re-enters through the same path as the
  initial file-change: if the buffer is dirty (per the conditional
  rule in §6 — typically true after a mixed-decision resolution but
  not when every hunk was accepted with no edits), show
  `DirtyConflictModal`; if clean, enter diff mode directly.
- If hunks are **empty** (disk now matches the merged buffer
  byte-for-byte), the queued event is dropped and the app remains in
  normal editing mode. No modal, no flash — this is the common
  "user accepted everything from disk, watcher echoed our own save"
  case, and even with the own-write filter (§2) a delayed external
  echo can still hit this path. Silent is correct.

If the fresh disk read fails (file deleted or moved between queue
time and resolution time), the queued event is silently dropped —
deleted-file handling is out of scope (§2).

This prevents a "diff mode forever" loop while still picking up
further changes.

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
- `src/app/modal/diff_exit_confirm.rs` (second-step confirmation when
  user `Esc`'s out of an in-progress diff review)
- `src/app/modal/diff_resolve_confirm.rs` (confirmation gate before
  applying the merged result when all hunks are decided)
- `tests/watcher.rs`
- `tests/diff_engine.rs`
- `tests/diff_history.rs`
- `tests/diff_view.rs`

**Modified files:**

- `Cargo.toml` — add `seahash` for the own-write content-hash filter (§2)
- `src/app.rs` — `AppEvent::FileChanged`, `watcher` field, `diff_paused` flag, queued-event single-slot (path only, no contents), `last_disk_hash: Option<u64>` field (§2)
- `src/app/event_loop.rs` — file-change arm; own-write filter (drop incoming `FileChanged` when `seahash(contents) == last_disk_hash`, otherwise stamp the new hash before dispatching); watcher pause/resume + `force_reconcile()` call in external-editor flow and in the post-resolution queued-event re-entry (§11); deadline integration
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
- **Diff history** (`tests/diff_history.rs`): sequence of accept / reject / edit / undo / redo asserts. Includes the hunk-id stability case where an edit reshapes the hunk list.
- **Diff view rendering** (`tests/diff_view.rs`): `TestBackend` snapshot tests for the stacked old-above-new layout, the focused- hunk gutter glyph, and the inline word highlight overlay.
- **Modal flow** (extend `tests/ui.rs`): `DirtyConflictModal` → `DiffIntroModal` → diff view; opt-out via the checkbox sets `show_diff_intro = false` in the loaded config.
- **Autosave gating** (extend `tests/editing.rs`): set `editor.diff = Some(...)`, advance the autosave clock past `autosave_idle_ms`, assert no save fires.
- **Own-write echo suppression** (`tests/watcher.rs`): with the content-hash filter (§2), call `App::save_buffer()`, then push a `FileChanged` whose contents equal the just-saved bytes; assert it is dropped. Push a second `FileChanged` whose contents differ by one byte; assert it is delivered. Cover the initial-load case: open a file, immediately push a `FileChanged` with identical contents, assert it is dropped (matches the just-loaded hash).
- **Queued-event single-slot replace** (`tests/diff_view.rs` or new `tests/diff_queue.rs`): with `editor.diff = Some(...)`, push two `AppEvent::FileChanged` events in succession; assert only one remains queued (the second overwrites the first, not appends). Then resolve the diff and assert the re-entry path is invoked exactly once with the latest-disk-read contents.
- **Boundary-crossing delete no-ops** (extend `tests/diff_history.rs` or in `tests/editing.rs`): enter `DiffSubMode::Edit` on a hunk; position the cursor at the first char of the focused new-side range and send `Action::DeleteCharBack` — assert the rope is unchanged, the cursor stays put, and a status flash is recorded. Repeat at the last char with `Action::DeleteCharForward`. Repeat in the middle of the range to assert the normal case still works.
- **Cursor clamp on MoveUp / MoveDown** (extend `tests/editing.rs`): enter Edit on a multi-line hunk; from the first new-side line send `Action::MoveUp` — assert `cursor.offset` is unchanged and the flash fires. From the last new-side line send `Action::MoveDown` — same. Send `MoveUp` / `MoveDown` from interior lines to assert ordinary motion still works. Also assert `MoveLeft` at the range's first offset and `MoveRight` at the range's last offset clamp identically. Run the test matrix with `visual_line_nav = true` and `false` to cover both motion algorithms (§5).
- **Ropey line-range invariants** (`tests/diff_engine.rs`): a small unit test that pins the ropey behavior the line-range convention in §3a relies on. Construct two ropes — one with a trailing newline, one without — and assert that `rope.byte_to_line(rope.len_bytes())` returns `rope.len_lines() - 1` for the no-trailing-newline rope and `rope.len_lines()` for the trailing-newline rope. The whole half-open line-range scheme rides on this; pin it as documentation in case ropey ever changes the edge.
- **Hunk-merge id stability** (extend `tests/diff_history.rs`): construct an `old_rope` / `new_rope` pair that produces two distinct hunks; record a decision on the first hunk (`Accepted`); enter Edit on the second hunk and insert enough lines that the inter-hunk context gap disappears so the two hunks merge under the next recomputation. Assert: (a) the merged hunk's `HunkId` equals the prior with larger old-side overlap (§6 rule 5); (b) the merged hunk's `Decision` is the inherited prior's (not forcibly reset to `Pending`); (c) the dropped prior's `Decision` is silently discarded; (d) any `DiffOp::Decision` in the undo stack referencing the dropped `HunkId` is skipped on undo without erroring (§6 rule 4).

## 14. Implementation checkpoints

Phase 1 is broken into five checkpoints. At each boundary, all tests
pass and the app builds and runs. Each checkpoint is a self-contained
PR-sized unit.

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
- Clean buffer → silent reload from disk.
- Dirty buffer → `DirtyConflictModal` with three working buttons: `[Save a copy]`, `[Discard & reload]` (with confirmation sub-modal), `[Keep buffer]`. The `[Merge]` button flashes "Diff mode coming soon" — it is wired in Checkpoint 3.
- Watcher pause/resume + `force_reconcile()` around external-editor suspend.

**Tests:** `tests/watcher.rs` (FakeFileWatcher debounce + tempfile integration + own-write hash echo suppression + stamp-on-accept behavior), modal button flows in `tests/ui.rs`.

**Verifiable live:** edit the open file in another editor; the modal appears with three working actions.

**CP2 deviations from the plan as written:**
- A `src/app/file_changed.rs` module hosts the file-change dispatch logic (`App::handle_file_changed`, `App::reload_buffer_from_disk`) instead of inlining it into `src/app/event_loop.rs`.  The event-loop arm is a one-line call into the new module; the dispatch decision tree (hash filter → no-diff short-circuit → dirty-check) lives in its own file alongside its tests.
- `[Save a copy]` from the dirty-conflict modal is implemented via a dedicated `DirtyConflictSaveCopyModal` (wraps `SaveCopyState` + `SaveCopyView`) rather than pushing the existing `SaveCopyModal`.  The dedicated variant carries the on-disk contents through the save path so the post-save reload is byte-identical to the watcher payload (no disk re-read race) and the parent `DirtyConflictModal` is popped on success.
- Watcher worker exposes events on a `mpsc::Sender<WatchedChange>` rather than `mpsc::Sender<AppEvent>`; a small bridge thread in `App::spawn_event_threads` forwards them as `AppEvent::FileChanged`.  Keeps the watcher unit-testable without an `App`.
- External-editor pause is implemented by calling `unwatch()` on suspend and `watch()` + `force_reconcile()` on resume (rather than a separate paused flag on the watcher).  Effect is identical: no organic events reach the main thread while the editor is in flight, and the forced reconcile picks up any external edits the editor made.

### Checkpoint 3 — Diff engine + raw DiffView + Review decisions (no edits, no undo)

**New files:** `src/diff.rs`, `src/diff/engine.rs`, `src/diff/state.rs`, `src/diff/hunk.rs`, `src/ui/diff_view.rs`, `src/app/modal/diff_intro.rs`, `src/app/modal/diff_resolve_confirm.rs`, `tests/diff_engine.rs`, `tests/diff_view.rs`

**Modified files:** `Cargo.toml` (add `similar`), `src/editor/mode.rs` (`Mode::Diff`), `src/editor/state.rs` (`diff: Option<DiffState>`, enter/exit helpers), `src/config/keymap.rs` (new `Action` variants + default binds), `src/config/theme.rs` (diff style slots), `src/config/sections.rs` (`show_diff_intro`), `src/ui/editor_view.rs` (dispatch to `DiffView`), `src/ui/status_bar.rs` (`DIFF` badge + colored bar), `src/ui/bottom_region.rs` (`Mode::Diff` hint set), `src/ui/settings_overlay/rows.rs` (`show_diff_intro` toggle), `src/ui/keybinds_overlay/categories.rs` ("Diff Review" section), `src/app/autosave.rs` (early-return in diff mode), `src/input/mode_handler/default.rs` (diff-mode action dispatch, Save flash)

**Scope:**
- `compute_hunks` engine with line-level + inline word-level diff, including row-level table sub-diff (§3, §3a).
- `DiffState` (without `DiffHistory` — the `history` field is
  `DiffHistory { past: vec![], future: vec![] }` and `record()` is
  never called in CP3. `Action::Undo` / `Action::Redo` in diff mode
  are explicit no-ops until CP4 wires them).
- `Mode::Diff` variant; `EditorState::enter_diff_mode()` / `exit_diff_mode()`.
- `DiffView` raw rendering with `DiffVisualLine` model (§5): stacked old/new, gutter glyph, decision indicator on first line.
- `DiffIntroModal` with opt-out checkbox (§8).
- Wire `DirtyConflictModal`'s `[Merge]` button to enter diff mode.
- Actions: `DiffNext`, `DiffPrev`, `DiffAcceptHunk`, `DiffRejectHunk`, `DiffAcceptAll`, `DiffRejectAll`, `DiffExit`. Review sub-mode only — no `DiffSubMode` enum yet. `DiffEnterEdit` and `DiffExitEdit` are added to the `Action` enum and keymap in this checkpoint (consistent with the "fully defined upfront" convention) but are explicit no-ops in the dispatch — they are wired in Checkpoint 5.
- Theme additions (§7), status bar + hint bar diff coloring.
- Keybinding overlay "Diff Review" section (§9).
- Autosave + `Action::Save` gated in diff mode; `SaveCopy` allowed (§10).
- Resolution: when all decisions are non-`Pending`, show `DiffResolveConfirmModal` (§8). On confirmation, swap resolved rope into buffer, flash "Diff resolved", exit diff mode. Dismissing the modal returns to Review with all decisions intact. **Decision-only history is in-memory and lost on exit; resolution writes nothing to `editor.history` in CP3.** The merge-revert entry described in §6 is introduced in CP4 alongside `DiffHistory`.

**Tests:** `tests/diff_engine.rs` (snapshot tests), `tests/diff_view.rs` (TestBackend snapshots), modal intro flow in `tests/ui.rs`, autosave gating in `tests/editing.rs`.

**Verifiable live:** full review-and-decide flow works; accept/reject each hunk, accept-all/reject-all; `Undo` is a no-op (no history yet).

### Checkpoint 4 — Decision undo/redo + Esc handling + event queue

**New files:** `src/diff/history.rs`, `src/app/modal/diff_exit_confirm.rs`, `tests/diff_history.rs`

**Modified files:** `src/diff/state.rs` (wire `DiffHistory`), `src/editor/edit_ops.rs` (route `Undo`/`Redo` to `DiffHistory` in diff mode), `src/app.rs` (queued-event single-slot), `src/app/event_loop.rs` (queue + re-entry after resolution)

**Scope:**
- `DiffHistory` with `DiffOp::Decision` and `DiffOp::BulkDecision` only (no `Edit` variant yet).
- `Action::Undo` / `Action::Redo` routed to `DiffHistory` while in diff mode; main `History` paused.
- **Resolution checkpoint:** introduce the single synthetic merge-revert `EditDelta` and the `History::reset_with(EditDelta)` setter (§6). After CP4, exiting diff mode leaves `editor.history` with that single entry as its sole undo step (replacing the CP3 behavior where exit wrote nothing to history).
- `DiffExitConfirmModal` for `Esc` with pending decisions (§9).
- Queued `FileChanged` event re-entry: after resolution, re-enter through the standard dirty-check path (§11).

**Tests:** `tests/diff_history.rs` (accept/reject/bulk + undo/redo sequences), exit-confirm modal flow, queued-event re-entry.

**Verifiable live:** cycle accept/reject with Ctrl-Z undoing decisions; Esc prompts when hunks are pending; editing file during review queues and replays after resolution.

### Checkpoint 5 — Edit sub-mode + clamped editing + Edit undo

**Modified files:** `src/diff/state.rs` (`DiffSubMode`, sub-mode tracking, `DiffState::apply_edit`), `src/diff/hunk.rs` (`HunkId` stability logic), `src/diff/history.rs` (`DiffOp::Edit`, word-group merging), `src/editor/state.rs` (mode-aware `apply_delta` branch, `edit_target()` accessor), `src/editor/edit_ops.rs` (in-hunk edit gating, boundary-crossing delete no-ops, `DiffEnterEdit`/`DiffExitEdit`, Undo/Redo diff-mode routing), `src/ui/diff_view.rs` (Edit cursor rendering), `src/ui/status_bar.rs` (`DIFF·EDIT` badge), `src/ui/bottom_region.rs` (Edit hint set), `src/config/keymap.rs` (`DiffEnterEdit`/`DiffExitEdit` default binds)

**Scope:**
- `DiffSubMode::Review` / `DiffSubMode::Edit` enum and sub-mode dispatch.
- Mode-aware `apply_delta` branch in `EditorState` and `DiffState::apply_edit` (§4a). `edit_target()` accessor for Undo/Redo routing and active-buffer reads.
- `DiffEnterEdit` / `DiffExitEdit` actions. `DiffEnterEdit` on a `Delete` hunk converts it to `Replace` by inserting a blank line (§4).
- Hard-clamped cursor motion within the focused hunk's new-side range.
- Boundary-crossing delete no-ops (`Backspace` at start, `Delete` at end) with status flash.
- Newline insertion expands the focused hunk downward.
- Hunk re-computation with `HunkId` stability: focused hunk preserved by construction, others matched by old-side range (§6).
- `DiffOp::Edit { delta: EditDelta }` with word-group merging (§6).
- Undo/Redo diff-mode dispatch: Decision undo flips a flag, Edit undo applies inverse delta + recomputes hunks (§4a, §6).
- `DIFF·EDIT` status badge, Edit hint set.

**Tests:** hunk-id stability across edits, edit-then-decision interleaving, boundary-clamp assertions, delete-hunk-to-replace conversion.

**Verifiable live:** enter Edit on an add hunk, type replacement text, Esc back to Review, accept; Ctrl-Z reverses word-groups; enter Edit on a delete hunk, type replacement, accept.

---

## 15. Phase 2 — Hybrid rendered diff view (future)

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
