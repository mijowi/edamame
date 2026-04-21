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
  Add `pub image: ImageConfig` to `Config`. Extend `config/default_config.toml`  with the new section and annotations.

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

### Phase 8 — Clickable Links and File Navigation
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
- [ ] Add `open = "5"` to `Cargo.toml`.  No system-library dependency; the crate shells out to `xdg-open` / `open` / `start` per platform.

**Tasks — AST-backed link hit-test (upgrade from Phase 5's source scan):**
- [ ] New module `src/ui/link_view.rs` modelled on `ui::image_view` and `ui::table_view`.  Owns `LinkLayoutSnapshot { rect: Rect, target: LinkTarget }` and a `build_snapshots(state, area, scroll)` entry point that walks the visible rendered-line range, consulting the AST rather than re-scanning the raw line.  Covers three AST-level targets:
      - `Inline::Link { url, .. }` — the common case; also reachable from inside paragraph, heading, list-item, and table-cell inlines.
      - GFM autolinks emitted by `pulldown-cmark` as `Event::Start(Link)` with `LinkType::Autolink` / `Email` — **confirm** that our parser already surfaces these as `Inline::Link` (they should, since no dedicated variant exists); add a parser test if so, or a new branch if not.
      - Reference-style links `[text][id]` — requires preserving the link definition table during parse.  `pulldown-cmark` resolves reference links to the same `Tag::Link` event as inline links when the definition exists, so nothing extra is needed beyond confirming this in a parser test.
- [ ] `LinkTarget` enum in `src/editor/link.rs` (new module):
      ```rust
      pub enum LinkTarget {
          Url(String),            // http, https, mailto, …
          LocalFile(PathBuf),     // relative or absolute, any extension
          Anchor(String),         // `#slug` within the current document
      }
      ```
      Classification happens once at snapshot build time via `LinkTarget::parse(url: &str, base_dir: Option<&Path>) -> LinkTarget`: `#foo` → `Anchor`; RFC-3986 scheme (or `mailto:`) → `Url`; everything else → `LocalFile`, resolved relative to the document dir.
- [ ] `RenderedViewState::link_snapshots: Vec<LinkLayoutSnapshot>` and `PreviewState::link_snapshots: Vec<LinkLayoutSnapshot>`, populated at the end of each `render()` pass.  The `App` hit-tests against the appropriate state when dispatching mouse actions, matching the pattern from Phase 6's `table_snapshots` and Phase 7's `image_snapshots`.
- [ ] **Raw-reveal fallback**: when the cursor's block is in the `RAW_REVEAL_DELAY` window, the revealed line shows raw `[text](url)` — the AST-backed snapshot won't have spans for the raw bytes.  `mouse_ops` falls back to `link_at_offset` against the raw source only for the revealed block.  Both paths produce a `LinkTarget` via the same parser so downstream dispatch is uniform.

**Tasks — click / keyboard dispatch:**
- [ ] Extend `MouseAction::Click` (and `DoubleClick`, `TripleClick`) to carry the `KeyModifiers` from `crossterm::event::MouseEvent`. `MouseDispatcher::dispatch` threads the modifier bits through; today they're dropped.  Without this, Ctrl-click can't be distinguished from a plain click in `mouse_ops::apply`.
- [ ] `mouse_ops::apply` click handling, per mode:
      - **Preview**: plain click OR `Ctrl`-click on a `LinkLayoutSnapshot` → `FollowLink` (the document is read-only; link click is the unambiguous intent).
      - **Rendered**: plain click places the cursor (existing behaviour); `Ctrl`-click on a `LinkLayoutSnapshot` → `FollowLink`.
      - **Raw**: same as Rendered.  The raw-reveal fallback path handles the revealed-block case.
- [ ] Add `Action::FollowLinkUnderCursor`, `Action::NavigateBack`,
      `Action::NavigateForward` to `config/keymap.rs`.  Default bindings:
      - `FollowLinkUnderCursor`: `Ctrl-Enter` in edit modes. Preview mode does not have a cursor but users get the click path.
      - `NavigateBack`: `Alt+Left`.  **Not** `Backspace` — that's already `Action::DeleteCharBack` in edit modes.
      - `NavigateForward`: `Alt+Right`.
      - Navigation actions only apply when navigating between Markdown files that open in edamame. There is no concept of navigating forward or back when opening a link in a different app.
