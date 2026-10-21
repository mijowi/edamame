# Changelog

All notable changes to edamame are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each released version's section is also what ships as the GitHub release notes: `dist` reads the section matching the tag and puts it at the top of the release body, above the generated install and download tables. edamame's update check reads that same body back and shows the part above the `## Install` heading, so **an entry written here is what users see in the "Update available" modal**. Keep entries short and user-facing for that reason — a release cut without a matching section here still notifies, just with no summary.

## [Unreleased]

### Added

- Releases now include a prebuilt Windows binary (`x86_64`), as a `.zip` archive. Windows is still a best-effort platform.
- Windows Terminal is detected, giving it 24-bit color, Sixel images, and mouse support on native Windows and under WSL.

### Security

Updated dependencies to include these fixes:

- Fixed security flaws in the math renderer that a crafted formula in a document could trigger (RUSTSEC-2026-0381, RUSTSEC-2026-0382).
- Fixed a flaw in HTML export's sanitizer that could let crafted raw HTML in a document through into the exported file.
- Fixed a TLS flaw in the connection used for remote images and the update check.

## [0.1.6] - 2026-10-14

### Changed

- Paragraphs inside list items, blockquotes and footnotes now reflow to the window width like top-level paragraphs. With reflow off, every paragraph keeps your line breaks.
- Tables, code blocks and other blocks inside a list item now line up under the item's text. Insert table on an empty item, or on the blank line below one, puts the table inside the item.
- Blank `>` lines in a blockquote now show as empty quoted rows, as many as you wrote.

### Fixed

- Heading links now use GitHub's anchors, so links to headings with non-Latin, accented or formatted text (`#日本語`, `#café`) work. A bare `#` link goes to the top of the document.
- Tables inside list items, blockquotes and footnotes, and tables without edge pipes or with short rows, now work like any other table: one-cell editing, Tab, row and column commands, and the mouse handles.
- A `$$` math block at the start of a list item, or in a file with Windows (CRLF) line endings, now renders as a formula instead of running its lines together as text.
- HTML comments inside a list item, blockquote or footnote are now hidden, as they already were elsewhere.
- Layout fixes, mostly in lists, blockquotes, footnotes, tables and wrapped text: clicks, cursor, highlights and line numbers landing in the wrong place; missing or extra blank rows; misaligned indentation.
- CJK text now wraps between characters, keeps punctuation such as `。` and `「` with its text, and no longer adds blank rows in code blocks. A reflowed paragraph no longer shows a space where a Chinese or Japanese line was wrapped in the source.
- Accented letters and Korean text stored in decomposed form, common in files from macOS, now display correctly.
- An emoji or accented letter is now treated as one character: cursor keys in text fields, and vim motions and edits, no longer stop inside or split one. In vim mode, a decomposed accent no longer ends a word for motions, `diw`, `*` or double-click, and `f`/`t` no longer match the plain letter under it.
- A `---` further down a file that starts with frontmatter is no longer mistaken for more frontmatter.
- Pasting an image on the blank line after an indented code block or HTML block now works.
- Dragging to select no longer switches lines between rendered and Markdown source mid-drag.
- `==highlighted==` text containing an apostrophe, quotation marks, a dash or `...` now shows as highlighted.

### Security

- HTML export's sanitizer no longer lets crafted SVG animation tags or CSS `url()` values in a document's raw HTML through into the exported file, where they could run script when the file is opened in a browser.
- Fixed a TLS flaw in the connection used for remote images and the update check.

## [0.1.5] - 2026-10-05

### Added

- An uninstall script that removes edamame's config and state files, and the binary (deferring to the package manager if applicable).
- edamame remembers where you left the cursor in files and reopens it there. Turn it off with the `remember_cursor` setting in `config.toml`.
- Daily tips: once a day at startup, edamame shows a short tip about a feature you might not know about. Turn it off with the tip's "Don't show tips" button or the "Daily tips" setting. See all tips with Ctrl-P → "Browse tips".
- Partly visible images now display in full resolution instead of dropping to coarse half-blocks in kitty, Ghostty, WezTerm, and Sixel terminals.
- Images now stay sharp *during* scroll in kitty, Ghostty, and WezTerm. Disable with the new `sharp_scrolling` setting under `[images]` in config.toml if images tear, flicker, or cause lag.
- Syntax highlighting added to HTML export. The bundled stylesheet colors them for light, dark, and print; a custom stylesheet can style the `hl-*` classes (see "Exporting" in `docs/editing.md`).
- `Ctrl-V` pastes a screenshot (or any image) from the clipboard, saved as a PNG with a relative reference inserted. Text is preferred if the clipboard contains both, but **Paste image from clipboard** in the palette always pastes the image.

### Changed

