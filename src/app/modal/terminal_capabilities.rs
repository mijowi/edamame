//! New-terminal capabilities notice.  Fires once the first time edamame
//! is launched in a terminal whose fingerprint
//! ([`crate::terminal::Capabilities::fingerprint`]) hasn't been recorded
//! in `editor.seen_terminal_fingerprints`.  Body lists every detected
//! capability (color, images, mouse, keyboard, unicode) with a ✓ or ✗
//! mark; dismiss appends the current fingerprint to the seen set so the
//! notice stays quiet on future launches in the same terminal but
//! re-fires when the user opens edamame somewhere new.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::terminal::Capabilities;
use crate::ui::{build_cap_lines, CapSummary, ModalButton, ModalResponse, ModalState, ModalView};

pub struct TerminalCapabilitiesModal {
    summary: CapSummary,
    fingerprint: String,
    buttons: Vec<ModalButton>,
    state: ModalState,
    kind: ModalKind,
    dismissable: bool,
}

impl TerminalCapabilitiesModal {
    /// Build the modal when this terminal's fingerprint isn't in the
    /// seen set.  Returns `None` when the fingerprint has already been
    /// recorded.
    pub fn from_capabilities(caps: &Capabilities, seen: &[String]) -> Option<Self> {
        let fingerprint = caps.fingerprint();
        if seen.iter().any(|s| s == &fingerprint) {
            return None;
        }
        Some(Self {
            summary: CapSummary::from_caps(caps),
            fingerprint,
            buttons: vec![],
            state: ModalState::new(),
            kind: ModalKind::Normal,
            dismissable: true,
        })
    }

    fn record_outcome(&self) -> ModalOutcome {
        let fp = self.fingerprint.clone();
        ModalOutcome::CloseAnd(Box::new(move |app| {
            if !app.config.editor.seen_terminal_fingerprints.contains(&fp) {
                app.config.editor.seen_terminal_fingerprints.push(fp);
                app.save_config_with_flash("failed to persist terminal capabilities notice");
            }
        }))
    }
}

impl Modal for TerminalCapabilitiesModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let mut body: Vec<Line<'static>> = Vec::new();
        body.push(Line::raw(
            "It looks like you're using a new terminal application.",
        ));
        body.push(Line::raw(
            "edamame has detected the following capabilities for this terminal:",
        ));
        body.push(Line::raw(""));
        body.extend(build_cap_lines(&self.summary.rows, ctx.theme));
        if !self.summary.all_ok() {
            body.push(Line::raw(""));
            body.push(Line::raw(
                "Items marked ✗ will be disabled or degraded automatically.",
            ));
        }
        let view = ModalView::new(
            "Terminal capabilities",
            &body,
            &self.buttons,
            ctx.theme,
            self.kind,
            self.dismissable,
        );
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
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => self.record_outcome(),
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        if super::types::esc_rect_hit(self.state.esc_button_rect, col, row) {
            self.record_outcome()
        } else {
            ModalOutcome::Continue
        }
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
