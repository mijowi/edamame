//! The clipboard payload model: the one non-text payload edamame reads, expressed in terms every
//! platform can offer.  The adapters in [`super::source`] do the mapping — a `CF_DIBV5` bitmap
//! and a PNG pasteboard entry both become a [`Bitmap`] — so nothing above this module names a
//! clipboard *format*.

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
