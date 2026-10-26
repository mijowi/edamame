//! Crate-wide constants shared across modules.

/// Spaces per indentation level, everywhere indentation is produced or consumed.  Fixed at 4 to
/// follow CommonMark (a nested block must clear a single-digit ordered marker) and to keep the
/// rendered and raw views in lockstep, so de-rendering a block causes no horizontal jump.
pub const INDENT_WIDTH: usize = 4;

/// Ceiling on the text one counted paste may build (`{count}p` repeats the register `count`
/// times).  The count itself is capped, but not the product: `9999p` of a 1 MB register would
/// otherwise ask for ~10 GB in one allocation.  64 MiB is far past any real paste and still small
/// enough to allocate.  Shared by every modal editing mode; see
/// [`repeat_for_paste`](crate::editor::edit_ops::repeat_for_paste).
pub const MAX_PASTE_BYTES: usize = 64 * 1024 * 1024;
