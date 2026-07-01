//! Crate-wide constants shared across modules.

/// Number of spaces per indentation level, applied everywhere indentation is
/// produced or consumed: the renderer's nested-list indent, list indent /
/// outdent (Tab / Shift-Tab), vim `>>` / `<<`, list continuation, and
/// plain-text tab insertion.  Fixed at 4 to follow the CommonMark convention
/// (a nested block must be indented far enough to clear a single-digit ordered
/// marker) and, crucially, to keep the rendered and raw views' indentation in
/// lockstep — a nested item must sit at the same column in both so de-rendering
/// a block causes no horizontal jump.
pub const INDENT_WIDTH: usize = 4;
