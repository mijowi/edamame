//! About edamame popover: bean art, rotating acronym tagline, version
//! info (installed + latest GitHub release), author credit, and a
//! `[ View on GitHub ]` button.
//!
//! Time-driven content (the tagline rotation and the release-check
//! spinner) is *derived* from `opened_at.elapsed()` at render time
//! rather than mutated by a tick — the modal just reports when the next
//! visual change is due via [`Modal::next_deadline`], and the run loop
//! wakes and redraws then.  The release result is pushed in by
//! `App::handle_async_event` via `ModalStack::find_first_mut`, the same
//! route the dirty-conflict modal uses for late-arriving data.

use std::any::Any;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::update_check::{self, ReleaseStatus};
use crate::app::App;
use crate::ui::{about, ModalButton, ModalResponse};

const INSTALLED_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long each acronym expansion stays up before rotating.
const TAGLINE_INTERVAL: Duration = Duration::from_secs(4);

/// Spinner frame advance rate while the release check is pending.
const SPINNER_TICK: Duration = Duration::from_millis(100);

pub struct AboutModal {
    chrome: ModalChrome,
    buttons: Vec<ModalButton>,
    opened_at: Instant,
    /// Offset into [`about::TAGLINES`] so the page doesn't open on the
    /// same expansion every time.
    tagline_start: usize,
    release: ReleaseStatus,
}

impl AboutModal {
    /// `release` is the App's session cache — [`ReleaseStatus::Pending`]
    /// on the first open (the caller spawns the fetch), the cached
    /// result on every reopen.
    pub fn new(release: ReleaseStatus) -> Self {
        // Vary the opening tagline without a rand dependency: the
        // sub-second nanos of the wall clock are effectively uniform
        // across user-initiated opens.
        let tagline_start = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize % about::TAGLINES.len())
            .unwrap_or(0);
        Self {
            chrome: ModalChrome::new(ModalKind::Normal, true),
            buttons: vec![ModalButton::new("View on GitHub")],
            opened_at: Instant::now(),
            tagline_start,
            release,
        }
    }

    /// Push the resolved release check into the open modal.  Called by
    /// `App::handle_async_event` when the worker reports back.
    pub(crate) fn set_release(&mut self, release: ReleaseStatus) {
        self.release = release;
    }

    fn tagline_index(&self) -> usize {
        let flips = self.opened_at.elapsed().as_secs() / TAGLINE_INTERVAL.as_secs();
        self.tagline_start + flips as usize
    }

    fn spinner_frame(&self) -> usize {
        (self.opened_at.elapsed().as_millis() / SPINNER_TICK.as_millis()) as usize
    }

    /// The "Current release" text, or `None` while the fetch is in
    /// flight (the body renders the spinner instead).
    fn release_display(&self) -> Option<String> {
        match &self.release {
            ReleaseStatus::Pending => None,
            ReleaseStatus::Available(tag) => Some(format!(
                "{tag}{}",
                update_check::release_suffix(INSTALLED_VERSION, tag)
            )),
            ReleaseStatus::Failed => Some("unavailable".to_owned()),
        }
    }

    /// Map a resolved response to an outcome — shared by the key and
    /// click paths.  The GitHub button keeps the modal open
    /// (`ContinueAnd`) so the user returns from the browser to the
    /// About page, not to a surprise dismissal.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::Close,
            ModalResponse::ButtonPressed(_) => ModalOutcome::ContinueAnd(Box::new(|app| {
                app.spawn_open_worker(update_check::GITHUB_URL.to_owned());
            })),
        }
    }
}

impl Modal for AboutModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let release = self.release_display();
        let body = about::body_lines(
            ctx.theme,
            self.tagline_index(),
            self.spinner_frame(),
            INSTALLED_VERSION,
            release.as_deref(),
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

    fn next_deadline(&self) -> Option<Instant> {
        // Next boundary of an `interval`-spaced grid anchored at
        // `opened_at`.  Saturating conversion: an (absurdly) long
        // session pins the deadline at the far future instead of
        // wrapping it into the past, where the run loop's
        // `> now` filter would silently drop it.
        let next_boundary = |interval: Duration| {
            let periods = self.opened_at.elapsed().as_nanos() / interval.as_nanos();
            let periods = u32::try_from(periods).unwrap_or(u32::MAX);
            self.opened_at + interval * periods.saturating_add(1)
        };
        let mut next = next_boundary(TAGLINE_INTERVAL);
        if self.release == ReleaseStatus::Pending {
            next = next.min(next_boundary(SPINNER_TICK));
        }
        Some(next)
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
    use crate::app::AppEvent;

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
    fn release_result_event_updates_modal_and_session_cache() {
        let mut app = make_app();
        app.open_about_modal();
        app.handle_async_event(AppEvent::ReleaseCheckResult(Ok("v9.9.9".to_owned())));
        let modal = app.modal_stack.find_first_mut::<AboutModal>().unwrap();
        assert_eq!(modal.release, ReleaseStatus::Available("v9.9.9".to_owned()));
        assert_eq!(
            modal.release_display().as_deref(),
            Some("v9.9.9 (update available)"),
        );
        // Reopening picks the cached result up instead of refetching.
        app.modal_stack.remove_first::<AboutModal>();
        app.open_about_modal();
        let modal = app.modal_stack.find_first_mut::<AboutModal>().unwrap();
        assert_eq!(modal.release, ReleaseStatus::Available("v9.9.9".to_owned()));
    }

    #[test]
    fn failed_release_check_shows_unavailable() {
        let mut app = make_app();
        app.open_about_modal();
        app.handle_async_event(AppEvent::ReleaseCheckResult(Err("404".to_owned())));
        let modal = app.modal_stack.find_first_mut::<AboutModal>().unwrap();
        assert_eq!(modal.release_display().as_deref(), Some("unavailable"));
    }

    #[test]
    fn next_deadline_is_sooner_while_spinner_is_pending() {
        let pending = AboutModal::new(ReleaseStatus::Pending);
        let resolved = AboutModal::new(ReleaseStatus::Failed);
        let now = Instant::now();
        // Pending: next wake is the ~100 ms spinner tick.
        let d = pending.next_deadline().expect("pending deadline");
        assert!(d > now && d <= now + SPINNER_TICK + Duration::from_millis(50));
        // Resolved: only the ~4 s tagline flip remains.
        let d = resolved.next_deadline().expect("resolved deadline");
        assert!(d > now + SPINNER_TICK && d <= now + TAGLINE_INTERVAL);
    }

    #[test]
    fn enter_on_github_button_keeps_modal_open() {
        let mut app = make_app();
        let mut modal = AboutModal::new(ReleaseStatus::Pending);
        let outcome = modal.handle_key(key(KeyCode::Enter), &mut app, 24, 80);
        assert!(matches!(outcome, ModalOutcome::ContinueAnd(_)));
    }

    #[test]
    fn esc_dismisses() {
        let mut app = make_app();
        let mut modal = AboutModal::new(ReleaseStatus::Pending);
        let outcome = modal.handle_key(key(KeyCode::Esc), &mut app, 24, 80);
        assert!(matches!(outcome, ModalOutcome::Close));
    }
}
