//! Image loading and caching: the document-layer half of image rendering (URL → decoded
//! `DynamicImage`). The UI half (decoded image → terminal cells) lives in `ui::image_view`.

pub mod cache;
pub mod kitty_direct;
pub mod loader;
pub mod render;
pub mod svg;

// The band protocols `paint_images` uses for Kitty and Sixel.  Re-exported so the decode worker
// (which builds them) and the editor view (which paints them) can name them without each reaching
// into `ratatui_image` directly.
pub use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};

// `DecodeStatus` is used by integration tests in tests/editing.rs.
#[allow(unused_imports)]
pub use cache::DecodeStatus;
pub use cache::{
    aspect_rows_of, build_direct_placement, build_sliced, render_halfblocks_scratch,
    DirectPlacement, ImageCache, NativePaint,
};
pub use kitty_direct::Geometry;
pub use loader::{resolve, LoadedImage};
pub use render::paint_halfblocks_partial;
pub use svg::{rasterize_svg, SvgError, SvgScaleMode, SvgSizing};
