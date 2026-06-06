use std::cell::RefCell;
use std::time::{Duration, Instant};

use crate::config::Theme;
use crate::diff::DiffState;
use crate::document::{Buffer, Cursor, EditDelta, History, ParsedDoc, Selection, VisualSelection};
use crate::editor::state_viewport::RawVisualRowCache;
use crate::editor::Mode;
use crate::image::ImageCache;

// ── Cursor blink ─────────────────────────────────────────────────────

const BLINK_INTERVAL: Duration = Duration::from_millis(530);

/// Tracks the on/off phase of a blinking cursor.
///
/// When `blinking` is true the cursor alternates between visible and hidden
/// on a fixed cadence.  Any cursor movement resets the phase to visible so
/// the cursor is always immediately apparent after a keypress.
#[derive(Debug, Clone)]
pub struct CursorBlink {
    blinking: bool,
    visible: bool,
    last_toggle: Instant,
}

impl Default for CursorBlink {
    fn default() -> Self {
        Self {
            blinking: true,
            visible: true,
            last_toggle: Instant::now(),
        }
    }
}

impl CursorBlink {
    /// Whether the cursor should be painted this frame.
    pub fn is_visible(&self) -> bool {
        !self.blinking || self.visible
    }

    /// Reset the blink cycle: cursor becomes visible and the timer restarts.
    /// Call this whenever the cursor moves or an edit occurs.
    pub fn reset(&mut self) {
        self.visible = true;
        self.last_toggle = Instant::now();
    }

    /// Advance the blink state.  Returns `true` if visibility changed
    /// (i.e. a redraw is needed).
    pub fn tick(&mut self) -> bool {
        if !self.blinking {
            return false;
        }
        if self.last_toggle.elapsed() >= BLINK_INTERVAL {
            self.visible = !self.visible;
            self.last_toggle = Instant::now();
            true
        } else {
            false
        }
    }

