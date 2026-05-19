# Reduce input lag in edamame

## Context

Holding a key (Backspace or a letter) in edamame produces output that
continues seconds after the key is released. On a ~140-line file the
editor feels snappy; on the ~1500-line `docs/plan.md` it visibly lags
(e.g. holding `a` for 3 s, characters continue printing for 5 s).

The buffer layer already defers re-parsing for **in-line** edits:
`apply_delta` in `src/editor/state.rs:617` sets `parsed_dirty = true`
without calling `refresh_parsed`. So the dominant per-event cost is no
longer the buffer mutation — it's downstream work that still scales
with document size:

1. **No keystroke coalescing.** The main loop in `src/app/event_loop.rs`
   reads one event per iteration, then draws (throttled at 16 ms in
   `frame_timer.rs`). Image events are coalesced via
   `drain_pending_image_ready`; keyboard events are not. When per-event
   work exceeds the terminal's autorepeat interval, the mpsc queue
   backs up and the lag compounds.

2. **Cross-line edits still trigger a full re-parse.** Pressing Enter,
   or Backspace at a line start, calls `refresh_parsed` synchronously
   → `buffer.contents()` clones the whole rope → pulldown-cmark parses
   the entire document → the visual-rows cache is discarded.

3. **`parsed_version` bumps on every in-line keystroke**
   (`state.rs:645`), invalidating the per-frame snapshot caches in
   `link_view`, `image_view`, and `table_view` even though the AST
   hasn't changed. `link_view::build_snapshots` walks every block on
   every frame during a typing burst.

The intended outcome: typing into `plan.md` should feel
indistinguishable from typing into `test.md`, with no perceivable
trailing output after key release.

## Plan

Ship in three PRs; each is independently valuable but PR 2 is where
B and C must land together.

### PR 1 — Keystroke coalescing in the main loop  *(implemented)*

**Change.** Before each draw, drain the mpsc channel of any
already-arrived `Term(Event::Key(Press))` events and dispatch them as
a batch. Within the batch, identify *same-kind* runs — a run of
`InsertChar(c)` actions, or a run of `DeleteCharBack`, or a run of
`DeleteCharForward` — and collapse each run into a single
`EditDelta`. **Do not mix kinds inside one delta.** `EditDelta` has a
single `(offset, removed, inserted)` triple, so a mixed run (e.g.
`Insert('a'), Backspace, Insert('b')`) can't be expressed as one
delta without ad-hoc cancellation logic; the run-membership predicate
must break on any kind change. The deferred-parse / version-bump path
then fires once per run, not once per keystroke.

**Files.**
- `src/app/event_loop.rs` — add `drain_pending_key_events` mirroring
  `drain_pending_image_ready` (which lives in `src/app.rs:491`, not
  `event_loop.rs` — only the call site is in the run loop). The drain
  must consume `pending_term_event` first, then pull from `rx` via
  `try_recv` until it would block. Non-key events encountered during
  the drain (`Mouse`, `Resize`, `Paste`, FocusGained/Lost, image-ready,
  etc.) cannot be stashed back into `pending_term_event` — it's a
  single slot, and a single drain pass can surface multiple non-key
  events (e.g. a Resize sandwiched between two key presses, plus a
  queued ImageReady). Two acceptable shapes:
  1. Replace `pending_term_event: Option<Event>` (`app.rs:176`) with
     `pending_events: VecDeque<AppEvent>`. The queue is strictly FIFO:
     `next_event` (event_loop.rs:329) pops from the front before
     consulting `rx`, and all stash sites push onto the back. This
     preserves the user's event timeline — a Resize that arrived
     between two keystrokes must be processed between them, not after
     the whole coalesced batch. This is the cleaner of the two shapes —
     it generalises the existing single-slot replay rather than
     building a parallel mechanism. **Coupled change required:**
     `drain_pending_image_ready` (`app.rs:491–542`) currently breaks on
     the first `AppEvent::Term` it sees and writes it to
     `pending_term_event`. Under shape 1 it must instead push the term
     event onto the back of `pending_events` and continue draining,
     otherwise queued image-ready events get starved behind the first
     keystroke. Update its early-exit and stash logic in the same PR —
     it will not compile under the new field shape regardless.
  2. Dispatch non-key events inline as the drain encounters them,
     keeping `pending_term_event` as-is. Riskier: mouse / resize
     handlers re-enter `dispatch_*` paths that may themselves mutate
     editor state mid-drain, and the per-batch coalescing must then
     be careful not to fuse key events that span an inline-dispatched
     non-key event.
  Prefer shape 1. Either way, do NOT silently drop non-key events.
