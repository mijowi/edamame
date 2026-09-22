//! The run loop's phases, one method on `App` per concern: setup, per-iteration
//! frame preparation, drawing, event acquisition, and event dispatch.  `App::run`
//! itself stays in `app.rs` and reads as a flat sequence of these steps.

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
use crate::ui::text_input::PASTE_CHAR_CAP;
use crate::ui::{position_for_click, position_for_drag, thumb_range, EditorView, ModalKind};
use crate::watcher::{NotifyWatcher, WatchedEvent};

use super::actions::{modal_wheel_delta, HandleEvent};
use super::flash::MessageKind;
use super::frame_timer::{MIN_FRAME_INTERVAL, RESIZE_QUIESCE};
use super::modal::ModalRenderCtx;
use super::{App, AppEvent};

/// Per-frame document-area dimensions, computed once per loop iteration so
/// every dispatch arm sees the same numbers.
pub(super) struct DocDims {
    /// Document-area height in rows (`term_size.height - bottom_rows`).
    pub doc_height: usize,
    /// Document-area width, terminal width less any `max_width_enabled` clamp.
    pub doc_width: usize,
    /// Rectangle used for mouse hit-testing; `x` is the centered offset when
    /// the max-width clamp is active.
    pub doc_area: Rect,
}

impl App {
    // ── Setup ─────────────────────────────────────────────────────────────────

    /// Ask for an I-beam pointer over the TUI area.  Terminals without OSC 22
    /// ignore it; no-op when mouse capabilities are absent.
    pub(super) fn startup_pointer_hint(&mut self) {
        if self.capabilities.mouse {
            self.update_pointer_shape(PointerShape::Text);
        }
    }

    /// Build the main-loop mpsc channel and spawn the background threads that
    /// feed it (terminal-event reader, watcher bridge, image-encode worker),
    /// returning the receiver.  Also populates `self.app_tx`.
    ///
    /// The reader thread is `poll`-based so its pause flag can take effect
    /// without interrupting a blocked syscall: while the App shells out to an
    /// external editor the child needs uncontested stdin, or both processes race
    /// for terminal bytes and the editor sees a corrupted input stream.
    ///
    /// The encoder thread funnels every resize-encode through one CPU — encoding
    /// is CPU-bound, so serial execution keeps cache locality and avoids
    /// contention on the terminal's graphics state.  The UI thread never
    /// encodes; it enqueues `ResizeRequest`s and paints the result.
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

