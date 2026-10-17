use crate::editor::vim_ops::{table_paste_plan, TablePaste};
use crate::editor::EditorState;

use super::{actions::replace_range, line, normalize_selection, HelixError};

pub fn paste(
    state: &mut EditorState,
    text: &str,
    linewise: bool,
    after: bool,
    count: u32,
) -> Result<(), HelixError> {
    if text.is_empty() {
        return Ok(());
    }
    let count = count.clamp(1, 9999) as usize;
    let unit_len = text
        .len()
        .checked_add(usize::from(linewise && !text.ends_with('\n')));
    if unit_len
        .and_then(|len| len.checked_mul(count))
        .is_none_or(|n| n > 64 * 1024 * 1024)
    {
        return Err(HelixError::PasteTooLarge);
    }
    normalize_selection(state);
    state.flush_parsed_if_dirty();
    let (lo, hi) = state.selection.expect("normalized selection").range();
    let mut at = if after { hi } else { lo };
    let unit = if linewise && !text.ends_with('\n') {
        format!("{text}\n")
    } else {
        text.to_owned()
    };
    let mut payload = unit.repeat(count);
    if linewise {
        let (first, last) = line::touched_lines(state);
        at = if after {
            line::line_after(state, last)
        } else {
            state.buffer.line_to_char(first)
        };
        let cursor = state.cursor;
        state.place_cursor(if after {
            hi.saturating_sub(1).max(lo)
        } else {
            lo
        });
        let plan = table_paste_plan(state, &payload, true, after);
        state.cursor = cursor;
        state.update_cursor_block();
        match plan {
            TablePaste::Refused => return Err(HelixError::TablePaste),
            TablePaste::RowsAt(offset) => at = offset,
            TablePaste::NotATable => {}
        }
        if at > 0 && state.buffer.rope().char(at - 1) != '\n' {
            payload = format!("\n{}", payload.strip_suffix('\n').unwrap_or(&payload));
        }
    }
    replace_range(state, at, at, &payload)
}
