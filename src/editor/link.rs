//! Link target classification for link following (see `docs/dev/link-following.md`).
//! Deliberately pure — no I/O, no `App` state — so mouse and keyboard dispatch share it.

use std::path::{Path, PathBuf};

/// A classified link destination ready for App-level dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// Absolute URL with a scheme (`https:`, `mailto:`, …); opened by the OS handler.
    Url(String),
    /// Local path, resolved against the document's directory when relative.  `.md` opens
    /// in-editor, anything else goes to `open::that`.  `fragment` is the `#section` of a
    /// deep link, split off before resolution — left attached it reached the OS handler as
    /// `editing.md#heading` and failed (issue #38).  `None` for no or an empty fragment.
    LocalFile {
        path: PathBuf,
        fragment: Option<String>,
    },
    /// In-document `#heading` anchor; `"#"` yields `Anchor("")` and the caller decides.
    Anchor(String),
    /// Footnote reference `[^label]` (raw label); built by the footnote scanner, not `parse`.
    Footnote(String),
    /// A footnote definition's back-link to the reference the reader came from.
    FootnoteBack(String),
}

impl LinkTarget {
    /// Classify `url`, resolving relative local paths against `base_dir`.  `file://` is a
    /// local-path hint (mirroring `image::loader::resolve_local_path`), and a one-letter
    /// "scheme" is a Windows drive letter, not a URL.
    pub fn parse(url: &str, base_dir: Option<&Path>) -> Self {
        if let Some(fragment) = url.strip_prefix('#') {
            return LinkTarget::Anchor(fragment.to_owned());
        }

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

    /// Case-insensitive `.md` / `.markdown` check on a `LocalFile` (test helper).
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

/// Split `path#fragment` at the first `#` (as every Markdown tool does; a `#` in a file name
/// must be percent-encoded).  An empty fragment becomes `None`.
fn split_fragment(url: &str) -> (&str, Option<String>) {
    match url.split_once('#') {
        Some((path, fragment)) if !fragment.is_empty() => (path, Some(fragment.to_owned())),
        Some((path, _)) => (path, None),
        None => (url, None),
    }
}

/// True when `url` starts with a multi-character RFC-3986 scheme (one char is a drive letter).
pub fn has_url_scheme(url: &str) -> bool {
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
        // Regression for issue #38.
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
        assert_eq!(
            LinkTarget::parse("https://example.com/p#frag", None),
            LinkTarget::Url("https://example.com/p#frag".to_owned())
        );
    }
}
