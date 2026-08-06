//! New-terminal capabilities notice.  Fires once the first time edamame
//! is launched in a terminal whose fingerprint
//! ([`crate::terminal::Capabilities::fingerprint`]) hasn't been recorded
//! in `editor.seen_terminal_fingerprints`.  Body lists every detected
//! capability (color, images, mouse, keyboard, unicode) with a ✓ or ✗
//! mark; dismiss appends the current fingerprint to the seen set so the
//! notice stays quiet on future launches in the same terminal but
//! re-fires when the user opens edamame somewhere new.
//!
//! Because a new terminal can be *worse* than the last one (or better),
//! the notice is not purely informational: an "Adjust settings" button
//! stacks the welcome modal — the one surface that re-derives
//! `full_color` / `image_capable` from the live capabilities and gates
//! theme / images / diagrams accordingly.  The generic settings overlay
//! is deliberately not the destination; it would happily let the user
//! enable images on a terminal that quantizes them.  The same surface
//! is reachable any time via `Action::OpenWelcome`.
//!
//! When the startup indexed-color theme substitution fires on the same
//! launch (a first visit to a terminal that also can't render the user's
//! theme — the common "moved from Ghostty to Terminal.app" case), this
//! notice absorbs that explanation via
//! [`with_theme_downgrade`](TerminalCapabilitiesModal::with_theme_downgrade)
//! and `App::new` suppresses the standalone
//! [`super::ThemeDowngradeModal`].  Two modals saying overlapping things
//! about the same terminal, one hiding the other, reads as a bug; the
//! capabilities summary is the more complete of the two, so it wins and
//! the downgrade prose joins it.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::terminal::Capabilities;
use crate::ui::{
    build_cap_lines, theme_downgrade_lines, CapSummary, ModalButton, ModalResponse,
    PROSE_CONTENT_WIDTH,
};

/// Index of the "Adjust settings" button in `buttons`.  Named so the
/// `resolve` match arm and the button-list order can't drift.
const ADJUST_BUTTON: usize = 0;

/// The startup theme substitution, when it fired on this launch — the
/// user's configured theme, the indexed-color built-in that replaced it,
/// and the depth that forced it.  See [`crate::ui::theme_downgrade_lines`].
struct ThemeDowngrade {
    configured: String,
    substituted: &'static str,
}

pub struct TerminalCapabilitiesModal {
    summary: CapSummary,
    fingerprint: String,
    downgrade: Option<ThemeDowngrade>,
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
            downgrade: None,
            buttons: vec![ModalButton::new("Adjust settings")],
            // Prose paragraphs around the capability rows, and it can
            // absorb the downgrade explanation too — cap the measure at
            // the same width the standalone downgrade modal uses so the
            // two stay visually interchangeable.  The rows themselves
            // are far narrower than the cap, so they are unaffected.
            chrome: ModalChrome::new(ModalKind::Normal, true)
                .with_max_content_width(PROSE_CONTENT_WIDTH),
        })
    }

    /// Fold the startup theme substitution's explanation into this
    /// notice.  `App::new` calls this instead of pushing a separate
    /// [`super::ThemeDowngradeModal`] whenever both would fire on the
    /// same launch.
    ///
    /// Also drops the "Adjust settings" button.  A downgrade means the
    /// terminal has no 24-bit color, and the welcome modal has nothing
    /// left to offer there: its theme row is disabled, and images and
    /// diagrams are force-set to `Never`, leaving only the vim toggle.
    /// Worse, saving it *persists* those forced values over whatever the
    /// user chose on their capable terminal — the opposite of the
    /// session-only promise this notice just made.  So the route stays
    /// closed here; `Action::OpenWelcome` remains for anyone who wants
    /// it deliberately.
    pub fn with_theme_downgrade(mut self, configured: String, substituted: &'static str) -> Self {
        self.downgrade = Some(ThemeDowngrade {
            configured,
            substituted,
        });
        self.buttons.clear();
        self
    }

    /// Close, record this terminal's fingerprint as seen, and — when
    /// `adjust` — stack the welcome modal so the user lands directly on
    /// the capability-aware settings surface.  Recording happens on
    /// *both* paths: the notice has served its purpose either way, and
    /// the welcome modal re-seeds the same fingerprint on save.
    fn record_outcome(&self, adjust: bool) -> ModalOutcome {
        let fp = self.fingerprint.clone();
        ModalOutcome::CloseAnd(Box::new(move |app| {
            if !app.config.editor.seen_terminal_fingerprints.contains(&fp) {
                app.config.editor.seen_terminal_fingerprints.push(fp);
                app.save_config_with_flash("failed to persist terminal capabilities notice");
            }
            if adjust {
                app.open_welcome_modal();
            }
        }))
    }

    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths: every resolution dismisses the notice and records
    /// the fingerprint; the "Adjust settings" button, when present, also
    /// opens the welcome modal.  There is deliberately no Dismiss button —
    /// per convention a dismissable modal is closed with `Esc` or the
    /// `esc` affordance in its title bar.
    fn resolve(&self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::ButtonPressed(ADJUST_BUTTON) => self.record_outcome(true),
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => {
                self.record_outcome(false)
            }
        }
    }
}

