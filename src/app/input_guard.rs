//! Input guards: drop keys, clicks and pastes for a short window after something the user did not
//! ask for takes over input (#111, #112).
//!
//! A modal that opens on its own — a timer, the file watcher, a background worker's result — or a
//! watcher-entered diff review takes focus at a moment the user didn't choose.  Someone typing at
//! that moment has two or three keys in flight before they notice, and those keys would press the
//! focused button or accept a hunk.  So the new input context ignores key, click and paste input
//! until [`INPUT_GUARD_WINDOW`] after it is first *drawn*; wheel scrolling and pointer motion pass,
//! since they decide nothing.
//!
//! **One mechanism, two holders.**  An [`InputGuard`] rides on each guarded
//! [`ModalStack`](super::modal::ModalStack) entry, and on [`App`]'s base-layer guard
//! ([`ModeGuard`]) for a takeover with no modal.  Both start on the first frame that shows them,
//! both feed the run loop's wake-up ([`InputGuard::deadline`]), and both show the dead window: a
//! guarded modal's controls and the hint line's chords render disabled until it expires.
//!
//! **What counts as "on its own"** is decided at the event loop's boundaries, not per modal: every
//! user action arrives as an `AppEvent::Term`, so everything run from
//! [`App::unprompted`] — timer ticks, async events, per-frame viewport prep — is unprompted.

use std::time::{Duration, Instant};

use super::App;
use crate::editor::Mode;

/// How long a newly shown input context ignores input.  Visual reaction time is ~200–250 ms and a
/// fast typist presses a key every ~150 ms, so this covers the keys already in flight when the
/// context appears, with margin, while staying short enough not to read as a hang.
pub(crate) const INPUT_GUARD_WINDOW: Duration = Duration::from_millis(450);

/// The guard on one input context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputGuard {
    /// Not drawn yet.  Blocks: a key queued before the frame that shows the context was not aimed
    /// at it either.
    Armed,
    /// Drawn; blocks until the instant passes.
    Until(Instant),
}

impl InputGuard {
    /// Whether input is dropped at `now`.
    pub(crate) fn blocks(self, now: Instant) -> bool {
        match self {
            Self::Armed => true,
            Self::Until(t) => now < t,
        }
    }

    /// Start the window: called once a frame showing the context has been drawn.  Idempotent, so
    /// the draw path can call it every frame.
    pub(crate) fn start(&mut self, now: Instant) {
        if *self == Self::Armed {
            *self = Self::Until(now + INPUT_GUARD_WINDOW);
        }
    }

    /// When the window ends, for the run loop's wake-up: the frame then redraws the context
    /// enabled.  `None` while still armed — the draw that starts the window comes first.
    pub(crate) fn deadline(self) -> Option<Instant> {
        match self {
            Self::Armed => None,
            Self::Until(t) => Some(t),
        }
    }
}

/// The base-layer guard: an [`InputGuard`] for an input takeover that is not a modal, tied to the
/// editor [`Mode`] it protects.  It stops applying once the editor leaves that mode, so a review
/// closed by something else (the file deleted under it) doesn't swallow the first keys typed into
/// the document after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModeGuard {
    pub(crate) mode: Mode,
    pub(crate) guard: InputGuard,
}

impl ModeGuard {
    pub(crate) fn armed(mode: Mode) -> Self {
        Self {
            mode,
            guard: InputGuard::Armed,
        }
    }

    /// Whether input is dropped at `now`, with the editor in `mode`.
    pub(crate) fn blocks(self, mode: Mode, now: Instant) -> bool {
        self.mode == mode && self.guard.blocks(now)
    }
}

// ── App side ──────────────────────────────────────────────────────────────

