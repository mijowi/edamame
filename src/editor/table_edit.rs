//! GFM table detection, parsing, and structure-editing primitives: which cell the cursor is in,
//! cell navigation, and row/column insert / delete / move.
//!
//! Every structure edit is a single `EditDelta`, so it undoes as one step.
//!
//! The parser is byte-oriented and does not go through `pulldown-cmark` — it scans lines for the
//! `| cell | cell |` shape with an alignment row second.  That keeps navigation cheap and avoids
//! reconciling a parsed AST back to exact byte offsets.

use crate::document::EditDelta;
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
    /// Per-cell information — `cells.len() == col_count` for well-formed rows.
    pub cells: Vec<TableCell>,
    pub kind: RowKind,
}

/// A single cell's content range, relative to the start of the row's `raw` string (not the
/// buffer) and inclusive of padding spaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCell {
    /// Byte offset within `raw` of the char immediately after the leading `|`.
    pub content_start: usize,
    /// Byte offset within `raw` of the char immediately before the trailing `|`.
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

/// True when `block_source` is a GFM table block.  `RenderedView` uses it to shift the
/// raw→rendered line mapping by one, for the top border the renderer prepends.
pub fn is_table_block(block_source: &str) -> bool {
    let mut lines = block_source.split('\n');
    match (lines.next(), lines.next()) {
        (Some(first), Some(second)) => is_table_line(first) && is_alignment_row(second),
        _ => false,
    }
}

/// Find the GFM table containing `cursor_byte`.  A run of `|`-delimited lines qualifies when it
/// has at least two lines and the second is a valid alignment row (cells matching `:?-+:?`).
pub fn find_table_at(source: &str, cursor_byte: usize) -> Option<TableInfo> {
    if source.is_empty() {
        return None;
    }

    let bytes = source.as_bytes();
    let clamped = cursor_byte.min(source.len());
    let line_start = line_start_byte(bytes, clamped);
    let line_end = line_end_byte(bytes, line_start);

    let cursor_line = &source[line_start..line_end];
    if !is_table_line(cursor_line) {
        return None;
    }

    // Scan upward for consecutive table lines.
    let mut first_start = line_start;
    loop {
        if first_start == 0 {
            break;
        }
        let prev_end = first_start - 1;
        let prev_start = line_start_byte(bytes, prev_end);
        let prev = &source[prev_start..prev_end];
        if is_table_line(prev) {
            first_start = prev_start;
        } else {
            break;
        }
    }

    // Scan downward for consecutive table lines.
    let mut last_end = line_end;
    loop {
        if last_end >= source.len() {
            break;
        }
        if bytes[last_end] != b'\n' {
            break;
        }
        let next_start = last_end + 1;
        if next_start >= source.len() {
            break;
        }
        let next_end = line_end_byte(bytes, next_start);
        let next = &source[next_start..next_end];
        if is_table_line(next) {
            last_end = next_end;
        } else {
            break;
        }
    }

    // Parse every line of the run.
    let mut rows: Vec<TableRow> = Vec::new();
    let mut cursor = first_start;
    while cursor < last_end {
        let row_start = cursor;
        let row_end_content = line_end_byte(bytes, row_start);
        let raw = source[row_start..row_end_content].to_owned();
        let row_end_incl_nl = if row_end_content < source.len() && bytes[row_end_content] == b'\n' {
            row_end_content + 1
        } else {
            row_end_content
        };

        let cells = parse_cells(&raw);
        rows.push(TableRow {
            start: row_start,
            end: row_end_incl_nl,
            raw,
            cells,
            kind: RowKind::Data, // placeholder, fixed after we know alignment row
        });
        cursor = row_end_incl_nl;
        if cursor == row_end_content {
            break; // EOF without trailing newline
        }
    }

    if rows.len() < 2 {
        return None;
    }
    if !is_alignment_row(&rows[1].raw) {
        return None;
    }

    rows[0].kind = RowKind::Header;
    rows[1].kind = RowKind::Alignment;
    for r in rows.iter_mut().skip(2) {
        r.kind = RowKind::Data;
    }
    let col_count = rows[1].cells.len();

    // Short rows get padded to `col_count`; excess cells are kept rather than dropping content.
    let overall_start = rows.first().map(|r| r.start).unwrap_or(first_start);
    let overall_end = rows.last().map(|r| r.end).unwrap_or(last_end);

    Some(TableInfo {
        start: overall_start,
        end: overall_end,
        rows,
        col_count,
    })
}

