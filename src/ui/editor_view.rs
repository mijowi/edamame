use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, StatefulWidget, Widget},
};

use crate::config::sections::MAX_WIDTH_COLS_MIN;
use crate::config::{StatusBarLayout, Theme};
use crate::editor::{EditorState, Mode};
use crate::terminal::Capabilities;

use super::{
    bottom_region::{BottomRegion, HintContent},
    diff_view::{DiffView, DiffViewState},
    image_view, link_view,
    preview::{PreviewState, PreviewView},
    raw_view::{RawView, RawViewState},
    rendered_view::{RenderedView, RenderedViewState},
    scrollbar::{Scrollbar, ScrollbarMetrics},
    status_bar::StatusBarState,
};

/// Top-level editor widget. Lays out the document area and the
/// bottom region (persistent status line plus optional
/// contextual hint line), then delegates rendering to the appropriate
/// sub-view based on mode.
pub struct EditorView<'a> {
    pub state: &'a mut EditorState,
    pub theme: &'a Theme,
    pub filename: &'a str,
    /// Passed through to `RenderedView::show_table_buttons`.  Callers in
    /// production pass `config.table.show_buttons && capabilities.mouse`;
    /// unit tests default to `false` so they don't paint the gutter glyphs
    /// over their assertions.
    pub show_table_buttons: bool,
    /// When `Some`, an in-progress table drag is active and
    /// the painter overlays the destination separator with the
    /// `Theme::table_drop_indicator` highlight after the handle glyphs.
    /// `None` when no relevant drag is in flight.
    pub table_drop_indicator: Option<crate::ui::table_view::DropIndicator>,
    /// Detected terminal capabilities — threaded through for the
    /// image overlay.  Without `capabilities.image_protocol`
    /// (and `image_picker`), `image_view::paint_images` is a no-op and
    /// the `[Image: alt]` placeholder stays visible.
    pub capabilities: &'a Capabilities,
    /// When true, a line-number gutter is painted at the left edge of
    /// the document area in all three modes.  Numbers are 1-indexed,
    /// right-aligned, and styled with `theme.line_number`.
    pub show_line_numbers: bool,
    /// True while the App has recorded a scroll change within the
    /// quiesce window.  During scroll, non-Kitty native protocols fall
    /// back to halfblocks rendering to avoid flickering re-encode on
    /// every frame.  Always `false` in tests that don't exercise scroll.
    pub is_scrolling: bool,
    /// Bottom-region layout (two-line or compact).
    pub status_bar_layout: StatusBarLayout,
    /// What the hint line should display for this frame.
    /// Ignored in compact mode.
    pub hint: HintContent,
    /// When true, the document area is capped to `max_width_cols` and
    /// centred horizontally; the gutters are filled with `theme.normal`.
    pub max_width_enabled: bool,
    /// Cap in columns when `max_width_enabled` is true.  Floored at
    /// `MAX_WIDTH_COLS_MIN` and clamped to the available width by
    /// `clamp_doc_area_to_max_width`.
    pub max_width_cols: usize,
    /// True while the user is hovering the scrollbar gutter or dragging
    /// the thumb.  Switches the rendered thumb to the bright variant.
    pub scrollbar_active: bool,
}

/// Lay out the document area and (when needed) a scrollbar gutter
/// inside `full`.  The gutter, when present, lives at `full`'s right
/// edge — *outside* any horizontal max-width clamp so the scrollbar
/// always rides the terminal boundary.
///
/// `total_for_width` is invoked with the candidate doc width to obtain
/// the total wrapped row count.  Wrap is width-dependent, so the
/// overflow decision must be made at the post-clamp width: a narrower
/// doc wraps to MORE rows than a wider one, so a "fits at full width"
/// answer doesn't imply "fits after the max-width clamp narrows it."
/// When overflow is detected the gutter is reserved at `full`'s right
/// edge and the doc is re-clamped against `full.width - 1` so the doc
/// never overlaps the gutter.
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

/// Centre `area` horizontally within itself, capping its width at
/// `max(cols, MAX_WIDTH_COLS_MIN)` when `enabled`.  When disabled, or
/// when the cap is at least `area.width`, returns `area` unchanged.  The
/// result has the same `y` and `height`; only `x` and `width` move.
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
    /// Used only in PreviewMode.
    pub preview: PreviewState,
    pub rendered: RenderedViewState,
    pub raw: RawViewState,
    /// Diff-mode view state.
    pub diff: DiffViewState,
    /// Layout published by the most recent render so the App's mouse
    /// layer can hit-test the scrollbar gutter without re-deriving the
    /// trio.  `None` when content fits the viewport (no gutter drawn).
    pub scrollbar: Option<ScrollbarMetrics>,
}

