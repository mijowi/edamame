pub mod modal;
mod theme_fallback;

mod actions;
mod autosave;
mod cursor_style;
mod diff_advance;
pub mod difftool;
pub use difftool::{diff_label, is_markdown_pair, read_side};
mod event_loop;
mod external_editor;
mod file_changed;
mod flash;
mod frame_timer;
mod image_dispatch;
mod nav;
mod pointer;
mod post_upgrade;
mod search;
mod section_jump;
mod update_check;
mod update_notice;

#[cfg(test)]
mod test_utils;

use std::collections::VecDeque;
use std::io::Stdout;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use std::time::Instant;

use anyhow::Result;
use crossterm::event::Event;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::config::sections::{DEFAULT_HANDLER, VIM_HANDLER};
use crate::config::{Config, ConfigWarning, KeyBindingOverrides, KeyMap, Theme, ThemeFile};
use crate::document::Buffer;
use crate::editor::{mouse_ops, EditorState};
use crate::input::{MouseDispatcher, VimState};
use crate::terminal::{Capabilities, ColorDepth, PointerShape};
use crate::ui::{EditorViewState, HintChord};
use crate::watcher::{FileWatcher, WatchedEvent};

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
    /// Worker-thread report that `open::that` finished on a
    /// URL or non-Markdown local file.  Currently only logged; a later
    /// change will surface failures on the hint line.
    LinkOpenResult(std::result::Result<(), String>),
    /// Watcher worker delivered an event for the open file.  A
    /// `Change` is routed through the own-write content-hash filter
    /// before being acted on; a `ReadError` is surfaced via a
    /// dismissable warning modal.  See [`App::handle_watcher_event`].
    Watcher(WatchedEvent),
    /// Worker-thread report from the GitHub latest-release check —
    /// spawned once at startup (throttled, opt-out) and on every
    /// explicit "Check for updates".  `Ok` carries the tag plus the
    /// already-bounded release notes; `Err(message)` is logged and
    /// surfaces as a failure state on an explicit check only.  See
    /// [`update_check`].
    ReleaseCheckResult(std::result::Result<update_check::ReleaseInfo, String>),
    /// Background HTML-export worker finished.  The `u64` is the export
    /// generation id of the spawning modal, so a result from a superseded
    /// export (the user dismissed the modal and opened a fresh one while the
    /// worker ran) is routed to the hint line instead of hijacking the new
    /// modal.  When the still-open `ExportHtmlModal` matches that id it is
    /// advanced to its success / error phase
    /// (`ExportHtmlModal::on_export_done`).  `Ok(path)` is the written file;
    /// `Err(message)` is a presentable failure string.
    ExportDone(u64, crate::export::ExportOutcome),
}

