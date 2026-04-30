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
- `surface`: dark grey — base UI chrome (`bright_surface`: status line, modal body, inline / fenced code bg, table-stripe fill); slightly lighter grey — elevated UI chrome (`dim_surface`: hint line and the transient-message strip that overlays it)

## Palette Assignments

Selected text: `dim_interactive` bg, `default_text` fg

Search/find highlight: `bright_structural` bg, `default_bg` fg

Active line highlight (not implemented): slightly lighter shade than default bg (orange tint?)

### Headings
- H1: `bright_emphasis` fg, bold, setext trailing rule (`═`)
- H2: `bright_primary` fg, bold
- H3: `bright_structural` fg, bold
- H4: `dim_primary` fg, bold
- H5: `dim_structural` fg, bold
- H6: `bright_muted`, bold

Strikethrough: `bright_muted` fg

Highlight: `dim_emphasis` bg, `default_bg` fg

Block quote: `dim_structural` left bar, italic text

Web link: `bright_interactive` fg, underlined

File link: `dim_interactive` fg, underlined

Heading link: `dim_interactive` fg

Image link (when image not shown): `dim_interactive` fg, italicized

Unordered list marker: `bright_structural` fg

Ordered list number marker (including `.`): `bright_structural` fg

### Task list
- Incomplete item: `bright_emphasis` fg bullet and checkbox
- Complete item: `bright_success` fg bullet and checkbox; `bright_muted` fg text with strikethrough

Horizontal rule: `dim_structural` fg

### Table
- Header row: bold, default fg
- Borders: `dim_muted` fg (light `─` rules around and between cells)
- Header bottom separator: `dim_structural` fg — the heavy `━` rule below the header row is themed independently of the regular borders so the header reads as a structural divider
- Row striping (when `[table] row_striping = true`): odd data rows get `bright_surface` bg so the table chrome matches the inline-code surface

Inline code: `surface_bright` bg, `bright_structural` fg

### Code block
- Language: `bright_structural` fg, italicized
- Block: `surface_bright` bg
- Text: `default_text` fg (syntax highlighting later)

Footnote/reference marker (not implemented yet)
- `dim_structural` fg

### Status line: `surface_bright` bg
Mode chip (bold)
  - Preview: `dim_muted` bg, `surface_bright` fg
  - Rendered: `bright_primary` bg, `default_bg` fg
  - Raw: `bright_emphasis` bg, `default_bg` fg
File name: `default_text` fg
Dirty file marker (`*`): `bright_emphasis` fg, bold
Cursor coordinates, line count, etc: `bright_primary` fg

### Hint line: `surface_dim` bg
Preview hint (`Press any key to edit`): `default_text` fg
Hint chord: `bright_interactive` fg, bold
Hint label: `default_text` fg
Transient message: 
- Info: `default_text` fg
- Warning: `bright_warning` fg
- Error: `bright_error` fg

Cursor (in editor): Each mode has its own cursor style mirroring the status-line mode chip. The renderer picks via `theme.cursor_style(mode)`:
- Preview: `dim_muted` bg, `bright_surface` fg (matches the Preview mode chip)
- Rendered: `bright_primary` bg, `default_bg` fg
- Raw: `bright_emphasis` bg, `default_bg` fg

Cursor (modal text inputs): `theme.cursor` — REVERSED only, so the `▏` glyph inverts whatever's underneath without needing to know the modal's surface colour. Kept distinct from the editor cursor because modal inputs aren't tied to editor mode.

### Modal windows
Background: `surface_bright`
Title: `bright_primary` fg, bold
Border: `dim_structural` fg
Item: `default_text`
Item hint / sub-label (right-aligned chord, value column, etc. on an unfocused row): `bright_interactive` fg
Selected item: `dim_interactive` bg, `default_text` fg, bold
Selected item hint (right-aligned chord / value column on the focused row): `bright_emphasis` fg
Description (pinned-footer copy explaining the focused row, e.g. settings overlay bottom line): `bright_emphasis` fg
Input (unfocused): `default_bg` fg, `dim_interactive` bg
Input (focused): `default_bg` fg, `bright_interactive` bg; modal-input cursor inverts whatever's underneath
Focused button (e.g. Save / Discard / Cancel in the unsaved-changes confirmation): `bright_interactive` fill (REVERSED, bold)
Section heading: `bright_structural` fg, bold

#### Command palette input — exception
Unlike other modal inputs, the command palette's typing row sits flush
against the modal body — no coloured bg fill — and the `▏` cursor
glyph is `bright_interactive` so the typing affordance still pops.
This break is deliberate: the palette is a search affordance, not a
form field, so it reads as part of the modal rather than as a sunken
input chip.

## Follow-ups

UI elements the design guide doesn't yet pin down — flagged here so we
can return to them once the visual language settles.

- **Active-line highlight.** `theme.active_line` is wired in but
  defaults to `Style::default()`; a real hint suggested an "orange
  tint" but no concrete fg/bg has been picked. The `RenderedView`
  isn't yet painting any active-line band either — a future patch
  needs both the palette assignment and the renderer pass.
- **Search / find-in-document.** `theme.search_highlight` is in place
  (bright_structural bg, default_bg fg) for when the find feature
  lands. The find UI itself (input chrome, match counter, surrounding
  status banner) is not specified.
- **Footnote / reference markers.** `theme.footnote` is in place
  (dim_structural fg) but the renderer doesn't emit footnote markers
  yet. Once `pulldown-cmark`'s footnote inlines surface, hook the
  style up.
- **Settings overlay layout.** Settings rows are styled but the
  overall row layout (label width, value alignment) is informally
  specified; the cookbook for adding a new row lives in
  `src/ui/settings_overlay.rs::CATEGORIES`. Either codify here or
  link into the design guide.
- **Scrollbars.** Modals draw a textual `↑` / `↓` / `↑↓` indicator
  in the title; there's no narrow gutter scrollbar. Once we add one,
  it'll need a palette entry (probably `dim_structural` fg).
- **Diff / merge markers.** No styling for diff view (added /
  removed / context lines) — deferred until the editor grows a
  diff feature.
- **Image-placeholder underline.** The renderer adds `UNDERLINED`
  to the image's display name span on top of `image_placeholder`'s
  italic; theming.md only specifies italic. Decide whether the
  underline is by design or a leftover.
- **Surface naming.** The palette description in this file says
  "dark grey — UI surfaces (status line, modal, code block bg);
  slightly lighter grey — elevated UI surfaces (inputs, hint
  line)", but the rule list above uses `surface_bright` for the
  darker chrome (status / modal / code) and `surface_dim` for the
  elevated surfaces (hint / inputs). The implementation follows the
  rule list. Consider renaming the variants — `surface_chrome` and
  `surface_elevated` would read more naturally — once we revisit.
