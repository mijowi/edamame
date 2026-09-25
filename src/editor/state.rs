use std::cell::RefCell;
use std::time::{Duration, Instant};

use crate::config::Theme;
use crate::diff::DiffState;
use crate::document::{Buffer, Cursor, EditDelta, History, ParsedDoc, Selection, VisualSelection};
use crate::editor::state_viewport::RawVisualRowCache;
use crate::editor::vim_ops::SubstitutePreview;
use crate::editor::Mode;
use crate::editor::YankFlash;
use crate::image::ImageCache;
use crate::markdown::RenderCache;
use crate::search::SearchState;

// ── Cursor blink ─────────────────────────────────────────────────────

/// Fallback blink cadence when no config value is supplied.  Mirrors `EditorConfig`'s
/// `cursor_blink_ms` default so behavior matches a fresh install.
const BLINK_INTERVAL: Duration = Duration::from_millis(530);

/// On/off phase of a blinking cursor.  Any cursor movement resets the phase to visible, so the
/// cursor is immediately apparent after a keypress.
#[derive(Debug, Clone)]
pub struct CursorBlink {
    blinking: bool,
    visible: bool,
    interval: Duration,
    last_toggle: Instant,
}

impl Default for CursorBlink {
    fn default() -> Self {
        Self {
            blinking: true,
            visible: true,
            interval: BLINK_INTERVAL,
            last_toggle: Instant::now(),
        }
    }
}

impl CursorBlink {
    /// A zero `interval_ms` falls back to [`BLINK_INTERVAL`] so a stray `0` can't spin the redraw
    /// loop.
    pub fn from_config(blinking: bool, interval_ms: u64) -> Self {
        let interval = if interval_ms == 0 {
            BLINK_INTERVAL
        } else {
            Duration::from_millis(interval_ms)
        };
        Self {
            blinking,
            visible: true,
            interval,
            last_toggle: Instant::now(),
        }
    }

    /// Re-apply config live (settings overlay), resetting the phase so the cursor reappears
    /// immediately when blinking is turned off.
    pub fn apply_config(&mut self, blinking: bool, interval_ms: u64) {
        self.blinking = blinking;
        if interval_ms != 0 {
            self.interval = Duration::from_millis(interval_ms);
        }
        self.reset();
    }

    /// Just the configured half — `is_visible` folds phase in.  Split out so a test can assert a
    /// freshly built `EditorState` picked the setting up; the two construction sites have drifted
    /// on exactly this before.
    pub fn is_blinking(&self) -> bool {
        self.blinking
    }

    /// Whether the cursor should be painted this frame.
    pub fn is_visible(&self) -> bool {
        !self.blinking || self.visible
    }

    /// Make the cursor visible and restart the timer.  Call on any cursor move or edit.
    pub fn reset(&mut self) {
        self.visible = true;
        self.last_toggle = Instant::now();
    }

    /// Advance the blink state; `true` when visibility changed and a redraw is needed.
    pub fn tick(&mut self) -> bool {
        if !self.blinking {
            return false;
        }
        if self.last_toggle.elapsed() >= self.interval {
            self.visible = !self.visible;
            self.last_toggle = Instant::now();
            true
        } else {
            false
        }
    }

    /// When the next toggle fires, or `None` when blinking is disabled.
    pub fn next_toggle(&self) -> Option<Instant> {
        if self.blinking {
            Some(self.last_toggle + self.interval)
        } else {
            None
        }
    }
}

// ── Image reveal ─────────────────────────────────────────────────────

/// The row reservation the raw-source reveal wants for the image block the cursor rests in.  See
/// [`EditorState::image_reveal`].
///
/// **Both `ordinal` and `url` are load-bearing.**  Matching on the URL alone would collapse every
/// block sharing it (one logo used twice).  The URL is the staleness check: a reservation held
/// across an in-line edit can name a block that is no longer an image, and requiring both means
/// such a pair matches nothing rather than sliding onto the next image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageReveal {
    /// Index into `ParsedDoc::image_blocks`.
    pub(crate) ordinal: usize,
    /// A `![alt](url)` target, or a diagram's synthetic `diagram-mermaid-<sha256>` key.
    pub(crate) url: String,
    /// Rendered rows to reserve for the raw-source reveal: one per
    /// revealed raw source line.
    pub(crate) rows: usize,
    /// Rows reserved for a live rendering of the formula, painted as a
    /// band at the block's **top** with the editable raw source below it —
    /// so the formula keeps the position it had before the reveal opened
    /// instead of jumping down under the source.  Non-zero only for
    /// `$$...$$` math blocks, and only when `EditorState::math_preview` is
    /// on (a preview is useless for mermaid — the whole point of its reveal
    /// is seeing the source — and ordinary images have a single source
    /// line).  Math formulas re-render on every keystroke, so this is how
    /// the user watches the formula take shape while typing.  The editor
    /// override reserves `preview_rows + rows`; `ParsedDoc::math_source_offset`
    /// records the band so the row ⇄ source-line mapping paints the source
    /// beneath it.
    pub(crate) preview_rows: usize,
}

