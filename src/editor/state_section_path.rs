//! Heading-ancestor chain for the cursor's current position.
//!
//! Used by the status bar to render a breadcrumb like
//! `notes.md › Checkpoint 1 › Item 1` so the user always sees where in
//! the document tree the cursor sits.  Complements the section picker
//! (`crate::ui::section_picker`) which lets them jump anywhere in the
//! same tree.
//!
//! Mirrors the heading-walk shape used by `App::open_section_picker`
//! (`src/app/section_jump.rs`) but lives one layer down so the UI layer
//! can call it without depending on `app`.

use pulldown_cmark::HeadingLevel;

use crate::editor::EditorState;
use crate::markdown::{ast::heading_plain_text, Block};

impl EditorState {
    /// The chain of headings whose scope contains the cursor, in
    /// document order (shallowest → deepest).  Empty when no heading
    /// precedes the cursor or the document has no headings at all.
    ///
    /// Walks `parsed.blocks` once, collecting all headings at or before
    /// the cursor's buffer line, then keeps only the strictly-decreasing
    /// suffix from the end — that's the deepest enclosing heading plus
    /// each of its true ancestors.  Skips intermediate sibling headings
    /// the cursor isn't actually under.
    ///
    /// Cheap enough to call on every status-bar redraw: O(blocks) with
    /// two rope lookups per heading, and `parsed` is stable between
    /// buffer mutations.
    pub fn cursor_section_chain(&self) -> Vec<String> {
        let cursor_line = self.buffer.char_to_line(self.cursor.offset);
        self.section_chain_for_buffer_line(cursor_line)
    }

    /// The heading-ancestor chain anchored on the top of the viewport
    /// rather than the cursor.  Used in Preview mode, where the cursor
    /// is hidden and doesn't track the scroll position: the top visible
    /// rendered line is mapped back to a source byte (via the
    /// `source_map`) and then to a buffer line, so the breadcrumb stays
    /// in sync with what the user is reading.  A scroll position past the
    /// last rendered line (which shouldn't normally happen — the viewport
    /// clamps it) is pinned to the last line so the breadcrumb tracks the
    /// deepest visible section rather than resetting to the document root.
    /// Falls back to the first buffer line when there is no source mapping
    /// at all (e.g. an empty document).
    pub fn scroll_section_chain(&self) -> Vec<String> {
        let map = &self.parsed.source_map;
        let last_line = map.rendered_line_count().saturating_sub(1);
        let rendered_line = self.scroll.min(last_line);
        let line = map
            .original_byte_for_rendered_line(rendered_line)
            .map(|byte| self.buffer.byte_to_line(byte))
            .unwrap_or(0);
        self.section_chain_for_buffer_line(line)
    }

    /// Shared implementation behind [`cursor_section_chain`] and
    /// [`scroll_section_chain`]: collect the heading chain enclosing
    /// `anchor_line` (a buffer line index).
    fn section_chain_for_buffer_line(&self, anchor_line: usize) -> Vec<String> {
        let mut at_or_before: Vec<(HeadingLevel, String)> = Vec::new();
        for (block_idx, block) in self.parsed.blocks.iter().enumerate() {
            let Block::Heading { level, inlines } = block else {
                continue;
            };
            let Some(range) = self.parsed.real_ranges.get(block_idx) else {
                continue;
            };
            let buffer_line = self.buffer.byte_to_line(range.start);
            if buffer_line > anchor_line {
                // Blocks are in document order — every later heading is
                // past the anchor line.
                break;
            }
            at_or_before.push((*level, heading_plain_text(inlines)));
        }

        // Walk back from the deepest heading at-or-before, taking only
        // strictly-shallower ancestors.  This skips siblings the cursor
        // isn't actually under (e.g. an earlier H2 in a different
        // section won't show up as an "ancestor" of an H3 under a later
        // H2).
        let mut chain: Vec<String> = Vec::new();
        let mut shallowest_level = usize::MAX;
        for (level, text) in at_or_before.iter().rev() {
            let lvl = *level as usize;
            if lvl < shallowest_level {
                chain.push(text.clone());
                shallowest_level = lvl;
            }
            if shallowest_level == 1 {
                break;
            }
        }
        chain.reverse();
        chain
    }
}

