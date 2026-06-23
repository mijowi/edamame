//! Event-loop arm for [`crate::AppEvent::Watcher`].
//!
//! The watcher worker has already done the disk read; this module
//! decides what (if anything) to do with the resulting bytes, in this
//! order:
//!
//! 1. **Own-write filter.**  If the incoming hash matches
//!    [`App::last_disk_hash`], the bytes are byte-identical to what
//!    we last observed on disk — either our own save echo or a
//!    no-op write by an external tool.  Drop silently.
//! 2. **Already-reviewing reconcile.**  If a diff review is already
//!    open, fold the new disk state into it in place via
//!    `App::reconcile_diff_with_disk`, preserving decisions on
//!    untouched hunks (§11b).  This precedes the no-diff short-circuit
//!    because in diff mode the buffer is the pre-diff original, so
//!    "disk == buffer" there means "all changes reverted" (exit diff),
//!    not "no-op".
//! 3. **No-diff short-circuit.**  If disk bytes equal the live
//!    buffer bytes, no diff would be produced — silently stamp
//!    `last_disk_hash` and return.  (This is the common "external
//!    tool re-saved the file unchanged" case; doing this before the
//!    stamp step below avoids hashing the same bytes twice when
//!    they also happen to match the buffer.)
//! 4. **Stamp & dispatch.**  Update `last_disk_hash` to the
//!    incoming bytes so any further echoes of the same content are
//!    filtered out, then route to one of:
//!    - Clean buffer → enter diff review directly (never silent
//!      reload — see §11a of the diff-mode plan).  There is no
//!      unsaved work to reconcile, so the conflict modal is skipped,
//!      but the change is still surfaced hunk by hunk and the buffer
//!      is not overwritten until the user resolves.
//!    - Dirty buffer → open the
//!      [`super::modal::DirtyConflictModal`] so the user can
//!      reconcile (or refresh the carried bytes on an already-open
//!      conflict modal stack).
//!
//! The buffer is **never** silently reloaded/overwritten on an
//! external change: the whole point of diff mode is that the user
//! sees every change before it replaces what they are looking at.
//! The only events that bypass review are genuine no-ops (filters 1
//! and 3 above), where disk has nothing new to show.
//!
//! Read errors from the worker (non-UTF-8 contents, file deleted
//! between event and read, permission denied) are surfaced through
//! a dismissable warning modal rather than dropped silently — the
//! user otherwise has no signal that their external-edit prompts
//! have stopped firing for this file.

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
    /// Top-level dispatch for a single [`WatchedEvent`] from the
    /// watcher worker.  Splits into [`Self::handle_file_changed`]
    /// (happy path) and a warning-modal surface for read failures.
    pub(crate) fn handle_watcher_event(&mut self, event: WatchedEvent) {
        match event {
            WatchedEvent::Change(change) => self.handle_file_changed(change),
            WatchedEvent::Removed { path } => self.handle_file_removed(path),
            WatchedEvent::ReadError { path, error } => {
                // Drop errors for files we are no longer editing.
                // The worker may have an in-flight read queued from
                // before a file switch.
                if self.file_path.as_deref() != Some(path.as_path()) {
                    return;
                }
                // `notify` dedups repeated identical messages so a
                // file stuck in a non-UTF-8 / deleted state doesn't
                // stack multiple modals as the watcher retries.
                self.notify(
                    format!("Could not read {}: {}", path.display(), error),
                    ModalKind::Warning,
                );
            }
        }
    }

    /// Handle the watched file disappearing from disk.  Unlike an
    /// external *change*, a deletion never enters diff review — there
    /// is nothing on disk to diff against — so any open diff is
    /// collapsed first, then a [`FileDeletedModal`] offers to re-save
    /// the buffer (the only remaining copy of its contents).
    ///
    /// The modal appears regardless of the dirty flag: even an
    /// unmodified buffer is now the sole copy once the file is gone.
    pub(crate) fn handle_file_removed(&mut self, path: PathBuf) {
        // Drop deletions for files we are no longer editing — a stale
        // in-flight read from before a file switch.
        if self.file_path.as_deref() != Some(path.as_path()) {
            return;
        }
        // Idempotent: a second deletion signal (e.g. a forced
        // reconcile) must not stack a duplicate modal — neither the
        // prompt itself nor its `[Save as…]` path-entry child (which
        // closes the prompt before opening, so the prompt alone is not
        // enough to detect an in-progress save-as flow).  Only a
        // *deletion-recovery* save-as counts here — a voluntary save-as
        // on a live file must not swallow a genuine deletion.
        if self.modal_stack.contains::<FileDeletedModal>()
            || self
                .modal_stack
                .find_first::<SaveAsModal>()
                .is_some_and(|m| m.is_deletion_recovery())
        {
            return;
        }
        // A diff review compares against a file that no longer exists;
        // collapse it (restoring pre-diff scroll) before prompting.
        if self.editor.diff.is_some() {
            self.exit_diff_mode_discarding();
        }
        self.modal_stack.push(Box::new(FileDeletedModal::new(path)));
        self.needs_draw = true;
    }

    /// Dispatch a single successful read from the watcher.  See
    /// module docs for the decision tree.
    pub(crate) fn handle_file_changed(&mut self, change: WatchedChange) {
        // Drop events for files we are not currently editing.  Can
        // happen if the user rapidly switches files while a debounce
        // window is in flight on the previous one.
        if self.file_path.as_deref() != Some(change.path.as_path()) {
            return;
        }

        // The user is mid-`[Save as…]` after a deletion: they have
        // already committed to writing their buffer out as the
        // resolution, so an external recreate must not yank that prompt
        // away or — worse — enter diff review behind it.  Skip the
        // change entirely; completing the save-as re-points the buffer
        // and the write reads back as an own-write.  (`handle_file_removed`
        // makes the symmetric choice, treating an open save-as child as
        // an in-progress deletion flow.)
        if self
            .modal_stack
            .find_first::<SaveAsModal>()
            .is_some_and(|m| m.is_deletion_recovery())
        {
            return;
        }

        // The file is back on disk.  If a `FileDeletedModal` is still
        // open from an earlier deletion, its "the buffer is the only
        // copy" premise no longer holds — tear it down before reviewing
        // the change so diff mode is never entered behind it.  (Without
        // this, a delete-then-recreate sequence would push the read
        // through to `enter_diff_mode` below while the modal still sat
        // on the stack.)
        self.modal_stack.remove_first::<FileDeletedModal>();

        let incoming_hash = seahash::hash(change.contents.as_bytes());

        // 1. Own-write filter.
        if self.last_disk_hash == Some(incoming_hash) {
            return;
        }

        // 2. Already reviewing: fold the new disk state into the live
        //    review, preserving decisions on hunks the write didn't
        //    touch (§11b).  This must precede the buffer-vs-disk filter
        //    below — in diff mode `editor.buffer` is the pre-diff
        //    original (== `old_rope`), so that filter's "disk == buffer"
        //    actually means "all changes reverted", which here means
        //    "collapse the review and exit", not "no-op".
        if self.editor.diff.is_some() {
            self.last_disk_hash = Some(incoming_hash); // stamp-before-dispatch
            self.reconcile_diff_with_disk(change.contents);
            return;
        }

        // 3. Buffer-vs-disk short-circuit: no diff would be
        //    produced.  Stamp and return — done before step 3 so
        //    the dirty-conflict modal is not opened for
        //    byte-identical state.
        let buffer_text = self.editor.buffer.contents();
        let buffer_hash = seahash::hash(buffer_text.as_bytes());
        if incoming_hash == buffer_hash {
            self.last_disk_hash = Some(incoming_hash);
            return;
        }

        // 4a. Stamp before dispatching the change so any further
        //     echoes that overlap modal-open time are filtered out.
        //     Re-use the hash we already computed above.
        self.last_disk_hash = Some(incoming_hash);

        // 4b. Dispatch.
        if self.editor.dirty {
            // If the user is already mid-flow on a prior conflict —
            // i.e. they've opened the [Save a copy] or [Discard &
            // reload] child modal — refresh the bytes that child is
            // holding so confirming its action reloads against the
            // *current* disk state, not the stale snapshot that
            // originally opened the modal.  Also refresh the parent
            // `DirtyConflictModal` underneath so that cancelling the
            // child returns the user to a modal whose carried bytes
            // are still in sync.
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
            // No child modal open — replace any existing
            // DirtyConflictModal so the user reconciles against the
            // freshest disk contents rather than acting on stale bytes.
            self.modal_stack.remove_first::<DirtyConflictModal>();
            self.modal_stack
                .push(Box::new(DirtyConflictModal::new(change.contents)));
        } else {
            // Clean buffer: no unsaved work to reconcile, but the
            // change is still reviewed hunk by hunk.  The buffer is
            // NOT silently overwritten — `enter_diff_mode` opens diff
            // review (and `DiffIntroModal` on first run) so the user
            // sees the external change before it replaces what they
            // are looking at.  See §11a of the diff-mode plan.
            self.enter_diff_mode(change.contents);
        }
    }

    /// Fold a mid-review external write into the open diff in place
    /// (§11b) rather than wholesale-resetting the review.  Decisions on
    /// hunks the write did not touch are carried forward; hunks whose
    /// new-side target changed reset to `Pending`; vanished hunks drop
    /// their decisions.  When the write reverts every change (disk ==
    /// `old_rope`), the review collapses and diff mode exits.
    ///
    /// Never calls `enter_diff_mode`, so no `DiffIntroModal` is pushed
    /// (we are already in diff).  The own-write hash is stamped by the
    /// caller before dispatch, matching the rest of `handle_file_changed`.
    fn reconcile_diff_with_disk(&mut self, new_disk: String) {
        let outcome = self
            .editor
            .diff
            .as_mut()
            .expect("guarded by diff.is_some()")
            .reconcile_with_disk(&new_disk);
        match outcome {
            ReconcileOutcome::StillReviewing { reset } => {
                // Re-center on the focused hunk next frame, when the run
                // loop knows the viewport height.
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

    /// Replace the in-memory buffer contents with the bytes from
    /// the file-change event.  Both callers are explicit,
    /// user-confirmed choices from `DirtyConflictModal` — never a
    /// silent action: `[Discard & reload]` (after its confirmation
    /// sub-modal) abandons the unsaved edits for the disk version, and
    /// `[Save a copy]` writes the user's edits aside and then loads
    /// the disk version into the buffer.  Clean-buffer external
    /// changes do NOT come here — they enter diff review instead; see
    /// the module docs and §11a of the diff-mode plan.
    ///
    /// The watcher worker already read the bytes; reuse them rather
    /// than re-reading from disk — a second read here would race the
    /// next watcher event.  Carry the previous buffer's `version`
    /// forward through [`crate::document::Buffer::reload`] so the
    /// monotonic-version invariant downstream consumers rely on
    /// (autosave edit detector, visual-row cache invalidation)
    /// survives the swap.
    ///
    /// Preserves cursor offset / viewport scroll best-effort: a long
    /// external rewrite may push the cursor off the end, which
    /// [`crate::editor::EditorState::replace_buffer`] clamps.
    pub(crate) fn reload_buffer_from_disk(&mut self, contents: String) {
        let Some(path) = self.editor.buffer.path().map(|p| p.to_path_buf()) else {
            // Unnamed buffer — should never happen (the watcher
            // only fires when a path was set), but be defensive.
            return;
        };
        let previous_version = self.editor.buffer.version();
        let new_buffer = crate::document::Buffer::reload(&path, &contents, previous_version);
        // Tear down any active search flow at the App level first so
        // the deferred-advance timer is cancelled along with the
        // session (`replace_buffer` would drop the session anyway,
        // but would leave the timer armed).
        self.exit_search_flow();
        self.editor.replace_buffer(new_buffer);
        // New contents may reference different image links; mark the
        // image cache for reconciliation on the next loop iteration
        // to match the convention used by `load_file_into_editor`.
        self.images_dirty = true;
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

    /// Build an `App` whose editor buffer holds `initial` and is
    /// associated with a temp path; returns the temp file handle so
    /// the test can mutate it.  The watcher hash filter is seeded
    /// from `initial` so subsequent FileChanged events are compared
    /// against a real hash rather than `None`.
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
    fn own_write_echo_is_dropped() {
        // Saving the buffer stamps `last_disk_hash`; an immediate
        // FileChanged with the same contents must be filtered out
        // and produce no UI side effects.
        let (mut app, tmp) = app_with_temp_file("alpha");
        // Simulate the byte-identical inotify echo.
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
        // A clean buffer is NOT silently reloaded — the external
        // change is surfaced for review and the buffer keeps its
        // original content until the user resolves (§11a).
        let (mut app, tmp) = app_with_temp_file("alpha");
        assert!(!app.editor.dirty);
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "beta"));
        assert!(
            app.editor.diff.is_some(),
            "clean buffer + external change must enter diff review",
        );
        assert_eq!(app.editor.mode, crate::editor::Mode::Diff);
        // Clean entry skips the conflict prompt — there is no unsaved
        // work to reconcile.
        assert!(
            !app.modal_stack.contains::<DirtyConflictModal>(),
            "clean entry must not open the dirty-conflict modal",
        );
        // The buffer is untouched until the user resolves the diff.
        assert_eq!(
            app.editor.buffer.contents(),
            "alpha",
            "buffer must not be silently overwritten",
        );
        // Hash was stamped to the new contents.
        assert_eq!(app.last_disk_hash, Some(seahash::hash(b"beta")));
    }

    #[test]
    fn clean_buffer_byte_identical_disk_does_not_enter_diff() {
        // "Clean" is not the no-op condition — "disk == buffer" is.
        // A clean buffer whose incoming disk content equals the buffer
        // must skip review (filter 2), not enter diff.
        let (mut app, tmp) = app_with_temp_file("alpha");
        assert!(!app.editor.dirty);
        // Force last_disk_hash to differ so the own-write filter
        // (filter 1) doesn't short-circuit first; the buffer-vs-disk
        // filter (filter 2) is the one under test.
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
        // Dirty up the buffer.
        let len = app.editor.buffer.len_chars();
        app.editor.buffer.insert_char(len, '!');
        app.editor.dirty = true;
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "external"));
        assert!(
            app.modal_stack.contains::<DirtyConflictModal>(),
            "dirty buffer + external change must open the conflict modal",
        );
        // The buffer is untouched until the user picks a button.
        assert!(app.editor.buffer.contents().ends_with('!'));
    }

    #[test]
    fn disk_equal_to_buffer_skips_modal_and_stamps_hash() {
        // External tool re-wrote the file with bytes that happen
        // to match what's already in the buffer.  This may differ
        // from `last_disk_hash` (e.g. the user typed `'!'` and
        // saved; the watcher's hash is the pre-save value).  No
        // diff would be produced — just stamp and return.
        let (mut app, tmp) = app_with_temp_file("alpha");
        let len = app.editor.buffer.len_chars();
        app.editor.buffer.insert_char(len, '!');
        app.editor.dirty = true;
        // Force last_disk_hash to differ from the current buffer
        // contents so the own-write filter doesn't short-circuit.
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
        // Clean buffer with two changeable regions.  First external
        // write enters diff (two hunks: line 1 b→B, line 3 d→D).
        let (mut app, tmp) = app_with_temp_file("a\nb\nc\nd\ne\n");
        app.handle_file_changed(file_changed_event(
            tmp.path().to_path_buf(),
            "a\nB\nc\nD\ne\n",
        ));
        assert!(app.editor.diff.is_some(), "first change enters diff");
        let diff = app.editor.diff.as_ref().unwrap();
        assert_eq!(diff.hunks.len(), 2);
        let h0_id = diff.hunks[0].id;
        // Accept the first hunk.
        app.editor.diff.as_mut().unwrap().decisions[0] = Decision::Accepted;
        app.transient = None;

        // Second external write touches only the second region (D → DD).
        app.handle_file_changed(file_changed_event(
            tmp.path().to_path_buf(),
            "a\nB\nc\nDD\ne\n",
        ));

        // Still reviewing — not wholesale-reset.
        assert!(app.editor.diff.is_some());
        assert_eq!(app.editor.mode, crate::editor::Mode::Diff);
        let diff = app.editor.diff.as_ref().unwrap();
        // h0 survived with its id and its Accepted decision intact.
        let h0 = diff.hunks.iter().position(|h| h.id == h0_id).expect("h0");
        assert_eq!(diff.decisions[h0], Decision::Accepted);
        // A reconcile flash was recorded.
        assert!(app.transient.is_some(), "reconcile records a flash");
    }

    #[test]
    fn external_revert_in_diff_exits_diff() {
        // Enter diff, then an external write reverts disk back to the
        // original buffer contents → the review collapses and exits.
        let (mut app, tmp) = app_with_temp_file("a\nb\n");
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "a\nB\n"));
        assert!(app.editor.diff.is_some(), "first change enters diff");

        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "a\nb\n"));

        assert!(app.editor.diff.is_none(), "revert exits diff mode");
        assert_ne!(app.editor.mode, crate::editor::Mode::Diff);
        // The buffer was never overwritten during the review.
        assert_eq!(app.editor.buffer.contents(), "a\nb\n");
    }

    #[test]
    fn deletion_opens_file_deleted_modal_for_clean_buffer() {
        use crate::app::modal::FileDeletedModal;
        // Even a clean (unmodified) buffer prompts — once the file is
        // gone the in-memory copy is the only one left.
        let (mut app, tmp) = app_with_temp_file("alpha");
        assert!(!app.editor.dirty);
        app.handle_file_removed(tmp.path().to_path_buf());
        assert!(
            app.modal_stack.contains::<FileDeletedModal>(),
            "deletion must surface the file-deleted modal",
        );
        // Deletion never enters diff review.
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
        // Enter diff via an external change, then delete the file: the
        // diff collapses and the deletion modal takes over.
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
        // User is mid-review with the diff-quit confirmation open, then
        // the file is deleted.  Exiting the diff must tear down the now
        // meaningless quit-confirm so it can't fire from underneath the
        // file-deleted modal.
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
        // A second deletion signal that arrives while the user is in
        // the `[Save as…]` path-entry child (the prompt itself is
        // closed) must not stack a fresh prompt on top of it.
        let (mut app, tmp) = app_with_temp_file("alpha");
        app.handle_file_removed(tmp.path().to_path_buf());
        // Simulate the user picking `[Save as…]`: the prompt closes and
        // the path-entry modal opens in its place.
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
        // File deleted (prompt open), then recreated externally with
        // new contents.  The prompt's "only copy" premise is void, so
        // it is torn down and the change enters normal diff review —
        // never behind the modal.
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
        // File deleted, user picks `[Save as…]` (prompt closed, path
        // entry open), then the file is recreated externally.  The
        // user has committed to saving their buffer out, so the change
        // must be skipped — never entering diff review behind the open
        // save-as path entry.
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
        // A *voluntary* Save As (not the deletion-recovery flow) is just a
        // path-entry prompt over a live file — it must NOT swallow a
        // genuine external change the way the deletion-recovery flow does.
        // This locks in the `is_deletion_recovery()` narrowing of the
        // watcher dedup: a `SaveAsModal` whose flag is false leaves
        // `handle_file_changed` to dispatch the conflict as normal.
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
        // Hash is untouched — the event was for a path we don't
        // own.
        assert_eq!(app.last_disk_hash, Some(seahash::hash(b"alpha")));
    }

    #[test]
    fn second_external_change_refreshes_open_discard_confirm_modal() {
        use crate::app::modal::dirty_conflict_discard_confirm::DirtyConflictDiscardConfirmModal;
        // Walk the user through: dirty buffer → first external write
        // opens DirtyConflictModal → user picks [Discard & reload] →
        // confirmation modal opens with first bytes → second external
        // write arrives while confirmation is up.  The confirmation
        // modal's carried bytes must be updated so confirming reloads
        // the *latest* disk state, not the stale first snapshot.
        let (mut app, tmp) = app_with_temp_file("alpha");
        let len = app.editor.buffer.len_chars();
        app.editor.buffer.insert_char(len, '!');
        app.editor.dirty = true;

        // First external write → opens DirtyConflictModal.
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "first"));
        assert!(app.modal_stack.contains::<DirtyConflictModal>());

        // Push the confirmation modal directly to simulate "user clicked
        // [Discard & reload]" (the conflict modal's button-2 path
        // pushes this same modal type carrying the same bytes).
        app.modal_stack
            .push(Box::new(DirtyConflictDiscardConfirmModal::new(
                "first".to_owned(),
            )));

        // Second external write while the child is open.
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "second"));

        // The child's carried bytes must now reflect the second write.
        let child = app
            .modal_stack
            .find_first_mut::<DirtyConflictDiscardConfirmModal>()
            .expect("child modal still on stack");
        // Use mem::take to inspect the carried contents.  The modal is
        // about to be dropped at test end so this is harmless.
        let carried = std::mem::take(&mut child.on_disk_contents);
        assert_eq!(carried, "second");

        // And the parent under it was kept in sync too.
        let parent = app
            .modal_stack
            .find_first_mut::<DirtyConflictModal>()
            .expect("parent modal still on stack");
        let parent_carried = std::mem::take(&mut parent.on_disk_contents);
        assert_eq!(parent_carried, "second");
    }
}
