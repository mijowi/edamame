//! `FileWatcher` trait + `NotifyWatcher` production implementation.
//!
//! A `notify::RecommendedWatcher` forwards events on a control mpsc to a worker thread that owns
//! the active path and a [`super::Debouncer`].  Every event resets the debounce window; when it
//! expires (or `force_reconcile` fires) the worker does the single disk read and pushes a
//! `WatchedChange`.  The main thread never reads the watched file, so a slow filesystem cannot
//! block the UI loop.
//!
//! **The watch is on the parent directory**, NonRecursive: an atomic-rename save replaces the
//! file's inode, which would lose an inotify watch on the file itself.  The worker filters events
//! back down to the target path.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::event::EventKind;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use super::Debouncer;

/// Quiet window: long enough to coalesce a typing-driven save, short enough to feel live.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(200);

/// A file-change notification.  The worker has already done the disk read, so the main thread
/// dispatches without further I/O.  Non-UTF-8 reads become [`WatchedEvent::ReadError`] instead.
#[derive(Debug, Clone)]
pub struct WatchedChange {
    pub path: PathBuf,
    pub contents: String,
}

/// Events surfaced on the worker → main channel, all three decided by the same post-debounce
/// read in [`do_read_and_send`].  `Removed` is split out from `ReadError` so the App can offer to
/// re-save the buffer.
///
/// Deciding it at *read* time is what makes it accurate: an atomic-rename save emits a `Remove`
/// followed by a `Create`, so by the time the debounce window fires the file exists again and
/// reads as `Change`.  Only a genuine deletion is still missing.
#[derive(Debug, Clone)]
pub enum WatchedEvent {
    Change(WatchedChange),
    Removed { path: PathBuf },
    ReadError { path: PathBuf, error: String },
}

/// Abstract watcher interface.  One impl ships today ([`NotifyWatcher`]); the trait exists so a
/// future multi-tab refactor can swap in a per-tab map without touching the App's call sites.
pub trait FileWatcher: Send {
    /// Begin (or replace) the active watch.  Idempotent: if `path`
    /// is the current watch this is a no-op.
    fn watch(&mut self, path: &Path) -> Result<()>;
    /// Stop the active watch.  Subsequent disk changes are not
    /// reported until the next `watch` call.
    fn unwatch(&mut self) -> Result<()>;
    /// Request a one-shot read of the active path (on the worker thread), bypassing the debounce
    /// window.  Used by the external-editor flow on resume to pick up a change that arrived while
    /// the watcher was paused.
    ///
    /// `&self` rather than `&mut self`: only `cmd_tx.send` is needed, so this stays callable from
    /// a context holding a shared reference.
    fn force_reconcile(&self) -> Result<()>;
}

/// Worker commands.  The notify callback emits `Event`; the watcher's own methods emit the rest.
enum WorkerCommand {
    Event(notify::Event),
    SetPath(PathBuf),
    Clear,
    Reconcile,
    Shutdown,
}

/// Production [`FileWatcher`]: a [`notify::RecommendedWatcher`] plus a worker thread owning the
/// debouncer and the active path.
pub struct NotifyWatcher {
    inner: RecommendedWatcher,
    /// The directory passed to `inner.watch`, kept so `unwatch` can pair the calls and `watch`
    /// can drop a stale parent watch before registering a new one.
    watched_dir: Option<PathBuf>,
    /// The file being watched conceptually; the parent dir is what notify is given.
    current_path: Option<PathBuf>,
    cmd_tx: mpsc::Sender<WorkerCommand>,
    worker: Option<JoinHandle<()>>,
}

impl NotifyWatcher {
    /// Build a watcher whose `WatchedEvent`s flow to `event_tx`.  Spawns the worker immediately;
    /// the watcher is idle until [`Self::watch`] is called.
    pub fn new(event_tx: mpsc::Sender<WatchedEvent>) -> Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>();
        let worker = std::thread::Builder::new()
            .name("edamame-watcher".to_owned())
            .spawn(move || worker_loop(cmd_rx, event_tx, DEBOUNCE_WINDOW))
            .context("failed to spawn watcher worker")?;

        // The callback runs on notify's own thread and takes a clone of the sender.  Send
        // failures are swallowed: losing one event is recoverable, but panicking here would tear
        // down the notify thread.
        let cb_tx = cmd_tx.clone();
        let inner =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(ev) => {
                    // `Remove` is forwarded too; the post-debounce read tells a real deletion
                    // from an atomic-rename replace (see `WatchedEvent`).
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
        // A missing parent (root path, current dir) defaults to ".".
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
        let parent_owned: PathBuf = parent
            .map(Path::to_owned)
            .unwrap_or_else(|| PathBuf::from("."));

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
            // Best-effort join; the worker exits on Shutdown or channel disconnect.
            let _ = worker.join();
        }
    }
}

/// Worker thread main loop: drives the debouncer and reads when the window elapses or a forced
/// reconcile fires.
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

/// True iff `event` reports a change to `target`.
fn event_matches_path(event: &notify::Event, target: &Path) -> bool {
    if event.paths.iter().any(|p| p == target) {
        return true;
    }
    // Filename fallback, for backends that drop the directory prefix.
    let Some(target_name) = target.file_name() else {
        return false;
    };
    event
        .paths
        .iter()
        .any(|p| p.file_name() == Some(target_name))
}

/// Read `path` and push the result onto `event_tx`.  `NotFound` becomes
/// [`WatchedEvent::Removed`] so the App can offer to re-save the buffer; every other failure
/// (non-UTF-8, permission denied, …) becomes a [`WatchedEvent::ReadError`] rather than being
/// dropped in the worker log.
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
    //! The two tests that wait on an organic notify event are `#[ignore]`d, like their
    //! counterparts in `tests/watcher.rs` — see that file's module docs.  The rest reach the same
    //! post-debounce read through `force_reconcile`, without needing the OS to speak first.

    use std::io::Write;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// Unwrap a `WatchedEvent::Change`, panicking usefully on the other variants.
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

        // Let notify install the watch first, or the write races the registration.
        std::thread::sleep(Duration::from_millis(80));

        std::fs::write(&path, "updated").expect("rewrite");

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

        // Drain any startup-time events so the reconcile read isn't confused with an organic one.
        std::thread::sleep(Duration::from_millis(80));
        while rx.try_recv().is_ok() {}

        // Reconcile *before* the debounce window elapses, proving it bypasses the window.
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
        // Exact event count is platform-dependent; the invariant is only that the *last*
        // change delivered carries the final contents.  A `truncate`-then-`write` is two
        // steps, so a debounced read can catch the file empty in between — and under CI
        // scheduling pressure the window can outlast the debounce.  Draining to the last
        // change sidesteps that: once the burst settles, the file is quiescent at "5", so
        // the final read always sees it.  Any earlier empty/intermediate read is discarded.
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

        // The first change may arrive slowly on a loaded runner; give it a wide window,
        // then keep collecting until the stream stays quiet past one debounce window.
        let mut last = expect_change(
            rx.recv_timeout(Duration::from_millis(1500))
                .expect("at least one event after the burst"),
        );
        while let Ok(ev) = rx.recv_timeout(Duration::from_millis(400)) {
            last = expect_change(ev);
        }
        assert_eq!(last.contents, "5", "final contents must win");
    }

    #[test]
    fn read_error_is_surfaced_on_invalid_utf8() {
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