#[cfg(test)]
mod tests {
    use crate::document::Buffer;
    use crate::editor::EditorState;

    fn state_from(src: &str, cursor_offset: usize) -> EditorState {
        let theme = Box::leak(Box::new(crate::config::Theme::default()));
        let mut st = EditorState::new(Buffer::from_str(src), theme);
        st.cursor.offset = cursor_offset;
        st
    }

    #[test]
    fn empty_when_no_headings() {
        let st = state_from("just a paragraph\n", 0);
        assert!(st.cursor_section_chain().is_empty());
    }

    #[test]
    fn empty_when_cursor_precedes_first_heading() {
        // Cursor at offset 0 — before the heading on line 1.
        let st = state_from("prelude\n\n# Top\n", 0);
        assert!(st.cursor_section_chain().is_empty());
    }

    #[test]
    fn returns_only_heading_when_under_single_h1() {
        // Cursor in the body line under "# Top".
        let src = "# Top\n\nbody text\n";
        let cursor = src.find("body").unwrap();
        let st = state_from(src, cursor);
        assert_eq!(st.cursor_section_chain(), vec!["Top".to_string()]);
    }

    #[test]
    fn returns_full_chain_in_document_order() {
        let src = "# A\n\n## B\n\n### C\n\nbody\n";
        let cursor = src.find("body").unwrap();
        let st = state_from(src, cursor);
        assert_eq!(
            st.cursor_section_chain(),
            vec!["A".to_string(), "B".to_string(), "C".to_string()]
        );
    }

    #[test]
    fn skips_earlier_sibling_at_same_level() {
        // The cursor is under "A2 > B" — the earlier "## A1" must not
        // appear in the chain.
        let src = "# Top\n\n## A1\n\nfirst body\n\n## A2\n\n### B\n\nsecond body\n";
        let cursor = src.find("second body").unwrap();
        let st = state_from(src, cursor);
        assert_eq!(
            st.cursor_section_chain(),
            vec!["Top".to_string(), "A2".to_string(), "B".to_string()]
        );
    }

    #[test]
    fn cursor_on_heading_line_includes_that_heading() {
        // Cursor on "## B" itself — that heading IS the section, plus
        // its ancestor "# A".
        let src = "# A\n\n## B\n\nbody\n";
        let cursor = src.find("## B").unwrap();
        let st = state_from(src, cursor);
        assert_eq!(
            st.cursor_section_chain(),
            vec!["A".to_string(), "B".to_string()]
        );
    }

    #[test]
    fn skipped_heading_levels_are_handled() {
        // H1 → H3 (no H2) — the chain has two entries, not three.
        let src = "# Top\n\n### Deep\n\nbody\n";
        let cursor = src.find("body").unwrap();
        let st = state_from(src, cursor);
        assert_eq!(
            st.cursor_section_chain(),
            vec!["Top".to_string(), "Deep".to_string()]
        );
    }

    /// In Preview, the breadcrumb follows the scroll position, not the
    /// (frozen) cursor.  Scrolling the top of the viewport into a deeper
    /// section updates the chain even though the cursor stays at offset 0.
    #[test]
    fn scroll_chain_follows_viewport_top() {
        let src = "# A\n\n## B\n\nbody\n";
        // Cursor parked on the top heading line — its chain is just "A".
        let st = state_from(src, 0);
        assert_eq!(st.cursor_section_chain(), vec!["A".to_string()]);

        // Scroll so the top visible rendered line is the "body" block.
        let body_byte = src.find("body").unwrap();
        let body_line = st
            .parsed
            .source_map
            .rendered_lines_for_byte(body_byte)
            .start;
        let mut st = st;
        st.scroll = body_line;
        assert_eq!(
            st.scroll_section_chain(),
            vec!["A".to_string(), "B".to_string()]
        );
    }

    #[test]
    fn formatted_heading_text_is_flattened() {
        let src = "## **Bold** and `code`\n\nbody\n";
        let cursor = src.find("body").unwrap();
        let st = state_from(src, cursor);
        assert_eq!(st.cursor_section_chain(), vec!["Bold and code".to_string()]);
    }
}
