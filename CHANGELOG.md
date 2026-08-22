# Changelog

All notable changes to edamame are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each released version's section is also what ships as the GitHub release notes: `dist` reads the section matching the tag and puts it at the top of the release body, above the generated install and download tables. edamame's update check reads that same body back and shows the part above the `## Install` heading, so **an entry written here is what users see in the "Update available" modal**. Keep entries short and user-facing for that reason — a release cut without a matching section here still notifies, just with no summary.

## [Unreleased]

### Added

- YAML (`---`) and TOML (`+++`) frontmatter is rendered as a metadata block — one row per source line, dimmed, with keys picked out from values — instead of a horizontal rule followed by a heading. Only the very first line of a file can open frontmatter, so `---` section separators elsewhere still render as rules. Frontmatter is left out of an HTML export.
- Three theme keys for it: `frontmatter_delimiter`, `frontmatter_key`, `frontmatter_value`.

### Changed

- Diff review now shows the unchanged parts of a document as rendered Markdown — headings styled, tables as grids, images in place — so only the regions actually under review drop to raw source.
- Block quotes are marked with a subtle background wash instead of italic text, so emphasis inside a quote is visible as emphasis.

### Fixed

- Bold, italic, inline code, highlights, strikethrough and links now keep their styling inside a block quote.

## [0.1.1] - 2026-08-18

### Added

- Startup update check. edamame now checks GitHub for a newer release at most once a day and shows a one-time notice, with the release notes, when a new version first appears. The check can be turned off in settings.
- "Check for updates" in the command palette, and a matching button on the About page.

### Changed

- The About page no longer contacts GitHub when it opens, and no longer shows a "Current release" row.
- The Markdown cheat sheet explains line breaks more clearly.

### Fixed

- Wrapped lines in raw mode no longer get a hanging indent — raw mode shows the file as written.
- The cursor and mouse clicks now land on the right character inside code blocks.
- Images now appear in files opened from within edamame, such as by following a link.
- Alt-Left / Alt-Right now work correctly for e.g. file navigation on macOS and other systems.
- A failed image render no longer takes the editor down with it.
- Debug logging (--log) now records the full trace instead of only startup lines.

## [0.1.0] - 2026-08-17

First public release.

[Unreleased]: https://github.com/mijowi/edamame/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/mijowi/edamame/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/mijowi/edamame/releases/tag/v0.1.0