use crate::config::Action;
use crate::document::{next_grapheme_offset, prev_grapheme_offset, Buffer, EditDelta, Selection};
use crate::editor::footnote_edit;
use crate::editor::list_edit::{self, ListInfo};
use crate::editor::table_edit;
use crate::editor::table_edit_ops::{
    cursor_in_table, table_delete_column, table_delete_row, table_insert_column, table_insert_row,
    table_move_column, table_move_horizontal, table_move_row, table_next_cell, table_next_row,
    table_prev_cell, table_prev_row, try_move_cell_vertical,
};
use crate::editor::{EditorState, Mode};

/// True when `action` is a hot-path typing action that applies a
/// single edit and doesn't read `state.parsed`.  Used by `apply` to
/// skip the pre-action parse flush for in-line edits: `apply_delta`
/// only defers the re-parse when the edit doesn't cross a line,
/// and the rendered view reads the cursor block's raw text directly
/// from the buffer via `cursor_block_line_range` — so no stale
/// `source_map` is observed.  Cross-line edits (Newline, etc.) are
/// listed too: their `apply_delta` path re-parses inline, so
/// there's nothing to flush at entry.
fn is_hot_typing_action(action: &Action) -> bool {
    matches!(
        action,
        Action::InsertChar(_)
            | Action::InsertTab
            | Action::Newline
            | Action::DeleteCharBack
            | Action::DeleteCharForward
            | Action::DeleteWordBack
            | Action::DeleteWordForward
    )
}

