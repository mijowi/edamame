//! "Go to section" support: build the heading list at modal-open time,
//! debounce live-preview scroll while the modal is open, and apply the
//! confirm / cancel transitions when it closes.
//!
//! The debounce mirrors the autosave shape — an `Option<Instant>` on
//! `App`, contributed to [`App::next_deadline`] via
//! [`App::section_jump_deadline`], drained in
//! [`App::tick_section_jump`] (called from `tick_timers`).  Holding
//! `↓` on the picker resets the timer; when the user lets go for
//! `SECTION_JUMP_DELAY` the most-recent `target_scroll` is applied and
//! the timer clears.

use std::time::{Duration, Instant};

use crate::editor::Mode;
use crate::markdown::{ast::heading_plain_text, Block};
use crate::ui::HeadingEntry;

use super::App;

/// Debounce window for live-preview scrolls.  Long enough to absorb a
/// held arrow key's autorepeat (~50 ms cadence on most platforms);
/// short enough that a tap feels immediate.  Tuned by feel — same
/// order of magnitude as the `RAW_REVEAL_DELAY` jitter suppression.
pub(super) const SECTION_JUMP_DELAY: Duration = Duration::from_millis(150);

impl App {
    /// Open the "Go to section" modal.  Walks `ParsedDoc::blocks` to
    /// collect every `Block::Heading`, precomputes the
    /// `target_scroll` for each one in the active mode, picks the
    /// preselected entry (nearest heading at or before the cursor's
    /// buffer line), and pushes a [`SectionPickerModal`] onto the
    /// modal stack.
    ///
    /// `doc_width` is the viewport width used to derive `target_scroll`
    /// — runs at the live document area width so the modal's previews
    /// land at the same row the editor will paint.
    pub fn open_section_picker(&mut self, doc_width: usize) {
        let entries = collect_heading_entries(&self.editor, doc_width);
        let cursor_line = self.editor.buffer.char_to_line(self.editor.cursor.offset);
        let focused = preselected_index(&entries, cursor_line);
        let original_scroll = self.editor.scroll;
        self.modal_stack
            .push(Box::new(super::modal::SectionPickerModal::new(
                entries,
                focused,
                original_scroll,
            )));
        self.needs_draw = true;
    }

    /// Live-preview path: arm (or extend) the debounce window so the
    /// scroll fires after the user stops navigating.  Multiple calls
    /// during a held arrow keep resetting the timer; the run loop
    /// applies the most-recent `target_scroll` once
    /// [`SECTION_JUMP_DELAY`] elapses.
    pub(crate) fn arm_section_jump(&mut self, target_scroll: usize) {
        self.section_jump_pending_since = Some(Instant::now());
        self.section_jump_target_scroll = Some(target_scroll);
    }

    /// Esc path: restore the original viewport snapshot and clear any
    /// pending preview so a debounce that's mid-flight doesn't fire
    /// after we've already reverted.
    pub(crate) fn cancel_section_jump(&mut self, original_scroll: usize) {
        self.section_jump_pending_since = None;
        self.section_jump_target_scroll = None;
        if self.editor.scroll != original_scroll {
            self.editor.scroll = original_scroll;
            self.mark_scrolling();
        }
        self.needs_draw = true;
    }

    /// Enter path: apply the target scroll immediately (overriding any
    /// pending debounce) and move the cursor to the end of
    /// `buffer_line`.  Cursor placement is a no-op in Preview mode
    /// visually — the cursor isn't drawn — but we still set
    /// `cursor.offset` so a later mode switch lands the cursor at the
    /// right place.
    pub(crate) fn commit_section_jump(&mut self, buffer_line: usize, target_scroll: usize) {
        self.section_jump_pending_since = None;
        self.section_jump_target_scroll = None;
        let scroll_changed = self.editor.scroll != target_scroll;
        self.editor.scroll = target_scroll;
        // Move the cursor to the end of the heading line (after the
        // last text char, before the trailing newline if any).
        let end_offset = end_of_line_offset(&self.editor, buffer_line);
        self.editor.cursor.offset = end_offset;
        self.editor.cursor.preferred_col = self.editor.cursor.cell_col(&self.editor.buffer);
        self.editor.update_cursor_block();
        if scroll_changed {
            self.mark_scrolling();
        }
        self.needs_draw = true;
    }

    /// Per-iteration debounce step.  When the pending timer has been
    /// armed for at least [`SECTION_JUMP_DELAY`], apply the stashed
    /// target scroll and clear the timer.
    pub(super) fn tick_section_jump(&mut self) {
        let Some(since) = self.section_jump_pending_since else {
            return;
        };
        if since.elapsed() < SECTION_JUMP_DELAY {
            return;
        }
        if let Some(target) = self.section_jump_target_scroll.take() {
            if self.editor.scroll != target {
                self.editor.scroll = target;
                self.mark_scrolling();
                self.needs_draw = true;
            }
        }
        self.section_jump_pending_since = None;
    }

