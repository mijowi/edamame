# File-change detection and inline diff mode

A plan for adding (a) a filesystem watcher that detects on-disk edits to
the open file, and (b) a "diff mode" overlay where the user reviews,
edits, accepts, or rejects each change inline before the merged result
becomes the new buffer.

## Goals

- Detect on-disk changes to the open file, debounced over a 200 ms quiet window.
- Enter a dedicated `Mode::Diff` with strong visual signalling (status-bar / hint-bar color shift, `DIFF` mode badge, first-time explanatory modal).
- Render stacked inline diffs: deleted lines above, added lines below, with word-level inline highlights within changed pairs.
- Let the user cycle through changes, accept/reject each one individually, accept-all / reject-all, skip changes, and edit added content before accepting.
- All accept / reject / skip / edit decisions inside diff mode are undo/redo-able through the standard `Ctrl-Z` / `Ctrl-Y` keys.
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
notify = { version = "6", default-features = false, features = ["macos_fsevent"] }
similar = "2"
```

- `notify` — cross-platform watcher (used by cargo-watch, mdbook, helix). Default features pull `crossbeam-channel`; we drop them and feed events into our existing `mpsc::Sender<AppEvent>`.
- `similar` — line-level + inline word-level diff in one crate (`TextDiff::from_lines`, `InlineChange`).

## 2. Watcher subsystem

```
src/watcher.rs                  # facade
src/watcher/ file_watcher.rs               # FileWatcher trait + NotifyWatcher impl debounce.rs                   # 200 ms debouncer
```

```rust
pub trait FileWatcher: Send {   fn watch(&mut self, path: &Path) -> Result<()>;   fn unwatch(&mut self) -> Result<()>;
}
```

- One impl today: `NotifyWatcher` wraps `notify::RecommendedWatcher`.
- The watcher worker thread accumulates events for 200 ms of quiet, then reads the file from disk and pushes a single `AppEvent::FileChanged { path, contents }` onto the existing mpsc. Reading happens on the worker, never on the main loop.
- A `paused: Arc<AtomicBool>` mirrors `read_paused`. The external-editor flow flips it true before suspend and false after re-entry, then drains queued `FileChanged` events. On resume the watcher does one forced reconciliation read so we don't miss a change that fired during suspend.

## 3. Diff subsystem

```
src/diff.rs                     # facade
src/diff/ engine.rs                     # compute_hunks(old: &str, new: &str) state.rs                      # DiffState hunk.rs                       # Hunk, HunkKind, InlineSpan, Decision history.rs                    # DiffHistory: per-diff undo/redo stack
```

```rust
pub struct DiffState {   pub old_rope: Rope,            // pre-change in-memory buffer   pub new_rope: Rope,            // current working copy (starts =                                  //   on-disk content; user edits                                  //   mutate this rope)   pub hunks: Vec<Hunk>,          // recomputed after every mutation   pub current_idx: usize,   pub decisions: Vec<Decision>,   pub history: DiffHistory,      // see §6
}

pub enum Decision { Pending, Accepted, Rejected }

pub struct Hunk {   pub old_lines: Range<usize>,   // line indices into old_rope   pub new_lines: Range<usize>,   // line indices into new_rope   pub inline: Vec<InlineSpan>,   // per-line word-level deltas   pub kind: HunkKind,            // Replace | Insert | Delete
}
```

`DiffState::resolved_rope()` walks `hunks` in order, picking the
old-side or new-side line range per `decisions[i]`. When every
decision is non-`Pending`, the App swaps the resolved rope into
`editor.buffer`, clears `editor.diff`, records a single coarse
"Resolved diff" entry on the main `History`, and exits diff mode.

## 4. EditorState integration

```rust
pub struct EditorState {   // ...existing fields...   pub diff: Option<DiffState>,
}
```

- New `Mode::Diff` variant added to `editor::mode::Mode`. The invariant is `mode == Mode::Diff ⟺ diff.is_some()`, kept consistent by `enter_diff_mode()` / `exit_diff_mode()` helpers. The redundancy is intentional — existing `match state.mode { … }` dispatch in status-bar, hint-line, and `preview_safe_action` is cheaper to extend with one arm than to thread `Option<&DiffState>` everywhere.
- Edits in diff mode mutate `diff.new_rope`, **not** `editor.buffer`. After each edit the hunk list is recomputed (cheap — `similar` line-diff over a typical markdown file is sub-millisecond).

### Sub-modes within `Mode::Diff`

Mirroring the existing top-level Mode architecture, diff mode has two
sub-modes tracked by a `DiffSubMode` field on `DiffState`:

```rust
pub enum DiffSubMode { Review, Edit }
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
  hunk-range boundaries; attempts past the edge flash a status hint).
  Other hunks — including unchanged context lines and other add/delete
  hunks — are unreachable from within Edit; the only way to edit a
  different hunk is `Esc` → `Tab`/`Shift-Tab` → `Enter`. Inserting
  newlines is explicitly allowed and **expands** the focused hunk's
  range; the hunk grows downward as the new-side line count
  increases, and subsequent hunks shift down by the net delta. The
  hunk re-computation after each edit re-snaps the (now larger)
  focused hunk so its `HunkId` is preserved (§6). Decision keys
  (`y` / `n` etc.) are just characters in Edit — to make a decision
  the user must `Esc` first. Bulk-decision keys likewise have no
  special meaning in Edit. `Esc` returns to Review; edits are
  retained (already applied to `new_rope`).

