pub mod ast;
pub mod parse_offsets;
pub mod parser;
pub mod renderer;
pub mod table_layout;

pub use ast::{inlines_to_plain, Block, Inline};
pub use parser::{
    attach_trailing_tui_columns_comments, parse, parse_raw, promote_diagram_code_blocks,
    promote_html_comments, promote_image_paragraphs,
};
pub use renderer::{ImageRowOverride, Renderer};
