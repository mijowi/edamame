use crate::config::Action;
use crate::document::{EditDelta, Selection};
use crate::editor::{EditorState, Mode};

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
    match action {
        // ── Quit ──────────────────────────────────────────────────
        Action::Quit => return true,

        // ── Mode transitions ──────────────────────────────────────
        Action::EnterEditMode => {
            if state.mode == Mode::Preview {
                sync_cursor_to_scroll(state, viewport_height);
                state.mode = Mode::Rendered;
            }
        }
        Action::ExitToPreview => {
            state.mode = Mode::Preview;
            state.selection = None;
        }
        Action::ToggleRawMode => {
            if state.mode == Mode::Preview {
                sync_cursor_to_scroll(state, viewport_height);
            }
            state.mode = match state.mode {
                Mode::Preview => Mode::Rendered,
                Mode::Rendered => Mode::Raw,
                Mode::Raw => Mode::Rendered,
            };
        }

        // ── Cursor movement ───────────────────────────────────────
        Action::MoveLeft => {
            enter_edit_if_preview(state, viewport_height);
            state.selection = None;
            state.cursor.move_left(&state.buffer);
            sync_preferred_visual(state, viewport_width);
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
        }
        Action::MoveRight => {
            enter_edit_if_preview(state, viewport_height);
            state.selection = None;
            state.cursor.move_right(&state.buffer);
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
            if state.visual_line_nav && viewport_width > 0 {
                state.move_up_visual(viewport_width);
            } else {
                state.cursor.move_up(&state.buffer);
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
            if state.visual_line_nav && viewport_width > 0 {
                state.move_down_visual(viewport_width);
            } else {
                state.cursor.move_down(&state.buffer);
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
            enter_edit_if_preview(state, viewport_height);
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
            insert_text(state, &ch.to_string());
        }
        Action::InsertTab => {
            if state.mode == Mode::Preview {
                sync_cursor_to_scroll(state, viewport_height);
                state.mode = Mode::Rendered;
                return false;
            }
            insert_text(state, "    "); // 4 spaces; TODO: use config tab_width
        }
        Action::Newline => {
            if state.mode == Mode::Preview {
                sync_cursor_to_scroll(state, viewport_height);
                state.mode = Mode::Rendered;
                return false;
            }
            insert_text(state, "\n");
        }
        Action::DeleteCharBack => {
            enter_edit_if_preview(state, viewport_height);
            if let Some(sel) = state.selection.take() {
                delete_selection(state, sel);
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
            let text = if let Some(sel) = &state.selection {
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

        // ── Phase 3+ — no-ops for now ─────────────────────────────
        Action::ToggleCheckbox => {}

        // ── Phase 2 — table editing; no-ops until implemented ─────
        Action::TableNextCell
        | Action::TablePrevCell
        | Action::TableNextRow
        | Action::TablePrevRow
        | Action::TableMoveRowUp
        | Action::TableMoveRowDown
        | Action::TableMoveColumnLeft
        | Action::TableMoveColumnRight
        | Action::TableInsertRowAbove
        | Action::TableInsertRowBelow
        | Action::TableInsertColumnLeft
        | Action::TableInsertColumnRight
        | Action::TableDeleteRow
        | Action::TableDeleteColumn => {}

        // Scroll-only aliases already handled above.
        _ => {}
    }

    false
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn enter_edit_if_preview(state: &mut EditorState, viewport_height: usize) {
    if state.mode == Mode::Preview {
        sync_cursor_to_scroll(state, viewport_height);
        state.mode = Mode::Rendered;
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

/// Write `text` to the OS clipboard if available; fall back to kill-ring.
fn copy_to_clipboard(state: &mut EditorState, text: String) {
    #[cfg(feature = "clipboard")]
    {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(&text);
        }
    }
    state.kill_ring = text;
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
