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

/// True when `action` is a hot-path typing action that applies a single edit and doesn't read
/// `state.parsed`.  Lets `apply` skip the pre-action parse flush: the rendered view reads the
/// cursor block's raw text straight from the buffer, so no stale `source_map` is observed, and
/// cross-line edits (Newline, …) re-parse inline inside `apply_delta`.
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
    // The sync point for the "re-parse on cursor move" invariant: non-typing actions read
    // `state.parsed`, so the deferred re-parse must land first.
    if !is_hot_typing_action(&action) {
        state.flush_parsed_if_dirty();
    }

    // Undo/Redo must NOT trigger the post-match renumber: they restore the previous buffer
    // exactly, so numbering the user deliberately reverted to must stick.
    let buffer_len_before = state.buffer.len_chars();
    let history_depth_before = state.history.undo_depth();
    let suppress_autonumber = matches!(action, Action::Undo | Action::Redo);

    match action {
        // ── Quit ──────────────────────────────────────────────────
        Action::Quit => return true,

        // ── Mode transitions ──────────────────────────────────────
        Action::EnterEditMode => {
            // A read-only document rests in Preview; leaving it is what read-only refuses.
            if state.mode == Mode::Preview && !state.readonly {
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
            // Refused for the same reason as `EnterEditMode`: Rendered and Raw are the editing
            // presentation a read-only document never enters.
            if state.readonly {
                return false;
            }
            if state.mode == Mode::Preview {
                sync_cursor_to_scroll(state, viewport_height);
            }
            state.visual_selection = None;
            // Rendered and Raw scroll in different units (rendered lines vs. buffer lines), so the
            // same `scroll` value lands elsewhere in the document; anchor on the screen row
            // instead.  Preview has no editing cursor to anchor on.
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
                // Unreachable in normal dispatch (`diff_safe_action` filters this out); no-op.
                Mode::Diff => Mode::Diff,
            };
            // A cursor inside an HTML comment is visible in Raw but not in Rendered; snap it to
            // the next visible block so hybrid rendering has a well-defined position.
            if was_raw && state.mode == Mode::Rendered {
                snap_cursor_out_of_hidden_block(state, viewport_width);
                state.update_cursor_block();
            }
            if let Some(row) = preserve_screen_row {
                state.set_scroll_for_cursor_screen_row(row, viewport_width);
                // Deliberately no `ensure_cursor_visible`: it tries to fit the whole *block*, which
                // clobbers this placement whenever the cursor's block overflows the viewport.
            }
        }

        // ── Cursor movement ───────────────────────────────────────
        Action::MoveLeft => {
            enter_edit_if_preview(state, viewport_height);
            state.selection = None;
            // In Raw every character is a valid cursor position, table chrome included.
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
                // Plain line step — don't skip the alignment row.
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
                // Preview selects rendered text, so span the whole rendered line list.
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
        }

        // ── Editing ───────────────────────────────────────────────
        Action::InsertChar(ch) => {
            if state.mode == Mode::Preview {
                // First keypress from Preview enters edit mode but does not insert.
                sync_cursor_to_scroll(state, viewport_height);
                state.mode = Mode::Rendered;
                return false;
            }
            // `|` inside a table cell is escaped so it doesn't split the cell.
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
            // Tab dispatches by context: next table cell, else indent the list item, else spaces.
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
            // Enter inside a table steps down a row, appending one on the last data row.
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
                // Whole grapheme cluster, so ZWJ sequences and combining marks leave no fragments.
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
            // Preview selects rendered characters (no Markdown markers); the editing modes
            // select raw buffer text.  Copy whichever the user actually sees.
            let text = if state.mode == Mode::Preview {
                if let Some(vs) = state.visual_selection {
                    crate::editor::mouse_ops::visual_selection_to_rendered_text(
                        vs,
                        &state.parsed.lines,
                    )
                } else {
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
                // A GFM `<br>` breaks the line without terminating the row.
                insert_text(state, "<br>");
            } else {
                insert_text(state, "\n");
            }
        }
        _ => {}
    }

    // Keep any surrounding ordered list's source numbering monotonic.  Skipped for Undo/Redo
    // (they must stay exact inverses) and outside Rendered (Raw is deliberately raw).
    let edited = state.buffer.len_chars() != buffer_len_before
        || state.history.undo_depth() != history_depth_before;
    if !suppress_autonumber && state.mode == Mode::Rendered && edited {
        list_renumber_at_cursor(state);
    }

    // An action may have parked the cursor on a list marker (`DeleteLine` lands it on the next
    // line's marker); snap onto the content so the next keystroke goes where the caret is drawn.
    // Not in Raw, where the cursor is expected to reach every byte.
    if state.mode == Mode::Rendered && !suppress_autonumber {
        clamp_cursor_out_of_marker(state);
    }

    // Pure edit actions can push the cursor onto a newly-wrapped row past the viewport bottom;
    // the movement arms above already handle navigation.  In Rendered, `ensure_cursor_visible`
    // detects wrap from `parsed.lines`, which an in-line edit leaves stale — flush first.  Raw
    // reads the live buffer, so it needs no flush.
    if edited && state.mode != Mode::Preview {
        if state.mode != Mode::Raw {
            state.flush_parsed_if_dirty();
        }
        state.ensure_cursor_visible(viewport_height, viewport_width);
    }

    false
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// The single door out of Preview into an editing mode, used by every mutating action.
///
/// The `readonly` guard here is the mode-level backstop, twin of [`EditorState::apply_delta`]'s
/// text-level one: refusing the transition makes every call site a no-op at once, so read-only is
/// *made* rather than maintained by auditing each mutating path.
fn enter_edit_if_preview(state: &mut EditorState, viewport_height: usize) {
    if state.readonly {
        return;
    }
    if state.mode == Mode::Preview {
        sync_cursor_to_scroll(state, viewport_height);
        state.mode = Mode::Rendered;
        state.visual_selection = None;
    }
}

/// Under visual-line nav, restate `preferred_col` in visual columns so subsequent vertical moves
/// aim at the column the user sees rather than the raw one.
fn sync_preferred_visual(state: &mut EditorState, viewport_width: usize) {
    if state.visual_line_nav && viewport_width > 0 {
        state.cursor.preferred_col = state.current_visual_col(viewport_width);
    }
}

/// Move the cursor to the first visible block, unless it is already on screen.  Called on the
/// Preview → editing transition so the cursor appears near what the user is looking at.
fn sync_cursor_to_scroll(state: &mut EditorState, viewport_height: usize) {
    let scroll = state.scroll;
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    let cursor_lines = state.parsed.source_map.rendered_lines_for_byte(cursor_byte);
    if !cursor_lines.is_empty() {
        let visible_end = scroll + viewport_height;
        if cursor_lines.start >= scroll && cursor_lines.start < visible_end {
            return;
        }
    }
    if let Some(byte) = state
        .parsed
        .source_map
        .original_byte_for_rendered_line(scroll)
    {
        let char_offset = state.buffer.rope().byte_to_char(byte);
        state.cursor.offset = char_offset.min(state.buffer.len_chars());
    }
}

/// Coalesced version of a run of `Action::InsertChar` events: a held-key autorepeat burst becomes
/// one delta, one history entry, and one `parsed_version` bump instead of N.  Applies the same
/// table-pipe escaping as the per-keystroke path.
///
/// Preconditions, enforced by the dispatcher's run-membership predicate: `chars` non-empty,
/// `state.mode != Mode::Preview` (Preview's first keystroke only transitions), `selection` is
/// `None` (a selection-deleting insert ends its own run).
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

    // Mirror the post-action upkeep from `apply()`.
    if state.mode == Mode::Rendered {
        list_renumber_at_cursor(state);
        clamp_cursor_out_of_marker(state);
    }
    if state.mode != Mode::Raw {
        state.flush_parsed_if_dirty();
    }
    state.ensure_cursor_visible(viewport_height, viewport_width);
}

