//! Block-level render memoization for the parse → render pipeline.
//!
//! `ParsedDoc::build` re-renders every top-level block on every reparse,
//! even though a typical edit changes exactly one block.  [`RenderCache`]
//! memoizes each block's rendered lines keyed by the block's AST value, so
//! an unchanged block costs a clone of its cached lines instead of a full
//! re-render (inline styling, word measurement, table column layout).
//! See docs/dev/performance.md — rendering dominated the pipeline for
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
    pub syntax_highlighting: bool,
    /// `highlight::warm_generation()` at build time, or 0 when
    /// highlighting is off.
    ///
    /// Warming a grammar changes a code block's rendered lines without
    /// changing its `Block` value, so nothing else here would invalidate
    /// the plain render memoized while that grammar was still cold — the
    /// block would stay uncoloured for the life of the document.  This is
    /// the same trap the module doc names for a new `Renderer` knob; a
    /// counter is simply the shape it takes when the "setting" is owned
    /// by a background thread.
    pub highlight_generation: u64,
    /// `highlight::retry_epoch()` at build time, or 0 when highlighting
    /// is off.
    ///
    /// The counter above covers a grammar that *finished compiling*; this
    /// one covers a grammar the burst budget turned away, which is the
    /// case it cannot cover.  A retry is granted exactly when no grammar
    /// warmed, so `highlight_generation` has not moved and no `Block`
    /// value has changed — leaving this out means the retry's reparse
    /// hits every cached block, never reaches `render_code_block`, and so
    /// never re-asks the budget for the slot it was just granted.  The
    /// language then stays plain for the rest of the session.
    pub highlight_retry_epoch: u64,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> RenderSettings {
        RenderSettings {
            theme_addr: 0,
            viewport_width: 80,
            code_wrap: false,
            image_max_height: 20,
            row_striping: false,
            big_h1: false,
            syntax_highlighting: true,
            highlight_generation: 0,
            highlight_retry_epoch: 0,
        }
    }

    /// Both highlighting counters have to be part of the fingerprint, and
    /// the retry epoch is the one whose absence is invisible: it moves
    /// exactly when `highlight_generation` does *not*, so a cache that
    /// ignored it would keep serving the plain render of a block whose
    /// grammar was refused for want of budget, and `render_code_block`
    /// would never run again to ask for the refilled slot.
    #[test]
    fn both_highlight_counters_clear_the_cache() {
        for bump in [
            |s: &mut RenderSettings| s.highlight_generation += 1,
            |s: &mut RenderSettings| s.highlight_retry_epoch += 1,
        ] {
            let mut cache = RenderCache::default();
            cache.begin_build(settings());
            cache
                .entries
                .insert(Block::HorizontalRule, vec![Line::from("x")]);

            // Same settings: the entry survives (and is handed back for
            // the renderer to move into the fresh map).
            let prev = cache.begin_build(settings());
            assert_eq!(prev.len(), 1, "an unchanged fingerprint must not clear");
            cache.entries = prev;

            let mut changed = settings();
            bump(&mut changed);
            let prev = cache.begin_build(changed);
            assert!(prev.is_empty(), "a moved counter must clear the cache");
        }
    }
}
