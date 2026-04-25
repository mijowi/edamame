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
//!
//! ## Scrolling
//!
//! Bodies that overflow the available terminal height scroll vertically.
//! Up/Down scroll one line, PgUp/PgDn scroll a page, Home/End jump to the
//! extremes; mouse wheel events route through [`ModalState::scroll_by`] from
//! the App layer so the same modal absorbs both keyboard and mouse scroll.
//! The title bar surfaces a discoverability hint (`↑`, `↓`, `↑↓`) whenever
//! the body actually overflows.  Scrolling is a no-op when the body fits.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, StatefulWidget, Widget, Wrap},
};
use unicode_width::UnicodeWidthStr;

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
    /// Vertical scroll offset, in wrapped body rows.  `0` is the top.
    /// Clamped to `last_total - last_visible` after each render.
    pub scroll: u16,
    /// Total wrapped body height observed at the most recent render.
    /// Used by [`Self::handle_key`] / [`Self::scroll_by`] to clamp the
    /// scroll position without re-running the wrap calculation.  `0`
    /// before the first render.
    pub last_total: u16,
    /// Visible body height observed at the most recent render.  Same
    /// staleness contract as [`Self::last_total`].
    pub last_visible: u16,
}

impl ModalState {
    pub fn new() -> Self {
        Self {
            focused: 0,
            response: None,
            scroll: 0,
            last_total: 0,
            last_visible: 0,
        }
    }

    /// Largest valid `scroll` given the most-recently-rendered body
    /// dimensions.  Returns `0` when the body fits — i.e. scrolling is
    /// disabled and Up/Down become no-ops.
    pub fn max_scroll(&self) -> u16 {
        self.last_total.saturating_sub(self.last_visible)
    }

    /// Adjust scroll by `delta` rows (negative scrolls toward the top,
    /// positive toward the bottom).  Clamped at both ends so callers
    /// never need to range-check before forwarding wheel events.
    pub fn scroll_by(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        let max = self.max_scroll() as i32;
        let next = (self.scroll as i32 + delta).clamp(0, max);
        self.scroll = next as u16;
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
        // Up / Down / PgUp / PgDn / Home / End drive scroll, not button
        // focus.  They're returned as `Continue` because a scroll
        // doesn't dismiss the modal — the caller should just redraw.
        match key.code {
            KeyCode::Up => {
                self.scroll_by(-1);
                return ModalResponse::Continue;
            }
            KeyCode::Down => {
                self.scroll_by(1);
                return ModalResponse::Continue;
            }
            KeyCode::PageUp => {
                self.scroll_by(-(self.last_visible.max(1) as i32));
                return ModalResponse::Continue;
            }
            KeyCode::PageDown => {
                self.scroll_by(self.last_visible.max(1) as i32);
                return ModalResponse::Continue;
            }
            KeyCode::Home if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll = 0;
                return ModalResponse::Continue;
            }
            KeyCode::End if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll = self.max_scroll();
                return ModalResponse::Continue;
            }
            _ => {}
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

        // Compute the body region first so we can size + clamp the
        // scroll BEFORE drawing the frame (the title needs to know
        // whether to render arrow indicators).
        let inner_w = modal_area.width.saturating_sub(2);
        let inner_h = modal_area.height.saturating_sub(2);

        // Body and buttons layout: all-but-last row for body, last
        // row for buttons.  This is more forgiving than using
        // `Layout` — it behaves sensibly when the modal is too small
        // to fit everything.
        let button_row_height: u16 = 1;
        let body_height = inner_h.saturating_sub(button_row_height + 1);
        let body_inner_w = inner_w.saturating_sub(2);

        // Pre-compute the wrapped body height so we can clamp scroll
        // BEFORE rendering and decide whether the title needs an
        // arrow indicator.  `Paragraph::line_count` is unstable in
        // ratatui 0.29, so we mirror its `Wrap { trim: false }`
        // arithmetic ourselves: each source line takes
        // `ceil(visual_width / available_width)` rows, with empty
        // lines counting as one row.
        let body_lines: Vec<Line<'_>> = self.body.iter().map(|s| Line::from(s.as_str())).collect();
        let body_paragraph = Paragraph::new(body_lines)
            .wrap(Wrap { trim: false })
            .style(self.theme.status_bar);
        let total = wrapped_body_rows(self.body, body_inner_w.max(1));
        state.last_total = total;
        state.last_visible = body_height;
        let max_scroll = state.max_scroll();
        if state.scroll > max_scroll {
            state.scroll = max_scroll;
        }

        // Frame.  Title carries an arrow hint when the body
        // overflows — `↓` at the top, `↑` at the bottom, `↑↓` in the
        // middle — so users know scroll is available without having
        // to guess.
        let title = format_title(self.title, state.scroll, state.max_scroll());
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, self.theme.modal_title))
            .style(self.theme.status_bar);
        let inner = block.inner(modal_area);
        block.render(modal_area, buf);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

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
            .scroll((state.scroll, 0))
            .render(body_area, buf);

        // Button row.
        render_buttons(button_area, buf, self.buttons, state.focused, self.theme);
    }
}

