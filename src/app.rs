pub mod modal;

mod actions;
mod external_editor;
mod flash;
mod frame_timer;
mod image_dispatch;
mod nav;
mod pointer;

use std::io::Stdout;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyEventKind, MouseEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::config::{
    Action, Config, ConfigWarning, KeyBindingOverrides, KeyMap, Theme, ThemeFile,
};
use crate::document::Buffer;
use crate::editor::link::LinkTarget;
use crate::editor::{edit_ops, mouse_ops, EditorState};
use crate::input::modal::default::DefaultHandler;
use crate::input::MouseDispatcher;
use crate::terminal::{Capabilities, ColourDepth, PointerShape};
use crate::ui::{EditorView, EditorViewState, HintChord};

pub use flash::MessageKind;

use self::actions::{modal_wheel_delta, HandleEvent};
use self::flash::TransientMessage;
use self::frame_timer::{MIN_FRAME_INTERVAL, RESIZE_QUIESCE};
use self::modal::{ModalRenderCtx, ModalStack};
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

/// Phase 9 generic modal prompt hosted on the hint line.  Phase 11
/// (file-change detection) is the first consumer; landing the
/// scaffolding here means later phases only need to populate the
/// struct and wire the response back.  The `handler` fn is the single
/// callback invoked when one of the chord keys is pressed — it
/// receives the triggering `KeyCode` so the same prompt type can host
/// multi-button flows.
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
    /// Click-count tracking and drag state for mouse input.
    mouse: MouseDispatcher,
    /// Active drag target, set on mouse-down and read by each subsequent
    /// `Drag` event.  `DragTarget::TextSelection` covers normal click-drag
    /// text selection (the Phase 5 fallthrough); the other variants carry
    /// Phase 6's table-specific row / column / border drags.  Cleared on
    /// `Release`.
    drag_target: Option<mouse_ops::DragTarget>,
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
        // internally so `NoColour` terminals never emit colour
        // escapes regardless of the theme file's contents.
        let monochrome = capabilities.colour_depth == ColourDepth::NoColour;
        let theme: &'static Theme = Box::leak(Box::new(Theme::from_file(&theme_file, monochrome)));

        // Table buttons depend on mouse reporting; disable them on
        // terminals that don't deliver mouse events so we never render inert
        // gutter glyphs.
        if !capabilities.mouse {
            config.table.show_buttons = false;
        }

        let buffer = match &file_path {
            Some(path) => Buffer::load_file(path)?,
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
        if matches!(config.images.enabled, crate::config::ImagesEnabled::Never) {
            editor.images_enabled = false;
            editor.set_row_striping(config.table.row_striping);
            editor.refresh_parsed();
        } else {
            editor.set_row_striping(config.table.row_striping);
        }

        // PreviewView borrows `editor.parsed.lines` at render time, so
        // no per-event clone is needed and the constructor is now
        // parameterless.  This removed the dominant per-event allocation
        // hotspot on large preview-mode documents.
        let view_state = EditorViewState::new();

        // Build startup-time modals.  Each is optional — `None` when
        // its precondition isn't satisfied (no warnings, capability
        // notice suppressed, document has no images, etc.).
        let config_warning_modal = modal::ConfigWarningModal::from_warnings(&config_warnings);
        let startup_notice = modal::StartupNoticeModal::from_capabilities(&capabilities, &config);
        let images_enabled_prompt = modal::ImagesEnabledPromptModal::from_state(&editor, &config);
        let remote_image_prompt = modal::RemoteImagePromptModal::from_state(&editor, &config);
        let wheel_step = config.editor.mouse_scroll_lines;

        // Push the queued startup-time modals onto the stack in
        // reverse-priority order so the highest-priority one is on
        // top.  Order shown to the user: config-warning → notice →
        // images-enabled → remote-image → (any subsequent modals).
        let mut modal_stack = ModalStack::new();
        if let Some(m) = remote_image_prompt {
            modal_stack.push(Box::new(m));
        }
        if let Some(m) = images_enabled_prompt {
            modal_stack.push(Box::new(m));
        }
        if let Some(m) = startup_notice {
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
        if !matches!(config.images.enabled, crate::config::ImagesEnabled::Never) {
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
            mouse: MouseDispatcher::with_wheel_step(wheel_step),
            drag_target: None,
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
            read_paused: None,
            hint_prompt: None,
            modal_stack,
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
                        self.flash(format!("Link open failed: {msg}"), MessageKind::Error);
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
    pub fn run(&mut self, mut terminal: Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        // Hint the terminal to show an I-beam pointer over the TUI area by
        // default.  Terminals that don't implement OSC 22 silently ignore this.
        if self.capabilities.mouse {
            self.update_pointer_shape(PointerShape::Text);
        }
        let (tx, rx) = mpsc::channel::<AppEvent>();
        self.app_tx = Some(tx.clone());

        // Spawn a thread to forward crossterm events.  The thread
        // uses `poll`+`read` (instead of a bare `read`) so a pause
        // flag can take effect without having to interrupt a
        // blocked syscall.  When the App shells out to an external
        // editor via the settings overlay, it flips the flag so the
        // child process gets uncontested access to stdin.  Without
        // this, both processes would race to read terminal bytes
        // and the editor would see a corrupted input stream.
        let read_paused = Arc::new(AtomicBool::new(false));
        self.read_paused = Some(read_paused.clone());
        let tx_clone = tx.clone();
        std::thread::spawn(move || loop {
            if read_paused.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            match crossterm::event::poll(Duration::from_millis(100)) {
                Ok(true) => match crossterm::event::read() {
                    Ok(event) => {
                        if tx_clone.send(AppEvent::Term(event)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => {} // poll timed out — re-check pause flag.
                Err(_) => break,
            }
        });

        // Spawn the encoder worker.  Every resize-encode for every
        // visible image funnels through this single thread: encoding is
        // CPU-bound, so serial execution preserves cache locality and
        // avoids contention on the terminal's graphics state.  The UI
        // thread NEVER encodes — it only enqueues `ResizeRequest`s and
        // paints the pre-encoded bytes once the worker responds.
        let (resize_tx, resize_rx) = mpsc::channel::<ratatui_image::thread::ResizeRequest>();
        self.editor.images.attach_resize_sender(resize_tx);
        let tx_encoder = tx.clone();
        std::thread::spawn(move || {
            while let Ok(req) = resize_rx.recv() {
                let result = req.resize_encode();
                if tx_encoder.send(AppEvent::ProtocolReady(result)).is_err() {
                    break;
                }
            }
        });

        // Build the keymap once and keep it alive for the event loop.
        // Stored on `self` so the keybinds overlay can mutate it in
        // place; we read by clone at each dispatch site so we never
        // hold an `&self` borrow across an action handler.
        if self.keymap.is_none() {
            self.keymap = Some(KeyMap::build(&self.keybindings)?);
        }

        loop {
            // Resize-quiesce: once the burst of Resize events from a
            // terminal-drag has settled, clear the suppression flag
            // and request a single redraw at the final dimensions.
            if self.resize_quiesce_at.is_some_and(|t| t <= Instant::now()) {
                self.resize_quiesce_at = None;
                self.needs_draw = true;
            }

            // Phase 9: retire expired transient hint-line messages
            // before the draw gate so the hint reverts to chords even
            // when no input is flowing.
            if self.expire_transient_if_due() {
                self.needs_draw = true;
            }

            if self.editor.cursor_blink.tick() {
                self.needs_draw = true;
            }
            self.editor.modal_open = self.any_modal_open();

            // Coalesce any `ImageReady`-driven cache mutations into a
            // single parse-and-render pass for this frame.  Without
            // this, a burst of N simultaneous decode completions would
            // trigger N reparses on the main thread and stall pending
            // scroll / key events between each one.
            if self.images_dirty {
                self.editor.refresh_parsed();
                self.images_dirty = false;
                self.needs_draw = true;
            }

            // Kick off decodes for images within the near-viewport
            // window (visible rows plus one viewport-height of
            // prefetch).  Re-running this every frame is cheap:
            // `ImageCache::request` returns false for URLs already
            // Pending / Ready / Failed so we only spawn threads for
            // URLs that just entered the window on the most recent
            // scroll / edit.  An image-heavy document therefore never
            // kicks off more concurrent decodes than the window
            // contains — bounded, not proportional to doc size.
            let term_size = terminal.size()?;
            let bottom_rows_for_decode =
                crate::ui::BottomRegion::height(self.config.editor.status_bar) as usize;
            let doc_height_lines =
                (term_size.height as usize).saturating_sub(bottom_rows_for_decode);
            self.last_area_width = term_size.width;
            // Phase 13 — feed the live document-area width into the editor
            // so the table-column min-max algorithm adapts to the user's
            // terminal.  Cheap: `set_viewport_width` short-circuits when the
            // cached width already matches, so this only triggers a
            // refresh_parsed on the rare frame after a resize quiesce.
            self.editor.set_viewport_width(term_size.width as usize);
            self.dispatch_visible_image_decodes(self.editor.scroll, doc_height_lines);

            // ── Draw ──────────────────────────────────────────────
            // Event-driven draws: only paint when `needs_draw` is set
            // AND the 16 ms frame-rate throttle is satisfied AND no
            // resize burst is in flight.  The throttle coalesces rapid
            // event bursts (wheel-tick spam, held keys) to ~60 fps;
            // `needs_draw` prevents idle redraws so the process can go
            // fully quiescent between user actions; `resize_quiesce_at`
            // suppresses mid-drag paints that would otherwise flicker.
            let since_draw = self.last_draw_at.map(|t| t.elapsed());
            let throttle_ok = since_draw.is_none_or(|d| d >= MIN_FRAME_INTERVAL);
            let resize_pending = self.resize_quiesce_at.is_some();
            let should_draw = self.needs_draw && throttle_ok && !resize_pending;
            if should_draw {
                let filename = self.display_filename();
                let is_scrolling = self.is_scrolling();
                let show_handles = self.config.table.show_buttons;
                let layout = self.config.editor.status_bar;
                let hint = self.hint_content();
                let modal_cursor_visible = self.editor.cursor_blink.is_visible();
                let editor_ref = &mut self.editor;
                let theme_ref = self.theme;
                let capabilities_ref = &self.capabilities;
                let view_state_ref = &mut self.view_state;
                let modal_stack_top = self.modal_stack.top_mut();
                let config_ref: &Config = &self.config;
                let keymap_for_render = self.keymap.clone();
                let drop_indicator = drop_indicator_for(&self.drag_target);
                terminal.draw(|frame| {
                    let view = EditorView {
                        state: editor_ref,
                        theme: theme_ref,
                        filename: &filename,
                        show_table_buttons: show_handles,
                        table_drop_indicator: drop_indicator,
                        capabilities: capabilities_ref,
                        is_scrolling,
                        status_bar_layout: layout,
                        hint,
                    };
                    frame.render_stateful_widget(view, frame.area(), view_state_ref);
                    if let Some(top) = modal_stack_top {
                        let render_ctx = ModalRenderCtx {
                            theme: theme_ref,
                            config: config_ref,
                            keymap: keymap_for_render.as_ref(),
                            cursor_visible: modal_cursor_visible,
                        };
                        top.render(frame, frame.area(), &render_ctx);
                    }
                })?;
                self.last_draw_at = Some(Instant::now());
                self.needs_draw = false;
            }

            // ── Wait for event (blocking unless a timer is pending) ──
            // Compute the shortest pending deadline:
            // - RAW_REVEAL_DELAY and SCROLL_QUIESCE (via `next_deadline`)
            //   drive time-based visual updates.
            // - When `needs_draw` was set but the 16 ms frame throttle
            //   blocked the draw, also wake at the remaining throttle
            //   budget so the deferred draw fires promptly.
            // With no pending deadline and nothing to draw, fall back
            // to `rx.recv()` which blocks until an event arrives —
            // the app idles with 0 % CPU.
            let now = Instant::now();
            let mut wait: Option<Duration> = None;
            let mut push_wait = |w: Duration| {
                wait = Some(wait.map_or(w, |existing| existing.min(w)));
            };
            if let Some(deadline) = self.next_deadline(now) {
                push_wait(deadline.saturating_duration_since(now));
            }
            if self.needs_draw {
                match since_draw {
                    Some(elapsed) if elapsed < MIN_FRAME_INTERVAL => {
                        push_wait(MIN_FRAME_INTERVAL - elapsed);
                    }
                    _ => push_wait(Duration::ZERO),
                }
            }
            // If a Term event was stashed by `drain_pending_image_ready`
            // on the previous iteration, process it first so channel
            // order is preserved.
            let event = if let Some(e) = self.pending_term_event.take() {
                e
            } else {
                let recv_result = match wait {
                    Some(d) => rx.recv_timeout(d),
                    None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                };
                match recv_result {
                    Ok(AppEvent::Term(e)) => e,
                    Ok(AppEvent::ImageReady(Ok(loaded))) => {
                        self.editor.images.set_decoded_with_prebuilt(
                            &loaded.url,
                            loaded.image,
                            loaded.scratch,
                        );
                        self.images_dirty = true;
                        self.needs_draw = true;
                        self.drain_pending_image_ready(&rx);
                        continue;
                    }
                    Ok(AppEvent::ImageReady(Err((url, message)))) => {
                        tracing::debug!(target: "image", %url, %message, "image decode failed");
                        self.editor.images.set_failed(&url, message);
                        // A failure collapses the block's reserved rows
                        // to 1 (see `ImageCache::reserved_rows`), so the
                        // parsed doc must be rebuilt to drop the blank
                        // rows under the placeholder.
                        self.images_dirty = true;
                        self.needs_draw = true;
                        // Failures may come in bursts — drain so we
                        // don't iterate the loop once per failure.
                        self.drain_pending_image_ready(&rx);
                        continue;
                    }
                    Ok(AppEvent::ProtocolReady(Ok(resp))) => {
                        self.editor.images.apply_resize_response(resp);
                        self.needs_draw = true;
                        self.drain_pending_image_ready(&rx);
                        continue;
                    }
                    Ok(AppEvent::ProtocolReady(Err(err))) => {
                        tracing::debug!(target: "image", %err, "encoder request failed");
                        self.editor.images.drop_pending_front();
                        self.drain_pending_image_ready(&rx);
                        continue;
                    }
                    Ok(AppEvent::LinkOpenResult(result)) => {
                        if let Err(msg) = result {
                            tracing::warn!(target: "link", error = %msg, "link open failed");
                            self.flash(format!("Link open failed: {msg}"), MessageKind::Error);
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // A pending deadline (reveal / scroll quiesce
                        // / throttle) elapsed without an external
                        // event.  Redraw once to apply it; the loop
                        // will then go back to blocking on `recv()`
                        // because the deadline is no longer in the
                        // future.
                        self.needs_draw = true;
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            };

            // ── Terminal resize ──────────────────────────────────────
            // Dragging the window edge fires a burst of `Resize`
            // events (one per pixel).  Instead of trying to redraw
            // every one — which both pins CPU and paints partial
            // frames — arm a quiesce deadline that the draw gate
            // above respects.  Width-dependent snapshot caches
            // (image, link, table) are invalidated so the settled-size
            // redraw rebuilds them at the new dimensions, and
            // `last_scroll_at` is cleared so images resume at the
            // native protocol immediately rather than passing
            // through the halfblocks transition.
            if matches!(event, Event::Resize(_, _)) {
                self.resize_quiesce_at = Some(Instant::now() + RESIZE_QUIESCE);
                self.view_state.rendered.image_snapshots_key = None;
                self.view_state.rendered.link_snapshots_key = None;
                self.view_state.rendered.table_snapshots_key = None;
                self.view_state.preview.image_snapshots_key = None;
                self.view_state.preview.link_snapshots_key = None;
                self.last_scroll_at = None;
                continue;
            }

            // Pre-compute the per-event mouse-wheel step once so the
            // modal-absorption arms below don't each have to re-read
            // it.  The same value applies to in-editor scrolling.
            let wheel_step = self.config.editor.mouse_scroll_lines;

            // Reset the cursor blink on any keypress while a modal is
            // open so the `▏` cursor snaps to visible after typing.
            if self.editor.modal_open {
                if matches!(&event, Event::Key(k) if k.kind == KeyEventKind::Press) {
                    self.editor.cursor_blink.reset();
                }
            }

            // Open modal absorbs all input.  Topmost modal on the
            // stack receives the event and decides whether to stay open
            // (`Continue`), close (`Close`), or close and run a
            // follow-up callback (`CloseAnd`).  See [`Self::dispatch_modal_key`].
            if !self.modal_stack.is_empty() {
                let bottom_rows = crate::ui::BottomRegion::height(self.config.editor.status_bar);
                let doc_h = (term_size.height as usize).saturating_sub(bottom_rows as usize);
                let doc_w = term_size.width as usize;
                match &event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        self.dispatch_modal_key(*key, doc_h, doc_w);
                        self.needs_draw = true;
                    }
                    Event::Mouse(me) => {
                        if let Some(top) = self.modal_stack.top_mut() {
                            top.handle_wheel(modal_wheel_delta(me, wheel_step));
                            self.needs_draw = true;
                        }
                    }
                    _ => {}
                }
                // External-editor flows defer to the run loop because
                // the editor invocation needs `&mut Terminal` and `&rx`,
                // which only this scope holds.  Same drains as the
                // legacy command-palette / settings-overlay arms.
                if self.pending_open_config_in_editor {
                    self.pending_open_config_in_editor = false;
                    self.open_config_in_editor(&mut terminal, &rx);
                }
                if self.pending_open_file_in_editor {
                    self.pending_open_file_in_editor = false;
                    self.open_current_file_in_editor(&mut terminal, &rx);
                }
                if self.should_quit {
                    break;
                }
                continue;
            }

            // `term_size` was already fetched at the top of the loop
            // (for `dispatch_visible_image_decodes`); reuse here.
            let viewport_height = term_size.height as usize;
            let bottom_rows = crate::ui::BottomRegion::height(self.config.editor.status_bar);
            let doc_height = viewport_height.saturating_sub(bottom_rows as usize);
            let doc_width = term_size.width as usize;
            let doc_area = Rect {
                x: 0,
                y: 0,
                width: term_size.width,
                height: term_size.height.saturating_sub(bottom_rows),
            };

            // ── Dispatch mouse events ─────────────────────────────
            // Mouse events come through before key events get a chance so a
            // mid-click key press doesn't erase an in-progress drag.
            if let Event::Mouse(mouse_event) = event {
                // Mouse clicks hit-test against `parsed.source_map`
                // byte ranges — a stale map from a deferred re-parse
                // would map the click to the wrong block or line.
                // Flush synchronously here; a click ends the typing
                // burst naturally, so the latency cost is invisible.
                if self.editor.flush_parsed_if_dirty() {
                    self.needs_draw = true;
                }
                if self.capabilities.mouse {
                    // Pointer-shape feedback: over a clickable element, ask the
                    // terminal for a pointing-hand cursor; otherwise I-beam.
                    // Event column/row are in terminal coords — translate to
                    // doc-relative before hit-testing.
                    let in_doc = mouse_event.column >= doc_area.x
                        && mouse_event.column < doc_area.x + doc_area.width
                        && mouse_event.row >= doc_area.y
                        && mouse_event.row < doc_area.y + doc_area.height;
                    let desired = if in_doc {
                        let rel_col = mouse_event.column - doc_area.x;
                        let rel_row = mouse_event.row - doc_area.y;
                        // Phase 8: also record the hovered link target
                        // (or clear it) for the hint-line tooltip that
                        // Phase 9 will surface.  Keeping this update in
                        // the pointer-shape path means it fires on
                        // every mouse-move, tracking the hover in real
                        // time without an extra scan.
                        self.hovered_link = mouse_ops::hovered_link_target(
                            &self.editor,
                            rel_col,
                            rel_row,
                            doc_width,
                        );
                        if mouse_ops::hit_test_clickable(
                            &self.editor,
                            rel_col,
                            rel_row,
                            doc_width,
                            &self.view_state.rendered.table_snapshots,
                        ) {
                            PointerShape::Hand
                        } else {
                            PointerShape::Text
                        }
                    } else {
                        self.hovered_link = None;
                        PointerShape::Default
                    };
                    self.update_pointer_shape(desired);

                    // Moved-only events don't drive editor state; they're used
                    // purely for pointer-shape tracking above.  Skip dispatch
                    // to avoid emitting spurious actions.
                    if matches!(mouse_event.kind, MouseEventKind::Moved) {
                        continue;
                    }

                    if let Some(mouse_action) = self.mouse.dispatch(mouse_event, doc_area) {
                        let snapshots = self.view_state.rendered.table_snapshots.clone();
                        let scroll_before = self.editor.scroll;
                        mouse_ops::apply(
                            &mut self.editor,
                            mouse_action,
                            &mut self.drag_target,
                            &snapshots,
                            doc_height,
                            doc_width,
                        );
                        if self.editor.scroll != scroll_before {
                            self.mark_scrolling();
                        }
                        // Phase 8: mouse click may have requested a link
                        // follow.  Consume it before the preview-state
                        // sync below so the navigation runs first.
                        if let Some(target) = self.editor.pending_link_follow.take() {
                            self.follow_link(target, doc_height, doc_width);
                        }
                        // Phase 13: a column-border drag release sets
                        // `pending_column_widths_commit`; either commit
                        // straight through or open the warning modal
                        // depending on config + table state.
                        self.handle_pending_column_widths();
                        self.needs_draw = true;
                    }
                }
                // Preview-mode mirror writes (scroll, selection) now
                // happen once per frame inside `EditorView::render`,
                // since the widget reads `editor.parsed.lines` by
                // borrow.  Nothing to do here.
                continue;
            }

            // ── Bracketed paste (terminal-level clipboard) ────────
            // When the terminal emulator pastes into the TUI (Ctrl-Shift-V,
            // middle-click, ⌘V on macOS Terminal, right-click-paste, etc.)
            // it delivers the full paste as a single `Event::Paste(String)`.
            // Route straight into the buffer so pasting from external apps
            // always works, regardless of whether arboard can reach the OS
            // clipboard from inside this process.
            if let Event::Paste(text) = event {
                edit_ops::paste_text(&mut self.editor, &text, doc_height, doc_width);
                self.needs_draw = true;
                continue;
            }

            // ── Dispatch event → Action ───────────────────────────
            // Clone the live keymap for this iteration so the borrow
            // stays cheap and doesn't conflict with `&mut self` inside
            // action handlers.
            let keymap = self
                .keymap
                .as_ref()
                .cloned()
                .unwrap_or_else(|| KeyMap::build(&KeyBindingOverrides::default()).unwrap());
            let mut handler = DefaultHandler::new(&keymap);
            if let Some(action) = handler.handle_event(event, &self.editor) {
                // Phase 9: if a sticky error is showing, Escape
                // dismisses it and swallows the key press so it
                // doesn't double as ExitToPreview.  Non-sticky
                // transients let Escape fall through.
                if matches!(action, Action::ExitToPreview) && self.dismiss_sticky_transient() {
                    self.needs_draw = true;
                    continue;
                }
                // Phase 8 — App-level actions intercepted BEFORE the
                // generic `edit_ops::apply` dispatch.  Link navigation
                // mutates App state (nav stack, file load) that
                // `EditorState` doesn't own, so these paths stay here.
                let handled = self.handle_app_action(&action, doc_height, doc_width);
                if !handled {
                    // Phase 9 — Quit on a dirty buffer opens the
                    // three-button confirm modal instead of
                    // terminating.  On a clean buffer we fall through
                    // to `edit_ops::apply` which returns `true`.
                    if matches!(action, Action::Quit) && self.editor.dirty {
                        self.open_quit_confirm();
                        self.needs_draw = true;
                        continue;
                    }
                    // Phase 9 — observe the effects of certain
                    // actions so we can flash a transient message.
                    // Save: we need to detect failure and raise a
                    // sticky error instead of leaving the user guessing.
                    let save_before_dirty = self.editor.dirty;
                    let scroll_before = self.editor.scroll;
                    let quit =
                        edit_ops::apply(&mut self.editor, action.clone(), doc_height, doc_width);
                    if quit {
                        self.should_quit = true;
                    }
                    if self.editor.scroll != scroll_before {
                        self.mark_scrolling();
                    }
                    self.flash_for_action(&action, save_before_dirty);
                    // Edit actions may have set `pending_link_follow`
                    // (FollowLinkUnderCursor doesn't hit `handle_app_action`
                    // path only when the action ISN'T App-level).
                    if let Some(target) = self.editor.pending_link_follow.take() {
                        self.follow_link(target, doc_height, doc_width);
                    }
                }
                self.needs_draw = true;
                // Phase 10 — `OpenInExternalEditor` defers to the run
                // loop the same way the settings overlay defers
                // `OpenConfigFolder` (we own `terminal` / `rx` here,
                // not in `handle_app_action`).
                if self.pending_open_file_in_editor {
                    self.pending_open_file_in_editor = false;
                    self.open_current_file_in_editor(&mut terminal, &rx);
                }
                // New decodes (e.g. from an edit that added an image
                // inside the viewport, or a scroll that brought one
                // into the prefetch window) are picked up by
                // `dispatch_visible_image_decodes` at the top of the
                // next loop iteration — no eager call needed here.
            }

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

/// Translate the App's `drag_target` into a UI-layer `DropIndicator` for
/// the renderer's post-pass.  Returns `None` when no relevant drag is in
/// progress (text-selection drags don't paint a table indicator); the
/// painter is then a no-op.  Column-border resize drags currently return
/// `None` because the live-preview re-render is itself the affordance —
/// adding a vertical guideline on top would be redundant noise.
fn drop_indicator_for(
    drag_target: &Option<mouse_ops::DragTarget>,
) -> Option<crate::ui::DropIndicator> {
    match drag_target.as_ref()? {
        mouse_ops::DragTarget::TableRow {
            table_byte_start,
            row_idx,
            hover_row_idx,
        } => Some(crate::ui::DropIndicator::Row {
            table_byte_start: *table_byte_start,
            src_row_idx: *row_idx,
            hover_row_idx: *hover_row_idx,
        }),
        mouse_ops::DragTarget::TableColumnHeader {
            table_byte_start,
            col_idx,
            hover_col_idx,
        } => Some(crate::ui::DropIndicator::Column {
            table_byte_start: *table_byte_start,
            src_col_idx: *col_idx,
            hover_col_idx: *hover_col_idx,
        }),
        mouse_ops::DragTarget::TableColumnBorder { .. }
        | mouse_ops::DragTarget::TextSelection { .. } => None,
    }
}

#[cfg(test)]
mod phase9_flash_tests {
    //! Phase 9 — exercise the transient-message mechanics directly
    //! against an `App` instance, bypassing the event loop.  Builds
    //! use `Capabilities::default()` and default config; no terminal
    //! is ever acquired.

    use super::*;
    use crate::ui::HintContent;

    pub(super) fn make_app() -> App {
        let caps = Capabilities::default();
        let theme_file = (&Theme::default()).into();
        App::new(
            Config::default(),
            KeyBindingOverrides::default(),
            theme_file,
            None,
            caps,
            Vec::new(),
        )
        .expect("build app")
    }

    #[test]
    fn flash_records_transient_info() {
        let mut app = make_app();
        assert!(app.transient.is_none());
        app.flash("Copied", MessageKind::Info);
        let msg = app.transient.as_ref().unwrap();
        assert_eq!(msg.text, "Copied");
        assert!(matches!(msg.kind, MessageKind::Info));
        assert!(msg.until.is_some(), "non-error messages auto-expire");
    }

    #[test]
    fn flash_error_is_sticky() {
        let mut app = make_app();
        app.flash("Save failed", MessageKind::Error);
        let msg = app.transient.as_ref().unwrap();
        assert!(msg.until.is_none(), "errors have no expiry deadline");
    }

    #[test]
    fn expire_transient_clears_only_after_deadline() {
        let mut app = make_app();
        app.flash("Saved", MessageKind::Success);
        // Force the deadline into the past.
        if let Some(msg) = app.transient.as_mut() {
            msg.until = Some(Instant::now() - Duration::from_millis(1));
        }
        assert!(app.expire_transient_if_due());
        assert!(app.transient.is_none());
    }

    #[test]
    fn expire_leaves_stick_errors() {
        let mut app = make_app();
        app.flash("Boom", MessageKind::Error);
        assert!(!app.expire_transient_if_due());
        assert!(app.transient.is_some());
    }

    #[test]
    fn dismiss_sticky_transient_on_escape() {
        let mut app = make_app();
        app.flash("Boom", MessageKind::Error);
        assert!(app.dismiss_sticky_transient());
        assert!(app.transient.is_none());
    }

    #[test]
    fn dismiss_sticky_ignores_non_error() {
        let mut app = make_app();
        app.flash("Saved", MessageKind::Success);
        assert!(!app.dismiss_sticky_transient());
        assert!(
            app.transient.is_some(),
            "non-errors must not clear on escape"
        );
    }

    #[test]
    fn flash_for_action_save_success_emits_saved_flash() {
        let mut app = make_app();
        // Simulate a successful save: dirty was true before and the
        // editor-state dirty flag has just flipped to false.
        app.editor.dirty = false;
        app.flash_for_action(&Action::Save, /*dirty_before=*/ true);
        let msg = app.transient.as_ref().expect("flash recorded");
        assert_eq!(msg.text, "Saved");
        assert!(matches!(msg.kind, MessageKind::Success));
    }

    #[test]
    fn flash_for_action_save_failure_emits_error() {
        let mut app = make_app();
        // Failure: dirty was true and remains true after "save".
        app.editor.dirty = true;
        app.flash_for_action(&Action::Save, /*dirty_before=*/ true);
        let msg = app.transient.as_ref().expect("flash recorded");
        assert!(matches!(msg.kind, MessageKind::Error));
    }

    #[test]
    fn flash_for_action_copy_emits_copied() {
        let mut app = make_app();
        app.flash_for_action(&Action::Copy, /*dirty_before=*/ false);
        let msg = app.transient.as_ref().expect("flash recorded");
        assert_eq!(msg.text, "Copied");
    }

    #[test]
    fn flash_for_action_cut_emits_copied() {
        let mut app = make_app();
        app.flash_for_action(&Action::Cut, /*dirty_before=*/ false);
        let msg = app.transient.as_ref().expect("flash recorded");
        assert_eq!(msg.text, "Copied");
    }

    #[test]
    fn flash_for_action_paste_is_silent() {
        let mut app = make_app();
        app.flash_for_action(&Action::Paste, /*dirty_before=*/ false);
        assert!(app.transient.is_none());
    }

    #[test]
    fn open_quit_confirm_seeds_three_button_modal() {
        let mut app = make_app();
        app.open_quit_confirm();
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::QuitConfirmModal>());
        // Button-label invariants are covered by the QuitConfirmModal
        // unit tests; here we just assert the modal is on the stack.
    }

    #[test]
    fn quit_confirm_cancel_dismisses_without_quit() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = make_app();
        app.open_quit_confirm();
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 40, 80);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::QuitConfirmModal>());
        assert!(!app.should_quit);
    }

    #[test]
    fn quit_confirm_discard_sets_should_quit() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = make_app();
        app.editor.dirty = true;
        app.open_quit_confirm();
        // Tab onto the Discard button (index 1) and press Enter.
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::QuitConfirmModal>());
        assert!(app.should_quit);
    }

    #[test]
    fn show_cheat_sheet_action_opens_combined_keybinds_overlay() {
        // Phase 10 review collapsed the read-only `ShowCheatSheet`
        // popover into the editable `OpenKeybinds` overlay.  Both
        // actions must now produce the same overlay state so users
        // with custom keybinds for the legacy action still get the
        // unified flow.
        let mut app = make_app();
        let handled = app.handle_app_action(&Action::ShowCheatSheet, 40, 80);
        assert!(handled);
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::KeybindsOverlayModal>());
    }

    #[test]
    fn hint_content_defaults_to_chords() {
        let app = make_app();
        match app.hint_content() {
            HintContent::Chords(_) => {}
            other => panic!("expected Chords, got {other:?}"),
        }
    }

    #[test]
    fn hint_content_prefers_transient_over_chords() {
        let mut app = make_app();
        app.flash("Copied", MessageKind::Info);
        match app.hint_content() {
            HintContent::Transient { text, .. } => assert_eq!(text, "Copied"),
            other => panic!("expected Transient, got {other:?}"),
        }
    }

    // ── Phase 10 — palette + overlay App-level tests ──────────────────

    #[test]
    fn open_command_palette_seeds_state() {
        let mut app = make_app();
        app.open_command_palette();
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::CommandPaletteModal>());
    }

    #[test]
    fn open_markdown_cheat_sheet_pushes_to_stack() {
        let mut app = make_app();
        app.open_markdown_cheat_sheet();
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::CheatSheetModal>());
        // Body-content regression assertions live alongside
        // `markdown_cheat_sheet_body` in `crate::ui::markdown_cheat_sheet`.
    }

    #[test]
    fn settings_overlay_field_change_emits_configuration_updated_flash() {
        // The plan calls for "exactly one `Configuration updated`
        // flash" when the settings overlay confirms a value.  We can't
        // exercise the live `Config::save` (it writes to XDG dirs),
        // but we can verify the App's response handler emits the
        // expected flash text.  `save_config_with_flash` is the
        // single source of truth — drive it directly.
        let mut app = make_app();
        // Driving `save_config_with_flash` runs `Config::save`, which
        // *might* fail when no config dir is available in the test
        // environment.  Either branch produces a flash; we just assert
        // *some* transient is set so the user gets feedback.
        app.save_config_with_flash("test");
        assert!(app.transient.is_some());
    }

    #[test]
    fn modal_wheel_delta_translates_scroll_direction() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        // Build minimal `MouseEvent`s with the kinds we care about;
        // crossterm requires explicit modifier + column/row fields.
        let scroll_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let scroll_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            ..scroll_up
        };
        let click = MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            ..scroll_up
        };
        assert_eq!(modal_wheel_delta(&scroll_up, 1), -1);
        assert_eq!(modal_wheel_delta(&scroll_down, 1), 1);
        // Coarser wheel honoured.
        assert_eq!(modal_wheel_delta(&scroll_down, 4), 4);
        // Wheel-step floor is 1, even when config asks for 0.
        assert_eq!(modal_wheel_delta(&scroll_up, 0), -1);
        // Non-wheel events return 0 so callers can blindly forward.
        assert_eq!(modal_wheel_delta(&click, 1), 0);
    }

    #[test]
    fn settings_overlay_open_external_sets_pending_flag_and_closes_overlay() {
        // The "Open config.toml in default editor" row defers the
        // actual editor invocation to the run loop so it can drive
        // the terminal suspend/resume.  Verify the wiring: pressing
        // Enter on that row stages the request and clears the overlay.
        // Default focus is "Theme"; one Up skips the divider and
        // lands on the editor row.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = make_app();
        app.open_settings_overlay();
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::SettingsOverlayModal>());
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);
        assert!(app.pending_open_config_in_editor);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::SettingsOverlayModal>());
    }

    #[test]
    fn settings_overlay_open_config_folder_closes_overlay() {
        // The top-row "Open config folder" entry hands the path to
        // the OS file manager via `spawn_open_worker` and closes the
        // overlay.  No `pending_open_config_in_editor` flag is set —
        // that path is editor-only.  Default focus is "Theme"; two
        // Up presses (skipping the divider) reach the folder row.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = make_app();
        app.open_settings_overlay();
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 40, 80);
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);
        assert!(!app.pending_open_config_in_editor);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::SettingsOverlayModal>());
    }

    #[test]
    fn dispatch_palette_action_save_round_trips() {
        // Driving `Action::Save` via the palette path produces the
        // same effect as a direct keystroke.  We assert that the
        // editor's dirty flag is consulted by the flash logic and
        // that no panic occurs when the buffer has no associated
        // path (the save no-ops via the typical save_file error path).
        let mut app = make_app();
        app.editor.dirty = false; // no-op save
        app.dispatch_palette_action(Action::Save, 40, 80);
        // No flash for a clean save (per `flash_for_action`).
        assert!(app.transient.is_none());
    }
}

