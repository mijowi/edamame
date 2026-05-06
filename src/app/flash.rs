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
