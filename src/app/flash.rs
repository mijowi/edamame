//! Phase 9 transient hint-line message system.  Owns [`MessageKind`],
//! the [`TransientMessage`] payload, and the App-level hooks for
//! emitting / expiring / dismissing flash notifications.
//!
//! Pulled out of `app.rs` in Step 2 of `refactor-app.md`.

use std::time::{Duration, Instant};

use crate::config::KeyBindingOverrides;
use crate::config::KeyMap;
use crate::ui::{hint_line_for, HintContent};

use super::App;

/// Severity of a [`TransientMessage`].  Drives style selection and
/// decides whether the message auto-expires.  `Error` is sticky:
/// the user must dismiss with Escape or a subsequent `Error` replaces
/// it.  Non-error kinds expire after `config.editor.transient_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Success,
    Warning,
    Error,
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
    /// Phase 9 — emit a transient message on the hint line.  Non-error
    /// kinds auto-expire after `config.editor.transient_ms`; `Error`
    /// kinds stick until Escape or until a subsequent `Error` replaces
    /// them.  Called from every phase that wants a one-shot
    /// notification — save/copy/cut outcomes, link-open failures,
    /// `Config::save` successes, etc.
    pub fn flash(&mut self, text: impl Into<String>, kind: MessageKind) {
        let mut text = text.into();
        let until = match kind {
            MessageKind::Error => {
                text.push_str(" — Esc to dismiss");
                None
            }
            _ => Some(Instant::now() + Duration::from_millis(self.config.editor.transient_ms)),
        };
        self.transient = Some(TransientMessage { text, kind, until });
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
                MessageKind::Warning => self.theme.transient_warning,
                MessageKind::Error => self.theme.transient_error,
            };
            return HintContent::Transient {
                text: msg.text.clone(),
                style,
            };
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

    /// Clear a sticky `Error` transient on Escape, returning true to
    /// signal that the Escape was consumed and should not fall through
    /// to `Action::ExitToPreview`.  Non-sticky transients don't absorb
    /// Escape.
    pub(super) fn dismiss_sticky_transient(&mut self) -> bool {
        let Some(msg) = self.transient.as_ref() else {
            return false;
        };
        if matches!(msg.kind, MessageKind::Error) {
            self.transient = None;
            return true;
        }
        false
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
                    self.flash("Save failed", MessageKind::Error);
                }
            }
            Action::Copy | Action::Cut => {
                self.flash("Copied", MessageKind::Info);
            }
            _ => {}
        }
    }

    /// Persist `config.toml` and flash a `Configuration updated`
    /// notification on success.  Centralises the save-and-notify
    /// pattern so every caller (capability suppression, remote-image
    /// policy, future settings overlay) gets the same UX without
    /// sprinkling `flash()` calls through the dispatch paths.
    pub(super) fn save_config_with_flash(&mut self, err_context: &'static str) {
        match self.config.save() {
            Ok(()) => {
                self.flash("Configuration updated", MessageKind::Warning);
            }
            Err(e) => {
                tracing::warn!(error = %e, "{}", err_context);
                self.flash(format!("Config save failed: {e}"), MessageKind::Error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Phase 9 — exercise the transient-message mechanics directly
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
        assert!(msg.until.is_some(), "non-error messages auto-expire");
    }

    #[test]
    fn flash_error_is_sticky() {
        let mut app = make_app();
        app.flash("Save failed", MessageKind::Error);
        let msg = app.transient.as_ref().unwrap();
        assert!(msg.until.is_none(), "errors have no expiry deadline");
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
    fn expire_leaves_stick_errors() {
        let mut app = make_app();
        app.flash("Boom", MessageKind::Error);
        assert!(!app.expire_transient_if_due());
        assert!(app.transient.is_some());
    }

    #[test]
    fn dismiss_sticky_transient_on_escape() {
        let mut app = make_app();
        app.flash("Boom", MessageKind::Error);
        assert!(app.dismiss_sticky_transient());
        assert!(app.transient.is_none());
    }

    #[test]
    fn dismiss_sticky_ignores_non_error() {
        let mut app = make_app();
        app.flash("Saved", MessageKind::Success);
        assert!(!app.dismiss_sticky_transient());
        assert!(
            app.transient.is_some(),
            "non-errors must not clear on escape"
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
    fn flash_for_action_save_failure_emits_error() {
        let mut app = make_app();
        // Failure: dirty was true and remains true after "save".
        app.editor.dirty = true;
        app.flash_for_action(&Action::Save, /*dirty_before=*/ true);
        let msg = app.transient.as_ref().expect("flash recorded");
        assert!(matches!(msg.kind, MessageKind::Error));
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
    fn save_config_with_flash_emits_transient() {
        // `Config::save` *might* fail when no config dir is available in
        // the test environment.  Either branch produces a flash; we just
        // assert *some* transient is set so the user gets feedback.
        let mut app = make_app();
        app.save_config_with_flash("test");
        assert!(app.transient.is_some());
    }
}
