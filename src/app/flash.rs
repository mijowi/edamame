//! Transient hint-line message system.  Owns [`MessageKind`],
//! the [`TransientMessage`] payload, and the App-level hooks for
//! emitting / expiring / dismissing flash notifications.
//!
//! Pulled out of `app.rs` in Step 2 of `refactor-app.md`.

use std::time::{Duration, Instant};

use crate::config::KeyBindingOverrides;
use crate::config::KeyMap;
use crate::ui::{hint_line_for, HintContent, HintSet, ModalKind};

use super::modal::{Modal, NoticeModal};
use super::App;

/// Severity of a [`TransientMessage`].  Drives style selection.
/// All transient kinds auto-expire after `config.editor.transient_ms`;
/// situations that need the user to actually acknowledge a message
/// (errors, rejections, stubs) use [`App::notify`] to push a sticky
/// [`NoticeModal`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Success,
}

/// A single transient status message shown in the hint line.
#[derive(Debug, Clone)]
pub(super) struct TransientMessage {
    pub(super) text: String,
    pub(super) kind: MessageKind,
    /// Wall-clock deadline after which non-error messages auto-expire.
    /// `None` for sticky errors.
    pub(super) until: Option<Instant>,
}

impl App {
    /// Emit a transient message on the hint line.  Auto-expires after
    /// `config.editor.transient_ms`.  Use for passive confirmations
    /// (saved, copied, file reloaded, etc.) — anything the user can
    /// safely miss.  For errors and rejections that need
    /// acknowledgement, use [`Self::notify`] instead.
    pub fn flash(&mut self, text: impl Into<String>, kind: MessageKind) {
        let text = text.into();
        let until = Some(Instant::now() + Duration::from_millis(self.config.editor.transient_ms));
        self.transient = Some(TransientMessage { text, kind, until });
        self.needs_draw = true;
    }

    /// Surface a sticky notification as a [`NoticeModal`].  Used for
    /// errors, rejections, and stub messages — anything the user needs
    /// to actually see and acknowledge rather than risk missing on the
    /// hint line.  Pushes onto [`Self::modal_stack`] so the notice
    /// stacks on top of any modal that triggered it; `Esc` dismisses.
    pub fn notify(&mut self, text: impl Into<String>, kind: ModalKind) {
        let text = text.into();
        // Coalesce: if the topmost modal is already a NoticeModal with
        // identical text+kind, skip the push so a retry loop (e.g.
        // repeated save failures) doesn't pile duplicates on the stack
        // for the user to dismiss one by one.
        if let Some(top) = self.modal_stack.top_mut() {
            if let Some(existing) = top.as_any().downcast_ref::<NoticeModal>() {
                if Modal::kind(existing) == kind && existing.text() == text {
                    return;
                }
            }
        }
        self.modal_stack
            .push(Box::new(NoticeModal::new(text, kind)));
        self.needs_draw = true;
    }

    /// Clear the current transient message if it has auto-expired.
    /// Called from the main loop before the draw gate so the hint line
    /// reverts to chords without the user having to press a key.
    /// Returns true when a redraw is needed.
    pub(super) fn expire_transient_if_due(&mut self) -> bool {
        let Some(msg) = self.transient.as_ref() else {
            return false;
        };
        let Some(deadline) = msg.until else {
            return false;
        };
        if Instant::now() >= deadline {
            self.transient = None;
            return true;
        }
        false
    }

    /// The deadline when the current transient expires, if any.
    /// Contributes to [`App::next_deadline`] so the main loop wakes in
    /// time to revert the hint line even with no input arriving.
    pub(super) fn transient_deadline(&self) -> Option<Instant> {
        self.transient.as_ref().and_then(|m| m.until)
    }

