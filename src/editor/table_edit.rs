//! GFM table detection, parsing, and structure-editing primitives: which cell the cursor is in,
//! cell navigation, and row/column insert / delete / move.
//!
//! Every structure edit is a single `EditDelta`, so it undoes as one step.
//!
//! Which lines form a table, and where each row's content starts past its container prefix, come
//! from the parse ([`row_map::table_lines`](crate::document::row_map::table_lines)), so a table in
//! a quote, a list item or a footnote edits as a top-level one does.  The rows themselves are split
//! here, from the buffer's text, as GFM splits them ([`table_layout::raw_cells`]): edge pipes
//! optional, a row's cell count free to differ from the header's.  Rewriting a row keeps its
//! prefix and its edge pipes ([`rebuild_row`]).

use crate::document::EditDelta;
use crate::editor::list_edit;
use crate::markdown::ast::{to_u32, Block};
use crate::markdown::parser::parse_document;
use crate::markdown::table_layout;

// ─── Types ───────────────────────────────────────────────────────────────────

/// Parsed view of a Markdown table found in the source buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    /// Byte offset of the first character of the header row in the buffer.
    pub start: usize,
    /// Byte offset just past the last `\n` of the final row.
    pub end: usize,
    /// Rows in source order: header, alignment, then data rows.
    pub rows: Vec<TableRow>,
    /// Number of columns (from the alignment row).
    pub col_count: usize,
    /// Per row, the char column its content starts at, as [`table_from_lines`] took it; kept so
    /// [`Self::reparse`] can re-split the same rows after an edit that leaves their prefixes be.
    content_cols: Vec<Option<usize>>,
}

/// A single physical line of a table (one row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRow {
    /// Byte offset of the first char of the row line.
    pub start: usize,
    /// Byte offset just past the trailing `\n` (or end-of-buffer for the last
    /// row when there is no trailing newline).
    pub end: usize,
    /// Raw text of the row, excluding the trailing newline.
    pub raw: String,
    /// Per-cell information, as GFM splits the row: `cells.len() == col_count` for a full row,
    /// fewer or more for a short or long one.
    pub cells: Vec<TableCell>,
    pub kind: RowKind,
    /// Byte length of the text before the row's cells: its container prefix (an item's indent,
    /// a quote's `> `, a footnote's label), then any space up to the opening `|` or, without
    /// one, the first cell.
    prefix_len: usize,
    /// Whether the row has an opening and a closing `|`; GFM makes both optional.
    lead_pipe: bool,
    trail_pipe: bool,
}

/// A single cell's content range, relative to the start of the row's `raw` string (not the
/// buffer) and inclusive of padding spaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCell {
    /// Byte offset within `raw` of the cell's first char: just past its opening `|`, or for a
    /// first cell without one, its first non-blank char.
    pub content_start: usize,
    /// Byte offset within `raw` just past the cell: its closing `|`, or for a last cell without
    /// one, the end of the row's text.
    pub content_end: usize,
    /// The cell as it appears in the raw line, padding and escaped `\|` included.
    pub raw: String,
}

impl TableCell {
    /// Content with surrounding whitespace stripped, for rebuilding a modified row.
    #[allow(dead_code)]
    pub fn trimmed(&self) -> &str {
        self.raw.trim()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Header,
    Alignment,
    Data,
}

// ─── Detection ───────────────────────────────────────────────────────────────

/// The table whose lines start at byte `start` of `source`, one line per entry of
/// `content_cols`: the header, the delimiter row, then the data rows, each with the char column
/// its content starts at ([`row_map::table_lines`](crate::document::row_map::table_lines)).  A
/// `None` column (the delimiter row, which the parse records as chrome) is read off the line: a
/// continuation line's prefix is only quote markers and indent.  `None` when the lines no longer
/// read as a table, as after an in-line edit to the delimiter row the parse hasn't caught up with.
pub fn table_from_lines(
    source: &str,
    start: usize,
    content_cols: &[Option<usize>],
) -> Option<TableInfo> {
    let bytes = source.as_bytes();
    let mut rows = Vec::with_capacity(content_cols.len());
    let mut at = start;
    for (i, &col) in content_cols.iter().enumerate() {
        if at > source.len() || (i > 0 && at == source.len()) {
            return None; // fewer lines than the parse saw
        }
        let line_end = line_end_byte(bytes, at);
        let end = if line_end < source.len() {
            line_end + 1
        } else {
            line_end
        };
        let raw = &source[at..line_end];
        let kind = match i {
            0 => RowKind::Header,
            1 => RowKind::Alignment,
            _ => RowKind::Data,
        };
        let col = col.unwrap_or_else(|| {
            raw.chars()
                .take_while(|c| matches!(c, ' ' | '\t' | '>'))
                .count()
        });
        rows.push(parse_row(raw, at, end, col, kind));
        at = end;
    }
    if rows.len() < 2 || !is_alignment_row(&rows[1]) {
        return None;
    }
    let col_count = rows[1].cells.len();
    Some(TableInfo {
        start,
        end: at,
        rows,
        col_count,
        content_cols: content_cols.to_vec(),
    })
}

/// The GFM table containing `cursor_byte` of `source`, located by parsing `source` whole.  The
/// editor locates through its live parse instead
/// ([`table_edit_ops::locate_table`](crate::editor::table_edit_ops::locate_table)).
#[cfg(test)]
pub(crate) fn find_table_at(source: &str, cursor_byte: usize) -> Option<TableInfo> {
    use crate::document::{row_map, ParsedDoc};
    let parsed = ParsedDoc::build(source, &crate::config::Theme::default(), false, 10);
    let line = parsed.byte_to_line(cursor_byte);
    let (first, cols) = row_map::table_lines(&parsed, line)?;
    table_from_lines(source, parsed.line_start_byte(first), &cols)
}

impl TableInfo {
    /// The same rows re-split from `source`, an edit of the text this table was found in that
    /// kept every row's line and prefix: a column swap.  `None` when they no longer read as a
    /// table.
    pub fn reparse(&self, source: &str) -> Option<TableInfo> {
        table_from_lines(source, self.start, &self.content_cols)
    }

