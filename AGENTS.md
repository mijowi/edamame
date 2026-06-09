# AGENTS.md — edamame

Guidance for human and agentic contributors working in this repository. `CLAUDE.md` is a symlink to this file; edit one, update both. 

## Project Overview

`edamame` is a Rust TUI application for viewing and editing Markdown files in the terminal. It uses `ratatui` for rendering, `pulldown-cmark` for parsing, and `ropey` for rope-based text editing. The crate ships as both a binary (`edamame`) and a library (so integration tests can import it).

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
cargo test --test palette
cargo test --test diagrams

# Review / accept updated insta snapshots:
cargo insta review
```

**Test frameworks in use:**
- `insta` — snapshot testing (`assert_debug_snapshot!`, `assert_snapshot!`)
- `proptest` — property-based testing (used in `tests/source_map.rs`)
- `tempfile` — temporary files for I/O tests
- `ratatui::backend::TestBackend` — headless widget rendering

**Do not** write tests for: terminal capability detection that depends on live terminal probing (`Picker::from_query_stdio`), cross-platform clipboard (OS clipboard paths race between parallel tests), or the actual crossterm terminal-mouse wire protocol — these are covered by manual smoke testing. *Do* test mouse logic at the `MouseDispatcher` + `mouse_ops::apply` layer: both are pure functions of an input event and an editor state. 

## Project Structure

```
src/
  main.rs           # CLI args, config load, terminal init, App::run
  lib.rs            # re-exports all modules (enables integration tests)

  app.rs            # facade — declares submodules and re-exports App, AppEvent
  app/
    actions.rs        # Action → App-level side effects (modal pushes, nav, …)
    autosave.rs       # debounced autosave timer
    event_loop.rs     # main run loop: term events, image-ready, link-open
    external_editor.rs # pause-read / suspend-term / spawn $EDITOR / re-enter
    flash.rs          # TransientMessage on the hint line (MessageKind, ttl)
    frame_timer.rs    # frame-rate / redraw pacing
    image_dispatch.rs # spawn decode + encode workers; route ImageReady
    nav.rs            # NavEntry / file-open history (back / forward)
    pointer.rs        # mouse-cursor shape changes (link hover, etc.)
    modal.rs          # facade for the modal subsystem
    modal/
      stack.rs        # ModalStack: Vec<Box<dyn Modal>>, top-of-stack dispatch
      types.rs        # Modal trait, ModalKind, ModalOutcome, ModalRenderCtx
      command_palette.rs, config_warning.rs, diagrams_enabled.rs,
      dirty_guard.rs, export_success.rs, export_theme.rs, images_enabled.rs,
      insert_table.rs, keybinds.rs, markdown_cheat_sheet.rs, notice.rs,
      quit_confirm.rs, remote_image.rs, save_copy.rs, settings.rs,
      terminal_capabilities.rs, theme_picker.rs, welcome.rs,
      width_injection.rs    # one modal adapter per file

  config.rs         # facade — re-exports Config, KeyMap, Theme, ThemeFile, …
  config/
    config.rs       # Config + sub-configs (Editor, Modal, Table, Images, Dev,
                    #   Export, …); LoadedConfig (serde+toml);
                    #   load() / save() / ensure_default_files()
    init.rs         # first-run scaffolding (writes annotated config.toml etc.)
    keymap.rs       # Action enum, KeyMap, KeyBindingOverrides, parse_key()
    readers.rs      # read_theme_named, read_keybindings — disk I/O helpers
    sections.rs     # surgical `toml_edit` updates that preserve comments
    theme.rs        # Theme: all Style values; BUILTIN_THEMES registry;
                    #   list_theme_names(), Palette::builtin()
    theme_file.rs   # facade for theme-file submodules
    theme_file/     # ThemeFile, StyleSpec, ColorField: user-authorable TOML
                    #   format — converts to/from Theme via From impls
    themes.rs       # facade for built-in theme constructors
    themes/         # one file per built-in theme (edamame.rs, dracula.rs, …)
    warnings.rs     # ConfigWarning, WarningKind — surfaced via a modal at startup

  diagram.rs        # facade
  diagram/
    mermaid.rs      # Mermaid → SVG → PNG render pipeline (mermaid-rs-renderer + resvg)

  document.rs       # facade — re-exports Buffer, Cursor, EditDelta, History,
                    #           ParsedDoc, Selection, SourceMap, grapheme helpers
  document/
    buffer.rs       # Buffer wrapping ropey::Rope; file I/O + edit primitives
    cursor.rs       # Cursor: rope char offset + preferred visual column
    graphemes.rs    # next/prev_grapheme_offset over a Rope slice
    history.rs      # History: undo/redo stack of EditDelta values; merges
                    #          adjacent alphanumeric inserts into word-groups
    parsed_doc.rs   # ParsedDoc: re-parses on change, caches AST + source map;
                    #          synthesises a virtual block per blank line
    selection.rs    # Selection + VisualSelection (anchor + active rope offsets)
    source_map.rs   # SourceMap: block byte-range ↔ rendered-line-index mapping
    visual_cache.rs # memoised visual-row counts for wrapped lines

  editor.rs         # facade — re-exports EditorState, Mode, RAW_REVEAL_DELAY
  editor/
    edit_ops.rs     # Action → EditorState mutations (cursor, buffer, history)
    footnote_edit.rs # pure footnote edit primitives: scan, auto-number
                    #   insert, renumber (by first reference), delete
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
    state.rs        # EditorState: owns Buffer, Cursor, History, Mode, ParsedDoc;
                    #   re-exports RAW_REVEAL_DELAY
    state_cursor_block.rs   # cursor-block lookup + reveal jitter suppression
    state_cursor_visual.rs  # move_up_visual / move_down_visual
    state_section_path.rs   # cursor_section_chain — heading-ancestor
                            #   breadcrumb for the status bar
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
    mouse.rs        # MouseDispatcher: click-count + drag state machine

  markdown.rs       # facade
  markdown/
    ast.rs          # Block, Inline, ListItem enums; inlines_to_plain()
    inline_col_map.rs # InlineColMap: raw byte ↔ rendered visual column
    parse_offsets.rs  # (byte_start, byte_end) spans from pulldown-cmark
    parser.rs       # pulldown-cmark → Vec<Block>; promotes images / diagrams /
                    #   html comments to their own blocks; splits lists on blanks
    parser/
      post_pass.rs  # promotion / splitting transforms
    renderer.rs     # Renderer<'t>: Vec<Block> → Vec<Line<'static>>
    renderer/
      list.rs, table.rs, util.rs  # block-specific render helpers
    table_layout.rs # column-width measurement and packed-comment hints

  terminal.rs       # facade
  terminal/
    capabilities.rs # Capabilities, ColorDepth, ImageProtocol;
                    #   detect_color_depth_from_env() (env-only, no I/O)
    setup.rs        # setup() / restore() / re_enter() / enable_mouse() /
                    #   set_pointer_shape() / PointerShape

  ui.rs             # facade — re-exports widgets and state types
  ui/
    bottom_region.rs    # hint line + status bar layout; HintChord / HintContent
    button_row.rs       # shared focusable [ Button ] row helper
    cap_summary.rs      # capabilities-notice body lines
    command_palette.rs  # PaletteView + PaletteState (nucleo-matcher fuzzy)
    command_palette/actions.rs   # palette-eligible Action list
    content_width.rs    # measure expected wrapped width
    dim.rs              # ContentSize, FrameOpts, centered_rect_for_content,
                        #   draw_frame, ModalKind, MAX_PAD_H (modal layout)
    editor_view.rs      # EditorView + EditorViewState; dispatches to sub-views
    export_theme_modal.rs # write-current-theme-to-disk modal
    gutter.rs           # optional line-number column; split_gutter()
    image_view.rs       # ImageLayoutSnapshot — block-image rendering
    insert_table_modal.rs # rows × cols prompt
    keybinds_overlay.rs # KeybindsView + KeybindsState (view + edit unified)
    keybinds_overlay/categories.rs # grouped Action list
    line_render.rs      # render_line / render_line_with_cursor:
                        #   word-aware wrap, trailing-cell background fill;
                        #   shared by Preview and Rendered
    link_view.rs        # LinkLayoutSnapshot — link-target hit map
    markdown_cheat_sheet.rs # body_lines() — Markdown syntax reference
    modal.rs            # ModalView + ModalState (scrollable button modal)
    modal_row.rs        # button row used inside ModalView
    overlay_nav.rs      # shared Tab / arrow focus cycling
    preview.rs          # PreviewView + PreviewState
    raw_view.rs         # RawView + RawViewState (plain-text editor)
    rendered_view.rs    # RenderedView + RenderedViewState (hybrid view)
    rendered_view/      # paint, cell_overlay, list_marker, raw_text — split
                        #   submodules of the rendered view
    save_copy_modal.rs  # "Save copy as…" path entry
    scroll_container.rs # ScrollContainerState; ModalKind lives here
    scrollbar.rs        # narrow side scrollbar widget
    settings_overlay.rs # SettingsView + SettingsState (config UI)
    settings_overlay/rows.rs # row definitions + theme cycle list
    status_bar.rs       # StatusBar + StatusBarState
    table_view.rs       # table rendering / column-divider hit map
    theme_picker.rs     # Ctrl+Shift+T live preview picker
    welcome.rs          # first-run welcome modal

