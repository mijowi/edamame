# Reveal fidelity — follow-ups from the row-provenance smoke test

Status: **IN PROGRESS** — steps 1, B, C and A done (2026-10-08); 8 remains. Fixes found by testing `tests/fixtures/row_provenance.md` after [`row-provenance.md`](row-provenance.md) landed. Table editing inside a quote is out of scope: it's tracked by #74.

## Problem

Five defects, three of them related:

| # | Symptom (fixture line) | Cause |
|---|---|---|
| 1 | The quote's wash appears on the blank line after a quote when the cursor is on it (32) | `RenderedView` picks `reveal_base` from `parsed.real_ranges`, which absorb trailing blank lines |
| B | An item's second paragraph renders at 4 cells, not under the item's text, and the blank between its paragraphs renders nothing (76–81) | Later blocks in an item take the nested-list indent `max(4, marker_width)`; list items don't emit rows for blank lines between their children |
| C | With reflow off, `delta *echo⏎golf* hotel` renders as one row (45–46); a hard break inside emphasis (`*a\⏎b*`) renders as a space, reflow on or off | `paragraph_rows` splits only at top-level breaks; `paragraph_can_reflow` checks only top-level hard breaks |
| A | The cursor's reveal hides source: on a blank line with no row it paints over the next row (79, 156); on a multi-line row that isn't a reflowed paragraph it shows only the cursor's line (174–175, 45–46 with reflow off) | The in-place reveal assumes one row ↔ one line; only reflowed paragraphs stack (`row_map::stacked_lines`) |
| 8 | A mouse drag reveals rows as it goes if it started on the cursor's line, and reveals nothing otherwise | `mouse_ops` skips `drag_in_progress` for a press on the cursor's line (to avoid a flash), so that drag runs with the reveal live |

B and C each create rows that A then mishandles. Fixing them first shrinks A to its general cases: a multi-line setext heading, a multi-line code span's continuation, a hidden link definition inside a container, a nested setext underline.

## Steps

Each step lands on its own with the suite green, in this order.

### 1. Wash from the cursor's real block (S) — done

`ParsedDoc::real_block_for_byte` now answers `None` past a block's content and its last line's `\n`, which is how `build` splits the absorbed blank lines into virtual blocks. Before, it matched its doc comment ("`None` on a blank line") only for blanks no range absorbed. `RenderedView`'s `cursor_block_ast` calls it instead of scanning `real_ranges`, which fixes `reveal_base` and `is_setext`. The same fix reaches `edit_ops`' image-insert checks: on the blank after an indented code block or an HTML block (whose ranges absorb it) an image was refused. Tests: `real_block_for_byte_is_none_on_a_blank_line_a_range_absorbs` (unit), `a_blank_line_after_a_quote_reveals_without_its_wash` (`tests/ui.rs`), `can_insert_image_reference_on_the_blank_line_after_a_block_that_absorbs_it` (`tests/editing.rs`).

### B. List items: text-column indent and blank rows (S–M) — done

The parser records each `ListItem`'s and `FootnoteDefinition`'s `hidden` lines (`parser::hidden_lines`, shared with the quote: uncovered, non-blank lines between two children, i.e. link reference definitions), and the renderer emits one blank `Chrome` row per other line between two children (`renderer::render_children`, shared by the quote, the footnote definition and the list item). Footnote definitions did drop those blanks too. A loose list keeps its spacing at any depth: a nested one rendered tight, so the blank between its items would have been the one blank line in a container left without a row. An item's later non-list blocks take its text column (marker plus task box); nested lists keep `max(4, marker_width)`. Only paragraphs and raw HTML honor an indent at all — code blocks, quotes and tables render flush — so the alignment shows on those. Nine existing tests pinned the missing row or the 4-cell indent and were updated; new: `an_items_later_blocks_sit_under_its_text_behind_a_row_per_blank_line` (renderer), `a_blank_between_an_items_blocks_has_a_row_of_its_own` (`row_map`, whose row-less example became a quoted link definition).

### C. Split rows at every break (M) — done

