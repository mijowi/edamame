//! Extract one release's section out of the bundled `CHANGELOG.md` — the offline
//! counterpart to [`crate::app::update_check::parse`].
//!
//! The section is bounded by that module's [`sanitize_notes`] not for trust (this text is
//! compiled in) but so a changelog entry and a fetched release body render in one
//! vocabulary with one set of caps.  `include_str!` rather than a disk read: a notice
//! about *this* build must not depend on a file the user could have moved.  There is
//! deliberately no Markdown parsing — the section is sliced by line and reaches the modal
//! verbatim.

use crate::app::update_check::parse::sanitize_notes;

/// The changelog this binary was built from.
const CHANGELOG_MD: &str = include_str!("../../../CHANGELOG.md");

/// Release notes for `version` out of the bundled changelog, bounded like a fetched
/// release body.
///
/// `None` when there is no `## [<version>]` heading — an in-development build, or a tag
/// cut before its entry was written.  That is an ordinary state, not a failure.
pub(crate) fn notes_for_version(version: &str) -> Option<Vec<String>> {
    notes_from(CHANGELOG_MD, version)
}

/// [`notes_for_version`] with the changelog injected, so the rule is testable against
/// small literals instead of the real file.
fn notes_from(changelog: &str, version: &str) -> Option<Vec<String>> {
    section_for_version(changelog, version).map(|raw| sanitize_notes(&raw))
}

/// The lines between a version's heading and whatever follows it, exclusive of the
/// heading — the modal states the version in its own row, and the network path likewise
/// carries only the section's contents.
fn section_for_version(changelog: &str, version: &str) -> Option<String> {
    let lines: Vec<&str> = changelog.lines().collect();
    let start = lines
        .iter()
        .position(|l| heading_version(l) == Some(version))?
        + 1;
    let end = lines[start..]
        .iter()
        .position(|l| ends_section(l))
        .map_or(lines.len(), |i| start + i);
    Some(lines[start..end].join("\n"))
}

/// The version a `## [x.y.z]` heading names, or `None` for any other line.
///
/// Matched on the bracketed text *exactly*, so `0.1.2` cannot claim a `## [0.1.20]`
/// heading.  A trailing date is outside the brackets and so ignored.
fn heading_version(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("## [")?;
    let end = rest.find(']')?;
    Some(&rest[..end])
}

/// Whether a line ends the section above it: the next `## ` heading (a `### Added`
/// subheading is content, not a boundary), or the trailing link-reference block — the
/// **last** section has no heading after it, and would otherwise end in raw URLs.
fn ends_section(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("## ") || is_reference_definition(line)
}