/// Coalesced delete run: removes `count` graphemes in one delta.  Same preconditions as
/// [`apply_insert_run`], plus `count >= 1`.
///
/// List-marker and task-checkbox erase are deliberately absent — they are one-shot transitions,
/// so the dispatcher routes the first delete through `apply()` (where
/// `list_backspace_consumes_marker` runs) and only later events into this run.
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
    // `apply_delta` lands the cursor at `start`, correct for both directions — no pre-set needed.
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

/// Wrap the active selection in `marker`, or unwrap it when the selection is already exactly that
/// emphasis (markers inside the selection, or immediately outside it).  Re-selects the inner text
/// so wraps can be chained.  No-op without a non-empty selection, as in Preview.
///
/// Multi-line selections are refused: CommonMark emphasis can't cross a blank line, so wrapping
/// would only emit literal asterisks.  Selections containing other inline formatting are wrapped
/// verbatim — doing better needs an AST-aware transform.
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

    // Markers are ASCII, so byte length == char count: valid for both rope offsets and slicing.
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

/// True when `text` is *exactly* `marker…marker` with no further `marker` in between.  Italic
/// (`*`) rejects the bold case so toggling italic over bold wraps rather than stripping one bold
/// marker, and the interior check stops `**a** and **b**` from unwrapping into malformed markdown.
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

