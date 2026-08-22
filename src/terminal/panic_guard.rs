//! Tells the process-wide panic hook that a panic is *expected* and will
//! be caught, so it doesn't tear the terminal down on the way past.
//!
//! # The problem this exists for
//!
//! `main` installs a panic hook that calls [`restore`](super::restore) —
//! leaving the alternate screen and raw mode — and then chains to the
//! default hook, which prints the payload to stderr.  That is exactly
//! right for a panic that ends the process: without it the user is left
//! at a shell with raw mode still on.
//!
//! It is exactly wrong for a panic wrapped in `catch_unwind`.  The hook
//! runs *before* unwinding, so it fires whether or not anyone is going to
//! catch the panic, and it has no way to tell the two apart.  A caught
//! panic therefore left the app **still running** on a terminal that had
//! been restored out from under it: no alt screen, no raw mode, panic
//! text on the scrollback, and no way back short of killing the process.
//! Strictly worse than the clean crash the hook was written to prevent.
//!
//! Seven call sites are wrapped in `catch_unwind` and all seven had it:
//! [`markdown::highlight`](crate::markdown::highlight)'s tokenizer and
//! its warm worker, [`diagram::mermaid`](crate::diagram::mermaid)'s
//! renderer and its font warmup, the image decode worker, its
//! scratch-encode step, and the `Picker::from_query_stdio` probe.  The
//! highlighter is the one that made it worth fixing — it runs
//! synchronously on the render thread over attacker-controlled code-block
//! text, through a backtracking regex engine, which is the likeliest of
//! the seven to actually fire.
//!
//! # Scope it to the `catch_unwind`, and nothing else
//!
//! A guard left live over the code *after* the catch claims a panic is
//! expected when nobody is going to catch it — so the hook stands down
//! and the terminal is never restored, which is the original defect with
//! the sign flipped.  On a worker thread that costs a silent death; on
//! the main thread (`detect_image_protocol` runs there, after the hook is
//! installed) it means unwinding out of `main` with the alternate screen
//! still up and no message printed.  Bind the guard in the same
//! expression or block as the `catch_unwind` it belongs to.
//!
//! # Why the counter is thread-local
//!
//! The hook runs on the panicking thread, so a thread-local answers the
//! question that is actually being asked: "is *this* thread inside a
//! guarded section?"  A process-global flag would let one thread's
//! guarded section silence an unrelated thread's genuine crash — the
//! image and encode workers run concurrently with the render thread, so
//! that overlap is the normal case rather than a corner.
//!
//! It is a counter rather than a flag because the guarded sections nest:
//! `image_dispatch`'s guarded decode calls `diagram::render_mermaid_svg`,
//! which guards a `catch_unwind` of its own.  A flag would be cleared by
//! the inner section's exit and leave the rest of the outer one exposed.

use std::cell::Cell;

thread_local! {
    /// How many [`ExpectedPanic`] guards are live on this thread.
    static EXPECTED: Cell<usize> = const { Cell::new(0) };
}

/// Marks the current thread as being inside a `catch_unwind` for as long
/// as it is alive.  Create one immediately before the `catch_unwind` that
/// will do the catching:
///
/// ```ignore
/// let _guard = ExpectedPanic::new();
/// catch_unwind(AssertUnwindSafe(|| risky()))
/// ```
///
/// `Drop` runs during unwinding — after the hook, before `catch_unwind`
/// returns — so the count is correct again by the time control comes
/// back, and a nested guard is restored rather than cleared.
#[derive(Debug)]
pub struct ExpectedPanic(());

impl ExpectedPanic {
    #[allow(clippy::new_without_default)] // a `Default` guard would be silently inert
    pub fn new() -> Self {
        adjust(1);
        Self(())
    }
}

impl Drop for ExpectedPanic {
    fn drop(&mut self) {
        adjust(-1);
    }
}

