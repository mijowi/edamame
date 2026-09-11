//! Link-following and back/forward navigation: the [`NavEntry`] stack record,
//! the [`App`] methods over `nav_back` / `nav_forward`, the buffer-replacement
//! helper [`App::load_file_into_editor`], and the [`App::follow_link`]
//! dispatcher.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::app::flash::MessageKind;
use crate::app::modal;
use crate::docs::DocId;
use crate::document::Buffer;
use crate::editor::link::LinkTarget;
use crate::editor::{mouse_ops, EditorState, Mode};
use crate::ui::EditorViewState;

use super::App;

/// Where a [`NavEntry`] restores to.
#[derive(Debug, Clone)]
pub(super) enum NavDest {
    /// A file to load before restoring scroll/cursor/mode.  A cross-file
    /// restore passes through the dirty guard.
    File(PathBuf),
    /// A position within the *currently loaded* document (footnote follow,
    /// `#heading` jump, footnote back-link): no reload, no dirty guard, and no
    /// path — so history is recorded even for an unsaved `[No file]` buffer.
    ///
    /// `footnote` names the footnote whose reference was followed to leave this
    /// position (`None` for a heading jump), so a definition's back-link can
    /// distinguish "I arrived by following *this* footnote" from an unrelated
    /// jump sitting on the stack.
    InDocument { footnote: Option<String> },
    /// A page of the embedded manual, named by id because it has no path — the
    /// text lives in the binary.
    EmbeddedDoc(DocId),
}

/// A destination held across the dirty guard, which has to name it in its prose
/// and resume it on Save / Discard.  Not a `PathBuf`: a manual page would need a
/// fake one, which every point treating a path as a real file (the watcher,
/// `Save`, the own-write hash) would then have to recognize and strip.
#[derive(Debug, Clone)]
pub(crate) enum NavPending {
    File(PathBuf),
    Doc(DocId),
}

impl NavPending {
    /// How the destination is named in the dirty guard's prose.
    pub(crate) fn display_name(&self) -> String {
        match self {
            NavPending::File(p) => p.display().to_string(),
            NavPending::Doc(id) => format!("the {} documentation", id.title()),
        }
    }
}

/// One entry on [`App::nav_back`] / [`App::nav_forward`]: enough state to
/// restore the exact scroll / cursor / mode a position was left in.
#[derive(Debug, Clone)]
pub(super) struct NavEntry {
    pub(super) dest: NavDest,
    pub(super) scroll: usize,
    pub(super) cursor_offset: usize,
    pub(super) mode: Mode,
}

