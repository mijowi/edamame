use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::StatefulWidget,
};

use crate::config::Theme;
use crate::editor::table_edit;
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
        // For tables the renderer prepends a top border before the header and
        // appends a bottom border after the last data row, so raw line i maps
        // to rendered line i + 1 within the block — and we must never replace
        // either border with raw text.
        let is_table = table_edit::is_table_block(&raw_block_source);
        let cursor_in_block = if is_table && cursor_block_own >= 3 {
            let last_replaceable = cursor_block_own.saturating_sub(2);
            (cursor_raw_line + 1).min(last_replaceable)
        } else {
            cursor_raw_line.min(cursor_block_own.saturating_sub(1))
        };
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
                let raw_text = raw_lines.get(cursor_raw_line).copied().unwrap_or("");
                // Prefer cell-scoped reveal for table rows — replace only the
                // active cell's content area with raw text, keeping the box-
                // drawing borders and neighbouring cells rendered.
                let cell_overlay = if is_table {
                    editor
                        .parsed
                        .lines
                        .get(virtual_idx)
                        .and_then(|line| compute_cell_overlay(raw_text, line, cursor_col))
                } else {
                    None
                };
                if let Some(overlay) = cell_overlay {
                    let line = &editor.parsed.lines[virtual_idx];
                    rows_used = render_line(line, area, buf, vis_y as u16, wrap) as usize;
                    overlay_raw_cell(buf, area, vis_y as u16, &overlay, self.theme);
                } else {
                    // Fall back to full row-reveal (non-table blocks, or when
                    // raw cell content won't fit in the rendered cell width).
                    let styled = make_raw_line(raw_text, Some(cursor_col), self.theme);
                    rows_used = render_line(&styled, area, buf, vis_y as u16, wrap) as usize;
                }
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

/// Metadata for overlaying a raw cell on top of a rendered table row.
///
/// The `rendered_start..rendered_end` char range spans the cell's content area
/// between the two surrounding `│` box-drawing characters (exclusive of both
/// pipes).  `raw_text` is padded/clamped to that width when painted, so the
/// surrounding borders and neighbouring cells remain intact.
struct CellOverlay {
    rendered_start: usize,
    rendered_end: usize,
    raw_text: String,
    /// Cursor offset within `raw_text` in chars; `None` if the cursor sits
    /// outside the cell's overlay area (fallback path should be taken).
    cursor_in_cell: Option<usize>,
}

/// Try to compute a cell-scoped overlay for the cursor's active cell.
///
/// Returns `None` when the row doesn't parse as a table row, when the rendered
/// and raw pipe counts disagree (e.g. the cursor row is the alignment row,
/// which renders as a `├─┼─┤` separator), or when the raw cell text is wider
/// than the rendered cell area (in which case the caller falls back to the
/// full row-reveal so the user can still see the content they're editing).
fn compute_cell_overlay(
    raw_row: &str,
    rendered_line: &Line<'_>,
    cursor_col: usize,
) -> Option<CellOverlay> {
    let raw_pipes = raw_pipe_positions(raw_row);
    let rendered_pipes = rendered_pipe_positions(rendered_line);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return None;
    }

    // Cell index: the number of raw pipes at or before the cursor, minus one
    // (pipe 0 begins cell 0).  Clamp to [0, col_count-1].
    let col_count = raw_pipes.len() - 1;
    let preceding = raw_pipes.iter().take_while(|&&p| p < cursor_col).count();
    let cell_idx = preceding.saturating_sub(1).min(col_count - 1);

    let raw_cell_start = raw_pipes[cell_idx] + 1;
    let raw_cell_end = raw_pipes[cell_idx + 1];
    let raw_text: String = raw_row
        .chars()
        .skip(raw_cell_start)
        .take(raw_cell_end - raw_cell_start)
        .collect();

    let rendered_start = rendered_pipes[cell_idx] + 1;
    let rendered_end = rendered_pipes[cell_idx + 1];
    let rendered_width = rendered_end.saturating_sub(rendered_start);

    if raw_text.chars().count() > rendered_width {
        return None;
    }

    let cursor_offset = cursor_col.saturating_sub(raw_cell_start);
    let cursor_in_cell = if cursor_offset < rendered_width {
        Some(cursor_offset)
    } else if cursor_offset == rendered_width {
        Some(rendered_width.saturating_sub(1))
    } else {
        None
    };

    Some(CellOverlay {
        rendered_start,
        rendered_end,
        raw_text,
        cursor_in_cell,
    })
}

/// Char positions of unescaped `|` characters in a raw table row.  Preceding
/// `\` escapes the pipe per GFM rules; `\\|` is a literal backslash followed
/// by an unescaped pipe.
fn raw_pipe_positions(row: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut escaped = false;
    for (i, ch) in row.chars().enumerate() {
        if ch == '|' && !escaped {
            positions.push(i);
            escaped = false;
        } else if ch == '\\' {
            escaped = !escaped;
        } else {
            escaped = false;
        }
    }
    positions
}

/// Char positions of `│` box-drawing pipe characters in a rendered line.
fn rendered_pipe_positions(line: &Line<'_>) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut col = 0usize;
    for span in &line.spans {
        for ch in span.content.chars() {
            if ch == '│' {
                positions.push(col);
            }
            col += 1;
        }
    }
    positions
}

/// Paint `overlay.raw_text` into the cell's rendered column range, inverting
/// the character at `overlay.cursor_in_cell` to draw the cursor.  Writes
/// directly to the `TuiBuf` — the caller must have already rendered the
/// underlying row so the pipes and neighbouring cells are intact.
fn overlay_raw_cell(
    buf: &mut TuiBuf,
    area: Rect,
    visual_y: u16,
    overlay: &CellOverlay,
    theme: &Theme,
) {
    if visual_y >= area.height {
        return;
    }
    let abs_y = area.y + visual_y;
    let cell_width = overlay.rendered_end.saturating_sub(overlay.rendered_start);
    let raw_chars: Vec<char> = overlay.raw_text.chars().collect();
    let cursor_style = Style::default().add_modifier(Modifier::REVERSED);
    let base_style = theme.normal;

    for i in 0..cell_width {
        let col = overlay.rendered_start + i;
        let abs_x = area.x.saturating_add(col as u16);
        if abs_x >= area.x.saturating_add(area.width) {
            break;
        }
        let ch = raw_chars.get(i).copied().unwrap_or(' ');
        let style = if overlay.cursor_in_cell == Some(i) {
            cursor_style
        } else {
            base_style
        };
        if let Some(cell) = buf.cell_mut((abs_x, abs_y)) {
            cell.set_char(ch);
            cell.set_style(style);
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
