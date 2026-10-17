//! Global editing chords use their ordinary meaning and the Helix replacement guard.
use crate::config::Action;
use crate::editor::{edit_ops, helix_ops, list_edit, table_edit_ops, Mode};
use crate::input::HelixSubMode;

use super::modal::save_as;
use super::{App, MessageKind};

impl App {
    /// Quit is a continuation of a successful write, including asynchronous Save As confirmation.
    pub(super) fn save_from_helix(&mut self, then_quit: bool) {
        if self.editor.readonly || self.editor.mode == Mode::Diff || self.search_flow_captures() {
            return;
        }
        if self.editor.buffer.path().is_none() {
            let after: Option<save_as::AfterSave> = then_quit
                .then(|| Box::new(|app: &mut App| app.should_quit = true) as save_as::AfterSave);
            self.open_save_as_modal(after);
        } else {
            match self.save_buffer() {
                Ok(()) => {
                    self.flash("Saved", MessageKind::Success);
                    if then_quit {
                        self.should_quit = true;
                    }
                }
                Err(error) => self.flash(format!("Save failed: {error}"), MessageKind::Info),
            }
        }
    }

    pub(super) fn dispatch_helix_range_action(
        &mut self,
        action: &Action,
        h: usize,
        w: usize,
    ) -> bool {
        if self.editor.mode == Mode::Diff
            || !self
                .modal_input
                .helix()
                .is_some_and(|s| s.sub_mode != HelixSubMode::Insert)
        {
            return false;
        }
        if matches!(action, Action::InsertTab) && self.editor.mode == Mode::Rendered {
            self.editor.flush_parsed_if_dirty();
            let byte = self
                .editor
                .buffer
                .rope()
                .char_to_byte(self.editor.cursor.offset);
            if table_edit_ops::locate_table(&self.editor, byte).is_some()
                || list_edit::find_list_at(&self.editor.contents(), byte).is_some()
            {
                return false;
            }
        }
        let marker = match action {
            Action::BoldSelection => Some("**"),
            Action::ItalicizeSelection => Some("*"),
            Action::InlineCodeSelection => Some("`"),
            Action::StrikethroughSelection => Some("~~"),
            Action::HighlightSelection => Some("=="),
            _ => None,
        };
        let mut inner_selection = None;
        let plan = if let Some(marker) = marker {
            if let Some((delta, inner)) = self
                .editor
                .selection
                .and_then(|s| edit_ops::wrap_plan(&self.editor.buffer, s, marker))
            {
                inner_selection = Some(inner);
                (
                    delta.offset,
                    delta.offset + delta.removed.chars().count(),
                    delta.inserted,
                )
            } else {
                return true;
            }
        } else if matches!(action, Action::InsertImage | Action::InsertLink) {
            let Some((delta, selection, _)) = edit_ops::placeholder_snippet_plan(
                &mut self.editor,
                matches!(action, Action::InsertImage),
                h,
            ) else {
                self.notify(
                    "Cannot insert an image or link inside this block",
                    crate::ui::ModalKind::Warning,
                );
                return true;
            };
            inner_selection = selection;
            (
                delta.offset,
                delta.offset + delta.removed.chars().count(),
                delta.inserted,
            )
        } else {
            let selection = self
                .editor
                .selection
                .map(|s| s.range())
                .unwrap_or((self.editor.cursor.offset, self.editor.cursor.offset));
            match action {
                Action::DeleteCharBack | Action::DeleteCharForward => {
                    (selection.0, selection.1, String::new())
                }
                Action::DeleteWordBack | Action::DeleteWordForward => {
                    let at = self.editor.cursor.offset;
                    let mut cursor = self.editor.cursor;
                    if matches!(action, Action::DeleteWordBack) {
                        cursor.move_word_left(&self.editor.buffer);
                    } else {
                        cursor.move_word_right(&self.editor.buffer);
                    }
                    (at.min(cursor.offset), at.max(cursor.offset), String::new())
                }
                Action::DeleteLine => {
                    let line = self.editor.buffer.char_to_line(self.editor.cursor.offset);
                    (
                        self.editor.buffer.line_to_char(line),
                        helix_ops::line::line_after(&self.editor, line),
                        String::new(),
                    )
                }
                Action::InsertChar(c) => (selection.0, selection.1, c.to_string()),
                Action::Newline => (selection.0, selection.1, "\n".to_owned()),
                Action::InsertTab => (
                    selection.0,
                    selection.1,
                    " ".repeat(crate::constants::INDENT_WIDTH),
                ),
                _ => return false,
            }
        };
        match helix_ops::actions::replace_range(&mut self.editor, plan.0, plan.1, &plan.2) {
            Ok(()) => {
                if let Some(inner) = inner_selection {
                    helix_ops::set_selection(&mut self.editor, inner);
                }
                let helix = self.modal_input.helix_mut().expect("active Helix");
                helix.sub_mode = HelixSubMode::Normal;
                helix.selection_linewise = false;
            }
            Err(message) => self.flash(message.to_string(), MessageKind::Info),
        }
        self.editor.ensure_cursor_visible(h, w);
        true
    }
}
