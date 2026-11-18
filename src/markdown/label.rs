//! Link and footnote labels as CommonMark matches them, shared by every module that resolves one.

/// A label as CommonMark matches it: case-insensitive, whitespace runs collapsed.  (Lowercasing
/// stands in for Unicode case folding, so a label differing only in a fold-specific letter such
/// as `ß` doesn't match.)
pub(crate) fn normalize_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_and_collapses_whitespace() {
        assert_eq!(normalize_label("  Foo \t\n Bar "), "foo bar");
    }
}
