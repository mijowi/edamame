//! The one-time post-upgrade notice: this build is new, here is what
//! changed in it.
//!
//! A different question from the one [`super::update_check`] answers,
//! and deliberately not built on it.  That module asks whether a
//! *newer* release exists on GitHub; this one asks whether *this*
//! build is newer than the one that last ran, and reads the answer out
//! of the changelog compiled into the binary.  No network, no consent
//! setting, no throttle — so none of `ReleaseStatus`'s vocabulary
//! fits: every one of its variants names the outcome of a fetch, and
//! nothing here fetches anything.
//!
//! **Synchronous, not park-and-tick.**  `update_notice` parks its
//! finding in an `Option` and pushes it from `tick_timers` because a
//! network result arrives long after `App::new` has decided the
//! startup modal ordering.  Here the config, the changelog and
//! `CARGO_PKG_VERSION` are all in hand inside the constructor, so the
//! modal joins that ordering directly and needs no `App` field, no
//! per-frame poll, and no "wait for the welcome" carve-out.
//!
//! **The decision happens in `App::new`; the write does not.**
//! `test_utils::make_app` builds an `App` through `App::new`, and most
//! tests that call it hold no `config_isolation()` guard — so a
//! `Config::save` there would rewrite the developer's own
//! `config.toml` on every `cargo test` run, which is the exact hazard
//! AGENTS.md's config-isolation rule exists to prevent.
//! [`App::stamp_last_version_seen`] therefore runs from `App::run`,
//! which only a real session reaches.  Deciding is free of that
//! problem: [`crate::config::persistence::config_writes_allowed`] is
//! an atomic load, not I/O.

pub(crate) mod changelog;

use super::modal;
use super::update_check::INSTALLED_VERSION;
use super::App;
use crate::config::persistence;

/// What this launch owes the user about the version it is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostUpgradeAction {
    /// The recorded version is the running one: nothing happened.
    Nothing,
    /// A first run.  Record the version so the *next* upgrade is
    /// recognizable, but say nothing — there is no "what's new" about
    /// a version the user has never run anything else.
    StampSilently,
    /// The build changed under a user who has been here before.
    Show,
}

/// Decide what an upgrade is owed, as a pure function of primitives —
/// the shape [`super::update_check::policy`] uses, and for the same
/// reason: the rule a user actually feels belongs somewhere it can be
/// read and table-tested without constructing an `App`.
///
/// The interesting case is an **empty** `last_version_seen`, which is
/// ambiguous: it is what a genuinely fresh install has, and also what
/// an upgrade *from a build that predates this field* has.  Guessing
/// wrong is visible either way — greet a brand-new user with the
/// release notes for a version they have never run, or silently eat
/// the first notice for every existing user.  `show_welcome`
/// disambiguates, because only somebody who has been here before could
/// have turned it off.
///
/// Nothing about this is limited to that migration: once a real
/// version is recorded, every later upgrade takes the non-empty branch
/// and is shown, so there is no second code path to delete once the
/// window closes.
pub(crate) fn post_upgrade_action(
    last_version_seen: &str,
    installed: &str,
    show_welcome: bool,
) -> PostUpgradeAction {
    if last_version_seen == installed {
        return PostUpgradeAction::Nothing;
    }
    if last_version_seen.is_empty() && show_welcome {
        return PostUpgradeAction::StampSilently;
    }
    PostUpgradeAction::Show
}

/// Build the startup notice, if this launch is owed one.
///
/// A free function rather than a method: `App::new` calls it while
/// still assembling itself, so there is no `self` yet — the same shape
/// as the other optional startup modals it sits beside.
///
/// Refused outright when config writes are suppressed (`--no-config`).
/// The stamp is what makes this notice *one-time*, and under that flag
/// it cannot persist — so showing it anyway would raise the same modal
/// on every single launch, which is worse than staying quiet. The gate
/// is asked here rather than left to `Config::save`'s own refusal, so
/// the decision is honestly suppressed instead of accidentally correct.
pub(crate) fn startup_notice(
    last_version_seen: &str,
    show_welcome: bool,
) -> Option<modal::PostUpgradeModal> {
    if !persistence::config_writes_allowed() {
        return None;
    }
    match post_upgrade_action(last_version_seen, INSTALLED_VERSION, show_welcome) {
        PostUpgradeAction::Show => modal::PostUpgradeModal::for_upgrade(),
        PostUpgradeAction::Nothing | PostUpgradeAction::StampSilently => None,
    }
}

