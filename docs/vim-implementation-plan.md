# Vim Mode — Implementation Plan

Implementation reference for adding Vim-style modal editing to edamame. Companion to [`vim-feature-prompt.md`](vim-feature-prompt.md) (the requirements/scope). This document records the agreed architecture, the locked design decisions, the module layout, and the checkpoint roadmap.

Status: **complete** — CP1 (walking skeleton), CP2 (core motions & mode entries), CP3 (operator+motion reducer), CP4 (remaining Normal primitives), CP5 (find & pair motions), CP6 (Visual & VisualLine operators), CP7 (text objects), CP8 (search), CP9 (ex commands), and CP10 (polish & markdown-aware wiring) all complete.

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

### CP1 — Walking skeleton  *(risk: HIGH)*  — ✅ **DONE**
**Goal:** config switch wires `VimState` onto `App`; vim branch in `dispatch_single_key`; `h j k l` move; `i`/`a`/`I`/`A` enter Insert; `Esc` → Normal (cursor left); status badge shows `NORMAL`/`INSERT`.
**Sanity check:** open a file with vim on → badge `NORMAL`; `j` moves; `i` then typing inserts; `Esc` → `NORMAL`; bare `j` no longer types.
**Files:** create `src/input/vim.rs`(+`state.rs`,`feed.rs`); `src/editor/vim_ops.rs` stub; modify `src/app.rs` (field + init from config), `src/app/event_loop.rs` (dispatch branch + coalesce guard), `src/ui/status_bar.rs` + `src/ui/editor_view.rs` (label), `src/input.rs` facade.
**Tests:** handler-disabled-by-default; `hjkl` move; `i`→insert→`Esc`→Normal; bare char doesn't insert in Normal; digit accumulation clears on `Esc`. Existing `tests/ui.rs` snapshots unchanged.

> **Implementation notes (CP1):**
> - The full `VimState` field set from §2.2 was laid down now (locked design). Forward-only enums (`PendingOp`, `FindKind`, `CmdLineKind`, and the not-yet-constructed `VimSubMode` variants) carry `#[allow(dead_code)]` with a "wired in CPn" comment — the binary crate (which re-`mod`s the sources separately from `lib.rs`) flags un-constructed pub variants as dead, and CI runs `-D warnings`. Each `allow` is removed as its checkpoint lands. Follows the existing `HintPrompt` "first consumer lands later" precedent.
> - CP1 motion/insert logic lives directly in `feed.rs` calling `editor.cursor.*` + `update_cursor_block` + `ensure_cursor_visible`; `vim_ops.rs` is a doc-only stub until CP2/CP3 need it.
> - `VimOutcome` is minimal for CP1 (`Pending` / `Consumed` / `Passthrough`). The `Save` / `Quit` / `EnterSearch` variants and their `event_loop` match arms are deferred to the checkpoints that emit them (CP8/CP9), to avoid pulling in `enter_search_flow`/`Action::Quit` wiring before it's exercised.
> - `vim_feed`'s signature drops the `keymap: &KeyMap` parameter shown in §2.3. CP1 needs no keymap (Ctrl-* chords are detected by modifier and returned as `Passthrough`, so the *caller* applies the keymap), and no later checkpoint's planned surface reads it either — `Save`/`Quit`/`EnterSearch`/`:s` all act without consulting bindings. The parameter is re-added only if a future checkpoint genuinely needs binding lookup inside the reducer.
> - App-wiring tests (handler enable/disable, startup mode) live in `src/app.rs`'s `vim_wiring_tests` unit module (private `App` fields); behavioral reducer tests are pure-function tests in `tests/vim.rs` (12 tests). `I` implements true first-non-blank; `Esc` from Insert never crosses a line boundary.
> - Counts accumulate in CP1 but do not yet drive motions (that's CP3) — they're parsed and cleared so the `Esc`-clears-count contract holds.
> - **Table chrome is the one exception to "motions traverse the raw buffer."** In a *rendered* view, `h`/`l`/`j`/`k` skip the auto-managed table border chrome (the `|` separators and the `|---|` alignment row), stepping cell-to-cell instead — landing on a border the editor owns would make the motion meaningless. This reuses the default handler's own table navigation (`EditorState::try_table_move_horizontal` / `try_table_move_vertical`, thin wrappers over `table_edit_ops::{table_move_horizontal, try_move_cell_vertical}`), so vim and arrow-key navigation behave identically inside a table. In **Raw** mode the borders are real, hand-editable source, so nothing is skipped — every character is a valid target. The skip is scoped to table chrome only; list markers stay char-navigable (they're user-authored text, unlike borders). The general principle ("rendered motions skip editor-managed chrome") extends to the word/find motions added in CP2/CP5.

### CP2 — Core motions & mode entries  *(risk: LOW)*  — needs CP1 — ✅ **DONE**
`w e b W E B 0 ^ $ gg G`; entries `a A I o O`; `v V` enter Visual/VisualLine (no operators yet).
New `Cursor`/`vim_ops::motion` helpers: `WordEnd` (`e`) and word-class `w/b` (vim distinguishes punctuation from alphanumeric; existing `move_word_*` is `W/B` semantics).
**Sanity:** every motion lands correctly; `o` opens a line below in Insert.
**Tests:** `w`/`e`/`b` boundaries, `$`/`^`/`0`, `gg`/`G`, `o`/`O`, `v`/`V` enter.

