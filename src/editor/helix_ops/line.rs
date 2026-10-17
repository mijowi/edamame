use crate::document::Selection;
use crate::editor::EditorState;

use super::{normalize_selection, set_selection};

/// Source lines touched by a half-open selection; an ending newline belongs to its line.
pub fn touched_lines(state: &EditorState) -> (usize, usize) {
    let (lo, hi) = state
        .selection
        .map(|s| s.range())
        .unwrap_or((state.cursor.offset, state.cursor.offset));
    (
        state.buffer.char_to_line(lo),
        state.buffer.char_to_line(hi.saturating_sub(1).max(lo)),
    )
}

pub fn select_lines(state: &mut EditorState, count: u32, extend_next: bool) {
    normalize_selection(state);
    let selection = state.selection.expect("normalized selection");
    let (first, mut last) = touched_lines(state);
    let start = state.buffer.line_to_char(first);
    let end = line_after(state, last);
    if !extend_next
        && selection
            .selected_text(&state.buffer)
            .trim_matches(['\r', '\n'])
            .is_empty()
    {
        return;
    }
    if extend_next {
        let extra = count.clamp(1, 9999) as usize - usize::from(selection.range() != (start, end));
        last = last
            .saturating_add(extra)
            .min(state.buffer.line_count().saturating_sub(1));
    }
    let end = line_after(state, last);
    set_selection(
        state,
        if selection.anchor <= selection.active {
            Selection {
                anchor: start,
                active: end,
            }
        } else {
            Selection {
                anchor: end,
                active: start,
            }
        },
    );
}

pub fn line_after(state: &EditorState, line: usize) -> usize {
    if line + 1 < state.buffer.line_count() {
        state.buffer.line_to_char(line + 1)
    } else {
        state.buffer.len_chars()
    }
}
