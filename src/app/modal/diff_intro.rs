//! Explanatory modal shown the first time the user enters diff-review
//! mode for this install.  Sits on top of [`crate::ui::ModalView`] but
//! carries a tiny additional bit of state — a `dont_show_again`
//! toggle the user flips with Space — so it can persist the opt-out
//! to `config.editor.show_diff_intro` on `[Continue]`.

use std::any::Any;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse, ModalState, ModalView};

pub struct DiffIntroModal {
    state: ModalState,
    buttons: Vec<ModalButton>,
    /// Live state of the "Don't show this again" toggle.  `false`
    /// means the modal will fire again on the next diff entry;
    /// `true` writes `config.editor.show_diff_intro = false` on
    /// confirm.
    dont_show_again: bool,
    kind: ModalKind,
    dismissable: bool,
}

impl DiffIntroModal {
    pub fn new() -> Self {
        Self {
            state: ModalState::new(),
            buttons: vec![ModalButton::new("Continue")],
            dont_show_again: false,
            kind: ModalKind::Normal,
            dismissable: true,
        }
    }

    fn body(&self) -> Vec<Line<'static>> {
        let checkbox = if self.dont_show_again { "[x]" } else { "[ ]" };
        vec![
            Line::raw("The file on disk has changed.  Review the differences below:"),
            Line::raw(""),
            Line::raw("  - Deleted lines appear above, added lines below."),
            Line::raw("  - The focused hunk is marked with > in the gutter."),
            Line::raw(""),
            Line::raw("Keys:"),
            Line::raw("  Tab / Shift-Tab   Next / previous hunk"),
            Line::raw("  y / n             Accept / reject hunk"),
            Line::raw("  Y / N             Accept / reject all"),
            Line::raw("  Esc               Exit diff mode"),
            Line::raw(""),
            Line::raw(format!(
                "  {checkbox}  Don't show this again (Space to toggle)"
            )),
        ]
    }
}

impl Default for DiffIntroModal {
    fn default() -> Self {
        Self::new()
    }
}

impl Modal for DiffIntroModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let body = self.body();
        let view = ModalView::new(
            "File changed on disk",
            &body,
            &self.buttons,
            ctx.theme,
            self.kind,
            self.dismissable,
        );
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        // Space toggles the checkbox.  Intercept before delegating to
        // ModalState so it doesn't try to interpret Space as a button
        // activation.
        if key.code == KeyCode::Char(' ') && key.modifiers == KeyModifiers::NONE {
            self.dont_show_again = !self.dont_show_again;
            return ModalOutcome::Continue;
        }
        match self
            .state
            .handle_key(&key, self.buttons.len(), self.dismissable)
        {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => self.close_outcome(),
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        // Route the esc-button click through `close_outcome()` so the
        // `dont_show_again` checkbox is persisted just like Esc /
        // [Continue].  Otherwise the user could tick the box, click
        // the [×], and see the modal again on the next diff entry.
        if super::types::esc_rect_hit(self.state.esc_button_rect, col, row) {
            self.close_outcome()
        } else {
            ModalOutcome::Continue
        }
    }

    fn kind(&self) -> ModalKind {
        self.kind
    }

    fn dismissable(&self) -> bool {
        self.dismissable
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl DiffIntroModal {
    fn close_outcome(&self) -> ModalOutcome {
        let persist_off = self.dont_show_again;
        ModalOutcome::CloseAnd(Box::new(move |app| {
            if persist_off && app.config.editor.show_diff_intro {
                app.config.editor.show_diff_intro = false;
                app.save_config_with_flash("failed to persist diff intro opt-out");
            }
        }))
    }
}
