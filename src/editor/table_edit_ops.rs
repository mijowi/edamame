//! Table-aware editing helpers.
//!
//! Wraps the byte-oriented primitives in [`crate::editor::table_edit`] into
//! stateful operations on `EditorState`.  Conversion between byte offsets
//! (used by `table_edit`) and rope char offsets (used by `EditorState`) is
//! handled here so the rest of the editor never has to think about it.
//!
//! All public functions in this module are `pub(super)` — visible to the
//! sibling `edit_ops` module and the `editor` parent only, so call sites
//! remain unchanged.

use crate::document::{next_grapheme_offset, prev_grapheme_offset};
use crate::editor::edit_ops::{apply_byte_delta, cursor_byte, set_cursor_byte};
use crate::editor::table_edit::{
    self, cell_cursor_offset, cell_end_cursor_offset, cursor_cell, find_table_at, RowKind,
    TableInfo,
};
use crate::editor::{EditorState, Mode};

/// Look up the table surrounding the cursor.
///
/// Suppressed in [`Mode::Raw`]: in raw mode the user must be able to type
/// `|` literally, walk through cell boundaries one char at a time, and have
/// Tab/Enter insert a literal tab/newline rather than jumping cells.
/// Returning `None` here short-circuits every table-aware code path without
/// any callsite changes.
pub(super) fn current_table(state: &EditorState) -> Option<TableInfo> {
    table_at(state, cursor_byte(state))
}

/// Look up the table containing byte offset `byte`, for the callers that ask
/// about a position other than the cursor's (the vim range guards sweep a
/// selection).  Same `Mode::Raw` suppression as [`current_table`].
///
/// **Only the table's own lines are copied out of the rope.**  This sits on
/// the per-keystroke motion path via `vim_ops::table::cell_scope`, and
/// `Buffer::contents()` would allocate a copy of the *whole document* on
/// every `w` / `$` / `f`.  `find_table_at` only ever looks at the contiguous
/// run of table-looking lines around its cursor, so handing it exactly that
/// run — and shifting the byte offsets it reports back into document space —
/// is the same answer for a cost proportional to the table.
pub(super) fn table_at(state: &EditorState, byte: usize) -> Option<TableInfo> {
    if state.mode == Mode::Raw {
        return None;
    }
    let (base, run) = table_line_run(state, byte)?;
    let mut info = find_table_at(&run, byte - base)?;
    info.start += base;
    info.end += base;
    for row in &mut info.rows {
        row.start += base;
        row.end += base;
    }
    Some(info)
}

/// The contiguous run of table-looking lines around `byte`, as
/// `(base_byte, text)`.  `None` when `byte`'s own line isn't one — the same
/// first test [`find_table_at`] makes, so declining here never hides a table
/// it would have found.
fn table_line_run(state: &EditorState, byte: usize) -> Option<(usize, String)> {
    let rope = state.buffer.rope();
    let len_bytes = rope.len_bytes();
    let byte = byte.min(len_bytes);
    let line = rope.byte_to_line(byte);
    if !line_is_table_line(state, line) {
        return None;
    }
    let mut first = line;
    while first > 0 && line_is_table_line(state, first - 1) {
        first -= 1;
    }
    let last_line = rope.len_lines().saturating_sub(1);
    let mut last = line;
    while last < last_line && line_is_table_line(state, last + 1) {
        last += 1;
    }
    let start = rope.line_to_byte(first);
    let end = if last >= last_line {
        len_bytes
    } else {
        rope.line_to_byte(last + 1)
    };
    Some((start, rope.byte_slice(start..end).to_string()))
}

/// Does buffer line `idx` look like a table row?
fn line_is_table_line(state: &EditorState, idx: usize) -> bool {
    let rope = state.buffer.rope();
    if idx >= rope.len_lines() {
        return false;
    }
    let line = rope.line(idx);
    match line.as_str() {
        Some(s) => table_edit::is_table_line(s),
        None => table_edit::is_table_line(&line.to_string()),
    }
}

/// Is the cursor currently inside a GFM table?
pub(super) fn cursor_in_table(state: &EditorState) -> bool {
    current_table(state).is_some()
}

