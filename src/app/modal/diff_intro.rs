//! Explanatory modal shown the first time the user enters diff-review
//! mode for this install.  Sits on top of [`crate::ui::ModalView`] but
//! carries a tiny additional bit of state — a `dont_show_again`
//! toggle — so it can persist the opt-out to
//! `config.editor.show_diff_intro` on `[Continue]`.
//!
//! The opt-out is a footer button (`[ [x] Don't show again ]`)
//! alongside `[ Continue ]`, so it joins the normal focus cycle:
//! Tab / Shift-Tab / Left / Right / Up / Down move focus between the
//! two, Enter / Space activate the focused one (toggle vs. continue),
//! and both are clickable.

use std::any::Any;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{esc_rect_hit, Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse, ModalState, ModalView};

/// Footer-button indices.  `[checkbox, Continue]`, Continue focused by
/// default so a bare Enter proceeds.
const CHECKBOX_IDX: usize = 0;
const CONTINUE_IDX: usize = 1;
const NUM_BUTTONS: usize = 2;

pub struct DiffIntroModal {
    state: ModalState,
    /// Live state of the "Don't show again" toggle.  `false` means the
    /// modal will fire again on the next diff entry; `true` writes
    /// `config.editor.show_diff_intro = false` on confirm.
    dont_show_again: bool,
    kind: ModalKind,
    dismissable: bool,
}

impl DiffIntroModal {
    pub fn new() -> Self {
        let mut state = ModalState::new();
        state.focused = CONTINUE_IDX;
        Self {
            state,
            dont_show_again: false,
            kind: ModalKind::Normal,
            dismissable: true,
        }
    }

