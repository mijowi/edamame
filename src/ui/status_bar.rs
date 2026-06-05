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

/// Data the status bar needs for rendering.
pub struct StatusBarState<'a> {
    pub mode: Mode,
    /// File name or path (display string only).
    pub filename: &'a str,
    /// Total number of rendered document lines.
    pub line_count: usize,
    /// Whether the buffer has unsaved changes.  Renders as a single
    /// colored `*` glued to the right edge of the filename.
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
    /// Heading-ancestor chain of the cursor's current position, in
    /// document order (shallowest → deepest).  Renders as a `›`-joined
    /// breadcrumb after the filename.  Empty when the cursor sits
    /// before the first heading or the document has none.
    pub section_path: Vec<String>,
    /// `(resolved, total)` hunk counts in diff mode; `None` in every
    /// other mode.  Rendered adjacent to the mode badge as
    /// `resolved/total` — a progress counter that climbs from `0/n` to
    /// `n/n` as hunks are accepted or rejected.
    pub diff_progress: Option<(usize, usize)>,
}

/// A single-row status bar widget.
///
/// Layout: ` [mode]  filename[*?] › section › ...   sel  cursor  N lines  Z% `
pub struct StatusBar<'a> {
    pub state: StatusBarState<'a>,
    pub theme: &'a Theme,
}

/// Cells used by one breadcrumb segment's separator `" › "`.  Each
/// segment's total cost in the fit calculation is `SEP_COST +
/// width(text)`.
const SEP_COST: usize = 3;

/// Minimum visible-char count required to bother prefix-truncating a
/// segment that doesn't fit whole.  Below this the segment is dropped
/// entirely — `…a` carries almost no information.
const MIN_TRUNC_VISIBLE_CELLS: usize = 3;

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let s = &self.state;
        let theme = self.theme;

        // ── Left side (fixed, committed first) ──────────────────────
        //
        // The mode badge, filename, and optional `*` dirty marker are
        // committed before we decide how much room remains for the
        // breadcrumb.  The breadcrumb absorbs whatever is left over
        // after subtracting the right-side info segments.
        // In diff mode the whole bar shifts to the diff color so the
        // mode change is unmissable.
        let bar_style = if matches!(s.mode, Mode::Diff) {
            theme.status_bar_diff
        } else {
            theme.status_bar
        };
        // In diff mode the informational spans (filename, selection,
        // cursor, line count) carry their own `surface` background by
        // default, which would punch the normal bar hue through the
        // recolored diff bar.  Recolor just their backgrounds to the
        // diff bar's bg so the whole region reads as one washed bar;
        // their foregrounds (and the accent mode/progress badges) are
        // left untouched.
        let bar_bg = if matches!(s.mode, Mode::Diff) {
            bar_style.bg
        } else {
            None
        };
        let with_bar_bg = |st: Style| match bar_bg {
            Some(bg) => st.bg(bg),
            None => st,
        };

        // Mode badge — color swaps per-mode so each mode reads at a
        // glance (orange = Rendered, yellow = Raw, muted = Preview).
        // Kept as an accent badge even in diff mode.
        let mode_text = format!(" {} ", s.mode);
        let mode_width = UnicodeWidthStr::width(mode_text.as_str());
        let mode_span = Span::styled(mode_text, theme.status_mode_style(s.mode));

        // Diff-mode progress counter, rendered adjacent to the badge.
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

        // `*` glued to the right edge of the filename, colored via the
        // already-defined `status_modified` slot (warning fg, bold) so
        // the marker reads at a glance without taking the 11 cells the
        // old `[modified]` text used to.  No separating space — the
        // breadcrumb's first `" › "` (or the gap's surface fill when
        // there's no breadcrumb) provides whatever spacing follows.
        let modified_span = s
            .modified
            .then(|| Span::styled("*".to_string(), theme.status_modified));

        let left_committed_width =
            mode_width + diff_width + filename_width + if s.modified { 1 } else { 0 };

        // ── Right side (fixed) ──────────────────────────────────────
        let sel_text = match s.selection_size {
            Some((chars, lines)) => format!(" Sel {} ch · {} ln ", chars, lines),
            None => String::new(),
        };
        let sel_width = UnicodeWidthStr::width(sel_text.as_str());
        let sel_span = Span::styled(sel_text, with_bar_bg(theme.status_selection));

        let cursor_text = match (s.cursor_line, s.cursor_col) {
            (Some(l), Some(c)) => format!(" {}:{} ", l, c),
            _ => String::new(),
        };
        let cursor_width = UnicodeWidthStr::width(cursor_text.as_str());
        let cursor_span = Span::styled(cursor_text, with_bar_bg(theme.status_info));

        let pct = if s.line_count == 0 {
            100
        } else {
            let visible_end = s.scroll + area.height as usize;
            (visible_end.min(s.line_count) * 100) / s.line_count
        };
        let info_text = format!(" {} lines  {}% ", s.line_count, pct);
        let info_width = UnicodeWidthStr::width(info_text.as_str());
        let info_span = Span::styled(info_text, with_bar_bg(theme.status_info));

        let right_width = sel_width + cursor_width + info_width;

        // ── Breadcrumb (fits into whatever's left) ──────────────────
        //
        // Reserve at least 1 cell of gap between the breadcrumb and the
        // right-side info so they never visually touch.  When the
        // budget is too small, the breadcrumb collapses to empty and
        // the gap expands to fill.
        let breadcrumb_budget = (area.width as usize)
            .saturating_sub(left_committed_width)
            .saturating_sub(right_width)
            .saturating_sub(1);
        let breadcrumb_segments = fit_breadcrumb(&s.section_path, breadcrumb_budget);

        // Style each segment by its position in the chain: every
        // segment except the last is an ancestor (dimmed); the last
        // segment is the deepest enclosing heading — the "you are
        // here" anchor — and gets the accented bold treatment that
        // makes it pop against the dim chain.
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
        spans.push(sel_span);
        spans.push(cursor_span);
        spans.push(info_span);

        Paragraph::new(Line::from(spans))
            .style(bar_style)
            .render(area, buf);
    }
}

