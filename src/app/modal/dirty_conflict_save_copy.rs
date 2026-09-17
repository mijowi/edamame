//! `[Save a copy]` step of [`super::DirtyConflictModal`]: the [`SaveCopyState`] path entry,
//! then save the buffer aside and reload the carried on-disk contents (already read by the
//! watcher, so no re-read race).

use std::any::Any;
use std::path::Path;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::dirty_conflict::DirtyConflictModal;
use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::flash::MessageKind;
use crate::app::App;
use crate::ui::{SaveCopyResponse, SaveCopyState, SaveCopyView};

pub struct DirtyConflictSaveCopyModal {
    state: SaveCopyState,
    on_disk_contents: String,
}

impl DirtyConflictSaveCopyModal {
    pub fn new(default_path: String, on_disk_contents: String) -> Self {
        Self {
            state: SaveCopyState::new(default_path),
            on_disk_contents,
        }
    }

    /// Refresh the carried contents when another external write lands while the modal is
    /// open, so the eventual reload uses the current disk state.
    pub fn set_on_disk_contents(&mut self, contents: String) {
        self.on_disk_contents = contents;
    }
}

impl Modal for DirtyConflictSaveCopyModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = SaveCopyView {
            theme: ctx.theme,
            cursor_visible: ctx.cursor_visible,
            title: "Save a Copy",
            note: None,
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
        match self.state.handle_key(&key) {
            SaveCopyResponse::Continue => ModalOutcome::Continue,
            SaveCopyResponse::Cancelled => ModalOutcome::Close,
            SaveCopyResponse::Save(path_str) => {
                let path = Path::new(&path_str).to_owned();
                match app.editor.buffer.save_copy(&path) {
                    Ok(()) => {
                        let contents = std::mem::take(&mut self.on_disk_contents);
                        let display = path_str.clone();
                        ModalOutcome::CloseAnd(Box::new(move |app| {
                            app.modal_stack.remove_first::<DirtyConflictModal>();
                            app.flash(format!("Buffer saved to {display}"), MessageKind::Success);
                            app.reload_buffer_from_disk(contents);
                        }))
                    }
                    Err(e) => {
                        self.state.last_error = Some(format!("{e}"));
                        ModalOutcome::Continue
                    }
                }
            }
        }
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