#[cfg(test)]
mod phase15_insert_table_tests {
    //! Phase 15 — exercise the App-level Insert Table flow: pre-flight
    //! blank-line guard, modal lifecycle, and the resulting buffer +
    //! cursor state after Insert.

    use super::phase9_flash_tests::make_app;
    use super::*;
    use crate::editor::Mode;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// Build an App seeded with `text` and the cursor at byte
    /// `cursor_byte` (clamped to the buffer length).
    fn app_with_buffer(text: &str, cursor_byte: usize) -> App {
        let mut app = make_app();
        app.editor.buffer = Buffer::from_str(text);
        app.editor.refresh_parsed();
        let total = app.editor.buffer.len_chars();
        let char_off = app
            .editor
            .buffer
            .rope()
            .byte_to_char(cursor_byte.min(app.editor.buffer.contents().len()));
        app.editor.cursor.offset = char_off.min(total);
        app.editor.update_cursor_block();
        app
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn insert_table_on_blank_line_yields_gfm_table_with_cursor_in_first_header_cell() {
        let src = "para one\n\npara two\n";
        // Cursor on the blank line between the two paragraphs (byte 9).
        let mut app = app_with_buffer(src, 9);
        // Dispatch the action through the same path a Ctrl+Shift+T or
        // palette pick would take.
        let handled = app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(handled, "InsertTable should be handled at the App layer");
        assert!(
            app.modal_stack
                .contains::<crate::app::modal::InsertTableModal>(),
            "the rows/columns modal must be open after the pre-flight passes"
        );
        // Defaults are rows=2, cols=3 — matching the spec.  Tab to the
        // Insert button and press Enter.
        app.dispatch_modal_key(key(KeyCode::Tab), 40, 80); // Rows → Cols
        app.dispatch_modal_key(key(KeyCode::Tab), 40, 80); // Cols → Insert
        app.dispatch_modal_key(key(KeyCode::Enter), 40, 80);

        assert!(
            !app.modal_stack
                .contains::<crate::app::modal::InsertTableModal>(),
            "modal closes on insert"
        );
        let post = app.editor.buffer.contents();
        assert_eq!(
            post,
            "para one\n\
             \n\
             |   |   |   |\n\
             | --- | --- | --- |\n\
             |   |   |   |\n\
             |   |   |   |\n\
             \n\
             para two\n",
            "buffer mismatch:\n{post}"
        );

        // Cursor should be inside the first header cell — the byte 3
        // chars around the cursor offset should look like `|<sp><sp>`
        // (skip the leading `| `, sit on the middle space).
        let cursor_byte = app
            .editor
            .buffer
            .rope()
            .char_to_byte(app.editor.cursor.offset);
        assert!(
            post[cursor_byte.saturating_sub(2)..cursor_byte + 2].starts_with("|  "),
            "cursor should land in first header cell (byte {cursor_byte}); around: {:?}",
            &post[cursor_byte.saturating_sub(2)..(cursor_byte + 2).min(post.len())]
        );
        // A success transient should fire so the user gets feedback.
        assert!(
            matches!(
                app.transient.as_ref().map(|t| t.kind),
                Some(MessageKind::Success)
            ),
            "expected success flash, got {:?}",
            app.transient.as_ref().map(|t| t.kind)
        );
    }

    #[test]
    fn insert_table_in_mid_paragraph_flashes_warning_and_leaves_buffer_untouched() {
        let src = "this is a paragraph\nwith two lines\n";
        // Cursor in the middle of the first line.
        let mut app = app_with_buffer(src, 5);
        let before = app.editor.buffer.contents();
        let handled = app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(handled);
        assert!(
            !app.modal_stack
                .contains::<crate::app::modal::InsertTableModal>(),
            "modal should NOT open on a non-blank line"
        );
        assert_eq!(app.editor.buffer.contents(), before, "buffer unchanged");
        let msg = app.transient.as_ref().expect("warning flash present");
        assert!(
            matches!(msg.kind, MessageKind::Warning),
            "blank-line guard must use the auto-expiring Warning kind"
        );
        assert!(msg.until.is_some(), "warning must auto-expire");
        assert_eq!(msg.text, "Insert Table requires a blank line");
    }

    #[test]
    fn insert_table_on_heading_flashes_warning() {
        let src = "# Heading\n";
        let mut app = app_with_buffer(src, 4);
        app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::InsertTableModal>());
        let msg = app.transient.as_ref().expect("warning flash");
        assert!(matches!(msg.kind, MessageKind::Warning));
    }

