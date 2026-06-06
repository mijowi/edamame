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

impl App {
    /// Whether inline image rendering should happen right now.  The
    /// persisted `config.images.enabled` decides when it's `Always` or
    /// `Never`; `Ask` defers to `session_images_enabled`, which is
    /// populated only after the user answers the images-enabled
    /// prompt.  While the prompt is still pending this returns false
    /// so no decodes are dispatched behind the user's back.
    pub(super) fn effective_images_enabled(&self) -> bool {
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
        match self.config.images.enabled {
            crate::config::ImagesEnabled::Never => false,
            crate::config::ImagesEnabled::Always => true,
            crate::config::ImagesEnabled::Ask => self.session_images_enabled != Some(false),
        }
    }

    /// Counterpart to [`Self::images_layout_enabled`] for diagram blocks.
    pub(super) fn diagrams_layout_enabled(&self) -> bool {
        match self.config.diagrams.enabled {
            crate::config::DiagramsEnabled::Never => false,
            crate::config::DiagramsEnabled::Always => true,
            crate::config::DiagramsEnabled::Ask => self.session_diagrams_enabled != Some(false),
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
}
