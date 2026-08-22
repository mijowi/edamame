//! Image-decode dispatch orchestration extracted from `app.rs` in
//! Step 2 of `refactor-app.md`.
//!
//! Owns:
//! - Pure helper [`infos_in_viewport_window`] for filtering image
//!   blocks against the prefetch window.
//! - The [`App`] methods that spawn decode workers and translate
//!   `ImagesEnabled` policy into runtime decisions.

use ratatui::layout::Rect;

use super::{App, AppEvent};

/// One-viewport-height prefetch margin (in rendered lines) above and
/// below the visible area.  An image whose rendered rows intersect
/// `[scroll - MARGIN, scroll + doc_height + MARGIN]` will have its
/// decode dispatched.  Tuned empirically: large enough that a
/// fast-scrolling user sees decoded images by the time they reach the
/// viewport, small enough that opening a long image-heavy document
/// doesn't immediately kick off every decode at once.
pub(super) const VIEWPORT_DISPATCH_MARGIN: usize = 80;

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
/// apart from regular images.
pub(super) fn infos_in_viewport_window(
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

/// Diff-mode counterpart of [`infos_in_viewport_window`].
///
/// Same half-open intersection and the same margin, but a block's window
/// range is its span in *diff* visual-line indices — the positions of its
/// `ContextRendered` entries — because in diff mode `scroll` counts diff
/// rows, not the new-side parse's rendered rows.  An image in a changed
/// region has no such entry and is never dispatched: it shows as raw
/// source, which is also what keeps the remote-image prompt honest during
/// a review.
///
/// Reads the rendered-row index through `DiffState::with_layout_index`,
/// the same memo the snapshot builder uses, so dispatch and placement
/// reason about the same rows and neither rebuilds the map per frame.
pub(super) fn infos_in_diff_viewport_window(
    diff: &crate::diff::DiffState,
    width: usize,
    scroll: usize,
    doc_height: usize,
    margin: usize,
) -> Vec<crate::document::ImageBlockInfo> {
    let Some(parsed) = diff.parsed_new.as_ref() else {
        return Vec::new();
    };
    // Bail before the rendered-row index, which is a full scan of — and
    // a `HashMap` sized by — every rendered context row in the review.
    // The map is memoised per layout version, but the *first* request
    // still has to build it, and this runs from `prepare_viewport`, i.e.
    // once per event-loop iteration for the whole length of the review.
    // On an image-free document (the common one) nothing would ever read
    // it back.  The non-diff `infos_in_viewport_window` is O(images) and
    // never pays it.
    if parsed.image_blocks.is_empty() {
        return Vec::new();
    }
    let window_start = scroll.saturating_sub(margin);
    let window_end = scroll.saturating_add(doc_height).saturating_add(margin);
    diff.with_layout_index(width, |_lines, _rc, index| {
        parsed
            .image_blocks
            .iter()
            .filter_map(|info| {
                let range = parsed.source_map.rendered_lines_for_block(info.block_idx);
                if range.is_empty() {
                    return None;
                }
                let first = *index.get(&range.start)?;
                let last = *index.get(&(range.end - 1))?;
                (first < window_end && last + 1 > window_start).then(|| info.clone())
            })
            .collect()
    })
}

impl App {
    /// Whether this terminal can render decoded pixels at all.
    ///
    /// Below 24-bit color every decoded pixel collapses into the
    /// 256-color cube, which reads as broken rather than degraded — the
    /// same reasoning that forces the indexed-color theme substitution
    /// (`app::theme_fallback`) and that the welcome modal applies to its
    /// images / diagrams rows.  Gating here rather than by rewriting
    /// `config.images.enabled` keeps it a *session* fact: a persisted
    /// `Always`, chosen on the user's truecolor terminal, survives
    /// untouched in `config.toml` and takes effect again the moment they
    /// go back to it.
    pub(super) fn media_renderable(&self) -> bool {
        self.capabilities.full_color()
    }

    /// Whether inline image rendering should happen right now.  The
    /// persisted `config.images.enabled` decides when it's `Always` or
    /// `Never`; `Ask` defers to `session_images_enabled`, which is
    /// populated only after the user answers the images-enabled
    /// prompt.  While the prompt is still pending this returns false
    /// so no decodes are dispatched behind the user's back.
    pub(super) fn effective_images_enabled(&self) -> bool {
        if !self.media_renderable() {
            return false;
        }
        match self.config.images.enabled {
            crate::config::ImagesEnabled::Always => true,
            crate::config::ImagesEnabled::Never => false,
            crate::config::ImagesEnabled::Ask => self.session_images_enabled.unwrap_or(false),
        }
    }

    /// Counterpart to [`Self::effective_images_enabled`] for diagram
    /// blocks (mermaid, etc.).  Decoupled from the image flag so a user
    /// can answer the two prompts independently.
    pub(super) fn effective_diagrams_enabled(&self) -> bool {
        if !self.media_renderable() {
            return false;
        }
        match self.config.diagrams.enabled {
            crate::config::DiagramsEnabled::Always => true,
            crate::config::DiagramsEnabled::Never => false,
            crate::config::DiagramsEnabled::Ask => self.session_diagrams_enabled.unwrap_or(false),
        }
    }

    /// Whether image blocks should still reserve layout rows, even if
    /// no decode will run.  Returns `false` only when the user has
    /// explicitly declined — persisted `Never` or a session-level `No`
    /// / Escape on the images-enabled prompt.  The `Ask` + pending
    /// state still reports `true` so the layout doesn't reflow while
    /// the modal is on screen.
    pub(super) fn images_layout_enabled(&self) -> bool {
        if !self.media_renderable() {
            return false;
        }
        match self.config.images.enabled {
            crate::config::ImagesEnabled::Never => false,
            crate::config::ImagesEnabled::Always => true,
            crate::config::ImagesEnabled::Ask => self.session_images_enabled != Some(false),
        }
    }

    /// Counterpart to [`Self::images_layout_enabled`] for diagram blocks.
    pub(super) fn diagrams_layout_enabled(&self) -> bool {
        if !self.media_renderable() {
            return false;
        }
        match self.config.diagrams.enabled {
            crate::config::DiagramsEnabled::Never => false,
            crate::config::DiagramsEnabled::Always => true,
            crate::config::DiagramsEnabled::Ask => self.session_diagrams_enabled != Some(false),
        }
    }

    /// React to a settings-overlay change of `config.images.enabled`.
    /// Called only when the value actually changed (the overlay emits
    /// `FieldChanged` solely on a real transition).  Mirrors what the
    /// startup prompts would have done under the new value:
    ///
    /// * `Always` — reserve image rows again and dispatch decodes.
    /// * `Ask` — reserve rows and queue the images-enabled prompt (it
    ///   surfaces once the settings overlay closes, matching the
    ///   startup queue order).
    /// * `Never` — collapse image blocks to their placeholders and
    ///   drop any queued prompts.
    ///
    /// The persisted choice supersedes any earlier session-level
    /// answer, so `session_images_enabled` is reset unconditionally.
    pub(super) fn apply_images_setting_change(&mut self) {
        self.session_images_enabled = None;
        let layout_on = self.images_layout_enabled();
        if self.editor.images_enabled != layout_on {
            self.editor.images_enabled = layout_on;
            self.editor.refresh_parsed();
        }
        // Any queued prompt reflects the pre-change value; rebuild from
        // scratch below.
        self.modal_stack
            .remove_first::<super::modal::ImagesEnabledPromptModal>();
        self.modal_stack
            .remove_first::<super::modal::RemoteImagePromptModal>();
        match self.config.images.enabled {
            crate::config::ImagesEnabled::Always => {
                self.queue_remote_image_prompt();
                self.dispatch_image_decodes();
            }
            crate::config::ImagesEnabled::Ask => {
                // Remote first, images on top — same relative order as
                // the startup stack, so answering "Yes" to the images
                // prompt reveals the remote prompt beneath it.
                self.queue_remote_image_prompt();
                self.queue_images_enabled_prompt();
            }
            crate::config::ImagesEnabled::Never => {}
        }
        self.images_dirty = true;
        self.needs_draw = true;
    }

    /// React to a settings-overlay change of `config.diagrams.enabled`.
    /// Called only when the value actually changed.  Counterpart of
    /// [`Self::apply_images_setting_change`] for diagram blocks —
    /// deliberately independent, mirroring the two startup prompts:
    ///
    /// * `Always` — reserve diagram rows again and dispatch renders.
    /// * `Ask` — reserve rows and queue the diagrams-enabled prompt.
    /// * `Never` — collapse diagram blocks to their placeholders and
    ///   drop any queued prompt.
    ///
    /// The persisted choice supersedes any earlier session-level
    /// answer, so `session_diagrams_enabled` is reset unconditionally.
    pub(super) fn apply_diagrams_setting_change(&mut self) {
        self.session_diagrams_enabled = None;
        let layout_on = self.diagrams_layout_enabled();
        if self.editor.diagrams_enabled != layout_on {
            self.editor.diagrams_enabled = layout_on;
            self.editor.refresh_parsed();
        }
        // Any queued prompt reflects the pre-change value; rebuild below.
        self.modal_stack
            .remove_first::<super::modal::DiagramsEnabledPromptModal>();
        match self.config.diagrams.enabled {
            crate::config::DiagramsEnabled::Always => self.dispatch_image_decodes(),
            crate::config::DiagramsEnabled::Ask => self.queue_diagrams_enabled_prompt(),
            crate::config::DiagramsEnabled::Never => {}
        }
        self.images_dirty = true;
        self.needs_draw = true;
    }

    /// React to a settings-overlay change of
    /// `config.images.remote_policy`.  Called only when the value
    /// actually changed.  Cached remote decodes are evicted so every
    /// remote URL re-resolves under the new policy: `Always`
    /// re-dispatches (refetch), `Ask` queues the remote-image prompt,
    /// `Never` lets the per-frame dispatch re-request and fail each
    /// remote URL into its blocked placeholder.
    pub(super) fn apply_remote_policy_change(&mut self) {
        // The persisted choice supersedes an earlier session-level
        // answer on the remote prompt — a "Yes" *or* a "No".
        self.session_allow_remote = false;
        self.session_remote_declined = false;
        self.editor.images.evict_remote();
        self.modal_stack
            .remove_first::<super::modal::RemoteImagePromptModal>();
        match self.config.images.remote_policy {
            crate::config::RemoteImagePolicy::Always => self.dispatch_image_decodes(),
            crate::config::RemoteImagePolicy::Ask => self.queue_remote_image_prompt(),
            crate::config::RemoteImagePolicy::Never => {}
        }
        self.images_dirty = true;
        self.needs_draw = true;
    }

    /// Single owner of the bookkeeping every *document-contents swap*
    /// owes, whatever swapped them: a link follow or back/forward
    /// navigation ([`App::load_file_into_editor`]), an accepted
    /// external change (`App::reload_buffer_from_disk`), and a resolved
    /// diff review (`App::apply_diff_resolution`).  Two things:
    ///
    /// * `images_dirty` — the new contents reference a different set of
    ///   image URLs, so the cache needs reconciling on the next loop
    ///   iteration.
    /// * the three media prompts, re-evaluated against the new document
    ///   (see [`Self::queue_images_enabled_prompt`] and its siblings).
    ///
    /// It exists as one method rather than three copies because the
    /// copies are what went wrong: the prompts are built from the
    /// *document* — the policy is `Ask` **and** this document actually
    /// contains an image / a diagram / a remote URL — so a document
    /// arriving mid-session needs the same evaluation `App::new` gives
    /// the startup one.  Without it, launching on a file with no images
    /// left `session_images_enabled` at `None` for the rest of the run:
    /// no prompt was ever queued for the later document, so
    /// `effective_images_enabled` stayed false and its images silently
    /// never decoded (issue #30).  A new path that replaces the
    /// document's contents owes this call.
    ///
    /// Nothing is dispatched here: the per-frame
    /// `dispatch_visible_image_decodes` picks up the new document's
    /// URLs as soon as an answer permits it.
    ///
    /// Push order mirrors `App::new` — remote at the bottom, images on
    /// top — so answering the top prompt reveals the next one.
    pub(super) fn on_document_contents_swapped(&mut self) {
        self.images_dirty = true;
        self.queue_remote_image_prompt();
        self.queue_diagrams_enabled_prompt();
        self.queue_images_enabled_prompt();
    }

    /// Queue the images-enabled prompt if this terminal, the config and
    /// the current document warrant one.
    ///
    /// A session answer is never re-asked: `Some(_)` on
    /// `session_images_enabled` means the user has already decided for
    /// this run, and that decision carries across documents exactly as
    /// it does while one document stays open.  Idempotent — a prompt
    /// still waiting on the stack is not stacked twice.
    fn queue_images_enabled_prompt(&mut self) {
        if !self.media_renderable()
            || self.session_images_enabled.is_some()
            || self
                .modal_stack
                .contains::<super::modal::ImagesEnabledPromptModal>()
        {
            return;
        }
        if let Some(m) =
            super::modal::ImagesEnabledPromptModal::from_state(&self.editor, &self.config)
        {
            self.modal_stack.push(Box::new(m));
        }
    }

    /// Counterpart of [`Self::queue_images_enabled_prompt`] for diagram
    /// blocks, gated on `session_diagrams_enabled` — the two prompts are
    /// answered independently.
    fn queue_diagrams_enabled_prompt(&mut self) {
        if !self.media_renderable()
            || self.session_diagrams_enabled.is_some()
            || self
                .modal_stack
                .contains::<super::modal::DiagramsEnabledPromptModal>()
        {
            return;
        }
        if let Some(m) =
            super::modal::DiagramsEnabledPromptModal::from_state(&self.editor, &self.config)
        {
            self.modal_stack.push(Box::new(m));
        }
    }

    /// Queue the remote-image prompt if the current document and config
    /// warrant one (policy `Ask`, at least one remote image, remote not
    /// already allowed *or declined* for this session, no prompt already
    /// queued).
    fn queue_remote_image_prompt(&mut self) {
        if !self.media_renderable()
            || self.session_allow_remote
            || self.session_remote_declined
            || self
                .modal_stack
                .contains::<super::modal::RemoteImagePromptModal>()
        {
            return;
        }
        if let Some(m) =
            super::modal::RemoteImagePromptModal::from_state(&self.editor, &self.config)
        {
            self.modal_stack.push(Box::new(m));
        }
    }

    /// Result-arrival recheck: decode workers capture the image / remote
    /// settings at spawn time, so a settings change (or a cache eviction)
    /// mid-decode can deliver a result the *current* settings forbid —
    /// e.g. a slow remote fetch landing after the user flipped remote
    /// images to `Never`.  The event loop accepts an `ImageReady(Ok)`
    /// result only when this returns true: the URL must still be tracked
    /// as `Pending` (anything else means it was evicted, or the result is
    /// a duplicate), its class (image / diagram) must still be enabled,
    /// and a remote URL must still be permitted to load.
    pub(super) fn image_result_still_wanted(&self, url: &str) -> bool {
        if !matches!(
            self.editor.images.status(url),
            Some(crate::image::DecodeStatus::Pending)
        ) {
            return false;
        }
        if crate::diagram::is_diagram_url(url) {
            return self.effective_diagrams_enabled();
        }
        if !self.effective_images_enabled() {
            return false;
        }
        if crate::image::loader::is_remote(url) {
            return match self.config.images.remote_policy {
                crate::config::RemoteImagePolicy::Always => true,
                crate::config::RemoteImagePolicy::Ask => self.session_allow_remote,
                crate::config::RemoteImagePolicy::Never => false,
            };
        }
        true
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
    pub(super) fn dispatch_image_decodes(&mut self) {
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
    pub(super) fn dispatch_visible_image_decodes(&mut self, scroll: usize, doc_height: usize) {
        let infos = infos_in_viewport_window(
            &self.editor.parsed.image_blocks,
            &self.editor.parsed.source_map,
            scroll,
            doc_height,
            VIEWPORT_DISPATCH_MARGIN,
        );
        self.dispatch_image_decodes_for(&infos);
    }

    /// Viewport-limited decode dispatch for a diff review, over the
    /// new-side parse's images.  Without it, an unchanged image that had
    /// not been decoded when the review opened (below the fold, or a
    /// review entered soon after launch) would reserve `image_max_height`
    /// blank rows for the whole review and never decode —
    /// `ImageCache::reserved_rows` returns `None` while a URL is unknown,
    /// and nothing else would ever request it.
    pub(super) fn dispatch_visible_diff_image_decodes(&mut self, scroll: usize, doc_height: usize) {
        let Some(diff) = self.editor.diff.as_ref() else {
            return;
        };
        let infos = infos_in_diff_viewport_window(
            diff,
            self.last_doc_width.max(1),
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
    pub(super) fn dispatch_image_decodes_for(&mut self, infos: &[crate::document::ImageBlockInfo]) {
        let images_on = self.effective_images_enabled();
        let diagrams_on = self.effective_diagrams_enabled();
        if !images_on && !diagrams_on {
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
            // ratatui-image 11 returns a `FontSize` struct; we carry font
            // size as a `(width, height)` tuple internally.
            .map(|p| {
                let fs = p.font_size();
                (fs.width, fs.height)
            });
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
            // Route each block to its respective enabled flag.  Skip
            // blocks whose class is currently declined so a user who
            // said "yes images, no diagrams" doesn't trigger mermaid
            // renders behind their back.
            let is_diagram = info.source.is_some();
            if is_diagram && !diagrams_on {
                continue;
            }
            if !is_diagram && !images_on {
                continue;
            }
            if !self.editor.images.request(&info.url) {
                continue;
            }
            tracing::debug!(
                target: "image", url = %info.url, is_diagram,
                remote = crate::image::loader::is_remote(&info.url),
                %session_allow_remote,
                "decode dispatched",
            );
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
                // Marks this worker thread for the process panic hook,
                // which would otherwise restore the terminal (and print
                // through it) for a panic we are about to catch.  Scoped
                // to the `catch_unwind` alone, so a panic in the event
                // assembly below still reaches the hook.  `resolve_mermaid`
                // guards a `catch_unwind` of its own inside this one,
                // which is why the guard is a counter and not a flag.
                let result: Result<crate::image::LoadedImage, (String, String)> = {
                    let _expected = crate::terminal::ExpectedPanic::new();
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
                }
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
                        //
                        // Inside its own `catch_unwind` for the reason
                        // the decode has one, and then some: this runs
                        // *after* the result exists, so a panic here
                        // kills the worker with the image in hand and no
                        // event ever sent — the cache entry stays
                        // `Pending` forever, which paints as a permanent
                        // placeholder under full reserved rows with
                        // nothing logged.  A scratch is an optimization;
                        // losing it costs one sync encode on the UI
                        // thread, so on a panic we log and send the
                        // image without one rather than dropping it.
                        if let (Some(picker), Some(width), Some((mw, mh)), Some(fs)) =
                            (&scratch_picker, scratch_width, max_cells, font_size)
                        {
                            let scratch = {
                                let _expected = crate::terminal::ExpectedPanic::new();
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    let rows =
                                        crate::image::aspect_rows_of(&loaded.image, mw, mh, fs)
                                            as u16;
                                    if width == 0 || rows == 0 {
                                        return None;
                                    }
                                    let rect = Rect::new(0, 0, width, rows);
                                    let buf = crate::image::render_halfblocks_scratch(
                                        picker,
                                        loaded.image.clone(),
                                        rect,
                                    );
                                    Some((rect, buf))
                                }))
                            };
                            match scratch {
                                Ok(s) => loaded.scratch = s,
                                Err(_) => tracing::warn!(
                                    target: "image", url = %loaded.url,
                                    "halfblocks scratch render panicked; sending the image without a prebuilt scratch",
                                ),
                            }
                        }
                        AppEvent::ImageReady(Ok(loaded))
                    }
                    Err(err) => AppEvent::ImageReady(Err(err)),
                };
                tracing::debug!(
                    target: "image",
                    ok = matches!(event, AppEvent::ImageReady(Ok(_))),
                    "decode worker finished",
                );
                let _ = tx.send(event);
            });
        }
    }
}

