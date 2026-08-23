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
//! A narrow [`crate::ui::scrollbar`] is painted beside the body whenever it
//! actually overflows.  Scrolling is a no-op when the body fits.
//!
//! Scroll arithmetic, frame rendering, and content-aware sizing all
//! live in [`crate::ui::scroll_container`] so the
//! overlays (palette, settings, keybinds) share the same
//! mechanics — see that module for the underlying primitives.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, StatefulWidget, Widget, Wrap},
};

use crate::config::Theme;
use crate::ui::button_row::{button_rows_height, buttons_row_width, render_buttons, Button};
use crate::ui::modal_links::{link_rects, wrap_rows, ModalLink, WrappedRow};
use crate::ui::scroll_container::{
    centered_rect_for_content, compute_pad_h, draw_frame, wrapped_rows, ContentSize, FrameOpts,
    ModalKind, ScrollContainerState, MAX_PAD_H, VERTICAL_CHROME_ROWS,
};
use crate::ui::scrollbar;

/// True when `(col, row)` falls inside `r`.  Shared by the footer-button
/// and `esc`-hint hit-tests in [`ModalState`].
fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

/// [`rect_contains`] over an optional rect — `None` never matches.
fn rect_contains_opt(r: Option<Rect>, col: u16, row: u16) -> bool {
    r.is_some_and(|r| rect_contains(r, col, row))
}

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

/// A [`ModalResponse`] widened with the one outcome a link-bearing
/// modal can produce that a button-only one cannot.
///
/// Deliberately a *parallel* type rather than a new `ModalResponse`
/// variant: every `resolve()` in `app::modal` matches `ModalResponse`
/// exhaustively without a wildcard, so a fourth variant would be a
/// compile error in twenty-two files that will never carry a link.
/// Wrapping instead keeps `ModalResponse` — and all of them —
/// untouched, and the modals that do opt in unwrap `Modal(..)` back
/// into their existing `resolve`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkableResponse {
    /// Everything a button-only modal could already produce.
    Modal(ModalResponse),
    /// The user activated the body link at this index into the
    /// modal's own `[ModalLink]` slice.
    Link(usize),
}

