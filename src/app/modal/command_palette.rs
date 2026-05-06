//! Fuzzy-searchable command palette.  Adapter that wraps
//! [`crate::ui::PaletteState`] so it can ride on the App's
//! [`super::ModalStack`].  Selecting a row dispatches the chosen
//! [`crate::config::Action`] back through
//! [`crate::app::App::dispatch_palette_action`] — same path as a
//! direct keystroke.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::KeyMap;
use crate::ui::{PaletteResponse, PaletteState, PaletteView};

pub struct CommandPaletteModal {
    state: PaletteState,
}

impl CommandPaletteModal {
    pub fn new(keymap: &KeyMap) -> Self {
        Self {
            state: PaletteState::open(keymap),
        }
    }
}

impl Modal for CommandPaletteModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = PaletteView {
            theme: ctx.theme,
            cursor_visible: ctx.cursor_visible,
        };
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        doc_height: usize,
        doc_width: usize,
    ) -> ModalOutcome {
        match self.state.handle_key(&key) {
            PaletteResponse::Continue => ModalOutcome::Continue,
            PaletteResponse::Cancelled => ModalOutcome::Close,
            PaletteResponse::Selected(action) => ModalOutcome::CloseAnd(Box::new(move |app| {
                app.dispatch_palette_action(action, doc_height, doc_width);
            })),
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_state.scroll_by(delta);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_utils::make_app;
    use crate::config::Action;

    #[test]
    fn open_command_palette_seeds_state() {
        let mut app = make_app();
        app.open_command_palette();
        assert!(app.modal_stack.contains::<CommandPaletteModal>());
    }

    #[test]
    fn dispatch_palette_action_save_round_trips() {
        // Driving `Action::Save` via the palette path produces the same
        // effect as a direct keystroke.  We assert that the editor's
        // dirty flag is consulted by the flash logic and that no panic
        // occurs when the buffer has no associated path (the save no-ops
        // via the typical save_file error path).
        let mut app = make_app();
        app.editor.dirty = false; // no-op save
        app.dispatch_palette_action(Action::Save, 40, 80);
        // No flash for a clean save (per `flash_for_action`).
        assert!(app.transient.is_none());
    }
}
