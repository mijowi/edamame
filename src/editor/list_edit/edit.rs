//! List structure-editing primitives.
//!
//! Each public function takes the parsed [`ListInfo`] (built by
//! `super::parse::find_list_at`), the source text, and a cursor byte offset,
//! and returns a `ContinueResult` carrying the `EditDelta` to apply and the
//! post-edit cursor target.  Pure: no `EditorState` mutation here — callers
//! convert byte offsets to char offsets and feed the delta through
//! `apply_byte_delta` themselves.

use crate::document::EditDelta;
use crate::editor::list_edit::parse::{
    cursor_item_idx, line_end_byte, line_start_byte, parse_line_start, ContinueResult, ListInfo,
    ListItemInfo, MarkerKind,
};

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

    // Multi-line items: list continuation fires from the marker line or from
    // the very end of the item's last line (a new sibling after the whole
    // item).  Anywhere else on a continuation line, Enter is a plain newline
    // — splitting a continuation paragraph shouldn't mint a new marker.
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

/// Build an `EditDelta` that breaks the list at the cursor's empty item.
/// The caller is expected to invoke this only after a blank line has
/// already been positioned directly above the empty item — i.e. as the
/// final step of the triple-`Enter` list-break sequence
/// (`continue_item` → [`space_out_empty_item`] → `exit_list`).  When
/// `blank_above` is `true` the function leaves the existing blank line
/// in place and simply strips the empty marker; when called without a
/// blank line above (e.g. from a direct unit test), it inserts a single
/// newline so the surviving head and any trailing items end up
/// separated by one blank line — which the parser's blank-line list
/// split treats as a list-fragmenting boundary.
///
/// When there are remaining items below the empty exit item AND the list
/// is ordered, those trailing items are rewritten so their numbering
/// restarts at 1.  Combined with the parser's blank-line list split, the
/// trailing items render as a separate ordered list with their own
/// numbering.  Bullet lists need no renumbering.
pub fn exit_list(info: &ListInfo, source: &str, cursor_byte: usize) -> Option<ContinueResult> {
    let item_idx = cursor_item_idx(info, cursor_byte)?;
    let item = &info.items[item_idx];
    if !item.content_is_empty(source) {
        return None;
    }

    let trailing = &info.items[item_idx + 1..];
    let blank_above = is_blank_line_above(source, item.start);

    if trailing.is_empty() {
        // No items below.  When a blank line already sits above the empty
        // item, stripping the marker is enough — the existing blank plus
        // the cursor's now-empty line produce the two-lines-below-the-list
        // resting state.  Without that blank, we fall back to the older
        // "replace the marker with a single newline" behaviour so direct
        // callers (and edge-case unit tests) still get a sensible result.
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

    // Items remain after the empty exit item.  The post-pass splits
    // lists at any blank line outside fenced code blocks, so a single
    // blank line between the surviving head and the renumbered trailing
    // items is enough to make the parser split them into two visually
    // distinct lists.  When a blank line is already sitting above the
    // empty item it can carry the gap by itself; otherwise we insert
    // exactly one newline.
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
    // The cursor lands on the blank line that separates the surviving
    // head from the renumbered trailing list.  With a pre-existing
    // blank line above the empty item, that's the byte just before
    // `item.start` (the `\n` that terminates the existing blank line);
    // without one, the function inserted a newline at `item.start` and
    // the cursor sits on it.
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

/// Build an `EditDelta` that widens the gap above an already-empty list
/// item by one blank line, keeping the empty item — and the cursor on it
/// — in place.  This is the second step of the triple-`Enter`
/// list-break sequence: the user has pressed `Enter` on a content-empty
/// item, but a section break has not been requested yet, so the editor
/// gives the user one more visual line of separation before committing
/// to actually leaving the list on the next press.
///
/// Returns `None` if `cursor_byte` is not inside any item, or if the
/// item at the cursor is not content-empty (callers should funnel
/// non-empty items to [`continue_item`] instead).
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
        // Inserting one byte at `item.start` shifts every byte at or
        // beyond it forward by one.  The cursor is by construction past
        // the marker (i.e. >= item.start), so it shifts too.
        cursor_byte: cursor_byte + 1,
    })
}