impl EditorViewState {
    /// Default-construct each per-mode state.  Preview no longer holds
    /// a copy of the rendered line list (it borrows from
    /// `EditorState::parsed.lines` at render time), so this constructor
    /// is parameterless.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'a> StatefulWidget for EditorView<'a> {
    type State = EditorViewState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Split into document area + bottom region.  Two rows
        // by default (hint line + status line), one in compact mode.
        let bottom_h = BottomRegion::height(self.status_bar_layout);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(bottom_h)])
            .split(area);

        let full_doc_area = chunks[0];
        let bar_area = chunks[1];

        // Paint the document area's "blank page" background first.
        // Without this, cells that the per-mode views never write to
        // (e.g. the trailing whitespace below the last paragraph) keep
        // the terminal's default fg/bg — defeating themes that supply
        // a concrete `default_text` / `default_bg`.  Subsequent line
        // and span renders patch onto these cells, so colored spans
        // (h1, code blocks, etc.) still win on the cells they touch.
        // The fill spans `full_doc_area` so the left/right gutters
        // exposed by `clamp_doc_area_to_max_width` carry the theme bg.
        Block::default()
            .style(self.theme.normal)
            .render(full_doc_area, buf);

        // ── Line-number gutter reservation ──────────────────────
        // When enabled, reserve a left gutter BEFORE the scrollbar /
        // max-width layout so word-wrap and scrollbar decisions use
        // the correct content width.
        let mode = self.state.mode;
        let line_count = if self.show_line_numbers {
            match mode {
                Mode::Preview | Mode::Rendered => self.state.parsed.line_count(),
                Mode::Raw => self.state.buffer.line_count(),
                // Diff mode doesn't render a line-number gutter — the
                // interleaved old/new ropes share no consistent line
                // numbering, and the per-hunk gutter glyph carries the
                // "where am I" affordance instead.
                Mode::Diff => 0,
            }
        } else {
            0
        };
        let (gutter_area, full_after_gutter) =
            super::gutter::split_gutter(full_doc_area, line_count);

        // Lay out doc area + optional scrollbar gutter.  The overflow
        // decision is made at the post-clamp doc width because narrower
        // widths wrap to MORE rows; deciding at the pre-clamp width
        // would miss overflow that only appears once the max-width
        // clamp narrows the doc.
        let (doc_area, scrollbar_area) = layout_doc_with_scrollbar(
            full_after_gutter,
            self.max_width_enabled,
            self.max_width_cols,
            |w| self.state.total_visual_rows_for_mode(w as usize),
        );

        // ── Document area ─────────────────────────────────────────
        match mode {
            Mode::Preview => {
                // Mirror the canonical scroll / selection from
                // `EditorState` onto the preview view-state once per
                // frame.  Was previously done in `App::run` after
                // every event, but now lives here so the App doesn't
                // need to know which view-state fields each mode
                // touches.
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
                // Link snapshots for preview-mode click
                // dispatch.  Cached alongside the image snapshots so
                // idle redraws skip the full block walk.
                link_view::build_snapshots_cached(
                    self.state,
                    doc_area,
                    state.preview.scroll,
                    &mut state.preview.link_snapshots,
                    &mut state.preview.link_snapshots_key,
                );
                // PreviewView borrows the rendered lines from
                // `EditorState::parsed.lines` — no per-event clone.
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
                    },
                    doc_area,
                    buf,
                    &mut state.raw,
                );
            }
            Mode::Diff => {
                if let Some(diff) = self.state.diff.as_ref() {
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

        // ── Line-number gutter paint ─────────────────────────────
        if let Some(ga) = gutter_area {
            let scroll = self.state.scroll;
            let content_width = doc_area.width as usize;
            let style = self.theme.line_number;
            match mode {
                Mode::Preview | Mode::Rendered => {
                    super::gutter::paint_gutter(
                        buf,
                        ga,
                        scroll,
                        line_count,
                        |row, w| self.state.rendered_line_at_visual_row(row, w),
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
                        |row, w| self.state.raw_line_at_visual_row(row, w),
                        content_width,
                        style,
                    );
                }
                // Diff mode: no per-line numbering (see above).
                Mode::Diff => {}
            }
        }

        // ── Image overlay (Preview + Rendered modes) ──────────────
        // Raw mode shows the plain Markdown source, so no image
        // rendering there.  Also skip the cursor's image block while
        // raw-reveal is active so the user can see their `![alt](url)`
        // source line instead of the image.
        if matches!(mode, Mode::Preview | Mode::Rendered) {
            let suppress = if mode == Mode::Rendered && self.state.cursor_block_revealed() {
                self.state.cursor_block_idx
            } else {
                None
            };
            let snapshots: &[crate::ui::ImageLayoutSnapshot] = match mode {
                Mode::Preview => &state.preview.image_snapshots,
                Mode::Rendered => &state.rendered.image_snapshots,
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
        // Painted last over the document so its glyphs win cleanly on
        // the gutter cells.  Published on `state.scrollbar` so the
        // App's mouse handler can hit-test the gutter on subsequent
        // events.
        state.scrollbar = if let Some(area) = scrollbar_area {
            // Recompute total at the (possibly narrower) post-clamp
            // doc width so the thumb position lines up with the
            // wrapped content the user actually sees.  Clamp the
            // displayed scroll to `[0, total - visible]` so mouse
            // overshoot doesn't push the thumb past the track end.
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
        let line_count = match mode {
            Mode::Preview | Mode::Rendered => self.state.parsed.line_count(),
            Mode::Raw => self.state.buffer.line_count(),
            // Status-bar "n / N" line indicator in diff mode shows the
            // *new-side* line count — the most useful frame of
            // reference when reviewing.
            Mode::Diff => self.state.buffer.line_count(),
        };
        // The canonical scroll is `EditorState::scroll` for every mode
        // (Preview's view-state mirror is updated above before render).
        let scroll = self.state.scroll;
        let selection_size = self.state.selection_size();

        let section_path = self.state.cursor_section_chain();
        let diff_progress = self
            .state
            .diff
            .as_ref()
            .map(|d| (d.resolved_count(), d.hunks.len()));
        let region = BottomRegion {
            status: StatusBarState {
                mode,
                filename: self.filename,
                line_count,
                modified: self.state.dirty,
                scroll,
                cursor_line: Some(cursor_line + 1), // 1-indexed display
                cursor_col: Some(cursor_col + 1),
                selection_size,
                section_path,
                diff_progress,
            },
            hint: self.hint,
            layout: self.status_bar_layout,
            theme: self.theme,
        };
        Widget::render(region, bar_area, buf);
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