/// Apply `action` to `state`, mutating the buffer, cursor, history and/or mode
/// as appropriate.
///
/// `viewport_width` is the column width of the document area and is used for
/// visual-line navigation when `state.visual_line_nav` is true.
///
/// Returns `true` if the application should quit.
pub fn apply(
    state: &mut EditorState,
    action: Action,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    // Flush any deferred re-parse before handling non-typing actions,
    // which typically read `state.parsed.source_map` /
    // `state.parsed.lines` (scroll, selection, mode transitions, list
    // / table structure, undo/redo, cursor movement).  This is the
    // sync point for the "re-parse on cursor move" invariant: the
    // cursor has already been moved off the typed-on line, or the
    // user triggered a selection / mode change that depends on a
    // fresh parse.  Hot typing actions skip this — `apply_delta`
    // decides inline whether the edit requires an immediate parse.
    if !is_hot_typing_action(&action) {
        state.flush_parsed_if_dirty();
    }

    // Snapshot pre-action state so we can decide after the match whether an
    // auto-renumber pass is warranted.  Undo/Redo must NOT trigger renumber —
    // their whole job is to restore the previous buffer exactly, so any
    // inconsistent numbering the user specifically reverted to must stick.
    let buffer_len_before = state.buffer.len_chars();
    let history_depth_before = state.history.undo_depth();
    let suppress_autonumber = matches!(action, Action::Undo | Action::Redo);

    match action {
        // ── Quit ──────────────────────────────────────────────────
        Action::Quit => return true,

        // ── Mode transitions ──────────────────────────────────────
        Action::EnterEditMode => {
            if state.mode == Mode::Preview {
                sync_cursor_to_scroll(state, viewport_height);
                state.mode = Mode::Rendered;
                state.visual_selection = None;
            }
        }
        Action::ExitToPreview => {
            state.mode = Mode::Preview;
            state.selection = None;
            state.visual_selection = None;
        }
        Action::ToggleRawMode => {
            if state.mode == Mode::Preview {
                sync_cursor_to_scroll(state, viewport_height);
            }
            state.visual_selection = None;
            // Capture the cursor's screen row before the mode switch.  The
            // two editing modes use different scroll units (rendered lines
            // vs. buffer lines), so without an adjustment the same `scroll`
            // value lands the user in a different part of the document and
            // the cursor often jumps off-screen.  Skip when transitioning
            // from Preview — there's no editing cursor to anchor on.
            let preserve_screen_row = if state.mode == Mode::Preview {
                None
            } else {
                Some(state.cursor_screen_row(viewport_width))
            };
            let was_raw = state.mode == Mode::Raw;
            state.mode = match state.mode {
                Mode::Preview => Mode::Rendered,
                Mode::Rendered => Mode::Raw,
                Mode::Raw => Mode::Rendered,
                // Diff mode is its own state machine — `ToggleRawMode`
                // is filtered out by `diff_safe_action` so this arm is
                // unreachable in normal dispatch, but stay safe and
                // no-op rather than panic.
                Mode::Diff => Mode::Diff,
            };
            // Raw → Rendered: if the cursor was sitting inside an HTML
            // comment (visible in Raw, invisible in Rendered), snap it to
            // the start of the next visible block so hybrid rendering has
            // a well-defined cursor position.
            if was_raw && state.mode == Mode::Rendered {
                snap_cursor_out_of_hidden_block(state, viewport_width);
                state.update_cursor_block();
            }
            if let Some(row) = preserve_screen_row {
                state.set_scroll_for_cursor_screen_row(row, viewport_width);
                // Don't call `ensure_cursor_visible` here — it's tuned to
                // make the cursor's whole BLOCK fit (it scrolls up if the
                // block starts above `scroll`), which clobbers the helper
                // when the cursor sits inside a tall block (a long
                // paragraph, a table) that legitimately overflows the
                // viewport.  The helper already places the cursor's line
                // at the requested row.
            }
        }

        // ── Cursor movement ───────────────────────────────────────
        Action::MoveLeft => {
            enter_edit_if_preview(state, viewport_height);
            state.selection = None;
            // Raw mode: every character is a valid cursor position — including
            // table borders and the alignment row.  The user owns the risk of
            // breaking formatting by editing the raw source directly.
            if state.mode == Mode::Raw
                || (!table_move_horizontal(state, /*forward=*/ false)
                    && !list_move_horizontal(state, /*forward=*/ false))
            {
                state.cursor.move_left(&state.buffer);
            }
            sync_preferred_visual(state, viewport_width);
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
        }
        Action::MoveRight => {
            enter_edit_if_preview(state, viewport_height);
            state.selection = None;
            if state.mode == Mode::Raw
                || (!table_move_horizontal(state, /*forward=*/ true)
                    && !list_move_horizontal(state, /*forward=*/ true))
            {
                state.cursor.move_right(&state.buffer);
            }
            sync_preferred_visual(state, viewport_width);
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
        }
        Action::MoveUp => {
            if state.mode == Mode::Preview {
                state.scroll_up(1);
                return false;
            }
            state.selection = None;
            if state.mode == Mode::Raw {
                // Plain visual/logical line step — don't skip the alignment row.
                if state.visual_line_nav && viewport_width > 0 {
                    state.move_up_visual(viewport_width);
                } else {
                    state.cursor.move_up(&state.buffer);
                }
            } else if !try_move_cell_vertical(
                state,
                /*down=*/ false,
                viewport_height,
                viewport_width,
            ) {
                move_line_skipping_alignment(state, /*down=*/ false, viewport_width);
            }
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
        }
        Action::MoveDown => {
            if state.mode == Mode::Preview {
                state.scroll_down(1, viewport_height);
                return false;
            }
            state.selection = None;
            if state.mode == Mode::Raw {
                if state.visual_line_nav && viewport_width > 0 {
                    state.move_down_visual(viewport_width);
                } else {
                    state.cursor.move_down(&state.buffer);
                }
            } else if !try_move_cell_vertical(
                state,
                /*down=*/ true,
                viewport_height,
                viewport_width,
            ) {
                move_line_skipping_alignment(state, /*down=*/ true, viewport_width);
            }
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
        }
        Action::MoveWordLeft => {
            enter_edit_if_preview(state, viewport_height);
            state.selection = None;
            state.cursor.move_word_left(&state.buffer);
            sync_preferred_visual(state, viewport_width);
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
        }
        Action::MoveWordRight => {
            enter_edit_if_preview(state, viewport_height);
            state.selection = None;
            state.cursor.move_word_right(&state.buffer);
            sync_preferred_visual(state, viewport_width);
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
        }
        Action::MoveLineStart => {
            enter_edit_if_preview(state, viewport_height);
            state.selection = None;
            state.cursor.move_line_start(&state.buffer);
            sync_preferred_visual(state, viewport_width);
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
        }
        Action::MoveLineEnd => {
            enter_edit_if_preview(state, viewport_height);
            state.selection = None;
            state.cursor.move_line_end(&state.buffer);
            sync_preferred_visual(state, viewport_width);
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
        }
        Action::MoveDocStart => {
            state.selection = None;
            state.cursor.move_doc_start();
            state.update_cursor_block();
            state.scroll_to_top();
        }
        Action::MoveDocEnd => {
            state.selection = None;
            state.cursor.move_doc_end(&state.buffer);
            state.update_cursor_block();
            state.scroll_to_bottom(viewport_height, viewport_width);
            state.ensure_cursor_visible(viewport_height, viewport_width);
        }

        // ── Selection ─────────────────────────────────────────────
        Action::SelectLeft => {
            enter_edit_if_preview(state, viewport_height);
            let anchor = state
                .selection
                .map(|s| s.anchor)
                .unwrap_or(state.cursor.offset);
            state.cursor.move_left(&state.buffer);
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
            state.selection = Some(Selection {
                anchor,
                active: state.cursor.offset,
            });
        }
        Action::SelectRight => {
            enter_edit_if_preview(state, viewport_height);
            let anchor = state
                .selection
                .map(|s| s.anchor)
                .unwrap_or(state.cursor.offset);
            state.cursor.move_right(&state.buffer);
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
            state.selection = Some(Selection {
                anchor,
                active: state.cursor.offset,
            });
        }
        Action::SelectUp => {
            enter_edit_if_preview(state, viewport_height);
            let anchor = state
                .selection
                .map(|s| s.anchor)
                .unwrap_or(state.cursor.offset);
            state.cursor.move_up(&state.buffer);
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
            state.selection = Some(Selection {
                anchor,
                active: state.cursor.offset,
            });
        }
        Action::SelectDown => {
            enter_edit_if_preview(state, viewport_height);
            let anchor = state
                .selection
                .map(|s| s.anchor)
                .unwrap_or(state.cursor.offset);
            state.cursor.move_down(&state.buffer);
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
            state.selection = Some(Selection {
                anchor,
                active: state.cursor.offset,
            });
        }
        Action::SelectAll => {
            if state.mode == Mode::Preview {
                // Preview mode: select the entire rendered document via a
                // `VisualSelection` that spans from the first rendered line
                // (col 0) to the last rendered line's final char.
                let lines = &state.parsed.lines;
                if !lines.is_empty() {
                    let last = lines.len() - 1;
                    let last_col: usize = lines[last]
                        .spans
                        .iter()
                        .map(|s| s.content.chars().count())
                        .sum();
                    state.visual_selection = Some(crate::document::VisualSelection::span(
                        (0, 0),
                        (last, last_col),
                    ));
                }
                return false;
            }
            state.selection = Some(Selection {
                anchor: 0,
                active: state.buffer.len_chars(),
            });
            state.cursor.move_doc_end(&state.buffer);
        }

        // ── Scrolling ─────────────────────────────────────────────
        Action::ScrollUp => state.scroll_up(1),
        Action::ScrollDown => {
            state.scroll_down(1, viewport_height);
            state.clamp_cursor_to_viewport_top();
        }
        Action::ScrollPageUp => state.scroll_up(viewport_height),
        Action::ScrollPageDown => {
            state.scroll_down(viewport_height, viewport_height);
            state.clamp_cursor_to_viewport_top();
        }
        Action::ScrollToTop => state.scroll_to_top(),
        Action::ScrollToBottom => {
            state.scroll_to_bottom(viewport_height, viewport_width);
            // scroll_to_bottom uses viewport-height based max (last line at
            // bottom), so no clamping needed here.
        }

        // ── Editing ───────────────────────────────────────────────
        Action::InsertChar(ch) => {
            if state.mode == Mode::Preview {
                // First keypress from preview: enter edit mode but don't insert.
                sync_cursor_to_scroll(state, viewport_height);
                state.mode = Mode::Rendered;
                return false;
            }
            // Typing `|` inside a table cell must insert an escaped `\|` so
            // the author doesn't inadvertently split the cell.  Outside a
            // table, `|` is a regular character.
            if ch == '|' && cursor_in_table(state) {
                insert_text(state, "\\|");
            } else {
                insert_text(state, &ch.to_string());
            }
        }
        Action::InsertTab => {
            if state.mode == Mode::Preview {
                sync_cursor_to_scroll(state, viewport_height);
                state.mode = Mode::Rendered;
                return false;
            }
            // Tab dispatches by context:
            //   - inside a table → advance to the next cell (auto-creating a
            //     row when pressed in the last cell of the last row)
            //   - inside a list  → indent the current item one level,
            //     producing a new nested list (ordered lists reset to 1)
            //   - otherwise       → insert `INDENT_WIDTH` spaces
            if cursor_in_table(state) {
                table_next_cell(state, viewport_height, viewport_width);
            } else if !list_indent(state) {
                let indent: String = " ".repeat(crate::constants::INDENT_WIDTH);
                insert_text(state, &indent);
            }
        }
        Action::Newline => {
            if state.mode == Mode::Preview {
                sync_cursor_to_scroll(state, viewport_height);
                state.mode = Mode::Rendered;
                return false;
            }
            // Enter inside a table moves the cursor down one row, auto-
            // creating a new row when pressed on the last data row so the
            // user never has to leave the table to append rows.
            if cursor_in_table(state) {
                table_next_row(state, viewport_height, viewport_width);
            } else if !list_handle_newline(state) {
                insert_text(state, "\n");
            }
        }
        Action::DeleteCharBack => {
            enter_edit_if_preview(state, viewport_height);
            if let Some(sel) = state.selection.take() {
                delete_selection(state, sel);
            } else if state.mode == Mode::Rendered && list_backspace_consumes_marker(state) {
                // Handled: the whole marker was deleted as a single atomic edit.
            } else if state.cursor.offset > 0 {
                // Delete the entire preceding grapheme cluster — flag emoji,
                // ZWJ sequences, and combining marks vanish in one keystroke
                // rather than leaving fragments behind.
                let end = state.cursor.offset;
                let offset = prev_grapheme_offset(&state.buffer, end);
                let removed = state.buffer.slice_to_string(offset, end);
                state.apply_delta(EditDelta {
                    offset,
                    removed,
                    inserted: String::new(),
                });
            }
        }
        Action::DeleteCharForward => {
            enter_edit_if_preview(state, viewport_height);
            if let Some(sel) = state.selection.take() {
                delete_selection(state, sel);
            } else if state.cursor.offset < state.buffer.len_chars() {
                let offset = state.cursor.offset;
                let end = next_grapheme_offset(&state.buffer, offset);
                let removed = state.buffer.slice_to_string(offset, end);
                state.apply_delta(EditDelta {
                    offset,
                    removed,
                    inserted: String::new(),
                });
            }
        }
        Action::DeleteWordBack => {
            enter_edit_if_preview(state, viewport_height);
            state.selection = None;
            let end = state.cursor.offset;
            // Move left: skip whitespace, then skip word chars.
            let mut temp = state.cursor;
            temp.move_word_left(&state.buffer);
            let start = temp.offset;
            if start < end {
                let removed = state.buffer.slice_to_string(start, end);
                state.cursor.offset = start;
                state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
                state.apply_delta(EditDelta {
                    offset: start,
                    removed,
                    inserted: String::new(),
                });
            }
        }
        Action::DeleteWordForward => {
            enter_edit_if_preview(state, viewport_height);
            state.selection = None;
            let start = state.cursor.offset;
            let mut temp = state.cursor;
            // Skip non-whitespace (the word), then skip trailing whitespace.
            temp.move_word_right(&state.buffer);
            let end = temp.offset;
            if start < end {
                let removed = state.buffer.slice_to_string(start, end);
                state.apply_delta(EditDelta {
                    offset: start,
                    removed,
                    inserted: String::new(),
                });
            }
        }
        Action::DeleteLine => {
            enter_edit_if_preview(state, viewport_height);
            state.selection = None;
            let (line, _) = state.cursor.line_col(&state.buffer);
            let start = state.buffer.line_to_char(line);
            let end = if line + 1 < state.buffer.line_count() {
                state.buffer.line_to_char(line + 1)
            } else {
                state.buffer.len_chars()
            };
            if start < end {
                let removed = state.buffer.slice_to_string(start, end);
                state.cursor.offset = start.min(state.buffer.len_chars().saturating_sub(1));
                state.apply_delta(EditDelta {
                    offset: start,
                    removed,
                    inserted: String::new(),
                });
            }
        }

        // ── History ───────────────────────────────────────────────
        Action::Undo => {
            if let Some(offset) = state.history.undo(&mut state.buffer) {
                state.cursor.offset = offset.min(state.buffer.len_chars());
                state.refresh_parsed();
                // Don't clear dirty here: the buffer may still differ from disk.
                state.ensure_cursor_visible(viewport_height, viewport_width);
            }
        }
        Action::Redo => {
            if let Some(offset) = state.history.redo(&mut state.buffer) {
                state.cursor.offset = offset.min(state.buffer.len_chars());
                state.refresh_parsed();
                state.ensure_cursor_visible(viewport_height, viewport_width);
            }
        }

        // ── Clipboard ─────────────────────────────────────────────
        Action::Copy => {
            // In Preview mode, the selection is over rendered characters
            // (no raw Markdown markers), so copy the rendered text exactly
            // as the user sees it.  In Rendered/Raw mode, copy the raw
            // buffer slice covered by the raw selection.
            let text = if state.mode == Mode::Preview {
                if let Some(vs) = state.visual_selection {
                    crate::editor::mouse_ops::visual_selection_to_rendered_text(
                        vs,
                        &state.parsed.lines,
                    )
                } else {
                    // No selection — copy the rendered line under the scroll top.
                    state
                        .parsed
                        .lines
                        .get(state.scroll)
                        .map(|line| {
                            line.spans
                                .iter()
                                .flat_map(|s| s.content.chars())
                                .collect::<String>()
                        })
                        .unwrap_or_default()
                }
            } else if let Some(sel) = &state.selection {
                sel.selected_text(&state.buffer)
            } else {
                // Copy current line.
                let (line, _) = state.cursor.line_col(&state.buffer);
                state.buffer.line(line).unwrap_or_default()
            };
            copy_to_clipboard(state, text);
        }
        Action::Cut => {
            if let Some(sel) = state.selection.take() {
                let text = sel.selected_text(&state.buffer);
                copy_to_clipboard(state, text.clone());
                delete_selection_text(state, &sel);
            } else {
                // Cut current line.
                let (line, _) = state.cursor.line_col(&state.buffer);
                let start = state.buffer.line_to_char(line);
                let end = if line + 1 < state.buffer.line_count() {
                    state.buffer.line_to_char(line + 1)
                } else {
                    state.buffer.len_chars()
                };
                if start < end {
                    let text = state.buffer.slice_to_string(start, end);
                    copy_to_clipboard(state, text.clone());
                    state.cursor.offset = start;
                    state.apply_delta(EditDelta {
                        offset: start,
                        removed: text,
                        inserted: String::new(),
                    });
                }
            }
        }
        Action::Paste => {
            enter_edit_if_preview(state, viewport_height);
            let text = clipboard_text(state);
            if !text.is_empty() {
                if let Some(sel) = state.selection.take() {
                    let (start, end) = sel.range();
                    let removed = state
                        .buffer
                        .slice_to_string(start, end.min(state.buffer.len_chars()));
                    state.cursor.offset = start;
                    state.apply_delta(EditDelta {
                        offset: start,
                        removed,
                        inserted: text,
                    });
                } else {
                    insert_text(state, &text);
                }
                // Ordered-list renumbering runs automatically at end of
                // `apply()` when the buffer has changed.
            }
        }

        // ── Formatting ────────────────────────────────────────────
        Action::BoldSelection => toggle_wrap(state, "**"),
        Action::ItalicizeSelection => toggle_wrap(state, "*"),
        Action::InlineCodeSelection => toggle_wrap(state, "`"),
        Action::StrikethroughSelection => toggle_wrap(state, "~~"),
        Action::HighlightSelection => toggle_wrap(state, "=="),

        // ── File operations ───────────────────────────────────────
        // `Action::Save` is intercepted by `App::handle_app_action`
        // before this dispatch is reached — see `App::save_buffer`
        // for the single call site of `Buffer::save_file`.
        // ── List editing ─────────────────────────────────────────
        Action::ToggleCheckbox => {
            enter_edit_if_preview(state, viewport_height);
            list_toggle_checkbox(state);
        }

        // ── Table editing ────────────────────────────────────────
        Action::TableNextCell => {
            enter_edit_if_preview(state, viewport_height);
            if cursor_in_table(state) {
                table_next_cell(state, viewport_height, viewport_width);
            }
        }
        Action::TablePrevCell => {
            enter_edit_if_preview(state, viewport_height);
            if cursor_in_table(state) {
                table_prev_cell(state, viewport_height, viewport_width);
            } else {
                // Shift+Tab in a list outdents the current item by one level
                // (removes up to `INDENT_WIDTH` leading spaces).  Outside a
                // list it's a no-op.
                list_outdent(state);
            }
        }
        Action::TableNextRow => {
            enter_edit_if_preview(state, viewport_height);
            if cursor_in_table(state) {
                table_next_row(state, viewport_height, viewport_width);
            }
        }
        Action::TablePrevRow => {
            enter_edit_if_preview(state, viewport_height);
            if cursor_in_table(state) {
                table_prev_row(state, viewport_height, viewport_width);
            }
        }
        Action::TableMoveRowUp => {
            table_move_row(state, /*down=*/ false, viewport_height, viewport_width);
        }
        Action::TableMoveRowDown => {
            table_move_row(state, /*down=*/ true, viewport_height, viewport_width);
        }
        Action::TableMoveColumnLeft => {
            table_move_column(
                state,
                /*right=*/ false,
                viewport_height,
                viewport_width,
            );
        }
        Action::TableMoveColumnRight => {
            table_move_column(state, /*right=*/ true, viewport_height, viewport_width);
        }
        Action::TableInsertRowAbove => {
            table_insert_row(
                state,
                /*below=*/ false,
                viewport_height,
                viewport_width,
            );
        }
        Action::TableInsertRowBelow => {
            table_insert_row(state, /*below=*/ true, viewport_height, viewport_width);
        }
        Action::TableInsertColumnLeft => {
            table_insert_column(
                state,
                /*right=*/ false,
                viewport_height,
                viewport_width,
            );
        }
        Action::TableInsertColumnRight => {
            table_insert_column(state, /*right=*/ true, viewport_height, viewport_width);
        }
        Action::TableDeleteRow => {
            table_delete_row(state, viewport_height, viewport_width);
        }
        Action::TableDeleteColumn => {
            table_delete_column(state, viewport_height, viewport_width);
        }
        Action::TableInsertBreak => {
            enter_edit_if_preview(state, viewport_height);
            if cursor_in_table(state) {
                // Inside a table, Shift+Enter inserts a GFM `<br>` so the
                // cell can contain a visual line break without terminating
                // the row.
                insert_text(state, "<br>");
            } else {
                // Outside a table, fall back to a normal newline.
                insert_text(state, "\n");
            }
        }
        _ => {}
    }

    // After any action that mutated the buffer (detected by a change in length
    // or a new history entry), run a renumber pass so the raw Markdown of any
    // surrounding ordered list stays monotonic.  We skip this for Undo/Redo so
    // those actions remain exact inverses of the recorded deltas, and we skip
    // it when the mode is not Rendered (Raw mode is deliberately raw).
    let edited = state.buffer.len_chars() != buffer_len_before
        || state.history.undo_depth() != history_depth_before;
    if !suppress_autonumber && state.mode == Mode::Rendered && edited {
        list_renumber_at_cursor(state);
    }

    // After any action that may have left the cursor on a list marker
    // (`- `, `1. `, `- [ ] `, …) — e.g. `DeleteLine` positions the cursor at
    // the start of the following line, which is the marker — snap it onto the
    // item's content start so the user's next keystroke lands where they see
    // the caret, not in the marker.  Skipped in Raw mode: there the cursor
    // is expected to reach every byte.
    if state.mode == Mode::Rendered && !suppress_autonumber {
        clamp_cursor_out_of_marker(state);
    }

    // After an edit, the cursor may have moved onto a new line or onto a
    // newly-wrapped visual row past the viewport bottom (e.g. typing the
    // character that pushes the line into a second visual row).  Pull
    // scroll along so the cursor stays visible.  The cursor-movement
    // arms above already do this for navigation actions; this catches
    // pure edit actions (InsertChar / Newline / Backspace / …) which
    // don't.
    //
    // In Rendered mode, `ensure_cursor_visible` reads `parsed.lines` and
    // the visual-row cache to detect wrap.  In-line edits leave both
    // stale (the deferred-reparse optimization), so the wrap check would
    // miss the visual row the just-typed char produced.  Flush before
    // the visibility check.  Raw mode reads the live buffer directly,
    // so no flush is needed there.
    if edited && state.mode != Mode::Preview {
        if state.mode != Mode::Raw {
            state.flush_parsed_if_dirty();
        }
        state.ensure_cursor_visible(viewport_height, viewport_width);
    }

    false
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn enter_edit_if_preview(state: &mut EditorState, viewport_height: usize) {
    if state.mode == Mode::Preview {
        sync_cursor_to_scroll(state, viewport_height);
        state.mode = Mode::Rendered;
        state.visual_selection = None;
    }
}

/// After a horizontal cursor movement, if visual-line navigation is enabled,
/// replace the raw `preferred_col` (set by the cursor method) with the visual
/// column on the current visual sub-line.  This keeps the "preferred column"
/// that subsequent vertical moves try to maintain aligned with what the user
/// actually sees on screen.
fn sync_preferred_visual(state: &mut EditorState, viewport_width: usize) {
    if state.visual_line_nav && viewport_width > 0 {
        state.cursor.preferred_col = state.current_visual_col(viewport_width);
    }
}

/// Move the cursor to the start of the block at the current scroll position,
/// but only if the cursor is not already within the visible viewport.
/// Called when transitioning from Preview mode to an editing mode so the
/// cursor appears near the visible area rather than wherever it last was.
fn sync_cursor_to_scroll(state: &mut EditorState, viewport_height: usize) {
    let scroll = state.scroll;
    // If the cursor's rendered position is already within the viewport, leave it.
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    let cursor_lines = state.parsed.source_map.rendered_lines_for_byte(cursor_byte);
    if !cursor_lines.is_empty() {
        let visible_end = scroll + viewport_height;
        if cursor_lines.start >= scroll && cursor_lines.start < visible_end {
            return;
        }
    }
    // Cursor is outside the visible area; move it to the first visible block.
    if let Some(byte) = state
        .parsed
        .source_map
        .original_byte_for_rendered_line(scroll)
    {
        let char_offset = state.buffer.rope().byte_to_char(byte);
        state.cursor.offset = char_offset.min(state.buffer.len_chars());
    }
}

/// Coalesced version of a run of `Action::InsertChar(c)` events.  Builds
/// a single string from `chars` (with the same table-pipe escaping that
/// the per-keystroke `Action::InsertChar` path applies) and routes it
/// through `state.apply_delta` as ONE delta — so a held-key autorepeat
/// burst becomes one buffer mutation, one history entry, and one
/// `parsed_version` bump instead of N.
///
/// Preconditions (enforced by the run-membership predicate in the
/// dispatcher):
/// * `chars` is non-empty.
/// * `state.mode != Mode::Preview` — Preview's first keystroke transitions
///   without inserting; the caller dispatches that single event normally
///   and only the post-transition events flow through here.
/// * `state.selection` is `None` — a selection-deleting insert ends its
///   own run before coalescing extends it.
pub fn apply_insert_run(
    state: &mut EditorState,
    chars: &[char],
    viewport_height: usize,
    viewport_width: usize,
) {
    if chars.is_empty() {
        return;
    }
    let in_table = cursor_in_table(state);
    let mut text = String::with_capacity(chars.len());
    for &ch in chars {
        if ch == '|' && in_table {
            text.push_str("\\|");
        } else {
            text.push(ch);
        }
    }
    insert_text(state, &text);

    // Mirror the post-action upkeep from `apply()`: renumber ordered
    // lists when typing inside one, snap the cursor off any list marker
    // it may have landed on, and re-flush + ensure-visible so the
    // cursor stays on-screen even when the burst added a wrapped row.
    if state.mode == Mode::Rendered {
        list_renumber_at_cursor(state);
        clamp_cursor_out_of_marker(state);
    }
    if state.mode != Mode::Raw {
        state.flush_parsed_if_dirty();
    }
    state.ensure_cursor_visible(viewport_height, viewport_width);
}

/// Coalesced version of a run of `Action::DeleteCharBack` /
/// `Action::DeleteCharForward` events.  Walks `count` graphemes from the
/// current cursor position in the requested direction and removes them
/// as a single delta.
///
/// Preconditions (enforced by the dispatcher):
/// * `count >= 1`.
/// * `state.mode != Mode::Preview`.
/// * `state.selection` is `None`.
///
/// List-marker erase and task-checkbox erase are NOT short-circuited
/// here — those are one-shot transitions, not autorepeat candidates.
/// The dispatcher arranges for the first delete to fire through the
/// regular `apply()` path, where `list_backspace_consumes_marker` runs;
/// any subsequent same-kind events fall into this run.
pub fn apply_delete_run(
    state: &mut EditorState,
    count: usize,
    backward: bool,
    viewport_height: usize,
    viewport_width: usize,
) {
    if count == 0 {
        return;
    }
    let buffer_len = state.buffer.len_chars();
    let (start, end) = if backward {
        let mut off = state.cursor.offset;
        for _ in 0..count {
            if off == 0 {
                break;
            }
            off = prev_grapheme_offset(&state.buffer, off);
        }
        (off, state.cursor.offset)
    } else {
        let mut off = state.cursor.offset;
        for _ in 0..count {
            if off >= buffer_len {
                break;
            }
            off = next_grapheme_offset(&state.buffer, off);
        }
        (state.cursor.offset, off)
    };
    if start >= end {
        return;
    }
    let removed = state.buffer.slice_to_string(start, end);
    // `apply_delta` sets cursor to `delta.redo_cursor()` (= `start`,
    // since `inserted` is empty), which is the correct post-delete
    // position for both backward and forward — no manual cursor
    // pre-set needed.
    state.apply_delta(EditDelta {
        offset: start,
        removed,
        inserted: String::new(),
    });

    if state.mode == Mode::Rendered {
        list_renumber_at_cursor(state);
        clamp_cursor_out_of_marker(state);
    }
    if state.mode != Mode::Raw {
        state.flush_parsed_if_dirty();
    }
    state.ensure_cursor_visible(viewport_height, viewport_width);
}

/// Insert `text` at the current cursor position, pushing through history.
fn insert_text(state: &mut EditorState, text: &str) {
    let offset = state.cursor.offset;
    // If there's a selection, replace it.
    if let Some(sel) = state.selection.take() {
        let (start, end) = sel.range();
        let removed = state
            .buffer
            .slice_to_string(start, end.min(state.buffer.len_chars()));
        state.cursor.offset = start;
        state.apply_delta(EditDelta {
            offset: start,
            removed,
            inserted: text.to_owned(),
        });
    } else {
        state.apply_delta(EditDelta {
            offset,
            removed: String::new(),
            inserted: text.to_owned(),
        });
    }
}

/// Delete the text in `sel` from the buffer.
fn delete_selection(state: &mut EditorState, sel: Selection) {
    delete_selection_text(state, &sel);
}

fn delete_selection_text(state: &mut EditorState, sel: &Selection) {
    let (start, end) = sel.range();
    let end = end.min(state.buffer.len_chars());
    if start < end {
        let removed = state.buffer.slice_to_string(start, end);
        state.cursor.offset = start;
        state.apply_delta(EditDelta {
            offset: start,
            removed,
            inserted: String::new(),
        });
    }
}

/// Wrap the active selection in `marker` (`**` for bold, `*` for italic),
/// or unwrap it when the selection is already exactly that emphasis.
/// Re-selects the inner text afterward so wraps can be chained (e.g. bold
/// then italic).  No-op without a non-empty selection — which is the case
/// in Preview mode, where the live selection is `None`.
///
/// Three unwrap/wrap shapes are recognized:
/// * inner markers included — `**x**` selected → `x`;
/// * markers just outside the selection — `x` selected within `**x**` → `x`;
/// * otherwise plain wrap — `x` → `**x**`.
///
/// Selections that span a block boundary (contain a newline) are refused:
/// CommonMark emphasis can't cross a blank line, so wrapping them would
/// only emit literal asterisks.  Selections that *contain* other inline
/// formatting are wrapped verbatim — handling those correctly needs an
/// AST-aware transform, which is out of scope for this convenience action.
fn toggle_wrap(state: &mut EditorState, marker: &str) {
    let Some(sel) = state.selection else {
        return;
    };
    let (start, end) = sel.range();
    let end = end.min(state.buffer.len_chars());
    if start >= end {
        return;
    }
    let text = state.buffer.slice_to_string(start, end);
    if text.contains('\n') {
        return;
    }

    // Markers are ASCII, so byte length == char count — the same value is
    // valid for both rope-char offsets and `&str` byte slicing.
    let mlen = marker.len();

    let (remove_start, removed, inserted, inner_start, inner_len) =
        if is_marker_wrapped(&text, marker) {
            // Selection includes the markers: `**x**` → `x`.
            let inner = text[mlen..text.len() - mlen].to_string();
            let n = inner.chars().count();
            (start, text, inner, start, n)
        } else if outside_wrapped(&state.buffer, start, end, marker) {
            // Markers sit just outside the selection: `x` within `**x**` → `x`.
            let removed = state.buffer.slice_to_string(start - mlen, end + mlen);
            let n = text.chars().count();
            (start - mlen, removed, text, start - mlen, n)
        } else {
            // Plain wrap: `x` → `**x**`.
            let n = text.chars().count();
            let inserted = format!("{marker}{text}{marker}");
            (start, text, inserted, start + mlen, n)
        };

    state.apply_delta(EditDelta {
        offset: remove_start,
        removed,
        inserted,
    });

    let inner_end = inner_start + inner_len;
    state.selection = Some(Selection {
        anchor: inner_start,
        active: inner_end,
    });
    state.cursor.offset = inner_end;
}

/// True when `text` is *exactly* `marker…marker` with no further `marker`
/// in between.  For italic (`*`) the bold case (`**…**`) is rejected so
/// toggling italic over bold text wraps it rather than stripping one of the
/// two bold markers.  The interior-marker check keeps a selection that
/// merely *starts and ends* with emphasis — e.g. `**a** and **b**` — from
/// being mis-unwrapped into malformed markdown; it falls through to a
/// verbatim wrap instead.
fn is_marker_wrapped(text: &str, marker: &str) -> bool {
    let mlen = marker.len();
    if text.len() < 2 * mlen || !text.starts_with(marker) || !text.ends_with(marker) {
        return false;
    }
    if marker == "*" && (text.starts_with("**") || text.ends_with("**")) {
        return false;
    }
    // Reject interior markers — `**x**` unwraps, but `**a** and **b**` must not.
    !text[mlen..text.len() - mlen].contains(marker)
}

/// True when the buffer has `marker` immediately outside `[start, end)` on
/// both sides — i.e. the selection's inner text is already wrapped.  For
/// italic (`*`) the bold case (`**…**`) is rejected, mirroring
/// [`is_marker_wrapped`].
fn outside_wrapped(buf: &Buffer, start: usize, end: usize, marker: &str) -> bool {
    let mlen = marker.len();
    if start < mlen || end + mlen > buf.len_chars() {
        return false;
    }
    if buf.slice_to_string(start - mlen, start) != marker
        || buf.slice_to_string(end, end + mlen) != marker
    {
        return false;
    }
    if marker == "*" {
        // A `*` immediately beyond the candidate marker means the real
        // markers are `**` (bold), not `*` (italic).
        let bold_before = start >= 2 && buf.slice_to_string(start - 2, start - 1) == "*";
        let bold_after = end + 2 <= buf.len_chars() && buf.slice_to_string(end + 1, end + 2) == "*";
        if bold_before || bold_after {
            return false;
        }
    }
    true
}

/// Write `text` to the OS clipboard (best-effort via arboard AND OSC 52)
/// and always mirror into the in-process kill-ring so internal paste still
/// works when neither external path is available.
fn copy_to_clipboard(state: &mut EditorState, text: String) {
    #[cfg(feature = "clipboard")]
    {
        copy_to_system_clipboard(text.clone());
    }
    // OSC 52 also reaches the terminal emulator's clipboard — the only path
    // that works over SSH, on Wayland without `wayland-data-control`, and in
    // WSL.  Any terminal that doesn't understand the escape silently ignores
    // it, so emitting unconditionally is safe.
    osc52_copy(&text);
    state.kill_ring = text;
}

/// Platform-aware copy.  On Linux the Wayland/X11 clipboard only holds data
/// while a process owns the selection, and arboard prints a warning to
/// *stderr* if the `Clipboard` is dropped too quickly after `set_text` —
/// which corrupts the TUI.  Spawn a thread that owns the clipboard until
/// another program takes over (or until the process exits).
///
/// On macOS and Windows the OS clipboard persists across process exit, so
/// the simple path is fine and we don't need a background thread.
#[cfg(all(feature = "clipboard", target_os = "linux"))]
fn copy_to_system_clipboard(text: String) {
    use arboard::SetExtLinux;
    std::thread::spawn(move || {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set().wait().text(text);
        }
    });
}

