use ratatui::text::Line;

use crate::document::{Selection, VisualSelection};
use crate::editor::table_edit;
use crate::editor::EditorState;

/// If the raw bytes immediately before `sel.start` and immediately after
/// `sel.end` form a matching pair of inline formatting markers (`*…*`,
/// `**…**`, `_…_`, `__…__`, `` `…` ``, `~~…~~`), expand the selection to
/// include both markers so the highlight matches what the user sees when
/// the element de-renders after the click-and-drag completes.
///
/// Only expands when the selection is entirely on a single source line —
/// inline formatting doesn't span newlines in CommonMark.
pub(super) fn expand_selection_to_inline_markers(
    buffer: &crate::document::Buffer,
    sel: Selection,
) -> Selection {
    let (start_char, end_char) = sel.range();
    if end_char <= start_char {
        return sel;
    }
    let rope = buffer.rope();
    let start_byte = rope.char_to_byte(start_char);
    let end_byte = rope.char_to_byte(end_char);
    let source = buffer.contents();
    if end_byte > source.len() {
        return sel;
    }

    // Same-line constraint.
    if source[start_byte..end_byte].contains('\n') {
        return sel;
    }

    // Try double-char markers first so `**foo**` doesn't get reduced to `*foo*`.
    const DOUBLE_MARKERS: &[&str] = &["**", "__", "~~"];
    const SINGLE_MARKERS: &[&str] = &["*", "_", "`"];

    for m in DOUBLE_MARKERS.iter().chain(SINGLE_MARKERS.iter()) {
        let len = m.len();
        if start_byte < len || end_byte + len > source.len() {
            continue;
        }
        // Use `get` rather than direct slicing: the bytes adjacent to the
        // selection may fall inside a multibyte char (e.g. `—`), which would
        // panic on a `&source[..]` slice. All markers are ASCII, so a
        // non-boundary range simply can't match and is safely skipped.
        let (Some(before), Some(after)) = (
            source.get(start_byte - len..start_byte),
            source.get(end_byte..end_byte + len),
        ) else {
            continue;
        };
        if before == *m && after == *m {
            // Don't cross a line boundary when expanding — redundant given
            // the check above but cheap to verify.
            if before.contains('\n') || after.contains('\n') {
                continue;
            }
            let new_start_byte = start_byte - len;
            let new_end_byte = end_byte + len;
            let new_start = rope.byte_to_char(new_start_byte);
            let new_end = rope.byte_to_char(new_end_byte);
            // Preserve anchor/active direction.
            let (anchor, active) = if sel.anchor <= sel.active {
                (new_start, new_end)
            } else {
                (new_end, new_start)
            };
            return Selection { anchor, active };
        }
    }
    sel
}

/// Generic word-boundary scan around char index `at` in a sequence of
/// length `len` whose chars are produced by `get_char`.  Mirrors the
/// double-click word-selection rule: alphanumeric-or-`_` first, falling
/// back to a punctuation run when the cursor is on neither a word char nor
/// whitespace.  Returns `None` only when both passes collapse (cursor sits
/// on whitespace with no adjacent word or punctuation).
///
/// Used by both the rope-offset path (`select_word_at_cursor`) and the
/// Preview rendered-line path (`mouse_ops::apply`'s DoubleClick arm) so a
/// single definition of "word" governs both selection mechanisms.
pub(super) fn word_range_around<F>(len: usize, at: usize, get_char: F) -> Option<(usize, usize)>
where
    F: Fn(usize) -> char,
{
    if len == 0 {
        return None;
    }
    let at = at.min(len);
    let is_word = |c: char| c.is_alphanumeric() || c == '_';

    let mut start = at;
    while start > 0 && is_word(get_char(start - 1)) {
        start -= 1;
    }
    let mut end = at;
    while end < len && is_word(get_char(end)) {
        end += 1;
    }
    if start != end {
        return Some((start, end));
    }

    // Punctuation fallback: expand across non-alphanumeric, non-whitespace
    // chars so a double-click on `==` or `**` still produces a meaningful
    // selection.
    let mut s2 = at;
    while s2 > 0 {
        let c = get_char(s2 - 1);
        if c.is_whitespace() || is_word(c) {
            break;
        }
        s2 -= 1;
    }
    let mut e2 = at;
    while e2 < len {
        let c = get_char(e2);
        if c.is_whitespace() || is_word(c) {
            break;
        }
        e2 += 1;
    }
    if s2 != e2 {
        Some((s2, e2))
    } else {
        None
    }
}

/// Expand the selection to the word under the cursor (double-click).
pub(super) fn select_word_at_cursor(state: &mut EditorState) {
    let buf = &state.buffer;
    let len = buf.len_chars();
    let offset = state.cursor.offset.min(len);

    if len == 0 {
        state.selection = None;
        return;
    }

    let rope = buf.rope();
    match word_range_around(len, offset, |i| rope.char(i)) {
        Some((start, end)) => {
            state.selection = Some(Selection {
                anchor: start,
                active: end,
            });
            state.cursor.offset = end;
            state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
        }
        None => {
            state.selection = None;
        }
    }
}

