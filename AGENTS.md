# AGENTS.md — edamame

Guidance for human and agentic contributors working in this repository. `CLAUDE.md` is a symlink to this file; edit one, update both.

## Project Overview

`edamame` is a Rust TUI application for viewing and editing Markdown files in the terminal: `ratatui` for rendering, `pulldown-cmark` for parsing, `ropey` for rope-based text editing. The crate ships as both a binary (`edamame`) and a library (so integration tests can import it).

> **Security:** edamame opens untrusted documents, so any change to a content-handling path (image/SVG decode, remote fetch, Mermaid, link opening, HTML export, subprocess spawning) must preserve the hardening in [`docs/security.md`](docs/security.md). Read it — and the checklist in [`docs/dev/security-invariants.md`](docs/dev/security-invariants.md) — before touching those areas.

## Build Commands

```bash
cargo build              # dev build
cargo build --release    # optimized build
cargo run -- path/to/file.md
cargo check              # fast type/borrow check (no codegen)
```

## Lint and Format Commands

```bash
cargo fmt                # format all code
cargo fmt -- --check     # check only (CI)
cargo clippy
cargo clippy --all-targets -- -D warnings   # CI enforcement (lib + bin + tests + examples)
```

No custom `rustfmt.toml` or `.clippy.toml`; standard Rust defaults apply.

### Cross-checking the Windows build

Windows is a best-effort platform (`docs/dev/windows.md`): it must compile, lint, and pass tests, and nothing more is verified. From Linux, the msvc target can be type-checked and linted but not run:

```bash
rustup target add x86_64-pc-windows-msvc && cargo install cargo-xwin   # once
cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets --all-features -- -D warnings
cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets --no-default-features -- -D warnings
```

`cargo-xwin` rather than plain `cargo clippy --target` because `ring` (via `ureq` → rustls) compiles C and needs the MSVC CRT headers; `xwin` downloads them into `~/.cache/cargo-xwin` on first use and drives `clang-cl` / `llvm-lib`, which Debian's `clang` package provides. The `--all-features` pass is the one that matters: it is the only place `arboard`'s Windows clipboard backend gets compiled, since the CI test jobs run `--no-default-features`. Run both before touching a `cfg(unix)` gate or a test module whose imports serve only `cfg(unix)` tests — an import that goes unused once those are compiled out is a hard error here.

## Git Hooks

```bash
git config core.hooksPath .githooks   # once per clone — the hooks are inert without it
```

`.githooks/pre-push` refuses to push a commit whose message carries an AI co-author trailer (`Co-Authored-By: Claude …`, `Claude-Session:`, `noreply@anthropic.com`). GitHub builds its contributor list from those trailers as well as the author field, and removing one after the fact means rewriting history and force-pushing, which invalidates every touched commit's signature. The `Trailers` workflow runs the same scan on pull requests, but cannot cover a direct push to `main` — hence the hook. If you use Claude Code, also set `includeCoAuthoredBy: false` in its settings.

## Doc Commands

```bash
cargo doc --no-deps --document-private-items [--open]
```

**Always pass `--document-private-items`.** Doc comments here are written for contributors, not downstream API consumers, and routinely link a private helper next to the public entry point that calls it — the *why* usually lives in the private half. Without the flag those links point at pages rustdoc never generated. `lib.rs` carries `#![allow(rustdoc::private_intra_doc_links)]` for the same reason.

`broken_intra_doc_links` and `invalid_html_tags` are left at their default `warn` and the tree is clean under both — treat a new warning as a real finding. A literal `<name>` in prose parses as an HTML tag; backtick it.

## Test Commands

```bash
cargo test --no-fail-fast               # all unit + integration tests
cargo test -- --list                    # list all test names
cargo test -- document::buffer::tests::insert_char --exact   # one test by exact name
cargo test parse_heading                # by name substring
cargo test --lib                        # unit tests only (covers every module,
                                        #   incl. `app`; main.rs declares none)
cargo test --test renderer              # one integration file (ui, editing,
                                        #   source_map, palette, diagrams, …)
cargo test --test watcher -- --ignored  # the watcher tests that need live
cargo test --lib -- --ignored watcher:: #   inotify / FSEvents (see below)
cargo insta review                      # review / accept updated snapshots
```

