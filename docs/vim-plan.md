# Vim Mode Implementation Plan

Reference document for the Vim-style modal editing feature in edamame.
Companion to the master `plan.md`; this file is the source of truth for the
modal-editing work specifically.

---

## 1. Goal and Scope

### Goal

Add Vim-style modal editing to edamame. This is the only modal editing style
edamame targets — Helix, Kakoune, and other schemes are explicitly not goals.
The feature must:

1. Coexist with edamame's existing `Mode::Preview` / `Mode::Rendered` /
   `Mode::Raw` rendering axis. Vim sub-modes (Normal/Insert/Visual/VisualLine)
   are nested inside `Rendered` and `Raw`.
2. Reuse edamame's Markdown-aware ops (list continuation, list indent, table
   navigation, GFM renumber).
3. Survive concurrent feature work — i.e. ship behind a config switch
   (`config.modal.handler = "vim"`) so default users are unaffected.

### Feature scope (target = Claude Code's Vim mode + counts + search + marks + ex)

**In scope** (across checkpoints):

- Modes: Normal, Insert, Visual (character-wise), Visual Line. Mode-switch
  keys: `Esc`, `i`, `I`, `a`, `A`, `o`, `O`, `v`, `V`.
- Motions: `h`, `j`, `k`, `l`, `w`, `e`, `b`, `0`, `$`, `^`, `gg`, `G`,
  `f{c}`, `F{c}`, `t{c}`, `T{c}`, `;`, `,`, `%` (matching pair), `{` / `}`
  (paragraph / block), `n` / `N` (search results, checkpoint 3),
  `` ` ``{c} / `'`{c} (marks, checkpoint 3).
- Editing primitives (Normal): `x`, `X`, `dd`, `D`, `dw` / `de` / `db`,
  `cc`, `C`, `cw` / `ce` / `cb`, `yy`, `Y`, `yw` / `ye` / `yb`,
  `p`, `P`, `>>`, `<<`, `J`, `u`, `Ctrl-R`, `.`, `r{c}`, `~`.
- Text objects (operator-pending, checkpoint 2): `iw` / `aw`, `iW` / `aW`,
  `i"` / `a"`, `i'` / `a'`, `` i` `` / `` a` ``, `i(` / `a(` / `i)` / `a)`,
  `i[` / `a[` / `i]` / `a]`, `i{` / `a{` / `i}` / `a}`.
- Visual mode (operators on selection): `d` / `x`, `y`, `c` / `s`, `p`,
  `r{c}`, `~` / `u` / `U`, `>` / `<`, `J`, `o` (swap cursor and anchor),
  `iw` / `aw` / etc. (text-object selection), `v` / `V` (toggle / exit).
- Count prefixes (checkpoint 3): `3j`, `5dw`, `2dd`, `3>>`. Both
  `[count][operator][motion]` and `[operator][count][motion]` shapes.
- Search (checkpoint 3): `/pattern`, `?pattern`, `n`, `N`, `*` (word under cursor),
  `#` (word under cursor reverse). Match highlighting in Rendered + Raw.
- Marks (checkpoint 3): `m{c}` to set, `` ` ``{c} (exact offset) and
  `'`{c} (line start) to jump.
- Ex commands (checkpoint 4): `:w`, `:q`, `:wq`, `:e <path>`,
  `:s/pattern/replacement/flags` (`g`, `i`).

**Out of scope** (deferred — explicitly _not_ designed for; do not block
Checkpoint 1–4 design on these):

- Named registers (`"ay`, `"ap`)
- Macros (`q{r}` recording, `@{r}` replay)
- Block-wise Visual mode (`Ctrl-V`)
- Vim's full Ex command suite (`:bn`, `:bp`, etc. — only the subset above)
- Visual-block-specific operators (`I` / `A` / `c` in block mode)
- Window splits (`:sp`, `:vsp`) — edamame doesn't need them.

### Design decisions

| Decision | Choice |
|---|---|
| Vim modes vs edamame modes | Nested inside `Mode::Rendered` / `Mode::Raw`. `Mode::Preview` unchanged. |
| Edit-entry from Preview | Lands in Vim Normal. |
| Esc in Vim Normal | Returns to `Mode::Preview` (overrides "vim Esc is no-op"). |
| Esc in Vim Insert | Transitions to Normal + cursor `MoveLeft` (vim convention). |
| Ctrl-* keymap chords | Always honored, even in Normal. (Ctrl-S, Ctrl-P, etc. fire from the keymap.) |
| Markdown-aware ops | Reused. `o` continues lists; `dd` renumbers; `>>` indents list items; etc. |
| Vim sub-mode location | Inside `VimHandler`. Surfaced via trait method. |
| `RAW_REVEAL_DELAY` in Normal | Suppressed via `EditorState::suppress_raw_reveal`. Block stays rendered. |
| User keybindings in Normal | Bare keys are vim motions. Ctrl-* chords still consult the keymap. |
| Library vs build-from-scratch | Build from scratch. Reference: `tui-textarea/examples/vim.rs` skeleton. Use `regex` crate for checkpoint-3 search. |

---

## 2. Architecture (Hybrid)

### 2.1 Trait extension

Replaces `src/input/modal.rs`:

```rust
use crossterm::event::{Event, KeyEvent, KeyEventKind};

use crate::config::{Action, KeyMap};
use crate::editor::EditorState;

pub mod default;
pub mod vim;

/// What a `ModalHandler` returns after processing a single key event.
pub enum HandleResult {
    /// The key was consumed; apply these actions in order.
    /// An empty vec = "consumed, no effect" (intermediate key in a
    /// multi-key sequence).
    Consumed(Vec<Action>),
    /// The handler is mid-sequence (operator-pending, count accumulating,
    /// `g` waiting for a follow-up). No mutation occurred.
    Pending,
    /// The handler did not recognise the key. Caller falls back.
    PassThrough,
    /// The handler wants the App to open a UI overlay (search prompt,
    /// ex command line). Subsequent input is consumed by the overlay.
    Overlay(OverlayRequest),
}

#[derive(Debug, Clone, Copy)]
pub enum OverlayRequest {
    SearchForward,
    SearchBackward,
    ExCommand,
}

pub trait ModalHandler {
    /// Process a single key press. May mutate `state` directly when an op
    /// has no clean `Action` representation (e.g. `r{c}`, `J`, `ciw`).
    fn handle(&mut self, event: KeyEvent, state: &mut EditorState) -> HandleResult;

    fn name(&self) -> &'static str;

    /// Sub-mode label for the status bar. `None` falls back to
    /// `state.mode.to_string()`. VimHandler returns `Some("NORMAL")` etc.
    fn mode_label(&self) -> Option<&str> { None }

    /// Pending-input hint for the hint line. e.g. `"3"`, `"d"`, `"d3i"`.
    fn pending_hint(&self) -> Option<String> { None }

    /// Notify the handler that the App keymap was rebound. Default no-op.
    /// `DefaultHandler` overrides to update its owned copy.
    fn update_keymap(&mut self, _keymap: &KeyMap) {}

    /// Convenience: filter to KeyPress events, delegate to `handle`.
    fn handle_event(&mut self, event: Event, state: &mut EditorState) -> HandleResult {
        match event {
            Event::Key(k) if k.kind == KeyEventKind::Press => self.handle(k, state),
            _ => HandleResult::PassThrough,
        }
    }
}
```

