//! Wrap geometry: where a styled `Line` or a raw source line breaks into visual rows at a
//! viewport width, the hanging indent its continuation rows take, and the cell each char lands
//! on.  Pure layout, no painting: [`ui::line_render`](crate::ui::line_render) paints exactly
//! these rows, and the editor's cursor, click and scroll math reads them, so the painter and
//! every mapping agree by construction.

use ratatui::style::Style;
use ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use crate::markdown::RowOrigin;

/// Display width of `ch` in terminal cells (0 for control chars).  Shared by the renderer
/// and the wrap-row calculator so geometry agrees with cursor and selection coordinates.
pub fn char_cells(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Char index at screen cell column `target_cell`, with the row's first content cell at
/// `indent`.  The landing rules shared by vertical navigation and mouse clicks:
///
/// - `target_cell <= indent` lands at char 0 — hanging-indent padding is virtual, not
///   text, so the cursor never sits in it.
/// - A `target_cell` *inside* a multi-cell glyph lands after it; the cursor never sits in
///   a wide char's right half.
/// - Past the row's width, returns one past the last char for the caller to clamp.
pub fn char_idx_at_cell_col<I>(iter: I, target_cell: usize, indent: usize) -> usize
where
    I: IntoIterator<Item = char>,
{
    if target_cell <= indent {
        return 0;
    }
    let mut acc = indent;
    let mut count = 0;
    for ch in iter {
        let w = char_cells(ch);
        if acc + w > target_cell {
            return if acc == target_cell { count } else { count + 1 };
        }
        acc += w;
        count += 1;
    }
    count
}

/// Inverse of [`char_idx_at_cell_col`], for seeding `preferred_col` after a horizontal
/// move so later vertical navigation lands at the same screen cell.
pub fn cell_col_at_char_idx<I>(iter: I, char_idx: usize, indent: usize) -> usize
where
    I: IntoIterator<Item = char>,
{
    let mut acc = indent;
    for (i, ch) in iter.into_iter().enumerate() {
        if i >= char_idx {
            break;
        }
        acc += char_cells(ch);
    }
    acc
}

/// Largest `n` with `chars[start..start + n]` fitting `cell_budget`.  A first char wider
/// than the budget still returns 1, so the wrap loop makes progress at narrow viewports;
/// the renderer clips the overflow.
fn chars_within_cell_budget(chars: &[(char, Style)], start: usize, cell_budget: usize) -> usize {
    let mut total = 0usize;
    let mut count = 0usize;
    for (ch, _) in &chars[start..] {
        let w = char_cells(*ch);
        if count > 0 && total + w > cell_budget {
            break;
        }
        total += w;
        count += 1;
        if total >= cell_budget {
            break;
        }
    }
    count
}

/// Where the row after one ending at `end` begins — normally `end`, but a lone space at
/// the break is absorbed and belongs to no row (`next_start > end`; see the row-tuple
/// contract on [`visual_rows_of_chars`]), since it would otherwise open the next row as
/// what reads like accidental indentation.
///
/// Applied to every break arm, not just the hard one: a row can end on a `.`, a `)` or an
/// emoji cluster with the sentence's space still to come.  Two spaces are never absorbed
/// (interior whitespace is content), nor is a trailing one, which would leave no following
/// row to hold the cursor.
fn absorbed_next_start(chars: &[(char, Style)], end: usize) -> usize {
    let lone_space = chars.get(end).is_some_and(|(c, _)| *c == ' ')
        && end + 1 < chars.len()
        && chars[end + 1].0 != ' ';
    if lone_space {
        end + 1
    } else {
        end
    }
}

// ── Grapheme clusters ─────────────────────────────────────────────────────

/// Mask over `0..=chars.len()` marking grapheme-cluster starts (the past-the-end index is
/// always a boundary).
///
/// A row must never end mid-cluster: the terminal draws a cluster as one glyph, and a ZWJ
/// is an ordinary break candidate under "anything non-alphanumeric", so an emoji family
/// would otherwise be split across rows.
///
/// `None` on the all-ASCII fast path.  It matters: this sits on the per-keystroke
/// navigation path as well as the paint path, and segmenting allocates.
fn cluster_starts(chars: &[(char, Style)]) -> Option<Vec<bool>> {
    if chars.iter().all(|(ch, _)| ch.is_ascii()) {
        return None;
    }
    let text: String = chars.iter().map(|(ch, _)| *ch).collect();
    // Consumers address text by char index, so carry a running char count rather than
    // materializing a byte→char table.
    let mut starts = vec![false; chars.len() + 1];
    let mut char_idx = 0usize;
    for cluster in UnicodeSegmentation::graphemes(text.as_str(), true) {
        starts[char_idx] = true;
        char_idx += cluster.chars().count();
    }
    starts[chars.len()] = true;
    Some(starts)
}

/// Is char index `i` a cluster boundary?  `None` (the all-ASCII path) means every one is.
fn is_cluster_boundary(clusters: Option<&[bool]>, i: usize) -> bool {
    clusters.is_none_or(|starts| starts.get(i).copied().unwrap_or(true))
}

/// Pull `end` back to the nearest cluster boundary so a hard break can't sever a cluster.
/// Never returns `start` — a row holding one over-wide cluster must still make progress,
/// and the renderer clips the overflow.
fn snap_to_cluster_boundary(clusters: Option<&[bool]>, start: usize, end: usize) -> usize {
    let mut snapped = end;
    while snapped > start && !is_cluster_boundary(clusters, snapped) {
        snapped -= 1;
    }
    if snapped == start {
        end
    } else {
        snapped
    }
}

// ── Wrap break candidates ─────────────────────────────────────────────────

/// Non-alphanumeric characters that still carry no wrap break.  A no-break space is
/// *defined* that way, and code blocks pad blank lines with U+00A0 — breaking there would
/// split padding emitted precisely to keep a row intact.
fn is_no_break_char(ch: char) -> bool {
    matches!(ch, '\u{a0}' | '\u{202f}' | '\u{2060}' | '\u{feff}')
}

/// Unambiguous opening delimiters.  Breaking *after* one strands it alone at
/// the row's right edge, away from the phrase it opens.
fn is_opening_delimiter(ch: char) -> bool {
    matches!(
        ch,
        '(' | '[' | '{' | '\u{201c}' | '\u{2018}' | '\u{ab}' | '\u{bf}' | '\u{a1}'
    )
}

/// Punctuation that binds a token when it sits *between* two alphanumerics: contractions,
/// decimals, clock times, file names, `snake_case`.  Outside that sandwich the same
/// character is an ordinary break point, so a URL still breaks after `//`, `?`, `#`, `&`.
///
/// `/` is deliberately **not** in the set: it would keep `and/or` whole but strip every
/// break out of a URL path, leaving long links to hard-break mid-segment.  Links are far
/// more common in Markdown, and wrapping after a `/` reads better than a severed word.
fn is_intra_word_punctuation(ch: char) -> bool {
    matches!(ch, '.' | ',' | ':' | '\'' | '\u{2019}' | '_')
}

/// May a visual row end with `chars[i]`?  The base rule is "anything non-alphanumeric",
/// with three refinements keeping tokens and punctuation pairs intact, over a
/// grapheme-cluster gate no refinement can override.  Both neighbors matter, hence the
/// slice rather than a lone `char`.
fn is_break_after(chars: &[(char, Style)], i: usize, clusters: Option<&[bool]>) -> bool {
    // Only a break when the next char opens a new cluster; otherwise `i` sits inside one
    // and the row would end mid-glyph.
    if !is_cluster_boundary(clusters, i + 1) {
        return false;
    }
    let ch = chars[i].0;
    if ch.is_alphanumeric() || is_no_break_char(ch) {
        return false;
    }
    let prev_alnum = i > 0 && chars[i - 1].0.is_alphanumeric();
    let next_alnum = chars.get(i + 1).is_some_and(|(c, _)| c.is_alphanumeric());

    if is_opening_delimiter(ch) && next_alnum {
        return false;
    }
    // `"` and `'` are ambiguous: opening when a word follows and none precedes.
    if matches!(ch, '"' | '\'') && next_alnum && !prev_alnum {
        return false;
    }
    if is_intra_word_punctuation(ch) && prev_alnum && next_alnum {
        return false;
    }
    true
}

/// Does the row end with a one-letter word marooned at the right edge?  Reported only
/// when text precedes it, since moving the row's *first* word down would empty the row.
fn ends_with_lone_word(chars: &[(char, Style)], start: usize, break_at: usize) -> bool {
    if !chars[break_at].0.is_whitespace() || break_at < start + 2 {
        return false;
    }
    let word = break_at - 1;
    chars[word].0.is_alphanumeric() && word > start && chars[word - 1].0.is_whitespace()
}

// ── Row indents ───────────────────────────────────────────────────────────

/// Where a line's wrapped rows start, in cells: `lead` before its first row's text, `hang` before
/// each continuation row's.  Never detected from the text: a rendered row's comes from the
/// renderer (`RowOrigin::hang`, through `ParsedDoc::row_indent`), a revealed raw line's from its
/// leaf's recorded content column (`row_map::revealed_indent`), so a row that merely reads like
/// a list item (`1\. text`) doesn't hang, and a footnote's flow hangs under its text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Indent {
    /// Blank cells before the first row's text: a revealed line padded to where its rendered
    /// content started (a right-aligned ` 6.` marker's pad).  0 on every rendered row.
    pub lead: usize,
    /// Cells before each continuation row's text: the hanging indent.
    pub hang: usize,
}

