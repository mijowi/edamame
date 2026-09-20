//! The clipboard payload model.
//!
//! One snapshot of the OS clipboard, expressed in the terms every
//! platform can offer rather than in any one platform's formats.  The
//! adapters in [`super::source`] do the mapping: Windows' `CF_HDROP`,
//! macOS' file-URL pasteboard type and X11's `text/uri-list` all become
//! [`ClipboardData::files`]; a `CF_DIBV5` bitmap and a PNG pasteboard
//! entry both become [`ClipboardData::bitmap`].
//!
//! Nothing above this module names a clipboard *format*, which is what
//! lets the paste policy stay platform-neutral — and testable, since a
//! snapshot is an ordinary value a test can write down.

use std::path::PathBuf;

/// Raw RGBA pixels, as the OS hands them over.
///
/// Every clipboard normalizes: Windows publishes a device-independent
/// bitmap, macOS a TIFF or PNG, and `arboard` decodes either into RGBA8
/// for us.  There is no "original encoding" to preserve — a screenshot
/// is pixels, and a lossless PNG is the only faithful thing edamame can
/// write for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Everything one clipboard read produced.
///
/// `Default` is the empty clipboard — the shape a test writes down when
/// it wants to say "the user copied nothing".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipboardData {
    /// Files copied in a file manager.
    ///
    /// Windows: `CF_HDROP`.  macOS: `public.file-url`.  X11/Wayland:
    /// `text/uri-list`.  On Windows this is the *only* thing a file copy
    /// publishes — measured on Windows 11 (build 26200), Explorer's
    /// `Ctrl+C` leaves `CF_HDROP` plus shell-private formats and no text
    /// format at all, so a reader that only asks for text sees an empty
    /// clipboard.
    pub files: Vec<PathBuf>,
    /// A bitmap, e.g. a screenshot.
    pub bitmap: Option<Bitmap>,
    /// Plain text.
    pub text: Option<String>,
}
