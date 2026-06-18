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
//!
//! CP5 adds the character-find motions `f F t T` (each waits one key for
//! its target, then records it so `;` / `,` can replay / reverse it),
//! the paragraph motions `{ }`, and the matching-pair motion `%`.  All
//! work as plain motions, as operator targets (`df(`, `d}`, `d%`), and as
//! Visual-selection extensions.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::Action;
use crate::document::{EditDelta, Selection};
use crate::editor::vim_ops::{
    doubled_line_range, execute_operator, first_non_blank, indent_lines, join_lines, paste,
    replace_char, replace_char_range, replace_range_with, resolve_find_repeat, resolve_motion,
    resolve_motion_range, set_case_range, toggle_case, toggle_case_range, vertical_line_range,
    visual_line_bounds, visual_line_char_range, FindKind, Motion, OpRange, OpResult, Operator,
};
use crate::editor::{edit_ops, EditorState, Mode};

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
    // `r{c}`: the previous key was `r`; this key is the replacement.  A
    // plain printable char replaces; anything else (Esc, an arrow, a
    // `Ctrl-*` chord) cancels the pending replace with no edit.
    if vim.pending_replace {
        return feed_replace_char(vim, editor, key, vh, vw);
    }
    // A pending `f`/`F`/`t`/`T` (possibly behind an operator, e.g. `df`)
    // is awaiting its target char; resolve it before anything else.
    if let Some(kind) = vim.pending_find {
        return feed_find_char(vim, editor, kind, key, vh, vw, /*visual=*/ false);
    }
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

