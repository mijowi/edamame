//! The App's half of the clipboard.  Every read and write of the OS clipboard happens here,
//! through [`App::clipboard`](super::App) — `edit_ops` only stages a Copy / Cut and inserts the
//! text it is handed — and so does the start of an image paste, which reads the bitmap on a
//! worker ([`App::start_clipboard_image_read`]) and ends in
//! [`PasteImageModal`](super::modal::PasteImageModal).

use std::path::{Path, PathBuf};

use crate::clipboard::TextRead;
use crate::config::Action;
use crate::editor::edit_ops;
use crate::image::paste;
use crate::ui::ModalKind;

use super::{App, MessageKind};

impl App {
    /// The text a paste inserts: the clipboard's text with CRLF collapsed, or the kill-ring
    /// when it holds none (no OS clipboard, or a non-text payload).
    fn paste_source_text(&self, read: TextRead) -> String {
        match read {
            TextRead::Text(text) if !text.is_empty() => {
                crate::document::buffer::normalize_newlines(text)
            }
            _ => self.editor.kill_ring.clone(),
        }
    }

    /// One text-only clipboard read, for a paste that only takes text (vim Visual, the vim
    /// command line) — no bitmap is asked for, so no image is decoded.
    pub(super) fn read_paste_text(&mut self) -> String {
        let read = self.clipboard.read_text();
        self.paste_source_text(read)
    }

    /// Send the text a Copy / Cut staged to the clipboard, in the buffer's own line endings — a
    /// CRLF document copies as CRLF — and flash "Copied".  Called after every path that can run
    /// a Copy or Cut through `edit_ops`; one that staged nothing (an empty line, a Cut with
    /// nothing to cut) neither writes nor flashes.
    pub(super) fn flush_clipboard_write(&mut self) {
        let Some(text) = self.editor.pending_clipboard_write.take() else {
            return;
        };
        let external =
            crate::document::buffer::encode_newlines(&text, self.editor.buffer.line_ending());
        self.clipboard.write_text(external);
        self.flash("Copied", MessageKind::Info);
    }

    /// `Paste` and `PasteImage`.  A plain paste takes the clipboard's text when there is any —
    /// a spreadsheet or a rich-text editor publishes a picture of the copied cells or words
    /// beside the text, and storing that picture would be the surprising behavior — and only a
    /// reachable clipboard with no text goes on to look for a bitmap, falling back to the
    /// kill-ring when there is none.  The text read is cheap, so the everyday `Ctrl-V` never
    /// decodes an image.  `PasteImage` looks only for a bitmap.  A source that can't hold one (a
    /// build without the `clipboard` feature) skips the bitmap read altogether, as does a plain
    /// paste from an unreachable clipboard (over SSH): it goes straight to the kill-ring,
    /// synchronously, and `PasteImage` says it is unavailable.
    pub(super) fn dispatch_paste_action(
        &mut self,
        action: &Action,
        doc_height: usize,
        doc_width: usize,
    ) {
        let plain = matches!(action, Action::Paste);
        let bitmaps = self.clipboard.can_read_bitmaps();
        if plain {
            let read = self.clipboard.read_text();
            let may_hold_bitmap = match &read {
                TextRead::Text(text) => text.is_empty(),
                TextRead::NoText => true,
                TextRead::Unreachable => false,
            };
            if !(bitmaps && may_hold_bitmap) {
                let text = self.paste_source_text(read);
                edit_ops::paste_text(&mut self.editor, &text, doc_height, doc_width);
                self.needs_draw = true;
                return;
            }
        } else if !bitmaps {
            self.flash(
                "This build can't read images from the clipboard",
                MessageKind::Info,
            );
            return;
        }
        self.start_clipboard_image_read(plain);
    }

    /// A plain paste that found neither text nor an image on the clipboard: paste the kill-ring.
    pub(super) fn paste_kill_ring(&mut self) {
        let text = self.editor.kill_ring.clone();
        let (h, w) = (self.last_doc_height, self.last_doc_width);
        edit_ops::paste_text(&mut self.editor, &text, h, w);
    }

