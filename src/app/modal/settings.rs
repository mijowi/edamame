//! Settings overlay.  Adapter wrapping
//! [`crate::ui::SettingsState`].
//!
//! Field changes drive [`crate::app::App::save_config_with_flash`];
//! the Theme row also drives [`crate::app::App::apply_active_theme`]
//! so the colour palette updates live.  The "Open config.toml in
//! external editor" row sets a deferred flag that the run loop
//! drains — the editor invocation needs `&mut Terminal` which only
//! the run loop owns.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::{App, MessageKind};
use crate::config::Config;
use crate::ui::{SettingsResponse, SettingsState, SettingsView};

pub struct SettingsOverlayModal {
    state: SettingsState,
}

impl SettingsOverlayModal {
    pub fn new() -> Self {
        Self {
            state: SettingsState::new(),
        }
    }
}

impl Default for SettingsOverlayModal {
    fn default() -> Self {
        Self::new()
    }
}

impl Modal for SettingsOverlayModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = SettingsView {
            theme: ctx.theme,
            config: ctx.config,
            cursor_visible: ctx.cursor_visible,
        };
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        let response = self.state.handle_key(&key, &mut app.config);
        match response {
            SettingsResponse::Continue => ModalOutcome::Continue,
            SettingsResponse::Cancelled => ModalOutcome::Close,
            SettingsResponse::OpenInExternalEditor => {
                // The actual editor invocation needs the live
                // `Terminal` handle, owned by the run loop.  Record
                // intent here and let the loop drain the flag at the
                // end of this iteration.
                ModalOutcome::CloseAnd(Box::new(|app| {
                    app.pending_open_config_in_editor = true;
                    app.needs_draw = true;
                }))
            }
            SettingsResponse::OpenConfigFolder => ModalOutcome::CloseAnd(Box::new(|app| {
                if let Some(dir) = Config::config_dir() {
                    app.spawn_open_worker(dir.display().to_string());
                } else {
                    app.flash("No config directory available", MessageKind::Error);
                }
                app.needs_draw = true;
            })),
            SettingsResponse::FieldChanged(label) => {
                app.save_config_with_flash("failed to persist settings overlay change");
                if label == "Theme" {
                    app.apply_active_theme();
                } else if label == "Big H1 headings" {
                    // Mirror the App-startup wiring: live-toggle the
                    // editor's big_h1 flag so the change takes effect
                    // on the next frame instead of waiting for a file
                    // reload.
                    app.editor.set_big_h1(app.config.editor.big_h1);
                }
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
    //! Settings overlay App-level wiring.  The "Open config.toml in
    //! default editor" row defers the actual editor invocation to the
    //! run loop so it can drive the terminal suspend/resume.

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::make_app;

    #[test]
    fn settings_overlay_open_external_sets_pending_flag_and_closes_overlay() {
        let mut app = make_app();
        app.open_settings_overlay();
        assert!(app.modal_stack.contains::<SettingsOverlayModal>());
        // Default focus is "Theme"; one Up skips the divider and lands
        // on the editor row.
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);
        assert!(app.pending_open_config_in_editor);
        assert!(!app.modal_stack.contains::<SettingsOverlayModal>());
    }

    #[test]
    fn settings_overlay_open_config_folder_closes_overlay() {
        // The top-row "Open config folder" entry hands the path to the
        // OS file manager via `spawn_open_worker` and closes the
        // overlay.  No `pending_open_config_in_editor` flag is set —
        // that path is editor-only.  Default focus is "Theme"; two Up
        // presses (skipping the divider) reach the folder row.
        let mut app = make_app();
        app.open_settings_overlay();
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);
        assert!(!app.pending_open_config_in_editor);
        assert!(!app.modal_stack.contains::<SettingsOverlayModal>());
    }
}
