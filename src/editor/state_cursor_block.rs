//! Cursor-block tracking and the jitter-suppression reveal timer.

use std::time::Instant;

use crate::editor::state::ImageReveal;
use crate::editor::{EditorState, Mode, RAW_REVEAL_DELAY};

impl EditorState {
    /// Call after any cursor movement in Rendered mode: refreshes the cursor block, its
    /// buffer line range, and the raw-reveal timer.  The timer resets on every change of
    /// **buffer line** (not block), so a fifty-line table reveals row by row with the same
    /// delay as a one-line paragraph.
    pub fn update_cursor_block(&mut self) {
        let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
        let previous_block_idx = self.cursor_block_idx;
        self.cursor_block_idx = self.parsed.source_map.block_for_byte(cursor_byte);

        // Lets rendered_view extract the raw block source during a typing burst without
        // consulting the stale source_map; in-line edits never move line indices.
        self.cursor_block_line_range = self.cursor_block_idx.and_then(|idx| {
            let byte_range = self.parsed.source_map.original_range_for_block(idx)?;
            let rope = self.buffer.rope();
            let total_bytes = rope.len_bytes();
            let start_byte = byte_range.start.min(total_bytes);
            let end_byte = byte_range.end.min(total_bytes);
            let start_char = rope.byte_to_char(start_byte);
            // `end_byte - 1` so a range ending on `\n` doesn't claim the next line.
            let end_char = rope.byte_to_char(end_byte.saturating_sub(1).max(start_byte));
            let start_line = rope.char_to_line(start_char);
            let end_line = rope.char_to_line(end_char).max(start_line);
            Some(start_line..end_line + 1)
        });

        // Crossing into a different block, or into or out of a stacked row (a reflowed paragraph
        // can be one of many in a list or quote), drops the "revealed as one unit" latch: the new
        // block or row must earn its own reveal (a dwell, or the immediate entry-from-below case
        // below).
        let previous_unit = self.cursor_stacked_unit;
        let stacked = self.cursor_stacked_row();
        self.cursor_stacked_unit = stacked.as_ref().map(|stacked| stacked.unit());
        let entering = previous_block_idx != self.cursor_block_idx
            || previous_unit != self.cursor_stacked_unit;
        if entering {
            self.cursor_reveal_latched = false;
        }

        let (current_line, _) = self.cursor.line_col(&self.buffer);
        if Some(current_line) != self.cursor_line_idx {
            self.cursor_line_idx = Some(current_line);
            // Re-arm the delay on every buffer-line change — the same beat every block gets — so
            // scrolling *through* a block never dwells long enough to reveal it.  (A diagram
            // (mermaid / `$$` math) and a stacked row then stay revealed once a dwell latches
            // them, via `cursor_reveal_latched`, so re-arming here doesn't flash them collapsed
            // mid-block.)
            //
            // The one exception is *entering* a stacked row on a line other than its first — an
            // upward move or a click.  Its raw form is taller than its rendered form, so during
            // the delay the collapsed row can't show the cursor on its true line: it would sit on
            // that top row and then drop when the row expands.  Reveal such an entry at once (and
            // latch it) so the cursor lands on the right line immediately.  A top-line entry (a
            // downward move) keeps the delay — its line *is* the collapsed row, so nothing jumps
            // and fast downward scrolling stays smooth.
            let off_first_line = stacked.is_some_and(|stacked| {
                self.cursor_block_line_range
                    .as_ref()
                    .is_some_and(|r| current_line != r.start + stacked.lines.start as usize)
            });
            if entering && off_first_line {
                self.cursor_block_entered_at = None;
                self.cursor_reveal_latched = true;
            } else {
                self.cursor_block_entered_at = Some(Instant::now());
            }
        }
        self.cursor_blink.reset();
    }

    /// [`Self::update_cursor_block`] as if the cursor had just entered its block and line: the
    /// latch drops and the reveal delay restarts (or, off a stacked row's first line, reveals at
    /// once).  A text drag calls it on every move, so after mouse-up the reveal waits out
    /// [`RAW_REVEAL_DELAY`] from the drag's last move even when the drag never left its line.
    pub fn rearm_cursor_reveal(&mut self) {
        self.cursor_block_idx = None;
        self.cursor_line_idx = None;
        self.update_cursor_block();
    }