/// Generic modal prompt hosted on the hint line.
/// The `handler` fn is the single callback invoked when one of the chord keys
/// is pressed — it receives the triggering `KeyCode` so the same prompt type
/// can host multiple-button flows.
#[allow(dead_code)] // first consumer lands later
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
    /// Name shown in the status bar in place of a file name, set only by
    /// the difftool presentation (`--diff`).
    ///
    /// That session opens no file — `file_path` is `None` so nothing can
    /// start a watcher on, or save over, the temp files git hands us —
    /// which would otherwise leave the status bar reading `[No file]` for
    /// every file in a `git difftool` loop, exactly when knowing which one
    /// is under review matters most.
    diff_label: Option<String>,
    /// Set when the user ended a difftool session with `Quit` rather
    /// than `Esc`, so `main` can stop the whole walk once the terminal
    /// is back.
    ///
    /// The two exits mean different things across a multi-file review:
    /// `Esc` is "done with this file, show me the next", `Ctrl-Q` is
    /// "Quit diff". Acting on the second is `main`'s job and not
    /// this type's, because it happens *after* `terminal::restore` —
    /// see [`crate::app::difftool::stop_walk`], which ends the walk by
    /// signalling the process group rather than by an exit code git
    /// discards by default.
    diff_stop_walk: bool,
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
    /// text selection (the text-selection fallthrough); the other variants carry
    /// the table-specific row / column / border drags.  Cleared on
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
    /// Counterpart of [`Self::session_allow_remote`] for a *declined*
    /// remote prompt (`No`, or Escape).  The two session-answer flags
    /// for images and diagrams are `Option<bool>`, so a decline is
    /// recorded there by construction; `session_allow_remote` is a bare
    /// bool, and without this companion flag a decline would be
    /// indistinguishable from "never asked" — so every document opened
    /// later in the session (link follow, back/forward) would re-queue
    /// the prompt the user just dismissed.  `Never` persists to
    /// `config.images.remote_policy` instead.
    session_remote_declined: bool,
    /// Sender for the encoder worker's channel, retained so a *newly
    /// loaded document* can be given one.
    ///
    /// `ImageCache::get_protocol_pair` returns `None` without a sender
    /// attached, and `paint_images` then draws the `[Image: alt]`
    /// placeholder — so a cache that never receives it renders no image
    /// at all, however healthy its decodes.  The cache is owned by
    /// `EditorState`, and `load_file_into_editor` builds a whole new
    /// one per document, so attaching once in `spawn_event_threads`
    /// (which is all that used to happen) covered the startup document
    /// and nothing else: every file opened by following a link or
    /// navigating back showed reserved rows and a placeholder while its
    /// images decoded perfectly in the background.  Kept here so the
    /// swap site can re-attach.
    resize_tx: Option<mpsc::Sender<ratatui_image::thread::ResizeRequest>>,
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
    /// Initialized to `true` so the first iteration paints the opening
    /// frame.  Without this gate, the 60 ms `recv_timeout` would fire a
    /// full redraw ~17 times per second even with no input — the
    /// dominant cause of idle CPU previously.
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
    /// Most recent document-area dimensions, cached each frame in
    /// [`App::prepare_viewport`].  Modal click handlers (`fn
    /// handle_click`) don't receive the live `DocDims` the keystroke
    /// path does, so callbacks that need them — e.g. the dirty-guard
    /// re-scrolling the cursor into view after navigating — read the
    /// last-known values from here instead.  `0` until the first frame.
    pub(crate) last_doc_height: usize,
    pub(crate) last_doc_width: usize,
    /// FIFO of terminal events pulled off the channel ahead of time —
    /// either by `drain_pending_image_ready` (which uses `try_recv` and
    /// can't put events back) or by `drain_pending_key_events` (which
    /// reads ahead so a burst of keystrokes can be coalesced into a
    /// single dispatch).  `next_event` pops from the front before
    /// consulting `rx`, preserving the user's event timeline — a
    /// Resize sandwiched between two keystrokes is still processed
    /// between them.
    pending_events: VecDeque<Event>,
    /// Back-stack: `NavigateBack` pops the most-recent entry
    /// and restores it.  A new link-follow clears `nav_forward`
    /// (browser semantics).
    nav_back: Vec<NavEntry>,
    /// Forward-stack: `NavigateBack` pushes the current state
    /// here so `NavigateForward` can redo the navigation.
    nav_forward: Vec<NavEntry>,
    /// Raw URL of the link currently under the mouse pointer (as
    /// written in the source), updated on every `MouseEventKind::Moved`
    /// event.  While `Some`, the hint line replaces its chord row with
    /// the URL — browser-status-bar style.  Only Preview and Rendered
    /// produce hovers: the hit-test keys on UNDERLINED link spans,
    /// which Raw mode never renders.
    hovered_link: Option<String>,
    /// Transient message overlayed on the hint line.  Non-
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
    /// Initialized in [`Self::run`] alongside the read-thread spawn.
    read_paused: Option<Arc<AtomicBool>>,
    /// Active hint-line prompt (first consumer lands later).
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
    /// Wall-clock instant of the most-recently observed buffer edit
    /// for autosave debounce.  Reset on every dirtying edit (detected
    /// via `Buffer::version()` change in [`App::tick_autosave`]); cleared
    /// when the buffer flips clean (manual save, autosave success,
    /// reload, …).  When set, the run loop wakes at
    /// `t + config.editor.autosave_idle_ms` and persists the buffer.
    autosave_pending_since: Option<Instant>,
    /// Last-observed `Buffer::version()`.  Used by `tick_autosave` to
    /// detect that an edit has happened since the previous tick and
    /// restart the debounce window.
    autosave_last_seen_version: u64,
    /// Debounce timer for the section picker's live-preview scroll.
    /// Set whenever the user navigates the picker; cleared once
    /// [`Self::tick_section_jump`] fires or the modal closes.  Without
    /// the debounce, holding `↓` on the picker would thrash the
    /// viewport for every focus change.
    section_jump_pending_since: Option<Instant>,
    /// Target scroll value to apply when the section-jump debounce
    /// elapses.  `None` between jumps; overwritten on every preview so
    /// only the most-recent target is kept.
    section_jump_target_scroll: Option<usize>,
    /// Set when a diff hunk is accepted/rejected: holds the focused
    /// hunk's resolved state visible for [`diff_advance::DIFF_ADVANCE_DELAY`]
    /// before focus auto-advances to the next pending hunk.  `None`
    /// when no advance is pending.  See [`App::tick_diff_advance`].
    diff_advance_pending_since: Option<Instant>,
    /// Set when a search-flow replace lands: keeps the replacement
    /// visible for [`search::SEARCH_ADVANCE_DELAY`] before focus
    /// auto-advances to the next match.  `None` when no advance is
    /// pending.  See [`App::tick_search_advance`].
    search_advance_pending_since: Option<Instant>,
    /// Active filesystem watcher for the open file, if any.  `None`
    /// until the run loop calls [`App::start_file_watcher`] after the
    /// initial buffer load.  Multi-tab work later swaps this for a
    /// per-tab map — the `Option<Box<dyn FileWatcher>>` shape is
    /// chosen so that refactor only touches this field and the
    /// watch / unwatch call sites.
    pub(crate) watcher: Option<Box<dyn FileWatcher>>,
    /// Content hash of the last-observed-on-disk bytes for the open
    /// file.  Updated from three sources: initial load, every
    /// successful save, and every accepted incoming `FileChanged`.
    /// Consulted by the `FileChanged` arm to suppress echoes of our
    /// own writes (the hash matches → drop the event silently).
    /// `None` only during the brief window between `App::new()` and
    /// the initial load — `Some` for any open file thereafter.
    pub(crate) last_disk_hash: Option<u64>,
    /// Session cache of the GitHub release check: `None` until the
    /// first fetch resolves, then the last resolved status.  Shared by
    /// the silent startup check and every explicit one, so the update
    /// modal has something to render the instant it opens even while a
    /// fresh fetch is in flight.
    latest_release: Option<update_check::ReleaseStatus>,
    /// True while a release-check worker is in flight, so a second
    /// trigger can't spawn a duplicate request.
    release_check_in_flight: bool,
    /// Whether the startup check should run, decided in [`App::new`]
    /// by the pure `update_check::network_check_due` and consumed by
    /// [`App::spawn_startup_update_check`] from `tick_timers`.  The
    /// policy decision and the network action are split across that
    /// boundary for two reasons: `App::new` has no channel to send a
    /// result on, and the check waits out the first-run welcome modal —
    /// the surface where the user answers the `check_for_updates`
    /// question in the first place.
    startup_update_check_due: bool,
    /// Last `markdown::highlight::warm_generation()` this session acted
    /// on.  Grammar compilation happens on a worker, so a code block in
    /// a not-yet-compiled language renders plain; when the counter moves
    /// `tick_syntax_warm` reparses so the colour lands.  Seeded from the
    /// live counter rather than 0 — a second `App` in one process (the
    /// test suite) would otherwise see a spurious change on its first
    /// tick and reparse for nothing — and that read is taken *before*
    /// this session's own first render queues anything, or a compile
    /// landing in between would be seeded in as the starting value and
    /// never seen as a change.
    syntax_warm_generation: u64,
    /// Set when a *startup* check finds a release worth announcing,
    /// cleared when `tick_update_notice` finds an empty modal stack and
    /// pushes it.  An explicit check never touches this — it opens its
    /// own modal directly.
    pending_update_notice: Option<update_check::ReleaseInfo>,
    /// True while the in-flight check is the silent startup one.  Only
    /// that flavor may arm `pending_update_notice`; an explicit check
    /// must never queue a notice behind the modal the user just
    /// opened.
    update_check_is_startup: bool,
    /// A `#section` named on the command line
    /// (`edamame notes.md#setup`), parked until the first frame knows
    /// the document's dimensions and consumed there by
    /// [`App::apply_startup_anchor`].  `None` on every launch that
    /// named no section, and after the jump has been made.
    pub(crate) startup_anchor: Option<String>,
    /// Vim modal-editing state.  `Some` iff `config.modal.handler ==
    /// "vim"`; `None` for the default handler, which keeps every vim
    /// code path inert for existing users.  Survives across keystrokes
    /// (counts, pending operators, the active sub-mode) and is read by
    /// the UI for the mode badge.
    vim: Option<VimState>,
}

