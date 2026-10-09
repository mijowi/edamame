# Row provenance — the renderer records where each row came from

Status: **DONE (2026-10-07)** — every phase landed; the cost is measured and accepted ([Phase 8](#phase-8--docs-s-alongside-each-phase--done)). Targeted at the next release, which ships every phase together, nested reflow included. Supersedes the discarded `list-row-mapping` patch (see [Phase 0](#phase-0--discard-the-patch-keep-its-tests-s--done)) and absorbed `nested-reflow.md` (deleted with Phase 7) as this plan's Phase 7. Sibling context: [`editing-model.md`](../editing-model.md), [`input.md`](../input.md), [`blockquotes.md`](../blockquotes.md), [`tables.md`](../tables.md).

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
    /// blank row between blocks.  A selection washes the whole row; a click lands at the row's first line's
    /// `content_col`, or where it is `None` at the leaf's nearest one (Phase 4 review).
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
    /// exactly today's block-wide map.  (Unsafe as written; Phase 4 maps line by line first.)
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
- **Diagram reveal** (mermaid and `$$…$$` rows painted 1:1 with source lines, below a math-preview band). See [Phase 6](#phase-6--images-diagrams-headings-sm--done).

**The functions.** All operate on rows shown rendered:

- `row_for_line(block, line) -> usize`: the first row whose lines reach `L` (`lines.end > L`), else the last row that shows a line. This is the same prefix-sum rule `sub_lines_in_block` encoded: a line rendering no row (an interior blank, a setext underline, a bare `-`) shares the next line's row, and an interior line of a multi-line `Flow` row lands on that row.
- `line_for_row(block, row) -> usize`: the row's `lines.start`, or, for `None`, the nearest owned line above. This is the inverse the discarded `raw_lines_by_sub_row` reconstructed.
- `raw_to_rendered_col(row, raw_col) -> Option<usize>` and `rendered_to_raw_col(row, rendered_col) -> usize`: one `match` on `ColOrigin`. Inside the prefix, both align the rendered prefix with the raw one from the right (see the Phase 4 notes). `raw_to_rendered_col` returns `None` where the inline map can't place a column, so a selection skips instead of painting off by N (a search match takes the one-for-one guess; Phase 4 review). For a `Flow` row the raw side is a `(line, col)` pair, not a bare column.
- `row_for_pos(block, pos) -> usize`: the cursor's row. `row_for_line`'s, unless that row is chrome and a later row showing the line places the column exactly (a marker on a row of its own, `- - a`); added in the Phase 4 review.
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
| `is_image_block` row pinning; `latex_source_offset`'s scattered call sites | `Chrome` rows from the renderer; one diagram-reveal helper in `row_map` (Phase 6, done) |

Unchanged: `SourceMap`'s block-level role (byte → top-level block, extended ranges for cursor lookup, virtual blank blocks), `InlineColMap`, `table_layout`'s cell geometry, `EffectiveRows`' shape and the wrap helpers in `line_render`, the diff view.

## Out of scope

- **The diff view's own layout** (`diff::layout`, `DiffView`). It calls the renderer and so compiles against the new sink, but its row model is untouched.
- **`table_layout`'s cell geometry** and column widths. Phase 5 only changes how a row *finds* its table row and chunk.
- **`SourceMap`'s block lookup** and the virtual-blank-block scheme.
- **Preview mode's mapping.** It never reveals, and it already reads the rendered rows. It benefits from `row_map` incidentally but gets no dedicated work.
- **Reflowing across hard breaks.** The `HardBreak` fallback in `render_paragraph` stays.
- **The wrap engine** (`line_render`) and `EffectiveRows`' reveal patch.
- **Any rendering change** beyond the two in [Rendering decisions](#rendering-decisions) and the setext H2 rule moving from `build` to the renderer. The review of Phases 1–3 added three small ones, recorded in their implementation notes: the setext H2 rule's reach, footnote continuation indents, and link definitions inside a quote.

## Phases

Each phase lands on its own with the full suite green. Sizes are relative (S < M < L).

### Phase 0 — discard the patch, keep its tests (S) — done

The uncommitted `list-row-mapping` patch fixed real bugs (code nested in list items, marker-line blocks, unclosed fences, quote blank rows), but it did so with the very pattern this plan removes: three extra parses and a richer second model of the renderer. Its value is in its **regression tests**, which describe user-visible behavior, not implementation:

1. Discard its non-test changes, CHANGELOG and `docs/` edits included.
2. Port the **behavioral** tests to the restored tree (clicks land on the clicked line and char, cursor rows, gutter labels, overlay coverage), from `tests/mouse.rs`, `tests/ui.rs`, `tests/renderer.rs`, and `state_source_lines`. Mark each one that fails on HEAD `#[ignore = "row-provenance: phase N"]`, naming the phase that un-ignores it.
3. Add the case the review found, which the patch also gets wrong: a loose list inside a blockquote (`> - a\n>\n> - b\n>\n> tail`) puts every row below it off.
4. Leave out the tests of the patch's internals (`code_lines`, `list_lines`, `quote_blanks`, `raw_lines_by_sub_row`). They are gone with the patch. The agreement test's seed corpus is instead the sources of the behavioral tests that commit `0c65503` added (listed under [Phase 2](#phase-2--the-renderer-emits-roworigin-m--done)).

The ignored set is the acceptance list; `grep -rn 'row-provenance: phase' src tests` shows what's left. It held tests for Phases 3 and 4, and is empty since Phase 4 landed. Phases 1, 2, 5, 6 and 7 are refactors or new work with no failing behavior on HEAD, so their done criteria are spelled out per phase below instead.

### Phase 1 — positions in the AST (M) — done

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

**Implementation notes (2026-10-05).** The stream lives in `markdown::parser::stream` (`EventStream`): the parser asks it for events exactly as before, and it notes each consumed event's position against the open leaf. Deviations and findings:

- **`content_col` needed four rules the design didn't state**, all forced by what pulldown-cmark emits:
  - *Text runs across lines.* A code block's blank line arrives folded into the previous line's `Text` (`"x\n\n"`), so no event starts on it. A `Text` (or block `Html`) running onto a later line holds that line from column 0 — safe, because pulldown-cmark splits text wherever a container prefix intervenes.
  - *Atomic inlines across lines.* A multi-line code span, math span or inline HTML spans a container prefix it doesn't hold (its payload is re-assembled), and renders on its first line's row. Its later lines are `None`, even when a later event starts on one (`d` `` ` `` ` e`). So `Some` marks exactly the lines a paragraph segment begins on, which the renderer relies on (Phase 2).
  - *A line opening with a link's or image's close.* In `[a\n](u)`, the only event on line 1 is `End(Link)`, and an `End` repeats its `Start`'s range. The recorder notes such a line itself, past its container prefix (`>`, spaces, tabs; paragraph text can't begin with `>`). Without that, the line had no column, and `paragraph_rows` handed the next segment's row a chrome origin.
  - *Images are atomic inlines too.* Their alt text renders on the image's first row, so alt lines after the first are `None`, like a code span's. The renderer counts no breaks inside an image, and before this a multi-line alt shifted every row below it up a line.
  - The proptest re-derivation encodes the first two, so it reads: `Some(col)` is the earliest content event start on the line, or 0 if a text run begun above covers it; `None` means no content event starts there unless the line continues an atomic inline.
- **Tabs.** Content starting inside a partly consumed tab arrives as an *empty-range* `Text` of synthesized spaces placed *past* the tab; the recorder maps it back to the tab's own column. A continuation line simply indented by a tab (`- a\n\tb`) records the column of `b`, after the tab — the tab there is whitespace, not content. Both are unit tests.
- **Container spans.** A blockquote's span comes from its range (a trailing bare `>` is the quote's), less a last line the range only reaches partway into (pulldown-cmark ends some ranges inside the next line's `> ` prefix). A list's, an item's and a footnote definition's come from their children, because their ranges absorb the blank lines after them.
- **Loose-list spacing differs from HEAD in two cases, both fixes.** The source scan `annotate_list_blanks` did never saw a fence opening on a marker line, and never closed a fence that ended with its container, so it double-counted a blank that is code content (`- ```\n\n- a`) or dropped a real separator after an unclosed nested fence. A differential run over 2,000 generated documents found no other disagreement; the existing suite is unchanged.
- **CRLF.** A content event starting at a CRLF line's `\n` takes the `\r`'s column, so a blank CRLF code line records column 0, the same as an LF one.
- The generator is shared by both proptests: `tests/support/markdown_gen.rs`.

### Phase 2 — the renderer emits `RowOrigin` (M) — done

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

**Implementation notes (2026-10-05).** Types in `markdown::row_origin` (`RowOrigin`, `ColOrigin`, `ContentKind`, `RowSink`); `ParsedDoc::row_origins()`. The agreement test also passed 15,000 generated cases in stress runs. Since the Phases 1–3 review it runs each document four ways (reflow off and on, each plain and with big H1, striping and figures-off diagrams), and checks a row that continues an atomic inline by its letters and digits instead of skipping it; the Phase 1 position proptest gained a CRLF twin that must record the same lines and columns as LF. Deviations and findings:

- **Tables are tagged now**, not in Phase 5: `TableRow { row, sub }` (row 0 the header, `1 + i` data row `i`). The heavy rule shows the delimiter line, each separator and the bottom border the line above them, and the top border none, so a line's first row is its content. Phase 5 still moves the consumers.
- **Image rows: row `k` shows line `min(k, last)`, as chrome.** That encodes today's two behaviors directly: a one-line image pins every reserved row to its line, and a diagram's reveal maps rows 1:1 onto source lines. Phase 6 may revisit it.
- **Figures-off `$$…$$` body rows take the opening line's column.** pulldown-cmark reports the formula as one span with its container prefixes stripped, so its later lines carry no column (see Phase 1). The opening line's column is right whenever the body shares its container prefix; a lazy line inside a nested formula would be off.
- **Raw HTML rows are `Verbatim`** (settled 2026-10-06). At HEAD they reveal; Phase 4's "`Verbatim` never de-renders" gate stops that, which is harmless, since the raw and rendered text are the same.
- **Multi-line rows outside reflow are `Flow`:** a list item's or setext heading's text spanning several lines, and a paragraph segment that runs onto the tail of a multi-line code span.
- **The setext H2 rule** is emitted for every *top-level* setext H2, multi-line ones included, so the reveal has a row for the underline as it does under an H1. A nested setext H2 still gets none. That differs from HEAD, whose `detect_setext` gave a multi-line H2 no rule and missed `Foo\n   ---` and `#tag\n---`, both of which pulldown-cmark parses as setext. `detect_setext` is gone: `RenderedView` asks the AST (`Block::is_setext_heading`), so the view and the renderer can't disagree about which blocks have a rule row. A renderer-level test that pinned "no rule from the renderer" was updated to the new contract.
- **Footnote continuation indent is in cells.** A definition's continuation lines align under its text by the leader's cell width, where HEAD counted chars, so `[^日本]: a\n    b` indents two cells further than before. It's an alignment fix, and the only other output change in Phase 2.
- **Finding for Phase 4 (resolved there, see its notes): `InlineColMap` over a slice is fragile.** Built on its own, a sliced line reading `2. a` parses as a list item, and a flow joined by `\n` turns a lazy `===` continuation into a setext underline. Neither was so in the document, so the §2 `Flow` definition (lines joined by `\n`) is unsafe as written. `InlineColMap` also doesn't model smart punctuation collapsing a run (`---` → `—`). The agreement test works around all three (a leading word pins paragraph context, flows are mapped line by line, smart-punctuation runs are skipped); `row_map`'s column functions need a real answer.

### Phase 3 — row consumers and the rendering decisions (M) — done

- Switch every row question to `row_map`: the cursor row, gutter, reveal-loop row selection, click row, overlay row, and `revealed_raw_row_count`'s line lookup. Delete `sub_lines_in_block` and the gutter's inversion rules.
- Land both [rendering decisions](#rendering-decisions) in the same change. They change row counts, and only after this phase does every consumer read rows from origins. Landing them earlier would put the old `sub_lines_in_block` model out of step with the renderer for every quote and list in the interim.
- **Done when:** the suite is green, and every `row-provenance: phase 3` test is un-ignored and passing:
  - the cursor-row and gutter tests in `state_source_lines`;
  - the click-line tests in `tests/mouse.rs`;
  - the quote/loose-list case;
  - the two rendering-decision tests in `tests/renderer.rs`.

**Benchmarks after Phases 1–3 (2026-10-06, against HEAD `6cf3e85`).** Measured as user-space instruction counts on a pinned P-core (repeatable to 0.001%; wall-clock on the power-saver laptop swings ±10–20%), 20k-line corpora:

| Corpus | `parse_merged` | `full_pipeline_memoized` (per edit) |
|---|---|---|
| prose | +8.8% | +4.7% |
| lists | +15.1% | +8.8% |
| tables | +10.0% | +11.1% |
| code | +51% (of a ~13M-instruction parse) | +13.2% |
| math | +16.2% | +11.6% |
| nested | +16.4% | +2.7% |
| mixed | +12.8% | +10.1% |

Cycles run 5–10 points above the instruction deltas. The first pass measured +15–26% parse and +10–25% per-edit instructions; what brought it down:

- **`SrcLines` stores its columns compactly**: a uniform column with at most two chrome lines is held inline, any other shape as one boxed slice. No leaf allocates for a common shape, and a cache key hashes a few words for it.
- **The recorder borrows one reusable column buffer** and writes `u32`s directly.
- **The range tracker lives in `EventStream`**, whose `pull` / `peek` / `next` are force-inlined: the `Map` closure layer and the out-of-line hand-off copied every `Event` several times.
- **Line lookups ride the forward cursor** — a leaf's and a container's first line, `set_base`, and a range's last line counted forward from its first — and a single-line event's end line is one comparison, not a scan.
- **`paragraph_rows` is a lazy iterator**, so a paragraph allocates nothing for its row origins.
- **The render cache** pre-sizes each build's map (no rehash, so no re-hashing every key) and asks `prev` first, the common hit: two hashes per hit instead of three.

What remains is mostly the recording itself, plus `Block` growing from 80 to 112 bytes (`Table` is the largest variant). Shrinking `Block` back was tried and did not improve these figures. Accepted with the finished plan; see [Phase 8](#phase-8--docs-s-alongside-each-phase--done) for the final figures.

**Implementation notes (2026-10-05).** `document::row_map` has `row_for_line`, `line_for_row` and `lines_of_row`; `sub_lines_in_block`, `cursor_sub_line_in_block` and the gutter's inversion rules are gone. Deviations:

- **`row_for_line` is "the first row whose lines reach `L`" (`lines.end > L`)**, not "`lines.start >= L`". The two agree on one-line rows; the stated rule would send an interior line of a multi-line `Flow` row to the next block's row.
- **The math-preview band is applied in `row_map`** (it reads `latex_source_offset`), since it is cursor state the origins can't carry. Phase 6's `revealed_diagram_line` can absorb it.
- **Tables:** the cursor's row and the gutter read origins, but a click's and the overlay's *row* still come from `classify_table_sub_lines`. On origins, a click on the heavy rule would land on the delimiter line instead of the first data row, and Phase 5's done criteria require the table mapping tests unchanged. Phase 5 moves them.
- **The gutter numbers a row with the first line it shows, and only the first row to reach a line** (a global ascending rule), so a code row and a virtual blank sharing a line can't both carry its number.
- **Rendering decision 1 applies whether reflow is on or off.** The acceptance test runs in Rendered mode, where reflow is on, and expects one row per source line. That matches the item's later paragraphs, which never reflowed; Phase 7 reflows them all.
- **`click_below_a_fence_on_a_list_marker_line_lands_on_clicked_line` is re-tagged phase 4.** Every click in it now lands on the right line, but on the code body row it also asserts the exact column, and a code row nested in a list item is Phase 4's column mapping (the same as `click_on_code_nested_in_list_item_lands_on_clicked_char`).
- **The revealed cursor row paints the cursor's own line**, not the line its origin names. The two differ when the cursor's line renders no row (an interior blank, the second text line of a multi-line setext heading) and so shares the next line's row. `RenderedView`'s setext arm, `revealed_raw_row_count` and the click mapping all use the cursor's line on that row (`editor::state::cursor_raw_line`). Every other row reads its line through `line_for_row`.
- **Link reference definitions inside a quote render nothing**, as at top level. The quote's gap-fill would have given each one a blank row, since no child covers it. The parser lists them in `BlockQuote::hidden`.

### Phase 4 — column consumers (M) — done

- Switch the cursor indicator, overlay painter, and click column to `row_map`'s column pair, and the reveal gate to `reveals`. That includes the top-level `Flow` arm `rendered_sub_line_to_offset` carries today.
- Delete `code_layout`'s sniffers, `list_layout`'s marker sniffers, and the per-kind arms. That includes the "code arm must precede the list arm" ordering invariant, which stops existing.
- Add the round-trip proptest from [Testing strategy](#testing-strategy).
- **Done when:**
  - the suite is green, with every `row-provenance: phase 4` test un-ignored and passing;
  - the round-trip proptest passes;
  - `grep -n 'raw_list_marker_char_width\|rendered_list_marker_char_width\|code_indent_strip_chars\|line_allows_raw_reveal' -r src` finds no definitions.

**Implementation notes (2026-10-06).** `document::row_map` gained `RawPos`, `rendered_to_raw_col`, `raw_to_rendered_col`, `raw_to_rendered_col_near` and `reveals`; the click (`coord::rendered_sub_line_to_offset`), the cursor indicator (`RenderedView`), the overlay painter (`paint_byte_range_overlay`) and `revealed_raw_row_count` call them. `code_layout` and `list_layout` are deleted outright (`CODE_PAD_COLS` moved to `renderer`), as are `EditorState::inline_map_for`, `ParsedDoc::inline_map` and the per-buffer-line map cache: maps are now built from the parse's own text and cached per row (`ParsedDoc::row_cache_or_init`), so a non-canonical `(line, text)` pair can't poison them. The round-trip proptest is in `tests/row_provenance.rs` (corpus, fixtures and 64 generated cases, four variants each; 5,000 cases in a stress run). Deviations and findings:

- **The prefix maps right-aligned, not clamped.** §3 says both directions clamp inside the prefix to the content start. Clicks can't: a task box (`[ ] ` against `- [ ] `), a footnote leader (`  1.  ` against `[^1]: `, the back-link hit-test) and a list marker are hit by mapping the rendered prefix onto the raw one from the right, as `list_layout`'s inverse marker map did, and five existing tests pin it. Indentation stands for nothing and lands past itself (on the marker, or on a continuation line's content, as `click_on_a_nested_items_continuation_lands_on_clicked_char` wants); a code row's pad cell lands on the content start. The alignment's anchor is the content column less any spaces the raw prefix ends in beyond the rendered prefix's, or a marker padded out to its content (`1.  foo`, `-   foo`) would meet a padding space instead of its glyph. The inverse is right-aligned too, so the pre-reveal indicator for a cursor on `#` or `-` sits on the rendered prefix, not on the content, and the round trip is exact.
- **`Flow` columns: line by line first, then joined.** Each line's slice past its content column gets its own map, the maps' spans joined by one space; if that doesn't render exactly what the row shows (an inline spanning a break, `*a⏎b*`), one map over the slices joined by `\n` is tried; if neither fits, the row has no exact map. A break's space maps to the end of the line it ends. Every slice is mapped through the new `InlineColMap::build_inline`, which prefixes a word so a slice reading `2. a`, `# b` or an indent parses as the paragraph text it was (the agreement test's trick, promoted).
- **`InlineColMap` models smart punctuation now.** `...` → `…`, `--` → `–`, `---` → `—` each map one glyph to its run's first char, so a line holding one has an exact map. Without it, "`None` → the overlay skips" would have dropped the selection highlight on every prose line containing an ellipsis, where HEAD painted 1:1.
- **Columns on a block's first line count from its range start**, in `RawPos` as everywhere else: an indented code block's range starts past its indent, and `raw_block_cursor`, the click and the overlay all slice the block from there. `row_map` converts to the origins' line columns internally.
- **An overlay reaching a row's first char paints its prefix** (the marker, the bar), as the list path did for a line selection; one covering the row's last char paints the whole content even where no map places the end. A `Verbatim` row's prefix (the pad cell) stays unpainted, as before. Chrome rows wash whole, as fence rows did.
- **Kept on the old paths, by design:** tables (Phase 5 — rows and columns still from glyph classification and pipes; a click on a pipe-less border row now maps one-for-one, clamped, where it went through the generic inline map), diagram reveals (Phase 6 — the overlay maps a diagram's rows one-for-one onto their source lines, as before), the stacked reveal of a reflowed paragraph, and the revealed cursor row (raw text, raw wrap).
- **Preview never reveals.** `is_revealed_cursor_row` now checks the mode: in Preview the cursor's row took the revealed-row shortcut and a click on it mapped against raw text. The reflowed-paragraph arm was the only one that had checked.
- **Behavior changes:** frontmatter body rows and raw HTML rows no longer de-render (they are `Verbatim`; same text either way, settled for HTML in Phase 2); a big-H1's glyph rows and other chrome rows wash whole under a selection or yank of their line, but a search match or `:s` preview leaves them alone; a click on a chrome row lands on its line's content start (a rule's first `-`), or on a line that is all chrome on its leaf's nearest content column (see the second review).

**Review follow-ups (2026-10-06).**

- **References resolve.** Each map parses one line, with no definitions in scope, so a reference link (`[a][r]`, `[r]`) read as literal brackets and its row had no exact map: overlays painted nothing on such a line, where HEAD painted 1:1. The parse now hands back the link definitions' labels (`parse_document`), `ParsedDoc` adds the footnote definitions', and `InlineColMap::build_inline` resolves against both (`RefLabels`), so an undefined `[^x]` stays literal and a defined one collapses. Labels match lowercased, which misses a fold-specific letter (`ss` against `ß`); such a row just has no exact map.
- **ATX closing sequences** (`## Title ##`) are stripped from a heading row's slice (`strip_atx_closing`). HEAD mapped these exactly.
- **Coverage is asserted.** The round trip requires every `Inline` / `Flow` row to have an exact map, the generator now produces reference links, footnote references, smart punctuation, entities, escapes and closing sequences, and `clicking_where_the_cursor_shows_keeps_it_there` runs the round trip through `mouse_ops` and `RenderedView` at two widths. Known exceptions: a row continuing a multi-line code span, whose later line keeps as much indent as pulldown-cmark decides; and display math nested in a container, which isn't promoted and renders its formula on one row, newlines and all, which no inline map reproduces (a rendering issue, open: [#72](https://github.com/mijowi/edamame/issues/72), which also covers a quoted `$$` block's stray blank rows).
- **The cursor's row is chosen by position** (`row_for_pos`). The end-to-end test found that a line whose marker renders on a row of its own (`- - a`, `- > q`, `- # H`) put the cursor on that bare marker row whatever its column, and a click there landed on the content start, so clicking where the cursor showed moved it. A Phase 3 gutter test pinned the old row; its `- - a` cursor expectation moved to the row below.
- **Line lookups are indexed.** `row_map` reads lines through `ParsedDoc`'s line-start index (kept from the parse, which builds it anyway; `byte_to_line` uses it too), and the overlay painter through that index and the rope, so neither scans the block, and the painter no longer copies the whole buffer per call.

**Second review (2026-10-06).**

- **Smart punctuation beside `==`.** A text run holding both a literal `==` and a `...` took the highlight-marker path, which didn't collapse the run, so the row had no exact map. `push_text` now walks the text and the raw slice in lockstep, skipping a highlight pair's markers on both sides and collapsing each run.
- **A search match on an unmapped row shows.** A selection still skips such a row's interior, but a match the user jumped to took the same skip and showed nowhere, where HEAD painted it one for one. It now shows at the one-for-one guess, as wide as its text, kept inside the row (`paint::Overlay::Match`, which also leaves chrome rows alone).
- **A chrome row's click lands past the container prefix.** On a line that is all chrome (a fence, a setext underline) it took char 0, the item's indent or the quote's `>`; it now takes the nearest content column of the same leaf, below first, then above.
- **Per-row cache.** `row_map` keeps each row's chars and its block's position beside its column map (`row_map::RowCache`), so a column question no longer collects the row's text or looks up its block's first line each time. The round-trip proptest went from 19 s to 5 s at 256 cases in a debug build, and runs 64 cases (2 s).

### Phase 5 — tables (S–M) — done

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

**Implementation notes (2026-10-06).** The renderer already tagged rows in Phase 2, so this phase is consumers only. `document::row_map::table_row` (returning a `TableRowHit`: the table row's line, index, chunk, whether the row shows cells, and the rows its chunks span) answers every table row question from the origins, and `coord`'s click, `preview_table_cell_band`, `revealed_raw_row_count`, `paint_byte_range_overlay`, `RenderedView`'s cursor-row branch and `compute_wrapped_cell_overlay` call it. `table_raw_line_idx` is deleted, and so is `line_row_width`. `editor/state.rs` had no table mapping callers by HEAD. Deviations and findings:

- **Borders and separators snap by the old rules, from origins.** A chrome row belongs to a table when the leaf its line falls in (the next row's, for the top border, which shows no line) is a `Table` in the AST. It then snaps onto the table row showing its line (a separator and the bottom border, onto the row above), else onto the row directly below (the top border onto the header, the heavy rule onto the first data row), else onto the nearest above (the heavy rule of a header-only table). That keeps `click_on_thick_header_separator_redirects_to_first_data_row` and the other snapping tests unchanged. Without the AST check, every chrome row directly above a table's header would count as the table's.
- **`RenderedView` also moved** (`ui/rendered_view.rs`, not in the done list). Its cursor-row `is_table` read `is_table_block` over the block's raw source, so a nested table's cursor row de-rendered whole while the click path, now origin-based, treated it as a table. Both read `table_row` now, and a nested table reveals cell by cell. A side effect: during a typing burst with a stale parse, a block that only just became a table (its delimiter line just typed) waits for the re-parse, where the source check caught it at once.
- **A click on a border or separator maps its column through its table row's pipes**, not one-for-one onto the raw line. The corpus had no top-level table, and adding one (and the nested one moving onto the table path) showed that `clicking_where_the_cursor_shows_keeps_it_there` fails at HEAD: a click on the top border's first cell put the cursor on the raw `|`, which shows on the cell's pad, and a click there lands on the cell's text. A border's cells line up with its table row's, so it now lands where a click on the same column of that row would. The striped separator already worked this way.
- **The table click resolves the wrap first.** The quote's bar doesn't shrink the table's width, so a quoted table overflows a narrow viewport and its rows wrap, and the click took the screen column as the logical row's cell. It now goes through `click_to_rendered_char_idx`, as the generic path does. HEAD never hit this, because a quoted table took the generic path. Shrinking the nested table's width is a rendering change and stays out of scope.
- **Pipes were still matched across the whole row** at this point, prefix included, rather than past the origin's `raw_col` / `rendered_col`; since fixed ([#70](https://github.com/mijowi/edamame/issues/70)): cells are split as GFM splits them, from the origin's `raw_col`, edge pipes optional (see [`tables.md`](../tables.md)). It held because a rendered prefix never contains `│` and a raw one almost never contains `|`, and where the counts disagreed the row fell back to one-for-one columns and a whole-line reveal. The second review found that this was common, not exotic: any row missing an edge pipe (`a | b`, `a | b |`, both valid GFM) has fewer raw pipes than the renderer draws, and a footnote label holding a `|` with a table on its leader line (`[^a|b]: | x | y |`) has one too many. Rows with more or fewer cells than the header rendered correctly but missed the same way.
- **The drag clamp takes only a cell holding the byte** (second review). `table_edit::cell_at` treats text before the first `|` as prefix, so on a row with no leading pipe (`1 | 2 |`) a drag from `1` was clamped to cell `2`, a range that excluded its own anchor. HEAD never reached that path for such a row (`find_table_at` rejected it). `cell_at` now answers `None` outside every cell, which leaves that drag unclamped, as at HEAD.
- **Preview's cell band now covers nested tables too.** It read `is_table_block` over the block before.
- **Two more table checks moved, found in review.** A same-line click's drag suppression (`mouse_ops::apply`) read `is_table_block` over the block, so a click into another cell of a nested table's row skipped it and the cell reveal couldn't swap. The drag clamp (`table_cell_char_range_at`) used `find_table_at`, whose line scan rejects a `> `-prefixed row, so a drag in a quoted table wasn't kept to its cell. Both read `table_row` now; the clamp then parses the raw line's cells (`table_edit::cell_at`), which skips any prefix before the first `|`.
- **Tests:** `click_on_a_table_inside_a_list_item_lands_in_the_clicked_cell` (`tests/mouse.rs`), `rendered_view_nested_table_highlights_and_reveals_by_cell` (`tests/ui.rs`, both halves fail at HEAD), three `row_map` unit tests, `same_line_click_inside_a_nested_table_still_sets_drag_in_progress` and `a_drag_from_a_table_cell_is_clamped_to_it_at_any_depth` (`tests/mouse.rs`), a top-level and a quoted table in the row-provenance corpus, and from the second review `click_on_a_table_border_lands_in_the_cell_beside_it`, `clicks_on_a_wrapped_quoted_table_land_under_the_pointer`, `a_drag_from_a_leading_pipe_less_tables_first_cell_is_never_clamped_elsewhere` (`tests/mouse.rs`) and `cell_at_skips_a_prefix_and_never_answers_another_cell` (`table_edit`). The corpus already held the nested source, and its `TableRow` agreement arm passed on it before this phase. The round trip still skips table rows (their columns aren't `row_map`'s); the click-and-paint test covers them.

### Phase 6 — images, diagrams, headings (S–M) — done

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

**Implementation notes (2026-10-06).** Most of this phase had already landed by Phase 5: image reserved rows and big-H1 glyph rows were `Chrome` since Phase 2, and `coord.rs` had no `is_image_block` caller. What remained was the helper. `row_map::revealed_diagram_line` answers a diagram row's source line (`None` for a band row) from `lines_of_row`, and the click (`coord::rendered_sub_line_to_offset`), `revealed_raw_row_count` and both of `RenderedView`'s diagram branches (mermaid and `$$…$$`) call it. Deviations and findings:

- **Image rows keep one line per row** (row `k` shows line `min(k, last)`), not `lines` covering the whole block. Covering lines would make `row_for_line` put the cursor of every revealed diagram line on row 0, so the cursor row would need its own diagram rule; with one line per row, the origins already *are* the 1:1 reveal, and `revealed_diagram_line` simply reads them. Big-H1 glyph rows cover the heading's text lines, as the plan says. The Phase 2 note anticipated this choice.
- **`latex_source_offset`'s only caller is `row_map::own_origins`**, not `revealed_diagram_line`: `row_for_line` / `row_for_pos` need the band too (the cursor row of a revealed formula sits below it, which `math_preview_offsets_source_rows_below_the_formula_band` pins). The done criterion (only its definition and `row_map`) holds.
- **That test changed in one assertion.** It read `latex_source_offset` directly; it now checks `math_source_offset` itself, and gained two `revealed_diagram_line` assertions (a band row is `None`, the row below the band's first is line 1). The two criteria conflicted; the grep one won, with the test asserting the same fact.
- **`is_image_block` in `state_cursor_block.rs` stays.** It isn't row pinning but the reveal's choice of block (`image_reveal_target`), so nothing falls out of `line_for_row` there. `rendered_view::paint`'s use (skip a real image's rows in the overlay) also stays.
- **Rows past the source are `None` too.** The origins clamp a reserved row past the source to the last line, but until `sync_image_reveal` shrinks the reservation (the first revealed frame) those rows pad the block. Reading the origins directly repeated the last line there (the closing fence, raw under the cursor); the helper answers only rows whose line matches their index, and a click on padding still lands on the last line (`line_for_row`). Test: `mermaid_reveal_pads_rows_past_the_source` (`tests/ui.rs`).
- **The overlay painter no longer maps diagram rows** (review). Its diagram branch was a fourth 1:1 mapping, and it clamped to the rendered `Line`'s width, which is 0 on every reserved row past the first: a yank flash over a revealed diagram's line showed nowhere, or on the fence label. Search and `:s` turn the reveal off, and the selection was already painted by the view, so the yank flash was the only path that reached the branch. `paint_byte_range_overlay` now skips every image row, and the reveal paints the yank flash with the selection, both through `raw_text::raw_line_sel_cols`, which the setext and stacked reveals share. Test: `yank_flash_paints_a_revealed_diagrams_source_line` (`tests/ui.rs`).
- **The yank flash moved into `RenderedView`'s loop for every row** (review). Its post-pass walked the rendered layout, but a revealed row shows raw text and a stacked reveal changes row heights: on the cursor's row, a setext or big-H1 heading and a stacked reflowed paragraph it landed at rendered columns over raw text, washed a big H1's blanked rows and a setext underline whole, and was a row off below a revealed paragraph. The view now keeps one highlight range (the selection, else the flash) that the row overlay and every reveal branch use; the post-pass runs in Preview only. The big-H1 reveal also highlights its raw line now and skips the overlay on its blanked rows, which fixes the same wash for a selection. Test: `yank_flash_paints_revealed_rows_at_their_raw_columns` (`tests/ui.rs`).
- **Two small consistency fixes.** A band or padding row's raw row count is now 1 (it paints empty), where it took the wrap count of the formula's first line or the source's last; and the mermaid branch reads its row's line through the helper rather than using the row index directly. Neither changes output for any reachable case: `$$` never wraps, and mermaid has no band.

### Phase 7 — nested reflow (L) — done

`nested-reflow.md`'s §A (nested ranges) is Phase 1, and its §B–§C (per-line metadata, prefix-aware mapping) are Phases 2–4, generalized beyond reflow. What remains is its consumer work: `Flow` origins for list-item, quote, and footnote paragraphs; `EffectiveRows` splicing one flow inside a multi-row block; and the reveal re-drawing container chrome on stacked raw lines. When Phase 4 lands, mark `nested-reflow.md` superseded except for that consumer work; delete it when this phase lands.

**Done when:** the battery in `nested-reflow.md` § Testing is written as tests and passes. Its round-trips, reveal re-prefixing and gutter cases come from there; its range-scan and generalization items are already covered by Phases 1 and 2. In addition, the agreement and round-trip proptests pass with reflow on over the full generator.

**Implementation notes (2026-10-07).** Every paragraph reflows when reflow is on and it has no hard break (`Renderer::paragraph_reflows`; the list item's first paragraph and `render_block` both ask it, so the `top_level` flag no longer gates reflow). `ParsedDoc::is_reflowed_paragraph_at` is gone; its replacement is `row_map::stacked_lines(block, row)`, which asks the row's origin (a content row) and the AST (`leaf_at` its first line is a paragraph that can reflow). `EffectiveRows` splices the one flow row (`Patch::first_line`; `RowHit::Raw::raw_line` and `raw_line_visual_row` are block-relative, `raw_lines()` names the stack), and `EditorState::cursor_stacked_row` picks it from the cursor's position, read off the rope rather than a copy of the block. `RenderedView`, the click (`rendered_sub_line_to_offset`, `revealed_raw_row_count`) and the reveal timer read that same patch or gate; `revealed_source_lines` is deleted, since the stack is the flow row's lines. Deviations and findings:

- **Stacked lines are whole source lines, not rendered chrome plus raw text** (settled 2026-10-06). `nested-reflow.md` wanted each stacked line re-prefixed with the bar, leader or continuation indent. Every other revealed row, top-level and nested, already shows its whole raw line (`> alpha`, `- alpha`), so the stack does too: columns stay one for one, nothing hides the markers being edited, and a quote keeps its wash. "Re-drawing container chrome" became the wash alone. The reveal-re-prefixing tests assert this shape instead (`nested_reflowed_paragraph_reveals_its_whole_source_lines_stacked`).
- **The stack wraps as the painter does.** `EffectiveRows` measured raw lines with a flat wrap (`visual_rows_of_str`), and the painter hangs a raw line's wrapped rows under its marker. Top-level paragraph lines rarely start with one, so they almost never disagreed; nested lines do. `line_render::revealed_rows_of_str` (moved out of `coord`) is now the one measure for the patch, the click and the cursor's sub-row. The revealed click battery includes a line (`- ab abcdefghijk` at 12 cells) that fails under the flat wrap.
- **The cursor's line must be in the stack.** A blank between an item's paragraphs renders no row and shares the next paragraph's row (`row_for_line`), which would have stacked that paragraph around a cursor outside it. `cursor_stacked_row` requires the cursor's line in the stack, so such a cursor reveals its own line in place, as before.
- **The reveal timer tracks paragraphs, not just blocks.** The latch and the "entering off the first line reveals at once" rule were per block, so moving up from one item's paragraph into another's, inside one list, sat on the collapsed flow and then dropped. `EditorState::cursor_stacked_unit` (block, first line) makes each reflowed paragraph a unit: entering or leaving one drops the latch, and entering off its first line reveals at once.
- **Three existing tests now run with reflow off**, keeping their subject (a row per source line in a list or quote): `emphasis_across_a_break_keeps_the_numbers_below_it`, `a_marker_line_opening_a_block_keeps_the_numbers_below_it` (`state_source_lines`) and `rendered_view_selection_on_a_nested_items_continuation_covers_it` (`tests/ui.rs`). With reflow on, their sources reflow.
- **Footnote wrap indent, not changed.** A footnote's flow wraps under `compute_hanging_indent`, which reads the leader `  1.  ` as an ordered marker and hangs one cell short of the text, and a non-numeric label not at all. That was already so for a long single line; nested reflow makes long footnote flows common. Since fixed ([#71](https://github.com/mijowi/edamame/issues/71)): the renderer states every row's indent (`RowOrigin::hang`) and `compute_hanging_indent` is gone; see `editing-model.md`.
- **Review follow-ups (2026-10-07).** `raw_text::cursor_block_pos` is now the one derivation of the cursor's block position: `raw_block_cursor`, `cursor_rendered_line_idx`, `cursor_raw_line` and `cursor_stacked_row` all read it. It counts lines at `\n` like `raw_source_lines` and clamps past the block's end. The rope-line version Phase 7 first added did neither, so it could disagree with `raw_block_cursor`. None of those callers copies the whole document any more: they used to, through `raw_block_cursor`, every frame. `cursor_stacked_row` is memoized per `(parsed_version, cursor offset)`, because `effective_rows` needs it before its own memo check and it walks the block's rows and a list's items. The cursor's sub-row on a row revealed in place now wraps as the painter does (`cursor_row_shows_raw`), as the stacked rows already did. Tests: `a_cursor_row_revealed_in_place_wraps_as_the_painter_does` and `cursor_block_pos_counts_lines_as_the_block_source_splits_them` (unit), and `clicking_around_a_revealed_row_keeps_the_cursor_where_it_shows`, the revealed click-and-paint over the corpus with the cursor mid-line on every non-blank line. That last test found a table bug outside row provenance and skips the affected entry at 12 cells (`KNOWN_REVEALED_MISSES`): a quoted table too wide for 12 cells wraps its rows, and a click on the revealed cell's last column lands one char left ([#69](https://github.com/mijowi/edamame/issues/69); top-level tables too narrow for the viewport do the same).
- **Tests:** in `tests/row_provenance.rs`, eight nested-reflow sources in the corpus (agreement, round trip and the unrevealed click-and-paint at two widths run over them) and `clicking_a_revealed_nested_flow_keeps_the_cursor_where_it_shows`: the click-and-paint round trip with the cursor's paragraph revealed and stacked, at 40 and 12 cells, which also requires a click on a stacked char to land on that char. In `tests/ui.rs`, the reveal shape above. Unit tests: `reflow_joins_nested_paragraphs_into_one_flow_each` (renderer), `a_nested_reveal_splices_one_row_inside_its_block` (`EffectiveRows`, against a brute force measuring with the painter's own `visual_rows_for_line`), `click_in_revealed_nested_flow_maps_to_its_stacked_raw_line` (`coord`), `a_nested_reflowed_paragraph_numbers_its_lines` and `a_quoted_reflowed_paragraph_numbers_its_lines` (gutter, collapsed and stacked), `entering_a_nested_reflowed_paragraph_from_below_reveals_immediately` and `leaving_a_nested_reflowed_paragraph_drops_its_latch` (timer). Both proptests already run reflow on; a stress run passed 4,000 agreement and 800 round-trip cases.
- **Second review (2026-10-07).** Footnote reveals gained tests: two footnote cases (the first paragraph stacking from its `[^n]:` line, and the second) in the `tests/ui.rs` reveal shape test, and `clicking_a_revealed_footnote_flow_keeps_the_cursor_where_it_shows`. To allow it, the click-and-paint helper skips a click that follows a footnote, as it skips one that edits: a click on the raw `[^n]:` of a stacked line follows the back-link, as it did on any revealed definition line before. That also let both corpus click tests drop their footnote filter. The revealed one then found a bug older than this plan: a line exactly as wide as the viewport painted no end-of-line cursor (no blank cell was left for it), revealed or in Raw mode. `line_render::paint_row` now draws that cursor over the last char (settled 2026-10-07, over an extra wrap row that would change every row count). A click on that cell lands on the char, not past it, so one footnote entry stays in `KNOWN_REVEALED_MISSES` at 12 cells. `stacked_lines` matches `Flow` origins only: a joined paragraph row is always `Flow`, one-line paragraphs included, so the `Inline` arm was dead. `StackedRow` became a struct.

### Phase 8 — docs (S, alongside each phase) — done

**Final benchmarks (2026-10-07).** Every phase, against `6cf3e85`, measured as before (user-space instructions on a pinned P-core) but each count the median of five runs over enough iterations to amortize setup: a process's count lands in one of two modes ~1.3% apart, so the noise floor is ~±1.5%, not the 0.001% claimed above. At 20k, parse +8–17% (`code` +48% of a tiny parse), render within noise except `lists` (+23%), cold open +0.2–10.7%, and each memoized edit +0.4–10.6%. Phase 7 alone is within ±1.6% everywhere. Wall-clock, `lists` at 5k crosses from fine into the marginal edit band (7.94 → 9.12 ms); nothing new exceeds the 16 ms budget. Phases 1–3's per-edit figures above measured higher (`code` +13.2% against +5.3% here) — that earlier run's noise, or work Phases 4–6 removed. **Accepted**: a fixed share of the parse that doesn't grow with size, for one source of truth; only incremental reparsing removes it. Tables and the method: [`performance.md`](../performance.md#cost-of-row-provenance), whose Linux section is re-measured with these changes in.

Done for Phase 7: `editing-model.md` (the reflow bullets: the gate, the splice, whole-line stacks, the painter's wrap, per-paragraph latching), the user-facing `docs/editing.md` (reflow reaches nested paragraphs; the reveal shows the source lines), the `config.toml` reflow note, the AGENTS.md project structure (`stacked_lines`, `revealed_rows_of_str`), the CHANGELOG (the list-item bullet rewritten for reflow on), and `nested-reflow.md` deleted. Benchmarks: see the final figures above.

Done for Phase 6: `editing-model.md` (the band and `revealed_diagram_line`).

Done for Phase 5: `tables.md`, `input.md`, `editing-model.md` (the mid-migration note), and the CHANGELOG (nested tables, border clicks).

Done for Phase 4: `editing-model.md` (the column bullet, the code-row and `Verbatim` reveal bullets replacing the `code_layout` ones), `frontmatter.md`, `input.md`, `search-replace.md`, the AGENTS.md project structure and layer list, and `nested-reflow.md` marked superseded except for Phase 7's consumer work. Benchmarks were not re-run for Phase 4.

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
2. **Bare `>` lines.** **Decision:** render one quoted blank row per bare `>` line, as the discarded patch did, instead of one blank between every pair of children. It's a fidelity fix the spans make free. A bare `>` between a loose list's items lies inside the list's span, so a list directly inside a quote keeps its loose spacing to give that line its row (settled 2026-10-06). Test: `a_blockquote_renders_one_row_per_source_line`.
