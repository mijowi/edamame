//! The vim reducer: one key in, one [`VimOutcome`] out.
//!
//! `vim_feed` is the input-layer half of the two-layer split (mirroring `MouseDispatcher`): it
//! decides *what* the user asked for and applies the simple cursor / mode transitions directly.
//! Range resolution — motions, operators, text objects — lives in `editor::vim_ops`.
//!
//! Counts combine across `[count]op[count]motion` (`2d3w` → 6 words).  In Normal a bare key is
//! always swallowed, never typed; Insert defers to the existing editing pipeline via
//! [`VimOutcome::Passthrough`].  See `docs/vim-mode.md` for the supported surface.

use std::ops::Range;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::Action;
use crate::document::{prev_grapheme_offset, EditDelta, Selection};
use crate::editor::edit_ops::{repeat_for_paste, PASTE_TOO_LARGE};
use crate::editor::vim_ops::{
    cell_scope, clear_substitute_preview, clear_table_cell, delete_table_row, doubled_line_range,
    end_incsearch, execute_operator, execute_substitute, first_non_blank, indent_lines,
    indent_list_item, insert_table_rows, join_lines, line_end_offset, lines_touch_a_table,
    op_range_breaks_a_table, open_list_continue, open_table_row, parse_ex, paste,
    range_breaks_a_table, renumber_list_at_cursor, replace_char, replace_char_range,
    replace_range_with, resolve_find_repeat, resolve_scoped_motion, resolve_scoped_op_range,
    resolve_text_object_range, scope_offset, set_case_range, table_paste_plan, toggle_case,
    toggle_case_range, update_incsearch, update_substitute_preview, vertical_line_range,
    visual_cell_step, visual_charwise_range, visual_endpoint_in_cell, visual_line_bounds,
    visual_line_char_range, word_under_cursor_at, CellLimit, ExCommand, FindKind, Motion, OpRange,
    OpResult, Operator, TableBreak, TableOpOutcome, TablePaste, TextObject,
};
use crate::editor::{edit_ops, EditorState, Mode};
use crate::input::mode_handler::default::{is_ctrl_backspace, is_ctrl_delete};

use super::cmdline::{self, CmdLineStep};
use super::state::{
    CmdLineKind, CmdLineState, PendingOp, VimRegister, VimState, VimSubMode, COUNT_CAP,
};

/// What `vim_feed` decided about a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VimOutcome {
    /// A multi-key sequence is still accumulating; keep the pending count / operator state.
    Pending,
    /// Fully handled (a mutation, or a deliberate no-op).  The caller redraws and stops.
    Consumed,
    /// Not a vim key — fall through to the default keymap handler.
    Passthrough,
    /// Start a search — an App-level effect, since the reducer holds only `&mut EditorState`.
    /// Emitted by `/` `?` and `*` `#`; `n`/`N` need no outcome, moving over
    /// `EditorState::search` directly.
    EnterSearch { forward: bool, query: String },
    /// `:w` — write via `Action::Save`, so the flash / autosave bookkeeping comes for free.
    Save,
    /// `:q` / `:wq` — quit via the dirty-guarded `Action::Quit`, saving first when
    /// `save_first`, so the buffer is clean before the guard runs.
    Quit { save_first: bool },
    /// `:saveas`, or a bare `:w` on a path-less buffer — write to a new path and *adopt* it.
    /// `path == None` opens the Save As modal.  `then_quit` carries the `:wq` / `:x` intent;
    /// `force` (a trailing `!`) skips the overwrite prompt.
    SaveAs {
        path: Option<PathBuf>,
        then_quit: bool,
        force: bool,
    },
    /// `:w <path>` — write a snapshot *without* changing the buffer's own path (real-vim
    /// `:w {file}`).  `then_quit` and `force` as for [`VimOutcome::SaveAs`].
    SaveCopy {
        path: PathBuf,
        then_quit: bool,
        force: bool,
    },
    /// A status / error message from an ex command for the App to flash.  The command itself
    /// already ran against `EditorState`; only the message bubbles up.
    Flash(String),
}

/// Feed one key press to the vim state machine.
pub fn vim_feed(
    vim: &mut VimState,
    editor: &mut EditorState,
    key: KeyEvent,
    viewport_height: usize,
    viewport_width: usize,
) -> VimOutcome {
    // Each Normal / Visual / ex command is its own undo step, so a barrier goes on both sides:
    // `x` `x` must not merge as one forward-delete run, nor `p` into the typing before it.
    // Insert-mode typing keeps merging, and so does the text typed after `o` into the newline
    // `o` opened — the trailing barrier is skipped when the command entered Insert.
    let command = vim.cmdline.is_some() || vim.sub_mode != VimSubMode::Insert;
    if command {
        editor.history.break_group();
    }
    // An active command line (`/` `?`, and `:` in CP9) captures every key
    // until the user submits (Enter) or cancels (Esc / Backspace past the
    // start), so it is checked before any sub-mode dispatch.
    let outcome = if vim.cmdline.is_some() {
        feed_cmdline(vim, editor, key, viewport_height, viewport_width)
    } else {
        match vim.sub_mode {
            VimSubMode::Insert => feed_insert(vim, editor, key, viewport_height, viewport_width),
            VimSubMode::Visual | VimSubMode::VisualLine => {
                feed_visual(vim, editor, key, viewport_height, viewport_width)
            }
            // Normal and OperatorPending share an entry; `feed_normal` branches on `pending_op`.
            _ => feed_normal(vim, editor, key, viewport_height, viewport_width),
        }
    };
    if command && vim.sub_mode != VimSubMode::Insert {
        editor.history.break_group();
    }
    outcome
}

// ── Insert ──────────────────────────────────────────────────────────────────

/// In Insert mode the reducer owns only `Esc`; everything else passes through to the unchanged
/// editing pipeline, so Insert reuses the existing editor verbatim.
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
            // Vim moves one char left on leaving Insert, never across a line boundary.
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

// ── Command line (`/` `?` `:`) ──────────────────────────────────────────────────

/// Drive the active command line.  A submitted `/` / `?` line becomes a
/// [`VimOutcome::EnterSearch`]; a submitted `:` line goes to [`submit_ex`]; an empty submit
/// just closes the prompt.
fn feed_cmdline(
    vim: &mut VimState,
    editor: &mut EditorState,
    key: KeyEvent,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    if vim.cmdline.is_none() {
        return VimOutcome::Passthrough;
    }
    // Up/Down recall the per-session history before the text field sees the key.  Both fields
    // are reached by direct field access, not a helper, so the disjoint-field borrows hold.
    if matches!(key.code, KeyCode::Up | KeyCode::Down) {
        let kind = vim.cmdline.as_ref().expect("cmdline is Some").kind;
        let history = match kind {
            CmdLineKind::Ex => &vim.ex_history,
            CmdLineKind::SearchForward | CmdLineKind::SearchBackward => &vim.search_history,
        };
        let cl = vim.cmdline.as_mut().expect("cmdline is Some");
        let input_before = cl.input.clone();
        if key.code == KeyCode::Up {
            cmdline::history_prev(cl, history);
        } else {
            cmdline::history_next(cl, history);
        }
        // History recall rewrites the whole line; re-derive the preview as for a typed edit.
        cmdline_live_update(vim, editor, &input_before, vh, vw);
        return VimOutcome::Consumed;
    }
    let cl = vim.cmdline.as_mut().expect("cmdline is Some");
    let kind = cl.kind;
    let input_before = cl.input.clone();
    match cmdline::feed_key(cl, key) {
        CmdLineStep::Editing => {
            cmdline_live_update(vim, editor, &input_before, vh, vw);
            VimOutcome::Consumed
        }
        CmdLineStep::Cancel => {
            vim.cmdline = None;
            match kind {
                CmdLineKind::Ex => {
                    // Esc reverts the previewed text, cursor, and scroll.
                    clear_substitute_preview(editor, /*restore_view=*/ true);
                }
                CmdLineKind::SearchForward | CmdLineKind::SearchBackward => {
                    // Esc drops the live search, restoring the prior hlsearch session.
                    end_incsearch(editor, &mut vim.incsearch);
                }
            }
            VimOutcome::Consumed
        }
        CmdLineStep::Submit(input) => {
            vim.cmdline = None;
            match kind {
                CmdLineKind::Ex => {
                    // Revert BEFORE `submit_ex` so `execute_substitute` runs against the
                    // pristine buffer: one undo step, identical to a preview-less submit.  No
                    // scroll restore — the execute path places the cursor, and the view is
                    // already where the edit landed.
                    clear_substitute_preview(editor, /*restore_view=*/ false);
                }
                CmdLineKind::SearchForward | CmdLineKind::SearchBackward => {
                    // Restore the pre-prompt view BEFORE `EnterSearch` runs: the App resolves
                    // focus relative to the original cursor.
                    end_incsearch(editor, &mut vim.incsearch);
                }
            }
            // Record every non-empty line, valid or not, as vim does.
            if !input.is_empty() {
                vim.record_command(kind, &input);
            }
            match kind {
                // An empty `/` / `?` query closes the prompt with no search.
                CmdLineKind::SearchForward if !input.is_empty() => VimOutcome::EnterSearch {
                    forward: true,
                    query: input,
                },
                CmdLineKind::SearchBackward if !input.is_empty() => VimOutcome::EnterSearch {
                    forward: false,
                    query: input,
                },
                CmdLineKind::Ex => submit_ex(editor, &input, vim.last_visual_range, vh, vw),
                _ => VimOutcome::Consumed,
            }
        }
    }
}

