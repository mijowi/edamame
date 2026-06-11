//! Block-level render memoization for the parse → render pipeline.
//!
//! `ParsedDoc::build` re-renders every top-level block on every reparse,
//! even though a typical edit changes exactly one block.  [`RenderCache`]
//! memoizes each block's rendered lines keyed by the block's AST value, so
//! an unchanged block costs a clone of its cached lines instead of a full
//! re-render (inline styling, word measurement, table column layout).
//! See docs/perf-benchmark-plan.md — rendering dominated the pipeline for
//! table-heavy and mixed documents before memoization.
//!
//! Keying by the `Block` value (not the source byte slice) is what makes
//! the cache safe against everything that changes rendering without
//! changing source text: live table-width drag overrides, post-pass
//! promotions, and list splitting all mutate the AST, so a mutated block
//! simply misses the cache and re-renders.
//!
//! `Block::ImageBlock` is never cached: its rendered row count depends on
//! the image decode cache (via the renderer's row override), which changes
//! out-of-band as decodes complete.  Image blocks are cheap placeholder
//! fills, so re-rendering them is free anyway.

use std::collections::HashMap;

use ratatui::text::Line;

use super::ast::Block;

/// Fingerprint of every `Renderer` input that affects a block's rendered
/// lines besides the block itself.  When any of these change between
/// builds the whole cache is cleared.  The theme is identified by address
/// — themes are `&'static` and the editor already treats pointer identity
/// as theme identity (`EditorState::set_theme` short-circuits on
/// `ptr::eq`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenderSettings {
    pub theme_addr: usize,
    pub viewport_width: usize,
    pub code_wrap: bool,
    pub image_max_height: usize,
    pub row_striping: bool,
    pub big_h1: bool,
}

/// Memoized rendered lines per top-level block, owned by `EditorState`
/// and threaded into `ParsedDoc::build_with_overrides` on every reparse.
///
/// Eviction is by document membership: each build moves the entries it
/// hits into a fresh map and drops the old map afterwards, so blocks no
/// longer present in the document are released immediately — the same
/// keep-what's-live policy as the image cache GC.
#[derive(Debug, Default)]
pub struct RenderCache {
    pub(super) settings: Option<RenderSettings>,
    pub(super) entries: HashMap<Block, Vec<Line<'static>>>,
}

impl RenderCache {
    /// Reset to the given settings, clearing all entries when they differ
    /// from the previous build's.  Returns the previous entry map; the
    /// caller (the renderer) moves hits out of it into the fresh
    /// [`entries`](Self::entries) map and lets the remainder drop.
    pub(super) fn begin_build(
        &mut self,
        settings: RenderSettings,
    ) -> HashMap<Block, Vec<Line<'static>>> {
        if self.settings.as_ref() != Some(&settings) {
            self.entries.clear();
            self.settings = Some(settings);
        }
        std::mem::take(&mut self.entries)
    }
}
