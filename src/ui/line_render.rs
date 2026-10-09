use ratatui::{buffer::Buffer as TuiBuf, layout::Rect, style::Style, text::Line};

use crate::document::wrap::{char_cells, visual_rows_of_chars, Indent, PaintedRows};

/// Write a styled `Line` to the TUI buffer, wrapping at `area.width` (in *cells*) when
/// `wrap` is true.  Returns the visual rows consumed (≥ 1).
///
/// Trailing cells are filled with the line's base style so styled blocks extend the full
/// width.  Wrapping is word-aware, and the rows start behind `indent`: its `lead` before the
/// first, its `hang` before every continuation row.  The caller states it — a rendered row's
/// from `ParsedDoc::row_indent`, a revealed line's from `row_map::revealed_indent` — so the
/// painter and every reader of the same row agree.
///
/// `cursor_col_override` is `Some((char index, style))` — not a cell column — and recolors
/// that cell while leaving the character visible.  It applies only to a wide char's first
/// cell; terminals can't style the right half independently.
///
/// Used by tests here; production code calls [`render_line_from_visual`] for sub-row
/// scrolling.
#[allow(dead_code)]
pub fn render_line(
    line: &Line<'static>,
    indent: Indent,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
) -> u16 {
    render_line_with_cursor_from_visual(line, indent, area, buf, visual_y, wrap, None, 0)
}

pub fn render_line_from_visual(
    line: &Line<'static>,
    indent: Indent,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    skip_rows: usize,
) -> u16 {
    render_line_with_cursor_from_visual(line, indent, area, buf, visual_y, wrap, None, skip_rows)
}

