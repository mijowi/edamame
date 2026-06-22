//! App-level search-and-replace flow.
//!
//! Owns:
//! - [`App::open_search_modal`] / [`App::enter_search_flow`] /
//!   [`App::exit_search_flow`] — flow lifecycle.
//! - [`App::dispatch_search_action`] — the in-flow dispatcher reached
//!   from `dispatch_action` once `search_safe_action` has allowed the
//!   action (default-deny, mirroring diff mode).
//! - The replace / replace-all implementations.
//! - The deferred post-replace advance timer, mirroring
//!   `diff_advance`: a replace stays in view for a beat so the user
//!   sees the new text land before focus jumps to the next match.

use std::time::{Duration, Instant};

use crate::config::Action;
use crate::document::EditDelta;
use crate::search::SearchState;
use crate::ui::ModalKind;

use super::flash::MessageKind;
use super::App;

/// How long a fresh replacement stays focused before the flow
/// auto-advances to the next match.  Mirrors `DIFF_ADVANCE_DELAY`.
pub(super) const SEARCH_ADVANCE_DELAY: Duration = Duration::from_millis(350);

impl App {
    /// Open the search/replace modal.  When a flow is already active
    /// (Ctrl+F pressed mid-search), the flow is torn down first and the
    /// modal opens pre-filled with its terms so the user can refine
    /// them.
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

