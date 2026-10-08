//! Command-line buffer editing for the `:` / `/` / `?` prompts: a view-agnostic text field
//! over [`CmdLineState`] that reports a [`CmdLineStep`] to `vim_feed`, which decides what a
//! submitted line means.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::CmdLineState;
use crate::document::{
    str_byte_index, str_next_grapheme, str_prev_grapheme, str_remove_grapheme_before,
};

/// What feeding one key to the command line decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdLineStep {
    Editing,
    /// `Enter`; the text may be empty.
    Submit(String),
    /// `Esc`, or `Backspace` past the start — close the prompt with no action.
    Cancel,
}

/// Feed one key to the command line, mutating `cl` in place.
pub fn feed_key(cl: &mut CmdLineState, key: KeyEvent) -> CmdLineStep {
    match key.code {
        KeyCode::Enter => CmdLineStep::Submit(cl.input.clone()),
        KeyCode::Esc => CmdLineStep::Cancel,
        KeyCode::Backspace => {
            if cl.cursor == 0 {
                // vim closes the prompt on backspace over an empty line.
                if cl.input.is_empty() {
                    return CmdLineStep::Cancel;
                }
                return CmdLineStep::Editing;
            }
            // A whole grapheme cluster, as the editor's own Backspace removes.
            cl.cursor = str_remove_grapheme_before(&mut cl.input, cl.cursor);
            CmdLineStep::Editing
        }
        KeyCode::Left => {
            cl.cursor = str_prev_grapheme(&cl.input, cl.cursor);
            CmdLineStep::Editing
        }
        KeyCode::Right => {
            cl.cursor = str_next_grapheme(&cl.input, cl.cursor);
            CmdLineStep::Editing
        }
        KeyCode::Home => {
            cl.cursor = 0;
            CmdLineStep::Editing
        }
        KeyCode::End => {
            cl.cursor = cl.input.chars().count();
            CmdLineStep::Editing
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            let idx = str_byte_index(&cl.input, cl.cursor);
            cl.input.insert(idx, c);
            cl.cursor += 1;
            CmdLineStep::Editing
        }
        // Chords and function keys are swallowed so the prompt keeps capturing.
        _ => CmdLineStep::Editing,
    }
}

/// Recall the next-older `history` entry. The first step stashes the live draft (restored
/// by [`history_next`]) and jumps to the newest entry; walking stops at the oldest.
pub fn history_prev(cl: &mut CmdLineState, history: &[String]) {
    if history.is_empty() {
        return;
    }
    let idx = match cl.history_idx {
        None => {
            cl.draft = cl.input.clone();
            history.len() - 1
        }
        Some(0) => return, // already at the oldest entry
        Some(i) => i.saturating_sub(1),
    };
    cl.history_idx = Some(idx);
    set_input(cl, history[idx].clone());
}

/// Step toward newer entries; past the newest, end the recall and restore the draft.
pub fn history_next(cl: &mut CmdLineState, history: &[String]) {
    let Some(idx) = cl.history_idx else {
        return;
    };
    if idx + 1 < history.len() {
        cl.history_idx = Some(idx + 1);
        set_input(cl, history[idx + 1].clone());
    } else {
        cl.history_idx = None;
        let draft = std::mem::take(&mut cl.draft);
        set_input(cl, draft);
    }
}

fn set_input(cl: &mut CmdLineState, text: String) {
    cl.cursor = text.chars().count();
    cl.input = text;
}

