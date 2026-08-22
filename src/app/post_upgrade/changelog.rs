//! Extract one release's section out of the bundled `CHANGELOG.md`.
//!
//! The offline counterpart to [`crate::app::update_check::parse`].
//! That module reads a release body off the network and has to treat
//! every byte as hostile; this one reads a file compiled into the
//! binary, authored in this repository, and needs no such posture.
//! What the two *do* share is the surface the text ends up on —
//! `ui::update_check`'s note renderer — so the section is bounded by
//! the very same [`sanitize_notes`] the network path uses.  Reusing it is not about trust here: it is
//! what keeps a changelog entry and a release body rendering in one
//! vocabulary, with one set of caps, so the two surfaces cannot drift.
//!
//! `include_str!` rather than a disk read: the changelog is part of
//! the build, so a notice about *this* build must not depend on a file
//! the user could have moved.  `Cargo.toml` uses an `exclude` list
//! that does not name `CHANGELOG.md`, so it ships in the published
//! tarball and this compiles from a packaged crate as well as from a
//! checkout.
//!
//! There is deliberately no Markdown parsing.  The section is sliced
//! by line, and every line inside it reaches the modal verbatim — the
//! renderer's structural styling is the whole vocabulary, exactly as
//! it is for a release body.

use crate::app::update_check::parse::sanitize_notes;

/// The changelog this binary was built from.
const CHANGELOG_MD: &str = include_str!("../../../CHANGELOG.md");

/// Release notes for `version` out of the bundled changelog, bounded
/// the same way a fetched release body is.
///
/// `None` when the changelog has no `## [<version>]` heading — an
/// in-development build whose section is still `## [Unreleased]`, or a
/// tag cut before its entry was written.  That is an ordinary state,
/// not a failure: the startup notice stays silent for it, and the
/// About page's on-demand opening says so.
pub(crate) fn notes_for_version(version: &str) -> Option<Vec<String>> {
    notes_from(CHANGELOG_MD, version)
}

/// [`notes_for_version`] with the changelog injected, so the rule is
/// testable against small literals instead of the real file — the same
/// split `status::compare_versions` keeps from `compare_to_installed`.
fn notes_from(changelog: &str, version: &str) -> Option<Vec<String>> {
    section_for_version(changelog, version).map(|raw| sanitize_notes(&raw))
}

/// The lines between a version's heading and the start of whatever
/// follows it, exclusive of the heading itself.
///
/// The heading is dropped because the modal states the version in its
/// own row; repeating `## [0.1.2] - 2026-08-22` above it would say the
/// same thing twice, in the changelog's punctuation rather than the
/// modal's.  It also matches what a reader sees on the network path,
/// where `dist` puts the section's *contents* into the release body.
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

/// The version a `## [x.y.z]` heading names, or `None` for any other
/// line.
///
/// Matched on the bracketed text *exactly*, so `0.1.2` cannot claim a
/// `## [0.1.20]` heading — a prefix comparison would hand a user the
/// wrong release's notes, and silently, since both are real versions.
/// A trailing date (`## [0.1.1] - 2026-08-18`) is ignored by
/// construction: it is outside the brackets.
fn heading_version(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("## [")?;
    let end = rest.find(']')?;
    Some(&rest[..end])
}

/// Whether a line ends the section above it.
///
/// The next `## ` heading is the obvious terminator, and a `### Added`
/// subheading deliberately is not one — its third character is `#`
/// rather than a space, so it stays inside the section as content.
///
/// The second arm is the one that is easy to miss: the **last**
/// section in the file has no heading after it, only Keep a
/// Changelog's trailing link-reference block (`[0.1.0]: https://…`).
/// Without stopping there, the oldest release's notes would end with a
/// paragraph of raw URLs — visible only once that release is the
/// installed one, which is to say on exactly the launch nobody tests.
fn ends_section(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("## ") || is_reference_definition(line)
}

/// A Markdown link-reference definition (`[label]: url`).
///
/// This is a terminator on the assumption that such a line only ever
/// appears in the footer block, which is true of Keep a Changelog and
/// of every entry this repository has written — entries are `-`
/// bullets and prose.  An entry that opened a *line* with one (`[GH-42]:
/// …`) would cut its own release's notes short there.  Worth revisiting
/// only if changelog authoring style changes; a reference used inline
/// within a bullet is unaffected, since the line does not start with
/// `[`.
fn is_reference_definition(line: &str) -> bool {
    line.starts_with('[') && line.contains("]:")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like the real file: an unreleased section on top, two
    /// released ones, and the trailing link-reference block.
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
        // `### Added` must not end the section, or every entry would
        // be cut down to the blank line under its own heading.
        let section = section_for_version(SAMPLE, "0.1.1").expect("section");
        assert!(section.contains("### Added"));
        assert!(section.contains("### Fixed"));
    }

    #[test]
    fn the_last_section_stops_before_the_link_reference_block() {
        // Nothing follows 0.1.0 but Keep a Changelog's link
        // definitions.  Without the reference-definition terminator
        // they would be read as release notes — and only ever on the
        // launch where the oldest release is the installed one.
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
        // A version bumped in Cargo.toml before its entry is renamed
        // must not silently pick up the in-flight notes.
        assert_eq!(section_for_version(SAMPLE, "0.1.2"), None);
        assert!(section_for_version(SAMPLE, "Unreleased").is_some());
    }

    #[test]
    fn notes_are_bounded_by_the_shared_sanitizer() {
        // The blank line under the heading is trimmed and control
        // characters are stripped — both `sanitize_notes` behaviours,
        // asserted here so the reuse can't quietly be dropped.
        let changelog = "## [1.0.0]\n\n- a\u{202e}b\n";
        let notes = notes_from(changelog, "1.0.0").expect("notes");
        assert_eq!(notes, vec!["- ab".to_owned()]);
    }

    #[test]
    fn the_bundled_changelog_parses_with_the_shipped_headings() {
        // A regression guard on the *file*, not the code: if someone
        // restyles CHANGELOG.md's headings (dropping the brackets,
        // say), every future upgrade notice goes silent, and nothing
        // else in the suite would notice.
        let notes = notes_for_version("0.1.1").expect("0.1.1 is a released section");
        assert!(
            notes.iter().any(|l| l.contains("Startup update check")),
            "expected 0.1.1's own notes, got {notes:?}"
        );
    }
}