impl Indent {
    /// No indent: a flat wrap (Raw mode, plain text).
    pub const NONE: Indent = Indent { lead: 0, hang: 0 };

    /// Continuation rows hang `hang` cells; the first row starts at cell 0.
    pub const fn hanging(hang: usize) -> Self {
        Indent { lead: 0, hang }
    }

    /// A rendered row's indent: the hang its origin states ([`RowOrigin::hang`]); none for a row
    /// with no origin.
    pub fn of_row(origin: Option<&RowOrigin>) -> Self {
        origin.map_or(Indent::NONE, |o| Indent::hanging(o.hang as usize))
    }

    /// The indent the wrap applies at `width`: each part clamped by [`effective_indent`], so a
    /// row always has room for text.
    pub fn at(self, width: usize) -> Self {
        Indent {
            lead: effective_indent(self.lead, width),
            hang: effective_indent(self.hang, width),
        }
    }

    /// The cells before row `sub_row`'s text: `lead` on the first row, `hang` on the rest.
    pub fn row(self, sub_row: usize) -> usize {
        if sub_row == 0 {
            self.lead
        } else {
            self.hang
        }
    }
}

/// The visual rows produced by wrapping `chars` at `width` cells behind `indent` (its `lead`
/// before the first row, its `hang` before every other), as `(start, end, next_start)`
/// char-index tuples.
///
/// `chars[start..end]` is the row's content.  `next_start` normally equals `end`, but is
/// `end + 1` when the break absorbed the single following space; chars in `end..next_start`
/// have no cell, and both `sub_line_of_col` and the painter show a cursor resting there at
/// the start of the following row.
///
/// The painter calls this directly, so rendering and visual-line navigation always agree
/// on where rows break.
pub fn visual_rows_of_chars(
    chars: &[(char, Style)],
    width: usize,
    indent: Indent,
) -> Vec<(usize, usize, usize)> {
    let mut rows = Vec::new();
    if width == 0 {
        rows.push((0, chars.len(), chars.len()));
        return rows;
    }
    let indent = indent.at(width);

    let clusters = cluster_starts(chars);
    let clusters = clusters.as_deref();

    let mut start = 0;
    let mut row_idx = 0usize;
    loop {
        if start >= chars.len() {
            if rows.is_empty() {
                rows.push((0, 0, 0));
            }
            break;
        }
        let row_width = width.saturating_sub(indent.row(row_idx)).max(1);
        let n_chars = chars_within_cell_budget(chars, start, row_width);
        let remaining = chars.len() - start;
        let (row_end, next_start) = if n_chars >= remaining {
            (chars.len(), chars.len())
        } else {
            // Pull the budget back so a hard break falls between clusters, not inside one.
            let window_end = snap_to_cluster_boundary(clusters, start, start + n_chars);
            let break_at = (start..window_end)
                .rev()
                .find(|&i| is_break_after(chars, i, clusters));

            let end = match break_at {
                Some(bp) => {
                    // Back up past a stranded one-letter word so it goes down with its
                    // noun.
                    let bp = if ends_with_lone_word(chars, start, bp) {
                        (start..bp - 1)
                            .rev()
                            .find(|&i| is_break_after(chars, i, clusters))
                            .unwrap_or(bp)
                    } else {
                        bp
                    };
                    bp + 1
                }
                // Nothing in the window carries a break: one long word or over-wide
                // cluster.
                None => window_end,
            };
            (end, absorbed_next_start(chars, end))
        };

        rows.push((start, row_end, next_start));
        if next_start >= chars.len() || next_start == start {
            break;
        }
        start = next_start;
        row_idx += 1;
    }

    if rows.is_empty() {
        rows.push((0, 0, 0));
    }

    rows
}