`renderer::split_at_breaks` lifts every soft or hard break to the top level, cutting a container in two around it (a link keeps its URL and title on both halves; an empty half is dropped), and borrows when no container holds a break. `paragraph_row_inlines` applies it to a paragraph that doesn't join, and both `paragraph_rows` callers (`render_paragraph` and a list item's first paragraph) go through it; `nested_breaks` is deleted. `paragraph_reflows(reflow, inlines)` (formerly the `Renderer` method plus `paragraph_can_reflow`) finds hard breaks at any depth, and the renderer, `link_view` and `row_map::stacked_lines` all call it. Columns: `InlineColMap` now records which rendered chars are breaks (`is_break`), and when a paragraph row's own lines don't map, `row_map::JoinedMap` maps the whole paragraph joined by `\n`, once per parse (`ParsedDoc::with_joined_map`), and each row takes the chars between the breaks around its lines (`JoinedMap::part`). Other leaves keep the old row-only join. The generator gained strikethrough, a hard break inside emphasis, and a `==` pair across a break. Tests: `a_break_inside_an_inline_splits_the_row`, `a_split_inline_keeps_its_style_on_both_rows` (`tests/renderer.rs`), `a_row_cut_from_an_inline_maps_through_its_paragraph` (`row_map`), `a_link_split_across_rows_pairs_both_halves` (`link_view`); `emphasis_across_a_break_keeps_the_numbers_below_it` now expects three numbered rows.

Deviations:
- **Highlights can't span a break.** The parser only finds `==…==` inside one text run (`parser.rs`, `Inline::Highlight(vec![Inline::Text(..)])`), so the generator's `==x⏎y==` case tests a literal pair, and strikethrough stands in as the third container.
- **Link hit-testing.** `link_view` and `mouse_ops::links` pair the Nth link-styled run on a block's rows with its Nth AST link, so a link cut in two (two runs) shifted every later link's URL. `collect_link_runs_from_block` now takes `reflow` and counts a non-joining paragraph's split halves. Reflowed, the same pairing was already broken: a soft break inside a link rendered as an unstyled space, giving two runs for one link. Breaks now render in their container's style. No snapshot changed.
- **The agreement oracle** (`tests/row_provenance.rs::check_doc`) checked a row against its own lines only, and now also accepts a paragraph row that matches its part of the whole paragraph (`check_paragraph_part`). That's an independent reimplementation; it doesn't call `is_break`.
- **The paragraph map is memoized.** Built per row, it cost 102 ms over 50 rows of a 500-line reflow-off paragraph whose emphasis spans every break, and 766 ms at 5000 lines (release build). Shared per paragraph: 2 ms and 29 ms, the latter about one parse of the document.
- **CHANGELOG cap.** `[Unreleased]` was near the 4000-byte notes cap (`the_unreleased_section_fits_under_the_notes_caps`), so C's entry is a single 222-byte line. About 50 bytes are left.

### A. Reveal every line a row stands for (M) — done

