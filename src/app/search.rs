//! App-level search-and-replace flow: lifecycle, the in-flow action dispatcher (reached once
//! `search_safe_action` has allowed the action), replace / replace-all, and the deferred
//! post-replace advance timer (mirroring `diff_advance`). See `docs/dev/search-replace.md`.

use std::time::{Duration, Instant};

use crate::config::Action;
use crate::document::EditDelta;
use crate::search::SearchState;
use crate::ui::ModalKind;

use super::flash::MessageKind;
use super::App;

/// How long a fresh replacement stays focused before auto-advancing. Mirrors `DIFF_ADVANCE_DELAY`.
pub(super) const SEARCH_ADVANCE_DELAY: Duration = Duration::from_millis(350);

impl App {
    /// Open the search/replace modal, pre-filled from (and tearing down) any active flow.
    pub fn open_search_modal(&mut self) {
        let prefill = self
            .editor
            .search
            .as_ref()
            .map(|s| (s.query.clone(), s.replace.clone().unwrap_or_default()));
        if prefill.is_some() {
            self.exit_search_flow();
        }
        let (query, replace) = prefill.unwrap_or_default();
        self.modal_stack
            .push(Box::new(super::modal::SearchReplaceModal::new(
                query, replace,
            )));
        self.needs_draw = true;
    }

    /// Start a search flow for `query`; a non-empty `replace` makes it a replace flow. Zero
    /// matches never enters the flow.
    ///
    /// A read-only document gets the find half only, dropped here because this is the one
    /// funnel every caller passes through: a replace flow captures input and does a bare
    /// Preview → Rendered transition below that nothing downstream can refuse.
    pub(crate) fn enter_search_flow(&mut self, query: String, replace: Option<String>) {
        let replace = if self.editor.readonly {
            // Said out loud: the user typed into a field the modal offered them.
            if replace.is_some() {
                self.flash(
                    "This document is read-only — searching without replacing.".to_owned(),
                    MessageKind::Info,
                );
            }
            None
        } else {
            replace
        };
        let state = match SearchState::new(query.clone(), replace) {
            Ok(state) => state,
            Err(e) => {
                // Backstop; the modal validates input in its own error row.
                self.notify(e.to_string(), ModalKind::Warning);
                return;
            }
        };
        self.editor.enter_search(state);
        if self
            .editor
            .search
            .as_ref()
            .is_some_and(|s| s.matches.is_empty())
        {
            self.editor.exit_search();
            self.flash(format!("No matches for \"{query}\""), MessageKind::Info);
            self.needs_draw = true;
            return;
        }
        // A replace flow edits the buffer, so it leaves Preview; navigate-only flows stay.
        if self.editor.mode == crate::editor::Mode::Preview
            && self
                .editor
                .search
                .as_ref()
                .is_some_and(|s| s.is_replace_flow())
        {
            self.editor.mode = crate::editor::Mode::Rendered;
        }
        self.editor.sync_cursor_to_search_focus();
        // `enter_search` set `pending_focus_scroll`; `prepare_viewport` scrolls the match in.
        self.needs_draw = true;
    }

    /// Start a vim `/` `?` `*` `#` search: navigate-only, with the initial focus cursor-relative
    /// (first match after the cursor, or before it for a backward search, wrapping) rather than
    /// the modal path's first-match start. Zero matches never enters the flow.
    pub(crate) fn enter_vim_search(&mut self, query: String, forward: bool) {
        let state = match SearchState::new(query.clone(), None) {
            Ok(state) => state,
            Err(e) => {
                self.flash(e.to_string(), MessageKind::Info);
                return;
            }
        };
        self.editor.enter_search(state);
        if self
            .editor
            .search
            .as_ref()
            .is_some_and(|s| s.matches.is_empty())
        {
            self.editor.exit_search();
            self.flash(format!("No matches for \"{query}\""), MessageKind::Info);
            self.needs_draw = true;
            return;
        }
        // Match offsets are bytes.
        let cursor_byte = self
            .editor
            .buffer
            .rope()
            .char_to_byte(self.editor.cursor.offset);
        if let Some(s) = self.editor.search.as_mut() {
            s.focus_relative_to(cursor_byte, forward);
        }
        self.editor.sync_cursor_to_search_focus();
        self.needs_draw = true;
    }

