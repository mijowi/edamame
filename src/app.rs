use std::io::Stdout;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::Event;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::config::{Config, KeyMap, Theme};
use crate::document::Buffer;
use crate::editor::{edit_ops, EditorState, Mode};
use crate::input::modal::default::DefaultHandler;
use crate::input::{InputDispatcher, ModalHandler};
use crate::ui::{EditorView, EditorViewState, PreviewState};

/// Events that the main loop can receive.
enum AppEvent {
    /// A raw crossterm terminal event.
    Term(Event),
}

/// The application: owns all state and drives the event loop.
pub struct App {
    config: Config,
    theme: &'static Theme,
    file_path: Option<PathBuf>,
    editor: EditorState,
    view_state: EditorViewState,
    should_quit: bool,
}

impl App {
    /// Create the app, loading the file if one is given.
    pub fn new(config: Config, file_path: Option<PathBuf>) -> Result<Self> {
        // Leak the theme so it can be stored as `&'static Theme`.
        // This is intentional: the theme lives for the duration of the process.
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));

        let buffer = match &file_path {
            Some(path) => Buffer::load_file(path)?,
            None => Buffer::new(),
        };

        let editor = EditorState::new_with_config(
            buffer,
            theme,
            config.editor.preserve_blank_lines,
            config.editor.visual_line_nav,
        );

        // Seed the preview state with the editor's already-parsed lines so the
        // first frame honours `preserve_blank_lines` (re-rendering from the raw
        // source bypasses the blank-line preservation pass in `ParsedDoc`).
        let view_state = EditorViewState::new(editor.parsed.lines.clone());

        Ok(Self {
            config,
            theme,
            file_path,
            editor,
            view_state,
            should_quit: false,
        })
    }

    /// Run the event loop until the user quits.
    pub fn run(&mut self, mut terminal: Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        let (tx, rx) = mpsc::channel::<AppEvent>();

        // Spawn a thread to forward crossterm events.
        let tx_clone = tx.clone();
        std::thread::spawn(move || loop {
            match crossterm::event::read() {
                Ok(event) => {
                    if tx_clone.send(AppEvent::Term(event)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        });

        // Build the keymap once and keep it alive for the event loop.
        let keymap = KeyMap::build(&self.config.keybindings)?;

        loop {
            // ── Draw ──────────────────────────────────────────────
            let filename = self.display_filename();
            let editor_ref = &self.editor;
            let theme_ref = self.theme;
            let view_state_ref = &mut self.view_state;

            terminal.draw(|frame| {
                let view = EditorView {
                    state: editor_ref,
                    theme: theme_ref,
                    filename: &filename,
                };
                frame.render_stateful_widget(view, frame.area(), view_state_ref);
            })?;

            // ── Wait for event (with timeout for jitter redraws) ──
            // Use a short timeout so that when the cursor has recently moved
            // to a new block, the view redraws after the reveal delay has
            // elapsed and shows the raw cursor-block view.
            let event_result = rx.recv_timeout(Duration::from_millis(60));
            let event = match event_result {
                Ok(AppEvent::Term(e)) => e,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // No event — just redraw to apply any jitter-delay reveals.
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };

            let term_size = terminal.size()?;
            let viewport_height = term_size.height as usize;
            let doc_height = viewport_height.saturating_sub(1); // minus status bar
            let doc_width = term_size.width as usize;

            // ── Dispatch event → Action ───────────────────────────
            let mut handler = DefaultHandler::new(&keymap);
            if let Some(action) = handler.handle_event(event, &self.editor) {
                let quit = edit_ops::apply(&mut self.editor, action, doc_height, doc_width);
                if quit {
                    self.should_quit = true;
                }
            }

            // Keep preview state lines in sync with editor's parsed doc.
            // (Only needed for Preview mode; Rendered and Raw read from EditorState directly.)
            if self.editor.mode == Mode::Preview {
                let new_lines = self.editor.parsed.lines.clone();
                self.view_state.preview = PreviewState::new(new_lines);
                self.view_state.preview.scroll = self.editor.scroll;
            }

            if self.should_quit {
                break;
            }
        }

        Ok(())
    }

    // ── Helpers ───────────────────────────────────────────────────

    fn display_filename(&self) -> String {
        match &self.file_path {
            Some(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned()),
            None => "[No file]".to_owned(),
        }
    }
}

// ── Extension trait for DefaultHandler ───────────────────────────────────────

/// Private extension trait so `DefaultHandler` can process raw crossterm events
/// (filtering for KeyPress) without exposing this logic in the `ModalHandler`
/// trait (which operates on already-filtered `KeyEvent`s).
trait HandleEvent {
    fn handle_event(&mut self, event: Event, state: &EditorState) -> Option<crate::config::Action>;
}

impl<'k> HandleEvent for DefaultHandler<'k> {
    fn handle_event(&mut self, event: Event, state: &EditorState) -> Option<crate::config::Action> {
        use crossterm::event::KeyEventKind;
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle(key, state),
            _ => None,
        }
    }
}