    /// Whether the cursor should be painted this frame.
    pub fn cursor_visible(&self) -> bool {
        self.terminal_focused
            && self.mode != Mode::Preview
            && (self.modal_open || self.cursor_blink.is_visible())
    }

    /// Whether the cursor block should show raw source.  False during the `RAW_REVEAL_DELAY`
    /// window, during a mouse drag (the click anchor must not shift), and while a search or
    /// `:s` preview is active (blocks must not flip to raw under the highlights).  A latched
    /// one-unit block (diagram — mermaid or `$$` math — or stacked row) stays revealed past
    /// a delay re-arm — see [`Self::cursor_reveal_latched`].
    pub fn cursor_block_revealed(&self) -> bool {
        if self.drag_in_progress {
            return false;
        }
        if self.search.is_some() {
            return false;
        }
        if self.substitute_preview.is_some() {
            return false;
        }
        if self.cursor_reveal_latched {
            return true;
        }
        match self.cursor_block_entered_at {
            None => true,
            Some(t) => t.elapsed() >= RAW_REVEAL_DELAY,
        }
    }

    /// Latch the reveal of a "reveal as one unit" block — a diagram (mermaid or `$$` math) or a
    /// stacked row (a reflowed paragraph, any row over several lines) — once it has been revealed
    /// by a dwell, so it stays revealed while the cursor remains inside even as line moves re-arm
    /// the delay.  Called once per frame from
    /// `App::prepare_viewport`.  Other blocks (tables, code) are left to the per-line delay, so
    /// they keep revealing row by row and hide again under a moving cursor.
    pub fn latch_cursor_reveal(&mut self) {
        if self.cursor_reveal_latched || self.mode != Mode::Rendered {
            return;
        }
        let is_one_unit = self.cursor_block_idx.is_some_and(|idx| {
            self.parsed.is_diagram_reveal_block(idx) || self.cursor_stacked_row().is_some()
        });
        if is_one_unit && self.cursor_block_revealed() {
            self.cursor_reveal_latched = true;
        }
    }

    /// Bring [`EditorState::image_reveal`] in line with the cursor, re-parsing when it
    /// changed; returns `true` when it did.  Called every event-loop pass from
    /// `App::prepare_viewport` because the reveal is time-driven and has no action site of
    /// its own, so the no-op path must not allocate.
    pub fn sync_image_reveal(&mut self) -> bool {
        let target = self.image_reveal_target();
        let unchanged = match (target, self.image_reveal.as_ref()) {
            (Some((ordinal, url, rows, preview_rows)), Some(cur)) => {
                ordinal == cur.ordinal
                    && url == cur.url.as_str()
                    && rows == cur.rows
                    && preview_rows == cur.preview_rows
            }
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return false;
        }
        self.image_reveal = target.map(|(ordinal, url, rows, preview_rows)| ImageReveal {
            ordinal,
            url: url.to_owned(),
            rows,
            preview_rows,
        });
        // Source is untouched, so every byte range survives the re-parse.
        self.refresh_parsed();
        true
    }

