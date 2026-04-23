//! Markdown list detection, parsing, and structure-editing primitives.
//!
//! Mirrors `table_edit.rs`: byte-oriented, scans buffer text to find the list
//! surrounding the cursor, and produces `EditDelta` values for continue/exit
//! actions and checkbox toggles.  Rope/char-offset conversions happen in
//! `edit_ops`.
//!
//! A "list" here is a contiguous run of item lines at the same indent and
//! marker family (bullet or ordered).  Blank lines or lines at a different
//! indent terminate the run.  This keeps cursor detection cheap and means the
//! cursor's list is always the innermost list at the cursor's own indent level
//! — which is what we want for Enter-to-continue and ToggleCheckbox.

use crate::document::EditDelta;

// ─── Types ───────────────────────────────────────────────────────────────────

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

// ─── Detection ───────────────────────────────────────────────────────────────

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
fn parse_line_start(line: &str) -> Option<(String, MarkerKind, Option<u64>)> {
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
fn matches_list_line(line: &str, indent: &str, kind: MarkerKind) -> bool {
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
fn parse_items(
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
        items.push(ListItemInfo {
            start: cursor,
            end: item_end,
            marker_start,
            marker_end,
            content_start,
            line_end: line_end_pos,
            number,
            task,
            task_box,
        });
        cursor = item_end;
    }
    Some(items)
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

// ─── Operations ──────────────────────────────────────────────────────────────

/// Result of a `continue_item` call: the `EditDelta` to apply and the byte
/// offset the cursor should land at after the edit (i.e. the start of the new
/// empty item's content, where the user will immediately begin typing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinueResult {
    pub delta: EditDelta,
    pub cursor_byte: usize,
}

/// Build an `EditDelta` that continues the list at `cursor_byte` by inserting
/// a new empty item.  For ordered lists, all items from `item_idx + 1` onward
/// are renumbered so the sequence stays monotonic.
///
/// The cursor must be at or past `marker_end` of the item — otherwise the
/// caller should fall through to a plain newline (a plain newline inside a
/// marker prefix would produce malformed output).
///
/// Returns `None` if `cursor_byte` is not inside any item (e.g. the cursor is
/// at the very end of the list past the final newline).
pub fn continue_item(info: &ListInfo, source: &str, cursor_byte: usize) -> Option<ContinueResult> {
    let item_idx = cursor_item_idx(info, cursor_byte)?;
    let item = &info.items[item_idx];

    // Require cursor to be past the marker prefix.  Being anywhere in
    // `marker_start..marker_end` means the user is inside `- ` or `N. ` — a
    // plain newline is the right behaviour there.
    if cursor_byte < item.marker_end {
        return None;
    }

    // Build the prefix for the new item.  For ordered lists the number is
    // `item.number.unwrap_or(1) + 1` — the caller's renumbering below will
    // fix up subsequent items so this is just the slot-in value at insertion.
    let new_number = match info.kind {
        MarkerKind::Ordered(_) => item.number.unwrap_or(0) + 1,
        MarkerKind::Bullet(_) => 0,
    };
    let marker_text = render_marker(&info.indent, info.kind, new_number);
    let task_prefix = if item.task.is_some() { "[ ] " } else { "" };
    let new_prefix = format!("{marker_text}{task_prefix}");

    // Rest of the current item's line after the cursor — this becomes the new
    // item's content.  Includes the item's trailing newline (if any).
    let tail = &source[cursor_byte..item.end];

    // Subsequent items, renumbered for ordered lists.
    let mut rebuilt_rest = String::new();
    rebuilt_rest.push('\n');
    rebuilt_rest.push_str(&new_prefix);
    // `tail` already begins with what was on the current item's line after
    // the cursor (possibly an inline text chunk + `\n`).  We need to drop any
    // leading `\n` inside `tail` because `rebuilt_rest` already supplied it
    // via its own trailing newline on the preceding item — but wait, we push
    // `\n<new prefix>` and then `tail` which itself ends with `\n`; that is
    // the new item's own trailing newline.  So we don't drop anything here.
    rebuilt_rest.push_str(tail);

    // Compute the cursor target: right after the inserted `\n<new prefix>`.
    let cursor_target = cursor_byte + 1 + new_prefix.len();

    // For ordered lists, renumber subsequent items.  `trim_marker_prefix`
    // already returns the content up through the line's trailing `\n` (if
    // present), so we do not add an extra newline per item.
    if let MarkerKind::Ordered(delim) = info.kind {
        let mut tail_num = new_number + 1;
        for item_next in &info.items[item_idx + 1..] {
            let renumbered_line = format!(
                "{}{}{delim} {}",
                info.indent,
                tail_num,
                trim_marker_prefix(source, item_next),
            );
            rebuilt_rest.push_str(&renumbered_line);
            tail_num += 1;
        }
    } else {
        // Bullets — just append subsequent items unchanged.
        let tail_start = info
            .items
            .get(item_idx + 1)
            .map(|it| it.start)
            .unwrap_or(info.end);
        rebuilt_rest.push_str(&source[tail_start..info.end]);
    }

    let removed = source[cursor_byte..info.end].to_owned();

    Some(ContinueResult {
        delta: EditDelta {
            offset: cursor_byte,
            removed,
            inserted: rebuilt_rest,
        },
        cursor_byte: cursor_target,
    })
}

/// Build an `EditDelta` that exits the list by removing the empty item's
/// marker prefix, leaving a blank line in its place.  The caller should
/// invoke this when `Enter` is pressed on an item whose content is empty.
pub fn exit_list(info: &ListInfo, source: &str, cursor_byte: usize) -> Option<ContinueResult> {
    let item_idx = cursor_item_idx(info, cursor_byte)?;
    let item = &info.items[item_idx];
    if !item.content_is_empty(source) {
        return None;
    }

    // Remove the entire empty item line (including any trailing \n) and
    // replace it with a single `\n` — the blank line separating the list
    // from whatever follows.
    let removed = source[item.start..item.end].to_owned();
    let inserted = "\n".to_owned();
    let cursor_target = item.start + 1;

    Some(ContinueResult {
        delta: EditDelta {
            offset: item.start,
            removed,
            inserted,
        },
        cursor_byte: cursor_target,
    })
}

/// Build an `EditDelta` that toggles the checkbox of the item at `cursor_byte`.
/// Returns `None` if the cursor is not on a task-list item.
pub fn toggle_checkbox(
    info: &ListInfo,
    source: &str,
    cursor_byte: usize,
) -> Option<ContinueResult> {
    let item_idx = cursor_item_idx(info, cursor_byte)?;
    let item = &info.items[item_idx];
    let (checked, box_off) = match (item.task, item.task_box) {
        (Some(c), Some(b)) => (c, b),
        _ => return None,
    };
    // The check char sits at `box_off + 1` — `[` `<check>` `]`.
    let new_char = if checked { ' ' } else { 'x' };
    let removed = source[box_off + 1..box_off + 2].to_owned();
    let inserted = new_char.to_string();
    Some(ContinueResult {
        delta: EditDelta {
            offset: box_off + 1,
            removed,
            inserted,
        },
        // Cursor stays where it was; the caller preserves its position by
        // passing the pre-edit byte through `apply_byte_delta`.
        cursor_byte,
    })
}

/// Build an `EditDelta` that indents the item at `cursor_byte` by `tab_width`
/// spaces, producing a new nested list one level deeper.  For ordered lists,
/// the indented item's number is reset to `1` (it starts a fresh nested
/// sequence) and the remaining outer items are renumbered to fill the gap.
/// For bullet lists the edit is just `tab_width` spaces prepended to the
/// item's line.  Returns `None` when `cursor_byte` is not inside any item.
pub fn indent_item(
    info: &ListInfo,
    source: &str,
    cursor_byte: usize,
    tab_width: usize,
) -> Option<ContinueResult> {
    if tab_width == 0 {
        return None;
    }
    let item_idx = cursor_item_idx(info, cursor_byte)?;
    let tab_str: String = " ".repeat(tab_width);

    // Bullet lists: the nested item can be emitted as an independent edit
    // (just prepend the extra indent) because there is no renumbering to do.
    if let MarkerKind::Bullet(_) = info.kind {
        let item = &info.items[item_idx];
        // When the item has no content, pulldown-cmark interprets the
        // indented `    - ` as a setext H2 underline of the previous item's
        // paragraph rather than a nested list marker (see CommonMark 4.3).
        // Inserting a blank line separator between the parent and the
        // indented marker forces the nested-list interpretation.  Non-empty
        // items aren't affected because the trailing content breaks the
        // setext pattern.
        let inserted = if item.content_is_empty(source) && item_idx > 0 {
            format!("\n{tab_str}")
        } else {
            tab_str.clone()
        };
        let cursor_target = cursor_byte + inserted.len();
        let delta = EditDelta {
            offset: item.start,
            removed: String::new(),
            inserted,
        };
        return Some(ContinueResult {
            delta,
            cursor_byte: cursor_target,
        });
    }

    // Ordered lists: rewrite the entire run so the outer items renumber
    // contiguously with the indented item removed from the sequence.  The
    // indented item becomes the sole starting member of a fresh nested list
    // at number 1.
    let MarkerKind::Ordered(delim) = info.kind else {
        unreachable!();
    };
    let base = info.items[0].number.unwrap_or(1);
    let mut out = String::new();
    let mut cursor_out: usize = 0;
    let mut outer_counter = base;
    let nested_indent = format!("{}{}", info.indent, tab_str);

    for (i, item) in info.items.iter().enumerate() {
        let rest = &source[item.marker_end..item.end];
        // Insert a blank-line separator before an empty indented item so
        // pulldown-cmark recognises it as a nested list rather than lazy
        // paragraph continuation of the preceding item.
        if i == item_idx && i > 0 && item.content_is_empty(source) {
            out.push('\n');
        }
        let new_marker = if i == item_idx {
            format!("{nested_indent}1{delim} ")
        } else {
            let m = format!("{}{outer_counter}{delim} ", info.indent);
            outer_counter += 1;
            m
        };
        let marker_out_start = out.len();
        out.push_str(&new_marker);
        if i == item_idx {
            // Cursor's byte offset inside the original item, measured from
            // the old marker_end.  For positions before marker_end (indent
            // or digits), the saturating_sub yields 0 so the cursor lands
            // immediately after the new marker.
            let in_item = cursor_byte.saturating_sub(item.marker_end);
            cursor_out = marker_out_start + new_marker.len() + in_item.min(rest.len());
        }
        out.push_str(rest);
    }

    let removed = source[info.start..info.end].to_owned();
    Some(ContinueResult {
        delta: EditDelta {
            offset: info.start,
            removed,
            inserted: out,
        },
        cursor_byte: info.start + cursor_out,
    })
}

/// Build an `EditDelta` that outdents the item at `cursor_byte` by removing
/// up to `tab_width` leading spaces from the front of that item's first
/// line.  Returns `None` when the item is already at the outermost level
/// (no indent to strip) or when `cursor_byte` is outside every item.
pub fn outdent_item(
    info: &ListInfo,
    source: &str,
    cursor_byte: usize,
    tab_width: usize,
) -> Option<ContinueResult> {
    if tab_width == 0 {
        return None;
    }
    let item_idx = cursor_item_idx(info, cursor_byte)?;
    let item = &info.items[item_idx];
    let indent_len = info.indent.len();
    if indent_len == 0 {
        return None;
    }
    let strip = tab_width.min(indent_len);
    let removed = source[item.start..item.start + strip].to_owned();
    let delta = EditDelta {
        offset: item.start,
        removed,
        inserted: String::new(),
    };
    // The cursor shifts left by `strip` bytes if it sat past the stripped
    // region; otherwise it tracks the line's new start.
    let cursor_target = if cursor_byte >= item.start + strip {
        cursor_byte - strip
    } else {
        item.start
    };
    Some(ContinueResult {
        delta,
        cursor_byte: cursor_target,
    })
}

/// Rewrite every ordered item's number so they form a monotonic sequence
/// starting from the first item's parsed number (falling back to 1).  No-op
/// for bullet lists.  Returns `None` when the list is already consistent.
pub fn renumber_list(info: &ListInfo, source: &str) -> Option<EditDelta> {
    let MarkerKind::Ordered(delim) = info.kind else {
        return None;
    };
    let base = info.items.first()?.number.unwrap_or(1);
    // Check whether the list is already consistent; if so, skip the edit.
    let already_sequential = info
        .items
        .iter()
        .enumerate()
        .all(|(offset, it)| it.number == Some(base + offset as u64));
    if already_sequential {
        return None;
    }

    let removed = source[info.start..info.end].to_owned();
    let mut inserted = String::with_capacity(removed.len());
    for (i, item) in info.items.iter().enumerate() {
        let num = base + i as u64;
        let rest = trim_marker_prefix(source, item);
        inserted.push_str(&format!("{}{}{delim} {}", info.indent, num, rest));
    }

    Some(EditDelta {
        offset: info.start,
        removed,
        inserted,
    })
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn render_marker(indent: &str, kind: MarkerKind, number: u64) -> String {
    match kind {
        MarkerKind::Bullet(c) => format!("{indent}{c} "),
        MarkerKind::Ordered(delim) => format!("{indent}{number}{delim} "),
    }
}

/// Return the slice of `item` that sits after its marker prefix (i.e. the
/// item's "rest of line" plus any continuation content up to `item.end`).
/// Used when renumbering — we preserve everything except the `N. ` prefix.
fn trim_marker_prefix<'a>(source: &'a str, item: &ListItemInfo) -> &'a str {
    &source[item.marker_end..item.end]
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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn info_at(source: &str, cursor_byte: usize) -> ListInfo {
        find_list_at(source, cursor_byte).expect("expected a list at cursor")
    }

    #[test]
    fn finds_simple_bullet_list() {
        let src = "- a\n- b\n- c\n";
        let info = info_at(src, 2); // inside "- a"
        assert_eq!(info.items.len(), 3);
        assert_eq!(info.kind, MarkerKind::Bullet('-'));
        assert_eq!(info.indent, "");
    }

    #[test]
    fn finds_ordered_list_with_numbers() {
        let src = "1. one\n2. two\n3. three\n";
        let info = info_at(src, 5);
        assert_eq!(info.items.len(), 3);
        assert_eq!(info.kind, MarkerKind::Ordered('.'));
        assert_eq!(info.items[0].number, Some(1));
        assert_eq!(info.items[2].number, Some(3));
    }

    #[test]
    fn detects_task_items() {
        let src = "- [ ] todo\n- [x] done\n";
        let info = info_at(src, 3);
        assert_eq!(info.items[0].task, Some(false));
        assert_eq!(info.items[1].task, Some(true));
    }

    #[test]
    fn none_outside_list() {
        let src = "just text\n";
        assert!(find_list_at(src, 5).is_none());
    }

    #[test]
    fn nested_list_scoped_to_indent() {
        let src = "- outer\n  - inner1\n  - inner2\n- outer2\n";
        // Cursor inside "  - inner1" (byte 12)
        let info = info_at(src, 12);
        assert_eq!(info.items.len(), 2);
        assert_eq!(info.indent, "  ");
    }

    #[test]
    fn continue_item_at_end_of_line() {
        let src = "- foo\n";
        let info = info_at(src, 5);
        let res = continue_item(&info, src, 5).expect("continue");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- foo\n- \n");
        assert_eq!(res.cursor_byte, 8);
    }

    #[test]
    fn continue_renumbers_subsequent_ordered_items() {
        let src = "1. a\n2. b\n3. c\n";
        let info = info_at(src, 4);
        let res = continue_item(&info, src, 4).expect("continue");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "1. a\n2. \n3. b\n4. c\n");
    }

    #[test]
    fn exit_list_removes_empty_marker() {
        let src = "- foo\n- \n";
        let info = info_at(src, 8);
        let res = exit_list(&info, src, 8).expect("exit");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- foo\n\n");
    }

    #[test]
    fn toggle_checkbox_flips_state() {
        let src = "- [x] done\n";
        let info = info_at(src, 6);
        let res = toggle_checkbox(&info, src, 6).expect("toggle");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- [ ] done\n");
    }

    #[test]
    fn renumber_list_fixes_disordered_numbers() {
        let src = "1. a\n1. b\n1. c\n";
        let info = info_at(src, 0);
        let delta = renumber_list(&info, src).expect("renumber");
        let mut out = src.to_owned();
        out.replace_range(
            delta.offset..delta.offset + delta.removed.len(),
            &delta.inserted,
        );
        assert_eq!(out, "1. a\n2. b\n3. c\n");
    }

    #[test]
    fn renumber_list_noop_when_already_sequential() {
        let src = "1. a\n2. b\n3. c\n";
        let info = info_at(src, 0);
        assert!(renumber_list(&info, src).is_none());
    }

    #[test]
    fn continue_item_rejects_cursor_in_marker() {
        let src = "- foo\n";
        let info = info_at(src, 1); // between `-` and ` `
        assert!(continue_item(&info, src, 1).is_none());
    }
}
