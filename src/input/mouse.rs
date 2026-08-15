//! Mouse event parsing and dispatch.
//!
//! Translates raw `crossterm::event::MouseEvent` values into high-level
//! [`MouseAction`]s that the editor can apply.  The dispatcher tracks click
//! timing and drag state so it can surface double-click / triple-click and
//! click-drag semantics from the flat stream of terminal mouse events.
//!
//! Coordinate translation (terminal columns/rows → editor-area cells) happens
//! at dispatch time: events that fall outside the document area are dropped
//! (returning `None`), while events inside are reported in
//! document-area-relative coordinates.

use std::time::{Duration, Instant};

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

/// Maximum interval between consecutive clicks that still count as a "chord"
/// (double-click, triple-click).  400 ms matches the common X11 default.
pub const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(400);

/// Default lines scrolled per wheel tick when no `config.editor.mouse_scroll_lines`
/// override is supplied.  One-line steps are the finest granularity the terminal
/// can report and preserve the rule that scrolling does not move the
/// cursor.  Users can configure a coarser step (2 or 3) for a snappier feel.
pub const DEFAULT_WHEEL_STEP: usize = 1;

/// High-level mouse action produced by the dispatcher.
///
/// All coordinates are relative to the document area (the editor's drawable
/// region, excluding the status bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    /// Single left-button click at `(col, row)`.  Places the cursor, clears
    /// any selection, and becomes the anchor for a subsequent drag.  The
    /// `modifiers` are the crossterm `KeyModifiers` in effect during the
    /// click — used to distinguish plain clicks (cursor
    /// placement) from `Ctrl`-clicks (follow link).
    Click {
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    },
    /// Second click within `MULTI_CLICK_WINDOW` at the same cell — select the
    /// word under the cursor.
    DoubleClick {
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    },
    /// Third click within `MULTI_CLICK_WINDOW` at the same cell — select the
    /// whole line.
    TripleClick {
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    },
    /// Left-button drag: extend the selection from the anchor to `(col, row)`.
    Drag { col: u16, row: u16 },
    /// Left-button release.  Currently informational; kept so future phases
    /// can distinguish "dragging" from "settled" selections.
    Release,
    /// Wheel scroll.  Positive values scroll *down* (content moves up); the
    /// magnitude is already pre-multiplied by the dispatcher's `wheel_step`.
    Scroll(i32),
}

/// Stateful mouse dispatcher.  Owns click-counting and drag state.
pub struct MouseDispatcher {
    last_click_time: Option<Instant>,
    last_click_cell: Option<(u16, u16)>,
    click_count: u32,
    left_down: bool,
    /// Whether the gesture that is ending (or just ended) included at least
    /// one `Drag` event.  A press that turned into a drag is not part of a
    /// double-click chord — every GUI toolkit breaks the chain there, and
    /// without it a grab → drag → release → *re-grab the same cell* (the
    /// natural retry when a table row or column border didn't land where
    /// the user wanted) arrives as a `DoubleClick` and arms no drag at all.
    dragged_since_down: bool,
    /// Lines emitted per wheel tick (see `DEFAULT_WHEEL_STEP`).  Seeded from
    /// `config.editor.mouse_scroll_lines` via `with_wheel_step`.
    wheel_step: usize,
}

impl Default for MouseDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl MouseDispatcher {
    pub fn new() -> Self {
        Self::with_wheel_step(DEFAULT_WHEEL_STEP)
    }

    /// Construct a dispatcher with a caller-supplied wheel-step.  `App::new`
    /// uses this to seed the dispatcher from
    /// `config.editor.mouse_scroll_lines`.
    pub fn with_wheel_step(wheel_step: usize) -> Self {
        Self {
            last_click_time: None,
            last_click_cell: None,
            click_count: 0,
            left_down: false,
            dragged_since_down: false,
            wheel_step: wheel_step.max(1),
        }
    }