/// Re-derive the open command line's live preview — `:s` substitution or incsearch — after its
/// text changed.  A no-op when `before` equals the current input: a recompute costs two full
/// reparses plus a regex scan for a large `:%s`, so keys that don't change the line must skip
/// it.  Shared by the typing, history-recall, and paste paths so they can't diverge.
pub fn cmdline_live_update(
    vim: &mut VimState,
    editor: &mut EditorState,
    before: &str,
    vh: usize,
    vw: usize,
) {
    let (kind, input) = match vim.cmdline.as_ref() {
        Some(cl) if cl.input != before => (cl.kind, cl.input.clone()),
        _ => return,
    };
    match kind {
        CmdLineKind::Ex => update_substitute_preview(editor, &input, vim.last_visual_range, vh, vw),
        CmdLineKind::SearchForward => {
            update_incsearch(editor, &mut vim.incsearch, &input, true, vh, vw)
        }
        CmdLineKind::SearchBackward => {
            update_incsearch(editor, &mut vim.incsearch, &input, false, vh, vw)
        }
    }
}

/// Parse and run a submitted `:` ex command.  `:w`/`:q`/`:wq` bubble up as App-level outcomes
/// so the dirty-quit confirm and save flash fire exactly as for the `Ctrl-*` chords; `:s`/`:%s`
/// execute here and report via `Flash`.  An empty line is a silent no-op.
fn submit_ex(
    editor: &mut EditorState,
    input: &str,
    visual_range: Option<(usize, usize)>,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    if input.trim().is_empty() {
        return VimOutcome::Consumed;
    }
    match parse_ex(input) {
        // A bare `:w` writes to the current path, or — for a never-saved
        // buffer — prompts for one via the Save As modal.
        Ok(ExCommand::Write) => {
            if editor.buffer.path().is_some() {
                VimOutcome::Save
            } else {
                VimOutcome::SaveAs {
                    path: None,
                    then_quit: false,
                    force: false,
                }
            }
        }
        // `:w <path>` — write a copy, keep the current file; `!` forces past the prompt.
        Ok(ExCommand::WriteCopy { path, force }) => VimOutcome::SaveCopy {
            path: PathBuf::from(path),
            then_quit: false,
            force,
        },
        // `:saveas <path>` — re-point the buffer at the named path.
        Ok(ExCommand::WriteAs { path, force }) => VimOutcome::SaveAs {
            path: Some(PathBuf::from(path)),
            then_quit: false,
            force,
        },
        // Always prompts; the modal does its own overwrite check, so no force is threaded.
        Ok(ExCommand::SaveAsPrompt) => VimOutcome::SaveAs {
            path: None,
            then_quit: false,
            force: false,
        },
        Ok(ExCommand::Quit) => VimOutcome::Quit { save_first: false },
        Ok(ExCommand::WriteQuit) => {
            if editor.buffer.path().is_some() {
                VimOutcome::Quit { save_first: true }
            } else {
                // No path yet — prompt, then quit once it's written.
                VimOutcome::SaveAs {
                    path: None,
                    then_quit: true,
                    force: false,
                }
            }
        }
        // `:wq <path>` — write a copy, then quit (copy semantics like `:w`).
        Ok(ExCommand::WriteQuitCopy { path, force }) => VimOutcome::SaveCopy {
            path: PathBuf::from(path),
            then_quit: true,
            force,
        },
        // `:x` writes only when the buffer is dirty, then quits — the
        // canonical vim behavior (`:wq` always writes).  A dirty,
        // path-less buffer prompts for a path before quitting.
        Ok(ExCommand::WriteQuitIfModified) => {
            if !editor.dirty {
                VimOutcome::Quit { save_first: false }
            } else if editor.buffer.path().is_some() {
                VimOutcome::Quit { save_first: true }
            } else {
                VimOutcome::SaveAs {
                    path: None,
                    then_quit: true,
                    force: false,
                }
            }
        }
        // The same jump `42G` makes, through the scoped wrapper like every other motion here
        // (the clamp is a no-op for a deliberately unscoped line jump).
        Ok(ExCommand::GoToLine(n)) => {
            let dest = resolve_scoped_motion(editor, Motion::GoToLine(n), 1, CellLimit::Append);
            move_to_offset(editor, dest, vh, vw, /*visual=*/ false);
            VimOutcome::Consumed
        }
        Ok(ExCommand::Substitute(sub)) => match execute_substitute(editor, &sub, visual_range) {
            Ok(0) => VimOutcome::Flash(format!("Pattern not found: {}", sub.pattern)),
            Ok(n) => {
                after_edit(editor, vh, vw);
                let plural = if n == 1 { "" } else { "s" };
                VimOutcome::Flash(format!("{n} substitution{plural}"))
            }
            Err(e) => VimOutcome::Flash(e.to_string()),
        },
        Err(e) => VimOutcome::Flash(e.to_string()),
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
    // `r{c}`: a printable char replaces, anything else cancels with no edit.
    if vim.pending_replace {
        return feed_replace_char(vim, editor, key, vh, vw);
    }
    // A pending `f`/`F`/`t`/`T` (possibly behind an operator) awaits its target char.
    if let Some(kind) = vim.pending_find {
        return feed_find_char(vim, editor, kind, key, vh, vw, /*visual=*/ false);
    }
    // `i`/`a` was pressed behind an operator; this key is the object char.
    if let Some(inner) = vim.pending_text_object {
        return feed_text_object(vim, editor, inner, key, vh, vw, /*visual=*/ false);
    }
    // These are the default keymap's word-delete chords, and Normal must never mutate the
    // buffer — so intercept them ahead of the passthrough and treat them as plain cursor
    // motions (Ctrl-H is move-left in real vim too).
    if is_ctrl_backspace(&key) || is_ctrl_delete(&key) {
        vim.sub_mode = VimSubMode::Normal;
        let dir = if is_ctrl_delete(&key) { 'l' } else { 'h' };
        for _ in 0..count_of(vim) {
            feed_hjkl(editor, dir, vh, vw, /*charwise_visual=*/ false);
        }
        vim.reset_pending();
        return VimOutcome::Consumed;
    }
    if is_passthrough_chord(&key) {
        // A half-typed operator / count must not linger behind the chord: `d` then `Ctrl-S`
        // would otherwise treat the next key as a delete target.
        if vim.sub_mode == VimSubMode::OperatorPending {
            vim.sub_mode = VimSubMode::Normal;
        }
        vim.reset_pending();
        return VimOutcome::Passthrough;
    }
    match key.code {
        KeyCode::Esc => {
            // Cancels the in-progress operator / count, dismisses an active search's
            // highlights (vim's `:noh`) while leaving the cursor on the match it navigated to,
            // and drops any lingering selection so Normal never sits on a stale highlight.
            // Only a *navigate* search reaches here — a capturing replace flow defers to
            // `DefaultHandler`, so vim never sees its `Esc`.
            vim.sub_mode = VimSubMode::Normal;
            vim.reset_pending();
            editor.exit_search();
            editor.selection = None;
            VimOutcome::Consumed
        }
        KeyCode::Char(c) => feed_command_char(vim, editor, c, vh, vw, /*visual=*/ false),
        // `Tab` / `Shift-Tab` walk the search matches like `n` / `N`, however the search was
        // started.  With no search they are inert — never `InsertTab`.
        KeyCode::Tab if editor.search.is_some() => {
            search_repeat(editor, /*forward=*/ true, count_of(vim), vh, vw);
            vim.reset_pending();
            VimOutcome::Consumed
        }
        KeyCode::BackTab if editor.search.is_some() => {
            search_repeat(editor, /*forward=*/ false, count_of(vim), vh, vw);
            vim.reset_pending();
            VimOutcome::Consumed
        }
        // Inside a table, passing through reaches `InsertTab`, whose in-table branch advances
        // a cell (mirroring `Shift-Tab` → `TablePrevCell`).  Outside one it would insert
        // spaces, so Tab stays inert there.
        KeyCode::Tab if editor.cursor_in_table() => {
            vim.reset_pending();
            VimOutcome::Passthrough
        }
        // The global keymap would turn these into `DeleteCharBack` / `Newline` / `InsertTab`,
        // and Normal must not mutate — so consume them as motions: left, right, next line's
        // first non-blank, and (for a searchless Tab) nothing.
        KeyCode::Backspace | KeyCode::Delete | KeyCode::Enter | KeyCode::Tab => {
            vim.sub_mode = VimSubMode::Normal;
            let n = count_of(vim);
            match key.code {
                KeyCode::Backspace => {
                    for _ in 0..n {
                        feed_hjkl(editor, 'h', vh, vw, /*charwise_visual=*/ false);
                    }
                }
                KeyCode::Delete => {
                    for _ in 0..n {
                        feed_hjkl(editor, 'l', vh, vw, /*charwise_visual=*/ false);
                    }
                }
                KeyCode::Enter => {
                    for _ in 0..n {
                        feed_hjkl(editor, 'j', vh, vw, /*charwise_visual=*/ false);
                    }
                    move_first_non_blank(editor);
                    after_move(editor, vh, vw);
                }
                _ => {} // Tab with no search: inert.
            }
            vim.reset_pending();
            VimOutcome::Consumed
        }
        // Arrows, Home/End, PageUp/Down keep their default bindings.
        _ => VimOutcome::Passthrough,
    }
}

// ── Visual / Visual-Line ──────────────────────────────────────────────────────

/// Visual handling: motions extend the shared `selection`, `Esc` returns to Normal.  The
/// Visual commands (operators, `o`, the `v`↔`V` toggle) are intercepted ahead of the shared
/// motion path so motions still extend while command keys act on the span.  `Ctrl-*` chords
/// pass through, so `Ctrl-C` copies via the existing clipboard action.
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
    // A pending `f`/`F`/`t`/`T` awaits its target; in Visual it only extends the selection.
    if let Some(kind) = vim.pending_find {
        return feed_find_char(vim, editor, kind, key, vh, vw, /*visual=*/ true);
    }
    // In Visual a text object sets the selection to the object's range.
    if let Some(inner) = vim.pending_text_object {
        return feed_text_object(vim, editor, inner, key, vh, vw, /*visual=*/ true);
    }
    // These would otherwise reach the keymap's word-delete actions and edit through the
    // selection; Visual must not mutate, so they extend like plain Backspace / Delete.
    if is_ctrl_backspace(&key) || is_ctrl_delete(&key) {
        let dir = if is_ctrl_delete(&key) { 'l' } else { 'h' };
        feed_hjkl(editor, dir, vh, vw, is_charwise_visual(vim));
        extend_selection(editor);
        vim.reset_pending();
        return VimOutcome::Consumed;
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
            // Otherwise fall through to the shared motion / count / find path.
            Some(out) => out,
            None => feed_command_char(vim, editor, c, vh, vw, /*visual=*/ true),
        },
        // Arrows mirror `h j k l` here: passing through would move the cursor *and* clear the
        // selection.  Backspace / Delete / Enter join them as left / right / down movers.
        KeyCode::Left
        | KeyCode::Right
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Backspace
        | KeyCode::Delete
        | KeyCode::Enter => {
            let dir = match key.code {
                KeyCode::Left | KeyCode::Backspace => 'h',
                KeyCode::Right | KeyCode::Delete => 'l',
                KeyCode::Up => 'k',
                _ => 'j', // Down, Enter
            };
            feed_hjkl(editor, dir, vh, vw, is_charwise_visual(vim));
            extend_selection(editor);
            vim.reset_pending();
            VimOutcome::Consumed
        }
        // Inert in Visual — `InsertTab` would replace the selection.
        KeyCode::Tab | KeyCode::BackTab => {
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

/// Visual-mode command keys: the operators, `o`, the `v`/`V` toggle.  `None` lets the shared
/// motion / count / find path handle `c` instead.
fn feed_visual_command(
    vim: &mut VimState,
    editor: &mut EditorState,
    c: char,
    vh: usize,
    vw: usize,
) -> Option<VimOutcome> {
    // Same refusal as Normal's `J` / `>>` / `<<`, asked of the *selection* rather than the
    // cursor: a selection anchored in a table whose cursor has since moved out still reshapes
    // the rows it covers.
    if matches!(c, 'J' | '>' | '<') && visual_selection_touches_a_table(editor) {
        leave_visual_to_normal(vim, editor, vh, vw);
        return Some(VimOutcome::Flash(TABLE_STRUCTURAL_REFUSAL.to_owned()));
    }
    match c {
        'd' | 'x' => return Some(run_visual_operator(vim, editor, Operator::Delete, vh, vw)),
        'y' => return Some(run_visual_operator(vim, editor, Operator::Yank, vh, vw)),
        'c' | 's' => return Some(run_visual_operator(vim, editor, Operator::Change, vh, vw)),
        '>' => run_visual_indent(vim, editor, /*right=*/ true, vh, vw),
        '<' => run_visual_indent(vim, editor, /*right=*/ false, vh, vw),
        '~' => run_visual_toggle_case(vim, editor, vh, vw),
        // In Visual `u` is lowercase, not undo.
        'u' => run_visual_set_case(vim, editor, /*upper=*/ false, vh, vw),
        'U' => run_visual_set_case(vim, editor, /*upper=*/ true, vh, vw),
        'J' => run_visual_join(vim, editor, vh, vw),
        'p' | 'P' => return Some(run_visual_paste(vim, editor, vh, vw)),
        // Arm the replace and stay in Visual until the target char arrives.
        'r' => {
            vim.pending_replace = true;
            return Some(VimOutcome::Pending);
        }
        // Arm a text object (`viw`, `va(`); the next key sets the selection.
        'i' | 'a' => {
            vim.pending_text_object = Some(c == 'i');
            return Some(VimOutcome::Pending);
        }
        // Open the ex line pre-filled with `'<,'>`.  The concrete bounds are captured now,
        // before Visual exits; the highlight goes but the marks persist on `last_visual_range`.
        ':' => {
            if let Some(sel) = editor.selection {
                vim.last_visual_range = Some(visual_line_bounds(&sel, &editor.buffer));
            }
            exit_visual(vim, editor);
            vim.cmdline = Some(CmdLineState::with_input(
                CmdLineKind::Ex,
                "'<,'>".to_owned(),
            ));
            return Some(VimOutcome::Pending);
        }
        'o' => swap_visual_ends(vim, editor, vh, vw),
        // The current mode's own key exits to Normal; the other toggles charwise/linewise,
        // keeping anchor and selection (never snapped, so the toggle is lossless).
        'v' => toggle_visual_mode(vim, editor, /*line=*/ false),
        'V' => toggle_visual_mode(vim, editor, /*line=*/ true),
        _ => return None,
    }
    Some(VimOutcome::Consumed)
}

/// Run `f` with the active Visual selection, bailing to Normal if it is somehow absent — one
/// shared early-exit for every Visual command that needs the span.
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

/// Run a `d`/`y`/`c` operator over the Visual selection, then leave Visual.  The span comes
/// from the shared `vim_ops::visual` helpers, so the edit matches the highlight exactly.
/// `c`/`s` enter Insert; everything else returns to Normal.
fn run_visual_operator(
    vim: &mut VimState,
    editor: &mut EditorState,
    op: Operator,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    let mut outcome = VimOutcome::Consumed;
    with_selection(vim, editor, |vim, editor, sel| {
        let range = visual_op_range(vim, editor, &sel);
        // The Visual twin of `run_operator`'s guard: a VisualLine span over a header row, or a
        // charwise drag across two cells, corrupts the table the same way.
        if let Some(reason) = mutating_table_break(editor, op, &range) {
            leave_visual_to_normal(vim, editor, vh, vw);
            outcome = VimOutcome::Flash(reason.message().to_owned());
            return;
        }
        let res = execute_operator(editor, op, range);
        vim.visual_anchor = None;
        editor.selection = None;
        fold_op_result(vim, editor, op, res, vh, vw);
    });
    outcome
}

/// The range a Visual operator would run over: whole lines in VisualLine,
/// the inclusive charwise span otherwise.
fn visual_op_range(vim: &VimState, editor: &EditorState, sel: &Selection) -> OpRange {
    if vim.sub_mode == VimSubMode::VisualLine {
        let (first, last) = visual_line_bounds(sel, &editor.buffer);
        OpRange::Lines { first, last }
    } else {
        OpRange::Chars(visual_charwise_range(sel, &editor.buffer))
    }
}

/// Does the Visual selection overlap a table?  For the commands that reshape lines in place
/// (`J`, `>`, `<`), which have no range to hand [`mutating_table_break`].
fn visual_selection_touches_a_table(editor: &EditorState) -> bool {
    match editor.selection {
        Some(sel) => {
            let (first, last) = visual_line_bounds(&sel, &editor.buffer);
            lines_touch_a_table(editor, first, last)
        }
        None => editor.cursor_in_table(),
    }
}

/// `>` / `<` in Visual: indent every line the selection touches — linewise even from charwise
/// Visual, matching vim — then leave.  Never fills the register.
fn run_visual_indent(
    vim: &mut VimState,
    editor: &mut EditorState,
    right: bool,
    vh: usize,
    vw: usize,
) {
    with_selection(vim, editor, |vim, editor, sel| {
        let (first, last) = visual_line_bounds(&sel, &editor.buffer);
        ensure_editing(editor);
        indent_lines(editor, first, last, right, crate::constants::INDENT_WIDTH);
        leave_visual_to_normal(vim, editor, vh, vw);
    });
}

/// The char range a Visual *range edit* (`~`/`u`/`U`/`r`/`p`) operates on, via the same shared
/// helpers the render overlay and the operators use — so the edit matches the highlight.
fn visual_edit_range(vim: &VimState, editor: &EditorState, sel: &Selection) -> Range<usize> {
    if vim.sub_mode == VimSubMode::VisualLine {
        visual_line_char_range(sel, &editor.buffer)
    } else {
        visual_charwise_range(sel, &editor.buffer)
    }
}

/// `~` in Visual: toggle the selection's case as one delta, then leave Visual.
fn run_visual_toggle_case(vim: &mut VimState, editor: &mut EditorState, vh: usize, vw: usize) {
    with_selection(vim, editor, |vim, editor, sel| {
        let range = visual_edit_range(vim, editor, &sel);
        ensure_editing(editor);
        toggle_case_range(editor, range.start, range.end);
        leave_visual_to_normal(vim, editor, vh, vw);
    });
}

/// `u` / `U` in Visual: force the selection to lower or upper case, like `~`.
fn run_visual_set_case(
    vim: &mut VimState,
    editor: &mut EditorState,
    upper: bool,
    vh: usize,
    vw: usize,
) {
    with_selection(vim, editor, |vim, editor, sel| {
        let range = visual_edit_range(vim, editor, &sel);
        ensure_editing(editor);
        set_case_range(editor, range.start, range.end, upper);
        leave_visual_to_normal(vim, editor, vh, vw);
    });
}

/// `p` / `P` in Visual: replace the selection with the unnamed register as one delta, then
/// leave Visual.  The register is left **unchanged** — a deliberate departure from vim, which
/// clobbers it with the deleted text — so one yank can be pasted over several selections in
/// turn.  An empty register is a no-op that still leaves Visual.
fn run_visual_paste(
    vim: &mut VimState,
    editor: &mut EditorState,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    let mut outcome = VimOutcome::Consumed;
    with_selection(vim, editor, |vim, editor, sel| {
        if vim.register.text.is_empty() {
            leave_visual_to_normal(vim, editor, vh, vw);
            return;
        }
        let line_mode = vim.sub_mode == VimSubMode::VisualLine;
        let range = visual_edit_range(vim, editor, &sel);
        // A VisualLine range ends in '\n', so a charwise register needs one to keep its line.
        let text = if line_mode && !vim.register.linewise {
            format!("{}\n", vim.register.text)
        } else {
            vim.register.text.clone()
        };
        // A delete plus an insert, so both halves of the guard must clear.  The shape asked
        // about is the *selection's*, not the register's: a linewise row register dropped into
        // a charwise in-cell selection carries a `|` and a newline into a cell, and a charwise
        // register over a VisualLine row replaces it with something that is not a table row.
        if let Some(message) = paste_over_range_refusal(editor, &range, &text, line_mode) {
            leave_visual_to_normal(vim, editor, vh, vw);
            outcome = VimOutcome::Flash(message);
            return;
        }
        ensure_editing(editor);
        replace_range_with(editor, range.start, range.end, &text);
        if text.contains('\n') {
            // As for `p`: whole lines dropped into or over an ordered list.
            renumber_list_at_cursor(editor);
        }
        leave_visual_to_normal(vim, editor, vh, vw);
    });
    outcome
}

/// The refusal for dropping `text` over `range`, or `None` when both the
/// removal and the insertion are safe.
///
/// `linewise` describes the *payload as it will land* — the selection's own
/// shape, not the register's — because `run_visual_paste` has already
/// reconciled the two by the time it asks.  Passing the register's flag
/// instead lets each mismatch through: a linewise register keeps its `|` and
/// its newline when it is spliced into a charwise selection, and a charwise
/// register grows a newline when it replaces a whole line.
fn paste_over_range_refusal(
    editor: &EditorState,
    range: &Range<usize>,
    text: &str,
    linewise: bool,
) -> Option<String> {
    let rope = editor.buffer.rope();
    let len = rope.len_chars();
    let start = rope.char_to_byte(range.start.min(len));
    let end = rope.char_to_byte(range.end.min(len));
    if let Some(reason) = range_breaks_a_table(editor, start, end) {
        return Some(reason.message().to_owned());
    }
    match table_paste_plan(editor, text, linewise, /*after=*/ true) {
        TablePaste::Refused => Some(TABLE_PASTE_REFUSAL.to_owned()),
        _ => None,
    }
}

/// Resolve a pending Visual `r{c}`: replace every char in the selection with `c`, then leave
/// Visual.  Any other key cancels with no edit and keeps the selection, as vim does.
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
                let range = visual_edit_range(vim, editor, &sel);
                // This overwrites every char in the span, `|` delimiters included.
                if let Some(reason) =
                    op_range_breaks_a_table(editor, &OpRange::Chars(range.start..range.end))
                {
                    leave_visual_to_normal(vim, editor, vh, vw);
                    return VimOutcome::Flash(reason.message().to_owned());
                }
                ensure_editing(editor);
                replace_char_range(editor, range.start, range.end, c);
            }
            leave_visual_to_normal(vim, editor, vh, vw);
        }
        // Cancel, but stay in Visual with the selection.
        _ => vim.pending_replace = false,
    }
    VimOutcome::Consumed
}