    /// Start a search flow for `query` (with `replace` enabling the
    /// replace keys when non-empty).  Called from the modal's confirm
    /// path.  Zero matches never enters the flow — the user gets a
    /// flash and stays where they were.
    pub(crate) fn enter_search_flow(&mut self, query: String, replace: Option<String>) {
        let Some(state) = SearchState::new(query.clone(), replace, self.editor.scroll) else {
            // The modal validates non-empty input, so this only fires
            // for a query the session can't represent (e.g. pasted
            // newline).
            self.notify("Search term cannot span lines", ModalKind::Warning);
            return;
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
        // A replace flow edits the buffer, and Preview is browse-only —
        // transition to Rendered on flow entry, exactly as the first
        // keystroke of a normal edit would.  Navigate-only flows stay
        // in Preview.  The cursor is synced to the first match below,
        // so no scroll-anchored cursor placement is needed here.
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
        // `pending_focus_scroll` was set by `enter_search`; the next
        // `prepare_viewport` scrolls the first match into view once the
        // viewport height is known.
        self.needs_draw = true;
    }

    /// Start a vim `/` / `?` / `*` / `#` search: a navigate-only flow whose
    /// initial focus is cursor-relative — the first match *after* the cursor
    /// for a forward search, *before* it for a backward one, wrapping around
    /// the document.  Unlike [`Self::enter_search_flow`] (the modal path,
    /// which always starts at the first match), this matches vim's semantics.
    /// Zero matches never enters the flow — the user gets a flash.
    pub(crate) fn enter_vim_search(&mut self, query: String, forward: bool) {
        let Some(state) = SearchState::new(query.clone(), None, self.editor.scroll) else {
            self.flash("Search term cannot span lines", MessageKind::Info);
            return;
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
        // Cursor-relative initial focus.  Match offsets are bytes, so compare
        // against the cursor's byte offset.
        let cursor_byte = self
            .editor
            .buffer
            .rope()
            .char_to_byte(self.editor.cursor.offset);
        if let Some(s) = self.editor.search.as_mut() {
            let idx = if forward {
                // First match starting strictly after the cursor; wrap to 0.
                let i = s.matches.partition_point(|m| m.start <= cursor_byte);
                if i >= s.matches.len() {
                    0
                } else {
                    i
                }
            } else {
                // Last match starting strictly before the cursor; wrap to last.
                let i = s.matches.partition_point(|m| m.start < cursor_byte);
                if i == 0 {
                    s.matches.len() - 1
                } else {
                    i - 1
                }
            };
            s.focused_idx = idx;
        }
        self.editor.sync_cursor_to_search_focus();
        // `enter_search` set `pending_focus_scroll`; the next `prepare_viewport`
        // scrolls the focused match into view once the height is known.
        self.needs_draw = true;
    }

    /// Whether the active search flow *captures* keyboard / mouse input — the
    /// gate that default-denies buffer edits and routes flow keys to
    /// `dispatch_search_action`.
    ///
    /// A flow captures unless it is a vim navigate-only search: vim owns its
    /// own keys (`n`/`N` move over `EditorState::search`, every other key is a
    /// normal vim command), so a vim `/` search must *not* trap input the way
    /// the `Ctrl-F` flow does.  A vim *replace* flow (`Ctrl-F` with a
    /// replacement) still captures — it needs the `Tab`/`r`/`a` flow keys.
    /// With no vim handler the rule reduces to "any active search captures",
    /// exactly the previous behavior.
    pub(crate) fn search_flow_captures(&self) -> bool {
        self.editor
            .search
            .as_ref()
            .is_some_and(|s| self.vim.is_none() || s.is_replace_flow())
    }

    /// Tear down the active flow (if any): cancel a pending advance and
    /// restore the pre-search scroll.  Safe to call unconditionally —
    /// buffer-replacing paths (diff entry, file reload) use this so a
    /// stale match list never survives a content swap.
    pub(crate) fn exit_search_flow(&mut self) {
        self.cancel_search_advance();
        if self.editor.search.is_some() {
            self.editor.exit_search();
            self.needs_draw = true;
        }
    }

    /// Dispatch a single action while a search flow is active.  The
    /// caller has already passed `action` through `search_safe_action`,
    /// so unsupported actions never arrive here.
    pub(super) fn dispatch_search_action(
        &mut self,
        action: Action,
        doc_height: usize,
        doc_width: usize,
    ) {
        // Free scrolling, mirroring diff mode: the viewport moves
        // without dragging the cursor (and therefore the focused
        // match) along.
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
                // Re-open the modal pre-filled; `open_search_modal`
                // tears the current flow down first.
                self.open_search_modal();
            }
            Action::Undo | Action::Redo => {
                // Undoing / redoing an in-flow replace mutates the
                // buffer, so the match list must be refreshed before
                // anything consults it again.
                crate::editor::edit_ops::apply(&mut self.editor, action, doc_height, doc_width);
                self.editor.ensure_search_fresh();
                if self.search_exit_if_empty() {
                    return;
                }
                self.editor.sync_cursor_to_search_focus();
                self.needs_draw = true;
            }
            Action::Quit => {
                // In-flow replaces may have dirtied the buffer; route
                // through the standard dirty-quit confirm so nothing is
                // silently lost.  The flow stays active behind the
                // modal — Esc on the modal returns to it.
                if self.editor.dirty {
                    self.open_quit_confirm();
                } else {
                    self.should_quit = true;
                }
            }
            // App-level openers and save paths reuse their standard
            // handlers; all are read-only with respect to the flow.
            Action::Save
            | Action::SaveCopy
            | Action::ShowCommandPalette
            | Action::ShowMarkdownCheatSheet
            | Action::ShowAbout
            | Action::OpenSettings
            | Action::OpenKeybinds
            | Action::SwitchTheme
            | Action::CreateCustomTheme
            | Action::OpenConfigFolder => {
                self.handle_app_action(&action, doc_height, doc_width);
            }
            // Everything else passed `search_safe_action` but needs no
            // arm here.
            _ => {}
        }
    }

    /// Advance (or retreat) the focused match, flushing any pending
    /// post-replace advance first so rapid keys walk matches instead of
    /// stacking timers.
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