    /// Update the wheel step at runtime.  Called when the user changes
    /// `config.editor.mouse_scroll_lines` via the settings overlay so
    /// the new value takes effect without restarting the app.
    pub fn set_wheel_step(&mut self, wheel_step: usize) {
        self.wheel_step = wheel_step.max(1);
    }

    /// Current wheel step (lines per wheel tick).  Exposed for tests
    /// that verify settings-overlay live-update wiring.
    #[cfg(test)]
    pub fn wheel_step(&self) -> usize {
        self.wheel_step
    }

    /// Translate a raw mouse event into a [`MouseAction`].
    ///
    /// `doc_area` is the editor's document area in terminal coordinates.
    /// Events outside that area return `None` so the caller can ignore clicks
    /// on the status bar or on any future popup widgets.  Drag events outside
    /// the area still return `None` — selections freeze until the cursor
    /// re-enters the document area.
    pub fn dispatch(&mut self, event: MouseEvent, doc_area: Rect) -> Option<MouseAction> {
        let in_area = contains(doc_area, event.column, event.row);
        let (rel_col, rel_row) = if in_area {
            (event.column - doc_area.x, event.row - doc_area.y)
        } else {
            (0, 0)
        };

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if in_area => {
                self.left_down = true;
                let now = Instant::now();
                let same_cell = self.last_click_cell == Some((rel_col, rel_row));
                let within_threshold = self
                    .last_click_time
                    .map(|t| now.duration_since(t) <= MULTI_CLICK_WINDOW)
                    .unwrap_or(false);
                if same_cell && within_threshold && !self.dragged_since_down {
                    self.click_count = (self.click_count + 1).min(3);
                } else {
                    self.click_count = 1;
                }
                self.dragged_since_down = false;
                self.last_click_time = Some(now);
                self.last_click_cell = Some((rel_col, rel_row));
                Some(match self.click_count {
                    1 => MouseAction::Click {
                        col: rel_col,
                        row: rel_row,
                        modifiers: event.modifiers,
                    },
                    2 => MouseAction::DoubleClick {
                        col: rel_col,
                        row: rel_row,
                        modifiers: event.modifiers,
                    },
                    _ => MouseAction::TripleClick {
                        col: rel_col,
                        row: rel_row,
                        modifiers: event.modifiers,
                    },
                })
            }
            MouseEventKind::Drag(MouseButton::Left) if self.left_down => {
                // The chord-breaking flag is set even for a drag that left
                // the document area — the gesture was still a drag, whether
                // or not the editor got to see this particular step.
                self.dragged_since_down = true;
                in_area.then_some(MouseAction::Drag {
                    col: rel_col,
                    row: rel_row,
                })
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.left_down = false;
                Some(MouseAction::Release)
            }
            MouseEventKind::ScrollDown => Some(MouseAction::Scroll(self.wheel_step as i32)),
            MouseEventKind::ScrollUp => Some(MouseAction::Scroll(-(self.wheel_step as i32))),
            _ => None,
        }
    }
}

