use crate::editor::EditorState;

/// Split raw block source into lines, dropping the phantom empty entry a trailing
/// newline produces.
pub(crate) fn raw_source_lines(source: &str) -> Vec<&str> {
    if source.is_empty() {
        return vec![""];
    }
    let mut lines: Vec<&str> = source.split('\n').collect();
    if lines.last() == Some(&"") && lines.len() > 1 {
        lines.pop();
    }
    lines
}

/// [`raw_source_lines`]`.len()` without building the `Vec`; the image reveal asks for it
/// every event-loop iteration. Pinned against `raw_source_lines` by a test.
pub(crate) fn raw_source_line_count(source: &str) -> usize {
    if source.is_empty() {
        return 1;
    }
    let count = source.split('\n').count();
    if count > 1 && source.ends_with('\n') {
        count - 1
    } else {
        count
    }
}

/// Rows the raw-source reveal reserves for a block: [`raw_source_line_count`] minus trailing
/// blank lines, never below one.
///
/// A paragraph's extended byte range absorbs the blank line after it, and that blank already
/// has a rendered row of its own (a virtual block), so counting it would shift the document
/// down by a row for the duration of the reveal.
pub(crate) fn revealed_source_line_count(source: &str) -> usize {
    let total = raw_source_line_count(source);
    let body = source.strip_suffix('\n').unwrap_or(source);
    let trailing = body
        .rsplit('\n')
        .take_while(|line| line.trim().is_empty())
        .count();
    total.saturating_sub(trailing).max(1)
}

/// The raw source lines a block reveals: [`raw_source_lines`] with trailing blank lines dropped
/// (never below one).  The reflow reveal stacks these, so it must use the same set
/// [`revealed_source_line_count`] counts — a paragraph's byte range absorbs the blank line after
/// it, and that blank owns its own rendered row, so revealing it too would over-count the block.
pub(crate) fn revealed_source_lines(source: &str) -> Vec<&str> {
    let mut lines = raw_source_lines(source);
    while lines.len() > 1 && lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines
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

/// The char columns `[start, end)` of raw line `line_idx` (whose text is `raw_text`) that the
/// buffer byte range `range` covers, for a block whose source `block_source` starts at buffer
/// byte `block_start`; `None` when it covers none of the line.  How every raw-revealed row
/// intersects the highlight with the line it paints.
pub(super) fn raw_line_sel_cols(
    block_source: &str,
    block_start: usize,
    line_idx: usize,
    raw_text: &str,
    range: (usize, usize),
) -> Option<(usize, usize)> {
    text_sel_cols(
        raw_text,
        block_start + raw_line_byte_start(block_source, line_idx),
        range,
    )
}

/// [`raw_line_sel_cols`] for any raw `text` starting at buffer byte `text_start` (a revealed
/// table cell's).
pub(super) fn text_sel_cols(
    text: &str,
    text_start: usize,
    (range_start, range_end): (usize, usize),
) -> Option<(usize, usize)> {
    let text_end = text_start + text.len();
    let lo = range_start.clamp(text_start, text_end);
    let hi = range_end.clamp(text_start, text_end);
    if lo >= hi {
        return None;
    }
    Some((
        text[..lo - text_start].chars().count(),
        text[..hi - text_start].chars().count(),
    ))
}

/// Raw source of the cursor's block, plus where the cursor sits inside it — the single
/// derivation shared by `RenderedView` and `editor::state::cursor_rendered_line_idx`, which
/// used to drift when computed twice.
///
/// Does not cover `RenderedView`'s stale-parse path, which rebuilds the source from
/// `cursor_block_line_range`.
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

/// `(raw_line_index, col)` of the cursor within the block; col is in chars. The index is into
/// [`raw_source_lines`], so a cursor at or past the end clamps to the last real line.
fn cursor_position_in_block(
    state: &EditorState,
    cursor_byte: usize,
    raw_source: &str,
) -> (usize, usize) {
    if raw_source.is_empty() {
        return (0, 0);
    }

    let block_start_byte = state
        .parsed
        .source_map
        .original_range_for_byte(cursor_byte)
        .map(|r| r.start)
        .unwrap_or(0);

    let cursor_offset_in_block = cursor_byte.saturating_sub(block_start_byte);

    let lines = raw_source_lines(raw_source);
    let mut byte_pos = 0usize;
    for (line_idx, line) in lines.iter().enumerate() {
        let line_end = byte_pos + line.len();
        if cursor_offset_in_block <= line_end {
            let col_bytes = cursor_offset_in_block.saturating_sub(byte_pos);
            let col = line[..col_bytes.min(line.len())].chars().count();
            return (line_idx, col);
        }
        byte_pos = line_end + 1; // +1 for the '\n'
    }

    let last_line = lines.last().copied().unwrap_or("");
    (lines.len().saturating_sub(1), last_line.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_line_sel_cols_intersects_the_range_with_one_line() {
        // Block "ab\nцd\n" at buffer byte 10; line 1 is "цd" at bytes 13..16.
        let src = "ab\nцd\n";
        assert_eq!(raw_line_sel_cols(src, 10, 1, "цd", (0, 100)), Some((0, 2)));
        // A range starting after `ц` (2 bytes) counts it as one char.
        assert_eq!(raw_line_sel_cols(src, 10, 1, "цd", (15, 16)), Some((1, 2)));
        // A range ending at the line's start, or covering only line 0, misses it.
        assert_eq!(raw_line_sel_cols(src, 10, 1, "цd", (10, 13)), None);
    }

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

    /// Trailing blanks absorbed by a block's extended range are virtual blocks with rows of
    /// their own; counting them shifts the document down during the reveal.
    #[test]
    fn revealed_count_drops_trailing_blank_lines() {
        assert_eq!(revealed_source_line_count("![cat](cat.png)\n\n"), 1);
        assert_eq!(revealed_source_line_count("![cat](cat.png)\n"), 1);
        assert_eq!(revealed_source_line_count("![cat](cat.png)"), 1);
        // A mermaid fence's range stops at the closing fence, so interior blanks stay.
        assert_eq!(
            revealed_source_line_count("```mermaid\nflowchart LR\n\n    A --> B\n```\n"),
            5
        );
        assert_eq!(revealed_source_line_count("text\n\n\n"), 1);
        assert_eq!(revealed_source_line_count(""), 1);
        assert_eq!(revealed_source_line_count("\n"), 1);
        assert_eq!(revealed_source_line_count("\n\n"), 1);
    }

    /// Rows are reserved off `raw_source_line_count` and painted from `raw_source_lines`;
    /// a drift clips the reveal or pads it with blank rows.
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
