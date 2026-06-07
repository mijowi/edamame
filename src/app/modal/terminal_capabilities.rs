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

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::terminal::Capabilities;
use crate::ui::{build_cap_lines, CapSummary, ModalButton, ModalResponse};

pub struct TerminalCapabilitiesModal {
    summary: CapSummary,
    fingerprint: String,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
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
            chrome: ModalChrome::new(ModalKind::Normal, true),
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

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths: both Esc/esc-click and any button dismiss the
    /// notice and record this terminal's fingerprint as seen.
    fn resolve(&self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => self.record_outcome(),
        }
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
        self.chrome.render(
            frame,
            area,
            ctx,
            "Terminal capabilities",
            &body,
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
