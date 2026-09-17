//! Shared building blocks for popup overlays: the centered frame, vertical scroll state
//! (keyboard and wheel), and content-aware sizing that grows the frame to fit its body,
//! clamped only to the terminal.  Each overlay keeps its own widget and layout but routes
//! scroll arithmetic, frame rendering, and sizing through here.  `ui::modal` is the
//! canonical text-body consumer; `ui::command_palette` shows pinned regions.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Widget, Wrap},
};

use crate::config::Theme;

/// Visual urgency of a modal; drives only the title color and is independent of
/// dismissability (that comes from `Modal::dismissable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModalKind {
    #[default]
    Normal,
    Warning,
    /// Used by [`NoticeModal`](crate::app::modal::NoticeModal) for sticky error notices.
    Error,
}

impl ModalKind {
    /// Title style for this kind, given the active theme.
    pub fn title_style(self, theme: &Theme) -> ratatui::style::Style {
        match self {
            Self::Normal => theme.modal_title_normal,
            Self::Warning => theme.modal_title_warning,
            Self::Error => theme.modal_title_error,
        }
    }
}

/// Default maximum horizontal padding per side; a modal can raise it via
/// [`ContentSize::max_pad_h`].
pub const MAX_PAD_H: u16 = 4;
/// Minimum horizontal padding per side.
pub const MIN_PAD_H: u16 = 1;
/// Comfortable maximum *content* width for a prose modal.  Without a cap a one-paragraph
/// body stretches to the full terminal width.  Opt in with
/// [`crate::ui::ModalView::with_max_content_width`]; tabular modals should not, since
/// clamping would wrap columns meant to align.
pub const PROSE_CONTENT_WIDTH: u16 = 64;
/// Vertical chrome reserved by `draw_frame`: top pad + title + spacer + bottom pad.
pub const VERTICAL_CHROME_ROWS: u16 = 4;
/// Row offset of the body within the modal rect (`VERTICAL_CHROME_ROWS - 1`, since the
/// bottom pad sits below the body).  Named separately so a chrome change can't desync it.
pub const VERTICAL_CHROME_TOP: u16 = 3;

/// Text of the close hint / clickable affordance.  Always 3 cells wide.
pub const CLOSE_HINT: &str = "esc";

/// Natural size of an overlay's content, in display cells.  `width` / `height` describe the
/// scrolling region alone; pinned regions are reported separately.
#[derive(Debug, Clone, Copy)]
pub struct ContentSize {
    /// Longest body row in display columns.
    pub width: u16,
    /// Scrolling-region row count, pre-clamp.
    pub height: u16,
    /// Rows reserved above the scroll viewport (e.g. palette input row).
    pub pinned_top: u16,
    /// Rows reserved below the scroll viewport (e.g. button row, footer).
    pub pinned_bottom: u16,
    /// Maximum horizontal padding per side (default [`MAX_PAD_H`]).  Single source of truth:
    /// [`FrameOpts`] carries the same `ContentSize` so sizing and padding can't disagree.
    pub max_pad_h: u16,
}

impl Default for ContentSize {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            pinned_top: 0,
            pinned_bottom: 0,
            max_pad_h: MAX_PAD_H,
        }
    }
}

/// Vertical-scroll bookkeeping shared by every overlay.
///
/// Contract: each render calls [`Self::observe`] with the post-layout heights; after that
/// `scroll` lies in `[0, max_scroll()]`, and a nonzero [`Self::max_scroll`] is what gates
/// the [`crate::ui::scrollbar`].
#[derive(Debug, Clone, Default)]
pub struct ScrollContainerState {
    pub scroll: u16,
    pub last_total: u16,
    pub last_visible: u16,
}

impl ScrollContainerState {
    #[allow(dead_code)] // used by tests in this module
    pub fn new() -> Self {
        Self::default()
    }

