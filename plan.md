# Markdown TUI Editor — Development Plan

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
| `ratatui-textarea` | latest (ratatui org fork) | Text editing widget — used for single-line raw-mode input within table cells and the active raw line; may be wrapped or partially reimplemented |
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
| `notify` | 7.x | File-system watching (Phase 10) |
| `similar` | 2.x | Diff computation for inline change display (Phase 10) |
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
│   - Config: loaded from ~/.config/markdown-tui/config.toml      │
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
// - notify watcher: sends AppEvent::FileChanged (Phase 10)
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

Background threads are spawned lazily: the image loader pool in Phase 7, the file watcher in Phase 10. The `AppEvent` enum gains new variants at those phases; earlier phases only ever see `AppEvent::Term`.

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
markdown-tui/
├── Cargo.toml
├── README.md
├── plan.md
├── overview.md
├── config/                         # Example and default config files
│   └── default_config.toml
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
    │   ├── raw_view.rs             # RawView: plain text editor (ratatui-textarea backed)
    │   ├── table_view.rs           # TableView: rendered table with raw-cell overlay
    │   ├── status_bar.rs           # StatusBar widget: mode, file, cursor pos, dirty flag
    │   ├── file_picker.rs          # FilePicker overlay widget
    │   ├── diff_overlay.rs         # DiffOverlay: inline red/green diff (Phase 10)
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

All colour and style values are routed through the `Theme` struct. There are no hardcoded `ratatui::style::Color` literals in the UI layer. The default theme is defined in code and can be overridden via `~/.config/markdown-tui/config.toml`. This means adding full theme support later requires only exposing the theme config keys — no refactoring.

### 6. Config File Architecture

A single TOML file at `$XDG_CONFIG_HOME/markdown-tui/config.toml` (fallback `~/.config/markdown-tui/config.toml`). Config sections:
- `[editor]` — tab width, word wrap, auto-save, etc.
- `[theme]` — colour overrides
- `[keybindings]` — key → action overrides (see below)
- `[modal]` — which modal handler to use (default: `"default"`)

At startup, the default config is loaded, then user config is merged on top (not replaced). Missing keys always fall back to defaults. The config struct is validated at load time with informative errors.

**Keybinding overrides**: `[keybindings]` is a TOML table mapping action names to key strings, e.g.:

