//! App-level orchestration for the update check: when to spawn one,
//! what to do with the answer, and when a startup finding is allowed to
//! interrupt.
//!
//! [`super::update_check`] is the pure fetch/parse/policy leaf; this is
//! the plumbing around it, in its own file for the same reason
//! `autosave.rs` and `diff_advance.rs` are — it is a small state
//! machine with its own timing, and `actions.rs` is already large.
//!
//! The one genuinely awkward part is *when* the startup notice may
//! appear.  Every other startup modal is built synchronously in
//! `App::new` and pushed in one deliberate priority order; this one
//! depends on a network result that arrives long after that ordering
//! has been decided, so it cannot join the batch.  Instead a finding is
//! parked in `pending_update_notice` and [`App::tick_update_notice`]
//! pushes it on the first frame the modal stack is empty — which is a
//! member of the existing `tick_timers` family, not a new per-frame
//! mechanism.  Gating on "stack is empty" rather than on "no welcome
//! modal" is what makes it robust: it cannot stomp the first-run
//! welcome, a config warning, the capabilities notice, or anything
//! added later, without needing to know any of them exist.

use super::modal;
use super::update_check::{self, ReleaseInfo, ReleaseStatus};
use super::App;

impl App {
    // ── Spawning ───────────────────────────────────────────────────────────

    /// Run the automatic check, if [`App::new`] decided one was due.
    ///
    /// A member of the `tick_timers` family rather than a one-shot call
    /// from `run()`, because it may have to wait: the first-run welcome
    /// modal is where the user answers the `check_for_updates`
    /// question, and firing the request before they have answered would
    /// make the request the setting exists to gate — the one launch
    /// where consent is actually asked for would be the one launch that
    /// asks too late.  So while that modal is up the decision stays
    /// parked, and the setting is re-read *after* it closes so a
    /// decline is honored on the very first run.
    ///
    /// Only the welcome gates it; the config warning and the
    /// capabilities notice are not consent surfaces for this and a
    /// check behind them is silent either way.
    pub(super) fn spawn_startup_update_check(&mut self) {
        if !self.startup_update_check_due {
            return;
        }
        if self.modal_stack.contains::<modal::WelcomeModal>() {
            return;
        }
        self.startup_update_check_due = false;
        // Re-read rather than trusting the decision `App::new` made:
        // the welcome that just closed may have turned the check off.
        if !self.config.editor.check_for_updates {
            return;
        }
        if !self.spawn_release_check_tracked(true) {
            return;
        }
        // Stamped at *spawn*, not on arrival.  A worker that hangs
        // until the timeout, or a process killed before the result
        // lands, would otherwise leave the clock untouched and re-check
        // on every single launch — the retry storm the throttle exists
        // to prevent.  The cost is that a transient failure waits out
        // the full interval before trying again, which is the right
        // trade for a notification nobody is waiting on.
        self.config.editor.last_update_check = update_check::now_unix();
        self.save_update_bookkeeping("last-update-check timestamp");
    }

    /// Spawn a check unless one is already in flight.  Returns whether
    /// a worker was actually started.  The single spawn site, so
    /// `release_check_in_flight` and `update_check_is_startup` can't
    /// disagree about what is running.
    fn spawn_release_check_tracked(&mut self, is_startup: bool) -> bool {
        if self.release_check_in_flight {
            return false;
        }
        let Some(tx) = self.app_tx.clone() else {
            return false;
        };
        update_check::spawn_release_check(tx);
        self.release_check_in_flight = true;
        self.update_check_is_startup = is_startup;
        true
    }

    // ── Result ─────────────────────────────────────────────────────────────

