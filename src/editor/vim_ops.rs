//! Vim operator / resolution layer — the editor-side half of the
//! two-layer split (mirrors `mouse_ops`).
//!
//! `input::vim::vim_feed` decides *what* the user asked for; this module
//! resolves offsets against the buffer and mutates `EditorState`.  CP1
//! ships only this skeleton — motion-range, operator, text-object, and
//! search resolution land in later checkpoints
//! (`motion.rs`, `text_object.rs`, `operator.rs`, `edits.rs`,
//! `search.rs`, `ex.rs`).  See `docs/vim-implementation-plan.md` §2.1.
