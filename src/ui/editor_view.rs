use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    text::Line,
    widgets::{StatefulWidget, Widget},
};

use crate::config::Theme;
use crate::editor::{EditorState, Mode};
use crate::terminal::Capabilities;

use super::{
    image_view,
    preview::{PreviewState, PreviewView},
    raw_view::{RawView, RawViewState},
    rendered_view::{RenderedView, RenderedViewState},
    status_bar::{StatusBar, StatusBarState},
};

/// Top-level editor widget. Lays out the document area and the status bar,
/// then delegates rendering to the appropriate sub-view based on mode.
pub struct EditorView<'a> {
    pub state: &'a mut EditorState,
    pub theme: &'a Theme,
    pub filename: &'a str,
    /// Passed through to `RenderedView::show_table_handles`.  Callers in
    /// production pass `config.table.show_drag_handles && capabilities.mouse`;
    /// unit tests default to `false` so they don't paint the gutter glyphs
    /// over their assertions.
    pub show_table_handles: bool,
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
}

/// State for the `EditorView`.
pub struct EditorViewState {
    /// Used only in PreviewMode.
    pub preview: PreviewState,
    pub rendered: RenderedViewState,
    pub raw: RawViewState,
}

impl EditorViewState {
    /// Create with the given initial rendered lines (for preview mode seeding).
    pub fn new(lines: Vec<Line<'static>>) -> Self {
        Self {
            preview: PreviewState::new(lines),
            rendered: RenderedViewState::default(),
            raw: RawViewState::default(),
        }
    }

    // ── Forwarded scroll helpers for Preview mode ─────────────────

    pub fn scroll_down(&mut self, n: usize, viewport_height: usize) {
        self.preview.scroll_down(n, viewport_height);
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.preview.scroll_up(n);
    }

    pub fn scroll_to_top(&mut self) {
        self.preview.scroll_to_top();
    }

    pub fn scroll_to_bottom(&mut self, viewport_height: usize) {
        self.preview.scroll_to_bottom(viewport_height);
    }

    pub fn total_lines(&self) -> usize {
        self.preview.total_lines()
    }

    pub fn scroll(&self) -> usize {
        self.preview.scroll
    }
}

impl<'a> StatefulWidget for EditorView<'a> {
    type State = EditorViewState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Split into document area + 1-row status bar.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);

        let doc_area = chunks[0];
        let bar_area = chunks[1];

        let _viewport_height = doc_area.height as usize;

        // ── Document area ─────────────────────────────────────────
        let mode = self.state.mode;
        match mode {
            Mode::Preview => {
                // PreviewView renders from its own `state.preview.lines`
                // vector (a snapshot of the parsed doc captured at App
                // startup / mode switch), so we have to populate its
                // image snapshots from here using the canonical
                // `EditorState::parsed` block list.
                image_view::build_snapshots_cached(
                    self.state,
                    doc_area,
                    state.preview.scroll,
                    &mut state.preview.image_snapshots,
                    &mut state.preview.image_snapshots_key,
                );
                // Keep preview state lines in sync with editor state.
                // In preview mode the editor state scroll is the canonical scroll.
                StatefulWidget::render(PreviewView, doc_area, buf, &mut state.preview);
            }
            Mode::Rendered => {
                StatefulWidget::render(
                    RenderedView {
                        state: &*self.state,
                        theme: self.theme,
                        show_table_handles: self.show_table_handles,
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

        // ── Status bar ────────────────────────────────────────────
        let (cursor_line, cursor_col) = self.state.cursor.line_col(&self.state.buffer);
        let line_count = match mode {
            Mode::Preview => state.preview.total_lines(),
            Mode::Raw => self.state.buffer.line_count(),
            Mode::Rendered => self.state.parsed.line_count(),
        };
        let scroll = match mode {
            Mode::Preview => state.preview.scroll,
            _ => self.state.scroll,
        };

        let bar = StatusBar {
            state: StatusBarState {
                mode,
                filename: self.filename,
                line_count,
                modified: self.state.dirty,
                scroll,
                cursor_line: Some(cursor_line + 1), // 1-indexed display
                cursor_col: Some(cursor_col + 1),
            },
            theme: self.theme,
        };
        Widget::render(bar, bar_area, buf);
    }
}