    /// The reservation the reveal wants for the cursor position: `(ordinal into
    /// `ParsedDoc::image_blocks`, URL, raw-source rows, preview-band rows)`, or `None` outside a
    /// revealed image block.  See [`ImageReveal`] for why the URL alone can't name a block.
    /// The URL is borrowed, not cloned, because this runs every event-loop pass.
    fn image_reveal_target(&self) -> Option<(usize, &str, usize, usize)> {
        if self.mode != Mode::Rendered {
            return None;
        }
        // Mid-typing the parse (and a diagram's source-hashed URL) is stale; an in-line edit
        // can't change the line count, so hold the current reservation.
        if self.parsed_dirty {
            return self
                .image_reveal
                .as_ref()
                .map(|r| (r.ordinal, r.url.as_str(), r.rows, r.preview_rows));
        }
        if !self.cursor_block_revealed() {
            return None;
        }
        let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
        let block_idx = self.parsed.source_map.block_for_byte(cursor_byte)?;
        if !self.parsed.is_image_block(block_idx) {
            return None;
        }
        let ordinal = self
            .parsed
            .image_blocks
            .iter()
            .position(|info| info.block_idx == block_idx)?;
        let url = self.parsed.image_blocks[ordinal].url.as_str();
        let range = self.parsed.source_map.original_range_for_block(block_idx)?;
        // Parse-time range, so read the parse-time source (also avoids `Buffer::contents()`
        // allocating the whole document every frame).
        let contents = self.parsed.source();
        let source = contents.get(range.start..range.end.min(contents.len()))?;
        // Same split the painter uses, so reserved rows and painted lines can't disagree.
        let raw_rows = crate::ui::rendered_view::revealed_source_line_count(source);
        // A `$$...$$` block with preview on reserves a top band for the decoded formula (keeping
        // its pre-reveal position) while the source paints below.  Same row count the renderer's
        // override gives the image outside the reveal, so it doesn't resize when the reveal opens;
        // zero for mermaid / plain images / preview off.
        let preview_rows = if self.math_preview && self.parsed.is_latex_block(block_idx) {
            let max_w = self.image_max_width.min(u16::MAX as usize) as u16;
            let max_h = self.image_max_height.min(u16::MAX as usize) as u16;
            // `aspect_rows`, not `reserved_rows`: the former answers `None` for a *failed* decode
            // as well as a pending one, so an invalid formula falls through to the same "keep the
            // last resolved band" branch as an in-flight one.  `reserved_rows` would instead
            // collapse the band to one row (`Some(1)`) the instant an intermediate keystroke fails
            // to parse, then spring it back when the formula is valid again — the janky mid-typing
            // reflow this branch exists to prevent.
            self.images
                .aspect_rows(url, max_w, max_h, self.image_font_size)
                .unwrap_or_else(|| {
                    // URL unknown (still decoding, a failed/invalid render, or a keystroke's
                    // throwaway hash the debounce is holding): keep this block's last resolved band
                    // so it doesn't jump to the placeholder while typing, falling back to the
                    // placeholder only with no prior.
                    self.image_reveal
                        .as_ref()
                        .filter(|r| r.ordinal == ordinal && r.preview_rows > 0)
                        .map_or(self.image_max_height, |r| r.preview_rows)
                })
        } else {
            0
        };
        Some((ordinal, url, raw_rows, preview_rows))
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Theme;
    use crate::document::Buffer;
    use crate::editor::{EditorState, Mode};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    /// Once a reflowed paragraph is revealed (latched by a dwell), moving between its source lines
    /// keeps it revealed even though each move re-arms the delay — so it never flashes collapsed
    /// mid-block.  The latch, not a suppressed timer, is what holds it.
    #[test]
    fn moving_within_a_revealed_reflowed_paragraph_keeps_it_revealed() {
        let mut st = EditorState::new(Buffer::from_str("one\ntwo\nthree\n\nafter\n"), theme());
        st.mode = Mode::Rendered;
        st.set_viewport_width(80);
        st.sync_reflow_for_mode();
        // Cursor rests on the paragraph's first source line; dwell reveals it, and the per-frame
        // latch step then pins it revealed.
        st.cursor.offset = 0;
        st.update_cursor_block();
        st.cursor_block_entered_at = None;
        st.latch_cursor_reveal();
        assert!(st.cursor_reveal_latched, "a dwell must latch the reveal");

        // Move down to the second source line: the delay re-arms, but the latch holds the reveal.
        let byte = st.buffer.contents().find("two").unwrap();
        st.cursor.offset = st.buffer.rope().byte_to_char(byte);
        st.update_cursor_block();
        assert!(
            st.cursor_block_entered_at.is_some(),
            "an intra-block line move re-arms the delay, like every other block",
        );
        assert!(
            st.cursor_block_revealed(),
            "the latch must hold the paragraph revealed across the move",
        );
    }

    /// Scrolling *through* a reflowed paragraph (a hold-down that re-arms the delay every line)
    /// must not reveal it — the delay never elapses, exactly as for any other block.  This is the
    /// regression the old "don't re-arm within a one-unit block" shortcut caused: the timer, set
    /// once on entry, elapsed a few lines in and de-rendered the block mid-scroll.
    #[test]
    fn scrolling_through_a_reflowed_paragraph_does_not_reveal_it() {
        let mut st = EditorState::new(
            Buffer::from_str("intro\n\none\ntwo\nthree\nfour\nfive\n\nafter\n"),
            theme(),
        );
        st.mode = Mode::Rendered;
        st.set_viewport_width(80);
        st.sync_reflow_for_mode();
        // Enter from above (first line), the way a downward scroll does.
        let first = st.buffer.contents().find("one").unwrap();
        st.cursor.offset = st.buffer.rope().byte_to_char(first);
        st.update_cursor_block();
        for word in ["two", "three", "four", "five"] {
            let byte = st.buffer.contents().find(word).unwrap();
            st.cursor.offset = st.buffer.rope().byte_to_char(byte);
            st.update_cursor_block(); // re-arms the delay with a fresh instant every line
            st.latch_cursor_reveal();
            assert!(
                !st.cursor_block_revealed(),
                "the block must stay rendered while scrolling through it (at {word})",
            );
        }
    }

    /// Entering a reflowed paragraph on a line other than its first (an upward move or a click)
    /// reveals it immediately, so the cursor lands on its true line at once rather than sitting on
    /// the collapsed flow's top row for `RAW_REVEAL_DELAY` and then dropping.
    #[test]
    fn entering_a_reflowed_paragraph_from_below_reveals_immediately() {
        let mut st = EditorState::new(
            Buffer::from_str("intro\n\none\ntwo\nthree\n\nafter\n"),
            theme(),
        );
        st.mode = Mode::Rendered;
        st.set_viewport_width(80);
        st.sync_reflow_for_mode();
        // Rest below the paragraph, revealed there, then move up onto its last source line.
        let after = st.buffer.contents().find("after").unwrap();
        st.cursor.offset = st.buffer.rope().byte_to_char(after);
        st.update_cursor_block();
        st.cursor_block_entered_at = None;

        let last = st.buffer.contents().find("three").unwrap();
        st.cursor.offset = st.buffer.rope().byte_to_char(last);
        st.update_cursor_block();
        assert!(
            st.cursor_block_entered_at.is_none(),
            "entering a reflowed paragraph on a non-first line must skip the reveal delay",
        );
        assert!(st.cursor_block_revealed());
    }

    /// The downward counterpart: entering a reflowed paragraph on its first line keeps the reveal
    /// delay (its line is the flow row, so nothing jumps, and fast downward scrolling stays smooth).
    #[test]
    fn entering_a_reflowed_paragraph_from_above_keeps_the_delay() {
        let mut st = EditorState::new(
            Buffer::from_str("intro\n\none\ntwo\nthree\n\nafter\n"),
            theme(),
        );
        st.mode = Mode::Rendered;
        st.set_viewport_width(80);
        st.sync_reflow_for_mode();
        let intro = st.buffer.contents().find("intro").unwrap();
        st.cursor.offset = st.buffer.rope().byte_to_char(intro);
        st.update_cursor_block();
        st.cursor_block_entered_at = None;

        let first = st.buffer.contents().find("one").unwrap();
        st.cursor.offset = st.buffer.rope().byte_to_char(first);
        st.update_cursor_block();
        assert!(
            st.cursor_block_entered_at.is_some(),
            "entering on the first line must keep the reveal delay",
        );
    }

    /// A nested reflowed paragraph is a unit of its own, though its block (a list) holds others:
    /// entering one on a line other than its first reveals it at once, from a sibling item's
    /// paragraph as from outside the list.
    #[test]
    fn entering_a_nested_reflowed_paragraph_from_below_reveals_immediately() {
        let src = "- one\n- two\n  three\n- four\n  five\n";
        let mut st = EditorState::new(Buffer::from_str(src), theme());
        st.mode = Mode::Rendered;
        st.set_viewport_width(80);
        st.sync_reflow_for_mode();
        // Revealed and latched in the last item's paragraph.
        st.cursor.offset = src.find("five").unwrap();
        st.update_cursor_block();
        st.cursor_block_entered_at = None;
        st.latch_cursor_reveal();
        assert!(st.cursor_reveal_latched);

        // Up into the item above: same block, another paragraph, entered on its second line.
        st.cursor.offset = src.find("three").unwrap();
        st.update_cursor_block();
        assert!(
            st.cursor_block_entered_at.is_none(),
            "entering a nested reflowed paragraph off its first line must skip the delay",
        );
        assert!(st.cursor_reveal_latched, "and latch it");
        assert_eq!(st.effective_rows(80).raw_lines(), 1..3);
    }

    /// A row a line with no row of its own shares (a quote's hidden link definition, the line
    /// below it) stacks that line only while the cursor is on it.  Moving between the two keeps
    /// the row revealed as one unit, its stack growing or shrinking by the hidden line, and the
    /// reveal patch follows (it is memoized per stacked lines, not just per row).
    #[test]
    fn moving_between_a_hidden_line_and_the_row_it_shares_keeps_it_revealed() {
        let src = "> a\n>\n> [d]: /u\n> b\n\nafter\n";
        let mut st = EditorState::new(Buffer::from_str(src), theme());
        st.mode = Mode::Rendered;
        st.set_viewport_width(80);
        st.sync_reflow_for_mode();
        st.cursor.offset = src.find("[d]").unwrap();
        st.update_cursor_block();
        st.cursor_block_entered_at = None;
        st.latch_cursor_reveal();
        assert!(st.cursor_reveal_latched);
        assert_eq!(st.effective_rows(80).raw_lines(), 2..4);

        st.cursor.offset = src.find("> b").unwrap();
        st.update_cursor_block();
        assert!(st.cursor_reveal_latched, "the same row keeps its latch");
        assert!(st.cursor_block_revealed());
        assert_eq!(st.effective_rows(80).raw_lines(), 3..4);

        st.cursor.offset = src.find("[d]").unwrap();
        st.update_cursor_block();
        assert!(st.cursor_block_revealed());
        assert_eq!(st.effective_rows(80).raw_lines(), 2..4);
    }

    /// Leaving a nested reflowed paragraph for another row of its block (a sibling item's first
    /// line) drops its latch: the new row honors the delay, as a new block would.
    #[test]
    fn leaving_a_nested_reflowed_paragraph_drops_its_latch() {
        let src = "- one\n- two\n  three\n- four\n";
        let mut st = EditorState::new(Buffer::from_str(src), theme());
        st.mode = Mode::Rendered;
        st.set_viewport_width(80);
        st.sync_reflow_for_mode();
        st.cursor.offset = src.find("three").unwrap();
        st.update_cursor_block();
        st.cursor_block_entered_at = None;
        st.latch_cursor_reveal();
        assert!(st.cursor_reveal_latched);

        st.cursor.offset = src.find("four").unwrap();
        st.update_cursor_block();
        assert!(
            !st.cursor_reveal_latched,
            "a sibling paragraph must earn its own reveal"
        );
        assert!(
            !st.cursor_block_revealed(),
            "entering it on its first line keeps the delay"
        );
    }

    /// The counterpart: crossing into a *different* block does re-arm the timer, so the new
    /// block honors the reveal delay (jitter suppression on entry is preserved).
    #[test]
    fn crossing_into_another_block_rearms_the_reveal_timer() {
        let mut st = EditorState::new(Buffer::from_str("one\ntwo\nthree\n\nafter\n"), theme());
        st.mode = Mode::Rendered;
        st.set_viewport_width(80);
        st.sync_reflow_for_mode();
        st.cursor.offset = 0;
        st.update_cursor_block();
        st.cursor_block_entered_at = None;

        // Into the `after` paragraph, a different block.
        let byte = st.buffer.contents().find("after").unwrap();
        st.cursor.offset = st.buffer.rope().byte_to_char(byte);
        st.update_cursor_block();
        assert!(
            st.cursor_block_entered_at.is_some(),
            "crossing into a new block must re-arm the reveal delay",
        );
    }
}