/// True when `path` ends in `.md` / `.markdown` (case-insensitive) — "is this a
/// file edamame handles?".
///
/// Shared by `App::follow_link` (open in-editor or hand to the OS?) and
/// [`super::difftool::is_markdown_pair`] (is a `--diff` pair reviewable?) rather
/// than copied: the two disagreeing is how a `git difftool` walk would open a
/// full-screen review of a shell script.
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
    /// Resolve the link under the keyboard cursor.  Shares
    /// `mouse_ops::link_at_offset` with the mouse path, so `FollowLink` behaves
    /// identically whichever device fired it.
    pub(super) fn resolve_link_at_cursor(&self) -> Option<LinkTarget> {
        let cursor_byte = self
            .editor
            .buffer
            .rope()
            .char_to_byte(self.editor.cursor.offset);
        let source = self.editor.buffer.contents();
        // Unambiguous (`[^…]` with no `(url)`), so checked first.
        if let Some(target) = mouse_ops::footnote_at_offset(&source, cursor_byte) {
            return Some(target);
        }
        let url = mouse_ops::link_at_offset(&source, cursor_byte)?;
        let base_dir = self.file_path.as_deref().and_then(|p| p.parent());
        Some(LinkTarget::parse(&url, base_dir))
    }

    /// Follow `target` by its classified kind.  A no-op for an empty anchor or
    /// an unknown heading slug, and when the dirty guard intercepts.
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
                // A link inside a manual page resolves against the *embedded*
                // set: the page is pathless, so `LinkTarget::parse` had no
                // `base_dir` and returned a bare relative path, which the branch
                // below would resolve against the process's working directory —
                // opening whatever `security.md` sits next to the user's shell.
                //
                // Gated here rather than in `LinkTarget::parse`, which is pure
                // and must keep answering the same way for an ordinary document
                // that links to a file of its own by one of these names.
                if self.open_doc.is_some() {
                    match crate::docs::resolve_doc_reference(&path, fragment) {
                        crate::docs::DocLinkResolution::Doc(id, frag) => {
                            // No dirty guard: a manual page is read-only, so
                            // `dirty` cannot be set while one is open.
                            self.open_doc_page(id, frag, doc_height, doc_width);
                        }
                        // Ships in the repository but not in the binary (the
                        // contributor docs, the root `SECURITY.md`).
                        crate::docs::DocLinkResolution::External(url) => {
                            self.spawn_open_worker(url);
                        }
                    }
                    return;
                }
                if is_markdown_path(&path) {
                    if self.editor.dirty {
                        self.open_dirty_guard(NavPending::File(path), fragment);
                    } else {
                        let _ = self.navigate_to_file_at(path, fragment, doc_height, doc_width);
                    }
                } else {
                    // Non-Markdown: defer to the OS handler, via the same
                    // worker path as remote URLs.
                    let url = path.to_string_lossy().into_owned();
                    self.spawn_open_worker(url);
                }
            }
        }
    }

    /// Rendered-line index of the heading `fragment` names in the currently
    /// loaded document, if any.  Both the in-document `#anchor` path and the
    /// cross-file deep link resolve through here, so they can't drift.
    ///
    /// Matched exactly against `ParsedDoc::heading_anchors`, keyed by GFM slug.
    /// Deliberately no leniency — no slugifying `#Getting Started`, no case
    /// folding: a fragment that resolved only here would be a link the author
    /// ships broken everywhere else without ever seeing it fail.
    pub(super) fn heading_line_for_fragment(&self, fragment: &str) -> Option<usize> {
        self.editor.parsed.heading_anchors.get(fragment).copied()
    }

    /// Scroll so `slug`'s heading sits at the viewport top, moving the cursor
    /// onto it in editing modes.  No-op for a slug not in the anchor table.
    pub(super) fn scroll_to_heading(&mut self, slug: &str, doc_height: usize, doc_width: usize) {
        let Some(line_idx) = self.heading_line_for_fragment(slug) else {
            return;
        };
        // So `NavigateBack` returns to the link.  Tagged `None`: a heading
        // jump isn't a footnote follow.
        self.record_in_doc_jump(None);
        self.scroll_to_rendered_line(line_idx, doc_height, doc_width);
    }

    /// Scroll so rendered line `line_idx` sits at the viewport top, moving the
    /// cursor to its first source byte in editing modes.  Records no nav
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

    /// Follow a footnote reference to its definition, recording the origin so
    /// the back-link returns here.  No-op for an undefined label.
    pub(super) fn follow_footnote_reference(
        &mut self,
        label: &str,
        doc_height: usize,
        doc_width: usize,
    ) {
        let Some(&line_idx) = self.editor.parsed.footnote_anchors.get(label) else {
            return;
        };
        // Tagged so the definition's back-link recognizes *this* follow.
        self.record_in_doc_jump(Some(label.to_string()));
        self.scroll_to_rendered_line(line_idx, doc_height, doc_width);
    }

    /// Follow a footnote definition's back-link: to the exact spot the reader
    /// came from, or — if they scrolled here directly — to the footnote's first
    /// reference.
    pub(super) fn follow_footnote_back_link(
        &mut self,
        label: &str,
        doc_height: usize,
        doc_width: usize,
    ) {
        // Only when the top entry records following *this* footnote; an
        // unrelated jump on the stack would warp the reader elsewhere.
        let top_is_this_footnote = matches!(
            self.nav_back.last().map(|e| &e.dest),
            Some(NavDest::InDocument { footnote: Some(l) }) if l == label
        );
        if top_is_this_footnote {
            self.navigate_back(doc_height, doc_width);
            return;
        }
        if let Some(line_idx) = self.first_reference_line(label) {
            self.record_in_doc_jump(None);
            self.scroll_to_rendered_line(line_idx, doc_height, doc_width);
        }
    }

    /// Rendered-line index of the first `[^label]` reference — not the
    /// `[^label]:` definition — in the current buffer.
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

    /// Apply the `#section` the command line named (`edamame notes.md#setup`),
    /// then clear it so it happens once.
    ///
    /// Runs from the first frame's `prepare_viewport`, not `App::new`: the jump
    /// needs live document dimensions, which nothing knows until a frame is
    /// measured.  Records no nav entry — there is no earlier position to return
    /// to.  A section resolving to nothing is reported on the hint line.
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

    /// Push the current position onto `nav_back` and load `path`, clearing
    /// `nav_forward` (browser semantics).  Returns whether the load succeeded.
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

    /// [`App::navigate_to_file`] plus the deep-link half: scroll to the heading
    /// `fragment` names once the file is loaded.
    ///
    /// Records *no* in-document history entry, unlike
    /// [`App::scroll_to_heading`]: `navigate_to_file` already pushed the origin
    /// as a file entry, so one `NavigateBack` returns to the link rather than to
    /// the top of a document the reader never saw.  A fragment naming no heading
    /// says so on the hint line rather than silently dropping half the link.
    ///
    /// The returned bool also says whether this call has taken ownership of the
    /// viewport: a caller that would otherwise re-assert cursor visibility (the
    /// dirty guard) must skip it on `true`, since a freshly loaded editor starts
    /// in `Mode::Preview`, where the jump moves `scroll` without the cursor and
    /// an `ensure_cursor_visible` drags the reader back to line 0.
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
        // The new editor was built at a default width; the anchor table and the
        // scroll arithmetic want the live one.
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

    /// Build the `EditorState` for a document replacing the current one, wired
    /// to everything an editor needs from `App` and `Config`.
    ///
    /// Shared because the wiring has drifted twice — the encoder worker's
    /// `ResizeRequest` sender, and `cursor_blink` — each time invisibly until a
    /// *second* document was opened.  The two callers, a file off disk and an
    /// embedded manual page, differ only in where the `Buffer` came from.
    pub(super) fn editor_for_buffer(&self, buffer: Buffer) -> EditorState {
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
                    // ratatui-image 11 returns a `FontSize`; we carry a tuple.
                    let fs = p.font_size();
                    (fs.width, fs.height)
                })
                .unwrap_or((10, 20)),
        );
        // The sender lives on the App because the cache needing it is rebuilt
        // per document.  Without it `get_protocol_pair` returns `None` and
        // images decode fine but paint as placeholders forever.
        if let Some(tx) = self.resize_tx.clone() {
            new_editor.images.attach_resize_sender(tx);
        }
        // Everything else from `Config`, shared with `App::new` — including a
        // session `No` or persisted `Never`, which stays in effect here.
        super::configure_new_editor(
            &mut new_editor,
            &self.config,
            self.images_layout_enabled(),
            self.diagrams_layout_enabled(),
        );
        new_editor
    }

    pub(super) fn load_file_into_editor(&mut self, path: PathBuf) -> Result<()> {
        let buffer = Buffer::load_file(&path)?;
        // Stamp the own-write filter from the bytes just read, so the inotify
        // event some backends synthesize on `open(2)` is suppressed.
        self.set_disk_hash(buffer.contents().as_bytes());
        self.editor = self.editor_for_buffer(buffer);
        self.file_path = Some(path.clone());
        // `open_doc` and `file_path` are mutually exclusive, and this is the
        // transition back to a real file.  Leaving it set would keep the status
        // bar naming a page the reader has left and — worse — keep `follow_link`
        // resolving relative links against the embedded manual.
        self.open_doc = None;
        self.view_state = EditorViewState::new();
        // The counterpart of `load_doc_into_editor`'s park: a vim session
        // suspended for a read-only document comes back with the next editable
        // one.
        self.sync_vim_suspension();
        // Marks the image cache dirty and re-evaluates the per-document media
        // prompts; a session answer already given is not re-asked.
        self.on_document_contents_swapped();
        // Best-effort: a failure leaves the user without external-edit prompts
        // on this file.
        if let Some(w) = self.watcher.as_mut() {
            if let Err(e) = w.watch(&path) {
                tracing::warn!(target: "watcher", path = %path.display(), error = %e, "watch swap failed");
            }
        }
        Ok(())
    }

    /// Snapshot the current position as a *file* nav entry, or `None` with no
    /// associated path — an entry we can't name can't be reloaded.
    pub(super) fn current_file_entry(&self) -> Option<NavEntry> {
        self.file_path.clone().map(|path| NavEntry {
            dest: NavDest::File(path),
            scroll: self.editor.scroll,
            cursor_offset: self.editor.cursor.offset,
            mode: self.editor.mode,
        })
    }

    /// Snapshot wherever we are now as the entry to return *to*.  Every site
    /// recording an origin before replacing the document goes through here
    /// rather than `current_file_entry`, which returns `None` for a pathless
    /// manual page and so loses the way back.
    pub(super) fn current_origin_entry(&self) -> Option<NavEntry> {
        if let Some(id) = self.open_doc {
            return Some(NavEntry {
                dest: NavDest::EmbeddedDoc(id),
                scroll: self.editor.scroll,
                cursor_offset: self.editor.cursor.offset,
                mode: self.editor.mode,
            });
        }
        self.current_file_entry()
    }

    /// Snapshot the current position as an *in-document* nav entry.  Needs no
    /// path, so jumps record history even in an unsaved `[No file]` buffer.
    /// `footnote` tags which footnote, if any, is being followed away from.
    pub(super) fn current_in_doc_entry(&self, footnote: Option<String>) -> NavEntry {
        NavEntry {
            dest: NavDest::InDocument { footnote },
            scroll: self.editor.scroll,
            cursor_offset: self.editor.cursor.offset,
            mode: self.editor.mode,
        }
    }

    /// The shared prelude for every in-document jump: push the current position
    /// onto `nav_back` and clear `nav_forward`.
    pub(super) fn record_in_doc_jump(&mut self, footnote: Option<String>) {
        self.nav_back.push(self.current_in_doc_entry(footnote));
        self.nav_forward.clear();
    }

    /// Pop `nav_back`, push the current state onto `nav_forward`, and load the
    /// popped destination.  Respects the dirty guard, as forward does.
    pub(super) fn navigate_back(&mut self, doc_height: usize, doc_width: usize) {
        let Some(dest) = self.nav_back.pop() else {
            return;
        };
        if let Some(target) = self.cross_file_dirty_target(&dest) {
            // Restored so Cancel is a true no-op.
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

    /// The destination to guard on when restoring `dest` would leave a dirty
    /// buffer for a *different* document.  In-document and same-file restores
    /// lose no unsaved edits, so they bypass the guard.
    fn cross_file_dirty_target(&self, dest: &NavEntry) -> Option<NavPending> {
        if !self.editor.dirty {
            return None;
        }
        match &dest.dest {
            NavDest::File(path) if self.file_path.as_deref() != Some(path.as_path()) => {
                Some(NavPending::File(path.clone()))
            }
            NavDest::EmbeddedDoc(id) if self.open_doc != Some(*id) => Some(NavPending::Doc(*id)),
            _ => None,
        }
    }

    /// Shared back/forward dispatch: push the current state onto the opposite
    /// stack, then load `dest` and restore its scroll / cursor / mode.
    fn navigate_to_entry(
        &mut self,
        dest: NavEntry,
        doc_height: usize,
        doc_width: usize,
        forward: bool,
    ) {
        // Only a `File` naming a *different* file reloads; same-file and
        // `InDocument` entries restore in place.
        let reload_path = match &dest.dest {
            NavDest::File(path) if self.file_path.as_deref() != Some(path.as_path()) => {
                Some(path.clone())
            }
            _ => None,
        };
        // The same question for a manual page.
        let reload_doc = match &dest.dest {
            NavDest::EmbeddedDoc(id) if self.open_doc != Some(*id) => Some(*id),
            _ => None,
        };

        // A restore that reloads needs a file entry so the reverse navigation
        // reloads too; otherwise an in-document entry suffices and also works
        // for `[No file]`.
        let current = if reload_path.is_some() || reload_doc.is_some() {
            self.current_origin_entry()
        } else {
            Some(self.current_in_doc_entry(None))
        };

        if let Some(path) = reload_path {
            if let Err(err) = self.load_file_into_editor(path.clone()) {
                tracing::warn!(target: "link", path = %path.display(), error = %err, "nav load failed");
                return;
            }
        }
        if let Some(id) = reload_doc {
            self.load_doc_into_editor(id);
        }
        if let Some(e) = current {
            if forward {
                self.nav_back.push(e);
            } else {
                self.nav_forward.push(e);
            }
        }
        self.editor.scroll = dest.scroll.min(
            self.editor
                .total_visual_rows_for_mode(doc_width)
                .saturating_sub(1),
        );
        self.editor.cursor.offset = dest.cursor_offset.min(self.editor.buffer.len_chars());
        self.editor.mode = dest.mode;
        // An entry pushed before a mid-session vim toggle carries `Preview`,
        // which vim has no way back out of.
        super::leave_preview_under_vim(&self.config, &mut self.editor);
        self.editor.update_cursor_block();
        // Preview decouples scroll from the cursor, so the restored scroll is
        // authoritative there; elsewhere the cursor drives.
        if self.editor.mode != Mode::Preview {
            self.editor.ensure_cursor_visible(doc_height, doc_width);
        }
    }

    /// Show the `Save / Discard / Cancel` modal for a pending link-follow.  The
    /// deep link's `#fragment` has to be carried across the modal's lifetime, or
    /// answering it drops the reader at the top of the target document.
    pub(super) fn open_dirty_guard(&mut self, pending: NavPending, fragment: Option<String>) {
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
        FiguresEnabledPromptModal, ImagesEnabledPromptModal, RemoteImagePromptModal,
    };
    use crate::app::test_utils::app_with_buffer;

    const H: usize = 20;
    const W: usize = 80;

    /// Write `contents` to a temp `.md` file; the handle is returned so the
    /// file outlives the navigation under test.
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
        // The heading sits near the bottom, so the jump moves the scroll.
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

        app.navigate_back(H, W);
        assert_eq!(
            app.editor.scroll, 0,
            "back should restore the origin scroll"
        );
        assert!(app.nav_back.is_empty());
        assert_eq!(app.nav_forward.len(), 1);

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

        app.follow_footnote_back_link("1", H, W);
        assert_eq!(
            app.editor.scroll, 0,
            "back-link should return to the reference"
        );
    }

    #[test]
    fn footnote_back_link_falls_back_to_first_reference_when_no_origin() {
        // Reached by scrolling, not by following, so there is no in-document
        // origin and the back-link falls back to the first reference.
        let src = "Intro[^1] text.\n\n".to_string()
            + &"filler\n\n".repeat(30)
            + "[^1]: The definition.\n";
        let mut app = app_with_buffer(&src, 0);
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
        // The heading jump on the stack top must not be consumed: the back-link
        // falls back to the footnote's first reference instead.
        let src = "Ref[^1] here.\n\n## Section\n\n".to_string()
            + &"filler\n\n".repeat(30)
            + "[^1]: The definition.\n";
        let mut app = app_with_buffer(&src, 0);
        app.scroll_to_heading("section", H, W);
        let heading_scroll = app.editor.scroll;
        assert_eq!(app.nav_back.len(), 1);

        app.follow_footnote_back_link("1", H, W);
        // The bug — consuming the heading entry via `navigate_back` — would pop
        // to len 1 and push forward instead.
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
        let at = src.find("[^1]").unwrap() + 1;
        app.editor.cursor.offset = app.editor.buffer.rope().byte_to_char(at);
        assert_eq!(
            app.resolve_link_at_cursor(),
            Some(LinkTarget::Footnote("1".into()))
        );
    }

    #[test]
    fn in_document_back_skips_dirty_guard() {
        // No file switch means no data loss, so no guard.
        let src = "Top.\n\n".to_string() + &"filler\n\n".repeat(30) + "## Here\n\nEnd.\n";
        let mut app = app_with_buffer(&src, 0);
        app.scroll_to_heading("here", H, W);
        assert_eq!(app.nav_back.len(), 1);
        assert!(matches!(app.nav_back[0].dest, NavDest::InDocument { .. }));
        app.editor.dirty = true;

        // `App::new` may have seeded a startup modal, so compare counts.
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

    /// Issue #38: the link classified as a non-Markdown local file — its
    /// "extension" was `md#section` — and went to the OS opener, which failed.
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

    /// The fragment has to survive the guard's close callback, which used to
    /// re-assert cursor visibility on the new document — and, a fresh editor
    /// starting in `Mode::Preview` at byte 0, scrolled straight off the section.
    /// Driven through `dispatch_modal_key` so the button routing is covered.
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

            // The Save arm branches on the buffer having a path; without one
            // it detours through the Save-as modal.
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

    /// Only the GFM slug resolves: accepting a hand-written `#Getting Started`
    /// would bless a fragment every other renderer rejects, and the author would
    /// ship the broken link without ever seeing it fail here.
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

        // A later frame must not re-apply it; the reader may have scrolled away.
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
        // The session starts with no images, so no prompt was queued.
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
    fn an_image_appearing_mid_session_queues_the_images_prompt() {
        // A document opened *without* images leaves the ask gate unset; an
        // image that appears afterwards — a pasted screenshot, or typed
        // `![](…)` — must raise the prompt the on-load path would have,
        // or `effective_images_enabled` stays false and it never decodes.
        let mut app = app_with_buffer("Just prose.\n", 0);
        assert_eq!(app.session_images_enabled, None);
        assert!(!app.modal_stack.contains::<ImagesEnabledPromptModal>());

        app.editor.buffer.insert(0, "![x](img.png)\n\n");
        app.editor.set_viewport_width(80);
        app.editor.refresh_parsed();
        app.dispatch_visible_image_decodes(0, 20);

        assert!(
            app.modal_stack.contains::<ImagesEnabledPromptModal>(),
            "an in-view image with the ask gate open must queue the prompt"
        );
    }

    #[test]
    fn dismissing_the_images_prompt_does_not_invite_it_back() {
        // The mid-session queue runs on every frame, so a dismissal that
        // left the ask gate open would raise the prompt again on the very
        // next one and the user could never get rid of it: Escape records
        // the same session answer `No` does, which is what closes the gate.
        let mut app = app_with_buffer("Just prose.\n", 0);
        app.editor.buffer.insert(0, "![x](img.png)\n\n");
        app.editor.set_viewport_width(80);
        app.editor.refresh_parsed();
        app.dispatch_visible_image_decodes(0, 20);
        assert!(app.modal_stack.contains::<ImagesEnabledPromptModal>());

        app.dispatch_modal_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 20, 80);
        assert!(
            !app.modal_stack.contains::<ImagesEnabledPromptModal>(),
            "Escape dismisses the prompt"
        );

        app.dispatch_visible_image_decodes(0, 20);
        assert!(
            !app.modal_stack.contains::<ImagesEnabledPromptModal>(),
            "a dismissed prompt must not be re-queued by the next frame"
        );
    }

    #[test]
    fn navigating_to_a_document_with_a_diagram_queues_the_diagrams_prompt() {
        let mut app = app_with_buffer("Just prose.\n", 0);
        let (_f, path) = md_file("```mermaid\ngraph TD;\n```\n");
        app.navigate_to_file(path);
        assert!(app.modal_stack.contains::<FiguresEnabledPromptModal>());
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
        let mut app = app_with_buffer("![a](img.png)\n", 0);
        app.modal_stack.remove_first::<ImagesEnabledPromptModal>();
        app.session_images_enabled = Some(true);
        app.session_diagrams_enabled = Some(true);

        let (_f, path) = md_file("![b](other.png)\n\n```mermaid\ngraph TD;\n```\n");
        app.navigate_to_file(path);

        assert!(!app.modal_stack.contains::<ImagesEnabledPromptModal>());
        assert!(!app.modal_stack.contains::<FiguresEnabledPromptModal>());
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
        assert!(!app.modal_stack.contains::<FiguresEnabledPromptModal>());
        assert!(!app.modal_stack.contains::<RemoteImagePromptModal>());
        assert!(!app.effective_images_enabled());
        assert!(
            !app.editor.images_enabled,
            "a declined session keeps the new document's image rows collapsed"
        );
    }

    #[test]
    fn an_indexed_terminal_is_not_prompted_by_navigation() {
        // `App::new` suppresses all three prompts below truecolor, where
        // `media_renderable` refuses to decode anyway; navigation owes the same
        // suppression.
        use crate::config::{Config, KeyBindingOverrides, Theme};
        use crate::terminal::{Capabilities, ColorDepth};

        let caps = Capabilities {
            color_depth: ColorDepth::Ansi256,
            ..Capabilities::minimal()
        };
        let mut config = Config::default();
        config.editor.show_welcome = false;
        // With no version recorded, the post-upgrade notice would otherwise open
        // over the document this test navigates.
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
        assert!(!app.modal_stack.contains::<FiguresEnabledPromptModal>());
        assert!(!app.modal_stack.contains::<RemoteImagePromptModal>());
    }

    #[test]
    fn navigation_carries_the_cursor_blink_setting_to_the_new_document() {
        // Drift regression: `App::new` applied `cursor_blink` and the navigation
        // path didn't, so following a link resumed blinking.
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
        // The post-`$EDITOR` config reload hands `configure_new_editor` an
        // already-configured editor, so every field must be re-applied, not
        // merely defaulted.  The reload used to live-apply only the theme and
        // keymap, so hand-editing `syntax_highlighting`, `big_h1`,
        // `cursor_blink` or `row_striping` did nothing until the next launch
        // while the flash still said "Configuration updated".  Driving the real
        // reload needs a live `$EDITOR`, so the invariant is pinned here.
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

        // Stands in for the user's hand-edit of `config.toml`.
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
        // The paint half needs `ImageCache::resize_tx`, and the cache is rebuilt
        // per document: attaching it only in `spawn_event_threads` left every
        // later document painting placeholders over images that decoded fine.
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
        // An answer already given must also reach the new document's decode
        // dispatch, which reads `session_*` off the App and the URLs off the
        // freshly built `EditorState`.  A local path keeps the worker offline.
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
        // Dispatch is viewport-limited.  Pinned because from the outside this
        // looks identical to a broken prompt: open a long document, see no
        // image, conclude nothing works.
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
        // Navigating before answering the startup prompt must not queue a
        // second copy.
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
