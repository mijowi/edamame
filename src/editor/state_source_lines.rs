//! Rendered-row → source-line mapping for the line-number gutter.  Rendered rows diverge
//! from buffer lines (tables, image reserves), while every other line-numbered surface
//! (`{count}G`, the Raw gutter) counts buffer lines, so the gutter translates.
//!
//! Invariants:
//! - The map is built by *calling* [`sub_lines_in_block`], the crate's single source-line →
//!   sub-row implementation, never by a hand-written inverse that could silently drift.
//! - **Last writer wins**: a line with no row of its own (an interior blank in a list item)
//!   shares its sub-row with the *next* line, whose text the row shows.  The number beside a
//!   row must be that text's line; numbers are omitted, never reassigned.  Artifact rows
//!   (table borders, image reserves, wrapped continuations) stay blank.
//! - The walk reads [`ParsedDoc::source`], never the live `Buffer`: it resolves parse-time
//!   byte ranges, and a deferred in-line edit leaves the buffer ahead of the parse.  The
//!   table is memoized per parse, so a mislabel would persist until the next re-parse.

use crate::document::ParsedDoc;
use crate::editor::effective_rows::EffectiveRows;
use crate::editor::state::sub_lines_in_block;
use crate::editor::EditorState;
use crate::ui::rendered_view::raw_source_lines;