/// `J` in Visual: join every line the selection touches; a single-line selection joins with
/// the line below, matching vim.
fn run_visual_join(vim: &mut VimState, editor: &mut EditorState, vh: usize, vw: usize) {
    with_selection(vim, editor, |vim, editor, sel| {
        let (first, last) = visual_line_bounds(&sel, &editor.buffer);
        ensure_editing(editor);
        editor.cursor.offset = editor.buffer.line_to_char(first);
        editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
        // `join_lines` does `max(2, count) - 1` joins, so a single-line span still joins one.
        let count = (last - first + 1) as u32;
        join_lines(editor, count);
        leave_visual_to_normal(vim, editor, vh, vw);
    });
}

/// `o` in Visual: swap the ends so a following motion grows the other side.
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

/// `v` / `V` while already in Visual: toggle charwise/linewise, or exit when the key matches
/// the current mode.  Anchor and selection survive the switch.
///
/// `V`→`v` is the second door into charwise Visual, so it owes the same append-slot pull-back
/// `enter_visual` performs — on *both* ends, since a linewise span's anchor and cursor are
/// independent.  The `v`→`V` direction needs nothing: linewise covers whole lines regardless.
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
        if !line {
            pull_cursor_into_cell(editor);
            pull_anchor_into_cell(vim, editor);
            // The operators read `EditorState::selection`, whose active end still holds the
            // pre-pull cursor.
            extend_selection(editor);
        }
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

