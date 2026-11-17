//! Event-loop arm for [`crate::app::AppEvent::Watcher`].
//!
//! The watcher worker has already read the disk; this module decides what to do
//! with the bytes, in order:
//!
//! 1. **Own-write filter.**  An incoming hash matching [`App::last_disk_hash`]
//!    is our own save echo or a no-op external write.  Dropped.
//! 2. **Already-reviewing reconcile.**  With a diff review open, fold the new
//!    disk state into it in place, preserving decisions on untouched hunks.
//!    This precedes step 3 because in diff mode the buffer is the pre-diff
//!    original, so "disk == buffer" there means "all changes reverted".
//! 3. **No-diff short-circuit.**  Disk bytes equal to the buffer's produce no
//!    diff; stamp `last_disk_hash` and return.
//! 4. **Stamp & dispatch.**  Stamp so later echoes are filtered, then enter diff
//!    review (clean buffer) or open [`super::modal::DirtyConflictModal`] (dirty
//!    buffer, or refresh the bytes an open conflict stack carries).
//!
//! The buffer is **never** silently reloaded on an external change: the user
//! sees every change before it replaces what they are looking at.  Only genuine
//! no-ops (filters 1 and 3) bypass review.
//!
//! Read errors (non-UTF-8, deleted between event and read, permission denied)
//! get a dismissable warning modal — otherwise the user has no signal that
//! external-edit prompts have stopped firing for this file.

use std::path::PathBuf;

use crate::diff::ReconcileOutcome;
use crate::ui::ModalKind;
use crate::watcher::{WatchedChange, WatchedEvent};

use super::flash::MessageKind;
use super::modal::dirty_conflict_discard_confirm::DirtyConflictDiscardConfirmModal;
use super::modal::dirty_conflict_save_copy::DirtyConflictSaveCopyModal;
use super::modal::{DirtyConflictModal, FileDeletedModal, SaveAsModal};
use super::App;

impl App {
    /// Top-level dispatch for one [`WatchedEvent`].
    pub(crate) fn handle_watcher_event(&mut self, event: WatchedEvent) {
        match event {
            WatchedEvent::Change(change) => self.handle_file_changed(change),
            WatchedEvent::Removed { path } => self.handle_file_removed(path),
            WatchedEvent::ReadError { path, error } => {
                // The worker may have a read queued from before a file switch.
                if self.file_path.as_deref() != Some(path.as_path()) {
                    return;
                }
                // `notify` dedups identical messages, so a file stuck in a
                // bad state doesn't stack modals as the watcher retries.
                self.notify(
                    format!("Could not read {}: {}", path.display(), error),
                    ModalKind::Warning,
                );
            }
        }
    }

    /// Handle the watched file disappearing.  A deletion never enters diff
    /// review — there is nothing to diff against — so any open diff is collapsed
    /// first and a [`FileDeletedModal`] offers to re-save.  It appears regardless
    /// of the dirty flag: even an unmodified buffer is now the sole copy.
    pub(crate) fn handle_file_removed(&mut self, path: PathBuf) {
        // A stale in-flight read from before a file switch.
        if self.file_path.as_deref() != Some(path.as_path()) {
            return;
        }
        // Idempotent: a second deletion signal must stack no duplicate — not the
        // prompt, and not its `[Save as…]` child, which closes the prompt before
        // opening.  Only a *deletion-recovery* save-as counts: a voluntary one on
        // a live file must not swallow a genuine deletion.
        if self.modal_stack.contains::<FileDeletedModal>()
            || self
                .modal_stack
                .find_first::<SaveAsModal>()
                .is_some_and(|m| m.is_deletion_recovery())
        {
            return;
        }
        // The review compares against a file that no longer exists.
        if self.editor.diff.is_some() {
            self.exit_diff_mode_discarding();
        }
        self.modal_stack.push(Box::new(FileDeletedModal::new(path)));
        self.needs_draw = true;
    }

