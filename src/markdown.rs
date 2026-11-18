pub mod ast;
pub mod cell_overlay;
pub mod destination;
pub mod footnote_numbers;
pub mod highlight;
pub mod html_comment;
pub mod inline_col_map;
pub mod label;
pub mod parse_offsets;
pub mod parser;
pub mod render_cache;
pub mod renderer;
pub mod row_origin;
pub mod segment_break;
pub mod table_layout;

pub use ast::{heading_slug_text, inlines_to_plain, Block, Inline, LineSpan, SrcLines};
pub use destination::{escape_destination, escape_destination_in_table, local_image_urls};
pub use footnote_numbers::FootnoteNumbers;
pub use inline_col_map::{strip_atx_closing, InlineColMap, RefLabels};
pub use parser::{
    attach_nested_tui_columns_comments, attach_trailing_tui_columns_comments,
    display_math_block_body, parse, parse_document, parse_raw_with_ranges,
    promote_diagram_code_blocks, promote_display_math_paragraphs, promote_html_comments,
    promote_image_paragraphs, reconstruct_broken_display_math, split_display_math_paragraphs,
    DocParse,
};
pub use render_cache::RenderCache;
pub use renderer::{ImageRowOverride, Renderer};
pub use row_origin::{ColOrigin, ContentKind, RowOrigin, RowSink};
