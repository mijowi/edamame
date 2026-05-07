# edamame refactor plan

A phased plan to reduce duplication, eliminate dead code, improve Rust idiom usage, and shrink the largest files — **without changing any features or capabilities**. Each step is sized to be reviewed and committed as a single PR. After every step the project must build with `cargo build`, lint clean with `cargo clippy -- -D warnings`, and pass `cargo test`.

The numbered steps are ordered so that low-risk mechanical changes land first and structural splits land last. Steps within the same phase are independent and could be reordered or parallelised.

## Current state

After Phase A (`f33b5eb` baseline + Phase A bundle): file sizes are
substantially unchanged (Phase A removed only ~130 LOC of dead code,
unused imports, and small helper extractions). The split candidates
below are the targets for Phase C.

```
src/editor/mouse_ops.rs           1788   ← biggest
src/editor/edit_ops.rs            1428
src/ui/rendered_view.rs           1150
src/markdown/renderer.rs          1089
src/markdown/parser.rs            1065
src/editor/state.rs               1003
src/config/theme_file.rs           949
src/editor/table_edit.rs           918
src/ui/settings_overlay.rs         910
src/ui/table_view.rs               902
src/ui/command_palette.rs          849
src/document/parsed_doc.rs         731
src/config/config.rs               707
src/editor/list_edit.rs            707
src/ui/keybinds_overlay.rs         647
src/ui/bottom_region.rs            640
src/config/keymap.rs               615
src/ui/save_copy_modal.rs          574
src/document/history.rs            547
src/markdown/table_layout.rs       534
src/ui/line_render.rs              533
src/ui/scroll_container.rs         520
```

Total `src/`: ~30 kLOC across ~84 files (`src/input/dispatcher.rs` was
deleted as dead code in Phase A).

---

## Phase A — Quick wins and dead-code cleanup ✅ DONE

Status: **shipped as one bundled commit-equivalent**. Build, clippy
(`-D warnings`), and `cargo fmt -- --check` all clean. 1707 of 1721
baseline tests pass — 14 missing tests (= 7 unique × lib + bin) are the
unit tests for `parse_offsets::covering_ranges`, deleted alongside the
function per the CLAUDE.md note that forbids reintroducing it.

The five sub-steps below (A0–A5) ran together; the description records
what actually landed and any deviations from the original plan.

### A0. Clean clippy + dead-code baseline ✅

Resolved every `cargo build` and `cargo clippy` warning. Final tally
turned out larger than first scoped: 48 build warnings + 86 clippy
warnings → 0 / 0.

What landed:

- **18 unused-import warnings**: cleared. Most were `pub use` re-exports
  no live caller used (`HintSet`, `PaletteEntry`, `ImageHit`, `PreviewView`,
  `RawView`, `StatusBar`, `TableHit`, `TableLayoutSnapshot`,
  `SaveCopyField`, the entire `scroll_container` re-export block,
  `link::LinkTarget`, `CursorBlink`, `attach_trailing_tui_columns_comments`,
  `dispatcher::InputDispatcher`, `ImageLoadError`, `ProtocolPair`,
  `render_mermaid_svg`). The two facade-collision false positives
  (`config::CustomExportEntry`, `diagram::render_mermaid_svg`) get a
  per-statement `#[allow(unused_imports)]` with a comment noting the
  cause is the `pub mod foo; pub use foo::*;` shadowing.
- **26 dead-code warnings** (more than the planned ~14): handled
  individually. See A1 for the breakdown.
- **6 + 2 + 2 doc-comment lints**: fixed by reflowing the offending
  comments. The trickiest was `src/markdown/parser.rs` doc that quoted
  `1. a / 2. b` text — clippy parses that as an ordered-list item;
  rewritten as prose.
- **3 derivable `Default` impls**: `StatusBarLayout`, `RemoteImagePolicy`,
  `ImagesEnabled` switched to `#[derive(Default)] + #[default]` on the
  variant.
- **5 `too_many_arguments` warnings** (not 3 — there were `8/7`, `9/7`,
  and `10/7` cases). **Deviation**: instead of bundling args into structs
  (which would change call sites broadly), each gets a per-site
  `#[allow(clippy::too_many_arguments)]` with a comment noting the real
  fix lands in Phase C. Sites: `parsed_doc::build_with_overrides`,
  `insert_table_modal::render_field_row`, `rendered_view::paint_selection_overlay`,
  `rendered_view::paint_cols_on_line`, `save_copy_modal::render_path_row`.
