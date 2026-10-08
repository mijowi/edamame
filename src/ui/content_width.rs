//! Shared "max width over a row set" helpers used by modal overlays to size themselves to their
//! content.  The per-row mapping differs per overlay, so the helper takes a closure.

use unicode_width::UnicodeWidthStr;

/// Maximum width yielded by `width_of` over the rows, as `u16`.  Empty rows give 0.
pub fn max_row_width<T>(rows: &[T], width_of: impl Fn(&T) -> usize) -> u16 {
    rows.iter().map(width_of).max().unwrap_or(0) as u16
}

/// Width of an optional text region (`prefix_len` plus its cells), or 0 when `text` is `None`.
pub fn optional_text_width(text: Option<&str>, prefix_len: usize) -> u16 {
    text.map(|s| (prefix_len + s.width()) as u16).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_row_width_empty_returns_zero() {
        let rows: [u8; 0] = [];
        assert_eq!(max_row_width(&rows, |_| 5), 0);
    }

    #[test]
    fn max_row_width_returns_largest() {
        let rows = ["a", "abc", "ab"];
        assert_eq!(max_row_width(&rows, |s| s.chars().count()), 3);
    }

    #[test]
    fn optional_text_width_handles_none() {
        assert_eq!(optional_text_width(None, 4), 0);
    }

    #[test]
    fn optional_text_width_includes_prefix() {
        assert_eq!(optional_text_width(Some("oops"), 2), 6);
    }
}
