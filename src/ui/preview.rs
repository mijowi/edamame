use ratatui::{buffer::Buffer, layout::Rect, style::Style, text::Line, widgets::StatefulWidget};

use super::line_render::{render_line_from_visual, visual_rows_of_chars};
use crate::document::VisualSelection;

/// State for the `PreviewView` widget — scroll offset, selection, and
/// hit-test snapshots.  The rendered line list is NOT held here; it
/// flows through the widget by borrow (`PreviewView::lines`) so a
/// scroll or mouse event no longer pays for a full `parsed.lines.clone()`
/// on every dispatch.  Mirrors the borrow style `RenderedView` already
/// uses against `&EditorState`.
#[derive(Debug, Default)]
pub struct PreviewState {
    /// Current scroll offset (top visible line index).
    pub scroll: usize,
    /// Optional selection in rendered coordinates, used to paint the
    /// selection background on top of the rendered cells.
    pub selection: Option<VisualSelection>,
    /// Background style to apply over selected cells.
    pub selection_style: Style,
    /// Snapshots of every visible `Block::ImageBlock`, populated in
    /// `EditorView::render` before the line-render pass so the image
    /// overlay step can paint pixels into the cells reserved by each
    /// placeholder.  Built against `EditorState::parsed.image_blocks`
    /// directly — no preview-local copy needed.
    pub image_snapshots: Vec<super::ImageLayoutSnapshot>,
    /// Cache key for `image_snapshots`: `(scroll, area, parsed_version)`.
    /// When the tuple matches the current frame, the snapshot vector is
    /// reused instead of rebuilt.
    pub image_snapshots_key: Option<(usize, ratatui::layout::Rect, u64)>,
    /// Phase 8 — link layout snapshots populated in `EditorView::render`
    /// so preview-mode mouse clicks can hit-test against link spans.
    pub link_snapshots: Vec<super::LinkLayoutSnapshot>,
    /// Cache key for `link_snapshots`: `(scroll, area, parsed_version)`.
    /// Mirrors `image_snapshots_key` — skips the link geometry walk
    /// when nothing that affects link layout has changed.
    pub link_snapshots_key: Option<(usize, ratatui::layout::Rect, u64)>,
}

