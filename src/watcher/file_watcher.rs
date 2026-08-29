//! `FileWatcher` trait + `NotifyWatcher` production implementation.
//!
//! Architecture (single-file mode):
//! - The watcher owns a `notify::RecommendedWatcher` whose event
//!   callback forwards `Modify` / `Create` events on a control mpsc.
//! - A dedicated worker thread holds the control mpsc, the active
//!   path, and a [`super::Debouncer`].  Every organic event resets the
//!   debounce window; when the window expires (or `force_reconcile`
//!   fires), the worker performs the single disk read and pushes a
//!   `WatchedChange` onto the caller-supplied channel.
//! - The main thread never reads from disk for the watched file.  The
//!   worker is the single owner of disk I/O so a slow filesystem can
//!   never block the UI loop.
//!
//! Why we watch the parent directory: editors that save via
//! atomic-rename replace the file's inode.  Watching the file
//! directly via inotify loses the watch when the inode changes.
//! Watching the parent dir at NonRecursive depth catches both
//! in-place writes and atomic-replace saves; the worker filters
//! events back down to the target path by matching `event.paths`.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::event::EventKind;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use super::Debouncer;

/// 200 ms quiet-window default.  The exact value matches every other
/// "modern editor" — long enough to coalesce a typing-driven save
/// (most editors batch over ~50 ms) and short enough that an external
/// change feels live.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(200);

/// A file-change notification produced by the watcher worker.  The
/// worker has already done the disk read so the main thread can
/// dispatch immediately without further I/O.  `contents` is the
/// UTF-8 file body — non-UTF-8 reads are reported as
/// [`WatchedEvent::ReadError`] instead.
#[derive(Debug, Clone)]
pub struct WatchedChange {
    pub path: PathBuf,
    pub contents: String,
}

/// Events surfaced on the worker → main channel.  `Change` is the
/// happy path; `Removed` signals the watched file no longer exists at
/// read time; `ReadError` is delivered when the read failed for any
/// other reason.
///
/// `Removed` and `ReadError` are both produced by the same
/// post-debounce read in [`do_read_and_send`].  The deletion case is
/// split out so the App can offer to re-save the buffer rather than
/// just flashing a generic "could not read" warning.  Crucially, this
/// is decided at *read* time, not when the raw `notify` event arrives:
/// atomic-rename saves (the reason we watch the parent directory) emit
/// a `Remove` of the old inode immediately followed by a `Create`, so
/// by the time the 200 ms debounce window fires the file exists again
/// and reads normally as `Change`.  Only a genuine deletion with no
/// recreate is still missing at read time and surfaces as `Removed`.
#[derive(Debug, Clone)]
pub enum WatchedEvent {
    Change(WatchedChange),
    Removed { path: PathBuf },
    ReadError { path: PathBuf, error: String },
}

/// Abstract watcher interface.  One impl ships today
/// ([`NotifyWatcher`]); the trait is here so a future multi-tab
/// refactor can swap `Option<Box<dyn FileWatcher>>` for a per-tab
/// map without touching the App's call sites.
pub trait FileWatcher: Send {
    /// Begin (or replace) the active watch.  Idempotent: if `path`
    /// is the current watch this is a no-op.
    fn watch(&mut self, path: &Path) -> Result<()>;
    /// Stop the active watch.  Subsequent disk changes are not
    /// reported until the next `watch` call.
    fn unwatch(&mut self) -> Result<()>;
    /// Request a one-shot read of the active path, bypassing the
    /// debounce window.  The worker thread performs the read off
    /// the main thread.  Used by the external-editor flow on resume
    /// (and, later, the post-diff-resolution requeue) to pick up
    /// any change that arrived while the watcher was paused.
    ///
    /// Takes `&self` rather than `&mut self` because the underlying
    /// `cmd_tx.send` is `&self`; the other trait methods need
    /// `&mut self` so they can call `notify::Watcher::watch` /
    /// `unwatch`, which are themselves `&mut self`.  The asymmetry
    /// is intentional and lets `force_reconcile` be called from
    /// contexts (e.g. a future shared-Arc model) that only hold a
    /// shared reference.
    fn force_reconcile(&self) -> Result<()>;
}

/// Worker commands routed through the control channel.  The notify
/// callback emits `Event`; the watcher's own methods emit
/// `SetPath` / `Clear` / `Reconcile`.
enum WorkerCommand {
    Event(notify::Event),
    SetPath(PathBuf),
    Clear,
    Reconcile,
    Shutdown,
}

