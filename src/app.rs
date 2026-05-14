pub mod modal;

mod actions;
mod event_loop;
mod external_editor;
mod flash;
mod frame_timer;
mod image_dispatch;
mod nav;
mod pointer;

#[cfg(test)]
mod test_utils;

use std::io::Stdout;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use std::time::Instant;

use anyhow::Result;
use crossterm::event::Event;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::config::{Config, ConfigWarning, KeyBindingOverrides, KeyMap, Theme, ThemeFile};
use crate::document::Buffer;
use crate::editor::link::LinkTarget;
use crate::editor::{mouse_ops, EditorState};
use crate::input::MouseDispatcher;
use crate::terminal::{Capabilities, ColorDepth, PointerShape};
use crate::ui::{EditorViewState, HintChord, ModalKind};

pub use flash::MessageKind;

use self::flash::TransientMessage;
use self::modal::ModalStack;
use self::nav::NavEntry;

/// Events that the main loop can receive.
pub(crate) enum AppEvent {
    /// A raw crossterm terminal event.
    Term(Event),
    /// Worker-thread notification that an image decode finished.
    /// `Ok(LoadedImage)` inserts the decoded bytes into
    /// `EditorState::images`; `Err((url, message))` records the failure
    /// so we don't retry on every render.
    ImageReady(Result<crate::image::LoadedImage, (String, String)>),
    /// Encoder-thread notification that a `ResizeRequest` finished.
    /// `Ok(response)` is routed to its originating `ThreadProtocol` via
    /// `ImageCache::apply_resize_response`.  `Err(_)` is only used to
    /// keep the pending-request FIFO balanced — the failed entry is
    /// popped and the placeholder stays visible until a subsequent
    /// frame re-enqueues the encode.
    ProtocolReady(Result<ratatui_image::thread::ResizeResponse, ratatui_image::errors::Errors>),
    /// Phase 8 — worker-thread report that `open::that` finished on a
    /// URL or non-Markdown local file.  Currently only logged; Phase 9
    /// will surface failures on the hint line.
    LinkOpenResult(std::result::Result<(), String>),
}

/// Generic modal prompt hosted on the hint line.
/// The `handler` fn is the single callback invoked when one of the chord keys
/// is pressed — it receives the triggering `KeyCode` so the same prompt type
/// can host multiple-button flows.
#[allow(dead_code)] // first consumer lands in Phase 11
pub struct HintPrompt {
    pub prompt: String,
    pub chords: Vec<HintChord>,
    pub handler: fn(&mut App, crossterm::event::KeyCode),
}

