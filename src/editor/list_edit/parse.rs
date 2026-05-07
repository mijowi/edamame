//! Markdown list detection and primitive parsing.
//!
//! Pure: takes a `&str` source plus a byte offset and returns parsed
//! information about the list (if any) surrounding that offset.  The edit
//! operations live in `super::edit`.

use crate::document::EditDelta;

/// Parsed view of a Markdown list found in the source buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListInfo {
    /// Byte offset of the first byte of the list (start of the first item,
    /// including its indent).
    pub start: usize,
    /// Byte offset just past the last byte of the list (including the final
    /// item's trailing `\n`, if any).
    pub end: usize,
    /// Leading whitespace (spaces/tabs) before each item's marker.  All items
    /// in a single `ListInfo` share the same indent.
    pub indent: String,
    /// Marker family: bullet character or ordered-list delimiter.
    pub kind: MarkerKind,
    /// Items in source order.
    pub items: Vec<ListItemInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    /// Bullet list: the char is one of `-`, `*`, `+`.
    Bullet(char),
    /// Ordered list: the char is the delimiter (`.` or `)`).  Each item stores
    /// its own parsed number.
    Ordered(char),
}

/// A single parsed item in a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItemInfo {
    /// Byte offset of the first byte of this item's first line (including
    /// indent).
    pub start: usize,
    /// Byte offset just past the last byte of this item — for MVP this is the
    /// byte just past the item's line-terminating `\n`, or the end of the
    /// buffer when the final item has no trailing newline.
    pub end: usize,
    /// Byte offset of the first char of the marker (i.e. `start + indent.len()`).
    pub marker_start: usize,
    /// Byte offset just past the marker prefix — `- `, `1. `, etc.  The space
    /// after the marker is included.
    pub marker_end: usize,
    /// Byte offset of the first byte of user content on this line.  Equals
    /// `marker_end` for non-task items; points just past the task-prefix
    /// (e.g. `[ ] `) for task items.
    pub content_start: usize,
    /// Byte offset of the line-terminating `\n`, or the item's `end` if the
    /// line has no trailing newline.  Used for "end of line" checks.
    pub line_end: usize,
    /// For ordered items, the item's parsed number.
    pub number: Option<u64>,
    /// `None` = not a task item; `Some(false)` = `[ ]`; `Some(true)` = `[x]`.
    pub task: Option<bool>,
    /// Byte offset of the `[` in the task checkbox (if `task.is_some()`).
    pub task_box: Option<usize>,
}

impl ListItemInfo {
    /// True if the item's content (after any task prefix) is empty or
    /// whitespace-only.  Used to decide between "continue" and "exit" on Enter.
    pub fn content_is_empty(&self, source: &str) -> bool {
        let slice = &source[self.content_start..self.line_end];
        slice.trim().is_empty()
    }
}

/// Result type re-exported by the facade.  Stored here so `parse.rs` can
/// reference the type used by `edit.rs`'s public functions without
/// introducing a back-edge dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinueResult {
    pub delta: EditDelta,
    pub cursor_byte: usize,
}

/// Find the Markdown list containing byte offset `cursor_byte` in `source`.
/// Returns `None` when the cursor is not on a list item line or when the line
/// does not belong to a list at that indent level.
pub fn find_list_at(source: &str, cursor_byte: usize) -> Option<ListInfo> {
    if source.is_empty() {
        return None;
    }
    let bytes = source.as_bytes();
    let clamped = cursor_byte.min(source.len());
    let line_start = line_start_byte(bytes, clamped);
    let line_end = line_end_byte(bytes, line_start);
    let cursor_line = &source[line_start..line_end];

    let (indent, kind, _num) = parse_line_start(cursor_line)?;

    // Scan upward for contiguous item lines at the same indent and kind.
    let mut first_start = line_start;
    while first_start > 0 {
        let prev_end = first_start - 1;
        // prev_end is the `\n` separating the previous line from this one.
        // If prev_end is 0 and not a \n, there is no previous line.
        if bytes.get(prev_end).copied() != Some(b'\n') {
            break;
        }
        let prev_start = line_start_byte(bytes, prev_end);
        let prev = &source[prev_start..prev_end];
        if matches_list_line(prev, &indent, kind) {
            first_start = prev_start;
        } else {
            break;
        }
    }

    // Scan downward for contiguous item lines at the same indent and kind.
    let mut last_end = line_end;
    while last_end < source.len() && bytes[last_end] == b'\n' {
        let next_start = last_end + 1;
        if next_start >= source.len() {
            // Buffer ends with the \n; no more lines after it.
            last_end += 1;
            break;
        }
        let next_end = line_end_byte(bytes, next_start);
        let next = &source[next_start..next_end];
        if matches_list_line(next, &indent, kind) {
            last_end = next_end;
        } else {
            break;
        }
    }
    // Extend last_end past the final `\n` of the last item (if present) so
    // that item.end covers the terminating newline.
    if last_end < source.len() && bytes[last_end] == b'\n' {
        last_end += 1;
    }

    let items = parse_items(source, first_start, last_end, &indent, kind)?;
    if items.is_empty() {
        return None;
    }

    Some(ListInfo {
        start: first_start,
        end: last_end,
        indent,
        kind,
        items,
    })
}

