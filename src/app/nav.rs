//! Link-following and back/forward navigation
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

/// Where a [`NavEntry`] restores to.
///
/// Back/forward navigation spans two kinds of destination under the same
/// `NavigateBack` / `NavigateForward` actions:
///   * [`NavDest::File`] — a (possibly different) file to load into the
///     editor before restoring scroll/cursor/mode.  Cross-file restores
///     pass through the dirty guard.
///   * [`NavDest::InDocument`] — a position within the *currently loaded*
///     document (footnote-reference follow, `#heading` anchor jump, and
///     the footnote back-link).  No reload, no dirty guard.  Carries no
///     path, so it records history even for an unsaved `[No file]` buffer.
///     `footnote` names the footnote whose reference was followed to leave
///     this position (`None` for heading-anchor jumps), so a definition's
///     back-link can tell "I arrived by following *this* footnote" from an
///     unrelated in-document jump sitting on the stack.
#[derive(Debug, Clone)]
pub(super) enum NavDest {
    File(PathBuf),
    InDocument { footnote: Option<String> },
}

/// One entry on [`App::nav_back`] / [`App::nav_forward`] — records
/// enough state to restore the exact scroll / cursor / mode we were in
/// when we left a particular position.
#[derive(Debug, Clone)]
pub(super) struct NavEntry {
    pub(super) dest: NavDest,
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
        // Footnote syntax is unambiguous (`[^…]` with no `(url)`), so check
        // it before the `[text](url)` scan.
        if let Some(target) = mouse_ops::footnote_at_offset(&source, cursor_byte) {
            return Some(target);
        }
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
            LinkTarget::Footnote(label) => {
                self.follow_footnote_reference(&label, doc_height, doc_width);
            }
            LinkTarget::FootnoteBack(label) => {
                self.follow_footnote_back_link(&label, doc_height, doc_width);
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
    pub(super) fn scroll_to_heading(&mut self, slug: &str, doc_height: usize, doc_width: usize) {
        let Some(&line_idx) = self.editor.parsed.heading_anchors.get(slug) else {
            return;
        };
        // Record where we jumped from so `NavigateBack` returns to the
        // link — heading-anchor jumps are now back-navigable, unified with
        // footnote navigation under the same in-document nav model.  Tagged
        // `None`: a heading jump isn't a footnote follow.
        self.record_in_doc_jump(None);
        self.scroll_to_rendered_line(line_idx, doc_height, doc_width);
    }

    /// Scroll so rendered line `line_idx` sits at the viewport top; in
    /// editing modes also move the cursor onto that line's first source
    /// byte so subsequent edits operate there.  Does NOT record nav
    /// history — callers push the origin first.
    fn scroll_to_rendered_line(&mut self, line_idx: usize, doc_height: usize, doc_width: usize) {
        self.editor.scroll = self.editor.parsed.visual_rows_before(line_idx, doc_width);
        if self.editor.mode != Mode::Preview {
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

    /// Follow a footnote *reference* `[^label]` to its definition,
    /// recording the origin so the definition's back-link (or
    /// `NavigateBack`) returns here.  No-op when the label has no
    /// definition in the current document.
    pub(super) fn follow_footnote_reference(
        &mut self,
        label: &str,
        doc_height: usize,
        doc_width: usize,
    ) {
        let Some(&line_idx) = self.editor.parsed.footnote_anchors.get(label) else {
            return;
        };
        // Tag the origin with this label so the definition's back-link can
        // recognize it as *this* footnote's follow.
        self.record_in_doc_jump(Some(label.to_string()));
        self.scroll_to_rendered_line(line_idx, doc_height, doc_width);
    }

    /// Follow a footnote definition's back-link.  When the reader arrived
    /// by following the reference, the in-document back entry returns them
    /// to that exact spot; when they scrolled to the definition directly
    /// (no in-document origin recorded), jump to the footnote's first
    /// reference instead.
    pub(super) fn follow_footnote_back_link(
        &mut self,
        label: &str,
        doc_height: usize,
        doc_width: usize,
    ) {
        // Use the nav stack only when the top entry records following
        // *this* footnote — otherwise an unrelated heading jump (or another
        // footnote) on the stack would warp the reader to the wrong place.
        let top_is_this_footnote = matches!(
            self.nav_back.last().map(|e| &e.dest),
            Some(NavDest::InDocument { footnote: Some(l) }) if l == label
        );
        if top_is_this_footnote {
            self.navigate_back(doc_height, doc_width);
            return;
        }
        // Reached directly (or via a different jump): go to the first
        // reference of this footnote.
        if let Some(line_idx) = self.first_reference_line(label) {
            self.record_in_doc_jump(None);
            self.scroll_to_rendered_line(line_idx, doc_height, doc_width);
        }
    }

    /// Rendered-line index of the first `[^label]` *reference* (not the
    /// `[^label]:` definition) in the current buffer, if any.
    fn first_reference_line(&self, label: &str) -> Option<usize> {
        let source = self.editor.buffer.contents();
        let needle = format!("[^{label}]");
        let mut from = 0;
        while let Some(rel) = source[from..].find(&needle) {
            let at = from + rel;
            let after = at + needle.len();
            if source.as_bytes().get(after) != Some(&b':') {
                return Some(
                    self.editor
                        .parsed
                        .source_map
                        .rendered_lines_for_byte(at)
                        .start,
                );
            }
            from = after;
        }
        None
    }

    /// Push the current (file, scroll, cursor, mode) onto `nav_back`
    /// and load `path` into the editor.  Clears `nav_forward` to match
    /// browser semantics.
    pub(super) fn navigate_to_file(&mut self, path: PathBuf) {
        let entry = self.current_file_entry();
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
        // Stamp the watcher's own-write filter from the bytes we
        // just read so the inotify event that some backends
        // synthesize on `open(2)` is suppressed.
        self.set_disk_hash(buffer.contents().as_bytes());
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
                .map(|p| {
                    // ratatui-image 11 returns a `FontSize` struct; we
                    // carry font size as a `(width, height)` tuple.
                    let fs = p.font_size();
                    (fs.width, fs.height)
                })
                .unwrap_or((10, 20)),
        );
        // Preserve the current declined state across file loads: a
        // session-level `No`, a persisted `Never`, or anything else that
        // zeroed `images_enabled` on the previous editor stays in
        // effect for the new one.
        let images_off = !self.images_layout_enabled();
        let diagrams_off = !self.diagrams_layout_enabled();
        if images_off {
            new_editor.images_enabled = false;
        }
        if diagrams_off {
            new_editor.diagrams_enabled = false;
        }
        new_editor.set_row_striping(self.config.table.row_striping);
        new_editor.set_big_h1(self.config.editor.big_h1);
        if images_off || diagrams_off {
            new_editor.refresh_parsed();
        }
        self.editor = new_editor;
        // Image cache is owned by `EditorState`, so swapping to a new
        // editor resets it — image URLs on the new doc are resolved
        // against the new base directory on the next draw.
        self.file_path = Some(path.clone());
        self.view_state = EditorViewState::new();
        self.images_dirty = true;
        // Repoint the filesystem watcher at the newly-loaded file.
        // Best-effort: failures just leave the user without
        // external-edit prompts on this file.
        if let Some(w) = self.watcher.as_mut() {
            if let Err(e) = w.watch(&path) {
                tracing::warn!(target: "watcher", path = %path.display(), error = %e, "watch swap failed");
            }
        }
        Ok(())
    }

    /// Snapshot the editor's current position as a *file* nav entry.
    /// Returns `None` when there's no associated file path — we can't
    /// reload an entry we can't name.  Used when navigating *away* from
    /// the current file to a different one.
    pub(super) fn current_file_entry(&self) -> Option<NavEntry> {
        self.file_path.clone().map(|path| NavEntry {
            dest: NavDest::File(path),
            scroll: self.editor.scroll,
            cursor_offset: self.editor.cursor.offset,
            mode: self.editor.mode,
        })
    }

    /// Snapshot the editor's current position as an *in-document* nav
    /// entry.  Always available (no path needed), so in-document jumps
    /// record history even in an unsaved `[No file]` buffer.  `footnote`
    /// tags which footnote (if any) is being followed away from here.
    pub(super) fn current_in_doc_entry(&self, footnote: Option<String>) -> NavEntry {
        NavEntry {
            dest: NavDest::InDocument { footnote },
            scroll: self.editor.scroll,
            cursor_offset: self.editor.cursor.offset,
            mode: self.editor.mode,
        }
    }

    /// Push the current position onto `nav_back` as an in-document entry
    /// and clear `nav_forward` (browser semantics) — the shared prelude
    /// for every in-document jump (heading anchor, footnote follow,
    /// footnote back-link).  `footnote` is the label being followed, or
    /// `None` for a heading-anchor jump.
    pub(super) fn record_in_doc_jump(&mut self, footnote: Option<String>) {
        self.nav_back.push(self.current_in_doc_entry(footnote));
        self.nav_forward.clear();
    }

    /// Pop `nav_back` (if any), push the current state onto
    /// `nav_forward`, and load the popped file.  Respects the dirty
    /// guard the same way forward navigation does.
    pub(super) fn navigate_back(&mut self, doc_height: usize, doc_width: usize) {
        let Some(dest) = self.nav_back.pop() else {
            return;
        };
        if let Some(target) = self.cross_file_dirty_target(&dest) {
            // Dirty guard path: restore the popped entry onto the back
            // stack (so Cancel is a true no-op) and prompt the user.
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
        if let Some(target) = self.cross_file_dirty_target(&dest) {
            self.nav_forward.push(dest);
            self.open_dirty_guard(target);
            return;
        }
        self.navigate_to_entry(dest, doc_height, doc_width, /*forward=*/ true);
    }

    /// Returns the path to guard on when restoring `dest` would switch
    /// away from a dirty buffer to a *different* file.  In-document
    /// restores and same-file restores never lose unsaved edits, so they
    /// bypass the dirty guard entirely.
    fn cross_file_dirty_target(&self, dest: &NavEntry) -> Option<PathBuf> {
        if !self.editor.dirty {
            return None;
        }
        match &dest.dest {
            NavDest::File(path) if self.file_path.as_deref() != Some(path.as_path()) => {
                Some(path.clone())
            }
            _ => None,
        }
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
        // A `File` destination naming a *different* file is the only case
        // that reloads the buffer; same-file `File` entries and every
        // `InDocument` entry restore in place.
        let reload_path = match &dest.dest {
            NavDest::File(path) if self.file_path.as_deref() != Some(path.as_path()) => {
                Some(path.clone())
            }
            _ => None,
        };

        // Snapshot where we are now for the opposite stack.  If this
        // restore reloads a different file, returning here needs a file
        // entry (so the reverse navigation reloads); otherwise an
        // in-document entry suffices and also works for `[No file]`.
        let current = if reload_path.is_some() {
            self.current_file_entry()
        } else {
            Some(self.current_in_doc_entry(None))
        };

        if let Some(path) = reload_path {
            if let Err(err) = self.load_file_into_editor(path.clone()) {
                tracing::warn!(target: "link", path = %path.display(), error = %err, "nav load failed");
                return;
            }
        }
        if let Some(e) = current {
            if forward {
                self.nav_back.push(e);
            } else {
                self.nav_forward.push(e);
            }
        }
        // Restore the saved scroll / cursor / mode.
        self.editor.scroll = dest.scroll.min(
            self.editor
                .total_visual_rows_for_mode(doc_width)
                .saturating_sub(1),
        );
        self.editor.cursor.offset = dest.cursor_offset.min(self.editor.buffer.len_chars());
        self.editor.mode = dest.mode;
        self.editor.update_cursor_block();
        // Preview decouples scroll from the cursor (jumps move the
        // viewport without the cursor), so the restored scroll is
        // authoritative there.  In editing modes the cursor drives, so
        // keep it on-screen — mirroring `scroll_to_heading`.
        if self.editor.mode != Mode::Preview {
            self.editor.ensure_cursor_visible(doc_height, doc_width);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_utils::app_with_buffer;

    const H: usize = 20;
    const W: usize = 80;

    #[test]
    fn heading_anchor_jump_records_history_and_back_returns() {
        // A document with a heading near the bottom so jumping to it
        // actually changes the scroll offset.
        let src =
            "Intro paragraph.\n\n".to_string() + &"filler\n\n".repeat(30) + "## Target\n\nEnd.\n";
        let mut app = app_with_buffer(&src, 0);
        assert_eq!(app.editor.scroll, 0);
        assert!(app.nav_back.is_empty());

        app.scroll_to_heading("target", H, W);
        let jumped = app.editor.scroll;
        assert!(jumped > 0, "jump should move the viewport");
        assert_eq!(app.nav_back.len(), 1, "jump should record one back entry");
        assert!(matches!(app.nav_back[0].dest, NavDest::InDocument { .. }));

        // Back returns to the original position without a reload (same
        // buffer), and populates the forward stack.
        app.navigate_back(H, W);
        assert_eq!(
            app.editor.scroll, 0,
            "back should restore the origin scroll"
        );
        assert!(app.nav_back.is_empty());
        assert_eq!(app.nav_forward.len(), 1);

        // Forward re-applies the jump.
        app.navigate_forward(H, W);
        assert_eq!(
            app.editor.scroll, jumped,
            "forward should re-apply the jump"
        );
    }

    #[test]
    fn footnote_reference_follow_jumps_to_definition_and_back_returns() {
        let src = "Intro[^1] text.\n\n".to_string()
            + &"filler\n\n".repeat(30)
            + "[^1]: The definition.\n";
        let mut app = app_with_buffer(&src, 0);
        assert_eq!(app.editor.scroll, 0);

        app.follow_footnote_reference("1", H, W);
        let jumped = app.editor.scroll;
        assert!(jumped > 0, "follow should scroll to the definition");
        assert_eq!(app.nav_back.len(), 1);
        assert!(matches!(app.nav_back[0].dest, NavDest::InDocument { .. }));

        // The definition's back-link returns to the reference.
        app.follow_footnote_back_link("1", H, W);
        assert_eq!(
            app.editor.scroll, 0,
            "back-link should return to the reference"
        );
    }

    #[test]
    fn footnote_back_link_falls_back_to_first_reference_when_no_origin() {
        // Reach the definition by scrolling, not by following — there is no
        // in-document origin, so the back-link jumps to the first reference.
        let src = "Intro[^1] text.\n\n".to_string()
            + &"filler\n\n".repeat(30)
            + "[^1]: The definition.\n";
        let mut app = app_with_buffer(&src, 0);
        // Simulate having scrolled to the bottom manually.
        app.editor.scroll = app.editor.total_visual_rows_for_mode(W).saturating_sub(1);
        assert!(app.nav_back.is_empty());

        app.follow_footnote_back_link("1", H, W);
        assert_eq!(
            app.editor.scroll, 0,
            "fallback should jump to the first reference near the top"
        );
        assert_eq!(app.nav_back.len(), 1, "fallback records its own origin");
    }

    #[test]
    fn back_link_ignores_unrelated_heading_jump_on_stack() {
        // Jump to a heading, then (without following a reference) scroll to
        // the footnote definition and activate its back-link.  The heading
        // jump on the stack top must NOT be consumed — the back-link falls
        // back to the footnote's first reference instead.
        let src = "Ref[^1] here.\n\n## Section\n\n".to_string()
            + &"filler\n\n".repeat(30)
            + "[^1]: The definition.\n";
        let mut app = app_with_buffer(&src, 0);
        app.scroll_to_heading("section", H, W);
        let heading_scroll = app.editor.scroll;
        assert_eq!(app.nav_back.len(), 1);

        app.follow_footnote_back_link("1", H, W);
        // Correct (fallback to first reference) grows nav_back to 2 and
        // leaves nav_forward empty.  The bug (consuming the heading entry
        // via navigate_back) would instead pop to len 1 and push forward.
        assert_eq!(
            app.nav_back.len(),
            2,
            "heading entry retained; back-link recorded its own origin"
        );
        assert!(
            app.nav_forward.is_empty(),
            "fallback must not consume the heading entry into nav_forward"
        );
        let _ = heading_scroll;
    }

    #[test]
    fn resolve_link_at_cursor_classifies_footnote_reference() {
        let src = "Body[^1] more.\n\n[^1]: def.\n";
        let mut app = app_with_buffer(src, 0);
        // Put the cursor inside the `[^1]` marker.
        let at = src.find("[^1]").unwrap() + 1;
        app.editor.cursor.offset = app.editor.buffer.rope().byte_to_char(at);
        assert_eq!(
            app.resolve_link_at_cursor(),
            Some(LinkTarget::Footnote("1".into()))
        );
    }

    #[test]
    fn in_document_back_skips_dirty_guard() {
        // Even with a dirty buffer, an in-document back/forward must not
        // open the dirty guard modal (no file switch → no data loss).
        let src = "Top.\n\n".to_string() + &"filler\n\n".repeat(30) + "## Here\n\nEnd.\n";
        let mut app = app_with_buffer(&src, 0);
        app.scroll_to_heading("here", H, W);
        assert_eq!(app.nav_back.len(), 1);
        assert!(matches!(app.nav_back[0].dest, NavDest::InDocument { .. }));
        app.editor.dirty = true;

        // Compare against the pre-existing modal count (App::new may seed a
        // startup modal) — the in-document back must add none.
        let modals_before = app.modal_stack.len();
        app.navigate_back(H, W);
        assert_eq!(
            app.modal_stack.len(),
            modals_before,
            "in-document back must not raise the dirty guard"
        );
        assert_eq!(app.editor.scroll, 0);
    }
}
