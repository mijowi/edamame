# AGENTS.md — edamame

Guidance for human and agentic contributors working in this repository. `CLAUDE.md` is a symlink to this file; edit one, update both. 

## Project Overview

`edamame` is a Rust TUI application for viewing and editing Markdown files in the terminal. It uses `ratatui` for rendering, `pulldown-cmark` for parsing, and `ropey` for rope-based text editing. The crate ships as both a binary (`edamame`) and a library (so integration tests can import it).

> **Security:** edamame opens untrusted documents, so any change to a content-handling path (image/SVG decode, remote fetch, Mermaid, link opening, HTML export, subprocess spawning) must preserve the hardening in [`docs/security.md`](docs/security.md). Read it — and its "Invariants for contributors" — before touching those areas.

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

**Test frameworks in use:**
- `insta` — snapshot testing (`assert_debug_snapshot!`, `assert_snapshot!`)
- `proptest` — property-based testing (used in `tests/source_map.rs`)
- `tempfile` — temporary files for I/O tests
- `ratatui::backend::TestBackend` — headless widget rendering

**Do not** write tests for: terminal capability detection that depends on live terminal probing (`Picker::from_query_stdio`), cross-platform clipboard (OS clipboard paths race between parallel tests), or the actual crossterm terminal-mouse wire protocol — these are covered by manual smoke testing. *Do* test mouse logic at the `MouseDispatcher` + `mouse_ops::apply` layer: both are pure functions of an input event and an editor state. 

## Project Structure

