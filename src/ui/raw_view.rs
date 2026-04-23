use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    text::{Line, Span},
    widgets::StatefulWidget,
};

use crate::config::Theme;
use crate::editor::EditorState;

/// Raw (plain text) document view.
///
/// Shows the entire buffer as plain Markdown text with a block cursor at the
/// cursor position. Used for `RawMode`.
pub struct RawView<'a> {
    pub state: &'a EditorState,
    pub theme: &'a Theme,
}

#[derive(Debug, Default)]
pub struct RawViewState {
    pub scroll: usize,
}

impl<'a> StatefulWidget for RawView<'a> {
    type State = RawViewState;

    fn render(self, area: Rect, buf: &mut TuiBuf, view_state: &mut Self::State) {
        if area.height == 0 {
            return;
        }

        let height = area.height as usize;
        view_state.scroll = self.state.scroll;

        let (cursor_line, cursor_col) = self.state.cursor.line_col(&self.state.buffer);
        let line_count = self.state.buffer.line_count();
        let cursor_style = self.theme.cursor;
        let sel_style = self.theme.selection;
        let selection_range = self.state.selection.map(|s| s.range());

        let mut vis_row: usize = 0;
        let mut buf_line = view_state.scroll;

        while vis_row < height && buf_line < line_count {
            let raw = self.state.buffer.line(buf_line).unwrap_or_default();
            // Strip trailing newline for display.
            let raw = raw.trim_end_matches('\n');

            let display_line: Line<'static> = if buf_line == cursor_line {
                let chars: Vec<char> = raw.chars().collect();
                let before: String = chars[..cursor_col.min(chars.len())].iter().collect();
                let at: String = chars
                    .get(cursor_col)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| " ".to_string());
                let after: String = if cursor_col + 1 <= chars.len() {
                    chars[cursor_col + 1..].iter().collect()
                } else {
                    String::new()
                };
                Line::from(vec![
                    Span::raw(before),
                    Span::styled(at, cursor_style),
                    Span::raw(after),
                ])
            } else {
                Line::raw(raw.to_owned())
            };

            // Precompute the selection's char-col range within this buffer line.
            let line_char_count = raw.chars().count();
            let line_start_char = self.state.buffer.line_to_char(buf_line);
            let line_end_char = line_start_char + line_char_count;
            let line_sel_cols = selection_range.and_then(|(s, e)| {
                if e <= line_start_char || s > line_end_char {
                    None
                } else {
                    let start = s.saturating_sub(line_start_char);
                    let end = e.saturating_sub(line_start_char).min(line_char_count);
                    if start < end {
                        Some((start, end))
                    } else {
                        None
                    }
                }
            });

            // Write the line to the TUI buffer with wrapping.
            let row_start = vis_row;
            let mut x = area.x;
            let mut char_col = 0usize;
            for span in &display_line.spans {
                for ch in span.content.chars() {
                    if x >= area.x + area.width {
                        vis_row += 1;
                        if vis_row >= height {
                            break;
                        }
                        x = area.x;
                    }
                    let y = area.y + vis_row as u16;
                    let in_selection =
                        matches!(line_sel_cols, Some((s, e)) if char_col >= s && char_col < e);
                    let style = if in_selection {
                        span.style.patch(sel_style)
                    } else {
                        span.style
                    };
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(ch);
                        cell.set_style(style);
                    }
                    x += 1;
                    char_col += 1;
                }
                if vis_row >= height {
                    break;
                }
            }
            let _ = row_start;

            vis_row += 1;
            buf_line += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Buffer, Selection};
    use crate::editor::EditorState;
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    #[test]
    fn raw_view_renders_text() {
        let theme = theme();
        let buf = Buffer::from_str("Hello\nWorld\n");
        let state = EditorState::new(buf, theme);
        let mut view_state = RawViewState::default();

        let backend = TestBackend::new(20, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let view = RawView {
                    state: &state,
                    theme,
                };
                StatefulWidget::render(view, frame.area(), frame.buffer_mut(), &mut view_state);
            })
            .unwrap();

        let output: String = (0..20u16)
            .map(|x| {
                terminal
                    .backend()
                    .buffer()
                    .cell((x, 0))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect();
        // First line should contain "Hello" with cursor on 'H'.
        assert!(output.contains('H'), "output: {:?}", output);
    }

    #[test]
    fn raw_view_paints_selection_background() {
        let theme = theme();
        let buf = Buffer::from_str("Hello world\n");
        let mut state = EditorState::new(buf, theme);
        state.selection = Some(Selection {
            anchor: 0,
            active: 5,
        });
        let mut view_state = RawViewState::default();

        let backend = TestBackend::new(20, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let view = RawView {
                    state: &state,
                    theme,
                };
                StatefulWidget::render(view, frame.area(), frame.buffer_mut(), &mut view_state);
            })
            .unwrap();

        let tbuf = terminal.backend().buffer().clone();
        // Columns 0..5 should carry the selection background.
        for x in 0..5u16 {
            let cell = tbuf.cell((x, 0)).expect("cell in bounds");
            assert_eq!(
                cell.style().bg,
                theme.selection.bg,
                "col {} missing selection bg",
                x
            );
        }
        // Col 5 (the space) should not be selected.
        let cell = tbuf.cell((5, 0)).expect("cell in bounds");
        assert_ne!(cell.style().bg, theme.selection.bg);
    }
}