    /// Whether the active flow captures input (default-denying buffer edits and routing flow
    /// keys to `dispatch_search_action`). Only a replace flow does: it needs the unmodified
    /// `Tab` / `r` / `a` keys. A navigate-only flow is a highlight overlay (vim `hlsearch`) that
    /// intercepts just next/prev and `Esc` and re-tracks matches as the buffer changes.
    pub(crate) fn search_flow_captures(&self) -> bool {
        self.editor
            .search
            .as_ref()
            .is_some_and(|s| s.is_replace_flow())
    }

    /// Tear down the active flow, if any, leaving the viewport where it is (search is a motion).
    /// Safe to call unconditionally; buffer-replacing paths use it so no stale match list
    /// survives a content swap.
    pub(crate) fn exit_search_flow(&mut self) {
        self.cancel_search_advance();
        if self.editor.search.is_some() {
            self.editor.exit_search();
            self.needs_draw = true;
        }
    }

    /// Dispatch one action while a flow is active. The caller has already passed it through
    /// `search_safe_action`.
    pub(super) fn dispatch_search_action(
        &mut self,
        action: Action,
        doc_height: usize,
        doc_width: usize,
    ) {
        // Free scrolling, as in diff mode: the viewport moves without dragging the cursor.
        if self.dispatch_flow_scroll(&action, doc_height, doc_width) {
            return;
        }
        match action {
            Action::SearchNext => self.search_move_focus(true, doc_height, doc_width),
            Action::SearchPrev => self.search_move_focus(false, doc_height, doc_width),
            Action::SearchReplace => self.search_replace_focused(),
            Action::SearchReplaceAll => self.search_replace_all(),
            Action::SearchExit => self.exit_search_flow(),
            Action::OpenSearch => {
                self.open_search_modal();
            }
            Action::Undo | Action::Redo => {
                crate::editor::edit_ops::apply(&mut self.editor, action, doc_height, doc_width);
                self.editor.ensure_search_fresh();
                if self.search_exit_if_empty() {
                    return;
                }
                self.editor.sync_cursor_to_search_focus();
                self.needs_draw = true;
            }
            Action::Quit => {
                // The flow stays active behind the confirm modal.
                if self.editor.dirty {
                    self.open_quit_confirm();
                } else {
                    self.should_quit = true;
                }
            }
            Action::Save
            | Action::SaveAs
            | Action::ShowCommandPalette
            | Action::ShowMarkdownCheatSheet
            | Action::BrowseTips
            | Action::ShowAbout
            | Action::OpenSettings
            | Action::OpenWelcome
            | Action::OpenKeybinds
            | Action::SwitchTheme
            | Action::CreateCustomTheme
            | Action::OpenConfigFolder => {
                self.handle_app_action(&action, doc_height, doc_width);
            }
            // Manual cursor movement supersedes a pending post-replace advance so the timer
            // cannot yank the cursor away afterward; Copy leaves both untouched.
            Action::MoveLeft
            | Action::MoveRight
            | Action::MoveUp
            | Action::MoveDown
            | Action::MoveWordLeft
            | Action::MoveWordRight
            | Action::MoveLineStart
            | Action::MoveLineEnd
            | Action::MoveDocStart
            | Action::MoveDocEnd
            | Action::SelectLeft
            | Action::SelectRight
            | Action::SelectUp
            | Action::SelectDown
            | Action::SelectAll => {
                self.cancel_search_advance();
                crate::editor::edit_ops::apply(&mut self.editor, action, doc_height, doc_width);
                self.needs_draw = true;
            }
            Action::Copy => {
                crate::editor::edit_ops::apply(&mut self.editor, action, doc_height, doc_width);
                self.flush_clipboard_write();
                self.needs_draw = true;
            }
            _ => {}
        }
    }