**Why `&mut EditorState`**: avoids `Box<dyn FnOnce>` closures in the
handler (which were Architecture A). The handler can call free functions
in `editor/region_edit.rs` directly to perform compound ops that don't
fit a single `Action`.

**Why `Consumed(Vec<Action>)`**: covers the common case of "vim emitted N
existing actions in order" (`o` = `MoveLineEnd` then `Newline`). Empty
vec is semantically distinct from `Pending` — `Pending` is "wait, more
keys coming"; `Consumed(vec![])` is "nothing to do but I claimed the
key". In practice we always use `Pending` in that case.

### 2.2 New `Action` variants

Per the principle "if the op is useful outside vim, it's an Action":

| Variant | Semantics |
|---|---|
| `Action::JoinLines` | Join the current line with the next (replace the trailing `\n` with a single space, collapsing leading whitespace on the next line). |
| `Action::IndentLine` | Insert `tab_width` spaces at line start. In a list item, delegates to `list_edit::indent`. |
| `Action::OutdentLine` | Remove up to `tab_width` leading spaces. In a list item, delegates to `list_edit::dedent`. |
| `Action::ToggleCaseAtCursor` | Flip case of the char under the cursor and advance one char. |
| `Action::LowercaseSelection` | Lowercase the active selection (or the cursor word in checkpoint 2). |
| `Action::UppercaseSelection` | Uppercase the active selection. |
| `Action::PasteAbove` | Insert kill-ring above the current line (linewise) or before the cursor (charwise). |
| `Action::PasteBelow` | Insert kill-ring below the current line (linewise) or after the cursor (charwise). |

All of these are palette-exposable and rebindable.

`Action::Paste` is preserved unchanged for the default handler.

### 2.3 Module layout

Following the project's facade convention (`src/foo.rs` + `src/foo/`):

```
src/
  input/
    modal.rs                       # MODIFIED: HandleResult, OverlayRequest, trait
    modal/
      default.rs                   # MODIFIED: HandleResult return; owned KeyMap
      vim.rs                       # NEW (facade): pub mod vim; pub use vim::VimHandler
      vim/
        mod.rs                     # VimHandler struct + ModalHandler impl + Normal dispatch
        state.rs                   # VimMode, PendingOp, VimState, LastChange, SearchDirection
        normal.rs                  # Normal-mode key dispatch
        insert.rs                  # Insert-mode key dispatch (mostly Esc handling)
        visual.rs                  # Visual + VisualLine dispatch
        text_object.rs             # iw, aw, i", a", i(, a(, ... (checkpoint 2)
        ex.rs                      # Ex command parser (checkpoint 4)
  editor/
    motion.rs                      # NEW: Motion enum + target()
    region_edit.rs                 # NEW: handler-agnostic compound ops
                                   #      (delete_region, change_region,
                                   #       replace_char_at, join_lines,
                                   #       toggle_case_region, ...)
    search.rs                      # NEW (checkpoint 3): pure pattern→Vec<Range<usize>>
  config/
    config.rs                      # MODIFIED: ModalConfig::handler validation
    keymap.rs                      # MODIFIED: new Action variants + parsing
    theme.rs                       # MODIFIED (checkpoint 3): search_highlight Style
    theme_file.rs                  # MODIFIED (checkpoint 3): search_highlight field
    themes/default.toml            # MODIFIED (checkpoint 3): search_highlight color
  ui/
    status_bar.rs                  # MODIFIED: vim_mode_label slot
    bottom_region.rs               # MODIFIED: pending_hint hook
    line_render.rs                 # MODIFIED (checkpoint 3): search-match overlay
  editor/
    edit_ops.rs                    # MODIFIED: new Action arms; linewise paste
    state.rs                       # MODIFIED: kill_ring_linewise,
                                   #          suppress_raw_reveal,
                                   #          search_pattern (checkpoint 3),
                                   #          search_matches (checkpoint 3)
  app.rs                           # MODIFIED: modal_handler field; run loop
  config/
    config.toml                    # MODIFIED: [modal] handler doc string
tests/
  vim_basics.rs                    # NEW (checkpoint 1)
  vim_operators.rs                 # NEW (checkpoint 2)
  vim_count_search_marks.rs        # NEW (checkpoint 3)
  vim_ex.rs                        # NEW (checkpoint 4)
```

### 2.4 `EditorState` additions

Checkpoint 1:

```rust
/// Set by vim `yy` / `dd` (linewise yank/delete). Read by `PasteAbove` /
/// `PasteBelow`. Cleared by character-wise yank/delete.
pub kill_ring_linewise: bool,

/// When true, `cursor_block_revealed()` always returns false — the cursor
/// block stays rendered. Set when VimHandler is in Normal mode to prevent
/// the 120ms raw-reveal flicker during navigation.
pub suppress_raw_reveal: bool,
```

Checkpoint 3:

```rust
/// Active search pattern. `None` when search is inactive. Cleared on
/// `ExitToPreview` and on empty-pattern search.
pub search_pattern: Option<String>,

/// Char-offset ranges of all current search matches in the buffer.
/// Updated whenever `search_pattern` changes. Read by `line_render` to
/// apply `Theme::search_highlight` overlay.
pub search_matches: Vec<std::ops::Range<usize>>,

/// Index into `search_matches` for the cursor's "current" match (set by
/// `n` / `N`). When set, the renderer uses a more emphasised highlight.
pub active_search_match: Option<usize>,
```

`cursor_block_revealed()` (state.rs:562) gains a one-line guard:

```rust
pub fn cursor_block_revealed(&self) -> bool {
    if self.suppress_raw_reveal { return false; }
    if self.drag_in_progress    { return false; }
    match self.cursor_block_entered_at {
        None => true,
        Some(t) => t.elapsed() >= RAW_REVEAL_DELAY,
    }
}
```

### 2.5 App-level wiring

**Field added to `App`** (around line 211 of `src/app.rs`):

```rust
/// Long-lived modal input handler. Built once in `run()` from
/// `config.modal.handler`. Replaces the per-iteration
/// `DefaultHandler::new(&keymap)` construction at app.rs:1696.
modal_handler: Box<dyn ModalHandler>,
```

**`DefaultHandler` lifetime fix.** Currently `DefaultHandler<'k>` borrows
`&'k KeyMap`. To live in a `Box<dyn ModalHandler>` it must own its keymap:

```rust
pub struct DefaultHandler {
    keymap: KeyMap,
}
impl DefaultHandler {
    pub fn new(keymap: KeyMap) -> Self { Self { keymap } }
}
```

`KeyMap: Clone` (already). Run-loop clones once at construction; the
per-iteration `keymap.clone()` at app.rs:1692 is removed.

**`update_keymap` propagation.** The keybinds overlay calls
`self.keymap.rebind(...)` on the live App keymap. After every rebind:

