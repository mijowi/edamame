//! Insert Table modal.  Adapter wrapping [`crate::ui::InsertTableState`].
//!
//! On Insert the blank-line precondition is re-verified defensively
//! (the App already checks it before opening the modal, but the user
//! may have moved the cursor in the meantime).  Insertion goes
//! through [`crate::editor::edit_ops::insert_table_at_cursor`] —
//! same path the keymap binding uses.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::{App, MessageKind};
use crate::editor::edit_ops;
use crate::ui::{InsertTableResponse, InsertTableState, InsertTableView};

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
                if !crate::editor::table_edit::cursor_line_is_blank(&source, cursor_byte) {
                    return ModalOutcome::CloseAnd(Box::new(|app| {
                        app.flash("Insert Table requires a blank line", MessageKind::Warning);
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

    fn as_any(&self) -> &dyn Any {
        self
    }
}
