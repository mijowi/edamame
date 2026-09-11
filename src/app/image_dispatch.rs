//! Image-decode dispatch: the viewport-window filters ([`infos_in_viewport_window`] and its diff
//! counterpart) plus the [`App`] methods that spawn decode workers and turn `ImagesEnabled` /
//! `FiguresEnabled` policy into runtime decisions.

use std::time::{Duration, Instant};

use ratatui::layout::Rect;

use super::{App, AppEvent};

/// Idle window after the last edit before a figure (diagram / `$$...$$` math) re-render is
/// dispatched.  Typing in Rendered mode reparses each keystroke, minting a fresh content-hashed
/// URL, so without this every keystroke would spawn a render (mermaid/RaTeX → SVG → raster) the
/// user never sees mid-burst.  Matches the 120 ms `RAW_REVEAL_DELAY` so it lands about when the
/// reveal settles.
pub(super) const DIAGRAM_RENDER_DEBOUNCE: Duration = Duration::from_millis(120);

/// Prefetch margin in rendered lines above and below the visible area.  Tuned empirically: big
/// enough that a fast scroll finds images decoded, small enough that opening a long image-heavy
/// document doesn't kick off every decode at once.
pub(super) const VIEWPORT_DISPATCH_MARGIN: usize = 80;

/// The image blocks whose rendered rows intersect the near-viewport window.  Pure, so it is
/// unit-testable without a terminal.
///
/// Document order is preserved, so during a scroll the image that enters the window first is
/// dispatched first — a measurable fairness win on slow connections.  Returns whole
/// `ImageBlockInfo`s because the dispatcher branches on `info.source` to spot diagram blocks.
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
            // Half-open intersection on both sides.
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
/// Same intersection and margin, but a block's range is in *diff* visual-line indices (its
/// `ContextRendered` positions), because diff-mode `scroll` counts diff rows.  An image in a
/// changed region has no such entry and is never dispatched — it shows as raw source, which also
/// keeps the remote-image prompt honest during a review.
///
/// Reads the row index through `DiffState::with_layout_index`, the memo the snapshot builder uses,
/// so dispatch and placement agree and neither rebuilds the map per frame.
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
    // Bail before the row index, whose first build scans every rendered context row in the
    // review.  This runs once per event-loop iteration, and on the common image-free document
    // nothing would ever read the map back.
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
    /// Below 24-bit color every pixel collapses into the 256-color cube and reads as broken rather
    /// than degraded (the same reasoning as `app::theme_fallback`).  Gating here rather than
    /// rewriting `config.images.enabled` keeps it a *session* fact, so a persisted `Always`
    /// survives in `config.toml` and applies again on a truecolor terminal.
    pub(super) fn media_renderable(&self) -> bool {
        self.capabilities.full_color()
    }

    /// Whether inline image rendering should happen right now.  `Ask` defers to
    /// `session_images_enabled`, which is `None` until the prompt is answered — false while it is
    /// pending, so nothing decodes behind the user's back.
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

    /// [`Self::effective_images_enabled`] for diagram blocks; decoupled so the two prompts are
    /// answered independently.
    pub(super) fn effective_diagrams_enabled(&self) -> bool {
        if !self.media_renderable() {
            return false;
        }
        match self.config.figures.enabled {
            crate::config::FiguresEnabled::Always => true,
            crate::config::FiguresEnabled::Never => false,
            crate::config::FiguresEnabled::Ask => self.session_diagrams_enabled.unwrap_or(false),
        }
    }

    /// Whether image blocks still reserve layout rows even when no decode will run.  `false` only
    /// on an explicit decline; `Ask` while pending stays `true` so the layout doesn't reflow with
    /// the modal on screen.
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
        match self.config.figures.enabled {
            crate::config::FiguresEnabled::Never => false,
            crate::config::FiguresEnabled::Always => true,
            crate::config::FiguresEnabled::Ask => self.session_diagrams_enabled != Some(false),
        }
    }

    /// React to a settings-overlay change of `config.images.enabled`, doing what the startup
    /// prompts would have done under the new value.  The persisted choice supersedes any earlier
    /// session-level answer, so `session_images_enabled` is reset unconditionally.
    pub(super) fn apply_images_setting_change(&mut self) {
        self.session_images_enabled = None;
        let layout_on = self.images_layout_enabled();
        if self.editor.images_enabled != layout_on {
            self.editor.images_enabled = layout_on;
            self.editor.refresh_parsed();
        }
        // Any queued prompt reflects the pre-change value; rebuild below.
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
                // Remote first, images on top — the startup stack's order, so answering "Yes" to
                // the images prompt reveals the remote one beneath it.
                self.queue_remote_image_prompt();
                self.queue_images_enabled_prompt();
            }
            crate::config::ImagesEnabled::Never => {}
        }
        self.images_dirty = true;
        self.needs_draw = true;
    }

    /// Reacts to a settings-overlay change of `config.figures.enabled`.  Counterpart of
    /// [`Self::apply_images_setting_change`] for figure blocks; deliberately independent, mirroring
    /// the two separate startup prompts.  Resets `session_diagrams_enabled` since the persisted
    /// choice supersedes any earlier session-level answer.
    pub(super) fn apply_diagrams_setting_change(&mut self) {
        self.session_diagrams_enabled = None;
        let layout_on = self.diagrams_layout_enabled();
        if self.editor.diagrams_enabled != layout_on {
            self.editor.diagrams_enabled = layout_on;
            self.editor.refresh_parsed();
        }
        // Any queued prompt reflects the pre-change value; rebuild below.
        self.modal_stack
            .remove_first::<super::modal::FiguresEnabledPromptModal>();
        match self.config.figures.enabled {
            crate::config::FiguresEnabled::Always => self.dispatch_image_decodes(),
            crate::config::FiguresEnabled::Ask => self.queue_diagrams_enabled_prompt(),
            crate::config::FiguresEnabled::Never => {}
        }
        self.images_dirty = true;
        self.needs_draw = true;
    }

    /// React to a settings-overlay change of `config.images.remote_policy`.  Cached remote decodes
    /// are evicted so every URL re-resolves under the new policy.
    pub(super) fn apply_remote_policy_change(&mut self) {
        // The persisted choice supersedes an earlier session answer, a "Yes" *or* a "No".
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

    /// **Every path that swaps the document's contents owes this call** — link follow / nav
    /// ([`App::load_file_into_editor`]), accepted external change, resolved diff review.  It marks
    /// `images_dirty` and re-evaluates the three media prompts against the new document.
    ///
    /// The prompts are built from the *document* (policy is `Ask` **and** this document has an
    /// image / diagram / remote URL), so a document arriving mid-session needs the evaluation
    /// `App::new` gives the startup one.  Without it, launching on an image-free file left
    /// `session_images_enabled` at `None` for the whole run and a later document's images silently
    /// never decoded (issue #30).
    ///
    /// Nothing is dispatched here; the per-frame dispatch picks the URLs up once an answer
    /// permits.  Push order mirrors `App::new`: remote at the bottom, images on top.
    pub(super) fn on_document_contents_swapped(&mut self) {
        self.images_dirty = true;
        self.queue_remote_image_prompt();
        self.queue_diagrams_enabled_prompt();
        self.queue_images_enabled_prompt();
    }

    /// Queue the images-enabled prompt if terminal, config and document warrant one.  A session
    /// answer is never re-asked and carries across documents; idempotent against the stack.
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

    /// [`Self::queue_images_enabled_prompt`] for diagram blocks, gated on its own session answer.
    fn queue_diagrams_enabled_prompt(&mut self) {
        if !self.media_renderable()
            || self.session_diagrams_enabled.is_some()
            || self
                .modal_stack
                .contains::<super::modal::FiguresEnabledPromptModal>()
        {
            return;
        }
        if let Some(m) =
            super::modal::FiguresEnabledPromptModal::from_state(&self.editor, &self.config)
        {
            self.modal_stack.push(Box::new(m));
        }
    }

    /// Queue the remote-image prompt when policy is `Ask`, the document has a remote image, and
    /// this session has neither allowed nor declined already.
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

    /// Result-arrival recheck, since workers capture settings at spawn time: a slow remote fetch
    /// can land after the user flipped remote images to `Never`.  The event loop accepts an
    /// `ImageReady(Ok)` only when the URL is still `Pending` (else it was evicted, or this is a
    /// duplicate), its class is still enabled, and a remote URL is still permitted.
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

    /// Dispatch every image in the document, viewport regardless.  Called after a prompt unlocks
    /// a batch of previously-blocked decodes, where eagerness matches the user's intent.  Safe to
    /// repeat: `ImageCache::request` rejects URLs it already tracks.
    pub(super) fn dispatch_image_decodes(&mut self) {
        let infos: Vec<crate::document::ImageBlockInfo> = self.editor.parsed.image_blocks.clone();
        self.dispatch_image_decodes_for(&infos);
    }

    /// Viewport-limited decode dispatch, called before each frame so scrolling introduces
    /// near-viewport decodes without ever decoding a far-off-screen image.  `doc_height` is the
    /// document area's rendered-line height; `scroll` is the top visible rendered line.
    pub(super) fn dispatch_visible_image_decodes(&mut self, scroll: usize, doc_height: usize) {
        // Debounce diagram / math render dispatch across a typing burst:
        // arm the hold on any buffer-version change (every edit path bumps
        // it), and while the window is open skip diagram-sourced blocks so
        // a keystroke's throwaway content-hashed URL isn't rendered.  Plain
        // images are never held — their URL is stable source text, not a
        // per-keystroke hash.  The first pass only records the version, so
        // opening a document isn't mistaken for an edit.
        let now = Instant::now();
        let version = self.editor.buffer.version();
        if self
            .diagram_render_watch_version
            .is_some_and(|prev| prev != version)
        {
            self.diagram_render_hold_until = Some(now + DIAGRAM_RENDER_DEBOUNCE);
        }
        self.diagram_render_watch_version = Some(version);
        let holding = self.diagram_render_hold_until.is_some_and(|t| now < t);
        if !holding {
            self.diagram_render_hold_until = None;
        }

        let mut infos = infos_in_viewport_window(
            &self.editor.parsed.image_blocks,
            &self.editor.parsed.source_map,
            scroll,
            doc_height,
            VIEWPORT_DISPATCH_MARGIN,
        );
        // A document that gains its first image *mid-session* — a pasted
        // screenshot, or freshly typed `![](…)` — never passed the
        // on-load prompt, so `session_images_enabled` stays unset and
        // `effective_images_enabled` is false: the image would stay a raw
        // source line forever.  Ask now.  The prompt is idempotent and
        // only queues while the gate is still open.
        if infos.iter().any(|info| info.source.is_none()) {
            self.queue_images_enabled_prompt();
        }
        if holding {
            infos.retain(|info| info.source.is_none());
        }
        self.dispatch_image_decodes_for(&infos);
    }

    /// [`Self::dispatch_visible_image_decodes`] for a diff review, over the new-side parse.
    /// Without it an unchanged image not yet decoded when the review opened would reserve
    /// `image_max_height` blank rows for the whole review and never decode, since nothing else
    /// would request it.
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

    /// Shared dispatch primitive: one worker thread per image block whose URL
    /// `ImageCache::request` accepts as new.  `info.source` routes diagrams through
    /// `diagram::resolve_mermaid` and everything else through `image::resolve`; both land in the
    /// same `AppEvent::ImageReady` pipeline.
    ///
    /// Each worker body runs under `catch_unwind` so a panic in the decoder or the mermaid
    /// renderer (v0.2.1 has known panic bugs) still produces exactly one `ImageReady(Err)` rather
    /// than stranding the cache entry as `Pending` forever.
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
        // The worker pre-resizes to `max_cells × font_size` pixels, so the main thread's protocol
        // never resizes and every render at the same target area is a no-op past the first encode.
        let max_cells = Some((
            self.config.images.max_width as u16,
            self.config.images.max_height as u16,
        ));
        let font_size = self
            .capabilities
            .image_picker
            .as_ref()
            // ratatui-image 11 returns a `FontSize`; internally we carry a `(width, height)`.
            .map(|p| {
                let fs = p.font_size();
                (fs.width, fs.height)
            });
        // Picker + observed area width let the worker build the scratch buffer off the UI thread.
        // Missing either (no image support, or the first iteration before a term size is known)
        // skips it and `get_protocol_pair` falls back to a sync encode on that cold path.
        let scratch_picker = self.capabilities.halfblocks_picker.clone();
        let scratch_width = if self.last_area_width > 0 {
            Some(self.last_area_width)
        } else {
            None
        };

        // Glyph colour for display math: the theme's text colour, so
        // formulas stay legible in the active theme (light or dark) when
        // composited transparently over the document background.
        // Falls back to a mid-grey when the theme's text colour is the
        // terminal default (`Reset`), whose RGB we cannot know.
        let latex_fg = crate::ui::dim::color_to_rgb(self.editor.theme().palette.text)
            .map(|[r, g, b]| [r, g, b, 255])
            .unwrap_or([0xcc, 0xcc, 0xcc, 255]);
        // Document background for the same reason the halfblocks fallback
        // needs an opaque image: transparent formula pixels must encode
        // as the document background, never as the black `to_rgb8()`
        // produces from `Rgba([0,0,0,0])`.
        let latex_bg = crate::ui::dim::color_to_rgb(self.editor.theme().palette.bg)
            .map(|[r, g, b]| [r, g, b, 255])
            .unwrap_or([0x1a, 0x1a, 0x1a, 255]);

        for info in infos {
            // Skip a declined class, so "yes images, no diagrams" triggers no mermaid renders.
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
                let url_for_panic = url.clone();
                // `ExpectedPanic` marks this thread for the process panic hook, which would
                // otherwise restore the terminal and print through it for a panic we are about to
                // catch.  Scoped to the `catch_unwind` alone, so a panic in the event assembly
                // below still reaches the hook.  It is a counter, not a flag, because
                // `resolve_mermaid` nests a `catch_unwind` of its own inside this one.
                let result: Result<crate::image::LoadedImage, (String, String)> = {
                    let _expected = crate::terminal::ExpectedPanic::new();
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &source {
                        Some(crate::diagram::DiagramSource::Mermaid(src)) => {
                            crate::diagram::resolve_mermaid(url.clone(), src, max_cells, font_size)
                                .map_err(|e| (url.clone(), e.to_string()))
                        }
                        Some(crate::diagram::DiagramSource::Latex(src)) => {
                            crate::diagram::resolve_latex(
                                url.clone(),
                                src,
                                max_cells,
                                font_size,
                                latex_fg,
                                latex_bg,
                            )
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
                        // Build the halfblocks scratch here so the UI thread's first paint is a
                        // cache hit.  It gets its own `catch_unwind` because it runs *after* the
                        // result exists: an escaping panic would kill the worker with the image in
                        // hand and no event sent, pinning the cache entry `Pending` forever.  A
                        // scratch is only an optimization, so on panic we log and send the image
                        // without one.
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

    /// A `(image_blocks, source_map)` pair where each image produces one rendered line at a known
    /// position, block indices incrementing in order.
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
        // Sentinel slots take their own index so unrelated rows don't collapse onto block 0.
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
        // Worker dispatched, then the policy tightens mid-flight and evicts the Pending entry.
        assert!(app.editor.images.request("https://example.com/a.png"));
        app.config.images.remote_policy = crate::config::RemoteImagePolicy::Never;
        app.apply_remote_policy_change();
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
        // Worker A dispatched, evicted mid-flight, then worker B dispatched.
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
        // A's stale failure must be dropped: `request` never retries a `Failed` entry, so
        // overwriting the Ready decode would pin the image as broken.
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
        app.config.images.enabled = crate::config::ImagesEnabled::Never;
        assert!(
            !app.image_result_still_wanted("img.png"),
            "class disabled mid-flight"
        );
        // Back on, but an entry that is no longer Pending is never wanted.
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

    /// A review with the rendered new-side parse installed, as `refresh_diff_parse` installs it.
    fn diff_review(old: &str, new: &str) -> crate::diff::DiffState {
        let theme: &'static crate::config::Theme =
            Box::leak(Box::new(crate::config::Theme::default()));
        let mut diff = crate::diff::DiffState::new(old, new).expect("non-empty diff");
        diff.set_rendered_parse(Some(crate::document::ParsedDoc::build(new, theme, true, 4)));
        diff
    }

    /// An image-free review returns before the full-document row-index scan.
    #[test]
    fn diff_viewport_window_is_empty_without_images() {
        let diff = diff_review("Alpha.\n\nbee\n", "Alpha.\n\nBEE\n");
        assert!(infos_in_diff_viewport_window(&diff, 40, 0, 20, 0).is_empty());
    }

    /// An image in a *clean* region is dispatched only when its rows fall in the window.
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

        assert!(infos_in_diff_viewport_window(&diff, 40, 500, 20, 0).is_empty());
    }

    /// An image in a *changed* region has no `ContextRendered` row, so it stays raw source.
    #[test]
    fn diff_viewport_window_skips_a_changed_image() {
        let old = "Intro.\n\n![cat](cat.png)\n\nTail.\n";
        let new = "Intro.\n\n![cat](other.png)\n\nTail.\n";
        let diff = diff_review(old, new);
        assert!(infos_in_diff_viewport_window(&diff, 40, 0, 20, 0).is_empty());
    }

    /// No parse installed — every review's first frame — means nothing to dispatch.
    #[test]
    fn diff_viewport_window_is_empty_without_a_rendered_parse() {
        let diff = crate::diff::DiffState::new(
            "Intro.\n\n![cat](cat.png)\n\nbee\n",
            "Intro.\n\n![cat](cat.png)\n\nBEE\n",
        )
        .expect("non-empty diff");
        assert!(infos_in_diff_viewport_window(&diff, 40, 0, 20, 0).is_empty());
    }

    /// Typing inside a `$$...$$` block mints a fresh content-hashed URL on
    /// every keystroke (Rendered mode reparses per key for cursor
    /// visibility).  The render dispatch must be debounced: the initial
    /// render fires immediately, an edit's new URL is *held* during the
    /// typing burst, and it dispatches only once the window elapses.
    #[test]
    fn diagram_render_dispatch_is_debounced_while_typing() {
        let mut app = crate::app::test_utils::app_with_buffer("$$\nx^2\n$$\n", 0);
        app.config.figures.enabled = crate::config::FiguresEnabled::Always;
        app.editor.diagrams_enabled = true;
        app.editor.mode = crate::editor::Mode::Rendered;
        app.editor.math_preview = true;
        app.editor.refresh_parsed();
        // Dispatch spawns workers only with a live event channel.
        let (tx, _rx) = std::sync::mpsc::channel();
        app.app_tx = Some(tx);

        let latex_url = |app: &crate::app::App| {
            app.editor
                .parsed
                .image_blocks
                .iter()
                .find(|i| matches!(i.source, Some(crate::diagram::DiagramSource::Latex(_))))
                .expect("latex block")
                .url
                .clone()
        };
        let url0 = latex_url(&app);

        // First pass records the version (not an edit) → initial render
        // dispatched immediately, no hold.
        app.dispatch_visible_image_decodes(0, 40);
        assert!(
            app.editor.images.status(&url0).is_some(),
            "initial render dispatched"
        );
        assert!(app.diagram_render_hold_until.is_none());

        // Type inside the formula: the reparse mints a new URL.
        app.editor.cursor.offset = "$$\n".chars().count() + 1;
        crate::editor::edit_ops::apply(
            &mut app.editor,
            crate::config::Action::InsertChar('y'),
            24,
            80,
        );
        let url1 = latex_url(&app);
        assert_ne!(url0, url1, "edit changed the formula URL");

        // Dispatch during the burst: the edit armed the hold, so the new
        // URL is not requested.
        app.dispatch_visible_image_decodes(0, 40);
        assert!(
            app.diagram_render_hold_until.is_some(),
            "edit armed the hold"
        );
        assert!(
            app.editor.images.status(&url1).is_none(),
            "new formula render held during typing"
        );

        // Window elapses (simulate) → the deferred render dispatches and
        // the hold clears.
        app.diagram_render_hold_until =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
        app.dispatch_visible_image_decodes(0, 40);
        assert!(
            app.editor.images.status(&url1).is_some(),
            "render dispatched after the debounce window"
        );
        assert!(
            app.diagram_render_hold_until.is_none(),
            "hold cleared after firing"
        );
    }
}
