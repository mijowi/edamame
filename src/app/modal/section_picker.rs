//! "Go to section" modal — adapter that wraps
//! [`crate::ui::SectionPickerState`].  Routes
//! [`SectionPickerResponse`] outcomes to App-level actions:
//!
//! - `Continue` → leave the modal open
//! - `Preview` → arm a debounced viewport scroll on `App` (the run
//!   loop's [`App::tick_section_jump`] applies it once the user stops
//!   navigating)
//! - `Cancelled` → close the modal AND restore `editor.scroll` to the
//!   value captured at open time, clearing any pending debounce
//! - `Selected` → close the modal, apply the target scroll
//!   synchronously (overriding any pending debounce), and move the
//!   cursor to the end of the heading's buffer line
//!
//! Behaviour matches the theme picker (the closest existing live-
//! preview + revert-on-cancel pattern): the picker mutates live state
//! while open, and Esc rewinds.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{HeadingEntry, SectionPickerResponse, SectionPickerState, SectionPickerView};

pub struct SectionPickerModal {
    state: SectionPickerState,
    /// `editor.scroll` at the moment the modal opened.  Restored on
    /// `Cancelled` so a fumbled preview reverts cleanly.
    original_scroll: usize,
}

impl SectionPickerModal {
    pub fn new(entries: Vec<HeadingEntry>, focused: usize, original_scroll: usize) -> Self {
        Self {
            state: SectionPickerState::open(entries, focused),
            original_scroll,
        }
    }
}

impl Modal for SectionPickerModal {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = SectionPickerView {
            theme: ctx.theme,
            cursor_visible: ctx.cursor_visible,
        };
        // Trim the bottom region (hint line + status bar) from the area
        // so the picker — which grows to fill the height it's given —
        // never paints over it.
        let bottom_rows = crate::ui::BottomRegion::height();
        let area = Rect {
            height: area.height.saturating_sub(bottom_rows),
            ..area
        };
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        match self.state.handle_key(&key) {
            SectionPickerResponse::Continue => ModalOutcome::Continue,
            SectionPickerResponse::Preview { target_scroll } => {
                ModalOutcome::ContinueAnd(Box::new(move |app| {
                    app.arm_section_jump(target_scroll);
                }))
            }
            SectionPickerResponse::Cancelled => {
                let original = self.original_scroll;
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    app.cancel_section_jump(original);
                }))
            }
            SectionPickerResponse::Selected {
                buffer_line,
                target_scroll,
            } => ModalOutcome::CloseAnd(Box::new(move |app| {
                app.commit_section_jump(buffer_line, target_scroll);
            })),
        }
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        // Paste only grows the filter, so it can yield `Continue` or a
        // live `Preview`; reuse the keyboard `Preview` arm's scroll arm.
        if let SectionPickerResponse::Preview { target_scroll } = self.state.paste(text) {
            return ModalOutcome::ContinueAnd(Box::new(move |app| {
                app.arm_section_jump(target_scroll);
            }));
        }
        ModalOutcome::Continue
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_state.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        // `close_if_esc_clicked` returns a plain `Close` outcome with no
        // callback, which would drop the modal without restoring the
        // live-preview scroll — and any pending debounce would still
        // fire after the modal is gone.  Route the click through the
        // same cancel callback the keyboard Esc path uses.
        if super::types::esc_rect_hit(self.state.esc_button_rect, col, row) {
            let original = self.original_scroll;
            return ModalOutcome::CloseAnd(Box::new(move |app| {
                app.cancel_section_jump(original);
            }));
        }
        // Clicks on a populated heading row commit the jump immediately
        // — same outcome as pressing Enter on that row.
        match self.state.handle_click(col, row) {
            SectionPickerResponse::Selected {
                buffer_line,
                target_scroll,
            } => ModalOutcome::CloseAnd(Box::new(move |app| {
                app.commit_section_jump(buffer_line, target_scroll);
            })),
            _ => ModalOutcome::Continue,
        }
    }

    fn as_any(&self) -> &dyn Any {
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