- Editing is faster, most of all in large documents with code blocks or tables: up to 4× faster on Linux and 1.8× on macOS.
- edamame's bookkeeping (e.g. update-check timestamps) moved from config.toml to state.toml in your data directory. edamame migrates these values on the next launch, leaving config.toml fully hand-editable and safe to share across machines. No action needed.
- `Ctrl-V` into a numbered list now renumbers the list, like a paste from the terminal (e.g. `Ctrl-Shift-V`) already did.
- A self-contained HTML export can now embed images from outside the document's folder, such as a shared `../assets/` folder. edamame lists them and asks first; before, they were silently left as links.
- HTML export keeps raw HTML such as `<details>`, `<kbd>`, `<sub>`/`<sup>` and `<img width>`, removing only what could run in a browser. Before, all raw HTML was dropped.
- HTML export keeps links to other apps (`obsidian://`, `vscode://`, `file://`, …); only `javascript:`, `vbscript:` and `data:` links are removed. Before, anything but `http`, `https`, `mailto` and `tel` was removed.
- Exported diagrams and formulas are now SVG instead of PNG, so they stay sharp when zoomed or printed.
- **Inline images** in HTML export now also embeds raw-HTML `<img>` tags and `file://` images. Embedded SVGs are cleaned of external references.

### Fixed

- A document with the same image twice in the source now displays it in both places, instead of only the second.
- Copying an empty line no longer empties the system clipboard, and no longer hints "Copied".
- CJK and other wide characters now cursor-place, render, and wrap correctly in table cells.
- A custom export format (PDF, DOCX, …) no longer lets its converter read images from outside the document's folder without asking, or download remote images you haven't allowed. edamame now embeds every image itself, asking first about out-of-folder ones.
- Links to a heading (`[x](#section)`) now work in HTML export. Exported headings had no anchors, so these links went nowhere.
- An image that arrives in the document *after* it was opened now raises the images question. Previously nothing was asked unless the document contained an image at load, so the image stayed as its source line and never rendered.
- With cursor blink turned off, a code block's syntax colors now appear on their own instead of waiting for the next keypress.

## [0.1.4] - 2026-09-11

### Added

- Manually-wrapped paragraphs now reflow to the editor width. A paragraph wrapped in the source is joined into a single line and wrapped to fit the editor, instead of showing one short row per source line. A hard break (two trailing spaces or a backslash) still splits. Can be turned off in settings (Reflow paragraphs).
- edamame now displays LaTeX math. A `$$...$$` block renders as a formula image using pure Rust — no additional install needed. HTML export renders math as embedded images.

### Changed

- The editor is now capped at 100 columns by default, for more comfortable reading on wide terminals. Turn it off with the "Limit editor width" toggle in settings (Ctrl-P → Open settings). You can also change the character limit there.
- Editing large documents does less work per keystroke: the render cache now uses a faster hash and no longer caches blocks that are cheap to redraw, which removes a slowdown on long list- and math-heavy files.
- Math and diagrams have been combined as **Figures** in the diagrams consent setting, HTML export options, and the `[diagrams]` config section (an existing `[diagrams]` is rewritten to `[figures]` automatically on the next launch).

### Fixed

- Clicks now map to the correct character in table cells with formatting.
- Mermaid diagrams no longer have missing text when rendered on some systems.

## [0.1.3] - 2026-09-02

### Fixed

- edamame now respects your documents' existing line endings, whether Windows CRLF or Mac/Linux LF. A file with CRLF line endings no longer shows up as one big change in diff review. New documents decide line ending based on your OS.
- Removed the 'Esc Preview' hint and 'Exit to Preview' palette entry when vim mode is enabled.

## [0.1.2] - 2026-08-24

### Added

- Export to PDF, DOCX or anything else a converter on your machine can produce. See Docs: Configuration for more info.
- A Terminal compatibility page in the manual: what each capability affects, the workarounds, and a table of which terminals support what. The terminal-capabilities notice links straight to it.
- The manual now ships with and opens in the app. Ctrl-P → Help: Documentation for the index, or jump straight to a page (Docs: Keybindings, Docs: Vim mode, …). Pages are read-only, searchable, and link to each other; `Alt+Left` returns to what you were writing.
- After an upgrade, edamame shows the new version's release notes once, read from the changelog built into it. The Release notes button on the About page shows them again at any time.
- Syntax highlighting for fenced code blocks, covering over 200 languages. A fence with no language renders as plain code. Colors come from the active theme. Highlighting can be turned off in settings.
- Links to a section of another document — `[text](other.md#a-heading)` — now open that file and land on the heading. Works from the command line, too.
- `edamame --diff <old> <new>` opens a read-only review of two files, for use as a `git difftool`. `Tab` moves between hunks, `Esc` goes on to the next file, and `Ctrl-Q` stops the walk. Nothing is written. A pair that isn't Markdown or isn't readable as text is reported and skipped.
- YAML (`---`) and TOML (`+++`) frontmatter is rendered as a metadata block.
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

[Unreleased]: https://github.com/mijowi/edamame/compare/v0.1.6...HEAD
[0.1.6]: https://github.com/mijowi/edamame/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/mijowi/edamame/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/mijowi/edamame/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/mijowi/edamame/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/mijowi/edamame/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/mijowi/edamame/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/mijowi/edamame/releases/tag/v0.1.0
