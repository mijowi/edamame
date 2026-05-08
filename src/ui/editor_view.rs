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
    image_view, link_view,
    preview::{PreviewState, PreviewView},
    raw_view::{RawView, RawViewState},
    rendered_view::{RenderedView, RenderedViewState},
    status_bar::StatusBarState,
};

/// Top-level editor widget. Lays out the document area and the
/// Phase 9 bottom region (persistent status line plus optional
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
    /// Phase 13 — when `Some`, an in-progress table drag is active and
    /// the painter overlays the destination separator with the
    /// `Theme::table_drop_indicator` highlight after the handle glyphs.
    /// `None` when no relevant drag is in flight.
    pub table_drop_indicator: Option<crate::ui::table_view::DropIndicator>,
    /// Detected terminal capabilities — threaded through for the
    /// Phase 7 image overlay.  Without `capabilities.image_protocol`
    /// (and `image_picker`), `image_view::paint_images` is a no-op and
    /// the `[Image: alt]` placeholder stays visible.
    pub capabilities: &'a Capabilities,
    /// True while the App has recorded a scroll change within the
    /// quiesce window.  During scroll, non-Kitty native protocols fall
    /// back to halfblocks rendering to avoid flickering re-encode on
    /// every frame.  Always `false` in tests that don't exercise scroll.
    pub is_scrolling: bool,
    /// Phase 9 — bottom-region layout (two-line or compact).
    pub status_bar_layout: StatusBarLayout,
    /// Phase 9 — what the hint line should display for this frame.
    /// Ignored in compact mode.
    pub hint: HintContent,
    /// When true, the document area is capped to `max_width_cols` and
    /// centred horizontally; the gutters are filled with `theme.normal`.
    pub max_width_enabled: bool,
    /// Cap in columns when `max_width_enabled` is true.  Floored at
    /// `MAX_WIDTH_COLS_MIN` and clamped to the available width by
    /// `clamp_doc_area_to_max_width`.
    pub max_width_cols: usize,
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
        // Split into document area + bottom region.  Phase 9: two rows
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
        // and span renders patch onto these cells, so coloured spans
        // (h1, code blocks, etc.) still win on the cells they touch.
        // The fill spans `full_doc_area` so the left/right gutters
        // exposed by `clamp_doc_area_to_max_width` carry the theme bg.
        Block::default()
            .style(self.theme.normal)
            .render(full_doc_area, buf);

        let doc_area =
            clamp_doc_area_to_max_width(full_doc_area, self.max_width_enabled, self.max_width_cols);

        // ── Document area ─────────────────────────────────────────
        let mode = self.state.mode;
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
                // Phase 8 — link snapshots for preview-mode click
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
                suppress_block_idx: suppress,
                bg: self.theme.normal.bg.unwrap_or(ratatui::style::Color::Reset),
            };
            image_view::paint_images(snapshots, ctx);
        }

        // ── Bottom region (hint line + status line) ───────────────
        let (cursor_line, cursor_col) = self.state.cursor.line_col(&self.state.buffer);
        let line_count = match mode {
            Mode::Preview | Mode::Rendered => self.state.parsed.line_count(),
            Mode::Raw => self.state.buffer.line_count(),
        };
        // The canonical scroll is `EditorState::scroll` for every mode
        // (Preview's view-state mirror is updated above before render).
        let scroll = self.state.scroll;
        let selection_size = self.state.selection_size();

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
