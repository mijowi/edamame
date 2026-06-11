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
    /// The literal, case-sensitive query.  Never empty, never contains
    /// a newline (rejected by [`Self::new`]).
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
    /// Scroll offset saved on entry and restored when the flow exits.
    pub pre_search_scroll: usize,
}

impl SearchState {
    /// Build a new session.  Returns `None` for an empty query or one
    /// containing a newline (matches can't span buffer lines).  The
    /// match list starts stale; the caller's first
    /// [`Self::ensure_fresh`] populates it.
    pub fn new(query: String, replace: Option<String>, pre_search_scroll: usize) -> Option<Self> {
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
            pre_search_scroll,
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
        self.matches = find_all(source, &self.query);
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
/// document order.  `str::match_indices` is non-overlapping by
/// construction and always yields char-boundary offsets, so the ranges
/// are UTF-8-safe.
pub fn find_all(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    haystack
        .match_indices(needle)
        .map(|(start, m)| start..start + m.len())
        .collect()
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
    fn find_all_is_case_sensitive_and_non_overlapping() {
        assert_eq!(find_all("Foo foo FOO", "foo"), vec![4..7]);
        // "aaa" contains "aa" at 0 and 1, but match_indices yields only
        // the non-overlapping occurrence at 0.
        assert_eq!(find_all("aaa", "aa"), vec![0..2]);
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
    fn new_rejects_empty_and_multiline_queries() {
        assert!(SearchState::new(String::new(), None, 0).is_none());
        assert!(SearchState::new("a\nb".to_owned(), None, 0).is_none());
        assert!(SearchState::new("ok".to_owned(), None, 0).is_some());
    }

    fn fresh(source: &str, query: &str) -> SearchState {
        let mut s = SearchState::new(query.to_owned(), None, 0).unwrap();
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