- `src/app/event_loop.rs` — `dispatch_key_event` (738) currently takes
  a single `Event`; change the signature to accept a batch
  (`&[Event]` or an iterator). Iterate the batch, detect leading runs
  of coalescable actions, and flush per-run side-effects
  (`flash_for_action`, `pending_link_follow`, `pending_open_*`)
  **once** at the end of the run, not per event.
- `src/editor/edit_ops.rs` — add a `apply_insert_run(state, &str,
  viewport_*)` and `apply_delete_run(state, count, backward, …)` that
  build one `EditDelta` and route through `state.apply_delta`. Reuse
  `next_grapheme_offset` / `prev_grapheme_offset` (`edit_ops.rs:412–
  435`) for the delete-run boundary walk.
- `src/config/keymap.rs` — no existing classification covers the
  coalescable trio (the `action_variants!` block at line 190 lists
  delete-family variants together for `Display`/`FromStr` derivation,
  but `InsertChar` carries a payload and is handled separately at
  line 171). Add a small predicate (e.g. `coalesce_kind(&Action) ->
  Option<CoalesceKind>` returning `Insert | BackDelete | ForwardDelete`
  for the trio and `None` for everything else) and use *equal `Some`
  kinds* as the run-membership test.

**Guards.**
- Only coalesce when no modal is open and no drag is in progress.
- **Break the run when a selection is active at the start of an
  event.** With a selection live, `DeleteCharBack` / `DeleteCharForward`
  delete the whole selection and clear it; coalescing a follow-up
  delete into the same run would silently extend a "delete selection"
  delta with a single-grapheme delete. The current (pre-PR1) behaviour
  is that the selection-replacing edit is its own history entry, and
  PR1 must preserve that. Sample `state.selection.is_some()` per event
  (not once at drain entry) — the first selection-deleting event in a
  run ends the run *after* dispatching itself, because dispatching it
  also clears the selection, and the next event would otherwise look
  selection-free and merge in.
- Break the run on any non-coalescable action (cursor move, mode
  switch, Enter, Escape, table/list special handling). Enter must
  break the run *before* it dispatches, because list/table newline
  handlers in `edit_ops.rs:389–402` have side-effects beyond an
  insert.
- Sample `mode`, `any_modal_open`, and `drag_anchor.is_some()`
  **once** at drain entry and use the captured values as the
  run-break predicate. Do not re-query any of them per event — under
  shape 2 a mid-drain inline-dispatched event could mutate them and
  silently change the predicate halfway through the run. A real mode
  change or drag start ends up as an action / mouse event in the
  batch, which then breaks the run via the non-coalescable-action
  rule above. (Selection is the deliberate exception above — it
  changes *as a result* of dispatching an event in the batch, and
  the next event must see the new value.)
- **A dispatched event may open a modal or set a pending App-level
  side-effect.** Actions like `ShowCommandPalette`, `OpenSettings`,
  `OpenKeybinds`, the dirty-`Quit` confirm flow, and any path that
  sets `pending_open_file_in_editor` / `pending_open_theme_in_editor`
  / `pending_link_follow` change the meaning of subsequent events:
  the next keystroke should route through `dispatch_modal_event`
  (or be deferred until the external-editor flow completes), not
  through `dispatch_key_event` as a raw keystroke. After dispatching
  each event in the batch, check `!self.modal_stack.is_empty()`,
  `self.pending_open_file_in_editor`, `self.pending_open_theme_in_editor`,
  and `self.editor.pending_link_follow.is_some()`. If any is set,
  stop draining: push the remaining unprocessed events to the front
  of `pending_events` (shape 1) in their original order and return
  from `dispatch_key_event`. The run loop's next iteration will then
  route them through the modal dispatcher / drain the deferred flow.
  Under shape 2 this is harder to get right — another reason to
  prefer shape 1.
- Preserve the kill-ring / clipboard side-effects by never coalescing
  across `Cut` / `Paste` / `Copy`.
