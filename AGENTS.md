# AGENTS.md — markdown-tui

Guidance for agentic coding agents working in this repository.

## Project Overview

`markdown-tui` is a Rust TUI application for viewing and editing Markdown files
in the terminal. It uses `ratatui` for rendering, `pulldown-cmark` for parsing,
and `ropey` for rope-based text editing. The crate is both a binary
(`markdown-tui`) and a library (so integration tests can import it).

## Build Commands

```bash
cargo build              # dev build
cargo build --release    # optimized build
cargo run -- path/to/file.md  # run the application
cargo check              # fast type/borrow check (no codegen)
```

## Lint and Format Commands

```bash
cargo fmt                # format all code with rustfmt
cargo fmt -- --check     # check formatting without modifying files (CI)
cargo clippy             # run the Clippy linter
cargo clippy -- -D warnings   # treat warnings as errors (CI enforcement)
```

No custom `rustfmt.toml` or `.clippy.toml` exists; standard Rust defaults apply.

## Test Commands

```bash
cargo test                              # run all unit + integration tests
cargo test -- --list                    # list all test names

# Run a single test by exact name:
cargo test -- document::buffer::tests::insert_char --exact

# Run tests matching a name substring:
cargo test parse_heading

# Run unit tests only (inline #[cfg(test)] blocks):
cargo test --lib

# Run a specific integration test file:
cargo test --test renderer
cargo test --test ui
cargo test --test editing
cargo test --test source_map

# Review / accept updated insta snapshots:
cargo insta review
```

**Test frameworks in use:**
- `insta` — snapshot testing (`assert_debug_snapshot!`, `assert_snapshot!`)
- `proptest` — property-based testing (declared; used in later phases)
- `tempfile` — temporary files for I/O tests
- `ratatui::backend::TestBackend` — headless widget rendering

**Do not** write tests for: terminal capability detection, cross-platform
clipboard, or mouse integration — these are covered by manual smoke testing.

## Project Structure

```
src/
  main.rs           # CLI args, config load, terminal init, App::run
  lib.rs            # re-exports all modules (enables integration tests)
  app.rs            # App: event loop, mpsc channel, action dispatch
  config.rs         # facade — re-exports Config, KeyMap, Theme
  config/
    config.rs       # Config, EditorConfig, ThemeConfig, ModalConfig (serde+toml)
    keymap.rs       # Action enum, KeyMap, parse_key()
    theme.rs        # Theme: all Style values; no hardcoded colors elsewhere
  document.rs       # facade — re-exports Buffer, Cursor, EditDelta, History,
                    #           ParsedDoc, Selection, SourceMap
  document/
    buffer.rs       # Buffer wrapping ropey::Rope; file I/O + edit primitives
    cursor.rs       # Cursor: rope char offset + preferred visual column
    history.rs      # History: undo/redo stack of EditDelta values
    parsed_doc.rs   # ParsedDoc: re-parses on change, caches AST + source map
    selection.rs    # Selection: anchor + active rope offsets
    source_map.rs   # SourceMap: block byte-range ↔ rendered-line-index mapping
  editor.rs         # facade — re-exports EditorState, Mode
  editor/
    edit_ops.rs     # Action → EditorState mutations (cursor, buffer, history)
    mode.rs         # Mode enum: Preview | Rendered | Raw
    state.rs        # EditorState: owns Buffer, Cursor, History, Mode, ParsedDoc
  input/            # NOTE: uses mod.rs layout (not the facade pattern) — to migrate
    mod.rs          # re-exports InputDispatcher, ModalHandler
    dispatcher.rs   # InputDispatcher: crossterm Event → Action
    modal/
      mod.rs        # ModalHandler trait
      default.rs    # DefaultHandler: non-modal keybinding implementation
  markdown.rs       # facade — re-exports parse, Renderer
  markdown/
    ast.rs          # Block, Inline, ListItem enums; inlines_to_plain()
    parse_offsets.rs # Collect (byte_start, byte_end) spans from pulldown-cmark
    parser.rs       # pulldown-cmark → Vec<Block>
    renderer.rs     # Renderer: Vec<Block> → Vec<Line<'static>>
  terminal.rs       # facade — re-exports Capabilities, setup, restore
  terminal/
    capabilities.rs # Capabilities struct
    setup.rs        # setup() / restore() terminal functions
  ui.rs             # facade — re-exports all widgets and their state types
  ui/
    editor_view.rs  # EditorView + EditorViewState (StatefulWidget); dispatches to sub-views
    preview.rs      # PreviewView + PreviewState (StatefulWidget)
    raw_view.rs     # RawView + RawViewState: plain-text editor from rope buffer
    rendered_view.rs # RenderedView + RenderedViewState: hybrid rendered+raw-line view
    status_bar.rs   # StatusBar + StatusBarState
tests/
  editing.rs        # integration tests: EditorState action sequences → buffer/cursor asserts
  renderer.rs       # integration tests: parse + render → assert/snapshot
  source_map.rs     # unit + proptest tests for SourceMap invariants
  ui.rs             # integration tests: TestBackend widget rendering
  snapshots/        # committed insta .snap files
config/
  default_config.toml  # annotated reference config
```

**Architectural layers** (higher depends only on lower):
1. `main` / `App` — event loop, terminal lifecycle
2. `ui` — ratatui widgets; `EditorView` dispatches to `PreviewView`, `RenderedView`, `RawView`
3. `input` — `InputDispatcher`; `ModalHandler` trait + `DefaultHandler` implementation
4. `editor` — `EditorState`; owns `Buffer`, `Cursor`, `History`, `Mode`, `ParsedDoc`
5. `config` — `Config`, `KeyMap`, `Theme` (loaded once at startup)
6. `document` — `Buffer`, `Cursor`, `History`, `ParsedDoc`, `Selection`, `SourceMap`
7. `markdown` — parser → AST → renderer pipeline; `parse_offsets` feeds `SourceMap`
8. `terminal` — raw terminal setup/teardown

