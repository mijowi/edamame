//! Table-aware editing helpers: the byte-oriented primitives in
//! [`crate::editor::table_edit`] wrapped as stateful operations on
//! `EditorState`.  Byte ↔ rope-char conversion lives here, so the rest of the
//! editor never has to think about it.

use crate::document::{next_grapheme_offset, prev_grapheme_offset};
use crate::editor::edit_ops::{apply_byte_delta, cursor_byte, set_cursor_byte};
use crate::editor::table_edit::{
    self, cell_cursor_offset, cell_end_cursor_offset, cursor_cell, RowKind, TableInfo,
};
use crate::editor::{EditorState, Mode};

/// Look up the table surrounding the cursor.
///
/// Suppressed in [`Mode::Raw`], where `|` must be typeable, cell boundaries must
/// step one char at a time, and Tab/Enter must insert literally.  Returning
/// `None` short-circuits every table-aware path with no callsite changes.
pub(super) fn current_table(state: &EditorState) -> Option<TableInfo> {
    table_at(state, cursor_byte(state))
}

/// Look up the table containing byte offset `byte`, for callers asking about a
/// position other than the cursor's (the vim range guards sweep a selection).
/// Same `Mode::Raw` suppression as [`current_table`].
pub(super) fn table_at(state: &EditorState, byte: usize) -> Option<TableInfo> {
    if state.mode == Mode::Raw {
        return None;
    }
    locate_table(state, byte)
}

/// The table containing byte offset `byte`, in any mode: which lines it spans and where each
/// row's content starts come from the parse
/// ([`row_map::table_lines`](crate::document::row_map::table_lines)), its cells from the live
/// buffer.  For the hint line and the mouse, which ask about tables whatever the mode.
///
/// **Located by line, so a stale parse is safe.**  An in-line edit defers the re-parse
/// (`EditorState::parsed_dirty`) but never moves a line, so the parse still names the table's
/// lines; a table the edit just made or broke waits for the re-parse.  **Only the table's own
/// lines are copied out of the rope**: the vim motions ask on every keystroke, where
/// `Buffer::contents()` would copy the whole document.
pub fn locate_table(state: &EditorState, byte: usize) -> Option<TableInfo> {
    let rope = state.buffer.rope();
    let line = rope.byte_to_line(byte.min(rope.len_bytes()));
    let (first, cols) = crate::document::row_map::table_lines(&state.parsed, line)?;
    let last = first + cols.len();
    if last > rope.len_lines() {
        return None;
    }
    let base = rope.line_to_byte(first);
    let end = rope.line_to_byte(last);
    let text = rope.byte_slice(base..end).to_string();
    let mut info = table_edit::table_from_lines(&text, 0, &cols)?;
    info.start += base;
    info.end += base;
    for row in &mut info.rows {
        row.start += base;
        row.end += base;
    }
    Some(info)
}

/// Is the cursor currently inside a GFM table?
pub(super) fn cursor_in_table(state: &EditorState) -> bool {
    current_table(state).is_some()
}

/// Is the cursor on a table's alignment row (`|---|---|`)?  Vertical movement
/// skips it: it is a structural artifact, never a navigation target.
pub(super) fn cursor_on_alignment_row(state: &EditorState) -> bool {
    let Some(info) = current_table(state) else {
        return false;
    };
    let byte = cursor_byte(state);
    cursor_cell(&info, byte)
        .and_then(|(row, _)| info.rows.get(row))
        .map(|row| row.kind == RowKind::Alignment)
        .unwrap_or(false)
}

/// The cursor's `(table, byte, row, col)` quadruple, the prelude to every
/// `table_*` helper.  Destructure `byte` as `_byte` where it isn't needed.
fn cursor_table_cell(state: &EditorState) -> Option<(TableInfo, usize, usize, usize)> {
    let info = current_table(state)?;
    let byte = cursor_byte(state);
    let (row, col) = cursor_cell(&info, byte)?;
    Some((info, byte, row, col))
}

/// Skip the alignment row when moving down: row 1 becomes row 2, all else
/// passes through.
fn skip_alignment_row(row: usize) -> usize {
    if row == 1 {
        2
    } else {
        row
    }
}