```
src/
  main.rs           # CLI args, config load, terminal init, App::run.
                    #   Declares NO modules — it `use`s the library crate
                    #   (`use edamame::app::App`).  Re-declaring the tree
                    #   here would compile a second private copy, hiding
                    #   its unit tests from `cargo test --lib`.
  lib.rs            # declares all modules, `app` included (enables both
                    #   integration tests and the binary)

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
      overwrite_confirm.rs, quit_confirm.rs, remote_image.rs, save_as.rs,
      settings.rs, terminal_capabilities.rs, theme_picker.rs, welcome.rs,
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
    list_layout.rs  # list-marker raw ↔ rendered column geometry, shared by
                    #   the overlay painters and the mouse click mapping
    parse_offsets.rs  # (byte_start, byte_end) spans from pulldown-cmark;
                    #   RangeTracker incremental depth-0 scanner
    parser.rs       # pulldown-cmark → Vec<Block>; parse_raw_with_ranges
                    #   (single-pass blocks + ranges); promotes images /
                    #   diagrams / html comments to their own blocks;
                    #   annotates loose-list items with blank counts
    parser/
      post_pass.rs  # promotion + loose-list blank annotation transforms
    render_cache.rs # RenderCache: block-level render memoization keyed by
                    #   Block value + render-settings fingerprint
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
                        #   (render_button_at); built on controls::button_style
    cap_summary.rs      # capabilities-notice body lines
    command_palette.rs  # PaletteView + PaletteState (nucleo-matcher fuzzy)
    command_palette/actions.rs   # palette-eligible Action list
    content_width.rs    # measure expected wrapped width
    controls.rs         # unified control family: Control enum (Toggle / Pill),
                        #   toggle_spans / pill_spans, the shared style helpers
                        #   (control_label_style, button_style, focused_style),
                        #   cycle_index + apply_images_cascade
    cursor.rs           # text_field_spans, split_at_char, CURSOR_BLOCK —
                        #   shared block-cursor helpers
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
    rendered_view/      # paint, cell_overlay, raw_text — split
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
  search.rs         # search-flow lifecycle, hint row, highlight painting
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

**Indexed-color substitution.** `256 Dark` / `256 Light` are the only built-ins authored against the xterm-256 cube; every other theme picks 24-bit colors that an indexed terminal quantizes, routinely landing fg and bg on the same entry. So on a terminal without truecolor, `app::theme_fallback::apply` swaps `config.theme` for the matching indexed built-in (dark/light follows the *current theme's* appearance, not just the `appearance` key) and `ThemeDowngradeModal` reports it. Themes that already render correctly below truecolor are exempt — `theme::INDEXED_SAFE_THEMES` — and `indexed_fallback_theme` returns `None` for them: the two `256 *` targets (which is also what makes the swap idempotent across reloads) plus `Monochrome Dark`, whose every palette slot is `Color::Reset` and so is correct at any depth including `NoColor`. Substituting a safe theme would trade a working palette for a less neutral one *and* raise a modal to explain it, so the exemption covers the warning as much as the swap; `indexed_safe_themes_are_registered` pins the list against `BUILTIN_THEMES` so a rename can't silently drop one. The standalone `ThemeDowngradeModal` carries the news in every case except when the capabilities notice fires on the same launch (a first visit to that terminal), in which case the notice absorbs the same prose via `with_theme_downgrade` and the standalone modal is dropped, so one terminal change produces one modal. Both render `ui::cap_summary::theme_downgrade_lines`, and both hand `ModalView` *paragraphs*, not pre-broken lines — it wraps the body itself. Three invariants: (1) the swap happens **before the first frame** in `App::new` — a modal explaining unreadable colors that is itself unreadable is worse than useless; (2) it is **never persisted** — the user's choice is stashed in `Config::theme_downgraded_from` and written back in `theme`'s place by `Config::save`, because one `config.toml` is typically shared with a truecolor machine; (3) both paths that resolve a theme from a freshly-read `Config` must call `theme_fallback::apply` — startup *and* the external-editor reload, which would otherwise repaint the session in the palette we just swapped away from. `NoColor` is exempt at the *depth* end for the mirror-image reason: `App::new` passes `monochrome` to `Theme::from_file`, which strips every color whatever the active theme, so a swap there would be invisible and its modal pure noise. Below 24-bit color `App::media_renderable` also refuses images and diagrams outright — same quantization argument, and likewise session-only, so a persisted `Always` survives in `config.toml` for the user's capable terminal. **That session-only promise is enforced at the write sites, not just asserted:** `WelcomeModal::save_outcome` writes the three media fields only when `image_capable`, so the `Never` the modal *displays* below truecolor is never persisted (the forcing that matters is `media_renderable`, which refuses to decode regardless of what `config` says). And when the notice carries the downgrade it drops its "Adjust settings" button — with theme, images, and diagrams all disabled there, the welcome modal has nothing left but the vim toggle. An explicit user pick clears the theme stash via `Config::set_theme` (the picker's live-preview writes deliberately do not — `Esc` must restore the substitution).

**While the stash is set, `config.theme` is effectively unwritable** — `Config::save` puts `theme_downgraded_from` in its place — so every path that commits a theme *choice* must go through `Config::set_theme`, which clears it: the theme picker's selection and the export-theme modal's newly created theme. A bare `app.config.theme = …` on either path writes the theme to screen but silently drops it on the floor at save time, and the user sees their choice vanish on next launch with no error. The only legitimate direct assignments are `theme_fallback::apply` itself and the picker's transient preview / revert.

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
- **Loose lists stay one block; blanks are annotated, not split.** A blank line between list items makes the list "loose" (CommonMark). edamame keeps it a single `Block::List` and records how many blank source lines precede each item in `ListItem::blank_lines_before` (`parser::post_pass::annotate_list_blanks`, reusing the fence-aware blank-run scanner). The renderer emits that many blank `Line`s before the item, so a loose list keeps its legibility spacing while numbering comes straight from pulldown-cmark (a source that restarts at `1.` after a blank renders sequentially — matching CommonMark, unlike the old split-into-separate-lists behavior). Don't reintroduce a `split_lists_on_blank_lines` pass: fragmenting the list forced per-group `start` re-derivation and block/range surgery for no rendering benefit. **The reveal depends on this staying 1:1:** `RenderedView` maps a block's rendered rows to its source lines via `block_text.split('\n')`, so every separator blank must emit exactly one rendered row. The raw→rendered line mapping — `editor::state::cursor_sub_line_in_block`, the single implementation — therefore counts *separator* blanks (a blank run ending at a top-level marker) as rendered rows while skipping interior-item blanks and soft-break continuations, and don't revert it to "count only non-blank lines" (that swallows the blank row and reveals the cursor line one row too high). **Three callers must agree on it and so must never re-derive it:** `RenderedView` (which rendered row gets the raw-text replacement), `cursor_rendered_line_idx` (where the cursor appears, for scroll arithmetic), and — through the latter — `mouse_ops::coord`'s `revealed_cursor_line` shortcut. When they drift, a click on a revealed line is mapped against the *rendered* spans instead of the raw text on screen, so a line whose markers were dropped (`` `code` ``, `**bold**`) places the cursor short by the marker width. **Its *inputs* are shared too, for the same reason:** `rendered_view::raw_text::raw_block_cursor` is the single derivation of the `(block source, raw line index, column)` triple both callers feed it — the line index is an index *into* that source, so deriving one without the other is how they drift at the edges (the two hand-written byte walks disagreed on where a cursor at the block end lands: last line vs. block top). `RenderedView` keeps exactly one branch of its own, for a stale parse, where it rebuilds the triple from `cursor_block_line_range` so just-typed characters are visible before the reparse.
- **Jitter-suppression reveal.** `EditorState::cursor_block_revealed()` returns false during a 120 ms `RAW_REVEAL_DELAY` after the cursor enters a new *buffer line* (not block). `RenderedView` keeps the block fully rendered and draws an inverted-cell cursor indicator at `(cursor_col, cursor_row)` until the delay elapses. The app loop uses `rx.recv_timeout(60 ms)` so the redraw fires without a keypress.
- **Single-pass parse.** `ParsedDoc::build` gets blocks AND top-level byte ranges from one `parse_raw_with_ranges` call — a `parse_offsets::RangeTracker` observes the same offset-iterator events the AST builder consumes, so blocks↔ranges stay 1:1 by construction. Don't reintroduce a second `top_level_block_ranges` pass alongside `parse_raw`; the two-pass pairing cost a full extra pulldown-cmark parse per reparse (see docs/perf-benchmark-plan.md).
- **Block-level render memoization.** `EditorState` owns a `markdown::RenderCache` threaded into every `refresh_parsed`; blocks whose AST value is unchanged reuse their rendered lines (a clone) instead of re-rendering. The cache key is the `Block` value itself plus a render-settings fingerprint (theme address, viewport width, striping, big-H1, …) — keying by AST, not source bytes, is what keeps live table-width drags and post-pass mutations correct (a mutated block simply misses). `Block::ImageBlock` is never cached because its row count tracks the image decode cache, which changes without an AST change. Eviction is by document membership per build, like the image-cache GC. If you add a `Renderer` knob that changes rendered output, add it to `RenderSettings` or stale cache hits will paint with the old setting.
- **Single shared `line_render` module.** `PreviewView` and `RenderedView` both call `ui::line_render`. The trailing-cell background fill and word-aware wrap live there. If you change one view's wrap or fill, change the shared function — don't fork it.
- **The cursor is a uniform block; color (not shape) signals context.** Every cursor in edamame is a fake block: the cell at the insertion point is recolored with the cursor style while the underlying character stays visible. The cursor color is resolved in one place — `app::cursor_style::editor_cursor_style` — and the views receive the resolved `Style` rather than picking a cursor color themselves. The default handler colors by *view* mode (`status_mode_preview` / `status_mode_rendered` / `status_mode_raw`); under the vim handler the cursor **mirrors the sub-mode chip**, reading the same `status_mode_vim_normal` / `_insert` / `_visual` fields the status bar uses (minus the chip's `BOLD`) so chip and cursor can never drift. RAW (`status_mode_raw`) is surfaced only in INSERT; NORMAL/VISUAL keep their sub-mode color in every view, matching the chip (no `(RAW)` suffix). Modal inputs use `theme.cursor` (its own `accent` block). There is no bar/caret shape and no `CursorShape` enum — a vim command sub-mode reads the same shape as Insert, distinguished by color + the status chip. The editor cursor is still painted via `cursor_col_override` (not baked into the wrapped `Line`) so the word-aware wrap keys on the *glyph-free* source text and stays in lockstep with `move_up_visual`/`move_down_visual` and the scroll math: `line_render::paint_row` recolors the resolved cell, the raw-reveal builders in `rendered_view/paint.rs` (`make_raw_line_with_selection`, `make_code_styled_body_line`) and `raw_view::raw_display_line` build the line without the cursor, and `overlay_raw_cell` recolors the table cell in place. A block cursor sitting on a selected/highlighted cell wins over that wash — the cursor cell shows the cursor color, not the selection bg.
- **Modal input cursors are blink-stable.** Every modal text field renders its block cursor via `ui::cursor::text_field_spans`, which always emits a one-cell slot — the character under the cursor recolored when the blink is on, shown plainly (a space past end-of-line) when off — so the field never changes width between blink phases. A new text-input modal must use this helper (or, where a value flows through `format_modal_row`/horizontal scroll and the cursor cell can't carry its own style, mirror its constant-width slot with `cursor::CURSOR_BLOCK`) rather than conditionally pushing the cursor glyph.
- **NBSP padding in code blocks.** Blank lines inside fenced code blocks use U+00A0 (NBSP) padding, not regular spaces. This works around a ratatui `WordWrapper` (`trim: false`) bug where an all-whitespace line produces an extra empty visual row. Don't "simplify" this back to spaces.
- **Word-group undo merging.** `History::record` merges single alphanumeric inserts into the previous delta when offsets are contiguous. Cursor moves break the group naturally (next insert lands at a different offset). It's still delta-based, not snapshot-based.
- **Visual line navigation.** `move_up_visual` / `move_down_visual` (in `editor/state_cursor_visual.rs`) and `line_render::render_line` must use the same wrap algorithm (`visual_rows_of_str` / `sub_line_of_col`). Otherwise the cursor lands in a different column than where it appears on screen.
- **Action enum is the full surface.** Every action lives in `config/keymap.rs::Action`. Keybindings stay stable even when a feature is in flight; unimplemented variants are no-ops in `edit_ops` until wired up.
- **Clipboard is feature-gated.** `arboard` is behind the `clipboard` Cargo feature (on by default). When disabled, copy/cut/paste use the in-process kill-ring only. Tests assert against the kill-ring, not the OS clipboard, to avoid cross-test races. On Wayland the `wayland-data-control` feature is required for read access — without it `Clipboard::new()` returns `Err`.

### Search and replace

- **The search flow is gated on `EditorState::search.is_some()`, not a `Mode` variant.** Unlike diff (which replaces the whole view), search keeps the document rendering in the current view mode with match highlights painted on top, so adding a `Mode::Search` would force an "effective view mode" indirection through every render-dispatch site.
- **Only a *replace* flow captures input; a navigate-only flow is a non-capturing overlay.** `App::search_flow_captures()` is `search.as_ref().is_some_and(|s| s.is_replace_flow())` — vim and default mode alike. A **replace** flow needs the unmodified `Tab`/`r`/`a` flow keys, so it traps input: it is enforced at three choke points — `DefaultHandler::handle` intercepts the hard-bound flow keys (`search::search_keys`, same table-driven pattern as `diff_keys`), `App::dispatch_action` default-denies everything off the `search_safe_action` allowlist *before* `handle_app_action` runs, and `dispatch_mouse_event` drops all mouse input except wheel scroll and pointer moves. A denied action flashes "Not available during search" via `App::flash_action_unavailable` (the diff gate's helper, "Not available during diff review"). `search_safe_action` allows the flow keys plus read-only navigation (cursor moves, selection, `SelectAll`, `Copy`) and the always-safe set (scroll, overlay openers, save, quit, in-flow undo/redo); a new buffer-mutating app-level action is denied automatically — add it to the allowlist only if it is genuinely read-only. A **navigate-only** flow (vim's `/`, `Ctrl-F` find with the replace field empty) does *not* capture: it is a lightweight highlight overlay (vim `hlsearch` / VS Code find widget). The user keeps full editing freedom; only `Tab`/`Shift+Tab` (next/prev, plus vim's `n`/`N`) and `Esc` (dismiss) are intercepted ahead of the keymap — `DefaultHandler` returns only `SearchNext`/`SearchPrev`/`SearchExit` and `dispatch_action` routes just those to `dispatch_search_action`; everything else falls through to normal editing.
- **A non-capturing flow's match list is refreshed every frame.** Because a navigate-only flow lets the buffer be edited outside the in-flow mutation paths, `prepare_viewport` calls `EditorState::ensure_search_fresh` each frame (version-guarded → a no-op when nothing changed) so the overlay painter and focus-scroll see live ranges.
- **Search exit is a motion — no scroll-back.** `EditorState::exit_search` just drops the session (`self.search = None`); it leaves the cursor and viewport on the match the user navigated to, matching vim's `/` and the VS Code find widget. There is deliberately no saved pre-search scroll to restore.
- **Vim `/` `?` search is incremental (incsearch).** While the search prompt is open, `vim_ops::incsearch::update_incsearch` rebuilds a real navigate-only `SearchState` from the input on every keystroke (typed, history-recalled, or pasted — all three route through `feed::cmdline_live_update`, shared with the `:s` preview), parks the cursor on the cursor-relative focus (`SearchState::focus_relative_to`, the same method `App::enter_vim_search` uses on submit), and scrolls it into view. Because the transient session *is* `EditorState::search`, the hlsearch painters, hint-line counter, and raw-reveal suppression work unchanged — no incsearch-specific render code exists. The `IncsearchSession` on `VimState` stashes the pre-prompt cursor/scroll and any prior hlsearch session; Esc restores all three, and Enter restores them *before* the `EnterSearch` outcome so the App-level submit resolves against the original cursor, byte-identical to a preview-less submit (the same revert-before-commit promise as the `:s` preview). Unlike that preview, incsearch never touches the buffer, so none of the preview's gates apply. The shared view primitives — `EditorState::place_cursor`, `restore_view`, `scroll_cursor_comfortably_into_view` (one TOP_MARGIN core also behind the hunk/match focus scrolls) — are the single implementations; don't hand-roll a cursor park or a context-margin scroll in a new flow.
- **Match freshness is version-keyed.** `SearchState` stores byte ranges valid for the `Buffer::version()` they were computed against; every in-flow mutation path (replace, replace-all, undo, redo) calls `EditorState::ensure_search_fresh` afterwards. The render layer additionally clamps each range against the live source so a stale list can never panic — but don't rely on that: a new mutation path must refresh. Wholesale content swaps (`replace_buffer`, diff entry) drop the session entirely.
- **Matching is smartcase for navigation, case-sensitive for replace.** `SearchState::ensure_fresh` picks the matcher by `is_replace_flow()`: a navigate flow (`/`, `n`/`N`, `Ctrl-F` find) uses `search::state::find_all` (smartcase — case-insensitive unless the pattern contains an uppercase char), so *every* edamame user gets smartcase, not just vim; a replace flow uses `find_all_cs` (always case-sensitive) so a lowercase find term never rewrites a casing variant the user didn't type (and the highlights match exactly what replace-all will hit). `find_all_cs` keeps `str::match_indices`; the smartcase case-insensitive path (`find_all_ci`) compares char-by-char against the **untouched** haystack so returned byte offsets stay on char boundaries for multibyte text (lowercasing up front would shift offsets for chars whose lowercase form differs in byte length, and the overlay painter slices the source by those offsets — see `rendered_view::paint`). There is deliberately **no regex** in `/` search; regex is confined to `:s`/`:%s` (CP9).
- **Replace keeps the reveal beat.** A single replace goes through `EditorState::apply_delta` (one undo delta) plus an immediate `flush_parsed_if_dirty` — the overlays and match recompute need fresh source-map ranges on the next frame, so don't let the in-line-edit deferral stand. It then refocuses past the inserted bytes (so a replacement containing the query can't trap the flow on one site) and arms `search_advance` (mirror of `diff_advance`) so the cursor jumps to the next match only after a 350 ms reveal. Replace-all is a single coarse `EditDelta` recorded onto the normal history stack — prior undo history is preserved, unlike the diff merge's `reset_with`.
- **A replace flow leaves Preview.** Preview is browse-only, so `App::enter_search_flow` transitions Preview → Rendered when the replace field was filled (mirroring the first keystroke of a normal edit). Navigate-only flows, and zero-match queries that never enter the flow, leave the mode untouched.
- **Raw reveal is suppressed during search.** `cursor_block_revealed()` returns false while the flow is active so blocks don't flip between rendered and raw under the highlights as the user tabs through matches.
- **Highlight painting is shared.** Rendered + Preview matches paint through `paint_search_overlays` → `paint_byte_range_overlay` (the generalized former `paint_selection_overlay`) called from `EditorView` as a post-pass; Raw mode paints per-char inside `RawView`. The focused match uses `theme.selection`, all others the muted `theme.selection_muted` variant. The painter's block-kind prefix shifts (heading space prefix, code-block pad cell) resolve the block via `ParsedDoc::real_block_for_byte` — never index `parsed.blocks` with a `source_map` block index; the source map's index space counts blank-line virtual blocks, so the two diverge in any document with blank lines.

### Live `:s` substitution preview (vim `inccommand`)

- **The preview transiently rewrites the real buffer — through raw `Buffer` edits only.** While the vim `:` command line holds a complete-enough `:s`/`:%s`/`:'<,'>s`, `vim_ops::preview::update_substitute_preview` applies the substitution via raw `Buffer::insert`/`remove` (never `EditorState::apply_delta`), so no undo delta is recorded and `dirty` is untouched. Every keystroke reverts the previous preview and recomputes against the pristine buffer — never diff two previews. On Enter the reducer reverts *before* `submit_ex`, so `execute_substitute` runs against the untouched buffer and commit semantics (single undo unit, flash text, cursor park) stay byte-identical to a preview-less submit. `SubstitutePreview` lives on `EditorState` (like `search` / `yank_flash`) so the painters read it off `&EditorState`.
- **The revert delta is version-stamped as a fail-safe.** The stashed inverse `EditDelta` carries the `Buffer::version()` it was applied at; a revert on a mismatched version silently drops the preview instead of corrupting text. `replace_buffer` (external reload) also drops any preview. Don't rely on the stamp: any new mutation path reachable while the cmdline is open must be gated.
- **Three gates hold while `substitute_preview.is_some()`.** (1) `tick_autosave` / `autosave_deadline` skip entirely (same pattern as the diff-mode guard) — raw preview edits bump `version` without touching `dirty`, so on an already-dirty buffer the debounce would otherwise arm and write preview text to disk. (2) `dispatch_mouse_event` blocks everything but wheel scroll and pointer moves (shares the capturing-search gate) — a click or checkbox toggle would mutate text that is about to revert. (3) `prepare_viewport` skips `ensure_search_fresh`, and both search-overlay painters early-out — a coexisting hlsearch session's byte ranges are stale against preview text; the session survives untouched and repaints after the revert. Keyboard needs no gate: the cmdline captures every key. Additionally `cursor_block_revealed()` returns false while a preview is active (same as during search) — the preview parks the cursor on the first affected line, and the reveal delay would elapse mid-typing, flipping that block to raw source under the highlights.
- **Compute is pure and shared with the commit path.** `ex::build_substitution` (the extracted line walk of `execute_substitute`) produces the single `EditDelta` plus the post-apply byte ranges of each inserted segment; `preview::compute_preview_plan` is a pure function of `(Buffer, cursor_line, Substitution, visual_range)` — the unit-test seam. `Substitution::replacement_present` (did the user type the second delimiter?) distinguishes highlight-only `:%s/foo` (match ranges, first per line without `g`, no edit) from deletion preview `:%s/foo/` (edit applied, zero-width highlight ranges filtered out).
- **The preview regex is bounded; the commit regex is not.** The preview builds its `fancy-regex` with `backtrack_limit(100_000)` and caps the walk at 1 000 matches (later lines stay original until submit) so a pathological half-typed pattern (`(a+)+b`) fails fast per keystroke. Parse/regex errors and matchless patterns silently end the preview session — never flash an error mid-typing. Painting reuses the search walk: `paint_substitute_preview_overlays` in Rendered/Preview, an inline branch in `RawView`, all ranges in the single `theme.selection` style (no focus concept, matching nvim's one `Substitute` group).

### Keyboard and mouse input

- **Two-layer mouse dispatch.** `MouseDispatcher` (in `src/input/mouse.rs`) is a pure state machine that turns crossterm `MouseEvent`s into semantic `MouseAction`s (click-count, drag, scroll). `mouse_ops::apply` (in `src/editor/mouse_ops/`) is where those actions mutate `EditorState`. Keep the split strict — coordinate translation belongs in `mouse_ops::coord`, click counting belongs in `MouseDispatcher`.
- **Mouse enable is gated by capabilities.** `terminal::enable_mouse()` is only called from `main` when `capabilities.mouse` is true. The app also gates `MouseDispatcher::dispatch` on `capabilities.mouse` so a fake mouse event can't drive the editor on a terminal where mouse wasn't enabled.
- **Drag anchor lives in `App`, not `EditorState`.** The `drag_anchor: Option<usize>` on `App` persists the mouse-down offset across events so the Drag handler can extend the selection. It's intentionally a UI-layer fact, not a document-layer fact, and clearing it doesn't need to go through the undo stack.
- **Mouse scroll uses a different bound than keyboard scroll.** `mouse_ops::selection::scroll_by_mouse` allows `max = total - 1` (last line at top of viewport) and never invokes `clamp_cursor_to_viewport_top`. Keyboard scroll (`Action::ScrollDown`) uses `EditorState::scroll_down` which keeps the cursor visible. Do not merge the two paths — the requirement is that mouse scroll specifically does not move the cursor. Keyboard `ScrollUp`/`ScrollDown` always step by exactly one line; the configurable `editor.mouse_scroll_lines` setting applies to the mouse wheel only.
- **Click-to-offset is approximate for formatted text.** Rendered inline styling (`**bold**` → `bold`) shifts char positions between raw and rendered. `mouse_ops::coord::rendered_sub_line_to_offset` maps the visual column 1:1 to the raw source column, which is exact for unformatted lines and off by a few chars for styled spans. The `RAW_REVEAL_DELAY` then turns the cursor's line raw so the user can correct on a second click. `markdown::InlineColMap` provides an exact raw↔rendered column map where precision matters (selection highlight projection in `RenderedView`). **A click on an already-revealed line, though, is exact and must stay that way** — the user is looking at raw source, so `coord`'s `revealed_cursor_line` branch maps the column straight onto the raw chars. That means laying the raw line out exactly as the painter did: `RenderedView` hands the raw text to `render_line`, which derives a *hanging indent* from the leading marker, so a revealed `- item` wraps its continuations two cells in and against a narrower budget. Use `coord::revealed_raw_rows` (both for the row count and for the column mapping, shifting `col` by the indent on any sub-row past the first) — never bare `visual_rows_of_str`, whose indent-0 assumption drifts the mapping further with every wrap. The indent it returns is the *effective* one: when the marker is as wide as the viewport (`indent + 1 >= width`) both `render_line` and `visual_rows_of_chars` fall back to a flat indent-0 layout, so reporting the raw marker width there would push every column of every continuation row into `char_idx_at_cell_col`'s forbidden-indent zone and collapse the row onto its first char. Any new caller of `compute_hanging_indent*` that pairs the indent with a wrap layout owes the same clamp.
- **Link hit-test is a source-scan shortcut.** `mouse_ops::links::link_at_offset` scans the line's raw bytes for balanced `[...](...)` — it is NOT driven by the AST. Upgrade to an AST-backed registry if reference-style links or autolinks need precise hit-testing.
- **Checkbox toggling short-circuits cursor placement.** `mouse_ops::checkbox::toggle_checkbox_at` runs BEFORE `click_to_char_offset` in the `MouseAction::Click` arm. A click on the `[ ]` glyph toggles and returns immediately — the cursor does NOT move. Clicks elsewhere on the task line fall through to normal placement.

### Unified UI controls

The interactive elements inside modals/overlays are one family, defined in `ui::controls`. The governing rule: **a control resolves its own styling from `controls`; the parent container only reports whether the control is `focused` / `disabled`.** Never hand-roll a focus style in a modal.

- **Four control flavors, declared at the definition site.** A toggle, a pill, a text input, and a button.
  - **Toggle** (`controls::toggle_spans`) — an on/off slider. It is the one control whose *widget* keeps its value color when focused (inverting it would destroy the on-is-green reading), so its focus is shown only by the row's label column.
  - **Pill** (`controls::pill_spans` over a `&[&str]`, e.g. the shared `ASK_ALWAYS_NEVER`) — a multi-value (2+) `‹ value ›` selector cycled with ←/→. On/off is **not** a pill flavor — a binary setting uses the Toggle. An option row declares which one it is via the `Control` enum (`Control::Toggle` / `Control::Pill(labels)`); don't reintroduce a `PillStyle::Toggle` that overloads the pill as a switch.
  - **Text input** — an inline editable value (`controls::text_value_style`; the blink-stable cursor comes from `ui::cursor::text_field_spans`).
  - **Button** — a bracketed press-to-act target; lives in `ui::button_row`, styled by `controls::button_style`.
- **Focus is one language; the label column is the single source of truth.** `REVERSED` means "filled affordance". `controls::focused_style` (= `theme.modal_button_focused`, a `primary` fill) is the shared focus fill. `controls::control_label_style(focused, disabled, theme)` resolves a labeled row's label column — focused → `modal_item_selected` fill, disabled → `modal_close_hint`, resting → `modal_item` — and **both** the settings overlay and the welcome modal call it. A focused row is one unit: pad the label across the whole column so the fill spans label → widget (the way the settings overlay does), rather than styling only the label glyphs.
- **Buttons go through `ui::button_row`, never a hand-built literal.** `render_button_row` (centered footer row) and `render_button_at` (left-aligned inline button, e.g. the welcome modal's "Switch theme") both build on `Button` + `controls::button_style`. Construct a `Button::bracketed(label)` and let the helper add the `[ … ]`, size the width, place it, and return the hit-rect — don't bake brackets into a string or count widths by hand.
- **Cycle + cascade logic is shared too.** Pill / toggle inputs route through `controls::Control::apply` (the single transition layer), whose pill arm and every index-valued caller delegate the wrap-around math to `controls::cycle_index`; `controls::apply_images_cascade` (images-`Never` forces remote-`Never`, stashing/restoring the prior choice) is likewise shared by both the settings overlay and the welcome modal so their behavior can't drift.

### Modals, overlays, and the keybinds editor

- **Live `KeyMap` on `App`, draft inside the overlay.** `App::keymap: Option<KeyMap>` is built once in `run()` and held for the life of the process. The keybinds overlay opens with a *clone* of it (`KeybindsState::draft_keymap`) plus a cloned `KeyBindingOverrides`, and every rebind mutates only the draft. Nothing is written back to `App::keymap` / `App::keybindings` (or to `keybindings.toml`) until the user activates the overlay's `[ Save ]` button — Esc and `[ Cancel ]` discard the draft so a mis-press is recoverable. On Save the overlay returns `KeybindsResponse::Save { keymap, overrides }` carrying the drafts; the modal adapter swaps them onto `App` and persists. Don't regress to mutating the live keymap on every keystroke — a fumbled chord would then only be recoverable by hand-editing `keybindings.toml`.
- **Combined view+edit keybindings overlay.** `OpenKeybinds` owns the unified view+edit overlay. There is no separate cheat-sheet variant; a user-supplied keybinding to the removed `Action::ShowCheatSheet` fails parsing with `KeyMapError::UnknownAction`.
- **`ModalView` is scrollable; the bespoke overlays are not.** `ModalState` carries `scroll`, `last_total`, `last_visible`, plus `scroll_by(i32)`. Up / Down / PgUp / PgDn / Home / End route to scroll, never to button focus — Left / Right and Tab / Shift-Tab still cycle buttons. Mouse-wheel events are forwarded into open `ModalView` slots via `modal_wheel_delta` in the run loop. The palette / settings / keybinds overlays don't scroll because their bodies fit comfortably.
- **Bracketed paste routes to the top modal's focused field.** When a modal is open, `dispatch_modal_event` forwards `Event::Paste` to `Modal::handle_paste` (pop-dispatch-push, like `handle_wheel`/`handle_click`); when no modal is open it goes to the editor buffer via `dispatch_paste` instead. The `Modal::handle_paste` default is a no-op `Continue`, so button-only modals ignore pastes; only the text-input modals (palette, search/replace, save-copy, export-theme, insert-table, theme/section pickers, settings field editor) override it to call their state's `paste()`. Every such `paste()` runs the payload through `ui::sanitize_paste` first — a single source of truth that strips control chars (so a multi-line clipboard collapses to one line) and caps at `PASTE_CHAR_CAP` (1024) — then layers field-specific policy and *mirrors that field's keyboard `Char` arm exactly* (append vs. cursor-insert, focus gating, digits-only, live preview). Don't flatten in the editor path: `dispatch_paste` keeps newlines because the buffer is multi-line. Adding a new text-input modal means overriding `handle_paste`; a new field means its `paste()` must match its typing behavior or paste and type will diverge.
- **External-editor flow needs three things.** When the settings overlay's "Open config.toml in default editor" fires, the App must (1) pause its crossterm read thread, (2) drain the rx channel, and (3) suspend the terminal — in that order — before `Command::new($EDITOR).status()`. Skip any of these and the editor races our read thread for stdin: bytes get split, keystrokes feel laggy, and OSC responses to startup-time queries leak into the buffer. The read thread is poll-based (`crossterm::event::poll(100ms)`) precisely so a `read_paused: Arc<AtomicBool>` flag can stop it without having to interrupt a blocked `read()` syscall. After the editor exits, `terminal::re_enter(mouse, keyboard_enhancement)` reinstates alt-screen + raw mode + transient features, and `Config::load()` is re-run so any edits take effect immediately. See `src/app/external_editor.rs`.
- **`Modal::kind` and `Modal::dismissable` are stored as struct fields, not hard-coded.** Every modal that uses `ModalView` carries `kind: ModalKind` and `dismissable: bool` fields set once in `new()`/`from_*()`. The `ModalView::new(.., self.kind, self.dismissable)` call, the `state.handle_key(.., self.dismissable)` call, AND the trait methods `fn kind()` / `fn dismissable()` all read from those fields — single source of truth per modal. Do NOT pass literals (`true`/`false`, `ModalKind::Warning`) at any of those three sites; they will drift. The `dismissable` field controls three things together: the rendered `esc` close-hint, the cached `esc_button_rect` for click hit-testing, AND whether `Esc`/`n`/`N` actually fire `ModalResponse::Cancelled`. Modals that don't use `ModalView` (palette, settings, keybinds, save_copy, insert_table, theme_picker, export_theme) inherit the trait defaults (`Normal` / `true`); don't add no-op overrides.
- **The welcome modal is dismissable only when it wasn't a first run.** It doesn't use `ModalView`, but it follows the same field-not-literal rule: `WelcomeState::dismissable` is the single source read by the `Esc` arm of `handle_key`, by `show_close_hint` in `render` (which is what populates `esc_button_rect`, so the click path needs no second gate), and by `Modal::dismissable`. `WelcomeModal::from_state` (first run) leaves it `false` — the spec replaces Cancel with the explicit "Show on next launch" toggle, and there is no prior choice to protect. Every on-demand opening — `Action::OpenWelcome`, the capabilities notice's "Adjust settings" button — goes through `WelcomeModal::new` and sets it `true` via `WelcomeState::with_dismissable`, because reopening carries a risk the first run doesn't: below truecolor `WelcomeState::new` force-sets images and diagrams to `Never` and `save_outcome` *persists* that forcing, so without a write-nothing exit, merely looking at the surface from a weaker terminal would overwrite the settings chosen on a capable one (`WelcomeResponse::Cancel` → a plain `ModalOutcome::Close`, and deliberately no fingerprint seeding — an on-demand opening is not the first-visit notice and shouldn't silence it).
- **Construct `ModalView` via `ModalView::new(...)`, not a struct literal.** The constructor pre-fills `max_pad_h` to `MAX_PAD_H` (4); chain `.with_max_pad_h(n)` to override for a modal whose content reads cramped. Struct-literal construction would force every call site to spell out `max_pad_h: MAX_PAD_H` and break silently the next time the default changes.
- **A prose modal must cap its content width; a tabular one must not.** `ModalView` sizes itself to its longest *unwrapped* body line, which is right for tables (capability rows, keybindings) and wrong for prose: a body that is one wrapped paragraph — as `ui::cap_summary::theme_downgrade_lines` deliberately is — has a natural width equal to the whole sentence, so the modal stretches across the terminal and renders as a few enormous lines. Chain `.with_max_content_width(PROSE_CONTENT_WIDTH)` — a count of *text* columns, so the outer modal is that plus `2 * max_pad_h` — on `ModalChrome::new` for a chrome-backed modal or on `ModalView::new` directly. It matches the welcome modal's hand-rolled `CONTENT_WIDTH`, the one other prose surface, so the two read at the same measure. The cap is raised to the button-row width before clamping, so it can never clip the footer. Don't instead pre-wrap the paragraph into short `Line`s — `ModalView` wraps and sizes with `wrapped_rows`, so hand-splitting double-wraps at narrow widths and leaves ragged rows at wide ones. The two modals that render the downgrade prose (`ThemeDowngradeModal` and the capabilities notice, which absorbs it) use the same cap so they stay visually interchangeable.
- **Horizontal padding lives on `ContentSize`, not on `FrameOpts`.** `FrameOpts.content` embeds the same `ContentSize` value fed to `centered_rect_for_content`, so the pre-render sizing pass and the post-render `draw_frame` padding can never disagree. Set `max_pad_h` once on the `ContentSize` (or take the default `MAX_PAD_H` via `..Default::default()`) and pass that one value to both calls. Do NOT reintroduce a parallel `FrameOpts.max_pad_h` field. The keybinds overlay raises `max_pad_h` to 8 because its bindings table is dense and the "Already bound to …" error string would otherwise reflow the modal during capture.
- **Preview-mode Ctrl-key allowlist.** `input::mode_handler::default::preview_safe_action` decides which Ctrl-* chords fire in Preview mode. Read-only overlay openers (`ShowCommandPalette`, `OpenSettings`, `OpenWelcome`, `OpenKeybinds`, `OpenConfigFolder`, `ShowMarkdownCheatSheet`, `SwitchTheme`, `CreateCustomTheme`) belong on the allowlist — adding a new modal-opening action means adding it here too, otherwise the chord will silently no-op in Preview.
- **Focus vs. persistent selection (modal styling convention).** When a modal carries a *persistent selection* that's independent of focus — e.g. the export-theme modal's highlighted theme name (`modal_item_selected_unfocused` at `export_theme_modal.rs`), or any form whose marked value survives focus moves — use this three-tier styling: (Note: ordinary labeled control rows — settings, welcome — are *not* this case; their focus styling is `controls::control_label_style`, see "Unified UI controls".) - Focused element → `theme.modal_button_focused`   (`primary` bg + REVERSED + bold). Filled, strongest. - Persistent selection *without* focus →   `theme.modal_item_selected_unfocused` (`secondary` **fg** on   `surface_elevated`, bold). Outlined, no fill — never reads the same as   the focused element. - Neither → `theme.modal_item` (plain text on `surface_elevated`).

Don't reuse `modal_item_selected` for "selected but unfocused" — it also uses a filled `primary` bg, which collides with the focused affordance. For composite affordances (checkbox glyph + label), apply the unfocused-selection style to the *glyph only*, not the full row. See [`docs/theming.md`](docs/theming.md) §"Focus vs. persistent selection" for the rationale and the monochrome fallback.

### Images, diagrams, and export

- **Decode happens off the UI thread.** `image::loader` runs in a worker; results arrive as `AppEvent::ImageReady` in the main loop. URL fetches use `ureq` (rustls, no system OpenSSL). Failures are memoised so a reparse won't re-issue a doomed request every frame.
- **ratatui-image encode uses a second worker.** `ResizeRequest` / `ResizeResponse` are routed through `AppEvent::ProtocolReady` and a pending-request FIFO. Failures still pop the queue so a placeholder stays visible until a later frame re-enqueues. `ThreadProtocol::resize_encode` *moves* the inner `StatefulProtocol` to the worker, so `render()` silently draws nothing until the response lands — and `ProtocolPair::native_ready` latches on the first successful encode and is never cleared. `paint_native` must therefore gate the native render on `native.protocol_type().is_some()` (inner present) as well as `native_ready`, falling back to the scratch otherwise; gating on `native_ready` alone leaves a hole where `clear_visible_reserved_rect` just blanked the rect.
- **A native transmission that is still on screen must not be re-sent.** iTerm2 and Sixel "deliver the full raw png image on every render" — `Iterm2::render` puts the entire base64 payload in one cell's `symbol` — and `Buffer::diff` sets `invalidated = max(symbol.width(), invalidated) - 1` per cell, where that symbol's *display width* is the length of the payload (100 000+ columns). `invalidated` therefore stays positive for the whole rest of the buffer, so **every** later cell is re-emitted whether it changed or not, including a second image's payload cell. One full-resolution image at the top of a document consequently retransmits every image below it on every frame; at the cursor-blink cadence (`cursor_blink_ms`, 530 ms) that reads as a ~2 Hz flicker on the lower image, and the ECH sweep at the head of each escape is what makes it visible. Kitty is immune — it transmits once and paints cheap unicode-placeholder rows after — which is why the symptom is iTerm2-only. `paint_native` therefore records each transmission as an `image::cache::NativePaint { rect, generation, frame }` on the `ProtocolPair` and, when the *immediately preceding* frame left that same encoding at that same rect, marks the whole rect `skip` instead of re-rendering, so ratatui emits nothing for the region and the terminal keeps what it has. Two things keep the record honest: the one-frame adjacency rule (any scratch / suppressed / off-screen frame invalidates it automatically — the scratch paths clear it explicitly, while the suppressed / off-screen paths never reach `paint_native` and rely on the frame gap alone, which is what `a_suppressed_frame_forces_the_next_native_frame_to_retransmit` pins), and `ImageCache::invalidate_native_paints()` on the two events that repaint the screen behind our back — `App::on_resize` and the `terminal.clear()` after the external editor returns. `ProtocolPair::native_generation` rides along as defense in depth only: the branch of `apply_resize_response` that bumps it already clears the record, so the comparison never actually sees a generation mismatch. **The skip marking makes the frame buffer lie** — it records blanks over rows the terminal is still showing an image in — and what keeps that from stranding a ghost image is that ratatui's hand-written `impl PartialEq for Cell` compares the `skip` flag alongside symbol and style: the first frame that stops skipping those cells diffs unequal and emits the blanks, even though both sides are blank. Without that, an image scrolling away into empty space below the end of the document would never be erased. It is an upstream detail this design rests on, pinned by `skipped_rect_still_diffs_against_the_same_cells_unskipped`. The frame counter is bumped in `App::draw_frame`, **not** in `paint_images`: Raw and Diff modes draw frames without painting images, and counting only painted frames would make a pre-Raw transmission look adjacent to the first frame back in Rendered. A new code path that repaints the terminal outside the paint pass must call `invalidate_native_paints`.
- **`Capabilities::halfblocks_picker` must be forced to `ProtocolType::Halfblocks`.** ratatui-image gives you a choice of two wrong constructors: `Picker::halfblocks()` hardcodes `font_size` to (10, 20) — an image would change aspect ratio every time it crossed the native↔halfblocks boundary — and `Picker::from_fontsize()` keeps the probed size but *infers a protocol from the environment*, returning an **iTerm2** picker whenever `$TERM_PROGRAM`/`$LC_TERMINAL` names iTerm2, WezTerm, VS Code, Warp, Hyper, Tabby, rio, mintty or Bobcat (or tmux under one of those). `terminal::capabilities::halfblocks_from` takes the font size from the first and stamps the protocol from the second; `image::render_halfblocks_scratch` re-forces it locally. Getting this wrong is catastrophic and silent: the scratch then holds one cell carrying a whole base64-PNG escape surrounded by `skip` cells, which `paint_halfblocks_partial` cannot clip by row — the image flashes at full fidelity only on the frames where row 0 is copied, vanishes otherwise, re-transmits the entire PNG on every scroll frame (the stutter), and punches straight through the modal dim sweep.
- **iTerm2 answers the Kitty capability query but can't do unicode placeholders.** ratatui-image's Kitty backend renders *exclusively* through the protocol's unicode-placeholder extension (transmit once with `U=1`, then paint U+10EEEE cells carrying the image id in diacritics), and iTerm2 3.5+ implements the graphics protocol without that part — so the probe lands on `Kitty`, the image is transmitted, never placed, and the reserved rows stay blank. `detect_image_protocol` overrides `Kitty` → `Iterm2` when `iterm2_hint_is_trustworthy()`, matching ratatui-image's own compatibility matrix. The decision is factored into the pure `resolve_protocol(probed, iterm2)` so it is testable without live stdio probing — the probe stays untestable, the *policy* does not. Only `Kitty` is overridden; a probe landing on Sixel or Halfblocks is left alone. **The override is suppressed inside tmux**, because the env hint it rests on is exactly the thing tmux makes stale: `TERM_PROGRAM` and `LC_TERMINAL` are not in tmux's default `update-environment`, so a session created from iTerm2 and reattached from Ghostty or kitty still advertises iTerm2 while the live terminal speaks Kitty and *not* iTerm2 — pinning `Iterm2` there would break images on a terminal that had them working. The stdio capability probe asks the live terminal through tmux passthrough and has no such staleness, which is why ratatui-image treats it as authoritative over env hints; inside tmux we do the same. The case this gives up is tmux running *inside* iTerm2, which falls back to blank image rows — the pre-override status quo, and recoverable via halfblocks in a way that a wrong-protocol pin on Ghostty is not.
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
| `fancy-regex` | Regex engine for vim `:s`/`:%s` substitution (backreferences + lookaround; the `/` search path stays literal-substring) |
| `insta` | Snapshot testing |
| `proptest` | Property-based testing |
