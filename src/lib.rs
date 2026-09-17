// Library entry point.  `main.rs` consumes this crate rather than re-declaring the module
// tree: a second `mod app;` there would compile a private duplicate of every module, hiding
// its unit tests from `cargo test --lib`.  Every module the binary needs is declared here.

// Doc comments here are written for contributors and routinely link private helpers, so build
// with `--document-private-items` (see AGENTS.md).  `broken_intra_doc_links` and
// `invalid_html_tags` stay at `warn` — a link naming a renamed item is doc rot worth hearing.
#![allow(rustdoc::private_intra_doc_links)]

pub mod constants;

pub mod app;
pub mod cli;
pub mod clipboard;
pub mod config;
pub mod diagram;
pub mod diff;
pub mod docs;
pub mod document;
pub mod editor;
pub mod export;
pub mod image;
pub mod input;
pub mod markdown;
pub mod search;
pub mod terminal;
pub mod ui;
pub mod watcher;

/// Shared test-only helpers.  Crate-wide rather than per-module because environment mutation
/// races across module boundaries.
#[cfg(test)]
pub mod test_env;
