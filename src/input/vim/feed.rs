//! The vim reducer: one key in, one [`VimOutcome`] out.
//!
//! `vim_feed` is the input-layer half of the two-layer split (mirroring
//! `MouseDispatcher`): it decides *what* the user asked for and applies
//! the simple cursor / mode transitions directly.  Heavier resolution
//! (motion ranges, operators, text objects) moves into
//! `editor::vim_ops` in later checkpoints.
//!
//! CP2 surface: the core motions `w e b W E B 0 ^ $ gg G` (resolved via
//! `vim_ops::motion`), the Insert entries `i a I A o O`, and `v`/`V`
//! entry into Visual / Visual-Line.
//!
//! CP3 makes counts drive motions (`3j`, `5l`) and adds the
//! operator+motion reducer: `d`/`c`/`y` enter OperatorPending and a
//! following motion (or a doubled operator, `dd`/`yy`/`cc`) resolves an
//! [`OpRange`] that `vim_ops::execute_operator` applies as a single
//! delta.  The single-key edits `x X D C Y` reuse that same operator
//! machinery, and `p`/`P` paste the unnamed register (`vim_ops::paste`).
//! Counts combine across `[count]op[count]motion` (e.g. `2d3w` → 6
//! words).  In Normal a bare key is swallowed (it must never type);
//! Insert defers to the existing editing pipeline via
//! [`VimOutcome::Passthrough`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::document::{EditDelta, Selection};
use crate::editor::vim_ops::{
    doubled_line_range, execute_operator, first_non_blank, paste, resolve_motion,
    resolve_motion_range, vertical_line_range, Motion, OpRange, Operator,
};
use crate::editor::{EditorState, Mode};

use super::state::{PendingOp, VimRegister, VimState, VimSubMode, COUNT_CAP};

/// What `vim_feed` decided about a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimOutcome {
    /// A multi-key sequence is still accumulating — keep the pending
    /// count / operator state.
    Pending,
    /// The key was fully handled (a mutation applied, or a deliberate
    /// no-op).  The caller redraws and stops.
    Consumed,
    /// Not a vim key (e.g. a `Ctrl-*` chord) — fall through to the
    /// default keymap handler.
    Passthrough,
}

/// Feed one key press to the vim state machine.
pub fn vim_feed(
    vim: &mut VimState,
    editor: &mut EditorState,
    key: KeyEvent,
    viewport_height: usize,
    viewport_width: usize,
) -> VimOutcome {
    match vim.sub_mode {
        VimSubMode::Insert => feed_insert(vim, editor, key, viewport_height, viewport_width),
        VimSubMode::Visual | VimSubMode::VisualLine => {
            feed_visual(vim, editor, key, viewport_height, viewport_width)
        }
        // Normal and OperatorPending share the same entry: `feed_normal`
        // branches on `pending_op` to route the operator-pending keys.
        _ => feed_normal(vim, editor, key, viewport_height, viewport_width),
    }
}

// ── Insert ──────────────────────────────────────────────────────────────────

/// In Insert mode `vim_feed` owns only `Esc`; every printable char,
/// `Backspace`, `Enter`, etc. passes through to the unchanged editing
/// pipeline.  So Insert reuses the entire existing editor verbatim.
fn feed_insert(
    vim: &mut VimState,
    editor: &mut EditorState,
    key: KeyEvent,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    match key.code {
        KeyCode::Esc => {
            vim.sub_mode = VimSubMode::Normal;
            vim.reset_pending();
            // Vim moves the cursor one char left on leaving Insert, but
            // never back across a line boundary.
            let (_, col) = editor.cursor.line_col(&editor.buffer);
            if col > 0 {
                editor.cursor.move_left(&editor.buffer);
                after_move(editor, vh, vw);
            }
            VimOutcome::Consumed
        }
        _ => VimOutcome::Passthrough,
    }
}

// ── Normal ────────────────────────────────────────────────────────────────────

