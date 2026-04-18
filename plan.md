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
edamame/
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
    │   ├── raw_view.rs             # RawView: plain text editor, renders directly from ropey buffer
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

All colour and style values are routed through the `Theme` struct. There are no hardcoded `ratatui::style::Color` literals in the UI layer. The default theme is defined in code and can be overridden via `~/.config/edamame/config.toml`. This means adding full theme support later requires only exposing the theme config keys — no refactoring.

### 6. Config File Architecture

A single TOML file at `$XDG_CONFIG_HOME/edamame/config.toml` (fallback `~/.config/edamame/config.toml`). Config sections:
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
Undo = "ctrl+z"
Redo = "ctrl+y"
Cut = "ctrl+x"
Copy = "ctrl+c"
Paste = "ctrl+v"
```

`KeyMap` is initialised with the full set of compiled-in defaults, then any keys present in `[keybindings]` are applied on top, replacing only those bindings. Action names are the string representation of the `Action` enum variants. An unrecognised action name or an unparseable key string is a hard error at startup (not silently ignored), so the user knows immediately if they've made a typo.

**Quit confirmation**: The `Quit` action (`Ctrl-Q`) always shows a confirmation dialog (e.g. "Save changes? [Y]es / [N]o / [C]ancel") when there are unsaved changes. When the buffer is clean the app quits immediately. `Ctrl-C` is bound to `Copy` and does not quit. `Escape` is the cancel/dismiss key for modals and dialogs; it does not trigger quit. Note: in crossterm raw mode `ISIG` is disabled, so `Ctrl-C` arrives as a key event rather than SIGINT — we must always intercept it explicitly to prevent SIGINT killing the process and leaving the terminal in raw mode; mapping it to `Copy` satisfies this.

**Undo/redo keybindings**: The compiled-in defaults are `Ctrl-Z` for undo and `Ctrl-Y` for redo. `Ctrl-Shift-Z` is registered as a secondary redo binding when the terminal supports the kitty keyboard enhancement protocol (`PushKeyboardEnhancementFlags`); without this protocol, `Ctrl-Shift-Z` is indistinguishable from `Ctrl-Z` at the byte level. Terminals known to support it: kitty, Alacritty, WezTerm, Ghostty, foot. In terminals that don't, only `Ctrl-Y` is available for redo. The keyboard enhancement flag is activated as part of Phase 4 capability detection and `Ctrl-Shift-Z` is only registered as a redo binding when the flag is successfully set.

### 7. Logging Strategy

`tracing` output is never written to stdout/stderr, because those would corrupt the TUI output. If an error occurs, we will show a popup to the user. Logging to a file (`$XDG_DATA_HOME/edamame/debug.log`) is gated behind a `dev_mode = true` flag in `config.toml` (default: `false`). When `dev_mode` is enabled, `tracing-appender` writes structured logs to the file; when disabled, the tracing subscriber is not initialised and no log file is created.

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
- **Deferred to Phase 6**: the dedicated `TableView` widget, snapshot tests for `TableView`, mouse-driven row/column drag and column resize, and wiring `tui-columns` comments into the renderer/buffer pipeline (the module can already read and write them). The cell-boundary metadata introduced in Phase 2 for the cell-scoped reveal is the seed for Phase 6's mouse hit-testing — `TableView` will take ownership of that metadata when mouse selection lands.

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

### Phase 4 — Capability Detection
*Goal: detect what the terminal supports and gate features accordingly.*

**Tasks:**
- [ ] Implement `terminal/capabilities.rs` with a `Capabilities` struct: `{ colour_depth: ColourDepth, mouse: bool, image_protocol: Option<ImageProtocol>, unicode_full: bool, keyboard_enhancement: bool }` — where `ImageProtocol` is an enum (`Sixel`, `KittyGraphics`, `ITerm2`, `Halfblocks`); `keyboard_enhancement` indicates whether `PushKeyboardEnhancementFlags` succeeded (required for `Ctrl-Shift-Z` redo)
- [ ] Probe at startup using crossterm queries and environment variable heuristics (`$TERM`, `$COLORTERM`, `$TERM_PROGRAM`, `$KITTY_WINDOW_ID`, etc.)
- [ ] Use `ratatui-image`'s `Picker` API for image protocol detection (this handles the detailed probing)
- [ ] Store capabilities in `App` and thread them through to features that need them
- [ ] Log detected capabilities to the tracing log file
- [ ] Graceful degradation: if no colour support, render without ANSI styles; if no mouse, disable all mouse features without error
- [ ] Show a popup modal notice if any features (e.g. mouse) are not available, with `[Ok]` and `[Don't show this again]`. The latter should set a flag in the config file to suppress future warnings.

**Acceptance criteria:** `edamame` starts correctly in a minimal `xterm` (no mouse, 8 colours) and in a feature-rich terminal like Ghostty or Kitty, adapting its behaviour in both cases.

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
- [ ] In terminals with mouse support, scrolling with the mouse should only move the page, not the cursor. The user can then click to move the cursor if desired. Since the cursor does not move when scrolling, there is no need to scroll line by line and we can use a smooth scroll instead, which is more visually pleasing.
- [ ] Allow scrolling up to one page below the last line with the mouse only, such that the last line remains in view at the top of the editor. Scrolling with the keyboard should not have this effect since the cursor is constrained within the editor window.

**Acceptance criteria:** Mouse clicks place the cursor correctly. Text can be selected by dragging. Scrolling works. Checkboxes toggle on click. No crashes or visual glitches on rapid mouse movement.

---

### Phase 6 — Table Row/Column Drag and Column Resize
*Goal: reorder rows and columns by dragging; resize columns by dragging borders.*

**Tasks:**
- [ ] Extract table rendering into a dedicated `TableView` widget (`src/ui/table_view.rs`) that owns the cell-boundary metadata introduced in Phase 2. `renderer::render_table` becomes an internal helper invoked by `TableView`; `RenderedView` delegates table blocks to `TableView` instead of splicing rows inline. The widget is the single owner of mouse hit-testing (which cell, which column border, which row handle).
- [ ] Write `insta` snapshot tests for `TableView` rendering (box-drawing output for a 2×3 and a 3×3 table, plus one case with a multi-line wrapped cell)
- [ ] Wire `<!-- tui-columns: [...] -->` persistence into the renderer/buffer pipeline — `table_layout` already parses/formats the comment; Phase 6 adds the load path (apply persisted widths to the `TableLayout` on parse) and the save path (emit or update the comment when a column is resized). Only user-set widths are persisted; auto-computed widths never emit a comment.
- [ ] Render a row-drag handle (e.g. `⠿` or `≡`) in the leftmost position of each non-header table row
- [ ] On mouse-down on a row handle and subsequent drag: show a visual indicator of the row being dragged and its destination position; on mouse-up, reorder the rows in the underlying buffer and re-parse
- [ ] Render column border separators as interactive drag targets (detect mouse-down within 1 column of a `│` border character)
- [ ] On drag of a column border: update the column width in real time as the mouse moves; commit on mouse-up; persist in the inline comment
- [ ] Render a column-drag handle in the header row (e.g. `⇔` above each column) for reordering
- [ ] On drag of a column header: reorder columns in the buffer (swap all cells in the column across all rows)
- [ ] Minimum column width: 3 characters (to show at least `...`)
- [ ] All drag operations are undoable via `Undo`

**Acceptance criteria:** Rows can be dragged to new positions. Column borders can be dragged to resize. Columns can be reordered by dragging their headers. User-set column widths round-trip through the `tui-columns` comment. The underlying Markdown is correctly updated after each operation.

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
  - Support both local file paths and HTTP/HTTPS URLs (load URLs with `ureq`, cache to disk in `$XDG_CACHE_HOME/edamame/images/`).
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
- [ ] Implement a settings overlay, accessible from the command palette: key-value list of common settings, editable inline; changes are written back to config.toml upon confirmation. Include a button to open config.toml in the default editor. This overlay should not show keybinds settings.
- [ ] Implement a keybinds overlay, accessible from the command palette: action-keybind list of all keybinds, editable inline; changes are written back to config.toml upon confirmation.
- [ ] Add a markdown cheat sheet (tailored to the markdown supported by this app), accessible from the command palette.
- [ ] Implement tab bar if multiple files are open (from link navigation or command line args)
- [ ] Accept multiple file arguments on the command line: `edamame file1.md file2.md`
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
- [ ] In the diff view, implement per-change accept/reject as `Action` variants routed through `KeyMap`: `DiffNextChange` (default: Tab), `DiffPrevChange` (default: Shift-Tab), `DiffAccept` (default: Y — keep the on-disk version of this change), `DiffReject` (default: N — keep the in-memory version of this change)
- [ ] After accepting/rejecting all changes, diff view closes and the buffer reflects the merged result
- [ ] This feature is particularly useful for agentic workflows where an AI agent is editing the file concurrently

**Acceptance criteria:** Editing a file in the editor while an external process modifies it triggers a notification. The diff overlay correctly shows all changes. Accepting/rejecting individual changes works correctly and the resulting buffer is coherent.

---

## Deferred Work

These features should be **architecturally anticipated** from Phase 0 but not implemented until after the numbered phases are complete.

### Useful undo/redo
Undo/redo currently works on a per-character basic, which is not very useful. We should figure out how to make it more useful.

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

### Optimization
- **High CPU Usage**: We should see what optimizing we can do to improve the performance of the app. For one thing, idle CPU usage high on my machine, which I think should be significantly lower when the app is just displaying static output and not being interacted with. Interestingly, CPU usage seems to decrease over time. Memory usage is too low to even be outputted by `ps aux`, so that's fine.

```
edamame main  ? ❯ ps aux|head -1
USER         PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND
edamame main  ? ❯ ps aux|grep markdown
mjw      3192551  7.4  0.0  78956  7724 pts/11   Sl+  13:24   0:50 ./target/debug/edamame example.md
edamame main  ? ❯ ps aux|grep markdown
mjw      3192551  6.7  0.0  78956  7724 pts/11   Sl+  13:24   1:06 ./target/debug/edamame example.md
edamame main  ? ❯ ps aux|grep markdown
mjw      3192551  6.6  0.0  78956  7724 pts/11   Sl+  13:24   1:10 ./target/debug/edamame example.md
edamame main  ? ❯ ps aux|grep markdown
mjw      3192551  6.3  0.0  78956  7724 pts/11   Sl+  13:24   1:38 ./target/debug/edamame example.md
```

- **Terminal resize**: `crossterm` sends a `Resize(cols, rows)` event when the terminal window is resized. All layout calculations — especially table column widths and paragraph word wrap — must be recalculated on resize. Ensure the rendered view and source map are invalidated and rebuilt when this event is received. Rather than attempting to re-render WHILE the terminal is being resized, which would undoubtedly be laggy or flickery, we will simply re-render AFTER it has been resized. We could potentially blank the screen during the resize operation.

### Heading visual hierarchy — how to show H1–H6 at "larger" sizes
Terminals use a fixed character-cell grid; the app cannot change font size at the cell level. Practical options in increasing complexity:

- **Framing/rules** (zero new deps): H1 gets a full-width `═══` rule above and below; H2 gets one rule below; H3 gets a `───` rule below; H4–H6 stay colour+bold. Readable everywhere, zero overhead. This is the **immediate improvement** — do this now.
- **`tui-big-text`** (one small dep): renders text with Unicode half-block characters (▀▄█) at 2×–3× visual height, works with ratatui's `TestBackend` and requires no terminal capability detection. H1 at ~3× and H2 at ~2× gives a genuine size hierarchy. Add as an opt-in `theme.headings.h1_big = true` flag. This is the **medium-term improvement** — implement in the theming phase.

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

7. **Split `config.toml` into multiple files?**

   The compelling argument for splitting is specifically themes: a `themes/` directory enables in-app theme switching and community theme sharing, mirroring the approach taken by Helix and Zellij. Moving keybinding configuration will more easily enable the keybinding overlay feature in Phase 9.

   Adopted layout:
   ```
   ~/.config/edamame/
   ├── config.toml          # [editor] and [modal] only
   ├── keybindings.toml     # [keybindings]
   └── themes/
       ├── default.toml     # written out on first run if missing
       ├── catppuccin.toml
       └── gruvbox.toml
   ```

   `Config::load()` reads `config.toml` for general settings, then `keybindings.toml` for keybinds, then loads `themes/<active_theme>.toml` (defaulting to `themes/default.toml`). The `ThemeConfig` struct should be populated now, before any user config files exist in the wild.

   **Decision**: yes, extract themes into a `themes/` directory. Implement during the theming/config phase. Keybindings get moved to `keybindings.toml`.
