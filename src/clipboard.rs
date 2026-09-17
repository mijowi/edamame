//! OS clipboard access, behind one substitutable port.
//!
//! The clipboard is the second OS service edamame touches directly (the
//! file watcher is the other), and the same problem applies to both: a
//! module that calls the OS itself cannot be tested, so it ends up
//! guarded off wholesale under `cfg(test)` and the feature it serves has
//! no coverage at all.  Behind [`ClipboardSource`] the real clipboard is
//! one implementation among several:
//!
//! - [`OsClipboard`] — the real thing, via `arboard`, plus OSC 52 on writes.
//! - [`TerminalClipboard`] — a build without the `clipboard` feature: OSC 52 writes only.
//! - [`NullClipboard`] — what an `App` starts with until `main` installs [`default_source`].
//! - a test's own source — contents it wrote down.
//!
//! Nothing here interprets what it reads.  Turning a bitmap into a stored
//! image is [`crate::image::paste`]'s job, and the app is the only thing
//! that decides which payload it wants.

pub mod data;
pub mod osc52;
pub mod source;

pub use data::Bitmap;
pub use osc52::osc52_copy;
pub use source::{
    default_source, BitmapReader, ClipboardSource, NullClipboard, TerminalClipboard, TextRead,
};

#[cfg(feature = "clipboard")]
pub use source::OsClipboard;