    /// Earliest instant the run loop must wake to apply a pending
    /// section-jump scroll.  Contributed to [`App::next_deadline`] so
    /// `recv_timeout` wakes exactly when the window expires — no
    /// polling.
    pub(super) fn section_jump_deadline(&self) -> Option<Instant> {
        self.section_jump_pending_since
            .map(|t| t + SECTION_JUMP_DELAY)
    }
}

/// Build the heading-entry list from the editor's parsed document.
/// `target_scroll` is computed per current mode using the visual-row
/// helpers on `ParsedDoc` (Rendered/Preview) and `EditorState` (Raw).
fn collect_heading_entries(
    state: &crate::editor::EditorState,
    doc_width: usize,
) -> Vec<HeadingEntry> {
    let mut entries: Vec<HeadingEntry> = Vec::new();
    let width = doc_width.max(1);
    for (block_idx, block) in state.parsed.blocks.iter().enumerate() {
        let Block::Heading { level, inlines } = block else {
            continue;
        };
        let text = heading_plain_text(inlines);
        let Some(range) = state.parsed.real_ranges.get(block_idx) else {
            continue;
        };
        let byte_start = range.start;
        let buffer_line = state.buffer.byte_to_line(byte_start);
        let target_scroll = match state.mode {
            Mode::Raw => state.visual_rows_before_raw_line(buffer_line, width),
            _ => {
                // `block_idx` indexes `parsed.blocks` / `real_ranges`, which
                // contains only real blocks; the source map's index space
                // also includes blank-line virtual blocks, so a direct
                // `rendered_lines_for_block(block_idx)` would land on the
                // wrong block whenever blank lines separate real blocks.
                // Route through the byte → virtual-idx → rendered lookup.
                let rendered = state.parsed.source_map.rendered_lines_for_byte(byte_start);
                state.parsed.visual_rows_before(rendered.start, width)
            }
        };
        entries.push(HeadingEntry {
            level: *level,
            text,
            buffer_line,
            target_scroll,
        });
    }
    entries
}

/// Find the index of the heading whose buffer line is the largest one
/// `<= cursor_line`.  Returns `0` when the cursor sits before every
/// heading (or the list is empty).  Relies on the document-order
/// invariant of `entries`: a `take_while` is sound because no later
/// entry can have a smaller `buffer_line`.
fn preselected_index(entries: &[HeadingEntry], cursor_line: usize) -> usize {
    entries
        .iter()
        .take_while(|e| e.buffer_line <= cursor_line)
        .count()
        .saturating_sub(1)
}

/// Char offset of the end of `line_idx` (after the last text char,
/// before any trailing `\n`).
fn end_of_line_offset(state: &crate::editor::EditorState, line_idx: usize) -> usize {
    let line_count = state.buffer.line_count();
    if line_count == 0 {
        return 0;
    }
    let idx = line_idx.min(line_count - 1);
    let start = state.buffer.line_to_char(idx);
    let line_slice = state.buffer.rope().line(idx);
    let len = line_slice.len_chars();
    // A non-final line ends in `\n` (or `\r\n` on CRLF files —
    // `std::fs::read_to_string` doesn't normalise on Linux/macOS).
    // Trim the line ending from the offset so the cursor lands before
    // it rather than at column 0 of the next line.
    let trim = if len > 0 && line_slice.char(len - 1) == '\n' {
        if len >= 2 && line_slice.char(len - 2) == '\r' {
            2
        } else {
            1
        }
    } else {
        0
    };
    start + len.saturating_sub(trim)
}

#[cfg(test)]
mod tests {
    use pulldown_cmark::HeadingLevel;

    use super::*;
    use crate::app::test_utils::make_app;
    use crate::editor::Mode;

    fn load(app: &mut App, src: &str) {
        app.editor.buffer = crate::document::Buffer::from_str(src);
        app.editor.refresh_parsed();
    }

    #[test]
    fn collect_entries_target_scroll_is_correct_with_blank_separator_blocks() {
        // Regression: previously the collector indexed
        // `rendered_lines_for_block` with the `parsed.blocks` index,
        // which doesn't include blank-line virtual blocks — so the
        // second heading's target_scroll was off whenever a blank
        // line separated it from the first block.
        let mut app = make_app();
        load(&mut app, "# First\n\n## Second\n\nbody\n");
        app.editor.mode = Mode::Rendered;
        let entries = collect_heading_entries(&app.editor, 80);
        assert_eq!(entries.len(), 2);
        // First heading starts at rendered-line 0.
        assert_eq!(entries[0].target_scroll, 0);
        // Second heading must point past the blank line(s) separating
        // the two blocks, not at the blank's rendered position.
        assert!(
            entries[1].target_scroll >= 2,
            "target_scroll for second heading was {}; expected at least 2 \
             rendered rows (heading + blank) before it",
            entries[1].target_scroll
        );
    }

