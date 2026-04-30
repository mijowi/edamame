use std::time::Instant;

use crate::config::Theme;
use crate::document::{Buffer, Cursor, EditDelta, History, ParsedDoc, Selection, VisualSelection};
use crate::editor::Mode;
use crate::image::ImageCache;

/// All mutable state owned by the editor.
///
/// `EditorState` is the single source of truth for the document contents,
/// cursor position, selection, undo/redo history, and current mode.
/// It is mutated by `edit_ops::apply()` and read by the UI layer.
pub struct EditorState {
    pub buffer: Buffer,
    pub cursor: Cursor,
    pub selection: Option<Selection>,
    /// Preview-mode selection in rendered (visible) coordinates.  Populated
    /// only when `mode == Mode::Preview` — switching to Rendered or Raw
    /// clears it because those modes drive selection from the raw buffer.
    pub visual_selection: Option<VisualSelection>,
    pub history: History,
    pub mode: Mode,
    pub parsed: ParsedDoc,
    /// Whether the buffer has unsaved changes since last save.
    pub dirty: bool,
    /// Internal clipboard (kill-ring). Used as fallback when arboard is
    /// unavailable.
    pub kill_ring: String,
    /// Scroll offset (in rendered lines) for RenderedMode.
    pub scroll: usize,
    /// Block index the cursor is currently inside (used for jitter suppression).
    pub cursor_block_idx: Option<usize>,
    /// Buffer line index the cursor is currently on (used to reset the reveal
    /// timer per logical line rather than per block).
    pub cursor_line_idx: Option<usize>,
    /// When the cursor last moved to a new buffer line. The raw/de-rendered view
    /// for the cursor block is shown only after `RAW_REVEAL_DELAY` has elapsed
    /// without further movement on the same line, preventing jitter when scrolling
    /// quickly through multi-line elements such as tables.
    pub cursor_block_entered_at: Option<Instant>,
    /// True while a mouse click-and-drag is in progress.  While true the
    /// cursor's block is never de-rendered, so the user's drag selection
    /// stays anchored to the rendered characters they clicked on — if the
    /// block reveals to raw mid-drag, the visible columns shift and the
    /// anchor would jump.
    pub drag_in_progress: bool,
    /// Optional theme reference — used to re-render after edits.
    theme: &'static Theme,
    /// Whether to preserve multiple consecutive blank lines between blocks.
    preserve_blank_lines: bool,
    /// Whether Up/Down navigate by visual lines (true) or logical lines (false).
    pub visual_line_nav: bool,
    /// Number of spaces per indent step — used by `Tab` / `Shift+Tab` to
    /// indent or outdent list items and by `InsertTab` outside a list.
    /// Seeded from `config.editor.tab_width`; defaults to 4.
    pub tab_width: usize,
    /// Ceiling (in rendered rows) for each `Block::ImageBlock` — propagated
    /// from `Config::image::max_height` so the renderer, navigation, and
    /// `image_view::paint_images` all agree on the reserved row count.
    pub image_max_height: usize,
    /// Ceiling (in rendered cells) for the horizontal extent of an
    /// image block's bounding box.  Used alongside `image_max_height`
    /// and `image_font_size` to compute the aspect-aware row count per
    /// decoded image — wide images reserve fewer rows than
    /// `image_max_height` because they fit in width before height.
    pub image_max_width: usize,
    /// Font size (width, height) in pixels reported by the detected
    /// image picker.  Default `(10, 20)` mirrors
    /// `Picker::from_fontsize`'s Halfblocks default.  Together with
    /// `image_max_width × image_max_height` this sets the bounding box
    /// in pixels used for aspect-aware row computation.
    pub image_font_size: (u16, u16),
    /// Decoded-image cache keyed by URL, retained across reparses so
    /// ordinary edits don't invalidate the expensive `StatefulProtocol`
    /// encoding.  Populated by the App's image-decode worker thread via
    /// `AppEvent::ImageReady` / `AppEvent::ImageFailed`.
    pub images: ImageCache,
    /// When `false`, every image block collapses to its one-line
    /// `[Image: alt]` placeholder — the row override short-circuits to
    /// `Some(1)` so no blank rows are reserved.  Set by the App when the
    /// user declines image rendering (images-enabled prompt `No` /
    /// `Never`, or `config.images.enabled = "never"`).  Default `true`
    /// preserves the cache-driven layout for tests and for the `Ask` /
    /// `Always` paths.
    pub images_enabled: bool,
    /// Monotonically-increasing version counter, bumped every time
    /// `refresh_parsed` rebuilds the `ParsedDoc`.  Consumed by the view
    /// state to invalidate per-frame snapshot caches only when the parse
    /// tree actually changed — a scroll-only change leaves the version
    /// alone, so `build_snapshots` can reuse the previous frame's
    /// geometry.
    pub parsed_version: u64,
    /// Phase 6 live-preview scratch for the column-resize drag.  When
    /// `Some((table_byte_start, widths))`, the table whose first row begins
    /// at `table_byte_start` renders with `widths` applied as a
    /// `user_widths` override — without touching the buffer.  Cleared on
    /// release (when the drag commits via `write_column_widths`) or on any
    /// non-resize action that invalidates the drag.
    pub live_table_widths: Option<(usize, Vec<Option<usize>>)>,
    /// Phase 13 — propagated from `config.table.row_striping`.  Controls
    /// whether the renderer fills alternating data rows with
    /// `Theme::table_row_even` / `Theme::table_row_odd`.  Set by the App
    /// at construction time and re-read on every `refresh_parsed`.
    pub row_striping: bool,
    /// Phase 13 — most-recently observed terminal column width, fed
    /// into `Renderer::with_viewport_width` on every `refresh_parsed`
    /// so the min-max proportional column-width algorithm adapts to
    /// the user's actual viewport.  Set to a sensible 80 default until
    /// the App posts the real width via [`set_viewport_width`].  Stored
    /// in the editor (rather than threaded as a parameter through
    /// `refresh_parsed`) so call-sites that don't know the width — e.g.
    /// undo/redo, paste, file load — pick up the cached value.
    pub viewport_width: usize,
    /// Phase 13 — set on the Release event of a column-border drag, this
    /// flags the App to either commit the in-progress `live_table_widths`
    /// (writing the `tui-columns` comment to the buffer) or open the
    /// width-injection warning modal.  Carries the `table_byte_start` of
    /// the table whose widths are pending.  Cleared by
    /// [`commit_pending_column_widths`] / [`cancel_pending_column_widths`].
    pub pending_column_widths_commit: Option<usize>,
    /// Phase 8 — mouse-ops and edit-ops set this when a click / keypress
    /// requests a link be followed.  The App consumes it on the next
    /// loop iteration and dispatches to its own navigation stack /
    /// worker threads.  Storing the intent on `EditorState` keeps
    /// `mouse_ops::apply` pure w.r.t. its `&mut EditorState` contract —
    /// it doesn't need an extra out-parameter or a reference to the App.
    pub pending_link_follow: Option<crate::editor::link::LinkTarget>,
    /// `true` when one or more in-line edits (no newline added or
    /// removed) have been applied since the last `refresh_parsed`,
    /// leaving `parsed` stale.  The rendered view keeps the cursor
    /// block displayed raw from the buffer — independent of
    /// `source_map` byte ranges — so the staleness is invisible
    /// until the user moves off the line (or a mouse click / other
    /// parse-dependent path consults `flush_parsed_if_dirty`).
    /// Cross-line edits (newline insert, backspace-at-line-start)
    /// call `refresh_parsed` immediately instead of setting this.
    pub parsed_dirty: bool,
    /// Buffer line range of the block that currently contains the
    /// cursor, as of the last `update_cursor_block`.  Stable across
    /// in-line typing (no newlines → line indices don't shift), so
    /// the rendered view can extract the cursor block's raw text
    /// from the live buffer without consulting the stale
    /// `source_map`.  `None` for empty documents / uninitialised state.
    pub cursor_block_line_range: Option<std::ops::Range<usize>>,
}