/// Apply the App-level configuration that every freshly built
/// [`EditorState`] needs, at both of the two sites that build one:
/// `App::new` for the startup document and
/// [`App::load_file_into_editor`] for every document opened after it.
///
/// It exists because those two sites drifted, twice, and both drifts
/// were invisible until someone opened a second document: the newer
/// site never applied `cursor_blink` (so a `cursor_blink = false`
/// config quietly started blinking again after following a link), and
/// separately never re-attached the encoder-worker sender (so images
/// decoded and then painted as placeholders forever — see
/// [`App::resize_tx`]).  Anything a new `EditorState` needs from
/// `Config` belongs here, not at a call site.
///
/// It is also the **config-reload** path: `external_editor` re-applies
/// it after the user hand-edits `config.toml`, where the editor being
/// configured is an existing one rather than a fresh one.  That is why
/// every field is written unconditionally rather than only defaulted —
/// the reload replaced `self.config` wholesale, and a field left alone
/// here silently keeps its launch-time value while the flash claims the
/// configuration was updated.
///
/// `images_layout_on` / `diagrams_layout_on` are passed rather than
/// derived because the callers know them differently: `App::new`
/// has no `self` to ask `images_layout_enabled()` yet.  The reparse at
/// the end is conditional for the same reason it always was — the
/// constructor already parsed once, and only a layout flag flipping off
/// invalidates that parse.
fn configure_new_editor(
    editor: &mut EditorState,
    config: &Config,
    images_layout_on: bool,
    diagrams_layout_on: bool,
) {
    editor.cursor_blink = crate::editor::CursorBlink::from_config(
        config.editor.cursor_blink,
        config.editor.cursor_blink_ms,
    );
    if !images_layout_on {
        editor.images_enabled = false;
    }
    if !diagrams_layout_on {
        editor.diagrams_enabled = false;
    }
    editor.set_row_striping(config.table.row_striping);
    editor.set_big_h1(config.editor.big_h1);
    editor.set_syntax_highlighting(config.editor.syntax_highlighting);
    if !images_layout_on || !diagrams_layout_on {
        editor.refresh_parsed();
    }
    leave_preview_under_vim(config, editor);
}

