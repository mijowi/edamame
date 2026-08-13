//! Command-line front end — everything edamame can do *without* starting
//! the TUI.
//!
//! The flag surface is deliberately tiny (`--help`, `--version`,
//! `--doctor`, `--no-config`, `--log`) and hand-parsed rather than routed
//! through `clap`: the dependency graph is kept narrow on purpose
//! (see `Cargo.toml`'s note on `mermaid-rs-renderer`'s `cli` feature,
//! which is disabled for exactly this reason), and a few dozen lines of
//! matching cost less than a derive macro plus four crates.
//!
//! `main` is a dispatcher over [`Invocation`]: parse first, then either
//! print and exit or hand a [`RunOpts`] to the normal startup path.  All
//! of it lives in the library crate — not in `main.rs` — so the parser's
//! unit tests are reachable from `cargo test --lib` (see `lib.rs`).

pub mod args;
pub mod doctor;
pub mod help;

pub use args::{CliError, Invocation, RunOpts};
pub use doctor::run as run_doctor;
pub use help::{help_text, version_line, USAGE};
