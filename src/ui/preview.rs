use ratatui::{buffer::Buffer, layout::Rect, text::Line, widgets::StatefulWidget};

use super::line_render::render_line;

/// State for the `PreviewView` widget, holding the rendered lines and the
/// current scroll offset (in rendered lines).
#[derive(Debug, Default)]
pub struct PreviewState {
    /// The fully-rendered document lines (produced by the Markdown renderer).
    pub lines: Vec<Line<'static>>,
    /// Current scroll offset (top visible line index).
    pub scroll: usize,
}

impl PreviewState {
    pub fn new(lines: Vec<Line<'static>>) -> Self {
        Self { lines, scroll: 0 }
    }

    /// Total number of rendered lines.
    pub fn total_lines(&self) -> usize {
        self.lines.len()
    }

    /// Scroll down by `n` lines, clamped to the document end.
    pub fn scroll_down(&mut self, n: usize, viewport_height: usize) {
        let max_scroll = self.lines.len().saturating_sub(viewport_height);
        self.scroll = (self.scroll + n).min(max_scroll);
    }

    /// Scroll up by `n` lines, clamped to 0.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Scroll to the very top.
    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    /// Scroll to the very bottom.
    pub fn scroll_to_bottom(&mut self, viewport_height: usize) {
        self.scroll = self.lines.len().saturating_sub(viewport_height);
    }
}

/// A read-only, scrollable preview of rendered Markdown lines.
///
/// Usage:
/// ```ignore
/// frame.render_stateful_widget(PreviewView, area, &mut state);
/// ```
pub struct PreviewView;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::markdown::{parse, Renderer};
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    /// A wrapped code block line (with `code_block_wrap` enabled) should have
    /// its background on every visual row — including the last, partial row.
    #[test]
    fn wrapped_code_block_bg_fills_last_row() {
        let theme = theme();
        // 100-char code line, renderer's block_width is 80, so this wraps.
        let long = "a".repeat(100);
        let md = format!("```\n{}\n```\n", long);
        let lines = Renderer::new(theme)
            .with_code_wrap(true)
            .render(&parse(&md));
        let mut state = PreviewState::new(lines);

        let backend = TestBackend::new(80, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(PreviewView, frame.area(), &mut state);
            })
            .unwrap();

        let tbuf = terminal.backend().buffer().clone();
        let expected_bg = theme.code_block_text.bg;
        // The 'a's wrap over two visual rows. Locate the last row that contains
        // any 'a' and verify every cell on that row has the code bg.
        let mut last_a_row: Option<u16> = None;
        for y in 0..6u16 {
            let row: String = (0..80)
                .map(|x| {
                    tbuf.cell((x, y))
                        .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                })
                .collect();
            if row.contains('a') {
                last_a_row = Some(y);
            }
        }
        let y = last_a_row.expect("code rows present");
        for x in 0..80u16 {
            let cell = tbuf.cell((x, y)).expect("cell in bounds");
            assert_eq!(
                cell.style().bg,
                expected_bg,
                "cell at column {} on last wrap row does not have the code bg",
                x
            );
        }
    }

    /// A code block line should have its background style applied to every
    /// cell of the row, from the first content column through the last cell
    /// of the viewport — even when the viewport is wider than the renderer's
    /// default `block_width`.
    #[test]
    fn code_block_bg_extends_to_viewport_edge() {
        let theme = theme();
        let lines = Renderer::new(theme).render(&parse("```\nfoo\n```\n"));
        let mut state = PreviewState::new(lines);

        // Use a 100-wide terminal; the renderer's default block_width is 80,
        // so the extra 20 cells must be filled by the preview widget.
        let backend = TestBackend::new(100, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(PreviewView, frame.area(), &mut state);
            })
            .unwrap();

        let tbuf = terminal.backend().buffer().clone();
        // Find the code row (contains "foo") and check that every cell in it
        // carries the code_block_text background style.
        let mut code_row: Option<u16> = None;
        for y in 0..3 {
            let row_text: String = (0..100)
                .map(|x| {
                    tbuf.cell((x, y))
                        .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                })
                .collect();
            if row_text.contains("foo") {
                code_row = Some(y);
                break;
            }
        }
        let y = code_row.expect("code row should be present");
        let expected_bg = theme.code_block_text.bg;
        for x in 0..100u16 {
            let cell = tbuf.cell((x, y)).expect("cell in bounds");
            assert_eq!(
                cell.style().bg,
                expected_bg,
                "cell at column {} does not have the code block background",
                x
            );
        }
    }
}

impl StatefulWidget for PreviewView {
    type State = PreviewState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.height == 0 {
            return;
        }

        // Render each visible line ourselves (rather than using `Paragraph`)
        // so that styled blocks like code blocks extend their background to
        // the full viewport width — both on their last (wrapped) visual row
        // and on short lines within a wider terminal.
        let mut vis_y: u16 = 0;
        let mut line_idx = state.scroll;
        while vis_y < area.height {
            let Some(line) = state.lines.get(line_idx) else {
                break;
            };
            let rows_used = render_line(line, area, buf, area.y + vis_y, true);
            vis_y = vis_y.saturating_add(rows_used.max(1));
            line_idx += 1;
        }
    }
}