- Autosave (`tick_autosave`) keys on `editor.dirty`; one merged delta
  still flips dirty, so it keeps working.
- **Extend `History` same-kind merging to cover all charsets.** Today
  `try_merge_insertion` / `try_merge_deletion` (`history.rs:137`,
  `:156`) gate on `is_alphanumeric` for both the incoming char and
  the adjacent char in `top`. That means a punctuation/whitespace
  autorepeat held across multiple frames produces one history entry
  *per frame* even after PR1's per-frame coalescing — the run loop
  drains at ~60 Hz, autorepeat fires at ~30 Hz, so a 3 s hold of `!`
  yields ~3 history entries, not one. To deliver the "one undo step
  per hold" claim, drop the `is_alphanumeric` predicates and merge on
  contiguity alone:
  - `try_merge_insertion`: merge when `new.offset == top.offset +
    top.inserted.chars().count()` and both `top` and `new` are pure
    insertions of any chars (including `\n`, punctuation, whitespace).
  - `try_merge_deletion`: merge when the offsets match the backspace
    or forward-delete adjacency rule, for any chars.
  - Keep the existing "cursor moved → non-contiguous offset → no
    merge" behaviour; that's what naturally breaks groups on
    intentional cursor motion. Tests covering that (e.g.
    `non_contiguous_delete_breaks_group` at `history.rs:589`) stay
    green.
  - Update `space_breaks_backspace_group` (`history.rs:531`) and
    `space_breaks_forward_delete_group` (`history.rs:560`) — they
    assert the *old* per-charset grouping and must be replaced with
    tests that assert merge-by-contiguity. Similarly audit the
    insertion-side tests at `history.rs:302` and `:351`.
  - This is a user-visible undo behaviour change: holding any key
    (alphanumeric, punctuation, whitespace, Backspace, Delete) now
    undoes in one step. Call this out in the PR description.

**Tests.**
- `tests/editing.rs` existing per-keystroke sequences continue to
  pass (one event at a time goes through unchanged).
- New: `tests/editing.rs::burst_of_inserts_coalesces_into_one_delta`
  — push N `InsertChar` actions via a new coalescing entrypoint;
  assert `History::len()` advances by one and final buffer contents
  match the concatenation. If PR2 has already landed, also assert
  `editor.ast_version` is unchanged (no cross-line edits in the
  burst). If PR1 lands first, assert against the pre-rename
  `editor.parsed_version` (one bump for the coalesced delta) and
  update the test when PR2 lands — don't churn the field name twice.
- New: `tests/editing.rs::burst_breaks_on_kind_change` — alternate
  `InsertChar('a'), DeleteCharBack, InsertChar('b')`; assert
  `History::len()` advances by three (one delta per run), not one.

**Impact.** Largest single win. Eliminates the "5 s of output after 3
s held" symptom by collapsing autorepeat bursts to one apply.

**Stop and reassess after PR1.** The trailing-output symptom is the
textbook signature of an mpsc backlog, and PR1 directly drains that
backlog. Before committing to PR2 and PR3, re-run the verification
steps (§ Verification) on `docs/plan.md`. If holding `a`, Backspace at
a line start, and Enter all stop within ~100 ms of key release, and
mouse-hover feels smooth, treat PR2 and PR3 as deferred — they
carry real risk (PR2 changes the parse-consistency invariant; PR3 is
mechanical but broad) and shouldn't ship speculatively. Only proceed
if (a) the symptom persists, or (b) profiling on `plan.md` shows
per-batch reparse / `Buffer::contents()` calls dominating frame time.

### PR 2 — Defer cross-line re-parse + split version counter  *(DEFERRED)*

These ship together: B is what makes typing through line boundaries
cheap; C is what makes B safe for the snapshot caches.

**B. Defer cross-line re-parse — flush at batch end, not on a timer.**

The infrastructure for deferred reparse is already in place:
`EditorState::parsed_dirty` (`state.rs:223`) and
`flush_parsed_if_dirty` (`state.rs:595`) exist today and are wired
into the in-line edit path. PR2-B is therefore *not* about building
new machinery — it's about (a) removing the eager-reparse arm in
`apply_delta`'s `crosses_line` branch, (b) adding `flush_parsed_if_dirty`
calls at the few entrypoints that currently don't have them, and (c)
auditing the readers that today implicitly rely on cross-line edits
having already reparsed.