/// True iff the line directly above `item_start` is empty (whitespace-only),
/// or `item_start == 0`.  Treating "no line above" as blank means an empty
/// list item that occupies the very first line of the buffer is exited
/// immediately on `Enter` rather than spending a keystroke on a gap that
/// has nowhere to go.
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

/// Build an `EditDelta` that indents the item at `cursor_byte` by
/// `indent_width` spaces, producing a new nested list one level deeper.  For
/// ordered lists, the indented item's number is reset to `1` (it starts a
/// fresh nested sequence) and the remaining outer items are renumbered to fill
/// the gap.  For bullet lists the edit is just `indent_width` spaces prepended
/// to the item's line.  Returns `None` when `cursor_byte` is not inside any
/// item, or when it is on the list's FIRST item — with no preceding sibling
/// to nest under there is no valid deeper position: the extra indent would
/// degrade the marker into a lazy paragraph continuation of the parent (or
/// an indented code block at the top level).
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

    // Bullet lists: no renumbering to do — rebuild the item's own lines with
    // the extra indent prepended to every non-blank line (the marker line
    // and its continuation lines shift together, keeping their relative
    // depth), leaving attached blank lines untouched.
    if let MarkerKind::Bullet(_) = info.kind {
        let item = &info.items[item_idx];
        let text = &source[item.start..item.end];
        let mut out = String::new();
        // When the item has no content, pulldown-cmark interprets the
        // indented `    - ` as a setext H2 underline of the previous item's
        // paragraph rather than a nested list marker (see CommonMark 4.3).
        // Inserting a blank line separator between the parent and the
        // indented marker forces the nested-list interpretation.  Non-empty
        // items aren't affected because the trailing content breaks the
        // setext pattern.  (There is always a previous item: first items
        // are rejected above.)
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
        // paragraph continuation of the preceding item.  (item_idx > 0
        // always: first items are rejected up front.)
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
            // Cursor's byte offset inside the original item, measured from
            // the old marker_end.  For positions before marker_end (indent
            // or digits), the saturating_sub yields 0 so the cursor lands
            // immediately after the new marker.
            let in_item = cursor_byte.saturating_sub(item.marker_end).min(rest.len());
            // The indented item's continuation lines shift with it: prepend
            // the extra indent to every non-blank line after the first
            // (blank lines stay untouched), tracking how many insertions
            // land at or before the cursor.
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

/// Build an `EditDelta` that outdents the item at `cursor_byte` by removing
/// up to `indent_width` leading spaces from the front of that item's first
/// line.  Returns `None` when the item is already at the outermost level
/// (no indent to strip) or when `cursor_byte` is outside every item.
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

    // Rebuild the item's own lines, stripping up to `strip` leading
    // whitespace chars from every non-blank line (marker line and
    // continuations shift together; blank lines stay untouched).  A
    // continuation line with less leading whitespace than `strip` — not
    // producible by our own indent, but present in hand-written sources —
    // loses only what it has.
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
            // Cursor inside the stripped region: it tracks the line start.
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

/// Renumber every ordered list run inside the contiguous list block
/// surrounding `cursor_byte`, nesting-aware.
///
/// Walks the whole block of adjacent list-item lines (any indent / kind) and
/// renumbers each ordered run independently with an indent stack, so:
///
///   - an outer list keeps counting across a nested child sitting between two of
///     its items (a flat per-indent renumber can't reach this, because the
///     deeper line fragments the run and a post-delete cursor lands on that
///     child), and
///   - each nested sub-list restarts its own sequence under its own parent.
///
/// Bullet lines and deeper continuation/child lines are preserved verbatim;
/// only ordered markers are rewritten.  Returns `None` when the cursor is not on
/// a list-item line or nothing needs changing (so it records no edit / no spare
/// undo step).  The block is bounded by blank or non-list lines, matching the
/// flat path's blank-line behavior.
pub fn renumber_list_block(source: &str, cursor_byte: usize) -> Option<EditDelta> {
    if source.is_empty() {
        return None;
    }
    let bytes = source.as_bytes();
    let clamped = cursor_byte.min(source.len());
    let cur_start = line_start_byte(bytes, clamped);
    // The cursor must rest on a list-item line — or on an indented
    // continuation line of one — for the block to exist.
    let cur_content_end = line_end_byte(bytes, cur_start);
    let cur_line = &source[cur_start..cur_content_end];
    if parse_line_start(cur_line).is_none() && !is_block_continuation_line(cur_line) {
        return None;
    }

    // Expand upward over contiguous list-block lines: marker lines and
    // indented non-blank continuation lines alike (a multi-line item must
    // not fragment the walk).  Blank lines still bound the block.
    let mut block_start = cur_start;
    while block_start > 0 {
        let prev_nl = block_start - 1; // the '\n' ending the previous line
        if bytes.get(prev_nl).copied() != Some(b'\n') {
            break;
        }
        let prev_start = line_start_byte(bytes, prev_nl);
        let prev = &source[prev_start..prev_nl];
        if parse_line_start(prev).is_some() || is_block_continuation_line(prev) {
            block_start = prev_start;
        } else {
            break;
        }
    }

    // Expand downward over contiguous list-item lines.
    let line_end_incl = |content_end: usize| {
        if content_end < source.len() && bytes[content_end] == b'\n' {
            content_end + 1
        } else {
            content_end
        }
    };
    let mut block_end = line_end_incl(cur_content_end);
    while block_end < source.len() {
        let next_end = line_end_byte(bytes, block_end);
        let next = &source[block_end..next_end];
        if parse_line_start(next).is_some() || is_block_continuation_line(next) {
            block_end = line_end_incl(next_end);
        } else {
            break;
        }
    }

    // Single pass over the block's lines, renumbering ordered runs with an
    // indent stack of `(indent_len, delimiter, next_number)`.
    let block = &source[block_start..block_end];
    let mut out = String::with_capacity(block.len());
    let mut stack: Vec<(usize, char, u64)> = Vec::new();
    let mut changed = false;
    let mut rest = block;
    while !rest.is_empty() {
        let (line, tail, had_nl) = match rest.find('\n') {
            Some(i) => (&rest[..i], &rest[i + 1..], true),
            None => (rest, "", false),
        };
        match parse_line_start(line) {
            Some((indent, MarkerKind::Ordered(delim), Some(num))) => {
                let k = indent.len();
                // Drop any deeper nested levels that just ended.
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
                        // New ordered run: a same-indent run of a different
                        // delimiter is a different list, so replace it.
                        if stack.last().is_some_and(|&(ki, _, _)| ki == k) {
                            stack.pop();
                        }
                        stack.push((k, delim, num + 1));
                        num
                    }
                };
                // `rest`-of-line after the `{indent}{digits}{delim} ` marker.
                // All marker chars are single-byte, so the byte slice is safe.
                let marker_len = indent.len() + num.to_string().len() + 2;
                out.push_str(&indent);
                out.push_str(&new_num.to_string());
                out.push(delim);
                out.push(' ');
                out.push_str(&line[marker_len..]);
                changed |= new_num != num;
            }
            other => {
                // A bullet (or differently-delimited) sibling ends any ordered
                // run at its indent or deeper; a deeper continuation / child
                // line (parse returns `None`) leaves the stack untouched.
                if let Some((indent, _, _)) = other {
                    let k = indent.len();
                    while stack.last().is_some_and(|&(ki, _, _)| ki >= k) {
                        stack.pop();
                    }
                }
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
        offset: block_start,
        removed: block.to_owned(),
        inserted: out,
    })
}

/// A non-blank line that starts with whitespace — treated as part of the
/// surrounding list block by the renumber walk (a continuation or
/// nested-content line), with no list-identity check: the walk itself only
/// rewrites lines that parse as ordered markers, so over-inclusion is
/// harmless.
fn is_block_continuation_line(line: &str) -> bool {
    (line.starts_with(' ') || line.starts_with('\t')) && !line.trim().is_empty()
}

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
