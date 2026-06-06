//! Integration tests for the filesystem watcher subsystem.
//!
//! Covers the public surface exposed via `edamame::watcher`:
//! - `Debouncer` 200 ms sliding window.
//! - `NotifyWatcher` end-to-end with a real tempfile, including
//!   coalescing and `force_reconcile`.
//!
//! App-level hash-filter semantics are exercised by unit tests in
//! `src/app/file_changed.rs` because they need access to the
//! private `App::handle_file_changed` API.

use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use edamame::watcher::{Debouncer, FileWatcher, NotifyWatcher, WatchedChange, WatchedEvent};

/// Unwrap a `WatchedEvent::Change`; the integration tests below
/// only exercise the happy path and want a `WatchedChange` to
/// assert against.  A `ReadError` arriving where a `Change` is
/// expected panics with the offending path / error.
fn expect_change(ev: WatchedEvent) -> WatchedChange {
    match ev {
        WatchedEvent::Change(c) => c,
        WatchedEvent::Removed { path } => {
            panic!("expected Change, got Removed on {}", path.display())
        }
        WatchedEvent::ReadError { path, error } => {
            panic!(
                "expected Change, got ReadError on {}: {error}",
                path.display()
            )
        }
    }
}

#[test]
fn debouncer_window_default_is_idle() {
    let d = Debouncer::new(Duration::from_millis(200));
    assert!(d.deadline().is_none());
}

#[test]
fn debouncer_record_then_fire() {
    let mut d = Debouncer::new(Duration::from_millis(200));
    let t0 = Instant::now();
    d.record(t0);
    assert_eq!(d.deadline(), Some(t0 + Duration::from_millis(200)));
    assert!(!d.fire_if_due(t0 + Duration::from_millis(150)));
    assert!(d.fire_if_due(t0 + Duration::from_millis(200)));
    assert!(d.deadline().is_none(), "deadline cleared after firing");
}

#[test]
fn debouncer_burst_extends_deadline() {
    let mut d = Debouncer::new(Duration::from_millis(200));
    let t0 = Instant::now();
    d.record(t0);
    let t1 = t0 + Duration::from_millis(120);
    d.record(t1);
    // The early-arriving deadline does not fire because the later
    // record extended it.
    assert!(!d.fire_if_due(t0 + Duration::from_millis(200)));
    assert!(d.fire_if_due(t1 + Duration::from_millis(200)));
}

#[test]
fn notify_watcher_emits_debounced_change_on_external_write() {
    // End-to-end: create a temp file, watch it, mutate it, expect a
    // single change carrying the latest bytes.  Exact event count
    // is OS-dependent — the assertion is "at least one event with
    // the final contents."
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("file.md");
    std::fs::write(&path, "initial").expect("seed");

    let (tx, rx) = mpsc::channel::<WatchedEvent>();
    let mut w = NotifyWatcher::new(tx).expect("build watcher");
    w.watch(&path).expect("watch");
    // Let the backend install the watch before mutating; some
    // backends are racy with rapid setup-then-mutate sequences.
    std::thread::sleep(Duration::from_millis(80));

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .expect("open");
    write!(f, "updated").expect("write");
    drop(f);

    let change = expect_change(
        rx.recv_timeout(Duration::from_millis(1500))
            .expect("expected a debounced change"),
    );
    assert_eq!(change.path, path);
    assert_eq!(change.contents, "updated");
}

#[test]
fn force_reconcile_emits_change_synchronously() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("file.md");
    std::fs::write(&path, "alpha").expect("seed");

    let (tx, rx) = mpsc::channel::<WatchedEvent>();
    let mut w = NotifyWatcher::new(tx).expect("build watcher");
    w.watch(&path).expect("watch");

    // Drain any startup-time events the backend may synthesize so
    // they don't masquerade as the forced reconcile.
    std::thread::sleep(Duration::from_millis(80));
    while rx.try_recv().is_ok() {}

    std::fs::write(&path, "beta").expect("rewrite");
    w.force_reconcile().expect("reconcile");

    let change = expect_change(
        rx.recv_timeout(Duration::from_millis(500))
            .expect("forced reconcile must deliver promptly"),
    );
    assert_eq!(change.contents, "beta");
}

#[test]
fn unwatch_stops_event_delivery() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("file.md");
    std::fs::write(&path, "x").expect("seed");

    let (tx, rx) = mpsc::channel::<WatchedEvent>();
    let mut w = NotifyWatcher::new(tx).expect("build watcher");
    w.watch(&path).expect("watch");
    std::thread::sleep(Duration::from_millis(80));
    while rx.try_recv().is_ok() {}

    w.unwatch().expect("unwatch");
    std::thread::sleep(Duration::from_millis(20));

    std::fs::write(&path, "y").expect("rewrite");
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        rx.try_recv().is_err(),
        "no change should be delivered after unwatch",
    );
}

#[test]
fn rewatching_a_different_file_redirects_events() {
    // The watcher's public API explicitly allows `watch(path)` to
    // replace the current watch.  Verify that events for the
    // previous file are dropped and events for the new file are
    // delivered.
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a.md");
    let b = dir.path().join("b.md");
    std::fs::write(&a, "aa").expect("seed a");
    std::fs::write(&b, "bb").expect("seed b");

    let (tx, rx) = mpsc::channel::<WatchedEvent>();
    let mut w = NotifyWatcher::new(tx).expect("build watcher");
    w.watch(&a).expect("watch a");
    std::thread::sleep(Duration::from_millis(80));
    while rx.try_recv().is_ok() {}

    w.watch(&b).expect("swap to b");
    std::thread::sleep(Duration::from_millis(80));
    while rx.try_recv().is_ok() {}

    // A mutation on the old path must be ignored.
    std::fs::write(&a, "aa-changed").expect("write a");
    std::thread::sleep(Duration::from_millis(400));
    assert!(rx.try_recv().is_err(), "old-file events must be filtered");

    // A mutation on the new path must arrive.
    std::fs::write(&b, "bb-changed").expect("write b");
    let change = expect_change(
        rx.recv_timeout(Duration::from_millis(1500))
            .expect("new-file change must be delivered"),
    );
    assert_eq!(change.path, b);
    assert_eq!(change.contents, "bb-changed");
}