/// A read-only, scrollable preview of rendered Markdown lines.
///
/// `lines` is borrowed from `EditorState::parsed.lines` so the widget
/// renders without owning a copy.  Scroll, selection, and snapshot
/// caches live on `PreviewState`.
///
/// Usage:
/// ```ignore
/// frame.render_stateful_widget(
///     PreviewView { lines: &editor.parsed.lines, scroll: editor.scroll },
///     area,
///     &mut state,
/// );
/// ```
pub struct PreviewView<'a> {
    pub lines: &'a [Line<'static>],
    pub scroll: usize,
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

    /// A list item whose text wraps in the viewport must hang-indent: the
    /// marker (`• `, `[ ] `, `1. `) sits alone on column 0 of the first
    /// visual row, and every wrapped continuation row begins at the column
    /// where the first row's text started — so the wrapped text is flush
    /// with the first character after the marker.
    #[test]
    fn list_item_wrap_hangs_indent_after_marker() {
        let theme = theme();
        // Bullet item with enough words to force wrap at width 12.  Marker
        // takes 2 cells; text begins at column 2.
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
        // Row 0: marker hangs off at col 0; text starts at col 2.
        let r0 = row_text(0);
        assert!(r0.starts_with("• "), "row 0 = {r0:?}");
        // Row 1+ are wrapped continuations.  At least one continuation row
        // must exist, and its first two cells must be blanks (the hanging
        // indent), with non-blank content starting at column 2.
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
        // Sanity: the wrap must actually have produced a continuation row
        // (i.e. the first row didn't fit the entire body).
        assert!(
            r1.trim_end().chars().any(|c| c.is_alphabetic()),
            "expected wrapped body on row 1: {r1:?}"
        );
    }

    /// Same hanging-indent rule applies to ordered, task, and nested lists —
    /// the wrap continuation aligns with the first row's text column for any
    /// recognized list marker.
    #[test]
    fn list_item_wrap_hangs_indent_for_task_and_ordered() {
        let theme = theme();
        // Task item: marker is `• [ ] ` (bullet + checkbox) = 6 cells.
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

    /// A code block line should have its background style applied to every
    /// cell of the row, from the first content column through the last cell
    /// of the viewport — even when the viewport is wider than the renderer's
    /// default `block_width`.
    #[test]
    fn code_block_bg_extends_to_viewport_edge() {
        let theme = theme();
        let lines = Renderer::new(theme).render(&parse("```\nfoo\n```\n"));
        let mut state = PreviewState::default();

        // Use a 100-wide terminal; the renderer's default block_width is 80,
        // so the extra 20 cells must be filled by the preview widget.
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

impl<'a> StatefulWidget for PreviewView<'a> {
    type State = PreviewState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.height == 0 {
            return;
        }

        // Render each visible line ourselves (rather than using `Paragraph`)
        // so that styled blocks like code blocks extend their background to
        // the full viewport width — both on their last (wrapped) visual row
        // and on short lines within a wider terminal.
        let sel_range = state.selection.map(|s| s.range());
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

            // Selection overlay: if this rendered line falls inside the
            // selection's line range, paint the theme's selection background
            // over the covered columns.  Uses the same word-wrap algorithm
            // as `render_line` to determine where visible content ends on
            // each sub-row so trailing padding isn't highlighted.
            if let Some(((s_line, s_col), (e_line, e_col))) = sel_range {
                if line_idx >= s_line && line_idx <= e_line {
                    let start_col = if line_idx == s_line { s_col } else { 0 };
                    let end_col = if line_idx == e_line {
                        e_col
                    } else {
                        line.spans.iter().map(|s| s.content.chars().count()).sum()
                    };
                    paint_preview_selection(
                        line, buf, area, vis_y, rows_used, width, skip_rows, start_col, end_col,
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

/// Paint `sel_style` as a background over the rendered cells on the visual
/// rows produced by wrapping `line` at `width`, clipped to `[start_col,
/// end_col)` in char columns.  Mirrors `paint_selection_overlay` in
/// `rendered_view` but for a single line's wrap layout.
#[allow(clippy::too_many_arguments)]
fn paint_preview_selection(
    line: &Line<'_>,
    buf: &mut Buffer,
    area: Rect,
    y_first: u16,
    rows_used: u16,
    width: usize,
    skip_rows: usize,
    start_col: usize,
    end_col: usize,
    sel_style: Style,
) {
    if width == 0 || end_col <= start_col {
        return;
    }
    let chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| {
            let style = span.style;
            span.content.chars().map(move |c| (c, style))
        })
        .collect();
    let indent = super::line_render::compute_hanging_indent(line);
    let rows = visual_rows_of_chars(&chars, width, indent);
    for (painted_off, (row_off, &(row_start, row_end, _))) in
        rows.iter().enumerate().skip(skip_rows).enumerate()
    {
        if painted_off as u16 >= rows_used {
            break;
        }
        let y = y_first + painted_off as u16;
        if y >= area.y + area.height {
            break;
        }
        // Intersect the selection's char range with this row's char span.
        let row_sel_start = start_col.max(row_start);
        let row_sel_end = end_col.min(row_end);
        if row_sel_start >= row_sel_end {
            continue;
        }
        // Continuation rows are pre-padded with `indent` blank cells; the
        // selection background must shift by the same amount.
        let row_indent = if row_off == 0 { 0 } else { indent };
        for i in row_sel_start..row_sel_end {
            let x_off = row_indent + (i - row_start);
            let x = area.x + x_off as u16;
            if x >= area.x + area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(cell.style().patch(sel_style));
            }
        }
    }
}