/// A single modal button (label only; actions live on the caller side).
/// Rendered wrapped in `[ … ]` via [`Button`].
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
    /// Absolute terminal rect of the rendered `esc` close hint.  Set
    /// each render when the modal is dismissable; passed to
    /// [`crate::app::modal::types::close_if_esc_clicked`] for click hit-testing.
    pub esc_button_rect: Option<Rect>,
    /// Absolute terminal rect of each footer button, in button order.
    /// Set every render so modals that want clickable buttons can
    /// hit-test a mouse click without re-deriving the centred layout.
    /// Empty when the modal has no buttons.
    pub button_rects: Vec<Rect>,
    /// `Some(i)` when body link `i` holds focus instead of a button.
    /// `focused` keeps its meaning throughout — it is always the
    /// focused *button* index — so every existing modal reads it
    /// exactly as before and only link-bearing modals consult this.
    pub(crate) focused_link: Option<usize>,
    /// `(link index, rect)` for every link row visible after scrolling,
    /// refreshed each render.  One entry per *row* a link occupies, so
    /// a link wrapped across two rows appears twice — the same model
    /// [`crate::ui::link_view`] uses in the editor.  Always empty for a
    /// modal that declares no links, which is what lets the button-only
    /// hit-test path below stay literally unchanged.
    pub(crate) link_rects: Vec<(usize, Rect)>,
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

    /// Index of the footer button whose last-rendered rect contains
    /// `(col, row)`, or `None` if the click misses every button (or no
    /// button has been rendered yet).  `button_rects` is refreshed each
    /// render in button order, so the returned index lines up with the
    /// `ModalButton` slice and with `ModalResponse::ButtonPressed`.
    pub fn button_at(&self, col: u16, row: u16) -> Option<usize> {
        self.button_rects
            .iter()
            .position(|r| rect_contains(*r, col, row))
    }

    /// Index of the body link whose last-rendered rect contains
    /// `(col, row)`, or `None`.  Only rects for rows inside the
    /// scrolled window are recorded, so a link scrolled out of view
    /// cannot be hit.
    pub(crate) fn link_at(&self, col: u16, row: u16) -> Option<usize> {
        self.link_rects
            .iter()
            .find(|(_, r)| rect_contains(*r, col, row))
            .map(|(i, _)| *i)
    }

    /// [`Self::handle_click`] widened with body links, which take
    /// priority over the footer: a link is *inside* the body, so no
    /// button or `esc` rect can overlap it, and checking it first keeps
    /// the ordering obvious rather than incidental.
    pub(crate) fn handle_click_linkable(
        &self,
        col: u16,
        row: u16,
        dismissable: bool,
    ) -> LinkableResponse {
        match self.link_at(col, row) {
            Some(i) => LinkableResponse::Link(i),
            None => LinkableResponse::Modal(self.handle_click(col, row, dismissable)),
        }
    }

    /// Translate a left-click at `(col, row)` into the same
    /// [`ModalResponse`] the keyboard path produces, so mouse and
    /// keyboard resolve a modal identically.  A footer button takes
    /// priority; failing that, the `esc` close affordance cancels (only
    /// when `dismissable`); otherwise the click is a no-op `Continue`.
    ///
    /// This is the single definition of footer-button click hit-testing
    /// — every `ModalView`-backed modal routes through it, so buttons
    /// are clickable everywhere without each modal re-deriving the
    /// centred layout.
    pub fn handle_click(&self, col: u16, row: u16, dismissable: bool) -> ModalResponse {
        if let Some(idx) = self.button_at(col, row) {
            return ModalResponse::ButtonPressed(idx);
        }
        if dismissable && rect_contains_opt(self.esc_button_rect, col, row) {
            return ModalResponse::Cancelled;
        }
        ModalResponse::Continue
    }

    /// Update focus/response in response to a key event.
    ///
    /// Returns the response so callers can branch on it immediately.  The
    /// response is also cached on `self.response` for the convenience of
    /// code that separates handle-event from act-on-result across frames.
    ///
    /// `dismissable` gates the `Esc` and `n`/`N` shortcuts: when false,
    /// the user must activate a footer button to resolve the modal.
    pub fn handle_key(
        &mut self,
        key: &crossterm::event::KeyEvent,
        num_buttons: usize,
        dismissable: bool,
    ) -> ModalResponse {
        match self.handle_key_linkable(key, 0, num_buttons, dismissable) {
            LinkableResponse::Modal(r) => r,
            // Unreachable with `num_links == 0`: the ring has no link
            // positions to focus and `link_at` finds nothing, so
            // `Link` is never constructed.
            LinkableResponse::Link(_) => ModalResponse::Continue,
        }
    }

    /// The real key handler, shared by [`Self::handle_key`] and the
    /// link-aware path.
    ///
    /// Tab / Left / Right walk **one ring over links then buttons**, so
    /// a body link is reachable without a mouse — which matters here
    /// because mouse support is capability-gated and simply absent in
    /// some terminals.  Enter activates whichever the ring is parked
    /// on.  With `num_links == 0` every branch below collapses to the
    /// button-only behavior this method replaced, which is why the
    /// twenty-odd modals that never declare a link need no changes and
    /// their existing tests still pin the same behavior.
    pub(crate) fn handle_key_linkable(
        &mut self,
        key: &crossterm::event::KeyEvent,
        num_links: usize,
        num_buttons: usize,
        dismissable: bool,
    ) -> LinkableResponse {
        use crossterm::event::{KeyCode, KeyModifiers};
        // Up/Down/PgUp/PgDn/Home/End drive scroll, not button focus.
        // They're returned as `Continue` because a scroll doesn't dismiss
        // the modal — the caller should just redraw.  Scroll runs even
        // for button-less modals so a future text-only modal can still
        // page through its body.
        if self.scroll_state.handle_scroll_key(key) {
            return LinkableResponse::Modal(ModalResponse::Continue);
        }
        let has_buttons = num_buttons > 0;
        let ring_len = num_links + num_buttons;
        // Ring positions `0..num_links` are links; the rest are
        // buttons, offset by `num_links`.
        //
        // `None` means the ring holds no focus *at all*, which is the
        // resting state of a modal with links and no buttons — the
        // config-warning modal's exact shape.  There is no button to
        // rest on there, so seeding the position as if there were one
        // put focus on a phantom button: Enter reported
        // `ButtonPressed(0)` against an empty button list, and with two
        // or more links the first Tab landed on link 1.
        let pos: Option<usize> = match self.focused_link {
            Some(i) => Some(i),
            None => has_buttons.then_some(num_links + self.focused),
        };
        let response = match key.code {
            KeyCode::Left | KeyCode::BackTab if ring_len > 0 => {
                // Unfocused, stepping backwards enters at the far end.
                let next = pos.map_or(ring_len - 1, |p| (p + ring_len - 1) % ring_len);
                self.set_ring_pos(next, num_links);
                ModalResponse::Continue
            }
            KeyCode::Right | KeyCode::Tab if ring_len > 0 => {
                let next = pos.map_or(0, |p| (p + 1) % ring_len);
                self.set_ring_pos(next, num_links);
                ModalResponse::Continue
            }
            // A focused link swallows Enter; otherwise it presses the
            // focused button, and with no buttons there is nothing to
            // press — the pre-link behavior for a button-less modal,
            // where only Esc resolves it.
            KeyCode::Enter | KeyCode::Char(' ') if ring_len > 0 => {
                if let Some(i) = self.focused_link {
                    return LinkableResponse::Link(i);
                }
                if !has_buttons {
                    return LinkableResponse::Modal(ModalResponse::Continue);
                }
                ModalResponse::ButtonPressed(self.focused)
            }
            KeyCode::Esc if dismissable => ModalResponse::Cancelled,
            // Treat `n`/`y` as shortcuts for cancel/primary when the user is
            // used to those bindings, but only in the absence of modifiers
            // so text editors embedding the modal don't hijack letters.
            KeyCode::Char('n') | KeyCode::Char('N')
                if dismissable && key.modifiers == KeyModifiers::NONE =>
            {
                ModalResponse::Cancelled
            }
            // `y` stays a shortcut for the focused *button* only.  With
            // a link focused it would otherwise fire whatever button the
            // ring last sat on, which is a different action than the one
            // the user is looking at.
            KeyCode::Char('y') | KeyCode::Char('Y')
                if has_buttons
                    && self.focused_link.is_none()
                    && key.modifiers == KeyModifiers::NONE =>
            {
                ModalResponse::ButtonPressed(self.focused)
            }
            _ => ModalResponse::Continue,
        };
        if !matches!(response, ModalResponse::Continue) {
            self.response = Some(response.clone());
        }
        LinkableResponse::Modal(response)
    }

    /// Park the focus ring on `pos`, splitting it back into the
    /// `focused_link` / `focused` pair the render path reads.
    ///
    /// `focused` is left alone while a link holds focus so Tabbing
    /// through the links and back onto the buttons returns to the
    /// button the user started from, rather than resetting to zero.
    fn set_ring_pos(&mut self, pos: usize, num_links: usize) {
        if pos < num_links {
            self.focused_link = Some(pos);
        } else {
            self.focused_link = None;
            self.focused = pos - num_links;
        }
    }
}

