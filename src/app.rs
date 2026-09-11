pub mod modal;
mod theme_fallback;

mod actions;
mod autosave;
mod cursor_style;
mod diff_advance;
pub mod difftool;
mod docs;
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
    /// An image decode finished.  `Err` records the failure so it isn't retried every render.
    ImageReady(Result<crate::image::LoadedImage, (String, String)>),
    /// An encoder `ResizeRequest` finished.  `Err` only keeps the pending-request FIFO balanced:
    /// the placeholder stays visible until a later frame re-enqueues the encode.
    ProtocolReady(Result<ratatui_image::thread::ResizeResponse, ratatui_image::errors::Errors>),
    /// `open::that` finished on a URL or non-Markdown file.  Currently only logged.
    LinkOpenResult(std::result::Result<(), String>),
    /// A watcher event for the open file.  See [`App::handle_watcher_event`].
    Watcher(WatchedEvent),
    /// The GitHub latest-release check finished.  `Err` surfaces as a failure state on an
    /// explicit check only.  See [`update_check`].
    ReleaseCheckResult(std::result::Result<update_check::ReleaseInfo, String>),
    /// The export worker finished.  The `u64` is the spawning modal's generation id, so a result
    /// from a superseded export goes to the hint line instead of hijacking the modal now open.
    ExportDone(u64, crate::export::ExportOutcome),
}

/// Generic modal prompt hosted on the hint line.  `handler` receives the triggering `KeyCode`, so
/// one prompt type can host a multiple-button flow.
#[allow(dead_code)] // first consumer lands later
pub struct HintPrompt {
    pub prompt: String,
    pub chords: Vec<HintChord>,
    pub handler: fn(&mut App, crossterm::event::KeyCode),
}