/// How long the cursor must rest on a block before it is shown in raw mode.
pub const RAW_REVEAL_DELAY: std::time::Duration = std::time::Duration::from_millis(120);

impl EditorState {
    /// Create an `EditorState` from a `Buffer` and a theme.
    ///
    /// # Panics
    ///
    /// Panics if the theme reference has an insufficiently long lifetime.
    /// Callers typically pass `Box::leak(Box::new(Theme::default()))` or a
    /// static reference.
    pub fn new(buffer: Buffer, theme: &'static Theme) -> Self {
        Self::new_with_config(buffer, theme, true, true, 24)
    }

    pub fn new_with_config(
        buffer: Buffer,
        theme: &'static Theme,
        preserve_blank_lines: bool,
        visual_line_nav: bool,
        image_max_height: usize,
    ) -> Self {
        Self::new_with_image_config(
            buffer,
            theme,
            preserve_blank_lines,
            visual_line_nav,
            image_max_height,
            80,       // default image_max_width (matches ImagesConfig::default)
            (10, 20), // default font_size (matches Picker::from_fontsize default)
        )
    }

    /// Full constructor with explicit image-layout inputs.  Callers that
    /// know the probed font-size and configured max width (i.e. the App
    /// after terminal capability detection) should use this so
    /// aspect-aware row computation is driven by real values.
    pub fn new_with_image_config(
        buffer: Buffer,
        theme: &'static Theme,
        preserve_blank_lines: bool,
        visual_line_nav: bool,
        image_max_height: usize,
        image_max_width: usize,
        image_font_size: (u16, u16),
    ) -> Self {
        let content = buffer.contents();
        let parsed = ParsedDoc::build(&content, theme, preserve_blank_lines, image_max_height);
        let mut state = Self {
            buffer,
            cursor: Cursor::new(),
            selection: None,
            visual_selection: None,
            history: History::new(),
            mode: Mode::Preview,
            parsed,
            dirty: false,
            kill_ring: String::new(),
            scroll: 0,
            cursor_block_idx: None,
            cursor_line_idx: None,
            cursor_block_entered_at: None,
            drag_in_progress: false,
            theme,
            preserve_blank_lines,
            visual_line_nav,
            tab_width: 4,
            image_max_height,
            image_max_width,
            image_font_size,
            images: ImageCache::new(),
            images_enabled: true,
            parsed_version: 0,
            live_table_widths: None,
            row_striping: false,
            viewport_width: 80,
            pending_column_widths_commit: None,
            pending_link_follow: None,
            parsed_dirty: false,
            cursor_block_line_range: None,
        };
        // Populate the cursor-block cache so the rendered view's
        // stale-map-tolerant path has correct line-range info on the
        // very first frame — before any cursor-move action has run
        // `update_cursor_block` naturally.  Don't start the reveal
        // timer: the document just loaded, there's no prior position
        // to animate from, and tests expect a `None` timer on a
        // freshly-constructed state so the cursor block reveals raw
        // immediately without a 120 ms sleep.
        state.update_cursor_block();
        state.cursor_block_entered_at = None;
        state.cursor_line_idx = None;
        state
    }

    /// Convenience constructor for tests: creates an in-memory buffer from `text`.
    #[cfg(test)]
    pub fn from_str(text: &str, theme: &'static Theme) -> Self {
        Self::new(Buffer::from_str(text), theme)
    }

    // ── Buffer access ─────────────────────────────────────────────

    pub fn contents(&self) -> String {
        self.buffer.contents()
    }

    /// Size of the current selection as `(char_count, line_count)`.
    /// Returns `None` when there is no active selection.  In Preview
    /// mode the visual selection is counted over rendered text; in
    /// Rendered / Raw the raw buffer selection is counted.
    pub fn selection_size(&self) -> Option<(usize, usize)> {
        if self.mode == Mode::Preview {
            let vs = self.visual_selection?;
            if vs.is_empty() {
                return None;
            }
            let ((sr, sc), (er, ec)) = vs.range();
            let mut chars = 0usize;
            let mut lines = 0usize;
            for row in sr..=er {
                let Some(line) = self.parsed.lines.get(row) else {
                    break;
                };
                let full: String = line.spans.iter().flat_map(|s| s.content.chars()).collect();
                let row_len = full.chars().count();
                let col_start = if row == sr { sc } else { 0 };
                let col_end = if row == er { ec.min(row_len) } else { row_len };
                chars += col_end.saturating_sub(col_start);
                lines += 1;
            }
            Some((chars, lines.max(1)))
        } else {
            let sel = self.selection?;
            if sel.is_empty() {
                return None;
            }
            let (start, end) = sel.range();
            let end = end.min(self.buffer.len_chars());
            if start >= end {
                return None;
            }
            let chars = end - start;
            let start_line = self.buffer.char_to_line(start);
            // Selections that end on a line-start (just past a `\n`)
            // conceptually cover one fewer line than the raw end index
            // would suggest; clamp end to a char position that lies
            // on content rather than on the next line's first byte.
            let end_inclusive = end.saturating_sub(1);
            let end_line = self.buffer.char_to_line(end_inclusive.max(start));
            Some((chars, end_line - start_line + 1))
        }
    }

    /// Swap the editor's theme reference and re-render so styled spans
    /// pick up the new palette without the user having to reopen the
    /// document.  Wired to the live theme-change path (settings overlay
    /// "Theme" cycle and the post-`Config::load` reload after the
    /// external editor exits).  No-op when the new reference equals
    /// the current one — same address means same theme, no rebuild
    /// needed.
    pub fn set_theme(&mut self, theme: &'static Theme) {
        if std::ptr::eq(self.theme, theme) {
            return;
        }
        self.theme = theme;
        self.refresh_parsed();
    }

    /// Toggle row striping for table data rows and re-render so the
    /// change is visible on the next frame.  Wired to
    /// `config.table.row_striping` at App startup; tests use this as a
    /// public entrypoint into the otherwise-private `refresh_parsed`.
    pub fn set_row_striping(&mut self, on: bool) {
        if self.row_striping == on {
            return;
        }
        self.row_striping = on;
        self.refresh_parsed();
    }

    /// Update the cached terminal width and re-render if it changed.
    /// Called by the App on terminal-resize events so the table
    /// column-width algorithm picks up the new viewport.  Called with
    /// the document area width (status bar / hint line excluded).
    pub fn set_viewport_width(&mut self, width: usize) {
        let width = width.max(1);
        if self.viewport_width == width {
            return;
        }
        self.viewport_width = width;
        self.refresh_parsed();
    }

    /// Commit the pending column-width drag (if any) by writing the
    /// `<!-- tui-columns: [...] -->` comment into the buffer.  Called by
    /// the App after a column-border drag release once the
    /// width-injection warning modal has been resolved (or skipped).
    /// No-op when no pending commit is recorded — a Cancel from the
    /// modal goes through [`cancel_pending_column_widths`] instead.
    pub fn commit_pending_column_widths(&mut self) {
        let Some(table_byte_start) = self.pending_column_widths_commit.take() else {
            return;
        };
        let live_widths = self
            .live_table_widths
            .as_ref()
            .filter(|(start, _)| *start == table_byte_start)
            .map(|(_, w)| w.clone());
        self.live_table_widths = None;
        let Some(widths) = live_widths else {
            self.refresh_parsed();
            return;
        };
        let source = self.buffer.contents();
        let Some(info) = crate::editor::table_edit::find_table_at(&source, table_byte_start) else {
            self.refresh_parsed();
            return;
        };
        let byte_delta = crate::editor::table_edit::write_column_widths(&source, &info, &widths);
        let rope = self.buffer.rope();
        let char_delta = EditDelta {
            offset: rope.byte_to_char(byte_delta.offset),
            removed: byte_delta.removed,
            inserted: byte_delta.inserted,
        };
        self.apply_delta(char_delta);
    }