/// [`visual_rows_of_chars`] for plain text, with no hanging indent — raw buffer text
/// follows the source layout, not the rendered one.
pub fn visual_rows_of_str(text: &str, width: usize) -> Vec<(usize, usize, usize)> {
    let chars: Vec<(char, Style)> = text.chars().map(|c| (c, Style::default())).collect();
    visual_rows_of_chars(&chars, width, Indent::NONE)
}

/// The wrap of a raw source line as the reveal paints it behind `indent` (the line's
/// `row_map::revealed_indent`), and that indent as applied at `width` ([`Indent::at`]).  The
/// reveal's row count (`EffectiveRows`), its click mapping and the cursor's sub-row (stacked or
/// revealed in place) all read this, so none of them can disagree with the painter about where a
/// revealed line wraps.  Callers mapping a column offset it by [`Indent::row`].
pub fn revealed_rows_of_str(
    text: &str,
    indent: Indent,
    width: usize,
) -> (Vec<(usize, usize, usize)>, Indent) {
    let width = width.max(1);
    let chars: Vec<(char, Style)> = text.chars().map(|c| (c, Style::default())).collect();
    (
        visual_rows_of_chars(&chars, width, indent),
        indent.at(width),
    )
}

/// Rows (>= 1) a revealed raw source line paints at `width`: [`revealed_rows_of_str`]'s count,
/// with an empty line taking one row.
pub fn revealed_row_count(text: &str, indent: Indent, width: usize) -> usize {
    revealed_rows_of_str(text, indent, width).0.len().max(1)
}