fn feed_normal(
    vim: &mut VimState,
    editor: &mut EditorState,
    key: KeyEvent,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    if is_passthrough_chord(&key) {
        // The chord fires its app action via the default handler; a
        // half-typed operator / count must not linger behind it (`d`
        // then `Ctrl-S` would otherwise treat the next key as a delete
        // target).  Cancel any in-progress parse and drop OperatorPending
        // back to Normal before falling through.
        if vim.sub_mode == VimSubMode::OperatorPending {
            vim.sub_mode = VimSubMode::Normal;
        }
        vim.reset_pending();
        return VimOutcome::Passthrough;
    }
    match key.code {
        KeyCode::Esc => {
            // Esc cancels any in-progress operator / count and leaves
            // OperatorPending back in Normal.
            vim.sub_mode = VimSubMode::Normal;
            vim.reset_pending();
            VimOutcome::Consumed
        }
        KeyCode::Char(c) => feed_command_char(vim, editor, c, vh, vw, /*visual=*/ false),
        // Non-character keys (arrows, Home/End, PageUp/Down, …) keep
        // their default bindings so navigation still works in Normal.
        _ => VimOutcome::Passthrough,
    }
}

// ── Visual / Visual-Line ──────────────────────────────────────────────────────

/// CP2 Visual handling: motions extend the shared `selection`; `Esc`
/// leaves Visual back to Normal and clears the selection.  Operators,
/// `o` (swap ends), and the `v`↔`V` toggle land in CP6, so other keys
/// are swallowed (Consumed) without effect.  `Ctrl-*` chords still pass
/// through, so `Ctrl-C` copies the highlighted span via the existing
/// clipboard action.
fn feed_visual(
    vim: &mut VimState,
    editor: &mut EditorState,
    key: KeyEvent,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    if is_passthrough_chord(&key) {
        return VimOutcome::Passthrough;
    }
    match key.code {
        KeyCode::Esc => {
            exit_visual(vim, editor);
            VimOutcome::Consumed
        }
        KeyCode::Char(c) => feed_command_char(vim, editor, c, vh, vw, /*visual=*/ true),
        // Arrow keys mirror `h j k l` in Visual: extend the selection
        // rather than passing through to the default handler (which would
        // move the cursor *and* clear the selection).
        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
            let dir = match key.code {
                KeyCode::Left => 'h',
                KeyCode::Right => 'l',
                KeyCode::Up => 'k',
                _ => 'j',
            };
            feed_hjkl(editor, dir, vh, vw);
            extend_selection(editor);
            vim.reset_pending();
            VimOutcome::Consumed
        }
        _ => VimOutcome::Passthrough,
    }
}

/// Leave Visual / Visual-Line, dropping the selection.
fn exit_visual(vim: &mut VimState, editor: &mut EditorState) {
    vim.sub_mode = VimSubMode::Normal;
    vim.visual_anchor = None;
    vim.reset_pending();
    editor.selection = None;
}

// ── Shared command dispatch ────────────────────────────────────────────────────

