//! Raw ↔ rendered column geometry of code blocks.
//!
//! [`Renderer::render_code_block`](crate::markdown::Renderer) paints every
//! *body* row of a code block as `format!(" {:<width$}", line, …)` — the raw
//! source line behind exactly one leading pad cell, on a background filled to
//! the viewport edge.  Rendered column `c` therefore shows raw char `c - 1`.
//! An *indented* (non-fenced) block adds a second term: pulldown-cmark strips
//! the up-to-four-space (or single-tab) indent before the text reaches
//! `Block::CodeBlock::content`, so those chars have no rendered cell at all.
//!
//! Three consumers need that mapping and must never re-derive it — the
//! selection / search overlay painter (`ui::rendered_view::paint`), the cursor
//! indicator (`ui::rendered_view`), and the mouse click → offset mapping
//! (`editor::mouse_ops::coord`, which needs the inverse).  When they drift the
//! cursor paints beside its character and clicks land past the glyph the user
//! aimed at (issue #28).  It lives in the `markdown` layer, like
//! [`list_layout`](crate::markdown::list_layout) and
//! [`table_layout`](crate::markdown::table_layout), because it is a property of
//! the renderer's output format and both the `ui` and `editor` layers depend on
//! this one.
//!
//! *Fence* rows are deliberately outside the mapping: the opening row renders
//! the ` lang ` label (or an NBSP placeholder) and the closing row an NBSP
//! placeholder, so no column relation to their raw ``` ``` ``` text exists.
//! Ask [`is_code_fence_row`] and handle them separately — and note it asks
//! the *text* whether the last line closes the block, because an unclosed
//! fence ends on ordinary code that still needs the mapping.

use crate::markdown::Block;

/// Cells the renderer puts to the left of a code body line's first
/// character — the leading space in `format!(" {:<width$}", …)`.
pub const CODE_PAD_COLS: usize = 1;

/// Leading chars pulldown-cmark strips from an *indented* code block's raw
/// line before it becomes `Block::CodeBlock::content`: one tab, or up to four
/// spaces.  Always 0 for a fenced block, whose content is taken verbatim.
pub fn code_indent_strip_chars(raw_line: &str, fenced: bool) -> usize {
    if fenced {
        return 0;
    }
    if raw_line.starts_with('\t') {
        return 1;
    }
    raw_line.chars().take(4).take_while(|c| *c == ' ').count()
}

/// True when a line is a CommonMark *closing* fence: three or more of the
/// same delimiter (`` ` `` or `~`) and nothing else.  A closing fence may
/// not carry an info string, which is what makes the plain "all one
/// delimiter char" test exact rather than a heuristic.
fn is_closing_fence_line(raw_line: &str) -> bool {
    let trimmed = raw_line.trim();
    let Some(first) = trimmed.chars().next() else {
        return false;
    };
    (first == '`' || first == '~')
        && trimmed.chars().count() >= 3
        && trimmed.chars().all(|c| c == first)
}

/// True when raw line `raw_line_idx` is one of a fenced block's fence rows.
/// An indented block has no fences, so this is always false for it.
///
/// The opening fence is raw line 0 by construction.  The closing fence is
/// the last raw line **only when that line really is a fence delimiter** —
/// a fenced block left unclosed (which is every code block a user is
/// part-way through typing) ends on an ordinary body line, and calling that
/// a fence skips the pad-cell column shift for it: the cursor paints beside
/// its character, a click lands one char late, and a selection washes the
/// whole row instead of the columns it covers.  Deriving it from the text
/// also keeps this in step with `RenderedView`'s own `is_closing_fence_row`,
/// which has always tested the line's text.
///
/// `raw_lines` must come from
/// [`raw_source_lines`](crate::ui::rendered_view::raw_source_lines) (or an
/// equivalent split that drops the single trailing empty entry a trailing
/// newline produces) — a bare `split('\n')` appends a phantom line, and the
/// *real* closing fence is then not the last element.
pub fn is_code_fence_row(fenced: bool, raw_line_idx: usize, raw_lines: &[&str]) -> bool {
    if !fenced {
        return false;
    }
    if raw_line_idx == 0 {
        return true;
    }
    raw_line_idx + 1 == raw_lines.len()
        && raw_lines
            .get(raw_line_idx)
            .is_some_and(|line| is_closing_fence_line(line))
}