/// Rows a styled `Line` occupies at `width` behind `indent` — the same layout
/// [`render_line`](crate::ui::line_render::render_line) paints, so scroll-bound math matches the
/// viewport.  Empty lines take one row.
pub fn visual_rows_for_line(line: &Line<'_>, indent: Indent, width: usize) -> usize {
    PaintedRows::new(line, indent, width).rows.len()
}

/// The hanging indent continuation rows take at `width`: `indent`, or 0 when it would leave no
/// room for text.  The wrap and every reader of its rows apply this one rule, so row counts and
/// cell positions agree with the painter.
pub fn effective_indent(indent: usize, width: usize) -> usize {
    if indent + 1 >= width {
        0
    } else {
        indent
    }
}

/// A styled `Line`'s wrapped rows at one width, laid out as
/// [`render_line`](crate::ui::line_render::render_line) paints them: chars, row breaks and the
/// applied indent derived together, for every reader of the painted geometry (row counts,
/// hit-tests, highlight patching).
pub struct PaintedRows {
    /// The line's chars, each with its span's style.
    pub chars: Vec<(char, Style)>,
    /// `(start, end, next_start)` per row, as [`visual_rows_of_chars`] returns them.
    pub rows: Vec<(usize, usize, usize)>,
    /// The indent the rows start behind, as applied at the width ([`Indent::at`]).
    pub indent: Indent,
}

impl PaintedRows {
    pub fn new(line: &Line<'_>, indent: Indent, width: usize) -> Self {
        let chars: Vec<(char, Style)> = line
            .spans
            .iter()
            .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
            .collect();
        let rows = visual_rows_of_chars(&chars, width, indent);
        Self {
            chars,
            rows,
            indent: indent.at(width),
        }
    }

    /// The cell where char `char_idx` starts on row `sub_row`, which must hold it (or end at it:
    /// the row's `end` gives the cell just past its last char).
    pub fn cell_of(&self, sub_row: usize, char_idx: usize) -> usize {
        self.indent.row(sub_row)
            + self.chars[self.rows[sub_row].0..char_idx]
                .iter()
                .map(|&(c, _)| char_cells(c))
                .sum::<usize>()
    }
}

/// Where char `char_idx` of `line` paints at `width`: its sub-row and the cells it covers there
/// (two for a wide glyph).  `None` past the last char, and for a char a wrap absorbed (the space
/// a break swallows owns no cell).  Hit-tests on rendered chrome with no source byte behind it
/// (a footnote's `↩`) go through this, never through a char count: a char index is a screen
/// cell only on an unwrapped row of single-cell chars.
pub fn char_cells_at(
    line: &Line<'_>,
    indent: Indent,
    width: usize,
    char_idx: usize,
) -> Option<(usize, std::ops::Range<usize>)> {
    if width == 0 {
        return None;
    }
    let painted = PaintedRows::new(line, indent, width);
    let sub_row = painted
        .rows
        .iter()
        .position(|&(start, end, _)| (start..end).contains(&char_idx))?;
    let x = painted.cell_of(sub_row, char_idx);
    Some((sub_row, x..x + char_cells(painted.chars[char_idx].0)))
}

