use crate::document::EditDelta;
use crate::editor::vim_ops::{
    cell_scope, first_non_blank, line_end_offset, open_list_continue, open_table_row,
};
use crate::editor::{EditorState, Mode};

use super::{actions::replace_range, line, normalize_selection, HelixError};

pub fn enter(state: &mut EditorState, command: char, h: usize, w: usize) -> Result<(), HelixError> {
    if state.readonly {
        return Err(HelixError::ReadOnly);
    }
    normalize_selection(state);
    state.flush_parsed_if_dirty();
    let (lo, hi) = state.selection.expect("normalized selection").range();
    let current_line = state.buffer.char_to_line(state.cursor.offset);
    let at = match command {
        'i' => lo,
        'a' => hi,
        'I' => cell_scope(state)
            .map(|s| s.start)
            .unwrap_or_else(|| first_non_blank(&state.buffer, current_line)),
        'A' => cell_scope(state)
            .map(|s| s.end)
            .unwrap_or_else(|| line_end_offset(&state.buffer, current_line)),
        'o' | 'O' => {
            let below = command == 'o';
            let (first, last) = line::touched_lines(state);
            let target_line = if below { last } else { first };
            state.place_cursor(first_non_blank(&state.buffer, target_line));
            state.selection = None;
            state.history.break_group();
            if !open_table_row(state, below, h, w) && !open_list_continue(state, below) {
                if !below && open_first_list_item(state)? {
                    // The shared continuation opens after an item; its first-item case needs an insertion before it.
                } else {
                    let at = if below {
                        line_end_offset(&state.buffer, target_line)
                    } else {
                        state.buffer.line_to_char(target_line)
                    };
                    state.apply_delta(EditDelta {
                        offset: at,
                        removed: String::new(),
                        inserted: "\n".into(),
                    });
                    if !below {
                        state.place_cursor(at);
                    }
                }
            }
            state.history.break_group();
            state.cursor.offset
        }
        _ => unreachable!(),
    };
    state.place_cursor(at);
    state.selection = None;
    state.ensure_cursor_visible(h, w);
    Ok(())
}

fn open_first_list_item(state: &mut EditorState) -> Result<bool, HelixError> {
    if state.mode == Mode::Raw {
        return Ok(false);
    }
    let source = state.contents();
    let byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    let Some(info) = crate::editor::list_edit::find_list_at(&source, byte) else {
        return Ok(false);
    };
    let Some(item) = info.items.first() else {
        return Ok(false);
    };
    let mut prefix = source[item.start..item.marker_end].to_owned();
    if item.task.is_some() {
        prefix.push_str("[ ] ");
    }
    let at = state.buffer.rope().byte_to_char(item.start);
    let landing = at + prefix.chars().count();
    prefix.push('\n');
    replace_range(state, at, at, &prefix)?;
    state.place_cursor(landing);
    Ok(true)
}
