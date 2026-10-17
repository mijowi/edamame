use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::config::Theme;
use crate::editor::Mode;
use crate::input::{HelixSubMode, ModalInput, VimSubMode};

/// Data the status bar needs for rendering.
pub struct StatusBarState<'a> {
    pub mode: Mode,
    /// File name or path (display string only).
    pub filename: &'a str,
    /// *Source* line count ("N lines") — the same coordinate space as [`cursor_line`] and the
    /// gutter, deliberately not the renderer's wrapped row count.
    ///
    /// [`cursor_line`]: Self::cursor_line
    pub line_count: usize,
    /// Total scrollable rows in the active mode at the current width — the denominator of the
    /// scroll percentage, in the same space as [`scroll`]. This one *is* the renderer's output.
    ///
    /// [`scroll`]: Self::scroll
    pub scroll_total: usize,
    /// Height of the *document* viewport, passed in rather than read from the widget's own
    /// one-row `area`; measuring against that reports the top row, so a document scrolled to
    /// the bottom reads well under 100%.
    pub viewport_rows: usize,
    /// Renders as a colored `*` glued to the filename.
    pub modified: bool,
    /// Current scroll offset (wrapped visual rows from the top).
    pub scroll: usize,
    /// Cursor line (1-indexed, `None` in Preview mode).
    pub cursor_line: Option<usize>,
    /// Cursor column (1-indexed, `None` in Preview mode).
    pub cursor_col: Option<usize>,
    /// Heading-ancestor chain at the cursor, shallowest → deepest; rendered as a `›`-joined
    /// breadcrumb after the filename.
    pub section_path: Vec<String>,
    /// `(resolved, total)` hunk counts in diff mode, rendered beside the mode badge.
    pub diff_progress: Option<(usize, usize)>,
    /// Active modal session; its badge outranks the rendering-mode badge except in [`Mode::Diff`].
    pub modal_input: &'a ModalInput,
}

/// A single-row status bar widget.
///
/// Layout: ` [mode]  filename[*?] › section › ...   cursor  N lines  Z% `
pub struct StatusBar<'a> {
    pub state: StatusBarState<'a>,
    pub theme: &'a Theme,
}

/// Cells of one breadcrumb separator `" › "`; a segment costs `SEP_COST + width(text)`.
const SEP_COST: usize = 3;

