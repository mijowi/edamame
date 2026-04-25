use crate::config::Action;
use crate::document::{EditDelta, Selection};
use crate::editor::list_edit::{self, ListInfo};
use crate::editor::table_edit::{
    self, cell_cursor_offset, cell_end_cursor_offset, cursor_cell, find_table_at, RowKind,
    TableInfo,
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
            let was_raw = state.mode == Mode::Raw;
            state.mode = match state.mode {
                Mode::Preview => Mode::Rendered,
                Mode::Rendered => Mode::Raw,
                Mode::Raw => Mode::Rendered,
            };
            // Raw → Rendered: if the cursor was sitting inside an HTML
            // comment (visible in Raw, invisible in Rendered), snap it to
            // the start of the next visible block so hybrid rendering has
            // a well-defined cursor position.
            if was_raw && state.mode == Mode::Rendered {
                snap_cursor_out_of_hidden_block(state, viewport_width);
                state.update_cursor_block();
            }
        }

        // ── Cursor movement ───────────────────────────────────────
        Action::MoveLeft => {
            enter_edit_if_preview(state, viewport_height);
            state.selection = None;
            // Raw mode: every character is a valid cursor position — including
            // table borders and the alignment row.  The user owns the risk of
            // breaking formatting by editing the raw source directly.
            if state.mode == Mode::Raw {
                state.cursor.move_left(&state.buffer);
            } else if !table_move_horizontal(state, /*forward=*/ false)
                && !list_move_horizontal(state, /*forward=*/ false)
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
            if state.mode == Mode::Raw {
                state.cursor.move_right(&state.buffer);
            } else if !table_move_horizontal(state, /*forward=*/ true)
                && !list_move_horizontal(state, /*forward=*/ true)
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
                    state.visual_selection = Some(crate::document::VisualSelection {
                        anchor: (0, 0),
                        active: (last, last_col),
                    });
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
            //   - otherwise       → insert `tab_width` spaces
            if cursor_in_table(state) {
                table_next_cell(state, viewport_height, viewport_width);
            } else if !list_indent(state) {
                let indent: String = " ".repeat(state.tab_width);
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
                let offset = state.cursor.offset - 1;
                let ch = state.buffer.rope().char(offset).to_string();
                state.apply_delta(EditDelta {
                    offset,
                    removed: ch,
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
                let ch = state.buffer.rope().char(offset).to_string();
                state.apply_delta(EditDelta {
                    offset,
                    removed: ch,
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
                state.cursor.preferred_col = state.cursor.line_col(&state.buffer).1;
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
            let text = paste_from_clipboard(state);
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

        // ── File operations ───────────────────────────────────────
        Action::Save => {
            if state.buffer.save_file().is_ok() {
                state.dirty = false;
            }
        }
        Action::Open => {
            // File picker is deferred to Phase 9.
        }

        // ── Phase 3 — list editing ───────────────────────────────
        Action::ToggleCheckbox => {
            enter_edit_if_preview(state, viewport_height);
            list_toggle_checkbox(state);
        }

        // ── Phase 2 — table editing ───────────────────────────────
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
                // (removes up to `tab_width` leading spaces).  Outside a list
                // it's a no-op.
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

        // ── Phase 8 — link navigation ───────────────────────────────
        // These are App-level actions: the event loop intercepts them
        // before calling `edit_ops::apply`, so reaching them here means
        // the App isn't wired up (e.g. a unit test driving edit_ops
        // directly).  No-op so tests don't panic.
        Action::FollowLinkUnderCursor | Action::NavigateBack | Action::NavigateForward => {}

        // ── Phase 9 — cheat-sheet popover ──────────────────────────
        // App-level: opens the cheat-sheet overlay.  edit_ops knows
        // nothing about UI widgets; reaching this arm means the App
        // wasn't wired up, which is the normal situation in unit
        // tests that drive edit_ops directly.
        Action::ShowCheatSheet => {}

        // ── Phase 10 — command palette + configuration overlays ───
        // Same story as `ShowCheatSheet`: these all open UI overlays
        // owned by the App (palette state, settings overlay, keybinds
        // overlay, OS file open, HTML export, file reload).  They are
        // intercepted in `App::handle_app_action` *before*
        // `edit_ops::apply` is called; reaching one of these arms
        // means a unit test is dispatching them directly with no App
        // attached, so a no-op is the correct behaviour.
        Action::ShowCommandPalette
        | Action::ShowMarkdownCheatSheet
        | Action::OpenSettings
        | Action::OpenKeybinds
        | Action::OpenConfigFolder
        | Action::ExportHtml
        | Action::ReloadFromDisk => {}
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
fn paste_from_clipboard(state: &EditorState) -> String {
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

/// Move the cursor one visual step horizontally when inside a table.
///
/// Each cell exposes a contiguous range `[cell_first, cell_end]` of valid
/// cursor positions — the content characters plus the trailing pad space
/// (the "cell-end" position, where typing appends to the cell).  Stepping
/// past `cell_end` or before `cell_first` hops directly to the adjacent
/// cell's cell-end, wrapping across row boundaries (and skipping the
/// alignment row).  The column separator `|`, the leading pad space, the
/// outer `|` borders, and the newline between rows are all skipped — they
/// are never valid cursor positions.
///
/// Returns `true` when the move was handled (cursor updated, or deliberately
/// clamped at a table edge).  Returns `false` when the caller should fall
/// back to ordinary cursor movement — the cursor isn't in a table, or sits
/// on the alignment row (which stays hand-editable via char-step).
fn table_move_horizontal(state: &mut EditorState, forward: bool) -> bool {
    let Some((_, info)) = current_table(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    let Some((row, col)) = cursor_cell(&info, byte) else {
        return false;
    };
    if info.rows[row].kind == RowKind::Alignment {
        return false;
    }
    let Some(cell_first) = cell_cursor_offset(&info, row, col) else {
        return false;
    };
    let Some(cell_end) = cell_end_cursor_offset(&info, row, col) else {
        return false;
    };

    if forward {
        if byte >= cell_end {
            if let Some((nr, nc)) = adjacent_cell(&info, row, col, /*forward=*/ true) {
                if let Some(target) = cell_end_cursor_offset(&info, nr, nc) {
                    set_cursor_byte(state, target);
                }
            }
            // At far edge of the table: stay put rather than walking onto
            // the trailing `|` or newline.
            return true;
        }
        let new_char = (state.cursor.offset + 1).min(state.buffer.len_chars());
        let new_byte = state.buffer.rope().char_to_byte(new_char);
        set_cursor_byte(state, new_byte.min(cell_end));
    } else {
        if byte <= cell_first {
            if let Some((pr, pc)) = adjacent_cell(&info, row, col, /*forward=*/ false) {
                if let Some(target) = cell_end_cursor_offset(&info, pr, pc) {
                    set_cursor_byte(state, target);
                }
            }
            return true;
        }
        let new_char = state.cursor.offset.saturating_sub(1);
        let new_byte = state.buffer.rope().char_to_byte(new_char);
        set_cursor_byte(state, new_byte.max(cell_first));
    }
    true
}

/// Find the cell adjacent to `(row, col)` in the given direction.  Wraps
/// across row boundaries, skipping the alignment row.  Returns `None` at
/// the outer edges of the table.
fn adjacent_cell(
    info: &TableInfo,
    row: usize,
    col: usize,
    forward: bool,
) -> Option<(usize, usize)> {
    if forward {
        if col + 1 < info.col_count {
            return Some((row, col + 1));
        }
        let mut nr = row + 1;
        if nr == 1 {
            nr = 2;
        }
        if nr < info.rows.len() {
            Some((nr, 0))
        } else {
            None
        }
    } else {
        if col > 0 {
            return Some((row, col - 1));
        }
        if row == 0 {
            return None;
        }
        let pr = if row == 2 { 0 } else { row - 1 };
        if pr == 1 {
            return None;
        }
        Some((pr, info.col_count.saturating_sub(1)))
    }
}

/// Set the cursor to a specific byte offset in the buffer.  Clamps to
/// buffer bounds and keeps `preferred_col` coherent.
fn set_cursor_byte(state: &mut EditorState, target_byte: usize) {
    let source_len = state.buffer.contents().len();
    let clamped = target_byte.min(source_len);
    let char_off = state.buffer.rope().byte_to_char(clamped);
    state.cursor.offset = char_off.min(state.buffer.len_chars());
}

// ── Table editing helpers ─────────────────────────────────────────────────────
//
// `table_edit` works in byte offsets over the source text.  `EditorState`
// works in char offsets.  These helpers convert between the two and wrap the
// pure `table_edit` primitives into stateful operations on `EditorState`.

/// Char offset of the cursor, as a byte offset into `buffer.contents()`.
fn cursor_byte(state: &EditorState) -> usize {
    state.buffer.rope().char_to_byte(state.cursor.offset)
}

/// Look up the table surrounding the cursor.  Returns the source snapshot
/// alongside the parsed `TableInfo` so the caller doesn't have to re-fetch
/// `buffer.contents()`.
fn current_table(state: &EditorState) -> Option<(String, TableInfo)> {
    // Raw mode is a plain-text view — the user should be able to type `|`
    // literally, move one char at a time through cell boundaries, and have
    // Tab/Enter insert a literal tab/newline rather than jumping cells.
    // Suppressing table detection here short-circuits every table-aware
    // code path (`|` escaping, InsertTab → TableNextCell, Backspace merging,
    // etc.) without any callsite changes.
    if state.mode == Mode::Raw {
        return None;
    }
    let source = state.buffer.contents();
    let byte = cursor_byte(state);
    find_table_at(&source, byte).map(|info| (source, info))
}

/// Is the cursor currently inside a GFM table?
fn cursor_in_table(state: &EditorState) -> bool {
    current_table(state).is_some()
}

/// Is the cursor currently sitting on the alignment row (`|---|---|`) of a
/// GFM table?  Used to skip that row during vertical cursor movement — the
/// alignment row is a structural artefact and should never be a navigation
/// target.
fn cursor_on_alignment_row(state: &EditorState) -> bool {
    let Some((_, info)) = current_table(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    cursor_cell(&info, byte)
        .and_then(|(row, _)| info.rows.get(row))
        .map(|row| row.kind == RowKind::Alignment)
        .unwrap_or(false)
}

/// Move the cursor up/down by one line, then — if that move landed on a
/// table's alignment row or inside a hidden (zero-rendered-line) block —
/// advance once more in the same direction so the cursor skips the
/// structural artefact entirely.  Honours `visual_line_nav` for all moves
/// so wrapped lines, tables, and hidden HTML comments cooperate.
fn move_line_skipping_alignment(state: &mut EditorState, down: bool, viewport_width: usize) {
    let step = |state: &mut EditorState| {
        if down {
            if state.visual_line_nav && viewport_width > 0 {
                state.move_down_visual(viewport_width);
            } else {
                state.cursor.move_down(&state.buffer);
            }
        } else if state.visual_line_nav && viewport_width > 0 {
            state.move_up_visual(viewport_width);
        } else {
            state.cursor.move_up(&state.buffer);
        }
    };
    step(state);
    if cursor_on_alignment_row(state) {
        step(state);
    }
    // Walk past any consecutive hidden (zero-rendered-line) blocks — HTML
    // comments don't produce rendered rows, so stopping on one would leave
    // the cursor "stuck" at a position the user can't see in hybrid view.
    // The loop is bounded by repeated `prev_offset` equality: when the step
    // function can't advance further (top/bottom of buffer), we stop rather
    // than spin.
    let mut safety = 32usize;
    while cursor_on_hidden_block(state) && safety > 0 {
        let prev_offset = state.cursor.offset;
        step(state);
        if state.cursor.offset == prev_offset {
            break;
        }
        safety -= 1;
    }
}

/// True iff the cursor currently falls inside a block with zero rendered
/// lines whose source text is an HTML comment.  Used by vertical-navigation
/// and mode-transition code to skip over invisible source bytes so the
/// cursor never stalls on a line the user can't see in hybrid view.
///
/// Intentionally specific to comments rather than "any zero-own block":
/// suppressed blank lines (when `preserve_blank_lines` is false) also have
/// zero own but are bytes the cursor may legitimately want to land on.
fn cursor_on_hidden_block(state: &EditorState) -> bool {
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

/// When the cursor is inside a table, move it to the cell directly above or
/// below (preserving the column and skipping the alignment row) and land on
/// the end-of-content of that cell.  Returns `true` when the move happened;
/// `false` tells the caller to fall back to ordinary vertical motion (e.g.
/// when the cursor is at the top/bottom edge of the table).
fn try_move_cell_vertical(
    state: &mut EditorState,
    down: bool,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    let Some((_, info)) = current_table(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    let Some((row, col)) = cursor_cell(&info, byte) else {
        return false;
    };

    let target = if down {
        let t = row + 1;
        if t == 1 {
            2
        } else {
            t
        }
    } else if row == 2 {
        0
    } else if row < 2 {
        return false;
    } else {
        row.saturating_sub(1)
    };

    if target >= info.rows.len() || target == row {
        return false;
    }

    let Some(target_byte) = cell_end_cursor_offset(&info, target, col) else {
        return false;
    };
    let char_off = state.buffer.rope().byte_to_char(target_byte);
    state.cursor.offset = char_off.min(state.buffer.len_chars());
    state.cursor.preferred_col = state.cursor.line_col(&state.buffer).1;
    state.update_cursor_block();
    state.ensure_cursor_visible(viewport_height, viewport_width);
    true
}

/// Apply a `table_edit`-produced `EditDelta` (whose offsets are **bytes**)
/// to the editor state by first converting those offsets into rope char
/// offsets.  The caller supplies the *post-edit* cursor byte position; we
/// convert it too after the buffer has been mutated.
fn apply_byte_delta(state: &mut EditorState, byte_delta: EditDelta, cursor_byte_target: usize) {
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
    state.cursor.preferred_col = state.cursor.line_col(&state.buffer).1;
    state.update_cursor_block();
}

/// Move the cursor to the end-of-content of `(row_idx, col_idx)` in the
/// table described by a *fresh* re-parse.  Re-parsing is required when the
/// buffer has changed since `info` was produced.  Landing on cell-end means
/// the user can immediately start typing to append to the cell.
fn jump_to_cell(
    state: &mut EditorState,
    row_idx: usize,
    col_idx: usize,
    viewport_height: usize,
    viewport_width: usize,
) {
    let source = state.buffer.contents();
    let byte = cursor_byte(state);
    if let Some(info) = find_table_at(&source, byte) {
        let row = row_idx.min(info.rows.len().saturating_sub(1));
        let col = col_idx.min(info.col_count.saturating_sub(1));
        if let Some(target_byte) = cell_end_cursor_offset(&info, row, col) {
            let char_off = state.buffer.rope().byte_to_char(target_byte);
            state.cursor.offset = char_off.min(state.buffer.len_chars());
            state.cursor.preferred_col = state.cursor.line_col(&state.buffer).1;
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
        }
    }
}

/// Tab / TableNextCell: move to the next cell; at the end of the last row,
/// append a fresh empty row and land in its first cell.
fn table_next_cell(state: &mut EditorState, viewport_height: usize, viewport_width: usize) {
    let Some((_, info)) = current_table(state) else {
        return;
    };
    let byte = cursor_byte(state);
    let Some((row, col)) = cursor_cell(&info, byte) else {
        return;
    };

    let next_col = col + 1;
    if next_col < info.col_count {
        jump_to_cell(state, row, next_col, viewport_height, viewport_width);
        return;
    }
    // Last cell of the row — advance to first cell of next data row,
    // skipping the alignment row (which the cursor never lands on via Tab).
    let mut next_row = row + 1;
    if next_row == 1 {
        next_row = 2;
    }
    if next_row < info.rows.len() {
        jump_to_cell(state, next_row, 0, viewport_height, viewport_width);
        return;
    }
    // End of table — append a new row below.
    let (byte_delta, new_row_idx) = table_edit::insert_row(&info, row, true);
    let inserted_len = byte_delta.inserted.len();
    let insertion_byte = byte_delta.offset;
    apply_byte_delta(state, byte_delta, insertion_byte);
    // After the insert, the new row occupies bytes insertion_byte..insertion_byte+inserted_len.
    // Jump to first cell of the new row.
    let _ = inserted_len;
    jump_to_cell(state, new_row_idx, 0, viewport_height, viewport_width);
}

/// Shift+Tab / TablePrevCell: move to the previous cell; at the first cell
/// of the first data row, stay put (don't cross into the alignment row).
fn table_prev_cell(state: &mut EditorState, viewport_height: usize, viewport_width: usize) {
    let Some((_, info)) = current_table(state) else {
        return;
    };
    let byte = cursor_byte(state);
    let Some((row, col)) = cursor_cell(&info, byte) else {
        return;
    };
    if col > 0 {
        jump_to_cell(state, row, col - 1, viewport_height, viewport_width);
        return;
    }
    // First cell of this row — jump to last cell of previous row.
    let prev_row = row.saturating_sub(1);
    // Skip alignment row (index 1).
    let prev_row = if prev_row == 1 { 0 } else { prev_row };
    if prev_row < info.rows.len() && prev_row != row {
        let last_col = info.col_count.saturating_sub(1);
        jump_to_cell(state, prev_row, last_col, viewport_height, viewport_width);
    }
}

/// Enter / TableNextRow: move down one data row, creating a new row when
/// pressed on the last row so the user never has to leave the table.
fn table_next_row(state: &mut EditorState, viewport_height: usize, viewport_width: usize) {
    let Some((_, info)) = current_table(state) else {
        return;
    };
    let byte = cursor_byte(state);
    let Some((row, col)) = cursor_cell(&info, byte) else {
        return;
    };
    let mut target = row + 1;
    // Skip alignment row.
    if target == 1 {
        target = 2;
    }
    if target < info.rows.len() {
        jump_to_cell(state, target, col, viewport_height, viewport_width);
        return;
    }
    // Append a new row.
    let (byte_delta, new_row_idx) = table_edit::insert_row(&info, row, true);
    let insertion_byte = byte_delta.offset;
    apply_byte_delta(state, byte_delta, insertion_byte);
    jump_to_cell(state, new_row_idx, col, viewport_height, viewport_width);
}

/// TablePrevRow: move up one row, skipping the alignment row.
fn table_prev_row(state: &mut EditorState, viewport_height: usize, viewport_width: usize) {
    let Some((_, info)) = current_table(state) else {
        return;
    };
    let byte = cursor_byte(state);
    let Some((row, col)) = cursor_cell(&info, byte) else {
        return;
    };
    // From row 2 (first data row) go to header row 0, skipping alignment at 1.
    let target = if row == 2 { 0 } else { row.saturating_sub(1) };
    if target == 1 || target >= info.rows.len() {
        return;
    }
    jump_to_cell(state, target, col, viewport_height, viewport_width);
}

/// Reorder the cursor's row up or down by one.  No-op outside a table, on
/// the header/alignment rows, or at the edge of the data rows.
fn table_move_row(
    state: &mut EditorState,
    down: bool,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((_, info)) = current_table(state) else {
        return;
    };
    let byte = cursor_byte(state);
    let Some((row, col)) = cursor_cell(&info, byte) else {
        return;
    };
    if row < 2 {
        return;
    }
    let other = if down { row + 1 } else { row.saturating_sub(1) };
    if other < 2 || other >= info.rows.len() || other == row {
        return;
    }
    let Some(byte_delta) = table_edit::swap_rows(&info, row, other) else {
        return;
    };
    apply_byte_delta(state, byte_delta, byte);
    // The row the user is "carrying" moved; land at the same column in the
    // new position.
    jump_to_cell(state, other, col, viewport_height, viewport_width);
}

/// Reorder the cursor's column left or right by one.  No-op outside a table
/// or at the edge columns.
fn table_move_column(
    state: &mut EditorState,
    right: bool,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((_, info)) = current_table(state) else {
        return;
    };
    let byte = cursor_byte(state);
    let Some((row, col)) = cursor_cell(&info, byte) else {
        return;
    };
    let other = if right {
        col + 1
    } else {
        col.saturating_sub(1)
    };
    if other >= info.col_count || other == col {
        return;
    }
    let Some(byte_delta) = table_edit::swap_columns(&info, col, other) else {
        return;
    };
    apply_byte_delta(state, byte_delta, byte);
    jump_to_cell(state, row, other, viewport_height, viewport_width);
}

/// Insert a new empty row above or below the cursor's row.  Inserting
/// "above" the header or alignment row is clamped to the first data row.
fn table_insert_row(
    state: &mut EditorState,
    below: bool,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((_, info)) = current_table(state) else {
        return;
    };
    let byte = cursor_byte(state);
    let Some((row, _col)) = cursor_cell(&info, byte) else {
        return;
    };
    let (byte_delta, new_row_idx) = table_edit::insert_row(&info, row, below);
    let insertion_byte = byte_delta.offset;
    apply_byte_delta(state, byte_delta, insertion_byte);
    jump_to_cell(state, new_row_idx, 0, viewport_height, viewport_width);
}

/// Insert a new empty column to the left or right of the cursor's column.
fn table_insert_column(
    state: &mut EditorState,
    right: bool,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((_, info)) = current_table(state) else {
        return;
    };
    let byte = cursor_byte(state);
    let Some((row, col)) = cursor_cell(&info, byte) else {
        return;
    };
    let byte_delta = table_edit::insert_column(&info, col, right);
    let insertion_byte = byte_delta.offset;
    apply_byte_delta(state, byte_delta, insertion_byte);
    let new_col = if right { col + 1 } else { col };
    jump_to_cell(state, row, new_col, viewport_height, viewport_width);
}

/// Delete the cursor's row.  Header and alignment rows are protected.  If
/// the cursor was on the last data row, the cursor moves to the row above.
fn table_delete_row(state: &mut EditorState, viewport_height: usize, viewport_width: usize) {
    let Some((_, info)) = current_table(state) else {
        return;
    };
    let byte = cursor_byte(state);
    let Some((row, col)) = cursor_cell(&info, byte) else {
        return;
    };
    // Only data rows (index >= 2) may be deleted.
    if info.rows[row].kind != RowKind::Data {
        return;
    }
    let Some(byte_delta) = table_edit::delete_row(&info, row) else {
        return;
    };
    let delta_offset = byte_delta.offset;
    apply_byte_delta(state, byte_delta, delta_offset);
    // After deletion, land on the row that took the deleted row's place, or
    // if the deleted row was the last data row, on the row above it.
    let target_row = if row < info.rows.len() - 1 {
        row
    } else {
        row.saturating_sub(1).max(2)
    };
    jump_to_cell(state, target_row, col, viewport_height, viewport_width);
}

/// Delete the cursor's column.  Refuses to delete the last remaining column
/// (that would destroy the table structure).
fn table_delete_column(state: &mut EditorState, viewport_height: usize, viewport_width: usize) {
    let Some((_, info)) = current_table(state) else {
        return;
    };
    let byte = cursor_byte(state);
    let Some((row, col)) = cursor_cell(&info, byte) else {
        return;
    };
    let Some(byte_delta) = table_edit::delete_column(&info, col) else {
        return;
    };
    let delta_offset = byte_delta.offset;
    apply_byte_delta(state, byte_delta, delta_offset);
    // After deletion, land on the column that now occupies this position,
    // clamped to the new column count.
    let new_col = col.min(info.col_count.saturating_sub(2));
    jump_to_cell(state, row, new_col, viewport_height, viewport_width);
}

// ── List editing helpers ──────────────────────────────────────────────────────
//
// `list_edit` (byte-oriented) mirrors `table_edit`.  These helpers look up the
// list at the cursor, convert between byte and char offsets, and apply the
// resulting `EditDelta`s through `apply_byte_delta`.

/// If the cursor is inside a Markdown list, dispatch `Enter` to either
/// `exit_list` (when the item is empty) or `continue_item` (otherwise), and
/// return `true` to signal the caller that the newline has been handled.
/// Returns `false` when the cursor is not in a list — the caller should fall
/// through to a plain newline insert.
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
        list_edit::exit_list(&info, &source, byte)
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

/// Indent the cursor's list item one level (adds `tab_width` spaces of
/// indent, resets ordered numbering to 1, renumbers the surrounding outer
/// list).  Returns `true` when handled so the caller can skip the fallback
/// plain-tab insertion.
fn list_indent(state: &mut EditorState) -> bool {
    let Some((source, info)) = current_list(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    let tab_width = state.tab_width;
    let Some(res) = list_edit::indent_item(&info, &source, byte, tab_width) else {
        return false;
    };
    apply_byte_delta(state, res.delta, res.cursor_byte);
    true
}

/// Outdent the cursor's list item one level (removes up to `tab_width`
/// leading spaces).  No-op (returns `false`) when the cursor isn't in a
/// list or when the item is already at the outermost indent.
fn list_outdent(state: &mut EditorState) -> bool {
    let Some((source, info)) = current_list(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    let tab_width = state.tab_width;
    let Some(res) = list_edit::outdent_item(&info, &source, byte, tab_width) else {
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

/// After a paste that may have landed in or adjacent to an ordered list,
/// renumber the surrounding list so the sequence stays monotonic.  No-op for
/// bullet lists or when the cursor is outside a list.
fn list_renumber_at_cursor(state: &mut EditorState) {
    let Some((source, info)) = current_list(state) else {
        return;
    };
    let byte_before = cursor_byte(state);
    if let Some(delta) = list_edit::renumber_list(&info, &source) {
        apply_byte_delta(state, delta, byte_before);
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
/// entire marker prefix (including the preceding `\n`, when one exists) so
/// the user never has to remove the marker character-by-character.  The edit
/// merges the current item's content with the end of the preceding line
/// (or, for the first item, just removes the marker).  Returns `true` when
/// the edit was applied.
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

    // Delete from the end of the preceding line (its `\n`) through
    // content_start.  For the very first line of the buffer (no preceding
    // line), delete from byte 0 through content_start.
    let delete_start = if item.start > 0 && source.as_bytes().get(item.start - 1) == Some(&b'\n') {
        item.start - 1
    } else {
        item.start
    };
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
        // Normal char-step, but if we'd land before the next item's content
        // (because we stepped onto a marker char, which shouldn't happen for
        // correctly-skipped cursors), clamp to content_start.
        let new_char = (state.cursor.offset + 1).min(state.buffer.len_chars());
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
        let new_char = state.cursor.offset.saturating_sub(1);
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
