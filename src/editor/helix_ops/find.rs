use crate::document::{next_grapheme_offset, prev_grapheme_offset, Selection};
use crate::editor::vim_ops::cell_scope;
use crate::editor::EditorState;

use super::{select_cursor, set_selection};

/// Find a grapheme by its leading character, across source lines except inside rendered cells.
pub fn find(state: &mut EditorState, command: char, target: char, count: u32, extend: bool) {
    state.flush_parsed_if_dirty();
    let backward = command.is_uppercase();
    let till = command.eq_ignore_ascii_case(&'t');
    let origin = state.cursor.offset;
    let (start, end) = cell_scope(state)
        .map(|s| (s.start, s.end))
        .unwrap_or((0, state.buffer.len_chars()));
    let mut offset = origin;
    let mut remaining = count.clamp(1, 9999);
    loop {
        let next = if backward {
            prev_grapheme_offset(&state.buffer, offset)
        } else {
            next_grapheme_offset(&state.buffer, offset)
        };
        if next == offset || next < start || next >= end {
            return;
        }
        offset = next;
        if state.buffer.rope().char(offset) == target {
            remaining -= 1;
            if remaining == 0 {
                break;
            }
        }
    }
    if till {
        offset = if backward {
            next_grapheme_offset(&state.buffer, offset)
        } else {
            prev_grapheme_offset(&state.buffer, offset)
        };
    }
    if extend {
        select_cursor(state, offset, true);
    } else if backward {
        set_selection(
            state,
            Selection {
                anchor: next_grapheme_offset(&state.buffer, origin),
                active: offset,
            },
        );
    } else {
        set_selection(
            state,
            Selection {
                anchor: origin,
                active: next_grapheme_offset(&state.buffer, offset),
            },
        );
    }
}
