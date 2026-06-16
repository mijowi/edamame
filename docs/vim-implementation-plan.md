# Vim Mode — Implementation Plan

Implementation reference for adding Vim-style modal editing to edamame. Companion to [`vim-feature-prompt.md`](vim-feature-prompt.md) (the requirements/scope). This document records the agreed architecture, the locked design decisions, the module layout, and the checkpoint roadmap.

Status: **planned** — implementation begins at Checkpoint 1.

---

## 1. Locked decisions

These were settled during planning and are not to be re-litigated without an explicit change request.

| Decision | Choice | Rationale |
|---|---|---|
| Config gate | `config.modal.handler = "vim"` (field already exists on `ModalConfig`, defaults to `"default"`) | Default users unaffected; every checkpoint is dead code until the switch is set. |
| Module layout | **Two-layer split**, mirroring the existing mouse dispatch | `src/input/vim/` = state machine (like `MouseDispatcher`); `src/editor/vim_ops/` = apply/resolution (like `mouse_ops::apply`). CLAUDE.md mandates following the mouse pattern. |
| State location | `VimState` on `App` as `Option<VimState>` | Survives across keystrokes; read by the UI; mirrors `App::drag_anchor` / `EditorState::search`. `None` unless vim is enabled. |
| Input pipeline | Dedicated `vim_feed` reducer, **not** the `ModeHandler` trait | The trait is single-key→`Action` with `&EditorState` (immutable). Vim needs `&mut EditorState` + multi-key pending state. |
| Action enum | **Not** polluted with vim-internal ops | `o`/`J`/`r`/`>>` etc. mutate `EditorState` directly via `vim_ops`. The `Action` enum stays the user-rebindable / palette / keybinds surface. Reuse existing actions only where a perfect, already-keybindable fit (`Undo`, `Redo`, `Save`). |
| Rendering axis | `EditorState::mode` stays `Mode::Rendered` for Normal/Insert/Visual (Raw still toggleable) | Normal renders like today's Rendered view (cursor visible, current line raw). Avoids an "effective view mode" indirection at every render site. |
| Dot-repeat (`.`) | **Deferred** | Design the operator layer so a "last change" recorder can be bolted on later, but don't build it now. |
| Search semantics | Literal substring + **smartcase** | `/`,`?`,`*`,`#`,`n`,`N` reuse the existing `SearchState`. Smartcase (case-insensitive unless the pattern contains an uppercase letter) is added to the **base search feature**, not vim-only — so every edamame user gets it. No regex in search. |
| `:s` substitution | Real **regex** via the `regex` crate | Separate single-shot path; does **not** reuse `SearchState`. Regex lives only here, never in `/` search. New unconditional dependency. |
| Yank/paste | Vim-internal unnamed register with a **charwise/linewise flag** | `dd`/`yy`/visual-line fill linewise; `p`/`P` open a new line for linewise content. Deletes/yanks do **not** touch the OS clipboard. `Ctrl-C`/`Ctrl-V` stay on the system clipboard. (Tradeoff vs. reusing the kill-ring — see note below.) |
| State machine source | **Roll our own** | `tui-textarea`'s buffer model is incompatible with edamame's rope + `ParsedDoc` + source-map. Borrow the structural pattern only. |

**Smartcase on the base feature.** The smartcase rule (lowercase pattern → case-insensitive; any uppercase → case-sensitive) is implemented inside `SearchState`/`search` itself, so `/` in vim mode and the normal `Ctrl-F`-style search share one code path. There is deliberately **no regex** in `/` search — the price of reusing `SearchState` (which is literal-substring) is that vim's `/` is literal, not a regex. Regex is confined to `:s`/`:%s`.

