//! A reusable modal popup widget.
//!
//! Draws a centred box with an optional title, body text lines, and a row of
//! buttons at the bottom.  One button is focused at a time; Left/Right or
//! Tab/Shift-Tab cycle the focus, Enter activates the focused button, and
//! Escape activates the "cancel" button if one is configured (by convention,
//! the first button).
//!
//! The widget is deliberately UI-only: it returns a `ModalResponse` indicating
//! which button the user pressed, and the caller handles the consequences.
//!
//! ## Scrolling
//!
//! Bodies that overflow the available terminal height scroll vertically.
//! Up/Down scroll one line, PgUp/PgDn scroll a page, Home/End jump to the
//! extremes; mouse wheel events route through [`ModalState::scroll_by`] from
//! the App layer so the same modal absorbs both keyboard and mouse scroll.
//! The title bar surfaces a discoverability hint (`↑`, `↓`, `↑↓`) whenever
//! the body actually overflows.  Scrolling is a no-op when the body fits.
//!
//! Scroll arithmetic, frame rendering, content-aware sizing, and the
//! arrow indicator all live in [`crate::ui::scroll_container`] so the
//! Phase 10 overlays (palette, settings, keybinds) share the same
//! mechanics — see that module for the underlying primitives.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget, Wrap},
};

use crate::config::Theme;
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, wrapped_rows, ContentSize, ScrollContainerState,
};

/// The outcome of a key event handed to the modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalResponse {
    /// No terminal change — the modal remains open and just re-renders.
    Continue,
    /// The user activated the button at this index.
    ButtonPressed(usize),
    /// The user dismissed with Escape without activating a specific button.
    Cancelled,
}

/// A single modal button (label only; actions live on the caller side).
#[derive(Debug, Clone)]
pub struct ModalButton {
    pub label: String,
}

impl ModalButton {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

/// Mutable state for a modal: which button has focus, whether the modal
/// has resolved, and embedded scroll bookkeeping shared with the
/// `scroll_container` module.
#[derive(Debug, Clone, Default)]
pub struct ModalState {
    pub focused: usize,
    /// Set by `handle_key` once the user activates a button or cancels.
    pub response: Option<ModalResponse>,
    pub scroll_state: ScrollContainerState,
}

impl ModalState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adjust scroll by `delta` rows.  Clamped at both ends.  Used by
    /// the App layer's mouse-wheel router; the keyboard scroll path
    /// goes through `scroll_state.handle_scroll_key` directly.
    pub fn scroll_by(&mut self, delta: i32) {
        self.scroll_state.scroll_by(delta);
    }

    /// Update focus/response in response to a key event.
    ///
    /// Returns the response so callers can branch on it immediately.  The
    /// response is also cached on `self.response` for the convenience of
    /// code that separates handle-event from act-on-result across frames.
    pub fn handle_key(
        &mut self,
        key: &crossterm::event::KeyEvent,
        num_buttons: usize,
    ) -> ModalResponse {
        use crossterm::event::{KeyCode, KeyModifiers};
        if num_buttons == 0 {
            return ModalResponse::Continue;
        }
        // Up/Down/PgUp/PgDn/Home/End drive scroll, not button focus.
        // They're returned as `Continue` because a scroll doesn't dismiss
        // the modal — the caller should just redraw.
        if self.scroll_state.handle_scroll_key(key) {
            return ModalResponse::Continue;
        }
        let response = match key.code {
            KeyCode::Left | KeyCode::BackTab => {
                if self.focused == 0 {
                    self.focused = num_buttons - 1;
                } else {
                    self.focused -= 1;
                }
                ModalResponse::Continue
            }
            KeyCode::Right | KeyCode::Tab => {
                self.focused = (self.focused + 1) % num_buttons;
                ModalResponse::Continue
            }
            KeyCode::Enter | KeyCode::Char(' ') => ModalResponse::ButtonPressed(self.focused),
            KeyCode::Esc => ModalResponse::Cancelled,
            // Treat `n`/`y` as shortcuts for cancel/primary when the user is
            // used to those bindings, but only in the absence of modifiers
            // so text editors embedding the modal don't hijack letters.
            KeyCode::Char('n') | KeyCode::Char('N') if key.modifiers == KeyModifiers::NONE => {
                ModalResponse::Cancelled
            }
            KeyCode::Char('y') | KeyCode::Char('Y') if key.modifiers == KeyModifiers::NONE => {
                ModalResponse::ButtonPressed(self.focused)
            }
            _ => ModalResponse::Continue,
        };
        if !matches!(response, ModalResponse::Continue) {
            self.response = Some(response.clone());
        }
        response
    }
}