/// Raw char column on a code-block **body** line → rendered char column.
///
/// Columns inside the stripped indent collapse onto the first rendered
/// content cell, so a cursor or selection edge there still lands on text.
/// Callers must have excluded fence rows via [`is_code_fence_row`].
pub fn code_raw_col_to_rendered_col(raw_line: &str, fenced: bool, raw_col: usize) -> usize {
    raw_col.saturating_sub(code_indent_strip_chars(raw_line, fenced)) + CODE_PAD_COLS
}

/// The inverse of [`code_raw_col_to_rendered_col`], for the mouse hit-test.
///
/// A click on the pad cell maps to the line's first content char, and one in
/// the trailing background fill clamps to end-of-line rather than running off
/// into the next line's bytes.
pub fn code_rendered_col_to_raw_col(raw_line: &str, fenced: bool, rendered_col: usize) -> usize {
    let strip = code_indent_strip_chars(raw_line, fenced);
    (rendered_col.saturating_sub(CODE_PAD_COLS) + strip).min(raw_line.chars().count())
}

/// Whether `RenderedView` de-renders (reveals the raw source for) raw line
/// `raw_line_idx` of the block the cursor is in.
///
/// Every non-code block reveals its cursor line.  A fenced code block reveals
/// only its two fence rows — a body row already shows the same characters, so
/// de-rendering it would be visual churn — and an indented block reveals
/// nothing.  This is the single derivation of the rule: the mouse hit-test
/// must agree with the view about which rows show raw text, or a click is
/// mapped 1:1 against text that is not the text on screen.
///
/// `block` is the *post-processed* AST block, resolved via
/// [`ParsedDoc::real_block_for_byte`](crate::document::ParsedDoc::real_block_for_byte)
/// — never by indexing `parsed.blocks` with a source-map index.  `None`
/// (a blank-line virtual block) reveals, like any non-code block.
pub fn line_allows_raw_reveal(
    block: Option<&Block>,
    raw_line_idx: usize,
    raw_lines: &[&str],
) -> bool {
    match block {
        Some(Block::CodeBlock { fenced, .. }) => {
            is_code_fence_row(*fenced, raw_line_idx, raw_lines)
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenced_body_col_shifts_by_the_pad_only() {
        assert_eq!(code_raw_col_to_rendered_col("let x = 1;", true, 0), 1);
        assert_eq!(code_raw_col_to_rendered_col("let x = 1;", true, 6), 7);
    }

    #[test]
    fn indented_body_col_drops_the_stripped_indent() {
        // Raw `    let x = 1;` renders as ` let x = 1;`: 4 stripped, 1 pad.
        assert_eq!(code_raw_col_to_rendered_col("    let x = 1;", false, 4), 1);
        assert_eq!(code_raw_col_to_rendered_col("    let x = 1;", false, 10), 7);
    }

    #[test]
    fn cols_inside_the_stripped_indent_collapse_onto_the_first_content_cell() {
        for raw_col in 0..=4 {
            assert_eq!(
                code_raw_col_to_rendered_col("    code", false, raw_col),
                CODE_PAD_COLS,
            );
        }
    }

    #[test]
    fn code_indent_strip_stops_at_four_spaces() {
        assert_eq!(code_indent_strip_chars("        deep", false), 4);
        assert_eq!(code_indent_strip_chars("\tcode", false), 1);
        assert_eq!(code_indent_strip_chars("  two", false), 2);
        assert_eq!(code_indent_strip_chars("none", false), 0);
        // A fenced block's content is verbatim — its indent is real code.
        assert_eq!(code_indent_strip_chars("    indented", true), 0);
    }

    #[test]
    fn col_round_trips_fenced() {
        let raw = "fn main() {}";
        for raw_col in 0..=raw.chars().count() {
            let rendered = code_raw_col_to_rendered_col(raw, true, raw_col);
            assert_eq!(code_rendered_col_to_raw_col(raw, true, rendered), raw_col);
        }
    }

    #[test]
    fn col_round_trips_indented() {
        let raw = "    fn main() {}";
        // Columns at or past the strip round-trip; earlier ones deliberately
        // collapse (see `cols_inside_the_stripped_indent_…`).
        for raw_col in 4..=raw.chars().count() {
            let rendered = code_raw_col_to_rendered_col(raw, false, raw_col);
            assert_eq!(code_rendered_col_to_raw_col(raw, false, rendered), raw_col);
        }
    }

    #[test]
    fn click_on_the_pad_cell_lands_on_the_first_content_char() {
        assert_eq!(code_rendered_col_to_raw_col("code", true, 0), 0);
        assert_eq!(code_rendered_col_to_raw_col("    code", false, 0), 4);
    }

    #[test]
    fn click_in_the_trailing_fill_clamps_to_end_of_line() {
        assert_eq!(code_rendered_col_to_raw_col("code", true, 60), 4);
    }

    #[test]
    fn fence_rows_are_the_first_and_last_line_of_a_fenced_block() {
        let lines = ["```rust", "let x = 1;", "```"];
        assert!(is_code_fence_row(true, 0, &lines));
        assert!(is_code_fence_row(true, 2, &lines));
        assert!(!is_code_fence_row(true, 1, &lines));
    }

    #[test]
    fn an_indented_block_has_no_fence_rows() {
        let lines = ["    a", "    b", "    c"];
        assert!(!is_code_fence_row(false, 0, &lines));
        assert!(!is_code_fence_row(false, 2, &lines));
    }

    /// A fenced block a user is part-way through typing has no closing
    /// fence, so its last line is ordinary code and must keep the pad-cell
    /// column shift — testing "is it the last line?" alone made every
    /// character of it map one cell off (issue #28, unclosed case).
    #[test]
    fn an_unclosed_fence_has_no_closing_fence_row() {
        let lines = ["```rust", "let x = 1;"];
        assert!(is_code_fence_row(true, 0, &lines));
        assert!(!is_code_fence_row(true, 1, &lines));
        assert!(!line_allows_raw_reveal(
            Some(&Block::CodeBlock {
                language: Some("rust".into()),
                content: "let x = 1;\n".into(),
                fenced: true,
            }),
            1,
            &lines,
        ));
    }

    #[test]
    fn closing_fence_accepts_tildes_and_longer_runs_but_not_prose() {
        assert!(is_closing_fence_line("```"));
        assert!(is_closing_fence_line("~~~"));
        assert!(is_closing_fence_line("`````"));
        assert!(is_closing_fence_line("  ```  "));
        // An info string is illegal on a closing fence, so these are not one.
        assert!(!is_closing_fence_line("```rust"));
        assert!(!is_closing_fence_line("let x = 1;"));
        assert!(!is_closing_fence_line("``"));
        assert!(!is_closing_fence_line(""));
    }

    #[test]
    fn reveal_rule_matches_the_rendered_view_gate() {
        let fenced = Block::CodeBlock {
            language: Some("rust".into()),
            content: "x\n".into(),
            fenced: true,
        };
        let indented = Block::CodeBlock {
            language: None,
            content: "x\n".into(),
            fenced: false,
        };
        let para = Block::Paragraph { inlines: vec![] };

        let fenced_lines = ["```rust", "x", "```"];
        let indented_lines = ["    x", "    y"];
        let prose_lines = ["hello"];

        // Fenced: fences reveal, the body does not.
        assert!(line_allows_raw_reveal(Some(&fenced), 0, &fenced_lines));
        assert!(line_allows_raw_reveal(Some(&fenced), 2, &fenced_lines));
        assert!(!line_allows_raw_reveal(Some(&fenced), 1, &fenced_lines));
        // Indented: nothing reveals.
        assert!(!line_allows_raw_reveal(Some(&indented), 0, &indented_lines));
        // Everything else reveals, blank-line virtual blocks included.
        assert!(line_allows_raw_reveal(Some(&para), 0, &prose_lines));
        assert!(line_allows_raw_reveal(None, 0, &prose_lines));
    }
}
