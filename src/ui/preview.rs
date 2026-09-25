use ratatui::{buffer::Buffer, layout::Rect, style::Style, text::Line, widgets::StatefulWidget};

use super::line_render::{patch_char_cols, render_line_from_visual};
use crate::document::VisualSelection;

/// State for the `PreviewView` widget.  The rendered lines are not held here: they are borrowed
/// through `PreviewView::lines` so a scroll or mouse event never clones `parsed.lines`.
#[derive(Debug, Default)]
pub struct PreviewState {
    /// Top visible line index.
    pub scroll: usize,
    /// Selection in rendered coordinates, painted over the rendered cells.
    pub selection: Option<VisualSelection>,
    pub selection_style: Style,
    /// Visible `Block::ImageBlock` snapshots, populated in `EditorView::render` before the
    /// line-render pass so the image overlay can paint into each placeholder's cells.
    pub image_snapshots: Vec<super::ImageLayoutSnapshot>,
    /// `(scroll, area, parsed_version)`; a match reuses `image_snapshots` instead of rebuilding.
    pub image_snapshots_key: Option<(usize, ratatui::layout::Rect, u64)>,
    /// Link snapshots for preview-mode click hit-testing, populated in `EditorView::render`.
    pub link_snapshots: Vec<super::LinkLayoutSnapshot>,
    /// Same scheme as `image_snapshots_key`.
    pub link_snapshots_key: Option<(usize, ratatui::layout::Rect, u64)>,
}

/// A read-only, scrollable preview of rendered Markdown lines, borrowed from
/// `EditorState::parsed.lines`.
pub struct PreviewView<'a> {
    pub lines: &'a [Line<'static>],
    pub scroll: usize,
}

impl<'a> StatefulWidget for PreviewView<'a> {
    type State = PreviewState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.height == 0 {
            return;
        }

        // Lines are rendered by hand rather than via `Paragraph` so block backgrounds fill the
        // full viewport width, including the last wrapped row.
        let sel_range = state.selection.map(|s| s.range());
        // A cell-banded selection (started inside a table cell) clips every line to that band.
        let band = state.selection.and_then(|s| s.band);
        let sel_style = state.selection_style;
        let width = area.width as usize;
        let (mut line_idx, mut first_sub_row) = line_at_visual_row(self.lines, self.scroll, width);
        let mut vis_y: u16 = 0;
        while vis_y < area.height {
            let Some(line) = self.lines.get(line_idx) else {
                break;
            };
            let skip_rows = first_sub_row;
            let rows_used = render_line_from_visual(line, area, buf, vis_y, true, skip_rows);
            if rows_used == 0 {
                break;
            }

            if let Some(((s_line, s_col), (e_line, e_col))) = sel_range {
                if line_idx >= s_line && line_idx <= e_line {
                    let band_cols = band.map(|b| b.char_cols(line));
                    let start_col = if line_idx == s_line {
                        s_col
                    } else {
                        band_cols.map_or(0, |c| c.0)
                    };
                    let end_col = if line_idx == e_line {
                        e_col
                    } else {
                        band_cols.map_or_else(
                            || line.spans.iter().map(|s| s.content.chars().count()).sum(),
                            |c| c.1,
                        )
                    };
                    patch_char_cols(
                        line,
                        buf,
                        area,
                        vis_y,
                        rows_used,
                        skip_rows,
                        start_col..end_col,
                        sel_style,
                    );
                }
            }

            vis_y = vis_y.saturating_add(rows_used.max(1));
            line_idx += 1;
            first_sub_row = 0;
        }
    }
}