/// All mutable state owned by the editor: the single source of truth for document contents,
/// cursor, selection, history, and mode.  Mutated by `edit_ops::apply`, read by the UI layer.
pub struct EditorState {
    pub buffer: Buffer,
    pub cursor: Cursor,
    pub selection: Option<Selection>,
    /// Preview-mode selection in rendered coordinates; cleared on a switch to Rendered or Raw,
    /// which select from the raw buffer instead.
    pub visual_selection: Option<VisualSelection>,
    pub history: History,
    pub mode: Mode,
    pub parsed: ParsedDoc,
    /// Unsaved changes since the last save.
    ///
    /// Autosave keys off the pair `(dirty, Buffer::version())`, so a path that sets `dirty = true`
    /// without bumping `Buffer::version()` would silently never autosave — keep the two in
    /// lockstep.  Clearing `dirty` without a save (a revert) is fine.
    pub dirty: bool,
    /// Internal clipboard, used when arboard is unavailable.
    pub kill_ring: String,
    /// Scroll offset in visual rows for the active mode.
    pub scroll: usize,
    /// Block the cursor is inside; used for reveal jitter suppression.
    pub cursor_block_idx: Option<usize>,
    /// Cursor's buffer line, so the reveal timer resets per logical line rather than per block.
    pub cursor_line_idx: Option<usize>,
    /// When the cursor last moved to a new buffer line.  The raw reveal waits `RAW_REVEAL_DELAY`
    /// without further movement, which is what stops multi-line elements flickering under fast
    /// cursor movement.
    pub cursor_block_entered_at: Option<Instant>,
    /// Latch for a "reveal as one unit" block (a mermaid diagram or a reflowed paragraph): once
    /// such a block reveals on a dwell it stays revealed while the cursor is inside it, even as
    /// line moves re-arm [`Self::cursor_block_entered_at`].  So scrolling *through* one never
    /// reveals it (the delay re-arms per line like every other block), a dwell does, and moving
    /// within a revealed one never flashes it collapsed.  Reset on crossing into another block
    /// ([`Self::update_cursor_block`]); set by [`Self::latch_cursor_reveal`].
    pub cursor_reveal_latched: bool,
    /// When the last click-driven table row / column delete landed, guarding the `✕` handles
    /// against an accidental double-click.  Anchored to the delete rather than the multi-click
    /// chord, whose window restarts on every press and so would never expire under sustained
    /// clicking.  Stamped only on a delete that actually applied.
    pub last_table_delete_at: Option<Instant>,
    /// While a drag is in progress the cursor's block is never de-rendered: revealing raw
    /// mid-drag would shift the visible columns and jump the selection anchor.
    pub drag_in_progress: bool,
    /// Theme the rendered lines were produced with.
    theme: &'static Theme,
    /// Whether to preserve multiple consecutive blank lines between blocks.
    preserve_blank_lines: bool,
    /// Whether Up/Down navigate by visual lines rather than logical ones.
    pub visual_line_nav: bool,
    /// Row ceiling per `Block::ImageBlock`, from `Config::image::max_height`, so renderer,
    /// navigation, and `image_view::paint_images` agree on the reservation.
    pub image_max_height: usize,
    /// Cell ceiling on an image block's width.  With `image_max_height` and `image_font_size` it
    /// gives the aspect-aware row count — a wide image fits in width first and reserves fewer rows.
    pub image_max_width: usize,
    /// Font size in pixels from the detected image picker; the default mirrors
    /// `Picker::from_fontsize`'s Halfblocks default.
    pub image_font_size: (u16, u16),
    /// Decoded-image cache keyed by URL, retained across reparses so ordinary edits don't
    /// invalidate the expensive `StatefulProtocol` encoding.  Filled by the decode worker.
    pub images: ImageCache,
    /// When `false`, every image block collapses to its one-line `[Image: alt]` placeholder.  Set
    /// by the App when the user declines image rendering.
    pub images_enabled: bool,
    /// [`Self::images_enabled`]'s counterpart for diagram blocks, so a user can opt in to images
    /// but not diagrams.  Image blocks with a `source` honor this instead.
    pub diagrams_enabled: bool,
    /// Bumped on every `refresh_parsed` **and** on every deferred in-line edit (which leaves
    /// `parsed` stale but changes the cursor block's geometry), so the view's per-frame snapshot
    /// caches invalidate exactly when the painted geometry changed.
    ///
    /// Because the deferred path moves it, this is the *wrong* key for a cache derived from the
    /// parse tree alone — that would rebuild per keystroke.  Such a cache belongs on `ParsedDoc`,
    /// where a reparse drops it by construction.
    pub parsed_version: u64,
    /// Live-preview scratch for a column-resize drag: the table at `table_byte_start` renders
    /// with these widths without the buffer being touched.  Cleared on commit or cancel.
    pub live_table_widths: Option<(usize, Vec<Option<usize>>)>,
    /// From `config.table.row_striping`; re-read on every `refresh_parsed`.
    pub row_striping: bool,
    /// From `config.editor.big_h1`; re-read on every `refresh_parsed`.
    pub big_h1: bool,
    /// From `config.editor.syntax_highlighting`; re-read on every `refresh_parsed`.
    pub syntax_highlighting: bool,
    /// Propagated from `config.figures.math_preview`.  When true, a revealed `$$...$$` block keeps
    /// the rendered formula in place and opens its raw source below it (`ImageReveal::preview_rows`);
    /// when false the reveal shows the source alone, like a mermaid fence.  Set at construction and
    /// pushed live via [`Self::set_math_preview`]; a bare `EditorState` defaults it off (the
    /// on-by-default value lives in `FiguresConfig::default`).
    pub math_preview: bool,
    /// Most-recent document-area width, fed to the renderer on every `refresh_parsed` so table
    /// column widths adapt to the viewport.  Stored here rather than threaded as a parameter so
    /// call sites that don't know the width — undo/redo, paste, file load — pick up the cached
    /// value.  Defaults to 80 until the App posts the real width.
    pub viewport_width: usize,
    /// Set on a column-drag release: the App must either commit `live_table_widths` or open the
    /// width-injection warning.  Carries the table's `table_byte_start`; cleared by
    /// [`Self::commit_pending_column_widths`] / [`Self::cancel_pending_column_widths`].
    pub pending_column_widths_commit: Option<usize>,
    /// A requested link follow, consumed by the App on the next loop iteration.  Parking the
    /// intent here keeps `mouse_ops::apply` to its `&mut EditorState` contract.
    pub pending_link_follow: Option<crate::editor::link::LinkTarget>,
    /// Set when an in-line edit (no newline added or removed) has left `parsed` stale.  The
    /// rendered view paints the cursor block raw from the buffer, so the staleness is invisible
    /// until a parse-dependent path calls [`Self::flush_parsed_if_dirty`].  Cross-line edits
    /// re-parse immediately instead.
    pub parsed_dirty: bool,
    /// Whether the current `parsed` was built with paragraph reflow on.  Reflow depends on the
    /// mode (`want_reflow`: on in Preview and Rendered, off in Raw), but `parsed` is one
    /// mode-independent spine, so a mode switch that changes it must reparse.
    /// [`Self::sync_reflow_for_mode`] compares this against the mode each frame.
    parsed_reflow: bool,
    /// Master switch for paragraph reflow (soft breaks → spaces, wrapped as one flow), from
    /// `config.editor.reflow`.  On by default.  Gates both Preview and Rendered (in Rendered the
    /// revealed cursor block expands to its raw lines via `EffectiveRows`); Raw and Diff never
    /// reflow.  See `docs/dev/plans/paragraph-reflow.md`.
    pub(crate) reflow: bool,
    /// Whether the cursor block was reflow-revealed last frame.  A change means the block's
    /// height just toggled under a cursor that no keypress moved, which is when
    /// [`Self::anchor_reflow_reveal`] re-checks cursor visibility.  Inert unless [`Self::reflow`]
    /// is on.
    prev_reflow_has_reveal: bool,
    /// Memo for the reveal patch [`Self::effective_rows`] hands out.  Building it allocates the
    /// revealed block's source and measures each raw line's wrap; the viewport, scrollbar, cursor
    /// row, and gutter each query `effective_rows` per frame, so without this that work repeats
    /// several times a frame.  `RefCell` because `effective_rows` runs behind `&self` (widgets
    /// query it mid-render).  Keyed on `parsed_version`, so a reparse invalidates it implicitly.
    effective_cache: std::cell::RefCell<crate::editor::effective_rows::EffectiveRowsCache>,
    /// Buffer line range of the cursor's block as of the last `update_cursor_block`.  Stable
    /// across in-line typing, which is what lets the rendered view read the block's raw text from
    /// the live buffer without consulting the stale `source_map`.
    pub cursor_block_line_range: Option<std::ops::Range<usize>>,
    /// Blink state, ticked by the App before each draw.
    pub cursor_blink: CursorBlink,
    /// Set by the App when a modal overlay is visible: the editor cursor goes solid and the modal
    /// cursor takes over the blink.
    pub modal_open: bool,
    /// Terminal window focus, from `Event::FocusGained` / `FocusLost`.  Defaults to `true` because
    /// terminals emit no FocusGained at startup.  While `false` the in-buffer cursor disappears.
    pub terminal_focused: bool,
    /// Lazy per-(buffer-version, viewport-width) cache of wrapped row counts for the raw view —
    /// `ParsedDoc::visual_rows`'s counterpart, living here because raw mode reads the buffer
    /// directly.  Without it every raw-mode scroll event re-wraps the whole document twice, which
    /// saturates a core on long files under queued wheel events.  `RefCell` because the view layer
    /// holds only `&EditorState`.
    pub(crate) raw_visual_rows: RefCell<Vec<RawVisualRowCache>>,
    /// Inline-diff review session; `Some` iff `mode == Mode::Diff`.
    pub diff: Option<DiffState>,
    /// Scroll saved on `enter_diff_mode` and restored on exit, since diff mode zeroes `scroll` to
    /// land the user at the top of the review.
    pub pre_diff_scroll: usize,
    /// Requests a scroll-to-focused-hunk on the next frame.  Deferred because the viewport height
    /// isn't known where diff mode is entered.
    pub pending_focus_scroll: bool,
    /// Active search-and-replace flow.  Unlike `diff` it does *not* change `mode` — the document
    /// keeps rendering with highlights on top.  While `Some`, the input handler intercepts the
    /// flow keys and `search_safe_action` default-denies everything else.
    pub search: Option<SearchState>,
    /// True for a document that cannot be edited — today, a page out of the embedded manual.
    ///
    /// It pins the editor in [`Mode::Preview`] (already the browse-only presentation) by making
    /// `edit_ops::enter_edit_if_preview` refuse the transition, which no-ops all of that
    /// function's call sites at once; [`Self::apply_delta`] is the second backstop, for text
    /// rather than mode.  The App reads it too, but only to refuse politely — the *guarantee* is
    /// made here, below every input path.
    ///
    /// Deliberately a bare flag rather than a `DocId` (layer 5 has no business knowing what
    /// documentation is) and not a fourth `Mode` (a doc page renders in the ordinary three views).
    pub readonly: bool,
    /// Recently-yanked span, painted as a brief highlight to confirm the copy.  A transient
    /// overlay, not a flow — independent of `mode` and `search`.  See `editor::yank_flash`.
    pub yank_flash: Option<YankFlash>,
    /// Live `:s` substitution preview (neovim's `inccommand=nosplit`).  While `Some` the buffer
    /// may have been transiently rewritten through the raw `Buffer` primitives — no undo delta,
    /// `dirty` untouched — with the inverse edit stashed inside; autosave is suspended, mutating
    /// mouse ops are gated, and search freshness is paused.  See `editor::vim_ops::preview`.
    pub substitute_preview: Option<SubstitutePreview>,
    /// Row reservation for the image block whose raw source is currently revealed.
    ///
    /// An image block reserves as many rows as its *image* occupies, which has nothing to do with
    /// how many source lines it was written on — so an unadjusted reveal clips a tall mermaid
    /// fence and leaves dead space around a one-line `![alt](url)`.  While the cursor rests in
    /// such a block it instead reserves one row per raw source line and the document reflows.
    /// Kept in sync by [`Self::sync_image_reveal`] once per frame.
    pub(crate) image_reveal: Option<ImageReveal>,
    /// Block-level render memoization: blocks whose AST is unchanged reuse their rendered lines.
    /// Keyed by block value plus a render-settings fingerprint, so theme / width / striping
    /// changes clear it automatically.  See `docs/dev/performance.md`.
    render_cache: RenderCache,
    /// A second cache, for the diff's rendered new-side parse only.  It cannot share
    /// [`Self::render_cache`]: eviction is by document membership per build, so the two parses
    /// would evict each other's entries wholesale on every frame of a resize drag.
    diff_render_cache: RenderCache,
    /// The diff's rendered new-side parse is stale.  Set by [`Self::enter_diff_mode`], which
    /// deliberately does not build it: at that moment `viewport_width` still holds the *editor*
    /// mode's width, which reserves a line-number gutter that diff mode does not, so a parse built
    /// there lays every table out against a width this view never paints at.
    pub(crate) diff_parse_dirty: bool,
}

/// How long the cursor must rest on a block before it is shown in raw mode.
pub const RAW_REVEAL_DELAY: std::time::Duration = std::time::Duration::from_millis(120);

impl EditorState {
    /// Test-only convenience; production callers use [`Self::new_with_image_config`] so image
    /// layout uses real terminal data.
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