## Module Facade Pattern

Every top-level module has both a file (`src/config.rs`) and a subdirectory
(`src/config/`). The file declares submodules with `pub mod` and re-exports
public types with `pub use`. This keeps call-site imports clean:

```rust
use crate::config::{Config, KeyMap, Theme};   // not crate::config::config::Config
```

Always follow this pattern when adding new top-level modules.

**Known exception**: `src/input/` currently uses the old `mod.rs` layout
(`src/input/mod.rs` and `src/input/modal/mod.rs`) instead of the facade pattern.
It should be migrated to `src/input.rs` + `src/input/modal.rs` when convenient,
but do not break the existing structure without also updating all import paths.

## Code Style

Always write idiomatic, modern Rust. Prefer language and standard-library features over manual workarounds. Specific rules:

- Prefer `#[derive(Default)]` over a hand-written `impl Default` when the derived output would be identical.
- Do not call `.into_iter()` on a value that already implements `Iterator`; call methods like `.peekable()` directly.
- Use the modern module layout: `src/foo.rs` alongside `src/foo/` for submodules — not `src/foo/mod.rs`.
- Prefer library helper methods (`saturating_sub`, `unwrap_or_else`, etc.) over manual equivalents.

### Imports

Group imports in this order, separated by blank lines:
1. `std` library
2. Third-party crates (alphabetical within the group)
3. `crate::` or `super::` local imports

```rust
use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::Result;
use crossterm::event::{Event, KeyEventKind};
use ratatui::Terminal;

use crate::config::{Action, Config, KeyMap, Theme};
use crate::document::Buffer;
```

### Naming Conventions

| Element | Convention | Example |
|---|---|---|
| Files / modules | `snake_case` | `editor_view.rs` |
| Functions / methods | `snake_case` | `render_block`, `scroll_down` |
| Variables | `snake_case` | `file_path`, `col_count` |
| Types / structs / enums | `PascalCase` | `Buffer`, `EditorView` |
| Enum variants | `PascalCase` | `ScrollPageUp`, `CodeBlock` |
| Traits | `PascalCase` | `ModalHandler` |
| Constructors | `new()` / `load()` / `build()` | `Config::load()`, `KeyMap::build()` |
| Boolean predicates | standard Rust | `is_empty()`, `is_some()` |

### Formatting

- 4-space indentation
- Trailing commas in struct/enum literals and match arms
- Target ~100 chars line length
- Use decorative section comments to group methods:
  ```rust
  // ── Query ─────────────────────────────────────────────────────────────────

  // ── Edit ──────────────────────────────────────────────────────────────────
  ```

### Types

- Prefer `enum` for domain state (`Mode`, `Action`, `Block`, `Inline`)
- Use `#[derive(Debug, Clone, PartialEq, Eq, Hash)]` on core types
- Use `Option<T>` instead of nullable patterns
- Lifetimes: use minimally and deliberately (e.g. `Renderer<'t>` borrows `Theme`)
- Newtype pattern for validated wrappers with `#[serde(transparent)]`
- `StatefulWidget` for widgets that mutate scroll/cursor state; `Widget` for
  purely functional rendering (no mutable state needed)

### Error Handling

Two-tier strategy:

**Application code — use `anyhow`:**
```rust
use anyhow::{Context, Result};

fn load(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))
}

// Non-fatal startup fallbacks:
let config = Config::load().unwrap_or_else(|e| {
    eprintln!("Warning: {e}. Using defaults.");
    Config::default()
});
```

**Library/domain errors — use `thiserror`:**
```rust
#[derive(Debug, thiserror::Error)]
pub enum KeyMapError {
    #[error("unknown action name: '{0}'")]
    UnknownAction(String),
    #[error("unparseable key string: '{0}'")]
    UnparseableKey(String),
}
```

**Panic prevention:**
- Use `.saturating_sub()` instead of `-` on `usize` to prevent underflow
- Use `.min()` / `.max()` to clamp values before indexing
- Bounds-check before any rope or buffer operation

### Logging

Use `tracing` macros (`tracing::info!`, `tracing::debug!`, etc.) — **never**
`println!` or `eprintln!` (would corrupt the TUI). Logging is only initialised
when `dev_mode = true` in config, so tracing calls in production are no-ops.

### Tests

- Every source file gets a `#[cfg(test)] mod tests { ... }` block for unit tests
- Integration tests in `tests/` import from the library crate (`markdown_tui::`)
- Use `insta::assert_debug_snapshot!` for complex output (ASTs, rendered lines)
- Use `ratatui::backend::TestBackend` for widget rendering tests
- Use `Box::leak(Box::new(Theme::default()))` to produce `&'static Theme` in
  tests without lifetime annotations — this is intentional and safe in tests
- Commit updated `.snap` files alongside the code changes that caused them

## Key Dependencies

| Crate | Purpose |
|---|---|
| `ratatui` | TUI framework (widgets, layout, rendering) |
| `crossterm` | Cross-platform terminal I/O and events |
| `pulldown-cmark` | CommonMark Markdown parser |
| `ropey` | Rope data structure for efficient text editing |
| `anyhow` | Application-level error handling |
| `thiserror` | Typed library-level error enums |
| `serde` + `toml` | Config file deserialization |
| `tracing` | Structured logging (dev mode only) |
| `insta` | Snapshot testing |
| `proptest` | Property-based testing |
