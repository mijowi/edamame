//! Resolve a relative link written inside an embedded documentation
//! page.
//!
//! Pure and I/O-free, like [`crate::editor::link::LinkTarget::parse`],
//! and unit-tested against string literals for the same reason.  The
//! `#fragment` has already been split off by `LinkTarget::parse` by the
//! time this sees a path, so it is carried through rather than parsed
//! again.
//!
//! **Why this exists at all.**  A doc page is pathless, so
//! `LinkTarget::parse` has no `base_dir` and hands back a bare relative
//! `PathBuf` — which the ordinary local-file path would resolve against
//! the process's working directory, opening whatever `security.md`
//! happens to sit next to the user's shell.  Interception therefore has
//! to happen somewhere; it happens in `App::follow_link`, gated on a
//! doc page actually being open, rather than inside `LinkTarget::parse`,
//! which has no business knowing about documentation and must keep
//! answering the same way for an ordinary user document that links to a
//! file of its own named `security.md`.

use std::path::Path;

use super::registry::DocId;

/// Where the repository lives, for links that leave the embedded set.
const REPO_BLOB_BASE: &str = "https://github.com/mijowi/edamame/blob/main";

/// What a relative link inside a doc page turns out to name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocLinkResolution {
    /// Another embedded page, with any fragment carried through.
    Doc(DocId, Option<String>),
    /// A file that ships in the repository but not in the binary —
    /// the contributor pages under `docs/dev/` and the root
    /// `SECURITY.md`.  Handed to the system browser as a GitHub URL.
    External(String),
}

/// Classify `path` as written inside an embedded page.
///
/// An exact file-name match is another embedded page; anything else is
/// a repository file we do not carry, mapped onto its GitHub URL so the
/// link still goes somewhere truthful instead of failing silently.
pub fn resolve_doc_reference(path: &Path, fragment: Option<String>) -> DocLinkResolution {
    if let Some(id) = path.to_str().and_then(DocId::from_slug) {
        return DocLinkResolution::Doc(id, fragment);
    }
    let mut url = format!("{REPO_BLOB_BASE}/{}", repo_relative_path(path));
    if let Some(f) = fragment {
        url.push('#');
        url.push_str(&f);
    }
    DocLinkResolution::External(url)
}

/// Re-root a link that leaves the embedded set onto a repository path.
///
/// Links inside the docs are written relative to `docs/`, so `..`
/// climbs to the repository root (`../SECURITY.md` → `SECURITY.md`) and
/// anything else stays beneath it (`dev/theming.md` →
/// `docs/dev/theming.md`).  Resolved textually rather than with
/// `Path::canonicalize`, which would consult a filesystem that has
/// nothing to do with these paths.
fn repo_relative_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    // Every doc lives in `docs/`, so that is the starting directory.
    parts.push("docs");
    for segment in raw.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn resolve(p: &str, frag: Option<&str>) -> DocLinkResolution {
        resolve_doc_reference(&PathBuf::from(p), frag.map(str::to_owned))
    }

    #[test]
    fn a_sibling_page_resolves_to_that_page() {
        assert_eq!(
            resolve("security.md", None),
            DocLinkResolution::Doc(DocId::Security, None)
        );
    }

    #[test]
    fn a_fragment_is_carried_through_to_the_page() {
        // This exact link is in the shipped docs today.
        assert_eq!(
            resolve("keybindings.md", Some("terminal-compatibility")),
            DocLinkResolution::Doc(
                DocId::Keybindings,
                Some("terminal-compatibility".to_owned())
            )
        );
    }

    #[test]
    fn a_contributor_page_becomes_a_github_url_under_docs() {
        assert_eq!(
            resolve("dev/theming.md", None),
            DocLinkResolution::External(format!("{REPO_BLOB_BASE}/docs/dev/theming.md"))
        );
    }

    #[test]
    fn a_parent_reference_climbs_out_of_the_docs_directory() {
        // `../SECURITY.md` is written from `docs/`, so it names the
        // repository root — not `docs/../SECURITY.md`.
        assert_eq!(
            resolve("../SECURITY.md", None),
            DocLinkResolution::External(format!("{REPO_BLOB_BASE}/SECURITY.md"))
        );
    }

    #[test]
    fn an_external_link_keeps_its_fragment() {
        assert_eq!(
            resolve("dev/security-invariants.md", Some("checklist")),
            DocLinkResolution::External(format!(
                "{REPO_BLOB_BASE}/docs/dev/security-invariants.md#checklist"
            ))
        );
    }

    #[test]
    fn an_unknown_file_name_does_not_masquerade_as_a_page() {
        // Falling through to GitHub is right: we know the docs
        // directory, so the guess is at least truthful about where it
        // looked.
        assert_eq!(
            resolve("nonexistent.md", None),
            DocLinkResolution::External(format!("{REPO_BLOB_BASE}/docs/nonexistent.md"))
        );
    }

    #[test]
    fn every_cross_link_in_the_shipped_docs_resolves_somewhere_sane() {
        // A regression guard against a doc being renamed out from
        // under a sibling's link: every `](*.md)` target in the
        // embedded set must either name an embedded page or be one of
        // the two known out-of-set destinations.
        for page in super::super::registry::ALL_DOCS {
            for target in md_link_targets(page.source) {
                let (path, _) = match target.split_once('#') {
                    Some((p, f)) => (p, Some(f)),
                    None => (target.as_str(), None),
                };
                if path.is_empty() {
                    continue; // a same-page `#anchor`
                }
                let resolved = resolve(path, None);
                if let DocLinkResolution::External(url) = &resolved {
                    assert!(
                        url.contains("/docs/dev/") || url.ends_with("/SECURITY.md"),
                        "{} links to {path}, which resolves to an unexpected {url}",
                        page.slug
                    );
                }
            }
        }
    }

    /// Every `](target)` in `src` whose target looks like a local
    /// Markdown path — enough for the guard above, deliberately not a
    /// Markdown parser.
    fn md_link_targets(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        let bytes: Vec<char> = src.chars().collect();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == ']' && bytes[i + 1] == '(' {
                let start = i + 2;
                let mut j = start;
                while j < bytes.len() && bytes[j] != ')' {
                    j += 1;
                }
                if j < bytes.len() {
                    let target: String = bytes[start..j].iter().collect();
                    if target.contains(".md") && !target.starts_with("http") {
                        out.push(target);
                    }
                }
                i = j;
            }
            i += 1;
        }
        out
    }
}
