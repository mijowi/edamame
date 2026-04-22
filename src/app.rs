use std::io::Stdout;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyEventKind, MouseEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::config::{Action, Config, KeyBindingOverrides, KeyMap, Theme, ThemeFile};
use crate::document::Buffer;
use crate::editor::link::LinkTarget;
use crate::editor::{edit_ops, mouse_ops, EditorState, Mode, PARSED_DEBOUNCE, RAW_REVEAL_DELAY};
use crate::input::modal::default::DefaultHandler;
use crate::input::{ModalHandler, MouseDispatcher};
use crate::terminal::{set_pointer_shape, Capabilities, ColourDepth, PointerShape};
use crate::ui::{EditorView, EditorViewState, ModalButton, ModalResponse, ModalState, ModalView};

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

/// Phase 8 three-button `Save / Discard / Cancel` prompt shown when
/// following a link would navigate away from a dirty buffer.  Carries
/// the pending target across the modal's lifetime so we can resume the
/// navigation once the user picks a button.
struct DirtyGuardPrompt {
    body: Vec<String>,
    buttons: Vec<ModalButton>,
    state: ModalState,
    /// The destination that was about to be followed when the guard
    /// fired.  Stored so we can re-dispatch after `Save` or `Discard`.
    pending: PathBuf,
}

/// A modal popup currently shown on top of the editor.  We only model the
/// startup capability-notice in Phase 4; the `ModalView` widget itself is
/// generic enough to host other modals in later phases.
struct StartupNotice {
    body: Vec<String>,
    buttons: Vec<ModalButton>,
    state: ModalState,
}

