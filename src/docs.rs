//! The user documentation shipped inside the binary.
//!
//! `docs/*.md` in this repository is the reference manual, and this
//! module is how a running edamame reads it back: every page is
//! `include_str!`d at build time and opened as a pathless, read-only
//! in-memory document (see [`crate::app::App::open_doc_page`]).
//! Nothing is ever extracted to disk.
//!
//! **Embedded rather than extracted, deliberately.**  Writing the
//! pages into a cache directory would buy the existing on-disk
//! navigation path for free, at the cost of a directory to create,
//! invalidate and garbage-collect, a `--no-config` story, a failure
//! mode on a read-only filesystem, and the genuinely confusing
//! semantics of a user editing a page whose edits vanish on the next
//! upgrade.  Compiling the text in has none of that, and buys one
//! thing extraction cannot: the manual can never drift from the build
//! it documents.  `src/app/post_upgrade/changelog.rs` embeds
//! `CHANGELOG.md` on the same reasoning.
//!
//! **This module parses nothing.**  It owns static strings and the
//! slug metadata that names them, which is why it sits with the leaf
//! subsystems (`image`, `diagram`, `export`) rather than anywhere near
//! `markdown` or `document`.  Turning a page's text into a `Buffer` is
//! `app`'s job and `app`'s alone.

pub mod link;
pub mod registry;

pub use link::{resolve_doc_reference, DocLinkResolution};
pub use registry::{DocId, DocPage, ALL_DOCS};