/// `try_with`, never `with`: this runs during unwinding and during thread
/// teardown, where the thread-local may already be destroyed and `with`
/// would panic — inside a panic, which aborts.
fn adjust(delta: isize) {
    let _ = EXPECTED.try_with(|c| {
        let next = if delta >= 0 {
            c.get().saturating_add(delta as usize)
        } else {
            c.get().saturating_sub(delta.unsigned_abs())
        };
        c.set(next);
    });
}

/// Is the panicking thread inside a [`ExpectedPanic`] guard?
///
/// Answers `false` when the thread-local is unavailable, so an
/// unexpected panic during thread teardown still restores the terminal.
/// Getting this wrong in that direction merely prints a stack trace;
/// getting it wrong in the other leaves a live TUI on a dead terminal.
pub fn panic_is_expected() -> bool {
    EXPECTED.try_with(|c| c.get() > 0).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guard_marks_and_unmarks_the_thread() {
        assert!(!panic_is_expected());
        {
            let _g = ExpectedPanic::new();
            assert!(panic_is_expected());
        }
        assert!(!panic_is_expected());
    }

    #[test]
    fn guards_nest() {
        // `image_dispatch`'s guarded decode calls `render_mermaid_svg`,
        // which guards a `catch_unwind` of its own, so the inner guard
        // dropping must not clear the outer one.
        let outer = ExpectedPanic::new();
        {
            let _inner = ExpectedPanic::new();
            assert!(panic_is_expected());
        }
        assert!(panic_is_expected(), "the outer guard is still live");
        drop(outer);
        assert!(!panic_is_expected());
    }

    #[test]
    fn the_guard_survives_the_unwind_it_exists_for() {
        // The ordering that matters: the hook reads the flag before
        // unwinding, and `Drop` clears it during. After the catch the
        // thread must be unmarked again, or the *next* genuine panic on
        // this thread would be silently swallowed.
        let caught = {
            let _g = ExpectedPanic::new();
            std::panic::catch_unwind(|| {
                assert!(panic_is_expected(), "marked while inside");
                panic!("expected");
            })
        };
        assert!(caught.is_err());
        assert!(!panic_is_expected(), "unmarked once the guard is dropped");
    }

    #[test]
    fn a_hook_shaped_like_main_s_does_not_restore_for_a_guarded_panic() {
        // The defect itself, reproduced against the same branch `main`
        // installs. `restore()` cannot be called from a test (it would
        // scribble escape sequences at the test harness), so the effect
        // is stood in for by a flag — what is under test is the
        // *decision*, which is the half that was wrong: the hook ran
        // unconditionally and left the app running on a terminal handed
        // back to the shell.
        // The count is thread-local, not a static: the hook is
        // process-global and the suite runs tests in parallel, so a
        // `#[should_panic]` or `catch_unwind` test on another thread
        // would otherwise be counted as ours and make this flaky.
        thread_local! {
            static RESTORED: Cell<usize> = const { Cell::new(0) };
        }
        fn restored() -> usize {
            RESTORED.with(Cell::get)
        }

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {
            if panic_is_expected() {
                return;
            }
            let _ = RESTORED.try_with(|c| c.set(c.get() + 1));
        }));

        let guarded = {
            let _g = ExpectedPanic::new();
            std::panic::catch_unwind(|| panic!("caught"))
        };
        assert!(guarded.is_err());
        assert_eq!(
            restored(),
            0,
            "a caught panic must not tear the terminal down"
        );

        // ...and an unguarded panic still does, which is the whole point
        // of the hook and must not be lost to the fix.
        let bare = std::panic::catch_unwind(|| panic!("uncaught by intent"));
        assert!(bare.is_err());
        assert_eq!(
            restored(),
            1,
            "an unexpected panic must still restore the terminal"
        );

        std::panic::set_hook(previous);
    }

    #[test]
    fn the_flag_does_not_leak_to_another_thread() {
        // A process-global would let a guarded render-thread section
        // silence a genuine crash in a concurrently running worker.
        let _g = ExpectedPanic::new();
        assert!(panic_is_expected());
        let seen = std::thread::spawn(panic_is_expected).join().unwrap();
        assert!(!seen, "another thread must not inherit the guard");
    }
}
