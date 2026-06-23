//! Fuzzy-searchable command palette.  Adapter that wraps
//! [`crate::ui::PaletteState`] so it can ride on the App's
//! [`super::ModalStack`].  Selecting a row dispatches the chosen
//! [`crate::config::Action`] back through
//! [`crate::app::App::dispatch_action`] — the unified dispatcher
//! shared with the run-loop keystroke path.

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
                app.dispatch_action(action, doc_height, doc_width);
            })),
        }
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        self.state.paste(text);
        ModalOutcome::Continue
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

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::flash::MessageKind;
    use crate::app::test_utils::make_app;
    use crate::config::Action;
    use crate::document::Buffer;

    #[test]
    fn open_command_palette_seeds_state() {
        let mut app = make_app();
        app.open_command_palette();
        assert!(app.modal_stack.contains::<CommandPaletteModal>());
    }

    #[test]
    fn paste_routes_into_the_open_palette_query() {
        // End-to-end: a bracketed paste while the palette is open must
        // reach `PaletteState::query` through `dispatch_modal_paste`,
        // flattened and length-capped by `sanitize_paste`.
        let mut app = make_app();
        app.open_command_palette();
        app.dispatch_modal_paste("sa\nve");
        let modal = app
            .modal_stack
            .find_first_mut::<CommandPaletteModal>()
            .expect("palette still open");
        assert_eq!(modal.state.query, "save");
    }

    #[test]
    fn dispatch_action_save_on_clean_buffer_is_silent() {
        // Driving `Action::Save` via the unified dispatcher with a
        // clean buffer is a no-op: nothing on disk, no flash, no
        // notice modal.
        let mut app = make_app();
        app.editor.dirty = false; // no-op save
        app.dispatch_action(Action::Save, 40, 80);
        assert!(app.transient.is_none());
    }

    #[test]
    fn dispatch_action_save_writes_to_disk_and_clears_dirty() {
        // Single source of truth for the unified save flow: the
        // keystroke arm (`dispatch_single_key`) and the palette modal
        // both funnel through `App::dispatch_action`, which routes
        // `Action::Save` to `handle_app_action` → `save_buffer` →
        // `Buffer::save_file`.  Verify the disk write happens, the
        // dirty flag clears, and the "Saved" flash fires exactly once.
        let mut app = make_app();
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        app.editor.buffer = Buffer::for_new_file(tmp.path());
        // Force a dirty edit so the save has something to flush and
        // the flash logic treats it as a meaningful save.
        let len = app.editor.buffer.len_chars();
        app.editor.buffer.insert_char(len, 'x');
        app.editor.dirty = true;

        app.dispatch_action(Action::Save, 40, 80);

        assert!(!app.editor.dirty, "save must clear the dirty flag");
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read back");
        assert!(on_disk.ends_with('x'), "buffer must reach the file");
        let msg = app.transient.as_ref().expect("Saved flash recorded");
        assert_eq!(msg.text, "Saved");
        assert!(matches!(msg.kind, MessageKind::Success));
    }

    #[test]
    fn dispatch_action_save_prompts_for_path_when_buffer_has_no_path() {
        // A never-saved buffer has no destination, so `Save` opens the
        // Save As path-entry modal instead of failing into a sticky
        // error.  The buffer stays dirty until the user supplies a path.
        use crate::app::modal::SaveAsModal;
        let mut app = make_app();
        assert!(app.editor.buffer.path().is_none());
        app.editor.dirty = true;
        app.dispatch_action(Action::Save, 40, 80);
        assert!(app.editor.dirty, "unsaved buffer must stay dirty");
        assert!(
            app.modal_stack.contains::<SaveAsModal>(),
            "path-less save must open the Save As modal"
        );
    }
}
