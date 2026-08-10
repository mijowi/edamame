//! Run-loop decomposition extracted from `app.rs` in Step 3 of
//! `refactor-app.md`.
//!
//! The previous `App::run` body inlined ~540 lines of setup, frame
//! preparation, drawing, event dispatch, and terminal-resize handling.
//! Each concern now lives in its own method on `App`, all defined here.
//! `run` itself stays in `app.rs` and reads as a flat sequence of named
//! steps.
//!
//! Owns:
//! - Setup: [`App::startup_pointer_hint`], [`App::spawn_event_threads`],
//!   [`App::build_keymap_if_needed`].
//! - Per-iter prep: [`App::tick_timers`], [`App::coalesce_image_updates`],
//!   [`App::compute_doc_dims`], [`App::prepare_viewport`],
//!   [`App::should_draw`], [`App::draw_frame`].
//! - Event acquisition: [`App::next_event`].
//! - Event dispatch: [`App::on_resize`], [`App::dispatch_modal_event`],
//!   [`App::dispatch_mouse_event`], [`App::dispatch_paste`],
//!   [`App::dispatch_key_event`].
//! - The [`drop_indicator_for`] free helper consumed by `draw_frame`.

use std::io::Stdout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyEventKind, MouseEvent, MouseEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Rect, Size};
use ratatui::Terminal;

use crate::config::{Action, CoalesceKind, Config, KeyBindingOverrides, KeyMap};
use crate::editor::{edit_ops, mouse_ops, Mode};
use crate::input::mode_handler::default::DefaultHandler;
use crate::input::{vim_feed, VimOutcome, VimSubMode};
use crate::terminal::PointerShape;
use crate::ui::editor_view::layout_doc_with_scrollbar;
use crate::ui::{position_for_click, position_for_drag, thumb_range, EditorView, ModalKind};
use crate::watcher::{NotifyWatcher, WatchedEvent};

use super::actions::{modal_wheel_delta, HandleEvent};
use super::flash::MessageKind;
use super::frame_timer::{MIN_FRAME_INTERVAL, RESIZE_QUIESCE};
use super::modal::ModalRenderCtx;
use super::{App, AppEvent};

/// Per-frame document-area dimensions derived from the live terminal
/// size and the configured status-bar layout.  Computed once per loop
/// iteration so every dispatch arm sees the same numbers.
pub(super) struct DocDims {
    /// Document-area height in rows (`term_size.height - bottom_rows`).
    pub doc_height: usize,
    /// Document-area width in columns.  Equals the terminal width minus
    /// any horizontal clamp imposed by `editor.max_width_enabled`.
    pub doc_width: usize,
    /// Document-area rectangle used for mouse hit-testing.  When the
    /// max-width clamp is active, `doc_area.x` is non-zero and reflects
    /// the centred offset.
    pub doc_area: Rect,
}

impl App {
    // ── Setup ─────────────────────────────────────────────────────────────────

    /// Hint the terminal to show an I-beam pointer over the TUI area by
    /// default.  Terminals that don't implement OSC 22 silently ignore
    /// this.  No-op when mouse capabilities aren't available.
    pub(super) fn startup_pointer_hint(&mut self) {
        if self.capabilities.mouse {
            self.update_pointer_shape(PointerShape::Text);
        }
    }

    /// Build the main-loop mpsc channel and spawn the two background
    /// threads that feed it: a terminal-event reader and an image-encode
    /// worker.  Returns the receiver so the run loop can drive event
    /// acquisition; senders for background workers come from
    /// `self.app_tx`, which this method populates.
    ///
    /// The reader thread is `poll`-based (instead of a bare `read`) so a
    /// pause flag can take effect without having to interrupt a blocked
    /// syscall.  When the App shells out to an external editor, it
    /// flips the flag so the child process gets uncontested access to
    /// stdin — without this, both processes would race to read terminal
    /// bytes and the editor would see a corrupted input stream.
    ///
    /// The encoder thread funnels every resize-encode for every visible
    /// image through one CPU.  Encoding is CPU-bound, so serial
    /// execution preserves cache locality and avoids contention on the
    /// terminal's graphics state.  The UI thread NEVER encodes — it
    /// only enqueues `ResizeRequest`s and paints the pre-encoded bytes
    /// once the worker responds.
    pub(super) fn spawn_event_threads(&mut self) -> mpsc::Receiver<AppEvent> {
        let (tx, rx) = mpsc::channel::<AppEvent>();
        self.app_tx = Some(tx.clone());

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

        // Filesystem watcher.  Spawn even when no file is open so
        // the wiring is uniform — `App::start_file_watcher` calls
        // `watch()` on the active path once we know what it is.
        // Failures are non-fatal: we log and proceed without a
        // watcher (the user just won't see external-edit prompts).
        //
        // The bridge thread is only spawned on the Ok arm.  If the
        // watcher fails to construct, `watch_tx` is dropped, the
        // bridge would exit on its first `recv` anyway — skipping it
        // avoids the wasted thread and keeps the failure mode tidy.
        let (watch_tx, watch_rx) = mpsc::channel::<WatchedEvent>();
        match NotifyWatcher::new(watch_tx) {
            Ok(w) => {
                self.watcher = Some(Box::new(w));
                // Bridge thread: forwards `WatchedEvent` from the
                // watcher worker onto the main mpsc as
                // `AppEvent::Watcher`.  Keeping the watcher's public
                // API generic in `mpsc::Sender<WatchedEvent>` (rather
                // than baking `AppEvent` in) keeps the watcher
                // unit-testable without an `App`.
                let bridge_tx = tx.clone();
                if let Err(e) = std::thread::Builder::new()
                    .name("edamame-watcher-bridge".to_owned())
                    .spawn(move || {
                        while let Ok(ev) = watch_rx.recv() {
                            if bridge_tx.send(AppEvent::Watcher(ev)).is_err() {
                                break;
                            }
                        }
                    })
                {
                    tracing::warn!(
                        target: "watcher",
                        error = %e,
                        "failed to spawn watcher bridge thread",
                    );
                }
            }
            Err(e) => {
                tracing::warn!(target: "watcher", error = %e, "failed to construct watcher");
            }
        }

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

        rx
    }

    /// Begin watching the currently-open file, if any.  Called once
    /// from the run loop after [`Self::spawn_event_threads`] has
    /// constructed the watcher.  Watcher failures are non-fatal —
    /// the user simply won't see external-edit prompts.
    ///
    /// Note: there is a tiny race window between `inner.watch(parent)`
    /// inside `FileWatcher::watch` and the worker thread receiving
    /// `WorkerCommand::SetPath`.  An organic notify event that races
    /// this gap sees `current_path == None` in the worker and is
    /// dropped.  We deliberately do *not* force a reconcile here:
    /// the just-loaded buffer contents are by definition the on-disk
    /// state, and any change that arrived during startup would have
    /// been part of that load.  The external-editor flow forces a
    /// reconcile on resume because there the loaded buffer and
    /// post-editor disk state can genuinely differ.
    pub(super) fn start_file_watcher(&mut self) {
        let Some(path) = self.file_path.clone() else {
            return;
        };
        let Some(watcher) = self.watcher.as_mut() else {
            return;
        };
        if let Err(e) = watcher.watch(&path) {
            tracing::warn!(target: "watcher", path = %path.display(), error = %e, "watch failed");
        }
    }

