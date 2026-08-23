//! Shared chrome for the family of modals built on
//! [`crate::ui::ModalView`].
//!
//! Almost every confirm / prompt / notice modal has the identical
//! shape: a scrollable framed body, a centred footer button row, an
//! `esc` close affordance, a [`ModalKind`], and a `dismissable` flag.
//! Before this shell each adapter hand-wrote the same `render`,
//! `handle_wheel`, `handle_click`, `kind`, and `dismissable` bodies and
//! the same `state.handle_key` scaffolding — and, crucially, all but
//! one forgot to hit-test footer-button *clicks*, so the buttons were
//! keyboard-only.
//!
//! `ModalChrome` owns the [`ModalState`] (scroll + focus + the cached
//! button / esc rects), the `kind`, and the `dismissable` flag, and
//! centralises the input plumbing.  A concrete modal embeds one and is
//! left with only the parts that genuinely differ: how it builds its
//! `title` / `body` / `buttons`, and how it maps a [`ModalResponse`] to
//! a [`super::ModalOutcome`] (its `resolve`).  Both the key and click
//! paths funnel through that one `resolve`, so mouse and keyboard
//! behave identically and footer buttons are clickable for free.
//!
//! Modals with bespoke key handling can still embed the chrome and
//! reuse `render` / `on_wheel` / `on_click`; they just intercept the
//! keys they care about before delegating the rest to `on_key`.  (A
//! modal that needs a pinned interactive control row above the button
//! row — like the diff-intro modal's opt-out toggle — instead builds on
//! the `scroll_container` primitives directly; see
//! [`crate::ui::diff_intro_modal`].)

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{ModalKind, ModalRenderCtx};
use crate::ui::modal::LinkableResponse;
use crate::ui::modal_links::ModalLink;
use crate::ui::{ModalButton, ModalResponse, ModalState, ModalView};

/// Columns a chrome-backed body can use at most, given the whole
/// terminal `area`.
///
/// The modal sizes itself to its content, so the body's real width is
/// not known until after the content exists — but the *ceiling* is:
/// [`ModalView`] clamps the frame to the terminal and `compute_pad_h`
/// floors the padding at [`crate::ui::MIN_PAD_H`] per side.  A body that has to be
/// built for its width (the About page's pod, the cheat sheet's washes)
/// builds against this.
pub fn body_columns(area: Rect) -> u16 {
    area.width.saturating_sub(2 * crate::ui::MIN_PAD_H)
}

/// Shared state + input plumbing for [`ModalView`]-backed modals.
pub struct ModalChrome {
    /// Scroll / focus / cached hit-rects.  `pub` so the rare caller
    /// that needs the raw rects (and the modal tests that inspect
    /// `esc_button_rect`) can reach them, mirroring the old per-modal
    /// `state` field.
    pub state: ModalState,
    kind: ModalKind,
    dismissable: bool,
    /// Optional prose width cap forwarded to [`ModalView`] each frame.
    /// Stored here rather than passed to `render` so it is set once in
    /// the modal's `new()`, like `kind` and `dismissable`.
    max_content_w: Option<u16>,
}

impl ModalChrome {
    /// Build chrome with the given visual urgency and dismissability.
    /// These two values feed [`ModalView`], the keyboard `handle_key`
    /// gate, and the `Modal::kind` / `Modal::dismissable` accessors from
    /// this one place, so they can't drift between the three.
    pub fn new(kind: ModalKind, dismissable: bool) -> Self {
        Self {
            state: ModalState::new(),
            kind,
            dismissable,
            max_content_w: None,
        }
    }

    /// Cap the body's content width so a prose modal wraps at a
    /// readable measure instead of stretching to the terminal width.
    /// Chain onto `new()`; see
    /// [`crate::ui::ModalView::with_max_content_width`].
    pub fn with_max_content_width(mut self, width: u16) -> Self {
        self.max_content_w = Some(width);
        self
    }

    // ── Accessors ──────────────────────────────────────────────────────────

    pub fn kind(&self) -> ModalKind {
        self.kind
    }

    pub fn dismissable(&self) -> bool {
        self.dismissable
    }

