use crossterm::event::KeyCode;

use crate::config::Action;
use crate::document::{next_grapheme_offset, prev_grapheme_offset, Selection};
use crate::editor::edit_ops;
use crate::editor::vim_ops::{cell_scope, first_non_blank, visual_cell_step};
use crate::editor::{helix_ops, EditorState, Mode};

#[derive(Clone, Copy)]
pub enum Navigation {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    DocStart,
    DocEnd,
    LineStart,
    LineEnd,
    FirstNonBlank,
    GotoLine(u32),
}

pub fn navigate(
    state: &mut EditorState,
    motion: Navigation,
    count: u32,
    extend: bool,
    viewport_height: usize,
    viewport_width: usize,
) {
    state.flush_parsed_if_dirty();
    let selection = state
        .selection
        .unwrap_or_else(|| Selection::new(state.cursor.offset));
    let preferred_col = state.cursor.preferred_col;
    let count = count.clamp(1, 9999);

    match motion {
        Navigation::Left | Navigation::Right => {
            let forward = matches!(motion, Navigation::Right);
            for _ in 0..count {
                let previous = state.cursor.offset;
                if !(state.mode == Mode::Rendered && visual_cell_step(state, forward)) {
                    edit_ops::apply(
                        state,
                        if forward {
                            Action::MoveRight
                        } else {
                            Action::MoveLeft
                        },
                        viewport_height,
                        viewport_width,
                    );
                }
                let target = state.cursor.offset;
                state.selection = Some(selection);
                helix_ops::select_cursor(state, target, extend);
                if state.cursor.offset == previous {
                    break;
                }
            }
        }
        Navigation::Up | Navigation::Down => {
            let action = if matches!(motion, Navigation::Down) {
                Action::MoveDown
            } else {
                Action::MoveUp
            };
            for _ in 0..count {
                let previous = state.cursor.offset;
                edit_ops::apply(state, action.clone(), viewport_height, viewport_width);
                let target = state.cursor.offset;
                state.selection = Some(selection);
                helix_ops::select_cursor(state, target, extend);
                state.cursor.preferred_col = preferred_col;
                if state.cursor.offset == previous {
                    break;
                }
            }
        }
        other => {
            let target = match other {
                Navigation::Home | Navigation::LineStart => line_start_target(state),
                Navigation::End | Navigation::LineEnd => line_end_target(state),
                Navigation::FirstNonBlank => first_non_blank_target(state),
                Navigation::DocStart => first_non_blank(&state.buffer, 0),
                Navigation::DocEnd => {
                    let last = last_content_line(state);
                    first_non_blank(&state.buffer, last)
                }
                Navigation::GotoLine(line) => {
                    let last = last_content_line(state);
                    let line = (line.saturating_sub(1) as usize).min(last);
                    first_non_blank(&state.buffer, line)
                }
                Navigation::Left | Navigation::Right | Navigation::Up | Navigation::Down => {
                    unreachable!()
                }
            };
            state.selection = Some(selection);
            helix_ops::select_cursor(state, target, extend);
        }
    }
    if matches!(motion, Navigation::Up | Navigation::Down) {
        state.cursor.preferred_col = preferred_col;
    }
    state.ensure_cursor_visible(viewport_height, viewport_width);
}

fn line_start_target(state: &EditorState) -> usize {
    if let Some(scope) = cell_scope(state) {
        return scope.start;
    }
    state
        .buffer
        .line_to_char(state.buffer.char_to_line(state.cursor.offset))
}

fn line_end_target(state: &EditorState) -> usize {
    if let Some(scope) = cell_scope(state) {
        return prev_grapheme_offset(&state.buffer, scope.end).max(scope.start);
    }
    let line = state.buffer.char_to_line(state.cursor.offset);
    content_end(state, line)
}

fn first_non_blank_target(state: &EditorState) -> usize {
    if let Some(scope) = cell_scope(state) {
        let mut offset = scope.start;
        while offset < scope.end && state.buffer.rope().char(offset).is_whitespace() {
            offset = next_grapheme_offset(&state.buffer, offset);
        }
        return offset.min(scope.end);
    }
    first_non_blank(
        &state.buffer,
        state.buffer.char_to_line(state.cursor.offset),
    )
}

fn content_end(state: &EditorState, line: usize) -> usize {
    let start = state.buffer.line_to_char(line);
    let mut end = start + state.buffer.line(line).unwrap_or_default().chars().count();
    if end > start && state.buffer.rope().char(end - 1) == '\n' {
        end -= 1;
    }
    if end > start && state.buffer.rope().char(end - 1) == '\r' {
        end -= 1;
    }
    prev_grapheme_offset(&state.buffer, end).max(start)
}

fn last_content_line(state: &EditorState) -> usize {
    let len = state.buffer.len_chars();
    let mut line = state.buffer.char_to_line(len);
    if line > 0 && state.buffer.line_to_char(line) == len {
        line -= 1;
    }
    line
}

pub fn from_key(code: KeyCode) -> Option<Navigation> {
    match code {
        KeyCode::Char('h') | KeyCode::Left => Some(Navigation::Left),
        KeyCode::Char('j') | KeyCode::Down => Some(Navigation::Down),
        KeyCode::Char('k') | KeyCode::Up => Some(Navigation::Up),
        KeyCode::Char('l') | KeyCode::Right => Some(Navigation::Right),
        KeyCode::Home => Some(Navigation::Home),
        KeyCode::End => Some(Navigation::End),
        _ => None,
    }
}