    /// Dispatch a successful read from the watcher; see the module docs for the
    /// decision tree.
    pub(crate) fn handle_file_changed(&mut self, mut change: WatchedChange) {
        // A debounce window still in flight on a file the user just switched
        // away from.
        if self.file_path.as_deref() != Some(change.path.as_path()) {
            return;
        }

        // Whether the file on disk mixes line endings is only visible in the raw
        // bytes, and it decides whether the next save has to ask first (#101).
        // Recorded for every read, own-write echoes included: a save's own echo
        // is uniform, so it clears nothing a choice had not already settled.
        self.editor.buffer.observe_disk_endings(&change.contents);

        // Normalize before anything hashes, diffs or stores the bytes: the
        // buffer is `\n`-only, so a raw CRLF read would make every comparison
        // below see a spurious whole-file change, and the own-write filter — which
        // stamps from the `\n` rope — would never match our own save.
        change.contents = crate::document::buffer::normalize_newlines(change.contents);

        // Mid-`[Save as…]` after a deletion, the user has already committed to
        // writing the buffer out, so an external recreate must not yank the
        // prompt away or enter diff review behind it.  Completing the save-as
        // re-points the buffer and reads back as an own-write.
        if self
            .modal_stack
            .find_first::<SaveAsModal>()
            .is_some_and(|m| m.is_deletion_recovery())
        {
            return;
        }

        // The file is back, so a `FileDeletedModal`'s "only copy" premise no
        // longer holds — tear it down before reviewing, or a delete-then-recreate
        // enters diff mode behind it.
        self.modal_stack.remove_first::<FileDeletedModal>();

        let incoming_hash = seahash::hash(change.contents.as_bytes());

        // 1. Own-write filter.
        if self.last_disk_hash == Some(incoming_hash) {
            return;
        }

        // 2. Already reviewing: fold the disk state in, preserving decisions on
        //    untouched hunks.  Must precede the buffer-vs-disk filter — in diff
        //    mode `editor.buffer` is the pre-diff original, so "disk == buffer"
        //    means "all changes reverted", not "no-op".
        if self.editor.diff.is_some() {
            self.last_disk_hash = Some(incoming_hash); // stamp-before-dispatch
            self.reconcile_diff_with_disk(change.contents);
            return;
        }

        // 3. Buffer-vs-disk short-circuit: no diff would be produced, and the
        //    conflict modal must not open for byte-identical state.
        let buffer_text = self.editor.buffer.contents();
        let buffer_hash = seahash::hash(buffer_text.as_bytes());
        if incoming_hash == buffer_hash {
            self.last_disk_hash = Some(incoming_hash);
            return;
        }

        // 4a. Stamp before dispatch, so echoes overlapping modal-open time are
        //     filtered out.
        self.last_disk_hash = Some(incoming_hash);

        // 4b. Dispatch.
        if self.editor.dirty {
            // Mid-flow on a prior conflict: refresh the bytes the child modal
            // carries so confirming reloads the *current* disk state, and the
            // parent underneath so cancelling returns to one still in sync.
            let has_save_copy = self.modal_stack.contains::<DirtyConflictSaveCopyModal>();
            let has_discard_confirm = self
                .modal_stack
                .contains::<DirtyConflictDiscardConfirmModal>();
            if has_save_copy || has_discard_confirm {
                if let Some(parent) = self.modal_stack.find_first_mut::<DirtyConflictModal>() {
                    parent.set_on_disk_contents(change.contents.clone());
                }
                if has_save_copy {
                    if let Some(child) = self
                        .modal_stack
                        .find_first_mut::<DirtyConflictSaveCopyModal>()
                    {
                        child.set_on_disk_contents(change.contents);
                    }
                } else if let Some(child) = self
                    .modal_stack
                    .find_first_mut::<DirtyConflictDiscardConfirmModal>()
                {
                    child.set_on_disk_contents(change.contents);
                }
                return;
            }
            // Replaced rather than left standing, so the user reconciles against
            // the freshest disk contents.
            self.modal_stack.remove_first::<DirtyConflictModal>();
            self.modal_stack
                .push(Box::new(DirtyConflictModal::new(change.contents)));
        } else if self.config.editor.diff_on_change {
            // No unsaved work to reconcile, but the change is still reviewed
            // hunk by hunk rather than silently overwriting the buffer.
            self.enter_diff_mode(change.contents);
        } else {
            // Diff-on-change off and nothing unsaved to lose.  The dirty branch
            // above still prompts, so edits are never discarded unconfirmed.
            self.reload_buffer_from_disk(change.contents);
        }
    }