    // ── Render ─────────────────────────────────────────────────────────────

    /// Render the framed modal.  `title` / `body` / `buttons` are
    /// supplied each frame so modals whose content depends on the theme
    /// or on live state (toggles, summaries) stay in control of their
    /// text — the chrome only owns the frame, scroll, and hit-rects.
    pub fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        ctx: &ModalRenderCtx<'_>,
        title: &str,
        body: &[Line<'_>],
        buttons: &[ModalButton],
    ) {
        self.render_with_links(frame, area, ctx, title, body, buttons, &[]);
    }

    /// [`Self::render`] for a modal carrying inline body links.
    ///
    /// `links` is rebuilt each frame alongside `body` — the modal reads
    /// [`Self::focused_link`] to decide which one draws focused, the
    /// same way it already reads `state.focused` for buttons.  Passing
    /// an empty slice is exactly `render`, which is how that method is
    /// implemented.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_with_links(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        ctx: &ModalRenderCtx<'_>,
        title: &str,
        body: &[Line<'_>],
        buttons: &[ModalButton],
        links: &[ModalLink],
    ) {
        let mut view = ModalView::new(title, body, buttons, ctx.theme, self.kind, self.dismissable);
        if let Some(w) = self.max_content_w {
            view = view.with_max_content_width(w);
        }
        if !links.is_empty() {
            view = view.with_links(links);
        }
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    // ── Input ──────────────────────────────────────────────────────────────

    /// Translate a key event into a [`ModalResponse`].  Scroll keys are
    /// absorbed by the underlying [`ModalState`] and reported as
    /// `Continue`.
    pub fn on_key(&mut self, key: &KeyEvent, num_buttons: usize) -> ModalResponse {
        self.state.handle_key(key, num_buttons, self.dismissable)
    }

    /// [`Self::on_key`] for a link-bearing modal: Tab walks one ring
    /// over links then buttons, and Enter on a link reports
    /// [`LinkableResponse::Link`].
    pub(crate) fn on_key_linkable(
        &mut self,
        key: &KeyEvent,
        num_links: usize,
        num_buttons: usize,
    ) -> LinkableResponse {
        self.state
            .handle_key_linkable(key, num_links, num_buttons, self.dismissable)
    }

    /// [`Self::on_click`] for a link-bearing modal.  A click inside a
    /// link's rect reports [`LinkableResponse::Link`]; everything else
    /// resolves exactly as before.
    pub(crate) fn on_click_linkable(&self, col: u16, row: u16) -> LinkableResponse {
        self.state.handle_click_linkable(col, row, self.dismissable)
    }

    /// Which body link currently holds focus, for the render pass to
    /// style via [`crate::ui::controls::link_style`].
    pub(crate) fn focused_link(&self) -> Option<usize> {
        self.state.focused_link
    }

    /// Translate a left-click into a [`ModalResponse`] — a footer
    /// button, the `esc` close affordance, or `Continue`.
    pub fn on_click(&self, col: u16, row: u16) -> ModalResponse {
        self.state.handle_click(col, row, self.dismissable)
    }

    /// Scroll the body by a mouse-wheel `delta`.
    pub fn on_wheel(&mut self, delta: i32) {
        self.state.scroll_by(delta);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::PROSE_CONTENT_WIDTH;

    #[test]
    fn the_width_cap_is_opt_in() {
        // `ModalChrome` is the only production `ModalView::new` call
        // site, so this default is what every chrome-backed modal gets.
        // It must stay `None` — size-to-content — so the modals that
        // never ask for a cap lay out exactly as they did before the
        // knob existed.
        assert_eq!(
            ModalChrome::new(ModalKind::Normal, true).max_content_w,
            None
        );
    }

    #[test]
    fn the_width_cap_builder_is_chainable_onto_new() {
        let chrome =
            ModalChrome::new(ModalKind::Warning, false).with_max_content_width(PROSE_CONTENT_WIDTH);
        assert_eq!(chrome.max_content_w, Some(PROSE_CONTENT_WIDTH));
        // The other two `new()` values survive the chain.
        assert_eq!(chrome.kind(), ModalKind::Warning);
        assert!(!chrome.dismissable());
    }
}