/// The application: owns all state and drives the event loop.
pub struct App {
    config: Config,
    /// Keybinding overrides loaded from `keybindings.toml`.  Held so
    /// `KeyMap::build` can be called in `run()` alongside capability
    /// detection and kitty-enhancement key registration, same as before
    /// the config split.
    keybindings: KeyBindingOverrides,
    theme: &'static Theme,
    capabilities: Capabilities,
    file_path: Option<PathBuf>,
    editor: EditorState,
    view_state: EditorViewState,
    should_quit: bool,
    /// Session-only override for the master images-enabled switch,
    /// set by `Yes` / `No` on the images-enabled prompt.  `Some(true)`
    /// renders images for the rest of this process; `Some(false)`
    /// keeps them as placeholders; `None` defers to `config.images.enabled`.
    /// `Always` / `Never` persist the choice to config instead of
    /// setting this flag.
    session_images_enabled: Option<bool>,
    /// Session-only override for the master diagrams-enabled switch,
    /// set by `Yes` / `No` on the diagrams-enabled prompt.  Mirrors
    /// [`Self::session_images_enabled`] — kept separate so the two
    /// prompts can be answered independently.
    pub(crate) session_diagrams_enabled: Option<bool>,
    /// Click-count tracking and drag state for mouse input.
    mouse: MouseDispatcher,
    /// Active drag target, set on mouse-down and read by each subsequent
    /// `Drag` event.  `DragTarget::TextSelection` covers normal click-drag
    /// text selection (the Phase 5 fallthrough); the other variants carry
    /// Phase 6's table-specific row / column / border drags.  Cleared on
    /// `Release`.
    drag_target: Option<mouse_ops::DragTarget>,
    /// True when the most recent mouse-move landed inside the editor's
    /// scrollbar gutter.  Used to render the thumb in its bright
    /// "active" style on hover.  Reset whenever the pointer leaves the
    /// gutter or the scrollbar disappears.
    scrollbar_hover: bool,
    /// Last pointer shape we asked the terminal for.  Used to avoid writing
    /// an OSC 22 escape on every mouse-move event when the shape hasn't
    /// actually changed — keeps the output stream quiet on terminals that do
    /// honour the escape and doesn't matter on those that don't.
    last_pointer_shape: PointerShape,
    /// Once the user picks `Yes` / `Always` on the remote-load prompt,
    /// this flag stays set for the rest of the process so further image
    /// loads can proceed without a second prompt.  Persists only in
    /// memory; `Always` also writes back to `config.images.remote_policy`.
    session_allow_remote: bool,
    /// Sender for the main loop's mpsc channel; retained so background
    /// decode threads can push `AppEvent::ImageReady`.  Initialised in
    /// `run`, so wrapped in `Option` during `new` construction.
    app_tx: Option<mpsc::Sender<AppEvent>>,
    /// Wall-clock timestamp of the last observed scroll change.  Used
    /// by `is_scrolling` to decide whether images should fall back to
    /// the halfblocks partial-render path (avoids per-frame re-encoding
    /// of Sixel / iTerm2 graphics during continuous scroll).  Reset to
    /// `None` on resize so a newly-visible image renders at the settled
    /// protocol immediately.
    last_scroll_at: Option<Instant>,
    /// Set whenever an `ImageReady` event updates the image cache but
    /// the parsed doc hasn't yet been rebuilt to reflect the image's
    /// aspect-aware row count.  Consumed at the top of the next loop
    /// iteration — coalescing N simultaneous decodes into a single
    /// `refresh_parsed` call instead of N.  Avoids stalling scroll
    /// input when several image workers complete in quick succession.
    images_dirty: bool,
    /// Drives the event-driven redraw gate: the main loop only calls
    /// `terminal.draw()` when this is true.  Set by event handlers
    /// that mutate visible state; cleared after a successful draw.
    /// Initialised to `true` so the first iteration paints the opening
    /// frame.  Without this gate, the 60 ms `recv_timeout` would fire a
    /// full redraw ~17 times per second even with no input — the
    /// dominant cause of idle CPU prior to Phase 15.
    needs_draw: bool,
    /// When `Some`, a `Resize` burst is in progress and draws are
    /// suppressed until this instant passes.  Each subsequent Resize
    /// extends the deadline, so a slow drag never paints mid-drag.
    /// Cleared by the deadline-elapse branch in the main loop, which
    /// then triggers a single settled-size redraw.
    resize_quiesce_at: Option<Instant>,
    /// Wall-clock timestamp of the last `terminal.draw()` call.  Used by
    /// the main-loop frame throttle: events can arrive faster than we
    /// want to draw (every wheel tick is an event), so we coalesce by
    /// skipping the draw when less than `MIN_FRAME_INTERVAL` has elapsed
    /// since the previous draw.  `None` before the first draw.
    last_draw_at: Option<Instant>,
    /// Most-recently observed width of the document area, refreshed at
    /// the top of each main-loop iteration.  The image decode worker
    /// uses it to pre-render the halfblocks scratch at the same
    /// dimensions the UI thread will request on first paint — eliminates
    /// the ~5-20 ms sync encode that `get_protocol_pair`'s cold path
    /// previously did on the UI thread.  `0` until the first iteration.
    ///
    /// This is the *clamped* doc-area width (after
    /// `editor.max_width_enabled` is applied), not the raw terminal
    /// width — images render inside the doc area, so the scratch must
    /// match the doc width or the first paint resizes anyway.
    last_area_width: u16,
    /// A `Term` event pulled off the channel by `drain_pending_image_ready`
    /// (which uses `try_recv` and can't put the event back).  Processed
    /// on the next loop iteration before calling `recv_timeout` again.
    pending_term_event: Option<Event>,
    /// Phase 8 back-stack: `NavigateBack` pops the most-recent entry
    /// and restores it.  A new link-follow clears `nav_forward`
    /// (browser semantics).
    nav_back: Vec<NavEntry>,
    /// Phase 8 forward-stack: `NavigateBack` pushes the current state
    /// here so `NavigateForward` can redo the navigation.
    nav_forward: Vec<NavEntry>,
    /// Phase 8 — target of the link currently under the mouse
    /// pointer, updated on every `MouseEventKind::Moved` event.
    /// Phase 9 will render this (plus the link's `title`) on the hint
    /// line.  Until then the field is wired through but not displayed.
    hovered_link: Option<LinkTarget>,
    /// Phase 9 — transient message overlayed on the hint line.  Non-
    /// error kinds auto-expire after `config.editor.transient_ms`;
    /// errors stick until dismissed.  Set by [`App::flash`] from any
    /// code path that wants a one-shot notification.
    transient: Option<TransientMessage>,
    /// Live keymap used for input dispatch.  Built once at startup
    /// from `keybindings`; mutated in place by the keybinds overlay
    /// so rebinds take effect immediately.
    keymap: Option<KeyMap>,
    /// Set by the settings overlay's "Open config.toml in default
    /// editor" action.  Consumed by the run loop, which has the
    /// `Terminal` handle needed to suspend / resume the TUI around
    /// the editor process.
    pending_open_config_in_editor: bool,
    /// Set by the palette's `OpenInExternalEditor` action.  Same
    /// motivation as `pending_open_config_in_editor` — the dispatch
    /// site doesn't have the `Terminal` handle, so the run loop
    /// drains the flag.
    pending_open_file_in_editor: bool,
    /// Set by the export-success modal's "Open in default editor"
    /// button.  Drained by the run loop, which then suspends the TUI
    /// and runs `$VISUAL` / `$EDITOR` on this path.  After the editor
    /// exits the active theme is reloaded so user edits take effect.
    pub(crate) pending_open_theme_in_editor: Option<std::path::PathBuf>,
    /// Pause flag for the crossterm read thread.  When `true`, the
    /// thread sleeps instead of polling stdin, releasing it to a
    /// child process (e.g. `$EDITOR` shelled out from the settings
    /// overlay).  Without this, our read thread and the editor would
    /// both try to consume the same bytes from the controlling
    /// terminal, causing dropped keystrokes (lag) and stray escape
    /// sequences leaking into the editor (the `1;rgb:...` artifact
    /// users saw at the top of their `config.toml` after closing
    /// neovim was an OSC 11 background-color response).
    /// Initialised in [`Self::run`] alongside the read-thread spawn.
    read_paused: Option<Arc<AtomicBool>>,
    /// Phase 9 — active hint-line prompt (first consumer is Phase 11).
    /// Renders in place of the default hint chords; Escape dismisses.
    hint_prompt: Option<HintPrompt>,
    /// Active stack of trait-based modals.  Adding a modal is one
    /// `modal_stack.push(Box::new(...))` call; render priority and
    /// input absorption are stack-order driven.
    modal_stack: ModalStack,
    /// True when the user named a file that did not exist on disk;
    /// `App::run` flashes "[New File]" once at startup so the user
    /// understands the buffer is empty and saving will create the file.
    started_with_new_file: bool,
}

