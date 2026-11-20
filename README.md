# edamame

**A Terminal Markdown editor**

<!-- GitHub badges -->
<p>
  <a href="https://crates.io/crates/edamame"><img src="https://img.shields.io/crates/v/edamame.svg" alt="edamame on crates.io" height="20"></a>
  <a href="https://docs.rs/edamame"><img src="https://docs.rs/edamame/badge.svg" alt="edamame documentation on docs.rs" height="20"></a>
  <a href="https://github.com/mijowi/edamame/actions/workflows/ci.yml"><img src="https://github.com/mijowi/edamame/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI status for the main branch" height="20"></a>
  <a href="https://github.com/mijowi/edamame/blob/main/LICENSE"><img src="https://img.shields.io/crates/l/edamame.svg" alt="License: Apache-2.0" height="20"></a>
</p>

**Jump to:**
1. [Docs](#documentation)
2. [Installation](#installation)
3. [Terminal Support](#terminal-support)

![Animation of clicking through a rendered document to reveal the raw Markdown on each line](https://raw.githubusercontent.com/mijowi/mijowi.com/refs/heads/main/edamame/media/edamame_hero.gif)

edamame displays Markdown *rendered* with headings, drawn tables, inline images, and more, as you work on it. Just the current line is shown as raw Markdown, for easy editing.

---

## Demo

<https://github.com/user-attachments/assets/85e2a303-08d6-4122-9f1f-18975613a127>

---

## Features

- **Hybrid rendered/raw editing** — the document stays formatted; only the current line shows its source
- **First class tables** — tables render with borders and auto-sized columns, `Tab` cell by cell, add/move/delete rows and columns with one key or use the mouse
- **Inline images, rendered Mermaid diagrams and LaTeX math** (pure Rust, no extra install)
- **Search and replace**, with smartcase navigation
- **Diff review for external changes** — when another app writes to your file, accept or reject each change hunk by hunk
- **Vim mode**, optional — motions, operators, text objects, `:s` with a live preview
- **27 themes**, and a documented format for writing your own
- **HTML export**, and a hook for converting to other formats
- **Footnotes, task lists, list continuation and renumbering**
- Mouse support, fuzzy command palette, jump-to-heading, navigation history

### The hybrid view

The document stays formatted while you work. The current line is shown as raw markdown for easy editing, and renders when the cursor leaves it.

![Editing a list in edamame, with the cursor's line showing raw Markdown while the rest stays rendered](https://raw.githubusercontent.com/mijowi/mijowi.com/refs/heads/main/edamame/media/raw_reveal_and_list_ops.gif)

### Tables as grids

Tables are drawn, not printed as pipes. `Tab` walks the cells, rows and columns can be added or removed in place, and the mouse can drag a divider to resize or a handle to reorder.

![Reordering and resizing columns in an edamame table with the mouse](https://raw.githubusercontent.com/mijowi/mijowi.com/refs/heads/main/edamame/media/table_ops.gif)

### Themes

27 built in, and a documented TOML format for writing your own.

![A grid of edamame's built-in themes](https://raw.githubusercontent.com/mijowi/mijowi.com/refs/heads/main/edamame/media/themes.jpg)

## Documentation

- [Getting started](https://github.com/mijowi/edamame/blob/main/docs/getting-started.md)
- [Editing](https://github.com/mijowi/edamame/blob/main/docs/editing.md)
- [Keybindings](https://github.com/mijowi/edamame/blob/main/docs/keybindings.md)
- [Terminal compatibility](https://github.com/mijowi/edamame/blob/main/docs/terminal-compatibility.md)
- [Configuration](https://github.com/mijowi/edamame/blob/main/docs/configuration.md) — every setting
- [Themes](https://github.com/mijowi/edamame/blob/main/docs/themes.md)
- [Vim mode](https://github.com/mijowi/edamame/blob/main/docs/vim-mode.md)
- [Security](https://github.com/mijowi/edamame/blob/main/docs/security.md)

## Installation

### Homebrew

```bash
brew install mijowi/tap/edamame
```

### Cargo

```bash
cargo install edamame
```

Requires Rust 1.90 or newer.

### Shell installer

```bash
curl -LsSf https://github.com/mijowi/edamame/releases/latest/download/edamame-installer.sh | sh
```

Downloads the prebuilt binary for your platform, verifies its checksum, and places it in `~/.cargo/bin`, adding that directory to your `PATH` if it isn't already there. Set `EDAMAME_NO_MODIFY_PATH=1` to skip the `PATH` change. Linux (x86_64, aarch64, and a static musl build) and macOS (Intel and Apple Silicon) are supported.

### Prebuilt binaries

Download for your platform from [Releases](https://github.com/mijowi/edamame/releases). Windows (`x86_64`) gets a `.zip` archive with `edamame.exe`; no installer covers it.

### From source

```bash
git clone https://github.com/mijowi/edamame
cd edamame
cargo build --release
# binary at target/release/edamame
```

Requires Rust 1.90 or newer.

### Platforms

Linux and macOS are the supported platforms. Windows is best-effort: edamame passes its test suite there in CI, and each release has a prebuilt `x86_64` Windows binary, but nobody manually tests on Windows. Use [Windows Terminal](https://github.com/microsoft/terminal); the legacy console lacks the alternate screen and mouse support a TUI depends on. If you run it on Windows and something is off, please [open an issue](https://github.com/mijowi/edamame/issues).

### Upgrading

edamame checks for a new release once a day and tells you when one appears (see [getting started](https://github.com/mijowi/edamame/blob/main/docs/getting-started.md)). It does not update itself; use the same method you installed with:

| Installed with | Upgrade command |
|---|---|
| Homebrew | `brew upgrade edamame` |
| Shell installer | Re-run the installer one-liner above — it replaces the binary in place |
| Cargo | `cargo install edamame` (`cargo update` does not touch installed binaries) |
| Prebuilt binary | Download the new release and replace the file |

### Uninstalling

Homebrew and Cargo remove their own binaries — `brew uninstall edamame` or `cargo uninstall edamame` — but neither removes the config and data directories edamame creates (`~/.config/edamame` and, on Linux, `~/.local/share/edamame`; on macOS `~/Library/Application Support/edamame`).

For the shell installer, a manual binary copy, or to clean up those directories, there's an uninstall script:

```bash
curl -LsSf https://raw.githubusercontent.com/mijowi/edamame/main/uninstall.sh | sh
```

It removes the binary (unless Homebrew or Cargo manages it, in which case it prints the right command instead) and then **asks** before touching your config and data — keeping them by default. Add `--purge` to remove everything without prompting, or `--yes` to remove only the binary.

The uninstall script does not run on Windows, so instead delete `edamame.exe` from wherever you unpacked it, and optionally the config folder `%USERPROFILE%\.config\edamame` and the data folder `%APPDATA%\edamame`.

## Terminal support

edamame runs anywhere, but a few things depend on your terminal:

| Feature | Requires |
|---|---|
| Images, diagrams, math, most themes | 24-bit color (plus an image protocol for images) |
| Mouse selection and table handles | Mouse reporting |
| ``Ctrl-` ``, `Ctrl-Shift-T`, `Alt-Shift-*` | The kitty keyboard protocol |

Where support is missing, edamame falls back rather than breaking. It switches to a 256-color theme, shows image placeholders, and tells you which chords won't arrive. Every command is reachable from the palette regardless of keyboard support.

Full support in foot, Ghostty, iTerm2, kitty, and WezTerm. Apple Terminal works, with the reduced feature set above. See [Terminal compatibility](docs/terminal-compatibility.md).

## Security

edamame is hardened to protect you from documents you didn't write. Image decoding is bounded against decompression bombs, remote image fetches are consent-gated and filtered against internal addresses, and HTML export strips scripts and unsafe link schemes. Details and the threat model are in [docs/security.md](https://github.com/mijowi/edamame/blob/main/docs/security.md).

To report a vulnerability, please use GitHub's private reporting on the [Security tab](https://github.com/mijowi/edamame/security) rather than a public issue.

## Contributing

See [CONTRIBUTING.md](https://github.com/mijowi/edamame/blob/main/CONTRIBUTING.md). 

Questions, ideas and anything you're not sure is a bug belong in [Discussions](https://github.com/mijowi/edamame/discussions).

## License

Apache-2.0. See [LICENSE](https://github.com/mijowi/edamame/blob/main/LICENSE).