    /// The `Instant` at which the next toggle will fire, or `None` when
    /// blinking is disabled.
    pub fn next_toggle(&self) -> Option<Instant> {
        if self.blinking {
            Some(self.last_toggle + BLINK_INTERVAL)
        } else {
            None
        }
    }
}

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
    ///
    /// Autosave keys off the pair `(dirty, Buffer::version())` —
    /// `App::tick_autosave` arms its debounce timer whenever the
    /// version bumps *and* `dirty` is true, and clears the pending
    /// timer when `dirty` flips back to false.  Any future code path
    /// that clears `dirty` without going through a save (e.g. a
    /// "discard changes" / revert action) is fine, but a path that
    /// sets `dirty = true` without bumping `Buffer::version()` would
    /// silently never trigger autosave — keep the two in lockstep.
    pub dirty: bool,
    /// Internal clipboard (kill-ring). Used as fallback when arboard is
    /// unavailable.
    pub kill_ring: String,
    /// Scroll offset in visual rows for the active mode.
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
    /// Counterpart to [`Self::images_enabled`] for diagram blocks
    /// (mermaid, etc.).  Image blocks whose `source` field is `Some(_)`
    /// honour this flag instead of `images_enabled` — so a user can opt
    /// in to images but not diagrams (or vice-versa).  Default `true`.
    pub diagrams_enabled: bool,
    /// Monotonically-increasing version counter, bumped every time
    /// `refresh_parsed` rebuilds the `ParsedDoc`.  Consumed by the view
    /// state to invalidate per-frame snapshot caches only when the parse
    /// tree actually changed — a scroll-only change leaves the version
    /// alone, so `build_snapshots` can reuse the previous frame's
    /// geometry.
    pub parsed_version: u64,
    /// Live-preview scratch for the column-resize drag.  When
    /// `Some((table_byte_start, widths))`, the table whose first row begins
    /// at `table_byte_start` renders with `widths` applied as a
    /// `user_widths` override — without touching the buffer.  Cleared on
    /// release (when the drag commits via `write_column_widths`) or on any
    /// non-resize action that invalidates the drag.
    pub live_table_widths: Option<(usize, Vec<Option<usize>>)>,
    /// Propagated from `config.table.row_striping`.  Controls
    /// whether the renderer fills alternating data rows with
    /// `Theme::table_row_even` / `Theme::table_row_odd`.  Set by the App
    /// at construction time and re-read on every `refresh_parsed`.
    pub row_striping: bool,
    /// Propagated from `config.editor.big_h1`.  Controls whether H1
    /// headings render as 4-row "big text" via `tui-big-text` (Quadrant
    /// pixel size).  Set by the App at construction time and re-read on
    /// every `refresh_parsed`.
    pub big_h1: bool,
    /// Most-recently observed terminal column width, fed
    /// into `Renderer::with_viewport_width` on every `refresh_parsed`
    /// so the min-max proportional column-width algorithm adapts to
    /// the user's actual viewport.  Set to a sensible 80 default until
    /// the App posts the real width via [`set_viewport_width`].  Stored
    /// in the editor (rather than threaded as a parameter through
    /// `refresh_parsed`) so call-sites that don't know the width — e.g.
    /// undo/redo, paste, file load — pick up the cached value.
    pub viewport_width: usize,
    /// Set on the Release event of a column-border drag, this
    /// flags the App to either commit the in-progress `live_table_widths`
    /// (writing the `tui-columns` comment to the buffer) or open the
    /// width-injection warning modal.  Carries the `table_byte_start` of
    /// the table whose widths are pending.  Cleared by
    /// [`commit_pending_column_widths`] / [`cancel_pending_column_widths`].
    pub pending_column_widths_commit: Option<usize>,
    /// Mouse-ops and edit-ops set this when a click / keypress
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
    /// `source_map`.  `None` for empty documents / uninitialized state.
    pub cursor_block_line_range: Option<std::ops::Range<usize>>,
    /// Blink state for the cursor.  The App ticks this before each
    /// draw and threads `cursor_visible()` into the view layer.
    pub cursor_blink: CursorBlink,
    /// Set by the App before each draw: `true` when any modal overlay
    /// is visible.  While set, the editor cursor is solid (ignores
    /// blink) and the modal cursor follows `cursor_blink`.
    pub modal_open: bool,
    /// Whether the host terminal window currently has focus.  Driven by
    /// `Event::FocusGained` / `Event::FocusLost` (xterm `CSI ?1004h`).
    /// Defaults to `true` because terminals don't emit a FocusGained at
    /// startup — the window that just launched us is presumed focused.
    /// While `false`, `cursor_visible()` returns false so the in-buffer
    /// cursor disappears.
    pub terminal_focused: bool,
    /// Lazy per-(buffer-version, viewport-width) cache of wrapped row
    /// counts for the raw view.  Mirrors `ParsedDoc::visual_rows` for
    /// rendered mode but lives on `EditorState` because raw mode reads
    /// directly from the buffer.  Without it, every scroll event in raw
    /// mode runs the wrap algorithm over every line of the document
    /// twice (`raw_total_visual_rows` plus `raw_line_at_visual_row`),
    /// which saturates a CPU core on long files when many trackpad-wheel
    /// events queue up.  `RefCell` because `&EditorState` callers in the
    /// view layer need shared access; `EditorState` is single-threaded.
    pub(crate) raw_visual_rows: RefCell<Vec<RawVisualRowCache>>,
    /// Inline-diff review session.  `Some` iff `mode == Mode::Diff`;
    /// the invariant is asserted in `enter_diff_mode` /
    /// `exit_diff_mode` and enforced indirectly by the App's
    /// `dispatch_action` (which only emits `Mode::Diff` after
    /// `enter_diff_mode` returns successfully).
    pub diff: Option<DiffState>,
    /// Scroll offset saved on `enter_diff_mode` and restored on
    /// `exit_diff_mode`.  Diff mode resets `scroll` to 0 on entry so
    /// the user lands at the top of the diff view; restoring on exit
    /// returns the user to where they were in the live buffer.
    pub pre_diff_scroll: usize,
    /// Set by `enter_diff_mode`; consumed on the next frame by the run
    /// loop (`App::prepare_viewport`) to scroll the first focused hunk
    /// into view.  Deferred because the viewport height isn't known at
    /// the modal-close call site where diff mode is entered.
    pub pending_focus_scroll: bool,
}

/// How long the cursor must rest on a block before it is shown in raw mode.
pub const RAW_REVEAL_DELAY: std::time::Duration = std::time::Duration::from_millis(120);

impl EditorState {
    /// Create an `EditorState` from a `Buffer` and a theme.  Used by
    /// integration tests in `tests/`; production callers go through
    /// `new_with_image_config` so the image layout uses real terminal data.
    ///
    /// # Panics
    ///
    /// Panics if the theme reference has an insufficiently long lifetime.
    /// Callers typically pass `Box::leak(Box::new(Theme::default()))` or a
    /// static reference.
    #[allow(dead_code)]
    pub fn new(buffer: Buffer, theme: &'static Theme) -> Self {
        Self::new_with_config(buffer, theme, true, true, 24)
    }

