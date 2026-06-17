//! Vim operator / resolution layer — the editor-side half of the
//! two-layer split (mirrors `mouse_ops`).
//!
//! `input::vim::vim_feed` decides *what* the user asked for; this module
//! resolves offsets against the buffer and mutates `EditorState`.  CP2
//! adds `motion` (the core motions); operator-range, text-object,
//! single-key edits, search, and ex resolution land in later checkpoints
//! (`text_object.rs`, `operator.rs`, `edits.rs`, `search.rs`, `ex.rs`).
//! See `docs/vim-implementation-plan.md` §2.1.

pub mod motion;

pub use motion::{first_non_blank, resolve_motion, Motion};