/// Insert a bracketed paste at the cursor. On a search prompt the payload is escaped first
/// (`search::escape`) so a multi-line snippet becomes a `\n`-joined query; on `:` it is an
/// ex command and keeps the plain strip. Any surviving line break is dropped.
pub fn paste_str(cl: &mut CmdLineState, text: &str) {
    let escaped;
    let text = if cl.kind.is_search() {
        escaped = crate::search::escape::escape(text);
        escaped.as_str()
    } else {
        text
    };
    for c in text.chars().filter(|c| *c != '\n' && *c != '\r') {
        let idx = str_byte_index(&cl.input, cl.cursor);
        cl.input.insert(idx, c);
        cl.cursor += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::vim::state::CmdLineKind;

    fn cl() -> CmdLineState {
        CmdLineState::new(CmdLineKind::SearchForward)
    }

    fn ch(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn typing_appends_and_advances_cursor() {
        let mut s = cl();
        assert_eq!(feed_key(&mut s, ch('f')), CmdLineStep::Editing);
        feed_key(&mut s, ch('o'));
        feed_key(&mut s, ch('o'));
        assert_eq!(s.input, "foo");
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn enter_submits_and_esc_cancels() {
        let mut s = cl();
        feed_key(&mut s, ch('x'));
        assert_eq!(
            feed_key(&mut s, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            CmdLineStep::Submit("x".to_owned())
        );
        assert_eq!(
            feed_key(&mut s, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            CmdLineStep::Cancel
        );
    }

    #[test]
    fn backspace_deletes_then_cancels_on_empty() {
        let mut s = cl();
        feed_key(&mut s, ch('a'));
        let bs = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(feed_key(&mut s, bs), CmdLineStep::Editing);
        assert_eq!(s.input, "");
        assert_eq!(feed_key(&mut s, bs), CmdLineStep::Cancel);
    }

    #[test]
    fn cursor_moves_let_insertion_happen_mid_string() {
        let mut s = cl();
        for c in "fo".chars() {
            feed_key(&mut s, ch(c));
        }
        feed_key(&mut s, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        feed_key(&mut s, ch('X'));
        assert_eq!(s.input, "fXo");
        assert_eq!(s.cursor, 2);
    }

    /// Left, Right and Backspace step over a whole grapheme cluster, so the cursor never stops
    /// on a combining mark or inside a ZWJ sequence.
    #[test]
    fn cursor_and_backspace_step_by_grapheme_cluster() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut s = cl();
        paste_str(&mut s, "ae\u{0301}👨\u{200D}👩");
        assert_eq!(s.cursor, 6);
        feed_key(&mut s, key(KeyCode::Left));
        assert_eq!(s.cursor, 3, "over the family");
        feed_key(&mut s, key(KeyCode::Left));
        assert_eq!(s.cursor, 1, "over the accented e");
        feed_key(&mut s, key(KeyCode::Right));
        assert_eq!(s.cursor, 3);
        feed_key(&mut s, key(KeyCode::Backspace));
        assert_eq!(s.input, "a👨\u{200D}👩", "the base and its accent");
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn history_up_walks_older_then_down_restores_draft() {
        let history = vec!["w".to_owned(), "q".to_owned(), "wq".to_owned()];
        let mut s = cl();
        feed_key(&mut s, ch('a'));
        history_prev(&mut s, &history);
        assert_eq!(s.draft, "a");
        assert_eq!(s.input, "wq");
        assert_eq!(s.cursor, 2);
        assert_eq!(s.history_idx, Some(2));
        history_prev(&mut s, &history);
        history_prev(&mut s, &history);
        assert_eq!(s.input, "w");
        assert_eq!(s.history_idx, Some(0));
        history_prev(&mut s, &history);
        assert_eq!(s.input, "w");
        history_next(&mut s, &history);
        assert_eq!(s.input, "q");
        history_next(&mut s, &history);
        assert_eq!(s.input, "wq");
        history_next(&mut s, &history);
        assert_eq!(s.input, "a");
        assert_eq!(s.cursor, 1);
        assert_eq!(s.history_idx, None);
        history_next(&mut s, &history);
        assert_eq!(s.input, "a");
    }

    #[test]
    fn history_up_on_empty_history_is_a_noop() {
        let mut s = cl();
        feed_key(&mut s, ch('x'));
        history_prev(&mut s, &[]);
        assert_eq!(s.input, "x");
        assert_eq!(s.history_idx, None);
    }

    #[test]
    fn ctrl_chord_is_ignored_not_inserted() {
        let mut s = cl();
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(feed_key(&mut s, ctrl_c), CmdLineStep::Editing);
        assert_eq!(s.input, "");
    }
}
