//! Shared paste policy for single-line modal text fields.
//!
//! Every text-input modal (search/replace, command palette, save-copy
//! path, export-theme name, insert-table dimensions, theme/section
//! filters, settings field editor) holds a *single line* of text.  A
//! bracketed paste, however, can carry newlines and arbitrarily large
//! content.  [`sanitize_paste`] is the single source of truth that
//! flattens such a paste into something a one-line field can accept:
//! control characters (newlines, tabs, …) are dropped so a multi-line
//! clipboard collapses to one line, and the result is capped at
//! [`PASTE_CHAR_CAP`] characters so pasting a whole document can't blow
//! up a prompt.
//!
//! Field-specific filtering (e.g. the digits-only insert-table fields)
//! is layered on top by each state's own `paste` method — this helper
//! only owns the line-flattening and length cap that every field shares.

/// Maximum number of characters a single paste may contribute to a
/// field.  Generous enough for long file paths (Linux `PATH_MAX` is
/// 4096) and long search/replace terms, while still guarding against
/// pasting an entire document into a one-line prompt.
pub const PASTE_CHAR_CAP: usize = 1024;

/// Flatten a bracketed paste for insertion into a single-line field:
/// drop every control character (so newlines/tabs can't break the
/// layout) and truncate to [`PASTE_CHAR_CAP`] characters.
pub fn sanitize_paste(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(PASTE_CHAR_CAP)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_newlines_and_other_control_chars() {
        assert_eq!(sanitize_paste("a\nb\r\nc\td"), "abcd");
    }

    #[test]
    fn keeps_printable_unicode() {
        assert_eq!(sanitize_paste("naïve — café"), "naïve — café");
    }

    #[test]
    fn caps_at_the_char_limit() {
        let huge = "x".repeat(PASTE_CHAR_CAP + 500);
        assert_eq!(sanitize_paste(&huge).chars().count(), PASTE_CHAR_CAP);
    }

    #[test]
    fn cap_counts_chars_not_bytes() {
        // Multi-byte chars must not be truncated mid-codepoint, and the
        // cap is a character count, not a byte count.
        let huge = "é".repeat(PASTE_CHAR_CAP + 10);
        assert_eq!(sanitize_paste(&huge).chars().count(), PASTE_CHAR_CAP);
    }
}
