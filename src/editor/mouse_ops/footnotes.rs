//! Raw-source hit-testing for footnote references and definitions.
//!
//! A raw scan of the enclosing line (no AST, mirroring [`super::links::link_at_offset`]),
//! shared by keyboard follow and mouse click: `[^label]` → [`LinkTarget::Footnote`],
//! `[^label]:` → [`LinkTarget::FootnoteBack`].  The rendered definition's `  N.  ` leader
//! maps 1:1 onto the `[^label]:` bytes, so the definition arm doubles as the back-link
//! hit-test.  The trailing `↩` glyph is appended chrome with no raw byte, so
//! [`back_link_glyph_at_click`] hit-tests it on the rendered line instead.

use crate::editor::footnote_edit;
use crate::editor::link::LinkTarget;
use crate::editor::EditorState;
use crate::ui::line_render;

use super::coord::rendered_line_at_row;

/// Kept in sync with `markdown::renderer`'s `render_footnote_definition`.
const BACK_LINK_GLYPH: char = '↩';

/// Classify the footnote syntax (if any) at `byte` in `source`.  Delegates to
/// [`footnote_edit::scan`] so the hit-test and the edit primitives share one implementation.
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
        // A definition's hit span also covers its trailing `:` (`s.end` is one past the `]`).
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

/// The [`LinkTarget::FootnoteBack`] target when a rendered `(col, row)` lands on a definition's
/// trailing `↩` glyph.  The hit zone is exactly `" ↩"` as painted: the glyph's cell on the
/// definition's last wrapped row, and the space before it when the wrap left that on the same
/// row.  A click past the glyph places the cursor at line end instead.  Located by where the
/// glyph paints, not by the line's char count, which matches its cell only on an unwrapped row
/// of single-cell chars (issue #60).
pub(super) fn back_link_glyph_at_click(
    state: &EditorState,
    col: u16,
    row: u16,
) -> Option<LinkTarget> {
    let (line, sub_row) = rendered_line_at_row(state, row as usize)?;
    if line.spans.iter().flat_map(|s| s.content.chars()).last() != Some(BACK_LINK_GLYPH) {
        return None;
    }
    let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    let width = state.viewport_width;
    let (glyph_row, glyph) = line_render::char_cells_at(&line, width, total - 1)?;
    if glyph_row != sub_row {
        return None;
    }
    let zone_start = match total
        .checked_sub(2)
        .and_then(|i| line_render::char_cells_at(&line, width, i))
    {
        Some((space_row, space)) if space_row == glyph_row => space.start,
        _ => glyph.start,
    };
    if !(zone_start..glyph.end).contains(&(col as usize)) {
        return None;
    }
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
        let byte = src.find("[^1]").unwrap() + 1;
        assert_eq!(
            footnote_at_offset(src, byte),
            Some(LinkTarget::Footnote("1".into()))
        );
    }

    #[test]
    fn definition_marker_classifies_as_back_link() {
        let src = "[^1]: the note text\n";
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