**Register choice — separate vim register, not the kill-ring.** Reusing the existing kill-ring with an added `linewise` flag was considered and rejected. A separate `VimRegister` is more vim-accurate (a `dd`/`yy` never clobbers what the user `Ctrl-C`'d, and vice versa), keeps the `linewise` flag coupled to its text so the two can't desync, and avoids entangling vim state with the feature-gated `arboard`/Wayland clipboard paths (which can `Err`). The only thing reuse would buy — `yy` then `Ctrl-V` sharing one buffer — is precisely the behavior vim users do *not* expect. `p`/`P` and `Ctrl-V` being separate buffers is correct, not a bug.

### Scope (from the requirements doc)

- **Modes:** Normal, Insert, Visual (charwise), Visual Line. Switch keys: `Esc i I a A o O v V`.
- **Motions:** `h j k l w e b W E B 0 $ ^ gg G f{c} F{c} t{c} T{c} ; , % { } n N`.
- **Normal editing:** `x X dd D dw de db cc C cw ce cb yy Y yw ye yb p P >> << J u Ctrl-R r{c} ~` (dot `.` deferred).
- **Text objects:** `iw aw iW aW i" a" i' a' i\` a\` i( a( i) a) i[ a[ i] a] i{ a{ i} a}`.
- **Visual operators:** `d/x y c/s p r{c} ~/u/U > < J o` (swap ends), text-object select, `v`/`V` toggle.
- **Counts:** `3j 5dw 2dd 3>>` — both `[count][op][motion]` and `[op][count][motion]`.
- **Search:** `/pattern ?pattern n N * #`, match highlighting + n/N counter (reuse search infra), smartcase.
- **Ex commands:** `:w :q :wq :s/pat/rep/flags` (g, i), `:%s`.

**Out of scope:** `:e <path>` (open file — deferred), marks (`m{c}` / `` `{c} `` / `'{c}`), named registers, macros, block-wise Visual (`Ctrl-V`), the rest of Ex, window splits.

---

## 2. Architecture

### 2.1 Two-layer split (mirrors mouse dispatch)

```
src/input/vim.rs           facade — re-exports VimState, VimSubMode, VimOutcome, vim_feed
src/input/vim/
  state.rs                 VimState, VimSubMode, PendingOp, FindKind, VimRegister,
                           CmdLineState, CmdLineKind
  feed.rs                  vim_feed() — the reducer: count/operator/pending parsing,
                           key → resolved intent; mode transitions; Ctrl-* passthrough
  cmdline.rs               `:` / `/` / `?` command-line buffer editing (insert, backspace,
                           cursor, submit/cancel)

src/editor/vim_ops.rs      facade — re-exports apply entry points + Motion/TextObject
src/editor/vim_ops/
  motion.rs                Motion, resolve_motion() (offset), resolve_motion_range() (range+linewise)
  text_object.rs           TextObject, resolve_text_object_range() — balanced-pair / word scan
  operator.rs              execute_operator() — delete/change/yank/indent → EditDelta(s), fill register
  edits.rs                 single-key edits: x/X, r{c}, ~, J, p/P, o/O (list-aware)
  search.rs                vim search entry: build query (incl. * / # word-under-cursor)
                           then hand to SearchState — smartcase lives in the base feature, not here
  ex.rs                    parse_ex() + execute: :w :q :wq :s :%s (regex); :e deferred
```

**Layering note:** `src/input/` (layer 4) sits above `src/editor/` (layer 5). `vim_feed` (input) decides *what* the user asked for; `vim_ops` (editor) resolves offsets against the buffer and mutates `EditorState`. This is exactly the `MouseDispatcher` → `mouse_ops::apply` split that CLAUDE.md tells us to keep strict.

### 2.2 State

`VimState` lives on `App` (`src/app.rs`):

```rust
pub vim: Option<VimState>,   // Some iff config.modal.handler == "vim"
```

```rust
/// Vim sub-mode — orthogonal to EditorState::mode (the rendering axis).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VimSubMode {
    #[default] Normal,
    OperatorPending,   // d/c/y/>/< entered, awaiting motion or text object
    Insert,
    Visual,            // charwise
    VisualLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingOp { Delete, Change, Yank, IndentRight, IndentLeft }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindKind { Forward, Backward, ForwardTill, BackwardTill }  // f F t T

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VimRegister {
    pub text: String,
    pub linewise: bool,   // dd/yy/V-mode → true; p/P open a new line
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdLineKind { Ex, SearchForward, SearchBackward }  // ':' '/' '?'

#[derive(Debug, Clone)]
pub struct CmdLineState {
    pub kind: CmdLineKind,
    pub input: String,
    pub cursor: usize,    // char index within input
}

#[derive(Debug, Clone, Default)]
pub struct VimState {
    pub sub_mode: VimSubMode,
    pub count: Option<u32>,            // leading count (3 in 3dw); capped (e.g. 9999)
    pub pending_op: Option<PendingOp>,
    pub motion_count: Option<u32>,     // count between op and motion (d2w)
    pub pending_g: bool,               // first 'g' of 'gg'
    pub pending_text_object: Option<bool>, // Some(true)=inner (i), Some(false)=around (a)
    pub last_find: Option<(FindKind, char)>, // for ; and ,
    pub visual_anchor: Option<usize>,  // char offset; Some in Visual/VisualLine
    pub register: VimRegister,
    pub cmdline: Option<CmdLineState>, // active while typing : / ?
}
```

**Sub-mode ↔ rendering invariants:**

| `VimSubMode` | `EditorState::mode` | Rendered behavior |
|---|---|---|
| Normal / OperatorPending | `Rendered` or `Raw` | Cursor visible; in `Rendered` the current line reveals raw via the existing `RAW_REVEAL_DELAY` (today's behavior). |
| Insert | `Rendered` or `Raw` | Standard editing cursor; current line raw. |
| Visual / VisualLine | `Rendered` or `Raw` | Selection painted; reveal inherited. |
| (any) while `Mode::Diff` | `Diff` | Vim suspended — diff owns the keymap. |

- Entering Normal from Preview forces `editor.mode = Rendered` first (Normal never coexists with `Preview`).
- `Esc` from Insert → Normal **and** move cursor one grapheme left (vim convention).
- When vim is active, `Action::ExitToPreview` reroutes to vim-Normal instead of `Mode::Preview` — handled in `App::dispatch_action`, keeping `EditorState` ignorant of vim.
- **Preview is unreachable while vim is active; `Raw` is fully supported.** Vim-Normal replaces `Mode::Preview`'s use case as the non-editing resting mode, so no key lands in `Preview` when `vim.is_some()` (the `ExitToPreview` reroute above is the single enforcement point). Every vim sub-mode, however, works in **both** `Rendered` and `Raw` — a user can toggle the whole document to raw markdown and navigate/edit there, exactly as in non-vim mode. Only `Preview` is excluded.
- **Raw reveal is NOT suppressed — deliberately.** Vim motions (`w` `e` `b` `f{c}` `x` `r` …) traverse the *raw* buffer, including hidden Markdown syntax (`**`, `[`, `](url)`). If the cursor's line were left rendered, the cursor would jump across characters that aren't on screen, so its visible position wouldn't correspond to anything the user can see — far more disorienting than vertical render-churn, and it defeats precise modal navigation. So Normal/Visual inherit today's Rendered behavior unchanged: the cursor's current line reveals its raw source (the existing `RAW_REVEAL_DELAY` + jitter-suppression already makes line-to-line navigation smooth, so there is no flicker to fix). Net effect: **vim adds no reveal-related code at all.** (This reverses the old plan's `suppress_raw_reveal` flag, which optimized for a clean rendered look at the cost of motion legibility — the wrong trade for a buffer-byte-oriented editor.)

### 2.3 Input pipeline

Reducer entry point (in `src/input/vim/feed.rs`):

```rust
pub fn vim_feed(
    vim: &mut VimState,
    editor: &mut EditorState,
    key: KeyEvent,
    keymap: &KeyMap,
    viewport_height: usize,
    viewport_width: usize,
) -> VimOutcome;

pub enum VimOutcome {
    Pending,      // multi-key sequence still accumulating — keep count/pending
    Consumed,     // key fully handled (mutation applied or no-op)
    Passthrough,  // not a vim key (e.g. Ctrl-* chord) — fall through to DefaultHandler
    // ── App-level effects the reducer can't perform itself (it has &mut EditorState, not &mut App).
    //    Small closed set; everything else (:s/:%s, n/N, all operators) stays in EditorState/vim_ops.
    Save,                                          // :w
    Quit { save_first: bool },                     // :q (false) / :wq (true). NOT Ctrl-Q (a chord → passthrough).
    EnterSearch { forward: bool, query: String },  // / ? (query from cmdline) and * # (word under cursor)
}
```

Save and Quit are dispatched as the existing `Action::Save`/`Action::Quit`, so the dirty-buffer guard, flash, and autosave bookkeeping come for free (consistent with §1's "reuse existing actions for a perfect fit"). `Quit { save_first: true }` models `:wq` as a single outcome. The set is kept small by deferring `:e` (out of scope) and by leaving `:s`/`:%s` and `n`/`N` inside the editor layer — none of those need `&mut App`.

Integration in `event_loop::dispatch_single_key` (`src/app/event_loop.rs:1122`):

```rust
fn dispatch_single_key(&mut self, event: Event, keymap: &KeyMap, dims: &DocDims) {
    if let (Some(vim), Event::Key(key)) = (self.vim.as_mut(), &event) {
        if self.editor.mode != Mode::Diff {
            match vim_feed(vim, &mut self.editor, *key, keymap, dims.doc_height, dims.doc_width) {
                VimOutcome::Pending | VimOutcome::Consumed => { self.needs_draw = true; return; }
                VimOutcome::Save => { self.dispatch_action(Action::Save, dims.doc_height, dims.doc_width); return; }
                VimOutcome::Quit { save_first } => {
                    if save_first { self.dispatch_action(Action::Save, dims.doc_height, dims.doc_width); }
                    self.dispatch_action(Action::Quit, dims.doc_height, dims.doc_width); // dirty guard (clean after save)
                    return;
                }
                VimOutcome::EnterSearch { forward, query } => {
                    self.enter_search_flow(forward, query, dims.doc_height, dims.doc_width);
                    self.needs_draw = true; return;
                }
                VimOutcome::Passthrough => {} // fall through
            }
        }
    }
    // existing DefaultHandler path (Ctrl-* chords, search/diff flow keys) unchanged
    let mut handler = DefaultHandler::new(keymap);
    let Some(action) = handler.handle_event(event, &self.editor) else { return; };
    self.dispatch_action(action, dims.doc_height, dims.doc_width);
    self.needs_draw = true;
}
```

**Coalesce-burst guard** (`dispatch_key_batch`, near `event_loop.rs:1041`): skip `InsertChar`/delete coalescing when `vim.is_some() && sub_mode != Insert`, alongside the existing `search.is_some()` guard. Otherwise `333` would coalesce instead of accumulating `count = 3`.

**Insert-mode typing path is implicit — by design.** `vim_feed` does *not* implement character insertion. In Insert sub-mode it handles only the keys it owns (`Esc` → Normal+left, and the table-aware `Tab`/`Shift+Tab`/`Enter` passthrough noted in §2.5) and returns `VimOutcome::Passthrough` for every printable char, `Backspace`, etc. Those fall through to the unchanged `DefaultHandler` path below, which already does list-aware Enter, indent Tab, and `InsertChar` (and gets normal burst-coalescing, since the guard above only fires when `sub_mode != Insert`). The consequence: Insert mode reuses the *entire* existing editing pipeline verbatim — there is no vim-specific insertion code, and any future editing feature works in vim Insert automatically.

**Sequence parsing examples:**

- `3dw` → `'3'` sets `count=3` (Pending); `'d'` sets `pending_op=Delete`, `sub_mode=OperatorPending` (Pending); `'w'` resolves `Motion::WordForward`, effective count `3*1`, `execute_operator(Delete, range)`, reset (Consumed).
- `d2w` → `'d'` (op); `'2'` sets `motion_count=2`; `'w'` count `1*2`.
- `ciw` → `'c'` (op); `'i'` sets `pending_text_object=Some(true)`; `'w'` → `TextObject::InnerWord` → change.
- `gg` → `'g'` sets `pending_g=true` (Pending); `'g'` → `Motion::DocStart`.
- `f x` → `'f'` sets a pending-find awaiting one char; `'x'` → `Motion::FindChar('x', Forward)`, recorded in `last_find`.
- **`0` is count-or-motion, decided by context.** When no count has been accumulated yet (`count == None` and `motion_count == None`), `'0'` is `Motion::LineStart`. When a count is already in progress, `'0'` is the digit zero and appends to the active count register (`30j` → count 30; `10dd` → 10 lines). Rule: a leading `0` is the motion; a `0` *after* any of `1`–`9` is a digit. Applies to both the leading `count` and the operator-pending `motion_count`.

### 2.4 Motion / operator resolution

```rust
// src/editor/vim_ops/motion.rs
pub enum Motion {
    Left, Right, Up, Down,
    WordForward, WordEnd, WordBackward,        // w e b
    BigWordForward, BigWordEnd, BigWordBackward, // W E B
    LineStart, LineFirstNonBlank, LineEnd,     // 0 ^ $
    DocStart, DocEnd, LineN(u32),              // gg G NG
    FindChar(char, FindKind), RepeatFind, RepeatFindRev, // f F t T ; ,
    ParagraphForward, ParagraphBackward,       // } {
    SearchNext, SearchPrev,                    // n N
    MatchingPair,                              // %
}

pub enum MotionOrObject { Motion(Motion), TextObject(TextObject) }

pub fn resolve_motion(/* motion, count, cursor, buf, vim, viewport */) -> usize;          // target offset
pub fn resolve_motion_range(/* MotionOrObject, count, cursor, buf, vim */)
    -> (std::ops::Range<usize>, bool /* linewise */);
```

**Offset convention:** every offset and `Range<usize>` in `vim_ops` is a rope **char** offset — the same space as `Cursor::offset`, `Selection`, and `visual_anchor` (and what `EditDelta`/`apply_delta` expect). Vim introduces **no** byte offsets. Conversion to byte ranges for highlight painting is already the existing selection path's job (`Selection` is char-based; `paint_byte_range_overlay` converts at the boundary), so vim selections ride that path unchanged and never touch `SourceMap`'s byte index space directly.

`execute_operator(op, range, linewise, vim, editor, ...)` (in `operator.rs`):
1. Extract text from `range`.
2. **Yank:** store in `vim.register` (with `linewise`), move cursor to `range.start`, no mutation.
3. **Delete:** store register, apply **one** `EditDelta { offset: range.start, removed, inserted: "" }` via `editor.apply_delta` (single undo unit), renumber list if linewise + in a list.
4. **Change:** delete, then `sub_mode = Insert`.
5. **IndentRight/Left:** rebuild affected lines, one combined `EditDelta`; list items go through `list_edit::indent_item`/`outdent_item`.

> **Critical:** an operator must resolve the full range and issue a **single** `apply_delta`, never N sequential char deletes (which `History`'s word-group merge would mishandle). This keeps `3dw` = one `u`.

### 2.5 Markdown-aware op reuse

- `o`/`O`: if cursor is in a list (`list_edit::find_list_at`), use `list_edit::continue_item` (auto-renumber) then enter Insert; else plain newline + Insert.
- `dd` (linewise delete) in an ordered list: after the delete, `list_edit::renumber_list`.
- `>>`/`<<`: `list_edit::indent_item`/`outdent_item` in a list; else add/strip `tab_width` spaces.
- `J`: remove the line's trailing `\n` + next line's leading whitespace as one `EditDelta`.
- Insert-mode `Tab`/`Shift+Tab`/`Enter` inside a table → `Passthrough` to existing `table_edit_ops`.

### 2.6 UI integration

**Status badge** (`src/ui/status_bar.rs`): add `vim_mode_label: Option<&'a str>` to `StatusBarState`. `StatusBar::render` prefers it over `format!(" {} ", s.mode)`. Style mapping reuses existing slots (no new theme fields for now):

| Label | Style slot | Intent |
|---|---|---|
| `NORMAL` | `status_mode_preview` | muted / browse |
| `INSERT` | `status_mode_rendered` | primary / editing |
| `VISUAL` / `V-LINE` | `status_mode_raw` | warning / selection |

Populated in `EditorView::render` from `App`'s vim state (`vim.as_ref().map(|v| v.mode_label())`).

**Command line** (`src/ui/bottom_region.rs`): new `HintContent::CommandLine { prefix: char, text: String, cursor: usize /* char index */ }`. Priority in `App::hint_content`: `Prompt > CommandLine > Transient > hovered-link > Chords`. Render `prefix` + text with the cursor cell styled `theme.cursor_rendered`. Enter/Esc clear `vim.cmdline`; the priority chain restores the chord row automatically.

**Normal-mode rendering:** no `EditorView` dispatch changes — mode stays `Rendered`, so it already routes to `RenderedView`. The single behavioral edit is the `ExitToPreview` reroute in `dispatch_action`.

**Visual painting:** Visual sets `editor.selection = Some(Selection { anchor: visual_anchor, active: cursor })` on each motion — existing `paint_byte_range_overlay` renders it for free. `Esc` clears `selection`.

**VisualLine — selection stays charwise; line-expansion happens in two distinct places.** The `selection` field always holds the raw charwise `(anchor, active)` pair, *even in VisualLine*. We never mutate `selection` to whole-line bounds. The line expansion is computed on demand at two separate sites, from the same rule (extend the byte range to the start of `anchor`'s line and the end of `active`'s line):

1. **Render side** — `RenderedView` + `RawView` gain a `visual_line_mode: bool`. When set, the overlay painter widens the charwise range to full lines purely for display, so the highlight covers whole rows.
2. **Operator side** — when an operator fires in VisualLine (`d`/`y`/`c`/`>`/…), `execute_operator` receives `linewise = true` and applies the *same* full-line widening to the range it actually edits (and yanks into the register with `linewise = true`).

Keeping `selection` charwise means a `v`↔`V` toggle is lossless (no information is destroyed by snapping), and the render and operator expansions can never disagree because they share one widening helper. Do **not** snap `selection` itself — that was the rejected shortcut; it loses the original charwise anchors on toggle and forces the painter and operator to re-derive bounds independently.

**Shared selection, private register.** Visual mode writes to the *shared* `EditorState::selection` — vim does **not** introduce a private selection buffer. Only the yank/paste *register* (§1) is vim-private. Consequence: `Ctrl-C`/`Ctrl-X` on a Visual selection copy/cut to the **system clipboard** via the existing `Copy`/`Cut` actions (they pass through `vim_feed` like any `Ctrl-*` chord and read `selection`), independent of the vim register that `y`/`d`/`p` use.

**What is copied always matches what is highlighted.** In charwise Visual that is the raw `selection` span, so `Ctrl-C` copies exactly it. In VisualLine the highlight is the line-expanded range, so `Ctrl-C`/`Ctrl-X` must copy/cut **that same expanded range**, not the charwise `selection` — otherwise the clipboard would get a ragged partial-line chunk while the screen shows whole lines. Implement with the *one shared* widening helper (`visual_line_range(selection, buffer) -> Range`) that the render and operator paths already use: the **App dispatch layer** applies it before the clipboard write whenever `vim.sub_mode == VisualLine`, so `EditorState`/`edit_ops` stay vim-agnostic and `selection` itself is never snapped. The rule generalizes — every consumer of a VisualLine selection (paint, operator, system copy/cut) goes through the same helper, so they can never disagree.

**Hint rows:** Normal/Insert/Visual show vim-specific chord rows (advertising Ctrl-bound app actions like `^P Menu`, `^S Save`, `/ Find`, `^Q Quit`) via a `vim_mode: Option<VimSubMode>` arg to `hint_line_for` — no new hard-bound key table needed.

### 2.7 Keybind conflicts

Audit of the default keymap (`config/keymap.rs::KeyMap::build`) against the vim surface:

- **Bare keys — no keybinding conflict.** The default keymap binds **no** bare letters; bare keys simply type (`InsertChar`). So vim claiming `h j k l w d f / :` etc. shadows only *typing*, which is the intended modal switch — it never overrides a configured binding. The one bare-key entry is `escape → ExitToPreview`, already handled by the reroute in §2.2.
- **`Ctrl-*` chords keep their edamame meaning.** `vim_feed` passes `Ctrl-*` through to the keymap, so `Ctrl-S` (Save), `Ctrl-P` (palette), `Ctrl-C`/`Ctrl-X`/`Ctrl-V` (system clipboard), `Ctrl-F` (search), `Ctrl-Z` (undo), `Ctrl-`` ` `` (toggle Raw), etc. all fire unchanged in every vim sub-mode.
- **`Ctrl-R` is the one real gap — fix in the keymap, not in vim.** Vim uses `Ctrl-R` for Redo, but the default keymap binds Redo to `Ctrl-Shift-Z` and leaves `Ctrl-R` unbound, so a bare passthrough would no-op. Resolution: **add `bind!("ctrl+r", Action::Redo)` to the default keymap** alongside `Ctrl-Shift-Z`. Vim Redo then works via plain passthrough with no vim-specific claim, and non-vim users gain a second Redo chord too. (Landed when `Ctrl-R` is wired, CP4.)
- **Vim scroll/edit `Ctrl` chords are deliberately out of scope.** `Ctrl-F`=OpenSearch (vim: page-down), `Ctrl-B`=BoldSelection (vim: page-up), `Ctrl-D`=DeleteLine (vim: half-page down), `Ctrl-U`=unbound (vim: half-page up), `Ctrl-E`=MoveLineEnd (vim: scroll line), `Ctrl-A`=SelectAll (vim: increment). These retain their edamame meanings; the matching vim motions are **not** implemented. This is a documented choice, not an oversight — revisit only on request.

---

## 3. Checkpoint roadmap

Every checkpoint **compiles cleanly, passes all existing + new tests, and is independently sanity-checkable**. Safety property: `config.modal.handler` defaults to `"default"`, so each checkpoint is inert for existing users and the existing test suite until the final polish.

Motions/operators are tested as **pure functions** of `(EditorState, VimState, key sequence) → resulting state`, the same way `mouse_ops::apply` is tested without a terminal. New tests go in `tests/vim.rs` (plus snapshot updates in `tests/ui.rs` at the end).

### CP1 — Walking skeleton  *(risk: HIGH)*
**Goal:** config switch wires `VimState` onto `App`; vim branch in `dispatch_single_key`; `h j k l` move; `i`/`a`/`I`/`A` enter Insert; `Esc` → Normal (cursor left); status badge shows `NORMAL`/`INSERT`.
**Sanity check:** open a file with vim on → badge `NORMAL`; `j` moves; `i` then typing inserts; `Esc` → `NORMAL`; bare `j` no longer types.
**Files:** create `src/input/vim.rs`(+`state.rs`,`feed.rs`); `src/editor/vim_ops.rs` stub; modify `src/app.rs` (field + init from config), `src/app/event_loop.rs` (dispatch branch + coalesce guard), `src/ui/status_bar.rs` + `src/ui/editor_view.rs` (label), `src/input.rs` facade.
**Tests:** handler-disabled-by-default; `hjkl` move; `i`→insert→`Esc`→Normal; bare char doesn't insert in Normal; digit accumulation clears on `Esc`. Existing `tests/ui.rs` snapshots unchanged.

### CP2 — Core motions & mode entries  *(risk: LOW)*  — needs CP1
`w e b W E B 0 ^ $ gg G`; entries `a A I o O`; `v V` enter Visual/VisualLine (no operators yet).
New `Cursor`/`vim_ops::motion` helpers: `WordEnd` (`e`) and word-class `w/b` (vim distinguishes punctuation from alphanumeric; existing `move_word_*` is `W/B` semantics).
**Sanity:** every motion lands correctly; `o` opens a line below in Insert.
**Tests:** `w`/`e`/`b` boundaries, `$`/`^`/`0`, `gg`/`G`, `o`/`O`, `v`/`V` enter.

### CP3 — Operator+motion reducer  *(risk: HIGH)*  — needs CP2
Counts (`3j`, `5l`, `d2w`, `2dd`); operators `d c y` × motions; `dd yy D C Y x X`; `p P` with the linewise register.
Build `resolve_motion_range` + `execute_operator`; the vim `VimRegister`.
**Sanity:** `3dw` deletes 3 words as one undo; `yy` then `p` duplicates the line below; `cw` deletes word + enters Insert.
**Tests:** `3dw` single undo; `dd`; `yy`/`p` linewise; `cw`; `D`/`C`/`Y`; `x`/`X`; count×op×motion both orderings.

### CP4 — Remaining Normal primitives  *(risk: LOW)*  — needs CP3
`r{c} ~ >> << J u Ctrl-R`. `u` maps to `Action::Undo`; `Ctrl-R` works via passthrough once `bind!("ctrl+r", Action::Redo)` is added to the default keymap (§2.7) — no vim-specific claim. The rest mutate via `vim_ops::edits`.
**Sanity:** `r` replaces a char; `~` toggles case; `>>` indents; `J` joins; `Ctrl-R` redoes.
**Tests:** each primitive; `>>`/`<<` round-trip; `Ctrl-R` redo via the new keymap binding (fires in both default and vim mode).

### CP5 — Find & pair motions  *(risk: LOW)*  — needs CP2
`f F t T` + `; ,` repeat; `{ }` paragraph; `%` matching pair. Entirely inside `vim_feed` + `motion.rs`.
**Sanity:** `f(` jumps; `;` repeats; `%` matches; `}` moves a paragraph.
**Tests:** `f`/`t` landing, `;`/`,` replay, `%` on each bracket type, `{`/`}`.

### CP6 — Visual & VisualLine operators  *(risk: MED)*  — needs CP3
Selection extends via motions; operators `d/x y c/s > < ~ J`; `o` swaps ends; `v`/`V` toggle/exit. The shared `visual_line_range` helper (used by the render-path `visual_line_mode` on `RenderedView`/`RawView`, the operators, and the system `Copy`/`Cut` path) so clipboard copy matches the highlight (§2.6).
**Sanity:** `v 3l d` deletes the span; `V j y` yanks 2 lines; `V >` indents; `V j` then `Ctrl-C` puts both whole lines on the clipboard.
**Tests:** charwise `d`/`y`/`c`; V-line `y`/`d`; `>`/`<`; `o` swap; selection painting byte range; **VisualLine `Ctrl-C`/`Ctrl-X` copy/cut the line-expanded range (matches the highlight), charwise Visual `Ctrl-C` copies the raw span.**

### CP7 — Text objects  *(risk: MED)*  — needs CP3
`iw aw iW aW`, quote pairs `i"/a" i'/a' i\`/a\``, bracket pairs `i(/a( i[/a[ i{/a{` — in Normal (`d`/`c`/`y`) and Visual. Balanced-pair / word scan in `text_object.rs`.
**Sanity:** `diw`, `ci(`, `vi"`, `aw` includes trailing space.
**Tests:** inner vs around for word / quote / each bracket; nested parens.

### CP8 — Search  *(risk: MED)*  — needs CP7 (word extraction for `*`/`#`)
First add **smartcase to the base search feature** (`SearchState`/`search`), so it benefits every user, not just vim — this is the only non-vim-gated change in the whole plan and ships even with `handler = "default"`. Then: `/ ?` open a command-line search; `n N` advance/retreat; `* #` search word under cursor. Reuse `SearchState` + `paint_search_overlays` + n/N counter. `/`/`?`/`*`/`#` return `VimOutcome::EnterSearch { forward, query }` (query from the cmdline for `/`/`?`, word-under-cursor for `*`/`#`); the App runs `enter_search_flow`. `n`/`N` need no outcome — they move the cursor over `EditorState::search` directly. **Creates `src/input/vim/cmdline.rs`** (the `:`/`/`/`?` command-line buffer editor) — the first checkpoint to need it; CP9 reuses it.
**Sanity:** `/word`↵ highlights all, cursor on first; `n` advances; `*` over a word searches it; a lowercase query in plain `Ctrl-F` search is now case-insensitive.
**Tests:** base-feature smartcase (lowercase = insensitive, mixed = sensitive) added to the existing search test file; then `/` starts flow; `n`/`N`; `*` extracts word.

### CP9 — Ex commands  *(risk: MED)*  — needs CP1 + the command line from CP8 (`cmdline.rs`)
`:` opens the hint-line command line; `:w :q :wq :s/pat/rep/flags :%s`. `parse_ex()` pure. `:w` → `VimOutcome::Save`, `:q`/`:wq` → `VimOutcome::Quit { save_first }`, both dispatched as the existing `Action::Save`/`Action::Quit` so the dirty-buffer confirm fires exactly as for `Ctrl-Q` (`:wq` saves first, so the buffer is clean by then). `:s`/`:%s` execute in `vim_ops` against `&mut EditorState` (no App round-trip); add the `regex` crate for them. `:e <path>` is deferred (out of scope). *(`:q!` force-quit is a small future addition if wanted.)*
**Sanity:** `:wq`↵ saves + quits; `:%s/a/b/g`↵ replaces all; `:w`↵ writes.
**Tests:** `:w` saves (tempfile); `:q` on a clean buffer quits; `:q` on a dirty buffer opens the quit-confirm; `:s` single-line; `:%s` global; flag handling (`g`, `i`); parse errors flash.

### CP10 — Polish & markdown-aware wiring  *(risk: LOW)*  — last
Vim-specific status/hint rows finalized; explicit markdown integration (`o` list-continue, `dd` renumber, `>>` list-indent); snapshot tests for badges. `regex` dependency documented in CLAUDE.md key-dependencies table.
**Sanity:** `o` after `1. Item` inserts `2. `; `>>` on `- item` indents with correct nesting; badges render in snapshots.
**Tests:** list-continue via `o`; renumber via `dd`; `tests/ui.rs` snapshots for `NORMAL`/`INSERT`/`VISUAL`.

### Dependency graph

```
CP1 ──► CP2 ──┬──► CP3 ──┬──► CP4
              │          ├──► CP6
              │          └──► CP7 ──► CP8 ──► CP9   (CP9 also needs CP1; CP8 supplies cmdline.rs)
              └──► CP5                              (CP5 needs only CP2)
all ──► CP10 (last)
```

**Highest-risk checkpoints:** CP1 (state location + dispatch integration without disturbing the coalesce burst) and CP3 (the operator×motion×count range reducer — every later operator depends on it). Validate both with the full existing suite green and the pure-function vim tests.

---

## 4. Key files reference

**Create:**
`src/input/vim.rs`, `src/input/vim/{state,feed,cmdline}.rs`,
`src/editor/vim_ops.rs`, `src/editor/vim_ops/{motion,text_object,operator,edits,search,ex}.rs`,
`tests/vim.rs`.

**Modify:**
`src/app.rs` (`vim: Option<VimState>` + init + accessors),
`src/app/event_loop.rs` (dispatch branch, coalesce guard, status label plumbing),
`src/app/actions.rs` (`ExitToPreview` reroute when vim active; any `App`-level vim entry like search/ex),
`src/app/flash.rs` (`hint_content` priority + `CommandLine`),
`src/input.rs` (facade re-exports),
`src/input/mode_handler.rs` (update the "deferred" doc comment),
`src/config/keymap.rs` (add `bind!("ctrl+r", Action::Redo)` to the default keymap, at CP4 — see §2.7),
`src/editor.rs` (`pub mod vim_ops;`),
`src/ui/status_bar.rs` (label field + style),
`src/ui/bottom_region.rs` (`HintContent::CommandLine`, `hint_line_for` vim arg, vim hint rows),
`src/ui/editor_view.rs` (thread label + `visual_line_mode`),
`src/ui/rendered_view.rs` + `src/ui/raw_view.rs` (`visual_line_mode` line-expansion in the render path; no reveal changes),
`src/search/state.rs` (+ `src/search.rs`) (**base-feature smartcase** in match computation, at CP8 — not vim-gated),
`tests/search.rs` (base-feature smartcase tests, at CP8),
`Cargo.toml` (`regex`, at CP9),
`CLAUDE.md` (architecture notes + dependency table, at CP10).

---

## 5. Risks & mitigations

1. **Coalesce burst vs. Normal mode** — `333` must accumulate, not coalesce. Guard the burst on `vim.sub_mode != Insert`. *(CP1)*
2. **Multi-step operators as one undo** — resolve the full range, issue one `apply_delta`. Never N char deletes. *(CP3)*
3. **`ExitToPreview` divergence** — reroute to vim-Normal in `dispatch_action` when vim active; keep `EditorState` vim-agnostic. *(CP1)*
4. **Visual selection clobbering** — bare motion keys are `Consumed` by `vim_feed` and never reach `edit_ops` (which would clear `selection`). Verify at each motion arm. *(CP6)*
5. **`:s` regex vs. literal search** — entirely separate paths; do not reuse `SearchState` for `:s`. *(CP9)*
6. **Word-class motions** — existing `move_word_*` is `W/B` (whitespace-only). Real `w/e/b` need punctuation/alphanumeric class logic; add in `vim_ops::motion`. *(CP2)*