tests/
  diagrams.rs       # Mermaid block detection + render pipeline
  editing.rs        # EditorState action sequences → buffer/cursor asserts
  footnotes.rs      # footnote edit primitives + mouse-follow path
  list_edit.rs      # list continuation, renumber, checkbox toggle
  mouse.rs          # mouse click / drag / scroll / checkbox
  palette.rs        # command palette filtering + selection
  renderer.rs       # parse + render → assert/snapshot
  source_map.rs     # unit + proptest tests for SourceMap invariants
                    #   (regressions saved in source_map.proptest-regressions)
  table.rs          # table navigation + structure edits
  ui.rs             # TestBackend widget rendering
  snapshots/        # committed insta .snap files

config/
  config.toml       # annotated reference config, written to
                    #   ~/.config/edamame/config.toml on first run
  keybindings.toml  # commented-out keybinding overrides reference
  export/default.css # default stylesheet bundled with self-contained HTML export
```

### Built-in themes

The `BUILTIN_THEMES` registry in `src/config/theme.rs` lists every compiled-in theme (Edamame, Dracula, Nord, Gruvbox, Catppuccin, …; see the registry for the full set). Each constructor lives in its own file under `src/config/themes/`.

`Config::ensure_default_files` creates `themes/` empty — built-in theme files are never written to disk. `read_theme_named` short-circuits to `Palette::builtin(name)` before any disk read, so a user file with a built-in name is ignored entirely. To add a new built-in: write a new `src/config/themes/<name>.rs` with a `pub fn theme()` and add it to `BUILTIN_THEMES`; both the load path and the theme-picker / settings cycle (via `list_theme_names`) pick it up automatically. Custom user themes go in `~/.config/edamame/themes/<name>.toml` under any name not in the registry.

### Architectural layers

Higher layers depend only on lower ones:

1. `main` — CLI args, config load, terminal lifecycle
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

Every top-level module has both a file (`src/config.rs`) and a subdirectory (`src/config/`). The file declares submodules with `pub mod` and re-exports public types with `pub use`. This keeps call-site imports clean:

```rust
use crate::config::{Config, KeyMap, Theme};   // not crate::config::config::Config
```

Always follow this pattern when adding new top-level modules. Several mid-level modules (e.g. `editor::mouse_ops`, `editor::list_edit`, `markdown::parser`, `markdown::renderer`, `config::themes`, `config::theme_file`, `ui::rendered_view`, `app::modal`) follow the same pattern recursively.

## Architectural Notes

These decisions are easy to break if you don't know they exist.

### Hybrid editing model

- **Virtual blocks for blank lines.** `ParsedDoc::build` synthesises a one-byte block for every blank line in the source (leading, between-block, and trailing). The cursor lands on each blank line as its own block, and the blank line is preserved in `RenderedView` even when the surrounding cursor block is replaced with raw text. Don't reintroduce `parse_offsets::covering_ranges` for the cursor mapping — it absorbs blank-line bytes into adjacent blocks and breaks navigation.
- **`per_block_own` vs. extended ranges.** `ParsedDoc` tracks both per-block *own* rendered line counts (for the raw-replacement region in `RenderedView`) and *extended* covering ranges (for cursor lookup). Mixing them up causes gap blank lines to collapse when the cursor enters the previous block.
- **Jitter-suppression reveal.** `EditorState::cursor_block_revealed()` returns false during a 120 ms `RAW_REVEAL_DELAY` after the cursor enters a new *buffer line* (not block). `RenderedView` keeps the block fully rendered and draws an inverted-cell cursor indicator at `(cursor_col, cursor_row)` until the delay elapses. The app loop uses `rx.recv_timeout(60 ms)` so the redraw fires without a keypress.
- **Single shared `line_render` module.** `PreviewView` and `RenderedView` both call `ui::line_render`. The trailing-cell background fill and word-aware wrap live there. If you change one view's wrap or fill, change the shared function — don't fork it.
- **NBSP padding in code blocks.** Blank lines inside fenced code blocks use U+00A0 (NBSP) padding, not regular spaces. This works around a ratatui `WordWrapper` (`trim: false`) bug where an all-whitespace line produces an extra empty visual row. Don't "simplify" this back to spaces.
- **Word-group undo merging.** `History::record` merges single alphanumeric inserts into the previous delta when offsets are contiguous. Cursor moves break the group naturally (next insert lands at a different offset). It's still delta-based, not snapshot-based.
- **Visual line navigation.** `move_up_visual` / `move_down_visual` (in `editor/state_cursor_visual.rs`) and `line_render::render_line` must use the same wrap algorithm (`visual_rows_of_str` / `sub_line_of_col`). Otherwise the cursor lands in a different column than where it appears on screen.
- **Action enum is the full surface.** Every action lives in `config/keymap.rs::Action`. Keybindings stay stable even when a feature is in flight; unimplemented variants are no-ops in `edit_ops` until wired up.
- **Clipboard is feature-gated.** `arboard` is behind the `clipboard` Cargo feature (on by default). When disabled, copy/cut/paste use the in-process kill-ring only. Tests assert against the kill-ring, not the OS clipboard, to avoid cross-test races. On Wayland the `wayland-data-control` feature is required for read access — without it `Clipboard::new()` returns `Err`.

### Keyboard and mouse input

- **Two-layer mouse dispatch.** `MouseDispatcher` (in `src/input/mouse.rs`) is a pure state machine that turns crossterm `MouseEvent`s into semantic `MouseAction`s (click-count, drag, scroll). `mouse_ops::apply` (in `src/editor/mouse_ops/`) is where those actions mutate `EditorState`. Keep the split strict — coordinate translation belongs in `mouse_ops::coord`, click counting belongs in `MouseDispatcher`.
- **Mouse enable is gated by capabilities.** `terminal::enable_mouse()` is only called from `main` when `capabilities.mouse` is true. The app also gates `MouseDispatcher::dispatch` on `capabilities.mouse` so a fake mouse event can't drive the editor on a terminal where mouse wasn't enabled.
- **Drag anchor lives in `App`, not `EditorState`.** The `drag_anchor: Option<usize>` on `App` persists the mouse-down offset across events so the Drag handler can extend the selection. It's intentionally a UI-layer fact, not a document-layer fact, and clearing it doesn't need to go through the undo stack.
- **Mouse scroll uses a different bound than keyboard scroll.** `mouse_ops::selection::scroll_by_mouse` allows `max = total - 1` (last line at top of viewport) and never invokes `clamp_cursor_to_viewport_top`. Keyboard scroll (`Action::ScrollDown`) uses `EditorState::scroll_down` which keeps the cursor visible. Do not merge the two paths — the requirement is that mouse scroll specifically does not move the cursor. Keyboard `ScrollUp`/`ScrollDown` always step by exactly one line; the configurable `editor.mouse_scroll_lines` setting applies to the mouse wheel only.
- **Click-to-offset is approximate for formatted text.** Rendered inline styling (`**bold**` → `bold`) shifts char positions between raw and rendered. `mouse_ops::coord::rendered_sub_line_to_offset` maps the visual column 1:1 to the raw source column, which is exact for unformatted lines and off by a few chars for styled spans. The `RAW_REVEAL_DELAY` then turns the cursor's line raw so the user can correct on a second click. `markdown::InlineColMap` provides an exact raw↔rendered column map where precision matters (selection highlight projection in `RenderedView`).
- **Link hit-test is a source-scan shortcut.** `mouse_ops::links::link_at_offset` scans the line's raw bytes for balanced `[...](...)` — it is NOT driven by the AST. Upgrade to an AST-backed registry if reference-style links or autolinks need precise hit-testing.
- **Checkbox toggling short-circuits cursor placement.** `mouse_ops::checkbox::toggle_checkbox_at` runs BEFORE `click_to_char_offset` in the `MouseAction::Click` arm. A click on the `[ ]` glyph toggles and returns immediately — the cursor does NOT move. Clicks elsewhere on the task line fall through to normal placement.

### Modals, overlays, and the keybinds editor

- **Live `KeyMap` on `App`, draft inside the overlay.** `App::keymap: Option<KeyMap>` is built once in `run()` and held for the life of the process. The keybinds overlay opens with a *clone* of it (`KeybindsState::draft_keymap`) plus a cloned `KeyBindingOverrides`, and every rebind mutates only the draft. Nothing is written back to `App::keymap` / `App::keybindings` (or to `keybindings.toml`) until the user activates the overlay's `[ Save ]` button — Esc and `[ Cancel ]` discard the draft so a mis-press is recoverable. On Save the overlay returns `KeybindsResponse::Save { keymap, overrides }` carrying the drafts; the modal adapter swaps them onto `App` and persists. Don't regress to mutating the live keymap on every keystroke — a fumbled chord would then only be recoverable by hand-editing `keybindings.toml`.
- **Combined view+edit keybindings overlay.** `OpenKeybinds` owns the unified view+edit overlay. There is no separate cheat-sheet variant; a user-supplied keybinding to the removed `Action::ShowCheatSheet` fails parsing with `KeyMapError::UnknownAction`.
- **`ModalView` is scrollable; the bespoke overlays are not.** `ModalState` carries `scroll`, `last_total`, `last_visible`, plus `scroll_by(i32)`. Up / Down / PgUp / PgDn / Home / End route to scroll, never to button focus — Left / Right and Tab / Shift-Tab still cycle buttons. Mouse-wheel events are forwarded into open `ModalView` slots via `modal_wheel_delta` in the run loop. The palette / settings / keybinds overlays don't scroll because their bodies fit comfortably.
- **External-editor flow needs three things.** When the settings overlay's "Open config.toml in default editor" fires, the App must (1) pause its crossterm read thread, (2) drain the rx channel, and (3) suspend the terminal — in that order — before `Command::new($EDITOR).status()`. Skip any of these and the editor races our read thread for stdin: bytes get split, keystrokes feel laggy, and OSC responses to startup-time queries leak into the buffer. The read thread is poll-based (`crossterm::event::poll(100ms)`) precisely so a `read_paused: Arc<AtomicBool>` flag can stop it without having to interrupt a blocked `read()` syscall. After the editor exits, `terminal::re_enter(mouse, keyboard_enhancement)` reinstates alt-screen + raw mode + transient features, and `Config::load()` is re-run so any edits take effect immediately. See `src/app/external_editor.rs`.
- **`Modal::kind` and `Modal::dismissable` are stored as struct fields, not hard-coded.** Every modal that uses `ModalView` carries `kind: ModalKind` and `dismissable: bool` fields set once in `new()`/`from_*()`. The `ModalView::new(.., self.kind, self.dismissable)` call, the `state.handle_key(.., self.dismissable)` call, AND the trait methods `fn kind()` / `fn dismissable()` all read from those fields — single source of truth per modal. Do NOT pass literals (`true`/`false`, `ModalKind::Warning`) at any of those three sites; they will drift. The `dismissable` field controls three things together: the rendered `esc` close-hint, the cached `esc_button_rect` for click hit-testing, AND whether `Esc`/`n`/`N` actually fire `ModalResponse::Cancelled`. Modals that don't use `ModalView` (palette, settings, keybinds, save_copy, insert_table, theme_picker, export_theme, welcome) inherit the trait defaults (`Normal` / `true`); don't add no-op overrides.
- **Construct `ModalView` via `ModalView::new(...)`, not a struct literal.** The constructor pre-fills `max_pad_h` to `MAX_PAD_H` (4); chain `.with_max_pad_h(n)` to override for a modal whose content reads cramped. Struct-literal construction would force every call site to spell out `max_pad_h: MAX_PAD_H` and break silently the next time the default changes.
- **Horizontal padding lives on `ContentSize`, not on `FrameOpts`.** `FrameOpts.content` embeds the same `ContentSize` value fed to `centered_rect_for_content`, so the pre-render sizing pass and the post-render `draw_frame` padding can never disagree. Set `max_pad_h` once on the `ContentSize` (or take the default `MAX_PAD_H` via `..Default::default()`) and pass that one value to both calls. Do NOT reintroduce a parallel `FrameOpts.max_pad_h` field. The keybinds overlay raises `max_pad_h` to 8 because its bindings table is dense and the "Already bound to …" error string would otherwise reflow the modal during capture.
- **Preview-mode Ctrl-key allowlist.** `input::mode_handler::default::preview_safe_action` decides which Ctrl-* chords fire in Preview mode. Read-only overlay openers (`ShowCommandPalette`, `OpenSettings`, `OpenKeybinds`, `OpenConfigFolder`, `ShowMarkdownCheatSheet`, `SwitchTheme`, `CreateCustomTheme`) belong on the allowlist — adding a new modal-opening action means adding it here too, otherwise the chord will silently no-op in Preview.
- **Focus vs. persistent selection (modal styling convention).** When a modal carries a *persistent selection* that's independent of focus — e.g. the welcome modal's `Ask | Always | Never` pill rows, the `Don't show this again` checkbox, or any future form whose value survives focus moves — use this three-tier styling: - Focused element → `theme.modal_button_focused`   (`primary` bg + REVERSED + bold). Filled, strongest. - Persistent selection *without* focus →   `theme.modal_item_selected_unfocused` (`secondary` **fg** on   `surface_elevated`, bold). Outlined, no fill — never reads the same as   the focused element. - Neither → `theme.modal_item` (plain text on `surface_elevated`).

