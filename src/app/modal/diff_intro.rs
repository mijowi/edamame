//! Explanatory modal shown the first time the user enters diff-review
//! mode for this install.
//!
//! Adapter over [`crate::ui::DiffIntroView`] / [`crate::ui::DiffIntroState`]:
//! it supplies the keybindings body text and persists the opt-out to
//! `config.editor.show_diff_intro` on close.  The "Don't show this again"
//! opt-out is an on/off toggle pinned on its own row directly above the
//! centred `[ Continue ]` button; both join the focus cycle (Tab /
//! Shift-Tab / arrows move focus, Enter / Space activate, and both are
//! clickable).

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::Action;
use crate::input::diff_hint;
use crate::ui::{DiffIntroResponse, DiffIntroState, DiffIntroView};

pub struct DiffIntroModal {
    state: DiffIntroState,
}

impl DiffIntroModal {
    pub fn new() -> Self {
        Self {
            state: DiffIntroState::new(),
        }
    }

    fn body(&self) -> Vec<Line<'static>> {
        vec![
            Line::raw("The file on disk has changed. edamame will now enter diff mode, in which you can review and accept or reject changes."),
            Line::raw(""),
            Line::raw("Deleted lines appear above in red and added lines below in green, with a checkbox between them to accept or reject the change."),
            Line::raw("The focused hunk is highlighted; the others are dimmed."),
            Line::raw(""),
            Line::raw("Keybindings:"),
            // Glyphs come from the shared `diff_keys` table so this
            // explanatory list can never teach a key the handler doesn't
            // actually honor; the phrasing stays local to the modal.
            Line::raw(format!(
                "  Next / previous hunk:  {} / {}",
                diff_hint(&Action::DiffNext),
                diff_hint(&Action::DiffPrev),
            )),
            Line::raw(format!(
                "  Accept / reject hunk:  {} / {}",
                diff_hint(&Action::DiffAcceptHunk),
                diff_hint(&Action::DiffRejectHunk),
            )),
            Line::raw(format!(
                "  Accept / reject all:   {} / {}",
                diff_hint(&Action::DiffAcceptAll),
                diff_hint(&Action::DiffRejectAll),
            )),
            Line::raw(format!(
                "  Undo a decision:       {}",
                diff_hint(&Action::DiffResetHunk),
            )),
            Line::raw(format!(
                "  Exit diff mode:        {}",
                diff_hint(&Action::DiffExit),
            )),
        ]
    }

    /// Map a widget response to a modal outcome: Continue keeps the modal
    /// open, Close persists the opt-out and drops the modal.
    fn resolve(&self, response: DiffIntroResponse) -> ModalOutcome {
        match response {
            DiffIntroResponse::Continue => ModalOutcome::Continue,
            DiffIntroResponse::Close => self.close_outcome(),
        }
    }

    fn close_outcome(&self) -> ModalOutcome {
        let persist_off = self.state.dont_show_again;
        ModalOutcome::CloseAnd(Box::new(move |app| {
            if persist_off && app.config.editor.show_diff_intro {
                app.config.editor.show_diff_intro = false;
                app.save_config_with_flash("failed to persist diff intro opt-out");
            }
        }))
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
        let view = DiffIntroView {
            theme: ctx.theme,
            body: &body,
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
        let response = self.state.handle_key(&key);
        self.resolve(response)
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.handle_wheel(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        let response = self.state.handle_click(col, row);
        self.resolve(response)
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
    use crate::ui::diff_intro_modal::DiffIntroFocus;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn focus_starts_on_continue() {
        assert_eq!(DiffIntroModal::new().state.focus, DiffIntroFocus::Confirm);
    }

    #[test]
    fn activating_toggle_keeps_modal_open_and_flips_opt_out() {
        let mut app = make_app();
        let mut modal = DiffIntroModal::new();
        modal.handle_key(key(KeyCode::Up), &mut app, 40, 80); // focus toggle
        assert!(!modal.state.dont_show_again);
        let out = modal.handle_key(key(KeyCode::Enter), &mut app, 40, 80);
        assert!(matches!(out, ModalOutcome::Continue));
        assert!(modal.state.dont_show_again);
    }

    #[test]
    fn activating_continue_closes() {
        let mut app = make_app();
        let mut modal = DiffIntroModal::new(); // focused on Continue
        let out = modal.handle_key(key(KeyCode::Enter), &mut app, 40, 80);
        assert!(matches!(out, ModalOutcome::CloseAnd(_)));
    }

    #[test]
    fn body_documents_the_undo_binding() {
        // Backspace-to-undo isn't surfaced on the hint line, so the intro
        // modal is where the user learns it.  The glyph comes from the
        // shared `diff_keys` table (`⌫`).
        let modal = DiffIntroModal::new();
        let undo_glyph = diff_hint(&Action::DiffResetHunk);
        let has_undo = modal.body().iter().any(|line| {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            text.contains("Undo") && text.contains(undo_glyph)
        });
        assert!(has_undo, "intro modal must document the undo-decision key");
    }
}
