//! Update-check modal: the single surface that answers "is there a
//! newer edamame, and what changed?".
//!
//! One modal, five states, three entry points.  The startup check
//! (`app::update_notice`) pushes it only in the `Available` state and
//! only once nothing else is on screen; the About page's
//! `[ Check for updates ]` button and the `CheckForUpdates` palette
//! action push it in whatever state is known and let the in-flight
//! result replace that.  Keeping the up-to-date, uncomparable and
//! failure states on the *same* modal is what lets an explicit check
//! answer honestly — the startup path simply never opens it for those.
//!
//! Like [`super::about`], time-driven content (the spinner) is derived
//! from `opened_at.elapsed()` at render time rather than mutated by a
//! tick; [`Modal::next_deadline`] tells the run loop when to redraw,
//! and returns `None` once the status resolves so a settled modal
//! costs no wakeups.

use std::any::Any;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::update_check::{self, ReleaseStatus, INSTALLED_VERSION};
use crate::app::App;
use crate::ui::update_check::UpdateReport;
use crate::ui::{update_check as ui_update, ModalButton, ModalResponse, PROSE_CONTENT_WIDTH};

/// Spinner frame advance rate while a check is in flight.
const SPINNER_TICK: Duration = Duration::from_millis(100);

pub struct UpdateModal {
    chrome: ModalChrome,
    /// Rebuilt by [`Self::set_status`] alongside the status, because
    /// only the states naming a reachable release have anything to
    /// press — see [`buttons_for`].
    buttons: Vec<ModalButton>,
    status: ReleaseStatus,
    opened_at: Instant,
}

impl UpdateModal {
    /// `status` is whatever the session knows when the modal opens —
    /// `Pending` when a fetch was just spawned with nothing useful
    /// cached, otherwise the cached result, which the in-flight fetch
    /// replaces via [`Self::set_status`] when it lands.
    ///
    /// `pub(crate)` to match `ReleaseStatus`, which is crate-private.
    /// Nothing outside the crate constructs a modal.
    pub(crate) fn new(status: ReleaseStatus) -> Self {
        Self {
            // Prose body (the release notes are free text), so the
            // content width is capped — an unwrapped-longest-line sizing
            // would stretch the modal across the terminal.
            chrome: ModalChrome::new(ModalKind::Normal, true)
                .with_max_content_width(PROSE_CONTENT_WIDTH),
            buttons: buttons_for(&status),
            status,
            opened_at: Instant::now(),
        }
    }

    /// Push a resolved check into the open modal.  Called by
    /// `App::handle_release_check_result` when the worker reports back.
    pub(crate) fn set_status(&mut self, status: ReleaseStatus) {
        self.buttons = buttons_for(&status);
        self.status = status;
    }

    /// The status currently on display.  Read by the App-level tests
    /// that assert which state an entry point opened the modal in.
    #[cfg(test)]
    pub(crate) fn status(&self) -> &ReleaseStatus {
        &self.status
    }

    fn spinner_frame(&self) -> usize {
        (self.opened_at.elapsed().as_millis() / SPINNER_TICK.as_millis()) as usize
    }

    /// Map the status onto the `ui` layer's own vocabulary.  This
    /// translation is the whole reason `ui::update_check` needs no
    /// `app` import.
    fn report(&self) -> UpdateReport<'_> {
        match &self.status {
            ReleaseStatus::Pending => UpdateReport::Checking {
                spinner_frame: self.spinner_frame(),
            },
            ReleaseStatus::UpToDate { tag } => UpdateReport::UpToDate { tag },
            ReleaseStatus::Available(info) => UpdateReport::Available {
                tag: &info.tag,
                notes: &info.notes,
            },
            ReleaseStatus::Inconclusive { tag } => UpdateReport::Inconclusive { tag },
            ReleaseStatus::Failed => UpdateReport::Failed,
        }
    }

    /// Resolve a response — shared by the key and click paths so mouse
    /// and keyboard can't diverge.  `[ View on GitHub ]` keeps the modal
    /// open (`ContinueAnd`), matching the About page's button of the
    /// same name:
    /// the user comes back from the browser to what they were reading,
    /// not to a surprise dismissal.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::Close,
            ModalResponse::ButtonPressed(_) => {
                let Some(tag) = self.status.tag() else {
                    // No other state has a button; a stray press is
                    // not a reason to act on a URL we don't have.
                    return ModalOutcome::Continue;
                };
                let url = update_check::release_url(tag);
                ModalOutcome::ContinueAnd(Box::new(move |app| {
                    app.spawn_open_worker(url);
                }))
            }
        }
    }
}

