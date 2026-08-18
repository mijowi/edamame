//! Release-check domain types: what a resolved check actually means.
//!
//! The up-to-date / update-available split is decided **here**, once, in
//! [`ReleaseStatus::from_fetch`], rather than re-derived at render time.
//! Two consumers have to agree on it — the update modal picks its body
//! copy from the variant, and [`super::policy::notice_due`] gates the
//! startup notice on the same fact — so a second, display-time version
//! comparison is precisely the drift this collapses.  It replaces the
//! older `release_suffix`, which annotated a string instead of naming a
//! state and had no way to say "don't nag about this".
//!
//! A comparison that can't be made is its own state rather than a
//! rounding of one.  [`compare_to_installed`] answers `None` when either
//! side doesn't parse as a numeric version, and that becomes
//! [`ReleaseStatus::Inconclusive`] — silent like `UpToDate` on the
//! notice path, but honest on the explicit one.  Folding it into
//! `UpToDate` is what made the modal assert "edamame is up to date."
//! directly above two version rows disagreeing with it.

use std::cmp::Ordering;

/// The version this binary was built as, with no leading `v`.
pub(crate) const INSTALLED_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A resolved release: its tag plus the release notes, already
/// truncated, control-stripped and line-capped by
/// [`super::parse::sanitize_notes`] on the worker thread — the main
/// thread never sees unbounded remote text.
///
/// `notes` is empty when the release carries nothing ahead of
/// cargo-dist's install boilerplate (a tag cut without a matching
/// `CHANGELOG.md` heading).  That is an ordinary state, not a failure:
/// the modal still reports the new version, just without a summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReleaseInfo {
    pub tag: String,
    pub notes: Vec<String>,
}

/// Outcome of a release check, as every consumer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReleaseStatus {
    /// A check is in flight and nothing is cached yet — the modal
    /// shows a spinner.
    Pending,
    /// The latest release is not newer than [`INSTALLED_VERSION`].
    /// Carries the tag so the modal can name what it compared against.
    UpToDate { tag: String },
    /// The latest release is newer than the installed build.
    Available(ReleaseInfo),
    /// The check reached GitHub and got a tag, but the two versions
    /// could not be ordered — a tag with a pre-release or build suffix,
    /// or one not shaped like a version at all.  Carries the tag so the
    /// modal can show both numbers and let the user judge.
    ///
    /// Distinct from `UpToDate` on purpose: it is equally silent on the
    /// notice path (see [`super::policy::notice_due`]), but an explicit
    /// check must not claim to have made a comparison it didn't.
    Inconclusive { tag: String },
    /// Network error, HTTP error, or an unparseable body.  Reported on
    /// an explicit check; silently dropped by the startup path.
    Failed,
}

impl ReleaseStatus {
    /// Classify a worker result.  The only place `Ok` is split into
    /// `Available` / `UpToDate` / `Inconclusive`.
    pub(crate) fn from_fetch(result: Result<ReleaseInfo, String>) -> Self {
        match result {
            Ok(info) => match compare_to_installed(&info.tag) {
                Some(Ordering::Greater) => ReleaseStatus::Available(info),
                Some(_) => ReleaseStatus::UpToDate { tag: info.tag },
                None => ReleaseStatus::Inconclusive { tag: info.tag },
            },
            Err(_) => ReleaseStatus::Failed,
        }
    }

    /// The release tag this status is about, or `None` for the two
    /// states that name no release (`Pending`, `Failed`).  The single
    /// accessor for it, so a consumer needing the tag doesn't grow a
    /// match arm per variant and then forget one when a state is added.
    pub(crate) fn tag(&self) -> Option<&str> {
        match self {
            ReleaseStatus::UpToDate { tag } | ReleaseStatus::Inconclusive { tag } => Some(tag),
            ReleaseStatus::Available(info) => Some(&info.tag),
            ReleaseStatus::Pending | ReleaseStatus::Failed => None,
        }
    }
}

