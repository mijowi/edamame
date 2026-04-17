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

`tracing` output is never written to stdout/stderr, because those would corrupt the TUI output. If an error occurs, we will show a popup to the user. Logging to a file (`$XDG_DATA_HOME/markdown-tui/debug.log`) is gated behind a `dev_mode = true` flag in `config.toml` (default: `false`). When `dev_mode` is enabled, `tracing-appender` writes structured logs to the file; when disabled, the tracing subscriber is not initialised and no log file is created.

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
- [x] `cargo new markdown-tui` — set up workspace with `Cargo.toml`
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

**Acceptance criteria:** `markdown-tui path/to/file.md` opens the file in preview mode, renders styled Markdown, scrolls smoothly, and quits cleanly.

**Implementation notes (deviations and additions vs. original plan):**

- **`src/lib.rs` added** — a library crate entry point was added (not in original plan) so that `tests/renderer.rs` and `tests/ui.rs` can import from `markdown_tui::`. Required by Rust's integration test model; integration tests cannot reference a binary-only crate.
- **`src/editor/mode.rs` created in Phase 0** — the `Mode` enum (`Preview / Rendered / Raw`) was defined upfront to support type-safe mode handling in the app and status bar, even though only `Preview` is active in Phase 0.
- **`src/ui/editor_view.rs` added** — a top-level `EditorView` stateful widget was added to compose `PreviewView` + `StatusBar` and act as the root UI widget. Dispatching to the Phase 1 `RenderedView` and `RawView` will simply be new `match` arms here.
- **`src/terminal/capabilities.rs` stubbed** — minimal `Capabilities` struct created with `detect()` returning conservative defaults, ready for Phase 4 probing without any structural refactoring.
- **Full `Action` enum defined upfront** — all actions across phases (Phase 1–3: editing, clipboard, selection, undo) were added to the enum in `config/keymap.rs` immediately, so keybindings are stable from day one. Phase 0 actions are implemented; later-phase actions are no-ops until their phase.
- **Quit confirmation dialog deferred** — the plan specified "Ctrl-Q / Ctrl-C with confirmation dialog" for Phase 0. Since the Phase 0 buffer is always clean (read-only preview, never modified), quit is immediate. The confirmation dialog will be wired up in Phase 1 when the dirty flag becomes meaningful.
- **Extra dev-dependency: `tempfile`** — added for file I/O tests in `buffer.rs`; not listed in the original dependency table.
- **Extra dependencies: `unicode-width`, `unicode-segmentation`, `tracing-subscriber`** — `unicode-width` and `unicode-segmentation` added for correct Unicode column-width handling in the renderer; `tracing-subscriber` added to initialise the file-based logging subscriber. All were implied by the plan but not listed in the dependency table.
- **Parser: additional options enabled** — `ENABLE_TASKLISTS`, `ENABLE_FOOTNOTES`, `ENABLE_SMART_PUNCTUATION` flags added to the pulldown-cmark parser in addition to the planned `ENABLE_TABLES` and `ENABLE_STRIKETHROUGH`. Task list checkboxes (`[ ]` / `[x]`) are fully parsed and rendered.