```rust
self.modal_handler.update_keymap(&self.keymap.as_ref().unwrap().clone());
```

`DefaultHandler::update_keymap` overrides to replace its owned copy.
`VimHandler::update_keymap` likewise (vim consults the keymap for Ctrl-*
chord pass-through).

**Run-loop dispatch** (replaces app.rs:1691-1757):

```rust
let result = self.modal_handler.handle_event(event, &mut self.editor);

// Sync the reveal-delay flag based on the handler's mode.
self.editor.suppress_raw_reveal =
    matches!(self.modal_handler.mode_label(), Some("NORMAL"));

match result {
    HandleResult::PassThrough => { /* ignored */ }
    HandleResult::Pending => { self.needs_draw = true; }
    HandleResult::Overlay(req) => {
        self.open_modal_overlay(req);
        self.needs_draw = true;
    }
    HandleResult::Consumed(actions) => {
        for action in actions {
            // Existing sticky-error swallow (app.rs:1702):
            if matches!(action, Action::ExitToPreview)
                && self.dismiss_sticky_transient()
            {
                self.needs_draw = true;
                break;
            }
            // App-level handlers first:
            let handled = self.handle_app_action(&action, doc_height, doc_width);
            if !handled {
                if matches!(action, Action::Quit) && self.editor.dirty {
                    self.open_quit_confirm();
                    break;
                }
                let save_before_dirty = self.editor.dirty;
                let scroll_before = self.editor.scroll;
                let quit = edit_ops::apply(
                    &mut self.editor, action.clone(), doc_height, doc_width,
                );
                if quit { self.should_quit = true; }
                if self.editor.scroll != scroll_before { self.mark_scrolling(); }
                self.flash_for_action(&action, save_before_dirty);
                if let Some(target) = self.editor.pending_link_follow.take() {
                    self.follow_link(target, doc_height, doc_width);
                }
            }
        }
        self.needs_draw = true;
    }
}
```

### 2.6 Status bar / hint line

`StatusBarState` gains:

```rust
pub vim_mode_label: Option<String>,
```

The status bar's mode badge prefers `vim_mode_label` over
`mode.to_string()`. Display: `" NORMAL "` / `" INSERT "` / `" VISUAL "` /
`" V-LINE "` (replaces `" EDIT "`).

Hint line (Phase 9 channel) gains a "modal-pending" slot. The App
populates it from `self.modal_handler.pending_hint()` before each draw.
When `Some`, render in place of the chord hints.

### 2.7 Test pattern

```rust
// tests/vim_basics.rs

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use edamame::config::{KeyBindingOverrides, KeyMap, Theme};
use edamame::document::Buffer;
use edamame::editor::{edit_ops, EditorState, Mode};
use edamame::input::modal::{HandleResult, ModalHandler};
use edamame::input::modal::vim::{VimHandler, VimMode};

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

fn keymap() -> KeyMap {
    KeyMap::build(&KeyBindingOverrides::default()).unwrap()
}

fn run(handler: &mut VimHandler, state: &mut EditorState, keys: &str) {
    for ch in keys.chars() {
        let evt = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
        if let HandleResult::Consumed(actions) = handler.handle(evt, state) {
            for action in actions {
                edit_ops::apply(state, action, 24, 80);
            }
        }
    }
}

fn run_special(
    handler: &mut VimHandler,
    state: &mut EditorState,
    code: KeyCode,
    mods: KeyModifiers,
) {
    let evt = KeyEvent::new(code, mods);
    if let HandleResult::Consumed(actions) = handler.handle(evt, state) {
        for action in actions {
            edit_ops::apply(state, action, 24, 80);
        }
    }
}

#[test]
fn dw_deletes_next_word() {
    let mut state = EditorState::new(Buffer::from_str("hello world\n"), theme());
    state.mode = Mode::Rendered;
    let mut handler = VimHandler::new(keymap());

    run(&mut handler, &mut state, "dw");

    assert_eq!(state.contents(), "world\n");
    assert_eq!(state.cursor.offset, 0);
    assert_eq!(state.kill_ring, "hello ");
    assert!(!state.kill_ring_linewise);
}
```

---

## 3. Checkpoint 1 — Foundation, motions, basic edits, visual mode

**Status: COMPLETE — landed 2026-04-26 / 2026-04-27.**
1582 tests pass (45 new in `tests/vim_basics.rs` + all existing).
Zero new clippy errors over the pre-existing baseline.  Pre-existing
clippy errors in `src/ui/table_view.rs` and elsewhere are tracked tech
debt, not introduced by this checkpoint.

### 3.1 Goal — done

Ship a usable vim experience covering the Claude Code spec's motion +
basic-editing surface plus visual mode.  After Checkpoint 1 a Vim user can
navigate, type, delete, yank, paste, and select with familiar
bindings.  No counts, no operator-pending text objects, no search, no
marks, no ex — those land in checkpoints 2-4.  Operator + motion
composition (`dw` / `db` / `de` / `yw` / `ye` / `yb` and the same-key
linewise `dd` / `yy`) is in place because the resolver was small
enough to land alongside the rest.

### 3.2 Trait + App scaffolding — done

- [x] Replace `src/input/modal.rs` with the trait + `HandleResult` +
      `OverlayRequest` (see §2.1).
- [x] Update `src/input/modal/default.rs`:
  - Owned `KeyMap` (no lifetime parameter).
  - Returns `HandleResult::Consumed(vec![action])` / `PassThrough`.
  - Implements `update_keymap`.
- [x] **Two-handler chain on `App`** instead of a single
      `Box<dyn ModalHandler>`.  `App` carries
      `modal_default: Option<Box<dyn ModalHandler>>` (always present
      after `run()`) plus `modal_vim: Option<Box<dyn ModalHandler>>`
      (`Some` when `config.modal.handler == "vim"`).  This is the
      §3.10 option (a) shape — the §2.5 / §3.2 sketch of a single
      `modal_handler` field was superseded because Insert-mode
      `PassThrough` from `VimHandler` needs to fall through to
      `DefaultHandler` for printable-char insertion.
- [x] Replace per-iteration handler construction with
      `App::dispatch_modal_event` (Vim-first, falls through to default
      on `PassThrough`).  Run loop matches on `HandleResult` arms.
- [x] After each `handle()`, sync
      `self.editor.suppress_raw_reveal = matches!(label, Some("NORMAL"))`.
- [x] Propagate keymap rebinds: in `App::handle_keybinds_overlay_key`'s
      `KeybindsResponse::Rebound` arm, `update_keymap` is called on
      both `modal_default` and `modal_vim` (when present).
- [x] `config/config.toml` documents `handler = "vim"` with the
      Checkpoint-1 feature surface inline.  No new validation in
      `ModalConfig::handler` — the `App::run()` constructor branches
      on `"vim"` and silently treats anything else as default
      (deliberate: matches the existing string-tolerance posture).

### 3.3 EditorState additions — done