Don't reuse `modal_item_selected` for "selected but unfocused" — it also uses a filled `primary` bg, which collides with the focused affordance. For composite affordances (checkbox glyph + label), apply the unfocused-selection style to the *glyph only*, not the full row. See [`docs/theming.md`](docs/theming.md) §"Focus vs. persistent selection" for the rationale and the monochrome fallback.

### Images, diagrams, and export

- **Decode happens off the UI thread.** `image::loader` runs in a worker; results arrive as `AppEvent::ImageReady` in the main loop. URL fetches use `ureq` (rustls, no system OpenSSL). Failures are memoised so a reparse won't re-issue a doomed request every frame.
- **ratatui-image encode uses a second worker.** `ResizeRequest` / `ResizeResponse` are routed through `AppEvent::ProtocolReady` and a pending-request FIFO. Failures still pop the queue so a placeholder stays visible until a later frame re-enqueues.
- **Mermaid diagrams use synthetic cache keys.** `diagram::mermaid` renders Mermaid → SVG → PNG via `mermaid-rs-renderer` + `resvg`/`usvg`, and the cache key is `diagram-mermaid-<sha256>` so the image cache reuses an already-rendered diagram across reparses. Pin the `mermaid-rs-renderer` version exactly (`=0.2.2`) — the crate is pre-1.0 with known panic bugs and rapid churn; `resvg`/`usvg` versions must match what it transitively depends on so cargo doesn't compile two copies.
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
- `StatefulWidget` for widgets that mutate scroll/cursor state; `Widget` for purely functional rendering (no mutable state needed)

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

Use `tracing` macros (`tracing::info!`, `tracing::debug!`, etc.) — **never** `println!` or `eprintln!` (would corrupt the TUI). Logging is only initialized when `[dev] logging = true` in config, so tracing calls in production are no-ops.

### Tests

- Every source file gets a `#[cfg(test)] mod tests { ... }` block for unit tests
- Integration tests in `tests/` import from the library crate (`edamame::`)
- Use `insta::assert_debug_snapshot!` for complex output (ASTs, rendered lines)
- Use `ratatui::backend::TestBackend` for widget rendering tests
- Use `Box::leak(Box::new(Theme::default()))` to produce `&'static Theme` in tests without lifetime annotations — this is intentional and safe in tests
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
| `insta` | Snapshot testing |
| `proptest` | Property-based testing |