impl App {
    /// Create the app, loading the file if one is given.
    pub fn new(
        mut config: Config,
        keybindings: KeyBindingOverrides,
        theme_file: ThemeFile,
        file_path: Option<PathBuf>,
        capabilities: Capabilities,
        config_warnings: Vec<ConfigWarning>,
    ) -> Result<Self> {
        // Leak the theme so it can be stored as `&'static Theme`.
        //
        // Why `'static`: `Theme` is read from many places (App,
        // every widget, `EditorState`) on the hot render path.
        // Threading a lifetime parameter would propagate through
        // dozens of types; wrapping in `Arc<Theme>` adds a refcount
        // bump on every clone and a deref on every read.  `'static`
        // sidesteps both — readers just hold a plain reference.
        //
        // Why leak: `'static` requires a backing allocation that
        // outlives the program, and `Box::leak` is the simplest way
        // to promote a heap allocation to that lifetime.  The
        // process owns the leaked memory until exit; the OS
        // reclaims it on termination.  `Theme` is a fixed-size
        // struct of ~100 `Style` values (~few KB), so the one-shot
        // startup leak is negligible.
        //
        // Live updates leak too: `apply_active_theme` (theme cycle
        // in the settings overlay, post-editor reload) leaks a
        // fresh `Theme` each time so `self.theme` can be reassigned
        // while satisfying `'static`.  Theme changes are
        // user-initiated and rare, so the accumulated cost stays
        // small across a session — see `apply_active_theme` for the
        // full rationale and the alternatives considered.
        //
        // `Theme::from_file` handles the monochrome fallback
        // internally so `NoColor` terminals never emit color
        // escapes regardless of the theme file's contents.
        let monochrome = capabilities.color_depth == ColorDepth::NoColor;
        let theme: &'static Theme = Box::leak(Box::new(Theme::from_file(&theme_file, monochrome)));

        // Table buttons depend on mouse reporting; disable them on
        // terminals that don't deliver mouse events so we never render inert
        // gutter glyphs.
        if !capabilities.mouse {
            config.table.show_buttons = false;
        }

        // Treat a non-existent path the same way `vim` / `nano` do:
        // open an empty buffer associated with the path so the first
        // save creates the file.  A "[New File]" flash is queued for
        // the run loop so the user is told what happened.
        let mut started_with_new_file = false;
        let buffer = match &file_path {
            Some(path) if path.exists() => Buffer::load_file(path)?,
            Some(path) => {
                started_with_new_file = true;
                Buffer::for_new_file(path)
            }
            None => Buffer::new(),
        };

        // Pass the probed font-size through so the renderer can compute
        // aspect-aware row counts for decoded images.  Fall back to
        // ratatui-image's Halfblocks default (10, 20) when no image
        // picker was detected — any image render will be a no-op on
        // those terminals anyway (capabilities.image_protocol == None).
        let image_font_size = capabilities
            .image_picker
            .as_ref()
            .map(|p| p.font_size())
            .unwrap_or((10, 20));
        let mut editor = EditorState::new_with_image_config(
            buffer,
            theme,
            config.editor.preserve_blank_lines,
            config.editor.visual_line_nav,
            config.images.max_height,
            config.images.max_width,
            image_font_size,
        );
        editor.tab_width = config.editor.tab_width;
        // When the user has persisted `images.enabled = "never"`, image
        // blocks must collapse to just the `[Image: alt]` placeholder —
        // no reserved rows beneath.  The `Ask` / `Always` paths leave
        // the layout reserved so the prompt / live decode populates the
        // area; the declined-session flip happens in the prompt handler.
        let images_off = matches!(config.images.enabled, crate::config::ImagesEnabled::Never);
        let diagrams_off = matches!(
            config.diagrams.enabled,
            crate::config::DiagramsEnabled::Never
        );
        if images_off {
            editor.images_enabled = false;
        }
        if diagrams_off {
            editor.diagrams_enabled = false;
        }
        editor.set_row_striping(config.table.row_striping);
        editor.set_big_h1(config.editor.big_h1);
        if images_off || diagrams_off {
            editor.refresh_parsed();
        }

        // PreviewView borrows `editor.parsed.lines` at render time, so
        // no per-event clone is needed and the constructor is now
        // parameterless.  This removed the dominant per-event allocation
        // hotspot on large preview-mode documents.
        let view_state = EditorViewState::new();

        // Build startup-time modals.  Each is optional — `None` when
        // its precondition isn't satisfied (no warnings, capability
        // notice suppressed, document has no images, etc.).
        //
        // The first-run welcome modal subsumes the four legacy startup
        // prompts (capability notice, images-enabled, remote-image,
        // diagrams) — they're skipped while the welcome is still
        // pending so the user is never double-prompted.
        let welcome_modal = modal::WelcomeModal::from_state(&capabilities, &config);
        let suppress_legacy_prompts = welcome_modal.is_some();
        let config_warning_modal = modal::ConfigWarningModal::from_warnings(&config_warnings);
        let startup_notice = if suppress_legacy_prompts {
            None
        } else {
            modal::StartupNoticeModal::from_capabilities(&capabilities, &config)
        };
        let images_enabled_prompt = if suppress_legacy_prompts {
            None
        } else {
            modal::ImagesEnabledPromptModal::from_state(&editor, &config)
        };
        let diagrams_enabled_prompt = if suppress_legacy_prompts {
            None
        } else {
            modal::DiagramsEnabledPromptModal::from_state(&editor, &config)
        };
        let remote_image_prompt = if suppress_legacy_prompts {
            None
        } else {
            modal::RemoteImagePromptModal::from_state(&editor, &config)
        };
        let wheel_step = config.editor.mouse_scroll_lines;

        // Push the queued startup-time modals onto the stack in
        // reverse-priority order so the highest-priority one is on
        // top.  Order shown to the user when present: config-warning →
        // welcome → startup-notice → images-enabled → diagrams-enabled
        // → remote-image.  The legacy prompts (everything below
        // welcome) are suppressed via `suppress_legacy_prompts` whenever
        // the welcome itself is queued, so on a launch that shows the
        // welcome the user only sees config-warning + welcome.
        let mut modal_stack = ModalStack::new();
        if let Some(m) = remote_image_prompt {
            modal_stack.push(Box::new(m));
        }
        if let Some(m) = diagrams_enabled_prompt {
            modal_stack.push(Box::new(m));
        }
        if let Some(m) = images_enabled_prompt {
            modal_stack.push(Box::new(m));
        }
        if let Some(m) = startup_notice {
            modal_stack.push(Box::new(m));
        }
        if let Some(m) = welcome_modal {
            modal_stack.push(Box::new(m));
        }
        if let Some(m) = config_warning_modal {
            modal_stack.push(Box::new(m));
        }

        // Phase 17 — warm the diagram pipeline's font caches off the
        // critical path.  Two caches load fonts on first call:
        //   * `mermaid_rs_renderer`'s internal fontdb (for text layout
        //     metrics during SVG generation),
        //   * our own shared `fontdb::Database` used by `usvg` when
        //     rasterising the SVG to PNG.
        // Both scan OS font dirs (~100–300 ms each, worse cold).
        // `diagram::warm_fontdb` primes both so the first real diagram
        // render doesn't pay them.  Without this warmup (or with
        // per-render loads as the previous implementation did), a
        // document with N diagrams spawns N concurrent font scans —
        // the dominant source of initial-load lag.
        // Skipped when images are configured as `Never` — no diagram
        // will ever decode, so the warmup would be wasted IO.
        if !matches!(
            config.diagrams.enabled,
            crate::config::DiagramsEnabled::Never
        ) {
            std::thread::spawn(crate::diagram::warm_fontdb);
        }

        Ok(Self {
            config,
            keybindings,
            theme,
            capabilities,
            file_path,
            editor,
            view_state,
            should_quit: false,
            session_images_enabled: None,
            session_diagrams_enabled: None,
            mouse: MouseDispatcher::with_wheel_step(wheel_step),
            drag_target: None,
            scrollbar_hover: false,
            last_pointer_shape: PointerShape::Default,
            session_allow_remote: false,
            app_tx: None,
            last_scroll_at: None,
            last_draw_at: None,
            last_area_width: 0,
            images_dirty: false,
            needs_draw: true,
            resize_quiesce_at: None,
            pending_term_event: None,
            nav_back: Vec::new(),
            nav_forward: Vec::new(),
            hovered_link: None,
            transient: None,
            keymap: None,
            pending_open_config_in_editor: false,
            pending_open_file_in_editor: false,
            pending_open_theme_in_editor: None,
            read_paused: None,
            hint_prompt: None,
            modal_stack,
            started_with_new_file,
        })
    }

