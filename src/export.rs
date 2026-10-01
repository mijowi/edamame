//! Document export. HTML is the built-in target and the intermediate format for
//! user-configured custom exports ([`custom`], [`crate::config::CustomExportEntry`]), which
//! pipe it through an external converter. UI-agnostic: long-running work runs on a
//! background thread and reports completion through a caller-supplied `FnOnce`.

pub mod custom;
pub mod html;
pub mod runner;

pub use custom::{spawn_custom_export, CustomExportError};
pub use html::{
    outside_images, render_html, spawn_html_export, HtmlExportOptions, ImageHandling, Stylesheet,
};
pub use runner::{preflight, target_for_source, ExportOutcome, Exported, PreflightError};
