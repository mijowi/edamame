//! Phase 8 link-following and back/forward navigation extracted from
//! `app.rs` in Step 2 of `refactor-app.md`.
//!
//! Owns:
//! - The [`NavEntry`] back/forward stack record.
//! - The [`App`] methods that consult `nav_back` / `nav_forward`.
//! - Buffer-replacement helper [`App::load_file_into_editor`].
//! - Link resolution and the [`App::follow_link`] dispatcher.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::app::modal;
use crate::document::Buffer;
use crate::editor::link::LinkTarget;
use crate::editor::{mouse_ops, EditorState, Mode};
use crate::ui::EditorViewState;

use super::App;

/// One entry on [`App::nav_back`] / [`App::nav_forward`] — records
/// enough state to restore the exact scroll / cursor / mode we were in
/// when we left a particular document.
#[derive(Debug, Clone)]
pub(super) struct NavEntry {
    pub(super) path: PathBuf,
    pub(super) scroll: usize,
    pub(super) cursor_offset: usize,
    pub(super) mode: Mode,
}

/// True when `path` ends in `.md` / `.markdown` (case-insensitive).
/// Used by `App::follow_link` to decide whether a LocalFile link
/// should be opened in-editor or handed off to the OS default app.
pub(super) fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_ascii_lowercase();
            lower == "md" || lower == "markdown"
        })
        .unwrap_or(false)
}

impl App {
    /// Resolve the link under the keyboard cursor by scanning the
    /// current raw line for `[text](url)` syntax and classifying the
    /// URL.  Mirrors `mouse_ops::link_at_offset` — keyboard and mouse
    /// paths use the same fallback scan so they behave identically
    /// regardless of which input device fired `FollowLink`.
    pub(super) fn resolve_link_at_cursor(&self) -> Option<LinkTarget> {
        let cursor_byte = self
            .editor
            .buffer
            .rope()
            .char_to_byte(self.editor.cursor.offset);
        let source = self.editor.buffer.contents();
        let url = mouse_ops::link_at_offset(&source, cursor_byte)?;
        let base_dir = self.file_path.as_deref().and_then(|p| p.parent());
        Some(LinkTarget::parse(&url, base_dir))
    }

    /// Central dispatch: follow `target` based on its classified kind.
    /// Returns without doing anything when `target` is an empty anchor
    /// (`url == "#"`), an unknown heading slug, or when the dirty
    /// guard intercepts the navigation.
    pub(super) fn follow_link(&mut self, target: LinkTarget, doc_height: usize, doc_width: usize) {
        match target {
            LinkTarget::Url(url) => {
                self.spawn_open_worker(url);
            }
            LinkTarget::Anchor(slug) => {
                self.scroll_to_heading(&slug, doc_height, doc_width);
            }
            LinkTarget::LocalFile(path) => {
                if is_markdown_path(&path) {
                    if self.editor.dirty {
                        self.open_dirty_guard(path);
                    } else {
                        self.navigate_to_file(path);
                    }
                } else {
                    // Non-Markdown local file — defer to the OS handler
                    // via the same worker path as remote URLs.
                    let url = path.to_string_lossy().into_owned();
                    self.spawn_open_worker(url);
                }
            }
        }
    }

    /// Scroll so `slug`'s heading sits at the top of the viewport.
    /// No-op if the slug isn't in the current document's anchor table.
    /// In editing modes (Rendered / Raw) also moves the cursor onto
    /// the heading so subsequent navigation feels anchored.
    pub(super) fn scroll_to_heading(
        &mut self,
        slug: &str,
        doc_height: usize,
        doc_width: usize,
    ) {
        let Some(&line_idx) = self.editor.parsed.heading_anchors.get(slug) else {
            return;
        };
        self.editor.scroll = self.editor.parsed.visual_rows_before(line_idx, doc_width);
        if self.editor.mode != Mode::Preview {
            // Move cursor to the heading's first byte so subsequent
            // keyboard edits operate on that block.
            if let Some(byte) = self
                .editor
                .parsed
                .source_map
                .original_byte_for_rendered_line(line_idx)
            {
                let char_offset = self.editor.buffer.rope().byte_to_char(byte);
                self.editor.cursor.offset = char_offset.min(self.editor.buffer.len_chars());
                self.editor.update_cursor_block();
                self.editor.ensure_cursor_visible(doc_height, doc_width);
            }
        }
        self.mark_scrolling();
    }

    /// Push the current (file, scroll, cursor, mode) onto `nav_back`
    /// and load `path` into the editor.  Clears `nav_forward` to match
    /// browser semantics.
    pub(super) fn navigate_to_file(&mut self, path: PathBuf) {
        let entry = self.current_nav_entry();
        if let Err(err) = self.load_file_into_editor(path.clone()) {
            tracing::warn!(target: "link", path = %path.display(), error = %err, "failed to load linked file");
            return;
        }
        if let Some(e) = entry {
            self.nav_back.push(e);
        }
        self.nav_forward.clear();
    }