/// True when `marker` sits immediately outside `[start, end)` on both sides.  Rejects the bold
/// case for italic, same rule as [`is_marker_wrapped`].
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
        // A `*` beyond the candidate marker means the real markers are `**`, not `*`.
        let bold_before = start >= 2 && buf.slice_to_string(start - 2, start - 1) == "*";
        let bold_after = end + 2 <= buf.len_chars() && buf.slice_to_string(end + 1, end + 2) == "*";
        if bold_before || bold_after {
            return false;
        }
    }
    true
}

/// Whether the OS-level clipboard paths are live.
///
/// **False in unit tests, always.** The OS clipboard is process-global state shared by every test
/// thread, so a live one makes clipboard tests race (and leaks the developer's own clipboard into
/// assertions); this constant is what enforces "tests assert against the kill-ring". It also keeps
/// OSC 52 escapes out of test stdout.  Integration tests in `tests/` link the library without
/// `cfg(test)` and are covered instead by CI's `--no-default-features`, which drops `arboard`.
const OS_CLIPBOARD: bool = !cfg!(test);

/// Write `text` to the OS clipboard (best-effort via arboard *and* OSC 52) and always mirror it
/// into the kill-ring so internal paste works when neither external path is available.
///
/// `text` arrives in the rope's `\n`-only form and the kill-ring keeps it that way, but the
/// external clipboard gets the buffer's on-disk newline convention — the counterpart of the
/// save-time translation, keyed off the buffer (a CRLF file opened on Linux copies `\r\n` too),
/// not the host platform.  Otherwise some applications render a copied CRLF document as one line.
fn copy_to_clipboard(state: &mut EditorState, text: String) {
    if OS_CLIPBOARD {
        let external = crate::document::buffer::encode_newlines(&text, state.buffer.line_ending());
        #[cfg(feature = "clipboard")]
        copy_to_system_clipboard(external.clone());
        // OSC 52 is the only path that works over SSH, on Wayland without `wayland-data-control`,
        // and in WSL.  Terminals that don't understand it ignore it, so emit unconditionally.
        osc52_copy(&external);
    }
    state.kill_ring = text;
}

/// Linux copy.  Wayland/X11 hold clipboard data only while a process owns the selection, and
/// arboard prints to *stderr* — corrupting the TUI — if the `Clipboard` drops too soon after
/// `set_text`; hence a thread that owns the selection until another program takes over.  macOS and
/// Windows clipboards persist past process exit and use the simple path below.
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

/// Write `text` to the terminal's clipboard via OSC 52 (`ESC ] 52 ; c ; base64 BEL`).
fn osc52_copy(text: &str) {
    use std::io::Write;
    let encoded = base64_encode(text.as_bytes());
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "\x1b]52;c;{encoded}\x07");
    let _ = stdout.flush();
}

/// Minimal RFC-4648 base64 encoder, hand-written to avoid a dependency for one caller.
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

/// Read from the OS clipboard if available, else the kill-ring.  Public so callers that reshape
/// the payload first (the App's linewise vim VisualLine paste) read the same source as
/// `Action::Paste`.
pub fn clipboard_text(state: &EditorState) -> String {
    #[cfg(feature = "clipboard")]
    if OS_CLIPBOARD {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if let Ok(text) = cb.get_text() {
                // Collapse external CRLF so the rope invariant holds and a CRLF save does not
                // double the `\r`.  The kill-ring is already `\n`-only.
                return crate::document::buffer::normalize_newlines(text);
            }
        }
    }
    state.kill_ring.clone()
}

