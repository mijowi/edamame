//! Shared building blocks for popup overlays.
//!
//! `ModalView`, `PaletteView`, `SettingsView`, and `KeybindsView` all need
//! the same primitives:
//!
//! - a centred bordered frame whose title carries scroll-indicator arrows
//!   (`↓`, `↑`, `↑↓`) when the body overflows;
//! - vertical scroll state with keyboard *and* mouse-wheel control;
//! - content-aware sizing — the frame grows to fit its body, clamped only
//!   to the terminal area so we never paint a 70%-of-screen modal that's
//!   mostly empty space.
//!
//! This module exposes those primitives as a small struct and a handful of
//! free functions.  Each overlay keeps its own widget type and bespoke
//! layout, but routes scroll arithmetic, frame rendering, and centred-rect
//! sizing through the helpers here.  See `src/ui/modal.rs` for the
//! canonical text-body consumer and `src/ui/command_palette.rs` for an
//! example with pinned regions (input row above the scrolling list).

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Widget, Wrap},
};

use crate::config::Theme;

/// Visual urgency of a modal.  Drives the title color (Normal =
/// `primary`, Warning = `warning`, Error = `error`)
/// and is independent of dismissability — a Warning may be either
/// freely dismissable (informational) or gated (must press a button),
/// depending on the owning `Modal::dismissable` return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModalKind {
    #[default]
    Normal,
    Warning,
    /// Error variant — reserved for future error modals (e.g.
    /// unrecoverable I/O failures).  Currently unused; the field
    /// exists so themes and the title-style switch are exhaustive.
    #[allow(dead_code)]
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

/// Maximum horizontal padding inside a modal, in cells.  Padding shrinks
/// to [`MIN_PAD_H`] when the terminal can't accommodate the full width.
pub const MAX_PAD_H: u16 = 4;
/// Minimum horizontal padding inside a modal, in cells.
pub const MIN_PAD_H: u16 = 1;
/// Vertical chrome rows reserved by `draw_frame`: 1 top pad + 1 title +
/// 1 spacer + 1 bottom pad.  Pinned content (button row, footer) sits
/// above the bottom pad inside the body rect returned in
/// [`FrameLayout::body`].
pub const VERTICAL_CHROME_ROWS: u16 = 4;
/// Row offset (within the modal rect) at which the body begins —
/// past the top pad, title, and spacer.  Equal to
/// `VERTICAL_CHROME_ROWS - 1` (one row of chrome, the bottom pad,
/// sits *below* the body).  Named separately so a layout change to
/// the chrome doesn't silently desync from this offset.
pub const VERTICAL_CHROME_TOP: u16 = 3;

/// The literal text rendered as the modal close hint / clickable
/// affordance.  Always 3 cells wide.
pub const CLOSE_HINT: &str = "esc";

/// Natural size of an overlay's content, in display cells.
///
/// `width` and `height` describe the *scrolling region* alone; pinned
/// regions (palette input row, settings/keybinds error footer, modal
/// button row) are reported separately via `pinned_top` / `pinned_bottom`.
/// `centered_rect_for_content` adds frame padding and clamps to the
/// available terminal area.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContentSize {
    /// Longest body row in display columns.
    pub width: u16,
    /// Total scrolling-region row count (pre-clamp; if larger than the
    /// available height the body simply scrolls inside).
    pub height: u16,
    /// Rows reserved above the scroll viewport (e.g. palette input row).
    pub pinned_top: u16,
    /// Rows reserved below the scroll viewport (e.g. button row, footer).
    pub pinned_bottom: u16,
}

