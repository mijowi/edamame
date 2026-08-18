use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::StatefulWidget,
};

use crate::config::Theme;
use crate::editor::vim_ops::VisualKind;
use crate::editor::EditorState;
use crate::ui::line_render::render_raw_line_with_cursor;

/// Raw (plain text) document view.
///
/// Shows the entire buffer as plain Markdown text with a block cursor at the
/// cursor position. Used for `RawMode`.
pub struct RawView<'a> {
    pub state: &'a EditorState,
    pub theme: &'a Theme,
    /// The active vim Visual flavor, if any: the stored half-open `selection`
    /// is widened for the highlight only via `vim_ops::visual_span` —
    /// inclusive of the char under the cursor charwise, whole lines in
    /// VisualLine (see §2.6).  `selection` itself is never snapped.
    pub visual_kind: Option<VisualKind>,
    /// Resolved block-cursor style for this frame (`app::cursor_style`).
    /// For the default handler this is `status_mode_raw`; under vim it follows
    /// the sub-mode (INSERT keeps the raw warning color; NORMAL / VISUAL carry
    /// their sub-mode color even here).
    pub cursor_style: Style,
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
        let cursor_style = self.cursor_style;
        let cursor_visible = self.state.cursor_visible();
        let sel_style = self.theme.selection;
        let selection_range = self.state.selection.map(|s| {
            let r = crate::editor::vim_ops::visual_span(&s, &self.state.buffer, self.visual_kind);
            (r.start, r.end)
        });

        // A live `:s` preview may have rewritten the buffer, so an active
        // search session's byte ranges are stale against the previewed
        // text — suspend the search wash for the preview's own highlights
        // (it reappears untouched once the preview reverts).
        let preview_active = self.state.substitute_preview.is_some();
        let search_matches: &[std::ops::Range<usize>] = if preview_active {
            &[]
        } else {
            self.state
                .search
                .as_ref()
                .map_or(&[], |s| s.matches.as_slice())
        };
        let focused_match = self.state.search.as_ref().map(|s| s.focused_idx);
        let preview_highlights: &[std::ops::Range<usize>] = self
            .state
            .substitute_preview
            .as_ref()
            .map_or(&[], |p| p.highlights.as_slice());
        let rope_len_bytes = self.state.buffer.rope().len_bytes();
        // Recently-yanked span, painted as a brief flash the same way as
        // in the rendered views (`theme.selection`).  A linewise yank
        // spans several buffer lines, so it is clipped per line below.
        let yank_flash = self.state.active_yank_flash();

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

            // Search-match highlights on this line, as `(start_col,
            // end_col, style)` char ranges.  **A match may span a line
            // break** (`/  \n`), so each range is clipped to this line's
            // byte span rather than skipped — the same shape as the yank
            // flash below, and for the same reason.  Clipping against
            // the live rope also means a stale list (one frame after a
            // content swap) skips rather than panics.
            let mut line_highlights: Vec<(usize, usize, ratatui::style::Style)> = Vec::new();
            let line_start_byte = self
                .state
                .buffer
                .rope()
                .char_to_byte(line_start_char.min(self.state.buffer.len_chars()));
            let line_end_byte = line_start_byte + raw.len();
            if !search_matches.is_empty() {
                let first = search_matches.partition_point(|m| m.end <= line_start_byte);
                for (i, m) in search_matches.iter().enumerate().skip(first) {
                    // Ranges are sorted and non-overlapping, so the first
                    // one starting past this line ends the scan.
                    if m.start >= line_end_byte {
                        break;
                    }
                    if m.end > rope_len_bytes {
                        break; // stale list — every later range is too
                    }
                    let style = if Some(i) == focused_match {
                        self.theme.selection
                    } else {
                        self.theme.selection_muted
                    };
                    push_clipped(
                        &mut line_highlights,
                        raw,
                        line_start_byte,
                        line_end_byte,
                        m,
                        style,
                    );
                }
            }

            // Live `:s` preview highlights on this line — same shape as
            // the search matches above (sorted, non-overlapping, and
            // likewise able to span a line break now that a `:s` pattern
            // can match across one), painted with the full
            // `theme.selection` (the preview has no focus concept).
            if !preview_highlights.is_empty() {
                let first = preview_highlights.partition_point(|r| r.end <= line_start_byte);
                for r in preview_highlights.iter().skip(first) {
                    if r.start >= line_end_byte {
                        break;
                    }
                    if r.end > rope_len_bytes {
                        break;
                    }
                    push_clipped(
                        &mut line_highlights,
                        raw,
                        line_start_byte,
                        line_end_byte,
                        r,
                        self.theme.selection,
                    );
                }
            }

            // Yank flash: the portion of the flashed byte span that falls
            // on this line — a linewise yank spans several.
            if let Some(flash) = yank_flash {
                push_clipped(
                    &mut line_highlights,
                    raw,
                    line_start_byte,
                    line_end_byte,
                    &(flash.start..flash.end),
                    self.theme.selection,
                );
            }

            // The block cursor is painted onto the resolved cell by the render
            // override — not baked into `display_line` — so the wrapped layout
            // is computed from the bare source text and stays in lockstep with
            // the scroll / navigation wrap (which never sees the cursor).
            let cursor_override =
                (buf_line == cursor_line && cursor_visible).then_some((cursor_col, cursor_style));
            let display_line = raw_display_line(raw, line_sel_cols, &line_highlights, sel_style);
            // Flat wrap — never a hanging indent.  Raw mode shows the file,
            // so wrapping is the one liberty it takes; an indent the source
            // doesn't contain would both misread as raw text and put this
            // painter in a different layout from the scroll cache and the
            // click mapping, which wrap at indent 0.
            let rows_used = render_raw_line_with_cursor(
                &display_line,
                area,
                buf,
                vis_row as u16,
                cursor_override,
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

/// Clip one absolute byte `range` to the buffer line spanning
/// `line_start_byte..line_end_byte` (whose text is `raw`, newline
/// excluded) and, if anything survives, push it onto `out` as a char-col
/// range in `style`.
///
/// Every highlight source in Raw mode goes through this: a search match,
/// a `:s` preview span, and the yank flash can all begin on one line and
/// end on another, so none of them may assume their range fits the line
/// being painted.  A range that covers only the line's newline clips to
/// empty and paints nothing.  `raw.get(..)` rather than direct indexing
/// keeps a stale (post-shrink) range from panicking on a non-char
/// boundary for one frame.
fn push_clipped(
    out: &mut Vec<(usize, usize, ratatui::style::Style)>,
    raw: &str,
    line_start_byte: usize,
    line_end_byte: usize,
    range: &std::ops::Range<usize>,
    style: ratatui::style::Style,
) {
    let start_byte = range.start.max(line_start_byte);
    let end_byte = range.end.min(line_end_byte);
    if start_byte >= end_byte {
        return;
    }
    let s = raw
        .get(..start_byte - line_start_byte)
        .map(|p| p.chars().count());
    let e = raw
        .get(..end_byte - line_start_byte)
        .map(|p| p.chars().count());
    if let (Some(start_col), Some(end_col)) = (s, e) {
        out.push((start_col, end_col, style));
    }
}

/// Build the styled line for one buffer line in Raw mode: selection
/// background plus search-match highlights, one span per char.  The cursor is
/// NOT painted here — it is drawn onto the resolved cell by the render
/// override so the wrapped layout matches the bare-text wrap.
fn raw_display_line(
    raw: &str,
    selection: Option<(usize, usize)>,
    highlights: &[(usize, usize, ratatui::style::Style)],
    selection_style: ratatui::style::Style,
) -> Line<'static> {
    let chars: Vec<char> = raw.chars().collect();
    let mut spans = Vec::with_capacity(chars.len());
    for (i, ch) in chars.iter().enumerate() {
        let in_selection = matches!(selection, Some((s, e)) if i >= s && i < e);
        let highlight = highlights
            .iter()
            .find(|(s, e, _)| i >= *s && i < *e)
            .map(|(_, _, st)| *st);
        let mut style = if in_selection {
            selection_style
        } else {
            ratatui::style::Style::default()
        };
        if let Some(h) = highlight {
            style = style.patch(h);
        }
        spans.push(Span::styled(ch.to_string(), style));
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
                    visual_kind: None,
                    state: &state,
                    theme,
                    cursor_style: theme.status_mode_raw,
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
                    visual_kind: None,
                    state: &state,
                    theme,
                    cursor_style: theme.status_mode_raw,
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
    fn raw_view_paints_yank_flash() {
        let theme = theme();
        let mut state = EditorState::new(Buffer::from_str("Hello world\n"), theme);
        state.mode = crate::editor::Mode::Raw;
        // Park the cursor off the flashed span so it doesn't recolor col 0.
        state.cursor.offset = state.buffer.len_chars();
        // Flash "Hello" (chars 0..5).
        state.flash_yank(0, 5);
        let mut view_state = RawViewState::default();

        let backend = TestBackend::new(20, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let view = RawView {
                    visual_kind: None,
                    state: &state,
                    theme,
                    cursor_style: theme.status_mode_raw,
                };
                StatefulWidget::render(view, frame.area(), frame.buffer_mut(), &mut view_state);
            })
            .unwrap();

        let tbuf = terminal.backend().buffer().clone();
        for x in 0..5u16 {
            assert_eq!(
                tbuf.cell((x, 0)).expect("cell in bounds").style().bg,
                theme.selection.bg,
                "flashed col {x} missing flash bg",
            );
        }
        // Col 5 (the space) is outside the flash span.
        assert_ne!(
            tbuf.cell((5, 0)).expect("cell in bounds").style().bg,
            theme.selection.bg
        );
    }

    #[test]
    fn raw_view_visual_line_mode_paints_whole_lines() {
        // A charwise selection covering only part of two lines paints, in
        // VisualLine mode, the full content of every touched line.
        let theme = theme();
        let buf = Buffer::from_str("Hello world\nsecond line\n");
        let mut state = EditorState::new(buf, theme);
        state.mode = crate::editor::Mode::Raw;
        // Anchor mid-line-0, active mid-line-1 — a ragged charwise span.
        state.selection = Some(Selection {
            anchor: 3,
            active: 15,
        });
        // Park the block cursor on the trailing empty line (row 2) so it
        // doesn't recolor a checked selection cell on rows 0-1.
        state.cursor.offset = state.buffer.len_chars();
        let mut view_state = RawViewState::default();

        let backend = TestBackend::new(20, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let view = RawView {
                    visual_kind: Some(VisualKind::Line),
                    state: &state,
                    theme,
                    cursor_style: theme.status_mode_raw,
                };
                StatefulWidget::render(view, frame.area(), frame.buffer_mut(), &mut view_state);
            })
            .unwrap();

        let tbuf = terminal.backend().buffer().clone();
        // Every content column of line 0 ("Hello world", 11 cols) and line 1
        // ("second line", 11 cols) carries the selection bg — including col 0,
        // which the ragged charwise span (anchor 3) would have left bare.
        for x in 0..11u16 {
            assert_eq!(
                tbuf.cell((x, 0)).unwrap().style().bg,
                theme.selection.bg,
                "line 0 col {x} missing selection bg"
            );
            assert_eq!(
                tbuf.cell((x, 1)).unwrap().style().bg,
                theme.selection.bg,
                "line 1 col {x} missing selection bg"
            );
        }
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
                    visual_kind: None,
                    state: &state,
                    theme,
                    cursor_style: theme.status_mode_raw,
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