/// A Markdown link-reference definition (`[label]: url`).  Treated as a section
/// terminator on the assumption it only appears in the footer block; an entry that
/// *started a line* with one would cut its own notes short there.
fn is_reference_definition(line: &str) -> bool {
    line.starts_with('[') && line.contains("]:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::update_check::parse::{MAX_NOTES_BYTES, MAX_NOTES_LINES, TRUNCATION_MARKER};

    /// Shaped like the real file, trailing link-reference block included.
    const SAMPLE: &str = "\
# Changelog

Preamble prose.

## [Unreleased]

### Added

- something in flight

## [0.1.1] - 2026-08-18

### Added

- startup update check

### Fixed

- a bug

## [0.1.0] - 2026-08-17

First public release.

[Unreleased]: https://example.com/compare/v0.1.1...HEAD
[0.1.1]: https://example.com/compare/v0.1.0...v0.1.1
";

    #[test]
    fn a_section_is_the_lines_under_its_heading() {
        let section = section_for_version(SAMPLE, "0.1.1").expect("section");
        assert!(section.contains("- startup update check"));
        assert!(section.contains("- a bug"));
    }

    #[test]
    fn the_heading_itself_is_not_part_of_the_section() {
        let section = section_for_version(SAMPLE, "0.1.1").expect("section");
        assert!(
            !section.contains("## [0.1.1]"),
            "the modal states the version in its own row"
        );
    }

    #[test]
    fn a_section_stops_at_the_next_release_heading() {
        let section = section_for_version(SAMPLE, "0.1.1").expect("section");
        assert!(
            !section.contains("First public release."),
            "0.1.0's prose belongs to 0.1.0"
        );
    }

    #[test]
    fn a_subheading_is_content_not_a_boundary() {
        let section = section_for_version(SAMPLE, "0.1.1").expect("section");
        assert!(section.contains("### Added"));
        assert!(section.contains("### Fixed"));
    }

    #[test]
    fn the_last_section_stops_before_the_link_reference_block() {
        let section = section_for_version(SAMPLE, "0.1.0").expect("section");
        assert_eq!(section.trim(), "First public release.");
        assert!(!section.contains("https://example.com"));
    }

    #[test]
    fn a_version_never_matches_a_longer_one() {
        let changelog = "## [0.1.20]\n\n- twenty\n";
        assert_eq!(section_for_version(changelog, "0.1.2"), None);
        assert!(section_for_version(changelog, "0.1.20").is_some());
    }

    #[test]
    fn a_trailing_date_does_not_affect_the_match() {
        assert_eq!(heading_version("## [0.1.1] - 2026-08-18"), Some("0.1.1"));
        assert_eq!(heading_version("## [0.1.1]"), Some("0.1.1"));
    }

    #[test]
    fn a_line_that_is_not_a_version_heading_names_no_version() {
        assert_eq!(heading_version("### Added"), None);
        assert_eq!(heading_version("## Install edamame"), None);
        assert_eq!(heading_version("- a list item"), None);
        assert_eq!(heading_version("## [unterminated"), None);
    }

    #[test]
    fn an_absent_version_yields_nothing() {
        assert_eq!(section_for_version(SAMPLE, "9.9.9"), None);
        assert_eq!(notes_from(SAMPLE, "9.9.9"), None);
    }

    #[test]
    fn an_unreleased_section_is_reachable_only_by_that_name() {
        // A version bumped before its entry is renamed must not pick up in-flight notes.
        assert_eq!(section_for_version(SAMPLE, "0.1.2"), None);
        assert!(section_for_version(SAMPLE, "Unreleased").is_some());
    }

    #[test]
    fn notes_are_bounded_by_the_shared_sanitizer() {
        // Trimming and control-char stripping are `sanitize_notes` behaviors; asserted
        // here so the reuse can't quietly be dropped.
        let changelog = "## [1.0.0]\n\n- a\u{202e}b\n";
        let notes = notes_from(changelog, "1.0.0").expect("notes");
        assert_eq!(notes, vec!["- ab".to_owned()]);
    }

    #[test]
    fn the_bundled_changelog_parses_with_the_shipped_headings() {
        // A guard on the *file*: restyling CHANGELOG.md's headings would silence every
        // future upgrade notice, and nothing else in the suite would notice.
        let notes = notes_for_version("0.1.1").expect("0.1.1 is a released section");
        assert!(
            notes.iter().any(|l| l.contains("Startup update check")),
            "expected 0.1.1's own notes, got {notes:?}"
        );
    }

    #[test]
    fn the_unreleased_section_fits_under_the_notes_caps() {
        // A local-only nag, not a correctness check: an over-cap section ships with a `…` and
        // the tail of its notes missing from both modals.  Checked while the section is still
        // `[Unreleased]` and editable.  Skipped in CI (`CI` read at compile time, so no
        // `env_lock`), which would otherwise go red on `main` between releases.  Released
        // sections aren't checked, so a packager building a tag never trips it either.
        if option_env!("CI").is_some() {
            return;
        }
        let Some(raw) = section_for_version(CHANGELOG_MD, "Unreleased") else {
            return;
        };
        let notes = sanitize_notes(&raw);
        let bytes: usize = raw.trim().lines().map(|l| l.len() + 1).sum();
        assert_ne!(
            notes.last().map(String::as_str),
            Some(TRUNCATION_MARKER),
            "CHANGELOG.md's [Unreleased] section is {} lines / {bytes} bytes, over the \
             {MAX_NOTES_LINES}-line / {MAX_NOTES_BYTES}-byte cap; the update and post-upgrade \
             modals will cut it short.  Trim the section or raise the caps in \
             src/app/update_check/parse.rs.",
            raw.trim().lines().count(),
        );
    }
}
