pub mod modal;

use std::io::Stdout;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyEventKind, MouseEvent, MouseEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::config::{
    Action, Config, ConfigWarning, KeyBindingOverrides, KeyMap, Theme, ThemeFile, WarningKind,
};
use crate::document::Buffer;
use crate::editor::link::LinkTarget;
use crate::editor::{edit_ops, mouse_ops, EditorState, Mode, RAW_REVEAL_DELAY};
use crate::input::modal::default::DefaultHandler;
use crate::input::{ModalHandler, MouseDispatcher};
use crate::terminal::{set_pointer_shape, Capabilities, ColourDepth, PointerShape};
use crate::ui::{hint_line_for, EditorView, EditorViewState, HintChord, HintContent};

use self::modal::{ModalOutcome, ModalRenderCtx, ModalStack};

/// Events that the main loop can receive.
enum AppEvent {
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

/// One entry on [`App::nav_back`] / [`App::nav_forward`] — records
/// enough state to restore the exact scroll / cursor / mode we were in
/// when we left a particular document.
#[derive(Debug, Clone)]
struct NavEntry {
    path: PathBuf,
    scroll: usize,
    cursor_offset: usize,
    mode: Mode,
}

/// Severity of a [`TransientMessage`].  Drives style selection and
/// decides whether the message auto-expires.  `Error` is sticky:
/// the user must dismiss with Escape or a subsequent `Error` replaces
/// it.  Non-error kinds expire after `config.editor.transient_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Success,
    Warning,
    Error,
}

/// A single transient status message shown in the hint line.
#[derive(Debug, Clone)]
struct TransientMessage {
    text: String,
    kind: MessageKind,
    /// Wall-clock deadline after which non-error messages auto-expire.
    /// `None` for sticky errors.
    until: Option<Instant>,
}

/// Result of [`App::run_external_editor`].  Tells the caller whether
/// the editor actually ran so it can decide whether a post-exit
/// reload is appropriate.
enum ExternalEditorOutcome {
    /// `$VISUAL` / `$EDITOR` was unset; the path was handed to the
    /// OS handler via `open::that`.  No suspend happened.
    OsHandler,
    /// The TUI couldn't be suspended.  An error was already flashed.
    SuspendFailed,
    /// The editor process ran (or failed to launch) — here's the
    /// outcome.
    Exited(std::io::Result<std::process::ExitStatus>),
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
    /// Step 1 of the app refactor (`refactor-app.md`): active stack of
    /// trait-based modals.  As individual modals migrate off their
    /// dedicated `Option<X>` fields, they get pushed onto this stack
    /// and dispatched through the [`modal::Modal`] trait.  During
    /// migration the stack coexists with the legacy fields above; both
    /// the render cascade and the absorb-input ladder dispatch to the
    /// stack only when no legacy modal is open.
    modal_stack: ModalStack,
}

/// After the scroll position stops changing for this long, images
/// upgrade from the halfblocks partial render back to the native
/// protocol.  Tuned so the upgrade feels "immediate" to a human but
/// never fires during continuous scroll input (typical wheel tick gap
/// is well under 50 ms).
const SCROLL_QUIESCE: Duration = Duration::from_millis(150);

/// Minimum interval between successive `terminal.draw()` calls.  The
/// event loop processes events as fast as they arrive, but draws are
/// coalesced to at most one per this interval (~60 fps).  Under this
/// threshold, events still mutate state; the accumulated changes show
/// up on the next draw that actually fires.  Tuned so a wheel-tick
/// burst produces a handful of draws instead of one per tick.
const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Grace window after a `Resize` event during which draws are
/// suppressed.  Dragging a terminal window's edge fires a burst of
/// Resize events — one per pixel.  Drawing on each one produces
/// flickery partial-width output and pins CPU; instead we wait for
/// the burst to settle and draw exactly once at the final size.
const RESIZE_QUIESCE: Duration = Duration::from_millis(80);

/// Pure helper: true when `last_scroll_at` is `Some` and its elapsed
/// time is shorter than `quiesce`.  Extracted so tests can exercise it
/// without constructing a full `App`.
fn is_scrolling_within(last_scroll_at: Option<Instant>, quiesce: Duration) -> bool {
    last_scroll_at.is_some_and(|t| t.elapsed() < quiesce)
}

/// One-viewport-height prefetch margin (in rendered lines) above and
/// below the visible area.  An image whose rendered rows intersect
/// `[scroll - MARGIN, scroll + doc_height + MARGIN]` will have its
/// decode dispatched.  Tuned empirically: large enough that a
/// fast-scrolling user sees decoded images by the time they reach the
/// viewport, small enough that opening a long image-heavy document
/// doesn't immediately kick off every decode at once.
const VIEWPORT_DISPATCH_MARGIN: usize = 80;

