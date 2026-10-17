use unicode_segmentation::UnicodeSegmentation;

use crate::document::{EditDelta, Selection};
use crate::editor::table_edit_ops::table_at;
use crate::editor::vim_ops::range_breaks_a_table;
use crate::editor::{EditorState, Mode};

use super::{collapse_selection, line, normalize_range, set_selection, HelixError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    Replace(char),
    SwapCase,
    Indent,
    Outdent,
    Join,
}

/// Validate the actual removed range and final payload before the first write.
pub fn replace_range(
    state: &mut EditorState,
    start: usize,
    end: usize,
    text: &str,
) -> Result<(), HelixError> {
    if state.readonly {
        return Err(HelixError::ReadOnly);
    }
    state.flush_parsed_if_dirty();
    let (start, end) = normalize_range(&state.buffer, (start, end));
    let rope = state.buffer.rope();
    let (a, b) = (rope.char_to_byte(start), rope.char_to_byte(end));
    let mut row_insertion = false;
    if !text.is_empty() {
        if let Some(info) = table_at(state, a) {
            if !(a <= info.start && b >= info.end) {
                let at_row = info.rows.iter().position(|r| r.start == a);
                let whole_rows = at_row.is_some_and(|i| i >= 2)
                    && (info.rows.iter().any(|r| r.end == b) || a == b);
                let valid_rows = whole_rows
                    && text.lines().all(crate::editor::table_edit::has_cell_pipe)
                    && (text.ends_with('\n') || b == rope.len_bytes());
                row_insertion = a == b && valid_rows;
                if !valid_rows && (text.contains(['|', '\n', '\r']) || whole_rows) {
                    return Err(HelixError::CellPaste);
                }
            }
        }
    }
    if !row_insertion {
        if let Some(reason) = range_breaks_a_table(state, a, b) {
            return Err(HelixError::TableRange(reason));
        }
    }
    let removed = state.buffer.slice_to_string(start, end);
    if removed != text {
        let mut delta = EditDelta {
            offset: start,
            removed,
            inserted: text.to_owned(),
        };
        let line_start = rope.line_to_char(rope.char_to_line(start));
        let first = rope
            .slice(line_start..start)
            .chars()
            .chain(text.chars())
            .chain(rope.slice(end..).chars())
            .next();
        // Only list markers or indented continuations can reach the renumbering helper.
        let may_renumber =
            first.is_some_and(|c| matches!(c, ' ' | '\t' | '-' | '*' | '+' | '0'..='9'));
        if state.mode == Mode::Rendered && may_renumber {
            let source = state.contents();
            let mut rewritten = String::with_capacity(source.len() - (b - a) + text.len());
            rewritten.push_str(&source[..a]);
            rewritten.push_str(text);
            rewritten.push_str(&source[b..]);
            if let Some(numbering) =
                crate::editor::list_edit::renumber_list_block(&rewritten, a.min(rewritten.len()))
            {
                rewritten = numbering.apply_to_string(&rewritten);
            }
            if let Some(composed) = EditDelta::diff(&source, &rewritten) {
                delta = EditDelta {
                    offset: state.buffer.rope().byte_to_char(composed.offset),
                    removed: composed.removed,
                    inserted: composed.inserted,
                };
            } else {
                return Ok(());
            }
        }
        state.history.break_group();
        state.apply_delta(delta);
        state.history.break_group();
    }
    if text.is_empty() {
        state.place_cursor(start);
        collapse_selection(state);
    } else {
        set_selection(
            state,
            Selection {
                anchor: start,
                active: start + text.chars().count(),
            },
        );
    }
    Ok(())
}

pub fn replace_selection(state: &mut EditorState, text: &str) -> Result<(), HelixError> {
    super::normalize_selection(state);
    let (start, end) = state.selection.expect("normalized selection").range();
    replace_range(state, start, end, text)
}

/// Compute the complete replacement before passing it to the guarded range application.
pub fn transform(
    state: &mut EditorState,
    transform: Transform,
    count: u32,
) -> Result<(), HelixError> {
    super::normalize_selection(state);
    let (mut start, mut end) = state.selection.expect("normalized selection").range();
    if matches!(
        transform,
        Transform::Indent | Transform::Outdent | Transform::Join
    ) {
        let (first, last) = line::touched_lines(state);
        if transform == Transform::Join && first == last {
            return Ok(());
        }
        start = state.buffer.line_to_char(first);
        end = line::line_after(state, last);
        state.flush_parsed_if_dirty();
        if state.mode == Mode::Rendered
            && crate::editor::vim_ops::lines_touch_a_table(state, first, last)
        {
            return Err(HelixError::TableTransform);
        }
    }
    let old = state.buffer.slice_to_string(start, end);
    let text = match transform {
        Transform::Replace(c) => {
            let c = c.to_string();
            old.graphemes(true)
                .map(|g| if g.ends_with('\n') { g } else { &c })
                .collect()
        }
        Transform::SwapCase => old
            .chars()
            .map(|c| {
                if c.is_uppercase() {
                    c.to_lowercase().collect::<String>()
                } else {
                    c.to_uppercase().collect::<String>()
                }
            })
            .collect(),
        Transform::Indent | Transform::Outdent => {
            let width = crate::constants::INDENT_WIDTH * count.clamp(1, 9999) as usize;
            let mut result = String::new();
            for line in old.split_inclusive('\n') {
                if transform == Transform::Indent {
                    if !line.trim().is_empty() {
                        result.push_str(&" ".repeat(width));
                    }
                    result.push_str(line);
                } else {
                    let remove = if line.starts_with('\t') {
                        1
                    } else {
                        line.chars().take(width).take_while(|c| *c == ' ').count()
                    };
                    result.push_str(&line[remove..]);
                }
            }
            result
        }
        Transform::Join => {
            let trailing = old.ends_with('\n');
            let mut result = String::new();
            for (i, line) in old.trim_end_matches('\n').split('\n').enumerate() {
                if i == 0 {
                    result.push_str(line);
                } else {
                    let content = line.trim_start_matches([' ', '\t']);
                    if !content.is_empty() && !result.ends_with(char::is_whitespace) {
                        result.push(' ');
                    }
                    result.push_str(content);
                }
            }
            if trailing {
                result.push('\n');
            }
            result
        }
    };
    replace_range(state, start, end, &text)
}
