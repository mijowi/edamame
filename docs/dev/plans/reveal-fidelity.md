# Reveal fidelity — follow-ups from the row-provenance smoke test

Status: **IN PROGRESS** — steps 1 and B done (2026-10-08). Fixes found by testing `tests/fixtures/row_provenance.md` after [`row-provenance.md`](row-provenance.md) landed. Table editing inside a quote is out of scope: it's tracked by #74.

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

### C. Split rows at every break (M)

- **Normalize before splitting.** Before `paragraph_rows`, split the inline tree at every soft or hard break, at any depth. A container cut by a break becomes two nodes of the same kind (`Italic([a])`, break, `Italic([b])`; a link keeps its URL on both halves). Then `paragraph_rows` splits only at top-level breaks, and `nested_breaks` is deleted. Allocate only when a container actually holds a break, since most paragraphs don't.
- **Reflow gate.** `paragraph_can_reflow` looks for hard breaks at any depth, and `row_map::stacked_lines` reads the same function, so the two can't disagree.
- **Columns.** A split row's own line slice no longer parses as it rendered (`delta *echo foxtrot` reads the `*` as literal). `row_map` builds such a row's map from one `InlineColMap` over the leaf's lines joined by `\n`, then takes the part on the row's line. That's the existing `Flow` fallback, applied to the paragraph rather than the row. The round-trip proptest, which requires an exact map on every inline row, is the check; add emphasis, links and highlights spanning a break to the generator.
- Rendering change: with reflow off, rows follow the source lines, so the CHANGELOG gets an entry. Update `emphasis_across_a_break_keeps_the_numbers_below_it` to the new row count.
- Tests: reflow off, `a *b⏎c* d` renders 2 rows; `*a\⏎b*` renders 2 rows, reflow on or off.

### A. Reveal every line a row stands for (M)

- **Stack any multi-line row.** `stacked_lines` answers for any `Inline` / `Flow` content row whose origin covers more than one line, reflow on or off. A reflowable paragraph is then just one case. `EffectiveRows`, the click, the view and the timer already go through it.
- **Include the cursor's own line.** When the cursor's line owns no row and shares a neighbor's (`row_for_line`), the stack is the union of that row's lines and the cursor's line. That union is contiguous, because a line with no row always borders the row it shares. This replaces Phase 7's "the cursor's line must be in the stack, else reveal in place": in-place reveal was the very behavior that painted over the shared row. After B, a blank inside an item has its own row, so the cases left are a hidden link definition, a nested setext underline and a code-span continuation.
- **Setext.** A multi-line setext heading's text row now stacks, and its rule row keeps revealing as the underline. Fold `RenderedView`'s setext arm into the stack path if the two overlap; otherwise keep the arm for the underline only.
- **Latch.** A stacked row latches per unit, as a reflowed paragraph does (`cursor_stacked_unit` keyed on the row's first line).
- Tests:
  - `tests/ui.rs`: with the cursor on each line of `Multi⏎line⏎---`, both text lines show; with the cursor on a quote's hidden definition line, the next line still shows.
  - `tests/row_provenance.rs`: extend the revealed click-and-paint corpus with those sources plus `- a⏎⏎  ```⏎  x⏎  ```⏎`.
  - Assert in the agreement test that every line of a block's span lies in some row's `lines` or is one of the known row-less kinds, so a new row-less case fails loudly.

### 8. A drag never changes the reveal mid-drag (S)

- Rule: once the pointer moves, the reveal is off until mouse-up, whichever line the press was on. A press on the cursor's line still leaves `drag_in_progress` unset, so a plain click doesn't flash. The first `Drag` event that moves the cursor sets it.
- Trade-off: a drag that starts on a revealed row collapses that row once, at the start, before the pointer has gone far. Revealing as the drag goes would move text under a stationary pointer on every row, and stacked paragraphs would change height. Keyboard selection (vim `v`, Shift+arrows) keeps its live reveal, which is where seeing the Markdown being selected is reliable.
- After mouse-up, the cursor's line reveals after the usual `RAW_REVEAL_DELAY`; check that this already holds.
- Tests (`tests/mouse.rs`): a press on the cursor's line followed by a drag onto another line leaves `cursor_block_revealed()` false; a press-release on the cursor's line keeps it true.

## Docs

`editing-model.md` (the stacked reveal's new gate; the cursor-line union replacing Phase 7's in-place rule), `blockquotes.md` (B's any-depth loose spacing), `input.md` (the drag rule), `docs/editing.md` (B's and C's rendering changes), and the CHANGELOG.
