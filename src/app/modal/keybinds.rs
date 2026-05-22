//! Keybinds overlay.  Adapter wrapping
//! [`crate::ui::KeybindsState`].
//!
//! Edits are buffered inside the overlay's draft keymap / overrides
//! and only applied to the live [`crate::config::KeyMap`] and
//! [`crate::config::KeyBindingOverrides`] when the user activates
//! `[ Save ]`.  Esc and `[ Cancel ]` discard the draft.  Persistence
//! to `keybindings.toml` happens on Save and only on Save.
//!
//! Save failures (missing config dir, disk write errors) surface as a
//! warning [`crate::ui::ModalKind::Warning`] notice and keep the
//! overlay open with the user's drafts intact, so they can retry
//! without losing work.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::{App, MessageKind};
use crate::config::{Config, KeyBindingOverrides, KeyMap};
use crate::ui::{KeybindsResponse, KeybindsState, KeybindsView, ModalKind};

pub struct KeybindsOverlayModal {
    state: KeybindsState,
}

impl KeybindsOverlayModal {
    pub fn new(keymap: &KeyMap, overrides: &KeyBindingOverrides) -> Self {
        Self {
            state: KeybindsState::open(keymap, overrides),
        }
    }
}

/// Try to persist the draft overrides to `keybindings.toml`.  Returns
/// `Err(message)` on missing config dir or write failure; the caller
/// surfaces the message via [`App::notify`] and keeps the overlay
/// open so the user can retry without losing their drafts.
fn try_persist(overrides: &KeyBindingOverrides) -> Result<(), String> {
    let Some(dir) = Config::config_dir() else {
        return Err("No config directory available — keybindings not saved".into());
    };
    let path = dir.join("keybindings.toml");
    overrides.save_to(&path).map_err(|e| {
        tracing::warn!(error = %e, "failed to write keybindings.toml");
        format!("Failed to save keybindings: {e}")
    })
}

/// Swap the drafts onto `app` after a successful persist.  Only the
/// in-memory state is touched here; disk has already been written.
fn install_drafts(app: &mut App, keymap: KeyMap, overrides: KeyBindingOverrides) {
    app.keymap = Some(keymap);
    app.keybindings = overrides;
    app.flash("Keybindings saved", MessageKind::Success);
}

/// Map a `KeybindsResponse::Save` to a `ModalOutcome` that either
/// closes-with-install on success or stays-open-with-warning on
/// failure.  `on_failure_outcome` controls how the warning is
/// delivered: from `handle_key` we have `&mut App` directly so the
/// notify is inline and we return `Continue`; from `handle_click` we
/// don't, so we return `ContinueAnd(notify)`.
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

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
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
}
