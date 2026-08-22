# AGENTS.md — edamame

Guidance for human and agentic contributors working in this repository. `CLAUDE.md` is a symlink to this file; edit one, update both.

## Project Overview

`edamame` is a Rust TUI application for viewing and editing Markdown files in the terminal. It uses `ratatui` for rendering, `pulldown-cmark` for parsing, and `ropey` for rope-based text editing. The crate ships as both a binary (`edamame`) and a library (so integration tests can import it).

> **Security:** edamame opens untrusted documents, so any change to a content-handling path (image/SVG decode, remote fetch, Mermaid, link opening, HTML export, subprocess spawning) must preserve the hardening in [`docs/security.md`](docs/security.md). Read it — and the checklist in [`docs/dev/security-invariants.md`](docs/dev/security-invariants.md) — before touching those areas.

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
cargo clippy                                # run the Clippy linter
cargo clippy --all-targets -- -D warnings   # treat warnings as errors (CI enforcement; covers lib + bin + tests + examples)
```

No custom `rustfmt.toml` or `.clippy.toml` exists; standard Rust defaults apply.

## Git Hooks

```bash
git config core.hooksPath .githooks   # once per clone — the hooks are inert without it
```

`.githooks/pre-push` refuses to push a commit whose message carries an AI co-author trailer (`Co-Authored-By: Claude …`, `Claude-Session:`, `noreply@anthropic.com`). GitHub builds its contributor list from those trailers as well as from the author field, so one of them puts a non-human in the contributor list, and removing it after the fact means rewriting history and force-pushing — which invalidates the signature on every commit it touches. The `Trailers` workflow runs the same scan on pull requests; it cannot cover a direct push to `main`, because by the time it runs the push has landed, which is why the hook exists as well. If you use Claude Code, also set `includeCoAuthoredBy: false` in its settings so the trailer is never written.

## Doc Commands

```bash
cargo doc --no-deps --document-private-items   # build the docs (canonical form)
cargo doc --no-deps --document-private-items --open
```

**Always pass `--document-private-items`.** This crate's doc comments are written for contributors, not downstream API consumers: they routinely link a private helper next to the public entry point that calls it, because the *why* usually lives in the private half. Without the flag those links point at pages rustdoc never generated. `lib.rs` carries `#![allow(rustdoc::private_intra_doc_links)]` for the same reason.

`broken_intra_doc_links` and `invalid_html_tags` are deliberately left at their default `warn`, and the tree is currently clean under both — treat a new warning as a real finding. Note that a literal `<name>` in prose parses as an HTML tag; backtick it.

## Test Commands

```bash
cargo test --no-fail-fast               # run all unit + integration tests
cargo test -- --list                    # list all test names

# Run a single test by exact name:
cargo test -- document::buffer::tests::insert_char --exact

# Run tests matching a name substring:
cargo test parse_heading

# Run unit tests only (inline #[cfg(test)] blocks).  Covers every
# module including `app` — main.rs declares no modules of its own, so
# there are no bin-only unit tests to miss.
cargo test --lib

# Run a specific integration test file:
cargo test --test renderer
cargo test --test ui
cargo test --test editing
cargo test --test source_map
cargo test --test palette
cargo test --test diagrams

# Review / accept updated insta snapshots:
cargo insta review
```

**Use `--no-fail-fast`, and expect the file-watcher tests to fail under an agent sandbox.** Plain `cargo test` stops at the first failing *target*, so a failure in the lib target aborts the run before `tests/` is even built — every integration test then silently goes unrun while the summary still looks like a real result. `watcher::file_watcher` and `tests/watcher.rs` need live filesystem-change notifications (FSEvents / inotify), which a sandbox typically withholds, so four tests there time out (`expected a debounced change: Timeout`). That failure is environmental; disregard *only* watcher timeouts and treat any other failure as real. `cargo nextest run` also runs every target regardless of failures and is faster where installed.

**Test frameworks in use:**
- `insta` — snapshot testing (`assert_debug_snapshot!`, `assert_snapshot!`)
- `proptest` — property-based testing (used in `tests/source_map.rs`)
- `tempfile` — temporary files for I/O tests
- `ratatui::backend::TestBackend` — headless widget rendering

**Environment-touching tests take `crate::test_env::env_lock()` — readers included — and mutate only through `test_env::EnvGuard`.** `std::env::set_var` is `unsafe` because it races any concurrent `env::var`, and cargo runs a binary's tests on parallel threads, so the exclusion has to be *crate-wide*: `config::config` writes `XDG_CONFIG_HOME` while `cli::doctor` reads it, and `terminal::capabilities` writes `TERM_PROGRAM` while `cli::doctor` reads that. A per-module lock cannot exclude either pair. `EnvGuard` restores on drop so a failing assertion can't leak a variable pointing at a deleted `tempdir` into every later test. **`config::persistence::SuppressGuard` is serialized by that same lock** and takes none of its own — hold `env_lock` for the whole test body, not just across the guard. These tests are shaped "assert nothing was written, drop the guard, assert the same call *does* write", and that second half runs with the gate back at `true`: a separate mutex released with the guard would let another test's suppression straddle it.

**A test that can reach `Config::save` takes `crate::test_env::config_isolation()`.** Nothing redirects `~/.config/edamame` during a test run, so an unguarded save rewrites *the developer's own config file* — and the values a test asserts are exactly the ones that do damage there (an update-check test recording `update_notified_for = "v999.0.0"` silences the real update notice permanently). The helper bundles `env_lock` with a `persistence::SuppressGuard`, so `Config::save` returns `Ok(())` without writing. Hold it for the whole test body and take it only once — it is the crate-wide mutex. The one test needing a *real* write (`save_writes_nothing_while_config_writes_are_suppressed`) takes `env_lock` and an `EnvGuard` pointing `XDG_CONFIG_HOME` at a tempdir instead. The check is that `cargo test` creates no file under the config directory.

**Do not** write tests for: terminal capability detection that depends on live terminal probing (`Picker::from_query_stdio`), cross-platform clipboard (OS clipboard paths race between parallel tests), or the crossterm terminal-mouse wire protocol — these are covered by manual smoke testing. *Do* test mouse logic at the `MouseDispatcher` + `mouse_ops::apply` layer: both are pure functions of an input event and an editor state.

## Project Structure

```
src/
  main.rs           # CLI dispatch, config load, terminal init, App::run.
                    #   Declares NO modules — it `use`s the library crate.
                    #   Re-declaring the tree here would compile a second
                    #   private copy, hiding its unit tests from `cargo test --lib`.
  lib.rs            # declares all modules, `app` included

  app.rs            # facade — declares submodules and re-exports App, AppEvent
  app/
    actions.rs        # Action → App-level side effects (modal pushes, nav, …)
    autosave.rs       # debounced autosave timer
    diff_advance.rs   # post-decision reveal delay before focus advances
    difftool.rs       # `--diff` helpers: read a side, decide whether the
                      #   pair is reviewable, name the status bar.  In the
                      #   library, not `main`, so they are testable
    event_loop.rs     # main run loop: term events, image-ready, link-open
    file_changed.rs   # watcher event → diff review / dirty-conflict modal /
                      #   mid-review reconcile
    external_editor.rs # pause-read / suspend-term / spawn $EDITOR / re-enter
    flash.rs          # TransientMessage on the hint line (MessageKind, ttl)
    frame_timer.rs    # frame-rate / redraw pacing
    image_dispatch.rs # spawn decode + encode workers; route ImageReady
    nav.rs            # NavEntry / file-open history (back / forward)
    pointer.rs        # mouse-cursor shape changes (link hover, etc.)
    update_check.rs   # facade for the GitHub release check
    update_check/
      fetch.rs        # the only network half: one worker, one GET
      parse.rs        # tag_name + release `body`; bounds the notes
      policy.rs       # pure: is a check due? does a result interrupt?
      status.rs       # ReleaseInfo / ReleaseStatus; version comparison
    update_notice.rs  # spawn / route / deferred-push orchestration
    modal.rs          # facade for the modal subsystem
    modal/
      stack.rs        # ModalStack: Vec<Box<dyn Modal>>, top-of-stack dispatch
      types.rs        # Modal trait, ModalKind, ModalOutcome, ModalRenderCtx
      command_palette.rs, config_warning.rs, diagrams_enabled.rs,
      diff_bulk_confirm.rs, diff_intro.rs, diff_quit_confirm.rs,
      diff_resolve_confirm.rs,
      dirty_guard.rs, export_success.rs, export_theme.rs, images_enabled.rs,
      insert_table.rs, keybinds.rs, markdown_cheat_sheet.rs, notice.rs,
      overwrite_confirm.rs, quit_confirm.rs, remote_image.rs, save_as.rs,
      settings.rs, terminal_capabilities.rs, theme_picker.rs, update.rs,
      welcome.rs, width_injection.rs    # one modal adapter per file

  cli.rs            # facade
  cli/
    args.rs         # Invocation / RunOpts / CliError; hand-rolled OsString
                    #   flag parser (no clap — see the module doc)
    doctor.rs       # `--doctor` report: system facts (file reads, never a
                    #   subprocess) + CapSummary rows
    help.rs         # `--help` / `--version` text; VERSION const

  config.rs         # facade — re-exports Config, KeyMap, Theme, ThemeFile, …
  config/
    config.rs       # Config + sub-configs (Editor, Modal, Table, Images, Dev,
                    #   Export, …); LoadedConfig; load() / save() /
                    #   ensure_default_files()
    init.rs         # first-run scaffolding (writes annotated config.toml etc.)
    keymap.rs       # Action enum, KeyMap, KeyBindingOverrides, parse_key()
    persistence.rs  # the single "is the config dir in play?" gate
                    #   (--no-config, reads + writes); NOT_PERSISTED_NOTE
    readers.rs      # read_theme_named, read_keybindings — disk I/O helpers
    sections.rs     # surgical `toml_edit` updates that preserve comments
    theme.rs        # Theme: all Style values; BUILTIN_THEMES registry;
                    #   list_theme_names(), Palette::builtin()
    theme_file.rs   # facade for theme-file submodules
    theme_file/     # ThemeFile, StyleSpec, ColorField: user-authorable TOML
                    #   format — converts to/from Theme via From impls
    themes.rs       # facade for built-in theme constructors
    themes/         # one file per built-in theme (edamame.rs, dracula.rs, …)
    warnings.rs     # ConfigWarning, WarningKind — surfaced via a startup modal

  diagram.rs        # facade
  diagram/
    mermaid.rs      # Mermaid → SVG → PNG (mermaid-rs-renderer + resvg)

  diff.rs           # facade — re-exports DiffState, Hunk, Decision, …
  diff/
    engine.rs       # pure line + word diff; stable HunkIds; per-row table
                    #   hunk splitting
    hunk.rs         # Hunk, HunkKind, Decision, InlineSpan / InlineSide
    layout.rs       # flat stacked visual-line model (DiffVisualLine /
                    #   DiffLineSource), the clean/changed block partition,
                    #   and the cached per-width row-count table the
                    #   renderer and the scroll math share
    state.rs        # DiffState: hunks, decisions, focused id, new-side
                    #   buffer, rendered new-side parse, layout cache;
                    #   reconcile_with_disk / resolved_rope

  document.rs       # facade — re-exports Buffer, Cursor, EditDelta, History,
                    #           ParsedDoc, Selection, SourceMap, grapheme helpers
  document/
    buffer.rs       # Buffer wrapping ropey::Rope; file I/O + edit primitives
    cursor.rs       # Cursor: rope char offset + preferred visual column
    graphemes.rs    # next/prev_grapheme_offset over a Rope slice
    history.rs      # undo/redo stack of EditDelta; merges adjacent
                    #   alphanumeric inserts into word-groups
    parsed_doc.rs   # re-parses on change, caches AST + source map;
                    #   synthesises a virtual block per blank line
    selection.rs    # Selection + VisualSelection (anchor + active rope offsets)
    source_map.rs   # block byte-range ↔ rendered-line-index mapping
    visual_cache.rs # memoised visual-row counts for wrapped lines

  editor.rs         # facade — re-exports EditorState, Mode, RAW_REVEAL_DELAY
  editor/
    edit_ops.rs     # Action → EditorState mutations (cursor, buffer, history)
    footnote_edit.rs # pure footnote edit primitives: scan, auto-number insert,
                    #   renumber (by first reference), delete
    link.rs         # LinkTarget enum + URL/path classification
    list_edit.rs    # facade
    list_edit/
      parse.rs        # list-marker detection
      edit.rs         # continuation, checkbox toggle, renumber
    mode.rs         # Mode enum: Preview | Rendered | Raw
    mouse_ops.rs    # facade — re-exports apply() and the public MouseOp shapes
    mouse_ops/
      checkbox.rs     # task-list checkbox hit-test + toggle
      coord.rs        # screen (row, col) → rope offset translation
      footnotes.rs    # footnote_at_offset hit-test ([^label] ref / def)
      links.rs        # link_at_offset source-scan (raw [..](..) parser)
      selection.rs    # click / drag / scroll selection logic
      table_drag.rs   # column-divider drag for table resize
    state.rs        # EditorState: owns Buffer, Cursor, History, Mode, ParsedDoc
    state_cursor_block.rs   # cursor-block lookup + reveal jitter suppression
    state_cursor_visual.rs  # move_up_visual / move_down_visual
    state_section_path.rs   # cursor_section_chain — heading breadcrumb
    state_source_lines.rs   # rendered-row → source-line map for the gutter
                            #   (inverts sub_lines_in_block; memoised per
                            #   parse in ParsedDoc::source_lines)
    state_viewport.rs       # scroll + viewport clamping
    table_edit.rs   # table detection + structure edits (row/col add/remove)
    table_edit_ops.rs # Action dispatch helpers for table_edit

  export.rs         # facade
  export/
    html.rs         # AST → HTML (embedded data: URIs when self-contained)
    custom.rs       # user-defined command pipeline (pandoc, weasyprint, …)
    runner.rs       # spawns commands, manages tempfiles

  image.rs          # facade
  image/
    loader.rs       # decode worker; URL fetch via ureq; format dispatch
    cache.rs        # URL → DynamicImage cache; failure memoisation
    render.rs       # ratatui_image::Picker integration

  input.rs          # facade — re-exports ModeHandler, MouseAction, MouseDispatcher
  input/
    mode_handler.rs # ModeHandler trait + `default` submodule
    mode_handler/
      default.rs    # DefaultHandler: non-modal keybinding implementation;
                    #   preview_safe_action() allowlist
      diff_keys.rs  # hard-bound diff-review key table (diff_action_for,
                    #   diff_hint) — mirrors search/search_keys.rs
    mouse.rs        # MouseDispatcher: click-count + drag state machine

  markdown.rs       # facade
  markdown/
    ast.rs          # Block, Inline, ListItem enums; inlines_to_plain()
    highlight.rs    # syntect-backed code-block tokenizer: scope → TokenClass,
                    #   char-indexed token ranges, size caps, incremental
                    #   per-line ParseState reuse
    code_layout.rs  # code-block raw ↔ rendered column geometry (the one
                    #   leading pad cell + an indented block's stripped
                    #   indent) and line_allows_raw_reveal
    inline_col_map.rs # InlineColMap: raw byte ↔ rendered visual column
    list_layout.rs  # list-marker raw ↔ rendered column geometry
    parse_offsets.rs  # (byte_start, byte_end) spans from pulldown-cmark;
                    #   RangeTracker incremental depth-0 scanner
    parser.rs       # pulldown-cmark → Vec<Block>; parse_raw_with_ranges
                    #   (single-pass blocks + ranges); promotes images /
                    #   diagrams / html comments to their own blocks
    parser/
      post_pass.rs  # promotion + loose-list blank annotation transforms
    render_cache.rs # block-level render memoization keyed by Block value +
                    #   render-settings fingerprint
    renderer.rs     # Renderer<'t>: Vec<Block> → Vec<Line<'static>>
    renderer/
      list.rs, table.rs, util.rs  # block-specific render helpers
    table_layout.rs # column-width measurement and packed-comment hints

  search.rs         # facade
  search/
    search_keys.rs  # hard-bound search-flow key table (search_action_for,
                    #   search_hint) — mirrors input/mode_handler/diff_keys.rs
    state.rs        # SearchState: query / replace terms, match byte ranges,
                    #   focused index, buffer-version freshness

  terminal.rs       # facade
  terminal/
    capabilities.rs # Capabilities, ColorDepth, ImageProtocol;
                    #   detect_color_depth_from_env() (env-only, no I/O)
    setup.rs        # setup() / restore() / re_enter() / enable_mouse() /
                    #   set_pointer_shape() / PointerShape

  ui.rs             # facade — re-exports widgets and state types
  ui/
    bottom_region.rs    # hint line + status bar layout; HintChord / HintContent
    button_row.rs       # shared [ Button ] elements: centered row
                        #   (render_button_row) + left-aligned inline
                        #   (render_button_at); on controls::button_style
    cap_summary.rs      # capabilities-notice body lines
    command_palette.rs  # PaletteView + PaletteState (nucleo-matcher fuzzy)
    command_palette/actions.rs   # palette-eligible Action list
    content_width.rs    # measure expected wrapped width
    controls.rs         # unified control family: Control enum (Toggle / Pill),
                        #   toggle_spans / pill_spans, shared style helpers
                        #   (control_label_style, button_style, focused_style),
                        #   cycle_index + apply_images_cascade
    cursor.rs           # text_field_spans, split_at_char, CURSOR_BLOCK
    dim.rs              # ContentSize, FrameOpts, centered_rect_for_content,
                        #   draw_frame, ModalKind, MAX_PAD_H (modal layout)
    diff_view.rs        # DiffView + DiffViewState (stacked review; rendered
                        #   clean regions, raw changed ones)
    editor_view.rs      # EditorView + EditorViewState; dispatches to sub-views
    export_theme_modal.rs # write-current-theme-to-disk modal
    gutter.rs           # optional line-number column; split_gutter()
    image_view.rs       # ImageLayoutSnapshot — block-image rendering
    insert_table_modal.rs # rows × cols prompt
    keybinds_overlay.rs # KeybindsView + KeybindsState (view + edit unified)
    keybinds_overlay/categories.rs # grouped Action list
    line_render.rs      # render_line / render_line_with_cursor: word-aware
                        #   wrap, trailing-cell background fill; shared by
                        #   Preview and Rendered
    link_view.rs        # LinkLayoutSnapshot — link-target hit map
    markdown_cheat_sheet.rs # body_lines() — Markdown syntax reference
    modal.rs            # ModalView + ModalState (scrollable button modal)
    modal_row.rs        # button row used inside ModalView
    overlay_nav.rs      # shared Tab / arrow focus cycling
    preview.rs          # PreviewView + PreviewState
    raw_view.rs         # RawView + RawViewState (plain-text editor)
    rendered_view.rs    # RenderedView + RenderedViewState (hybrid view)
    rendered_view/      # paint, cell_overlay, raw_text submodules
    save_copy_modal.rs  # "Save copy as…" path entry
    scroll_container.rs # ScrollContainerState; ModalKind lives here
    scrollbar.rs        # narrow side scrollbar widget
    settings_overlay.rs # SettingsView + SettingsState (config UI)
    settings_overlay/rows.rs # row definitions + theme cycle list
    status_bar.rs       # StatusBar + StatusBarState
    table_view.rs       # table rendering / column-divider hit map
    theme_picker.rs     # live review picker
    update_check.rs     # UpdateReport -> body lines for the update modal
    welcome.rs          # first-run welcome modal

  test_env.rs       # #[cfg(test)] only — crate-wide env_lock() + EnvGuard

tests/
  diagrams.rs       # Mermaid block detection + render pipeline
  search.rs         # search-flow lifecycle, hint row, highlight painting
  editing.rs        # EditorState action sequences → buffer/cursor asserts
  footnotes.rs      # footnote edit primitives + mouse-follow path
  list_edit.rs      # list continuation, renumber, checkbox toggle
  mouse.rs          # mouse click / drag / scroll / checkbox
  palette.rs        # command palette filtering + selection
  renderer.rs       # parse + render → assert/snapshot
  source_map.rs     # unit + proptest tests for SourceMap invariants
  table.rs          # table navigation + structure edits
  ui.rs             # TestBackend widget rendering
  snapshots/        # committed insta .snap files
  fixtures/         # sample Markdown documents for manual smoke testing

config/
  config.toml       # annotated reference config, written to
                    #   ~/.config/edamame/config.toml on first run
  keybindings.toml  # commented-out keybinding overrides reference
  export/default.css # default stylesheet bundled with self-contained HTML export

docs/               # USER-facing documentation — shipped, kept accurate.
  getting-started.md, keybindings.md, configuration.md, editing.md,
  themes.md, vim-mode.md, security.md
  dev/              # CONTRIBUTOR-facing — design specs and rationale
    theming.md      #   visual-language conventions (cursor, focus, controls)
    why.md          #   project rationale
    plans/          #   historical implementation plans (excluded from crate)
```

