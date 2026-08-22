//! Post-upgrade notice: edamame was updated, here is what changed.
//!
//! Two entry points, one body.  [`PostUpgradeModal::for_upgrade`] is
//! the once-per-upgrade startup notice `App::new` builds, and returns
//! `None` when the installed version has no changelog section — the
//! startup path stays silent rather than raising a modal with nothing
//! in it.  [`PostUpgradeModal::on_demand`] is the About page's
//! `[ Release notes ]` button, and always builds: an explicit request
//! is answered even when the answer is "there aren't any", the same
//! split [`super::update`] draws between the silent startup check and
//! the explicit one.
//!
//! Simpler than that modal in every other respect: the content is a
//! compiled-in string, so nothing animates, nothing arrives later, and
//! there is no button to press — hence no `next_deadline`, no
//! `set_status`, and an empty button slice.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::post_upgrade::changelog;
use crate::app::update_check::INSTALLED_VERSION;
use crate::app::App;
use crate::ui::update_check::{self as ui_update, PostUpgradeOccasion, PostUpgradeReport};
use crate::ui::{ModalResponse, PROSE_CONTENT_WIDTH};

pub struct PostUpgradeModal {
    chrome: ModalChrome,
    /// `None` means the installed version has no changelog section —
    /// distinct from `Some(vec![])`, a section that exists but says
    /// nothing.  Only the first is worth reporting as an absence.
    notes: Option<Vec<String>>,
    /// Which entry point built this one.  The body's opening line is
    /// the only thing it decides — see [`PostUpgradeOccasion`] — but it
    /// has to be carried on the modal, because `render` is the first
    /// place the two paths meet again.
    occasion: PostUpgradeOccasion,
}

impl PostUpgradeModal {
    /// The startup notice.  `None` when there is nothing to show, so
    /// the caller has no "should I?" test of its own to get wrong.
    pub(crate) fn for_upgrade() -> Option<Self> {
        Some(Self::new(
            Some(changelog::notes_for_version(INSTALLED_VERSION)?),
            PostUpgradeOccasion::Upgrade,
        ))
    }

    /// The About page's on-demand opening, which always has something
    /// to say even if that something is "no notes are bundled".
    pub(crate) fn on_demand() -> Self {
        Self::new(
            changelog::notes_for_version(INSTALLED_VERSION),
            PostUpgradeOccasion::OnDemand,
        )
    }

    fn new(notes: Option<Vec<String>>, occasion: PostUpgradeOccasion) -> Self {
        Self {
            // Prose body, so the content width is capped — an
            // unwrapped-longest-line sizing would stretch the modal
            // across the terminal.  Same reasoning as `UpdateModal`.
            chrome: ModalChrome::new(ModalKind::Normal, true)
                .with_max_content_width(PROSE_CONTENT_WIDTH),
            notes,
            occasion,
        }
    }

    /// Map onto the `ui` layer's own vocabulary — the translation that
    /// keeps `ui::update_check` free of any `app` import.
    fn report(&self) -> PostUpgradeReport<'_> {
        match &self.notes {
            Some(notes) => PostUpgradeReport::Found { notes },
            None => PostUpgradeReport::NotFound,
        }
    }

    /// Shared by the key and click paths so mouse and keyboard can't
    /// diverge.  There are no buttons, so every response that isn't a
    /// scroll closes.
    fn resolve(response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }
}

impl Modal for PostUpgradeModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let body = ui_update::post_upgrade_body_lines(
            ctx.theme,
            self.occasion,
            self.report(),
            INSTALLED_VERSION,
        );
        self.chrome
            .render(frame, area, ctx, "Release notes", &body, &[]);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        Self::resolve(self.chrome.on_key(&key, 0))
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.chrome.on_wheel(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        Self::resolve(self.chrome.on_click(col, row))
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::make_app;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn with_notes() -> PostUpgradeModal {
        PostUpgradeModal::new(
            Some(vec!["- a thing".to_owned()]),
            PostUpgradeOccasion::Upgrade,
        )
    }

    #[test]
    fn esc_dismisses() {
        let mut app = make_app();
        let mut modal = with_notes();
        assert!(matches!(
            modal.handle_key(key(KeyCode::Esc), &mut app, 24, 80),
            ModalOutcome::Close
        ));
    }

    #[test]
    fn enter_does_nothing_since_there_is_nothing_to_press() {
        // The button-less shape `NoticeModal` established: with no
        // footer row, Enter has no target and Esc is the way out.
        let mut app = make_app();
        let mut modal = with_notes();
        assert!(matches!(
            modal.handle_key(key(KeyCode::Enter), &mut app, 24, 80),
            ModalOutcome::Continue
        ));
    }

    #[test]
    fn a_missing_section_and_an_empty_one_are_different_states() {
        // `None` is "this version has no entry"; `Some(vec![])` is "it
        // has one and it is empty".  Only the first is reported as an
        // absence, so the two must not be collapsed.
        assert!(matches!(
            PostUpgradeModal::new(None, PostUpgradeOccasion::OnDemand).report(),
            PostUpgradeReport::NotFound
        ));
        assert!(matches!(
            PostUpgradeModal::new(Some(Vec::new()), PostUpgradeOccasion::OnDemand).report(),
            PostUpgradeReport::Found { .. }
        ));
    }

    #[test]
    fn the_on_demand_opening_always_builds_a_modal() {
        // Whatever the bundled changelog happens to say about the
        // version under test — including nothing at all, which is the
        // normal state between releases.
        let modal = PostUpgradeModal::on_demand();
        assert!(modal.dismissable());
        assert_eq!(modal.kind(), ModalKind::Normal);
    }

    #[test]
    fn each_entry_point_carries_its_own_occasion() {
        // The About page's opening must not announce an upgrade, and
        // the startup notice must; the body is otherwise identical, so
        // this field is the whole difference between them.
        assert_eq!(
            PostUpgradeModal::on_demand().occasion,
            PostUpgradeOccasion::OnDemand
        );
        if let Some(modal) = PostUpgradeModal::for_upgrade() {
            assert_eq!(modal.occasion, PostUpgradeOccasion::Upgrade);
        }
    }
}
