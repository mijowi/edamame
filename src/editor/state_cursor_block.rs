//! Cursor-block tracking and the jitter-suppression reveal timer.
//!
//! Methods extracted from `EditorState`'s big `impl` block.  Lives on the
//! same struct via Rust's ability to have multiple `impl` blocks across
//! files in the same crate.

use std::time::Instant;

use crate::editor::{EditorState, Mode, RAW_REVEAL_DELAY};

impl EditorState {
    /// Call after any cursor movement in Rendered mode. Tracks which block the
    /// cursor is in and which buffer line it is on. `RenderedView` uses
    /// `cursor_block_entered_at` to delay revealing the raw cursor-block view.
    /// The timer resets whenever the cursor moves to a **different buffer line**
    /// (not just a different block), so that the delay is consistent regardless
    /// of whether the block is a single-line paragraph or a fifty-line table.
    pub fn update_cursor_block(&mut self) {
        let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
        // Always keep cursor_block_idx up-to-date (used by rendered_view for
        // extracting the raw source of the current block).
        self.cursor_block_idx = self.parsed.source_map.block_for_byte(cursor_byte);

        // Cache the cursor block's buffer line range.  Used by rendered_view
        // to extract the raw block source during a typing burst without
        // consulting the (then-stale) source_map.  In-line edits keep line
        // indices stable — no newlines added or removed — so this range
        // stays correct until the cursor moves or a cross-line edit fires
        // refresh_parsed.
        self.cursor_block_line_range = self.cursor_block_idx.and_then(|idx| {
            let byte_range = self.parsed.source_map.original_range_for_block(idx)?;
            let rope = self.buffer.rope();
            let total_bytes = rope.len_bytes();
            let start_byte = byte_range.start.min(total_bytes);
            let end_byte = byte_range.end.min(total_bytes);
            let start_char = rope.byte_to_char(start_byte);
            // Use `end_byte.saturating_sub(1)` so a range that ends on a `\n`
            // doesn't claim the next line.
            let end_char = rope.byte_to_char(end_byte.saturating_sub(1).max(start_byte));
            let start_line = rope.char_to_line(start_char);
            let end_line = rope.char_to_line(end_char).max(start_line);
            Some(start_line..end_line + 1)
        });

        // Reset the reveal timer only when the cursor moves to a different
        // logical buffer line — this makes scrolling through a large table feel
        // uniform: each row gets the same delay, not the whole table at once.
        let (current_line, _) = self.cursor.line_col(&self.buffer);
        if Some(current_line) != self.cursor_line_idx {
            self.cursor_line_idx = Some(current_line);
            self.cursor_block_entered_at = Some(Instant::now());
        }
        self.cursor_blink.reset();
    }

    /// Whether the cursor should be painted this frame.  Combines the
    /// blink state with the current mode — Preview never shows a cursor.
    pub fn cursor_visible(&self) -> bool {
        self.mode != Mode::Preview && (self.modal_open || self.cursor_blink.is_visible())
    }

    /// Returns true when the raw view for the cursor block should be shown.
    /// False during the `RAW_REVEAL_DELAY` window after the cursor entered a
    /// new block (so rapidly-traversed blocks stay rendered), and false
    /// while a mouse drag is in progress (so the user's visible click
    /// anchor doesn't shift under the drag).
    pub fn cursor_block_revealed(&self) -> bool {
        if self.drag_in_progress {
            return false;
        }
        match self.cursor_block_entered_at {
            None => true,
            Some(t) => t.elapsed() >= RAW_REVEAL_DELAY,
        }
    }
}