/// The cell just past the last char `line` paints on wrapped row `sub_row` at `width`, its
/// indent included; 0 for a row the line doesn't have.
pub fn sub_row_end_cell(line: &Line<'_>, indent: Indent, width: usize, sub_row: usize) -> usize {
    if width == 0 {
        return 0;
    }
    let painted = PaintedRows::new(line, indent, width);
    painted
        .rows
        .get(sub_row)
        .map_or(0, |&(_, end, _)| painted.cell_of(sub_row, end))
}

/// The highest char column the cursor may occupy while still rendering on visual row
/// `row`.
///
/// The *last* row owns the one-past-the-end slot so an end-of-line cursor can sit on its
/// trailing blank cell; every other row stops one char short of `end`, since column `end`
/// is the next row's first char and paints at its column 0 — a cursor clamped there makes
/// Up appear stuck.
///
/// **Clamp against `end`, never `next_start`.** They differ when a break absorbed the
/// following space, and those chars own no cell, so `next_start - 1` lands the cursor on
/// the absorbed space — the same failure again.  The single derivation all four click- and
/// navigation-mapping sites share.
pub fn last_col_in_row(row: (usize, usize, usize), is_last_row: bool) -> usize {
    let (start, end, _) = row;
    if is_last_row {
        end
    } else {
        end.saturating_sub(1).max(start)
    }
}

