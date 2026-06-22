//! Session state for an active search-and-replace flow.

use std::ops::Range;

/// The active search (and optionally replace) session.  Owned by
/// `EditorState::search`; `Some` for exactly the lifetime of the flow.
///
/// Match ranges are byte offsets into the buffer contents, valid for
/// the [`Self::buffer_version`] they were computed against.  Callers
/// must run [`Self::ensure_fresh`] after any buffer mutation (replace,
/// undo, redo) before consulting `matches` again; the render layer
/// additionally clamps every range against the live source so a missed
/// refresh can never panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchState {
    /// The literal query.  Never empty, never contains a newline
    /// (rejected by [`Self::new`]).  Matched smartcase for navigation,
    /// case-sensitively for a replace flow — see [`Self::ensure_fresh`].
    pub query: String,
    /// Replacement text.  `None` means the user left the replace field
    /// empty — a navigate-only flow with no Replace / Replace-all keys.
    pub replace: Option<String>,
    /// Non-overlapping match byte ranges in document order.
    pub matches: Vec<Range<usize>>,
    /// Index into [`Self::matches`] of the emphasized current match.
    pub focused_idx: usize,
    /// `Buffer::version()` the match list was computed against.
    buffer_version: u64,
}

impl SearchState {
    /// Build a new session.  Returns `None` for an empty query or one
    /// containing a newline (matches can't span buffer lines).  The
    /// match list starts stale; the caller's first
    /// [`Self::ensure_fresh`] populates it.
    pub fn new(query: String, replace: Option<String>) -> Option<Self> {
        if query.is_empty() || query.contains('\n') {
            return None;
        }
        Some(Self {
            query,
            replace,
            matches: Vec::new(),
            focused_idx: 0,
            // Forces the first `ensure_fresh` to compute.
            buffer_version: u64::MAX,
        })
    }

    /// True when a replacement string was provided — enables the
    /// Replace / Replace-all keys and their hint chords.
    pub fn is_replace_flow(&self) -> bool {
        self.replace.is_some()
    }

    /// True when the match list was computed against `version`.
    /// Callers use this to skip materializing the buffer contents when
    /// no recompute is needed — [`Self::ensure_fresh`] takes the source
    /// as `&str`, which costs an O(n) rope-to-`String` copy to produce.
    pub fn is_fresh(&self, version: u64) -> bool {
        self.buffer_version == version
    }

    /// Recompute the match list when `version` differs from the one the
    /// list was built against.  Clamps `focused_idx` into the new list
    /// (an index past the end wraps to the first match, matching the
    /// flow's wrap-around navigation).  Returns true when a recompute
    /// happened.
    pub fn ensure_fresh(&mut self, source: &str, version: u64) -> bool {
        if self.buffer_version == version {
            return false;
        }
        // Smartcase is a navigation-only convenience.  A replace flow stays
        // strictly case-sensitive, so neither its highlights nor the
        // replacement ever rewrite a casing variant the user didn't type.
        self.matches = if self.is_replace_flow() {
            find_all_cs(source, &self.query)
        } else {
            find_all(source, &self.query)
        };
        self.buffer_version = version;
        if self.focused_idx >= self.matches.len() {
            self.focused_idx = 0;
        }
        true
    }

    /// Byte range of the current match, if any matches remain.
    pub fn focused_range(&self) -> Option<Range<usize>> {
        self.matches.get(self.focused_idx).cloned()
    }

    /// Move focus to the next match, wrapping at the end.
    pub fn advance_focus(&mut self) {
        if !self.matches.is_empty() {
            self.focused_idx = (self.focused_idx + 1) % self.matches.len();
        }
    }

    /// Move focus to the previous match, wrapping at the start.
    pub fn retreat_focus(&mut self) {
        if !self.matches.is_empty() {
            self.focused_idx = self
                .focused_idx
                .checked_sub(1)
                .unwrap_or(self.matches.len() - 1);
        }
    }
}

/// All non-overlapping byte ranges of `needle` in `haystack`, in
/// document order, applying **smartcase**: the search is case-insensitive
/// unless `needle` contains an uppercase letter, in which case it is
/// case-sensitive.  Every returned offset is a char boundary, so the
/// ranges are UTF-8-safe.
///
/// This is the **navigation** matcher (`/`, `n`/`N`, `Ctrl-F` find).
/// Smartcase lives here, in the base search feature, so both the
/// `Ctrl-F` flow and vim's `/` share it — every edamame user gets it,
/// not just vim.  The replace flow deliberately uses [`find_all_cs`]
/// instead, so a lowercase find term never overwrites a casing variant.
pub fn find_all(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    // Smartcase: any uppercase char in the pattern → case-sensitive.
    if needle.chars().any(char::is_uppercase) {
        return find_all_cs(haystack, needle);
    }
    find_all_ci(haystack, needle)
}

/// Case-sensitive, non-overlapping match search — the matcher the
/// replace flow always uses (smartcase is navigation-only).
/// `str::match_indices` is non-overlapping by construction and always
/// yields char-boundary offsets, so the ranges are UTF-8-safe.
fn find_all_cs(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    haystack
        .match_indices(needle)
        .map(|(start, m)| start..start + m.len())
        .collect()
}

