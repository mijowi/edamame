//! Vim modal input — the state-machine half of the two-layer split.
//!
//! `vim_feed` (here) is the reducer; `editor::vim_ops` is the
//! apply/resolution layer.  This mirrors `MouseDispatcher` →
//! `mouse_ops::apply`.  See `docs/vim-implementation-plan.md`.

pub mod feed;
pub mod state;

pub use feed::{vim_feed, VimOutcome};
pub use state::{VimState, VimSubMode};