    #[test]
    fn collect_entries_includes_every_heading() {
        let mut app = make_app();
        load(&mut app, "# A\n\nsome text\n\n## B\n\n### C\n\nmore text\n");
        let entries = collect_heading_entries(&app.editor, 80);
        let texts: Vec<&str> = entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, vec!["A", "B", "C"]);
    }

    #[test]
    fn preselected_picks_largest_heading_at_or_before_cursor() {
        let entries = vec![
            HeadingEntry {
                level: HeadingLevel::H1,
                text: "Top".into(),
                buffer_line: 0,
                target_scroll: 0,
            },
            HeadingEntry {
                level: HeadingLevel::H2,
                text: "Middle".into(),
                buffer_line: 5,
                target_scroll: 6,
            },
            HeadingEntry {
                level: HeadingLevel::H2,
                text: "Bottom".into(),
                buffer_line: 20,
                target_scroll: 30,
            },
        ];
        assert_eq!(preselected_index(&entries, 0), 0);
        assert_eq!(preselected_index(&entries, 3), 0);
        assert_eq!(preselected_index(&entries, 5), 1);
        assert_eq!(preselected_index(&entries, 19), 1);
        assert_eq!(preselected_index(&entries, 100), 2);
    }

    #[test]
    fn preselected_returns_zero_for_empty_or_cursor_above_first() {
        assert_eq!(preselected_index(&[], 0), 0);
        let entries = vec![HeadingEntry {
            level: HeadingLevel::H1,
            text: "Only".into(),
            buffer_line: 4,
            target_scroll: 0,
        }];
        assert_eq!(preselected_index(&entries, 0), 0);
    }

    #[test]
    fn end_of_line_offset_lands_before_trailing_newline() {
        let mut app = make_app();
        load(&mut app, "abc\ndef\n");
        // Line 0 is "abc\n" — end_of_line should be after 'c', i.e. 3.
        assert_eq!(end_of_line_offset(&app.editor, 0), 3);
        // Line 1 is "def\n" — start 4, end at 7.
        assert_eq!(end_of_line_offset(&app.editor, 1), 7);
    }

    #[test]
    fn end_of_line_offset_handles_crlf_line_endings() {
        let mut app = make_app();
        load(&mut app, "abc\r\ndef\r\n");
        // Line 0 is "abc\r\n" — end_of_line should land after 'c'
        // (offset 3), trimming both `\r` and `\n`.
        assert_eq!(end_of_line_offset(&app.editor, 0), 3);
        // Line 1 starts at 5 ("def\r\n"); end should be after 'f' (8).
        assert_eq!(end_of_line_offset(&app.editor, 1), 8);
    }

    #[test]
    fn commit_section_jump_moves_cursor_to_end_of_heading_line() {
        let mut app = make_app();
        load(&mut app, "# Heading one\n\nbody\n");
        app.editor.mode = Mode::Rendered;
        app.commit_section_jump(0, 0);
        // "# Heading one" is 13 chars; cursor should land at offset 13.
        assert_eq!(app.editor.cursor.offset, 13);
    }

    #[test]
    fn cancel_section_jump_restores_original_scroll() {
        let mut app = make_app();
        load(&mut app, "# Hi\n\nbody\n");
        app.editor.scroll = 7;
        app.cancel_section_jump(2);
        assert_eq!(app.editor.scroll, 2);
        assert!(app.section_jump_pending_since.is_none());
        assert!(app.section_jump_target_scroll.is_none());
    }

    #[test]
    fn arm_then_cancel_clears_pending_preview() {
        let mut app = make_app();
        app.arm_section_jump(10);
        assert!(app.section_jump_pending_since.is_some());
        app.cancel_section_jump(0);
        assert!(app.section_jump_pending_since.is_none());
        assert!(app.section_jump_target_scroll.is_none());
    }

    #[test]
    fn tick_applies_target_after_window_elapses() {
        let mut app = make_app();
        app.arm_section_jump(5);
        // Force the pending timestamp into the past so the tick fires.
        app.section_jump_pending_since =
            Some(Instant::now() - SECTION_JUMP_DELAY - Duration::from_millis(5));
        app.tick_section_jump();
        assert_eq!(app.editor.scroll, 5);
        assert!(app.section_jump_pending_since.is_none());
        assert!(app.section_jump_target_scroll.is_none());
    }

    #[test]
    fn tick_no_op_before_window_elapses() {
        let mut app = make_app();
        app.arm_section_jump(5);
        app.tick_section_jump();
        // Just-armed timer: scroll should not have advanced yet.
        assert_eq!(app.editor.scroll, 0);
        assert!(app.section_jump_pending_since.is_some());
    }
}
