//! Keybinds overlay.  Adapter wrapping
//! [`crate::ui::KeybindsState`].
//!
//! Rebinds mutate the live [`crate::config::KeyMap`] in place (so the
//! next keystroke uses the new binding) AND
//! [`crate::config::KeyBindingOverrides`] (so the override persists to
//! `keybindings.toml`).  Both are owned by `App`; the overlay needs
//! `&mut` access to both for [`crate::ui::KeybindsState::handle_key`].

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::{App, MessageKind};
use crate::config::{Config, KeyMap};
use crate::ui::{KeybindsResponse, KeybindsState, KeybindsView};

pub struct KeybindsOverlayModal {
    state: KeybindsState,
}

impl KeybindsOverlayModal {
    pub fn new(keymap: &KeyMap) -> Self {
        Self {
            state: KeybindsState::open(keymap),
        }
    }
}

impl Modal for KeybindsOverlayModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        if let Some(km) = ctx.keymap {
            let view = KeybindsView {
                theme: ctx.theme,
                keymap: km,
                cursor_visible: ctx.cursor_visible,
            };
            frame.render_stateful_widget(view, area, &mut self.state);
        }
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        // The overlay needs `&mut KeyMap` and `&mut KeyBindingOverrides`;
        // ensure both exist on `app` first so the borrow stays simple.
        if app.keymap.is_none() {
            match KeyMap::build(&app.keybindings) {
                Ok(km) => app.keymap = Some(km),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to build KeyMap for keybinds overlay");
                    return ModalOutcome::Close;
                }
            }
        }
        let response = {
            let keymap = app.keymap.as_mut().expect("keymap built above");
            self.state.handle_key(&key, keymap, &mut app.keybindings)
        };
        match response {
            KeybindsResponse::Continue => ModalOutcome::Continue,
            KeybindsResponse::Cancelled => ModalOutcome::Close,
            KeybindsResponse::Rebound { action, key } => {
                if let Some(dir) = Config::config_dir() {
                    let path = dir.join("keybindings.toml");
                    if let Err(e) = app.keybindings.save_to(&path) {
                        tracing::warn!(error = %e, "failed to write keybindings.toml");
                        app.flash(format!("Save failed: {e}"), MessageKind::Error);
                    } else {
                        app.flash(format!("Bound {action} to {key}"), MessageKind::Success);
                    }
                } else {
                    app.flash("No config directory available", MessageKind::Error);
                }
                ModalOutcome::Continue
            }
            KeybindsResponse::Conflict {
                key,
                existing_action,
            } => {
                app.flash(
                    format!("'{key}' is already bound to {existing_action}"),
                    MessageKind::Error,
                );
                ModalOutcome::Continue
            }
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_state.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        super::types::close_if_esc_clicked(self.state.esc_button_rect, col, row)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    //! Phase 10 review collapsed the read-only `ShowCheatSheet` popover
    //! into the editable `OpenKeybinds` overlay.  Both actions must
    //! produce the same overlay state so users with custom keybinds for
    //! the legacy action still get the unified flow.

    use super::*;
    use crate::app::test_utils::make_app;
    use crate::config::Action;

    #[test]
    fn show_cheat_sheet_action_opens_combined_keybinds_overlay() {
        let mut app = make_app();
        let handled = app.handle_app_action(&Action::ShowCheatSheet, 40, 80);
        assert!(handled);
        assert!(app.modal_stack.contains::<KeybindsOverlayModal>());
    }
}