- **3 `nonminimal_bool` warnings**: 2 of 3 suppressed with
  `#[allow(clippy::nonminimal_bool)]` because the suggested De Morgan
  collapse hides intent (`renderer.rs::not-empty-not-whitespace` and
  `rendered_view.rs::triple-reveal-flag`). The third was already
  collapsable.
- **2 `if has identical blocks`**: collapsed `MoveLeft` / `MoveRight` in
  `editor/edit_ops.rs` to a single `if Raw || (!table && !list)` shape;
  the side-effecting `table_move_horizontal` / `list_move_horizontal`
  calls run via `||` short-circuit, identical to the original behaviour.
- **3 enum-variant-name lints**: handled with one
  `#[allow(clippy::enum_variant_names)]` on `markdown::ast::Block` —
  `CodeBlock` and `BlockQuote` are intentional Markdown terminology.
- **1 `Vec of Range one element`**: kept the `vec![0..total_bytes]` (the
  return type is `Vec<Range<usize>>`, single-element vec is correct);
  added `#[allow(clippy::single_range_in_vec_init)]`.
- **1 `very complex type`**: introduced `type ParsedTable` for
  `(Vec<Vec<Inline>>, Vec<Vec<Vec<Inline>>>, usize)` in `parser.rs`.
- **1 `Iterator::last on DoubleEndedIterator`**: switched to `next_back`
  in `rendered_view.rs::raw_line_at_visual_row`.
- **1 `module has same name`**: `pub mod config;` inside `src/config.rs`
  gets `#[allow(clippy::module_inception)]` with a CLAUDE.md reference
  to the project's facade pattern.
- **1 `from_str` confused with `FromStr`**: `Buffer::from_str` is a test
  helper named for symmetry with `Rope::from_str`; added
  `#[allow(clippy::should_implement_trait)]`.
- **3 `needless_range_loop` / `explicit_counter_loop`**: rewrote
  `truncate_to_width` as `text.chars().take(width).collect()`. The two
  others (`mouse_ops::rendered_text_for_visual_selection` and
  `renderer.rs::table-row-paint`) suppressed because the body indexes
  multiple parallel slices.

### A1. Remove or document `#[allow(dead_code)]` items ✅

Folded into A0. Specifically:

- `src/editor/table_edit.rs::TableCell::trimmed()` — **deleted**.
- `src/editor/state.rs::scroll_for_last_visible()` — **deleted**.
- `src/markdown/table_layout.rs` module-level — **kept**; the comment
  about Phase 6 still applies and the file is a Phase 6 dependency.
- `src/app.rs::HintPrompt` — **kept**; not yet wired up. Re-audit when
  Phase 11 is reviewed.

**Other dead code removed beyond the original plan:**

- `src/input/dispatcher.rs` whole file — `InputDispatcher` had zero
  users; the `App` calls `DefaultHandler::handle` directly through a
  private `HandleEvent` extension trait.
- `ModalHandler::name()` — never called; deleted along with its
  `DefaultHandler` impl.
- `Cursor::clamp` — restored after deletion; tests in this module call
  it. Kept as `#[allow(dead_code)]`.
- `EditorState::set_cursor` — deleted (no users anywhere).
- `EditorState::contents`, `Buffer::insert_char`, `Buffer::remove_char`,
  `Cursor::clamp`, `ParsedDoc::visual_rows_between`,
  `EditorState::visual_rows_between` — kept with `#[allow(dead_code)]`
  because in-module `#[cfg(test)]` blocks use them.
- `Buffer::from_str`, `Buffer::save_as`, `EditorState::new`,
  `EditorState::new_with_config`, `EditorState::has_pending_column_widths`,
  `KeybindsState::focus_action`, `ImageCache::set_decoded`,
  `ImageCache::status`, `SourceMap::rendered_line_count`,
  `SourceMap::block_count`, `SourceMap::total_bytes`,
  `DropIndicator::ColumnBorder`, `DecodeStatus::Failed(String)` payload —
  used by integration tests in `tests/`. Kept with `#[allow(dead_code)]`
  and a comment naming the consumer.
