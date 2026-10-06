//! Raw ↔ rendered column geometry of code blocks — the single derivation, shared by the overlay
//! painter, the cursor indicator, and the mouse hit-test, which must never re-derive it (drift
//! paints the cursor beside its character and lands clicks off by one; issue #28).
//!
//! A code body row renders as `format!(" {:<width$}", line, …)`, so rendered column `c` shows
//! raw char `c - 1`.  An *indented* block adds a second term: pulldown-cmark strips its
//! up-to-four-space (or single-tab) indent before the text reaches `Block::CodeBlock::content`.
//!
//! *Fence* rows are outside the mapping — they render a label or an NBSP placeholder, with no
//! column relation to their raw text.  Ask [`is_code_fence_row`] and handle them separately.

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

/// True when a line is a CommonMark *closing* fence: three or more of the same delimiter
/// (`` ` `` or `~`) and nothing else.  A closing fence may carry no info string, which makes
/// this test exact rather than a heuristic.
fn is_closing_fence_line(raw_line: &str) -> bool {
    let trimmed = raw_line.trim();
    let Some(first) = trimmed.chars().next() else {
        return false;
    };
    (first == '`' || first == '~')
        && trimmed.chars().count() >= 3
        && trimmed.chars().all(|c| c == first)
}

/// True when raw line `raw_line_idx` is one of a fenced block's fence rows (an indented block
/// has none).
///
/// The closing fence is the last raw line **only when that line really is a fence delimiter**:
/// an unclosed block — every one a user is part-way through typing — ends on ordinary code that
/// still needs the pad-cell column shift.
///
/// `raw_lines` must come from
/// [`raw_source_lines`](crate::ui::rendered_view::raw_source_lines) (or an equivalent split that
/// drops the trailing empty entry) — a bare `split('\n')` appends a phantom line, so the real
/// closing fence would not be the last element.
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

/// Raw char column on a code-block **body** line → rendered char column.  Columns inside the
/// stripped indent collapse onto the first rendered content cell.  Callers must have excluded
/// fence rows via [`is_code_fence_row`].
pub fn code_raw_col_to_rendered_col(raw_line: &str, fenced: bool, raw_col: usize) -> usize {
    raw_col.saturating_sub(code_indent_strip_chars(raw_line, fenced)) + CODE_PAD_COLS
}

/// The inverse of [`code_raw_col_to_rendered_col`], for the mouse hit-test.  A click on the pad
/// cell maps to the first content char; one in the trailing fill clamps to end-of-line.
pub fn code_rendered_col_to_raw_col(raw_line: &str, fenced: bool, rendered_col: usize) -> usize {
    let strip = code_indent_strip_chars(raw_line, fenced);
    (rendered_col.saturating_sub(CODE_PAD_COLS) + strip).min(raw_line.chars().count())
}

/// Whether `RenderedView` reveals the raw source for raw line `raw_line_idx` of the block the
/// cursor is in.  A fenced code block reveals only its fence rows and an indented block nothing;
/// every other block reveals its cursor line.  The single derivation of the rule — the mouse
/// hit-test must agree with the view about which rows show raw text.
///
/// A figures-off `$$...$$` paragraph renders as a fenced-style `math` code
/// block (see [`display_math_block_body`](crate::markdown::parser::post_pass::display_math_block_body)),
/// so it follows the same rule: only the `$$` delimiter rows reveal, and the
/// formula body — whose characters don't change — stays rendered, exactly
/// like a `` ```mermaid `` fence's body.
///
/// `block` is the *post-processed* AST block, resolved via
/// [`ParsedDoc::real_block_for_byte`](crate::document::ParsedDoc::real_block_for_byte) — never by
/// indexing `parsed.blocks` with a source-map index.  `None` (a blank-line virtual block)
/// reveals.
pub fn line_allows_raw_reveal(
    block: Option<&Block>,
    raw_line_idx: usize,
    raw_lines: &[&str],
) -> bool {
    match block {
        Some(Block::CodeBlock { fenced, .. }) => {
            is_code_fence_row(*fenced, raw_line_idx, raw_lines)
        }
        // A figures-off `$$...$$` math paragraph is painted as a fenced-style
        // `math` code block, so reveal only its `$$` delimiter rows (the
        // opening line and any `$$`-only line — matched by text so a trailing
        // blank the paragraph range absorbs can't hide the closing one),
        // never the formula body.
        Some(b) if crate::markdown::parser::post_pass::display_math_block_body(b).is_some() => {
            raw_line_idx == 0
                || raw_lines
                    .get(raw_line_idx)
                    .is_some_and(|l| l.trim() == "$$")
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
        // Columns at or past the strip round-trip; earlier ones deliberately collapse.
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

    /// Regression, issue #28: an unclosed fence's last line is ordinary code and must keep the
    /// pad-cell column shift.
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
                src: Default::default(),
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
            src: Default::default(),
        };
        let indented = Block::CodeBlock {
            language: None,
            content: "x\n".into(),
            fenced: false,
            src: Default::default(),
        };
        let para = Block::Paragraph {
            inlines: vec![],
            src: Default::default(),
        };

        let fenced_lines = ["```rust", "x", "```"];
        let indented_lines = ["    x", "    y"];
        let prose_lines = ["hello"];

        assert!(line_allows_raw_reveal(Some(&fenced), 0, &fenced_lines));
        assert!(line_allows_raw_reveal(Some(&fenced), 2, &fenced_lines));
        assert!(!line_allows_raw_reveal(Some(&fenced), 1, &fenced_lines));
        assert!(!line_allows_raw_reveal(Some(&indented), 0, &indented_lines));
        assert!(line_allows_raw_reveal(Some(&para), 0, &prose_lines));
        assert!(line_allows_raw_reveal(None, 0, &prose_lines));
    }

    /// A figures-off `$$...$$` math paragraph reveals like a fenced code
    /// block: only its `$$` delimiter rows de-render (opening row 0 and the
    /// closing `$$` line), and the formula body stays rendered — the
    /// characters don't change, so de-rendering it would be pointless churn
    /// (the bug the reuse of this gate fixes).
    #[test]
    fn display_math_paragraph_reveals_only_its_delimiter_rows() {
        use crate::markdown::ast::Inline;
        let math = Block::Paragraph {
            inlines: vec![Inline::Math {
                source: "\nE = mc^2\n".into(),
                display: true,
            }],
            src: Default::default(),
        };
        let lines = ["$$", "E = mc^2", "$$"];
        assert!(
            line_allows_raw_reveal(Some(&math), 0, &lines),
            "opening `$$` reveals"
        );
        assert!(
            !line_allows_raw_reveal(Some(&math), 1, &lines),
            "formula body must NOT de-render"
        );
        assert!(
            line_allows_raw_reveal(Some(&math), 2, &lines),
            "closing `$$` reveals"
        );
        // A trailing blank the paragraph range can absorb must not hide the
        // closing `$$` (matched by text, not by last index).
        let with_blank = ["$$", "E = mc^2", "$$", ""];
        assert!(line_allows_raw_reveal(Some(&math), 2, &with_blank));
        assert!(!line_allows_raw_reveal(Some(&math), 1, &with_blank));
    }
}
