//! Command-line buffer editing for the `:` / `/` / `?` prompts.
//!
//! A small, view-agnostic text field: it owns only the editing of a
//! [`CmdLineState`] (insert, backspace, cursor moves) and reports a
//! [`CmdLineStep`] back to `vim_feed`, which decides what a submitted
//! line means (a search query, an ex command, …).  First needed by CP8's
//! `/` / `?` search; CP9 reuses it for `:`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::CmdLineState;

/// What feeding one key to the command line decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdLineStep {
    /// Still editing — redraw the prompt, keep capturing keys.
    Editing,
    /// `Enter`: submit the typed text (which may be empty).
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
                // Backspace with nothing before the cursor closes the prompt
                // when the line is empty (vim closes the `/` on the last
                // backspace); otherwise it is a no-op.
                if cl.input.is_empty() {
                    return CmdLineStep::Cancel;
                }
                return CmdLineStep::Editing;
            }
            let idx = byte_index(&cl.input, cl.cursor - 1);
            cl.input.remove(idx);
            cl.cursor -= 1;
            CmdLineStep::Editing
        }
        KeyCode::Left => {
            cl.cursor = cl.cursor.saturating_sub(1);
            CmdLineStep::Editing
        }
        KeyCode::Right => {
            cl.cursor = (cl.cursor + 1).min(cl.input.chars().count());
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
        // A printable char (no `Ctrl`/`Alt`/`Super` chord) is inserted at the
        // cursor.  `Shift` is fine — it just gives an uppercase letter.
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            let idx = byte_index(&cl.input, cl.cursor);
            cl.input.insert(idx, c);
            cl.cursor += 1;
            CmdLineStep::Editing
        }
        // Anything else (a `Ctrl-*` chord, a function key, …) is ignored so
        // the prompt keeps capturing until an explicit submit/cancel.
        _ => CmdLineStep::Editing,
    }
}

/// Recall the previous (older) entry of `history` into the prompt. The first
/// step back stashes the live draft (so [`history_next`] can restore it) and
/// jumps to the newest entry; further steps walk toward older entries and stop
/// at the oldest. A no-op when `history` is empty. The cursor lands at the end
/// of the recalled line, as in vim.
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

/// Step toward newer entries of `history`. Stepping past the newest entry ends
/// the recall and restores the stashed draft. A no-op when not currently
/// browsing history.
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

/// Replace the prompt text and park the cursor at its end.
fn set_input(cl: &mut CmdLineState, text: String) {
    cl.cursor = text.chars().count();
    cl.input = text;
}

/// Insert pasted `text` at the cursor (a bracketed paste into an open
/// `/` `?` `:` prompt).  The command line is a single line, so a
/// multi-line paste would otherwise corrupt it (this is also what
/// triggered the buffer-paste panic before paste was routed here).
///
/// On a **search** prompt the payload is escaped first, so a pasted
/// multi-line snippet becomes a working `\n`-joined query rather than
/// silently losing its breaks — search queries are written in escape
/// syntax (`search::escape`).  An `:` prompt is an ex command, not a
/// search term, so it keeps the plain strip.  Either way the loop below
/// drops any break the transform didn't consume.
pub fn paste_str(cl: &mut CmdLineState, text: &str) {
    let escaped;
    let text = if cl.kind.is_search() {
        escaped = crate::search::escape::escape(text);
        escaped.as_str()
    } else {
        text
    };
    for c in text.chars().filter(|c| *c != '\n' && *c != '\r') {
        let idx = byte_index(&cl.input, cl.cursor);
        cl.input.insert(idx, c);
        cl.cursor += 1;
    }
}

/// Byte offset of char index `char_idx` within `s` (clamped to `s.len()`).
fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
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
        // Backspace on the now-empty line closes the prompt.
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

    #[test]
    fn history_up_walks_older_then_down_restores_draft() {
        let history = vec!["w".to_owned(), "q".to_owned(), "wq".to_owned()];
        let mut s = cl();
        feed_key(&mut s, ch('a'));
        // First Up stashes the half-typed draft and jumps to the newest entry.
        history_prev(&mut s, &history);
        assert_eq!(s.draft, "a");
        assert_eq!(s.input, "wq");
        assert_eq!(s.cursor, 2);
        assert_eq!(s.history_idx, Some(2));
        // Walk to the oldest, then stay put at the boundary.
        history_prev(&mut s, &history);
        history_prev(&mut s, &history);
        assert_eq!(s.input, "w");
        assert_eq!(s.history_idx, Some(0));
        history_prev(&mut s, &history); // already oldest — no-op
        assert_eq!(s.input, "w");
        // Down steps back toward newer entries…
        history_next(&mut s, &history);
        assert_eq!(s.input, "q");
        history_next(&mut s, &history);
        assert_eq!(s.input, "wq");
        // …and Down past the newest restores the original draft.
        history_next(&mut s, &history);
        assert_eq!(s.input, "a");
        assert_eq!(s.cursor, 1);
        assert_eq!(s.history_idx, None);
        // Down with no recall active is a no-op.
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
