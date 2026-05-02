use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, StatefulWidget, Widget},
};

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

        let doc_area = chunks[0];
        let bar_area = chunks[1];

        let _viewport_height = doc_area.height as usize;

        // Paint the document area's "blank page" background first.
        // Without this, cells that the per-mode views never write to
        // (e.g. the trailing whitespace below the last paragraph) keep
        // the terminal's default fg/bg — defeating themes that supply
        // a concrete `default_text` / `default_bg`.  Subsequent line
        // and span renders patch onto these cells, so coloured spans
        // (h1, code blocks, etc.) still win on the cells they touch.
        Block::default()
            .style(self.theme.normal)
            .render(doc_area, buf);

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