    /// Drain any additional `ImageReady` events already sitting in
    /// `rx` without blocking.  Called after handling the first
    /// `ImageReady` in a burst so the main loop processes all
    /// simultaneous decode completions as one unit, followed by a
    /// single `refresh_parsed` on the next iteration.  Non-image
    /// events are left in the channel for the next loop iteration to
    /// handle normally.
    fn drain_pending_image_ready(&mut self, rx: &mpsc::Receiver<AppEvent>) {
        loop {
            match rx.try_recv() {
                Ok(AppEvent::ImageReady(Ok(loaded))) => {
                    self.editor.images.set_decoded_with_prebuilt(
                        &loaded.url,
                        loaded.image,
                        loaded.scratch,
                    );
                    self.images_dirty = true;
                }
                Ok(AppEvent::ImageReady(Err((url, message)))) => {
                    tracing::debug!(target: "image", %url, %message, "image decode failed");
                    self.editor.images.set_failed(&url, message);
                    // A failure collapses the block's reserved rows to 1
                    // (see `ImageCache::reserved_rows`), so the parsed
                    // doc must be rebuilt to drop the placeholder's
                    // extra blank rows.
                    self.images_dirty = true;
                }
                Ok(AppEvent::ProtocolReady(Ok(resp))) => {
                    self.editor.images.apply_resize_response(resp);
                }
                Ok(AppEvent::ProtocolReady(Err(err))) => {
                    tracing::debug!(target: "image", %err, "encoder request failed");
                    // Keep the pending FIFO balanced — see ImageCache.
                    self.editor.images.drop_pending_front();
                }
                Ok(AppEvent::LinkOpenResult(result)) => {
                    // Phase 8: `open::that` finished in a worker.
                    // Phase 9: surface failures on the hint line as a
                    // sticky error so the user knows the click did not
                    // result in a navigation.
                    if let Err(msg) = result {
                        tracing::warn!(target: "link", error = %msg, "link open failed");
                        self.notify(format!("Link open failed: {msg}"), ModalKind::Error);
                        self.needs_draw = true;
                    }
                }
                // A Term event pulled via `try_recv` cannot be put back
                // into the channel, so stash it for the next iteration
                // and stop draining.  The main loop checks this field
                // before calling `recv_timeout` again so events are
                // still processed in the original channel order.
                Ok(AppEvent::Term(e)) => {
                    self.pending_term_event = Some(e);
                    break;
                }
                Err(_) => break,
            }
        }
    }