/// Handle one `Char` key in Normal, OperatorPending, or Visual.  Under `visual` a motion
/// *extends* the selection instead of clearing it, and the Normal-only entry keys are inert.
fn feed_command_char(
    vim: &mut VimState,
    editor: &mut EditorState,
    c: char,
    vh: usize,
    vw: usize,
    visual: bool,
) -> VimOutcome {
    // Resolved *before* count accumulation, so a stray `g` followed by a digit can't leave
    // `pending_g` set while the digit grows the count.
    if vim.pending_g {
        vim.pending_g = false;
        if c == 'g' {
            // `5gg` is `5G`: the count is already a line number by now.
            let motion = line_jump(Motion::DocStart, operand_count(vim));
            if let Some(operator) = vim.pending_op.and_then(operator_kind) {
                let range = resolve_scoped_op_range(editor, motion, 1);
                return run_operator(vim, editor, operator, range, vh, vw);
            }
            apply_motion(
                editor,
                motion,
                count_of(vim),
                vh,
                vw,
                visual,
                cell_limit(vim),
            );
        }
        // Clear the parse; Visual keeps its sub-mode.
        if !visual {
            vim.sub_mode = VimSubMode::Normal;
        }
        vim.reset_pending();
        return VimOutcome::Consumed;
    }

    // An operator is awaiting its motion.
    if let Some(op) = vim.pending_op {
        return feed_operator_pending(vim, editor, op, c, vh, vw);
    }

    // A leading `0` is the line-start motion; a `0` after any `1`–`9` is the digit zero.
    if is_count_digit(c, vim.count) {
        vim.count = Some(accumulate(vim.count, c));
        return VimOutcome::Pending;
    }

    if c == 'g' {
        vim.pending_g = true;
        return VimOutcome::Pending;
    }

    // Operators enter OperatorPending; Visual has its own operator path.
    if !visual {
        if let Some(op) = operator_for(c) {
            vim.pending_op = Some(op);
            vim.sub_mode = VimSubMode::OperatorPending;
            return VimOutcome::Pending;
        }
    }

    let count = count_of(vim);

    // Arm a pending find, keeping the count so `3fx` finds the third `x`.
    if let Some(kind) = find_kind_for(c) {
        vim.pending_find = Some(kind);
        return VimOutcome::Pending;
    }

    // Replay (or reverse) the last find.  `resolve_find_repeat` skips an adjacent match for a
    // `t`/`T` repeat so `;` never sticks one char before the same target.
    if c == ';' || c == ',' {
        if let Some((kind, target)) = vim.last_find {
            let kind = if c == ',' { reverse_find(kind) } else { kind };
            let dest =
                resolve_find_repeat(&editor.buffer, editor.cursor.offset, target, kind, count);
            // Its own resolver, so the cell clamp is applied here rather than inherited from
            // `resolve_scoped_motion`.
            let dest = scope_offset(
                editor,
                Motion::FindChar(target, kind),
                dest,
                cell_limit(vim),
            );
            move_to_offset(editor, dest, vh, vw, visual);
        }
        vim.reset_pending();
        return VimOutcome::Consumed;
    }

    // Pure motions.  `G` alone reinterprets the count as a line number.
    if let Some(motion) = motion_for(c) {
        let motion = line_jump(motion, operand_count(vim));
        apply_motion(editor, motion, count, vh, vw, visual, cell_limit(vim));
        vim.reset_pending();
        return VimOutcome::Consumed;
    }

    // `h j k l` keep bespoke table-aware handling — they mutate the editor and manage the
    // viewport themselves — so they are not part of the offset-only motion set.
    if matches!(c, 'h' | 'l' | 'j' | 'k') {
        if !visual {
            clear_selection(editor);
        }
        let charwise_visual = is_charwise_visual(vim);
        for _ in 0..count {
            feed_hjkl(editor, c, vh, vw, charwise_visual);
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
            // Spelled via the operator machinery so they share the single-delta / register path.
            'x' => {
                let range = resolve_scoped_op_range(editor, Motion::Right, count);
                return run_operator(vim, editor, Operator::Delete, range, vh, vw);
            }
            'X' => {
                let range = resolve_scoped_op_range(editor, Motion::Left, count);
                return run_operator(vim, editor, Operator::Delete, range, vh, vw);
            }
            'D' => {
                let range = resolve_scoped_op_range(editor, Motion::LineEnd, count);
                return run_operator(vim, editor, Operator::Delete, range, vh, vw);
            }
            'C' => {
                let range = resolve_scoped_op_range(editor, Motion::LineEnd, count);
                return run_operator(vim, editor, Operator::Change, range, vh, vw);
            }
            'Y' => {
                let range = doubled_line_range(&editor.buffer, editor.cursor.offset, count);
                return run_operator(vim, editor, Operator::Yank, range, vh, vw);
            }
            'p' => {
                let outcome = paste_register(vim, editor, count, /*after=*/ true, vh, vw);
                vim.reset_pending();
                return outcome;
            }
            'P' => {
                let outcome = paste_register(vim, editor, count, /*after=*/ false, vh, vw);
                vim.reset_pending();
                return outcome;
            }
            // Keep the accumulated count: `3rx` replaces three chars.
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
                // Joining two rows — or the line above a table onto its header — makes one
                // malformed line, hence a span past the cursor's own line.  It reaches exactly
                // as far as `join_lines` does: `max(count, 2) - 1` lines below.
                let line = editor.buffer.char_to_line(editor.cursor.offset);
                let last = line + count.max(2) as usize - 1;
                if lines_touch_a_table(editor, line, last) {
                    vim.reset_pending();
                    return VimOutcome::Flash(TABLE_STRUCTURAL_REFUSAL.to_owned());
                }
                join_lines(editor, count);
                after_edit(editor, vh, vw);
                vim.reset_pending();
            }
            // Undo through the existing history path, so the dirty / list bookkeeping matches.
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
                // Never step across the newline: at end-of-line the insertion point is already
                // past the last char.
                let line = editor.buffer.char_to_line(editor.cursor.offset);
                if editor.cursor.offset < line_end_offset(&editor.buffer, line) {
                    editor.cursor.move_right(&editor.buffer);
                }
                enter_insert(vim, editor);
                after_move(editor, vh, vw);
                vim.reset_pending();
            }
            // Inside a table these target the *cell* — the row's `|` delimiters aren't
            // content, so line start / end are never useful insertion points.
            'I' => {
                match cell_scope(editor) {
                    Some(scope) => editor.place_cursor(scope.start),
                    None => move_first_non_blank(editor),
                }
                enter_insert(vim, editor);
                after_move(editor, vh, vw);
                vim.reset_pending();
            }
            'A' => {
                match cell_scope(editor) {
                    Some(scope) => editor.place_cursor(scope.end),
                    None => editor.cursor.move_line_end(&editor.buffer),
                }
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
            // Open the search prompt; `feed_cmdline` captures keys until Enter / Esc.
            '/' => {
                start_cmdline(vim, CmdLineKind::SearchForward);
                return VimOutcome::Pending;
            }
            '?' => {
                start_cmdline(vim, CmdLineKind::SearchBackward);
                return VimOutcome::Pending;
            }
            // Open the ex command line, likewise captured by `feed_cmdline`.
            ':' => {
                start_cmdline(vim, CmdLineKind::Ex);
                return VimOutcome::Pending;
            }
            // Walk the active search matches; a no-op when no search is active.
            'n' => {
                search_repeat(editor, /*forward=*/ true, count, vh, vw);
                vim.reset_pending();
            }
            'N' => {
                search_repeat(editor, /*forward=*/ false, count, vh, vw);
                vim.reset_pending();
            }
            // Search the word under the cursor; a no-op when the line has no keyword there.
            '*' => {
                let outcome = search_word_outcome(editor, /*forward=*/ true);
                vim.reset_pending();
                return outcome;
            }
            '#' => {
                let outcome = search_word_outcome(editor, /*forward=*/ false);
                vim.reset_pending();
                return outcome;
            }
            // Any other bare key is swallowed — Normal must never fall through to `InsertChar`.
            _ => vim.reset_pending(),
        }
    } else {
        vim.reset_pending();
    }

    VimOutcome::Consumed
}

