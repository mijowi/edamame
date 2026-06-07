//! Remote-image fetch prompt.  Shown when `config.images.remote_policy`
//! is `Ask` and the open document references at least one
//! `http(s)://` image.  Four buttons (Yes / No / Always / Never) —
//! the first two affect only the current session; the latter two
//! persist to config.  Escape (or the `esc` close hint) is
//! equivalent to "No": images stay un-fetched for this session, no
//! preference is written.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::Config;
use crate::editor::EditorState;
use crate::ui::{ModalButton, ModalResponse};

pub struct RemoteImagePromptModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
}

impl RemoteImagePromptModal {
    /// Construct the modal when the document references at least one
    /// `http(s)://` image and the policy is `Ask`.  Returns `None`
    /// otherwise.
    pub fn from_state(editor: &EditorState, config: &Config) -> Option<Self> {
        if matches!(config.images.enabled, crate::config::ImagesEnabled::Never) {
            return None;
        }
        if config.images.remote_policy != crate::config::RemoteImagePolicy::Ask {
            return None;
        }
        let has_remote = editor
            .parsed
            .image_blocks
            .iter()
            .any(|b| crate::image::loader::is_remote(&b.url));
        if !has_remote {
            return None;
        }
        let body = vec![
            Line::raw("This document references one or more remote images."),
            Line::raw("Fetching them sends HTTP requests from your machine."),
            Line::raw(""),
            Line::raw("Would you like edamame to fetch remote images?"),
        ];
        // Button order is intentional: the leftmost button is the default
        // focus.  "Yes" allows the fetch for this session only and is
        // the safe default if the user hammers Enter without reading.
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
            ModalResponse::Cancelled => ModalOutcome::Close,
            ModalResponse::ButtonPressed(idx) => {
                // Button order:
                //   0 → Yes    (session-only, no config change)
                //   1 → No     (dismiss, no config change)
                //   2 → Always (persist `RemoteImagePolicy::Always`)
                //   3 → Never  (persist `RemoteImagePolicy::Never`)
                match idx {
                    0 => ModalOutcome::CloseAnd(Box::new(|app| {
                        app.session_allow_remote = true;
                        app.editor.images.clear_failures_for_remote_reopening();
                        app.dispatch_image_decodes();
                    })),
                    1 => ModalOutcome::Close,
                    2 => ModalOutcome::CloseAnd(Box::new(|app| {
                        app.config.images.remote_policy = crate::config::RemoteImagePolicy::Always;
                        app.save_config_with_flash("failed to persist remote_policy=Always");
                        app.editor.images.clear_failures_for_remote_reopening();
                        app.dispatch_image_decodes();
                    })),
                    _ => ModalOutcome::CloseAnd(Box::new(|app| {
                        app.config.images.remote_policy = crate::config::RemoteImagePolicy::Never;
                        app.save_config_with_flash("failed to persist remote_policy=Never");
                    })),
                }
            }
        }
    }
}

impl Modal for RemoteImagePromptModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome
            .render(frame, area, ctx, "Remote Images", &self.body, &self.buttons);
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