    /// Advance or retreat the focused match, cancelling any pending post-replace advance first.
    fn search_move_focus(&mut self, forward: bool, doc_height: usize, doc_width: usize) {
        self.cancel_search_advance();
        self.editor.ensure_search_fresh();
        if self.search_exit_if_empty() {
            return;
        }
        if let Some(s) = self.editor.search.as_mut() {
            if forward {
                s.advance_focus();
            } else {
                s.retreat_focus();
            }
        }
        self.editor.sync_cursor_to_search_focus();
        self.editor
            .scroll_focused_match_into_view(doc_height, doc_width);
        self.needs_draw = true;
    }

    /// Replace the focused match as one undo step, move focus to the next match, and arm the
    /// deferred advance so the replacement stays in view for a beat.
    fn search_replace_focused(&mut self) {
        if self.search_advance_pending_since.is_some() {
            self.apply_search_advance();
        }
        let Some(s) = self.editor.search.as_ref() else {
            return;
        };
        let (Some(range), Some(replacement)) = (s.focused_range(), s.replacement.clone()) else {
            return;
        };
        let rope = self.editor.buffer.rope();
        if range.end > rope.len_bytes() {
            // Stale range; should be unreachable since every mutation path refreshes.
            self.editor.ensure_search_fresh();
            return;
        }
        let char_start = rope.byte_to_char(range.start);
        let char_end = rope.byte_to_char(range.end);
        let removed = self.editor.buffer.slice_to_string(char_start, char_end);
        let replacement_len = replacement.len();
        self.editor.apply_delta(EditDelta {
            offset: char_start,
            removed,
            inserted: replacement,
        });
        // `apply_delta` defers the reparse for in-line edits; the match recompute needs fresh
        // source-map byte ranges on the next frame.
        self.editor.flush_parsed_if_dirty();
        self.editor.ensure_search_fresh();
        self.editor.update_cursor_block();
        // Focus the first match strictly past the inserted bytes: index reuse would land on a
        // match the replacement itself introduced (`a` → `aa`) and trap the flow on one site.
        if let Some(s) = self.editor.search.as_mut() {
            let next_byte = range.start + replacement_len;
            let idx = s.matches.partition_point(|m| m.start < next_byte);
            s.focused_idx = if idx >= s.matches.len() { 0 } else { idx };
        }
        if self.search_exit_if_empty() {
            return;
        }
        self.arm_search_advance();
        self.needs_draw = true;
    }

    /// Replace every match as a single undo step, then exit the flow with a count flash.
    fn search_replace_all(&mut self) {
        let Some(s) = self.editor.search.as_ref() else {
            return;
        };
        let (Some(replacement), false) = (s.replacement.clone(), s.matches.is_empty()) else {
            return;
        };
        let matches = s.matches.clone();
        let old = self.editor.buffer.contents();
        // Splice from the match list so the text swapped is exactly the highlighted set.
        let mut new = String::with_capacity(old.len());
        let mut cursor = 0usize;
        for r in &matches {
            new.push_str(&old[cursor..r.start]);
            new.push_str(&replacement);
            cursor = r.end;
        }
        new.push_str(&old[cursor..]);
        let count = matches.len();

        self.editor.buffer.set_rope(ropey::Rope::from_str(&new));
        self.editor.history.record(EditDelta {
            offset: 0,
            removed: old,
            inserted: new,
        });
        self.editor.dirty = true;
        let max = self.editor.buffer.len_chars();
        self.editor.cursor.offset = self.editor.cursor.offset.min(max);
        self.editor.refresh_parsed();
        self.editor.update_cursor_block();
        self.exit_search_flow();
        self.flash(format!("{count} replaced"), MessageKind::Success);
        self.needs_draw = true;
    }

    /// Exit the flow with a flash when no matches remain; returns whether it did.
    fn search_exit_if_empty(&mut self) -> bool {
        let empty = self
            .editor
            .search
            .as_ref()
            .is_some_and(|s| s.matches.is_empty());
        if empty {
            self.exit_search_flow();
            self.flash("No matches remain", MessageKind::Info);
            self.needs_draw = true;
        }
        empty
    }

    // ── Deferred post-replace advance ─────────────────────────────────