fn line_at_visual_row(lines: &[Line<'static>], visual_row: usize, width: usize) -> (usize, usize) {
    let mut acc = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        let rows = super::line_render::visual_rows_for_line(line, width).max(1);
        if visual_row < acc + rows {
            return (idx, visual_row - acc);
        }
        acc += rows;
    }
    (lines.len(), 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::markdown::{parse, Renderer};
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    #[test]
    fn wrapped_code_block_bg_fills_last_row() {
        let theme = theme();
        let long = "a".repeat(100);
        let md = format!("```\n{}\n```\n", long);
        let lines = Renderer::new(theme)
            .with_code_wrap(true)
            .render(&parse(&md));
        let mut state = PreviewState::default();

        let backend = TestBackend::new(80, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(
                    PreviewView {
                        lines: &lines,
                        scroll: 0,
                    },
                    frame.area(),
                    &mut state,
                );
            })
            .unwrap();

        let tbuf = terminal.backend().buffer().clone();
        let expected_bg = theme.code_block_text.bg;
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

    /// Wrapped list continuation rows must align with the first row's text column.
    #[test]
    fn list_item_wrap_hangs_indent_after_marker() {
        let theme = theme();
        let lines = Renderer::new(theme).render(&parse("- alpha bravo charlie delta\n"));
        let mut state = PreviewState::default();

        let backend = TestBackend::new(12, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(
                    PreviewView {
                        lines: &lines,
                        scroll: 0,
                    },
                    frame.area(),
                    &mut state,
                );
            })
            .unwrap();

        let tbuf = terminal.backend().buffer().clone();
        let row_text = |y: u16| -> String {
            (0..12)
                .map(|x| {
                    tbuf.cell((x, y))
                        .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                })
                .collect()
        };
        let r0 = row_text(0);
        assert!(r0.starts_with("• "), "row 0 = {r0:?}");
        let r1 = row_text(1);
        assert_eq!(
            &r1[..2],
            "  ",
            "continuation row should be left-padded by indent: {r1:?}"
        );
        assert!(
            r1.chars().nth(2).map(|c| c != ' ').unwrap_or(false),
            "continuation row must have text starting at indent column: {r1:?}"
        );
        assert!(
            r1.trim_end().chars().any(|c| c.is_alphabetic()),
            "expected wrapped body on row 1: {r1:?}"
        );
    }

    #[test]
    fn list_item_wrap_hangs_indent_for_task_and_ordered() {
        let theme = theme();
        let lines = Renderer::new(theme).render(&parse("- [ ] alpha bravo charlie delta\n"));
        let mut state = PreviewState::default();
        let backend = TestBackend::new(16, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(
                    PreviewView {
                        lines: &lines,
                        scroll: 0,
                    },
                    frame.area(),
                    &mut state,
                );
            })
            .unwrap();
        let tbuf = terminal.backend().buffer().clone();
        let row_text = |y: u16| -> String {
            (0..16)
                .map(|x| {
                    tbuf.cell((x, y))
                        .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                })
                .collect()
        };
        let r0 = row_text(0);
        assert!(r0.starts_with("• [ ] "), "row 0 = {r0:?}");
        let r1 = row_text(1);
        assert_eq!(
            &r1[..6],
            "      ",
            "task continuation must be padded by 6 cells: {r1:?}"
        );
        assert!(
            r1.chars().nth(6).map(|c| c != ' ').unwrap_or(false),
            "row 1 must start text at col 6: {r1:?}"
        );
    }

    #[test]
    fn code_block_bg_extends_to_viewport_edge() {
        let theme = theme();
        let lines = Renderer::new(theme).render(&parse("```\nfoo\n```\n"));
        let mut state = PreviewState::default();

        // The renderer's default block_width is 80; the extra 20 cells must be filled here.
        let backend = TestBackend::new(100, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(
                    PreviewView {
                        lines: &lines,
                        scroll: 0,
                    },
                    frame.area(),
                    &mut state,
                );
            })
            .unwrap();

        let tbuf = terminal.backend().buffer().clone();
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

    #[test]
    fn visual_scroll_starts_inside_wrapped_line() {
        let theme = theme();
        let lines = Renderer::new(theme).render(&parse("abcdefghijklmnopqrstuvwxyz\n"));
        let mut state = PreviewState::default();

        let backend = TestBackend::new(10, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(
                    PreviewView {
                        lines: &lines,
                        scroll: 1,
                    },
                    frame.area(),
                    &mut state,
                );
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