/// A button for every state that names a release the user could go and
/// look at — `Available`, and `Inconclusive` for the same reason it
/// exists: the modal has just admitted it can't judge the tag, so the
/// release page is exactly where the answer is.  It opens *that
/// release's* page rather than the releases list, since the modal is
/// talking about one version and that page carries its install
/// instructions and downloads with no newer entries to scroll past.
fn buttons_for(status: &ReleaseStatus) -> Vec<ModalButton> {
    match status {
        ReleaseStatus::Available(_) | ReleaseStatus::Inconclusive { .. } => {
            vec![ModalButton::new("View on GitHub")]
        }
        _ => Vec::new(),
    }
}

impl Modal for UpdateModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let body = ui_update::body_lines(ctx.theme, self.report(), INSTALLED_VERSION);
        // One title for all five states: the body's first line is the
        // verdict, and a frame captioned "Update available" above a
        // line reading "Update available." says it twice.
        self.chrome
            .render(frame, area, ctx, "Check for updates", &body, &self.buttons);
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
        // Only the spinner animates, so a resolved modal asks for no
        // wakeups at all.  Saturating conversion: an absurdly long
        // session pins the deadline in the far future rather than
        // wrapping it into the past, where the run loop's `> now`
        // filter would silently drop it.
        if self.status != ReleaseStatus::Pending {
            return None;
        }
        let periods = self.opened_at.elapsed().as_nanos() / SPINNER_TICK.as_nanos();
        let periods = u32::try_from(periods).unwrap_or(u32::MAX);
        Some(self.opened_at + SPINNER_TICK * periods.saturating_add(1))
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
    use crate::app::update_check::ReleaseInfo;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn available() -> ReleaseStatus {
        ReleaseStatus::Available(ReleaseInfo {
            tag: "v999.0.0".to_owned(),
            notes: vec!["- a thing".to_owned()],
        })
    }

    #[test]
    fn only_an_available_release_offers_a_button() {
        assert_eq!(UpdateModal::new(available()).buttons.len(), 1);
        assert!(UpdateModal::new(ReleaseStatus::Pending).buttons.is_empty());
        assert!(UpdateModal::new(ReleaseStatus::Failed).buttons.is_empty());
        assert!(UpdateModal::new(ReleaseStatus::UpToDate {
            tag: "v0.1.0".to_owned()
        })
        .buttons
        .is_empty());
    }

    #[test]
    fn an_uncomparable_tag_still_offers_the_release_page() {
        // The modal has just said it can't judge the tag, so the one
        // place that answers the question must be reachable from it.
        let status = ReleaseStatus::Inconclusive {
            tag: "v999.0.0-rc1".to_owned(),
        };
        let mut app = make_app();
        let mut modal = UpdateModal::new(status);
        assert_eq!(modal.buttons.len(), 1);
        let outcome = modal.handle_key(key(KeyCode::Enter), &mut app, 24, 80);
        assert!(matches!(outcome, ModalOutcome::ContinueAnd(_)));
    }

    #[test]
    fn resolving_a_check_swaps_the_buttons_in() {
        let mut modal = UpdateModal::new(ReleaseStatus::Pending);
        assert!(modal.buttons.is_empty());
        modal.set_status(available());
        assert_eq!(modal.buttons.len(), 1, "the release became reachable");
    }

    #[test]
    fn the_view_on_github_button_keeps_the_modal_open() {
        let mut app = make_app();
        let mut modal = UpdateModal::new(available());
        let outcome = modal.handle_key(key(KeyCode::Enter), &mut app, 24, 80);
        assert!(matches!(outcome, ModalOutcome::ContinueAnd(_)));
    }

    #[test]
    fn esc_dismisses() {
        let mut app = make_app();
        let mut modal = UpdateModal::new(available());
        let outcome = modal.handle_key(key(KeyCode::Esc), &mut app, 24, 80);
        assert!(matches!(outcome, ModalOutcome::Close));
    }

    #[test]
    fn only_a_pending_check_asks_for_a_redraw() {
        let pending = UpdateModal::new(ReleaseStatus::Pending);
        let now = Instant::now();
        let deadline = pending.next_deadline().expect("spinner deadline");
        assert!(deadline > now && deadline <= now + SPINNER_TICK + Duration::from_millis(50));

        // Resolved: nothing animates, so no wakeup is requested.
        assert!(UpdateModal::new(available()).next_deadline().is_none());
        assert!(UpdateModal::new(ReleaseStatus::Failed)
            .next_deadline()
            .is_none());
    }
}
