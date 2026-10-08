pub mod ast;
pub mod destination;
pub mod highlight;
pub mod inline_col_map;
pub mod parse_offsets;
pub mod parser;
pub mod render_cache;
pub mod renderer;
pub mod row_origin;
pub mod table_layout;

pub use ast::{inlines_to_plain, Block, Inline, LineSpan, SrcLines};
pub use destination::{escape_destination, escape_destination_in_table, local_image_urls};
pub use inline_col_map::{strip_atx_closing, InlineColMap, RefLabels};
pub use parser::{
    attach_nested_tui_columns_comments, parse, parse_document, parse_raw_with_ranges,
    promote_diagram_code_blocks, promote_display_math_paragraphs, promote_html_comments,
    promote_image_paragraphs, reconstruct_broken_display_math, split_display_math_paragraphs,
    DocParse,
};
pub use render_cache::RenderCache;
pub use renderer::{ImageRowOverride, Renderer};
pub use row_origin::{ColOrigin, ContentKind, RowOrigin, RowSink};