- [x] `kill_ring_linewise: bool` on `EditorState`, default `false`.
- [x] `suppress_raw_reveal: bool` on `EditorState`, default `false`.
- [x] `cursor_block_revealed()` short-circuits on `suppress_raw_reveal`
      *before* the existing `drag_in_progress` check.
- [x] **`Cut` / `Copy` arms set `kill_ring_linewise` based on
      selection presence**, not always `false` as the plan suggested:
  - With selection → `kill_ring_linewise = false` (charwise).
  - Without selection → `kill_ring_linewise = true` (the default-handler
    "Cut/Copy current line" path is naturally linewise; this lets a
    subsequent vim `p` / `P` open a new line as expected).

### 3.4 New `Action` variants — done

- [x] Added to `src/config/keymap.rs::Action`:
  - `JoinLines`, `IndentLine`, `OutdentLine`, `PasteAbove`, `PasteBelow`.
- [x] `Display` / `FromStr` round-trip covered by
      `tests/vim_basics.rs::new_action_variants_roundtrip`.
- [x] All five variants implemented in `src/editor/edit_ops.rs::apply`:
  - `JoinLines` → `join_lines_at_cursor` helper (replaces `\n` + leading
    whitespace with one space; cursor lands on the inserted space).
  - `IndentLine` → `list_indent` first; falls back to
    `indent_line_with_spaces` (prepend `tab_width` spaces to line start,
    shift cursor by `tab_width`).
  - `OutdentLine` → `list_outdent` first; falls back to
    `outdent_line_remove_spaces` (strip up to `tab_width` leading
    spaces, walk cursor back).
  - `PasteAbove` / `PasteBelow` → `paste_linewise_or_charwise(state, below)`:
    branches on `kill_ring_linewise`; linewise inserts at line start
    (`P`) or after current line's `\n` (`p`); charwise replaces selection
    if any, else inserts at cursor (`P`) or one char past (`p`).  Handles
    the "last line without trailing `\n`" case by injecting a leading
    `\n` so the new line lands cleanly.
