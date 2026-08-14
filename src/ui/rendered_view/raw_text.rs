use crate::editor::EditorState;

/// Split raw block source into lines, keeping any content before the final
/// trailing newline (which ropey line indexing includes).
pub(crate) fn raw_source_lines(source: &str) -> Vec<&str> {
    if source.is_empty() {
        return vec![""];
    }
    // Split on newlines. If source ends with '\n', the last element would be
    // empty — we include it as an empty line so cursor positioning still works.
    let mut lines: Vec<&str> = source.split('\n').collect();
    // Remove the trailing empty string only if there are multiple lines and
    // the source ends with '\n' (the split always produces an extra empty entry
    // at the end for trailing newlines, which we don't want to display as an
    // extra blank).
    if lines.last() == Some(&"") && lines.len() > 1 {
        lines.pop();
    }
    lines
}

/// How many lines [`raw_source_lines`] would yield, without building the
/// `Vec`.
///
/// The diagram reveal wants only the count, and asks for it on every run
/// of the event loop while the cursor rests in a fence (see
/// [`EditorState::sync_diagram_reveal`]) — allocating a `Vec` of slices
/// per iteration to read `.len()` off it is pure waste.  Kept beside
/// `raw_source_lines`, and pinned against it by
/// `raw_source_line_count_agrees_with_raw_source_lines`, so the reserved
/// row count and the painted lines can't drift apart.
pub(crate) fn raw_source_line_count(source: &str) -> usize {
    if source.is_empty() {
        return 1;
    }
    let count = source.split('\n').count();
    // Mirror the trailing-empty pop above: on a non-empty source that last
    // element is empty exactly when the source ends with a newline.
    if count > 1 && source.ends_with('\n') {
        count - 1
    } else {
        count
    }
}

/// Byte offset within `block_source` where raw line `line_idx` starts.
pub(super) fn raw_line_byte_start(block_source: &str, line_idx: usize) -> usize {
    let mut byte = 0usize;
    for (i, line) in block_source.split('\n').enumerate() {
        if i == line_idx {
            return byte;
        }
        byte += line.len() + 1;
    }
    block_source.len()
}

/// Raw source of the cursor's block, plus where the cursor sits inside it.
///
/// This is the shared derivation behind the hybrid-edit reveal: both
/// `RenderedView` (deciding which rendered row to paint raw source onto) and
/// `editor::state::cursor_rendered_line_idx` (reporting where the cursor
/// appears, which the mouse hit-test then keys its revealed-line shortcut
/// off) need the same `(source, raw_line, col)` triple.  Deriving it twice is
/// how the two used to drift.
///
/// `RenderedView` has one extra path this does *not* cover: when the parse is
/// stale it rebuilds the block source from `cursor_block_line_range` so the
/// just-typed characters are visible.  That branch stays in the view.
pub(crate) struct RawBlockCursor {
    /// Raw source text of the block, as `original_range_for_byte` bounds it.
    pub source: String,
    /// Index of the cursor's line within [`raw_source_lines`] of `source`.
    pub raw_line: usize,
    /// Char offset of the cursor from the start of that raw line.
    pub col: usize,
}

/// Extract the cursor block's raw source and locate the cursor within it.
pub(crate) fn raw_block_cursor(state: &EditorState, cursor_byte: usize) -> RawBlockCursor {
    let source: String = state
        .parsed
        .source_map
        .original_range_for_byte(cursor_byte)
        .map(|r| {
            let contents = state.buffer.contents();
            let end = r.end.min(contents.len());
            contents.get(r.start..end).unwrap_or("").to_owned()
        })
        .unwrap_or_default();
    let (raw_line, col) = cursor_position_in_block(state, cursor_byte, &source);
    RawBlockCursor {
        source,
        raw_line,
        col,
    }
}

/// Find which raw line of the block the cursor is on, and its column offset.
///
/// Returns `(raw_line_index, col)` where col is the char count from the start
/// of the raw line.  The index is into [`raw_source_lines`], not a bare
/// `split('\n')` — a cursor at or past the end clamps to the last *real*
/// line rather than the phantom empty entry a trailing newline produces.
fn cursor_position_in_block(
    state: &EditorState,
    cursor_byte: usize,
    raw_source: &str,
) -> (usize, usize) {
    if raw_source.is_empty() {
        return (0, 0);
    }

    // Get the original byte range of the block to find where cursor_byte falls
    // within the raw source text.
    let block_start_byte = state
        .parsed
        .source_map
        .original_range_for_byte(cursor_byte)
        .map(|r| r.start)
        .unwrap_or(0);

    let cursor_offset_in_block = cursor_byte.saturating_sub(block_start_byte);

    // Walk through the raw source in bytes to find which line and col.
    let lines = raw_source_lines(raw_source);
    let mut byte_pos = 0usize;
    for (line_idx, line) in lines.iter().enumerate() {
        let line_end = byte_pos + line.len();
        if cursor_offset_in_block <= line_end {
            // Cursor is on this line. Convert byte offset within line to char count.
            let col_bytes = cursor_offset_in_block.saturating_sub(byte_pos);
            let col = line[..col_bytes.min(line.len())].chars().count();
            return (line_idx, col);
        }
        byte_pos = line_end + 1; // +1 for the '\n'
    }

    // Cursor is at or past the end.
    let last_line = lines.last().copied().unwrap_or("");
    (lines.len().saturating_sub(1), last_line.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_source_lines_no_trailing_newline() {
        let lines = raw_source_lines("hello\nworld");
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn raw_source_lines_trailing_newline() {
        let lines = raw_source_lines("hello\nworld\n");
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn raw_source_lines_single() {
        let lines = raw_source_lines("hello");
        assert_eq!(lines, vec!["hello"]);
    }

    #[test]
    fn raw_source_lines_empty() {
        let lines = raw_source_lines("");
        assert_eq!(lines, vec![""]);
    }

    /// The diagram reveal reserves `raw_source_line_count` rows and
    /// `RenderedView` paints `raw_source_lines` onto them, so the two must
    /// agree for every shape of block source — a drift here clips the
    /// reveal or pads it with blank rows.  Covers the cases that separate
    /// them: empty, no trailing newline, trailing newline, a lone newline,
    /// interior blanks, and a blank line before the terminating newline.
    #[test]
    fn raw_source_line_count_agrees_with_raw_source_lines() {
        for source in [
            "",
            "hello",
            "hello\n",
            "\n",
            "\n\n",
            "hello\nworld",
            "hello\nworld\n",
            "hello\n\nworld",
            "hello\n\nworld\n",
            "hello\nworld\n\n",
            "```mermaid\nflowchart LR\n    A --> B\n```",
            "```mermaid\nflowchart LR\n    A --> B\n```\n",
        ] {
            assert_eq!(
                raw_source_line_count(source),
                raw_source_lines(source).len(),
                "count and split disagree for {source:?}"
            );
        }
    }
}
