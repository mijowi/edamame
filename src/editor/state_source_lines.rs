//! Rendered-row → source-line mapping for the line-number gutter.
//!
//! Preview and Rendered paint the *renderer's* output rows, whose count
//! diverges from the buffer's line count as soon as a block renders taller
//! or shorter than its source (a GFM table renders roughly two rows per data
//! row, an image block reserves N rows from a single `![…](…)` line).  Every
//! other surface that speaks in line numbers — vim's `{count}G` / `:{N}`, the
//! Raw gutter, the cursor read-out — counts *buffer* lines, so the gutter has
//! to translate rather than number its own rows.
//!
//! The translation is the inverse of [`sub_lines_in_block`], which is the
//! crate's single source-line → rendered-sub-row implementation.  It is built
//! by *calling* that function for every block rather than by hand-writing an
//! inverse: a second derivation of the same relation would drift from it
//! exactly the way the pre-`raw_block_cursor` duplicate walks did, and the
//! failure would be silent (numbers one row off inside tables and lists).
//!
//! **Last writer wins**, because a source line that renders no row of its own
//! — an interior blank inside a list item — maps to the same sub-row as the
//! *next* line, the one whose text that row actually shows.  Labelling the row
//! with the earlier line would print a number beside text that belongs to a
//! different one, which is worse than printing nothing: the gutter's whole
//! contract is that the number beside a line of text is that line's source
//! number, identically in Raw and Rendered.  What the contract cannot promise
//! is that every number `1..N` appears — a line with no rendered row (a hidden
//! block-level HTML comment, the swallowed blank above) has nowhere to print
//! one, exactly as a wrapped continuation row does.  Numbers are *omitted*,
//! never reassigned.  Rows that are pure rendering artifacts (table borders,
//! image reserve rows) stay blank for the same reason.
//!
//! The whole walk reads the document out of [`ParsedDoc::source`], never out
//! of the live `Buffer`: it resolves *parse-time* byte ranges, and a deferred
//! in-line edit leaves the buffer ahead of the parse, so the two are different
//! coordinate spaces whenever `parsed_dirty` is set.  The table is memoised
//! per parse, so a build that happened to land in that window would mislabel
//! the document until the next re-parse rather than for a single frame.

use crate::document::ParsedDoc;
use crate::editor::state::sub_lines_in_block;
use crate::editor::EditorState;
use crate::ui::rendered_view::raw_source_lines;

