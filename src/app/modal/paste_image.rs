//! The two modals of a clipboard image paste.
//!
//! [`ClipboardReadModal`] holds the screen while a worker reads the clipboard's bitmap and
//! encodes it as PNG: both take long enough on a large screenshot to freeze the UI.  It ignores
//! every key but `Esc`, so nothing moves the cursor the paste lands at, and `Esc` stops waiting —
//! the result of a read no modal waits on is dropped (matched by [`ClipboardImageRead::id`]).
//! It stays hidden for [`READ_MODAL_DELAY`], so a read that finds nothing (an empty clipboard)
//! or a small image doesn't flash a modal on and off.
//! [`App::handle_clipboard_image_read`] takes the result: no image falls back to a text paste
//! (or says so, for the explicit command), and an image opens the prompt.
//!
//! [`PasteImageModal`] confirms where the PNG is stored.  One path field, relative to the
//! document and pre-filled by [`App::open_paste_image_modal`] — directory and file name together,
//! e.g. `images/20260928-143012.png` — reusing the shared [`SaveCopyState`] + [`SaveCopyView`]
//! widget.  `Save` checks the path ([`paste::resolve_destination`]), writes the already-encoded
//! bytes, and inserts the reference; `Esc` writes nothing, since nothing touches the disk before
//! the path is confirmed.

use std::any::Any;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::{App, MessageKind};
use crate::editor::edit_ops;
use crate::image::paste;
use crate::ui::scroll_container::{centered_rect_for_content, draw_frame, ContentSize, FrameOpts};
use crate::ui::{ModalKind, SaveCopyResponse, SaveCopyState, SaveCopyView};

/// Monotonic read ids — see [`ClipboardImageRead::id`].
static READ_SEQ: AtomicU64 = AtomicU64::new(1);

/// What the read worker reports back in [`AppEvent::ClipboardImageRead`](crate::app::AppEvent).
#[derive(Debug)]
pub struct ClipboardImageRead {
    /// The read's id, matched against [`ClipboardReadModal::id`]; a result no open modal is
    /// waiting on was abandoned with `Esc`.
    id: u64,
    /// The bitmap encoded as PNG, `Err` when it could not be, or `None` when the clipboard held
    /// no bitmap.
    png: Option<Result<Vec<u8>, String>>,
}

// ── Reading ───────────────────────────────────────────────────────────────

const READING_NOTE: &str = "Reading the clipboard image…";

/// How long [`ClipboardReadModal`] stays hidden: about the point where a delay stops reading as
/// instant.  Keys still go to it meanwhile, so a key pressed in this window is dropped.
pub(in crate::app) const READ_MODAL_DELAY: Duration = Duration::from_millis(100);

pub struct ClipboardReadModal {
    id: u64,
    /// A plain `Paste`, which falls back to a text paste when there is no image, rather than the
    /// explicit `PasteImage`, which says so.
    plain: bool,
    /// When the modal is drawn; [`App::tick_clipboard_read`] sets `shown` once it passes.
    show_at: Instant,
    shown: bool,
    esc_button_rect: Option<Rect>,
}

impl Modal for ClipboardReadModal {
    fn is_shown(&self) -> bool {
        self.shown
    }

    fn next_deadline(&self) -> Option<Instant> {
        (!self.shown).then_some(self.show_at)
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let content = ContentSize {
            width: READING_NOTE.chars().count() as u16,
            height: 0,
            pinned_top: 1,
            pinned_bottom: 0,
            ..Default::default()
        };
        let buf = frame.buffer_mut();
        let layout = draw_frame(
            centered_rect_for_content(content, area),
            buf,
            FrameOpts {
                title: "Paste Image",
                kind: ModalKind::Normal,
                show_close_hint: true,
                content,
                theme: ctx.theme,
            },
        );
        self.esc_button_rect = layout.esc_hit_rect;
        Paragraph::new(Line::from(Span::styled(
            READING_NOTE,
            ctx.theme.modal_description,
        )))
        .style(ctx.theme.modal_bg)
        .render(layout.body, buf);
    }

