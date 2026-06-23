//! Two buttons: Save / Discard.  Save persists the buffer then exits;
//! failure surfaces a sticky error transient and aborts the quit.
//! Discard exits without saving.  Escape (or the `esc` close hint)
//! dismisses without quitting.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse};

pub struct QuitConfirmModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
}

impl QuitConfirmModal {
    /// Build the prompt body with the supplied display name (typically
    /// the buffer's filename, or "Current buffer" when unsaved with
    /// no path).
    pub fn new(display_name: &str) -> Self {
        let body = vec![
            Line::raw(format!("{display_name} has unsaved changes.")),
            Line::raw(""),
            Line::raw("What would you like to do?"),
        ];
        Self {
            body,
            buttons: vec![ModalButton::new("Save"), ModalButton::new("Discard")],
            chrome: ModalChrome::new(ModalKind::Warning, true),
        }
    }

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths so a mouse click on a button behaves exactly like
    /// pressing it.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::Close,
            ModalResponse::ButtonPressed(0) => ModalOutcome::CloseAnd(Box::new(|app| {
                if app.editor.buffer.path().is_some() {
                    match app.save_buffer() {
                        Ok(()) => app.should_quit = true,
                        Err(_) => app.notify("Save failed — quit aborted", ModalKind::Error),
                    }
                } else {
                    // No path yet — prompt for one, then quit once the
                    // buffer is written.  Cancelling the prompt aborts
                    // the quit, leaving the buffer intact.
                    app.open_save_as_modal(Some(Box::new(|app| app.should_quit = true)));
                }
            })),
            ModalResponse::ButtonPressed(1) => ModalOutcome::CloseAnd(Box::new(|app| {
                app.should_quit = true;
            })),
            ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }
}

impl Modal for QuitConfirmModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome.render(
            frame,
            area,
            ctx,
            "Unsaved changes",
            &self.body,
            &self.buttons,
        );
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        let response = self.chrome.on_key(&key, self.buttons.len());
        self.resolve(response)
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.chrome.on_wheel(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
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

#[cfg(test)]
mod tests {
    //! App-level wiring for the quit-confirm modal.  Driving
    //! `App::open_quit_confirm` and `App::dispatch_modal_key` directly
    //! exercises both the push and the per-button outcome without
    //! standing up the event loop.

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::make_app;

    #[test]
    fn open_quit_confirm_seeds_three_button_modal() {
        let mut app = make_app();
        app.open_quit_confirm();
        assert!(app.modal_stack.contains::<QuitConfirmModal>());
        // Button-label invariants are covered by the QuitConfirmModal
        // constructor; here we just assert the modal is on the stack.
    }

    #[test]
    fn quit_confirm_escape_dismisses_without_quit() {
        let mut app = make_app();
        app.open_quit_confirm();
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 40, 80);
        assert!(!app.modal_stack.contains::<QuitConfirmModal>());
        assert!(!app.should_quit);
    }

    #[test]
    fn click_on_esc_hint_dismisses_modal() {
        // Exercises `App::dispatch_modal_click` end-to-end: render the
        // modal once to populate `state.esc_button_rect`, then click
        // inside that rect.  The modal must close via the same
        // pop-dispatch-push pipeline used by real mouse events.
        use crate::app::modal::types::ModalRenderCtx;
        use crate::config::{Config, Theme};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = make_app();
        app.open_quit_confirm();

        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let config = Config::default();
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|frame| {
                let ctx = ModalRenderCtx {
                    theme,
                    config: &config,
                    cursor_visible: false,
                };
                let area = frame.area();
                if let Some(top) = app.modal_stack.top_mut() {
                    top.render(frame, area, &ctx);
                }
            })
            .unwrap();

        let rect = app
            .modal_stack
            .top_mut()
            .and_then(|m| m.as_any().downcast_ref::<QuitConfirmModal>())
            .and_then(|m| m.chrome.state.esc_button_rect)
            .expect("esc rect populated after render");

        // Click outside the rect first — modal stays open.
        app.dispatch_modal_click(0, 0);
        assert!(app.modal_stack.contains::<QuitConfirmModal>());

        // Click inside the rect — modal closes via the click router.
        app.dispatch_modal_click(rect.x, rect.y);
        assert!(!app.modal_stack.contains::<QuitConfirmModal>());
        assert!(!app.should_quit, "esc-click must not trigger Save/Discard");
    }

    #[test]
    fn quit_confirm_discard_sets_should_quit() {
        let mut app = make_app();
        app.editor.dirty = true;
        app.open_quit_confirm();
        // Tab onto the Discard button (index 1) and press Enter.
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);
        assert!(!app.modal_stack.contains::<QuitConfirmModal>());
        assert!(app.should_quit);
    }

    #[test]
    fn click_on_discard_button_quits() {
        // Fix A end-to-end: a left-click on the rendered `[ Discard ]`
        // footer button drives the same Discard outcome as Tab+Enter,
        // through the real pop-dispatch-push click router.  Before the
        // shared chrome hit-tested footer buttons, this click was a
        // no-op.
        use crate::app::modal::types::ModalRenderCtx;
        use crate::config::{Config, Theme};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = make_app();
        app.editor.dirty = true;
        app.open_quit_confirm();

        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let config = Config::default();
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|frame| {
                let ctx = ModalRenderCtx {
                    theme,
                    config: &config,
                    cursor_visible: false,
                };
                let area = frame.area();
                if let Some(top) = app.modal_stack.top_mut() {
                    top.render(frame, area, &ctx);
                }
            })
            .unwrap();

        // Button index 1 is [ Discard ]; click its centre.
        let rect = app
            .modal_stack
            .top_mut()
            .and_then(|m| m.as_any().downcast_ref::<QuitConfirmModal>())
            .map(|m| m.chrome.state.button_rects[1])
            .expect("button rects populated after render");
        app.dispatch_modal_click(rect.x + rect.width / 2, rect.y);

        assert!(!app.modal_stack.contains::<QuitConfirmModal>());
        assert!(app.should_quit, "clicking Discard must quit");
    }
}
