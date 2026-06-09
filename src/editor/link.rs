//! Link target classification for clickable-link navigation.
//!
//! `LinkTarget` divides the raw `url` string on an `Inline::Link` into one
//! of three categories so the App can pick the right dispatch path:
//!   * `Anchor(slug)` — `#heading` fragment, resolved against the current
//!     document's heading table.
//!   * `Url(string)`  — any absolute URL with an RFC-3986 scheme
//!     (including `mailto:`), handed off to `open::that` so the OS picks
//!     the handler.
//!   * `LocalFile(path)` — everything else is treated as a filesystem
//!     path, resolved relative to the current document's directory.  The
//!     `.md` extension then triggers in-editor navigation; other
//!     extensions get handed off to `open::that`.
//!
//! This module is deliberately pure — no I/O, no `App` state — so the
//! classification is trivial to test and can be invoked identically from
//! the mouse dispatch path and the keyboard `FollowLinkUnderCursor`
//! handler.

use std::path::{Path, PathBuf};

/// A classified link destination ready for App-level dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// Absolute URL (`https://…`, `mailto:…`, etc.).  Opened via the OS
    /// default handler.
    Url(String),
    /// Local filesystem path, resolved against the document's base
    /// directory when the source URL was relative.  `.md` extensions
    /// are loaded into the editor; others hand off to `open::that`.
    LocalFile(PathBuf),
    /// In-document anchor (the text after `#` in `#heading`).  Empty
    /// fragments (`url = "#"`) are still classified as `Anchor("")` —
    /// the caller decides whether to treat them as no-ops.
    Anchor(String),
    /// A footnote reference `[^label]` — follow jumps to the matching
    /// definition.  The inner string is the raw label (`"1"`, `"note"`).
    /// Not produced by [`LinkTarget::parse`]; constructed by the footnote
    /// source scanner.
    Footnote(String),
    /// A footnote definition's back-link — follow returns to the
    /// reference the reader came from (or, if they scrolled here
    /// directly, the footnote's first reference).  The inner string is
    /// the raw label.
    FootnoteBack(String),
}

impl LinkTarget {
    /// Classify `url` against an optional `base_dir` (the directory of
    /// the current document, used to resolve relative paths).
    ///
    /// Rules:
    /// - `"#foo"` → `Anchor("foo")`
    /// - Scheme-prefixed URLs (`http:`, `https:`, `mailto:`, `ftp:`, …)
    ///   → `Url(url.to_owned())`.  A single-character "scheme" (Windows
    ///   drive letters like `C:/path`) is NOT a scheme for this purpose
    ///   so absolute Windows paths still classify as `LocalFile`.
    /// - `file:///abs/path` → `LocalFile(/abs/path)` (the `file:`
    ///   scheme is treated as a local-path hint, mirroring
    ///   `image::loader::resolve_local_path`).
    /// - Anything else → `LocalFile`, resolved relative to `base_dir`
    ///   when the path is relative and `base_dir` is `Some`.
    pub fn parse(url: &str, base_dir: Option<&Path>) -> Self {
        if let Some(fragment) = url.strip_prefix('#') {
            return LinkTarget::Anchor(fragment.to_owned());
        }

        // `file://` is a local-path hint, not a remote URL.
        if let Some(stripped) = url.strip_prefix("file://") {
            return LinkTarget::LocalFile(PathBuf::from(stripped));
        }

        if has_url_scheme(url) {
            return LinkTarget::Url(url.to_owned());
        }

        let path = PathBuf::from(url);
        let resolved = if path.is_absolute() {
            path
        } else if let Some(dir) = base_dir {
            dir.join(path)
        } else {
            path
        };
        LinkTarget::LocalFile(resolved)
    }