    fn handle_key(&mut self, key: KeyEvent, _: &mut App, _: usize, _: usize) -> ModalOutcome {
        if key.code == KeyCode::Esc {
            ModalOutcome::Close
        } else {
            ModalOutcome::Continue
        }
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        super::types::close_if_esc_clicked(self.esc_button_rect, col, row)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// ── Confirming ────────────────────────────────────────────────────────────

pub struct PasteImageModal {
    state: SaveCopyState,
    /// The encoded image, waiting for its path.
    png: Vec<u8>,
    /// The document's directory, which the path field is relative to.
    doc_dir: PathBuf,
}

impl PasteImageModal {
    pub fn new(png: Vec<u8>, doc_dir: PathBuf, proposed: String) -> Self {
        Self {
            state: SaveCopyState::new(proposed),
            png,
            doc_dir,
        }
    }
}

impl Modal for PasteImageModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = SaveCopyView {
            theme: ctx.theme,
            cursor_visible: ctx.cursor_visible,
            title: "Paste Image",
            note: Some("Save the image to this path, relative to the document's folder:"),
        };
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        app: &mut App,
        doc_height: usize,
        doc_width: usize,
    ) -> ModalOutcome {
        let input = match self.state.handle_key(&key) {
            SaveCopyResponse::Continue => return ModalOutcome::Continue,
            SaveCopyResponse::Cancelled => return ModalOutcome::Close,
            SaveCopyResponse::Save(input) => input,
        };
        let dest = match paste::resolve_destination(&input, &self.doc_dir) {
            Ok(dest) => dest,
            Err(e) => {
                self.state.last_error = Some(e);
                return ModalOutcome::Continue;
            }
        };
        // Checked when the paste began; re-checked because the buffer can change underneath an
        // open modal (an external reload), and writing first would orphan the file.
        if !edit_ops::can_insert_image_reference(&mut app.editor, doc_height) {
            return ModalOutcome::CloseAnd(Box::new(|app| {
                app.notify("Cannot insert image inside this block", ModalKind::Warning);
            }));
        }
        if let Err(e) = paste::write(&self.png, &dest.target) {
            self.state.last_error = Some(e);
            return ModalOutcome::Continue;
        }
        let link = dest.link;
        ModalOutcome::CloseAnd(Box::new(move |app| {
            edit_ops::insert_image_reference_at_cursor(
                &mut app.editor,
                &link,
                doc_height,
                doc_width,
            );
            app.needs_draw = true;
        }))
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        self.state.paste(text);
        ModalOutcome::Continue
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        super::types::close_if_esc_clicked(self.state.esc_button_rect, col, row)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// ── App side ──────────────────────────────────────────────────────────────

impl App {
    /// Read the clipboard's bitmap on a worker, encoding it as PNG there too, behind a
    /// [`ClipboardReadModal`].  `plain` is a `Paste` that found no text, which falls back to a
    /// text paste when there is no image either.
    pub(in crate::app) fn start_clipboard_image_read(&mut self, plain: bool) {
        let Some(tx) = self.app_tx.clone() else {
            self.notify("Internal error: no event channel.", ModalKind::Error);
            return;
        };
        let id = READ_SEQ.fetch_add(1, Ordering::Relaxed);
        let reader = self.clipboard.bitmap_reader();
        std::thread::spawn(move || {
            let png = reader().map(|bitmap| paste::encode_png(&bitmap));
            let _ = tx.send(crate::app::AppEvent::ClipboardImageRead(
                ClipboardImageRead { id, png },
            ));
        });
        self.modal_stack.push(Box::new(ClipboardReadModal {
            id,
            plain,
            show_at: Instant::now() + READ_MODAL_DELAY,
            shown: false,
            esc_button_rect: None,
        }));
    }

    /// Show a pending read's [`ClipboardReadModal`] once [`READ_MODAL_DELAY`] has passed.  Runs
    /// with the per-iteration timers, before `editor.modal_open` is derived, so the modal and
    /// the editor state under it change on the same frame.
    pub(in crate::app) fn tick_clipboard_read(&mut self) {
        let Some(modal) = self.modal_stack.find_first_mut::<ClipboardReadModal>() else {
            return;
        };
        if !modal.shown && Instant::now() >= modal.show_at {
            modal.shown = true;
            self.needs_draw = true;
        }
    }

    /// Take a finished read: close its [`ClipboardReadModal`], then paste text when a plain
    /// paste found no image, say so when the explicit command found none, or go on to the path
    /// prompt.  A read no modal is waiting on was abandoned with `Esc`, and is dropped.
    pub(in crate::app) fn handle_clipboard_image_read(&mut self, read: ClipboardImageRead) {
        let Some(plain) = self
            .modal_stack
            .find_first_mut::<ClipboardReadModal>()
            .filter(|modal| modal.id == read.id)
            .map(|modal| modal.plain)
        else {
            return;
        };
        self.modal_stack.remove_first::<ClipboardReadModal>();
        self.needs_draw = true;
        match read.png {
            None if plain => self.paste_kill_ring(),
            None => self.flash("No image on the clipboard", MessageKind::Info),
            Some(Err(e)) => self.notify(e, ModalKind::Error),
            Some(Ok(png)) => self.begin_image_paste(png),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Instant;

    use super::{ClipboardReadModal, READ_MODAL_DELAY};
    use crate::app::test_utils::{app_with_buffer, close_startup_modals};
    use crate::app::App;

    /// `app` waiting on a clipboard read, with the read modal just pushed.
    fn reading() -> App {
        let mut app = app_with_buffer("", 0);
        close_startup_modals(&mut app);
        let (tx, _rx) = mpsc::channel();
        app.app_tx = Some(tx);
        app.start_clipboard_image_read(true);
        app
    }

    #[test]
    fn the_read_modal_starts_hidden_but_wakes_the_loop_to_show_it() {
        let started = Instant::now();
        let mut app = reading();
        assert!(
            app.modal_stack.contains::<ClipboardReadModal>(),
            "it takes input"
        );
        app.tick_clipboard_read();
        assert!(!app.any_modal_shown(), "hidden before the delay");
        let wake = app.next_deadline(started).expect("a wake-up to show it");
        assert!(wake <= Instant::now() + READ_MODAL_DELAY);
    }

    #[test]
    fn the_read_modal_shows_once_the_delay_passes() {
        let mut app = reading();
        app.modal_stack
            .find_first_mut::<ClipboardReadModal>()
            .unwrap()
            .show_at = Instant::now();
        app.needs_draw = false;
        app.tick_clipboard_read();
        assert!(app.any_modal_shown());
        assert!(app.needs_draw);
        assert_eq!(
            app.modal_stack.next_deadline(),
            None,
            "nothing left to wake for"
        );
    }
}