/// The modal widget.  Renders on top of whatever the underlying view drew
/// (callers should draw the editor first, then the modal).
///
/// `body` is a slice of styled `Line`s — callers wrap plain strings as
/// `Line::raw(s)` and use `Line::from(vec![Span::styled(...), ...])` when
/// they need theme-driven color or emphasis (e.g. the Markdown cheat
/// sheet, which mirrors preview-mode styling on top of the raw syntax).
pub struct ModalView<'a> {
    pub title: &'a str,
    pub body: &'a [Line<'a>],
    pub buttons: &'a [ModalButton],
    pub theme: &'a Theme,
    /// Visual urgency — drives title color.
    pub kind: ModalKind,
    /// When false, no `esc` close hint is rendered and the widget's
    /// `handle_key` ignores `Esc`.  Set to false on modals that gate
    /// the user on an explicit choice (warnings, errors).
    pub dismissable: bool,
    /// Maximum horizontal padding per side, in cells.  Defaults to
    /// [`MAX_PAD_H`] via [`ModalView::new`]; raise it with
    /// [`ModalView::with_max_pad_h`] for modals whose content reads
    /// cramped at the default.
    pub max_pad_h: u16,
    /// Optional cap on the body's *content* width in columns, before
    /// padding.  `None` (the default) sizes the modal to its longest
    /// body line, which is right for tabular content but wrong for
    /// prose: a one-paragraph body's natural width is the paragraph's
    /// whole unwrapped length, so the modal stretches to fill the
    /// terminal.  Set it with [`ModalView::with_max_content_width`].
    ///
    /// Never narrows the modal below its button row — see the clamp in
    /// `render`.
    pub max_content_w: Option<u16>,
    /// Inline hyperlinks in `body`, declared by coordinate — see
    /// [`crate::ui::modal_links`].  Empty by default, and empty is the
    /// path every pre-existing modal takes: it renders through
    /// `Paragraph`'s own `Wrap` exactly as before.  A non-empty slice
    /// switches `render` onto the pre-wrapped path, which is the only
    /// way to know where a span landed.
    pub(crate) links: &'a [ModalLink],
}

impl<'a> ModalView<'a> {
    /// Construct a `ModalView` with the default maximum horizontal
    /// padding ([`MAX_PAD_H`]).  Use the field-by-field struct literal
    /// only inside the module's own tests; production callers should go
    /// through this constructor so a future default change picks them
    /// up automatically.
    pub fn new(
        title: &'a str,
        body: &'a [Line<'a>],
        buttons: &'a [ModalButton],
        theme: &'a Theme,
        kind: ModalKind,
        dismissable: bool,
    ) -> Self {
        Self {
            title,
            body,
            buttons,
            theme,
            kind,
            dismissable,
            max_pad_h: MAX_PAD_H,
            max_content_w: None,
            links: &[],
        }
    }

    /// Override the maximum horizontal padding.  Builder-style so the
    /// caller can chain: `ModalView::new(...).with_max_pad_h(8)`.
    #[allow(dead_code)]
    pub fn with_max_pad_h(mut self, max_pad_h: u16) -> Self {
        self.max_pad_h = max_pad_h;
        self
    }

    /// Cap the body's content width, so a prose modal wraps at a
    /// readable measure instead of stretching to the terminal width.
    /// Chainable: `ModalView::new(...).with_max_content_width(PROSE_CONTENT_WIDTH)`.
    pub fn with_max_content_width(mut self, width: u16) -> Self {
        self.max_content_w = Some(width);
        self
    }

    /// Declare inline body links.  Chainable:
    /// `ModalView::new(...).with_links(&links)`.
    ///
    /// The slice must outlive the render call and its coordinates must
    /// index `body`; a link naming a span that does not exist simply
    /// never produces a rect, so a stale coordinate degrades to an
    /// unclickable label rather than a panic.
    pub(crate) fn with_links(mut self, links: &'a [ModalLink]) -> Self {
        self.links = links;
        self
    }
}