    /// Largest valid `scroll`; `0` when the body fits.
    pub fn max_scroll(&self) -> u16 {
        self.last_total.saturating_sub(self.last_visible)
    }

    /// Adjust scroll by `delta` rows, clamped at both ends.
    pub fn scroll_by(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        let max = self.max_scroll() as i32;
        let next = (self.scroll as i32 + delta).clamp(0, max);
        self.scroll = next as u16;
    }

    /// Handle Up/Down/PgUp/PgDn/Home/End as scroll keys; returns `true` if consumed.  For
    /// text bodies with no focus concept; focusable overlays use [`Self::handle_paging_key`].
    pub fn handle_scroll_key(&mut self, key: &crossterm::event::KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        match key.code {
            KeyCode::Up => {
                self.scroll_by(-1);
                true
            }
            KeyCode::Down => {
                self.scroll_by(1);
                true
            }
            KeyCode::PageUp => {
                self.scroll_by(-(self.last_visible.max(1) as i32));
                true
            }
            KeyCode::PageDown => {
                self.scroll_by(self.last_visible.max(1) as i32);
                true
            }
            KeyCode::Home if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll = 0;
                true
            }
            KeyCode::End if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll = self.max_scroll();
                true
            }
            _ => false,
        }
    }

    /// Handle PgUp/PgDn/Home/End only, leaving Up/Down free for focus moves.
    pub fn handle_paging_key(&mut self, key: &crossterm::event::KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        match key.code {
            KeyCode::PageUp => {
                self.scroll_by(-(self.last_visible.max(1) as i32));
                true
            }
            KeyCode::PageDown => {
                self.scroll_by(self.last_visible.max(1) as i32);
                true
            }
            KeyCode::Home if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll = 0;
                true
            }
            KeyCode::End if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll = self.max_scroll();
                true
            }
            _ => false,
        }
    }

    /// Adjust `scroll` so `focus_row` (body row coords) lies within the visible window.
    pub fn ensure_visible(&mut self, focus_row: u16) {
        if self.last_visible == 0 {
            return;
        }
        if focus_row < self.scroll {
            self.scroll = focus_row;
        } else if focus_row >= self.scroll + self.last_visible {
            self.scroll = focus_row + 1 - self.last_visible;
        }
        // `focus_row` may exceed total, and observe() may not have run for the new layout.
        let max = self.max_scroll();
        if self.scroll > max {
            self.scroll = max;
        }
    }

    /// Record the layout heights and clamp `scroll`; once per render.
    pub fn observe(&mut self, total: u16, visible: u16) {
        self.last_total = total;
        self.last_visible = visible;
        let max = self.max_scroll();
        if self.scroll > max {
            self.scroll = max;
        }
    }
}

/// Centered rectangle sized to fit `content`, clamped to `area`.  When clamped, height
/// overflow scrolls and width overflow wraps (the caller's responsibility).
pub fn centered_rect_for_content(content: ContentSize, area: Rect) -> Rect {
    let (modal_width, modal_height) = modal_dimensions_for(content, area);
    let x = area.x + (area.width.saturating_sub(modal_width)) / 2;
    let y = area.y + (area.height.saturating_sub(modal_height)) / 2;
    Rect {
        x,
        y,
        width: modal_width,
        height: modal_height,
    }
}

/// Outer modal width and height: content plus horizontal padding and vertical chrome,
/// clamped to `area`.
fn modal_dimensions_for(content: ContentSize, area: Rect) -> (u16, u16) {
    let modal_width = (content.width)
        .saturating_add(2 * content.max_pad_h)
        .min(area.width);
    let body_height = content
        .height
        .saturating_add(content.pinned_top)
        .saturating_add(content.pinned_bottom)
        .max(1);
    let modal_height = body_height
        .saturating_add(VERTICAL_CHROME_ROWS)
        .min(area.height);
    (modal_width, modal_height)
}