The status-bar mode badge renders `DIFF` in Review and `DIFF·EDIT` in
Edit so the active sub-mode is always visible. The hint bar swaps
chord sets between sub-modes (see §9).

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

For each visible visual row, the widget emits a `Line<'static>` with:

- **Unchanged context** — borrowed from `new_rope`, no `Line.style`.
- **Delete-side lines** — from `old_rope`, `Line.style = Style::default().bg(theme.diff_delete_line.bg)`.
- **Add-side lines** — from `new_rope`, `Line.style = Style::default().bg(theme.diff_add_line.bg)`, with per-`Span` overrides on the inline-changed word ranges using `theme.diff_add_inline` / `diff_delete_inline` (brighter bg + bold).
- **Stacked order** — old lines first, then new lines (per spec)
- **Focused hunk** — `>` gutter glyph using `theme.diff_cursor_gutter`, and per-line `Decision` indicator (`[ ]` Pending, `[Y]` Accepted, `[N]` Rejected) at the line head. `Y`/`N` is preferred over `+`/`-` because the latter would collide visually with the line-add / line-delete semantics already conveyed by the background colors.

`render_line_from_visual` already propagates `line.style` across the
trailing cells (the same mechanism code blocks use), so a single
`Line.style` gives the full-width bg fill without changes to
`line_render`.

Keyboard scroll remains 1 line / step (per existing memory note); mouse
wheel honours the configured step.

## 6. Undo / redo of accept-reject decisions and in-diff edits

The design must accommodate both *kinds* of mutation that can happen inside diff mode:

1. Toggling a hunk decision (`Pending → Accepted`, `Pending →  Rejected`, `Accepted → Pending` via undo, etc.).
2. Editing the text inside an added hunk (mutates `new_rope`, which  then forces a hunk re-computation that may shift later hunk  indices).

### `DiffHistory`

A per-diff undo stack scoped to `DiffState`, independent of the main
`History` stack:

```rust
pub struct DiffHistory {   past: Vec<DiffOp>,   future: Vec<DiffOp>,
}

pub enum DiffOp {   /// Change a single hunk decision.   /// `hunk_id` is a stable per-hunk identifier captured at   /// `DiffState::new()`, NOT the hunk's current position, because   /// in-diff edits can reorder/insert/remove hunks.   Decision { hunk_id: HunkId, before: Decision, after: Decision },
   /// Bulk decision flip (accept-all / reject-all).   /// Stored as the full prior `Vec<Decision>` so that one Ctrl-Z   /// restores exactly the pre-bulk state in one step.   BulkDecision { before: Vec<Decision>, after: Decision /* the bulk target */ },
   /// A text edit applied to `new_rope` inside a focused add hunk.   /// Reuses the existing `EditDelta` type from `document::history`   /// so the same insert/delete primitives are reused.   Edit { delta: EditDelta },
}
```

`HunkId` is allocated monotonically at `DiffState::new` and re-used
across re-computations: when the engine reruns after an edit, it tries
to match new hunks against previous-pass hunks by overlap (start line
± a small tolerance) and re-uses the prior `HunkId`. Hunks that don't
match get a fresh ID. A `Decision` op whose `hunk_id` no longer
matches any current hunk is silently dropped during undo (rare;
happens when an edit collapses a hunk that previously had a recorded
decision — the decision is moot).

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
user must either resolve all hunks or trigger `Action::DiffExit`.

### Edit-then-decision interaction

When an `Edit` op shifts hunk offsets, subsequent decisions reference
the post-edit hunk by `HunkId`. Undoing an `Edit` reverses the rope
mutation and re-runs hunk computation, restoring the old hunk
boundaries. Undoing a `Decision` does not re-run hunk computation;
it just flips `decisions[i]`.

### Resolution checkpoint

When all decisions become non-`Pending` and `DiffState` resolves, the
resolved rope is swapped into `editor.buffer` and **`editor.history`
is cleared**. The diff merge is a hard checkpoint — undo cannot
cross it.