    /// Discard the pending column-width drag without writing the comment.
    /// Cancels both the live preview (so the table snaps back to its
    /// pre-drag widths on the next render) and the pending-commit flag.
    pub fn cancel_pending_column_widths(&mut self) {
        self.pending_column_widths_commit = None;
        self.live_table_widths = None;
        self.refresh_parsed();
    }

    /// True if a column-border drag has just released and the App still
    /// needs to decide whether to commit (or open the warning modal).
    pub fn has_pending_column_widths(&self) -> bool {
        self.pending_column_widths_commit.is_some()
    }

    /// Whether the table whose first byte is `table_byte_start` already
    /// carries a `<!-- tui-columns: [...] -->` comment immediately after
    /// it.  Used by the App to skip the width-injection warning when the
    /// comment is already present (the user has already accepted the
    /// injection on a previous drag for this table).
    pub fn table_has_tui_columns_comment(&self, table_byte_start: usize) -> bool {
        let source = self.buffer.contents();
        let Some(info) = crate::editor::table_edit::find_table_at(&source, table_byte_start) else {
            return false;
        };
        if info.end >= source.len() {
            return false;
        }
        let comment_line_end = source[info.end..]
            .find('\n')
            .map(|i| info.end + i)
            .unwrap_or(source.len());
        let comment_line = &source[info.end..comment_line_end];
        crate::markdown::table_layout::parse_column_widths_comment(comment_line).is_some()
    }

    /// Re-parse and re-render after an edit. Called automatically by `edit_ops`.
    pub(crate) fn refresh_parsed(&mut self) {
        let content = self.buffer.contents();
        // Build a row-override closure that consults the image cache.
        // Captures by reference so the cache isn't cloned.  See
        // `ImageCache::reserved_rows` for the per-status decision —
        // `Ready` → aspect rows, `Failed` → 1 (collapsed placeholder),
        // `Pending` / unknown → `None` so the renderer falls back to
        // `image_max_height` for stable layout while the decode is in
        // flight.  When `images_enabled` is false the override
        // short-circuits to `Some(1)` so declined blocks collapse to
        // the one-line placeholder (same row count as a `Failed` entry).
        let images = &self.images;
        let max_w = self.image_max_width as u16;
        let max_h = self.image_max_height as u16;
        let font_size = self.image_font_size;
        let images_enabled = self.images_enabled;
        let override_fn = |url: &str| {
            if !images_enabled {
                return Some(1);
            }
            images.reserved_rows(url, max_w, max_h, font_size)
        };
        self.parsed = ParsedDoc::build_with_overrides(
            &content,
            self.theme,
            self.preserve_blank_lines,
            self.image_max_height,
            self.live_table_widths.as_ref(),
            Some(&override_fn),
            self.row_striping,
            self.viewport_width,
        );
        self.parsed_version = self.parsed_version.wrapping_add(1);
        self.parsed_dirty = false;
        // Drop cache entries whose URL is no longer referenced by any
        // image block — keeps `images.decoded`/`protocols`/scratches
        // from growing without bound as the user edits diagrams
        // (every content change inside a ```mermaid block mints a new
        // synthetic URL, so the old entry becomes orphaned).
        let live: std::collections::HashSet<String> = self
            .parsed
            .image_blocks
            .iter()
            .map(|i| i.url.clone())
            .collect();
        self.images.gc(&live);
    }

    /// If an in-line edit has left `parsed` stale, re-parse now and
    /// clear the dirty flag.  Returns `true` when a re-parse actually
    /// fired.  Callers must invoke this before any code path that
    /// consults `parsed.source_map` byte ranges (mouse hit-tests,
    /// cursor-move navigation) — otherwise the stale map maps the
    /// live cursor's byte onto the wrong block.
    pub fn flush_parsed_if_dirty(&mut self) -> bool {
        if self.parsed_dirty {
            self.refresh_parsed();
            true
        } else {
            false
        }
    }

    /// Set the cursor offset and clamp it to buffer bounds.
    pub(crate) fn set_cursor(&mut self, offset: usize) {
        self.cursor.offset = offset;
        self.cursor.clamp(&self.buffer);
    }

    /// Apply an edit delta to the buffer, record it in history, mark dirty,
    /// and — for edits that cross a line boundary — refresh the parsed
    /// document.
    ///
    /// In-line edits (neither `removed` nor `inserted` contain `\n`) do NOT
    /// re-parse: the cursor stays in the same block, block line indices
    /// don't shift, and the rendered view extracts the cursor block's raw
    /// text from the live buffer via the cached `cursor_block_line_range`.
    /// The parse is refreshed later — on cursor movement, on mouse events,
    /// or on any action that reads `source_map` byte ranges — via
    /// `flush_parsed_if_dirty`.  This batches a whole typing burst into a
    /// single re-parse at the moment the user moves off the line, and
    /// eliminates the mid-typing rendered → raw → rendered flash.
    pub(crate) fn apply_delta(&mut self, delta: EditDelta) {
        let crosses_line = delta.inserted.contains('\n') || delta.removed.contains('\n');
        let new_cursor = delta.redo_cursor();
        // Apply the edit.
        let end = delta.offset + delta.removed.chars().count();
        if !delta.removed.is_empty() {
            self.buffer
                .remove(delta.offset, end.min(self.buffer.len_chars()));
        }
        if !delta.inserted.is_empty() {
            self.buffer.insert(delta.offset, &delta.inserted);
        }
        self.history.record(delta);
        self.cursor.offset = new_cursor.min(self.buffer.len_chars());
        self.dirty = true;

        if crosses_line {
            // A newline added or removed reflows block boundaries — re-parse
            // immediately so the rendered view, source_map, and
            // cursor_block_line_range all reflect the new block layout.
            self.refresh_parsed();
            self.update_cursor_block();
        } else {
            // In-line edit: defer the re-parse.  Bump `parsed_version` so
            // per-frame snapshot caches (image, link, table) invalidate on
            // the next draw — otherwise they'd paint with stale geometry
            // against a document whose cursor block has grown / shrunk.
            self.parsed_dirty = true;
            self.parsed_version = self.parsed_version.wrapping_add(1);
        }
    }

    // ── Jitter suppression ────────────────────────────────────────

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

