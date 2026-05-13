# Edamame Design Guide and Default Theme

## Color palette — flat semantic names
The assigned colors describe edamame's default (256 Dark) theme. The
palette is a single named slot per role; focus / active / disabled
affordances layer text modifiers (BOLD, REVERSED, DIM) on top rather
than reaching for a second palette slot.

- `text`: white — default document foreground
- `text_muted`: light grey — peripheral text (h6, strikethrough body,
  completed-task text, modal close hint, Preview-mode chip bg)
- `bg`: black — default document background
- `bg_muted`: dark grey — inline-code background, table-row stripes,
  scrollbar track, Preview-mode cursor bg
- `surface`: slightly lifted dark grey — hint line and the
  transient-message strip that overlays it
- `surface_elevated`: heavier chrome dark grey — status bar, modal
  body, fenced code-block bg
- `primary`/brand: orange — identity, headings, mode chip, non-link
  focus affordances (selected modal row, modal input fill, button
  focus, scrollbar thumb)
- `secondary`: gold — structural chrome (section headings,
  search-highlight bg, rules, blockquote bar, footnote marker,
  command-palette divider)
- `accent`: blue — list markers, table header, modal description /
  selected-row hint, and the bg for text selection
- `link`: bright blue — web link, file link, heading link, image
  placeholder (reserved for link affordances only)
- `success`: green
- `warning`: yellow
- `error`: red
- `code`: purple — inline-code and code-block-language foreground
- The `h1`–`h6` headings are *derived* — they're not palette
  fields.  `Theme::from_palette` builds the ramp by alternating
  `primary` and `secondary` and dulling / darkening with each
  level: `h1` = primary bright, `h2` = secondary bright, `h3` =
  primary medium, `h4` = secondary medium, `h5` = primary dull,
  `h6` = secondary dull.  For RGB palettes the medium / dull
  shades are computed by darkening the base toward black.  Indexed
  palettes can't be cleanly darkened without shifting hue, so
  built-in 256-colour themes pin the ramp manually in their ctor
  (see `BUILTIN_THEMES`).  User themes can still override an
  individual heading by setting a `[h1]`..`[h6]` style section in
  their TOML.
- `diff_add` / `diff_delete`: reserved for a future diff view; no
  styles currently consume them

## Palette Assignments

Selected text: `accent` bg, `text` fg

Search/find highlight: `secondary` bg, `bg` fg

Active line highlight (not implemented): slightly lighter shade than
default bg (orange tint?)

### Headings
- H1: `primary` (bright), bold, setext trailing rule (`═`)
- H2: `secondary` (bright), bold, underlined
- H3: `primary` (medium), bold, underlined
- H4: `secondary` (medium), bold, underlined
- H5: `primary` (dull), bold, underlined
- H6: `secondary` (dull), bold, underlined

Strikethrough: `text_muted` fg, CROSSED_OUT

Highlight: `warning` bg, `bg` fg

Block quote: `secondary` left bar, italic text

Web link: `link` fg, underlined

File link: `link` fg, underlined

Heading link: `link` fg

Image link (when image not shown): `link` fg, italicized

Unordered list marker: `accent` fg

Ordered list number marker (including `.`): `accent` fg

### Task list
- Incomplete item: `warning` fg bullet and checkbox
- Complete item: `success` fg bullet and checkbox; `text_muted` fg
  text with strikethrough
- Inline code inside a checked task: `code` fg, `bg_muted` bg, DIM
  modifier — derives from the inline `code_span` style with DIM
  layered on so the snippet still reads as code while fading with the
  surrounding strikethrough

Horizontal rule: `secondary` fg

### Table
- Header row: bold, `accent` fg
- Borders: `surface_elevated` fg
- Row striping (when `[table] row_striping = true`): odd data rows
  get `bg_muted` bg
- Drop indicator (active drag target): `primary` fg
- Drop target (inert valid sites during drag): `primary` fg + DIM
- Row / column reorder + resize handles: `primary` fg + DIM
- Row / column delete handle: `error` fg

Inline code: `bg_muted` bg, `code` fg

### Code block
- Language: `code` fg on `surface` bg, italicized
- Block border + text: `text` fg on `bg_muted` bg

Footnote/reference marker (not implemented yet): `secondary` fg

### Status line: `surface` bg
Mode chip (bold)
  - Preview: `text_muted` bg, `surface` fg
  - Rendered: `primary` bg, `bg` fg
  - Raw: `warning` bg, `bg` fg
File name: `text` fg
Dirty file marker (`*`): `warning` fg, bold
Cursor coordinates, line count, etc: `primary` fg
Selection size indicator: `primary` fg, bold

### Hint line: `surface_elevated` bg
Preview hint (`Press any key to edit`): `text` fg
Hint chord: `primary` fg, bold
Hint label: `text` fg
Transient message:
- Info: `text` fg
- Success: `success` fg, bold
- Warning: `warning` fg, bold
- Error: `error` fg, bold