```toml
[keybindings]
Save = "ctrl+s"
ToggleRawMode = "ctrl+`"
Quit = "ctrl+q"
```

`KeyMap` is initialised with the full set of compiled-in defaults, then any keys present in `[keybindings]` are applied on top, replacing only those bindings. Action names are the string representation of the `Action` enum variants. An unrecognised action name or an unparseable key string is a hard error at startup (not silently ignored), so the user knows immediately if they've made a typo.

### 7. Logging Strategy

`tracing` output is written **only** to a log file (`$XDG_DATA_HOME/markdown-tui/debug.log`) and never to stdout/stderr, because those would corrupt the TUI output. If an error occurs, we will show a popup to the user. We will not output errors or logs to a file unless in development mode, as determined by a flag in the config file.

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

**Tasks:**
- [ ] `cargo new markdown-tui` — set up workspace with `Cargo.toml`
- [ ] Add `ratatui`, `crossterm`, `pulldown-cmark`, `ropey`, `serde`, `toml`, `dirs`, `thiserror`, `anyhow`, `tracing`, `tracing-appender` as dependencies
- [ ] Implement `Config` with serde/toml deserialization; load from XDG path with defaults fallback
- [ ] Implement `KeyMap` in `config/keymap.rs`: define the `Action` enum and all compiled-in default key bindings; after loading `Config`, iterate the `[keybindings]` table and override defaults — error at startup on unknown action names or unparseable key strings
- [ ] Implement `Theme` with default dark-mode colour palette wired to all rendered elements
- [ ] Implement `Buffer` wrapping `ropey::Rope` with `load_file` / `save_file` / `insert` / `delete` / `line` / `line_count`
- [ ] Implement `parser.rs` — parse a `&str` with pulldown-cmark, return typed AST
- [ ] Implement `renderer.rs` — walk AST, produce `Vec<ratatui::text::Line>` with styling for: headings (H1–H6), bold, italic, code spans, fenced code blocks, blockquotes, bullet lists, horizontal rules, links (styled but not yet clickable)
- [ ] Implement `PreviewView` widget — renders styled lines with vertical scrolling (no cursor)
- [ ] Implement `StatusBar` — shows filename, line count, mode label
- [ ] Implement basic `App` event loop: draw → read event → handle Ctrl-C / q to quit, scroll with arrow keys / PgUp / PgDn / Home / End
- [ ] Set up `tracing-appender` to write logs to file before TUI starts
- [ ] Add `insta` and `proptest` as dev-dependencies in `Cargo.toml`
- [ ] Write snapshot tests in `tests/renderer.rs` covering headings H1–H6, bold, italic, inline code, fenced code block, blockquote, bullet list, and horizontal rule — assert `Vec<Line>` output with `insta::assert_debug_snapshot!`
- [ ] Write a `TestBackend` rendering test for `StatusBar` (filename, line count, mode label)
- [ ] Manual smoke test: open several `.md` files in a Linux terminal and in macOS/WSL to verify no visual regressions beyond what automated tests cover

**Acceptance criteria:** `markdown-tui path/to/file.md` opens the file in preview mode, renders styled Markdown, scrolls smoothly, and quits cleanly.

---

### Phase 1 — Hybrid Rendered/Raw Editing
*Goal: editing in RenderedMode where the cursor line is shown raw, all other lines rendered.*

**Tasks:**
- [ ] Before implementing document-layer types: write unit tests for `Buffer` (insert, delete, boundary conditions, line indexing), `Cursor` (move left/right/up/down, preferred column behaviour at line ends), and `History` (undo/redo, undo past empty stack, redo cleared after new edit) — implement each module to make the tests pass
- [ ] Write integration tests in `tests/editing.rs`: construct an `EditorState`, apply `InsertChar` / `Newline` / `DeleteChar` / `Undo` / `Redo` sequences, assert buffer content and cursor position after each step
- [ ] Implement `SourceMap` — after parsing, build a `Vec<(usize, usize)>` of (start_offset, end_offset) per rendered line, using `pulldown-cmark`'s offset iterator
- [ ] Write `proptest` round-trip tests for `SourceMap`: for any sequence of edits, every offset in the buffer maps to exactly one rendered line, and the ranges are non-overlapping and cover the full buffer
- [ ] Implement `Cursor` — stores a rope char offset and a preferred visual column (for vertical movement)
- [ ] Implement `Selection` — anchor + active rope offsets; None when no selection
- [ ] Implement `History` — undo/redo stack; each entry is an `EditDelta { offset, removed: String, inserted: String }`; undo/redo reconstruct and replay deltas
- [ ] Implement `EditorState` — owns `Buffer`, `Cursor`, `Selection`, `History`, `Mode`, `ParsedDoc`
- [ ] Implement `actions.rs` — define `Action` enum: `InsertChar(char)`, `DeleteChar`, `DeleteWord`, `MoveLeft/Right/Up/Down`, `MoveLineStart/End`, `MoveDocStart/End`, `Newline`, `Undo`, `Redo`, `ToggleRawMode`, `EnterEditMode`, `ExitToPreview`, `Save`, `Quit`, etc.
- [ ] Implement `edit_ops.rs` — apply `Action` variants to `EditorState`, updating buffer, cursor, and history
- [ ] Implement `RenderedView` — for each visual line, check if it contains the cursor; if so, render a raw inline text input widget for that line (using `ratatui-textarea` or a custom single-line widget); otherwise render the styled Markdown line
- [ ] Implement `DefaultHandler` in `input/modal/default.rs` — map key events to `Action` values using a configurable `KeyMap`
- [ ] Implement mode transitions: Preview → Rendered on typing, Rendered ↔ Raw on Ctrl-\`
- [ ] Implement word-wrap for paragraph text in rendered mode (wrap at terminal width)
- [ ] Implement auto-scroll: keep cursor line visible when editing
- [ ] Implement `Save` action: write buffer to disk via `Buffer::save_file`
- [ ] Track dirty state; show `[modified]` in status bar
- [ ] Implement basic clipboard: yank line (Ctrl-Y), paste (Ctrl-P); use OS clipboard via `arboard` if available, internal kill-ring otherwise

**Acceptance criteria:** Can open a .md file, navigate with arrow keys, type to edit, undo/redo, save with Ctrl-S. The cursor line appears raw while the rest of the document is rendered. Switching to Raw mode shows the whole document as plain text.

---

### Phase 2 — Table Editing
*Goal: frictionless table editing; user never sees raw table border syntax.*

**Tasks:**
- [ ] Implement `TableLayout` — given a GFM table AST node and available width, compute column widths (auto from content, min column width, user-set widths) and cell text wrapping
- [ ] Implement `TableView` widget — renders a table using box-drawing characters (e.g. `┌─┬─┐`); handles multi-line cells by expanding row height
- [ ] Implement `table_edit.rs` — given a cursor offset, detect if inside a table; identify which row/column; extract the cell content (raw Markdown between `|` delimiters)
- [ ] Implement cell editing overlay — when cursor is in a cell, replace that cell in the rendered table with an inline text input; all other cells remain rendered
- [ ] Tab / Shift-Tab to move between cells; wrap to next/previous row
- [ ] Enter in a cell confirms the edit; arrow-key-down from last row appends a new row
- [ ] Typing a `|` character within a cell is escaped as a literal character (raw `\|`), not treated as a column separator
- [ ] Implement column width persistence: store per-file column widths in a trailing HTML comment `<!-- tui-columns: [20, 15, 30] -->` within the table; parse and apply on load (only for user-set column widths)
- [ ] Implement table-aware `Newline` action: pressing Enter at the end of a table (outside a cell) inserts a new paragraph, not a new table row
- [ ] Write unit tests in `tests/table.rs` covering: cell content extraction for empty cells, cells with bold/italic, cells with code spans, and wide Unicode characters; column-width computation for various table widths; `|` escaping round-trip
- [ ] Write `insta` snapshot tests for `TableView` rendering (box-drawing output for a 2×3 and a 3×3 table)

**Acceptance criteria:** Opening a file with a GFM table shows a rendered bordered table. Navigating into a cell allows editing the cell content inline. Tab moves between cells. Column widths adjust sensibly. The underlying Markdown is valid and well-formed after every edit.

---

### Phase 3 — Smart List Editing
*Goal: numbered lists auto-continue and self-heal.*

**Tasks:**
- [ ] Before implementing, write tests in `tests/list_edit.rs` covering: bullet list continuation, numbered list continuation with correct next number, double-Enter exits the list, inserting an item mid-list renumbers subsequent items, nested lists at multiple indentation levels, task list continuation (`- [ ] `), and toggle-checkbox (`[ ]` ↔ `[x]`) — implement `list_edit.rs` to make each test pass
- [ ] Implement `list_edit.rs` — detect when cursor is at the end of a list item line
- [ ] On `Newline` inside a bullet list item: insert `- ` (or matching bullet character) at the start of the new line
- [ ] On `Newline` inside a numbered list item: insert `N. ` where N is the correct next number
- [ ] On `Newline` on a blank list-item line (i.e. pressing Enter twice): exit the list by removing the list prefix and inserting a blank paragraph
- [ ] Implement list renumbering: after any insert/delete/paste that changes a numbered list, scan the list and re-number all items sequentially
- [ ] Implement renumbering on paste: if a block of lines is pasted into the middle of a numbered list, renumber the whole list
- [ ] Handle nested lists: detect indentation level; continue the list at the same level
- [ ] Handle task list items: `- [ ] ` → `- [ ] `; `- [x] ` → `- [ ] ` (new unchecked item)
- [ ] Implement toggle-checkbox action (Ctrl-Space or T when cursor is on a task list item): toggles `[ ]` ↔ `[x]`

**Acceptance criteria:** Typing in a numbered list auto-continues with the correct next number. Pressing Enter twice exits the list. Inserting items into the middle of a list renumbers subsequent items correctly. Nested lists work at multiple indentation levels.

---

### Phase 4 — Capability Detection
*Goal: detect what the terminal supports and gate features accordingly.*

**Tasks:**
- [ ] Implement `terminal/capabilities.rs` with a `Capabilities` struct: `{ colour_depth: ColourDepth, mouse: bool, image_protocol: Option<ImageProtocol>, sixel: bool, kitty_graphics: bool, iterm2: bool, unicode_full: bool }`
- [ ] Probe at startup using crossterm queries and environment variable heuristics (`$TERM`, `$COLORTERM`, `$TERM_PROGRAM`, `$KITTY_WINDOW_ID`, etc.)
- [ ] Use `ratatui-image`'s `Picker` API for image protocol detection (this handles the detailed probing)
- [ ] Store capabilities in `App` and thread them through to features that need them
- [ ] Log detected capabilities to the tracing log file
- [ ] Graceful degradation: if no colour support, render without ANSI styles; if no mouse, disable all mouse features without error
- [ ] Show a one-time notice in the status bar if a required feature (e.g. mouse) is not available

**Acceptance criteria:** `markdown-tui` starts correctly in a minimal `xterm` (no mouse, 8 colours) and in a feature-rich terminal like Ghostty or Kitty, adapting its behaviour in both cases.

---

### Phase 5 — Mouse Support
*Goal: full mouse interaction — clicks, drags, scrolling, checkboxes.*

**Tasks:**
- [ ] Enable `crossterm::event::EnableMouseCapture` on startup (if `capabilities.mouse`)
- [ ] Implement `mouse.rs` — parse `MouseEvent` variants: `Down`, `Up`, `Drag`, `ScrollUp`, `ScrollDown`
- [ ] Click in PreviewMode → transition to RenderedMode, place cursor at clicked position (via source map)
- [ ] Click in RenderedMode → move cursor to clicked position; if clicking a different table cell, switch active cell
- [ ] Click-drag → begin text selection; update selection while dragging
- [ ] Double-click → select word under cursor
- [ ] Triple-click → select line under cursor
- [ ] Scroll wheel → scroll view (in PreviewMode and RenderedMode when document is longer than screen)
- [ ] Click on a rendered link → open in browser / navigate to local file (Phase 8 prerequisite, but register the hit-test region here)
- [ ] Click on a task list checkbox `[ ]` / `[x]` → toggle it
- [ ] Drag table row handle (leftmost column, rendered as `≡`) to reorder rows (prerequisite for Phase 6)

**Acceptance criteria:** Mouse clicks place the cursor correctly. Text can be selected by dragging. Scrolling works. Checkboxes toggle on click. No crashes or visual glitches on rapid mouse movement.

---

### Phase 6 — Table Row/Column Drag and Column Resize
*Goal: reorder rows and columns by dragging; resize columns by dragging borders.*

**Tasks:**
- [ ] Render a row-drag handle (e.g. `⠿` or `≡`) in the leftmost position of each non-header table row
- [ ] On mouse-down on a row handle and subsequent drag: show a visual indicator of the row being dragged and its destination position; on mouse-up, reorder the rows in the underlying buffer and re-parse
- [ ] Render column border separators as interactive drag targets (detect mouse-down within 1 column of a `│` border character)
- [ ] On drag of a column border: update the column width in real time as the mouse moves; commit on mouse-up; persist in the inline comment
- [ ] Render a column-drag handle in the header row (e.g. `⇔` above each column) for reordering
- [ ] On drag of a column header: reorder columns in the buffer (swap all cells in the column across all rows)
- [ ] Minimum column width: 3 characters (to show at least `...`)
- [ ] All drag operations are undoable via `Undo`

**Acceptance criteria:** Rows can be dragged to new positions. Column borders can be dragged to resize. Columns can be reordered by dragging their headers. The underlying Markdown is correctly updated after each operation.

---

### Phase 7 — Image Display
*Goal: render inline images using the best available terminal graphics protocol.*

**Tasks:**
- [ ] In the AST renderer, identify `Image` nodes (alt text + URL)
- [ ] For each image node, determine the display area (a block of lines reserved in the layout)
- [ ] Implement `image_view.rs` using `ratatui-image`:
  - Use `Picker` (initialised in Phase 4) to select the best available protocol
  - Load images lazily (only when they scroll into the visible area)
  - Cache decoded+resized images keyed by (path, display_width, display_height)
  - Show a placeholder (alt text in brackets) while the image loads or if loading fails
  - Support both local file paths and HTTP/HTTPS URLs (load URLs with `reqwest` or `ureq`, cache to disk in `$XDG_CACHE_HOME/markdown-tui/images/`).
  - If HTTP/S URLs are present in the file, show a popup on startup asking if user wants to load images from remote server (always/never/this time only). This should be a hook in the renderer.
- [ ] Respect terminal cell dimensions when computing image size
- [ ] Implement a `[image]` config section: `max_width`, `max_height`, `enabled: bool`
- [ ] In PreviewMode and RenderedMode, images are shown inline at their rendered position
- [ ] In RawMode, images are shown as their raw Markdown `![alt](url)` syntax

**Acceptance criteria:** A .md file with `![alt](./image.png)` displays the image inline in supporting terminals. Unsupported terminals show `[alt]` in a styled block. Large images are scaled down to fit the display area.

---

### Phase 8 — Clickable Links and File Navigation
*Goal: follow links on click; open other Markdown files in the editor.*

**Tasks:**
- [ ] During rendering, register link areas in a `HitMap`: `Vec<(Rect, LinkTarget)>` where `LinkTarget` is `Url(String)` or `LocalFile(PathBuf)` or `AnchorId(String)`
- [ ] On left-click over a link area (detected in `mouse.rs`): dispatch a `FollowLink` action
- [ ] `FollowLink(Url)` → open the URL in the system browser via `open` crate (cross-platform)
- [ ] `FollowLink(LocalFile)` → if the file is a .md file, push it onto a navigation stack and render it; otherwise open with the OS default application
- [ ] Implement a navigation stack: `Vec<(PathBuf, ScrollPosition, CursorOffset)>`; Backspace or Alt-Left pops back
- [ ] `FollowLink(AnchorId)` → scroll to the heading with the matching slug within the current document
- [ ] Show the link target in the status bar when the cursor hovers over a link
- [ ] In RawMode, Ctrl-click (or a keybinding) follows a link from the raw `[text](url)` syntax by parsing it inline

**Acceptance criteria:** Clicking a URL link opens the browser. Clicking a relative `.md` path opens that file in the editor. Back-navigation returns to the previous file and position. Heading anchors scroll correctly.

---

### Phase 9 — Status Bar, Menu, and File Picker
*Goal: a polished UI chrome with file navigation and settings access.*

**Tasks:**
- [ ] Expand `StatusBar` to show: mode indicator, file path, dirty marker (`*`), cursor position (`line:col`), selection size (when selection active), detected image protocol
- [ ] Implement a command palette (Ctrl-P): fuzzy-searchable list of actions, opens as an overlay
- [ ] Implement `FilePicker` overlay widget: shows directory tree (using `tui-tree-widget` or a custom implementation); navigable with arrows, filterable by typing
- [ ] File picker opens with Ctrl-O; shows recent files at the top
- [ ] Implement a settings overlay: key-value list of common settings, editable inline; changes are written back to config.toml
- [ ] Implement tab bar if multiple files are open (from link navigation or command line args)
- [ ] Accept multiple file arguments on the command line: `markdown-tui file1.md file2.md`
- [ ] Show key binding hints for the current mode at the bottom of the status bar (hideable via config)

**Acceptance criteria:** Status bar shows all relevant information at a glance. File picker is fast and keyboard-navigable. Command palette lists all actions with their key bindings. Multiple files can be open simultaneously.

---

### Phase 10 — File Change Detection and Inline Diff
*Goal: detect external file changes and show an inline diff for agentic workflow support.*

**Tasks:**
- [ ] Add `notify` as a dependency; start a background watcher thread after opening each file
- [ ] On file change notification: if the buffer is unmodified, offer to reload (status bar prompt: `[R]eload / [I]gnore`)
- [ ] If the buffer is modified when a change is detected: show a `[modified externally]` warning and render the diff view automatically; offer three options (keep the diff visible until the user confirms so they can see what they're choosing to keep or discard):
  - **[R]eload** — discard in-memory changes, load the on-disk version
  - **[S]ave copy** — write the in-memory buffer to a new file (auto-named `filename.bak.md` or user-prompted), then reload from disk, preserving both versions
  - **[O]verwrite** — write the in-memory buffer to disk, discarding the external changes
- [ ] Implement `DiffOverlay` using `similar` (Myers diff algorithm): compare in-memory buffer against on-disk content; render:
  - Deleted lines: red background, strikethrough
  - Added lines: green background
  - Changed lines: show both old (red) and new (green) inline
- [ ] In the diff view, implement per-change accept/reject: navigate to each change with `]c` / `[c`; press `a` to accept (keep the new version) or `r` to reject (keep the buffer version)
- [ ] After accepting/rejecting all changes, diff view closes and the buffer reflects the merged result
- [ ] This feature is particularly useful for agentic workflows where an AI agent is editing the file concurrently

**Acceptance criteria:** Editing a file in the editor while an external process modifies it triggers a notification. The diff overlay correctly shows all changes. Accepting/rejecting individual changes works correctly and the resulting buffer is coherent.

---

## Deferred Features

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
- Theme can be selected by name (`theme = "dracula"`) in `config.toml`; resolved first from `~/.config/markdown-tui/themes/`, then from built-ins
- Custom themes are standalone TOML files in `~/.config/markdown-tui/themes/<name>.toml`, mapping semantic keys to hex colour values:
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
- Should support custom theming via a separate theme file. ==COMMENT: WHAT FORMAT SHOULD THIS BE?==

---

## Open Questions

1. **ratatui-textarea vs. custom raw-line widget**: `ratatui-textarea` provides a lot of editing functionality out of the box (undo/redo, Emacs bindings, search). However, it manages its own internal text state, which conflicts with our `ropey`-backed `Buffer` as single source of truth. The likely solution is to use `ratatui-textarea` **only** for ephemeral inline cell editing (the raw-line overlay), treating it as a short-lived input widget whose content is flushed back to the buffer on commit — similar to how an `<input>` element works in HTML. Investigate this in Phase 1.

2. **Re-parse performance**: For large files (>10,000 lines), re-parsing the entire document on every keystroke may introduce latency. Consider:
   - Debouncing the re-parse to ~50ms after the last keystroke
   - Incremental parsing: `pulldown-cmark` does not natively support incremental parsing, but we can limit re-parsing to the changed block by identifying block boundaries around the edit and splicing the old and new rendered output
   - Benchmark in Phase 1 with large files to determine if this optimisation is needed

3. **Table column width storage**: Storing column widths in an inline HTML comment is a pragmatic choice but slightly pollutes the Markdown file. An alternative is a sidecar file (`.filename.md.tui-meta`). The HTML comment approach is preferred because the file remains self-contained; document this behaviour clearly.

4. **Scrolling in RenderedMode**: When the rendered document is taller than the raw source (e.g. due to table row expansion or long paragraphs wrapping), the visual scroll position and the rope cursor position can diverge. We need a clear mapping between "visual line on screen" and "rope offset". The `SourceMap` addresses this, but edge cases (images, collapsed/expanded blocks) need careful handling.

5. **WSL clipboard**: On WSL, `arboard` may not have access to the Windows clipboard without additional configuration (`clip.exe` workaround or `win32yank`). Detect WSL via `$WSL_DISTRO_NAME` and fall back to `clip.exe` / `powershell.exe Get-Clipboard` as appropriate.

6. **Terminal resize**: `crossterm` sends a `Resize(cols, rows)` event when the terminal window is resized. All layout calculations — especially table column widths and paragraph word wrap — must be recalculated on resize. Ensure the rendered view and source map are invalidated and rebuilt when this event is received. Rather than attempting to re-render WHILE the terminal is being resized, which would undoubtedly be laggy, we will simply re-render AFTER it has been resized. We could potentially blank the screen during the resize operation.

7. **Kakoune / Helix modal mode**: The `ModalHandler` trait is designed to be swappable. A `KakouneHandler` or `HelixHandler` could be implemented as a community contribution without touching core editor logic. Document the trait contract clearly.
