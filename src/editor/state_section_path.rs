//! Heading-ancestor chain for the status-bar breadcrumb (`notes.md › Checkpoint 1 › Item 1`).
//! Mirrors the heading walk in `App::open_section_picker` but lives a layer down so the UI
//! can call it without depending on `app`.

use pulldown_cmark::HeadingLevel;

use crate::editor::EditorState;
use crate::markdown::{ast::heading_plain_text, Block};

impl EditorState {
    /// Headings whose scope contains the cursor, shallowest first; empty when none precedes
    /// it.  O(blocks), cheap enough for every status-bar redraw.
    pub fn cursor_section_chain(&self) -> Vec<String> {
        let cursor_line = self.buffer.char_to_line(self.cursor.offset);
        self.section_chain_for_buffer_line(cursor_line)
    }

    /// Same chain anchored on the top of the viewport, for Preview mode where the hidden
    /// cursor doesn't track the scroll.  An out-of-range scroll pins to the last rendered
    /// line; no source mapping at all (empty document) falls back to line 0.
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

    /// Collect the heading chain enclosing buffer line `anchor_line`: all headings at or
    /// before it, then the strictly-shallower suffix walked back from the last one, which
    /// drops earlier siblings the line isn't actually under.
    fn section_chain_for_buffer_line(&self, anchor_line: usize) -> Vec<String> {
        let mut at_or_before: Vec<(HeadingLevel, String)> = Vec::new();
        for (block_idx, block) in self.parsed.blocks.iter().enumerate() {
            let Block::Heading { level, inlines, .. } = block else {
                continue;
            };
            let Some(range) = self.parsed.real_ranges.get(block_idx) else {
                continue;
            };
            let buffer_line = self.buffer.byte_to_line(range.start);
            if buffer_line > anchor_line {
                break;
            }
            at_or_before.push((*level, heading_plain_text(inlines)));
        }

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
        let st = state_from("prelude\n\n# Top\n", 0);
        assert!(st.cursor_section_chain().is_empty());
    }

    #[test]
    fn returns_only_heading_when_under_single_h1() {
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
        let src = "# Top\n\n### Deep\n\nbody\n";
        let cursor = src.find("body").unwrap();
        let st = state_from(src, cursor);
        assert_eq!(
            st.cursor_section_chain(),
            vec!["Top".to_string(), "Deep".to_string()]
        );
    }

    #[test]
    fn scroll_chain_follows_viewport_top() {
        let src = "# A\n\n## B\n\nbody\n";
        let st = state_from(src, 0);
        assert_eq!(st.cursor_section_chain(), vec!["A".to_string()]);

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