#[cfg(test)]
mod tests {
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
        let max_block = *rendered_to_block.iter().max().unwrap() + 1;
        let ranges = (0..max_block).map(|i| i..i + 1).collect::<Vec<_>>();
        let map = SourceMap::new(rendered_to_block, ranges.clone(), ranges, 0);
        (blocks, map)
    }

    #[test]
    fn in_flight_remote_result_is_discarded_after_policy_tightens() {
        let mut app =
            crate::app::test_utils::app_with_buffer("![a](https://example.com/a.png)\n", 0);
        app.config.images.enabled = crate::config::ImagesEnabled::Always;
        app.config.images.remote_policy = crate::config::RemoteImagePolicy::Always;
        // Worker dispatched: the URL is tracked as Pending.
        assert!(app.editor.images.request("https://example.com/a.png"));
        // The policy tightens while the fetch is in flight (evicts the
        // Pending entry) …
        app.config.images.remote_policy = crate::config::RemoteImagePolicy::Never;
        app.apply_remote_policy_change();
        // … then the worker's decoded result lands.  It must be
        // discarded, not cached as Ready.
        app.handle_async_event(crate::app::AppEvent::ImageReady(Ok(
            crate::image::LoadedImage {
                url: "https://example.com/a.png".into(),
                image: image::DynamicImage::new_rgba8(1, 1),
                scratch: None,
            },
        )));
        assert!(
            app.editor
                .images
                .status("https://example.com/a.png")
                .is_none(),
            "a decoded remote image must not resurface under `Never`"
        );
    }

    #[test]
    fn stale_worker_failure_does_not_overwrite_a_fresh_decode() {
        let mut app = crate::app::test_utils::app_with_buffer("![a](img.png)\n", 0);
        app.config.images.enabled = crate::config::ImagesEnabled::Always;
        // Worker A dispatched, then the entry is evicted mid-flight
        // (e.g. the Ok-arm discard path) and worker B is dispatched.
        assert!(app.editor.images.request("img.png"));
        app.editor.images.forget("img.png");
        assert!(app.editor.images.request("img.png"));
        // Worker B's decode lands first.
        app.handle_async_event(crate::app::AppEvent::ImageReady(Ok(
            crate::image::LoadedImage {
                url: "img.png".into(),
                image: image::DynamicImage::new_rgba8(1, 1),
                scratch: None,
            },
        )));
        assert!(matches!(
            app.editor.images.status("img.png"),
            Some(crate::image::DecodeStatus::Ready(_))
        ));
        // Worker A's stale failure lands second: it must be dropped —
        // `request` never retries a `Failed` entry, so overwriting the
        // Ready decode would pin the image as broken.
        app.handle_async_event(crate::app::AppEvent::ImageReady(Err((
            "img.png".into(),
            "stale worker error".into(),
        ))));
        assert!(
            matches!(
                app.editor.images.status("img.png"),
                Some(crate::image::DecodeStatus::Ready(_))
            ),
            "a stale failure must not overwrite a fresh decode"
        );
    }

    #[test]
    fn image_result_still_wanted_rechecks_class_and_policy() {
        let mut app = crate::app::test_utils::app_with_buffer("![a](img.png)\n", 0);
        app.config.images.enabled = crate::config::ImagesEnabled::Always;
        assert!(app.editor.images.request("img.png"));
        assert!(app.image_result_still_wanted("img.png"));
        // Images flipped off mid-decode: the local result is unwanted too.
        app.config.images.enabled = crate::config::ImagesEnabled::Never;
        assert!(
            !app.image_result_still_wanted("img.png"),
            "class disabled mid-flight"
        );
        // Back on — but an entry that is no longer Pending (evicted /
        // already resolved) is never wanted.
        app.config.images.enabled = crate::config::ImagesEnabled::Always;
        app.editor.images.forget("img.png");
        assert!(!app.image_result_still_wanted("img.png"));
    }

    #[test]
    fn viewport_window_keeps_images_inside_visible_rows() {
        let (blocks, map) = fixture(&[("a.png", 5), ("b.png", 50), ("c.png", 200)]);
        let urls: Vec<String> = infos_in_viewport_window(&blocks, &map, 0, 20, 0)
            .into_iter()
            .map(|i| i.url)
            .collect();
        assert_eq!(urls, vec!["a.png".to_owned()]);
    }

    #[test]
    fn viewport_window_keeps_images_inside_prefetch_margin() {
        let (blocks, map) = fixture(&[("a.png", 5), ("b.png", 50), ("c.png", 200)]);
        let urls: Vec<String> = infos_in_viewport_window(&blocks, &map, 0, 20, 40)
            .into_iter()
            .map(|i| i.url)
            .collect();
        assert_eq!(urls, vec!["a.png".to_owned(), "b.png".to_owned()]);
    }

    #[test]
    fn viewport_window_respects_scroll_offset() {
        let (blocks, map) = fixture(&[("a.png", 5), ("b.png", 50), ("c.png", 200)]);
        let urls: Vec<String> = infos_in_viewport_window(&blocks, &map, 180, 20, 10)
            .into_iter()
            .map(|i| i.url)
            .collect();
        assert_eq!(urls, vec!["c.png".to_owned()]);
    }

    #[test]
    fn viewport_window_preserves_document_order() {
        let (blocks, map) = fixture(&[("c.png", 2), ("a.png", 0), ("b.png", 1)]);
        let urls: Vec<String> = infos_in_viewport_window(&blocks, &map, 0, 10, 0)
            .into_iter()
            .map(|i| i.url)
            .collect();
        assert_eq!(
            urls,
            vec!["c.png".to_owned(), "a.png".to_owned(), "b.png".to_owned()]
        );
    }

    #[test]
    fn viewport_window_empty_when_all_images_above() {
        let (blocks, map) = fixture(&[("a.png", 0), ("b.png", 5)]);
        let urls = infos_in_viewport_window(&blocks, &map, 100, 20, 10);
        assert!(urls.is_empty());
    }

    #[test]
    fn viewport_window_handles_saturating_scroll_underflow() {
        let (blocks, map) = fixture(&[("a.png", 0), ("b.png", 5)]);
        let urls: Vec<String> = infos_in_viewport_window(&blocks, &map, 2, 3, 100)
            .into_iter()
            .map(|i| i.url)
            .collect();
        assert_eq!(urls, vec!["a.png".to_owned(), "b.png".to_owned()]);
    }

    // ── Diff-mode dispatch window ────────────────────────────────────

    /// A review of `old` → `new` with the rendered new-side parse
    /// installed, as `EditorState::refresh_diff_parse` installs it.
    fn diff_review(old: &str, new: &str) -> crate::diff::DiffState {
        let theme: &'static crate::config::Theme =
            Box::leak(Box::new(crate::config::Theme::default()));
        let mut diff = crate::diff::DiffState::new(old, new).expect("non-empty diff");
        diff.set_rendered_parse(Some(crate::document::ParsedDoc::build(new, theme, true, 4)));
        diff
    }

    /// An image-free review returns early, before the full-document
    /// `rendered_row_index` scan this runs at the frame cadence.
    #[test]
    fn diff_viewport_window_is_empty_without_images() {
        let diff = diff_review("Alpha.\n\nbee\n", "Alpha.\n\nBEE\n");
        assert!(infos_in_diff_viewport_window(&diff, 40, 0, 20, 0).is_empty());
    }

    /// An image in a *clean* region is dispatched when its rows fall in
    /// the window, and skipped when they don't.
    #[test]
    fn diff_viewport_window_keeps_clean_images_inside_visible_rows() {
        let old = "Intro.\n\n![cat](cat.png)\n\nbee\n";
        let new = "Intro.\n\n![cat](cat.png)\n\nBEE\n";
        let diff = diff_review(old, new);

        let urls: Vec<String> = infos_in_diff_viewport_window(&diff, 40, 0, 20, 0)
            .into_iter()
            .map(|i| i.url)
            .collect();
        assert_eq!(urls, vec!["cat.png".to_owned()]);

        // Scrolled far past it, with no prefetch margin, it drops out.
        assert!(infos_in_diff_viewport_window(&diff, 40, 500, 20, 0).is_empty());
    }

    /// An image inside a *changed* region has no `ContextRendered` row,
    /// so it is never dispatched — it shows as `![alt](url)` source.
    #[test]
    fn diff_viewport_window_skips_a_changed_image() {
        let old = "Intro.\n\n![cat](cat.png)\n\nTail.\n";
        let new = "Intro.\n\n![cat](other.png)\n\nTail.\n";
        let diff = diff_review(old, new);
        assert!(infos_in_diff_viewport_window(&diff, 40, 0, 20, 0).is_empty());
    }

    /// No parse installed — the state every review passes through on
    /// its first frame → nothing to dispatch; the whole review is raw.
    #[test]
    fn diff_viewport_window_is_empty_without_a_rendered_parse() {
        let diff = crate::diff::DiffState::new(
            "Intro.\n\n![cat](cat.png)\n\nbee\n",
            "Intro.\n\n![cat](cat.png)\n\nBEE\n",
        )
        .expect("non-empty diff");
        assert!(infos_in_diff_viewport_window(&diff, 40, 0, 20, 0).is_empty());
    }
}