/// Vertical-scroll bookkeeping shared by every overlay.  Embedded as
/// `scroll_state` on each overlay's state struct.
///
/// The contract: each render must call [`Self::observe`] with the
/// post-layout `total` and `visible` heights.  After that, `scroll`
/// is guaranteed to lie in `[0, max_scroll()]` and [`Self::arrow`]
/// returns the right indicator for the title bar.
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

    /// Largest valid `scroll` given the most-recently-observed body
    /// dimensions.  Returns `0` when the body fits — i.e. scrolling is
    /// disabled.
    pub fn max_scroll(&self) -> u16 {
        self.last_total.saturating_sub(self.last_visible)
    }

    /// Adjust scroll by `delta` rows (negative = toward top, positive =
    /// toward bottom).  Clamped at both ends so callers never need to
    /// range-check before forwarding wheel events.
    pub fn scroll_by(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        let max = self.max_scroll() as i32;
        let next = (self.scroll as i32 + delta).clamp(0, max);
        self.scroll = next as u16;
    }

    /// Handle Up/Down/PgUp/PgDn/Home/End as scroll keys.  Returns
    /// `true` if the key was consumed.
    ///
    /// Used by `ModalView` (text bodies, no focus concept).  Phase 10
    /// overlays should use [`Self::handle_paging_key`] instead so that
    /// Up/Down remain available for focus moves.
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

    /// Handle PgUp/PgDn/Home/End as paging keys.  Returns `true` if the
    /// key was consumed.  Up/Down are intentionally *not* consumed, so
    /// they remain available for focus moves in row-based overlays.
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

    /// Adjust `scroll` so `focus_row` (in body row coords) lies within
    /// the visible window `[scroll, scroll + last_visible)`.  Use this
    /// in row-based overlays after Up/Down move the focus.
    pub fn ensure_visible(&mut self, focus_row: u16) {
        if self.last_visible == 0 {
            return;
        }
        if focus_row < self.scroll {
            self.scroll = focus_row;
        } else if focus_row >= self.scroll + self.last_visible {
            self.scroll = focus_row + 1 - self.last_visible;
        }
        // Clamp again — `focus_row` could exceed total in a degenerate
        // call, and observe() may not have run yet for the new layout.
        let max = self.max_scroll();
        if self.scroll > max {
            self.scroll = max;
        }
    }

    /// Update `last_total` / `last_visible` and clamp `scroll`.  Call
    /// once per render after the layout is known.
    pub fn observe(&mut self, total: u16, visible: u16) {
        self.last_total = total;
        self.last_visible = visible;
        let max = self.max_scroll();
        if self.scroll > max {
            self.scroll = max;
        }
    }
}

/// Centred rectangle sized to fit `content`, clamped to `area`.
///
/// The returned rect's interior (after subtracting the 2-cell border)
/// has room for `pinned_top + height + pinned_bottom` rows and
/// `width + 2` columns of padding, whenever the terminal allows.  When
/// the terminal is smaller, height clamps and the body scrolls; width
/// clamps and lines wrap (caller's responsibility).
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

/// Compute the modal's outer width and height for a given content size
/// and available area.  Padding is `2 * MAX_PAD_H` cells on the
/// horizontal axis (clamped to area), and [`VERTICAL_CHROME_ROWS`]
/// (top pad + title + spacer + bottom pad) on the vertical axis.
/// Inside we want `pinned_top + height + pinned_bottom` rows.  Both
/// dimensions clamp to `area`.
fn modal_dimensions_for(content: ContentSize, area: Rect) -> (u16, u16) {
    let modal_width = (content.width)
        .saturating_add(2 * MAX_PAD_H)
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

/// Options controlling how `draw_frame` paints the modal chrome.
pub struct FrameOpts<'a> {
    /// Title text rendered on row 1 of `area`, left-aligned at the left
    /// padding edge.  Not formatted — pass the bare title.
    pub title: &'a str,
    /// Visual urgency — drives the title color.
    pub kind: ModalKind,
    /// When true, render the `esc` close hint at the right edge of the
    /// title row using `theme.modal_close_hint`, and populate
    /// [`FrameLayout::esc_hit_rect`] for click hit-testing.
    pub show_close_hint: bool,
    /// Natural body content width.  Used to derive horizontal padding
    /// (`pad_h = ((area.width - content_width) / 2).clamp(MIN_PAD_H,
    /// MAX_PAD_H)`).  Pass the same value used to size `area`.
    pub content_width: u16,
    pub theme: &'a Theme,
}

/// Layout produced by `draw_frame`.  Carries everything callers need to
/// place the body content, the optional scrollbar, and to hit-test
/// later clicks against the close hint.
pub struct FrameLayout {
    /// Inner area for body + pinned regions.  Excludes the rightmost
    /// padding column when a scrollbar is to be drawn — the scrollbar
    /// paints into [`Self::scrollbar_col`] inside the right padding.
    pub body: Rect,
    /// Absolute terminal coordinates of the `esc` close hint, when
    /// rendered.  Callers cache this on their state struct so a later
    /// click event can hit-test against it.
    pub esc_hit_rect: Option<Rect>,
    /// Absolute terminal column of the rightmost padding cell.  Use
    /// for the scrollbar gutter when the body overflows.
    pub scrollbar_col: u16,
}