    /// Build the hint content for this frame.  Prompt > Transient >
    /// Chords, matching the plan's priority.
    pub(super) fn hint_content(&self) -> HintContent {
        if let Some(prompt) = self.hint_prompt.as_ref() {
            return HintContent::Prompt {
                prompt: prompt.prompt.clone(),
                chords: prompt.chords.clone(),
            };
        }
        if let Some(msg) = self.transient.as_ref() {
            let style = match msg.kind {
                MessageKind::Info => self.theme.transient_info,
                MessageKind::Success => self.theme.transient_success,
            };
            return HintContent::Transient {
                text: msg.text.clone(),
                style,
            };
        }
        // Hovered-link tooltip: while the mouse pointer rests on a link
        // the hint row shows its raw URL (browser-status-bar style),
        // replacing the chord row.  A prelude with no chords reuses the
        // Chords rendering path — plain `hint_label` text on the bar.
        // Sits below Prompt and Transient so a `Saved` flash or a
        // file-changed prompt is never masked by an idle hover.
        if let Some(url) = self.hovered_link.as_ref() {
            return HintContent::Chords(HintSet {
                prelude: Some(url.clone()),
                chords: Vec::new(),
            });
        }
        // Look up chord glyphs against the live KeyMap so any rebind
        // applied via the keybinds overlay shows up in the hint line
        // on the very next frame.  Falls back to the compiled-in
        // defaults during the brief window between `App::new` and the
        // first `KeyMap::build` in `run` — that path runs only when
        // building the override-aware keymap fails for unrelated
        // reasons, and the default keymap always builds.
        let fallback;
        let keymap = match self.keymap.as_ref() {
            Some(km) => km,
            None => {
                fallback = KeyMap::build(&KeyBindingOverrides::default())
                    .expect("default keymap always builds");
                &fallback
            }
        };
        HintContent::Chords(hint_line_for(&self.editor, keymap))
    }

    /// Inspect `action` after dispatch and emit the matching flash
    /// notification.  Centralising this here means every code path
    /// that calls `Action::Save` / `Copy` / `Cut` gets consistent
    /// messaging without polluting `edit_ops::apply` with UI concerns.
    pub(super) fn flash_for_action(
        &mut self,
        action: &crate::config::Action,
        dirty_before_save: bool,
    ) {
        use crate::config::Action;
        match action {
            Action::Save => {
                if dirty_before_save && !self.editor.dirty {
                    self.flash("Saved", MessageKind::Success);
                } else if dirty_before_save && self.editor.dirty {
                    self.notify("Save failed", ModalKind::Error);
                }
            }
            Action::Copy | Action::Cut => {
                self.flash("Copied", MessageKind::Info);
            }
            _ => {}
        }
    }

    /// Flash shown when a modal flow's default-deny gate drops an
    /// action — shared by diff review and the search flow so a denied
    /// keypress gets the same "why did nothing happen" feedback in
    /// both.  `flow` names the flow ("search", "diff review").
    pub(super) fn flash_action_unavailable(&mut self, flow: &str) {
        self.flash(format!("Not available during {flow}"), MessageKind::Info);
        self.needs_draw = true;
    }