impl App {
    /// Record the version this session is running, so the notice fires
    /// once per upgrade.
    ///
    /// Called from `App::run`, not `App::new` — see the module doc for
    /// why the constructor must not write.  The stamp is unconditional
    /// on whether a modal was actually shown: a release cut without a
    /// matching changelog section is silent, and if that silence left
    /// the version unrecorded it would be re-evaluated on every later
    /// launch, turning "nothing to say" into a permanent one.
    ///
    /// Needs no `--no-config` gate of its own; `Config::save` already
    /// declines there, and [`startup_notice`] has independently
    /// refused to show anything, so that session simply carries on
    /// with nothing recorded and nothing shown.
    pub(super) fn stamp_last_version_seen(&mut self) {
        if self.config.editor.last_version_seen == INSTALLED_VERSION {
            return;
        }
        self.config.editor.last_version_seen = INSTALLED_VERSION.to_owned();
        self.save_update_bookkeeping("last-version-seen");
    }

    /// Open the release notes on demand — the About page's
    /// `[ Release notes ]` button.
    ///
    /// Always shows the modal, including when the installed version
    /// has no changelog section, mirroring the rule the explicit
    /// update check follows: an unattended check may stay silent about
    /// an inconclusive answer, but a question the user just asked gets
    /// answered.  It reads no bookkeeping and writes none — looking is
    /// not the same as having been notified, so this neither arms nor
    /// disarms the startup notice.
    pub fn open_post_upgrade_modal(&mut self) {
        if self.modal_stack.contains::<modal::PostUpgradeModal>() {
            return;
        }
        self.modal_stack
            .push(Box::new(modal::PostUpgradeModal::on_demand()));
        self.needs_draw = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_utils::make_app;

    const OLDER: &str = "0.0.1";

    #[test]
    fn the_running_version_owes_nothing() {
        assert_eq!(
            post_upgrade_action(INSTALLED_VERSION, INSTALLED_VERSION, false),
            PostUpgradeAction::Nothing
        );
        // …and a pending welcome doesn't change that: the version is
        // already recorded, so this is not a first run whatever the
        // flag says.
        assert_eq!(
            post_upgrade_action(INSTALLED_VERSION, INSTALLED_VERSION, true),
            PostUpgradeAction::Nothing
        );
    }

    #[test]
    fn a_fresh_install_is_stamped_but_not_greeted() {
        assert_eq!(
            post_upgrade_action("", INSTALLED_VERSION, true),
            PostUpgradeAction::StampSilently
        );
    }

    #[test]
    fn an_upgrade_from_before_the_field_existed_is_shown() {
        // Same empty string as a fresh install; `show_welcome` off is
        // the only thing separating them, because only a returning
        // user could have turned it off.
        assert_eq!(
            post_upgrade_action("", INSTALLED_VERSION, false),
            PostUpgradeAction::Show
        );
    }

    #[test]
    fn an_ordinary_upgrade_is_shown_whatever_the_welcome_says() {
        // Once a real version is recorded the welcome flag stops
        // mattering — the migration case is the only one it decides.
        assert_eq!(
            post_upgrade_action(OLDER, INSTALLED_VERSION, false),
            PostUpgradeAction::Show
        );
        assert_eq!(
            post_upgrade_action(OLDER, INSTALLED_VERSION, true),
            PostUpgradeAction::Show
        );
    }

    #[test]
    fn a_downgrade_is_shown_too() {
        // Running an older build than the one recorded is still a
        // change of build, and the notes shown are the running
        // version's own.  Reachable by checking out an old tag; not
        // worth a state of its own.
        assert_eq!(
            post_upgrade_action("999.0.0", INSTALLED_VERSION, false),
            PostUpgradeAction::Show
        );
    }

    #[test]
    fn no_config_refuses_the_notice_outright() {
        // The stamp can't persist under `--no-config`, so a notice
        // shown there would repeat on every launch.  Guarded by the
        // same suppression the config-isolation helper uses, which is
        // why this asks `startup_notice` rather than building an App.
        let _iso = crate::test_env::config_isolation();
        assert!(
            startup_notice("", false).is_none(),
            "writes are suppressed, so nothing may be shown"
        );
    }

    #[test]
    fn stamping_records_the_running_version() {
        let _iso = crate::test_env::config_isolation();
        let mut app = make_app();
        app.config.editor.last_version_seen = OLDER.to_owned();
        app.stamp_last_version_seen();
        assert_eq!(app.config.editor.last_version_seen, INSTALLED_VERSION);
    }

    #[test]
    fn stamping_an_already_current_version_changes_nothing() {
        let _iso = crate::test_env::config_isolation();
        let mut app = make_app();
        app.config.editor.last_version_seen = INSTALLED_VERSION.to_owned();
        app.stamp_last_version_seen();
        assert_eq!(app.config.editor.last_version_seen, INSTALLED_VERSION);
    }

    #[test]
    fn the_explicit_opening_writes_no_bookkeeping() {
        // Looking at the notes is not being notified about them.
        let _iso = crate::test_env::config_isolation();
        let mut app = make_app();
        app.config.editor.last_version_seen = OLDER.to_owned();
        app.open_post_upgrade_modal();
        assert!(app.modal_stack.contains::<modal::PostUpgradeModal>());
        assert_eq!(app.config.editor.last_version_seen, OLDER);
    }

    /// Build an `App` the way a returning user's launch does: welcome
    /// already dismissed, and `last_version_seen` as given.
    ///
    /// **The caller must hold [`crate::test_env::env_lock`] — and not
    /// `config_isolation` — for the whole test.** `startup_notice`
    /// reads the process-global write gate, and `config_isolation`
    /// clears exactly that flag, so isolating this test would gate away
    /// the behaviour under test; but another test holding the guard
    /// concurrently would do the same thing from the outside, which is
    /// what the bare lock excludes. Safe without the suppression
    /// because `App::new` performs no write — the reason the stamp
    /// lives in `App::run` — and nothing here calls it.
    fn returning_user_app(last_version_seen: &str) -> App {
        use crate::config::{Config, KeyBindingOverrides, Theme};
        use crate::terminal::{Capabilities, ColorDepth};

        let caps = Capabilities {
            color_depth: ColorDepth::TrueColor,
            ..Capabilities::default()
        };
        let mut config = Config::default();
        config.editor.show_welcome = false;
        config.editor.last_version_seen = last_version_seen.to_owned();
        App::new(
            config,
            KeyBindingOverrides::default(),
            (&Theme::default()).into(),
            None,
            caps,
            Vec::new(),
        )
        .expect("build app")
    }

    #[test]
    fn a_returning_user_on_a_new_build_is_shown_the_notice_at_startup() {
        // End to end through `App::new`: the whole point of the
        // feature, and the one thing the pure policy test can't prove.
        // Only reaches a modal because the bundled changelog has a
        // section for this version — which the `0.0.1` argument makes
        // an upgrade *to*.
        let _lock = crate::test_env::env_lock();
        let app = returning_user_app("0.0.1");
        assert_eq!(
            app.modal_stack.contains::<modal::PostUpgradeModal>(),
            changelog::notes_for_version(INSTALLED_VERSION).is_some(),
            "shown exactly when this version has changelog notes"
        );
    }

    #[test]
    fn a_launch_on_the_recorded_version_raises_nothing() {
        let _lock = crate::test_env::env_lock();
        let app = returning_user_app(INSTALLED_VERSION);
        assert!(!app.modal_stack.contains::<modal::PostUpgradeModal>());
    }

    #[test]
    fn a_first_run_is_never_greeted_with_release_notes() {
        // Default config: `show_welcome` on, no version recorded.
        // `env_lock` rather than `config_isolation` for the reason
        // `returning_user_app` documents — under the suppression this
        // assertion would hold no matter what the rule did, which is
        // the one way a test like this fails silently.
        let _lock = crate::test_env::env_lock();
        let app = make_app();
        assert!(!app.modal_stack.contains::<modal::PostUpgradeModal>());
    }

    #[test]
    fn opening_it_twice_does_not_stack_it() {
        let _iso = crate::test_env::config_isolation();
        let mut app = make_app();
        app.open_post_upgrade_modal();
        app.open_post_upgrade_modal();
        assert_eq!(app.modal_stack.count::<modal::PostUpgradeModal>(), 1);
    }
}