/// Production [`FileWatcher`].  Wraps [`notify::RecommendedWatcher`]
/// and spawns a worker thread that owns the debouncer and the
/// active path.
pub struct NotifyWatcher {
    inner: RecommendedWatcher,
    /// The directory currently passed to `inner.watch`.  Stored so
    /// `unwatch` can pair the calls correctly and `watch` can drop
    /// a stale parent watch before re-registering on a new one.
    watched_dir: Option<PathBuf>,
    /// The file path we are conceptually watching — the parent dir
    /// is what we actually hand to notify.
    current_path: Option<PathBuf>,
    cmd_tx: mpsc::Sender<WorkerCommand>,
    worker: Option<JoinHandle<()>>,
}

impl NotifyWatcher {
    /// Build a new watcher whose `WatchedEvent`s flow to `event_tx`.
    /// Spawns the worker thread immediately; the returned watcher is
    /// idle until [`Self::watch`] is called.
    pub fn new(event_tx: mpsc::Sender<WatchedEvent>) -> Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>();
        let worker = std::thread::Builder::new()
            .name("edamame-watcher".to_owned())
            .spawn(move || worker_loop(cmd_rx, event_tx, DEBOUNCE_WINDOW))
            .context("failed to spawn watcher worker")?;

        // The notify callback runs on notify's internal thread; it
        // can't borrow `self`, so we hand it a clone of the cmd
        // sender.  Failures are logged but not propagated — losing
        // a notify event is recoverable (next event will trigger
        // a reconcile); panicking the callback would tear down the
        // notify thread.
        let cb_tx = cmd_tx.clone();
        let inner =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(ev) => {
                    // `Remove` is forwarded alongside `Modify` / `Create`
                    // so a deletion records into the debouncer like any
                    // other event; the worker's post-debounce read then
                    // distinguishes a genuine deletion from an
                    // atomic-rename replace (see [`WatchedEvent`]).
                    if matches!(
                        ev.kind,
                        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                    ) {
                        let _ = cb_tx.send(WorkerCommand::Event(ev));
                    }
                }
                Err(err) => {
                    tracing::warn!(target: "watcher", error = %err, "notify error");
                }
            })
            .context("failed to construct notify watcher")?;

        Ok(Self {
            inner,
            watched_dir: None,
            current_path: None,
            cmd_tx,
            worker: Some(worker),
        })
    }
}

impl FileWatcher for NotifyWatcher {
    fn watch(&mut self, path: &Path) -> Result<()> {
        if self.current_path.as_deref() == Some(path) {
            return Ok(());
        }
        // Resolve the parent directory.  A missing parent (root
        // path, current dir) defaults to ".".  We use NonRecursive
        // so we only receive events for files directly inside the
        // dir — avoids noise from sibling subdirectories.
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
        let parent_owned: PathBuf = parent
            .map(Path::to_owned)
            .unwrap_or_else(|| PathBuf::from("."));

        // Drop the previous watch (if any) before adding the new
        // one.  notify dedups identical watch calls but it's safer
        // to be explicit about lifecycle.
        if let Some(prev) = self.watched_dir.take() {
            let _ = self.inner.unwatch(&prev);
        }
        self.inner
            .watch(&parent_owned, RecursiveMode::NonRecursive)
            .with_context(|| {
                format!(
                    "failed to watch {} (for file {})",
                    parent_owned.display(),
                    path.display(),
                )
            })?;
        self.watched_dir = Some(parent_owned);
        self.current_path = Some(path.to_owned());
        let _ = self.cmd_tx.send(WorkerCommand::SetPath(path.to_owned()));
        Ok(())
    }

    fn unwatch(&mut self) -> Result<()> {
        self.current_path = None;
        let _ = self.cmd_tx.send(WorkerCommand::Clear);
        if let Some(prev) = self.watched_dir.take() {
            let _ = self.inner.unwatch(&prev);
        }
        Ok(())
    }

    fn force_reconcile(&self) -> Result<()> {
        let _ = self.cmd_tx.send(WorkerCommand::Reconcile);
        Ok(())
    }
}

impl Drop for NotifyWatcher {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(WorkerCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            // Best-effort join — the worker exits as soon as it sees
            // Shutdown or its channel disconnect.  Ignore any
            // panic propagation; tear-down is async.
            let _ = worker.join();
        }
    }
}