Rationale: if the diff merge were recorded as a single `EditDelta` on
the main `History`, `Ctrl-Z` could revert it. That's tolerable on its
own, but as soon as the user makes any subsequent edit, `future` is
cleared, and a later `Ctrl-Z` past the merge would silently destroy
the merge result with no path back. The safer model is that
in-diff undo/redo is fully bidirectional via `DiffHistory` while the
review is open; once the user resolves, that history is dropped along
with `editor.history`, and the resolved buffer is the new ground
truth. The `DirtyConflictModal`'s "Save a copy" option (§8) is the
escape hatch for users who want to preserve the pre-diff buffer
across the merge.

`editor.dirty` is set to `true` after resolution so the normal save
path can write the merged result to disk.

## 7. Theme additions

`Palette::diff_add` / `Palette::diff_delete` already exist as
`Color`s reserved for this feature. Promote them to full `Theme`
style fields and add the diff-mode signalling slots:

```rust
pub struct Theme {   // ...   pub diff_add_line: Style,   pub diff_delete_line: Style,   pub diff_add_inline: Style,        // brighter bg, bold   pub diff_delete_inline: Style,   pub diff_cursor_gutter: Style,   pub status_mode_diff: Style,   pub status_bar_diff: Style,        // saturated bg   pub hint_bar_diff: Style,
}
```

All derive from existing `Palette` slots in `Theme::from_palette`;
all overridable via `ThemeFile` TOML (`StyleSpec` mechanism).

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
| `[Save a copy]` | Write the current buffer to `<stem>.local.<ext>` next to the original path (with collision-avoiding numeric suffix if needed), then reload the on-disk file into `editor.buffer`. Flash the copy path on success. |
| `[Discard & reload]` | Drop the in-memory buffer, load the on-disk file, clear `editor.history`. Destructive — this option requires a second confirmation modal ("Discard your unsaved edits? They cannot be recovered."). |
| `[Keep buffer]` | Do nothing. The buffer remains dirty; the next save overwrites the on-disk changes. Equivalent to the old "Cancel". |

`Discard & reload` is the fourth option you asked about — included
because users occasionally want exactly that behavior (the on-disk
version is canonical, my edits were experimental), but gated behind
a confirmation because it's the only destructive choice.

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

### Review sub-mode binds

| Key | Action |
|---|---|
| `Tab` / `Shift-Tab` | next / previous hunk (`DiffNext` / `DiffPrev`) |
| `y` | accept current hunk and advance (`DiffAcceptHunk`) |
| `n` | reject current hunk and advance (`DiffRejectHunk`) |
| `Shift-Y` | accept all remaining `Pending` hunks (`DiffAcceptAll`) |
| `Shift-N` | reject all remaining `Pending` hunks (`DiffRejectAll`) |
| `Enter` or `i` | enter Edit sub-mode on the focused hunk (`DiffEnterEdit`) |
| `Ctrl-Z` / `Ctrl-Shift-Z` | undo / redo (routed to `DiffHistory`) |
| `Esc` | exit diff mode (see below) |

`y` / `n` over `a` / `r` follows the convention established by `git
add -p`, `jj split`, and most terminal accept/reject prompts; it also
matches the `[Y]` / `[N]` glyph indicators (§5). With `Tab` /
`Shift-Tab` for navigation, `y` / `n` are unambiguous bare keys —
there is no double-duty.

### Edit sub-mode binds

In Edit, the active key map is the **standard editor keymap** for the
focused hunk's clamped range, with three differences:

| Key | Action |
|---|---|
| `Esc` | exit Edit, return to Review (`DiffExitEdit`) |
| `Tab` / `Shift-Tab` | unchanged — standard list indent / tab character (does **not** navigate hunks while editing; user must `Esc` first) |
| `Ctrl-Z` / `Ctrl-Shift-Z` | undo / redo (routed to `DiffHistory`, including `Edit` ops) |

All other keys are normal editing. Decision keys (`y` / `n` /
`Shift-Y` / `Shift-N`) are just characters in Edit and insert as
typed.

