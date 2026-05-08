//! Reusable vertical scrollbar widget.
//!
//! A scrollbar is just three numbers (`position`, `total`, `visible`)
//! plus a 1-column `Rect` to paint into.  The pure helpers
//! [`thumb_range`], [`position_for_click`], and [`position_for_drag`]
//! convert between (position, total, visible, track height) and
//! (thumb_top, thumb_height) so both the renderer and the App-layer
//! mouse handler share the same arithmetic.
//!
//! The widget itself paints a `│` track over the parent's background
//! and a `█` thumb in either `theme.scrollbar_thumb` (dim) or
//! `theme.scrollbar_thumb_active` (bright) depending on whether the
//! user is hovering or dragging it.  Track and thumb glyphs differ in
//! shape so monochrome terminals can still tell them apart.

use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

use crate::config::Theme;
use crate::ui::scroll_container::ScrollContainerState;

/// Minimum visual height of the thumb in cells.  Without a floor a very
/// large document would shrink the thumb to a single cell that's hard
/// to see and impossible to drag accurately.
pub const MIN_THUMB: u16 = 2;

/// Layout for a rendered scrollbar — the rect it occupies plus the
/// (position, total, visible) trio that produced it.  Published by
/// [`crate::ui::EditorViewState`] each frame so the App's mouse layer
/// can hit-test the gutter without re-deriving the numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarMetrics {
    pub area: Rect,
    /// Total content rows (post-wrap visual rows for the editor;
    /// wrapped body rows for modals).
    pub total: u16,
    /// Viewport height in rows.
    pub visible: u16,
    /// Current scroll offset (clamped to `[0, total - visible]` for
    /// display purposes; the widget clamps internally as well).
    pub position: u16,
}

/// Compute (top_offset, height) of the thumb within a track of `track`
/// rows.  Returns `None` when the content fits the viewport — i.e.
/// no scrollbar should be drawn.
///
/// The thumb height is proportional to `visible / total`, floored at
/// [`MIN_THUMB`] so the thumb stays usable on long documents.  The top
/// position interpolates between 0 (at `position == 0`) and
/// `track - thumb_height` (at `position == total - visible`).
pub fn thumb_range(total: u16, visible: u16, position: u16, track: u16) -> Option<(u16, u16)> {
    if track == 0 || total == 0 || total <= visible {
        return None;
    }
    let total_u = total as u32;
    let visible_u = visible as u32;
    let track_u = track as u32;
    let raw_height = (visible_u * track_u + total_u / 2) / total_u;
    let thumb_h = (raw_height as u16).max(MIN_THUMB).min(track);
    let max_pos = total.saturating_sub(visible) as u32;
    let max_top = (track - thumb_h) as u32;
    let pos = (position as u32).min(max_pos);
    let top = if max_pos == 0 || max_top == 0 {
        0
    } else {
        ((pos * max_top) + (max_pos / 2)) / max_pos
    } as u16;
    Some((top, thumb_h))
}

/// Convert a click at row `click_row` within the track to a scroll
/// position.  Centres the thumb on the click point, then clamps.
pub fn position_for_click(total: u16, visible: u16, track: u16, click_row: u16) -> u16 {
    let Some((_, thumb_h)) = thumb_range(total, visible, 0, track) else {
        return 0;
    };
    let max_top = track.saturating_sub(thumb_h);
    if max_top == 0 {
        return 0;
    }
    let target_top = click_row.saturating_sub(thumb_h / 2).min(max_top);
    let max_pos = total.saturating_sub(visible) as u32;
    ((target_top as u32 * max_pos + (max_top as u32 / 2)) / max_top as u32) as u16
}

/// Convert a pointer at row `pointer_row` within the track to a scroll
/// position, given a thumb-grab offset (rows from thumb-top to the
/// initial click).  Used by drag updates.
pub fn position_for_drag(
    total: u16,
    visible: u16,
    track: u16,
    pointer_row: u16,
    grab_offset: u16,
) -> u16 {
    let Some((_, thumb_h)) = thumb_range(total, visible, 0, track) else {
        return 0;
    };
    let max_top = track.saturating_sub(thumb_h);
    if max_top == 0 {
        return 0;
    }
    let target_top = pointer_row.saturating_sub(grab_offset).min(max_top);
    let max_pos = total.saturating_sub(visible) as u32;
    ((target_top as u32 * max_pos + (max_top as u32 / 2)) / max_top as u32) as u16
}

/// Reserve the rightmost column of `area` for a scrollbar gutter when
/// the supplied scroll state reports any overflow.  Returns the
/// shrunken body rect plus the gutter rect (or `None` when the body
/// fits the viewport).  Used by overlays that drive a
/// [`ScrollContainerState`] — the editor has its own variant that
/// keys off `total > height` directly.
pub fn split_for_scroll_state(area: Rect, state: &ScrollContainerState) -> (Rect, Option<Rect>) {
    if state.max_scroll() == 0 || area.width < 2 || area.height == 0 {
        return (area, None);
    }
    let bar = Rect {
        x: area.x + area.width - 1,
        y: area.y,
        width: 1,
        height: area.height,
    };
    let body = Rect {
        width: area.width - 1,
        ..area
    };
    (body, Some(bar))
}