#[cfg(test)]
thread_local! {
    /// Rebuild counter for the cache-key tests; thread-local because tests run in parallel.
    static BUILD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl EditorState {
    /// Buffer line to label visual row `visual_row` with, or `None` for an unnumbered row.
    /// Preview/Rendered counterpart of `raw_line_at_visual_row`.  Builds an [`EffectiveRows`] per
    /// call; the gutter, which asks per visible row, calls [`Self::source_line_at_visual_row_with`]
    /// with one built for the whole frame instead.
    pub fn source_line_at_visual_row(&self, visual_row: usize, width: usize) -> Option<usize> {
        self.source_line_at_visual_row_with(&self.effective_rows(width), visual_row)
    }

    /// [`Self::source_line_at_visual_row`] against a prebuilt [`EffectiveRows`], so a per-row
    /// caller doesn't reconstruct it (which allocates the revealed block's source) every row.
    pub(crate) fn source_line_at_visual_row_with(
        &self,
        effective: &EffectiveRows,
        visual_row: usize,
    ) -> Option<usize> {
        use crate::editor::effective_rows::RowHit;
        // Route through `EffectiveRows` so a revealed reflowed block's raw expansion is counted:
        // its rows are raw source lines, and rows below it shift.  Identity everywhere else, so
        // this equals the base `line_at_visual_row` outside that reveal.
        match effective.line_at_visual_row(visual_row) {
            RowHit::Raw { raw_line, sub } => {
                if sub != 0 {
                    return None; // a wrap continuation of a raw line carries no number
                }
                // Each revealed raw line is a source line: the block's first source line + offset.
                let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
                let block_start = self
                    .parsed
                    .source_map
                    .original_range_for_byte(cursor_byte)?
                    .start;
                Some(self.buffer.rope().byte_to_line(block_start) + raw_line)
            }
            RowHit::Rendered {
                line: rendered_idx,
                sub,
            } => {
                if sub != 0 {
                    return None;
                }
                // Cached on the `ParsedDoc`, not keyed on buffer version: an in-line edit bumps the
                // version without moving a line, and the walk is full-document.
                let parsed = &self.parsed;
                parsed
                    .source_lines_or_init(|| build_source_line_map(parsed))
                    .get(rendered_idx)
                    .copied()
                    .flatten()
            }
        }
    }
}

/// Walk every block, asking [`sub_lines_in_block`] where each raw line lands among its
/// rendered rows.  Reads [`ParsedDoc::source`] only (see the module doc).
fn build_source_line_map(parsed: &ParsedDoc) -> Vec<Option<usize>> {
    #[cfg(test)]
    BUILD_COUNT.with(|c| c.set(c.get() + 1));

    let mut map: Vec<Option<usize>> = vec![None; parsed.lines.len()];
    if map.is_empty() {
        return map;
    }
    let contents = parsed.source();
    // Running newline count instead of `byte_to_line` per block, which would be quadratic.
    let mut scanned = 0usize;
    let mut block_line = 0usize;

    for block_idx in 0..parsed.source_map.block_count() {
        let Some(range) = parsed.source_map.original_range_for_block(block_idx) else {
            continue;
        };
        let start = range.start.min(contents.len());
        // Before any `continue`: the counter is only honest while it sees every byte.
        if start > scanned {
            block_line += contents.as_bytes()[scanned..start]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
            scanned = start;
        }

        // Row-less blocks (collapsed blank, hidden HTML comment) must be skipped by their
        // *own* row count: `rendered_lines_for_block` hands such a block its neighbor's
        // range as a fallback, which would label another block's row with this one's line.
        let own = parsed.block_own_line_count(block_idx);
        if own == 0 {
            continue;
        }
        let rendered = parsed.source_map.rendered_lines_for_block(block_idx);
        let source = contents
            .get(start..range.end.min(contents.len()))
            .unwrap_or("");
        let raw_lines = raw_source_lines(source);

        // Trailing blanks absorbed into the block range already own virtual blocks of their
        // own; labeling them here too would print the same number twice.
        let last_content = raw_lines
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .unwrap_or(0);

        let subs = sub_lines_in_block(parsed, start, block_idx, own, source, &raw_lines);
        // Last writer wins (module doc): overwrite, never `get_or_insert` — *except* a reflowed
        // paragraph, whose several source lines all collapse onto one rendered flow row (`subs`
        // is all-zeros because `block_own == 1` caps them).  There the row shows the flow's first
        // character, so it must be labeled with the *first* source line — first writer wins.
        let reflowed = parsed.is_reflowed_paragraph_at(start);
        for (raw_line, &sub) in subs.iter().enumerate().take(last_content + 1) {
            if let Some(slot) = map.get_mut(rendered.start + sub) {
                if reflowed {
                    slot.get_or_insert(block_line + raw_line);
                } else {
                    *slot = Some(block_line + raw_line);
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use crate::config::Theme;
    use crate::document::Buffer;
    use crate::editor::EditorState;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn state_for(source: &str, width: usize) -> EditorState {
        let mut state = EditorState::new(Buffer::from_str(source), theme());
        state.mode = crate::editor::Mode::Rendered;
        state.set_viewport_width(width);
        state.refresh_parsed();
        state
    }

    /// How many times `f` rebuilt the memoised table.
    fn builds(f: impl FnOnce()) -> usize {
        let before = super::BUILD_COUNT.with(|c| c.get());
        f();
        super::BUILD_COUNT.with(|c| c.get()) - before
    }

    /// Type `text` the way `edit_ops::insert_text` does, so the deferred-parse path applies.
    fn type_text(state: &mut EditorState, text: &str) {
        state.apply_delta(crate::document::EditDelta {
            offset: state.cursor.offset,
            removed: String::new(),
            inserted: text.to_owned(),
        });
    }

    /// Every visual row's label, in order, for the whole document.
    fn labels(state: &EditorState, width: usize) -> Vec<Option<usize>> {
        (0..state.parsed.total_visual_rows(width))
            .map(|row| state.source_line_at_visual_row(row, width))
            .collect()
    }

    #[test]
    fn plain_paragraphs_number_one_to_one() {
        let state = state_for("alpha\n\nbravo\n", 80);
        assert_eq!(labels(&state, 80), vec![Some(0), Some(1), Some(2), Some(3)]);
    }

    #[test]
    fn table_rows_map_back_to_their_source_lines() {
        let source = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\nafter\n";
        let state = state_for(source, 80);
        let labels = labels(&state, 80);
        let numbered: Vec<usize> = labels.iter().flatten().copied().collect();
        assert_eq!(numbered, vec![0, 1, 2, 3, 4, 5]);
        assert!(labels.len() > numbered.len());
    }

    /// The rule row `ParsedDoc::build` appends is the underline's own line, not an artifact.
    #[test]
    fn setext_heading_numbers_both_of_its_source_lines() {
        let state = state_for("Title\n-----\n\nbody\n", 80);
        let labels = labels(&state, 80);
        assert_eq!(labels, vec![Some(0), Some(1), Some(2), Some(3), Some(4)]);
    }

    /// Guards the row-less-block skip in `build_source_line_map`.
    #[test]
    fn hidden_html_comment_claims_no_row() {
        let state = state_for("Alpha.\n\n<!-- hidden -->\n\nBeta.\n", 80);
        let labels = labels(&state, 80);
        let numbered: Vec<usize> = labels.iter().flatten().copied().collect();
        assert!(
            !numbered.contains(&2),
            "the comment's line must not be numbered: {labels:?}"
        );
        assert!(
            numbered.contains(&4),
            "`Beta.` must keep its own number: {labels:?}"
        );
        assert!(
            numbered.windows(2).all(|w| w[0] < w[1]),
            "numbers must stay ascending and unique: {labels:?}"
        );
    }

    /// Same trap as the hidden comment, for collapsed blank runs.
    #[test]
    fn suppressed_blank_lines_claim_no_row() {
        let theme = theme();
        let mut state = EditorState::new_with_config(
            Buffer::from_str("alpha\n\n\n\nbravo\n"),
            theme,
            false,
            true,
            24,
        );
        state.mode = crate::editor::Mode::Rendered;
        state.set_viewport_width(80);
        state.refresh_parsed();
        let labels = labels(&state, 80);
        let numbered: Vec<usize> = labels.iter().flatten().copied().collect();
        assert!(
            numbered.contains(&4),
            "`bravo` must keep its own number: {labels:?}"
        );
        assert!(
            numbered.windows(2).all(|w| w[0] < w[1]),
            "numbers must stay ascending and unique: {labels:?}"
        );
    }

    #[test]
    fn image_reserve_rows_are_blank_below_the_first() {
        let mut state = state_for("![alt](missing.png)\n\nafter\n", 80);
        state.image_max_height = 6;
        state.refresh_parsed();
        let labels = labels(&state, 80);
        assert_eq!(labels.first().copied().flatten(), Some(0));
        let numbered: Vec<usize> = labels.iter().flatten().copied().collect();
        assert_eq!(numbered, vec![0, 1, 2, 3]);
    }

    #[test]
    fn wrapped_continuation_rows_are_blank() {
        let source = "aaaa bbbb cccc dddd eeee ffff\n";
        let state = state_for(source, 12);
        let labels = labels(&state, 12);
        assert!(labels.len() > 2, "expected the line to wrap");
        assert_eq!(labels[0], Some(0));
        assert!(labels[1..labels.len() - 1].iter().all(|l| l.is_none()));
    }

    /// Regression for last-writer-wins: first-writer numbered the `continuation` row with
    /// the swallowed blank's line.
    #[test]
    fn a_swallowed_blank_does_not_steal_the_next_line_number() {
        let state = state_for("Intro.\n\n- item\n\n  continuation\n\nafter\n", 80);
        let labels = labels(&state, 80);
        let rendered: Vec<String> = state
            .parsed
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let row = rendered
            .iter()
            .position(|t| t.contains("continuation"))
            .expect("the continuation must render");
        assert_eq!(
            labels[row],
            Some(4),
            "row {row} shows `continuation` (line index 4): {labels:?}"
        );
        assert!(
            !labels.iter().flatten().any(|&l| l == 3),
            "the swallowed blank must not be numbered elsewhere: {labels:?}"
        );
    }

    /// A blank inside a code block nested in a list item renders a code row, unlike an
    /// interior blank in the item's prose; counting it as rowless shifted every number below it.
    #[test]
    #[ignore = "row-provenance: phase 3"]
    fn a_blank_inside_a_nested_code_block_keeps_its_number() {
        let source = "8. Tag it.\n\n    ```bash\n    gh run watch\n\n      indented\n    ```\n";
        let state = state_for(source, 80);
        let labels = labels(&state, 80);
        // Rows: item, ` bash ` label, body, blank body, body, closing fence.
        assert_eq!(
            labels[..6],
            [Some(0), Some(2), Some(3), Some(4), Some(5), Some(6)],
            "{labels:?}"
        );
    }

    /// An item whose first block starts on its marker line renders the marker on a row of its
    /// own, so that line spans two rows; counting it as one put every number below it a row
    /// high.  The extra row (the fence label, the nested item, the quote) stays unnumbered,
    /// and the cursor on each line lands on its numbered row.
    #[test]
    #[ignore = "row-provenance: phase 3"]
    fn a_marker_line_opening_a_block_keeps_the_numbers_below_it() {
        for (source, expected) in [
            (
                "- ```bash\n  code\n  ```\n- next item\n\n- third\n",
                // Rows: `•`, ` bash ` label, body, closing fence, item, separator, item.
                &[Some(0), None, Some(1), Some(2), Some(3), Some(4), Some(5)][..],
            ),
            (
                "- - a\n  - b\n- c\n",
                // Rows: `•`, nested `• a`, nested `• b`, `• c`.
                &[Some(0), None, Some(1), Some(2)][..],
            ),
            (
                "1. a\n   - ```\n     x\n     ```\n2. b\n",
                // Rows: `1. a`, nested `•`, empty label, body, closing fence, `2. b`.
                &[Some(0), Some(1), None, Some(2), Some(3), Some(4)][..],
            ),
            (
                // An unclosed fence, as while typing it, still renders a closing row.
                "- a\n  ```\n  x\n- b\n",
                // Rows: `• a`, empty label, body, closing placeholder, `• b`.
                &[Some(0), Some(1), Some(2), None, Some(3)][..],
            ),
            (
                // A bare marker shares its paragraph's row: `• text`.
                "-\n  text\n- b\n",
                &[Some(1), Some(2)][..],
            ),
            (
                "- a\n  soft\n- b\n",
                // Rows: `• a`, `  soft`, `• b`.
                &[Some(0), Some(1), Some(2)][..],
            ),
        ] {
            let mut state = state_for(source, 80);
            let labels = labels(&state, 80);
            assert_eq!(labels[..expected.len()], *expected, "{source:?}");
            for (row, line) in expected
                .iter()
                .enumerate()
                .filter_map(|(row, l)| l.map(|line| (row, line)))
            {
                state.cursor.offset = state.buffer.line_to_char(line);
                state.update_cursor_block();
                assert_eq!(
                    state.cursor_visual_row(80),
                    row,
                    "{source:?}: the cursor on line {line} must land on row {row}"
                );
            }
        }
    }

    /// A loose list inside a blockquote renders tight, so the bare `>` between its items renders
    /// no row; counting that line as a row put every number and cursor below it a row low.
    #[test]
    #[ignore = "row-provenance: phase 3"]
    fn a_loose_list_in_a_blockquote_keeps_the_numbers_below_it() {
        let source = "> - a\n>\n> - b\n>\n> tail\n";
        let mut state = state_for(source, 80);
        // Rows: `• a`, `• b`, the quoted blank, `tail`.
        let expected = [Some(0), Some(2), Some(3), Some(4)];
        let labels = labels(&state, 80);
        assert_eq!(labels[..expected.len()], expected, "{labels:?}");
        for (row, line) in [(0, 0), (1, 2), (2, 3), (3, 4)] {
            state.cursor.offset = state.buffer.line_to_char(line);
            state.update_cursor_block();
            assert_eq!(
                state.cursor_visual_row(80),
                row,
                "the cursor on line {line} must land on row {row}"
            );
        }
    }

    /// Sweeps the gutter invariants (unique, ascending, agrees with where `{count}G` parks
    /// the cursor) over every block kind in the sample fixture at a wrapping and a
    /// non-wrapping width.  Compares against `cursor_visual_row`, not
    /// `cursor_rendered_line_idx`, which only coincides with visual rows when nothing wraps.
    #[test]
    fn fixture_labels_are_unique_ascending_and_agree_with_the_cursor() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/general.md"
        ))
        .expect("the sample fixture must be readable");

        for width in [100, 60] {
            let mut state = state_for(&source, width);
            let labels = labels(&state, width);
            let numbered: Vec<usize> = labels.iter().flatten().copied().collect();

            assert!(
                numbered.iter().all(|&l| l < state.buffer.line_count()),
                "width {width}: a label named a line past the end of the buffer"
            );
            assert!(
                numbered.windows(2).all(|w| w[0] < w[1]),
                "width {width}: labels must ascend and be unique, got {numbered:?}"
            );

            for (row, line) in labels
                .iter()
                .enumerate()
                .filter_map(|(row, l)| l.map(|line| (row, line)))
            {
                state.cursor.offset = state.buffer.line_to_char(line);
                state.update_cursor_block();
                assert_eq!(
                    state.cursor_visual_row(width),
                    row,
                    "width {width}: the gutter labels row {row} with line {line}, \
                     but the cursor on that line lands elsewhere"
                );
            }
        }
    }

    /// A reflowed paragraph (Preview mode) collapses its source lines into one flow, so the
    /// gutter numbers only its first rendered row — with the flow's *first* source line — and
    /// leaves the rest folded.  Without the first-writer branch the row would show the paragraph's
    /// last source line.
    #[test]
    fn reflowed_paragraph_labels_its_first_source_line() {
        let mut state = EditorState::new(Buffer::from_str("one\ntwo\nthree\n\nafter\n"), theme());
        // Preview reflows by default; reconcile the parse the way the App does each frame.
        state.set_viewport_width(80);
        state.sync_reflow_for_mode();
        let labels = labels(&state, 80);
        assert_eq!(
            labels.first().copied().flatten(),
            Some(0),
            "the flow's row must be numbered with its first source line: {labels:?}"
        );
        let numbered: Vec<usize> = labels.iter().flatten().copied().collect();
        assert!(
            !numbered.contains(&1) && !numbered.contains(&2),
            "the folded soft-break lines must not be numbered: {labels:?}"
        );
        assert!(
            numbered.contains(&4),
            "`after` (line 4) must keep its own number: {labels:?}"
        );
    }

    /// A version-keyed cache would rebuild the full-document walk on every keystroke.
    #[test]
    fn typing_within_a_line_does_not_rebuild_the_table() {
        let mut state = state_for("alpha\n\nbravo\n", 80);
        // This exercises the memoized rendered→source table, which is orthogonal to reflow; with
        // reflow on, row 0 is a revealed raw line that never consults the table, so disable it to
        // keep the caching assertions about the table itself.
        state.set_reflow(false);
        let before = builds(|| {
            let _ = state.source_line_at_visual_row(0, 80);
        });
        assert_eq!(before, 1, "first query builds");

        state.cursor.offset = state.buffer.line_to_char(2);
        type_text(&mut state, "xyz");
        let rebuilds = builds(|| {
            let _ = state.source_line_at_visual_row(0, 80);
        });
        assert_eq!(rebuilds, 0, "an in-line edit must reuse the table");
        assert_eq!(labels(&state, 80), vec![Some(0), Some(1), Some(2), Some(3)]);
    }

    #[test]
    fn inserting_a_newline_rebuilds_the_table() {
        let mut state = state_for("alpha\n\nbravo\n", 80);
        let _ = state.source_line_at_visual_row(0, 80);
        state.cursor.offset = state.buffer.line_to_char(2);
        type_text(&mut state, "\n");
        let rebuilds = builds(|| {
            let _ = state.source_line_at_visual_row(0, 80);
        });
        assert_eq!(rebuilds, 1, "a re-parse must drop the table");
        assert_eq!(
            labels(&state, 80),
            vec![Some(0), Some(1), Some(2), Some(3), Some(4)]
        );
    }

    /// Regression: the walk once read block text out of the live `Buffer` with parse-time
    /// byte ranges, so a deferred edit ahead of the parse (a newline and a character typed
    /// within one 16 ms frame) produced `[0, 1, 1, 3, 3, 5, 5]`.
    #[test]
    fn a_deferred_edit_does_not_shift_the_labels() {
        let mut state = state_for("alpha\n\nbravo\n\ncharlie\n", 80);
        state.cursor.offset = 0;
        type_text(&mut state, "\n");
        state.cursor.offset = 0;
        type_text(&mut state, "zzz");
        assert_eq!(state.buffer.contents(), "zzz\nalpha\n\nbravo\n\ncharlie\n");

        assert_eq!(
            labels(&state, 80),
            vec![
                Some(0),
                Some(1),
                Some(2),
                Some(3),
                Some(4),
                Some(5),
                Some(6)
            ],
        );
    }

    #[test]
    fn rows_past_the_end_have_no_label() {
        let state = state_for("alpha\n", 80);
        let total = state.parsed.total_visual_rows(80);
        assert_eq!(state.source_line_at_visual_row(total, 80), None);
        assert_eq!(state.source_line_at_visual_row(total + 50, 80), None);
    }
}