/// Phase 7 remote-image prompt: shown when `config.image.remote_policy`
/// is `Ask` and the open document references at least one `http(s)://`
/// image.  Four buttons: `Yes` (in-memory allow for this session),
/// `No` (dismiss without fetching), `Always` (persist config),
/// `Never` (persist config).
struct RemoteImagePrompt {
    body: Vec<String>,
    buttons: Vec<ModalButton>,
    state: ModalState,
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
    /// When `Some`, a startup notice modal is displayed and absorbs key
    /// events.  Cleared to `None` once the user dismisses it.
    startup_notice: Option<StartupNotice>,
    /// Phase 7 remote-image prompt.  Shown only after the startup
    /// notice is dismissed (they're stacked one-at-a-time so the user
    /// doesn't see two modals at once).
    remote_image_prompt: Option<RemoteImagePrompt>,
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
    /// memory; `Always` also writes back to `config.image.remote_policy`.
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
    /// Dirty-buffer guard shown before navigating away from an unsaved
    /// document.  `Some(prompt)` means the modal is currently
    /// displayed; click / key events are absorbed by the modal until
    /// dismissed.
    dirty_guard: Option<DirtyGuardPrompt>,
    /// Phase 8 — target of the link currently under the mouse
    /// pointer, updated on every `MouseEventKind::Moved` event.
    /// Phase 9 will render this (plus the link's `title`) on the hint
    /// line.  Until then the field is wired through but not displayed.
    hovered_link: Option<LinkTarget>,
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
/// viewport height, and a prefetch margin, return the URLs whose
/// rendered rows intersect the near-viewport window.  Lifted out of
/// the App so it can be unit-tested without constructing a terminal.
///
/// Order is preserved (document order) so that during a scroll, the
/// image that enters the window first is the one that gets dispatched
/// first — small but measurable fairness win on slow connections.
fn urls_in_viewport_window(
    image_blocks: &[crate::document::ImageBlockInfo],
    source_map: &crate::document::SourceMap,
    scroll: usize,
    doc_height: usize,
    margin: usize,
) -> Vec<String> {
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
                Some(info.url.clone())
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
    ) -> Result<Self> {
        // Leak the theme so it can be stored as `&'static Theme`.  This is
        // intentional: the theme lives for the duration of the process.
        // `Theme::from_file` handles the monochrome fallback internally so
        // `NoColour` terminals never emit colour escapes regardless of the
        // theme file's contents.
        let monochrome = capabilities.colour_depth == ColourDepth::NoColour;
        let theme: &'static Theme = Box::leak(Box::new(Theme::from_file(&theme_file, monochrome)));

        // Table drag handles depend on mouse reporting; disable them on
        // terminals that don't deliver mouse events so we never render inert
        // gutter glyphs.
        if !capabilities.mouse {
            config.table.show_drag_handles = false;
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
        let editor = EditorState::new_with_image_config(
            buffer,
            theme,
            config.editor.preserve_blank_lines,
            config.editor.visual_line_nav,
            config.image.max_height,
            config.image.max_width,
            image_font_size,
        );

        // PreviewView borrows `editor.parsed.lines` at render time, so
        // no per-event clone is needed and the constructor is now
        // parameterless.  This removed the dominant per-event allocation
        // hotspot on large preview-mode documents.
        let view_state = EditorViewState::new();

        // Decide whether to show the capability-notice on startup.
        let startup_notice = build_startup_notice(&capabilities, &config);
        let remote_image_prompt = build_remote_image_prompt(&editor, &config);
        let wheel_step = config.editor.mouse_scroll_lines;

        Ok(Self {
            config,
            keybindings,
            theme,
            capabilities,
            file_path,
            editor,
            view_state,
            should_quit: false,
            startup_notice,
            remote_image_prompt,
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
            dirty_guard: None,
            hovered_link: None,
        })
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
                    // Phase 8: `open::that` finished in a worker.  Only
                    // log — Phase 9 will surface failures on the hint
                    // line.
                    if let Err(msg) = result {
                        tracing::warn!(target: "link", error = %msg, "link open failed");
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
    /// - `editor.parsed_dirty_at + PARSED_DEBOUNCE` — wake to flush a
    ///   deferred re-parse after a typing burst pauses.
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
        push(self.editor.parsed_dirty_at.map(|t| t + PARSED_DEBOUNCE));
        push(self.resize_quiesce_at);
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
        let keymap = KeyMap::build(&self.keybindings)?;

        loop {
            // Flush any deferred re-parse whose debounce window has
            // elapsed.  Typing bursts defer the re-parse so the UI
            // thread doesn't rebuild the whole ParsedDoc on every
            // keystroke; this is the point where we pay that cost
            // once the user pauses.
            if self
                .editor
                .parsed_dirty_at
                .is_some_and(|t| t.elapsed() >= PARSED_DEBOUNCE)
            {
                self.editor.flush_parsed_if_dirty();
                self.needs_draw = true;
            }

            // Resize-quiesce: once the burst of Resize events from a
            // terminal-drag has settled, clear the suppression flag
            // and request a single redraw at the final dimensions.
            if self.resize_quiesce_at.is_some_and(|t| t <= Instant::now()) {
                self.resize_quiesce_at = None;
                self.needs_draw = true;
            }

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
            let doc_height_lines = term_size.height.saturating_sub(1) as usize;
            self.last_area_width = term_size.width;
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
                let show_handles = self.config.table.show_drag_handles;
                let editor_ref = &mut self.editor;
                let theme_ref = self.theme;
                let capabilities_ref = &self.capabilities;
                let view_state_ref = &mut self.view_state;
                let notice_ref = self.startup_notice.as_mut();
                // Only show the remote prompt once the capability notice
                // has been dismissed so the user never sees two modals
                // stacked.
                let remote_prompt_ref = if notice_ref.is_none() {
                    self.remote_image_prompt.as_mut()
                } else {
                    None
                };
                // Phase 8 dirty-guard takes priority over the remote
                // prompt so the user's link-follow action isn't
                // overshadowed by a startup-ish prompt.
                let dirty_guard_ref = if notice_ref.is_none() && remote_prompt_ref.is_none() {
                    self.dirty_guard.as_mut()
                } else {
                    None
                };
                terminal.draw(|frame| {
                    let view = EditorView {
                        state: editor_ref,
                        theme: theme_ref,
                        filename: &filename,
                        show_table_handles: show_handles,
                        capabilities: capabilities_ref,
                        is_scrolling,
                    };
                    frame.render_stateful_widget(view, frame.area(), view_state_ref);
                    if let Some(notice) = notice_ref {
                        let modal = ModalView {
                            title: "Terminal capabilities",
                            body: &notice.body,
                            buttons: &notice.buttons,
                            theme: theme_ref,
                        };
                        frame.render_stateful_widget(modal, frame.area(), &mut notice.state);
                    } else if let Some(prompt) = remote_prompt_ref {
                        let modal = ModalView {
                            title: "Remote Images",
                            body: &prompt.body,
                            buttons: &prompt.buttons,
                            theme: theme_ref,
                        };
                        frame.render_stateful_widget(modal, frame.area(), &mut prompt.state);
                    } else if let Some(guard) = dirty_guard_ref {
                        let modal = ModalView {
                            title: "Unsaved changes",
                            body: &guard.body,
                            buttons: &guard.buttons,
                            theme: theme_ref,
                        };
                        frame.render_stateful_widget(modal, frame.area(), &mut guard.state);
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

            // Dirty-guard modal absorbs all input while it's visible.
            // Evaluated before the other modal checks so the guard (which
            // is opened on user action, not startup) always takes
            // precedence.
            if self.dirty_guard.is_some() {
                if let Event::Key(key) = &event {
                    if key.kind == KeyEventKind::Press {
                        let doc_w = term_size.width as usize;
                        let doc_h = term_size.height.saturating_sub(1) as usize;
                        self.handle_dirty_guard_key(*key, doc_h, doc_w);
                        self.needs_draw = true;
                    }
                }
                continue;
            }

            // Remote-image prompt absorbs all input while it's visible
            // (and only when the startup notice has already been dismissed).
            if self.startup_notice.is_none() && self.remote_image_prompt.is_some() {
                if let Event::Key(key) = &event {
                    if key.kind == KeyEventKind::Press {
                        self.handle_remote_image_prompt_key(*key);
                        self.needs_draw = true;
                    }
                }
                continue;
            }

            // Startup notice absorbs all input while it's visible.
            if self.startup_notice.is_some() {
                if let Event::Key(key) = &event {
                    if key.kind == KeyEventKind::Press {
                        self.handle_startup_notice_key(*key);
                        self.needs_draw = true;
                    }
                }
                continue;
            }

            // `term_size` was already fetched at the top of the loop
            // (for `dispatch_visible_image_decodes`); reuse here.
            let viewport_height = term_size.height as usize;
            let doc_height = viewport_height.saturating_sub(1); // minus status bar
            let doc_width = term_size.width as usize;
            let doc_area = Rect {
                x: 0,
                y: 0,
                width: term_size.width,
                height: term_size.height.saturating_sub(1),
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
                        if mouse_ops::hit_test_clickable(&self.editor, rel_col, rel_row, doc_width)
                        {
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
            let mut handler = DefaultHandler::new(&keymap);
            if let Some(action) = handler.handle_event(event, &self.editor) {
                // Phase 8 — App-level actions intercepted BEFORE the
                // generic `edit_ops::apply` dispatch.  Link navigation
                // mutates App state (nav stack, file load) that
                // `EditorState` doesn't own, so these paths stay here.
                let handled = self.handle_app_action(&action, doc_height, doc_width);
                if !handled {
                    let scroll_before = self.editor.scroll;
                    let quit = edit_ops::apply(&mut self.editor, action, doc_height, doc_width);
                    if quit {
                        self.should_quit = true;
                    }
                    if self.editor.scroll != scroll_before {
                        self.mark_scrolling();
                    }
                    // Edit actions may have set `pending_link_follow`
                    // (FollowLinkUnderCursor doesn't hit `handle_app_action`
                    // path only when the action ISN'T App-level).
                    if let Some(target) = self.editor.pending_link_follow.take() {
                        self.follow_link(target, doc_height, doc_width);
                    }
                }
                self.needs_draw = true;
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
            _ => false,
        }
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
        self.editor.scroll = line_idx;
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
        let new_editor = EditorState::new_with_image_config(
            buffer,
            self.theme,
            self.config.editor.preserve_blank_lines,
            self.config.editor.visual_line_nav,
            self.config.image.max_height,
            self.config.image.max_width,
            self.capabilities
                .image_picker
                .as_ref()
                .map(|p| p.font_size())
                .unwrap_or((10, 20)),
        );
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
        self.editor.scroll = dest
            .scroll
            .min(self.editor.parsed.line_count().saturating_sub(1));
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
        let body = vec![
            format!("{} has unsaved changes.", display),
            String::new(),
            format!("Opening {} will abandon them.", pending.display()),
            String::new(),
            "What would you like to do?".to_owned(),
        ];
        self.dirty_guard = Some(DirtyGuardPrompt {
            body,
            buttons: vec![
                ModalButton::new("Save"),
                ModalButton::new("Discard"),
                ModalButton::new("Cancel"),
            ],
            state: ModalState::new(),
            pending,
        });
    }

    /// Apply a keypress to the dirty-guard modal.  The three buttons
    /// map to: 0 = Save (persist, continue), 1 = Discard (continue
    /// without saving), 2 = Cancel (abort).  Escape is Cancel.
    fn handle_dirty_guard_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        doc_height: usize,
        doc_width: usize,
    ) {
        let Some(guard) = self.dirty_guard.as_mut() else {
            return;
        };
        let num_buttons = guard.buttons.len();
        let response = guard.state.handle_key(&key, num_buttons);
        match response {
            ModalResponse::Continue => {}
            ModalResponse::Cancelled => {
                self.dirty_guard = None;
            }
            ModalResponse::ButtonPressed(idx) => {
                let pending = guard.pending.clone();
                self.dirty_guard = None;
                match idx {
                    0 => {
                        if self.editor.buffer.save_file().is_ok() {
                            self.editor.dirty = false;
                            self.navigate_to_file(pending);
                        } else {
                            tracing::warn!(target: "link", "save-before-navigate failed");
                        }
                    }
                    1 => {
                        self.editor.dirty = false;
                        self.navigate_to_file(pending);
                    }
                    _ => {}
                }
                // Whichever button ran, kick a redraw so the modal
                // disappears.
                self.editor.ensure_cursor_visible(doc_height, doc_width);
            }
        }
    }

    // ── End Phase 8 navigation helpers ────────────────────────────────────

    /// Apply a keypress targeted at the startup-notice modal.  On dismissal
    /// clears the notice and, if the user chose "Don't show this again",
    /// persists the preference.
    fn handle_startup_notice_key(&mut self, key: crossterm::event::KeyEvent) {
        let Some(notice) = self.startup_notice.as_mut() else {
            return;
        };
        let num_buttons = notice.buttons.len();
        let response = notice.state.handle_key(&key, num_buttons);
        match response {
            ModalResponse::Continue => {}
            ModalResponse::Cancelled => {
                self.startup_notice = None;
            }
            ModalResponse::ButtonPressed(idx) => {
                // Button index 1 is "Don't show this again" (see
                // `build_startup_notice`).  Any other button closes the
                // modal without touching config.
                if idx == 1 {
                    self.config.editor.suppress_capability_warnings = true;
                    if let Err(e) = self.config.save() {
                        tracing::warn!(error = %e, "failed to persist capability-warning preference");
                    }
                }
                self.startup_notice = None;
            }
        }
    }

    /// Apply a keypress to the remote-image prompt.  The four buttons
    /// map to:
    ///   * index 0 "Yes" — set `session_allow_remote = true` in-memory;
    ///     config is unchanged (policy stays `Ask` for future sessions).
    ///   * index 1 "No" — dismiss without fetching; policy unchanged.
    ///   * index 2 "Always" — persist `RemoteImagePolicy::Always`, allow
    ///     future sessions to fetch automatically.
    ///   * index 3 "Never" — persist `RemoteImagePolicy::Never`, all
    ///     remote images stay as placeholders.
    ///
    /// For "Yes"/"Always" we dispatch decode jobs immediately after so
    /// newly-allowed URLs start loading without waiting for a keypress.
    fn handle_remote_image_prompt_key(&mut self, key: crossterm::event::KeyEvent) {
        let Some(prompt) = self.remote_image_prompt.as_mut() else {
            return;
        };
        let num_buttons = prompt.buttons.len();
        let response = prompt.state.handle_key(&key, num_buttons);
        match response {
            ModalResponse::Continue => {}
            ModalResponse::Cancelled => {
                // Escape → treat as "No" — just dismiss, no policy change.
                // Remote decodes will continue to fail with `RemoteBlocked`.
                self.remote_image_prompt = None;
            }
            ModalResponse::ButtonPressed(idx) => {
                // Button order defined in `build_remote_image_prompt`:
                //   0 → Yes    (session-only, no config change)
                //   1 → No     (dismiss, no config change)
                //   2 → Always (persist `RemoteImagePolicy::Always`)
                //   3 → Never  (persist `RemoteImagePolicy::Never`)
                let allow_now = match idx {
                    0 => {
                        self.session_allow_remote = true;
                        true
                    }
                    1 => false,
                    2 => {
                        self.config.image.remote_policy = crate::config::RemoteImagePolicy::Always;
                        if let Err(e) = self.config.save() {
                            tracing::warn!(error = %e, "failed to persist remote_policy=Always");
                        }
                        true
                    }
                    _ => {
                        self.config.image.remote_policy = crate::config::RemoteImagePolicy::Never;
                        if let Err(e) = self.config.save() {
                            tracing::warn!(error = %e, "failed to persist remote_policy=Never");
                        }
                        false
                    }
                };
                self.remote_image_prompt = None;
                if allow_now {
                    // Newly-allowed URLs may have been recorded as failed
                    // with `RemoteBlocked`; clear those entries so the
                    // decode workers re-attempt them.
                    self.editor.images.clear_failures_for_remote_reopening();
                    self.dispatch_image_decodes();
                }
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
        let urls: Vec<String> = self
            .editor
            .parsed
            .image_blocks
            .iter()
            .map(|i| i.url.clone())
            .collect();
        self.dispatch_image_decodes_for(&urls);
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
        let urls = urls_in_viewport_window(
            &self.editor.parsed.image_blocks,
            &self.editor.parsed.source_map,
            scroll,
            doc_height,
            VIEWPORT_DISPATCH_MARGIN,
        );
        self.dispatch_image_decodes_for(&urls);
    }

    /// Shared dispatch primitive: spawn a worker thread for each URL
    /// that `ImageCache::request` accepts as new.
    fn dispatch_image_decodes_for(&mut self, urls: &[String]) {
        if !self.config.image.enabled {
            return;
        }
        let Some(tx) = self.app_tx.clone() else {
            return;
        };
        let doc_path = self.file_path.clone();
        let remote_policy = self.config.image.remote_policy;
        let session_allow_remote = self.session_allow_remote;
        // Give the worker the target ceiling AND the terminal's font-size
        // so the decoded image is pre-resized to fit within
        // `max_cells × font_size` pixels.  After this the main thread's
        // protocol never has to resize — every render call at the same
        // target area is a no-op beyond the first encode.
        let max_cells = Some((
            self.config.image.max_width as u16,
            self.config.image.max_height as u16,
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

        for url in urls {
            if !self.editor.images.request(url) {
                continue;
            }
            let tx = tx.clone();
            let doc_path = doc_path.clone();
            let url = url.clone();
            let scratch_picker = scratch_picker.clone();
            std::thread::spawn(move || {
                let result = crate::image::resolve(
                    &url,
                    doc_path.as_deref(),
                    remote_policy,
                    session_allow_remote,
                    max_cells,
                    font_size,
                );
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
                    Err(err) => AppEvent::ImageReady(Err((url.clone(), err.to_string()))),
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

/// Build the remote-image prompt when the document references at least
/// one `http(s)://` image and the policy is `Ask`.  Returns `None` when
/// there are no remote URLs, image rendering is disabled, or the policy
/// has been pinned to `Always` / `Never`.
fn build_remote_image_prompt(editor: &EditorState, config: &Config) -> Option<RemoteImagePrompt> {
    if !config.image.enabled {
        return None;
    }
    if config.image.remote_policy != crate::config::RemoteImagePolicy::Ask {
        return None;
    }
    let has_remote = editor
        .parsed
        .image_blocks
        .iter()
        .any(|b| crate::image::loader::is_remote(&b.url));
    if !has_remote {
        return None;
    }
    let body = vec![
        "This document references one or more remote images.".to_owned(),
        "Fetching them sends HTTP requests from your machine.".to_owned(),
        String::new(),
        "Would you like edamame to fetch remote images?".to_owned(),
    ];
    // Button order is intentional: the leftmost button is the default
    // focus (`ModalState::new` sets `focused = 0`).  "Yes" allows the
    // fetch for this session only and is the safe default if the user
    // hammers Enter without reading.  "No" dismisses without fetching.
    // The persistent choices ("Always", "Never") come after.
    Some(RemoteImagePrompt {
        body,
        buttons: vec![
            ModalButton::new("Yes"),
            ModalButton::new("No"),
            ModalButton::new("Always"),
            ModalButton::new("Never"),
        ],
        state: ModalState::new(),
    })
}

/// Construct the startup-notice modal when there's something worth reporting
/// and the user hasn't asked to suppress it.
fn build_startup_notice(caps: &Capabilities, config: &Config) -> Option<StartupNotice> {
    if config.editor.suppress_capability_warnings {
        return None;
    }
    if !caps.has_missing_features() {
        return None;
    }
    let mut body = caps.missing_features_summary();
    body.push(String::new());
    body.push("Affected features will be disabled automatically.".to_owned());
    Some(StartupNotice {
        body,
        buttons: vec![
            ModalButton::new("Ok"),
            ModalButton::new("Don't show this again"),
        ],
        state: ModalState::new(),
    })
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

/// True when the editor's cursor sits inside a table block.  Mirrors
/// the check used by `edit_ops::cursor_in_table`; re-implemented here
/// to keep the App free of a cross-module private dep.
fn cursor_in_table(state: &EditorState) -> bool {
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    let source = state.buffer.contents();
    crate::editor::table_edit::find_table_at(&source, cursor_byte).is_some()
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
        let urls = urls_in_viewport_window(&blocks, &map, 0, 20, 0);
        assert_eq!(urls, vec!["a.png".to_owned()]);
    }

    #[test]
    fn viewport_window_keeps_images_inside_prefetch_margin() {
        let (blocks, map) = fixture(&[("a.png", 5), ("b.png", 50), ("c.png", 200)]);
        // scroll=0, doc_height=20, margin=40 → window = [0, 60),
        // picks up a.png (row 5) and b.png (row 50).
        let urls = urls_in_viewport_window(&blocks, &map, 0, 20, 40);
        assert_eq!(urls, vec!["a.png".to_owned(), "b.png".to_owned()]);
    }

    #[test]
    fn viewport_window_respects_scroll_offset() {
        let (blocks, map) = fixture(&[("a.png", 5), ("b.png", 50), ("c.png", 200)]);
        // scroll=180, doc_height=20, margin=10 → window = [170, 210),
        // picks up c.png only.
        let urls = urls_in_viewport_window(&blocks, &map, 180, 20, 10);
        assert_eq!(urls, vec!["c.png".to_owned()]);
    }

    #[test]
    fn viewport_window_preserves_document_order() {
        // Doc order is a, b, c; they're all in the window.  The
        // returned Vec must keep that order so the first-into-window
        // image is dispatched first on slow connections.
        let (blocks, map) = fixture(&[("c.png", 2), ("a.png", 0), ("b.png", 1)]);
        let urls = urls_in_viewport_window(&blocks, &map, 0, 10, 0);
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
        let urls = urls_in_viewport_window(&blocks, &map, 100, 20, 10);
        assert!(urls.is_empty());
    }

    #[test]
    fn viewport_window_handles_saturating_scroll_underflow() {
        // scroll < margin would underflow a signed subtract; we use
        // saturating_sub so the window just clamps at 0.
        let (blocks, map) = fixture(&[("a.png", 0), ("b.png", 5)]);
        let urls = urls_in_viewport_window(&blocks, &map, 2, 3, 100);
        // Window = [0, 105), both images inside.
        assert_eq!(urls, vec!["a.png".to_owned(), "b.png".to_owned()]);
    }
}