    /// Expose detected capabilities to later phases (mouse, images, etc.).
    #[allow(dead_code)]
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// Run the event loop until the user quits.
    ///
    /// The body of this method is intentionally minimal: every step is
    /// a named call into [`event_loop`].  Background threads, frame
    /// preparation, drawing, and event dispatch each live in their own
    /// method on `App`; this loop reads as a flat sequence of those
    /// steps so the control flow is legible at a glance.
    pub fn run(&mut self, mut terminal: Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        self.startup_pointer_hint();
        let rx = self.spawn_event_threads();
        self.build_keymap_if_needed()?;
        if self.started_with_new_file {
            self.flash("[New File]", MessageKind::Info);
        }

        loop {
            self.tick_timers();
            self.coalesce_image_updates();

            let term_size = terminal.size()?;
            let dims = self.compute_doc_dims(term_size);
            self.prepare_viewport(&dims);

            let since_draw = self.last_draw_at.map(|t| t.elapsed());
            if self.should_draw(since_draw) {
                self.draw_frame(&mut terminal)?;
            }

            let event = match self.next_event(&rx, since_draw) {
                Some(e) => e,
                None => {
                    if self.should_quit {
                        break;
                    }
                    continue;
                }
            };

            if matches!(event, Event::Resize(_, _)) {
                self.on_resize();
                continue;
            }

            if !self.modal_stack.is_empty() {
                self.dispatch_modal_event(&event, &dims, &mut terminal, &rx);
                if self.should_quit {
                    break;
                }
                continue;
            }

            if let Event::Mouse(mouse_event) = event {
                self.dispatch_mouse_event(mouse_event, &dims);
                continue;
            }

            if let Event::Paste(text) = event {
                self.dispatch_paste(text, &dims);
                continue;
            }

            self.dispatch_key_event(event, &dims, &mut terminal, &rx);
            if self.should_quit {
                break;
            }
        }

        Ok(())
    }

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

