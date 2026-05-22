//! 200 ms quiet-window debouncer used by the watcher worker.
//!
//! Pure state machine over wall-clock instants — no I/O.  The worker
//! calls [`Debouncer::record`] on every incoming notify event and
//! consults [`Debouncer::deadline`] to compute the next channel
//! `recv_timeout`; when that timeout fires, [`Debouncer::fire_if_due`]
//! returns true and the worker performs a single disk read.
//!
//! Splitting the debouncer out keeps the timing logic testable
//! without standing up a notify backend.

use std::time::{Duration, Instant};

pub struct Debouncer {
    window: Duration,
    pending_since: Option<Instant>,
}

impl Debouncer {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            pending_since: None,
        }
    }

    /// Record that an event arrived at `now`.  Sliding window: a fresh
    /// event during an existing pending interval restarts the timer,
    /// so a rapid burst of events fires exactly once after the burst
    /// finishes.
    pub fn record(&mut self, now: Instant) {
        self.pending_since = Some(now);
    }

    /// The instant at which the pending fire is due, if any.  Used by
    /// the worker to set the channel `recv_timeout`.
    pub fn deadline(&self) -> Option<Instant> {
        self.pending_since.map(|t| t + self.window)
    }

    /// True when a fire is pending (regardless of whether the
    /// deadline has elapsed yet).
    #[allow(dead_code)] // used by tests and by future CP3+ work
    pub fn is_pending(&self) -> bool {
        self.pending_since.is_some()
    }

    /// If a fire is pending and `now` has reached the deadline,
    /// clear the pending state and return `true`.  Otherwise return
    /// `false` without mutating state.
    pub fn fire_if_due(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.deadline() else {
            return false;
        };
        if now >= deadline {
            self.pending_since = None;
            true
        } else {
            false
        }
    }

    /// Drop any pending fire without firing.  Used when the worker
    /// performs a forced reconcile or unwatch.
    pub fn clear(&mut self) {
        self.pending_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Duration {
        Duration::from_millis(200)
    }

    #[test]
    fn fresh_debouncer_is_idle() {
        let d = Debouncer::new(window());
        assert!(!d.is_pending());
        assert!(d.deadline().is_none());
    }

    #[test]
    fn record_arms_the_deadline() {
        let mut d = Debouncer::new(window());
        let t0 = Instant::now();
        d.record(t0);
        assert!(d.is_pending());
        assert_eq!(d.deadline(), Some(t0 + window()));
    }

    #[test]
    fn fire_if_due_returns_false_before_deadline() {
        let mut d = Debouncer::new(window());
        let t0 = Instant::now();
        d.record(t0);
        assert!(!d.fire_if_due(t0 + Duration::from_millis(50)));
        assert!(d.is_pending(), "still pending below deadline");
    }

    #[test]
    fn fire_if_due_clears_state_at_deadline() {
        let mut d = Debouncer::new(window());
        let t0 = Instant::now();
        d.record(t0);
        assert!(d.fire_if_due(t0 + window()));
        assert!(!d.is_pending());
        assert!(d.deadline().is_none());
    }

    #[test]
    fn record_during_pending_extends_window() {
        // Sliding window: a burst of events keeps pushing the
        // deadline forward, so the fire only happens after the
        // burst finishes.
        let mut d = Debouncer::new(window());
        let t0 = Instant::now();
        d.record(t0);
        let t1 = t0 + Duration::from_millis(100);
        d.record(t1);
        // The original deadline has not yet elapsed, but the new one
        // is later — verify the slide.
        assert_eq!(d.deadline(), Some(t1 + window()));
        assert!(!d.fire_if_due(t0 + window()));
    }

    #[test]
    fn clear_discards_pending_state() {
        let mut d = Debouncer::new(window());
        d.record(Instant::now());
        d.clear();
        assert!(!d.is_pending());
        assert!(d.deadline().is_none());
    }

    #[test]
    fn fire_if_due_on_idle_is_a_noop() {
        let mut d = Debouncer::new(window());
        assert!(!d.fire_if_due(Instant::now()));
        assert!(!d.is_pending());
    }
}
