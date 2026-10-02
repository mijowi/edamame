//! Every relative link in the repository's Markdown resolves: the target file exists, and a
//! `#fragment` on a Markdown target names one of its headings.
//!
//! `tests/docs.rs` checks the manual as the binary embeds and resolves it; this checks every
//! Markdown file as a reader on GitHub follows it — README, AGENTS.md, CHANGELOG, `docs/dev/`
//! (which nothing else reads until a contributor follows a link that a rename broke), the
//! user pages again, and `tests/fixtures/`.  `docs/dev/plans/` is historical and left as
//! written; links into it are checked only where it exists, since the crates.io package
//! excludes it.
//!
//! Links come from a real Markdown parse, so a `[x](y)` inside a code span or fence is not
//! mistaken for one.  Fragments are checked against [`ParsedDoc::heading_anchors`] — the GFM
//! slugs edamame's own `#anchor` jump uses, which match GitHub's for top-level headings.
//! Paths and fragments are percent-decoded, and a leading `/` is the repository root, as on
//! GitHub.  External URLs are not fetched.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};

use edamame::config::Theme;
use edamame::document::ParsedDoc;
use edamame::editor::link::has_url_scheme;

/// Historical plans: never scanned, and not shipped in the crates.io package (see `exclude` in
/// `Cargo.toml`), so a link *into* them only resolves in a git checkout.
const PLANS: &str = "docs/dev/plans";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The Markdown checked: the repository root's own files, `tests/fixtures/`, and `docs/`
/// recursively minus [`PLANS`].  An allowlist rather than a skip list because whatever else
/// sits in a working tree — `target/`, `.claude/worktrees/` (whole stale checkouts), scratch
/// notes — is untracked and would otherwise fail the test on one machine only.
fn markdown_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_markdown(&root(), false, &mut out);
    collect_markdown(&root().join("tests/fixtures"), false, &mut out);
    collect_markdown(&root().join("docs"), true, &mut out);
    out.sort();
    out
}

/// `file_type` does not follow symlinks, so a symlink is neither read nor descended into:
/// `CLAUDE.md` would only repeat `AGENTS.md`, and a linked directory (a Nix `result`) can
/// point anywhere.
fn collect_markdown(dir: &Path, recurse: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if recurse && path != root().join(PLANS) {
                collect_markdown(&path, true, out);
            }
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
}

/// Fold `.` and `..` out of `path` without touching the filesystem, so a target can be
/// compared by prefix however the link spelled it.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Link and image destinations, as the parser resolved them (reference-style included).
/// An email autolink is left out: its destination is the bare address, with no `mailto:`
/// scheme to mark it as external.
fn destinations(src: &str) -> Vec<String> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_FOOTNOTES | Options::ENABLE_STRIKETHROUGH;
    Parser::new_ext(src, opts)
        .filter_map(|event| match event {
            Event::Start(Tag::Link {
                link_type: LinkType::Email,
                ..
            }) => None,
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                Some(dest_url.into_string())
            }
            _ => None,
        })
        .collect()
}

fn heading_anchors(path: &Path, theme: &'static Theme) -> Vec<String> {
    let src = fs::read_to_string(path).unwrap_or_default();
    ParsedDoc::build(&src, theme, false, 80)
        .heading_anchors
        .into_keys()
        .collect()
}

#[test]
fn every_relative_link_in_the_repository_docs_resolves() {
    let files = markdown_files();
    assert!(
        files.iter().any(|f| f.ends_with("AGENTS.md"))
            && files.iter().any(|f| f.starts_with(root().join("docs/dev"))),
        "the walk missed the docs; is CARGO_MANIFEST_DIR the repository root?"
    );
    let plans = root().join(PLANS);
    let plans_shipped = plans.exists();

    let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
    let mut anchors: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let mut broken = Vec::new();
    for file in &files {
        let src = fs::read_to_string(file).expect("readable Markdown");
        let rel = file.strip_prefix(root()).unwrap_or(file).display();
        for dest in destinations(&src) {
            if dest.is_empty() || has_url_scheme(&dest) {
                continue;
            }
            // An empty fragment (`file.md#`) is no fragment, as `editor::link` treats it.
            let (path, fragment) = match dest.split_once('#') {
                Some((p, f)) => (p, Some(f).filter(|f| !f.is_empty())),
                None => (dest.as_str(), None),
            };
            let Ok(path) = percent_decode_str(path).decode_utf8() else {
                broken.push(format!("{rel}: {dest} — path is not UTF-8 once decoded"));
                continue;
            };
            let target = normalize(&if path.is_empty() {
                file.clone()
            } else if let Some(from_root) = path.strip_prefix('/') {
                root().join(from_root)
            } else {
                file.parent().unwrap_or(&root()).join(&*path)
            });
            if !target.exists() {
                // Running from the crates.io package, which leaves the plans out.
                if !plans_shipped && target.starts_with(&plans) {
                    continue;
                }
                broken.push(format!("{rel}: {dest} — no such file"));
                continue;
            }
            let Some(fragment) = fragment else { continue };
            if target.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            let fragment = percent_decode_str(fragment).decode_utf8_lossy();
            let known = anchors
                .entry(target.clone())
                .or_insert_with(|| heading_anchors(&target, theme));
            if !known.iter().any(|a| *a == fragment) {
                broken.push(format!("{rel}: {dest} — no such heading"));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "broken documentation links:\n  {}",
        broken.join("\n  ")
    );
}
