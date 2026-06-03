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
    /// Current scroll offset (rendered lines from top).
    pub scroll: usize,
    /// Cursor line (1-indexed, `None` in Preview mode).
    pub cursor_line: Option<usize>,
    /// Cursor column (1-indexed, `None` in Preview mode).
    pub cursor_col: Option<usize>,
    /// Active selection size as `(char_count, line_count)`.  Rendered
    /// as ` Sel 42 ch · 3 ln ` between the filename and cursor info
    /// when present.
    pub selection_size: Option<(usize, usize)>,
    /// `(resolved, total)` hunk counts in diff mode; `None` in every
    /// other mode.  Rendered adjacent to the mode badge as
    /// `resolved/total` — a progress counter that climbs from `0/n` to
    /// `n/n` as hunks are accepted or rejected.
    pub diff_progress: Option<(usize, usize)>,
}

/// A single-row status bar widget.
///
/// Layout: `[mode] filename [modified?]   cursor_pos  line_count lines  scroll%`
pub struct StatusBar<'a> {
    pub state: StatusBarState<'a>,
    pub theme: &'a Theme,
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let s = &self.state;
        let theme = self.theme;

        // Mode badge — color swaps per-mode so each mode reads at a
        // glance (orange = Rendered, yellow = Raw, muted = Preview).
        let mode_text = format!(" {} ", s.mode);
        let mode_span = Span::styled(mode_text.clone(), theme.status_mode_style(s.mode));

        // Diff-mode progress counter, rendered adjacent to the badge.
        let diff_text = match s.diff_progress {
            Some((resolved, total)) => format!(" {}/{} ", resolved, total),
            None => String::new(),
        };
        let diff_span = Span::styled(diff_text.clone(), theme.status_mode_diff);

        // Filename + modified flag
        let modified_marker = if s.modified { " [modified]" } else { "" };
        let filename_text = format!(" {}{} ", s.filename, modified_marker);
        let filename_span = Span::styled(filename_text, theme.status_filename);

        // Selection size (` Sel 42 ch · 3 ln `) — only visible when
        // there's an active selection, sits between filename and cursor.
        let sel_text = match s.selection_size {
            Some((chars, lines)) => format!(" Sel {} ch · {} ln ", chars, lines),
            None => String::new(),
        };
        let sel_span = Span::styled(sel_text.clone(), theme.status_selection);

        // Cursor position (1-indexed line:col, only in edit modes)
        let cursor_text = match (s.cursor_line, s.cursor_col) {
            (Some(l), Some(c)) => format!(" {}:{} ", l, c),
            _ => String::new(),
        };
        let cursor_span = Span::styled(cursor_text.clone(), theme.status_info);

        // Right-aligned info: line count and scroll %
        let pct = if s.line_count == 0 {
            100
        } else {
            let visible_end = s.scroll + area.height as usize;
            (visible_end.min(s.line_count) * 100) / s.line_count
        };
        let info_text = format!(" {} lines  {}% ", s.line_count, pct);
        let info_span = Span::styled(info_text, theme.status_info);

        // Fill gap between left and right sides.
        let left_width = mode_text.len() + diff_text.len() + filename_span.content.len();
        let right_width = sel_text.len() + cursor_text.len() + info_span.content.len();
        let gap = (area.width as usize)
            .saturating_sub(left_width)
            .saturating_sub(right_width);
        // In diff mode the whole bar shifts to the diff color so the
        // mode change is unmissable.
        let bar_style = if matches!(s.mode, Mode::Diff) {
            theme.status_bar_diff
        } else {
            theme.status_bar
        };
        let gap_span = Span::styled(" ".repeat(gap), bar_style);

        let line = Line::from(vec![
            mode_span,
            diff_span,
            filename_span,
            gap_span,
            sel_span,
            cursor_span,
            info_span,
        ]);
        Paragraph::new(line).style(bar_style).render(area, buf);
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
                        cursor_line: None,
                        cursor_col: None,
                        selection_size: None,
                        diff_progress: None,
                    },
                    theme,
                };
                frame.render_widget(bar, frame.area());
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        (0..60u16)
            .map(|x| {
                buf.cell((x, 0))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
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

    #[test]
    fn shows_cursor_position() {
        let theme = Box::leak(Box::new(Theme::default()));
        let backend = TestBackend::new(60, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let bar = StatusBar {
                    state: StatusBarState {
                        mode: Mode::Rendered,
                        filename: "f.md",
                        line_count: 10,
                        modified: false,
                        scroll: 0,
                        cursor_line: Some(3),
                        cursor_col: Some(7),
                        selection_size: None,
                        diff_progress: None,
                    },
                    theme,
                };
                frame.render_widget(bar, frame.area());
            })
            .unwrap();

        let output: String = (0..60u16)
            .map(|x| {
                terminal
                    .backend()
                    .buffer()
                    .cell((x, 0))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect();
        assert!(output.contains("3:7"), "output was: {:?}", output);
    }

    #[test]
    fn shows_selection_size_when_present() {
        let theme = Box::leak(Box::new(Theme::default()));
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let bar = StatusBar {
                    state: StatusBarState {
                        mode: Mode::Rendered,
                        filename: "f.md",
                        line_count: 10,
                        modified: false,
                        scroll: 0,
                        cursor_line: Some(1),
                        cursor_col: Some(1),
                        selection_size: Some((42, 3)),
                        diff_progress: None,
                    },
                    theme,
                };
                frame.render_widget(bar, frame.area());
            })
            .unwrap();
        let output: String = (0..80u16)
            .map(|x| {
                terminal
                    .backend()
                    .buffer()
                    .cell((x, 0))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect();
        assert!(output.contains("Sel 42 ch"), "output was: {:?}", output);
        assert!(output.contains("3 ln"), "output was: {:?}", output);
    }
}
