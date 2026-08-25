//! Document export.
//!
//! HTML is the built-in target, and it doubles as the intermediate format
//! for user-configured custom exports (PDF, DOCX, …), which pipe the
//! generated HTML through an external tool such as `weasyprint` or
//! `pandoc` — see [`custom`] and [`crate::config::CustomExportEntry`].
//! Both reach the user through one command-palette family and one modal
//! (`crate::app::modal::export`): a custom export is an HTML export with
//! a converter on the end, so it is offered the same options.
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