    /// Replace the focused match with the replacement text as one undo
    /// step, then arm the deferred advance.  After the recompute, the
    /// match list's same index already names the *next* match, so the
    /// timer only has to sync the cursor and scroll once the reveal
    /// window elapses.
    fn search_replace_focused(&mut self) {
        if self.search_advance_pending_since.is_some() {
            self.apply_search_advance();
        }
        let Some(s) = self.editor.search.as_ref() else {
            return;
        };
        let (Some(range), Some(replacement)) = (s.focused_range(), s.replace.clone()) else {
            // Navigate-only flow — `r` is inert.
            return;
        };
        let rope = self.editor.buffer.rope();
        if range.end > rope.len_bytes() {
            // Stale range (should be unreachable — every mutation path
            // refreshes); refresh and bail rather than panic.
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
        // `apply_delta` defers the reparse for in-line edits (and a
        // single-line query means the edit is always in-line), but the
        // match recompute and the highlight overlays need fresh
        // source-map byte ranges on the very next frame.
        self.editor.flush_parsed_if_dirty();
        self.editor.ensure_search_fresh();
        self.editor.update_cursor_block();
        // Focus the first match strictly past the replacement.  Plain
        // index reuse would land on any match the replacement itself
        // introduced (replacing `a` with `aa`), trapping the flow on
        // one site; skipping past the inserted bytes guarantees
        // forward progress.  Wraps to the first match at end-of-doc.
        if let Some(s) = self.editor.search.as_mut() {
            let next_byte = range.start + replacement_len;
            let idx = s.matches.partition_point(|m| m.start < next_byte);
            s.focused_idx = if idx >= s.matches.len() { 0 } else { idx };
        }
        if self.search_exit_if_empty() {
            return;
        }
        // Leave the cursor / viewport on the replacement site for the
        // reveal window; the timer advances focus afterwards.
        self.arm_search_advance();
        self.needs_draw = true;
    }

    /// Replace every match in one shot — a single coarse undo step that
    /// preserves prior history — then exit the flow with a count flash.
    fn search_replace_all(&mut self) {
        let Some(s) = self.editor.search.as_ref() else {
            return;
        };
        let (Some(replacement), false) = (s.replace.clone(), s.matches.is_empty()) else {
            return;
        };
        let matches = s.matches.clone();
        let old = self.editor.buffer.contents();
        // Splice from the match list (rather than `str::replace`) so
        // the text swapped is exactly the highlighted set.
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

    /// Exit the flow with an explanatory flash when no matches remain
    /// (last match replaced, or an undo/redo removed them all).
    /// Returns true when the flow was exited.
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

    /// Arm (or re-arm) the post-replace advance timer.
    pub(crate) fn arm_search_advance(&mut self) {
        self.search_advance_pending_since = Some(Instant::now());
    }

    /// Clear any pending advance without performing it (manual
    /// navigation or exit supersedes the timer).
    pub(crate) fn cancel_search_advance(&mut self) {
        self.search_advance_pending_since = None;
    }

    /// Perform the deferred advance now: sync the cursor to the focused
    /// match (the post-replace recompute already left `focused_idx` on
    /// the next match) and request a scroll-into-view.
    pub(crate) fn apply_search_advance(&mut self) {
        self.search_advance_pending_since = None;
        if self.editor.search.is_some() {
            self.editor.sync_cursor_to_search_focus();
            self.editor.pending_focus_scroll = true;
            self.needs_draw = true;
        }
    }

    /// Per-iteration step: once the reveal window has elapsed, advance.
    pub(super) fn tick_search_advance(&mut self) {
        let Some(since) = self.search_advance_pending_since else {
            return;
        };
        if since.elapsed() < SEARCH_ADVANCE_DELAY {
            return;
        }
        self.apply_search_advance();
    }

    /// Earliest instant the run loop must wake to fire a pending
    /// advance.  Contributed to [`App::next_deadline`].
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

    /// Build an app with `src` loaded and an active flow for `query`
    /// (replace flow when `replace` is non-empty).
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
    fn gate_blocks_buffer_edits_during_flow() {
        let mut app = app_in_search("alpha beta alpha\n", "alpha", "");
        let before = app.editor.buffer.contents();
        dispatch(&mut app, Action::InsertChar('x'));
        dispatch(&mut app, Action::DeleteCharBack);
        dispatch(&mut app, Action::Paste);
        assert_eq!(app.editor.buffer.contents(), before);
        assert!(app.editor.search.is_some(), "flow survives denied actions");
    }

    #[test]
    fn denied_action_flashes_not_available() {
        let mut app = app_in_search("alpha beta\n", "alpha", "");
        dispatch(&mut app, Action::InsertChar('x'));
        let text = app.transient.as_ref().map(|t| t.text.clone());
        assert_eq!(text.as_deref(), Some("Not available during search"));
        assert!(app.editor.search.is_some(), "flow survives the denial");
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
    fn exit_restores_pre_search_scroll() {
        let mut app = app_with_buffer(&"line\n".repeat(100), 0);
        app.editor.scroll = 42;
        app.enter_search_flow("line".to_owned(), None);
        app.editor.scroll = 7;
        dispatch(&mut app, Action::SearchExit);
        assert!(app.editor.search.is_none());
        assert_eq!(app.editor.scroll, 42);
    }

    #[test]
    fn replace_swaps_one_match_and_defers_the_advance() {
        let mut app = app_in_search("foo bar foo\n", "foo", "baz");
        dispatch(&mut app, Action::SearchReplace);
        assert_eq!(app.editor.buffer.contents(), "baz bar foo\n");
        assert!(app.editor.dirty);
        // The advance is pending, not applied: the cursor stays at the
        // replacement site for the reveal window.
        assert!(app.search_advance_pending_since.is_some());
        // Force the window open; the tick syncs the cursor to the next
        // match.
        app.search_advance_pending_since =
            Some(std::time::Instant::now() - SEARCH_ADVANCE_DELAY - Duration::from_millis(5));
        app.tick_search_advance();
        assert!(app.search_advance_pending_since.is_none());
        assert_eq!(app.editor.cursor.offset, 8, "cursor lands on next match");
        // One undo step reverts the single replace.
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
        // Replacing `a` with `aa` introduces fresh matches inside every
        // replacement; focus must skip past them or `r` would grind on
        // one site forever.
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
        // A real pre-flow edit so the undo stack has prior depth.
        // Force Rendered mode first — in Preview the keystroke only
        // performs the Preview→Rendered transition.
        app.editor.mode = crate::editor::Mode::Rendered;
        crate::editor::edit_ops::apply(&mut app.editor, Action::InsertChar('x'), 24, 80);
        assert_eq!(app.editor.buffer.contents(), "xfoo bar foo bar foo\n");
        app.enter_search_flow("foo".to_owned(), Some("qux".to_owned()));
        app.dispatch_action(Action::SearchReplaceAll, 24, 80);
        assert_eq!(app.editor.buffer.contents(), "xqux bar qux bar qux\n");
        assert!(app.editor.search.is_none(), "replace-all exits the flow");
        assert_eq!(app.editor.history.undo_depth(), 2);
        // One undo reverts the whole replace-all…
        crate::editor::edit_ops::apply(&mut app.editor, Action::Undo, 24, 80);
        assert_eq!(app.editor.buffer.contents(), "xfoo bar foo bar foo\n");
        // …and the next reverts the pre-flow edit.
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
        // The single match was replaced, so the flow has exited and the
        // buffer is dirty — but exercise Quit while still in a flow:
        // re-enter against the remaining text.
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
    fn hash_from_mid_word_jumps_to_the_previous_occurrence() {
        use crate::input::{vim_feed, VimOutcome};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        // "foo" at 0, 8, 16; cursor in the *middle* of the third occurrence.
        let mut app = app_with_buffer("foo bar foo baz foo\n", 0);
        app.set_vim_enabled(true);
        app.editor.cursor.offset = 17; // 'o' inside the third "foo" (start 16)
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
        // search_word_outcome repositioned the cursor to the word start (16),
        // not left at 17 — that is what makes the backward jump correct.
        assert_eq!(app.editor.cursor.offset, 16);

        app.enter_vim_search(query, forward);
        // Backward from word-start 16 → previous occurrence at 8 (idx 1),
        // NOT the current word at 16 (the bug being fixed).
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