    /// Build the live keymap once and stash it on `self`.  Held for the
    /// life of the process so the keybinds overlay can mutate it in
    /// place — rebinds take effect on the next keystroke without a
    /// rebuild.  No-op if the keymap is already built.
    pub(super) fn build_keymap_if_needed(&mut self) -> Result<()> {
        if self.keymap.is_none() {
            self.keymap = Some(KeyMap::build(&self.keybindings)?);
        }
        Ok(())
    }

    // ── Per-iter prep ─────────────────────────────────────────────────────────

    /// Apply the per-iteration time-driven state changes that happen
    /// before any event is read: clear an elapsed resize-quiesce
    /// deadline, expire transient hint-line messages, advance the
    /// cursor blink, and refresh the editor's `modal_open` flag.  Sets
    /// `needs_draw` whenever any of these caused visible state to
    /// change.
    pub(super) fn tick_timers(&mut self) {
        if self.resize_quiesce_at.is_some_and(|t| t <= Instant::now()) {
            self.resize_quiesce_at = None;
            self.needs_draw = true;
        }
        if self.expire_transient_if_due() {
            self.needs_draw = true;
        }
        if self.editor.cursor_blink.tick() {
            self.needs_draw = true;
        }
        if self.editor.expire_yank_flash() {
            self.needs_draw = true;
        }
        self.tick_autosave();
        self.tick_section_jump();
        self.tick_diff_advance();
        self.tick_search_advance();
        self.editor.modal_open = self.any_modal_open();
    }

    /// Coalesce any `ImageReady`-driven cache mutations into a single
    /// parse-and-render pass for this frame.  Without this, a burst of
    /// N simultaneous decode completions would trigger N reparses on
    /// the main thread and stall pending scroll / key events between
    /// each one.
    pub(super) fn coalesce_image_updates(&mut self) {
        if self.images_dirty {
            self.editor.refresh_parsed();
            self.images_dirty = false;
            self.needs_draw = true;
        }
    }

    /// Translate the live `term_size` into the document-area dimensions
    /// the rest of the iteration uses.  Pure: no side effects.
    pub(super) fn compute_doc_dims(&self, term_size: Size) -> DocDims {
        let bottom_rows = crate::ui::BottomRegion::height();
        let doc_height = (term_size.height as usize).saturating_sub(bottom_rows as usize);
        let full_doc_area = Rect {
            x: 0,
            y: 0,
            width: term_size.width,
            height: term_size.height.saturating_sub(bottom_rows),
        };
        // Mirror `EditorView::render`'s line-number gutter + scrollbar
        // + max-width layout so `viewport_width`, mouse hit-testing,
        // and per-line wrap all agree with the painted content area.
        let line_count = if self.config.editor.show_line_numbers {
            match self.editor.mode {
                Mode::Preview | Mode::Rendered => self.editor.parsed.line_count(),
                Mode::Raw => self.editor.buffer.line_count(),
                // Diff mode doesn't paint a line-number gutter; keep
                // the reservation at zero so `viewport_width` matches
                // what `DiffView` actually paints into.
                Mode::Diff => 0,
            }
        } else {
            0
        };
        let (_gutter, full_after_gutter) = crate::ui::split_gutter(full_doc_area, line_count);
        let (doc_area, _bar) = layout_doc_with_scrollbar(
            full_after_gutter,
            self.config.editor.max_width_enabled,
            self.config.editor.max_width_cols,
            |w| self.editor.total_visual_rows_for_mode(w as usize),
        );
        let doc_width = doc_area.width as usize;
        DocDims {
            doc_height,
            doc_width,
            doc_area,
        }
    }

    /// Refresh per-frame state that depends on the live document width:
    /// record `last_area_width`, propagate the live width into the
    /// editor (so the table-column min-max algorithm adapts to the
    /// user's terminal), and kick off decodes for images within the
    /// near-viewport window.  Re-running every frame is cheap —
    /// `ImageCache::request` short-circuits for URLs already in any
    /// non-Idle state, so we only spawn threads for URLs that just
    /// entered the window.
    pub(super) fn prepare_viewport(&mut self, dims: &DocDims) {
        self.last_area_width = dims.doc_area.width;
        self.last_doc_height = dims.doc_height;
        self.last_doc_width = dims.doc_width;
        self.editor.set_viewport_width(dims.doc_width);
        // A non-capturing navigate flow lets the buffer be edited freely, so
        // the match list can go stale outside the in-flow mutation paths.
        // Refresh here (version-guarded → a no-op when nothing changed) so the
        // focus-scroll below and the overlay painter both see live ranges.
        // Paused while a `:s` preview is active — the previewed text is
        // transient, and recomputing matches against it would re-anchor a
        // coexisting hlsearch session to text that is about to revert (the
        // overlay painters suspend the search wash for the same reason).
        if self.editor.substitute_preview.is_none() {
            self.editor.ensure_search_fresh();
        }
        // Resolve a diff-entry scroll request now that the viewport
        // height is known (it isn't at the modal-close site that enters
        // diff mode).  One-shot: cleared after it fires.
        if self.editor.pending_focus_scroll {
            // The flag is shared by diff entry and the search flow;
            // each scroll helper no-ops when its session isn't active.
            self.editor
                .scroll_focused_hunk_into_view(dims.doc_height, dims.doc_width);
            self.editor
                .scroll_focused_match_into_view(dims.doc_height, dims.doc_width);
            self.editor.pending_focus_scroll = false;
            self.needs_draw = true;
        }
        self.dispatch_visible_image_decodes(self.editor.scroll, dims.doc_height);
    }

    /// True when the run loop should call `terminal.draw` on this
    /// iteration: state has changed since the last draw AND the 16 ms
    /// frame-rate throttle is satisfied AND no resize burst is in
    /// flight.  `since_draw` is the elapsed time since the last
    /// `terminal.draw` (`None` before the first draw) so the caller can
    /// reuse the same value when computing the blocking deadline.
    pub(super) fn should_draw(&self, since_draw: Option<Duration>) -> bool {
        let throttle_ok = since_draw.is_none_or(|d| d >= MIN_FRAME_INTERVAL);
        let resize_pending = self.resize_quiesce_at.is_some();
        self.needs_draw && throttle_ok && !resize_pending
    }