/// Insert `text` at the cursor (or over the selection) as if pasted.  Used by `App`'s
/// bracketed-paste handler, so terminal-level pastes need no reachable OS clipboard.
pub fn paste_text(
    state: &mut EditorState,
    text: &str,
    viewport_height: usize,
    viewport_width: usize,
) {
    if text.is_empty() {
        return;
    }
    // Same CRLF collapse as `clipboard_text`.
    let text = crate::document::buffer::normalize_newlines(text.to_owned());
    let text = text.as_str();
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

/// [`table_edit::insert_table`] applied to `EditorState`, landing the cursor in the new table's
/// first header cell.  Inserts unconditionally: the App-level handler runs the blank-line
/// pre-flight before opening the modal.
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

/// Placeholder shared by the image / link snippets, left selected after insert so the user's next
/// keystrokes replace it.
pub const URL_PLACEHOLDER: &str = "file path or URL";

/// True when the block under the cursor can host inline Markdown, i.e. an image or link snippet
/// there parses as markup rather than literal text.
///
/// Classification is by *top-level* block, so a code fence nested in a list item is not detected —
/// the same fidelity as the other location guards (`cursor_line_is_blank`, `cursor_in_table`).
// Library-only: the binary reaches the offset-based guard directly; this wrapper exists for the
// block-classification integration tests.
#[allow(dead_code)]
pub fn cursor_block_allows_inline_markdown(state: &mut EditorState) -> bool {
    let offset = state.cursor.offset;
    block_allows_inline_markdown_at(state, offset)
}

/// Offset-based body of [`cursor_block_allows_inline_markdown`]: a wrapping insert lands at the
/// selection start, not the cursor.
fn block_allows_inline_markdown_at(state: &mut EditorState, char_offset: usize) -> bool {
    use crate::markdown::Block;
    // A typing burst defers the re-parse; flush so classification sees fresh ranges.
    state.flush_parsed_if_dirty();
    let byte = state.buffer.rope().char_to_byte(char_offset);
    let Some(block) = state.parsed.real_block_for_byte(byte) else {
        // Blank line or EOF: the snippet becomes its own paragraph, ideal for an image.
        return true;
    };
    !matches!(
        block,
        Block::CodeBlock { .. }
            | Block::Html(_)
            | Block::HtmlComment(_)
            | Block::HorizontalRule
            | Block::ImageBlock { .. }
            // Frontmatter is YAML / TOML: inline Markdown there is corruption, not emphasis.
            | Block::MetadataBlock { .. }
    )
}

/// Insert an image snippet at the cursor.  See [`insert_inline_snippet`] for the behavior.
pub fn insert_image_at_cursor(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    insert_inline_snippet(
        state,
        "!",
        "alt text",
        None,
        viewport_height,
        viewport_width,
    )
}

/// Insert a link snippet at the cursor.  See [`insert_inline_snippet`] for the behavior.
pub fn insert_link_at_cursor(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    insert_inline_snippet(
        state,
        "",
        "link text",
        None,
        viewport_height,
        viewport_width,
    )
}

/// Insert a complete image reference `![](url)` at the cursor (empty alt
/// text), reusing [`insert_inline_snippet`]'s pre-flight and
/// selection-wrapping but skipping the placeholder selection — the
/// cursor lands just past the link.  Used by the clipboard paste flow
/// where the path is already known.
pub fn insert_image_reference_at_cursor(
    state: &mut EditorState,
    url: &str,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    insert_inline_snippet(state, "!", "", Some(url), viewport_height, viewport_width)
}

/// Frame `reference` as a paragraph of its own at `offset`: a blank line
/// before it (unless one is already there) and a newline after it (unless
/// the line already ends).  Returns the text to insert in one delta.
///
/// The parse only promotes an image to a block when it is a paragraph's
/// sole content, so a reference inserted flush against other text would
/// stay inline and render as a text placeholder instead of the image.
fn frame_own_paragraph(buffer: &crate::document::Buffer, offset: usize, reference: &str) -> String {
    let rope = buffer.rope();
    let len = rope.len_chars();
    let offset = offset.min(len);
    // A blank line (or the buffer start) already behind the cursor means
    // the reference opens its own paragraph and needs no leading break;
    // sitting at the start of a line whose predecessor is text needs one
    // blank line, and sitting mid-line needs that line ended first.
    let at_line_start = offset == 0 || rope.char(offset - 1) == '\n';
    let above_blank = offset < 2 || rope.char(offset - 2) == '\n';
    let mut out = String::new();
    if !(at_line_start && above_blank) {
        out.push_str(if at_line_start { "\n" } else { "\n\n" });
    }
    out.push_str(reference);
    if offset >= len || rope.char(offset) != '\n' {
        out.push('\n');
    }
    out
}

/// Shared body of the image / link snippet inserts.  Returns `false` — mode, selection and buffer
/// untouched — when the target block can't host inline Markdown (see
/// [`cursor_block_allows_inline_markdown`]); the App-level handler flashes a warning there.
///
/// A single-line selection becomes the visible text and the URL placeholder is left selected;
/// otherwise the whole snippet is inserted with the text placeholder selected.  A multi-line
/// selection is dropped rather than wrapped — link text can't span blocks — so nothing is lost.
///
/// `url` is `None` to insert and select the URL placeholder (the image / link snippet flows), or
/// `Some(url)` to insert a fixed destination and leave the cursor just past the link (the
/// clipboard paste flow).
fn insert_inline_snippet(
    state: &mut EditorState,
    prefix: &str,
    text_placeholder: &str,
    url: Option<&str>,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    // Sync before the pre-flight so the guard classifies the block the snippet really lands in.
    // Deliberately not `enter_edit_if_preview` yet: a denied insert must not leave Preview.
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
    // `prefix`, the brackets and the placeholders are ASCII, so byte lengths double as char
    // counts; only the wrapped selection text needs `chars().count()`.
    let (offset, removed, visible_text, select_placeholder_url) = match wrap {
        Some((start, text)) => (start, text.clone(), text, true),
        None => (
            state.cursor.offset,
            String::new(),
            text_placeholder.to_owned(),
            false,
        ),
    };
    let reference = format!(
        "{prefix}[{visible_text}]({})",
        url.unwrap_or(URL_PLACEHOLDER)
    );
    // A fixed-URL insert is the clipboard paste's block image: the parse
    // promotes an image to a block only when it is a paragraph's *sole*
    // content (`markdown::parser::post_pass::promote_image_paragraphs`),
    // so it is framed with its own blank lines.  Left inline it would
    // paint as a text placeholder rather than the image.
    let inserted = if url.is_some() {
        frame_own_paragraph(&state.buffer, offset, &reference)
    } else {
        reference
    };
    let inserted_len = inserted.chars().count();
    state.cursor.offset = offset;
    state.apply_delta(EditDelta {
        offset,
        removed,
        inserted,
    });
    if url.is_none() {
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
    } else {
        state.cursor.offset = offset + inserted_len;
    }
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
    state.update_cursor_block();
    state.ensure_cursor_visible(viewport_height, viewport_width);
    true
}

/// Insert an auto-numbered `[^N]` footnote reference at the cursor; the user writes the definition
/// wherever they like.  Until one exists the marker renders as literal text, per CommonMark.
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

/// Re-sequence numeric footnotes into order of first reference, leaving named labels alone.
/// Returns `false` when nothing needed renumbering.
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

/// Set the cursor to a byte offset, clamped to buffer bounds.
pub(super) fn set_cursor_byte(state: &mut EditorState, target_byte: usize) {
    let source_len = state.buffer.contents().len();
    let clamped = target_byte.min(source_len);
    let char_off = state.buffer.rope().byte_to_char(clamped);
    state.cursor.offset = char_off.min(state.buffer.len_chars());
}

/// The cursor as a byte offset into `buffer.contents()`, for the byte-oriented sibling helpers.
pub(super) fn cursor_byte(state: &EditorState) -> usize {
    state.buffer.rope().char_to_byte(state.cursor.offset)
}

/// One-line vertical move that skips a table's alignment row and hidden blocks, so the cursor
/// never stalls on a structural artifact.  The skip/step logic lives on
/// [`EditorState::move_cursor_line`], shared with vim `j`/`k`.
fn move_line_skipping_alignment(state: &mut EditorState, down: bool, viewport_width: usize) {
    let visual = state.visual_line_nav;
    state.move_cursor_line(down, visual, viewport_width);
}

/// True when the cursor is inside a zero-rendered-line block whose source is an HTML comment, so
/// navigation can skip bytes the user can't see.
///
/// Deliberately narrower than "any zero-own block": suppressed blank lines also have zero own
/// lines but are positions the cursor may legitimately occupy.
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

/// Walk the cursor forward past hidden (HTML-comment) blocks so rendering can assume its block has
/// at least one rendered line.  Called on the Raw → Rendered transition.
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
            // At the buffer's end, nowhere to skip to; the rendered view falls back gracefully.
            break;
        }
        safety -= 1;
    }
}

