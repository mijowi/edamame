//! Transient hint-line messages: [`MessageKind`], [`TransientMessage`], and the App hooks
//! for emitting / expiring them, plus [`App::hint_content`] which decides what the hint
//! row shows each frame.

use std::time::{Duration, Instant};

use crate::config;
use crate::config::KeyBindingOverrides;
use crate::config::KeyMap;
use crate::ui::{hint_line_for, HintContent, HintCtx, HintSet, ModalKind};

use super::modal::{Modal, NoticeModal};
use super::App;

/// Severity of a [`TransientMessage`]; drives style selection.  Every kind auto-expires —
/// anything needing acknowledgement goes through [`App::notify`] instead.
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
    /// Auto-expiry deadline; `None` means sticky.
    pub(super) until: Option<Instant>,
}

impl App {
    /// Emit a transient hint-line message for passive confirmations the user can safely
    /// miss.  Use [`Self::notify`] for anything that needs acknowledgement.
    pub fn flash(&mut self, text: impl Into<String>, kind: MessageKind) {
        let text = text.into();
        let until = Some(Instant::now() + Duration::from_millis(self.config.editor.transient_ms));
        self.transient = Some(TransientMessage { text, kind, until });
        self.needs_draw = true;
    }

    /// Surface a sticky [`NoticeModal`] on top of whatever modal triggered it.
    pub fn notify(&mut self, text: impl Into<String>, kind: ModalKind) {
        let text = text.into();
        // Coalesce identical consecutive notices so a retry loop doesn't pile up duplicates.
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

    /// Clear an expired transient; returns true when a redraw is needed.
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

    /// Expiry of the current transient, if any (feeds [`App::next_deadline`]).
    pub(super) fn transient_deadline(&self) -> Option<Instant> {
        self.transient.as_ref().and_then(|m| m.until)
    }

    /// Build the hint content for this frame.  Priority order: Prompt, then CommandLine, then
    /// Transient, then hovered-link, then Chords — so a `Saved` flash or a file-changed prompt
    /// is never masked by an idle hover.
    pub(super) fn hint_content(&self) -> HintContent {
        if let Some(prompt) = self.hint_prompt.as_ref() {
            return HintContent::Prompt {
                prompt: prompt.prompt.clone(),
                chords: prompt.chords.clone(),
            };
        }
        if let Some(cl) = self.vim.as_ref().and_then(|v| v.cmdline.as_ref()) {
            return HintContent::CommandLine {
                prefix: cl.kind.prefix(),
                text: cl.input.clone(),
                cursor: cl.cursor,
                cursor_visible: self.editor.cursor_blink.is_visible(),
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
        // Hover tooltip: a prelude with no chords reuses the Chords rendering path.
        if let Some(url) = self.hovered_link.as_ref() {
            return HintContent::Chords(HintSet {
                prelude: Some(url.clone()),
                chords: Vec::new(),
                search_match: None,
            });
        }
        // The live KeyMap so rebinds show on the next frame; the default keymap only
        // covers the window between `App::new` and the first `KeyMap::build` in `run`.
        let fallback;
        let keymap = match self.keymap.as_ref() {
            Some(km) => km,
            None => {
                fallback = KeyMap::build(&KeyBindingOverrides::default())
                    .expect("default keymap always builds");
                &fallback
            }
        };
        // `visual_line`: a single-line V-LINE selection is charwise-empty, so the hint line
        // can't infer it from the selection alone.  `vim_enabled` drops the `Esc Preview`
        // chord, unreachable under vim.
        let ctx = HintCtx {
            nav_available: !self.nav_back.is_empty() || !self.nav_forward.is_empty(),
            visual_line: self.vim.as_ref().is_some_and(|v| v.is_visual_line()),
            vim_enabled: self.vim.is_some(),
        };
        HintContent::Chords(hint_line_for(&self.editor, keymap, ctx))
    }

    /// Emit the flash matching `action` after dispatch, keeping UI messaging out of
    /// `edit_ops::apply`.  Copy / Cut flash from `App::flush_clipboard_write` instead, which
    /// knows whether anything was copied.
    pub(super) fn flash_for_action(
        &mut self,
        action: &crate::config::Action,
        dirty_before_save: bool,
    ) {
        if *action == crate::config::Action::Save && dirty_before_save {
            if self.editor.dirty {
                self.notify("Save failed", ModalKind::Error);
            } else {
                self.flash("Saved", MessageKind::Success);
            }
        }
    }

    /// Flash shown when a modal flow's default-deny gate drops an action; `flow` names the
    /// flow ("search", "diff review").
    pub(super) fn flash_action_unavailable(&mut self, flow: &str) {
        self.flash(format!("Not available during {flow}"), MessageKind::Info);
        self.needs_draw = true;
    }

    /// Persist `config.toml` and flash `Configuration updated` on success, or notify on
    /// failure.
    pub(super) fn save_config_with_flash(&mut self, err_context: &'static str) {
        match self.config.save() {
            Ok(()) => {
                let msg = format!("Configuration updated{}", config::unpersisted_suffix());
                self.flash(msg, MessageKind::Success);
            }
            Err(e) => {
                tracing::warn!(error = %e, "{}", err_context);
                self.notify(format!("Config save failed: {e}"), ModalKind::Error);
            }
        }
    }

    /// Persist `state.toml` and flash `Configuration updated` on success, or notify on failure.
    /// The state counterpart of [`Self::save_config_with_flash`], for outcomes that record only
    /// machine bookkeeping (e.g. a newly seen terminal) and change no `config.toml` setting.
    pub(super) fn save_state_with_flash(&mut self, err_context: &'static str) {
        match self.state.save() {
            Ok(()) => {
                let msg = format!("Configuration updated{}", config::unpersisted_suffix());
                self.flash(msg, MessageKind::Success);
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

    /// Pins the wiring end-to-end: hard-coding `HintCtx::visual_line = false` would pass
    /// every `hint_line_for` unit test while restoring the original bug.
    #[test]
    fn hint_content_derives_visual_line_from_vim_state() {
        use crate::document::Selection;
        use crate::input::vim::state::{VimState, VimSubMode};
        let mut app = make_app();
        app.editor.buffer.insert(0, "alpha\nbeta\n");
        app.editor.mode = crate::editor::Mode::Rendered;
        app.editor.refresh_parsed();
        app.editor.selection = Some(Selection {
            anchor: 0,
            active: 0,
        });
        app.vim = Some(VimState {
            sub_mode: VimSubMode::VisualLine,
            visual_anchor: Some(0),
            ..Default::default()
        });
        match app.hint_content() {
            HintContent::Chords(set) => {
                let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
                assert_eq!(
                    labels,
                    vec!["Cut", "Copy", "Paste"],
                    "hint_content must pass the live V-LINE sub-mode through to hint_line_for"
                );
            }
            other => panic!("expected Chords, got {other:?}"),
        }
        app.vim = Some(VimState::default());
        app.editor.selection = None;
        match app.hint_content() {
            HintContent::Chords(set) => {
                assert_eq!(set.chords[0].label, "Menu");
            }
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
        // Either outcome is accepted: `Config::save` may fail without a config dir.
        use crate::app::modal::NoticeModal;
        let _iso = crate::test_env::config_isolation();
        let mut app = make_app();
        app.save_config_with_flash("test");
        assert!(app.transient.is_some() || app.modal_stack.contains::<NoticeModal>());
    }
}
