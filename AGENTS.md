# AGENTS.md — edamame

Guidance for agentic coding agents working in this repository.

## Project Overview

`edamame` is a Rust TUI application for viewing and editing Markdown files
in the terminal. It uses `ratatui` for rendering, `pulldown-cmark` for parsing,
and `ropey` for rope-based text editing. The crate is both a binary
(`edamame`) and a library (so integration tests can import it).

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

**Do not** write tests for: terminal capability detection that depends on
live terminal probing (`Picker::from_query_stdio`), cross-platform clipboard
(OS clipboard paths race between parallel tests), or the actual crossterm
terminal-mouse wire protocol — these are covered by manual smoke testing.
*Do* test mouse logic at the `MouseDispatcher` + `mouse_ops::apply` layer:
both are pure functions of an input event and an editor state.

## Project Structure

```
src/
  main.rs           # CLI args, config load, terminal init, App::run
  lib.rs            # re-exports all modules (enables integration tests)
  app.rs            # App: event loop, mpsc channel, action dispatch
  config.rs         # facade — re-exports Config, KeyMap, Theme, ThemeFile
  config/
    config.rs       # Config, EditorConfig, ModalConfig, TableConfig, ImagesConfig,
                    #   LoadedConfig (serde+toml); load()/save()/ensure_default_files()
    keymap.rs       # Action enum, KeyMap, KeyBindingOverrides, parse_key()
    theme.rs        # Theme: all Style values; no hardcoded colors elsewhere.
                    #   Theme::from_file(ThemeFile, monochrome) builds from TOML
    theme_file.rs   # ThemeFile, StyleSpec, ColorField: user-authorable TOML
                    #   theme format — converts to/from Theme via From impls
  document.rs       # facade — re-exports Buffer, Cursor, EditDelta, History,
                    #           ParsedDoc, Selection, SourceMap
  document/
    buffer.rs       # Buffer wrapping ropey::Rope; file I/O + edit primitives
    cursor.rs       # Cursor: rope char offset + preferred visual column
    history.rs      # History: undo/redo stack of EditDelta values; merges
                    #          adjacent alphanumeric inserts into word-groups
    parsed_doc.rs   # ParsedDoc: re-parses on change, caches AST + source map;
                    #          synthesises a virtual block per blank line so the
                    #          cursor lands on each blank line independently
    selection.rs    # Selection: anchor + active rope offsets
    source_map.rs   # SourceMap: block byte-range ↔ rendered-line-index mapping
  editor.rs         # facade — re-exports EditorState, Mode
  editor/
    edit_ops.rs     # Action → EditorState mutations (cursor, buffer, history)
    list_edit.rs    # List detection, continuation, checkbox toggle, renumber
    mode.rs         # Mode enum: Preview | Rendered | Raw
    mouse_ops.rs    # MouseAction → EditorState mutations (click, drag, scroll,
                    #          selection, checkbox toggle, link hit-test)
    state.rs        # EditorState: owns Buffer, Cursor, History, Mode, ParsedDoc
    table_edit.rs   # Table detection, row/col structure edits
  input.rs          # facade — re-exports InputDispatcher, ModalHandler,
                    #          MouseAction, MouseDispatcher
  input/
    dispatcher.rs   # InputDispatcher: crossterm Event → Action
    modal.rs        # facade — declares `default` submodule; ModalHandler trait
    modal/
      default.rs    # DefaultHandler: non-modal keybinding implementation
    mouse.rs        # MouseDispatcher: click-count + drag state machine that
                    #          converts crossterm MouseEvents into MouseActions
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
    line_render.rs  # render_line / render_line_with_cursor: word-aware wrap,
                    #          trailing-cell background fill; shared by Preview
                    #          and Rendered views — update once, both views update
    preview.rs      # PreviewView + PreviewState (StatefulWidget)
    raw_view.rs     # RawView + RawViewState: plain-text editor from rope buffer
    rendered_view.rs # RenderedView + RenderedViewState: hybrid rendered+raw-line view
    status_bar.rs   # StatusBar + StatusBarState
tests/
  editing.rs        # integration tests: EditorState action sequences → buffer/cursor asserts
  list_edit.rs      # integration tests: list continuation, renumber, checkbox toggle
  mouse.rs          # integration tests: mouse click / drag / scroll / checkbox
  renderer.rs       # integration tests: parse + render → assert/snapshot
  source_map.rs     # unit + proptest tests for SourceMap invariants
                    #   (proptest regressions saved in source_map.proptest-regressions)
  table.rs          # integration tests: table navigation + structure edits
  ui.rs             # integration tests: TestBackend widget rendering
  snapshots/        # committed insta .snap files
config/
  config.toml              # annotated reference config (editor/modal/images +
                           #   the active theme name).  Written to
                           #   ~/.config/edamame/config.toml on first run.
  keybindings.toml         # commented-out keybinding overrides reference.
                           #   Written to ~/.config/edamame/keybindings.toml.
```

