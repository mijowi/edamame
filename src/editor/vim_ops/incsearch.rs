//! Live `/` / `?` incremental search — vim's `incsearch`.
//!
//! While the user types a search command line, the document updates
//! live: a navigate-only [`SearchState`] is rebuilt from the input on
//! every keystroke, the cursor parks on the cursor-relative first match
//! (after the origin for `/`, before it for `?`), and the view scrolls
//! it into view.  Esc restores the pre-prompt cursor, scroll, and any
//! hlsearch session that was live when the prompt opened; Enter restores
//! them too, so the App-level `EnterSearch` path runs against the
//! original view and its semantics stay byte-identical to a preview-less
//! submit (the sibling `:s` preview makes the same promise — see
//! `vim_ops::preview`).
//!
//! Unlike the `:s` preview, incsearch never touches the buffer — there
//! is no revert delta, no version stamp, and none of the App-level gates
//! (autosave, mouse, search freshness) apply.  The transient session is
//! a real `EditorState::search`, so the hlsearch overlay painters and
//! the raw-reveal suppression work unchanged.

use crate::editor::EditorState;
use crate::search::SearchState;

/// State saved when an incsearch session starts (the first keystroke of
/// an open `/` / `?` prompt), restored when it ends.  Lives on
/// `VimState` — its lifetime is bounded by the command line's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncsearchSession {
    /// The hlsearch session that was live when the prompt opened,
    /// restored when the prompt closes (vim keeps the previous
    /// highlights when an incsearch is aborted).
    prior: Option<SearchState>,
    /// Cursor char offset at session start.
    saved_cursor: usize,
    /// Viewport scroll at session start.
    saved_scroll: usize,
}

/// Re-derive the live search from the current command-line text.  Starts
/// the session on the first call (stashing the prior hlsearch session
/// and view); an input that is empty or matches nothing shows no
/// highlights and returns the view to the origin, but keeps the session
/// alive for later keystrokes.  The focused match is resolved relative
/// to the *saved* cursor — the parked cursor never feeds back into the
/// next keystroke's resolution (the `:s` preview isolates its saved
/// cursor the same way).
pub fn update_incsearch(
    editor: &mut EditorState,
    session: &mut Option<IncsearchSession>,
    input: &str,
    forward: bool,
    viewport_height: usize,
    viewport_width: usize,
) {
    if session.is_none() {
        *session = Some(IncsearchSession {
            prior: editor.search.take(),
            saved_cursor: editor.cursor.offset,
            saved_scroll: editor.scroll,
        });
    }
    let (saved_cursor, saved_scroll) = {
        let s = session.as_ref().expect("session ensured above");
        (s.saved_cursor, s.saved_scroll)
    };
    // An empty input (or one the session can't represent) highlights
    // nothing; likewise a matchless one.  Vim shows the original view
    // while the pattern doesn't match.  A half-typed escape (`/a\`, or
    // `/\d` before the user backspaces) lands here too and is treated
    // the same way — deliberately no flash, since the user is still
    // typing; the error is reported on submit.
    let Ok(mut state) = SearchState::new(input.to_owned(), None) else {
        editor.search = None;
        editor.restore_view(saved_cursor, Some(saved_scroll));
        return;
    };
    state.ensure_fresh(&editor.buffer.contents(), editor.buffer.version());
    if state.matches.is_empty() {
        editor.search = None;
        editor.restore_view(saved_cursor, Some(saved_scroll));
        return;
    }
    let cursor_byte = editor
        .buffer
        .rope()
        .char_to_byte(saved_cursor.min(editor.buffer.len_chars()));
    state.focus_relative_to(cursor_byte, forward);
    editor.search = Some(state);
    editor.sync_cursor_to_search_focus();
    editor.scroll_cursor_comfortably_into_view(viewport_height, viewport_width);
}