    /// Check the image can land at the cursor *before* anything is written, then ask where to
    /// store it — after a Save As when the buffer has no path yet, since the image is stored
    /// relative to the document.
    pub(super) fn begin_image_paste(&mut self, png: Vec<u8>) {
        if !edit_ops::can_insert_image_reference(&mut self.editor, self.last_doc_height) {
            self.notify("Cannot insert image inside this block", ModalKind::Warning);
            return;
        }
        if self.file_path.is_none() {
            self.open_save_as_modal(Some(Box::new(move |app: &mut App| {
                app.open_paste_image_modal(png);
            })));
            return;
        }
        self.open_paste_image_modal(png);
    }

    /// Propose a destination relative to the document — in the directory its images already use —
    /// and open the prompt that confirms it.
    fn open_paste_image_modal(&mut self, png: Vec<u8>) {
        let Some(doc_dir) = self.doc_dir() else {
            return;
        };
        let urls = crate::markdown::local_image_urls(&self.editor.parsed.blocks);
        let dir = paste::infer_dir(&urls);
        let proposed = paste::default_destination(&dir, &doc_dir);
        self.modal_stack
            .push(Box::new(super::modal::PasteImageModal::new(
                png, doc_dir, proposed,
            )));
        self.needs_draw = true;
    }

