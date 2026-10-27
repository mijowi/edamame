//! Fuzzy-searchable command palette: an adapter putting a
//! [`SearchableList<PaletteEntry>`](crate::ui::searchable_list::SearchableList) on the App's
//! [`super::ModalStack`].  Selecting a row dispatches its [`crate::config::Action`] through
//! [`crate::app::App::dispatch_action`], the same dispatcher the keystroke path uses.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::{Action, KeyMap};
use crate::ui::command_palette::{build_palette_list, render_palette, PaletteEntry};
use crate::ui::searchable_list::{ListEvent, SearchableList};

pub struct CommandPaletteModal {
    list: SearchableList<PaletteEntry>,
    /// Cached `esc` close-hint rect, refreshed each render for click
    /// hit-testing.
    esc_button_rect: Option<Rect>,
}

impl CommandPaletteModal {
    /// `modal_enabled` hides `Exit to preview`; see [`build_palette_list`].
    pub fn new(keymap: &KeyMap, modal_enabled: bool) -> Self {
        Self {
            list: build_palette_list(keymap, modal_enabled),
            esc_button_rect: None,
        }
    }

    /// The close+dispatch outcome for a selected action.
    fn dispatch(action: Action, doc_height: usize, doc_width: usize) -> ModalOutcome {
        ModalOutcome::CloseAnd(Box::new(move |app| {
            app.dispatch_action(action, doc_height, doc_width);
        }))
    }
}

impl Modal for CommandPaletteModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.esc_button_rect = render_palette(
            &mut self.list,
            area,
            frame.buffer_mut(),
            ctx.theme,
            ctx.cursor_visible,
        );
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        doc_height: usize,
        doc_width: usize,
    ) -> ModalOutcome {
        match self.list.handle_key(&key) {
            ListEvent::Cancelled => ModalOutcome::Close,
            ListEvent::Submitted(i) => {
                let action = self.list.items()[i].action.clone();
                Self::dispatch(action, doc_height, doc_width)
            }
            // The palette has no live preview, so focus changes are inert.
            ListEvent::Continue | ListEvent::FocusChanged(_) => ModalOutcome::Continue,
        }
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        self.list.paste(text);
        ModalOutcome::Continue
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.list.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        if super::types::esc_rect_hit(self.esc_button_rect, col, row) {
            return ModalOutcome::Close;
        }
        match self.list.handle_click(col, row) {
            ListEvent::Submitted(i) => {
                let action = self.list.items()[i].action.clone();
                // `handle_click` has no doc dims; use the last-rendered ones.
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    app.dispatch_action(action, app.last_doc_height, app.last_doc_width);
                }))
            }
            _ => ModalOutcome::Continue,
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
        // End-to-end through `dispatch_modal_paste`, flattened by `sanitize_paste`.
        let mut app = make_app();
        app.open_command_palette();
        app.dispatch_modal_paste("sa\nve");
        let modal = app
            .modal_stack
            .find_first_mut::<CommandPaletteModal>()
            .expect("palette still open");
        assert_eq!(modal.list.query(), "save");
    }

    #[test]
    fn dispatch_action_save_on_clean_buffer_is_silent() {
        let mut app = make_app();
        app.editor.dirty = false; // no-op save
        app.dispatch_action(Action::Save, 40, 80);
        assert!(app.transient.is_none());
    }

    #[test]
    fn dispatch_action_save_writes_to_disk_and_clears_dirty() {
        // The keystroke arm and the palette both funnel through `App::dispatch_action` →
        // `handle_app_action` → `save_buffer`, so this covers the whole unified save flow.
        let mut app = make_app();
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        app.editor.buffer = Buffer::for_new_file(tmp.path());
        // A real edit, so the flash logic treats this as a meaningful save.
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
        // `Save` opens the Save As modal rather than failing into a sticky error.
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
