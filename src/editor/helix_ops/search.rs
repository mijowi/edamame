use crate::editor::EditorState;
use crate::search::SearchState;

use super::{select_range, HelixError};

pub fn start(
    state: &mut EditorState,
    query: String,
    forward: bool,
    extend: bool,
) -> Result<(), HelixError> {
    let mut search = SearchState::new(query, None)?;
    search.ensure_fresh(&state.contents(), state.buffer.version());
    let byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    let origin = relative_origin(&search, byte, forward);
    search.focus_relative_to(origin, forward);
    state.search = Some(search);
    select_focused(state, extend)
}

pub fn next(
    state: &mut EditorState,
    forward: bool,
    count: u32,
    extend: bool,
) -> Result<(), HelixError> {
    state.ensure_search_fresh();
    let byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    let Some(search) = state.search.as_mut() else {
        return Err(HelixError::NoSearchPattern);
    };
    if search.matches.is_empty() {
        return Err(HelixError::NoMatches);
    }
    let origin = relative_origin(search, byte, forward);
    search.focus_relative_to(origin, forward);
    for _ in 1..count.clamp(1, 9999) {
        if forward {
            search.advance_focus();
        } else {
            search.retreat_focus();
        }
    }
    select_focused(state, extend)
}

pub fn select_focused(state: &mut EditorState, extend: bool) -> Result<(), HelixError> {
    let range = state
        .search
        .as_ref()
        .and_then(|s| s.focused_range())
        .ok_or(HelixError::NoMatches)?;
    let (start, end) = (
        state.buffer.rope().byte_to_char(range.start),
        state.buffer.rope().byte_to_char(range.end),
    );
    select_range(state, (start, end), extend);
    state.pending_focus_scroll = true;
    Ok(())
}

fn relative_origin(search: &SearchState, byte: usize, forward: bool) -> usize {
    search
        .matches
        .iter()
        .find(|r| r.contains(&byte))
        .map(|r| {
            if forward {
                r.end.saturating_sub(1)
            } else {
                r.start
            }
        })
        .unwrap_or(byte)
}