#### To Fix
- [x] Add support for highlighting with double equals: `==Highlighted text==`. **Fixed 2026-04-13.** Added `Inline::Highlight` AST node; post-processing in parser splits `Text` events on `==…==` patterns; renderer applies `theme.highlight` (yellow-bg/black-fg); `inlines_to_plain` updated.
- [x] When entering rendered edit mode from preview mode, the first keystroke should make the cursor appear and NOT write a character. Current behavior writes a character. **Fixed 2026-04-13.** `InsertChar`, `InsertTab`, and `Newline` now transition Preview→Rendered without performing the edit; the second keypress performs the action.
- [x] Checklists currently show a bullet in front of the checkbox. Remove the bullet. **Fixed 2026-04-13.** Task items (`item.task.is_some()`) now use indentation-only as the marker instead of `•`.
- [x] Checklists currently format a completed item with strike through. Add an option to strike through or not strike through completed items. **Fixed 2026-04-13.** Added `task_strikethrough: bool` to `Theme` (default `true`). `task_checked` style no longer contains `CROSSED_OUT`; the renderer applies it conditionally.
- [x] Unordered and ordered lists currently have a blank line between items, but they should not. **Fixed 2026-04-13.** Removed the `Line::raw("")` that was appended after each list item's paragraph.
- [x] The previous fix removing blank lines after list items has introduced a bug that causes list items to consume any blank lines below them. The desired end state is that list items do not *automatically* have a blank line below them, but if there is one, it is rendered, and signifies the end of the preceding list. **Fixed 2026-04-14.** Added a single trailing `Line::raw("")` at the end of `render_list`, consistent with all other block types. This separates the list from the next block without adding blank lines *between* items.
- [x] There is an off by one glitch in the table row length calculation, leading to the column borders being 1 character offset to the right and not lining up with the header and bottom border. **Fixed 2026-04-14.** Column widths are now computed from the *rendered* char width of each cell (via `rendered_inlines_char_width`) rather than `inlines_to_plain(...).len()` (byte count). `render_table_row` likewise measures actual rendered span char count for padding. Additionally, `render_line` now applies `line.style.patch(span.style)` instead of `span.style` alone, so lines built with `Line::styled(str, style)` (which stores the style at the line level) are displayed correctly in rendered-edit mode.
- [x] In raw edit mode, the cursor always remains on the screen when scrolling, but in rendered edit mode, when scrolling to the bottom of the document, the last few lines of the document are not visible and the cursor overflows out of the rendered area and is also not visible. **Fixed 2026-04-14.** `ensure_cursor_visible` in Rendered mode now computes the virtual last line of the cursor block using the raw source line count (via `raw_line_count_for_cursor`), which can differ from the rendered line count. `MoveDocEnd` also calls `ensure_cursor_visible` after `scroll_to_bottom`.
- [x] Code blocks should be rendered in a block with a different colored background (the same color as inline code blocks) instead of within a table. The colored block should be the full width of the terminal at minimum (wider if the line is wider than the terminal and code line wrapping is not enabled). Add a configuration option for code in code blocks to wrap or not wrap when exceeding the terminal width (default no wrap). **Fixed 2026-04-14.** Replaced the box-border rendering with plain background-colored lines using `code_block_text` style (bg now matches inline code spans at `Color::Indexed(236)`). Each line is padded to `max(content_width, viewport_width)`. Added `code_block_wrap: bool` to `EditorConfig` (default `false`) and `Renderer::with_code_wrap`. Language tag shown on its own line above the block using `code_block_lang` style.
- [x] In preview mode, lines that exceed the width of the terminal wrap, which is the desired default behavior (except for code blocks—see above). But in editing mode (both rendered and raw), long lines overflow outside the terminal instead of wrapping. The desired end state is that long lines will wrap by default in preview mode and both editing modes. This should be configurable. **Fixed 2026-04-14.** Both `RawView` and `RenderedView` now wrap long lines at the terminal width. `render_line` returns the number of visual rows consumed so the main loop can advance `vis_y` accordingly. Added `line_wrap: bool` to `EditorConfig` (default `true`); wiring to views is deferred to Phase 1.
- [x] Tables borders are not colored correctly. The majority of the table border is one color, but the pipe symbols that make up the column borders are a different color. **Fixed 2026-04-14.** Root cause: `render_line` only consulted `span.style`, ignoring the line-level `Line::style` field. Border rows built with `Line::styled(str, border_style)` stored the style at the line level, so pipe spans (which carried `border_style` at span level) showed up in a different color. Fixed by using `line.style.patch(span.style)` in `render_line`.
- [x] When switching from preview mode to editing mode (both raw and rendered), if you have scrolled in preview mode, the cursor is still at the top of the document, meaning you won't be able to see the cursor and there is a jarring jump back to the top when pressing a navigation key. Change the behavior so that the cursor moves with the page in preview mode, just like in editing mode, but is invisible until switching to edit mode. **Fixed 2026-04-14.** Added `sync_cursor_to_scroll` in `edit_ops.rs`, called on every Preview→edit transition (`EnterEditMode`, `ToggleRawMode`, `InsertChar`, `InsertTab`, `Newline`, `enter_edit_if_preview`). Uses `SourceMap::original_byte_for_rendered_line(scroll)` to move the cursor to the start of the topmost visible block.

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

