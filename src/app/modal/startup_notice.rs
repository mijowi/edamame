//! Startup capability-notice modal.  Shown when the terminal lacks
//! features the editor would otherwise use (mouse, image protocols,
//! truecolour, etc.).  Two buttons: "Ok" and "Don't show this again".

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::Config;
use crate::terminal::Capabilities;
use crate::ui::{ModalButton, ModalResponse, ModalState, ModalView};

pub struct StartupNoticeModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    state: ModalState,
    kind: ModalKind,
    dismissable: bool,
}

impl StartupNoticeModal {
    /// Construct the modal when there's something worth reporting and
    /// the user hasn't asked to suppress it.  Returns `None` otherwise.
    pub fn from_capabilities(caps: &Capabilities, config: &Config) -> Option<Self> {
        if config.editor.suppress_capability_warnings {
            return None;
        }
        if !caps.has_missing_features() {
            return None;
        }
        let mut body: Vec<Line<'static>> = caps
            .missing_features_summary()
            .into_iter()
            .map(Line::raw)
            .collect();
        body.push(Line::raw(""));
        body.push(Line::raw(
            "Affected features will be disabled automatically.",
        ));
        Some(Self {
            body,
            buttons: vec![
                ModalButton::new("Ok"),
                ModalButton::new("Don't show this again"),
            ],
            state: ModalState::new(),
            kind: ModalKind::Normal,
            dismissable: true,
        })
    }
}

impl Modal for StartupNoticeModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = ModalView {
            title: "Terminal capabilities",
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
            ModalResponse::Cancelled => ModalOutcome::Close,
            // Button index 1 is "Don't show this again".
            ModalResponse::ButtonPressed(1) => ModalOutcome::CloseAnd(Box::new(|app| {
                app.config.editor.suppress_capability_warnings = true;
                app.save_config_with_flash("failed to persist capability-warning preference");
            })),
            ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
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