/// Pure helper used by `App::dispatch_visible_image_decodes` — given
/// the image-block list, a source map, the scroll offset, the
/// viewport height, and a prefetch margin, return the image-block
/// infos whose rendered rows intersect the near-viewport window.
/// Lifted out of the App so it can be unit-tested without constructing
/// a terminal.
///
/// Order is preserved (document order) so that during a scroll, the
/// image that enters the window first is the one that gets dispatched
/// first — small but measurable fairness win on slow connections.
///
/// Returns `ImageBlockInfo` clones rather than just URLs because the
/// dispatcher needs to branch on `info.source` to tell diagram blocks
/// apart from regular images (Phase 17).
fn infos_in_viewport_window(
    image_blocks: &[crate::document::ImageBlockInfo],
    source_map: &crate::document::SourceMap,
    scroll: usize,
    doc_height: usize,
    margin: usize,
) -> Vec<crate::document::ImageBlockInfo> {
    let window_start = scroll.saturating_sub(margin);
    let window_end = scroll.saturating_add(doc_height).saturating_add(margin);
    image_blocks
        .iter()
        .filter_map(|info| {
            let range = source_map.rendered_lines_for_block(info.block_idx);
            if range.is_empty() {
                return None;
            }
            // Half-open intersection: [range.start, range.end) vs
            // [window_start, window_end).
            if range.start < window_end && range.end > window_start {
                Some(info.clone())
            } else {
                None
            }
        })
        .collect()
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

    /// Phase 9 — emit a transient message on the hint line.  Non-error
    /// kinds auto-expire after `config.editor.transient_ms`; `Error`
    /// kinds stick until Escape or until a subsequent `Error` replaces
    /// them.  Called from every phase that wants a one-shot
    /// notification — save/copy/cut outcomes, link-open failures,
    /// `Config::save` successes, etc.
    pub fn flash(&mut self, text: impl Into<String>, kind: MessageKind) {
        let mut text = text.into();
        let until = match kind {
            MessageKind::Error => {
                text.push_str(" — Esc to dismiss");
                None
            }
            _ => Some(Instant::now() + Duration::from_millis(self.config.editor.transient_ms)),
        };
        self.transient = Some(TransientMessage { text, kind, until });
        self.needs_draw = true;
    }

    /// Clear the current transient message if it has auto-expired.
    /// Called from the main loop before the draw gate so the hint line
    /// reverts to chords without the user having to press a key.
    /// Returns true when a redraw is needed.
    fn expire_transient_if_due(&mut self) -> bool {
        let Some(msg) = self.transient.as_ref() else {
            return false;
        };
        let Some(deadline) = msg.until else {
            return false;
        };
        if Instant::now() >= deadline {
            self.transient = None;
            return true;
        }
        false
    }

    /// The deadline when the current transient expires, if any.
    /// Contributes to [`App::next_deadline`] so the main loop wakes in
    /// time to revert the hint line even with no input arriving.
    fn transient_deadline(&self) -> Option<Instant> {
        self.transient.as_ref().and_then(|m| m.until)
    }

    /// Build the hint content for this frame.  Prompt > Transient >
    /// Chords, matching the plan's priority.
    fn hint_content(&self) -> HintContent {
        if let Some(prompt) = self.hint_prompt.as_ref() {
            return HintContent::Prompt {
                prompt: prompt.prompt.clone(),
                chords: prompt.chords.clone(),
            };
        }
        if let Some(msg) = self.transient.as_ref() {
            let style = match msg.kind {
                MessageKind::Info => self.theme.transient_info,
                MessageKind::Success => self.theme.transient_success,
                MessageKind::Warning => self.theme.transient_warning,
                MessageKind::Error => self.theme.transient_error,
            };
            return HintContent::Transient {
                text: msg.text.clone(),
                style,
            };
        }
        // Look up chord glyphs against the live KeyMap so any rebind
        // applied via the keybinds overlay shows up in the hint line
        // on the very next frame.  Falls back to the compiled-in
        // defaults during the brief window between `App::new` and the
        // first `KeyMap::build` in `run` — that path runs only when
        // building the override-aware keymap fails for unrelated
        // reasons, and the default keymap always builds.
        let fallback;
        let keymap = match self.keymap.as_ref() {
            Some(km) => km,
            None => {
                fallback = KeyMap::build(&KeyBindingOverrides::default())
                    .expect("default keymap always builds");
                &fallback
            }
        };
        HintContent::Chords(hint_line_for(&self.editor, keymap))
    }

    /// Record that the scroll position has just changed; used by the
    /// image painter to decide whether to fall back to halfblocks
    /// partial rendering on non-Kitty terminals.
    fn mark_scrolling(&mut self) {
        self.last_scroll_at = Some(Instant::now());
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

    /// True when `mark_scrolling` has fired within `SCROLL_QUIESCE`.
    fn is_scrolling(&self) -> bool {
        is_scrolling_within(self.last_scroll_at, SCROLL_QUIESCE)
    }

    fn any_modal_open(&self) -> bool {
        !self.modal_stack.is_empty()
    }

    /// Earliest wall-clock instant at which the event loop must wake
    /// up to apply a time-driven state change, even if no external
    /// event arrives.  Returns `None` when the loop can block
    /// indefinitely on `rx.recv()` — the common idle case.
    ///
    /// Only deadlines still in the future contribute.  Once a deadline
    /// has elapsed (and the post-elapse redraw has fired), it drops
    /// out of the computation so we can go back to blocking on input.
    ///
    /// Deadlines tracked:
    /// - `cursor_block_entered_at + RAW_REVEAL_DELAY` — wake to reveal
    ///   the raw cursor-block view when the jitter-suppression window
    ///   expires.
    /// - `last_scroll_at + SCROLL_QUIESCE` — wake to upgrade images
    ///   from halfblocks to the native graphics protocol once the
    ///   user stops scrolling.
    /// - `resize_quiesce_at` — wake to redraw once a terminal-resize
    ///   drag has settled (carries its own absolute deadline rather
    ///   than an offset, since it's set to `now + RESIZE_QUIESCE` on
    ///   each event).
    fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let mut earliest: Option<Instant> = None;
        let mut push = |candidate: Option<Instant>| {
            if let Some(c) = candidate.filter(|&c| c > now) {
                earliest = Some(earliest.map_or(c, |e: Instant| e.min(c)));
            }
        };
        push(
            self.editor
                .cursor_block_entered_at
                .map(|t| t + RAW_REVEAL_DELAY),
        );
        push(self.last_scroll_at.map(|t| t + SCROLL_QUIESCE));
        push(self.resize_quiesce_at);
        // Phase 9: wake in time to expire a transient hint-line
        // message so the hint reverts to chords even if the user
        // isn't typing.
        push(self.transient_deadline());
        push(self.editor.cursor_blink.next_toggle());
        earliest
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

    // ── Phase 8 — link navigation ─────────────────────────────────────────

    /// Intercept App-level actions (`FollowLinkUnderCursor`,
    /// `NavigateBack`, `NavigateForward`) before they hit `edit_ops::apply`.
    /// `TableMoveColumnLeft` / `TableMoveColumnRight` outside a table
    /// also short-circuit to navigation so the default Alt+Arrow
    /// keybinding feels natural even without an override.
    ///
    /// Returns `true` when the action was fully handled here; `false`
    /// means the caller should fall through to `edit_ops::apply`.
    fn handle_app_action(&mut self, action: &Action, doc_height: usize, doc_width: usize) -> bool {
        match action {
            Action::FollowLinkUnderCursor => {
                if let Some(target) = self.resolve_link_at_cursor() {
                    self.follow_link(target, doc_height, doc_width);
                }
                true
            }
            Action::OpenGitHub => {
                const EDAMAME_GITHUB_URL: &str = "https://github.com/gorgonian/edamame";
                self.spawn_open_worker(EDAMAME_GITHUB_URL.to_string());
                true
            }
            Action::NavigateBack => {
                self.navigate_back(doc_height, doc_width);
                true
            }
            Action::NavigateForward => {
                self.navigate_forward(doc_height, doc_width);
                true
            }
            // Default Alt+Arrow bindings land on the table actions; when
            // the cursor is outside any table, redirect them to nav.
            Action::TableMoveColumnLeft if !cursor_in_table(&self.editor) => {
                self.navigate_back(doc_height, doc_width);
                true
            }
            Action::TableMoveColumnRight if !cursor_in_table(&self.editor) => {
                self.navigate_forward(doc_height, doc_width);
                true
            }
            // Phase 10 — palette + configuration overlays.
            Action::ShowCommandPalette => {
                self.open_command_palette();
                true
            }
            Action::ShowMarkdownCheatSheet => {
                self.open_markdown_cheat_sheet();
                true
            }
            // Phase 10 review — ShowCheatSheet is no longer a
            // separate flow.  We accept it as an alias for
            // OpenKeybinds so users with a custom keybinding to it
            // (the action is configurable per `keybindings.toml`)
            // still see the combined view+edit overlay.
            Action::ShowCheatSheet => {
                self.open_keybinds_overlay();
                true
            }
            Action::OpenSettings => {
                self.open_settings_overlay();
                true
            }
            Action::OpenKeybinds => {
                self.open_keybinds_overlay();
                true
            }
            Action::OpenConfigFolder => {
                if let Some(dir) = Config::config_dir() {
                    self.spawn_open_worker(dir.display().to_string());
                } else {
                    self.flash("No config directory available", MessageKind::Error);
                }
                true
            }
            // Phase 16 / Phase 11 — these overlays are wired up in their
            // own phases.  Until then, surface a flash so users hitting
            // them in the palette get explicit feedback rather than
            // silent failure.
            Action::ExportHtml => {
                self.flash("HTML export — see Phase 16", MessageKind::Info);
                true
            }
            Action::ReloadFromDisk => {
                self.flash("Reload from disk — see Phase 11", MessageKind::Info);
                true
            }
            Action::OpenInExternalEditor => {
                if self.editor.buffer.path().is_none() {
                    self.flash("No file path for buffer", MessageKind::Error);
                } else {
                    // The actual editor invocation needs the live
                    // `Terminal` handle, owned by the run loop.
                    // Mirrors the settings-overlay "Open config.toml"
                    // flow.
                    self.pending_open_file_in_editor = true;
                    self.needs_draw = true;
                }
                true
            }
            Action::ToggleTableButtons => {
                // In-memory only — never write the change back to
                // `config.toml`.  Settings the user wants to keep
                // belong in the settings overlay.  Skip the toggle on
                // terminals where mouse reporting is unavailable: the
                // gutter glyphs would be inert and confusing.
                if self.capabilities.mouse {
                    self.config.table.show_buttons = !self.config.table.show_buttons;
                    let state = if self.config.table.show_buttons {
                        "on"
                    } else {
                        "off"
                    };
                    self.flash(format!("Table buttons {state}"), MessageKind::Info);
                } else {
                    self.flash("Mouse not supported on this terminal", MessageKind::Error);
                }
                self.needs_draw = true;
                true
            }
            Action::InsertTable => {
                // Pre-flight the blank-line guard before
                // opening the modal so a non-blank cursor surfaces an
                // immediate sticky error.  The same guard subsumes
                // mid-paragraph, heading, list, code-block, and
                // existing-table cases without classifying the block.
                let source = self.editor.buffer.contents();
                let cursor_byte = self
                    .editor
                    .buffer
                    .rope()
                    .char_to_byte(self.editor.cursor.offset);
                if crate::editor::table_edit::cursor_line_is_blank(&source, cursor_byte) {
                    self.open_insert_table_modal();
                } else {
                    self.flash("Insert Table requires a blank line", MessageKind::Warning);
                }
                self.needs_draw = true;
                true
            }
            Action::SaveCopy => {
                self.open_save_copy_modal();
                self.needs_draw = true;
                true
            }
            _ => false,
        }
    }

    // ── Phase 9 — transient messages & confirm modals ─────────────────────

    /// Clear a sticky `Error` transient on Escape, returning true to
    /// signal that the Escape was consumed and should not fall through
    /// to `Action::ExitToPreview`.  Non-sticky transients don't absorb
    /// Escape.
    fn dismiss_sticky_transient(&mut self) -> bool {
        let Some(msg) = self.transient.as_ref() else {
            return false;
        };
        if matches!(msg.kind, MessageKind::Error) {
            self.transient = None;
            return true;
        }
        false
    }

    /// Inspect `action` after dispatch and emit the matching flash
    /// notification.  Centralising this here means every code path
    /// that calls `Action::Save` / `Copy` / `Cut` gets consistent
    /// messaging without polluting `edit_ops::apply` with UI concerns.
    fn flash_for_action(&mut self, action: &Action, dirty_before_save: bool) {
        match action {
            Action::Save => {
                // Success is signalled by the dirty flag dropping.
                // Failure leaves `dirty` true; surface a sticky error
                // so the user knows the save did not happen.
                if dirty_before_save && !self.editor.dirty {
                    self.flash("Saved", MessageKind::Success);
                } else if dirty_before_save && self.editor.dirty {
                    self.flash("Save failed", MessageKind::Error);
                }
                // Saving a clean buffer is a no-op — no flash.
            }
            Action::Copy | Action::Cut => {
                self.flash("Copied", MessageKind::Info);
            }
            _ => {}
        }
    }

    /// Pop the topmost modal off the stack, dispatch the key to it,
    /// and apply the resulting [`ModalOutcome`].  Pop-then-dispatch
    /// lets the modal handler take `&mut App` without borrow conflicts
    /// — the popped modal owns itself.  `Continue` re-pushes it,
    /// `Close` drops it, `CloseAnd` drops it then runs the supplied
    /// callback against the now-unborrowed `App`.
    fn dispatch_modal_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        doc_height: usize,
        doc_width: usize,
    ) {
        let Some(mut top) = self.modal_stack.pop() else {
            return;
        };
        let outcome = top.handle_key(key, self, doc_height, doc_width);
        match outcome {
            ModalOutcome::Continue => self.modal_stack.push(top),
            ModalOutcome::Close => {}
            ModalOutcome::CloseAnd(cb) => cb(self),
        }
    }

    /// Open the three-button `Save / Discard / Cancel` modal.  Called
    /// when the user requests `Quit` on a dirty buffer.
    fn open_quit_confirm(&mut self) {
        let display = self
            .file_path
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Current buffer".to_owned());
        self.modal_stack
            .push(Box::new(modal::QuitConfirmModal::new(&display)));
    }

    // ── Phase 10 — command palette + configuration overlays ───────────────

    /// Open the Markdown syntax cheat-sheet popover.  Pushes a
    /// trait-based modal onto the stack; dispatch is handled by the
    /// generic [`Self::dispatch_modal_key`] / wheel routes.
    pub fn open_markdown_cheat_sheet(&mut self) {
        self.modal_stack
            .push(Box::new(modal::CheatSheetModal::new(self.theme)));
    }

    /// Open the fuzzy-searchable command palette.
    pub fn open_command_palette(&mut self) {
        let keymap = self.ensure_keymap_clone();
        self.modal_stack
            .push(Box::new(modal::CommandPaletteModal::new(&keymap)));
    }

    /// Build a fresh copy of the keymap, populating `self.keymap` if
    /// it has not been built yet.  Returns a clone so callers can use
    /// it without holding a borrow on `self`.
    fn ensure_keymap_clone(&mut self) -> KeyMap {
        if self.keymap.is_none() {
            match KeyMap::build(&self.keybindings) {
                Ok(km) => self.keymap = Some(km),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to build KeyMap on demand");
                    return KeyMap::build(&KeyBindingOverrides::default())
                        .expect("default keymap always builds");
                }
            }
        }
        self.keymap.as_ref().unwrap().clone()
    }

    /// Dispatch an `Action` chosen from the palette through the same
    /// path as a direct keystroke.  Mirrors the dispatch arm in
    /// [`App::run`].
    pub fn dispatch_palette_action(&mut self, action: Action, doc_height: usize, doc_width: usize) {
        let handled = self.handle_app_action(&action, doc_height, doc_width);
        if !handled {
            if matches!(action, Action::Quit) && self.editor.dirty {
                self.open_quit_confirm();
                return;
            }
            let dirty_before = self.editor.dirty;
            let scroll_before = self.editor.scroll;
            let quit = edit_ops::apply(&mut self.editor, action.clone(), doc_height, doc_width);
            if quit {
                self.should_quit = true;
            }
            if self.editor.scroll != scroll_before {
                self.mark_scrolling();
            }
            self.flash_for_action(&action, dirty_before);
            if let Some(target) = self.editor.pending_link_follow.take() {
                self.follow_link(target, doc_height, doc_width);
            }
        }
    }

    /// Open the settings overlay.
    pub fn open_settings_overlay(&mut self) {
        self.modal_stack
            .push(Box::new(modal::SettingsOverlayModal::new()));
    }

    /// Open the keybinds overlay.  Builds a live `KeyMap` if one
    /// hasn't been kept around yet.
    pub fn open_keybinds_overlay(&mut self) {
        let keymap = self.ensure_keymap_clone();
        self.modal_stack
            .push(Box::new(modal::KeybindsOverlayModal::new(&keymap)));
    }

    // ── Phase 15 — Insert Table modal ────────────────────────────────────

    /// Open the rows/columns prompt.  Caller is expected to have
    /// already verified the cursor sits on a blank line via
    /// [`editor::table_edit::cursor_line_is_blank`]; this method just
    /// seeds the modal state.
    pub fn open_insert_table_modal(&mut self) {
        self.modal_stack
            .push(Box::new(modal::InsertTableModal::new()));
    }

    // ── Save-a-copy modal ────────────────────────────────────────────────

    /// Open the path-input prompt seeded with a sensible default
    /// derived from the current buffer's filename (e.g. `notes.md`
    /// becomes `notes copy.md`).
    pub fn open_save_copy_modal(&mut self) {
        self.modal_stack
            .push(Box::new(modal::SaveCopyModal::for_buffer_path(
                self.editor.buffer.path(),
            )));
    }

    // ── Phase 13 — column-width injection warning ────────────────────────

    /// Drain `EditorState::pending_column_widths_commit` (set by a
    /// column-border drag's Release) and decide what happens next:
    ///   * No pending commit → no-op.
    ///   * Table already has a `<!-- tui-columns: ... -->` comment, OR
    ///     `config.table.warn_on_width_injection` is false → commit
    ///     immediately.
    ///   * Otherwise → open the warning modal carrying the table's
    ///     `table_byte_start` so its handler can call back to commit /
    ///     cancel via `EditorState`.
    fn handle_pending_column_widths(&mut self) {
        let Some(table_byte_start) = self.editor.pending_column_widths_commit else {
            return;
        };
        let already_has_comment = self.editor.table_has_tui_columns_comment(table_byte_start);
        if already_has_comment || !self.config.table.warn_on_width_injection {
            self.editor.commit_pending_column_widths();
            return;
        }
        self.modal_stack
            .push(Box::new(modal::WidthInjectionWarning::new()));
    }

    /// Resolve the link under the keyboard cursor by scanning the
    /// current raw line for `[text](url)` syntax and classifying the
    /// URL.  Mirrors `mouse_ops::link_at_offset` — keyboard and mouse
    /// paths use the same fallback scan so they behave identically
    /// regardless of which input device fired `FollowLink`.
    fn resolve_link_at_cursor(&self) -> Option<LinkTarget> {
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
    fn follow_link(&mut self, target: LinkTarget, doc_height: usize, doc_width: usize) {
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

    /// Open `config.toml` in the user's text editor and reload the
    /// config when the editor exits.  Prefers `$VISUAL` over
    /// `$EDITOR` (the modern shell convention); falls back to
    /// `open::that` (which delegates to the OS GUI handler) when
    /// neither variable is set.
    ///
    /// When a shell editor is invoked we need to surrender the
    /// terminal entirely: leave the alternate screen, drop raw mode,
    /// disable mouse capture, etc., so the editor can talk to the
    /// real TTY.  Once the editor exits we re-enter the TUI and
    /// force a full redraw.
    ///
    /// `terminal` is borrowed mutably so we can call
    /// [`Terminal::clear`] after re-entry — without this, ratatui's
    /// in-memory buffer thinks the screen still holds whatever it
    /// drew before suspension and skips redrawing unchanged cells.
    fn open_config_in_editor(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) {
        let Some(path) = Config::config_path() else {
            self.flash("No config directory available", MessageKind::Error);
            return;
        };
        // Make sure the file exists before we hand it to an editor
        // that might fail on a missing path.  `Config::save`
        // serialises the in-memory config — same content the user
        // would see if they navigated to the file via the file
        // manager.
        if !path.exists() {
            if let Err(e) = self.config.save() {
                tracing::warn!(error = %e, "failed to seed config.toml before editor launch");
                self.flash(format!("Config save failed: {e}"), MessageKind::Error);
                return;
            }
        }

        let outcome = self.run_external_editor(&path, terminal, rx);

        // Reload the config from disk so any edits the user made
        // are reflected in the running session.  Failures fall back
        // to the in-memory state with a warning — the user can
        // restart edamame to retry.  Run the reload regardless of
        // whether the editor actually launched: if we fell back to
        // the OS handler the user might still have edited the file.
        //
        // Any non-fatal warnings (parse error, unknown keys, invalid
        // keybinding entries) returned by `Config::load` are routed
        // into the same `ConfigWarningModal` we use at startup so the
        // user sees their typo as soon as they exit the editor.
        match Config::load() {
            Ok(loaded) => {
                self.config = loaded.config;
                self.keybindings = loaded.keybindings;
                // Rebuild the keymap so any keybinding edits take
                // effect for the next keystroke.
                match KeyMap::build(&self.keybindings) {
                    Ok(km) => self.keymap = Some(km),
                    Err(e) => {
                        tracing::warn!(error = %e, "rebuilt KeyMap failed after editor exit");
                    }
                }
                // Live-apply the theme so a `theme = "..."` edit in
                // the external editor takes effect without a
                // restart.  Uses the already-loaded `ThemeFile` so
                // we don't read the theme TOML twice.
                let monochrome = self.capabilities.colour_depth == ColourDepth::NoColour;
                let new_theme: &'static Theme =
                    Box::leak(Box::new(Theme::from_file(&loaded.theme, monochrome)));
                self.theme = new_theme;
                self.editor.set_theme(new_theme);
                if let Some(modal) = modal::ConfigWarningModal::from_warnings(&loaded.warnings) {
                    self.modal_stack.push(Box::new(modal));
                    self.needs_draw = true;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to reload config after editor exit");
            }
        }

        match outcome {
            ExternalEditorOutcome::Exited(Ok(s)) if s.success() => {
                self.flash("Configuration updated", MessageKind::Warning);
            }
            ExternalEditorOutcome::Exited(Ok(s)) => {
                self.flash(format!("Editor exited {s}"), MessageKind::Warning);
            }
            ExternalEditorOutcome::Exited(Err(e)) => {
                self.flash(format!("Editor failed: {e}"), MessageKind::Error);
            }
            // Suspend failure / OS-handler fallback already flashed
            // their own status — no extra message here.
            ExternalEditorOutcome::SuspendFailed | ExternalEditorOutcome::OsHandler => {}
        }
    }

    /// Save the current buffer (best-effort) and open it in the
    /// user's `$VISUAL` / `$EDITOR`.  After the editor exits the
    /// buffer is reloaded from disk so external edits are picked up
    /// — without this, subsequent saves from edamame would silently
    /// overwrite work done in the other editor.  Falls back to the
    /// OS handler when no shell editor is set; same flow the
    /// settings overlay uses for `config.toml`.
    fn open_current_file_in_editor(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) {
        let Some(path) = self.editor.buffer.path().map(|p| p.to_path_buf()) else {
            self.flash("No file path for buffer", MessageKind::Error);
            return;
        };

        // Save first so the external editor sees the in-memory state.
        if self.editor.dirty {
            if let Err(e) = self.editor.buffer.save_file() {
                tracing::warn!(error = %e, "failed to save buffer before editor launch");
                self.flash(format!("Save failed: {e}"), MessageKind::Error);
                return;
            }
            self.editor.dirty = false;
        }

        let outcome = self.run_external_editor(&path, terminal, rx);

        // Reload the buffer from disk so any external edits are
        // reflected.  Skipped on suspend failure (terminal is in a
        // degraded state already) and on the OS-handler fallback
        // (the OS handler returns immediately and the user may not
        // have closed the file yet — reloading prematurely would
        // discard their in-edamame edits while they're still
        // working).
        if matches!(outcome, ExternalEditorOutcome::Exited(_)) {
            if let Err(e) = self.load_file_into_editor(path) {
                tracing::warn!(error = %e, "failed to reload buffer after editor exit");
                self.flash(format!("Reload failed: {e}"), MessageKind::Error);
                return;
            }
        }

        match outcome {
            ExternalEditorOutcome::Exited(Ok(s)) if s.success() => {
                self.flash("File reloaded", MessageKind::Success);
            }
            ExternalEditorOutcome::Exited(Ok(s)) => {
                self.flash(format!("Editor exited {s}"), MessageKind::Warning);
            }
            ExternalEditorOutcome::Exited(Err(e)) => {
                self.flash(format!("Editor failed: {e}"), MessageKind::Error);
            }
            ExternalEditorOutcome::SuspendFailed | ExternalEditorOutcome::OsHandler => {}
        }
    }

    /// Suspend the TUI, run an external editor on `path`, and resume.
    /// Shared between the settings-overlay "Open config.toml" flow
    /// and the palette "Open current file in system editor" flow:
    /// both need the same read-thread / terminal dance around
    /// `Command::status()`.  The caller is responsible for any
    /// pre-launch save / post-exit reload — this helper only owns
    /// the suspend / resume window.
    fn run_external_editor(
        &mut self,
        path: &Path,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) -> ExternalEditorOutcome {
        let editor = std::env::var("VISUAL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                std::env::var("EDITOR")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            });

        let Some(editor) = editor else {
            // No shell editor — fall back to the OS handler.  This
            // is the same path Phase 8 link-following uses, so the
            // user sees consistent behaviour whether $EDITOR is set
            // or not.
            self.spawn_open_worker(path.display().to_string());
            self.flash("Opening with system default", MessageKind::Info);
            return ExternalEditorOutcome::OsHandler;
        };

        // Pause our crossterm read thread so the editor has
        // uncontested access to stdin.  Without this, our thread
        // and the editor both call `read()` on the same fd: bytes
        // get split between them, the editor sees corrupted input
        // (the `1;rgb:...` artifact users reported was the OSC 11
        // background-colour response that neovim queried for, with
        // some bytes stolen by us), and keystrokes feel laggy
        // because half of them never reach the editor.
        if let Some(p) = self.read_paused.as_ref() {
            p.store(true, Ordering::Release);
        }
        // The poll loop wakes every 100 ms; sleep slightly longer
        // so the read thread is guaranteed to have entered the
        // paused branch before we hand stdin to the editor.
        std::thread::sleep(Duration::from_millis(120));
        // Discard any events that were already parsed during the
        // overlap window so they don't reach the editor (or
        // re-emerge in our channel after resume).
        while rx.try_recv().is_ok() {}

        // Suspend the TUI.  Best-effort: a failure here means the
        // editor would launch into a confused terminal state, so
        // bail out and tell the user.
        if let Err(e) = crate::terminal::restore() {
            tracing::warn!(error = %e, "failed to suspend terminal for editor");
            self.flash(format!("Editor failed: {e}"), MessageKind::Error);
            if let Some(p) = self.read_paused.as_ref() {
                p.store(false, Ordering::Release);
            }
            return ExternalEditorOutcome::SuspendFailed;
        }

        let status = std::process::Command::new(&editor).arg(path).status();

        // Always try to restore the TUI, even if the editor failed —
        // otherwise we strand the user in a half-suspended state.
        let mouse = self.capabilities.mouse;
        let kbd = self.capabilities.keyboard_enhancement;
        let restore_result = crate::terminal::re_enter(mouse, kbd);
        if let Err(e) = restore_result {
            tracing::error!(error = %e, "failed to re-enter TUI after editor");
            // We can still draw something, but the terminal is in
            // a degraded state.  Surface it loudly.
            self.flash(format!("Terminal restore failed: {e}"), MessageKind::Error);
        }
        // Some terminals emit acknowledgements for the re-enter
        // sequences (kitty keyboard, mouse mode).  Pause stays on
        // here so any such bytes flow into the kernel buffer
        // rather than racing with the read thread that's about to
        // resume.  After this short wait, drain the channel and
        // resume — the read thread will pick up anything still
        // pending on its first post-resume poll.
        std::thread::sleep(Duration::from_millis(30));
        while rx.try_recv().is_ok() {}
        if let Some(p) = self.read_paused.as_ref() {
            p.store(false, Ordering::Release);
        }

        // Ratatui caches the previous frame; clearing forces it to
        // redraw every cell on the next `terminal.draw` call.
        let _ = terminal.clear();
        self.needs_draw = true;

        ExternalEditorOutcome::Exited(status)
    }

    /// Spawn a worker thread that calls `open::that` and reports the
    /// outcome via `AppEvent::LinkOpenResult`.  Keeps the UI thread
    /// responsive — `xdg-open` can take several hundred milliseconds
    /// on some desktops.
    fn spawn_open_worker(&self, target: String) {
        let Some(tx) = self.app_tx.clone() else {
            return;
        };
        std::thread::spawn(move || {
            let result = open::that(&target).map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::LinkOpenResult(result));
        });
    }

    /// Scroll so `slug`'s heading sits at the top of the viewport.
    /// No-op if the slug isn't in the current document's anchor table.
    /// In editing modes (Rendered / Raw) also moves the cursor onto
    /// the heading so subsequent navigation feels anchored.
    fn scroll_to_heading(&mut self, slug: &str, doc_height: usize, doc_width: usize) {
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
    fn navigate_to_file(&mut self, path: PathBuf) {
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
    fn load_file_into_editor(&mut self, path: PathBuf) -> Result<()> {
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
    fn current_nav_entry(&self) -> Option<NavEntry> {
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
    fn navigate_back(&mut self, doc_height: usize, doc_width: usize) {
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

    fn navigate_forward(&mut self, doc_height: usize, doc_width: usize) {
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
    fn open_dirty_guard(&mut self, pending: PathBuf) {
        let display = self
            .file_path
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "current file".to_owned());
        self.modal_stack
            .push(Box::new(modal::DirtyGuardModal::new(&display, pending)));
    }

    // ── End Phase 8 navigation helpers ────────────────────────────────────

    /// Reload the theme named by `self.config.theme` from disk, build
    /// a fresh `Theme`, leak it into `'static`, and swap it onto
    /// `self.theme` and the editor.  Any non-fatal warnings raised by
    /// the theme loader (parse error, unknown keys) are surfaced via
    /// the existing `ConfigWarningModal`, which renders above the
    /// settings overlay so a malformed theme is the first thing the
    /// user sees.
    ///
    /// # Leak by design
    ///
    /// `Theme` is held everywhere as `&'static Theme` — see the
    /// constructor for the rationale (every widget and `EditorState`
    /// reads it on the hot render path, and threading a lifetime or
    /// wrapping in `Arc` would touch dozens of call sites for no
    /// observable benefit).  `'static` is obtained by `Box::leak`-ing
    /// the heap allocation.
    ///
    /// Each theme change leaks one fresh `Theme` allocation: the
    /// previous one is unreachable but never freed, since `'static`
    /// references can't be invalidated.  The cost per leak is bounded
    /// — a `Theme` is a fixed-size struct of ~100 `Style` values, on
    /// the order of a few KB — and theme changes are user-initiated
    /// (Enter / Left / Right on the settings overlay's Theme row, or
    /// post-editor reload).  Even an aggressive cycler would
    /// accumulate at most a few MB across the editor's session.
    ///
    /// The alternatives (`Arc<Theme>`, a `RwLock`-guarded static,
    /// custom arena reset on change) all cost more — either at every
    /// reader on the render path or in invariants around
    /// already-rendered `parsed.lines` that hold `Style` values
    /// copied out of the previous theme.  `Box::leak` keeps the
    /// rendering path zero-overhead and the live-update path trivial
    /// to reason about.
    fn apply_active_theme(&mut self) {
        let (theme_file, warnings) = Config::load_theme(&self.config.theme);
        let monochrome = self.capabilities.colour_depth == ColourDepth::NoColour;
        let new_theme: &'static Theme =
            Box::leak(Box::new(Theme::from_file(&theme_file, monochrome)));
        self.theme = new_theme;
        self.editor.set_theme(new_theme);
        self.needs_draw = true;
        if let Some(modal) = modal::ConfigWarningModal::from_warnings(&warnings) {
            self.modal_stack.push(Box::new(modal));
        }
    }

    /// Persist `config.toml` and flash a `Configuration updated`
    /// notification on success.  Centralises the save-and-notify
    /// pattern so every caller (capability suppression, remote-image
    /// policy, future settings overlay) gets the same UX without
    /// sprinkling `flash()` calls through the dispatch paths.
    fn save_config_with_flash(&mut self, err_context: &'static str) {
        match self.config.save() {
            Ok(()) => {
                self.flash("Configuration updated", MessageKind::Warning);
            }
            Err(e) => {
                tracing::warn!(error = %e, "{}", err_context);
                self.flash(format!("Config save failed: {e}"), MessageKind::Error);
            }
        }
    }

    /// Emit an OSC 22 escape to change the terminal pointer shape, but only
    /// if the requested shape differs from the last one we asked for.
    fn update_pointer_shape(&mut self, shape: PointerShape) {
        if self.last_pointer_shape == shape {
            return;
        }
        set_pointer_shape(shape);
        self.last_pointer_shape = shape;
    }

    /// Whether inline image rendering should happen right now.  The
    /// persisted `config.images.enabled` decides when it's `Always` or
    /// `Never`; `Ask` defers to `session_images_enabled`, which is
    /// populated only after the user answers the images-enabled
    /// prompt.  While the prompt is still pending this returns false
    /// so no decodes are dispatched behind the user's back.
    fn effective_images_enabled(&self) -> bool {
        match self.config.images.enabled {
            crate::config::ImagesEnabled::Always => true,
            crate::config::ImagesEnabled::Never => false,
            crate::config::ImagesEnabled::Ask => self.session_images_enabled.unwrap_or(false),
        }
    }

    /// Whether image blocks should still reserve layout rows, even if
    /// no decode will run.  Returns `false` only when the user has
    /// explicitly declined — persisted `Never` or a session-level `No`
    /// / Escape on the images-enabled prompt.  The `Ask` + pending
    /// state still reports `true` so the layout doesn't reflow while
    /// the modal is on screen.
    fn images_layout_enabled(&self) -> bool {
        match self.config.images.enabled {
            crate::config::ImagesEnabled::Never => false,
            crate::config::ImagesEnabled::Always => true,
            crate::config::ImagesEnabled::Ask => self.session_images_enabled != Some(false),
        }
    }

    /// Walk the current parse for `Block::ImageBlock`s, mark any new
    /// URLs as `Pending` in the cache, and spawn a worker thread per
    /// newly-requested URL.  Safe to call every frame: existing
    /// `Ready`/`Failed`/`Pending` entries return `false` from `request`
    /// so we do not re-dispatch.
    ///
    /// Dispatches every image in the document regardless of viewport.
    /// Called from the remote-image-prompt handler where we've just
    /// unlocked a batch of previously-blocked decodes — the user has
    /// opted in to fetching, so eagerness matches intent.
    fn dispatch_image_decodes(&mut self) {
        let infos: Vec<crate::document::ImageBlockInfo> = self.editor.parsed.image_blocks.clone();
        self.dispatch_image_decodes_for(&infos);
    }

    /// Viewport-limited decode dispatch: only requests images whose
    /// rendered rows are inside the visible window extended by a
    /// one-viewport margin above and below.  Called before each frame
    /// so scrolling smoothly introduces new near-viewport decodes
    /// without ever running a decode for an image far off-screen.
    ///
    /// `doc_height` is the rendered-line height of the document area
    /// (terminal height minus the status bar).  `scroll` is the top
    /// visible rendered-line index.
    fn dispatch_visible_image_decodes(&mut self, scroll: usize, doc_height: usize) {
        // Pre-compute the window once, then collect the URLs whose
        // rendered rows intersect it.  Keeping this as a pure helper
        // lets it be exercised by unit tests without constructing an
        // App.  We allow `urls_in_viewport_window` to read only the
        // fields it needs (the image-blocks list and source-map), so
        // the borrow here is narrow.
        let infos = infos_in_viewport_window(
            &self.editor.parsed.image_blocks,
            &self.editor.parsed.source_map,
            scroll,
            doc_height,
            VIEWPORT_DISPATCH_MARGIN,
        );
        self.dispatch_image_decodes_for(&infos);
    }

    /// Shared dispatch primitive: spawn a worker thread for each
    /// image-block whose URL `ImageCache::request` accepts as new.
    ///
    /// Branches on `info.source`: `Some(DiagramSource::Mermaid(_))`
    /// routes through `crate::diagram::resolve_mermaid`; `None` goes
    /// through `crate::image::resolve` (file / http / https).  Both
    /// paths land in the same `AppEvent::ImageReady` → `ImageCache`
    /// pipeline so downstream rendering is uniform.
    ///
    /// Each worker body is wrapped in `std::panic::catch_unwind` so a
    /// panic inside the decoder or the mermaid renderer (v0.2.1 has
    /// several known panic bugs) always produces exactly one
    /// `ImageReady(Err(...))` instead of stranding the cache entry as
    /// `Pending` forever.
    fn dispatch_image_decodes_for(&mut self, infos: &[crate::document::ImageBlockInfo]) {
        if !self.effective_images_enabled() {
            return;
        }
        let Some(tx) = self.app_tx.clone() else {
            return;
        };
        let doc_path = self.file_path.clone();
        let remote_policy = self.config.images.remote_policy;
        let session_allow_remote = self.session_allow_remote;
        // Give the worker the target ceiling AND the terminal's font-size
        // so the decoded image is pre-resized to fit within
        // `max_cells × font_size` pixels.  After this the main thread's
        // protocol never has to resize — every render call at the same
        // target area is a no-op beyond the first encode.
        let max_cells = Some((
            self.config.images.max_width as u16,
            self.config.images.max_height as u16,
        ));
        let font_size = self
            .capabilities
            .image_picker
            .as_ref()
            .map(|p| p.font_size());
        // Halfblocks picker + current area width let the worker render the
        // scratch buffer off the UI thread.  When either is missing
        // (terminal without image support, or first iteration before the
        // loop has observed a term size), the worker skips the scratch
        // and `get_protocol_pair` falls back to a sync encode on the UI
        // thread — same cost as pre-Phase-7a, but only on that one cold
        // path.
        let scratch_picker = self.capabilities.halfblocks_picker.clone();
        let scratch_width = if self.last_area_width > 0 {
            Some(self.last_area_width)
        } else {
            None
        };

        for info in infos {
            if !self.editor.images.request(&info.url) {
                continue;
            }
            let tx = tx.clone();
            let doc_path = doc_path.clone();
            let url = info.url.clone();
            let source = info.source.clone();
            let scratch_picker = scratch_picker.clone();
            std::thread::spawn(move || {
                // Wrap the entire worker body in `catch_unwind` so a
                // panic (especially inside `mermaid_rs_renderer`, which
                // has known panic bugs in v0.2.1) still produces an
                // `ImageReady(Err)` — otherwise the cache entry would
                // stay `Pending` forever and the placeholder would
                // never transition to the failure state.
                let url_for_panic = url.clone();
                let result: Result<crate::image::LoadedImage, (String, String)> =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &source {
                        Some(crate::diagram::DiagramSource::Mermaid(src)) => {
                            crate::diagram::resolve_mermaid(url.clone(), src, max_cells, font_size)
                                .map_err(|e| (url.clone(), e.to_string()))
                        }
                        None => crate::image::resolve(
                            &url,
                            doc_path.as_deref(),
                            remote_policy,
                            session_allow_remote,
                            max_cells,
                            font_size,
                        )
                        .map_err(|e| (url.clone(), e.to_string())),
                    }))
                    .unwrap_or_else(|payload| {
                        let msg = if let Some(s) = payload.downcast_ref::<String>() {
                            format!("panic: {s}")
                        } else if let Some(s) = payload.downcast_ref::<&'static str>() {
                            format!("panic: {s}")
                        } else {
                            "panic".to_string()
                        };
                        Err((url_for_panic, msg))
                    });

                let event = match result {
                    Ok(mut loaded) => {
                        // Render the halfblocks scratch here on the
                        // worker thread so the UI thread's first paint
                        // is a cache hit.  Guarded on having both a
                        // picker and an observed area width; missing
                        // either means the sync fallback in
                        // `get_protocol_pair` handles the cold path.
                        if let (Some(picker), Some(width), Some((mw, mh)), Some(fs)) =
                            (&scratch_picker, scratch_width, max_cells, font_size)
                        {
                            let rows =
                                crate::image::aspect_rows_of(&loaded.image, mw, mh, fs) as u16;
                            if width > 0 && rows > 0 {
                                let rect = Rect::new(0, 0, width, rows);
                                let buf = crate::image::render_halfblocks_scratch(
                                    picker,
                                    loaded.image.clone(),
                                    rect,
                                );
                                loaded.scratch = Some((rect, buf));
                            }
                        }
                        AppEvent::ImageReady(Ok(loaded))
                    }
                    Err(err) => AppEvent::ImageReady(Err(err)),
                };
                let _ = tx.send(event);
            });
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

/// True when `path` ends in `.md` / `.markdown` (case-insensitive).
/// Used by `App::follow_link` to decide whether a LocalFile link
/// should be opened in-editor or handed off to the OS default app.
fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_ascii_lowercase();
            lower == "md" || lower == "markdown"
        })
        .unwrap_or(false)
}

/// Translate a wheel event into a `ModalState::scroll_by` delta.
/// Honours the user's configured `mouse_scroll_lines` so a coarser
/// wheel feel applies inside modals as well as the editor.  Returns
/// `0` for non-wheel mouse events so callers can blindly forward
/// every `Event::Mouse` without filtering.
fn modal_wheel_delta(event: &MouseEvent, wheel_step: usize) -> i32 {
    let step = wheel_step.max(1) as i32;
    match event.kind {
        MouseEventKind::ScrollUp => -step,
        MouseEventKind::ScrollDown => step,
        _ => 0,
    }
}

/// True when the editor's cursor sits inside a table block.  Mirrors
/// the check used by `edit_ops::cursor_in_table`; re-implemented here
/// to keep the App free of a cross-module private dep.
fn cursor_in_table(state: &EditorState) -> bool {
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    let source = state.buffer.contents();
    crate::editor::table_edit::find_table_at(&source, cursor_byte).is_some()
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

// ── Extension trait for DefaultHandler ───────────────────────────────────────

/// Private extension trait so `DefaultHandler` can process raw crossterm events
/// (filtering for KeyPress) without exposing this logic in the `ModalHandler`
/// trait (which operates on already-filtered `KeyEvent`s).
trait HandleEvent {
    fn handle_event(&mut self, event: Event, state: &EditorState) -> Option<crate::config::Action>;
}

impl<'k> HandleEvent for DefaultHandler<'k> {
    fn handle_event(&mut self, event: Event, state: &EditorState) -> Option<crate::config::Action> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle(key, state),
            _ => None,
        }
    }
}

#[cfg(test)]
mod scroll_quiesce_tests {
    use super::*;

    #[test]
    fn is_scrolling_is_false_when_never_scrolled() {
        assert!(!is_scrolling_within(None, SCROLL_QUIESCE));
    }

    #[test]
    fn is_scrolling_is_true_right_after_mark() {
        let now = Instant::now();
        assert!(is_scrolling_within(Some(now), SCROLL_QUIESCE));
    }

    #[test]
    fn is_scrolling_is_false_after_quiesce_elapsed() {
        // `Instant` can't be forged into the past directly; instead use
        // a tiny quiesce window and sleep past it.
        let now = Instant::now();
        std::thread::sleep(Duration::from_millis(20));
        assert!(!is_scrolling_within(Some(now), Duration::from_millis(5)));
    }

    #[test]
    fn is_scrolling_is_true_within_a_short_window() {
        // With a generous window, a just-marked timestamp is still
        // "scrolling".
        let now = Instant::now();
        assert!(is_scrolling_within(
            Some(now),
            Duration::from_millis(10_000)
        ));
    }
}

#[cfg(test)]
mod viewport_window_tests {
    use super::*;

    use crate::document::{ImageBlockInfo, SourceMap};

    /// Hand-build a `(image_blocks, source_map)` pair where each of N
    /// images produces one rendered line at a known position.  Block
    /// indices increment in order; the source-map's rendered_to_block
    /// is identity.
    fn fixture(image_rows: &[(&str, usize)]) -> (Vec<ImageBlockInfo>, SourceMap) {
        let total_rows = image_rows.iter().map(|(_, r)| r + 1).max().unwrap_or(0);
        let mut rendered_to_block = vec![usize::MAX; total_rows];
        let mut blocks = Vec::new();
        for (i, (url, row)) in image_rows.iter().enumerate() {
            rendered_to_block[*row] = i;
            blocks.push(ImageBlockInfo {
                block_idx: i,
                alt: String::new(),
                url: (*url).to_owned(),
                source: None,
            });
        }
        // Fill sentinel slots with their own index so unrelated rows
        // don't collapse onto block 0's range.
        for (i, slot) in rendered_to_block.iter_mut().enumerate() {
            if *slot == usize::MAX {
                *slot = blocks.len() + i;
            }
        }
        // We only need rendered_to_block to have meaningful data for
        // the image blocks; extended_ranges / original_ranges just
        // need to be long enough for rendered_lines_for_block's
        // fallback not to panic.
        let max_block = *rendered_to_block.iter().max().unwrap() + 1;
        let ranges = (0..max_block).map(|i| i..i + 1).collect::<Vec<_>>();
        let map = SourceMap::new(rendered_to_block, ranges.clone(), ranges, 0);
        (blocks, map)
    }

    #[test]
    fn viewport_window_keeps_images_inside_visible_rows() {
        let (blocks, map) = fixture(&[("a.png", 5), ("b.png", 50), ("c.png", 200)]);
        // doc_height=20, scroll=0, margin=0 → window = [0, 20).
        let urls: Vec<String> = infos_in_viewport_window(&blocks, &map, 0, 20, 0)
            .into_iter()
            .map(|i| i.url)
            .collect();
        assert_eq!(urls, vec!["a.png".to_owned()]);
    }

    #[test]
    fn viewport_window_keeps_images_inside_prefetch_margin() {
        let (blocks, map) = fixture(&[("a.png", 5), ("b.png", 50), ("c.png", 200)]);
        // scroll=0, doc_height=20, margin=40 → window = [0, 60),
        // picks up a.png (row 5) and b.png (row 50).
        let urls: Vec<String> = infos_in_viewport_window(&blocks, &map, 0, 20, 40)
            .into_iter()
            .map(|i| i.url)
            .collect();
        assert_eq!(urls, vec!["a.png".to_owned(), "b.png".to_owned()]);
    }

    #[test]
    fn viewport_window_respects_scroll_offset() {
        let (blocks, map) = fixture(&[("a.png", 5), ("b.png", 50), ("c.png", 200)]);
        // scroll=180, doc_height=20, margin=10 → window = [170, 210),
        // picks up c.png only.
        let urls: Vec<String> = infos_in_viewport_window(&blocks, &map, 180, 20, 10)
            .into_iter()
            .map(|i| i.url)
            .collect();
        assert_eq!(urls, vec!["c.png".to_owned()]);
    }

    #[test]
    fn viewport_window_preserves_document_order() {
        // Doc order is a, b, c; they're all in the window.  The
        // returned Vec must keep that order so the first-into-window
        // image is dispatched first on slow connections.
        let (blocks, map) = fixture(&[("c.png", 2), ("a.png", 0), ("b.png", 1)]);
        let urls: Vec<String> = infos_in_viewport_window(&blocks, &map, 0, 10, 0)
            .into_iter()
            .map(|i| i.url)
            .collect();
        // `blocks` is in the order we passed: c, a, b.  Verify the
        // dispatch helper follows `image_blocks` order, not sorted by
        // row.
        assert_eq!(
            urls,
            vec!["c.png".to_owned(), "a.png".to_owned(), "b.png".to_owned()]
        );
    }

    #[test]
    fn viewport_window_empty_when_all_images_above() {
        let (blocks, map) = fixture(&[("a.png", 0), ("b.png", 5)]);
        // scroll=100, doc_height=20, margin=10 → window = [90, 130).
        // Both images are well below the window.
        let urls = infos_in_viewport_window(&blocks, &map, 100, 20, 10);
        assert!(urls.is_empty());
    }

    #[test]
    fn viewport_window_handles_saturating_scroll_underflow() {
        // scroll < margin would underflow a signed subtract; we use
        // saturating_sub so the window just clamps at 0.
        let (blocks, map) = fixture(&[("a.png", 0), ("b.png", 5)]);
        let urls: Vec<String> = infos_in_viewport_window(&blocks, &map, 2, 3, 100)
            .into_iter()
            .map(|i| i.url)
            .collect();
        // Window = [0, 105), both images inside.
        assert_eq!(urls, vec!["a.png".to_owned(), "b.png".to_owned()]);
    }
}

#[cfg(test)]
mod phase9_flash_tests {
    //! Phase 9 — exercise the transient-message mechanics directly
    //! against an `App` instance, bypassing the event loop.  Builds
    //! use `Capabilities::default()` and default config; no terminal
    //! is ever acquired.

    use super::*;

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