/// Choose which breadcrumb segments to render under a horizontal budget.
///
/// `chain` is in document order (shallowest → deepest).  Returns the
/// segments to render in the same order; segments are dropped from the
/// shallow end first so the user's current section (the deepest entry)
/// stays visible.  When even one full segment plus its `" › "` won't
/// fit, the leftmost remaining segment may be replaced with a
/// prefix-truncated `"…suffix"` form so the user still reads as much
/// of the heading text as space allows.
///
/// Display algorithm:
/// 1. Walk `chain` deepest → shallowest, including each segment whole
///    while `SEP_COST + width(text)` fits in the remaining budget.
/// 2. When the next segment doesn't fit, see if a prefix-truncated
///    version (`"…<last N cells>"`) fits with at least
///    [`MIN_TRUNC_VISIBLE_CELLS`] visible cells after the `…`.  If so,
///    include it; otherwise stop.
/// 3. Reverse so the result is in document order.
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
        // Try a prefix-truncated form: " › …<suffix>".  Cost is
        // SEP_COST + 1 (for `…`) + suffix_width.
        let leftover = budget
            .saturating_sub(used)
            .saturating_sub(SEP_COST)
            .saturating_sub(1);
        if leftover >= MIN_TRUNC_VISIBLE_CELLS {
            let suffix = last_cells(text, leftover);
            // Only useful if we actually captured *some* characters —
            // otherwise the `…` carries no information.
            if !suffix.is_empty() {
                included.push(format!("…{}", suffix));
            }
        }
        break;
    }
    included.reverse();
    included
}

/// Return the suffix of `text` whose display width is `<= cells`.
/// Walks characters from the end so wide graphemes (CJK, emoji) are
/// counted by their cell width, not their byte length.
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
        let theme = Box::leak(Box::new(Theme::default()));
        let backend = TestBackend::new(width, 1);
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
                        section_path,
                        diff_progress: None,
                    },
                    theme,
                };
                frame.render_widget(bar, frame.area());
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
    fn dirty_marker_is_asterisk_glued_to_filename() {
        let output = make_bar(Mode::Preview, "f.md", 5, true);
        assert!(
            output.contains("f.md*"),
            "expected `f.md*`, output was: {:?}",
            output
        );
        // Old text marker must be gone.
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
                        section_path: Vec::new(),
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
                        section_path: Vec::new(),
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
        // Width is tight enough that "Top" must drop, but the deepest
        // pair fits — should land as `notes.md › Mid › Deep`.
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
        // Wide enough that "Item 1" fits whole but "Checkpoint 1" only
        // partially — the leftmost segment should appear with a `…`
        // prefix capturing as much suffix as fits.  Width 50 leaves an
        // 18-cell breadcrumb budget: 9 for " › Item 1" + 9 for
        // " › …int 1".
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
        // Each segment costs " › X" = 4 cells; 3 segments = 12; plenty.
        assert_eq!(fit_breadcrumb(&chain, 80), chain);
    }

    #[test]
    fn fit_breadcrumb_drops_shallowest_first() {
        let chain = vec!["Top".to_string(), "Middle".to_string(), "Deep".to_string()];
        // Budget fits " › Deep" (7) + " › Middle" (9) = 16 but not
        // " › Top" (6) on top, which would need 22.
        let fit = fit_breadcrumb(&chain, 16);
        assert_eq!(fit, vec!["Middle".to_string(), "Deep".to_string()]);
    }

    #[test]
    fn fit_breadcrumb_prefix_truncates_leftmost_when_partial() {
        let chain = vec!["Checkpoint 1".to_string(), "Item 1".to_string()];
        // " › Item 1" = 9 cells; budget 16 leaves 7 cells for the
        // truncated leftmost segment.  Overhead is SEP_COST (3) + `…`
        // (1) = 4 cells, leaving 3 cells of suffix from "Checkpoint 1"
        // — the last three columns are "t 1".
        let fit = fit_breadcrumb(&chain, 16);
        assert_eq!(fit, vec!["…t 1".to_string(), "Item 1".to_string()]);
    }

    #[test]
    fn fit_breadcrumb_drops_when_too_few_visible_chars_remain() {
        let chain = vec!["Checkpoint 1".to_string(), "Item 1".to_string()];
        // " › Item 1" = 9; budget 11 leaves 2 cells — below
        // MIN_TRUNC_VISIBLE_CELLS = 3, so the leftmost segment is
        // dropped entirely instead of yielding `…X`.
        let fit = fit_breadcrumb(&chain, 11);
        assert_eq!(fit, vec!["Item 1".to_string()]);
    }

    #[test]
    fn fit_breadcrumb_returns_empty_when_deepest_alone_overflows() {
        let chain = vec!["A really long heading title".to_string()];
        // Budget too small to even prefix-truncate to MIN_TRUNC=3 cells
        // (need 3 + 1 + 3 = 7).
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
        // `漢` is 2 cells wide.  Budget 3 fits the trailing space (1)
        // plus one half-width char before it, but `漢` (2) won't fit
        // alongside the space.  Walking right-to-left: take ' ' (used
        // 1), next is `字` (2), used would be 3, OK; next `漢` (2),
        // used would be 5, exceeds — stop.  Result: `字 `.
        assert_eq!(last_cells("漢字 ", 3), "字 ");
    }
}