- [ ] `FollowLinkUnderCursor` resolves the link at the cursor's rope offset by consulting the AST (same classification as the mouse path), so the action works identically whether invoked by keyboard or mouse.

**Tasks — `FollowLink` dispatch by target:**
- [ ] `LinkTarget::Url` — spawn a worker thread that calls `open::that(&url)` and reports completion via a new `AppEvent::LinkOpenResult(Result<(), String>)`.  On failure, surface the error on the hint line (Phase 11 transient message; until Phase 11 lands, fall back to `tracing::warn!` and no user-visible indication).  Do **not** block the main loop — slow `xdg-open` invocations would stall the UI for several hundred ms.
- [ ] `LinkTarget::LocalFile` with `.md` extension (case-insensitive) → push the current `(PathBuf, scroll, cursor_offset)` onto a navigation stack on `App` (not `EditorState` — same UI-layer-fact rationale as `drag_target`), load the new file into `EditorState::load_file`, rebuild the `ImageCache` since image URLs are now relative to a new base dir, and reset scroll/cursor to document start.
- [ ] `LinkTarget::LocalFile` with any other extension → `open::that(&path)` on the worker thread (same path as `Url`), letting the OS pick the handler.
- [ ] `LinkTarget::Anchor(slug)` → resolve via a new `ParsedDoc::heading_anchors: HashMap<String, usize>` (slug → rendered line index), built during `ParsedDoc::build` from each `Block::Heading`'s plain-text `inlines_to_plain`.  Slug algorithm matches GFM: lowercase, strip characters not in `[a-z0-9 -]`, replace runs of whitespace with `-`, uniquify with `-N` suffix on collision. On miss, no-op (do **not** open an unrelated file with that name). Scrolls the viewport so the heading sits at the top; in edit modes also places the cursor on the heading's first line.

**Tasks — navigation stack:**
- [ ] `App::nav_back: Vec<NavEntry>` and `App::nav_forward: Vec<NavEntry>` where `NavEntry = { path: PathBuf, scroll: usize, cursor_offset: usize, mode: Mode }`.  `NavigateBack` pops `nav_back` and pushes the current state onto `nav_forward`; `NavigateForward` is the inverse.  Following a new link clears `nav_forward` (browser semantics).
- [ ] **Dirty-buffer guard**: when `FollowLink` would navigate away from the current file and `editor.buffer.is_dirty()`, display a three- button `ModalView` (`Save` / `Discard` / `Cancel`) — same pattern as the Phase 7 remote-image prompt.  `Save` persists and continues; `Discard` continues without saving; `Cancel` aborts.
- [ ] Phase 9 note: this navigation stack is per-tab-history, not a replacement for Phase 9's tab bar.  The tab bar in Phase 9 renders one entry per *currently open* file; the nav stack is the linear history *within* a single tab.  Phase 9 can lift the stack into a `Vec<Tab { path, nav_back, nav_forward, editor_state }>` without re-architecting.

**Tasks — hover target display:**
- [ ] Extend `hit_test_clickable` to return the hovered `LinkTarget` (not just a bool) so `App` can stash the currently hovered link on `App::hovered_link: Option<LinkTarget>`.  Phase 11 will surface the target (and `Inline::Link::title`, when present) on the contextual hint line.  Until Phase 11 lands, wire the field but don't render it — the pointer-shape change is already a sufficient affordance.