impl Modal for TerminalCapabilitiesModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        // Paragraphs, not pre-broken display lines: `ModalView` wraps
        // the body it is handed.
        let mut body: Vec<Line<'static>> = Vec::new();
        body.push(Line::raw(
            "It looks like you're using a new terminal application. edamame has \
             detected the following capabilities for this terminal:",
        ));
        body.push(Line::raw(""));
        body.extend(build_cap_lines(&self.summary.rows, ctx.theme));
        if !self.summary.all_ok() {
            body.push(Line::raw(""));
            body.push(Line::styled(
                "Items marked ✗ will be disabled or degraded automatically.",
                Style::default().fg(ctx.theme.palette.warning),
            ));
        }
        if let Some(d) = &self.downgrade {
            body.push(Line::raw(""));
            body.extend(theme_downgrade_lines(
                &d.configured,
                d.substituted,
                ctx.theme,
            ));
        }
        // The trailer describes the button, so it goes when the button
        // does (see `with_theme_downgrade`).
        if self.downgrade.is_none() {
            body.push(Line::raw(""));
            body.push(Line::raw(
                "Adjust settings re-opens the setup screen so theme, images, and diagrams \
                 can be matched to this terminal.",
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
    fn a_folded_downgrade_drops_the_welcome_route() {
        // The welcome modal can only overwrite good settings with the
        // forced-off ones on a terminal this weak — see
        // `with_theme_downgrade`.
        let modal = TerminalCapabilitiesModal::from_capabilities(&Capabilities::minimal(), &[])
            .expect("unseen fingerprint yields a modal")
            .with_theme_downgrade("Dracula".into(), "256 Dark");
        assert!(modal.buttons.is_empty());
    }

    #[test]
    fn adjust_button_index_matches_its_position() {
        // `resolve` matches on ADJUST_BUTTON; if the button list is ever
        // reordered without updating the const, "Adjust settings" would
        // silently become a plain dismiss.
        let modal = TerminalCapabilitiesModal::from_capabilities(&Capabilities::minimal(), &[])
            .expect("unseen fingerprint yields a modal");
        assert_eq!(modal.buttons[ADJUST_BUTTON].label, "Adjust settings");
        // No Dismiss button — Esc / the `esc` affordance is the
        // acknowledge path.
        assert_eq!(modal.buttons.len(), 1);
    }

    #[test]
    fn seen_fingerprint_suppresses_the_notice() {
        let caps = Capabilities::minimal();
        let seen = vec![caps.fingerprint()];
        assert!(TerminalCapabilitiesModal::from_capabilities(&caps, &seen).is_none());
    }
}