    /// Render one frame: the editor view plus the topmost modal (if
    /// any).  Runs inside `terminal.draw`'s closure so `Frame` is
    /// available; updates `last_draw_at` and clears `needs_draw`
    /// afterwards.
    pub(super) fn draw_frame(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> Result<()> {
        let filename = self.display_filename();
        let is_scrolling = self.is_scrolling();
        let show_handles = self.config.table.show_buttons;
        let max_width_enabled = self.config.editor.max_width_enabled;
        let max_width_cols = self.config.editor.max_width_cols;
        let hint = self.hint_content();
        let vim_mode_label = self.vim.as_ref().map(|v| v.mode_label());
        let visual_kind = self.vim.as_ref().and_then(|v| v.visual_kind());
        let editor_cursor_style = super::cursor_style::editor_cursor_style(
            self.theme,
            self.editor.mode,
            self.vim.as_ref().map(|v| v.sub_mode),
        );
        // Hide the modal's block cursor when the terminal window has lost
        // focus, mirroring the editor cursor (see `EditorState::cursor_visible`).
        let modal_cursor_visible =
            self.editor.terminal_focused && self.editor.cursor_blink.is_visible();
        let theme_ref = self.theme;
        let drop_indicator = drop_indicator_for(&self.drag_target);
        let scrollbar_active = self.scrollbar_hover
            || matches!(
                self.drag_target,
                Some(mouse_ops::DragTarget::Scrollbar { .. })
            );
        let show_line_numbers = self.config.editor.show_line_numbers;
        let capabilities_ref = &self.capabilities;
        let config_ref: &Config = &self.config;
        // One tick per real draw (not per paint pass — Raw and Diff
        // modes draw without painting images).  `image_view::paint_native`
        // reuses a native transmission only when it was made on the
        // immediately preceding frame.
        self.editor.images.begin_frame();
        let editor_ref = &mut self.editor;
        let view_state_ref = &mut self.view_state;
        let modal_stack_top = self.modal_stack.top_mut();
        terminal.draw(|frame| {
            let view = EditorView {
                state: editor_ref,
                theme: theme_ref,
                filename: &filename,
                show_table_buttons: show_handles,
                table_drop_indicator: drop_indicator,
                show_line_numbers,
                capabilities: capabilities_ref,
                is_scrolling,
                hint,
                vim_mode_label,
                visual_kind,
                editor_cursor_style,
                max_width_enabled,
                max_width_cols,
                scrollbar_active,
            };
            frame.render_stateful_widget(view, frame.area(), view_state_ref);
            if let Some(top) = modal_stack_top {
                // Dim the editor (status + hint included) before
                // painting the modal.  The modal's own `Clear` + bg
                // fill overwrites its rect cleanly, so this sweep can
                // cover the whole terminal area without computing a
                // complement.  Strategy depends on terminal color
                // depth — see `crate::ui::dim`.
                let area = frame.area();
                crate::ui::dim::dim_area(frame.buffer_mut(), area, capabilities_ref, theme_ref);
                let render_ctx = ModalRenderCtx {
                    theme: theme_ref,
                    config: config_ref,
                    cursor_visible: modal_cursor_visible,
                };
                top.render(frame, frame.area(), &render_ctx);
            }
        })?;
        self.last_draw_at = Some(Instant::now());
        self.needs_draw = false;
        Ok(())
    }

    // ── Event acquisition ─────────────────────────────────────────────────────

    /// Apply a non-[`AppEvent::Term`] event: image decode/encode
    /// completion, link-open result, or watcher notification.
    /// Centralised here so the three event-receiving sites
    /// ([`Self::next_event`], [`Self::collect_key_burst`], and
    /// `App::drain_pending_image_ready`) stay in lockstep when a new
    /// variant is added — previously the same `match` arms were
    /// inlined at all three sites and quietly drifted.
    ///
    /// Sets `needs_draw = true` for most variants; the exceptions are
    /// `ProtocolReady(Err)` (FIFO rebalance only, no visual change)
    /// and `LinkOpenResult(Ok)` (success is silent).  The caller is
    /// responsible for draining follow-on queued image events from
    /// `rx` if it wants to coalesce them into a single refresh.
    ///
    /// `Term` events are caller-specific (passthrough vs. push into
    /// `pending_events` vs. filter for key-press batching) so they
    /// stay at each call site rather than being routed through here.
    pub(super) fn handle_async_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Term(_) => {
                // Filtered out by the caller; reaching here would be a
                // programmer error.  Logged rather than panicked to
                // keep the run loop alive in release builds.
                debug_assert!(false, "handle_async_event called with Term");
                tracing::warn!(
                    target: "app",
                    "handle_async_event called with Term — should be filtered by caller",
                );
            }
            AppEvent::ImageReady(Ok(loaded)) => {
                // The worker captured the image / remote settings at spawn
                // time; recheck against the *current* settings before
                // accepting the result — a slow remote fetch must not
                // resurface after the user tightened the remote policy.
                // Forgetting the entry (rather than leaving it `Pending`)
                // lets the per-frame dispatch re-resolve the URL if the
                // settings permit it again.
                if !self.image_result_still_wanted(&loaded.url) {
                    tracing::debug!(
                        target: "image", url = %loaded.url,
                        "decode result discarded — settings changed mid-flight",
                    );
                    self.editor.images.forget(&loaded.url);
                    return;
                }
                self.editor.images.set_decoded_with_prebuilt(
                    &loaded.url,
                    loaded.image,
                    loaded.scratch,
                );
                self.images_dirty = true;
                self.needs_draw = true;
            }
            AppEvent::ImageReady(Err((url, message))) => {
                tracing::debug!(target: "image", %url, %message, "image decode failed");
                // Only a still-`Pending` entry may take the failure —
                // same condition as the `Ok` arm.  An evicted entry must
                // not be resurrected (memoising the failure would pin the
                // URL against the settings it was evicted under), and a
                // `Ready` entry means a newer worker already delivered a
                // good decode for this URL (an evict + re-request while
                // this worker ran) — a stale failure must not overwrite
                // it, since `request` never retries a `Failed` entry.
                if !matches!(
                    self.editor.images.status(&url),
                    Some(crate::image::DecodeStatus::Pending)
                ) {
                    return;
                }
                self.editor.images.set_failed(&url, message);
                // A failure collapses the block's reserved rows to 1
                // (see `ImageCache::reserved_rows`), so the parsed
                // doc must be rebuilt to drop the blank rows under
                // the placeholder.
                self.images_dirty = true;
                self.needs_draw = true;
            }
            AppEvent::ProtocolReady(Ok(resp)) => {
                self.editor.images.apply_resize_response(resp);
                self.needs_draw = true;
            }
            AppEvent::ProtocolReady(Err(err)) => {
                tracing::debug!(target: "image", %err, "encoder request failed");
                // Keep the pending FIFO balanced — see ImageCache.
                self.editor.images.drop_pending_front();
            }
            AppEvent::LinkOpenResult(result) => {
                if let Err(msg) = result {
                    tracing::warn!(target: "link", error = %msg, "link open failed");
                    self.notify(format!("Link open failed: {msg}"), ModalKind::Error);
                    self.needs_draw = true;
                }
            }
            AppEvent::Watcher(event) => {
                self.handle_watcher_event(event);
                self.needs_draw = true;
            }
            AppEvent::ExportDone(id, outcome) => {
                self.handle_export_done(id, outcome);
            }
            AppEvent::ReleaseCheckResult(result) => {
                self.release_check_in_flight = false;
                let status = match result {
                    Ok(tag) => super::update_check::ReleaseStatus::Available(tag),
                    Err(msg) => {
                        tracing::debug!(target: "about", %msg, "release check failed");
                        super::update_check::ReleaseStatus::Failed
                    }
                };
                self.latest_release = Some(status.clone());
                if let Some(about) = self
                    .modal_stack
                    .find_first_mut::<crate::app::modal::AboutModal>()
                {
                    about.set_release(status);
                }
                self.needs_draw = true;
            }
        }
    }

    /// Pull the next event the run loop should process.
    ///
    /// Returns:
    /// - `Some(event)` — a real terminal event the dispatch arms should
    ///   handle.
    /// - `None` — a background event (image ready, encoder response,
    ///   link-open result) was processed internally, or a pending
    ///   deadline elapsed without an external event; the run loop
    ///   should `continue`.  May also signal channel disconnect, in
    ///   which case `should_quit` is set so the loop's bottom check
    ///   breaks.
    ///
    /// `since_draw` is the elapsed time since the last successful
    /// `terminal.draw`.  Used to wake at the remaining frame-throttle
    /// budget when `needs_draw` was set but the throttle blocked the
    /// draw.  With no pending deadline and nothing to draw, blocks on
    /// `rx.recv()` so the app idles with 0 % CPU.
    pub(super) fn next_event(
        &mut self,
        rx: &mpsc::Receiver<AppEvent>,
        since_draw: Option<Duration>,
    ) -> Option<Event> {
        // If a previous drain stashed Term events, replay them in
        // order before consulting the channel.
        if let Some(e) = self.pending_events.pop_front() {
            return Some(e);
        }

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

        let recv_result = match wait {
            Some(d) => rx.recv_timeout(d),
            None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match recv_result {
            Ok(AppEvent::Term(e)) => Some(e),
            Ok(ev) => {
                self.handle_async_event(ev);
                // Coalesce any queued image/protocol events into a
                // single refresh.  Drains any subsequent non-Term
                // events too; harmless for `LinkOpenResult` /
                // `Watcher` (which would have been handled the same
                // way on the next loop iteration anyway).
                self.drain_pending_image_ready(rx);
                None
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // A pending deadline (reveal / scroll quiesce / throttle)
                // elapsed without an external event.  Redraw once to
                // apply it; the loop will then go back to blocking on
                // `recv()` because the deadline is no longer in the
                // future.
                self.needs_draw = true;
                None
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.should_quit = true;
                None
            }
        }
    }

    // ── Event dispatch ────────────────────────────────────────────────────────

    /// Handle a `Resize` event: arm a quiesce deadline so the burst
    /// doesn't pin CPU painting partial frames, invalidate width-
    /// dependent snapshot caches so the settled-size redraw rebuilds
    /// them at the new dimensions, and clear `last_scroll_at` so newly-
    /// visible images render at their native protocol immediately.
    pub(super) fn on_resize(&mut self) {
        self.resize_quiesce_at = Some(Instant::now() + RESIZE_QUIESCE);
        self.view_state.rendered.image_snapshots_key = None;
        self.view_state.rendered.link_snapshots_key = None;
        self.view_state.rendered.table_snapshots_key = None;
        self.view_state.preview.image_snapshots_key = None;
        self.view_state.preview.link_snapshots_key = None;
        self.last_scroll_at = None;
        // A resize repaints the whole screen, so no native image
        // transmission survives it — even one whose rect is unchanged.
        self.editor.images.invalidate_native_paints();
    }

    /// Route an event to the topmost modal.  Key presses dispatch
    /// through `Modal::handle_key` (which in turn applies any
    /// `ModalOutcome` such as a follow-up Action or close-with-flash);
    /// wheel events translate to `ModalState::scroll_by`.  Non-key /
    /// non-mouse events are absorbed silently to prevent the editor
    /// behind the modal from reacting.
    ///
    /// Drains any pending external-editor flow at the end so the
    /// `&mut Terminal` / `&mpsc::Receiver` borrows don't have to leak
    /// across into action.rs.
    pub(super) fn dispatch_modal_event(
        &mut self,
        event: &Event,
        dims: &DocDims,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) {
        // Reset the cursor blink on any keypress while a modal is open
        // so the `▏` cursor snaps to visible after typing.
        if matches!(event, Event::Key(k) if k.kind == KeyEventKind::Press) {
            self.editor.cursor_blink.reset();
        }

        let wheel_step = self.config.editor.mouse_scroll_lines;
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                self.dispatch_modal_key(*key, dims.doc_height, dims.doc_width);
                self.needs_draw = true;
            }
            Event::Mouse(me) => {
                use crossterm::event::MouseButton;
                match me.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.dispatch_modal_click(me.column, me.row);
                        self.needs_draw = true;
                    }
                    _ => {
                        if let Some(top) = self.modal_stack.top_mut() {
                            top.handle_wheel(modal_wheel_delta(me, wheel_step));
                            self.needs_draw = true;
                        }
                    }
                }
            }
            Event::Paste(text) => {
                self.dispatch_modal_paste(text);
                self.needs_draw = true;
            }
            _ => {}
        }

        // External-editor flows defer to the run loop because the
        // editor invocation needs `&mut Terminal` and `&rx`, which
        // only this scope holds.
        if self.pending_open_config_in_editor {
            self.pending_open_config_in_editor = false;
            self.open_config_in_editor(terminal, rx);
        }
        if self.pending_open_file_in_editor {
            self.pending_open_file_in_editor = false;
            self.open_current_file_in_editor(terminal, rx);
        }
        if let Some(path) = self.pending_open_theme_in_editor.take() {
            self.open_theme_in_editor(&path, terminal, rx);
        }
    }

    /// Pre-empt scrollbar interactions before normal mouse dispatch.
    /// Returns `true` when the event was fully consumed by the
    /// scrollbar (drag in flight, click in gutter, or hover update on
    /// the gutter); the caller should not pass it to `MouseDispatcher`.
    /// Returns `false` for events outside the gutter when no drag is
    /// in flight — normal dispatch continues.
    fn handle_scrollbar_event(&mut self, mouse_event: &MouseEvent, dims: &DocDims) -> bool {
        let dragging = matches!(
            self.drag_target,
            Some(mouse_ops::DragTarget::Scrollbar { .. })
        );
        let metrics = match self.view_state.scrollbar {
            Some(m) => m,
            None => {
                // Scrollbar disappeared (content shrank, mode change,
                // resize) — clear lingering hover/drag state and let
                // the event dispatch normally.
                if dragging {
                    self.drag_target = None;
                    self.editor.drag_in_progress = false;
                }
                if self.scrollbar_hover {
                    self.scrollbar_hover = false;
                    self.needs_draw = true;
                }
                return false;
            }
        };
        let in_gutter = mouse_event.column >= metrics.area.x
            && mouse_event.column < metrics.area.x + metrics.area.width
            && mouse_event.row >= metrics.area.y
            && mouse_event.row < metrics.area.y + metrics.area.height;

        match mouse_event.kind {
            MouseEventKind::Moved => {
                let new_hover = in_gutter;
                if self.scrollbar_hover != new_hover {
                    self.scrollbar_hover = new_hover;
                    self.needs_draw = true;
                }
                if in_gutter {
                    self.update_pointer_shape(PointerShape::Default);
                    return true;
                }
                return false;
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Left) if in_gutter => {
                let track = metrics.area.height;
                let click_row = mouse_event.row.saturating_sub(metrics.area.y);
                let (thumb_top, thumb_h) =
                    match thumb_range(metrics.total, metrics.visible, metrics.position, track) {
                        Some(t) => t,
                        None => return true, // body fits — gutter shouldn't even be rendered
                    };
                let on_thumb =
                    click_row >= thumb_top && click_row < thumb_top.saturating_add(thumb_h);
                let (new_position, grab_offset) = if on_thumb {
                    (metrics.position, click_row - thumb_top)
                } else {
                    let pos = position_for_click(metrics.total, metrics.visible, track, click_row);
                    (pos, thumb_h / 2)
                };
                let scroll_before = self.editor.scroll;
                mouse_ops::set_scroll_absolute(
                    &mut self.editor,
                    new_position as usize,
                    dims.doc_width,
                    dims.doc_height,
                );
                if self.editor.scroll != scroll_before {
                    self.mark_scrolling();
                }
                self.drag_target = Some(mouse_ops::DragTarget::Scrollbar { grab_offset });
                self.editor.drag_in_progress = true;
                return true;
            }
            MouseEventKind::Drag(crossterm::event::MouseButton::Left) if dragging => {
                let grab_offset = match self.drag_target {
                    Some(mouse_ops::DragTarget::Scrollbar { grab_offset }) => grab_offset,
                    _ => return false,
                };
                let track = metrics.area.height;
                // Pointer rows above the gutter top map to the start of
                // the track; rows below clamp to track-1.  Saturating
                // arithmetic on u16 handles both extremes.
                let pointer_row = mouse_event
                    .row
                    .saturating_sub(metrics.area.y)
                    .min(track.saturating_sub(1));
                let new_position = position_for_drag(
                    metrics.total,
                    metrics.visible,
                    track,
                    pointer_row,
                    grab_offset,
                );
                let scroll_before = self.editor.scroll;
                mouse_ops::set_scroll_absolute(
                    &mut self.editor,
                    new_position as usize,
                    dims.doc_width,
                    dims.doc_height,
                );
                if self.editor.scroll != scroll_before {
                    self.mark_scrolling();
                }
                return true;
            }
            MouseEventKind::Up(crossterm::event::MouseButton::Left) if dragging => {
                self.drag_target = None;
                self.editor.drag_in_progress = false;
                return true;
            }
            _ => {}
        }

        // Wheel ticks landing in the gutter scroll the document the
        // same way they would over the body — fall through.
        if dragging {
            // Any other event arriving while a scrollbar drag is in
            // flight is irrelevant to the drag and shouldn't drive
            // text selection underneath.  Absorb silently.
            return true;
        }
        false
    }

    /// Handle a mouse event when no modal is open: refresh the
    /// pointer-shape feedback, hover-link tracking, and (for non-Moved
    /// events) hand the event to `MouseDispatcher` and `mouse_ops::apply`
    /// so it can mutate the editor.
    pub(super) fn dispatch_mouse_event(&mut self, mouse_event: MouseEvent, dims: &DocDims) {
        // Mouse clicks hit-test against `parsed.source_map` byte ranges
        // — a stale map from a deferred re-parse would map the click to
        // the wrong block or line.  Flush synchronously here; a click
        // ends the typing burst naturally, so the latency cost is
        // invisible.
        if self.editor.flush_parsed_if_dirty() {
            self.needs_draw = true;
        }
        if !self.capabilities.mouse {
            return;
        }

        // Scrollbar interception runs before every other dispatch:
        // hover updates the active-thumb tint; mouse-down on the
        // gutter starts a drag (and jumps the thumb under the
        // pointer); subsequent drags update the scroll position; the
        // pointer-shape feedback below uses the doc Rect, not the
        // gutter, so the I-beam disappears over the scrollbar.
        if self.handle_scrollbar_event(&mouse_event, dims) {
            self.needs_draw = true;
            return;
        }

        // During a capturing (replace) search flow only viewport movement
        // is allowed — clicks and drags would relocate the cursor or start
        // a selection underneath the flow's own focus management.  Mirrors
        // the keyboard gate in `search_safe_action`.  A navigate-only search
        // does not capture, so clicks stay live there (in vim or default
        // mode).  `Moved` events stay live for pointer-shape tracking.
        // The same gate covers a live `:s` preview: the buffer transiently
        // shows previewed text, so a click / drag / checkbox toggle would
        // mutate (or place the cursor in) content that is about to revert.
        // Scrolling stays live so the user can inspect the preview.
        if (self.search_flow_captures() || self.editor.substitute_preview.is_some())
            && !matches!(
                mouse_event.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown | MouseEventKind::Moved
            )
        {
            return;
        }

        // Pointer-shape feedback: over a clickable element ask the
        // terminal for a pointing-hand cursor; otherwise I-beam.  Event
        // column/row are in terminal coords — translate to doc-relative
        // before hit-testing.
        let in_doc = mouse_event.column >= dims.doc_area.x
            && mouse_event.column < dims.doc_area.x + dims.doc_area.width
            && mouse_event.row >= dims.doc_area.y
            && mouse_event.row < dims.doc_area.y + dims.doc_area.height;
        // Also record the hovered link's URL (or clear it) for the
        // hint-line tooltip.  Keeping this update in the pointer-shape
        // path means it fires on every mouse-move, tracking the hover
        // in real time without an extra scan.
        self.refresh_hovered_link(&mouse_event, in_doc, dims);
        let desired = if in_doc {
            let rel_col = mouse_event.column - dims.doc_area.x;
            let rel_row = mouse_event.row - dims.doc_area.y;
            if mouse_ops::hit_test_clickable(
                &self.editor,
                rel_col,
                rel_row,
                dims.doc_width,
                &self.view_state.rendered.table_snapshots,
            ) {
                PointerShape::Hand
            } else {
                PointerShape::Text
            }
        } else {
            PointerShape::Default
        };
        self.update_pointer_shape(desired);

        // Moved-only events don't drive editor state; they're used
        // purely for pointer-shape tracking above.  Skip dispatch to
        // avoid emitting spurious actions.
        if matches!(mouse_event.kind, MouseEventKind::Moved) {
            return;
        }

        if let Some(mouse_action) = self.mouse.dispatch(mouse_event, dims.doc_area) {
            let snapshots = self.view_state.rendered.table_snapshots.clone();
            let scroll_before = self.editor.scroll;
            mouse_ops::apply(
                &mut self.editor,
                mouse_action,
                &mut self.drag_target,
                &snapshots,
                dims.doc_height,
                dims.doc_width,
            );
            if self.editor.scroll != scroll_before {
                self.mark_scrolling();
            }
            // Mouse click may have requested a link follow.
            // Consume it before the preview-state sync below so the
            // navigation runs first.
            if let Some(target) = self.editor.pending_link_follow.take() {
                self.follow_link(target, dims.doc_height, dims.doc_width);
            }
            // A column-border drag release sets
            // `pending_column_widths_commit`; either commit straight
            // through or open the warning modal depending on config +
            // table state.
            self.handle_pending_column_widths();
            self.needs_draw = true;
            // The action just applied may have scrolled the view,
            // edited the line, or navigated to another file — any of
            // which can change or remove the link under the stationary
            // pointer.  Recompute against the post-action state so the
            // hint-line URL doesn't go stale until the next mouse-move.
            self.refresh_hovered_link(&mouse_event, in_doc, dims);
        }
    }

    /// Recompute `hovered_link` for the pointer position carried by
    /// `mouse_event`, clearing it when the pointer is outside the doc
    /// area.  Sets `needs_draw` only on a hover *change* — Moved events
    /// otherwise skip the draw gate, and repainting every move would be
    /// wasted work while the pointer slides along one link.
    fn refresh_hovered_link(&mut self, mouse_event: &MouseEvent, in_doc: bool, dims: &DocDims) {
        let hovered = if in_doc {
            let rel_col = mouse_event.column - dims.doc_area.x;
            let rel_row = mouse_event.row - dims.doc_area.y;
            mouse_ops::hovered_link_url(&self.editor, rel_col, rel_row, dims.doc_width)
        } else {
            None
        };
        if hovered != self.hovered_link {
            self.hovered_link = hovered;
            self.needs_draw = true;
        }
    }

    /// Handle a bracketed-paste event: when the terminal pastes into
    /// the TUI (Ctrl-Shift-V, middle-click, ⌘V on macOS Terminal,
    /// right-click-paste, etc.) it delivers the full paste as a single
    /// `Event::Paste(String)`.  Route straight into the buffer so
    /// pasting from external apps always works, regardless of whether
    /// arboard can reach the OS clipboard from inside this process.
    ///
    /// Vim re-routes two cases so a paste never corrupts the document:
    /// while a `/` `?` (`:` in CP9) command line is open the paste fills
    /// the prompt instead of the buffer, and in any non-Insert sub-mode
    /// the buffer is read-only — a stray paste is dropped (use `p`/`P`
    /// to paste the register in Normal), upholding the "Normal mode does
    /// not edit" rule.
    pub(super) fn dispatch_paste(&mut self, text: String, dims: &DocDims) {
        if let Some(vim) = self.vim.as_mut() {
            if let Some(cl) = vim.cmdline.as_mut() {
                let before = cl.input.clone();
                crate::input::vim::cmdline::paste_str(cl, &text);
                // A paste changes the line like typing does — re-derive
                // the live `:s` / incsearch preview from the new text.
                crate::input::vim::feed::cmdline_live_update(
                    vim,
                    &mut self.editor,
                    &before,
                    dims.doc_height,
                    dims.doc_width,
                );
                self.needs_draw = true;
                return;
            }
            if vim.sub_mode != VimSubMode::Insert {
                return;
            }
        }
        edit_ops::paste_text(&mut self.editor, &text, dims.doc_height, dims.doc_width);
        self.needs_draw = true;
    }

    /// Handle a key (or other non-mouse / non-paste) event when no
    /// modal is open.  Reads ahead any additional key-press events
    /// already sitting in the channel so a burst of autorepeat events
    /// is processed and (where possible) coalesced into one buffer
    /// mutation per same-kind run, rather than one per keystroke.
    ///
    /// Drains the deferred external-editor flow at the end.
    pub(super) fn dispatch_key_event(
        &mut self,
        event: Event,
        dims: &DocDims,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) {
        // Any keypress dismisses the hovered-link tooltip: the edit or
        // mode switch it triggers can move or remove the link under the
        // stationary pointer, and nothing else would refresh the hover
        // until the next mouse-move.  Mirrors browsers hiding the
        // status-bar URL once the user starts typing.
        if self.hovered_link.take().is_some() {
            self.needs_draw = true;
        }
        let mut batch: Vec<Event> = vec![event];
        self.collect_key_burst(rx, &mut batch);
        self.dispatch_key_batch(batch, dims, terminal, rx);
    }

    /// Drain key-press events from the front of `pending_events` and
    /// from `rx` into `batch`.  Non-key terminal events interrupt the
    /// burst: they're stashed to `pending_events` (back, preserving
    /// channel order) so the next loop iteration routes them through
    /// the normal dispatcher.  Non-Term `AppEvent`s (image-ready /
    /// protocol-ready / link-open-result) are processed inline so the
    /// image pipeline doesn't starve behind a typing burst — mirrors
    /// `drain_pending_image_ready`.
    fn collect_key_burst(&mut self, rx: &mpsc::Receiver<AppEvent>, batch: &mut Vec<Event>) {
        while matches!(self.pending_events.front(), Some(e) if is_key_press(e)) {
            if let Some(e) = self.pending_events.pop_front() {
                batch.push(e);
            }
        }
        loop {
            match rx.try_recv() {
                Ok(AppEvent::Term(e)) => {
                    if is_key_press(&e) {
                        batch.push(e);
                    } else {
                        self.pending_events.push_back(e);
                        break;
                    }
                }
                Ok(ev) => self.handle_async_event(ev),
                Err(_) => break,
            }
        }
    }

    /// Process a batch of key-press events.  The first event in any
    /// run of same-coalesceable-kind actions is always dispatched
    /// through the regular per-event path (so list-marker erase,
    /// Preview-to-Rendered transition, selection-clearing-delete, and
    /// other one-shot transitions still fire).  Subsequent same-kind
    /// events with no selection / no modal / no Preview mode collapse
    /// into a single `apply_insert_run` / `apply_delete_run` call —
    /// one buffer mutation, one history entry, one `parsed_version`
    /// bump for the whole burst.
    ///
    /// When a single dispatch opens a modal, sets a pending external-
    /// editor flag, or sets `pending_link_follow`, the remaining events
    /// are pushed back to the front of `pending_events` so the next
    /// run-loop iteration routes them through the appropriate dispatcher
    /// (modal vs. key vs. deferred-flow).
    fn dispatch_key_batch(
        &mut self,
        events: Vec<Event>,
        dims: &DocDims,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) {
        let keymap = self
            .keymap
            .as_ref()
            .cloned()
            .unwrap_or_else(|| KeyMap::build(&KeyBindingOverrides::default()).unwrap());

        let mut i = 0;
        while i < events.len() {
            // 1. Dispatch event[i] individually.  This handles every
            //    one-shot transition (Preview→Rendered, selection
            //    clear, list-marker consume) before we attempt to
            //    coalesce subsequent same-kind events.
            // `resolve_action` runs the keymap, so a bare Normal-mode vim
            // key resolves to `InsertChar` here even though the vim
            // intercept inside `dispatch_single_key` consumes it instead.
            // That mismatch is harmless: the `vim.sub_mode != Insert` guard
            // below `continue`s before any coalesce run is built, so this
            // `coalesce` value is intentionally discarded for vim-consumed
            // keys.  Don't reorder these past the guard.
            let action_i = resolve_action(&events[i], &keymap, &self.editor);
            let coalesce = action_i.as_ref().and_then(Action::coalesce_kind);
            self.dispatch_single_key(events[i].clone(), &keymap, dims);
            i += 1;
            if self.should_quit {
                self.requeue_remaining(&events[i..]);
                break;
            }

            // 2. If a side-effect has fired that demands re-dispatch
            //    routing (modal opened, external-editor pending,
            //    link-follow queued), stop draining and route the rest
            //    through the next run-loop iteration.
            if self.should_break_after_dispatch() {
                self.requeue_remaining(&events[i..]);
                return;
            }
            // If event[i-1] wasn't coalesceable, or the editor state
            // can't extend a run, fall through to the next outer
            // iteration (which dispatches event[i] regularly).
            let Some(kind) = coalesce else { continue };
            if self.editor.selection.is_some()
                || self.drag_target.is_some()
                || self.editor.mode == crate::editor::Mode::Preview
                // A capturing search flow blocks buffer edits via the
                // `search_safe_action` gate inside `dispatch_action`;
                // the coalesced insert/delete runs below bypass that
                // dispatch entirely, so an autorepeat burst must not
                // be allowed to extend a run mid-flow.  A vim navigate
                // search does not capture, so Insert-mode typing during
                // one still coalesces normally.
                || self.search_flow_captures()
                // Vim outside Insert must not coalesce: a held digit
                // (`333`) accumulates a count, and bare keys are
                // commands — neither is `InsertChar` typing.  Only
                // Insert sub-mode gets the normal typing burst.
                || self
                    .vim
                    .as_ref()
                    .is_some_and(|v| v.sub_mode != VimSubMode::Insert)
            {
                continue;
            }

            let mut run_chars: Vec<char> = Vec::new();
            let mut run_count = 0usize;
            while i < events.len() {
                let Some(action_n) = resolve_action(&events[i], &keymap, &self.editor) else {
                    break;
                };
                if action_n.coalesce_kind() != Some(kind) {
                    break;
                }
                // Selection becomes live only between events; sample it
                // per iteration so a mid-batch selection (impossible
                // for pure typing, but cheap to guard) ends the run.
                if self.editor.selection.is_some() {
                    break;
                }
                if let Action::InsertChar(c) = action_n {
                    run_chars.push(c);
                }
                run_count += 1;
                i += 1;
            }
            if run_count > 0 {
                let scroll_before = self.editor.scroll;
                match kind {
                    CoalesceKind::Insert => {
                        edit_ops::apply_insert_run(
                            &mut self.editor,
                            &run_chars,
                            dims.doc_height,
                            dims.doc_width,
                        );
                    }
                    CoalesceKind::BackDelete => {
                        edit_ops::apply_delete_run(
                            &mut self.editor,
                            run_count,
                            true,
                            dims.doc_height,
                            dims.doc_width,
                        );
                    }
                    CoalesceKind::ForwardDelete => {
                        edit_ops::apply_delete_run(
                            &mut self.editor,
                            run_count,
                            false,
                            dims.doc_height,
                            dims.doc_width,
                        );
                    }
                }
                if self.editor.scroll != scroll_before {
                    self.mark_scrolling();
                }
                self.needs_draw = true;
            }
            if self.should_break_after_dispatch() {
                self.requeue_remaining(&events[i..]);
                return;
            }
        }

        if self.pending_open_file_in_editor {
            self.pending_open_file_in_editor = false;
            self.open_current_file_in_editor(terminal, rx);
        }
        if let Some(path) = self.pending_open_theme_in_editor.take() {
            self.open_theme_in_editor(&path, terminal, rx);
        }
    }

    /// Dispatch one key event through the unified
    /// [`App::dispatch_action`] pipeline.  The external-editor drain
    /// is intentionally not handled here — `dispatch_key_batch` runs
    /// it once at the end of a batch.
    pub(super) fn dispatch_single_key(&mut self, event: Event, keymap: &KeyMap, dims: &DocDims) {
        // Vim intercept: when the vim handler is active, it owns the key
        // first.  Two exceptions defer to a flow that hard-binds these
        // keys downstream and would otherwise be shadowed:
        //   - Diff mode — the diff-review keymap owns its keys.
        //   - A *capturing* search flow — a replace flow (or any search in
        //     non-vim mode).  Its bindings (`Esc`/`r`/`a`, …) are matched in
        //     `DefaultHandler::handle`, which runs *after* this intercept;
        //     without this guard vim Normal would swallow `Esc` and trap the
        //     user.  A vim navigate-only search does *not* defer — vim owns
        //     `n`/`N` and every other key over the highlighted matches (§2.3).
        // A `Pending`/`Consumed` outcome ends dispatch here; an `EnterSearch`
        // starts the search flow; a `Passthrough` (e.g. a `Ctrl-*` chord, or
        // any printable char in Insert mode) falls through to the default
        // keymap path below.
        let vim_deferred = self.editor.mode == Mode::Diff || self.search_flow_captures();
        if let Event::Key(key) = &event {
            if key.kind == KeyEventKind::Press && !vim_deferred {
                if let Some(vim) = self.vim.as_mut() {
                    let key = *key;
                    match vim_feed(vim, &mut self.editor, key, dims.doc_height, dims.doc_width) {
                        VimOutcome::Pending | VimOutcome::Consumed => {
                            self.needs_draw = true;
                            return;
                        }
                        VimOutcome::EnterSearch { forward, query } => {
                            self.enter_vim_search(query, forward);
                            self.needs_draw = true;
                            return;
                        }
                        // `:w` / `:q` / `:wq` route through the existing
                        // save / quit actions so the dirty-buffer confirm and
                        // save flash behave exactly as for `Ctrl-*`.  `:wq`
                        // saves first, leaving the buffer clean before the
                        // quit guard runs.
                        VimOutcome::Save => {
                            self.dispatch_action(Action::Save, dims.doc_height, dims.doc_width);
                            self.needs_draw = true;
                            return;
                        }
                        VimOutcome::Quit { save_first } => {
                            if save_first {
                                self.dispatch_action(Action::Save, dims.doc_height, dims.doc_width);
                            }
                            self.dispatch_action(Action::Quit, dims.doc_height, dims.doc_width);
                            self.needs_draw = true;
                            return;
                        }
                        // `:w <path>` / `:saveas` / a path-less `:w`.  A
                        // named destination saves directly (confirming an
                        // overwrite of a different existing file first,
                        // unless `force` from a trailing `!`); an unnamed
                        // one opens the Save As modal.  `then_quit` (`:wq` /
                        // `:x`) quits once the write succeeds.
                        VimOutcome::SaveAs {
                            path,
                            then_quit,
                            force,
                        } => {
                            let after: Option<crate::app::modal::save_as::AfterSave> = if then_quit
                            {
                                Some(Box::new(|app| app.should_quit = true))
                            } else {
                                None
                            };
                            match path {
                                Some(p) => self.save_buffer_as_confirmed(p, force, after),
                                None => self.open_save_as_modal(after),
                            }
                            self.needs_draw = true;
                            return;
                        }
                        // `:w <path>` / `:wq <path>` — write a copy to the
                        // named path, keeping the current file (real vim).
                        // Confirms an overwrite of a different existing file
                        // unless `force` (`:w!`).
                        VimOutcome::SaveCopy {
                            path,
                            then_quit,
                            force,
                        } => {
                            let after: Option<crate::app::modal::save_as::AfterSave> = if then_quit
                            {
                                Some(Box::new(|app| app.should_quit = true))
                            } else {
                                None
                            };
                            self.save_copy_confirmed(path, force, after);
                            self.needs_draw = true;
                            return;
                        }
                        // A `:s` result or an ex parse / regex error: the
                        // substitution already ran in the reducer; just flash.
                        VimOutcome::Flash(text) => {
                            self.flash(text, MessageKind::Info);
                            self.needs_draw = true;
                            return;
                        }
                        VimOutcome::Passthrough => {}
                    }
                }
            }
        }

        let mut handler = DefaultHandler::new(keymap);
        let Some(action) = handler.handle_event(event, &self.editor) else {
            return;
        };
        self.dispatch_action(action, dims.doc_height, dims.doc_width);
        self.needs_draw = true;
    }

    /// True when a just-dispatched event left App state that the next
    /// event in the batch shouldn't route through `dispatch_single_key`:
    /// a modal has opened, an external-editor flow is pending, or a
    /// link-follow is queued.  The remaining batch is pushed back onto
    /// the front of `pending_events` so the run loop dispatches it
    /// through the appropriate path on its next iteration.
    fn should_break_after_dispatch(&self) -> bool {
        !self.modal_stack.is_empty()
            || self.pending_open_file_in_editor
            || self.pending_open_theme_in_editor.is_some()
            || self.pending_open_config_in_editor
            || self.editor.pending_link_follow.is_some()
    }

    /// Push `remaining` onto the front of `pending_events` in their
    /// original order.  Reverses iteration so the first event of
    /// `remaining` ends up at the front of the queue.
    fn requeue_remaining(&mut self, remaining: &[Event]) {
        for e in remaining.iter().rev() {
            self.pending_events.push_front(e.clone());
        }
    }
}