/// Parse the marker at the start of `line` (a raw line without its trailing
/// `\n`).  Returns `(indent, kind, number)` where `number` is `Some` for
/// ordered items.
pub(super) fn parse_line_start(line: &str) -> Option<(String, MarkerKind, Option<u64>)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let indent = line[..i].to_owned();
    let rest = &line[i..];
    let rb = rest.as_bytes();

    if let Some(&c) = rb.first() {
        if (c == b'-' || c == b'*' || c == b'+') && rb.get(1) == Some(&b' ') {
            return Some((indent, MarkerKind::Bullet(c as char), None));
        }
    }

    let digits_len = rb.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits_len > 0 {
        let num: u64 = rest[..digits_len].parse().ok()?;
        let delim = *rb.get(digits_len)?;
        if (delim == b'.' || delim == b')') && rb.get(digits_len + 1) == Some(&b' ') {
            return Some((indent, MarkerKind::Ordered(delim as char), Some(num)));
        }
    }

    None
}

/// Does `line` start with a marker of the given kind at the given indent?
pub(super) fn matches_list_line(line: &str, indent: &str, kind: MarkerKind) -> bool {
    let Some((line_indent, line_kind, _)) = parse_line_start(line) else {
        return false;
    };
    if line_indent != indent {
        return false;
    }
    match (kind, line_kind) {
        (MarkerKind::Bullet(a), MarkerKind::Bullet(b)) => a == b,
        (MarkerKind::Ordered(a), MarkerKind::Ordered(b)) => a == b,
        _ => false,
    }
}

/// Parse the range `start..end` into `ListItemInfo`s — assumes every line in
/// the range is a valid list item line at `indent` / `kind`.
pub(super) fn parse_items(
    source: &str,
    start: usize,
    end: usize,
    indent: &str,
    kind: MarkerKind,
) -> Option<Vec<ListItemInfo>> {
    let bytes = source.as_bytes();
    let mut items: Vec<ListItemInfo> = Vec::new();
    let mut cursor = start;
    while cursor < end {
        let item = parse_single_item(source, bytes, cursor, end, indent, kind)?;
        cursor = item.end;
        items.push(item);
    }
    Some(items)
}

/// Parse a single item starting at byte `cursor` (assumed to be a line
/// start).  Returns `None` if the line at `cursor` is not a valid item of
/// the given indent/kind family.
fn parse_single_item(
    source: &str,
    bytes: &[u8],
    cursor: usize,
    end: usize,
    indent: &str,
    kind: MarkerKind,
) -> Option<ListItemInfo> {
    let line_end_pos = line_end_byte(bytes, cursor);
    let line = &source[cursor..line_end_pos];
    let (line_indent, line_kind, number) = parse_line_start(line)?;
    if line_indent != indent {
        return None;
    }
    let marker_start = cursor + line_indent.len();
    let marker_text_len = match line_kind {
        MarkerKind::Bullet(_) => 2, // `<c> `
        MarkerKind::Ordered(_) => {
            let after = &line[line_indent.len()..];
            let digits = after.bytes().take_while(|b| b.is_ascii_digit()).count();
            digits + 2 // digits + delim + space
        }
    };
    let marker_end = marker_start + marker_text_len;
    // Kind consistency: the first item's kind has already been determined;
    // we accept matches_list_line-compatible kinds.
    match (kind, line_kind) {
        (MarkerKind::Bullet(a), MarkerKind::Bullet(b)) if a == b => {}
        (MarkerKind::Ordered(a), MarkerKind::Ordered(b)) if a == b => {}
        _ => return None,
    }

    // Task detection: the bytes immediately after the marker must be
    // `[ ] ` or `[x] `/`[X] ` for an item to be a task.  Anything else
    // (including `[ ]` with no trailing space, or any other text) is
    // treated as plain content.
    let after_marker = &source[marker_end..line_end_pos];
    let (task, task_box, content_start) = if after_marker.starts_with("[ ] ") {
        (Some(false), Some(marker_end), marker_end + 4)
    } else if after_marker.starts_with("[x] ") || after_marker.starts_with("[X] ") {
        (Some(true), Some(marker_end), marker_end + 4)
    } else {
        (None, None, marker_end)
    };

    // The item extends to just past the terminating `\n`.  The final item
    // in the buffer may have no trailing newline.
    let item_end = if line_end_pos < end && bytes[line_end_pos] == b'\n' {
        line_end_pos + 1
    } else {
        line_end_pos
    };
    Some(ListItemInfo {
        start: cursor,
        end: item_end,
        marker_start,
        marker_end,
        content_start,
        line_end: line_end_pos,
        number,
        task,
        task_box,
    })
}

/// Return the index of the item that contains `cursor_byte`, or `None` if the
/// cursor lies between items (e.g. on a blank line somewhere the parser
/// included).
pub fn cursor_item_idx(info: &ListInfo, cursor_byte: usize) -> Option<usize> {
    for (i, item) in info.items.iter().enumerate() {
        if cursor_byte >= item.start && cursor_byte < item.end {
            return Some(i);
        }
    }
    // Cursor may be at the very end of the list (past the final newline).
    if cursor_byte == info.end && !info.items.is_empty() {
        return Some(info.items.len() - 1);
    }
    None
}

pub(super) fn line_start_byte(bytes: &[u8], pos: usize) -> usize {
    let mut p = pos.min(bytes.len());
    while p > 0 && bytes[p - 1] != b'\n' {
        p -= 1;
    }
    p
}

pub(super) fn line_end_byte(bytes: &[u8], start: usize) -> usize {
    let mut p = start;
    while p < bytes.len() && bytes[p] != b'\n' {
        p += 1;
    }
    p
}
