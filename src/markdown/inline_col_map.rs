use pulldown_cmark::{Event, Options, Parser};

/// Bidirectional character-column map between raw Markdown source and
/// its rendered (inline-markup-collapsed) form for a single line.
///
/// Built by re-parsing `raw_line` with pulldown-cmark and recording
/// the raw byte position of every rendered character emitted by inline
/// `Text`, `Code`, and `SoftBreak`/`HardBreak` events.  Marker bytes
/// (asterisks, brackets, the URL portion of a link) sit in the gaps
/// between events and are correctly skipped.
#[derive(Debug, Clone)]
pub struct InlineColMap {
    rendered_to_raw: Vec<usize>,
    raw_to_rendered: Vec<usize>,
    rendered_len: usize,
    raw_len: usize,
}

impl InlineColMap {
    pub fn build(raw_line: &str) -> Self {
        let raw_len = raw_line.chars().count();
        let mut walk = CharMapWalk::new(raw_line);

        let opts = Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_SMART_PUNCTUATION;

        for (event, range) in Parser::new_ext(raw_line, opts).into_offset_iter() {
            match event {
                Event::Text(_) => walk.push_text(raw_line, range),
                Event::Code(s) => walk.push_code(&s, range),
                Event::SoftBreak | Event::HardBreak => walk.push_break(range.start),
                _ => {}
            }
        }

        let rendered_to_raw = walk.finish();
        let rendered_len = rendered_to_raw.len().saturating_sub(1);

        // Build the inverse map (raw char idx -> rendered char idx).
        let mut raw_to_rendered = vec![usize::MAX; raw_len + 1];

        for (rendered_idx, &raw_char_idx) in rendered_to_raw.iter().enumerate() {
            if raw_char_idx <= raw_len && raw_to_rendered[raw_char_idx] == usize::MAX {
                raw_to_rendered[raw_char_idx] = rendered_idx;
            }
        }

        // Past-end entry.
        raw_to_rendered[raw_len] = rendered_len;

        // Backward-fill: marker bytes get the rendered index of the next
        // visible character (matching paragraph_raw_col_to_rendered_col's
        // "smallest rendered idx whose raw position is >= raw_col").
        for i in (0..raw_len).rev() {
            if raw_to_rendered[i] == usize::MAX {
                raw_to_rendered[i] = raw_to_rendered[i + 1];
            }
        }

        Self {
            rendered_to_raw,
            raw_to_rendered,
            rendered_len,
            raw_len,
        }
    }

    /// Raw char index for a rendered char column.  Clamps to `raw_len`.
    #[cfg(test)]
    pub fn rendered_to_raw(&self, rendered_char: usize) -> usize {
        let idx = rendered_char.min(self.rendered_len);
        self.rendered_to_raw[idx]
    }

    /// Rendered char index for a raw char column.  Always returns a value.
    ///
    /// When `raw_col` lands on a marker byte (the `[` of `[link]`, the `*`
    /// of `**bold**`), returns the rendered idx immediately after the marker.
    pub fn raw_to_rendered(&self, raw_char: usize) -> usize {
        let idx = raw_char.min(self.raw_len);
        self.raw_to_rendered[idx]
    }

    /// Same as `raw_to_rendered`, but returns `None` when the walker's
    /// `rendered_len` doesn't match `actual_rendered_count`.  Headings,
    /// blockquotes, and list-marker prefixes add rendered glyphs the
    /// walker can't see, causing the counts to diverge.
    pub fn raw_to_rendered_checked(
        &self,
        raw_char: usize,
        actual_rendered_count: usize,
    ) -> Option<usize> {
        if self.rendered_len != actual_rendered_count {
            return None;
        }
        Some(self.raw_to_rendered(raw_char))
    }

    pub fn rendered_len(&self) -> usize {
        self.rendered_len
    }

    pub fn raw_len(&self) -> usize {
        self.raw_len
    }

    /// Direct access to the forward map for tests and callers that need
    /// the full vector (e.g. `rendered_sub_line_to_offset`).
    pub fn rendered_to_raw_vec(&self) -> &[usize] {
        &self.rendered_to_raw
    }
}

// ── Walker ──────────────────────────────────────────────────────────────────

struct CharMapWalk {
    byte_to_char: Vec<usize>,
    total_chars: usize,
    map: Vec<usize>,
}

impl CharMapWalk {
    fn new(raw_line: &str) -> Self {
        let mut byte_to_char = vec![0usize; raw_line.len() + 1];
        let mut char_idx = 0usize;
        for (byte_idx, _) in raw_line.char_indices() {
            byte_to_char[byte_idx] = char_idx;
            char_idx += 1;
        }
        byte_to_char[raw_line.len()] = char_idx;
        Self {
            byte_to_char,
            total_chars: char_idx,
            map: Vec::new(),
        }
    }

