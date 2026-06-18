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
//! Text-object, search, and
//! ex resolution land in later checkpoints (`text_object.rs`, `search.rs`,
//! `ex.rs`).  See `docs/vim-implementation-plan.md` §2.1.

pub mod edits;
pub mod motion;
pub mod operator;
pub mod visual;

pub use edits::{
    indent_lines, join_lines, paste, replace_char, replace_char_range, replace_range_with,
    set_case_range, toggle_case, toggle_case_range,
};
pub use motion::{
    doubled_line_range, first_non_blank, resolve_find_repeat, resolve_motion, resolve_motion_range,
    vertical_line_range, FindKind, Motion, OpRange,
};
pub use operator::{execute_operator, OpResult, Operator};
pub use visual::{visual_line_bounds, visual_line_char_range};