The first instinct was: drop the synchronous reparse in `apply_delta`,
set `parsed_dirty = true` even when `crosses_line`, run a 50 ms
quiesce timer, and patch `cursor_block_line_range` in place by the
delta's net line count to keep the rendered view consistent during
the quiesce window. That path is fragile: pressing Enter mid-block
doesn't merely shift the cursor block's line range by ±1, it *splits*
the block (heading → heading + new paragraph; list item → two list
items with continuation rules; blank line → virtual block insertion
via `ParsedDoc::build`). The stale `cursor_block_line_range` then
describes a region that no longer corresponds to one block in the
post-edit document, and subsequent in-line edits inside the new block
need their range tracked too. The line-range shadow state would have
to mirror most of `ParsedDoc`'s block-boundary logic to stay correct.

**Default approach: flush at the end of each coalesced batch.**
PR1 makes batches large (one per autorepeat burst per frame), so one
reparse per batch is already an order-of-magnitude reduction from
today's one-per-keystroke. Crucially, this preserves the invariant
that `parsed`, `cursor_block_line_range`, and `source_map` are always
consistent at the point any caller reads them — provided the reader
audit below is exhaustive.

- `src/editor/state.rs:617` `apply_delta` — drop the
  `if crosses_line { refresh_parsed; update_cursor_block }` arm
  (lines 633–638). Always set `parsed_dirty = true`. Do NOT patch
  `cursor_block_line_range` here.
- `src/app/event_loop.rs` `dispatch_key_event` (738) — after the
  coalesced batch has been applied, call
  `self.editor.flush_parsed_if_dirty()` exactly once. The batch is
  the natural quiesce boundary: between batches the run loop draws,
  and the draw must see a consistent parse.
- `src/app/event_loop.rs` `dispatch_paste` (726) — `Event::Paste`
  bypasses `dispatch_key_event` entirely and goes straight through
  `edit_ops::paste_text` → `apply_delta`. A multi-line paste sets
  `parsed_dirty` but never reaches the key-batch flush. Add a
  `flush_parsed_if_dirty` call at the end of `dispatch_paste` so
  cross-line pastes don't leave the next draw reading a stale parse.
- `src/app/event_loop.rs` `dispatch_modal_event` — modal-driven
  buffer mutations (table-insert modal, save-copy, future overlays
  that edit text) are a separate entrypoint from `dispatch_key_event`
  and likewise need a `flush_parsed_if_dirty` at the end of the
  dispatch. List every modal action that can route through
  `edit_ops::apply` or `apply_delta` and confirm coverage.
- `src/app/event_loop.rs` `tick_timers` (151) — defensive flush for
  any remaining code paths that mutate the buffer outside the three
  dispatchers above (e.g. autosave-driven reformat, timer-fired
  edits). No new deadline field needed.
- `coalesce_image_updates` (in `src/app.rs`) calls `refresh_parsed`
  unconditionally when `images_dirty` and runs in the same frame as
  the key batch. If a key batch left `parsed_dirty = true` and an
  image-decode completes in the same frame, both will reparse —
  harmless (idempotent) but it breaks the "exactly one reparse per
  batch" invariant the new tests assert. Either (a) have
  `coalesce_image_updates` short-circuit when `parsed_dirty` is
  already set and let the batch-end flush handle it, or (b) gate the
  `batch_end_flush_reparses_once` test on `images_dirty == false` at
  entry. Prefer (a) — keeps the invariant honest.