    fn lookup(&self, byte: usize) -> usize {
        self.byte_to_char
            .get(byte.min(self.byte_to_char.len().saturating_sub(1)))
            .copied()
            .unwrap_or(self.total_chars)
    }

    fn push_chars(&mut self, text: &str, mut byte: usize) -> usize {
        for c in text.chars() {
            self.map.push(self.lookup(byte));
            byte += c.len_utf8();
        }
        byte
    }

    fn push_text(&mut self, raw_line: &str, range: std::ops::Range<usize>) {
        let slice_end = range.end.min(raw_line.len());
        let raw_slice = &raw_line[range.start..slice_end];
        let mut byte = range.start;
        let mut rest = raw_slice;
        while let Some(start) = rest.find("==") {
            let after_open = &rest[start + 2..];
            let Some(rel_end) = after_open.find("==") else {
                break;
            };
            byte = self.push_chars(&rest[..start], byte);
            byte += 2; // skip opening ==
            byte = self.push_chars(&after_open[..rel_end], byte);
            byte += 2; // skip closing ==
            rest = &after_open[rel_end + 2..];
        }
        self.push_chars(rest, byte);
    }

    fn push_code(&mut self, inner: &str, range: std::ops::Range<usize>) {
        self.map.push(self.lookup(range.start));
        self.push_chars(inner, range.start + 1);
        self.map.push(self.lookup(range.end.saturating_sub(1)));
    }

    fn push_break(&mut self, byte: usize) {
        self.map.push(self.lookup(byte));
    }