    /// The open document's directory; `.` for a bare file name.
    fn doc_dir(&self) -> Option<PathBuf> {
        let parent = self.file_path.as_deref()?.parent()?;
        Some(if parent.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            Path::to_path_buf(parent)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::modal::{ClipboardReadModal, NoticeModal, PasteImageModal, SaveAsModal};
    use crate::app::test_utils::{app_with_buffer, close_startup_modals};
    use crate::app::{App, AppEvent};
    use crate::clipboard::{Bitmap, BitmapReader, ClipboardSource, TextRead};
    use crate::config::Action;

    const H: usize = 40;
    const W: usize = 80;

    /// A clipboard holding `text` and `bitmap` on every read and recording every write; or,
    /// with `unreachable`, one whose text read fails as it does over SSH.
    #[derive(Default)]
    struct StubClipboard {
        text: Option<String>,
        bitmap: Option<Bitmap>,
        unreachable: bool,
        writes: Arc<Mutex<Vec<String>>>,
    }

    impl ClipboardSource for StubClipboard {
        fn read_text(&mut self) -> TextRead {
            match &self.text {
                _ if self.unreachable => TextRead::Unreachable,
                Some(text) => TextRead::Text(text.clone()),
                None => TextRead::NoText,
            }
        }

        fn can_read_bitmaps(&self) -> bool {
            true
        }

        fn bitmap_reader(&mut self) -> BitmapReader {
            let bitmap = self.bitmap.clone();
            Box::new(move || bitmap)
        }

        fn write_text(&mut self, text: String) {
            self.writes.lock().unwrap().push(text);
        }
    }

    /// Hand `app` a clipboard holding `contents`; the returned log fills with its writes.
    fn stub(app: &mut App, contents: StubClipboard) -> Arc<Mutex<Vec<String>>> {
        let writes = Arc::clone(&contents.writes);
        app.clipboard = Box::new(contents);
        writes
    }

    fn empty() -> StubClipboard {
        StubClipboard::default()
    }

    fn text(text: &str) -> StubClipboard {
        StubClipboard {
            text: Some(text.to_owned()),
            ..StubClipboard::default()
        }
    }

    fn screenshot() -> StubClipboard {
        StubClipboard {
            bitmap: Some(Bitmap {
                width: 2,
                height: 1,
                rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
            }),
            ..StubClipboard::default()
        }
    }

    /// `app` editing `text` as `notes.md` in a fresh directory, cursor at the end, with an event
    /// channel for the clipboard read's worker.
    fn app_in_dir(text: &str) -> (App, tempfile::TempDir, mpsc::Receiver<AppEvent>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_buffer(text, text.len());
        close_startup_modals(&mut app);
        app.file_path = Some(dir.path().join("notes.md"));
        let rx = channel(&mut app);
        (app, dir, rx)
    }

    /// Give `app` what the run loop would before a clipboard read completes: the event channel
    /// the worker reports on, and the viewport the paste lands under.
    fn channel(app: &mut App) -> mpsc::Receiver<AppEvent> {
        let (tx, rx) = mpsc::channel();
        app.app_tx = Some(tx);
        app.last_doc_height = H;
        app.last_doc_width = W;
        rx
    }

    /// Deliver the clipboard read's result, as the run loop would.
    fn finish_read(app: &mut App, rx: &mpsc::Receiver<AppEvent>) {
        let ev = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the read worker reports back");
        app.handle_async_event(ev);
    }

    /// Dispatch `action` and deliver the read it starts.
    fn paste(app: &mut App, action: Action, rx: &mpsc::Receiver<AppEvent>) {
        app.dispatch_action(action, H, W);
        finish_read(app, rx);
    }

    fn key(app: &mut App, code: KeyCode) {
        app.dispatch_modal_key(KeyEvent::new(code, KeyModifiers::NONE), H, W);
    }

    /// Replace the focused path field's contents with `value`.
    fn retype(app: &mut App, value: &str) {
        for _ in 0..1000 {
            key(app, KeyCode::Backspace);
        }
        for c in value.chars() {
            key(app, KeyCode::Char(c));
        }
    }

    fn files_under(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for entry in walk(dir) {
            out.push(
                entry
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
        out.sort();
        out
    }

    fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
        out
    }

    // ── Text ──────────────────────────────────────────────────────────────

    #[test]
    fn copy_writes_through_the_port_in_the_buffers_line_endings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("crlf.md");
        std::fs::write(&path, "alpha\r\nbeta\r\n").unwrap();
        let mut app = app_with_buffer("", 0);
        app.editor.buffer = crate::document::Buffer::load_file(&path).expect("load");
        app.editor.mode = crate::editor::Mode::Raw;
        let writes = stub(&mut app, empty());
        app.dispatch_action(Action::Copy, H, W);
        assert_eq!(*writes.lock().unwrap(), ["alpha\r\n"]);
        assert_eq!(
            app.editor.kill_ring, "alpha\n",
            "the kill-ring stays `\\n`-only"
        );
        let flash = app.transient.as_ref().expect("a copy flashes");
        assert_eq!(flash.text, "Copied");
    }

    #[test]
    fn a_cut_with_nothing_to_cut_leaves_the_clipboard_alone() {
        let mut app = app_with_buffer("", 0);
        let writes = stub(&mut app, empty());
        app.dispatch_action(Action::Cut, H, W);
        assert!(writes.lock().unwrap().is_empty());
        assert!(
            app.transient.is_none(),
            "nothing was copied, so nothing says so"
        );
    }

    #[test]
    fn copying_an_empty_line_leaves_the_clipboard_alone() {
        let mut app = app_with_buffer("", 0);
        app.editor.mode = crate::editor::Mode::Raw;
        app.editor.kill_ring = "kept".to_owned();
        let writes = stub(&mut app, empty());
        app.dispatch_action(Action::Copy, H, W);
        assert!(writes.lock().unwrap().is_empty());
        assert_eq!(app.editor.kill_ring, "kept");
        assert!(
            app.transient.is_none(),
            "nothing was copied, so nothing says so"
        );
    }

    #[test]
    fn paste_inserts_the_clipboards_text_with_crlf_collapsed() {
        let mut app = app_with_buffer("", 0);
        stub(&mut app, text("one\r\ntwo"));
        app.dispatch_action(Action::Paste, H, W);
        assert_eq!(app.editor.contents(), "one\ntwo");
    }

    #[test]
    fn paste_falls_back_to_the_kill_ring_when_the_clipboard_is_empty() {
        let (mut app, _dir, rx) = app_in_dir("");
        stub(&mut app, empty());
        app.editor.kill_ring = "kept".to_owned();
        paste(&mut app, Action::Paste, &rx);
        assert_eq!(app.editor.contents(), "kept");
        assert!(app.modal_stack.is_empty());
    }

    #[test]
    fn without_bitmap_support_paste_takes_the_kill_ring_at_once() {
        // No event channel: a bitmap read would fail to start, so this proves none is tried.
        let mut app = app_with_buffer("", 0);
        close_startup_modals(&mut app);
        app.clipboard = Box::new(crate::clipboard::TerminalClipboard);
        app.editor.kill_ring = "kept".to_owned();
        app.dispatch_action(Action::Paste, H, W);
        assert_eq!(app.editor.contents(), "kept");
        assert!(app.modal_stack.is_empty());
    }

    #[test]
    fn an_unreachable_clipboard_pastes_the_kill_ring_at_once() {
        // Over SSH the OS clipboard can't be reached, but the source can still hold bitmaps: a
        // bitmap read would find nothing, so none is started.  No event channel, as above.
        let mut app = app_with_buffer("", 0);
        close_startup_modals(&mut app);
        stub(
            &mut app,
            StubClipboard {
                unreachable: true,
                ..screenshot()
            },
        );
        app.editor.kill_ring = "kept".to_owned();
        app.dispatch_action(Action::Paste, H, W);
        assert_eq!(app.editor.contents(), "kept");
        assert!(app.modal_stack.is_empty());
    }

    #[test]
    fn without_bitmap_support_paste_image_says_it_is_unavailable() {
        let mut app = app_with_buffer("prose\n", 0);
        close_startup_modals(&mut app);
        app.clipboard = Box::new(crate::clipboard::TerminalClipboard);
        app.dispatch_action(Action::PasteImage, H, W);
        assert!(app.modal_stack.is_empty());
        assert_eq!(app.editor.contents(), "prose\n");
        let flash = app.transient.as_ref().expect("a message must be shown");
        assert_eq!(
            flash.text,
            "This build can't read images from the clipboard"
        );
    }

    #[test]
    fn paste_with_text_and_an_image_pastes_the_text() {
        let (mut app, dir, _rx) = app_in_dir("");
        let mut data = screenshot();
        data.text = Some("words".to_owned());
        stub(&mut app, data);
        app.dispatch_action(Action::Paste, H, W);
        assert_eq!(app.editor.contents(), "words");
        assert!(app.modal_stack.is_empty(), "no image read is started");
        assert!(files_under(dir.path()).is_empty());
    }

    // ── Images ────────────────────────────────────────────────────────────

    #[test]
    fn a_screenshot_is_saved_beside_the_document_after_confirming() {
        let (mut app, dir, rx) = app_in_dir("prose\n");
        stub(&mut app, screenshot());
        app.dispatch_action(Action::Paste, H, W);
        assert!(app.modal_stack.contains::<ClipboardReadModal>());
        key(&mut app, KeyCode::Char('x'));
        assert!(
            app.modal_stack.contains::<ClipboardReadModal>(),
            "the read modal ignores input"
        );
        finish_read(&mut app, &rx);
        assert!(!app.modal_stack.contains::<ClipboardReadModal>());
        assert!(app.modal_stack.contains::<PasteImageModal>());
        assert!(
            files_under(dir.path()).is_empty(),
            "nothing before confirming"
        );

        key(&mut app, KeyCode::Enter);
        assert!(!app.modal_stack.contains::<PasteImageModal>());
        let files = files_under(dir.path());
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].starts_with("images/") && files[0].ends_with(".png"));
        let decoded = image::open(dir.path().join(&files[0])).expect("a real PNG");
        assert_eq!((decoded.width(), decoded.height()), (2, 1));
        assert_eq!(
            app.editor.contents(),
            format!("prose\n\n![]({})\n", files[0])
        );
        assert!(
            app.editor
                .parsed
                .image_blocks
                .iter()
                .any(|i| i.url == files[0]),
            "the reference must parse as an image block"
        );
    }

