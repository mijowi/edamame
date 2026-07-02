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
    /// Byte offset just past the last byte of this item, including its
    /// continuation lines (lines indented deeper than the list's own indent,
    /// nested list lines, and any interior blank run followed by such a
    /// line).  Covers the final line's terminating `\n` when present.
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
    /// Byte offset of the FIRST line's terminating `\n`, or that line's end
    /// when it has no trailing newline.  Deliberately a first-line fact even
    /// for multi-line items — marker-adjacent checks (`content_start..
    /// line_end`) only make sense on the marker line.
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
    /// whitespace-only — including any continuation lines, so an item whose
    /// first line is blank but that carries indented continuation content is
    /// NOT empty.  Used to decide between "continue" and "exit" on Enter.
    pub fn content_is_empty(&self, source: &str) -> bool {
        let slice = &source[self.content_start..self.end];
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
/// Returns `None` when the cursor's line neither is a list-item line nor
/// belongs to one as a continuation, or when the line does not belong to a
/// list at that indent level.
///
/// Items may span multiple lines: a non-blank line whose leading whitespace
/// starts with the list's indent and is strictly longer belongs to the item
/// above it (indented continuation paragraphs and nested list lines alike),
/// and an interior blank run belongs to the item iff the first non-blank
/// line after the run is such a continuation.  A blank run followed by a
/// same-indent marker line still terminates the list — that gap is the
/// parser's blank-line list split, two visually distinct lists.
///
/// When the cursor sits on a continuation (or attached blank) line, the list
/// is anchored on the nearest marker line above it, so `cursor_item_idx`
/// resolves the cursor to the item that owns the continuation.
pub fn find_list_at(source: &str, cursor_byte: usize) -> Option<ListInfo> {
    if source.is_empty() {
        return None;
    }
    let bytes = source.as_bytes();
    let clamped = cursor_byte.min(source.len());
    let cursor_line_start = line_start_byte(bytes, clamped);
    let cursor_line_end = line_end_byte(bytes, cursor_line_start);
    let cursor_line = &source[cursor_line_start..cursor_line_end];

    // Anchor: the marker line that owns the cursor's line.  A cursor on a
    // marker line anchors there (a nested marker anchors the nested list);
    // a cursor on a blank or indented line walks up to the nearest marker.
    let anchor_start = if parse_line_start(cursor_line).is_some() {
        cursor_line_start
    } else {
        resolve_anchor_upward(source, bytes, cursor_line_start)?
    };
    let anchor_end = line_end_byte(bytes, anchor_start);
    let (indent, kind, _num) = parse_line_start(&source[anchor_start..anchor_end])?;

    // A cursor on a non-marker line must actually belong to the anchor's
    // list: blank, or a continuation at the anchor's indent.
    if anchor_start != cursor_line_start
        && !cursor_line.trim().is_empty()
        && !is_continuation_line(cursor_line, &indent)
    {
        return None;
    }

    // Scan upward for contiguous lines of this list: marker lines at the
    // same indent and kind, their continuation lines, and attached blank
    // runs.  Only a marker line commits the extension — a run of
    // continuation-shaped lines with no marker above (e.g. an indented
    // block under a paragraph) is discarded.  A blank line whose nearest
    // non-blank line BELOW is a marker line is a list-splitting separator
    // and stops the scan.
    let mut first_start = anchor_start;
    let mut probe = anchor_start;
    let mut below_is_marker = true;
    loop {
        if probe == 0 || bytes[probe - 1] != b'\n' {
            break;
        }
        let ps = line_start_byte(bytes, probe - 1);
        let line = &source[ps..probe - 1];
        if matches_list_line(line, &indent, kind) {
            first_start = ps;
            below_is_marker = true;
        } else if is_continuation_line(line, &indent) {
            below_is_marker = false;
        } else if line.trim().is_empty() {
            if below_is_marker {
                break;
            }
        } else {
            break;
        }
        probe = ps;
    }

    // Scan downward: same-list marker lines, continuation lines, and blank
    // runs that attach (first non-blank line after the run is a
    // continuation).
    let mut last_end = anchor_end;
    while last_end < source.len() && bytes[last_end] == b'\n' {
        let next_start = last_end + 1;
        if next_start >= source.len() {
            break;
        }
        let next_end = line_end_byte(bytes, next_start);
        let next = &source[next_start..next_end];
        if matches_list_line(next, &indent, kind) || is_continuation_line(next, &indent) {
            last_end = next_end;
        } else if next.trim().is_empty() {
            let Some(resume_end) = blank_run_attaches(source, bytes, next_start, &indent) else {
                break;
            };
            last_end = resume_end;
        } else {
            break;
        }
    }
    // Extend last_end past the final `\n` of the last item (if present) so
    // that item.end covers the terminating newline.
    if last_end < source.len() && bytes[last_end] == b'\n' {
        last_end += 1;
    }

    // A cursor on a blank line past the list's end — the separator below it
    // — is not in the list: edits fired there (Tab, ToggleCheckbox, …) must
    // fall back to plain-text handling instead of mutating the item above.
    // Attached interior blank lines start before `last_end` and stay owned.
    if cursor_line.trim().is_empty() && cursor_line_start >= last_end {
        return None;
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

/// Walk upward from a non-marker line to the nearest marker line above it,
/// crossing only blank lines and indented (continuation-shaped) lines.
/// Returns `None` when a flush-left non-marker line (or the buffer start)
/// is reached first — the cursor's line has no list above it to belong to.
fn resolve_anchor_upward(source: &str, bytes: &[u8], cursor_line_start: usize) -> Option<usize> {
    let mut line_start = cursor_line_start;
    loop {
        if line_start == 0 || bytes[line_start - 1] != b'\n' {
            return None;
        }
        let ps = line_start_byte(bytes, line_start - 1);
        let line = &source[ps..line_start - 1];
        if parse_line_start(line).is_some() {
            return Some(ps);
        }
        if !line.trim().is_empty() && !line.starts_with(' ') && !line.starts_with('\t') {
            return None;
        }
        line_start = ps;
    }
}

/// Does `line` extend the item above it in a list indented by `list_indent`?
/// True for a non-blank line whose leading whitespace starts with
/// `list_indent` and is strictly longer — indented continuation paragraphs
/// and deeper nested marker lines alike.  (Lazy continuations at or below
/// the list's own indent are deliberately not recognized.)
pub(super) fn is_continuation_line(line: &str, list_indent: &str) -> bool {
    let lead_len: usize = line
        .chars()
        .take_while(|&c| c == ' ' || c == '\t')
        .map(char::len_utf8)
        .sum();
    lead_len < line.len() // non-blank
        && lead_len > list_indent.len()
        && line[..lead_len].starts_with(list_indent)
}

/// If the blank run starting at `run_start` attaches to the item above it —
/// i.e. the first non-blank line after the run is a continuation line at
/// `list_indent` — return that continuation line's content end (so the
/// caller's scan resumes past it).  Returns `None` when the run is a
/// list-terminating separator (next non-blank is a marker line, a shallower
/// line, or the buffer ends).
fn blank_run_attaches(
    source: &str,
    bytes: &[u8],
    run_start: usize,
    list_indent: &str,
) -> Option<usize> {
    let mut line_start = run_start;
    loop {
        let line_end = line_end_byte(bytes, line_start);
        let line = &source[line_start..line_end];
        if !line.trim().is_empty() {
            return is_continuation_line(line, list_indent).then_some(line_end);
        }
        if line_end >= source.len() {
            return None;
        }
        line_start = line_end + 1;
    }
}

/// Parse the marker at the start of `line` (a raw line without its trailing
/// `\n`).  Returns `(indent, kind, number)` where `number` is `Some` for
/// ordered items.
pub(super) fn parse_line_start(line: &str) -> Option<(String, MarkerKind, Option<u64>)> {
    let indent_len: usize = line
        .chars()
        .take_while(|&c| c == ' ' || c == '\t')
        .map(char::len_utf8)
        .sum();
    let indent = line[..indent_len].to_owned();
    let rest = &line[indent_len..];
    let mut chars = rest.chars();

    let first = chars.next()?;
    if matches!(first, '-' | '*' | '+') && chars.next() == Some(' ') {
        return Some((indent, MarkerKind::Bullet(first), None));
    }

    let digits_len: usize = rest.chars().take_while(char::is_ascii_digit).count();
    if digits_len > 0 {
        let num: u64 = rest[..digits_len].parse().ok()?;
        let mut after = rest[digits_len..].chars();
        let delim = after.next()?;
        if matches!(delim, '.' | ')') && after.next() == Some(' ') {
            return Some((indent, MarkerKind::Ordered(delim), Some(num)));
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

/// Parse the range `start..end` into `ListItemInfo`s — assumes the range
/// starts on a marker line at `indent` / `kind` and that every other line in
/// it is a marker line, a continuation line, or an attached blank (which is
/// what `find_list_at`'s scans produce).
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

    // The item extends past its first line's terminating `\n` and then over
    // every following line inside the caller's range that is not itself a
    // same-list marker line — by the scan's construction those are the
    // item's continuation lines and attached blank runs.  The final line in
    // the buffer may have no trailing newline.
    let past_line = |content_end: usize| {
        if content_end < end && bytes[content_end] == b'\n' {
            content_end + 1
        } else {
            content_end
        }
    };
    let mut item_end = past_line(line_end_pos);
    while item_end < end {
        let next_end = line_end_byte(bytes, item_end);
        if matches_list_line(&source[item_end..next_end], indent, kind) {
            break;
        }
        item_end = past_line(next_end);
    }
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