impl<'a> StatefulWidget for ModalView<'a> {
    type State = ModalState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let body_width = self.body.iter().map(|l| l.width()).max().unwrap_or(0) as u16;
        let button_specs: Vec<Button> = self
            .buttons
            .iter()
            .map(|b| Button::bracketed(b.label.as_str()))
            .collect();
        let button_width = buttons_row_width(&button_specs);
        // A capped modal still has to fit its button row, so raise the
        // cap to `button_width` before clamping — otherwise a narrow cap
        // would clip the buttons rather than wrap the prose.  Everything
        // downstream (the prospective wrap width, `ContentSize`, and the
        // final `compute_pad_h`) reads this one value, so the pre-render
        // sizing pass and the real render stay in agreement.
        let content_width = match self.max_content_w {
            Some(cap) => body_width.min(cap.max(button_width)).max(button_width),
            None => body_width.max(button_width),
        };
        // Prospective modal width: content + 2*MAX_PAD_H of horizontal
        // padding, clamped to the available area.  Derive the body's
        // inner wrap width from that — using the same padding rule
        // `draw_frame` will apply (`compute_pad_h`) so the pre-render
        // wrap matches the post-render rendering.  Otherwise a long
        // line that wraps when the modal clamps to the terminal width
        // leaves the modal too short for its content, AND a narrow
        // terminal forces `compute_pad_h` to floor at MIN_PAD_H while
        // this pre-pass still subtracts the full MAX padding.
        let prospective_modal_width = content_width
            .saturating_add(2 * self.max_pad_h)
            .min(area.width);
        let prospective_pad_h =
            compute_pad_h(prospective_modal_width, content_width, self.max_pad_h);
        let prospective_body_inner_w = prospective_modal_width
            .saturating_sub(2 * prospective_pad_h)
            .max(1);
        // The body is rendered as a block of its own natural width,
        // centred in the modal (see the `body_area` construction), so it
        // is measured at that width too — otherwise a body narrower than
        // the button row would be sized against columns it never paints.
        let prospective_body_render_w = body_width.clamp(1, prospective_body_inner_w);
        let wrapped_body_height = wrapped_rows(self.body, prospective_body_render_w);
        // Pinned bottom = 1 spacer + however many rows the footer needs.
        // A row too narrow for every button wraps rather than clipping,
        // so this is a function of the width, not a constant — and it
        // must be asked at the same width `render_buttons` will pack
        // against.
        let button_row_count = if self.buttons.is_empty() {
            0
        } else {
            button_rows_height(&button_specs, prospective_body_inner_w).max(1)
        };
        let pinned_bottom: u16 = if self.buttons.is_empty() {
            0
        } else {
            1 + button_row_count
        };
        let content = ContentSize {
            width: content_width,
            height: wrapped_body_height,
            pinned_top: 0,
            pinned_bottom,
            max_pad_h: self.max_pad_h,
        };
        let modal_area = centered_rect_for_content(content, area);

        // Pre-compute the body inner dimensions and observe the scroll
        // state with the post-clamp totals.  The body height excludes
        // the 1-row spacer + button row when buttons are present.
        let body_inner_h = modal_area.height.saturating_sub(VERTICAL_CHROME_ROWS);
        let text_body_height = body_inner_h.saturating_sub(pinned_bottom);
        // Inside the body rect we may also yield 1 column for the
        // scrollbar — but the scrollbar paints into the rightmost
        // padding column, NOT inside the body, so the body's wrap
        // width is the full inner width.  Use `compute_pad_h` so the
        // padding here exactly matches what `draw_frame` will apply.
        let pad_h = compute_pad_h(modal_area.width, content_width, self.max_pad_h);
        let body_inner_w = modal_area.width.saturating_sub(2 * pad_h).max(1);
        let body_render_w = body_width.clamp(1, body_inner_w);
        // A link-bearing body is wrapped here rather than by
        // `Paragraph`, because a hit-test needs to know which row and
        // column each span landed on and ratatui exposes no such
        // mapping.  `wrap_rows` reproduces `Wrap { trim: false }` row
        // for row (pinned by `wrap_matches_ratatui_paragraph_wrapping`),
        // so the two paths agree on height and the scroll math below is
        // shared unchanged.
        let wrapped: Option<Vec<WrappedRow>> =
            (!self.links.is_empty()).then(|| wrap_rows(self.body, body_render_w));
        let total = match &wrapped {
            Some(rows) => rows.len() as u16,
            None => wrapped_rows(self.body, body_render_w),
        };
        state.scroll_state.observe(total, text_body_height);

        let layout = draw_frame(
            modal_area,
            buf,
            FrameOpts {
                title: self.title,
                kind: self.kind,
                show_close_hint: self.dismissable,
                content,
                theme: self.theme,
            },
        );
        state.esc_button_rect = layout.esc_hit_rect;
        let inner = layout.body;
        if inner.height == 0 || inner.width == 0 {
            // Nothing is painted this frame, so nothing is clickable.
            // Left alone, the rects from the last frame that *did* paint
            // would keep answering `link_at`, and a click could follow a
            // link that is not on screen.
            state.link_rects.clear();
            return;
        }

