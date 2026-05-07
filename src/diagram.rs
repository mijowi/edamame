//! Phase 17 — diagram rendering.
//!
//! Turns fenced ```mermaid``` code blocks into images that flow through
//! the Phase 7 image pipeline (AST `Block::ImageBlock` → decode worker →
//! URL-keyed `ImageCache` → per-frame overlay).  Mermaid diagrams get a
//! synthetic URL of the form `diagram-mermaid-<sha256(source)>` so the
//! cache reuses renders across reparses while keystrokes inside a block
//! produce a new hash → a fresh render.
//!
//! The actual renderer lives in `mermaid` — this file is the facade.
//! `DiagramSource` is the enum carried on `ImageBlockInfo.source`; the
//! App's decode dispatcher branches on it to pick the right worker.

pub mod mermaid;

// `render_mermaid_svg` is consumed via `crate::diagram::` from `src/export/`,
// but rustc misreports it as unused on this re-export. See similar note in
// `src/config.rs`.
#[allow(unused_imports)]
pub use mermaid::{render_mermaid_svg, resolve_mermaid, synthetic_url, warm_fontdb, DiagramSource};