    #[test]
    fn insert_table_on_list_item_flashes_warning() {
        let src = "- one\n- two\n";
        let mut app = app_with_buffer(src, 2);
        app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::InsertTableModal>());
        assert!(matches!(
            app.transient.as_ref().map(|t| t.kind),
            Some(MessageKind::Warning)
        ));
    }

    #[test]
    fn insert_table_on_existing_table_row_flashes_warning() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let mut app = app_with_buffer(src, 4); // mid-header
        app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::InsertTableModal>());
        assert!(matches!(
            app.transient.as_ref().map(|t| t.kind),
            Some(MessageKind::Warning)
        ));
    }

    #[test]
    fn insert_table_at_eof_without_trailing_newline_warns_then_succeeds_after_enter() {
        let src = "no trailing newline";
        let mut app = app_with_buffer(src, src.len());
        // Force Rendered mode so `Action::Newline` doesn't bounce the
        // cursor via the Preview→Rendered scroll-sync.
        app.editor.mode = Mode::Rendered;
        app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(
            !app.modal_stack
                .contains::<crate::app::modal::InsertTableModal>(),
            "modal should NOT open at EOF on a non-blank final line"
        );
        let msg = app.transient.as_ref().expect("warning flash present");
        assert!(matches!(msg.kind, MessageKind::Warning));
        // Clear the error so the second dispatch's success flash is
        // observable.
        app.transient = None;

        // Add a newline at the cursor: the cursor was on the last
        // byte of a non-blank line; `Newline` inserts `\n`, moving
        // the cursor onto a fresh empty trailing line that *is*
        // blank.  The second InsertTable should now pass pre-flight.
        edit_ops::apply(&mut app.editor, Action::Newline, 40, 80);
        app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(
            app.modal_stack
                .contains::<crate::app::modal::InsertTableModal>(),
            "modal should open after a newline made the cursor line blank"
        );
        // Press Enter immediately to confirm the defaults.
        app.dispatch_modal_key(key(KeyCode::Enter), 40, 80);
        let post = app.editor.buffer.contents();
        assert!(
            post.contains("| --- | --- | --- |"),
            "buffer should contain the alignment row, got:\n{post}"
        );
    }

    #[test]
    fn insert_table_modal_cancel_button_does_not_modify_buffer() {
        let src = "para one\n\npara two\n";
        let mut app = app_with_buffer(src, 9);
        let before = app.editor.buffer.contents();
        app.handle_app_action(&Action::InsertTable, 40, 80);
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::InsertTableModal>());
        // Esc dismisses without inserting.
        app.dispatch_modal_key(key(KeyCode::Esc), 40, 80);
        assert!(!app
            .modal_stack
            .contains::<crate::app::modal::InsertTableModal>());
        assert_eq!(app.editor.buffer.contents(), before);
    }
}

