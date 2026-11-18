//! Heading-ancestor chain for the status-bar breadcrumb (`notes.md › Checkpoint 1 › Item 1`).
//! Mirrors the heading walk in `App::open_section_picker` but lives a layer down so the UI
//! can call it without depending on `app`.

use pulldown_cmark::HeadingLevel;

use crate::editor::{EditorState, Mode};
use crate::markdown::{ast::heading_plain_text, Block};

impl EditorState {
    /// Headings whose scope contains [`Self::section_anchor_line`], shallowest first; empty when
    /// none precedes it.  O(blocks), cheap enough for every status-bar redraw.
    pub fn section_chain(&self, width: usize) -> Vec<String> {
        self.section_chain_for_buffer_line(self.section_anchor_line(width))
    }

    /// Buffer line that defines "the section you're in", shared by the breadcrumb and the Go to
    /// section preselection so the two can't disagree: the viewport top in Preview, where the
    /// hidden cursor doesn't follow scrolling (see [`Self::scroll_anchor_line`]), else the cursor
    /// line.  `width` is the document area width `scroll` was measured at.
    pub fn section_anchor_line(&self, width: usize) -> usize {
        match self.mode {
            Mode::Preview => self.scroll_anchor_line(width),
            _ => self.buffer.char_to_line(self.cursor.offset),
        }
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
    use crate::editor::{EditorState, Mode};

    /// A Rendered-mode state, so the chain follows the cursor; the scroll tests switch to Preview.
    fn state_from(src: &str, cursor_offset: usize) -> EditorState {
        let theme = Box::leak(Box::new(crate::config::Theme::default()));
        let mut st = EditorState::new(Buffer::from_str(src), theme);
        st.mode = Mode::Rendered;
        st.cursor.offset = cursor_offset;
        st
    }

    #[test]
    fn empty_when_no_headings() {
        let st = state_from("just a paragraph\n", 0);
        assert!(st.section_chain(80).is_empty());
    }

    #[test]
    fn empty_when_cursor_precedes_first_heading() {
        let st = state_from("prelude\n\n# Top\n", 0);
        assert!(st.section_chain(80).is_empty());
    }

    #[test]
    fn returns_only_heading_when_under_single_h1() {
        let src = "# Top\n\nbody text\n";
        let cursor = src.find("body").unwrap();
        let st = state_from(src, cursor);
        assert_eq!(st.section_chain(80), vec!["Top".to_string()]);
    }

    #[test]
    fn returns_full_chain_in_document_order() {
        let src = "# A\n\n## B\n\n### C\n\nbody\n";
        let cursor = src.find("body").unwrap();
        let st = state_from(src, cursor);
        assert_eq!(
            st.section_chain(80),
            vec!["A".to_string(), "B".to_string(), "C".to_string()]
        );
    }

    #[test]
    fn skips_earlier_sibling_at_same_level() {
        let src = "# Top\n\n## A1\n\nfirst body\n\n## A2\n\n### B\n\nsecond body\n";
        let cursor = src.find("second body").unwrap();
        let st = state_from(src, cursor);
        assert_eq!(
            st.section_chain(80),
            vec!["Top".to_string(), "A2".to_string(), "B".to_string()]
        );
    }

    #[test]
    fn cursor_on_heading_line_includes_that_heading() {
        let src = "# A\n\n## B\n\nbody\n";
        let cursor = src.find("## B").unwrap();
        let st = state_from(src, cursor);
        assert_eq!(st.section_chain(80), vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn skipped_heading_levels_are_handled() {
        let src = "# Top\n\n### Deep\n\nbody\n";
        let cursor = src.find("body").unwrap();
        let st = state_from(src, cursor);
        assert_eq!(
            st.section_chain(80),
            vec!["Top".to_string(), "Deep".to_string()]
        );
    }

    #[test]
    fn scroll_chain_follows_viewport_top() {
        let src = "# A\n\n## B\n\nbody\n";
        let st = state_from(src, 0);
        assert_eq!(st.section_chain(80), vec!["A".to_string()]);

        let body_byte = src.find("body").unwrap();
        let body_line = st
            .parsed
            .source_map
            .rendered_lines_for_byte(body_byte)
            .start;
        let mut st = st;
        st.mode = Mode::Preview;
        st.scroll = st.parsed.visual_rows_before(body_line, 80);
        assert_eq!(st.section_chain(80), vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn scroll_anchor_counts_wrapped_rows() {
        // Regression: `scroll` was read as a rendered-line index, so a viewport partway down a
        // wrapped paragraph anchored several rendered lines further on, past `## B`.
        let para = "word ".repeat(40);
        let src = format!("# A\n\n{para}\n\n## B\n\nbody\n");
        let width = 20;
        let mut st = state_from(&src, 0);
        st.mode = Mode::Preview;
        let para_line = st
            .parsed
            .source_map
            .rendered_lines_for_byte(src.find("word").unwrap())
            .start;
        assert!(st.parsed.visual_rows_for_line_at(para_line, width) > 5);
        st.scroll = st.parsed.visual_rows_before(para_line, width) + 4;
        assert_eq!(st.scroll_anchor_line(width), 2);
        assert_eq!(st.section_chain(width), vec!["A".to_string()]);
    }

    /// `scroll_anchor_line` skips resolving the column, so the breadcrumb stays cheap deep inside
    /// a long block; it must still name the line `scroll_anchor_offset` (the cursor sync) lands
    /// on, on every row of every kind of block, wrapped or not.
    #[test]
    fn scroll_anchor_line_agrees_with_scroll_anchor_offset_on_every_row() {
        let src = "---\ntitle: t\n---\n\nSetext\n======\n\n\
            A reflowed paragraph whose\nsoft breaks join lines that\nwrap at narrow widths.\n\n\
            - item one that is long enough to wrap\n  - [ ] nested task\n\n  continued\n- two\n\n\
            > quote line one\nlazy continuation\n>\n> [ref]: https://example.com\n\n\
            | head | a much longer heading cell |\n|---|---|\n| 1 | a cell that wraps |\n| 2 | x |\n\n\
            ```rust\nfn main() {}\nlet x = 1;\n```\n\n<!-- a comment -->\n\n\
            Text with a note[^1].\n\n[^1]: The note.\n\n***\n\n日本語の段落はここで折り返します。\n\n\n\
            last line without newline";
        let buffer = Buffer::from_str(src);
        // In Rendered mode the cursor's block is revealed, so each line's cursor exercises the
        // raw-source rows (a stacked reflowed paragraph, a revealed table or code row).
        let line_starts: Vec<usize> = (0..buffer.line_count())
            .map(|l| buffer.line_to_char(l))
            .collect();
        for mode in [Mode::Preview, Mode::Rendered, Mode::Raw] {
            let (cursors, widths): (&[usize], &[usize]) = if mode == Mode::Rendered {
                (&line_starts, &[80, 20])
            } else {
                (&[0], &[80, 20, 7])
            };
            for &cursor in cursors {
                for &width in widths {
                    let mut st = state_from(src, cursor);
                    st.mode = mode;
                    for scroll in 0..st.total_visual_rows_for_mode(width) + 2 {
                        st.scroll = scroll;
                        let offset = st.scroll_anchor_offset(width);
                        assert_eq!(
                            st.scroll_anchor_line(width),
                            st.buffer.char_to_line(offset),
                            "{mode:?}, cursor {cursor}, width {width}, scroll {scroll}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn formatted_heading_text_is_flattened() {
        let src = "## **Bold** and `code`\n\nbody\n";
        let cursor = src.find("body").unwrap();
        let st = state_from(src, cursor);
        assert_eq!(st.section_chain(80), vec!["Bold and code".to_string()]);
    }
}