/// The modal widget.  Renders on top of whatever the underlying view drew
/// (callers should draw the editor first, then the modal).
///
/// `body` is a slice of styled `Line`s — callers wrap plain strings as
/// `Line::raw(s)` and use `Line::from(vec![Span::styled(...), ...])` when
/// they need theme-driven colour or emphasis (e.g. the Markdown cheat
/// sheet, which mirrors preview-mode styling on top of the raw syntax).
pub struct ModalView<'a> {
    pub title: &'a str,
    pub body: &'a [Line<'a>],
    pub buttons: &'a [ModalButton],
    pub theme: &'a Theme,
}

impl<'a> StatefulWidget for ModalView<'a> {
    type State = ModalState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if self.buttons.is_empty() {
            // A button-less modal would be a deadend (no way to dismiss);
            // callers that want that should use a Paragraph instead.
            return;
        }
        let body_width = self.body.iter().map(|l| l.width()).max().unwrap_or(0) as u16;
        let button_width = button_row_width(self.buttons);
        let content_width = body_width.max(button_width);
        // Pinned bottom: 1 spacer + 1 button row.
        let content = ContentSize {
            width: content_width,
            height: self.body.len() as u16,
            pinned_top: 0,
            pinned_bottom: 2,
        };
        let modal_area = centered_rect_for_content(content, area);

        // Pre-compute the inner dimensions and wrapped body height so we
        // know the post-observe scroll bounds before drawing the frame —
        // the title's arrow indicator depends on them.
        let inner_w = modal_area.width.saturating_sub(2);
        let inner_h = modal_area.height.saturating_sub(2);
        let button_row_height: u16 = 1;
        let body_height = inner_h.saturating_sub(button_row_height + 1);
        let body_inner_w = inner_w.saturating_sub(2);
        let total = wrapped_rows(self.body, body_inner_w.max(1));
        state.scroll_state.observe(total, body_height);

        let inner = draw_frame(
            modal_area,
            buf,
            self.title,
            state.scroll_state.arrow(),
            self.theme,
        );
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let body_paragraph = Paragraph::new(self.body.to_vec())
            .wrap(Wrap { trim: false })
            .style(self.theme.status_bar);

        let body_area = Rect {
            x: inner.x + 1,
            y: inner.y,
            width: inner.width.saturating_sub(2),
            height: body_height,
        };
        let button_area = Rect {
            x: inner.x,
            y: inner.y + inner.height - button_row_height,
            width: inner.width,
            height: button_row_height,
        };

        body_paragraph
            .scroll((state.scroll_state.scroll, 0))
            .render(body_area, buf);
        render_buttons(button_area, buf, self.buttons, state.focused, self.theme);
    }
}

/// Width in columns of the rendered button row: each button is `[ label ]`
/// (label + 4 frame chars), separated by 2-space gaps.
fn button_row_width(buttons: &[ModalButton]) -> u16 {
    let labels: usize = buttons.iter().map(|b| b.label.chars().count() + 4).sum();
    let gaps = buttons.len().saturating_sub(1) * 2;
    (labels + gaps) as u16
}