/// Case-insensitive, non-overlapping match search keeping byte offsets
/// aligned to the original `haystack` (lowercasing the strings up front
/// would shift offsets for chars whose lowercase form differs in byte
/// length, so we compare char-by-char against the untouched source).
fn find_all_ci(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    let needle_chars: Vec<char> = needle.chars().collect();
    let hay_chars: Vec<(usize, char)> = haystack.char_indices().collect();
    let n = needle_chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i + n <= hay_chars.len() {
        let matched = (0..n).all(|k| chars_eq_ci(hay_chars[i + k].1, needle_chars[k]));
        if matched {
            let start = hay_chars[i].0;
            let end = hay_chars
                .get(i + n)
                .map_or(haystack.len(), |&(byte, _)| byte);
            out.push(start..end);
            i += n; // non-overlapping, mirroring `match_indices`
        } else {
            i += 1;
        }
    }
    out
}

/// Compare two chars ignoring case (Unicode simple case folding via
/// `to_lowercase`, which covers the markdown-text cases we care about).
fn chars_eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_all_locates_every_occurrence_in_order() {
        assert_eq!(find_all("abcabcabc", "abc"), vec![0..3, 3..6, 6..9]);
        assert_eq!(find_all("no hits here", "xyz"), Vec::<Range<usize>>::new());
    }

    #[test]
    fn find_all_is_non_overlapping() {
        // "aaa" contains "aa" at 0 and 1, but both the case-sensitive and
        // case-insensitive paths yield only the non-overlapping match at 0.
        assert_eq!(find_all("aaa", "aa"), vec![0..2]);
        assert_eq!(find_all("AAA", "aa"), vec![0..2]);
    }

    #[test]
    fn find_all_smartcase_lowercase_query_is_insensitive() {
        // An all-lowercase pattern matches every case variant.
        assert_eq!(find_all("Foo foo FOO", "foo"), vec![0..3, 4..7, 8..11]);
    }

    #[test]
    fn find_all_smartcase_uppercase_query_is_sensitive() {
        // Any uppercase letter flips the search to case-sensitive.
        assert_eq!(find_all("Foo foo FOO", "Foo"), vec![0..3]);
        assert_eq!(find_all("Foo foo FOO", "FOO"), vec![8..11]);
    }

    #[test]
    fn find_all_ci_keeps_byte_offsets_aligned_for_multibyte() {
        // Case-insensitive matching must not shift offsets even when a
        // char's lowercase form differs; "café" / "CAFÉ" stays byte-aligned.
        let hay = "café CAFÉ";
        let ranges = find_all(hay, "café");
        assert_eq!(ranges.len(), 2);
        // Each range must slice cleanly (a panic here would mean a
        // non-char-boundary offset) and recover a case variant of "café".
        let hits: Vec<&str> = ranges.iter().map(|r| &hay[r.clone()]).collect();
        assert_eq!(hits, vec!["café", "CAFÉ"]);
    }

    #[test]
    fn find_all_handles_multibyte_needles_and_haystacks() {
        let hay = "naïve café naïve";
        let ranges = find_all(hay, "naïve");
        assert_eq!(ranges.len(), 2);
        for r in ranges {
            assert_eq!(&hay[r], "naïve");
        }
    }

    #[test]
    fn replace_flow_matching_is_case_sensitive_not_smartcase() {
        // A navigate flow with a lowercase query is smartcase (3 hits)...
        let mut nav = SearchState::new("foo".to_owned(), None).unwrap();
        nav.ensure_fresh("Foo foo FOO", 1);
        assert_eq!(nav.matches.len(), 3);
        // ...but the same query in a replace flow stays case-sensitive, so
        // only the exact-case occurrence is hit (and would be replaced).
        let mut repl = SearchState::new("foo".to_owned(), Some("bar".to_owned())).unwrap();
        repl.ensure_fresh("Foo foo FOO", 1);
        assert_eq!(repl.matches, vec![4..7]);
    }

    #[test]
    fn new_rejects_empty_and_multiline_queries() {
        assert!(SearchState::new(String::new(), None).is_none());
        assert!(SearchState::new("a\nb".to_owned(), None).is_none());
        assert!(SearchState::new("ok".to_owned(), None).is_some());
    }

    fn fresh(source: &str, query: &str) -> SearchState {
        let mut s = SearchState::new(query.to_owned(), None).unwrap();
        s.ensure_fresh(source, 1);
        s
    }

    #[test]
    fn ensure_fresh_skips_when_version_unchanged() {
        let mut s = fresh("aba", "a");
        assert_eq!(s.matches.len(), 2);
        // Same version: even with different source text, no recompute.
        assert!(!s.ensure_fresh("bbb", 1));
        assert_eq!(s.matches.len(), 2);
        // New version: recompute happens.
        assert!(s.ensure_fresh("bbb", 2));
        assert!(s.matches.is_empty());
    }

    #[test]
    fn navigation_wraps_both_directions() {
        let mut s = fresh("x.x.x", "x");
        assert_eq!(s.focused_idx, 0);
        s.advance_focus();
        s.advance_focus();
        assert_eq!(s.focused_idx, 2);
        s.advance_focus();
        assert_eq!(s.focused_idx, 0, "next past last wraps to first");
        s.retreat_focus();
        assert_eq!(s.focused_idx, 2, "prev before first wraps to last");
    }

    #[test]
    fn ensure_fresh_wraps_out_of_range_focus_to_first() {
        let mut s = fresh("x x x", "x");
        s.focused_idx = 2;
        // Source shrinks to a single match: index 2 is gone, wraps to 0.
        s.ensure_fresh("x", 2);
        assert_eq!(s.matches.len(), 1);
        assert_eq!(s.focused_idx, 0);
    }
}
