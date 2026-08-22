# Configuration

Every setting edamame reads, what it defaults to, and where to change it.

Most people never need this page — the settings overlay covers the common
options. Reach it from the command palette: **`Ctrl-P` → "Open settings"**.

---

## Where config lives

edamame follows the XDG convention on **every** platform, macOS included:

```
$XDG_CONFIG_HOME/edamame/     # if XDG_CONFIG_HOME is set to an absolute path
~/.config/edamame/            # otherwise
```

Inside it:

| File | Purpose |
|---|---|
| `config.toml` | Everything on this page |
| `keybindings.toml` | Key overrides — see [keybindings.md](keybindings.md) |
| `themes/` | Your custom themes — see [themes.md](themes.md) |
| `export/` | Custom stylesheets for HTML export |

All four are created on first run and **never overwritten afterwards**. The
shipped `config.toml` is heavily commented, so reading it is often faster than
reading this page.

To find the folder from inside edamame: `Ctrl-P` → "Open settings" → the first
row is "Open config folder". The second, "Open config.toml", opens the file in
`$VISUAL` / `$EDITOR`.

### When something is wrong with your config

Nothing about config loading is fatal. A missing file means defaults. A file
that doesn't parse means defaults plus a warning. An unrecognized key is kept
in the file and reported. Warnings appear in a modal at startup, naming the
file and the key — so a typo tells you rather than silently doing nothing.

Saving from inside edamame **preserves your comments, blank lines, and section
order** — settings are written surgically, not round-tripped.

---

## Appearance

| Key | Type | Default | Where |
|---|---|---|---|
| `theme` | string | `"Edamame"` | Theme picker |
| `appearance` | `"dark"` \| `"light"` | `"dark"` | Theme picker |

```toml
theme = "Edamame"
appearance = "dark"
```

`theme` is a built-in name or the filename stem of a file in `themes/`. See
[themes.md](themes.md) for the full list and how to write your own.

`appearance` filters the theme picker to matching themes and decides which
counterpart is previewed when you flip modes. It does not itself change colors.

Change both from the theme picker: `Ctrl-P` → "Switch theme". There is no
default chord for it, and no Appearance row in the settings overlay.

> On a first run in a terminal without 24-bit color, edamame seeds `theme = "256 Dark"` instead, because most themes quantize badly. If you later launch an existing config in such a terminal it substitutes an indexed-color theme **for that session only** — your `config.toml` keeps the theme you chose, so a config shared with a capable machine is never clobbered.

---

## `[editor]`

### Layout and display

| Key | Type | Default | Where |
|---|---|---|---|
| `line_wrap` | bool | `true` | file only |
| `code_block_wrap` | bool | `false` | file only |
| `preserve_blank_lines` | bool | `true` | file only |
| `show_line_numbers` | bool | `false` | overlay, palette |
| `big_h1` | bool | `false` | overlay, palette |
| `syntax_highlighting` | bool | `true` | overlay |
| `max_width_enabled` | bool | `false` | overlay, palette |
| `max_width_cols` | integer | `100` | overlay |

`line_wrap` wraps long lines at the terminal width. `code_block_wrap` is
separate because wrapped code is often harder to read than clipped code.

`preserve_blank_lines` renders runs of blank lines as written. Standard
Markdown collapses them to one; this keeps your spacing.

`big_h1` renders H1 titles as four-row block characters. It falls back to
normal rendering when the title is wider than the viewport or contains
non-ASCII characters.

`syntax_highlighting` colors fenced code blocks. The language comes only from the opening fence — edamame never guesses — so a fence with no language, or one naming a language it doesn't ship, renders as plain code. Token colors are theme fields (`syntax_keyword`, `syntax_string`, and the rest), so a custom theme can restyle or flatten them; see [themes.md](themes.md).

`max_width_cols` caps the content column and centers it, which helps on very
wide terminals. Values below **20** are clamped up. The status and hint rows
always span the full width. No effect when the terminal is already narrower
than the cap.

### Cursor and movement

| Key | Type | Default | Where |
|---|---|---|---|
| `visual_line_nav` | bool | `true` | overlay, palette |
| `cursor_blink` | bool | `true` | overlay, palette |
| `cursor_blink_ms` | integer | `530` | file only |
| `mouse_scroll_lines` | integer | `1` | overlay |

`visual_line_nav` makes `↑`/`↓` move by visual rows, so the cursor keeps its
screen column across a wrapped line. Set `false` to move by logical lines.

`cursor_blink_ms` is the half-period — the cursor toggles every this many
milliseconds.

`mouse_scroll_lines` is lines per wheel tick, and also governs trackpad
scrolling (where `1` usually feels best). Keyboard scrolling always steps one
line and ignores this.

### Saving

| Key | Type | Default | Where |
|---|---|---|---|
| `autosave_enabled` | bool | `false` | overlay, palette |
| `autosave_idle_ms` | integer | `5000` | file only |

Autosave is a debounce, not a timer: every keystroke resets the clock, so a
burst of typing produces one save at the end. A successful save flashes
"Autosaved". **Buffers with no filename never autosave** — there is nowhere to
write. Autosave is also suspended during diff review.

`autosave_idle_ms` must be **strictly between 1000 and 600000** (10 minutes).
Anything outside that range is rejected at load with a warning and the default
is used — so `0` can't quietly turn autosave into save-on-every-keystroke.

### External changes

| Key | Type | Default | Where |
|---|---|---|---|
| `diff_on_change` | bool | `true` | overlay, palette |
| `show_diff_intro` | bool | `true` | file only |

`diff_on_change` decides what happens when something else writes the file you
have open **and your buffer is clean**: `true` opens diff review so you see
each change; `false` reloads silently.