    /// Fold a mid-review external write into the open diff in place rather than
    /// resetting the review: untouched hunks keep their decisions, hunks whose
    /// new-side target changed reset to `Pending`, vanished ones are dropped, and
    /// a write reverting everything collapses the review.
    ///
    /// Never calls `enter_diff_mode`, so no `DiffIntroModal` is pushed.  The
    /// caller stamps the own-write hash before dispatch.
    fn reconcile_diff_with_disk(&mut self, new_disk: String) {
        let outcome = self
            .editor
            .diff
            .as_mut()
            .expect("guarded by diff.is_some()")
            .reconcile_with_disk(&new_disk);
        match outcome {
            ReconcileOutcome::StillReviewing { reset } => {
                // `reconcile_with_disk` dropped the new-side parse it
                // invalidated, and the editor's own buffer didn't change — so
                // nothing goes through `refresh_parsed` and its diff-parse tail
                // call.  Without this the review paints raw until some unrelated
                // re-render comes along.
                self.editor.refresh_diff_parse();
                // Re-center next frame, when the viewport height is known.
                self.editor.pending_focus_scroll = true;
                self.flash(
                    if reset > 0 {
                        "File changed on disk — updated hunks reset for review"
                    } else {
                        "File changed on disk — review updated"
                    },
                    MessageKind::Info,
                );
            }
            ReconcileOutcome::NoChangesRemain => {
                // Restores pre_diff_scroll and clears `diff`.
                self.editor.exit_diff_mode();
                self.flash(
                    "On-disk changes reverted — nothing to review",
                    MessageKind::Info,
                );
            }
        }
        self.needs_draw = true;
    }