    /// Full constructor.  The App uses this after capability detection so aspect-aware image row
    /// computation runs on the probed font size and configured max width.
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
            cursor_reveal_latched: false,
            last_table_delete_at: None,
            drag_in_progress: false,
            theme,
            preserve_blank_lines,
            visual_line_nav,
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
            // Off here, like `big_h1`: "on by default" is carried by `EditorConfig::default()`
            // and applied through `app::configure_new_editor`.
            syntax_highlighting: false,
            math_preview: false,
            viewport_width: 80,
            pending_column_widths_commit: None,
            pending_link_follow: None,
            parsed_dirty: false,
            // Matches `ParsedDoc::build` above (reflow off).  The App's initial `refresh_parsed`
            // reconciles this with the Preview default before the first frame.
            parsed_reflow: false,
            reflow: true,
            prev_reflow_has_reveal: false,
            effective_cache: std::cell::RefCell::new(Default::default()),
            cursor_block_line_range: None,
            cursor_blink: CursorBlink::default(),
            modal_open: false,
            terminal_focused: true,
            raw_visual_rows: RefCell::new(Vec::new()),
            diff: None,
            pre_diff_scroll: 0,
            pending_focus_scroll: false,
            search: None,
            readonly: false,
            yank_flash: None,
            substitute_preview: None,
            image_reveal: None,
            render_cache: RenderCache::default(),
            diff_render_cache: RenderCache::default(),
            diff_parse_dirty: false,
        };
        // Seed the cursor-block cache so the first frame has line-range info, but leave the
        // reveal timer unarmed: there is no prior position to animate from, and a fresh state
        // should reveal raw immediately rather than after a 120 ms wait.
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

    /// `(char_count, line_count)` of the active selection, counted over rendered text in Preview
    /// and over the raw buffer otherwise.
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
                let mut col_start = if row == sr { sc } else { 0 };
                let mut col_end = if row == er { ec.min(row_len) } else { row_len };
                // A cell-banded selection counts only the band, matching what copy extracts.
                if let Some(band) = vs.band {
                    let (lo, hi) = band.char_cols(line);
                    col_start = col_start.max(lo);
                    col_end = col_end.min(hi.min(row_len));
                }
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
            // A selection ending just past a `\n` covers one fewer line than its end index
            // suggests, so clamp onto content rather than the next line's first byte.
            let end_inclusive = end.saturating_sub(1);
            let end_line = self.buffer.char_to_line(end_inclusive.max(start));
            Some((chars, end_line - start_line + 1))
        }
    }

    /// The theme the rendered lines were produced with, so hit-testing can recognize a span by
    /// the style the renderer gave it.
    pub fn theme(&self) -> &'static Theme {
        self.theme
    }

    /// Swap the theme and re-render so styled spans pick up the new palette live.  No-op when the
    /// reference is unchanged — same address means same theme.
    pub fn set_theme(&mut self, theme: &'static Theme) {
        if std::ptr::eq(self.theme, theme) {
            return;
        }
        self.theme = theme;
        self.refresh_parsed();
    }

    /// The canonical buffer-swap entry point: resets every field the old buffer made stale, so
    /// adding a derived field is a single edit here rather than a hunt for swap sites.  Never
    /// mutate `buffer` directly instead.
    ///
    /// The cursor offset is clamped but otherwise preserved, and scroll is deliberately kept, so
    /// a silent reload after a small external rewrite leaves the user where they were.
    pub fn replace_buffer(&mut self, new_buffer: Buffer) {
        // All three name positions in the old contents: the search matches, the `:s` preview's
        // stashed revert delta, and the reveal's row reservation.  Drop rather than re-anchor.
        self.search = None;
        self.substitute_preview = None;
        self.image_reveal = None;
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

    /// Enter diff review mode.  The caller must already have verified `DiffState::new` returned
    /// `Some` — an empty hunk list must not reach here.
    pub fn enter_diff_mode(&mut self, diff_state: DiffState) {
        self.pre_diff_scroll = self.scroll;
        self.scroll = 0;
        self.diff = Some(diff_state);
        self.mode = Mode::Diff;
        self.diff_parse_dirty = true; // Deferred a frame — see the field's doc.
        self.selection = None;
        self.visual_selection = None;
        self.pending_focus_scroll = true;
    }

    /// Scroll the focused hunk comfortably into view.  No-op outside diff mode.
    pub fn scroll_focused_hunk_into_view(&mut self, viewport_height: usize, viewport_width: usize) {
        if viewport_height == 0 {
            return;
        }
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let row = diff.focused_hunk_visual_row(viewport_width);
        let total = diff.total_visual_rows(viewport_width);
        self.scroll_row_comfortably_into_view(row, total, viewport_height);
    }

    /// Shared core of the hunk / match / `:s`-preview / incsearch focus scrolls: reposition only
    /// when `row` isn't already comfortably in view.
    fn scroll_row_comfortably_into_view(
        &mut self,
        row: usize,
        total: usize,
        viewport_height: usize,
    ) {
        /// Rows of context kept above the focused row.
        const TOP_MARGIN: usize = 3;
        let max_scroll = total.saturating_sub(1);
        let comfortably_visible =
            row >= self.scroll + TOP_MARGIN && row < self.scroll + viewport_height;
        if !comfortably_visible {
            self.scroll = row.saturating_sub(TOP_MARGIN).min(max_scroll);
        }
    }

    /// [`Self::scroll_row_comfortably_into_view`] targeting the cursor, for the flows that park
    /// it somewhere possibly off-screen.
    pub fn scroll_cursor_comfortably_into_view(
        &mut self,
        viewport_height: usize,
        viewport_width: usize,
    ) {
        if viewport_height == 0 || viewport_width == 0 {
            return;
        }
        let row = self.cursor_visual_row(viewport_width);
        let total = self.total_visual_rows_for_mode(viewport_width);
        self.scroll_row_comfortably_into_view(row, total, viewport_height);
    }

    /// Park the cursor at char `offset` (clamped), refreshing the preferred column and the
    /// cursor-block cache.
    pub fn place_cursor(&mut self, offset: usize) {
        self.cursor.offset = offset.min(self.buffer.len_chars());
        self.cursor.preferred_col = self.cursor.cell_col(&self.buffer);
        self.update_cursor_block();
    }

    /// The restore half of the `:s` preview and vim incsearch sessions.
    pub fn restore_view(&mut self, cursor: usize, scroll: Option<usize>) {
        self.place_cursor(cursor);
        if let Some(scroll) = scroll {
            self.scroll = scroll;
        }
    }

    /// Drop the active review and return to `Mode::Rendered`.  Serves both the resolve and the
    /// discard paths; the caller owns any buffer / cursor side effects.
    pub fn exit_diff_mode(&mut self) {
        self.diff = None;
        self.scroll = self.pre_diff_scroll;
        self.pre_diff_scroll = 0;
        if self.mode == Mode::Diff {
            self.mode = Mode::Rendered;
        }
    }

    /// Start a search flow.  Clears the selection (the flow paints its own highlights) and defers
    /// the scroll-to-first-match a frame.  Unlike diff, `mode` is untouched.
    pub fn enter_search(&mut self, search_state: SearchState) {
        self.search = Some(search_state);
        self.selection = None;
        self.visual_selection = None;
        self.pending_focus_scroll = true;
        self.ensure_search_fresh();
    }

    /// Drop the active search flow, leaving cursor and viewport on the match reached.  Search is
    /// a *motion*, like vim's `/`: exiting never scrolls back to where it began.
    pub fn exit_search(&mut self) {
        self.search = None;
    }

    /// Bidirectional raw↔rendered char-column map for `raw_line`, cached per buffer line.
    ///
    /// **The only sanctioned way to reach `ParsedDoc::inline_map`.**  That cache is keyed by index
    /// alone, so seeding it with text that isn't the canonical content of `buffer_line_idx`
    /// poisons the entry for every later caller.  A non-matching `raw_line` therefore gets an
    /// uncached map: correct column math for the caller, canonical cache for everyone else.
    pub fn inline_map_for(
        &self,
        buffer_line_idx: usize,
        raw_line: &str,
    ) -> std::borrow::Cow<'_, crate::markdown::InlineColMap> {
        let canonical = self
            .buffer
            .line(buffer_line_idx)
            .is_some_and(|s| s.trim_end_matches('\n') == raw_line);
        if canonical {
            std::borrow::Cow::Borrowed(self.parsed.inline_map(buffer_line_idx, raw_line))
        } else {
            std::borrow::Cow::Owned(crate::markdown::InlineColMap::build(raw_line))
        }
    }

    /// Recompute the search match list if the buffer changed since it was built.
    pub fn ensure_search_fresh(&mut self) {
        let version = self.buffer.version();
        let Some(s) = self.search.as_mut() else {
            return;
        };
        if s.is_fresh(version) {
            return;
        }
        // Materialize only when a recompute is due: `contents()` copies the whole rope, and this
        // runs on every match-navigation keypress.
        let source = self.buffer.contents();
        s.ensure_fresh(&source, version);
    }

    /// Place the cursor at the start of the focused search match, so exit position and the usual
    /// scroll machinery both stay meaningful.
    pub fn sync_cursor_to_search_focus(&mut self) {
        let Some(range) = self.search.as_ref().and_then(|s| s.focused_range()) else {
            return;
        };
        let total_bytes = self.buffer.rope().len_bytes();
        let byte = range.start.min(total_bytes);
        let offset = self.buffer.rope().byte_to_char(byte);
        self.place_cursor(offset);
    }

    /// Scroll the focused search match into view.  The cursor has already been synced to it, so
    /// its visual row is the target.
    pub fn scroll_focused_match_into_view(
        &mut self,
        viewport_height: usize,
        viewport_width: usize,
    ) {
        if self.search.is_none() {
            return;
        }
        self.scroll_cursor_comfortably_into_view(viewport_height, viewport_width);
    }

    /// Toggle row striping and re-render.  Also the tests' public entry point into the otherwise
    /// private `refresh_parsed`.
    pub fn set_row_striping(&mut self, on: bool) {
        if self.row_striping == on {
            return;
        }
        self.row_striping = on;
        self.refresh_parsed();
    }

    /// Toggle big-text H1 rendering and re-render.
    pub fn set_big_h1(&mut self, on: bool) {
        if self.big_h1 == on {
            return;
        }
        self.big_h1 = on;
        self.refresh_parsed();
    }

    /// Toggle syntax highlighting and re-render.
    pub fn set_syntax_highlighting(&mut self, on: bool) {
        if self.syntax_highlighting == on {
            return;
        }
        self.syntax_highlighting = on;
        self.refresh_parsed();
    }

    /// Toggle the `$$...$$` live-edit preview.  Wired to `config.figures.math_preview` at startup
    /// and pushed live by the settings overlay.  Only the reveal reservation depends on it, so
    /// re-sync the reveal: a formula the cursor is inside reflows immediately, elsewhere a no-op.
    pub fn set_math_preview(&mut self, on: bool) {
        if self.math_preview == on {
            return;
        }
        self.math_preview = on;
        self.sync_image_reveal();
    }

    /// Post a new *document area* width (chrome excluded) and re-render if it changed, so table
    /// column widths follow the viewport.
    pub fn set_viewport_width(&mut self, width: usize) {
        let width = width.max(1);
        if self.viewport_width == width {
            return;
        }
        self.viewport_width = width;
        self.refresh_parsed();
    }

    /// Reparse if the reflow the current `parsed` was built with no longer matches the mode
    /// (`want_reflow`), since `parsed` is one mode-independent spine.  Called once per frame from
    /// `App::prepare_viewport`; a no-op except the first frame after a mode switch that changes it.
    pub fn sync_reflow_for_mode(&mut self) {
        if self.parsed_reflow != self.want_reflow() {
            self.refresh_parsed();
        }
    }

    /// Enable (or disable) paragraph reflow (`config.editor.reflow`).  Reparses if this changes
    /// the effective reflow for the current mode.
    pub fn set_reflow(&mut self, on: bool) {
        if self.reflow == on {
            return;
        }
        self.reflow = on;
        self.sync_reflow_for_mode();
    }

    /// Whether the current mode should render paragraphs reflowed: Preview and Rendered do when
    /// `reflow` is on; Raw is verbatim source and Diff has its own parse, so neither ever does.
    pub(crate) fn want_reflow(&self) -> bool {
        self.reflow && matches!(self.mode, Mode::Preview | Mode::Rendered)
    }

    /// The per-frame visual-row view with the raw-reveal patch applied.  Identity unless the
    /// cursor rests in a *reflowed* paragraph that is currently revealed: only then does the
    /// block's raw form (its source lines) differ in height from its rendered (one wrapped flow)
    /// form, so only then must scroll, gutter, and mouse arithmetic count the raw lines instead.
    pub fn effective_rows(&self, width: usize) -> crate::editor::effective_rows::EffectiveRows<'_> {
        use crate::editor::effective_rows::EffectiveRows;
        let width = width.max(1);
        // The cheap decision (no allocation): does the cursor rest in a revealed reflowed block?
        // The block's rendered start uniquely identifies it within a parse, so it — with the parse
        // version and width — keys the memo; an intra-block cursor move stays a hit.
        let reveal = self.reflow_reveal_target();
        let key = (
            self.parsed_version,
            width,
            reveal.as_ref().map(|(rendered, _)| rendered.start),
        );
        if let Some((base_total, patch)) = self.effective_cache.borrow().get(key) {
            return EffectiveRows::from_cached(&self.parsed, width, base_total, patch);
        }
        // Miss: build the view once (the allocating path — the block's source and its raw-line wrap
        // counts) and memoize its parts so the frame's remaining queries reuse them.
        let built = match reveal {
            Some((rendered, cursor_byte)) => {
                let raw = crate::ui::rendered_view::raw_block_cursor(self, cursor_byte);
                // Trailing blanks absorbed into the block range own their own rendered rows, so
                // exclude them — the reveal stacks only the content lines.
                let raw_lines = crate::ui::rendered_view::revealed_source_lines(&raw.source);
                EffectiveRows::with_reveal(&self.parsed, width, rendered, &raw_lines)
            }
            None => EffectiveRows::identity(&self.parsed, width),
        };
        let (base_total, patch) = built.cache_parts();
        self.effective_cache
            .borrow_mut()
            .store(key, base_total, patch);
        built
    }

    /// The revealed reflowed block, if any: its rendered-line range and the cursor byte inside it.
    /// Only `RenderedView` reveals a block as raw; Preview paints pure rendered lines, so a patch
    /// there would make the arithmetic count raw rows the paint never shows.  Cheap — no allocation.
    fn reflow_reveal_target(&self) -> Option<(std::ops::Range<usize>, usize)> {
        if self.mode != Mode::Rendered || !self.cursor_block_revealed() {
            return None;
        }
        let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
        if !self.parsed.is_reflowed_paragraph_at(cursor_byte) {
            return None;
        }
        let block_idx = self.parsed.source_map.block_for_byte(cursor_byte)?;
        let rendered = self.parsed.source_map.rendered_lines_for_block(block_idx);
        (!rendered.is_empty()).then_some((rendered, cursor_byte))
    }

    /// Keep the view sensible across a reflow reveal/un-reveal.  When the cursor rests in a
    /// reflowed paragraph, entering it (after `RAW_REVEAL_DELAY`) expands the block from one
    /// wrapped flow row to its raw source lines, and leaving it collapses it back — a height
    /// change driven by the frame timer, not a keypress.  Called once per frame from
    /// `App::prepare_viewport`; inert unless `reflow` is on in Rendered mode.
    ///
    /// The expansion happens *below* the block's first row, which sits at the same visual row
    /// before and after (the rows above it are unchanged), so simply leaving `scroll` alone pins
    /// the block's top and everything above it and lets only the content below reflow — the same
    /// feel as an image/mermaid reveal.  On the toggle frame we therefore just re-run
    /// `ensure_cursor_visible`, which scrolls the *minimum* to keep the cursor on screen (usually
    /// nothing) rather than dragging the whole document to re-pin the cursor's exact row.
    pub fn anchor_reflow_reveal(&mut self, width: usize, height: usize) {
        if self.mode != Mode::Rendered || !self.reflow || width == 0 {
            // Re-entering (mode switch, reflow-enable) should be treated as a fresh toggle, not a
            // continuation, so the next eligible frame reconciles cursor visibility once.
            self.prev_reflow_has_reveal = false;
            return;
        }
        let has_reveal = self.effective_rows(width).has_reveal();
        if has_reveal != self.prev_reflow_has_reveal {
            self.ensure_cursor_visible(height, width);
        }
        self.prev_reflow_has_reveal = has_reveal;
    }

    /// Commit a pending column-width drag by writing the `<!-- tui-columns: [...] -->` comment
    /// into the buffer.  Cancel goes through [`Self::cancel_pending_column_widths`].
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

    /// Discard a pending column-width drag, snapping the table back to its pre-drag widths.
    pub fn cancel_pending_column_widths(&mut self) {
        self.pending_column_widths_commit = None;
        self.live_table_widths = None;
        self.refresh_parsed();
    }

    /// Whether a released column drag still awaits a commit decision.  Used by `tests/`.
    #[allow(dead_code)]
    pub fn has_pending_column_widths(&self) -> bool {
        self.pending_column_widths_commit.is_some()
    }

    /// Whether that table already carries a `tui-columns` comment, in which case the App skips
    /// the width-injection warning — the user accepted the injection on an earlier drag.
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

    /// Whether the cursor sits inside a Markdown table.  Public so the vim reducer can decide
    /// whether `Tab` means cell navigation.
    pub fn cursor_in_table(&self) -> bool {
        crate::editor::table_edit_ops::cursor_in_table(self)
    }

    /// Re-parse and re-render after an edit.  Called automatically by `edit_ops`.
    pub(crate) fn refresh_parsed(&mut self) {
        let content = self.buffer.contents();
        // Row-override closure over the image cache; see `ImageCache::reserved_rows` for the
        // per-status decision.  A `Pending` entry answers `None` so the renderer falls back to
        // `image_max_height` and layout stays stable while the decode is in flight.
        let images = &self.images;
        let max_w = self.image_max_width as u16;
        let max_h = self.image_max_height as u16;
        let font_size = self.image_font_size;
        let images_enabled = self.images_enabled;
        // `diagrams_enabled` is honored at promotion time, so a disabled diagram or `$$...$$` math
        // block never reaches this override as a URL at all.
        //
        // The reveal is checked first because it replaces the image on screen entirely: neither
        // the decode cache nor the images-disabled collapse has a say in the block's height.
        // Matched on ordinal *and* URL — see `ImageReveal`.
        let image_reveal = self.image_reveal.as_ref();
        let override_fn = |url: &str, ordinal: usize| {
            if let Some(reveal) = image_reveal {
                if reveal.ordinal == ordinal && reveal.url == url {
                    // The raw-source reveal replaces the image's reserved
                    // rows with one row per revealed source line; a
                    // `$$...$$` block with the preview on additionally
                    // reserves a live-preview band for the formula at the
                    // block's top, with the source below it (see
                    // `ImageReveal::preview_rows`).
                    return Some(reveal.rows + reveal.preview_rows);
                }
            }
            // The images-disabled collapse applies to *real* images only.
            // Promoted diagram / `$$...$$` math blocks are gated by
            // `diagrams_enabled` at promotion time — a diagram URL reaching
            // this override means figures are enabled, so it keeps its
            // decoded height regardless of the images toggle.  Without this
            // exemption, turning "Show images" off collapsed every rendered
            // formula and diagram to a single row (tiny).
            if !images_enabled && !crate::diagram::is_diagram_url(url) {
                return Some(1);
            }
            images.reserved_rows(url, max_w, max_h, font_size)
        };
        // Reflow applies per `want_reflow`.  It rides in `RenderSettings`, so a mode switch that
        // changes it clears the render cache; `sync_reflow_for_mode` triggers the reparse.
        let reflow_paragraphs = self.want_reflow();
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
            self.syntax_highlighting,
            self.diagrams_enabled,
            reflow_paragraphs,
            Some(&mut self.render_cache),
        );
        self.parsed_reflow = reflow_paragraphs;
        self.parsed_version = self.parsed_version.wrapping_add(1);
        self.parsed_dirty = false;
        // Record the math-preview split so the row ⇄ source-line mapping paints the rendered
        // formula in the block's top rows and the editable `$$...$$` source below — keeping the
        // image where it sat before the reveal.  Non-zero only for a revealed `$$...$$` block with
        // preview on (`ImageReveal::preview_rows`).
        self.parsed.math_source_offset = self.image_reveal.as_ref().and_then(|r| {
            if r.preview_rows == 0 {
                return None;
            }
            let block_idx = self.parsed.image_blocks.get(r.ordinal)?.block_idx;
            Some((block_idx, r.preview_rows))
        });
        // Evict unreferenced URLs — editing inside a mermaid fence or `$$...$$` block mints a new
        // synthetic URL per keystroke, orphaning the old entry.  The live set is the *union* of
        // both parses while a review is open: a URL only on the diff's new side would otherwise be
        // evicted here and immediately re-requested, a decode/evict loop lasting the whole review.
        let mut live: std::collections::HashSet<String> = self
            .parsed
            .image_blocks
            .iter()
            .map(|i| i.url.clone())
            .collect();
        if let Some(parsed_new) = self.diff.as_ref().and_then(|d| d.parsed_new.as_ref()) {
            live.extend(parsed_new.image_blocks.iter().map(|i| i.url.clone()));
        }
        self.images.gc(&live);

        // A tail call rather than a hand-maintained list of sites: everything that re-renders the
        // document comes through here, and several of those are reachable *during* a review, where
        // a stale `parsed_new` would disagree with the row cache about block heights.  No
        // recursion — `refresh_diff_parse` never calls back — and it is one branch outside a
        // review.
        self.refresh_diff_parse();
        // The rebuild may have *shortened* the document — a decoded image block replaces its
        // `image_max_height` reservation with the image's real height the moment its decode lands,
        // and a scrolled reader would otherwise be left past the last row, where the viewport
        // paints nothing.  Clamping here covers every rebuild, not just the image one.
        self.clamp_scroll_to_document();
    }

    /// Rebuild the diff's rendered new-side parse.  No-op outside a review.
    ///
    /// Uses exactly `refresh_parsed`'s render settings, so a rendered context row paints
    /// identically here and in Preview.  Three deliberate differences:
    ///
    /// - `live_table_widths: None` — the column-drag preview belongs to the editor's document.
    /// - a dedicated [`Self::diff_render_cache`] (see its doc).
    /// - no `images.gc()` — that would evict the *editor* document's URLs; `refresh_parsed` owns
    ///   the GC and unions both parses into its live set.
    ///
    /// Reusing the real row override is what makes unchanged media free: `ImageCache` is keyed by
    /// URL, so an unchanged image or diagram is a cache hit with no second set of workers.  There
    /// is no `image_reveal` arm — diff mode has no in-document cursor.
    pub(crate) fn refresh_diff_parse(&mut self) {
        self.diff_parse_dirty = false;
        if self.diff.is_none() {
            return;
        }
        let content = self
            .diff
            .as_ref()
            .expect("checked above")
            .new_buffer
            .contents();
        let parsed = {
            let images = &self.images;
            let max_w = self.image_max_width as u16;
            let max_h = self.image_max_height as u16;
            let font_size = self.image_font_size;
            let images_enabled = self.images_enabled;
            let override_fn = |url: &str, _ordinal: usize| {
                if !images_enabled {
                    return Some(1);
                }
                images.reserved_rows(url, max_w, max_h, font_size)
            };
            ParsedDoc::build_with_overrides(
                &content,
                self.theme,
                self.preserve_blank_lines,
                self.image_max_height,
                None,
                Some(&override_fn),
                self.row_striping,
                self.viewport_width,
                self.big_h1,
                self.syntax_highlighting,
                self.diagrams_enabled,
                // Diff review has its own parse and never reflows.
                false,
                Some(&mut self.diff_render_cache),
            )
        };
        self.diff
            .as_mut()
            .expect("checked above")
            .set_rendered_parse(Some(parsed));
    }

    /// [`Self::flush_parsed_if_dirty`]'s counterpart for the diff parse, called right after the
    /// real diff-mode width is posted.  Free when the width genuinely changed —
    /// `set_viewport_width` will already have done the work.
    pub(crate) fn flush_diff_parse_if_dirty(&mut self) {
        if self.diff_parse_dirty {
            self.refresh_diff_parse();
        }
    }

    /// Re-parse if an in-line edit left `parsed` stale; `true` when it fired.
    ///
    /// **Call this before any path that consults `parsed.source_map` byte ranges** (mouse
    /// hit-tests, cursor-move navigation) — a stale map puts the live cursor's byte in the wrong
    /// block.
    pub fn flush_parsed_if_dirty(&mut self) -> bool {
        if self.parsed_dirty {
            self.refresh_parsed();
            true
        } else {
            false
        }
    }

    /// Apply an edit delta, record it in history, mark dirty, and re-parse when the edit crosses
    /// a line boundary.
    ///
    /// An in-line edit deliberately does *not* re-parse: block line indices don't shift, and the
    /// rendered view reads the cursor block's raw text from the live buffer via
    /// `cursor_block_line_range`.  [`Self::flush_parsed_if_dirty`] catches up later.  This batches
    /// a typing burst into one re-parse and removes the mid-typing rendered → raw → rendered
    /// flash.
    pub(crate) fn apply_delta(&mut self, delta: EditDelta) {
        // The text backstop for a read-only document (the mode backstop is
        // `edit_ops::enter_edit_if_preview`).  Every edit lands here, so the guarantee is *made*
        // rather than maintained: per-site guards were tried first and missed eight paths in turn.
        // Silent by design at this depth — reachable paths report the refusal before getting here.
        if self.readonly {
            return;
        }
        let crosses_line = delta.inserted.contains('\n') || delta.removed.contains('\n');
        let new_cursor = delta.redo_cursor();
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
            // A newline reflows block boundaries; re-parse so the view, source map, and cached
            // line range all agree on the new layout.
            self.refresh_parsed();
            self.update_cursor_block();
        } else {
            // Defer the re-parse, but bump `parsed_version` so the per-frame snapshot caches
            // don't paint stale geometry against a block that just grew or shrank.
            self.parsed_dirty = true;
            self.parsed_version = self.parsed_version.wrapping_add(1);
            self.cursor_blink.reset();
        }
    }

    /// Viewport-relative screen row of the cursor; 0 when it is above the viewport.  Counted in
    /// rendered lines for Rendered / Preview and buffer lines for Raw, matching each view.
    ///
    /// Used by `ToggleRawMode` to hold the cursor on the same screen row across the switch — the
    /// two modes use different scroll units, so the same `scroll` value otherwise lands somewhere
    /// unrelated.
    pub fn cursor_screen_row(&self, viewport_width: usize) -> usize {
        if viewport_width == 0 {
            return 0;
        }
        match self.mode {
            crate::editor::Mode::Raw => raw_cursor_screen_row(self, viewport_width),
            _ => rendered_cursor_screen_row(self, viewport_width),
        }
    }

    /// Set `scroll` so the cursor lands at `target_row`, or the nearest line-start row above it
    /// — quantized by line boundaries, and never past the viewport bottom.
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

