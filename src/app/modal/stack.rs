//! `ModalStack`: the ordered [`super::Modal`] instances the App layers over the editor view.  The
//! topmost absorbs input and renders last.  The dispatcher pattern is "pop, dispatch, decide whether
//! to push back", which lets [`super::Modal::handle_key`] take `&mut self` and `&mut App` at once.

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

    /// Pop the topmost modal, if any.  The dispatcher pops before handing over the event so the
    /// modal can take `&mut App` without re-borrowing the stack.
    pub fn pop(&mut self) -> Option<Box<dyn Modal>> {
        self.inner.pop()
    }

    /// Borrow the topmost modal without removing it — for the render and wheel-scroll paths, which
    /// don't call back into `App`.
    pub fn top_mut(&mut self) -> Option<&mut dyn Modal> {
        match self.inner.last_mut() {
            Some(b) => Some(&mut **b),
            None => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Whether any modal is drawn — see [`Modal::is_shown`].
    pub fn any_shown(&self) -> bool {
        self.inner.iter().any(|m| m.is_shown())
    }

    /// Earliest [`Modal::next_deadline`] across the whole stack.  Every modal is consulted, not
    /// just the topmost, so an animated modal buried under an overlay resumes when revealed.
    pub fn next_deadline(&self) -> Option<std::time::Instant> {
        self.inner.iter().filter_map(|m| m.next_deadline()).min()
    }

    #[allow(dead_code)] // used by tests in this module
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Remove the topmost modal of type `T`, reporting whether one matched.  Used to drop a queued
    /// modal whose precondition has become unsatisfiable.
    pub fn remove_first<T: Modal + 'static>(&mut self) -> bool {
        if let Some(idx) = self.inner.iter().position(|m| m.as_any().is::<T>()) {
            self.inner.remove(idx);
            true
        } else {
            false
        }
    }

    /// True if any modal of type `T` is on the stack.
    #[allow(dead_code)]
    pub fn contains<T: Modal + 'static>(&self) -> bool {
        self.inner.iter().any(|m| m.as_any().is::<T>())
    }

    /// Number of modals of type `T` on the stack — tests assert a modal is never stacked twice.
    #[allow(dead_code)]
    pub fn count<T: Modal + 'static>(&self) -> usize {
        self.inner.iter().filter(|m| m.as_any().is::<T>()).count()
    }

    /// Mutable borrow of the first modal of type `T`, if any.  "First" is bottom-up, matching
    /// [`Self::remove_first`].  Used to refresh a queued reconciliation modal's `on_disk_contents`
    /// when a fresh external write arrives before the user has confirmed.
    pub fn find_first_mut<T: Modal + 'static>(&mut self) -> Option<&mut T> {
        self.inner
            .iter_mut()
            .find(|m| m.as_any().is::<T>())
            .and_then(|m| m.as_any_mut().downcast_mut::<T>())
    }

    /// Shared borrow of the first modal of type `T`, bottom-up like [`Self::find_first_mut`].
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