These emerged from the "To Fix" iterations and are documented as gotchas in
`AGENTS.md` ("Phase 1 Architectural Notes"). Summary for plan-reading agents:

- **Virtual blocks for blank lines**: `ParsedDoc::build` synthesises a one-byte
  block per blank line (leading, between-block, and trailing). Replaced the
  earlier use of `parse_offsets::covering_ranges` for cursor lookup, which
  silently absorbed blank-line bytes into adjacent blocks.
- **`per_block_own` vs. extended ranges**: `ParsedDoc` tracks both per-block
  *own* rendered line counts (used by `RenderedView` to size the raw-replacement
  region) and *extended* covering ranges (used for cursor-to-block lookup).
- **Jitter-suppression reveal**: `RAW_REVEAL_DELAY = 120 ms`; `RenderedView`
  keeps the cursor block fully rendered and overlays an inverted-cell cursor
  indicator at `(cursor_col, cursor_row)` until the delay elapses. App loop
  uses `recv_timeout(60 ms)` so the redraw fires without a keypress.
- **Single shared `line_render` module**: `PreviewView` and `RenderedView` both
  call `ui::line_render::render_line` for word-aware wrap and trailing-cell
  background fill (so styled blocks like code blocks extend full width).
- **NBSP padding in code blocks**: blank code-block lines pad with U+00A0,
  not space, to work around a ratatui `WordWrapper` (`trim: false`) bug that
  produces a spurious extra empty visual row for all-whitespace lines.
- **Word-group undo merging**: `History::record` merges single alphanumeric
  inserts into the prior delta when contiguous. Cursor moves break the group.
- **Visual line navigation**: `move_up_visual` / `move_down_visual` and
  `line_render::render_line` share the same wrap algorithm via
  `visual_rows_of_str` / `sub_line_of_col`.
- **Per-line raw replacement (not per-block)**: `RenderedView` replaces only
  the single rendered line containing the cursor, not the whole block, when
  the reveal delay elapses.

**Known unfixed issues carried into later phases:**

- Scrolling beyond the last element in raw and hybrid edit modes (cursor stops
  at last line). Deferred to Phase 5 (Mouse Support); see "To Fix" entry.
- Click+drag text selection. Deferred to Phase 5 (Mouse Support).

**Acceptance criteria:** Can open a .md file, navigate with arrow keys, type to edit, undo/redo, save with Ctrl-S. The cursor line appears raw while the rest of the document is rendered. Switching to Raw mode shows the whole document as plain text.

- To-do: Check clipboard functionality works.