- `ImageHit` enum, `ImageLayoutSnapshot::source_range`, `::hit_test`,
  `::alt`, `LinkLayoutSnapshot::hit_test`, `ScrollContainerState::new`,
  `top_anchored_rect_for_content`, `LinkTarget::is_markdown_file`,
  `Renderer::with_code_wrap`, `Renderer::render`,
  `PaletteState::match_count`, `PaletteState::focused_action`,
  `KeybindsState::focused_action`, `ModalStack::len`,
  `ModalStack::contains`, `History::can_undo`, `History::can_redo`,
  `Selection::new`, `line_render::render_line`,
  `line_render::render_line_with_cursor`, `ImageCache::invalidate_protocols`,
  `ImageCache::aspect_rows` — used only by same-module
  `#[cfg(test)] mod tests`. Kept with `#[allow(dead_code)]`.
- `PaletteEntry::new` — deleted (no users in src or tests).
- `parse_offsets::covering_ranges` and its 7 unit tests — deleted.
  CLAUDE.md explicitly forbids reintroducing this function for cursor
  mapping; keeping its test scaffolding only invites accidental
  resurrection.

### A2. `History`: extract `apply_delta` helper ✅

Done as planned. `undo()` calls `apply_delta(buf, offset, &delta.inserted,
&delta.removed)`, `redo()` calls `apply_delta(buf, offset, &delta.removed,
&delta.inserted)`. The argument-order asymmetry visibly reflects the
undo/redo inversion. ~16 LOC saved. All 21 history tests pass.

### A3. Remove now-unnecessary `let _ = rest;` ✅

Done: `src/editor/link.rs::has_url_scheme` switched from
`let Some((scheme, rest)) = ...; ...; let _ = rest;` to
`let Some((scheme, _rest)) = ...;`. No other idle `let _` discards
remain.

### A4. Extract keymap modifier helper ✅

Done. `src/config/keymap.rs` now has three pieces:

- `parse_modifier(part) -> Option<KeyModifiers>` — replaces the inline
  `"ctrl" | "alt" | "shift"` match.
- `parse_key_code(key_part) -> Option<KeyCode>` — pulls the 30-arm
  key-name match out of `parse_key`. Also fixes the awkward
  `c.chars().count() == 1 ... .unwrap()` to a `chars().next() / chars()
  .next().is_some()` guard.
- `parse_key` is now ~20 lines and reads as "for each part, either a
  modifier or the final key".

All `parse_key` tests pass.

### A5. Replace hand-written `Default` impls with `#[derive]` ✅

Three impls switched: `StatusBarLayout`, `RemoteImagePolicy`,
`ImagesEnabled`. Each gets `#[derive(Default)]` on the enum and
`#[default]` on the previously-default variant. Wider audit found no
other hand-rolled `Default` that matched the derived form.

### Phase A acceptance summary

- `cargo build`: zero warnings.
- `cargo clippy -- -D warnings`: clean.
- `cargo fmt -- --check`: clean.
- `cargo test`: 1707 pass, 0 fail (was 1721 — the 14-test loss is the
  intentionally-deleted `covering_ranges` tests counted across lib +
  bin compilation).
- 42 files modified, 1 file deleted (`src/input/dispatcher.rs`), 1 file
  added (`REFACTOR_PLAN.md`). Net `src/` LOC change: roughly −130.
- Bar tightens for the rest of the plan: zero `cargo build` warnings,
  zero `cargo clippy` warnings is now a hard gate.

---

## Phase B — Shared helpers (cross-file deduplication) ✅ DONE

Status: **all seven sub-steps shipped**. Build, clippy (`-D warnings`),
fmt, and the full test suite stay clean (715 lib + 746 bin + 10
doc-test-related + integration suites all pass). Four small modules
landed under `src/ui/`; `config/config.rs` and `config/keymap.rs` lost
substantial repetition.

### B1. `src/ui/modal_row.rs` — shared focused-row formatter ✅

Added `format_modal_row(label, value, focused, editing, theme, layout)`
returning a styled `Line`.  `RowLayout::FixedPad(usize)` covers the
settings/keybinds shape (label padded to a fixed width, value follows);
`RowLayout::RightAlign(u16)` covers the palette shape (value pushed to
the right edge with at least one space of slack).  The `editing` flag
overrides the value style to `theme.modal_input_focused` so settings
and keybinds share their inline-edit affordance.