    /// Replace the buffer contents with the bytes from a file-change event.
    /// Reached only from user-confirmed `DirtyConflictModal` choices — never
    /// silently; a clean-buffer external change enters diff review instead.
    ///
    /// Reuses the worker's bytes rather than re-reading, which would race the
    /// next watcher event.  Carries the previous `version` forward through
    /// [`crate::document::Buffer::reload`] so the monotonic-version invariant
    /// (autosave edit detector, visual-row cache invalidation) survives the swap.
    /// Cursor and scroll are preserved best-effort;
    /// [`crate::editor::EditorState::replace_buffer`] clamps them.
    pub(crate) fn reload_buffer_from_disk(&mut self, contents: String) {
        if self.editor.buffer.path().is_none() {
            // Unreachable: the watcher only fires when a path was set.
            return;
        }
        // `contents` is already normalized to `\n`, so `reload` carries the
        // ending and the mixed-endings state forward rather than re-detecting.
        let new_buffer = self.editor.buffer.reload(&contents);
        // At the App level, so the deferred-advance timer goes with the session;
        // `replace_buffer` drops the session but leaves the timer armed.
        self.exit_search_flow();
        self.editor.replace_buffer(new_buffer);
        // New contents may reference different images, or be the first to carry
        // one at all — under the default `Ask` policy the prompt is what enables
        // rendering.  Same call `load_file_into_editor` makes.
        self.on_document_contents_swapped();
        self.needs_draw = true;
        self.flash("Reloaded from disk", super::flash::MessageKind::Info);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::app::modal::DirtyConflictModal;
    use crate::app::test_utils::make_app;
    use crate::document::Buffer;
    use crate::watcher::WatchedChange;

    /// An `App` holding `initial` at a temp path, returned alongside the file
    /// handle.  The watcher hash filter is seeded from `initial`, so later
    /// events compare against a real hash rather than `None`.
    fn app_with_temp_file(initial: &str) -> (crate::app::App, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(tmp.path(), initial).expect("seed");
        let mut app = make_app();
        app.editor.buffer = Buffer::for_new_file(tmp.path());
        if !initial.is_empty() {
            app.editor.buffer.insert(0, initial);
        }
        app.editor.refresh_parsed();
        app.file_path = Some(tmp.path().to_path_buf());
        app.set_disk_hash(initial.as_bytes());
        (app, tmp)
    }

    fn file_changed_event(path: PathBuf, contents: &str) -> WatchedChange {
        WatchedChange {
            path,
            contents: contents.to_owned(),
        }
    }

    #[test]
    fn a_file_turning_mixed_on_disk_needs_a_choice_through_a_reload() {
        let (mut app, tmp) = app_with_temp_file("alpha\n");
        app.config.editor.diff_on_change = false;
        assert!(app.editor.buffer.needs_line_ending_choice().is_none());
        app.handle_file_changed(file_changed_event(
            tmp.path().to_path_buf(),
            "alpha\r\nbeta\n",
        ));
        assert_eq!(app.editor.buffer.contents(), "alpha\nbeta\n", "reloaded");
        assert_eq!(
            app.editor.buffer.needs_line_ending_choice(),
            Some(crate::document::MixedEndings { crlf: 1, lf: 1 })
        );
        // Turning uniform again on disk clears it.
        app.handle_file_changed(file_changed_event(
            tmp.path().to_path_buf(),
            "alpha\nbeta\ngamma\n",
        ));
        assert!(app.editor.buffer.needs_line_ending_choice().is_none());
    }

    #[test]
    fn own_write_echo_is_dropped() {
        let (mut app, tmp) = app_with_temp_file("alpha");
        // The byte-identical inotify echo of our own save.
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "alpha"));
        assert!(
            !app.modal_stack.contains::<DirtyConflictModal>(),
            "own-write echo must not open the dirty-conflict modal",
        );
        assert!(
            app.transient.is_none(),
            "own-write echo must not produce a flash",
        );
    }

    #[test]
    fn external_change_with_clean_buffer_enters_diff() {
        let (mut app, tmp) = app_with_temp_file("alpha");
        assert!(!app.editor.dirty);
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "beta"));
        assert!(
            app.editor.diff.is_some(),
            "clean buffer + external change must enter diff review",
        );
        assert_eq!(app.editor.mode, crate::editor::Mode::Diff);
        assert!(
            !app.modal_stack.contains::<DirtyConflictModal>(),
            "clean entry must not open the dirty-conflict modal",
        );
        assert_eq!(
            app.editor.buffer.contents(),
            "alpha",
            "buffer must not be silently overwritten",
        );
        assert_eq!(app.last_disk_hash, Some(seahash::hash(b"beta")));
    }

    #[test]
    fn clean_buffer_with_diff_disabled_reloads_silently() {
        let (mut app, tmp) = app_with_temp_file("alpha");
        app.config.editor.diff_on_change = false;
        assert!(!app.editor.dirty);
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "beta"));
        assert!(
            app.editor.diff.is_none(),
            "diff-on-change off must not enter diff review",
        );
        assert_ne!(app.editor.mode, crate::editor::Mode::Diff);
        assert_eq!(
            app.editor.buffer.contents(),
            "beta",
            "clean buffer must be reloaded with the disk contents",
        );
        assert_eq!(app.last_disk_hash, Some(seahash::hash(b"beta")));
    }

    #[test]
    fn dirty_buffer_with_diff_disabled_still_prompts() {
        // Diff-on-change off only affects the clean path.
        let (mut app, tmp) = app_with_temp_file("alpha");
        app.config.editor.diff_on_change = false;
        let len = app.editor.buffer.len_chars();
        app.editor.buffer.insert_char(len, '!');
        app.editor.dirty = true;
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "external"));
        assert!(
            app.modal_stack.contains::<DirtyConflictModal>(),
            "a dirty buffer must still prompt when diff-on-change is off",
        );
        assert!(app.editor.buffer.contents().ends_with('!'));
    }

    #[test]
    fn clean_buffer_byte_identical_disk_does_not_enter_diff() {
        // "Clean" is not the no-op condition — "disk == buffer" is.
        let (mut app, tmp) = app_with_temp_file("alpha");
        assert!(!app.editor.dirty);
        // Forced to differ so filter 1 doesn't short-circuit; filter 3 is under
        // test.
        app.last_disk_hash = Some(seahash::hash(b"stale"));
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "alpha"));
        assert!(
            app.editor.diff.is_none(),
            "byte-identical disk must not enter diff even with a clean buffer",
        );
        assert!(!app.modal_stack.contains::<DirtyConflictModal>());
        assert_eq!(app.last_disk_hash, Some(seahash::hash(b"alpha")));
    }

    #[test]
    fn external_change_with_dirty_buffer_opens_modal() {
        let (mut app, tmp) = app_with_temp_file("alpha");
        let len = app.editor.buffer.len_chars();
        app.editor.buffer.insert_char(len, '!');
        app.editor.dirty = true;
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "external"));
        assert!(
            app.modal_stack.contains::<DirtyConflictModal>(),
            "dirty buffer + external change must open the conflict modal",
        );
        assert!(app.editor.buffer.contents().ends_with('!'));
    }

    #[test]
    fn disk_equal_to_buffer_skips_modal_and_stamps_hash() {
        // An external rewrite whose bytes match the buffer's can still differ
        // from `last_disk_hash`, so filter 1 misses and filter 3 must catch it.
        let (mut app, tmp) = app_with_temp_file("alpha");
        let len = app.editor.buffer.len_chars();
        app.editor.buffer.insert_char(len, '!');
        app.editor.dirty = true;
        app.last_disk_hash = Some(seahash::hash(b"alpha"));
        let buffer_text = app.editor.buffer.contents();
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), &buffer_text));
        assert!(
            !app.modal_stack.contains::<DirtyConflictModal>(),
            "byte-identical change must skip the modal",
        );
        assert_eq!(
            app.last_disk_hash,
            Some(seahash::hash(buffer_text.as_bytes()))
        );
    }

    #[test]
    fn external_change_in_diff_preserves_decisions() {
        use crate::diff::Decision;
        // Two changeable regions, so the first write enters diff with two hunks.
        let (mut app, tmp) = app_with_temp_file("a\nb\nc\nd\ne\n");
        app.handle_file_changed(file_changed_event(
            tmp.path().to_path_buf(),
            "a\nB\nc\nD\ne\n",
        ));
        assert!(app.editor.diff.is_some(), "first change enters diff");
        let diff = app.editor.diff.as_ref().unwrap();
        assert_eq!(diff.hunks.len(), 2);
        let h0_id = diff.hunks[0].id;
        app.editor.diff.as_mut().unwrap().decisions[0] = Decision::Accepted;
        app.transient = None;

        // The second write touches only the second region.
        app.handle_file_changed(file_changed_event(
            tmp.path().to_path_buf(),
            "a\nB\nc\nDD\ne\n",
        ));

        assert!(app.editor.diff.is_some());
        assert_eq!(app.editor.mode, crate::editor::Mode::Diff);
        let diff = app.editor.diff.as_ref().unwrap();
        let h0 = diff.hunks.iter().position(|h| h.id == h0_id).expect("h0");
        assert_eq!(diff.decisions[h0], Decision::Accepted);
        assert!(app.transient.is_some(), "reconcile records a flash");
    }

    #[test]
    fn external_revert_in_diff_exits_diff() {
        let (mut app, tmp) = app_with_temp_file("a\nb\n");
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "a\nB\n"));
        assert!(app.editor.diff.is_some(), "first change enters diff");

        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "a\nb\n"));

        assert!(app.editor.diff.is_none(), "revert exits diff mode");
        assert_ne!(app.editor.mode, crate::editor::Mode::Diff);
        assert_eq!(app.editor.buffer.contents(), "a\nb\n");
    }

    #[test]
    fn deletion_opens_file_deleted_modal_for_clean_buffer() {
        use crate::app::modal::FileDeletedModal;
        // Even an unmodified buffer prompts: it is now the only copy.
        let (mut app, tmp) = app_with_temp_file("alpha");
        assert!(!app.editor.dirty);
        app.handle_file_removed(tmp.path().to_path_buf());
        assert!(
            app.modal_stack.contains::<FileDeletedModal>(),
            "deletion must surface the file-deleted modal",
        );
        assert!(app.editor.diff.is_none());
        assert_ne!(app.editor.mode, crate::editor::Mode::Diff);
    }

    #[test]
    fn deletion_is_idempotent() {
        use crate::app::modal::FileDeletedModal;
        let (mut app, tmp) = app_with_temp_file("alpha");
        let base = app.modal_stack.len();
        app.handle_file_removed(tmp.path().to_path_buf());
        app.handle_file_removed(tmp.path().to_path_buf());
        assert_eq!(
            app.modal_stack.len() - base,
            1,
            "a repeated deletion signal must not stack duplicate modals",
        );
        assert!(app.modal_stack.contains::<FileDeletedModal>());
    }

    #[test]
    fn deletion_for_other_path_is_ignored() {
        use crate::app::modal::FileDeletedModal;
        let (mut app, _tmp) = app_with_temp_file("alpha");
        app.handle_file_removed(PathBuf::from("/nonexistent/other.md"));
        assert!(!app.modal_stack.contains::<FileDeletedModal>());
    }

    #[test]
    fn deletion_during_diff_exits_diff_and_prompts() {
        use crate::app::modal::FileDeletedModal;
        let (mut app, tmp) = app_with_temp_file("a\nb\n");
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "a\nB\n"));
        assert!(app.editor.diff.is_some(), "first change enters diff");

        app.handle_file_removed(tmp.path().to_path_buf());
        assert!(app.editor.diff.is_none(), "deletion must exit diff mode");
        assert_ne!(app.editor.mode, crate::editor::Mode::Diff);
        assert!(app.modal_stack.contains::<FileDeletedModal>());
    }

    #[test]
    fn deletion_during_diff_clears_orphaned_diff_modals() {
        use crate::app::modal::{DiffQuitConfirmModal, FileDeletedModal};
        // Exiting the diff must tear down the now-meaningless quit-confirm, so
        // it can't fire from under the file-deleted modal.
        let (mut app, tmp) = app_with_temp_file("a\nb\n");
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "a\nB\n"));
        assert!(app.editor.diff.is_some());
        app.modal_stack.push(Box::new(DiffQuitConfirmModal::new()));

        app.handle_file_removed(tmp.path().to_path_buf());

        assert!(
            !app.modal_stack.contains::<DiffQuitConfirmModal>(),
            "the orphaned diff-quit confirmation must be removed",
        );
        assert!(app.modal_stack.contains::<FileDeletedModal>());
    }

    #[test]
    fn deletion_is_idempotent_across_save_as_flow() {
        use crate::app::modal::{FileDeletedModal, SaveAsModal};
        // A second signal arriving during the `[Save as…]` child — with the
        // prompt itself already closed — must not stack a fresh prompt.
        let (mut app, tmp) = app_with_temp_file("alpha");
        app.handle_file_removed(tmp.path().to_path_buf());
        // The user picks `[Save as…]`: the prompt closes, path entry opens.
        app.modal_stack.remove_first::<FileDeletedModal>();
        app.modal_stack
            .push(Box::new(SaveAsModal::for_deleted_file("x".into())));
        let base = app.modal_stack.len();

        app.handle_file_removed(tmp.path().to_path_buf());

        assert_eq!(
            app.modal_stack.len(),
            base,
            "must not stack a duplicate prompt"
        );
        assert!(!app.modal_stack.contains::<FileDeletedModal>());
        assert!(app.modal_stack.contains::<SaveAsModal>());
    }

    #[test]
    fn recreate_while_deleted_modal_open_dismisses_it_and_reviews() {
        use crate::app::modal::FileDeletedModal;
        // A recreate voids the prompt's "only copy" premise, so it is torn down
        // and the change reviews normally rather than behind the modal.
        let (mut app, tmp) = app_with_temp_file("a\nb\n");
        app.handle_file_removed(tmp.path().to_path_buf());
        assert!(app.modal_stack.contains::<FileDeletedModal>());

        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "a\nB\n"));

        assert!(
            !app.modal_stack.contains::<FileDeletedModal>(),
            "the file-deleted prompt must be dismissed once the file returns",
        );
        assert!(app.editor.diff.is_some(), "the recreate enters diff review");
    }

    #[test]
    fn recreate_during_save_as_does_not_enter_diff_behind_it() {
        use crate::app::modal::{FileDeletedModal, SaveAsModal};
        // The user has committed to saving their buffer out, so a recreate must
        // be skipped rather than entering diff review behind the path entry.
        let (mut app, tmp) = app_with_temp_file("a\nb\n");
        app.handle_file_removed(tmp.path().to_path_buf());
        app.modal_stack.remove_first::<FileDeletedModal>();
        app.modal_stack
            .push(Box::new(SaveAsModal::for_deleted_file("x".into())));

        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "a\nB\n"));

        assert!(
            app.modal_stack.contains::<SaveAsModal>(),
            "the save-as path entry must stay open",
        );
        assert!(
            app.editor.diff.is_none(),
            "an external recreate must not enter diff mode behind save-as",
        );
        assert_ne!(app.editor.mode, crate::editor::Mode::Diff);
    }

    #[test]
    fn voluntary_save_as_does_not_suppress_external_change() {
        use crate::app::modal::{DirtyConflictModal, SaveAsModal};
        // Locks in the `is_deletion_recovery()` narrowing of the watcher dedup: a
        // voluntary Save As is just a prompt over a live file and must not
        // swallow a genuine external change.
        let (mut app, tmp) = app_with_temp_file("alpha");
        let len = app.editor.buffer.len_chars();
        app.editor.buffer.insert_char(len, '!');
        app.editor.dirty = true;
        app.modal_stack.push(Box::new(SaveAsModal::for_buffer_path(
            Some(tmp.path()),
            None,
        )));
        assert!(
            !app.modal_stack
                .find_first::<SaveAsModal>()
                .unwrap()
                .is_deletion_recovery(),
            "this must be the voluntary (non-recovery) variant",
        );

        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "external"));

        assert!(
            app.modal_stack.contains::<DirtyConflictModal>(),
            "a voluntary save-as must not suppress the dirty-conflict modal",
        );
    }

    #[test]
    fn change_for_other_path_is_ignored() {
        let (mut app, _tmp) = app_with_temp_file("alpha");
        let other = PathBuf::from("/nonexistent/path.md");
        app.handle_file_changed(file_changed_event(other, "anything"));
        assert!(!app.modal_stack.contains::<DirtyConflictModal>());
        assert_eq!(app.last_disk_hash, Some(seahash::hash(b"alpha")));
    }

    #[test]
    fn second_external_change_refreshes_open_discard_confirm_modal() {
        use crate::app::modal::dirty_conflict_discard_confirm::DirtyConflictDiscardConfirmModal;
        // Dirty buffer → conflict modal → [Discard & reload] confirmation → a
        // second external write.  The confirmation's carried bytes must update,
        // so confirming reloads the latest disk state, not the first snapshot.
        let (mut app, tmp) = app_with_temp_file("alpha");
        let len = app.editor.buffer.len_chars();
        app.editor.buffer.insert_char(len, '!');
        app.editor.dirty = true;

        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "first"));
        assert!(app.modal_stack.contains::<DirtyConflictModal>());

        // Pushed directly, as the conflict modal's button-2 path would.
        app.modal_stack
            .push(Box::new(DirtyConflictDiscardConfirmModal::new(
                "first".to_owned(),
            )));

        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "second"));

        let child = app
            .modal_stack
            .find_first_mut::<DirtyConflictDiscardConfirmModal>()
            .expect("child modal still on stack");
        // The modal is dropped at test end, so taking the contents is harmless.
        let carried = std::mem::take(&mut child.on_disk_contents);
        assert_eq!(carried, "second");

        let parent = app
            .modal_stack
            .find_first_mut::<DirtyConflictModal>()
            .expect("parent modal still on stack");
        let parent_carried = std::mem::take(&mut parent.on_disk_contents);
        assert_eq!(parent_carried, "second");
    }
}