        // Pre-wrapped rows are already cut to width, so no `Wrap` is
        // set on their `Paragraph`.  That is deliberate: with no
        // wrapping configured ratatui truncates instead, so any residual
        // disagreement with `WordWrapper` would clip one trailing cell
        // rather than reflow the row out from under the link rects
        // computed beside it.
        let body_paragraph = match &wrapped {
            Some(rows) => Paragraph::new(rows.iter().map(|r| r.line.clone()).collect::<Vec<_>>())
                .style(self.theme.modal_bg),
            None => Paragraph::new(self.body.to_vec())
                .wrap(Wrap { trim: false })
                .style(self.theme.modal_bg),
        };

        // Centre the body as a block whenever the modal is wider than
        // the body needs — which happens whenever the footer is the
        // widest thing in it.  Left-aligning inside the full inner width
        // instead pushes a self-centred body (the About page's art and
        // title column) off-centre by half the difference.  Text stays
        // left-aligned *within* the block; only the block moves.
        let body_area = Rect {
            x: inner.x + inner.width.saturating_sub(body_render_w) / 2,
            y: inner.y,
            width: body_render_w,
            height: text_body_height,
        };
        body_paragraph
            .scroll((state.scroll_state.scroll, 0))
            .render(body_area, buf);

        // Recorded after `body_area` is final so the rects are absolute
        // terminal coordinates, matching what a click carries.
        state.link_rects = match &wrapped {
            Some(rows) => link_rects(rows, self.links, body_area, state.scroll_state.scroll),
            None => Vec::new(),
        };

        // Scrollbar paints into the rightmost padding column when the
        // body overflows.  Click-and-drag on the modal gutter would
        // require routing mouse events into the `Modal` trait — for
        // now scrollbars are visual-only.
        if state.scroll_state.max_scroll() > 0 {
            let bar_area = Rect {
                x: layout.scrollbar_col,
                y: body_area.y,
                width: 1,
                height: body_area.height,
            };
            scrollbar::render_for_scroll_state(bar_area, &state.scroll_state, self.theme, buf);
        }