/// Expand the selection to the whole line (triple-click).
///
/// Inside a table the whole buffer line is `| cell | cell | cell |` — selecting
/// that pulls in the borders and neighbouring cells, which almost never matches
/// what the user wants.  When the cursor is in a table cell, select just the
/// trimmed content of that cell instead.
pub(super) fn select_line_at_cursor(state: &mut EditorState) {
    let source = state.buffer.contents();
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    if let Some(info) = table_edit::find_table_at(&source, cursor_byte) {
        if let Some((row_idx, col_idx)) = table_edit::cursor_cell(&info, cursor_byte) {
            if let Some(row) = info.rows.get(row_idx) {
                if let Some(cell) = row.cells.get(col_idx) {
                    let raw_bytes = cell.raw.as_bytes();
                    let leading = raw_bytes
                        .iter()
                        .take_while(|b| matches!(**b, b' ' | b'\t'))
                        .count();
                    let trailing = raw_bytes
                        .iter()
                        .rev()
                        .take_while(|b| matches!(**b, b' ' | b'\t'))
                        .count();
                    let content_len = cell.raw.len().saturating_sub(leading + trailing);
                    let start_byte = row.start + cell.content_start + leading;
                    let end_byte = start_byte + content_len;
                    let rope = state.buffer.rope();
                    let anchor = rope.byte_to_char(start_byte);
                    let active = rope.byte_to_char(end_byte);
                    state.selection = Some(Selection { anchor, active });
                    state.cursor.offset = active;
                    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
                    return;
                }
            }
        }
    }

    let (line, _) = state.cursor.line_col(&state.buffer);
    let start = state.buffer.line_to_char(line);
    let end = if line + 1 < state.buffer.line_count() {
        state.buffer.line_to_char(line + 1)
    } else {
        state.buffer.len_chars()
    };
    state.selection = Some(Selection {
        anchor: start,
        active: end,
    });
    state.cursor.offset = end;
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
}

/// Extract the rendered text covered by `sel` from `lines`.  Lines between
/// the anchor and active endpoints are fully included; the first and last
/// lines are clipped to the selection's char columns.  A newline separates
/// each rendered line so multi-line copies preserve structure.
///
/// A cell-banded selection (started inside a table cell) clips every line
/// to the cell's column band, drops the cell's trailing padding, and joins
/// the lines with a single space instead of a newline: the banded lines are
/// wrap chunks of one logical source cell and wrap points are always
/// whitespace, so this reconstructs the cell text — matching what
/// Rendered-mode cell selection copies.
pub fn visual_selection_to_rendered_text(sel: VisualSelection, lines: &[Line<'_>]) -> String {
    let (start, end) = sel.range();
    let (start_line, start_col) = start;
    let (end_line, end_col) = end;
    if lines.is_empty() || start_line >= lines.len() {
        return String::new();
    }
    let end_line = end_line.min(lines.len() - 1);

    let mut out = String::new();
    // Iterate by index because the body needs to compare `idx` against
    // both `start_line` and `end_line`; an `enumerate().skip(...)` shape
    // is less direct.
    #[allow(clippy::needless_range_loop)]
    for idx in start_line..=end_line {
        let line = &lines[idx];
        let chars: Vec<char> = line.spans.iter().flat_map(|s| s.content.chars()).collect();
        let mut lo = if idx == start_line { start_col } else { 0 };
        let mut hi = if idx == end_line {
            end_col
        } else {
            chars.len()
        };
        if let Some(band) = sel.band {
            lo = lo.max(band.cols.0);
            hi = hi.min(band.cols.1);
        }
        let lo = lo.min(chars.len());
        let hi = hi.min(chars.len());
        if lo < hi {
            let slice: String = chars[lo..hi].iter().collect();
            if sel.band.is_some() {
                out.push_str(slice.trim_end());
            } else {
                out.push_str(&slice);
            }
        }
        if idx < end_line {
            out.push(if sel.band.is_some() { ' ' } else { '\n' });
        }
    }
    out
}

#[cfg(test)]
mod marker_expansion_tests {
    use super::*;
    use crate::document::Buffer;

    /// Regression: a marker check on the bytes adjacent to the selection
    /// must not panic when those bytes fall inside a multibyte char (e.g.
    /// the em-dash `—`, three bytes wide).
    #[test]
    fn no_panic_on_multibyte_char_adjacent_to_selection() {
        // The selection must start immediately *after* the em-dash (not on
        // it) and have a trailing char, so the `start_byte - len` probe for a
        // 1-byte marker lands inside `—` (bytes 0..3) while the `end_byte +
        // len` probe stays in bounds. Selecting the em-dash itself would only
        // ever probe the ASCII chars on either side and never hit the bug.
        let buffer = Buffer::from_str("—bc");
        // Select `b` (char index 1..2): `start_byte` is 3, so `before` probes
        // source[2..3], which is inside the em-dash.
        let sel = Selection {
            anchor: 1,
            active: 2,
        };
        let out = expand_selection_to_inline_markers(&buffer, sel);
        assert_eq!((out.anchor, out.active), (1, 2));
    }

    #[test]
    fn still_expands_real_markers() {
        let buffer = Buffer::from_str("a *foo* b");
        // Select `foo` (char index 3..6).
        let sel = Selection {
            anchor: 3,
            active: 6,
        };
        let out = expand_selection_to_inline_markers(&buffer, sel);
        // Expanded to include the surrounding `*` markers (char 2..7).
        assert_eq!((out.anchor, out.active), (2, 7));
    }
}