/// Below this many visible chars a prefix-truncated segment is dropped instead — `…a` carries
/// almost no information.
const MIN_TRUNC_VISIBLE_CELLS: usize = 3;

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let s = &self.state;
        let theme = self.theme;

        // ── Left side (fixed, committed first) ──────────────────────
        //
        // Committed first; the breadcrumb absorbs whatever is left after the right side.
        let bar_style = if matches!(s.mode, Mode::Diff) {
            theme.status_bar_diff
        } else {
            theme.status_bar
        };
        // The info spans carry their own `surface` bg, which would punch the normal hue
        // through the diff bar; recolor just their backgrounds.
        let bar_bg = if matches!(s.mode, Mode::Diff) {
            bar_style.bg
        } else {
            None
        };
        let with_bar_bg = |st: Style| match bar_bg {
            Some(bg) => st.bg(bg),
            None => st,
        };

        // Diff owns input, so its badge wins over the modal sub-mode. Enforce this here so
        // independently built StatusBarState values cannot advertise a deferred handler.
        let badge = if s.mode == Mode::Diff {
            None
        } else {
            modal_badge(theme, s.modal_input)
        };
        let (mode_text, mode_style) = match badge {
            Some((label, style)) => (format!(" {} ", label), style),
            None => (format!(" {} ", s.mode), theme.status_mode_style(s.mode)),
        };
        let mode_width = UnicodeWidthStr::width(mode_text.as_str());
        let mode_span = Span::styled(mode_text, mode_style);

        // Accent badge — not washed with the bar bg.
        let diff_text = match s.diff_progress {
            Some((resolved, total)) => format!(" {}/{} ", resolved, total),
            None => String::new(),
        };
        let diff_width = UnicodeWidthStr::width(diff_text.as_str());
        let diff_span = Span::styled(diff_text, theme.status_mode_diff);

        let filename_lead = format!(" {}", s.filename);
        let filename_width = UnicodeWidthStr::width(filename_lead.as_str());
        let filename_span = Span::styled(filename_lead, with_bar_bg(theme.status_filename));

        // No separating space: the breadcrumb's first `" › "` (or the gap fill) provides it.
        let modified_span = s
            .modified
            .then(|| Span::styled("*".to_string(), theme.status_modified));

        let left_committed_width =
            mode_width + diff_width + filename_width + if s.modified { 1 } else { 0 };

        // ── Right side (fixed) ──────────────────────────────────────
        let cursor_text = match (s.cursor_line, s.cursor_col) {
            (Some(l), Some(c)) => format!(" {}:{} ", l, c),
            _ => String::new(),
        };
        let cursor_width = UnicodeWidthStr::width(cursor_text.as_str());
        let cursor_span = Span::styled(cursor_text, with_bar_bg(theme.status_info));

        // Measured against the *last visible* row so a document whose end is on screen reads
        // 100%; an empty document or zero-height viewport reads 100% too.
        let pct = match s.scroll_total {
            0 => 100,
            total => {
                let visible_end = s.scroll.saturating_add(s.viewport_rows.max(1));
                (visible_end.min(total) * 100) / total
            }
        };
        let info_text = format!(" {} lines  {}% ", s.line_count, pct);
        let info_width = UnicodeWidthStr::width(info_text.as_str());
        let info_span = Span::styled(info_text, with_bar_bg(theme.status_info));

        let right_width = cursor_width + info_width;

        // ── Breadcrumb (fits into whatever's left) ──────────────────
        //
        // At least one cell of gap before the right-side info.
        let breadcrumb_budget = (area.width as usize)
            .saturating_sub(left_committed_width)
            .saturating_sub(right_width)
            .saturating_sub(1);
        let breadcrumb_segments = fit_breadcrumb(&s.section_path, breadcrumb_budget);

        // Ancestors dim; the last segment is the "you are here" anchor.
        let mut breadcrumb_spans: Vec<Span<'_>> = Vec::with_capacity(breadcrumb_segments.len() * 2);
        let mut breadcrumb_width = 0usize;
        let last_idx = breadcrumb_segments.len().saturating_sub(1);
        for (i, seg) in breadcrumb_segments.iter().enumerate() {
            breadcrumb_spans.push(Span::styled(" › ", theme.status_breadcrumb_sep));
            breadcrumb_width += SEP_COST + UnicodeWidthStr::width(seg.as_str());
            let seg_style = if i == last_idx {
                theme.status_breadcrumb_current
            } else {
                theme.status_breadcrumb_ancestor
            };
            breadcrumb_spans.push(Span::styled(seg.clone(), seg_style));
        }

        // ── Gap fill ────────────────────────────────────────────────
        let gap = (area.width as usize)
            .saturating_sub(left_committed_width)
            .saturating_sub(breadcrumb_width)
            .saturating_sub(right_width);
        let gap_span = Span::styled(" ".repeat(gap), bar_style);

        // ── Assemble ────────────────────────────────────────────────
        let mut spans: Vec<Span<'_>> = Vec::with_capacity(9 + breadcrumb_spans.len());
        spans.push(mode_span);
        spans.push(diff_span);
        spans.push(filename_span);
        if let Some(m) = modified_span {
            spans.push(m);
        }
        spans.extend(breadcrumb_spans);
        spans.push(gap_span);
        spans.push(cursor_span);
        spans.push(info_span);

        Paragraph::new(Line::from(spans))
            .style(bar_style)
            .render(area, buf);
    }
}

/// Resolve presentation from typed state; the cursor reuses the style without parsing labels.
pub(crate) fn modal_badge(theme: &Theme, input: &ModalInput) -> Option<(&'static str, Style)> {
    Some(match input {
        ModalInput::Default => return None,
        ModalInput::Vim(state) => (
            state.mode_label(),
            match state.sub_mode {
                VimSubMode::Normal | VimSubMode::OperatorPending => theme.status_mode_vim_normal,
                VimSubMode::Insert => theme.status_mode_vim_insert,
                VimSubMode::Visual | VimSubMode::VisualLine => theme.status_mode_vim_visual,
            },
        ),
        ModalInput::Helix(state) => (
            state.mode_label(),
            match state.sub_mode {
                HelixSubMode::Normal => theme.status_mode_vim_normal,
                HelixSubMode::Insert => theme.status_mode_vim_insert,
                HelixSubMode::Select => theme.status_mode_vim_visual,
            },
        ),
    })
}

