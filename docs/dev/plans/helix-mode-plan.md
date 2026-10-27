# Helix Mode Plan

Status: in progress — commit 1 done (uncommitted on the branch), commits 2–4c not started. Branch: `feat/helix-mode`, cut from `develop` at `c13fc67`. Where an earlier draft and the working tree disagree on a name, [Key types](#key-types) is canonical and matches the tree.

## Goal

Add a Helix-style modal editing handler alongside the default handler and vim. Helix must integrate tightly with the existing vim code: one shared modal core, one set of guarded edit primitives, one prompt/ex/search stack, with Helix-only code limited to what is genuinely different about Helix (motion-then-verb key parsing, word *selection* motions, and the Normal/Select extend rule).

## Background

An outside PR (#86) built Helix as a parallel silo: its own reducer, ex parser, paste/join/indent/replace/find/search/save paths and its own composition of the table and list guards. It was declined for that reason, but it confirmed the feature's shape and some of its pieces are worth keeping (the `ModalInput` enum, typed badge resolution, the three-way editing-mode setting). Four vim bugs it exposed (#91–#94: discrete commands merging into one undo step, `O` on a first list item, the unbounded paste repeat, a linewise delete and its renumber being two undo steps) are already fixed on `develop` in `c13fc67`. This plan assumes those fixes and builds on them: `History::break_group`, the command barriers in `vim_feed`, `edit_ops::{repeat_for_paste, PASTE_TOO_LARGE}`, `History::push_composed`, and the first-item `O` in `vim_ops::edits`.

How "one undo step" works today, since every guarded entry point below relies on it: `vim_feed` calls `History::break_group` before and after each non-Insert command. A barrier only stops the next `record` from merging into the previous entry; it composes nothing. A command that writes once is therefore one step. A command that writes twice (an edit, then the list renumber it triggers) records two entries and folds them into one with `History::push_composed`, which the renumber paths already do after `c13fc67`.

Two architecture designs were compared before this plan. Both rejected a single generic reducer: vim's grammar is verb-then-motion with operator-pending state, Helix's is motion-then-verb with none, and most shared key letters mean different things in each (`w`, `x`, `d`, `u`, `%`). A shared key table would cover about ten keys while a policy-trait reducer would put the vim suite at risk. The plan below is the hybrid of the two designs.

## Decisions

| # | Decision |
|---|---|
| 1 | v1 key subset as in [Key table](#key-table); no dot-repeat, macros, surround, multiple selections, or tree-sitter text objects. |
| 2 | Helix semantics win for selection behavior (cross-line `f`/`t`, `n` selects the match, `u` restores a selection, `%` selects all, `mm` matches brackets, `;` collapses). edamame's shared behavior wins for Markdown specifics (table refusals, list continuation and renumbering) and character rules (`~` uses vim's one-char case mapping). |
| 3 | The four vim fixes are already on `develop`; not part of this work. |
| 4 | Helix reuses `parse_ex` and gets every command it has, plus Helix aliases. Force quit (`:q!`) is added for both handlers. The aliases are accepted under vim too, since the parser is shared. |
| 5 | Helix `/` `?` reuse vim's incsearch and the App's `EnterSearch` path. |
| 6 | Badges use Helix's labels `NOR` / `INS` / `SEL`. Colors reuse `status_mode_vim_*` (Helix has no fixed mode palette: its `ui.statusline.{normal,insert,select}` keys are theme-defined and only used when `editor.color-modes` is on, which is off by default). No new theme keys. |
| 7 | Built on `develop` from scratch; nothing is merged from PR #86. Commits that carry over an idea or code shape from it credit the contributor with a `Co-authored-by` trailer (human trailers pass the pre-push hook). |
| 8 | Six commits: `ModalInput` refactor → guarded range primitives in the editor layer → shared modal core → Helix in three parts (4a selection and motions, 4b verbs, 4c config, UI and docs). |
| 9 | Selection model: Helix stores selections half-open, the convention the default handler, the mouse and the clipboard already use and the one Helix uses internally. The cursor is drawn on the head grapheme, so for a forward selection `cursor.offset == prev_grapheme(active)`, not `active`. vim Visual keeps its inclusive model (`cursor.offset == selection.active`) unchanged. Linewise-ness is derived from a selection's shape, not stored. See [Selection model](#selection-model). |
| 10 | Shared edits are plain guarded functions returning `Result<_, Refusal>`, not a `Verb` enum. The register stays in the input layer. |
| 11 | A handler switch builds a fresh `ModalCore`, so vim's linewise register never leaks into Helix. Re-selecting the live handler keeps its session, including one parked behind a read-only document (`App::set_modal_handler`). |
| 12 | The shared input module is `input::modal_core`: the session enum lives in `modal_core/session.rs`, `ModalCore` in `modal_core/state.rs`. The shared outcome is `KeyOutcome` (`ModalOutcome` is taken by `app::modal`). |

## Architecture overview

```
input::vim::feed     input::helix::feed          ← key parsers only (vim grammar / Helix key table)
          \              /
         input::modal_core                        ← ModalInput, ModalCore, KeyOutcome, prompt/ex/search
                 |                                  submit, counts, register, shared key helpers
editor::vim_ops::guard   editor::helix_ops        ← guarded range edits (shared) / Helix selection
          \              /                          geometry and word-selection motions
editor::vim_ops::{motion, operator, edits, table, text_object, visual, ex, incsearch}
```

`editor::vim_ops` keeps its name: renaming it touches nearly every file for no behavior gain. Its module doc gains a line saying `guard`, `table`, `edits`, `operator`, `text_object`, `ex` and `incsearch` are handler-agnostic. `input::vim` and `input::helix` never import each other; both import `input::modal_core`. Layering is unchanged: everything new sits in layers 4 (`input`) and 5 (`editor`).

### Key types

```rust
// input/modal_core/state.rs (commit 3)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModalCore {
    pub count: Option<u32>,
    pub pending_replace: bool,
    pub pending_find: Option<FindKind>,
    pub pending_text_object: Option<bool>, // vim `i`/`a` after an operator or in Visual; Helix `mi`/`ma`
                                           // (Helix: `m` sets `HelixState::pending_match`; a second `m`
                                           // resolves `mm`, while `i`/`a` clears `pending_match` and sets
                                           // this field, and the next key names the object)
    pub last_find: Option<(FindKind, char)>,
    pub register: Register,                // renamed from VimRegister; `pub type VimRegister = Register`
    pub cmdline: Option<CmdLineState>,
    pub incsearch: Option<IncsearchSession>,
    pub ex_history: Vec<String>,
    pub search_history: Vec<String>,
}

impl ModalCore {
    /// Clears `count`, `pending_replace`, `pending_find`, `pending_text_object`.  Each handler's
    /// own `reset_pending` calls this, then clears its own prefix fields.
    pub fn reset_pending(&mut self);
    /// Moved from `VimState` with its private `history_for` helper; same capping rule.
    pub fn record_command(&mut self, kind: CmdLineKind, cmd: &str);
}

// input/modal_core/session.rs (commit 1)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalInput { Vim(VimState) }  // commit 4a adds Helix(HelixState), unboxed: both variants
                                       // carry a ModalCore, so their sizes stay close.  Box one
                                       // only if clippy's large_enum_variant fires

/// What the badge color and the cursor style key on; replaces label-string matching in status_bar.rs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeFace { Normal, Insert, Select }

/// The status-bar badge: the handler's label plus the face that colors it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeBadge { pub label: &'static str, pub face: ModeFace }

impl ModalInput {
    pub fn for_handler(handler: &str) -> Option<Self>;   // None for "default" and unknown names
    pub fn is_modal_handler(handler: &str) -> bool;      // the one "is a modal handler configured?" test
    pub fn handler(&self) -> &'static str;
    pub fn feed(&mut self, ed: &mut EditorState, key: KeyEvent, vh: usize, vw: usize)
        -> VimOutcome;                                   // KeyOutcome from commit 3
    pub fn face(&self) -> ModeFace;                      // OperatorPending folds into Normal
    pub fn badge(&self) -> ModeBadge;                    // NORMAL… / NOR INS SEL
    pub fn is_insert(&self) -> bool;
    pub fn visual_kind(&self) -> Option<VisualKind>;     // always None under Helix (half-open)
    pub fn is_visual_line(&self) -> bool;
    pub fn cmdline(&self) -> Option<&CmdLineState>;
    pub fn cmdline_mut(&mut self) -> Option<&mut CmdLineState>;
    pub fn cmdline_live_update(&mut self, ed: &mut EditorState, before: &str, vh: usize, vw: usize);
    pub fn leave_select(&mut self);                      // after the App's Visual Cut / Paste
    pub fn reset_for_diff_entry(&mut self, ed: &mut EditorState);
    #[cfg(test)] pub fn vim(&self) -> Option<&VimState>; // and vim_mut; test-only, so no
                                                         // production code reaches into VimState
    // commit 3
    pub fn core(&self) -> &ModalCore;
    pub fn core_mut(&mut self) -> &mut ModalCore;
    // commit 4b
    pub fn on_search_landed(&mut self, ed: &mut EditorState); // no-op for vim; Helix selects the match
}

// input/modal_core/outcome.rs (commit 3) — renamed VimOutcome, plus the force flag on Quit
pub enum KeyOutcome {
    Pending, Consumed, Passthrough,
    EnterSearch { forward: bool, query: String },
    Save,
    Quit { save_first: bool, force: bool },
    SaveAs { path: Option<PathBuf>, then_quit: bool, force: bool },
    SaveCopy { path: PathBuf, then_quit: bool, force: bool },
    Flash(String),
}

// editor/vim_ops/guard.rs (commit 2)
pub enum Refusal { Table(TableBreak), TablePaste, TableStructural, PasteTooLarge }
impl Refusal { pub fn message(&self) -> &'static str; }

/// Which side of `span` a paste lands on.  Charwise: at `span.end` (After) or `span.start`
/// (Before).  Linewise: below `span`'s last line (After) or above its first line (Before).
pub enum PasteAt { Before, After }

/// `Text::linewise` is the shape the payload *lands as*: the target selection's shape, not
/// the register's (see `paste_over_range_refusal`).  The caller has already reconciled the
/// text with it (vim adds the `\n` a charwise register needs over a VisualLine span).
pub enum ReplaceWith { Char(char), ToggleCase, Upper, Lower, Text { text: String, linewise: bool } }

/// `table_unit`: when `range` is `Lines { first, last }` with `first == last` on a table row
/// and `op` is not `Yank`, take the structural table path (`Delete` → `delete_table_row`,
/// `Change` → `clear_table_cell`) instead of the plain linewise one.  Ignored otherwise.
/// `vh` / `vw` are the viewport size `delete_table_row` needs.
pub fn apply_operator(ed: &mut EditorState, op: Operator, range: OpRange, table_unit: bool,
    vh: usize, vw: usize) -> Result<OpResult, Refusal>;
/// Returns the char span of the inserted text.
pub fn paste_at(ed: &mut EditorState, text: &str, linewise: bool, count: u32,
    span: Range<usize>, at: PasteAt) -> Result<Range<usize>, Refusal>;
/// Returns the char span of the written text (`range` itself, except for `Text`).
pub fn replace_span(ed: &mut EditorState, range: Range<usize>, with: ReplaceWith)
    -> Result<Range<usize>, Refusal>;
/// `first..=last` are line indices; a one-line span joins with the line below.
pub fn join_lines_span(ed: &mut EditorState, first: usize, last: usize) -> Result<(), Refusal>;
pub fn indent_span(ed: &mut EditorState, first: usize, last: usize, right: bool,
    list_item: bool) -> Result<(), Refusal>;

// input/helix/state.rs (commit 4a)
#[derive(Debug, Clone, Default, PartialEq, Eq)]   // ModalInput derives PartialEq, Eq, so
pub struct HelixState {                           // HelixMode and HelixMotion must too
    pub core: ModalCore,
    pub mode: HelixMode,        // Normal | Insert | Select
    pub insert_appended: bool,  // Insert was entered with `a`/`A`; see Esc in the Key table
    pub pending_g: bool,
    pub pending_match: bool,    // after `m`
    pub last_motion: Option<HelixMotion>, // for `Alt-.`; v1 records only f/t/F/T
}
```

`App` holds `modal: Option<ModalInput>` and `parked_modal: Option<ModalInput>` in place of `vim` / `parked_vim`. `None` is the default handler.

## Selection model

Helix itself stores a selection as a half-open `Range { anchor, head }`: both ends are positions *between* characters, and the cursor is drawn on the grapheme before `head` when `head > anchor`, or on the grapheme at `head` otherwise. edamame's default handler, mouse and clipboard store `Selection { anchor, active }` the same half-open way. Helix therefore stores its selections in that convention, so every selection, whoever wrote it, means the same thing under Helix. vim Visual keeps its inclusive model; the two handlers never share a session (Decision 11), so they never read each other's selections.

- **Storage.** `editor.selection = Some(Selection { anchor, active: head })`, half-open, with `editor.cursor.offset` on the *head grapheme*: `prev_grapheme(active)` when `active > anchor`, `active` when `active < anchor`. `helix_ops::select::head_cursor(sel, buf)` computes it, and Helix motions start from it. So `cursor.offset == selection.active` holds for a backward selection but not a forward one. That is the one place Helix departs from the default handler; see Risk 3.
- **Collapsed.** A selection of one grapheme or none (the Helix "cursor") is stored as `selection = None`, with the cursor on that grapheme. The block cursor already paints it, and every verb treats `None` as `cursor .. next_grapheme(cursor)`. This avoids a painted highlight under the cursor on every Normal-mode frame. A collapsed selection has no direction, so `Alt-;` on it does nothing.
- **Writers.** `helix_ops::select::set_range(ed, anchor, head)` is the only Helix writer of `selection` and `cursor`. It clamps both ends to grapheme boundaries and the buffer, stores `None` when the range spans at most one grapheme, and sets the cursor to the head grapheme. `put_cursor(ed, target, extend)`, built on it, is a port of Helix's `Range::put_cursor`:
  - Without `extend` (Normal), it collapses onto `target`.
  - With `extend` (Select), the anchor stays, except that when the head crosses to the other side of the anchor, the anchor moves one grapheme (`next_grapheme(anchor)` when a forward range turns backward, `prev_grapheme(anchor)` when a backward one turns forward), so the anchor's own grapheme stays selected. The head becomes `next_grapheme(target)` when `target >= anchor`, else `target`.

  Motions that produce a whole range (`w e b W E B`, `f t F T`, text objects, `x`, `%`) call `set_range` in Normal mode and `put_cursor(range's head grapheme, extend: true)` in Select mode. Motions that produce a position (`h j k l`, `gg ge gh gl gs`, `mm`) call `put_cursor(target, extend: mode == Select)`.
- **Sticky column.** `set_range` recomputes `cursor.preferred_col` only when it changes `cursor.offset`. `j` / `k` move the cursor first (see the Key table) and then call `put_cursor` on the cursor's own offset, so the column the vertical move preserved survives a run of `j` / `k`.
- **Adopting other writers' selections.** The mouse, Shift+arrows in Insert, the clipboard actions and App-level cursor moves write `selection` and `cursor` without Helix. Their selections are already half-open, so only the cursor can disagree. At the start of every `helix_feed` call outside Insert, `helix_ops::select::adopt(ed)` settles it:
  1. selection spanning at most one grapheme: collapse, `selection = None`, cursor on that grapheme (`min(anchor, active)`). Checked first, so a one-grapheme forward drag (whose `cursor == active`) is not kept by case 3;
  2. selection `Some` and the cursor on the head grapheme: consistent, nothing to do;
  3. selection `Some` and `cursor.offset == active` (a forward selection from a default-convention writer, such as a mouse drag): keep the selection and move the cursor to the head grapheme;
  4. selection `Some` and the cursor anywhere else (an external cursor move left a stale selection): collapse, `selection = None`, cursor left where the external move put it.

  Until that next key, a forward mouse selection draws its cursor one grapheme past the selection, just as it does under the default handler. App paths that move the cursor *and* mean to select (search landing) call `on_search_landed`, which writes through `set_range` so `adopt` finds the selection consistent.
- **`visual_kind()` is `None` under Helix**, always. Rendering (`rendered_view.rs`, `raw_view.rs`) paints the stored half-open span as it does for the default handler, and Ctrl-C / Ctrl-X / Ctrl-V skip `dispatch_visual_clipboard` and go to `edit_ops::apply`, which acts on the stored selection, or inserts at the cursor when there is none, exactly as under the default handler. After Ctrl-X / Ctrl-V consume a selection, Helix stays in its current mode with `selection = None`; Select mode with a collapsed selection is valid in Helix. `is_visual_line()` is `false`, so the hint line shows the ordinary selection chords. No renderer, clipboard-routing or hint-line change.
- **Linewise is derived from shape, not stored.** `helix_ops::select::line_span(sel, buf) -> Option<(usize, usize)>` returns `Some((first, last))` when the selection is non-empty, starts at a line start, and ends at a line start (so it includes its last line's `\n`). A selection ending at the end of a buffer with no trailing newline is `None`. `first` is the start's line; `last` is the line of `prev_grapheme(end)`. `d` and `y` pass such a selection as `OpRange::Lines { first, last }`, with `table_unit: first == last` for `d`, so `x d` on a table row removes it structurally and a linewise delete in an ordered list gets the composed renumber. Because the selection ends at a line start, vim's linewise span (`content_start .. block_end`) is exactly the selection, so the deleted text and the register text match what real Helix produces. `c` always passes `OpRange::Chars(lo..hi)`: Helix's `x c` deletes the line's newline too, and vim's linewise Change would keep an empty line. Any other selection goes as `OpRange::Chars(lo..hi)`. A mouse drag that happens to cover whole lines is linewise too, which is what deleting whole table rows needs. There is no flag to go stale.
- **The last line without a trailing newline** is charwise by the rule above, as in Helix: `x y` there yanks the text without a `\n` (so `p` does not open a line), and `x d` leaves the preceding newline and an empty last line. vim's linewise path would add a `\n` to the register and consume the preceding newline, which is why it is not used there.
- **Register linewise-ness** follows Helix's rule: a yank or delete stores `register.linewise = text.ends_with('\n')`, so `p` opens a line exactly when the text is line-terminated. With `line_span` as above, this agrees with `OpResult::linewise` on every `Lines` path.
- **After `d`**, Helix collapses onto the deletion point: `set_range(lo, lo)`, clamped to the buffer, replacing the landing vim's operator chose (first non-blank for a linewise delete).
- **`x` / `X`** select whole lines half-open: `line_start(first) .. line_start(last + 1)`, or `.. len` on the last line. A half-open range takes the trailing `\n` naturally, with none of vim Visual's newline widening.
- **Table cells:** Helix motions inside a rendered table clamp the head with `CellLimit::Append`, so the exclusive end may sit on the cell's append slot while the head grapheme, and so the drawn cursor, stays on the cell's last character. The anchor enters through vim's existing cell pull-in, so a selection never *starts* on the append slot.

Undo and redo restore a selection derived from the replayed step (see commit 4b), not stored in history. Every span the shared code returns (`paste_at`, `replace_span`, `History::last_replayed`, a search match) is already half-open and goes to `set_range` unchanged.

## Key table

v1 subset, with each key's Helix semantics and the code it reuses. "Sel" means the current selection as a half-open char range `lo..hi`: the stored selection, or `cursor .. next_grapheme(cursor)` when `None`. Counts apply wherever vim applies them, clamped by `COUNT_CAP`.

| Key | Helix behavior | Implementation |
|---|---|---|
| `h` `j` `k` `l`, arrows | Move the head; Normal collapses to one grapheme, Select extends | `modal_core::keys::move_hjkl` (vim's `feed_hjkl`, moved in commit 3; table-aware) moves `cursor` directly, starting from the head grapheme the cursor already sits on. Helix then reads `cursor.offset` as the target and calls `put_cursor(target, extend)`. `move_hjkl` neither reads nor clears `selection`, so the anchor survives the move |
| `w` `e` `b`, `W` `E` `B` | Select the traversed span (word start / word end / previous word start; long-word variants) | New `helix_ops::motion::word_range` (half-open, as Helix returns it), built on `document::graphemes::class_at`; cell-scoped via `cell_scope_at`; then `set_range` (Normal) or `put_cursor` (Select) |
| `f` `t` `F` `T` `{c}` | Select from the head up to / onto the next or previous `c`, across lines. No match: nothing changes (selection and mode untouched), but `last_find` is still recorded | New public `vim_ops::motion::find_char_target(buf, cursor, target, kind, count, skip_adjacent, scope: FindScope) -> Option<usize>`, `None` on a miss. The private `find_char` becomes `find_char_target(…, FindScope::Line).unwrap_or(cursor)`, so vim is unchanged. Helix passes `FindScope::Buffer`; the range runs from the head grapheme to the target, half-open |
| `Alt-.` | Repeat the last `f`/`t` motion | `find_char_target` with `skip_adjacent: true` and `FindScope::Buffer`, from `core.last_find` |
| `gg` / `{n}gg`, `ge`, `gh`, `gl`, `gs` | Document start / line n, last line, line start, line end, first non-blank | `resolve_scoped_motion` (`DocStart`/`GoToLine`, `DocEnd`, `LineStart`, `LineEnd`, `LineFirstNonBlank`), then `put_cursor`. vim's `DocStart` / `GoToLine` / `DocEnd` land on the first non-blank, and `DocEnd` uses `last_content_line`. The commit-4a spike records Helix's `gg`, `5gg` and `ge` landings on lines with leading whitespace and with a trailing empty line. If Helix differs, Helix computes the target line with `goto_line_index` / its own last-line rule and lands where the spike shows (for example `line_to_char(line)`) instead of using these motions |
| `mm` | Matching bracket | `Motion::MatchingPair` via `resolve_scoped_motion`, then `put_cursor` |
| `mi{o}` / `ma{o}` | Select inside / around a text object | Shared `text_object_for` + `resolve_text_object_range`; objects `w W ( ) b [ ] { } B " ' \``, exactly the set vim supports today. Paragraph (`p`) and angle-bracket (`<` `>`) objects are out of scope |
| `x` | Select the current line; when Sel is already whole lines ending on the head's line, extend one line down | `helix_ops::select_lines` (see Selection model) |
| `X` | Extend Sel to whole-line bounds | `helix_ops::select_lines` |
| `%` | Select the whole document | `set_range(0, len)` |
| `;` | Collapse to the head | `selection = None`; the cursor stays on the head grapheme |
| `Alt-;` | Flip anchor and head | `set_range(head, anchor)`; the cursor moves to the other end |
| `v` | Toggle Select mode | `HelixMode::Select` |
| `d` / `c` / `y` | Delete / change / yank Sel; `c` enters Insert; `y` keeps the selection; `d` collapses onto the deletion point | `guard::apply_operator`: `d` and `y` with `OpRange::Lines` when `line_span` is `Some` (`table_unit: first == last` for `d`, `false` for `y`), else `OpRange::Chars(Sel)`; `c` always `OpRange::Chars(Sel)` with `table_unit: false`. Register `linewise = text.ends_with('\n')`. See Selection model |
| `p` / `P` | Paste the register after / before Sel; selects the pasted text. An empty register is a no-op | `guard::paste_at(…, span = Sel, PasteAt::After / Before)`, then `set_range` over the returned span. The empty-register check runs before `paste_at`, as in vim's `paste_register` |
| `R` | Replace Sel with the register; selects the replacement | `guard::replace_span(ReplaceWith::Text { text: register.text, linewise: line_span(Sel).is_some() })` (vim Visual `p`'s path), then `set_range` over the returned span. Helix adds no `\n`: `R` replaces exactly the selected text |
| `r{c}` | Replace every grapheme in Sel with `c` | `guard::replace_span(ReplaceWith::Char)` |
| `~` | Toggle case of Sel | `guard::replace_span(ReplaceWith::ToggleCase)` |
| `J` | Join the lines Sel touches; a one-line Sel joins with the line below | `guard::join_lines_span` |
| `>` / `<` | Indent / outdent the lines Sel touches. A one-line Sel on a list item nests / un-nests the item structurally; anything else shifts the lines by spaces | `guard::indent_span(…, list_item: true)` |
| `i` / `a` | Insert at Sel's start / end (`lo` / `hi`); clears the selection | Shared `enter_insert` after placing the cursor and setting `selection = None`; `a` sets `insert_appended` |
| `I` / `A` | Insert at the line's first non-blank / end (cell start / end in a table); clears the selection | Shared `enter_insert` with `cell_scope_at`; `A` sets `insert_appended` |
| `o` / `O` | Open a line below / above (table row, list item); clears the selection | Shared `open_line` (`open_table_row` → `open_list_continue` → newline) |
| `u` / `U` | Undo / redo, then select the restored text | `edit_ops::apply(Undo/Redo)` + `History::last_replayed`, then `set_range` |
| `/` `?` | Search forward / backward with incsearch; selects the match | Shared `start_cmdline` + `feed_cmdline` → `KeyOutcome::EnterSearch` → `App::enter_modal_search` → `ModalInput::on_search_landed` (commit 4b) |
| `n` / `N` | Select the next / previous match (Select mode extends) | Shared `search_repeat`, then `on_search_landed` |
| `:` | Command prompt | Shared `start_cmdline(Ex)` + `submit_ex` / `parse_ex` |
| `Esc` | Insert → Normal; Select → Normal, keeping the selection (as Helix does); Normal → clear pending count / prefix | Helix arms for all three; see below |

Entering Insert always clears the selection first: the default handler runs Insert, and it replaces a live selection with the first typed character. `c` deletes Sel through `apply_operator` before entering Insert, so it leaves no selection either.

`Esc` from Insert does not use vim's Insert-Esc, which steps the cursor back one grapheme. Helix leaves the cursor where it is after `i`, `I`, `o`, `O` and `c`, and steps back one grapheme only after `a` / `A`, so the cursor lands on the last typed character (never across a line start). Helix's own Insert-Esc arm does this from `insert_appended`, clears the flag, and leaves `selection = None`. Confirm both landings against real Helix in the commit-4a spike session and add them to the golden table.

Helix claims `Alt-;` and `Alt-.` in Normal and Select before the shared passthrough-chord check, so a global binding on either is shadowed there; the user manual says so. Every other Ctrl / Alt / Super chord passes through as it does under vim.

### `:` commands

Everything `parse_ex` accepts today keeps its meaning. The table lists only the forms that change, and the `ExCommand` each parses to:

| Input | Parses to | Today | Notes |
|---|---|---|---|
| `:q!`, `:quit!` | `QuitForce` | `UnknownCommand` | New variant, in `parse_ex`'s exact-match table. |
| `:wq!` | `WriteQuit { force: true }` | `WriteQuit` (the `("wq", None)` arm of `parse_write_forms` drops the `!`) | `WriteQuit` becomes a struct variant; `:wq` is `force: false`. |
| `:x!`, `:xit!` | `WriteQuitIfModified { force: true }` | `UnknownCommand` | Same shape as `:wq!`. |
| `:wq! <path>` | `WriteQuitCopy { path, force: true }` | Same | Unchanged: `force` there already means "skip the overwrite prompt". |
| `:write-quit[!]` | `WriteQuit { force }` | `UnknownCommand` | Helix alias (commit 4b). Add `"write-quit"` next to `"wq"` in both `parse_write_forms` arms, so `:write-quit <path>` → `WriteQuitCopy` as well. |
| `:write`, `:quit` | — | Already accepted | Listed only because Helix users type them; no change. |

On a quit, `force` means "skip the dirty-buffer confirm". Only `QuitForce` needs it: after a successful `:wq` / `:x` the buffer is clean, so `force: true` changes nothing there, and a failed write must still keep the user in the editor. The App's save-then-quit path already reaches the dirty guard when the save fails. All of this applies under vim too, since the parser is shared. `:'<,'>` ranges are vim-only and never pre-filled under Helix.

## Commit series

Each commit passes `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --no-fail-fast`, and `cargo doc --no-deps --document-private-items` with no new warnings. Commits 1 and 3 also run the Windows cross-clippy from AGENTS.md, since they move test modules and imports. Each commit is a checkpoint: stop, review the diff, and update this plan's Status line before starting the next.

### Commit 1 — `refactor(input): replace App's vim option with a ModalInput enum`

**Done.** One intended behavior change (below). About 550 lines added, mostly mechanical.

As built:

- `src/input/modal_core.rs` + `modal_core/session.rs`: `ModalInput` (one `Vim` variant), `ModeFace`, `ModeBadge`, and the commit-1 accessors in [Key types](#key-types). The file is `session.rs`, not `state.rs`, which commit 3 uses for `ModalCore`.
- Names that differ from the original draft: `for_handler` (it returns `Option`, so not a `from_*` conversion), `badge() -> ModeBadge` (one value instead of a label plus a separate face), and `ModeFace::Select` (neutral between vim Visual and Helix Select).
- `App::set_modal_handler(&str)` is the one handler switch; `set_vim_enabled(bool)` wraps it, and the settings row's `apply_live_update` already calls `set_modal_handler`. **Behavior change:** re-selecting the live handler keeps its session even when it is parked behind a read-only document; before, enabling vim while reading replaced the parked session with a fresh one. Covered by `re_enabling_vim_while_reading_keeps_the_parked_session`.
- `leave_preview_under_modal` checks `ModalInput::is_modal_handler`, so a `modal.handler` name no handler claims (such as `"Vim"`) behaves as the default handler, as it did before: no session, and Preview stays. Covered by `an_unknown_handler_name_behaves_as_the_default_handler`.
- The App never touches `VimState` outside tests: Visual Cut / Paste ends with `leave_select`, `enter_diff_mode` calls `reset_for_diff_entry`, and `paste_into_cmdline` uses `cmdline_mut` + `cmdline_live_update`. `App::vim()` / `vim_mut()` are `#[cfg(test)]`. One reach into `input::vim` remains: `paste_into_cmdline` calls `vim::cmdline::paste_str`, which moves to `modal_core` in commit 3. (`enter_diff_mode`'s `vim_ops::clear_substitute_preview` is editor-layer and handler-agnostic, so it stays.)
- `HintCtx::vim_enabled` and the `vim_enabled` parameters of the command palette and keybinds overlay are renamed to `modal_enabled`.
- Verified: fmt, clippy (Linux, plus both Windows cross-clippy passes), `cargo doc` with no warnings, `cargo test --no-fail-fast` all green.

Original scope:

- Add `src/input/modal_core.rs` and `src/input/modal_core/session.rs` with `ModalInput` (one `Vim` variant for now), `ModeFace`, and the accessors above implemented over `VimState`. `ModalCore` does not exist yet; `core()` arrives in commit 3.
- `App`: `vim` → `modal`, `parked_vim` → `parked_modal`. `set_vim_enabled(bool)` becomes `set_modal_handler(&str)` (keep `set_vim_enabled` as a thin wrapper until commit 4). `sync_vim_suspension` → `sync_modal_suspension`, same logic over the whole enum. `leave_preview_under_vim` → `leave_preview_under_modal`, true for any modal handler.
- Replace every `self.vim` read with an accessor: badge and cursor style in `event_loop.rs`'s render path, the non-Insert paste gate in `dispatch_paste`, `paste_into_cmdline`, the batch-coalescing guard in `dispatch_key_batch` (`!is_insert()`), the prompt-paste chord and the intercept in `dispatch_single_key`, `open_command_palette` / `open_keybinds_overlay` (`modal.is_some()`), the Visual clipboard routing and its Cut/Paste reset in `actions.rs`, `enter_diff_mode`'s cleanup (`reset_for_diff_entry`), `hint_content` in `flash.rs`, `nav.rs`'s park comment, `settings.rs`, `welcome.rs`.
- `status_bar.rs`: `vim_mode_label: Option<&str>` → `mode_badge: Option<ModeBadge>`; `vim_badge_style` → `mode_face_style`, matching `ModeFace` instead of label strings. `cursor_style.rs` takes `Option<ModeFace>`; `OperatorPending` folds into `Normal` inside `face()`. `HintCtx::vim_enabled` → `modal_enabled`.
- Tests: update in place. Add test-only `App::vim()` / `vim_mut()` helpers so the ~40 `app.vim` test references stay one-liners.

### Commit 2 — `refactor(vim): move the table and paste guards into the editor layer`

No intended behavior change. About 500 lines plus guard unit tests. The riskiest commit for vim: `tests/vim.rs` is the regression net and must pass unchanged, so run `cargo test --test vim --no-fail-fast` after each extraction rather than only at the end.

- New `src/editor/vim_ops/guard.rs`, re-exported from `vim_ops.rs`. Move into it from `input/vim/feed.rs`: `mutating_table_break`, `paste_over_range_refusal`, `table_doubled_operator`, and the refusal strings (`TABLE_STRUCTURAL_REFUSAL`, `TABLE_PASTE_REFUSAL`), which become `Refusal` variants.
- Move `ensure_editing` from `input/vim/feed.rs` to the editor layer as `EditorState::ensure_editing(&mut self)` (Preview → Rendered; Raw untouched), since it touches only `editor.mode`. Input-layer callers on unguarded paths (`enter_insert`, `enter_visual`, `feed_hjkl`, …) call the method instead.
- Guarded entry points, with the signatures in [Key types](#key-types). Each runs its guard before the first write, then calls `ed.ensure_editing()`, then writes. A refusal therefore changes nothing, the mode included, as `paste_register` does today ("Refused before anything changes, the switch out of Preview included"). Callers never call `ensure_editing` before a guarded function. Each entry point is one undo step: it either writes once or folds its follow-up renumber in with `History::push_composed` (see [Background](#background)). The callers' `break_group` barriers (in `vim_feed` today, in `with_command_barriers` after commit 3) keep it from merging with its neighbors.
  - `apply_operator`: guard (skipped for `Yank`). Then, when `table_unit` is set, `range` is `Lines { first, last }` with `first == last`, and `op` is not `Yank`: place the cursor at `line_to_char(first)` and run today's `table_doubled_operator` body (`delete_table_row(ed, vh, vw)` / `clear_table_cell`). `Applied(res)` returns `Ok(res)`; `Refused(reason)` returns `Err(Refusal::Table(reason))`; `NotATable` falls through. Otherwise `execute_operator`, plus the linewise renumber that `fold_op_result` runs today. `fold_op_result` keeps only the register store and the mode change. vim passes `table_unit: true` only from the doubled-operator branch of `feed_operator_pending` with `count == 1` (today's `table_doubled_operator` call site). Every other vim caller (`Ndd`, `dj`, `Vd`, motions) passes `false`, so vim's behavior is unchanged.
  - `paste_at`: an empty `text` returns `Ok(span.start .. span.start)` without writing. Then the `repeat_for_paste` cap (`Refusal::PasteTooLarge`). `span` is half-open for both handlers. `paste_at` computes the landing from `span`, not from the cursor:
    - Charwise: insert at `span.end` (After) or `span.start` (Before). An empty `span` inserts at `span.start` either way.
    - Linewise: the anchor line is the line of `prev_grapheme(span.end)` for After with a non-empty `span`, otherwise the line of `span.start`. The text goes below the anchor line (After) or above it (Before).
    - Table: place the cursor on the anchor (After: `prev_grapheme(span.end)`, or `span.start` when `span` is empty; Before: `span.start`) and ask `table_paste_plan` with the unrepeated register text (it stays cursor-based): `Refused` → `Refusal::TablePaste`; `RowsAt(at)` → `insert_table_rows`; `NotATable` → the charwise / linewise paths above.

    Refactor the private `edits::paste_charwise` / `paste_linewise` into `paste_charwise_at(ed, text, insert_at)` and `paste_linewise_at(ed, text, line, after)`. They keep vim's landings: the cursor on the last inserted glyph for charwise, and the first non-blank of the first pasted line for linewise. `edits::paste(ed, text, linewise, after)` becomes a wrapper that derives the old cursor-based arguments. `paste_charwise_at`, `paste_linewise_at` and `insert_table_rows` return the inserted char span. For a linewise paste below the last line, whose payload is `"\n{body}"`, the span covers only the body: `len + 1 .. len + payload chars`. So the selection starts at a line start and `line_span` reads it as linewise. `paste_at` passes the span back. A follow-up renumber that widens a marker (`9.` → `10.`) can leave the span's end one char short. `helix_ops::select::set_range` clamps it to grapheme boundaries, which is acceptable for a selection.

    vim's `paste_register` passes `span = cursor .. after_point`, where `after_point` is today's rule from `paste_charwise`: `next_grapheme(cursor)` when `cursor < line_end_offset(line)`, else `cursor`. It passes `PasteAt::After` / `Before` for `p` / `P`. On an empty line or at a line end the span is empty, so a charwise `p` inserts at the cursor, exactly as today. Helix passes Sel unchanged, so a charwise `p` after an `x` selection inserts at the start of the next line (`span.end`), not before the selection's `\n`.
  - `join_lines_span`: extracted from `edits::join_lines(count)`, with the `lines_touch_a_table` refusal (`Refusal::TableStructural`) moved inside. `join_lines(count)` becomes a wrapper; `run_visual_join` loses its move-the-cursor-first workaround.
  - `indent_span`: first the `lines_touch_a_table` refusal (`Refusal::TableStructural`). Then, when `list_item && first == last`, it places the cursor on line `first` and tries `indent_list_item`. If that is skipped or declines, it runs `indent_lines` over `first..=last`. vim's `>>` / `<<` pass `list_item: count == 1` (today's rule in `feed_indent_pending`). vim Visual `>` / `<` pass `list_item: false`, because `run_visual_indent` calls only `indent_lines` today. Helix passes `true`.
  - `replace_span`:
    - `Char`, `ToggleCase`, `Upper`, `Lower`: `op_range_breaks_a_table(OpRange::Chars(range))` (`Refusal::Table`), then `replace_char_range` / `toggle_case_range` / `set_case_range`. Returns `range`.
    - `Text { text, linewise }`: `paste_over_range_refusal(ed, &range, &text, linewise)`, both halves. `range_breaks_a_table` becomes `Refusal::Table`; `table_paste_plan` → `Refused` becomes `Refusal::TablePaste`. Then `replace_range_with`, then `renumber_list_at_cursor` when `text` contains `\n`, folded into the same step (moved here from `run_visual_paste`). Returns `range.start .. range.start + text chars`, with the same renumber caveat as `paste_at`.
    - vim's `run_visual_paste` keeps the newline reconciliation (a charwise register over a VisualLine span gets a trailing `\n`) and its empty-register early return, then passes `linewise: line_mode`.
- vim's `run_operator`, `run_visual_operator`, `run_visual_*`, `feed_visual_replace_char`, `feed_indent_pending`, `paste_register` and `run_visual_paste` shrink to: build the range, call the guarded function, flash a `Refusal`, change mode. None of them calls `ensure_editing` before the guarded function.
- `table.rs`: make `cell_scope_at(state, offset)` public.
- `motion.rs`: add `FindScope::{Line, Buffer}` and the public `find_char_target(…) -> Option<usize>` (see the Key table); `find_char` and `resolve_find_repeat` wrap it with `FindScope::Line` and `unwrap_or(cursor)`, so every existing caller is unchanged.
- Tests: unit tests in `guard.rs` for:
  - every entry point against a header row, the alignment row, a cell, a whole-table range, and an oversized paste;
  - one undo step per call, and the renumber fold;
  - a refusal from Preview leaves `mode == Preview`;
  - `paste_at`'s returned span for charwise, linewise, last-line linewise (body only, without the leading `\n`), table-row and empty-text pastes;
  - a charwise After paste whose `span` ends just past a `\n` inserting at `span.end`;
  - vim's empty `span` at a line end inserting at the cursor;
  - `replace_span(Text)` refusing a linewise row register into a charwise in-cell range and a charwise register over a whole row;
  - `apply_operator` with `table_unit` true and false on a single table row;
  - `indent_span` on a one-line list item with `list_item` true and false;
  - `find_char_target` returning `None` on a miss and `Some(cursor)` for an adjacent `t`.
- Docs: `docs/dev/vim-tables.md` points at `vim_ops::guard` as the only place the guards run.

### Commit 3 — `refactor(input): extract the shared modal core from vim`

Behavior change limited to force quit. About 1,000 lines moved, roughly +100 net.

- `src/input/modal_core/` gains `outcome.rs` (`KeyOutcome`; keep `pub use KeyOutcome as VimOutcome` so `tests/vim.rs`'s imports are untouched), `cmdline.rs` (moved from `input/vim/cmdline.rs`, joined by `CmdLineKind`, `CmdLineState` and `HISTORY_CAP` from `input/vim/state.rs`), `prompt.rs`, `keys.rs`, and `ModalCore` in `state.rs`.
- Move `VimState`'s handler-agnostic fields into `core: ModalCore` (see Key types). `VimState` keeps `sub_mode`, `pending_op`, `motion_count`, `pending_g`, `visual_anchor`, and `last_visual_range`, which only vim's `:` from Visual ever sets. Mechanical `vim.x` → `vim.core.x` across `feed.rs` and `tests/vim.rs`; no `Deref` (it would hide the disjoint-field borrows `feed_cmdline` relies on).
- Split `VimState`'s methods: `record_command` and its private `history_for` move to `ModalCore`. `ModalCore::reset_pending` clears the core's pending fields, and `VimState::reset_pending` calls it, then clears `pending_op`, `motion_count` and `pending_g`.
- Move to `modal_core`, changing `&mut VimState` parameters to `&mut ModalCore` plus a returned or passed mode transition where needed:
  - `prompt.rs`: `feed_cmdline`, `cmdline_live_update`, `submit_ex`, `start_cmdline`. `last_visual_range` is not in `ModalCore`. `submit_ex` and `update_substitute_preview` already take it as a parameter, and today `feed_cmdline` (`feed.rs:228`) and `cmdline_live_update` (`feed.rs:251`) read it from `vim`. Once moved, both take `visual_range: Option<(usize, usize)>` instead: vim passes `vim.last_visual_range`, Helix `None` (`execute_substitute` already falls back to the current line). `ModalInput::cmdline_live_update`, `cmdline`, `cmdline_mut` and `reset_for_diff_entry` then read `core()` instead of matching per variant. `App::paste_into_cmdline` switches from `input::vim::cmdline::paste_str` to the moved `modal_core::cmdline::paste_str`, its last reach into `input::vim`.
  - `keys.rs`: `is_count_digit` / `accumulate` / `count_of` (as `ModalCore` methods), `is_passthrough_chord`, `text_object_for`, `find_kind_for`, `reverse_find`.
  - Shared editing helpers: `enter_insert`, `open_line`, `after_move`, `after_edit`, `move_to_offset`, `feed_hjkl` (renamed `move_hjkl`: it moves `cursor` and touches nothing else, so Helix can read the target back; see the Key table), `search_repeat`, `search_word_outcome`, and the cores of `feed_find_char` / `feed_replace_char` (resolve a target; each handler decides cursor vs. selection).
  - The command-barrier prelude of `vim_feed` becomes `with_command_barriers(ed, is_command, f)`, used by both handlers.
- `ModalInput::feed` already dispatches to `vim_feed` and `dispatch_single_key` already calls `modal.feed(...)` (commit 1); its outcome arms change only by the rename.
- Rename `App::enter_vim_search` (`app/search.rs`) → `enter_modal_search`.
- Force quit, per the [`:` commands](#-commands) table: add `ExCommand::QuitForce`; `WriteQuit` and `WriteQuitIfModified` gain a `force` field; `submit_ex` maps all three to `KeyOutcome::Quit { save_first, force }`. In `dispatch_single_key`'s `Quit` arm, `force && !save_first` sets `self.should_quit = true` directly. Every other combination keeps today's path: save first if asked, then dispatch `Action::Quit`, whose `Action::Quit && self.editor.dirty` check in `dispatch_action` opens `open_quit_confirm`. That way a failed `:wq!` write still stops at the guard. `:q!` cannot arrive during diff review or a capturing search flow, because `dispatch_single_key` skips the modal handler while `modal_deferred` is set. There is no existing force-quit action to reuse (`Action::Quit` always guards). Document `:q!`, `:wq!` and `:x!` in `docs/vim-mode.md` and add a CHANGELOG entry.
- Tests: unit tests move with their code; `tests/vim.rs` changes only by the field rename; `parse_ex` tests for every row of the `:` table; App tests: `:q!` on a dirty buffer quits with no modal pushed, bare `:q` on a dirty buffer still opens the quit confirm, `:wq!` on a dirty buffer saves then quits, and `:wq!` whose save fails stays open behind the quit confirm.
- Docs: `docs/dev/vim-cmdline.md` and `docs/dev/input.md` point at `modal_core`.

### Commit 4a — `feat(input): add Helix selection and motions`

The Helix handler with its state, motions and selection model, but not yet selectable: nothing in config, settings or the palette builds `ModalInput::Helix`, so tests construct it directly. About 800 lines.

Spike first: before anything else, write `helix_ops::motion::word_range` against a golden table of Helix's actual `w e b W E B` selections, recorded as Helix's own half-open `(anchor, head)` pairs (which this plan stores unchanged), taken from running Helix on fixed inputs (punctuation runs, leading and trailing whitespace, line ends, empty lines, CJK, combining marks). In the same session, record:
- the Insert-Esc landings after `i` and `a` (see the [Key table](#key-table));
- the `gg`, `5gg` and `ge` landings on lines with leading whitespace and with a trailing empty line (see the Key table's `gg` row);
- the `x d`, `x y` and `x c` results on a middle line and on a last line without a trailing newline, to confirm the [Selection model](#selection-model)'s routing. If Helix disagrees, change the routing and this plan before writing the verbs. Put the output of `hx --version` in the test module's doc comment so a later disagreement can be traced to a Helix release. This is the one sizable piece of logic Helix cannot share and the likeliest to be wrong. Commit the table as a test.

- `src/editor/helix_ops.rs` + `helix_ops/{select.rs, motion.rs}`:
  - `select.rs`, per the [Selection model](#selection-model): `set_range(ed, anchor, head)` (the sole writer of `selection` and `cursor`; grapheme and cell clamp; `None` for at most one grapheme), `put_cursor(ed, target, extend)`, `head_cursor(sel, buf) -> usize`, `adopt(ed)`, `line_span(sel, buf) -> Option<(usize, usize)>`, `collapse`, `flip`, `select_lines(ed, count, extend)` for `x` / `X`, `select_all`, `select_text_object`.
  - `motion.rs`: `word_range(buf, anchor, head, count, kind, big) -> (usize, usize)`, half-open.
- Audit the code that reads `cursor` and `selection` together, since a forward Helix selection has `cursor.offset != selection.active`. Each site must either work with that or not be reachable in Helix Normal / Select. Write the result into `docs/dev/modal-input.md`. The candidates, from a grep for files touching both:
  - `editor::mouse_ops::{selection, table_drag, coord}`: a Shift+click or drag extends from `selection.anchor` and writes `cursor = active`, which `adopt` fixes on the next key;
  - `edit_ops`' Shift+arrow `extend_selection`: unreachable, since `helix_feed` takes the arrow keys in Normal and Select whatever Shift says (only Ctrl / Alt / Super chords pass through);
  - the selection painting in `ui::rendered_view` and `ui::raw_view`;
  - `editor::table_edit_ops` and `editor::state`;
  - `app/actions.rs`' clipboard paths;
  - `ui::bottom_region`'s selection hints.
- `src/input/helix.rs` + `helix/{state.rs, feed.rs}`: `HelixState`, `HelixMode`, `HelixMotion`, and `helix_feed`, a `match` on mode and key in this order: prompt capture → Insert-Esc → `Alt-;` / `Alt-.` → passthrough chords → pending prefixes (`g`, `m`, `mi`/`ma`, `f`/`t`) → count digits → commands. This commit handles the motion and mode keys of the Key table (`h` through `v`, `i` `a` `I` `A` `o` `O`, `:` and `Esc`); the verb keys return `KeyOutcome::Consumed` until 4b. Outside Insert, every call starts with `adopt` and runs inside `with_command_barriers`; every motion ends in `set_range` or `put_cursor`.
- `ModalInput::Helix` variant and its arms: `face()` maps Normal / Insert / Select to `ModeFace::Normal` / `Insert` / `Select`; `badge()` labels are `NOR` / `INS` / `SEL`; `leave_select` returns Select to Normal; `reset_for_diff_entry` mirrors vim's; `visual_kind()` returns `None`.
- Tests: see [Testing](#testing), the items marked 4a.

### Commit 4b — `feat(input): add Helix verbs, undo selection and search landing`

About 600 lines.

- Verbs `d c y p P R r ~ J > <` per the Key table. Each takes Sel as stored (half-open, no widening), turns it into the shape its guarded function wants (`OpRange::Lines` via `line_span` or `OpRange::Chars` for `d y`, always `OpRange::Chars` for `c`; the char range for `R r ~`; the touched lines `line(lo) ..= line(prev_grapheme(hi))` for `J > <`), and calls a commit-2 guarded function; a `Refusal` becomes `KeyOutcome::Flash(refusal.message().to_owned())`. Pending prefix `r` joins the `helix_feed` order from 4a.
- Undo selection restore: `History::last_replayed(&self) -> Option<Range<usize>>`, recorded by `undo` / `redo`: the restored text's span after an undo (`offset .. offset + removed.chars().count()`), the inserted span after a redo (`offset .. offset + inserted.chars().count()`). A composed step (`push_composed`) yields its whole span, renumbered lines included. Helix `u` / `U` select that span, or collapse at its offset when it is empty. The direction of the pre-edit selection is not restored; the manual says so.
- Search landing: `ModalInput::on_search_landed(ed)` reads `ed.search`'s `focused_range()` (bytes; convert with `byte_to_char`) and, under Helix, selects it: `set_range` over the match in Normal mode, `put_cursor` onto the match's last grapheme with `extend: true` in Select mode. Three call sites:
  - the end of `App::enter_modal_search`, after `sync_cursor_to_search_focus` (covers `/` `?` `*` `#`);
  - `App::search_move_focus`, after its `sync_cursor_to_search_focus`, for a global-keymap next/prev during a navigate-only flow, which the modal handler never sees;
  - Helix's own `n` / `N`, after `search_repeat`.

  The other `sync_cursor_to_search_focus` calls in `app/search.rs` belong to the capturing replace flow. `modal_deferred` keeps the handler out of that flow, so they need no call.
- `parse_ex` alias `write-quit[!]` per the [`:` commands](#-commands) table.
- `tests/modal_parity.rs` (see [Testing](#testing)).

### Commit 4c — `feat(config): make Helix selectable and document it`

About 400 lines of source plus docs.

- `config/sections.rs`: `HELIX_HANDLER = "helix"`; the handler doc comment lists all three. `ModalInput::for_handler` builds `Helix` for it, and `HELIX_HANDLER` joins `MODAL_HANDLERS` in `session.rs` (which `is_modal_handler` reads).
- Unknown-handler warning: in `validate_main_config` (`src/config/readers.rs`), when `config.modal.handler` is none of `DEFAULT_HANDLER`, `VIM_HANDLER`, `HELIX_HANDLER`, push `WarningKind::InvalidValue { key: "modal.handler", message: "unknown handler \"…\"; using the default editing mode" }`. Report it but leave the field alone, for the reason `validate_custom_exports` gives: the loaded value is written back on the next save, so resetting it would erase a `helix` setting written by a newer build. `for_handler` already treats an unknown name as the default handler.
- Settings: the vim toggle row in `ui/settings_overlay/rows.rs` (`LABEL_VIM_MODE`) becomes a three-way "Editing mode" pill (Default / Vim / Helix) using `ui/controls.rs`'s `Control::Pill`, `ControlValue::Choice` and `cycle_index`. An unknown handler reads as Default. `apply_live_update` already calls `set_modal_handler` (commit 1). Remove `set_vim_enabled` once the welcome modal and `Action::ToggleVimMode` call `set_modal_handler` directly.
- Welcome modal: the vim toggle becomes the same pill (`use_vim: bool` → a handler choice).
- `Action::ToggleHelixMode` in `config/keymap.rs`, palette label "Toggle Helix mode"; both toggle actions switch to their handler, or back to default when it is already active. Place it next to `ToggleVimMode` in the keybinds overlay's `src/ui/keybinds_overlay/categories.rs` and in `docs/keybindings.md`'s action list.
- Keybinds overlay, command palette and hint line: the "drop `ExitToPreview` under a modal handler" logic already keys on `modal.is_some()` (`modal_enabled`) after commit 1.
- `config/config.toml`: document `handler = "helix"`.
- A tip in `src/app/tips.rs` (new id appended at the end).
- In-app manual: `docs/helix-mode.md` registered as a `DocId` next to `VimMode` in `src/docs/registry.rs`, and in the command palette's doc list.
- The [Docs](#docs) list, and the App tests marked 4c in [Testing](#testing).

## Testing

- `tests/vim.rs`: unchanged apart from commit 3's field rename. It is the regression net for commits 1–3.
- `src/editor/vim_ops/guard.rs` unit tests (commit 2), as listed above.
- `tests/helix.rs` (commits 4a and 4b), using the `feed` harness shape of `tests/vim.rs` with `HelixState::default()`:
  - The word-motion golden table (4a).
  - Each motion key in the Key table: selection extents and direction, counts, Normal vs. Select, `x` repeat, `X`, `%`, `;`, `Alt-;`, cross-line `f`/`t`, `Alt-.`, `mm`, `mi`/`ma` (4a).
  - Entering Insert from a selection leaves `selection = None`. Esc after `i`, `I`, `o`, `O` and `c` keeps the cursor in place; after `a` / `A` it steps back one grapheme and never across a line start (4a; `c` in 4b).
  - Each verb's buffer result, register, landing selection, and one undo step; `u` / `U` selection restore including a composed renumber step (4b).
  - Search landing, `n` / `N` in Select, `:` aliases and their `KeyOutcome`s (4b).
  - Grapheme safety: selections never split `❤\u{FE0F}`, ZWJ families, or `\r\n` (4a).
  - `put_cursor` with `extend` crossing the anchor in both directions keeps the anchor's grapheme selected (4a).
  - `adopt`, one test per case (4a), each followed by `d`:
    - a forward mouse-drag selection is kept and the cursor moves onto its last grapheme, so `d` deletes exactly the dragged text;
    - a backward one is already consistent;
    - a Shift+arrow selection made in Insert, followed by Esc, is kept;
    - an external cursor move collapses the stale selection.
  - `line_span`: `x` on a middle line and on an empty line are `Some`; `x` on the last line without a trailing newline is `None`; a mouse selection that happens to cover whole lines is `Some`; a selection ending mid-line is `None` (4a).
  - `adopt`: a one-grapheme forward drag collapses with the cursor on that grapheme (4a).
  - `f` with no match leaves the selection and mode unchanged; `j` `j` `j` through a short line keeps the sticky column (4a).
  - `x y` / `x d` on the last line without a trailing newline: charwise register, preceding newline kept. `x c` on a middle line deletes the newline. `x d` on a table row removes the row structurally (4b).
  - `p` after an `x` selection with a charwise register inserts at the start of the next line; `p` with an empty register changes nothing (4b).
- `tests/modal_parity.rs` (commit 4b): one scenario table run through both handlers via `ModalInput::feed`, asserting identical refusals and results for deleting across table cells, a non-row paste into a table, `J` across a header, a linewise delete in an ordered list (renumbered, one undo step), an oversized paste, and the `:w` / `:q` / `:q!` outcomes. This is the guard against Helix drifting into a silo again: a new verb that bypasses `vim_ops::guard` fails it.
- Proptest (commit 4a, extended with the verb keys in 4b) over `tests/support/markdown_gen.rs` documents. Random Helix key sequences never panic, and after each key:
  - the selection is within the buffer and on grapheme boundaries;
  - when the key leaves Helix in Normal or Select (not Insert, where the default handler's Shift+arrows legitimately write `cursor == active`), the selection is `None`, or it spans at least two graphemes and `cursor.offset == head_cursor(selection)`;
  - no table is broken. Re-parse the buffer and count the *orphan pipe lines*: source lines whose first non-blank char is `|` and that fall outside every `Block::Table`'s source line range. A table that loses its header or alignment row stops parsing as a table, and its rows become orphan pipe lines.
    - For every key except `p`, `P` and `R`, the orphan count must not increase.
    - For those three, it may increase by at most the number of `|`-led lines in the register times the paste count, since pasting a yanked row into prose is allowed.
- App tests:
  - (4a) Ctrl-C, Ctrl-X and Ctrl-V in Helix Normal and Select behave as under the default handler: with a selection they act on the stored span exactly as painted, without one Ctrl-V inserts at the cursor, and Helix's mode is unchanged afterwards.
  - (4c) Parameterize the park / enable / switch tests in `src/app.rs` over both handlers. Add Helix twins of the command-line paste tests in `event_loop.rs`, and a Helix case for the capturing search-flow deferral (`modal_deferred`: Helix gets no keys while a replace flow is open). Test the unknown-handler warning, with the field left unchanged. Any test that can reach `Config::save` (the settings pill) takes `test_env::config_isolation()`.
- Insta snapshots (4a) of a rendered Helix selection, forward and backward, in Rendered and Raw views, plus a linewise `x` selection.

## Docs

- New: `docs/helix-mode.md` (user manual: key table, differences from Helix, the shadowed `Alt-;` / `Alt-.` bindings, undo selection note, the Insert-Esc landing); `docs/dev/modal-input.md` (`ModalInput` / `ModalCore` / `KeyOutcome`; the two selection conventions, vim Visual inclusive with `cursor == active` and Helix half-open with the cursor on the head grapheme, plus `adopt` and the commit-4a audit results; where guards live; and the rule that a handler never mutates the buffer except through `vim_ops::guard` or the shared helpers, enforced by `tests/modal_parity.rs`).
- Update: `docs/vim-mode.md` (`:q!`, `:wq!`, `:x!`, and the `:write-quit` alias now accepted), `docs/keybindings.md`, `docs/configuration.md` (including the unknown-handler warning), `docs/getting-started.md` (editing-mode choice), `docs/dev/vim-tables.md`, `docs/dev/vim-cmdline.md`, `docs/dev/input.md`, `docs/dev/search-replace.md` (search landing under Helix), `docs/dev/theming.md` (the `status_mode_vim_*` keys serve Helix too), `README.md` if it lists editing modes, `config/config.toml`, `CHANGELOG.md`.
- `AGENTS.md`: the project-structure tree (`input/modal_core`, `input/helix`, `editor/helix_ops`, `vim_ops/guard.rs`), the architecture deep-dives list (`modal-input.md`), and the overview sentence that names vim and default modes.

## Risks

1. **Helix word-selection semantics.** The largest unshared logic. Mitigated by the golden-table spike before any other 4a work.
2. **Commit 2 moves the table-corruption guards.** A caller that bypasses `vim_ops::guard` regresses the invariants in [vim-tables.md](../vim-tables.md). Mitigation: `tests/vim.rs` after every extraction, guard unit tests, and the parity test in commit 4b.
3. **The forward-selection cursor.** Under Helix a forward selection has `cursor.offset == prev_grapheme(active)`, which code written for the default handler or vim Visual may not expect. Mitigation: the commit-4a audit of every site that reads both, `adopt` as the single place other writers' selections are reconciled, and the proptest invariant. Collapsed selections are `None`, the state mouse clicks and `Action::Cut` already produce, so any Helix code that assumes `Some` must treat `None` as `cursor .. next_grapheme(cursor)`.
4. **Commit 3's rename diff.** Large but mechanical; review it as a pure move with `git diff --color-moved`.
5. **Force-quit plumbing.** New App path; tests must show a bare `:q` on a dirty buffer still reaches the dirty guard.
6. **Shadowed `Alt-;` / `Alt-.` chords.** Documented; the keybinds overlay could later flag the conflict.

## Out of scope / follow-ups

- Multiple selections and simultaneous editing (`C`, `s`, `S`, `Alt-s`, `,`).
- Dot-repeat, macros, marks, named registers.
- Surround (`ms`, `md`, `mr`) and tree-sitter text objects (`mif`, `mic`, …).
- Helix's `Space` and `z` menus, `*` (search for selection), `Alt-d` / `Alt-c` (delete without yank), `` ` `` / ``Alt-` `` (case forcing — cheap via `ReplaceWith::Lower` / `Upper` if wanted).
- Restoring the pre-edit selection direction on undo.
- One undo step per Insert session. Helix Insert passes keys through to the default handler, whose `History` merges typing into word groups, so one Insert session can take several `u` to undo. Real Helix undoes the whole session at once. vim has the same granularity today; changing it means a barrier policy for both handlers.
- Paragraph (`mip` / `map`) and angle-bracket (`mi<` / `ma<`) text objects. Adding them means new `TextObject` variants, which vim would gain as `ip` / `i<` too.
