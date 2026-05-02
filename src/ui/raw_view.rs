use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    text::{Line, Span},
    widgets::StatefulWidget,
};

use crate::config::Theme;
use crate::editor::EditorState;
use crate::ui::line_render::render_line_from_visual;

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

        let width = area.width as usize;
        let (cursor_line, cursor_col) = self.state.cursor.line_col(&self.state.buffer);
        let line_count = self.state.buffer.line_count();
        let cursor_style = self.theme.cursor_raw;
        let sel_style = self.theme.selection;
        let selection_range = self.state.selection.map(|s| s.range());

        let mut vis_row: usize = 0;
        let (mut buf_line, mut first_sub_row) = self
            .state
            .raw_line_at_visual_row(view_state.scroll, width.max(1));

        while vis_row < height && buf_line < line_count {
            let raw = self.state.buffer.line(buf_line).unwrap_or_default();
            // Strip trailing newline for display.
            let raw = raw.trim_end_matches('\n');

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

            let display_line = raw_display_line(
                raw,
                if buf_line == cursor_line {
                    Some(cursor_col)
                } else {
                    None
                },
                line_sel_cols,
                cursor_style,
                sel_style,
            );
            let rows_used = render_line_from_visual(
                &display_line,
                area,
                buf,
                vis_row as u16,
                true,
                first_sub_row,
            ) as usize;
            if rows_used == 0 {
                break;
            }

            vis_row += rows_used;
            buf_line += 1;
            first_sub_row = 0;
        }
    }
}

fn raw_display_line(
    raw: &str,
    cursor_col: Option<usize>,
    selection: Option<(usize, usize)>,
    cursor_style: ratatui::style::Style,
    selection_style: ratatui::style::Style,
) -> Line<'static> {
    let mut spans = Vec::new();
    let chars: Vec<char> = raw.chars().collect();
    let cursor_at = cursor_col.unwrap_or(usize::MAX);
    for i in 0..=chars.len() {
        if i == chars.len() {
            if cursor_at == i {
                spans.push(Span::styled(" ", cursor_style));
            }
            break;
        }
        let in_selection = matches!(selection, Some((s, e)) if i >= s && i < e);
        let style = if cursor_at == i && in_selection {
            cursor_style.patch(selection_style)
        } else if cursor_at == i {
            cursor_style
        } else if in_selection {
            selection_style
        } else {
            ratatui::style::Style::default()
        };
        spans.push(Span::styled(chars[i].to_string(), style));
    }
    Line::from(spans)
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

    #[test]
    fn raw_view_visual_scroll_starts_inside_wrapped_line() {
        let theme = theme();
        let buf = Buffer::from_str("abcdefghijklmnopqrstuvwxyz\n");
        let mut state = EditorState::new(buf, theme);
        state.mode = crate::editor::Mode::Raw;
        state.scroll = 1;
        let mut view_state = RawViewState::default();

        let backend = TestBackend::new(10, 2);
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

        let row: String = (0..10u16)
            .map(|x| {
                terminal
                    .backend()
                    .buffer()
                    .cell((x, 0))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect();
        assert_eq!(row, "klmnopqrst");
    }
}