    /// Used by integration tests in `tests/` and by `ui::image_view` tests.
    #[allow(dead_code)]
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
            diagrams_enabled: true,
            parsed_version: 0,
            live_table_widths: None,
            row_striping: false,
            big_h1: false,
            viewport_width: 80,
            pending_column_widths_commit: None,
            pending_link_follow: None,
            parsed_dirty: false,
            cursor_block_line_range: None,
            cursor_blink: CursorBlink::default(),
            modal_open: false,
            terminal_focused: true,
            raw_visual_rows: RefCell::new(Vec::new()),
            diff: None,
            pre_diff_scroll: 0,
            pending_focus_scroll: false,
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

    // ── Buffer access ─────────────────────────────────────────────

    /// Used by tests in this crate.
    #[allow(dead_code)]
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

    /// Replace the editor's buffer with `new_buffer` and reset every
    /// derived field that the old buffer made stale: history, both
    /// selection caches, the parsed-doc cache (via `refresh_parsed`),
    /// and the cursor-block lookup.  The cursor's char offset is
    /// clamped to the new buffer's length but otherwise preserved
    /// best-effort, so a silent reload (clean buffer + external edit)
    /// keeps the user roughly where they were.  Viewport scroll is
    /// intentionally not reset — preserving it matches user expectations
    /// when the external rewrite is small.
    ///
    /// This is the canonical buffer-swap entry point.  New consumers
    /// (multi-tab switch, future diff-mode resolve, …) should call
    /// this rather than mutating `buffer` directly and hand-resetting
    /// derived state, so adding a new derived field is a single-edit
    /// change instead of a hunt for every swap site.
    pub fn replace_buffer(&mut self, new_buffer: Buffer) {
        let new_len = new_buffer.len_chars();
        self.buffer = new_buffer;
        self.dirty = false;
        self.history = History::new();
        self.selection = None;
        self.visual_selection = None;
        self.cursor.offset = self.cursor.offset.min(new_len);
        self.refresh_parsed();
        self.update_cursor_block();
    }

    /// Enter diff review mode with `diff_state` as the active review.
    /// Saves the current `scroll` so [`Self::exit_diff_mode`] can
    /// restore it, then resets `scroll = 0` and sets
    /// `mode = Mode::Diff`.  The caller (`App::enter_diff_mode`) is
    /// responsible for having already verified that
    /// `DiffState::new` returned `Some` — empty hunk lists must not
    /// reach this entry point (§4).
    pub fn enter_diff_mode(&mut self, diff_state: DiffState) {
        self.pre_diff_scroll = self.scroll;
        self.scroll = 0;
        self.diff = Some(diff_state);
        self.mode = Mode::Diff;
        self.selection = None;
        self.visual_selection = None;
        // Defer the scroll-to-first-hunk until the next frame, when the
        // run loop knows the viewport height.
        self.pending_focus_scroll = true;
    }

    /// Scroll so the focused hunk is comfortably visible — a few rows
    /// of context above it when possible.  No-op outside diff mode.
    /// Called on entry (via the deferred `pending_focus_scroll` flag)
    /// and after every hunk-focus change (`DiffNext` / `DiffPrev` /
    /// accept / reject).
    pub fn scroll_focused_hunk_into_view(&mut self, viewport_height: usize, viewport_width: usize) {
        if viewport_height == 0 {
            return;
        }
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        /// Rows of context kept above the focused hunk when scrolling
        /// it into view from off-screen.
        const TOP_MARGIN: usize = 3;
        let row = diff.focused_hunk_visual_row(viewport_width);
        let total = diff.total_visual_rows(viewport_width);
        let max_scroll = total.saturating_sub(1);
        // Reposition only when the hunk isn't already comfortably in
        // view: above the current top (+margin) or below the bottom.
        let comfortably_visible =
            row >= self.scroll + TOP_MARGIN && row < self.scroll + viewport_height;
        if !comfortably_visible {
            self.scroll = row.saturating_sub(TOP_MARGIN).min(max_scroll);
        }
    }