#[cfg(test)]
thread_local! {
    /// Rebuild counter for the cache-key tests below.  Thread-local because
    /// cargo runs the crate's tests on parallel threads and each test wants
    /// only its own builds.
    static BUILD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl EditorState {
    /// Buffer line index to label document visual row `visual_row` with, or
    /// `None` when that row carries no number (a wrapped continuation row, a
    /// rendering artifact, or a row past the end of the document).
    ///
    /// The Preview / Rendered counterpart of `raw_line_at_visual_row`, which
    /// needs no translation because Raw paints buffer lines directly.
    pub fn source_line_at_visual_row(&self, visual_row: usize, width: usize) -> Option<usize> {
        let (rendered_idx, sub_row) = self.parsed.line_at_visual_row(visual_row, width);
        if sub_row != 0 {
            return None;
        }
        // Cached on the `ParsedDoc`, so the full-document walk below runs
        // once per parse: a deferred in-line edit bumps the buffer version
        // without moving a single line, and rebuilding per keystroke is what
        // a version-keyed cache would do.
        let parsed = &self.parsed;
        parsed
            .source_lines_or_init(|| build_source_line_map(parsed))
            .get(rendered_idx)
            .copied()
            .flatten()
    }
}

/// Walk every block, asking [`sub_lines_in_block`] where each of its raw
/// lines lands among the block's rendered rows.
///
/// Reads the document out of [`ParsedDoc::source`] — never out of the live
/// `Buffer`.  The two disagree whenever a deferred in-line edit is
/// outstanding, and this walk resolves *parse-time* byte ranges, so mixing
/// in the buffer would slice each block's text at an offset shifted by the
/// pending edit and count its first line against a document that has moved
/// underneath it.  Both defects are silent and both mislabel whole blocks.
fn build_source_line_map(parsed: &ParsedDoc) -> Vec<Option<usize>> {
    #[cfg(test)]
    BUILD_COUNT.with(|c| c.set(c.get() + 1));

    let mut map: Vec<Option<usize>> = vec![None; parsed.lines.len()];
    if map.is_empty() {
        return map;
    }
    let contents = parsed.source();
    // Blocks come in ascending byte order, so the line each one starts on is
    // a running count of the newlines behind it — `ParsedDoc::byte_to_line`
    // per block would rescan the whole prefix each time and make the walk
    // quadratic in the document's length.
    let mut scanned = 0usize;
    let mut block_line = 0usize;

    for block_idx in 0..parsed.source_map.block_count() {
        let Some(range) = parsed.source_map.original_range_for_block(block_idx) else {
            continue;
        };
        let start = range.start.min(contents.len());
        // Advance the running line count to this block's start.  Done before
        // any `continue` below so a skipped block still contributes the lines
        // it spans — the counter only stays honest while it sees every byte.
        if start > scanned {
            block_line += contents.as_bytes()[scanned..start]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
            scanned = start;
        }

        // Blocks that render nothing at all: a blank-line virtual block whose
        // row was collapsed away (`preserve_blank_lines` off), or a hidden
        // block-level HTML comment.  They must be skipped by their *own* row
        // count, not by an empty rendered range — `rendered_lines_for_block`
        // hands a row-less block its *neighbour's* range as a fallback, so a
        // builder that trusted that range would label a row belonging to some
        // other block with this block's invisible line.  Whether that lands
        // before or after the neighbour's own write (and so whether it wins
        // under last-writer) depends on which side the fallback came from, so
        // the skip is the guard, not the write order.
        let own = parsed.block_own_line_count(block_idx);
        if own == 0 {
            continue;
        }
        let rendered = parsed.source_map.rendered_lines_for_block(block_idx);
        let source = contents
            .get(start..range.end.min(contents.len()))
            .unwrap_or("");
        let raw_lines = raw_source_lines(source);

        // pulldown-cmark's block ranges absorb the blank lines that follow a
        // block, but `build_with_overrides` has already given each of those
        // its own virtual block — so labelling them here would print the same
        // number twice (once on the block's last rendered row, where the
        // clamp in `sub_lines_in_block` lands them, and again on the blank's
        // own row).  Stop at the block's last line with content.
        let last_content = raw_lines
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .unwrap_or(0);

        // One call per block, not one per line: see `sub_lines_in_block`.
        let subs = sub_lines_in_block(parsed, start, block_idx, own, source, &raw_lines);
        // Last writer wins (see the module doc): a line that renders no row
        // shares its sub-row with the *following* line, and it is that
        // following line's text the row displays.  Overwriting rather than
        // `get_or_insert`ing is what keeps the number beside the text it
        // belongs to; the swallowed line simply goes unnumbered.
        for (raw_line, &sub) in subs.iter().enumerate().take(last_content + 1) {
            if let Some(slot) = map.get_mut(rendered.start + sub) {
                *slot = Some(block_line + raw_line);
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

    /// Type `text` at the cursor the way `edit_ops::insert_text` does — the
    /// path whose `crosses_line` check decides whether the parse is deferred.
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

    /// The issue's reproduction: 6 source lines, 10 rendered rows.  `6G` must
    /// land on a row the gutter labels "6" (index 5), not on row 10.
    #[test]
    fn table_rows_map_back_to_their_source_lines() {
        let source = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\nafter\n";
        let state = state_for(source, 80);
        let labels = labels(&state, 80);
        // Every source line appears exactly once, in ascending order.
        let numbered: Vec<usize> = labels.iter().flatten().copied().collect();
        assert_eq!(numbered, vec![0, 1, 2, 3, 4, 5]);
        // …and the borders in between carry no number.
        assert!(labels.len() > numbered.len());
    }

    /// A setext H2 owns two rendered rows — the title and the rule
    /// `ParsedDoc::build` appends — but only two source lines, and the rule
    /// row is the *underline*'s.  Every source line still gets exactly one
    /// number, in order.
    #[test]
    fn setext_heading_numbers_both_of_its_source_lines() {
        let state = state_for("Title\n-----\n\nbody\n", 80);
        let labels = labels(&state, 80);
        // Title, rule, blank, body, trailing blank — one row each, and the
        // rule row is the underline's own line, not a blank artifact.
        assert_eq!(labels, vec![Some(0), Some(1), Some(2), Some(3), Some(4)]);
    }

    /// A block-level HTML comment is hidden — it owns zero rendered rows, so
    /// it must claim none.  `rendered_lines_for_block` hands a row-less block
    /// its *neighbour's* range as a fallback, so a builder that trusted that
    /// range would label the following paragraph's row with the comment's
    /// line number and drop the paragraph's own.
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

    /// With `preserve_blank_lines` off, only the first blank of a run renders
    /// — the rest own no row.  Same fallback trap as the hidden comment: the
    /// suppressed blanks must not eat the following block's number.
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
        // The reserved rows below the image line carry no number.
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

    /// A source line that renders no row of its own shares a row with the
    /// *next* line — and it is that next line's text the row shows, so the
    /// next line's number is the one that belongs there.  Regression: with
    /// first-writer-wins the `continuation` row was numbered 4 (the swallowed
    /// blank) while line 5 went unnumbered anywhere, i.e. a number printed
    /// beside text from a different line.
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
        // The blank it collapsed into simply goes unnumbered — omitted, never
        // reassigned to some other row.
        assert!(
            !labels.iter().flatten().any(|&l| l == 3),
            "the swallowed blank must not be numbered elsewhere: {labels:?}"
        );
    }

    /// The invariants the gutter exists to keep, swept over every block kind
    /// the sample fixture exercises (headings, setext, tables, lists, task
    /// lists, code blocks, a Mermaid diagram, images, footnotes, a hidden HTML
    /// comment) at both a wrapping and a non-wrapping width:
    ///
    /// 1. every label is a real buffer line, and no line is labelled twice;
    /// 2. labels ascend down the document — numbers never run backwards;
    /// 3. a labelled row is the row a line-addressed jump (`{count}G`, `:{N}`)
    ///    parks the cursor on, so the gutter and the cursor agree.
    ///
    /// Deliberately checked against `cursor_visual_row` — *visual* rows, the
    /// same space the gutter paints in.  `cursor_rendered_line_idx` counts
    /// rendered lines, which coincide with visual rows only when nothing
    /// wraps; comparing the two silently passes at width 80 and reports
    /// nonsense at any width where a line wraps.
    #[test]
    fn fixture_labels_are_unique_ascending_and_agree_with_the_cursor() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sample.md"
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

    /// The table costs a full document walk, so it must not be rebuilt for an
    /// edit that cannot have moved a line: typing inside a line defers the
    /// re-parse, leaving both the rendered rows and every block's first
    /// source line exactly where they were.  This is what living on
    /// `ParsedDoc` buys — a cache keyed on `EditorState::parsed_version`,
    /// which the deferred path bumps, rebuilt on every keystroke.
    #[test]
    fn typing_within_a_line_does_not_rebuild_the_table() {
        let mut state = state_for("alpha\n\nbravo\n", 80);
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

    /// …but a newline reflows blocks, so it must.  `apply_delta` re-parses
    /// eagerly for any edit containing `\n`, and the fresh `ParsedDoc` brings
    /// an empty cell with it.
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

    /// The table may be built while a deferred in-line edit has left the
    /// buffer ahead of the parse — the run loop draws at most once per 16 ms,
    /// so a newline (which re-parses eagerly) and the character typed behind
    /// it inside that window reach the gutter as one frame, with no draw in
    /// between to build the table while the two agreed.
    ///
    /// Regression: the walk read block text and block start lines out of the
    /// live `Buffer` using this parse's byte ranges, so every block after the
    /// edit was labelled against a document shifted by the pending insert.
    /// Labels came back `[0, 1, 1, 3, 3, 5, 5]` — duplicated, and printed
    /// beside another line's text.  Cached on the `ParsedDoc`, that survived
    /// until the next re-parse rather than for one frame.
    #[test]
    fn a_deferred_edit_does_not_shift_the_labels() {
        let mut state = state_for("alpha\n\nbravo\n\ncharlie\n", 80);
        // Eager re-parse (the edit crosses a line), which mints a fresh —
        // and empty — cache cell.
        state.cursor.offset = 0;
        type_text(&mut state, "\n");
        // Then an in-line edit *before* every later block, deferring the
        // re-parse and shifting the whole document out from under the byte
        // ranges the fresh parse recorded.
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
