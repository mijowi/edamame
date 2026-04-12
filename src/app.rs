use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::Result;
use crossterm::event::{Event, KeyEventKind};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::Stdout;

use crate::config::{Action, Config, KeyMap, Theme};
use crate::document::Buffer;
use crate::editor::Mode;
use crate::markdown::{parse, Renderer};
use crate::ui::{EditorView, EditorViewState};

/// Events that the main loop can receive.
enum AppEvent {
    /// A raw crossterm terminal event.
    Term(Event),
}

/// The application: owns all state and drives the event loop.
pub struct App {
    config: Config,
    keymap: KeyMap,
    theme: Theme,
    file_path: Option<PathBuf>,
    buffer: Buffer,
    mode: Mode,
    view_state: EditorViewState,
    should_quit: bool,
    /// Set while a quit-confirmation dialog is pending.
    pending_quit: bool,
}

impl App {
    /// Create the app, loading the file if one is given.
    pub fn new(config: Config, file_path: Option<PathBuf>) -> Result<Self> {
        let keymap = KeyMap::build(&config.keybindings)?;
        let theme = Theme::default();

        let buffer = match &file_path {
            Some(path) => Buffer::load_file(path)?,
            None => Buffer::new(),
        };

        // Parse and render the document into styled lines.
        let content = buffer.contents();
        let blocks = parse(&content);
        let renderer = Renderer::new(&theme);
        let lines = renderer.render(&blocks);

        let view_state = EditorViewState::new(lines);

        Ok(Self {
            config,
            keymap,
            theme,
            file_path,
            buffer,
            mode: Mode::Preview,
            view_state,
            should_quit: false,
            pending_quit: false,
        })
    }

    /// Run the event loop until the user quits.
    pub fn run(
        &mut self,
        mut terminal: Terminal<CrosstermBackend<Stdout>>,
    ) -> Result<()> {
        let (tx, rx) = mpsc::channel::<AppEvent>();

        // Spawn a thread to forward crossterm events via the channel.
        let tx_clone = tx.clone();
        std::thread::spawn(move || {
            loop {
                match crossterm::event::read() {
                    Ok(event) => {
                        if tx_clone.send(AppEvent::Term(event)).is_err() {
                            break; // receiver dropped — app is shutting down
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        loop {
            // Draw
            terminal.draw(|frame| {
                let filename = self.display_filename();
                let view = EditorView {
                    theme: &self.theme,
                    mode: self.mode,
                    filename: &filename,
                    modified: false, // Phase 0: read-only preview
                };
                frame.render_stateful_widget(view, frame.area(), &mut self.view_state);
            })?;

            // Wait for next event
            match rx.recv() {
                Ok(AppEvent::Term(event)) => {
                    self.handle_event(event, terminal.size()?.height as usize);
                }
                Err(_) => break, // channel closed
            }

            if self.should_quit {
                break;
            }
        }

        Ok(())
    }

    // ── Event handling ────────────────────────────────────────────

    fn handle_event(&mut self, event: Event, terminal_height: usize) {
        // Only handle key press events (ignore key release / repeat).
        let key = match event {
            Event::Key(k) if k.kind == KeyEventKind::Press => k,
            _ => return,
        };

        let viewport_height = terminal_height.saturating_sub(1); // minus status bar

        if let Some(action) = self.keymap.action_for(&key).cloned() {
            self.apply_action(action, viewport_height);
        }
    }

    fn apply_action(&mut self, action: Action, viewport_height: usize) {
        match action {
            Action::Quit => {
                // Phase 0: buffer is never dirty, so quit immediately.
                self.should_quit = true;
            }

            Action::ScrollUp => {
                self.view_state.scroll_up(1);
            }
            Action::ScrollDown => {
                self.view_state.scroll_down(1, viewport_height);
            }
            Action::ScrollPageUp => {
                self.view_state.scroll_up(viewport_height);
            }
            Action::ScrollPageDown => {
                self.view_state.scroll_down(viewport_height, viewport_height);
            }
            Action::ScrollToTop => {
                self.view_state.scroll_to_top();
            }
            Action::ScrollToBottom => {
                self.view_state.scroll_to_bottom(viewport_height);
            }

            // Phase 0 stubs — will be implemented in later phases.
            Action::MoveUp => self.view_state.scroll_up(1),
            Action::MoveDown => self.view_state.scroll_down(1, viewport_height),

            // All other actions are unimplemented in Phase 0.
            _ => {}
        }
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