    #[test]
    fn the_image_goes_in_the_directory_the_document_uses() {
        let (mut app, dir, rx) = app_in_dir("![a](assets/a.png)\n\nprose\n");
        stub(&mut app, screenshot());
        paste(&mut app, Action::PasteImage, &rx);
        key(&mut app, KeyCode::Enter);
        let files = files_under(dir.path());
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].starts_with("assets/"), "{files:?}");
        assert!(app
            .editor
            .contents()
            .ends_with(&format!("prose\n\n![]({})\n", files[0])));
    }

    #[test]
    fn paste_image_ignores_text_on_the_clipboard() {
        let (mut app, _dir, rx) = app_in_dir("");
        let mut data = screenshot();
        data.text = Some("words".to_owned());
        stub(&mut app, data);
        paste(&mut app, Action::PasteImage, &rx);
        assert!(app.modal_stack.contains::<PasteImageModal>());
    }

    #[test]
    fn escape_writes_nothing_and_leaves_the_buffer_alone() {
        let (mut app, dir, rx) = app_in_dir("prose\n");
        stub(&mut app, screenshot());
        paste(&mut app, Action::PasteImage, &rx);
        key(&mut app, KeyCode::Esc);
        assert!(app.modal_stack.is_empty());
        assert!(files_under(dir.path()).is_empty());
        assert_eq!(app.editor.contents(), "prose\n");
    }

    #[test]
    fn escape_during_the_read_drops_its_result() {
        let (mut app, dir, rx) = app_in_dir("prose\n");
        stub(&mut app, empty());
        app.editor.kill_ring = "kept".to_owned();
        app.dispatch_action(Action::Paste, H, W);
        key(&mut app, KeyCode::Esc);
        assert!(app.modal_stack.is_empty(), "Esc stops waiting at once");

        finish_read(&mut app, &rx);
        assert!(app.modal_stack.is_empty());
        assert_eq!(
            app.editor.contents(),
            "prose\n",
            "an abandoned plain paste pastes nothing"
        );
        assert!(files_under(dir.path()).is_empty());
    }

    #[test]
    fn an_abandoned_read_landing_under_a_new_one_is_not_taken_for_it() {
        let (mut app, _dir, rx) = app_in_dir("");
        stub(&mut app, screenshot());
        app.dispatch_action(Action::PasteImage, H, W);
        key(&mut app, KeyCode::Esc);
        app.dispatch_action(Action::PasteImage, H, W);
        // Both workers report; whichever lands first, only the second's opens a prompt.
        finish_read(&mut app, &rx);
        finish_read(&mut app, &rx);
        assert_eq!(app.modal_stack.len(), 1);
        assert!(app.modal_stack.contains::<PasteImageModal>());
    }

    #[test]
    fn an_absolute_path_keeps_the_prompt_open() {
        let (mut app, dir, rx) = app_in_dir("");
        stub(&mut app, screenshot());
        paste(&mut app, Action::PasteImage, &rx);
        retype(&mut app, "/x.png");
        key(&mut app, KeyCode::Enter);
        assert!(app.modal_stack.contains::<PasteImageModal>());
        assert!(files_under(dir.path()).is_empty());
        assert_eq!(app.editor.contents(), "");
    }

    /// Documents in subfolders commonly share an image folder beside them, so a `..` path is
    /// written there and linked as typed.
    #[test]
    fn a_path_through_the_parent_folder_is_written_and_linked() {
        let (mut app, dir, rx) = app_in_dir("");
        let docs = dir.path().join("docs");
        std::fs::create_dir(&docs).unwrap();
        app.file_path = Some(docs.join("notes.md"));
        stub(&mut app, screenshot());
        paste(&mut app, Action::PasteImage, &rx);
        retype(&mut app, "../assets/shot.png");
        key(&mut app, KeyCode::Enter);
        assert!(dir.path().join("assets/shot.png").exists());
        assert_eq!(app.editor.contents(), "![](../assets/shot.png)\n");
    }

    #[test]
    fn an_edited_path_with_spaces_is_written_and_escaped() {
        let (mut app, dir, rx) = app_in_dir("");
        stub(&mut app, screenshot());
        paste(&mut app, Action::PasteImage, &rx);
        retype(&mut app, "my pics/shot (1).png");
        key(&mut app, KeyCode::Enter);
        assert!(dir.path().join("my pics/shot (1).png").exists());
        assert_eq!(app.editor.contents(), "![](<my pics/shot (1).png>)\n");
        assert!(app
            .editor
            .parsed
            .image_blocks
            .iter()
            .any(|i| i.url == "my pics/shot (1).png"));
    }

    #[test]
    fn a_failed_write_keeps_the_prompt_open_for_another_try() {
        let (mut app, dir, rx) = app_in_dir("");
        std::fs::write(dir.path().join("blocker"), b"a file, not a folder").unwrap();
        stub(&mut app, screenshot());
        paste(&mut app, Action::PasteImage, &rx);
        retype(&mut app, "blocker/x.png");
        key(&mut app, KeyCode::Enter);
        assert!(app.modal_stack.contains::<PasteImageModal>());
        assert_eq!(app.editor.contents(), "");

        retype(&mut app, "x.png");
        key(&mut app, KeyCode::Enter);
        assert!(!app.modal_stack.contains::<PasteImageModal>());
        assert!(dir.path().join("x.png").exists());
        assert_eq!(app.editor.contents(), "![](x.png)\n");
    }

    #[test]
    fn inside_a_table_the_image_goes_in_the_cell() {
        let (mut app, dir, rx) = app_in_dir("| a | b |\n|---|---|\n| c | d |\n");
        app.editor.cursor.offset = "| a | b |\n|---|---|\n| c".len();
        stub(&mut app, screenshot());
        paste(&mut app, Action::PasteImage, &rx);
        retype(&mut app, "shot.png");
        key(&mut app, KeyCode::Enter);
        assert!(dir.path().join("shot.png").exists());
        assert_eq!(
            app.editor.contents(),
            "| a | b |\n|---|---|\n| c![](shot.png) | d |\n"
        );
    }

    /// A paste the block at the cursor refuses warns, and neither asks for a path nor writes.
    fn assert_refused_at(src: &str, offset: usize) {
        let (mut app, dir, rx) = app_in_dir(src);
        app.editor.cursor.offset = offset;
        stub(&mut app, screenshot());
        paste(&mut app, Action::PasteImage, &rx);
        assert!(!app.modal_stack.contains::<PasteImageModal>(), "{src:?}");
        assert!(
            app.modal_stack.contains::<NoticeModal>(),
            "a warning is shown: {src:?}"
        );
        assert!(files_under(dir.path()).is_empty());
        assert_eq!(app.editor.contents(), src);
    }

    #[test]
    fn inside_a_code_block_nothing_is_asked_or_written() {
        assert_refused_at("```\ncode\n```\n", 5);
    }

    #[test]
    fn inside_a_heading_nothing_is_asked_or_written() {
        assert_refused_at("# Title\n\nprose\n", 4);
        assert_refused_at("Title\n=====\n", 2);
    }

    #[test]
    fn an_unsaved_buffer_is_saved_first_then_asked_about() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_buffer("", 0);
        close_startup_modals(&mut app);
        let rx = channel(&mut app);
        stub(&mut app, screenshot());
        paste(&mut app, Action::PasteImage, &rx);
        assert!(app.modal_stack.contains::<SaveAsModal>());
        assert!(!app.modal_stack.contains::<PasteImageModal>());

        retype(&mut app, &dir.path().join("new.md").to_string_lossy());
        key(&mut app, KeyCode::Enter);
        assert!(app.modal_stack.contains::<PasteImageModal>());

        key(&mut app, KeyCode::Enter);
        let files = files_under(dir.path());
        assert_eq!(files.len(), 2, "the document and the image: {files:?}");
        assert!(files.iter().any(|f| f.starts_with("images/")));
    }

    #[test]
    fn paste_image_with_no_image_says_so() {
        let (mut app, _dir, rx) = app_in_dir("prose\n");
        stub(&mut app, text("words"));
        paste(&mut app, Action::PasteImage, &rx);
        assert_eq!(app.editor.contents(), "prose\n");
        let flash = app.transient.as_ref().expect("a message must be shown");
        assert_eq!(flash.text, "No image on the clipboard");
    }

    #[test]
    fn a_malformed_bitmap_is_reported_and_nothing_is_asked() {
        let (mut app, dir, rx) = app_in_dir("");
        let mut data = screenshot();
        data.bitmap.as_mut().unwrap().rgba.pop();
        stub(&mut app, data);
        paste(&mut app, Action::PasteImage, &rx);
        assert!(!app.modal_stack.contains::<PasteImageModal>());
        assert!(app.modal_stack.contains::<NoticeModal>());
        assert!(files_under(dir.path()).is_empty());
    }
}
