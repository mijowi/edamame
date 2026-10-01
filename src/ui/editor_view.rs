use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    widgets::{Block, StatefulWidget, Widget},
};

use crate::config::sections::MAX_WIDTH_COLS_MIN;
use crate::config::Theme;
use crate::editor::vim_ops::VisualKind;
use crate::editor::{EditorState, Mode};
use crate::terminal::Capabilities;

use super::{
    bottom_region::{BottomRegion, HintContent, HintLineState},
    diff_view::{DiffView, DiffViewState},
    image_view, link_view,
    preview::{PreviewState, PreviewView},
    raw_view::{RawView, RawViewState},
    rendered_view::{RenderedView, RenderedViewState},
    scrollbar::{Scrollbar, ScrollbarMetrics},
    status_bar::StatusBarState,
};

/// Top-level editor widget: lays out the document area and the bottom region (status line plus
/// optional hint line), then delegates to the per-mode sub-view.
pub struct EditorView<'a> {
    pub state: &'a mut EditorState,
    pub theme: &'a Theme,
    pub filename: &'a str,
    /// Passed to `RenderedView::show_table_buttons`; production passes
    /// `config.table.show_buttons && capabilities.mouse`.
    pub show_table_buttons: bool,
    /// An in-progress table drag; the painter overlays the destination separator with the
    /// `Theme::table_drop_indicator` highlight.
    pub table_drop_indicator: Option<crate::ui::table_view::DropIndicator>,
    /// Threaded through for the image overlay: without `image_protocol` (and `image_picker`),
    /// `image_view::paint_images` is a no-op and the `[Image: alt]` placeholder stays.
    pub capabilities: &'a Capabilities,
    /// Paint a 1-indexed line-number gutter at the left edge of the document area.
    pub show_line_numbers: bool,
    /// True within the post-scroll quiesce window, during which non-Kitty native protocols
    /// fall back to halfblocks to avoid a flickering re-encode every frame.
    pub is_scrolling: bool,
    /// What the hint line should display for this frame.
    pub hint: HintContent,
    /// Vim sub-mode badge when the vim handler is active; `None` leaves the status bar showing
    /// the rendering-mode badge.
    pub vim_mode_label: Option<&'a str>,
    /// The active vim Visual flavor, so `RenderedView` / `RawView` can widen the half-open
    /// `selection` for the highlight overlay only — inclusive charwise, whole lines in
    /// VisualLine.
    pub visual_kind: Option<VisualKind>,
    /// Block-cursor style for this frame, already resolved against the view mode and vim
    /// sub-mode, so the sub-views paint it directly.
    pub editor_cursor_style: Style,
    /// Cap the document area to `max_width_cols` and center it horizontally.
    pub max_width_enabled: bool,
    /// The cap in columns; floored at `MAX_WIDTH_COLS_MIN` and clamped to the available width.
    pub max_width_cols: usize,
    /// Hovering the scrollbar gutter or dragging the thumb; selects the bright thumb variant.
    pub scrollbar_active: bool,
}

/// Lay out the document area and, when needed, a scrollbar gutter inside `full`.  The gutter
/// sits at `full`'s right edge — *outside* any max-width clamp, so it always rides the terminal
/// boundary.
///
/// `total_for_width` returns the total wrapped row count at a candidate width.  The overflow
/// decision must be made at the *post-clamp* width: a narrower doc wraps to MORE rows, so
/// "fits at full width" does not imply "fits once the clamp narrows it."
pub fn layout_doc_with_scrollbar(
    full: Rect,
    max_width_enabled: bool,
    max_width_cols: usize,
    total_for_width: impl Fn(u16) -> usize,
) -> (Rect, Option<Rect>) {
    let doc_no_bar = clamp_doc_area_to_max_width(full, max_width_enabled, max_width_cols);
    let total = total_for_width(doc_no_bar.width);
    let needs_bar = total > full.height as usize && full.width >= 1 && full.height >= 1;
    if !needs_bar {
        return (doc_no_bar, None);
    }
    let bar = Rect {
        x: full.x + full.width - 1,
        y: full.y,
        width: 1,
        height: full.height,
    };
    let reduced = Rect {
        width: full.width - 1,
        ..full
    };
    let doc = clamp_doc_area_to_max_width(reduced, max_width_enabled, max_width_cols);
    (doc, Some(bar))
}