/// Render the button row, horizontally centred, with the focused button
/// drawn in reverse video.
fn render_buttons(
    area: Rect,
    buf: &mut Buffer,
    buttons: &[ModalButton],
    focused: usize,
    theme: &Theme,
) {
    let mut spans: Vec<Span<'_>> = Vec::with_capacity(buttons.len() * 2 + 1);
    for (i, b) in buttons.iter().enumerate() {
        let label = format!(" {} ", b.label);
        let style = if i == focused {
            theme.modal_button_focused
        } else {
            theme.status_info
        };
        spans.push(Span::styled(format!("[{label}]"), style));
        if i + 1 < buttons.len() {
            spans.push(Span::raw("  "));
        }
    }
    Paragraph::new(Line::from(spans))
        .alignment(Alignment::Center)
        .style(theme.status_bar)
        .render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn state_with_scroll(scroll: u16, total: u16, visible: u16) -> ModalState {
        ModalState {
            scroll_state: ScrollContainerState {
                scroll,
                last_total: total,
                last_visible: visible,
            },
            ..ModalState::new()
        }
    }

    #[test]
    fn tab_cycles_focus_forward() {
        let mut state = ModalState::new();
        assert_eq!(state.focused, 0);
        state.handle_key(&key(KeyCode::Tab), 3);
        assert_eq!(state.focused, 1);
        state.handle_key(&key(KeyCode::Tab), 3);
        assert_eq!(state.focused, 2);
        state.handle_key(&key(KeyCode::Tab), 3);
        assert_eq!(state.focused, 0); // wraps
    }

    #[test]
    fn left_cycles_focus_backward_with_wrap() {
        let mut state = ModalState::new();
        state.handle_key(&key(KeyCode::Left), 3);
        assert_eq!(state.focused, 2);
        state.handle_key(&key(KeyCode::Left), 3);
        assert_eq!(state.focused, 1);
    }

    #[test]
    fn enter_activates_focused_button() {
        let mut state = ModalState::new();
        state.focused = 1;
        let response = state.handle_key(&key(KeyCode::Enter), 2);
        assert_eq!(response, ModalResponse::ButtonPressed(1));
        assert_eq!(state.response, Some(ModalResponse::ButtonPressed(1)));
    }

    #[test]
    fn escape_cancels() {
        let mut state = ModalState::new();
        let response = state.handle_key(&key(KeyCode::Esc), 2);
        assert_eq!(response, ModalResponse::Cancelled);
    }

    #[test]
    fn render_draws_title_body_and_buttons() {
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body = vec![Line::raw("Hello."), Line::raw("World.")];
        let buttons = vec![ModalButton::new("Ok"), ModalButton::new("Cancel")];
        terminal
            .draw(|frame| {
                let m = ModalView {
                    title: "Notice",
                    body: &body,
                    buttons: &buttons,
                    theme: theme(),
                };
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();

        let contents: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(contents.contains("Notice"), "title missing: {contents}");
        assert!(contents.contains("Hello."), "body missing: {contents}");
        assert!(contents.contains("Ok"), "button missing: {contents}");
        assert!(contents.contains("Cancel"), "button missing: {contents}");
    }

    // ── Scroll behaviour ─────────────────────────────────────────────────

    #[test]
    fn scroll_by_clamps_at_top() {
        let mut state = state_with_scroll(2, 10, 5);
        state.scroll_by(-100);
        assert_eq!(state.scroll_state.scroll, 0);
    }

    #[test]
    fn scroll_by_clamps_at_bottom() {
        let mut state = state_with_scroll(0, 10, 5);
        state.scroll_by(100);
        assert_eq!(state.scroll_state.scroll, 5); // 10 - 5
    }

    #[test]
    fn scroll_by_is_a_noop_when_body_fits() {
        let mut state = state_with_scroll(0, 4, 10);
        state.scroll_by(3);
        assert_eq!(state.scroll_state.scroll, 0);
    }

    #[test]
    fn down_key_advances_scroll_one_line() {
        let mut state = state_with_scroll(0, 20, 5);
        let resp = state.handle_key(&key(KeyCode::Down), 1);
        assert_eq!(resp, ModalResponse::Continue);
        assert_eq!(state.scroll_state.scroll, 1);
    }

    #[test]
    fn page_down_jumps_by_visible_height() {
        let mut state = state_with_scroll(0, 30, 10);
        state.handle_key(&key(KeyCode::PageDown), 1);
        assert_eq!(state.scroll_state.scroll, 10);
        // Second PageDown clamps at max_scroll (30 - 10 = 20).
        state.handle_key(&key(KeyCode::PageDown), 1);
        assert_eq!(state.scroll_state.scroll, 20);
        state.handle_key(&key(KeyCode::PageDown), 1);
        assert_eq!(state.scroll_state.scroll, 20);
    }

    #[test]
    fn home_and_end_jump_to_extremes() {
        let mut state = state_with_scroll(4, 12, 4);
        state.handle_key(&key(KeyCode::End), 1);
        assert_eq!(state.scroll_state.scroll, 8); // 12 - 4
        state.handle_key(&key(KeyCode::Home), 1);
        assert_eq!(state.scroll_state.scroll, 0);
    }

    #[test]
    fn render_clamps_scroll_when_body_shrinks() {
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState {
            scroll_state: ScrollContainerState {
                scroll: 100, // intentionally past the end
                ..ScrollContainerState::default()
            },
            ..ModalState::new()
        };
        // 5-row body + chrome rows leaves the body fully visible, so
        // max_scroll is 0 after render.
        let body: Vec<Line<'_>> = (0..5).map(|i| Line::raw(format!("line {i}"))).collect();
        let buttons = vec![ModalButton::new("Ok")];
        terminal
            .draw(|frame| {
                let m = ModalView {
                    title: "Notice",
                    body: &body,
                    buttons: &buttons,
                    theme: theme(),
                };
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        assert_eq!(state.scroll_state.scroll, 0);
    }

    #[test]
    fn render_writes_scroll_indicator_when_body_overflows() {
        // 60×6 terminal → after borders/buttons there are only 2 body
        // rows.  Render an 8-line body so 6 rows are below the fold.
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body: Vec<Line<'_>> = (0..8).map(|i| Line::raw(format!("body {i}"))).collect();
        let buttons = vec![ModalButton::new("Ok")];
        terminal
            .draw(|frame| {
                let m = ModalView {
                    title: "Tall",
                    body: &body,
                    buttons: &buttons,
                    theme: theme(),
                };
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();

        let contents: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            contents.contains("Tall ↓"),
            "expected scroll-down arrow in title, got: {contents}"
        );
        assert!(state.scroll_state.last_total >= state.scroll_state.last_visible);
    }
}
