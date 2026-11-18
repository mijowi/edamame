//! "Go to section" modal over a
//! [`SearchableList<HeadingEntry>`](crate::ui::searchable_list::SearchableList).  The
//! picker live-previews by scrolling the editor as focus moves (debounced through
//! [`App::tick_section_jump`]); Esc rewinds to the scroll captured at open time and
//! Submit commits synchronously.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::searchable_list::{ListEvent, SearchableList};
use crate::ui::section_picker::{build_section_list, render_section_picker};
use crate::ui::HeadingEntry;

pub struct SectionPickerModal {
    list: SearchableList<HeadingEntry>,
    /// `editor.scroll` at open time, restored on cancel.
    original_scroll: usize,
    esc_button_rect: Option<Rect>,
}

impl SectionPickerModal {
    pub fn new(entries: Vec<HeadingEntry>, focused: usize, original_scroll: usize) -> Self {
        Self {
            list: build_section_list(entries, focused),
            original_scroll,
            esc_button_rect: None,
        }
    }

    /// Shared by the keyboard and paste paths.
    fn outcome_for(&self, event: ListEvent) -> ModalOutcome {
        match event {
            ListEvent::Continue => ModalOutcome::Continue,
            ListEvent::FocusChanged(i) => {
                let target = self.list.items()[i].target_scroll;
                ModalOutcome::ContinueAnd(Box::new(move |app| app.arm_section_jump(target)))
            }
            ListEvent::Cancelled => {
                let original = self.original_scroll;
                ModalOutcome::CloseAnd(Box::new(move |app| app.cancel_section_jump(original)))
            }
            ListEvent::Submitted(i) => {
                let entry = &self.list.items()[i];
                let (buffer_line, target_scroll) = (entry.buffer_line, entry.target_scroll);
                let original_scroll = self.original_scroll;
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    app.cancel_section_jump(original_scroll);
                    app.commit_section_jump(buffer_line, target_scroll)
                }))
            }
        }
    }
}

impl Modal for SectionPickerModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        // The picker fills the height it's given; keep it off the hint line + status bar.
        let bottom_rows = crate::ui::BottomRegion::height();
        let area = Rect {
            height: area.height.saturating_sub(bottom_rows),
            ..area
        };
        self.esc_button_rect = render_section_picker(
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
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        let event = self.list.handle_key(&key);
        self.outcome_for(event)
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        let event = self.list.paste(text);
        self.outcome_for(event)
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.list.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        // A plain `Close` would leave the preview scroll in place and a pending debounce
        // live; route through the same cancel callback as keyboard Esc.
        if super::types::esc_rect_hit(self.esc_button_rect, col, row) {
            let original = self.original_scroll;
            return ModalOutcome::CloseAnd(Box::new(move |app| app.cancel_section_jump(original)));
        }
        match self.list.handle_click(col, row) {
            ListEvent::Submitted(i) => {
                let entry = &self.list.items()[i];
                let (buffer_line, target_scroll) = (entry.buffer_line, entry.target_scroll);
                let original_scroll = self.original_scroll;
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    app.cancel_section_jump(original_scroll);
                    app.commit_section_jump(buffer_line, target_scroll)
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
    use crate::app::test_utils::make_app;

    #[test]
    fn opening_a_picker_pushes_the_modal() {
        let mut app = make_app();
        app.open_section_picker(80);
        assert!(app.modal_stack.contains::<SectionPickerModal>());
    }

    #[test]
    fn picker_commit_records_origin_before_live_preview_for_back_and_forward() {
        use crossterm::event::{KeyCode, KeyModifiers};

        use crate::app::test_utils::app_with_buffer;

        let mut app = app_with_buffer(&format!("# First\n\n{}## Last\n", "body\n\n".repeat(30)), 3);
        app.editor.scroll = 1;
        let mut picker = SectionPickerModal::new(vec![], 0, 1);
        // Use the actual shared index populated by Ctrl-G.
        app.open_section_picker(80);
        std::mem::swap(
            &mut picker,
            app.modal_stack
                .top_mut()
                .unwrap()
                .as_any_mut()
                .downcast_mut::<SectionPickerModal>()
                .unwrap(),
        );
        let outcome = picker.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &mut app,
            10,
            80,
        );
        if let ModalOutcome::ContinueAnd(callback) = outcome {
            callback(&mut app);
        }
        app.editor.scroll = 40; // A completed debounced preview must not become the origin.
        let outcome = picker.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut app,
            10,
            80,
        );
        if let ModalOutcome::CloseAnd(callback) = outcome {
            callback(&mut app);
        }
        assert_eq!(app.nav_back.len(), 1);
        let target = app.editor.cursor.offset;
        app.navigate_back(10, 80);
        assert_eq!(app.editor.cursor.offset, 3);
        assert_eq!(app.editor.scroll, 1);
        app.navigate_forward(10, 80);
        assert_eq!(app.editor.cursor.offset, target);
    }
}