/// Handle one `Char` key in Normal, OperatorPending, or Visual.  When
/// `visual` is set, a motion *extends* the selection (updating its active
/// end) instead of clearing it, and the Insert-entry / Visual-entry /
/// operator keys are inert (those belong to Normal).
fn feed_command_char(
    vim: &mut VimState,
    editor: &mut EditorState,
    c: char,
    vh: usize,
    vw: usize,
    visual: bool,
) -> VimOutcome {
    // `gg`: the first `g` is pending; the second resolves DocStart (or,
    // mid-operator, the linewise `dgg` span).  Resolved *before* count
    // accumulation so a stray `g` followed by a digit can't leave
    // `pending_g` set while the digit grows the count.
    if vim.pending_g {
        vim.pending_g = false;
        if c == 'g' {
            if let Some(operator) = vim.pending_op.and_then(operator_kind) {
                let range =
                    resolve_motion_range(Motion::DocStart, 1, editor.cursor.offset, &editor.buffer);
                run_operator(vim, editor, operator, range, vh, vw);
                return VimOutcome::Consumed;
            }
            apply_motion(editor, Motion::DocStart, count_of(vim), vh, vw, visual);
        }
        // Unknown `g`-command (or `gg` resolved above): clear the parse and
        // drop OperatorPending back to Normal.  Visual keeps its sub-mode.
        if !visual {
            vim.sub_mode = VimSubMode::Normal;
        }
        vim.reset_pending();
        return VimOutcome::Consumed;
    }

    // Operator-pending: an operator (`d`/`c`/`y`) is awaiting its motion.
    if let Some(op) = vim.pending_op {
        return feed_operator_pending(vim, editor, op, c, vh, vw);
    }

    // Count accumulation.  A leading `0` (no count yet) is the line-start
    // motion; a `0` *after* any `1`–`9` is the digit zero.
    if is_count_digit(c, vim.count) {
        vim.count = Some(accumulate(vim.count, c));
        return VimOutcome::Pending;
    }

    if c == 'g' {
        vim.pending_g = true;
        return VimOutcome::Pending;
    }

    // Operators enter OperatorPending (Normal only — Visual operators are
    // CP6).
    if !visual {
        if let Some(op) = operator_for(c) {
            vim.pending_op = Some(op);
            vim.sub_mode = VimSubMode::OperatorPending;
            return VimOutcome::Pending;
        }
    }

    let count = count_of(vim);

    // Pure motions resolved by `vim_ops::motion`.
    if let Some(motion) = motion_for(c) {
        apply_motion(editor, motion, count, vh, vw, visual);
        vim.reset_pending();
        return VimOutcome::Consumed;
    }

    // `h j k l` keep their bespoke table-aware handling (they mutate the
    // editor and manage the viewport themselves), so they're not part of
    // the offset-only `resolve_motion` set.  The count repeats the step.
    if matches!(c, 'h' | 'l' | 'j' | 'k') {
        if !visual {
            clear_selection(editor);
        }
        for _ in 0..count {
            feed_hjkl(editor, c, vh, vw);
        }
        if visual {
            extend_selection(editor);
        }
        vim.reset_pending();
        return VimOutcome::Consumed;
    }

    // Single-key edits and Insert / Visual entries act only from Normal.
    if !visual {
        match c {
            // `x`/`X`/`D`/`C`/`Y` are spelled in terms of the operator
            // machinery so they share the single-delta / register path.
            'x' => {
                let range = resolve_motion_range(
                    Motion::Right,
                    count,
                    editor.cursor.offset,
                    &editor.buffer,
                );
                run_operator(vim, editor, Operator::Delete, range, vh, vw);
            }
            'X' => {
                let range =
                    resolve_motion_range(Motion::Left, count, editor.cursor.offset, &editor.buffer);
                run_operator(vim, editor, Operator::Delete, range, vh, vw);
            }
            'D' => {
                let range = resolve_motion_range(
                    Motion::LineEnd,
                    count,
                    editor.cursor.offset,
                    &editor.buffer,
                );
                run_operator(vim, editor, Operator::Delete, range, vh, vw);
            }
            'C' => {
                let range = resolve_motion_range(
                    Motion::LineEnd,
                    count,
                    editor.cursor.offset,
                    &editor.buffer,
                );
                run_operator(vim, editor, Operator::Change, range, vh, vw);
            }
            'Y' => {
                let range = doubled_line_range(&editor.buffer, editor.cursor.offset, count);
                run_operator(vim, editor, Operator::Yank, range, vh, vw);
            }
            'p' => {
                paste_register(vim, editor, count, /*after=*/ true, vh, vw);
                vim.reset_pending();
            }
            'P' => {
                paste_register(vim, editor, count, /*after=*/ false, vh, vw);
                vim.reset_pending();
            }
            'i' => {
                enter_insert(vim, editor);
                vim.reset_pending();
            }
            'a' => {
                editor.cursor.move_right(&editor.buffer);
                enter_insert(vim, editor);
                after_move(editor, vh, vw);
                vim.reset_pending();
            }
            'I' => {
                move_first_non_blank(editor);
                enter_insert(vim, editor);
                vim.reset_pending();
            }
            'A' => {
                editor.cursor.move_line_end(&editor.buffer);
                enter_insert(vim, editor);
                after_move(editor, vh, vw);
                vim.reset_pending();
            }
            'o' => {
                open_line(vim, editor, /*below=*/ true, vh, vw);
                vim.reset_pending();
            }
            'O' => {
                open_line(vim, editor, /*below=*/ false, vh, vw);
                vim.reset_pending();
            }
            'v' => {
                enter_visual(vim, editor, /*line=*/ false);
                vim.reset_pending();
            }
            'V' => {
                enter_visual(vim, editor, /*line=*/ true);
                vim.reset_pending();
            }
            // Any other bare key is swallowed — a Normal-mode key must
            // never fall through to `InsertChar`.
            _ => vim.reset_pending(),
        }
    } else {
        vim.reset_pending();
    }

    VimOutcome::Consumed
}

