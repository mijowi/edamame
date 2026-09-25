use ratatui::{buffer::Buffer as TuiBuf, layout::Rect, style::Style, text::Line};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

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

/// Write a styled `Line` to the TUI buffer, wrapping at `area.width` (in *cells*) when
/// `wrap` is true.  Returns the visual rows consumed (≥ 1).
///
/// Trailing cells are filled with the line's base style so styled blocks extend the full
/// width.  Wrapping is word-aware, and a recognized list marker gives continuation rows a
/// hanging indent (see [`compute_hanging_indent`]).
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
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
) -> u16 {
    render_line_with_cursor_from_visual(line, area, buf, visual_y, wrap, None, 0)
}

pub fn render_line_from_visual(
    line: &Line<'static>,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    skip_rows: usize,
) -> u16 {
    render_line_with_cursor_from_visual(line, area, buf, visual_y, wrap, None, skip_rows)
}

/// Used by tests in this module.
#[allow(dead_code)]
pub fn render_line_with_cursor(
    line: &Line<'static>,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    cursor_col_override: Option<(usize, Style)>,
) -> u16 {
    render_line_with_cursor_from_visual(line, area, buf, visual_y, wrap, cursor_col_override, 0)
}

pub fn render_line_with_cursor_from_visual(
    line: &Line<'static>,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    cursor_col_override: Option<(usize, Style)>,
    skip_rows: usize,
) -> u16 {
    render_line_reporting_cursor(
        line,
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
pub fn render_line_reporting_cursor(
    line: &Line<'static>,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    cursor_col_override: Option<(usize, Style)>,
    skip_rows: usize,
) -> (u16, Option<(u16, u16)>) {
    render_line_core(
        line,
        area,
        buf,
        visual_y,
        wrap,
        cursor_col_override,
        skip_rows,
        None,
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
        area,
        buf,
        visual_y,
        true,
        cursor_col_override,
        skip_rows,
        Some(0),
    )
    .0
}

/// Shared implementation behind [`render_line_reporting_cursor`] and
/// [`render_raw_line_with_cursor`].  `hanging_indent` of `None` detects the indent from
/// the leading marker; `Some(n)` forces it, which is how Raw mode asks for a flat wrap.
#[allow(clippy::too_many_arguments)]
fn render_line_core(
    line: &Line<'static>,
    area: Rect,
    buf: &mut TuiBuf,
    visual_y: u16,
    wrap: bool,
    cursor_col_override: Option<(usize, Style)>,
    skip_rows: usize,
    hanging_indent: Option<usize>,
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
    let indent = hanging_indent.unwrap_or_else(|| compute_hanging_indent(line));
    let rows = visual_rows_of_chars(&chars, width, indent);
    let effective_indent = if indent + 1 >= width { 0 } else { indent };
    // Repainted on each continuation row so the quote gutter doesn't vanish mid-quote.
    let cont_prefix = leading_bar_prefix(&chars);

    let mut cursor_cell = None;
    let mut cur_visual = visual_y;
    for (row_idx, &(start, row_end, _next_start)) in rows.iter().enumerate().skip(skip_rows) {
        if cur_visual >= area.height {
            break;
        }
        let cur_abs_y = area.y + cur_visual;
        let row_indent = if row_idx == 0 { 0 } else { effective_indent };
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
    for (rel_idx, (ch, style)) in chars[start..end].iter().enumerate() {
        let cells = char_cells(*ch) as u16;
        if cells == 0 || x >= area_end {
            // Zero-width chars merge into the preceding grapheme: skip without
            // advancing `x` rather than overwriting that cell's glyph.
            if cells == 0 {
                continue;
            }
            break;
        }
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
    cursor_cell
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

/// The visual rows produced by wrapping `chars` at `width` cells with a hanging `indent`
/// (applied to continuation rows only), as `(start, end, next_start)` char-index tuples.
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
    indent: usize,
) -> Vec<(usize, usize, usize)> {
    let mut rows = Vec::new();
    if width == 0 {
        rows.push((0, chars.len(), chars.len()));
        return rows;
    }
    // An indent leaving no room is ignored, matching the painter so row counts agree.
    let indent = if indent + 1 >= width { 0 } else { indent };

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
        let row_width = if row_idx == 0 {
            width
        } else {
            width.saturating_sub(indent).max(1)
        };
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
    visual_rows_of_chars(&chars, width, 0)
}

/// Rows a styled `Line` occupies at `width`, hanging indent included — the same layout
/// [`render_line`] paints, so scroll-bound math matches the viewport.  Empty lines take one
/// row.
pub fn visual_rows_for_line(line: &Line<'_>, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
        .collect();
    if chars.is_empty() {
        return 1;
    }
    let indent = compute_hanging_indent(line);
    visual_rows_of_chars(&chars, width, indent).len().max(1)
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
    let chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
        .collect();
    let indent = compute_hanging_indent(line);
    let rows = visual_rows_of_chars(&chars, width, indent);
    for (painted_off, (row_off, &(row_start, row_end, _))) in
        rows.iter().enumerate().skip(skip_rows).enumerate()
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
        // Continuation rows are pre-padded with `indent` blank cells.  Columns are chars; the
        // screen advances by cells, two for a wide glyph.
        let row_indent = if row_off == 0 { 0 } else { indent };
        let mut x_off = row_indent
            + chars[row_start..sel_start]
                .iter()
                .map(|&(c, _)| char_cells(c))
                .sum::<usize>();
        for &(ch, _) in &chars[sel_start..sel_end] {
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

/// Hanging indent in cells: the column where text begins after a list marker, so
/// continuation rows align under it and the marker hangs off to the left.
///
/// Detected shapes: rendered (`• `) and raw (`- `) bullets, either plus a task marker
/// (`[ ] `), ordered markers in raw (`1. `) and right-aligned rendered (` 1. `) form, and
/// a plain leading-whitespace continuation.  0 for anything else.
pub fn compute_hanging_indent(line: &Line<'_>) -> usize {
    let chars: Vec<char> = line.spans.iter().flat_map(|s| s.content.chars()).collect();
    compute_hanging_indent_chars(&chars)
}

/// [`compute_hanging_indent`] against raw buffer text, where no `Line` spans exist.
pub fn compute_hanging_indent_str(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    compute_hanging_indent_chars(&chars)
}

fn compute_hanging_indent_chars(chars: &[char]) -> usize {
    let mut i = 0;
    while i < chars.len() && chars[i] == ' ' {
        i += 1;
    }
    let leading = i;

    // Each blockquote level is a 2-cell prefix that hangs off continuation rows, where
    // the bar is repainted (see `leading_bar_prefix`).  Recurse so an inner list marker
    // after the bar(s) is aligned too.
    if blockquote_prefix_unit(&chars[i..]) {
        let after = i + 2;
        return 2 + compute_hanging_indent_chars(&chars[after..]);
    }

    if chars.get(i) == Some(&'•') && chars.get(i + 1) == Some(&' ') {
        return text_start_after_optional_task_prefix(chars, i + 2);
    }
    // Raw bullet: the cursor's raw-revealed list line inside `RenderedView`.  Indented
    // too, so its row stays aligned with the surrounding rendered list.
    if matches!(chars.get(i), Some('-') | Some('*') | Some('+')) && chars.get(i + 1) == Some(&' ') {
        return text_start_after_optional_task_prefix(chars, i + 2);
    }
    let digit_count = chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digit_count > 0
        && matches!(chars.get(i + digit_count), Some('.') | Some(')'))
        && chars.get(i + digit_count + 1) == Some(&' ')
    {
        return text_start_after_optional_task_prefix(chars, i + digit_count + 2);
    }

    // Continuation paragraph or otherwise-indented text: indenting at the leading-space
    // count keeps wrapped continuations flush with the indented body.
    if leading > 0 {
        return leading;
    }
    0
}

fn is_task_marker(chars: &[char], i: usize) -> bool {
    chars.get(i) == Some(&'[')
        && matches!(chars.get(i + 1), Some(' ') | Some('x') | Some('X'))
        && chars.get(i + 2) == Some(&']')
        && chars.get(i + 3) == Some(&' ')
}

fn text_start_after_optional_task_prefix(chars: &[char], pos: usize) -> usize {
    if is_task_marker(chars, pos) {
        pos + 4
    } else {
        pos
    }
}

/// One blockquote-bar unit: `▎` or `>` plus a space.  Nesting is handled by recursion in
/// [`compute_hanging_indent_chars`].
fn blockquote_prefix_unit(chars: &[char]) -> bool {
    matches!(chars.first(), Some('▎') | Some('>')) && chars.get(1) == Some(&' ')
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
    use ratatui::style::Color;
    use ratatui::text::Span;

    /// Highlight columns are chars; the painter places them in cells, so `a` after two wide
    /// glyphs sits at cell 4, and a highlighted wide glyph covers both its cells.
    #[test]
    fn patch_char_cols_places_char_columns_in_cells() {
        let sel = Style::default().bg(Color::Magenta);
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = TuiBuf::empty(area);
        patch_char_cols(&Line::from("日本ab"), &mut buf, area, 0, 1, 0, 1..3, sel);
        let bg = |x: u16| buf[(x, 0)].bg;
        assert_ne!(bg(1), Color::Magenta);
        assert_eq!(bg(2), Color::Magenta);
        assert_eq!(bg(3), Color::Magenta);
        assert_eq!(bg(4), Color::Magenta, "`a` is at cell 4");
        assert_ne!(bg(5), Color::Magenta);
    }

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
    fn hanging_indent_bullet() {
        let line = Line::from(vec![Span::raw("• "), Span::raw("foo bar")]);
        assert_eq!(compute_hanging_indent(&line), 2);
    }

    #[test]
    fn hanging_indent_raw_bullet() {
        let line = Line::from(vec![Span::raw("- foo bar")]);
        assert_eq!(compute_hanging_indent(&line), 2);
    }

    #[test]
    fn hanging_indent_ordered_single_digit() {
        let line = Line::from(vec![Span::raw("1. "), Span::raw("foo")]);
        assert_eq!(compute_hanging_indent(&line), 3);
    }

    #[test]
    fn hanging_indent_ordered_padded() {
        // ` 1. foo` — right-aligned single-digit when list reaches 10+.
        let line = Line::from(vec![Span::raw(" 1. "), Span::raw("foo")]);
        assert_eq!(compute_hanging_indent(&line), 4);
    }

    #[test]
    fn hanging_indent_ordered_double_digit() {
        let line = Line::from(vec![Span::raw("10. "), Span::raw("foo")]);
        assert_eq!(compute_hanging_indent(&line), 4);
    }

    #[test]
    fn hanging_indent_rendered_task_includes_bullet_and_checkbox() {
        // Tasks render as `• [ ] foo` — bullet + space + checkbox + space
        // = 6 cells of marker before the body text begins.
        let line = Line::from(vec![Span::raw("• [ ] "), Span::raw("foo")]);
        assert_eq!(compute_hanging_indent(&line), 6);
    }

    #[test]
    fn hanging_indent_task_raw_revealed() {
        // Cursor's raw line in Rendered view: `- [ ] foo`.
        let line = Line::from(vec![Span::raw("- [ ] foo")]);
        assert_eq!(compute_hanging_indent(&line), 6);
    }

    #[test]
    fn hanging_indent_nested_bullet() {
        // Outer bullet → child indent of 2 spaces, then nested bullet.
        let line = Line::from(vec![Span::raw("  • "), Span::raw("inner")]);
        assert_eq!(compute_hanging_indent(&line), 4);
    }

    #[test]
    fn hanging_indent_continuation_paragraph() {
        // List-item continuation paragraph: just leading spaces, no marker.
        let line = Line::from(vec![Span::raw("   "), Span::raw("more text")]);
        assert_eq!(compute_hanging_indent(&line), 3);
    }

    #[test]
    fn hanging_indent_plain_paragraph() {
        let line = Line::from(vec![Span::raw("Hello world")]);
        assert_eq!(compute_hanging_indent(&line), 0);
    }

    #[test]
    fn hanging_indent_blockquote() {
        // Rendered blockquote bar hangs off so wrapped quote text aligns
        // under the gutter (2 cells: glyph + space).
        let line = Line::from(vec![Span::raw("▎ "), Span::raw("quoted")]);
        assert_eq!(compute_hanging_indent(&line), 2);
    }

    #[test]
    fn hanging_indent_blockquote_raw_marker() {
        // The raw `> ` marker (cursor's quote line raw-revealed, and the
        // text the navigation side wraps) hangs off the same 2 cells, so
        // wrap budgets agree between the rendered bar and the raw source.
        assert_eq!(compute_hanging_indent_str("> quoted text"), 2);
    }

    #[test]
    fn hanging_indent_nested_blockquote() {
        // Two bar levels stack to a 4-cell hanging indent.
        let line = Line::from(vec![Span::raw("▎ ▎ "), Span::raw("quoted")]);
        assert_eq!(compute_hanging_indent(&line), 4);
    }

    #[test]
    fn hanging_indent_list_inside_blockquote() {
        // A bullet nested in a quote: bar (2) + bullet marker (2) = 4.
        let line = Line::from(vec![Span::raw("▎ • "), Span::raw("item")]);
        assert_eq!(compute_hanging_indent(&line), 4);
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
        let rows = render_line(&line, area, &mut buf, 0, true);
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

    #[test]
    fn visual_rows_with_indent_word_aligned() {
        let chars: Vec<(char, Style)> = "• hello world foo"
            .chars()
            .map(|c| (c, Style::default()))
            .collect();
        let rows = visual_rows_of_chars(&chars, 10, 2);
        assert_eq!(rows[0].0, 0);
        assert!(rows.len() >= 2);
        assert_eq!(rows.last().map(|r| r.1), Some(17));
    }

    #[test]
    fn visual_rows_with_indent_zero_matches_flat() {
        let s = "hello world foo";
        let chars: Vec<(char, Style)> = s.chars().map(|c| (c, Style::default())).collect();
        let with_indent = visual_rows_of_chars(&chars, 10, 0);
        let flat = visual_rows_of_str(s, 10);
        assert_eq!(with_indent, flat);
    }

    #[test]
    fn visual_rows_for_line_counts_indent_extra_rows() {
        // Indent 2 makes continuation rows narrower, so the row count can only grow.
        let line = Line::from(vec![Span::raw("• "), Span::raw("hello world foo bar baz")]);
        let with_marker = visual_rows_for_line(&line, 10);
        let line_flat = Line::from(vec![Span::raw("hello world foo bar baz")]);
        let flat = visual_rows_for_line(&line_flat, 10);
        assert!(with_marker >= flat);
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
    fn cursor_on_an_absorbed_space_paints_on_the_next_row() {
        let line = Line::from("abcdefghij klm");
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = TuiBuf::empty(area);
        let style = Style::default().fg(ratatui::style::Color::Red);
        let (_, cursor) =
            render_line_reporting_cursor(&line, area, &mut buf, 0, true, Some((10, style)), 0);
        assert_eq!(cursor, Some((0, 1)));
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
        let rows = visual_rows_of_chars(&chars, 4, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0, 2, 2));
        assert_eq!(rows[1], (2, 3, 3));
    }

    #[test]
    fn wrap_force_breaks_when_single_wide_char_exceeds_width() {
        // Width 1 can't fit a 2-cell emoji, but the loop must still make progress.
        let chars: Vec<(char, Style)> = "🥇🥇".chars().map(|c| (c, Style::default())).collect();
        let rows = visual_rows_of_chars(&chars, 1, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0, 1, 1));
        assert_eq!(rows[1], (1, 2, 2));
    }

    #[test]
    fn render_line_paints_wide_char_using_two_cells() {
        // The right-half cell of the wide char is left unwritten — terminals own it — so
        // the next char lands at column 3.
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = TuiBuf::empty(area);
        let line = Line::from(vec![Span::raw("A🥇B")]);
        render_line(&line, area, &mut buf, 0, false);
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

    #[test]
    fn zero_width_combining_mark_does_not_advance_cell_cursor() {
        // The combining mark has zero display width and must not consume a cell, so
        // column 1 holds '!' rather than a blank.
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = TuiBuf::empty(area);
        let line = Line::from(vec![Span::raw("e\u{0301}!")]);
        render_line(&line, area, &mut buf, 0, false);
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
