//! About edamame popover: bean art, rotating acronym tagline, the
//! installed version, author credit, and the `[ Release notes ]` /
//! `[ Check for updates ]` / `[ View on GitHub ]` buttons.
//!
//! It deliberately reports **no** release information of its own.  It
//! used to fetch the latest release on every first open and show a
//! "Current release" row, which made merely opening the page a network
//! request and put a second surface in the business of rendering
//! release state.  Both jobs now belong to `modal::UpdateModal`, which
//! the button opens on top of this page — one display site for that
//! state, so nothing can drift out of sync with it.
//!
//! Time-driven content (the tagline rotation) is *derived* from
//! `opened_at.elapsed()` at render time rather than mutated by a tick —
//! the modal just reports when the next visual change is due via
//! [`Modal::next_deadline`], and the run loop wakes and redraws then.

use std::any::Any;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::chrome::{body_columns, ModalChrome};
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::update_check;
use crate::app::App;
use crate::ui::{about, ModalButton, ModalResponse};

const INSTALLED_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long each acronym expansion stays up before rotating.
const TAGLINE_INTERVAL: Duration = Duration::from_secs(4);

/// Indices into `buttons`, named so the button order and the `resolve`
/// arms acting on them can't drift apart.  Every button has a name:
/// the old catch-all `ButtonPressed(_)` arm meant a fourth button
/// would silently have opened GitHub.
const RELEASE_NOTES_BUTTON: usize = 0;
const CHECK_UPDATES_BUTTON: usize = 1;
const VIEW_ON_GITHUB_BUTTON: usize = 2;

pub struct AboutModal {
    chrome: ModalChrome,
    buttons: Vec<ModalButton>,
    opened_at: Instant,
    /// Offset into [`about::TAGLINES`] so the page doesn't open on the
    /// same expansion every time.
    tagline_start: usize,
}

impl AboutModal {
    /// `pub(crate)` for consistency with the rest of the modal family;
    /// nothing outside the crate constructs a modal.
    pub(crate) fn new() -> Self {
        // Vary the opening tagline without a rand dependency: the
        // sub-second nanos of the wall clock are effectively uniform
        // across user-initiated opens.
        let tagline_start = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize % about::TAGLINES.len())
            .unwrap_or(0);
        Self {
            chrome: ModalChrome::new(ModalKind::Normal, true),
            buttons: vec![
                // Release notes first: it sits directly under the
                // installed-version row it describes, and unlike the
                // other two it answers from the binary itself.
                ModalButton::new("Release notes"),
                ModalButton::new("Check for updates"),
                ModalButton::new("View on GitHub"),
            ],
            opened_at: Instant::now(),
            tagline_start,
        }
    }

    fn tagline_index(&self) -> usize {
        let flips = self.opened_at.elapsed().as_secs() / TAGLINE_INTERVAL.as_secs();
        self.tagline_start + flips as usize
    }

    /// Map a resolved response to an outcome — shared by the key and
    /// click paths.  Both buttons keep the modal open (`ContinueAnd`):
    /// the user returns from the browser, or from the update modal
    /// stacked on top, to the About page rather than to a surprise
    /// dismissal.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::Close,
            ModalResponse::ButtonPressed(RELEASE_NOTES_BUTTON) => {
                ModalOutcome::ContinueAnd(Box::new(|app| app.open_post_upgrade_modal()))
            }
            ModalResponse::ButtonPressed(CHECK_UPDATES_BUTTON) => {
                ModalOutcome::ContinueAnd(Box::new(|app| app.open_update_modal()))
            }
            ModalResponse::ButtonPressed(VIEW_ON_GITHUB_BUTTON) => {
                ModalOutcome::ContinueAnd(Box::new(|app| {
                    app.spawn_open_worker(update_check::GITHUB_URL.to_owned());
                }))
            }
            // No other index exists; a stray press acts on nothing
            // rather than falling into whichever arm happens to be
            // last.
            ModalResponse::ButtonPressed(_) => ModalOutcome::Continue,
        }
    }
}