#[cfg(all(feature = "clipboard", not(target_os = "linux")))]
fn copy_to_system_clipboard(text: String) {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(&text);
    }
}

/// Write `text` to the terminal emulator's clipboard via the OSC 52 escape
/// sequence (`ESC ] 52 ; c ; <base64> BEL`).  Works across SSH, Wayland and
/// WSL as long as the host terminal supports the escape; unsupported
/// terminals silently ignore it.
fn osc52_copy(text: &str) {
    use std::io::Write;
    let encoded = base64_encode(text.as_bytes());
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "\x1b]52;c;{encoded}\x07");
    let _ = stdout.flush();
}

/// Minimal RFC-4648 base64 encoder.  Written out by hand to avoid adding a
/// dependency for a one-caller helper.
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(CHARS[(b0 >> 2) as usize] as char);
        out.push(CHARS[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(b2 & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Read from the OS clipboard if available; fall back to kill-ring.
/// Public so a caller that needs to reshape the payload before it lands
/// — the App's vim VisualLine paste, which makes it linewise — can read
/// the same source the plain `Action::Paste` arm does.
pub fn clipboard_text(state: &EditorState) -> String {
    #[cfg(feature = "clipboard")]
    {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if let Ok(text) = cb.get_text() {
                return text;
            }
        }
    }
    state.kill_ring.clone()
}

/// Insert `text` at the cursor (or over the current selection) as if it came
/// from a paste action.  Used by the bracketed-paste handler in `App` so
/// terminal-level pastes (Ctrl-Shift-V, right-click-paste, etc.) land in the
/// buffer without needing the OS clipboard to be reachable from this process.
pub fn paste_text(
    state: &mut EditorState,
    text: &str,
    viewport_height: usize,
    viewport_width: usize,
) {
    if text.is_empty() {
        return;
    }
    let buffer_len_before = state.buffer.len_chars();
    let history_depth_before = state.history.undo_depth();

    enter_edit_if_preview(state, viewport_height);
    if let Some(sel) = state.selection.take() {
        let (start, end) = sel.range();
        let removed = state
            .buffer
            .slice_to_string(start, end.min(state.buffer.len_chars()));
        state.cursor.offset = start;
        state.apply_delta(EditDelta {
            offset: start,
            removed,
            inserted: text.to_owned(),
        });
    } else {
        insert_text(state, text);
    }

    let edited = state.buffer.len_chars() != buffer_len_before
        || state.history.undo_depth() != history_depth_before;
    if state.mode == Mode::Rendered && edited {
        list_renumber_at_cursor(state);
    }
    if state.mode == Mode::Rendered {
        clamp_cursor_out_of_marker(state);
    }
    state.ensure_cursor_visible(viewport_height, viewport_width);
}

/// Wrapper around [`table_edit::insert_table`] that mutates
/// `EditorState` and lands the cursor in the new table's first header
/// cell.  The caller is expected to have run the blank-line pre-flight
/// already (the App-level handler does this before opening the modal),
/// so this function unconditionally inserts.  In Preview mode the
/// caller flips the editor into Rendered first, mirroring how typing
/// actions transition out of Preview.
pub fn insert_table_at_cursor(
    state: &mut EditorState,
    rows: usize,
    cols: usize,
    viewport_height: usize,
    viewport_width: usize,
) {
    enter_edit_if_preview(state, viewport_height);
    let source = state.buffer.contents();
    let cursor_byte = cursor_byte(state);
    let (byte_delta, cursor_target) = table_edit::insert_table(&source, cursor_byte, rows, cols);
    apply_byte_delta(state, byte_delta, cursor_target);
    state.ensure_cursor_visible(viewport_height, viewport_width);
}

/// Placeholder text shared by the image / link snippets.  Left selected
/// after a selection-wrapping insert so the user's next keystrokes
/// replace it with the real destination.
pub const URL_PLACEHOLDER: &str = "file path or URL";

/// True when the block under the cursor can host inline Markdown — an
/// image or link snippet inserted there will parse as markup rather
/// than literal text.  Denies code blocks, raw HTML, HTML comments,
/// horizontal rules, and existing image / diagram blocks; a blank line
/// (no real block) and every inline-bearing block (paragraph, heading,
/// list, quote, table, footnote definition) are allowed.
///
/// Classification is by *top-level* block: a code fence nested inside
/// a list item or block quote is not detected.  That matches the
/// fidelity of the other location guards (`cursor_line_is_blank`,
/// `cursor_in_table`), which also reason at the top level.
// Library-only surface: the snippet inserts run the offset-based guard
// internally, so the binary never calls the cursor-based wrapper — it
// exists for the block-classification integration tests.
#[allow(dead_code)]
pub fn cursor_block_allows_inline_markdown(state: &mut EditorState) -> bool {
    let offset = state.cursor.offset;
    block_allows_inline_markdown_at(state, offset)
}

/// Offset-based body of [`cursor_block_allows_inline_markdown`] — the
/// snippet insert classifies at the offset the snippet will actually
/// land on (the selection start when wrapping), which is not always
/// the cursor.
fn block_allows_inline_markdown_at(state: &mut EditorState, char_offset: usize) -> bool {
    use crate::markdown::Block;
    // An in-line typing burst defers the re-parse; flush so the block
    // classification below can't run against stale ranges.
    state.flush_parsed_if_dirty();
    let byte = state.buffer.rope().char_to_byte(char_offset);
    let Some(block) = state.parsed.real_block_for_byte(byte) else {
        // Blank line (a virtual block) or EOF — a snippet here becomes
        // its own paragraph, the ideal spot for an image.
        return true;
    };
    !matches!(
        block,
        Block::CodeBlock { .. }
            | Block::Html(_)
            | Block::HtmlComment(_)
            | Block::HorizontalRule
            | Block::ImageBlock { .. }
    )
}

/// Insert an image snippet (`![alt text](file path or URL)`) at the
/// cursor.  See [`insert_inline_snippet`] for the selection-wrapping,
/// placeholder-selection, and pre-flight behavior.
pub fn insert_image_at_cursor(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    insert_inline_snippet(state, "!", "alt text", viewport_height, viewport_width)
}

/// Insert a link snippet (`[link text](file path or URL)`) at the
/// cursor.  See [`insert_inline_snippet`] for the selection-wrapping,
/// placeholder-selection, and pre-flight behavior.
pub fn insert_link_at_cursor(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    insert_inline_snippet(state, "", "link text", viewport_height, viewport_width)
}

/// Shared body of the image / link snippet inserts.  Returns `false` —
/// leaving mode, selection, and buffer untouched — when the target
/// block can't host inline Markdown (see
/// [`cursor_block_allows_inline_markdown`]); the App-level handler
/// flashes a warning on that path.  The pre-flight runs here, after the
/// Preview cursor→scroll sync and against the actual insert offset, so
/// the block it classifies is always the block the snippet lands in.
///
/// - With a single-line selection, the selected text becomes the
///   visible text — `sel` → `{prefix}[sel](file path or URL)` — and the
///   URL placeholder is left selected so the user types the destination
///   next (typing replaces a selection, matching [`toggle_wrap`]).
/// - Otherwise the full snippet is inserted at the cursor with the
///   text placeholder selected instead.  A multi-line selection is
///   dropped rather than wrapped (link text can't span blocks) so no
///   buffer text is destroyed.
fn insert_inline_snippet(
    state: &mut EditorState,
    prefix: &str,
    text_placeholder: &str,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    // In Preview the cursor may be far from the viewport; sync it to the
    // scroll position *before* the pre-flight so the guard classifies
    // the block the snippet will actually land in.  Deliberately not
    // `enter_edit_if_preview` yet — a denied insert must not leave
    // Preview.
    if state.mode == Mode::Preview {
        sync_cursor_to_scroll(state, viewport_height);
    }
    let wrap = state.selection.as_ref().and_then(|sel| {
        let (start, end) = sel.range();
        let end = end.min(state.buffer.len_chars());
        if start >= end {
            return None;
        }
        let text = state.buffer.slice_to_string(start, end);
        if text.contains('\n') {
            return None;
        }
        Some((start, text))
    });
    let insert_at = wrap
        .as_ref()
        .map_or(state.cursor.offset, |(start, _)| *start);
    if !block_allows_inline_markdown_at(state, insert_at) {
        return false;
    }
    state.selection = None;
    enter_edit_if_preview(state, viewport_height);
    // `prefix`, the brackets, and the placeholders are all ASCII, so
    // their byte lengths double as char counts; only the wrapped
    // selection text needs a `chars().count()`.
    let (offset, removed, visible_text, select_placeholder_url) = match wrap {
        Some((start, text)) => (start, text.clone(), text, true),
        None => (
            state.cursor.offset,
            String::new(),
            text_placeholder.to_owned(),
            false,
        ),
    };
    let inserted = format!("{prefix}[{visible_text}]({URL_PLACEHOLDER})");
    state.cursor.offset = offset;
    state.apply_delta(EditDelta {
        offset,
        removed,
        inserted,
    });
    let (sel_start, sel_len) = if select_placeholder_url {
        (
            offset + prefix.len() + 1 + visible_text.chars().count() + 2,
            URL_PLACEHOLDER.len(),
        )
    } else {
        (offset + prefix.len() + 1, text_placeholder.len())
    };
    let sel_end = sel_start + sel_len;
    state.selection = Some(Selection {
        anchor: sel_start,
        active: sel_end,
    });
    state.cursor.offset = sel_end;
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
    state.update_cursor_block();
    state.ensure_cursor_visible(viewport_height, viewport_width);
    true
}

/// Insert an auto-numbered `[^N]` footnote reference at the cursor.  Only
/// the reference is inserted — the user writes the definition wherever
/// they want.  Until a matching `[^N]:` definition exists the marker
/// renders as literal text (CommonMark treats an undefined reference as
/// plain text).
pub fn insert_footnote_at_cursor(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) {
    enter_edit_if_preview(state, viewport_height);
    let source = state.buffer.contents();
    let cursor_byte = cursor_byte(state);
    let (delta, cursor_target) = footnote_edit::insert_footnote(&source, cursor_byte);
    apply_byte_delta(state, delta, cursor_target);
    state.ensure_cursor_visible(viewport_height, viewport_width);
}

/// Re-sequence all numeric footnotes into order-of-first-reference.  Named
/// labels are left untouched.  Returns `false` (no edit) when nothing
/// needs renumbering.
pub fn renumber_footnotes(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    let source = state.buffer.contents();
    let Some(delta) = footnote_edit::renumber_footnotes(&source) else {
        return false;
    };
    let cursor_target = cursor_byte(state);
    apply_byte_delta(state, delta, cursor_target);
    state.ensure_cursor_visible(viewport_height, viewport_width);
    true
}

/// Delete the footnote at the cursor — every reference plus its definition
/// line — and renumber the remainder, as one undoable edit.  Returns
/// `false` when the cursor isn't on a footnote.
pub fn delete_footnote_at_cursor(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    let source = state.buffer.contents();
    let cursor_byte = cursor_byte(state);
    let Some(label) = footnote_edit::label_at(&source, cursor_byte) else {
        return false;
    };
    let Some(delta) = footnote_edit::delete_footnote(&source, &label) else {
        return false;
    };
    let cursor_target = delta.offset;
    apply_byte_delta(state, delta, cursor_target);
    state.ensure_cursor_visible(viewport_height, viewport_width);
    true
}

/// Set the cursor to a specific byte offset in the buffer.  Clamps to
/// buffer bounds and keeps `preferred_col` coherent.
pub(super) fn set_cursor_byte(state: &mut EditorState, target_byte: usize) {
    let source_len = state.buffer.contents().len();
    let clamped = target_byte.min(source_len);
    let char_off = state.buffer.rope().byte_to_char(clamped);
    state.cursor.offset = char_off.min(state.buffer.len_chars());
}

/// Char offset of the cursor, as a byte offset into `buffer.contents()`.
/// Visible to sibling editor modules so byte-oriented helpers (`table_edit`,
/// `list_edit`) can read the cursor's source position without re-deriving it.
pub(super) fn cursor_byte(state: &EditorState) -> usize {
    state.buffer.rope().char_to_byte(state.cursor.offset)
}

/// Move the cursor up/down by one line in a rendered view, skipping a
/// table's alignment row and any hidden (zero-rendered-line) blocks so the
/// cursor never stalls on a structural artefact.  Honours `visual_line_nav`
/// (the default handler's wrapped-line nav).  The shared skip/step logic
/// lives on [`EditorState::move_cursor_line`], which also gates the skip on
/// the rendered-vs-`Raw` view so vim `j`/`k` and this path stay in lockstep.
fn move_line_skipping_alignment(state: &mut EditorState, down: bool, viewport_width: usize) {
    let visual = state.visual_line_nav;
    state.move_cursor_line(down, visual, viewport_width);
}

/// True iff the cursor currently falls inside a block with zero rendered
/// lines whose source text is an HTML comment.  Used by vertical-navigation
/// and mode-transition code to skip over invisible source bytes so the
/// cursor never stalls on a line the user can't see in hybrid view.
///
/// Intentionally specific to comments rather than "any zero-own block":
/// suppressed blank lines (when `preserve_blank_lines` is false) also have
/// zero own but are bytes the cursor may legitimately want to land on.
pub(super) fn cursor_on_hidden_block(state: &EditorState) -> bool {
    let rope = state.buffer.rope();
    let cursor_byte = rope.char_to_byte(state.cursor.offset);
    let Some(block_idx) = state.parsed.source_map.block_for_byte(cursor_byte) else {
        return false;
    };
    if state.parsed.block_own_line_count(block_idx) > 0 {
        return false;
    }
    let Some(range) = state.parsed.source_map.original_range_for_byte(cursor_byte) else {
        return false;
    };
    let source = state.buffer.contents();
    let end = range.end.min(source.len());
    source[range.start..end].trim_start().starts_with("<!--")
}

/// Walk the cursor forward past any hidden (HTML-comment) blocks so
/// subsequent rendering logic can assume the cursor's block has at least
/// one rendered line.  Called on the Raw → Rendered / Preview mode
/// transition, where the cursor may have been sitting inside a comment
/// that's invisible in the destination mode.
fn snap_cursor_out_of_hidden_block(state: &mut EditorState, viewport_width: usize) {
    let mut safety = 32usize;
    while cursor_on_hidden_block(state) && safety > 0 {
        let prev_offset = state.cursor.offset;
        if state.visual_line_nav && viewport_width > 0 {
            state.move_down_visual(viewport_width);
        } else {
            state.cursor.move_down(&state.buffer);
        }
        if state.cursor.offset == prev_offset {
            // Already at the buffer's end — nowhere to skip to.  Leave the
            // cursor where it is; the rendered view falls back gracefully.
            break;
        }
        safety -= 1;
    }
}

/// Apply a `table_edit`-produced `EditDelta` (whose offsets are **bytes**)
/// to the editor state by first converting those offsets into rope char
/// offsets.  The caller supplies the *post-edit* cursor byte position; we
/// convert it too after the buffer has been mutated.
pub(super) fn apply_byte_delta(
    state: &mut EditorState,
    byte_delta: EditDelta,
    cursor_byte_target: usize,
) {
    // Convert byte offsets → char offsets using the *pre-edit* rope.
    let offset_char = state.buffer.rope().byte_to_char(byte_delta.offset);
    let delta = EditDelta {
        offset: offset_char,
        removed: byte_delta.removed,
        inserted: byte_delta.inserted,
    };
    state.apply_delta(delta);
    // Now map the target byte onto the mutated rope.  Clamp to buffer bounds.
    let source = state.buffer.contents();
    let clamped_byte = cursor_byte_target.min(source.len());
    let char_off = state.buffer.rope().byte_to_char(clamped_byte);
    state.cursor.offset = char_off.min(state.buffer.len_chars());
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
    state.update_cursor_block();
}

// ── List editing helpers ──────────────────────────────────────────────────────
//
// `list_edit` (byte-oriented) mirrors `table_edit`.  These helpers look up the
// list at the cursor, convert between byte and char offsets, and apply the
// resulting `EditDelta`s through `apply_byte_delta`.

/// If the cursor is inside a Markdown list, dispatch `Enter` to one of
/// three list-aware handlers and return `true` to signal that the newline
/// has been consumed.  The dispatch ladder implements the triple-`Enter`
/// list-break gesture:
///
/// 1. **Item with content** → [`list_edit::continue_item`] inserts a new
///    empty item directly below the cursor.
/// 2. **Empty item with no blank line above it** →
///    [`list_edit::space_out_empty_item`] pushes the empty marker (and
///    the cursor on it) down one line, widening the gap above without
///    yet ending the list.
/// 3. **Empty item with a blank line above it** →
///    [`list_edit::exit_list`] strips the empty marker and, in mid-list,
///    inserts whatever is needed to complete a two-blank-line section
///    break so the parser splits the surviving head and renumbered tail
///    into two distinct lists.
///
/// Returns `false` when the cursor is not in a list — the caller should
/// fall through to a plain newline insert.
fn list_handle_newline(state: &mut EditorState) -> bool {
    let Some((source, info)) = current_list(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    let Some(item_idx) = list_edit::cursor_item_idx(&info, byte) else {
        return false;
    };
    let item = &info.items[item_idx];

    // Cursor must be past the marker prefix for any list-aware handling.  If
    // it's in the indent/marker itself, fall through to plain newline.
    if byte < item.marker_end {
        return false;
    }

    let result = if item.content_is_empty(&source) {
        if list_edit::is_blank_line_above(&source, item.start) {
            list_edit::exit_list(&info, &source, byte)
        } else {
            list_edit::space_out_empty_item(&info, &source, byte)
        }
    } else {
        list_edit::continue_item(&info, &source, byte)
    };

    if let Some(res) = result {
        apply_byte_delta(state, res.delta, res.cursor_byte);
        true
    } else {
        false
    }
}

/// Indent the cursor's list item one level (adds `INDENT_WIDTH` spaces of
/// indent, resets ordered numbering to 1, renumbers the surrounding outer
/// list).  Returns `true` when handled so the caller can skip the fallback
/// plain-tab insertion.
fn list_indent(state: &mut EditorState) -> bool {
    let Some((source, info)) = current_list(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    // The first item of a list cannot be indented — it has no preceding
    // sibling to nest under, so any extra indent degrades the marker (lazy
    // paragraph continuation, or an indented code block at the top level).
    // Swallow the Tab as handled: the plain-space fallback would corrupt
    // the marker line the same way.
    if list_edit::cursor_item_idx(&info, byte) == Some(0) {
        return true;
    }
    let Some(res) = list_edit::indent_item(&info, &source, byte, crate::constants::INDENT_WIDTH)
    else {
        return false;
    };
    apply_byte_delta(state, res.delta, res.cursor_byte);
    true
}

/// Outdent the cursor's list item one level (removes up to `INDENT_WIDTH`
/// leading spaces).  No-op (returns `false`) when the cursor isn't in a
/// list or when the item is already at the outermost indent.
fn list_outdent(state: &mut EditorState) -> bool {
    let Some((source, info)) = current_list(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    let Some(res) = list_edit::outdent_item(&info, &source, byte, crate::constants::INDENT_WIDTH)
    else {
        return false;
    };
    apply_byte_delta(state, res.delta, res.cursor_byte);
    true
}

/// Toggle the checkbox on the cursor's task-list item, if any.
fn list_toggle_checkbox(state: &mut EditorState) {
    let Some((source, info)) = current_list(state) else {
        return;
    };
    let byte = cursor_byte(state);
    let Some(res) = list_edit::toggle_checkbox(&info, &source, byte) else {
        return;
    };
    apply_byte_delta(state, res.delta, res.cursor_byte);
}

/// After an edit that may have landed in or adjacent to an ordered list
/// (delete, paste, list-break, …), renumber the surrounding list so the
/// sequence stays monotonic.  No-op for bullet lists, in Raw mode, or when the
/// cursor is outside a list.
///
/// Uses the pure, nesting-aware, loose-list-aware
/// [`list_edit::renumber_list_block`]: it scans the buffer source (no reparse,
/// so it is cheap enough to run on every keystroke) and renumbers every ordered
/// run in the surrounding list block, spanning loose-list blank gaps so a
/// blank-separated list — which pulldown-cmark renders as one continuous
/// sequence — is renumbered as a whole.  A delete lands the cursor on the line
/// below, which for a list whose items have nested children is the *child*, so
/// the block walk (rather than a flat per-indent renumber) is what keeps the
/// outer sequence correct.
pub(crate) fn list_renumber_at_cursor(state: &mut EditorState) {
    // Raw mode: defer to plain text, never rewrite markers (mirrors
    // `current_list`'s bail-out, which the other list-edit paths use).
    if state.mode == Mode::Raw {
        return;
    }
    let source = state.buffer.contents();
    let byte_before = cursor_byte(state);
    if let Some(delta) = list_edit::renumber_list_block(&source, byte_before) {
        apply_byte_delta(state, delta, byte_before);
    }
}

/// Outcome of [`fix_list_numbering`], so the App layer can pick the right
/// flash message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixListNumbering {
    /// The cursor is not inside an ordered list (empty, plain text, or a
    /// bullet list).
    NotOrdered,
    /// The cursor's ordered list is already sequential — nothing to do.
    AlreadyCorrect,
    /// The list was renumbered as a single undoable edit.
    Fixed,
}

/// Renumber the ordered list under the cursor so its source numbering matches
/// what is rendered, as one undoable edit.  This is the user-invokable "Fix
/// list numbering" command; unlike [`list_renumber_at_cursor`] (the silent
/// post-edit recovery hook) it reports *why* nothing changed so the caller can
/// flash feedback.
pub fn fix_list_numbering(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) -> FixListNumbering {
    // Works in every view mode, including Raw.  Unlike the *automatic*
    // list-edit paths (`current_list`, `list_renumber_at_cursor`) — which
    // defer to plain text in Raw so the engine never rewrites markers the user
    // is editing by hand — this is an *explicit* user command (like
    // `renumber_footnotes`): running it is a deliberate request to rewrite the
    // source, and Raw is exactly where the user sees that source.
    let source = state.buffer.contents();
    let byte = cursor_byte(state);
    // Classify the cursor's *immediate* list: a cursor on a nested ordered
    // list inside a bullet list is still "in an ordered list".
    match list_edit::find_list_at(&source, byte).map(|info| info.kind) {
        Some(list_edit::MarkerKind::Ordered(_)) => {}
        _ => return FixListNumbering::NotOrdered,
    }
    // Renumber the whole surrounding list block (loose-list gaps included).
    match list_edit::renumber_list_block(&source, byte) {
        Some(delta) => {
            apply_byte_delta(state, delta, byte);
            state.ensure_cursor_visible(viewport_height, viewport_width);
            FixListNumbering::Fixed
        }
        None => FixListNumbering::AlreadyCorrect,
    }
}

/// Look up the list surrounding the cursor, returning the source snapshot and
/// parsed `ListInfo` so the caller need not re-fetch `buffer.contents()`.
fn current_list(state: &EditorState) -> Option<(String, ListInfo)> {
    // Mirrors the Raw-mode bail-out in `current_table`: every list-aware
    // editing path should defer to plain text behaviour in Raw mode so the
    // user can edit markers and checkboxes directly without the engine
    // "helpfully" rewriting them.
    if state.mode == Mode::Raw {
        return None;
    }
    let source = state.buffer.contents();
    let byte = cursor_byte(state);
    list_edit::find_list_at(&source, byte).map(|info| (source, info))
}

/// If the cursor sits exactly at `content_start` of a list item, delete the
/// entire marker prefix (the indent + `- ` / `N. `) so the user never has to
/// remove the marker character-by-character.  The item's content stays on its
/// own line and the cursor lands at the start of that line — the item is
/// "un-bulleted", not merged into the item above.  A second backspace then
/// falls through to the plain-text path and joins the lines, so the merge is
/// still reachable, just never as the surprising first step.  Returns `true`
/// when the edit was applied.
///
/// Task items get a two-step erase: the first backspace peels off only the
/// `[ ] ` checkbox prefix (turning the task back into a plain bullet item),
/// and a subsequent backspace falls through to the marker-eating path that
/// removes the bullet itself.  This matches the user-facing rule that the
/// checkbox is the "extra" decoration on a bullet — deleting it shouldn't
/// also delete the bullet.
fn list_backspace_consumes_marker(state: &mut EditorState) -> bool {
    let Some((source, info)) = current_list(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    let Some(item_idx) = list_edit::cursor_item_idx(&info, byte) else {
        return false;
    };
    let item = &info.items[item_idx];
    if byte != item.content_start {
        return false;
    }

    // Step 1 of two-step erase for task items.
    if item.task.is_some() {
        let removed = source[item.marker_end..item.content_start].to_owned();
        if removed.is_empty() {
            return false;
        }
        let delta = EditDelta {
            offset: item.marker_end,
            removed,
            inserted: String::new(),
        };
        apply_byte_delta(state, delta, item.marker_end);
        return true;
    }

    // Delete the marker prefix only — from the start of the item's line
    // through `content_start`.  The preceding `\n` is deliberately left
    // alone so the content keeps its own line.
    let delete_start = item.start;
    let removed = source[delete_start..item.content_start].to_owned();
    if removed.is_empty() {
        return false;
    }
    let delta = EditDelta {
        offset: delete_start,
        removed,
        inserted: String::new(),
    };
    apply_byte_delta(state, delta, delete_start);
    // Ordered-list renumbering runs automatically at end of `apply()` when
    // the buffer has changed, so no explicit call is needed here.
    true
}

/// If the cursor sits on a list-item marker (the indent + `- ` / `N. ` /
/// `[ ] ` prefix), snap it to the item's `content_start`.  Called after
/// editing actions that may leave the cursor on a marker (`DeleteLine`,
/// `Paste` of non-list content into the middle of a list, etc.).
fn clamp_cursor_out_of_marker(state: &mut EditorState) {
    let Some((_, info)) = current_list(state) else {
        return;
    };
    let byte = cursor_byte(state);
    let Some(item_idx) = list_edit::cursor_item_idx(&info, byte) else {
        return;
    };
    let item = &info.items[item_idx];
    if byte >= item.start && byte < item.content_start {
        set_cursor_byte(state, item.content_start);
    }
}

/// Treat list-item markers as non-navigable when the cursor moves
/// horizontally.  Within a list, the cursor only lands on positions between
/// `content_start` and `line_end` for each item; stepping across those
/// boundaries hops directly to the adjacent item (or out of the list when
/// already at the first item's content start or the last item's line end).
fn list_move_horizontal(state: &mut EditorState, forward: bool) -> bool {
    let Some((source, info)) = current_list(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    let Some(item_idx) = list_edit::cursor_item_idx(&info, byte) else {
        return false;
    };
    let item = &info.items[item_idx];

    // Multi-line items: the marker-hopping logic below is first-line
    // geometry (`content_start` / `line_end`).  A cursor on a continuation
    // line — or at the first line's end when continuation lines follow —
    // moves char-by-char like plain text instead of hopping items.
    let has_continuation = item.end > item.line_end + 1;
    if byte > item.line_end || (byte == item.line_end && has_continuation && forward) {
        return false;
    }

    if forward {
        if byte >= item.line_end {
            if let Some(next) = info.items.get(item_idx + 1) {
                set_cursor_byte(state, next.content_start);
                return true;
            }
            // Last item: step past the list entirely.  info.end sits just
            // past the final `\n`, which is where the first post-list line
            // begins.
            set_cursor_byte(state, info.end.min(source.len()));
            return true;
        }
        // Normal grapheme-step, but if we'd land before the next item's
        // content (because we stepped onto a marker char, which shouldn't
        // happen for correctly-skipped cursors), clamp to content_start.
        let new_char = next_grapheme_offset(&state.buffer, state.cursor.offset);
        state.cursor.offset = new_char;
        true
    } else {
        if byte <= item.content_start {
            if item_idx > 0 {
                let prev = &info.items[item_idx - 1];
                set_cursor_byte(state, prev.line_end);
                return true;
            }
            // First item: step out past the list's starting `\n`, if any.
            if info.start > 0 {
                set_cursor_byte(state, info.start - 1);
            }
            // else stay put at content_start.
            return true;
        }
        let new_char = prev_grapheme_offset(&state.buffer, state.cursor.offset);
        state.cursor.offset = new_char;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn base64_encodes_empty() {
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn base64_encodes_one_byte() {
        assert_eq!(base64_encode(b"f"), "Zg==");
    }

    #[test]
    fn base64_encodes_two_bytes() {
        assert_eq!(base64_encode(b"fo"), "Zm8=");
    }

    #[test]
    fn base64_encodes_three_bytes() {
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }

    #[test]
    fn base64_encodes_rfc4648_vectors() {
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"Hello, world!"), "SGVsbG8sIHdvcmxkIQ==");
    }
}