/// Used by tests in this module.
#[allow(dead_code)]
pub fn render_line_with_cursor(
    line: &Line<'static>,
    indent: Indent,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    cursor_col_override: Option<(usize, Style)>,
) -> u16 {
    render_line_with_cursor_from_visual(
        line,
        indent,
        area,
        buf,
        visual_y,
        wrap,
        cursor_col_override,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn render_line_with_cursor_from_visual(
    line: &Line<'static>,
    indent: Indent,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    cursor_col_override: Option<(usize, Style)>,
    skip_rows: usize,
) -> u16 {
    render_line_reporting_cursor(
        line,
        indent,
        area,
        buf,
        visual_y,
        wrap,
        cursor_col_override,
        skip_rows,
    )
    .0
}

/// [`render_line_with_cursor_from_visual`] plus the absolute `(x, y)` cell the cursor
/// override was painted at.  `RenderedView` uses it to re-stamp the cursor over post-pass
/// overlays (search highlights, selection washes) that would otherwise bury it.
#[allow(clippy::too_many_arguments)]
pub fn render_line_reporting_cursor(
    line: &Line<'static>,
    indent: Indent,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    cursor_col_override: Option<(usize, Style)>,
    skip_rows: usize,
) -> (u16, Option<(u16, u16)>) {
    render_line_core(
        line,
        indent,
        area,
        buf,
        visual_y,
        wrap,
        cursor_col_override,
        skip_rows,
    )
}

/// Raw-mode variant of [`render_line_with_cursor_from_visual`]: a **flat** wrap, never a
/// hanging indent.
///
/// Raw mode shows the file, so indenting continuation rows would draw whitespace that
/// isn't in the document — and `visual_rows_of_str`, which backs both the scroll cache and
/// the click mapping, wraps at indent 0, so an indent here would give the painter a
/// different row count from the scroll math and offset every continuation-row click.
/// Indent 0 also suppresses the blockquote-bar repaint, correctly: the `> ` on the first
/// row is real source text and continuation rows have none.
pub fn render_raw_line_with_cursor(
    line: &Line<'static>,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    cursor_col_override: Option<(usize, Style)>,
    skip_rows: usize,
) -> u16 {
    render_line_core(
        line,
        Indent::NONE,
        area,
        buf,
        visual_y,
        true,
        cursor_col_override,
        skip_rows,
    )
    .0
}

/// Shared implementation behind [`render_line_reporting_cursor`] and
/// [`render_raw_line_with_cursor`], which passes [`Indent::NONE`] for Raw mode's flat wrap.
#[allow(clippy::too_many_arguments)]
fn render_line_core(
    line: &Line<'static>,
    indent: Indent,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    cursor_col_override: Option<(usize, Style)>,
    skip_rows: usize,
) -> (u16, Option<(u16, u16)>) {
    if visual_y >= area.height {
        return (0, None);
    }
    let width = area.width as usize;
    if width == 0 {
        return (1, None);
    }
    let abs_y = area.y + visual_y;

    let line_style = line.style;
    let mut chars: Vec<(char, Style)> = Vec::new();
    for span in &line.spans {
        let style = line_style.patch(span.style);
        for ch in span.content.chars() {
            chars.push((ch, style));
        }
    }

    if !wrap {
        if skip_rows > 0 {
            return (0, None);
        }
        let cursor_cell = paint_row(
            &chars,
            0,
            chars.len(),
            0,
            0,
            &[],
            area,
            buf,
            abs_y,
            line_style,
            cursor_col_override,
        );
        return (1, cursor_cell);
    }

    // Single source of truth for row breaks, keeping the renderer in lockstep with the
    // navigation/selection helpers below.
    let rows = visual_rows_of_chars(&chars, width, indent);
    let indent = indent.at(width);
    // Repainted on each continuation row so the quote gutter doesn't vanish mid-quote.
    let cont_prefix = leading_bar_prefix(&chars);

    let mut cursor_cell = None;
    let mut cur_visual = visual_y;
    for (row_idx, &(start, row_end, _next_start)) in rows.iter().enumerate().skip(skip_rows) {
        if cur_visual >= area.height {
            break;
        }
        let cur_abs_y = area.y + cur_visual;
        let row_indent = indent.row(row_idx);
        let row_prefix: &[(char, Style)] = if row_idx == 0 { &[] } else { &cont_prefix };
        // A space absorbed by the previous row's break owns no cell, so show a cursor
        // resting on it at this row's first char — where `sub_line_of_col` reports it.
        let row_override = match (
            row_idx.checked_sub(1).and_then(|p| rows.get(p)),
            cursor_col_override,
        ) {
            (Some(&(_, prev_end, prev_next)), Some((col, style)))
                if col >= prev_end && col < prev_next =>
            {
                Some((start, style))
            }
            _ => cursor_col_override,
        };
        if let Some(cell) = paint_row(
            &chars,
            start,
            row_end,
            start,
            row_indent,
            row_prefix,
            area,
            buf,
            cur_abs_y,
            line_style,
            row_override,
        ) {
            cursor_cell = Some(cell);
        }
        cur_visual += 1;
    }

    (cur_visual - visual_y, cursor_cell)
}

/// Paint one visual row: `chars[start..end]` after `row_indent` cells of padding.
/// `abs_col_base` is added to each relative index when matching
/// `cursor_col_override` — the row's `start` when wrapped, 0 on the no-wrap path.
///
/// `cont_prefix` is the styled run repainted into the indent zone (the blockquote bar, see
/// [`leading_bar_prefix`]); remaining indent cells are blank-filled in `line_style`.
///
/// Returns the absolute cell the cursor override was drawn at, `None` when it isn't on
/// this row.
#[allow(clippy::too_many_arguments)]
fn paint_row(
    chars: &[(char, Style)],
    start: usize,
    end: usize,
    abs_col_base: usize,
    row_indent: usize,
    cont_prefix: &[(char, Style)],
    area: Rect,
    buf: &mut TuiBuf,
    abs_y: u16,
    line_style: Style,
    cursor_col_override: Option<(usize, Style)>,
) -> Option<(u16, u16)> {
    let mut cursor_cell = None;
    let mut x = area.x;
    let area_end = area.x + area.width;
    // Repaint the blockquote bar(s) so the gutter persists, then blank-fill the rest of
    // the indent with the surrounding background.
    let mut prefix_iter = cont_prefix.iter();
    for _ in 0..row_indent {
        if x >= area_end {
            break;
        }
        if let Some(cell) = buf.cell_mut((x, abs_y)) {
            match prefix_iter.next() {
                Some((ch, style)) => {
                    cell.set_char(*ch);
                    cell.set_style(*style);
                }
                None => {
                    cell.set_char(' ');
                    cell.set_style(line_style);
                }
            }
        }
        x += 1;
    }
    // The last char's cell, while every char of the row has one (for the full-row EOL cursor).
    let mut last_cell = None;
    for (rel_idx, (ch, style)) in chars[start..end].iter().enumerate() {
        let cells = char_cells(*ch) as u16;
        if cells == 0 || x >= area_end {
            // Zero-width chars merge into the preceding grapheme: skip without
            // advancing `x` rather than overwriting that cell's glyph.
            if cells == 0 {
                continue;
            }
            last_cell = None;
            break;
        }
        last_cell = Some(x);
        let abs_col = abs_col_base + rel_idx;
        let cursor_style = cursor_col_override
            .filter(|(col, _)| *col == abs_col)
            .map(|(_, s)| s);
        if let Some(cell) = buf.cell_mut((x, abs_y)) {
            cell.set_char(*ch);
            cell.set_style(cursor_style.unwrap_or(*style));
        }
        if cursor_style.is_some() {
            cursor_cell = Some((x, abs_y));
        }
        x += cells;
    }
    // End-of-line cursor: its column is one past the last char, so the loop above never
    // reaches it — draw it on the first trailing blank cell instead.  Guarded on
    // `col == chars.len()` so a word-wrap gap never matches.
    let eol_cursor = cursor_col_override.filter(|&(col, _)| col == chars.len());
    let mut fill_col = abs_col_base + (end - start);
    while x < area_end {
        if let Some(cell) = buf.cell_mut((x, abs_y)) {
            if let Some((_, s)) = eol_cursor.filter(|&(col, _)| col == fill_col) {
                cell.set_char(' ');
                cell.set_style(s);
                cursor_cell = Some((x, abs_y));
            } else {
                cell.set_style(line_style);
            }
        }
        x += 1;
        fill_col += 1;
    }
    // A row ending the line that fills every cell leaves the EOL cursor no blank to sit on:
    // draw it over the last char instead, rather than nowhere.
    if let (None, Some(lx), Some((_, s))) = (cursor_cell, last_cell, eol_cursor) {
        if end == chars.len() {
            if let Some(cell) = buf.cell_mut((lx, abs_y)) {
                cell.set_style(s);
                cursor_cell = Some((lx, abs_y));
            }
        }
    }
    cursor_cell
}

/// Patch `style` onto the screen cells of `line`'s chars in `cols` (char columns), walking the
/// line's wrapped rows as [`render_line`] lays them out at `area.width`.  `y_first` is the
/// absolute row of the first painted sub-row, `skip_rows` the sub-rows scrolled off above it,
/// and `rows_used` how many the line painted.  A wide glyph gets both its cells; trailing
/// padding is never touched.  The one highlight painter behind Preview selection and Rendered
/// search / selection.
#[allow(clippy::too_many_arguments)]
pub fn patch_char_cols(
    line: &Line<'_>,
    indent: Indent,
    buf: &mut TuiBuf,
    area: Rect,
    y_first: u16,
    rows_used: u16,
    skip_rows: usize,
    cols: std::ops::Range<usize>,
    style: Style,
) {
    let width = area.width as usize;
    if width == 0 || cols.is_empty() {
        return;
    }
    let painted = PaintedRows::new(line, indent, width);
    for (painted_off, (row_off, &(row_start, row_end, _))) in
        painted.rows.iter().enumerate().skip(skip_rows).enumerate()
    {
        if painted_off as u16 >= rows_used {
            break;
        }
        let y = y_first + painted_off as u16;
        if y >= area.y + area.height {
            break;
        }
        let sel_start = cols.start.max(row_start);
        let sel_end = cols.end.min(row_end);
        if sel_start >= sel_end {
            continue;
        }
        // Columns are chars; the screen advances by cells, two for a wide glyph.
        let mut x_off = painted.cell_of(row_off, sel_start);
        for &(ch, _) in &painted.chars[sel_start..sel_end] {
            let w = char_cells(ch);
            for dx in 0..w {
                let x = area.x + (x_off + dx) as u16;
                if x >= area.x + area.width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(cell.style().patch(style));
                }
            }
            x_off += w;
        }
    }
}

/// The leading run of rendered `▎ ` bar units, repainted into each continuation row's
/// indent zone so the quote gutter survives the wrap.  Only the rendered glyph is
/// captured: a raw-revealed `> ` is literal source and gets plain blank padding.
fn leading_bar_prefix(chars: &[(char, Style)]) -> Vec<(char, Style)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < chars.len() && chars[i].0 == '▎' && chars[i + 1].0 == ' ' {
        out.push(chars[i]);
        out.push(chars[i + 1]);
        i += 2;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use ratatui::text::Span;

    /// Highlight columns are chars; the painter places them in cells, so `a` after two wide
    /// glyphs sits at cell 4, and a highlighted wide glyph covers both its cells.
    #[test]
    fn patch_char_cols_places_char_columns_in_cells() {
        let sel = Style::default().bg(Color::Magenta);
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = TuiBuf::empty(area);
        patch_char_cols(
            &Line::from("日本ab"),
            Indent::NONE,
            &mut buf,
            area,
            0,
            1,
            0,
            1..3,
            sel,
        );
        let bg = |x: u16| buf[(x, 0)].bg;
        assert_ne!(bg(1), Color::Magenta);
        assert_eq!(bg(2), Color::Magenta);
        assert_eq!(bg(3), Color::Magenta);
        assert_eq!(bg(4), Color::Magenta, "`a` is at cell 4");
        assert_ne!(bg(5), Color::Magenta);
    }

    /// At a width the hanging indent leaves no room in, continuation rows start at the left edge,
    /// and the highlight follows the painter there.  It once padded them by the unclamped indent.
    #[test]
    fn patch_char_cols_drops_an_indent_that_leaves_no_room() {
        let sel = Style::default().bg(Color::Magenta);
        let area = Rect::new(0, 0, 3, 3);
        let line = Line::from("• abcdef");
        let indent = Indent::hanging(2);
        let mut painted = TuiBuf::empty(area);
        render_line(&line, indent, area, &mut painted, 0, true);
        assert_eq!(painted[(0, 1)].symbol(), "a", "rows: `• ` / `abc` / `def`");
        let mut buf = TuiBuf::empty(area);
        patch_char_cols(&line, indent, &mut buf, area, 0, 3, 0, 2..3, sel);
        assert_eq!(
            buf[(0, 1)].bg,
            Color::Magenta,
            "`a` is at the row's first cell"
        );
        assert_ne!(buf[(2, 1)].bg, Color::Magenta);
    }

    #[test]
    fn leading_bar_prefix_captures_rendered_bars_only() {
        let rendered: Vec<(char, Style)> = "▎ ▎ quoted"
            .chars()
            .map(|c| (c, Style::default()))
            .collect();
        assert_eq!(leading_bar_prefix(&rendered).len(), 4);
        // Raw `> ` is literal source, never repainted on continuation rows.
        let raw: Vec<(char, Style)> = "> quoted".chars().map(|c| (c, Style::default())).collect();
        assert!(leading_bar_prefix(&raw).is_empty());
    }

    #[test]
    fn wrapped_blockquote_repaints_bar_on_continuation_rows() {
        // "▎ alpha beta gamma" wrapped at width 10: row 0 holds "▎ alpha "
        // and the continuation row must begin with the "▎ " gutter, not blanks.
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = TuiBuf::empty(area);
        let line = Line::from(vec![Span::raw("▎ "), Span::raw("alpha beta gamma")]);
        let rows = render_line(&line, Indent::hanging(2), area, &mut buf, 0, true);
        assert!(rows >= 2, "expected the quote to wrap, got {rows} row(s)");
        // Row 0 starts with the bar.
        assert_eq!(
            buf.cell((0, 0)).map(|c| c.symbol().to_string()),
            Some("▎".into())
        );
        assert_eq!(
            buf.cell((0, 1)).map(|c| c.symbol().to_string()),
            Some("▎".into())
        );
        assert_eq!(
            buf.cell((1, 1)).map(|c| c.symbol().to_string()),
            Some(" ".into())
        );
        assert_ne!(
            buf.cell((2, 1)).map(|c| c.symbol().to_string()),
            Some(" ".into())
        );
    }

    /// A line whose last row fills every cell has no trailing blank for an end-of-line cursor,
    /// so the cursor is drawn over the last char, unwrapped or on a wrapped last row.  A line
    /// cut off unwrapped never shows its end, so its cursor stays hidden.
    #[test]
    fn eol_cursor_on_a_full_row_paints_over_the_last_char() {
        let style = Style::default().fg(ratatui::style::Color::Red);
        let area = Rect::new(0, 0, 10, 3);
        // The cell and the symbol left under the cursor: the last char, or the trailing blank
        // a line one cell shorter keeps.
        for (text, wrap, want) in [
            ("abcdefghij", true, Some(((9, 0), "j"))),
            ("abcdefghij", false, Some(((9, 0), "j"))),
            ("abcdefghij abcdefghij", true, Some(((9, 1), "j"))),
            ("abcdefghi", true, Some(((9, 0), " "))),
            ("abcdefghijk", false, None),
        ] {
            let mut buf = TuiBuf::empty(area);
            let col = text.chars().count();
            let (_, cursor) = render_line_reporting_cursor(
                &Line::from(text),
                Indent::NONE,
                area,
                &mut buf,
                0,
                wrap,
                Some((col, style)),
                0,
            );
            assert_eq!(cursor, want.map(|(cell, _)| cell), "{text:?}, wrap {wrap}");
            if let Some((cell, symbol)) = want {
                assert_eq!(buf[cell].fg, ratatui::style::Color::Red, "{text:?}");
                assert_eq!(buf[cell].symbol(), symbol, "{text:?}");
            }
        }
    }

    #[test]
    fn cursor_on_an_absorbed_space_paints_on_the_next_row() {
        let line = Line::from("abcdefghij klm");
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = TuiBuf::empty(area);
        let style = Style::default().fg(ratatui::style::Color::Red);
        let (_, cursor) = render_line_reporting_cursor(
            &line,
            Indent::NONE,
            area,
            &mut buf,
            0,
            true,
            Some((10, style)),
            0,
        );
        assert_eq!(cursor, Some((0, 1)));
    }

    /// A `lead` pads the first row and narrows it; the cursor and the continuation rows follow.
    #[test]
    fn a_lead_pads_the_first_row_and_the_cursor_with_it() {
        let area = Rect::new(0, 0, 12, 3);
        let mut buf = TuiBuf::empty(area);
        let style = Style::default().fg(ratatui::style::Color::Red);
        let indent = Indent { lead: 1, hang: 4 };
        let (rows, cursor) = render_line_reporting_cursor(
            &Line::from("6. alpha bravo"),
            indent,
            area,
            &mut buf,
            0,
            true,
            Some((3, style)),
            0,
        );
        let row = |y: u16| (0..12u16).map(|x| buf[(x, y)].symbol()).collect::<String>();
        assert_eq!(rows, 2);
        assert_eq!(row(0), " 6. alpha   ");
        assert_eq!(row(1), "    bravo   ");
        assert_eq!(cursor, Some((4, 0)), "on the `a`, past the pad");
    }

    #[test]
    fn render_line_paints_wide_char_using_two_cells() {
        // The right-half cell of the wide char is left unwritten — terminals own it — so
        // the next char lands at column 3.
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = TuiBuf::empty(area);
        let line = Line::from(vec![Span::raw("A🥇B")]);
        render_line(&line, Indent::NONE, area, &mut buf, 0, false);
        assert_eq!(
            buf.cell((0, 0)).map(|c| c.symbol().to_string()),
            Some("A".into())
        );
        assert_eq!(
            buf.cell((1, 0)).map(|c| c.symbol().to_string()),
            Some("🥇".into())
        );
        assert_eq!(
            buf.cell((3, 0)).map(|c| c.symbol().to_string()),
            Some("B".into())
        );
    }

    #[test]
    fn zero_width_combining_mark_does_not_advance_cell_cursor() {
        // The combining mark has zero display width and must not consume a cell, so
        // column 1 holds '!' rather than a blank.
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = TuiBuf::empty(area);
        let line = Line::from(vec![Span::raw("e\u{0301}!")]);
        render_line(&line, Indent::NONE, area, &mut buf, 0, false);
        assert_eq!(
            buf.cell((0, 0)).map(|c| c.symbol().to_string()),
            Some("e".into())
        );
        assert_eq!(
            buf.cell((1, 0)).map(|c| c.symbol().to_string()),
            Some("!".into())
        );
    }
}