/// Options for `draw_frame`.
pub struct FrameOpts<'a> {
    /// Bare title text, rendered on the title row at the left padding edge.
    pub title: &'a str,
    pub kind: ModalKind,
    /// Render the `esc` hint at the right of the title row and populate
    /// [`FrameLayout::esc_hit_rect`].
    pub show_close_hint: bool,
    /// Must be the *same* value fed to [`centered_rect_for_content`] so sizing and padding
    /// agree.
    pub content: ContentSize,
    pub theme: &'a Theme,
}

/// Layout produced by `draw_frame`.
pub struct FrameLayout {
    /// Inner area for body + pinned regions.
    pub body: Rect,
    /// Absolute rect of the `esc` hint, when rendered; callers cache it for click hit-tests.
    pub esc_hit_rect: Option<Rect>,
    /// Absolute column of the rightmost padding cell, the scrollbar gutter.
    pub scrollbar_col: u16,
}

/// Render the modal chrome (clear, fill, title row with optional close hint, spacer) and
/// return the body layout.  No border characters: same-background padding is the frame.
pub fn draw_frame(area: Rect, buf: &mut Buffer, opts: FrameOpts<'_>) -> FrameLayout {
    Clear.render(area, buf);
    Block::default()
        .style(opts.theme.modal_bg)
        .render(area, buf);

    let pad_h = compute_pad_h(area.width, opts.content.width, opts.content.max_pad_h);

    let body_x = area.x + pad_h;
    let body_w = area.width.saturating_sub(2 * pad_h);
    let body_y = area.y + VERTICAL_CHROME_TOP;
    let body_h = area.height.saturating_sub(VERTICAL_CHROME_ROWS);
    let body = Rect {
        x: body_x,
        y: body_y,
        width: body_w,
        height: body_h,
    };

    let mut esc_hit_rect = None;
    if area.height >= 2 && body_w > 0 {
        let title_row = area.y + 1;
        let title_left = area.x + pad_h;
        let title_right_edge = area.x + area.width - pad_h; // exclusive
        let title_inner_w = title_right_edge.saturating_sub(title_left);

        // Reserve the hint first (plus one cell of separation) so the title never overlaps it.
        let hint_w: u16 = CLOSE_HINT.len() as u16;
        let (title_w, hint_rect) = if opts.show_close_hint && title_inner_w > hint_w + 1 {
            let hr = Rect {
                x: title_right_edge.saturating_sub(hint_w),
                y: title_row,
                width: hint_w,
                height: 1,
            };
            (title_inner_w.saturating_sub(hint_w + 1), Some(hr))
        } else {
            (title_inner_w, None)
        };

        let title_style = opts.kind.title_style(opts.theme);
        let title_para = Paragraph::new(Line::from(Span::styled(opts.title, title_style)))
            .style(opts.theme.modal_bg);
        let title_area = Rect {
            x: title_left,
            y: title_row,
            width: title_w,
            height: 1,
        };
        title_para.render(title_area, buf);

        if let Some(hr) = hint_rect {
            let hint = Paragraph::new(Line::from(Span::styled(
                CLOSE_HINT,
                opts.theme.modal_close_hint,
            )))
            .style(opts.theme.modal_bg);
            hint.render(hr, buf);
            esc_hit_rect = Some(hr);
        }
    }

    let scrollbar_col = area.x + area.width.saturating_sub(1);

    FrameLayout {
        body,
        esc_hit_rect,
        scrollbar_col,
    }
}

/// Text width inside a modal whose content wants `content_w` columns, in a terminal `area_w`
/// wide.  Runs the frame's own width clamp and [`compute_pad_h`], so a modal that sizes itself
/// by wrapped text, before its frame exists, wraps at the width the text renders at.
pub fn modal_inner_width(content_w: u16, area_w: u16, max_pad_h: u16) -> u16 {
    let modal_w = content_w.saturating_add(2 * max_pad_h).min(area_w);
    let pad_h = compute_pad_h(modal_w, content_w, max_pad_h);
    modal_w.saturating_sub(2 * pad_h).max(1)
}

