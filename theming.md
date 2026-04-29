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
- `muted`: light grey — muted text, borders, backgrounds
### These do not have bright/dim variants
- `surface`: dark grey — UI surfaces (status line, modal, code block bg)
- `surface_alt`: slightly lighter than `surface` — Slightly elevated UI surfaces (inputs, hint line)

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

Highlight: (bright?) `dim_emphasis` bg, black fg

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
- Header row: bold, default fg, `dim_structural` bottom border
- Borders: `dim_muted` fg
- Row striping: `dim_muted`

Inline code: `surface_alt` bg, `dim_structural` fg

### Code block
- Language: `bright_emphasis` fg, italicized
- Block: `surface_alt` bg
- Text: `default_text` fg (syntax highlighting later)

Footnote/reference marker (not implemented yet)
- `dim_structural` fg

### Status line: `surface_alt` bg
Mode chip (bold)
  - Preview: `dim_muted` bg, `surface_alt` fg
  - Rendered: `bright_primary` bg, `default_bg` fg
  - Raw: `bright_emphasis` bg, `default_bg` fg
File name: `default_text` fg
Dirty file marker (`*`): `bright emphasis`
Cursor coordinates, line count, etc: `bright_primary` fg

### Hint line: `surface` bg
Preview hint (`Press any key to edit`): `default_text` fg
Hint chord: `interactive`, bold
Hint label: `default_text` fg
Transient message: 
- Info: `default_text` fg
- Warning: `bright_warning` fg
- Error: `bright_error` fg

Cursor (in editor): Same as status line mode bg

### Modal windows
Background: `surface_alt`
Title: `bright_primary` fg
Border: `dim_structural` fg
Item: `default_text`
Selected item: `interactive` bg, `default_bg` fg
Selected item hint: `bright_emphasis`
Input (unfocused): `default_text` fg, `surface` bg (lighter than modal background)
Input (focused): `dim_interactive` bg; `default_text` cursor
Section heading: `bright_emphasis` fg

