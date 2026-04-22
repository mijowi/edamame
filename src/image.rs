//! Image loading and caching for Phase 7.
//!
//! This module is the document-layer half of image rendering: resolving
//! URLs to decoded `DynamicImage`s.  The UI-layer half — turning a
//! decoded image into terminal cells — lives in `ui::image_view`, which
//! drives `ratatui-image`'s `StatefulProtocol` using the decoded bytes
//! we produce here.

pub mod cache;
pub mod loader;
pub mod render;

pub use cache::{
    aspect_rows_of, render_halfblocks_scratch, DecodeStatus, ImageCache, ProtocolPair,
};
pub use loader::{resolve, ImageLoadError, LoadedImage};
pub use render::paint_halfblocks_partial;
