use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{StatefulWidget, Widget},
};

use crate::config::Theme;
use crate::editor::Mode;

use super::{
    preview::{PreviewState, PreviewView},
    status_bar::{StatusBar, StatusBarState},
};

/// The top-level editor widget. Lays out the document area and the status bar.
///
/// Phase 0: only `PreviewMode` rendering is implemented; `RenderedMode` and
/// `RawMode` will be added in Phase 1.
pub struct EditorView<'a> {
    pub theme: &'a Theme,
    pub mode: Mode,
    pub filename: &'a str,
    pub modified: bool,
}

/// State for the `EditorView`: essentially delegates to `PreviewState` in Phase 0.
pub struct EditorViewState {
    pub preview: PreviewState,
}

impl EditorViewState {
    pub fn new(lines: Vec<ratatui::text::Line<'static>>) -> Self {
        Self {
            preview: PreviewState::new(lines),
        }
    }

    pub fn scroll_down(&mut self, n: usize, viewport_height: usize) {
        self.preview.scroll_down(n, viewport_height);
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.preview.scroll_up(n);
    }

    pub fn scroll_to_top(&mut self) {
        self.preview.scroll_to_top();
    }

    pub fn scroll_to_bottom(&mut self, viewport_height: usize) {
        self.preview.scroll_to_bottom(viewport_height);
    }

    pub fn total_lines(&self) -> usize {
        self.preview.total_lines()
    }

    pub fn scroll(&self) -> usize {
        self.preview.scroll
    }
}

impl<'a> StatefulWidget for EditorView<'a> {
    type State = EditorViewState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Split into document area + 1-row status bar.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);

        let doc_area = chunks[0];
        let bar_area = chunks[1];

        // Phase 0: always use PreviewView.
        StatefulWidget::render(PreviewView, doc_area, buf, &mut state.preview);

        // Status bar
        let bar = StatusBar {
            state: StatusBarState {
                mode: self.mode,
                filename: self.filename,
                line_count: state.preview.total_lines(),
                modified: self.modified,
                scroll: state.preview.scroll,
            },
            theme: self.theme,
        };
        Widget::render(bar, bar_area, buf);
    }
}
