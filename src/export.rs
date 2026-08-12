//! Document export.
//!
//! HTML is the only export target that has shipped.  It is also designed to
//! serve as the intermediate format for user-configured custom exports (PDF,
//! DOCX, …) that pipe the generated HTML through an external tool such as
//! `weasyprint` or `pandoc` — [`custom`] implements and tests that runner,
//! but no UI reaches it yet, so `[[export.custom]]` config is inert.  See
//! [`crate::config::CustomExportEntry`].
//!
//! The module is deliberately UI-agnostic: every public function is callable
//! from the command palette without that code needing to
//! understand the export pipeline.  Long-running work (rendering or shelling
//! out to a converter) always runs on a background thread and reports
//! completion through a caller-supplied `FnOnce` closure.

pub mod custom;
pub mod html;
pub mod runner;

pub use custom::{spawn_custom_export, CustomExportError};
pub use html::{render_html, spawn_html_export, HtmlExportOptions, Stylesheet};
pub use runner::{preflight, target_for_source, ExportOutcome, PreflightError};
