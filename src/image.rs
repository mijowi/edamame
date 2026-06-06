//! Image loading and caching.
//!
//! This module is the document-layer half of image rendering: resolving
//! URLs to decoded `DynamicImage`s.  The UI-layer half — turning a
//! decoded image into terminal cells — lives in `ui::image_view`, which
//! drives `ratatui-image`'s `StatefulProtocol` using the decoded bytes
//! we produce here.

pub mod cache;
pub mod loader;
pub mod render;

// `DecodeStatus` is used by integration tests in tests/editing.rs.
#[allow(unused_imports)]
pub use cache::DecodeStatus;
pub use cache::{aspect_rows_of, render_halfblocks_scratch, ImageCache};
pub use loader::{resolve, LoadedImage};
pub use render::paint_halfblocks_partial;