/// Order `tag` against [`INSTALLED_VERSION`].  See
/// [`compare_versions`]; this is the one-argument form every caller
/// outside the tests wants.
pub(crate) fn compare_to_installed(tag: &str) -> Option<Ordering> {
    compare_versions(INSTALLED_VERSION, tag)
}

/// Order `tag` against `installed`, both tolerating a leading `v`.
/// `Greater` means `tag` names a strictly newer release.
///
/// `None` means the two could not be compared — either side unparseable
/// as a dotted numeric version, which covers a pre-release or build
/// suffix (`v0.2.0-rc1`, `v0.2.0+build`) and a tag not shaped like a
/// version at all.  It is deliberately not folded into "not newer":
/// both answers keep the startup notice silent, but only one of them is
/// true, and the explicit-check modal shows the user both numbers.
///
/// A build *ahead* of the latest release (a local build between tags)
/// compares `Less` and is likewise not an update.
pub(crate) fn compare_versions(installed: &str, tag: &str) -> Option<Ordering> {
    let installed = parse_version(installed)?;
    let tag = parse_version(tag)?;
    Some(tag.cmp(&installed))
}

/// Parse `v1.2.3` / `1.2.3` into numeric segments for ordering.
/// `Vec` comparison is lexicographic over the segments, so a longer
/// tuple sharing a prefix sorts newer (`1.0` < `1.0.1`).
fn parse_version(v: &str) -> Option<Vec<u64>> {
    v.trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(tag: &str) -> ReleaseInfo {
        ReleaseInfo {
            tag: tag.to_owned(),
            notes: Vec::new(),
        }
    }

    /// `Some(Greater)` — `tag` is newer.
    fn newer(installed: &str, tag: &str) -> bool {
        compare_versions(installed, tag) == Some(Ordering::Greater)
    }

    #[test]
    fn compare_versions_orders_numeric_versions() {
        assert_eq!(compare_versions("0.1.0", "v0.1.0"), Some(Ordering::Equal));
        assert!(newer("0.1.0", "v0.2.0"));
        assert!(newer("0.1.0", "v0.1.1"));
        assert_eq!(
            compare_versions("0.2.0", "v0.1.9"),
            Some(Ordering::Less),
            "ahead of release is not an update"
        );
        // Numeric, not lexicographic: 0.10.0 > 0.9.0.
        assert!(newer("0.9.0", "v0.10.0"));
        // A longer tuple with the same prefix is newer.
        assert!(newer("1.0", "v1.0.1"));
    }

    #[test]
    fn compare_versions_reports_an_uncomparable_pair_as_none() {
        // Not "not newer" — unknown.  Either side is enough to spoil it.
        assert_eq!(compare_versions("0.1.0", "nightly"), None);
        assert_eq!(compare_versions("0.1.0", "v0.2.0-rc1"), None);
        assert_eq!(compare_versions("0.1.0-beta", "v0.2.0"), None);
        assert_eq!(compare_versions("0.1.0", "v0.2.0+build.7"), None);
    }

    #[test]
    fn from_fetch_classifies_each_outcome() {
        assert_eq!(
            ReleaseStatus::from_fetch(Err("offline".to_owned())),
            ReleaseStatus::Failed
        );
        // Equal to the installed version → up to date, tag retained.
        let same = ReleaseStatus::from_fetch(Ok(info(INSTALLED_VERSION)));
        assert_eq!(
            same,
            ReleaseStatus::UpToDate {
                tag: INSTALLED_VERSION.to_owned()
            }
        );
        // A version nothing will plausibly reach → available.
        assert_eq!(
            ReleaseStatus::from_fetch(Ok(info("v999.0.0"))),
            ReleaseStatus::Available(info("v999.0.0"))
        );
        // Uncomparable → its own state, never a claim of up-to-date.
        assert_eq!(
            ReleaseStatus::from_fetch(Ok(info("v999.0.0-rc1"))),
            ReleaseStatus::Inconclusive {
                tag: "v999.0.0-rc1".to_owned()
            }
        );
    }
}
