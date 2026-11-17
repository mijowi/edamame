//! `ModalStack`: the ordered [`super::Modal`] instances the App layers over the editor view.  The
//! topmost absorbs input and renders last.  The dispatcher pattern is "pop, dispatch, decide whether
//! to push back", which lets [`super::Modal::handle_key`] take `&mut self` and `&mut App` at once.
//!
//! Each entry may carry an [`InputGuard`] (see [`crate::app::input_guard`]): a modal that opened
//! on its own ignores input until shortly after it is first drawn.  The stack doesn't know why a
//! modal was pushed; [`App::unprompted`](crate::app::App::unprompted) brackets the code that runs
//! without user input with [`ModalStack::push_mark`] / [`ModalStack::guard_pushed_since`].

use std::time::Instant;

use super::Modal;
use crate::app::input_guard::InputGuard;

struct Entry {
    modal: Box<dyn Modal>,
    /// Push sequence number, compared against a [`PushMark`].
    seq: u64,
    guard: Option<InputGuard>,
}

/// A point in the stack's push history; see [`ModalStack::guard_pushed_since`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushMark(u64);

/// Stack of active modals.  The last entry is the topmost.
#[derive(Default)]
pub struct ModalStack {
    inner: Vec<Entry>,
    /// Pushes so far; the next entry's `seq`.
    pushes: u64,
}

