//! List structure-editing primitives: [`ListInfo`] + source + cursor byte in, a
//! `ContinueResult` (delta and post-edit cursor) out.  Pure; callers feed the delta through
//! `apply_byte_delta`.

use crate::document::EditDelta;
use crate::editor::list_edit::parse::{
    cursor_item_idx, line_end_byte, line_start_byte, parse_line_start, ContinueResult, ListInfo,
    ListItemInfo, MarkerKind,
};

/// Continue the list at `cursor_byte` with a new empty item, renumbering later ordered
/// items.  `None` (caller falls through to a plain newline) when the cursor is inside the
/// marker prefix, mid-continuation-line, or outside every item.
pub fn continue_item(info: &ListInfo, source: &str, cursor_byte: usize) -> Option<ContinueResult> {
    let item_idx = cursor_item_idx(info, cursor_byte)?;
    let item = &info.items[item_idx];

    if cursor_byte < item.marker_end {
        return None;
    }

    // On a continuation line only the very end of the item continues; splitting a
    // continuation paragraph must not mint a new marker.
    if cursor_byte > item.line_end {
        let content_end = if item.end > item.start && source.as_bytes()[item.end - 1] == b'\n' {
            item.end - 1
        } else {
            item.end
        };
        if cursor_byte != content_end {
            return None;
        }
    }

    let new_number = match info.kind {
        MarkerKind::Ordered(_) => item.number.unwrap_or(0) + 1,
        MarkerKind::Bullet(_) => 0,
    };
    let marker_text = render_marker(&info.indent, info.kind, new_number);
    let task_prefix = if item.task.is_some() { "[ ] " } else { "" };
    let new_prefix = format!("{marker_text}{task_prefix}");

    let tail = &source[cursor_byte..item.end];

    let mut rebuilt_rest = String::new();
    rebuilt_rest.push('\n');
    rebuilt_rest.push_str(&new_prefix);
    rebuilt_rest.push_str(tail);

    let cursor_target = cursor_byte + 1 + new_prefix.len();

    if let MarkerKind::Ordered(delim) = info.kind {
        for (tail_num, item_next) in (new_number + 1..).zip(&info.items[item_idx + 1..]) {
            let renumbered_line = format!(
                "{}{}{delim} {}",
                info.indent,
                tail_num,
                trim_marker_prefix(source, item_next),
            );
            rebuilt_rest.push_str(&renumbered_line);
        }
    } else {
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

/// Break the list at the cursor's empty item — the last step of the triple-`Enter`
/// sequence (`continue_item` → [`space_out_empty_item`] → `exit_list`).  With a blank line
/// already above, the marker is simply stripped; without one, a single newline is inserted
/// so the parser's blank-line split still separates head from tail.  Trailing ordered items
/// are renumbered from 1 since they render as a fresh list.
pub fn exit_list(info: &ListInfo, source: &str, cursor_byte: usize) -> Option<ContinueResult> {
    let item_idx = cursor_item_idx(info, cursor_byte)?;
    let item = &info.items[item_idx];
    if !item.content_is_empty(source) {
        return None;
    }

    let trailing = &info.items[item_idx + 1..];
    let blank_above = is_blank_line_above(source, item.start);

    if trailing.is_empty() {
        let removed = source[item.start..item.end].to_owned();
        let (inserted, cursor_target) = if blank_above {
            (String::new(), item.start)
        } else {
            ("\n".to_owned(), item.start + 1)
        };
        return Some(ContinueResult {
            delta: EditDelta {
                offset: item.start,
                removed,
                inserted,
            },
            cursor_byte: cursor_target,
        });
    }

    let mut inserted = if blank_above {
        String::new()
    } else {
        String::from("\n")
    };
    match info.kind {
        MarkerKind::Ordered(delim) => {
            for (k, trailing_item) in trailing.iter().enumerate() {
                let new_num = (k as u64) + 1;
                inserted.push_str(&info.indent);
                inserted.push_str(&new_num.to_string());
                inserted.push(delim);
                inserted.push(' ');
                inserted.push_str(&source[trailing_item.marker_end..trailing_item.end]);
            }
        }
        MarkerKind::Bullet(_) => {
            let tail_start = trailing[0].start;
            inserted.push_str(&source[tail_start..info.end]);
        }
    }

    let removed = source[item.start..info.end].to_owned();
    // Cursor lands on the separating blank line.
    let cursor_target = if blank_above {
        item.start.saturating_sub(1)
    } else {
        item.start
    };

    Some(ContinueResult {
        delta: EditDelta {
            offset: item.start,
            removed,
            inserted,
        },
        cursor_byte: cursor_target,
    })
}

/// Insert one blank line above an already-empty item, keeping the item and cursor in place:
/// the second step of the triple-`Enter` sequence.  `None` for a non-empty item (callers
/// route those to [`continue_item`]).
pub fn space_out_empty_item(
    info: &ListInfo,
    source: &str,
    cursor_byte: usize,
) -> Option<ContinueResult> {
    let item_idx = cursor_item_idx(info, cursor_byte)?;
    let item = &info.items[item_idx];
    if !item.content_is_empty(source) {
        return None;
    }
    Some(ContinueResult {
        delta: EditDelta {
            offset: item.start,
            removed: String::new(),
            inserted: "\n".to_owned(),
        },
        cursor_byte: cursor_byte + 1,
    })
}

/// True when the line above `item_start` is blank, or there is none — so an empty item on
/// the buffer's first line exits on one `Enter` instead of spending a keystroke on a gap.
pub fn is_blank_line_above(source: &str, item_start: usize) -> bool {
    if item_start == 0 {
        return true;
    }
    let bytes = source.as_bytes();
    if item_start > bytes.len() || bytes[item_start - 1] != b'\n' {
        return false;
    }
    let mut prev_line_start = item_start - 1;
    while prev_line_start > 0 && bytes[prev_line_start - 1] != b'\n' {
        prev_line_start -= 1;
    }
    let prev_line = &source[prev_line_start..item_start - 1];
    prev_line.chars().all(char::is_whitespace)
}

/// Toggle the checkbox of the task item at `cursor_byte`.
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
    let new_char = if checked { ' ' } else { 'x' };
    let removed = source[box_off + 1..box_off + 2].to_owned();
    let inserted = new_char.to_string();
    Some(ContinueResult {
        delta: EditDelta {
            offset: box_off + 1,
            removed,
            inserted,
        },
        cursor_byte,
    })
}

/// Indent the item at `cursor_byte` by `indent_width` spaces into a nested list; an ordered
/// item restarts at 1 and the outer items renumber around it.  `None` on the list's first
/// item: with no sibling to nest under, the indent would degrade the marker into a lazy
/// continuation or an indented code block.
pub fn indent_item(
    info: &ListInfo,
    source: &str,
    cursor_byte: usize,
    indent_width: usize,
) -> Option<ContinueResult> {
    if indent_width == 0 {
        return None;
    }
    let item_idx = cursor_item_idx(info, cursor_byte)?;
    if item_idx == 0 {
        return None;
    }
    let tab_str: String = " ".repeat(indent_width);

    if let MarkerKind::Bullet(_) = info.kind {
        let item = &info.items[item_idx];
        let text = &source[item.start..item.end];
        let mut out = String::new();
        // An empty indented `    - ` parses as a setext H2 underline of the previous item
        // (CommonMark 4.3); a blank line before it forces the nested-list reading.
        let mut shift_before_cursor = 0usize;
        if item.content_is_empty(source) {
            out.push('\n');
            shift_before_cursor += 1;
        }
        let mut pos = item.start;
        for line in text.split_inclusive('\n') {
            if !line.trim().is_empty() {
                out.push_str(&tab_str);
                if cursor_byte >= pos {
                    shift_before_cursor += indent_width;
                }
            }
            out.push_str(line);
            pos += line.len();
        }
        return Some(ContinueResult {
            delta: EditDelta {
                offset: item.start,
                removed: text.to_owned(),
                inserted: out,
            },
            cursor_byte: cursor_byte + shift_before_cursor,
        });
    }

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
        // Same setext guard as the bullet branch.
        if i == item_idx && item.content_is_empty(source) {
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
            let in_item = cursor_byte.saturating_sub(item.marker_end).min(rest.len());
            let mut extra_before_cursor = 0usize;
            let mut pos = 0usize;
            for (li, line) in rest.split_inclusive('\n').enumerate() {
                if li > 0 && !line.trim().is_empty() {
                    out.push_str(&tab_str);
                    if in_item >= pos {
                        extra_before_cursor += indent_width;
                    }
                }
                out.push_str(line);
                pos += line.len();
            }
            cursor_out = marker_out_start + new_marker.len() + in_item + extra_before_cursor;
        } else {
            out.push_str(rest);
        }
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

/// Outdent the item at `cursor_byte` by up to `indent_width` leading spaces on each of its
/// non-blank lines; `None` at the outermost level.
pub fn outdent_item(
    info: &ListInfo,
    source: &str,
    cursor_byte: usize,
    indent_width: usize,
) -> Option<ContinueResult> {
    if indent_width == 0 {
        return None;
    }
    let item_idx = cursor_item_idx(info, cursor_byte)?;
    let item = &info.items[item_idx];
    let indent_len = info.indent.len();
    if indent_len == 0 {
        return None;
    }
    let strip = indent_width.min(indent_len);

    // A hand-written continuation line with less indent than `strip` loses only what it has.
    let text = &source[item.start..item.end];
    let mut out = String::new();
    let mut removed_before_cursor = 0usize;
    let mut pos = item.start;
    for line in text.split_inclusive('\n') {
        let lead = line.chars().take_while(|&c| c == ' ' || c == '\t').count();
        let s = if line.trim().is_empty() {
            0
        } else {
            strip.min(lead)
        };
        out.push_str(&line[s..]);
        if cursor_byte >= pos + s {
            removed_before_cursor += s;
        } else if cursor_byte > pos {
            removed_before_cursor += cursor_byte - pos;
        }
        pos += line.len();
    }
    Some(ContinueResult {
        delta: EditDelta {
            offset: item.start,
            removed: text.to_owned(),
            inserted: out,
        },
        cursor_byte: cursor_byte - removed_before_cursor,
    })
}

/// Renumber every ordered run in the list block around `cursor_byte`, nesting-aware.  Pure
/// (no parse), so cheap enough for the post-edit hook on every keystroke.
///
/// The block is the maximal run of list lines, crossing *loose-list blank gaps* (a blank
/// run whose next non-blank line is a list line) but bounded by non-list content, matching
/// pulldown-cmark's grouping so the numbers match the render.  Crossing into a bullet or
/// differently-delimited run is harmless: the counter restarts on a delimiter change.
///
/// `None` when the cursor is not on a list line (interior blanks included) or nothing
/// changes, so callers record no spare undo step.
pub fn renumber_list_block(source: &str, cursor_byte: usize) -> Option<EditDelta> {
    if source.is_empty() {
        return None;
    }
    let bytes = source.as_bytes();
    let clamped = cursor_byte.min(source.len());
    let cur_start = line_start_byte(bytes, clamped);
    let cur_content_end = line_end_byte(bytes, cur_start);
    let cur_line = &source[cur_start..cur_content_end];
    if parse_line_start(cur_line).is_none() && !is_block_continuation_line(cur_line) {
        return None;
    }

    let line_end_incl = |content_end: usize| {
        if content_end < source.len() && bytes[content_end] == b'\n' {
            content_end + 1
        } else {
            content_end
        }
    };

    // A list line commits the block start; a blank is absorbed only when a list line
    // further up commits past it.
    let mut block_start = cur_start;
    let mut probe = cur_start;
    while probe > 0 {
        let prev_nl = probe - 1;
        if bytes[prev_nl] != b'\n' {
            break;
        }
        let ps = line_start_byte(bytes, prev_nl);
        let prev = &source[ps..prev_nl];
        if parse_line_start(prev).is_some() || is_block_continuation_line(prev) {
            block_start = ps;
        } else if !prev.trim().is_empty() {
            break;
        }
        probe = ps;
    }

    let mut block_end = line_end_incl(cur_content_end);
    let mut probe = block_end;
    while probe < source.len() {
        let next_end = line_end_byte(bytes, probe);
        let next = &source[probe..next_end];
        let after = line_end_incl(next_end);
        if parse_line_start(next).is_some() || is_block_continuation_line(next) {
            block_end = after;
        } else if !next.trim().is_empty() {
            break;
        }
        probe = after;
    }

    renumber_ordered_runs_in_range(source, block_start, block_end)
}

/// Indented non-blank line, counted into the block by [`renumber_list_block`] without a
/// list-identity check — over-inclusion is harmless since only ordered markers get rewritten.
fn is_block_continuation_line(line: &str) -> bool {
    (line.starts_with(' ') || line.starts_with('\t')) && !line.trim().is_empty()
}

/// Renumber every ordered run inside `start..end`, nesting-aware: an outer list keeps
/// counting across a nested child, each nested list restarts under its parent, and each
/// run keeps its first number as the start (the renderer's `start.unwrap_or(1)`).  The
/// range must be a whole list block including loose-list blank gaps, as
/// [`renumber_list_block`] computes it; a blank-bounded range would diverge from the render.
/// `None` when nothing changes.
pub fn renumber_ordered_runs_in_range(source: &str, start: usize, end: usize) -> Option<EditDelta> {
    if start >= end || end > source.len() {
        return None;
    }
    let block = &source[start..end];
    let mut out = String::with_capacity(block.len());
    // `(indent_len, delimiter, next_number)`.
    let mut stack: Vec<(usize, char, u64)> = Vec::new();
    let mut changed = false;
    // Marker-shaped lines inside a fenced code block are literal; rewriting them would
    // corrupt the code.
    let mut fence: Option<(char, usize)> = None;
    let mut rest = block;
    while !rest.is_empty() {
        let (line, tail, had_nl) = match rest.find('\n') {
            Some(i) => (&rest[..i], &rest[i + 1..], true),
            None => (rest, "", false),
        };

        let in_fence = match fence {
            Some((c, count)) => {
                if is_closing_fence(line, c, count) {
                    fence = None;
                }
                true
            }
            None => match parse_opening_fence(line) {
                Some((c, count)) => {
                    fence = Some((c, count));
                    true
                }
                None => false,
            },
        };

        match (in_fence, parse_line_start(line)) {
            (false, Some((indent, MarkerKind::Ordered(delim), Some(num)))) => {
                let k = indent.len();
                while stack.last().is_some_and(|&(ki, _, _)| ki > k) {
                    stack.pop();
                }
                let new_num = match stack.last_mut() {
                    Some((ki, d, counter)) if *ki == k && *d == delim => {
                        let n = *counter;
                        *counter += 1;
                        n
                    }
                    _ => {
                        // A same-indent run with another delimiter is a different list.
                        if stack.last().is_some_and(|&(ki, _, _)| ki == k) {
                            stack.pop();
                        }
                        stack.push((k, delim, num + 1));
                        num
                    }
                };
                // Measure the digit run from the source: `01.` is wider than `num`.
                let digits = line[indent.len()..]
                    .bytes()
                    .take_while(u8::is_ascii_digit)
                    .count();
                let marker_len = indent.len() + digits + 2;
                out.push_str(&indent);
                out.push_str(&new_num.to_string());
                out.push(delim);
                out.push(' ');
                out.push_str(&line[marker_len..]);
                changed |= new_num != num;
            }
            (false, other) => {
                // A bullet sibling ends any ordered run at its indent or deeper.
                if let Some((indent, _, _)) = other {
                    let k = indent.len();
                    while stack.last().is_some_and(|&(ki, _, _)| ki >= k) {
                        stack.pop();
                    }
                }
                out.push_str(line);
            }
            (true, _) => {
                out.push_str(line);
            }
        }
        if had_nl {
            out.push('\n');
        }
        rest = tail;
    }

    if !changed {
        return None;
    }
    Some(EditDelta {
        offset: start,
        removed: block.to_owned(),
        inserted: out,
    })
}

/// An opening fence marker: its character (`` ` `` or `~`) and run length.  Indentation of
/// any depth is permitted — inside a list item the fence sits at the content column, and
/// renumbering only needs to know whether a line is inside a fence.
fn parse_opening_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let count = trimmed.chars().take_while(|&c| c == first).count();
    if count < 3 {
        return None;
    }
    // Backtick fences disallow backticks anywhere in the info string.
    if first == '`' && trimmed[count..].contains('`') {
        return None;
    }
    Some((first, count))
}

/// A closing fence for an open `fence_char` × `min_count`: same character, at least as
/// long, whitespace-only after it (CommonMark).
fn is_closing_fence(line: &str, fence_char: char, min_count: usize) -> bool {
    let trimmed = line.trim_start();
    let count = trimmed.chars().take_while(|&c| c == fence_char).count();
    if count < min_count {
        return false;
    }
    trimmed[count..].chars().all(char::is_whitespace)
}

fn render_marker(indent: &str, kind: MarkerKind, number: u64) -> String {
    match kind {
        MarkerKind::Bullet(c) => format!("{indent}{c} "),
        MarkerKind::Ordered(delim) => format!("{indent}{number}{delim} "),
    }
}

/// Everything in `item` after its marker prefix, through `item.end`.
fn trim_marker_prefix<'a>(source: &'a str, item: &ListItemInfo) -> &'a str {
    &source[item.marker_end..item.end]
}