/// Center `area` horizontally, capping its width at `max(cols, MAX_WIDTH_COLS_MIN)`.  Only
/// `x` and `width` move; `area` is returned unchanged when the cap doesn't bite.
pub fn clamp_doc_area_to_max_width(area: Rect, enabled: bool, cols: usize) -> Rect {
    if !enabled {
        return area;
    }
    let cap = cols.max(MAX_WIDTH_COLS_MIN) as u16;
    if cap >= area.width {
        return area;
    }
    let x_off = area.x + (area.width - cap) / 2;
    Rect {
        x: x_off,
        y: area.y,
        width: cap,
        height: area.height,
    }
}

/// State for the `EditorView`.
#[derive(Default)]
pub struct EditorViewState {
    /// Preview mode only.
    pub preview: PreviewState,
    pub rendered: RenderedViewState,
    pub raw: RawViewState,
    /// Diff-mode view state.
    pub diff: DiffViewState,
    /// Published each render so the App's mouse layer can hit-test the gutter without
    /// re-deriving it.  `None` when the content fits and no gutter is drawn.
    pub scrollbar: Option<ScrollbarMetrics>,
    /// The hint line's cross-frame state (the vim command line's scroll).
    pub hint: HintLineState,
}

impl EditorViewState {
    /// Default-construct each per-mode state.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'a> StatefulWidget for EditorView<'a> {
    type State = EditorViewState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Document area + bottom region.
        let bottom_h = BottomRegion::height();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(bottom_h)])
            .split(area);

        let full_doc_area = chunks[0];
        let bar_area = chunks[1];

        // The "blank page" background.  Without it, cells the per-mode views never write keep
        // the terminal's default fg/bg, defeating themes with a concrete `default_bg`.  Later
        // renders patch over these cells, so colored spans still win.  Spans `full_doc_area`
        // so the max-width gutters carry the theme bg too.
        Block::default()
            .style(self.theme.normal)
            .render(full_doc_area, buf);

        // ── Line-number gutter reservation ──────────────────────
        // Reserved BEFORE the scrollbar / max-width layout so wrap and overflow decisions use
        // the correct content width.
        let mode = self.state.mode;
        let line_count = if self.show_line_numbers {
            match mode {
                // Buffer lines in every mode: the gutter numbers source lines, and this count
                // drives its *width*, which must follow the largest number displayed.
                Mode::Preview | Mode::Rendered | Mode::Raw => self.state.buffer.line_count(),
                // No gutter in diff mode: the interleaved old/new ropes share no consistent
                // numbering, and the per-hunk glyph carries the "where am I" affordance.
                Mode::Diff => 0,
            }
        } else {
            0
        };
        let (gutter_area, full_after_gutter) =
            super::gutter::split_gutter(full_doc_area, line_count);

        // The overflow decision is made at the post-clamp width; see `layout_doc_and_gutter`.
        let (doc_area, scrollbar_area) = layout_doc_with_scrollbar(
            full_after_gutter,
            self.max_width_enabled,
            self.max_width_cols,
            |w| self.state.total_visual_rows_for_mode(w as usize),
        );

        // ── Document area ─────────────────────────────────────────
        match mode {
            Mode::Preview => {
                // Mirror the canonical scroll / selection onto the preview view-state once per
                // frame, so the App need not know which fields each mode touches.
                state.preview.scroll = self.state.scroll;
                state.preview.selection = self.state.visual_selection;
                state.preview.selection_style = self.theme.selection;

                image_view::build_snapshots_cached(
                    self.state,
                    doc_area,
                    state.preview.scroll,
                    &mut state.preview.image_snapshots,
                    &mut state.preview.image_snapshots_key,
                );
                // Cached alongside the image snapshots so idle redraws skip the block walk.
                link_view::build_snapshots_cached(
                    self.state,
                    doc_area,
                    state.preview.scroll,
                    &mut state.preview.link_snapshots,
                    &mut state.preview.link_snapshots_key,
                );
                // Borrowed from `EditorState::parsed.lines` — no per-event clone.
                StatefulWidget::render(
                    PreviewView {
                        lines: &self.state.parsed.lines,
                        scroll: self.state.scroll,
                    },
                    doc_area,
                    buf,
                    &mut state.preview,
                );
            }
            Mode::Rendered => {
                StatefulWidget::render(
                    RenderedView {
                        state: &*self.state,
                        theme: self.theme,
                        show_table_buttons: self.show_table_buttons,
                        drop_indicator: self.table_drop_indicator,
                        visual_kind: self.visual_kind,
                        cursor_style: self.editor_cursor_style,
                    },
                    doc_area,
                    buf,
                    &mut state.rendered,
                );
            }
            Mode::Raw => {
                StatefulWidget::render(
                    RawView {
                        state: &*self.state,
                        theme: self.theme,
                        visual_kind: self.visual_kind,
                        cursor_style: self.editor_cursor_style,
                    },
                    doc_area,
                    buf,
                    &mut state.raw,
                );
            }
            Mode::Diff => {
                if let Some(diff) = self.state.diff.as_ref() {
                    // Same place Preview and Rendered build theirs; the paint pass reads them
                    // off `state.diff`.
                    image_view::build_diff_snapshots_cached(
                        diff,
                        doc_area,
                        self.state.scroll,
                        &mut state.diff.image_snapshots,
                        &mut state.diff.image_snapshots_key,
                    );
                    StatefulWidget::render(
                        DiffView {
                            diff,
                            theme: self.theme,
                            scroll: self.state.scroll,
                        },
                        doc_area,
                        buf,
                        &mut state.diff,
                    );
                }
            }
        }

        // ── Search-match + yank-flash overlays (Preview + Rendered) ─
        // A post-pass over the rendered cells: both views walk `parsed.lines` with the same
        // wrap, so one overlay walk serves both.  Raw mode paints these inline instead.
        if matches!(mode, Mode::Preview | Mode::Rendered) {
            super::rendered_view::paint_search_overlays(self.state, buf, doc_area, self.theme);
            super::rendered_view::paint_substitute_preview_overlays(
                self.state, buf, doc_area, self.theme,
            );
            super::rendered_view::paint_yank_flash(self.state, buf, doc_area, self.theme);
        }

        // ── Cursor re-stamp ───────────────────────────────────────
        // The overlays above wash every covered cell, the cursor's included, so re-apply the
        // cursor style.  `cursor_screen` is `Some` only for the rendered indicator path; the
        // raw-reveal / cell-overlay paths composite the cursor themselves.
        if mode == Mode::Rendered {
            if let Some((x, y)) = state.rendered.cursor_screen {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(self.editor_cursor_style);
                }
            }
        }

        // ── Line-number gutter paint ─────────────────────────────
        if let Some(ga) = gutter_area {
            let scroll = self.state.scroll;
            let content_width = doc_area.width as usize;
            let style = self.theme.line_number;
            match mode {
                Mode::Preview | Mode::Rendered => {
                    // One `EffectiveRows` for the whole gutter: rebuilding it per row would
                    // re-allocate the revealed block's source on every visible line.
                    let effective = self.state.effective_rows(content_width);
                    super::gutter::paint_gutter(
                        buf,
                        ga,
                        scroll,
                        line_count,
                        |row, _w| self.state.source_line_at_visual_row_with(&effective, row),
                        content_width,
                        style,
                    );
                }
                Mode::Raw => {
                    super::gutter::paint_gutter(
                        buf,
                        ga,
                        scroll,
                        line_count,
                        |row, w| {
                            let (line, sub) = self.state.raw_line_at_visual_row(row, w);
                            (sub == 0).then_some(line)
                        },
                        content_width,
                        style,
                    );
                }
                // Diff mode: no per-line numbering.
                Mode::Diff => {}
            }
        }

        // ── Image overlay (Preview + Rendered modes) ──────────────
        // Raw mode shows source, so nothing to paint.  The cursor's own image block is skipped
        // while raw-reveal is active so its `![alt](url)` line stays visible.  Diff review
        // paints its *clean* regions' images; a changed image block has no snapshot and shows
        // as source, and diff has no raw-reveal to suppress.
        if matches!(mode, Mode::Preview | Mode::Rendered | Mode::Diff) {
            let suppress = if mode == Mode::Rendered && self.state.cursor_block_revealed() {
                // A revealed block suppresses its image so the raw source
                // is all the user sees.  The exception is a `$$...$$` block
                // with the math preview on: it keeps its image as the live
                // preview painted in the band at the block's top, with the
                // source below (the snapshot rect already excludes the
                // source rows).
                let preview = self.state.math_preview;
                self.state
                    .cursor_block_idx
                    .filter(|&idx| !(preview && self.state.parsed.is_latex_block(idx)))
            } else {
                None
            };
            let snapshots: &[crate::ui::ImageLayoutSnapshot] = match mode {
                Mode::Preview => &state.preview.image_snapshots,
                Mode::Rendered => &state.rendered.image_snapshots,
                Mode::Diff => &state.diff.image_snapshots,
                _ => &[],
            };
            let ctx = image_view::PaintContext {
                area: doc_area,
                buf,
                images: &mut self.state.images,
                native_picker: self.capabilities.image_picker.as_ref(),
                halfblocks_picker: self.capabilities.halfblocks_picker.as_ref(),
                native_protocol: self.capabilities.image_protocol,
                is_scrolling: self.is_scrolling,
                modal_open: self.state.modal_open,
                suppress_block_idx: suppress,
                bg: self.theme.normal.bg.unwrap_or(ratatui::style::Color::Reset),
            };
            image_view::paint_images(snapshots, ctx);
        }

        // ── Scrollbar gutter ──────────────────────────────────────
        // Painted last so its glyphs win on the gutter cells, and published on
        // `state.scrollbar` for the App's mouse handler to hit-test.
        state.scrollbar = if let Some(area) = scrollbar_area {
            // Recomputed at the post-clamp width so the thumb tracks the wrapped content the
            // user sees; the scroll is clamped so mouse overshoot can't push it past the track.
            let total_post = self
                .state
                .total_visual_rows_for_mode(doc_area.width as usize);
            let visible = doc_area.height;
            let total = u16::try_from(total_post).unwrap_or(u16::MAX);
            let max_scroll = total.saturating_sub(visible);
            let position = u16::try_from(self.state.scroll)
                .unwrap_or(u16::MAX)
                .min(max_scroll);
            let metrics = ScrollbarMetrics {
                area,
                total,
                visible,
                position,
            };
            Scrollbar {
                metrics,
                theme: self.theme,
                active: self.scrollbar_active,
            }
            .render(area, buf);
            Some(metrics)
        } else {
            None
        };

        // ── Bottom region (hint line + status line) ───────────────
        let (cursor_line, cursor_col) = self.state.cursor.line_col(&self.state.buffer);
        // "N lines" counts source lines in every mode, agreeing with the cursor read-out and
        // the gutter; in diff mode, the *new-side* count.
        let line_count = self.state.buffer.line_count();
        // Must stay in `EditorState::scroll`'s units — wrapped visual rows, not rendered lines
        // — or the percentage saturates early on a document with wrapped lines.
        let scroll_total = self
            .state
            .total_visual_rows_for_mode(doc_area.width.max(1) as usize);
        // The document viewport, not the status bar's own row: the percentage answers "have I
        // seen this far", which is about the *last* visible row.
        let viewport_rows = doc_area.height as usize;
        // `EditorState::scroll` is canonical in every mode.
        let scroll = self.state.scroll;

        // Preview's cursor is hidden and frozen, so the breadcrumb anchors on the viewport
        // top; every other mode follows the cursor.
        let section_path = match mode {
            Mode::Preview => self.state.scroll_section_chain(),
            _ => self.state.cursor_section_chain(),
        };
        // The counter tracks merge decisions, so a read-only `git difftool` session — which
        // makes none — keeps the bare `DIFF` badge.
        let diff_progress = self
            .state
            .diff
            .as_ref()
            .filter(|d| !d.read_only)
            .map(|d| (d.resolved_count(), d.hunks.len()));
        let region = BottomRegion {
            status: StatusBarState {
                mode,
                filename: self.filename,
                line_count,
                scroll_total,
                viewport_rows,
                modified: self.state.dirty,
                scroll,
                cursor_line: Some(cursor_line + 1), // 1-indexed display
                cursor_col: Some(cursor_col + 1),
                section_path,
                diff_progress,
                vim_mode_label: self.vim_mode_label,
            },
            hint: self.hint,
            theme: self.theme,
        };
        StatefulWidget::render(region, bar_area, buf, &mut state.hint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn disabled_returns_input_unchanged() {
        let area = rect(0, 0, 200, 40);
        assert_eq!(clamp_doc_area_to_max_width(area, false, 80), area);
    }

    #[test]
    fn enabled_centres_when_term_wider_than_cap() {
        let area = rect(0, 0, 200, 40);
        let out = clamp_doc_area_to_max_width(area, true, 80);
        assert_eq!(out, rect(60, 0, 80, 40));
    }

    #[test]
    fn enabled_returns_input_when_term_narrower_than_cap() {
        let area = rect(0, 0, 60, 40);
        assert_eq!(clamp_doc_area_to_max_width(area, true, 80), area);
    }

    #[test]
    fn cap_is_floored_at_min() {
        let area = rect(0, 0, 200, 40);
        let out = clamp_doc_area_to_max_width(area, true, 5);
        assert_eq!(out.width, MAX_WIDTH_COLS_MIN as u16);
    }

    #[test]
    fn odd_remainder_biases_left() {
        // 100 - 81 = 19; left gutter = 9, right = 10.
        let area = rect(0, 0, 100, 10);
        let out = clamp_doc_area_to_max_width(area, true, 81);
        assert_eq!(out.x, 9);
        assert_eq!(out.width, 81);
    }

    #[test]
    fn preserves_y_and_height() {
        let area = rect(0, 5, 200, 30);
        let out = clamp_doc_area_to_max_width(area, true, 80);
        assert_eq!(out.y, 5);
        assert_eq!(out.height, 30);
    }
}