/// Text of buffer line `line`, without its trailing newline.
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
    let er = state.effective_rows(width);
    if er.has_reveal() {
        // The cursor rests inside a reflowed, revealed block: its visual row is the rows before
        // the block, plus the raw lines above the cursor's, plus its sub-row within its raw line.
        // `cursor_sub_line_in_rendered` already wraps the cursor's *buffer* line — which is its
        // raw source line — so it supplies that sub-row exactly.
        let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
        let raw = crate::ui::rendered_view::raw_block_cursor(state, cursor_byte);
        return er.raw_line_visual_row(raw.raw_line) + cursor_sub_line_in_rendered(state, 0, width);
    }
    let cursor_rendered = cursor_rendered_line_idx(state);
    let rows_before = state.parsed.visual_rows_before(cursor_rendered, width);
    rows_before + cursor_sub_line_in_rendered(state, cursor_rendered, width)
}

fn set_rendered_scroll_for_screen_row(state: &mut EditorState, target_row: usize, width: usize) {
    state.scroll = rendered_cursor_visual_row(state, width).saturating_sub(target_row);
}

/// Visual sub-line offset of the cursor within its rendered line.  The wrap is taken over the
/// *buffer* text, because the reveal path paints the cursor's line from the live buffer and the
/// rendered text can drop or expand chars relative to source.
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