    /// Drop the active diff review, restore the pre-diff scroll, and
    /// return to `Mode::Rendered`.  Used both on the resolution happy
    /// path (after `Buffer::set_rope` swaps the merged rope in) and on
    /// the discard path that abandons the review without applying it
    /// (e.g. quitting mid-review).  The caller is responsible for any
    /// buffer / cursor side effects before this; this helper only
    /// cleans up the diff fields.
    pub fn exit_diff_mode(&mut self) {
        self.diff = None;
        self.scroll = self.pre_diff_scroll;
        self.pre_diff_scroll = 0;
        if self.mode == Mode::Diff {
            self.mode = Mode::Rendered;
        }
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

    /// Toggle big-text H1 rendering and re-render so the change is
    /// visible on the next frame.  Wired to `config.editor.big_h1` at
    /// App startup and after a live config reload.
    pub fn set_big_h1(&mut self, on: bool) {
        if self.big_h1 == on {
            return;
        }
        self.big_h1 = on;
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
    /// needs to decide whether to commit (or open the warning modal). Used
    /// by integration tests in `tests/`.
    #[allow(dead_code)]
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
        // Diagram blocks honour `self.diagrams_enabled` at promotion
        // time — when false, `build_with_overrides` leaves the mermaid
        // fenced code blocks intact, so the row override never sees a
        // diagram URL and only has to think about real images.
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
            self.big_h1,
            self.diagrams_enabled,
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
            self.cursor_blink.reset();
        }
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
}

/// Fetch the text of buffer line `line`, stripped of any trailing newline.
/// `pub(super)` so sibling state-impl modules (visual nav, viewport) can
/// reach it without re-deriving.
pub(super) fn line_text_trimmed(buf: &crate::document::Buffer, line: usize) -> String {
    buf.line(line)
        .map(|s| s.trim_end_matches('\n').to_owned())
        .unwrap_or_default()
}

// ── Cursor screen-row helpers ──────────────────────────────────────────────

fn raw_cursor_screen_row(state: &EditorState, width: usize) -> usize {
    raw_cursor_visual_row(state, width).saturating_sub(state.scroll)
}

pub(super) fn raw_cursor_visual_row(state: &EditorState, width: usize) -> usize {
    let (cursor_line, cursor_col) = state.cursor.line_col(&state.buffer);
    let rows = state.visual_rows_before_raw_line(cursor_line, width);
    let cursor_text = line_text_trimmed(&state.buffer, cursor_line);
    let cursor_rows = crate::ui::line_render::visual_rows_of_str(&cursor_text, width);
    let (sub, _) = crate::ui::line_render::sub_line_of_col(&cursor_rows, cursor_col);
    rows + sub
}

fn set_raw_scroll_for_screen_row(state: &mut EditorState, target_row: usize, width: usize) {
    state.scroll = raw_cursor_visual_row(state, width).saturating_sub(target_row);
}

fn rendered_cursor_screen_row(state: &EditorState, width: usize) -> usize {
    rendered_cursor_visual_row(state, width).saturating_sub(state.scroll)
}

pub(super) fn rendered_cursor_visual_row(state: &EditorState, width: usize) -> usize {
    let cursor_rendered = cursor_rendered_line_idx(state);
    let rows_before = state.parsed.visual_rows_before(cursor_rendered, width);
    rows_before + cursor_sub_line_in_rendered(state, cursor_rendered, width)
}

fn set_rendered_scroll_for_screen_row(state: &mut EditorState, target_row: usize, width: usize) {
    state.scroll = rendered_cursor_visual_row(state, width).saturating_sub(target_row);
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
pub(crate) fn cursor_rendered_line_idx(state: &EditorState) -> usize {
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
            source.get(r.start..end).unwrap_or("").to_owned()
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

    // ── Diff scroll-into-view ───────────────────────────────────────────

    /// Entering diff mode sets `pending_focus_scroll`, and
    /// `scroll_focused_hunk_into_view` brings an off-screen focused hunk
    /// into view with a small top margin.
    #[test]
    fn scroll_focused_hunk_into_view_scrolls_offscreen_hunk_up() {
        // 20 context lines precede the change, so the focused hunk's
        // first row is at visual row 20 — far below a 5-row viewport.
        let mut old = String::new();
        for i in 0..20 {
            old.push_str(&format!("ctx{i}\n"));
        }
        let mut new = old.clone();
        old.push_str("before\n");
        new.push_str("AFTER\n");
        let diff = crate::diff::DiffState::new(&old, &new).unwrap();

        let mut state = EditorState::new(Buffer::from_str(&old), theme());
        state.scroll = 0;
        state.enter_diff_mode(diff);
        // enter_diff_mode requests a deferred scroll and zeroes scroll.
        assert!(state.pending_focus_scroll);
        assert_eq!(state.scroll, 0);

        // Resolve it: focused row 20, top margin 3 → scroll 17.
        state.scroll_focused_hunk_into_view(5, 80);
        assert_eq!(state.scroll, 17);

        // Idempotent: the hunk is now comfortably visible, so a second
        // call leaves the scroll unchanged.
        state.scroll_focused_hunk_into_view(5, 80);
        assert_eq!(state.scroll, 17);
    }

    /// A hunk already on the first screen needs no scrolling.
    #[test]
    fn scroll_focused_hunk_into_view_noop_when_already_visible() {
        let diff = crate::diff::DiffState::new("a\nB\nc\n", "a\nBB\nc\n").unwrap();
        let mut state = EditorState::new(Buffer::from_str("a\nB\nc\n"), theme());
        state.enter_diff_mode(diff);
        state.scroll_focused_hunk_into_view(20, 80);
        assert_eq!(state.scroll, 0);
    }
}