**Use `--no-fail-fast`.** Plain `cargo test` stops at the first failing *target*, so a lib-target failure aborts the run before `tests/` is built — every integration test silently goes unrun while the summary still looks real. `cargo nextest run` also runs every target regardless of failures and is faster where installed.

**A test that waits on an OS-delivered event is `#[ignore]`d, not expected to fail.** Four watcher tests — two in `watcher::file_watcher`, two in `tests/watcher.rs` — assert on a live inotify / FSEvents notification, which a sandbox typically withholds; Nix's Darwin builders deliver none at all. They carry `#[ignore = "requires live filesystem notifications (inotify/FSEvents)"]`, so a bare `cargo test` is both green *and* honest wherever the stream is missing, and a downstream packager needs no hand-maintained skip list (nixpkgs kept one, and it silently missed `rewatching_a_different_file_redirects_events` — the name does not contain `watcher` — which is how the gap reached a Darwin build failure). The remaining watcher coverage is unconditional because none of it needs the stream: the debouncer is pure, `force_reconcile` drives the read synchronously, and the post-`unwatch` assertion is a negative one. CI spends the ignored half on the Linux and macOS runners, which do deliver — run the same two commands locally before touching the watcher. Keep that step *filtered*: the crate's other `#[ignore]`d tests want system fonts and a live mermaid renderer, and a bare `--ignored` would drag them in.

**Test frameworks:** `insta` (snapshots), `proptest` (`tests/source_map.rs`), `tempfile`, `ratatui::backend::TestBackend` (headless widget rendering).

**Environment-touching tests take `crate::test_env::env_lock()` — readers included — and mutate only through `test_env::EnvGuard`.** `std::env::set_var` is `unsafe` because it races any concurrent `env::var`, and cargo runs a binary's tests on parallel threads, so the exclusion must be *crate-wide*: `config::config` writes `XDG_CONFIG_HOME` while `cli::doctor` reads it; `terminal::capabilities` writes `TERM_PROGRAM` while `cli::doctor` reads that. A per-module lock cannot exclude either pair. `EnvGuard` restores on drop so a failing assertion can't leak a variable pointing at a deleted tempdir. **`config::persistence::SuppressGuard` is serialized by that same lock** and takes none of its own — hold `env_lock` for the whole test body, not just across the guard: these tests are shaped "assert nothing was written, drop the guard, assert the same call *does* write", and a separate mutex released with the guard would let another test's suppression straddle that second half.

**A test that can reach `Config::save` takes `crate::test_env::config_isolation()`.** Nothing redirects `~/.config/edamame` during a test run, so an unguarded save rewrites *the developer's own config file* — and the asserted values are exactly the damaging ones (an update-check test recording `update_notified_for = "v999.0.0"` silences the real update notice permanently). The helper bundles `env_lock` with a `persistence::SuppressGuard` so `Config::save` returns `Ok(())` without writing. Hold it for the whole test body and take it only once — it is the crate-wide mutex. The one test needing a *real* write (`save_writes_nothing_while_config_writes_are_suppressed`) takes `env_lock` plus an `EnvGuard` pointing `XDG_CONFIG_HOME` at a tempdir. The check: `cargo test` creates no file under the config directory.

**Do not** write tests for live terminal capability probing (`Picker::from_query_stdio`), cross-platform clipboard (OS clipboard paths race between parallel tests), or the crossterm mouse wire protocol — all covered by manual smoke testing. *Do* test mouse logic at the `MouseDispatcher` + `mouse_ops::apply` layer: both are pure functions of an input event and an editor state.

## Project Structure