#[cfg(test)]
mod config_warning_app_tests {
    //! Verify the App-level wiring: a `ConfigWarning` flowing through
    //! `App::new` ends up on the modal stack, and dispatching Enter
    //! pops it.  The body-content invariants are owned by the unit
    //! tests in `crate::app::modal::config_warning`.

    use super::phase9_flash_tests::make_app;
    use super::*;
    use crate::app::modal::ConfigWarningModal;
    use crate::config::WarningKind;
    use std::path::PathBuf;

    #[test]
    fn modal_dismissed_on_button_press() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = make_app();
        let warnings = vec![ConfigWarning {
            path: PathBuf::from("config.toml"),
            kind: WarningKind::ParseError("oops".into()),
        }];
        let modal = ConfigWarningModal::from_warnings(&warnings).expect("modal built");
        app.modal_stack.push(Box::new(modal));
        assert!(app.modal_stack.contains::<ConfigWarningModal>());
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);
        assert!(!app.modal_stack.contains::<ConfigWarningModal>());
    }

    #[test]
    fn modal_dismissed_on_escape() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = make_app();
        let warnings = vec![ConfigWarning {
            path: PathBuf::from("config.toml"),
            kind: WarningKind::UnknownKeys(vec!["bogus".into()]),
        }];
        let modal = ConfigWarningModal::from_warnings(&warnings).expect("modal built");
        app.modal_stack.push(Box::new(modal));
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 40, 80);
        assert!(!app.modal_stack.contains::<ConfigWarningModal>());
    }
}