/// Per-side horizontal padding: half the slack, clamped to `[MIN_PAD_H, max_pad_h]`.
pub fn compute_pad_h(area_w: u16, content_w: u16, max_pad_h: u16) -> u16 {
    let slack = area_w.saturating_sub(content_w);
    (slack / 2).clamp(MIN_PAD_H, max_pad_h)
}

/// Wrapped row count for `lines` at `width` under `Wrap { trim: false }`.  Delegates to
/// ratatui's `Paragraph::line_count` (feature `unstable-rendered-line-info`) so sizing
/// matches the real `WordWrapper`: a character-level `div_ceil` undercounts when a single
/// word wider than `width` forces an extra row.
pub fn wrapped_rows(lines: &[Line<'_>], width: u16) -> u16 {
    if width == 0 {
        return lines.len() as u16;
    }
    let owned: Vec<Line<'static>> = lines
        .iter()
        .map(|l| Line {
            spans: l
                .spans
                .iter()
                .map(|s| Span::styled(s.content.clone().into_owned(), s.style))
                .collect(),
            style: l.style,
            alignment: l.alignment,
        })
        .collect();
    Paragraph::new(owned)
        .wrap(Wrap { trim: false })
        .line_count(width)
        .min(u16::MAX as usize) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // ── scroll_by ────────────────────────────────────────────────────────

    #[test]
    fn scroll_by_clamps_at_top() {
        let mut s = ScrollContainerState {
            scroll: 2,
            last_total: 10,
            last_visible: 5,
        };
        s.scroll_by(-100);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn scroll_by_clamps_at_bottom() {
        let mut s = ScrollContainerState {
            last_total: 10,
            last_visible: 5,
            ..ScrollContainerState::new()
        };
        s.scroll_by(100);
        assert_eq!(s.scroll, 5);
    }

    #[test]
    fn scroll_by_is_a_noop_when_body_fits() {
        let mut s = ScrollContainerState {
            last_total: 4,
            last_visible: 10,
            ..ScrollContainerState::new()
        };
        s.scroll_by(3);
        assert_eq!(s.scroll, 0);
    }

    // ── handle_scroll_key ────────────────────────────────────────────────

    #[test]
    fn handle_scroll_key_consumes_arrow_keys() {
        let mut s = ScrollContainerState {
            last_total: 20,
            last_visible: 5,
            ..ScrollContainerState::new()
        };
        assert!(s.handle_scroll_key(&key(KeyCode::Down)));
        assert_eq!(s.scroll, 1);
        assert!(s.handle_scroll_key(&key(KeyCode::Up)));
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn handle_scroll_key_pgdown_jumps_visible_height() {
        let mut s = ScrollContainerState {
            last_total: 30,
            last_visible: 10,
            ..ScrollContainerState::new()
        };
        s.handle_scroll_key(&key(KeyCode::PageDown));
        assert_eq!(s.scroll, 10);
        s.handle_scroll_key(&key(KeyCode::PageDown));
        assert_eq!(s.scroll, 20);
        s.handle_scroll_key(&key(KeyCode::PageDown));
        assert_eq!(s.scroll, 20);
    }

    #[test]
    fn handle_scroll_key_home_end_jump_to_extremes() {
        let mut s = ScrollContainerState {
            scroll: 4,
            last_total: 12,
            last_visible: 4,
        };
        assert!(s.handle_scroll_key(&key(KeyCode::End)));
        assert_eq!(s.scroll, 8);
        assert!(s.handle_scroll_key(&key(KeyCode::Home)));
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn handle_scroll_key_returns_false_for_unrecognized() {
        let mut s = ScrollContainerState::default();
        assert!(!s.handle_scroll_key(&key(KeyCode::Char('x'))));
        assert!(!s.handle_scroll_key(&key(KeyCode::Enter)));
    }

    // ── handle_paging_key ────────────────────────────────────────────────

    #[test]
    fn handle_paging_key_returns_false_for_arrow_keys() {
        let mut s = ScrollContainerState {
            last_total: 20,
            last_visible: 5,
            ..ScrollContainerState::new()
        };
        assert!(!s.handle_paging_key(&key(KeyCode::Up)));
        assert!(!s.handle_paging_key(&key(KeyCode::Down)));
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn handle_paging_key_consumes_pgup_pgdn_home_end() {
        let mut s = ScrollContainerState {
            last_total: 30,
            last_visible: 10,
            ..ScrollContainerState::new()
        };
        assert!(s.handle_paging_key(&key(KeyCode::PageDown)));
        assert_eq!(s.scroll, 10);
        assert!(s.handle_paging_key(&key(KeyCode::Home)));
        assert_eq!(s.scroll, 0);
        assert!(s.handle_paging_key(&key(KeyCode::End)));
        assert_eq!(s.scroll, 20);
        assert!(s.handle_paging_key(&key(KeyCode::PageUp)));
        assert_eq!(s.scroll, 10);
    }

    // ── ensure_visible ───────────────────────────────────────────────────

    #[test]
    fn ensure_visible_scrolls_down_when_focus_below_viewport() {
        let mut s = ScrollContainerState {
            scroll: 0,
            last_total: 20,
            last_visible: 5,
        };
        s.ensure_visible(7);
        assert_eq!(s.scroll, 3);
    }

    #[test]
    fn ensure_visible_scrolls_up_when_focus_above_viewport() {
        let mut s = ScrollContainerState {
            scroll: 10,
            last_total: 20,
            last_visible: 5,
        };
        s.ensure_visible(2);
        assert_eq!(s.scroll, 2);
    }

    #[test]
    fn ensure_visible_is_a_noop_when_focus_already_visible() {
        let mut s = ScrollContainerState {
            scroll: 5,
            last_total: 20,
            last_visible: 5,
        };
        s.ensure_visible(7);
        assert_eq!(s.scroll, 5);
    }

    #[test]
    fn ensure_visible_does_nothing_before_first_observe() {
        let mut s = ScrollContainerState::new();
        s.ensure_visible(100);
        assert_eq!(s.scroll, 0);
    }

    // ── observe ──────────────────────────────────────────────────────────

    #[test]
    fn observe_clamps_scroll_when_body_shrinks() {
        let mut s = ScrollContainerState {
            scroll: 30,
            last_total: 50,
            last_visible: 10,
        };
        s.observe(8, 10);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn observe_records_total_and_visible() {
        let mut s = ScrollContainerState::new();
        s.observe(20, 5);
        assert_eq!(s.last_total, 20);
        assert_eq!(s.last_visible, 5);
        assert_eq!(s.max_scroll(), 15);
    }

    // ── centered_rect_for_content ────────────────────────────────────────

    #[test]
    fn centered_rect_grows_to_content_when_terminal_is_large() {
        let area = Rect::new(0, 0, 200, 60);
        let content = ContentSize {
            width: 30,
            height: 5,
            pinned_top: 0,
            pinned_bottom: 1,
            ..Default::default()
        };
        let r = centered_rect_for_content(content, area);
        assert_eq!(r.width, 38);
        assert_eq!(r.height, 10);
        assert_eq!(r.x, (200 - 38) / 2);
        assert_eq!(r.y, (60 - 10) / 2);
    }

    #[test]
    fn centered_rect_clamps_to_area() {
        let area = Rect::new(0, 0, 20, 6);
        let content = ContentSize {
            width: 40,
            height: 10,
            pinned_top: 0,
            pinned_bottom: 0,
            ..Default::default()
        };
        let r = centered_rect_for_content(content, area);
        assert_eq!(r.width, 20);
        assert_eq!(r.height, 6);
    }

    #[test]
    fn centered_rect_includes_pinned_regions_in_height() {
        let area = Rect::new(0, 0, 100, 30);
        let content = ContentSize {
            width: 10,
            height: 5,
            pinned_top: 2,
            pinned_bottom: 3,
            ..Default::default()
        };
        let r = centered_rect_for_content(content, area);
        assert_eq!(r.height, 14);
    }

    // ── compute_pad_h ────────────────────────────────────────────────────

    #[test]
    fn pad_h_caps_at_max_when_terminal_is_wide() {
        assert_eq!(compute_pad_h(200, 30, MAX_PAD_H), MAX_PAD_H);
    }

    #[test]
    fn pad_h_floors_at_min_when_content_fills_modal() {
        assert_eq!(compute_pad_h(30, 30, MAX_PAD_H), MIN_PAD_H);
        assert_eq!(compute_pad_h(20, 30, MAX_PAD_H), MIN_PAD_H);
    }

    #[test]
    fn pad_h_uses_full_slack_when_modest() {
        assert_eq!(compute_pad_h(38, 30, MAX_PAD_H), 4);
        assert_eq!(compute_pad_h(36, 30, MAX_PAD_H), 3);
        assert_eq!(compute_pad_h(32, 30, MAX_PAD_H), MIN_PAD_H);
    }

    #[test]
    fn modal_inner_width_is_the_content_width_when_it_fits() {
        assert_eq!(modal_inner_width(30, 200, MAX_PAD_H), 30);
        // Too narrow for the content: the frame clamps to the terminal and pads the minimum.
        assert_eq!(modal_inner_width(60, 40, MAX_PAD_H), 40 - 2 * MIN_PAD_H);
    }

    #[test]
    fn pad_h_raised_cap_in_wide_terminal() {
        assert_eq!(compute_pad_h(200, 30, 8), 8);
    }

    #[test]
    fn pad_h_raised_cap_still_floors_at_min_when_narrow() {
        assert_eq!(compute_pad_h(32, 30, 8), MIN_PAD_H);
    }

    #[test]
    fn pad_h_raised_cap_shrinks_gracefully() {
        assert_eq!(compute_pad_h(40, 30, 8), 5);
    }

    #[test]
    fn centered_rect_grows_to_raised_max_pad_when_terminal_is_large() {
        let area = Rect::new(0, 0, 200, 60);
        let content = ContentSize {
            width: 30,
            height: 5,
            pinned_top: 0,
            pinned_bottom: 1,
            max_pad_h: 8,
        };
        let r = centered_rect_for_content(content, area);
        assert_eq!(r.width, 30 + 2 * 8);
    }

    #[test]
    fn centered_rect_with_raised_max_pad_clamps_to_area_when_narrow() {
        let area = Rect::new(0, 0, 20, 6);
        let content = ContentSize {
            width: 40,
            height: 10,
            pinned_top: 0,
            pinned_bottom: 0,
            max_pad_h: 8,
        };
        let r = centered_rect_for_content(content, area);
        assert_eq!(r.width, 20);
    }

    // ── wrapped_rows ─────────────────────────────────────────────────────

    #[test]
    fn wrapped_rows_counts_each_short_line_once() {
        let lines = vec![Line::raw("abc"), Line::raw("def")];
        assert_eq!(wrapped_rows(&lines, 80), 2);
    }

    #[test]
    fn wrapped_rows_counts_blank_lines_as_one_row() {
        let lines = vec![Line::raw(""), Line::raw("")];
        assert_eq!(wrapped_rows(&lines, 80), 2);
    }

    #[test]
    fn wrapped_rows_wraps_long_lines() {
        let lines = vec![Line::raw("a".repeat(200))];
        assert_eq!(wrapped_rows(&lines, 80), 3);
    }

    #[test]
    fn wrapped_rows_handles_zero_width_gracefully() {
        let lines = vec![Line::raw("a"), Line::raw("b"), Line::raw("c")];
        assert_eq!(wrapped_rows(&lines, 0), 3);
    }
}