/// Operator-pending dispatch: `op` is set and this key is its target.  An unrecognized key
/// cancels the operator, as vim does.
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

    // Indent operators are linewise and never touch the register, so they take their own path.
    if matches!(op, PendingOp::IndentRight | PendingOp::IndentLeft) {
        return feed_indent_pending(vim, editor, op, c, vh, vw);
    }

    // `dgg`: resolved on the next key by `feed_command_char`'s `pending_g` arm.
    if c == 'g' {
        vim.pending_g = true;
        return VimOutcome::Pending;
    }

    // Only `Delete`/`Change`/`Yank` reach here; the indent operators returned above.
    let operator = operator_kind(op).expect("indent operators handled before this point");

    // `[count1] op [count2] motion` multiplies the two counts.
    let count = vim
        .count
        .unwrap_or(1)
        .saturating_mul(vim.motion_count.unwrap_or(1))
        .clamp(1, COUNT_CAP);

    // Doubled operator (`dd`/`yy`/`cc`) → linewise over `count` lines.
    if operator_for(c) == Some(op) {
        // In a table the linewise unit is the row or the cell, not the raw source line.  `yy`
        // and a counted `Ndd` keep plain linewise behavior, which is safe because
        // `run_operator` refuses a table-breaking span: this interpretation is a convenience,
        // not the protection.
        if count == 1 {
            if let Some(out) = table_doubled_operator(vim, editor, operator, vh, vw) {
                return out;
            }
        }
        let range = doubled_line_range(&editor.buffer, editor.cursor.offset, count);
        return run_operator(vim, editor, operator, range, vh, vw);
    }

    // Vertical linewise targets (`dj` / `dk`).
    if c == 'j' || c == 'k' {
        let range = vertical_line_range(&editor.buffer, editor.cursor.offset, count, c == 'j');
        return run_operator(vim, editor, operator, range, vh, vw);
    }

    // `df(` / `dt(`: the next key resolves the range and runs the operator.
    if let Some(kind) = find_kind_for(c) {
        vim.pending_find = Some(kind);
        return VimOutcome::Pending;
    }

    // Charwise / `gg`-`G` motion targets.
    if let Some(motion) = operator_motion_for(c) {
        let motion = line_jump(motion, operand_count(vim));
        let motion = change_word_to_word_end(operator, motion, editor);
        let range = resolve_scoped_op_range(editor, motion, count);
        return run_operator(vim, editor, operator, range, vh, vw);
    }

    // `i`/`a` arm a text object, resolved by `feed_text_object` — checked at the top of
    // `feed_normal`, since `sub_mode` is still OperatorPending here.
    if c == 'i' || c == 'a' {
        vim.pending_text_object = Some(c == 'i');
        return VimOutcome::Pending;
    }

    // Any other key cancels the operator (vim's behavior).
    vim.sub_mode = VimSubMode::Normal;
    vim.reset_pending();
    VimOutcome::Consumed
}