- **Reader audit — must complete before PR2 lands.** Today's
  `apply_delta` guarantees that after any cross-line edit,
  `cursor_block_line_range`, `parsed.source_map`, and `parsed.blocks`
  are consistent with the buffer. Once we defer that work, every
  reader that runs between a cross-line edit and the next
  batch-end flush sees stale state. The concrete readers (from
  `grep cursor_block_line_range|cursor_rendered_line_idx|source_map|parsed\.blocks`)
  that need to be classified are:
  - **`cursor_rendered_line_idx`** (`state.rs:756`) — consults
    `cursor_block_line_range`. Called from `state.rs:723` and from
    `mouse_ops/coord.rs:241` (mouse hit-test "is this the cursor
    line?"). Mouse path: `dispatch_mouse_event` is a separate
    entrypoint from `dispatch_key_event`; it needs a
    `flush_parsed_if_dirty` at entry, otherwise a click landing
    immediately after a held-Enter run resolves against a stale
    line range.
  - **`update_cursor_block` callers** (`state.rs:1112`, `:1152`;
    `state_viewport.rs:112`; `table_edit_ops.rs:204`, `:229`; the
    ~15 sites in `edit_ops.rs` lines 114–301, 1117; the
    `mouse_ops.rs` sites at 332, 389, 422, 437, 452). Each one
    re-derives `cursor_block_line_range` from the *current* parse —
    so if `parsed_dirty` is set, they'll cache stale block bounds.
    Add a `flush_parsed_if_dirty` immediately before each
    `update_cursor_block` call site that runs after a potential
    cross-line edit. The `edit_ops.rs` sites mostly run *as part of*
    the cross-line edit and so are pre-flush by definition — a
    blanket "flush before every `update_cursor_block`" rule is the
    cheap, safe option and worth taking even if redundant in some
    arms.
  - **`RenderedView`** already branches on `parsed_dirty` (see
    `rendered_view.rs:108`, `:118`, `paint.rs:149`) and uses the
    cached source. Re-verify under cross-line deferral: the cache
    captures a snapshot from the last reparse, so a `\n` insert
    deferred until batch end may leave one frame of rendering
    against the pre-edit block layout. Confirm this is visually
    indistinguishable (it should be — same frame the batch is
    flushed at the end) or add a `flush_parsed_if_dirty` to the
    render entry.
  - **`link_view`, `image_view`, `table_view` `build_snapshots`** —
    these read `parsed.{blocks, source_map}`. Today they're keyed
    on `parsed_version` and run inside `draw`, which happens after
    `dispatch_key_event` returns. The batch-end flush at the end of
    `dispatch_key_event` keeps them consistent — confirm draw
    ordering and document it.
  - **`edit_ops::apply` internal helpers** (list continuation,
    table-edit) — these read `source_map` mid-action. They already
    call `flush_parsed_if_dirty` at `edit_ops.rs:58` and `:736`;
    verify those two are the only entry points and that no helper
    reads `source_map` without going through them.
- The audit produces either (a) a small set of new
  `flush_parsed_if_dirty` calls (preferred), or (b) evidence that a
  given reader is tolerant of stale state (must be documented in a
  comment at the call site). Do not ship PR2 until every reader is
  classified.

**Fallback: timer-based quiesce.** Only consider this if profiling
shows one reparse per batch is still hot enough to drop frames on
1500-line docs. In that case, add `parsed_quiesce_at: Option<Instant>`
to `app.rs` next to `resize_quiesce_at`, a `PARSED_QUIESCE = 50 ms`
constant in `frame_timer.rs`, and have `tick_timers` call
`flush_parsed_if_dirty` when the deadline elapses. The line-range
shadow state stays off the table — instead, route every reader of
`parsed.source_map` / `parsed.blocks` through `flush_parsed_if_dirty`
so the parse becomes lazy-on-read rather than eager-on-quiesce. That
flips the invariant from "structures are always consistent" to
"structures are consistent at read time", which is a bigger
refactor and should not be undertaken speculatively.

**C. Split `parsed_version` into `ast_version` + `geometry_version`.**

- `src/editor/state.rs` — replace the single `parsed_version` field.
  `refresh_parsed` (535) bumps both; `apply_delta` (645) bumps only
  `geometry_version`.
- Snapshot cache keys:
  - `src/ui/link_view.rs:89` → key on `ast_version`.
  - `src/ui/table_view.rs:323` → key on `ast_version`.
  - `src/ui/image_view.rs:124` → key on `ast_version` (already cheap,
    but consistency keeps it correct).
- Any rendered-view caches that need to invalidate on cursor-block
  geometry shift use `geometry_version`. Grep `parsed_version` to
  catch all sites.

**Cache-key audit (done up-front so the split is safe).**

- `link_view::build_snapshots` (`src/ui/link_view.rs:103`) reads only
  `state.parsed.{blocks, real_ranges, source_map}` plus
  `state.buffer.path()` for `base_dir`. Purely AST-derived; keying
  on `ast_version` is correct.
- `image_view::build_snapshots` reads only AST state. Correct.
- `table_view::build_snapshots` (`src/ui/table_view.rs:338`) reads
  `state.buffer.contents()` (line 347) *in addition to*
  `state.parsed.source_map`. The byte-range guard at line 396–403
  (`source.get(range.start..end).unwrap_or("")`) was added precisely
  because in-line edits can leave `source_map` byte ranges pointing
  into the middle of a UTF-8 sequence in the live buffer. Keying on
  `ast_version` does NOT regress this — today's `parsed_version` key
  already lets the cache be rebuilt against a live buffer / stale
  AST pair, and the guard absorbs the mismatch. After the split,
  cache rebuilds simply happen less often, so the mismatch window
  the guard protects against gets shorter, not longer.
- Mouse hit-testing inside the cursor block during the
  edit-to-flush window remains approximate for the same reason as
  the existing `RAW_REVEAL_DELAY` (rendered ↔ raw column drift) —
  no new correctness gap introduced.

**Tests.**
- New: `tests/editing.rs::enter_within_batch_defers_reparse` — call
  `apply_delta` directly with a delta whose `inserted` is `"\n"`
  (bypassing both the `Action::Newline` list/table side-effects and
  the batch flush). Assert `ast_version` unchanged from its pre-edit
  value and `parsed_dirty == true`. Then call `flush_parsed_if_dirty`
  and assert `ast_version` advanced by exactly one and
  `parsed_dirty == false`. Deliberately do NOT pin the post-flush
  value of `geometry_version` — the choice of whether `refresh_parsed`
  bumps `geometry_version` in addition to `ast_version` is decided in
  C-task implementation, and pinning it here couples this test to
  that convention. The load-bearing invariants are only that
  `ast_version` advances exactly once and the dirty flag clears.
- New: `tests/editing.rs::inline_edit_does_not_bump_ast_version` —
  apply an `InsertChar('x')`; assert `ast_version` unchanged from
  its pre-edit value, `geometry_version` advanced by exactly one.
- New: `tests/editing.rs::batch_end_flush_reparses_once` — push a
  burst of inserts including one `\n` through the coalescing
  entrypoint; assert `ast_version` advanced by exactly one after the
  batch (not once per `\n`, not once per keystroke). Do not pin
  `geometry_version` for the same reason as
  `enter_within_batch_defers_reparse` above.
- Existing snapshot-cache tests in `src/ui/link_view.rs:482`,
  `src/ui/image_view.rs:482`, `src/ui/table_view.rs:1272` should
  still pass; update key tuple to use `ast_version`.

**Risk.** With batch-end flushing (the default above), the danger
window is the duration of a single coalesced batch — i.e. one frame.
Mid-batch readers of `source_map` / `blocks` would see stale data;
the audit above catches them. The historical concern about
`cursor_block_line_range` drifting under mid-block edits goes away
because we no longer try to keep it consistent without a reparse.
The remaining residual cost is one reparse per autorepeat batch on
large docs; if that proves too expensive, fall back to the
timer-based quiesce documented above (with the read-time-flush
invariant, not the line-range shadow state).

**Impact.** Removes the tens-of-ms per-cross-line cost. Combined with
PR 1 produces the snappy 140-line feel at 1500 lines. C alone removes
the per-frame `link_view` block walk during typing bursts, which is
the dominant residual draw-time cost.

### PR 3 — Reduce `Buffer::contents()` allocations  *(DEFERRED)*

**Change.** On hot paths, replace `buffer.contents()` (which clones
the whole rope) with `rope.byte_slice(range).to_string()` against the
byte range the call site actually needs. ropey's `Rope::byte_slice`
returns a lightweight `RopeSlice`; only the substring requested
allocates.

**Files (start with the hottest, all confirmed by grep).**
- `src/document/buffer.rs:130` — keep `contents()` for cold paths
  (file save, parser entry); document as such.
- `src/editor/state.rs:777` (`cursor_rendered_line_idx`) — the block
  range is already in hand.
- `src/editor/mouse_ops/coord.rs:197, 406, 501` — mouse-move fires on
  every pointer event; this pays off on hover.
- `src/editor/mouse_ops.rs:172, 261, 362, 405, 597` and
  `src/editor/mouse_ops/selection.rs:26, 138`,
  `src/editor/mouse_ops/links.rs:75, 142`,
  `src/editor/mouse_ops/checkbox.rs:25`.
- `src/editor/edit_ops.rs:983, 993, 1066, 1112, 1247` — list/table
  helpers; scope to the current block's range.
- `src/editor/table_edit_ops.rs:33, 220` and
  `src/editor/mouse_ops/table_drag.rs:44, 184, 241, 272, 312` — scope
  to the table's byte range.

**Skip.** `src/editor/state.rs:536` (refresh_parsed) genuinely needs
the whole document — pulldown-cmark takes `&str`. `src/editor/state.rs:519`
(table-comment lookup) is one-shot, cold. Test-only call sites stay.

**Verified absent.** `src/document/parsed_doc.rs` does not call
`buffer.contents()` (confirmed by grep over `src/document/`,
`src/editor/`, `src/ui/`). Parsing is driven from `refresh_parsed`
in `state.rs`, which passes `&content` into `ParsedDoc::build*`; the
parser side takes `&str` and doesn't re-clone. No hidden allocations
on the parse path beyond the one in `refresh_parsed` itself.

**Tests.** Existing coverage in `tests/mouse.rs`, `tests/editing.rs`,
`tests/table.rs` exercises every changed call site. Mind the
char/byte distinction: ropey is explicit, so add one
`rope.byte_to_char` / `char_to_byte` conversion at the boundary
rather than threading byte offsets through char-indexed APIs.

**Impact.** Lower priority than PR 1/PR 2. Pays off on mouse-move
latency at 1500 lines and shaves the cross-line edit / cursor-block-
reveal cost. Mechanical, low risk.

**Possible follow-up.** `Buffer::contents()` itself could return
`Cow<str>` (or a `RopeSlice`) so cold-path callers that only need
`&str` skip the allocation entirely. Out of scope for this plan but
cheap to revisit once the hot-path sites have moved to `byte_slice`.

## Verification

End-to-end:

1. `cargo run --release -- docs/plan.md`.
2. Hold `a` for 3 s in the middle of the document; release. Output
   must stop within ~100 ms.
3. Hold Backspace at a line boundary (start of a line) for 3 s;
   release. Output must stop within ~100 ms, and the document must
   remain visually consistent with the buffer (no stale rendered
   line under the cursor).
4. Hold Enter to spam newlines. Release; the document must reflow
   correctly within ~100 ms.
5. Mouse-hover across links and tables in the 1500-line doc; pointer
   shape feedback must remain smooth (no per-event stutter).
6. Repeat with `docs/test.md` to confirm no regression on small docs.

Automated:

- `cargo test` — all existing suites including
  `tests/editing.rs`, `tests/list_edit.rs`, `tests/mouse.rs`,
  `tests/table.rs`, `tests/source_map.rs`.
- `cargo insta review` for any snapshot drift.
- `cargo clippy --all-targets -- -D warnings`.
- New tests listed under each PR.
- New: `tests/editing.rs::synthetic_burst_per_batch_cost` — feed N
  synthetic `Press` events through the run loop's drain+dispatch
  path; assert `History::len() == 1`, one `ast_version` bump if any
  event was cross-line, otherwise zero. This is the CI regression
  guard for the user-visible "100 ms output stop" criterion above —
  manual hold-key tests can't gate CI.

Manual sanity:

- Save behaviour (`Ctrl-S`) and autosave (idle window) must still
  flush the buffer correctly after a coalesced burst.
- Undo (`Ctrl-Z`) must walk back through the coalesced delta as a
  single step, then continue stepping through prior history entries
  one at a time.
- Mode switches (Preview ↔ Rendered ↔ Raw) during a held key must
  not lose buffered keystrokes or apply them in the new mode by
  mistake — break the coalescing run on any mode-changing action.

## Critical files

- `src/app/event_loop.rs`
- `src/app.rs`
- `src/app/frame_timer.rs`
- `src/editor/state.rs`
- `src/editor/edit_ops.rs`
- `src/ui/link_view.rs`
- `src/ui/table_view.rs`
- `src/ui/image_view.rs`
- `src/document/buffer.rs`
- `src/editor/mouse_ops.rs` and `src/editor/mouse_ops/*.rs`