    /// Route a resolved release check.  Replaces what used to be an
    /// inline arm in `handle_async_event`.
    pub(super) fn handle_release_check_result(&mut self, result: Result<ReleaseInfo, String>) {
        self.release_check_in_flight = false;
        let was_startup = std::mem::take(&mut self.update_check_is_startup);
        if let Err(msg) = &result {
            tracing::debug!(target: "update_check", %msg, "release check failed");
        }
        let status = ReleaseStatus::from_fetch(result);
        self.latest_release = Some(status.clone());

        // An open modal is showing this result live — the explicit
        // path, or a startup notice the user has already been handed.
        let mut on_screen = false;
        if let Some(open) = self.modal_stack.find_first_mut::<modal::UpdateModal>() {
            open.set_status(status.clone());
            on_screen = true;
        }

        if on_screen {
            // Already told, by definition.  Recording it here keeps
            // "at most one notice per version" true across the
            // explicit path too, so a user who checked by hand isn't
            // greeted by the same news at the next launch.
            self.mark_update_notified(&status);
        } else if was_startup {
            if let Some(info) =
                update_check::notice_due(&status, &self.config.editor.update_notified_for)
            {
                self.pending_update_notice = Some(info.clone());
            }
        }
        self.needs_draw = true;
    }

    /// Push a parked startup finding once nothing else is on screen.
    /// A no-op on every frame but the one where that first becomes
    /// true, in the shape of the other `tick_*` members.
    pub(super) fn tick_update_notice(&mut self) {
        if self.pending_update_notice.is_none() || !self.modal_stack.is_empty() {
            return;
        }
        let Some(info) = self.pending_update_notice.take() else {
            return;
        };
        let status = ReleaseStatus::Available(info);
        self.mark_update_notified(&status);
        self.modal_stack
            .push(Box::new(modal::UpdateModal::new(status)));
        self.needs_draw = true;
    }

    // ── Explicit check ─────────────────────────────────────────────────────

    /// Open the update modal on demand — the About page's
    /// `[ Check for updates ]` button and the `CheckForUpdates`
    /// palette action both land here.
    ///
    /// An explicit request always re-fetches, ignoring the daily
    /// throttle: that gate bounds unattended chatter, and this is the
    /// opposite of unattended.  A cached *positive* result is shown
    /// meanwhile so the modal isn't blank — including an `Inconclusive`
    /// one, which reached GitHub and has both version numbers to show.
    /// A cached *failure* is not, because re-showing "couldn't check"
    /// while a fresh attempt is already running would be answering a
    /// question the user just asked again with a stale answer.
    pub fn open_update_modal(&mut self) {
        if self.modal_stack.contains::<modal::UpdateModal>() {
            return;
        }
        self.spawn_release_check_tracked(false);
        // A queued startup notice is superseded — the user is about to
        // see the same thing, and it must not reappear afterwards.
        self.pending_update_notice = None;

        let status = match self.latest_release.clone() {
            Some(
                cached @ (ReleaseStatus::UpToDate { .. }
                | ReleaseStatus::Available(_)
                | ReleaseStatus::Inconclusive { .. }),
            ) => cached,
            _ => ReleaseStatus::Pending,
        };
        self.mark_update_notified(&status);
        self.modal_stack
            .push(Box::new(modal::UpdateModal::new(status)));
        self.needs_draw = true;
    }

    // ── Bookkeeping ────────────────────────────────────────────────────────

    /// Record that the user has been shown this release, so the
    /// startup notice doesn't repeat it.  A no-op for every status but
    /// `Available` — there is nothing to suppress about good news.
    fn mark_update_notified(&mut self, status: &ReleaseStatus) {
        let ReleaseStatus::Available(info) = status else {
            return;
        };
        if self.config.editor.update_notified_for == info.tag {
            return;
        }
        self.config.editor.update_notified_for = info.tag.clone();
        self.save_update_bookkeeping("update-notified tag");
    }