```
src/
  main.rs           # CLI dispatch, config load, terminal init, App::run.  Declares NO
                    #   modules — it `use`s the library crate; re-declaring the tree
                    #   would compile a second private copy, hiding its unit tests
                    #   from `cargo test --lib`.
  lib.rs            # declares all modules, `app` included

  app.rs / app/     # event loop, modals, timers.  actions.rs (Action → App side
                    #   effects), autosave.rs, diff_advance.rs, difftool.rs (`--diff`
                    #   helpers — in the library, not `main`, so they are testable),
                    #   event_loop.rs, file_changed.rs (watcher event → diff review /
                    #   dirty-conflict modal / reconcile), external_editor.rs,
                    #   flash.rs (TransientMessage), frame_timer.rs,
                    #   image_dispatch.rs, nav.rs (back / forward history), pointer.rs
    update_check.rs / update_check/  # fetch.rs (the only network half: one worker,
                    #   one GET), parse.rs (tag_name + release body; bounds the
                    #   notes), policy.rs (pure: is a check due?), status.rs
                    #   (ReleaseInfo / ReleaseStatus; version comparison)
    update_notice.rs  # spawn / route / deferred-push orchestration
    post_upgrade.rs / post_upgrade/  # the one-time post-upgrade notice:
                    #   post_upgrade_action (pure policy), the last_version_seen
                    #   stamp, changelog.rs (the `## [x.y.z]` section out of the
                    #   `include_str!`d CHANGELOG.md).  No network; shares only
                    #   update_check's note bounding and renderer
    modal.rs / modal/ # stack.rs (ModalStack, top-of-stack dispatch), types.rs (Modal
                    #   trait, ModalKind, ModalOutcome, ModalRenderCtx),
                    #   docs_link.rs (the shared "see the manual" footnote), then
                    #   one adapter per file: command_palette, config_warning,
                    #   diagrams_enabled, diff_bulk_confirm, diff_intro,
                    #   diff_quit_confirm, diff_resolve_confirm, dirty_guard,
                    #   export_success, export_theme, images_enabled, insert_table,
                    #   keybinds, markdown_cheat_sheet, notice, overwrite_confirm,
                    #   post_upgrade, quit_confirm, remote_image, save_as, settings,
                    #   terminal_capabilities, theme_picker, update, welcome,
                    #   width_injection

  cli.rs / cli/
    args.rs         # Invocation / RunOpts / CliError; hand-rolled OsString flag parser
    doctor.rs       # `--doctor`: system facts (file reads only) + CapSummary rows
    help.rs         # `--help` / `--version` text; VERSION const

  config.rs / config/
    config.rs       # Config + sub-configs; LoadedConfig; load/save/ensure_default_files
    init.rs         # first-run scaffolding (writes the annotated config.toml)
    keymap.rs       # Action enum, KeyMap, KeyBindingOverrides, parse_key()
    persistence.rs  # the single "is the config dir in play?" gate; NOT_PERSISTED_NOTE
    readers.rs      # read_theme_named, read_keybindings — disk I/O helpers
    sections.rs     # surgical `toml_edit` updates that preserve comments
    theme.rs        # Theme styles; BUILTIN_THEMES; list_theme_names, Palette::builtin
    theme_file/     # ThemeFile, StyleSpec, ColorField — user-authorable TOML format
    themes/         # one file per built-in theme (edamame.rs, dracula.rs, …)
    warnings.rs     # ConfigWarning, WarningKind — surfaced via a startup modal

  diagram/mermaid.rs  # Mermaid → SVG → PNG (mermaid-rs-renderer + resvg)

  diff.rs / diff/
    engine.rs       # pure line + word diff; stable HunkIds; per-row table hunk split
    hunk.rs         # Hunk, HunkKind, Decision, InlineSpan / InlineSide
    layout.rs       # flat stacked visual-line model, clean/changed block partition,
                    #   cached per-width row-count table
    state.rs        # DiffState: hunks, decisions, focus, new-side buffer + parse,
                    #   layout cache; reconcile_with_disk / resolved_rope

  document.rs / document/
    buffer.rs       # Buffer over ropey::Rope; file I/O + edit primitives
    cursor.rs       # rope char offset + preferred visual column
    graphemes.rs, visual_cache.rs, selection.rs, source_map.rs
                    #   grapheme steps; memoised visual-row counts; Selection +
                    #   VisualSelection; block byte-range ↔ rendered-line mapping
    history.rs      # undo/redo stack of EditDelta; merges inserts into word-groups
    parsed_doc.rs   # re-parse on change; caches AST + source map; virtual blank blocks

  editor.rs / editor/   # EditorState, Mode, RAW_REVEAL_DELAY
    edit_ops.rs     # Action → EditorState mutations
    footnote_edit.rs, link.rs, list_edit/, table_edit.rs, table_edit_ops.rs
    mode.rs         # Mode enum: Preview | Rendered | Raw
    mouse_ops/      # checkbox, coord (screen → rope offset), footnotes, links,
                    #   selection, table_drag
    state.rs        # owns Buffer, Cursor, History, Mode, ParsedDoc
    state_cursor_block.rs   # cursor-block lookup + reveal jitter suppression
    state_cursor_visual.rs  # move_up_visual / move_down_visual
    state_section_path.rs   # cursor_section_chain — heading breadcrumb
    state_source_lines.rs   # rendered-row → source-line map for the gutter
    state_viewport.rs       # scroll + viewport clamping
    vim_ops/        # table scoping, :s preview, incsearch (see the vim sections)

  export/           # html.rs (AST → HTML, data: URIs when self-contained),
                    #   custom.rs (user command pipeline), runner.rs (tempfiles)

  image/            # loader.rs (decode worker, ureq fetch), cache.rs (URL →
                    #   DynamicImage + failure memoisation), render.rs (Picker)

  input.rs / input/
    mode_handler/default.rs  # DefaultHandler; preview_safe_action() allowlist
    mode_handler/diff_keys.rs # hard-bound diff key table — mirrors search_keys.rs
    mouse.rs        # MouseDispatcher: click-count + drag state machine
    vim/feed.rs     # vim keystroke reducer (see the vim sections)

  markdown.rs / markdown/
    ast.rs          # Block, Inline, ListItem; inlines_to_plain()
    highlight.rs    # syntect tokenizer: scope → TokenClass, char-indexed ranges,
                    #   size caps, incremental per-line ParseState reuse
    code_layout.rs  # code-block raw ↔ rendered column geometry; line_allows_raw_reveal
    inline_col_map.rs, list_layout.rs   # raw ↔ rendered column maps
    parse_offsets.rs # byte spans from pulldown-cmark; RangeTracker depth-0 scanner
    parser.rs (+ parser/post_pass.rs)   # → Vec<Block>; parse_raw_with_ranges
                    #   (single-pass blocks + ranges); promotion + loose-list blanks
    render_cache.rs # memoization keyed by Block value + settings fingerprint
    renderer.rs (+ renderer/{list,table,util}.rs)  # Vec<Block> → Vec<Line<'static>>
    table_layout.rs # column-width measurement and packed-comment hints

  search.rs / search/  # search_keys.rs (hard-bound key table), state.rs (SearchState:
                    #   terms, match ranges, focus, buffer-version freshness),
                    #   escape.rs (`\n` `\t` `\r` `\\` query escape syntax)

  terminal.rs / terminal/
    capabilities.rs # Capabilities, ColorDepth, ImageProtocol; env-only color detect
    setup.rs        # setup / restore / re_enter / enable_mouse / set_pointer_shape

  ui.rs / ui/
    bottom_region.rs    # hint line + status bar layout; HintChord / HintContent
    button_row.rs       # render_button_row (centered) / render_button_at (inline)
    cap_summary.rs      # capabilities-notice rows; theme_downgrade_lines
    command_palette.rs (+ /actions.rs)   # PaletteView / PaletteState (nucleo)
    controls.rs         # Control enum (Toggle / Pill), toggle_spans / pill_spans,
                        #   control_label_style / button_style / focused_style,
                        #   cycle_index + apply_images_cascade
    cursor.rs           # text_field_spans, split_at_char, CURSOR_BLOCK
    dim.rs              # ContentSize, FrameOpts, centered_rect_for_content,
                        #   draw_frame, ModalKind, MAX_PAD_H
    diff_view.rs        # DiffView + DiffViewState (stacked review)
    editor_view.rs      # dispatches to the three sub-views
    line_render.rs      # render_line / render_line_with_cursor: word-aware wrap,
                        #   trailing-cell fill; shared by Preview and Rendered
    preview.rs, raw_view.rs, rendered_view.rs (+ rendered_view/{paint, cell_overlay,
                        #   raw_text}.rs)         # the three editor sub-views
    export_theme_modal.rs, insert_table_modal.rs, save_copy_modal.rs  # text inputs
    keybinds_overlay.rs (+ /categories.rs), settings_overlay.rs (+ /rows.rs)
    modal.rs, modal_row.rs   # ModalView + ModalState (scrollable button modal)
    modal_links.rs      # inline modal-body hyperlinks: ModalLink / ModalLinkTarget,
                        #   the `Wrap { trim: false }` port that makes their
                        #   geometry knowable, and per-row rect derivation
    content_width.rs, gutter.rs, overlay_nav.rs, scroll_container.rs, scrollbar.rs
    image_view.rs, link_view.rs, table_view.rs   # layout snapshots / hit maps
    markdown_cheat_sheet.rs, status_bar.rs, theme_picker.rs, update_check.rs,
    welcome.rs

  test_env.rs       # #[cfg(test)] only — crate-wide env_lock() + EnvGuard

tests/              # diagrams, search, editing, footnotes, list_edit, mouse, palette,
                    #   renderer, source_map, table, ui; snapshots/ and fixtures/

config/             # config.toml (annotated reference, written on first run),
                    #   keybindings.toml, export/default.css

docs/               # USER-facing — shipped, kept accurate: getting-started, keybindings,
                    #   terminal-compatibility, configuration, editing, themes,
                    #   vim-mode, security
  dev/              # CONTRIBUTOR-facing: performance.md, security-invariants.md,
                    #   theming.md, why.md, windows.md, plans/ (historical), and one per-subsystem
                    #   deep-dive per subject (see "Architecture deep-dives" below)
```

**Docs are split by audience.** `docs/*.md` is for users and is published; `docs/dev/` is for contributors. When you change user-visible behavior, the user page is part of the change — and it must be derived from the code, not from this file. `AGENTS.md` records *intent and invariants*, which drift from the shipped surface faster than the code does.

### Architectural layers

Higher layers depend only on lower ones:

0. `cli` — argument parsing and the print-and-exit flags (`--help` / `--version` / `--doctor`); never starts the TUI
1. `main` — CLI dispatch, config load, terminal lifecycle
2. `app` — event loop, modal stack, autosave, external-editor flow
3. `ui` — ratatui widgets; `EditorView` dispatches to `PreviewView` / `RenderedView` / `RawView`; modal overlays composite on top
4. `input` — `ModeHandler` trait + `DefaultHandler`; `MouseDispatcher`
5. `editor` — `EditorState`; owns `Buffer`, `Cursor`, `History`, `Mode`, `ParsedDoc`
6. `document` — `Buffer`, `Cursor`, `History`, `ParsedDoc`, `Selection`, `SourceMap`, grapheme helpers
7. `markdown` — parser → AST → renderer; `parse_offsets` and `inline_col_map` feed `SourceMap`
8. `config` — `Config`, `KeyMap`, `Theme` (loaded once at startup)
9. `image`, `diagram`, `export`, `docs` — leaf subsystems used by the renderer / app
10. `terminal` — raw terminal setup / teardown / capability probing

`docs` is the one leaf reached from *above* layer 3: `Action::OpenDoc` carries a `docs::DocId`, `config`'s first and only dependency on another top-level module. Legal because `docs` is a true leaf — static strings and slug metadata, parsing nothing, importing nothing from this crate — but worth knowing before adding a second edge into `config`.

## Module Facade Pattern

Every top-level module has both a file (`src/config.rs`) and a subdirectory (`src/config/`). The file declares submodules with `pub mod` and re-exports public types with `pub use`, keeping call-site imports clean:

```rust
use crate::config::{Config, KeyMap, Theme};   // not crate::config::config::Config
```

Always follow this pattern for new top-level modules. Several mid-level modules (`editor::mouse_ops`, `editor::list_edit`, `markdown::parser`, `markdown::renderer`, `config::themes`, `config::theme_file`, `ui::rendered_view`, `app::modal`) follow it recursively.

## Architecture deep-dives

These decisions are easy to break if you don't know they exist. Each subsystem's invariants now live in one file per subject under [`docs/dev/`](docs/dev/); read the relevant one before touching that area.

- [Built-in themes and indexed-color fallback](docs/dev/themes.md) — the compiled-in theme registry and the truecolor-less substitution/downgrade path
- [Command-line entry points](docs/dev/cli.md) — the hand-rolled `cli::args` parser, `--doctor`, and the one-gate `--no-config` enforcement
- [Hybrid editing model](docs/dev/editing-model.md) — virtual blank blocks, the raw-reveal beat, wrap/cursor invariants, and render memoization
- [Frontmatter (YAML / TOML metadata blocks)](docs/dev/frontmatter.md) — verbatim metadata rendering and the identity column mapping four consumers depend on
- [Blockquotes](docs/dev/blockquotes.md) — base-style layering over already-rendered lines and the background wash
- [Syntax highlighting](docs/dev/syntax-highlighting.md) — the syntect tokenizer, the three caps, the grammar budget, and two-sided incremental reuse
- [Footnote reference markers](docs/dev/footnotes.md) — plain-ASCII markers and how adjacent references fuse into one
- [In-app documentation](docs/dev/in-app-docs.md) — the embedded manual: pathless read-only pages, relative-link and fragment resolution
- [Inline links in modal bodies](docs/dev/modal-links.md) — clickable, Tab-focusable links in modal prose and the wrapping port that places them
- [Search and replace](docs/dev/search-replace.md) — the flow's capture gating, freshness, smartcase-vs-exact matching, and escape syntax
- [Live `:s` substitution preview (vim `inccommand`)](docs/dev/substitute-preview.md) — the transient buffer rewrite, its three gates, and the shared match walk
- [Vim commands inside a table](docs/dev/vim-tables.md) — scoped motions, the range guard, and the paste/visual rules that keep table chrome intact
- [The vim command line](docs/dev/vim-cmdline.md) — how every paste reaches an open `/` `?` `:` prompt through one bounded path
- [Link following and deep links](docs/dev/link-following.md) — local/remote classification, `#fragment` handling, nav entries, and startup anchors
- [Diff review](docs/dev/diff-review.md) — the rendered/raw display partition, the parse lifecycle, and diff-mode image geometry
- [Keyboard and mouse input](docs/dev/input.md) — two-layer mouse dispatch, click-to-offset, table drags, and the distinct scroll bounds
- [Unified UI controls](docs/dev/ui-controls.md) — the one control family — toggles, pills, buttons, text inputs — and shared focus language
- [Modals, overlays, and the keybinds editor](docs/dev/modals.md) — the modal/overlay system, footer wrapping, scrolling, and the draft-keymap editor
- [Update check](docs/dev/update-check.md) — the GitHub release check: one fetch, one cache, four states, three entry points
- [Post-upgrade notice](docs/dev/post-upgrade.md) — the one-time notice driven from the bundled CHANGELOG, distinct from the update check
- [Images, diagrams, and export](docs/dev/media-export.md) — the image decode/encode workers, protocol quirks, Mermaid, and HTML/custom export

Longer-standing contributor docs live alongside them:

- [Performance](docs/dev/performance.md) — the frame budget, per-stage costs, and remaining ceilings
- [Security invariants](docs/dev/security-invariants.md) — the checklist for content-handling paths
- [Theming](docs/dev/theming.md) — authoring themes and the focus-vs-selection styling convention
- [Why](docs/dev/why.md) — design rationale
- [Windows](docs/dev/windows.md) — the best-effort stance: what CI verifies, what nobody has run, and the path-literal rules for tests

## Code Style

Write idiomatic, modern Rust; prefer language and standard-library features over manual workarounds.

- Prefer `#[derive(Default)]` over a hand-written `impl Default` when the output would be identical.
- Don't call `.into_iter()` on a value that already implements `Iterator`; call `.peekable()` etc. directly.
- Modern module layout: `src/foo.rs` alongside `src/foo/` — never `src/foo/mod.rs`.
- Prefer library helpers (`saturating_sub`, `unwrap_or_else`, …) over manual equivalents.

### Imports

Group with blank lines between: (1) `std`, (2) third-party crates (alphabetical), (3) `crate::` / `super::`.

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
| Traits | `PascalCase` | `ModeHandler` |
| Constructors | `new()` / `load()` / `build()` | `Config::load()` |
| Boolean predicates | standard Rust | `is_empty()`, `is_some()` |

### Formatting

4-space indentation; trailing commas in struct/enum literals and match arms; ~100-char target line length. Group methods with decorative section comments:

```rust
// ── Query ─────────────────────────────────────────────────────────────────

// ── Edit ──────────────────────────────────────────────────────────────────
```

### Types

- `enum` for domain state (`Mode`, `Action`, `Block`, `Inline`); `Option<T>` over nullable patterns.
- `#[derive(Debug, Clone, PartialEq, Eq, Hash)]` on core types.
- Lifetimes minimally and deliberately (e.g. `Renderer<'t>` borrows `Theme`).
- Newtype pattern for validated wrappers with `#[serde(transparent)]`.
- `StatefulWidget` for widgets that mutate scroll/cursor state; `Widget` for purely functional rendering.

### Error Handling

**Application code — `anyhow`:**

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

**Library/domain errors — `thiserror`:**

```rust
#[derive(Debug, thiserror::Error)]
pub enum KeyMapError {
    #[error("unknown action name: '{0}'")]
    UnknownAction(String),
    #[error("unparseable key string: '{0}'")]
    UnparseableKey(String),
}
```

**Panic prevention:** `.saturating_sub()` instead of `-` on `usize`; `.min()` / `.max()` to clamp before indexing; bounds-check before any rope or buffer operation.

### Logging

Use `tracing` macros — **never** `println!` / `eprintln!`, which would corrupt the TUI. Logging is initialized only when `[dev] logging = true` (or `--log`), so tracing calls are no-ops in production.

**The subscriber's filter is a bare `debug`, and it has to stay unscoped.** `tracing_subscriber::fmt()` defaults to `info`, which silently dropped every `debug!` — the diagnostic trail the flag exists for (image decode, watcher events, link handling) is almost entirely at `debug`. The obvious repair, `edamame=debug`, is also wrong: `EnvFilter` matches on *target*, and the diagnostic call sites set their own (`image`, `watcher`, `link`, `mouse`, `app`), none under the crate's target path. Nothing in the dependency graph pulls `tracing` (`cargo tree -i tracing` lists only this crate and the subscriber), so an unscoped filter cannot be flooded by a chatty dependency. `RUST_LOG` overrides it. A new custom target needs no filter change; a new *dependency* emitting `tracing` means re-checking that claim.

### Tests

- Every source file gets a `#[cfg(test)] mod tests { ... }` block; integration tests in `tests/` import from the library crate (`edamame::`).
- `insta::assert_debug_snapshot!` for complex output (ASTs, rendered lines); `ratatui::backend::TestBackend` for widget rendering.
- `Box::leak(Box::new(Theme::default()))` for a `&'static Theme` in tests — intentional and safe there.
- Commit updated `.snap` files alongside the code changes that caused them.

## Key Dependencies

| Crate | Purpose |
|---|---|
| `ratatui` | TUI framework (widgets, layout, rendering) |
| `crossterm` | Cross-platform terminal I/O and events |
| `pulldown-cmark` | CommonMark Markdown parser |
| `ropey` | Rope data structure for efficient text editing |
| `anyhow` / `thiserror` | Application errors / typed library error enums |
| `serde` + `toml` + `toml_edit` | Config deserialization and surgical writes |
| `serde_ignored` | Unknown-key warnings without `deny_unknown_fields` |
| `dirs` | Platform paths (XDG_CONFIG_HOME etc.) |
| `tracing` (+ appender, subscriber) | Structured logging (dev mode only) |
| `unicode-width`, `unicode-segmentation` | Grapheme and column measurement |
| `arboard` | OS clipboard (feature-gated `clipboard`; Wayland data-control on) |
| `ratatui-image` | Image-protocol probing and rendering |
| `image` | PNG/JPEG/GIF/BMP/WEBP decoding |
| `ureq` | Blocking HTTP client (rustls) for remote image fetches |
| `open` | Cross-platform URL / file opener |
| `nucleo-matcher` | Fuzzy matching for the command palette |
| `base64`, `tempfile` | Self-contained HTML export and custom-command pipelines |
| `mermaid-rs-renderer` + `resvg` + `usvg` + `sha2` | Mermaid diagram rendering |
| `tui-big-text` | Big-text rendering for H1 headings |
| `fancy-regex` | Regex for vim `:s`/`:%s` (the `/` search path stays literal-substring) |
| `syntect` + `two-face` | Code-block highlighting — parsing only (no themes, no HTML writer); 75 bundled grammars, 213 with `two-face` |
| `insta` / `proptest` | Snapshot / property-based testing |