/// Horizontal cursor motion inside a table cell: one grapheme within the cell,
/// or on a boundary the cell-end of the adjacent cell (skipping the alignment
/// row).  At the table's outer edge it stays put rather than walking onto the
/// trailing `|` or newline, which are never valid cursor positions.
///
/// `false` tells the caller to fall back to ordinary movement — the cursor isn't
/// in a table, or sits on the alignment row, which stays hand-editable.
pub(super) fn table_move_horizontal(state: &mut EditorState, forward: bool) -> bool {
    let Some((info, byte, row, col)) = cursor_table_cell(state) else {
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
            // At the table edge, stay put.
            return true;
        }
        let new_char = next_grapheme_offset(&state.buffer, state.cursor.offset);
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
        let new_char = prev_grapheme_offset(&state.buffer, state.cursor.offset);
        let new_byte = state.buffer.rope().char_to_byte(new_char);
        set_cursor_byte(state, new_byte.max(cell_first));
    }
    true
}

/// The cell adjacent to `(row, col)`, wrapping across rows and skipping the
/// alignment row.  `None` at the table's outer edges.
fn adjacent_cell(
    info: &TableInfo,
    row: usize,
    col: usize,
    forward: bool,
) -> Option<(usize, usize)> {
    if forward {
        if col < info.last_col(row) {
            return Some((row, col + 1));
        }
        let nr = skip_alignment_row(row + 1);
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
        Some((pr, info.last_col(pr)))
    }
}

