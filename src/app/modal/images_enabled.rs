//! Master images-enabled prompt.  Shown when `config.images.enabled`
//! is `Ask` and the open document contains at least one image.
//! Four buttons (Yes / No / Always / Never) — the first two affect
//! only the current session; the latter two persist to config.
//! Escape (or the `esc` close hint) is equivalent to "No": images
//! are disabled for this session without persisting a preference.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use super::RemoteImagePromptModal;
use crate::app::App;
use crate::config::Config;
use crate::editor::EditorState;
use crate::ui::{ModalButton, ModalResponse};

pub struct ImagesEnabledPromptModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
}

impl ImagesEnabledPromptModal {
    /// Construct the modal when the policy is `Ask` and the document
    /// has image blocks.  Returns `None` otherwise.
    pub fn from_state(editor: &EditorState, config: &Config) -> Option<Self> {
        if !matches!(config.images.enabled, crate::config::ImagesEnabled::Ask) {
            return None;
        }
        // Diagram blocks are synthetic `Block::ImageBlock`s promoted
        // from fenced code blocks; they carry `source: Some(_)` and are
        // handled by `DiagramsEnabledPromptModal` instead.  A document
        // with only diagrams (no real images) must not trigger this
        // prompt.
        let has_real_image = editor
            .parsed
            .image_blocks
            .iter()
            .any(|b| b.source.is_none());
        if !has_real_image {
            return None;
        }
        let body = vec![
            Line::raw("This document contains images."),
            Line::raw(""),
            Line::raw("Would you like edamame to display images?"),
        ];
        Some(Self {
            body,
            buttons: vec![
                ModalButton::new("Yes"),
                ModalButton::new("No"),
                ModalButton::new("Always"),
                ModalButton::new("Never"),
            ],
            chrome: ModalChrome::new(ModalKind::Warning, true),
        })
    }

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths so a mouse click on a button behaves exactly like
    /// pressing it.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::CloseAnd(Box::new(decline_for_session)),
            ModalResponse::ButtonPressed(idx) => {
                // Button order:
                //   0 → Yes    (session-only show, no config change)
                //   1 → No     (session-only hide, no config change)
                //   2 → Always (persist `ImagesEnabled::Always`)
                //   3 → Never  (persist `ImagesEnabled::Never`)
                match idx {
                    0 => ModalOutcome::CloseAnd(Box::new(|app| {
                        app.session_images_enabled = Some(true);
                        app.dispatch_image_decodes();
                    })),
                    1 => ModalOutcome::CloseAnd(Box::new(decline_for_session)),
                    2 => ModalOutcome::CloseAnd(Box::new(|app| {
                        app.config.images.enabled = crate::config::ImagesEnabled::Always;
                        app.save_config_with_flash("failed to persist images.enabled=always");
                        app.dispatch_image_decodes();
                    })),
                    _ => ModalOutcome::CloseAnd(Box::new(|app| {
                        app.config.images.enabled = crate::config::ImagesEnabled::Never;
                        app.save_config_with_flash("failed to persist images.enabled=never");
                        decline_for_session(app);
                    })),
                }
            }
        }
    }
}

impl Modal for ImagesEnabledPromptModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome
            .render(frame, area, ctx, "Images", &self.body, &self.buttons);
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

/// Common cleanup when the user opts out of images this session: drop
/// any queued remote-image prompt (since no images will load),
/// collapse image blocks to their one-line placeholders, and refresh
/// the parse so the layout reflects the change immediately.
fn decline_for_session(app: &mut App) {
    app.session_images_enabled = Some(false);
    app.modal_stack.remove_first::<RemoteImagePromptModal>();
    app.editor.images_enabled = false;
    app.editor.refresh_parsed();
}
