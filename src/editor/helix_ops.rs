pub mod actions;
pub mod error;
pub mod find;
pub mod insert;
pub mod line;
pub mod motion;
pub mod navigation;
pub mod paste;
pub mod search;

pub use error::HelixError;
pub use motion::{select_word, WordMotion};

use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

use crate::document::{Buffer, Selection};
use crate::editor::vim_ops::{execute_operator, OpRange, OpResult, Operator};
use crate::editor::EditorState;

/// Apply an operator to the editor's current Helix selection after checking its guards.
/// `linewise` describes the register; the selected text is always the exact edit range.
pub fn apply_operator(
    state: &mut EditorState,
    op: Operator,
    linewise: bool,
) -> Result<OpResult, HelixError> {
    if state.readonly {
        return Err(HelixError::ReadOnly);
    }
    normalize_selection(state);
    let selection = state
        .selection
        .unwrap_or_else(|| Selection::new(state.cursor.offset));
    let (start, end) = normalize_range(&state.buffer, selection.range());
    if start == end {
        if op == Operator::Change {
            state.selection = None;
        }
        return Ok(OpResult {
            register_text: String::new(),
            linewise,
            enter_insert: op == Operator::Change,
        });
    }

    if op != Operator::Yank {
        let register_text = state.buffer.slice_to_string(start, end);
        actions::replace_range(state, start, end, "")?;
        if op == Operator::Change {
            state.selection = None;
        }
        return Ok(OpResult {
            register_text,
            linewise,
            enter_insert: op == Operator::Change,
        });
    }
    let range = OpRange::Chars(start..end);

    let selection_before = state.selection;
    let cursor_before = state.cursor;
    let mut result = execute_operator(state, op, range);
    result.linewise = linewise;
    state.selection = selection_before;
    state.cursor = cursor_before;
    state.update_cursor_block();
    Ok(result)
}

fn boundary_before(buffer: &Buffer, offset: usize) -> usize {
    snap_boundary(buffer, offset, false)
}

fn boundary_after(buffer: &Buffer, offset: usize) -> usize {
    snap_boundary(buffer, offset, true)
}

fn snap_boundary(buffer: &Buffer, offset: usize, forward: bool) -> usize {
    let rope = buffer.rope();
    let offset = offset.min(rope.len_chars());
    if (forward && offset == 0) || (!forward && offset == rope.len_chars()) {
        return offset;
    }
    // Starting one character across makes the snap inclusive of an existing boundary.
    let probe = if forward { offset - 1 } else { offset + 1 };
    let byte = rope.char_to_byte(probe);
    let mut cursor = GraphemeCursor::new(byte, rope.len_bytes(), true);
    let (mut chunk, mut chunk_start, _, _) = rope.chunk_at_byte(byte);
    loop {
        let result = if forward {
            cursor.next_boundary(chunk, chunk_start)
        } else {
            cursor.prev_boundary(chunk, chunk_start)
        };
        match result {
            Ok(Some(boundary)) => return rope.byte_to_char(boundary),
            Ok(None) => return if forward { rope.len_chars() } else { 0 },
            Err(GraphemeIncomplete::NextChunk) => {
                (chunk, chunk_start, _, _) = rope.chunk_at_byte(chunk_start + chunk.len());
            }
            Err(GraphemeIncomplete::PrevChunk) => {
                (chunk, chunk_start, _, _) = rope.chunk_at_byte(chunk_start - 1);
            }
            Err(GraphemeIncomplete::PreContext(end)) => {
                let (context, start, _, _) = rope.chunk_at_byte(end - 1);
                cursor.provide_context(context, start);
            }
            Err(GraphemeIncomplete::InvalidOffset) => unreachable!("rope chunk contains cursor"),
        }
    }
}

/// Snap a half-open range outward to grapheme boundaries.
pub fn normalize_range(buffer: &Buffer, (start, end): (usize, usize)) -> (usize, usize) {
    let len = buffer.len_chars();
    let start = start.min(len);
    let end = end.min(len);
    let start = boundary_before(buffer, start);
    let end = boundary_after(buffer, end);
    (start.min(end), end.max(start))
}

