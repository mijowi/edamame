// Library entry point — exposes modules for integration tests and future
// embedding. The binary in main.rs is the primary deliverable, and it
// consumes *this* crate rather than re-declaring the module tree: a
// second `mod app;` in main.rs would compile a private duplicate of
// every module, so `app`'s unit tests would run only under
// `cargo test --bin edamame` and be invisible to `cargo test --lib`.
// Every module the binary needs must therefore be declared here.

pub mod constants;

pub mod app;
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