A buffer with unsaved edits always prompts, regardless of this setting.
edamame never silently discards your work. See
[editing.md](editing.md#when-the-file-changes-underneath-you).

`show_diff_intro` shows the explainer when diff review opens. The modal's
"Don't show this again" flips it off; set it back to `true` here to bring it
back — it is not in the settings overlay.

### Startup prompts

| Key | Type | Default | Where |
|---|---|---|---|
| `show_welcome` | bool | `true` | welcome modal |
| `check_for_updates` | bool | `true` | settings overlay, welcome modal |
| `last_update_check` | integer | `0` | written by edamame |
| `update_notified_for` | string | `""` | written by edamame |
| `seen_terminal_fingerprints` | list of string | `[]` | written by edamame |

`seen_terminal_fingerprints` records which terminals have already shown the
capabilities notice, so it fires once per new terminal rather than every
launch. **Clear the list to see it again** — useful after changing terminals or
upgrading one.

`check_for_updates` governs the automatic release check at startup. It runs at
most once every 24 hours — and never before the first-run welcome screen has
been answered, so turning it off there stops the first check too. It is silent
unless there is news: nothing is
shown when you are already on the latest release, and a given version raises
the notice only once. Turning it off disables **only** the automatic check —
"Check for updates" in the command palette, and the button of the same name on
the About page, always check on request. What the request does and doesn't send
is documented in [security.md](security.md).

`last_update_check` and `update_notified_for` are the bookkeeping behind those
two rules: when the last automatic check ran (Unix epoch seconds, `0` for
never), and which release tag you have already been shown. **Delete
`update_notified_for` to be reminded about the current release again.** Neither
is worth editing otherwise, and neither persists under `--no-config`, so that
flag re-checks on every launch.

---

## `[modal]`

| Key | Type | Default | Where |
|---|---|---|---|
| `handler` | `"default"` \| `"vim"` | `"default"` | overlay, palette, welcome |

```toml
[modal]
handler = "vim"
```

`"vim"` turns on modal editing — see [vim-mode.md](vim-mode.md).

Note that an unrecognized value is accepted without complaint and behaves as
`"default"`, so check your spelling if vim mode doesn't appear.

---

## `[table]`

| Key | Type | Default | Where |
|---|---|---|---|
| `show_buttons` | bool | `true` | overlay, palette |
| `row_striping` | bool | `true` | file only |
| `warn_on_width_injection` | bool | `true` | file only |

`show_buttons` draws the mouse handles on tables — `⠿` to drag rows and columns
into a new order, `⇔` to resize a column, `✕` to delete a row or column.
They're inert without mouse support, so edamame forces this off on a terminal
that has none.

`warn_on_width_injection` controls the confirmation shown the first time a
column resize would write a `<!-- tui-columns: [...] -->` comment into your
Markdown. That comment is how column widths persist; the modal's "Continue and
don't ask again" flips this off.

---

## `[images]`

| Key | Type | Default | Where |
|---|---|---|---|
| `enabled` | `"ask"` \| `"always"` \| `"never"` | `"ask"` | overlay |
| `remote_policy` | `"ask"` \| `"always"` \| `"never"` | `"ask"` | overlay |
| `max_width` | integer | `100` | file only |
| `max_height` | integer | `24` | file only |

`enabled` is the master switch. `"ask"` prompts the first time you open a
document containing images.

`remote_policy` separately governs `http(s)://` image URLs — the same idea as
an email client's "load remote images", and for the same reason: it stops a
document tracking you the moment you open it. It has no effect when `enabled`
is `"never"`.

`max_width` / `max_height` are ceilings in terminal cells. Images scale to fit
inside that box, keeping their aspect ratio.

> **Images need 24-bit color** as well as a supporting terminal. Below that, edamame declines to render them regardless of these settings — for that session only, so a config shared with a capable terminal keeps working. See [editing.md](editing.md#images).

---

## `[diagrams]`

| Key | Type | Default | Where |
|---|---|---|---|
| `enabled` | `"ask"` \| `"always"` \| `"never"` | `"ask"` | overlay |

Controls rendering of ` ```mermaid ` blocks. Deliberately independent of
`[images]` so you can opt into one without the other. Same 24-bit color
requirement.

---

## `[export.html]`

| Key | Type | Default |
|---|---|---|
| `stylesheet` | string | `"builtin"` |
| `inline_images` | bool | `false` |
| `diagrams` | bool | `true` |

These are the values the Export HTML modal opens with; whatever you pick in
that modal is written back here.

`stylesheet` is either the literal `"builtin"` or a path to a `.css` file. Drop
stylesheets into the `export/` folder beside `config.toml` and they appear in
the modal's picker. A `default.css.example` is written there on first run to
copy from.

`inline_images` base64-embeds local images into the HTML so the file is
self-contained. Off by default, partly because it makes large files and partly
because an embedded file leaves your machine when you share the export — only
images inside the document's own directory tree are ever inlined.

See [editing.md](editing.md#exporting-to-html).

---

## `[dev]`

| Key | Type | Default |
|---|---|---|
| `logging` | bool | `false` |

Writes `tracing` logs to your XDG data directory (e.g.
`~/.local/share/edamame/`). When off, the subscriber is never installed and
logging calls cost nothing. Useful when reporting a bug.

---

## Full example

A complete `config.toml` with non-default choices throughout:

```toml
theme = "Tokyo Night"
appearance = "dark"

[editor]
show_line_numbers = true
max_width_enabled = true
max_width_cols = 90
autosave_enabled = true
autosave_idle_ms = 3000
big_h1 = true
syntax_highlighting = true
cursor_blink = false

[modal]
handler = "vim"

[table]
row_striping = false

[images]
enabled = "always"
remote_policy = "never"

[diagrams]
enabled = "always"

[export.html]
inline_images = true
```
