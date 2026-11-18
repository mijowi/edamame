//! Heading anchors against the ones GitHub renders, in-app and in the HTML export.
//!
//! The expected anchors were taken from GitHub's Markdown API, which renders repository files
//! with their heading links: `gh api markdown -f mode=markdown -F text=@doc.md`, reading each
//! `id="user-content-…"` in order.  To extend a case list, render the document it describes
//! that way and record what GitHub returns, never what edamame does.

use edamame::config::Theme;
use edamame::document::ParsedDoc;
use edamame::export::{render_html, HtmlExportOptions};

/// Structural cases: separators, case, inline syntax that adds or hides text, line breaks
/// inside a heading, and duplicate numbering.
const STRUCTURAL: &str = r"# Hello, World!

# API v2 — Release Notes

# snake_case and kebab-case

#   spaces   here

# a	tab

# ΟΔΟΣ ΣΑΣ

# İstanbul

# Rust 🦀 tips

# 見出し、その「一」

# x² ① ★ Ⓐ

# A ![alt text](x.png) b

# c <span>in</span> d

# e \<b\> f

# g &amp; h &copy; i

# `co de` *em* __strong__ ~~del~~

# [link *text*](http://example.com) j

# $x+y$ k

# l <!-- note --> m

# p\* q \# r

# 1. s

# Echo

# Echo

# Echo 1

# Echo 1

# Echo

Foo
bar
===

baz\
qux
---
";

const STRUCTURAL_ANCHORS: &[&str] = &[
    "hello-world",
    "api-v2--release-notes",
    "snake_case-and-kebab-case",
    "spaces---here",
    "atab",
    "οδοσ-σασ",
    "i\u{307}stanbul",
    "rust--tips",
    "見出しその一",
    "x---ⓐ",
    "a--b",
    "c-in-d",
    "e-b-f",
    "g--h--i",
    "co-de-em-strong-del",
    "link-text-j",
    "xy-k",
    "l--m",
    "p-q--r",
    "1-s",
    "echo",
    "echo-1",
    "echo-1-1",
    "echo-1-2",
    "echo-2",
    "foobar",
    "bazqux",
];

/// Footnote references slug as the number GitHub renders them as: numbered by first reference in
/// source order, wherever it sits (a paragraph, quote, list, table, image alt, link text, or a
/// definition, even an unreferenced one), case-insensitively, a repeat reusing its number and
/// adjacent references concatenating; an undefined label stays literal text.
const FOOTNOTES: &str = r"[^early]: Defined first, referencing [^inside].

Text[^b] and[^a].

# H1[^a] x

# H2[^note][^b] y

# H3[^C] z

# H4[^missing] w

# H5[^b] v

> Quote[^q].

- Item[^l].

| t[^tb] |
|---|
| c |

![alt[^img]](x.png) [link[^lk]](http://e.com)

# H6[^early] u

# H7[^inside] t

# H8[^late] s

[^a]: A.
[^b]: B.
[^note]: N.
[^c]: c.
[^q]: q.
[^l]: l.
[^tb]: t.
[^img]: i.
[^lk]: k.
[^inside]: in.
[^unused]: u [^late].
[^late]: l.
";

const FOOTNOTE_ANCHORS: &[&str] = &[
    "h13-x",
    "h242-y",
    "h35-z",
    "h4missing-w",
    "h52-v",
    "h611-u",
    "h71-t",
    "h812-s",
];

/// Up to eight code points from every Unicode general category, spread evenly, plus the
/// edges the slug rules turn on; each is tested as the heading `# U+<code point> a<char>b`.
const CODE_POINTS: &str = include_str!("fixtures/github_anchors.tsv");

fn code_point_cases() -> (String, Vec<String>) {
    let mut doc = String::new();
    let mut anchors = Vec::new();
    for line in CODE_POINTS.lines().filter(|l| !l.starts_with('#')) {
        let (hex, anchor) = line.split_once('\t').expect("`code point<TAB>anchor`");
        let ch = u32::from_str_radix(hex, 16)
            .ok()
            .and_then(char::from_u32)
            .unwrap_or_else(|| panic!("bad code point {hex}"));
        doc.push_str(&format!("# U+{hex} a{ch}b\n\n"));
        anchors.push(anchor.to_owned());
    }
    (doc, anchors)
}

/// `heading_anchors` in document order.
fn in_app_anchors(md: &str) -> Vec<String> {
    let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
    let parsed = ParsedDoc::build(md, theme, true, 80);
    let mut anchors: Vec<(usize, String)> = parsed
        .heading_anchors
        .iter()
        .map(|(slug, &line)| (line, slug.clone()))
        .collect();
    anchors.sort_unstable();
    anchors.into_iter().map(|(_, slug)| slug).collect()
}

/// Every heading `id` the export writes, in document order.
fn exported_anchors(md: &str) -> Vec<String> {
    let html = render_html(md, &HtmlExportOptions::default()).expect("export renders");
    html.split("<h")
        .skip(1)
        .filter(|tag| tag.starts_with(|c: char| ('1'..='6').contains(&c)))
        .filter_map(|tag| {
            let tag = &tag[..tag.find('>')?];
            let start = tag.find(" id=\"")? + 5;
            Some(tag[start..start + tag[start..].find('"')?].to_owned())
        })
        .collect()
}

/// Report every mismatch at once, so one run shows the whole drift.
fn assert_anchors(actual: &[String], expected: &[impl AsRef<str>], what: &str) {
    let mismatches: Vec<String> = actual
        .iter()
        .zip(expected)
        .filter(|(a, e)| a.as_str() != e.as_ref())
        .map(|(a, e)| format!("  expected {:?}, got {a:?}", e.as_ref()))
        .collect();
    assert!(
        mismatches.is_empty() && actual.len() == expected.len(),
        "{what}: {} of {} differ from GitHub ({} anchors, {} expected)\n{}",
        mismatches.len(),
        expected.len(),
        actual.len(),
        expected.len(),
        mismatches.join("\n"),
    );
}

#[test]
fn structural_cases_match_github_in_app() {
    assert_anchors(&in_app_anchors(STRUCTURAL), STRUCTURAL_ANCHORS, "in-app");
}

#[test]
fn structural_cases_match_github_in_the_export() {
    assert_anchors(&exported_anchors(STRUCTURAL), STRUCTURAL_ANCHORS, "export");
}

#[test]
fn every_category_matches_github_in_app() {
    let (doc, expected) = code_point_cases();
    assert_anchors(&in_app_anchors(&doc), &expected, "in-app");
}

#[test]
fn every_category_matches_github_in_the_export() {
    let (doc, expected) = code_point_cases();
    assert_anchors(&exported_anchors(&doc), &expected, "export");
}

#[test]
fn footnote_numbers_match_github_in_app() {
    assert_anchors(&in_app_anchors(FOOTNOTES), FOOTNOTE_ANCHORS, "in-app");
}

#[test]
fn footnote_numbers_match_github_in_the_export() {
    assert_anchors(&exported_anchors(FOOTNOTES), FOOTNOTE_ANCHORS, "export");
}