> **Implementation notes (CP2):**
> - **Motions live in the new `src/editor/vim_ops/motion.rs`** as a `Motion` enum + pure `resolve_motion(motion, count, cursor, buf) -> usize`, born this checkpoint as planned. Word classes are `Blank`/`Word`/`Punct`; `W/E/B` collapse `Word`+`Punct` into one class. `vim_ops.rs` re-exports `{Motion, resolve_motion}`.
> - **`resolve_motion`'s signature is leaner than the §2.4 sketch** — no `vim` / `viewport` params, since none of the CP2 motions need them. CP3 will widen it (vertical motions, `LineN`, `f/F/t/T`) when a motion genuinely consults that state. The `Motion` enum likewise holds only the CP2 variants; later checkpoints grow it (no `#[allow(dead_code)]` needed because every variant is constructed now).
> - **`h j k l` stay in `feed.rs`**, not `resolve_motion` — they carry bespoke table-chrome handling and manage the viewport themselves (CP1 behavior, unchanged). Only the offset-only motions route through `resolve_motion`.
> - **Counts accumulate but don't drive motions yet** — `apply_motion` passes a fixed count of `1`. Honoring `vim.count` (and the operator `motion_count`) is CP3's reducer work; `resolve_motion` already accepts the count so CP3 is a one-line flip at the call site.
> - **`o`/`O` insert a plain newline** via a single `EditDelta` (so undo is one unit) and enter Insert. List-aware continuation (marker copy / auto-renumber) and indent-copy are deferred to CP10 per §2.5.
> - **Visual is minimal-but-coherent (a small, deliberate reach into CP6).** `v`/`V` set `sub_mode` + `visual_anchor` + the shared `EditorState::selection`; in Visual, motions *extend* the selection (update `active`) instead of clearing it, and `Esc` exits to Normal dropping the selection. This is the floor needed for "enter Visual" to mean anything — without motion-extension, entering Visual would be a dead-end demo. Arrow keys (`←↑↓→`) mirror `h k j l` in Visual (extend, not passthrough) so the two navigation styles behave identically; in Normal, arrows still pass through to the default handler. **Operators (`d/y/c/…`), `o` (swap ends), the `v`↔`V` toggle, and VisualLine full-line expansion remain CP6**; in CP2 those keys are inert (Consumed no-ops) and a VisualLine selection paints charwise. `Ctrl-*` chords still pass through in Visual.

### CP3 — Operator+motion reducer  *(risk: HIGH)*  — needs CP2 — ✅ **DONE**
Counts (`3j`, `5l`, `d2w`, `2dd`); operators `d c y` × motions; `dd yy D C Y x X`; `p P` with the linewise register.
Build `resolve_motion_range` + `execute_operator`; the vim `VimRegister`.
**Sanity:** `3dw` deletes 3 words as one undo; `yy` then `p` duplicates the line below; `cw` deletes word + enters Insert.
**Tests:** `3dw` single undo; `dd`; `yy`/`p` linewise; `cw`; `D`/`C`/`Y`; `x`/`X`; count×op×motion both orderings.

