//! Tiny shared helper for the "max width over a row set" calculation
//! every modal overlay does to size itself to its content.
//!
//! Each overlay's content width is `max(per_row_width)` plus a few
//! extras (longest description, longest error) — the per-row mapping
//! differs but the `iter().map().max().unwrap_or(0) as u16` shape is
//! identical, so the helper takes a closure.

/// Return the maximum width (in `usize` terms) yielded by `width_of`
/// over the rows, capped to `u16`.  Empty iterators return 0.
pub fn max_row_width<T>(rows: &[T], width_of: impl Fn(&T) -> usize) -> u16 {
    rows.iter().map(width_of).max().unwrap_or(0) as u16
}

/// Width of an optional text region `prefix_len + text.chars().count()`,
/// or 0 when `text` is `None`.  Convenience for the longest-error and
/// longest-description companions every overlay computes.
pub fn optional_text_width(text: Option<&str>, prefix_len: usize) -> u16 {
    text.map(|s| (prefix_len + s.chars().count()) as u16)
        .unwrap_or(0)
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