`themes/default.toml` is **not** checked in: it is generated at first run
by `theme_file::default_theme_toml()` from `Theme::default()` /
`Palette::default()`, with every palette value commented out so the live
default tracks the code automatically.  Edit `Palette::default()` /
`Theme::from_palette()` in `src/config/theme.rs` and the next first-run
write picks up the new defaults.

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

## Phase 1 Architectural Notes (Hybrid Editing)

These decisions emerged during Phase 1 and are easy to break if you don't know
they exist:

- **Virtual blocks for blank lines**: `ParsedDoc::build` synthesises a one-byte
  block for every blank line in the source (leading, between-block, and
  trailing). The cursor lands on each blank line as its own block, and the
  blank line is preserved in `RenderedView` even when the surrounding cursor
  block is replaced with raw text. Don't reintroduce `parse_offsets::covering_ranges`
  for the cursor mapping — it absorbs blank-line bytes into adjacent blocks
  and breaks navigation.
- **`per_block_own` vs. extended ranges**: `ParsedDoc` tracks both per-block
  *own* rendered line counts (for the raw-replacement region in `RenderedView`)
  and *extended* covering ranges (for cursor lookup). Mixing them up causes
  gap blank lines to be collapsed when the cursor enters the previous block.
- **Jitter-suppression reveal**: `EditorState::cursor_block_revealed()` returns
  false during a 120 ms `RAW_REVEAL_DELAY` after the cursor enters a new
  *buffer line* (not block). `RenderedView` keeps the block fully rendered and
  draws an inverted-cell cursor indicator at `(cursor_col, cursor_row)` until
  the delay elapses. The App loop uses `rx.recv_timeout(60 ms)` so the redraw
  fires without a keypress.
- **Single shared `line_render` module**: `PreviewView` and `RenderedView` both
  call into `ui::line_render`. The trailing-cell background fill and word-aware
  wrap live there. If you change one view's wrap or fill behaviour, change the
  shared function — don't fork it.
- **NBSP padding in code blocks**: blank lines inside fenced code blocks use
  U+00A0 (NBSP) padding, not regular spaces. This works around a ratatui
  `WordWrapper` (`trim: false`) bug where an all-whitespace line produces an
  extra empty visual row. Don't "simplify" this back to spaces.
- **Word-group undo merging**: `History::record` merges single alphanumeric
  inserts into the previous delta when offsets are contiguous. Cursor moves
  break the group naturally (next insert lands at a different offset). Don't
  mistake this for snapshot-based history — it's still delta-based.
- **Visual line navigation**: `move_up_visual` / `move_down_visual` and
  `line_render::render_line` must use the same wrap algorithm
  (`visual_rows_of_str` / `sub_line_of_col`). Otherwise the cursor lands in a
  different column than where it appears on screen.
- **Action enum is fully defined upfront**: every action across phases lives in
  `config/keymap.rs::Action` from Phase 0. Later-phase actions are no-ops in
  `edit_ops` until their phase implements them; keybindings stay stable.
- **Clipboard is feature-gated**: `arboard` is behind the `clipboard` Cargo
  feature (on by default). When disabled, copy/cut/paste use the in-process
  kill-ring only. Tests assert against the kill-ring, not the OS clipboard,
  to avoid cross-test races.

## Phase 5 Architectural Notes (Mouse Support)

- **Two-layer mouse dispatch**: `MouseDispatcher` (in `src/input/mouse.rs`) is
  a pure state machine that turns crossterm `MouseEvent`s into semantic
  `MouseAction`s (click-count, drag, scroll).  `mouse_ops::apply` (in
  `src/editor/mouse_ops.rs`) is where those actions mutate `EditorState`.
  Keep the split strict — coordinate translation belongs in `mouse_ops`, click
  counting belongs in `MouseDispatcher`.
- **Mouse enable is gated by capabilities**: `terminal::enable_mouse()` is
  only called from `main` when `capabilities.mouse` is true.  The app also
  gates `MouseDispatcher::dispatch` on `capabilities.mouse` so a fake mouse
  event (e.g. injected via a test hook) can't drive the editor on a terminal
  where mouse wasn't enabled.
- **Drag anchor lives in `App`, not `EditorState`**: the `drag_anchor:
  Option<usize>` on `App` persists the mouse-down offset across events so the
  Drag handler can extend the selection.  It's intentionally not in
  `EditorState` — it's a UI-layer fact, not a document-layer fact, and
  clearing it doesn't need to go through the undo stack.
