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
  document.rs       # facade — re-exports Buffer
  document/
    buffer.rs       # Buffer wrapping ropey::Rope; file I/O + edit primitives
  editor.rs         # facade — re-exports Mode
  editor/
    mode.rs         # Mode enum: Preview | Rendered | Raw
  markdown.rs       # facade — re-exports parse, Renderer
  markdown/
    ast.rs          # Block, Inline, ListItem enums; inlines_to_plain()
    parser.rs       # pulldown-cmark → Vec<Block>
    renderer.rs     # Renderer: Vec<Block> → Vec<Line<'static>>
  terminal.rs       # facade — re-exports Capabilities, setup, restore
  terminal/
    capabilities.rs # Capabilities struct
    setup.rs        # setup() / restore() terminal functions
  ui.rs             # facade — re-exports all widgets
  ui/
    editor_view.rs  # EditorView + EditorViewState (StatefulWidget)
    preview.rs      # PreviewView + PreviewState (StatefulWidget)
    status_bar.rs   # StatusBar widget (Widget — no mutable state)
tests/
  renderer.rs       # integration tests: parse + render → assert/snapshot
  ui.rs             # integration tests: TestBackend widget rendering
  snapshots/        # committed insta .snap files
config/
  default_config.toml  # annotated reference config
```

**Architectural layers** (higher depends only on lower):
1. `main` / `App` — event loop, terminal lifecycle
2. `ui` — stateless ratatui widgets
3. `editor` — `Mode` enum; editing state
4. `config` — `Config`, `KeyMap`, `Theme` (loaded once at startup)
5. `document` — `Buffer` (ropey rope)
6. `markdown` — parser → ast → renderer pipeline
7. `terminal` — raw terminal setup/teardown

## Module Facade Pattern

Every top-level module has both a file (`src/config.rs`) and a subdirectory
(`src/config/`). The file declares submodules with `pub mod` and re-exports
public types with `pub use`. This keeps call-site imports clean:

```rust
use crate::config::{Config, KeyMap, Theme};   // not crate::config::config::Config
```

Always follow this pattern when adding new top-level modules.

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