impl ModalStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a modal onto the top of the stack, unguarded; see [`Self::guard_pushed_since`].
    pub fn push(&mut self, modal: Box<dyn Modal>) {
        self.inner.push(Entry {
            modal,
            seq: self.pushes,
            guard: None,
        });
        self.pushes += 1;
    }

    /// Pop the topmost modal, if any.  The dispatcher pops before handing over the event so the
    /// modal can take `&mut App` without re-borrowing the stack.  The entry's guard is dropped:
    /// the dispatcher pops only a modal whose guard no longer blocks.
    pub fn pop(&mut self) -> Option<Box<dyn Modal>> {
        self.inner.pop().map(|e| e.modal)
    }

    /// Borrow the topmost modal without removing it — for the render and wheel-scroll paths, which
    /// don't call back into `App`.
    pub fn top_mut(&mut self) -> Option<&mut dyn Modal> {
        match self.inner.last_mut() {
            Some(e) => Some(&mut *e.modal),
            None => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Whether any modal is drawn — see [`Modal::is_shown`].
    pub fn any_shown(&self) -> bool {
        self.inner.iter().any(|e| e.modal.is_shown())
    }

    /// Earliest [`Modal::next_deadline`] across the whole stack, and the topmost entry's guard
    /// expiry, so the frame that re-enables its buttons is drawn on time.  Every modal is
    /// consulted, not just the topmost, so an animated modal buried under an overlay resumes when
    /// revealed.  A past guard expiry may be returned; the caller ignores past deadlines.
    pub fn next_deadline(&self) -> Option<Instant> {
        let guard = self.inner.last().and_then(|e| e.guard?.deadline());
        self.inner
            .iter()
            .filter_map(|e| e.modal.next_deadline())
            .chain(guard)
            .min()
    }

    // ── Input guards ──────────────────────────────────────────────────────

    /// The current point in the push history, to pass to [`Self::guard_pushed_since`].
    pub fn push_mark(&self) -> PushMark {
        PushMark(self.pushes)
    }

    /// Arm the guard on every entry still on the stack that was pushed at or after `mark`.  An
    /// entry already carrying a guard keeps it.
    pub fn guard_pushed_since(&mut self, mark: PushMark) {
        for e in self.inner.iter_mut().filter(|e| e.seq >= mark.0) {
            e.guard.get_or_insert(InputGuard::Armed);
        }
    }

    /// Arm the guard on every entry on the stack — the modals `App::new` opened, before any input
    /// could have asked for them.
    pub fn guard_all(&mut self) {
        self.guard_pushed_since(PushMark(0));
    }

    /// Whether the topmost modal drops key, click and paste input at `now`.
    pub fn top_blocks_input(&self, now: Instant) -> bool {
        self.inner
            .last()
            .and_then(|e| e.guard)
            .is_some_and(|g| g.blocks(now))
    }

    /// Start the topmost entry's guard window if it is shown: called after a frame drew it.
    pub fn start_top_guard(&mut self, now: Instant) {
        if let Some(e) = self.inner.last_mut().filter(|e| e.modal.is_shown()) {
            if let Some(g) = e.guard.as_mut() {
                g.start(now);
            }
        }
    }

    /// Lift every guard on the stack, as if each window had elapsed — for tests that drive an
    /// unprompted modal with keys.
    #[cfg(test)]
    pub fn lift_guards(&mut self) {
        for e in &mut self.inner {
            e.guard = None;
        }
    }

    // ── Lookup ────────────────────────────────────────────────────────────

    #[allow(dead_code)] // used by tests in this module
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Remove the topmost modal of type `T`, reporting whether one matched.  Used to drop a queued
    /// modal whose precondition has become unsatisfiable.
    pub fn remove_first<T: Modal + 'static>(&mut self) -> bool {
        if let Some(idx) = self.inner.iter().position(|e| e.modal.as_any().is::<T>()) {
            self.inner.remove(idx);
            true
        } else {
            false
        }
    }

    /// True if any modal of type `T` is on the stack.
    #[allow(dead_code)]
    pub fn contains<T: Modal + 'static>(&self) -> bool {
        self.inner.iter().any(|e| e.modal.as_any().is::<T>())
    }

    /// Number of modals of type `T` on the stack — tests assert a modal is never stacked twice.
    #[allow(dead_code)]
    pub fn count<T: Modal + 'static>(&self) -> usize {
        self.inner
            .iter()
            .filter(|e| e.modal.as_any().is::<T>())
            .count()
    }

    /// Mutable borrow of the first modal of type `T`, if any.  "First" is bottom-up, matching
    /// [`Self::remove_first`].  Used to refresh a queued reconciliation modal's `on_disk_contents`
    /// when a fresh external write arrives before the user has confirmed.
    pub fn find_first_mut<T: Modal + 'static>(&mut self) -> Option<&mut T> {
        self.inner
            .iter_mut()
            .find(|e| e.modal.as_any().is::<T>())
            .and_then(|e| e.modal.as_any_mut().downcast_mut::<T>())
    }

    /// Shared borrow of the first modal of type `T`, bottom-up like [`Self::find_first_mut`].
    pub fn find_first<T: Modal + 'static>(&self) -> Option<&T> {
        self.inner
            .iter()
            .find(|e| e.modal.as_any().is::<T>())
            .and_then(|e| e.modal.as_any().downcast_ref::<T>())
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
    use std::time::Duration;

    use crate::app::input_guard::INPUT_GUARD_WINDOW;

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

    #[test]
    fn a_started_guard_feeds_the_next_deadline() {
        let now = Instant::now();
        let mut stack = ModalStack::new();
        stack.push(Box::new(ModalA));
        stack.guard_all();
        assert_eq!(stack.next_deadline(), None, "armed: the draw comes first");
        stack.start_top_guard(now);
        assert_eq!(stack.next_deadline(), Some(now + INPUT_GUARD_WINDOW));
        assert!(!stack.top_blocks_input(now + INPUT_GUARD_WINDOW));
    }

    #[test]
    fn a_guard_buried_under_another_modal_starts_when_revealed() {
        let now = Instant::now();
        let mut stack = ModalStack::new();
        stack.push(Box::new(ModalA));
        stack.push(Box::new(ModalB));
        stack.guard_all();
        stack.start_top_guard(now);
        stack.pop();
        assert!(
            stack.top_blocks_input(now + Duration::from_secs(60)),
            "A was never drawn, so it is still armed"
        );
    }

    fn _key(_code: KeyCode) -> KeyEvent {
        KeyEvent::new(_code, KeyModifiers::NONE)
    }
}
