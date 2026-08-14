// Library entry point — exposes modules for integration tests and future
// embedding. The binary in main.rs is the primary deliverable, and it
// consumes *this* crate rather than re-declaring the module tree: a
// second `mod app;` in main.rs would compile a private duplicate of
// every module, so `app`'s unit tests would run only under
// `cargo test --bin edamame` and be invisible to `cargo test --lib`.
// Every module the binary needs must therefore be declared here.

// The doc comments in this crate are written for contributors, not for
// downstream API consumers: they routinely link private helpers next to
// the public entry points that call them, because the *why* usually lives
// in the private half.  Build the docs with `--document-private-items`
// (see AGENTS.md) and those links resolve to real pages.
//
// `broken_intra_doc_links` and `invalid_html_tags` are deliberately left
// at their default `warn` — a link that names a since-renamed item is doc
// rot worth hearing about.
#![allow(rustdoc::private_intra_doc_links)]

pub mod constants;

pub mod app;
pub mod cli;
pub mod config;
pub mod diagram;
pub mod diff;
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

/// Shared test-only helpers.  Crate-wide rather than per-module because
/// environment mutation races across module boundaries — see the module
/// docs.
#[cfg(test)]
pub mod test_env;