/// Operator-pending dispatch: an operator `op` is set and we're reading
/// its target.  Handles the inter-operator count (`d2w`), the `dgg`
/// pending `g`, doubled operators (`dd`/`yy`/`cc`), vertical linewise
/// targets (`dj`/`dk`), and the charwise motion targets.  An unrecognized
/// key cancels the operator (vim's behavior).
fn feed_operator_pending(
    vim: &mut VimState,
    editor: &mut EditorState,
    op: PendingOp,
    c: char,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    // Count between the operator and its motion (`d2w`).
    if is_count_digit(c, vim.motion_count) {
        vim.motion_count = Some(accumulate(vim.motion_count, c));
        return VimOutcome::Pending;
    }

    // `dgg`: first `g` is pending, resolved on the next key by
    // `feed_command_char`'s `pending_g` arm (which sees `pending_op`).
    if c == 'g' {
        vim.pending_g = true;
        return VimOutcome::Pending;
    }

    let Some(operator) = operator_kind(op) else {
        // IndentRight / IndentLeft land in CP4; until then, cancel.
        vim.sub_mode = VimSubMode::Normal;
        vim.reset_pending();
        return VimOutcome::Consumed;
    };

    // `[count1] op [count2] motion` multiplies the two counts.
    let count = vim
        .count
        .unwrap_or(1)
        .saturating_mul(vim.motion_count.unwrap_or(1))
        .clamp(1, COUNT_CAP);

    // Doubled operator (`dd`/`yy`/`cc`) → linewise over `count` lines.
    if operator_for(c) == Some(op) {
        let range = doubled_line_range(&editor.buffer, editor.cursor.offset, count);
        run_operator(vim, editor, operator, range, vh, vw);
        return VimOutcome::Consumed;
    }

    // Vertical linewise targets (`dj` / `dk`).
    if c == 'j' || c == 'k' {
        let range = vertical_line_range(&editor.buffer, editor.cursor.offset, count, c == 'j');
        run_operator(vim, editor, operator, range, vh, vw);
        return VimOutcome::Consumed;
    }

    // Charwise / `gg`-`G` motion targets.
    if let Some(motion) = operator_motion_for(c) {
        let motion = change_word_to_word_end(operator, motion, editor);
        let range = resolve_motion_range(motion, count, editor.cursor.offset, &editor.buffer);
        run_operator(vim, editor, operator, range, vh, vw);
        return VimOutcome::Consumed;
    }

    // Text objects (`iw`, `i(`, …) land in CP7; any other key cancels.
    vim.sub_mode = VimSubMode::Normal;
    vim.reset_pending();
    VimOutcome::Consumed
}

