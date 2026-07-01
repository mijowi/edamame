//! Vim operator / resolution layer — the editor-side half of the
//! two-layer split (mirrors `mouse_ops`).
//!
//! `input::vim::vim_feed` decides *what* the user asked for; this module
//! resolves offsets against the buffer and mutates `EditorState`.  CP2
//! added `motion` (the core motions); CP3 added `operator` (the `d`/`c`/`y`
//! application) and `edits` (`p`/`P` paste), plus the count-aware
//! `resolve_motion_range` operator entry point.  CP4 extends `edits` with
//! the remaining Normal primitives (`r{c}`, `~`, `J`, `>>`/`<<`).  CP6 adds
//! `visual` (the shared VisualLine line-expansion helper used by the render
//! path, the Visual operators, and the system clipboard copy/cut) plus the
//! Visual range edits in `edits` (`u`/`U` force-case, `r{c}`, `p` paste-over).
//! CP7 adds `text_object` (the `iw`/`aw`/quote/bracket-pair objects used by
//! `d`/`c`/`y` and Visual).  CP8 added `search` (the `*`/`#` word scan).  CP9
//! adds `ex` (the `:w`/`:q`/`:wq`/`:s`/`:%s` parser + the regex substitution —
//! the only use of the `fancy-regex` crate).  CP10 extends `edits` with the
//! markdown-aware list wiring (`open_list_continue` for `o`/`O`,
//! `renumber_list_at_cursor` after `dd`, `indent_list_item` for `>>`/`<<`),
//! which reuse the byte-oriented `list_edit` primitives.  See
//! `docs/vim-implementation-plan.md` §2.1.

pub mod edits;
pub mod ex;
pub mod motion;
pub mod operator;
pub mod preview;
pub mod search;
pub mod text_object;
pub mod vim_regex;
pub mod visual;

pub use edits::{
    indent_lines, indent_list_item, join_lines, open_list_continue, paste, renumber_list_at_cursor,
    replace_char, replace_char_range, replace_range_with, set_case_range, toggle_case,
    toggle_case_range,
};
pub use ex::{execute_substitute, parse_ex, ExCommand};
pub use motion::{
    doubled_line_range, first_non_blank, line_end_offset, resolve_find_repeat, resolve_motion,
    resolve_motion_range, vertical_line_range, FindKind, Motion, OpRange,
};
pub use operator::{execute_operator, OpResult, Operator};
pub use preview::{clear_substitute_preview, update_substitute_preview, SubstitutePreview};
pub use search::word_under_cursor_at;
pub use text_object::{resolve_text_object_range, TextObject};
pub use visual::{visual_line_bounds, visual_line_char_range};