    /// The last column a cursor can reach on row `row_idx`: the row's last cell, short of the
    /// table's column count.  A short row's missing cells hold no text, so Tab skips them.
    pub fn last_col(&self, row_idx: usize) -> usize {
        self.rows.get(row_idx).map_or(0, |row| {
            row.cells.len().min(self.col_count).saturating_sub(1)
        })
    }
}

/// The cursor's `(row_idx, col_idx)` within a table, or `None` when `cursor_byte` falls outside
/// it.
pub fn cursor_cell(info: &TableInfo, cursor_byte: usize) -> Option<(usize, usize)> {
    for (i, row) in info.rows.iter().enumerate() {
        if cursor_byte >= row.start && cursor_byte < row.end {
            return Some((i, row.column_at(cursor_byte - row.start, info.col_count)));
        }
    }
    // Cursor may be at the very end of the table (past the final newline).
    if !info.rows.is_empty() && cursor_byte == info.end {
        let last = info.rows.len() - 1;
        let last_row = &info.rows[last];
        return Some((
            last,
            last_row
                .cells
                .len()
                .saturating_sub(1)
                .min(info.col_count.saturating_sub(1)),
        ));
    }
    None
}

/// Where the cursor lands when jumping into a cell: the first byte of its content, past the one
/// leading padding space.
pub fn cell_cursor_offset(info: &TableInfo, row_idx: usize, col_idx: usize) -> Option<usize> {
    let row = info.rows.get(row_idx)?;
    let col = col_idx.min(info.last_col(row_idx));
    let cell = row.cells.get(col)?;
    let mut offset_in_raw = cell.content_start;
    if row.raw.as_bytes().get(offset_in_raw) == Some(&b' ') {
        offset_in_raw += 1;
    }
    Some(row.start + offset_in_raw)
}

/// Where the cursor lands when entering a cell from above or below: just past its last
/// non-whitespace character, falling back to [`cell_cursor_offset`] for an empty cell.
pub fn cell_end_cursor_offset(info: &TableInfo, row_idx: usize, col_idx: usize) -> Option<usize> {
    let row = info.rows.get(row_idx)?;
    let col = col_idx.min(info.last_col(row_idx));
    let cell = row.cells.get(col)?;
    let trimmed_len = cell.raw.trim_end().len();
    let offset_in_raw = if trimmed_len > 0 {
        cell.content_start + trimmed_len
    } else {
        let mut o = cell.content_start;
        if row.raw.as_bytes().get(o) == Some(&b' ') {
            o += 1;
        }
        o
    };
    Some(row.start + offset_in_raw)
}

// ─── Structure edits ─────────────────────────────────────────────────────────

/// Insert a new empty row above or below `row_idx`.  Place the cursor afterwards with
/// [`cell_cursor_offset`].
pub fn insert_row(info: &TableInfo, row_idx: usize, below: bool) -> (EditDelta, usize) {
    let target_idx = if below { row_idx + 1 } else { row_idx };
    // The alignment row must stay at index 1, so an earlier target inserts just after it.
    let target_idx = target_idx.max(2).min(info.rows.len());

    // The row above (the alignment row at the earliest) supplies the container indent.
    let new_row = format!(
        "{}{}",
        info.rows[target_idx - 1].prefix(),
        empty_row_text(info.col_count)
    );
    let offset = if target_idx < info.rows.len() {
        info.rows[target_idx].start
    } else {
        info.end
    };

    // Every row ends with `\n` except possibly the last, when the buffer has no trailing
    // newline; there, prepend one to the new row instead.
    let needs_newline_before = target_idx == info.rows.len()
        && info
            .rows
            .last()
            .map(|r| !r.raw_ends_with_newline())
            .unwrap_or(false);
    let inserted = if needs_newline_before {
        format!("\n{new_row}")
    } else {
        new_row
    };

    let delta = EditDelta {
        offset,
        removed: String::new(),
        inserted,
    };
    (delta, target_idx)
}

/// Delete the row at `row_idx`.  The alignment row and the header may not be
/// deleted; the call is a no-op (returns `None`) in that case.
pub fn delete_row(info: &TableInfo, row_idx: usize) -> Option<EditDelta> {
    if row_idx < 2 {
        return None; // can't delete header or alignment row
    }
    let row = info.rows.get(row_idx)?;
    Some(EditDelta {
        offset: row.start,
        removed: raw_with_newline(info, row_idx),
        inserted: String::new(),
    })
}

/// Swap two adjacent data rows (index `2..`).  The caller updates the cursor.
pub fn swap_rows(info: &TableInfo, a: usize, b: usize) -> Option<EditDelta> {
    if a == b {
        return None;
    }
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    if lo < 2 || hi >= info.rows.len() {
        return None; // can't touch header/alignment
    }
    if hi != lo + 1 {
        return None; // only adjacent swaps supported
    }

    let row_lo = &info.rows[lo];
    let row_hi = &info.rows[hi];
    let start = row_lo.start;
    let end = row_hi.end;
    let removed: String = info.rows[lo..=hi].iter().map(format_row_with_nl).collect();
    let inserted: String = [hi, lo]
        .into_iter()
        .map(|idx| format_row_with_nl(&info.rows[idx]))
        .collect();
    // A final row with no newline must stay that way — `format_row_with_nl` preserves it.
    let _ = (start, end, row_lo);

    Some(EditDelta {
        offset: row_lo.start,
        removed,
        inserted,
    })
}

/// Insert a new empty column adjacent to `col_idx`, rewriting every row.  The alignment row's
/// new cell uses `---` (left-align).
pub fn insert_column(info: &TableInfo, col_idx: usize, right: bool) -> EditDelta {
    let target_col = if right { col_idx + 1 } else { col_idx };
    let target_col = target_col.min(info.col_count);

    let removed = collect_raw(info);
    let mut inserted = String::with_capacity(removed.len() + 16);

    for row in &info.rows {
        let new_cells = insert_blank_cell(&row.padded_cells(), target_col, row.kind);
        inserted.push_str(&rebuild_row(row, &new_cells));
        if row.raw_ends_with_newline_or_next_exists(info) {
            inserted.push('\n');
        }
    }

    EditDelta {
        offset: info.start,
        removed,
        inserted,
    }
}

/// Delete the column at `col_idx`.  Every row loses one cell.
pub fn delete_column(info: &TableInfo, col_idx: usize) -> Option<EditDelta> {
    if info.col_count <= 1 {
        return None; // refuse to delete the last remaining column
    }
    if col_idx >= info.col_count {
        return None;
    }

    let removed = collect_raw(info);
    let mut inserted = String::with_capacity(removed.len());

    for row in &info.rows {
        let mut new_cells = row.padded_cells();
        if col_idx < new_cells.len() {
            new_cells.remove(col_idx);
        }
        inserted.push_str(&rebuild_row(row, &new_cells));
        if row.raw_ends_with_newline_or_next_exists(info) {
            inserted.push('\n');
        }
    }

    Some(EditDelta {
        offset: info.start,
        removed,
        inserted,
    })
}

/// Swap two adjacent columns.
pub fn swap_columns(info: &TableInfo, a: usize, b: usize) -> Option<EditDelta> {
    if a == b {
        return None;
    }
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    if hi >= info.col_count {
        return None;
    }
    if hi != lo + 1 {
        return None;
    }

    let removed = collect_raw(info);
    let mut inserted = String::with_capacity(removed.len());

    for row in &info.rows {
        let mut new_cells = row.padded_cells();
        if lo < new_cells.len() {
            // A short row lacking column `hi` gets it, blank, so its `lo` cell moves with
            // the header rather than staying under the column that moved in.
            if hi == new_cells.len() {
                new_cells = insert_blank_cell(&new_cells, hi, row.kind);
            }
            new_cells.swap(lo, hi);
        }
        inserted.push_str(&rebuild_row(row, &new_cells));
        if row.raw_ends_with_newline_or_next_exists(info) {
            inserted.push('\n');
        }
    }

    Some(EditDelta {
        offset: info.start,
        removed,
        inserted,
    })
}

// ─── Column-width persistence ────────────────────────────────────────────────

/// Insert or replace the `<!-- tui-columns: [..] -->` comment row immediately after the table,
/// as one `EditDelta` so the resize and the comment update undo together.
pub fn write_column_widths(source: &str, info: &TableInfo, widths: &[Option<usize>]) -> EditDelta {
    // Indented like the table's last row, so a table inside a list item keeps its comment in the
    // item.  Not the header's: its prefix may hold the item's marker or a footnote's label.
    let mut comment = format!(
        "{}{}",
        info.rows.last().map_or("", TableRow::prefix),
        table_layout::format_column_widths_comment(widths)
    );
    comment.push('\n');

    if let Some(existing) = find_existing_widths_comment(source, info.end) {
        let removed_end = advance_past_one_newline(source, existing.end);
        EditDelta {
            offset: existing.start,
            removed: source[existing.start..removed_end].to_owned(),
            inserted: comment,
        }
    } else {
        // `info.end` is already past the last row's trailing `\n`, so this starts a fresh line.
        EditDelta {
            offset: info.end,
            removed: String::new(),
            inserted: comment,
        }
    }
}

/// Byte range (excluding the trailing `\n`) of a `<!-- tui-columns: [..] -->` line at exactly
/// `search_start`.  Intervening blank lines are deliberately not skipped: the comment must be the
/// line immediately after the table for the round-trip parser to pair it correctly.
fn find_existing_widths_comment(
    source: &str,
    search_start: usize,
) -> Option<std::ops::Range<usize>> {
    let len = source.len();
    if search_start >= len {
        return None;
    }
    let line_end = source.as_bytes()[search_start..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| search_start + i)
        .unwrap_or(len);
    let line = &source[search_start..line_end];
    if table_layout::parse_column_widths_comment(line).is_some() {
        Some(search_start..line_end)
    } else {
        None
    }
}

fn advance_past_one_newline(source: &str, pos: usize) -> usize {
    if source.as_bytes().get(pos) == Some(&b'\n') {
        pos + 1
    } else {
        pos
    }
}

// ─── Row text helpers ────────────────────────────────────────────────────────

impl TableRow {
    /// The text before the row's cells ([`Self::prefix_len`]).  Every writer re-emits it, or an
    /// edit would move the row out of its list item or quote (issue #75).
    fn prefix(&self) -> &str {
        &self.raw[..self.prefix_len]
    }

