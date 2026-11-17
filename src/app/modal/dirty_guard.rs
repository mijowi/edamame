//! Save / Discard guard shown before navigating away from an unsaved document; Esc abandons
//! the navigation.  Carries the pending target so the App can resume it.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::nav::NavPending;
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse};

pub struct DirtyGuardModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
    /// `Option` only so the close callback can take ownership.
    pending: Option<NavPending>,
    /// The deep link's `#fragment`; rides along so the guard resumes the *whole* link.
    fragment: Option<String>,
}

impl DirtyGuardModal {
    pub(crate) fn new(
        current_display: &str,
        pending: NavPending,
        fragment: Option<String>,
    ) -> Self {
        let body = vec![
            Line::raw(format!("{current_display} has unsaved changes.")),
            Line::raw(""),
            Line::raw(format!(
                "Opening {} will abandon them.",
                pending.display_name()
            )),
            Line::raw(""),
            Line::raw("What would you like to do?"),
        ];
        Self {
            body,
            buttons: vec![ModalButton::new("Save"), ModalButton::new("Discard")],
            chrome: ModalChrome::new(ModalKind::Warning, true),
            pending: Some(pending),
            fragment,
        }
    }

    /// Shared by the key and click paths; doc dimensions come from `App`'s cache because
    /// `handle_click` has none to thread in.
    ///
    /// `ensure_cursor_visible` corrects the document the modal was covering, so it is
    /// skipped on every branch where navigation happened: a fragment jump moves `scroll`
    /// without the cursor, and re-asserting visibility would throw the jump away.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::CloseAnd(Box::new(move |app| {
                let (h, w) = (app.last_doc_height, app.last_doc_width);
                app.editor.ensure_cursor_visible(h, w);
            })),
            ModalResponse::ButtonPressed(idx) => {
                let Some(pending) = self.pending.take() else {
                    return ModalOutcome::Close;
                };
                let fragment = self.fragment.take();
                match idx {
                    // Behind the mixed-endings prompt, whose `Esc` abandons the navigation.
                    0 => ModalOutcome::CloseAnd(Box::new(move |app| {
                        app.with_line_ending_choice(Box::new(move |app| {
                            save_then_navigate(app, pending, fragment);
                        }));
                    })),
                    _ => ModalOutcome::CloseAnd(Box::new(move |app| {
                        app.editor.dirty = false;
                        let (h, w) = (app.last_doc_height, app.last_doc_width);
                        if !app.navigate_to_pending(pending, fragment, h, w) {
                            app.editor.ensure_cursor_visible(h, w);
                        }
                    })),
                }
            }
        }
    }
}

/// `[Save]`: write the buffer, then resume the navigation it was guarding.
fn save_then_navigate(app: &mut App, pending: NavPending, fragment: Option<String>) {
    let (h, w) = (app.last_doc_height, app.last_doc_width);
    if app.editor.buffer.path().is_some() {
        match app.save_buffer() {
            Ok(()) => {
                if app.navigate_to_pending(pending, fragment, h, w) {
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(target: "link", error = %e, "save-before-navigate failed");
            }
        }
    } else {
        // Pathless: prompt, then navigate once written.  This document
        // stays on screen meanwhile, so the correction below applies.
        app.open_save_as_modal(Some(Box::new(move |app| {
            let (h, w) = (app.last_doc_height, app.last_doc_width);
            let _ = app.navigate_to_pending(pending, fragment, h, w);
        })));
    }
    app.editor.ensure_cursor_visible(h, w);
}

impl Modal for DirtyGuardModal {
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

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
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
