use crate::document::row_map::RawPos;
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

/// Raw source of the cursor's block, plus where the cursor sits inside it: [`block_source`]
/// and [`cursor_block_pos`] together, for `RenderedView`, which paints from the text.
///
/// Does not cover `RenderedView`'s stale-parse path, which rebuilds the source from
/// `cursor_block_line_range`.
pub(crate) struct RawBlockCursor {
    /// Raw source text of the block, as `original_range_for_block` bounds it.
    pub source: String,
    /// Index of the cursor's line within [`raw_source_lines`] of `source`.
    pub raw_line: usize,
    /// Char offset of the cursor from the start of that raw line.
    pub col: usize,
}

/// Extract the cursor block's raw source and locate the cursor within it.
pub(crate) fn raw_block_cursor(state: &EditorState) -> RawBlockCursor {
    let Some((block, pos)) = cursor_block_pos(state) else {
        return RawBlockCursor {
            source: String::new(),
            raw_line: 0,
            col: 0,
        };
    };
    RawBlockCursor {
        source: block_source(state, block),
        raw_line: pos.line,
        col: pos.col,
    }
}

/// The raw source of block `block` (a source-map index), read off the live buffer: the block
/// alone, never a copy of the whole document.
pub(crate) fn block_source(state: &EditorState, block: usize) -> String {
    let rope = state.buffer.rope();
    state
        .parsed
        .source_map
        .original_range_for_block(block)
        .map(|r| {
            let end = r.end.min(rope.len_bytes());
            let start = r.start.min(end);
            rope.slice(rope.byte_to_char(start)..rope.byte_to_char(end))
                .to_string()
        })
        .unwrap_or_default()
}

/// The cursor's block (a source-map index) and its position there: the line within
/// [`raw_source_lines`] of the block's source and the char column on it.  The single derivation
/// every cursor-row question reads — `RenderedView` (through [`raw_block_cursor`]),
/// `editor::state::cursor_rendered_line_idx` / `cursor_raw_line`, the reflow reveal
/// (`EditorState::cursor_stacked_row`) and the click mapping — so they can't drift apart.
///
/// Lines are counted at `\n` only, as [`raw_source_lines`] splits them, not at every break
/// `ropey` knows.  A cursor past the block's last line (its extended range runs on, or the
/// phantom line after a final newline) clamps to that line's end; one before the block's start
/// to its start.  Walks the block's chunks in the rope without copying them.
pub(crate) fn cursor_block_pos(state: &EditorState) -> Option<(usize, RawPos)> {
    let rope = state.buffer.rope();
    let cursor_byte = rope.char_to_byte(state.cursor.offset);
    let block = state.parsed.source_map.block_for_byte(cursor_byte)?;
    let range = state.parsed.source_map.original_range_for_block(block)?;
    let end = range.end.min(rope.len_bytes());
    let start = range.start.min(end);
    // A trailing newline ends the last line rather than opening another (`raw_source_lines`).
    let content_end = if end > start && rope.byte(end - 1) == b'\n' {
        end - 1
    } else {
        end
    };
    let at = cursor_byte.clamp(start, content_end);
    let mut pos = RawPos { line: 0, col: 0 };
    for chunk in rope
        .slice(rope.byte_to_char(start)..rope.byte_to_char(at))
        .chunks()
    {
        match chunk.rfind('\n') {
            Some(i) => {
                pos.line += chunk.bytes().filter(|&b| b == b'\n').count();
                pos.col = chunk[i + 1..].chars().count();
            }
            None => pos.col += chunk.chars().count(),
        }
    }
    Some((block, pos))
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

    /// `cursor_block_pos` walks the rope; it must place every cursor exactly where splitting the
    /// block's source at `\n` would, the way `raw_source_lines` and the painter count: past
    /// breaks `ropey` alone counts as lines (`\r`, U+2028), on a trailing blank
    /// the block absorbs, and clamped past the block's end.
    #[test]
    fn cursor_block_pos_counts_lines_as_the_block_source_splits_them() {
        use crate::config::Theme;
        use crate::document::Buffer;

        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        for src in [
            "alpha\nbravo\n\nnext\n",
            "a\u{2028}b\nc d\n",
            "lone\rcr\nline\n",
            "- a\n  b\n\n  c\n- d\n",
            "> q\n> r\n",
            "| a |\n|---|\n| ж |\n",
            "no newline",
            "",
        ] {
            let mut state = EditorState::new(Buffer::from_str(src), theme);
            // The buffer normalizes line endings, so the reference reads its text.
            let text = state.buffer.contents();
            for offset in 0..=state.buffer.len_chars() {
                state.cursor.offset = offset;
                let Some((block, pos)) = cursor_block_pos(&state) else {
                    continue;
                };
                // The reference: the block's source split at `\n`, the cursor clamped into it.
                let range = state
                    .parsed
                    .source_map
                    .original_range_for_block(block)
                    .unwrap();
                let source = &text[range.start..range.end.min(text.len())];
                let at = state.buffer.rope().char_to_byte(offset).max(range.start) - range.start;
                let lines = raw_source_lines(source);
                let mut want = (lines.len() - 1, lines.last().unwrap().chars().count());
                let mut line_start = 0;
                for (i, line) in lines.iter().enumerate() {
                    if at <= line_start + line.len() {
                        want = (i, line[..at.saturating_sub(line_start)].chars().count());
                        break;
                    }
                    line_start += line.len() + 1;
                }
                assert_eq!((pos.line, pos.col), want, "offset {offset} in {src:?}");
                let raw = raw_block_cursor(&state);
                assert_eq!(raw.source, source, "offset {offset} in {src:?}");
            }
        }
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
