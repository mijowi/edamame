//! The vim reducer: one key in, one [`VimOutcome`] out.
//!
//! `vim_feed` is the input-layer half of the two-layer split (mirroring
//! `MouseDispatcher`): it decides *what* the user asked for and applies
//! the simple cursor / mode transitions directly.  Heavier resolution
//! (motion ranges, operators, text objects) moves into
//! `editor::vim_ops` in later checkpoints.
//!
//! CP1 surface: `h j k l` motion, `i a I A` Insert entries, `Esc`
//! transitions, and leading-count digit accumulation.  Everything else
//! in Normal is swallowed (a bare key must never type); Insert defers to
//! the existing editing pipeline via [`VimOutcome::Passthrough`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::editor::{EditorState, Mode};

use super::state::{VimState, VimSubMode, COUNT_CAP};

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
        // OperatorPending / Visual / VisualLine collapse to the Normal
        // path in CP1 (operators and visual mode land later).
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
    // `Ctrl-*` / `Alt-*` / `Super-*` chords keep their edamame meaning —
    // they fall through to the default keymap (Save, palette, undo, …).
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return VimOutcome::Passthrough;
    }

    match key.code {
        KeyCode::Esc => {
            vim.reset_pending();
            VimOutcome::Consumed
        }
        KeyCode::Char(c) => feed_normal_char(vim, editor, c, vh, vw),
        // Non-character keys (arrows, Home/End, PageUp/Down, …) keep
        // their default bindings so navigation still works in Normal.
        _ => VimOutcome::Passthrough,
    }
}

fn feed_normal_char(
    vim: &mut VimState,
    editor: &mut EditorState,
    c: char,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    // Count accumulation.  A leading `0` (no count yet) is the
    // line-start motion, handled in a later checkpoint; a `0` *after*
    // any `1`–`9` is the digit zero.  CP1 only accumulates the count —
    // it does not yet drive motions.
    if c.is_ascii_digit() && !(c == '0' && vim.count.is_none()) {
        let digit = c.to_digit(10).unwrap_or(0);
        let next = vim
            .count
            .unwrap_or(0)
            .saturating_mul(10)
            .saturating_add(digit)
            .min(COUNT_CAP);
        vim.count = Some(next);
        return VimOutcome::Pending;
    }

    // `moved` gates the post-command viewport sync.  Insert-entry keys
    // that don't shift the cursor (`i`, `I`) set it false; the rest move
    // and then re-clamp.
    let mut moved = true;
    match c {
        'h' => {
            ensure_editing(editor);
            clear_selection(editor);
            // In a rendered table, step cell-to-cell over the auto-managed
            // border chrome (reusing the default handler's table logic);
            // elsewhere — and always in Raw — a plain grapheme step.
            if !editor.try_table_move_horizontal(/*forward=*/ false) {
                editor.cursor.move_left(&editor.buffer);
            }
        }
        'l' => {
            ensure_editing(editor);
            clear_selection(editor);
            if !editor.try_table_move_horizontal(/*forward=*/ true) {
                editor.cursor.move_right(&editor.buffer);
            }
        }
        'j' => {
            ensure_editing(editor);
            clear_selection(editor);
            // In a rendered table, move cell-to-cell (skipping the alignment
            // row); otherwise `j`/`k` are logical-line motions (`gj`/`gk`
            // would be the visual-row variants) and `move_cursor_line` adds
            // the rendered-view alignment-row / hidden-block skip.
            // `try_table_move_vertical` already refreshes the cursor block
            // and viewport on success, so only the plain-line path needs the
            // trailing `after_move`.
            if editor.try_table_move_vertical(/*down=*/ true, vh, vw) {
                moved = false;
            } else {
                editor.move_cursor_line(/*down=*/ true, /*visual=*/ false, vw);
            }
        }
        'k' => {
            ensure_editing(editor);
            clear_selection(editor);
            if editor.try_table_move_vertical(/*down=*/ false, vh, vw) {
                moved = false;
            } else {
                editor.move_cursor_line(/*down=*/ false, /*visual=*/ false, vw);
            }
        }
        'i' => {
            enter_insert(vim, editor);
            moved = false;
        }
        'a' => {
            // `enter_insert` switches out of Preview, so no separate
            // `ensure_editing` is needed here.
            editor.cursor.move_right(&editor.buffer);
            enter_insert(vim, editor);
        }
        'I' => {
            move_first_non_blank(editor);
            enter_insert(vim, editor);
            moved = false;
        }
        'A' => {
            // Like `a`, the cursor move is mode-independent and
            // `enter_insert` switches out of Preview, so no separate
            // `ensure_editing` is needed before the move.
            editor.cursor.move_line_end(&editor.buffer);
            enter_insert(vim, editor);
        }
        // Any other bare key is swallowed — a Normal-mode key must never
        // fall through to `InsertChar`.
        _ => moved = false,
    }

    if moved {
        after_move(editor, vh, vw);
    }
    vim.reset_pending();
    VimOutcome::Consumed
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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

/// Drop any active selection before a Normal-mode motion.  A lingering
/// mouse-drag selection would otherwise keep painting under the moving
/// cursor.  Visual sub-modes (CP6) will instead *extend* the selection,
/// so this is only called from the Normal motion arms.
fn clear_selection(editor: &mut EditorState) {
    editor.selection = None;
}

/// Re-derive the cursor block and re-clamp the viewport after a motion.
fn after_move(editor: &mut EditorState, vh: usize, vw: usize) {
    editor.update_cursor_block();
    editor.ensure_cursor_visible(vh, vw);
}

/// Move the cursor to the first non-blank character of its line (the
/// `I` insert point).  Falls back to the line start on a blank line.
fn move_first_non_blank(editor: &mut EditorState) {
    editor.cursor.move_line_start(&editor.buffer);
    let len = editor.buffer.len_chars();
    while editor.cursor.offset < len {
        let ch = editor.buffer.rope().char(editor.cursor.offset);
        if ch == '\n' || !ch.is_whitespace() {
            break;
        }
        editor.cursor.move_right(&editor.buffer);
    }
    editor.update_cursor_block();
}
