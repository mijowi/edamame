//! Core trait, render context, and dispatch outcome for modals.
//!
//! The `Modal` trait abstracts every popup, prompt, and overlay that the
//! App can layer on top of the editor view.  See [`super`] for the
//! `ModalStack` that owns these as `Box<dyn Modal>` and dispatches input.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use crate::app::App;
use crate::config::{Config, KeyMap, Theme};

/// Read-only context handed to [`Modal::render`].  Centralises the
/// references every modal needs at draw time so individual modal
/// implementations don't have to pull them off `App` themselves.
pub struct ModalRenderCtx<'a> {
    pub theme: &'a Theme,
    pub config: &'a Config,
    pub keymap: Option<&'a KeyMap>,
    pub cursor_visible: bool,
}

/// Outcome of dispatching a key event to a modal.
///
/// Returned from [`Modal::handle_key`].  The dispatcher pops the modal
/// off the stack before invoking the handler — `Continue` re-pushes it,
/// `Close` drops it, `CloseAnd` drops it and runs the supplied callback
/// against the now-unborrowed `App`.
pub enum ModalOutcome {
    /// Modal stays on the stack, no follow-up action.
    Continue,
    /// Modal is removed from the stack; no follow-up.
    Close,
    /// Modal is removed from the stack; run the callback against `App`.
    /// Used for the common "close + dispatch" pattern (e.g. dirty-guard
    /// closes then triggers a navigation).
    CloseAnd(Box<dyn FnOnce(&mut App)>),
}

/// A modal popup or overlay that can sit on top of the editor view.
///
/// The topmost modal on the [`super::ModalStack`] absorbs all keyboard
/// and wheel input and renders last.
pub trait Modal {
    /// Draw the modal into `area` of `frame`.
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>);

    /// Apply a keypress.  Receives `&mut App` so handlers can flash
    /// messages, persist config, dispatch follow-up actions, etc.
    /// `doc_height` and `doc_width` are the document area dimensions
    /// — needed by overlays that dispatch `Action`s through the same
    /// pipeline as direct keystrokes.
    fn handle_key(
        &mut self,
        key: KeyEvent,
        app: &mut App,
        doc_height: usize,
        doc_width: usize,
    ) -> ModalOutcome;

    /// Apply a mouse-wheel delta.  Default: no-op (modals without a
    /// scrollable body ignore wheel events).
    fn handle_wheel(&mut self, _delta: i32) {}

    /// Type-erased self-reference, used by [`super::ModalStack`] for
    /// type-aware operations (`remove_first<T>`, `contains<T>`).  Every
    /// implementation should be the trivial `fn as_any(&self) -> &dyn
    /// Any { self }` — but the trait can't provide a default because
    /// `Self: Any` isn't a supertrait bound (would force `'static` on
    /// every implementor, which is true today but constrains future
    /// adapter types).
    fn as_any(&self) -> &dyn Any;
}