> **Implementation notes (CP3):**
> - **Range resolution is an `OpRange` enum**, not the `(Range<usize>, bool)` tuple sketched in §2.4.  `OpRange::Chars(start..end)` carries a charwise span; `OpRange::Lines { first, last }` carries inclusive buffer-line indices.  Encoding linewise as line indices (rather than a char range + flag) lets `operator.rs` derive the full-line content range, the delete range (which may consume a neighboring newline), and the register text without re-deriving line boundaries from offsets — and removes the separate `bool`.  `resolve_motion_range` returns charwise spans for `w e b W E B 0 ^ $ h l` (end-inclusive for `e`/`E`) and linewise for `gg`/`G`; `vertical_line_range` / `doubled_line_range` produce the `j`/`k` and `dd` spans.
> - **Layering: `execute_operator` does NOT take `VimState`** (the §2.4 sketch passed `vim`).  `VimState` lives in the input layer (4), above `editor::vim_ops` (5), so an editor→input dependency would invert the architecture.  Instead `execute_operator(editor, op, range)` returns an `OpResult { register_text, linewise, enter_insert }` and `feed.rs` (input layer) folds that into `vim.register` / `vim.sub_mode`.  `op` is an editor-layer `Operator { Delete, Change, Yank }`; `feed.rs` maps `PendingOp → Operator` (the indent variants return `None` until CP4).
> - **`x X D C Y` reuse the operator machinery** rather than bespoke edits: `x`/`X` are `Delete` over `resolve_motion_range(Right/Left, …)` (clamped to the line, so `x` at line end and `X` at line start are no-ops), `D`/`C` are `Delete`/`Change` over `LineEnd`, `Y` is a doubled-yank line span.  Only `p`/`P` needed a dedicated `vim_ops::edits::paste` (charwise paste-after-cursor vs. linewise open-a-line, repeated by count, taking the register *contents* not a `VimRegister` so the editor layer stays input-free).  `r{c}`/`~`/`J`/`o`/`O` stay deferred to CP4/CP2.
> - **Single delta per operator** (Risk #2) holds: `execute_operator` issues exactly one `apply_delta`, verified by the `3dw`/`dd` single-undo tests.  **vim special cases implemented:** `cw`/`cW` → `ce`/`cE` on a non-blank (keeps the trailing space); `dw` on the last word of a line clamps to the line end (never joins lines); linewise delete of the final line consumes the *preceding* newline so no blank line is left; `cc` clears the line(s) but keeps one empty line and enters Insert.  **Deferred:** `NG` (count-to-line `G`) — `G` still ignores its count; not in the CP3 test list.  Ordered-list renumber after `dd` is left for CP10 per §2.5.
> - **Counts multiply** across `[count1]op[count2]motion` (`2d3w` = 6 words), both orderings tested.  `VimSubMode::OperatorPending` lost its `#[allow(dead_code)]` (now constructed); `PendingOp`'s allow was narrowed to the `IndentRight`/`IndentLeft` variants (CP4).  Tests: 29 new pure-function reducer tests in `tests/vim.rs` (64 total) plus 9 `resolve_motion_range`/range-helper unit tests in `motion.rs`.

### CP4 — Remaining Normal primitives  *(risk: LOW)*  — needs CP3 — ✅ **DONE**
`r{c} ~ >> << J u Ctrl-R`. `u` maps to `Action::Undo`; `Ctrl-R` works via passthrough once `bind!("ctrl+r", Action::Redo)` is added to the default keymap (§2.7) — no vim-specific claim. The rest mutate via `vim_ops::edits`.
**Sanity:** `r` replaces a char; `~` toggles case; `>>` indents; `J` joins; `Ctrl-R` redoes.
**Tests:** each primitive; `>>`/`<<` round-trip; `Ctrl-R` redo via the new keymap binding (fires in both default and vim mode).

> **Implementation notes (CP4):**
> - **`r` needed a pending-key field not in the §2.2 sketch.** `r{c}` waits one key for the replacement char, but the locked `VimState` had no slot for it (the §2.3 sequence examples assume an `f`/`t`-style pending-char mechanism that the struct never spelled out). Added a minimal `pending_replace: bool`, cleared by `reset_pending` and on `Esc`/arrows/`Ctrl-*` (which cancel with no edit). CP5's `f F t T` will want a richer pending-find state; `pending_replace` is the narrow CP4-only version of that, replaced/generalized then.
> - **`u`/`Ctrl-R` reuse the existing history path, no new edit code.** `u` calls `edit_ops::apply(editor, Action::Undo, …)` directly from the reducer (input layer → editor layer, allowed), honoring a count (`3u`). `Ctrl-R` is a `Ctrl-*` chord, so it already returns `Passthrough`; the only change is the new `bind!("ctrl+r", Action::Redo)` in the default keymap (lands here per §2.7). `first_key_for(Redo)` is now ambiguous between `Ctrl-Shift-Z` and `Ctrl-R` (the keymap is a `HashMap`), but no test or hint advertises Redo's chord, so the keybinds-overlay display is unaffected.
> - **`>>`/`<<` bypass `execute_operator`.** Indent is linewise and must **not** touch the register, so it takes its own `vim_ops::edits::indent_lines` path rather than the `Delete/Change/Yank` operator machinery. `>`/`<` enter `OperatorPending` via the extended `operator_for`; a new `feed_indent_pending` arm handles the doubled `>>`/`<<` (with counts, `3>>`) and cancels on any other following key. **Operator+motion indent (`>j`, `>G`) is deliberately out of CP4 scope** — only the doubled form ships, matching §1's `>> <<` surface; Visual `>`/`<` is CP6. Indent adds `tab_width` spaces to non-blank lines (blank lines stay empty, vim-style); outdent strips up to `tab_width` leading spaces or one leading tab; a no-op outdent records no delta. List-aware indenting stays deferred to CP10 (§2.5), like `o` list-continuation.
> - **Single delta per primitive** holds for the multi-line / multi-char forms: `3>>`, `3J`, and `3rx` each issue exactly one `apply_delta` (one `u`), verified by the single-undo tests. `J`'s multi-line join builds the whole replacement as one delta rather than N per-line joins. `~` advances the cursor past the toggled run (clamped to the line content); `J` lands the cursor on the first join column.
> - **`PendingOp`'s `#[allow(dead_code)]` removed** (its `IndentRight`/`IndentLeft` variants are now constructed). Tests: 20 new pure-function reducer tests in `tests/vim.rs` (92 total), including a keymap assertion that `Ctrl-R` (and `Ctrl-Shift-Z`) map to `Redo`.

### CP5 — Find & pair motions  *(risk: LOW)*  — needs CP2 — ✅ **DONE**
`f F t T` + `; ,` repeat; `{ }` paragraph; `%` matching pair. Entirely inside `vim_feed` + `motion.rs`.
**Sanity:** `f(` jumps; `;` repeats; `%` matches; `}` moves a paragraph.
**Tests:** `f`/`t` landing, `;`/`,` replay, `%` on each bracket type, `{`/`}`.

> **Implementation notes (CP5):**
> - **`FindKind` moved from `input::vim::state` to `vim_ops::motion`** (re-exported via `vim_ops.rs`).  The `Motion` enum now carries `FindChar(char, FindKind)`, so `FindKind` had to live in the editor layer; `state.rs` imports it back (input→editor, the correct direction).  Its `#[allow(dead_code)]` is gone now that the variants are constructed.  `Motion` also grew `ParagraphForward`/`ParagraphBackward` (`}`/`{`) and `MatchingPair` (`%`) — no `allow` needed (all constructed this checkpoint).
> - **New pure resolvers in `motion.rs`:** `find_char` (line-bounded; `t`/`T` stop one short; misses leave the cursor put; honors `count` for the *N*th occurrence), `paragraph` (boundary = a completely empty line — whitespace-only lines are *not* boundaries, matching vim; clamps to BOF/EOF), and `matching_pair` (scans the cursor→line-end for the first `()`/`[]`/`{}` bracket, then jumps to its nesting-aware match, forward from an opener / backward from a closer, across lines).  `%` ignores `count` (the vim *count*`%` "percent of file" motion is out of scope).  `find_char` takes a `skip_adjacent` flag (set only by `resolve_find_repeat`, the public `;`/`,` entry point) so a `t`/`T` *repeat* steps over a match sitting immediately in the search direction instead of staying stuck one char before the same target — vim's default `;` behavior; a *fresh* `t`/`T` still resolves with the flag off and correctly stays put when already adjacent.
> - **`resolve_motion_range` inclusivity extended:** a forward find (`f`/`t`) and `%` are end-inclusive operator targets (`df(` / `dt(` / `d%` include the landing char / matched bracket); backward finds fall through as the default exclusive `[lo, hi)` span, which is already correct.  A miss (`target == cursor`) stays empty so `df<miss>` is a no-op.
> - **Pending-find mechanism (`VimState::pending_find: Option<FindKind>`):** generalizes CP4's `pending_replace`.  `f`/`F`/`t`/`T` arm it and return `Pending`; the next key resolves via `feed_find_char`, checked at the top of `feed_normal`/`feed_visual` (so it works behind an operator — `df(` — and in Visual to extend the selection).  A non-char key or `Ctrl-*` chord cancels with no edit (and drops OperatorPending back to Normal), mirroring `feed_replace_char`; the plain-motion path also fails safe back to Normal if it is ever reached still in OperatorPending (it never is today — a find only arms behind Delete/Change/Yank — but the guard keeps a future operator from lingering half-consumed).  `last_find` is recorded on every find (success *or* miss) so `;` replays it and `,` replays the reversed direction (`reverse_find`: `f`↔`F`, `t`↔`T`).  `;`/`,` resolve through `resolve_find_repeat` (the till-skip path) and move via the shared `move_to_offset` helper — `apply_motion` now delegates to that same helper.
> - **`{ } %` route through `motion_for`**, so they work as plain motions, Visual extensions, and operator targets (`d}`, `d%`) for free.  **Deferred / out of scope:** `;`/`,` as operator targets (`d;`) — rare, cancels harmlessly; the exclusive-linewise special case for `d}` ending at column 0 (treated as plain charwise).  (The classic `;`-after-`t` "stuck on the adjacent match" case *is* handled — see the `skip_adjacent` note above.)
> - Tests: 14 new pure-function reducer tests in `tests/vim.rs` (109 total) plus 11 new `motion.rs` unit tests (27 total) covering find landing/till/count/miss, `;`/`,` replay+reverse, the `t`/`T` repeat adjacent-skip (forward and backward), `%` on each bracket type + nesting + forward-scan, `{`/`}` between blank lines, and the operator combos `df`/`dt`/`d%` (with single-undo).

### CP6 — Visual & VisualLine operators  *(risk: MED)*  — needs CP3 — ✅ **DONE**
Selection extends via motions; operators `d/x y c/s p r{c} ~/u/U > < J`; `o` swaps ends; `v`/`V` toggle/exit. The shared `visual_line_range` helper (used by the render-path `visual_line_mode` on `RenderedView`/`RawView`, the operators, and the system `Copy`/`Cut` path) so clipboard copy matches the highlight (§2.6).
**Sanity:** `v 3l d` deletes the span; `V j y` yanks 2 lines; `V >` indents; `V j` then `Ctrl-C` puts both whole lines on the clipboard.
**Tests:** charwise `d`/`y`/`c`; V-line `y`/`d`; `>`/`<`; `o` swap; selection painting byte range; **VisualLine `Ctrl-C`/`Ctrl-X` copy/cut the line-expanded range (matches the highlight), charwise Visual `Ctrl-C` copies the raw span.**

> **Implementation notes (CP6):**
> - **The shared widening helper lives in `src/editor/vim_ops/visual.rs`** (`visual_line_bounds` → inclusive `(first, last)` buffer lines; `visual_line_char_range` → the whole-line char span, trailing newline included, EOF-clamped on the final line).  All three consumers route through it: the render overlay (`visual_line_mode` on `RenderedView`/`RawView`), the Visual operators, and the App-layer clipboard.  `selection` itself is **never** snapped — it stays the charwise `{anchor, active}` pair, so a `v`↔`V` toggle is lossless and render/operator/clipboard can't diverge (§2.6).
> - **Visual commands are intercepted in `feed_visual` ahead of the shared motion path** via a new `feed_visual_command`: it returns `Some` for the operators / `o` / `v` / `V` and `None` for everything else, so motions, counts, `gg`, and finds still flow through `feed_command_char` and *extend* the selection.  Operators reuse CP3's `execute_operator` (charwise span for Visual, `OpRange::Lines` for VisualLine); `>`/`<` reuse CP4's `indent_lines` (linewise even from charwise Visual, no register); `~` uses a new `vim_ops::edits::toggle_case_range`; `J` reuses `join_lines` with a count derived from the selected line span; `o` swaps `selection.{anchor,active}` and re-homes the cursor + `visual_anchor`; `v`/`V` toggle to the other visual mode (or exit when the pressed key matches the current mode).  All operators issue a **single** delta (one `u`), verified by `visual_operator_is_a_single_undo_unit`.
> - **Charwise Visual operates on the raw half-open `selection.range()` span — per §2.6's "copy always matches the highlight" — not vim's inclusive-of-the-cursor-char span.**  `v l d` deletes one char, not two.  This is the plan's deliberate trade (copy == highlight == operator, all consistent) rather than stock-vim inclusivity; a future refinement could widen all three consumers by one grapheme together if exact vim parity is wanted.
> - **The remaining in-scope Visual operators — `p`/`P`, `r{c}`, `u`/`U` — are wired through the same charwise/line-expanded range helper** (`visual_char_range`, which folds the `~` range derivation too).  `p`/`P` (`run_visual_paste`) replace the selection with the unnamed register as one delta via `vim_ops::edits::replace_range_with`; a charwise register dropped over a VisualLine span gets a trailing `\n` so it keeps its own line, and an empty register is a no-op that still leaves Visual.  **The register is left unchanged on `p` — a deliberate departure from vim's default** (which clobbers it with the deleted text) so the same yank can be pasted over several selections in turn.  `r{c}` reuses CP4's `pending_replace` flag, now checked at the top of `feed_visual`; `feed_visual_replace_char` fills the span via `replace_char_range` (newlines preserved), and a cancel (Esc / non-char / chord) keeps the Visual selection.  `u`/`U` force-case via `set_case_range` (in Visual, `u` is *not* undo — that's Normal-only).  All are single-delta.
> - **`run_operator` and `run_visual_operator` share `fold_op_result`** — the `OpResult` → `VimState` fold (register store, parse reset, Insert/Normal transition, editor refresh) lives in one place so the Normal and Visual operator paths can't drift.
> - **VisualLine `Ctrl-C`/`Ctrl-X` are widened at the App dispatch layer** (`App::dispatch_visual_line_clipboard`), keeping `EditorState`/`edit_ops` vim-agnostic: it temporarily installs the line-expanded range, runs the existing `Copy`/`Cut`, then for `Copy` restores the charwise span (never snapped, Visual continues) and for `Cut` deletes the lines and drops to Normal.  Charwise `Ctrl-C` is not intercepted, so it copies the raw span.  *(Edge: `Ctrl-X` of the document's final line(s) leaves a stray empty line — the App Cut path doesn't consume the preceding newline the way the linewise `dd` operator does.  Not hit by the tests; acceptable for the clipboard path.)*
> - Tests: 32 new pure-function reducer tests in `tests/vim.rs` (140 total) covering charwise/linewise `d`/`x`/`y`/`c`/`s`, VisualLine change (keep-one-empty-line + Insert), `>`/`<` round-trip + register-untouched, `~`, `u`/`U` force-case, `r{c}` (charwise + newline-preserving linewise + Esc-cancel), `p`/`P` (charwise span, linewise-register whole-line, charwise-register-keeps-line, empty-register no-op), `J` (multi-line + single-line pull-up), `o` swap, the `v`↔`V` toggles, and single-undo; plus 3 unit tests in `visual.rs`, a `RawView` `visual_line_mode` whole-line-highlight test, and 3 App-level clipboard tests in `app.rs`'s `vim_wiring_tests` (VisualLine copy-without-snap, cut-and-exit, charwise raw-span copy).

### CP7 — Text objects  *(risk: MED)*  — needs CP3 — ✅ **DONE**
`iw aw iW aW`, quote pairs `i"/a" i'/a' i\`/a\``, bracket pairs `i(/a( i[/a[ i{/a{` — in Normal (`d`/`c`/`y`) and Visual. Balanced-pair / word scan in `text_object.rs`.
**Sanity:** `diw`, `ci(`, `vi"`, `aw` includes trailing space.
**Tests:** inner vs around for word / quote / each bracket; nested parens.

> **Implementation notes (CP7):**
> - **The resolver lives in the new `src/editor/vim_ops/text_object.rs`** as a `TextObject` enum (`Word { inner, big }`, `Quote { inner, quote }`, `Pair { inner, open, close }`) + a pure `resolve_text_object_range(obj, cursor, buf) -> Option<Range<usize>>`. `vim_ops.rs` re-exports `{TextObject, resolve_text_object_range}`. All in-scope objects are **charwise**, so the resolver returns a bare `Range` (no linewise flag — the §2.4 `(Range, bool)` sketch is unneeded here), and `feed.rs` wraps it in `OpRange::Chars`. `None` (cursor outside any pair / no quote on the line / empty buffer) cancels the operator; an *empty* inner range (`ci(` on `()` → `start == end`) is **not** `None` — it flows through `execute_operator`, which no-ops a Delete/Yank but still enters Insert for Change (vim's `ci(` on an empty pair).
> - **Word objects reuse `motion::class`/`Class`** (made `pub(crate)` this checkpoint) so `iw`/`aw` word-class boundaries can't drift from the `w`/`e`/`b` motions. A newline is always a hard boundary (a word object never spans lines). `aw` extends over trailing whitespace, falling back to leading whitespace when there is none (and `aw` on whitespace extends over the following word) — vim's rules.
> - **Quote objects are line-bounded**, pairing quotes left-to-right and choosing the first pair whose close is at/after the cursor (so a cursor *before* or *between* strings selects the next one). `a"` adds the quotes plus trailing (or leading) whitespace. **Bracket-pair objects span lines** via a nesting-aware backward scan for the enclosing open (a cursor *on* an open is its own answer; on a close it resolves that close's match) then a forward scan for the matching close.
> - **`b`/`B` aliases added** for the paren / brace pairs (`ib`/`ab`, `iB`/`aB`) and both bracket directions resolve to the same pair (`i)` == `i(`) — standard vim, zero extra cost. This is a small superset of the documented `i(/i[/i{` surface; a deviation noted here, covered by a `di}`-from-closing-bracket test.
> - **Wiring: a pending text object generalizes the CP4/CP5 `pending_replace`/`pending_find` pattern** via the existing `VimState::pending_text_object: Option<bool>` field (`Some(true)` = inner). `i`/`a` behind an operator (in `feed_operator_pending`) or in Visual (in `feed_visual_command`) arm it and return `Pending`; the next key resolves through a new `feed_text_object`, checked at the top of `feed_normal`/`feed_visual` (so it works behind an operator — `diw` — and in Visual — `viw` sets the selection to the object via `select_text_object`). A non-object key or a `Ctrl-*` chord cancels with no edit (dropping OperatorPending back to Normal), mirroring `feed_find_char`. The Normal `i`/`a` *without* an operator are untouched (Insert entry / append).
> - **Single delta per object** holds (`diw` is one `u`), since the object resolves to one `OpRange::Chars` run through the existing `run_operator`/`execute_operator` path. Tests: 15 new pure-function reducer tests in `tests/vim.rs` (154 total) covering `diw`/`daw`/`ciw`, `di"`/`da"`, `ci(`/`da[`/`di}`-from-closing-bracket, nested-paren innermost pick, the missing-pair no-op, Visual `viw`/`vi"`, single-undo, and the Esc-cancel; plus 15 unit tests in `text_object.rs` (inner/around for word/quote/each bracket, nested parens, empty inner pair, multi-line pair, outside-any-pair `None`).

### CP8 — Search  *(risk: MED)*  — needs CP7 (word extraction for `*`/`#`) — ✅ **DONE**
First add **smartcase to the base search feature** (`SearchState`/`search`), so it benefits every user, not just vim — this is the only non-vim-gated change in the whole plan and ships even with `handler = "default"`. Then: `/ ?` open a command-line search; `n N` advance/retreat; `* #` search word under cursor. Reuse `SearchState` + `paint_search_overlays` + n/N counter. `/`/`?`/`*`/`#` return `VimOutcome::EnterSearch { forward, query }` (query from the cmdline for `/`/`?`, word-under-cursor for `*`/`#`); the App runs `enter_search_flow`. `n`/`N` need no outcome — they move the cursor over `EditorState::search` directly. **Creates `src/input/vim/cmdline.rs`** (the `:`/`/`/`?` command-line buffer editor) — the first checkpoint to need it; CP9 reuses it.
**Sanity:** `/word`↵ highlights all, cursor on first; `n` advances; `*` over a word searches it; a lowercase query in plain `Ctrl-F` search is now case-insensitive.
**Tests:** base-feature smartcase (lowercase = insensitive, mixed = sensitive) added to the existing search test file; then `/` starts flow; `n`/`N`; `*` extracts word.

> **Implementation notes (CP8):**
> - **Smartcase landed in `search::state::find_all`** (the one matcher both `Ctrl-F` and vim's `/` share), gated only on "the pattern contains an uppercase char". The case-sensitive path keeps `str::match_indices`; the new case-insensitive path (`find_all_ci`) scans char-by-char against the *untouched* haystack so byte offsets stay aligned for multibyte text (lowercasing the strings up front would shift offsets for chars whose lowercase form differs in byte length). The old `find_all_is_case_sensitive_and_non_overlapping` unit test was **replaced** (its assertion that `/foo/` skips `Foo`/`FOO` is the opposite of smartcase); new unit tests cover lowercase-insensitive, uppercase-sensitive, non-overlap, and multibyte alignment, plus two integration tests in `tests/search.rs`.
> - **`VimOutcome` gained `EnterSearch { forward, query }`** and lost `Copy` (it now carries a `String`). `/` `?` arm the command line and, on submit, return `EnterSearch`; `*` `#` return it directly from the word under the cursor. The App's `dispatch_single_key` runs the new `App::enter_vim_search`. `n`/`N` need no outcome — they advance/retreat `EditorState::search` directly inside the reducer (`search_repeat`, honoring a count, mirroring `App::search_move_focus`).
> - **The deferral / gate is now one shared predicate, `App::search_flow_captures`** (`search.is_some() && (vim.is_none() || is_replace_flow())`). It replaces the four raw `editor.search.is_some()` checks (the `dispatch_single_key` vim deferral, the `dispatch_action` default-deny gate, the coalesce-burst guard, and the mouse gate). The effect: a vim **navigate-only** search does *not* capture — vim keeps full control (motions, edits, Insert typing, `n`/`N`) while the matches stay highlighted — whereas a **replace** flow (`Ctrl-F` with a replacement) still captures its `Tab`/`r`/`a` keys, and non-vim behavior is byte-for-byte unchanged (`vim.is_none()` ⇒ "any search captures"). This is what makes "`n`/`N` move over `EditorState::search` directly" reachable: without it the old `search.is_some()` deferral would never call `vim_feed` once a search was active.
> - **`Esc` in vim Normal dismisses an active (navigate) search** (drops `editor.search` directly, *not* via `exit_search`, so the pre-search scroll is **not** restored — the cursor stays on the match it reached). This both keeps the prior `esc_exits_the_search_flow_even_with_vim_active` regression test green and gives vim users a "clear highlights" key before CP9's `:noh`. A capturing replace flow never reaches this arm (it defers to `DefaultHandler`'s `SearchExit`). **Deviation from stock vim**, where `Esc` does not clear `hlsearch`; documented and deliberate.
> - **`/` is forward-from-cursor, `?` is backward-from-cursor** (cursor-relative initial focus via `partition_point` over the byte-offset match list, wrapping around the document) — vim semantics, unlike the modal `enter_search_flow` which always starts at the first match. `n`/`N` are plain advance/retreat (next/prev) regardless of the originating direction, matching the plan's "advance/retreat" wording (a minor simplification vs. stock vim, where `n` after `?` reverses).
> - **`cmdline.rs` is a view-agnostic text field** (`feed_key(&mut CmdLineState, key) -> CmdLineStep::{Editing, Submit(String), Cancel}`) with cursor/insert/backspace/Home/End; `Backspace` past an empty line cancels (closes the `/`). CP9 reuses it verbatim for `:`. The hint line renders it via the new `HintContent::CommandLine { prefix, text, cursor }` (block cursor in `theme.cursor_rendered`), slotted into `App::hint_content` at `Prompt > CommandLine > Transient > hovered-link > Chords`.
> - **`*`/`#` reuse the `Class` word scan** in the new `vim_ops::search::word_under_cursor_at` (literal keyword text — no `\<…\>` boundaries, since the base search is literal-substring, not regex). It skips forward on the line to the next keyword when the cursor isn't on one, and never crosses a newline. It returns the keyword's **start** offset alongside the text: `search_word_outcome` repositions the cursor to that start before emitting `EnterSearch` (vim's behavior), so a backward `#` from the *middle* of an occurrence jumps to the *previous* occurrence rather than snapping to the current word's start.
> - **Post-testing fixes (manual smoke test):**
>   - **`Tab`/`Shift-Tab` now walk the matches like `n`/`N`** for *any* navigate search under vim — `/`, `?`, `Ctrl-F`, or the palette's "Search and replace". `feed_normal` handles them via `search_repeat` (identical to `n`/`N`) when `editor.search.is_some()`; a replace flow still captures them through `DefaultHandler` as before. (Supersedes the earlier "`Tab` is an inert no-op" plan note.) The hint chord row deliberately keeps showing `Tab`/`Shift-Tab` — vim users know to try `n`/`N` too.
>   - **Normal/Visual never edit via non-character keys.** `Backspace`/`Delete`/`Enter`/`Tab` previously fell through to the default keymap (`DeleteCharBack`/`Newline`/`InsertTab`), violating the "Normal mode does not edit" rule. They are now consumed: in Normal, `Backspace` moves left, `Delete` moves right, `Enter` drops to the next line's first non-blank, `Tab` (no search) is inert; in Visual they extend the selection (with `Tab`/`Shift-Tab` inert). Genuine navigation keys (arrows, Home/End, PageUp/Down) still pass through. *(`Delete` deviates from stock vim, where `<Del>` = `x`; chosen to uphold the no-edit rule.)*
>   - **Paste is vim-aware** (`App::dispatch_paste`). A bracketed paste into an open `/`/`?` command line fills the prompt (`cmdline::paste_str`, newlines stripped) instead of the buffer; in any non-Insert sub-mode a paste is dropped (use `p`/`P`). This fixes a panic where pasting during `/` edited the buffer and desynced the parsed doc / search ranges (char-boundary slice in `rendered_view::paint`).
> - **Deferred to CP10 / known limitations:** the search hint *chord row* still shows the generic flow chords (`Tab Next` / `Esc Exit`) during a vim search — only the `n/N` match **counter** is reused this checkpoint; vim-specific hint rows are CP10. `n`/`N`/`*`/`#`/`/`/`?` act in Normal only (swallowed in Visual). Tests: 12 + 7 pure-function reducer tests in `tests/vim.rs` (178 total), 6 `word_under_cursor_at` unit tests, 5 `cmdline` unit tests, 4 smartcase unit tests + 2 integration tests, and `enter_vim_search` / Tab-dispatch / paste / `#`-mid-word App-level tests.

### CP9 — Ex commands  *(risk: MED)*  — needs CP1 + the command line from CP8 (`cmdline.rs`) — ✅ **DONE**
`:` opens the hint-line command line; `:w :q :wq :s/pat/rep/flags :%s`. `parse_ex()` pure. `:w` → `VimOutcome::Save`, `:q`/`:wq` → `VimOutcome::Quit { save_first }`, both dispatched as the existing `Action::Save`/`Action::Quit` so the dirty-buffer confirm fires exactly as for `Ctrl-Q` (`:wq` saves first, so the buffer is clean by then). `:s`/`:%s` execute in `vim_ops` against `&mut EditorState` (no App round-trip); add the `regex` crate for them. `:e <path>` is deferred (out of scope). *(`:q!` force-quit is a small future addition if wanted.)*
**Sanity:** `:wq`↵ saves + quits; `:%s/a/b/g`↵ replaces all; `:w`↵ writes.
**Tests:** `:w` saves (tempfile); `:q` on a clean buffer quits; `:q` on a dirty buffer opens the quit-confirm; `:s` single-line; `:%s` global; flag handling (`g`, `i`); parse errors flash.

> **Implementation notes (CP9):**
> - **The parser + substitution live in the new `src/editor/vim_ops/ex.rs`** (`parse_ex(&str) -> Result<ExCommand, ExError>`, pure; `execute_substitute(&mut EditorState, &Substitution) -> Result<usize, ExError>`). `vim_ops.rs` re-exports `{parse_ex, execute_substitute, ExCommand}` (the `ExError`/`Substitution` types stay reachable via the `ex::` path — re-exporting unused names tripped the binary crate's `unused_imports` under `-D warnings`, the same lib-vs-bin quirk CP1 noted). `ExError` is a `thiserror` enum whose `Display` is exactly the hint-line flash text.
> - **The vim regex dialect is translated, not passed through (CP9 follow-up).** A new pure module `src/editor/vim_ops/vim_regex.rs` lets users type patterns *as they would in vim*: `translate_pattern(&str) -> Result<String, ExError>` rewrites magic-level escaping (`\( \) \+ \|` ↔ very-magic `( ) + |`, plus the `\v \m \M \V` switches), `\<`/`\>` word boundaries (→ lookaround), and the `\a \l \u \x \o \h` character classes (→ bracket expansions); `expand_replacement(&str, &Captures) -> String` applies vim's `\1` / `&` / `\u \U \l \L \e \E` per match (done by hand, because no engine does vim case-folding in its `$1` replacement). A leading quantifier is treated as a literal (vim's rule, and it stops the engine erroring). Rare atoms (`\zs \ze`, postfix `\@=`, `\%[…]`/`\%^`/…) are rejected with a friendly `ExError::UnsupportedPattern` rather than mistranslated. Unit-tested in `vim_regex.rs` (17 tests). An escaped delimiter `\/` is reduced to a literal `/` during parsing, before the pattern reaches the translator.
> - **The engine is `fancy-regex` (0.18), not the `regex` crate.** It is pure Rust (no system library — matching the project's libchafa/rustls stance), built *on* `regex` (delegates the simple fast path, only backtracks for fancy features), and supports **backreferences and lookaround** — which is exactly what makes pattern backrefs (`\(.\)\1`) and the `\<`/`\>`→lookaround translation work. Its methods return `Result` (matching can error at run time, e.g. a backtrack limit), so `substitute_line` folds those into the same `ExError`/flash path; `RegexBuilder::case_insensitive` drives the `i` flag. The `/` search path is untouched — still literal-substring + smartcase, no regex.
> - **Per-line semantics, single delta.** `execute_substitute` processes each affected line independently — `:s` the cursor's line only, `:%s` every line — replacing the first match (or all with `g`), with the trailing `\n` split off so `^`/`$` anchor per line and the newline is never consumed. The whole edit is one coarse `EditDelta` over the affected char range, so `:%s/…/…/g` is a **single undo unit** (verified by `ex_substitute_is_a_single_undo_unit`). A zero-match run records no edit (returns `Ok(0)`) and leaves the buffer clean. The cursor parks at the start of the first affected line rather than `apply_delta`'s default end-of-insert (which for `:%s` would jump to EOF).
> - **`VimOutcome` gained `Save`, `Quit { save_first }`, and `Flash(String)`.** `:w`/`:q`/`:wq` bubble up as `Save`/`Quit` and the `event_loop` dispatches the existing `Action::Save`/`Action::Quit` (so the dirty-quit confirm and save flash are reused verbatim; `:wq` saves first → clean → quits). The substitution runs *in the reducer* (it holds `&mut EditorState`); only its user-facing result message rides up as `Flash` — flashing is an App concern (`MessageKind` lives in layer 2, above the input layer), so the reducer can't do it directly. The App flashes every ex message as `Info`.
> - **Wiring reused CP8's `cmdline.rs` verbatim.** `:` opens a `CmdLineKind::Ex` command line via the renamed `start_cmdline` (was `start_search_cmdline`); `feed_cmdline` now takes `&mut EditorState` (+ viewport) so a submitted Ex line routes to the new `submit_ex`. The hint-line prefix renders for free via `CmdLineKind::prefix()` (`:`), and bracketed paste into the prompt already routes through `cmdline::paste_str`. `CmdLineKind::Ex`'s `#[allow(dead_code)]` is gone now that it's constructed.
> - **`dispatch_single_key` was widened to `pub(super)`** so the App-level CP9 tests can drive a full `:`-command (the `:`, the body, Enter) through the real key entry point. Tests: 19 pure-function reducer tests in `tests/vim.rs` (197 total) — the core `:w`/`:q`/`:wq` outcomes, `:` open/Esc-cancel, empty-`:` no-op, `:s` first-only / `g` / `%s` global / `i` flag / single-undo / no-match flash / parse-error flash / invalid-regex flash, **plus the follow-up vim-syntax cases** (`\(…\)\2 \1` swap, `\U\1` case modifier, `\(.\)\1` pattern backref, `\v` very-magic, `\<…\>` word boundary, and an unsupported-atom flash); 6 `parse_ex` + 17 `vim_regex` unit tests in the lib; and 5 App-level tests in `app.rs`'s `vim_wiring_tests` driving `:w` (tempfile save), `:q` clean-quit, `:q` dirty → quit-confirm, `:%s` edit+flash, and parse-error flash end-to-end.
> - **Deferred / out of scope:** `:e <path>`, `:q!` force-quit, bare `:s` (repeat-last-substitution), ranges other than the current line / `%`, and the rare pattern atoms the translator rejects (`\zs \ze`, postfix `\@=`, `\%[…]`/`\%^`/…). `:x` is treated as `:wq` (a minor simplification — vim's `:x` writes only when modified). The `fancy-regex` dependency is documented in the CLAUDE.md key-dependencies table at CP10.

### CP10 — Polish & markdown-aware wiring  *(risk: LOW)*  — last — ✅ **DONE**
Vim-specific status/hint rows finalized; explicit markdown integration (`o` list-continue, `dd` renumber, `>>` list-indent); snapshot tests for badges. `regex` dependency documented in CLAUDE.md key-dependencies table.
**Sanity:** `o` after `1. Item` inserts `2. `; `>>` on `- item` indents with correct nesting; badges render in snapshots.
**Tests:** list-continue via `o`; renumber via `dd`; `tests/ui.rs` snapshots for `NORMAL`/`INSERT`/`VISUAL`.

> **Implementation notes (CP10):**
> - **Markdown wiring reuses the byte-oriented `list_edit` primitives** the non-vim editing path already drives, so vim and arrow-key list editing produce identical structure. Three new editor-layer helpers in `vim_ops::edits` route through `edit_ops::{cursor_byte, apply_byte_delta}` (both `pub(in crate::editor)`, so the `vim_ops` descendant can call them) and all bail out in `Mode::Raw`, where markers are hand-editable source:
>   - **`open_list_continue(editor, below)`** (`o`/`O`): continues the list with a fresh empty item via `list_edit::continue_item` (one delta — it renumbers ordered runs *inline*, keeping `o` a single undo). `o` continues from the cursor's item line-end; `O` continues from the *previous* item's line-end so the new marker lands above. **`O` on the list's first item returns `false`** (there is no earlier item to split from) and falls back to a plain open-above — a small, documented limitation (the only list `O` case that isn't marker-aware).
>   - **`renumber_list_at_cursor(editor)`** is called from `feed.rs::fold_op_result` whenever an operator result is `linewise` (so `dd`/`dj`/`dG`/`Vd` all renumber). It is a no-op for bullet lists, already-sequential lists, non-lists, and unchanged buffers (yank/`cc`), so calling it broadly is safe. It mirrors `edit_ops::list_renumber_at_cursor`, which the non-vim delete path runs automatically. **`dd`+renumber is two deltas** (delete, then renumber) exactly like the non-vim `DeleteLine` — consistent with the rest of the app; the CP3 single-undo guarantee holds for plain-text `dd` (no renumber delta).
>   - **`indent_list_item(editor, right)`** (`>>`/`<<`): a **bare** `>>`/`<<` (count 1) on a list item indents it structurally via `list_edit::indent_item`/`outdent_item` (nest / un-nest, ordered renumber). A counted `N>>`, a non-list line, or Raw mode falls back to the plain space-based `indent_lines` (CP4). Wired in `feed_indent_pending`.
> - **Hint rows: `hint_line_for` gained a `vim_mode: Option<VimSubMode>` arg** (§2.6). When `Some`, a vim-specific chord row replaces the default mode row: Normal/OperatorPending → `i Insert · v Visual · : Cmd · / Find · ^P Menu · ^S Save · ^Q Quit`; Insert → `Esc Normal · ^P Menu · ^S Save`; Visual/VisualLine → `d Delete · y Yank · c Change · > Indent · Esc Normal`. Modal keys are literal glyphs; the app actions are looked up live from `keymap` (rebinds appear next frame, unbound actions drop out). **An active search still wins** (its arm precedes the vim arm), so the search-flow chords + match counter stay visible while navigating matches. The App threads `self.vim.as_ref().map(|v| v.sub_mode)` from `flash.rs::hint_content`. `ui::bottom_region` already depends on `crate::input` (for `diff_hint`), so importing `VimSubMode` is consistent with existing layering.
> - **Status badges were already wired** (`StatusBarState::vim_mode_label` + `vim_badge_style`, landed earlier). CP10 only adds the snapshot coverage: `tests/ui.rs` gains `snapshot_status_bar_vim_{normal,insert,visual}` plus an assertion that the vim badge wins over the `EDIT` view-mode badge.
> - **The regex dependency in the key-dependencies table is `fancy-regex`, not `regex`** (CP9 follow-up) — documented in `CLAUDE.md` accordingly.
> - Tests: 11 new pure-function reducer tests in `tests/vim.rs` (`o`/`O` list continue + single-undo, fallback outside a list, `dd` renumber ordered / no-op bullet, `>>`/`<<` bullet + nested-ordered + plain fallback); 5 vim-hint unit tests in `bottom_region.rs`; and 3 badge snapshots + 1 badge assertion in `tests/ui.rs`.

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

café
