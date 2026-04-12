use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::config::Theme;
use crate::editor::Mode;

/// Data the status bar needs for rendering.
pub struct StatusBarState<'a> {
    pub mode: Mode,
    /// File name or path (display string only).
    pub filename: &'a str,
    /// Total number of rendered document lines.
    pub line_count: usize,
    /// Whether the buffer has unsaved changes.
    pub modified: bool,
    /// Optional scroll position (current top line / total lines).
    pub scroll: usize,
}

/// A single-row status bar widget.
///
/// Layout: `[mode] filename [modified?]   line_count lines  scroll%`
pub struct StatusBar<'a> {
    pub state: StatusBarState<'a>,
    pub theme: &'a Theme,
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let s = &self.state;
        let theme = self.theme;

        // Mode badge
        let mode_text = format!(" {} ", s.mode);
        let mode_span = Span::styled(mode_text.clone(), theme.status_mode);

        // Filename + modified flag
        let modified_marker = if s.modified { " [modified]" } else { "" };
        let filename_text = format!(" {}{} ", s.filename, modified_marker);
        let filename_span = Span::styled(filename_text, theme.status_filename);

        // Right-aligned info: line count and scroll %
        let pct = if s.line_count == 0 {
            100
        } else {
            let visible_end = s.scroll + area.height as usize;
            (visible_end.min(s.line_count) * 100) / s.line_count
        };
        let info_text = format!(" {} lines  {}% ", s.line_count, pct);
        let info_span = Span::styled(info_text, theme.status_info);

        // Fill gap between left and right sides
        let left_width = mode_text.len() + filename_span.content.len();
        let right_width = info_span.content.len();
        let gap = (area.width as usize)
            .saturating_sub(left_width)
            .saturating_sub(right_width);
        let gap_span = Span::styled(" ".repeat(gap), theme.status_bar);

        let line = Line::from(vec![mode_span, filename_span, gap_span, info_span]);
        Paragraph::new(line)
            .style(theme.status_bar)
            .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn make_bar(mode: Mode, filename: &str, line_count: usize, modified: bool) -> String {
        let theme = Box::leak(Box::new(Theme::default()));
        let backend = TestBackend::new(60, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let bar = StatusBar {
                    state: StatusBarState {
                        mode,
                        filename,
                        line_count,
                        modified,
                        scroll: 0,
                    },
                    theme,
                };
                frame.render_widget(bar, frame.area());
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        (0..60u16)
            .map(|x| buf.cell((x, 0)).map_or(' ', |c| c.symbol().chars().next().unwrap_or(' ')))
            .collect()
    }

    #[test]
    fn shows_mode() {
        let output = make_bar(Mode::Preview, "test.md", 42, false);
        assert!(output.contains("PREVIEW"), "output was: {:?}", output);
    }

    #[test]
    fn shows_filename() {
        let output = make_bar(Mode::Preview, "readme.md", 10, false);
        assert!(output.contains("readme.md"), "output was: {:?}", output);
    }

    #[test]
    fn shows_line_count() {
        let output = make_bar(Mode::Preview, "f.md", 99, false);
        assert!(output.contains("99"), "output was: {:?}", output);
    }

    #[test]
    fn shows_modified_flag() {
        let output = make_bar(Mode::Preview, "f.md", 5, true);
        assert!(output.contains("[modified]"), "output was: {:?}", output);
    }

    #[test]
    fn no_modified_flag_when_clean() {
        let output = make_bar(Mode::Preview, "f.md", 5, false);
        assert!(!output.contains("[modified]"), "output was: {:?}", output);
    }
}
