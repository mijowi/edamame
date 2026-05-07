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
    widgets::{Block, Borders, Clear, Widget},
};

use crate::config::Theme;

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

    /// Returns the scroll-indicator arrow for the title bar, or `None`
    /// when the body fits in the viewport.
    pub fn arrow(&self) -> Option<&'static str> {
        let max = self.max_scroll();
        if max == 0 {
            return None;
        }
        Some(match self.scroll {
            0 => "↓",
            s if s >= max => "↑",
            _ => "↑↓",
        })
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

/// Build the title string, optionally suffixed with a scroll-indicator
/// arrow.  Pure so it's easy to unit-test the indicator logic without
/// rendering.
pub fn format_title(title: &str, arrow: Option<&str>) -> String {
    match arrow {
        Some(a) => format!(" {title} {a} "),
        None => format!(" {title} "),
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

/// Same sizing as [`centered_rect_for_content`], but anchors the modal
/// near the *top* of `area` instead of vertically centring it.  Used by
/// the command palette so the input row stays put as the match list
/// grows or shrinks per keystroke — a centred palette would shift up
/// and down by half the height delta on every character typed, making
/// the input row appear to jump.
///
/// The anchor sits at one-eighth of the area height (capped at 4 rows
/// from the top to keep the offset small on tall terminals), then is
/// clamped so the modal still fits inside `area`.
#[allow(dead_code)] // used by tests in this module
pub fn top_anchored_rect_for_content(content: ContentSize, area: Rect) -> Rect {
    let (modal_width, modal_height) = modal_dimensions_for(content, area);
    let x = area.x + (area.width.saturating_sub(modal_width)) / 2;
    let desired_offset = (area.height / 8).min(4);
    let max_y = area.y + area.height.saturating_sub(modal_height);
    let y = (area.y + desired_offset).min(max_y);
    Rect {
        x,
        y,
        width: modal_width,
        height: modal_height,
    }
}

/// Compute the modal's outer width and height for a given content size
/// and available area.  Border is 2 cells (top + bottom + sides); we
/// add 1 cell of horizontal padding on each side so text doesn't kiss
/// the frame.  Inside we want `pinned_top + height + pinned_bottom`
/// rows.  Both dimensions clamp to `area`.
fn modal_dimensions_for(content: ContentSize, area: Rect) -> (u16, u16) {
    let modal_width = (content.width).saturating_add(4).min(area.width);
    let body_height = content
        .height
        .saturating_add(content.pinned_top)
        .saturating_add(content.pinned_bottom)
        .max(1);
    let modal_height = body_height.saturating_add(2).min(area.height);
    (modal_width, modal_height)
}

/// Render the Clear + bordered Block with a scroll-aware title.
/// Returns the inner rect (the area available for body + pinned
/// regions, in caller-defined order).
pub fn draw_frame(
    area: Rect,
    buf: &mut Buffer,
    title: &str,
    arrow: Option<&str>,
    theme: &Theme,
) -> Rect {
    Clear.render(area, buf);
    let title_str = format_title(title, arrow);
    // Border style overrides the surrounding modal_bg fill on the
    // frame characters only — body fill stays modal_bg.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.modal_border)
        .title(Span::styled(title_str, theme.modal_title))
        .style(theme.modal_bg);
    let inner = block.inner(area);
    block.render(area, buf);
    inner
}

/// Total wrapped row count for `lines` at `width` columns, mirroring
/// `Paragraph::wrap(Wrap { trim: false })`.  Pure — used by `ModalView`
/// to size text bodies before rendering.
pub fn wrapped_rows(lines: &[Line<'_>], width: u16) -> u16 {
    if width == 0 {
        return lines.len() as u16;
    }
    let w = width as usize;
    let mut total: u16 = 0;
    for line in lines {
        let visual = line.width();
        let rows = if visual == 0 {
            1
        } else {
            visual.div_ceil(w).max(1)
        };
        total = total.saturating_add(rows as u16);
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // ── arrow indicator ──────────────────────────────────────────────────

    #[test]
    fn arrow_omits_when_body_fits() {
        let s = ScrollContainerState {
            scroll: 0,
            last_total: 5,
            last_visible: 10,
        };
        assert_eq!(s.arrow(), None);
    }

    #[test]
    fn arrow_shows_down_at_top() {
        let s = ScrollContainerState {
            scroll: 0,
            last_total: 20,
            last_visible: 5,
        };
        assert_eq!(s.arrow(), Some("↓"));
    }

    #[test]
    fn arrow_shows_up_at_bottom() {
        let s = ScrollContainerState {
            scroll: 15,
            last_total: 20,
            last_visible: 5,
        };
        assert_eq!(s.arrow(), Some("↑"));
    }

    #[test]
    fn arrow_shows_both_in_middle() {
        let s = ScrollContainerState {
            scroll: 5,
            last_total: 20,
            last_visible: 5,
        };
        assert_eq!(s.arrow(), Some("↑↓"));
    }

    // ── format_title ─────────────────────────────────────────────────────

    #[test]
    fn format_title_omits_arrow_when_none() {
        assert_eq!(format_title("Help", None), " Help ");
    }

    #[test]
    fn format_title_appends_arrow_when_some() {
        assert_eq!(format_title("Help", Some("↓")), " Help ↓ ");
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
        assert_eq!(r.width, 34); // 30 + 4 (border + 2 padding)
        assert_eq!(r.height, 8); // 5 + 1 + 2 (border)
                                 // Centred:
        assert_eq!(r.x, (200 - 34) / 2);
        assert_eq!(r.y, (60 - 8) / 2);
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
        assert_eq!(r.height, 12); // 5 + 2 + 3 + 2 (border)
    }

    // ── top_anchored_rect_for_content ────────────────────────────────────

    #[test]
    fn top_anchored_rect_y_does_not_change_with_content_height() {
        // The whole point of top-anchoring: the y position must be the
        // same regardless of body height, so the input row at the top
        // of the modal stays put as the content grows or shrinks.
        let area = Rect::new(0, 0, 100, 40);
        let small = ContentSize {
            width: 30,
            height: 3,
            pinned_top: 2,
            pinned_bottom: 0,
        };
        let large = ContentSize {
            width: 30,
            height: 15,
            pinned_top: 2,
            pinned_bottom: 0,
        };
        let r_small = top_anchored_rect_for_content(small, area);
        let r_large = top_anchored_rect_for_content(large, area);
        assert_eq!(r_small.y, r_large.y);
    }

    #[test]
    fn top_anchored_rect_uses_capped_offset_on_tall_terminals() {
        // The desired offset is `area.height / 8` capped at 4.  At
        // height 80 the eighth would be 10 — verify the cap kicks in.
        let area = Rect::new(0, 0, 100, 80);
        let content = ContentSize {
            width: 30,
            height: 5,
            pinned_top: 2,
            pinned_bottom: 0,
        };
        let r = top_anchored_rect_for_content(content, area);
        assert_eq!(r.y, 4);
    }

    #[test]
    fn top_anchored_rect_clamps_y_when_modal_would_overflow_bottom() {
        // Tiny terminal: the modal nearly fills the area, so the
        // top-anchor offset has to retreat to keep the modal on screen.
        let area = Rect::new(0, 0, 100, 8);
        let content = ContentSize {
            width: 30,
            height: 10,
            pinned_top: 2,
            pinned_bottom: 0,
        };
        let r = top_anchored_rect_for_content(content, area);
        assert!(r.y + r.height <= area.y + area.height);
    }

    #[test]
    fn top_anchored_rect_centres_x_like_centered_variant() {
        let area = Rect::new(0, 0, 100, 40);
        let content = ContentSize {
            width: 30,
            height: 5,
            pinned_top: 2,
            pinned_bottom: 0,
        };
        let r_top = top_anchored_rect_for_content(content, area);
        let r_centred = centered_rect_for_content(content, area);
        assert_eq!(r_top.x, r_centred.x);
        assert_eq!(r_top.width, r_centred.width);
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
