//! Search-and-replace subsystem.
//!
//! Mirrors the `diff` module's shape: a session-state object
//! ([`SearchState`], owned by `EditorState::search` while a search flow
//! is active) plus a hard-bound key table ([`search_keys`]) that wins
//! over the user keymap for the duration of the flow.
//!
//! Unlike diff mode, an active search does **not** change
//! `EditorState::mode` — the document keeps rendering in whatever view
//! mode it was in (Preview / Rendered / Raw) with match highlights
//! painted on top.  The flow is gated on `search.is_some()` instead:
//! the input handler intercepts the flow keys, and
//! `app::actions::search_safe_action` default-denies every other
//! action.

pub mod search_keys;
pub mod state;

pub use search_keys::{search_action_for, search_hint};
pub use state::SearchState;