/// Choose which breadcrumb segments fit `budget`, in document order. Segments drop from the
/// shallow end so the current section stays visible; the leftmost survivor may be
/// prefix-truncated to `"…suffix"` if at least [`MIN_TRUNC_VISIBLE_CELLS`] remain.
fn fit_breadcrumb(chain: &[String], budget: usize) -> Vec<String> {
    let mut included: Vec<String> = Vec::new();
    let mut used = 0usize;
    for text in chain.iter().rev() {
        let text_width = UnicodeWidthStr::width(text.as_str());
        let full_cost = SEP_COST + text_width;
        if used + full_cost <= budget {
            included.push(text.clone());
            used += full_cost;
            continue;
        }
        // Prefix-truncated form costs SEP_COST + 1 (`…`) + suffix width.
        let leftover = budget
            .saturating_sub(used)
            .saturating_sub(SEP_COST)
            .saturating_sub(1);
        if leftover >= MIN_TRUNC_VISIBLE_CELLS {
            let suffix = last_cells(text, leftover);
            if !suffix.is_empty() {
                included.push(format!("…{}", suffix));
            }
        }
        break;
    }
    included.reverse();
    included
}

/// Suffix of `text` whose display width is `<= cells`, measured by cell width.
fn last_cells(text: &str, cells: usize) -> String {
    let chars: Vec<(char, usize)> = text
        .chars()
        .map(|c| (c, UnicodeWidthChar::width(c).unwrap_or(0)))
        .collect();
    let mut taken_width = 0usize;
    let mut start_idx = chars.len();
    for i in (0..chars.len()).rev() {
        let w = chars[i].1;
        if taken_width + w > cells {
            break;
        }
        taken_width += w;
        start_idx = i;
    }
    chars[start_idx..].iter().map(|(c, _)| *c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    /// Every optional field empty; tests spell out just the field they need.
    fn base_state(mode: Mode, filename: &str) -> StatusBarState<'_> {
        StatusBarState {
            mode,
            filename,
            line_count: 10,
            scroll_total: 10,
            viewport_rows: 1,
            modified: false,
            scroll: 0,
            cursor_line: None,
            cursor_col: None,
            section_path: Vec::new(),
            diff_progress: None,
            modal_input: &ModalInput::Default,
        }
    }

    /// Render into a one-row bar and scrape the row back (first char of each cell).
    fn render_bar(state: StatusBarState<'_>, width: u16) -> String {
        let theme = Box::leak(Box::new(Theme::default()));
        let backend = TestBackend::new(width, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(StatusBar { state, theme }, frame.area());
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        (0..width)
            .map(|x| {
                buf.cell((x, 0))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect()
    }

    fn make_bar(mode: Mode, filename: &str, line_count: usize, modified: bool) -> String {
        make_bar_with_path(mode, filename, line_count, modified, Vec::new(), 60)
    }

    fn make_bar_with_path(
        mode: Mode,
        filename: &str,
        line_count: usize,
        modified: bool,
        section_path: Vec<String>,
        width: u16,
    ) -> String {
        render_bar(
            StatusBarState {
                line_count,
                modified,
                section_path,
                ..base_state(mode, filename)
            },
            width,
        )
    }

    #[test]
    fn shows_mode() {
        let output = make_bar(Mode::Preview, "test.md", 42, false);
        assert!(output.contains("PREVIEW"), "output was: {:?}", output);
    }

    #[test]
    fn diff_badge_outranks_the_vim_sub_mode_badge() {
        // See the precedence note in `render`.
        let output = render_bar(
            StatusBarState {
                diff_progress: Some((3, 7)),
                modal_input: &ModalInput::Vim(Box::default()),
                ..base_state(Mode::Diff, "f.md")
            },
            60,
        );
        assert!(
            output.contains("DIFF"),
            "DIFF badge must win over the vim label, output was: {output:?}"
        );
        assert!(
            !output.contains("NORMAL"),
            "vim sub-mode badge leaked into diff mode: {output:?}"
        );
        assert!(
            output.contains("3/7"),
            "diff progress must ride beside the badge: {output:?}"
        );
    }

    #[test]
    fn vim_badge_still_wins_outside_diff_mode() {
        let output = render_bar(
            StatusBarState {
                modal_input: &ModalInput::Vim(Box::default()),
                ..base_state(Mode::Rendered, "f.md")
            },
            60,
        );
        assert!(
            output.contains("NORMAL"),
            "vim badge must still supersede the mode badge: {output:?}"
        );
        assert!(
            !output.contains("EDIT"),
            "rendering-mode badge leaked alongside the vim badge: {output:?}"
        );
    }

    // Utility coverage for every typed badge, independently of its display text.
    #[test]
    fn modal_badges_use_per_sub_mode_colors() {
        use crate::input::{HelixState, VimState};

        let t = Theme::default();
        assert_eq!(modal_badge(&t, &ModalInput::Default), None);
        for (sub_mode, label, expected) in [
            (VimSubMode::Normal, "NORMAL", t.status_mode_vim_normal),
            (
                VimSubMode::OperatorPending,
                "NORMAL",
                t.status_mode_vim_normal,
            ),
            (VimSubMode::Insert, "INSERT", t.status_mode_vim_insert),
            (VimSubMode::Visual, "VISUAL", t.status_mode_vim_visual),
            (VimSubMode::VisualLine, "V-LINE", t.status_mode_vim_visual),
        ] {
            let input = ModalInput::Vim(Box::new(VimState {
                sub_mode,
                ..VimState::default()
            }));
            assert_eq!(modal_badge(&t, &input), Some((label, expected)));
        }
        for (sub_mode, label, expected) in [
            (
                HelixSubMode::Normal,
                "HELIX NORMAL",
                t.status_mode_vim_normal,
            ),
            (
                HelixSubMode::Insert,
                "HELIX INSERT",
                t.status_mode_vim_insert,
            ),
            (
                HelixSubMode::Select,
                "HELIX SELECT",
                t.status_mode_vim_visual,
            ),
        ] {
            let input = ModalInput::Helix(Box::new(HelixState {
                sub_mode,
                ..HelixState::default()
            }));
            assert_eq!(modal_badge(&t, &input), Some((label, expected)));
        }
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
    fn line_count_and_percentage_use_separate_counts() {
        // 6 source lines rendering as 10 rows, scrolled to the last row.
        let output = render_bar(
            StatusBarState {
                line_count: 6,
                scroll_total: 10,
                scroll: 9,
                ..base_state(Mode::Rendered, "f.md")
            },
            60,
        );
        assert!(output.contains("6 lines"), "output was: {output:?}");
        assert!(output.contains("100%"), "output was: {output:?}");
    }

    /// The percentage measures the *last* visible row, so a viewport showing
    /// the end of the document reads 100% even though `scroll` is well short
    /// of `scroll_total` — which is where `scroll_to_bottom` parks it.
    #[test]
    fn percentage_reaches_100_when_the_document_end_is_on_screen() {
        let output = render_bar(
            StatusBarState {
                scroll_total: 200,
                viewport_rows: 40,
                scroll: 160, // `scroll_to_bottom`: total - viewport_rows
                ..base_state(Mode::Rendered, "f.md")
            },
            60,
        );
        assert!(output.contains("100%"), "output was: {output:?}");
    }
    /// `scroll_to_bottom` parks `scroll` well short of `scroll_total`.
    #[test]
    fn percentage_reports_the_viewport_fraction_at_the_top() {
        let output = render_bar(
            StatusBarState {
                scroll_total: 200,
                viewport_rows: 40,
                scroll: 0,
                ..base_state(Mode::Rendered, "f.md")
            },
            60,
        );
        assert!(output.contains("20%"), "output was: {output:?}");
    }

    /// `max(1)` keeps a zero-row viewport from reading 0%.
    #[test]
    fn zero_height_viewport_still_counts_the_top_row() {
        let output = render_bar(
            StatusBarState {
                scroll_total: 10,
                viewport_rows: 0,
                scroll: 0,
                ..base_state(Mode::Rendered, "f.md")
            },
            60,
        );
        assert!(output.contains("10%"), "output was: {output:?}");
    }

    #[test]
    fn dirty_marker_is_asterisk_glued_to_filename() {
        let output = make_bar(Mode::Preview, "f.md", 5, true);
        assert!(
            output.contains("f.md*"),
            "expected `f.md*`, output was: {:?}",
            output
        );
        assert!(
            !output.contains("[modified]"),
            "stale `[modified]` text leaked: {:?}",
            output
        );
    }

    #[test]
    fn no_asterisk_when_clean() {
        let output = make_bar(Mode::Preview, "f.md", 5, false);
        assert!(!output.contains("f.md*"), "output was: {:?}", output);
    }

    #[test]
    fn shows_cursor_position() {
        let output = render_bar(
            StatusBarState {
                cursor_line: Some(3),
                cursor_col: Some(7),
                ..base_state(Mode::Rendered, "f.md")
            },
            60,
        );
        assert!(output.contains("3:7"), "output was: {:?}", output);
    }

    #[test]
    fn breadcrumb_renders_full_chain_when_space_allows() {
        let output = make_bar_with_path(
            Mode::Rendered,
            "notes.md",
            42,
            false,
            vec!["Checkpoint 1".to_string(), "Item 1".to_string()],
            80,
        );
        assert!(
            output.contains("notes.md › Checkpoint 1 › Item 1"),
            "expected full breadcrumb, output was: {:?}",
            output
        );
    }

    #[test]
    fn breadcrumb_drops_shallowest_when_overlong() {
        let output = make_bar_with_path(
            Mode::Rendered,
            "notes.md",
            5,
            false,
            vec!["Top".to_string(), "Mid".to_string(), "Deep".to_string()],
            48,
        );
        assert!(
            output.contains("notes.md › Mid › Deep"),
            "expected ancestor drop, output was: {:?}",
            output
        );
        assert!(
            !output.contains("Top"),
            "shallow segment leaked: {:?}",
            output
        );
    }

    #[test]
    fn breadcrumb_prefix_truncates_leftmost_when_partial_fit() {
        // Width 50 leaves an 18-cell budget: 9 for " › Item 1" + 9 for " › …int 1".
        let output = make_bar_with_path(
            Mode::Rendered,
            "notes.md",
            5,
            false,
            vec!["Checkpoint 1".to_string(), "Item 1".to_string()],
            50,
        );
        assert!(
            output.contains('…'),
            "expected ellipsis from prefix-truncation, output was: {:?}",
            output
        );
        assert!(
            output.contains("Item 1"),
            "deepest segment must survive truncation, output was: {:?}",
            output
        );
    }

    // ── fit_breadcrumb unit tests ─────────────────────────────────

    #[test]
    fn fit_breadcrumb_returns_empty_for_no_chain() {
        assert!(fit_breadcrumb(&[], 80).is_empty());
    }

    #[test]
    fn fit_breadcrumb_fits_full_chain_when_budget_is_ample() {
        let chain = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        assert_eq!(fit_breadcrumb(&chain, 80), chain);
    }

    #[test]
    fn fit_breadcrumb_drops_shallowest_first() {
        let chain = vec!["Top".to_string(), "Middle".to_string(), "Deep".to_string()];
        // " › Deep" (7) + " › Middle" (9) = 16; " › Top" would need 22.
        let fit = fit_breadcrumb(&chain, 16);
        assert_eq!(fit, vec!["Middle".to_string(), "Deep".to_string()]);
    }

    #[test]
    fn fit_breadcrumb_prefix_truncates_leftmost_when_partial() {
        let chain = vec!["Checkpoint 1".to_string(), "Item 1".to_string()];
        // " › Item 1" = 9; 16 - 9 - SEP_COST - `…` = 3 cells of suffix.
        let fit = fit_breadcrumb(&chain, 16);
        assert_eq!(fit, vec!["…t 1".to_string(), "Item 1".to_string()]);
    }

    #[test]
    fn fit_breadcrumb_drops_when_too_few_visible_chars_remain() {
        let chain = vec!["Checkpoint 1".to_string(), "Item 1".to_string()];
        // 11 - 9 = 2 cells, below MIN_TRUNC_VISIBLE_CELLS.
        let fit = fit_breadcrumb(&chain, 11);
        assert_eq!(fit, vec!["Item 1".to_string()]);
    }

    #[test]
    fn fit_breadcrumb_returns_empty_when_deepest_alone_overflows() {
        let chain = vec!["A really long heading title".to_string()];
        // Needs 3 + 1 + 3 = 7 to truncate.
        assert!(fit_breadcrumb(&chain, 6).is_empty());
    }

    // ── last_cells unit tests ─────────────────────────────────────

    #[test]
    fn last_cells_returns_full_text_when_budget_ample() {
        assert_eq!(last_cells("hello", 10), "hello");
    }

    #[test]
    fn last_cells_returns_suffix_within_budget() {
        assert_eq!(last_cells("Checkpoint 1", 3), "t 1");
        assert_eq!(last_cells("Checkpoint 1", 5), "int 1");
    }

    #[test]
    fn last_cells_zero_budget_is_empty() {
        assert_eq!(last_cells("hi", 0), "");
    }

    #[test]
    fn last_cells_respects_wide_characters() {
        // `漢` and `字` are 2 cells each.
        assert_eq!(last_cells("漢字 ", 3), "字 ");
    }
}
