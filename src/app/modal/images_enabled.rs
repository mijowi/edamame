//! Master images-enabled prompt.  Shown when `config.images.enabled`
//! is `Ask` and the open document contains at least one image.
//! Four buttons (Yes / No / Always / Never) — the first two affect
//! only the current session; the latter two persist to config.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use super::RemoteImagePromptModal;
use crate::app::App;
use crate::config::Config;
use crate::editor::EditorState;
use crate::ui::{ModalButton, ModalResponse, ModalState, ModalView};

pub struct ImagesEnabledPromptModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    state: ModalState,
    kind: ModalKind,
    dismissable: bool,
}

impl ImagesEnabledPromptModal {
    /// Construct the modal when the policy is `Ask` and the document
    /// has image blocks.  Returns `None` otherwise.
    pub fn from_state(editor: &EditorState, config: &Config) -> Option<Self> {
        if !matches!(config.images.enabled, crate::config::ImagesEnabled::Ask) {
            return None;
        }
        if editor.parsed.image_blocks.is_empty() {
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
            state: ModalState::new(),
            kind: ModalKind::Warning,
            dismissable: false,
        })
    }
}

impl Modal for ImagesEnabledPromptModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = ModalView {
            title: "Images",
            body: &self.body,
            buttons: &self.buttons,
            theme: ctx.theme,
            kind: self.kind,
            dismissable: self.dismissable,
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
        match self
            .state
            .handle_key(&key, self.buttons.len(), self.dismissable)
        {
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

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        super::types::close_if_esc_clicked(self.state.esc_button_rect, col, row)
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
