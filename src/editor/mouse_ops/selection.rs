use ratatui::text::Line;

use crate::document::{Selection, VisualSelection};
use crate::editor::table_edit;
use crate::editor::EditorState;

/// Expand `sel` over a matching pair of inline formatting markers (`*…*`, `**…**`, `_…_`,
/// `__…__`, `` `…` ``, `~~…~~`) bracketing it, so the highlight matches what the user sees once
/// the element de-renders.  Only when the selection is on a single source line — inline
/// formatting doesn't span newlines in CommonMark.
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
        // `get` rather than slicing: the adjacent bytes may fall inside a multibyte char
        // (e.g. `—`), which would panic.  Markers are ASCII, so a non-boundary range can't match.
        let (Some(before), Some(after)) = (
            source.get(start_byte - len..start_byte),
            source.get(end_byte..end_byte + len),
        ) else {
            continue;
        };
        if before == *m && after == *m {
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

/// Word-boundary scan around char index `at` in a sequence of length `len` whose chars come from
/// `get_char`: alphanumeric-or-`_` first, falling back to a punctuation run.  `None` when both
/// passes collapse (whitespace with no adjacent word or punctuation).
///
/// The single definition of "word" for both the rope-offset path (`select_word_at_cursor`) and
/// the Preview rendered-line path (`mouse_ops::apply`'s DoubleClick arm).
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

    // Punctuation fallback, so a double-click on `==` or `**` still selects something.
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

/// Expand the selection to the whole line (triple-click) — or, inside a table, to just the
/// trimmed content of the cursor's cell, since the buffer line would pull in borders and
/// neighboring cells.
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

/// Extract the rendered text covered by `sel` from `lines`, clipping the first and last lines to
/// the selection's char columns and joining with newlines.
///
/// A cell-banded selection (started inside a table cell) instead clips every line to the cell's
/// column band, drops trailing padding, and joins with a single space: the banded lines are wrap
/// chunks of one logical cell and wrap points are always whitespace, so this reconstructs the
/// cell text.
pub fn visual_selection_to_rendered_text(sel: VisualSelection, lines: &[Line<'_>]) -> String {
    let (start, end) = sel.range();
    let (start_line, start_col) = start;
    let (end_line, end_col) = end;
    if lines.is_empty() || start_line >= lines.len() {
        return String::new();
    }
    let end_line = end_line.min(lines.len() - 1);

    let mut out = String::new();
    // By index because the body compares `idx` against both `start_line` and `end_line`.
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
            let (band_lo, band_hi) = band.char_cols(line);
            lo = lo.max(band_lo);
            hi = hi.min(band_hi);
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
    use crate::document::{Buffer, CellBand};

    /// Copying a banded selection takes the same cell from every sub-line, although the sub-lines
    /// put it at different char columns.
    #[test]
    fn banded_copy_takes_the_cell_from_every_sub_line() {
        let lines = [Line::from("│ 日本 │ ab │"), Line::from("│ 語   │ cd │")];
        let sel = VisualSelection {
            anchor: (0, 7),
            active: (1, 10),
            band: Some(CellBand {
                lines: (0, 1),
                cols: (9, 11),
            }),
        };
        assert_eq!(visual_selection_to_rendered_text(sel, &lines), "ab cd");
    }

    /// Regression: the marker probe must not panic when the adjacent bytes fall inside a
    /// multibyte char.  Selecting `b` puts the 1-byte-marker probe at source[2..3], inside the
    /// three-byte em-dash; selecting the em-dash itself would never reach the bug.
    #[test]
    fn no_panic_on_multibyte_char_adjacent_to_selection() {
        let buffer = Buffer::from_str("—bc");
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
        let sel = Selection {
            anchor: 3,
            active: 6,
        };
        let out = expand_selection_to_inline_markers(&buffer, sel);
        assert_eq!((out.anchor, out.active), (2, 7));
    }
}
