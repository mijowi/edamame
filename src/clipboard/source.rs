//! The clipboard port: the one place an OS clipboard is touched.
//!
//! Shaped like [`crate::watcher::FileWatcher`] — the crate's other
//! substituted OS service — so a test can hand the app a clipboard whose
//! contents it wrote down instead of borrowing the developer's.

#[cfg(feature = "clipboard")]
use super::data::Bitmap;
use super::data::ClipboardData;

/// Reads the OS clipboard.
///
/// Snapshot-shaped rather than format-addressed on purpose: one `read`
/// is one open of the clipboard, so the payloads cannot come from
/// different moments — a format-addressed port would let a caller see a
/// file list and a bitmap that were never on the clipboard together.
pub trait ClipboardSource: Send {
    /// The clipboard's current contents.
    ///
    /// An unreachable or empty clipboard is
    /// [`ClipboardData::default`], never an error: every caller treats
    /// "nothing there" as an ordinary outcome, and the reason a read
    /// failed is not something the user can act on.
    fn read(&mut self) -> ClipboardData;
}

/// The real clipboard, via `arboard`.
#[cfg(feature = "clipboard")]
pub struct OsClipboard;

#[cfg(feature = "clipboard")]
impl ClipboardSource for OsClipboard {
    fn read(&mut self) -> ClipboardData {
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            return ClipboardData::default();
        };
        // The bitmap is read unconditionally — including when a file list
        // is already in hand and `image::paste::select` will not look at
        // it.  That costs one DIBV5 decode on the rare clipboard holding
        // both (an image viewer's copy, which also publishes the source
        // file); skipping it would move the priority order in here, where
        // it does not belong.
        let text = clipboard.get_text().ok();
        let files = clipboard.get().file_list().unwrap_or_default();
        let bitmap = clipboard.get_image().ok().map(|image| Bitmap {
            width: image.width as u32,
            height: image.height as u32,
            rgba: image.bytes.into_owned(),
        });
        ClipboardData {
            files,
            bitmap,
            text,
        }
    }
}

/// The clipboard of a build without the `clipboard` feature.
pub struct NullClipboard;

impl ClipboardSource for NullClipboard {
    fn read(&mut self) -> ClipboardData {
        ClipboardData::default()
    }
}

/// The source an `App` starts with.
///
/// Under `cfg(test)` this is [`NullClipboard`] even when the feature is
/// on: a test binary runs its tests on parallel threads against one
/// process-wide clipboard, so touching the real one would let one test's
/// copy land between another's copy and its paste, and would let the
/// developer's own clipboard leak into assertions.  The readers used to
/// repeat that guard at every call site; here it is one decision, and a
/// new payload kind cannot forget it.
///
/// Integration tests link the library compiled *without* `cfg(test)`, so
/// they get the real clipboard — the same split the old `OS_CLIPBOARD`
/// constant produced.
pub fn default_source() -> Box<dyn ClipboardSource> {
    #[cfg(all(feature = "clipboard", not(test)))]
    {
        Box::new(OsClipboard)
    }
    #[cfg(not(all(feature = "clipboard", not(test))))]
    {
        Box::new(NullClipboard)
    }
}