/// Flashed when a line-oriented command would corrupt a table: `J` merges two rows into one
/// broken line, `>>` / `<<` indent a row out of the table.  Refusing loudly beats corrupting.
const TABLE_STRUCTURAL_REFUSAL: &str = "Not available in a table";

/// Flashed when a paste would break a table's shape.
const TABLE_PASTE_REFUSAL: &str = "Can't paste that into a table";

/// A doubled operator with the cursor inside a table: `dd` removes the row structurally, `cc`
/// clears the cell.  `None` falls through to the ordinary linewise path (outside a table, in
/// Raw mode, and for `yy`).  Both reuse `fold_op_result`, so the register, undo grouping, and
/// Insert transition come from the existing implementation.
fn table_doubled_operator(
    vim: &mut VimState,
    editor: &mut EditorState,
    operator: Operator,
    vh: usize,
    vw: usize,
) -> Option<VimOutcome> {
    let outcome = match operator {
        Operator::Delete => delete_table_row(editor, vh, vw),
        Operator::Change => clear_table_cell(editor),
        // `yy` yanks the raw row text, unchanged — it mutates nothing, so
        // there is no structure to protect.
        Operator::Yank => return None,
    };
    match outcome {
        TableOpOutcome::Applied(res) => {
            fold_op_result(vim, editor, operator, res, vh, vw);
            Some(VimOutcome::Consumed)
        }
        TableOpOutcome::Refused(reason) => {
            vim.sub_mode = VimSubMode::Normal;
            vim.reset_pending();
            Some(VimOutcome::Flash(reason.message().to_owned()))
        }
        TableOpOutcome::NotATable => None,
    }
}

/// Apply `op` over `range`, fold the yank into the register, then move to Insert (for `c`) or
/// Normal and re-clamp the viewport.
///
/// This and [`run_visual_operator`] are the two funnels every vim range mutation passes
/// through, which is why the structural table guard lives here rather than at the dozen call
/// sites that build a range.
fn run_operator(
    vim: &mut VimState,
    editor: &mut EditorState,
    op: Operator,
    range: OpRange,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    if let Some(reason) = mutating_table_break(editor, op, &range) {
        vim.sub_mode = VimSubMode::Normal;
        vim.reset_pending();
        return VimOutcome::Flash(reason.message().to_owned());
    }
    let res = execute_operator(editor, op, range);
    fold_op_result(vim, editor, op, res, vh, vw);
    VimOutcome::Consumed
}

/// The structural refusal for `op` over `range`, or `None` to proceed.  `Yank` never mutates,
/// so `yy` on a header row is just a copy.
fn mutating_table_break(editor: &EditorState, op: Operator, range: &OpRange) -> Option<TableBreak> {
    if op == Operator::Yank {
        return None;
    }
    op_range_breaks_a_table(editor, range)
}

/// Fold an [`execute_operator`] result back into `VimState`: store the register (a no-op
/// operator leaves it alone), clear the parse, transition to Insert or Normal, refresh.  Shared
/// by `run_operator` and `run_visual_operator` so the two can't drift; the Visual caller drops
/// the selection and anchor first.
fn fold_op_result(
    vim: &mut VimState,
    editor: &mut EditorState,
    op: Operator,
    res: OpResult,
    vh: usize,
    vw: usize,
) {
    // A linewise edit can disturb an ordered list's numbering; renumber as the non-vim delete
    // path does, folded into the operator's own undo step.  `yy` edits nothing, so there is no
    // step to fold into and nothing it could have disturbed.
    if res.linewise && op != Operator::Yank {
        renumber_list_at_cursor(editor);
    }
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
) -> VimOutcome {
    if vim.register.text.is_empty() {
        return VimOutcome::Consumed;
    }
    // Refused before anything changes, the switch out of Preview included.
    let Some(text) = repeat_for_paste(&vim.register.text, count as usize) else {
        return VimOutcome::Flash(PASTE_TOO_LARGE.to_owned());
    };
    // Inside a table the ordinary "line after the cursor's" landing spot sits above the
    // alignment row when the cursor is on the header, wedging a data row into the table's
    // declaration.  `table_paste_plan` picks a legal row boundary and refuses a non-row
    // register.
    match table_paste_plan(editor, &vim.register.text, vim.register.linewise, after) {
        TablePaste::Refused => return VimOutcome::Flash(TABLE_PASTE_REFUSAL.to_owned()),
        TablePaste::RowsAt(at) => {
            ensure_editing(editor);
            insert_table_rows(editor, at, &text);
            after_edit(editor, vh, vw);
            return VimOutcome::Consumed;
        }
        TablePaste::NotATable => {}
    }
    ensure_editing(editor);
    paste(editor, &text, vim.register.linewise, after);
    after_edit(editor, vh, vw);
    VimOutcome::Consumed
}

/// Operator-pending dispatch for the indent operators: only the doubled forms `>>` / `<<` are
/// supported; any other following key cancels.
fn feed_indent_pending(
    vim: &mut VimState,
    editor: &mut EditorState,
    op: PendingOp,
    c: char,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    let right = op == PendingOp::IndentRight;
    // Multiplies the leading and inter-operator counts, like the other operators.
    if operator_for(c) == Some(op) {
        let count = vim
            .count
            .unwrap_or(1)
            .saturating_mul(vim.motion_count.unwrap_or(1))
            .clamp(1, COUNT_CAP);
        // Indenting a row pushes it out of the table; asked of every line the count covers.
        let line = editor.buffer.char_to_line(editor.cursor.offset);
        if lines_touch_a_table(editor, line, line + count as usize - 1) {
            vim.sub_mode = VimSubMode::Normal;
            vim.reset_pending();
            return VimOutcome::Flash(TABLE_STRUCTURAL_REFUSAL.to_owned());
        }
        ensure_editing(editor);
        // An uncounted `>>` on a list item nests / un-nests it structurally; anything else
        // falls back to the plain space-based indent over the line span.
        if count == 1 && indent_list_item(editor, right) {
            after_edit(editor, vh, vw);
        } else if let OpRange::Lines { first, last } =
            doubled_line_range(&editor.buffer, editor.cursor.offset, count)
        {
            indent_lines(editor, first, last, right, crate::constants::INDENT_WIDTH);
            after_edit(editor, vh, vw);
        }
    }
    // Doubled or not, the sequence is finished.
    vim.sub_mode = VimSubMode::Normal;
    vim.reset_pending();
    VimOutcome::Consumed
}

/// Cancel a pending sub-state (`r{c}`, a find, a text object) because `key` is not what it was
/// awaiting.  A `Ctrl-*` chord still fires its app action (`Passthrough`); anything else is
/// swallowed.  Either way the operator / count is dropped and `OperatorPending` falls back to
/// Normal.  A Visual selection is untouched — `sub_mode` is never `OperatorPending` there.
fn cancel_pending(vim: &mut VimState, key: &KeyEvent) -> VimOutcome {
    if vim.sub_mode == VimSubMode::OperatorPending {
        vim.sub_mode = VimSubMode::Normal;
    }
    vim.reset_pending();
    if is_passthrough_chord(key) {
        VimOutcome::Passthrough
    } else {
        VimOutcome::Consumed
    }
}

/// Resolve a pending `r{c}`: replace `count` chars with `c`.  Other keys go to
/// [`cancel_pending`].
fn feed_replace_char(
    vim: &mut VimState,
    editor: &mut EditorState,
    key: KeyEvent,
    vh: usize,
    vw: usize,
) -> VimOutcome {
    match key.code {
        KeyCode::Char(c) if !is_passthrough_chord(&key) => {
            let count = count_of(vim);
            // The Normal twin of `feed_visual_replace_char`'s guard: a count running past the
            // cell's content would write over the row's `|`.  The span is deliberately
            // unclamped — `replace_char` already refuses when it overflows the line, so only
            // the in-table case changes.
            let span = editor.cursor.offset..editor.cursor.offset + count as usize;
            if let Some(reason) = op_range_breaks_a_table(editor, &OpRange::Chars(span)) {
                vim.reset_pending();
                return VimOutcome::Flash(reason.message().to_owned());
            }
            ensure_editing(editor);
            replace_char(editor, c, count);
            after_edit(editor, vh, vw);
            vim.reset_pending();
            VimOutcome::Consumed
        }
        _ => cancel_pending(vim, &key),
    }
}