**Deviation from plan:** the three call sites weren't byte-for-byte
identical (palette is right-aligned to the area width; settings/keybinds
use fixed padding + an editing state), so the helper takes a `RowLayout`
enum and an `editing: bool` rather than just `(label, value, focused,
theme, width)`.  This keeps the three sites converging on the same
styling rules while preserving their distinct geometry.

### B2. `src/ui/button_row.rs` — shared button-row renderer ✅

`render_button_row(area, buf, labels: &[&str], focused_idx, theme)`
plus a `button_row_width(labels)` companion.  Three former copies are
now thin wrappers:

- `modal::ModalView::render` builds a `Vec<&str>` from
  `self.buttons` and calls `render_button_row` directly.
- `insert_table_modal::render_buttons` maps `InsertTableField` to a
  button index (`Insert → 0`, `Cancel → 1`, field focus → `usize::MAX`
  meaning "no button focused") and forwards.
- `save_copy_modal::render_buttons` does the same for `SaveCopyField`.

Local `button_row_width()` shims in the latter two files were deleted;
their callers pass the file-local `BUTTON_LABELS` constant directly.

### B3. `src/ui/overlay_nav.rs` — shared focus-skip helper ✅

`next_focusable(rows, current, delta, is_focusable) -> Option<usize>`
walks rows from `current` in direction `delta`, skipping rows for
which the predicate returns false.  Returns `None` when no focusable
row exists in that direction (non-wrapping by design — wrap makes
overlay nav feel jumpy).  Replaces the open-coded `while
(0..len).contains(&idx)` loops in `settings_overlay::move_focus` and
`keybinds_overlay::move_focus`.

### B4. `src/ui/content_width.rs` — shared content-width helper ✅

Two tiny helpers:

- `max_row_width(rows, |r| width)` — `rows.iter().map(...).max()
  .unwrap_or(0) as u16`, used by every overlay's
  `*_content_width` function.
- `optional_text_width(Some(s), prefix_len)` — `prefix_len +
  s.chars().count()` or 0 when `None`, replacing the `last_error`
  width branches.

Net LOC change is small but the three call sites read a lot more
clearly.

### B5. Generic `cycle_enum` for settings overlay ✅

`cycle_enum<T: PartialEq + Copy>(current: T, order: &[T], delta: i32)
-> T` plus two `const &[T]` order tables (`IMAGES_ENABLED_ORDER`,
`REMOTE_POLICY_ORDER`).  `cycle_images_enabled` /
`cycle_remote_policy` deleted.  No `parse_enum` companion was needed —
the existing `parse_images_enabled` / `parse_remote_policy` are each a
short inline match and didn't share enough structure to factor out
without adding indirection.

### B6. Generic config reader to DRY the three loaders ✅