/// Apply `op` over `range`, fold the yanked text into the register, then
/// move to Insert (for `c`) or back to Normal, and re-clamp the viewport.
fn run_operator(
    vim: &mut VimState,
    editor: &mut EditorState,
    op: Operator,
    range: OpRange,
    vh: usize,
    vw: usize,
) {
    let res = execute_operator(editor, op, range);
    // A no-op operator (empty charwise span) leaves the register untouched.
    if !res.register_text.is_empty() {
        vim.register = VimRegister {
            text: res.register_text,
            linewise: res.linewise,
        };
    }
    vim.reset_pending();
    if res.enter_insert {
        ensure_editing(editor);
        vim.sub_mode = VimSubMode::Insert;
    } else {
        vim.sub_mode = VimSubMode::Normal;
    }
    after_edit(editor, vh, vw);
}

/// Paste the unnamed register with the effective `count`.
fn paste_register(
    vim: &mut VimState,
    editor: &mut EditorState,
    count: u32,
    after: bool,
    vh: usize,
    vw: usize,
) {
    if vim.register.text.is_empty() {
        return;
    }
    ensure_editing(editor);
    paste(
        editor,
        &vim.register.text,
        vim.register.linewise,
        count,
        after,
    );
    after_edit(editor, vh, vw);
}

/// Map a key to one of the offset-only Normal/Visual motions, or `None`.
fn motion_for(c: char) -> Option<Motion> {
    Some(match c {
        'w' => Motion::WordForward,
        'e' => Motion::WordEnd,
        'b' => Motion::WordBackward,
        'W' => Motion::BigWordForward,
        'E' => Motion::BigWordEnd,
        'B' => Motion::BigWordBackward,
        '0' => Motion::LineStart,
        '^' => Motion::LineFirstNonBlank,
        '$' => Motion::LineEnd,
        'G' => Motion::DocEnd,
        _ => return None,
    })
}

/// Map a key to a motion usable as an operator target.  Adds `h`/`l`
/// (charwise `Left`/`Right`) to the plain `motion_for` set; `j`/`k` and
/// `gg` are handled separately (they're linewise).
fn operator_motion_for(c: char) -> Option<Motion> {
    match c {
        'h' => Some(Motion::Left),
        'l' => Some(Motion::Right),
        _ => motion_for(c),
    }
}

/// Map an operator key to its `PendingOp`, or `None`.  `>`/`<` (indent)
/// land in CP4.
fn operator_for(c: char) -> Option<PendingOp> {
    match c {
        'd' => Some(PendingOp::Delete),
        'c' => Some(PendingOp::Change),
        'y' => Some(PendingOp::Yank),
        _ => None,
    }
}

/// Translate a `PendingOp` to the editor-layer [`Operator`], or `None` for
/// the not-yet-wired indent operators (CP4).
fn operator_kind(op: PendingOp) -> Option<Operator> {
    match op {
        PendingOp::Delete => Some(Operator::Delete),
        PendingOp::Change => Some(Operator::Change),
        PendingOp::Yank => Some(Operator::Yank),
        PendingOp::IndentRight | PendingOp::IndentLeft => None,
    }
}

/// vim's `cw`/`cW` special case: when the cursor is on a non-blank, change
/// behaves like "change to the end of the current word" — it does not
/// swallow the trailing whitespace.  This is *not* the same as `ce`/`cE`:
/// `e` always advances past the cursor, so when the cursor is already on a
/// word's last char (always true for single-char words) `ce` would jump to
/// the *next* word's end and over-change.  `CurrentWordEnd` /
/// `CurrentBigWordEnd` stop at the end of the word the cursor is in.
fn change_word_to_word_end(op: Operator, motion: Motion, editor: &EditorState) -> Motion {
    if op != Operator::Change {
        return motion;
    }
    let cursor = editor.cursor.offset;
    let on_blank =
        cursor < editor.buffer.len_chars() && editor.buffer.rope().char(cursor).is_whitespace();
    if on_blank {
        return motion;
    }
    match motion {
        Motion::WordForward => Motion::CurrentWordEnd,
        Motion::BigWordForward => Motion::CurrentBigWordEnd,
        _ => motion,
    }
}

