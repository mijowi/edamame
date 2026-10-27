//! Keybinds overlay: adapter wrapping [`crate::ui::KeybindsState`].
//!
//! Edits are buffered in the overlay's draft keymap / overrides and reach the live
//! [`crate::config::KeyMap`] — and `keybindings.toml` — only on `[ Save ]`; Esc and
//! `[ Cancel ]` discard.  A save failure surfaces as a warning notice and keeps the overlay
//! open with the drafts intact.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::{App, MessageKind};
use crate::config;
use crate::config::{Config, KeyBindingOverrides, KeyMap};
use crate::ui::{KeybindsResponse, KeybindsState, KeybindsView, ModalKind};

pub struct KeybindsOverlayModal {
    state: KeybindsState,
}

impl KeybindsOverlayModal {
    pub fn new(keymap: &KeyMap, overrides: &KeyBindingOverrides, modal_enabled: bool) -> Self {
        Self {
            state: KeybindsState::open(keymap, overrides, modal_enabled),
        }
    }
}

/// Persist the draft overrides to `keybindings.toml`, returning `Err(message)` for the caller
/// to surface.
///
/// Under `--no-config` this writes nothing and reports success.  `save_to` truncates the whole
/// file from the in-memory overrides — which such a session never read — so without the gate
/// one rebind would replace the user's entire `keybindings.toml`.  See
/// [`crate::config::persistence`].
fn try_persist(overrides: &KeyBindingOverrides) -> Result<(), String> {
    if !config::config_writes_allowed() {
        return Ok(());
    }
    let Some(dir) = Config::config_dir() else {
        return Err("No config directory available — keybindings not saved".into());
    };
    let path = dir.join("keybindings.toml");
    overrides.save_to(&path).map_err(|e| {
        tracing::warn!(error = %e, "failed to write keybindings.toml");
        format!("Failed to save keybindings: {e}")
    })
}

/// Swap the drafts onto `app` after a successful persist.  When writes are suppressed the
/// flash says so rather than claiming a save that didn't happen.
fn install_drafts(app: &mut App, keymap: KeyMap, overrides: KeyBindingOverrides) {
    app.keymap = Some(keymap);
    app.keybindings = overrides;
    let msg = if config::config_writes_allowed() {
        "Keybindings saved".to_owned()
    } else {
        format!("Keybindings applied{}", config::NOT_PERSISTED_NOTE)
    };
    app.flash(msg, MessageKind::Success);
}

/// Map a `KeybindsResponse::Save` to a `ModalOutcome`: close-and-install on success, stay open
/// with a warning on failure.  `notify_inline` is `Some` only from `handle_key`, which holds
/// `&mut App`; `handle_click` does not, so its warning goes through `ContinueAnd`.
fn outcome_for_save(
    keymap: KeyMap,
    overrides: KeyBindingOverrides,
    notify_inline: Option<&mut App>,
) -> ModalOutcome {
    match try_persist(&overrides) {
        Ok(()) => {
            ModalOutcome::CloseAnd(Box::new(move |app| install_drafts(app, keymap, overrides)))
        }
        Err(msg) => match notify_inline {
            Some(app) => {
                app.notify(msg, ModalKind::Warning);
                ModalOutcome::Continue
            }
            None => ModalOutcome::ContinueAnd(Box::new(move |app| {
                app.notify(msg, ModalKind::Warning);
            })),
        },
    }
}

impl Modal for KeybindsOverlayModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = KeybindsView { theme: ctx.theme };
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        match self.state.handle_key(&key) {
            KeybindsResponse::Continue => ModalOutcome::Continue,
            KeybindsResponse::Cancelled => ModalOutcome::Close,
            KeybindsResponse::Save { keymap, overrides } => {
                outcome_for_save(keymap, overrides, Some(app))
            }
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_state.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        match self.state.handle_click(col, row) {
            KeybindsResponse::Continue => ModalOutcome::Continue,
            KeybindsResponse::Cancelled => ModalOutcome::Close,
            KeybindsResponse::Save { keymap, overrides } => {
                outcome_for_save(keymap, overrides, None)
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_utils::make_app;
    use crate::config::Action;

    #[test]
    fn open_keybinds_action_opens_combined_keybinds_overlay() {
        let mut app = make_app();
        let handled = app.handle_app_action(&Action::OpenKeybinds, 40, 80);
        assert!(handled);
        assert!(app.modal_stack.contains::<KeybindsOverlayModal>());
    }

    /// An ungated Save would replace the user's real `keybindings.toml` with whatever single
    /// rebind they made during a `--no-config` run.
    #[test]
    fn saving_writes_no_keybindings_file_while_writes_are_suppressed() {
        let _lock = crate::test_env::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _xdg = crate::test_env::EnvGuard::set("XDG_CONFIG_HOME", dir.path());
        let path = dir.path().join("edamame/keybindings.toml");

        let overrides = KeyBindingOverrides(
            [("Quit".to_owned(), "ctrl+x".to_owned())]
                .into_iter()
                .collect(),
        );

        {
            let _suppressed = crate::config::persistence::SuppressGuard::new();
            assert_eq!(
                try_persist(&overrides),
                Ok(()),
                "suppression is not a failure"
            );
            assert!(!path.exists(), "--no-config must not create {path:?}");
        }

        // Ungated the same call does write, so the assertion above is about the gate, not a
        // misdirected path.
        assert_eq!(try_persist(&overrides), Ok(()));
        assert!(path.exists());
        assert!(std::fs::read_to_string(&path).unwrap().contains("ctrl+x"));
    }

    /// The flash must not claim a save that didn't happen.
    #[test]
    fn the_save_flash_says_so_when_nothing_was_written() {
        let _lock = crate::test_env::env_lock();
        let mut app = make_app();
        let keymap = app.ensure_keymap_clone();

        {
            let _suppressed = crate::config::persistence::SuppressGuard::new();
            install_drafts(&mut app, keymap.clone(), KeyBindingOverrides::default());
            let msg = app.transient.as_ref().expect("flash").text.clone();
            assert!(msg.contains(crate::config::NOT_PERSISTED_NOTE), "{msg}");
            assert!(msg.starts_with("Keybindings applied"), "{msg}");
        }

        install_drafts(&mut app, keymap, KeyBindingOverrides::default());
        assert_eq!(
            app.transient.as_ref().expect("flash").text,
            "Keybindings saved"
        );
    }
}
