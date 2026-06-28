//! "Go to section" modal — adapter that drives a
//! [`SearchableList<HeadingEntry>`](crate::ui::searchable_list::SearchableList)
//! and maps its [`ListEvent`] outcomes to App-level actions:
//!
//! - `Continue` → leave the modal open
//! - `FocusChanged` → arm a debounced viewport scroll on `App` (the run
//!   loop's [`App::tick_section_jump`] applies it once the user stops
//!   navigating)
//! - `Cancelled` → close the modal AND restore `editor.scroll` to the value
//!   captured at open time, clearing any pending debounce
//! - `Submitted` → close the modal, apply the target scroll synchronously
//!   (overriding any pending debounce), and move the cursor to the heading's
//!   buffer line
//!
//! The picker mutates live state while open (live preview) and Esc rewinds.

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
    /// `editor.scroll` at the moment the modal opened.  Restored on
    /// `Cancelled` so a fumbled preview reverts cleanly.
    original_scroll: usize,
    /// Cached `esc` close-hint rect, refreshed each render for click
    /// hit-testing.
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

    /// Map a list event to the App-level outcome, sharing the arm/commit
    /// closures between the keyboard and paste paths.
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
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    app.commit_section_jump(buffer_line, target_scroll)
                }))
            }
        }
    }
}

impl Modal for SectionPickerModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        // Trim the bottom region (hint line + status bar) from the area so the
        // picker — which grows to fill the height it's given — never paints
        // over it.
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

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        // A plain `Close` would drop the modal without restoring the
        // live-preview scroll — and any pending debounce would still fire
        // after the modal is gone.  Route the esc click through the same
        // cancel callback the keyboard Esc path uses.
        if super::types::esc_rect_hit(self.esc_button_rect, col, row) {
            let original = self.original_scroll;
            return ModalOutcome::CloseAnd(Box::new(move |app| app.cancel_section_jump(original)));
        }
        // Clicks on a populated heading row commit the jump immediately.
        match self.list.handle_click(col, row) {
            ListEvent::Submitted(i) => {
                let entry = &self.list.items()[i];
                let (buffer_line, target_scroll) = (entry.buffer_line, entry.target_scroll);
                ModalOutcome::CloseAnd(Box::new(move |app| {
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
}