/// Visual handling: motions extend the shared `selection`; `Esc` leaves
/// Visual back to Normal and clears the selection.  CP6 wires the Visual
/// commands — the operators `d`/`x`/`y`/`c`/`s`/`>`/`<`/`~`/`J`, `o` (swap
/// ends), and the `v`↔`V` toggle — intercepted ahead of the shared
/// motion path so motions still extend the selection while a command key
/// acts on it.  `Ctrl-*` chords still pass through, so `Ctrl-C` copies the
/// highlighted span via the existing clipboard action (VisualLine copy/cut
/// is widened to whole lines at the App dispatch layer).
fn feed_visual(
    vim: &mut VimState,
    editor: &mut EditorState,
    key: KeyEvent,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    // A pending Visual `r{c}` is awaiting its replacement char.
    if vim.pending_replace {
        return feed_visual_replace_char(vim, editor, key, vh, vw);
    }
    // A pending `f`/`F`/`t`/`T` is awaiting its target char (this only ever
    // extends the selection — Visual operators act on the existing span).
    if let Some(kind) = vim.pending_find {
        return feed_find_char(vim, editor, kind, key, vh, vw, /*visual=*/ true);
    }
    if is_passthrough_chord(&key) {
        return VimOutcome::Passthrough;
    }
    match key.code {
        KeyCode::Esc => {
            exit_visual(vim, editor);
            VimOutcome::Consumed
        }
        KeyCode::Char(c) => match feed_visual_command(vim, editor, c, vh, vw) {
            // A Visual command (operator / swap / toggle) acted on the
            // selection; otherwise fall through to the shared motion / count
            // / find / `gg` path so motions extend the selection.
            Some(out) => out,
            None => feed_command_char(vim, editor, c, vh, vw, /*visual=*/ true),
        },
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

/// Visual-mode command keys: the operators, `o` (swap ends), and the
/// `v`/`V` toggle.  Returns `Some(outcome)` when `c` is a Visual command;
/// `None` lets the shared motion / count / find / `gg` path handle it (so
/// motions still extend the selection and counts accumulate).
fn feed_visual_command(
    vim: &mut VimState,
    editor: &mut EditorState,
    c: char,
    vh: usize,
    vw: usize,
) -> Option<VimOutcome> {
    match c {
        'd' | 'x' => run_visual_operator(vim, editor, Operator::Delete, vh, vw),
        'y' => run_visual_operator(vim, editor, Operator::Yank, vh, vw),
        'c' | 's' => run_visual_operator(vim, editor, Operator::Change, vh, vw),
        '>' => run_visual_indent(vim, editor, /*right=*/ true, vh, vw),
        '<' => run_visual_indent(vim, editor, /*right=*/ false, vh, vw),
        '~' => run_visual_toggle_case(vim, editor, vh, vw),
        // `u` / `U`: force the selection to lower / upper case (in Visual,
        // `u` is *not* undo — that's a Normal-only key).
        'u' => run_visual_set_case(vim, editor, /*upper=*/ false, vh, vw),
        'U' => run_visual_set_case(vim, editor, /*upper=*/ true, vh, vw),
        'J' => run_visual_join(vim, editor, vh, vw),
        // `p` / `P`: replace the selection with the unnamed register.
        'p' | 'P' => run_visual_paste(vim, editor, vh, vw),
        // `r{c}`: arm the replace and wait for the target char (resolved by
        // `feed_visual_replace_char`).  Stays in Visual until then.
        'r' => {
            vim.pending_replace = true;
            return Some(VimOutcome::Pending);
        }
        'o' => swap_visual_ends(vim, editor, vh, vw),
        // `v` / `V`: pressing the *current* mode's key exits to Normal; the
        // other key switches between charwise and linewise, keeping the
        // anchor and selection (a lossless toggle — `selection` is never
        // snapped).
        'v' => toggle_visual_mode(vim, editor, /*line=*/ false),
        'V' => toggle_visual_mode(vim, editor, /*line=*/ true),
        _ => return None,
    }
    Some(VimOutcome::Consumed)
}

/// Run `f` with the active Visual selection.  If the selection is somehow
/// absent (it never should be while in a Visual sub-mode), bail to Normal via
/// `exit_visual` instead — so every Visual command that needs the span shares
/// one early-exit rather than repeating it.  `Selection` is `Copy`, so the
/// closure receives it by value alongside fresh `&mut` borrows.
fn with_selection(
    vim: &mut VimState,
    editor: &mut EditorState,
    f: impl FnOnce(&mut VimState, &mut EditorState, Selection),
) {
    let Some(sel) = editor.selection else {
        exit_visual(vim, editor);
        return;
    };
    f(vim, editor, sel);
}

/// Run a `d`/`y`/`c` operator over the current Visual selection, then leave
/// Visual.  Charwise Visual uses the raw `selection.range()` span (so the
/// edit matches the highlight exactly — see §2.6); VisualLine widens to
/// whole lines via the shared `visual_line_bounds` helper and yanks
/// linewise.  `c`/`s` enter Insert; everything else returns to Normal.
fn run_visual_operator(
    vim: &mut VimState,
    editor: &mut EditorState,
    op: Operator,
    vh: usize,
    vw: usize,
) {
    with_selection(vim, editor, |vim, editor, sel| {
        let range = if vim.sub_mode == VimSubMode::VisualLine {
            let (first, last) = visual_line_bounds(&sel, &editor.buffer);
            OpRange::Lines { first, last }
        } else {
            let (lo, hi) = sel.range();
            OpRange::Chars(lo..hi)
        };
        let res = execute_operator(editor, op, range);
        vim.visual_anchor = None;
        editor.selection = None;
        fold_op_result(vim, editor, res, vh, vw);
    });
}

/// `>` / `<` in Visual: indent / outdent every line the selection touches
/// (linewise even from charwise Visual, matching vim), then leave Visual.
/// Indent never fills the register (it reuses the `>>` / `<<` path).
fn run_visual_indent(
    vim: &mut VimState,
    editor: &mut EditorState,
    right: bool,
    vh: usize,
    vw: usize,
) {
    with_selection(vim, editor, |vim, editor, sel| {
        let (first, last) = visual_line_bounds(&sel, &editor.buffer);
        let tab_width = editor.tab_width;
        ensure_editing(editor);
        indent_lines(editor, first, last, right, tab_width);
        leave_visual_to_normal(vim, editor, vh, vw);
    });
}

/// The char range a Visual *range edit* (`~`/`u`/`U`/`r`/`p`) operates on:
/// the raw charwise `selection` span, or the line-expanded whole-line range
/// in VisualLine (the shared `visual_line_char_range`, so the edit always
/// matches the highlight).
fn visual_char_range(vim: &VimState, editor: &EditorState, sel: &Selection) -> Range<usize> {
    if vim.sub_mode == VimSubMode::VisualLine {
        visual_line_char_range(sel, &editor.buffer)
    } else {
        let (lo, hi) = sel.range();
        lo..hi
    }
}

/// `~` in Visual: toggle the case of the selection — the charwise span, or
/// the line-expanded range in VisualLine — as one delta, then leave Visual.
fn run_visual_toggle_case(vim: &mut VimState, editor: &mut EditorState, vh: usize, vw: usize) {
    with_selection(vim, editor, |vim, editor, sel| {
        let range = visual_char_range(vim, editor, &sel);
        ensure_editing(editor);
        toggle_case_range(editor, range.start, range.end);
        leave_visual_to_normal(vim, editor, vh, vw);
    });
}

/// `u` / `U` in Visual: force the selection to lower (`upper == false`) or
/// upper case as one delta, then leave Visual.  Operates on the charwise
/// span or the line-expanded range, exactly like `~`.
fn run_visual_set_case(
    vim: &mut VimState,
    editor: &mut EditorState,
    upper: bool,
    vh: usize,
    vw: usize,
) {
    with_selection(vim, editor, |vim, editor, sel| {
        let range = visual_char_range(vim, editor, &sel);
        ensure_editing(editor);
        set_case_range(editor, range.start, range.end, upper);
        leave_visual_to_normal(vim, editor, vh, vw);
    });
}

/// `p` / `P` in Visual: replace the selection with the unnamed register as a
/// single delta, then leave Visual.  The register is left **unchanged** — a
/// deliberate departure from vim's default (which clobbers the register with
/// the deleted text), so the same yank can be pasted over several selections
/// in turn (the widely-preferred behavior).  Charwise Visual replaces the raw
/// span; VisualLine replaces the whole lines.  A charwise register dropped
/// over whole lines gets a trailing newline so it keeps its own line.  An
/// empty register is a no-op that still leaves Visual.
fn run_visual_paste(vim: &mut VimState, editor: &mut EditorState, vh: usize, vw: usize) {
    with_selection(vim, editor, |vim, editor, sel| {
        if vim.register.text.is_empty() {
            leave_visual_to_normal(vim, editor, vh, vw);
            return;
        }
        let line_mode = vim.sub_mode == VimSubMode::VisualLine;
        let range = visual_char_range(vim, editor, &sel);
        // VisualLine replaces whole lines (range ends in '\n'); a charwise
        // register has no trailing newline, so add one to keep it on its own line.
        let text = if line_mode && !vim.register.linewise {
            format!("{}\n", vim.register.text)
        } else {
            vim.register.text.clone()
        };
        ensure_editing(editor);
        replace_range_with(editor, range.start, range.end, &text);
        leave_visual_to_normal(vim, editor, vh, vw);
    });
}

/// Resolve a pending Visual `r{c}`: replace every char in the selection (the
/// charwise span or the line-expanded range) with the printable key `c`, then
/// leave Visual.  A non-char key or a `Ctrl-*` chord cancels with no edit and
/// keeps the Visual selection (vim's behavior).
fn feed_visual_replace_char(
    vim: &mut VimState,
    editor: &mut EditorState,
    key: KeyEvent,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    match key.code {
        KeyCode::Char(c) if !is_passthrough_chord(&key) => {
            if let Some(sel) = editor.selection {
                let range = visual_char_range(vim, editor, &sel);
                ensure_editing(editor);
                replace_char_range(editor, range.start, range.end, c);
            }
            leave_visual_to_normal(vim, editor, vh, vw);
        }
        // Cancel the pending replace but stay in Visual with the selection.
        _ => vim.pending_replace = false,
    }
    VimOutcome::Consumed
}

/// `J` in Visual: join every line the selection touches into one (a
/// single-line selection joins with the line below, matching vim), then
/// leave Visual.
fn run_visual_join(vim: &mut VimState, editor: &mut EditorState, vh: usize, vw: usize) {
    with_selection(vim, editor, |vim, editor, sel| {
        let (first, last) = visual_line_bounds(&sel, &editor.buffer);
        ensure_editing(editor);
        editor.cursor.offset = editor.buffer.line_to_char(first);
        editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
        // `join_lines(count)` performs `max(2, count) - 1` joins, so a
        // single-line span (`last == first`) still joins one line below.
        let count = (last - first + 1) as u32;
        join_lines(editor, count);
        leave_visual_to_normal(vim, editor, vh, vw);
    });
}

/// `o` in Visual: swap the anchor and active ends so a following motion
/// grows the other side.  Stays in Visual.
fn swap_visual_ends(vim: &mut VimState, editor: &mut EditorState, vh: usize, vw: usize) {
    if let Some(sel) = editor.selection.as_mut() {
        std::mem::swap(&mut sel.anchor, &mut sel.active);
        let active = sel.active;
        vim.visual_anchor = Some(sel.anchor);
        editor.cursor.offset = active.min(editor.buffer.len_chars());
        editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
        after_move(editor, vh, vw);
    }
    vim.reset_pending();
}

/// `v` / `V` while already in Visual: toggle between charwise and linewise,
/// or exit to Normal when the pressed key matches the current mode.  The
/// anchor and selection survive a switch (the line expansion is recomputed
/// on demand, so nothing is lost).
fn toggle_visual_mode(vim: &mut VimState, editor: &mut EditorState, line: bool) {
    let target = if line {
        VimSubMode::VisualLine
    } else {
        VimSubMode::Visual
    };
    if vim.sub_mode == target {
        exit_visual(vim, editor);
    } else {
        vim.sub_mode = target;
        vim.reset_pending();
    }
}

/// Drop the Visual selection and return to Normal after a Visual edit that
/// does not enter Insert (`>`/`<`/`~`/`J`).
fn leave_visual_to_normal(vim: &mut VimState, editor: &mut EditorState, vh: usize, vw: usize) {
    vim.visual_anchor = None;
    editor.selection = None;
    vim.reset_pending();
    vim.sub_mode = VimSubMode::Normal;
    after_edit(editor, vh, vw);
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

    // `f`/`F`/`t`/`T`: arm a pending find and wait for the target char
    // (kept count intact so `3fx` finds the third `x`).
    if let Some(kind) = find_kind_for(c) {
        vim.pending_find = Some(kind);
        return VimOutcome::Pending;
    }

    // `;` / `,`: replay (or reverse) the last find.  `resolve_find_repeat`
    // skips an adjacent match for a `t`/`T` repeat so `;` never gets stuck
    // one char before the same target.
    if c == ';' || c == ',' {
        if let Some((kind, target)) = vim.last_find {
            let kind = if c == ',' { reverse_find(kind) } else { kind };
            let dest =
                resolve_find_repeat(&editor.buffer, editor.cursor.offset, target, kind, count);
            move_to_offset(editor, dest, vh, vw, visual);
        }
        vim.reset_pending();
        return VimOutcome::Consumed;
    }

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
            // `r{c}`: arm the replace and wait for the next key; keep the
            // accumulated count (`3rx` replaces three chars).
            'r' => {
                vim.pending_replace = true;
                return VimOutcome::Pending;
            }
            '~' => {
                toggle_case(editor, count);
                after_edit(editor, vh, vw);
                vim.reset_pending();
            }
            'J' => {
                join_lines(editor, count);
                after_edit(editor, vh, vw);
                vim.reset_pending();
            }
            // `u`: undo, reusing the existing history path (so dirty / list
            // bookkeeping match a normal undo).  `count` repeats it (`3u`).
            'u' => {
                for _ in 0..count {
                    edit_ops::apply(editor, Action::Undo, vh, vw);
                }
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

    // Indent operators (`>>` / `<<`) are linewise and never touch the
    // register, so they take their own path rather than `execute_operator`.
    if matches!(op, PendingOp::IndentRight | PendingOp::IndentLeft) {
        return feed_indent_pending(vim, editor, op, c, vh, vw);
    }

    // `dgg`: first `g` is pending, resolved on the next key by
    // `feed_command_char`'s `pending_g` arm (which sees `pending_op`).
    if c == 'g' {
        vim.pending_g = true;
        return VimOutcome::Pending;
    }

    // Only `Delete`/`Change`/`Yank` reach here — the indent operators
    // returned above via `feed_indent_pending`, so `operator_kind` is `Some`.
    let operator = operator_kind(op).expect("indent operators handled before this point");

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

    // `df(` / `dt(` / …: arm a pending find; the next key (the target
    // char) resolves the range and runs the operator via `feed_find_char`.
    if let Some(kind) = find_kind_for(c) {
        vim.pending_find = Some(kind);
        return VimOutcome::Pending;
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
    fold_op_result(vim, editor, res, vh, vw);
}

/// Fold an [`execute_operator`] result back into `VimState`: store the
/// register (unless the operator covered nothing — a no-op leaves it alone),
/// clear the in-progress parse, transition to Insert (for `c`) or Normal, and
/// refresh the editor.  Shared by the Normal-mode `run_operator` and the
/// Visual `run_visual_operator` so the two can't drift apart.  The Visual
/// caller drops the selection / anchor before calling this.
fn fold_op_result(
    vim: &mut VimState,
    editor: &mut EditorState,
    res: OpResult,
    vh: usize,
    vw: usize,
) {
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

/// Operator-pending dispatch for the indent operators (`>`/`<`).  CP4 wires
/// the doubled forms `>>` / `<<` (over `count` lines); any other following
/// key cancels (operator+motion indent like `>j` is out of CP4 scope, and
/// Visual `>`/`<` arrive in CP6).
fn feed_indent_pending(
    vim: &mut VimState,
    editor: &mut EditorState,
    op: PendingOp,
    c: char,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    let right = op == PendingOp::IndentRight;
    // Doubled operator (`>>` / `<<`): indent `count` lines from the cursor,
    // multiplying the leading and inter-operator counts like other operators.
    if operator_for(c) == Some(op) {
        let count = vim
            .count
            .unwrap_or(1)
            .saturating_mul(vim.motion_count.unwrap_or(1))
            .clamp(1, COUNT_CAP);
        if let OpRange::Lines { first, last } =
            doubled_line_range(&editor.buffer, editor.cursor.offset, count)
        {
            let tab_width = editor.tab_width;
            ensure_editing(editor);
            indent_lines(editor, first, last, right, tab_width);
            after_edit(editor, vh, vw);
        }
    }
    // Doubled or not, the sequence is finished: back to Normal, parse cleared.
    vim.sub_mode = VimSubMode::Normal;
    vim.reset_pending();
    VimOutcome::Consumed
}

/// Resolve a pending `r{c}`: replace `count` chars with the printable key
/// `c`.  Esc / arrows / `Ctrl-*` chords cancel with no edit.
fn feed_replace_char(
    vim: &mut VimState,
    editor: &mut EditorState,
    key: KeyEvent,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    if let KeyCode::Char(c) = key.code {
        if !is_passthrough_chord(&key) {
            let count = count_of(vim);
            ensure_editing(editor);
            replace_char(editor, c, count);
            after_edit(editor, vh, vw);
        }
    }
    vim.reset_pending();
    VimOutcome::Consumed
}

/// Resolve a pending `f`/`F`/`t`/`T`: the previous key armed the find and
/// `key` carries the target char.  Records the find for `;` / `,`, then
/// either runs the pending operator over the find range (`df(`) or moves
/// the cursor / extends the Visual selection.  A non-char key or a `Ctrl-*`
/// chord cancels with no edit (and drops OperatorPending back to Normal).
fn feed_find_char(
    vim: &mut VimState,
    editor: &mut EditorState,
    kind: FindKind,
    key: KeyEvent,
    vh: usize,
    vw: usize,
    visual: bool,
) -> VimOutcome {
    let target = match key.code {
        KeyCode::Char(c) if !is_passthrough_chord(&key) => c,
        _ => {
            if vim.sub_mode == VimSubMode::OperatorPending {
                vim.sub_mode = VimSubMode::Normal;
            }
            vim.reset_pending();
            return VimOutcome::Consumed;
        }
    };
    vim.last_find = Some((kind, target));
    let motion = Motion::FindChar(target, kind);

    // Operator target (`df(`): multiply the leading and inter-motion counts
    // exactly like the other operator motions.  A find can only be armed
    // behind a Delete/Change/Yank (indent ops route through
    // `feed_indent_pending`, which never arms a find), so `operator_kind` is
    // always `Some` when `pending_op` is set here.
    if let Some(operator) = vim.pending_op.and_then(operator_kind) {
        let count = vim
            .count
            .unwrap_or(1)
            .saturating_mul(vim.motion_count.unwrap_or(1))
            .clamp(1, COUNT_CAP);
        let range = resolve_motion_range(motion, count, editor.cursor.offset, &editor.buffer);
        run_operator(vim, editor, operator, range, vh, vw);
        return VimOutcome::Consumed;
    }

    // Plain motion: a Normal cursor move or a Visual selection extend.  Per
    // the invariant above the operator branch always fires when `pending_op`
    // is set, so we should never still be OperatorPending here — but fail
    // safe back to Normal rather than linger in a half-consumed operator if
    // that ever changes (e.g. a future operator without an `operator_kind`).
    if vim.sub_mode == VimSubMode::OperatorPending {
        vim.sub_mode = VimSubMode::Normal;
    }
    apply_motion(editor, motion, count_of(vim), vh, vw, visual);
    vim.reset_pending();
    VimOutcome::Consumed
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
        '{' => Motion::ParagraphBackward,
        '}' => Motion::ParagraphForward,
        '%' => Motion::MatchingPair,
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

/// Map `f`/`F`/`t`/`T` to its [`FindKind`], or `None`.
fn find_kind_for(c: char) -> Option<FindKind> {
    Some(match c {
        'f' => FindKind::Forward,
        'F' => FindKind::Backward,
        't' => FindKind::ForwardTill,
        'T' => FindKind::BackwardTill,
        _ => return None,
    })
}

/// The reversed find direction, for `,` (replay the last find the other
/// way): `f`↔`F`, `t`↔`T`.
fn reverse_find(kind: FindKind) -> FindKind {
    match kind {
        FindKind::Forward => FindKind::Backward,
        FindKind::Backward => FindKind::Forward,
        FindKind::ForwardTill => FindKind::BackwardTill,
        FindKind::BackwardTill => FindKind::ForwardTill,
    }
}

/// Map an operator key to its `PendingOp`, or `None`.
fn operator_for(c: char) -> Option<PendingOp> {
    match c {
        'd' => Some(PendingOp::Delete),
        'c' => Some(PendingOp::Change),
        'y' => Some(PendingOp::Yank),
        '>' => Some(PendingOp::IndentRight),
        '<' => Some(PendingOp::IndentLeft),
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
    let target = resolve_motion(motion, count, editor.cursor.offset, &editor.buffer);
    move_to_offset(editor, target, vh, vw, visual);
}

/// Move the cursor to an already-resolved `target` offset and run the
/// shared post-move bookkeeping; in Visual, extend the selection to the new
/// position instead of clearing it.  Shared by `apply_motion` and the
/// `;`/`,` find-repeat path (which resolves its own target).
fn move_to_offset(editor: &mut EditorState, target: usize, vh: usize, vw: usize, visual: bool) {
    ensure_editing(editor);
    if !visual {
        clear_selection(editor);
    }
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