/// Whether `c` is a count digit given the current accumulator: any digit,
/// except a leading `0` (which is the line-start motion).
fn is_count_digit(c: char, acc: Option<u32>) -> bool {
    c.is_ascii_digit() && !(c == '0' && acc.is_none())
}

/// Append digit `c` to a count accumulator, capped at [`COUNT_CAP`].
fn accumulate(acc: Option<u32>, c: char) -> u32 {
    let digit = c.to_digit(10).unwrap_or(0);
    acc.unwrap_or(0)
        .saturating_mul(10)
        .saturating_add(digit)
        .min(COUNT_CAP)
}

/// The effective leading count for a plain motion (defaults to 1).
fn count_of(vim: &VimState) -> u32 {
    vim.count.unwrap_or(1).max(1)
}

/// Resolve `motion` to a target offset, move the cursor there with the
/// given `count`, and — in Visual — extend the selection.
fn apply_motion(
    editor: &mut EditorState,
    motion: Motion,
    count: u32,
    vh: usize,
    vw: usize,
    visual: bool,
) {
    ensure_editing(editor);
    if !visual {
        clear_selection(editor);
    }
    let target = resolve_motion(motion, count, editor.cursor.offset, &editor.buffer);
    editor.cursor.offset = target.min(editor.buffer.len_chars());
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    after_move(editor, vh, vw);
    if visual {
        extend_selection(editor);
    }
}

