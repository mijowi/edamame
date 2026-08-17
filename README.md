# edamame

**A fast Markdown editor and viewer for the terminal.**

edamame shows your document *rendered* with headings, drawn table grids, inline images, and more — while you edit it. Only the line your cursor is on drops to raw Markdown, and it snaps back the moment you move away.

![The same document in edamame's rendered view and as raw Markdown](https://raw.githubusercontent.com/mijowi/mijowi.com/refs/heads/main/edamame/media/render_raw.jpg)

---

## Why

Markdown is the right format for most documents, and it's become the medium we use to work with AI agents — so we all read and edit a lot more of it than we used to.

The tools are mostly split. Electron apps render beautifully but feel slow. Text editors are fast but show you asterisks and pipe characters. Neovim plugins can decorate lines but can't restructure them, so a table stays a row of `|`. Most tools are good at *viewing* or *editing*, not both.

edamame is an attempt at all of it at once: rendered and editable at the same time, fast enough that it never gets in the way, and small enough to run over SSH.

## Demo

http://github.com/mijowi/mijowi.com/raw/refs/heads/main/edamame/media/edamame_demo_web.mp4

## Features

- **Hybrid rendered/raw editing** — the document stays formatted; only the cursor's line shows its source
- **Real table editing** — tables render as a grid, you edit cell by cell, `Tab` between cells, drag with the mouse to reorder or resize
- **Inline images and Mermaid diagrams** on terminals that support them
- **Search and replace**, with smartcase navigation
- **Diff review for external changes** — when something else writes your file, accept or reject each change hunk by hunk instead of losing work
- **Vim mode**, optional — motions, operators, text objects, `:s` with a live preview
- **27 themes**, and a documented format for writing your own
- **HTML export**, self-contained or linked
- **Footnotes, task lists, list continuation and renumbering**
- Mouse support, fuzzy command palette, jump-to-heading, navigation history

### The hybrid view

The document stays formatted while you work. The cursor's line shows its source, and renders again as soon as you leave it — so list markers, emphasis and links are editable in place without turning the page into syntax.

![Editing a list in edamame, with the cursor's line showing raw Markdown while the rest stays rendered](https://raw.githubusercontent.com/mijowi/mijowi.com/refs/heads/main/edamame/media/raw_reveal_and_list_ops.gif)

### Tables as grids

Tables are drawn, not printed as pipes. `Tab` walks the cells, rows and columns can be added or removed in place, and the mouse can drag a divider to resize or a handle to reorder.

![Reordering and resizing columns in an edamame table with the mouse](https://raw.githubusercontent.com/mijowi/mijowi.com/refs/heads/main/edamame/media/table_ops.gif)

### Themes

27 built in, and a documented TOML format for writing your own.

![A grid of edamame's built-in themes](https://raw.githubusercontent.com/mijowi/mijowi.com/refs/heads/main/edamame/media/themes.jpg)

## Install

### Homebrew

```bash
brew install mijowi/tap/edamame
```

### Cargo

```bash
cargo install edamame
```

### Prebuilt binaries

Download for your platform from [Releases](https://github.com/mijowi/edamame/releases).

### From source

```bash
git clone https://github.com/mijowi/edamame
cd edamame
cargo build --release
# binary at target/release/edamame
```

Requires **Rust 1.90 or newer**.

## Usage

```bash
edamame notes.md     # open a file
edamame              # start with an empty buffer
edamame --help       # the full flag list
edamame --doctor     # diagnostics to paste into a bug report
```

Then:

|Key | Action|
|---|---|
| **Any key** | Start editing (files open read-only) |
| `Ctrl-P` | Command palette — everything is here |
| `Ctrl-S` | Save |
| `Esc` | Back to reading |
| ``Ctrl-` `` | Toggle the whole document to raw Markdown |
| `Ctrl-Q` | Quit |

`Ctrl-C` copies — it doesn't quit.

## Terminal support

edamame runs anywhere, but a few things depend on your terminal:

| Feature | Requires |
|---|---|
| Images, diagrams, most themes | 24-bit color (plus an image protocol for images) |
| Mouse selection and table handles | Mouse reporting |
| ``Ctrl-` ``, `Ctrl-Shift-T`, `Alt-Shift-*` | The kitty keyboard protocol |

Where support is missing, edamame adapts rather than breaking — it switches to a 256-color theme, keeps image placeholders, and tells you which chords won't arrive. Every command is reachable from the palette regardless.

Full support in kitty, Ghostty, WezTerm, and foot. Apple Terminal works, with the reduced feature set above. Run `edamame --doctor` to see what your terminal reports.

## Documentation

- [Getting started](https://github.com/mijowi/edamame/blob/main/docs/getting-started.md)
  — first run, the three view modes, reading the status bar
- [Editing](https://github.com/mijowi/edamame/blob/main/docs/editing.md)
  — tables, lists, links, footnotes, search, diff review, images, export
- [Keybindings](https://github.com/mijowi/edamame/blob/main/docs/keybindings.md)
  — every default chord and how to change it
- [Configuration](https://github.com/mijowi/edamame/blob/main/docs/configuration.md)
  — every setting
- [Themes](https://github.com/mijowi/edamame/blob/main/docs/themes.md)
  — switching, and writing your own
- [Vim mode](https://github.com/mijowi/edamame/blob/main/docs/vim-mode.md)
  — what's supported and how it differs from Vim
- [Security](https://github.com/mijowi/edamame/blob/main/docs/security.md)
  — what protects you when you open a document you didn't write

## Security

edamame is built to open documents you didn't write. Image decoding is bounded against decompression bombs, remote image fetches are consent-gated and filtered against internal addresses, and HTML export strips scripts and unsafe link schemes. Details and the threat model are in [docs/security.md](https://github.com/mijowi/edamame/blob/main/docs/security.md).

To report a vulnerability, please use GitHub's private reporting on the [Security tab](https://github.com/mijowi/edamame/security) rather than a public issue.

## Contributing

Start with [CONTRIBUTING.md](https://github.com/mijowi/edamame/blob/main/CONTRIBUTING.md). For anything non-trivial, please open an issue first — edamame keeps a deliberately narrow scope, so a feature can be well-built and still not fit.

Questions, ideas and anything you're not sure is a bug belong in [Discussions](https://github.com/mijowi/edamame/discussions).

[AGENTS.md](https://github.com/mijowi/edamame/blob/main/AGENTS.md) is the architecture guide — the module layout, the invariants that are easy to break, and the code style. `docs/dev/` holds the design specs.

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## License

Apache-2.0. See [LICENSE](https://github.com/mijowi/edamame/blob/main/LICENSE).
