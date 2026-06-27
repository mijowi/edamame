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
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::{Action, Theme};
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

    fn body(&self, theme: &Theme) -> Vec<Line<'static>> {
        // Keybinding glyphs are painted in the accent color so they stand
        // out from their plain-text labels; the glyphs themselves come
        // from the shared `diff_keys` table so this explanatory list can
        // never teach a key the handler doesn't actually honor.
        let accent = Style::default().fg(theme.palette.accent);
        vec![
            Line::raw("The file on disk has changed. edamame will now enter diff mode, in which you can review and accept or reject changes."),
            Line::raw(""),
            Line::raw("Deleted lines appear above in red and added lines below in green, with a checkbox between them to accept or reject the change."),
            Line::raw("The focused hunk is highlighted; the others are dimmed."),
            Line::raw(""),
            Line::raw("Keybindings:"),
            binding_line(
                "  Next / previous hunk:  ",
                &[&Action::DiffNext, &Action::DiffPrev],
                accent,
            ),
            binding_line(
                "  Accept / reject hunk:  ",
                &[&Action::DiffAcceptHunk, &Action::DiffRejectHunk],
                accent,
            ),
            binding_line(
                "  Accept / reject all:   ",
                &[&Action::DiffAcceptAll, &Action::DiffRejectAll],
                accent,
            ),
            binding_line("  Undo a decision:       ", &[&Action::DiffResetHunk], accent),
            binding_line("  Exit diff mode:        ", &[&Action::DiffExit], accent),
            Line::raw(""),
            Line::raw("Don't want this? Diff mode can be turned off in settings (\"Diff when file changes\"), or use the toggle below to just stop showing this notice."),
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

/// Build one keybinding row: a fixed-width plain `prefix` (label + padding)
/// followed by the action glyphs in the accent color, joined by " / ".
fn binding_line(prefix: &str, actions: &[&Action], accent: Style) -> Line<'static> {
    let mut spans = vec![Span::raw(prefix.to_owned())];
    for (i, action) in actions.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" / "));
        }
        spans.push(Span::styled(diff_hint(action), accent));
    }
    Line::from(spans)
}

impl Default for DiffIntroModal {
    fn default() -> Self {
        Self::new()
    }
}

impl Modal for DiffIntroModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let body = self.body(ctx.theme);
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
        let theme = Theme::default();
        let undo_glyph = diff_hint(&Action::DiffResetHunk);
        let has_undo = modal.body(&theme).iter().any(|line| {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            text.contains("Undo") && text.contains(undo_glyph)
        });
        assert!(has_undo, "intro modal must document the undo-decision key");
    }
}