/// The `h j k l` cursor moves, including the rendered-table chrome skip.
fn feed_hjkl(editor: &mut EditorState, c: char, vh: usize, vw: usize) {
    ensure_editing(editor);
    let mut moved = true;
    match c {
        'h' => {
            // In a rendered table, step cell-to-cell over the auto-managed
            // border chrome; elsewhere — and always in Raw — a plain
            // grapheme step.
            if !editor.try_table_move_horizontal(/*forward=*/ false) {
                editor.cursor.move_left(&editor.buffer);
            }
        }
        'l' => {
            if !editor.try_table_move_horizontal(/*forward=*/ true) {
                editor.cursor.move_right(&editor.buffer);
            }
        }
        'j' => {
            // `try_table_move_vertical` refreshes the cursor block and
            // viewport on success, so only the plain-line path needs the
            // trailing `after_move`.
            if editor.try_table_move_vertical(/*down=*/ true, vh, vw) {
                moved = false;
            } else {
                editor.move_cursor_line(/*down=*/ true, /*visual=*/ false, vw);
            }
        }
        'k' => {
            if editor.try_table_move_vertical(/*down=*/ false, vh, vw) {
                moved = false;
            } else {
                editor.move_cursor_line(/*down=*/ false, /*visual=*/ false, vw);
            }
        }
        _ => unreachable!("feed_hjkl only handles h/j/k/l"),
    }
    if moved {
        after_move(editor, vh, vw);
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// `Ctrl-*` / `Alt-*` / `Super-*` chords keep their edamame meaning —
/// they fall through to the default keymap (Save, palette, undo, …).
/// `Shift` is *not* a passthrough modifier: a shifted letter like `I`
/// arrives as `Char('I')` with `SHIFT` and must still reach the reducer.
fn is_passthrough_chord(key: &KeyEvent) -> bool {
    key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
}

/// Vim never rests in Preview; coming from it (or any non-edit mode)
/// switches to Rendered so the cursor is visible and edits apply.  Raw
/// is left untouched — it's a fully supported vim surface.
fn ensure_editing(editor: &mut EditorState) {
    if editor.mode == Mode::Preview {
        editor.mode = Mode::Rendered;
    }
}

/// Enter Insert sub-mode, switching out of Preview first.
fn enter_insert(vim: &mut VimState, editor: &mut EditorState) {
    ensure_editing(editor);
    vim.sub_mode = VimSubMode::Insert;
}

/// Enter Visual / Visual-Line, anchoring the selection at the cursor.
/// The anchor is recorded both on the vim state (so CP6's `o`-swap and
/// line-expansion can find it) and on the shared `EditorState::selection`
/// (so the existing overlay painter highlights it for free).
fn enter_visual(vim: &mut VimState, editor: &mut EditorState, line: bool) {
    ensure_editing(editor);
    vim.sub_mode = if line {
        VimSubMode::VisualLine
    } else {
        VimSubMode::Visual
    };
    let offset = editor.cursor.offset;
    vim.visual_anchor = Some(offset);
    editor.selection = Some(Selection {
        anchor: offset,
        active: offset,
    });
}

/// Update the active end of the Visual selection to the cursor.  Falls
/// back to anchoring at the cursor if no selection exists (defensive —
/// `enter_visual` always installs one).
fn extend_selection(editor: &mut EditorState) {
    let active = editor.cursor.offset;
    match editor.selection.as_mut() {
        Some(sel) => sel.active = active,
        None => {
            editor.selection = Some(Selection {
                anchor: active,
                active,
            })
        }
    }
}

/// Open a new line below (`o`) or above (`O`) the cursor's line, place
/// the cursor on it, and enter Insert.  CP2 inserts a plain newline;
/// list-aware continuation (auto-renumber / marker copy) arrives in CP10.
fn open_line(vim: &mut VimState, editor: &mut EditorState, below: bool, vh: usize, vw: usize) {
    ensure_editing(editor);
    let (line, _) = editor.cursor.line_col(&editor.buffer);
    let line_start = editor.buffer.line_to_char(line);
    if below {
        // Insert a newline at the line end; `apply_delta`'s redo-cursor
        // lands on the start of the freshly-opened line below.
        let mut probe = editor.cursor;
        probe.move_line_end(&editor.buffer);
        editor.apply_delta(EditDelta {
            offset: probe.offset,
            removed: String::new(),
            inserted: "\n".to_string(),
        });
    } else {
        // Insert a newline at the line start; the new empty line sits
        // above, so park the cursor back on it.
        editor.apply_delta(EditDelta {
            offset: line_start,
            removed: String::new(),
            inserted: "\n".to_string(),
        });
        editor.cursor.offset = line_start;
        editor.update_cursor_block();
    }
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    after_move(editor, vh, vw);
    vim.sub_mode = VimSubMode::Insert;
}

/// Drop any active selection before a Normal-mode motion.  A lingering
/// mouse-drag selection would otherwise keep painting under the moving
/// cursor.  Visual sub-modes instead *extend* the selection, so this is
/// only called from the Normal motion path.
fn clear_selection(editor: &mut EditorState) {
    editor.selection = None;
}

/// Re-derive the cursor block and re-clamp the viewport after a motion.
fn after_move(editor: &mut EditorState, vh: usize, vw: usize) {
    editor.update_cursor_block();
    editor.ensure_cursor_visible(vh, vw);
}

/// Re-derive the cursor block and re-clamp the viewport after an edit.
/// An in-line operator delete leaves `parsed` stale (the deferred-reparse
/// optimization); flush it first so the rendered view and the
/// visibility check see fresh geometry.  Raw mode reads the buffer
/// directly, so no flush is needed there.
fn after_edit(editor: &mut EditorState, vh: usize, vw: usize) {
    if editor.mode != Mode::Raw {
        editor.flush_parsed_if_dirty();
    }
    editor.update_cursor_block();
    editor.ensure_cursor_visible(vh, vw);
}

/// Move the cursor to the first non-blank character of its line (the
/// `I` insert point).  Falls back to the line start on a blank line.
/// Shares the pure `vim_ops::motion::first_non_blank` resolver with the
/// `^` / `gg` / `G` motions so the two can't diverge.
fn move_first_non_blank(editor: &mut EditorState) {
    let line = editor.buffer.char_to_line(editor.cursor.offset);
    editor.cursor.offset = first_non_blank(&editor.buffer, line);
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}