        if !self.buttons.is_empty() {
            let button_area = Rect {
                x: inner.x,
                y: inner.y + inner.height.saturating_sub(button_row_count),
                width: inner.width,
                height: button_row_count,
            };
            state.button_rects =
                render_buttons(button_area, buf, &button_specs, state.focused, self.theme);
        } else {
            state.button_rects.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    use crate::ui::scroll_container::PROSE_CONTENT_WIDTH;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // ── The focus ring on a modal with links and no buttons ───────

    /// The config-warning modal's shape: one link, no footer buttons.
    /// Nothing rests on a button there, so Enter must not report one.
    #[test]
    fn enter_presses_nothing_on_a_button_less_modal() {
        let mut st = ModalState::default();
        assert_eq!(
            st.handle_key_linkable(&key(KeyCode::Enter), 1, 0, true),
            LinkableResponse::Modal(ModalResponse::Continue),
            "there is no button 0 to press"
        );
        // Tab onto the link, and Enter follows it.
        st.handle_key_linkable(&key(KeyCode::Tab), 1, 0, true);
        assert_eq!(
            st.handle_key_linkable(&key(KeyCode::Enter), 1, 0, true),
            LinkableResponse::Link(0)
        );
    }

    /// With no button to start from, the ring has no resting position,
    /// so the first Tab has to enter it at the first link rather than
    /// stepping off a phantom one.
    #[test]
    fn the_first_tab_reaches_the_first_link_when_there_are_no_buttons() {
        let mut st = ModalState::default();
        st.handle_key_linkable(&key(KeyCode::Tab), 3, 0, true);
        assert_eq!(st.focused_link, Some(0));
        st.handle_key_linkable(&key(KeyCode::Tab), 3, 0, true);
        assert_eq!(st.focused_link, Some(1));
    }

    /// And backwards enters at the far end, the way a ring should.
    #[test]
    fn the_first_back_tab_reaches_the_last_link_when_there_are_no_buttons() {
        let mut st = ModalState::default();
        st.handle_key_linkable(&key(KeyCode::BackTab), 3, 0, true);
        assert_eq!(st.focused_link, Some(2));
    }

    /// A modal that has buttons still rests on button 0, so its ring is
    /// unchanged: the first Tab steps off the button, not onto it.
    #[test]
    fn a_modal_with_buttons_still_rests_on_its_first_button() {
        let mut st = ModalState::default();
        assert_eq!(st.focused_link, None);
        assert_eq!(
            st.handle_key_linkable(&key(KeyCode::Enter), 1, 2, true),
            LinkableResponse::Modal(ModalResponse::ButtonPressed(0))
        );
        st.handle_key_linkable(&key(KeyCode::Tab), 1, 2, true);
        assert_eq!(st.focused, 1, "Tab walks to the second button first");
        st.handle_key_linkable(&key(KeyCode::Tab), 1, 2, true);
        assert_eq!(st.focused_link, Some(0), "then wraps onto the link");
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
        state.handle_key(&key(KeyCode::Tab), 3, true);
        assert_eq!(state.focused, 1);
        state.handle_key(&key(KeyCode::Tab), 3, true);
        assert_eq!(state.focused, 2);
        state.handle_key(&key(KeyCode::Tab), 3, true);
        assert_eq!(state.focused, 0); // wraps
    }

    #[test]
    fn left_cycles_focus_backward_with_wrap() {
        let mut state = ModalState::new();
        state.handle_key(&key(KeyCode::Left), 3, true);
        assert_eq!(state.focused, 2);
        state.handle_key(&key(KeyCode::Left), 3, true);
        assert_eq!(state.focused, 1);
    }

    #[test]
    fn enter_activates_focused_button() {
        let mut state = ModalState::new();
        state.focused = 1;
        let response = state.handle_key(&key(KeyCode::Enter), 2, true);
        assert_eq!(response, ModalResponse::ButtonPressed(1));
        assert_eq!(state.response, Some(ModalResponse::ButtonPressed(1)));
    }

    #[test]
    fn escape_cancels() {
        let mut state = ModalState::new();
        let response = state.handle_key(&key(KeyCode::Esc), 2, true);
        assert_eq!(response, ModalResponse::Cancelled);
    }

    #[test]
    fn escape_does_not_dismiss_when_not_dismissable() {
        let mut state = ModalState::new();
        let response = state.handle_key(&key(KeyCode::Esc), 2, false);
        assert_eq!(response, ModalResponse::Continue);
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
                let m = ModalView::new("Notice", &body, &buttons, theme(), ModalKind::Normal, true);
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
        assert!(contents.contains("esc"), "esc hint missing: {contents}");
        assert!(state.esc_button_rect.is_some());
    }

    #[test]
    fn render_omits_esc_hint_when_not_dismissable() {
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body = vec![Line::raw("Choose:")];
        let buttons = vec![ModalButton::new("Ok"), ModalButton::new("No")];
        terminal
            .draw(|frame| {
                let m = ModalView::new(
                    "Pick one",
                    &body,
                    &buttons,
                    theme(),
                    ModalKind::Warning,
                    false,
                );
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        assert!(state.esc_button_rect.is_none());
    }

    #[test]
    fn esc_rect_is_set_after_dismissable_render() {
        // The hit-test itself is exercised by
        // `crate::app::modal::types::close_if_esc_clicked`; here we
        // just confirm the modal exposes the rect each render.
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body = vec![Line::raw("hi")];
        let buttons = vec![ModalButton::new("Ok")];
        terminal
            .draw(|frame| {
                let m = ModalView::new("T", &body, &buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        let r = state.esc_button_rect.expect("esc rect populated");
        assert!(r.width > 0 && r.height == 1);
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
        let resp = state.handle_key(&key(KeyCode::Down), 1, true);
        assert_eq!(resp, ModalResponse::Continue);
        assert_eq!(state.scroll_state.scroll, 1);
    }

    #[test]
    fn page_down_jumps_by_visible_height() {
        let mut state = state_with_scroll(0, 30, 10);
        state.handle_key(&key(KeyCode::PageDown), 1, true);
        assert_eq!(state.scroll_state.scroll, 10);
        // Second PageDown clamps at max_scroll (30 - 10 = 20).
        state.handle_key(&key(KeyCode::PageDown), 1, true);
        assert_eq!(state.scroll_state.scroll, 20);
        state.handle_key(&key(KeyCode::PageDown), 1, true);
        assert_eq!(state.scroll_state.scroll, 20);
    }

    #[test]
    fn home_and_end_jump_to_extremes() {
        let mut state = state_with_scroll(4, 12, 4);
        state.handle_key(&key(KeyCode::End), 1, true);
        assert_eq!(state.scroll_state.scroll, 8); // 12 - 4
        state.handle_key(&key(KeyCode::Home), 1, true);
        assert_eq!(state.scroll_state.scroll, 0);
    }

    #[test]
    fn render_clamps_scroll_when_body_shrinks() {
        // Modal chrome is 4 rows + 2 pinned-bottom (spacer + button) =
        // 6 fixed rows of overhead, plus 5 body rows = 11 modal rows.
        let backend = TestBackend::new(60, 12);
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
                let m = ModalView::new("Notice", &body, &buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        assert_eq!(state.scroll_state.scroll, 0);
    }

    #[test]
    fn render_paints_scrollbar_when_body_overflows() {
        // 60×8 terminal → after the new chrome (4 rows) + pinned bottom
        // (1 spacer + 1 button) only 2 body rows are visible.  Render
        // an 8-line body so 6 rows are below the fold.
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body: Vec<Line<'_>> = (0..8).map(|i| Line::raw(format!("body {i}"))).collect();
        let buttons = vec![ModalButton::new("Ok")];
        terminal
            .draw(|frame| {
                let m = ModalView::new("Tall", &body, &buttons, theme(), ModalKind::Normal, true);
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
            contents.contains('█'),
            "expected scrollbar thumb glyph, got: {contents}"
        );
        assert!(state.scroll_state.last_total > state.scroll_state.last_visible);
    }

    /// Render `body` + `buttons` at `w`x`h` and return the state plus the
    /// painted rows, so a test can assert on both geometry and glyphs.
    fn render_modal(
        w: u16,
        h: u16,
        body: &[Line<'static>],
        buttons: &[ModalButton],
    ) -> (ModalState, Vec<String>) {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut state = ModalState::new();
        terminal
            .draw(|frame| {
                let m = ModalView::new("Title", body, buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let rows = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_owned()).collect())
            .collect();
        (state, rows)
    }

    #[test]
    fn a_footer_too_wide_for_the_modal_wraps_onto_a_second_row() {
        // Clipping instead would leave a button the keyboard can still
        // focus and the click rect still points at, off the frame.
        let body = vec![Line::raw("Short.")];
        let buttons = vec![
            ModalButton::new("Release notes"),
            ModalButton::new("Check for updates"),
            ModalButton::new("View on GitHub"),
        ];
        let (state, rows) = render_modal(44, 20, &body, &buttons);
        assert_eq!(state.button_rects.len(), 3);
        let ys: Vec<u16> = state.button_rects.iter().map(|r| r.y).collect();
        assert!(ys[0] < ys[2], "the row wrapped: {ys:?}");
        // Every button is painted, and inside the terminal.
        let painted = rows.join("\n");
        for label in ["Release notes", "Check for updates", "View on GitHub"] {
            assert!(painted.contains(label), "{label} missing:\n{painted}");
        }
        assert!(state
            .button_rects
            .iter()
            .all(|r| r.x + r.width <= 44 && r.y < 20));
    }

    #[test]
    fn a_wrapped_footer_gets_the_rows_it_needs() {
        // The body must not be painted over: the frame grows by a row
        // instead, which only happens if the sizing pass packs the
        // footer at the same width the render does.
        let body: Vec<Line<'static>> = (0..3).map(|i| Line::raw(format!("Body {i}"))).collect();
        let buttons = vec![
            ModalButton::new("Release notes"),
            ModalButton::new("Check for updates"),
            ModalButton::new("View on GitHub"),
        ];
        let (_, rows) = render_modal(44, 20, &body, &buttons);
        let painted = rows.join("\n");
        for i in 0..3 {
            assert!(painted.contains(&format!("Body {i}")), "\n{painted}");
        }
    }

    #[test]
    fn a_body_narrower_than_the_footer_is_centred_in_the_modal() {
        // The About page centres its own art and title column, so
        // left-aligning that block inside a modal the footer widened
        // pushes the whole page off-centre by half the difference.
        let body = vec![Line::raw("|....|")];
        let buttons = vec![
            ModalButton::new("Check for updates"),
            ModalButton::new("View on GitHub"),
        ];
        let (_, rows) = render_modal(80, 12, &body, &buttons);
        let body_row = rows
            .iter()
            .find(|r| r.contains("|....|"))
            .expect("body row");
        let start = body_row.find('|').unwrap();
        let end = body_row.rfind('|').unwrap() + 1;
        let button_row = rows.iter().find(|r| r.contains("[ View")).expect("footer");
        let b_start = button_row.find('[').unwrap();
        let b_end = button_row.rfind(']').unwrap() + 1;
        // Both blocks share a centre, within the rounding of an odd
        // leftover column.
        let body_centre = start + end;
        let footer_centre = b_start + b_end;
        assert!(
            body_centre.abs_diff(footer_centre) <= 1,
            "body {start}..{end} vs footer {b_start}..{b_end}"
        );
    }

    /// Width of the painted modal — the run of rows whose cells carry
    /// the modal background, measured on the title row.
    fn painted_modal_width(terminal: &Terminal<TestBackend>) -> u16 {
        let buf = terminal.backend().buffer();
        let area = *buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .filter(|&x| buf[(x, y)].bg == theme().modal_bg.bg.unwrap())
                    .count() as u16
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn uncapped_prose_stretches_to_the_terminal_width() {
        // Baseline for `max_content_width_caps_a_prose_modal` — proves
        // the cap below isn't passing vacuously on a modal that was
        // already narrow.
        let backend = TestBackend::new(160, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body = vec![Line::raw("word ".repeat(40))];
        let buttons: Vec<ModalButton> = vec![];
        terminal
            .draw(|frame| {
                let m = ModalView::new("Prose", &body, &buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        assert_eq!(painted_modal_width(&terminal), 160);
    }

    #[test]
    fn max_content_width_caps_a_prose_modal() {
        // A single unwrapped paragraph has a natural content width of
        // its whole length, so without the cap the modal fills the
        // terminal.  Capped, the outer width is cap + 2 * MAX_PAD_H.
        let backend = TestBackend::new(160, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body = vec![Line::raw("word ".repeat(40))];
        let buttons: Vec<ModalButton> = vec![];
        terminal
            .draw(|frame| {
                let m = ModalView::new("Prose", &body, &buttons, theme(), ModalKind::Normal, true)
                    .with_max_content_width(80);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        assert_eq!(painted_modal_width(&terminal), 80 + 2 * MAX_PAD_H);
    }

    #[test]
    fn max_content_width_never_clips_the_button_row() {
        // A cap narrower than the buttons must lose to them — otherwise
        // the footer would be cut off rather than the prose rewrapped.
        let backend = TestBackend::new(160, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body = vec![Line::raw("word ".repeat(40))];
        let buttons = vec![
            ModalButton::new("A rather long button label"),
            ModalButton::new("And another one"),
        ];
        let button_w = buttons_row_width(
            &buttons
                .iter()
                .map(|b| Button::bracketed(b.label.as_str()))
                .collect::<Vec<_>>(),
        );
        terminal
            .draw(|frame| {
                let m = ModalView::new("Prose", &body, &buttons, theme(), ModalKind::Normal, true)
                    .with_max_content_width(10);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        assert_eq!(painted_modal_width(&terminal), button_w + 2 * MAX_PAD_H);
    }

    #[test]
    fn modal_height_grows_to_fit_wrapped_body_lines() {
        // 40-col terminal forces a body-line longer than the inner wrap
        // width to wrap onto two visual rows.  The modal must size itself
        // for the wrapped row count, not the pre-wrap line count, so the
        // user doesn't see a scroll arrow when the terminal has plenty of
        // room.
        let backend = TestBackend::new(40, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        // One long line (wraps to ~3 visual rows at width 36) + one short
        // line.  Pre-wrap height = 2; wrapped height should be ~4.
        let body = vec![
            Line::raw(
                "This is a fairly long body line that will definitely wrap inside a 40 column modal.",
            ),
            Line::raw("short tail"),
        ];
        let buttons = vec![ModalButton::new("Ok")];
        terminal
            .draw(|frame| {
                let m = ModalView::new("Wrap", &body, &buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        // Body fits — no scroll arrow expected.
        assert_eq!(state.scroll_state.max_scroll(), 0);
        // last_visible should be at least the wrapped row count.
        assert!(
            state.scroll_state.last_visible >= state.scroll_state.last_total,
            "expected body to fit; last_total={}, last_visible={}",
            state.scroll_state.last_total,
            state.scroll_state.last_visible,
        );
    }

    // ── Click hit-testing ────────────────────────────────────────────────

    fn render_two_button_modal(state: &mut ModalState) {
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let body = vec![Line::raw("Pick one.")];
        let buttons = vec![ModalButton::new("Ok"), ModalButton::new("Cancel")];
        terminal
            .draw(|frame| {
                let m = ModalView::new("Notice", &body, &buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), state);
            })
            .unwrap();
    }

    #[test]
    fn button_at_maps_click_to_footer_button_index() {
        let mut state = ModalState::new();
        render_two_button_modal(&mut state);
        assert_eq!(state.button_rects.len(), 2);
        // A click in the middle of each rendered button rect resolves to
        // that button's index.
        for (idx, rect) in state.button_rects.clone().iter().enumerate() {
            let col = rect.x + rect.width / 2;
            assert_eq!(state.button_at(col, rect.y), Some(idx));
        }
        // A click on the body (above the button row) hits no button.
        assert_eq!(state.button_at(0, 0), None);
    }

    #[test]
    fn handle_click_prefers_button_then_esc_then_continue() {
        let mut state = ModalState::new();
        render_two_button_modal(&mut state);
        let btn = state.button_rects[1];
        assert_eq!(
            state.handle_click(btn.x + btn.width / 2, btn.y, true),
            ModalResponse::ButtonPressed(1),
        );
        let esc = state.esc_button_rect.expect("esc rect populated");
        assert_eq!(
            state.handle_click(esc.x, esc.y, true),
            ModalResponse::Cancelled,
        );
        // The same esc click is inert when the modal isn't dismissable.
        assert_eq!(
            state.handle_click(esc.x, esc.y, false),
            ModalResponse::Continue
        );
        // A click on empty chrome resolves to Continue.
        assert_eq!(state.handle_click(0, 0, true), ModalResponse::Continue);
    }

    #[test]
    fn new_uses_default_max_pad_and_builder_overrides_it() {
        let body: [Line<'_>; 0] = [];
        let buttons: [ModalButton; 0] = [];
        let m = ModalView::new("T", &body, &buttons, theme(), ModalKind::Normal, true);
        assert_eq!(m.max_pad_h, MAX_PAD_H);
        let m = ModalView::new("T", &body, &buttons, theme(), ModalKind::Normal, true)
            .with_max_pad_h(8);
        assert_eq!(m.max_pad_h, 8);
    }

    #[test]
    fn width_cap_is_opt_in_and_defaults_off() {
        // The knob is shared machinery, so the default must stay
        // size-to-content: every modal that doesn't ask for a cap has to
        // lay out exactly as it did before the field existed.
        let body: [Line<'_>; 0] = [];
        let buttons: [ModalButton; 0] = [];
        let m = ModalView::new("T", &body, &buttons, theme(), ModalKind::Normal, true);
        assert_eq!(m.max_content_w, None);
        let m = ModalView::new("T", &body, &buttons, theme(), ModalKind::Normal, true)
            .with_max_content_width(PROSE_CONTENT_WIDTH);
        assert_eq!(m.max_content_w, Some(PROSE_CONTENT_WIDTH));
    }
}