/// Rendered-line index where the cursor appears, mirroring `ui::rendered_view`'s own computation
/// so scroll arithmetic lands on the line the view actually paints.
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

    // Shared with `RenderedView`, which has one extra branch for a stale parse; this path always
    // sees a fresh one.
    let raw = crate::ui::rendered_view::raw_block_cursor(state, cursor_byte);
    let raw_lines: Vec<&str> = crate::ui::rendered_view::raw_source_lines(&raw.source);

    let cursor_in_block = cursor_sub_line_in_block(
        &state.parsed,
        cursor_byte,
        cursor_block_idx,
        cursor_block_own,
        &raw.source,
        &raw_lines,
        raw.raw_line,
    );

    cursor_block_lines.start + cursor_in_block
}

/// Single-line entry point into [`sub_lines_in_block`]: which rendered sub-line the reveal paints
/// this raw line onto.
///
/// Three callers must agree here — the view (which row to paint raw), `cursor_rendered_line_idx`
/// (where the cursor appears), and `mouse_ops::coord` (whether a click landed on a revealed row).
/// When they disagree, clicks on a revealed line map against the *rendered* spans instead of the
/// raw text on screen, which is wrong for any line with dropped markers.
///
/// `raw_lines` must come from `rendered_view::raw_text::raw_source_lines`.  A `cursor_raw_line`
/// past the last raw line clamps to the block's end.
pub(crate) fn cursor_sub_line_in_block(
    parsed: &ParsedDoc,
    cursor_byte: usize,
    cursor_block_idx: usize,
    cursor_block_own: usize,
    raw_block_source: &str,
    raw_lines: &[&str],
    cursor_raw_line: usize,
) -> usize {
    let subs = sub_lines_in_block(
        parsed,
        cursor_byte,
        cursor_block_idx,
        cursor_block_own,
        raw_block_source,
        raw_lines,
    );
    subs.get(cursor_raw_line)
        .or_else(|| subs.last())
        .copied()
        .unwrap_or(0)
}