    /// Replace the editor's buffer with the contents of `path` and
    /// refresh dependent caches.  Does NOT touch the nav stack — the
    /// caller decides whether the transition should record history.
    pub(super) fn load_file_into_editor(&mut self, path: PathBuf) -> Result<()> {
        let buffer = Buffer::load_file(&path)?;
        let mut new_editor = EditorState::new_with_image_config(
            buffer,
            self.theme,
            self.config.editor.preserve_blank_lines,
            self.config.editor.visual_line_nav,
            self.config.images.max_height,
            self.config.images.max_width,
            self.capabilities
                .image_picker
                .as_ref()
                .map(|p| p.font_size())
                .unwrap_or((10, 20)),
        );
        new_editor.tab_width = self.config.editor.tab_width;
        // Preserve the current declined state across file loads: a
        // session-level `No`, a persisted `Never`, or anything else that
        // zeroed `images_enabled` on the previous editor stays in
        // effect for the new one.
        if !self.images_layout_enabled() {
            new_editor.images_enabled = false;
            new_editor.set_row_striping(self.config.table.row_striping);
            new_editor.refresh_parsed();
        } else {
            new_editor.set_row_striping(self.config.table.row_striping);
        }
        self.editor = new_editor;
        // Image cache is owned by `EditorState`, so swapping to a new
        // editor resets it — image URLs on the new doc are resolved
        // against the new base directory on the next draw.
        self.file_path = Some(path);
        self.view_state = EditorViewState::new();
        self.images_dirty = true;
        Ok(())
    }

    /// Snapshot the editor's current nav state.  Returns `None` when
    /// there's no associated file path — we can't push an entry we
    /// can't restore.
    pub(super) fn current_nav_entry(&self) -> Option<NavEntry> {
        self.file_path.clone().map(|path| NavEntry {
            path,
            scroll: self.editor.scroll,
            cursor_offset: self.editor.cursor.offset,
            mode: self.editor.mode,
        })
    }

    /// Pop `nav_back` (if any), push the current state onto
    /// `nav_forward`, and load the popped file.  Respects the dirty
    /// guard the same way forward navigation does.
    pub(super) fn navigate_back(&mut self, doc_height: usize, doc_width: usize) {
        let Some(dest) = self.nav_back.pop() else {
            return;
        };
        if self.editor.dirty {
            // Dirty guard path: restore the popped entry onto the back
            // stack (so Cancel is a true no-op) and prompt the user.
            let target = dest.path.clone();
            self.nav_back.push(dest);
            self.open_dirty_guard(target);
            return;
        }
        self.navigate_to_entry(dest, doc_height, doc_width, /*forward=*/ false);
    }

    pub(super) fn navigate_forward(&mut self, doc_height: usize, doc_width: usize) {
        let Some(dest) = self.nav_forward.pop() else {
            return;
        };
        if self.editor.dirty {
            let target = dest.path.clone();
            self.nav_forward.push(dest);
            self.open_dirty_guard(target);
            return;
        }
        self.navigate_to_entry(dest, doc_height, doc_width, /*forward=*/ true);
    }

    /// Shared back/forward dispatch: push the current state onto the
    /// opposite stack, then load `dest` and restore the recorded
    /// scroll/cursor/mode.
    fn navigate_to_entry(
        &mut self,
        dest: NavEntry,
        doc_height: usize,
        doc_width: usize,
        forward: bool,
    ) {
        let current = self.current_nav_entry();
        if let Err(err) = self.load_file_into_editor(dest.path.clone()) {
            tracing::warn!(target: "link", path = %dest.path.display(), error = %err, "nav load failed");
            return;
        }
        if let Some(e) = current {
            if forward {
                self.nav_back.push(e);
            } else {
                self.nav_forward.push(e);
            }
        }
        // Restore the saved scroll / cursor / mode on the loaded doc.
        self.editor.scroll = dest.scroll.min(
            self.editor
                .total_visual_rows_for_mode(doc_width)
                .saturating_sub(1),
        );
        self.editor.cursor.offset = dest.cursor_offset.min(self.editor.buffer.len_chars());
        self.editor.mode = dest.mode;
        self.editor.update_cursor_block();
        self.editor.ensure_cursor_visible(doc_height, doc_width);
    }

    /// Show the three-button `Save / Discard / Cancel` modal for the
    /// pending link-follow destination.  Caller supplies the resolved
    /// destination path.
    pub(super) fn open_dirty_guard(&mut self, pending: PathBuf) {
        let display = self
            .file_path
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "current file".to_owned());
        self.modal_stack
            .push(Box::new(modal::DirtyGuardModal::new(&display, pending)));
    }
}