/// Move to the cell above or below, preserving the column, skipping the
/// alignment row, and landing on cell-end.  `false` tells the caller to fall
/// back to ordinary vertical motion.
pub(super) fn try_move_cell_vertical(
    state: &mut EditorState,
    down: bool,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    let Some((info, _byte, row, col)) = cursor_table_cell(state) else {
        return false;
    };

    let target = if down {
        skip_alignment_row(row + 1)
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
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
    state.update_cursor_block();
    state.ensure_cursor_visible(viewport_height, viewport_width);
    true
}

/// Move to the end-of-content of `(row_idx, col_idx)`, re-parsing the table
/// because the buffer may have changed since a caller's `info` was produced.
/// Cell-end means the user can start typing straight away.
pub(super) fn jump_to_cell(
    state: &mut EditorState,
    row_idx: usize,
    col_idx: usize,
    viewport_height: usize,
    viewport_width: usize,
) {
    if let Some(info) = current_table(state) {
        let row = row_idx.min(info.rows.len().saturating_sub(1));
        let col = col_idx.min(info.col_count.saturating_sub(1));
        if let Some(target_byte) = cell_end_cursor_offset(&info, row, col) {
            let char_off = state.buffer.rope().byte_to_char(target_byte);
            state.cursor.offset = char_off.min(state.buffer.len_chars());
            state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
            state.update_cursor_block();
            state.ensure_cursor_visible(viewport_height, viewport_width);
        }
    }
}

/// Tab / TableNextCell: move to the next cell; at the end of the last row,
/// append a fresh empty row and land in its first cell.
pub(super) fn table_next_cell(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((info, _byte, row, col)) = cursor_table_cell(state) else {
        return;
    };

    // A short row's missing cells hold no text to land in, so Tab skips them.
    if col < info.last_col(row) {
        jump_to_cell(state, row, col + 1, viewport_height, viewport_width);
        return;
    }
    // Advance to the next data row, which Tab never lands on the alignment
    // row of.
    let next_row = skip_alignment_row(row + 1);
    if next_row < info.rows.len() {
        jump_to_cell(state, next_row, 0, viewport_height, viewport_width);
        return;
    }
    let (byte_delta, new_row_idx) = table_edit::insert_row(&info, row, true);
    let insertion_byte = byte_delta.offset;
    apply_byte_delta(state, byte_delta, insertion_byte);
    jump_to_cell(state, new_row_idx, 0, viewport_height, viewport_width);
}

/// Shift+Tab / TablePrevCell: move to the previous cell; at the first cell
/// of the first data row, stay put (don't cross into the alignment row).
pub(super) fn table_prev_cell(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((info, _byte, row, col)) = cursor_table_cell(state) else {
        return;
    };
    if col > 0 {
        jump_to_cell(state, row, col - 1, viewport_height, viewport_width);
        return;
    }
    // Jump to the last cell of the previous row, stepping over the alignment
    // row to the header.
    let prev_row = row.saturating_sub(1);
    let prev_row = if prev_row == 1 { 0 } else { prev_row };
    if prev_row < info.rows.len() && prev_row != row {
        let last_col = info.last_col(prev_row);
        jump_to_cell(state, prev_row, last_col, viewport_height, viewport_width);
    }
}

/// Enter / TableNextRow: move down one data row, creating a new row when
/// pressed on the last row so the user never has to leave the table.
pub(super) fn table_next_row(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((info, _byte, row, col)) = cursor_table_cell(state) else {
        return;
    };
    let target = skip_alignment_row(row + 1);
    if target < info.rows.len() {
        jump_to_cell(state, target, col, viewport_height, viewport_width);
        return;
    }
    let (byte_delta, new_row_idx) = table_edit::insert_row(&info, row, true);
    let insertion_byte = byte_delta.offset;
    apply_byte_delta(state, byte_delta, insertion_byte);
    jump_to_cell(state, new_row_idx, col, viewport_height, viewport_width);
}

/// TablePrevRow: move up one row, skipping the alignment row.
pub(super) fn table_prev_row(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((info, _byte, row, col)) = cursor_table_cell(state) else {
        return;
    };
    // From the first data row, go to the header, skipping alignment at 1.
    let target = if row == 2 { 0 } else { row.saturating_sub(1) };
    if target == 1 || target >= info.rows.len() {
        return;
    }
    jump_to_cell(state, target, col, viewport_height, viewport_width);
}

/// Reorder the cursor's row up or down by one.  No-op outside a table, on
/// the header/alignment rows, or at the edge of the data rows.
pub(super) fn table_move_row(
    state: &mut EditorState,
    down: bool,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((info, byte, row, col)) = cursor_table_cell(state) else {
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
    jump_to_cell(state, other, col, viewport_height, viewport_width);
}

/// Reorder the cursor's column left or right by one.  No-op outside a table
/// or at the edge columns.
pub(super) fn table_move_column(
    state: &mut EditorState,
    right: bool,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((info, byte, row, col)) = cursor_table_cell(state) else {
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

/// Insert an empty row; inserting above the header or alignment row is clamped
/// to the first data row.
pub(super) fn table_insert_row(
    state: &mut EditorState,
    below: bool,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((info, _byte, row, _col)) = cursor_table_cell(state) else {
        return;
    };
    let (byte_delta, new_row_idx) = table_edit::insert_row(&info, row, below);
    let insertion_byte = byte_delta.offset;
    apply_byte_delta(state, byte_delta, insertion_byte);
    jump_to_cell(state, new_row_idx, 0, viewport_height, viewport_width);
}

/// Insert a new empty column to the left or right of the cursor's column.
pub(super) fn table_insert_column(
    state: &mut EditorState,
    right: bool,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((info, _byte, row, col)) = cursor_table_cell(state) else {
        return;
    };
    let byte_delta = table_edit::insert_column(&info, col, right);
    let insertion_byte = byte_delta.offset;
    apply_byte_delta(state, byte_delta, insertion_byte);
    let new_col = if right { col + 1 } else { col };
    jump_to_cell(state, row, new_col, viewport_height, viewport_width);
}

/// Delete the cursor's row, leaving the header and alignment rows alone.  From
/// the last data row the cursor moves up.
pub(super) fn table_delete_row(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((info, _byte, row, col)) = cursor_table_cell(state) else {
        return;
    };
    if info.rows[row].kind != RowKind::Data {
        return;
    }
    let Some(byte_delta) = table_edit::delete_row(&info, row) else {
        return;
    };
    let delta_offset = byte_delta.offset;
    apply_byte_delta(state, byte_delta, delta_offset);
    // Land on the row that took this one's place, or the row above.
    let target_row = if row < info.rows.len() - 1 {
        row
    } else {
        row.saturating_sub(1).max(2)
    };
    jump_to_cell(state, target_row, col, viewport_height, viewport_width);
}

/// Delete the cursor's column, refusing the last one — that would destroy the
/// table structure.
pub(super) fn table_delete_column(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((info, _byte, row, col)) = cursor_table_cell(state) else {
        return;
    };
    let Some(byte_delta) = table_edit::delete_column(&info, col) else {
        return;
    };
    let delta_offset = byte_delta.offset;
    apply_byte_delta(state, byte_delta, delta_offset);
    let new_col = col.min(info.col_count.saturating_sub(2));
    jump_to_cell(state, row, new_col, viewport_height, viewport_width);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_alignment_row_only_remaps_one() {
        assert_eq!(skip_alignment_row(0), 0);
        assert_eq!(skip_alignment_row(1), 2);
        assert_eq!(skip_alignment_row(2), 2);
        assert_eq!(skip_alignment_row(3), 3);
        assert_eq!(skip_alignment_row(99), 99);
    }
}
