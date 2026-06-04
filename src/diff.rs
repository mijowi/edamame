//! Diff-mode subsystem.  Exposes:
//!
//! - [`engine`] — pure line + word diff over two strings; returns
//!   [`hunk::Hunk`] sequences with stable ids.
//! - [`state::DiffState`] — owned by `EditorState::diff` while
//!   `Mode::Diff` is active; carries the hunk list, per-hunk
//!   decisions, focused id, working new-side buffer, and a per-diff
//!   undo stack reserved for in-diff text edits.
//! - [`hunk`] — `Hunk`, `HunkKind`, `Decision`, `InlineSpan`, etc.
//! - [`layout`] — the flat stacked visual-line model + a cached
//!   per-width row-count table the renderer and scroll math share.

pub mod engine;
pub mod hunk;
pub mod layout;
pub mod state;

#[allow(unused_imports)]
pub use engine::{compute, HunkIdAllocator};
#[allow(unused_imports)]
pub use hunk::{Decision, Hunk, HunkId, HunkKind, InlineSide, InlineSpan};
#[allow(unused_imports)]
pub use layout::{DiffLineSource, DiffVisualLine};
pub use state::DiffState;
