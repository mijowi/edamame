//! Link target classification for clickable-link navigation.
//!
//! `LinkTarget` divides the raw `url` string on an `Inline::Link` into one
//! of three categories so the App can pick the right dispatch path:
//!   * `Anchor(slug)` — `#heading` fragment, resolved against the current
//!     document's heading table.
//!   * `Url(string)`  — any absolute URL with an RFC-3986 scheme
//!     (including `mailto:`), handed off to `open::that` so the OS picks
//!     the handler.
//!   * `LocalFile { path, fragment }` — everything else is treated as a
//!     filesystem path, resolved relative to the current document's
//!     directory, with any trailing `#fragment` split off.  The `.md`
//!     extension then triggers in-editor navigation (scrolling to the
//!     fragment's heading once the file is loaded — a deep link); other
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
    ///
    /// `fragment` carries the `#section` part of `other.md#section`
    /// when the link had one — a *deep link*.  It is split off the
    /// path before resolution, because it is not part of any file
    /// name: leaving it attached is what made the OS handler receive
    /// `docs/editing.md#some-heading` and fail (issue #38).  `None`
    /// when the link named no fragment, and also when it named an
    /// empty one (`other.md#`), which has no heading to resolve.
    LocalFile {
        path: PathBuf,
        fragment: Option<String>,
    },
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
    /// - `file:///abs/path` → `LocalFile { path: /abs/path, .. }` (the
    ///   `file:` scheme is treated as a local-path hint, mirroring
    ///   `image::loader::resolve_local_path`).
    /// - Anything else → `LocalFile`, resolved relative to `base_dir`
    ///   when the path is relative and `base_dir` is `Some`.
    /// - A trailing `#fragment` on either of the two local forms is
    ///   split off into `LocalFile::fragment` before the path is
    ///   resolved.
    pub fn parse(url: &str, base_dir: Option<&Path>) -> Self {
        if let Some(fragment) = url.strip_prefix('#') {
            return LinkTarget::Anchor(fragment.to_owned());
        }

        // `file://` is a local-path hint, not a remote URL.
        if let Some(stripped) = url.strip_prefix("file://") {
            let (path, fragment) = split_fragment(stripped);
            return LinkTarget::LocalFile {
                path: PathBuf::from(path),
                fragment,
            };
        }

        if has_url_scheme(url) {
            return LinkTarget::Url(url.to_owned());
        }

        let (path, fragment) = split_fragment(url);
        let path = PathBuf::from(path);
        let resolved = if path.is_absolute() {
            path
        } else if let Some(dir) = base_dir {
            dir.join(path)
        } else {
            path
        };
        LinkTarget::LocalFile {
            path: resolved,
            fragment,
        }
    }

    /// True when this target points at a Markdown file that edamame can
    /// open in-editor.  Case-insensitive check on `.md`/`.markdown`.
    /// Used by tests in this module.
    #[allow(dead_code)]
    pub fn is_markdown_file(&self) -> bool {
        match self {
            LinkTarget::LocalFile { path, .. } => path
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

/// Split a local link's `path#fragment` at the first `#`, returning
/// the path text and the fragment (`None` when there is no `#`, and
/// also when the fragment is empty — `foo.md#` names no heading).
///
/// The split is unconditional on the first `#`, which is what every
/// other Markdown tool does; the cost is that a file whose *name*
/// contains a `#` can only be linked with the character
/// percent-encoded.  Callers reach here only after the `#`-leading
/// (pure anchor) case has been handled, so `path` is never empty.
fn split_fragment(url: &str) -> (&str, Option<String>) {
    match url.split_once('#') {
        Some((path, fragment)) if !fragment.is_empty() => (path, Some(fragment.to_owned())),
        Some((path, _)) => (path, None),
        None => (url, None),
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

    /// A fragment-less `LocalFile` target, the shape most assertions
    /// here want.
    fn local(path: &str) -> LinkTarget {
        LinkTarget::LocalFile {
            path: PathBuf::from(path),
            fragment: None,
        }
    }

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
            LinkTarget::LocalFile {
                path: PathBuf::from("/abs/path.md"),
                fragment: None,
            }
        );
    }

    #[test]
    fn relative_path_resolves_against_base_dir() {
        let base = PathBuf::from("/home/user/docs");
        assert_eq!(
            LinkTarget::parse("./sibling.md", Some(&base)),
            LinkTarget::LocalFile {
                path: base.join("./sibling.md"),
                fragment: None,
            }
        );
        assert_eq!(
            LinkTarget::parse("../other.md", Some(&base)),
            LinkTarget::LocalFile {
                path: base.join("../other.md"),
                fragment: None,
            }
        );
    }

    #[test]
    fn bare_filename_without_base_stays_relative() {
        assert_eq!(
            LinkTarget::parse("foo.md", None),
            LinkTarget::LocalFile {
                path: PathBuf::from("foo.md"),
                fragment: None,
            }
        );
    }

    #[test]
    fn absolute_path_is_not_rejoined_with_base() {
        let base = PathBuf::from("/home/user/docs");
        assert_eq!(
            LinkTarget::parse("/etc/hosts.md", Some(&base)),
            LinkTarget::LocalFile {
                path: PathBuf::from("/etc/hosts.md"),
                fragment: None,
            }
        );
    }

    #[test]
    fn windows_drive_letter_is_not_a_url_scheme() {
        // Single-char "scheme" is a Windows drive letter — classify as a
        // local path, not a URL.
        let classified = LinkTarget::parse("C:/Users/name/doc.md", None);
        assert!(matches!(classified, LinkTarget::LocalFile { .. }));
    }

    #[test]
    fn is_markdown_file_matches_md_and_markdown() {
        let md = local("foo.md");
        let markdown = local("bar.Markdown");
        let other = local("baz.txt");
        assert!(md.is_markdown_file());
        assert!(markdown.is_markdown_file());
        assert!(!other.is_markdown_file());
        assert!(!LinkTarget::Anchor("x".into()).is_markdown_file());
        assert!(!LinkTarget::Url("https://x".into()).is_markdown_file());
    }
    #[test]
    fn markdown_link_with_fragment_splits_path_and_fragment() {
        let base = PathBuf::from("/home/user/docs");
        assert_eq!(
            LinkTarget::parse("editing.md#when-the-file-changes", Some(&base)),
            LinkTarget::LocalFile {
                path: base.join("editing.md"),
                fragment: Some("when-the-file-changes".to_owned()),
            }
        );
    }

    #[test]
    fn empty_trailing_fragment_is_dropped() {
        assert_eq!(LinkTarget::parse("foo.md#", None), local("foo.md"));
    }

    #[test]
    fn fragment_bearing_link_still_reads_as_markdown() {
        // The whole point of the split: with the fragment attached the
        // extension was `md#when-the-file-changes`, so the link was
        // handed to the OS opener instead of the editor (issue #38).
        assert!(LinkTarget::parse("editing.md#section", None).is_markdown_file());
    }

    #[test]
    fn file_scheme_carries_a_fragment_too() {
        assert_eq!(
            LinkTarget::parse("file:///abs/path.md#intro", None),
            LinkTarget::LocalFile {
                path: PathBuf::from("/abs/path.md"),
                fragment: Some("intro".to_owned()),
            }
        );
    }

    #[test]
    fn remote_url_keeps_its_fragment_inline() {
        // The OS handler wants the whole URL, fragment included.
        assert_eq!(
            LinkTarget::parse("https://example.com/p#frag", None),
            LinkTarget::Url("https://example.com/p#frag".to_owned())
        );
    }
}