/// Cursor offset for an oriented half-open selection.
pub fn cursor_offset(buffer: &Buffer, selection: Selection) -> usize {
    let (lo, hi) = selection.range();
    if selection.is_empty() {
        return lo;
    }
    if selection.anchor <= selection.active {
        boundary_before(buffer, hi.saturating_sub(1))
    } else {
        lo
    }
}

/// Set the editor's sole Helix selection and place the cursor on its moving-end grapheme.
pub fn set_selection(state: &mut EditorState, selection: Selection) {
    let len = state.buffer.len_chars();
    let (lo, hi) = selection.range();
    let lo = boundary_before(&state.buffer, lo.min(len));
    let hi = boundary_after(&state.buffer, hi.min(len));
    let (anchor, active) = if selection.anchor <= selection.active {
        (lo, hi)
    } else {
        (hi, lo)
    };
    let (anchor, active) = if lo == hi && len > 0 {
        let at = lo.min(len);
        state.flush_parsed_if_dirty();
        let empty_cell =
            crate::editor::vim_ops::cell_scope(state).filter(|cell| cell.start == cell.end);
        if let Some(cell) = empty_cell {
            (cell.start, cell.start)
        } else if at < len {
            (at, boundary_after(&state.buffer, at + 1))
        } else {
            (at, boundary_before(&state.buffer, at.saturating_sub(1)))
        }
    } else {
        (anchor, active)
    };
    state.selection = Some(Selection { anchor, active });
    state.place_cursor(cursor_offset(&state.buffer, Selection { anchor, active }));
    if state.visual_line_nav && state.viewport_width > 0 {
        state.cursor.preferred_col = state.current_visual_col(state.viewport_width);
    }
}

/// Normalize the current selection, or create the current grapheme selection if absent.
pub fn normalize_selection(state: &mut EditorState) {
    let selection = state
        .selection
        .unwrap_or_else(|| Selection::new(state.cursor.offset));
    let cursor = state.cursor;
    set_selection(state, selection);
    if state.cursor.offset == cursor.offset {
        state.cursor.preferred_col = cursor.preferred_col;
    }
}

/// Reduce the selection to the grapheme under the cursor, or keep it empty in an empty buffer.
pub fn collapse_selection(state: &mut EditorState) {
    set_selection(state, Selection::new(state.cursor.offset));
}

/// Exchange the fixed and moving ends without changing the selected text.
pub fn flip_selection(state: &mut EditorState) {
    if let Some(selection) = state.selection {
        set_selection(
            state,
            Selection {
                anchor: selection.active,
                active: selection.anchor,
            },
        );
    }
}

/// Select a destination range, optionally extending from the fixed grapheme in Select mode.
/// Empty ranges represent a point destination and snap to the grapheme under that point.
pub(crate) fn select_range(state: &mut EditorState, (start, end): (usize, usize), extend: bool) {
    if !extend {
        set_selection(
            state,
            Selection {
                anchor: start,
                active: end,
            },
        );
        return;
    }
    normalize_selection(state);
    let selection = state.selection.expect("normalized selection");
    let (start, end) = normalize_range(&state.buffer, (start, end));
    if selection.is_empty() && start == end && start == selection.active {
        set_selection(state, Selection::new(start));
        return;
    }
    let fixed = if selection.anchor <= selection.active {
        selection.anchor
    } else {
        boundary_before(&state.buffer, selection.anchor.saturating_sub(1))
    };
    let fixed_end = boundary_after(&state.buffer, fixed.saturating_add(1));
    let end = if start == end {
        boundary_after(&state.buffer, end.saturating_add(1))
    } else {
        end
    };
    let selection = if start >= fixed {
        Selection {
            anchor: fixed,
            active: end.max(fixed_end),
        }
    } else {
        Selection {
            anchor: end.max(fixed_end),
            active: start,
        }
    };
    set_selection(state, selection);
}

/// Move the active end while preserving the fixed graphème in Select mode.
pub fn select_cursor(state: &mut EditorState, target: usize, extend: bool) {
    select_range(state, (target, target), extend);
}
