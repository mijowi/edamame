//! Indexed-color theme substitution notice.  Fires at startup when the
//! terminal lacks 24-bit color and the configured theme is not one of
//! the two built-ins authored against the 256-color cube
//! ([`crate::config::theme::indexed_fallback_theme`]).
//!
//! The substitution has already happened by the time this renders —
//! `App::new` swaps the theme *before* the first frame precisely so this
//! modal is legible.  An RGB theme quantized into the 256-color cube
//! regularly lands fg and bg on the same entry, which would make a
//! "your colors are wrong" notice invisible: the failure mode hides its
//! own explanation.  So the swap is not advice, and this modal is not a
//! prompt — it reports what was already done and how to keep or undo it.
//!
//! Nothing is written to `config.toml`.  The likely story is a user
//! whose theme was chosen on a *different*, more capable terminal (the
//! config travels between them via dotfiles), so their choice is stashed
//! in `Config::theme_downgraded_from` and restored by `Config::save`.
//! There is deliberately no "switch theme" affordance: on a terminal
//! this weak the only other theme that renders correctly is the opposite
//! appearance of the one just substituted, which is not a choice worth a
//! button.  Anyone who wants it can open the theme picker directly.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{theme_downgrade_lines, ModalButton, ModalResponse, PROSE_CONTENT_WIDTH};

pub struct ThemeDowngradeModal {
    /// The theme the user configured — restored on any terminal that
    /// can render it.
    configured: String,
    /// The indexed-color theme substituted for this session.
    substituted: &'static str,
    /// Always empty — kept so the `ModalChrome` calls take the same
    /// shape as every other modal's.
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
}

impl ThemeDowngradeModal {
    pub fn new(configured: String, substituted: &'static str) -> Self {
        Self {
            configured,
            substituted,
            buttons: Vec::new(),
            // One long paragraph, so cap the measure — uncapped, the
            // body's natural width is the whole unwrapped sentence and
            // the modal stretches across the terminal.
            chrome: ModalChrome::new(ModalKind::Warning, true)
                .with_max_content_width(PROSE_CONTENT_WIDTH),
        }
    }

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths.  The modal is buttonless: `Esc` or the `esc`
    /// affordance is the only resolution, per convention.
    fn resolve(&self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }
}

impl Modal for ThemeDowngradeModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        // Paragraphs, not pre-broken display lines: `ModalView` wraps
        // the body it is handed.  The explanation itself is shared with
        // the capabilities notice so the two can't drift.
        let body = theme_downgrade_lines(&self.configured, self.substituted, ctx.theme);
        self.chrome
            .render(frame, area, ctx, "Theme changed", &body, &self.buttons);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_modal_is_buttonless() {
        // Esc / the `esc` affordance is the only resolution; the only
        // theme worth switching to here is the opposite appearance.
        let modal = ThemeDowngradeModal::new("Dracula".into(), "256 Dark");
        assert!(modal.buttons.is_empty());
    }
}