**Tasks — testing:**
- [ ] Unit tests for `LinkTarget::parse`:  `#heading` → `Anchor`; `https://example.com` → `Url`; `mailto:a@b.c` → `Url`; `./sibling.md` → `LocalFile(absolute)`; `../other.md` → `LocalFile` resolving through the base dir; bare `foo.md` with no base dir stays relative.
- [ ] Unit tests for GFM slug generation: round-trip `"Hello, World!"` → `"hello-world"`, collision uniquification (`"Foo"` twice → `"foo"` + `"foo-1"`), Unicode stripping.
- [ ] Unit tests for `ParsedDoc::heading_anchors` — one entry per heading, correct line index, stable across reparses when headings are unchanged.
- [ ] Integration tests in `tests/mouse.rs`: plain click on a link in Preview opens (assert the `FollowLink` action was dispatched via a test hook / recorded side-effect; we don't actually spawn `xdg-open`).  Plain click on a link in Rendered places cursor. Ctrl-click on a link in Rendered opens.
- [ ] Integration test for the nav stack: open file A, follow link to file B, `NavigateBack` returns to A at the original scroll and cursor position; `NavigateForward` returns to B.
- [ ] Integration test for the dirty-buffer guard: dirty A + click a link to B shows the modal; `Cancel` leaves A unchanged; `Discard` loads B.
- [ ] Integration test for the raw-reveal fallback: cursor in block, click       the revealed `[text](url)` syntax — `FollowLink` fires with the correct target.

**Tasks — deferred to later phases:**
- [ ] Hint-line tooltip with link target + title (Phase 11 — hint line      ownership).
- [ ] Tab-bar integration of the nav stack (Phase 9 — tab bar ownership).

**Acceptance criteria:** Clicking a URL link in Preview opens the browser. `Ctrl`-click on a link in Rendered/Raw mode opens it without moving the cursor. `Ctrl-Enter` on a link in rendered/raw mode follows it. Clicking a relative `.md`path navigates to that file in the same editor window, reusing the imagecache's base-dir-resolution convention. `Alt+Left` / `Alt+Right` walk the navigation history. Heading anchors (`#slug`) scroll to the matching heading. Dirty buffers prompt before being replaced. The pointer shape already changes on hover (Phase 5); the hint-line tooltip is explicitly deferred to Phase 11.

---

### Phase 9 — Status Bar, Menu, and File Picker
*Goal: a polished UI chrome with file navigation and settings access.*

**Tasks:**
- [ ] Expand `StatusBar` persistent line (the **lower** of the two status rows — see Phase 11 layout) to show: mode indicator, file path, dirty marker (`*`), cursor position (`line:col`), selection size (when selection active). Detected image protocol is intentionally *not* surfaced here — users who want to see it can reach it via the settings overlay / an `:info` command.
- [ ] Implement a command palette (Ctrl-P): fuzzy-searchable list of actions, opens as an overlay
- [ ] Implement `FilePicker` overlay widget: shows directory tree (using `tui-tree-widget` or a custom implementation); navigable with arrows, filterable by typing
- [ ] File picker opens with Ctrl-O; shows recent files at the top
- [ ] Implement a settings overlay, accessible from the command palette: key-value list of common settings, editable inline; changes are written back to config.toml upon confirmation. Include a button to open config.toml in the default editor. This overlay should not show keybinds settings.
- [ ] Implement a keybinds overlay, accessible from the command palette: action-keybind list of all keybinds, editable inline; changes are written back to config.toml upon confirmation.
- [ ] Add a markdown cheat sheet (tailored to the markdown supported by this app), accessible from the command palette.
- [ ] Implement tab bar — rendered **only when more than one file is open** (from link navigation or command-line args). Single-file sessions show no tab bar at all, saving a row. Users who want dedicated single-file windows can open another terminal.
- [ ] Accept multiple file arguments on the command line: `edamame file1.md file2.md`
- [ ] Any time the configuration file is updated by the app, we want this to be evident to the user. Add a temporary "Configuration updated" notification to the status bar. This should not be hardcoded into each possible place where the configuration might be updated. Instead, write once and call from everywhere that it's needed. For example, when the remote images modal pops up, if the user selects "Always" or "Never" to load remote images, the "Configuration updated" notification should be displayed. This is a clue to the user that they can go back and change this in the configuration file if they desire.

> The status region is **two lines**, stacked immediately below the editor content: an upper **hint line** (contextual keybind hints, transient status messages, and modal prompts — owned by Phase 11) and a lower **status line** (the persistent info specified above — owned by Phase 9). Dynamic content sits adjacent to the editor; persistent state anchors the bottom edge. Phase 11 also defines an opt-in single-line compact mode. Phase 10's reload / save-copy prompts render on the hint line.

**Acceptance criteria:** Status bar shows all relevant information at a glance. File picker is fast and keyboard-navigable. Command palette lists all actions with their key bindings. Multiple files can be open simultaneously.

---

### Phase 10 — File Change Detection and Inline Diff
*Goal: detect external file changes and show an inline diff for agentic workflow support.*

**Tasks:**
- [ ] Add `notify` as a dependency; start a background watcher thread after opening each file
- [ ] On file change notification: if the buffer is unmodified, offer to reload (status bar prompt: `R Reload   I Ignore`)
- [ ] If the buffer is modified when a change is detected: show a `[modified externally]` warning and render the diff view automatically; offer three options (keep the diff visible until the user confirms so they can see what they're choosing to keep or discard):
  - **R Reload** — discard in-memory changes, load the on-disk version
  - **S Save copy** — write the in-memory buffer to a new file (auto-named `filename.bak.md` or user-prompted), then reload from disk, preserving both versions
  - **O Overwrite** — write the in-memory buffer to disk, discarding the external changes
- [ ] Implement `DiffOverlay` using `similar` (Myers diff algorithm): compare in-memory buffer against on-disk content; render:
  - Deleted lines: red background, strikethrough
  - Added lines: green background
  - Changed lines: show both old (red) and new (green) inline
- [ ] In the diff view, implement per-change accept/reject as `Action` variants routed through `KeyMap`: `DiffNextChange` (default: Tab), `DiffPrevChange` (default: Shift-Tab), `DiffAccept` (default: Y — keep the on-disk version of this change), `DiffReject` (default: N — keep the in-memory version of this change)
- [ ] After accepting/rejecting all changes, diff view closes and the buffer reflects the merged result
- [ ] This feature is particularly useful for agentic workflows where an AI agent is editing the file concurrently

**Acceptance criteria:** Editing a file in the editor while an external process modifies it triggers a notification. The diff overlay correctly shows all changes. Accepting/rejecting individual changes works correctly and the resulting buffer is coherent.

---
 

### Phase 11 — More Polish
*Goal: further UX work to make the app fun and easy to use*

- [ ] Checkbox gylphs

- [ ] **Emoji support** — config opt-in, default off. No probing of terminal capabilities in this phase: no reliable query exists, and terminals that claim emoji support routinely miscompute cell widths and corrupt layout. Revisit if users request automatic detection.

- [ ] Add a contrasting background for all keybind hints, like nano.

- [ ] Quit without saving confirmation modal

- [ ] Experiment with 1- and 2-line scrolling instead of 3-line. Maybe it will feel smoother and still be fast.

- [ ] Add a "It looks like your terminal is capapble of displaying images. When do you want edamame to display images?" modal—"This time only" (default), "Always", "Never"

#### Bottom Bars
A **two-line status region** by default, stacked directly beneath the editor content:

- **Hint line** (upper, adjacent to content) — owned by this phase. Carries contextual keybind hints, transient status messages ("Saved", "Copied", "Autosaved"), and modal prompts (Phase 10 reload / save-copy, future filename or search inputs).
- **Status line** (lower, at the bottom edge) — owned by Phase 9. Carries persistent state: mode indicator, file path, dirty marker, cursor `line:col`, selection size.

The dynamic surface sits closest to the cursor because it describes *"what can I do here"* — putting it adjacent to content minimises eye travel, and transient messages appear right next to the action that triggered them. Persistent info anchors the bottom edge as stable reference data, matching the nano / mc / htop convention.

**Rationale for two lines (vs. one).** The common failure mode for a TUI is horizontal, not vertical: users routinely run edamame in a tmux pane or tiling-WM split that's 50–80 cols wide but still has full vertical height. A single-line status forces aggressive truncation of either info or hints exactly when both matter most. Two lines gives both regions room and gives Phase 10 prompts a natural home.

**Hint-line states** (mutually exclusive):
- *Default* — contextual keybind hints (see task list below).
- *Transient message* — a status notification overlays for ~1.5s, then reverts to hints. Errors stick until dismissed.
- *Modal prompt* — a prompt (Phase 10 reload / save-copy filename, future search, etc.) replaces the hints until dismissed.

**Compact mode.** Optional `status_bar = "compact"` in `config.toml` collapses to a single line by dropping the hint line entirely — only the persistent status line remains. Keybinds become reachable via a `?` popover. Not the default; opt-in for users on very short terminals or who prefer minimal chrome.

**Input during a transient message.** Input is never blocked. If `Copied` is on-screen and the user hits `^X`, the cut fires normally and the next message / hint revert proceeds.

**Keybind notation convention.** Plain letter-plus-label everywhere a key is surfaced to the user — hint line *and* prompt overlays. Examples: `^C Copy`, `^X Cut` for Ctrl-chords; `R Reload`, `I Ignore` for bare keys. This supersedes the `[R]eload / [I]gnore` bracket notation previously in Phase 10 (already updated to match).

- [ ] **Contextual hint line** — default hint-line content; adapts to cursor context.
  - [ ] Preview mode: `any key → edit   ^C Copy   ^P Menu   ^Q Quit`. Global chords (^C/^P/^Q) are reserved; "any other key" triggers the mode switch to edit.
  - [ ] Hybrid / raw edit mode: `^C Copy  ^X Cut  ^V Paste  ^S Save  ^P Menu  ^Q Quit`.
  - [ ] Hybrid mode, cursor inside a table: replace the hint line with table manipulation keybinds. Full names when terminal width allows, abbreviated fallback when it doesn't. A `?` popover exposes the full list when abbreviation is unavoidable.
- [ ] **Transient status messages** — overlay the hint line for ~1.5s, then revert to hints. Errors stick until dismissed.
  - [ ] `Copied` on both copy *and* cut (the deletion is self-evident; the clipboard side-effect is not). No notification on paste.
  - [ ] `Autosaved` only on the dirty → clean transition, not on every autosave cycle, to avoid noise.
- [ ] **Compact-mode fallback** — honour `status_bar = "compact"` in `config.toml`: render only the persistent status line; expose hints via a `?` popover.

#### Tables
- [ ] **Smart table column widths** — adopt a min-max proportional distribution (the algorithm browsers use for `table-layout: auto` and what `rich` / `tabulate` converge on):
      - Per column: `min = longest word`, `max = longest cell`.
      - Distribute remaining viewport width weighted by `(max − min)`.
      - Prose columns wrap onto multiple rendered rows when their allocation is below `max`; short/numeric columns stay at their `max`.
      - *Rejected:* average-width-as-target — it breaks the invariant that content fits, forcing silent truncation of outlier cells.
- [ ] If the user manually adjusts column widths, a popup modal should be shown warning that an HTML comment will be added to the Markdown source to store the column widths.
- [ ] Table row striping
- [ ] Table row and column reorder drop destination highlighting
- [ ] **Hide HTML comments in rendered and preview modes** — `<!-- ... -->`      is annotation, not content. Today `renderer::render_table`'s sibling      `Block::Html` arm (`src/markdown/renderer.rs:123`) renders all HTML as      muted text; comments should render as zero lines in rendered and      preview modes while staying visible in raw mode (raw reads the buffer      directly, so this falls out for free).
      - [ ] Parser: detect comment-only `Block::Html` (content matches            `<!-- ... -->` with optional surrounding whitespace) and promote            to a `Block::HtmlComment(String)` variant — keeps the byte            range in the AST for source-map fidelity without conflating            with renderable HTML.
      - [ ] Inline path: `parser.rs:320` currently pushes `Event::Html` as            `Inline::Text`; add an `Inline::HtmlComment` branch for inline            `<!-- ... -->` so paragraphs containing comments don't show            them as body text.
      - [ ] Renderer: `Block::HtmlComment` and `Inline::HtmlComment` emit            zero lines / zero spans in `Preview` and `Rendered` modes.            `Raw` mode is untouched.
      - [ ] Cursor navigation: hybrid-mode vertical movement skips            zero-rendered-line blocks so the cursor doesn't stall on an            invisible comment. Clicking a comment in hybrid mode is            impossible (no screen cells belong to it); switching raw → hybrid            with the cursor inside a comment snaps the cursor to the start            of the next visible block.
      - [ ] Phase 6's `tui-columns` handling becomes a specialisation:            after the generic comment-hide pass, `markdown::parser` /            `ParsedDoc::build` additionally strips trailing            `<!-- tui-columns: ... -->` blocks from a table's byte range and            attaches `user_widths` to `Block::Table`. The two passes don't            conflict — the first hides the comment visually, the second            extracts semantic data from it.

---

### Phase 14 — Security
*Goal: Ensure the application is not vulnerable to unintenional code execution*
- [ ] Check if input sanitization is needed. edamame should not run any code in files it opens.
- [ ] Check if sanitization is needed for images.
- [ ] Dependencies—supply chain vulnerabilities
- [ ] Any other security concerns?


---

### Phase 12 — Exporting
*Goal: Export .md files to other basic formats*

**Tasks:**
- [ ] HTML export
- [ ] PDF export

---

### Phase 13 — Diagrams
*Goal: See if we can add support for mermaid diagrams*

Terminals that support showing images should be able to show diagrams. We can hand off diagram code to a mermaid subroutine, have it generate an image, and display that.

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