/// Rendered sub-line index, relative to the block's first rendered line, for **every** raw line of
/// one block plus one trailing entry for an index past the last (which a stale cursor byte can
/// produce).  The result is always `raw_lines.len() + 1` long.
///
/// The crate's single raw-line → rendered-row derivation; see [`cursor_sub_line_in_block`] for
/// what breaks when a caller re-derives it.  Batch form because the gutter needs a whole block at
/// once and per-line answers were quadratic in the block's length.
///
/// `classify_byte` only picks the block's *flavor*, so any byte inside the block will do.
pub(crate) fn sub_lines_in_block(
    parsed: &ParsedDoc,
    classify_byte: usize,
    block_idx: usize,
    block_own: usize,
    raw_block_source: &str,
    raw_lines: &[&str],
) -> Vec<usize> {
    use crate::markdown::list_layout::raw_list_marker_char_width;
    use crate::ui::table_view::TableSubLineKind;

    let n = raw_lines.len();

    let is_table = crate::editor::table_edit::is_table_block(raw_block_source);
    if is_table && block_own >= 3 {
        let rendered = parsed.source_map.rendered_lines_for_block(block_idx);
        let block_lines = parsed.lines.get(rendered).unwrap_or(&[]);
        let kinds = crate::ui::table_view::classify_table_sub_lines(block_lines);
        let last_replaceable = block_own.saturating_sub(2);
        // Invert `kinds` in one pass; first occurrence wins, matching the `position()` scans
        // this replaces.
        let mut header: Option<usize> = None;
        let mut thick: Option<usize> = None;
        let mut data: Vec<Option<usize>> = Vec::new();
        for (i, kind) in kinds.iter().enumerate() {
            match *kind {
                TableSubLineKind::Header { sub: 0 } => header.get_or_insert(i),
                TableSubLineKind::ThickSeparator => thick.get_or_insert(i),
                TableSubLineKind::DataRow { row, sub: 0 } => {
                    if data.len() <= row {
                        data.resize(row + 1, None);
                    }
                    data[row].get_or_insert(i)
                }
                _ => continue,
            };
        }
        return (0..=n)
            .map(|r| {
                let sub = match r {
                    0 => header.unwrap_or(1),
                    1 => thick.unwrap_or(2),
                    r => data
                        .get(r - 2)
                        .copied()
                        .flatten()
                        .unwrap_or_else(|| 2 * r - 1),
                };
                sub.min(last_replaceable)
            })
            .collect();
    }

    // These all map 1:1: a diagram block's reveal (mermaid fence or `$$...$$` math) paints onto
    // its reserved rows, and code and metadata blocks render every body line including blanks (as
    // NBSP-padded rows).  Falling through to the counting branch below would drift the cursor up
    // one row per blank.
    let is_diagram_reveal = parsed.is_diagram_reveal_block(block_idx);
    let real_block = parsed.real_block_for_byte(classify_byte);
    let is_verbatim = matches!(
        real_block,
        Some(
            crate::markdown::Block::CodeBlock { .. } | crate::markdown::Block::MetadataBlock { .. }
        ) // A figures-off `$$...$$` paragraph renders as a fenced-style `math`
          // code block (see `display_math_block_body`), so its rendered rows map
          // 1:1 onto source lines — including any blank line inside the formula,
          // which the prose branch below would otherwise drop.
    ) || real_block
        .is_some_and(|b| crate::markdown::parser::post_pass::display_math_block_body(b).is_some());
    if is_diagram_reveal || is_verbatim {
        let last = block_own.saturating_sub(1);
        // A `$$...$$` block revealed with the math preview reserves a top
        // band for the rendered formula and paints the raw source below
        // it, so each source line r lands on rendered row `r + band`.  The
        // offset is 0 for mermaid, verbatim blocks, and a preview-off math
        // reveal, leaving those 1:1.
        let offset = parsed.latex_source_offset(block_idx);
        return (0..=n).map(|r| (r + offset).min(last)).collect();
    }

    // One rendered line per raw line, except that interior blanks and soft-break continuations
    // produce none.  A *separator* blank — one directly before a top-level item marker — does
    // render (loose-list spacing).  So a line's sub-row is the count of preceding raw lines that
    // render: every non-blank, plus separator blanks.
    let base_indent = raw_lines
        .first()
        .map(|l| l.len() - l.trim_start().len())
        .unwrap_or(0);
    let is_top_level_marker = |line: &str| {
        let indent = line.len() - line.trim_start().len();
        indent == base_indent && raw_list_marker_char_width(line).is_some()
    };
    // Backwards, because a blank renders only if its contiguous run ends at a top-level marker:
    // walking from the end answers that in O(1) per line, and a trailing run with no following
    // line correctly stays false.
    let mut renders = vec![false; n];
    let mut run_ends_at_marker = false;
    for i in (0..n).rev() {
        if raw_lines[i].trim().is_empty() {
            renders[i] = run_ends_at_marker;
        } else {
            renders[i] = true;
            run_ends_at_marker = is_top_level_marker(raw_lines[i]);
        }
    }

    let last = block_own.saturating_sub(1);
    let mut subs = Vec::with_capacity(n + 1);
    let mut rendered_before = 0usize;
    for renders_row in renders {
        subs.push(rendered_before.min(last));
        rendered_before += usize::from(renders_row);
    }
    subs.push(rendered_before.min(last));
    subs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Buffer;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    /// A non-canonical `(index, text)` pair must not poison the per-line `InlineColMap` cache;
    /// mouse hit-testing can derive one.
    #[test]
    fn inline_map_for_does_not_poison_cache_with_noncanonical_text() {
        let state = EditorState::new(Buffer::from_str("hello **world**\nsecond line\n"), theme());

        // Wrong text for line 1 — must be served by a local map, leaving the cache untouched.
        let wrong = state.inline_map_for(1, "hello **world**");
        assert_eq!(wrong.raw_len(), 15);

        let right = state.inline_map_for(1, "second line");
        assert_eq!(right.raw_len(), 11);

        // Out-of-bounds index must not panic.
        let oob = state.inline_map_for(99, "anything");
        assert_eq!(oob.raw_len(), 8);
    }

    /// Concatenated text of every non-blank rendered line (skips the phantom trailing row and
    /// any separator blanks), one string per row.
    fn line_texts(state: &EditorState) -> Vec<String> {
        state
            .parsed
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Reflow is on by default in both Preview and Rendered, off in Raw, and the master switch
    /// (`set_reflow`) disables it everywhere.  A mode switch that changes the effective reflow
    /// reparses (via `sync_reflow_for_mode`, as `prepare_viewport` calls it each frame).  Note
    /// `line_texts` reads the *rendered* spine (`parsed.lines`): the Rendered-mode raw reveal is a
    /// display overlay, so the rendered spine is the reflowed flow in both view modes.
    #[test]
    fn reflow_is_default_on_in_preview_and_rendered_off_in_raw() {
        let flow = vec!["one two three".to_string()];
        let split = vec!["one".to_string(), "two".to_string(), "three".to_string()];

        let mut state = EditorState::new(Buffer::from_str("one\ntwo\nthree\n"), theme());
        assert_eq!(state.mode, Mode::Preview);
        state.sync_reflow_for_mode();
        assert_eq!(line_texts(&state), flow, "Preview reflows by default");

        state.mode = Mode::Rendered;
        state.sync_reflow_for_mode();
        assert_eq!(line_texts(&state), flow, "Rendered reflows by default too");

        state.mode = Mode::Raw;
        state.sync_reflow_for_mode();
        // Raw uses buffer text, not `parsed.lines`, but the parse still tracks `want_reflow`: Raw
        // never reflows, so the rendered spine is per-line again.
        assert_eq!(line_texts(&state), split, "Raw never reflows");

        // The master switch turns it off in the view modes too.
        state.mode = Mode::Rendered;
        state.set_reflow(false);
        assert_eq!(
            line_texts(&state),
            split,
            "disabling reflow splits the flow back"
        );
    }

    /// When a reflowed paragraph reveals (grows) while the cursor stays on screen, the block's
    /// top and everything above it must stay put — only content below reflows.  The expansion is
    /// entirely below the block's first row, so `scroll` is left unchanged (the mermaid-style
    /// reveal), rather than dragging the document to re-pin the cursor's exact row.
    #[test]
    fn reflow_reveal_keeps_block_top_and_content_above_put() {
        let src = "a\n\nb\n\nc\n\none\ntwo\nthree\nfour\nfive\n\nafter\n";
        let mut state = EditorState::new(Buffer::from_str(src), theme());
        state.mode = Mode::Rendered;
        state.set_viewport_width(20);
        state.set_reflow(true);
        let (w, h) = (20usize, 20usize); // tall enough the expansion never pushes the cursor off
                                         // Cursor into the reflowed paragraph on its first line (a downward entry), the case that
                                         // keeps the reveal delay; the expansion then happens entirely below the cursor.
        let byte = state.buffer.contents().find("one").unwrap();
        state.cursor.offset = state.buffer.rope().byte_to_char(byte);
        state.update_cursor_block();

        // Frame 1: reveal delay pending (block still one flow row).  Park the flow row at the top
        // of the viewport so the expansion has ample headroom below.
        state.cursor_block_entered_at = Some(std::time::Instant::now());
        assert!(!state.cursor_block_revealed());
        state.scroll = super::rendered_cursor_visual_row(&state, w);
        state.anchor_reflow_reveal(w, h);
        let scroll_before = state.scroll;

        // Frame 2: the reveal fires (block expands to its raw lines) — cursor hasn't moved.
        state.cursor_block_entered_at = None;
        assert!(
            state.effective_rows(w).has_reveal(),
            "the block must now be reflow-revealed"
        );
        state.anchor_reflow_reveal(w, h);
        assert_eq!(
            state.scroll, scroll_before,
            "revealing a paragraph while the cursor stays visible must not move the document",
        );
    }

    /// Switching into Rendered mode with the cursor already resting in a reflowed paragraph must
    /// not yank the view: the first Rendered frame's `anchor_reflow_reveal` only re-checks cursor
    /// visibility, so the scroll the mode switch established (cursor already visible) survives.
    #[test]
    fn switching_into_rendered_mode_leaves_the_scroll_alone() {
        // A reflowed paragraph deep enough that the cursor sits mid-viewport, not at the top.
        let mut src = String::new();
        for i in 0..10 {
            src.push_str(&format!("filler {i}\n\n"));
        }
        src.push_str("alpha beta\ngamma delta\nepsilon zeta\n\ntail\n");
        let mut state = EditorState::new(Buffer::from_str(&src), theme());
        let (w, h) = (20usize, 10usize);
        state.set_viewport_width(w);
        let byte = state.buffer.contents().find("gamma").unwrap();
        state.cursor.offset = state.buffer.rope().byte_to_char(byte);
        state.update_cursor_block();
        state.cursor_block_entered_at = None; // resting → revealed once in Rendered

        // A few Preview frames (as `prepare_viewport` runs them), then scroll so the cursor is
        // visible mid-document.
        for _ in 0..3 {
            state.sync_reflow_for_mode();
            state.anchor_reflow_reveal(w, h);
        }
        state.mode = Mode::Preview;
        state.ensure_cursor_visible(h, w);

        // The mode-switch action flips to Rendered and fits the cursor.
        state.mode = Mode::Rendered;
        state.ensure_cursor_visible(h, w);
        let scroll_after_switch = state.scroll;

        // The next frame's per-frame anchor must leave that scroll alone.
        state.sync_reflow_for_mode();
        state.anchor_reflow_reveal(w, h);
        assert_eq!(
            state.scroll, scroll_after_switch,
            "entering Rendered mode must not move the view",
        );
    }

    /// With images declined, every image block collapses to its one-line placeholder — the same
    /// layout as the `Failed` branch of `ImageCache::reserved_rows`.
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

        let expanded = state.parsed.line_count();
        assert!(
            expanded >= 20,
            "expected ≥ 20 rendered lines with images expanded, got {expanded}",
        );

        state.images_enabled = false;
        state.refresh_parsed();

        // Two placeholders + the blank gap + the phantom final line = 4.
        assert_eq!(state.parsed.line_count(), 4);
    }

    /// Down through a word-wrapped line lands on the visually corresponding column.
    #[test]
    fn move_down_visual_honours_word_wrap_boundaries() {
        let text = "hello world foo bar baz quux wibble wobble";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        // Cursor on row 0 at visual col 3.
        state.cursor.offset = 3;
        state.cursor.preferred_col = 3;

        state.move_down_visual(20);

        let rows = crate::ui::line_render::visual_rows_of_str(text, 20);
        assert!(rows.len() >= 2, "expected wrap into at least 2 rows");
        let (row1_start, _, _) = rows[1];
        assert_eq!(state.cursor.offset, row1_start + 3);
    }

    /// Up from the first sub-line lands on the *last* sub-line of the previous line.
    #[test]
    fn move_up_visual_crosses_to_last_subline_of_previous_line() {
        let long = "aaaaa bbbbb ccccc ddddd eeeee fffff ggggg hhhhh";
        let text = format!("{}\nshort\n", long);
        let mut state = EditorState::new(Buffer::from_str(&text), theme());
        // Cursor on line 1 at col 3.
        let line1_start = state.buffer.line_to_char(1);
        state.cursor.offset = line1_start + 3;
        state.cursor.preferred_col = 3;

        state.move_up_visual(20);

        let rows = crate::ui::line_render::visual_rows_of_str(long, 20);
        let last = *rows.last().unwrap();
        let expected_raw_col = last.0 + 3;
        assert_eq!(
            state.cursor.offset,
            state.buffer.line_to_char(0) + expected_raw_col
        );
    }

    /// Up from a visual column wider than the sub-row above must clamp within that sub-row, not
    /// land on the wrap boundary — which renders at column 0 of the *current* row and leaves Up
    /// stuck there.
    #[test]
    fn move_up_visual_clamps_within_target_subrow_not_on_wrap_boundary() {
        // Width 40: row 0 is the 33-char prefix, rows 1+ are 40-char runs of 'a'.
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

        let (sub_idx, _) = crate::ui::line_render::sub_line_of_col(&rows, state.cursor.offset);
        assert_eq!(
            sub_idx, 0,
            "cursor at offset {} should be visually on row 0, not row {}",
            state.cursor.offset, sub_idx
        );

        // Up again must keep moving, not stall.
        let before = state.cursor.offset;
        state.move_up_visual(width);
        assert_ne!(
            state.cursor.offset, before,
            "Up from row 0 must not stall at offset {before}",
        );
    }

    /// Regression: clamping against `next_start` put the cursor on a space absorbed by the wrap —
    /// a char owning no cell, rendered at the next row's column 0 — so Up stalled there forever.
    /// `last_col_in_row` clamps against `end`, the row's real last char.
    #[test]
    fn move_up_visual_never_lands_on_a_space_absorbed_by_the_wrap() {
        // Width 10: every row fills exactly and swallows its trailing space.
        let text = "abcdefghij klmnopqrst uvwxyzabcd";
        let width = 10;
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        let rows = crate::ui::line_render::visual_rows_of_str(text, width);
        assert_eq!(rows[1], (11, 21, 22), "fixture stopped absorbing its space");

        // End of the last row, so `preferred_col` is a full row width.
        state.cursor.offset = text.chars().count();
        state.cursor.preferred_col = state.current_visual_col(width);

        state.move_up_visual(width);
        let (sub, _) = crate::ui::line_render::sub_line_of_col(&rows, state.cursor.offset);
        assert_eq!(
            sub, 1,
            "Up from row 2 should render on row 1, not row {sub} (offset {})",
            state.cursor.offset,
        );

        let before = state.cursor.offset;
        state.move_up_visual(width);
        assert_ne!(
            state.cursor.offset, before,
            "Up from row 1 must not stall at offset {before}",
        );
    }

    /// Regression: `preferred_col` stored without the hanging indent made Down off a list-item
    /// continuation row jump horizontally by the marker width.
    #[test]
    fn move_down_visual_preserves_screen_cell_across_indent_boundary() {
        let text = "- list item content that wraps to a second row\nplain paragraph here";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        let width = 30;
        // The offset of "screen cell 5 on row 1" depends on the wrap point, so derive it with the
        // same helpers the editor uses.
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
        // Screen cell 5 on row 1 → content cell 3.
        state.cursor.offset = row1_start + 3.min(row1_end - row1_start);
        state.cursor.preferred_col = state.current_visual_col(width);
        assert_eq!(state.cursor.preferred_col, 5);

        state.move_down_visual(width);

        // Line 1 has no indent, so screen cell 5 is char 5.
        let line1_start = state.buffer.line_to_char(1);
        assert_eq!(state.cursor.offset, line1_start + 5);
    }

    /// Regression: a click on a wrapped continuation row seeded `preferred_col` from the
    /// line-relative column, which is huge, so later vertical nav clamped every line to its end.
    #[test]
    fn click_on_wrapped_continuation_row_seeds_preferred_col_from_screen() {
        let text = "the quick brown fox jumps over the lazy dog one more time";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = crate::editor::Mode::Rendered;
        let viewport_w: usize = 20;

        // Char offset for "screen cell 4 on row 2".
        let rows = crate::ui::line_render::visual_rows_of_str(text, viewport_w);
        assert!(rows.len() >= 3);
        let (row2_start, row2_end, _) = rows[2];
        let target_offset = row2_start + 4.min(row2_end - row2_start);
        let click_action = crate::input::MouseAction::Click {
            col: 4,
            row: 2,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let mut anchor: Option<crate::editor::mouse_ops::DragTarget> = None;
        crate::editor::mouse_ops::apply(&mut state, click_action, &mut anchor, &[], 24, viewport_w);
        assert_eq!(state.cursor.offset, target_offset);
        // Must be the *screen* cell column, not ~row2_start + 4.
        assert_eq!(state.cursor.preferred_col, 4);
    }

    #[test]
    fn move_down_visual_on_list_item_without_offset_bug() {
        // The `- ` marker gives a 2-cell hanging indent, so screen cell 5 on the continuation row
        // is content cell 3.
        let text = "- hello world foo bar baz quux wibble";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
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
        assert_eq!(state.cursor.offset, row1_start + 3);
    }

    #[test]
    fn move_down_visual_in_raw_mode_wraps_flat() {
        // Raw paints wrapped rows flat, so navigation must too: the same position is content
        // cell 5 here, not `5 - 2` as in a rendered view.
        let text = "- hello world foo bar baz quux wibble";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = crate::editor::Mode::Raw;
        state.cursor.offset = 5;
        state.cursor.preferred_col = 5;

        state.move_down_visual(20);

        let rows = crate::ui::line_render::visual_rows_of_str(text, 20);
        assert!(rows.len() >= 2);
        let (row1_start, _, _) = rows[1];
        assert_eq!(state.cursor.offset, row1_start + 5);
    }

    #[test]
    fn current_visual_col_in_raw_mode_takes_no_hanging_indent() {
        // `preferred_col` is seeded from this, so an indent counted here would push every
        // vertical move off by the marker width.
        let text = "- hello world foo bar baz quux wibble";
        let mut state = EditorState::new(Buffer::from_str(text), theme());
        state.mode = crate::editor::Mode::Raw;
        let rows = crate::ui::line_render::visual_rows_of_str(text, 20);
        assert!(rows.len() >= 2);
        let (row1_start, row1_end, _) = rows[1];
        state.cursor.offset = row1_start + 3.min(row1_end - row1_start);
        // Flat wrap: content cell 3 *is* screen cell 3; a rendered view reads it as 5.
        assert_eq!(state.current_visual_col(20), 3);
        state.mode = crate::editor::Mode::Rendered;
        assert_eq!(state.current_visual_col(20), 5);
    }

    /// Regression: the bottom of the document was unreachable in Rendered mode when earlier lines
    /// wrapped, because the scroll bound counted logical lines rather than visual rows.
    #[test]
    fn scroll_to_bottom_accounts_for_wrapped_lines() {
        // The long paragraph occupies more than the 5-row viewport, so a wrap-ignoring bound
        // would push the final paragraph off the bottom.
        let long = "a".repeat(100);
        let src = format!("{long}\n\nfinal line.\n");
        let mut state = EditorState::new(Buffer::from_str(&src), theme());
        state.mode = crate::editor::Mode::Rendered;
        state.cursor.move_doc_end(&state.buffer);
        state.update_cursor_block();

        let vp_h = 5;
        let vp_w = 20;
        state.scroll_to_bottom(vp_h, vp_w);
        state.ensure_cursor_visible(vp_h, vp_w);

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

    /// Regression: the rendered → raw mode switch jumped the visible region.  The screen-row
    /// getter and setter must round-trip across it.
    #[test]
    fn cursor_screen_row_round_trips_across_mode_switch() {
        // Plain paragraphs map 1:1 between rendered and raw, so rows compare exactly.
        let mut src = String::new();
        for i in 0..20 {
            src.push_str(&format!("line {i}\n"));
        }
        let vp_w = 40;
        let mut state = EditorState::new(Buffer::from_str(&src), theme());
        state.mode = crate::editor::Mode::Rendered;
        state.cursor.offset = state.buffer.line_to_char(12);
        state.update_cursor_block();
        state.scroll = 9; // cursor at screen row 3 in Rendered mode.

        let row_before = state.cursor_screen_row(vp_w);
        assert_eq!(row_before, 3);

        // Force a mismatch before re-anchoring, so the helper is exercised rather than a happy
        // 1:1 alignment.
        state.mode = crate::editor::Mode::Raw;
        state.scroll = 0;

        state.set_scroll_for_cursor_screen_row(row_before, vp_w);
        let row_after = state.cursor_screen_row(vp_w);
        assert_eq!(row_after, row_before);
    }

    /// `set_theme` bumps `parsed_version` so dependent caches invalidate, and is a no-op for the
    /// same pointer.
    #[test]
    fn set_theme_swaps_reference_and_refreshes_parsed() {
        let original: &'static Theme = theme();
        let mut state = EditorState::new(Buffer::from_str("# heading\n"), original);
        let v_before = state.parsed_version;

        state.set_theme(original);
        assert_eq!(state.parsed_version, v_before);

        let other: &'static Theme = Box::leak(Box::new(Theme::default()));
        state.set_theme(other);
        assert!(std::ptr::eq(state.theme, other));
        assert_ne!(state.parsed_version, v_before);
    }

    /// A requested row past the cursor's distance from the document start clamps to scroll 0; the
    /// cursor lands lower but stays visible.
    #[test]
    fn set_scroll_for_cursor_screen_row_clamps_at_top() {
        let src = "line 0\nline 1\nline 2\n";
        let mut state = EditorState::new(Buffer::from_str(src), theme());
        state.mode = crate::editor::Mode::Raw;
        state.cursor.offset = state.buffer.line_to_char(1);
        state.scroll = 0;
        state.set_scroll_for_cursor_screen_row(50, 40);
        assert_eq!(state.scroll, 0);
        assert_eq!(state.cursor_screen_row(40), 1);
    }

    // ── Diff scroll-into-view ───────────────────────────────────────────

    /// Entry defers the scroll; resolving it brings an off-screen hunk up with a top margin.
    #[test]
    fn scroll_focused_hunk_into_view_scrolls_offscreen_hunk_up() {
        // 20 context lines precede the change, far below a 5-row viewport.
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
        assert!(state.pending_focus_scroll);
        assert_eq!(state.scroll, 0);

        // Focused row 20, top margin 3 → scroll 17.
        state.scroll_focused_hunk_into_view(5, 80);
        assert_eq!(state.scroll, 17);

        // Idempotent once visible.
        state.scroll_focused_hunk_into_view(5, 80);
        assert_eq!(state.scroll, 17);
    }

    // ── Rendered diff parse ──────────────────────────────────────────

    fn diff_state_for(old: &str, new: &str) -> EditorState {
        let diff = crate::diff::DiffState::new(old, new).unwrap();
        let mut state = EditorState::new(Buffer::from_str(old), theme());
        state.enter_diff_mode(diff);
        state
    }

    /// The initial build is deferred a frame so it happens against the diff-mode width, which
    /// reserves no line-number gutter.
    #[test]
    fn enter_diff_mode_defers_the_rendered_parse_by_one_frame() {
        let mut state = diff_state_for("# T\n\nbee\n", "# T\n\nBEE\n");
        assert!(state.diff_parse_dirty);
        assert!(state.diff.as_ref().unwrap().parsed_new.is_none());

        state.flush_diff_parse_if_dirty();
        assert!(!state.diff_parse_dirty);
        assert!(state.diff.as_ref().unwrap().parsed_new.is_some());
    }

    /// The build rides `refresh_parsed`'s tail, so a mid-review setting change rebuilds it too.
    #[test]
    fn a_mid_review_render_setting_change_rebuilds_the_diff_parse() {
        let mut state = diff_state_for("| a |\n|---|\n| 1 |\n", "| a |\n|---|\n| 2 |\n");
        state.flush_diff_parse_if_dirty();
        let before = state.diff.as_ref().unwrap().layout_version();
        state.set_row_striping(true);
        assert!(state.diff.as_ref().unwrap().parsed_new.is_some());
        assert!(state.diff.as_ref().unwrap().layout_version() > before);
    }

    /// Outside a review the tail call installs nothing.
    #[test]
    fn refresh_diff_parse_is_inert_without_a_review() {
        let mut state = EditorState::new(Buffer::from_str("hello\n"), theme());
        state.refresh_parsed();
        assert!(state.diff.is_none());
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

    /// Drive a `$$...$$` block's reveal with the cursor inside it and
    /// return the resolved [`ImageReveal`].
    fn latex_reveal_with(math_preview: bool) -> ImageReveal {
        let mut state = EditorState::new(Buffer::from_str("$$\nE = mc^2\n$$\n"), theme());
        state.mode = crate::editor::Mode::Rendered;
        state.math_preview = math_preview;
        state.refresh_parsed();
        let latex_idx = state
            .parsed
            .image_blocks
            .iter()
            .find(|i| matches!(i.source, Some(crate::diagram::DiagramSource::Latex(_))))
            .expect("latex block")
            .block_idx;
        // Park the cursor inside the block and let the reveal fire (skip the dwell delay).
        state.cursor.offset = "$$\n".chars().count() + 1;
        state.update_cursor_block();
        state.cursor_block_entered_at = None;
        assert_eq!(state.cursor_block_idx, Some(latex_idx));
        assert!(state.cursor_block_revealed());
        assert!(state.sync_image_reveal());
        state.image_reveal.clone().expect("reveal active")
    }

    /// With the math preview off, a `$$...$$` block reveals exactly its
    /// raw source lines — no image band — so it collapses to the source
    /// the user edits, the same affordance a mermaid fence gets.
    #[test]
    fn latex_reveal_without_preview_reserves_only_source_rows() {
        let reveal = latex_reveal_with(false);
        // Source: `$$` / body / `$$` → 3 rows, and no band.
        assert_eq!(reveal.rows, 3);
        assert_eq!(reveal.preview_rows, 0);
    }

    /// With the math preview on, the block reserves a live-preview band
    /// PLUS its source rows — the image not yet decoded → the
    /// `image_max_height` placeholder reservation (24), so the image
    /// doesn't resize when the reveal opens.
    #[test]
    fn latex_reveal_with_preview_reserves_a_band_above_the_source() {
        let reveal = latex_reveal_with(true);
        assert_eq!(reveal.rows, 3);
        assert_eq!(reveal.preview_rows, 24);
    }

    /// The flip: with the preview on, the revealed source rows are pushed
    /// below the formula band, so `math_source_offset` records the band
    /// height for `block_idx` and `sub_lines_in_block` maps source line 0
    /// onto rendered row `band` (not row 0).  With the preview off there is
    /// no band and the mapping stays 1:1 from the block's top.
    #[test]
    fn math_preview_offsets_source_rows_below_the_formula_band() {
        let with = latex_reveal_with(true);
        let mut state = EditorState::new(Buffer::from_str("$$\nE = mc^2\n$$\n"), theme());
        state.mode = crate::editor::Mode::Rendered;
        state.math_preview = true;
        state.refresh_parsed();
        let latex_idx = state
            .parsed
            .image_blocks
            .iter()
            .find(|i| matches!(i.source, Some(crate::diagram::DiagramSource::Latex(_))))
            .expect("latex block")
            .block_idx;
        state.cursor.offset = "$$\n".chars().count() + 1;
        state.update_cursor_block();
        state.cursor_block_entered_at = None;
        assert!(state.sync_image_reveal());
        assert_eq!(
            state.parsed.latex_source_offset(latex_idx),
            with.preview_rows,
            "the source offset equals the reserved preview band"
        );
        // Source line 0 (`$$`) now renders `band` rows down, not at the top.
        let raw = crate::ui::rendered_view::raw_block_cursor(
            &state,
            state.buffer.rope().char_to_byte(state.cursor.offset),
        );
        let raw_lines = crate::ui::rendered_view::raw_source_lines(&raw.source);
        let subs = sub_lines_in_block(
            &state.parsed,
            state.buffer.rope().char_to_byte(state.cursor.offset),
            latex_idx,
            state.parsed.block_own_line_count(latex_idx),
            &raw.source,
            &raw_lines,
        );
        assert_eq!(
            subs[0], with.preview_rows,
            "first source line sits below the band"
        );
    }

    /// A plain image reveals its single source line, never a preview band.
    #[test]
    fn plain_image_reveal_reserves_one_source_row() {
        let mut state = EditorState::new(Buffer::from_str("![logo](logo.png)\n"), theme());
        state.mode = crate::editor::Mode::Rendered;
        // Even with the preview on, a plain image gets no band — the band
        // is `$$...$$`-only.
        state.math_preview = true;
        state.refresh_parsed();
        state.cursor.offset = 2;
        state.cursor_block_entered_at = None;
        state.update_cursor_block();
        // The reveal timer re-arms on any intra-block line move for a
        // plain image (unlike diagram blocks, which keep their reveal
        // time); simulate the 120 ms jitter window having elapsed.
        state.cursor_block_entered_at = None;
        assert!(state.cursor_block_revealed());
        assert!(state.sync_image_reveal());
        let reveal = state.image_reveal.as_ref().expect("reveal active");
        assert_eq!(reveal.rows, 1, "single source line");
        assert_eq!(reveal.preview_rows, 0, "plain image: no preview band");
    }

    /// Turning "Show images" off must NOT shrink a rendered `$$...$$`
    /// formula (or a mermaid diagram): figures are gated by their own
    /// setting, so a promoted diagram URL keeps its reserved height
    /// regardless of the images toggle.  Regression for the images-off
    /// collapse that squeezed every formula to one row ("tiny math").
    /// The cursor stays outside the block so the reveal doesn't override
    /// the reservation.
    #[test]
    fn disabling_images_does_not_collapse_a_rendered_math_block() {
        let src = "Above.\n\n$$\nE = mc^2\n$$\n\nBelow.\n";
        let mut state = EditorState::new(Buffer::from_str(src), theme());
        state.mode = crate::editor::Mode::Rendered;
        state.diagrams_enabled = true;
        state.cursor.offset = 0; // outside the math block
        state.images_enabled = false;
        state.refresh_parsed();
        let latex_idx = state
            .parsed
            .image_blocks
            .iter()
            .find(|i| matches!(i.source, Some(crate::diagram::DiagramSource::Latex(_))))
            .expect("latex block promoted")
            .block_idx;
        // With images off, the (undecoded) formula falls back to the
        // `image_max_height` placeholder reservation — many rows — not the
        // single-row collapse a real image gets.
        assert!(
            state.parsed.block_own_line_count(latex_idx) > 1,
            "math block collapsed to {} row(s) with images off",
            state.parsed.block_own_line_count(latex_idx)
        );
    }

    /// Regression: while typing a `$$...$$` formula, an intermediate keystroke
    /// that leaves the LaTeX invalid must NOT collapse the live-preview band to
    /// one row.  The band holds its last resolved height, so the document
    /// doesn't reflow on every not-yet-valid intermediate state — the fix uses
    /// `aspect_rows` (which reports `None` for a failed decode) rather than
    /// `reserved_rows` (which collapses a failure to the single placeholder row).
    #[test]
    fn invalid_formula_holds_the_last_preview_band() {
        let mut state = EditorState::new(Buffer::from_str("$$\nE = mc^2\n$$\n"), theme());
        state.mode = crate::editor::Mode::Rendered;
        state.math_preview = true;
        state.refresh_parsed();
        let url = state
            .parsed
            .image_blocks
            .iter()
            .find(|i| matches!(i.source, Some(crate::diagram::DiagramSource::Latex(_))))
            .expect("latex block")
            .url
            .clone();
        // A valid formula has decoded: the band takes the image's fitted height.
        state
            .images
            .set_decoded(&url, image::DynamicImage::new_rgba8(100, 200));
        state.cursor.offset = "$$\n".chars().count() + 1;
        state.update_cursor_block();
        state.cursor_block_entered_at = None;
        assert!(state.sync_image_reveal());
        let band = state.image_reveal.as_ref().expect("reveal").preview_rows;
        assert!(
            band > 1,
            "a decoded formula reserves a multi-row band, got {band}"
        );

        // The current source's decode now fails — the state a half-typed,
        // not-yet-valid formula lands in.  The band must hold `band`, never
        // collapse to the single placeholder row `reserved_rows` would give.
        state.images.set_failed(&url, "invalid latex".into());
        state.sync_image_reveal();
        assert_eq!(
            state.image_reveal.as_ref().expect("reveal").preview_rows,
            band,
            "an invalid formula holds the last resolved band, not the 1-row placeholder",
        );
    }
}
