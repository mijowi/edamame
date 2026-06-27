//! Document export.
//!
//! HTML is the single built-in export target and doubles as the intermediate
//! format for user-configured custom exports (PDF, DOCX, …) that pipe the
//! generated HTML through an external tool such as `weasyprint` or `pandoc`.
//!
//! The module is deliberately UI-agnostic: every public function is callable
//! from the command palette without that code needing to
//! understand the export pipeline.  Long-running work (rendering or shelling
//! out to a converter) always runs on a background thread and reports
//! completion through a caller-supplied `FnOnce` closure.

// The whole custom-export pipeline is public library API exercised by the
// lib tests, but the binary never invokes it — scope its bin-only dead-code
// allow here instead of over the entire `export` module.
#[allow(dead_code)]
pub mod custom;
pub mod html;
pub mod runner;

// The custom-pipeline re-exports and `render_html` are public API used by the
// library and its tests; the binary imports a different subset, so these read
// as unused in the bin build alone.
#[allow(unused_imports)]
pub use custom::{spawn_custom_export, CustomExportError};
#[allow(unused_imports)]
pub use html::{render_html, spawn_html_export, HtmlExportOptions, Stylesheet};
pub use runner::{preflight, target_for_source, ExportOutcome, PreflightError};