#### To Fix
- [x] When scrolling in hybrid edit mode, it's jarring to see elements briefly de-render when scrolling quickly. Add a very short delay so that an element is only de-rendered when the user pauses the cursor on that line. **Fixed 2026-04-14.** Added `cursor_block_idx`, `cursor_block_entered_at`, and `RAW_REVEAL_DELAY` (120 ms) to `EditorState`. `RenderedView` checks `cursor_block_revealed()` before switching the cursor block to raw mode; during the delay the block stays rendered. `App::run` uses `recv_timeout(60 ms)` so the reveal triggers a redraw without waiting for a keypress.
- [x] The previous scrolling render delay fix above seems to be timing how long the cursor is *within an element*, e.g. inside a table, rather than how long the cursor is on a line. This has the effect of the app being significantly more likely to render larger multiline elements than single or few-line elements when scrolling. The render delay should be based on the pause time *per line*, not per element. **Fixed 2026-04-14.** Added `cursor_line_idx` to `EditorState`; `update_cursor_block` now resets `cursor_block_entered_at` when the buffer line index changes rather than when the block index changes.
- [x] The cursor should remain visible at all times when scrolling in hybrid edit mode. Current behavior is that the cursor is not visible when scrolling and only appears when the user pauses long enough to derender the current line. **Fixed 2026-04-14.** `RenderedView` now overlays an inverted first-character indicator on the approximate cursor line during the delay window.
- [x] Line wraps should always break on the previous non-alphanumeric character (whitespace or punctuation, dashes, etc.) unless the length of the current word exceeds the width of the editor. Current behavior breaks in the middle of words. **Fixed 2026-04-14.** `render_line` in `rendered_view.rs` rewrote with a word-aware algorithm: searches backward from the column limit for the last non-alphanumeric char; falls back to hard break when no boundary found.
- [x] When a line in a code block is longer than the width of the editor, it breaks the code block rendering by adding a blank line after every line of text. **Fixed 2026-04-14.** `block_width` is now capped at `viewport_width` so short lines are never over-padded beyond the terminal width.
- [x] Add "hold shift to select", which should work with cut, copy, paste, delete, insert over (typing replaces the selection). Holding shift and navigating or click+drag selecting with the mouse should highlight text as selected. We must either incorporate or work around the terminal's built-in text selection. **Partially fixed 2026-04-14.** Shift+Arrow key bindings added to the default keymap (`shift+left/right/up/down` → `Select{Left,Right,Up,Down}`). The `Select*` actions already handled cut/copy/paste/delete correctly. Click+drag selection is deferred to Phase 5 (Mouse Support).
- [x] In preview mode, blank lines in a code block are currently rendered as 2 blank lines, one with the editor background color and one with the code block background color. Hybrid edit mode correctly displays a single line with the code block background color. **Fixed 2026-04-14.** `render_code_block` now uses `content.split('\n')` and pops the trailing empty string (pulldown-cmark always appends `\n`) instead of `content.lines()`. **Re-fixed 2026-04-15.** Root cause was ratatui's `WordWrapper` with `trim: false`: all-whitespace lines (80 spaces) trigger a bug that produces an extra empty line. Fix: blank code lines now use U+00A0 (NBSP) padding instead of regular spaces; ratatui treats NBSP as non-whitespace so the WordWrapper bug is not triggered.
- [x] In preview and hybrid edit mode, multiple blank lines are currently collapsed into one blank line. While this is in accordance with markdown convention, we will render multiple blank lines by default instead of collapsing. Add this as a configuration option. **Fixed 2026-04-14.** Added `preserve_blank_lines` to `EditorConfig` (default `true`). `ParsedDoc::build` counts `\n` in inter-block gaps and inserts extra `Line::raw("")` entries for each extra blank line. **Re-fixed 2026-04-15.** Gap blank lines were attributed to the preceding block in `rendered_to_block`. When the cursor was in that block, `RenderedView` replaced ALL attributed lines (including gap blanks) with raw source lines, collapsing the gaps. Fix: `ParsedDoc` now tracks per-block OWN rendered line counts (before gap inserts); `RenderedView` uses `cursor_block_own` (not `cursor_block_rendered`) when mapping virtual indices to rendered indices, so gap blank lines after the cursor block are always preserved.
- [x] Allow scrolling the view up to one page below the last line of a document. The cursor should always remain visible, and when scrolled as far down as possible the cursor should be stopped on the very top line of the editor. **Fixed 2026-04-14.** `scroll_down` max changed to `total - 1` (last line at top of viewport). Added `clamp_cursor_to_viewport_top` called after `ScrollDown`/`ScrollPageDown` in `edit_ops`. **Re-fixed 2026-04-15.** `clamp_cursor_to_viewport_top` (Rendered mode) previously called `original_byte_for_rendered_line` which returns a block's START byte; if the block spans many rendered lines starting before scroll, the cursor was placed at the block start (before scroll) and stuck. Now scans forward from `self.scroll` until it finds the first block whose rendered start is ≥ scroll. **Review: Still not fixed.** Both raw and hybrid edit modes still do not scroll beyond the last element. We will address this in the mouse support phase instead.
- [x] Hybrid mode should only de-render the line the cursor is currently on. The current behavior is to de-render the whole element. **Fixed 2026-04-16.** `RenderedView` now replaces only the single rendered line that corresponds to the cursor's position within the block, leaving all other lines of the block fully rendered.
- [x] When returning to preview mode from edit mode, if the cursor has been moved, again entering edit mode will move the cursor to the top of the first element on the page. The cursor should remain in the same place it was in edit mode. **Fixed 2026-04-16.** `sync_cursor_to_scroll` now checks if the cursor is already within the visible viewport before moving it; if it is, the cursor position is preserved.
- [x] Holding Ctrl and pressing backspace should delete the word preceding the cursor, up to the next non-alphanumeric character. This already works in the other direction with the delete key. **Fixed 2026-04-14.** The `ctrl+backspace` → `DeleteWordBack` binding already existed. Added a `DefaultHandler` fallback that maps `KeyCode::Char('\x08')` (raw BS sent by some terminals) to `DeleteWordBack`. **Re-fixed 2026-04-15.** Added a second fallback for terminals (e.g. urxvt, Alacritty) that send Ctrl+Backspace as `KeyCode::Char('\x7f')` with `KeyModifiers::CONTROL`. **Re-fixed 2026-04-16.** Consolidated all known encodings into a single `is_ctrl_backspace` predicate: accepts `Backspace + CONTROL`, raw `\x08` with or without modifiers, `\x7f + CONTROL`, and `h`/`H` + CONTROL (the ASCII equivalence Ctrl+H = BS used by some terminals). Modifier checks use `contains(CONTROL)` so combinations like Ctrl+Shift+Backspace also fire.
- [x] The background color of code blocks should be the full width of the editor for all lines in the code block. The current behavior only applies the background color up to column 80 unless the line is longer than 80 characters. **Fixed 2026-04-16.** `render_line_with_cursor` now fills all remaining cells in each visual row with the line's background style after writing the line's content. **Re-fixed 2026-04-16.** Extracted `render_line` / `render_line_with_cursor` into a shared `ui::line_render` module and switched `PreviewView` to use it in place of ratatui's `Paragraph` widget, so the trailing-cell background fill applies in preview mode too. Added `code_block_bg_extends_to_viewport_edge` test covering a 100-column viewport where the renderer's internal `block_width` is still 80.
- [x] A line in a code block that is long enough to wrap does not have the background color applied to the full width of the last line. Current behavior only applies the background to the characters on the last line. **Fixed 2026-04-16.** Same fix as above: the background fill is applied to trailing cells of every visual row, including wrapped rows. **Re-fixed 2026-04-16.** Preview now shares the same `render_line` path that fills trailing cells on every wrap row. Added `wrapped_code_block_bg_fills_last_row` test.
- [x] In a wrapped line, navigating up/down should move the character to the same vertical position within the wrapped line, e.g. the same offset from the left visually, instead of the current behavior which is to move between logical lines. This should be a configuration item (default: cursor moves with visual lines, not logical lines). **Fixed 2026-04-14.** Added `visual_line_nav` to `EditorConfig` (default `true`) and `EditorState`. `edit_ops` calls `move_up_visual`/`move_down_visual` when enabled, computing visual sub-lines from `preferred_col / col_width`. **Re-fixed 2026-04-16.** Rewrote `move_up_visual` / `move_down_visual` to use the same word-aware wrap algorithm as `line_render::render_line` (extracted into `visual_rows_of_str` / `sub_line_of_col` helpers). Crossing a logical-line boundary upward now lands on the LAST visual sub-line of the previous logical line (and downward on the first sub-line of the next). `preferred_col` is treated as the target visual column, synced on horizontal moves via `sync_preferred_visual` in `edit_ops`.
- [x] It seems that a blank line is inserted below all elements. This should not be the case. Extra blank lines can be inserted for spacing by the user if desired. **Fixed 2026-04-16.** Removed trailing `Line::raw("")` from all block renderers. Visual separation between blocks now derives entirely from blank lines present in the source (via `ParsedDoc::build`'s gap analysis). **Re-fixed 2026-04-16.** pulldown-cmark's top-level block ranges absorb a variable number of trailing newlines, so the previous `gap = &source[a.end..b.start]` count under-reported blank lines by one for every pair of adjacent blocks. Fixed by backing up past any trailing `\n` in each block's range (`content_end_of_block`), counting newlines from `content_end` to the next block's start, and subtracting 1 for the natural line break. Added preservation of leading and trailing blank lines around the first/last block. Source-to-rendered mapping of blank lines is now 1:1.
- [x] Undo/redo currently operates on one character at a time, such that if the user types "cat" it would take 3 undos to remove the word. Instead, group adjacent alphanumeric characters (i.e. a word) into a single undo/redo action instead, such that undoing "cat" would be one undo action, assuming all 3 letters were typed in sequence with no other edits elsewhere in the document. This character grouping into words should use the same detection logic we already use for breaking in line wrapping. **Fixed 2026-04-16.** `History::record` now merges an incoming delta into the top of the undo stack via `can_merge_word_group` when (a) both deltas are pure insertions, (b) the new delta is a single alphanumeric char, (c) the top's last inserted char is also alphanumeric, and (d) the new offset is immediately after the top's inserted text. Cursor movement (via `MoveLeft` etc.) naturally breaks the group because the next insert lands at a different offset. Non-alphanumeric characters (spaces, punctuation) and multi-char inserts (tab, newline, paste) start fresh groups.
- [x] In hybrid edit mode, when navigating between lines with the cursor not in the first column, the cursor briefly jumps to first column of the next line (i.e. the first character of the line), then moves to the correct position. For example, if the cursor is on line 3, column 3, then we press the down key to go to line 4, column 3, the cursor briefly jumps to line 4, column 1, before jumping to the correct position at line 4, column 3. The correct behavior should be to move directly to line 4 column 3. **Fixed 2026-04-16.** During the `RAW_REVEAL_DELAY` window `RenderedView` overlaid its cursor indicator at column 0 of the new line regardless of the actual column. Replaced the hardcoded `0` with `cursor_col` in `rendered_view.rs`, so the indicator is drawn at the cursor's real column from the first frame and there is no column-jump when the raw view later reveals.
- [x] It seems like blank lines are being grouped into their preceding elements. The cursor never lands on a blank line, instead it jumps to the preceding non-blank line element and the last line of that element renders that as a blank line (or lines if there are multiple). Each blank line should be considered its own element and the cursor should land on each blank line. However, blank lines do not need to call the render/de-render logic since they will look the same. **Fixed 2026-04-16.** `ParsedDoc::build` now creates a **virtual block** for every blank line in the source (leading, between-block, and trailing). Each virtual block owns exactly the blank line's `\n` byte, has `per_block_own = 1`, and has its own entry in `rendered_to_block`. The cursor's byte-to-block lookup (`block_for_byte`) therefore lands on the blank line's own block rather than the preceding paragraph's extended range, so navigation stops on each blank line as its own element. Because a blank rendered line looks identical to a blank raw line, the reveal/de-render path is a visual no-op on blank lines. Replaces the previous use of `parse_offsets::covering_ranges`, which had absorbed blank-line bytes into the adjacent block.
- [x] Checklist items have an extra 2 space indentation (where the "- " is in the source), which should not be there. **Fixed 2026-04-16.** `render_list` in `markdown/renderer.rs` used `format!("{}  ", indent_str)` as the task-item marker, reserving a 2-column stand-in for where a bullet would go. That produced a permanent 2-space indent at indent 0. The marker is now just `indent_str.clone()`, so the checkbox starts at column 0 on top-level items and nested items retain their proper 2-spaces-per-level indentation.
- [x] When the app first starts, in preview mode, blank lines are not rendered. As soon as the user scrolls, blank lines are rendered correctly and remain rendered correctly, but this should be correct from first document load. **Fixed 2026-04-16.** `App::new` in `app.rs` built the initial preview lines by re-parsing the source and calling `Renderer::render` directly, bypassing `ParsedDoc::build` — which is the only place that applies `preserve_blank_lines`. The first sync of the editor's parsed lines into the preview state happened after the first event, so blank lines appeared as soon as the user scrolled. Now the initial `EditorViewState` is seeded with `editor.parsed.lines.clone()`, so the first frame already has the blank-line-preserving line list.

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
- [ ] Implement `terminal/capabilities.rs` with a `Capabilities` struct: `{ colour_depth: ColourDepth, mouse: bool, image_protocol: Option<ImageProtocol>, unicode_full: bool, keyboard_enhancement: bool }` — where `ImageProtocol` is an enum (`Sixel`, `KittyGraphics`, `ITerm2`, `Halfblocks`); `keyboard_enhancement` indicates whether `PushKeyboardEnhancementFlags` succeeded (required for `Ctrl-Shift-Z` redo)
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
- [ ] In terminals with mouse support, scrolling with the mouse should only move the page, not the cursor. The user can then click to move the cursor if desired. Since the cursor does not move when scrolling, there is no need to scroll line by line and we can use a smooth scroll instead, which is more visually pleasing.
- [ ] Allow scrolling up to one page below the last line with the mouse only, such that the last line remains in view at the top of the editor. Scrolling with the keyboard should not have this effect since the cursor is constrained within the editor window.

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
  - Support both local file paths and HTTP/HTTPS URLs (load URLs with `ureq`, cache to disk in `$XDG_CACHE_HOME/markdown-tui/images/`).
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

### Optimization
- **High CPU Usage**: We should see what optimizing we can do to improve the performance of the app. For one thing, idle CPU usage high on my machine, which I think should be significantly lower when the app is just displaying static output and not being interacted with. Interestingly, CPU usage seems to decrease over time. Memory usage is too low to even be outputted by `ps aux`, so that's fine.

```
markdown-tui main  ? ❯ ps aux|head -1
USER         PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND
markdown-tui main  ? ❯ ps aux|grep markdown
mjw      3192551  7.4  0.0  78956  7724 pts/11   Sl+  13:24   0:50 ./target/debug/markdown-tui example.md
markdown-tui main  ? ❯ ps aux|grep markdown
mjw      3192551  6.7  0.0  78956  7724 pts/11   Sl+  13:24   1:06 ./target/debug/markdown-tui example.md
markdown-tui main  ? ❯ ps aux|grep markdown
mjw      3192551  6.6  0.0  78956  7724 pts/11   Sl+  13:24   1:10 ./target/debug/markdown-tui example.md
markdown-tui main  ? ❯ ps aux|grep markdown
mjw      3192551  6.3  0.0  78956  7724 pts/11   Sl+  13:24   1:38 ./target/debug/markdown-tui example.md
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
   ~/.config/markdown-tui/
   ├── config.toml          # [editor] and [modal] only
   ├── keybindings.toml     # [keybindings]
   └── themes/
       ├── default.toml     # written out on first run if missing
       ├── catppuccin.toml
       └── gruvbox.toml
   ```

   `Config::load()` reads `config.toml` for general settings, then `keybindings.toml` for keybinds, then loads `themes/<active_theme>.toml` (defaulting to `themes/default.toml`). The `ThemeConfig` struct should be populated now, before any user config files exist in the wild.

   **Decision**: yes, extract themes into a `themes/` directory. Implement during the theming/config phase. Keybindings get moved to `keybindings.toml`.
