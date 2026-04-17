use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::StatefulWidget,
};

use crate::config::Theme;
use crate::editor::EditorState;

use super::line_render::{render_line, render_line_with_cursor};

/// State for the `RenderedView` widget.
///
/// Owned by `EditorViewState`; updated every frame from `EditorState`.
#[derive(Debug, Default)]
pub struct RenderedViewState {
    /// First visible rendered line (scroll offset).
    pub scroll: usize,
}

/// Hybrid rendered/raw editing view.
///
/// Every rendered block is shown as styled Markdown EXCEPT the block that
/// contains the cursor, which is replaced by the raw source text with an
/// inline cursor.
pub struct RenderedView<'a> {
    pub state: &'a EditorState,
    pub theme: &'a Theme,
}

impl<'a> StatefulWidget for RenderedView<'a> {
    type State = RenderedViewState;

    fn render(self, area: Rect, buf: &mut TuiBuf, view_state: &mut Self::State) {
        if area.height == 0 {
            return;
        }

        let height = area.height as usize;
        let editor = self.state;
        let cursor_offset = editor.cursor.offset;
        let cursor_byte = editor.buffer.rope().char_to_byte(cursor_offset);

        // Find which rendered lines belong to the cursor's source block.
        let cursor_block_lines = editor
            .parsed
            .source_map
            .rendered_lines_for_byte(cursor_byte);

        // The cursor block's OWN rendered lines (before gap blanks).
        let cursor_block_idx = editor
            .parsed
            .source_map
            .block_for_byte(cursor_byte)
            .unwrap_or(0);
        let cursor_block_own = editor.parsed.block_own_line_count(cursor_block_idx);

        // Get the raw source text for the cursor's block.
        let raw_block_source: String = editor
            .parsed
            .source_map
            .original_range_for_byte(cursor_byte)
            .map(|r| {
                let source = editor.buffer.contents();
                let end = r.end.min(source.len());
                source[r.start..end].to_owned()
            })
            .unwrap_or_default();

        // Split raw source into lines.
        let raw_lines: Vec<&str> = raw_source_lines(&raw_block_source);

        // Find where the cursor is within the raw block.
        let (cursor_raw_line, cursor_col) =
            cursor_position_in_block(editor, cursor_byte, &raw_block_source);

        // Map the cursor's raw source line to a rendered line within the block.
        // Clamp to the block's own (non-gap) rendered lines to avoid replacing
        // gap blank lines with raw text.
        let cursor_in_block = cursor_raw_line.min(cursor_block_own.saturating_sub(1));
        let cursor_rendered_line = cursor_block_lines.start + cursor_in_block;

        // Determine the scroll offset; sync from editor state.
        view_state.scroll = editor.scroll;
        let scroll = view_state.scroll;

        // Jitter suppression: if the cursor only recently moved to this line,
        // keep showing the block as rendered until the reveal delay has elapsed.
        let reveal_raw = editor.cursor_block_revealed();

        let cursor_indicator_style = Style::default().add_modifier(Modifier::REVERSED);

        let total_rendered = editor.parsed.lines.len();
        // Long-line wrapping is enabled in rendered-edit mode.
        let wrap = true;

        // Walk rendered lines from scroll offset. For each line, render it
        // normally EXCEPT cursor_rendered_line, which is shown as raw text.
        let mut virtual_idx = scroll;
        let mut vis_y: usize = 0;
        while vis_y < height {
            if virtual_idx >= total_rendered {
                break;
            }

            let rows_used;
            if reveal_raw && virtual_idx == cursor_rendered_line {
                // Show this one line as raw text with the cursor.
                let raw_text = raw_lines.get(cursor_raw_line).copied().unwrap_or("");
                let styled = make_raw_line(raw_text, Some(cursor_col), self.theme);
                rows_used = render_line(&styled, area, buf, vis_y as u16, wrap) as usize;
            } else if !reveal_raw && virtual_idx == cursor_rendered_line {
                // Still in jitter delay: show the rendered version with a cursor indicator
                // at the cursor's column so there is no visible column-jump when it reveals.
                if let Some(line) = editor.parsed.lines.get(virtual_idx) {
                    rows_used = render_line_with_cursor(
                        line,
                        area,
                        buf,
                        vis_y as u16,
                        wrap,
                        Some((cursor_col, cursor_indicator_style)),
                    ) as usize;
                } else {
                    rows_used = 1;
                }
            } else {
                // Normal rendered line.
                if let Some(line) = editor.parsed.lines.get(virtual_idx) {
                    rows_used = render_line(line, area, buf, vis_y as u16, wrap) as usize;
                } else {
                    break;
                }
            }

            vis_y += rows_used.max(1);
            virtual_idx += 1;
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a `Line` showing `raw_text` with a block cursor at `cursor_col`.
///
/// If `cursor_col` is `None`, no cursor is drawn (other lines of the block).
fn make_raw_line(raw_text: &str, cursor_col: Option<usize>, theme: &Theme) -> Line<'static> {
    let cursor_style = Style::default().add_modifier(Modifier::REVERSED);

    match cursor_col {
        None => Line::styled(raw_text.to_owned(), theme.normal),
        Some(col) => {
            let chars: Vec<char> = raw_text.chars().collect();
            let before: String = chars[..col.min(chars.len())].iter().collect();
            let cursor_char: String = chars
                .get(col)
                .map(|c| c.to_string())
                .unwrap_or_else(|| " ".to_string());
            let after: String = if col + 1 <= chars.len() {
                chars[col + 1..].iter().collect()
            } else {
                String::new()
            };

            Line::from(vec![
                Span::styled(before, theme.normal),
                Span::styled(cursor_char, cursor_style),
                Span::styled(after, theme.normal),
            ])
        }
    }
}

/// Split raw block source into lines, keeping any content before the final
/// trailing newline (which ropey line indexing includes).
fn raw_source_lines(source: &str) -> Vec<&str> {
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

/// Find which raw line of the block the cursor is on, and its column offset.
///
/// Returns `(raw_line_index, col)` where col is the char count from the start
/// of the raw line.
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
    let mut byte_pos = 0usize;
    for (line_idx, line) in raw_source.split('\n').enumerate() {
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
    let last_line_idx = raw_source.split('\n').count().saturating_sub(1);
    let last_line = raw_source.split('\n').last().unwrap_or("");
    (last_line_idx, last_line.chars().count())
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

    #[test]
    fn make_raw_line_with_cursor_at_start() {
        let theme = Theme::default();
        let line = make_raw_line("hello", Some(0), &theme);
        // First span should be empty (before cursor), second should be 'h'.
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hello");
    }

    #[test]
    fn make_raw_line_with_cursor_at_end() {
        let theme = Theme::default();
        let line = make_raw_line("hi", Some(2), &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hi "); // space added for end-of-line cursor
    }

    #[test]
    fn make_raw_line_without_cursor() {
        let theme = Theme::default();
        let line = make_raw_line("hello", None, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hello");
    }
}
