use ratatui::{buffer::Buffer as TuiBuf, layout::Rect, style::Style, text::Line};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Display width of `ch` in terminal cells.  Wide chars (CJK, most emoji)
/// return 2; ASCII / BMP narrow chars return 1; control chars return 0.
/// Used by both the renderer and the wrap-row calculator so on-screen
/// geometry agrees with cursor and selection coordinates.
pub fn char_cells(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Char index in `text` (0-based) corresponding to screen cell column
/// `target_cell`, assuming the row's first content cell sits at column
/// `indent` (>0 for hanging-indent continuation rows).  Implements the
/// landing rules used by vertical navigation and mouse clicks alike:
///
/// - **Forbidden indent zone:** when `target_cell <= indent` the cursor
///   lands at char index 0 — the first content char.  The hanging-indent
///   padding on a wrapped list-item continuation row is virtual, not
///   text, so the cursor never sits there.
/// - **Wide-char snap-past:** when `target_cell` falls *inside* a
///   multi-cell glyph (CJK, emoji), the cursor lands *after* that glyph.
///   It never visually sits in the right half of a wide char.
/// - **Past content:** when `target_cell` exceeds the row's total cell
///   width, returns the total char count of `text` (one past the last
///   char) — callers clamp to row end as appropriate.
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

/// Inverse of `char_idx_at_cell_col`: cumulative cell width of the first
/// `char_idx` chars of `iter`, plus `indent`.  Use this to seed
/// `preferred_col` after a horizontal cursor move so subsequent vertical
/// navigation lands at the same screen cell.
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

/// Largest `n` such that the cumulative cell width of
/// `chars[start..start + n]` fits within `cell_budget`.  When the very
/// first char is itself wider than `cell_budget` we still return `1` so
/// the wrap loop makes progress at very narrow viewports — the renderer
/// will clip the overflowing right half on draw.
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

/// Write a styled `Line` to the TUI buffer, wrapping at `area.width` when
/// `wrap` is true. Returns the number of visual rows consumed (≥ 1).
///
/// Trailing cells in every visual row are filled with the line's base style so
/// styled blocks (e.g. code blocks) extend to the full width of `area`.  The
/// wrap algorithm is word-aware: breaks prefer the last non-alphanumeric
/// character within the row, falling back to a hard break when a single word
/// exceeds the row width.  `area.width` is interpreted as terminal *cells*,
/// so wide chars (emoji, CJK) consume two columns of budget per char.
///
/// Hanging-indent: when the line begins with a recognized list marker (or
/// leading whitespace from a list-item continuation paragraph), wrapped
/// continuation rows are left-padded so their text aligns with the column
/// where the first row's text begins — the marker hangs off on the left.
/// Detection lives in `compute_hanging_indent`; an indent of 0 (the default
/// for non-list lines) preserves the legacy zero-padding wrap.
///
/// `cursor_col_override`: when `Some((col, style))`, the character at char
/// index `col` (NOT cell column) on the first output row is rendered as the
/// block cursor — the cell is recolored with `style` while the character stays
/// visible (used to show a cursor indicator during the jitter-suppression
/// delay in hybrid rendered mode).  The override applies only to the first
/// cell of a wide char — terminals can't independently style the right half.
/// Used by tests in this module; production code uses `render_line_from_visual`
/// to support sub-row scrolling.
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

/// Like [`render_line_with_cursor_from_visual`], but also returns the
/// absolute `(x, y)` cell where the cursor override was painted (`None`
/// when no override, or the override fell outside the drawn rows).  The
/// hybrid `RenderedView` uses the reported cell to re-stamp the cursor on
/// top of post-pass overlays (search-match highlights, selection washes)
/// that run after this widget and would otherwise bury it.
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

/// Raw-mode variant of [`render_line_with_cursor_from_visual`]: wraps with a
/// **flat** layout, never a hanging indent.
///
/// Raw mode shows the file, so the only liberty it takes with the source is
/// word-wrapping a line too long for the viewport; indenting the continuation
/// rows would draw leading whitespace that isn't in the document.  It is also
/// what the rest of Raw mode already assumes: `visual_rows_of_str` — which
/// backs the scroll cache (`EditorState::raw_line_at_visual_row`,
/// `raw_total_visual_rows`) and the click mapping
/// (`mouse_ops::coord::raw_click_to_offset`) — wraps at indent 0, so a
/// hanging indent here would put the painter in a different layout from the
/// scroll math (a differing *row count* at narrow widths, not just a column
/// shift) and offset every click on a continuation row by the marker width.
///
/// Passing indent 0 also suppresses the blockquote-bar repaint, which is
/// correct here for the same reason: in Raw mode the `> ` on the first row is
/// real source text, and the continuation rows have none.
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
/// [`render_raw_line_with_cursor`].  `hanging_indent` of `None` detects the
/// indent from the line's leading marker (`compute_hanging_indent`); `Some(n)`
/// forces it, which is how Raw mode asks for a flat wrap.
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

    // Collect all (char, style) pairs from the spans, resolving line-level style.
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

    // Single source of truth for row breaks — keeps the renderer in lockstep
    // with the navigation/selection helpers below.
    let indent = hanging_indent.unwrap_or_else(|| compute_hanging_indent(line));
    let rows = visual_rows_of_chars(&chars, width, indent);
    let effective_indent = if indent + 1 >= width { 0 } else { indent };
    // Blockquote bar to repaint on each wrapped continuation row so the gutter
    // doesn't vanish mid-quote; empty for non-blockquote lines.
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
        // A space the previous row's break absorbed owns no cell, so the
        // content loop would never paint a cursor resting on it.  Show it on
        // this row's first char — the same place `sub_line_of_col` reports.
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

/// Paint a single visual row.  `chars[start..end]` are written starting at
/// `area.x + row_indent` (after filling `row_indent` cells of hanging-indent
/// padding).  `abs_col_base` is the char-index offset to add to `rel_idx`
/// when matching against `cursor_col_override` — for wrapped continuation
/// rows this is the row's `start`; for the no-wrap fast path it's 0.
///
/// `cont_prefix` is the styled glyph run repainted at the start of the indent
/// zone (the blockquote `▎ ` bar — see [`leading_bar_prefix`]); any indent
/// cells beyond it are blank-filled in `line_style`.  It is empty for the
/// first row of a line and for non-blockquote continuations, so list-item
/// continuations keep their plain blank padding.
///
/// Returns the absolute `(x, y)` cell where the cursor override was drawn —
/// `None` when the override doesn't fall on this row.  Callers use the
/// reported cell to re-stamp the cursor on top of post-pass overlays
/// (search-match highlights, selection washes) that would otherwise bury it.
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
    // Indent zone: repaint the blockquote bar(s) so the gutter persists on
    // wrapped rows, then blank-fill the remainder (e.g. a list marker's width
    // when a list item inside a quote wraps) with the surrounding background.
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
            // Zero-width chars (e.g. ZWJ, variation selectors, combining
            // marks) are conceptually merged into the preceding grapheme
            // by the terminal — skip without advancing `x` rather than
            // overwriting the previous cell's glyph.
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
            // Block cursor: recolor the cell, leaving the char visible.
            cell.set_char(*ch);
            cell.set_style(cursor_style.unwrap_or(*style));
        }
        if cursor_style.is_some() {
            cursor_cell = Some((x, abs_y));
        }
        x += cells;
    }
    // End-of-line cursor: the override column sits one past the last char,
    // so the content loop above never reaches it.  Draw it on the first
    // trailing (blank) cell so an end-of-line cursor stays visible even when
    // the block is shown rendered rather than raw — e.g. while a search flow
    // suppresses the raw cursor-block reveal, or during the jitter
    // suppression delay.  Guard on `col == chars.len()` so a word-wrap gap
    // (whose trailing cells belong to the next row's content) never matches.
    let eol_cursor = cursor_col_override.filter(|&(col, _)| col == chars.len());
    let mut fill_col = abs_col_base + (end - start);
    while x < area_end {
        if let Some(cell) = buf.cell_mut((x, abs_y)) {
            if let Some((_, s)) = eol_cursor.filter(|&(col, _)| col == fill_col) {
                // Block cursor on the trailing blank cell.
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

/// Where the row after one ending at `end` begins.
///
/// Normally `end` — but when a lone space sits right at the break it would
/// open the next row as what reads like accidental indentation, so the break
/// absorbs it and the space belongs to no row at all (`next_start > end`;
/// see the row-tuple contract on `visual_rows_of_chars`).
///
/// Most soft breaks are already past their space — the break char *is* the
/// space, and it ends the row invisibly.  The ones that aren't are the reason
/// this is applied to every arm rather than only to the hard break: a row may
/// also end on a `.`, a `)` or an emoji cluster with the sentence's space
/// still to come.
///
/// Two spaces are never absorbed — interior whitespace is content
/// (`visual_rows_preserves_interior_whitespace_across_wrap`) — and neither is
/// a trailing one, which would leave the char with no following row to hold
/// the cursor.
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

/// Char indices in `chars` at which a grapheme cluster *starts*, as a mask
/// over `0..=chars.len()` (the past-the-end index is always a boundary).
///
/// A row must never end mid-cluster: the terminal draws a cluster as one
/// glyph, so splitting `👨\u{200d}👩\u{200d}👧\u{200d}👦` across two rows
/// leaves a partial family on each — which is exactly what the wrap did
/// before, since it reasoned in `char`s and a ZWJ is a perfectly ordinary
/// break candidate under "anything non-alphanumeric".
///
/// Returns `None` when every char stands alone, which is the common case:
/// ASCII has no multi-char clusters (`\r\n` aside, and a line has already
/// been split on `\n`).  The fast path matters because this function sits on
/// the per-keystroke navigation path as well as the paint path — segmenting
/// allocates, so plain text must not pay for it.
fn cluster_starts(chars: &[(char, Style)]) -> Option<Vec<bool>> {
    if chars.iter().all(|(ch, _)| ch.is_ascii()) {
        return None;
    }
    let text: String = chars.iter().map(|(ch, _)| *ch).collect();
    // Every consumer of the wrap layout addresses text by char index, so walk
    // the clusters and carry a running char count rather than materialising a
    // byte→char table — the segmenter's byte offsets are never needed.
    let mut starts = vec![false; chars.len() + 1];
    let mut char_idx = 0usize;
    for cluster in UnicodeSegmentation::graphemes(text.as_str(), true) {
        starts[char_idx] = true;
        char_idx += cluster.chars().count();
    }
    starts[chars.len()] = true;
    Some(starts)
}

/// Is char index `i` a grapheme-cluster boundary?  `None` (the all-ASCII
/// fast path) means every index is one.
fn is_cluster_boundary(clusters: Option<&[bool]>, i: usize) -> bool {
    clusters.is_none_or(|starts| starts.get(i).copied().unwrap_or(true))
}

/// Pull `end` back to the nearest cluster boundary at or before it, so a
/// hard break can't sever a cluster.  Never returns `start` itself — a row
/// holding a single cluster wider than the viewport must still make
/// progress, and the renderer clips the overflow.
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

/// Characters that never carry a wrap break even though they aren't
/// alphanumeric.  A no-break space is *defined* by not being a wrap point,
/// and code blocks pad their blank lines with U+00A0 (see the NBSP note in
/// `Renderer::render_code_block`) — breaking there would split padding the
/// renderer emits precisely to keep a row intact.
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

/// Punctuation that binds a token together when it sits *between* two
/// alphanumerics: contractions and possessives (`they're`, `it’s`), decimals
/// and thousands separators (`3.14`, `1,000`), clock times (`12:30`), file
/// names (`file.md`) and identifiers (`snake_case`).  Outside that sandwich
/// the same character is an ordinary break point, so a URL still breaks
/// after `//`, `?`, `#` and `&`.
///
/// `/` is deliberately **not** in the set.  It would keep `and/or` whole, but
/// it also strips every break point out of a URL path — the `/` in
/// `repo/blob/main` is between two alphanumerics just like the one in
/// `and/or` — leaving a long link to hard-break mid-segment at whatever
/// column the cell budget ran out on.  Links are far more common in Markdown
/// than `and/or`, and a path that wraps after a `/` reads better than one
/// severed mid-word, so the slash stays an ordinary break.
fn is_intra_word_punctuation(ch: char) -> bool {
    matches!(ch, '.' | ',' | ':' | '\'' | '\u{2019}' | '_')
}

/// May a visual row end with `chars[i]` — i.e. is a wrap break allowed
/// *after* that character?
///
/// The base rule is "anything non-alphanumeric", with three refinements that
/// keep tokens and punctuation pairs intact, over a grapheme-cluster gate
/// that no refinement can override.  Both neighbours matter, so this takes
/// the whole slice rather than a lone `char`.
fn is_break_after(chars: &[(char, Style)], i: usize, clusters: Option<&[bool]>) -> bool {
    // A break after `i` is only a break at all when the next char opens a new
    // cluster; otherwise `i` sits inside one (a ZWJ, a combining mark, a
    // regional-indicator pair) and the row would end mid-glyph.
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
    // `"` and `'` are ambiguous: opening when a word follows and none
    // precedes, closing otherwise (`'` between two words is an apostrophe,
    // covered by the intra-word rule below).
    if matches!(ch, '"' | '\'') && next_alnum && !prev_alnum {
        return false;
    }
    if is_intra_word_punctuation(ch) && prev_alnum && next_alnum {
        return false;
    }
    true
}

/// Does the row `chars[start..=break_at]` end with a one-letter word — an
/// `a` or `I` marooned at the right edge, away from the noun it belongs to?
/// Reported only when there is text before it on the row, since moving the
/// row's *first* word down would leave the row empty.
fn ends_with_lone_word(chars: &[(char, Style)], start: usize, break_at: usize) -> bool {
    if !chars[break_at].0.is_whitespace() || break_at < start + 2 {
        return false;
    }
    let word = break_at - 1;
    chars[word].0.is_alphanumeric() && word > start && chars[word - 1].0.is_whitespace()
}

/// Compute the list of visual rows produced by wrapping `chars` at `width`
/// (in terminal cells) with a hanging `indent`.  When `indent > 0`, the
/// first row uses the full `width` and every continuation row uses
/// `width - indent`.
///
/// Returns a list of `(start, end, next_start)` tuples, where:
/// - `chars[start..end]` is the content placed on that visual row
/// - `next_start` is the index at which the next visual row begins.  It is
///   normally equal to `end`; it is `end + 1` when the break absorbed the
///   single space that followed a mid-word hard break, so that space opens
///   no row of its own.  Chars in `end..next_start` therefore have no cell —
///   `sub_line_of_col` and the painter both show a cursor resting there at
///   the start of the following row.
///
/// `width` and `indent` are cell counts; the returned indices are char
/// indices.  `render_line_with_cursor` calls this directly to drive its
/// row-by-row painting, so the renderer and visual-line navigation always
/// agree on where rows break.
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
    // If the indent leaves no room on continuation rows, ignore it — matches
    // `render_line_with_cursor`'s fallback so wrap-row counts stay in sync.
    let indent = if indent + 1 >= width { 0 } else { indent };

    // One segmentation pass for the whole line, shared by every row.
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
            // The cell budget lands wherever it lands; pull it back so a
            // hard break falls between clusters rather than inside one.
            let window_end = snap_to_cluster_boundary(clusters, start, start + n_chars);
            let break_at = (start..window_end)
                .rev()
                .find(|&i| is_break_after(chars, i, clusters));

            let end = match break_at {
                Some(bp) => {
                    // A break that strands a one-letter word at the row edge
                    // backs up to the break before that word, carrying it
                    // down to sit with the noun it belongs to.
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
                // Nothing in the window may carry a break — a single long
                // word, or one cluster wider than the row.
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

/// Compute the list of visual rows produced by wrapping a plain string `text`
/// at `width`.  Convenience wrapper around `visual_rows_of_chars` used by the
/// cursor/navigation code which operates on RAW buffer text — no hanging
/// indent applies (the cursor's raw line in Rendered mode and every line in
/// Raw mode follow the source layout, not the rendered layout).
pub fn visual_rows_of_str(text: &str, width: usize) -> Vec<(usize, usize, usize)> {
    let chars: Vec<(char, Style)> = text.chars().map(|c| (c, Style::default())).collect();
    visual_rows_of_chars(&chars, width, 0)
}

/// Number of visual rows a styled `Line` occupies when wrapped at `width`.
///
/// Mirrors the wrap algorithm used by `render_line`, so scroll-bound
/// calculations can match what the viewport actually draws.  Empty lines
/// always consume a single row.  Detects the line's hanging indent the same
/// way `render_line` does — list-item continuations consume more rows when
/// indented than they would under flat wrap.
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

/// Hanging-indent (in cells) for the given rendered `Line`.
///
/// The indent is the column where the first character after the list marker
/// begins on the first visual row — wrapped continuation rows are then
/// left-padded by this amount so the wrapped text aligns with the first
/// line's text column and the marker visually hangs off to its left.
///
/// Detected shapes:
///
/// 1. Rendered bullet:           `• text`           → indent = leading_ws + 2
/// 2. Raw bullet (raw-revealed): `- text`           → indent = leading_ws + 2
/// 3. Bullet + task:             `- [ ] text`       → indent = leading_ws + 6
/// 4. Ordered (rendered/raw):    `1. text`/` 1. `   → indent = leading_ws + digit_width + 2
/// 5. Continuation paragraph:    `   text`          → indent = leading_ws
///
/// Returns 0 for lines that don't match any of these shapes (plain
/// paragraphs, blockquoted content, table rows, code blocks, etc.).
pub fn compute_hanging_indent(line: &Line<'_>) -> usize {
    let chars: Vec<char> = line.spans.iter().flat_map(|s| s.content.chars()).collect();
    compute_hanging_indent_chars(&chars)
}

/// String-based variant of [`compute_hanging_indent`] for use against the
/// raw buffer text (where there are no `Line` spans available, e.g. inside
/// `EditorState::move_up_visual` / `move_down_visual`).  Same detection
/// rules — see that function for the recognized marker shapes.
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

    // Blockquote bar prefix: the rendered `▎ ` gutter, or the raw `> ` marker
    // shown when the cursor's quote line is raw-revealed.  Each level is a
    // 2-cell prefix that must hang off wrapped continuation rows — the bar is
    // repainted there (see `leading_bar_prefix`) so the gutter persists, and
    // the wrap budget matches the navigation side, which sees the raw `> `.
    // After the bar(s) an inner list marker may follow, so recurse on the
    // remainder to keep a wrapped list item inside a quote aligned too.
    if blockquote_prefix_unit(&chars[i..]) {
        let after = i + 2;
        return 2 + compute_hanging_indent_chars(&chars[after..]);
    }

    // Rendered bullet glyph (`•`) — only emitted by the renderer.
    if chars.get(i) == Some(&'•') && chars.get(i + 1) == Some(&' ') {
        return text_start_after_optional_task_prefix(chars, i + 2);
    }
    // Raw bullet glyph (`-`, `*`, `+`) — used when the cursor's list-item
    // line is shown raw inside the otherwise-rendered `RenderedView`.  We
    // hang-indent it too so the cursor's row stays visually aligned with
    // the surrounding rendered list.
    if matches!(chars.get(i), Some('-') | Some('*') | Some('+')) && chars.get(i + 1) == Some(&' ') {
        return text_start_after_optional_task_prefix(chars, i + 2);
    }
    // Ordered marker: digits + `.`/`)` + space.  Matches both the raw form
    // (`1. `) and the rendered right-aligned form (` 1. `).
    let digit_count = chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digit_count > 0
        && matches!(chars.get(i + digit_count), Some('.') | Some(')'))
        && chars.get(i + digit_count + 1) == Some(&' ')
    {
        return text_start_after_optional_task_prefix(chars, i + digit_count + 2);
    }

    // Continuation paragraph or otherwise-indented text (e.g. list-item
    // child block, indented heading).  Hanging-indent at the leading-space
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

/// True when `chars` begins with one blockquote-bar unit: the rendered gutter
/// glyph `▎` or the raw `>` marker, followed by a space.  Each unit is two
/// cells wide; nesting (`▎ ▎ `, `> > `) is handled by recursion in
/// [`compute_hanging_indent_chars`].
fn blockquote_prefix_unit(chars: &[char]) -> bool {
    matches!(chars.first(), Some('▎') | Some('>')) && chars.get(1) == Some(&' ')
}

/// The leading run of rendered blockquote bar glyphs (`▎ ` units, with their
/// styles) at the start of a styled line.  Repainted into the hanging-indent
/// zone of each wrapped continuation row so the quote gutter persists across
/// the wrap.  Only the rendered `▎` glyph is captured — a raw-revealed `> `
/// line is literal source and gets plain blank padding on continuation rows.
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

/// The highest char column of the logical line that the cursor may occupy
/// while still rendering on visual row `row`.
///
/// The *last* row owns the one-past-the-end slot, so an end-of-line cursor
/// can sit on its trailing blank cell.  Every other row must stop one char
/// short of `end`: column `end` is already the next row's first char, and it
/// renders at that row's column 0 — a cursor clamped there looks like it
/// never left the row below, which makes Up appear stuck.
///
/// **Clamp against `end`, never against `next_start`.** The two are equal
/// for an ordinary wrap, but a hard break that absorbed the following space
/// leaves `next_start > end` (see `visual_rows_of_chars`), and the chars in
/// between own no cell at all — clamping to `next_start - 1` lands the
/// cursor on the absorbed space, which paints at the next row's column 0.
/// That is the same failure the last-row/other-row split exists to prevent,
/// so this is the single derivation all four click- and navigation-mapping
/// sites share.
pub fn last_col_in_row(row: (usize, usize, usize), is_last_row: bool) -> usize {
    let (start, end, _) = row;
    if is_last_row {
        end
    } else {
        end.saturating_sub(1).max(start)
    }
}

/// Given the visual-row layout of a line and a raw char column, return
/// `(sub_line_idx, visual_col)` — which visual row the char is on, and its
/// visual column within that row (0-based).
///
/// Cursor positions at end-of-line or within a wrap-point skip zone are mapped
/// to the nearest visible row at its end column.
pub fn sub_line_of_col(rows: &[(usize, usize, usize)], raw_col: usize) -> (usize, usize) {
    for (i, &(s, e, n)) in rows.iter().enumerate() {
        if raw_col < n {
            // A space absorbed by the wrap (`end..next_start`) has no cell on
            // this row.  Report the next row's first column instead of this
            // row's phantom one past the edge, so the cursor stays visible.
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
        // Continuation row 1 also starts with the bar glyph + space, then text.
        assert_eq!(
            buf.cell((0, 1)).map(|c| c.symbol().to_string()),
            Some("▎".into())
        );
        assert_eq!(
            buf.cell((1, 1)).map(|c| c.symbol().to_string()),
            Some(" ".into())
        );
        // The wrapped text begins at the indent column (2), not column 0.
        assert_ne!(
            buf.cell((2, 1)).map(|c| c.symbol().to_string()),
            Some(" ".into())
        );
    }

    #[test]
    fn visual_rows_with_indent_word_aligned() {
        // First row holds "• hello " (8 chars), continuation row has width
        // 10 - 2 = 8 for indented body.  "world foo" fits in 9 chars, so
        // the wrap places "world " on row 2 and "foo" on row 3.
        let chars: Vec<(char, Style)> = "• hello world foo"
            .chars()
            .map(|c| (c, Style::default()))
            .collect();
        let rows = visual_rows_of_chars(&chars, 10, 2);
        // Row 0 width 10: breaks at the space after "hello", giving
        // "• hello " (8 chars) before consuming the trailing space.
        assert_eq!(rows[0].0, 0);
        // Row 1 width 8: "world " then break.
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
        // Bullet line wrapped at width 10 — indent = 2, so continuation rows
        // are 8 chars wide.  Total visual rows must be ≥ flat count.
        let line = Line::from(vec![Span::raw("• "), Span::raw("hello world foo bar baz")]);
        let with_marker = visual_rows_for_line(&line, 10);
        let line_flat = Line::from(vec![Span::raw("hello world foo bar baz")]);
        let flat = visual_rows_for_line(&line_flat, 10);
        assert!(with_marker >= flat);
    }

    #[test]
    fn visual_rows_preserves_interior_whitespace_across_wrap() {
        // "a              b" at width 5: the wrap happens after "a    "
        // (5 chars).  The remaining spaces before "b" must NOT be swallowed.
        let rows = visual_rows_of_str("a              b", 5);
        // Row 0: "a    " (indices 0..5)
        assert_eq!(rows[0], (0, 5, 5));
        // Row 1: "     " (indices 5..10)
        assert_eq!(rows[1], (5, 10, 10));
        // Row 2: "     " (indices 10..15)
        assert_eq!(rows[2], (10, 15, 15));
        // Row 3: "b" (index 15..16)
        assert_eq!(rows[3], (15, 16, 16));
    }

    // ── Break-candidate refinements ───────────────────────────────

    #[test]
    fn contraction_apostrophe_is_not_a_break_point() {
        // Width 12 fits "when they'r"; the apostrophe must not be taken as a
        // break, so the whole word moves down and the row ends at the space.
        let rows = visual_rows_of_str("when they're here", 12);
        assert_eq!(rows[0], (0, 5, 5)); // "when "
        assert_eq!(rows[1], (5, 17, 17)); // "they're here"
    }

    #[test]
    fn smart_apostrophe_is_not_a_break_point() {
        // Rendered text carries U+2019, not ASCII \', because smart
        // punctuation is enabled in the parser.
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
        // The intra-word rule needs alphanumerics on both sides, so `//`
        // stays a break point even though `example.com` no longer is.
        let rows = visual_rows_of_str("see https://example.com/x", 20);
        assert_eq!(rows[0], (0, 12, 12)); // "see https://"
    }

    #[test]
    fn a_url_path_breaks_at_a_slash_rather_than_mid_segment() {
        // `/` is not intra-word punctuation, so a long path still has break
        // points inside it.  With `/` in that set every slash here sits
        // between two alphanumerics, the whole path becomes one unbreakable
        // token, and the row hard-breaks at whatever column the cell budget
        // happened to run out on.
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
        // Width 10 fits `a note (rem`; breaking after `(` would leave the
        // paren hanging alone at the row edge.
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
        // Width 14 fits "tell them a "; the lone "a" moves down with "story".
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
        // "abcdefghij" fills the row exactly; the space after it must not
        // open the next row as visible indentation.
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
        // Nothing follows it, so absorbing would drop the char entirely and
        // leave no row for the cursor.
        let rows = visual_rows_of_str("abcdefghij ", 10);
        assert_eq!(rows[0], (0, 10, 10));
        assert_eq!(rows[1], (10, 11, 11));
    }

    #[test]
    fn last_col_in_row_clamps_against_end_not_next_start() {
        let rows = visual_rows_of_str("abcdefghij klm", 10);
        assert_eq!(rows[0], (0, 10, 11));
        // Row 0 absorbed the space at char 10, so the cursor's last legal
        // column there is char 9 — not `next_start - 1`, which *is* the
        // absorbed space and renders at row 1's column 0.
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
        // The row ends on `.`, so the sentence space is still to come — it
        // would open the next row as visible indentation.
        let rows = visual_rows_of_str("abcde. fgh", 6);
        assert_eq!(rows[0], (0, 6, 7));
        assert_eq!(rows[1], (7, 10, 10));
    }

    // ── Grapheme clusters ─────────────────────────────────────────

    #[test]
    fn a_zwj_sequence_is_never_split_across_rows() {
        // The family emoji is 7 chars (4 emoji + 3 ZWJ) drawn as one glyph.
        // The ZWJ used to be an ordinary break candidate — non-alphanumeric —
        // so the row ended mid-family.
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
        // "e" + U+0301 is one cluster; a break between them would strand the
        // accent at the head of the next row.
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
        // Nothing can keep it whole, so the wrap falls back to a hard break
        // rather than emitting an empty row and looping forever.
        let text = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let rows = visual_rows_of_str(text, 3);
        assert!(rows.len() > 1);
        assert!(rows.iter().all(|&(s, e, _)| e > s));
    }

    #[test]
    fn ascii_text_takes_the_no_segmentation_fast_path() {
        // Not a behavior assertion so much as a guard on the fast path's
        // premise: an all-ASCII line has no multi-char clusters, so the
        // layout must be identical either way.
        let chars: Vec<(char, Style)> = "hello world foo bar"
            .chars()
            .map(|c| (c, Style::default()))
            .collect();
        assert!(cluster_starts(&chars).is_none());
    }

    // ── Cell-width awareness ──────────────────────────────────────

    #[test]
    fn wrap_budget_is_cells_not_chars_for_wide_chars() {
        // Each emoji is 1 char / 2 cells.  At width 4, two emoji fill the
        // first row exactly; the third spills onto row 2.
        let chars: Vec<(char, Style)> = "🥇🥇🥇".chars().map(|c| (c, Style::default())).collect();
        let rows = visual_rows_of_chars(&chars, 4, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0, 2, 2));
        assert_eq!(rows[1], (2, 3, 3));
    }

    #[test]
    fn wrap_force_breaks_when_single_wide_char_exceeds_width() {
        // Width 1 can't fit a 2-cell emoji, but the wrap loop must still
        // make progress — emit one char per row even though they overflow.
        let chars: Vec<(char, Style)> = "🥇🥇".chars().map(|c| (c, Style::default())).collect();
        let rows = visual_rows_of_chars(&chars, 1, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0, 1, 1));
        assert_eq!(rows[1], (1, 2, 2));
    }

    #[test]
    fn render_line_paints_wide_char_using_two_cells() {
        // After painting "A🥇B" the cells must read 'A', '🥇', <skipped>, 'B'.
        // The renderer leaves the right-half cell of the wide char unwritten
        // — terminals own that half.  The next char must land at column 3.
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
        // Continuation row of a wrapped list item: indent = 2.  Clicks on
        // cells 0 and 1 land in the virtual padding; cells 2+ are content.
        // All cells in [0..=indent] must snap to char index 0 — the first
        // content char of the row.
        let chars = ['x', 'y', 'z'];
        for cell in 0..=2 {
            assert_eq!(
                char_idx_at_cell_col(chars.iter().copied(), cell, 2),
                0,
                "indent zone cell {cell} did not snap to row start",
            );
        }
        // Cell 3 is the first content cell — lands on 'x' (index 0 in the
        // row, since acc starts at indent=2 and the first char fills cell 2).
        // Wait: walk acc=2, ch='x', w=1, acc+w=3 NOT > 3, acc=3.  Then
        // ch='y', w=1, acc+w=4 > 3, acc==3==target → return count=1.
        assert_eq!(char_idx_at_cell_col(chars.iter().copied(), 3, 2), 1);
    }

    #[test]
    fn char_idx_at_cell_col_snaps_past_wide_char() {
        // 🥇 occupies cells 0–1.  Targeting cell 1 (mid-glyph) must snap
        // past, returning index 1 (cursor *after* the emoji).  Targeting
        // cell 0 (the glyph's start) returns index 0 (cursor before).
        let chars = ['🥇', 'B'];
        assert_eq!(char_idx_at_cell_col(chars.iter().copied(), 0, 0), 0);
        assert_eq!(char_idx_at_cell_col(chars.iter().copied(), 1, 0), 1);
        assert_eq!(char_idx_at_cell_col(chars.iter().copied(), 2, 0), 1);
    }

    #[test]
    fn cell_col_at_char_idx_round_trips_with_wide_chars() {
        let chars = ['A', '🥇', 'B'];
        // Char 0 → cell 0; char 1 → cell 1 (after A); char 2 → cell 3
        // (past A and the wide emoji); char 3 → cell 4.
        assert_eq!(cell_col_at_char_idx(chars.iter().copied(), 0, 0), 0);
        assert_eq!(cell_col_at_char_idx(chars.iter().copied(), 1, 0), 1);
        assert_eq!(cell_col_at_char_idx(chars.iter().copied(), 2, 0), 3);
        assert_eq!(cell_col_at_char_idx(chars.iter().copied(), 3, 0), 4);
    }

    #[test]
    fn zero_width_combining_mark_does_not_advance_cell_cursor() {
        // 'e' + U+0301 ("é") — the combining mark has zero display width;
        // it must not consume a cell on its own.  After painting, column 1
        // is the next character ('!'), not blank.
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