/// Render the [`Scrollbar`] widget at `bar_area` driven by a
/// [`ScrollContainerState`].  The state's `last_total` / `last_visible`
/// must already reflect the post-observe layout — the typical caller
/// is an overlay's `render` method, immediately after `observe`.
pub fn render_for_scroll_state(
    bar_area: Rect,
    state: &ScrollContainerState,
    theme: &Theme,
    buf: &mut Buffer,
) {
    let metrics = ScrollbarMetrics {
        area: bar_area,
        total: state.last_total,
        visible: state.last_visible,
        position: state.scroll,
    };
    Scrollbar {
        metrics,
        theme,
        active: false,
    }
    .render(bar_area, buf);
}

/// Vertical scrollbar widget.  Pass `active = true` to use the bright
/// thumb style (used while the user is hovering the gutter or dragging
/// the thumb).
pub struct Scrollbar<'a> {
    pub metrics: ScrollbarMetrics,
    pub theme: &'a Theme,
    pub active: bool,
}

impl<'a> Widget for Scrollbar<'a> {
    fn render(self, _area: Rect, buf: &mut Buffer) {
        let area = self.metrics.area;
        if area.width == 0 || area.height == 0 {
            return;
        }
        let track = area.height;
        let track_style = self.theme.scrollbar_track;
        for y in 0..track {
            let cell = &mut buf[(area.x, area.y + y)];
            cell.set_symbol("│").set_style(track_style);
        }
        if let Some((top, thumb_h)) = thumb_range(
            self.metrics.total,
            self.metrics.visible,
            self.metrics.position,
            track,
        ) {
            let thumb_style = if self.active {
                self.theme.scrollbar_thumb_active
            } else {
                self.theme.scrollbar_thumb
            };
            for y in top..(top + thumb_h).min(track) {
                let cell = &mut buf[(area.x, area.y + y)];
                cell.set_symbol("█").set_style(thumb_style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_thumb_when_content_fits() {
        assert_eq!(thumb_range(10, 10, 0, 20), None);
        assert_eq!(thumb_range(5, 10, 0, 20), None);
        assert_eq!(thumb_range(0, 0, 0, 20), None);
    }

    #[test]
    fn no_thumb_with_zero_track() {
        assert_eq!(thumb_range(100, 10, 0, 0), None);
    }

    #[test]
    fn thumb_at_top_when_position_zero() {
        let (top, _) = thumb_range(100, 10, 0, 20).unwrap();
        assert_eq!(top, 0);
    }

    #[test]
    fn thumb_at_bottom_when_position_max() {
        // max position = total - visible = 90.
        let (top, h) = thumb_range(100, 10, 90, 20).unwrap();
        assert_eq!(top + h, 20);
    }

    #[test]
    fn thumb_height_scales_with_visible_over_total() {
        // 50 visible of 100 total in a 20-cell track → ~10 cells.
        let (_, h) = thumb_range(100, 50, 0, 20).unwrap();
        assert_eq!(h, 10);
    }

    #[test]
    fn thumb_floored_at_minimum() {
        let (_, h) = thumb_range(10000, 1, 0, 20).unwrap();
        assert!(h >= MIN_THUMB);
    }

    #[test]
    fn thumb_height_capped_at_track() {
        let (top, h) = thumb_range(11, 10, 0, 5).unwrap();
        assert!(h <= 5);
        assert_eq!(top, 0);
    }

    #[test]
    fn position_for_click_centres_thumb() {
        // 100 total, 10 visible, 20-cell track → thumb_h = 2, max_top = 18.
        // Click at row 9 → target_top = 8 → scroll ~= 8/18 * 90 = 40.
        let p = position_for_click(100, 10, 20, 9);
        assert!((38..=42).contains(&p), "got {p}");
    }

    #[test]
    fn position_for_click_clamps_within_track() {
        // Click way past the bottom should land at max scroll.
        let p = position_for_click(100, 10, 20, 100);
        assert_eq!(p, 90);
        // Click at row 0 → top.
        let p = position_for_click(100, 10, 20, 0);
        assert_eq!(p, 0);
    }

    #[test]
    fn position_for_drag_uses_grab_offset() {
        // Same track but the user grabbed the thumb 1 cell from its top.
        // Pointer at row 9 means thumb_top = 8 → ~scroll 40.
        let p = position_for_drag(100, 10, 20, 9, 1);
        assert!((38..=42).contains(&p), "got {p}");
    }

    #[test]
    fn position_for_click_with_no_overflow_returns_zero() {
        assert_eq!(position_for_click(5, 10, 20, 5), 0);
    }
}
