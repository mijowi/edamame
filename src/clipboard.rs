//! OS clipboard access, behind one substitutable port.
//!
//! The clipboard is the second OS service edamame reads directly (the
//! file watcher is the other), and the same problem applies to both: a
//! module that calls the OS itself cannot be tested, so it ends up
//! guarded off wholesale under `cfg(test)` and the feature it serves has
//! no coverage at all.  Behind [`ClipboardSource`] the real clipboard is
//! one implementation among several:
//!
//! - [`OsClipboard`] — the real thing, via `arboard`.
//! - [`NullClipboard`] — a build without the `clipboard` feature.
//! - a test's own source — a snapshot it wrote down.
//!
//! Nothing here interprets what it reads.  Turning a snapshot into an
//! image reference is [`crate::image::paste`]'s job, and the app is the
//! only thing that decides which of the two it wants.

pub mod data;
pub mod source;

pub use data::{Bitmap, ClipboardData};
pub use source::{default_source, ClipboardSource, NullClipboard};

#[cfg(feature = "clipboard")]
pub use source::OsClipboard;
