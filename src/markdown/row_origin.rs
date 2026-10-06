//! Where each rendered row came from, recorded by the renderer at the moment it emits the row.
//!
//! The renderer writes every row into a [`RowSink`] together with its [`RowOrigin`]: which of
//! the block's source lines the row shows, and how its columns relate to those lines'
//! characters.  Line numbers are block-relative, like the [`SrcLines`](super::ast::SrcLines)
//! they come from (which says why).  The rules are in `docs/dev/editing-model.md`.

use std::ops::Range;

use ratatui::text::Line;

use super::ast::to_u32;

/// Where one rendered row came from.  1:1 with the rows it was emitted beside.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RowOrigin {
    /// Block-relative source lines this row shows: one line, or several for a reflowed flow.
    /// `None` for a row no source line owns (an unclosed fence's placeholder closing row, a
    /// table's top border).
    pub lines: Option<Range<u32>>,
    pub cols: ColOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColOrigin {
    /// No column relation to the source: fence label, closing-fence placeholder, table border,
    /// horizontal rule, a marker-only row, a big-H1 glyph row, an image's reserved rows, a
    /// blank row between blocks.
    Chrome,
    /// Content starting at raw char `raw_col` and rendered cell `rendered_col`; the part before
    /// both is prefix (bar, marker, indent, pad cell).
    Content {
        raw_col: u32,
        rendered_col: u32,
        kind: ContentKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentKind {
    /// Inline Markdown: map through `InlineColMap` over the raw text past `raw_col`.
    Inline,
    /// Characters shown verbatim (code body, frontmatter, raw HTML): identity past the prefix.
    Verbatim,
    /// Chunk `sub` of table row `row` (0 the header, `1 + i` data row `i`): map through
    /// `table_layout`'s cell geometry.
    TableRow { row: u32, sub: u32 },
    /// A flow over every line in `lines` (a reflowed paragraph, a heading or list item whose
    /// text spans several lines).  `document::row_map` maps it line by line (each line sliced
    /// past its content column under its own `InlineColMap`, one space per break), or, where an
    /// inline spans a break (`*a⏎b*`), through one map over the slices joined by `\n` — not
    /// first, since joined, a continuation reading `2. a` or `===` turns into block syntax it
    /// wasn't in the document.
    Flow,
}

impl RowOrigin {
    /// A row with no column relation, showing `line` (or no line at all).  A line at
    /// `u32::MAX` (a saturated count) clamps one below, so the range is never empty.
    pub fn chrome(line: Option<u32>) -> Self {
        Self {
            lines: line.map(|l| {
                let l = l.min(u32::MAX - 1);
                l..l + 1
            }),
            cols: ColOrigin::Chrome,
        }
    }

    /// A row of content of `kind` showing `lines`.
    pub fn content(lines: Range<u32>, raw_col: u32, rendered_col: u32, kind: ContentKind) -> Self {
        Self {
            lines: Some(lines),
            cols: ColOrigin::Content {
                raw_col,
                rendered_col,
                kind,
            },
        }
    }

    /// The same row behind `cells` more cells of prefix (a quote bar, a footnote leader).
    pub fn shifted(mut self, cells: usize) -> Self {
        if let ColOrigin::Content { rendered_col, .. } = &mut self.cols {
            *rendered_col = rendered_col.saturating_add(to_u32(cells));
        }
        self
    }

    /// The first source line this row shows.
    pub fn first_line(&self) -> Option<u32> {
        self.lines.as_ref().map(|l| l.start)
    }
}

/// Rendered rows and their origins, kept in lockstep: every push adds one of each.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowSink {
    pub lines: Vec<Line<'static>>,
    pub origins: Vec<RowOrigin>,
}

impl RowSink {
    pub fn push(&mut self, line: Line<'static>, origin: RowOrigin) {
        self.lines.push(line);
        self.origins.push(origin);
    }

    pub fn len(&self) -> usize {
        debug_assert_eq!(self.lines.len(), self.origins.len());
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Append clones of `lines` / `origins` (a render-cache hit).
    pub fn extend_from(&mut self, lines: &[Line<'static>], origins: &[RowOrigin]) {
        self.lines.extend(lines.iter().cloned());
        self.origins.extend(origins.iter().cloned());
    }

    /// Rows and origins, in lockstep.
    pub fn into_rows(self) -> impl Iterator<Item = (Line<'static>, RowOrigin)> {
        debug_assert_eq!(self.lines.len(), self.origins.len());
        self.lines.into_iter().zip(self.origins)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_shows_one_line_or_none() {
        assert_eq!(RowOrigin::chrome(Some(3)).lines, Some(3..4));
        assert_eq!(RowOrigin::chrome(None).first_line(), None);
        // A line at the top of the range neither overflows nor empties the range.
        assert_eq!(
            RowOrigin::chrome(Some(u32::MAX)).lines,
            Some(u32::MAX - 1..u32::MAX)
        );
    }

    #[test]
    fn shifting_moves_only_the_rendered_column() {
        let row = RowOrigin::content(2..4, 3, 1, ContentKind::Flow).shifted(2);
        assert_eq!(row, RowOrigin::content(2..4, 3, 3, ContentKind::Flow));
        assert_eq!(row.first_line(), Some(2));
        // Chrome has no column to shift.
        assert_eq!(
            RowOrigin::chrome(Some(1)).shifted(2),
            RowOrigin::chrome(Some(1))
        );
        // Saturates rather than wrapping.
        let far = RowOrigin::content(0..1, 0, u32::MAX - 1, ContentKind::Inline).shifted(5);
        assert_eq!(
            far.cols,
            ColOrigin::Content {
                raw_col: 0,
                rendered_col: u32::MAX,
                kind: ContentKind::Inline,
            }
        );
    }

    #[test]
    fn a_sink_keeps_rows_and_origins_in_lockstep() {
        let mut sink = RowSink::default();
        assert!(sink.is_empty());
        sink.push(Line::from("a"), RowOrigin::chrome(Some(0)));
        let other = [RowOrigin::chrome(Some(1)), RowOrigin::chrome(Some(2))];
        sink.extend_from(&[Line::from("b"), Line::from("c")], &other);
        assert_eq!(sink.len(), 3);
        let lines: Vec<Option<u32>> = sink.into_rows().map(|(_, o)| o.first_line()).collect();
        assert_eq!(lines, [Some(0), Some(1), Some(2)]);
    }
}
