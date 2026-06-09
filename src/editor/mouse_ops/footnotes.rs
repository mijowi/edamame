//! Raw-source hit-testing for footnote references and definitions.
//!
//! A click or keyboard-follow lands on a rope byte; this module scans the
//! enclosing source line for `[^label]` syntax and classifies the hit:
//!   * `[^label]` (not followed by `:`) → [`LinkTarget::Footnote`] — jump
//!     to the matching definition.
//!   * `[^label]:` (a definition leader) → [`LinkTarget::FootnoteBack`] —
//!     return to the reference.
//!
//! The classification is deliberately a raw scan (no AST), mirroring
//! [`super::links::link_at_offset`].  It serves both input paths: the
//! keyboard `FollowLinkUnderCursor` handler and the mouse click path.  A
//! click on the rendered definition's `  N.  ` leader maps, via the 1:1
//! raw-column coordinate translation, back onto the `[^label]:` source
//! bytes — so the definition arm doubles as the back-link hit-test without
//! any rendered-column bookkeeping.
//!
//! The definition also renders a trailing `↩` glyph (the visible back-link
//! affordance, see `markdown::renderer`).  That glyph is appended chrome
//! with no raw source byte, so the raw scan can't see it;
//! [`back_link_glyph_at_click`] hit-tests it on the rendered line directly.

use crate::editor::footnote_edit;
use crate::editor::link::LinkTarget;
use crate::editor::EditorState;

use super::coord::rendered_line_at_row;

/// The back-link glyph appended to the end of a rendered footnote
/// definition.  Kept in sync with `markdown::renderer`'s
/// `render_footnote_definition`.
const BACK_LINK_GLYPH: char = '↩';

/// Classify the footnote (if any) at `byte` in `source`.  Returns the
/// follow target, or `None` when the byte isn't on footnote syntax.
///
/// Delegates the `[^label]` scan to [`footnote_edit::scan`] (run over the
/// enclosing line) so the hit-test and the edit primitives share one
/// implementation.
pub fn footnote_at_offset(source: &str, byte: usize) -> Option<LinkTarget> {
    let byte = byte.min(source.len());
    let line_start = source[..byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = source[byte..]
        .find('\n')
        .map(|i| byte + i)
        .unwrap_or(source.len());
    let line = &source[line_start..line_end];
    let col = byte - line_start;

    footnote_edit::scan(line).into_iter().find_map(|s| {
        // Reference hit span is `[^label]`; a definition also covers its
        // trailing `:` so a click on the leader's colon still counts.
        // `s.end` is one past the `]`.
        let span_end = if s.is_definition { s.end } else { s.end - 1 };
        if col >= s.start && col <= span_end {
            Some(if s.is_definition {
                LinkTarget::FootnoteBack(s.label)
            } else {
                LinkTarget::Footnote(s.label)
            })
        } else {
            None
        }
    })
}

/// If the click at rendered `(col, row)` lands on a footnote definition's
/// trailing back-link glyph, return the [`LinkTarget::FootnoteBack`]
/// target.
///
/// The glyph is appended chrome with no raw source byte, so
/// [`footnote_at_offset`]'s raw scan can't resolve it (that path handles
/// the `  N.  ` leader, which IS column-matched to the `[^N]:` source).
/// This rendered-line check covers the trailing glyph as a second
/// affordance.  The click column must be at or past the glyph (it is the
/// last cell of the line, so there's nothing beyond it to confuse).
pub(super) fn back_link_glyph_at_click(
    state: &EditorState,
    col: u16,
    row: u16,
) -> Option<LinkTarget> {
    let (line, _) = rendered_line_at_row(state, row as usize)?;
    let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    let glyph_col = total.checked_sub(1)?;
    if (col as usize) < glyph_col {
        return None;
    }
    // Cheap guard before the source lookup: the last rendered char must be
    // the glyph.
    if line.spans.iter().flat_map(|s| s.content.chars()).last() != Some(BACK_LINK_GLYPH) {
        return None;
    }
    // Resolve the label from the definition block that produced this row.
    let (line_idx, _) = state.rendered_line_at_visual_row(
        state.scroll.saturating_add(row as usize),
        state.viewport_width,
    );
    let block_byte = state
        .parsed
        .source_map
        .original_byte_for_rendered_line(line_idx)?;
    let range = state
        .parsed
        .source_map
        .original_range_for_byte(block_byte)?;
    let source = state.buffer.contents();
    let block_text = source.get(range.start..range.end.min(source.len()))?;
    let label = footnote_edit::scan(block_text)
        .into_iter()
        .find(|s| s.is_definition)
        .map(|s| s.label)?;
    Some(LinkTarget::FootnoteBack(label))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_classifies_as_footnote() {
        let src = "See note.[^1] end\n";
        let byte = src.find("[^1]").unwrap() + 1; // inside the marker
        assert_eq!(
            footnote_at_offset(src, byte),
            Some(LinkTarget::Footnote("1".into()))
        );
    }

    #[test]
    fn definition_marker_classifies_as_back_link() {
        let src = "[^1]: the note text\n";
        // Click on the leading `[` (where the rendered `  N.  ` leader maps
        // 1:1 — the leader is the back-link's column-matched hit zone).
        assert_eq!(
            footnote_at_offset(src, 0),
            Some(LinkTarget::FootnoteBack("1".into()))
        );
    }

    #[test]
    fn named_label_supported() {
        let src = "ref[^note] here\n";
        let byte = src.find("[^note]").unwrap() + 2;
        assert_eq!(
            footnote_at_offset(src, byte),
            Some(LinkTarget::Footnote("note".into()))
        );
    }

    #[test]
    fn reference_inside_definition_body_resolves_to_that_reference() {
        let src = "[^1]: see [^2] also\n";
        let byte = src.find("[^2]").unwrap() + 1;
        assert_eq!(
            footnote_at_offset(src, byte),
            Some(LinkTarget::Footnote("2".into()))
        );
    }

    #[test]
    fn body_text_is_not_a_footnote() {
        let src = "[^1]: the note text\n";
        let byte = src.find("note").unwrap();
        assert_eq!(footnote_at_offset(src, byte), None);
    }

    #[test]
    fn plain_brackets_are_not_footnotes() {
        let src = "an [array] index\n";
        let byte = src.find("array").unwrap();
        assert_eq!(footnote_at_offset(src, byte), None);
    }

    #[test]
    fn escaped_reference_is_not_a_footnote() {
        let src = r"an \[^1] escaped marker";
        let byte = src.find("[^1]").unwrap() + 1;
        assert_eq!(footnote_at_offset(src, byte), None);
    }
}
