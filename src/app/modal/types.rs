//! The [`Modal`] trait, its render context, and dispatch outcomes.  See
//! [`super::ModalStack`] for ownership and dispatch.

use std::any::Any;
use std::time::Instant;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use crate::app::App;
use crate::config::{Config, Theme};

pub use crate::ui::ModalKind;

/// Hit-test for a cached `esc` close-affordance rect.
pub fn esc_rect_hit(esc_rect: Option<ratatui::layout::Rect>, col: u16, row: u16) -> bool {
    match esc_rect {
        Some(r) => col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height,
        None => false,
    }
}

/// `Close` when `(col, row)` lands inside `esc_rect`, else `Continue`.
pub fn close_if_esc_clicked(
    esc_rect: Option<ratatui::layout::Rect>,
    col: u16,
    row: u16,
) -> ModalOutcome {
    if esc_rect_hit(esc_rect, col, row) {
        ModalOutcome::Close
    } else {
        ModalOutcome::Continue
    }
}

/// Read-only context handed to [`Modal::render`].
pub struct ModalRenderCtx<'a> {
    pub theme: &'a Theme,
    pub config: &'a Config,
    pub cursor_visible: bool,
}

/// Outcome of dispatching input to a modal.  The dispatcher pops the modal before invoking
/// the handler; `Continue*` re-pushes it, `Close*` drops it, and the `*And` callbacks run
/// afterwards against the now-unborrowed `App`.
pub enum ModalOutcome {
    Continue,
    /// Stay open; the callback runs after the modal is pushed back (e.g. to open another
    /// modal on top).
    ContinueAnd(Box<dyn FnOnce(&mut App)>),
    Close,
    CloseAnd(Box<dyn FnOnce(&mut App)>),
}

/// A popup or overlay on top of the editor view.  The topmost modal on the
/// [`super::ModalStack`] absorbs all keyboard and wheel input and renders last.
pub trait Modal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>);

    /// `doc_height` / `doc_width` serve overlays that dispatch `Action`s through the same
    /// pipeline as direct keystrokes.
    fn handle_key(
        &mut self,
        key: KeyEvent,
        app: &mut App,
        doc_height: usize,
        doc_width: usize,
    ) -> ModalOutcome;

    /// Apply a bracketed paste.  `text` is raw; the modal sanitizes it
    /// ([`crate::ui::sanitize_paste`]).  Default: ignore.
    fn handle_paste(&mut self, _text: &str) -> ModalOutcome {
        ModalOutcome::Continue
    }

    fn handle_wheel(&mut self, _delta: i32) {}

    /// Left click at terminal `(col, row)`.  Default: ignore.
    fn handle_click(&mut self, _col: u16, _row: u16, _app: &mut App) -> ModalOutcome {
        ModalOutcome::Continue
    }

    /// Visual urgency; drives the title color.  Must read the same field the render path
    /// uses so the two can't drift.
    #[allow(dead_code)]
    fn kind(&self) -> ModalKind {
        ModalKind::Normal
    }

    /// Whether `Esc` / the `esc` button may dismiss this modal; `false` forces a footer
    /// button.  Same single-field rule as [`Self::kind`].
    #[allow(dead_code)]
    fn dismissable(&self) -> bool {
        true
    }

    /// Whether this modal is drawn.  `false` keeps its input capture but paints nothing — no
    /// frame, no background dim, and the editor renders as if no modal were open — for a modal
    /// that usually closes before it is worth showing
    /// ([`ClipboardReadModal`](super::ClipboardReadModal)).  A modal that starts hidden owes a
    /// [`Self::next_deadline`] for when it appears, and something that flips it at that time.
    fn is_shown(&self) -> bool {
        true
    }

    /// When this modal next needs a redraw for time-driven content (spinner, rotating
    /// tagline); aggregated by [`super::ModalStack::next_deadline`].
    fn next_deadline(&self) -> Option<Instant> {
        None
    }

    /// Always the trivial `{ self }`; no default because an `Any` supertrait would force
    /// `'static` on every implementor.
    fn as_any(&self) -> &dyn Any;

    /// Mutable counterpart to [`Self::as_any`], for [`super::ModalStack::find_first_mut`].
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