impl Modal for AboutModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let body = about::body_lines(
            ctx.theme,
            self.tagline_index(),
            INSTALLED_VERSION,
            body_columns(area),
        );
        self.chrome
            .render(frame, area, ctx, "About edamame", &body, &self.buttons);
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

    fn next_deadline(&self) -> Option<Instant> {
        // Next boundary of a `TAGLINE_INTERVAL`-spaced grid anchored at
        // `opened_at`.  Saturating conversion: an (absurdly) long
        // session pins the deadline at the far future instead of
        // wrapping it into the past, where the run loop's
        // `> now` filter would silently drop it.
        let periods = self.opened_at.elapsed().as_nanos() / TAGLINE_INTERVAL.as_nanos();
        let periods = u32::try_from(periods).unwrap_or(u32::MAX);
        Some(self.opened_at + TAGLINE_INTERVAL * periods.saturating_add(1))
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::make_app;
    use crate::ui::MIN_PAD_H;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn open_about_pushes_to_stack_once() {
        let mut app = make_app();
        app.open_about_modal();
        assert!(app.modal_stack.contains::<AboutModal>());
        // Re-dispatching while open must not stack a second copy.
        app.open_about_modal();
        assert_eq!(app.modal_stack.count::<AboutModal>(), 1);
    }

    #[test]
    fn opening_about_does_not_touch_the_network() {
        // The page used to fetch the latest release on first open.  It
        // must not: an explicit "Check for updates" is the only thing
        // that reaches GitHub from here now.
        let mut app = make_app();
        app.open_about_modal();
        assert!(!app.release_check_in_flight);
        assert!(app.latest_release.is_none());
    }

    /// Press the button at `index`, from a freshly opened page, and
    /// run whatever it asked the app to do.  Focus starts on button 0,
    /// so `index` Tabs get there — spelled out once so a change to the
    /// button order breaks one helper rather than every test.
    fn press_button(app: &mut App, index: usize) {
        let mut modal = AboutModal::new();
        for _ in 0..index {
            modal.handle_key(key(KeyCode::Tab), app, 24, 80);
        }
        let outcome = modal.handle_key(key(KeyCode::Enter), app, 24, 80);
        let ModalOutcome::ContinueAnd(action) = outcome else {
            panic!("expected the About page to stay open");
        };
        action(app);
    }

    #[test]
    fn the_check_button_stacks_the_update_modal_over_about() {
        let mut app = make_app();
        app.open_about_modal();
        press_button(&mut app, CHECK_UPDATES_BUTTON);
        assert!(app.modal_stack.contains::<crate::app::modal::UpdateModal>());
        assert!(
            app.modal_stack.contains::<AboutModal>(),
            "About stays underneath"
        );
    }

    #[test]
    fn the_release_notes_button_stacks_the_post_upgrade_modal_over_about() {
        let mut app = make_app();
        app.open_about_modal();
        press_button(&mut app, RELEASE_NOTES_BUTTON);
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::PostUpgradeModal>());
        assert!(
            app.modal_stack.contains::<AboutModal>(),
            "About stays underneath"
        );
    }

    #[test]
    fn the_release_notes_button_reaches_no_network() {
        // It answers out of the binary's own changelog; opening it
        // must not look like an update check.
        let mut app = make_app();
        app.open_about_modal();
        press_button(&mut app, RELEASE_NOTES_BUTTON);
        assert!(!app.release_check_in_flight);
        assert!(app.latest_release.is_none());
    }

    #[test]
    fn enter_on_github_button_keeps_modal_open() {
        let mut app = make_app();
        let mut modal = AboutModal::new();
        // Move focus along to "View on GitHub".
        for _ in 0..VIEW_ON_GITHUB_BUTTON {
            modal.handle_key(key(KeyCode::Tab), &mut app, 24, 80);
        }
        let outcome = modal.handle_key(key(KeyCode::Enter), &mut app, 24, 80);
        assert!(matches!(outcome, ModalOutcome::ContinueAnd(_)));
    }

    #[test]
    fn every_button_index_has_a_named_arm() {
        // The catch-all that used to sit at the end of `resolve` meant
        // a new button silently opened GitHub.  Assert the count
        // matches the named indices instead.
        assert_eq!(AboutModal::new().buttons.len(), VIEW_ON_GITHUB_BUTTON + 1);
    }

    #[test]
    fn the_body_is_built_for_the_columns_the_frame_will_have() {
        // `body_columns` is the ceiling `ModalView` can give the body:
        // the terminal less the minimum padding on each side.  The About
        // body drops its pod against that number, so getting it wrong
        // draws a pod into a frame too narrow to hold it.
        assert_eq!(body_columns(Rect::new(0, 0, 80, 24)), 80 - 2 * MIN_PAD_H);
        // A terminal narrower than the padding itself still asks for a
        // body rather than underflowing.
        assert_eq!(body_columns(Rect::new(0, 0, 1, 24)), 0);
    }

    #[test]
    fn the_tagline_keeps_asking_for_a_redraw() {
        let modal = AboutModal::new();
        let now = Instant::now();
        let d = modal.next_deadline().expect("tagline deadline");
        assert!(d > now && d <= now + TAGLINE_INTERVAL);
    }

    #[test]
    fn esc_dismisses() {
        let mut app = make_app();
        let mut modal = AboutModal::new();
        let outcome = modal.handle_key(key(KeyCode::Esc), &mut app, 24, 80);
        assert!(matches!(outcome, ModalOutcome::Close));
    }
}