    /// The column byte `rel` (relative to the row's `raw`) belongs to in a table of `col_count`
    /// columns: the cell holding it, a pipe belonging to the cell before it and the prefix to the
    /// first.  Past the closing pipe of a row with fewer cells than `col_count`, the first cell it
    /// lacks; past a full row's, its last cell: both where the cursor shows
    /// ([`table_layout::drawn_cell_at`]).
    fn column_at(&self, rel: usize, col_count: usize) -> usize {
        let i = self
            .cells
            .iter()
            .take_while(|c| c.content_start <= rel)
            .count()
            .saturating_sub(1);
        match self.cells.last() {
            Some(last)
                if self.trail_pipe && rel > last.content_end && self.cells.len() < col_count =>
            {
                self.cells.len()
            }
            _ => i,
        }
    }

    /// The cell holding byte `rel` (relative to the row's `raw`), its pipes included; `None` in
    /// the prefix or past the row's cells.
    pub fn cell_at(&self, rel: usize) -> Option<&TableCell> {
        self.cells
            .iter()
            .find(|c| (c.content_start..=c.content_end).contains(&rel))
    }

    /// The row's cells for a rewrite that may move them: an edge cell without its pipe gets the
    /// padding space a pipe-side cell has, which [`rebuild_row`] drops again wherever it ends up
    /// on an edge without one.  `a | b` swaps to `b | a`, not ` b|a `.
    fn padded_cells(&self) -> Vec<TableCell> {
        let mut cells = self.cells.clone();
        let last = cells.len().saturating_sub(1);
        for (i, cell) in cells.iter_mut().enumerate() {
            if i == 0 && !self.lead_pipe && !cell.raw.starts_with(char::is_whitespace) {
                cell.raw.insert(0, ' ');
            }
            if i == last && !self.trail_pipe && !cell.raw.ends_with(char::is_whitespace) {
                cell.raw.push(' ');
            }
        }
        cells
    }

    fn raw_ends_with_newline(&self) -> bool {
        self.end > self.start + self.raw.len()
    }

    /// True if the row ends with a newline in the source, or a later row exists (which implies
    /// one separated them).
    fn raw_ends_with_newline_or_next_exists(&self, info: &TableInfo) -> bool {
        if self.raw_ends_with_newline() {
            return true;
        }
        info.rows
            .last()
            .map(|last| last.start != self.start)
            .unwrap_or(false)
    }
}

/// Split row `raw` (a line of the buffer from byte `start` to `end`, its newline included) into
/// cells as GFM does, from char column `content_col`, where its content starts past any prefix.
fn parse_row(raw: &str, start: usize, end: usize, content_col: usize, kind: RowKind) -> TableRow {
    let chars: Vec<char> = raw.chars().collect();
    let byte_at: Vec<usize> = raw
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(raw.len()))
        .collect();
    let ranges = table_layout::raw_cells(raw, content_col);
    let cells = ranges
        .iter()
        .map(|r| TableCell {
            content_start: byte_at[r.start],
            content_end: byte_at[r.end],
            raw: raw[byte_at[r.start]..byte_at[r.end]].to_owned(),
        })
        .collect();
    let (prefix_len, lead_pipe, trail_pipe) = match (ranges.first(), ranges.last()) {
        (Some(first), Some(last)) => {
            let lead = first.start > content_col && chars[first.start - 1] == '|';
            let trail = chars.get(last.end) == Some(&'|');
            (byte_at[first.start - usize::from(lead)], lead, trail)
        }
        _ => (raw.len(), false, false),
    };
    TableRow {
        start,
        end,
        raw: raw.to_owned(),
        cells,
        kind,
        prefix_len,
        lead_pipe,
        trail_pipe,
    }
}

/// True when `line` carries an unescaped `|`: the least a line needs to paste in as a table row.
pub fn has_cell_pipe(line: &str) -> bool {
    !table_layout::raw_pipe_positions(line).is_empty()
}

/// True when `row` is a valid GFM alignment row: every cell matches `:?-+:?`, e.g. `|---|:-:|`
/// or `--|--`.
fn is_alignment_row(row: &TableRow) -> bool {
    !row.cells.is_empty()
        && row.cells.iter().all(|cell| {
            let c = cell.raw.trim();
            let c = c.strip_prefix(':').unwrap_or(c);
            let c = c.strip_suffix(':').unwrap_or(c);
            !c.is_empty() && c.bytes().all(|b| b == b'-')
        })
}

fn line_start_byte(bytes: &[u8], pos: usize) -> usize {
    let mut p = pos.min(bytes.len());
    while p > 0 && bytes[p - 1] != b'\n' {
        p -= 1;
    }
    p
}

fn line_end_byte(bytes: &[u8], start: usize) -> usize {
    let mut p = start;
    while p < bytes.len() && bytes[p] != b'\n' {
        p += 1;
    }
    p
}

fn empty_row_text(col_count: usize) -> String {
    // `|   |   |   |\n`
    let mut s = String::with_capacity(4 * col_count + 2);
    s.push('|');
    for _ in 0..col_count {
        s.push_str("   |");
    }
    s.push('\n');
    s
}

fn raw_with_newline(info: &TableInfo, row_idx: usize) -> String {
    let row = &info.rows[row_idx];
    format_row_with_nl(row)
}

fn format_row_with_nl(row: &TableRow) -> String {
    let mut s = row.raw.clone();
    if row.raw_ends_with_newline() {
        s.push('\n');
    }
    s
}

/// Insert an empty cell at `col_idx`: `---` for an alignment row, padding spaces otherwise.
fn insert_blank_cell(cells: &[TableCell], col_idx: usize, kind: RowKind) -> Vec<TableCell> {
    let new_cell_raw = match kind {
        RowKind::Alignment => " --- ".to_owned(),
        _ => "   ".to_owned(),
    };
    let mut out = cells.to_vec();
    let col_idx = col_idx.min(out.len());
    out.insert(
        col_idx,
        TableCell {
            content_start: 0,
            content_end: new_cell_raw.len(),
            raw: new_cell_raw,
        },
    );
    out
}

