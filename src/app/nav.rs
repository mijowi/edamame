//! Link-following and back/forward navigation
//!
//! Owns:
//! - The [`NavEntry`] back/forward stack record.
//! - The [`App`] methods that consult `nav_back` / `nav_forward`.
//! - Buffer-replacement helper [`App::load_file_into_editor`].
//! - Link resolution and the [`App::follow_link`] dispatcher.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::app::flash::MessageKind;
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
///
/// Two callers, for the same underlying question — "is this a file
/// edamame handles?".  `App::follow_link` uses it to decide whether a
/// `LocalFile` link opens in-editor or is handed to the OS default app;
/// [`super::difftool::is_markdown_pair`] uses it to decide whether a
/// `--diff` pair is reviewable at all.  Shared rather than copied,
/// because the two answers disagreeing is how a `git difftool` walk
/// would open a full-screen review of a shell script.
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
            LinkTarget::LocalFile { path, fragment } => {
                if is_markdown_path(&path) {
                    if self.editor.dirty {
                        self.open_dirty_guard(path, fragment);
                    } else {
                        let _ = self.navigate_to_file_at(path, fragment, doc_height, doc_width);
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

    /// Rendered-line index of the heading `fragment` names in the
    /// *currently loaded* document, if any.
    ///
    /// The match is exact against `ParsedDoc::heading_anchors`, which is
    /// keyed by GFM slug — the same fragment GitHub, a browser, and any
    /// other Markdown renderer resolve.  There is deliberately **no**
    /// leniency here: no slugifying a hand-written `#Getting Started`,
    /// no case folding.  edamame is an editor, so a document written in
    /// it travels; a fragment that resolves only here is a link the
    /// author ships broken everywhere else without ever seeing it fail.
    /// Being strict is what makes a link that works in edamame a link
    /// that works, full stop.
    ///
    /// Both the in-document `#anchor` path and the cross-file deep link
    /// resolve through here, so they can't drift.
    pub(super) fn heading_line_for_fragment(&self, fragment: &str) -> Option<usize> {
        self.editor.parsed.heading_anchors.get(fragment).copied()
    }

    /// Scroll so `slug`'s heading sits at the top of the viewport.
    /// No-op if the slug isn't in the current document's anchor table.
    /// In editing modes (Rendered / Raw) also moves the cursor onto
    /// the heading so subsequent navigation feels anchored.
    pub(super) fn scroll_to_heading(&mut self, slug: &str, doc_height: usize, doc_width: usize) {
        let Some(line_idx) = self.heading_line_for_fragment(slug) else {
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
    pub(super) fn scroll_to_rendered_line(
        &mut self,
        line_idx: usize,
        doc_height: usize,
        doc_width: usize,
    ) {
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

    /// Apply the `#section` the command line named
    /// (`edamame notes.md#setup`), then clear it so it happens once.
    ///
    /// It runs from the first frame's `prepare_viewport` rather than
    /// from `App::new` for the same reason the update notice does: the
    /// jump needs the document's live dimensions, and nothing knows
    /// those until a frame has been measured.  No nav entry is recorded
    /// — there is no earlier position in this session to go back to.
    ///
    /// Like a deep link, a section that resolves to nothing is reported
    /// on the hint line rather than silently ignored.
    pub(super) fn apply_startup_anchor(&mut self, doc_height: usize, doc_width: usize) {
        let Some(fragment) = self.startup_anchor.take() else {
            return;
        };
        match self.heading_line_for_fragment(&fragment) {
            Some(line_idx) => self.scroll_to_rendered_line(line_idx, doc_height, doc_width),
            None => self.flash(
                format!("No section '#{fragment}' in this document"),
                MessageKind::Info,
            ),
        }
        self.needs_draw = true;
    }

    /// Push the current (file, scroll, cursor, mode) onto `nav_back`
    /// and load `path` into the editor.  Clears `nav_forward` to match
    /// browser semantics.  Returns whether the file actually loaded.
    pub(super) fn navigate_to_file(&mut self, path: PathBuf) -> bool {
        let entry = self.current_file_entry();
        if let Err(err) = self.load_file_into_editor(path.clone()) {
            tracing::warn!(target: "link", path = %path.display(), error = %err, "failed to load linked file");
            return false;
        }
        if let Some(e) = entry {
            self.nav_back.push(e);
        }
        self.nav_forward.clear();
        true
    }

    /// [`App::navigate_to_file`] plus the deep-link half: once the file
    /// is loaded, scroll to the heading `fragment` names.
    ///
    /// The jump records *no* in-document history entry — unlike
    /// [`App::scroll_to_heading`], which is a jump *within* a document.
    /// `navigate_to_file` already pushed the origin as a file entry, so
    /// one `NavigateBack` returns the reader to the link they followed
    /// rather than to the top of a document they never saw.
    ///
    /// A fragment naming no heading in the loaded document leaves the
    /// reader at the top of it and says so on the hint line: the file
    /// opened, so silently ignoring the second half of the link would
    /// read as edamame having ignored the anchor rather than the
    /// document having drifted away from it.
    ///
    /// Returns whether the file loaded — and with it, whether this call
    /// has taken ownership of the viewport.  A caller that would
    /// otherwise re-assert cursor visibility (the dirty guard) must skip
    /// doing so on `true`: a freshly loaded editor starts in
    /// `Mode::Preview`, where the fragment jump deliberately moves
    /// `scroll` without moving the cursor, so an `ensure_cursor_visible`
    /// on top of it drags the reader straight back to line 0.
    pub(super) fn navigate_to_file_at(
        &mut self,
        path: PathBuf,
        fragment: Option<String>,
        doc_height: usize,
        doc_width: usize,
    ) -> bool {
        if !self.navigate_to_file(path) {
            return false;
        }
        let Some(fragment) = fragment else {
            return true;
        };
        // The new editor was built at a default viewport width; the
        // anchor table and the scroll arithmetic below both want the
        // live one.
        self.editor.set_viewport_width(doc_width);
        match self.heading_line_for_fragment(&fragment) {
            Some(line_idx) => self.scroll_to_rendered_line(line_idx, doc_height, doc_width),
            None => self.flash(
                format!("No section '#{fragment}' in this document"),
                MessageKind::Info,
            ),
        }
        true
    }

    /// Replace the editor's buffer with the contents of `path` and
    /// refresh dependent caches.  Does NOT touch the nav stack — the
    /// caller decides whether the transition should record history.
    ///
    /// Ends in [`App::on_document_contents_swapped`], which re-runs the
    /// per-document media prompts for the new document: under the
    /// default `images.enabled = "ask"` the prompt is what sets
    /// `session_images_enabled`, so a session that started on a
    /// document with no images would otherwise never display images in
    /// any document opened from within edamame.
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
        // The encoder worker's sender lives on the App because the cache
        // that needs it is rebuilt per document.  Without this the new
        // document's images decode fine and then paint as placeholders
        // forever — `get_protocol_pair` returns `None` with no sender.
        if let Some(tx) = self.resize_tx.clone() {
            new_editor.images.attach_resize_sender(tx);
        }
        // Everything else a new editor needs from `Config`, shared with
        // `App::new` — including preserving a session-level `No` or a
        // persisted `Never` that zeroed `images_enabled` on the previous
        // editor, which stays in effect for this one.
        super::configure_new_editor(
            &mut new_editor,
            &self.config,
            self.images_layout_enabled(),
            self.diagrams_layout_enabled(),
        );
        self.editor = new_editor;
        // Image cache is owned by `EditorState`, so swapping to a new
        // editor resets it — image URLs on the new doc are resolved
        // against the new base directory on the next draw.
        self.file_path = Some(path.clone());
        self.view_state = EditorViewState::new();
        // Marks the image cache dirty and re-evaluates the three
        // per-document media prompts ("this document contains images —
        // show them?") against the newly-loaded document.  A session
        // answer already given is not re-asked.
        self.on_document_contents_swapped();
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
            self.open_dirty_guard(target, None);
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
            self.open_dirty_guard(target, None);
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
        // A nav entry records the mode it was taken in, so an entry
        // pushed before a mid-session vim toggle carries `Preview` —
        // which vim has no way back out of.
        super::leave_preview_under_vim(&self.config, &mut self.editor);
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
    /// destination path, plus the deep link's `#fragment` when the link
    /// carried one — the guard has to carry it across the modal's
    /// lifetime, or answering it drops the reader at the top of the
    /// target document.
    pub(super) fn open_dirty_guard(&mut self, pending: PathBuf, fragment: Option<String>) {
        let display = self
            .file_path
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "current file".to_owned());
        self.modal_stack.push(Box::new(modal::DirtyGuardModal::new(
            &display, pending, fragment,
        )));
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::modal::{
        DiagramsEnabledPromptModal, ImagesEnabledPromptModal, RemoteImagePromptModal,
    };
    use crate::app::test_utils::app_with_buffer;

    const H: usize = 20;
    const W: usize = 80;

    /// Write `contents` to a temporary `.md` file.  The handle is
    /// returned alongside the path so the file outlives the navigation
    /// under test.
    fn md_file(contents: &str) -> (tempfile::NamedTempFile, PathBuf) {
        let mut f = tempfile::Builder::new()
            .suffix(".md")
            .tempfile()
            .expect("temp file");
        f.write_all(contents.as_bytes()).expect("write");
        f.flush().expect("flush");
        let path = f.path().to_path_buf();
        (f, path)
    }

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

    // ── Per-document media prompts (issue #30) ────────────────────────────

    /// The bug in issue #38: a `file.md#section` link classified as a
    /// non-Markdown local file (its "extension" was
    /// `md#section`), so it went to the OS opener, which failed.
    #[test]
    fn a_deep_link_opens_the_file_in_editor_and_lands_on_the_section() {
        let target_src =
            "# Top\n\n".to_string() + &"filler\n\n".repeat(30) + "## Deep Section\n\nEnd.\n";
        let (_f, path) = md_file(&target_src);
        let mut app = app_with_buffer("Link here.\n", 0);

        let url = format!("{}#deep-section", path.display());
        app.follow_link(LinkTarget::parse(&url, None), H, W);

        assert_eq!(
            app.file_path.as_deref(),
            Some(path.as_path()),
            "the link must load in-editor, not hand off to the OS opener"
        );
        assert!(
            app.editor.scroll > 0,
            "the fragment must scroll to its heading, not stay at the top"
        );
    }

    /// The dirty guard sits between a deep link and its destination, so
    /// the fragment has to survive not just the modal's own state but
    /// the close callback that resumes the navigation — which used to
    /// re-assert cursor visibility on the *new* document and, because a
    /// freshly loaded editor starts in `Mode::Preview` with its cursor
    /// at byte 0, scrolled straight back off the section it had just
    /// landed on.  Driven through `dispatch_modal_key` rather than by
    /// replaying the callback, so the button routing is covered too.
    #[test]
    fn a_deep_link_answered_through_the_dirty_guard_still_lands_on_the_section() {
        for (button, keys) in [
            ("Discard", vec![KeyCode::Right, KeyCode::Enter]),
            ("Save", vec![KeyCode::Enter]),
        ] {
            let target_src =
                "# Top\n\n".to_string() + &"filler\n\n".repeat(30) + "## Deep Section\n\nEnd.\n";
            let (_f, path) = md_file(&target_src);
            let (_origin_f, origin) = md_file("Link here.\n");

            // Load the origin from disk so the buffer carries a path —
            // the Save arm branches on that, and without one it detours
            // through the Save-as modal instead.
            let mut app = app_with_buffer("Link here.\n", 0);
            app.load_file_into_editor(origin.clone())
                .expect("load origin");
            app.editor.dirty = true;
            app.last_doc_height = H;
            app.last_doc_width = W;

            let url = format!("{}#deep-section", path.display());
            app.follow_link(LinkTarget::parse(&url, None), H, W);
            assert!(
                app.modal_stack.contains::<modal::DirtyGuardModal>(),
                "{button}: a dirty buffer must route the deep link through the guard"
            );
            assert_eq!(
                app.file_path.as_deref(),
                Some(origin.as_path()),
                "{button}: the guard must not navigate before it is answered"
            );

            for code in keys {
                app.dispatch_modal_key(KeyEvent::new(code, KeyModifiers::NONE), H, W);
            }

            assert!(
                !app.modal_stack.contains::<modal::DirtyGuardModal>(),
                "{button}: answering the guard closes it"
            );
            assert_eq!(
                app.file_path.as_deref(),
                Some(path.as_path()),
                "{button}: the pending destination must load"
            );
            assert!(
                app.editor.scroll > 0,
                "{button}: the fragment must survive the guard — landed at the top instead"
            );
            assert_eq!(
                app.heading_line_for_fragment("deep-section")
                    .map(|l| app.editor.parsed.visual_rows_before(l, W)),
                Some(app.editor.scroll),
                "{button}: the viewport must sit on the linked heading"
            );
        }
    }

    #[test]
    fn a_deep_link_records_one_file_entry_so_back_returns_to_the_link() {
        let target_src = "# Top\n\n".to_string() + &"filler\n\n".repeat(30) + "## Deep\n";
        let (_f, path) = md_file(&target_src);
        let (_origin_f, origin) = md_file("Link here.\n");
        let mut app = app_with_buffer("Link here.\n", 0);
        app.file_path = Some(origin.clone());

        let url = format!("{}#deep", path.display());
        app.follow_link(LinkTarget::parse(&url, None), H, W);
        assert_eq!(
            app.nav_back.len(),
            1,
            "the jump within the freshly-loaded document must not record a second entry"
        );

        app.navigate_back(H, W);
        assert_eq!(app.file_path.as_deref(), Some(origin.as_path()));
    }

    #[test]
    fn a_deep_link_whose_section_is_missing_opens_the_file_and_says_so() {
        let (_f, path) = md_file("# Top\n\nProse.\n");
        let mut app = app_with_buffer("Link here.\n", 0);

        let url = format!("{}#no-such-section", path.display());
        app.follow_link(LinkTarget::parse(&url, None), H, W);

        assert_eq!(app.file_path.as_deref(), Some(path.as_path()));
        assert_eq!(app.editor.scroll, 0);
        assert!(
            app.transient
                .as_ref()
                .is_some_and(|m| m.text.contains("no-such-section")),
            "a fragment that resolves to nothing must be reported, not silently dropped"
        );
    }

    /// Only the GFM slug resolves.  edamame accepting a hand-written
    /// `#Getting Started` or `#Getting-Started` would bless a fragment
    /// that GitHub, a browser, and every other renderer reject — the
    /// author would ship the broken link without ever seeing it fail
    /// here.
    #[test]
    fn only_the_gfm_slug_resolves_a_fragment() {
        let src = "Intro.\n\n".to_string() + &"filler\n\n".repeat(30) + "## Getting Started\n";
        let mut app = app_with_buffer(&src, 0);

        for near_miss in [
            "Getting Started",
            "Getting-Started",
            "getting started",
            "getting%20started",
        ] {
            app.editor.scroll = 0;
            app.scroll_to_heading(near_miss, H, W);
            assert_eq!(
                app.editor.scroll, 0,
                "'{near_miss}' is not the slug and must not resolve"
            );
        }

        app.scroll_to_heading("getting-started", H, W);
        assert!(app.editor.scroll > 0, "the slug itself must resolve");
    }

    #[test]
    fn a_startup_anchor_lands_on_its_section_once() {
        let src = "Intro.\n\n".to_string() + &"filler\n\n".repeat(30) + "## Setup\n\nEnd.\n";
        let mut app = app_with_buffer(&src, 0);
        app.startup_anchor = Some("setup".to_owned());

        app.apply_startup_anchor(H, W);
        let landed = app.editor.scroll;
        assert!(landed > 0, "the named section should be scrolled to");
        assert!(
            app.nav_back.is_empty(),
            "there is no earlier position in the session to go back to"
        );
        assert_eq!(app.startup_anchor, None, "the jump happens once");

        // A later frame must not re-apply it — the reader may have
        // scrolled away by then.
        app.editor.scroll = 0;
        app.apply_startup_anchor(H, W);
        assert_eq!(app.editor.scroll, 0);
    }

    #[test]
    fn a_startup_anchor_naming_no_heading_says_so() {
        let mut app = app_with_buffer("# Top\n\nProse.\n", 0);
        app.startup_anchor = Some("nowhere".to_owned());

        app.apply_startup_anchor(H, W);

        assert_eq!(app.editor.scroll, 0);
        assert!(app
            .transient
            .as_ref()
            .is_some_and(|m| m.text.contains("nowhere")));
    }

    #[test]
    fn navigating_to_a_document_with_images_queues_the_images_prompt() {
        // The session starts on a document with no images, so `App::new`
        // queued no prompt and `session_images_enabled` is still unset.
        let mut app = app_with_buffer("Just prose.\n", 0);
        assert_eq!(app.config.images.enabled, crate::config::ImagesEnabled::Ask);
        assert_eq!(app.session_images_enabled, None);
        assert!(!app.modal_stack.contains::<ImagesEnabledPromptModal>());
        assert!(
            !app.effective_images_enabled(),
            "no answer yet means no decoding"
        );

        let (_f, path) = md_file("![a](img.png)\n");
        app.navigate_to_file(path);

        assert!(
            app.modal_stack.contains::<ImagesEnabledPromptModal>(),
            "the linked document's images must raise the prompt that enables them"
        );
    }

    #[test]
    fn navigating_to_a_document_with_a_diagram_queues_the_diagrams_prompt() {
        let mut app = app_with_buffer("Just prose.\n", 0);
        let (_f, path) = md_file("```mermaid\ngraph TD;\n```\n");
        app.navigate_to_file(path);
        assert!(app.modal_stack.contains::<DiagramsEnabledPromptModal>());
        assert!(
            !app.modal_stack.contains::<ImagesEnabledPromptModal>(),
            "a diagram-only document must not raise the images prompt"
        );
    }

    #[test]
    fn navigating_to_a_document_with_a_remote_image_queues_the_remote_prompt() {
        let mut app = app_with_buffer("Just prose.\n", 0);
        let (_f, path) = md_file("![a](https://example.com/a.png)\n");
        app.navigate_to_file(path);
        assert!(app.modal_stack.contains::<RemoteImagePromptModal>());
        // Images on top of remote, mirroring the startup push order.
        assert!(app.modal_stack.contains::<ImagesEnabledPromptModal>());
    }

    #[test]
    fn a_session_answer_is_not_re_asked_after_navigation() {
        // Answered "Yes" for this session on the startup document …
        let mut app = app_with_buffer("![a](img.png)\n", 0);
        app.modal_stack.remove_first::<ImagesEnabledPromptModal>();
        app.session_images_enabled = Some(true);
        app.session_diagrams_enabled = Some(true);

        let (_f, path) = md_file("![b](other.png)\n\n```mermaid\ngraph TD;\n```\n");
        app.navigate_to_file(path);

        assert!(!app.modal_stack.contains::<ImagesEnabledPromptModal>());
        assert!(!app.modal_stack.contains::<DiagramsEnabledPromptModal>());
        assert!(
            app.effective_images_enabled(),
            "the session answer carries into the new document"
        );
    }

    #[test]
    fn a_session_decline_is_not_re_asked_after_navigation() {
        let mut app = app_with_buffer("![a](img.png)\n", 0);
        app.modal_stack.remove_first::<ImagesEnabledPromptModal>();
        app.session_images_enabled = Some(false);
        app.session_diagrams_enabled = Some(false);
        app.session_remote_declined = true;

        let (_f, path) = md_file("![b](https://example.com/b.png)\n\n```mermaid\ngraph TD;\n```\n");
        app.navigate_to_file(path);

        assert!(!app.modal_stack.contains::<ImagesEnabledPromptModal>());
        assert!(!app.modal_stack.contains::<DiagramsEnabledPromptModal>());
        assert!(!app.modal_stack.contains::<RemoteImagePromptModal>());
        assert!(!app.effective_images_enabled());
        assert!(
            !app.editor.images_enabled,
            "a declined session keeps the new document's image rows collapsed"
        );
    }

    #[test]
    fn an_indexed_terminal_is_not_prompted_by_navigation() {
        // `App::new` suppresses all three prompts below truecolor —
        // `media_renderable` refuses to decode there, so an opt-in we
        // will then decline to honor is pure noise, and `Always` /
        // `Never` would persist a choice made on a terminal that can't
        // show the result.  Navigation owes the same suppression.
        use crate::config::{Config, KeyBindingOverrides, Theme};
        use crate::terminal::{Capabilities, ColorDepth};

        let caps = Capabilities {
            color_depth: ColorDepth::Ansi256,
            ..Capabilities::minimal()
        };
        let mut config = Config::default();
        config.editor.show_welcome = false;
        // …which, with no version recorded, would otherwise raise the
        // post-upgrade notice over the document this test navigates.
        config.editor.last_version_seen = crate::app::update_check::INSTALLED_VERSION.to_owned();
        let mut app = App::new(
            config,
            KeyBindingOverrides::default(),
            (&Theme::default()).into(),
            None,
            caps,
            Vec::new(),
        )
        .expect("build app");
        assert!(!app.media_renderable());

        let (_f, path) = md_file("![a](https://example.com/a.png)\n\n```mermaid\ngraph TD;\n```\n");
        app.navigate_to_file(path);

        assert!(!app.modal_stack.contains::<ImagesEnabledPromptModal>());
        assert!(!app.modal_stack.contains::<DiagramsEnabledPromptModal>());
        assert!(!app.modal_stack.contains::<RemoteImagePromptModal>());
    }

    #[test]
    fn navigation_carries_the_cursor_blink_setting_to_the_new_document() {
        // Second drift found at the same construction site: `App::new`
        // applied `cursor_blink`, the navigation path didn't, so a
        // `cursor_blink = false` config started blinking again the
        // moment the user followed a link.  Both now go through
        // `configure_new_editor`.
        let mut app = app_with_buffer("Just prose.\n", 0);
        app.config.editor.cursor_blink = false;
        let (_f, path) = md_file("More prose.\n");
        app.navigate_to_file(path);
        assert!(
            !app.editor.cursor_blink.is_blinking(),
            "the new document must honor the configured blink setting",
        );
    }

    #[test]
    fn reconfiguring_an_existing_editor_picks_up_every_config_field() {
        // `configure_new_editor` is shared by three callers now: the
        // constructor, the document swap, and — since the drift below —
        // the post-`$EDITOR` config reload.  That third caller hands it
        // an editor that is *already* configured, so every field has to
        // be re-applied rather than merely defaulted.
        //
        // The reload used to live-apply only the theme and the keymap,
        // so hand-editing `syntax_highlighting`, `big_h1`,
        // `cursor_blink` or `table.row_striping` in `config.toml` from
        // inside edamame did nothing until the next launch, while the
        // flash still said "Configuration updated".  Driving the real
        // reload needs a live `$EDITOR`, so the invariant is pinned at
        // the shared helper the reload calls.
        let mut app = app_with_buffer(
            "```rust
fn main() {}
```
",
            0,
        );
        app.editor.set_syntax_highlighting(false);
        app.editor.set_big_h1(false);
        app.editor.set_row_striping(false);
        assert!(app.editor.cursor_blink.is_blinking());

        // Stand in for the user's hand-edit of `config.toml`.
        app.config.editor.syntax_highlighting = true;
        app.config.editor.big_h1 = true;
        app.config.table.row_striping = true;
        app.config.editor.cursor_blink = false;

        let (images_on, diagrams_on) = (app.images_layout_enabled(), app.diagrams_layout_enabled());
        crate::app::configure_new_editor(&mut app.editor, &app.config, images_on, diagrams_on);

        assert!(app.editor.syntax_highlighting, "syntax_highlighting stale");
        assert!(app.editor.big_h1, "big_h1 stale");
        assert!(app.editor.row_striping, "row_striping stale");
        assert!(!app.editor.cursor_blink.is_blinking(), "cursor_blink stale");
    }

    #[test]
    fn navigation_carries_the_encoder_sender_to_the_new_document() {
        // The decode half of the pipeline is not the whole story: the
        // *paint* half needs `ImageCache::resize_tx`, and the cache is
        // rebuilt per document.  Attaching it only in
        // `spawn_event_threads` left every later document with
        // `get_protocol_pair` returning `None` — reserved rows and a
        // placeholder over images that had decoded perfectly.
        let mut app = app_with_buffer("![a](img.png)\n", 0);
        let (tx, _rx) = std::sync::mpsc::channel();
        app.editor.images.attach_resize_sender(tx.clone());
        app.resize_tx = Some(tx);
        assert!(app.editor.images.has_resize_sender());

        let (_f, path) = md_file("![b](other.png)\n");
        app.navigate_to_file(path);

        assert!(
            app.editor.images.has_resize_sender(),
            "a document opened mid-session must be able to encode its images",
        );
    }

    #[test]
    fn navigation_dispatches_decodes_for_the_new_documents_images() {
        // The prompts are only half the story: an answer already given
        // must also reach the *new* document's decode dispatch, which
        // reads `session_*` off the App and the URLs off the freshly
        // built `EditorState` (a swap that resets the image cache).
        // A local path keeps the worker off the network.
        let mut app = app_with_buffer("Just prose.\n", 0);
        app.session_images_enabled = Some(true);
        let (tx, _rx) = std::sync::mpsc::channel();
        app.app_tx = Some(tx);

        let (_f, path) = md_file("![a](img.png)\n");
        app.navigate_to_file(path);
        app.editor.refresh_parsed();
        app.dispatch_visible_image_decodes(0, 20);

        let url = app.editor.parsed.image_blocks[0].url.clone();
        assert!(
            app.editor.images.status(&url).is_some(),
            "the new document's image must be requested, not left untracked",
        );
    }

    #[test]
    fn images_below_the_dispatch_window_are_not_requested_yet() {
        // Dispatch is viewport-limited: an image far below the fold is
        // untouched until the user scrolls toward it.  Pinned because
        // it looks identical to a broken prompt from the outside —
        // open a long document, see no image, conclude nothing works.
        let mut app = app_with_buffer("Just prose.\n", 0);
        app.session_images_enabled = Some(true);
        let (tx, _rx) = std::sync::mpsc::channel();
        app.app_tx = Some(tx);

        let filler = "text\n\n".repeat(200);
        let (_f, path) = md_file(&format!("{filler}![a](img.png)\n"));
        app.navigate_to_file(path);
        app.editor.refresh_parsed();
        let url = app.editor.parsed.image_blocks[0].url.clone();

        app.dispatch_visible_image_decodes(0, 20);
        assert!(
            app.editor.images.status(&url).is_none(),
            "an image 400 rows down must not be fetched from the top of the document",
        );

        let rows = app
            .editor
            .parsed
            .source_map
            .rendered_lines_for_block(app.editor.parsed.image_blocks[0].block_idx);
        app.dispatch_visible_image_decodes(rows.start.saturating_sub(5), 20);
        assert!(
            app.editor.images.status(&url).is_some(),
            "scrolling to it must request it",
        );
    }

    #[test]
    fn a_pending_prompt_is_not_stacked_twice_by_navigation() {
        // The startup document already raised the prompt; navigating
        // before answering it must not queue a second copy.
        let mut app = app_with_buffer("![a](img.png)\n", 0);
        app.on_document_contents_swapped();
        let (_f, path) = md_file("![b](other.png)\n");
        app.navigate_to_file(path);
        assert_eq!(
            app.modal_stack.count::<ImagesEnabledPromptModal>(),
            1,
            "one pending images prompt, not one per document"
        );
    }
}
