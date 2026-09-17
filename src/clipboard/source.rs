//! The clipboard port: the one place an OS clipboard is touched.
//!
//! Shaped like [`crate::watcher::FileWatcher`] — the crate's other
//! substituted OS service — so a test can hand the app a clipboard whose
//! contents it wrote down instead of borrowing the developer's.

use super::data::Bitmap;

/// Reads the clipboard's bitmap; built on the UI thread, run on a worker.  See
/// [`ClipboardSource::bitmap_reader`].
pub type BitmapReader = Box<dyn FnOnce() -> Option<Bitmap> + Send>;

/// What [`ClipboardSource::read_text`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextRead {
    /// The clipboard's text, possibly empty.
    Text(String),
    /// The clipboard is reachable but holds no text: it may hold a bitmap instead.
    NoText,
    /// No clipboard could be reached — no display server over SSH, or Wayland without
    /// data-control — so it holds no bitmap either, and a paste need not look for one.
    Unreachable,
}

/// Reads and writes the clipboard.
///
/// Reads are split by cost.  [`Self::read_text`] is cheap, so a paste makes it on the UI thread.
/// A bitmap read decodes the pixels, which for a large screenshot takes long enough to freeze
/// the screen, so [`Self::bitmap_reader`] only hands back a reader for a worker thread to run.
/// Neither read is atomic with the other: the clipboard can change between them.
///
/// A read never errors: every caller treats "nothing there" as an ordinary outcome, and the
/// reason a read failed is not something the user can act on.  [`TextRead`] only tells an
/// unreachable clipboard from one without text, since only the latter is worth a bitmap read.
pub trait ClipboardSource: Send {
    /// The clipboard's text, or why there is none.
    fn read_text(&mut self) -> TextRead;

    /// Whether this source can ever hold a bitmap.  `false` gates image paste off entirely, so a
    /// plain paste goes straight to the kill-ring without a worker round-trip.
    fn can_read_bitmaps(&self) -> bool;

    /// A reader for the clipboard's bitmap, e.g. a screenshot, to run off the UI thread.
    fn bitmap_reader(&mut self) -> BitmapReader;

    /// Put `text` on the clipboard, best-effort, for the same reason a read cannot fail.
    fn write_text(&mut self, text: String);
}

/// The real clipboard, via `arboard`, plus the terminal's (OSC 52) on every write.
#[cfg(feature = "clipboard")]
pub struct OsClipboard;

#[cfg(feature = "clipboard")]
impl ClipboardSource for OsClipboard {
    fn read_text(&mut self) -> TextRead {
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            return TextRead::Unreachable;
        };
        clipboard
            .get_text()
            .map_or(TextRead::NoText, TextRead::Text)
    }

    fn can_read_bitmaps(&self) -> bool {
        true
    }

    fn bitmap_reader(&mut self) -> BitmapReader {
        Box::new(|| {
            let image = arboard::Clipboard::new().ok()?.get_image().ok()?;
            Some(Bitmap {
                width: image.width as u32,
                height: image.height as u32,
                rgba: image.bytes.into_owned(),
            })
        })
    }

    fn write_text(&mut self, text: String) {
        super::osc52_copy(&text);
        write_os_text(text);
    }
}

/// Linux: Wayland/X11 hold clipboard data only while a process owns the
/// selection, and `arboard` prints to *stderr* — corrupting the TUI — if
/// the `Clipboard` drops too soon after setting it; hence a thread that
/// owns the selection until another program takes over.
#[cfg(all(feature = "clipboard", target_os = "linux"))]
fn write_os_text(text: String) {
    use arboard::SetExtLinux;
    std::thread::spawn(move || {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set().wait().text(text);
        }
    });
}

/// macOS and Windows clipboards persist past the writer, so no thread.
#[cfg(all(feature = "clipboard", not(target_os = "linux")))]
fn write_os_text(text: String) {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(&text);
    }
}

/// The clipboard of a build without the `clipboard` feature: nothing to read, and writes go to
/// the terminal alone (OSC 52), which needs no OS access.
pub struct TerminalClipboard;

impl ClipboardSource for TerminalClipboard {
    fn read_text(&mut self) -> TextRead {
        TextRead::Unreachable
    }

    fn can_read_bitmaps(&self) -> bool {
        false
    }

    fn bitmap_reader(&mut self) -> BitmapReader {
        Box::new(|| None)
    }

    fn write_text(&mut self, text: String) {
        super::osc52_copy(&text);
    }
}

/// A clipboard that is always empty and discards writes — what an `App` starts with, so that
/// only `main`, which installs [`default_source`], ever reaches a real one.  Tests run on
/// parallel threads against one process-wide clipboard, so touching the real one would let one
/// test's copy land between another's copy and its paste, let the developer's own clipboard leak
/// into assertions, and write OSC 52 escapes to test stdout.
pub struct NullClipboard;

impl ClipboardSource for NullClipboard {
    fn read_text(&mut self) -> TextRead {
        TextRead::Unreachable
    }

    fn can_read_bitmaps(&self) -> bool {
        false
    }

    fn bitmap_reader(&mut self) -> BitmapReader {
        Box::new(|| None)
    }

    fn write_text(&mut self, _text: String) {}
}

/// The real clipboard for this build: [`OsClipboard`] with the `clipboard` feature, else
/// [`TerminalClipboard`].
pub fn default_source() -> Box<dyn ClipboardSource> {
    #[cfg(feature = "clipboard")]
    {
        Box::new(OsClipboard)
    }
    #[cfg(not(feature = "clipboard"))]
    {
        Box::new(TerminalClipboard)
    }
}