/// Rebuild `row`'s text from `cells` (its [`TableRow::padded_cells`], edited), behind its
/// prefix and with its own edge pipes.  An edge pipe is added where GFM would misread the row
/// without one: a blank edge cell (`   | b` reads as `| b`), a single cell (`a` alone is no
/// row), or a first cell moved in that [`may_open_block`] (`- x | 1` is a list item, ending the
/// table), padded as a written pipe is.  An edge cell without its pipe drops its padding there.
fn rebuild_row(row: &TableRow, cells: &[TableCell]) -> String {
    let blank = |c: Option<&TableCell>| c.is_some_and(|c| c.raw.trim().is_empty());
    // The row's own first cell read as a row already; only a newcomer can open a block, and a
    // delimiter row's cells never do.
    let opens_block = row.kind != RowKind::Alignment
        && cells.first().is_some_and(|c| {
            row.cells
                .first()
                .is_none_or(|own| own.raw.trim() != c.raw.trim())
                && may_open_block(&c.raw)
        });
    let lead = row.lead_pipe || cells.len() < 2 || blank(cells.first()) || opens_block;
    let trail = row.trail_pipe || cells.len() < 2 || blank(cells.last());
    let last = cells.len().saturating_sub(1);
    let mut s = String::from(row.prefix());
    if lead {
        s.push('|');
    }
    for (i, cell) in cells.iter().enumerate() {
        let mut text = cell.raw.as_str();
        if i == 0 && !lead {
            text = text.trim_start();
        }
        if i == last && !trail {
            text = text.trim_end();
        }
        // A pipe the row didn't have gets the padding space beside it a written one has.
        if i == 0 && lead && !row.lead_pipe && !text.starts_with(char::is_whitespace) {
            s.push(' ');
        }
        s.push_str(text);
        if i == last && trail && !row.trail_pipe && !text.ends_with(char::is_whitespace) {
            s.push(' ');
        }
        if i < last || trail {
            s.push('|');
        }
    }
    s
}

/// Whether `cell`, at the start of a line, might open a block that ends the table (a list
/// item, heading, quote, fence, HTML block, thematic break, …).  Deliberately broad: anything
/// but text starting with a letter, or with digits not followed by an ordered-list `.` / `)`.
/// A false positive costs only a leading pipe the row didn't strictly need.
fn may_open_block(cell: &str) -> bool {
    let text = cell.trim_start();
    match text.chars().next() {
        None => false,
        Some(c) if c.is_ascii_digit() => text
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .starts_with(['.', ')']),
        Some(c) => !c.is_alphanumeric(),
    }
}

/// Concatenate every row's raw text, trailing newlines included.
fn collect_raw(info: &TableInfo) -> String {
    info.rows.iter().map(format_row_with_nl).collect()
}

// ─── Table insertion ─────────────────────────────────────────────────────────

/// True when the line containing `cursor_byte` is whitespace-only.  A `cursor_byte` past the end
/// belongs to the final line, so a buffer with no trailing newline reports `false` at EOF — the
/// "file ends without trailing newline" case the insert-table pre-flight catches.
pub fn cursor_line_is_blank(source: &str, cursor_byte: usize) -> bool {
    let bytes = source.as_bytes();
    let pos = cursor_byte.min(bytes.len());
    let start = line_start_byte(bytes, pos);
    let end = line_end_byte(bytes, start);
    source[start..end].trim().is_empty()
}

/// True when Insert Table can go at `cursor_byte`: on a blank line, or on an empty list item's
/// marker line ([`empty_item_at`]) where the table would fill the item ([`table_fills_item`]).
pub fn can_insert_table(source: &str, cursor_byte: usize) -> bool {
    cursor_line_is_blank(source, cursor_byte)
        || (empty_item_at(source, cursor_byte).is_some() && table_fills_item(source, cursor_byte))
}

/// Whether the table [`insert_table`] puts on the empty item at `cursor_byte` parses as that
/// item's content.  The text alone can't tell: a `- ` in a code block is code, and a `2. ` below
/// a paragraph line continues the paragraph, table or not.  The check is on the text after the
/// insertion because the line before it can mislead the other way: an empty item can't
/// interrupt a paragraph, so `text⏎- ` reads as a setext heading, but `- | a |` can.
fn table_fills_item(source: &str, cursor_byte: usize) -> bool {
    let (delta, _) = insert_table(source, cursor_byte, 0, 1, 0);
    let mut post = source.to_owned();
    post.replace_range(
        delta.offset..delta.offset + delta.removed.len(),
        &delta.inserted,
    );
    let doc = parse_document(&post);
    let line_of = |byte: usize| doc.line_starts.partition_point(|&s| s <= byte) - 1;
    let Some(i) = doc.ranges.iter().position(|r| r.contains(&delta.offset)) else {
        return false;
    };
    let line = to_u32(line_of(delta.offset) - line_of(doc.ranges[i].start));
    doc.blocks[i]
        .items_holding(line)
        .and_then(|items| items.last().copied())
        .is_some_and(|item| {
            item.span.start == line && matches!(item.blocks.first(), Some(Block::Table { .. }))
        })
}

/// The empty, non-task list item whose marker line holds `cursor_byte`, as the range past its
/// marker to its line end and the prefix that puts a continuation line at its content column.
/// A task item is left out: GFM reads `[ ]` as a checkbox only before a paragraph.
fn empty_item_at(source: &str, cursor_byte: usize) -> Option<(std::ops::Range<usize>, String)> {
    let info = list_edit::find_list_at(source, cursor_byte)?;
    let item = &info.items[list_edit::cursor_item_idx(&info, cursor_byte)?];
    if cursor_byte > item.line_end || item.task.is_some() || !item.content_is_empty(source) {
        return None;
    }
    let prefix = format!(
        "{}{}",
        info.indent,
        " ".repeat(item.marker_end - item.marker_start)
    );
    Some((item.marker_end..item.line_end, prefix))
}

/// The column a table inserted on the blank line at `cursor_byte` starts at: the content
/// column of the list item text typed there would belong to, else 0.  `item_cols` gives the
/// content columns of the items holding the line starting at a byte, outermost first
/// (`row_map::item_content_cols`).
///
/// Directly below an item's last line, text continues that line, so the innermost item takes
/// the table.  Past a blank line, text belongs to an item only when indented to its content
/// column, so the line's own indentation picks the deepest item it reaches; an unindented line
/// there (where `Enter` leaves the cursor on leaving a list) stays top-level.
pub fn blank_line_indent(
    source: &str,
    cursor_byte: usize,
    item_cols: impl FnOnce(usize) -> Vec<usize>,
) -> usize {
    let bytes = source.as_bytes();
    let line_start = line_start_byte(bytes, cursor_byte.min(bytes.len()));
    let indent = source[line_start..]
        .chars()
        .map_while(|c| match c {
            ' ' => Some(1),
            '\t' => Some(4),
            _ => None,
        })
        .sum::<usize>();
    // The nearest line above with text, and whether any blank line lies between.
    let mut end = line_start;
    let mut directly_below = true;
    let above = loop {
        if end == 0 {
            return 0;
        }
        let start = line_start_byte(bytes, end - 1);
        if !source[start..end - 1].trim().is_empty() {
            break start;
        }
        directly_below = false;
        end = start;
    };
    let cols = item_cols(above);
    let chosen = if directly_below {
        cols.last()
    } else {
        cols.iter().rev().find(|&&c| c <= indent)
    };
    chosen.copied().unwrap_or(0)
}