/// Resolve a pending `f`/`F`/`t`/`T` with `key` as the target char.  Records the find for
/// `;` / `,`, then either runs the pending operator over its range (`df(`) or moves the cursor
/// / extends the selection.  Other keys go to [`cancel_pending`].
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
        _ => return cancel_pending(vim, &key),
    };
    vim.last_find = Some((kind, target));
    let motion = Motion::FindChar(target, kind);

    // A find can only be armed behind a Delete/Change/Yank — the indent ops route through
    // `feed_indent_pending`, which never arms one — so `operator_kind` is always `Some` here.
    if let Some(operator) = vim.pending_op.and_then(operator_kind) {
        let count = vim
            .count
            .unwrap_or(1)
            .saturating_mul(vim.motion_count.unwrap_or(1))
            .clamp(1, COUNT_CAP);
        let range = resolve_scoped_op_range(editor, motion, count);
        return run_operator(vim, editor, operator, range, vh, vw);
    }

    // Per the invariant above we should never still be OperatorPending here, but fail safe to
    // Normal rather than linger in a half-consumed operator if that ever changes.
    if vim.sub_mode == VimSubMode::OperatorPending {
        vim.sub_mode = VimSubMode::Normal;
    }
    apply_motion(
        editor,
        motion,
        count_of(vim),
        vh,
        vw,
        visual,
        cell_limit(vim),
    );
    vim.reset_pending();
    VimOutcome::Consumed
}

/// Resolve a pending text object: the previous key was `i` / `a` and `key`
/// carries the object char (`w`, `(`, `"`, …).  In Normal it runs the
/// pending operator over the object's char range (`diw`, `ci(`); in Visual
/// it sets the selection to the object.  A non-object key cancels with no
/// edit; a `Ctrl-*` chord cancels and passes through so its app action still
/// fires (see [`cancel_pending`]).  Either cancel drops OperatorPending back
/// to Normal and leaves any Visual selection intact.
fn feed_text_object(
    vim: &mut VimState,
    editor: &mut EditorState,
    inner: bool,
    key: KeyEvent,
    vh: usize,
    vw: usize,
    visual: bool,
) -> VimOutcome {
    let obj = match key.code {
        KeyCode::Char(c) if !is_passthrough_chord(&key) => text_object_for(c, inner),
        _ => None,
    };
    let Some(obj) = obj else {
        // Not a text-object char (a chord, a non-char key, or something like `dij`).
        return cancel_pending(vim, &key);
    };
    let range = resolve_text_object_range(obj, editor.cursor.offset, &editor.buffer);

    if visual {
        // A missing object leaves the selection as-is; an empty inner object collapses it.
        if let Some(r) = range {
            select_text_object(vim, editor, r, vh, vw);
        }
        vim.reset_pending();
        return VimOutcome::Consumed;
    }

    // An empty inner range (`ci(` on `()`) is `execute_operator`'s problem: Delete/Yank no-op,
    // Change still enters Insert at the spot.  A missing object cancels.
    if let Some(operator) = vim.pending_op.and_then(operator_kind) {
        if let Some(r) = range {
            return run_operator(vim, editor, operator, OpRange::Chars(r), vh, vw);
        } else {
            vim.sub_mode = VimSubMode::Normal;
            vim.reset_pending();
        }
        return VimOutcome::Consumed;
    }

    // Unreachable — objects are only armed behind an operator — but fail safe.
    if vim.sub_mode == VimSubMode::OperatorPending {
        vim.sub_mode = VimSubMode::Normal;
    }
    vim.reset_pending();
    VimOutcome::Consumed
}

/// Install a Visual selection over a text object's half-open `range`, parking the cursor on the
/// object's **last** character (vim's landing spot for `viw`).  Charwise Visual is inclusive, so
/// `active` is `end` stepped back one grapheme — the derived span then reproduces `range`.
fn select_text_object(
    vim: &mut VimState,
    editor: &mut EditorState,
    range: Range<usize>,
    vh: usize,
    vw: usize,
) {
    ensure_editing(editor);
    let len = editor.buffer.len_chars();
    let start = range.start.min(len);
    let end = range.end.min(len);
    let active = if end > start {
        prev_grapheme_offset(&editor.buffer, end)
    } else {
        start
    };
    vim.visual_anchor = Some(start);
    editor.selection = Some(Selection {
        anchor: start,
        active,
    });
    editor.cursor.offset = active;
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    after_move(editor, vh, vw);
}