        // Spawned even with no file open, so the wiring is uniform;
        // `App::start_file_watcher` supplies the path later.  Failure is
        // non-fatal — the user just won't see external-edit prompts — and skips
        // the bridge thread, which would exit on its first `recv` anyway.
        let (watch_tx, watch_rx) = mpsc::channel::<WatchedEvent>();
        match NotifyWatcher::new(watch_tx) {
            Ok(w) => {
                self.watcher = Some(Box::new(w));
                // The bridge exists so the watcher's public API can stay
                // generic in `mpsc::Sender<WatchedEvent>` — testable without
                // an `App` — rather than baking `AppEvent` in.
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
        // Keep a clone: every later document gets a fresh `EditorState`, and so
        // a fresh `ImageCache`, which needs the same sender or its images never
        // reach a protocol.  See `App::resize_tx`.
        self.editor.images.attach_resize_sender(resize_tx.clone());
        self.resize_tx = Some(resize_tx);
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

    /// Begin watching the currently-open file, if any.  Called once from the run
    /// loop after [`Self::spawn_event_threads`]; failures are non-fatal.
    ///
    /// A notify event racing the gap between `FileWatcher::watch` and the worker
    /// receiving `SetPath` is dropped.  No reconcile is forced here on purpose:
    /// the just-loaded buffer *is* the on-disk state.  (The external-editor flow
    /// does force one, because there the two can genuinely differ.)
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

    /// Build the live keymap once and stash it on `self`, held for the life of
    /// the process so the keybinds overlay can mutate it in place.
    pub(super) fn build_keymap_if_needed(&mut self) -> Result<()> {
        if self.keymap.is_none() {
            self.keymap = Some(KeyMap::build(&self.keybindings)?);
        }
        Ok(())
    }

    // ── Per-iter prep ─────────────────────────────────────────────────────────

    /// Apply the time-driven state changes that happen before any event is
    /// read, setting `needs_draw` when any of them changed visible state.
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
        self.spawn_startup_update_check();
        self.tick_update_notice();
        // After the update notice: a tip yields to an update, and reads whether one is pending.
        self.tick_daily_tip();
        self.tick_syntax_warm();
        self.editor.modal_open = self.any_modal_open();
    }

    /// Repaint when the grammar warm worker has finished a language.
    ///
    /// Grammar compilation happens on a worker and the block renders plain
    /// meanwhile.  Nothing else would bring the color in: warming changes no
    /// `Block` value, so `RenderCache` would keep serving the plain render.  The
    /// generation rides in `RenderSettings`, so `refresh_parsed` misses the
    /// cache and re-renders with tokens.  Polled rather than pushed because
    /// `markdown` sits far below `app` and must not learn about `AppEvent`.
    ///
    /// **Two reasons to reparse, not one.**  The generation covers *queued*
    /// grammars; `refused_grammar_retry_due` covers those
    /// `MAX_HIGHLIGHT_GRAMMARS` turned away.  Without it that cap is a session
    /// limit: the queued burst compiles well inside the refill window, so the
    /// generation stops moving while refusals still stand.  The retry is
    /// edge-triggered, so a standing refusal costs one reparse per refilled slot.
    ///
    /// It also bumps `highlight::retry_epoch`, which rides in the same
    /// fingerprint — otherwise the retry-path reparse would hit the cache
    /// wholesale, `render_code_block` would never run, the refilled slot would
    /// go unspent, and the language would stay plain for the session.
    fn tick_syntax_warm(&mut self) {
        if !self.config.editor.syntax_highlighting {
            return;
        }
        let generation = crate::markdown::highlight::warm_generation();
        let warmed = generation != self.syntax_warm_generation;
        // Asked unconditionally: it consumes its own edge, so skipping it on a
        // frame that already reparsed would drop that slot's retry.
        let retry_due = crate::markdown::highlight::refused_grammar_retry_due();
        if !warmed && !retry_due {
            return;
        }
        self.syntax_warm_generation = generation;
        self.editor.refresh_parsed();
        self.needs_draw = true;
    }

    /// Coalesce `ImageReady`-driven cache mutations into one parse-and-render
    /// pass, so a burst of N decode completions costs one reparse, not N.
    pub(super) fn coalesce_image_updates(&mut self) {
        if self.images_dirty {
            self.editor.refresh_parsed();
            self.images_dirty = false;
            self.needs_draw = true;
        }
    }

    /// Translate `term_size` into the iteration's document-area dimensions.
    pub(super) fn compute_doc_dims(&self, term_size: Size) -> DocDims {
        let bottom_rows = crate::ui::BottomRegion::height();
        let doc_height = (term_size.height as usize).saturating_sub(bottom_rows as usize);
        let full_doc_area = Rect {
            x: 0,
            y: 0,
            width: term_size.width,
            height: term_size.height.saturating_sub(bottom_rows),
        };
        // Mirror `EditorView::render`'s gutter + scrollbar + max-width layout so
        // `viewport_width`, hit-testing and wrap agree with the painted area.
        let line_count = if self.config.editor.show_line_numbers {
            match self.editor.mode {
                Mode::Preview | Mode::Rendered => self.editor.parsed.line_count(),
                Mode::Raw => self.editor.buffer.line_count(),
                // Diff mode paints no gutter; zero keeps `viewport_width`
                // matching what `DiffView` paints into.
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

    /// Refresh per-frame state that depends on the live document width, and kick
    /// off decodes for images within the near-viewport window.  Cheap to re-run
    /// each frame: `ImageCache::request` short-circuits any non-Idle URL, so
    /// only URLs that just entered the window spawn work.
    pub(super) fn prepare_viewport(&mut self, dims: &DocDims) {
        self.last_area_width = dims.doc_area.width;
        self.last_doc_height = dims.doc_height;
        self.last_doc_width = dims.doc_width;
        self.editor.set_viewport_width(dims.doc_width);
        // `parsed` is one mode-independent spine, so a mode switch that changes whether paragraphs
        // reflow has to rebuild it.  A no-op when the reflow state already agrees with the mode.
        self.editor.sync_reflow_for_mode();
        // Latch a mermaid / reflowed-paragraph reveal once its dwell delay elapses, so it stays
        // revealed as the cursor moves within it (the delay itself re-arms per line, time-driven,
        // so this has no action site of its own).
        self.editor.latch_cursor_reveal();
        // Reconcile cursor visibility across a reflowed block's reveal/un-reveal height change
        // (time-driven, so it has no action site of its own): the block's top and everything above
        // stay put and only content below reflows.  Inert unless reflow is on in Rendered mode.
        self.editor
            .anchor_reflow_reveal(dims.doc_width, dims.doc_height);
        // The cursor this file was left at lands at the viewport's vertical middle.  Applied
        // *before* the anchor below, which is explicit command-line intent and wins.
        self.apply_pending_cursor_restore(dims.doc_height, dims.doc_width);
        // A command-line `#section` applies on the first frame that knows the
        // document's dimensions and clears itself.
        self.apply_startup_anchor(dims.doc_height, dims.doc_width);
        // Resolve a deferred diff-parse build now the diff-mode width is posted
        // (see `diff_parse_dirty`); a no-op if the width actually changed.
        self.editor.flush_diff_parse_if_dirty();
        // An image block swaps its reserved image rows for one row per raw
        // source line once the cursor rests inside it.  The transition is
        // time-driven (the reveal delay elapses with no event of its own), so
        // it is resolved per frame here.
        //
        // The reflow moves rendered rows under a cursor that didn't move, and
        // `ensure_cursor_visible` otherwise only runs off a move or an edit, so
        // it has to be called explicitly.  `dims` is measured *before* the
        // reflow, so a reveal that changes the line count can paint one frame
        // with a stale gutter width; `needs_draw` re-measures next frame.
        if self.editor.sync_image_reveal() {
            self.editor
                .ensure_cursor_visible(dims.doc_height, dims.doc_width);
            self.needs_draw = true;
        }
        // A non-capturing navigate flow lets the buffer be edited freely, so the
        // match list can go stale.  Version-guarded, hence a no-op when nothing
        // changed.  Paused under a `:s` preview: recomputing against transient
        // text would re-anchor a coexisting hlsearch session to text about to
        // revert (the overlay painters suspend the wash for the same reason).
        if self.editor.substitute_preview.is_none() {
            self.editor.ensure_search_fresh();
        }
        // Resolve a scroll request now the viewport height is known (it isn't at
        // the modal-close site that enters diff mode).  One-shot.
        if self.editor.pending_focus_scroll {
            // Shared by diff entry and the search flow; each helper no-ops when
            // its session isn't active.
            self.editor
                .scroll_focused_hunk_into_view(dims.doc_height, dims.doc_width);
            self.editor
                .scroll_focused_match_into_view(dims.doc_height, dims.doc_width);
            self.editor.pending_focus_scroll = false;
            self.needs_draw = true;
        }
        // Diff mode's `scroll` is a *diff visual row*, so the editor's
        // source-map window would be meaningless there; it gets its own helper
        // over the rendered context rows.  Without this, an image undecoded when
        // the review opened would reserve blank rows for the whole review.
        if self.editor.mode == Mode::Diff {
            self.dispatch_visible_diff_image_decodes(self.editor.scroll, dims.doc_height);
        } else {
            self.dispatch_visible_image_decodes(self.editor.scroll, dims.doc_height);
        }
    }

    /// True when this iteration should call `terminal.draw`: state changed, the
    /// frame throttle is satisfied, and no resize burst is in flight.
    /// `since_draw` is `None` before the first draw.
    pub(super) fn should_draw(&self, since_draw: Option<Duration>) -> bool {
        let throttle_ok = since_draw.is_none_or(|d| d >= MIN_FRAME_INTERVAL);
        let resize_pending = self.resize_quiesce_at.is_some();
        self.needs_draw && throttle_ok && !resize_pending
    }

    /// Render one frame: the editor view plus the topmost modal, updating
    /// `last_draw_at` and clearing `needs_draw`.
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
        // Hide the modal cursor when the window has lost focus, mirroring
        // `EditorState::cursor_visible`.
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
        // One tick per real draw, not per paint pass (Raw and Diff draw without
        // painting images): `image_view::paint_native` reuses a native
        // transmission only from the immediately preceding frame.
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
                // The modal's own `Clear` + bg fill overwrites its rect
                // cleanly, so this dim sweep can cover the whole area without
                // computing a complement.  See `crate::ui::dim`.
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

    /// Apply a non-[`AppEvent::Term`] event: image decode/encode completion,
    /// link-open result, watcher notification, export or release check.
    ///
    /// Centralized so the three receiving sites ([`Self::next_event`],
    /// [`Self::collect_key_burst`], `App::drain_pending_image_ready`) stay in
    /// lockstep as variants are added; they inlined the same arms before and
    /// drifted.  Callers coalesce follow-on image events from `rx` themselves.
    ///
    /// `Term` events are caller-specific (passthrough vs. stash vs. key-press
    /// batching), so they stay at each call site.
    pub(super) fn handle_async_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Term(_) => {
                // Caller bug; logged rather than panicked so release builds
                // keep the run loop alive.
                debug_assert!(false, "handle_async_event called with Term");
                tracing::warn!(
                    target: "app",
                    "handle_async_event called with Term — should be filtered by caller",
                );
            }
            AppEvent::ImageReady(Ok(loaded)) => {
                // The worker captured the settings at spawn time, so recheck
                // the current ones: a slow remote fetch must not resurface
                // after the user tightened the remote policy.  Forgetting the
                // entry lets the per-frame dispatch re-resolve the URL if the
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
                    loaded.sliced,
                    loaded.direct,
                );
                self.images_dirty = true;
                self.needs_draw = true;
            }
            AppEvent::ImageReady(Err((url, message))) => {
                tracing::debug!(target: "image", %url, %message, "image decode failed");
                // Only a still-`Pending` entry may take the failure.  An
                // evicted one must not be resurrected (memoizing the failure
                // would pin the URL against settings it was evicted under), and
                // a `Ready` one means a newer worker already delivered a good
                // decode — `request` never retries a `Failed` entry, so a stale
                // failure would be permanent.
                if !matches!(
                    self.editor.images.status(&url),
                    Some(crate::image::DecodeStatus::Pending)
                ) {
                    return;
                }
                self.editor.images.set_failed(&url, message);
                // A failure collapses the block to 1 row (see
                // `ImageCache::reserved_rows`), so the parse must be rebuilt.
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
                self.handle_release_check_result(result);
            }
        }
    }

    /// Pull the next event the run loop should process.  `None` means a
    /// background event was handled internally or a deadline elapsed and the
    /// loop should `continue`; on channel disconnect it also sets `should_quit`.
    ///
    /// `since_draw` lets the wait end at the remaining frame-throttle budget
    /// when `needs_draw` was set but the throttle blocked the draw.  With
    /// nothing pending this blocks on `rx.recv()`, so the app idles at 0 % CPU.
    pub(super) fn next_event(
        &mut self,
        rx: &mpsc::Receiver<AppEvent>,
        since_draw: Option<Duration>,
    ) -> Option<Event> {
        // Replay any stashed Term events before consulting the channel.
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
                // Coalesce queued image/protocol events into one refresh.
                // Also drains other non-Term events, which is harmless.
                self.drain_pending_image_ready(rx);
                None
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // A deadline (reveal / quiesce / throttle) elapsed with no
                // event.  Redraw once; the loop then blocks on `recv()` again.
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

    /// Handle a `Resize`: arm a quiesce deadline so the burst doesn't pin CPU on
    /// partial frames, invalidate width-dependent snapshot caches, and clear
    /// `last_scroll_at` so newly-visible images render natively at once.
    pub(super) fn on_resize(&mut self) {
        self.resize_quiesce_at = Some(Instant::now() + RESIZE_QUIESCE);
        self.view_state.rendered.image_snapshots_key = None;
        self.view_state.rendered.link_snapshots_key = None;
        self.view_state.rendered.table_snapshots_key = None;
        self.view_state.preview.image_snapshots_key = None;
        self.view_state.preview.link_snapshots_key = None;
        self.last_scroll_at = None;
        // A repaint of the whole screen invalidates every native transmission,
        // even one whose rect is unchanged.
        self.editor.images.invalidate_native_paints();
    }

    /// Route an event to the topmost modal, absorbing anything it doesn't
    /// handle so the editor behind it can't react.
    ///
    /// Drains any pending external-editor flow at the end, so the
    /// `&mut Terminal` / `&mpsc::Receiver` borrows don't leak into actions.rs.
    pub(super) fn dispatch_modal_event(
        &mut self,
        event: &Event,
        dims: &DocDims,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) {
        // Snap the `▏` cursor visible on any keypress.
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

        // Deferred here because the editor invocation needs `&mut Terminal`
        // and `&rx`, which only this scope holds.
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

    /// Pre-empt scrollbar interactions before normal mouse dispatch.  `true`
    /// when the scrollbar fully consumed the event (drag in flight, gutter click
    /// or hover), in which case the caller must not pass it to
    /// `MouseDispatcher`.
    fn handle_scrollbar_event(&mut self, mouse_event: &MouseEvent, dims: &DocDims) -> bool {
        let dragging = matches!(
            self.drag_target,
            Some(mouse_ops::DragTarget::Scrollbar { .. })
        );
        let metrics = match self.view_state.scrollbar {
            Some(m) => m,
            None => {
                // The scrollbar disappeared (content shrank, mode change,
                // resize): clear lingering state and dispatch normally.
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
                // Rows above the gutter map to the track start, rows below
                // clamp to track-1; saturating u16 arithmetic covers both.
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

        // Wheel ticks in the gutter scroll the document as they would over the
        // body, so they fall through.  Anything else arriving mid-drag is
        // absorbed, so it can't drive text selection underneath.
        if dragging {
            return true;
        }
        false
    }

    /// Handle a mouse event when no modal is open: pointer-shape feedback,
    /// hover-link tracking, and (for non-Moved events) dispatch through
    /// `MouseDispatcher` and `mouse_ops::apply`.
    pub(super) fn dispatch_mouse_event(&mut self, mouse_event: MouseEvent, dims: &DocDims) {
        // Clicks hit-test against `parsed.source_map` byte ranges, so a stale
        // map would resolve to the wrong block.  A click ends the typing burst
        // anyway, so the synchronous flush costs no visible latency.
        if self.editor.flush_parsed_if_dirty() {
            self.needs_draw = true;
        }
        if !self.capabilities.mouse {
            return;
        }

        // Runs before every other dispatch.  The pointer-shape feedback below
        // uses the doc Rect, not the gutter, so the I-beam disappears over the
        // scrollbar.
        if self.handle_scrollbar_event(&mouse_event, dims) {
            self.needs_draw = true;
            return;
        }

        // During a capturing (replace) search flow, or a live `:s` preview,
        // only viewport movement is allowed: a click or drag would relocate the
        // cursor under the flow's own focus management, or mutate text that is
        // about to revert.  Mirrors the keyboard gate in `search_safe_action`.
        // A navigate-only search does not capture, so clicks stay live there.
        if (self.search_flow_captures() || self.editor.substitute_preview.is_some())
            && !matches!(
                mouse_event.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown | MouseEventKind::Moved
            )
        {
            return;
        }

        // Pointer-shape feedback.  Event coordinates are terminal-relative and
        // must be translated before hit-testing.
        let in_doc = mouse_event.column >= dims.doc_area.x
            && mouse_event.column < dims.doc_area.x + dims.doc_area.width
            && mouse_event.row >= dims.doc_area.y
            && mouse_event.row < dims.doc_area.y + dims.doc_area.height;
        // Records the hovered URL for the hint-line tooltip; sharing the
        // pointer-shape path tracks the hover live without an extra scan.
        self.refresh_hovered_link(&mouse_event, in_doc, dims);
        let desired = if in_doc {
            let rel_col = mouse_event.column - dims.doc_area.x;
            let rel_row = mouse_event.row - dims.doc_area.y;
            // Reuse `refresh_hovered_link`'s answer for this exact position:
            // the resolver slices and re-parses the block on a hit, and this
            // runs on every pointer report.
            if self.hovered_link.is_some()
                || mouse_ops::hit_test_clickable_non_link(
                    &self.editor,
                    rel_col,
                    rel_row,
                    dims.doc_width,
                    &self.view_state.rendered.table_snapshots,
                )
            {
                PointerShape::Hand
            } else {
                PointerShape::Text
            }
        } else {
            PointerShape::Default
        };
        self.update_pointer_shape(desired);

        // Moved events only drive the pointer-shape tracking above.
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
            // Consumed before the sync below so navigation runs first.
            if let Some(target) = self.editor.pending_link_follow.take() {
                self.follow_link(target, dims.doc_height, dims.doc_width);
            }
            // A column-border drag release sets
            // `pending_column_widths_commit`.
            self.handle_pending_column_widths();
            self.needs_draw = true;
            // The action may have scrolled, edited or navigated, any of which
            // changes the link under a stationary pointer.  Recompute so the
            // hint-line URL doesn't go stale until the next mouse-move.
            self.refresh_hovered_link(&mouse_event, in_doc, dims);
        }
    }

    /// Recompute `hovered_link` for the pointer position, clearing it outside
    /// the doc area.  Sets `needs_draw` only on a hover *change*, so sliding
    /// along one link doesn't repaint every move.
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

    /// Handle a bracketed paste (⌘V, middle-click, Ctrl-Shift-V …), which the
    /// terminal delivers whole as one `Event::Paste`.  Routed straight into the
    /// buffer, so external pastes work whether or not arboard can reach the OS
    /// clipboard from this process.
    ///
    /// Vim re-routes two cases so a paste can't corrupt the document: an open
    /// `/` `?` `:` prompt takes the text instead, and any non-Insert sub-mode
    /// drops it ("Normal mode does not edit" — `p`/`P` paste the register).
    pub(super) fn dispatch_paste(&mut self, text: String, dims: &DocDims) {
        if self.paste_into_cmdline(&text, dims) {
            return;
        }
        if let Some(vim) = self.vim.as_ref() {
            if vim.sub_mode != VimSubMode::Insert {
                return;
            }
        }
        edit_ops::paste_text(&mut self.editor, &text, dims.doc_height, dims.doc_width);
        self.needs_draw = true;
    }

    /// Insert `text` into an open vim `/` `?` `:` prompt, reporting whether
    /// there was one.  Shared by both ways a paste can arrive while the prompt
    /// is up — a bracketed paste and edamame's own paste chord — which landed in
    /// different places before (issue #17).
    ///
    /// Capped at [`PASTE_CHAR_CAP`] chars, the bound every single-line modal
    /// field takes via [`sanitize_paste`](crate::ui::sanitize_paste).  Applied
    /// here rather than in `cmdline::paste_str` because `input` sits below `ui`;
    /// capping rather than sanitizing because a search prompt needs the breaks,
    /// which `paste_str` turns into `\n` escapes.  Unbounded, `paste_str`'s
    /// per-char insert is quadratic — a 200 KB clipboard froze the UI ~9 s.
    fn paste_into_cmdline(&mut self, text: &str, dims: &DocDims) -> bool {
        let Some(vim) = self.vim.as_mut() else {
            return false;
        };
        let Some(cl) = vim.cmdline.as_mut() else {
            return false;
        };
        let capped: String = text.chars().take(PASTE_CHAR_CAP).collect();
        let before = cl.input.clone();
        crate::input::vim::cmdline::paste_str(cl, &capped);
        // A paste changes the line like typing does, so re-derive the live
        // `:s` / incsearch preview.
        crate::input::vim::feed::cmdline_live_update(
            vim,
            &mut self.editor,
            &before,
            dims.doc_height,
            dims.doc_width,
        );
        self.needs_draw = true;
        true
    }

    /// Handle a key event when no modal is open, reading ahead the key presses
    /// already queued so an autorepeat burst coalesces into one buffer mutation
    /// per same-kind run.  Drains the deferred external-editor flow at the end.
    pub(super) fn dispatch_key_event(
        &mut self,
        event: Event,
        dims: &DocDims,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) {
        // Any keypress dismisses the tooltip: the edit it triggers can move the
        // link under a stationary pointer, and nothing else would refresh the
        // hover until the next mouse-move.
        if self.hovered_link.take().is_some() {
            self.needs_draw = true;
        }
        let mut batch: Vec<Event> = vec![event];
        self.collect_key_burst(rx, &mut batch);
        self.dispatch_key_batch(batch, dims, terminal, rx);
    }

    /// Drain key presses from `pending_events` and `rx` into `batch`.  A non-key
    /// terminal event ends the burst and is stashed (preserving channel order)
    /// for the next iteration's normal dispatch; non-`Term` events are handled
    /// inline so the image pipeline doesn't starve behind a typing burst.
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

    /// Process a batch of key-press events.  The first event of any coalesceable
    /// run always goes through the regular per-event path, so one-shot
    /// transitions (list-marker erase, Preview→Rendered, selection-clearing
    /// delete) still fire; the rest collapse into one `apply_insert_run` /
    /// `apply_delete_run` — one buffer mutation, one history entry, one
    /// `parsed_version` bump for the burst.
    ///
    /// A dispatch that opens a modal, sets a pending external-editor flag or
    /// queues a link follow requeues the remaining events so the next loop
    /// iteration routes them through the right dispatcher.
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
            // Dispatch event[i] individually first, so every one-shot
            // transition fires before same-kind events coalesce.
            //
            // `resolve_action` runs the keymap, so a bare Normal-mode vim key
            // resolves to `InsertChar` here even though the vim intercept in
            // `dispatch_single_key` consumes it.  Harmless: the
            // `sub_mode != Insert` guard below `continue`s before a run is
            // built, so the value is discarded.  Don't reorder past that guard.
            let action_i = resolve_action(&events[i], &keymap, &self.editor);
            let coalesce = action_i.as_ref().and_then(Action::coalesce_kind);
            self.dispatch_single_key(events[i].clone(), &keymap, dims);
            i += 1;
            if self.should_quit {
                self.requeue_remaining(&events[i..]);
                break;
            }

            if self.should_break_after_dispatch() {
                self.requeue_remaining(&events[i..]);
                return;
            }
            let Some(kind) = coalesce else { continue };
            if self.editor.selection.is_some()
                || self.drag_target.is_some()
                || self.editor.mode == crate::editor::Mode::Preview
                // The coalesced runs below bypass `dispatch_action`, and so
                // the `search_safe_action` gate inside it — a burst must not
                // extend a run mid-flow.  A navigate search does not capture,
                // so Insert-mode typing during one still coalesces.
                || self.search_flow_captures()
                // Vim outside Insert must not coalesce: a held digit
                // accumulates a count and bare keys are commands.
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
                // Sampled per iteration so a mid-batch selection ends the run.
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

    /// Dispatch one key event through [`App::dispatch_action`].  The
    /// external-editor drain deliberately isn't here — `dispatch_key_batch` runs
    /// it once per batch.
    pub(super) fn dispatch_single_key(&mut self, event: Event, keymap: &KeyMap, dims: &DocDims) {
        // When vim is active it owns the key first, except in two flows that
        // hard-bind keys downstream in `DefaultHandler` (which runs *after* this
        // intercept) and would otherwise be shadowed: diff mode, and a
        // *capturing* search flow — without the deferral vim Normal swallows
        // `Esc` and traps the user.  A navigate-only search does not defer; vim
        // owns `n`/`N` and every other key over the matches.
        let vim_deferred = self.editor.mode == Mode::Diff || self.search_flow_captures();
        // An open vim command line captures every key, so the global keymap
        // never runs while a prompt is up — including the paste chord.  That is
        // why ⌘V (an `Event::Paste`) filled the prompt while edamame's own paste
        // did nothing (issue #17).  Resolve just that action against the live
        // keymap, so a rebound key works too, and route it to the same
        // prompt-paste path; everything else stays captured by `feed_cmdline`.
        if let Event::Key(key) = &event {
            if key.kind == KeyEventKind::Press
                && !vim_deferred
                && self.vim.as_ref().is_some_and(|v| v.cmdline.is_some())
                && keymap.action_for(key) == Some(&Action::Paste)
            {
                let text = edit_ops::clipboard_text(&self.editor);
                self.paste_into_cmdline(&text, dims);
                return;
            }
        }
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
                        // Routed through the ordinary save / quit actions so
                        // the dirty-buffer confirm and save flash behave as
                        // they do for `Ctrl-*`.
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
                        // A named destination saves directly (confirming an
                        // overwrite unless `force`, from a trailing `!`); an
                        // unnamed one opens the Save As modal.
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
                        // Write a copy to the named path, keeping the current
                        // file open, as real vim does.
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
                        // The substitution already ran in the reducer.
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

    /// True when a dispatch left state the rest of the batch must not route
    /// through `dispatch_single_key`: a modal opened, an external-editor flow is
    /// pending, or a link-follow is queued.
    fn should_break_after_dispatch(&self) -> bool {
        !self.modal_stack.is_empty()
            || self.pending_open_file_in_editor
            || self.pending_open_theme_in_editor.is_some()
            || self.pending_open_config_in_editor
            || self.editor.pending_link_follow.is_some()
    }

    /// Push `remaining` onto the front of `pending_events`, in order.
    fn requeue_remaining(&mut self, remaining: &[Event]) {
        for e in remaining.iter().rev() {
            self.pending_events.push_front(e.clone());
        }
    }
}

/// The only terminal event the coalescing path accepts into a key batch.
fn is_key_press(event: &Event) -> bool {
    matches!(event, Event::Key(k) if k.kind == KeyEventKind::Press)
}

/// Resolve `event` to an `Action`, or `None` for non-key / unbound events.
/// Stateless, so the run-detection look-ahead can call it freely.
fn resolve_action(
    event: &Event,
    keymap: &KeyMap,
    editor: &crate::editor::EditorState,
) -> Option<Action> {
    let mut handler = DefaultHandler::new(keymap);
    handler.handle_event(event.clone(), editor)
}

/// Translate `drag_target` into a UI-layer `DropIndicator`, or `None` when no
/// drag paints one.  Column-border resizes return `None` deliberately: the
/// live-preview re-render is itself the affordance.
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
    use crate::ui::text_input::PASTE_CHAR_CAP;

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

        app.dispatch_mouse_event(mouse(MouseEventKind::Moved, 2, 0), &dims);
        assert_eq!(app.hovered_link.as_deref(), Some("https://example.com"));

        // Scroll without moving the pointer: the hover must track the
        // post-scroll state, not the old URL.
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

        app.dispatch_mouse_event(mouse(MouseEventKind::Moved, 2, 10), &dims);
        assert!(app.hovered_link.is_none());
    }

    #[test]
    fn esc_exits_the_search_flow_even_with_vim_active() {
        // Regression: without the search-flow deferral in
        // `dispatch_single_key`, vim Normal swallows `Esc` and traps the user.
        let mut app = app_with_buffer("hello world\n", 0);
        app.set_vim_enabled(true);
        let keymap = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let dims = dims();

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
        // A search started via Ctrl-F / palette must stay Tab-navigable under
        // vim: the flow doesn't capture, so the key reaches `vim_feed`.
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
        assert_eq!(app.editor.buffer.contents(), "foo bar foo baz foo\n");
    }

    #[test]
    fn a_bracketed_paste_cannot_edit_a_read_only_documentation_page() {
        // A terminal paste never becomes an `Action`, so no gate over `Action`
        // can see it; it is refused two layers down, by `enter_edit_if_preview`
        // and `apply_delta`.
        let mut app = app_with_buffer("notes\n", 0);
        app.open_doc_page(crate::docs::DocId::Security, None, 20, 80);
        let before = app.editor.buffer.contents();
        app.dispatch_paste("pasted text".to_owned(), &dims());
        assert_eq!(app.editor.buffer.contents(), before);
        assert!(!app.editor.dirty);
    }

    #[test]
    fn a_bracketed_paste_still_works_in_an_ordinary_document() {
        let mut app = app_with_buffer("hello\n", 0);
        app.dispatch_paste("XYZ".to_owned(), &dims());
        assert!(app.editor.buffer.contents().contains("XYZ"));
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
        // Regression: this used to fall into the buffer and could desync the
        // parsed doc.
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
    fn paste_chord_fills_an_open_vim_command_line() {
        // Regression (issue #17): the prompt captures every key, so the paste
        // chord never fired while ⌘V did fill the prompt.
        use crate::input::vim::state::{CmdLineKind, CmdLineState};
        let mut app = app_with_buffer("hello\n", 0);
        app.set_vim_enabled(true);
        app.editor.kill_ring = "world".to_owned();
        if let Some(vim) = app.vim.as_mut() {
            vim.cmdline = Some(CmdLineState::new(CmdLineKind::Ex));
        }
        let keymap = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let ctrl_v = Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
        app.dispatch_single_key(ctrl_v, &keymap, &dims());

        let cl = app.vim.as_ref().unwrap().cmdline.as_ref().unwrap();
        assert_eq!(cl.input, "world", "the chord fills the prompt");
        assert_eq!(cl.cursor, 5);
        assert_eq!(
            app.editor.buffer.contents(),
            "hello\n",
            "and never reaches the buffer"
        );
    }

    #[test]
    fn paste_chord_into_a_search_prompt_escapes_newlines() {
        // The chord shares `paste_into_cmdline` with the bracketed paste.
        use crate::input::vim::state::{CmdLineKind, CmdLineState};
        let mut app = app_with_buffer("hi\n", 0);
        app.set_vim_enabled(true);
        app.editor.kill_ring = "a\nb".to_owned();
        if let Some(vim) = app.vim.as_mut() {
            vim.cmdline = Some(CmdLineState::new(CmdLineKind::SearchForward));
        }
        let keymap = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let ctrl_v = Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
        app.dispatch_single_key(ctrl_v, &keymap, &dims());

        assert_eq!(
            app.vim.as_ref().unwrap().cmdline.as_ref().unwrap().input,
            r"a\nb"
        );
    }

    #[test]
    fn paste_chord_outside_a_command_line_still_reaches_the_buffer() {
        // The intercept is scoped to an open prompt.
        let mut app = app_with_buffer("hi\n", 0);
        app.set_vim_enabled(true);
        app.editor.mode = crate::editor::Mode::Rendered;
        if let Some(vim) = app.vim.as_mut() {
            vim.sub_mode = crate::input::VimSubMode::Insert;
        }
        app.editor.kill_ring = "X".to_owned();
        let keymap = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let ctrl_v = Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
        app.dispatch_single_key(ctrl_v, &keymap, &dims());
        assert_eq!(app.editor.buffer.contents(), "Xhi\n");
    }

    #[test]
    fn a_command_line_paste_is_capped_at_the_shared_char_limit() {
        // Uncapped, `paste_str`'s per-char insert is quadratic — a large
        // clipboard froze the UI for seconds.
        use crate::input::vim::state::{CmdLineKind, CmdLineState};
        let mut app = app_with_buffer("hi\n", 0);
        app.set_vim_enabled(true);
        if let Some(vim) = app.vim.as_mut() {
            vim.cmdline = Some(CmdLineState::new(CmdLineKind::Ex));
        }
        let huge = "x".repeat(PASTE_CHAR_CAP + 500);
        app.dispatch_paste(huge, &dims());

        let cl = app.vim.as_ref().unwrap().cmdline.as_ref().unwrap();
        assert_eq!(cl.input.chars().count(), PASTE_CHAR_CAP);
        assert_eq!(cl.cursor, PASTE_CHAR_CAP);
    }

    #[test]
    fn the_paste_cap_counts_chars_not_bytes() {
        // The cap is a character count, so multi-byte text can't be truncated
        // mid-codepoint.
        use crate::input::vim::state::{CmdLineKind, CmdLineState};
        let mut app = app_with_buffer("hi\n", 0);
        app.set_vim_enabled(true);
        app.editor.kill_ring = "é".repeat(PASTE_CHAR_CAP + 10);
        if let Some(vim) = app.vim.as_mut() {
            vim.cmdline = Some(CmdLineState::new(CmdLineKind::Ex));
        }
        let keymap = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let ctrl_v = Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
        app.dispatch_single_key(ctrl_v, &keymap, &dims());

        let cl = app.vim.as_ref().unwrap().cmdline.as_ref().unwrap();
        assert_eq!(cl.input.chars().count(), PASTE_CHAR_CAP);
    }

    #[test]
    fn search_command_line_paste_escapes_newlines() {
        use crate::input::vim::state::{CmdLineKind, CmdLineState};
        let mut app = app_with_buffer("hi\n", 0);
        app.set_vim_enabled(true);
        if let Some(vim) = app.vim.as_mut() {
            vim.cmdline = Some(CmdLineState::new(CmdLineKind::SearchForward));
        }
        app.dispatch_paste("a\nb\r\nc".to_owned(), &dims());
        assert_eq!(
            app.vim.as_ref().unwrap().cmdline.as_ref().unwrap().input,
            r"a\nb\r\nc",
            "a multi-line paste becomes a single escaped search line"
        );
    }

    #[test]
    fn ex_command_line_paste_still_strips_newlines() {
        use crate::input::vim::state::{CmdLineKind, CmdLineState};
        let mut app = app_with_buffer("hi\n", 0);
        app.set_vim_enabled(true);
        if let Some(vim) = app.vim.as_mut() {
            vim.cmdline = Some(CmdLineState::new(CmdLineKind::Ex));
        }
        app.dispatch_paste("a\nb\r\nc".to_owned(), &dims());
        assert_eq!(
            app.vim.as_ref().unwrap().cmdline.as_ref().unwrap().input,
            "abc",
            "an ex command is not a search term — no escape syntax"
        );
    }
}