- **Deferred to Checkpoint 2** (per the plan's "if convenient" caveat —
  weren't): `ToggleCaseAtCursor`, `LowercaseSelection`,
  `UppercaseSelection`.  The `region_edit::toggle_case_at` helper IS
  already written (used by Checkpoint 2 for `~`).

### 3.5 `editor/motion.rs` — done

Shipped Motion variants (the Checkpoint-1 set):
`Left`, `Right`, `Up`, `Down`, `WordForward`, `WordForwardEnd`,
`WordBack`, `LineStart`, `LineFirstNonBlank`, `LineEnd`, `DocStart`,
`DocEnd`, `FindCharForward { ch, before }`, `FindCharBackward { ch, before }`.

**Deferred to checkpoint 3** (not declared yet — the enum is exhaustively
matched, so adding them requires touching every `match Motion { … }`
site, and we don't want stub-arms cluttering the codebase):
`SearchNext`, `SearchPrev`, `ToMarkExact`, `ToMarkLineStart`,
`MatchingPair`, `ParagraphForward`, `ParagraphBack`.  The plan at the
top of this file lists them so future checkpoints remember.

`MotionTarget` trait shipped with `target()` + `is_linewise()` (default
`false`; `Motion::DocStart` / `DocEnd` override to `true`).

`vim_word_forward` / `vim_word_back` / `vim_word_end` use the
three-class boundary (`Word` for `[A-Za-z0-9_]`, `Punct` for everything
else non-whitespace, `Whitespace`).  Test coverage in
`src/editor/motion.rs::tests` confirms `foo.bar` advances `f → . → b`,
plus boundary cases for `b` / `e` at start / end of line.

`find_char_forward` / `find_char_backward` are `pub fn`s exposed for
direct use by the handler's pending-find resolver; they stop at `\n`
(current-line vim convention).

### 3.6 `editor/region_edit.rs` — done

Handler-agnostic compound ops, all on `&mut EditorState`.  Shipped:

- `yank_region(state, start, end, linewise)` — fills kill ring; sets
  `kill_ring_linewise`.
- `delete_region(state, start, end, linewise)` — yank + delete in one
  `EditDelta`; cursor lands at `start`.
- `replace_char_at(state, cursor, ch)` — vim `r{c}`; refuses to clobber
  `\n`; cursor stays on the replaced char.
- `toggle_case_at(state, cursor)` — vim `~` in Normal; advances cursor
  by one (vim semantics) even when the char is non-letter.
- `open_line_above(state, line_idx)` — splice `\n` at line start;
  cursor on the new blank line.

**Not yet shipped** (Checkpoint 2 will add):
`change_region` (= `delete_region` then return `Action::EnterEditMode`).
The plan listed it; held off because Checkpoint 1 doesn't have any
`c{motion}` callers.  Adding it is one short function when Checkpoint 2
needs it.

Tests in `src/editor/region_edit.rs::tests` cover linewise vs charwise
flag, yank-without-mutation, replace_char_at skip-on-newline, and
toggle_case_at advance-through-punct.

### 3.7 `input/modal/vim/state.rs` — done

Shipped variants:

- `VimMode::{Normal, Insert, Visual, VisualLine}` — `Default = Normal`.
- `Operator::{Delete, Yank}` — Checkpoint 2 will add `Change`, `Indent`,
  `Dedent`, `ToggleCase`, `Uppercase`, `Lowercase`.
- `PendingOp::{Operator(Operator), FindChar { forward, till }, GPrefix}`
  — Checkpoint 2 adds `ReplaceChar`; Checkpoint 3 adds `SetMark`,
  `JumpMark { line_only }`.
- `LastFind { ch, forward, till }` with helper `as_motion(reverse)` for
  `;` and `,`.
- `VimState { mode, pending, last_find, _marks_reserved }`.

**Deviation from plan**:
- The plan declared every future-checkpoint field upfront (`pending_count`,
  `last_change`, `marks`, `last_search`, `insert_capture`) so Checkpoint 2-4
  could just populate them.  Implementation defers each until its
  checkpoint — only `_marks_reserved: HashMap<char, usize>` is pre-allocated
  (a placeholder so adding marks doesn't touch every constructor).
  Reason: dead fields trigger `dead_code` warnings unless prefixed
  with `_`, and mass-prefixing felt sloppier than just adding them
  per-checkpoint.

### 3.8 `input/modal/vim.rs` — VimHandler — done

Lives at `src/input/modal/vim.rs` (facade file containing the handler
itself; `vim/state.rs` is the only sibling module so far).  Field
names ended up as:

```rust
pub struct VimHandler {
    pub vim: VimState,        // (plan called this `state`; renamed to
    keymap: KeyMap,           //  avoid shadowing the &mut EditorState
    pending_hint: String,     //  parameter named `state`)
}
```

Mode dispatch follows the plan exactly: `handle_normal`, `handle_insert`,
`handle_visual` (with a `linewise: bool` param distinguishing Visual
from VisualLine).  Ctrl-* dispatch lives in the inline handler body
(see §3.9 below) — there's no separate `handle_ctrl` method, and the
order is keymap-first / vim-claimed-default-second (deviation —
see §3.9).

`refresh_pending_hint()` rebuilds the `pending_hint: String` field
after every keystroke so the trait method just clones the string.

### 3.9 Ctrl-* delegation rule — done (with inverted priority)

**Deviation from plan**: the implementation routes Ctrl-* chords
**through the keymap FIRST**, then to vim-claimed defaults only when
the keymap has no binding.  The plan had vim-claimed-first.  Inverted
because user keybinding overrides should always win — if a user
deliberately rebinds `Ctrl-R` to something other than Redo, vim must
not silently shadow it.

Wired vim-claimed Ctrl chords (in `vim_ctrl_default`):

- `Ctrl-R` → `Action::Redo`
- `Ctrl-F` → `Action::ScrollPageDown`
- `Ctrl-B` → `Action::ScrollPageUp`

**Not wired** (the plan listed but I deferred until a user asks):
`Ctrl-D` (half-page down), `Ctrl-U` (half-page up), `Ctrl-E` /
`Ctrl-Y` (scroll one line).  The existing default keymap's
`Ctrl-D = DeleteLine` and `Ctrl-U = DeleteToLineStart` therefore still
fire from any vim mode, including Normal.  This is more compatible
with existing edamame muscle memory than the plan's "vim takes over"
posture, and any user who wants the half-page behaviour can rebind
those chords or wait for Checkpoint 2 to address it.

Unknown Ctrl chord (no keymap entry, no vim claim) → `PassThrough` so
the default handler can take its turn (which currently also returns
`PassThrough` for unknown Ctrl chords — net effect: ignored).

### 3.10 Checkpoint 1 key tables — done

**Normal mode** — exactly as planned, with these implementation notes:

| Key | Implementation |
|---|---|
| `h` `Left` / `l` `Right` / `k` `Up` / `j` `Down` | dispatched as `Action::Move{Left,Right,Up,Down}` |
| `w` / `e` / `b` | direct cursor mutation via `Motion::WordForward` / `WordForwardEnd` / `WordBack` (avoids round-trip through `Action`) |
| `0` | `Action::MoveLineStart` |
| `^` | direct mutation via `Motion::LineFirstNonBlank` |
| `$` / `G` | `Action::MoveLineEnd` / `Action::MoveDocEnd` |
| `gg` | `PendingOp::GPrefix` → on second `g`, `Action::MoveDocStart`; any other key clears |
| `f`/`F`/`t`/`T` | `PendingOp::FindChar`; resolution stores `LastFind` |
| `;` / `,` | replay `last_find` (`,` flips direction via `LastFind::as_motion(reverse: true)`) |
| `i` | mode = Insert, emits `[EnterEditMode]` |
| `I` | mode = Insert, emits `[EnterEditMode, MoveLineStart]` (vim `I` = line start, NOT first non-blank) |
| `a` / `A` / `o` | mode = Insert, emit MoveRight / MoveLineEnd / `[MoveLineEnd, Newline]` |
| `O` | direct `region_edit::open_line_above` then mode = Insert |
| `v` / `V` | set selection anchor at cursor, mode = Visual / VisualLine |
| `Esc` | `Action::ExitToPreview` |
| `x` / `X` | `DeleteCharForward` / `DeleteCharBack` |
| `dd` | yank current line linewise via `region_edit::yank_region`, then `Action::DeleteLine` |
| `D` | direct `region_edit::delete_region` from cursor to line end (charwise) |
| `dw` / `de` / `db` | resolved via `resolve_operator_motion` → `region_edit::delete_region(cursor → motion_target, false)` |
| `yy` / `Y` | direct `yank_current_line` (linewise) |
| `yw` / `ye` / `yb` | `resolve_operator_motion` → `region_edit::yank_region(charwise)` |
| `p` / `P` | `Action::PasteBelow` / `Action::PasteAbove` |
| `J` | `Action::JoinLines` |
| `u` | `Action::Undo`. Redo is `Ctrl-R` (vim convention). |

**Implementation note on `dd`**: `Action::DeleteLine` does NOT populate
the kill ring on its own (charwise / linewise distinction is a
vim concept).  So vim's `dd` calls `yank_current_line` first, then
dispatches `DeleteLine`.  Without this the `kill_ring_linewise` flag
would be set but the kill_ring itself empty — `dd_deletes_line_linewise`
test caught this during development.

`r{c}`, `~`, `cw`, `ce`, `cb`, `cc`, `>>`, `<<`, `ciw`, `daw`, `iw`,
text objects, dot-repeat, count prefixes — DEFERRED to Checkpoint 2.

**Insert mode** — done.  Esc → mode = Normal + emits `[Action::MoveLeft]`.
Everything else returns `HandleResult::PassThrough` so the App's
`dispatch_modal_event` chain falls through to the default handler's
printable-char + Backspace + Enter (list-aware) + Tab (list-indent)
machinery.

**Two-handler chain on `App`** — implemented as plan §3.10 option (a):

```rust
fn dispatch_modal_event(&mut self, event: Event) -> HandleResult {
    if let Some(vim) = self.modal_vim.as_mut() {
        let result = vim.handle_event(event.clone(), &mut self.editor);
        if !matches!(result, HandleResult::PassThrough) {
            return result;
        }
    }
    if let Some(default) = self.modal_default.as_mut() {
        default.handle_event(event, &mut self.editor)
    } else {
        HandleResult::PassThrough
    }
}
```

Status-bar `mode_label` / `pending_hint` come from `modal_vim` only;
the default handler returns `None` for both.

**Visual mode** — done.  Implementation differs from the plan in one
small way: motions extend the selection by directly mutating
`state.selection.active` and `state.cursor.offset` in
`update_visual_cursor`, NOT by emitting `Action::Select{Left,Right,Up,Down}`.
This was necessary for word motions (`w`/`b`/`e`) and find motions
(`f`/`F`/`t`/`T`) which have no `SelectWord*` actions in the enum.
For consistency, `hjkl` in Visual also use direct mutation rather
than the existing `Select*` actions — keeps the Visual-extend code
path uniform.

| Key | Effect |
|---|---|
| `Esc` / `v` (in Visual) / `V` (in V-LINE) | mode = Normal; clear selection |
| `V` in Visual | promote to VisualLine |
| `v` in V-LINE | demote to Visual |
| `h`/`l`/`k`/`j` / `w`/`b`/`e` / `0`/`^`/`$` / `G` / `gg` | direct cursor + `selection.active` mutation |
| `f`/`F`/`t`/`T` / `;` / `,` | same (find motions extend selection) |
| `d` / `x` | snap selection to line bounds if linewise; emit `[Cut]`; mode = Normal |
| `y` | snap if linewise; emit `[Copy]`; mode = Normal |
| `c` / `s` | snap if linewise; emit `[Cut]`; mode = Insert |
| `o` | swap anchor and active in selection |

`>`, `<`, `J` in Visual mode, plus `iw`/`aw` text objects, and
`u`/`U`/`~` case ops — DEFERRED to Checkpoint 2.

VisualLine selection: implemented as `consume_visual_selection` —
called only at consume-time (when `d`/`y`/`c` fires).  Snaps
`anchor` and `active` to whole-line bounds at that moment, preserving
their relative order so the cursor lands on the natural side after
the op.  This is simpler than the plan's "snap on every extend"
sketch and produces the same observable behaviour.

### 3.11 Status bar / hint line wiring — partially done

- [x] `StatusBarState` gained `vim_mode_label: Option<&'a str>` (a
      borrowed `&str` rather than `String` — matches `filename`'s
      shape and avoids per-frame allocation).
- [x] `EditorView` carries `vim_mode_label: Option<&'a str>` and
      threads it into `StatusBarState`.
- [x] App populates it via
      `self.modal_vim.as_ref().and_then(|h| h.mode_label())` before
      the `terminal.draw` call.
- [x] `StatusBar::render` prefers `vim_mode_label` over
      `mode.to_string()` for the badge.

**Gap — `pending_hint` is wired to the trait but not yet displayed**:
the trait method `ModalHandler::pending_hint() -> Option<String>` is
implemented and `VimHandler::refresh_pending_hint` rebuilds the hint
on every keystroke (`"d"` after `d`, `"f"` after `f`, `"g"` after `g`).
But the hint line itself doesn't render the value yet — `bottom_region`'s
`HintContent` enum was not extended.

This is harmless: vim's pending sequences resolve on the next
keystroke in <1 second, so the user never sees a stuck-pending
state.  Checkpoint 3's count-prefix and search overlays will need this
slot anyway, so the plan defers the display wiring to Checkpoint 3 where
multi-keystroke pending state actually matters (`3`, `35`, `35d`,
`35dw`).

### 3.12 Checkpoint 1 tests — done (45 in `tests/vim_basics.rs`)

Coverage by category:

- **Mode transitions (9 tests)**: `starts_in_normal_mode`,
  `i_enters_insert_mode`, `esc_in_insert_returns_to_normal_with_cursor_back`,
  `esc_in_normal_emits_exit_to_preview`,
  `capital_i_moves_to_line_start_and_inserts`,
  `lowercase_a_appends_after_cursor`, `capital_a_appends_at_line_end`,
  `o_opens_line_below_and_enters_insert`,
  `capital_o_opens_line_above_and_enters_insert`.
- **Motions (10 tests)**: `hjkl_move_one_char`, `w_jumps_to_next_word`,
  `b_jumps_to_previous_word_start`, `dollar_moves_to_line_end`,
  `zero_moves_to_line_start`, `caret_moves_to_first_non_blank`,
  `gg_moves_to_doc_start`, `capital_g_moves_to_doc_end`,
  `f_jumps_to_next_occurrence_of_char`,
  `t_jumps_to_just_before_next_occurrence`, `capital_f_jumps_back`,
  `semicolon_repeats_last_find`, `comma_repeats_find_in_reverse`.
- **Edits (10 tests)**: `x_deletes_char_under_cursor`,
  `capital_x_deletes_char_before_cursor`, `dd_deletes_line_linewise`,
  `dw_deletes_to_next_word_charwise`, `capital_d_deletes_to_line_end`,
  `yy_yanks_line_linewise`, `yw_yanks_word_charwise`,
  `p_after_linewise_yank_opens_new_line_below`,
  `capital_p_after_linewise_yank_opens_new_line_above`,
  `join_lines_replaces_newline_with_space`,
  `join_lines_collapses_leading_whitespace`, `u_undoes_last_edit`.
- **Visual mode (6 tests)**: `v_enters_visual_with_anchor_at_cursor`,
  `visual_motions_extend_selection`,
  `visual_d_cuts_selection_and_returns_to_normal`,
  `visual_y_copies_selection_and_returns_to_normal`,
  `visual_o_swaps_anchor_and_active`,
  `capital_v_enters_visual_line_mode`,
  `visual_line_d_deletes_full_lines`.
- **Markdown-aware reuse (2 tests)**:
  `o_in_a_bullet_list_continues_the_list`,
  `dd_inside_numbered_list_renumbers_remaining_items`.
- **Reveal-delay default (1 test)**:
  `suppress_raw_reveal_unaffected_by_handler_alone` — confirms the
  flag stays `false` until the App syncs it.
- **Action enum round-trip (1 test)**: `new_action_variants_roundtrip`.

Plus `motion.rs::tests` (8 unit tests on motion targets) and
`region_edit.rs::tests` (8 unit tests on the compound-op helpers).

**Not covered by automated tests** — would require a TTY:
- Status bar visually showing `NORMAL` / `INSERT` / `VISUAL`.
- The `suppress_raw_reveal` flag actually preventing the 120ms flicker
  in Rendered mode.
- Hint-line modal-pending display (which isn't wired anyway — see §3.11).

### 3.13 Checkpoint 1 acceptance — passed

- [x] `cargo test` passes (1582 tests, 0 failed, 5 ignored).
- [x] No new clippy errors over baseline (49 errors with
      `--all-targets`, all pre-existing in unrelated files).
- [x] `cargo fmt -- --check` clean (after the in-flight `cargo fmt`
      pass).
- [ ] **Manual smoke test pending**: this checkpoint was developed in an
      agent context without a TTY.  The user should run
      `cargo run --release -- some.md` after setting
      `handler = "vim"` in `~/.config/edamame/config.toml`, navigate
      with `hjkl`, edit a list with `o` / `dd`, and confirm the
      status bar reads `NORMAL` / `INSERT` / `VISUAL` / `V-LINE`.
      The test suite covers the data path; the smoke test verifies
      the rendered output and per-keystroke responsiveness in a real
      terminal.
- [x] Ctrl-* keybindings honoured: rebinding e.g. `ctrl+s` to
      `Save` in `keybindings.toml` still fires from Normal mode
      (handled by the keymap-first Ctrl-* dispatch).
- [x] Bare-key vim bindings are NOT user-configurable in Checkpoint 1 —
      hardcoded in `handle_normal` / `handle_visual`.  Deferred to a
      future enhancement after Checkpoint 4.

### 3.14 Readiness for Checkpoint 2

Foundation in place:

- `ModalHandler` trait shape covers Checkpoint 2's needs (operator
  composition uses the same `Pending` / `Consumed(actions)` arms; no
  trait extension required).
- `Motion` enum + `MotionTarget` trait already supply every motion
  Checkpoint 2's text objects compose with.
- `region_edit::{delete_region, yank_region, replace_char_at, toggle_case_at}`
  cover checkpoint 2's `r{c}`, `~`, `cw` / `ciw` (once `change_region` is
  added — see §3.6).
- `VimState::pending` already supports `PendingOp::Operator(_)`,
  `PendingOp::FindChar`, `PendingOp::GPrefix` — Checkpoint 2 just adds
  `PendingOp::ReplaceChar` and the operator variants `Change`,
  `Indent`, `Dedent`, `ToggleCase`, `Uppercase`, `Lowercase`.
- The Checkpoint 2 `Action` variants for case ops
  (`ToggleCaseAtCursor`, `LowercaseSelection`, `UppercaseSelection`)
  haven't landed; they should land in Checkpoint 2 alongside the visual
  `~` / `u` / `U` keybindings.

Open question for Checkpoint 2: dot-repeat (`.`) needs a `last_change`
field on `VimState` that captures the operator + motion + count +
inserted text from the most recent change.  The plan §4 lists this;
the implementation will need to decide between (a) recording per-op
in the resolver (complex) or (b) snapshotting the buffer before each
change and computing the delta on `.` (simple but loses the
operator+motion semantics of vim's actual `.` — replays buffer state
instead of the operation itself).  Recommendation: defer the choice
until Checkpoint 2 actually starts and pick based on how clean (a) turns
out to be in practice.

---

## 4. Checkpoint 2 — Operator+motion composition, text objects, dot-repeat

### 4.1 Goal

Add the operator-pending state machine so `dw`, `cw`, `ciw`, `da"`,
`y3w`, `>}` etc. work. Add dot-repeat. Add `r{c}`, `~`, `gu` / `gU`,
`>>`, `<<`.

(Counts and search are still Checkpoint 3.)

### 4.2 Tasks

- [ ] `src/input/modal/vim/text_object.rs`:
  - `TextObject` enum variants
  - `text_object_range(obj, cursor, buf) -> Option<Range<usize>>`
  - Pure buffer-walk; uses `Buffer::rope()`.
- [ ] In `vim/normal.rs`, implement the `PendingOp::Operator(_)` arm:
  - On second key, dispatch to either:
    - Motion → compute target via `motion::Motion::target`, build
      `(start, end)` range, call `region_edit::delete_region` /
      `yank_region` / `change_region`.
    - Text object → call `text_object_range`, then ditto.
    - Same-key (`dd`, `yy`, `cc`, `>>`, `<<`) → linewise op on
      current line.
- [ ] Add `Action::ToggleCaseAtCursor`, `Action::LowercaseSelection`,
      `Action::UppercaseSelection` to keymap.rs.
- [ ] Implement them in `edit_ops::apply` (using
      `region_edit::toggle_case_region` etc.).
- [ ] In `vim/mod.rs`, implement `r{c}` via `PendingOp::ReplaceChar` +
      `region_edit::replace_char_at`.
- [ ] In `vim/mod.rs`, implement `~` (Normal) via
      `region_edit::toggle_case_at` then `MoveRight`.
- [ ] Dot-repeat: capture the last operator+motion+inserted-text combo
      into `VimState::last_change` on every "completed change". On `.`,
      replay the captured combo. Insert-mode capture: track everything
      typed between entering Insert and `Esc` back to Normal.

### 4.3 Checkpoint 2 tests (`tests/vim_operators.rs`)

- `ciw` changes inner word.
- `da"` deletes around quotes.
- `>>` indents (in list and outside).
- `<<` dedents.
- `~` toggles case.
- `gu{motion}` lowercases.
- `r{c}` replaces a single char.
- Dot repeat: `dw .` deletes the word, then deletes the next word.
- Dot repeat with insert: `cw foo Esc .` — repeats the change.

---

## 5. Checkpoint 3 — Counts, search, marks

### 5.1 Goal

Add count prefix accumulation, `/` `?` `n` `N` `*` `#` search, and
`m{c}` / `` ` ``{c} / `'`{c} marks.

### 5.2 Counts

- [ ] In `vim/normal.rs`, accumulate digits 1-9 (and 0 if a count is
      already started — bare `0` is `MoveLineStart`) into
      `VimState::pending_count`.
- [ ] Update `pending_hint` to render the count + pending op:
      `"3"`, `"3d"`, `"3di"`, `"3di("`.
- [ ] Update operator+motion dispatch to apply count = 1 default.
      Operator-without-count + `count`-prefixed motion = N-times the
      motion. Both `[count][op][motion]` and `[op][count][motion]`
      shapes resolve to the same range.
- [ ] Reset `pending_count` after every completed command.

### 5.3 Search engine (`editor/search.rs`)

- [ ] Pure function: `find_all(buf: &Buffer, pattern: &str) -> Vec<Range<usize>>`.
  - Checkpoint 3 uses `regex::Regex` (literal escape if user types plain text,
    raw regex if they prefix with `\v` or similar — pick one and document).
  - Return char-offset ranges.
- [ ] `find_next(buf, from, pattern, forward) -> Option<usize>` to jump
      to the next/previous match.
- [ ] `find_all_for_word_under_cursor(buf, cursor) -> Vec<Range<usize>>`
      for `*` / `#`.

### 5.4 Search UI

- [ ] Reuse the Phase 9 hint-line modal-prompt channel. The handler
      returns `HandleResult::Overlay(OverlayRequest::SearchForward)` on
      `/`. The App opens the prompt, captures the pattern, on submit
      calls back into the handler with `vim.complete_search(pattern)`.
- [ ] `vim.complete_search(pat)` populates
      `state.search_pattern`/`search_matches` via `editor::search::find_all`.
- [ ] `n` / `N` advance the cursor to the next / previous match offset.
- [ ] `*` / `#` set `search_pattern` to the word under cursor (with
      `\b` word-boundary anchors), then act like `n` / `N`.

### 5.5 Match highlighting

- [ ] Add `Theme::search_highlight: Style`. Default: a yellow
      background. Document in `themes/default.toml`.
- [ ] In `ui/line_render.rs`, after constructing the line spans, walk
      `state.search_matches` and apply the highlight style to any span
      that overlaps a match range. The "active" match (cursor's current
      `n`-target) gets a more emphasised style — add
      `Theme::search_highlight_active`.
- [ ] Clear `search_matches` on `ExitToPreview` and on empty pattern.

### 5.6 Marks

- [ ] `VimState::marks: HashMap<char, usize>` (char offset).
- [ ] On `m{c}` (PendingOp::SetMark resolution), store
      `state.cursor.offset` under the char.
- [ ] On `` ` ``{c}, jump cursor to the stored offset (if absent, no-op
      with a sticky error "no mark `{c}`").
- [ ] On `'`{c}, jump cursor to the start of the line containing the
      stored offset.
- [ ] **Known limitation**: marks are not adjusted on buffer edit. If
      the user inserts/deletes text before a mark, the mark drifts.
      Document this in the user docs. A future checkpoint can add a
      `ModalHandler::on_edit(&EditDelta)` callback for mark adjustment.

### 5.7 Checkpoint 3 tests

- Count: `3j`, `5dw`, `2dd`, `3>>`.
- Search forward/back, `n`/`N`, `*`/`#`.
- Match highlighting (snapshot test in `tests/snapshots/`).
- Mark set/jump: `ma`, `` `a ``, `'a`.
- Mark drift after edit (assert known limitation).

---

## 6. Checkpoint 4 — Ex commands

### 6.1 Goal

`:` opens a command line. Support `:w`, `:q`, `:wq`, `:e <path>`,
`:s/pattern/replacement/flags`.

### 6.2 Tasks

- [ ] `src/input/modal/vim/ex.rs`:
  - `parse_ex(input: &str) -> Result<ExCommand, ExError>` using a
    small hand-written parser (no `nom` — `:s/.../.../...` is simple
    enough for `str::split` with backslash escapes).
  - `ExCommand` enum: `Write`, `Quit`, `WriteQuit`, `Edit(PathBuf)`,
    `Substitute { pattern: String, replacement: String, global: bool, ignore_case: bool }`.
- [ ] Handler: `:` returns `HandleResult::Overlay(OverlayRequest::ExCommand)`.
- [ ] App: route ex prompt completion through `vim.complete_ex(input)`,
      which:
  - For `Write` / `Quit` / `WriteQuit`: emits
    `HandleResult::Consumed(vec![Action::Save, Action::Quit])`.
  - For `Edit(path)`: emits `Action::Open` with the path threaded
    through (may need a new `Action::OpenPath(PathBuf)` variant or
    a hook on `App`).
  - For `Substitute`: calls a new
    `region_edit::substitute(state, range, pattern, replacement, flags)`.
    Default range = whole buffer. Future enhancement: `:n,m s/...` for
    line ranges.

### 6.3 Checkpoint 4 tests

- `:w` saves.
- `:q` quits (with dirty-buffer guard via existing `App::open_quit_confirm`).
- `:wq` saves then quits.
- `:s/foo/bar/` replaces first match on the cursor's line (vim default).
- `:s/foo/bar/g` replaces all on the line.
- `:%s/foo/bar/g` replaces all in buffer (if `%` range is supported in
  Checkpoint 4, otherwise punt).

---

## 7. Risks and mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| `Box<dyn ModalHandler>` lifetime vs `KeyMap` borrow | Certain | Med | Owned `KeyMap` in handlers via `Clone`. Checkpoint 1 §3.2. |
| `RAW_REVEAL_DELAY` flicker in Normal | Likely | Med | `suppress_raw_reveal` flag synced after every `handle()`. Checkpoint 1 §3.3. |
| `Box<dyn ModalHandler>` impl is one big match — hard to test | Likely | Low | Split per-mode dispatch into separate methods (`handle_normal`, etc.) — each unit-testable. |
| Markdown-aware ops conflicts (e.g. `dd` on the GFM separator row) | Possible | Med | Reuse existing `list_edit` / `table_edit` paths via `Action::DeleteLine`. Test in tests/vim_basics.rs. |
| Dot-repeat capturing inserted-text correctly | Likely | Med | Capture in a per-Insert-mode buffer; flush on Esc → Normal. Take a before/after delta as fallback. |
| Search regex perf on large docs | Unlikely | Low | `regex` crate is fast enough for human-sized Markdown. Profile only if it's a real problem. |
| Match highlighting interacting with cursor-block raw reveal | Possible | Low | The cursor block is rendered raw — the highlight overlay can paint over both. Test rendering with insta. |
| Mark drift on edit | Certain | Low | Documented limitation. Future checkpoint adds `on_edit` callback. |
| Ex parser edge cases (escaped `/` in `:s`) | Possible | Low | Cover with unit tests in `ex.rs`. Strict subset for V1. |
| Ctrl-D / Ctrl-U keybinding override surprises users with rebound chords | Possible | Low | Document in `config.toml` that vim mode claims a small set of Ctrl chords. |

---

## 8. Open questions parked for later

These do NOT block any checkpoint; revisit when each becomes relevant.

- **`relative_line_numbers`** (vim's `set rnu`): would require row-number
  rendering in `RawView` / `RenderedView`. Not in V1.
- **`gj`/`gk` (visual-line-up/down)**: vim has both logical and
  visual-line motions. edamame already does visual-line nav by default
  (`visual_line_nav: true`); `j`/`k` already match. Add `gj`/`gk` only
  if a user complains.
- **`]p` / `[p` (paste with indent match)**: deferred.
- **`@/` (re-execute last search) and `@:` (re-execute last ex)**:
  deferred until macros land.
- **Selection multi-cursor**: not a vim feature; out of scope.
- **`w` / `W` distinction**: vim distinguishes `word` (alphanumeric +
  `_`) from `WORD` (non-whitespace). Checkpoint 1 implements only `word`
  motion. Checkpoint 2 adds `W`/`E`/`B` if time allows.
- **Whether VisualLine should clear `kill_ring_linewise = false` on
  yank-from-charwise-visual**: design subtlety. The principled answer
  is "yes, charwise yank produces charwise paste". Cover in tests.

---

## 9. Glossary / decisions log

- **Checkpoint**: a self-contained vertical of work that's mergeable on its
  own. Checkpoints 1-4 stack but don't depend on each other for correctness
  beyond their stated prerequisites.
- **Handler / `ModalHandler`**: the trait-object-based input layer that
  translates key events into either Actions or direct EditorState
  mutations. Vim is one handler; Default is another.
- **Vim sub-mode**: Normal / Insert / Visual / VisualLine. Lives on
  `VimHandler`. Distinct from `EditorState::mode` (Preview / Rendered /
  Raw), which is the rendering axis.
- **Operator-pending**: vim's "I've seen `d` but no motion yet" state.
  Modelled as `VimState::pending_op = Some(PendingOp::Operator(_))`.
- **Linewise / charwise**: kill-ring content carries a "linewise"
  flavor. `dd` / `yy` are linewise; `dw` / `yw` are charwise. Affects
  paste behavior (`p` / `P`).
- **Dot-repeat**: vim's `.`. Replays the last "change" — operator +
  motion + count + inserted text. Captured into `VimState::last_change`.

---

## 10. Implementation checklist (top-level)

- [ ] Checkpoint 1: trait extension + scaffolding (2026-04-26)
- [ ] Checkpoint 1: `motion.rs`, `region_edit.rs` (2026-04-26)
- [ ] Checkpoint 1: `VimHandler` Normal/Insert/Visual (2026-04-26)
- [ ] Checkpoint 1: status bar wiring (hint-line slot deferred; see §3.11)
- [ ] Checkpoint 1: `tests/vim_basics.rs` — 45 tests (2026-04-27)
- [ ] Checkpoint 1: manual smoke test (TTY required) + MERGE
- [ ] Checkpoint 2: text objects, operator-pending, dot-repeat
- [ ] Checkpoint 2: case + replace + indent ops
- [ ] Checkpoint 2: `tests/vim_operators.rs`
- [ ] Checkpoint 2: smoke test, MERGE
- [ ] Checkpoint 3: counts
- [ ] Checkpoint 3: `editor/search.rs`, search UI, match highlighting
- [ ] Checkpoint 3: marks
- [ ] Checkpoint 3: `tests/vim_count_search_marks.rs`
- [ ] Checkpoint 3: smoke test, MERGE
- [ ] Checkpoint 4: `vim/ex.rs` parser
- [ ] Checkpoint 4: ex command UI (reuses prompt channel)
- [ ] Checkpoint 4: `:w` / `:q` / `:wq` / `:e` / `:s` execution
- [ ] Checkpoint 4: `tests/vim_ex.rs`
- [ ] Checkpoint 4: smoke test, docs, MERGE

---

End of plan.
