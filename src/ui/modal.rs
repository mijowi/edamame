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
//! Future phases will reuse this for the settings panel and confirm dialogs.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, StatefulWidget, Widget, Wrap},
};

use crate::config::Theme;

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

/// Mutable state for a modal: which button has focus, and whether the modal
/// is still open.  The caller owns the lifetime: construct when the modal
/// opens, discard when `Open::Closed` is observed.
#[derive(Debug, Clone)]
pub struct ModalState {
    pub focused: usize,
    /// Set by `handle_key` once the user activates a button or cancels.
    pub response: Option<ModalResponse>,
}

impl ModalState {
    pub fn new() -> Self {
        Self {
            focused: 0,
            response: None,
        }
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

impl Default for ModalState {
    fn default() -> Self {
        Self::new()
    }
}

/// The modal widget.  Renders on top of whatever the underlying view drew
/// (callers should draw the editor first, then the modal).
pub struct ModalView<'a> {
    pub title: &'a str,
    pub body: &'a [String],
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
        let modal_area = centered_rect(self.body, self.buttons, area);
        // Clear background beneath the modal so underlying content doesn't
        // bleed through.
        Clear.render(modal_area, buf);

        // Frame.
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                format!(" {} ", self.title),
                self.theme.modal_title,
            ))
            .style(self.theme.status_bar);
        let inner = block.inner(modal_area);
        block.render(modal_area, buf);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Body and buttons layout: all-but-last row for body, last row for
        // buttons.  This is more forgiving than using `Layout` since it
        // behaves sensibly when the modal is too small to fit everything.
        let button_row_height: u16 = 1;
        let body_height = inner.height.saturating_sub(button_row_height + 1);
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

        // Body text.
        let body_lines: Vec<Line<'_>> = self.body.iter().map(|s| Line::from(s.as_str())).collect();
        Paragraph::new(body_lines)
            .wrap(Wrap { trim: false })
            .style(self.theme.status_bar)
            .render(body_area, buf);

        // Button row.
        render_buttons(button_area, buf, self.buttons, state.focused, self.theme);
    }
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

/// Compute a centred rectangle sized to fit the body and button row, clamped
/// to the enclosing `area`.
fn centered_rect(body: &[String], buttons: &[ModalButton], area: Rect) -> Rect {
    let body_width = body.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let button_width: usize = buttons
        .iter()
        .map(|b| b.label.chars().count() + 4) // "[ label ]"
        .sum::<usize>()
        + buttons.len().saturating_sub(1) * 2; // "  " separators
    let content_width = body_width.max(button_width);
    // +4 for the border + 1 col padding on each side.
    let modal_width = (content_width as u16).saturating_add(4).min(area.width);
    // Body height is the sum of body lines (each stored as a single line —
    // wrapping inside the modal itself will happen via `Wrap::trim=false`,
    // but we still size based on the raw line count plus a one-row buffer).
    let body_height = (body.len() as u16).max(1);
    // +2 for borders, +1 for button row, +1 for spacer between body and buttons.
    let modal_height = body_height.saturating_add(4).min(area.height);
    let x = area.x + (area.width.saturating_sub(modal_width)) / 2;
    let y = area.y + (area.height.saturating_sub(modal_height)) / 2;
    Rect {
        x,
        y,
        width: modal_width,
        height: modal_height,
    }
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
        let body = vec!["Hello.".to_owned(), "World.".to_owned()];
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
}
