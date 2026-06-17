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
//! entry into Visual / Visual-Line.  Counts accumulate but do not yet
//! drive motions (that arrives in CP3).  In Normal a bare key is
//! swallowed (it must never type); Insert defers to the existing editing
//! pipeline via [`VimOutcome::Passthrough`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::document::{EditDelta, Selection};
use crate::editor::vim_ops::{first_non_blank, resolve_motion, Motion};
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
        VimSubMode::Visual | VimSubMode::VisualLine => {
            feed_visual(vim, editor, key, viewport_height, viewport_width)
        }
        // OperatorPending collapses to the Normal path in CP2 (operators
        // land in CP3).
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
        return VimOutcome::Passthrough;
    }
    match key.code {
        KeyCode::Esc => {
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

/// Handle one `Char` key in Normal or Visual.  When `visual` is set, a
/// motion *extends* the selection (updating its active end) instead of
/// clearing it, and the Insert-entry / Visual-entry keys are inert
/// (those belong to Normal).
fn feed_command_char(
    vim: &mut VimState,
    editor: &mut EditorState,
    c: char,
    vh: usize,
    vw: usize,
    visual: bool,
) -> VimOutcome {
    // `gg`: the first `g` is pending; the second resolves DocStart.  This
    // is resolved *before* count accumulation so a stray `g` followed by a
    // digit can't leave `pending_g` set while the digit grows the count —
    // any non-`g` follow-up key is an unknown `g`-command and is swallowed.
    if vim.pending_g {
        vim.pending_g = false;
        if c == 'g' {
            apply_motion(editor, Motion::DocStart, vh, vw, visual);
        }
        vim.reset_pending();
        return VimOutcome::Consumed;
    }

    // Count accumulation.  A leading `0` (no count yet) is the line-start
    // motion; a `0` *after* any `1`–`9` is the digit zero.  CP2 only
    // accumulates the count — it does not yet drive motions (CP3).
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

    if c == 'g' {
        vim.pending_g = true;
        return VimOutcome::Pending;
    }

    // Pure motions resolved by `vim_ops::motion`.
    if let Some(motion) = motion_for(c) {
        apply_motion(editor, motion, vh, vw, visual);
        vim.reset_pending();
        return VimOutcome::Consumed;
    }

    // `h j k l` keep their bespoke table-aware handling (they mutate the
    // editor and manage the viewport themselves), so they're not part of
    // the offset-only `resolve_motion` set.
    if matches!(c, 'h' | 'l' | 'j' | 'k') {
        if !visual {
            clear_selection(editor);
        }
        feed_hjkl(editor, c, vh, vw);
        if visual {
            extend_selection(editor);
        }
        vim.reset_pending();
        return VimOutcome::Consumed;
    }

    // Insert-entry and Visual-entry keys act only from Normal.
    if !visual {
        match c {
            'i' => {
                enter_insert(vim, editor);
            }
            'a' => {
                editor.cursor.move_right(&editor.buffer);
                enter_insert(vim, editor);
                after_move(editor, vh, vw);
            }
            'I' => {
                move_first_non_blank(editor);
                enter_insert(vim, editor);
            }
            'A' => {
                editor.cursor.move_line_end(&editor.buffer);
                enter_insert(vim, editor);
                after_move(editor, vh, vw);
            }
            'o' => open_line(vim, editor, /*below=*/ true, vh, vw),
            'O' => open_line(vim, editor, /*below=*/ false, vh, vw),
            'v' => enter_visual(vim, editor, /*line=*/ false),
            'V' => enter_visual(vim, editor, /*line=*/ true),
            // Any other bare key is swallowed — a Normal-mode key must
            // never fall through to `InsertChar`.
            _ => {}
        }
    }

    vim.reset_pending();
    VimOutcome::Consumed
}

/// Map a key to one of the offset-only motions, or `None` if the key
/// isn't a `resolve_motion` motion.
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

/// Resolve `motion` to a target offset, move the cursor there, and — in
/// Visual — extend the selection.  Counts are not yet applied (CP3); a
/// fixed count of 1 is passed.
fn apply_motion(editor: &mut EditorState, motion: Motion, vh: usize, vw: usize, visual: bool) {
    ensure_editing(editor);
    if !visual {
        clear_selection(editor);
    }
    let target = resolve_motion(motion, 1, editor.cursor.offset, &editor.buffer);
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