- **Stack any multi-line row.** `stacked_lines` answers for any `Inline` / `Flow` content row whose origin covers more than one line, reflow on or off. A reflowable paragraph is then just one case. `EffectiveRows`, the click, the view and the timer already go through it.
- **Include the cursor's own line.** When the cursor's line owns no row and shares a neighbor's (`row_for_line`), the stack is the union of that row's lines and the cursor's line. That union is contiguous, because a line with no row always borders the row it shares. This replaces Phase 7's "the cursor's line must be in the stack, else reveal in place": in-place reveal was the very behavior that painted over the shared row. After B, a blank inside an item has its own row, so the cases left are a hidden link definition, a nested setext underline and a code-span continuation.
- **Setext.** A multi-line setext heading's text row now stacks, and its rule row keeps revealing as the underline. Fold `RenderedView`'s setext arm into the stack path if the two overlap; otherwise keep the arm for the underline only.
- **Latch.** A stacked row latches per unit, as a reflowed paragraph does (`cursor_stacked_unit` keyed on the row's first line; see the deviation below).
- Tests:
  - `tests/ui.rs`: with the cursor on each line of `Multi⏎line⏎---`, both text lines show; with the cursor on a quote's hidden definition line, the next line still shows.
  - `tests/row_provenance.rs`: extend the revealed click-and-paint corpus with those sources plus `- a⏎⏎  ```⏎  x⏎  ```⏎`.
  - Assert in the agreement test that every line of a block's span lies in some row's `lines` or is one of the known row-less kinds, so a new row-less case fails loudly.

Done as planned: `row_map::stacked_lines` answers for any `Inline`/`Flow` row over several lines, reflow on or off, and a new `row_map::cursor_stack` builds the cursor's stack, which `EditorState::cursor_stacked_row` now calls with no reflow gate (nor `anchor_reflow_reveal`, renamed `anchor_stacked_reveal`). The setext arm in `RenderedView` kept only rule rows and one-line text rows. Tests: `multi_line_setext_heading_reveals_every_text_line` (replacing `multi_line_setext_heading_reveals_the_cursors_line`, which pinned the old behavior) and `a_quotes_hidden_definition_reveals_with_the_line_below` (`tests/ui.rs`); `a_bare_markers_line_reveals_with_the_row_it_shares` (`state_source_lines`, which took the bare-marker case out of `a_marker_line_opening_a_block_keeps_the_numbers_below_it`); four corpus entries (`Multi⏎line⏎---`, the quoted definition, `- a⏎  b⏎  ---`, the item's code block); and `renders_no_row` in the agreement check.

Deviations:
- **A fourth row-less kind.** The agreement check found a bare list marker (`-` with the text on the next line) has no row of its own; `cursor_stack` already handles it, and `renders_no_row` lists it.
- **The latch keys on the row.** A row-less line's stack differs from the stack of the row it shares, so keying `cursor_stacked_unit` on the stack's first line dropped the latch, and flashed the row collapsed, on moving between them. It is `(block, row)` now, and the `EffectiveRows` memo key gained the stacked lines, which it had assumed fixed per row (`moving_between_a_hidden_line_and_the_row_it_shares_keeps_it_revealed`).
- **A marker row stacks the cursor's line alone.** A hidden definition sharing `- - a`'s marker row would have shown `- - a` twice, raw in the stack and rendered on the row below (`a_hidden_definition_over_a_marker_row_shows_no_line_twice`, plus a corpus entry).
- **Two exclusions.** `cursor_stack` never stacks a shared table row (it reveals cell by cell; a quoted table is #74) or a diagram.
- **Not covered:** a multi-line setext H1 drawn as big text. Its glyph rows are chrome, so it still reveals only its first line.
- **Fixed on the way:**
  - The `==x⏎y==` generator case from C exposed an older bug. `InlineColMap` parsed without `ENABLE_MATH`, so `y== $a$ ==x` read as one text run holding a highlight, where the parser splits it at the math. Math now maps as its delimited source (`math_splits_the_text_around_it`).
  - The click round trip held the reveal off with a running 120 ms delay, which the threaded runs from `644c5c0` could outlast mid-check. The delay is now started an hour out.

### 8. A drag never changes the reveal mid-drag (S)

- Rule: once the pointer moves, the reveal is off until mouse-up, whichever line the press was on. A press on the cursor's line still leaves `drag_in_progress` unset, so a plain click doesn't flash. The first `Drag` event that moves the cursor sets it.
- Trade-off: a drag that starts on a revealed row collapses that row once, at the start, before the pointer has gone far. Revealing as the drag goes would move text under a stationary pointer on every row, and stacked paragraphs would change height. Keyboard selection (vim `v`, Shift+arrows) keeps its live reveal, which is where seeing the Markdown being selected is reliable.
- After mouse-up, the cursor's line reveals after the usual `RAW_REVEAL_DELAY`; check that this already holds.
- Tests (`tests/mouse.rs`): a press on the cursor's line followed by a drag onto another line leaves `cursor_block_revealed()` false; a press-release on the cursor's line keeps it true.

## Docs

`editing-model.md` (the stacked reveal's new gate; the cursor-line union replacing Phase 7's in-place rule), `blockquotes.md` (B's any-depth loose spacing), `input.md` (the drag rule), `docs/editing.md` (B's and C's rendering changes), and the CHANGELOG.