/// End the session: restore the prior hlsearch session (if any) and the
/// pre-prompt cursor and scroll.  Called on both Esc and Enter — on
/// submit the restored view is what the App-level `EnterSearch` path
/// expects to resolve the cursor-relative focus against.  Returns `true`
/// when a session existed.
pub fn end_incsearch(editor: &mut EditorState, session: &mut Option<IncsearchSession>) -> bool {
    let Some(s) = session.take() else {
        return false;
    };
    editor.search = s.prior;
    editor.restore_view(s.saved_cursor, Some(s.saved_scroll));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::Buffer;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn editor(text: &str) -> EditorState {
        let mut st = EditorState::new(Buffer::from_str(text), theme());
        st.update_cursor_block();
        st
    }

    #[test]
    fn update_focuses_the_first_match_after_the_origin() {
        let mut st = editor("foo bar\nfoo");
        let mut session = None;
        update_incsearch(&mut st, &mut session, "foo", true, 24, 80);
        let s = st.search.as_ref().expect("session live");
        // The match at byte 0 starts at the cursor, not after it —
        // forward search wraps to the next occurrence.
        assert_eq!(s.focused_range(), Some(8..11));
        assert_eq!(st.cursor.offset, 8, "cursor parked on the focus");
    }

    #[test]
    fn backward_search_focuses_the_last_match_before_the_origin() {
        let mut st = editor("foo bar\nfoo");
        st.place_cursor(8); // at the start of the second "foo"
        let mut session = None;
        update_incsearch(&mut st, &mut session, "foo", false, 24, 80);
        let s = st.search.as_ref().expect("session live");
        assert_eq!(s.focused_range(), Some(0..3));
    }

    #[test]
    fn every_keystroke_resolves_from_the_saved_cursor_not_the_parked_one() {
        let mut st = editor("aa ab ac");
        let mut session = None;
        // "a" focuses the match after offset 0 → byte 1.
        update_incsearch(&mut st, &mut session, "a", true, 24, 80);
        assert_eq!(st.search.as_ref().unwrap().focused_range(), Some(1..2));
        // Narrowing to "ab" must resolve from the ORIGINAL cursor (0),
        // not from the parked position — the first "ab" is at byte 3.
        update_incsearch(&mut st, &mut session, "ab", true, 24, 80);
        assert_eq!(st.search.as_ref().unwrap().focused_range(), Some(3..5));
    }

    #[test]
    fn a_matchless_input_clears_highlights_and_restores_the_view() {
        let mut st = editor("foo bar");
        st.scroll = 0;
        let mut session = None;
        update_incsearch(&mut st, &mut session, "bar", true, 24, 80);
        assert!(st.search.is_some());
        update_incsearch(&mut st, &mut session, "barz", true, 24, 80);
        assert!(st.search.is_none(), "no match → no highlights");
        assert_eq!(st.cursor.offset, 0, "view back at the origin");
        assert!(session.is_some(), "the session survives for later keys");
        // Backspacing to a matching prefix resumes.
        update_incsearch(&mut st, &mut session, "bar", true, 24, 80);
        assert!(st.search.is_some());
    }

    #[test]
    fn end_restores_prior_session_cursor_and_scroll() {
        let mut st = editor("foo bar\nfoo");
        let mut prior = SearchState::new("bar".to_owned(), None).expect("valid");
        prior.ensure_fresh(&st.buffer.contents(), st.buffer.version());
        st.search = Some(prior.clone());
        st.place_cursor(5);
        st.scroll = 1;
        let mut session = None;
        update_incsearch(&mut st, &mut session, "foo", true, 24, 80);
        assert_eq!(st.search.as_ref().unwrap().query, "foo");
        assert!(end_incsearch(&mut st, &mut session));
        assert_eq!(
            st.search.as_ref().map(|s| s.query.as_str()),
            Some("bar"),
            "prior hlsearch session restored"
        );
        assert_eq!(st.cursor.offset, 5);
        assert_eq!(st.scroll, 1);
        assert!(!end_incsearch(&mut st, &mut session), "already ended");
    }
}