/// Emit a fresh GFM pipe table at the cursor, which the pre-flight ([`can_insert_table`]) has
/// verified is on a blank line or an empty list item.  Returns the delta plus the post-edit byte
/// offset of the first header cell's content.
///
/// On an empty item the table opens on the marker line, its other rows at the item's content
/// column, so the item holds it; the marker line's own `\n` ends it.
///
/// On a blank line every row starts at column `indent` ([`blank_line_indent`]), and CommonMark
/// needs a blank line either side.  The cursor's own blank line, preserved after the insertion
/// at `line_start`, always supplies the trailing one; only the leading `\n` may be missing, and
/// is prepended when the line above carries content.
pub fn insert_table(
    source: &str,
    cursor_byte: usize,
    rows: usize,
    cols: usize,
    indent: usize,
) -> (EditDelta, usize) {
    debug_assert!(cols >= 1, "table must have at least one column");
    if let Some((after_marker, prefix)) = empty_item_at(source, cursor_byte) {
        let mut inserted = table_text(rows, cols, &prefix);
        inserted.drain(..prefix.len()); // the marker line holds the header
        inserted.pop(); // the marker line's `\n` stays
        let cursor_target = after_marker.start + 2;
        let delta = EditDelta {
            offset: after_marker.start,
            removed: source[after_marker].to_owned(),
            inserted,
        };
        return (delta, cursor_target);
    }
    let bytes = source.as_bytes();
    let pos = cursor_byte.min(bytes.len());
    let line_start = line_start_byte(bytes, pos);

    let need_prefix = if line_start == 0 {
        false
    } else {
        let prev_end = line_start.saturating_sub(1); // index of `\n`
        let prev_start = line_start_byte(bytes, prev_end.saturating_sub(1));
        !source[prev_start..prev_end].trim().is_empty()
    };

    let mut inserted = String::new();
    if need_prefix {
        inserted.push('\n');
    }
    inserted.push_str(&table_text(rows, cols, &" ".repeat(indent)));

    // First header cell content: `empty_row_text` lays out `|   |   |…`, so +1 for the `|` and
    // +1 to skip the leading padding space.
    let cursor_target = line_start + usize::from(need_prefix) + indent + 2;

    let delta = EditDelta {
        offset: line_start,
        removed: String::new(),
        inserted,
    };
    (delta, cursor_target)
}

/// An empty table's text, header through its `rows` data rows, each line after `prefix`.
fn table_text(rows: usize, cols: usize, prefix: &str) -> String {
    let lines = [empty_row_text(cols), alignment_row_text(cols)];
    let mut text = String::new();
    for line in lines.iter().chain(std::iter::repeat_n(&lines[0], rows)) {
        text.push_str(prefix);
        text.push_str(line);
    }
    text
}

