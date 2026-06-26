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
use crate::ui::{ModalButton, ModalResponse, ModalState, ModalView};

/// Shared state + input plumbing for [`ModalView`]-backed modals.
pub struct ModalChrome {
    /// Scroll / focus / cached hit-rects.  `pub` so the rare caller
    /// that needs the raw rects (and the modal tests that inspect
    /// `esc_button_rect`) can reach them, mirroring the old per-modal
    /// `state` field.
    pub state: ModalState,
    kind: ModalKind,
    dismissable: bool,
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
        }
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
        let view = ModalView::new(title, body, buttons, ctx.theme, self.kind, self.dismissable);
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    // ── Input ──────────────────────────────────────────────────────────────

    /// Translate a key event into a [`ModalResponse`].  Scroll keys are
    /// absorbed by the underlying [`ModalState`] and reported as
    /// `Continue`.
    pub fn on_key(&mut self, key: &KeyEvent, num_buttons: usize) -> ModalResponse {
        self.state.handle_key(key, num_buttons, self.dismissable)
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
