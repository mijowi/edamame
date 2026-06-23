//! `ModalStack`: ordered collection of [`super::Modal`] instances that
//! the App layers on top of the editor view.
//!
//! Topmost modal absorbs input and renders last.  Pushes append; pops
//! peel from the top.  The dispatcher pattern is "pop, dispatch, decide
//! whether to push back" — this lets [`super::Modal::handle_key`] take
//! both `&mut self` (the modal) and `&mut App` (the application state)
//! without borrow conflicts, since the popped modal owns itself.

use super::Modal;

/// Stack of active modals.  The last entry is the topmost.
#[derive(Default)]
pub struct ModalStack {
    inner: Vec<Box<dyn Modal>>,
}

impl ModalStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a modal onto the top of the stack.
    pub fn push(&mut self, modal: Box<dyn Modal>) {
        self.inner.push(modal);
    }

    /// Pop the topmost modal, if any.  The dispatcher calls this before
    /// handing the event to the modal so the modal can take `&mut App`
    /// without re-borrowing the stack.
    pub fn pop(&mut self) -> Option<Box<dyn Modal>> {
        self.inner.pop()
    }

    /// Borrow the topmost modal without removing it.  Used by the
    /// render path and the wheel-scroll path, which don't need the
    /// pop-and-replace dance because they don't call back into `App`.
    pub fn top_mut(&mut self) -> Option<&mut dyn Modal> {
        match self.inner.last_mut() {
            Some(b) => Some(&mut **b),
            None => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Earliest [`Modal::next_deadline`] across the whole stack, for
    /// the run loop's blocking-deadline aggregation.  Every modal is
    /// consulted — not just the topmost — so an animated modal buried
    /// under a transient overlay resumes seamlessly when revealed.
    pub fn next_deadline(&self) -> Option<std::time::Instant> {
        self.inner.iter().filter_map(|m| m.next_deadline()).min()
    }

    #[allow(dead_code)] // used by tests in this module
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Remove the topmost modal of type `T`, if any.  Returns whether a
    /// matching modal was removed.  Used to drop a queued modal when
    /// its precondition becomes unsatisfiable (e.g. dropping a queued
    /// remote-image-prompt after the user opts out of images entirely).
    pub fn remove_first<T: Modal + 'static>(&mut self) -> bool {
        if let Some(idx) = self.inner.iter().position(|m| m.as_any().is::<T>()) {
            self.inner.remove(idx);
            true
        } else {
            false
        }
    }

    /// True if any modal of type `T` is currently on the stack.
    /// Used by tests in this module.
    #[allow(dead_code)]
    pub fn contains<T: Modal + 'static>(&self) -> bool {
        self.inner.iter().any(|m| m.as_any().is::<T>())
    }

    /// Number of modals of type `T` currently on the stack.  Used by
    /// tests asserting that a modal is never stacked more than once.
    #[allow(dead_code)]
    pub fn count<T: Modal + 'static>(&self) -> usize {
        self.inner.iter().filter(|m| m.as_any().is::<T>()).count()
    }

    /// Mutable borrow of the first modal of type `T` on the stack, if
    /// any.  Used by `App::handle_file_changed` to refresh the
    /// `on_disk_contents` carried by a child reconciliation modal
    /// (`DirtyConflictSaveCopyModal` / `DirtyConflictDiscardConfirmModal`)
    /// when a fresh external write arrives before the user has
    /// confirmed.  "First" is bottom-up — matches the order
    /// [`Self::remove_first`] uses so the two methods pair naturally.
    pub fn find_first_mut<T: Modal + 'static>(&mut self) -> Option<&mut T> {
        self.inner
            .iter_mut()
            .find(|m| m.as_any().is::<T>())
            .and_then(|m| m.as_any_mut().downcast_mut::<T>())
    }

    /// Shared borrow of the first modal of type `T` on the stack, if any.
    /// "First" is bottom-up, matching [`Self::find_first_mut`].  Used to
    /// inspect a flag on an open modal (e.g. whether a `SaveAsModal` is the
    /// file-deletion recovery flow) without mutating it.
    pub fn find_first<T: Modal + 'static>(&self) -> Option<&T> {
        self.inner
            .iter()
            .find(|m| m.as_any().is::<T>())
            .and_then(|m| m.as_any().downcast_ref::<T>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::modal::types::{ModalOutcome, ModalRenderCtx};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;
    use ratatui::Frame;
    use std::any::Any;

    struct ModalA;
    struct ModalB;

    impl Modal for ModalA {
        fn render(&mut self, _f: &mut Frame<'_>, _a: Rect, _c: &ModalRenderCtx<'_>) {}
        fn handle_key(
            &mut self,
            _k: KeyEvent,
            _app: &mut crate::app::App,
            _h: usize,
            _w: usize,
        ) -> ModalOutcome {
            ModalOutcome::Continue
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    impl Modal for ModalB {
        fn render(&mut self, _f: &mut Frame<'_>, _a: Rect, _c: &ModalRenderCtx<'_>) {}
        fn handle_key(
            &mut self,
            _k: KeyEvent,
            _app: &mut crate::app::App,
            _h: usize,
            _w: usize,
        ) -> ModalOutcome {
            ModalOutcome::Continue
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    #[test]
    fn push_and_pop_preserve_order() {
        let mut stack = ModalStack::new();
        stack.push(Box::new(ModalA));
        stack.push(Box::new(ModalB));
        assert_eq!(stack.len(), 2);

        let top = stack.pop().unwrap();
        assert!(top.as_any().is::<ModalB>());
        assert_eq!(stack.len(), 1);

        let bottom = stack.pop().unwrap();
        assert!(bottom.as_any().is::<ModalA>());
        assert!(stack.is_empty());
    }

    #[test]
    fn contains_detects_present_type() {
        let mut stack = ModalStack::new();
        stack.push(Box::new(ModalA));
        stack.push(Box::new(ModalB));
        assert!(stack.contains::<ModalA>());
        assert!(stack.contains::<ModalB>());
    }

    #[test]
    fn contains_returns_false_when_absent() {
        let mut stack = ModalStack::new();
        stack.push(Box::new(ModalA));
        assert!(!stack.contains::<ModalB>());
    }

    #[test]
    fn remove_first_drops_queued_modal_below_top() {
        let mut stack = ModalStack::new();
        stack.push(Box::new(ModalB)); // bottom
        stack.push(Box::new(ModalA)); // top
        assert!(stack.remove_first::<ModalB>());
        assert_eq!(stack.len(), 1);
        // ModalA stays on top
        assert!(stack.contains::<ModalA>());
        assert!(!stack.contains::<ModalB>());
    }

    #[test]
    fn remove_first_returns_false_when_no_match() {
        let mut stack = ModalStack::new();
        stack.push(Box::new(ModalA));
        assert!(!stack.remove_first::<ModalB>());
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn top_mut_returns_topmost_only() {
        let mut stack = ModalStack::new();
        stack.push(Box::new(ModalA));
        stack.push(Box::new(ModalB));
        let top = stack.top_mut().unwrap();
        assert!(top.as_any().is::<ModalB>());
    }

    #[test]
    fn empty_stack_returns_none() {
        let mut stack = ModalStack::new();
        assert!(stack.top_mut().is_none());
        assert!(stack.pop().is_none());
        assert!(stack.is_empty());
    }

    fn _key(_code: KeyCode) -> KeyEvent {
        KeyEvent::new(_code, KeyModifiers::NONE)
    }
}