    /// True when this target points at a Markdown file that edamame can
    /// open in-editor.  Case-insensitive check on `.md`/`.markdown`.
    /// Used by tests in this module.
    #[allow(dead_code)]
    pub fn is_markdown_file(&self) -> bool {
        match self {
            LinkTarget::LocalFile(path) => path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| {
                    let lower = e.to_ascii_lowercase();
                    lower == "md" || lower == "markdown"
                })
                .unwrap_or(false),
            _ => false,
        }
    }
}

/// True when `url` begins with a multi-character URL scheme
/// (`scheme:rest`).  A single-character prefix (e.g. Windows `C:/…`) is
/// rejected so absolute Windows paths keep their `LocalFile`
/// classification.
fn has_url_scheme(url: &str) -> bool {
    let Some((scheme, _rest)) = url.split_once(':') else {
        return false;
    };
    if scheme.len() < 2 {
        return false;
    }
    let valid_first = scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic());
    let valid_rest = scheme
        .chars()
        .skip(1)
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.');
    valid_first && valid_rest
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn anchor_hash_fragment_classifies_as_anchor() {
        assert_eq!(
            LinkTarget::parse("#heading", None),
            LinkTarget::Anchor("heading".to_owned())
        );
    }

    #[test]
    fn empty_fragment_is_anchor() {
        assert_eq!(
            LinkTarget::parse("#", None),
            LinkTarget::Anchor(String::new())
        );
    }

    #[test]
    fn https_scheme_classifies_as_url() {
        assert_eq!(
            LinkTarget::parse("https://example.com/page", None),
            LinkTarget::Url("https://example.com/page".to_owned())
        );
    }

    #[test]
    fn mailto_scheme_classifies_as_url() {
        assert_eq!(
            LinkTarget::parse("mailto:a@b.c", None),
            LinkTarget::Url("mailto:a@b.c".to_owned())
        );
    }

    #[test]
    fn file_scheme_classifies_as_local_file() {
        assert_eq!(
            LinkTarget::parse("file:///abs/path.md", None),
            LinkTarget::LocalFile(PathBuf::from("/abs/path.md"))
        );
    }

    #[test]
    fn relative_path_resolves_against_base_dir() {
        let base = PathBuf::from("/home/user/docs");
        assert_eq!(
            LinkTarget::parse("./sibling.md", Some(&base)),
            LinkTarget::LocalFile(base.join("./sibling.md"))
        );
        assert_eq!(
            LinkTarget::parse("../other.md", Some(&base)),
            LinkTarget::LocalFile(base.join("../other.md"))
        );
    }

    #[test]
    fn bare_filename_without_base_stays_relative() {
        assert_eq!(
            LinkTarget::parse("foo.md", None),
            LinkTarget::LocalFile(PathBuf::from("foo.md"))
        );
    }

    #[test]
    fn absolute_path_is_not_rejoined_with_base() {
        let base = PathBuf::from("/home/user/docs");
        assert_eq!(
            LinkTarget::parse("/etc/hosts.md", Some(&base)),
            LinkTarget::LocalFile(PathBuf::from("/etc/hosts.md"))
        );
    }

    #[test]
    fn windows_drive_letter_is_not_a_url_scheme() {
        // Single-char "scheme" is a Windows drive letter — classify as a
        // local path, not a URL.
        let classified = LinkTarget::parse("C:/Users/name/doc.md", None);
        assert!(matches!(classified, LinkTarget::LocalFile(_)));
    }

    #[test]
    fn is_markdown_file_matches_md_and_markdown() {
        let md = LinkTarget::LocalFile(PathBuf::from("foo.md"));
        let markdown = LinkTarget::LocalFile(PathBuf::from("bar.Markdown"));
        let other = LinkTarget::LocalFile(PathBuf::from("baz.txt"));
        assert!(md.is_markdown_file());
        assert!(markdown.is_markdown_file());
        assert!(!other.is_markdown_file());
        assert!(!LinkTarget::Anchor("x".into()).is_markdown_file());
        assert!(!LinkTarget::Url("https://x".into()).is_markdown_file());
    }
}