/// True iff `event` is a `Event::Key(Press)` — the only kind of
/// terminal event the coalescing path accepts into a key batch.
fn is_key_press(event: &Event) -> bool {
    matches!(event, Event::Key(k) if k.kind == KeyEventKind::Press)
}

/// Resolve `event` to an `Action` via a freshly-constructed
/// `DefaultHandler`.  Returns `None` for non-key events or keys with
/// no binding.  Stateless — safe to call from the run-detection
/// look-ahead without mutating App state.
fn resolve_action(
    event: &Event,
    keymap: &KeyMap,
    editor: &crate::editor::EditorState,
) -> Option<Action> {
    let mut handler = DefaultHandler::new(keymap);
    handler.handle_event(event.clone(), editor)
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
        | mouse_ops::DragTarget::TextSelection { .. }
        | mouse_ops::DragTarget::Scrollbar { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;

    use crate::app::test_utils::app_with_buffer;
    use crate::config::{KeyBindingOverrides, KeyMap};
    use crate::search::SearchState;

    use super::DocDims;

    fn dims() -> DocDims {
        DocDims {
            doc_height: 10,
            doc_width: 60,
            doc_area: Rect::new(0, 0, 60, 10),
        }
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn wheel_scroll_refreshes_link_hover_under_stationary_pointer() {
        let mut app = app_with_buffer(
            "[docs](https://example.com)\nplain text line\nmore plain text\n",
            0,
        );
        app.capabilities.mouse = true;
        let dims = dims();

        // Hover the link on the first rendered line.
        app.dispatch_mouse_event(mouse(MouseEventKind::Moved, 2, 0), &dims);
        assert_eq!(app.hovered_link.as_deref(), Some("https://example.com"));

        // Wheel-scroll one line without moving the pointer: the link
        // scrolls away and a plain line lands under the cursor.  The
        // hover must track the post-scroll state, not show the old URL
        // until the next mouse-move.
        app.dispatch_mouse_event(mouse(MouseEventKind::ScrollDown, 2, 0), &dims);
        assert!(
            app.hovered_link.is_none(),
            "hover must refresh after a mouse action scrolls the view"
        );
    }

    #[test]
    fn pointer_leaving_doc_area_clears_hover() {
        let mut app = app_with_buffer("[docs](https://example.com)\n", 0);
        app.capabilities.mouse = true;
        let dims = dims();

        app.dispatch_mouse_event(mouse(MouseEventKind::Moved, 2, 0), &dims);
        assert!(app.hovered_link.is_some());

        // Move below the doc area (e.g. onto the status bar).
        app.dispatch_mouse_event(mouse(MouseEventKind::Moved, 2, 10), &dims);
        assert!(app.hovered_link.is_none());
    }

    #[test]
    fn esc_exits_the_search_flow_even_with_vim_active() {
        // Regression: the vim intercept in `dispatch_single_key` must
        // defer to the search flow (which hard-binds `Esc` → `SearchExit`
        // downstream in `DefaultHandler`).  Without the deferral vim
        // Normal swallows `Esc` (`reset_pending`, Consumed) and the user
        // is trapped in the flow with no way out.
        let mut app = app_with_buffer("hello world\n", 0);
        app.set_vim_enabled(true);
        let keymap = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let dims = dims();

        // Enter a navigate-only search flow with at least one match.
        let search = SearchState::new("world".to_string(), None).unwrap();
        app.editor.enter_search(search);
        assert!(app.editor.search.is_some(), "search flow is active");

        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.dispatch_single_key(esc, &keymap, &dims);

        assert!(
            app.editor.search.is_none(),
            "Esc must exit the search flow, not be swallowed by vim Normal"
        );
    }

    #[test]
    fn tab_walks_a_navigate_search_started_outside_vim() {
        // A navigate search started via Ctrl-F / palette (not `/`) must still
        // be Tab-navigable when vim is enabled — the key reaches vim_feed
        // (the flow doesn't capture), which advances the match like `n`.
        let mut app = app_with_buffer("foo bar foo baz foo\n", 0);
        app.set_vim_enabled(true);
        let keymap = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let dims = dims();
        let search = SearchState::new("foo".to_string(), None).unwrap();
        app.editor.enter_search(search);
        assert_eq!(app.editor.search.as_ref().unwrap().focused_idx, 0);

        let tab = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.dispatch_single_key(tab, &keymap, &dims);
        assert_eq!(
            app.editor.search.as_ref().unwrap().focused_idx,
            1,
            "Tab advances the focused match"
        );
        // The buffer must be untouched (no `InsertTab` leaked through).
        assert_eq!(app.editor.buffer.contents(), "foo bar foo baz foo\n");
    }

    #[test]
    fn paste_into_an_open_vim_command_line_fills_the_prompt_not_the_buffer() {
        use crate::input::vim::state::{CmdLineKind, CmdLineState};
        let mut app = app_with_buffer("hello\n", 0);
        app.set_vim_enabled(true);
        if let Some(vim) = app.vim.as_mut() {
            vim.cmdline = Some(CmdLineState::new(CmdLineKind::SearchForward));
        }
        let before = app.editor.buffer.contents();
        app.dispatch_paste("wor".to_owned(), &dims());
        assert_eq!(app.editor.buffer.contents(), before, "buffer untouched");
        let cl = app.vim.as_ref().unwrap().cmdline.as_ref().unwrap();
        assert_eq!(cl.input, "wor");
        assert_eq!(cl.cursor, 3);
    }

    #[test]
    fn paste_in_vim_normal_mode_does_not_edit_the_buffer() {
        // Regression: a bracketed paste in Normal used to fall straight into
        // the buffer (and could panic by desyncing the parsed doc).
        let mut app = app_with_buffer("hello\n", 0);
        app.set_vim_enabled(true); // default sub_mode = Normal
        let before = app.editor.buffer.contents();
        app.dispatch_paste("XYZ".to_owned(), &dims());
        assert_eq!(
            app.editor.buffer.contents(),
            before,
            "Normal mode does not edit"
        );
    }

    #[test]
    fn command_line_paste_strips_newlines() {
        use crate::input::vim::state::{CmdLineKind, CmdLineState};
        let mut app = app_with_buffer("hi\n", 0);
        app.set_vim_enabled(true);
        if let Some(vim) = app.vim.as_mut() {
            vim.cmdline = Some(CmdLineState::new(CmdLineKind::SearchForward));
        }
        app.dispatch_paste("a\nb\r\nc".to_owned(), &dims());
        assert_eq!(
            app.vim.as_ref().unwrap().cmdline.as_ref().unwrap().input,
            "abc",
            "multi-line paste collapses to a single search line"
        );
    }
}