    pub(crate) fn arm_search_advance(&mut self) {
        self.search_advance_pending_since = Some(Instant::now());
    }

    /// Clear a pending advance without performing it.
    pub(crate) fn cancel_search_advance(&mut self) {
        self.search_advance_pending_since = None;
    }

    /// Perform the deferred advance now: `focused_idx` already names the next match, so this
    /// only syncs the cursor and requests a scroll-into-view.
    pub(crate) fn apply_search_advance(&mut self) {
        self.search_advance_pending_since = None;
        if self.editor.search.is_some() {
            self.editor.sync_cursor_to_search_focus();
            self.editor.pending_focus_scroll = true;
            self.needs_draw = true;
        }
    }

    pub(super) fn tick_search_advance(&mut self) {
        let Some(since) = self.search_advance_pending_since else {
            return;
        };
        if since.elapsed() < SEARCH_ADVANCE_DELAY {
            return;
        }
        self.apply_search_advance();
    }

    /// Contributed to [`App::next_deadline`].
    pub(super) fn search_advance_deadline(&self) -> Option<Instant> {
        self.search_advance_pending_since
            .map(|t| t + SEARCH_ADVANCE_DELAY)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::SEARCH_ADVANCE_DELAY;
    use crate::app::test_utils::app_with_buffer;
    use crate::config::Action;

    /// An app with `src` loaded and an active flow (a replace flow when `replace` is non-empty).
    fn app_in_search(src: &str, query: &str, replace: &str) -> crate::app::App {
        let mut app = app_with_buffer(src, 0);
        let replace = (!replace.is_empty()).then(|| replace.to_owned());
        app.enter_search_flow(query.to_owned(), replace);
        assert!(app.editor.search.is_some(), "flow must be active");
        app
    }

    fn dispatch(app: &mut crate::app::App, action: Action) {
        app.dispatch_action(action, 24, 80);
    }

    #[test]
    fn gate_blocks_buffer_edits_during_replace_flow() {
        let mut app = app_in_search("alpha beta alpha\n", "alpha", "OMEGA");
        let before = app.editor.buffer.contents();
        dispatch(&mut app, Action::InsertChar('x'));
        dispatch(&mut app, Action::DeleteCharBack);
        dispatch(&mut app, Action::Paste);
        assert_eq!(app.editor.buffer.contents(), before);
        assert!(app.editor.search.is_some(), "flow survives denied actions");
    }

    #[test]
    fn denied_action_flashes_not_available() {
        let mut app = app_in_search("alpha beta\n", "alpha", "OMEGA");
        dispatch(&mut app, Action::InsertChar('x'));
        let text = app.transient.as_ref().map(|t| t.text.clone());
        assert_eq!(text.as_deref(), Some("Not available during search"));
        assert!(app.editor.search.is_some(), "flow survives the denial");
    }

    #[test]
    fn navigate_flow_allows_editing_and_keeps_highlights() {
        // `ensure_search_fresh` below simulates the per-frame refresh in `prepare_viewport`.
        let mut app = app_in_search("foo foo foo\n", "foo", "");
        app.editor.mode = crate::editor::Mode::Rendered;
        assert_eq!(app.editor.search.as_ref().unwrap().matches.len(), 3);
        dispatch(&mut app, Action::InsertChar('x'));
        assert_eq!(app.editor.buffer.contents(), "xfoo foo foo\n");
        assert!(app.editor.search.is_some(), "editing does not end the flow");
        app.editor.ensure_search_fresh();
        assert_eq!(
            app.editor.search.as_ref().unwrap().matches.len(),
            3,
            "the three matches still track after the edit"
        );
    }

    #[test]
    fn replace_flow_allows_cursor_movement_and_copy() {
        let mut app = app_in_search("alpha beta alpha\n", "alpha", "OMEGA");
        let before = app.editor.cursor.offset;
        dispatch(&mut app, Action::MoveRight);
        assert_ne!(app.editor.cursor.offset, before, "cursor may move");
        dispatch(&mut app, Action::SelectRight);
        dispatch(&mut app, Action::Copy);
        assert_eq!(app.editor.buffer.contents(), "alpha beta alpha\n");
        assert!(app.editor.search.is_some());
    }

    #[test]
    fn replace_flow_entered_from_preview_switches_to_rendered() {
        let mut app = app_with_buffer("foo bar\n", 0);
        assert_eq!(app.editor.mode, crate::editor::Mode::Preview);
        app.enter_search_flow("foo".to_owned(), Some("baz".to_owned()));
        assert_eq!(
            app.editor.mode,
            crate::editor::Mode::Rendered,
            "a replace flow edits the buffer, so Preview must hand over to Rendered"
        );
    }

    #[test]
    fn navigate_only_flow_keeps_preview_mode() {
        let mut app = app_with_buffer("foo bar\n", 0);
        app.enter_search_flow("foo".to_owned(), None);
        assert_eq!(app.editor.mode, crate::editor::Mode::Preview);
    }

    #[test]
    fn zero_match_replace_query_leaves_preview_untouched() {
        let mut app = app_with_buffer("foo bar\n", 0);
        app.enter_search_flow("missing".to_owned(), Some("baz".to_owned()));
        assert!(app.editor.search.is_none());
        assert_eq!(
            app.editor.mode,
            crate::editor::Mode::Preview,
            "no flow entered → no mode transition"
        );
    }

    #[test]
    fn next_and_prev_wrap_and_move_the_cursor() {
        let mut app = app_in_search("aa bb aa bb aa\n", "aa", "");
        assert_eq!(app.editor.search.as_ref().unwrap().focused_idx, 0);
        dispatch(&mut app, Action::SearchNext);
        assert_eq!(app.editor.search.as_ref().unwrap().focused_idx, 1);
        assert_eq!(app.editor.cursor.offset, 6, "cursor follows the match");
        dispatch(&mut app, Action::SearchNext);
        dispatch(&mut app, Action::SearchNext);
        assert_eq!(
            app.editor.search.as_ref().unwrap().focused_idx,
            0,
            "next past the last match wraps to the first"
        );
        dispatch(&mut app, Action::SearchPrev);
        assert_eq!(
            app.editor.search.as_ref().unwrap().focused_idx,
            2,
            "prev before the first match wraps to the last"
        );
    }

    #[test]
    fn exit_stays_on_the_current_match_without_scroll_back() {
        let mut app = app_with_buffer(&"line\n".repeat(100), 0);
        app.editor.scroll = 42;
        app.enter_search_flow("line".to_owned(), None);
        app.editor.scroll = 7;
        dispatch(&mut app, Action::SearchExit);
        assert!(app.editor.search.is_none());
        assert_eq!(app.editor.scroll, 7, "no scroll-back to origin on exit");
    }

    #[test]
    fn replace_swaps_one_match_and_defers_the_advance() {
        let mut app = app_in_search("foo bar foo\n", "foo", "baz");
        dispatch(&mut app, Action::SearchReplace);
        assert_eq!(app.editor.buffer.contents(), "baz bar foo\n");
        assert!(app.editor.dirty);
        assert!(app.search_advance_pending_since.is_some());
        // Force the reveal window to elapse.
        app.search_advance_pending_since =
            Some(std::time::Instant::now() - SEARCH_ADVANCE_DELAY - Duration::from_millis(5));
        app.tick_search_advance();
        assert!(app.search_advance_pending_since.is_none());
        assert_eq!(app.editor.cursor.offset, 8, "cursor lands on next match");
        dispatch(&mut app, Action::Undo);
        assert_eq!(app.editor.buffer.contents(), "foo bar foo\n");
        assert_eq!(
            app.editor.search.as_ref().unwrap().matches.len(),
            2,
            "undo refreshes the match list"
        );
    }

    #[test]
    fn replace_is_inert_in_a_navigate_only_flow() {
        let mut app = app_in_search("foo bar foo\n", "foo", "");
        dispatch(&mut app, Action::SearchReplace);
        dispatch(&mut app, Action::SearchReplaceAll);
        assert_eq!(app.editor.buffer.contents(), "foo bar foo\n");
        assert!(app.editor.search.is_some());
    }

    #[test]
    fn replacing_the_last_match_exits_the_flow() {
        let mut app = app_in_search("only one foo here\n", "foo", "bar");
        dispatch(&mut app, Action::SearchReplace);
        assert_eq!(app.editor.buffer.contents(), "only one bar here\n");
        assert!(app.editor.search.is_none(), "no matches remain → exit");
    }

    #[test]
    fn replacement_containing_the_query_still_makes_progress() {
        let mut app = app_in_search("a b a\n", "a", "aa");
        dispatch(&mut app, Action::SearchReplace);
        assert_eq!(app.editor.buffer.contents(), "aa b a\n");
        let s = app.editor.search.as_ref().unwrap();
        let focused = s.focused_range().unwrap();
        assert_eq!(focused.start, 5, "focus skipped the inserted text");
    }

    #[test]
    fn replace_all_is_one_undo_step_and_preserves_prior_history() {
        let mut app = app_with_buffer("foo bar foo bar foo\n", 0);
        // A pre-flow edit gives the undo stack prior depth; in Preview the keystroke would only
        // perform the mode transition.
        app.editor.mode = crate::editor::Mode::Rendered;
        crate::editor::edit_ops::apply(&mut app.editor, Action::InsertChar('x'), 24, 80);
        assert_eq!(app.editor.buffer.contents(), "xfoo bar foo bar foo\n");
        app.enter_search_flow("foo".to_owned(), Some("qux".to_owned()));
        app.dispatch_action(Action::SearchReplaceAll, 24, 80);
        assert_eq!(app.editor.buffer.contents(), "xqux bar qux bar qux\n");
        assert!(app.editor.search.is_none(), "replace-all exits the flow");
        assert_eq!(app.editor.history.undo_depth(), 2);
        crate::editor::edit_ops::apply(&mut app.editor, Action::Undo, 24, 80);
        assert_eq!(app.editor.buffer.contents(), "xfoo bar foo bar foo\n");
        crate::editor::edit_ops::apply(&mut app.editor, Action::Undo, 24, 80);
        assert_eq!(app.editor.buffer.contents(), "foo bar foo bar foo\n");
    }

    #[test]
    fn replace_all_flashes_the_count() {
        let mut app = app_in_search("x y x y x\n", "x", "z");
        dispatch(&mut app, Action::SearchReplaceAll);
        let text = app.transient.as_ref().map(|t| t.text.clone());
        assert_eq!(text.as_deref(), Some("3 replaced"));
    }

    #[test]
    fn quit_with_in_flow_edits_opens_the_dirty_confirm() {
        let mut app = app_in_search("foo bar\n", "foo", "baz");
        dispatch(&mut app, Action::SearchReplace);
        // The single replace exited the flow; re-enter so Quit fires inside one.
        app.enter_search_flow("bar".to_owned(), None);
        dispatch(&mut app, Action::Quit);
        assert!(
            app.modal_stack
                .contains::<crate::app::modal::QuitConfirmModal>(),
            "dirty buffer must gate Quit behind the confirm modal"
        );
        assert!(!app.should_quit);
        assert!(app.editor.search.is_some(), "flow stays active behind it");
    }

    #[test]
    fn vim_search_forward_focuses_first_match_after_cursor() {
        let mut app = app_with_buffer("foo bar foo baz foo\n", 0); // matches 0,8,16
        app.set_vim_enabled(true);
        app.editor.cursor.offset = 5; // within "bar"
        app.enter_vim_search("foo".to_owned(), true);
        assert_eq!(app.editor.search.as_ref().unwrap().focused_idx, 1);
        assert_eq!(app.editor.cursor.offset, 8, "cursor on the focused match");
    }

    #[test]
    fn vim_search_backward_focuses_last_match_before_cursor() {
        let mut app = app_with_buffer("foo bar foo baz foo\n", 0);
        app.set_vim_enabled(true);
        app.editor.cursor.offset = 12; // within "baz"
        app.enter_vim_search("foo".to_owned(), false);
        assert_eq!(app.editor.search.as_ref().unwrap().focused_idx, 1);
    }

    #[test]
    fn vim_search_forward_wraps_when_no_match_follows_the_cursor() {
        let mut app = app_with_buffer("foo bar foo\n", 0); // matches 0,8
        app.set_vim_enabled(true);
        app.editor.cursor.offset = 9; // within the last match
        app.enter_vim_search("foo".to_owned(), true);
        assert_eq!(
            app.editor.search.as_ref().unwrap().focused_idx,
            0,
            "no match strictly after the cursor wraps to the first"
        );
    }

    #[test]
    fn vim_search_with_no_match_flashes_and_does_not_enter() {
        let mut app = app_with_buffer("foo bar\n", 0);
        app.set_vim_enabled(true);
        app.enter_vim_search("zzz".to_owned(), true);
        assert!(app.editor.search.is_none(), "zero matches → no flow");
        assert!(app.transient.is_some(), "a no-match flash is shown");
    }

    #[test]
    fn vim_search_matches_across_a_line_break() {
        let mut app = app_with_buffer("foo  \nbar  \nbaz", 0);
        app.set_vim_enabled(true);
        app.enter_vim_search(r"  \n".to_owned(), true);
        let s = app.editor.search.as_ref().expect("flow entered");
        assert_eq!(s.matches, vec![3..6, 9..12]);
        assert_eq!(s.needle, "  \n");
        assert_eq!(s.query, r"  \n", "the typed form is kept for display");
    }

    #[test]
    fn vim_search_with_a_bad_escape_flashes_and_does_not_enter() {
        // Search is literal, not regex.
        let mut app = app_with_buffer("a1b2\n", 0);
        app.set_vim_enabled(true);
        app.enter_vim_search(r"\d".to_owned(), true);
        assert!(app.editor.search.is_none(), "bad escape → no flow");
        let flash = app.transient.as_ref().expect("an error is flashed");
        assert!(flash.text.contains("Unsupported escape"), "{}", flash.text);
    }

    #[test]
    fn vim_search_finds_a_literal_backslash_when_it_is_escaped() {
        let mut app = app_with_buffer(r"a \ b", 0);
        app.set_vim_enabled(true);
        app.enter_vim_search(r"\\".to_owned(), true);
        let s = app.editor.search.as_ref().expect("flow entered");
        assert_eq!(s.matches, vec![2..3]);
    }

    #[test]
    fn hash_from_mid_word_jumps_to_the_previous_occurrence() {
        use crate::input::{vim_feed, VimOutcome};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        // Regression: cursor mid-word in the third "foo" (0, 8, 16) used to jump to 16, not 8.
        let mut app = app_with_buffer("foo bar foo baz foo\n", 0);
        app.set_vim_enabled(true);
        app.editor.cursor.offset = 17;
        app.editor.update_cursor_block();

        let mut vim = app.vim.take().unwrap();
        let out = vim_feed(
            &mut vim,
            &mut app.editor,
            KeyEvent::new(KeyCode::Char('#'), KeyModifiers::NONE),
            24,
            80,
        );
        app.vim = Some(vim);
        let VimOutcome::EnterSearch { forward, query } = out else {
            panic!("# should emit EnterSearch, got {out:?}");
        };
        assert!(!forward);
        assert_eq!(query, "foo");
        // `search_word_outcome` moved the cursor to the word start, which is what makes the
        // backward jump land on the previous occurrence.
        assert_eq!(app.editor.cursor.offset, 16);

        app.enter_vim_search(query, forward);
        assert_eq!(app.editor.search.as_ref().unwrap().focused_idx, 1);
        assert_eq!(app.editor.cursor.offset, 8, "cursor on the previous match");
    }

    #[test]
    fn entering_diff_mode_tears_down_an_active_flow() {
        let mut app = app_in_search("alpha\nbeta\n", "alpha", "");
        app.enter_diff_mode("alpha\nGAMMA\n".to_owned());
        assert!(app.editor.search.is_none());
        assert_eq!(app.editor.mode, crate::editor::Mode::Diff);
    }
}
