use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, StatefulWidget, Widget, Wrap},
};

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

impl StatefulWidget for PreviewView {
    type State = PreviewState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Slice only the visible lines.
        let visible: Vec<Line<'static>> = state
            .lines
            .iter()
            .skip(state.scroll)
            .take(area.height as usize)
            .cloned()
            .collect();

        let paragraph = Paragraph::new(visible).wrap(Wrap { trim: false });
        Widget::render(paragraph, area, buf);
    }
}