    // ── Scroll helpers ────────────────────────────────────────────

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Scroll down by `n` rendered lines. The maximum scroll is set so that
    /// the last document line can reach the very top of the viewport (one full
    /// page past where the last line is at the bottom), giving the user room to
    /// work near the end of the document without content jumping away.
    pub fn scroll_down(&mut self, n: usize, _viewport_height: usize) {
        let total = self.total_line_count_for_mode();
        // Allow scroll until the last line sits at the top of the viewport.
        let max = total.saturating_sub(1);
        self.scroll = (self.scroll + n).min(max);
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    /// Scroll so the last document line sits at the bottom of the viewport.
    ///
    /// `viewport_width` is used to compute visual-wrap-aware scroll positions
    /// in Rendered/Preview mode, where long rendered lines may wrap onto
    /// multiple visual rows.  In Raw mode visibility is measured by logical
    /// buffer lines, so `viewport_width` is ignored.
    pub fn scroll_to_bottom(&mut self, viewport_height: usize, viewport_width: usize) {
        match self.mode {
            crate::editor::Mode::Raw => {
                let total = self.buffer.line_count();
                self.scroll = total.saturating_sub(viewport_height);
            }
            _ => {
                let total = self.parsed.line_count();
                if total == 0 {
                    self.scroll = 0;
                    return;
                }
                self.scroll =
                    self.scroll_for_last_visible(total - 1, viewport_height, viewport_width);
            }
        }
    }

    /// Smallest scroll offset such that rendered line `target_last` fits on the
    /// last visual row of a viewport of `viewport_height` rows, accounting for
    /// word-wrap at `viewport_width`.  Walks backward from `target_last`,
    /// accumulating visual rows, and stops when adding another line would
    /// overflow the viewport.
    fn scroll_for_last_visible(
        &self,
        target_last: usize,
        viewport_height: usize,
        viewport_width: usize,
    ) -> usize {
        if viewport_height == 0 {
            return target_last;
        }
        let lines = &self.parsed.lines;
        if lines.is_empty() {
            return 0;
        }
        let target_last = target_last.min(lines.len() - 1);

        let mut rows_used = 0usize;
        let mut line_idx = target_last;
        loop {
            // O(1) cache lookup — the historical inline `visual_rows_for_line`
            // call here was a per-keystroke cost on long documents.
            let rows = self
                .parsed
                .visual_rows_for_line_at(line_idx, viewport_width);
            if rows_used + rows > viewport_height {
                // Including this line would overflow — start from the next one.
                return line_idx + 1;
            }
            rows_used += rows;
            if line_idx == 0 {
                return 0;
            }
            line_idx -= 1;
        }
    }

    /// If the cursor has scrolled above the top of the viewport (because the
    /// user scrolled down past it), move the cursor to the first visible line.
    /// No-op in Preview mode (no editing cursor there).
    pub fn clamp_cursor_to_viewport_top(&mut self) {
        if self.mode == crate::editor::Mode::Preview {
            return;
        }

        if self.mode == crate::editor::Mode::Raw {
            let (cursor_line, _) = self.cursor.line_col(&self.buffer);
            if cursor_line < self.scroll {
                let target = self.scroll.min(self.buffer.line_count().saturating_sub(1));
                self.cursor.offset = self.buffer.line_to_char(target);
                self.cursor.preferred_col = self.cursor.cell_col(&self.buffer);
            }
            return;
        }

        // Rendered mode: check the cursor's rendered line.
        let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
        let cursor_rendered = self.parsed.source_map.rendered_lines_for_byte(cursor_byte);
        if cursor_rendered.start < self.scroll {
            // Scan forward from self.scroll to find the first block whose rendered
            // start is >= scroll. `original_byte_for_rendered_line` returns the block
            // START byte, but a block may start before scroll; in that case we skip to
            // the next block and try again.
            let total = self.parsed.lines.len();
            let mut scan = self.scroll.min(total.saturating_sub(1));
            loop {
                if let Some(byte) = self.parsed.source_map.original_byte_for_rendered_line(scan) {
                    let block_lines = self.parsed.source_map.rendered_lines_for_byte(byte);
                    if block_lines.start >= self.scroll {
                        let char_offset = self.buffer.rope().byte_to_char(byte);
                        self.cursor.offset = char_offset.min(self.buffer.len_chars());
                        self.update_cursor_block();
                        break;
                    }
                    // Block starts before scroll — skip to end of this block.
                    scan = block_lines.end;
                    if scan >= total {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
    }

    /// Total lines to use for scroll-bound calculations, based on current mode.
    fn total_line_count_for_mode(&self) -> usize {
        match self.mode {
            crate::editor::Mode::Raw => self.buffer.line_count(),
            _ => self.parsed.line_count(),
        }
    }

    /// Ensure the cursor is visible within the viewport.
    ///
    /// In Raw mode, visibility is based on buffer line numbers.
    /// In Rendered/Preview mode, visibility is measured in visual rows —
    /// long rendered lines wrap at `viewport_width` and consume multiple rows,
    /// so scroll bounds must account for that or the last lines of the
    /// document get pushed off-screen.
    pub fn ensure_cursor_visible(&mut self, viewport_height: usize, viewport_width: usize) {
        if viewport_height == 0 {
            return;
        }

        if self.mode == crate::editor::Mode::Raw {
            // Raw mode: cursor visibility is wrap-aware — long buffer lines
            // wrap at `viewport_width` and consume multiple visual rows, so
            // a cursor whose buffer-line is inside `[scroll, scroll+vp_h)`
            // can still be visually past the viewport bottom.  Use
            // `cursor_screen_row` (which sums per-line visual rows from the
            // live buffer) to detect both the above- and below-viewport
            // cases.
            let (cursor_line, _) = self.cursor.line_col(&self.buffer);
            if cursor_line < self.scroll {
                self.scroll = cursor_line;
                return;
            }
            if viewport_width == 0 {
                let visible_end = self.scroll + viewport_height;
                if cursor_line >= visible_end {
                    self.scroll = cursor_line + 1 - viewport_height;
                }
                return;
            }
            let cursor_row = self.cursor_screen_row(viewport_width);
            if cursor_row >= viewport_height {
                self.set_scroll_for_cursor_screen_row(
                    viewport_height.saturating_sub(1),
                    viewport_width,
                );
            }
            return;
        }

        // Rendered / Preview mode: use source-map rendered line coordinates.
        let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
        let cursor_lines = self.parsed.source_map.rendered_lines_for_byte(cursor_byte);

        if cursor_lines.is_empty() {
            return;
        }

        let cursor_first = cursor_lines.start;

        // RenderedView now replaces only one rendered line (not the whole block),
        // so the block height does not change. Use the block's last rendered line
        // as the cursor's effective last line in all non-Raw modes.
        let cursor_last = cursor_lines.end.saturating_sub(1);

        // Scroll up if cursor is above the visible area.
        if cursor_first < self.scroll {
            self.scroll = cursor_first;
        }

        // Scroll down if the cursor line doesn't fit in the viewport, measured
        // in visual rows so that wrapped lines between `scroll` and `cursor_last`
        // are counted correctly.
        let total_rows = self.visual_rows_between(self.scroll, cursor_last, viewport_width);
        if total_rows > viewport_height {
            self.scroll =
                self.scroll_for_last_visible(cursor_last, viewport_height, viewport_width);
        }
    }

    /// Sum of visual rows for rendered lines `first..=last`, wrapped at
    /// `width`.  O(1) after `ParsedDoc`'s per-frame visual-row cache is
    /// populated; the historical loop here ran the wrap algorithm
    /// inline on every call.
    fn visual_rows_between(&self, first: usize, last: usize, width: usize) -> usize {
        self.parsed.visual_rows_between(first, last, width)
    }

    /// Estimated screen row (0-indexed within the viewport) at which the
    /// cursor currently appears.  Sums the visual rows consumed by every
    /// line between `scroll` and the cursor's line, plus the cursor's
    /// sub-line within its own (potentially-wrapped) line.  In Rendered /
    /// Preview mode the unit is rendered lines (mirroring `RenderedView`);
    /// in Raw mode the unit is buffer lines (mirroring `RawView`).
    /// Returns 0 when the cursor is currently above the viewport.
    ///
    /// Used by `Action::ToggleRawMode` to keep the cursor on the same
    /// screen row when switching between Rendered and Raw, since the two
    /// modes use different scroll units and the same `scroll` value
    /// otherwise jumps to a totally different part of the document.
    pub fn cursor_screen_row(&self, viewport_width: usize) -> usize {
        if viewport_width == 0 {
            return 0;
        }
        match self.mode {
            crate::editor::Mode::Raw => raw_cursor_screen_row(self, viewport_width),
            _ => rendered_cursor_screen_row(self, viewport_width),
        }
    }

    /// Set `scroll` so the cursor's line appears at `target_row` on
    /// screen.  Quantized by line boundaries — the cursor will land at
    /// `target_row` exactly when possible, otherwise on the nearest
    /// line-start row at or above `target_row` (so the cursor stays
    /// visible rather than disappearing past the bottom).
    pub fn set_scroll_for_cursor_screen_row(&mut self, target_row: usize, viewport_width: usize) {
        if viewport_width == 0 {
            return;
        }
        match self.mode {
            crate::editor::Mode::Raw => {
                set_raw_scroll_for_screen_row(self, target_row, viewport_width)
            }
            _ => set_rendered_scroll_for_screen_row(self, target_row, viewport_width),
        }
    }

    // ── Visual-line navigation ────────────────────────────────────

    /// Move the cursor up by one **visual** line, accounting for word-wrap at
    /// `col_width`.
    ///
    /// Uses the same word-aware wrap algorithm as `line_render::render_line`
    /// so navigation lands on the same visual column the user sees on screen.
    /// If the cursor is on the first visual sub-line of its logical line, it
    /// moves to the LAST visual sub-line of the previous logical line,
    /// preserving `cursor.preferred_col` as the target visual column.
    pub fn move_up_visual(&mut self, col_width: usize) {
        if col_width == 0 {
            self.cursor.move_up(&self.buffer);
            return;
        }
        let (line, col) = self.cursor.line_col(&self.buffer);
        let target_cell = self.cursor.preferred_col;

        let text = line_text_trimmed(&self.buffer, line);
        let indent = crate::ui::line_render::compute_hanging_indent_str(&text);
        let rows = wrap_rows_for_text(&text, col_width, indent);
        let (sub_idx, _) = crate::ui::line_render::sub_line_of_col(&rows, col);

        if sub_idx > 0 {
            let target_idx = sub_idx - 1;
            let target = rows[target_idx];
            let is_last = target_idx + 1 == rows.len();
            let row_indent = if target_idx == 0 { 0 } else { indent };
            let raw_col = raw_col_for_visual_cells(&text, target, target_cell, is_last, row_indent);
            let line_start = self.buffer.line_to_char(line);
            self.cursor.offset = line_start + raw_col;
        } else if line > 0 {
            let prev_line = line - 1;
            let prev_text = line_text_trimmed(&self.buffer, prev_line);
            let prev_indent = crate::ui::line_render::compute_hanging_indent_str(&prev_text);
            let prev_rows = wrap_rows_for_text(&prev_text, col_width, prev_indent);
            let target_idx = prev_rows.len() - 1;
            let target = *prev_rows.last().expect("rows always non-empty");
            let row_indent = if target_idx == 0 { 0 } else { prev_indent };
            let raw_col =
                raw_col_for_visual_cells(&prev_text, target, target_cell, true, row_indent);
            let prev_start = self.buffer.line_to_char(prev_line);
            self.cursor.offset = prev_start + raw_col;
        } else {
            self.cursor.offset = self.buffer.line_to_char(0);
        }
    }

    /// Move the cursor down by one **visual** line, accounting for word-wrap at
    /// `col_width`. See `move_up_visual` for details.
    pub fn move_down_visual(&mut self, col_width: usize) {
        if col_width == 0 {
            self.cursor.move_down(&self.buffer);
            return;
        }
        let (line, col) = self.cursor.line_col(&self.buffer);
        let target_cell = self.cursor.preferred_col;

        let text = line_text_trimmed(&self.buffer, line);
        let indent = crate::ui::line_render::compute_hanging_indent_str(&text);
        let rows = wrap_rows_for_text(&text, col_width, indent);
        let (sub_idx, _) = crate::ui::line_render::sub_line_of_col(&rows, col);

        if sub_idx + 1 < rows.len() {
            let target_idx = sub_idx + 1;
            let target = rows[target_idx];
            let is_last = target_idx + 1 == rows.len();
            let row_indent = if target_idx == 0 { 0 } else { indent };
            let raw_col = raw_col_for_visual_cells(&text, target, target_cell, is_last, row_indent);
            let line_start = self.buffer.line_to_char(line);
            self.cursor.offset = line_start + raw_col;
        } else {
            let last_line = self.buffer.line_count().saturating_sub(1);
            if line < last_line {
                let next_line = line + 1;
                let next_text = line_text_trimmed(&self.buffer, next_line);
                let next_indent = crate::ui::line_render::compute_hanging_indent_str(&next_text);
                let next_rows = wrap_rows_for_text(&next_text, col_width, next_indent);
                let target = next_rows[0];
                let is_last = next_rows.len() == 1;
                // First row of the next logical line uses no hanging indent;
                // continuation rows of the same line do.
                let raw_col = raw_col_for_visual_cells(&next_text, target, target_cell, is_last, 0);
                let next_start = self.buffer.line_to_char(next_line);
                self.cursor.offset = next_start + raw_col;
            } else {
                self.cursor.move_line_end(&self.buffer);
            }
        }
    }

    /// Cell column of the cursor measured from the **screen-row** left
    /// edge, including any hanging-indent padding when the cursor sits on
    /// a wrapped continuation row.  Used to seed `preferred_col` so
    /// vertical navigation lands at the same screen X on the target row.
    pub fn current_visual_col(&self, col_width: usize) -> usize {
        if col_width == 0 {
            return self.cursor.cell_col(&self.buffer);
        }
        let (line, col) = self.cursor.line_col(&self.buffer);
        let text = line_text_trimmed(&self.buffer, line);
        let indent = crate::ui::line_render::compute_hanging_indent_str(&text);
        let rows = wrap_rows_for_text(&text, col_width, indent);
        let (sub_idx, _) = crate::ui::line_render::sub_line_of_col(&rows, col);
        let row = rows[sub_idx];
        let row_indent = if sub_idx == 0 { 0 } else { indent };
        cell_col_within_row(&text, row, col, row_indent)
    }
}

/// Fetch the text of buffer line `line`, stripped of any trailing newline.
fn line_text_trimmed(buf: &crate::document::Buffer, line: usize) -> String {
    buf.line(line)
        .map(|s| s.trim_end_matches('\n').to_owned())
        .unwrap_or_default()
}

/// Wrap `text` at `col_width` cells with a hanging `indent` on continuation
/// rows.  Returns `(start, end, next_start)` tuples (char indices) — the
/// same shape `visual_rows_of_chars` returns, just bridged from `&str`
/// since `EditorState` works in raw text.
fn wrap_rows_for_text(text: &str, col_width: usize, indent: usize) -> Vec<(usize, usize, usize)> {
    let chars: Vec<(char, ratatui::style::Style)> = text
        .chars()
        .map(|c| (c, ratatui::style::Style::default()))
        .collect();
    crate::ui::line_render::visual_rows_of_chars(&chars, col_width, indent)
}

/// Cell-aware inverse of the wrap layout: given the text of a logical line,
/// one of its visual rows `(start, end, next_start)` (char indices), the
/// desired screen cell column `target_cell`, whether this row is the last
/// in its line, and the row's hanging-indent width in cells, return the
/// absolute char column on the logical line where the cursor should land.
///
/// Wide chars (CJK, emoji) are handled via the snap-past rule: a target
/// cell that lands inside a wide glyph places the cursor *after* the glyph
/// rather than splitting it.  For non-last rows, the cursor is kept off the
/// wrap boundary at `next_start` so it stays visually on this row.  When
/// `indent > 0` (a continuation row of a wrapped list item), the indent
/// area is a forbidden zone — clicks inside it snap forward to the row's
/// first content char.
fn raw_col_for_visual_cells(
    text: &str,
    row: (usize, usize, usize),
    target_cell: usize,
    is_last_row: bool,
    indent: usize,
) -> usize {
    let (start, end, next_start) = row;
    let max_char_in_row = if is_last_row {
        end
    } else {
        next_start.saturating_sub(1).max(start)
    };
    let row_chars = text.chars().skip(start).take(end - start);
    let in_row_idx = crate::ui::line_render::char_idx_at_cell_col(row_chars, target_cell, indent);
    let absolute = start + in_row_idx;
    absolute.min(max_char_in_row)
}

/// Screen cell column of char position `char_col` within its visual row
/// `row`, for a logical line whose raw text is `text`.  `indent` is the
/// hanging-indent in cells for this row (0 for first rows; the line's
/// detected indent for continuation rows).  Used by `current_visual_col`
/// to seed `preferred_col` after a horizontal move so subsequent vertical
/// nav preserves the cursor's screen X.
fn cell_col_within_row(
    text: &str,
    row: (usize, usize, usize),
    char_col: usize,
    indent: usize,
) -> usize {
    let (start, _, _) = row;
    let take = char_col.saturating_sub(start);
    let row_chars = text.chars().skip(start).take(take);
    crate::ui::line_render::cell_col_at_char_idx(row_chars, take, indent)
}

// ── Cursor screen-row helpers ──────────────────────────────────────────────

fn raw_cursor_screen_row(state: &EditorState, width: usize) -> usize {
    let (cursor_line, cursor_col) = state.cursor.line_col(&state.buffer);
    if cursor_line < state.scroll {
        return 0;
    }
    let mut rows = 0usize;
    for i in state.scroll..cursor_line {
        let text = line_text_trimmed(&state.buffer, i);
        rows += crate::ui::line_render::visual_rows_of_str(&text, width)
            .len()
            .max(1);
    }
    let cursor_text = line_text_trimmed(&state.buffer, cursor_line);
    let cursor_rows = crate::ui::line_render::visual_rows_of_str(&cursor_text, width);
    let (sub, _) = crate::ui::line_render::sub_line_of_col(&cursor_rows, cursor_col);
    rows + sub
}

fn set_raw_scroll_for_screen_row(state: &mut EditorState, target_row: usize, width: usize) {
    let (cursor_line, cursor_col) = state.cursor.line_col(&state.buffer);
    let cursor_text = line_text_trimmed(&state.buffer, cursor_line);
    let cursor_rows = crate::ui::line_render::visual_rows_of_str(&cursor_text, width);
    let (cursor_sub, _) = crate::ui::line_render::sub_line_of_col(&cursor_rows, cursor_col);
    // We want screen_row(scroll) = target_row.  In Raw mode that decomposes to
    // (visual rows in lines [scroll..cursor_line)) + cursor_sub, so the line-
    // count walk needs to consume `target_row - cursor_sub` rows.
    let need = target_row.saturating_sub(cursor_sub);
    let mut acc = 0usize;
    let mut scroll = cursor_line;
    while scroll > 0 && acc < need {
        let prev = scroll - 1;
        let text = line_text_trimmed(&state.buffer, prev);
        let rows = crate::ui::line_render::visual_rows_of_str(&text, width)
            .len()
            .max(1);
        if acc + rows > need {
            break;
        }
        acc += rows;
        scroll -= 1;
    }
    state.scroll = scroll;
}

fn rendered_cursor_screen_row(state: &EditorState, width: usize) -> usize {
    let cursor_rendered = cursor_rendered_line_idx(state);
    if cursor_rendered < state.scroll {
        return 0;
    }
    let rows_before = state
        .parsed
        .visual_rows_before(cursor_rendered, width)
        .saturating_sub(state.parsed.visual_rows_before(state.scroll, width));
    rows_before + cursor_sub_line_in_rendered(state, cursor_rendered, width)
}

fn set_rendered_scroll_for_screen_row(state: &mut EditorState, target_row: usize, width: usize) {
    let cursor_rendered = cursor_rendered_line_idx(state);
    // Subtract the cursor's sub-line within its rendered line: walking
    // backward by full rendered-line counts only consumes
    // `target_row - cursor_sub` rows before the cursor's line, since the
    // cursor lives `cursor_sub` rows into its own line.
    let cursor_sub = cursor_sub_line_in_rendered(state, cursor_rendered, width);
    let need = target_row.saturating_sub(cursor_sub);
    let mut acc = 0usize;
    let mut scroll = cursor_rendered;
    while scroll > 0 && acc < need {
        let prev = scroll - 1;
        let rows = state.parsed.visual_rows_for_line_at(prev, width);
        if acc + rows > need {
            break;
        }
        acc += rows;
        scroll -= 1;
    }
    state.scroll = scroll;
}

/// Visual sub-line offset of the cursor within its rendered line.  In
/// Rendered/Preview mode the cursor's line is painted from the live
/// buffer (the "cursor block reveal" path), so the wrap that determines
/// the cursor's sub-line is over the buffer text — not the parsed
/// rendered text, which can drop or expand chars relative to source.
/// Falls back to the parsed line's wrap when the cursor's buffer-line
/// text isn't available.
fn cursor_sub_line_in_rendered(
    state: &EditorState,
    _cursor_rendered: usize,
    width: usize,
) -> usize {
    let (cursor_buf_line, cursor_col) = state.cursor.line_col(&state.buffer);
    let line_text = line_text_trimmed(&state.buffer, cursor_buf_line);
    let rows = crate::ui::line_render::visual_rows_of_str(&line_text, width);
    let (sub, _) = crate::ui::line_render::sub_line_of_col(&rows, cursor_col);
    sub
}

/// Rendered-line index where the cursor currently appears in `RenderedView`.
/// Mirrors the `cursor_rendered_line` computation in `ui::rendered_view` so
/// scroll arithmetic that needs to match the on-screen cursor position
/// (e.g. preserving the cursor's screen row across a mode switch) lands on
/// the same line the view actually paints.
fn cursor_rendered_line_idx(state: &EditorState) -> usize {
    let cursor_offset = state.cursor.offset;
    let cursor_byte = state.buffer.rope().char_to_byte(cursor_offset);
    let cursor_block_idx = state
        .parsed
        .source_map
        .block_for_byte(cursor_byte)
        .unwrap_or(0);
    let cursor_block_lines = state
        .parsed
        .source_map
        .rendered_lines_for_block(cursor_block_idx);
    if cursor_block_lines.is_empty() {
        return state.scroll;
    }
    let cursor_block_own = state.parsed.block_own_line_count(cursor_block_idx);
    let block_range = state.parsed.source_map.original_range_for_byte(cursor_byte);

    let raw_block_source: String = block_range
        .as_ref()
        .map(|r| {
            let source = state.buffer.contents();
            let end = r.end.min(source.len());
            source[r.start..end].to_owned()
        })
        .unwrap_or_default();

    let block_start_byte = block_range.as_ref().map(|r| r.start).unwrap_or(0);
    let cursor_offset_in_block = cursor_byte.saturating_sub(block_start_byte);

    let raw_lines: Vec<&str> = raw_block_source.split('\n').collect();
    let mut byte_pos = 0usize;
    let mut cursor_raw_line = 0usize;
    for (i, line) in raw_lines.iter().enumerate() {
        let line_end = byte_pos + line.len();
        if cursor_offset_in_block <= line_end {
            cursor_raw_line = i;
            break;
        }
        byte_pos = line_end + 1;
    }

    let is_table = crate::editor::table_edit::is_table_block(&raw_block_source);
    let cursor_in_block = if is_table && cursor_block_own >= 3 {
        let block_lines = state
            .parsed
            .lines
            .get(cursor_block_lines.clone())
            .unwrap_or(&[]);
        let kinds = crate::ui::table_view::classify_table_sub_lines(block_lines);
        let last_replaceable = cursor_block_own.saturating_sub(2);
        let sub = match cursor_raw_line {
            0 => kinds
                .iter()
                .position(|k| {
                    matches!(
                        k,
                        crate::ui::table_view::TableSubLineKind::Header { sub: 0 }
                    )
                })
                .unwrap_or(1),
            1 => kinds
                .iter()
                .position(|k| matches!(k, crate::ui::table_view::TableSubLineKind::ThickSeparator))
                .unwrap_or(2),
            r => {
                let target = r - 2;
                kinds
                    .iter()
                    .position(|k| {
                        matches!(
                            k,
                            crate::ui::table_view::TableSubLineKind::DataRow { row, sub: 0 }
                                if *row == target
                        )
                    })
                    .unwrap_or(2 * r - 1)
            }
        };
        sub.min(last_replaceable)
    } else {
        let preceding_non_blank = raw_lines
            .iter()
            .take(cursor_raw_line)
            .filter(|l| !l.trim().is_empty())
            .count();
        preceding_non_blank.min(cursor_block_own.saturating_sub(1))
    };

    cursor_block_lines.start + cursor_in_block
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Buffer;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    /// When `images_enabled == false`, every image block collapses to
    /// its one-line `[Image: alt]` placeholder: no blank rows are
    /// reserved underneath.  This matches the `Failed` branch of
    /// `ImageCache::reserved_rows` and is the declined-session layout.
    #[test]
    fn images_enabled_false_collapses_blocks_to_placeholder() {
        // image_max_height = 10 → expanded blocks each reserve 10 rows.
        let src = "![cat](cat.png)\n\n![dog](dog.png)\n";
        let mut state = EditorState::new_with_config(
            Buffer::from_str(src),
            theme(),
            true,
            true,
            10, // image_max_height
        );

        // Default path reserves 10 rows per image (plus the blank
        // between them).  Sanity-check the expanded count.
        let expanded = state.parsed.line_count();
        assert!(
            expanded >= 20,
            "expected ≥ 20 rendered lines with images expanded, got {expanded}",
        );

        state.images_enabled = false;
        state.refresh_parsed();

        // Collapsed: each image block emits exactly its one-line
        // placeholder — two images + the blank gap = 3 rendered lines.
        assert_eq!(state.parsed.line_count(), 3);
    }

    /// Moving down through a word-wrapped line should land on the visually
    /// corresponding column, not on `col / col_width`.
    #[test]
    fn move_down_visual_honours_word_wrap_boundaries() {
        // Line has a natural wrap point at the last space before col 20.
        // "hello world foo bar baz quux wibble wobble"
        // Wrapping at width 20: row 0 ends at last space ≤ 20.
        let text = "hello world foo bar baz quux wibble wobble";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        // Cursor on row 0 at visual col 3 ("l" in "hello").
        state.cursor.offset = 3;
        state.cursor.preferred_col = 3;

        state.move_down_visual(20);

        // After moving down one visual row, cursor should be on row 1 at
        // visual col 3 — i.e. raw col = row1_start + 3.
        let rows = crate::ui::line_render::visual_rows_of_str(text, 20);
        assert!(rows.len() >= 2, "expected wrap into at least 2 rows");
        let (row1_start, _, _) = rows[1];
        assert_eq!(state.cursor.offset, row1_start + 3);
    }

    /// Moving up from the first visual sub-line of a line should land on the
    /// LAST visual sub-line of the previous line, preserving the visual col.
    #[test]
    fn move_up_visual_crosses_to_last_subline_of_previous_line() {
        let long = "aaaaa bbbbb ccccc ddddd eeeee fffff ggggg hhhhh";
        // Two logical lines separated by \n.
        let text = format!("{}\nshort\n", long);
        let mut state = EditorState::new(Buffer::from_str(&text), theme());
        // Cursor on line 1 (the "short" line) at col 3.
        let line1_start = state.buffer.line_to_char(1);
        state.cursor.offset = line1_start + 3;
        state.cursor.preferred_col = 3;

        state.move_up_visual(20);

        // Expected: land on the LAST visual sub-line of line 0 at visual col 3.
        let rows = crate::ui::line_render::visual_rows_of_str(long, 20);
        let last = *rows.last().unwrap();
        let expected_raw_col = last.0 + 3;
        assert_eq!(
            state.cursor.offset,
            state.buffer.line_to_char(0) + expected_raw_col
        );
    }

    /// A list item's content must navigate the same way — the 2-char `- `
    /// prefix should be part of the line's raw col and must NOT shift the
    /// cursor's visual column by 2.
    /// When the cursor sits on a wide wrapped sub-row at a visual column that
    /// exceeds the width of the sub-row above, pressing Up must land the cursor
    /// visually on the previous sub-row (clamped to its last position), not on
    /// the wrap boundary — which visually renders at column 0 of the current
    /// sub-row and leaves Up "stuck" there.
    #[test]
    fn move_up_visual_clamps_within_target_subrow_not_on_wrap_boundary() {
        // Logical line: prefix that wraps at a space, followed by a long run
        // of 'a's. Width 40 gives row 0 = "Super long line of inline code ` "
        // (33 chars) and row 1+ = 40-char runs of 'a's.
        let text = format!("Super long line of inline code ` {}`", "a".repeat(150));
        let mut state = EditorState::new(Buffer::from_str(&text), theme());
        let width = 40;

        let rows = crate::ui::line_render::visual_rows_of_str(&text, width);
        assert!(rows.len() >= 2);
        let (row0_s, row0_e, _) = rows[0];
        let row0_width = row0_e - row0_s;
        let (row1_start, _, _) = rows[1];

        // Cursor on row 1 at a visual column exceeding row 0's width.
        let visual_col_on_row1 = row0_width + 3;
        state.cursor.offset = row1_start + visual_col_on_row1;
        state.cursor.preferred_col = visual_col_on_row1;

        state.move_up_visual(width);

        // Cursor must now be visually on row 0, not on the row 0/row 1 boundary
        // (which renders at column 0 of row 1).
        let (sub_idx, _) = crate::ui::line_render::sub_line_of_col(&rows, state.cursor.offset);
        assert_eq!(
            sub_idx, 0,
            "cursor at offset {} should be visually on row 0, not row {}",
            state.cursor.offset, sub_idx
        );

        // Pressing Up again from the last position of row 0 should keep moving
        // (snap to start of line), not stall at the same offset.
        let before = state.cursor.offset;
        state.move_up_visual(width);
        assert_ne!(
            state.cursor.offset, before,
            "Up from row 0 must not stall at offset {before}",
        );
    }

    /// Regression: pressing Down from a wrapped continuation row of a list
    /// item used to land at content cell `preferred_col` on the next line,
    /// off by the hanging-indent width because `preferred_col` was stored
    /// without the indent.  After the fix, a cursor visually at screen
    /// cell 5 on a list-item continuation row (= 2 cells of indent + 3
    /// cells into content) lands at screen cell 5 on the next plain
    /// paragraph too — no horizontal jump by the indent amount.
    #[test]
    fn move_down_visual_preserves_screen_cell_across_indent_boundary() {
        // Logical line 0: list item that wraps.  Logical line 1: plain
        // paragraph (no marker, no indent).
        let text = "- list item content that wraps to a second row\nplain paragraph here";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        let width = 30;
        // Place the cursor on the *continuation* row of line 0, then
        // sync `preferred_col` from that screen position (5 cells from
        // the screen-row left edge).  The exact char offset of "screen
        // cell 5 on row 1" depends on the wrap point, so derive it via
        // the same helpers the editor uses.
        let chars: Vec<(char, ratatui::style::Style)> = text
            .lines()
            .next()
            .unwrap()
            .chars()
            .map(|c| (c, ratatui::style::Style::default()))
            .collect();
        let rows = crate::ui::line_render::visual_rows_of_chars(&chars, width, 2);
        assert!(rows.len() >= 2, "list item must wrap");
        let (row1_start, row1_end, _) = rows[1];
        // Pick screen cell 5 on row 1 → content cell 3 in row 1 → row1_start + 3.
        state.cursor.offset = row1_start + 3.min(row1_end - row1_start);
        state.cursor.preferred_col = state.current_visual_col(width);
        // The seeded preferred_col must reflect the screen position, not
        // the line-relative cell column.  On row 1 with indent 2, screen
        // col 5 = content cell 3 + indent 2.
        assert_eq!(state.cursor.preferred_col, 5);

        state.move_down_visual(width);

        // We're now on logical line 1's first row (plain paragraph, no
        // indent).  Screen cell 5 there = content cell 5 = char 5.
        let line1_start = state.buffer.line_to_char(1);
        assert_eq!(state.cursor.offset, line1_start + 5);
    }

    /// Regression: clicking on a wrapped continuation row used to seed
    /// `preferred_col` from the line-relative cell column, which on a
    /// long wrapped line is huge.  Subsequent vertical nav then clamped
    /// every target line to its end.  The fix routes click landing
    /// through `current_visual_col` so `preferred_col` reflects the
    /// click's screen column.
    #[test]
    fn click_on_wrapped_continuation_row_seeds_preferred_col_from_screen() {
        // 60-cell paragraph that wraps at width 20 to three rows.  No
        // hanging indent (plain paragraph).
        let text = "the quick brown fox jumps over the lazy dog one more time";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = crate::editor::Mode::Rendered;
        let viewport_w: usize = 20;

        // Compute char offset for "screen cell 4 on row 2".
        let rows = crate::ui::line_render::visual_rows_of_str(text, viewport_w);
        assert!(rows.len() >= 3);
        let (row2_start, row2_end, _) = rows[2];
        let target_offset = row2_start + 4.min(row2_end - row2_start);
        // Simulate a click landing at that offset.
        let click_action = crate::input::MouseAction::Click {
            col: 4,
            row: 2,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let mut anchor: Option<crate::editor::mouse_ops::DragTarget> = None;
        crate::editor::mouse_ops::apply(&mut state, click_action, &mut anchor, &[], 24, viewport_w);
        assert_eq!(state.cursor.offset, target_offset);
        // The bug: preferred_col would have been ~row2_start + 4 (large).
        // After the fix it's the *screen* cell column (4).
        assert_eq!(state.cursor.preferred_col, 4);
    }

    #[test]
    fn move_down_visual_on_list_item_without_offset_bug() {
        // A single-line list item whose content wraps at width 20.  The
        // raw text has a 2-cell hanging indent (the `- ` marker), so the
        // continuation row's first content char sits at screen cell 2.
        // `preferred_col = 5` is a *screen* cell col — on the continuation
        // row it should map to content cell `5 - 2 = 3`.
        let text = "- hello world foo bar baz quux wibble";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        // Cursor on row 0 at screen cell 5 (the second 'l' in "hello",
        // since `- ` consumes cells 0–1 and "hell" runs across cells 2–5).
        state.cursor.offset = 5;
        state.cursor.preferred_col = 5;

        state.move_down_visual(20);

        let rows = crate::ui::line_render::visual_rows_of_chars(
            &text
                .chars()
                .map(|c| (c, ratatui::style::Style::default()))
                .collect::<Vec<_>>(),
            20,
            2,
        );
        assert!(rows.len() >= 2);
        let (row1_start, _, _) = rows[1];
        // Screen cell 5 with indent 2 → content offset 3 within row 1.
        assert_eq!(state.cursor.offset, row1_start + 3);
    }

    /// When the cursor moves to the last rendered line of a document that
    /// contains wrapped lines above it, the scroll offset must back up enough
    /// visual rows (not logical lines) to keep the last line on screen.
    /// Regression test for "bottom of document is never visible in Rendered
    /// mode when earlier lines wrap".
    #[test]
    fn scroll_to_bottom_accounts_for_wrapped_lines() {
        // Build a document whose first paragraph wraps across several visual
        // rows, followed by a short final paragraph.  At viewport_height=5 and
        // viewport_width=20, the long paragraph occupies >5 visual rows, so a
        // naive "scroll = total - height" bound (which ignores wrap) would
        // push the final paragraph past the viewport bottom.
        let long = "a".repeat(100); // one rendered line, ~5 visual rows @ width 20
        let src = format!("{long}\n\nfinal line.\n");
        let mut state = EditorState::new(Buffer::from_str(&src), theme());
        state.mode = crate::editor::Mode::Rendered;
        state.cursor.move_doc_end(&state.buffer);
        state.update_cursor_block();

        let vp_h = 5;
        let vp_w = 20;
        state.scroll_to_bottom(vp_h, vp_w);
        state.ensure_cursor_visible(vp_h, vp_w);

        // From scroll..=last_rendered_line, the total visual rows must fit.
        let total = state.parsed.lines.len();
        let last = total - 1;
        let used = state.visual_rows_between(state.scroll, last, vp_w);
        assert!(
            used <= vp_h,
            "scroll {} leaves {} visual rows between scroll and last rendered line (viewport is {})",
            state.scroll,
            used,
            vp_h
        );
    }

    /// `cursor_screen_row` paired with `set_scroll_for_cursor_screen_row`
    /// should round-trip — switching modes and asking the editor to put the
    /// cursor at the captured row must end with the cursor at the same
    /// visual row.  Regression test for the rendered → raw mode-switch
    /// jumping the visible region.
    #[test]
    fn cursor_screen_row_round_trips_across_mode_switch() {
        // Document with enough text that scrolling kicks in.  Plain
        // paragraphs map 1:1 between rendered and raw, so we can compare
        // rows precisely.
        let mut src = String::new();
        for i in 0..20 {
            src.push_str(&format!("line {i}\n"));
        }
        let vp_w = 40;
        let mut state = EditorState::new(Buffer::from_str(&src), theme());
        state.mode = crate::editor::Mode::Rendered;
        // Place cursor on line 12 and scroll so it sits a few rows down
        // from the top of the viewport.
        state.cursor.offset = state.buffer.line_to_char(12);
        state.update_cursor_block();
        state.scroll = 9; // cursor at screen row 3 in Rendered mode.

        let row_before = state.cursor_screen_row(vp_w);
        assert_eq!(row_before, 3);

        // Simulate switching to Raw mode: in Raw the same `scroll` value is
        // a buffer-line index, but those happen to coincide for plain
        // paragraphs.  Force an artificial mismatch by shifting scroll
        // before re-anchoring, so we exercise the helper rather than a
        // happy 1:1 alignment.
        state.mode = crate::editor::Mode::Raw;
        state.scroll = 0; // pretend the mode switch left the cursor far above

        state.set_scroll_for_cursor_screen_row(row_before, vp_w);
        let row_after = state.cursor_screen_row(vp_w);
        assert_eq!(row_after, row_before);
    }

    /// `set_theme` must swap the cached reference and bump
    /// `parsed_version` so dependent caches invalidate, and re-running
    /// it with the same pointer must be a no-op (parsed_version
    /// stable).
    #[test]
    fn set_theme_swaps_reference_and_refreshes_parsed() {
        let original: &'static Theme = theme();
        let mut state = EditorState::new(Buffer::from_str("# heading\n"), original);
        let v_before = state.parsed_version;

        // Same reference → no rebuild, version unchanged.
        state.set_theme(original);
        assert_eq!(state.parsed_version, v_before);

        // Different reference → rebuild + version bump.
        let other: &'static Theme = Box::leak(Box::new(Theme::default()));
        state.set_theme(other);
        assert!(std::ptr::eq(state.theme, other));
        assert_ne!(state.parsed_version, v_before);
    }

    /// `set_scroll_for_cursor_screen_row` must clamp to scroll = 0 when the
    /// requested row is larger than the cursor's distance from the document
    /// start — the cursor will land on a smaller screen row but stay
    /// visible (no negative scroll, no off-screen cursor).
    #[test]
    fn set_scroll_for_cursor_screen_row_clamps_at_top() {
        let src = "line 0\nline 1\nline 2\n";
        let mut state = EditorState::new(Buffer::from_str(src), theme());
        state.mode = crate::editor::Mode::Raw;
        // Cursor on line 1, asking for screen row 50 (way past the top).
        state.cursor.offset = state.buffer.line_to_char(1);
        state.scroll = 0;
        state.set_scroll_for_cursor_screen_row(50, 40);
        assert_eq!(state.scroll, 0);
        // Cursor stays visible at row 1 (its line is 1 line below the top).
        assert_eq!(state.cursor_screen_row(40), 1);
    }
}
