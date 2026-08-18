//! When to check, and when a result deserves to interrupt.
//!
//! Both decisions are pure functions of primitives — not of `Config`,
//! not of `App`.  That is what makes the nag behavior table-testable
//! without constructing either, and it keeps the two rules that a user
//! actually feels (checked at most daily, told at most once per
//! version) in one readable place rather than spread across the spawn
//! site and the result handler.

use super::status::{ReleaseInfo, ReleaseStatus};

/// Minimum gap between automatic checks.  Manual checks — the About
/// button and the command-palette action — deliberately ignore this:
/// it exists to bound *unattended* network chatter, and an explicit
/// request is not unattended.
pub(crate) const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Wall-clock seconds since the Unix epoch, or `0` if the system clock
/// predates it.  `0` reads as "never checked", which fails safe: the
/// next launch checks rather than silently never checking again.
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Should the startup check hit the network?
///
/// `last_check == 0` means never checked, and is always due — spelled
/// out rather than left to the arithmetic, which would only reach the
/// same answer on a machine whose clock has already passed the
/// interval since the epoch.
///
/// A `last_check` in the future means the clock moved backwards (a
/// timezone fix, a VM restore, a dead RTC) — treat that as due rather
/// than letting a bogus future timestamp disable the check until it
/// arrives, which for a badly wrong clock could be years.
pub(crate) fn network_check_due(enabled: bool, last_check: u64, now: u64) -> bool {
    enabled && (last_check == 0 || last_check > now || now - last_check >= CHECK_INTERVAL_SECS)
}

/// Should this result raise the startup notice?
///
/// Only a genuinely newer release the user has not already been told
/// about.  `UpToDate` says nothing (the whole point of the feature is
/// silence when there is no news), `Inconclusive` says nothing either
/// (a tag we couldn't order is not grounds to interrupt anybody), and
/// neither does `Failed` — an unattended check that couldn't reach
/// GitHub is not the user's problem to dismiss.
pub(crate) fn notice_due<'a>(
    status: &'a ReleaseStatus,
    notified_for: &str,
) -> Option<&'a ReleaseInfo> {
    match status {
        ReleaseStatus::Available(info) if info.tag != notified_for => Some(info),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = CHECK_INTERVAL_SECS;

    fn available(tag: &str) -> ReleaseStatus {
        ReleaseStatus::Available(ReleaseInfo {
            tag: tag.to_owned(),
            notes: Vec::new(),
        })
    }

    #[test]
    fn a_disabled_check_is_never_due() {
        assert!(!network_check_due(false, 0, DAY * 100));
        assert!(!network_check_due(false, 0, 0));
    }

    #[test]
    fn a_never_run_check_is_due_immediately() {
        assert!(network_check_due(true, 0, 1));
    }

    #[test]
    fn a_check_inside_the_window_is_not_due() {
        let now = DAY * 10;
        assert!(!network_check_due(true, now, now));
        assert!(!network_check_due(true, now - 1, now));
        assert!(!network_check_due(true, now - (DAY - 1), now));
    }

    #[test]
    fn a_check_at_or_past_the_window_is_due() {
        let now = DAY * 10;
        assert!(network_check_due(true, now - DAY, now));
        assert!(network_check_due(true, now - DAY * 3, now));
    }

    #[test]
    fn a_timestamp_from_the_future_is_due_rather_than_stuck() {
        // Clock skew must not disable the check until the bogus
        // timestamp actually arrives.
        assert!(network_check_due(true, DAY * 100, DAY * 10));
    }

    #[test]
    fn only_a_new_available_release_raises_the_notice() {
        assert!(notice_due(&available("v0.2.0"), "").is_some());
        assert!(notice_due(&available("v0.2.0"), "v0.1.0").is_some());
    }

    #[test]
    fn an_already_notified_tag_stays_quiet() {
        assert!(notice_due(&available("v0.2.0"), "v0.2.0").is_none());
    }

    #[test]
    fn a_later_release_re_arms_the_notice() {
        // Told about 0.2.0 yesterday; 0.3.0 is news again.
        assert!(notice_due(&available("v0.3.0"), "v0.2.0").is_some());
    }

    #[test]
    fn nothing_else_raises_the_notice() {
        assert!(notice_due(&ReleaseStatus::Pending, "").is_none());
        assert!(notice_due(&ReleaseStatus::Failed, "").is_none());
        assert!(notice_due(
            &ReleaseStatus::UpToDate {
                tag: "v0.1.0".to_owned()
            },
            ""
        )
        .is_none());
        // Uncomparable is as silent as up-to-date: the explicit modal
        // says so honestly, but nothing nags off a guess.
        assert!(notice_due(
            &ReleaseStatus::Inconclusive {
                tag: "v0.2.0-rc1".to_owned()
            },
            ""
        )
        .is_none());
    }
}