/// Map an object char (after `i`/`a`) to a [`TextObject`].  Both bracket directions resolve to
/// the same pair, and `b`/`B` are vim's aliases for the paren / brace pairs.
fn text_object_for(c: char, inner: bool) -> Option<TextObject> {
    Some(match c {
        'w' => TextObject::Word { inner, big: false },
        'W' => TextObject::Word { inner, big: true },
        '"' => TextObject::Quote { inner, quote: '"' },
        '\'' => TextObject::Quote { inner, quote: '\'' },
        '`' => TextObject::Quote { inner, quote: '`' },
        '(' | ')' | 'b' => TextObject::Pair {
            inner,
            open: '(',
            close: ')',
        },
        '[' | ']' => TextObject::Pair {
            inner,
            open: '[',
            close: ']',
        },
        '{' | '}' | 'B' => TextObject::Pair {
            inner,
            open: '{',
            close: '}',
        },
        _ => return None,
    })
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

/// Map a key to a motion usable as an operator target: `motion_for` plus charwise `h`/`l`.
/// The linewise `j`/`k` and `gg` are handled separately.
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

/// The reversed find direction, for `,`.
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

/// Translate a `PendingOp` to the editor-layer [`Operator`], or `None` for the indent
/// operators, which have their own path.
fn operator_kind(op: PendingOp) -> Option<Operator> {
    match op {
        PendingOp::Delete => Some(Operator::Delete),
        PendingOp::Change => Some(Operator::Change),
        PendingOp::Yank => Some(Operator::Yank),
        PendingOp::IndentRight | PendingOp::IndentLeft => None,
    }
}

/// vim's `cw`/`cW` special case: on a non-blank, change stops at the end of the current word
/// rather than swallowing the trailing whitespace.  Not the same as `ce`: `e` always advances
/// past the cursor, so on a word's last char it would jump to the *next* word's end and
/// over-change.
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

/// Any digit, except a leading `0` — that is the line-start motion.
fn is_count_digit(c: char, acc: Option<u32>) -> bool {
    c.is_ascii_digit() && !(c == '0' && acc.is_none())
}

/// Append digit `c` to a count accumulator, saturating at `u32::MAX`.
///
/// Deliberately *not* capped at [`COUNT_CAP`]: `{count}G` reads a count as a line number, and
/// clamping here would put line 10 000 out of keyboard reach while `:10000` still worked.  The
/// cap belongs to the consumers that turn a count into work.
fn accumulate(acc: Option<u32>, c: char) -> u32 {
    let digit = c.to_digit(10).unwrap_or(0);
    acc.unwrap_or(0).saturating_mul(10).saturating_add(digit)
}

/// The effective leading count for a plain motion, capped at [`COUNT_CAP`].  Every repetition
/// consumer reads through here, so this cap is what keeps `999999999j` from hanging the UI.
fn count_of(vim: &VimState) -> u32 {
    vim.count.unwrap_or(1).clamp(1, COUNT_CAP)
}

/// Fold a typed count into `gg` / `G`, which read it as a *line number* (`5G` → line 5).
///
/// Must happen at the key layer, not in `resolve_motion`: a bare `G` means the *last* line
/// while `1G` means the first, and a `count: u32` cannot tell them apart once `count_of` has
/// defaulted the absent count to 1.  `Option<u32>` is the distinction.
fn line_jump(motion: Motion, count: Option<u32>) -> Motion {
    match (motion, count) {
        (Motion::DocStart | Motion::DocEnd, Some(n)) => Motion::GoToLine(n),
        _ => motion,
    }
}

/// The count a line jump reads as its line number, or `None` when none was typed.  Both
/// accumulators multiply, mirroring `feed_operator_pending`'s product.
///
/// Uncapped, unlike every other count reader: this is a line number, `goto_line_index` clamps
/// it, and no iteration is driven by it.
fn operand_count(vim: &VimState) -> Option<u32> {
    if vim.count.is_none() && vim.motion_count.is_none() {
        return None;
    }
    Some(
        vim.count
            .unwrap_or(1)
            .saturating_mul(vim.motion_count.unwrap_or(1))
            .max(1),
    )
}

/// Is the charwise Visual span (the one covering the char under the cursor) live?  The single
/// predicate behind both halves of the table-cell tightening, so they can't drift apart.
fn is_charwise_visual(vim: &VimState) -> bool {
    vim.sub_mode == VimSubMode::Visual
}

/// How far into a table cell's trailing edge a cursor move may go: one
/// grapheme short of the append slot in charwise Visual, whose span would
/// otherwise highlight the padding space before the `|` and then eat it on
/// the edit (see `CellLimit::LastChar` — the guard permits that one, which
/// is exactly why the clamp has to prevent it).  VisualLine needs no
/// tightening — its span is whole lines however the cursor sits.
fn cell_limit(vim: &VimState) -> CellLimit {
    if is_charwise_visual(vim) {
        CellLimit::LastChar
    } else {
        CellLimit::Append
    }
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
    limit: CellLimit,
) {
    let target = resolve_scoped_motion(editor, motion, count, limit);
    move_to_offset(editor, target, vh, vw, visual);
}

/// Move the cursor to an already-resolved `target` and run the shared post-move bookkeeping;
/// in Visual, extend the selection rather than clearing it.
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
///
/// `charwise_visual` tightens the horizontal pair: such a span covers the char under the
/// cursor, so hopping to the next cell would highlight the `|` between them and promise an edit
/// the structural guard refuses.  There the step stays inside the cell.
fn feed_hjkl(editor: &mut EditorState, c: char, vh: usize, vw: usize, charwise_visual: bool) {
    ensure_editing(editor);
    let mut moved = true;
    match c {
        'h' => {
            // In a rendered table, step over the auto-managed border chrome; elsewhere (and
            // always in Raw) a plain grapheme step.
            let held = charwise_visual && visual_cell_step(editor, /*forward=*/ false);
            if !held && !editor.try_table_move_horizontal(/*forward=*/ false) {
                editor.cursor.move_left(&editor.buffer);
            }
        }
        'l' => {
            let held = charwise_visual && visual_cell_step(editor, /*forward=*/ true);
            if !held && !editor.try_table_move_horizontal(/*forward=*/ true) {
                editor.cursor.move_right(&editor.buffer);
            }
        }
        'j' => {
            // `try_table_move_vertical` refreshes the block and viewport itself on success.
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

/// `Ctrl-*` / `Alt-*` / `Super-*` chords keep their edamame meaning and fall through to the
/// default keymap.  `Shift` is *not* a passthrough modifier — a shifted letter like `I` arrives
/// as `Char('I')` with `SHIFT` and must reach the reducer.
fn is_passthrough_chord(key: &KeyEvent) -> bool {
    key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
}

/// Vim never rests in Preview; switch to Rendered so the cursor is visible and edits apply.
/// Raw is left untouched — a fully supported vim surface.
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

/// Enter Visual / Visual-Line, anchoring at the cursor.  The anchor is recorded on both the
/// vim state (for the `o`-swap and line expansion) and `EditorState::selection` (which the
/// overlay painter reads).
fn enter_visual(vim: &mut VimState, editor: &mut EditorState, line: bool) {
    ensure_editing(editor);
    vim.sub_mode = if line {
        VimSubMode::VisualLine
    } else {
        VimSubMode::Visual
    };
    // `$` parks the cursor on a cell's append slot, where a charwise span would highlight —
    // and eat — the padding before the `|`.  The same tightening `CellLimit::LastChar` gives
    // the motions, applied at the entry point they bypass; both ends come from the cursor here.
    if !line {
        pull_cursor_into_cell(editor);
    }
    let offset = editor.cursor.offset;
    vim.visual_anchor = Some(offset);
    editor.selection = Some(Selection {
        anchor: offset,
        active: offset,
    });
}

/// Pull the cursor off its table cell's append slot onto the cell's last character, for a
/// charwise Visual span about to cover it.  A no-op wherever `visual_endpoint_in_cell` declines.
fn pull_cursor_into_cell(editor: &mut EditorState) {
    if let Some(offset) = visual_endpoint_in_cell(editor, editor.cursor.offset) {
        editor.cursor.offset = offset;
        editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    }
}

/// The anchor half of [`pull_cursor_into_cell`], against the anchor's *own* cell.  Both stores
/// of the anchor move together, so the highlight and the edit stay the same span.
fn pull_anchor_into_cell(vim: &mut VimState, editor: &mut EditorState) {
    let Some(anchor) = vim.visual_anchor else {
        return;
    };
    let Some(pulled) = visual_endpoint_in_cell(editor, anchor) else {
        return;
    };
    vim.visual_anchor = Some(pulled);
    if let Some(sel) = editor.selection.as_mut() {
        sel.anchor = pulled;
    }
}

/// Update the Visual selection's active end to the cursor, anchoring there if (defensively)
/// none exists.
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

/// Open a new line below (`o`) or above (`O`), place the cursor on it, and enter Insert.
/// Inside a list it becomes a fresh list item; inside a table, a structural row.
fn open_line(vim: &mut VimState, editor: &mut EditorState, below: bool, vh: usize, vw: usize) {
    ensure_editing(editor);
    // A bare newline would split the current row in half and break the table.
    if open_table_row(editor, below, vh, vw) {
        after_edit(editor, vh, vw);
        vim.sub_mode = VimSubMode::Insert;
        return;
    }
    if open_list_continue(editor, below) {
        after_edit(editor, vh, vw);
        vim.sub_mode = VimSubMode::Insert;
        return;
    }
    let (line, _) = editor.cursor.line_col(&editor.buffer);
    let line_start = editor.buffer.line_to_char(line);
    if below {
        // `apply_delta`'s redo-cursor lands on the start of the freshly-opened line.
        let mut probe = editor.cursor;
        probe.move_line_end(&editor.buffer);
        editor.apply_delta(EditDelta {
            offset: probe.offset,
            removed: String::new(),
            inserted: "\n".to_string(),
        });
    } else {
        // The new empty line sits above, so park the cursor back on it.
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

/// Drop any active selection before a Normal-mode motion — a lingering mouse-drag selection
/// would otherwise keep painting under the moving cursor.
fn clear_selection(editor: &mut EditorState) {
    editor.selection = None;
}

/// Re-derive the cursor block and re-clamp the viewport after a motion.
fn after_move(editor: &mut EditorState, vh: usize, vw: usize) {
    editor.update_cursor_block();
    editor.ensure_cursor_visible(vh, vw);
}

/// Re-derive the cursor block and re-clamp the viewport after an edit.  An in-line operator
/// delete leaves `parsed` stale (the deferred-reparse optimization), so flush it first — except
/// in Raw, which reads the buffer directly.
fn after_edit(editor: &mut EditorState, vh: usize, vw: usize) {
    if editor.mode != Mode::Raw {
        editor.flush_parsed_if_dirty();
    }
    editor.update_cursor_block();
    editor.ensure_cursor_visible(vh, vw);
}

/// Open a command line (`/` `?` search, or `:` ex), clearing any in-progress parse first.
fn start_cmdline(vim: &mut VimState, kind: CmdLineKind) {
    vim.reset_pending();
    vim.cmdline = Some(CmdLineState::new(kind));
}

/// `n` / `N`: walk the focused match `count` times, then sync the cursor and scroll it into
/// view.  Mirrors `App::search_move_focus` but acts directly on `EditorState`, since vim owns
/// the keys.
fn search_repeat(editor: &mut EditorState, forward: bool, count: u32, vh: usize, vw: usize) {
    if editor.search.is_none() {
        return;
    }
    // Vim edits may have staled the match list since the last search.
    editor.ensure_search_fresh();
    for _ in 0..count.max(1) {
        if let Some(s) = editor.search.as_mut() {
            if forward {
                s.advance_focus();
            } else {
                s.retreat_focus();
            }
        }
    }
    editor.sync_cursor_to_search_focus();
    editor.scroll_focused_match_into_view(vh, vw);
}

/// `*` / `#`: build a search for the word under the cursor.  The cursor moves to the word's
/// start first, as vim does, so the App's cursor-relative focus is right — otherwise a backward
/// `#` from mid-word would snap to the current word's start instead of the previous occurrence.
///
/// The keyword comes from the buffer, not the keyboard, so it is `escape`d before becoming a
/// query: `EnterSearch` carries the *typed* form.  A no-op today, since a keyword run is
/// word-class only, but it keeps the contract true by construction.
fn search_word_outcome(editor: &mut EditorState, forward: bool) -> VimOutcome {
    match word_under_cursor_at(&editor.buffer, editor.cursor.offset) {
        Some((start, keyword)) => {
            editor.cursor.offset = start;
            editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
            editor.update_cursor_block();
            VimOutcome::EnterSearch {
                forward,
                query: crate::search::escape::escape(&keyword),
            }
        }
        None => VimOutcome::Consumed,
    }
}

/// Move the cursor to the first non-blank of its line (the `I` insert point), or the line
/// start when blank.  Shares `vim_ops::motion::first_non_blank` with the `^` / `gg` / `G`
/// motions so the two can't diverge.
fn move_first_non_blank(editor: &mut EditorState) {
    let line = editor.buffer.char_to_line(editor.cursor.offset);
    editor.cursor.offset = first_non_blank(&editor.buffer, line);
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}