    fn body(&self) -> Vec<Line<'static>> {
        vec![
            Line::raw("The file on disk has changed. edamame will now enter diff mode, in which you can review and accept or reject changes."),
            Line::raw(""),
            Line::raw("Deleted lines appear above in red and added lines below in green."),
            Line::raw("The focused hunk is marked with > in the gutter."),
            Line::raw(""),
            Line::raw("Keybindings:"),
            Line::raw("  Next / previous hunk:  Tab / Shift-Tab"),
            Line::raw("  Accept / reject hunk:  y / n"),
            Line::raw("  Accept / reject all:   Y / N"),
            Line::raw("  Exit diff mode:        Esc"),
        ]
    }

    /// Footer buttons, with the checkbox label reflecting the live
    /// toggle.  Rebuilt each render so the glyph tracks `dont_show_again`.
    fn buttons(&self) -> Vec<ModalButton> {
        let check = if self.dont_show_again { "[x]" } else { "[ ]" };
        vec![
            ModalButton::new(format!("{check} Don't show again")),
            ModalButton::new("Continue"),
        ]
    }

    /// Apply a footer-button activation; toggling the checkbox keeps
    /// the modal open, Continue closes it.
    fn activate(&mut self, idx: usize) -> ModalOutcome {
        if idx == CHECKBOX_IDX {
            self.dont_show_again = !self.dont_show_again;
            self.state.focused = CHECKBOX_IDX;
            ModalOutcome::Continue
        } else {
            self.close_outcome()
        }
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
        let buttons = self.buttons();
        let view = ModalView::new(
            "Entering diff mode",
            &body,
            &buttons,
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
        // Up / Down move focus between the two buttons.  Intercepted
        // before delegating because `ModalState` routes arrow keys to
        // body scrolling (a no-op here, since the body fits) and would
        // otherwise swallow them.
        if key.modifiers == KeyModifiers::NONE && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            let step = if key.code == KeyCode::Down {
                1
            } else {
                NUM_BUTTONS - 1
            };
            self.state.focused = (self.state.focused + step) % NUM_BUTTONS;
            return ModalOutcome::Continue;
        }
        // Tab / Shift-Tab / Left / Right cycle focus; Enter / Space / y
        // activate the focused button; Esc / n cancel.
        match self.state.handle_key(&key, NUM_BUTTONS, self.dismissable) {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::ButtonPressed(idx) => self.activate(idx),
            ModalResponse::Cancelled => self.close_outcome(),
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        // Scroll the body like every other `ModalView` modal.  The
        // trait default is a no-op, so without this override the wheel
        // does nothing — and this modal also repurposes Up / Down for
        // button focus, so the wheel is the body's only scroll path on
        // a terminal too short to show it all at once.
        self.state.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        // Footer buttons first, then the esc close hint.  Routing the
        // esc-hint click through `close_outcome()` persists the
        // `dont_show_again` toggle just like Esc / [Continue].
        if let Some(rect) = self.state.button_rects.get(CHECKBOX_IDX).copied() {
            if esc_rect_hit(Some(rect), col, row) {
                return self.activate(CHECKBOX_IDX);
            }
        }
        if let Some(rect) = self.state.button_rects.get(CONTINUE_IDX).copied() {
            if esc_rect_hit(Some(rect), col, row) {
                return self.activate(CONTINUE_IDX);
            }
        }
        if esc_rect_hit(self.state.esc_button_rect, col, row) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_utils::make_app;
    use crate::config::{Config, Theme};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Render the modal once into an off-screen backend so
    /// `state.button_rects` / `esc_button_rect` get populated for the
    /// click tests.
    fn render_offscreen(modal: &mut DiffIntroModal) {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let config = Config::default();
        let mut terminal = Terminal::new(TestBackend::new(70, 20)).unwrap();
        terminal
            .draw(|frame| {
                let ctx = ModalRenderCtx {
                    theme,
                    config: &config,
                    cursor_visible: false,
                };
                let area = frame.area();
                modal.render(frame, area, &ctx);
            })
            .unwrap();
    }

    #[test]
    fn focus_starts_on_continue() {
        assert_eq!(DiffIntroModal::new().state.focused, CONTINUE_IDX);
    }

    #[test]
    fn arrows_and_tab_move_focus_between_buttons() {
        let mut app = make_app();
        let mut modal = DiffIntroModal::new();
        // Down: Continue → checkbox.
        modal.handle_key(key(KeyCode::Down), &mut app, 40, 80);
        assert_eq!(modal.state.focused, CHECKBOX_IDX);
        // Up: checkbox → Continue.
        modal.handle_key(key(KeyCode::Up), &mut app, 40, 80);
        assert_eq!(modal.state.focused, CONTINUE_IDX);
        // Tab cycles as well.
        modal.handle_key(key(KeyCode::Tab), &mut app, 40, 80);
        assert_eq!(modal.state.focused, CHECKBOX_IDX);
    }

    #[test]
    fn activating_checkbox_toggles_and_keeps_modal_open() {
        let mut app = make_app();
        let mut modal = DiffIntroModal::new();
        modal.handle_key(key(KeyCode::Down), &mut app, 40, 80); // focus checkbox
        assert!(!modal.dont_show_again);
        let out = modal.handle_key(key(KeyCode::Enter), &mut app, 40, 80);
        assert!(matches!(out, ModalOutcome::Continue));
        assert!(modal.dont_show_again);
        // Space toggles it back off without closing.
        let out = modal.handle_key(key(KeyCode::Char(' ')), &mut app, 40, 80);
        assert!(matches!(out, ModalOutcome::Continue));
        assert!(!modal.dont_show_again);
    }

    #[test]
    fn activating_continue_closes() {
        let mut app = make_app();
        let mut modal = DiffIntroModal::new(); // focused on Continue
        let out = modal.handle_key(key(KeyCode::Enter), &mut app, 40, 80);
        assert!(matches!(out, ModalOutcome::CloseAnd(_)));
    }

    #[test]
    fn clicking_checkbox_toggles_then_clicking_continue_closes() {
        let mut modal = DiffIntroModal::new();
        render_offscreen(&mut modal);
        let cb = modal.state.button_rects[CHECKBOX_IDX];
        let out = modal.handle_click(cb.x, cb.y);
        assert!(matches!(out, ModalOutcome::Continue));
        assert!(modal.dont_show_again, "checkbox click must toggle");

        // Re-render so rects reflect the new label width, then click
        // Continue to close.
        render_offscreen(&mut modal);
        let cont = modal.state.button_rects[CONTINUE_IDX];
        let out = modal.handle_click(cont.x, cont.y);
        assert!(matches!(out, ModalOutcome::CloseAnd(_)));
    }

    #[test]
    fn click_outside_buttons_keeps_modal_open() {
        let mut modal = DiffIntroModal::new();
        render_offscreen(&mut modal);
        let out = modal.handle_click(0, 0);
        assert!(matches!(out, ModalOutcome::Continue));
    }
}
