//! Filesystem watcher subsystem.
//!
//! `FileWatcher` is the small trait the App holds (`Option<Box<dyn
//! FileWatcher>>`) so a future multi-tab refactor can swap it for a
//! per-tab map.  `NotifyWatcher` is the production implementation
//! backed by [`notify`]; the events it produces are coalesced through
//! a 200 ms debouncer before the worker reads the file and pushes a
//! single [`crate::app::AppEvent::Watcher`] onto the main mpsc.
//!
//! The watcher worker thread is the single owner of disk reads for
//! the watched file: organic events, the debounce timer, and the
//! external-editor `force_reconcile` path all funnel through the same
//! `do_read_and_send` codepath so the main thread never blocks on
//! disk I/O.

pub mod debounce;
pub mod file_watcher;

pub use debounce::Debouncer;
pub use file_watcher::{FileWatcher, NotifyWatcher, WatchedChange, WatchedEvent};
