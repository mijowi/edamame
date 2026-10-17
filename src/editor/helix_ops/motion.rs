//! Word selections use the moving boundary, which differs from Vim's cursor motions.

use crate::document::{class_of, next_grapheme_offset, prev_grapheme_offset, Buffer, Selection};
use crate::editor::vim_ops::cell_scope;
use crate::editor::EditorState;

use super::{cursor_offset, normalize_selection, select_cursor, set_selection};

#[derive(Clone, Copy)]
pub enum WordMotion {
    Forward,
    End,
    Backward,
}

pub fn select_word(
    state: &mut EditorState,
    motion: WordMotion,
    big: bool,
    count: u32,
    extend: bool,
) {
    normalize_selection(state);
    state.flush_parsed_if_dirty();
    let selection = state.selection.expect("normalized selection");
    let mut result = word_range(&state.buffer, selection, motion, big, count);
    if let Some(cell) = cell_scope(state) {
        result.anchor = result.anchor.clamp(cell.start, cell.end);
        result.active = result.active.clamp(cell.start, cell.end);
        if result.is_empty() && cell.end > cell.start {
            result = Selection {
                anchor: prev_grapheme_offset(&state.buffer, cell.end).max(cell.start),
                active: cell.end,
            };
        }
    }
    if extend {
        let target = cursor_offset(&state.buffer, result);
        select_cursor(state, target, true);
    } else {
        set_selection(state, result);
    }
}

fn word_range(
    buf: &Buffer,
    selection: Selection,
    motion: WordMotion,
    big: bool,
    count: u32,
) -> Selection {
    let backward = matches!(motion, WordMotion::Backward);
    let len = buf.len_chars();
    if len == 0 {
        return selection;
    }
    let forward_selection = selection.anchor < selection.active;
    let mut head = match (backward, forward_selection) {
        (true, true) => prev_grapheme_offset(buf, selection.active),
        (false, false) => next_grapheme_offset(buf, selection.active),
        _ => selection.active,
    };
    let mut anchor = if backward {
        next_grapheme_offset(buf, head)
    } else {
        prev_grapheme_offset(buf, head)
    };
    let mut result = selection;
    for _ in 0..count.clamp(1, 9999) {
        if (backward && head == 0) || (!backward && head == len) {
            break;
        }
        // A fresh word motion crosses line endings without selecting only the separator.
        let prior_head = head;
        while (backward && head > 0 && buf.rope().char(head - 1) == '\n')
            || (!backward && head < len && buf.rope().char(head) == '\n')
        {
            if backward {
                head -= 1;
            } else {
                head += 1;
            }
        }
        if head != prior_head {
            anchor = head;
        }
        let start = head;
        loop {
            if (backward && head == 0) || (!backward && head == len) {
                break;
            }
            let (previous, next) = if backward {
                ((head < len).then_some(head), head - 1)
            } else {
                (head.checked_sub(1), head)
            };
            let boundary = previous.is_none_or(|previous| {
                let a = buf.rope().char(previous);
                let b = buf.rope().char(next);
                class_of(buf, previous, big) != class_of(buf, next, big)
                    && match motion {
                        WordMotion::Forward => b == '\n' || !b.is_whitespace(),
                        WordMotion::End | WordMotion::Backward => !a.is_whitespace() || b == '\n',
                    }
            });
            if boundary {
                if head != start {
                    break;
                }
                anchor = head;
            }
            if backward {
                head -= 1;
            } else {
                head += 1;
            }
        }
        result = Selection {
            anchor,
            active: head,
        };
    }
    result
}