    /// Persist background bookkeeping *without* the "Configuration
    /// updated" flash `save_config_with_flash` raises: the user changed
    /// no setting, and a toast for a timestamp write is pure noise.
    /// Under `--no-config` `Config::save` already declines to write, so
    /// this needs no gate of its own — the session simply re-checks on
    /// the next launch, which is what that flag promises.
    fn save_update_bookkeeping(&mut self, what: &str) {
        if let Err(e) = self.config.save() {
            tracing::warn!(
                target: "update_check",
                error = %e,
                field = what,
                "failed to persist update-check bookkeeping",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_utils::make_app;
    use crate::app::AppEvent;

    /// A `make_app()` plus the config-isolation guard, as one call.
    ///
    /// Every test here drives a path that can reach `Config::save`
    /// (`mark_update_notified` and the startup stamp both persist), and
    /// nothing redirects `~/.config/edamame` in a test run — so an
    /// unguarded test would rewrite the developer's own config, and
    /// leave `update_notified_for = "v999.0.0"` in it, silencing the
    /// real update notice for good.  Returned as a tuple so the guard
    /// outlives the test body: `let (_iso, mut app) = isolated_app();`.
    fn isolated_app() -> (crate::test_env::ConfigIsolation, App) {
        let iso = crate::test_env::config_isolation();
        let app = make_app();
        (iso, app)
    }

    fn info(tag: &str) -> ReleaseInfo {
        ReleaseInfo {
            tag: tag.to_owned(),
            notes: vec!["- a thing".to_owned()],
        }
    }

    /// A tag no real build will reach, so `is_newer` says yes whatever
    /// `CARGO_PKG_VERSION` happens to be.
    const NEWER: &str = "v999.0.0";

    #[test]
    fn a_startup_result_with_a_newer_release_queues_a_notice() {
        let (_iso, mut app) = isolated_app();
        app.update_check_is_startup = true;
        app.handle_release_check_result(Ok(info(NEWER)));
        assert_eq!(app.pending_update_notice, Some(info(NEWER)));
        // Queued, not pushed — the stack decision belongs to the tick.
        assert!(!app.modal_stack.contains::<modal::UpdateModal>());
    }

    #[test]
    fn an_explicit_result_never_queues_a_notice() {
        let (_iso, mut app) = isolated_app();
        app.update_check_is_startup = false;
        app.handle_release_check_result(Ok(info(NEWER)));
        assert_eq!(app.pending_update_notice, None);
        assert!(app.latest_release.is_some(), "still cached for reuse");
    }

    #[test]
    fn an_up_to_date_or_failed_startup_result_stays_silent() {
        let (_iso, mut app) = isolated_app();
        app.update_check_is_startup = true;
        app.handle_release_check_result(Ok(info("v0.0.1")));
        assert_eq!(app.pending_update_notice, None);

        app.update_check_is_startup = true;
        app.handle_release_check_result(Err("offline".to_owned()));
        assert_eq!(app.pending_update_notice, None);
    }

    #[test]
    fn a_tag_already_notified_about_does_not_queue_again() {
        let (_iso, mut app) = isolated_app();
        app.config.editor.update_notified_for = NEWER.to_owned();
        app.update_check_is_startup = true;
        app.handle_release_check_result(Ok(info(NEWER)));
        assert_eq!(app.pending_update_notice, None);
    }

    /// Dismiss whatever `App::new` queued (a fresh `Config` has
    /// `show_welcome`, so the first-run welcome modal is always there)
    /// to reach the "nothing on screen" state the notice waits for.
    fn clear_modals(app: &mut App) {
        while !app.modal_stack.is_empty() {
            app.modal_stack.pop();
        }
    }

    #[test]
    fn the_notice_waits_for_an_empty_modal_stack() {
        let (_iso, mut app) = isolated_app();
        // Exactly the case this mechanism exists for: a first run,
        // where the welcome modal is already up when the check lands.
        assert!(
            app.modal_stack.contains::<modal::WelcomeModal>(),
            "a default config puts the welcome modal on the stack"
        );
        app.pending_update_notice = Some(info(NEWER));

        app.tick_update_notice();
        assert!(
            !app.modal_stack.contains::<modal::UpdateModal>(),
            "must not stack on top of the welcome modal"
        );
        assert!(app.pending_update_notice.is_some(), "still parked");

        clear_modals(&mut app);
        app.tick_update_notice();
        assert!(app.modal_stack.contains::<modal::UpdateModal>());
        assert_eq!(app.pending_update_notice, None);
    }

    #[test]
    fn pushing_the_notice_records_the_tag_so_it_fires_once() {
        let (_iso, mut app) = isolated_app();
        clear_modals(&mut app);
        app.pending_update_notice = Some(info(NEWER));
        app.tick_update_notice();
        assert_eq!(app.config.editor.update_notified_for, NEWER);
    }

    #[test]
    fn a_result_seen_in_an_open_modal_counts_as_told() {
        let (_iso, mut app) = isolated_app();
        app.open_update_modal();
        assert!(app.modal_stack.contains::<modal::UpdateModal>());
        app.handle_release_check_result(Ok(info(NEWER)));
        assert_eq!(
            app.config.editor.update_notified_for, NEWER,
            "an explicit check the user watched should not re-notify next launch"
        );
        assert_eq!(app.pending_update_notice, None);
    }

    #[test]
    fn opening_the_modal_supersedes_a_queued_notice() {
        let (_iso, mut app) = isolated_app();
        app.pending_update_notice = Some(info(NEWER));
        app.open_update_modal();
        assert_eq!(app.pending_update_notice, None);
        // And the tick can't resurrect it once the modal is dismissed.
        clear_modals(&mut app);
        app.tick_update_notice();
        assert!(!app.modal_stack.contains::<modal::UpdateModal>());
    }

    #[test]
    fn opening_the_modal_twice_does_not_stack_it() {
        let (_iso, mut app) = isolated_app();
        app.open_update_modal();
        app.open_update_modal();
        assert_eq!(app.modal_stack.count::<modal::UpdateModal>(), 1);
    }

    #[test]
    fn a_cached_failure_reopens_as_pending_rather_than_stale() {
        let (_iso, mut app) = isolated_app();
        app.latest_release = Some(ReleaseStatus::Failed);
        app.open_update_modal();
        let m = app
            .modal_stack
            .find_first_mut::<modal::UpdateModal>()
            .expect("modal");
        assert_eq!(m.status(), &ReleaseStatus::Pending);
    }

    #[test]
    fn a_cached_success_renders_while_the_refetch_runs() {
        let (_iso, mut app) = isolated_app();
        let cached = ReleaseStatus::UpToDate {
            tag: "v0.1.0".to_owned(),
        };
        app.latest_release = Some(cached.clone());
        app.open_update_modal();
        let m = app
            .modal_stack
            .find_first_mut::<modal::UpdateModal>()
            .expect("modal");
        assert_eq!(m.status(), &cached);
    }

    #[test]
    fn the_release_event_routes_through_the_handler() {
        let (_iso, mut app) = isolated_app();
        app.handle_async_event(AppEvent::ReleaseCheckResult(Ok(info(NEWER))));
        assert_eq!(
            app.latest_release,
            Some(ReleaseStatus::Available(info(NEWER)))
        );
    }

    #[test]
    fn a_startup_check_that_is_not_due_never_spawns() {
        let (_iso, mut app) = isolated_app();
        app.startup_update_check_due = false;
        app.spawn_startup_update_check();
        assert!(!app.release_check_in_flight);
    }

    #[test]
    fn the_startup_check_waits_for_the_first_run_welcome() {
        // The welcome modal is where `check_for_updates` is asked, so
        // the request must not have gone out before it is answered.
        let (_iso, mut app) = isolated_app();
        assert!(app.modal_stack.contains::<modal::WelcomeModal>());
        app.startup_update_check_due = true;

        app.spawn_startup_update_check();
        assert!(
            app.startup_update_check_due,
            "still parked behind the welcome"
        );
        assert!(!app.release_check_in_flight);
    }

    #[test]
    fn declining_on_the_welcome_cancels_the_startup_check() {
        // …and the answer is read *after* the modal closes, so a
        // first-run decline is honored on that same launch.
        let (_iso, mut app) = isolated_app();
        app.startup_update_check_due = true;
        clear_modals(&mut app);
        app.config.editor.check_for_updates = false;

        app.spawn_startup_update_check();
        assert!(!app.startup_update_check_due, "decision consumed");
        assert!(!app.release_check_in_flight, "and nothing was requested");
        assert_eq!(
            app.config.editor.last_update_check, 0,
            "an un-run check stamps no clock"
        );
    }
}