/// `(sub_line_idx, visual_col)` for a raw char column, given a line's visual-row layout.
/// End-of-line and wrap-skip positions map to the nearest visible row's end column.
pub fn sub_line_of_col(rows: &[(usize, usize, usize)], raw_col: usize) -> (usize, usize) {
    for (i, &(s, e, n)) in rows.iter().enumerate() {
        if raw_col < n {
            // An absorbed space has no cell here; report the next row's first column so
            // the cursor stays visible.
            if raw_col >= e && i + 1 < rows.len() {
                return (i + 1, 0);
            }
            let row_width = e - s;
            let visual_col = raw_col.saturating_sub(s).min(row_width);
            return (i, visual_col);
        }
    }
    if let Some(&(s, e, _)) = rows.last() {
        let last_idx = rows.len() - 1;
        let row_width = e - s;
        let visual_col = raw_col.saturating_sub(s).min(row_width);
        return (last_idx, visual_col);
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    #[test]
    fn visual_rows_short_line() {
        let rows = visual_rows_of_str("hello", 10);
        assert_eq!(rows, vec![(0, 5, 5)]);
    }

    #[test]
    fn visual_rows_wraps_at_space() {
        // "hello world" wraps at space (col 5).
        let rows = visual_rows_of_str("hello world foo", 10);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0, 6, 6)); // "hello " on row 0
        assert_eq!(rows[1], (6, 15, 15)); // "world foo" on row 1
    }

    #[test]
    fn visual_rows_force_break_on_long_word() {
        // Single long word exceeds width — force break.
        let rows = visual_rows_of_str("abcdefghijklmnop", 8);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0, 8, 8));
        assert_eq!(rows[1], (8, 16, 16));
    }

    #[test]
    fn visual_rows_empty_string() {
        let rows = visual_rows_of_str("", 10);
        assert_eq!(rows, vec![(0, 0, 0)]);
    }

    #[test]
    fn visual_rows_with_indent_word_aligned() {
        let chars: Vec<(char, Style)> = "• hello world foo"
            .chars()
            .map(|c| (c, Style::default()))
            .collect();
        let rows = visual_rows_of_chars(&chars, 10, Indent::hanging(2));
        assert_eq!(rows[0].0, 0);
        assert!(rows.len() >= 2);
        assert_eq!(rows.last().map(|r| r.1), Some(17));
    }

    #[test]
    fn visual_rows_with_indent_zero_matches_flat() {
        let s = "hello world foo";
        let chars: Vec<(char, Style)> = s.chars().map(|c| (c, Style::default())).collect();
        let with_indent = visual_rows_of_chars(&chars, 10, Indent::NONE);
        let flat = visual_rows_of_str(s, 10);
        assert_eq!(with_indent, flat);
    }

    #[test]
    fn visual_rows_for_line_counts_indent_extra_rows() {
        // Indent 2 makes continuation rows narrower, so the row count can only grow.
        let line = Line::from(vec![Span::raw("• "), Span::raw("hello world foo bar baz")]);
        let with_marker = visual_rows_for_line(&line, Indent::hanging(2), 10);
        let flat = visual_rows_for_line(&line, Indent::NONE, 10);
        assert!(with_marker >= flat);
    }

    /// The indent is stated, never read off the text: a line that looks like a list item wraps
    /// flat unless told to hang, and hangs wherever it is told to.
    #[test]
    fn the_indent_comes_from_the_caller_not_the_text() {
        let text = "1. alpha bravo charlie delta";
        let flat = revealed_rows_of_str(text, Indent::NONE, 15).0;
        let hung = revealed_rows_of_str(text, Indent::hanging(3), 15).0;
        assert_eq!(flat, visual_rows_of_str(text, 15));
        // `charlie delta` fits a 15-cell row, not the 12 cells left behind a 3-cell hang.
        assert_eq!((flat.len(), hung.len()), (2, 3));
    }

    /// A `lead` narrows the first row and shifts its cells; continuation rows take `hang`.
    #[test]
    fn a_lead_narrows_and_shifts_the_first_row_only() {
        let line = Line::from("ab cd ef gh");
        let indent = Indent { lead: 2, hang: 4 };
        let painted = PaintedRows::new(&line, indent, 8);
        // 6 cells for `ab cd `, then 4 for each continuation row.
        assert_eq!(painted.rows[0], (0, 6, 6));
        assert_eq!(painted.cell_of(0, 0), 2);
        assert_eq!(painted.cell_of(1, painted.rows[1].0), 4);
    }

    /// Both parts collapse to 0 where they would leave a row no room for text.
    #[test]
    fn an_indent_too_wide_for_the_row_collapses() {
        let indent = Indent { lead: 9, hang: 3 };
        assert_eq!(indent.at(10), Indent { lead: 0, hang: 3 });
        assert_eq!(indent.at(4), Indent { lead: 0, hang: 0 });
        assert_eq!(indent.at(80), indent);
    }

    #[test]
    fn visual_rows_preserves_interior_whitespace_across_wrap() {
        // The runs of spaces before "b" must NOT be swallowed.
        let rows = visual_rows_of_str("a              b", 5);
        assert_eq!(rows[0], (0, 5, 5));
        assert_eq!(rows[1], (5, 10, 10));
        assert_eq!(rows[2], (10, 15, 15));
        assert_eq!(rows[3], (15, 16, 16));
    }

    // ── Break-candidate refinements ───────────────────────────────

    #[test]
    fn contraction_apostrophe_is_not_a_break_point() {
        // The apostrophe is not a break, so the whole word moves down.
        let rows = visual_rows_of_str("when they're here", 12);
        assert_eq!(rows[0], (0, 5, 5)); // "when "
        assert_eq!(rows[1], (5, 17, 17)); // "they're here"
    }

    #[test]
    fn smart_apostrophe_is_not_a_break_point() {
        // Rendered text carries U+2019: the parser enables smart punctuation.
        let rows = visual_rows_of_str("when they\u{2019}re here", 12);
        assert_eq!(rows[0], (0, 5, 5));
        assert_eq!(rows[1], (5, 17, 17));
    }

    #[test]
    fn intra_word_punctuation_keeps_tokens_whole() {
        for text in ["value 3.14159 x", "count 1,000,00 x", "meet 12:30:00 x"] {
            let rows = visual_rows_of_str(text, 12);
            assert_eq!(
                rows[0].1,
                text.find(' ').unwrap() + 1,
                "{text} broke inside its token"
            );
        }
    }

    #[test]
    fn url_still_breaks_after_the_scheme_slashes() {
        // The intra-word rule needs alphanumerics on both sides, so `//` still breaks.
        let rows = visual_rows_of_str("see https://example.com/x", 20);
        assert_eq!(rows[0], (0, 12, 12)); // "see https://"
    }

    #[test]
    fn a_url_path_breaks_at_a_slash_rather_than_mid_segment() {
        // With `/` in the intra-word set the whole path would be one unbreakable token
        // and the row would hard-break at whatever column the budget ran out on.
        let text = "at github.com/user/repo/blob/main/x";
        let rows = visual_rows_of_str(text, 20);
        let chars: Vec<char> = text.chars().collect();
        for &(start, end, _) in &rows {
            let row: String = chars[start..end].iter().collect();
            assert!(
                row.ends_with('/') || end == chars.len(),
                "row {row:?} broke mid-segment instead of after a slash"
            );
        }
    }

    #[test]
    fn no_break_after_an_opening_delimiter() {
        // Breaking after `(` would leave the paren hanging alone at the row edge.
        let rows = visual_rows_of_str("a note (remark) here", 11);
        assert_eq!(rows[0], (0, 7, 7)); // "a note "
    }

    #[test]
    fn nbsp_is_never_a_break_point() {
        let text = "aa\u{a0}bb cc";
        let rows = visual_rows_of_str(text, 5);
        // The NBSP is not a candidate, so the row hard-breaks instead.
        assert_eq!(rows[0].1, 5);
    }

    #[test]
    fn one_letter_word_is_carried_down_to_its_noun() {
        // The lone "a" moves down with "story".
        let rows = visual_rows_of_str("tell them a story", 14);
        assert_eq!(rows[0], (0, 10, 10)); // "tell them "
        assert_eq!(rows[1], (10, 17, 17)); // "a story"
    }

    #[test]
    fn a_lone_word_starting_the_row_is_left_alone() {
        // Nothing precedes it on the row, so backing up would empty the row.
        let rows = visual_rows_of_str("a xyzzyplugh", 3);
        assert_eq!(rows[0], (0, 2, 2)); // "a "
    }

    // ── Absorbed wrap space ───────────────────────────────────────

    #[test]
    fn hard_break_absorbs_the_following_space() {
        // The space after an exactly-filled row must not open the next as indentation.
        let rows = visual_rows_of_str("abcdefghij klm", 10);
        assert_eq!(rows[0], (0, 10, 11));
        assert_eq!(rows[1], (11, 14, 14));
    }

    #[test]
    fn absorbed_space_maps_the_cursor_to_the_next_row_start() {
        let rows = visual_rows_of_str("abcdefghij klm", 10);
        assert_eq!(sub_line_of_col(&rows, 10), (1, 0));
        assert_eq!(sub_line_of_col(&rows, 11), (1, 0));
    }

    #[test]
    fn a_run_of_spaces_at_a_hard_break_is_preserved() {
        let rows = visual_rows_of_str("abcdefghij  klm", 10);
        assert_eq!(rows[0], (0, 10, 10));
    }

    #[test]
    fn a_trailing_space_at_a_hard_break_is_not_absorbed() {
        // Absorbing a trailing space would leave no row to hold the cursor.
        let rows = visual_rows_of_str("abcdefghij ", 10);
        assert_eq!(rows[0], (0, 10, 10));
        assert_eq!(rows[1], (10, 11, 11));
    }

    #[test]
    fn last_col_in_row_clamps_against_end_not_next_start() {
        let rows = visual_rows_of_str("abcdefghij klm", 10);
        assert_eq!(rows[0], (0, 10, 11));
        // Row 0 absorbed the space at char 10, so its last legal column is 9 — not
        // `next_start - 1`, which is the absorbed space and paints at row 1's column 0.
        assert_eq!(last_col_in_row(rows[0], false), 9);
        // The last row owns the one-past-the-end slot for an EOL cursor.
        assert_eq!(last_col_in_row(rows[1], true), 14);
        // A single-char row can never be clamped below its own start.
        assert_eq!(last_col_in_row((7, 8, 8), false), 7);
    }

    #[test]
    fn a_soft_break_on_punctuation_absorbs_the_space_after_it() {
        // The row ends on `.`, so the sentence space is still to come.
        let rows = visual_rows_of_str("abcde. fgh", 6);
        assert_eq!(rows[0], (0, 6, 7));
        assert_eq!(rows[1], (7, 10, 10));
    }

    // ── Grapheme clusters ─────────────────────────────────────────

    #[test]
    fn a_zwj_sequence_is_never_split_across_rows() {
        // 7 chars drawn as one glyph; the ZWJ is non-alphanumeric and so was once an
        // ordinary break candidate.
        let text = "a \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466} family here";
        let rows = visual_rows_of_str(text, 8);
        let chars: Vec<char> = text.chars().collect();
        for &(start, end, _) in &rows {
            let row: String = chars[start..end].iter().collect();
            assert!(
                !row.starts_with('\u{200d}') && !row.ends_with('\u{200d}'),
                "row {row:?} ends or starts inside the cluster"
            );
        }
    }

    #[test]
    fn a_combining_mark_stays_with_its_base_char() {
        // A break inside the cluster would strand the accent on the next row.
        let text = "cafe\u{301} au lait";
        let rows = visual_rows_of_str(text, 5);
        let chars: Vec<char> = text.chars().collect();
        for &(_, end, _) in &rows {
            assert_ne!(
                chars.get(end),
                Some(&'\u{301}'),
                "row ended between the base char and its combining mark"
            );
        }
    }

    #[test]
    fn a_regional_indicator_pair_stays_whole() {
        // Two regional indicators form one flag glyph.
        let text = "go \u{1f1ef}\u{1f1f5} now";
        let rows = visual_rows_of_str(text, 5);
        let chars: Vec<char> = text.chars().collect();
        for &(_, end, _) in &rows {
            assert_ne!(
                chars.get(end),
                Some(&'\u{1f1f5}'),
                "row ended between the two halves of the flag"
            );
        }
    }

    #[test]
    fn a_cluster_wider_than_the_row_still_makes_progress() {
        // The wrap must fall back to a hard break rather than loop on an empty row.
        let text = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let rows = visual_rows_of_str(text, 3);
        assert!(rows.len() > 1);
        assert!(rows.iter().all(|&(s, e, _)| e > s));
    }

    #[test]
    fn ascii_text_takes_the_no_segmentation_fast_path() {
        // A guard on the fast path's premise: an all-ASCII line has no multi-char
        // clusters, so the layout must be identical either way.
        let chars: Vec<(char, Style)> = "hello world foo bar"
            .chars()
            .map(|c| (c, Style::default()))
            .collect();
        assert!(cluster_starts(&chars).is_none());
    }

    // ── Cell-width awareness ──────────────────────────────────────

    #[test]
    fn wrap_budget_is_cells_not_chars_for_wide_chars() {
        let chars: Vec<(char, Style)> = "🥇🥇🥇".chars().map(|c| (c, Style::default())).collect();
        let rows = visual_rows_of_chars(&chars, 4, Indent::NONE);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0, 2, 2));
        assert_eq!(rows[1], (2, 3, 3));
    }

    #[test]
    fn wrap_force_breaks_when_single_wide_char_exceeds_width() {
        // Width 1 can't fit a 2-cell emoji, but the loop must still make progress.
        let chars: Vec<(char, Style)> = "🥇🥇".chars().map(|c| (c, Style::default())).collect();
        let rows = visual_rows_of_chars(&chars, 1, Indent::NONE);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0, 1, 1));
        assert_eq!(rows[1], (1, 2, 2));
    }

    #[test]
    fn char_idx_at_cell_col_forbidden_indent_zone_snaps_to_row_start() {
        // Clicks anywhere in the virtual padding snap to char index 0.
        let chars = ['x', 'y', 'z'];
        for cell in 0..=2 {
            assert_eq!(
                char_idx_at_cell_col(chars.iter().copied(), cell, 2),
                0,
                "indent zone cell {cell} did not snap to row start",
            );
        }
        // Cell 3 is the first content cell.
        assert_eq!(char_idx_at_cell_col(chars.iter().copied(), 3, 2), 1);
    }

    #[test]
    fn char_idx_at_cell_col_snaps_past_wide_char() {
        // Cell 1 is mid-glyph and must snap past the emoji; cell 0 lands before it.
        let chars = ['🥇', 'B'];
        assert_eq!(char_idx_at_cell_col(chars.iter().copied(), 0, 0), 0);
        assert_eq!(char_idx_at_cell_col(chars.iter().copied(), 1, 0), 1);
        assert_eq!(char_idx_at_cell_col(chars.iter().copied(), 2, 0), 1);
    }

    #[test]
    fn cell_col_at_char_idx_round_trips_with_wide_chars() {
        let chars = ['A', '🥇', 'B'];
        assert_eq!(cell_col_at_char_idx(chars.iter().copied(), 0, 0), 0);
        assert_eq!(cell_col_at_char_idx(chars.iter().copied(), 1, 0), 1);
        assert_eq!(cell_col_at_char_idx(chars.iter().copied(), 2, 0), 3);
        assert_eq!(cell_col_at_char_idx(chars.iter().copied(), 3, 0), 4);
    }
}