/// Vim modal editing replaces Preview with vim-Normal as the resting
/// non-editing mode, so no editor may come to rest in Preview while it
/// is on.  Every `EditorState` is born in `Mode::Preview`, and a new one
/// is built for *every* document — so without this a file opened by
/// link, back-navigation or an `$EDITOR` return landed in Preview, with
/// its "Press any key to edit" prelude and browse-only chord row, while
/// the status bar still read `NORMAL`.  One helper rather than a copy
/// per site: the copies are how the per-document path was missed.
fn leave_preview_under_vim(config: &Config, editor: &mut EditorState) {
    if config.modal.handler == VIM_HANDLER && editor.mode == crate::editor::Mode::Preview {
        editor.mode = crate::editor::Mode::Rendered;
    }
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
        // Substitute an indexed-color theme when this terminal can't do
        // 24-bit.  Every other built-in (and essentially every user
        // theme) is authored in RGB; an indexed terminal quantizes those
        // values, which routinely collapses fg and bg into the same cube
        // entry — including inside the very modals that would explain
        // the problem.  So the swap happens here, before the first
        // frame, rather than being offered as advice.  Nothing is
        // persisted: see `theme_fallback` and `Config::save`.
        let mut theme_file = theme_file;
        let theme_downgrade = theme_fallback::apply(&mut config, &capabilities).map(|d| {
            theme_file = d.theme_file;
            (d.configured, d.substituted)
        });

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
        // Seed the watcher's own-write filter from the just-loaded
        // bytes so the very first inotify event after startup (which
        // some editors synthesize when other tools touch the file
        // around launch time) is compared against a real hash, not
        // `None`.  An empty `[New File]` buffer hashes to a stable
        // value too — fine, the next on-disk change will differ.
        let initial_disk_hash = Some(seahash::hash(buffer.contents().as_bytes()));

        // Pass the probed font-size through so the renderer can compute
        // aspect-aware row counts for decoded images.  Fall back to
        // ratatui-image's Halfblocks default (10, 20) when no image
        // picker was detected — any image render will be a no-op on
        // those terminals anyway (capabilities.image_protocol == None).
        let image_font_size = capabilities
            .image_picker
            .as_ref()
            .map(|p| {
                // ratatui-image 11 returns a `FontSize` struct here; we
                // carry font size as a `(width, height)` tuple internally.
                let fs = p.font_size();
                (fs.width, fs.height)
            })
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
        // When the user has persisted `images.enabled = "never"`, image
        // blocks must collapse to just the `[Image: alt]` placeholder —
        // no reserved rows beneath.  The `Ask` / `Always` paths leave
        // the layout reserved so the prompt / live decode populates the
        // area; the declined-session flip happens in the prompt handler.
        // A terminal without 24-bit color collapses them too, for the
        // same reason `media_renderable` refuses to decode there — the
        // quantized output reads as broken, not degraded.  Session-only:
        // `config.images.enabled` is left untouched, so the user's
        // choice returns with them to a capable terminal.
        let images_off = !capabilities.full_color()
            || matches!(config.images.enabled, crate::config::ImagesEnabled::Never);
        let diagrams_off = !capabilities.full_color()
            || matches!(
                config.diagrams.enabled,
                crate::config::DiagramsEnabled::Never
            );
        // Start the grammar warm worker before the first render.  It
        // does two jobs off the critical path: deserializing the syntax
        // dump (~2 ms), and compiling each grammar a document names
        // (~9 ms, ~18 ms for Rust's) — the latter being the one
        // highlighting cost that is a function of how many *languages*
        // are in play rather than of how much text is, so neither size
        // cap bounds it.
        //
        // This has to sit *above* `configure_new_editor`, which is where
        // the first highlighted render happens — it flips
        // `syntax_highlighting` on, and `set_syntax_highlighting`
        // reparses.  Spawned below that line the thread has nothing left
        // to get ahead of on any document containing a code block, which
        // is the very case it exists for.
        //
        // Skipped when the setting is off: nothing would ever ask it for
        // a grammar, and the dump load would be pure waste.  Turning the
        // setting on mid-session still works — the first warm request
        // spawns the worker itself.
        //
        // Read the counter *before* either the worker or the first
        // render exists, and carry that value to the field below.
        // Reading it at the struct literal instead leaves a window: the
        // render inside `configure_new_editor` queues the document's
        // grammars, and a compile landing before the seed is taken would
        // be captured as the starting value — `tick_syntax_warm` then
        // sees no change, ever, and the block stays plain until some
        // unrelated reparse. A read-only viewing session never has one.
        let syntax_warm_generation = crate::markdown::highlight::warm_generation();
        if config.editor.syntax_highlighting {
            crate::markdown::highlight::spawn_warm_worker();
        }
        configure_new_editor(&mut editor, &config, !images_off, !diagrams_off);

        // Vim modal editing is opt-in via `config.modal.handler`.  The
        // Preview escape that comes with it is handled by
        // `configure_new_editor` above, shared with every document
        // opened later, so the `NORMAL` badge shows from the first frame.
        let vim = (config.modal.handler == VIM_HANDLER).then(VimState::default);

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
        // The one-time post-upgrade notice, read from the bundled
        // `CHANGELOG.md`.  Unlike the release-check notice this waits
        // on nothing, so it joins the synchronous ordering below
        // instead of being parked for `tick_update_notice`.  Only the
        // *decision* happens here: the matching `last_version_seen`
        // write is `App::run`'s, because `App::new` must stay
        // disk-free — `test_utils::make_app` builds an `App` through
        // it, mostly without a config-isolation guard.  See
        // `app::post_upgrade`.
        let post_upgrade_modal = post_upgrade::startup_notice(
            &config.editor.last_version_seen,
            config.editor.show_welcome,
        );
        let config_warning_modal = modal::ConfigWarningModal::from_warnings(&config_warnings);
        let capabilities_notice = if suppress_legacy_prompts {
            None
        } else {
            modal::TerminalCapabilitiesModal::from_capabilities(
                &capabilities,
                &config.editor.seen_terminal_fingerprints,
            )
        };
        // A first visit to a terminal that also can't render the user's
        // theme is one story, not two.  When both notices would fire the
        // capabilities summary — the more complete of the two — absorbs
        // the substitution's explanation and the standalone modal is
        // dropped; otherwise (terminal already seen, or the notice is
        // suppressed behind the welcome) the standalone modal carries it.
        let (capabilities_notice, theme_downgrade_modal) =
            match (capabilities_notice, theme_downgrade) {
                (Some(notice), Some((configured, substituted))) => (
                    Some(notice.with_theme_downgrade(configured, substituted)),
                    None,
                ),
                (notice, Some((configured, substituted))) => (
                    notice,
                    Some(modal::ThemeDowngradeModal::new(configured, substituted)),
                ),
                (notice, None) => (notice, None),
            };
        // Also suppressed below 24-bit color: `media_renderable` refuses
        // to decode there, so asking the user to opt in to something we
        // will then decline to draw is worse than staying quiet.
        let media_capable = capabilities.full_color();
        let images_enabled_prompt = if suppress_legacy_prompts || !media_capable {
            None
        } else {
            modal::ImagesEnabledPromptModal::from_state(&editor, &config)
        };
        let diagrams_enabled_prompt = if suppress_legacy_prompts || !media_capable {
            None
        } else {
            modal::DiagramsEnabledPromptModal::from_state(&editor, &config)
        };
        let remote_image_prompt = if suppress_legacy_prompts || !media_capable {
            None
        } else {
            modal::RemoteImagePromptModal::from_state(&editor, &config)
        };
        let wheel_step = config.editor.mouse_scroll_lines;

        // Push the queued startup-time modals onto the stack in
        // reverse-priority order so the highest-priority one is on
        // top.  Order shown to the user when present: config-warning →
        // theme-downgrade → welcome → post-upgrade → startup-notice →
        // images-enabled → diagrams-enabled → remote-image.  The
        // post-upgrade notice sits under the welcome because a first
        // run has nothing to be welcomed *back* from.  The two rarely
        // coincide — `show_welcome` being on is what routes a launch
        // with no recorded version to the silent branch — but they are
        // not exclusive: a user who leaves the welcome's "Show on next
        // launch" toggle on keeps it true, and after an upgrade gets
        // both.  Stacking is then correct, and this is the order they
        // should be read in.  The theme-downgrade and
        // startup-notice are mutually exclusive (see above): when both
        // apply, the notice carries the downgrade text.  The legacy prompts (everything below
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
        if let Some(m) = capabilities_notice {
            modal_stack.push(Box::new(m));
        }
        if let Some(m) = post_upgrade_modal {
            modal_stack.push(Box::new(m));
        }
        if let Some(m) = welcome_modal {
            modal_stack.push(Box::new(m));
        }
        // Above the welcome: the theme substitution explains the colors
        // every other modal is being drawn in, so it should be read
        // first.  Below the config warning, which reports a broken file.
        if let Some(m) = theme_downgrade_modal {
            modal_stack.push(Box::new(m));
        }
        if let Some(m) = config_warning_modal {
            modal_stack.push(Box::new(m));
        }

        // Warm the diagram pipeline's font caches off the
        // critical path.  Two caches load fonts on first call:
        //   * `mermaid_rs_renderer`'s internal fontdb (for text layout
        //     metrics during SVG generation),
        //   * our own shared `fontdb::Database` used by `usvg` when
        //     rasterizing the SVG to PNG.
        // Both scan OS font dirs (~100–300 ms each, worse cold).
        // `diagram::warm_fontdb` primes both so the first real diagram
        // render doesn't pay them.  Without this warmup (or with
        // per-render loads as the previous implementation did), a
        // document with N diagrams spawns N concurrent font scans —
        // the dominant source of initial-load lag.
        // Skipped when images are configured as `Never` — no diagram
        // will ever decode, so the warmup would be wasted IO.
        // Skipped when images are configured as `Never`, and when the
        // terminal can't render them at all — no diagram will ever
        // decode, so the warmup would be wasted IO.
        if media_capable
            && !matches!(
                config.diagrams.enabled,
                crate::config::DiagramsEnabled::Never
            )
        {
            std::thread::spawn(crate::diagram::warm_fontdb);
        }

        // Decide the startup update check here, while `config` is still
        // owned locally and before any modal can have been dismissed —
        // but don't act on it: `app_tx` doesn't exist until `run()`
        // spawns the event threads, so the network half waits for
        // `spawn_startup_update_check`.
        let startup_update_check_due = update_check::network_check_due(
            config.editor.check_for_updates,
            config.editor.last_update_check,
            update_check::now_unix(),
        );

        Ok(Self {
            config,
            keybindings,
            theme,
            capabilities,
            file_path,
            diff_label: None,
            diff_stop_walk: false,
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
            session_remote_declined: false,
            resize_tx: None,
            app_tx: None,
            last_scroll_at: None,
            last_draw_at: None,
            last_area_width: 0,
            last_doc_height: 0,
            last_doc_width: 0,
            images_dirty: false,
            needs_draw: true,
            resize_quiesce_at: None,
            pending_events: VecDeque::new(),
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
            autosave_pending_since: None,
            autosave_last_seen_version: 0,
            section_jump_pending_since: None,
            section_jump_target_scroll: None,
            diff_advance_pending_since: None,
            search_advance_pending_since: None,
            watcher: None,
            last_disk_hash: initial_disk_hash,
            latest_release: None,
            release_check_in_flight: false,
            startup_update_check_due,
            syntax_warm_generation,
            pending_update_notice: None,
            update_check_is_startup: false,
            startup_anchor: None,
            vim,
        })
    }

    /// Record the `#section` the command line named, to be applied on
    /// the first frame.  A builder rather than a `new` parameter: the
    /// startup anchor is one caller's concern (`main`), and every other
    /// construction site — the tests included — wants the default.
    #[must_use]
    pub fn with_startup_anchor(mut self, anchor: Option<String>) -> Self {
        self.startup_anchor = anchor;
        self
    }

    /// Enable or disable vim modal editing for the running session,
    /// keeping `config.modal.handler` and the editor mode in sync.
    /// Mirrors the startup wiring in `App::new` so a mid-session toggle
    /// (e.g. from the welcome modal) takes effect immediately instead of
    /// waiting for the next launch.
    pub(crate) fn set_vim_enabled(&mut self, enabled: bool) {
        if enabled {
            self.config.modal.handler = VIM_HANDLER.into();
            if self.vim.is_none() {
                self.vim = Some(VimState::default());
            }
            // Vim-Normal replaces Preview as the resting mode, so leave
            // Preview behind exactly as startup does.
            leave_preview_under_vim(&self.config, &mut self.editor);
        } else {
            self.config.modal.handler = DEFAULT_HANDLER.into();
            self.vim = None;
        }
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
                // A Term event pulled via `try_recv` cannot be put
                // back into the channel, so push it onto
                // `pending_events` — the next loop iteration consults
                // that queue before `recv_timeout` so events stay in
                // their original order.  We keep draining so queued
                // image-ready events behind the first key aren't
                // starved.
                Ok(AppEvent::Term(e)) => self.pending_events.push_back(e),
                Ok(ev) => self.handle_async_event(ev),
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
        // Records the running version for the post-upgrade notice.
        // Here rather than in `App::new` because it writes to disk and
        // the constructor must not — see `app::post_upgrade`.
        self.stamp_last_version_seen();
        let rx = self.spawn_event_threads();
        self.start_file_watcher();
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

            if matches!(event, Event::FocusGained | Event::FocusLost) {
                let focused = matches!(event, Event::FocusGained);
                if self.editor.terminal_focused != focused {
                    self.editor.terminal_focused = focused;
                    // Reset the blink phase so the cursor reappears solid
                    // for a full interval on regaining focus, and so the
                    // last visible/hidden phase before focus loss doesn't
                    // determine what shows up on the next regain.
                    self.editor.cursor_blink.reset();
                    self.needs_draw = true;
                }
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

    /// True when a difftool session ended via `Quit` — see
    /// [`App::diff_stop_walk`].
    pub fn diff_stop_walk(&self) -> bool {
        self.diff_stop_walk
    }

    fn display_filename(&self) -> String {
        if let Some(label) = &self.diff_label {
            return label.clone();
        }
        match &self.file_path {
            Some(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned()),
            None => "[No file]".to_owned(),
        }
    }
}

#[cfg(test)]
mod vim_wiring_tests {
    use crate::config::{Config, KeyBindingOverrides, Theme};
    use crate::editor::Mode;
    use crate::terminal::Capabilities;

    use super::App;

    fn app_with_handler(handler: &str) -> App {
        let mut config = Config::default();
        config.modal.handler = handler.into();
        let theme_file = (&Theme::default()).into();
        App::new(
            config,
            KeyBindingOverrides::default(),
            theme_file,
            None,
            Capabilities::default(),
            Vec::new(),
        )
        .expect("build app")
    }

    #[test]
    fn vim_disabled_by_default() {
        let app = app_with_handler("default");
        assert!(app.vim.is_none(), "default handler must not enable vim");
    }

    #[test]
    fn vim_enabled_when_configured() {
        let app = app_with_handler("vim");
        assert!(app.vim.is_some(), "vim handler must enable vim state");
        // Vim never rests in Preview — startup switches to Rendered so
        // the NORMAL badge shows from the first frame.
        assert_eq!(app.editor.mode, Mode::Rendered);
    }

    #[test]
    fn a_document_opened_under_vim_never_lands_in_preview() {
        // Every document gets a fresh `EditorState`, born in Preview —
        // so a link follow / back-navigation used to drop a vim session
        // into Preview, complete with its "Press any key to edit" hint.
        let mut app = app_with_handler("vim");
        assert_eq!(app.editor.mode, Mode::Rendered);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("other.md");
        std::fs::write(&path, "# Other\n").expect("write");
        app.load_file_into_editor(path).expect("load");

        assert_eq!(
            app.editor.mode,
            Mode::Rendered,
            "a newly loaded document must not rest in Preview under vim"
        );
    }

    #[test]
    fn set_vim_enabled_mirrors_startup_wiring() {
        // Enabling mid-session (e.g. from the welcome modal) must reach
        // the exact state startup produces: vim state present, handler
        // string flipped, and Preview left behind for Rendered.
        let mut app = app_with_handler("default");
        assert!(app.vim.is_none());
        assert_eq!(app.editor.mode, Mode::Preview);

        app.set_vim_enabled(true);
        assert!(app.vim.is_some(), "vim state created");
        assert_eq!(app.config.modal.handler, "vim");
        assert_eq!(
            app.editor.mode,
            Mode::Rendered,
            "Preview gives way to vim-Normal"
        );
    }

    #[test]
    fn set_vim_enabled_false_clears_vim() {
        // Disabling tears down vim state and restores the default handler
        // string so a later `Config::save` writes `handler = "default"`.
        let mut app = app_with_handler("vim");
        assert!(app.vim.is_some());

        app.set_vim_enabled(false);
        assert!(app.vim.is_none(), "vim state cleared");
        assert_eq!(app.config.modal.handler, "default");
    }

    #[test]
    fn set_vim_enabled_true_is_idempotent() {
        // A redundant enable (already-vim session re-saving the welcome
        // modal) must preserve the live vim state, not swap in a fresh
        // default — the guard is `if self.vim.is_none()`.
        let mut app = app_with_handler("vim");
        app.vim.as_mut().expect("vim active").pending_g = true;
        app.editor.mode = Mode::Raw;

        app.set_vim_enabled(true);
        assert!(
            app.vim.as_ref().expect("vim still active").pending_g,
            "existing vim state is preserved, not reset"
        );
        // A non-Preview mode is left untouched — only Preview is rewritten.
        assert_eq!(app.editor.mode, Mode::Raw);
    }

    // ── CP6: VisualLine clipboard widening (Ctrl-C / Ctrl-X / Ctrl-V) ──

    use crate::config::Action;
    use crate::document::{Buffer, Selection};
    use crate::input::VimSubMode;

    /// Install a VisualLine selection over `text` spanning the given charwise
    /// `anchor`/`active` (deliberately ragged, mid-line endpoints).
    fn app_in_visual_line(text: &str, anchor: usize, active: usize) -> App {
        let mut app = app_with_handler("vim");
        app.editor.replace_buffer(Buffer::from_str(text));
        app.editor.selection = Some(Selection { anchor, active });
        let vim = app.vim.as_mut().expect("vim active");
        vim.sub_mode = VimSubMode::VisualLine;
        vim.visual_anchor = Some(anchor);
        app
    }

    #[test]
    fn visual_line_copy_grabs_whole_lines_without_snapping_selection() {
        // Charwise span from mid-line-0 to mid-line-1; `Ctrl-C` must copy
        // both whole lines (matching the VisualLine highlight).
        let mut app = app_in_visual_line("alpha\nbeta\ngamma", 2, 7);
        app.dispatch_action(Action::Copy, 40, 80);
        assert_eq!(app.editor.kill_ring, "alpha\nbeta\n");
        // The persistent selection is restored to the charwise span — never
        // snapped — and Visual continues.
        let sel = app.editor.selection.expect("selection restored");
        assert_eq!((sel.anchor, sel.active), (2, 7));
        assert_eq!(app.vim.as_ref().unwrap().sub_mode, VimSubMode::VisualLine);
    }

    #[test]
    fn visual_line_cut_removes_whole_lines_and_exits_visual() {
        let mut app = app_in_visual_line("alpha\nbeta\ngamma", 2, 7);
        app.dispatch_action(Action::Cut, 40, 80);
        assert_eq!(app.editor.buffer.contents(), "gamma");
        assert_eq!(app.editor.kill_ring, "alpha\nbeta\n");
        assert_eq!(app.vim.as_ref().unwrap().sub_mode, VimSubMode::Normal);
        assert!(app.editor.selection.is_none());
    }

    #[test]
    fn visual_line_paste_replaces_whole_lines_and_exits_visual() {
        // Copy first so the paste source is deterministic whichever way
        // `clipboard_text` resolves — `Copy` writes the OS clipboard *and*
        // the kill-ring with the same linewise payload.
        let mut app = app_in_visual_line("alpha\nbeta\ngamma", 2, 2);
        app.dispatch_action(Action::Copy, 40, 80);
        assert_eq!(app.editor.kill_ring, "alpha\n", "test premise");
        // Re-anchor the V-LINE selection on line 1 (again mid-line, so the
        // charwise span is not the line) and paste over it.
        app.editor.selection = Some(Selection {
            anchor: 8,
            active: 8,
        });
        app.vim.as_mut().unwrap().visual_anchor = Some(8);
        app.dispatch_action(Action::Paste, 40, 80);
        assert_eq!(
            app.editor.buffer.contents(),
            "alpha\nalpha\ngamma",
            "the whole highlighted line is replaced, not the empty charwise span"
        );
        assert_eq!(app.vim.as_ref().unwrap().sub_mode, VimSubMode::Normal);
        assert!(app.editor.selection.is_none());
    }

    #[test]
    fn charwise_visual_copy_grabs_the_inclusive_span() {
        // In charwise Visual the widening is vim's inclusive one: `Ctrl-C`
        // copies the highlighted span *plus* the char under the cursor, and
        // leaves the stored half-open selection alone so a continued Visual
        // session keeps its anchor.
        let mut app = app_with_handler("vim");
        app.editor.replace_buffer(Buffer::from_str("alpha\nbeta"));
        let sel = Selection {
            anchor: 0,
            active: 2,
        };
        app.editor.selection = Some(sel);
        app.vim.as_mut().unwrap().sub_mode = VimSubMode::Visual;
        app.dispatch_action(Action::Copy, 40, 80);
        assert_eq!(app.editor.kill_ring, "alp");
        assert_eq!(
            app.editor.selection,
            Some(sel),
            "Copy never snaps `selection`"
        );
    }

    // ── CP9: Ex commands driven end-to-end through `dispatch_single_key` ───

    use crate::app::event_loop::DocDims;
    use crate::config::KeyMap;
    use crate::document::Buffer as Buf;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;

    fn ex_dims() -> DocDims {
        DocDims {
            doc_height: 24,
            doc_width: 80,
            doc_area: Rect::new(0, 0, 80, 24),
        }
    }

    /// Type a full `:`-command (the `:`, the body, then Enter) into `app`
    /// through the real key-dispatch entry point.
    fn run_ex(app: &mut App, body: &str) {
        let keymap = KeyMap::build(&KeyBindingOverrides::default()).expect("keymap");
        let dims = ex_dims();
        let press = |app: &mut App, code: KeyCode| {
            app.dispatch_single_key(
                Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
                &keymap,
                &dims,
            );
        };
        press(app, KeyCode::Char(':'));
        for c in body.chars() {
            press(app, KeyCode::Char(c));
        }
        press(app, KeyCode::Enter);
    }

    #[test]
    fn ex_write_saves_the_buffer_to_disk() {
        let mut app = app_with_handler("vim");
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        app.editor.buffer = Buf::for_new_file(tmp.path());
        app.editor.buffer.insert(0, "hello vim");
        app.editor.dirty = true;

        run_ex(&mut app, "w");

        assert!(!app.editor.dirty, ":w clears the dirty flag");
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read back");
        assert_eq!(on_disk, "hello vim", ":w writes the buffer to disk");
    }

    #[test]
    fn ex_quit_on_clean_buffer_quits_immediately() {
        let mut app = app_with_handler("vim");
        assert!(!app.editor.dirty);
        let modals_before = app.modal_stack.len();
        run_ex(&mut app, "q");
        assert!(app.should_quit, ":q on a clean buffer quits");
        assert_eq!(
            app.modal_stack.len(),
            modals_before,
            "no quit-confirm pushed when the buffer is clean"
        );
    }

    #[test]
    fn ex_quit_on_dirty_buffer_opens_the_quit_confirm() {
        let mut app = app_with_handler("vim");
        app.editor.buffer.insert(0, "x");
        app.editor.dirty = true;
        // Drop any startup modal so the assertion below sees only the
        // quit-confirm the `:q` itself opens.
        while app.modal_stack.pop().is_some() {}
        run_ex(&mut app, "q");
        assert!(!app.should_quit, "dirty :q must not quit silently");
        assert!(
            app.modal_stack
                .contains::<crate::app::modal::QuitConfirmModal>(),
            "dirty :q opens the quit-confirm modal"
        );
    }

    #[test]
    fn ex_substitute_global_flashes_and_edits_through_the_app() {
        let mut app = app_with_handler("vim");
        app.editor.replace_buffer(Buffer::from_str("foo\nfoo"));
        run_ex(&mut app, "%s/foo/bar/g");
        assert_eq!(app.editor.buffer.contents(), "bar\nbar");
        let msg = app.transient.as_ref().expect("substitution flash");
        assert_eq!(msg.text, "2 substitutions");
    }

    #[test]
    fn ex_parse_error_flashes_through_the_app() {
        let mut app = app_with_handler("vim");
        app.editor.replace_buffer(Buffer::from_str("hello"));
        run_ex(&mut app, "nope");
        let msg = app.transient.as_ref().expect("parse-error flash");
        assert_eq!(msg.text, "Not an editor command: nope");
        assert_eq!(app.editor.buffer.contents(), "hello");
    }
}
