//! Insert Table modal.  Adapter wrapping [`crate::ui::InsertTableState`].
//!
//! The location precondition (a blank line or an empty list item) is re-verified on Insert: the
//! App checks it before opening the modal, but the cursor may have moved since.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::{App, MessageKind};
use crate::editor::edit_ops;
use crate::ui::{InsertTableResponse, InsertTableState, InsertTableView, ModalKind};

pub struct InsertTableModal {
    state: InsertTableState,
}

impl InsertTableModal {
    pub fn new() -> Self {
        Self {
            state: InsertTableState::new(),
        }
    }
}

impl Default for InsertTableModal {
    fn default() -> Self {
        Self::new()
    }
}

impl Modal for InsertTableModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = InsertTableView {
            theme: ctx.theme,
            cursor_visible: ctx.cursor_visible,
        };
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        app: &mut App,
        doc_height: usize,
        doc_width: usize,
    ) -> ModalOutcome {
        match self.state.handle_key(&key) {
            InsertTableResponse::Continue => ModalOutcome::Continue,
            InsertTableResponse::Cancelled => ModalOutcome::Close,
            InsertTableResponse::Insert { rows, cols } => {
                let source = app.editor.buffer.contents();
                let cursor_byte = app
                    .editor
                    .buffer
                    .rope()
                    .char_to_byte(app.editor.cursor.offset);
                if !crate::editor::table_edit::can_insert_table(&source, cursor_byte) {
                    return ModalOutcome::CloseAnd(Box::new(|app| {
                        app.notify(
                            "Insert Table requires a blank line or an empty list item",
                            ModalKind::Warning,
                        );
                    }));
                }
                edit_ops::insert_table_at_cursor(
                    &mut app.editor,
                    rows,
                    cols,
                    doc_height,
                    doc_width,
                );
                ModalOutcome::CloseAnd(Box::new(|app| {
                    app.flash("Table inserted", MessageKind::Success);
                }))
            }
        }
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        self.state.paste(text);
        ModalOutcome::Continue
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
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
    //! The App-level Insert Table flow: pre-flight location guard, modal lifecycle, and the
    //! resulting buffer + cursor state.

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::modal::NoticeModal;
    use crate::app::test_utils::app_with_buffer;
    use crate::config::Action;
    use crate::editor::Mode;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn insert_table_on_blank_line_yields_gfm_table_with_cursor_in_first_header_cell() {
        let src = "para one\n\npara two\n";
        // Cursor on the blank line between the two paragraphs (byte 9).
        let mut app = app_with_buffer(src, 9);
        let handled = app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(handled, "InsertTable should be handled at the App layer");
        assert!(
            app.modal_stack.contains::<InsertTableModal>(),
            "the rows/columns modal must be open after the pre-flight passes"
        );
        // Defaults are rows=2, cols=3; Tab to the Insert button and press Enter.
        app.dispatch_modal_key(key(KeyCode::Tab), 40, 80); // Rows → Cols
        app.dispatch_modal_key(key(KeyCode::Tab), 40, 80); // Cols → Insert
        app.dispatch_modal_key(key(KeyCode::Enter), 40, 80);

        assert!(
            !app.modal_stack.contains::<InsertTableModal>(),
            "modal closes on insert"
        );
        let post = app.editor.buffer.contents();
        assert_eq!(
            post,
            "para one\n\
             \n\
             |   |   |   |\n\
             | --- | --- | --- |\n\
             |   |   |   |\n\
             |   |   |   |\n\
             \n\
             para two\n",
            "buffer mismatch:\n{post}"
        );

        // The cursor should sit on the middle space of the first header cell's `|  `.
        let cursor_byte = app
            .editor
            .buffer
            .rope()
            .char_to_byte(app.editor.cursor.offset);
        assert!(
            post[cursor_byte.saturating_sub(2)..cursor_byte + 2].starts_with("|  "),
            "cursor should land in first header cell (byte {cursor_byte}); around: {:?}",
            &post[cursor_byte.saturating_sub(2)..(cursor_byte + 2).min(post.len())]
        );
        assert!(
            matches!(
                app.transient.as_ref().map(|t| t.kind),
                Some(MessageKind::Success)
            ),
            "expected success flash, got {:?}",
            app.transient.as_ref().map(|t| t.kind)
        );
    }

    #[test]
    fn insert_table_in_mid_paragraph_warns_and_leaves_buffer_untouched() {
        let src = "this is a paragraph\nwith two lines\n";
        // Cursor mid-line.
        let mut app = app_with_buffer(src, 5);
        let before = app.editor.buffer.contents();
        let handled = app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(handled);
        assert!(
            !app.modal_stack.contains::<InsertTableModal>(),
            "modal should NOT open on a non-blank line"
        );
        assert_eq!(app.editor.buffer.contents(), before, "buffer unchanged");
        assert!(
            app.modal_stack.contains::<NoticeModal>(),
            "location guard must push a NoticeModal"
        );
    }

    #[test]
    fn insert_table_on_heading_warns() {
        let src = "# Heading\n";
        let mut app = app_with_buffer(src, 4);
        app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(!app.modal_stack.contains::<InsertTableModal>());
        assert!(app.modal_stack.contains::<NoticeModal>());
    }

    #[test]
    fn insert_table_on_list_item_warns() {
        let src = "- one\n- two\n";
        let mut app = app_with_buffer(src, 2);
        app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(!app.modal_stack.contains::<InsertTableModal>());
        assert!(app.modal_stack.contains::<NoticeModal>());
    }

    /// On an empty item the table opens on the marker line, inside the item.
    #[test]
    fn insert_table_on_an_empty_list_item_fills_the_item() {
        let src = "- one\n- \n";
        let mut app = app_with_buffer(src, src.len() - 1);
        app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(app.modal_stack.contains::<InsertTableModal>());
        app.dispatch_modal_key(key(KeyCode::Enter), 40, 80);
        let post = app.editor.buffer.contents();
        assert!(
            post.starts_with("- one\n- |   |   |   |\n  | --- | --- | --- |\n  |   |"),
            "got:\n{post}"
        );
        let cursor = app
            .editor
            .buffer
            .rope()
            .char_to_byte(app.editor.cursor.offset);
        assert_eq!(
            cursor,
            "- one\n- | ".len(),
            "cursor in the first header cell"
        );
    }

    #[test]
    fn insert_table_on_existing_table_row_warns() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let mut app = app_with_buffer(src, 4); // mid-header
        app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(!app.modal_stack.contains::<InsertTableModal>());
        assert!(app.modal_stack.contains::<NoticeModal>());
    }

    #[test]
    fn insert_table_at_eof_without_trailing_newline_warns_then_succeeds_after_enter() {
        let src = "no trailing newline";
        let mut app = app_with_buffer(src, src.len());
        // Rendered mode, so `Action::Newline` doesn't bounce the cursor via the scroll-sync.
        app.editor.mode = Mode::Rendered;
        app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(
            !app.modal_stack.contains::<InsertTableModal>(),
            "modal should NOT open at EOF on a non-blank final line"
        );
        assert!(
            app.modal_stack.contains::<NoticeModal>(),
            "location guard must push a NoticeModal"
        );
        app.dispatch_modal_key(key(KeyCode::Esc), 40, 80);

        // The newline moves the cursor onto a fresh empty trailing line, so pre-flight passes.
        crate::editor::edit_ops::apply(&mut app.editor, Action::Newline, 40, 80);
        app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(
            app.modal_stack.contains::<InsertTableModal>(),
            "modal should open after a newline made the cursor line blank"
        );
        app.dispatch_modal_key(key(KeyCode::Enter), 40, 80);
        let post = app.editor.buffer.contents();
        assert!(
            post.contains("| --- | --- | --- |"),
            "buffer should contain the alignment row, got:\n{post}"
        );
    }

    #[test]
    fn insert_table_modal_cancel_button_does_not_modify_buffer() {
        let src = "para one\n\npara two\n";
        let mut app = app_with_buffer(src, 9);
        let before = app.editor.buffer.contents();
        app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(app.modal_stack.contains::<InsertTableModal>());
        app.dispatch_modal_key(key(KeyCode::Esc), 40, 80);
        assert!(!app.modal_stack.contains::<InsertTableModal>());
        assert_eq!(app.editor.buffer.contents(), before);
    }
}