/// The application: owns all state and drives the event loop.
pub struct App {
    config: Config,
    /// Overrides from `keybindings.toml`, held so `KeyMap::build` can run in `run()` alongside
    /// capability detection.
    keybindings: KeyBindingOverrides,
    theme: &'static Theme,
    capabilities: Capabilities,
    file_path: Option<PathBuf>,
    /// Status-bar name for the difftool presentation (`--diff`), which opens no file: `file_path`
    /// stays `None` so nothing can watch or save over git's temp files, and without this the bar
    /// would read `[No file]` for every file in a `git difftool` loop.
    diff_label: Option<String>,
    /// The manual page currently open, when the buffer came from [`crate::docs`] rather than
    /// disk.  Mutually exclusive with `file_path` for the same pathless reason as `diff_label`,
    /// and the gate that makes `follow_link` resolve a relative link against the embedded set.
    open_doc: Option<crate::docs::DocId>,
    /// Set when a difftool session ended with `Quit` rather than `Esc` — "quit the whole walk"
    /// rather than "show me the next file".  Acting on it is `main`'s job because it happens after
    /// `terminal::restore`; see [`crate::app::difftool::stop_walk`].
    diff_stop_walk: bool,
    editor: EditorState,
    view_state: EditorViewState,
    should_quit: bool,
    /// Session-only override for the images switch, set by `Yes` / `No` on the prompt; `None`
    /// defers to `config.images.enabled`.  `Always` / `Never` persist to config instead.
    session_images_enabled: Option<bool>,
    /// Diagrams mirror of [`Self::session_images_enabled`], kept separate so the two prompts can
    /// be answered independently.
    pub(crate) session_diagrams_enabled: Option<bool>,
    /// Click-count tracking and drag state for mouse input.
    mouse: MouseDispatcher,
    /// Active drag target, set on mouse-down, read by each `Drag`, cleared on `Release`.
    drag_target: Option<mouse_ops::DragTarget>,
    /// True while the pointer is in the scrollbar gutter, so the thumb renders in its hover style.
    scrollbar_hover: bool,
    /// Last pointer shape requested, so an unchanged shape doesn't write an OSC 22 escape on
    /// every mouse-move.
    last_pointer_shape: PointerShape,
    /// Set by `Yes` / `Always` on the remote-load prompt, so later loads don't re-prompt.
    /// Memory-only; `Always` also writes back to `config.images.remote_policy`.
    session_allow_remote: bool,
    /// Counterpart of [`Self::session_allow_remote`] for a *declined* prompt.  Without it a
    /// decline is indistinguishable from "never asked", and every document opened later in the
    /// session would re-queue the prompt the user just dismissed.
    session_remote_declined: bool,
    /// Sender for the encoder worker's channel, retained so a *newly loaded document* can be
    /// given one.  A cache without it renders placeholders however healthy its decodes, and a new
    /// cache is built per document — attaching only at startup left every later file in that state.
    resize_tx: Option<mpsc::Sender<ratatui_image::thread::ResizeRequest>>,
    /// Sender for the main loop's channel, so worker threads can push events.  `None` until
    /// `run` creates it.
    app_tx: Option<mpsc::Sender<AppEvent>>,
    /// Timestamp of the last scroll change.  `is_scrolling` reads it to fall back to halfblocks
    /// mid-scroll rather than re-encoding Sixel / iTerm2 graphics per frame.  Cleared on resize.
    last_scroll_at: Option<Instant>,
    /// An `ImageReady` updated the cache but the parse hasn't caught up to the new row count.
    /// Consumed next iteration, coalescing N simultaneous decodes into one `refresh_parsed`.
    images_dirty: bool,
    /// The redraw gate: the loop only draws when this is true.  Without it the `recv_timeout`
    /// would redraw ~17 times a second at idle, the dominant idle-CPU cost.
    needs_draw: bool,
    /// While `Some`, a `Resize` burst is in progress and draws are suppressed; each Resize
    /// extends the deadline, so a slow drag paints only once it settles.
    resize_quiesce_at: Option<Instant>,
    /// Timestamp of the last draw, for the frame throttle: events arrive faster than we want to
    /// draw (every wheel tick is one), so a draw within `MIN_FRAME_INTERVAL` is skipped.
    last_draw_at: Option<Instant>,
    /// Latest document-area width, refreshed each iteration.  The decode worker pre-renders its
    /// halfblocks scratch at this width, sparing the UI thread a 5-20 ms sync encode on first
    /// paint.  It is the *clamped* doc width, not the terminal width, or the scratch would be
    /// resized on that first paint anyway.
    last_area_width: u16,
    /// Document-area dimensions cached each frame, for modal click handlers, which don't receive
    /// the live `DocDims` the keystroke path does.
    pub(crate) last_doc_height: usize,
    pub(crate) last_doc_width: usize,
    /// Terminal events read off the channel ahead of time (by the image drain, which can't put
    /// them back, or by the key-coalescing read-ahead).  `next_event` pops from here before
    /// consulting `rx`, so the user's event timeline is preserved.
    pending_events: VecDeque<Event>,
    /// Back-stack; a new link-follow clears `nav_forward`, as a browser does.
    nav_back: Vec<NavEntry>,
    /// Forward-stack, pushed by `NavigateBack`.
    nav_forward: Vec<NavEntry>,
    /// Raw URL under the pointer, as written in the source.  While `Some`, the hint line shows it
    /// in place of the chord row.  Only Preview and Rendered produce hovers — the hit-test pairs
    /// link-styled spans with the block's AST links, and Raw renders neither.
    hovered_link: Option<String>,
    /// Transient hint-line message, set by [`App::flash`].  Non-error kinds auto-expire after
    /// `config.editor.transient_ms`; errors stick until dismissed.
    transient: Option<TransientMessage>,
    /// Live keymap, built once at startup and mutated in place by the keybinds overlay.
    keymap: Option<KeyMap>,
    /// Set by the settings overlay's "Open config.toml" action, drained by the run loop, which
    /// holds the `Terminal` handle needed to suspend / resume the TUI around the editor.
    pending_open_config_in_editor: bool,
    /// Same deferral as `pending_open_config_in_editor`, for the palette's editor action.
    pending_open_file_in_editor: bool,
    /// Same deferral again, for a theme file; the active theme is reloaded once the editor exits.
    pub(crate) pending_open_theme_in_editor: Option<std::path::PathBuf>,
    /// Pause flag for the crossterm read thread: while set it sleeps instead of polling stdin,
    /// releasing it to a child process such as `$EDITOR`.  Without it both read the same bytes off
    /// the controlling terminal, dropping keystrokes and leaking escape sequences into the editor
    /// (an OSC 11 response is how `1;rgb:...` ended up at the top of users' `config.toml`).
    read_paused: Option<Arc<AtomicBool>>,
    /// Active hint-line prompt, rendered in place of the default chords; Escape dismisses.
    hint_prompt: Option<HintPrompt>,
    /// Active modal stack; render priority and input absorption are stack-order driven.
    modal_stack: ModalStack,
    /// The named file didn't exist; `App::run` flashes "[New File]" once at startup.
    started_with_new_file: bool,
    /// Autosave debounce anchor: reset on every dirtying edit, cleared when the buffer goes
    /// clean.  While set, the run loop wakes at `t + config.editor.autosave_idle_ms` to save.
    autosave_pending_since: Option<Instant>,
    /// Last-observed `Buffer::version()`, so `tick_autosave` can spot an edit since the last tick.
    autosave_last_seen_version: u64,
    /// Debounce deadline for figure (diagram / `$$...$$` math) render dispatch.  While `now < t`
    /// the per-frame dispatch skips figure-sourced blocks (plain images unaffected), so a
    /// keystroke's throwaway content-hashed URL isn't rendered mid-burst — see
    /// [`DIAGRAM_RENDER_DEBOUNCE`](image_dispatch::DIAGRAM_RENDER_DEBOUNCE).  Re-armed on each edit;
    /// paired with [`Self::diagram_render_watch_version`] for edit-edge detection.
    diagram_render_hold_until: Option<Instant>,
    /// Last-observed `Buffer::version()` for the figure-render debounce.  `None` until the first
    /// dispatch pass so opening a document doesn't count as an edit and delay its first render.
    diagram_render_watch_version: Option<u64>,
    /// Debounce for the section picker's live-preview scroll; without it, holding `↓` thrashes
    /// the viewport on every focus change.
    section_jump_pending_since: Option<Instant>,
    /// Scroll target for that debounce; overwritten on every preview, so only the latest is kept.
    section_jump_target_scroll: Option<usize>,
    /// Keeps a just-decided hunk's resolved state visible for
    /// [`diff_advance::DIFF_ADVANCE_DELAY`] before focus advances.  See [`App::tick_diff_advance`].
    diff_advance_pending_since: Option<Instant>,
    /// The search-flow mirror of [`Self::diff_advance_pending_since`], for a landed replacement.
    search_advance_pending_since: Option<Instant>,
    /// Filesystem watcher for the open file; `None` until [`App::start_file_watcher`] runs.  The
    /// boxed-trait shape is chosen so multi-tab work can swap it for a per-tab map without
    /// touching anything but this field and the watch / unwatch sites.
    pub(crate) watcher: Option<Box<dyn FileWatcher>>,
    /// The OS clipboard.  Production reads the real one; a test swaps in
    /// a clipboard whose contents it wrote down, which is the only way
    /// the paste path can be exercised without borrowing the developer's
    /// clipboard (or racing parallel tests over it).  See
    /// [`crate::clipboard`].
    clipboard: Box<dyn crate::clipboard::ClipboardSource>,
    /// Content hash of the last-observed-on-disk bytes for the open
    /// file.  Updated from three sources: initial load, every
    /// successful save, and every accepted incoming `FileChanged`.
    /// Consulted by the `FileChanged` arm to suppress echoes of our
    /// own writes (the hash matches → drop the event silently).
    /// `None` only during the brief window between `App::new()` and
    /// the initial load — `Some` for any open file thereafter.
    /// Hash of the last-observed on-disk bytes, updated on load, save, and every accepted
    /// `FileChanged`.  The `FileChanged` arm compares against it to drop echoes of our own writes.
    pub(crate) last_disk_hash: Option<u64>,
    /// Last resolved release-check status, shared by the startup and explicit checks so the
    /// update modal has something to render the instant it opens.
    latest_release: Option<update_check::ReleaseStatus>,
    /// True while a release-check worker is in flight, so a second
    /// trigger can't spawn a duplicate request.
    release_check_in_flight: bool,
    /// Whether the startup check should run, decided in [`App::new`] and acted on later by
    /// [`App::spawn_startup_update_check`].  Split because `App::new` has no channel to send a
    /// result on, and because the check waits out the welcome modal — where the user answers the
    /// `check_for_updates` question in the first place.
    startup_update_check_due: bool,
    /// Last `markdown::highlight::warm_generation()` acted on.  Grammars compile on a worker, so
    /// a block in a not-yet-compiled language renders plain until the counter moves and
    /// `tick_syntax_warm` reparses.  Seeded from the live counter (a second `App` in one process
    /// would otherwise reparse for nothing) and read *before* this session's first render, or a
    /// compile landing in between would be seeded in and never seen as a change.
    syntax_warm_generation: u64,
    /// A release a *startup* check found worth announcing, pushed once `tick_update_notice` sees
    /// an empty modal stack.  An explicit check opens its own modal instead.
    pending_update_notice: Option<update_check::ReleaseInfo>,
    /// True while the in-flight check is the silent startup one — the only flavor that may arm
    /// `pending_update_notice`.
    update_check_is_startup: bool,
    /// A `#section` named on the command line, parked until the first frame knows the document's
    /// dimensions and consumed there by [`App::apply_startup_anchor`].
    pub(crate) startup_anchor: Option<String>,
    /// Vim modal-editing state; `Some` iff the vim handler is configured, which keeps every vim
    /// code path inert otherwise.  Carries counts, pending operators, and the active sub-mode.
    vim: Option<VimState>,
    /// The vim session parked while a read-only document is open.  Preview and vim-Normal are
    /// alternative *resting* states that never coexist (see [`leave_preview_under_vim`]), so a
    /// read-only document suspends vim rather than fighting it — parked, not destroyed, so the
    /// session survives a trip into the manual.  [`App::sync_vim_suspension`] is the only writer.
    parked_vim: Option<VimState>,
}