impl App {
    /// Run `f` as code the user's input did not trigger: every modal it pushes is guarded, and a
    /// non-modal takeover it starts may arm the base guard ([`Self::arm_base_input_guard`]).
    ///
    /// The run loop calls this around everything but terminal-event dispatch — timer ticks,
    /// per-frame viewport prep, and every async event (including those `collect_key_burst` meets
    /// mid-burst, inside a user dispatch).  Nests: the flag is restored, and marks compose.
    pub(super) fn unprompted<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let mark = self.modal_stack.push_mark();
        let outer = std::mem::replace(&mut self.unprompted, true);
        let out = f(self);
        self.unprompted = outer;
        self.modal_stack.guard_pushed_since(mark);
        out
    }

    /// Guard the base layer, now in `mode`, if the code running is [`Self::unprompted`].  Called by
    /// a non-modal input takeover — entering diff review — after it has switched the mode.  A
    /// takeover the user asked for clears any guard left over from an earlier one, which would
    /// otherwise come back to life when the editor re-enters its mode.
    pub(super) fn arm_base_input_guard(&mut self, mode: Mode) {
        self.base_input_guard = self.unprompted.then(|| ModeGuard::armed(mode));
    }

    /// Whether the base layer (the editor, no modal open) drops key, click and paste input at
    /// `now`.
    pub(super) fn base_input_blocked(&self, now: Instant) -> bool {
        self.base_input_guard
            .is_some_and(|g| g.blocks(self.editor.mode, now))
    }

    /// When a base-layer guard window ends, for the run loop's wake-up.
    pub(super) fn base_input_guard_deadline(&self) -> Option<Instant> {
        self.base_input_guard.and_then(|g| g.guard.deadline())
    }

    /// Start the guard window of whatever the frame just drawn showed: the topmost modal, or —
    /// with no modal on the stack — the base layer.
    pub(super) fn start_input_guards(&mut self, now: Instant) {
        // A guard whose mode the editor has left is spent, armed or not.
        if self
            .base_input_guard
            .is_some_and(|g| g.mode != self.editor.mode)
        {
            self.base_input_guard = None;
        }
        if self.modal_stack.is_empty() {
            if let Some(g) = self.base_input_guard.as_mut() {
                g.guard.start(now);
            }
        } else {
            self.modal_stack.start_top_guard(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_armed_guard_blocks_until_started_then_for_the_window() {
        let mut g = InputGuard::Armed;
        let t0 = Instant::now();
        assert!(
            g.blocks(t0 + Duration::from_secs(60)),
            "armed blocks indefinitely"
        );
        assert_eq!(g.deadline(), None);
        g.start(t0);
        assert_eq!(g.deadline(), Some(t0 + INPUT_GUARD_WINDOW));
        assert!(g.blocks(t0 + INPUT_GUARD_WINDOW - Duration::from_millis(1)));
        assert!(!g.blocks(t0 + INPUT_GUARD_WINDOW));
    }

    #[test]
    fn starting_twice_keeps_the_first_window() {
        let mut g = InputGuard::Armed;
        let t0 = Instant::now();
        g.start(t0);
        g.start(t0 + Duration::from_millis(300));
        assert_eq!(g.deadline(), Some(t0 + INPUT_GUARD_WINDOW));
    }

    // ── App wiring ────────────────────────────────────────────────────────

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::modal::{DiffIntroModal, NoticeModal};
    use crate::app::test_utils::{app_with_buffer, close_startup_modals, make_app};
    use crate::app::AppEvent;

    fn esc() -> KeyEvent {
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
    }

    /// A time far enough past `t` that any window started at `t` has run out.
    fn after_window(t: Instant) -> Instant {
        t + INPUT_GUARD_WINDOW + Duration::from_millis(1)
    }

    fn clean_app() -> App {
        let mut app = make_app();
        close_startup_modals(&mut app);
        app
    }

    #[test]
    fn a_modal_the_user_opened_takes_input_at_once() {
        let mut app = clean_app();
        app.notify("Saved a copy", crate::ui::ModalKind::Normal);
        assert!(!app.modal_stack.top_blocks_input(Instant::now()));
        app.dispatch_modal_key(esc(), 20, 80);
        assert!(app.modal_stack.is_empty(), "Esc closed it");
    }

    #[test]
    fn an_unprompted_modal_drops_keys_until_drawn_and_the_window_ends() {
        let mut app = clean_app();
        app.unprompted(|app| app.notify("Update available", crate::ui::ModalKind::Normal));

        app.dispatch_modal_key(esc(), 20, 80);
        assert!(
            app.modal_stack.contains::<NoticeModal>(),
            "a key before the first frame is dropped"
        );

        let drawn = Instant::now();
        app.start_input_guards(drawn);
        assert!(app.modal_stack.top_blocks_input(drawn));
        assert_eq!(
            app.modal_stack.next_deadline(),
            Some(drawn + INPUT_GUARD_WINDOW),
            "the loop wakes to redraw the buttons enabled"
        );

        // Lift the guard as if its window had run out.
        app.modal_stack.lift_guards();
        app.dispatch_modal_key(esc(), 20, 80);
        assert!(app.modal_stack.is_empty(), "past the window, Esc closes it");
    }

    #[test]
    fn the_window_runs_from_the_first_draw_not_from_the_push() {
        let mut app = clean_app();
        app.unprompted(|app| app.notify("Tip", crate::ui::ModalKind::Normal));
        let drawn = Instant::now() + Duration::from_secs(5);
        app.start_input_guards(drawn);
        assert!(app
            .modal_stack
            .top_blocks_input(drawn + Duration::from_millis(100)));
        assert!(!app.modal_stack.top_blocks_input(after_window(drawn)));
    }

    #[test]
    fn clicks_and_pastes_are_dropped_too() {
        let mut app = clean_app();
        app.unprompted(|app| app.notify("Export finished", crate::ui::ModalKind::Normal));
        app.dispatch_modal_click(0, 0);
        app.dispatch_modal_paste("text");
        assert!(app.modal_stack.contains::<NoticeModal>());
    }

    #[test]
    fn an_async_event_is_unprompted_wherever_it_is_handled() {
        let mut app = clean_app();
        app.handle_async_event(AppEvent::LinkOpenResult(Err("no handler".into())));
        assert!(app.modal_stack.contains::<NoticeModal>());
        assert!(app.modal_stack.top_blocks_input(Instant::now()));
        assert!(!app.unprompted, "the scope flag is restored");
    }

    #[test]
    fn a_modal_pushed_before_the_scope_keeps_taking_input() {
        let mut app = clean_app();
        app.notify("Saved", crate::ui::ModalKind::Normal);
        app.unprompted(|_| {});
        assert!(!app.modal_stack.top_blocks_input(Instant::now()));
    }

    // ── Diff review (#112) ────────────────────────────────────────────────

    fn app_without_intro() -> App {
        let mut app = app_with_buffer("alpha\nbeta\n", 0);
        close_startup_modals(&mut app);
        app.config.editor.show_diff_intro = false;
        app
    }

    #[test]
    fn a_watcher_entered_review_ignores_input_until_drawn_and_the_window_ends() {
        let mut app = app_without_intro();
        app.unprompted(|app| app.enter_diff_mode("alpha\nGAMMA\n".to_owned()));
        assert_eq!(app.editor.mode, Mode::Diff);
        assert!(app.base_input_blocked(Instant::now()));

        let drawn = Instant::now();
        app.start_input_guards(drawn);
        assert_eq!(
            app.base_input_guard_deadline(),
            Some(drawn + INPUT_GUARD_WINDOW)
        );
        assert!(app.base_input_blocked(drawn));
        assert!(!app.base_input_blocked(after_window(drawn)));
    }

    #[test]
    fn the_hint_line_shows_the_hunk_keys_disabled_while_guarded() {
        let mut app = app_without_intro();
        app.unprompted(|app| app.enter_diff_mode("alpha\nGAMMA\n".to_owned()));
        app.transient = None;
        match app.hint_content() {
            crate::ui::HintContent::Chords(set) => assert!(set.disabled),
            other => panic!("expected chords, got {other:?}"),
        }
        app.base_input_guard = None;
        match app.hint_content() {
            crate::ui::HintContent::Chords(set) => assert!(!set.disabled),
            other => panic!("expected chords, got {other:?}"),
        }
    }

    #[test]
    fn a_guarded_base_layer_drops_pastes() {
        // The gate is mode-agnostic; a plain document shows the paste would otherwise land.
        let mut app = app_with_buffer("alpha\n", 0);
        close_startup_modals(&mut app);
        let dims = crate::app::event_loop::DocDims {
            doc_height: 10,
            doc_width: 60,
            doc_area: ratatui::layout::Rect::new(0, 0, 60, 10),
        };
        app.base_input_guard = Some(ModeGuard::armed(app.editor.mode));
        app.dispatch_paste("pasted".into(), &dims);
        assert_eq!(app.editor.buffer.contents(), "alpha\n");

        app.base_input_guard = None;
        app.dispatch_paste("pasted".into(), &dims);
        assert_ne!(app.editor.buffer.contents(), "alpha\n");
    }

    #[test]
    fn a_review_the_user_opened_is_not_guarded() {
        let mut app = app_without_intro();
        app.enter_diff_mode("alpha\nGAMMA\n".to_owned());
        assert_eq!(app.editor.mode, Mode::Diff);
        assert!(!app.base_input_blocked(Instant::now()));
    }

    #[test]
    fn the_intro_modal_stands_guard_in_front_of_a_first_review() {
        let mut app = app_with_buffer("alpha\nbeta\n", 0);
        close_startup_modals(&mut app);
        assert!(app.config.editor.show_diff_intro);
        app.unprompted(|app| app.enter_diff_mode("alpha\nGAMMA\n".to_owned()));
        assert!(app.modal_stack.contains::<DiffIntroModal>());
        assert!(app.modal_stack.top_blocks_input(Instant::now()));
        assert!(
            !app.base_input_blocked(Instant::now()),
            "dismissing the intro is the user's own act; the review behind it needs no guard"
        );
    }

    #[test]
    fn the_review_guard_lapses_when_the_review_closes() {
        let mut app = app_without_intro();
        app.unprompted(|app| app.enter_diff_mode("alpha\nGAMMA\n".to_owned()));
        app.exit_diff_mode_discarding();
        assert_ne!(app.editor.mode, Mode::Diff);
        assert!(!app.base_input_blocked(Instant::now()));

        // Not even an armed one that was never drawn survives into a later review the user opens.
        app.enter_diff_mode("alpha\nDELTA\n".to_owned());
        assert!(!app.base_input_blocked(Instant::now()));
    }

    #[test]
    fn a_mode_guard_lapses_when_the_editor_leaves_its_mode() {
        let g = ModeGuard::armed(Mode::Diff);
        let now = Instant::now();
        assert!(g.blocks(Mode::Diff, now));
        assert!(!g.blocks(Mode::Rendered, now));
    }
}