Added `read_and_warn<T: DeserializeOwned, M, F>(path, warnings,
on_missing, on_parse_failure) -> T`.  The two fallback closures are
separate so `read_theme_named` can attach a `tracing::warn!` to the
missing-file path only (named themes that can't be found should log;
falling back from a parse error shouldn't).  The three loaders shrink
to:

- `read_main_config` — one line: `read_and_warn(path, warnings,
  Config::default, Config::default)`.
- `read_keybindings` — one `read_and_warn` call followed by the
  Action/parse_key validation pass.
- `read_theme_named` — one `read_and_warn` call with a custom
  `on_missing` that logs a `tracing::warn!` for non-default theme
  names before returning the compiled-default-derived `ThemeFile`.

`config.rs` drops from 1049 → 1014 LOC.

### B7. `Action` ↔ string mapping driven by a single table ✅

Introduced an `action_variants! { ... }` macro that takes a list of
unit-variant idents and emits both `impl fmt::Display for Action`
and `impl FromStr for Action`.  `Action::InsertChar(_)` is special-
cased inside the macro for `Display` only (FromStr can't reconstruct
the payload).  The `bind!` macro mentioned in the plan turned out
*not* to enumerate variants — it's a 5-line `parse_key` shorthand —
so it's unchanged.

`keymap.rs` drops from 873 → 770 LOC; the two impls go from 158
hand-listed match arms to ~28 lines total (macro + variant list).

---

## Phase C — File splits

Each step splits one large file into several smaller modules behind the existing facade. Public API is preserved via `pub use` re-exports — call-site imports stay unchanged.

### C1. Split `src/editor/mouse_ops.rs` (1787)

Convert into a `src/editor/mouse_ops/` directory with the existing `src/editor/mouse_ops.rs` becoming the facade:

| New file | LOC | Contains |
|---|---|---|
| `mouse_ops/dispatch.rs` | ~250 | `apply()` top-level dispatcher and small action arms |
| `mouse_ops/table.rs` | ~900 | `DragTarget`, table hit-tests, drag commit, width calculations, row/col delete |
| `mouse_ops/selection.rs` | ~450 | `select_word_at_cursor`, `select_line_at_cursor`, `expand_selection_to_inline_markers`, `preview_*` |
| `mouse_ops/coord.rs` | ~400 | `raw_click_to_offset`, `rendered_click_to_offset`, `rendered_sub_line_to_offset`, `rendered_to_raw_char_map`, `line_row_width` |

The facade re-exports the existing public types and free functions so the rest of the crate sees no change.

**Risk:** Medium. Mouse logic has dense interdependencies; mouse integration tests under `tests/mouse.rs` are the safety net.

### C2. Extract table-aware editing from `src/editor/edit_ops.rs` (1430)

Move `table_move_*`, `adjacent_cell`, `table_next_*`, `table_prev_*`, `jump_to_cell`, `table_insert_*`, `table_delete_*` (lines 1008–1603) into `src/editor/table_edit_ops.rs`. Also extract two tiny helpers:

- `skip_alignment_row(row) -> usize` — replaces 4 hand-inlined `if row == 1 { 2 } else { row }` blocks.
- `cursor_cell_or_return!` macro / `with_cursor_cell` helper — replaces the repeated `let Some(...)` triple-let block (lines 1338–1424).

After this step, `edit_ops.rs` should drop to ~900 LOC.

**Risk:** Low–medium. Table integration tests (`tests/table.rs`) cover the moved code.

### C3. Carve up `src/editor/state.rs` (1020)

Split impl blocks into focused files; the struct definition stays in `state.rs`:

| New file | Methods moved |
|---|---|
| `editor/state_viewport.rs` | `scroll_*`, `clamp_cursor_to_viewport_top`, `ensure_cursor_visible`, `total_visual_rows_for_mode`, `visual_rows_between`, `rendered_line_at_visual_row`, `raw_line_at_visual_row`, `visual_rows_before_raw_line` |
| `editor/state_cursor_visual.rs` | `move_up_visual`, `move_down_visual`, `current_visual_col`, `cursor_visual_row` and friends |
| `editor/state_cursor_block.rs` | `update_cursor_block`, `cursor_block_revealed`, `cursor_visible` |

Each file is a `impl EditorState { ... }` block; no facade work needed because `EditorState` itself doesn't move.

Also collapse the three-step `EditorState::new` → `new_with_config` → `new_with_image_config` constructor chain into a single `EditorState::new` taking `EditorConfigBundle` (or similar). This is small and worth doing in the same PR.

**Risk:** Low — pure mechanical move. Heavy test coverage in `tests/editing.rs`.

### C4. Split `src/ui/rendered_view.rs` (1147)

Move table-related rendering out:

| New file | Contents |
|---|---|
| `ui/rendered_view/snapshot.rs` | `TableLayoutSnapshot` and `build_snapshots` (lines 61–544). Extract `build_snapshots`'s inner block-closing and row-accumulation closures into named functions while moving — drops nesting from 5 to 2–3. |
| `ui/rendered_view/classify.rs` | `TableSubLineKind` and classification (lines 545–674) |
| `ui/rendered_view/paint.rs` | `paint_handles`, `paint_drop_indicator`. Consolidate `paint_horizontal_drop` + `paint_vertical_drop` into one parameterised helper. |

`rendered_view.rs` becomes a thin coordinator (~300 LOC).

**Risk:** Medium. Snapshot tests under `tests/snapshots/` and table mouse tests are the safety net.

### C5. Split `src/markdown/renderer.rs` (1094)

| New file | Contents |
|---|---|
| `markdown/renderer/util.rs` | `wrap_styled_chars`, `hard_split_styled`, `extend_with_styled_chars`, `longest_word_chars`, `truncate_to_width`, `link_*` helpers (lines 30–236) |
| `markdown/renderer/table.rs` | `render_table`, `blank_table_separator`, `render_table_row`, `cell_styled_chars` (lines 769–1008). Extract `collect_cell_metrics(rows, col_count)` to dedupe lines 787–811. |
| `markdown/renderer/list.rs` | `render_list`. Split into `compute_list_metrics` + `render_list_item` (current `render_list` is 141 LOC). |

Block dispatch and inline rendering stay in the main `renderer.rs`, which drops to ~400 LOC.

**Risk:** Medium. Snapshot tests under `tests/renderer.rs` cover output.

### C6. Extract `src/markdown/parser/post_pass.rs` from `parser.rs` (1066)

Move `promote_image_paragraphs`, `promote_diagram_code_blocks`, `promote_html_comments`, `attach_trailing_tui_columns_comments`, `split_lists_on_blank_lines` (and their helpers) into a single `post_pass.rs`. Reduces `parser.rs` by ~280 LOC and isolates the normalisation pipeline.

**Risk:** Low — these functions are independent transforms.

### C7. Split `src/config/theme_file.rs` (949)

| New file | Contents |
|---|---|
| `config/theme_file/color.rs` | `ColorField` and its `From` impls (lines 40–71) |
| `config/theme_file/palette.rs` | `PaletteFile` + `resolve()` + `From<&Palette>` (lines 73–218). Replace the parallel field-mapping by introducing a small declarative macro that lists every palette field once and generates both directions. |
| `config/theme_file/style_spec.rs` | `StyleSpec` and modifier conversion (lines 220–314). Replace the 6 `if` blocks with a `[(bool, Modifier)]` table. |
| `config/theme_file/defaults.rs` | `default_theme_toml()` (lines 640–694). |

`theme_file.rs` keeps the `ThemeFile` outer type and re-exports the rest. Tests stay in their current files; the four `apply!` / `check!` macro invocations get a shared field-list `macro_rules!` so the 294-call repetition collapses.

**Risk:** Medium. Test coverage in this file is heavy and will catch any field-mapping regression.

### C8. Split `src/config/config.rs` (719)

| New file | Contents |
|---|---|
| `config/readers.rs` | `read_main_config`, `read_keybindings`, `read_theme_named`, `deserialize_with_unknown_keys`, the generic `read_and_warn` from B6 |
| `config/sections.rs` | `EditorConfig`, `ModalConfig`, `TableConfig`, `ImagesConfig`, `ExportConfig`, `DevConfig` (lines 428–701) |
| `config/init.rs` | `ensure_default_files_in`, `write_if_absent` |
| `config/warnings.rs` | `ConfigWarning`, `WarningKind` |

`config.rs` keeps `Config`, `LoadedConfig`, and the `load()` orchestration (~150 LOC). Tests get a `setup_config_dir()` helper to dedupe `tempfile::tempdir().unwrap() + create_dir_all` boilerplate.

**Risk:** Medium — startup path. Existing config tests are extensive.

### C9. Split `src/editor/list_edit.rs` (707)

`list_edit.rs` mostly contains parse helpers (`parse_line_start`, `matches_list_line`, `parse_items`) plus continuation/indent/checkbox logic. Split:

| New file | Contents |
|---|---|
| `editor/list_edit/parse.rs` | `parse_line_start`, `matches_list_line`, `parse_items` (split `parse_items` into `parse_single_item` + iterator) |
| `editor/list_edit/edit.rs` | continuation, indent/outdent, checkbox toggle, renumber |

**Risk:** Low — `tests/list_edit.rs` has good coverage.

### C10. Extract `src/document/visual_cache.rs` from `parsed_doc.rs` (728)

Move `VisualRowCache` and its `ensure_visual_rows`/`visual_rows_*` methods into their own file. `parsed_doc.rs` drops to ~600 LOC.

**Risk:** Low.

### C11. Split `src/ui/settings_overlay.rs` (910)

Move `RowDef`, `RowKind`, `RowAction`, and `build_rows()` into `src/ui/settings_overlay/rows.rs` (~250 LOC). Also fold in the per-enum cycle/parse helpers from B5. The widget struct + render path stays in the main file.

**Risk:** Low.

### C12. Split `src/ui/command_palette.rs` (856) and `src/ui/keybinds_overlay.rs` (645)

For each:
- Move the `ALL_ACTIONS` / `CATEGORIES` / `build_entries` data to a sibling `actions.rs` / `categories.rs` file.
- Pre-compute focus offsets at construction (currently recomputed every render in `keybinds_overlay.rs`).

**Risk:** Low.

---

## Phase D — Internal cleanups inside the now-smaller files

Smaller, in-file improvements that benefit from being applied after the splits land — they would be hard to review while files are massive.

### D1. Decompose long functions

Targets, all >80 LOC after the splits land:
- `mouse_ops::rendered_sub_line_to_offset` — break into named helpers per case.
- `mouse_ops::rendered_to_raw_char_map` — extract a small state struct for the pulldown-cmark walk.
- `parser::parse_blocks` — likely still long after C6; review and extract per-block-kind parsers.
- `renderer::wrap_styled_chars` — currently 80 LOC; only split if a clearer factoring emerges.

### D2. Replace remaining manual loops with iterator combinators

Spot list — apply only where it improves clarity, not as a blanket policy:
- `mouse_ops::preview_pos` — `lines.iter().enumerate().skip(scroll).find_map(...)`.
- `editor/list_edit/parse::parse_line_start` — `chars().take_while(|c| c.is_whitespace())`.
- `theme_file::default_theme_toml` — `lines().scan(...)` instead of an `in_palette` flag.
- `keymap::format_key` / `format_key_compact` — share their key-code match arms via a single helper that returns `&'static str`.

### D3. Replace magic numbers with named constants

- Header rows offset (`2 + i`) in `rendered_view.rs` → `const HEADER_ROWS: usize = 2`.
- Label padding widths (`{:<28}`, `{:<22}`) in overlay files → `const LABEL_PAD_*`.
- `MAX_LIST_ROWS = 20` in `command_palette.rs` is already named — keep.

### D4. Tidy `parse_line_start` and friends

Replace byte iteration with `str::chars().take_while`. Use char literals instead of byte literals where it improves readability.

---

## Suggested PR sequence

A reasonable order, optimised so each PR is small and independent:

1. ~~**Phase A bundle** (PR): A0+A1+A2+A3+A4+A5~~ — **shipped as one
   commit** covering all clippy/build cleanup plus the small extractions.
2. **Phase B as separate PRs**, in order: B1, B2, B3, B4, B5, B6, B7.
   Each is small and reviewable.
3. **Phase C as separate PRs**, ideally in this order to minimise rebase pain:
   - C2 (table edit ops) — biggest LOC win for the smallest blast radius.
   - C3 (state.rs split).
   - C9 (list_edit split).
   - C10 (visual cache extract).
   - C6 (parser post_pass).
   - C5 (renderer split).
   - C8 (config.rs split).
   - C7 (theme_file split).
   - C11 (settings_overlay split).
   - C12 (command_palette + keybinds split).
   - C4 (rendered_view split) — leave for last; densest interdependencies.
   - C1 (mouse_ops split) — biggest, leave for last.
4. **Phase D** as 2–3 follow-up PRs, scoped per file.

Total remaining: roughly 22–25 PRs. If reviewer bandwidth is the
constraint, B can be combined into 2–3 larger PRs without much risk.

## Baseline

Pre-refactor (commit `f33b5eb`):
- 1721 tests passing.
- 48 `cargo build` warnings (mostly dead code + unused imports).
- 86 `cargo clippy` warnings.

After Phase A:
- 1707 tests passing (the 14-test loss = 7 unique tests for the deleted
  `parse_offsets::covering_ranges`, counted across lib + bin
  compilation artifacts).
- 0 `cargo build` warnings.
- 0 `cargo clippy` warnings (passes `-D warnings`).
- `cargo fmt -- --check` clean.

Phase A folded in the clippy cleanup so the rest of the plan can be
executed against a clean baseline. CI does not yet enforce
`-D warnings`; if the gate is to be tightened, this is the moment.

## Acceptance criteria for every step

- `cargo build` and `cargo build --release` succeed with **zero warnings**.
- `cargo clippy -- -D warnings` is clean.
- `cargo fmt -- --check` is clean.
- `cargo test` passes; updated `insta` snapshots reviewed and committed in the same PR if any change.
- Public API at the facade layer is unchanged: no call site outside the touched module needs to update its `use` statements.
- No new features, no removed features. Behaviour is byte-for-byte identical for the user.

## Out of scope

- Any feature work, including changes to keybindings, theming, or rendered output.
- Replacing `pulldown-cmark`, `ropey`, `ratatui`, or `crossterm`.
- Performance optimisation (acceptable when it falls out of refactor; not a goal).
- Adding new tests except where needed to prove a refactor preserves behaviour.