/// Render the modal chrome: clear, fill with `modal_bg`, draw the title
/// row with optional close hint, leave a blank spacer, and return the
/// inner body layout.  No border characters — same-bg padding is the
/// frame.
pub fn draw_frame(area: Rect, buf: &mut Buffer, opts: FrameOpts<'_>) -> FrameLayout {
    Clear.render(area, buf);
    // Fill the entire modal rect with modal_bg so the padding picks up
    // the same surface color as the body.
    Block::default()
        .style(opts.theme.modal_bg)
        .render(area, buf);

    let pad_h = compute_pad_h(area.width, opts.content_width);

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

    // Title row: row 1 (after the 1-row top pad).
    let mut esc_hit_rect = None;
    if area.height >= 2 && body_w > 0 {
        let title_row = area.y + 1;
        let title_left = area.x + pad_h;
        let title_right_edge = area.x + area.width - pad_h; // exclusive
        let title_inner_w = title_right_edge.saturating_sub(title_left);

        // Reserve the close hint at the right edge first so the title
        // text never overlaps it.
        let hint_w: u16 = CLOSE_HINT.len() as u16;
        let (title_w, hint_rect) = if opts.show_close_hint && title_inner_w > hint_w + 1 {
            // Leave at least one cell of separation between title and hint.
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

/// Horizontal padding for a modal of `area_w` total width with a
/// natural body of `content_w` cells.  Centred: each side gets
/// `(area_w - content_w) / 2`, clamped to `[MIN_PAD_H, MAX_PAD_H]`.
pub fn compute_pad_h(area_w: u16, content_w: u16) -> u16 {
    let slack = area_w.saturating_sub(content_w);
    (slack / 2).clamp(MIN_PAD_H, MAX_PAD_H)
}

/// Total wrapped row count for `lines` at `width` columns under
/// `Paragraph::wrap(Wrap { trim: false })`.  Delegates to ratatui's own
/// `Paragraph::line_count` (gated by the `unstable-rendered-line-info`
/// feature) so the pre-render sizing matches the actual `WordWrapper`
/// output — character-level `div_ceil` undercounts when a single word
/// wider than `width` forces an extra row.  Pure; used by `ModalView`
/// to size text bodies before rendering.
pub fn wrapped_rows(lines: &[Line<'_>], width: u16) -> u16 {
    if width == 0 {
        return lines.len() as u16;
    }
    // `Paragraph::new` accepts owned `Text`, so clone the lines into a
    // local `Vec` rather than borrowing the slice (line_count needs
    // owned storage to feed its grapheme iterator).
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
        // Clamped at max_scroll.
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
    fn handle_scroll_key_returns_false_for_unrecognised() {
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
        // Focus row 7 must be in [scroll, scroll+5), so scroll = 3.
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
        // Body shrinks to fit entirely.
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
        };
        let r = centered_rect_for_content(content, area);
        // 30 + 2 * MAX_PAD_H (4) horizontal padding.
        assert_eq!(r.width, 38);
        // 5 + 1 pinned + 4 vertical chrome (top pad + title + spacer + bot pad).
        assert_eq!(r.height, 10);
        // Centred:
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
        };
        let r = centered_rect_for_content(content, area);
        // 5 + 2 + 3 pinned + 4 vertical chrome.
        assert_eq!(r.height, 14);
    }

    // ── compute_pad_h ────────────────────────────────────────────────────

    #[test]
    fn pad_h_caps_at_max_when_terminal_is_wide() {
        // 200-wide modal, 30-cell content → slack 170, half = 85,
        // clamped to MAX_PAD_H = 4.
        assert_eq!(compute_pad_h(200, 30), MAX_PAD_H);
    }

    #[test]
    fn pad_h_floors_at_min_when_content_fills_modal() {
        // Modal width equals content width: no slack.  Padding still
        // honours MIN_PAD_H so the title text never kisses the edge.
        assert_eq!(compute_pad_h(30, 30), MIN_PAD_H);
        assert_eq!(compute_pad_h(20, 30), MIN_PAD_H);
    }

    #[test]
    fn pad_h_uses_full_slack_when_modest() {
        // 38-wide modal, 30-cell content → slack 8, half = 4 = MAX.
        assert_eq!(compute_pad_h(38, 30), 4);
        // 36-wide modal, 30-cell content → slack 6, half = 3.
        assert_eq!(compute_pad_h(36, 30), 3);
        // 32-wide modal, 30-cell content → slack 2, half = 1 = MIN.
        assert_eq!(compute_pad_h(32, 30), MIN_PAD_H);
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
        // 200 / 80 = 2.5 → 3 rows.
        assert_eq!(wrapped_rows(&lines, 80), 3);
    }

    #[test]
    fn wrapped_rows_handles_zero_width_gracefully() {
        let lines = vec![Line::raw("a"), Line::raw("b"), Line::raw("c")];
        assert_eq!(wrapped_rows(&lines, 0), 3);
    }
}