/// Apply an `EditDelta` whose offsets are **bytes** (as produced by `table_edit` / `list_edit`),
/// converting to rope char offsets.  `cursor_byte_target` is the *post-edit* cursor position and
/// is converted against the mutated rope.
pub(super) fn apply_byte_delta(
    state: &mut EditorState,
    byte_delta: EditDelta,
    cursor_byte_target: usize,
) {
    // Byte → char against the *pre-edit* rope.
    let offset_char = state.buffer.rope().byte_to_char(byte_delta.offset);
    let delta = EditDelta {
        offset: offset_char,
        removed: byte_delta.removed,
        inserted: byte_delta.inserted,
    };
    state.apply_delta(delta);
    let source = state.buffer.contents();
    let clamped_byte = cursor_byte_target.min(source.len());
    let char_off = state.buffer.rope().byte_to_char(clamped_byte);
    state.cursor.offset = char_off.min(state.buffer.len_chars());
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
    state.update_cursor_block();
}

// ── List editing helpers ──────────────────────────────────────────────────────
//
// These look up the list at the cursor and route byte-oriented `list_edit` deltas through
// `apply_byte_delta`, mirroring the `table_edit` helpers.

/// Dispatch `Enter` inside a list, returning `true` when the newline was consumed (`false` means
/// the caller inserts a plain newline).  The ladder is the triple-`Enter` list-break gesture:
/// item with content → [`list_edit::continue_item`]; empty item →
/// [`list_edit::space_out_empty_item`]; empty item already preceded by a blank line →
/// [`list_edit::exit_list`], which mid-list completes a two-blank-line break so the parser splits
/// the surviving head and renumbered tail into separate lists.
fn list_handle_newline(state: &mut EditorState) -> bool {
    let Some((source, info)) = current_list(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    let Some(item_idx) = list_edit::cursor_item_idx(&info, byte) else {
        return false;
    };
    let item = &info.items[item_idx];

    // Inside the indent/marker itself: fall through to a plain newline.
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

/// Indent the cursor's list item one level, resetting ordered numbering to 1 and renumbering the
/// outer list.  `true` when handled, so the caller skips the plain-tab fallback.
fn list_indent(state: &mut EditorState) -> bool {
    let Some((source, info)) = current_list(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    // The first item has no sibling to nest under, so extra indent degrades the marker into lazy
    // continuation or a code block.  Swallow the Tab: the plain-space fallback corrupts it too.
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

/// Outdent the cursor's list item one level.  `false` when not in a list, or already outermost.
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

/// Silent post-edit renumber of the ordered list around the cursor.  No-op for bullet lists, in
/// Raw, or outside a list.
///
/// [`list_edit::renumber_list_block`] scans the source without reparsing (cheap enough per
/// keystroke) and renumbers the whole surrounding *block*, spanning loose-list blank gaps because
/// pulldown-cmark renders those as one sequence.  The block walk rather than a flat per-indent
/// pass is what keeps the outer sequence right when a delete lands the cursor on a nested child.
pub(crate) fn list_renumber_at_cursor(state: &mut EditorState) {
    // Raw defers to plain text, same bail-out as `current_list`.
    if state.mode == Mode::Raw {
        return;
    }
    let source = state.buffer.contents();
    let byte_before = cursor_byte(state);
    if let Some(delta) = list_edit::renumber_list_block(&source, byte_before) {
        apply_byte_delta(state, delta, byte_before);
    }
}

/// Outcome of [`fix_list_numbering`], so the App layer can pick a flash message.
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

/// The user-invokable "Fix list numbering" command: one undoable edit that makes the source
/// numbering match what is rendered.  Unlike [`list_renumber_at_cursor`] it reports *why* nothing
/// changed so the caller can flash feedback.
pub fn fix_list_numbering(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) -> FixListNumbering {
    // Works in Raw too, unlike the automatic paths that defer to plain text there: this is an
    // explicit request to rewrite the source, and Raw is where the user sees it.
    let source = state.buffer.contents();
    let byte = cursor_byte(state);
    // The cursor's *immediate* list: a nested ordered list inside a bullet list still counts.
    match list_edit::find_list_at(&source, byte).map(|info| info.kind) {
        Some(list_edit::MarkerKind::Ordered(_)) => {}
        _ => return FixListNumbering::NotOrdered,
    }
    match list_edit::renumber_list_block(&source, byte) {
        Some(delta) => {
            apply_byte_delta(state, delta, byte);
            state.ensure_cursor_visible(viewport_height, viewport_width);
            FixListNumbering::Fixed
        }
        None => FixListNumbering::AlreadyCorrect,
    }
}

/// The list surrounding the cursor, with the source snapshot so the caller need not re-fetch it.
fn current_list(state: &EditorState) -> Option<(String, ListInfo)> {
    // Every list-aware editing path defers to plain text in Raw (mirroring `current_table`) so the
    // user can edit markers and checkboxes by hand without the engine rewriting them.
    if state.mode == Mode::Raw {
        return None;
    }
    let source = state.buffer.contents();
    let byte = cursor_byte(state);
    list_edit::find_list_at(&source, byte).map(|info| (source, info))
}

/// Backspace at a list item's `content_start` deletes the whole marker prefix, un-bulleting the
/// item in place rather than merging it into the one above.  A second backspace falls through to
/// the plain-text join, so the merge stays reachable but is never the surprising first step.
///
/// Task items erase in two steps — checkbox prefix first, bullet second — because the checkbox is
/// the extra decoration on a bullet, so removing it shouldn't remove the bullet too.
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

    // The preceding `\n` is deliberately left alone so the content keeps its own line.
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
    true
}

/// Snap a cursor sitting on a list-item marker to the item's `content_start`.
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

/// Treat list-item markers as non-navigable: horizontal movement stays between `content_start`
/// and `line_end`, hopping to the adjacent item (or out of the list) at the boundaries.
fn list_move_horizontal(state: &mut EditorState, forward: bool) -> bool {
    let Some((source, info)) = current_list(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    let Some(item_idx) = list_edit::cursor_item_idx(&info, byte) else {
        return false;
    };
    let item = &info.items[item_idx];

    // The hopping below is first-line geometry, so a cursor on (or moving onto) a continuation
    // line steps char-by-char like plain text instead.
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
            // `info.end` sits just past the final `\n`, where the first post-list line begins.
            set_cursor_byte(state, info.end.min(source.len()));
            return true;
        }
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
            // First item: step out past the list's starting `\n`, if any; else stay put.
            if info.start > 0 {
                set_cursor_byte(state, info.start - 1);
            }
            return true;
        }
        let new_char = prev_grapheme_offset(&state.buffer, state.cursor.offset);
        state.cursor.offset = new_char;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{base64_encode, paste_text};
    use crate::config::Theme;
    use crate::document::Buffer;
    use crate::editor::{EditorState, Mode};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    #[test]
    fn paste_normalizes_crlf_to_lf() {
        let mut state = EditorState::new(Buffer::from_str(""), theme());
        state.mode = Mode::Raw;
        paste_text(&mut state, "one\r\ntwo\r\n", 24, 80);
        let contents = state.buffer.contents();
        assert!(
            !contents.contains('\r'),
            "rope must hold no CR: {contents:?}"
        );
        assert_eq!(contents, "one\ntwo\n");
    }

    #[test]
    fn paste_into_crlf_buffer_does_not_double_cr_on_save() {
        // Without normalization the save widens an already-`\r`-suffixed `\n` into `\r\r\n`.
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("crlf.md");
        std::fs::write(&src, "a\r\nb\r\n").expect("seed");
        let mut state = EditorState::new(Buffer::load_file(&src).expect("load"), theme());
        state.mode = Mode::Raw;
        state.cursor.offset = 0;

        paste_text(&mut state, "X\r\n", 24, 80);
        let out = dir.path().join("out.md");
        state.buffer.save_copy(&out).expect("save");
        assert_eq!(std::fs::read(&out).expect("read"), b"X\r\na\r\nb\r\n");
    }

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
