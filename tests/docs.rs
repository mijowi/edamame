//! Integration tests for the manual shipped inside the binary.
//!
//! The unit tests under `docs::` cover the registry and the pure link
//! resolver against literals.  These cover the half that needs the
//! Markdown pipeline: that every page actually parses, and that the
//! links *between* pages resolve all the way down to a real heading.
//!
//! That second guard is the one worth having.  Fragments are matched
//! exactly — no slugifying, no case folding — so renaming a heading
//! breaks every sibling page's link to it silently, and only for the
//! reader who follows it in the app.

use edamame::config::Theme;
use edamame::docs::{DocId, DocLinkResolution, ALL_DOCS};
use edamame::document::ParsedDoc;

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

fn parse(src: &str) -> ParsedDoc {
    ParsedDoc::build(src, theme(), true, 20)
}

/// Every `](target)` in `src` that names a local Markdown path.
/// Deliberately a scan, not a Markdown parse — it only has to find the
/// links this repository's own docs are written with.
fn md_link_targets(src: &str) -> Vec<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == ']' && chars[i + 1] == '(' {
            let start = i + 2;
            let mut j = start;
            while j < chars.len() && chars[j] != ')' {
                j += 1;
            }
            if j < chars.len() {
                let t: String = chars[start..j].iter().collect();
                if t.contains(".md") && !t.starts_with("http") {
                    out.push(t);
                }
            }
            i = j;
        }
        i += 1;
    }
    out
}

#[test]
fn every_embedded_page_parses_into_blocks() {
    for page in ALL_DOCS {
        let doc = parse(page.source);
        assert!(!doc.blocks.is_empty(), "{} parsed to nothing", page.slug);
    }
}

#[test]
fn the_generated_index_parses_and_links_every_page() {
    let src = DocId::Index.source();
    let doc = parse(&src);
    assert!(!doc.blocks.is_empty(), "the index parsed to nothing");
    let targets = md_link_targets(&src);
    for page in ALL_DOCS {
        assert!(
            targets.iter().any(|t| t == page.slug),
            "the index does not link {}",
            page.slug
        );
    }
}

#[test]
fn every_cross_page_fragment_names_a_real_heading() {
    // The guard this file exists for.  A fragment that no longer
    // matches would leave the link opening the right page at the wrong
    // place, reported as "section not found" — and only in the app.
    for page in ALL_DOCS {
        for target in md_link_targets(page.source) {
            let Some((path, frag)) = target.split_once('#') else {
                continue;
            };
            if path.is_empty() || frag.is_empty() {
                continue;
            }
            let DocLinkResolution::Doc(id, _) =
                edamame::docs::resolve_doc_reference(std::path::Path::new(path), None)
            else {
                // Out of the embedded set (the contributor pages); its
                // headings are not ours to check.
                continue;
            };
            let doc = parse(&id.source());
            assert!(
                doc.heading_anchors.contains_key(frag),
                "{} links to {target}, but '{frag}' is not a heading in {}",
                page.slug,
                id.title(),
            );
        }
    }
}

#[test]
fn a_same_page_anchor_names_a_real_heading() {
    // The same guard for `#section` links a page makes to itself.
    for page in ALL_DOCS {
        let doc = parse(page.source);
        let chars: Vec<char> = page.source.chars().collect();
        let mut i = 0;
        while i + 2 < chars.len() {
            if chars[i] == ']' && chars[i + 1] == '(' && chars[i + 2] == '#' {
                let start = i + 3;
                let mut j = start;
                while j < chars.len() && chars[j] != ')' {
                    j += 1;
                }
                if j < chars.len() {
                    let frag: String = chars[start..j].iter().collect();
                    if !frag.is_empty() {
                        assert!(
                            doc.heading_anchors.contains_key(frag.as_str()),
                            "{} links to #{frag}, which is not one of its headings",
                            page.slug
                        );
                    }
                }
                i = j;
            }
            i += 1;
        }
    }
}
