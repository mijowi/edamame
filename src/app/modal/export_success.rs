//! Post-export success modal.  Three buttons: OK (close), Open the
//! exported `<name>.toml` in the user's `$VISUAL` / `$EDITOR`, or
//! Open the surrounding config folder in the OS file manager.
//!
//! The theme has already been written and applied by
//! [`super::ExportThemeModal`] before this modal opens, so all three
//! buttons are post-effect: dismissing is harmless.

use std::any::Any;
use std::path::PathBuf;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::Config;
use crate::ui::{ModalButton, ModalKind, ModalResponse};

const BUTTONS: &[&str] = &["OK", "Open in default editor", "Open config folder"];

pub struct ExportSuccessModal {
    chrome: ModalChrome,
    path: PathBuf,
    body: [Line<'static>; 1],
    buttons: Vec<ModalButton>,
}

impl ExportSuccessModal {
    pub fn new(path: PathBuf) -> Self {
        let body = [Line::raw(format!("Theme exported to {}", path.display()))];
        let buttons = BUTTONS.iter().map(|l| ModalButton::new(*l)).collect();
        Self {
            chrome: ModalChrome::new(ModalKind::Normal, true),
            path,
            body,
            buttons,
        }
    }

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths so a mouse click on a button behaves exactly like
    /// pressing it.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::Close,
            ModalResponse::ButtonPressed(0) => ModalOutcome::Close,
            ModalResponse::ButtonPressed(1) => {
                let path = self.path.clone();
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    app.pending_open_theme_in_editor = Some(path);
                    app.needs_draw = true;
                }))
            }
            ModalResponse::ButtonPressed(2) => ModalOutcome::CloseAnd(Box::new(|app| {
                if let Some(dir) = Config::config_dir() {
                    app.spawn_open_worker(dir.display().to_string());
                } else {
                    app.notify("No config directory available", ModalKind::Error);
                }
            })),
            ModalResponse::ButtonPressed(_) => ModalOutcome::Continue,
        }
    }
}

impl Modal for ExportSuccessModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome.render(
            frame,
            area,
            ctx,
            "Theme exported",
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
