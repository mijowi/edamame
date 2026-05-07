# Edamame Design Guide and Default Theme

## Color palette — each have bright/dim variants
The assigned colors represent edamame's default theme. The palette is used by every theme.
- `default_text`: white
- `default_bg`: black
- `primary`/brand: orange — identity, headings
- `emphasis`: yellow — highlights, warnings, hints
- `structural`: purple — chrome, frames, dividers, asides
- `interactive`: blue — links, focus, selection, hint chords
- `success`: green
- `warning`: same as emphasis in this theme, but can be differentiated in future themes
- `error`: red
- `muted`: light grey — peripheral text (h6, strikethrough) and borders (table rules, separators)
- `surface`: dark grey — base UI chrome (`surface_elevated`: status line, modal body, inline / fenced code bg, table-stripe fill); slightly lighter grey — elevated UI chrome (`surface`: hint line and the transient-message strip that overlays it)

## Palette Assignments

Selected text: `interactive_dim` bg, `default_text` fg

Search/find highlight: `structural_bright` bg, `default_bg` fg

Active line highlight (not implemented): slightly lighter shade than default bg (orange tint?)

### Headings
- H1: `emphasis_bright` fg, bold, setext trailing rule (`═`)
- H2: `primary_bright` fg, bold
- H3: `structural_bright` fg, bold
- H4: `primary_dim` fg, bold
- H5: `structural_dim` fg, bold
- H6: `text_muted`, bold

Strikethrough: `text_muted` fg

Highlight: `emphasis_dim` bg, `default_bg` fg

Block quote: `structural_dim` left bar, italic text

Web link: `interactive_bright` fg, underlined

File link: `interactive_dim` fg, underlined

Heading link: `interactive_dim` fg

Image link (when image not shown): `interactive_dim` fg, italicized

Unordered list marker: `structural_bright` fg

Ordered list number marker (including `.`): `structural_bright` fg

### Task list
- Incomplete item: `emphasis_bright` fg bullet and checkbox
- Complete item: `success_bright` fg bullet and checkbox; `text_muted` fg text with strikethrough

Horizontal rule: `structural_dim` fg

### Table
- Header row: bold, default fg
- Borders: `muted` fg (light `─` rules around and between cells)
- Header bottom separator: `structural_dim` fg — the heavy `━` rule below the header row is themed independently of the regular borders so the header reads as a structural divider
- Row striping (when `[table] row_striping = true`): odd data rows get `surface_elevated` bg so the table chrome matches the inline-code surface

Inline code: `surface_elevated` bg, `structural_bright` fg

### Code block
- Language: `structural_bright` fg, italicized
- Block: `surface_elevated` bg
- Text: `default_text` fg (syntax highlighting later)

Footnote/reference marker (not implemented yet)
- `structural_dim` fg

### Status line: `surface_elevated` bg
Mode chip (bold)
  - Preview: `muted` bg, `surface_elevated` fg
  - Rendered: `primary_bright` bg, `default_bg` fg
  - Raw: `emphasis_bright` bg, `default_bg` fg
File name: `default_text` fg
Dirty file marker (`*`): `emphasis_bright` fg, bold
Cursor coordinates, line count, etc: `primary_bright` fg

### Hint line: `surface` bg
Preview hint (`Press any key to edit`): `default_text` fg
Hint chord: `interactive_bright` fg, bold
Hint label: `default_text` fg
Transient message: 
- Info: `default_text` fg
- Warning: `warning_bright` fg
- Error: `error_bright` fg

Cursor (in editor): Each mode has its own cursor style mirroring the status-line mode chip. The renderer picks via `theme.cursor_style(mode)`:
- Preview: `muted` bg, `surface_elevated` fg (matches the Preview mode chip)
- Rendered: `primary_bright` bg, `default_bg` fg
- Raw: `emphasis_bright` bg, `default_bg` fg

Cursor (modal text inputs): `theme.cursor` — REVERSED only, so the `▏` glyph inverts whatever's underneath without needing to know the modal's surface colour. Kept distinct from the editor cursor because modal inputs aren't tied to editor mode.

### Modal windows
Background: `surface_elevated`
Title: `primary_bright` fg, bold
Border: `structural_dim` fg
Item: `default_text`
Item hint / sub-label (right-aligned chord, value column, etc. on an unfocused row): `interactive_bright` fg
Selected item: `interactive_dim` bg, `default_text` fg, bold
Selected item hint (right-aligned chord / value column on the focused row): `emphasis_bright` fg
Description (pinned-footer copy explaining the focused row, e.g. settings overlay bottom line): `emphasis_bright` fg
Input (unfocused): `default_bg` fg, `interactive_dim` bg
Input (focused): `default_bg` fg, `interactive_bright` bg; modal-input cursor inverts whatever's underneath
Focused button (e.g. Save / Discard / Cancel in the unsaved-changes confirmation): `interactive_bright` fill (REVERSED, bold)
Section heading: `structural_bright` fg, bold

#### Command palette input — exception
Unlike other modal inputs, the command palette's typing row sits flush
against the modal body — no coloured bg fill — and the `▏` cursor
glyph is `interactive_bright` so the typing affordance still pops.
This break is deliberate: the palette is a search affordance, not a
form field, so it reads as part of the modal rather than as a sunken
input chip.

## Built-in themes
- [ ] Edamame
- [ ] Monochrome
- [x] 256 color dark
- [ ] 256 color light
- [ ] GitHub
- [ ] Dracula
- [ ] Catpuccin
- [ ] Tokyo Night
- [ ] Monokai

## Follow-ups

UI elements the design guide doesn't yet pin down — flagged here so we
can return to them once the visual language settles.

- **Active-line highlight.** `theme.active_line` is wired in but
  defaults to `Style::default()`; a real hint suggested an "orange
  tint" but no concrete fg/bg has been picked. The `RenderedView`
  isn't yet painting any active-line band either — a future patch
  needs both the palette assignment and the renderer pass.
- **Search / find-in-document.** `theme.search_highlight` is in place
  (structural_bright bg, default_bg fg) for when the find feature
  lands. The find UI itself (input chrome, match counter, surrounding
  status banner) is not specified.
- **Footnote / reference markers.** `theme.footnote` is in place
  (structural_dim fg) but the renderer doesn't emit footnote markers
  yet. Once `pulldown-cmark`'s footnote inlines surface, hook the
  style up.
- **Scrollbars.** Modals draw a textual `↑` / `↓` / `↑↓` indicator
  in the title; there's no narrow gutter scrollbar. Once we add one,
  it'll need a palette entry (probably `structural_dim` fg).
- **Diff / merge markers.** No styling for diff view (added /
  removed / context lines) — deferred until the editor grows a
  diff feature.
- **Surface naming.** The palette description in this file says
  "dark grey — UI surfaces (status line, modal, code block bg);
  slightly lighter grey — elevated UI surfaces (inputs, hint
  line)", but the rule list above uses `surface_elevated` for the
  darker chrome (status / modal / code) and `surface` for the
  elevated surfaces (hint / inputs). The implementation follows the
  rule list. Consider renaming the variants — `surface_chrome` and
  `surface_elevated` would read more naturally — once we revisit.