    /// Persist `config.toml` and flash a `Configuration updated`
    /// notification on success.  Centralises the save-and-notify
    /// pattern so every caller (capability suppression, remote-image
    /// policy, future settings overlay) gets the same UX without
    /// sprinkling `flash()` calls through the dispatch paths.
    pub(super) fn save_config_with_flash(&mut self, err_context: &'static str) {
        match self.config.save() {
            Ok(()) => {
                self.flash("Configuration updated", MessageKind::Success);
            }
            Err(e) => {
                tracing::warn!(error = %e, "{}", err_context);
                self.notify(format!("Config save failed: {e}"), ModalKind::Error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Exercise the transient-message mechanics directly
    //! against an `App` instance, bypassing the event loop.  Builds use
    //! [`Capabilities::default`] and the default config; no terminal is
    //! ever acquired.

    use std::time::Duration;

    use super::*;
    use crate::app::test_utils::make_app;
    use crate::config::Action;
    use crate::ui::HintContent;

    #[test]
    fn flash_records_transient_info() {
        let mut app = make_app();
        assert!(app.transient.is_none());
        app.flash("Copied", MessageKind::Info);
        let msg = app.transient.as_ref().unwrap();
        assert_eq!(msg.text, "Copied");
        assert!(matches!(msg.kind, MessageKind::Info));
        assert!(msg.until.is_some(), "all flashes auto-expire");
    }

    #[test]
    fn expire_transient_clears_only_after_deadline() {
        let mut app = make_app();
        app.flash("Saved", MessageKind::Success);
        // Force the deadline into the past.
        if let Some(msg) = app.transient.as_mut() {
            msg.until = Some(Instant::now() - Duration::from_millis(1));
        }
        assert!(app.expire_transient_if_due());
        assert!(app.transient.is_none());
    }

    #[test]
    fn notify_pushes_notice_modal() {
        use crate::app::modal::NoticeModal;
        let mut app = make_app();
        app.notify("Boom", ModalKind::Error);
        assert!(app.modal_stack.contains::<NoticeModal>());
    }

    #[test]
    fn notify_coalesces_duplicate_top_notice() {
        let mut app = make_app();
        let base = app.modal_stack.len();
        app.notify("Save failed", ModalKind::Error);
        app.notify("Save failed", ModalKind::Error);
        app.notify("Save failed", ModalKind::Error);
        assert_eq!(
            app.modal_stack.len() - base,
            1,
            "identical consecutive notices must collapse to one modal"
        );
    }

    #[test]
    fn notify_does_not_coalesce_distinct_text_or_kind() {
        let mut app = make_app();
        let base = app.modal_stack.len();
        app.notify("Save failed", ModalKind::Error);
        app.notify("Reload failed", ModalKind::Error);
        app.notify("Save failed", ModalKind::Warning);
        assert_eq!(
            app.modal_stack.len() - base,
            3,
            "different text or kind must each push a fresh modal"
        );
    }

    #[test]
    fn flash_for_action_save_success_emits_saved_flash() {
        let mut app = make_app();
        // Simulate a successful save: dirty was true before and the
        // editor-state dirty flag has just flipped to false.
        app.editor.dirty = false;
        app.flash_for_action(&Action::Save, /*dirty_before=*/ true);
        let msg = app.transient.as_ref().expect("flash recorded");
        assert_eq!(msg.text, "Saved");
        assert!(matches!(msg.kind, MessageKind::Success));
    }

    #[test]
    fn flash_for_action_save_failure_pushes_error_modal() {
        use crate::app::modal::NoticeModal;
        let mut app = make_app();
        // Failure: dirty was true and remains true after "save".
        app.editor.dirty = true;
        app.flash_for_action(&Action::Save, /*dirty_before=*/ true);
        assert!(
            app.modal_stack.contains::<NoticeModal>(),
            "save failure must surface a sticky NoticeModal"
        );
        assert!(
            app.transient.is_none(),
            "save failure no longer leaves a transient flash"
        );
    }

    #[test]
    fn flash_for_action_copy_emits_copied() {
        let mut app = make_app();
        app.flash_for_action(&Action::Copy, /*dirty_before=*/ false);
        let msg = app.transient.as_ref().expect("flash recorded");
        assert_eq!(msg.text, "Copied");
    }

    #[test]
    fn flash_for_action_cut_emits_copied() {
        let mut app = make_app();
        app.flash_for_action(&Action::Cut, /*dirty_before=*/ false);
        let msg = app.transient.as_ref().expect("flash recorded");
        assert_eq!(msg.text, "Copied");
    }

    #[test]
    fn flash_for_action_paste_is_silent() {
        let mut app = make_app();
        app.flash_for_action(&Action::Paste, /*dirty_before=*/ false);
        assert!(app.transient.is_none());
    }

    #[test]
    fn hint_content_defaults_to_chords() {
        let app = make_app();
        match app.hint_content() {
            HintContent::Chords(_) => {}
            other => panic!("expected Chords, got {other:?}"),
        }
    }

    #[test]
    fn hint_content_prefers_transient_over_chords() {
        let mut app = make_app();
        app.flash("Copied", MessageKind::Info);
        match app.hint_content() {
            HintContent::Transient { text, .. } => assert_eq!(text, "Copied"),
            other => panic!("expected Transient, got {other:?}"),
        }
    }

    #[test]
    fn hovered_link_replaces_chord_row_with_url() {
        let mut app = make_app();
        app.hovered_link = Some("https://example.com".to_owned());
        match app.hint_content() {
            HintContent::Chords(set) => {
                assert_eq!(set.prelude.as_deref(), Some("https://example.com"));
                assert!(
                    set.chords.is_empty(),
                    "hover tooltip must replace the chord row, not prefix it"
                );
            }
            other => panic!("expected Chords with URL prelude, got {other:?}"),
        }
    }

    #[test]
    fn transient_outranks_hovered_link() {
        let mut app = make_app();
        app.hovered_link = Some("https://example.com".to_owned());
        app.flash("Saved", MessageKind::Success);
        match app.hint_content() {
            HintContent::Transient { text, .. } => assert_eq!(text, "Saved"),
            other => panic!("transient must mask the hover tooltip, got {other:?}"),
        }
    }

    #[test]
    fn clearing_hover_restores_chords() {
        let mut app = make_app();
        app.hovered_link = Some("./notes.md".to_owned());
        app.hovered_link = None;
        match app.hint_content() {
            HintContent::Chords(set) => assert!(!set.chords.is_empty()),
            other => panic!("expected default chord row, got {other:?}"),
        }
    }

    #[test]
    fn save_config_with_flash_emits_feedback() {
        // `Config::save` *might* fail when no config dir is available
        // in the test environment.  The success branch records a
        // transient; the failure branch pushes a NoticeModal — we
        // accept either so the test is robust to the environment.
        use crate::app::modal::NoticeModal;
        let mut app = make_app();
        app.save_config_with_flash("test");
        assert!(app.transient.is_some() || app.modal_stack.contains::<NoticeModal>());
    }
}