/// Worker thread main loop.  Consumes [`WorkerCommand`]s, drives
/// the debouncer, and pushes a [`WatchedChange`] when the window
/// elapses (or a forced reconcile fires).
fn worker_loop(
    cmd_rx: mpsc::Receiver<WorkerCommand>,
    event_tx: mpsc::Sender<WatchedEvent>,
    window: Duration,
) {
    let mut current_path: Option<PathBuf> = None;
    let mut debouncer = Debouncer::new(window);

    loop {
        let recv_result = match debouncer.deadline() {
            Some(deadline) => {
                let now = Instant::now();
                let remaining = deadline.saturating_duration_since(now);
                cmd_rx.recv_timeout(remaining)
            }
            None => cmd_rx
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };

        match recv_result {
            Ok(WorkerCommand::SetPath(p)) => {
                current_path = Some(p);
                debouncer.clear();
            }
            Ok(WorkerCommand::Clear) => {
                current_path = None;
                debouncer.clear();
            }
            Ok(WorkerCommand::Event(ev)) => {
                let Some(path) = current_path.as_ref() else {
                    continue;
                };
                if event_matches_path(&ev, path) {
                    debouncer.record(Instant::now());
                }
            }
            Ok(WorkerCommand::Reconcile) => {
                debouncer.clear();
                if let Some(path) = current_path.clone() {
                    do_read_and_send(&path, &event_tx);
                }
            }
            Ok(WorkerCommand::Shutdown) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if debouncer.fire_if_due(Instant::now()) {
                    if let Some(path) = current_path.clone() {
                        do_read_and_send(&path, &event_tx);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// True iff `event` reports a change to `target`.  notify reports
/// paths as absolute (or as-supplied), so we compare both as-is and
/// against the canonical form when available — atomic-replace flows
/// occasionally report the parent-relative form on macOS.
fn event_matches_path(event: &notify::Event, target: &Path) -> bool {
    if event.paths.iter().any(|p| p == target) {
        return true;
    }
    // Filename match as a fallback: the notify backend on Linux
    // reports paths as parent/filename, which always matches above.
    // This guards against backends that drop the dir prefix.
    let Some(target_name) = target.file_name() else {
        return false;
    };
    event
        .paths
        .iter()
        .any(|p| p.file_name() == Some(target_name))
}

/// Read `path` from disk and push the result onto `event_tx`.  A
/// missing file (`ErrorKind::NotFound`) is reported as
/// [`WatchedEvent::Removed`] so the App can offer to re-save the
/// buffer; every other failure (non-UTF-8 contents, permission
/// denied, …) becomes a [`WatchedEvent::ReadError`] so the App can
/// surface it instead of silently dropping it in the worker log.
fn do_read_and_send(path: &Path, event_tx: &mpsc::Sender<WatchedEvent>) {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let _ = event_tx.send(WatchedEvent::Change(WatchedChange {
                path: path.to_owned(),
                contents,
            }));
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {
            tracing::info!(
                target: "watcher",
                path = %path.display(),
                "watched file no longer exists",
            );
            let _ = event_tx.send(WatchedEvent::Removed {
                path: path.to_owned(),
            });
        }
        Err(err) => {
            tracing::warn!(
                target: "watcher",
                path = %path.display(),
                error = %err,
                "failed to read watched file",
            );
            let _ = event_tx.send(WatchedEvent::ReadError {
                path: path.to_owned(),
                error: err.to_string(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    //! The two tests below that wait on an organic notify event are
    //! `#[ignore]`d, as are their two counterparts in
    //! `tests/watcher.rs` — see that file's module docs for why, and
    //! for the command CI runs them under. The rest reach the
    //! post-debounce read through `force_reconcile`, so they assert
    //! the same delivery path without depending on the OS to speak
    //! first.

    use std::io::Write;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// Unwrap a `WatchedEvent::Change` for assertions; panic with a
    /// useful message if a `ReadError` arrives instead (these tests
    /// only exercise the happy path).
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
    #[ignore = "requires live filesystem notifications (inotify/FSEvents)"]
    fn watcher_emits_change_on_external_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.md");
        std::fs::write(&path, "initial").expect("seed");

        let (tx, rx) = mpsc::channel::<WatchedEvent>();
        let mut w = NotifyWatcher::new(tx).expect("build watcher");
        w.watch(&path).expect("watch");

        // Give notify a moment to install the inotify watch before
        // the first mutation — without this, the test is flaky on
        // fast machines because the write races the watch
        // registration.
        std::thread::sleep(Duration::from_millis(80));

        std::fs::write(&path, "updated").expect("rewrite");

        // 200 ms debounce + a generous slop for CI.  notify's
        // inotify backend usually delivers within 5 ms, so a 600 ms
        // ceiling is far in excess of typical latency without making
        // the failure mode slow.
        let change = expect_change(
            rx.recv_timeout(Duration::from_millis(1500))
                .expect("expected a debounced change"),
        );
        assert_eq!(change.path, path);
        assert_eq!(change.contents, "updated");
    }

    #[test]
    fn force_reconcile_emits_without_filesystem_event() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.md");
        std::fs::write(&path, "alpha").expect("seed");

        let (tx, rx) = mpsc::channel::<WatchedEvent>();
        let mut w = NotifyWatcher::new(tx).expect("build watcher");
        w.watch(&path).expect("watch");

        // Briefly drain any startup-time events the backend might
        // synthesize so the reconcile read isn't confused with an
        // organic notify event.
        std::thread::sleep(Duration::from_millis(80));
        while rx.try_recv().is_ok() {}

        // Mutate the file but trigger reconcile *before* the
        // 200 ms debounce window — proves that reconcile bypasses
        // the window.
        std::fs::write(&path, "beta").expect("rewrite");
        w.force_reconcile().expect("reconcile");

        let change = expect_change(
            rx.recv_timeout(Duration::from_millis(500))
                .expect("forced reconcile must deliver synchronously"),
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
        // Give the worker a moment to process the Clear command.
        std::thread::sleep(Duration::from_millis(20));

        std::fs::write(&path, "y").expect("rewrite");
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            rx.try_recv().is_err(),
            "no change should be delivered after unwatch"
        );
    }

    #[test]
    #[ignore = "requires live filesystem notifications (inotify/FSEvents)"]
    fn rapid_writes_coalesce_into_a_single_change() {
        // Five writes inside the 200 ms window should produce one
        // change carrying the final contents.  Exact event count is
        // platform-dependent — the assertion is "at least one" so
        // the test isn't flaky.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.md");
        std::fs::write(&path, "0").expect("seed");

        let (tx, rx) = mpsc::channel::<WatchedEvent>();
        let mut w = NotifyWatcher::new(tx).expect("build watcher");
        w.watch(&path).expect("watch");
        std::thread::sleep(Duration::from_millis(80));
        while rx.try_recv().is_ok() {}

        for i in 1..=5 {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .expect("open");
            write!(f, "{i}").expect("write");
            std::thread::sleep(Duration::from_millis(20));
        }

        let first = expect_change(
            rx.recv_timeout(Duration::from_millis(1500))
                .expect("at least one event after the burst"),
        );
        assert_eq!(first.contents, "5", "final contents must win");
    }

    #[test]
    fn read_error_is_surfaced_on_invalid_utf8() {
        // A file whose bytes are not valid UTF-8 yields ReadError
        // rather than silently dropping the event.  The watcher's
        // `force_reconcile` path drives the read synchronously so
        // the test does not depend on inotify timing.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.md");
        std::fs::write(&path, b"\xff\xfe not utf-8").expect("seed");

        let (tx, rx) = mpsc::channel::<WatchedEvent>();
        let mut w = NotifyWatcher::new(tx).expect("build watcher");
        w.watch(&path).expect("watch");
        std::thread::sleep(Duration::from_millis(80));
        while rx.try_recv().is_ok() {}

        w.force_reconcile().expect("reconcile");
        let ev = rx
            .recv_timeout(Duration::from_millis(500))
            .expect("read error must be delivered");
        match ev {
            WatchedEvent::ReadError { path: p, .. } => assert_eq!(p, path),
            WatchedEvent::Removed { path: p } => {
                panic!("expected ReadError, got Removed on {}", p.display())
            }
            WatchedEvent::Change(c) => {
                panic!(
                    "expected ReadError, got Change with {} bytes",
                    c.contents.len()
                )
            }
        }
    }

    #[test]
    fn watcher_emits_removed_on_deletion() {
        // Deleting the watched file (with no recreate) yields a
        // `Removed` event, distinct from `ReadError`.  Driven through
        // `force_reconcile` so the test does not depend on inotify
        // delivery timing for the deletion event itself.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.md");
        std::fs::write(&path, "doomed").expect("seed");

        let (tx, rx) = mpsc::channel::<WatchedEvent>();
        let mut w = NotifyWatcher::new(tx).expect("build watcher");
        w.watch(&path).expect("watch");
        std::thread::sleep(Duration::from_millis(80));
        while rx.try_recv().is_ok() {}

        std::fs::remove_file(&path).expect("delete");
        w.force_reconcile().expect("reconcile");

        let ev = rx
            .recv_timeout(Duration::from_millis(500))
            .expect("removal must be delivered");
        match ev {
            WatchedEvent::Removed { path: p } => assert_eq!(p, path),
            other => panic!("expected Removed, got {other:?}"),
        }
    }
}