    fn finish(mut self) -> Vec<usize> {
        self.map.push(self.total_chars);
        self.map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_maps_one_to_one() {
        let map = InlineColMap::build("hello world");
        assert_eq!(map.rendered_len(), 11);
        assert_eq!(map.raw_len(), 11);
        for i in 0..=11 {
            assert_eq!(map.rendered_to_raw(i), i);
            assert_eq!(map.raw_to_rendered(i), i);
        }
    }

    #[test]
    fn bold_text_skips_markers() {
        let map = InlineColMap::build("**Bold text**");
        // Rendered: "Bold text" = 9 chars
        assert_eq!(map.rendered_len(), 9);
        assert_eq!(map.raw_len(), 13);
        // Rendered chars map to raw chars 2..10 (inside the **)
        assert_eq!(map.rendered_to_raw(0), 2); // B
        assert_eq!(map.rendered_to_raw(8), 10); // t
        assert_eq!(map.rendered_to_raw(9), 13); // sentinel
                                                // Inverse: raw marker bytes forward-fill to first content char
        assert_eq!(map.raw_to_rendered(0), 0); // first *
        assert_eq!(map.raw_to_rendered(1), 0); // second *
        assert_eq!(map.raw_to_rendered(2), 0); // B
        assert_eq!(map.raw_to_rendered(10), 8); // t
        assert_eq!(map.raw_to_rendered(11), 9); // closing *
        assert_eq!(map.raw_to_rendered(12), 9); // closing *
        assert_eq!(map.raw_to_rendered(13), 9); // past end
    }

    #[test]
    fn italic_text_skips_markers() {
        let map = InlineColMap::build("*Italic*");
        assert_eq!(map.rendered_len(), 6);
        assert_eq!(map.rendered_to_raw(0), 1); // I
        assert_eq!(map.rendered_to_raw(5), 6); // c
    }

    #[test]
    fn underscore_emphasis_skips_markers() {
        let map = InlineColMap::build("_under_");
        assert_eq!(map.rendered_len(), 5);
        assert_eq!(map.rendered_to_raw(0), 1); // u
    }

    #[test]
    fn strikethrough_skips_markers() {
        let map = InlineColMap::build("~~strike~~");
        assert_eq!(map.rendered_len(), 6);
        assert_eq!(map.rendered_to_raw(0), 2); // s
        assert_eq!(map.rendered_to_raw(5), 7); // e
    }

    #[test]
    fn highlight_skips_markers() {
        let map = InlineColMap::build("alpha ==beta== gamma");
        // Rendered: "alpha beta gamma" = 16 chars
        assert_eq!(map.rendered_len(), 16);
        assert_eq!(&map.rendered_to_raw_vec()[..6], &[0, 1, 2, 3, 4, 5]);
        assert_eq!(&map.rendered_to_raw_vec()[6..10], &[8, 9, 10, 11]);
        assert_eq!(
            &map.rendered_to_raw_vec()[10..16],
            &[14, 15, 16, 17, 18, 19]
        );
        assert_eq!(map.rendered_to_raw_vec()[16], 20);
    }

    #[test]
    fn nested_bold_italic() {
        let map = InlineColMap::build("**_Bold and italic_**");
        // Rendered: "Bold and italic" = 15 chars
        assert_eq!(map.rendered_len(), 15);
        assert_eq!(map.rendered_to_raw(0), 3); // B
    }

    #[test]
    fn code_span_adds_space_padding() {
        let map = InlineColMap::build("`code`");
        // Rendered: " code " = 6 chars (leading + trailing space)
        assert_eq!(map.rendered_len(), 6);
        assert_eq!(map.rendered_to_raw(0), 0); // leading space maps to opening backtick
        assert_eq!(map.rendered_to_raw(5), 5); // trailing space maps to closing backtick
    }

    #[test]
    fn link_collapses_url() {
        let map = InlineColMap::build("[File link](./plan.md)");
        // Rendered: "File link" = 9 chars
        assert_eq!(map.rendered_len(), 9);
        assert_eq!(
            &map.rendered_to_raw_vec()[..9],
            &[1, 2, 3, 4, 5, 6, 7, 8, 9]
        );
        assert_eq!(map.rendered_to_raw_vec()[9], 22); // sentinel
    }

    #[test]
    fn link_round_trip() {
        let map = InlineColMap::build("[File link](./plan.md)");
        for rendered_col in 0..9 {
            let raw_col = map.rendered_to_raw(rendered_col);
            let back = map.raw_to_rendered(raw_col);
            assert_eq!(
                back, rendered_col,
                "round-trip failed at rendered col {rendered_col}"
            );
        }
    }

    #[test]
    fn highlight_round_trip() {
        let map = InlineColMap::build("alpha ==beta== gamma");
        for rendered_col in 0..map.rendered_len() {
            let raw_col = map.rendered_to_raw(rendered_col);
            let back = map.raw_to_rendered(raw_col);
            assert_eq!(
                back, rendered_col,
                "round-trip failed at rendered col {rendered_col}"
            );
        }
    }

    #[test]
    fn heading_well_formedness_mismatch() {
        // "## Heading" — pulldown-cmark sees "Heading" (7 chars) but
        // the renderer emits a styled prefix so the rendered line is
        // longer.  The map's rendered_len won't match.
        let map = InlineColMap::build("## Heading");
        // Walker only sees the text content "Heading" (7 chars).
        assert_eq!(map.rendered_len(), 7);
        // A heading renders as e.g. "  Heading" (9 chars) so checked
        // lookup should return None.
        assert_eq!(map.raw_to_rendered_checked(5, 9), None);
        // But unchecked still works.
        assert!(map.raw_to_rendered(5) <= map.rendered_len());
    }

    #[test]
    fn blockquote_well_formedness_mismatch() {
        let map = InlineColMap::build("> blockquoted text");
        // Walker sees "blockquoted text" (16 chars) but rendered has
        // a "▎ " prefix (2 chars extra).
        assert_eq!(map.raw_to_rendered_checked(3, 18), None);
    }

    #[test]
    fn marker_byte_maps_to_next_visible() {
        let map = InlineColMap::build("[link](url)");
        // raw col 0 is `[` — should map to rendered col of `l` (0)
        assert_eq!(map.raw_to_rendered(0), 0);
        // raw col 5 is `]` — should map to rendered col after `k` (4)
        assert_eq!(map.raw_to_rendered(5), 4);
    }

    #[test]
    fn list_prefix_backward_fills() {
        let map = InlineColMap::build("- **bold** item");
        // pulldown-cmark treats "- " as a list marker — never emits
        // Text for it.  The walker sees "bold item" (9 chars) after
        // collapsing the ** markers.
        // The "- " prefix chars (raw 0, 1) should backward-fill to
        // rendered index 0.
        assert_eq!(map.raw_to_rendered(0), 0);
        assert_eq!(map.raw_to_rendered(1), 0);
        // "b" of "bold" is at raw col 4 (after "- **"), rendered col 0
        assert_eq!(map.raw_to_rendered(4), 0);
    }

    /// Multi-char-to-single-glyph smart-punctuation substitutions
    /// (`...` → `…`, `---` → `—`, `--` → `–`) are emitted by
    /// pulldown-cmark as inlined `Text` events, but the walker advances
    /// over the raw byte slice rather than the substituted glyph.  As a
    /// result `rendered_len` reflects the raw char count for these
    /// spans, not the renderer's actual output.
    ///
    /// `raw_to_rendered_checked` against the renderer's actual count
    /// therefore returns `None` for these lines — callers
    /// (`paint_selection_overlay`, the cursor indicator) fall back to a
    /// 1:1 mapping.  That fallback is slightly off past the substitution
    /// but is the documented contract.
    ///
    /// If a future walker change teaches it to consume the substituted
    /// glyph (so `rendered_len` matches the renderer), this assertion
    /// will flip and we should update the test + drop the 1:1 fallback.
    #[test]
    fn multi_char_smart_punct_triggers_checked_fallback() {
        for (raw, actual_rendered) in [
            ("hello...", 6), // "hello…"
            ("a---b", 3),    // "a—b"
            ("a--b", 3),     // "a–b"
        ] {
            let map = InlineColMap::build(raw);
            assert_eq!(
                map.raw_to_rendered_checked(0, actual_rendered),
                None,
                "smart-punct in {raw:?}: expected fallback (None) but walker matched",
            );
            // Unchecked still produces a value — must not panic.
            let _ = map.raw_to_rendered(0);
        }
    }

    /// Curly-quote substitutions are 1-raw-char-to-1-rendered-char, so
    /// counts agree and `checked()` accepts the line.  Each rendered
    /// position still points at the correct raw char (just a `"` instead
    /// of the curly glyph), so round-trip holds.
    #[test]
    fn curly_quote_substitution_round_trips() {
        let map = InlineColMap::build("\"hi\"");
        // 4 raw chars in, 4 rendered chars out.
        assert_eq!(map.rendered_len(), 4);
        assert_eq!(map.raw_len(), 4);
        for rendered_col in 0..map.rendered_len() {
            let raw_col = map.rendered_to_raw(rendered_col);
            assert_eq!(map.raw_to_rendered(raw_col), rendered_col);
        }
        assert_eq!(map.raw_to_rendered_checked(0, 4), Some(0));
    }

    /// Multi-byte raw chars (e.g. `é`, `ü`) are >1 byte in UTF-8 but
    /// only 1 char.  The forward and inverse maps must index by char,
    /// not byte — otherwise round-trip drifts after the first non-ASCII.
    #[test]
    fn unicode_text_round_trip() {
        let map = InlineColMap::build("café résumé");
        // Plain text → rendered_len == raw_len
        assert_eq!(map.rendered_len(), map.raw_len());
        for rendered_col in 0..map.rendered_len() {
            let raw_col = map.rendered_to_raw(rendered_col);
            assert_eq!(
                map.raw_to_rendered(raw_col),
                rendered_col,
                "round-trip failed at rendered col {rendered_col}",
            );
        }
    }

    /// Empty line: only the past-end sentinel exists.  Guards against
    /// any `saturating_sub(1)` underflow or zero-length indexing in
    /// the inverse-map fill, and against `checked()` panicking for a
    /// zero-length actual rendered count.
    #[test]
    fn empty_line() {
        let map = InlineColMap::build("");
        assert_eq!(map.rendered_len(), 0);
        assert_eq!(map.raw_len(), 0);
        assert_eq!(map.raw_to_rendered(0), 0);
        assert_eq!(map.raw_to_rendered_checked(0, 0), Some(0));
        assert_eq!(map.raw_to_rendered_checked(0, 2), None);
    }

    /// Querying `raw_to_rendered` past `raw_len` must clamp to the
    /// sentinel rather than panic.  Real callers (`paint_selection_overlay`)
    /// can pass `end_raw_col == raw_len`, and a future regression that
    /// fed `raw_len + 1` would otherwise OOB-panic in release builds
    /// without the `min` guard.
    #[test]
    fn raw_to_rendered_clamps_past_end() {
        let map = InlineColMap::build("hi");
        assert_eq!(map.raw_to_rendered(2), 2);
        assert_eq!(map.raw_to_rendered(999), 2);
        // Checked variant clamps under the count guard too.
        assert_eq!(map.raw_to_rendered_checked(999, 2), Some(2));
    }

    /// `raw_to_rendered_checked` accepts plain paragraphs (counts agree)
    /// and rejects heading-style prefixes (counts diverge).  This pins
    /// the contract the callers rely on.
    #[test]
    fn checked_accepts_plain_rejects_prefix_mismatch() {
        let map = InlineColMap::build("plain text");
        assert_eq!(map.raw_to_rendered_checked(0, 10), Some(0));
        assert_eq!(map.raw_to_rendered_checked(5, 10), Some(5));
        // Heading-rendered with a 2-char prefix → mismatch.
        assert_eq!(map.raw_to_rendered_checked(0, 12), None);
        // Off-by-one too — any mismatch fails.
        assert_eq!(map.raw_to_rendered_checked(0, 11), None);
        assert_eq!(map.raw_to_rendered_checked(0, 9), None);
    }

    #[test]
    fn mixed_formatting() {
        let raw = "**bold** *italic* `code` [link](url)";
        let map = InlineColMap::build(raw);
        // Just verify round-trip for all rendered positions
        for rendered_col in 0..map.rendered_len() {
            let raw_col = map.rendered_to_raw(rendered_col);
            let back = map.raw_to_rendered(raw_col);
            assert_eq!(
                back, rendered_col,
                "round-trip failed at rendered col {rendered_col} (raw {raw_col})"
            );
        }
    }
}