- **Mouse scroll uses a different bound than keyboard scroll**:
  `mouse_ops::scroll_by_mouse` allows `max = total - 1` (last line at top of
  viewport) and never invokes `clamp_cursor_to_viewport_top`.  Keyboard scroll
  (`Action::ScrollDown`) still uses `EditorState::scroll_down` which keeps the
  cursor visible.  Do not merge the two paths — the Phase 5 requirement is
  that mouse scroll specifically does not move the cursor.
- **Click-to-offset is approximate for formatted text**: rendered inline
  styling (`**bold**` → `bold`) shifts char positions between raw and
  rendered.  `rendered_sub_line_to_offset` maps the visual column 1:1 to the
  raw source column, which is exact for unformatted lines and off by a few
  chars for styled spans.  The `RAW_REVEAL_DELAY` then turns the cursor's
  line raw so the user can correct on a second click.  If you change the
  renderer to collapse / expand more characters (e.g. rendering `~~strike~~`
  as `strike`), expect click precision to drift in a corresponding way and
  consider whether a proper char-map table is worth the complexity.
- **Link hit-test is a source-scan shortcut**: `mouse_ops::link_at_offset`
  scans the line's raw bytes for balanced `[...](...)` — it is NOT driven by
  the AST.  Good enough for Phase 8 prerequisite (we only need to know
  *whether* the click was on a link); upgrade to an AST-backed registry if
  Phase 8 needs reference-style links or autolinks.
- **Checkbox toggling short-circuits cursor placement**: `toggle_checkbox_at`
  runs BEFORE `click_to_char_offset` in the `MouseAction::Click` arm.  A click
  on the `[ ]` glyph toggles and returns immediately — the cursor does NOT
  move.  Clicks elsewhere on the task line fall through to normal placement.

## Phase 10 Architectural Notes (Command Palette + Overlays)

- **One `KeyMap`, mutated in place.** `App::keymap: Option<KeyMap>` is built
  once in `run()` and held for the life of the process.  The keybinds overlay
  calls `KeyMap::rebind(&action, key_str, &mut overrides)` directly on it so
  rebinds take effect on the next keystroke without rebuilding.  Don't clone
  the keymap into the overlay state — that breaks live propagation.
- **Combined view+edit keybindings overlay.** `Action::ShowCheatSheet` is now
  an alias that opens the same `KeybindsView` as `Action::OpenKeybinds`; the
  Phase 9 read-only `?` cheat sheet and the standalone `cheat_sheet.rs` are
  gone.  Action variants are kept for backwards-compat with user keybindings.
- **`ModalView` is scrollable; Phase 10 overlays are not.** `ModalState`
  carries `scroll`, `last_total`, `last_visible`, plus `scroll_by(i32)`.  Up /
  Down / PgUp / PgDn / Home / End route to scroll, never to button focus —
  Left / Right and Tab / Shift-Tab still cycle buttons.  Mouse-wheel events
  are forwarded into open `ModalView` slots via `modal_wheel_delta` in the
  run loop; the palette / settings / keybinds overlays don't scroll yet
  because their bodies fit comfortably.
- **External-editor flow needs three things.** When the settings overlay's
  "Open config.toml in default editor" fires, the App must (1) pause its
  crossterm read thread, (2) drain the rx channel, and (3) suspend the
  terminal — in that order — before `Command::new($EDITOR).status()`.  Skip
  any of these and the editor races our read thread for stdin: bytes get
  split, keystrokes feel laggy, and OSC responses to startup-time queries
  leak into the buffer (the original symptom was `1;rgb:0e0e/0909/1d1d` —
  a partially-consumed background-colour reply — landing at the top of
  `config.toml`).  The read thread is poll-based (`crossterm::event::poll(100ms)`)
  precisely so a `read_paused: Arc<AtomicBool>` flag can stop it without
  having to interrupt a blocked `read()` syscall.  After the editor exits,
  `terminal::re_enter(mouse, keyboard_enhancement)` reinstates alt-screen +
  raw mode + transient features (mirroring `setup` minus the `Terminal`
  construction), and `Config::load()` is re-run so any edits the user made
  take effect immediately.
- **Preview-mode Ctrl-key allowlist.** `input::modal::default::preview_safe_action`
  decides which Ctrl-* chords fire in Preview mode.  Read-only overlay
  openers (`ShowCommandPalette`, `OpenSettings`, `OpenKeybinds`,
  `OpenConfigFolder`, `ShowMarkdownCheatSheet`, `ShowCheatSheet`) belong on
  the allowlist — adding a new modal-opening action means adding it here too,
  otherwise Ctrl-P / similar will silently no-op in Preview.

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
when `[dev] logging = true` in config, so tracing calls in production are no-ops.

### Tests

- Every source file gets a `#[cfg(test)] mod tests { ... }` block for unit tests
- Integration tests in `tests/` import from the library crate (`edamame::`)
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
