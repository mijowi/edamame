//! Explanatory modal shown the first time the user enters diff-review
//! mode for this install.  Sits on top of [`crate::ui::ModalView`] but
//! carries a tiny additional bit of state — a `dont_show_again`
//! toggle — so it can persist the opt-out to
//! `config.editor.show_diff_intro` on `[Continue]`.
//!
//! The opt-out is a bare footer toggle (`[x] Don't show this again`)
//! alongside `[ Continue ]`, so it joins the normal focus cycle:
//! Tab / Shift-Tab / Left / Right / Up / Down move focus between the
//! two, Enter / Space activate the focused one (toggle vs. continue),
//! and both are clickable.

use std::any::Any;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::Action;
use crate::input::diff_hint;
use crate::ui::{ModalButton, ModalResponse};

/// Footer-button indices.  `[checkbox, Continue]`, Continue focused by
/// default so a bare Enter proceeds.
const CHECKBOX_IDX: usize = 0;
const CONTINUE_IDX: usize = 1;
const NUM_BUTTONS: usize = 2;

pub struct DiffIntroModal {
    chrome: ModalChrome,
    /// Live state of the "Don't show again" toggle.  `false` means the
    /// modal will fire again on the next diff entry; `true` writes
    /// `config.editor.show_diff_intro = false` on confirm.
    dont_show_again: bool,
}

impl DiffIntroModal {
    pub fn new() -> Self {
        let mut chrome = ModalChrome::new(ModalKind::Normal, true);
        chrome.state.focused = CONTINUE_IDX;
        Self {
            chrome,
            dont_show_again: false,
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

    /// Footer buttons, with the checkbox label reflecting the live
    /// toggle.  Rebuilt each render so the glyph tracks `dont_show_again`.
    fn buttons(&self) -> Vec<ModalButton> {
        let check = if self.dont_show_again { "[x]" } else { "[ ]" };
        vec![
            // Bare (no `[ … ]` wrapper): the `[ ]`/`[x]` glyph is the
            // checkbox, mirroring the welcome modal's toggle.
            ModalButton::bare(format!("{check} Don't show this again")),
            ModalButton::new("Continue"),
        ]
    }

    /// Apply a footer-button activation; toggling the checkbox keeps
    /// the modal open, Continue closes it.
    fn activate(&mut self, idx: usize) -> ModalOutcome {
        if idx == CHECKBOX_IDX {
            self.dont_show_again = !self.dont_show_again;
            self.chrome.state.focused = CHECKBOX_IDX;
            ModalOutcome::Continue
        } else {
            self.close_outcome()
        }
    }

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths: activating a button toggles or continues, Esc /
    /// esc-click persists the opt-out and closes.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::ButtonPressed(idx) => self.activate(idx),
            ModalResponse::Cancelled => self.close_outcome(),
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
        self.chrome
            .render(frame, area, ctx, "Entering diff mode", &body, &buttons);
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
            let focused = &mut self.chrome.state.focused;
            *focused = (*focused + step) % NUM_BUTTONS;
            return ModalOutcome::Continue;
        }
        // Tab / Shift-Tab / Left / Right cycle focus; Enter / Space / y
        // activate the focused button; Esc / n cancel.
        let response = self.chrome.on_key(&key, NUM_BUTTONS);
        self.resolve(response)
    }

    fn handle_wheel(&mut self, delta: i32) {
        // Scroll the body like every other `ModalView` modal.  This
        // modal also repurposes Up / Down for button focus, so the
        // wheel is the body's only scroll path on a terminal too short
        // to show it all at once.
        self.chrome.on_wheel(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        // The chrome hit-tests both footer buttons (checkbox + Continue)
        // and the esc hint; routing all three through `resolve` keeps a
        // checkbox click a toggle and an esc-hint click a persist-and-
        // close, matching the keyboard paths.
        let response = self.chrome.on_click(col, row);
        self.resolve(response)
    }

    fn kind(&self) -> ModalKind {
        self.chrome.kind()
    }

    fn dismissable(&self) -> bool {
        self.chrome.dismissable()
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
        assert_eq!(DiffIntroModal::new().chrome.state.focused, CONTINUE_IDX);
    }

    #[test]
    fn arrows_and_tab_move_focus_between_buttons() {
        let mut app = make_app();
        let mut modal = DiffIntroModal::new();
        // Down: Continue → checkbox.
        modal.handle_key(key(KeyCode::Down), &mut app, 40, 80);
        assert_eq!(modal.chrome.state.focused, CHECKBOX_IDX);
        // Up: checkbox → Continue.
        modal.handle_key(key(KeyCode::Up), &mut app, 40, 80);
        assert_eq!(modal.chrome.state.focused, CONTINUE_IDX);
        // Tab cycles as well.
        modal.handle_key(key(KeyCode::Tab), &mut app, 40, 80);
        assert_eq!(modal.chrome.state.focused, CHECKBOX_IDX);
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
        let cb = modal.chrome.state.button_rects[CHECKBOX_IDX];
        let out = modal.handle_click(cb.x, cb.y);
        assert!(matches!(out, ModalOutcome::Continue));
        assert!(modal.dont_show_again, "checkbox click must toggle");

        // Re-render so rects reflect the new label width, then click
        // Continue to close.
        render_offscreen(&mut modal);
        let cont = modal.chrome.state.button_rects[CONTINUE_IDX];
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