/// Build the alignment row text with neutral `---` cells: `| --- | --- |\n`.
fn alignment_row_text(col_count: usize) -> String {
    let mut s = String::with_capacity(6 * col_count + 2);
    s.push('|');
    for _ in 0..col_count {
        s.push_str(" --- |");
    }
    s.push('\n');
    s
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::parser::parse;
    use crate::markdown::{inlines_to_plain, Block, Inline};

    fn src_offset_of(src: &str, needle: &str) -> usize {
        src.find(needle).expect("needle not found")
    }

    /// `raw` split as a data row (or, for `kind`, another) whose content starts at char `col`.
    fn row_at(raw: &str, col: usize, kind: RowKind) -> TableRow {
        parse_row(raw, 0, raw.len(), col, kind)
    }

    fn raws(row: &TableRow) -> Vec<&str> {
        row.cells.iter().map(|c| c.raw.as_str()).collect()
    }

    #[test]
    fn is_alignment_row_basic() {
        let align = |raw: &str| is_alignment_row(&row_at(raw, 0, RowKind::Alignment));
        assert!(align("| --- | --- |"));
        assert!(align("|---|---|"));
        assert!(align("| :--- | ---: | :---: |"));
        assert!(align("--|--"), "edge pipes are optional");
        assert!(align(":-: | -"));
        assert!(!align("| abc | def |"));
        assert!(!align("|  |  |"));
    }

    /// A lone `|`, which a user can leave for one keystroke while editing the alignment row,
    /// is no alignment row, and nothing panics on it.
    #[test]
    fn is_alignment_row_single_pipe_does_not_panic() {
        let align = |raw: &str| is_alignment_row(&row_at(raw, 0, RowKind::Alignment));
        assert!(!align("|"));
        assert!(!align(" | "));
        assert!(!align(""));
    }

    #[test]
    fn has_cell_pipe_basic() {
        assert!(has_cell_pipe("| a | b |"));
        assert!(has_cell_pipe("a | b"));
        assert!(!has_cell_pipe(r"a \| b"));
        assert!(
            has_cell_pipe(r"a \\| b"),
            "an escaped backslash leaves the pipe a pipe"
        );
        assert!(!has_cell_pipe("hello world"));
    }

    /// Rows split as GFM splits them, from their content column: edge pipes optional, escaped
    /// pipes inside a cell, a container prefix (even one holding a `|`) never a cell.
    #[test]
    fn parse_row_splits_as_gfm_does() {
        let piped = row_at("| a | b | c |", 0, RowKind::Data);
        assert_eq!(raws(&piped), [" a ", " b ", " c "]);
        assert_eq!(
            (piped.prefix(), piped.lead_pipe, piped.trail_pipe),
            ("", true, true)
        );

        let escaped = row_at(r"| a \| x | b |", 0, RowKind::Data);
        assert_eq!(raws(&escaped), [r" a \| x ", " b "]);

        let bare = row_at("a | b", 0, RowKind::Data);
        assert_eq!(raws(&bare), ["a ", " b"]);
        assert_eq!(
            (bare.prefix(), bare.lead_pipe, bare.trail_pipe),
            ("", false, false)
        );

        let half = row_at("  1 | 2 |", 2, RowKind::Data);
        assert_eq!(raws(&half), ["1 ", " 2 "]);
        assert_eq!(
            (half.prefix(), half.lead_pipe, half.trail_pipe),
            ("  ", false, true)
        );

        let quoted = row_at("> | a | b |", 2, RowKind::Data);
        assert_eq!(raws(&quoted), [" a ", " b "]);
        assert_eq!(quoted.prefix(), "> ");

        let footnote = row_at("[^a|b]: | x | y |", 8, RowKind::Header);
        assert_eq!(raws(&footnote), [" x ", " y "]);
        assert_eq!(footnote.prefix(), "[^a|b]: ");
    }

    /// A pipe belongs to the cell before it and the prefix to the first; past a short row's
    /// closing pipe is the first cell it lacks, past a full row's its last cell.
    #[test]
    fn column_at_and_cell_at_find_the_cell_holding_a_byte() {
        let row = row_at(r"| a \| x | b |", 0, RowKind::Data);
        assert_eq!(row.column_at(4, 2), 0, "inside the first cell");
        assert_eq!(row.column_at(9, 2), 0, "on the pipe after it");
        assert_eq!(row.column_at(11, 2), 1);

        let quoted = row_at("> | a | b |", 2, RowKind::Data);
        let b = quoted.raw.find('b').unwrap();
        assert_eq!(quoted.column_at(0, 2), 0, "the quote's `>`");
        assert_eq!(quoted.cell_at(b).map(|c| c.raw.as_str()), Some(" b "));
        assert_eq!(quoted.cell_at(0), None, "the quote's `>` is in no cell");

        let bare = row_at("1 | 2 |", 0, RowKind::Data);
        assert_eq!(bare.cell_at(0).map(|c| c.raw.as_str()), Some("1 "));

        let short = row_at("| 1 |", 0, RowKind::Data);
        assert_eq!(short.column_at(4, 3), 0, "the closing pipe");
        assert_eq!(short.column_at(5, 3), 1, "past it");
        assert_eq!(short.column_at(5, 1), 0, "past a full row's, its last cell");
        assert_eq!(row_at("| 1", 0, RowKind::Data).column_at(3, 3), 0);
    }

    #[test]
    fn find_table_detects_cursor_in_data_row() {
        let src = "\
# Title

| a | b |
|---|---|
| 1 | 2 |
| 3 | 4 |
";
        let cursor = src_offset_of(src, "3");
        let info = find_table_at(src, cursor).expect("table");
        assert_eq!(info.col_count, 2);
        assert_eq!(info.rows.len(), 4); // header + align + 2 data rows
        assert_eq!(info.rows[0].kind, RowKind::Header);
        assert_eq!(info.rows[1].kind, RowKind::Alignment);
        assert_eq!(info.rows[2].kind, RowKind::Data);
        assert_eq!(info.rows[3].kind, RowKind::Data);
    }

    #[test]
    fn find_table_returns_none_outside_table() {
        let src = "# Title\n\nParagraph\n";
        assert!(find_table_at(src, 0).is_none());
        assert!(find_table_at(src, 10).is_none());
    }

    #[test]
    fn cursor_cell_identifies_row_and_column() {
        let src = "| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |\n";
        let cursor = src_offset_of(src, "2");
        let info = find_table_at(src, cursor).unwrap();
        let (row, col) = cursor_cell(&info, cursor).unwrap();
        assert_eq!(row, 2);
        assert_eq!(col, 1);
    }

    #[test]
    fn cell_cursor_offset_lands_on_content() {
        let src = "| a | b |\n|---|---|\n| 11 | 22 |\n";
        let info = find_table_at(src, 0).unwrap();
        let offset = cell_cursor_offset(&info, 2, 1).unwrap();
        assert_eq!(&src[offset..offset + 2], "22");
    }

    #[test]
    fn cell_end_cursor_offset_lands_past_last_non_whitespace() {
        let src = "| a | b |\n|---|---|\n| 11 | 22 |\n";
        let info = find_table_at(src, 0).unwrap();
        // Just past the last '1' of " 11 ", i.e. on its trailing space.
        let offset = cell_end_cursor_offset(&info, 2, 0).unwrap();
        assert_eq!(&src[offset..offset + 1], " ");
        assert_eq!(&src[offset - 1..offset], "1");
    }

    #[test]
    fn cell_end_cursor_offset_for_empty_cell_falls_back_to_typing_position() {
        let src = "| a | b |\n|---|---|\n|   |   |\n";
        let info = find_table_at(src, 0).unwrap();
        let offset = cell_end_cursor_offset(&info, 2, 0).unwrap();
        let start_offset = cell_cursor_offset(&info, 2, 0).unwrap();
        assert_eq!(offset, start_offset);
    }

    #[test]
    fn insert_row_below_appends_new_row() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let info = find_table_at(src, 0).unwrap();
        let (delta, target_idx) = insert_row(&info, 2, true); // below row 2
        assert_eq!(target_idx, 3);

        let mut new_src = String::new();
        new_src.push_str(&src[..delta.offset]);
        new_src.push_str(&delta.inserted);
        new_src.push_str(&src[delta.offset..]);
        assert!(new_src.contains("| 1 | 2 |\n|   |   |\n"));
    }

    #[test]
    fn delete_row_removes_data_row() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
        let info = find_table_at(src, 0).unwrap();
        let delta = delete_row(&info, 2).unwrap(); // delete first data row
        let mut new_src = String::new();
        new_src.push_str(&src[..delta.offset]);
        new_src.push_str(&src[delta.offset + delta.removed.len()..]);
        assert!(!new_src.contains("| 1 | 2 |"));
        assert!(new_src.contains("| 3 | 4 |"));
    }

    #[test]
    fn delete_row_refuses_header_and_alignment() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let info = find_table_at(src, 0).unwrap();
        assert!(delete_row(&info, 0).is_none());
        assert!(delete_row(&info, 1).is_none());
    }

    #[test]
    fn swap_rows_adjacent_data_rows() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
        let info = find_table_at(src, 0).unwrap();
        let delta = swap_rows(&info, 2, 3).unwrap();
        let mut new_src = String::new();
        new_src.push_str(&src[..delta.offset]);
        new_src.push_str(&delta.inserted);
        new_src.push_str(&src[delta.offset + delta.removed.len()..]);
        let expected = "| a | b |\n|---|---|\n| 3 | 4 |\n| 1 | 2 |\n";
        assert_eq!(new_src, expected);
    }

    #[test]
    fn insert_column_adds_to_all_rows() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let info = find_table_at(src, 0).unwrap();
        let delta = insert_column(&info, 1, true); // insert to right of col 1
        let mut new_src = String::new();
        new_src.push_str(&src[..delta.offset]);
        new_src.push_str(&delta.inserted);
        new_src.push_str(&src[delta.offset + delta.removed.len()..]);

        let info2 = find_table_at(&new_src, 0).unwrap();
        assert_eq!(info2.col_count, 3);
    }

    #[test]
    fn delete_column_removes_from_all_rows() {
        let src = "| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |\n";
        let info = find_table_at(src, 0).unwrap();
        let delta = delete_column(&info, 1).unwrap();
        let mut new_src = String::new();
        new_src.push_str(&src[..delta.offset]);
        new_src.push_str(&delta.inserted);
        new_src.push_str(&src[delta.offset + delta.removed.len()..]);

        let info2 = find_table_at(&new_src, 0).unwrap();
        assert_eq!(info2.col_count, 2);
    }

    #[test]
    fn delete_column_refuses_last_column() {
        let src = "| a |\n|---|\n| 1 |\n";
        let info = find_table_at(src, 0).unwrap();
        assert!(delete_column(&info, 0).is_none());
    }

    #[test]
    fn swap_columns_adjacent_pair() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let info = find_table_at(src, 0).unwrap();
        let delta = swap_columns(&info, 0, 1).unwrap();
        let mut new_src = String::new();
        new_src.push_str(&src[..delta.offset]);
        new_src.push_str(&delta.inserted);
        new_src.push_str(&src[delta.offset + delta.removed.len()..]);

        let info2 = find_table_at(&new_src, 0).unwrap();
        assert_eq!(info2.rows[0].cells[0].trimmed(), "b");
        assert_eq!(info2.rows[0].cells[1].trimmed(), "a");
    }

    #[test]
    fn find_table_handles_cursor_on_alignment_row() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let cursor = src.find("---").unwrap();
        let info = find_table_at(src, cursor).unwrap();
        assert_eq!(info.col_count, 2);
    }

    #[test]
    fn write_column_widths_inserts_new_comment() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let info = find_table_at(src, 0).unwrap();
        let delta = write_column_widths(src, &info, &[Some(10), Some(20)]);
        assert_eq!(delta.offset, info.end);
        assert_eq!(delta.removed, "");
        assert_eq!(delta.inserted, "<!-- tui-columns: [10, 20] -->\n");
    }

    #[test]
    fn write_column_widths_replaces_existing_comment() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n<!-- tui-columns: [5, 7] -->\n";
        let info = find_table_at(src, 0).unwrap();
        let delta = write_column_widths(src, &info, &[Some(10), Some(20)]);
        let mut new_src = String::new();
        new_src.push_str(&src[..delta.offset]);
        new_src.push_str(&delta.inserted);
        new_src.push_str(&src[delta.offset + delta.removed.len()..]);
        assert!(new_src.contains("<!-- tui-columns: [10, 20] -->"));
        assert!(!new_src.contains("[5, 7]"));
    }

    #[test]
    fn write_column_widths_emits_underscore_for_auto_entries() {
        let src = "| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |\n";
        let info = find_table_at(src, 0).unwrap();
        let delta = write_column_widths(src, &info, &[Some(8), None, Some(12)]);
        assert_eq!(delta.inserted, "<!-- tui-columns: [8, _, 12] -->\n");
    }

    // ── `insert_table` and `cursor_line_is_blank` ───────────────────────────

    #[test]
    fn cursor_line_is_blank_recognises_empty_and_whitespace_lines() {
        assert!(cursor_line_is_blank("", 0));
        assert!(cursor_line_is_blank("\n", 0));
        assert!(cursor_line_is_blank("hello\n\nworld\n", 6)); // on the blank line
        assert!(cursor_line_is_blank("hello\n   \nworld\n", 8)); // whitespace-only line
    }

    #[test]
    fn cursor_line_is_blank_rejects_text_lines() {
        assert!(!cursor_line_is_blank("hello\n", 0));
        assert!(!cursor_line_is_blank("# Heading\n", 4));
    }

    #[test]
    fn cursor_line_is_blank_rejects_eof_when_final_line_has_content() {
        let src = "no trailing newline";
        assert!(!cursor_line_is_blank(src, src.len()));
    }

    #[test]
    fn insert_table_between_paragraphs_pads_with_blank_lines() {
        let src = "para one\n\npara two\n";
        // Byte 9 starts the blank line between the paragraphs.
        let cursor = 9usize;
        assert!(cursor_line_is_blank(src, cursor));
        let (delta, cursor_target) = insert_table(src, cursor, 2, 3, 0);

        let mut post = String::new();
        post.push_str(&src[..delta.offset]);
        post.push_str(&delta.inserted);
        post.push_str(&src[delta.offset..]);

        // The pre-existing blank line carries the trailing gap; only the leading `\n` is added.
        assert_eq!(
            post,
            "para one\n\
             \n\
             |   |   |   |\n\
             | --- | --- | --- |\n\
             |   |   |   |\n\
             |   |   |   |\n\
             \n\
             para two\n"
        );
        let around: &str = &post[cursor_target - 2..cursor_target + 2];
        assert_eq!(
            around, "|   ",
            "cursor should land in the first header cell, around={around:?}"
        );
    }

    #[test]
    fn insert_table_at_start_of_buffer_omits_leading_padding() {
        let src = "\nparagraph\n";
        let cursor = 0;
        assert!(cursor_line_is_blank(src, cursor));
        let (delta, _cursor_target) = insert_table(src, cursor, 1, 2, 0);
        assert_eq!(delta.offset, 0);
        // No `\n` prefix at the top of the buffer; the cursor's blank line still supplies the
        // trailing separator before "paragraph".
        assert!(delta.inserted.starts_with('|'));
        let mut post = String::new();
        post.push_str(&src[..delta.offset]);
        post.push_str(&delta.inserted);
        post.push_str(&src[delta.offset..]);
        assert!(
            post.contains("|\n\nparagraph"),
            "table should be followed by a blank line, post={post}"
        );
    }

    #[test]
    fn insert_table_at_end_of_buffer_with_blank_trailing_line() {
        let src = "para\n\n";
        let cursor = src.len();
        assert!(cursor_line_is_blank(src, cursor));
        let (delta, _) = insert_table(src, cursor, 1, 1, 0);
        // The leading `\n` is needed because `para` is non-blank.
        assert!(
            delta.inserted.starts_with('\n'),
            "leading newline missing, got {:?}",
            delta.inserted
        );
        // The helper appends no trailing newline of its own.
        let trailing_newlines = delta
            .inserted
            .chars()
            .rev()
            .take_while(|c| *c == '\n')
            .count();
        assert_eq!(
            trailing_newlines, 1,
            "delta should end with the table's own row terminator only, got {:?}",
            delta.inserted
        );
    }

    #[test]
    fn insert_table_emits_alignment_row_with_dashes() {
        let src = "\n";
        let (delta, _) = insert_table(src, 0, 0, 3, 0);
        assert!(
            delta.inserted.contains("| --- | --- | --- |"),
            "alignment row missing dashes, got {:?}",
            delta.inserted
        );
    }

    // ── Tables inside a list item (issue #75) ────────────────────────────────

    /// A two-row table inside an item, one and two list levels deep.
    const NESTED: [&str; 2] = [
        "- item\n\n  | a | b |\n  |---|---|\n  | 1 | 2 |\n  | 3 | 4 |\n",
        "- outer\n\n  - inner\n\n    | a | b |\n    |---|---|\n    | 1 | 2 |\n    | 3 | 4 |\n",
    ];

    /// `src` with `delta` (byte offsets) applied.
    fn apply(src: &str, delta: &EditDelta) -> String {
        let end = delta.offset + delta.removed.len();
        assert_eq!(&src[delta.offset..end], delta.removed);
        format!("{}{}{}", &src[..delta.offset], delta.inserted, &src[end..])
    }

    /// The one table in `src`'s blocks.  It must sit in an item's blocks, with the list the
    /// document's only block: the edit kept the table inside its list item.
    fn nested_table(src: &str) -> Block {
        fn find(blocks: &[Block]) -> Option<&Block> {
            blocks.iter().find_map(|b| match b {
                Block::List { items, .. } => items.iter().find_map(|it| find(&it.blocks)),
                Block::Table { .. } => Some(b),
                _ => None,
            })
        }
        let blocks = parse(src);
        assert!(
            matches!(blocks.as_slice(), [Block::List { .. }]),
            "everything stays in the list:\n{src}"
        );
        find(&blocks)
            .unwrap_or_else(|| panic!("no table inside the item:\n{src}"))
            .clone()
    }

    /// The cells of [`nested_table`], header first.
    fn nested_table_cells(src: &str) -> Vec<Vec<String>> {
        let Block::Table { headers, rows, .. } = nested_table(src) else {
            unreachable!();
        };
        let plain = |cells: &Vec<Vec<Inline>>| {
            cells
                .iter()
                .map(|c| inlines_to_plain(c).trim().to_owned())
                .collect::<Vec<_>>()
        };
        std::iter::once(plain(&headers))
            .chain(rows.iter().map(plain))
            .collect()
    }

    fn nested_info(src: &str) -> TableInfo {
        find_table_at(src, src.find("| 1").unwrap()).expect("table")
    }

    fn cells(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|c| (*c).to_owned()).collect())
            .collect()
    }

    #[test]
    fn row_edits_keep_a_nested_table_in_its_item() {
        for src in NESTED {
            let info = nested_info(src);
            let (delta, _) = insert_row(&info, 2, true);
            assert_eq!(
                nested_table_cells(&apply(src, &delta)),
                cells(&[&["a", "b"], &["1", "2"], &["", ""], &["3", "4"]])
            );
            let (delta, _) = insert_row(&info, 3, true);
            assert_eq!(
                nested_table_cells(&apply(src, &delta)),
                cells(&[&["a", "b"], &["1", "2"], &["3", "4"], &["", ""]])
            );
            let delta = delete_row(&info, 2).unwrap();
            assert_eq!(
                nested_table_cells(&apply(src, &delta)),
                cells(&[&["a", "b"], &["3", "4"]])
            );
            let delta = swap_rows(&info, 2, 3).unwrap();
            assert_eq!(
                nested_table_cells(&apply(src, &delta)),
                cells(&[&["a", "b"], &["3", "4"], &["1", "2"]])
            );
        }
    }

    #[test]
    fn column_edits_keep_a_nested_table_in_its_item() {
        for src in NESTED {
            let info = nested_info(src);
            let delta = insert_column(&info, 0, true);
            assert_eq!(
                nested_table_cells(&apply(src, &delta)),
                cells(&[&["a", "", "b"], &["1", "", "2"], &["3", "", "4"]])
            );
            let delta = delete_column(&info, 0).unwrap();
            assert_eq!(
                nested_table_cells(&apply(src, &delta)),
                cells(&[&["b"], &["2"], &["4"]])
            );
            let delta = swap_columns(&info, 0, 1).unwrap();
            assert_eq!(
                nested_table_cells(&apply(src, &delta)),
                cells(&[&["b", "a"], &["2", "1"], &["4", "3"]])
            );
        }
    }

    /// A table opening on its item's marker line or a footnote's leader line indents its widths
    /// comment like its last row, not behind the header's marker or label.
    #[test]
    fn a_widths_comment_never_copies_the_headers_marker_or_label() {
        for (src, indent) in [
            ("- | a |\n  |---|\n  | 1 |\n", "  "),
            ("x[^n]\n\n[^n]: | a |\n    |---|\n    | 1 |\n", "    "),
        ] {
            let info = find_table_at(src, src.find('1').unwrap()).expect("a table");
            let delta = write_column_widths(src, &info, &[Some(5)]);
            assert_eq!(
                delta.inserted,
                format!("{indent}<!-- tui-columns: [5] -->\n")
            );
        }
    }

    /// `reparse` re-splits the same rows from edited text, as a column-swap chain needs.
    #[test]
    fn reparse_follows_a_column_swap() {
        let src = "> a | b\n> --|--\n> 1 | 2\n";
        let info = find_table_at(src, 0).unwrap();
        let swapped = apply(src, &swap_columns(&info, 0, 1).unwrap());
        let again = info.reparse(&swapped).expect("still a table");
        assert_eq!(raws(&again.rows[2]), ["2 ", " 1"]);
        assert_eq!(again.rows[2].prefix(), "> ");
    }

    /// A short row lacking the column a swap moves its cell into gets it, blank, so its cell
    /// moves with its header.
    #[test]
    fn swap_columns_moves_a_short_rows_cell_with_its_header() {
        let src = "| a | b | c |\n|---|---|---|\n| 1 | 2 |\n";
        let info = find_table_at(src, 0).unwrap();
        let swapped = apply(src, &swap_columns(&info, 1, 2).unwrap());
        assert_eq!(swapped, "| a | c | b |\n|---|---|---|\n| 1 |   | 2 |\n");
    }

    #[test]
    fn may_open_block_flags_anything_but_plain_text() {
        for cell in [
            " - x", "# h", "> q", "```", "<div>", "1. x", "2) x", "***", "+ x",
        ] {
            assert!(may_open_block(cell), "{cell:?}");
        }
        for cell in [" x ", "word", "12", "1x", "日本", ""] {
            assert!(!may_open_block(cell), "{cell:?}");
        }
    }

    /// A short row's last reachable column is its last cell; a long row's, the table's last.
    #[test]
    fn last_col_stops_at_a_short_rows_last_cell() {
        let src = "| a | b | c |\n|---|---|---|\n| 1 |\n| 1 | 2 | 3 | 4 |\n";
        let info = find_table_at(src, 0).unwrap();
        assert_eq!(info.col_count, 3);
        assert_eq!(
            (info.last_col(0), info.last_col(2), info.last_col(3)),
            (2, 0, 2)
        );
    }

    /// The widths comment is written at the table's indent, so it stays in the item, gives the
    /// table its widths, and the next write finds and replaces it.
    #[test]
    fn a_nested_tables_widths_comment_stays_in_its_item() {
        for src in NESTED {
            let info = nested_info(src);
            let once = apply(src, &write_column_widths(src, &info, &[Some(5), None]));
            let indent = info.rows[0].prefix();
            assert!(
                once.ends_with(&format!("{indent}<!-- tui-columns: [5, _] -->\n")),
                "{once:?}"
            );
            assert!(
                matches!(
                    nested_table(&once),
                    Block::Table { user_widths: Some(w), .. } if w == [Some(5), None]
                ),
                "{once}"
            );
            let info = nested_info(&once);
            let twice = apply(&once, &write_column_widths(&once, &info, &[Some(7), None]));
            assert_eq!(twice, once.replace("[5, _]", "[7, _]"));
        }
    }

    // ── `insert_table` on an empty list item ─────────────────────────────────

    /// The table opens on the empty item's marker line, its other rows at the item's content
    /// column, and parses inside that item, whatever surrounds it.
    #[test]
    fn insert_table_fills_an_empty_list_item() {
        // (source, the empty item's marker, the continuation prefix)
        let cases = [
            ("- \n", "- ", "  "),
            ("- one\n- \n- three\n", "- \n", "  "),
            ("1. one\n10. \n", "10. ", "    "),
            ("- outer\n  - inner\n  - \n", "  - \n", "    "),
            // Empty, the item reads as a setext underline; holding the table, it's an item.
            ("- outer\n  - \n", "  - \n", "    "),
            ("- ", "- ", "  "),
        ];
        for (src, marker, prefix) in cases {
            let cursor = src.rfind(marker).unwrap() + marker.trim_end().len() + 1;
            assert!(can_insert_table(src, cursor), "{src:?}");
            let (delta, target) = insert_table(src, cursor, 1, 2, 0);
            let post = apply(src, &delta);
            let head = format!(
                "{}|   |   |\n{prefix}| --- | --- |\n{prefix}|   |   |",
                marker.trim_end_matches('\n')
            );
            assert!(post.contains(&head), "{src:?} became {post:?}");
            assert_eq!(&post[target - 2..target], "| ", "{src:?}");
            assert_eq!(
                nested_table_cells(&post),
                vec![vec![String::new(); 2]; 2],
                "{post:?}"
            );
        }
    }

    /// Text around the list stays out of the table: a paragraph the item's list interrupts, and
    /// one that follows it.
    #[test]
    fn a_table_in_an_empty_item_leaves_the_surrounding_paragraphs_alone() {
        for src in ["text\n- \n", "- \nafter\n"] {
            let cursor = src.find("- ").unwrap() + 2;
            assert!(can_insert_table(src, cursor), "{src:?}");
            let (delta, _) = insert_table(src, cursor, 1, 1, 0);
            let blocks = parse(&apply(src, &delta));
            let kinds: Vec<_> = blocks
                .iter()
                .map(|b| match b {
                    Block::Paragraph { .. } => "paragraph",
                    Block::List { items, .. }
                        if matches!(items[0].blocks.as_slice(), [Block::Table { .. }]) =>
                    {
                        "list(table)"
                    }
                    _ => "other",
                })
                .collect();
            let want = if src.starts_with("text") {
                ["paragraph", "list(table)"]
            } else {
                ["list(table)", "paragraph"]
            };
            assert_eq!(kinds, want, "{src:?}");
        }
    }

    /// Only an empty, non-task item's marker line qualifies.
    #[test]
    fn insert_table_rejects_other_list_lines() {
        for (src, at) in [
            ("- one\n", "one"),        // an item with text
            ("- [ ] \n", "] "),        // a task item
            ("- \n  more\n", "- "),    // empty first line, text below
            ("- one\n  two\n", "two"), // a continuation line
            ("```\n- \n```\n", "- "),  // code
            ("text\n2. \n", "2. "),    // continues the paragraph, table or not
        ] {
            let cursor = src.find(at).unwrap() + at.len();
            assert!(!can_insert_table(src, cursor), "{src:?}");
        }
    }

    /// Directly below a line the innermost item holding it takes the table; past a blank line,
    /// the deepest item the line's own indentation reaches, else none.
    #[test]
    fn blank_line_indent_follows_where_text_would_belong() {
        let nested = |_| vec![2, 4];
        // (source, cursor, want)
        let cases = [
            ("- a\n\n", 4, 4),        // directly below
            ("- a\n  \n", 4, 4),      // directly below, indentation ignored
            ("- a\n\n\n", 5, 0),      // past a blank line, unindented
            ("- a\n\n  \n", 5, 2),    // past a blank line, at the outer column
            ("- a\n\n     \n", 5, 4), // past a blank line, beyond the inner column
            ("- a\n\n\t\n", 5, 4),    // a tab counts four columns
            ("\n", 0, 0),             // nothing above
        ];
        for (src, cursor, want) in cases {
            assert_eq!(
                blank_line_indent(src, cursor, nested),
                want,
                "{src:?} at {cursor}"
            );
        }
        // The line above is what the columns are asked for.
        let asked = std::cell::Cell::new(None);
        blank_line_indent("p\n- a\n\n\n", 8, |at| {
            asked.set(Some(at));
            vec![]
        });
        assert_eq!(asked.get(), Some(2));
    }
}