Cursor-motion actions are clamped to the focused hunk's new-side line
range (§4). Newlines are allowed and expand the hunk downward. Any
attempt to move past the range edge flashes a status hint ("Esc to
leave hunk") and the cursor stays put.

### Hint line content

`hint_line_for` (`src/ui/bottom_region.rs`) gains a `Mode::Diff` arm
that further dispatches on the current `DiffSubMode`:

- **Review hint set:** `Tab/⇧Tab nav · y accept · n reject · ⇧Y/⇧N all · ⏎/i edit · ⌃Z/⌃⇧Z undo · Esc exit`
- **Edit hint set:** `Esc done · ⌃Z/⌃⇧Z undo · ⏎ newline · ⌫ delete`

Both sets render against `theme.hint_bar_diff` so the strong
diff-mode color is preserved across both sub-modes.

### `Esc` with unresolved changes (Review only)

Open a `DiffExitConfirmModal` (`ModalKind::Warning`, dismissable,
body "You have unresolved changes. Discard them and exit diff mode?",
buttons `[Keep reviewing]` default + `[Discard]`). `[Discard]`
reverts `editor.buffer` to the cached `old_rope` and clears
`editor.diff` and `editor.history`. `[Keep reviewing]` dismisses the
modal with no state change. If every hunk is already decided, `Esc`
is a no-op (resolution happens automatically the moment the last
`Pending` flips).

`Esc` in Edit sub-mode is intercepted *before* this path and simply
returns to Review — it never directly triggers diff exit.

## 10. Autosave + Ctrl-S in diff mode

- `App::tick_autosave` early-returns when `editor.diff.is_some()`. The autosave deadline is suppressed from `next_deadline` so the main loop doesn't wake spuriously.
- `Action::Save` is matched in the input dispatcher's diff-mode arm (mirroring `preview_safe_action`) and rewritten to a status flash ("Resolve diff to save").
- Manual `Action::SaveAs` is also disabled.

## 11. File-change events while already in diff mode

Any `AppEvent::FileChanged` received while `editor.diff.is_some()`
is queued (single-slot — newer overwrites older) on `App`. After diff
resolution completes, if a queued event exists the App immediately
re-evaluates: read the on-disk file fresh, diff against the just-
merged buffer, and if hunks are non-empty enter diff mode again. This
prevents a "diff mode forever" loop while still picking up further
changes.

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
- `tests/watcher.rs`
- `tests/diff_engine.rs`
- `tests/diff_history.rs`
- `tests/diff_view.rs`

**Modified files:**

- `src/app.rs` — `AppEvent::FileChanged`, `watcher` field, `diff_paused` flag, queued-event single-slot
- `src/app/event_loop.rs` — file-change arm, watcher pause/resume in external-editor flow, deadline integration
- `src/app/autosave.rs` — early-return when in diff mode
- `src/editor/state.rs` — `diff: Option<DiffState>` field, enter/exit helpers, undo/redo routing
- `src/editor/mode.rs` — `Mode::Diff` variant
- `src/editor/edit_ops.rs` — diff actions, in-hunk edit gating, undo/redo dispatch to `DiffHistory`
- `src/config/keymap.rs` — new `Action` variants and default binds
- `src/config/sections.rs` — `EditorConfig::show_diff_intro`
- `src/config/theme.rs` — new style fields and `Theme::from_palette` derivations
- `src/ui/editor_view.rs` — dispatch to `DiffView` when `diff.is_some()`
- `src/ui/status_bar.rs` — `Mode::Diff` mode badge + colored bar
- `src/ui/bottom_region.rs` — `Mode::Diff` hint set + colored hint bar
- `src/ui/settings_overlay/rows.rs` — `show_diff_intro` toggle row
- `src/input/mode_handler/default.rs` — diff-mode action dispatch + save-blocked flash

## 13. Tests

- **Watcher** (`tests/watcher.rs`): with a `tempfile`, write the file, wait, mutate it twice within 200 ms, assert exactly one `FileChanged` event is emitted with the latest contents. Use a `FakeFileWatcher` for the non-tempfile cases to avoid filesystem flakiness.
- **Diff engine** (`tests/diff_engine.rs`): snapshot tests (`insta::assert_debug_snapshot!`) over a handful of old/new pairs, including pure insert, pure delete, replace, multi-hunk, and inline-word-only changes.
- **Diff history** (`tests/diff_history.rs`): sequence of accept / reject / edit / undo / redo asserts. Includes the hunk-id stability case where an edit reshapes the hunk list.
- **Diff view rendering** (`tests/diff_view.rs`): `TestBackend` snapshot tests for the stacked old-above-new layout, the focused- hunk gutter glyph, and the inline word highlight overlay.
- **Modal flow** (extend `tests/ui.rs`): `DirtyConflictModal` → `DiffIntroModal` → diff view; opt-out via the checkbox sets `show_diff_intro = false` in the loaded config.
- **Autosave gating** (extend `tests/editing.rs`): set `editor.diff = Some(...)`, advance the autosave clock past `autosave_idle_ms`, assert no save fires.

## 14. Open points still worth confirming

1. **`[Y]` / `[N]` vs. checkmark glyphs.** ASCII `[Y]` / `[N]` reads
   the same in monochrome terminals and respects the
   `Theme::from_palette(monochrome=true)` path; Unicode `[✓]` / `[✗]`
   reads better in modern emoji-capable terminals but is wider in some
   fonts and inconsistent in others. **Recommendation: ASCII.**
   Confirm.

(All previous open points — `Esc` semantics, post-resolution undo
granularity, hunk-navigation key collision, and hunk-edit transitions
— have been resolved in §4, §6, §8, and §9.)