/// Is the cursor currently sitting on the alignment row (`|---|---|`) of a
/// GFM table?  Used to skip that row during vertical cursor movement — the
/// alignment row is a structural artefact and should never be a navigation
/// target.
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

/// Look up the cursor's `(table, byte, row, col)` quadruple in a single
/// call.  Replaces the repeated triple-`let` block that used to appear at
/// the top of every `table_*` helper.  The `byte` element is the cursor's
/// byte offset and is consumed by some helpers (e.g. `table_move_row` for
/// `apply_byte_delta`); when not needed, destructure with `_byte`.
fn cursor_table_cell(state: &EditorState) -> Option<(TableInfo, usize, usize, usize)> {
    let info = current_table(state)?;
    let byte = cursor_byte(state);
    let (row, col) = cursor_cell(&info, byte)?;
    Some((info, byte, row, col))
}

/// Skip the alignment row when moving downward through table rows: row 1
/// becomes row 2, every other row passes through unchanged.  Used wherever
/// "move down one row" needs to step *past* the alignment artefact.
fn skip_alignment_row(row: usize) -> usize {
    if row == 1 {
        2
    } else {
        row
    }
}

/// Horizontal cursor motion inside a table cell.  Steps one grapheme within
/// the current cell; on a cell-boundary, jumps to the cell-end of the
/// adjacent cell (skipping the alignment row); at the table's outer edge,
/// stays put rather than walking onto the trailing `|` or newline — these
/// are never valid cursor positions.
///
/// Returns `true` when the move was handled (cursor updated, or deliberately
/// clamped at a table edge).  Returns `false` when the caller should fall
/// back to ordinary cursor movement — the cursor isn't in a table, or sits
/// on the alignment row (which stays hand-editable via char-step).
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
            // At far edge of the table: stay put rather than walking onto
            // the trailing `|` or newline.
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
        Some((pr, info.col_count.saturating_sub(1)))
    }
}

/// When the cursor is inside a table, move it to the cell directly above or
/// below (preserving the column and skipping the alignment row) and land on
/// the end-of-content of that cell.  Returns `true` when the move happened;
/// `false` tells the caller to fall back to ordinary vertical motion (e.g.
/// when the cursor is at the top/bottom edge of the table).
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

/// Move the cursor to the end-of-content of `(row_idx, col_idx)` in the
/// table described by a *fresh* re-parse.  Re-parsing is required when the
/// buffer has changed since `info` was produced.  Landing on cell-end means
/// the user can immediately start typing to append to the cell.
pub(super) fn jump_to_cell(
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

    let next_col = col + 1;
    if next_col < info.col_count {
        jump_to_cell(state, row, next_col, viewport_height, viewport_width);
        return;
    }
    // Last cell of the row — advance to first cell of next data row,
    // skipping the alignment row (which the cursor never lands on via Tab).
    let next_row = skip_alignment_row(row + 1);
    if next_row < info.rows.len() {
        jump_to_cell(state, next_row, 0, viewport_height, viewport_width);
        return;
    }
    // End of table — append a new row below.
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
    // First cell of this row — jump to last cell of previous row.
    let prev_row = row.saturating_sub(1);
    // Skip alignment row (index 1) by stepping back to header.
    let prev_row = if prev_row == 1 { 0 } else { prev_row };
    if prev_row < info.rows.len() && prev_row != row {
        let last_col = info.col_count.saturating_sub(1);
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
    // Append a new row.
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
    // From row 2 (first data row) go to header row 0, skipping alignment at 1.
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
    // The row the user is "carrying" moved; land at the same column in the
    // new position.
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

/// Insert a new empty row above or below the cursor's row.  Inserting
/// "above" the header or alignment row is clamped to the first data row.
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

/// Delete the cursor's row.  Header and alignment rows are protected.  If
/// the cursor was on the last data row, the cursor moves to the row above.
pub(super) fn table_delete_row(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) {
    let Some((info, _byte, row, col)) = cursor_table_cell(state) else {
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
    // After deletion, land on the column that now occupies this position,
    // clamped to the new column count.
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