Cursor (in editor): Each mode has its own cursor style mirroring the
status-line mode chip. The renderer picks via `theme.cursor_style(mode)`:
- Preview: `bg_muted` bg, `surface_elevated` fg
- Rendered: `primary` bg, `bg` fg
- Raw: `warning` bg, `bg` fg

Cursor (modal text inputs): `theme.cursor` — REVERSED only, so the
`▏` glyph inverts whatever's underneath without needing to know the
modal's surface colour. Kept distinct from the editor cursor because
modal inputs aren't tied to editor mode.

### Modal windows
Background: `surface_elevated`
Title: `primary` (Normal) / `warning` / `error` fg, bold
Item: `text` fg on `surface_elevated`
Item hint / sub-label (right-aligned chord, value column, etc. on an
unfocused row): `primary` fg
Selected item (the row that currently has focus): `primary` bg, `text`
fg, bold
Selected item, unfocused (persistent selection on a row that does NOT
have focus — e.g. the active tri-state pill in a row whose label isn't
focused): `secondary` **fg** (no fill) on `surface_elevated`, bold.
See "Focus vs. persistent selection" below for the full convention.
Selected item hint (right-aligned chord / value column on the focused
row): `accent` fg on the `primary` selection fill
Description (pinned-footer copy explaining the focused row): `accent`
fg on `surface_elevated`
Input (unfocused): `bg` fg, `primary` bg
Input (focused): `bg` fg, `primary` bg, **bold** — focus is shown via
the BOLD modifier on top of the same `primary` fill
Focused button (e.g. Save / Discard / Cancel): `primary` fg + REVERSED
+ bold
Section heading: `secondary` fg, bold

Scrollbar
- Track: `bg_muted` fg (`│` glyph)
- Thumb (idle): `primary` fg (`█` glyph)
- Thumb (hover / drag): `primary` fg + REVERSED

#### Focus vs. persistent selection
Some modals carry **persistent selections** independent of which row
has focus — for example, the welcome modal's three tri-state rows each
remember an `Ask | Always | Never` value while focus moves around, and
the "Don't show this again" toggle remembers a checked / unchecked
state.  These differ from list-style modals (palette, settings,
keybinds) where focus and "the chosen row" are the same concept.

Convention for such modals — pick which tier each affordance belongs
to:

| State | Style | Theme field |
|---|---|---|
| Focused | `primary` bg + REVERSED + bold (filled, strongest) | `modal_button_focused` |
| Persistent selection without focus | `secondary` **fg** on `surface_elevated`, bold (outlined, no fill) | `modal_item_selected_unfocused` |
| Neither | plain `text` fg on `surface_elevated` | `modal_item` |

The filled-vs-outlined distinction makes the focus location and the
persistent selection independently scannable.  Two filled affordances
of the same colour read as ambiguous; an outlined "selected" with a
filled "focused" reads unambiguously.

For composite affordances (e.g. a checkbox toggle: `[x]` glyph + label
text), apply the `modal_item_selected_unfocused` style **only to the
glyph that carries the selection**, not the full row — the persistent-
selection cue should land on the smallest expressive surface so the
surrounding label stays legible.

Monochrome fallback: `modal_item_selected_unfocused` uses plain `DIM`
so it reads as "marked but quiet" — distinct from `BOLD` (focused
selection) and plain (unselected) without needing REVERSED (which is
already taken by the unselected `modal_item` state in monochrome).

#### Command palette input — exception
Unlike other modal inputs, the command palette's typing row sits flush
against the modal body — no coloured bg fill — and the `▏` cursor
glyph is `primary` so the typing affordance still pops. This break is
deliberate: the palette is a search affordance, not a form field, so
it reads as part of the modal rather than as a sunken input chip.

## Built-in themes
- [x] 256 Dark
- [x] 256 Light
- [ ] Edamame
- [ ] Monochrome
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
  (`secondary` bg, `bg` fg) for when the find feature lands. The find
  UI itself (input chrome, match counter, surrounding status banner)
  is not specified.
- **Footnote / reference markers.** `theme.footnote` is in place
  (`secondary` fg) but the renderer doesn't emit footnote markers
  yet. Once `pulldown-cmark`'s footnote inlines surface, hook the
  style up.
- **Diff / merge markers.** The palette ships `diff_add` and
  `diff_delete` slots so themes can pre-author the colours, but no
  rendered styles consume them yet. Wire them up when the diff
  feature lands.
- **Surface naming.** The palette description says `surface` is the
  lifted-chrome variant (hint / transient strip) and
  `surface_elevated` is the heavier chrome (status / modal / code
  block). The rule list above follows that convention; consider
  renaming the variants — `surface_chrome` and `surface_elevated`
  would read more naturally — once we revisit.
