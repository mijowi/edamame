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
    /// Total number of *source* lines in the document — the count shown as
    /// "N lines", and the same coordinate space as [`cursor_line`] beside it
    /// and as the line-number gutter.  Deliberately not the renderer's row
    /// count: a 6-line document that renders as 10 rows (a table renders
    /// roughly two rows per data row) has 6 lines, and reporting 10 next to a
    /// `6:1` cursor read-out contradicts it.
    ///
    /// [`cursor_line`]: Self::cursor_line
    pub line_count: usize,
    /// Total scrollable rows in the active mode, at the current viewport
    /// width — the denominator of the scroll percentage, and the same
    /// coordinate space as [`scroll`].  This one *is* the renderer's output
    /// (wrapped): the percentage answers "how far down the thing I'm
    /// scrolling am I", which has nothing to do with source lines.
    ///
    /// [`scroll`]: Self::scroll
    pub scroll_total: usize,
    /// Height of the *document* viewport in rows — how far past [`scroll`]
    /// the last visible row sits, and so the reach of the percentage's
    /// numerator.  Deliberately passed in rather than taken from the widget's
    /// own `area`, which is the one-row status bar: measuring against that
    /// reports the row at the *top* of the screen, so a document scrolled
    /// fully to the bottom reads well under 100%.
    ///
    /// [`scroll`]: Self::scroll
    pub viewport_rows: usize,
    /// Whether the buffer has unsaved changes.  Renders as a single
    /// colored `*` glued to the right edge of the filename.
    pub modified: bool,
    /// Current scroll offset (wrapped visual rows from the top).
    pub scroll: usize,
    /// Cursor line (1-indexed, `None` in Preview mode).
    pub cursor_line: Option<usize>,
    /// Cursor column (1-indexed, `None` in Preview mode).
    pub cursor_col: Option<usize>,
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
    /// When the vim handler is active, the sub-mode badge text
    /// (`NORMAL` / `INSERT` / `VISUAL` / `V-LINE`).  Takes precedence
    /// over the rendering-mode badge; `None` for the default handler.
    /// The one exception is [`Mode::Diff`], whose badge outranks this
    /// one — see the precedence note in `render`.
    pub vim_mode_label: Option<&'a str>,
}

/// A single-row status bar widget.
///
/// Layout: ` [mode]  filename[*?] › section › ...   cursor  N lines  Z% `
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
        // Kept as an accent badge even in diff mode.  When the vim
        // handler is active its sub-mode badge wins, using the
        // `status_mode_vim_*` colors (NORMAL = primary, INSERT = success,
        // VISUAL/V-LINE = secondary) that the editor cursor also mirrors.
        //
        // `Mode::Diff` outranks the vim badge, though: the diff-review
        // keymap owns every key for the duration of the review — the
        // `vim_deferred` guard in `App::dispatch_single_key` bypasses the
        // vim handler outright — so a `NORMAL` badge there would advertise
        // a handler that isn't live (`i` / `v` / `:` all no-op).  The
        // badge names whichever keymap is actually reading the user's
        // keystrokes, which in diff is the same `DIFF` the default
        // handler shows.  Resolved here rather than at the call site so
        // it can't be bypassed by a `StatusBarState` built elsewhere.
        let vim_label = s.vim_mode_label.filter(|_| !matches!(s.mode, Mode::Diff));
        let (mode_text, mode_style) = match vim_label {
            Some(label) => (format!(" {} ", label), vim_badge_style(theme, label)),
            None => (format!(" {} ", s.mode), theme.status_mode_style(s.mode)),
        };
        let mode_width = UnicodeWidthStr::width(mode_text.as_str());
        let mode_span = Span::styled(mode_text, mode_style);

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
        let cursor_text = match (s.cursor_line, s.cursor_col) {
            (Some(l), Some(c)) => format!(" {}:{} ", l, c),
            _ => String::new(),
        };
        let cursor_width = UnicodeWidthStr::width(cursor_text.as_str());
        let cursor_span = Span::styled(cursor_text, with_bar_bg(theme.status_info));

        // Measured against the *last visible* row, so a document whose end is
        // on screen reads 100% — `scroll` alone would report the top row and
        // never reach it.  An empty document has nothing left to scroll to,
        // and a zero-height viewport nothing to measure; read both as 100%.
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
        spans.push(cursor_span);
        spans.push(info_span);

        Paragraph::new(Line::from(spans))
            .style(bar_style)
            .render(area, buf);
    }
}

/// Map a vim sub-mode badge label onto its `status_mode_vim_*` style.
/// These are the canonical per-vim-mode colors (NORMAL = primary,
/// INSERT = success, VISUAL / V-LINE = secondary); the editor cursor
/// mirrors the same fields, so chip and cursor always agree.
fn vim_badge_style(theme: &Theme, label: &str) -> Style {
    match label {
        "INSERT" => theme.status_mode_vim_insert,
        "VISUAL" | "V-LINE" => theme.status_mode_vim_visual,
        // NORMAL (and any future label, e.g. Operator-pending).
        _ => theme.status_mode_vim_normal,
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

    /// A plain state with every optional field empty: no cursor
    /// position, no breadcrumb, no diff progress, no vim badge.  Tests
    /// that need one of those spell out just that field and fill the
    /// rest with `..base_state(..)`, so a new `StatusBarState` field
    /// costs one line here instead of one per test.
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
            vim_mode_label: None,
        }
    }

    /// Render `state` into a one-row bar `width` cells wide and scrape
    /// the row back as a string (first char of each cell).  The single
    /// place these tests touch `TestBackend`.
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
        // The diff-review keymap owns every key while `Mode::Diff` is
        // active (`vim_deferred` in `App::dispatch_single_key`), so a
        // `NORMAL` badge would advertise a handler that isn't live.
        let output = render_bar(
            StatusBarState {
                diff_progress: Some((3, 7)),
                vim_mode_label: Some("NORMAL"),
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
        // The progress counter stays adjacent to the badge it belongs to.
        assert!(
            output.contains("3/7"),
            "diff progress must ride beside the badge: {output:?}"
        );
    }

    #[test]
    fn vim_badge_still_wins_outside_diff_mode() {
        // The suppression is scoped to diff — every other mode keeps the
        // sub-mode badge in the rendering mode's place.
        let output = render_bar(
            StatusBarState {
                vim_mode_label: Some("NORMAL"),
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

    #[test]
    fn vim_badge_uses_per_sub_mode_colors() {
        let t = Theme::default();
        assert_eq!(
            super::vim_badge_style(&t, "NORMAL"),
            t.status_mode_vim_normal
        );
        assert_eq!(
            super::vim_badge_style(&t, "INSERT"),
            t.status_mode_vim_insert
        );
        assert_eq!(
            super::vim_badge_style(&t, "VISUAL"),
            t.status_mode_vim_visual
        );
        assert_eq!(
            super::vim_badge_style(&t, "V-LINE"),
            t.status_mode_vim_visual
        );
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
        // 6 source lines rendering as 10 scrollable rows, scrolled to the
        // last row: the count must read the source lines, the percentage
        // must still reach 100% at the bottom.
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

    /// …and it is not stuck at 100%: the same viewport at the top of the same
    /// document reports the fraction it can actually see.
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

    /// A degenerate zero-row viewport must not read 0% for a document whose
    /// first row is nominally visible — `max(1)` keeps the numerator at the
    /// pre-`viewport_rows` behavior rather than collapsing it.
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
