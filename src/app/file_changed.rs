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
//! 2. **No-diff short-circuit.**  If disk bytes equal the live
//!    buffer bytes, no diff would be produced — silently stamp
//!    `last_disk_hash` and return.  (This is the common "external
//!    tool re-saved the file unchanged" case; doing this before the
//!    stamp step below avoids hashing the same bytes twice when
//!    they also happen to match the buffer.)
//! 3. **Stamp & dispatch.**  Update `last_disk_hash` to the
//!    incoming bytes so any further echoes of the same content are
//!    filtered out, then route to one of:
//!    - Clean buffer → reload from disk silently.
//!    - Dirty buffer → open the
//!      [`super::modal::DirtyConflictModal`] so the user can
//!      reconcile (or refresh the carried bytes on an already-open
//!      conflict modal stack).
//!
//! Read errors from the worker (non-UTF-8 contents, file deleted
//! between event and read, permission denied) are surfaced through
//! a dismissable warning modal rather than dropped silently — the
//! user otherwise has no signal that their external-edit prompts
//! have stopped firing for this file.
//!
//! CP2 stops here.  CP3 wires up the `[Merge]` button to actually
//! enter diff mode; today it flashes the placeholder.

use crate::ui::ModalKind;
use crate::watcher::{WatchedChange, WatchedEvent};

use super::modal::dirty_conflict_discard_confirm::DirtyConflictDiscardConfirmModal;
use super::modal::dirty_conflict_save_copy::DirtyConflictSaveCopyModal;
use super::modal::DirtyConflictModal;
use super::App;

impl App {
    /// Top-level dispatch for a single [`WatchedEvent`] from the
    /// watcher worker.  Splits into [`Self::handle_file_changed`]
    /// (happy path) and a warning-modal surface for read failures.
    pub(crate) fn handle_watcher_event(&mut self, event: WatchedEvent) {
        match event {
            WatchedEvent::Change(change) => self.handle_file_changed(change),
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

    /// Dispatch a single successful read from the watcher.  See
    /// module docs for the decision tree.
    pub(crate) fn handle_file_changed(&mut self, change: WatchedChange) {
        // Drop events for files we are not currently editing.  Can
        // happen if the user rapidly switches files while a debounce
        // window is in flight on the previous one.
        if self.file_path.as_deref() != Some(change.path.as_path()) {
            return;
        }

        let incoming_hash = seahash::hash(change.contents.as_bytes());

        // 1. Own-write filter.
        if self.last_disk_hash == Some(incoming_hash) {
            return;
        }

        // 2. Buffer-vs-disk short-circuit: no diff would be
        //    produced.  Stamp and return — done before step 3 so
        //    the dirty-conflict modal is not opened for
        //    byte-identical state.
        let buffer_text = self.editor.buffer.contents();
        let buffer_hash = seahash::hash(buffer_text.as_bytes());
        if incoming_hash == buffer_hash {
            self.last_disk_hash = Some(incoming_hash);
            return;
        }

        // 3a. Stamp before dispatching the change so any further
        //     echoes that overlap modal-open time are filtered out.
        //     Re-use the hash we already computed above.
        self.last_disk_hash = Some(incoming_hash);

        // 3b. Dispatch.
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
            self.reload_buffer_from_disk(change.contents);
        }
    }

    /// Replace the in-memory buffer contents with the bytes from
    /// the file-change event.  Used by the silent-reload path
    /// (clean buffer) and by `DirtyConflictModal`'s `[Discard &
    /// reload]` button once the user confirms.
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
    /// [`crate::editor::EditorState::replace_buffer`] clamps.  CP3's
    /// diff-mode entry will supersede this codepath for the merge
    /// case.
    pub(crate) fn reload_buffer_from_disk(&mut self, contents: String) {
        let Some(path) = self.editor.buffer.path().map(|p| p.to_path_buf()) else {
            // Unnamed buffer — should never happen (the watcher
            // only fires when a path was set), but be defensive.
            return;
        };
        let previous_version = self.editor.buffer.version();
        let new_buffer = crate::document::Buffer::reload(&path, &contents, previous_version);
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
    fn external_change_with_clean_buffer_reloads_silently() {
        let (mut app, tmp) = app_with_temp_file("alpha");
        assert!(!app.editor.dirty);
        app.handle_file_changed(file_changed_event(tmp.path().to_path_buf(), "beta"));
        assert!(
            !app.modal_stack.contains::<DirtyConflictModal>(),
            "clean buffer must reload without a modal",
        );
        assert_eq!(app.editor.buffer.contents(), "beta");
        // Hash was stamped to the new contents.
        assert_eq!(app.last_disk_hash, Some(seahash::hash(b"beta")));
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