fn contains(area: Rect, col: u16, row: u16) -> bool {
    col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn up(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn drag(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn wheel_down() -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        }
    }

    #[test]
    fn single_click_is_single_click() {
        let mut d = MouseDispatcher::new();
        assert_eq!(
            d.dispatch(down(5, 2), area()),
            Some(MouseAction::Click {
                col: 5,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    #[test]
    fn click_release_then_click_same_cell_is_double() {
        let mut d = MouseDispatcher::new();
        d.dispatch(down(5, 2), area());
        d.dispatch(up(5, 2), area());
        assert_eq!(
            d.dispatch(down(5, 2), area()),
            Some(MouseAction::DoubleClick {
                col: 5,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    #[test]
    fn third_click_is_triple_click() {
        let mut d = MouseDispatcher::new();
        d.dispatch(down(5, 2), area());
        d.dispatch(up(5, 2), area());
        d.dispatch(down(5, 2), area());
        d.dispatch(up(5, 2), area());
        assert_eq!(
            d.dispatch(down(5, 2), area()),
            Some(MouseAction::TripleClick {
                col: 5,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    #[test]
    fn click_at_different_cell_resets_counter() {
        let mut d = MouseDispatcher::new();
        d.dispatch(down(5, 2), area());
        d.dispatch(up(5, 2), area());
        assert_eq!(
            d.dispatch(down(10, 4), area()),
            Some(MouseAction::Click {
                col: 10,
                row: 4,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    #[test]
    fn ctrl_modifier_is_threaded_into_click() {
        let mut d = MouseDispatcher::new();
        let ctrl_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row: 1,
            modifiers: KeyModifiers::CONTROL,
        };
        match d.dispatch(ctrl_click, area()) {
            Some(MouseAction::Click { modifiers, .. }) => {
                assert!(modifiers.contains(KeyModifiers::CONTROL));
            }
            other => panic!("expected Click, got {other:?}"),
        }
    }

    #[test]
    fn drag_without_prior_down_is_ignored() {
        let mut d = MouseDispatcher::new();
        assert_eq!(d.dispatch(drag(5, 5), area()), None);
    }

    #[test]
    fn drag_after_down_reports_drag() {
        let mut d = MouseDispatcher::new();
        d.dispatch(down(5, 2), area());
        assert_eq!(
            d.dispatch(drag(7, 3), area()),
            Some(MouseAction::Drag { col: 7, row: 3 })
        );
    }

    /// A press that turned into a drag isn't the first half of a chord, so
    /// re-grabbing the same cell right afterwards must report a fresh
    /// `Click` — otherwise the retry after an unsatisfying table-row or
    /// column-border drag silently arms nothing.
    #[test]
    fn drag_breaks_the_double_click_chord() {
        let mut d = MouseDispatcher::new();
        d.dispatch(down(5, 2), area());
        d.dispatch(drag(9, 2), area());
        d.dispatch(up(9, 2), area());
        assert_eq!(
            d.dispatch(down(5, 2), area()),
            Some(MouseAction::Click {
                col: 5,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    /// …and the drag that broke it doesn't keep breaking it: the press
    /// after the retry chords normally again.
    #[test]
    fn chord_resumes_after_a_dragless_click() {
        let mut d = MouseDispatcher::new();
        d.dispatch(down(5, 2), area());
        d.dispatch(drag(9, 2), area());
        d.dispatch(up(9, 2), area());
        d.dispatch(down(5, 2), area());
        d.dispatch(up(5, 2), area());
        assert_eq!(
            d.dispatch(down(5, 2), area()),
            Some(MouseAction::DoubleClick {
                col: 5,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    #[test]
    fn out_of_area_events_are_dropped() {
        let mut d = MouseDispatcher::new();
        let outside = Rect {
            x: 10,
            y: 10,
            width: 10,
            height: 10,
        };
        assert_eq!(d.dispatch(down(5, 2), outside), None);
    }

    #[test]
    fn scroll_down_emits_positive_step() {
        let mut d = MouseDispatcher::new();
        assert_eq!(
            d.dispatch(wheel_down(), area()),
            Some(MouseAction::Scroll(DEFAULT_WHEEL_STEP as i32))
        );
    }

    #[test]
    fn scroll_down_honours_configured_wheel_step() {
        let mut d = MouseDispatcher::with_wheel_step(3);
        assert_eq!(
            d.dispatch(wheel_down(), area()),
            Some(MouseAction::Scroll(3))
        );
    }

    #[test]
    fn wheel_step_is_clamped_to_at_least_one() {
        let mut d = MouseDispatcher::with_wheel_step(0);
        assert_eq!(
            d.dispatch(wheel_down(), area()),
            Some(MouseAction::Scroll(1))
        );
    }
}