/// The cursor's `(row_idx, col_idx)` within a table, or `None` when `cursor_byte` falls outside
/// it.
pub fn cursor_cell(info: &TableInfo, cursor_byte: usize) -> Option<(usize, usize)> {
    for (i, row) in info.rows.iter().enumerate() {
        if cursor_byte >= row.start && cursor_byte < row.end {
            let rel = cursor_byte - row.start;
            let col = column_for_offset(&row.raw, rel);
            return Some((i, col));
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
    let col = col_idx.min(row.cells.len().saturating_sub(1));
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
    let col = col_idx.min(row.cells.len().saturating_sub(1));
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

    let new_row = empty_row_text(info.col_count);
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
        let new_cells = insert_blank_cell(&row.cells, target_col, row.kind);
        inserted.push_str(&rebuild_row(&new_cells));
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
        let mut new_cells = row.cells.clone();
        if col_idx < new_cells.len() {
            new_cells.remove(col_idx);
        }
        inserted.push_str(&rebuild_row(&new_cells));
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
        let mut new_cells = row.cells.clone();
        if lo < new_cells.len() && hi < new_cells.len() {
            new_cells.swap(lo, hi);
        }
        inserted.push_str(&rebuild_row(&new_cells));
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
    let mut comment = table_layout::format_column_widths_comment(widths);
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

/// Split a row's raw text into cells: the text between unescaped `|` characters, excluding the
/// outer `|`s (which this implementation requires).
fn parse_cells(raw: &str) -> Vec<TableCell> {
    let mut cells = Vec::new();
    let bytes = raw.as_bytes();
    let len = bytes.len();
    if len == 0 {
        return cells;
    }

    // Find the first `|`.
    let mut i = 0;
    while i < len && bytes[i] != b'|' {
        i += 1;
    }
    if i >= len {
        return cells;
    }
    let mut content_start = i + 1;
    i = content_start;

    while i <= len {
        if i == len {
            break; // unterminated row — no trailing |
        }
        if bytes[i] == b'|' && (i == 0 || bytes[i - 1] != b'\\') {
            // Cell content is [content_start..i).
            let raw_cell = raw[content_start..i].to_owned();
            cells.push(TableCell {
                content_start,
                content_end: i,
                raw: raw_cell,
            });
            content_start = i + 1;
        }
        i += 1;
    }

    cells
}

/// The cell of raw table line `raw` holding byte `rel_byte`, or `None` outside every cell.  The
/// line may carry a container prefix (an item's indent, a quote's `> `) before its first `|`.
/// Text before the first `|` is taken for prefix, so a row with no leading pipe (`1 | 2 |`) has
/// no cell for its first column's bytes rather than the second column's.
pub fn cell_at(raw: &str, rel_byte: usize) -> Option<TableCell> {
    parse_cells(raw)
        .into_iter()
        .nth(column_for_offset(raw, rel_byte))
        .filter(|c| (c.content_start..=c.content_end).contains(&rel_byte))
}

/// Which column the byte at `rel_byte` (relative to the row's raw string) belongs to.  Bytes
/// before the first `|` count as column 0, those past the last as the final column.
fn column_for_offset(raw: &str, rel_byte: usize) -> usize {
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let rel_byte = rel_byte.min(len);
    let mut col = 0usize;
    let mut seen_first = false;
    for i in 0..rel_byte {
        if bytes[i] == b'|' && (i == 0 || bytes[i - 1] != b'\\') {
            if !seen_first {
                seen_first = true; // leading `|` establishes column 0
            } else {
                col += 1;
            }
        }
    }
    col
}

/// True when a line looks like a table row (starts and ends with `|` after
/// trimming whitespace, and contains at least one additional `|`).
pub fn is_table_line(line: &str) -> bool {
    let t = line.trim();
    if !t.starts_with('|') || !t.ends_with('|') {
        return false;
    }
    let unescaped_pipes = count_unescaped_pipes(t);
    unescaped_pipes >= 2
}

fn count_unescaped_pipes(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut n = 0;
    for i in 0..bytes.len() {
        if bytes[i] == b'|' && (i == 0 || bytes[i - 1] != b'\\') {
            n += 1;
        }
    }
    n
}

/// True when a line is a valid GFM alignment row, e.g. `|---|:-:|---:|`.
fn is_alignment_row(line: &str) -> bool {
    let t = line.trim();
    // At least `|x|`: a lone `|` (which a user leaves mid-edit) satisfies both `starts_with`
    // and `ends_with` and would panic on the `[1..len-1]` slice below.
    if t.len() < 3 || !t.starts_with('|') || !t.ends_with('|') {
        return false;
    }
    let inner = &t[1..t.len() - 1];
    for cell in inner.split('|') {
        let c = cell.trim();
        if c.is_empty() {
            return false;
        }
        let bytes = c.as_bytes();
        let mut start = 0;
        let mut end = bytes.len();
        if bytes[start] == b':' {
            start += 1;
        }
        if end > start && bytes[end - 1] == b':' {
            end -= 1;
        }
        if end <= start {
            return false;
        }
        if !bytes[start..end].iter().all(|&b| b == b'-') {
            return false;
        }
    }
    true
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

/// Rebuild a row's raw text from a cell list, re-inserting `|` separators.
fn rebuild_row(cells: &[TableCell]) -> String {
    let mut s = String::new();
    s.push('|');
    for cell in cells {
        s.push_str(&cell.raw);
        s.push('|');
    }
    s
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

/// Emit a fresh GFM pipe table at the cursor, which the pre-flight has verified is on a blank
/// line.  Returns the delta plus the post-edit byte offset of the first header cell's content.
///
/// CommonMark needs a blank line either side.  The cursor's own blank line, preserved after the
/// insertion at `line_start`, always supplies the trailing one; only the leading `\n` may be
/// missing, and is prepended when the line above carries content.
pub fn insert_table(
    source: &str,
    cursor_byte: usize,
    rows: usize,
    cols: usize,
) -> (EditDelta, usize) {
    debug_assert!(cols >= 1, "table must have at least one column");
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

    let header = empty_row_text(cols);
    let alignment = alignment_row_text(cols);
    let body = empty_row_text(cols).repeat(rows);
    let table_text = format!("{header}{alignment}{body}");

    let mut inserted = String::with_capacity(table_text.len() + 1);
    if need_prefix {
        inserted.push('\n');
    }
    inserted.push_str(&table_text);

    // First header cell content: `empty_row_text` lays out `|   |   |…`, so +1 for the `|` and
    // +1 to skip the leading padding space.
    let prefix_len = if need_prefix { 1 } else { 0 };
    let cursor_target = line_start + prefix_len + 2;

    let delta = EditDelta {
        offset: line_start,
        removed: String::new(),
        inserted,
    };
    (delta, cursor_target)
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

    fn src_offset_of(src: &str, needle: &str) -> usize {
        src.find(needle).expect("needle not found")
    }

    #[test]
    fn is_alignment_row_basic() {
        assert!(is_alignment_row("| --- | --- |"));
        assert!(is_alignment_row("|---|---|"));
        assert!(is_alignment_row("| :--- | ---: | :---: |"));
        assert!(!is_alignment_row("| abc | def |"));
        assert!(!is_alignment_row("|  |  |"));
    }

    /// Regression: a single `|` (which a user can leave for one
    /// keystroke while editing the alignment row) used to panic on the
    /// `[1..len-1]` slice.
    #[test]
    fn is_alignment_row_single_pipe_does_not_panic() {
        assert!(!is_alignment_row("|"));
        assert!(!is_alignment_row(" | "));
        assert!(!is_alignment_row(""));
    }

    #[test]
    fn is_table_block_basic() {
        assert!(is_table_block("| a | b |\n|---|---|\n| 1 | 2 |\n"));
        assert!(is_table_block("| a |\n|---|\n"));
        assert!(!is_table_block("paragraph\n"));
        assert!(!is_table_block("| a | b |\n"));
        assert!(!is_table_block("| a | b |\n| c | d |\n")); // second row not alignment
        assert!(!is_table_block(""));
    }

    #[test]
    fn is_table_line_basic() {
        assert!(is_table_line("| a | b |"));
        assert!(is_table_line("|---|---|"));
        assert!(!is_table_line("hello world"));
        assert!(!is_table_line("| a"));
    }

    #[test]
    fn parse_cells_basic() {
        let row = "| a | b | c |";
        let cells = parse_cells(row);
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].raw, " a ");
        assert_eq!(cells[1].raw, " b ");
        assert_eq!(cells[2].raw, " c ");
    }

    #[test]
    fn parse_cells_escaped_pipe() {
        let row = r"| a \| x | b |";
        let cells = parse_cells(row);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].raw, r" a \| x ");
        assert_eq!(cells[1].raw, " b ");
    }

    /// A container prefix before the first `|` is skipped; a first column with no leading pipe
    /// is no cell, not the next column's.
    #[test]
    fn cell_at_skips_a_prefix_and_never_answers_another_cell() {
        let quoted = "> | a | b |";
        let b = quoted.find('b').unwrap();
        assert_eq!(cell_at(quoted, b).map(|c| c.raw), Some(" b ".to_owned()));
        assert_eq!(cell_at(quoted, 0), None, "the quote's `>`");

        let bare = "1 | 2 |";
        assert_eq!(cell_at(bare, 0), None, "the leading-pipe-less first column");
        let two = bare.find('2').unwrap();
        assert_eq!(cell_at(bare, two).map(|c| c.raw), Some(" 2 ".to_owned()));
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

    #[test]
    fn column_for_offset_respects_escaped_pipes() {
        // col 0 = " a \| x " (offsets 1..9), col 1 = " b " (10..13)
        let row = r"| a \| x | b |";
        assert_eq!(column_for_offset(row, 4), 0); // inside first cell
        assert_eq!(column_for_offset(row, 10), 1); // inside second cell
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
        let (delta, cursor_target) = insert_table(src, cursor, 2, 3);

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
        let (delta, _cursor_target) = insert_table(src, cursor, 1, 2);
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
        let (delta, _) = insert_table(src, cursor, 1, 1);
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
        let (delta, _) = insert_table(src, 0, 0, 3);
        assert!(
            delta.inserted.contains("| --- | --- | --- |"),
            "alignment row missing dashes, got {:?}",
            delta.inserted
        );
    }
}
