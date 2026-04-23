# Edamame Editor — Development Plan

## Table of Contents

1. [Goals and Non-Goals](#goals-and-non-goals)
2. [Technology Stack](#technology-stack)
3. [Architecture Overview](#architecture-overview)
4. [Module Structure](#module-structure)
5. [Key Design Decisions](#key-design-decisions)
6. [Testing Strategy](#testing-strategy)
7. [Phase-by-Phase Implementation](#phase-by-phase-implementation)
8. [Deferred Features](#deferred-features)
9. [Open Questions](#open-questions)

---

## Goals and Non-Goals

### Goals
- Fast, jank-free Markdown editing and viewing in the terminal
- Full CommonMark + GFM support (tables, task lists, strikethrough, footnotes)
- A seamless hybrid rendered/raw editing experience — not a split view
- Frictionless table editing (user never edits raw table border syntax)
- Cross-platform: Linux (primary), macOS, WSL
- Clean, minimal UI — no clutter, nothing beside the point
- Architected from day one to support Vim/modal keybindings and theming, even before those are implemented

### Non-Goals
- Markdown *export* to HTML, PDF, etc. (out of scope)
- A full IDE / LSP integration
- Rendering Markdown flavours beyond CommonMark + GFM
- Collaborative editing

---

## Technology Stack

### Core

| Crate | Version | Purpose |
|---|---|---|
| `ratatui` | latest (0.29+) | TUI framework |
| `crossterm` | latest | Terminal backend, raw mode, event handling |
| `pulldown-cmark` | 0.13 | CommonMark + GFM parsing with source-map offsets |
| `ropey` | 2.x (beta) or 1.6 stable | Rope data structure for the text buffer |

### Editing

| Crate | Version | Purpose |
|---|---|---|
| `ratatui-textarea` | latest (ratatui org fork) | Text editing widget — used for UI input elements—not for editing the document
| `arboard` | 3.x | OS clipboard read/write (Phase 1); WSL fallback via `clip.exe`/`powershell` |

### Rendering & Display

| Crate | Version | Purpose |
|---|---|---|
| `ratatui-image` | 10.x | Image display (Sixel, Kitty, iTerm2, halfblocks fallback) |
| `unicode-width` | latest | Correct terminal column widths for Unicode characters |
| `unicode-segmentation` | latest | Grapheme cluster boundaries |

### Config & Persistence

| Crate | Version | Purpose |
|---|---|---|
| `serde` | 1 | Serialization framework |
| `toml` | 0.8 | Config file format |
| `dirs` | latest | XDG-compliant config/data paths |

### Utilities

| Crate | Version | Purpose |
|---|---|---|
| `notify` | 7.x | File-system watching (Phase 11) |
| `similar` | 2.x | Diff computation for inline change display (Phase 11) |
| `thiserror` | 2 | Ergonomic error types |
| `anyhow` | 1 | Error propagation in application code |
| `tracing` | 0.1 | Structured logging (to a file, never stdout) |
| `tracing-appender` | 0.2 | Non-blocking log file writer |
| `open` | 5.x | Open URLs in the system browser and local files via OS default (Phase 8) |
| `ureq` | 2.x | Blocking HTTP for remote image fetching (Phase 7); runs on a background thread |

### Testing

These are `[dev-dependencies]` only — not shipped in release builds.

| Crate | Version | Purpose |
|---|---|---|
| `insta` | 1.x | Snapshot testing for rendered output (AST → styled lines, widget frames) |
| `proptest` | 1.x | Property-based testing for `Buffer` and `SourceMap` invariants |
| `ratatui::backend::TestBackend` | (built-in) | Headless widget rendering; no real terminal needed |

### Library Candidates Reviewed and Decisions

- **`tui-markdown` / `md-tui`**: These render Markdown but do not support editing or expose the source-map information we need for hybrid editing. We will build our own renderer on top of `pulldown-cmark` instead.
- **`edtui` / `ratatui-code-editor`**: These are full editor widgets with their own opinionated design. We need precise control over how lines are rendered (raw vs. styled), so we build our own editor view that uses `ropey` directly.
- **`rat-widget`**: Useful widget collection, but we will pull in individual pieces as needed rather than taking it as a hard dependency.
- **`tui-syntax-highlight`**: Deferred until code syntax highlighting phase.

---

## Architecture Overview

The application is structured in horizontal layers. Higher layers depend on lower layers; no lower layer knows about a higher one.

```
┌──────────────────────────────────────────────────────────────────┐
│                        main / App                                │
│   - Initialises terminal, config, logging                        │
│   - Owns the ratatui event loop                                  │
│   - Routes crossterm events to the InputDispatcher               │
│   - Calls the Renderer to draw each frame                        │
├──────────────────────────────────────────────────────────────────┤
│                         UI Layer                                 │
│   EditorView · StatusBar · FilePicker · DiffOverlay · Popups     │
│   - Stateless ratatui widgets; receive references to Editor      │
├──────────────────────────────────────────────────────────────────┤
│                       Editor Layer                               │
│   - EditorState: owns Document, Cursor, History, Mode            │
│   - Processes Actions dispatched by the Input layer              │
│   - Smart editing logic: list auto-continuation, table editing   │
├──────────────────────────────────────────────────────────────────┤
│                     Input / Keybinding Layer                     │
│   - InputDispatcher: translates crossterm Events → Actions       │
│   - KeyMap: configurable key → Action binding table              │
│   - ModalHandler trait: swappable modal implementations          │
│     (DefaultHandler now; VimHandler deferred)                    │
├──────────────────────────────────────────────────────────────────┤
│                      Document Layer                              │
│   - Buffer: wraps ropey::Rope; exposes edit ops with positions   │
│   - ParsedDoc: pulldown-cmark AST + raw byte-span index          │
│   - SourceMap: bidirectional char-offset ↔ rendered-line index   │
│     (built from parse_offsets spans; owned here, not in markdown/)│
│   - History: undo/redo stack of edit deltas                      │
│   - Cursor: logical cursor (rope char offset + preferred column) │
│   - Selection: anchor + active rope offsets                      │
├──────────────────────────────────────────────────────────────────┤
│                   Markdown Rendering Layer                       │
│   - MarkdownRenderer: AST events → Vec<ratatui::text::Line>      │
│   - TableLayout: computes column widths, wraps cell content      │
│   - ImageResolver: maps image paths/URLs → ratatui-image state   │
├──────────────────────────────────────────────────────────────────┤
│                    Config / Theme Layer                          │
│   - Config: loaded from ~/.config/edamamey/config.toml      │
│   - Theme: colour palette, applied to every rendered element     │
│   - KeyMap: default bindings overridable in config               │
└──────────────────────────────────────────────────────────────────┘
```

### Event Loop

All input and background-task notifications are routed through a single `mpsc` channel as an `AppEvent` enum. This avoids blocking the main thread on `crossterm::event::read()` while background threads (file watcher, image loader) need to wake the loop.

```rust
enum AppEvent {
    Term(crossterm::event::Event),
    FileChanged(PathBuf),
    ImageReady(PathBuf, DynamicImage),
}

// Spawned once at startup:
// - crossterm thread: calls event::read() in a loop, sends AppEvent::Term
// - notify watcher: sends AppEvent::FileChanged (Phase 11)
// - image loader pool: sends AppEvent::ImageReady (Phase 7)

loop {
    terminal.draw(|frame| ui.render(frame, &editor_state))?;

    match rx.recv()? {
        AppEvent::Term(event) => {
            if let Some(action) = input_dispatcher.handle(event, &editor_state.mode) {
                editor_state.apply(action)?;
            }
        }
        AppEvent::FileChanged(path) => editor_state.handle_external_change(path),
        AppEvent::ImageReady(path, img) => editor_state.cache_image(path, img),
    }
    if editor_state.should_quit() { break; }
}
```

Background threads are spawned lazily: the image loader pool in Phase 7, the file watcher in Phase 11. The `AppEvent` enum gains new variants at those phases; earlier phases only ever see `AppEvent::Term`.

### Editor Modes

```
            ┌──────────────────────────────────────┐
            │             PreviewMode              │
            │  No cursor. No raw text. Read-only.  │
            │  Files open here.                    │
            └────────────┬─────────────────────────┘
                         │  click / start typing
                         │  (NOT on scroll alone)
                         ▼
            ┌──────────────────────────────────────┐
            │           RenderedMode               │
            │  Cursor visible. Current line (or    │
            │  current table cell) shown raw.      │
            │  Rest of document rendered.          │
            └────────────┬─────────────────────────┘
                         │  explicit toggle (e.g. Ctrl-`)
                         ▼
            ┌──────────────────────────────────────┐
            │             RawMode                  │
            │  Entire document shown as plain      │
            │  Markdown text. Standard editing.    │
            └──────────────────────────────────────┘
```

---

## Module Structure

```
edamame/
├── Cargo.toml
├── README.md
├── plan.md
├── overview.md
├── config/                         # Reference default config files (include_str!'d
│   ├── config.toml                 #   at build time, written to ~/.config/edamame/
│   ├── keybindings.toml            #   on first run if absent)
│   └── themes/
│       └── default.toml
└── src/
    ├── main.rs                     # Entry point: init terminal, load config, run app
    ├── app.rs                      # App struct, event loop, quit logic
    │
    ├── config/
    │   ├── mod.rs
    │   ├── config.rs               # Config struct; serde/toml loading + saving
    │   ├── keymap.rs               # KeyMap: Action enum, compiled-in default bindings, override merging from [keybindings] config
    │   └── theme.rs                # Theme: named colour palette; default + user themes
    │
    ├── document/
    │   ├── mod.rs
    │   ├── buffer.rs               # Buffer: wraps ropey::Rope; insert/delete/slice ops
    │   ├── cursor.rs               # Cursor: char offset, preferred visual column
    │   ├── selection.rs            # Selection: anchor..active range
    │   ├── history.rs              # History: undo/redo using edit deltas (not snapshots)
    │   ├── parsed_doc.rs           # ParsedDoc: re-parses on change, caches AST + source map
    │   └── source_map.rs           # SourceMap: offset ↔ rendered-line-index mapping
    │
    ├── editor/
    │   ├── mod.rs
    │   ├── state.rs                # EditorState: owns Buffer, Cursor, History, Mode
    │   ├── mode.rs                 # Mode enum: Preview, Rendered, Raw
    │   ├── actions.rs              # Action enum: all editor commands (Insert, Delete, …)
    │   ├── edit_ops.rs             # Primitive editing operations on Buffer
    │   ├── list_edit.rs            # Smart list continuation, renumbering
    │   └── table_edit.rs           # Table cell navigation, raw-cell editing, column resize
    │
    ├── input/
    │   ├── mod.rs
    │   ├── dispatcher.rs           # InputDispatcher: crossterm Event → Action
    │   ├── mouse.rs                # Mouse event parsing: clicks, drags, scroll
    │   └── modal/
    │       ├── mod.rs              # ModalHandler trait
    │       └── default.rs          # Default (non-modal) keybinding implementation
    │       # vim.rs goes here when deferred vim mode is implemented
    │
    ├── markdown/
    │   ├── mod.rs
    │   ├── parser.rs               # Thin wrapper: runs pulldown-cmark, returns Event stream
    │   ├── ast.rs                  # Typed AST built from pulldown-cmark events
    │   ├── renderer.rs             # AST → Vec<ratatui::text::Line<'_>> styled output
    │   ├── parse_offsets.rs        # Collect raw (byte_start, byte_end) spans from pulldown-cmark offset iter; consumed by document::SourceMap
    │   └── table_layout.rs         # Column width calculation, cell text wrapping
    │
    ├── ui/
    │   ├── mod.rs
    │   ├── editor_view.rs          # Main editor widget: dispatches to preview/rendered/raw
    │   ├── preview.rs              # PreviewView: pure rendered output + scrolling
    │   ├── rendered_view.rs        # RenderedView: hybrid rendered+raw-line view
    │   ├── raw_view.rs             # RawView: plain text editor, renders directly from ropey buffer
    │   ├── table_view.rs           # TableView: rendered table with raw-cell overlay
    │   ├── status_bar.rs           # StatusBar widget: mode, file, cursor pos, dirty flag
    │   ├── file_picker.rs          # FilePicker overlay widget
    │   ├── diff_overlay.rs         # DiffOverlay: inline red/green diff (Phase 11)
    │   └── image_view.rs           # ImageView: ratatui-image integration
    │
    └── terminal/
        ├── mod.rs
        └── capabilities.rs         # Probe terminal at startup: colour depth, mouse, images
```

---

## Key Design Decisions

### 1. Source-Map–Driven Hybrid Rendering

The central challenge of this editor is rendering most of the document as styled Markdown while showing the cursor line (or cursor table cell) as raw text. This is solved through a **source map**: a data structure that, for every rendered visual line, records the start and end char-offset in the underlying rope buffer.

`pulldown-cmark`'s `into_offset_iter()` exposes byte offsets for every parse event. We build the `SourceMap` from these offsets after every edit (debounced). This lets us:
- Given a cursor rope-offset → find which rendered line it belongs to → replace just that line with a raw text widget
- Given a mouse click on a rendered position → map back to a rope offset → place the cursor

**Incremental re-parsing**: For responsiveness, `ParsedDoc` is re-parsed on every edit but only the rendered output for changed regions is recomputed (dirty-region tracking on the AST).

### 2. Table Editing Strategy

Tables are the hardest Markdown feature to edit. The strategy:
- The table as a whole is always **rendered** using border-drawing characters; the user never sees `| --- | --- |`
- When the cursor is inside a table, `table_edit.rs` identifies **which cell** the cursor is in by consulting the source map
- Only that cell's content is shown in a small inline raw-input widget; all other cells remain rendered
- Tab/Shift-Tab move between cells; Enter confirms a cell edit
- Column widths are computed by `table_layout.rs` taking into account terminal width, minimum column sizes, and user-set widths (persisted per table via a comment marker). A column width comment should NOT be added UNLESS the user manually set column widths. Column widths set via the automatic rap algorithm should not be persisted in the raw markdown.
- Text in cells wraps within the column width; the row height expands accordingly
- The raw Markdown is kept in sync with every cell edit; the user never manually aligns columns

### 3. Rope-Based Text Buffer

`ropey` is used as the single source of truth for document text. All edits go through `Buffer` which wraps `ropey::Rope`. This gives:
- O(log n) insert/delete at any position — no reallocation
- Efficient line-indexed access for rendering
- Unicode-correct char/byte/line index conversion
- The basis for efficient undo (store delta ranges, not full snapshots)

### 4. Pluggable Modal Input System

The `ModalHandler` trait is defined as:
```rust
trait ModalHandler {
    fn handle(&mut self, event: KeyEvent, state: &EditorState) -> Option<Action>;
    fn name(&self) -> &str;
}
```

`DefaultHandler` is the only implementation in the initial release. `VimHandler` will be added as a deferred feature. The active handler is set via config and can in principle be hot-swapped. This means **no modal-specific logic leaks into `EditorState`** — mode state (e.g. Vim's Normal/Insert/Visual) lives entirely inside the handler.

### 5. Theming from Day One

All colour and style values are routed through the `Theme` struct. There are no hardcoded `ratatui::style::Color` literals in the UI layer. The default theme is defined in code and can be overridden via `~/.config/edamame/config.toml`. This means adding full theme support later requires only exposing the theme config keys — no refactoring.

### 6. Config File Architecture

Three files under `$XDG_CONFIG_HOME/edamame/` (fallback `~/.config/edamame/`):

```
~/.config/edamame/
├── config.toml          # [editor], [modal], [table], [image] + `theme = "<name>"`
├── keybindings.toml     # keybinding overrides
└── themes/
    ├── default.toml     # shipped default; written on first run if missing
    └── <custom>.toml    # add your own; select via `theme = "custom"` in config.toml
```

`Config::load()` reads all three files via `LoadedConfig` (`src/config/config.rs`).
Missing files are silently treated as empty — every key falls back to a compiled-in
default.  Parse errors propagate with the file path and line number so typos surface
immediately.  First run scaffolds all three default files via
`Config::ensure_default_files()` (never overwrites existing files).

`Config::save()` writes ONLY `config.toml`.  `keybindings.toml` and theme files are
user-authored and never touched by an ordinary save; the `Config` struct doesn't hold
those fields, so the invariant is type-enforced.

**Keybinding overrides** live in `keybindings.toml` as a flat TOML table mapping
action names to key strings, e.g.:

```toml
# ~/.config/edamame/keybindings.toml
Save = "ctrl+s"
ToggleRawMode = "ctrl+`"
Quit = "ctrl+q"
Undo = "ctrl+z"
Redo = "ctrl+y"
Cut = "ctrl+x"
Copy = "ctrl+c"
Paste = "ctrl+v"
```

`KeyMap` is initialised with the full set of compiled-in defaults, then any keys
present in `keybindings.toml` are applied on top, replacing only those bindings.
Action names are the string representation of the `Action` enum variants.  An
unrecognised action name or an unparseable key string is a hard error at startup
(not silently ignored), so the user knows immediately if they've made a typo.

**Themes** live in `themes/<name>.toml`.  The file format is one section per style
field on `Theme`, with `fg` / `bg` colour strings and per-modifier booleans:

```toml
# ~/.config/edamame/themes/default.toml
[h1]
fg = "magenta"
bold = true

[code_span]
fg = "yellow"
bg = 236          # indexed palette entry

[link_text]
fg = "#00afff"    # hex
underlined = true
```

Colours accept named values (`"magenta"`, `"darkgray"`, …), hex (`"#ff00aa"`),
indexed palette entries as either strings (`"236"`) or bare integers (`236`), and
`{ r = 0, g = 95, b = 175 }` RGB tables.  The shipped `themes/default.toml` is
regenerated from `Theme::default()` via the `#[ignore]`'d
`regenerate_default_theme_toml` test in `src/config/theme_file.rs`.  On monochrome
terminals (`ColourDepth::NoColour`) the theme file is ignored and the compiled-in
`Theme::monochrome()` palette is used.

**Quit confirmation**: The `Quit` action (`Ctrl-Q`) always shows a confirmation dialog (e.g. "Save changes? [Y]es / [N]o / [C]ancel") when there are unsaved changes. When the buffer is clean the app quits immediately. `Ctrl-C` is bound to `Copy` and does not quit. `Escape` is the cancel/dismiss key for modals and dialogs; it does not trigger quit. Note: in crossterm raw mode `ISIG` is disabled, so `Ctrl-C` arrives as a key event rather than SIGINT — we must always intercept it explicitly to prevent SIGINT killing the process and leaving the terminal in raw mode; mapping it to `Copy` satisfies this.

**Undo/redo keybindings**: The compiled-in defaults are `Ctrl-Z` for undo and `Ctrl-Y` for redo. `Ctrl-Shift-Z` is registered as a secondary redo binding when the terminal supports the kitty keyboard enhancement protocol (`PushKeyboardEnhancementFlags`); without this protocol, `Ctrl-Shift-Z` is indistinguishable from `Ctrl-Z` at the byte level. Terminals known to support it: kitty, Alacritty, WezTerm, Ghostty, foot. In terminals that don't, only `Ctrl-Y` is available for redo. The keyboard enhancement flag is activated as part of Phase 4 capability detection and `Ctrl-Shift-Z` is only registered as a redo binding when the flag is successfully set.

### 7. Logging Strategy

`tracing` output is never written to stdout/stderr, because those would corrupt the TUI output. If an error occurs, we will show a popup to the user. Logging to a file (`$XDG_DATA_HOME/edamame/debug.log`) is gated behind a `[dev] logging = true` flag in `config.toml` (default: `false`). When enabled, `tracing-appender` writes structured logs to the file; when disabled, the tracing subscriber is not initialised and no log file is created.

---

## Testing Strategy

**Philosophy**: Tests are written alongside or before each module (TDD). Each source file has an inline `#[cfg(test)]` module for unit tests. Integration tests live in `tests/`. Snapshot tests use `insta` and their fixtures live in `tests/snapshots/`.

**What to test at each layer:**

- **Document layer** (`buffer.rs`, `cursor.rs`, `history.rs`, `source_map.rs`): Pure data-structure logic. Unit test every public method. Use `proptest` for round-trip invariants (e.g. insert-then-delete returns original rope; source-map offsets are always monotonically increasing and within buffer bounds).
- **Markdown layer** (`parser.rs`, `ast.rs`, `renderer.rs`, `table_layout.rs`): Snapshot-test the rendered `Vec<Line>` output for a fixed set of Markdown inputs using `insta::assert_debug_snapshot!`. Column-width computation in `table_layout.rs` is unit-testable with table string fixtures.
- **Editor layer** (`edit_ops.rs`, `list_edit.rs`, `table_edit.rs`): Integration-style unit tests: construct an `EditorState`, dispatch a sequence of `Action`s, assert the resulting buffer content and cursor position. These are the most important TDD targets.
- **UI layer** (`editor_view.rs`, `status_bar.rs`, etc.): Use `ratatui::backend::TestBackend` + `Terminal::new(TestBackend::new(w, h))` to render widgets to a buffer and snapshot the cell output with `insta`.
- **Config layer** (`config.rs`, `keymap.rs`, `theme.rs`): Unit test TOML round-trips, missing-key fallback to defaults, and invalid-key error messages.

**What not to test:**
- Terminal capability detection (`capabilities.rs`) — depends on real terminal I/O; covered by manual smoke testing.
- Cross-platform clipboard — tested manually on each target platform.
- Mouse integration — covered by the manual acceptance criteria per phase.

**Running tests**: `cargo test` runs all unit and integration tests. `cargo insta review` is run after any change that affects snapshot output, to review and accept updated snapshots.

---

## Phase-by-Phase Implementation

### Phase 0 — Foundation
*Goal: a working skeleton that opens a file and renders it (read-only) with scrolling.*
*Status: **Complete** — 2026-04-12. 113 tests passing. Manual cross-platform smoke test pending.*

**Tasks:**
- [x] `cargo new edamame` — set up workspace with `Cargo.toml`
- [x] Add `ratatui`, `crossterm`, `pulldown-cmark`, `ropey`, `serde`, `toml`, `dirs`, `thiserror`, `anyhow`, `tracing`, `tracing-appender` as dependencies
- [x] Implement `Config` with serde/toml deserialization; load from XDG path with defaults fallback
- [x] Implement `KeyMap` in `config/keymap.rs`: define the `Action` enum and all compiled-in default key bindings; after loading `Config`, iterate the `[keybindings]` table and override defaults — error at startup on unknown action names or unparseable key strings
- [x] Implement `Theme` with default dark-mode colour palette wired to all rendered elements
- [x] Implement `Buffer` wrapping `ropey::Rope` with `load_file` / `save_file` / `insert` / `delete` / `line` / `line_count`
- [x] Implement `parser.rs` — parse a `&str` with pulldown-cmark, return typed AST
- [x] Implement `renderer.rs` — walk AST, produce `Vec<ratatui::text::Line>` with styling for: headings (H1–H6), bold, italic, code spans, fenced code blocks, blockquotes, bullet lists, horizontal rules, links (styled but not yet clickable)
- [x] Implement `PreviewView` widget — renders styled lines with vertical scrolling (no cursor)
- [x] Implement `StatusBar` — shows filename, line count, mode label
- [x] Implement basic `App` event loop: draw → read event → handle quit (Ctrl-Q / Ctrl-C with confirmation dialog), scroll with arrow keys / PgUp / PgDn / Home / End
- [x] Set up `tracing-appender` to write logs to file before TUI starts, gated behind `dev_mode` config flag (disabled by default)
- [x] Add `insta` and `proptest` as dev-dependencies in `Cargo.toml`
- [x] Write snapshot tests in `tests/renderer.rs` covering headings H1–H6, bold, italic, inline code, fenced code block, blockquote, bullet list, and horizontal rule — assert `Vec<Line>` output with `insta::assert_debug_snapshot!`
- [x] Write a `TestBackend` rendering test for `StatusBar` (filename, line count, mode label)
- [ ] Manual smoke test: open several `.md` files in a Linux terminal and in macOS/WSL to verify no visual regressions beyond what automated tests cover

**Acceptance criteria:** `edamame path/to/file.md` opens the file in preview mode, renders styled Markdown, scrolls smoothly, and quits cleanly.

**Implementation notes (deviations and additions vs. original plan):**

- **`src/lib.rs` added** — a library crate entry point was added (not in original plan) so that `tests/renderer.rs` and `tests/ui.rs` can import from `edamame::`. Required by Rust's integration test model; integration tests cannot reference a binary-only crate.
- **`src/editor/mode.rs` created in Phase 0** — the `Mode` enum (`Preview / Rendered / Raw`) was defined upfront to support type-safe mode handling in the app and status bar, even though only `Preview` is active in Phase 0.
- **`src/ui/editor_view.rs` added** — a top-level `EditorView` stateful widget was added to compose `PreviewView` + `StatusBar` and act as the root UI widget. Dispatching to the Phase 1 `RenderedView` and `RawView` will simply be new `match` arms here.
- **`src/terminal/capabilities.rs` stubbed** — minimal `Capabilities` struct created with `detect()` returning conservative defaults, ready for Phase 4 probing without any structural refactoring.
- **Full `Action` enum defined upfront** — all actions across phases (Phase 1–3: editing, clipboard, selection, undo) were added to the enum in `config/keymap.rs` immediately, so keybindings are stable from day one. Phase 0 actions are implemented; later-phase actions are no-ops until their phase.
- **Quit confirmation dialog deferred** — the plan specified "Ctrl-Q / Ctrl-C with confirmation dialog" for Phase 0. Since the Phase 0 buffer is always clean (read-only preview, never modified), quit is immediate. The confirmation dialog will be wired up in Phase 1 when the dirty flag becomes meaningful.
- **Extra dev-dependency: `tempfile`** — added for file I/O tests in `buffer.rs`; not listed in the original dependency table.
- **Extra dependencies: `unicode-width`, `unicode-segmentation`, `tracing-subscriber`** — `unicode-width` and `unicode-segmentation` added for correct Unicode column-width handling in the renderer; `tracing-subscriber` added to initialise the file-based logging subscriber. All were implied by the plan but not listed in the dependency table.
- **Parser: additional options enabled** — `ENABLE_TASKLISTS`, `ENABLE_FOOTNOTES`, `ENABLE_SMART_PUNCTUATION` flags added to the pulldown-cmark parser in addition to the planned `ENABLE_TABLES` and `ENABLE_STRIKETHROUGH`. Task list checkboxes (`[ ]` / `[x]`) are fully parsed and rendered.

---

### Phase 1 — Hybrid Rendered/Raw Editing
*Goal: editing in RenderedMode where the cursor line is shown raw, all other lines rendered.*

*Status: **Complete** — initial implementation 2026-04-12; follow-up fixes (see "To Fix" below) through 2026-04-16. 218 tests passing (134 unit + 38 editing integration + 24 renderer + 12 source_map (incl. 2 proptest) + 10 UI).*

**Tasks:**
- [x] Before implementing document-layer types: write unit tests for `Buffer` (insert, delete, boundary conditions, line indexing), `Cursor` (move left/right/up/down, preferred column behaviour at line ends), and `History` (undo/redo, undo past empty stack, redo cleared after new edit) — implement each module to make the tests pass
- [x] Write integration tests in `tests/editing.rs`: construct an `EditorState`, apply `InsertChar` / `Newline` / `DeleteChar` / `Undo` / `Redo` sequences, assert buffer content and cursor position after each step
- [x] Implement `SourceMap` — after parsing, build a `Vec<(usize, usize)>` of (start_offset, end_offset) per rendered line, using `pulldown-cmark`'s offset iterator
- [x] Write `proptest` round-trip tests for `SourceMap`: for any sequence of edits, every offset in the buffer maps to exactly one rendered line, and the ranges are non-overlapping and cover the full buffer
- [x] Implement `Cursor` — stores a rope char offset and a preferred visual column (for vertical movement)
- [x] Implement `Selection` — anchor + active rope offsets; None when no selection
- [x] Implement `History` — undo/redo stack; each entry is an `EditDelta { offset, removed: String, inserted: String }`; undo/redo reconstruct and replay deltas
- [x] Implement `EditorState` — owns `Buffer`, `Cursor`, `Selection`, `History`, `Mode`, `ParsedDoc`
- [x] Implement `actions.rs` — define `Action` enum: `InsertChar(char)`, `DeleteChar`, `DeleteWord`, `MoveLeft/Right/Up/Down`, `MoveLineStart/End`, `MoveDocStart/End`, `Newline`, `Undo`, `Redo`, `ToggleRawMode`, `EnterEditMode`, `ExitToPreview`, `Save`, `Quit`, etc.
- [x] Implement `edit_ops.rs` — apply `Action` variants to `EditorState`, updating buffer, cursor, and history
- [x] Implement `RenderedView` — for each visual line, check if it contains the cursor; if so, render a raw inline text input widget for that line (using a custom single-line widget that reads from the rope buffer); otherwise render the styled Markdown line
- [x] Implement `DefaultHandler` in `input/modal/default.rs` — map key events to `Action` values using a configurable `KeyMap`
- [x] Implement mode transitions: Preview → Rendered on typing, Rendered ↔ Raw on Ctrl-\`
- [x] Implement auto-scroll: keep cursor line visible when editing
- [x] Implement `Save` action: write buffer to disk via `Buffer::save_file`
- [x] Track dirty state; show `[modified]` in status bar
- [x] Implement basic clipboard: cut (Ctrl-X), copy (Ctrl-C), paste (Ctrl-V) (default keybinds); use OS clipboard via `arboard` if available, internal kill-ring otherwise
- [x] Verify that no framework default or scaffold code quits on Ctrl-C; remove any such behaviour so that Ctrl-C is handled solely as `Copy` and only Ctrl-Q triggers quit

**Implementation notes:**
- `SourceMap` uses block-granularity (not line-granularity): pulldown-cmark's offset iterator gives per-block byte ranges; `render_with_counts()` gives per-block rendered line counts. `covering_ranges()` absorbs blank-line gaps to guarantee complete coverage.
- Empty list items (e.g. `*\n` with no content) rendered 0 lines; fixed by rendering the bullet marker even when `item.blocks` is empty. `rendered_lines_for_block` also has a defensive fallback for any remaining edge cases.
- `DeleteWordForward` uses Emacs-style: deletes word chars then trailing whitespace.
- Clipboard tests use kill-ring state rather than OS clipboard to avoid parallel-test race conditions.

**Architectural decisions consolidated from Phase 1 follow-up work:**

These emerged from the "To Fix" iterations and are documented as gotchas in `AGENTS.md` ("Phase 1 Architectural Notes"). Summary for plan-reading agents:

- **Virtual blocks for blank lines**: `ParsedDoc::build` synthesises a one-byte block per blank line (leading, between-block, and trailing). Replaced the earlier use of `parse_offsets::covering_ranges` for cursor lookup, which silently absorbed blank-line bytes into adjacent blocks.
- **`per_block_own` vs. extended ranges**: `ParsedDoc` tracks both per-block *own* rendered line counts (used by `RenderedView` to size the raw-replacement region) and *extended* covering ranges (used for cursor-to-block lookup).
- **Jitter-suppression reveal**: `RAW_REVEAL_DELAY = 120 ms`; `RenderedView` keeps the cursor block fully rendered and overlays an inverted-cell cursor indicator at `(cursor_col, cursor_row)` until the delay elapses. App loop uses `recv_timeout(60 ms)` so the redraw fires without a keypress.
- **Single shared `line_render` module**: `PreviewView` and `RenderedView` both call `ui::line_render::render_line` for word-aware wrap and trailing-cell background fill (so styled blocks like code blocks extend full width).
- **NBSP padding in code blocks**: blank code-block lines pad with U+00A0, not space, to work around a ratatui `WordWrapper` (`trim: false`) bug that produces a spurious extra empty visual row for all-whitespace lines.a
- **Word-group undo merging**: `History::record` merges single alphanumeric inserts into the prior delta when contiguous. Cursor moves break the group.
- **Visual line navigation**: `move_up_visual` / `move_down_visual` and `line_render::render_line` share the same wrap algorithm via `visual_rows_of_str` / `sub_line_of_col`.
- **Per-line raw replacement (not per-block)**: `RenderedView` replaces only the single rendered line containing the cursor, not the whole block, when the reveal delay elapses.

**Known unfixed issues carried into later phases:**

- Scrolling beyond the last element in raw and hybrid edit modes (cursor stops at last line). Deferred to Phase 5 (Mouse Support); see "To Fix" entry.
- Click+drag text selection. Deferred to Phase 5 (Mouse Support).

**Acceptance criteria:** Can open a .md file, navigate with arrow keys, type to edit, undo/redo, save with Ctrl-S. The cursor line appears raw while the rest of the document is rendered. Switching to Raw mode shows the whole document as plain text.

---

### Phase 2 — Table Editing
*Goal: frictionless table editing; user never sees raw table border syntax.*
*Status: **Complete** — 2026-04-17. 297 tests passing (175 unit + 38 editing + 37 table + 24 renderer + 12 source_map + 11 UI). Cell-scoped raw reveal landed in `RenderedView`: when the cursor sits in a table row, only the active cell's span is overlaid with raw Markdown text while neighbouring cells and box-drawing borders stay rendered. The dedicated `TableView` widget, its snapshot tests, column-width persistence wiring, and mouse-driven row/column drag remain deferred to Phase 6 (Table Row/Column Drag and Column Resize).*

**Keybinding rationale:** Table editing borrows its keybinding scheme from Emacs org-mode — the most mature precedent for TUI table editing. The direction of an arrow key is the direction of the operation; `Shift` promotes a "move/reorder" into an "insert/grow" on that side. The result is a symmetric, low-collision set that doesn't clash with existing editor bindings, and that users of org-mode will find immediately familiar. `Action` variants exist for all table commands so users can remap them via `keybindings.toml`.

**Tasks — Navigation (seamless; no separate "table mode"):**
- [x] Arrow keys cross cell boundaries at cell edges: `←` at start-of-cell moves to the previous column's cell; `→` at end-of-cell moves to the next column's cell; `↑` / `↓` cross rows at cell top/bottom
- [x] Arrow-key navigation inside a wrapped cell uses visual-line movement (reusing `move_up_visual` / `move_down_visual`) before crossing cell boundaries
- [x] `Tab` advances to the next cell, wrapping to the first cell of the next row; appends a new row if invoked from the last cell of the last row
- [x] `Shift+Tab` moves to the previous cell, wrapping to the last cell of the previous row
- [x] `Enter` moves to the cell below in the same column; appends a new row if at the last row
- [x] `Shift+Enter` inserts a literal newline within the current cell (stored as `<br>` in the Markdown)
- [x] `InsertTab` and `Newline` actions dispatch to `TableNextCell` / `TableNextRow` when the cursor is inside a table (context check in `edit_ops`); outside a table they retain their existing behaviour

**Tasks — Structure editing (Alt + Arrow family):**

| Key | Action |
|---|---|
| `Alt+↑` / `Alt+↓` | Move current row up / down (swap with neighbour) |
| `Alt+←` / `Alt+→` | Move current column left / right (swap with neighbour) |
| `Alt+Shift+↑` / `Alt+Shift+↓` | Insert row above / below current |
| `Alt+Shift+←` / `Alt+Shift+→` | Insert column left / right of current |
| `Alt+Backspace` | Delete current row |
| `Alt+Shift+Backspace` | Delete current column |

- [x] Implement row reorder (`TableMoveRowUp` / `TableMoveRowDown`) — swap the cells of the current row with the adjacent row in the buffer; cursor follows the moved row
- [x] Implement column reorder (`TableMoveColumnLeft` / `TableMoveColumnRight`) — swap the cells of the current column across all rows, including the header and alignment row; cursor follows the moved column
- [x] Implement row insertion (`TableInsertRowAbove` / `TableInsertRowBelow`) — insert a new empty row with the correct number of cells; cursor moves into the first cell of the new row
- [x] Implement column insertion (`TableInsertColumnLeft` / `TableInsertColumnRight`) — insert a new empty column across all rows; update the alignment row to `---`; cursor moves into the header cell of the new column
- [x] Implement row deletion (`TableDeleteRow`) — delete the row containing the cursor; move cursor to the same column in the adjacent row (below if possible, else above); deleting the last data row leaves the header intact
- [x] Implement column deletion (`TableDeleteColumn`) — delete the column containing the cursor across all rows (including header and alignment row); cursor moves to the adjacent column
- [x] All structure operations are single atomic edits — one `Ctrl+Z` reverts each

**Tasks — Cell editing and rendering (reuses the Phase 1 hybrid-raw model):**
- [x] Implement `TableLayout` — given a GFM table AST node and available width, compute column widths (auto from content, min column width, user-set widths) and cell text wrapping
- [x] Implement `table_edit.rs` — given a cursor offset, detect if inside a table; identify which row/column; extract the cell content (raw Markdown between `|` delimiters)
- [x] Typing a `|` character within a cell is escaped as a literal character (raw `\|`), not treated as a column separator
- [x] Implement table-aware `Newline` action: pressing Enter at the end of a table (outside a cell) inserts a new paragraph, not a new table row
- [x] Write unit tests in `tests/table.rs` covering: cell content extraction for empty cells, cells with bold/italic, cells with code spans, and wide Unicode characters; column-width computation for various table widths; `|` escaping round-trip; every structure-edit action (insert/delete/reorder row and column) round-tripping to valid GFM
- [x] Implement cell-scoped raw reveal in `RenderedView` — when the cursor is in a table row, only the **active cell** shows raw Markdown text; all other cells in the row keep their box-drawing borders and rendered inline styles. This extends the existing row-reveal branch in `rendered_view.rs`; it does **not** introduce a new `TableView` widget. Per-cell column ranges are derived on-the-fly from the already-rendered `│` pipe positions (and matching raw `|` positions with `\|` escapes), so no new metadata needs to be threaded through the renderer. Falls back to the full row-reveal when raw cell text is wider than the rendered cell area or when rendered/raw pipe counts disagree (e.g. the alignment row). Closes the Phase 2 goal that the user never sees the surrounding row's raw `|` separators while editing a cell.
- [x] Write a `RenderedView` test that constructs a table, places the cursor in the middle cell of the header row, advances past `RAW_REVEAL_DELAY`, and asserts that neighbouring cells still contain their rendered box-drawing glyphs while the active cell's span shows raw text (use `TestBackend` + `ratatui::buffer::Buffer` cell inspection; no new snapshot file needed) — implemented as `rendered_view_cell_scoped_reveal_keeps_neighbouring_pipes_rendered` in `tests/ui.rs`

 **Deferred work:**
- [ ] Write `insta` snapshot tests for `TableView` rendering (box-drawing output for a 2×3 and a 3×3 table) — **deferred with `TableView` itself to Phase 6**
- [ ] Implement column width persistence: store per-file column widths in a trailing HTML comment `<!-- tui-columns: [20, 15, 30] -->` within the table; parse and apply on load (only for user-set column widths) — parse/format implemented in `table_layout`; wiring into the renderer/buffer pipeline is deferred until column-resize (Phase 6) produces user-set widths worth persisting
- [ ] Implement `TableView` widget — renders a table using box-drawing characters (e.g. `┌─┬─┐`); handles multi-line cells by expanding row height — **deferred to Phase 6**; `renderer::render_table` already draws box-drawing borders and auto-sizes, and Phase 2's cell-scoped raw reveal lives inside `RenderedView` rather than a new widget. A dedicated `TableView` is only justified once mouse-driven cell selection and drag-to-resize land together and require a widget that owns cell hit-testing state.
- [ ] Phase 4 capability detection should log a warning if the terminal cannot distinguish `Alt+Shift+Arrow` from `Alt+Arrow` (users of bare VT100 / basic Linux console will lose the insert bindings) — deferred to Phase 4

**Acceptance criteria:** Opening a file with a GFM table shows a rendered bordered table. Arrow keys, `Tab` / `Shift+Tab`, and `Enter` navigate cells seamlessly. `Alt+Arrow` reorders the current row or column; `Alt+Shift+Arrow` inserts a new row or column on the indicated side; `Alt+Backspace` and `Alt+Shift+Backspace` delete the current row or column. Column widths adjust sensibly. The underlying Markdown is valid and well-formed after every edit, and every structure operation is undoable with `Ctrl+Z`.

**Implementation notes (Phase 2):**

- **Byte-offset / char-offset boundary**: `table_edit.rs` operates on byte offsets throughout (it walks a `&str` representation of the table region), while `Buffer` / `Cursor` use rope char offsets. `edit_ops::apply_byte_delta` is the single translation point: it converts the byte-offset `EditDelta` returned by `table_edit` into a char-offset delta via `rope.byte_to_char()` before calling `state.apply_delta`, and converts the post-edit cursor target back through the mutated rope. No other caller in `edit_ops` should talk directly to the byte API.
- **Adjacent-only row/column swaps**: `TableMoveRowUp/Down` and `TableMoveColumnLeft/Right` only swap with the immediate neighbour. Multi-step moves require multiple keypresses. This keeps each action a simple, symmetric `EditDelta` and avoids ambiguity when the cursor is between two candidate neighbours.
- **Alignment row (index 1) is protected**: the GFM alignment row (`|---|---|`) is never a navigation target and cannot be deleted or inserted-above. `TableNextRow` jumps from header (row 0) to the first data row (row 2), skipping row 1; `TableDeleteRow` refuses `row < 2`; `TableInsertRowAbove` at row 1 is rejected (user should use `TableInsertRowBelow` on the header instead).
- **Last-column deletion refused**: `TableDeleteColumn` refuses to act when only one column remains; removing it would produce invalid GFM (`| |\n|---|`).
- **`Action::TableInsertBreak` added**: `Shift+Enter` dispatches `TableInsertBreak`, which inserts a literal `<br>` inside a cell and a normal `\n` outside a table. `shift+tab` → `TablePrevCell` and `shift+enter` → `TableInsertBreak` are the only new default bindings added in Phase 2; the rest of the `Action` enum was already present from Phase 0.
- **Cell-based horizontal navigation**: in-table `MoveLeft`/`MoveRight` go through `edit_ops::table_move_horizontal`, which treats each cell as the contiguous range `[cell_first, cell_end]` of valid cursor positions — content characters plus the trailing pad space (the "cell-end" position where typing appends). Stepping past `cell_end` or before `cell_first` hops directly to the adjacent cell's `cell_end`, wrapping across rows via `adjacent_cell` (which skips the alignment row). Column separator `|`, leading pad, outer `|` borders, and newlines are all skipped — they are never valid cursor positions. Outside a table the function returns `false` and ordinary char-by-char movement takes over; the alignment row also falls through so it stays hand-editable. All cell jumps (Tab, Shift+Tab, Enter, arrow-key wraps, and structural-edit landing) land on `cell_end` so the user can immediately start typing to append.
- **Key-event normalization strips kitty-protocol state flags**: `KeyMap::action_for` constructs `KeyEvent::new(event.code, event.modifiers)` before the HashMap lookup. The kitty keyboard protocol (enabled in `terminal::setup` via `KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES`) attaches non-default `state` flags (e.g. `KEYPAD`) to events; since `KeyEvent`'s `PartialEq` / `Hash` compare *all four* fields, raw lookup would miss bindings that were inserted via `parse_key` (which always produces `state: EMPTY, kind: Press`). The normalization also covers any exotic `kind` values by forcing `Press`.
- **`|` escaping is context-sensitive**: `InsertChar('|')` is escaped to `\|` only when `cursor_in_table(state)` is true. Outside a table it inserts a literal `|`. Typing `|` in a table cell therefore produces the right raw Markdown without the user having to think about it.
- **Pure layout module (`src/markdown/table_layout.rs`)**: width computation, cell wrapping, and the `<!-- tui-columns: [...] -->` comment parser/formatter live here with no `ratatui` dependency, which is why they unit-test cleanly. Rendering of the box-drawing borders still lives in `renderer::render_table`; the two modules stay separate so that Phase 6's drag-to-resize can plug into `table_layout` without rewriting the renderer.
- **Virtual blank-line blocks preserved**: the Phase 1 virtual-block mechanism is unchanged — table navigation never synthesises or discards blocks, it only mutates the rope contents of an existing table block.
- **Cell-scoped raw reveal is a `RenderedView` extension, not a widget split**: the active-cell raw overlay is implemented by extending the existing row-reveal branch in `rendered_view.rs` — when `is_table` and the current block is the cursor's block, rewrite only the column range corresponding to the active cell instead of the entire row. `renderer::render_table` gains a sibling helper (or a parallel return value) that reports per-cell screen column ranges keyed by (table_row, table_col) so `RenderedView` can splice raw text into one cell without redrawing the table. This deliberately stays inside `RenderedView` + the existing renderer so there is no second rendering path to keep in sync with `render_table`.
- **Deferred to Phase 6**: the dedicated `TableView` widget, snapshot tests for `TableView`, mouse-driven row/column drag and column resize, and wiring `tui-columns` comments into the renderer/buffer pipeline (the module can already read and write them). Phase 2 deliberately kept cell-boundary computation on-demand inside `rendered_view.rs` rather than introducing persistent metadata — Phase 6 extracts the private pipe-position / cell-range helpers (`raw_pipe_positions`, `rendered_pipe_positions`, `table_raw_col_to_rendered_col`, `compute_cell_overlay`) into `table_layout` so `TableView`'s mouse hit-testing and `RenderedView`'s reveal share one implementation.

### Phase 3 — Smart List Editing
*Goal: numbered lists auto-continue and self-heal.*
*Status: **Complete** — 2026-04-17. 349 tests passing (188 unit + 38 editing + 37 list_edit + 26 renderer + 12 source_map + 37 table + 11 UI). `src/editor/list_edit.rs` follows the `table_edit.rs` byte-oriented pattern: `find_list_at` scans the cursor line's indent/marker family, `continue_item` / `exit_list` / `toggle_checkbox` produce `EditDelta`s, and `edit_ops` converts byte ↔ char via `apply_byte_delta`. Auto-renumber runs after every editing action that changes the buffer (detected via length or history-depth delta) so ordered-list markers in the raw Markdown stay monotonic; Undo/Redo are exempt. Follow-up fixes landed: empty task items render with their `[ ]` box; Backspace at `content_start` deletes the entire marker prefix and merges with the preceding line; horizontal cursor movement and a post-edit clamp treat the marker as non-navigable; `parse_key` now accepts `space`, so the default `ctrl+space` → `ToggleCheckbox` binding resolves.*

**Tasks:**
- [x] Before implementing, write tests in `tests/list_edit.rs` covering: bullet list continuation, numbered list continuation with correct next number, double-Enter exits the list, inserting an item mid-list renumbers subsequent items, nested lists at multiple indentation levels, task list continuation (`- [ ] `), and toggle-checkbox (`[ ]` ↔ `[x]`) — implement `list_edit.rs` to make each test pass
- [x] Implement `list_edit.rs` — detect when cursor is at the end of a list item line
- [x] On `Newline` inside a bullet list item: insert `- ` (or matching bullet character) at the start of the new line
- [x] On `Newline` inside a numbered list item: insert `N. ` where N is the correct next number
- [x] On `Newline` on a blank list-item line (i.e. pressing Enter twice): exit the list by removing the list prefix and inserting a blank paragraph
- [x] Implement list renumbering: after any insert/delete/paste that changes a numbered list, scan the list and re-number all items sequentially
- [x] Implement renumbering on paste: if a block of lines is pasted into the middle of a numbered list, renumber the whole list
- [x] Handle nested lists: detect indentation level; continue the list at the same level
- [x] Handle task list items: `- [ ] ` → `- [ ] `; `- [x] ` → `- [ ] ` (new unchecked item)
- [x] Implement toggle-checkbox action (Ctrl-Space or T when cursor is on a task list item): toggles `[ ]` ↔ `[x]`

**Acceptance criteria:** Typing in a numbered list auto-continues with the correct next number. Pressing Enter twice exits the list. Inserting items into the middle of a list renumbers subsequent items correctly. Nested lists work at multiple indentation levels.

**Implementation notes (Phase 3):**

- **Byte-offset / char-offset boundary**: `list_edit.rs` operates on byte offsets throughout (it walks a `&str` slice of the buffer), while `Buffer` / `Cursor` use rope char offsets. `edit_ops::apply_byte_delta` is the single translation point; `list_handle_newline`, `list_toggle_checkbox`, and `list_renumber_at_cursor` are the only callers and each produces a `ContinueResult { delta, cursor_byte }` that round-trips through `apply_byte_delta`.
- **List detection is cursor-line-indent-scoped**: `find_list_at` reads the cursor's line and, if it parses as a list-item line, scans up and down for contiguous items at the *same indent and marker family*. Blank lines, differently-indented lines, and lines with a different marker family all terminate the run. This means nested lists are naturally handled — the cursor's list is always the innermost list at the cursor's own indent level — without having to track nesting state explicitly.
- **Empty-item exit is dispatched before continue**: `list_handle_newline` checks `item.content_is_empty()` first; when true it routes to `exit_list` (replaces the entire empty-item line with a single `\n`, leaving a blank paragraph). Otherwise it routes to `continue_item`. This is why the double-Enter flow works without any modal state.
- **Continuation inside the marker falls through**: when the cursor is before `marker_end` (i.e. inside the indent, the marker characters, or the trailing space of the marker), `continue_item` / `list_handle_newline` return `false` and `edit_ops` inserts a plain `\n`. Placing a list marker in the middle of another marker would produce malformed output.
- **Single atomic `EditDelta` per action**: every list-aware edit — continue-item, exit-list, toggle-checkbox, renumber — is one `EditDelta`, so `Ctrl+Z` reverts it in one step. Renumbering subsequent items after a continue-item is folded into the same delta (the insertion's `inserted` string contains both the new item and the rewritten marker prefixes of every later item).
- **Task-list new items are always unchecked**: on Enter after `- [x] done`, the continuation is `- [ ] `, not `- [x] `. This matches the common editor convention that the user chooses the state of each new task.
- **Paste-renumber runs after every paste**: `Action::Paste` calls `list_renumber_at_cursor` after the paste applies. If the cursor is now in an ordered list, `renumber_list` rewrites every item's marker so the sequence is monotonic starting from the first item's number. Bullet lists and non-list pastes are no-ops. `renumber_list` short-circuits when the sequence is already consistent, so the common "paste doesn't land in a list" path is cheap.
- **Ordered-list delimiter is preserved per list**: `1. ` and `1) ` are distinct marker families. Continuing a `1) ` list produces `2) `, not `2. `. The `matches_list_line` predicate checks both indent and delim char.
- **Post-action invariants (Phase 3 fixes)**: at the end of `edit_ops::apply()` the engine does two sweeps when the buffer was mutated in `Rendered` mode — `list_renumber_at_cursor` (self-healing ordered-list numbering) and `clamp_cursor_out_of_marker` (snap the caret to `content_start` if an action left it on a marker, e.g. after `DeleteLine`). Both are gated on `(buffer length changed OR history depth changed)` so no-op actions like cursor movement skip the pass, and both are skipped for `Undo` / `Redo` so those remain exact inverses of the recorded deltas.
- **Marker as an atomic prefix**: horizontal arrow-key navigation (`list_move_horizontal`) and `Backspace` at `content_start` (`list_backspace_consumes_marker`) both treat the whole marker prefix — indent + `- ` / `N. ` + optional `[ ] ` — as a single indivisible unit. The cursor cannot land inside it, and the user cannot peel it off one character at a time. This is what makes list editing feel "managed" rather than textual.
- **`parse_key` accepts `space`**: the bound key string `ctrl+space` failed silently in Phase 2 because `parse_key` only recognised single characters and a handful of named keys. `space` is now mapped to `KeyCode::Char(' ')`, so the default `ctrl+space` → `ToggleCheckbox` binding actually resolves.
- **Empty task items render with `[ ]`**: `renderer::render_list` previously emitted only the marker text when a list item's blocks were empty. For task items the "marker" is just the indentation, so an empty `- [ ] ` line rendered as an invisible row. The fix appends the `task_prefix` span in the empty-blocks branch.

---

### Phase 4 — Capability Detection ✅
*Goal: detect what the terminal supports and gate features accordingly.*

**Tasks:**
- [x] Implement `terminal/capabilities.rs` with a `Capabilities` struct: `{ colour_depth: ColourDepth, mouse: bool, image_protocol: Option<ImageProtocol>, unicode_full: bool, keyboard_enhancement: bool }` — where `ImageProtocol` is an enum (`Sixel`, `KittyGraphics`, `ITerm2`, `Halfblocks`); `keyboard_enhancement` indicates whether `PushKeyboardEnhancementFlags` succeeded (required for `Ctrl-Shift-Z` redo)
- [x] Probe at startup using crossterm queries and environment variable heuristics (`$TERM`, `$COLORTERM`, `$TERM_PROGRAM`, `$KITTY_WINDOW_ID`, etc.)
- [x] Use `ratatui-image`'s `Picker` API for image protocol detection (this handles the detailed probing)
- [x] Store capabilities in `App` and thread them through to features that need them
- [x] Log detected capabilities to the tracing log file
- [x] Graceful degradation: if no colour support, render without ANSI styles (`Theme::monochrome()`); if no mouse, disable all mouse features without error
- [x] Show a popup modal notice if any features (e.g. mouse) are not available, with `[Ok]` and `[Don't show this again]`. The latter sets `editor.suppress_capability_warnings = true` in the config file via the new `Config::save()`. The `ui::modal` widget is generic enough to host the settings panel and confirm dialogs in later phases.

**Acceptance criteria:** `edamame` starts correctly in a minimal `xterm` (no mouse, 8 colours) and in a feature-rich terminal like Ghostty or Kitty, adapting its behaviour in both cases.

**Implementation notes:**
- `ratatui-image` is pinned to `9.*` (not `10.*`) because 10 requires ratatui 0.30; default features are disabled to skip the `libchafa` system dependency (we only need the probing Picker, not halfblock rendering).
- `terminal::setup()` now returns a `TerminalSetup { terminal, keyboard_enhancement }` struct so the App can tell whether kitty keyboard enhancement was actually enabled (crossterm's `supports_keyboard_enhancement()` can disagree with the actual push operation on some terminals, so we trust the push result).
- Capability detection runs between `setup()` and `app.run()` so the Picker's escape-sequence probes aren't eaten by the App's event-reader thread.
- Config persistence uses `toml::to_string_pretty` and creates `~/.config/edamame/` on demand.
- Picker lifecycle: Phase 4 currently discards the `Picker` after extracting `ImageProtocol`. Phase 7 changes this — `Capabilities` will hold an `Option<Picker>` so image rendering can reuse the already-probed instance instead of re-running `Picker::from_query_stdio` on every cold image load. Noted here so the two phases stay consistent.

---

### Phase 5 — Mouse Support ✅
*Goal: full mouse interaction — clicks, drags, scrolling, checkboxes.*
*Status: **Complete** — 2026-04-17. 399 tests passing (229 unit + 38 editing + 37 list_edit + 37 table + 26 renderer + 12 source_map + 11 UI + 9 new mouse). New modules: `src/input/mouse.rs` (click-count / drag state machine producing `MouseAction`), `src/editor/mouse_ops.rs` (applies `MouseAction` to `EditorState` — placement, selection, scroll, checkbox, link hit-test).*

**Tasks:**
- [x] Enable `crossterm::event::EnableMouseCapture` on startup (if `capabilities.mouse`)
- [x] Implement `mouse.rs` — parse `MouseEvent` variants: `Down`, `Up`, `Drag`, `ScrollUp`, `ScrollDown`
- [x] Click in PreviewMode → transition to RenderedMode, place cursor at clicked position (via source map)
- [x] Click in RenderedMode → move cursor to clicked position; if clicking a different table cell, switch active cell — cell switching falls out naturally: the click places the cursor in the target cell's byte range, and existing table navigation takes over
- [x] Click-drag → begin text selection; update selection while dragging
- [x] Double-click → select word under cursor
- [x] Triple-click → select line under cursor
- [x] Scroll wheel → scroll view (in PreviewMode and RenderedMode when document is longer than screen)
- [x] Click on a rendered link → open in browser / navigate to local file (Phase 8 prerequisite, but register the hit-test region here) — Phase 5 detects the click and logs the URL via `tracing::info!(target: "mouse")`; Phase 8 replaces the log with an OS-open invocation
- [x] Click on a task list checkbox `[ ]` / `[x]` → toggle it
- [x] Drag table row handle (leftmost column, rendered as `≡`) to reorder rows (prerequisite for Phase 6) — the generic `Click`/`Drag`/`Release` state machine is in place; Phase 6 adds drag-target classification (via a `DragTarget` enum replacing `drag_anchor`), the handle glyph in `TableView`'s external gutter, and the `table_edit::swap_rows` dispatch on `Release`
- [x] In terminals with mouse support, scrolling with the mouse should only move the page, not the cursor. Cursor stays put; `mouse_ops::scroll_by_mouse` never invokes `clamp_cursor_to_viewport_top`.
- [x] Smooth scroll via `WHEEL_STEP = 3` lines per wheel tick (GUI convention; feels continuous without cursor-motion side-effects).
- [x] Allow scrolling up to one page below the last line with the mouse only — `scroll_by_mouse` uses `max = total - 1` so the last line can sit at the top of the viewport; keyboard scrolling still uses `EditorState::scroll_down` with its cursor-bound clamp.

**Acceptance criteria:** Mouse clicks place the cursor correctly. Text can be selected by dragging. Scrolling works. Checkboxes toggle on click. No crashes or visual glitches on rapid mouse movement.

**Implementation notes (Phase 5):**

- **Event gating by capability**: `Event::Mouse` is only dispatched to `MouseDispatcher` when `capabilities.mouse` is true — terminals without mouse reporting (TERM=dumb, linux framebuffer) never generate mouse events and the dispatcher is never called.  `terminal::enable_mouse()` is the same gated toggle at the escape-sequence level, so we never send `EnableMouseCapture` to a terminal that would echo the bytes literally.
- **Mouse enable runs after capability detection**: `main.rs` calls `terminal::enable_mouse()` only after `Capabilities::detect()` completes — the Picker's image-protocol probe would otherwise compete with mouse reporting for stdin bytes.  `terminal::restore()` calls `disable_mouse()` unconditionally (best-effort) before `PopKeyboardEnhancementFlags` / `LeaveAlternateScreen`, matching th e tear-down order of the push operations in `setup()`.
- **Click-count state machine**: `MouseDispatcher` tracks `last_click_time` + `last_click_cell` and emits `DoubleClick` / `TripleClick` when a second / third `Down` happens at the same cell within `MULTI_CLICK_WINDOW = 400 ms`.  The same-cell check is against the rel-coord of the previous click, so dragging the cursor a column between clicks resets the counter (treating it as a new click) — intentional, otherwise tiny hand tremors would inflate every click to a double-click.
- **Drag anchor lives in `App`, not `EditorState`**: the drag anchor (cursor offset at mouse-down) is held by the app loop and threaded into `mouse_ops::apply` as `&mut Option<usize>` so it can persist across events.  It isn't stored in `EditorState` because a drag is a UI-layer interaction, not a document-layer fact.
- **Selection on drag uses `Selection::anchor` for the captured offset**: mouse drag produces a `Selection { anchor, active }` where `anchor` is the down-offset and `active` is the current cursor offset.  This matches the keyboard selection semantics (Shift+Arrow uses the same struct), so clipboard / cut / copy actions work uniformly regardless of how the selection was made.
- **Word-boundary for double-click matches `move_word_*` predicates**: `select_word_at_cursor` uses `char::is_alphanumeric || '_'` as the word-char predicate, matching the logic that Ctrl+Left / Ctrl+Right already use for keyboard word navigation.  When the cursor sits on punctuation, it expands across the contiguous punctuation run rather than producing an empty selection.
- **Rendered click → buffer offset is approximate, not exact**: rendered inline formatting (`**bold**` → `bold`) shifts char positions between the raw source and the rendered line.  `rendered_sub_line_to_offset` maps the clicked visual column directly to the same column in the raw line, which is exact for blocks with no inline formatting (paragraphs without bold/italic, headings, code blocks, list items' first raw line) and off by a few chars for decorated spans.  Once the cursor lands in the block, the `RAW_REVEAL_DELAY` turns that line raw and the user can refine with a second click.  Tables prepend a top border so the click maps `sub_line_in_block - 1` to the raw row.
- **Scroll wheel uses a separate bound**: `scroll_by_mouse` is not the same as `EditorState::scroll_down` — it uses `max = total - 1` (last line at top of viewport) AND deliberately does NOT call `clamp_cursor_to_viewport_top`.  Keyboard scroll still pulls the cursor along because the cursor is the focus of navigation there; mouse scroll leaves the cursor where the user last placed it.
- **Link hit-test via source scanning**: `link_at_offset` walks the raw source line for `[text](url)` syntax using a balanced-bracket scanner (supports one level of nesting).  This is a best-effort Phase 8 prerequisite — autolinks (`<url>`) and reference links (`[text][id]`) are not detected and will need a proper AST-based hit-test registry when Phase 8 lands.
- **Link clicks are logged, not opened**: Phase 5 surfaces detected URL clicks via `tracing::info!(target: "mouse")` so Phase 8 can replace the log with an OS-open invocation (`open` / `xdg-open` / `start`) without touching the dispatch path.
- **Checkbox click is a distinct mode from click**: `Click` first tries `toggle_checkbox_at`; if the click falls within the 3-char `[ ]`/`[x]` glyph of a task-list item, the checkbox is toggled and the cursor does NOT move.  Clicks elsewhere on the same line fall through to normal cursor placement.  Flash-artifact fix: `toggle_checkbox_at` now saves and restores `cursor_block_idx` / `cursor_line_idx` / `cursor_block_entered_at` across the delta apply so the reveal timer isn't reset — the cursor's block stays in its current rendered/raw state instead of briefly flipping.

#### Issues
- [ ] Selecting from the first (or last) rendered character of an element should select from the first (or last) *raw* character of the element.  Deferred — implementing this cleanly requires the renderer to emit a per-char raw-byte map so edge-of-element detection is possible without reparsing.  Tracked as a Phase 6 (or later) refactor.
  
---

### Phase 6 — Table Row/Column Drag and Column Resize ✅
*Goal: reorder rows and columns by dragging; resize columns by dragging borders.*
*Status: **Complete** — 2026-04-20. 716 tests passing (267 unit + 38 editing + 37 list_edit + 37 table + 24 mouse + 26 renderer + 12 source_map + 18 UI). New module `src/ui/table_view.rs` owns the per-frame `TableLayoutSnapshot` plus the `≡` / `⇔` drag-handle painters. Phase 5's `drag_anchor: Option<usize>` is replaced by a `DragTarget` enum in `src/editor/mouse_ops.rs`; `MouseAction::Click` hit-tests against the current frame's snapshots and dispatches table drags to the new `commit_row_drag` / `commit_column_border_drag` / `commit_column_drag` helpers. Column widths persisted via `<!-- tui-columns: [...] -->` round-trip through a `user_widths: Option<Vec<usize>>` field on `Block::Table`, applied by the renderer via the pre-existing `table_layout::compute_widths` override path.*

**Tasks — layout & hit-test foundation:**
- [x] Extract the four private helpers from `rendered_view.rs`  (`raw_pipe_positions`, `rendered_pipe_positions`,  `table_raw_col_to_rendered_col`, `compute_cell_overlay` / `CellOverlay`)  into `src/markdown/table_layout.rs` as `pub` items. Move their unit-test  coverage with them. `RenderedView` imports them from the new location so  the cell-scoped reveal keeps working unchanged.
- [x] Introduce a `TableView` module at `src/ui/table_view.rs` that owns a  per-frame `TableLayoutSnapshot { col_ranges: Vec<Range<u16>>, row_ranges:  Vec<Range<u16>>, row_handle_col: Option<u16>, header_handle_row:  Option<u16> }`. Exposes  `hit_test(col, row) -> Option<TableHit>` where `TableHit = Cell(r, c) |  ColumnBorder(c) | RowHandle(r) | ColumnHandle(c)`. Scope is explicitly  **layout + hit-test + drag-handle rendering**: the cell-scoped raw reveal  stays in `RenderedView` and imports the shared helpers from `table_layout`  rather than being subsumed by a new widget.
- [x] `renderer::render_table` gains a `user_widths` parameter and delegates  width computation to `table_layout::compute_widths`. `RenderedView` calls  `table_view::build_snapshots` + `table_view::paint_handles` after its  line-render pass so the drag gutter and column-handle row are painted  on top of the existing rendering without requiring a second renderer.
- [x] ~~`insta` snapshot tests for `TableView` rendering~~ — **deferred**: the  Phase 6 design keeps rendering in `renderer::render_table` and paints  handles as a post-pass, so there's no isolated "TableView rendered  output" to snapshot. Coverage is provided by the three new integration  tests in `tests/ui.rs` (`table_view_paints_row_and_column_handles_when_enabled`,  `table_view_snapshots_empty_when_no_table`,  `table_view_persists_user_widths_from_tui_columns_comment`), which  assert actual glyph placement and width persistence through a  `TestBackend`.

**Tasks — drag-target classification (prerequisite for every drag below):**
- [x] Replace `drag_anchor: Option<usize>` on `App` with a `DragTarget` enum  living in `src/editor/mouse_ops.rs`:
  ```rust
  pub enum DragTarget {
      TextSelection { anchor: usize },
      TableRow { table_byte_start, row_idx, hover_row_idx },
      TableColumnBorder { table_byte_start, col_idx, start_widths, anchor_x },
      TableColumnHeader { table_byte_start, col_idx, hover_col_idx },
  }
  ```
  On `MouseAction::Click`, `mouse_ops::apply` hit-tests against the  snapshots stored on `RenderedViewState` (threaded through `App`) and  sets the target; subsequent `Drag` events dispatch on the variant  instead of always extending a text selection. `Release` commits via  `commit_row_drag` / `commit_column_border_drag` / `commit_column_drag`.  The text-selection drag remains the fallthrough variant.

**Tasks — column-width persistence (load + save paths split):**
- [x] **Load path**: `parse_raw` emits tables with `user_widths: None`; a  post-pass (`merge_trailing_tui_columns_comments` in `document/parsed_doc.rs`)  detects a trailing `<!-- tui-columns: [..] -->` HTML comment adjacent to  a `Block::Table`, moves its widths onto the table's new  `user_widths: Option<Vec<usize>>` field, and drops the comment block.  `renderer::render_table` threads `user_widths` into  `table_layout::compute_widths`.
- [x] **Save path**: `table_edit::write_column_widths(source, &info, &widths)`  produces a single `EditDelta` that inserts or replaces the  `<!-- tui-columns: [...] -->` comment row immediately after the table.  `mouse_ops::commit_column_border_drag` calls this on release so the  resize and the comment update sit in one undo step.

**Tasks — row reordering:**
- [x] Render a row-drag handle (`≡`) via `table_view::paint_handles` in an  **external gutter** one cell wide to the left of the table's outer `│`,  for every data row (not header, not alignment). Gating: only painted  when `RenderedView::show_table_handles` is true, which in production  resolves to `config.table.show_drag_handles && capabilities.mouse`.
- [x] Hit-test on the row-handle column produces `TableHit::RowHandle(r)`,  which sets `DragTarget::TableRow` on mouse-down. Drag updates  `hover_row_idx` as the pointer moves across data rows; `Release`  commits via `table_edit::swap_rows` applied repeatedly between source  and destination (Phase 2 only supports adjacent swaps, so each  intermediate step lands as its own `EditDelta`). ~~Horizontal-separator  drop-indicator highlighting~~ — **deferred** as polish; the swap still  commits correctly on release.

**Tasks — column border resize:**
- [x] Hit-test `±1` around each rendered `│` border produces  `TableHit::ColumnBorder(c)`. Mouse-down records `start_widths` (the  current `compute_widths` result, reconstructed from `TableInfo` cell  text) and `anchor_x` so drag deltas are additive rather than cumulative  across frames.
- [x] On `Drag`: `resize_widths` adjusts `user_widths[c-1]` and  `user_widths[c]` by the delta, preserving total width and clamping each  to `MIN_COL_WIDTH`.  The new widths are stored on  `EditorState::live_table_widths`, which `ParsedDoc::build_with_overrides`  splices onto the matching `Block::Table` so the next render picks them  up immediately. **No buffer mutation during drag.**
- [x] On `Release`: commit via `table_edit::write_column_widths` so the  `tui-columns` comment is inserted or updated in a single `EditDelta`;  `live_table_widths` is cleared.

**Tasks — column reordering:**
- [x] `table_view::paint_handles` draws a `⇔` glyph centred over each column  one screen row above the top border. This does not affect raw Markdown  alignment because the glyphs are painted post-line-render; the source  still ends at the trailing `\n` of the last row.
- [x] Hit-test on the column-handle row produces `TableHit::ColumnHandle(c)`,  which sets `DragTarget::TableColumnHeader` on mouse-down. `Drag` tracks  `hover_col_idx`; `Release` commits via repeated `table_edit::swap_columns`  between source and destination. ~~Drop-indicator highlighting on the  vertical `│` border~~ — **deferred** as polish.

**Tasks — configuration & degradation:**
- [x] `[table] show_drag_handles: bool` added to `Config` with a default of  `true`. `App::new` overrides to `false` when `capabilities.mouse` is  false, so terminals without mouse reporting never paint inert gutter  glyphs.

**Tasks — testing:**
- [x] Unit tests for the extracted pipe-position / cell-range helpers now  living in `table_layout` (pure functions) — `raw_pipe_positions_basic`,  `raw_pipe_positions_skips_escaped_pipes`,  `rendered_pipe_positions_counts_box_drawing_pipes`,  `table_raw_col_to_rendered_col_maps_first_cell`,  `table_raw_col_to_rendered_col_returns_none_on_pipe_mismatch`,  `compute_cell_overlay_none_when_raw_exceeds_rendered_width`,  `compute_cell_overlay_returns_metadata_when_fits`.
- [x] Unit tests for `TableLayoutSnapshot::hit_test` in `src/ui/table_view.rs`  — cell hit, border hit (on-pipe + ±1 tolerance), row handle hit, column  handle hit, out-of-region None, and `table_sub_to_row_idx` layout.
- [x] `mouse_ops` integration tests in `tests/mouse.rs` for each drag flow:  `row_handle_drag_swaps_rows_in_buffer`,  `column_border_drag_writes_tui_columns_comment`,  `column_handle_drag_swaps_columns_in_buffer`. Each drives a full  Click → Drag → Release sequence through `mouse_ops::apply` with  fabricated `TableLayoutSnapshot`s and asserts the buffer content  afterwards.
- [x] Manual smoke test — documented here: in a mouse-reporting terminal,  open a file with a GFM table, verify the `≡` gutter glyphs appear to  the left of each data row and `⇔` glyphs appear above each column,  then verify drag-and-drop on each works and produces valid Markdown.  (Not automated per CLAUDE.md.)

**Tasks — invariants:**
- [x] All drag operations are undoable via `Undo`. Row swap drags across  non-adjacent rows land as N adjacent-swap `EditDelta`s (one per step),  so `Undo` reverts one step at a time — not a single step. This is a  known deviation from the spec's stricter "single-step undo" goal;  coalescing the intermediate deltas is a follow-up polish (requires  either augmenting `History` with drag groups or adding a  non-adjacent `table_edit::swap_rows_range` primitive).
- [x] Minimum column width: `MIN_COL_WIDTH = 3` reused from `table_layout`  via `resize_widths`.

**Acceptance criteria:** Rows can be dragged to new positions. Column borders can be dragged to resize. Columns can be reordered by dragging their headers. User-set column widths round-trip through the `tui-columns` comment. The underlying Markdown is correctly updated after each operation and is undoable (row/column swaps in adjacent-pair increments rather than single atomic deltas — see invariants). Drag handles are hidden on terminals without mouse reporting.

**Implementation notes (Phase 6):**
- **`TableView` is a partial widget**: Phase 6 keeps the rendered table lines  flowing through `ParsedDoc::lines` so scroll, wrap, and cell-scoped raw  reveal keep working unchanged. The new `src/ui/table_view.rs` module owns  only three pieces: `TableLayoutSnapshot` (per-frame geometry),  `paint_handles` (post-line-render glyph painter), and `build_snapshots`  (walks `RenderedView`'s visible line range, fabricating snapshots for each  visible table). There's intentionally no `StatefulWidget for TableView` —  tables render through `renderer::render_table` as before.
- **Snapshot lifetime**: `RenderedView::render` calls `build_snapshots` at the  end of its loop and stashes the result on `RenderedViewState::table_snapshots`.  The next mouse event reads them from there via `App::run`'s dispatch path.  Snapshots are thus one frame stale — acceptable because the buffer state  from the frame the user saw is what they intend to click on.
- **Live column-width preview**: `EditorState::live_table_widths:  Option<(usize, Vec<usize>)>` holds `(table_byte_start, widths)` during a  resize drag. `ParsedDoc::build_with_overrides` accepts this and splices it  onto the matching `Block::Table` before rendering. Each drag event calls  `refresh_parsed`, so the user sees the new widths without the buffer ever  being mutated until release.
- **Attach-trailing-comment pass runs in two places**: the standalone  `parser::parse` calls `attach_trailing_tui_columns_comments` directly, but  `ParsedDoc::build_with_overrides` uses the lower-level `parse_raw` (blocks  in 1:1 correspondence with `real_ranges`), applies the live-widths override,  then runs `merge_trailing_tui_columns_comments` which also shrinks the  `real_ranges` vector to match. Without that range-aware merge, the  per-block rendered-line count assignment downstream would go out of sync  whenever a persisted `tui-columns` comment was present.
- **Snapshot does not close on separator rows**: the initial  `build_snapshots` implementation closed the open snapshot whenever a  rendered row mapped to `None` (top border, thin separators, bottom  border), which produced one snapshot per data row. The fix is to close  the snapshot only when we leave the table's source block entirely —  tracked via a separate `open_table_block` variable.
- **Multi-step swaps, not single deltas**: the plan asked for row/column  drags to be single-step undo, but Phase 2's `swap_rows` / `swap_columns`  only support adjacent pairs. `commit_row_drag` / `commit_column_drag`  therefore emit N adjacent `EditDelta`s for a drag that spans N rows /  columns, and `Undo` reverts one at a time. Coalescing them into one  delta requires either extending the primitives to support arbitrary  index pairs or teaching `History` to group a sequence of deltas — both  are follow-up polish items rather than Phase 6 scope.
- **Column widths persistence load path passes through parser_raw**: because  `parse_offsets::top_level_block_ranges` emits a range for the comment  block that hasn't been removed yet, `ParsedDoc::build_with_overrides`  uses `parse_raw` (no attach pass) so blocks and ranges stay aligned 1:1.  The merge pass then removes both the block and the range in lockstep.

---

### Phase 7 — Image Display ✅
*Goal: render inline images using the best available terminal graphics protocol.*
*Status: **Complete** — 2026-04-20.  791 tests passing (294 unit/lib × 2 + 44 editing + 37 list_edit + 28 mouse + 26 renderer + 12 source_map + 37 table + 19 ui).  New modules: `src/image/loader.rs` (URL-to-DynamicImage resolution with `http`/`https` via `ureq`, `file://` and bare paths, remote-policy gating), `src/image/cache.rs` (URL-keyed `ImageCache` with per-(url, w, h) `StatefulProtocol` reuse — retained across reparses on `EditorState`, NOT on `ParsedDoc`, so ordinary keystrokes don't invalidate the expensive encoding), `src/ui/image_view.rs` (per-frame `ImageLayoutSnapshot` + `paint_images` post-render overlay).  `Block::ImageBlock { alt, url }` AST variant; `promote_image_paragraphs` post-parse pass promotes single-image paragraphs; renderer emits `[Image: alt]` on row 0 with NBSP padding up to `image_max_height`.  `Capabilities` retains the `Picker` from Phase 4's probe (was dropped before).  `AppEvent::ImageReady` variant carries worker-thread decode results; `App::dispatch_image_decodes` scans `editor.parsed.image_blocks` on load and after every edit, spawning one thread per newly-requested URL.  Remote-image prompt via the existing `ModalView` with three buttons (`Always` / `Never` / `This time only`); persists policy to `config.toml` or sets session-only flag.  Images render in Preview + Rendered modes; Raw mode shows plain Markdown source unchanged.  The cursor's image block is suppressed from the overlay during `RAW_REVEAL_DELAY` so the user sees raw `![alt](url)` when editing.*

**Starting state (already in place):** `Inline::Image { alt, url }` is parsedat `src/markdown/parser.rs:355` and rendered as a styled `[Image: alt]`placeholder at `src/markdown/renderer.rs:597` using `Theme::image_placeholder`.RawMode reads the rope buffer directly, so raw `![alt](url)` display is alreadyfree — no renderer change needed for RawMode. `ratatui-image` 9 is in`Cargo.toml` with `crossterm` + `image-defaults` features. The generic`ui::modal::ModalView` (`src/ui/modal.rs`) supports arbitrary button countsand is already used for Phase 4's startup notice at `src/app.rs:365–383`.`Config::save()` exists (Phase 4) and writes to `~/.config/edamame/config.toml`.

**Tasks — dependencies & config:**
- [x] Add `ureq` to `Cargo.toml` with rustls features to avoid an OpenSSL  system dep: `ureq = { version = "2", default-features = false, features = ["tls"] }`.
- [x] New `ImageConfig` section in `src/config/config.rs`:
  ```rust
  pub struct ImageConfig {
      pub enabled: bool,        // master switch; defaults to true
      pub max_width: usize,     // cells; defaults sensibly (e.g. 80)
      pub max_height: usize,    // cells; defaults sensibly (e.g. 24)
      pub remote_policy: RemoteImagePolicy, // Ask | Always | Never
  }
  ```
  Add `pub image: ImageConfig` to `Config`. Extend `config/config.toml`  with the new section and annotations.

**Tasks — Picker ownership (see Phase 4 implementation notes):**
- [x] Change `Capabilities` to hold `Option<Picker>` alongside `image_protocol`.  Initialise both in `detect_image_protocol` instead of dropping the Picker.  Thread the capability reference down to image-loading code.

**Tasks — AST normalisation:**
- [x] Add a post-parse pass (in `parser.rs` or a small normalisation step  called from `ParsedDoc::build`) that promotes any paragraph whose single  inline is `Inline::Image` to a new `Block::ImageBlock { alt, url }`  variant. Inline images inside mixed-content paragraphs stay as  `Inline::Image` placeholders — terminal graphics cannot sit mid-line in  a wrapped paragraph without breaking layout.
- [x] `SourceMap` / `ParsedDoc::per_block_own` must count the reserved image  rows so that `move_up_visual` / `move_down_visual` traverse image blocks  the same way they traverse multi-line blocks today, preserving Phase 1's  virtual-block and raw-reveal invariants.

**Tasks — image loader:**
- [x] New module `src/image/loader.rs` (and facade `src/image.rs`) that  resolves an image URL to decoded bytes. Local files resolve relative to  the document path and are read on demand (the OS page cache handles  repeated reads — no extra caching layer needed). `http`/`https` URLs  are fetched once per session via `ureq`; the decoded bytes are held in  an in-process `HashMap<String, Result<DynamicImage, _>>` for the life  of the `App` and dropped on shutdown. **No on-disk cache** — if the  user reopens the document later, remote images are refetched.
- [x] Retention of encoded protocol state (the expensive part for  Sixel/Kitty) lives on `ParsedDoc`: a per-image `Option<StatefulProtocol>`  built lazily the first time the block becomes visible and kept alive as  long as the `Block::ImageBlock` exists in the current parse. When the  document is reparsed, stale protocol state is dropped alongside the old  AST. This removes the need for a separate dimension-keyed cache — the  protocol object IS the cache.
- [x] Loading is lazy and viewport-gated: neither the raw bytes nor the  protocol state are built for `Block::ImageBlock`s whose reserved rows  don't intersect the current visible window.
- [x] On load failure (IO, decode, blocked remote), emit the existing  `[Image: alt]` styled placeholder via `Theme::image_placeholder`.

**Tasks — rendering:**
- [x] `renderer::render_image_block` emits `N` blank lines filled with NBSP  padding (to preserve the Phase 1 background-fill path in  `ui::line_render`) where `N = min(config.image.max_height,  computed_rows_for_this_image)`.
- [x] `RenderedView::render` (and `PreviewView::render`) accumulate a  per-frame `Vec<ImageDraw { rect, block_idx }>` side-channel as the  line-based draw proceeds. After the line pass, each image is overlaid  onto its reserved rect using `ratatui_image::StatefulImage` driven by  the `StatefulProtocol` retained on `ParsedDoc` (see  `ratatui-image-9.0.0/src/lib.rs:176` and `:240`). Because protocol  state is retained across frames, routine redraws do not trigger  re-encoding.
- [x] Image size is clamped to cell dimensions via  `ratatui_image`'s `Resize`/`ResizeEncodeRender` path  (`ratatui-image-9.0.0/src/lib.rs:196`, `:288`), honouring  `config.image.max_width` / `max_height`.

**Tasks — remote-load prompt (App layer, not renderer):**
- [x] On document load, scan parsed image nodes for `http`/`https` URLs.  If any exist and `config.image.remote_policy == Ask`, show a three-button  modal via the existing startup-notice flow (`src/app.rs:365–383`) with  buttons `Always`, `Never`, `This time only`.
- [x] `Always` / `Never` persist to `config.image.remote_policy` via  `Config::save()`. `This time only` sets an in-process flag on `App`.  The image loader consults this policy/flag before issuing `ureq`  requests; the renderer never performs IO or awaits input.

**Tasks — testing:**
- [x] Unit tests for the ast-normalisation pass: paragraph with a single  image promotes to `Block::ImageBlock`; mixed paragraphs keep inline  images; multiple stacked image paragraphs each become their own block.
- [x] Unit tests for the loader's remote-policy gating: `Never` short-circuits  before `ureq`; `Always` / `This time only` proceed; failures fall back to  the placeholder.
- [x] Integration tests for `per_block_own` updates: vertical navigation  skips image rows the same way it skips any multi-line block.
- [x] Integration test for protocol-state retention across reparses: a  `Block::ImageBlock` whose `url` is unchanged after an edit elsewhere in  the document keeps its existing `StatefulProtocol` rather than  rebuilding (cheap to assert via a build-counter on a test stub).
- [x] Manual smoke test (documented, not automated) in a mouse- and  graphics-capable terminal (Ghostty, Kitty, WezTerm). Terminal-graphics  wire protocol testing is excluded per CLAUDE.md.

**Acceptance criteria:** A `.md` file containing `![alt](./image.png)` displaysthe image inline in a terminal supporting Sixel/Kitty/iTerm2; `![alt](https://…)`prompts on first load per `remote_policy` and fetches once per session (noon-disk cache). Scrolling an image out of view and back in is cheap — nore-decode or re-encode. Terminals without graphics support render`[Image: alt]` in the existing placeholder style. Large images are scaled to`max_width` × `max_height`. RawMode is unchanged. Cursor navigation overimage blocks is consistent with other multi-line blocks and Phase 1'sraw-reveal behaviour still holds.

**Phase ordering note:** Phase 7 does not depend on Phase 6 (tables) and canland first. If Phase 6's `TableView` extraction lands first, Phase 7 shouldfollow the same per-block widget pattern and place the image overlay logic in a new `src/ui/image_view.rs` rather than stitching draws into`RenderedView` directly.

**Implementation notes (Phase 7):**

- **Decoded-image cache lives on `EditorState`, not `ParsedDoc`** (deviation from the original spec at `plan.md:740`).  `ParsedDoc` is rebuilt on every buffer mutation, so attaching `StatefulProtocol` retention to the parse tree would mean re-encoding every image on every keystroke.  The cache is instead a URL-keyed `ImageCache` on `EditorState` (`src/editor/state.rs`); it survives reparses because edits almost never change the image URL set.  Protocols are additionally keyed by `(url, width, height)` so terminal resizes invalidate only affected entries.  See `src/image/cache.rs`.
- **Fixed-height row reservation**: the renderer reserves exactly `image_max_height` rows per `Block::ImageBlock`, regardless of the image's actual aspect ratio.  This keeps `per_block_own` stable across the decode lifecycle (cursor navigation doesn't depend on a decode completing).  Short images leave bottom padding.  Aspect-aware reservation is a noted follow-up.
- **Image rendering happens in `EditorView`, not the per-mode widgets**: `RenderedView::render` and `PreviewView::render` populate `image_snapshots` on their respective states during the line-render pass, but the actual `paint_images` call is in `EditorView::render` after the sub-widget returns.  This is the only way to satisfy the borrow checker: `paint_images` needs `&mut EditorState::images` while the sub-widgets hold `&EditorState`.  `EditorView` therefore takes `state: &'a mut EditorState` (changed from `&'a EditorState`).
- **Image overlay suppression on raw-reveal**: `EditorView` passes `state.cursor_block_idx` as `suppress_block_idx` to `paint_images` when `cursor_block_revealed()` returns true, so the user sees the raw `![alt](url)` text instead of the image when editing the source line.  Outside the reveal delay, the image paints over the placeholder normally.
- **Decode worker threads over async**: each newly-requested URL spawns a std::thread that calls `loader::resolve` (blocking IO) and reports completion via `AppEvent::ImageReady`.  No async runtime, no tokio.  The thread count per session is bounded by the number of distinct image URLs in the document; typical docs have ≤ 10.
- **Remote-load prompt stacks behind the startup notice**: both modals are rendered via the same `ModalView` widget, but the remote prompt only shows after the capability notice is dismissed.  Escape on the prompt is a dismissal, not `This time only` — the user has to explicitly choose a button to enable remote loads.
- **Module name collision with the `image` crate**: `src/image/` is our module, but `image` is also a Cargo dependency (transitively from `ratatui-image`; made explicit in `Cargo.toml` so we can use `image::load_from_memory` and `image::DynamicImage` in our code).  The Rust 2018+ path-resolution rules treat `use image::…` in `src/image/loader.rs` as the external crate, not the current module.  No renaming needed; confirmed by the passing loader tests.

---

### Phase 8 — Clickable Links and File Navigation ✅
*Goal: follow links on click; open other Markdown files in the editor.*

**Starting state (already in place after Phases 4–7):**
- `Inline::Link { text, url, title }` is parsed at `src/markdown/parser.rs` and rendered by `src/markdown/renderer.rs` as an UNDERLINED + `Theme::link_text` span.  The `title` field is parsed but currently unused — Phase 8 can surface it on hover.
- `mouse_ops::link_at_offset` (`src/editor/mouse_ops.rs:1401`) scans the raw line for balanced `[text](url)` and returns the URL.  Today Phase 5 only logs the detected URL via `tracing::info!(target: "mouse")` — Phase 8 replaces the log with action dispatch without touching the mouse-dispatch plumbing.
- `mouse_ops::hit_test_clickable` + `App::update_pointer_shape` already switch the terminal pointer to `PointerShape::Hand` when the cursor hovers a link span (detected via the renderer's `UNDERLINED` modifier) or a task-list checkbox.  Phase 8 does **not** need to add hover detection — only to extend the hover channel so hovered-link *target* metadata is also emitted for the hint-line tooltip (see below).
- `ui::ModalView` (`src/ui/modal.rs`) hosts Phase 4's startup notice and Phase 7's remote-image prompt with arbitrary button counts.  Phase 8 reuses it for the unsaved-changes guard on forward navigation.
- `AppEvent` (`src/app.rs:23`) already supports worker-thread notifications (`ImageReady`).  Phase 8 adds `LinkOpenResult` so `open::that` can run off the main thread without blocking the UI on a slow `xdg-open` / `start` invocation.
- `Action` enum (`src/config/keymap.rs:15`) is extended phase-by-phase in practice — Phase 8 adds `FollowLinkUnderCursor` / `NavigateBack` / `NavigateForward`.  The CLAUDE.md claim that every action lives in Phase 0 is aspirational; follow the existing convention of adding them here.
- The document's base directory for resolving relative paths is `App::file_path.parent()` — the same convention used by `src/image/loader.rs`.  Reuse, don't reinvent.

**Tasks — dependencies:**
- [x] Add `open = "5"` to `Cargo.toml`.  No system-library dependency; the crate shells out to `xdg-open` / `open` / `start` per platform.

**Tasks — AST-backed link hit-test (upgrade from Phase 5's source scan):**
- [x] New module `src/ui/link_view.rs` modelled on `ui::image_view` and `ui::table_view`.  Owns `LinkLayoutSnapshot { rect: Rect, target: LinkTarget, url, title }` and a `build_snapshots(state, area, scroll)` entry point that walks the visible rendered-line range, consulting the AST rather than re-scanning the raw line.  Covers three AST-level targets:
      - `Inline::Link { url, .. }` — common case, reachable from paragraph, heading, list-item, table-cell, and block-quote inlines.
      - GFM autolinks / reference-style links: `pulldown-cmark` normalises both to `Tag::Link`, so the AST-walk branch above handles them transparently.
- [x] `LinkTarget` enum in `src/editor/link.rs` (new module):
      ```rust
      pub enum LinkTarget {
          Url(String),            // http, https, mailto, …
          LocalFile(PathBuf),     // relative or absolute, any extension
          Anchor(String),         // `#slug` within the current document
      }
      ```
      Classification happens via `LinkTarget::parse(url: &str, base_dir: Option<&Path>) -> LinkTarget`: `#foo` → `Anchor`; RFC-3986 scheme (or `mailto:`) → `Url`; everything else → `LocalFile`, resolved relative to the document dir.  Single-char "schemes" (Windows drive letters) are explicitly not treated as URLs.
- [x] `RenderedViewState::link_snapshots: Vec<LinkLayoutSnapshot>` and `PreviewState::link_snapshots: Vec<LinkLayoutSnapshot>`, populated at the end of each `render()` pass.  Matches the pattern from Phase 6's `table_snapshots` and Phase 7's `image_snapshots`.
- [x] **Raw-reveal fallback**: `mouse_ops::follow_link_at_click` first consults underlined spans on the rendered line; when no AST span is present it falls back to `link_at_offset` on the raw source so clicks on raw `[text](url)` syntax still resolve.  Both paths produce a `LinkTarget` via `LinkTarget::parse` so downstream dispatch is uniform.

**Tasks — click / keyboard dispatch:**
- [x] Extended `MouseAction::Click` / `DoubleClick` / `TripleClick` to carry `KeyModifiers` from the crossterm `MouseEvent`.  `MouseDispatcher::dispatch` threads the modifier bits through.
- [x] `mouse_ops::apply` click handling, per mode:
      - **Preview**: any click on a link fires `FollowLink` (the document is read-only).
      - **Rendered**: plain click places the cursor (unchanged); `Ctrl`-click on a link fires `FollowLink` without moving the cursor.
      - **Raw**: same as Rendered — the raw-reveal fallback handles the revealed-block case.
- [x] Added `Action::FollowLinkUnderCursor`, `Action::NavigateBack`, `Action::NavigateForward` to `config/keymap.rs`.  Default bindings:
      - `FollowLinkUnderCursor`: `Ctrl-Enter`.
      - `NavigateBack` / `NavigateForward`: no explicit default binding; instead the App redirects `Alt+Left` / `Alt+Right` (bound to `TableMoveColumnLeft` / `Right`) to navigation when the cursor is outside any table.  This keeps the table-reorder shortcuts intact while giving navigation a keyboard path, and users can still rebind navigation explicitly.
- [x] `FollowLinkUnderCursor` resolves the link at the cursor's rope offset via the same raw-source scan (`link_at_offset`) used by the mouse fallback.

**Tasks — `FollowLink` dispatch by target:**
- [x] `LinkTarget::Url` — spawns a worker thread that calls `open::that(&url)` and reports completion via `AppEvent::LinkOpenResult(Result<(), String>)`.  Failures are currently logged via `tracing::warn!`; Phase 9 will surface them on the hint line.
- [x] `LinkTarget::LocalFile` with `.md`/`.markdown` extension (case-insensitive) → push the current `NavEntry` onto `App::nav_back`, load the new file into the editor, rebuild the image cache / view state against the new base dir.
- [x] `LinkTarget::LocalFile` with any other extension → `open::that(&path)` on the worker thread.
- [x] `LinkTarget::Anchor(slug)` → resolves via `ParsedDoc::heading_anchors: HashMap<String, usize>`, built during `ParsedDoc::build` from each `Block::Heading`'s plain-text `inlines_to_plain`.  Slug algorithm matches GFM: lowercase, strip characters not in `[a-z0-9 -]`, replace runs of whitespace with `-`, uniquify with `-N` suffix on collision.  On miss the anchor navigation is a no-op.

**Tasks — navigation stack:**
- [x] `App::nav_back: Vec<NavEntry>` and `App::nav_forward: Vec<NavEntry>` where `NavEntry = { path, scroll, cursor_offset, mode }`.  `NavigateBack` pops `nav_back` and pushes the current state onto `nav_forward`; `NavigateForward` is the inverse.  Following a new link clears `nav_forward` (browser semantics).
- [x] **Dirty-buffer guard**: `App::dirty_guard: Option<DirtyGuardPrompt>` drives a three-button `ModalView` (`Save` / `Discard` / `Cancel`) rendered above the editor when follow-link would navigate away from a dirty buffer.  Reused for back / forward navigation too.
- Phase 10 note: this navigation stack stays per-tab-history so Phase 10 can lift it into a `Vec<Tab>` without re-architecting.

**Tasks — hover target display:**
- [x] Added `mouse_ops::hovered_link_target(state, col, row, width) -> Option<LinkTarget>` and `App::hovered_link: Option<LinkTarget>`, updated on every mouse-move event.  Pointer shape keeps using the faster `hit_test_clickable` bool path.  Phase 9 will surface the target + `Inline::Link::title` on the hint line.

**Tasks — testing:**
- [x] Unit tests for `LinkTarget::parse` covering `#heading`, `https://`, `mailto:`, `file://`, relative / absolute paths, and Windows drive letters (see `src/editor/link.rs::tests`).
- [x] Unit tests for `gfm_slug` and `uniquify_slug` covering basic cases, collisions, Unicode stripping, and underscore / hyphen preservation.
- [x] Unit tests for `ParsedDoc::heading_anchors` — one entry per heading, correct rendered line index, stable across reparses, collisions uniquify.
- [x] Integration tests in `tests/mouse.rs`: Preview click on a link sets `pending_link_follow`; plain click in Rendered places the cursor; Ctrl-click in Rendered sets `pending_link_follow` without moving the cursor; raw-reveal fallback handles clicks on raw bracket syntax.
- Nav-stack and dirty-guard integration tests are deferred: exercising them end-to-end requires driving the App loop (terminal setup), which this repo doesn't yet harness for tests.  Mouse-layer coverage ensures the dispatch edge is correct; the App-layer plumbing is deliberately thin (push / pop / load).

**Tasks — deferred to later phases:**
- [ ] Hint-line tooltip with link target + title (Phase 9 — hint line      ownership).
- [ ] Tab-bar integration of the nav stack (Phase 10 — tab bar ownership).

**Acceptance criteria:** Clicking a URL link in Preview opens the browser. `Ctrl`-click on a link in Rendered/Raw mode opens it without moving the cursor. `Ctrl-Enter` on a link in rendered/raw mode follows it. Clicking a relative `.md`path navigates to that file in the same editor window, reusing the imagecache's base-dir-resolution convention. `Alt+Left` / `Alt+Right` walk the navigation history. Heading anchors (`#slug`) scroll to the matching heading. Dirty buffers prompt before being replaced. The pointer shape already changes on hover (Phase 5); the hint-line tooltip is explicitly deferred to Phase 9.

---

### Phase 9 — Bottom Status Region ✅
*Goal: a coherent two-line chrome at the bottom of the editor that carries persistent state, contextual hints, transient notifications, and modal prompts.*

This phase deliberately owns the **entire** bottom region so later phases (file-change diff, command palette, tabs, etc.) can consume a single, stable surface rather than reinventing it. Previously this work was split across old Phases 9/10/11; consolidating it here removes the circular dependency where Phase 10's reload prompt needed infrastructure that old Phase 11 was going to provide.

**Layout — two-line status region by default**, stacked immediately below the editor content:

- **Hint line** (upper, adjacent to content) — contextual keybind hints, transient status messages, and modal prompts.  Dynamic content sits closest to the cursor because it describes *"what can I do here"*; eye travel is minimal and transient messages appear right next to the action that triggered them.
- **Status line** (lower, at the bottom edge) — persistent state: mode indicator, file path, dirty marker, cursor `line:col`, selection size.  Stable reference data, matching the nano / mc / htop convention.

**Rationale for two lines (vs. one).** The common failure mode for a TUI is horizontal, not vertical: users routinely run edamame in a tmux pane or tiling-WM split that's 50–80 cols wide but still has full vertical height. A single-line status forces aggressive truncation of either info or hints exactly when both matter most. Two lines gives both regions room and Phase 11's file-change prompts a natural home.

**Hint-line states** (mutually exclusive):
- *Default* — contextual keybind hints (see task list below).
- *Transient message* — a status notification overlays for ~1.5s, then reverts to hints.  Errors stick until dismissed.
- *Modal prompt* — a prompt (Phase 11 reload / save-copy filename, future search, etc.) replaces the hints until dismissed.

**Keybind notation convention.** Plain letter-plus-label everywhere a key is surfaced to the user — hint line *and* prompt overlays.  Examples: `^C Copy`, `^X Cut` for Ctrl-chords; `R Reload`, `I Ignore` for bare keys.  No bracket notation (`[R]eload`); downstream phases (11, 10) honour this convention.

**Input during a transient message.** Input is never blocked.  If `Copied` is on-screen and the user hits `^X`, the cut fires normally and the next message / hint revert proceeds.

**Starting state (already in place):**
- `StatusBar` widget exists at `src/ui/status_bar.rs` and already shows mode badge, filename, modified flag, cursor line:col, line count, and scroll %.  Phase 9 extends it but does not replace it.
- `ModalView` at `src/ui/modal.rs` hosts arbitrary-button modals (used by Phase 4 capability notice and Phase 7 remote-image prompt).  Phase 9 reuses this for the quit-confirm dialog.
- `Theme` already has slots for status styling (`status_bar`, `status_mode`, `status_filename`, `status_info`).  New slots are added for the hint line (see testing tasks).

**Tasks — layout & integration:**
- [x] Introduce a `BottomRegion` composite widget (`src/ui/bottom_region.rs`) that owns layout of the hint line + status line.  It is rendered by `EditorView` in place of the current single-row `StatusBar`.  Single-line compact mode collapses the composite to just the status line.
- [x] `EditorView::render` allocates 2 rows for the bottom region (1 in compact mode) and subtracts from the editor content area.  Existing `PreviewView` / `RenderedView` / `RawView` continue to receive the same inner rect they do today.

**Tasks — persistent status line:**
- [x] Expand `StatusBarState` to carry `selection_size: Option<(usize, usize)>` (char count + line count) populated from `EditorState::selection_size()`; render as ` Sel 42 ch · 3 ln ` when present.
- [x] Detected image protocol is intentionally *not* surfaced here — users who want to see it reach it via the settings overlay (Phase 10) or an `:info` command.

**Tasks — contextual hint line:**
- [x] New `HintLine` sub-widget inside `BottomRegion`.  Default content adapts to editor mode and cursor context via a `hint_line_for(state: &EditorState) -> Vec<HintChord>` pure function.
- [x] *Preview* mode: `any key → edit   ^C Copy   ^P Menu   ^Q Quit`.
- [x] *Rendered / Raw* mode: `^C Copy  ^X Cut  ^V Paste  ^S Save  ^P Menu  ^Q Quit`.
- [x] *Rendered* mode, cursor inside a table: replace the hint line with table manipulation keybinds (Alt+Arrow variants, Tab/Shift-Tab, Alt+Backspace).  A `?` popover exposes the full list when abbreviation is unavoidable.
- [x] *Rendered / Raw* mode, cursor inside a list item: surface `^Space Toggle` (task checkbox) in the hint line.
- [x] Contrasting background for all hint chords, like nano: routed through a new `Theme::hint_chord` style slot (plus `hint_bar`, `hint_label`) so themes can override.
- [x] Width-responsive abbreviation: `lay_out_chords` drops labels left-to-right, then collapses to bare chord glyphs when even those overflow.

**Tasks — transient messages (a single channel all phases consume):**
- [x] Added `App::transient: Option<TransientMessage { text, kind, until }>` with `kind ∈ { Info, Success, Warning, Error }`.  Messages overlay the hint line until `until`; errors stick until dismissed with Escape.  `App::next_deadline` wakes the event loop for the auto-expire.
- [x] Exposed a single helper `App::flash(text, kind)`.  Wired for:
      - `Saved` (Success) / `Save failed` (Error) on `Action::Save`.
      - `Copied` (Info) on both `Copy` and `Cut`.
      - `Configuration updated` (Warning) inside `App::save_config_with_flash`, called from capability-notice "Don't show this again" and remote-image Never / Always.
      - `Link open failed` (Error) on `AppEvent::LinkOpenResult` failures.
      - Autosaved dirty→clean is scaffolded; the autosave path itself lands in a later phase.
- [x] Errors are sticky (`until: None`) until Escape or replaced by a new Error.

**Tasks — modal-prompt channel on the hint line:**
- [x] Defined a `HintPrompt { prompt, chords, handler: fn(&mut App, KeyCode) }` pattern owned by `App` (`App::hint_prompt`).  Only one prompt is active at a time; `HintContent::Prompt` preempts both transient messages and chords.  Escape dismisses via the main-loop dispatch.
- [ ] Phase 11 (file-change detection) will populate the prompt definitions.  The infrastructure is landed; consumer wiring is deferred to Phase 11 proper.

**Tasks — quit-without-saving modal:**
- [x] Phase 1's direct-terminate path in `Action::Quit` now intercepts dirty buffers and opens `QuitConfirm` (3-button ModalView: `Save / Discard / Cancel`).  Clean buffers fall through to the original `edit_ops::apply` return.
- [x] `Save` persists via `Buffer::save_file`; failure raises a sticky Error transient and aborts the quit.
- [x] `Discard` sets `should_quit`.
- [x] `Cancel` and Escape dismiss the modal without quitting.

**Tasks — compact mode:**
- [x] Added `EditorConfig::status_bar: StatusBarLayout` (`two_line` | `compact`; default `two_line`) plus `transient_ms: u64` (default 1500).  Compact mode renders only the persistent status line via `BottomRegion::height`.
- [x] Cheat-sheet popover (`src/ui/cheat_sheet.rs::build_cheat_sheet_body`) produced from the current `KeyMap::first_key_for` so overrides show their bound keys.  Displayed via `ModalView` and dispatched by `App::open_cheat_sheet`.  `Action::ShowCheatSheet` is intentionally unbound — the sheet is reached only via the command palette (Phase 10), avoiding a dedicated key that would conflict with typed text.

**Tasks — testing:**
- [x] Unit tests for `hint_line_for` cover Preview / Rendered / list-item / table contexts (`src/ui/bottom_region.rs::tests`).
- [x] Unit tests for `App::flash` expiry, sticky-error semantics, and `flash_for_action` Save/Copy/Cut/Paste dispatch (`src/app.rs::phase9_flash_tests`).
- [x] `TestBackend` snapshots of `BottomRegion` cover default chords, transient overlay, prompt, compact mode, and selection-size rendering.
- [x] App-level tests cover quit-confirm mechanics (open / cancel / discard) and cheat-sheet content.

**Acceptance criteria:** The bottom region shows a persistent status line and a contextual hint line.  Saving / copying / cutting displays a ~1.5 s transient notification.  Quit on a dirty buffer opens a three-button confirm modal and does not exit immediately.  `Config::save()` invoked via `save_config_with_flash` produces a `Configuration updated` notification.  Compact mode collapses to one line with chord hints behind `?`.  Hint content adapts when the cursor enters a table or list.

**Deferred to Phase 11:**
- Autosave dirty→clean flash — no autosave path yet exists to emit it.
- `HintPrompt` consumer wiring (file-change `R Reload / I Ignore` etc.) — infrastructure is landed, specific prompts belong with Phase 11.

#### Issues

- Tell me what UI components currently inherit colors from the system terminal theme due to having no color set explicitly by edamame.

- [ ] Is it true that typing currently re-renders the document after each character? I don't think that is necessary or desirable. We don't need to re-render the current line since it is displayed as raw Markdown, and it's actually bad UX because there is a distracting flash when the line quickly goes from raw->rendered->raw. We shouldn't need to re-render the rest of the document either, until the cursor moves to another line, via keyboard/mouse navigation or if the user enters a newline. Is that right?

- [ ] Pressing `Tab` or `Shift-Tab` in any list (ordered, unordered, checkbox) should create a new indented nested list. The indentation should be whatever `tab_width` is set to (default 4) both when rendered and in the raw Markdown. New ordered lists should start over from 1 and maintain the indent for each item. 
- [ ] When the user tries to quit the app with unsaved changes, the unsaved changes modal pops up. If "Save" or "Discard" is selected, the app should quit immediately (after saving if the former), but it just closes the modal, and the user must press another key to get the app to close.

---

### Phase 10 — Command Palette, File Picker, Overlays, and Tabs
*Goal: keyboard-driven discovery and navigation — every action reachable without a mouse, and multiple files open simultaneously.*

Scope covers old Phase 9 *minus* the bottom-bar work (now Phase 9).  Consumes the Phase 9 transient-message channel for "Configuration updated" notifications and the `?` popover widget for the cheat sheet.

**Starting state (already in place after Phases 8–9):**
- Phase 8 builds a per-tab nav stack (`App::nav_back` / `App::nav_forward`) and dirty-buffer guard modal.  Phase 10 lifts this into a `Vec<Tab>` without re-architecting.
- Phase 9's `ModalView` + `?` popover + transient-message channel are the UI primitives for every overlay below.
- `KeyMap::bindings()` already iterates every bound `(KeyEvent, Action)` pair for the default handler — the cheat sheet and command-palette listings consume the same source.

**Tasks — command palette (Ctrl-P):**
- [ ] New widget `src/ui/command_palette.rs`: modal overlay with a single-line input (reuse `ratatui-textarea` from `Cargo.toml`'s editing deps) + a scrollable fuzzy-matched result list.
- [ ] Fuzzy matcher: prefer `nucleo-matcher` (modern, well-maintained, used by Helix) over rolling our own.  Add to `Cargo.toml` in this phase.
- [ ] Each result row shows `action label` + the bound chord (via `KeyMap::chord_for(action)`), so users learn bindings organically.
- [ ] Selecting a result dispatches the `Action` through the normal `edit_ops::apply` path — no palette-specific handlers.

**Tasks — file picker (Ctrl-O):**
- [ ] New widget `src/ui/file_picker.rs`: overlay with a filterable tree rooted at the current document's directory (or the CWD when no file is open).  Prefer a small custom implementation over `tui-tree-widget` — the picker only needs expand/collapse + flat-filter, not full tree manipulation.
- [ ] Recent files shown at the top (persisted in `$XDG_DATA_HOME/edamame/recent.json`, capped at 20).  Selecting a recent file jumps directly without walking the tree.
- [ ] Selecting a `.md` file opens it in the active tab (subject to Phase 8's dirty-buffer guard); selecting a non-`.md` file calls `open::that(path)` (Phase 8 dispatch path, no re-implementation).

**Tasks — settings overlay:**
- [ ] Accessible from the command palette (`Open Settings`).  Key/value list of the sections in `config.toml`, sourced from `ConfigSchema` (a new struct that mirrors `Config` with metadata — description, type, default).  No keybind settings in this overlay.
- [ ] Edit-in-place: Enter on a row opens an inline editor for the value; Esc cancels.  On confirm, `Config::save()` is called — which, via Phase 9, emits the `Configuration updated` flash.
- [ ] Button at the top: `Open config.toml in default editor` → `open::that(&config_path)`.

**Tasks — keybinds overlay:**
- [ ] Separate overlay (command palette entry `Open Keybinds`).  Action-keybind table with edit-in-place.  On confirm, the override is written to `keybindings.toml` via `KeyMap::save_overrides()` (a new method — today `KeyMap` only *reads*) and the `KeyMap` in-memory is updated so the change takes effect immediately.
- [ ] Conflict detection: assigning an already-bound chord produces a sticky `Error` transient message and the edit is rejected.

**Tasks — markdown cheat sheet:**
- [ ] Accessible from the command palette (`Show Markdown Cheat Sheet`).  Reuses the Phase 9 `?` popover widget.  Content tailored to what edamame supports: CommonMark + GFM tables + task lists + strikethrough + footnotes.  No HTML, no raw inline styling shortcuts beyond what the renderer honours.
- [ ] Content is a static `&str` fixture in `src/ui/cheat_sheet.rs` — not parsed from a Markdown file at runtime; the cheat sheet itself is internal doc, not user-facing content.

**Tasks — tabs:**
- [ ] Promote `App::file_path` + `App::editor` + Phase 8's `nav_back` / `nav_forward` into `App::tabs: Vec<Tab>` and `App::active_tab: usize`.  `Tab { path, editor_state, nav_back, nav_forward, scroll }`.  All existing single-file code paths reduce to `self.tabs[self.active_tab].editor_state` style accesses.
- [ ] Tab bar rendered *only when more than one file is open* — single-file sessions show no tab bar, saving a row.  Users who want dedicated single-file windows open another terminal.
- [ ] New bindings: `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle tabs; `Ctrl+W` closes the active tab (subject to dirty-buffer guard).
- [ ] Ellipsis truncation when total tab width exceeds terminal width; active tab is always visible.

**Tasks — multi-file CLI:**
- [ ] Accept `edamame file1.md file2.md file3.md` — each argument opens in its own tab.  First tab is active on startup.  Zero-arg launch continues to show the file picker (if not already the Phase 0 behaviour).

**Tasks — testing:**
- [ ] Integration test: `nucleo-matcher` is wired and a palette-dispatched `Action::Save` produces the same buffer state as a keyboard-dispatched one.
- [ ] Integration test: `Config::save()` via the settings overlay produces the `Configuration updated` flash exactly once.
- [ ] `TestBackend` snapshots for the tab bar (1 tab hidden, 2 tabs visible, N tabs truncated).

**Acceptance criteria:** `Ctrl-P` opens a fuzzy-searchable action palette.  `Ctrl-O` opens a file picker with recent files on top.  The settings overlay edits `config.toml` in place with live feedback.  The keybinds overlay edits `keybindings.toml` with conflict detection.  Multiple files open via CLI args or file picker share a tab bar; `Ctrl+Tab` cycles.  The markdown cheat sheet is one keybind away.

---

### Phase 11 — File Change Detection and Inline Diff
*Goal: detect external file changes and surface an inline diff for agentic workflow support.*

Was old Phase 10.  Renumbered to flow after the Phase 9 status-region infrastructure it consumes.  Agentic workflows (where an AI agent and the user are editing the same file concurrently) are the motivating use case.

**Starting state (already in place after Phases 9–10):**
- Phase 9 provides the hint-line modal-prompt channel and sticky `Error` transient messages.  This phase only writes the specific prompt definitions — no new UI primitives.
- Phase 10 provides the tab-level state model; a file-change watcher attaches per-tab rather than per-process.

**Tasks — watcher:**
- [ ] Add `notify` (latest stable 7.x) and `similar` (2.x) as dependencies.
- [ ] `App::file_watcher: Option<notify::RecommendedWatcher>` — one watcher per `App`, with a filter that dispatches events only for paths matching an open tab's `file_path`.  Sends `AppEvent::FileChanged(PathBuf)` into the existing `mpsc` channel.
- [ ] Debounce: coalesce multiple `Modify` events within a 200 ms window before dispatching — editors like vim produce a write-rename-delete sequence that would otherwise fire the reload prompt three times.

**Tasks — reload flow (clean buffer):**
- [ ] On `FileChanged` for a tab whose `editor.buffer.is_dirty() == false`, post a `HintPrompt` (`R Reload   I Ignore`) via the Phase 9 channel.  No modal overlay, no interruption to editing elsewhere.
- [ ] `R` → `Buffer::load_file` + rebuild `ParsedDoc` + reset scroll to preserve cursor line; `I` → dismiss.
- [ ] Autosave-safe: if `config.editor.autosave` is on and an external change triggers a reload while the buffer is also dirty-from-autosave, treat as dirty (three-way below).

**Tasks — three-way flow (dirty buffer):**
- [ ] On `FileChanged` for a dirty tab, show a sticky `[modified externally]` badge on the status line (Phase 9 slot) and render the diff overlay automatically.  Do **not** auto-dismiss — the user needs to see what's about to be reconciled.
- [ ] Hint-line prompt: `R Reload   S Save copy   O Overwrite   C Cancel`.
      - `R Reload` — discard in-memory changes; load from disk.
      - `S Save copy` — write the in-memory buffer to a new file (auto-named `<stem>.bak.md`; if present, `.bak-2.md`, `.bak-3.md`, …), then reload from disk.  Both versions preserved.
      - `O Overwrite` — write the in-memory buffer to disk, discarding external changes.
      - `C Cancel` — dismiss the diff; leave the buffer as-is.  `[modified externally]` badge stays until next save or explicit reload.

**Tasks — diff overlay:**
- [ ] New widget `src/ui/diff_overlay.rs`.  Uses `similar::TextDiff::from_lines(on_disk, in_memory)` (Myers).  Each change is a `Change { range, side: Deleted | Added | Changed }`.
- [ ] Rendering: overlays the editor content area, preserving scroll so the user isn't disoriented.
      - Deleted lines: red background, strikethrough text.
      - Added lines: green background.
      - Changed lines: stacked — old on top (red), new below (green).
- [ ] Per-change navigation + accept/reject as `Action` variants routed through `KeyMap`:
      - `DiffNextChange` (default `Tab`).
      - `DiffPrevChange` (default `Shift+Tab`).
      - `DiffAcceptExternal` (default `Y`) — keep the on-disk version of the focused change.
      - `DiffRejectExternal` (default `N`) — keep the in-memory version of the focused change.
      - `DiffApply` (default `Enter`) — commit the merged buffer and close the overlay.
      - `DiffCancel` (default `Esc`) — close the overlay without applying.
- [ ] After all changes are resolved, the overlay closes automatically and the merged buffer becomes the new in-memory state.  The `[modified externally]` badge clears on the next successful save.

**Tasks — testing:**
- [ ] Integration test: modify a file on disk while a clean buffer is open, assert the hint prompt fires and `R` reloads correctly.
- [ ] Integration test: modify a file on disk while the buffer is dirty, assert the diff overlay appears and per-change accept/reject produces the expected merged content.
- [ ] Unit tests for the debounce window.
- [ ] `TestBackend` snapshot of the diff overlay for a fixture with one added line, one deleted line, one changed line.

**Acceptance criteria:** Editing a file in the editor while an external process modifies it fires a `R Reload  I Ignore` prompt when clean, or a diff overlay + three-way prompt when dirty.  Per-change accept/reject works and the merged buffer is coherent.  Rapid external writes are debounced.

---

### Phase 12 — Hide HTML Comments in Rendered Views
*Goal: `<!-- ... -->` is annotation, not content — render it invisibly in Preview/Rendered modes while keeping it editable in Raw.*

Extracted from old Phase 11.  This is a **parser + renderer + navigation** change, not polish — it shares the invariants that the virtual-blank-line mechanism relied on in Phase 1 and specialises Phase 6's `tui-columns` handling.  Keeping it isolated from Phase 13's table polish and Phase 14's visual polish makes the test surface tractable.

**Starting state (already in place):**
- `renderer::render_table`'s sibling `Block::Html` arm at `src/markdown/renderer.rs:123` renders *all* HTML — comments and tags alike — as muted text.
- `parser.rs` (around line 320) currently pushes `Event::Html` as `Inline::Text` for inline HTML, so paragraphs containing inline comments show them as body text.
- Raw mode reads directly from the rope buffer, so comment visibility in Raw is already correct; no Raw-side change is needed.
- Phase 6's `merge_trailing_tui_columns_comments` (in `document/parsed_doc.rs`) already strips trailing `<!-- tui-columns: ... -->` blocks from tables and attaches `user_widths` to `Block::Table`.  Phase 12 generalises this pattern.

**Tasks — AST:**
- [ ] New `Block::HtmlComment(String)` variant — content excluding delimiters, byte range preserved for source-map fidelity.  Emitted by a parser post-pass that detects comment-only `Block::Html` (content matches `<!-- ... -->` with optional surrounding whitespace).
- [ ] New `Inline::HtmlComment(String)` variant.  Parser branch in `parser.rs` distinguishes `<!-- ... -->` from inline HTML tags when building inline sequences.

**Tasks — renderer:**
- [ ] `Block::HtmlComment` emits zero lines in Preview and Rendered modes.
- [ ] `Inline::HtmlComment` emits zero spans in Preview and Rendered modes (the surrounding paragraph's other inlines render normally).
- [ ] Raw mode is untouched — it reads the rope directly.

**Tasks — source map & navigation:**
- [ ] `per_block_own` counts zero lines for `HtmlComment` blocks in Preview/Rendered, matching the virtual-blank-line convention from Phase 1.  The byte range is still present in the AST so `SourceMap` coverage stays complete.
- [ ] Hybrid-mode vertical movement (`move_up_visual` / `move_down_visual`) skips zero-rendered-line blocks so the cursor doesn't stall on an invisible comment.
- [ ] Clicking a comment in hybrid mode is impossible (no screen cells belong to it); switching raw → hybrid with the cursor inside a comment snaps the cursor to the start of the next visible block.

**Tasks — Phase 6 specialisation:**
- [ ] After the generic `HtmlComment` promotion pass runs, Phase 6's `merge_trailing_tui_columns_comments` still runs and extracts `user_widths` from trailing `tui-columns` comments.  The two passes don't conflict: the first hides the comment visually, the second extracts semantic data from its source bytes.  Test: an isolated `<!-- tui-columns: [10, 20, 30] -->` block outside any table stays as a hidden `Block::HtmlComment` — no widths are attached to any table.

**Tasks — testing:**
- [ ] Unit tests: round-trip `<!-- hello -->` through parser → renderer → Preview (zero lines); inline `paragraph <!-- x --> text` renders as `paragraph  text` (two spaces collapse at the renderer's discretion); block-level comment between paragraphs renders as zero lines and cursor skips over it.
- [ ] Integration test in `tests/editing.rs`: down-arrow past a block comment lands on the following block's first line, not on the comment.
- [ ] Regression test that Phase 6's `tui-columns` extraction still works when the trailing comment is now a `Block::HtmlComment`.

**Acceptance criteria:** `<!-- ... -->` renders as zero lines in Preview and Rendered modes and as source text in Raw.  Cursor navigation skips comment blocks.  Phase 6's `tui-columns` extraction still works.

---

### Phase 13 — Table Rendering Polish
*Goal: production-quality table visuals: smart column widths, row striping, drag-drop feedback, and user-facing disclosure of comment injection.*

Extracted from old Phase 11.  These items are cohesive (all table-layer concerns) and share the `table_layout` module as their implementation surface.  Consolidates deferred polish items from Phase 6.

**Starting state (already in place):**
- Phase 2 built `table_layout::compute_widths` with user-width overrides.
- Phase 6 extracted the per-frame `TableLayoutSnapshot` + `paint_handles` so visual decorations can render post-line-render.
- Phase 6 deferred drag-drop "drop destination highlighting" as polish.
- Phase 6's `tui-columns` comment injection is silent — the user has no explicit warning that a resize operation will modify the Markdown source.

**Tasks — smart column widths (min-max proportional):**
- [ ] Adopt the min-max proportional distribution (the algorithm browsers use for `table-layout: auto` and what `rich` / `tabulate` converge on):
      - Per column: `min = longest word`, `max = longest cell`.
      - Distribute remaining viewport width weighted by `(max − min)`.
      - Prose columns wrap onto multiple rendered rows when their allocation is below `max`; short/numeric columns stay at their `max`.
      - *Rejected:* average-width-as-target — breaks the invariant that content fits, forces silent truncation of outlier cells.
- [ ] Replace the current `compute_widths` algorithm in `table_layout` (which is auto-to-max subject to a terminal-width cap).  `user_widths`, when present, still override everything — users who set widths explicitly via drag (Phase 6) or comment (persisted) get exactly what they asked for.

**Tasks — manual-width warning modal:**
- [ ] When a Phase 6 column-border drag completes *for the first time on a given table*, show a `ModalView` with text: "Setting custom column widths adds a `<!-- tui-columns: [...] -->` comment to the Markdown source.  Continue?"  Buttons: `Continue` (default) / `Continue and don't ask again` / `Cancel`.
- [ ] `Continue and don't ask again` writes `config.table.warn_on_width_injection = false` via `Config::save()` (which fires the Phase 9 `Configuration updated` flash).
- [ ] `Cancel` reverts the drag — `live_table_widths` is cleared without commit.
- [ ] On tables that already have a `tui-columns` comment, no warning — the comment is already there.

**Tasks — row striping:**
- [ ] Add `Theme::table_row_even` and `Theme::table_row_odd` style slots (default: no-op / same as background).  Themes can override for alternating-row visual aid.
- [ ] `renderer::render_table` applies the alternating style as a background fill per data row (not the header, not the alignment row).
- [ ] Opt-in via `config.table.row_striping: bool` (default `false`) since not every user wants it.

**Tasks — drop destination highlighting:**
- [ ] During a row-handle drag (`DragTarget::TableRow`), highlight the horizontal separator between `hover_row_idx - 1` and `hover_row_idx` using `Theme::table_drop_indicator` (a new style slot).  Paints via a post-pass on `paint_handles`, no buffer mutation.
- [ ] During a column-handle drag (`DragTarget::TableColumnHeader`), highlight the vertical `│` border between `hover_col_idx - 1` and `hover_col_idx`.
- [ ] During a column-border resize (`DragTarget::TableColumnBorder`), show a faint vertical guideline at the current pointer X to indicate where the release will commit.  Optional — if visual noise outweighs value, drop.

**Tasks — testing:**
- [ ] Unit tests in `table_layout` for min-max proportional: a narrow prose column stays at `min` when there's no slack; excess distribution respects the `(max − min)` weighting.
- [ ] Integration test in `tests/mouse.rs`: a column-border drag on a table without `tui-columns` shows the warning modal; `Cancel` reverts with no buffer mutation.
- [ ] `TestBackend` snapshot of a striped-row table.
- [ ] `TestBackend` snapshot of a table mid-drag with row-drop indicator visible.

**Acceptance criteria:** Tables allocate column widths proportionally; narrow prose columns wrap rather than forcing wide columns to truncate.  Drag-to-resize warns on first use that an HTML comment will be injected.  Row striping is opt-in and theme-controlled.  Row / column drag shows a drop indicator on the target separator.

---

### Phase 14 — Visual Polish
*Goal: small UX improvements that make the app feel cared-for.*

Extracted from old Phase 11.  Pulls in the "Heading visual hierarchy — framing/rules" item from *Deferred Work* since the plan itself flagged it as "do this now."  Items are independent — any subset can ship.

**Tasks — checkbox glyphs:**
- [ ] Replace the current `[ ]` / `[x]` text rendering of task-list checkboxes with Unicode glyphs (e.g. ☐ / ☑ or ▢ / ▣) in Preview and Rendered modes.  Raw mode is untouched.
- [ ] `Theme::checkbox_unchecked` and `Theme::checkbox_checked` are `&'static str` slots (not `Style`) so themes can switch glyph sets — `[ ]` / `[x]` remains an opt-in for users on terminals without reliable Unicode.
- [ ] Click hit-test on the task-list checkbox (Phase 5's `toggle_checkbox_at`) must be updated to account for the rendered glyph width rather than the 3-char `[ ]` raw form.

**Tasks — heading visual hierarchy (framing/rules):**
- [ ] H1 gets a full-width `═══` rule above and below.
- [ ] H2 gets a `═══` rule below.
- [ ] H3 gets a `───` rule below.
- [ ] H4–H6 stay colour + bold (current behaviour).
- [ ] Readable everywhere, zero new dependencies.  The `tui-big-text` variant stays in Deferred Work for the theming phase.
- [ ] `per_block_own` accounts for the added rule rows so cursor navigation doesn't skip them.

**Tasks — scroll granularity experiment:**
- [x] Make the wheel-step configurable: `config.editor.mouse_scroll_lines` (default **1** — finer control out of the box; users can bump to 2 or 3).  Seeds `MouseDispatcher::with_wheel_step` at startup.  Keyboard `ScrollUp` / `ScrollDown` intentionally always step by one line — per-keypress has to be fine-grained, so it stays hardcoded.

**Tasks — emoji support:**
- [ ] `config.editor.unicode.emoji_support: bool` (default `false`).  No probing of terminal capabilities — no reliable query exists, and terminals that claim emoji support routinely miscompute cell widths and corrupt layout.
- [ ] When enabled, the renderer passes emoji-bearing strings through `unicode-segmentation` grapheme clusters and uses `unicode-width` cell widths as-is.  When disabled, emoji sequences are rendered as `:shortcode:` text fallback.
- [ ] Revisit automatic detection only if users request it.

**Tasks — image-display onboarding modal:**
- [ ] **Consider dropping.**  Old Phase 11 proposed a "Display Images" three-button modal (Always / Never / This time only) on first-encounter, but Phase 7 already ships `config.image.enabled` (master switch) and an `http`/`https`-specific remote-policy modal.  A third, per-document prompt would be redundant.  Leave unimplemented unless user feedback identifies a concrete gap.

**Acceptance criteria:** Task checkboxes render as Unicode glyphs in Preview/Rendered and toggle on click.  Headings show a visual hierarchy via rules.  Emoji support is opt-in and layout-safe when disabled.  Scroll granularity is configurable.

---

### Phase 15 — Security
*Goal: Ensure the application is not vulnerable to unintentional code execution.*

- [ ] Check if input sanitization is needed. edamame should not run any code in files it opens.
- [ ] Check if sanitization is needed for images.
- [ ] Dependencies — supply chain vulnerabilities.
- [ ] Any other security concerns?

---

### Phase 16 — Exporting
*Goal: Export .md files to other basic formats.*

HTML is the single built-in target; it doubles as the intermediate format for
user-configured custom commands (pandoc, weasyprint, …) so PDF, DOCX, EPUB,
etc. are one external-tool invocation away — edamame stays dependency-free.

**Tasks (Phase 16 core):**
- [x] `src/export/` module — pure-Rust HTML renderer via `pulldown-cmark`
      with GFM options matching the in-app parser (tables, task lists,
      strikethrough, footnotes, smart punctuation) and raw-HTML events
      stripped (Phase 15 overlap).
- [x] Bundled GitHub-ish stylesheet at `config/export/default.css`.  Overridable
      via `[export.html].stylesheet = "/path/to/custom.css"`.
- [x] Optional inline-image embedding (`[export.html].inline_images = true`)
      that rewrites local `![alt](rel/path.png)` references to base64 `data:`
      URIs.  Default off — keeps output compact and portable alongside the
      asset directory.  Remote / `data:` / `file://` URLs are left untouched.
- [x] Custom-command runner (`[[export.custom]]`): the user declares
      entries with `name`, `command`, and `extension`; each pipes the
      rendered HTML through an external tool via `{html}` / `{out}` path
      substitution.  Captures stdout as a fallback when the command does
      not write `{out}` directly.
- [x] Background-thread execution with a caller-supplied `FnOnce` callback
      reporting `Result<PathBuf, String>`; neither HTML render nor the
      custom-command shell-out blocks the UI.
- [x] Atomic temp-file-and-rename write so a partial or interrupted export
      never leaves a truncated file at the target path.
- [x] `preflight(target, overwrite)` returns `PreflightError::TargetExists`
      when the output exists and overwrite is false — exposes the primitive
      the future command-palette overwrite-confirmation modal needs.

**Deferred to Phase 10 (command palette):**
- [ ] Dispatch — palette entries `Export HTML` + one `Export <name>` per
      custom entry.
- [ ] Overwrite confirmation modal when `preflight` returns `TargetExists`;
      on approve, re-invoke the spawner with the overwrite precondition met.
- [ ] Transient-message wiring on the App's mpsc channel (success: "Exported
      to <path>"; failure: sticky error with the tool's stderr).
- [ ] Action enum variants (`ExportHtml`, `ExportCustom(String)`) so the
      palette dispatch stays in the same `Action` pipeline as every other
      command.

---

### Phase 17 — Diagrams
*Goal: See if we can add support for mermaid diagrams.*

Terminals that support showing native images should be able to show diagrams. We can hand off diagram code to a mermaid subroutine, have it generate an image, and display that.

### Phase 18 — Handle Terminal Change
*Goal: Ask user about changing their configuration if they start using a different terminal application*

On occasion a user may start using a new terminal application, which may have improved or degraded support for edamame's features. We should detect this and prompt the user to revise their configuration. For example it would be unfortunate for a user who upgrades to a terminal with better image support to never see images because they disabled image display when they were using the previous terminal application.

---

## Deferred Work

These features should be **architecturally anticipated** from Phase 0 but not implemented until after the numbered phases are complete.

### Vim / Modal Editing
- Implement `VimHandler` in `input/modal/vim.rs` implementing the `ModalHandler` trait
- Internal state machine: `Normal`, `Insert`, `Visual`, `VisualLine`, `VisualBlock`, `Command`
- Support for motions (`w`, `b`, `e`, `0`, `$`, `gg`, `G`, `f`/`F`/`t`/`T`), operators (`d`, `y`, `c`, `=`), text objects (`iw`, `aw`, `is`, `as`, `i"`, `a"`, etc.)
- Ex commands (`:w`, `:q`, `:wq`, `:e`, `:bn`, `:bp`)
- Visual mode selection with `v`, `V`, Ctrl-V
- Substitution (`:s/pattern/replacement/flags`)
- **Count prefixes** (`3j`, `5dw`, `2dd`, etc.) — tracked as a numeric accumulator in `VimHandler` state before each motion or operator
- **Search** (`/pattern`, `?pattern`, `n`, `N`, `*`, `#`) — highlights matches, jumps between them; search state lives entirely in `VimHandler`
- **Dot repeat** (`.`) — repeats the last change; requires `VimHandler` to record the last operator + motion + inserted text
- **Marks** (`ma` to set, `` `a `` / `'a` to jump) — a `HashMap<char, rope_offset>` in `VimHandler` state; `'a` jumps to the line, `` `a `` to the exact offset
- Registers (`"ay`, `"ap`) and macros (`q`/`@`) are deferred further — complex and rarely essential for initial vim support
- The `ModalHandler` trait ensures that **zero Vim-specific logic touches `EditorState`**

### Code Syntax Highlighting
- In the AST renderer, when rendering a fenced code block, identify the language tag
- Use `syntect` (via `syntect_tui` or a custom bridge) to apply token-based highlighting
- Fall back to plain monospace rendering if the language is unrecognised
- Cache highlighted blocks keyed by (language, content hash)

### Theming
- The `Theme` struct is already wired in from Phase 0
- Deferred work: document all theme keys, ship several built-in themes (dark, light, dracula, gruvbox, catppuccin, github dark/light)
- Theme can be selected by name (`theme = "dracula"`) in `config.toml`; resolved first from `~/.config/edamame/themes/`, then from built-ins
- Custom themes are standalone TOML files in `~/.config/edamame/themes/<name>.toml`, mapping semantic keys to hex colour values:
  ```toml
  [ui]
  background = "#1e1e2e"
  foreground = "#cdd6f4"
  cursor     = "#f5e0dc"

  [markdown]
  heading  = "#cba6f7"
  bold     = "#f9e2af"
  code_span = "#a6e3a1"
  link     = "#89b4fa"
  ```
  Unknown keys are a hard error; missing keys fall back to the default theme
- Implement a live theme preview mode in the settings overlay

### Heading visual hierarchy — `tui-big-text` variant
Terminals use a fixed character-cell grid; the app cannot change font size at the cell level.  The zero-dep **framing/rules** approach has been folded into Phase 14 ("Visual Polish").  The larger step below stays deferred to the theming phase:

- **`tui-big-text`** (one small dep): renders text with Unicode half-block characters (▀▄█) at 2×–3× visual height, works with ratatui's `TestBackend` and requires no terminal capability detection. H1 at ~3× and H2 at ~2× gives a genuine size hierarchy. Add as an opt-in `theme.headings.h1_big = true` flag. Implement alongside the rest of the theming work.

---

## Open Questions

1. **Re-parse performance**: For large files (>10,000 lines), re-parsing the entire document on every keystroke may introduce latency. Consider:
   - Debouncing the re-parse to ~50ms after the last keystroke
   - Incremental parsing: `pulldown-cmark` does not natively support incremental parsing, but we can limit re-parsing to the changed block by identifying block boundaries around the edit and splicing the old and new rendered output
   - Benchmark in Phase 1 with large files to determine if this optimisation is needed

2. **Table column width storage**: Storing column widths in an inline HTML comment is a pragmatic choice but slightly pollutes the Markdown file. An alternative is a sidecar file (`.filename.md.tui-meta`). The HTML comment approach is preferred because the file remains self-contained; document this behaviour clearly.

3. **Scrolling in RenderedMode**: When the rendered document is taller than the raw source (e.g. due to table row expansion or long paragraphs wrapping), the visual scroll position and the rope cursor position can diverge. We need a clear mapping between "visual line on screen" and "rope offset". The `SourceMap` addresses this, but edge cases (images, collapsed/expanded blocks) need careful handling.

4. **WSL clipboard**: On WSL, `arboard` may not have access to the Windows clipboard without additional configuration (`clip.exe` workaround or `win32yank`). Detect WSL via `$WSL_DISTRO_NAME` and fall back to `clip.exe` / `powershell.exe Get-Clipboard` as appropriate.

5. **Kakoune / Helix modal mode**: The `ModalHandler` trait is designed to be swappable. A `KakouneHandler` or `HelixHandler` could be implemented as a community contribution without touching core editor logic. Document the trait contract clearly.

7. ~~**Split `config.toml` into multiple files?**~~ **Done — 2026-04-22.**

   Implemented ahead of schedule to lock in the architecture before more
   UI/UX work accrues coupling to the single-file assumption.  See
   Section 6 (Config File Architecture) for the current layout.  Summary:

   ```
   ~/.config/edamame/
   ├── config.toml          # [editor], [modal], [table], [image] + `theme = "<name>"`
   ├── keybindings.toml     # keybinding overrides
   └── themes/
       └── default.toml     # written on first run; add community themes as siblings
   ```

   `Config::load()` → `LoadedConfig { config, keybindings, theme: ThemeFile }`.
   First-run scaffolding writes all three defaults from `include_str!`'d
   repo assets and never overwrites.  A user-authored `ThemeFile` format
   lives in `src/config/theme_file.rs`; `Theme::from_file` converts it
   (with monochrome-fallback short-circuit).  `Config::save()` only writes
   `config.toml`, so an ordinary save-on-prompt path cannot clobber a
   user-edited theme or keybindings file.  Migration was a hard cut —
   stale `[keybindings]` / `[theme]` sections in a pre-split `config.toml`
   are silently ignored.
