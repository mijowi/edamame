# Changelog

All notable changes to edamame are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each released version's section is also what ships as the GitHub release notes: `dist` reads the section matching the tag and puts it at the top of the release body, above the generated install and download tables. edamame's update check reads that same body back and shows the part above the `## Install` heading, so **an entry written here is what users see in the "Update available" modal**. Keep entries short and user-facing for that reason — a release cut without a matching section here still notifies, just with no summary.

## [Unreleased]

### Added

- A Terminal compatibility page in the manual: what each capability affects, the workarounds, and a table of which terminals support what. The terminal-capabilities notice links straight to it.
- The manual now ships inside the binary and opens in the app. `Ctrl-P` → **Help: Documentation** for the index, or jump straight to a page (**Docs: Keybindings**, **Docs: Vim mode**, …). Pages are read-only, searchable, and link to each other; `Alt+Left` returns to what you were writing.
- After an upgrade, edamame shows the new version's release notes once, read from the changelog built into it. The Release notes button on the About page shows them again at any time.
- Syntax highlighting for fenced code blocks, covering over 200 languages. The language comes only from the opening fence — a fence with no language renders as plain code. Colors come from the active theme. Highlighting can be turned off in settings.
- Links to a section of another document — `[text](other.md#a-heading)` — now open that file and land on the heading.
- The same on the command line: `edamame notes.md#a-heading` opens the file at that heading.
- `edamame --diff <old> <new>` opens a read-only review of two files, for use as a `git difftool`. `Tab` moves between hunks, `Esc` goes on to the next file, and `Ctrl-Q` stops the walk. Nothing is written. A pair that isn't Markdown or isn't readable as text is reported and skipped.
- YAML (`---`) and TOML (`+++`) frontmatter is rendered as a metadata block — one row per source line, dimmed, with keys picked out from values — instead of a horizontal rule followed by a headng. Only the very first line of a file can open frontmatter, so `---` section separators elsewhere still render as rules. Frontmatter is left out of an HTML export.
- Three theme keys for it: `frontmatter_delimiter`, `frontmatter_key`, `frontmatter_value`.

### Changed

- Diff review now shows the unchanged parts of a document as rendered Markdown — headings styled, tables as grids, images in place — so only the regions actually under review drop to raw source.
- Block quotes are marked with a subtle background wash instead of italic text, so emphasis inside a quote is visible as emphasis.

### Fixed

- Bold, italic, inline code, highlights, strikethrough and links now keep their styling inside a block quote.
- Following a link to a section of another document no longer fails with an OS launcher error.
- In vim mode, the paste shortcut now fills an open `:` or `/` command line, matching a terminal-level paste (⌘V, right-click); before it did nothing at all.
- Improved line wrapping. Wraps no longer splits on contractions, decimals, times, file names, emojis, opening brackets or quotes, and a wrapped line no longer can start with a space.
- Editing `config.toml` from inside edamame now applies every changed setting straight away, not only the theme and keybindings.
- A recovered failure in the diagram renderer or an image worker no longer hands the terminal back to the shell while edamame is still running.
- An empty file now correctly displays the cursor.
- Improved modal wrapping; modals no longer break up in a small terminal.

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