**Docs are split by audience.** `docs/*.md` is written for users and is published; `docs/dev/` is written for contributors. When you change user-visible behavior, the user page is part of the change — and it must be derived from the code, not from this file. `AGENTS.md` records *intent and invariants*, which drift from the shipped surface faster than the code does.

### Built-in themes

The `BUILTIN_THEMES` registry in `src/config/theme.rs` lists every compiled-in theme (Edamame, Dracula, Nord, Gruvbox, Catppuccin, …). Each constructor lives in its own file under `src/config/themes/`.

`Config::ensure_default_files` creates `themes/` empty — built-in theme files are never written to disk. `read_theme_named` short-circuits to `Palette::builtin(name)` before any disk read, so a user file with a built-in name is ignored entirely. To add a new built-in: write `src/config/themes/<name>.rs` with a `pub fn theme()` and add it to `BUILTIN_THEMES`; both the load path and the theme-picker / settings cycle (via `list_theme_names`) pick it up automatically. Custom user themes go in `~/.config/edamame/themes/<name>.toml` under any name not in the registry.

**Indexed-color substitution.** `256 Dark` / `256 Light` are the only built-ins authored against the xterm-256 cube; every other theme picks 24-bit colors that an indexed terminal quantizes, routinely landing fg and bg on the same entry. So on a terminal without truecolor, `app::theme_fallback::apply` swaps `config.theme` for the matching indexed built-in (dark/light follows the *current theme's* appearance, not just the `appearance` key) and `ThemeDowngradeModal` reports it.

Themes that already render correctly below truecolor are exempt — `theme::INDEXED_SAFE_THEMES`, for which `indexed_fallback_theme` returns `None`: the two `256 *` targets (which also makes the swap idempotent across reloads) plus `Monochrome Dark`, whose every palette slot is `Color::Reset`. The exemption covers the warning as much as the swap; `indexed_safe_themes_are_registered` pins the list against `BUILTIN_THEMES`.

The standalone `ThemeDowngradeModal` carries the news except when the capabilities notice fires on the same launch (a first visit to that terminal), where the notice absorbs the same prose via `with_theme_downgrade` — one terminal change, one modal. Both render `ui::cap_summary::theme_downgrade_lines`, and both hand `ModalView` *paragraphs*, not pre-broken lines.

Three invariants: (1) the swap happens **before the first frame** in `App::new` — a modal explaining unreadable colors that is itself unreadable is worse than useless; (2) it is **never persisted** — the user's choice is stashed in `Config::theme_downgraded_from` and written back in `theme`'s place by `Config::save`, because one `config.toml` is typically shared with a truecolor machine; (3) both paths that resolve a theme from a freshly-read `Config` must call `theme_fallback::apply` — startup *and* the external-editor reload, which would otherwise repaint the session in the palette we just swapped away from.

`NoColor` is exempt at the *depth* end for the mirror-image reason: `App::new` passes `monochrome` to `Theme::from_file`, which strips every color whatever the active theme, so a swap there would be invisible. Below 24-bit color `App::media_renderable` also refuses images and diagrams outright — same quantization argument, and likewise session-only, so a persisted `Always` survives in `config.toml` for the user's capable terminal. **That session-only promise is enforced at the write sites:** `WelcomeModal::save_outcome` writes the three media fields only when `image_capable`, so the `Never` the modal *displays* below truecolor is never persisted. And when the notice carries the downgrade it drops its "Adjust settings" button — with theme, images, and diagrams all disabled there, the welcome modal has nothing left but the vim toggle.

**While the stash is set, `config.theme` is effectively unwritable** — `Config::save` puts `theme_downgraded_from` in its place — so every path that commits a theme *choice* must go through `Config::set_theme`, which clears it: the theme picker's selection and the export-theme modal's newly created theme. A bare `app.config.theme = …` writes the theme to screen but silently drops it at save time, and the user sees their choice vanish on next launch with no error. The only legitimate direct assignments are `theme_fallback::apply` itself and the picker's transient preview / revert (which deliberately do *not* clear the stash — `Esc` must restore the substitution).

### Architectural layers

Higher layers depend only on lower ones:

0. `cli` — argument parsing and the flags that print and exit
   (`--help` / `--version` / `--doctor`); never starts the TUI
1. `main` — CLI dispatch, config load, terminal lifecycle
2. `app` — event loop, modal stack, autosave, external-editor flow
3. `ui` — ratatui widgets; `EditorView` dispatches to `PreviewView`,
   `RenderedView`, `RawView`; modal overlays composite on top
4. `input` — `ModeHandler` trait + `DefaultHandler`; `MouseDispatcher`
5. `editor` — `EditorState`; owns `Buffer`, `Cursor`, `History`, `Mode`,
   `ParsedDoc`
6. `document` — `Buffer`, `Cursor`, `History`, `ParsedDoc`, `Selection`,
   `SourceMap`, grapheme helpers
7. `markdown` — parser → AST → renderer pipeline; `parse_offsets` and
   `inline_col_map` feed `SourceMap`
8. `config` — `Config`, `KeyMap`, `Theme` (loaded once at startup)
9. `image`, `diagram`, `export` — leaf subsystems used by the renderer / app
10. `terminal` — raw terminal setup / teardown / capability probing

## Module Facade Pattern

Every top-level module has both a file (`src/config.rs`) and a subdirectory (`src/config/`). The file declares submodules with `pub mod` and re-exports public types with `pub use`, keeping call-site imports clean:

```rust
use crate::config::{Config, KeyMap, Theme};   // not crate::config::config::Config
```

Always follow this pattern when adding new top-level modules. Several mid-level modules (e.g. `editor::mouse_ops`, `editor::list_edit`, `markdown::parser`, `markdown::renderer`, `config::themes`, `config::theme_file`, `ui::rendered_view`, `app::modal`) follow it recursively.

## Architectural Notes

These decisions are easy to break if you don't know they exist.

### Command-line entry points

- **`main` is a dispatcher over `cli::Invocation`, and the parser is hand-rolled on purpose.** `clap` is deliberately absent from the graph (which is why `mermaid-rs-renderer` is declared `default-features = false` — its `cli` feature would drag clap in), so a new flag is a match arm in `cli::args`, not a new dependency. Arguments are taken as `OsString`: `std::env::args()` *panics* on a non-UTF-8 argument, which is a legal Linux file name, so flags are matched only after a successful `to_str()` and anything else falls through to the positional arm intact.
- **`--doctor` never re-derives capability text.** The five capability rows come from `ui::cap_summary::CapSummary::from_caps`, the same builder the welcome modal and the capabilities notice render; a second phrasing would drift. `cli::doctor` adds only the *system* half (OS, terminal, `TERM`, `COLORTERM`, locale, tmux) and the third `Status::Unknown` state. Every row describes the *machine*, never the person: the report is written to be pasted into a public issue tracker. That is why the config directory is not in it — it is a username in the common case, and all it carries diagnostically is "this path is or isn't the default". A new row owes the same test.
- **That third state exists because the probe needs a tty.** `Capabilities::detect` writes escape sequences and reads the replies off the terminal, so `edamame --doctor > report.txt` would both pollute the file and report "no image support" for a terminal that has it. `doctor::run` checks `IsTerminal` on stdout *and* stdin, falls back to `Capabilities::env_only` (color / mouse / locale, all env-derived), and marks exactly the two probe-derived rows — Images and Keyboard — unknown.
- **System facts are file reads, never subprocesses.** `/etc/os-release` on Linux, `SystemVersion.plist` on macOS (a string scan, same posture as `update_check::parse_tag_name`), `$TERM_PROGRAM_VERSION` for the terminal version; Windows reports the bare OS name. Spawning `sw_vers` / `lsb_release` would add a process spawn to an area `docs/security.md` hardens; every lookup degrades to a coarser answer instead of an error.
- **`--no-config` is enforced at every read and write site, through one gate.** Skipping the load is the easy half; the trap is a mid-session *write* overwriting the user's real config with compiled defaults. `config::persistence` owns one process-global `AtomicBool` that `main` clears once via `disable_config_dir()` before any config file is touched, exposed as `config_writes_allowed()` and `config_reads_allowed()` — two names for one fact, so a reader isn't guarded by a function with "writes" in its name.
  - **Four sites write into `~/.config/edamame` and all four ask it:** `Config::save` (returns `Ok(())` without writing — nothing went wrong), the keybinds overlay's `try_persist` (whose `save_to` *truncates* `keybindings.toml` from in-memory overrides a `--no-config` session never read, so one rebind would wipe the user's real file), the export-theme modal (refused outright — a custom theme *is* its file, so a suppressed write would leave `set_theme` naming a theme that can't resolve), and `App::open_config_in_editor` (refused: it both seeds `config.toml` and reloads from it). `ensure_default_files` is the sole exception, and only because `main` never calls it under the flag.
  - **The read half needs the same treatment**, because the config directory is read *again* mid-session by surfaces that enumerate what the user dropped into it. Ungated, `--no-config` would still list — and on selection load — `themes/*.toml` in the theme picker, the settings overlay's theme cycle, and the export-theme source list. Three sites ask `config_reads_allowed()`: `theme::list_theme_names` (which all three theme surfaces build from), `readers::list_export_stylesheets`, and `readers::read_theme_named` as defense in depth — it substitutes the capability-appropriate built-in and returns `None` for the fallback name, since there is no rename to persist and nothing is *missing* (a `MissingTheme` warning modal on every launch would be noise). A new reader or writer owes the matching check.
  - **The gate is a global precisely because a `Config` field was not enough.** It was one, briefly: a `#[serde(skip)]` `persist` field. `App::open_config_in_editor` reloads config from disk and assigns the result to `self.config` wholesale, so the flag silently reverted to its serde default mid-session. A per-process fact belongs in a per-process place, and there is deliberately no way to re-enable it. Messages reporting a settings change append `config::unpersisted_suffix()` (`NOT_PERSISTED_NOTE`, one const so all three flashes phrase it identically) so "Configuration updated" never claims a write that didn't happen.

### Hybrid editing model

- **Virtual blocks for blank lines.** `ParsedDoc::build` synthesises a one-byte block for every blank line in the source (leading, between-block, trailing). The cursor lands on each blank line as its own block, and the blank line is preserved in `RenderedView` even when the surrounding cursor block is replaced with raw text. Don't reintroduce `parse_offsets::covering_ranges` for the cursor mapping — it absorbs blank-line bytes into adjacent blocks and breaks navigation.
- **`per_block_own` vs. extended ranges.** `ParsedDoc` tracks both per-block *own* rendered line counts (for the raw-replacement region in `RenderedView`) and *extended* covering ranges (for cursor lookup). Mixing them up causes gap blank lines to collapse when the cursor enters the previous block.
- **Loose lists stay one block; blanks are annotated, not split.** A blank line between list items makes the list "loose" (CommonMark). edamame keeps it a single `Block::List` and records how many blank source lines precede each item in `ListItem::blank_lines_before` (`parser::post_pass::annotate_list_blanks`, reusing the fence-aware blank-run scanner). The renderer emits that many blank `Line`s before the item, so a loose list keeps its spacing while numbering comes straight from pulldown-cmark (a source restarting at `1.` after a blank renders sequentially, matching CommonMark). Don't reintroduce a `split_lists_on_blank_lines` pass: fragmenting the list forced per-group `start` re-derivation and block/range surgery for no rendering benefit.
- **The reveal depends on rendered rows staying 1:1 with source lines.** `RenderedView` maps a block's rendered rows to its source lines via `block_text.split('\n')`, so every separator blank must emit exactly one rendered row. The raw→rendered line mapping — `editor::state::sub_lines_in_block`, the single implementation — therefore counts *separator* blanks (a blank run ending at a top-level marker) as rendered rows while skipping interior-item blanks and soft-break continuations; don't revert it to "count only non-blank lines" (that swallows the blank row and reveals the cursor line one row too high). It answers a *whole block* per call, with `cursor_sub_line_in_block` the thin single-line entry point that indexes the result — the gutter's inverse needs every line anyway, and asking line-by-line made it quadratic in block length.
  - **Four callers must agree on it and so must never re-derive it:** `RenderedView` (which rendered row gets the raw-text replacement), `cursor_rendered_line_idx` (where the cursor appears, for scroll arithmetic), — through the latter — `mouse_ops::coord`'s `revealed_cursor_line` shortcut, and `editor::state_source_lines` (which source line the gutter labels each rendered row with). When they drift, a click on a revealed line is mapped against the *rendered* spans instead of the raw text on screen, so a line whose markers were dropped (`` `code` ``, `**bold**`) places the cursor short by the marker width.
  - `state_source_lines` *inverts* the function rather than hand-writing an inverse, precisely so it isn't a fourth derivation. Three rules there: **last writer wins** (a line that renders no row shares its sub-row with the *next* line, and it is that next line's text on screen); it skips blocks whose `block_own_line_count` is 0 (`rendered_lines_for_block` hands a row-less block its neighbour's range, which would label a row belonging to another block); and it reads the document out of `ParsedDoc::source` rather than the live `Buffer`, because it resolves *parse-time* byte ranges and a deferred in-line edit makes those two different coordinate spaces — and since the table is memoised per parse, a build landing in that window mislabels the document until the next reparse, not for one frame.
  - **Its *inputs* are shared too:** `rendered_view::raw_text::raw_block_cursor` is the single derivation of the `(block source, raw line index, column)` triple both callers feed it — the line index is an index *into* that source, so deriving one without the other is how they drift (two hand-written byte walks disagreed on where a cursor at the block end lands: last line vs. block top). `RenderedView` keeps exactly one branch of its own, for a stale parse, where it rebuilds the triple from `cursor_block_line_range` so just-typed characters are visible before the reparse.
- **Jitter-suppression reveal.** `EditorState::cursor_block_revealed()` returns false during a 120 ms `RAW_REVEAL_DELAY` after the cursor enters a new *buffer line* (not block). `RenderedView` keeps the block fully rendered and draws an inverted-cell cursor indicator at `(cursor_col, cursor_row)` until the delay elapses. The app loop uses `rx.recv_timeout(60 ms)` so the redraw fires without a keypress.
- **Single-pass parse.** `ParsedDoc::build` gets blocks AND top-level byte ranges from one `parse_raw_with_ranges` call — a `parse_offsets::RangeTracker` observes the same offset-iterator events the AST builder consumes, so blocks↔ranges stay 1:1 by construction. Don't reintroduce a second `top_level_block_ranges` pass alongside `parse_raw`; the two-pass pairing cost a full extra pulldown-cmark parse per reparse.
- **Block-level render memoization.** `EditorState` owns a `markdown::RenderCache` threaded into every `refresh_parsed`; blocks whose AST value is unchanged reuse their rendered lines (a clone). The cache key is the `Block` value itself plus a render-settings fingerprint (theme address, viewport width, `table.row_striping`, big-H1, …) — keying by AST, not source bytes, is what keeps live table-width drags and post-pass mutations correct (a mutated block simply misses). `Block::ImageBlock` is never cached because its row count tracks the image decode cache, which changes without an AST change. Eviction is by document membership per build. If you add a `Renderer` knob that changes rendered output, add it to `RenderSettings` or stale cache hits will paint with the old setting.
- **Single shared `line_render` module.** `PreviewView` and `RenderedView` both call `ui::line_render`; the trailing-cell background fill and word-aware wrap live there. Change the shared function, don't fork it.
- **Where a row may break is one predicate, and a break may swallow a space.** `line_render::is_break_after` decides whether a visual row can end with a given char. The base rule is "anything non-alphanumeric", narrowed by three refinements that exist because the bare rule split things users read as one token (issue #36): a no-break char (U+00A0 and friends — the same NBSP the code-block padding relies on) is never a break; an *opening* delimiter (`(` `[` `{` `“` `‘` `«` `¿` `¡`, plus `"`/`'` when a word follows and none precedes) is never one, so it can't hang alone at the row edge; and `.` `,` `:` `'` `’` `_` are not breaks when sandwiched between two alphanumerics, which keeps `they’re`, `3.14`, `12:30` and `file.md` whole. **`/` is deliberately outside that set**: the slashes in a URL path sit between two alphanumerics exactly as `and/or`'s does, so including it strips every break point out of `github.com/user/repo/blob/main/…`. Links are far more common in Markdown than `and/or`, and the ordinary-break treatment is what lets a URL break after `//`, `?`, `#` and `&`. Both neighbours matter, so the predicate takes the slice and an index rather than a lone `char`. A break landing right after a one-letter word backs up to the preceding break (`ends_with_lone_word`) so an `a` or `I` travels down with its noun — unless that word starts the row. **Because smart punctuation is on, a rendered contraction carries `’` (U+2019), not `'`** — a rule naming one owes the other.
- **A row may never end mid-grapheme-cluster, and the cluster gate outranks every break refinement.** The wrap reasons in `char`s, where a ZWJ is an ordinary non-alphanumeric and so was a good break candidate — `👨\u{200d}👩\u{200d}👧\u{200d}👦` wrapped as a partial family across two rows, and a combining mark could be stranded at a row head without its base char. `cluster_starts` builds a boundary mask once per line and both the break search (`is_break_after`) and the hard-break window (`snap_to_cluster_boundary`) consult it. Two things keep it affordable and terminating: an **all-ASCII fast path** returns `None` (no allocation, no segmentation), keeping this off the per-keystroke navigation path for ordinary text; and a snap that would empty the row **keeps the unsnapped break**, so a single cluster wider than the viewport still makes progress. This fixes *breaking* inside a cluster, not *measuring* one: `char_cells` still sums a ZWJ sequence's members (8 cells for the family), right on terminals that draw it that way and wrong on kitty and ghostty — a separate, terminal-dependent question.
- **A break absorbs the single space that follows it, so `next_start` may exceed `row_end`.** A space left at a row *start* reads as accidental indentation. Most soft breaks are already past their space, but not all, so `absorbed_next_start` applies to every arm rather than only the hard break: a row ending on a `.`, a `)` or an emoji cluster still has the sentence's space to come. The absorbed char belongs to no row, which the `(start, end, next_start)` triple can express — but nothing produced it before, so both cursor paths had to learn it: `sub_line_of_col` reports a column in `end..next_start` as the **next row's column 0**, and `render_line_reporting_cursor` rewrites a `cursor_col_override` landing there onto that row's first char, or the cursor sits on a cell that doesn't exist and simply doesn't paint. A run of spaces is never absorbed (interior whitespace is content — `visual_rows_preserves_interior_whitespace_across_wrap`), nor is a trailing one. The overlay painters intersect ranges with `[row_start, row_end)`, so an absorbed char is never washed.
  - **The clamp keeping a cursor on its own row is `line_render::last_col_in_row`, the single derivation all four click- and navigation-mapping sites share** (`state_cursor_visual::raw_col_for_visual_cells`, and `mouse_ops::coord`'s `char_in_row_at_cell`, `non_table_click_to_raw_col` revealed-line branch and `click_to_rendered_char_idx`; `rendered_click_to_line_col` clamps with it too). It measures against `end`, never `next_start` — the two are equal for an ordinary wrap, so `next_start - 1` read as correct until a break absorbed something, and then it clamped the cursor *onto* the absorbed space: a char with no cell. Up then looked like it never moved and stalled permanently, and a click on a row's last cell landed a row low. A new site mapping a screen cell back to a char column owes the helper the call.
- **The cursor is a uniform block; color (not shape) signals context.** Every cursor is a fake block: the cell at the insertion point is recolored with the cursor style while the character stays visible. The color is resolved in one place — `app::cursor_style::editor_cursor_style` — and the views receive the resolved `Style`. The default handler colors by *view* mode (`status_mode_preview` / `status_mode_rendered` / `status_mode_raw`); under the vim handler the cursor **mirrors the sub-mode chip**, reading the same `status_mode_vim_normal` / `_insert` / `_visual` fields the status bar uses (minus the chip's `BOLD`) so chip and cursor can't drift. RAW (`status_mode_raw`) is surfaced only in INSERT; NORMAL/VISUAL keep their sub-mode color in every view, matching the chip (no `(RAW)` suffix). Modal inputs use `theme.cursor`. There is no bar/caret shape and no `CursorShape` enum.
  - The editor cursor is painted via `cursor_col_override`, not baked into the wrapped `Line`, so the word-aware wrap keys on the *glyph-free* source text and stays in lockstep with `move_up_visual`/`move_down_visual` and the scroll math: `line_render::paint_row` recolors the resolved cell, the raw-reveal builders in `rendered_view/paint.rs` (`make_raw_line_with_selection`, `make_code_styled_body_line`) and `raw_view::raw_display_line` build the line without the cursor, and `overlay_raw_cell` recolors the table cell in place. A block cursor on a selected/highlighted cell wins over that wash.
- **Modal input cursors are blink-stable.** Every modal text field renders its block cursor via `ui::cursor::text_field_spans`, which always emits a one-cell slot — the character under the cursor recolored when the blink is on, shown plainly (a space past end-of-line) when off — so the field never changes width between blink phases. A new text-input modal must use this helper, or where a value flows through `format_modal_row`/horizontal scroll and the cursor cell can't carry its own style, mirror its constant-width slot with `cursor::CURSOR_BLOCK`.
- **Code body rows render behind one pad cell, and three consumers must agree on it.** `Renderer::render_code_block` paints every non-empty body line as `format!(" {:<width$}", …)`, so rendered column `c` shows raw char `c - 1`; an *indented* block adds the up-to-4-space (or tab) indent pulldown-cmark strips before the text reaches `Block::CodeBlock::content`. Both live in `markdown::code_layout` — forward for the two painters (`paint_byte_range_overlay`'s selection/search wash and `RenderedView`'s cursor indicator), inverse (`code_rendered_col_to_raw_col`) for `mouse_ops::coord`'s hit-test. Only the overlay had it once, which is issue #28: the cursor painted one cell left of its character and a click landed one char right of the glyph, on every code line. The cursor arm sits **ahead of the list arm** in the `visual_col` chain, or a code line reading `- foo` is claimed by the list-marker sniff and shifted by a marker width it doesn't have.
  - **Fence rows are outside the mapping** — the opening row renders a ` lang ` label (or an NBSP placeholder) and the closing row a placeholder, so no column relation to their raw ``` ``` ``` exists; ask `is_code_fence_row` and wash the whole row (overlay) or leave the column alone (cursor). A **blank** body row still maps to the pad column, or the cursor jumps on the first keypress.
  - **`raw_lines` must come from `raw_source_lines`, never a bare `split('\n')`** — a block range ends with its trailing newline, so the bare split appends a phantom entry and the *real* closing fence is not the last element. **And the closing fence is identified by its *text*, not its index**: `is_code_fence_row` asks `is_closing_fence_line` (3+ of one delimiter and nothing else) before believing the last raw line is a fence, because an *unclosed* fence — what every code block looks like while being typed — ends on ordinary code that still needs the column mapping. Index alone put the cursor beside its character, landed clicks late and washed the whole row, and disagreed with `RenderedView`'s own `is_closing_fence_row`, which has always tested the text.
- **A code body line never de-renders, so "revealed ⇒ 1:1" is wrong inside one.** `code_layout::line_allows_raw_reveal` is the single derivation of which lines `RenderedView` replaces with raw source: every non-code line, a fenced block's opening fence plus a closing one the user has actually typed, and nothing in an indented block. `EditorState::cursor_block_revealed()` deliberately does **not** know about it — that answers a block-level, time-based question for four other consumers and can't name a line. Both `RenderedView`'s reveal gate and `mouse_ops::coord` (the revealed-line click shortcut *and* `revealed_raw_row_count`, which would otherwise report a raw line's wrap count for a row painted as one padded rendered line) ask the shared predicate.
- **NBSP padding in code blocks.** Blank lines inside fenced code blocks use U+00A0, not regular spaces, working around a ratatui `WordWrapper` (`trim: false`) bug where an all-whitespace line produces an extra empty visual row. Don't "simplify" this back to spaces.
- **Word-group undo merging.** `History::record` merges single alphanumeric inserts into the previous delta when offsets are contiguous. Cursor moves break the group naturally. It's still delta-based, not snapshot-based.
- **Visual line navigation.** `move_up_visual` / `move_down_visual` (in `editor/state_cursor_visual.rs`) and `line_render::render_line` must use the same wrap algorithm (`visual_rows_of_str` / `sub_line_of_col`), or the cursor lands in a different column than where it appears.
- **Action enum is the full surface.** Every action lives in `config/keymap.rs::Action`. Keybindings stay stable even when a feature is in flight; unimplemented variants are no-ops in `edit_ops` until wired up.
- **Clipboard is feature-gated.** `arboard` is behind the `clipboard` Cargo feature (on by default). When disabled, copy/cut/paste use the in-process kill-ring only. Tests assert against the kill-ring, not the OS clipboard, to avoid cross-test races. On Wayland the `wayland-data-control` feature is required for read access — without it `Clipboard::new()` returns `Err`.

### Frontmatter (YAML / TOML metadata blocks)

- **The block is rendered *verbatim*, and that is load-bearing rather than visual.** `parse_offsets::options_for` enables the matching metadata-block extension, so `---`-delimited YAML and `+++`-delimited TOML become `Block::MetadataBlock` instead of the thematic-break-plus-setext-H2 pair that made a Hugo/Jekyll/Obsidian file's keys the loudest element on the page (issue #34). `Renderer::render_metadata_block` emits **one rendered row per source line, every character in its source column** — rendering adds only color. That keeps the raw↔rendered column relation the identity function and the row count 1:1 with source lines, which the raw reveal, the gutter's inverse, and the scroll math all assume. A prettier treatment would owe every one of those a block-specific arm.
- **Four consumers must be told the mapping is the identity, because their *defaults* are wrong here in different ways.** `sub_lines_in_block` joins code blocks in the verbatim branch — a blank line inside frontmatter is data and renders a row, while the generic prose walk skips interior blanks and would drift the cursor up a row per blank. The overlay painter (`rendered_view::paint`) and the click mapper (`mouse_ops::coord`, via `LineMapping::Verbatim`) must not consult `InlineColMap`: it re-parses the line as Markdown, where a quoted YAML value picks up smart quotes and a `*` in a glob opens emphasis. And the cursor indicator's `visual_col` chain in `RenderedView` needs its metadata arm **ahead of the list arm**, for the same reason the code arm sits there: `  - tag` is a YAML sequence entry and the list-marker *text sniff* would shift the indicator. (`coord`'s list branch is AST-gated, so only the sniffing sites need the ordering.)
- **The delimiter lines are reproduced from `MetadataKind`, not stored.** pulldown-cmark hands the AST builder the inner text only — the delimiters live in the event *ranges*, which `parse_blocks` doesn't see. So a YAML block closed with `...` (legal, and what `scan_closing_metadata_block` accepts) or a delimiter carrying trailing spaces renders as a plain `---`. Both keep the same three columns, so no column mapping is disturbed, and entering the block reveals the true source.
- **The key/value split is cosmetic and deliberately shallow.** `metadata_key_end` finds the first `:` / `=` followed by a space or end-of-line, requires a non-empty key, and otherwise declines — so `- https://example.com` doesn't split at the scheme's colon. It must not become a YAML or TOML parse: the two spans always concatenate back to the source line byte for byte, which is what makes the identity column mapping true.
- **The export path applies the same gate.** `export::html` keeps its own base option list, and without the metadata half an exported file reproduces the exact misparse the renderer no longer has. pulldown-cmark's HTML writer treats a metadata block as non-writing, which also gives the wanted behavior — frontmatter describes the document rather than belonging to its body, so it is omitted.
- **The extensions are enabled per *document*, gated on its first line.** With them on unconditionally, pulldown-cmark claims *any* later `---` line followed by non-blank text and eventually closed by another `---`, so `Intro.\n\n---\n## Section 2\n\nText.\n\n---\n## Section 3` parses the middle section as metadata. That is an ordinary separator style (a rule above a heading, a reveal.js / Marp slide break), and the damage is not cosmetic — the section renders as dim key/value data, `block_allows_inline_markdown_at` refuses emphasis inside it, and pulldown-cmark's HTML writer emits *nothing* for it, so the export silently drops content. `parse_offsets::options_for` therefore enables the extension matching the source's own first line and only then, and only that flavor. Every parse of a document — AST, offset scan, HTML export — must pass *that document's* text through it, or the 1:1 blocks↔ranges pairing breaks; `export::html` takes the metadata half from the shared `metadata_options_for`.
- **A `---` is still a rule almost everywhere.** Every `---` past the first line is one unconditionally. Even the opening one is only frontmatter when the delimiter run is exactly three characters, the line below is non-blank, and a closing delimiter exists — an unclosed `---`, a `---` followed by a blank line, and an empty `---`/`---` pair all stay thematic breaks. `edit_ops`'s inline-snippet gate excludes the block: emphasis or a link inside frontmatter is data corruption, not formatting.

### Blockquotes

- **The quote's style is a *base*, never a replacement.** `render_blockquote` renders its inner blocks first and then prefixes each line with the `▎ ` bar, so every span arriving there has already resolved its own style. It used to overwrite all of them with `blockquote_text`, silencing bold, italic, code spans, highlights, strikethrough and link color inside any quote (issue #33). Each span is now `base.patch(span.style)` and the inner block's own line style (a nested code block's surface) layers into `base` first, so it still wins over the quote. A new decoration wrapped around already-rendered lines owes the same shape.
- **`blockquote_text` is a background wash, not a text attribute, and that is what makes emphasis expressible.** A blanket ITALIC leaves `*emphasis*` inside a quote with nothing to say, and reads as a claim about the quoted prose's tone. The wash marks the region the way the code surface does. It is `secondary` mixed almost all the way to `bg` (`QUOTE_BG_MIX_TOWARD_BG`) — the same `blend` no-op for non-RGB palettes as `code_bg`, so `dark_256` / `light_256` pin it by hand, and `Monochrome Dark` uses DIM instead.
- **The wash is carried by the *line* style, not by padding.** `line_render` fills a row's trailing cells with `Line::style`, which extends the wash to the viewport edge and fills the indent zone of a wrapped continuation row (the bar itself is repainted there by `leading_bar_prefix`). Don't pad quote rows out to `viewport_width` the way `render_code_block` does — that predates the fill and would break the 1:1 raw↔rendered column relation.
- **The revealed row keeps the wash.** `RenderedView` resolves a `reveal_base` from the cursor block's AST kind and hands it to `paint::make_raw_line_over`, the generalized `make_raw_line_with_selection`. Without it the one row being edited drops out of the block it visibly belongs to on every reveal — the same reason a revealed mermaid body row gets `make_code_styled_body_line`. A new block kind painting its own surface owes an arm there.

### Syntax highlighting

- **The language comes only from the fence's info string, and the lookup takes only its first token.** There is no auto-detection and none is planned: guessing wrong recolors a document the author never asked to have recolored. `language_token` splits on `,`, whitespace and `{` so `rust,ignore` (rustdoc) and `js {1,3-4}` (line-highlight hints) still resolve, and the lookup is `find_syntax_by_token` — syntect's name/alias index — never `find_syntax_by_extension`, because a fence info string is a language name, not a path. The ` lang ` label row keeps rendering the **whole** info string verbatim.
- **Token ranges are char indices, and converting them is the module's main job.** syntect reports *byte* offsets; every column map in this crate is char-indexed (`code_layout`, `InlineColMap`, `line_render`). `highlight.rs` converts once, at its API boundary, and a byte offset never escapes it — letting one out puts every token boundary after a non-ASCII character in the wrong column (issue #28's failure mode through a new door). `ranges_are_char_indices_not_byte_offsets` pins it against a `"héllo"` literal.
- **`TokenClass` has no `Default` variant, and that omission is load-bearing.** Unclassified text produces no token at all, so "the grammar had nothing to say", "this language is unknown", "the block is over cap" and "the setting is off" are one path downstream: `code_body_row` with no tokens, reproducing the pre-feature single-span line character for character. That equivalence lets the existing code-block snapshots stand as the regression guard (`an_unknown_language_renders_exactly_like_highlighting_off` pins it); a `Default` class would fork the plain path into two implementations that can drift.
- **Classification walks the scope stack innermost-outward, and `storage` is a keyword.** A string's delimiter carries `punctuation.definition.string.begin`, which no rule claims, so the walk must fall *outward* to the enclosing `string.quoted.double`; checking outermost-first would let a broad scope swallow the specific one inside it. All of `storage` maps to `Keyword`, not just `storage.modifier`: TextMate uses `storage.type` for the keyword that *declares* something (Rust's `fn` is `storage.type.function.rust`, as are C's `int` and JavaScript's `var`) while an actual type *name* is `entity.name.type` or `support.type`, so mapping `storage.type` to `Type` colors `fn` as if it named one.
- **Three caps, and none is redundant.** `MAX_HIGHLIGHT_SOURCE_BYTES` (64 KiB) bounds the *cold* parse; `MAX_HIGHLIGHT_LINE_CHARS` (2 000) bounds the *per-keystroke* cost, which the byte cap cannot — incremental reuse works by finding an unchanged prefix, and a one-line block has none, so every keystroke in a minified line pays a full re-parse. Both were sized against the module's `#[ignore]`d `throughput` test, and both bound **color, never content**: over either cap the block still renders every byte, because mermaid's refuse-to-render posture would be wrong for code a user must be able to read. Tokenizing is the one content-handling path that runs *synchronously on the render thread*, so it is also wrapped in `catch_unwind`.
- **The third cap is `MAX_HIGHLIGHT_GRAMMARS` (24), because grammar *compilation* is not a function of block size.** syntect compiles a grammar's regexes lazily, on first use of that language (~9 ms typical, ~18 ms for Rust's), so a document of fifty one-line fences in fifty languages passes both other caps and still costs ~430 ms. That work is on a worker, so the cap bounds queued background CPU and queue memory (the queue holds an owned copy of each block, itself bounded by `MAX_HIGHLIGHT_SOURCE_BYTES` — ~13 MB and two seconds of a core for a document naming every language we ship). Past the budget a language renders plain, reusing the unknown-language path rather than adding a fourth degradation.
- **It bounds a burst, not the process lifetime.** A flat lifetime counter locked a long session out permanently, and LRU eviction is exactly backwards: syntect keeps a compiled grammar's regexes in a `once_cell::sync::OnceCell` inside the shared `SyntaxSet` for the process's life, so re-using one is *free* and evicting its record refuses a free grammar to admit a paid one — and an adversarial fifty-language document would evict its way through all fifty in one render anyway. So `GrammarBudget` splits the two questions the flat counter conflated: `warm` (already paid for?) only ever grows, bounded by the 213 grammars that exist; `budget` (affordable *now*?) is a token bucket refilling one slot per `GRAMMAR_BUDGET_REFILL` and saturating at the cap, so a long idle can't bank slots. `refill` advances `last_refill` by the intervals actually consumed rather than to `now`, or a document re-rendering faster than the interval never earns a slot. A third state, `pending`, stops the render thread re-queueing the same cold grammar every reparse. The rule is `GrammarBudget::admit` with the clock and cap injected, so tests never touch the process-global `GRAMMARS`.
- **A refusal is also an event, and `refused_grammar_retry_due` is what makes the burst bound true in practice.** The only thing that re-asks `admit` about a refused block is a re-render, which for highlighting means `warm_generation` moving — and the queued burst compiles far inside one `GRAMMAR_BUDGET_REFILL`, so once it lands the generation stops moving while the refusals still stand: the budget refills into a bucket nobody asks, and a document over the cap stays partly plain for the session unless the user types or resizes. `GrammarBudget::refused` therefore gives `tick_syntax_warm` a second reason to reparse, **edge-triggered** — `take_retry` consumes the answer, so a standing refusal costs one reparse per refilled slot rather than one per 60 ms tick, and a document at twice the cap colors itself a language per interval.
  - **That reparse only reaches the highlighter because `refused_grammar_retry_due` also bumps `highlight::retry_epoch`, which rides in `RenderSettings` beside `warm_generation`.** The second counter's absence is invisible: a retry is granted *precisely* when no grammar warmed, so neither the generation nor any `Block` value has changed, `RenderCache::begin_build` clears only on a differing fingerprint, every block hits the cache, `admit` is never re-asked, and `refused` has already been consumed by the asking — leaving the cap a session limit again, with every `GrammarBudget` unit test still passing (they drive a bare budget and never go through `refresh_parsed`). `render_cache::tests::both_highlight_counters_clear_the_cache` pins it.
- **Parsing is synchronous and compiling is not, and the asymmetry is the whole design.** Tokenizing stays on the render thread because highlighting changes only *color*, on text already on screen: any deferral must paint something during the gap, and both options are bad — plain text drops the colors out and back on every keystroke, and stale tokens are char ranges, so an insert shifts every color on the line being looked at. Incremental reuse makes the steady-state cost one line's parse, which buys the right to stay synchronous. **A debounce was rejected on that same argument**, plus one of its own: `"`, `` ` ``, `/*` and `#` reclassify the rest of a block in one keystroke, so a heuristic waiting for whitespace or `()[].` holds the stale render exactly when it is most wrong.
- **Compilation is the exception because it happens once per language, not once per keystroke**, so deferring it costs a frame or two of plain text and then never again. `spawn_warm_worker` owns it; a cold grammar returns `Admission::Queue`, the block renders plain, and the worker replays *the block's own lines* — not invented sample text, because syntect compiles per **match pattern** and there is no public API to force a whole syntax, so replaying the real block compiles exactly the patterns the render thread is about to need. Editing the block can still reach a pattern the warm parse missed, compiling one regex inline; that residual is accepted. A warm parse that panics leaves the grammar `pending` forever: plain, rather than retried into the same panic every frame.
- **`warm_generation` is how a plain block ever becomes colored, and it must ride in `RenderSettings`.** Warming changes no `Block` value, so `RenderCache` would serve the plain render it memoised while the grammar was cold for the life of the document. `App::tick_syntax_warm` polls the counter on the existing 60 ms loop tick — polled rather than pushed because `markdown` sits far below `app` and must not learn about `AppEvent`. `App::syntax_warm_generation` is seeded from the live counter rather than 0 (a second `App` in one process — the test suite — must not see a spurious first-tick change), and that read is taken **before** `spawn_warm_worker` and `configure_new_editor`, not at the struct literal that stores it: the constructor's own first render queues this document's grammars, so a later read can capture a landed compile *as* the starting value, after which the block stays plain until some unrelated reparse — which a read-only viewing session never has. The renderer reads the counter itself rather than having it threaded in from `EditorState`, and pins it to 0 when the feature is off so toggling can't leave a stale generation in the fingerprint.
- **Asynchronous compilation makes highlighting eventually-consistent, which tests must opt out of.** A bare `highlight_block` on a cold grammar answers `[]`, so a color assertion either races the worker or — worse — passes because an *unrelated* test warmed that grammar first, making the suite order-dependent. `warm_inline` is the escape hatch: it marks a grammar usable immediately, compiling inline on the calling thread. Every classification test goes through the `hl` helper, and both `render_highlighted` helpers call `warm_fence_languages` first. Production code must never call it on the render thread — inline compilation is the stall the worker exists to remove.
- **Incremental reuse is two-sided, and the convergence check is what makes it correct rather than merely fast.** The cache holds, per block, the `(ParseState, ScopeStack)` at the *start* of each line plus a tail entry for the state after the last one — without the tail, appending at the end of a block could not resume. Lines before the first change carry over directly; for lines after it, the parser position is compared against the cached position at the matching line (re-based, since an edit can add or remove lines), and once the two are equal *and* the remaining text is unchanged, the rest carries over too. **Both halves of the position are needed** — `ParseState` alone omits the scope stack, which is accumulated by applying each line's ops, so resuming with a fresh stack misclassifies everything below the edit. The invariant the tests assert is not "reuse happened" but "the incremental result equals a cold parse"; `reuse_is_actually_happening` guards the other direction, so a cache degrading into a full re-parse can't leave every correctness test passing.
- **`syntax_*` theme styles are patched over `code_block_text`, never used alone.** Each sets a foreground only; the code surface owns the background, and a syntax style setting its own `bg` would paint a stale one wherever a theme moved that surface (`syntax_styles_carry_no_background_of_their_own` pins it). All seven derive from existing `Palette` slots, so every built-in theme gets a coherent set with no per-theme authoring; only `monochrome_dark` needs hand-written entries (an exhaustive struct literal that won't compile without them), and it marks only `Keyword` and `Comment` since with no color to spend, attributing everything distinguishes nothing.
- **The seven slots they derive from must be *foreground* slots, and the derivation is contrast-checked.** A palette's slots split into ones that carry text (`primary`, `text_muted`, `link`, `success`, `warning`, `error`, `code`) and ones that are fills or chrome (`secondary`, `accent`, the `bg` / `surface` family); only the first group has ever had to be legible as characters. `syntax_type` was `secondary` and `syntax_function` was `accent` — the slot `dark_256`'s own comment calls "selection bg, table header" — measuring **1.99:1 and 1.51:1** against that theme's code surface against 6.97:1 for the plain code text they replaced: highlighting made code *less* readable, on the very theme `theme_fallback::apply` substitutes into below truecolor. They are now `code` (already required to be legible on this exact surface, since `code_span` paints it there) and `link`, with `error` taking `attribute`. `themes::util::legible_on` is the backstop for the rest, lifting each color toward `text` until it clears `SYNTAX_MIN_CONTRAST` (4.5:1, the body-text threshold — a code token is read character by character), keeping its hue.
- **`legible_on` is a no-op for indexed colors, so `dark_256` / `light_256` pin all seven by hand** — the same `blend`-is-a-no-op reason those two files already pin the heading ramp, `code_bg`, `selection_muted` and the diff washes. Their picks are the bright- (or dark-) tint siblings of the slots the RGB derivation would have used, because the mid shades those slots hold are chosen to read on `bg`, not on the code surface; `light_256` diverges further, since the 256 cube offers no dark orange clearing the floor, so `keyword` takes the deep red light editor themes conventionally give it and `attribute` moves to teal. `syntax_contrast_clears_the_floor_for_every_builtin_theme` holds all 27 themes to account, indexed ones included — it resolves the xterm cube itself, and treats an ANSI 0–15 index as a failure rather than a skip, since those have no fixed value and would otherwise be a way to opt out. Its floor is `min(SYNTAX_MIN_CONTRAST, plain code text)`, not a flat 4.5: a theme cannot make a token more legible than its own body text (Solarized Light's is 4.49:1), and the promise that matters is that highlighting never makes a block *less* readable than leaving it plain.
- **The mermaid source reveal is deliberately out of scope, not merely unfinished.** `make_code_styled_body_line`'s one call site always has the language `"mermaid"`, and no Mermaid grammar exists in either syntax set, so threading tokens through it would be provably inert code. `mermaid_has_no_grammar_so_that_surface_stays_out_of_scope` pins the reason, and starts failing if that stops being true.
- **`syntax_highlighting` must be in `RenderSettings`.** The `Block` value is unchanged by the toggle, so without the fingerprint field a cached hit repaints with the old setting — the trap `render_cache`'s module doc names. It reaches `EditorState` through `app::configure_new_editor`, like `big_h1` and `cursor_blink`, so a document opened by following a link gets it too.

### Footnote reference markers

- **The marker is plain ASCII, and that is a constraint rather than a style choice.** `renderer::reference_marker` renders `[^label]` as `[label]`. It was once superscript — `⁽¹⁾`, from U+207D/U+207E — and those codepoints are missing from most monospace fonts (JetBrains Mono, ghostty's default, among them). A terminal falling back to a proportional face draws the parenthesis wider than the cell, and ghostty only shrinks an oversized glyph for a curated codepoint list; outside it the glyph spills over the digit. iTerm2 and Terminal.app pick a different fallback and clip instead, which made the bug look terminal-specific rather than font-specific. `footnote_marker_is_plain_ascii` pins it. The superscript digits carried a second problem: `¹²³⁴` are East Asian Width **Ambiguous** while `⁰⁵⁶⁷⁸⁹` are Neutral, so a terminal treating ambiguous as wide measured footnotes 1–4 a column wider than edamame did. A marker glyph outside Basic Latin has to be justified against font coverage *and* width class.
- **Adjacent references fuse into one marker, and three places must agree on how.** `[^1][^2]` renders `[1,2]`, not `[1][2]`. `renderer::footnote_run_at` is the single derivation of a run's marker; `render_inlines` and `rendered_inlines_char_width` both consume it so painted marker and measured width can't drift, and `render_paragraph` splits at breaks and calls back through `render_inlines` rather than walking inlines itself. "Adjacent" means adjacent *inlines*, so `[^1] [^2]` stays two markers.
- **`InlineColMap` collapses a fused run by two chars per abutting reference, not one.** A lone `[^1]` → `[1]` differs from its source by exactly the dropped `^`, hence `collapse_footnote_refs` being a single `retain`. A fused run additionally drops the second reference's `[`; the entry surviving at that position is the *previous* reference's `]`, which becomes the rendered comma. Drop only the carets and the map runs one column long per fused pair, so every selection projection and click past it lands short — `adjacent_footnote_references_collapse_into_one_marker` and `click_inside_a_fused_marker_follows_the_label_clicked` pin both halves.

### Search and replace

- **The search flow is gated on `EditorState::search.is_some()`, not a `Mode` variant.** Unlike diff (which replaces the whole view), search keeps the document rendering in the current view mode with match highlights on top, so a `Mode::Search` would force an "effective view mode" indirection through every render-dispatch site.
- **Only a *replace* flow captures input; a navigate-only flow is a non-capturing overlay.** `App::search_flow_captures()` is `search.as_ref().is_some_and(|s| s.is_replace_flow())`, in vim and default mode alike. A **replace** flow needs the unmodified `Tab`/`r`/`a` flow keys, so it traps input at three choke points — `DefaultHandler::handle` intercepts the hard-bound flow keys (`search::search_keys`, same table-driven pattern as `diff_keys`), `App::dispatch_action` default-denies everything off the `search_safe_action` allowlist *before* `handle_app_action` runs, and `dispatch_mouse_event` drops all mouse input except wheel scroll and pointer moves. A denied action flashes "Not available during search" via `App::flash_action_unavailable`. `search_safe_action` allows the flow keys, read-only navigation (cursor moves, selection, `SelectAll`, `Copy`) and the always-safe set (scroll, overlay openers, save, quit, in-flow undo/redo); a new buffer-mutating app-level action is denied automatically. A **navigate-only** flow (vim's `/`, `Ctrl-F` find with the replace field empty) does *not* capture: it is a lightweight highlight overlay (vim `hlsearch` / VS Code find widget), and only `Tab`/`Shift+Tab` (plus vim's `n`/`N`) and `Esc` are intercepted ahead of the keymap — `DefaultHandler` returns only `SearchNext`/`SearchPrev`/`SearchExit`, `dispatch_action` routes just those to `dispatch_search_action`, and everything else falls through to normal editing.
- **A non-capturing flow's match list is refreshed every frame.** Because it lets the buffer be edited outside the in-flow mutation paths, `prepare_viewport` calls `EditorState::ensure_search_fresh` each frame (version-guarded → a no-op when nothing changed) so the overlay painter and focus-scroll see live ranges.
- **Search exit is a motion — no scroll-back.** `EditorState::exit_search` just drops the session (`self.search = None`); it leaves the cursor and viewport on the match the user navigated to, matching vim's `/` and the VS Code find widget.
- **Vim `/` `?` search is incremental (incsearch).** While the prompt is open, `vim_ops::incsearch::update_incsearch` rebuilds a real navigate-only `SearchState` from the input on every keystroke (typed, history-recalled, or pasted — all three route through `feed::cmdline_live_update`, shared with the `:s` preview), parks the cursor on the cursor-relative focus (`SearchState::focus_relative_to`, the same method `App::enter_vim_search` uses on submit), and scrolls it into view. Because the transient session *is* `EditorState::search`, the hlsearch painters, hint-line counter, and raw-reveal suppression work unchanged — no incsearch-specific render code exists. The `IncsearchSession` on `VimState` stashes the pre-prompt cursor/scroll and any prior hlsearch session; Esc restores all three, and Enter restores them *before* the `EnterSearch` outcome so the App-level submit resolves against the original cursor, byte-identical to a preview-less submit. Unlike the `:s` preview, incsearch never touches the buffer. The shared view primitives — `EditorState::place_cursor`, `restore_view`, `scroll_cursor_comfortably_into_view` (one TOP_MARGIN core behind the hunk/match focus scrolls too) — are the single implementations; don't hand-roll a cursor park or a context-margin scroll.
- **Match freshness is version-keyed.** `SearchState` stores byte ranges valid for the `Buffer::version()` they were computed against; every in-flow mutation path (replace, replace-all, undo, redo) calls `EditorState::ensure_search_fresh` afterwards. The render layer additionally clamps each range against the live source so a stale list can never panic — but don't rely on that. Wholesale content swaps (`replace_buffer`, diff entry) drop the session entirely.
- **Matching is smartcase for navigation, case-sensitive for replace.** `SearchState::ensure_fresh` picks the matcher by `is_replace_flow()`: a navigate flow uses `search::state::find_all` (smartcase — case-insensitive unless the pattern contains an uppercase char), so *every* edamame user gets smartcase, not just vim; a replace flow uses `find_all_cs` (always case-sensitive) so a lowercase find term never rewrites a casing variant the user didn't type. `find_all_cs` keeps `str::match_indices`; the case-insensitive path (`find_all_ci`) compares char-by-char against the **untouched** haystack so returned byte offsets stay on char boundaries for multibyte text (lowercasing up front would shift offsets, and the overlay painter slices the source by those offsets). There is deliberately **no regex** in `/` search; regex is confined to `:s`/`:%s`.
- **The query is escape syntax, so `query` and `needle` are different strings.** Search stays literal-substring, but a single-row text field can't hold a line break — so `search::escape` gives the query vim's backslash convention (`\n` `\t` `\r` `\\`), and `SearchState::new` splits the input into the typed `query` (kept verbatim for *display*: modal prefill, "no matches" flash, incsearch prompt) and the decoded `needle` (what `ensure_fresh` matches). `replace` / `replacement` are the same pair. **Match with `needle`, display `query`** — swapping them either searches for a literal `\n` or carries a raw newline into a one-line surface. Because a backslash always starts an escape, an unrecognized one is a `SearchError`, not a silent literal: `SearchState::new` returns `Result`, the modal reports it in its own error row with focus on the offending field, `enter_vim_search` flashes it, and incsearch silently shows nothing (mid-typing, every prefix of a valid query is briefly invalid). **Text edamame supplies rather than the user types must go through `escape::escape` first** — `feed::search_word_outcome` (`*` / `#`), `search_modal::paste`, and `cmdline::paste_str` on a `/` `?` prompt (`CmdLineKind::is_search`; an `:` prompt is an ex command and keeps the plain newline strip).
- **A match may span a line break, so no highlight consumer may assume one range sits on one line.** This is a standing constraint on three painters: `RawView` clips every range (search, `:s` preview, yank flash) to the line being painted through the shared `push_clipped`; `paint_byte_range_overlay`'s per-raw-line intersection does the same for Rendered / Preview; and `paint_search_overlays` re-selects overlapping matches per *block* so a match straddling two blocks paints in both.
- **Replace keeps the reveal beat.** A single replace goes through `EditorState::apply_delta` (one undo delta) plus an immediate `flush_parsed_if_dirty` — the overlays and match recompute need fresh source-map ranges on the next frame. It then refocuses past the inserted bytes (so a replacement containing the query can't trap the flow on one site) and arms `search_advance` (mirror of `diff_advance`) so the cursor jumps to the next match only after a 350 ms reveal. Replace-all is a single coarse `EditDelta` on the normal history stack — prior undo history is preserved, unlike the diff merge's `reset_with`.
- **A replace flow leaves Preview.** Preview is browse-only, so `App::enter_search_flow` transitions Preview → Rendered when the replace field was filled. Navigate-only flows, and zero-match queries that never enter the flow, leave the mode untouched.
- **Raw reveal is suppressed during search**, so blocks don't flip between rendered and raw under the highlights as the user tabs through matches.
- **Highlight painting is shared.** Rendered + Preview matches paint through `paint_search_overlays` → `paint_byte_range_overlay` (the generalized former `paint_selection_overlay`) called from `EditorView` as a post-pass; Raw mode paints per-char inside `RawView`. The focused match uses `theme.selection`, all others `theme.selection_muted`. The painter's block-kind prefix shifts (heading space prefix, code-block pad cell) resolve the block via `ParsedDoc::real_block_for_byte` — never index `parsed.blocks` with a `source_map` block index; the source map's index space counts blank-line virtual blocks, so the two diverge in any document with blank lines.

### Live `:s` substitution preview (vim `inccommand`)

- **The preview transiently rewrites the real buffer — through raw `Buffer` edits only.** While the vim `:` command line holds a complete-enough `:s`/`:%s`/`:'<,'>s`, `vim_ops::preview::update_substitute_preview` applies the substitution via raw `Buffer::insert`/`remove` (never `EditorState::apply_delta`), so no undo delta is recorded and `dirty` is untouched. Every keystroke reverts the previous preview and recomputes against the pristine buffer — never diff two previews. On Enter the reducer reverts *before* `submit_ex`, so `execute_substitute` runs against the untouched buffer and commit semantics stay byte-identical to a preview-less submit. `SubstitutePreview` lives on `EditorState` (like `search` / `yank_flash`) so the painters read it off `&EditorState`.
- **The revert delta is version-stamped as a fail-safe.** The stashed inverse `EditDelta` carries the `Buffer::version()` it was applied at; a revert on a mismatched version silently drops the preview instead of corrupting text. `replace_buffer` (external reload) also drops any preview. Don't rely on the stamp: any new mutation path reachable while the cmdline is open must be gated.
- **Three gates hold while `substitute_preview.is_some()`.** (1) `tick_autosave` / `autosave_deadline` skip entirely (same pattern as the diff-mode guard) — raw preview edits bump `version` without touching `dirty`, so on an already-dirty buffer the debounce would write preview text to disk. (2) `dispatch_mouse_event` blocks everything but wheel scroll and pointer moves (shares the capturing-search gate) — a click or checkbox toggle would mutate text that is about to revert. (3) `prepare_viewport` skips `ensure_search_fresh`, and both search-overlay painters early-out — a coexisting hlsearch session's byte ranges are stale against preview text; the session survives untouched and repaints after the revert. Keyboard needs no gate: the cmdline captures every key. Additionally `cursor_block_revealed()` returns false while a preview is active — the preview parks the cursor on the first affected line, and the reveal delay would elapse mid-typing, flipping that block to raw source under the highlights.
- **A `:s` pattern sees the whole resolved range at once, not one line at a time.** `ex::region_haystack` materializes lines `first..=last` as one string and `ex::for_each_region_match` is the *single* match-finding implementation driving the commit path, the replacement preview, and the highlight-only preview — which is what makes "what the preview highlights" and "what Enter replaces" the same set by construction. Four load-bearing details: (1) **`multi_line(true)` at both compile sites** (`execute_substitute` and `compute_preview_plan`) keeps `^`/`$` anchoring per line — setting it on one and not the other makes the preview lie; `dot_matches_new_line` stays off so `.` still refuses to cross a break, as in vim. (2) **The region excludes the last line's own break**, which is the entire enforcement of the range bound — a `\n` pattern can never reach past the named lines. That costs one deliberate divergence from vim (a single-line `:s/\n//` has no break to match, so it can't join with the next line); `:%s` resolves `last` to ropey's phantom line past the trailing newline, so it *can* consume the file's final newline. (3) **The non-`g` walk replaces the first match *starting on* each line**, resuming at the line after the last line the match covered — with a correction for a match ending exactly at a line start (any pattern ending in `\n`), without which every other line would be skipped. (4) **`match_cap` stops on a match boundary, not a line boundary**, so the preview's `removed` stays a verbatim prefix of the region. Don't reintroduce a per-line `substitute_line`.
- **Compute is pure and shared with the commit path.** `ex::build_substitution` (the extracted region walk of `execute_substitute`) produces the single `EditDelta` plus the post-apply byte ranges of each inserted segment; `preview::compute_preview_plan` is a pure function of `(Buffer, cursor_line, Substitution, visual_range)` — the unit-test seam. `Substitution::replacement_present` (did the user type the second delimiter?) distinguishes highlight-only `:%s/foo` (match ranges, first per line without `g`, no edit) from deletion preview `:%s/foo/` (edit applied, zero-width highlight ranges filtered out).
- **The preview regex is bounded; the commit regex is not.** The preview builds its `fancy-regex` with `backtrack_limit(100_000)` and caps the walk at 1 000 matches (later lines stay original until submit) so a pathological half-typed pattern (`(a+)+b`) fails fast per keystroke. Parse/regex errors and matchless patterns silently end the preview session — never flash an error mid-typing. Painting reuses the search walk: `paint_substitute_preview_overlays` in Rendered/Preview, an inline branch in `RawView`, all ranges in the single `theme.selection` style (no focus concept, matching nvim's one `Substitute` group).

### Vim commands inside a table

A rendered GFM table's `|` delimiters and alignment row are auto-managed chrome, not prose, so the vim layer treats the *cell* as the unit a motion moves within and the *row* as the unit a line command acts on. All of it lives in `editor::vim_ops::table`.

- **Raw mode is exempt, and gets that for free.** Every query funnels through `table_edit_ops::current_table`, which returns `None` in `Mode::Raw` — raw is hand-editable source, so no call site needs its own mode check.
- **The scoped resolvers replace the bare ones at the input layer.** `resolve_scoped_motion` / `resolve_scoped_op_range` stand in for `motion::resolve_motion` / `resolve_motion_range` throughout `feed.rs`, rather than each call site clamping the result, so a new operator target can't forget the clamp. `motion.rs` itself stays pure and buffer-only. The check is `rg 'resolve_motion(_range)?\(' src/input/vim/feed.rs`, which should match nothing. `;` / `,` resolve through `resolve_find_repeat` and so apply `scope_offset` by hand — the one exception.
- **Clamping shapes the keystroke; `range_breaks_a_table` is the safety net.** Do not rely on the cell clamp to protect the table: a range reaches a header or alignment row by plenty of routes with no cell to clamp against (`2dd`, `dj`, a VisualLine selection whose cursor has left the table, a charwise selection carried into the row below by `j`, `Vp`, Visual `r`). So the guard is asked of the **range**, immediately before the mutation, at the two funnels every vim range edit passes through — `run_operator` and `run_visual_operator` — plus the paste and replace sites. A new vim mutation path must call it too. `Yank` is never refused; it mutates nothing.
- **A charwise Visual highlight must cover only the cell's content — horizontally.** The span is inclusive of the char under the cursor, so in charwise Visual the *horizontal* clamp is one grapheme tighter than in Normal: `table::CellLimit::LastChar` (vs. `Append`) stops `$`/`w`/`f` on the cell's last character, and `h`/`l` route through `table::visual_cell_step` so they step *within* the cell. `feed.rs` derives both from the single predicate `is_charwise_visual`. Operator ranges keep `Append` (exclusive end — `D` must still clear the whole cell), and the guarantee is horizontal only: `j`/`k` and the deliberately unscoped document motions still leave the cell, which is what the range guard is for.
  - **The two halves of the clamp answer different failures.** Crossing a `|` promises an edit `range_breaks_a_table` refuses — there the clamp keeps the highlight honest. Reaching the append slot is *not* refused: `table_break` measures confinement against `Cell::content_end`, the **untrimmed** span between the pipes, while `CellScope::end` is trimmed past the last non-blank, so the padding space between them is inside the cell as far as the guard is concerned and the edit silently eats it. Cosmetic rather than structural — and one grapheme back is where vim's own `$` rests in Visual — but the clamp is the only thing preventing it.
- **Both doors into charwise Visual owe the same append-slot pull-back, on both ends of the span.** `enter_visual` (`v`) and `toggle_visual_mode` (`V`→`v`) each call `table::visual_endpoint_in_cell`, which resolves against the *endpoint's own* cell rather than the cursor's — `V`, `$`, `v` parks the cursor on the slot, and an `o` in between puts the anchor there instead, in a different cell. `v` needs only the cursor half; the toggle pulls cursor and anchor and then re-syncs `EditorState::selection`, which is the span the operators actually read (`visual_charwise_range`) and which still holds the pre-pull active end. A third entry into `VimSubMode::Visual` would owe all of it.
- **The guard refuses only what actually breaks.** Deleting a table *whole* is allowed (nothing is left to be broken), as is deleting complete data rows, an edit confined to one cell's content, and an edit confined to the alignment row's own text (that row stays hand-editable, which is why it has no cell scope). Everything else — a partial span over a protected row, a range crossing a `|` — is refused with the reason on the flash line.
- **`dd`/`cc`'s structural routing is a convenience, not the protection.** `table_doubled_operator` re-routes an uncounted `dd` onto `table_edit`'s row delete and `cc` onto a cell clear, both through `execute_operator`/`fold_op_result` so the register, single-delta undo, and Insert transition come from the existing implementation. Counted forms deliberately fall through to plain linewise — safe *because* the funnel guard catches them. `TableOpOutcome` keeps "in a table, refused" distinct from "not a table, fall through"; collapsing the two into an `Option` is what let `cc` blank the alignment row.
- **`p` picks a legal row boundary.** The ordinary linewise landing spot ("the line after the cursor's") is *between the header and the alignment row* when the cursor is on the header, so `table_paste_plan` clamps the target index below the alignment row and refuses a register that isn't table rows (or a charwise one carrying a `|`). Raw mode keeps the plain landing spot.
- **Visual `p` asks about the payload, not the register.** A Visual paste reconciles the register's shape with the *selection's*, so `run_visual_paste` builds `text` first and hands `paste_over_range_refusal` that string plus the selection's linewise flag. Passing `vim.register.{text,linewise}` instead lets both mismatches through, and each one broke the table: a `dd`'d row pasted over a charwise in-cell selection carried its `|` and newline into mid-cell, and a charwise prose register over a VisualLine row replaced that row with a line that is not a table row. A new paste path owes the guard the bytes it is actually about to write.
- **Only the table's own lines are read out of the rope.** `table_edit_ops::table_at` slices the contiguous run of table-looking lines around a byte offset rather than calling `Buffer::contents()` — `cell_scope` sits on the per-keystroke motion path, and materializing the whole document per `w`/`$`/`f` is what that avoids.

### The vim command line

- **Every way a paste can reach an open `/` `?` `:` prompt goes through `App::paste_into_cmdline`.** There are two and they used to land in different places: a terminal bracketed paste arrives as `Event::Paste` and was routed to the prompt, while edamame's own paste chord never fired at all — an open command line captures *every* key, so the global keymap never ran and `Action::Paste` was swallowed by `feed_cmdline` (issue #17). `dispatch_single_key` now resolves *that one action* against the live keymap ahead of the vim feed — against the keymap, not a hardcoded `Ctrl-V`, so a rebound paste key works — and routes it to the same method `dispatch_paste` uses. Everything else stays captured.
- **The prompt is a single line, so a paste is transformed and bounded — in that order.** `cmdline::paste_str` escapes first on a *search* prompt (`search::escape::escape`, so a pasted break survives as `\n`) and drops any break the transform didn't consume; an `:` prompt keeps the plain strip. The `PASTE_CHAR_CAP` bound is applied by `paste_into_cmdline` *before* handing the text over, and deliberately **not** by calling `ui::sanitize_paste`: that helper strips control characters, so it would eat the newlines the search escape exists to preserve. It is applied at the call site rather than inside `paste_str` because `input` sits below `ui` in the layer order. The cap is not cosmetic — `paste_str` inserts char by char through an O(n) `byte_index` scan, so an uncapped paste is quadratic and a 200 KB clipboard measured ~9 s of frozen UI. The chord path makes that worse than the bracketed one, because `edit_ops::clipboard_text` reads the OS clipboard whole.
- **A paste re-derives the live preview, exactly as typing does.** `paste_into_cmdline` calls `feed::cmdline_live_update` with the pre-paste input, so the `:s` substitution preview and incsearch resolve against the new line. Mutating `cl.input` without it leaves the preview describing text that is no longer in the prompt.

### Link following and deep links

- **A local link's `#fragment` is split off at classification time, not at dispatch.** `LinkTarget::LocalFile` carries `{ path, fragment }`, and `LinkTarget::parse` splits on the first `#` for both the bare-path and the `file://` forms. It has to happen there because *every* downstream question is asked of the path: with the fragment attached, `other.md#a-heading` has the extension `md#a-heading`, so `is_markdown_path` said no and the link went to `open::that`, which failed with the OS launcher's exit status (issue #38). A remote `Url` keeps its fragment inline — the browser wants the whole thing.
- **One resolver answers "which heading is this fragment", it matches the GFM slug exactly, and both link paths use it.** `App::heading_line_for_fragment` is a plain lookup in `ParsedDoc::heading_anchors`; the in-document `#anchor` jump (`scroll_to_heading`), the cross-file deep link (`navigate_to_file_at`) and the startup anchor all go through it. **Do not reintroduce a normalizing fallback.** It was briefly `.or_else(|| anchors.get(&gfm_slug(fragment)))`, so a hand-written `#Getting Started` resolved — which sounds harmless and is not: edamame is an editor, so documents written in it travel, and a fragment that resolves only here is a link the author ships broken to GitHub and every browser without ever seeing it fail locally. Strictness makes "works in edamame" mean "works". (It was also incoherent in both directions: a bare `[x](#Getting Started)` isn't a link at all — CommonMark forbids spaces in an unbracketed destination — while `#Getting%20Started`, which a browser produces, missed anyway because `gfm_slug` strips the `%` and keeps the digits.) `only_the_gfm_slug_resolves_a_fragment` pins the near misses.
- **A deep link records one nav entry, not two.** `navigate_to_file` already pushes the origin as a `NavDest::File`, so the heading jump that follows calls `scroll_to_rendered_line` directly and deliberately does *not* `record_in_doc_jump` — otherwise one `NavigateBack` would land the reader at the top of a document they never saw, and it would take two to get back to the link they clicked.
- **The fragment must survive the dirty guard.** Following a deep link out of a modified buffer routes through `DirtyGuardModal`, which carries the pending destination across the modal's lifetime; it carries the fragment alongside it (and through the save-as callback), or answering the guard opens the file at the top. The back/forward call sites pass `None` — a restored `NavEntry` has its own recorded scroll.
- **`navigate_to_file_at` owns the new document's viewport, so a caller must not re-assert cursor visibility on top of it.** That is why it returns whether the file loaded, and why the guard's Save and Discard arms skip their trailing `ensure_cursor_visible` on `true`: those calls correct the document the modal was *covering*. A freshly loaded `EditorState` starts in `Mode::Preview`, where `scroll_to_rendered_line` deliberately moves `scroll` without moving the cursor — so an `ensure_cursor_visible` behind the jump sees the cursor at byte 0 above the viewport and scrolls back to line 0, silently discarding the fragment. The un-guarded `follow_link` path was always correct because it makes no such call. `a_deep_link_answered_through_the_dirty_guard_still_lands_on_the_section` pins both buttons.
- **The command line splits its own `#section`, and does it outside the parser.** `edamame notes.md#setup` is the same deep link from the shell, but `#` is a legal character in a file name — so `cli::split_startup_anchor` asks the disk (the literal path wins; only a path that doesn't exist gives its `#` up) and therefore cannot live in `Invocation::parse`, which is pure and unit-tested without an environment. `main` calls it between argument parsing and `App::new`, and the injected-predicate inner function keeps the *rule* testable while the lookup isn't. It splits on the **last** `#`, so a directory carrying one (`notes#2024/index.md#intro`) still resolves.
- **The startup anchor is applied on the first frame, not in `App::new`.** The jump needs the document's live dimensions and nothing has measured a frame yet, so `App::with_startup_anchor` parks it and `prepare_viewport` consumes it through `apply_startup_anchor` — same shape as the update notice's park-and-tick. It clears itself, so a later frame can't yank a reader who has since scrolled away, and it records no nav entry.
- **A fragment naming no heading is reported, not swallowed.** The file opens and `App::flash` says the section wasn't found — the document did load, so silence would read as edamame ignoring the anchor. (The in-document `#anchor` path stays a silent no-op — nothing happened there at all.)

### Diff review

A clean-buffer external write opens **diff review** (`Mode::Diff`): the hunk list stacked old-above-new with a decision divider per hunk. Unchanged regions are painted as *rendered* Markdown; only changed regions drop to raw source. `src/diff/` owns the model, `ui::diff_view` the painting.

- **The rendered/raw split is a display partition, and it must never reshape `hunks`.** `layout::block_spans` partitions the new side by source-map block, marks the blocks a hunk lands in as `touched`, and `build_visual_lines_rendered` emits a maximal run of touched blocks as one *raw region* (today's stacked walk, restricted to that line range) and every other block as its pre-rendered rows. Snapping hunk ranges out to block boundaries instead would collapse the per-row table hunks `engine::split_table_hunk` produces back into one whole-table hunk — the row-by-row review is the feature. A display-only partition leaves `hunks`, `decisions`, `HunkId` stability, `reconcile_with_disk` and `resolved_rope` untouched.
- **Every source-map block gets a span, zero-row blocks included.** A block that renders nothing — a standalone HTML comment, a `<!-- tui-columns: […] -->` hint, a blank run collapsed by `preserve_blank_lines = false` — is still real and still carries source lines. Without a span, a hunk confined to it would mark nothing touched, fall inside no raw region, and its delete / decision / add rows would never be emitted: the user reviews a diff that does not contain the change while `all_resolved()` still lets them resolve it. Zero rendered rows is a property of a clean block's *emission*, never of its *existence*. Spans run from a block's own first line to the *next* block's first line rather than from its byte range (pulldown-cmark ranges absorb trailing blanks that already have virtual blocks), and the last runs to `len_lines()` — the phantom line included — so the set is a total partition. A `debug_assert!` pins both that and "every hunk emits exactly one decision divider"; a hunk emitted twice would paint two dividers for one decision, and the second would disagree with the first the moment the user presses `y`.
- **A new side that parses to *no blocks* falls back to the raw walk.** Reachable only when the file was truncated to empty on disk (`> notes.md`, a failed save, a partial sync), which the watcher hands straight through. With an empty partition the whole-document delete has no span to be emitted against, so in release the review is *blank* with `all_resolved()` false — `Esc` refuses to finish a change the user was never shown. The raw walk has no such gap.
- **`DiffState::parsed_new` is not stamped against `new_buffer`, so the one call that replaces the buffer drops the parse.** `build_visual_lines_rendered` reads source lines out of the parse while taking the document's line count from the buffer, and nothing pairs the two. `DiffState` holds no theme and no width, so it cannot rebuild — `reconcile_with_disk` therefore calls `set_rendered_parse(None)` and lets its caller (`App::reconcile_diff_with_disk`) reinstall a fresh one via `EditorState::refresh_diff_parse`. A caller that forgets loses the rendered presentation until `refresh_parsed`'s tail call comes round; it never gets a review partitioned against stale line ranges. Don't replace that drop with an explicit rebuild at the call site alone — the point is that the *unsafe* state is unrepresentable, not merely unreached.
- **The initial build is deferred by one frame, on purpose.** `enter_diff_mode` sets `EditorState::diff_parse_dirty` instead of building, because at that moment `viewport_width` still holds the *editor* mode's document width — which reserves a line-number gutter that `Mode::Diff` does not (`compute_doc_dims`) — so a parse built there lays every table out against a width the diff view never paints at. `App::prepare_viewport` calls `flush_diff_parse_if_dirty` immediately after posting the real width. The review is queried in the raw state at least once per entry, which is why `parsed_new: None` is a live path and not just a constructor artifact.
- **Every later rebuild rides `refresh_parsed`'s tail, not a hand-maintained list of sites.** Everything that re-renders the document — a theme switch, a width change, big-H1 / striping toggles, an images-or-diagrams settings change, an arriving image decode — goes through `refresh_parsed`, and `OpenSettings` / `SwitchTheme` / `CreateCustomTheme` are all on the `diff_safe_action` allowlist, so several are reachable *during* a review. A stale `parsed_new` would then disagree with the row cache about block heights. `refresh_diff_parse` never calls back into `refresh_parsed`, so there is no recursion, and it guards on `self.diff.is_some()` so outside a review the tail call is one branch.
- **The diff parse gets its own `RenderCache`.** `RenderCache` is keyed by `Block` value plus a render-settings fingerprint, so two documents sharing one cache collide on every identical block, and eviction is by document membership per build — each parse would evict the other's entries wholesale. `EditorState::diff_render_cache` pays for itself on the first terminal-resize drag, which posts a new width nearly every frame.
- **`refresh_parsed`'s image-GC live set is the *union* of both parses.** A URL on the diff's new side but not in the editor's buffer — a diagram whose source changed on disk, an image the user is about to accept — would otherwise be evicted there and immediately re-requested by the diff-side dispatch: a decode/evict loop that runs for the length of the review. `refresh_diff_parse` runs no GC of its own for the mirror-image reason.
- **In diff mode `scroll` counts *diff* visual rows, so the image paths need diff-specific geometry — through one memo.** `image_dispatch::infos_in_diff_viewport_window` (decode dispatch) and `image_view::build_diff_snapshots` (placement) both read the rendered-row map via `DiffState::with_layout_index`, which memoises it on the layout cache beside `lines` and drops it with them. Rebuilding it per call was O(rendered rows) plus a `HashMap` allocation at the frame cadence, because dispatch runs from `prepare_viewport`. Dispatch still early-outs on an image-free new side so such a review never builds the map at all. An image inside a *changed* region has no `ContextRendered` row, yields no snapshot and is never dispatched: it shows as `![alt](url)` source, which is also what keeps the media prompts honest mid-review — a clean-region URL is by definition one the editor's own document already carries. Without the dispatch, an unchanged image not yet decoded when the review opened would reserve `image_max_height` blank rows for the whole review and never decode, since `ImageCache::reserved_rows` returns `None` for an unknown URL and nothing else would request it.
- **`ROW_CACHE_CAP` is 2 because exactly two widths are queried per frame** — the scrollbar-decide width and the post-scrollbar display width. Every new per-frame `with_layout` / `with_layout_index` caller must use one of those two (the image paths use the display width, `last_doc_width`); a third distinct value turns the LRU into a full prefix-sum rebuild on every frame.
- **A `ContextRendered` row carries no marker and no wash — it *is* the document.** `line_marker` returns `""` for it: a `- ` / `+ ` / two-space prefix would overflow table grids and code-block padding, both laid out by the renderer at the full viewport width. The markers' alignment reference is the raw context *inside* a changed region, which is unaffected. The row cache measures it by handing the finished `Line` to `visual_rows_for_line` — the identical call `PreviewView`'s painter makes — so its wrap and the diff's scroll math agree by construction; that arm sits *ahead* of the `line_text` call, which has nothing to say about a row with no source text.

### Keyboard and mouse input

- **Two-layer mouse dispatch.** `MouseDispatcher` (`src/input/mouse.rs`) is a pure state machine turning crossterm `MouseEvent`s into semantic `MouseAction`s (click-count, drag, scroll). `mouse_ops::apply` (`src/editor/mouse_ops/`) is where those mutate `EditorState`. Keep the split strict — coordinate translation belongs in `mouse_ops::coord`, click counting in `MouseDispatcher`.
- **Mouse enable is gated by capabilities.** `terminal::enable_mouse()` is only called from `main` when `capabilities.mouse` is true, and the app also gates `MouseDispatcher::dispatch` on it so a fake mouse event can't drive the editor.
- **Drag anchor lives in `App`, not `EditorState`.** The `drag_anchor: Option<usize>` persists the mouse-down offset across events so the Drag handler can extend the selection. It's a UI-layer fact, not a document-layer one, and clearing it doesn't go through the undo stack.
- **Mouse scroll uses a different bound than keyboard scroll.** `mouse_ops::selection::scroll_by_mouse` allows `max = total - 1` (last line at top of viewport) and never invokes `clamp_cursor_to_viewport_top`; keyboard `Action::ScrollDown` uses `EditorState::scroll_down`, which keeps the cursor visible. Do not merge the two — mouse scroll specifically must not move the cursor. Keyboard `ScrollUp`/`ScrollDown` always step by exactly one line; the configurable `editor.mouse_scroll_lines` applies to the wheel only.
- **Click-to-offset is approximate for formatted text.** Rendered inline styling (`**bold**` → `bold`) shifts char positions between raw and rendered. `mouse_ops::coord::rendered_sub_line_to_offset` maps the visual column 1:1 to the raw source column — exact for unformatted lines, off by a few chars for styled spans. `RAW_REVEAL_DELAY` then turns the cursor's line raw so the user can correct on a second click. `markdown::InlineColMap` provides an exact raw↔rendered column map where precision matters (selection highlight projection in `RenderedView`).
- **A click on an already-revealed line is exact and must stay that way.** The user is looking at raw source, so `coord`'s `revealed_cursor_line` branch maps the column straight onto the raw chars — which means laying the raw line out exactly as the painter did: `RenderedView` hands the raw text to `render_line`, which derives a *hanging indent* from the leading marker, so a revealed `- item` wraps its continuations two cells in and against a narrower budget. Use `coord::revealed_raw_rows` (both for the row count and for the column mapping, shifting `col` by the indent on any sub-row past the first) — never bare `visual_rows_of_str`, whose indent-0 assumption drifts further with every wrap. The indent it returns is the *effective* one: when the marker is as wide as the viewport (`indent + 1 >= width`) both `render_line` and `visual_rows_of_chars` fall back to a flat indent-0 layout, so reporting the raw marker width there would push every continuation column into `char_idx_at_cell_col`'s forbidden-indent zone. Any new caller of `compute_hanging_indent*` that pairs the indent with a wrap layout owes the same clamp.
- **Raw mode wraps flat — no hanging indent — and three consumers depend on that.** Raw shows the file, so word-wrapping a too-long line is the one liberty it takes; indenting the continuation rows of a `- item` would paint whitespace the document doesn't contain. It is also the only self-consistent choice: the scroll cache (`EditorState::raw_line_at_visual_row`, `raw_total_visual_rows`, `char_offset_at_visual_row`) and the click mapping (`coord::raw_click_to_offset`) both wrap through `visual_rows_of_str`, which is hardcoded to indent 0. While `RawView` painted through `render_line_with_cursor_from_visual` — which detects the indent — painter and scroll math were in *different layouts*, disagreeing on the column (every click on a continuation row landed a marker-width short) and on the **row count** at narrow widths (`- aaaaaaaaaa bbbbbbbbbb …` at width 12 is 7 flat rows and 12 indented ones, so every line below painted at the wrong row). `RawView` therefore calls `line_render::render_raw_line_with_cursor`, which forces indent 0 (and thereby drops the blockquote-bar repaint, correct here because in Raw the `> ` on the first row is real source). The navigation half is mode-gated rather than forced, since `move_up_visual` / `move_down_visual` / `current_visual_col` are shared with the rendered views where the cursor's revealed line *is* indented: `state_cursor_visual::hanging_indent_for_mode` returns 0 in `Mode::Raw` and detects otherwise.
- **Link hit-test is a source-scan shortcut.** `mouse_ops::links::link_at_offset` scans the line's raw bytes for balanced `[...](...)` — it is NOT AST-driven. Upgrade to an AST-backed registry if reference-style links or autolinks need precise hit-testing.
- **Checkbox toggling short-circuits cursor placement.** `mouse_ops::checkbox::toggle_checkbox_at` runs BEFORE `click_to_char_offset` in the `MouseAction::Click` arm: a click on the `[ ]` glyph toggles and returns immediately, without moving the cursor. Clicks elsewhere on the task line fall through to normal placement.
- **A table drag resolves its target on one axis, geometrically — never by asking `hit_test` for a classification.** `TableLayoutSnapshot::col_ranges` / `row_ranges` cover only *content*; the `│` borders, the `┬` vertices, and the `├─┼─┤` separator between two data rows all sit in the one-cell gaps between entries. `hit_test` answers those gaps with `ColumnBorder` (or nothing), so a row drag that asked it for `RowHandle | Cell` stalled its hover wherever the pointer crossed a border or separator — roughly half the pointer positions, which made the drop indicator look random. `data_row_at_y` / `column_at_x` snap to the nearest row / column within the table's extent instead, and the `Drag` arms use them: a row drag follows the pointer's **y** alone, a column drag its **x** alone. They also back the two grab hit-tests, so the whole gutter height and the whole top border are live, `┬` vertices included. Outside the table's extent both return `None` — the hover freezes rather than snapping to an edge.
- **A handle acts only on the focused table, because that is the only table it is drawn on.** `paint_handles` draws the `⠿` / `⇔` / `✕` glyphs only on the table the cursor is inside, while `hit_test` runs against every visible snapshot — so `dispatch_table_click` routes *every* handle hit through `focus_table_first`, which moves the cursor into that table and consumes the click without acting. Otherwise a press drives a control that was never painted: a click on another table's right border deletes a row from it, and a drag along another table's top border reorders its columns (the widened `column_at_x` band makes the whole border live, so "only the ✕ is destructive" doesn't hold). Only `Cell` and the inert leftmost outer border are exempt, and only because they fall through to ordinary cursor placement.
- **The `✕` pair is scoped tighter than the rest, spatially and temporally.** Spatially: the column-delete hitbox is `column_glyph_x` ±1, not the column's full width, so a click aimed at a border from the bottom row resizes rather than dropping a column (`column_glyph_x` is shared with `paint_handles`, so glyph and delete cell can't drift). Temporally: `TABLE_DELETE_COOLDOWN` (250 ms) swallows a second delete landing too soon after the last, so a habitual double-click on `✕` removes one row rather than two. **The cooldown is anchored to the delete, not to the click chord** — it was a chord gate (`allow_destructive: false` from the multi-click arms) and that gate never expires: `MULTI_CLICK_WINDOW` restarts on every press, so a user clicking `✕` faster than 2.5 Hz stays in the chord indefinitely and deletes exactly one row before the button goes silently dead, which is precisely the "delete several rows" gesture the handle exists for. `EditorState::last_table_delete_at` is stamped by `table_drag`'s two delete helpers and only on a delete that actually applied.
- **Multi-click arms must dispatch table hits too, because a drag *is* a click-chord member otherwise.** `dispatch_table_click` is shared by the `Click` / `DoubleClick` / `TripleClick` arms, which behave identically. A table interaction is a gesture the user retries: grab, drag, release, and — when it didn't land right — grab the same cell again. That second press arrives as `DoubleClick`, and while those arms did no table hit-testing it armed nothing. `MouseDispatcher::dragged_since_down` closes the other half: a press that turned into a drag doesn't start a chord at all, matching every GUI toolkit, so the retry is a fresh `Click`. Both halves are needed — the flag can't help a retry whose *first* press never dragged.

### Unified UI controls

The interactive elements inside modals/overlays are one family, defined in `ui::controls`. The governing rule: **a control resolves its own styling from `controls`; the parent container only reports whether it is `focused` / `disabled`.** Never hand-roll a focus style in a modal.

- **Four control flavors, declared at the definition site.**
  - **Toggle** (`controls::toggle_spans`) — an on/off slider. It is the one control whose *widget* keeps its value color when focused (inverting it would destroy the on-is-green reading), so its focus shows only in the row's label column.
  - **Pill** (`controls::pill_spans` over a `&[&str]`, e.g. the shared `ASK_ALWAYS_NEVER`) — a multi-value (2+) `‹ value ›` selector cycled with ←/→. On/off is **not** a pill flavor — a binary setting uses the Toggle. An option row declares which it is via the `Control` enum (`Control::Toggle` / `Control::Pill(labels)`); don't reintroduce a `PillStyle::Toggle`.
  - **Text input** — an inline editable value (`controls::text_value_style`; the blink-stable cursor comes from `ui::cursor::text_field_spans`).
  - **Button** — a bracketed press-to-act target; lives in `ui::button_row`, styled by `controls::button_style`.
- **Focus is one language; the label column is the single source of truth.** `REVERSED` means "filled affordance". `controls::focused_style` (= `theme.modal_button_focused`, a `primary` fill) is the shared focus fill. `controls::control_label_style(focused, disabled, theme)` resolves a labeled row's label column — focused → `modal_item_selected` fill, disabled → `modal_close_hint`, resting → `modal_item` — and **both** the settings overlay and the welcome modal call it. A focused row is one unit: pad the label across the whole column so the fill spans label → widget, rather than styling only the label glyphs.
- **Buttons go through `ui::button_row`, never a hand-built literal.** `render_button_row` (centered footer row) and `render_button_at` (left-aligned inline button) both build on `Button` + `controls::button_style`. Construct a `Button::bracketed(label)` and let the helper add the `[ … ]`, size the width, place it, and return the hit-rect.
- **Cycle + cascade logic is shared too.** Pill / toggle inputs route through `controls::Control::apply` (the single transition layer), whose pill arm and every index-valued caller delegate wrap-around math to `controls::cycle_index`; `controls::apply_images_cascade` (images-`Never` forces remote-`Never`, stashing/restoring the prior choice) is shared by the settings overlay and the welcome modal so their behavior can't drift.

### Modals, overlays, and the keybinds editor

- **Live `KeyMap` on `App`, draft inside the overlay.** `App::keymap: Option<KeyMap>` is built once in `run()` and held for the life of the process. The keybinds overlay opens with a *clone* (`KeybindsState::draft_keymap`) plus a cloned `KeyBindingOverrides`, and every rebind mutates only the draft. Nothing is written back to `App::keymap` / `App::keybindings` (or to `keybindings.toml`) until the user activates `[ Save ]`; Esc and `[ Cancel ]` discard the draft so a mis-press is recoverable. On Save the overlay returns `KeybindsResponse::Save { keymap, overrides }` and the modal adapter swaps them onto `App` and persists. Don't regress to mutating the live keymap on every keystroke — a fumbled chord would then only be recoverable by hand-editing `keybindings.toml`.
- **Combined view+edit keybindings overlay.** `OpenKeybinds` owns it; there is no separate cheat-sheet variant, and a user binding to the removed `Action::ShowCheatSheet` fails parsing with `KeyMapError::UnknownAction`.
- **`ModalView` is scrollable; the bespoke overlays are not.** `ModalState` carries `scroll`, `last_total`, `last_visible`, plus `scroll_by(i32)`. Up / Down / PgUp / PgDn / Home / End route to scroll, never to button focus — Left / Right and Tab / Shift-Tab still cycle buttons. Mouse-wheel events are forwarded into open `ModalView` slots via `modal_wheel_delta` in the run loop. The palette / settings / keybinds overlays don't scroll because their bodies fit.
- **Bracketed paste routes to the top modal's focused field.** When a modal is open, `dispatch_modal_event` forwards `Event::Paste` to `Modal::handle_paste` (pop-dispatch-push, like `handle_wheel`/`handle_click`); otherwise it goes to the editor buffer via `dispatch_paste`. The `Modal::handle_paste` default is a no-op `Continue`, so button-only modals ignore pastes; only text-input modals (palette, search/replace, save-copy, export-theme, insert-table, theme/section pickers, settings field editor) override it. Every such `paste()` runs the payload through `ui::sanitize_paste` first — one source of truth that strips control chars (so a multi-line clipboard collapses to one line) and caps at `PASTE_CHAR_CAP` (1024) — then layers field-specific policy and *mirrors that field's keyboard `Char` arm exactly* (append vs. cursor-insert, focus gating, digits-only, live preview). The search modal is the one field that transforms *before* sanitizing (`search::escape::escape` first, so a pasted break survives as `\n`). Don't flatten in the editor path: `dispatch_paste` keeps newlines because the buffer is multi-line. A new text-input modal must override `handle_paste`; a new field's `paste()` must match its typing behavior.
- **External-editor flow needs three things, in order.** When the settings overlay's "Open config.toml in default editor" fires, the App must (1) pause its crossterm read thread, (2) drain the rx channel, and (3) suspend the terminal — before `Command::new($EDITOR).status()`. Skip any and the editor races our read thread for stdin: bytes get split, keystrokes lag, and OSC responses to startup-time queries leak into the buffer. The read thread is poll-based (`crossterm::event::poll(100ms)`) precisely so a `read_paused: Arc<AtomicBool>` can stop it without interrupting a blocked `read()`. After the editor exits, `terminal::re_enter(mouse, keyboard_enhancement)` reinstates alt-screen + raw mode + transient features, and `Config::load()` is re-run. See `src/app/external_editor.rs`.
- **`Modal::kind` and `Modal::dismissable` are stored as struct fields, not hard-coded.** Every modal using `ModalView` carries `kind: ModalKind` and `dismissable: bool` set once in `new()`/`from_*()`. The `ModalView::new(.., self.kind, self.dismissable)` call, the `state.handle_key(.., self.dismissable)` call, AND the trait methods `fn kind()` / `fn dismissable()` all read from those fields. Do NOT pass literals at any of those three sites; they will drift. The `dismissable` field controls three things together: the rendered `esc` close-hint, the cached `esc_button_rect` for click hit-testing, and whether `Esc`/`n`/`N` actually fire `ModalResponse::Cancelled`. Modals that don't use `ModalView` (palette, settings, keybinds, save_copy, insert_table, theme_picker, export_theme) inherit the trait defaults (`Normal` / `true`); don't add no-op overrides.
- **The welcome modal is dismissable only when it wasn't a first run.** It doesn't use `ModalView` but follows the same field-not-literal rule: `WelcomeState::dismissable` is read by the `Esc` arm of `handle_key`, by `show_close_hint` in `render` (which populates `esc_button_rect`, so the click path needs no second gate), and by `Modal::dismissable`. `WelcomeModal::from_state` (first run) leaves it `false` — the spec replaces Cancel with an explicit "Show on next launch" toggle, and there is no prior choice to protect. Every on-demand opening (`Action::OpenWelcome`, the capabilities notice's "Adjust settings" button) goes through `WelcomeModal::new` and sets it `true` via `WelcomeState::with_dismissable`, because reopening carries a risk the first run doesn't: below truecolor `WelcomeState::new` force-sets images and diagrams to `Never` and `save_outcome` *persists* that, so without a write-nothing exit, merely looking at the surface from a weaker terminal would overwrite settings chosen on a capable one. (`WelcomeResponse::Cancel` → a plain `ModalOutcome::Close`, and deliberately no fingerprint seeding — an on-demand opening is not the first-visit notice.)
- **Construct `ModalView` via `ModalView::new(...)`, not a struct literal.** The constructor pre-fills `max_pad_h` to `MAX_PAD_H` (4); chain `.with_max_pad_h(n)` to override. Struct-literal construction would force every call site to spell it out and break silently the next time the default changes.
- **A prose modal must cap its content width; a tabular one must not.** `ModalView` sizes itself to its longest *unwrapped* body line, right for tables (capability rows, keybindings) and wrong for prose: a body that is one wrapped paragraph — as `ui::cap_summary::theme_downgrade_lines` deliberately is — has a natural width equal to the whole sentence, so the modal stretches across the terminal. Chain `.with_max_content_width(PROSE_CONTENT_WIDTH)` — a count of *text* columns, so the outer modal is that plus `2 * max_pad_h` — on `ModalChrome::new` or on `ModalView::new` directly. It matches the welcome modal's `CONTENT_WIDTH`, so the two prose surfaces read at the same measure. The cap is raised to the button-row width before clamping, so it can never clip the footer. Don't instead pre-wrap the paragraph into short `Line`s — `ModalView` wraps and sizes with `wrapped_rows`, so hand-splitting double-wraps at narrow widths and leaves ragged rows at wide ones.
- **Horizontal padding lives on `ContentSize`, not on `FrameOpts`.** `FrameOpts.content` embeds the same `ContentSize` value fed to `centered_rect_for_content`, so the pre-render sizing pass and the post-render `draw_frame` padding can't disagree. Set `max_pad_h` once on the `ContentSize` (or take `MAX_PAD_H` via `..Default::default()`) and pass that one value to both calls. Do NOT reintroduce a parallel `FrameOpts.max_pad_h`. The keybinds overlay raises `max_pad_h` to 8 because its table is dense and the "Already bound to …" error string would otherwise reflow the modal during capture.
- **Preview-mode Ctrl-key allowlist.** `input::mode_handler::default::preview_safe_action` decides which Ctrl-* chords fire in Preview mode. Read-only overlay openers (`ShowCommandPalette`, `OpenSettings`, `OpenWelcome`, `OpenKeybinds`, `OpenConfigFolder`, `ShowMarkdownCheatSheet`, `SwitchTheme`, `CreateCustomTheme`) belong on it — a new modal-opening action must be added here too, or the chord silently no-ops in Preview.
- **Focus vs. persistent selection (modal styling convention).** When a modal carries a *persistent selection* independent of focus — e.g. the export-theme modal's highlighted theme name — use three-tier styling. (Ordinary labeled control rows — settings, welcome — are *not* this case; their focus styling is `controls::control_label_style`.)
  - Focused element → `theme.modal_button_focused` (`primary` bg + REVERSED + bold). Filled, strongest.
  - Persistent selection *without* focus → `theme.modal_item_selected_unfocused` (`secondary` **fg** on `surface_elevated`, bold). Outlined, no fill.
  - Neither → `theme.modal_item` (plain text on `surface_elevated`).

  Don't reuse `modal_item_selected` for "selected but unfocused" — it also uses a filled `primary` bg, colliding with the focused affordance. For composite affordances (checkbox glyph + label), apply the unfocused-selection style to the *glyph only*. See [`docs/dev/theming.md`](docs/dev/theming.md) §"Focus vs. persistent selection" for the rationale and monochrome fallback.

### Update check

- **One fetch, one cache, four states, three entry points.** `App::latest_release` is the session cache every path reads; `ReleaseStatus` is `Pending | UpToDate | Available | Failed`, and `UpdateModal` renders all four. The startup check pushes it *only* for `Available`; the About page's `[ Check for updates ]` button and `Action::CheckForUpdates` push it in whatever state is known and let the in-flight result replace that. Keeping the up-to-date and failure states on the same modal is what lets an explicit check answer honestly while the startup path stays silent — don't split them into a second modal.
- **Both conclusive states share one body shape and one title.** A verdict line (`edamame is up to date.` / `Update available.`), a blank, then the `Installed version:` / `Latest release:` rows — a two-row table with padded values, not numbers embedded in a sentence, so the two states differ by verdict rather than layout. The modal frame is captioned `Check for updates` in every state, including `Available` (a frame captioned "Update available" above a line reading "Update available." says it twice). `Available` alone adds the notes and the `[ View on GitHub ]` button, which opens *that release's* page (`release_url`), not the releases list.
- **The up-to-date / available split is decided once, in `ReleaseStatus::from_fetch`, not at render time.** Two consumers must agree: the modal picks its copy from the variant, and `policy::notice_due` gates the nag on it. The old `release_suffix` annotated a *string* and had no way to say "don't nag". A build *ahead* of the latest release (a local build between tags) compares `Less` and is not an update.
- **A comparison that can't be made is its own state.** `status::compare_versions` answers `Option<Ordering>`; `None` — either side unparseable as a dotted numeric version, so a `v0.2.0-rc1` or `v0.2.0+build.7` tag — becomes `ReleaseStatus::Inconclusive`, which `notice_due` keeps as silent as `UpToDate`. Don't fold it back in: both are silent on the notice path, but only one is *true*, and the explicit-check modal prints installed and latest versions directly under its verdict — so an `UpToDate` verdict there sat above two rows disagreeing with it. `Inconclusive` says "Couldn't compare versions." and keeps `[ View on GitHub ]`. `ReleaseStatus::tag()` is the single accessor for the tag across the three states that name one.
- **The notice cannot join `App::new`'s startup-modal ordering.** Every other startup modal is built synchronously and pushed in one deliberate priority order; this one depends on a result arriving long after. A finding is parked in `App::pending_update_notice` and `tick_update_notice` (a member of the `tick_timers` family) pushes it on the first frame `modal_stack.is_empty()`. Gate on *the stack being empty*, never on "no welcome modal" — that is what makes it robust against the first-run welcome, the config warning, the capabilities notice, and anything added later.
- **The startup check waits out the first-run welcome modal.** That modal is where `check_for_updates` is asked, so a check fired before it is answered makes the request the setting exists to gate. `spawn_startup_update_check` is therefore a `tick_timers` member, not a one-shot call from `run()`: it parks while a `WelcomeModal` is on the stack and re-reads `config.editor.check_for_updates` *after* it closes, so a first-run decline is honored on that same launch. Only the welcome gates it — the config warning and the capabilities notice are not consent surfaces for this.
- **Both field scans are anchored to real top-level keys.** `parse::top_level_value` tracks object depth and consumes strings whole; a bare `find("\"body\"")` matches the key text inside another field's *value*, and a release `name` is free text somebody types. Depth also excludes the nested `author` / `assets` objects. It is still not a JSON parser — it locates one key and hands the rest to the caller.
- **`tag_name` is validated, not just extracted.** It is remote text reaching a rendered line *and* — interpolated by `fetch::release_url` — a URL handed to the system browser, so `parse_tag_name` refuses a tag over `MAX_TAG_BYTES` (64) or carrying anything outside the semver alphabet (`is_tag_char`: ASCII alphanumerics plus `.`, `-`, `_`, `+`). It rejects rather than sanitizes because, unlike prose, a tag that isn't a plain tag is not worth reporting — and a rejected tag drops the whole release (`parse_release` returns `None`), since notes without a version are not a finding. The excluded characters are the ones that would break the `Latest release:` row's layout (whitespace, controls) or change what the release URL resolves to (`/ ? # % &`).
- **`sanitize_notes` strips `Cf` formatting characters as well as controls.** Bidi overrides and zero-width joiners are not `char::is_control`, and they are the last thing remote text could still do to a surface that never re-parses it as Markdown: reverse or hide part of a line.
- **`last_update_check` is stamped at *spawn*, not on arrival.** A worker that hangs to its timeout, or a process killed before the result lands, would otherwise leave the clock untouched and re-check on every launch — the retry storm the throttle prevents. The cost is that a transient failure waits out the full interval, which is right for a notification nobody is waiting on.
- **The throttle governs the automatic check only.** `open_update_modal` always re-fetches: the 24 h gate bounds *unattended* chatter, and an explicit request is the opposite. It shows a cached *positive* result meanwhile so the modal isn't blank, but never a cached `Failed` — re-showing "couldn't check" while a fresh attempt is running answers the question with a stale answer.
- **Bookkeeping writes must not flash.** `update_notified_for` and `last_update_check` go through a bare `Config::save`, not `save_config_with_flash`: the user changed no setting, and a toast for a timestamp is noise. `Config::save` already declines to write under `--no-config`, so neither needs its own gate — that session simply re-checks each launch.
- **Notes get structural styling, never Markdown parsing.** `ui::update_check::note_lines` bolds an ATX heading (dropping its `#` run), swaps a `-`/`*`/`+` marker for a `•`, and drops the blank line Keep a Changelog puts *under* each heading while keeping the one above it. That is the whole vocabulary: a *line* is classified locally over already-bounded text, with a choice between three fixed styles. Don't grow it into an inline parser — `**bold**` and `[text](url)` stay literal on purpose, and a parser would hand a release body emphasis, links, images, and layout inside an app modal. Wrapped lines have no hanging indent: `ModalView` owns the wrapping.
- **Release notes are remote text and are bounded on the worker, before anything else sees them.** `parse::sanitize_notes` cuts at cargo-dist's first `## Install` heading, strips control characters, and caps at 30 lines / 2 000 bytes; the main thread never holds an unbounded string. They are then rendered a line at a time with structural styling only and **never re-parsed as Markdown**.
- **`body` needs a real JSON string decoder; `tag_name` does not.** A tag never contains a quote or backslash, so `parse_tag_name` can scan to the next `"`. The body is prose carrying quotes, backslashes, newlines and emoji, so `decode_json_string` walks escapes properly — an embedded `\"` would otherwise terminate the value at the first quotation mark the author typed. Still no `serde_json`: this is a JSON *string-literal* decoder, not a value parser, and every failure degrades to "no notes". Note that a `"###` sequence terminates an `r#"…"#` literal, so test fixtures containing one need different quoting.
- **The release notes ship from `CHANGELOG.md`.** `dist` reads the section matching the tag and puts it at the top of the GitHub release body; edamame reads that body back and shows the part above `## Install`. So a `CHANGELOG.md` entry *is* what users read in the update modal. A release cut without a matching section still notifies, just with no summary.
- **`ui::update_check` takes plain values, not `ReleaseStatus`.** Nothing under `src/ui/` imports `crate::app`, and this is not the module to start: the modal adapter maps the status onto `ui`'s own `UpdateReport`. Same rule `ui::about` documents. That translation is the only reason `update_check` can stay under `app` rather than being promoted to a top-level leaf subsystem.
- **The About page reports no release information at all.** It used to fetch on every first open and show a "Current release" row, which made merely opening the page a network request and put a second surface in the business of rendering release state.

### Images, diagrams, and export

- **The accepted image-format list is a security boundary, and Cargo feature unification is what breaks it.** `Cargo.toml` declares `image` with exactly the formats edamame accepts (`png`, `jpeg`, `gif`, `bmp`, `webp`; SVG goes through `usvg`, not `image`), and `loader::decode` refuses anything else as an ordinary `Unsupported` error. That list only holds if *no other crate in the graph* asks `image` for more, because Cargo unions features across the whole graph: `ratatui-image`'s `image-defaults` feature forwards `image`'s full defaults, silently re-widening our decode surface (it also dragged in the AVIF *encoder* — `ravif` → `rav1e` → the unmaintained `paste` — for a program that encodes nothing). So `ratatui-image` is declared `default-features = false, features = ["crossterm"]`, and a new dependency that wants `image` must be checked for the same trap; `cargo tree -e features -i image` is the check. Widening the list is a deliberate decision recorded in `docs/editing.md`, never a side effect.
- **Advisory policy is a checked-in file, not CI flags.** `.cargo/audit.toml` denies informational advisories (unmaintained / unsound / yanked) alongside vulnerabilities and carries the reasoned `ignore` list, so `cargo audit` from the repo root gives the same verdict as CI. An advisory belongs in `ignore` only when the fix is genuinely unreachable (upstream pin, or our own deliberate `=` pin); each entry records why it is acceptable and what would let us delete it. If a bump can clear it, take the bump.
- **The media prompts are per *document*, not per launch — and one method owns that.** Under the default `images.enabled = "ask"` (and its diagrams / remote twins) the prompt is the *only* thing that sets `session_images_enabled`, and `effective_images_enabled` is false until it does — so a question never asked means images never decode. All three prompts are built from the open document (`Ask` **and** this document has an image / a diagram / a remote URL), so every path replacing the document's *contents* must re-evaluate them. That is `App::on_document_contents_swapped` (marks `images_dirty`, then queues the three), and its three callers are `load_file_into_editor` (link follow, back/forward, external-editor return), `reload_buffer_from_disk`, and `apply_diff_resolution` — the last being the *default* external-change path, since a clean buffer with `diff_on_change` goes to diff review rather than a reload. Launching on an image-free file and following a link to one with images was issue #30. It is one method rather than three copies because the copies are how the diff path was missed; a new contents-replacing path owes the call, placed **after** `refresh_parsed` (the prompts read `editor.parsed`).
  - Each `queue_*_prompt` helper carries its own gates, so every queue site — the document swap and the three settings-overlay `apply_*` handlers — inherits them: `media_renderable()` (below truecolor `App::new` asks nothing, because an opt-in we then decline to honor is noise and `Always` / `Never` would persist a choice made where the result can't be seen), an answer already given this session, and a prompt already on the stack (idempotent). The session-answer gate is what forces `session_remote_declined` to exist: images and diagrams record a decline in their `Option<bool>` by construction, while `session_allow_remote` is a bare bool, so without a companion flag a dismissed remote prompt would be indistinguishable from "never asked". The two remote flags are mutually exclusive — the `Yes` and `Always` arms clear the decline — and a persisted policy change supersedes both (`apply_remote_policy_change`).
- **A new document means a new `EditorState`, and everything App-level it needs must be re-applied — `app::configure_new_editor` is where that lives.** `load_file_into_editor` builds a whole new `EditorState` (and so a whole new `ImageCache`) per document, so anything wired onto the startup editor in `App::new` or `spawn_event_threads` is *gone* for every file opened by following a link, navigating back, or returning from `$EDITOR`. It has drifted twice, both invisible until a second document was opened. (1) The encoder worker's `ResizeRequest` sender was attached exactly once, in `spawn_event_threads`; `ImageCache::get_protocol_pair` returns `None` without one and `paint_images` then draws the `[Image: alt]` placeholder — so every later document showed reserved rows over a placeholder while its images fetched, decoded and cached perfectly in the background. `App::resize_tx` keeps the clone the swap site re-attaches. (2) `cursor_blink` was applied only in `App::new`, so a `cursor_blink = false` config started blinking again after the first link follow. Both go through `configure_new_editor`; a new setting an `EditorState` reads out of `Config` belongs *there*. The `resize_tx` re-attach stays separate only because `App::new` has no sender yet when it builds its editor.
- **Decode happens off the UI thread.** `image::loader` runs in a worker; results arrive as `AppEvent::ImageReady`. URL fetches use `ureq` (rustls, no system OpenSSL). Failures are memoised so a reparse won't re-issue a doomed request every frame.
- **ratatui-image encode uses a second worker.** `ResizeRequest` / `ResizeResponse` route through `AppEvent::ProtocolReady` and a pending-request FIFO. Failures still pop the queue so a placeholder stays visible until a later frame re-enqueues. `ThreadProtocol::resize_encode` *moves* the inner `StatefulProtocol` to the worker, so `render()` silently draws nothing until the response lands — and `ProtocolPair::native_ready` latches on the first successful encode and is never cleared. `paint_native` must therefore gate the native render on `native.protocol_type().is_some()` (inner present) as well as `native_ready`, falling back to the scratch otherwise; gating on `native_ready` alone leaves a hole where `clear_visible_reserved_rect` just blanked the rect.
- **A native transmission that is still on screen must not be re-sent.** iTerm2 and Sixel deliver the full raw png on every render — `Iterm2::render` puts the entire base64 payload in one cell's `symbol` — and `Buffer::diff` sets `invalidated = max(symbol.width(), invalidated) - 1` per cell, where that symbol's *display width* is the payload length (100 000+ columns). `invalidated` therefore stays positive for the rest of the buffer, so **every** later cell is re-emitted, including a second image's payload cell. One full-resolution image at the top of a document consequently retransmits every image below it on every frame; at the cursor-blink cadence (`cursor_blink_ms`, 530 ms) that reads as a ~2 Hz flicker, and the ECH sweep at the head of each escape makes it visible. Kitty is immune (it transmits once and paints cheap unicode-placeholder rows), which is why the symptom is iTerm2-only.
  - `paint_native` therefore records each transmission as an `image::cache::NativePaint { rect, generation, frame }` on the `ProtocolPair` and, when the *immediately preceding* frame left that same encoding at that same rect, marks the whole rect `skip` instead of re-rendering, so ratatui emits nothing and the terminal keeps what it has. Two things keep the record honest: the one-frame adjacency rule (any scratch / suppressed / off-screen frame invalidates it automatically — the scratch paths clear it explicitly, while the suppressed / off-screen paths rely on the frame gap alone, which `a_suppressed_frame_forces_the_next_native_frame_to_retransmit` pins), and `ImageCache::invalidate_native_paints()` on the two events that repaint the screen behind our back — `App::on_resize` and the `terminal.clear()` after the external editor returns. `ProtocolPair::native_generation` rides along as defense in depth only: the branch of `apply_resize_response` that bumps it already clears the record, so the comparison never actually sees a mismatch.
  - **The skip marking makes the frame buffer lie** — it records blanks over rows the terminal is still showing an image in — and what keeps that from stranding a ghost is that ratatui's hand-written `impl PartialEq for Cell` compares the `skip` flag alongside symbol and style: the first frame that stops skipping diffs unequal and emits the blanks, even though both sides are blank. Without that, an image scrolling away into empty space below the document would never be erased. It is an upstream detail this design rests on, pinned by `skipped_rect_still_diffs_against_the_same_cells_unskipped`. The frame counter is bumped in `App::draw_frame`, **not** in `paint_images`: Raw and Diff modes draw frames without painting images, and counting only painted frames would make a pre-Raw transmission look adjacent to the first frame back in Rendered. A new code path that repaints the terminal outside the paint pass must call `invalidate_native_paints`.
- **`Capabilities::halfblocks_picker` must be forced to `ProtocolType::Halfblocks`.** ratatui-image gives you two wrong constructors: `Picker::halfblocks()` hardcodes `font_size` to (10, 20) — an image would change aspect ratio every time it crossed the native↔halfblocks boundary — and `Picker::from_fontsize()` keeps the probed size but *infers a protocol from the environment*, returning an **iTerm2** picker whenever `$TERM_PROGRAM`/`$LC_TERMINAL` names iTerm2, WezTerm, VS Code, Warp, Hyper, Tabby, rio, mintty or Bobcat (or tmux under one of those). `terminal::capabilities::halfblocks_from` takes the font size from the first and stamps the protocol from the second; `image::render_halfblocks_scratch` re-forces it locally. Getting this wrong is catastrophic and silent: the scratch then holds one cell carrying a whole base64-PNG escape surrounded by `skip` cells, which `paint_halfblocks_partial` cannot clip by row — the image flashes at full fidelity only on frames where row 0 is copied, vanishes otherwise, re-transmits the entire PNG on every scroll frame, and punches through the modal dim sweep.
- **iTerm2 answers the Kitty capability query but can't do unicode placeholders.** ratatui-image's Kitty backend renders *exclusively* through the protocol's unicode-placeholder extension (transmit once with `U=1`, then paint U+10EEEE cells carrying the image id in diacritics), and iTerm2 3.5+ implements the graphics protocol without that part — so the probe lands on `Kitty`, the image is transmitted, never placed, and the reserved rows stay blank. `detect_image_protocol` overrides `Kitty` → `Iterm2` when `iterm2_hint_is_trustworthy()`, matching ratatui-image's own compatibility matrix. The decision is factored into the pure `resolve_protocol(probed, iterm2)` so it is testable without live stdio probing. Only `Kitty` is overridden; a probe landing on Sixel or Halfblocks is left alone. **The override is suppressed inside tmux**, because the env hint it rests on is exactly what tmux makes stale: `TERM_PROGRAM` and `LC_TERMINAL` are not in tmux's default `update-environment`, so a session created from iTerm2 and reattached from Ghostty still advertises iTerm2 while the live terminal speaks Kitty. The stdio probe asks the live terminal through tmux passthrough and has no such staleness, which is why ratatui-image treats it as authoritative over env hints; inside tmux we do the same. The case this gives up is tmux running *inside* iTerm2, which falls back to blank image rows — the pre-override status quo, and recoverable via halfblocks in a way that a wrong-protocol pin on Ghostty is not.
- **Mermaid diagrams use synthetic cache keys.** `diagram::mermaid` renders Mermaid → SVG → PNG via `mermaid-rs-renderer` + `resvg`/`usvg`, and the cache key is `diagram-mermaid-<sha256>` so the image cache reuses an already-rendered diagram across reparses. Pin `mermaid-rs-renderer` exactly (`=0.2.2`) — the crate is pre-1.0 with known panic bugs and rapid churn; `resvg`/`usvg` versions must match what it transitively depends on so cargo doesn't compile two copies.
- **A revealed image block reserves one row per *source* line, not its image's rows.** A `Block::ImageBlock` reserves as many rendered rows as the decoded image occupies, which has nothing to do with how many lines its source was written on — so the raw-source reveal was clipped by a short-wide diagram (issue #24) and padded out by a tall one, and an ordinary one-line `![alt](url)` was left floating in a screenful of blank rows. **Both are the same bug and take the same fix; the gate is `is_image_block`, not `is_mermaid_block`.** `EditorState::image_reveal` stashes an `ImageReveal { ordinal, url, rows }` for the block the cursor rests in, `refresh_parsed`'s row-override consults it **first** (ahead of both the decode cache and the images-disabled collapse to 1 row — the reveal replaces the image on screen entirely), and the document reflows.
  - The transition is resolved once per frame by `sync_image_reveal` from `App::prepare_viewport`, because the reveal is time-driven — the `RAW_REVEAL_DELAY` window elapses with no event of its own. The call is a no-op on frames where the target hasn't moved. **A sync returning `true` owes `ensure_cursor_visible`**: the reflow moves rendered rows under a cursor that never moved, and `ensure_cursor_visible` otherwise only runs off a motion or an edit — so entering a fence from below near the bottom of the viewport grows the block downward and carries the cursor off screen (`revealing_a_diagram_at_the_fold_keeps_the_cursor_on_screen`); the image direction shrinks the block instead, which can strand the cursor above `scroll`. The `dims` that call is given were measured *before* the reflow, so a reveal changing the line count can paint one frame with a stale gutter width or scrollbar reservation; the `needs_draw` it sets re-measures on the next. It holds the current reservation while `parsed_dirty` (a diagram's synthetic URL hashes its source and an ordinary image's *is* source text, so mid-typing the parse names a URL the next reparse will retire — and an in-line edit can't change the line count anyway).
  - **The row count is `rendered_view::revealed_source_line_count`, not the raw line count**, because the two differ exactly where the mermaid-only version never had to look: a block's *extended* byte range absorbs the blank line that follows a paragraph — and the promoted `![alt](url)` paragraph with it — while a fence's range stops at its closing ```. That blank is a virtual block with a rendered row of its own, so counting it reserves one row too many and shifts everything below down. It otherwise counts through the same split the reveal paints with (`raw_source_lines`), so reserved rows and painted lines can't disagree.
  - **The block is named by ordinal *and* URL, and `ImageRowOverride` takes both** — the callback is the renderer's only channel and a URL alone doesn't identify a block: a document using one image twice (a logo in a header and a footer) would see every copy collapse to the revealed block's line count for as long as the cursor rested in any of them (`revealing_one_image_leaves_a_duplicate_of_it_alone`). The ordinal counts `Block::ImageBlock`s in document order — `Renderer::image_block_seq` on one side, the index into `ParsedDoc::image_blocks` on the other, both walking the same top-level block list. The URL rides along as a staleness check for the `parsed_dirty` hold: a reservation whose block has stopped being an image matches nothing, instead of the ordinal sliding onto the next image. `sub_lines_in_block`'s mermaid branch clamps to `block_own - 1`, which is what turned the short reservation into a stuck cursor as well as a clipped view; with the expansion in place that clamp is a no-op.
- **HTML export has two modes.** `export::html` produces either a linked or self-contained HTML file (base64 `data:` URIs for local images). The bundled stylesheet is `config/export/default.css`. `export::custom` shells out to a user-defined command (pandoc, weasyprint, …); the intermediate HTML file is hosted via `tempfile` and cleaned up on drop.

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
| Traits | `PascalCase` | `ModeHandler` |
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
- `StatefulWidget` for widgets that mutate scroll/cursor state; `Widget` for purely functional rendering

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

Use `tracing` macros (`tracing::info!`, `tracing::debug!`, etc.) — **never** `println!` or `eprintln!` (would corrupt the TUI). Logging is only initialized when `[dev] logging = true` in config (or `--log`), so tracing calls in production are no-ops.

**The subscriber's filter is a bare `debug`, and it has to stay unscoped.** `tracing_subscriber::fmt()` defaults to `info`, which silently dropped every `debug!` in the crate — the diagnostic trail the flag exists for (image decode dispatch and results, watcher events, link handling) is almost entirely at `debug`. The obvious repair, `edamame=debug`, is also wrong: `EnvFilter` matches on *target*, and the diagnostic call sites set their own — `image`, `watcher`, `link`, `mouse`, `app` — none of which live under the crate's target path. Nothing in the dependency graph pulls `tracing` (`cargo tree -i tracing` lists only this crate and the subscriber), so an unscoped filter cannot be flooded by a chatty dependency. `RUST_LOG` overrides it. A new custom target needs no filter change; a new *dependency* that emits `tracing` means re-checking that claim.

### Tests

- Every source file gets a `#[cfg(test)] mod tests { ... }` block for unit tests
- Integration tests in `tests/` import from the library crate (`edamame::`)
- Use `insta::assert_debug_snapshot!` for complex output (ASTs, rendered lines)
- Use `ratatui::backend::TestBackend` for widget rendering tests
- Use `Box::leak(Box::new(Theme::default()))` to produce `&'static Theme` in tests without lifetime annotations — intentional and safe in tests
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
| `serde` + `toml` + `toml_edit` | Config file deserialization and surgical writes |
| `serde_ignored` | Surface unknown-key warnings without `deny_unknown_fields` |
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
| `fancy-regex` | Regex engine for vim `:s`/`:%s` substitution (backreferences + lookaround; the `/` search path stays literal-substring) |
| `syntect` + `two-face` | Code-block syntax highlighting — parsing only (no themes, no HTML writer); syntect's bundled Sublime defaults are 75 grammars, `two-face` takes it to 213 |
| `insta` | Snapshot testing |
| `proptest` | Property-based testing |