/// Total wrapped row count for `body` at `width` columns, mirroring
/// `Paragraph::wrap(Wrap { trim: false })`.  Pure so the scroll-clamp
/// path doesn't depend on a real `Buffer` to ask the widget itself.
fn wrapped_body_rows(body: &[String], width: u16) -> u16 {
    if width == 0 {
        return body.len() as u16;
    }
    let w = width as usize;
    let mut total: u16 = 0;
    for line in body {
        let visual = UnicodeWidthStr::width(line.as_str());
        let rows = if visual == 0 {
            1
        } else {
            visual.div_ceil(w).max(1)
        };
        total = total.saturating_add(rows as u16);
    }
    total
}

/// Build the title string, optionally suffixed with a scroll indicator
/// when the body overflows the visible area.  Pure so it's
/// straightforward to unit-test the indicator logic without
/// rendering.
fn format_title(title: &str, scroll: u16, max_scroll: u16) -> String {
    if max_scroll == 0 {
        return format!(" {} ", title);
    }
    let arrow = match (scroll, max_scroll) {
        (0, _) => "↓",
        (s, max) if s >= max => "↑",
        _ => "↑↓",
    };
    format!(" {title} {arrow} ")
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
/// to the enclosing `area`.  When the body would exceed `area.height`, the
/// modal grows to use as much vertical space as available and the body
/// scrolls.  Width is unaffected by overflow — long lines wrap rather than
/// drive horizontal scrolling.
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

    // ── Scroll behaviour ─────────────────────────────────────────────────

    #[test]
    fn format_title_omits_arrow_when_body_fits() {
        assert_eq!(format_title("Help", 0, 0), " Help ");
    }

    #[test]
    fn format_title_shows_down_arrow_at_top() {
        assert_eq!(format_title("Help", 0, 5), " Help ↓ ");
    }

    #[test]
    fn format_title_shows_up_arrow_at_bottom() {
        assert_eq!(format_title("Help", 5, 5), " Help ↑ ");
    }

    #[test]
    fn format_title_shows_both_arrows_in_the_middle() {
        assert_eq!(format_title("Help", 2, 5), " Help ↑↓ ");
    }

    #[test]
    fn scroll_by_clamps_at_top() {
        let mut state = ModalState {
            scroll: 2,
            last_total: 10,
            last_visible: 5,
            ..ModalState::new()
        };
        state.scroll_by(-100);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn scroll_by_clamps_at_bottom() {
        let mut state = ModalState {
            last_total: 10,
            last_visible: 5,
            ..ModalState::new()
        };
        state.scroll_by(100);
        assert_eq!(state.scroll, 5); // 10 - 5
    }

    #[test]
    fn scroll_by_is_a_noop_when_body_fits() {
        let mut state = ModalState {
            last_total: 4,
            last_visible: 10,
            ..ModalState::new()
        };
        state.scroll_by(3);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn down_key_advances_scroll_one_line() {
        let mut state = ModalState {
            last_total: 20,
            last_visible: 5,
            ..ModalState::new()
        };
        let resp = state.handle_key(&key(KeyCode::Down), 1);
        assert_eq!(resp, ModalResponse::Continue);
        assert_eq!(state.scroll, 1);
    }

    #[test]
    fn page_down_jumps_by_visible_height() {
        let mut state = ModalState {
            last_total: 30,
            last_visible: 10,
            ..ModalState::new()
        };
        state.handle_key(&key(KeyCode::PageDown), 1);
        assert_eq!(state.scroll, 10);
        // Second PageDown clamps at max_scroll (30 - 10 = 20).
        state.handle_key(&key(KeyCode::PageDown), 1);
        assert_eq!(state.scroll, 20);
        state.handle_key(&key(KeyCode::PageDown), 1);
        assert_eq!(state.scroll, 20);
    }

    #[test]
    fn home_and_end_jump_to_extremes() {
        let mut state = ModalState {
            scroll: 4,
            last_total: 12,
            last_visible: 4,
            ..ModalState::new()
        };
        state.handle_key(&key(KeyCode::End), 1);
        assert_eq!(state.scroll, 8); // 12 - 4
        state.handle_key(&key(KeyCode::Home), 1);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn render_clamps_scroll_when_body_shrinks() {
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState {
            scroll: 100, // intentionally past the end
            ..ModalState::new()
        };
        // 5-row body + 4 chrome rows leaves the body fully visible
        // in a 10-row terminal, so max_scroll is 0 after render.
        let body: Vec<String> = (0..5).map(|i| format!("line {i}")).collect();
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
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn render_writes_scroll_indicator_when_body_overflows() {
        // 60×6 terminal → after borders/buttons there are only 2 body
        // rows.  Render an 8-line body so 6 rows are below the fold.
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body: Vec<String> = (0..8).map(|i| format!("body {i}")).collect();
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
        assert!(state.last_total >= state.last_visible);
    }
}
