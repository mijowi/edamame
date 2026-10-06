# Row provenance — the renderer records where each row came from

Status: **DESIGN (2026-10-05).** Targeted at the next release, which ships every phase together, nested reflow included. Supersedes the discarded `list-row-mapping` patch (see [Phase 0](#phase-0--discard-the-patch-keep-its-tests)) and absorbs [`nested-reflow.md`](nested-reflow.md) as this plan's Phase 7. Sibling context: [`editing-model.md`](../editing-model.md), [`input.md`](../input.md), [`blockquotes.md`](../blockquotes.md), [`tables.md`](../tables.md).

## Problem

Every Rendered-mode interaction (the cursor row, the raw reveal, the gutter, clicks, and the selection / search / `:s` / yank overlays) needs to know, for each rendered row, **which source line it shows and how its columns map to that line's characters**. Today nothing records that. It is *inferred*, separately, by each consumer:

- **The AST throws pulldown-cmark's offsets away.** `parse_raw_with_ranges` maps `(Event, Range)` to `Event` before `parse_blocks` sees it; only top-level ranges survive (`RangeTracker`, depth 0). No nested `Block`, `ListItem`, or source line carries a position.
- **So positions are re-derived, by re-parsing or by sniffing.** Re-parsing the same text again: `inline_col_map` (per line), and in the discarded patch `code_layout::CodeLineScan`, `list_layout::list_lines`, and `post_pass::quote_blank_lines`, three more pulldown-cmark passes over block text the first parse already walked. Sniffing raw or rendered text: `raw_list_marker_char_width`, `rendered_list_marker_char_width`, `code_indent_strip_chars`, `is_closing_fence_line`, `is_table_block`, `detect_setext`, `annotate_list_blanks`' marker/fence scan, and `classify_table_sub_lines`, which recognizes table rows by their box-drawing glyphs.
- **The row mapping is a second model of the renderer.** `editor::state::sub_lines_in_block` predicts how many rows each source line renders. Whenever the renderer does something the model doesn't know (a marker on a row of its own, an unclosed fence's closing row, a bare `-`, a loose list inside a quote, a heading or table inside an item), every consumer drifts at once. The docs hold this together with invariants like "four callers must agree and must never re-derive it", and a proptest that checks the model's row count against the renderer's.
- **The rendering gets bent to fit the model.** To keep the 1:1 assumption true, the discarded patch made list paragraphs stop joining soft breaks and changed how quotes emit blank rows. `nested-reflow.md` exists because the same assumption forbids reflow anywhere but a top-level paragraph.

Some of this is essential complexity. A hybrid editor needs a raw↔rendered mapping, and CommonMark makes it genuinely hard (container prefixes, lazy continuation, indent stripping, partial tabs, markers that change width). What isn't essential is computing the mapping after the fact, in five places, from incomplete information.

## Principle

**The component that decides a row's layout records its provenance at the moment it emits the row.** The parser knows where every piece of content starts in the source; the renderer knows what it put in front of that content and which rows are chrome. Together they record, per rendered row, a `RowOrigin`. Every consumer reads it, and none of them re-derives it.

Column mapping *within* inline content stays with `InlineColMap`. It is one of the sound parts: one owner, built from pulldown-cmark offsets over a single line. What changes is that it gets applied to the right slice of the right line, chosen by the origin instead of by inference.

## Design

### 1. Source positions in the AST

`parse_blocks` and its helpers consume `(Event, Range<usize>)` instead of `Event`. The `RangeTracker` keeps observing the same stream, so top-level ranges stay 1:1 with blocks, and there is still exactly **one** pulldown-cmark pass.

Each leaf block records its source lines, **relative to its top-level block**:

```rust
/// Where a leaf block's content sits in the source, relative to its top-level block's first line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SrcLines {
    /// Block-relative index of the leaf's first source line.
    pub first: u32,
    /// Per source line of the leaf, the char column where its content starts: past container
    /// prefixes (`> `, list indent), its own marker, and any stripped code indent.  `None` for a
    /// line that is all chrome (a fence, a setext underline).
    pub content_col: Vec<Option<u32>>,
}
```

- **Every leaf variant carries a `SrcLines`:** `Paragraph`, `Heading`, `CodeBlock`, `Html`, `HtmlComment`, `MetadataBlock`, `Table`, `ImageBlock`, and `HorizontalRule`.
  - For a code block, `content_col` is exactly where pulldown-cmark's `Text` starts on each body line. That is the CommonMark-correct `strip` the discarded `CodeLineScan` computed with an extra parse.
  - For a paragraph, it is where the inline content starts on each line, lazy continuation lines included.
  - For an `HtmlComment`, it is where the `<!--` starts (the row reveals and takes clicks).
  - For a `HorizontalRule`, it is where its first `-`/`*`/`_` starts; the rule's row is `Chrome`, and the column is where a click on it lands.
- `ListItem` and every container child record a block-relative line span, so the renderer can emit loose-list spacing and bare-`>` rows from the gaps between child spans. That replaces `ListItem::blank_lines_before` and `annotate_list_blanks`, and the discarded `BlockQuote::blank_lines` and `annotate_quote_blanks` are never needed.

**Why block-relative:** `RenderCache` keys on `Block` by value. Absolute offsets would make every block below an edit miss the cache. Block-relative positions keep identical blocks hashing identically wherever they sit, which is today's behavior.

**Why `u32` columns:** the document is untrusted (see [`security-invariants.md`](../security-invariants.md)), and a column is bounded by line length, not by terminal width. A lazy continuation line with 70,000 leading spaces is valid CommonMark and puts `content_col` past `u16::MAX`. Every narrowing from `usize` goes through `u32::try_from(..).unwrap_or(u32::MAX)` (no `as` casts); a `u32::MAX` column clamps to the line's end wherever it is used. Phase 1 adds that line as a test.

**Post-pass promotions** (images, mermaid, display math, HTML comments, `tui-columns`) carry the `SrcLines` of the block they replace. They stay top-level only, as today.

### 2. `RowOrigin`, emitted by the renderer

```rust
/// Where one rendered row came from.  1:1 with `ParsedDoc::lines`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowOrigin {
    /// Block-relative source lines this row shows: one line, or several for a reflowed flow.
    /// `None` for a row no source line owns (an unclosed fence's placeholder closing row).
    pub lines: Option<Range<u32>>,
    pub cols: ColOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColOrigin {
    /// No column relation to the source: fence label, closing-fence placeholder, table border,
    /// horizontal rule, a marker-only row, a big-H1 glyph row, an image's reserved rows, a
    /// blank row between blocks.  The whole row washes; a click lands at the row's first line's
    /// `content_col`, or at char 0 of that line where it is `None`.
    Chrome,
    /// Content starting at raw char `raw_col` and rendered cell `rendered_col`; the part before
    /// both is prefix (bar, marker, indent, pad cell).
    Content { raw_col: u32, rendered_col: u32, kind: ContentKind },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    /// Inline Markdown: map through `InlineColMap` over the raw text past `raw_col`.
    Inline,
    /// Characters shown verbatim (code body, frontmatter): identity past the prefix.  Such a
    /// row never de-renders on reveal.
    Verbatim,
    /// Chunk `sub` of table row `row`: map through `table_layout`'s cell geometry.
    TableRow { row: u32, sub: u32 },
    /// A reflowed paragraph's flow: map through one `InlineColMap` built over the flow's text,
    /// i.e. each line in `lines` sliced past its `content_col`, joined by `\n` (soft breaks
    /// render as spaces).  A raw char index into that text converts back to `(line, col)` by
    /// walking the slices.  For a top-level paragraph every `content_col` is 0, so this is
    /// exactly today's block-wide map.
    Flow,
}
```

**Who emits rows.** Rows come from two places, and both write origins:

- **The renderer.** `render_block` writes into a sink holding `lines` and `origins` in lockstep. Container renderers (`render_blockquote`, `render_list`, footnote definitions) wrap their children's origins exactly as they wrap their lines: the quote adds its bar's cells to `rendered_col`. The raw side needs no adjustment, because `content_col` already counts past the `> `.
- **`ParsedDoc::build`.** It appends rows of its own around the rendered blocks, and each gets an origin at the same push:
  - Every `push_blank` row (leading blanks, blanks in the gap after a block, the phantom final line) is a virtual blank block's only row: `RowOrigin { lines: Some(0..1), cols: Chrome }`, relative to that virtual block.
  - The setext H2 rule `build` appends today moves into `Renderer::render_heading`, beside the setext H1 rule it already emits. The renderer knows the heading is setext because its `SrcLines` has two lines with the second `None`. It emits the rule as `Chrome` with `lines: Some(1..2)`. `detect_setext` leaves `build`.
  - The defensive "stray rendered lines" loop pushes their origins from the same iterator. It already cannot fire, so add a `debug_assert!` that it doesn't.
- **The lockstep check** is a `debug_assert_eq!(origins.len(), lines.len())` after each block in the sink, and again at the end of `build`.

**Other rules:**

- **`rendered_col` is measured by the code that pads**, at emit time (cells, not chars; marker right-alignment, the code pad cell, task boxes included), so there's no second measurement to drift.
- **The reveal gate becomes a property of the row.** `Verbatim` rows never de-render; every other row reveals. That replaces `line_allows_raw_reveal`.
- **`RenderCache`** stores `(lines, origins)` per block. Origins are block-relative, so a cached block is position-independent, as today.
- **`ParsedDoc`** stores `row_origins: Vec<RowOrigin>` beside `lines`, and `per_block_own` is its per-block length.
- **Reflowed top-level paragraphs** (`reflow_paragraphs` on) emit one row with `lines: Some(0..n)` and `kind: Flow` from Phase 2 on. They exist today, so they are not Phase 7 work. Phases 3 and 4 route them through `row_map` like any other row. The fallback that skips reflow for a paragraph containing a `HardBreak` (`render_paragraph`) stays as it is. With origins it could become one `Flow` row per hard-break segment, but that changes rendering and is [out of scope](#out-of-scope).

### 3. One mapping module

A new `document::row_map` owns every question the consumers ask, answered from `row_origins`, `real_ranges`, `ParsedDoc::source`, and the per-line `InlineColMap`s.

**Index spaces.** Three spaces are in play, and `row_map` works in exactly one of them:

| Space | Unit | Who maps into it |
|---|---|---|
| **Display row** | one screen row, after wrap and after the reveal patch | the viewport |
| **Logical row** | one entry of `ParsedDoc::lines` / `row_origins`, before wrap | `EffectiveRows::line_at_visual_row` → `RowHit::Rendered { line, sub }` |
| **Rendered cell column** | a cell within a logical row, before wrap | `line_render`'s wrap geometry (`visual_rows_of_chars`, `char_idx_at_cell_col`, `cell_col_at_char_idx`, `sub_line_of_col`, and `click_to_rendered_char_idx` in `coord.rs`) |

`row_map` takes and returns **logical rows** and **rendered cell columns**. It knows nothing about wrapping or about the reveal patch. Those stay where they are:

- **Wrap.** A click on a display row is resolved in two steps. First, `EffectiveRows` turns the display row into `(line, sub)`. Second, the wrap helpers turn `(sub, screen col)` into a rendered cell column. Then `row_map` maps that column to raw. The cursor indicator runs the same steps in reverse: `row_map` gives the cell column, and `sub_line_of_col` picks the wrap sub-row. None of this changes.
- **Stacked raw reveal** (`RowHit::Raw { raw_line, sub }`, a revealed reflowed paragraph). `raw_line` already *is* the block-relative source line, and the row shows that line's raw text, so columns are the raw line's own. `row_map` is not consulted for these rows. It only supplies `row_for_line` for where the block starts.
- **Height-neutral reveal** (the cursor's row of a non-reflowed block, painted as raw source). The row index is a logical row. `line_for_row` names the source line it paints, and columns are identity over that raw line (`reveals(row)` gates whether this happens at all). The raw line's wrap count stays with `revealed_raw_row_count`; only its line lookup changes.
- **Diagram reveal** (mermaid and `$$…$$` rows painted 1:1 with source lines, below a math-preview band). See [Phase 6](#phase-6--images-diagrams-headings-sm).

**The functions.** All operate on rows shown rendered:

- `row_for_line(block, line) -> usize`: the first row whose `lines.start >= L`, clamped to the block's last own row. This is the same prefix-sum rule `sub_lines_in_block` encodes today: a line rendering no row (an interior blank, a setext underline, a bare `-`) shares the next line's row.
- `line_for_row(block, row) -> usize`: the row's `lines.start`, or, for `None`, the nearest owned line above. This is the inverse the discarded `raw_lines_by_sub_row` reconstructed.
- `raw_to_rendered_col(row, raw_col) -> Option<usize>` and `rendered_to_raw_col(row, rendered_col) -> usize`: one `match` on `ColOrigin`. Inside the prefix, both clamp to the content start. `raw_to_rendered_col` returns `None` where the inline map can't place a column, so the overlay skips instead of painting off by N. For a `Flow` row the raw side is a `(line, col)` pair, not a bare column.
- `reveals(row) -> bool`.

Every consumer calls these; none of them branches on block kind to pick a mapping.

## What it replaces

| Today (HEAD) | Becomes |
|---|---|
| `editor::state::sub_lines_in_block`, `cursor_sub_line_in_block`, `cursor_rendered_line_idx`'s derivation | `row_map::row_for_line` |
| `state_source_lines::build_source_line_map`'s inversion rules (last writer wins, `block_own == 0` skip) | a walk over `row_origins` |
| `mouse_ops::coord::rendered_sub_line_to_offset`'s row and column branches, including the reflowed-paragraph "not revealed" arm, and `non_table_click_to_raw_col`'s list and code arms | `row_map::line_for_row` / `rendered_to_raw_col` |
| `revealed_raw_row_count`'s source-line lookup (the function stays; it still owns the raw line's wrap count) | `row_map::line_for_row` |
| `RenderedView`'s cursor-indicator chain (table, code, list-marker, inline, setext arms) and its reveal gate | `row_map::raw_to_rendered_col` / `reveals` |
| `paint_byte_range_overlay`'s row lookup and its kind-by-kind column chain | `row_map` |
| `code_layout` (`code_indent_strip_chars`, `is_code_fence_row`, `is_closing_fence_line`, `line_allows_raw_reveal`; the column helpers fold into `row_map`) | deleted |
| `list_layout`'s marker-width sniffers (`raw_list_marker_char_width`, `rendered_list_marker_char_width`, the marker col maps) | deleted |
| `post_pass::annotate_list_blanks`, its fence/marker scanners, `ListItem::blank_lines_before` | spans from §1 |
| `classify_table_sub_lines`, `is_table_block` on the mapping paths | `ContentKind::TableRow` and the AST kind |
| `detect_setext` in `RenderedView` and in `ParsedDoc::build` (the H2 rule) | `SrcLines::content_col` (the underline is `None`); the renderer emits the rule |
| `is_image_block` row pinning; `latex_source_offset`'s scattered call sites | `Chrome` rows from the renderer; one diagram-reveal helper in `row_map` (Phase 6) |

Unchanged: `SourceMap`'s block-level role (byte → top-level block, extended ranges for cursor lookup, virtual blank blocks), `InlineColMap`, `table_layout`'s cell geometry, `EffectiveRows`' shape and the wrap helpers in `line_render`, the diff view.

## Out of scope

- **The diff view's own layout** (`diff::layout`, `DiffView`). It calls the renderer and so compiles against the new sink, but its row model is untouched.
- **`table_layout`'s cell geometry** and column widths. Phase 5 only changes how a row *finds* its table row and chunk.
- **`SourceMap`'s block lookup** and the virtual-blank-block scheme.
- **Preview mode's mapping.** It never reveals, and it already reads the rendered rows. It benefits from `row_map` incidentally but gets no dedicated work.
- **Reflowing across hard breaks.** The `HardBreak` fallback in `render_paragraph` stays.
- **The wrap engine** (`line_render`) and `EffectiveRows`' reveal patch.
- **Any rendering change** beyond the two in [Rendering decisions](#rendering-decisions) and the setext H2 rule moving (not changing) from `build` to the renderer.

## Phases

Each phase lands on its own with the full suite green. Sizes are relative (S < M < L).

### Phase 0 — discard the patch, keep its tests (S) — done

The uncommitted `list-row-mapping` patch fixed real bugs (code nested in list items, marker-line blocks, unclosed fences, quote blank rows), but it did so with the very pattern this plan removes: three extra parses and a richer second model of the renderer. Its value is in its **regression tests**, which describe user-visible behavior, not implementation:

1. Discard its non-test changes, CHANGELOG and `docs/` edits included.
2. Port the **behavioral** tests to the restored tree (clicks land on the clicked line and char, cursor rows, gutter labels, overlay coverage), from `tests/mouse.rs`, `tests/ui.rs`, `tests/renderer.rs`, and `state_source_lines`. Mark each one that fails on HEAD `#[ignore = "row-provenance: phase N"]`, naming the phase that un-ignores it.
3. Add the case the review found, which the patch also gets wrong: a loose list inside a blockquote (`> - a\n>\n> - b\n>\n> tail`) puts every row below it off.
4. Leave out the tests of the patch's internals (`code_lines`, `list_lines`, `quote_blanks`, `raw_lines_by_sub_row`). They are gone with the patch. The agreement test's seed corpus is instead the sources of the behavioral tests that commit `0c65503` added (listed under [Phase 2](#phase-2--the-renderer-emits-roworigin-m)).

The ignored set is the acceptance list; `grep -rn 'row-provenance: phase' src tests` shows what's left. It holds tests for Phases 3 and 4. Phases 1, 2, 5, 6 and 7 are refactors or new work with no failing behavior on HEAD, so their done criteria are spelled out per phase below instead.

### Phase 1 — positions in the AST (M)

- `parse_blocks` and helpers take `(Event, Range)`; leaf blocks get `SrcLines`, containers' children get spans.
- **Still one parse.** A `#[cfg(test)]` thread-local counter in `markdown::parser`, incremented where `parse_raw_with_ranges` calls `Parser::new_ext`, asserts that one `ParsedDoc::build` increments it exactly once. It counts that call site only: `inline_col_map` builds a `Parser` per line and `parse_raw` builds one for tests and benches, and neither is the block parse. `state_source_lines`' `BUILD_COUNT` is the pattern.
- The renderer derives loose-list spacing from item spans; `annotate_list_blanks` and `blank_lines_before` go. **Output must stay byte-identical to HEAD**: the existing renderer tests and snapshots pass unchanged.
- **Proptest** (in `tests/source_map.rs`, over generated documents of lists, quotes, code, and nesting, without tabs):
  - every `SrcLines` line index points at a real line of its top-level block;
  - child spans nest inside their parent's;
  - for each paragraph and code-block line with `Some(col)`, `col` equals the char column of the start of the first pulldown-cmark event whose range starts on that line (re-derived inside the test with a separate `into_offset_iter` pass, which is fine in a test);
  - for each `None`, no content event starts on that line.
- **Unit tests:**
  - tab handling: a list item continued with a tab gets `content_col` at the tab's own column (see [Risks](#risks));
  - the 70,000-space lazy continuation line from §1 records `content_col == 70_000` without panicking.
- **No consumer changes and no rendering changes.** The only snapshot churn is the AST debug snapshots gaining `SrcLines` (see [Risks](#risks)).
- **Done when:** the suite is green, the proptest and unit tests pass, and `annotate_list_blanks` and `ListItem::blank_lines_before` are gone.

### Phase 2 — the renderer emits `RowOrigin` (M)

- Sink, `ParsedDoc::build`'s own rows, container wrapping, cache, and `ParsedDoc::row_origins` as in §2. `Flow` origins for reflowed top-level paragraphs. The setext H2 rule moves into `render_heading`. Nothing reads origins yet.
- **Agreement test, the safety net for the rest.** It lives in a new `tests/row_provenance.rs`. It runs over a fixed corpus plus a proptest generator (lists, quotes, code, nesting, headings and tables inside items, loose lists inside quotes, lazy continuation lines), with reflow both on and off.
  - **What it checks:** for every row, the origin is checked against the rendered output itself, not against today's mapping, so it can't inherit that mapping's bugs.
  - **Universal:**
    - `row_origins.len() == lines.len()`;
    - every `Some(range)` lies inside its block's source lines and is non-empty;
    - `rendered_col` ≤ the row's cell width;
    - `raw_col` ≤ its line's char length.
  - **Per `ColOrigin`/`ContentKind`, where "the row's content" is the row's text past `rendered_col` and "the source content" is the source line's text past `raw_col`:**
    - `Chrome`: nothing beyond the universal checks. Chrome is defined by having no column relation.
    - `Inline`: `InlineColMap` built over the source content has `rendered_len()` equal to the row's content length in chars, and the rendered text it implies equals the row's content.
    - `Verbatim`: the row's content equals the source content after the renderer's tab expansion (and trailing pad trimmed).
    - `TableRow { row, sub }`: `row` is a data/header row index the table's AST has, and `sub` is below that row's wrap-chunk count from `table_layout`. Each cell's text in the row's content equals chunk `sub` of the corresponding cell's `table_layout` wrap.
    - `Flow`: `InlineColMap` built over the flow text defined in §2 has `rendered_len()` equal to the row's content length.
- **Seed corpus:** the source strings of the tests commit `0c65503` added:
  - `"8. Tag it.\n\n    ```bash\n    gh run watch\n\n      indented\n    ```\n"`
  - `"8. Tag it.\n\n    ```bash\n    git tag\n    gh run watch\n    ```\n"`
  - `"- ```bash\n  code\n  ```\n- next item\n\n- third\n"`
  - `"- ```bash\n  gh run watch\n  ```\n- next item\n"`
  - `"- - a\n  - b\n- c\n"`
  - `"1. a\n   - ```\n     x\n     ```\n2. b\n"`
  - `"- a\n  ```\n  x\n- b\n"` (unclosed fence)
  - `"-\n  text\n- b\n"`
  - `"- a\n  soft\n- b\n"`
  - `"> - a\n>\n> - b\n>\n> tail\n"`
  - `"- a\n  - b\n    soft word\n"`
  - `"- a\n  - b\n    soft *word*\n    - c\n      deep word\n\nafter\n"`
  - `"- a\n- > q\n\nafter\n"`
  - `"- a\n- - b\n\nafter\n"`
  - `"- a\n- - q\n\nafter\n"`
  - `"- a\n  soft\n- b\\\n  hard\n1. c\nlazy\n"`
  - the four quote sources of `a_blockquote_renders_one_row_per_source_line`
  - `tests/fixtures/general.md` and `tests/fixtures/syntax.md`
- **Done when:** the suite is green with output byte-identical to HEAD, and the agreement test passes on the corpus and on 256 generated cases.

### Phase 3 — row consumers and the rendering decisions (M)

- Switch every row question to `row_map`: the cursor row, gutter, reveal-loop row selection, click row, overlay row, and `revealed_raw_row_count`'s line lookup. Delete `sub_lines_in_block` and the gutter's inversion rules.
- Land both [rendering decisions](#rendering-decisions) in the same change. They change row counts, and only after this phase does every consumer read rows from origins. Landing them earlier would put the old `sub_lines_in_block` model out of step with the renderer for every quote and list in the interim.
- **Done when:** the suite is green, and every `row-provenance: phase 3` test is un-ignored and passing:
  - the cursor-row and gutter tests in `state_source_lines`;
  - the click-line tests in `tests/mouse.rs`;
  - the quote/loose-list case;
  - the two rendering-decision tests in `tests/renderer.rs`.

### Phase 4 — column consumers (M)

- Switch the cursor indicator, overlay painter, and click column to `row_map`'s column pair, and the reveal gate to `reveals`. That includes the top-level `Flow` arm `rendered_sub_line_to_offset` carries today.
- Delete `code_layout`'s sniffers, `list_layout`'s marker sniffers, and the per-kind arms. That includes the "code arm must precede the list arm" ordering invariant, which stops existing.
- Add the round-trip proptest from [Testing strategy](#testing-strategy).
- **Done when:**
  - the suite is green, with every `row-provenance: phase 4` test un-ignored and passing;
  - the round-trip proptest passes;
  - `grep -n 'raw_list_marker_char_width\|rendered_list_marker_char_width\|code_indent_strip_chars\|line_allows_raw_reveal' -r src` finds no definitions.

### Phase 5 — tables (S–M)

- The renderer tags table rows `TableRow { row, sub }` and borders `Chrome`.
- `classify_table_sub_lines` and `is_table_block` leave the mapping paths. Cell geometry stays in `table_layout`.
- `cell_overlay` reads `row`/`sub` from the origin instead of classifying glyphs.
- **Done when:**
  - the existing table mapping tests pass unchanged:
    - in `tests/mouse.rs`: `click_in_table_cell_with_code_span_maps_through_hidden_backticks`, `click_on_table_cell_stays_in_that_cell_despite_padding`, `triple_click_in_table_cell_selects_only_cell_content`, `same_line_click_inside_table_still_sets_drag_in_progress`;
    - in `tests/ui.rs`: `rendered_view_selection_inside_cursors_own_cell_survives_cell_overlay`;
    - `mouse_ops::selection::banded_copy_takes_the_cell_from_every_sub_line`;
    - all of `tests/table.rs`.
  - a new test, `click_on_a_table_inside_a_list_item_lands_in_the_clicked_cell` (`"- item\n\n  | a | b |\n  |---|---|\n  | 1 | 2 |\n"`, a click on the `2`), passes;
  - the agreement test's `TableRow` arm passes on that source too;
  - `classify_table_sub_lines` and `is_table_block` have no callers in `editor/mouse_ops/coord.rs`, `editor/state.rs`, `ui/rendered_view/paint.rs`, or `ui/rendered_view/cell_overlay.rs`. Their editing-path callers (`table_edit`, `table_view`) stay.

### Phase 6 — images, diagrams, headings (S–M)

- Image reserved rows and big-H1 glyph rows become `Chrome`, with `lines` covering the block's source lines. `is_image_block`'s row pinning in `coord.rs` and `state_cursor_block.rs` falls out of `line_for_row`.
- **The math-preview band stays editor-owned.** `math_source_offset` is set by `EditorState::refresh_parsed` from the current `ImageReveal`. That depends on where the cursor is and on the math-preview setting. Emitting the band from the renderer would put cursor state into the render cache key and invalidate a block's cache on every reveal.
- **The diagram reveal goes behind one helper instead.** Today it maps 1:1 from reserved rows to source lines, shifted down by the band, at three call sites (`coord.rs`, `state.rs`, `rendered_view.rs`). Phase 6 gathers it into one function, `row_map::revealed_diagram_line(parsed, block, row_in_block) -> Option<usize>`. It reads `latex_source_offset` and returns `None` for a band row (a click there lands on the first source line, as today). It is the only caller of `latex_source_offset`.
- **Done when:**
  - the existing tests pass unchanged:
    - in `tests/mouse.rs`: `click_on_visible_mermaid_block_parks_cursor_at_end_of_last_code_line`, `click_on_visible_image_block_parks_cursor_at_end_of_source_line`, `click_on_mermaid_row_lands_on_clicked_column`, `click_on_wrapped_mermaid_line_lands_on_continuation`, `click_on_reserved_image_row_does_not_poison_inline_map_cache`;
    - in `tests/diagrams.rs`: the `revealed_diagram_*` tests;
    - in `editor::state`: `math_preview_offsets_source_rows_below_the_formula_band` and the other `latex_reveal_*` tests;
    - the `big_h1_*` renderer tests;
  - `grep -rn 'latex_source_offset' src` finds only its definition and `row_map`;
  - `is_image_block` has no caller in `editor/mouse_ops/coord.rs`.

### Phase 7 — nested reflow (L)

`nested-reflow.md`'s §A (nested ranges) is Phase 1, and its §B–§C (per-line metadata, prefix-aware mapping) are Phases 2–4, generalized beyond reflow. What remains is its consumer work: `Flow` origins for list-item, quote, and footnote paragraphs; `EffectiveRows` splicing one flow inside a multi-row block; and the reveal re-drawing container chrome on stacked raw lines. When Phase 4 lands, mark `nested-reflow.md` superseded except for that consumer work; delete it when this phase lands.

**Done when:** the battery in `nested-reflow.md` § Testing is written as tests and passes. Its round-trips, reveal re-prefixing and gutter cases come from there; its range-scan and generalization items are already covered by Phases 1 and 2. In addition, the agreement and round-trip proptests pass with reflow on over the full generator.

### Phase 8 — docs (S, alongside each phase)

`editing-model.md` loses most of its "must agree / never re-derive" bullets and gains one on `RowOrigin` being the only mapping. Also update `input.md`, `blockquotes.md`, `tables.md`, the AGENTS.md project structure (`document/row_map.rs`, `tests/row_provenance.rs`; `code_layout`/`list_layout` shrink or go), `performance.md` (re-run the M3 pipeline benches after Phases 1 and 2), and the user-facing `docs/editing.md` for any rendering change.

## Testing strategy

- **The Phase 0 acceptance suite** is the user-visible definition of done for Phases 3 and 4; the per-phase done criteria cover the rest.
- **The Phase 2 agreement proptest** checks origins against rendered output, phase by phase. Its vocabulary deliberately *includes* the cases the discarded patch couldn't model: headings and tables inside items, loose lists inside quotes, lazy continuation lines.
- **Round-trip proptest** (Phase 4): for random rendered cells, `rendered_to_raw_col` then `raw_to_rendered_col` returns the same cell or the content start, and the cursor indicator lands where a click on that cell put the cursor. This is the property issue #28 and its successors kept violating.
- **No new tests of a derivation**: the derivations stop existing.
- **Benchmarks:** Phase 1 should make the pipeline *cheaper* than the discarded patch (one parse, not four). Phase 2 adds one small `Vec` per rendered line and one entry per cached block; measure both with `cargo bench` before and after.

## Risks

- **Phase 1 touches the parser's every helper.** It's mechanical but wide. Snapshot tests on the AST catch any change in tree shape; `SrcLines` fields should be excluded from the existing AST snapshots (or the snapshots re-accepted once) so the diff stays reviewable.
- **Tab stops.** pulldown-cmark can start content mid-tab, emitting synthesized spaces. `content_col` is a raw char column, so a partially consumed tab maps to the tab's own column. That's the same as today, but now it's documented in one place. (This is why the Phase 1 proptest generator excludes tabs and a unit test covers them.)
- **`InlineColMap` over a sliced line.** Mapping the text past `content_col` (not the whole line) changes what the map parses. It fixes the "4+ space continuation parses as indented code" problem the discarded `ListIndent` worked around, but every inline-map call site must slice consistently. That's `row_map`'s job alone.
- **Partial migration.** Between Phases 3 and 4, rows come from origins while columns still come from the old chains. The old column chains are correct wherever the old row mapping was, so the interim is never worse than HEAD; the acceptance suite enforces that. The rendering decisions land in Phase 3 with the row consumers for the same reason.

## Rendering decisions

Both settled 2026-10-05; each lands in **Phase 3**, together with the row consumers that read the new row counts, and is encoded by an ignored Phase 3 test.

1. **List-item paragraphs with reflow off.** At HEAD, an item's *first* paragraph joins soft breaks with spaces and its later paragraphs keep the source's lines, which is inconsistent. With origins, either is mappable (a joined row is a `Flow`). **Decision:** one row per source line, like a top-level paragraph with reflow off; with reflow on, Phase 7 reflows them all. Test: `a_list_items_first_paragraph_renders_one_row_per_source_line`.
2. **Bare `>` lines.** **Decision:** render one quoted blank row per bare `>` line, as the discarded patch did, instead of one blank between every pair of children. It's a fidelity fix the spans make free. Test: `a_blockquote_renders_one_row_per_source_line`.