/// Apply the App-level configuration every freshly built [`EditorState`] needs.  **Anything a new
/// editor needs from `Config` belongs here, not at a call site**: the two building sites drifted
/// twice, and both drifts were invisible until someone opened a second document.
///
/// It is also the **config-reload** path, re-applied after the user hand-edits `config.toml`.  That
/// is why every field is written unconditionally: the reload replaced `self.config` wholesale, and
/// a field left alone would silently keep its launch-time value while the flash claims otherwise.
///
/// The layout flags are passed rather than derived because `App::new` has no `self` to ask yet.
/// The trailing reparse is conditional because the constructor already parsed once, and only a
/// layout flag flipping *off* invalidates that parse.
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
    editor.set_reflow(config.editor.reflow);
    editor.set_math_preview(config.figures.math_preview);
    if !images_layout_on || !diagrams_layout_on {
        editor.refresh_parsed();
    }
    leave_preview_under_vim(config, editor);
}

/// Vim-Normal replaces Preview as the resting non-editing mode, so no editor may rest in Preview
/// while vim is on.  Every `EditorState` is born in Preview and one is built per document, so
/// without this a file opened by link or back-navigation landed in Preview — "Press any key to
/// edit" and all — while the status bar read `NORMAL`.  One helper, because the copies are how the
/// per-document path was missed.
fn leave_preview_under_vim(config: &Config, editor: &mut EditorState) {
    // A read-only document is the one editor that *must* rest in Preview: vim is suspended for
    // its duration, so "force Preview" and "suspend vim" are one decision, not two.
    if editor.readonly {
        return;
    }
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
        // The theme is leaked so it can be held as `&'static Theme`: it is read from App, every
        // widget, and `EditorState` on the hot render path, where a lifetime parameter would
        // propagate through dozens of types and an `Arc` would cost a deref on every read.  It is
        // a few KB, and `apply_active_theme` leaks a fresh one per (rare, user-initiated) theme
        // change — see there for the alternatives considered.
        //
        // Substitute an indexed-color theme when this terminal can't do 24-bit: essentially every
        // theme is authored in RGB, and quantizing routinely collapses fg and bg into the same
        // cube entry — including inside the modals that would explain the problem.  Nothing is
        // persisted; see `theme_fallback`.
        let mut theme_file = theme_file;
        let theme_downgrade = theme_fallback::apply(&mut config, &capabilities).map(|d| {
            theme_file = d.theme_file;
            (d.configured, d.substituted)
        });

        let monochrome = capabilities.color_depth == ColorDepth::NoColor;
        let theme: &'static Theme = Box::leak(Box::new(Theme::from_file(&theme_file, monochrome)));

        // Table buttons need mouse reporting; without it they would be inert gutter glyphs.
        if !capabilities.mouse {
            config.table.show_buttons = false;
        }

        // A non-existent path opens an empty buffer bound to it, as vim and nano do, so the first
        // save creates the file.
        let mut started_with_new_file = false;
        let buffer = match &file_path {
            Some(path) if path.exists() => Buffer::load_file(path)?,
            Some(path) => {
                started_with_new_file = true;
                Buffer::for_new_file(path)
            }
            None => Buffer::new(),
        };
        // Seed the watcher's own-write filter so the first inotify event after startup is
        // compared against a real hash rather than `None`.
        let initial_disk_hash = Some(seahash::hash(buffer.contents().as_bytes()));

        // The renderer needs the font size for aspect-aware image row counts.  The fallback is
        // ratatui-image's Halfblocks default; image rendering is a no-op on those terminals anyway.
        let image_font_size = capabilities
            .image_picker
            .as_ref()
            .map(|p| {
                // ratatui-image returns a `FontSize`; we carry a `(width, height)` tuple.
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
        // Under `Never`, image blocks collapse to the placeholder with no reserved rows; `Ask` /
        // `Always` keep the rows for the prompt or the live decode to fill.  A terminal without
        // 24-bit color collapses them too, for the same reason `media_renderable` won't decode
        // there — the quantized output reads as broken, not degraded.  That is session-only, so
        // the user's choice returns with them to a capable terminal.
        let images_off = !capabilities.full_color()
            || matches!(config.images.enabled, crate::config::ImagesEnabled::Never);
        let diagrams_off = !capabilities.full_color()
            || matches!(config.figures.enabled, crate::config::FiguresEnabled::Never);
        // The grammar warm worker takes two costs off the critical path: deserializing the syntax
        // dump (~2 ms) and compiling each grammar a document names (~9-18 ms) — the latter being
        // the one highlighting cost that scales with how many *languages* are in play rather than
        // how much text, so neither size cap bounds it.
        //
        // It must be spawned *above* `configure_new_editor`, where the first highlighted render
        // happens; below that line it has nothing left to get ahead of on the documents it exists
        // for.  Skipped when highlighting is off — turning it on mid-session spawns the worker
        // from the first warm request.
        //
        // The generation counter is read here, before either the worker or the first render
        // exists.  Reading it at the struct literal instead would capture a compile that landed in
        // between as the starting value, and `tick_syntax_warm` would then never see a change.
        let syntax_warm_generation = crate::markdown::highlight::warm_generation();
        if config.editor.syntax_highlighting {
            crate::markdown::highlight::spawn_warm_worker();
        }
        configure_new_editor(&mut editor, &config, !images_off, !diagrams_off);

        // The Preview escape vim needs is handled by `configure_new_editor` above, shared with
        // every document opened later, so the `NORMAL` badge shows from the first frame.
        let vim = (config.modal.handler == VIM_HANDLER).then(VimState::default);

        // PreviewView borrows `editor.parsed.lines` at render time rather than cloning — this was
        // the dominant per-event allocation on large preview-mode documents.
        let view_state = EditorViewState::new();

        // Startup modals, each `None` when its precondition isn't met.  The first-run welcome
        // subsumes the four legacy prompts, which are skipped while it is pending so the user is
        // never double-prompted.
        let welcome_modal = modal::WelcomeModal::from_state(&capabilities, &config);
        let suppress_legacy_prompts = welcome_modal.is_some();
        // The one-time post-upgrade notice, from the bundled `CHANGELOG.md`.  It waits on
        // nothing, so it joins the ordering below rather than being parked for
        // `tick_update_notice`.  Only the *decision* happens here: the `last_version_seen` write
        // is `App::run`'s, because `App::new` must stay disk-free — `test_utils::make_app` builds
        // an `App` through it, mostly without a config-isolation guard.
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
        // A first visit to a terminal that also can't render the user's theme is one story, not
        // two: the capabilities summary absorbs the substitution's explanation when both fire,
        // and the standalone modal carries it otherwise.
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
        // Suppressed below 24-bit color too: asking the user to opt in to something
        // `media_renderable` will then decline to draw is worse than staying quiet.
        let media_capable = capabilities.full_color();
        let images_enabled_prompt = if suppress_legacy_prompts || !media_capable {
            None
        } else {
            modal::ImagesEnabledPromptModal::from_state(&editor, &config)
        };
        let diagrams_enabled_prompt = if suppress_legacy_prompts || !media_capable {
            None
        } else {
            modal::FiguresEnabledPromptModal::from_state(&editor, &config)
        };
        let remote_image_prompt = if suppress_legacy_prompts || !media_capable {
            None
        } else {
            modal::RemoteImagePromptModal::from_state(&editor, &config)
        };
        let wheel_step = config.editor.mouse_scroll_lines;

        // Pushed in reverse-priority order, so the user reads: config-warning → theme-downgrade →
        // welcome → post-upgrade → startup-notice → images-enabled → diagrams-enabled →
        // remote-image.  The post-upgrade notice sits under the welcome because a first run has
        // nothing to be welcomed *back* from; the two rarely coincide but are not exclusive, so
        // stacking them in that order is the reading order.
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
        // Above the welcome: the substitution explains the colors every other modal is drawn in.
        // Below the config warning, which reports a broken file.
        if let Some(m) = theme_downgrade_modal {
            modal_stack.push(Box::new(m));
        }
        if let Some(m) = config_warning_modal {
            modal_stack.push(Box::new(m));
        }

        // Warm both font caches the figure pipeline loads on first call (mermaid's own fontdb
        // and ours for `usvg`) off the critical path: each scans OS font dirs for 100-300 ms, and
        // without this a document with N figures spawns N concurrent scans — the dominant source
        // of initial-load lag.  Skipped when no figure can ever decode, where it is wasted IO.
        if media_capable && !matches!(config.figures.enabled, crate::config::FiguresEnabled::Never)
        {
            std::thread::spawn(crate::diagram::warm_fontdb);
        }

        // Decided here, before any modal can have been dismissed, but acted on later: `app_tx`
        // doesn't exist until `run()` spawns the event threads.
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
            open_doc: None,
            parked_vim: None,
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
            diagram_render_hold_until: None,
            diagram_render_watch_version: None,
            section_jump_pending_since: None,
            section_jump_target_scroll: None,
            diff_advance_pending_since: None,
            search_advance_pending_since: None,
            watcher: None,
            clipboard: crate::clipboard::default_source(),
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

    /// Record the `#section` the command line named, applied on the first frame.  A builder
    /// rather than a `new` parameter: only `main` sets it, and every other site wants the default.
    #[must_use]
    pub fn with_startup_anchor(mut self, anchor: Option<String>) -> Self {
        self.startup_anchor = anchor;
        self
    }

    /// Enable or disable vim modal editing mid-session, keeping `config.modal.handler` and the
    /// editor mode in sync.  Mirrors the startup wiring in `App::new`.
    pub(crate) fn set_vim_enabled(&mut self, enabled: bool) {
        if enabled {
            self.config.modal.handler = VIM_HANDLER.into();
            if self.vim.is_none() {
                self.vim = Some(VimState::default());
            }
            // Leave Preview behind exactly as startup does.
            leave_preview_under_vim(&self.config, &mut self.editor);
        } else {
            self.config.modal.handler = DEFAULT_HANDLER.into();
            self.vim = None;
            // A session the user turned off must not reappear when they leave the manual.
            self.parked_vim = None;
        }
        // Enabling while a read-only document is open parks the fresh session straight away, so
        // the toggle lands on the next editable buffer rather than being lost.
        self.sync_vim_suspension();
    }

    /// Park or restore the vim session to match the live document's read-only-ness.  The single
    /// writer of [`App::parked_vim`], called by every document-swapping path; idempotent, so a new
    /// one need only remember to call it.
    pub(super) fn sync_vim_suspension(&mut self) {
        if self.editor.readonly {
            if let Some(vim) = self.vim.take() {
                self.parked_vim = Some(vim);
            }
        } else if self.vim.is_none() {
            self.vim = self.parked_vim.take();
        }
    }

    /// Drain any further `ImageReady` events already in `rx`, so a burst of decode completions is
    /// handled as one unit followed by a single `refresh_parsed`.
    fn drain_pending_image_ready(&mut self, rx: &mpsc::Receiver<AppEvent>) {
        loop {
            match rx.try_recv() {
                // A `try_recv`'d Term event can't be put back, so it goes on `pending_events`,
                // which the next iteration consults first.  Draining continues so image-ready
                // events queued behind the first key aren't starved.
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

    /// Run the event loop until the user quits.  The body is deliberately a flat sequence of
    /// named calls into [`event_loop`], so the control flow is legible at a glance.
    pub fn run(&mut self, mut terminal: Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        self.startup_pointer_hint();
        // Here rather than in `App::new` because it writes to disk and the constructor must not.
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
                    // Reset the phase so the cursor reappears solid for a full interval,
                    // whatever phase it was in when focus was lost.
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
        if let Some(id) = self.open_doc {
            return format!("Docs: {}", id.title());
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
        // Vim never rests in Preview.
        assert_eq!(app.editor.mode, Mode::Rendered);
    }

    #[test]
    fn a_document_opened_under_vim_never_lands_in_preview() {
        // Regression: every document gets a fresh `EditorState`, born in Preview, so a link
        // follow used to drop a vim session there.
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

    /// Preview and vim-Normal are alternative resting states, so a read-only document suspends
    /// vim rather than fighting it.
    #[test]
    fn a_read_only_document_parks_the_vim_session_and_gives_it_back() {
        let mut app = app_with_handler("vim");
        app.vim.as_mut().expect("vim active").pending_g = true;

        app.open_doc_page(crate::docs::DocId::Keybindings, None, 20, 80);
        assert!(app.vim.is_none(), "vim is suspended while reading");
        assert_eq!(
            app.editor.mode,
            crate::editor::Mode::Preview,
            "a read-only document rests in Preview even under vim"
        );

        // The session comes back with the next editable buffer, carrying its parked state.
        let mut f = tempfile::Builder::new()
            .suffix(".md")
            .tempfile()
            .expect("temp file");
        {
            use std::io::Write;
            f.write_all(b"alpha\n").expect("write");
            f.flush().expect("flush");
        }
        app.load_file_into_editor(f.path().to_path_buf())
            .expect("load");
        assert!(
            app.vim.as_ref().is_some_and(|v| v.pending_g),
            "the parked session is restored, not rebuilt"
        );
        assert_ne!(
            app.editor.mode,
            crate::editor::Mode::Preview,
            "an editable document under vim leaves Preview again"
        );
    }

    /// Turning vim off while reading must not leave a session waiting to reappear later.
    #[test]
    fn disabling_vim_while_reading_clears_the_parked_session() {
        let mut app = app_with_handler("vim");
        app.open_doc_page(crate::docs::DocId::Editing, None, 20, 80);
        assert!(app.parked_vim.is_some());

        app.set_vim_enabled(false);
        assert!(app.parked_vim.is_none(), "parked session cleared");
    }

    /// Enabling vim while reading parks the fresh session instead of dropping a cursor into a
    /// document that draws none.
    #[test]
    fn enabling_vim_while_reading_parks_it_instead_of_leaving_preview() {
        let mut app = app_with_handler("default");
        app.open_doc_page(crate::docs::DocId::Editing, None, 20, 80);

        app.set_vim_enabled(true);
        assert!(app.vim.is_none(), "not active while reading");
        assert!(app.parked_vim.is_some(), "parked for the next document");
        assert_eq!(app.editor.mode, crate::editor::Mode::Preview);
    }

    #[test]
    fn set_vim_enabled_mirrors_startup_wiring() {
        // Enabling mid-session must reach the exact state startup produces.
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
        // The handler string must revert too, so a later `Config::save` writes it.
        let mut app = app_with_handler("vim");
        assert!(app.vim.is_some());

        app.set_vim_enabled(false);
        assert!(app.vim.is_none(), "vim state cleared");
        assert_eq!(app.config.modal.handler, "default");
    }

    #[test]
    fn set_vim_enabled_true_is_idempotent() {
        // A redundant enable must preserve the live vim state, not swap in a fresh default.
        let mut app = app_with_handler("vim");
        app.vim.as_mut().expect("vim active").pending_g = true;
        app.editor.mode = Mode::Raw;

        app.set_vim_enabled(true);
        assert!(
            app.vim.as_ref().expect("vim still active").pending_g,
            "existing vim state is preserved, not reset"
        );
        assert_eq!(app.editor.mode, Mode::Raw); // only Preview is rewritten
    }

    // ── VisualLine clipboard widening ─────────────────────────────────

    use crate::config::Action;
    use crate::document::{Buffer, Selection};
    use crate::input::VimSubMode;

    /// A VisualLine selection over `text`, with deliberately ragged mid-line endpoints.
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
        // A charwise span from mid-line-0 to mid-line-1 must copy both whole lines.
        let mut app = app_in_visual_line("alpha\nbeta\ngamma", 2, 7);
        app.dispatch_action(Action::Copy, 40, 80);
        assert_eq!(app.editor.kill_ring, "alpha\nbeta\n");
        // The stored selection is restored to the charwise span, never snapped.
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
        // Copy first so the paste source is deterministic whichever way `clipboard_text` resolves.
        let mut app = app_in_visual_line("alpha\nbeta\ngamma", 2, 2);
        app.dispatch_action(Action::Copy, 40, 80);
        assert_eq!(app.editor.kill_ring, "alpha\n", "test premise");
        // Re-anchor on line 1, again mid-line, so the charwise span is not the line.
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
        // Charwise widening is vim's inclusive one — the span plus the char under the cursor —
        // and leaves the stored half-open selection alone.
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

    // ── Ex commands, end-to-end through `dispatch_single_key` ─────────

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

    /// Type a full `:`-command into `app` through the real key-dispatch entry point.
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
        // Drop any startup modal so the assertion sees only the `:q`'s own quit-confirm.
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
