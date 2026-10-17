//! The documentation pages compiled into the binary. [`ALL_DOCS`] drives both the generated
//! index and cross-document link resolution. The command palette does not derive from it
//! (`ui::command_palette::actions::ALL_ACTIONS` is a `const` list of literals), so a new
//! page needs one more line there; `the_palette_lists_every_embedded_page_exactly_once`
//! pins the two together.

use std::borrow::Cow;

/// One page of the shipped manual. [`DocId::Index`] has no file behind it — it is built by
/// [`index_source`] — and so is absent from [`ALL_DOCS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocId {
    Index,
    GettingStarted,
    Editing,
    Keybindings,
    TerminalCompatibility,
    Configuration,
    Themes,
    VimMode,
    HelixMode,
    Security,
}

#[derive(Debug, Clone, Copy)]
pub struct DocPage {
    pub id: DocId,
    /// File name as written in Markdown links inside the docs (`security.md`) — the join
    /// key cross-document links resolve against, hence the extension.
    pub slug: &'static str,
    /// Status-bar name.
    pub title: &'static str,
    /// A literal rather than derived from `title`: the palette wants a `&'static str`.
    pub palette_label: &'static str,
    pub source: &'static str,
}

/// Every embedded page, in the order the index lists them (reading order, not alphabetical).
pub const ALL_DOCS: &[DocPage] = &[
    DocPage {
        id: DocId::GettingStarted,
        slug: "getting-started.md",
        title: "Getting started",
        palette_label: "Docs: Getting started",
        source: include_str!("../../docs/getting-started.md"),
    },
    DocPage {
        id: DocId::Editing,
        slug: "editing.md",
        title: "Editing",
        palette_label: "Docs: Editing",
        source: include_str!("../../docs/editing.md"),
    },
    DocPage {
        id: DocId::Keybindings,
        slug: "keybindings.md",
        title: "Keybindings",
        palette_label: "Docs: Keybindings",
        source: include_str!("../../docs/keybindings.md"),
    },
    DocPage {
        id: DocId::TerminalCompatibility,
        slug: "terminal-compatibility.md",
        title: "Terminal compatibility",
        palette_label: "Docs: Terminal compatibility",
        source: include_str!("../../docs/terminal-compatibility.md"),
    },
    DocPage {
        id: DocId::Configuration,
        slug: "configuration.md",
        title: "Configuration",
        palette_label: "Docs: Configuration",
        source: include_str!("../../docs/configuration.md"),
    },
    DocPage {
        id: DocId::Themes,
        slug: "themes.md",
        title: "Themes",
        palette_label: "Docs: Themes",
        source: include_str!("../../docs/themes.md"),
    },
    DocPage {
        id: DocId::VimMode,
        slug: "vim-mode.md",
        title: "Vim mode",
        palette_label: "Docs: Vim mode",
        source: include_str!("../../docs/vim-mode.md"),
    },
    DocPage {
        id: DocId::HelixMode,
        slug: "helix-mode.md",
        title: "Helix mode",
        palette_label: "Docs: Helix mode",
        source: include_str!("../../docs/helix-mode.md"),
    },
    DocPage {
        id: DocId::Security,
        slug: "security.md",
        title: "Security",
        palette_label: "Docs: Security",
        source: include_str!("../../docs/security.md"),
    },
];

const INDEX_TITLE: &str = "Documentation Index";

/// "Help" rather than "Docs" so it sorts away from the per-page entries.
const INDEX_PALETTE_LABEL: &str = "Help: Documentation";

impl DocId {
    fn page(self) -> Option<&'static DocPage> {
        ALL_DOCS.iter().find(|p| p.id == self)
    }

    pub fn title(self) -> &'static str {
        self.page().map_or(INDEX_TITLE, |p| p.title)
    }

    pub fn palette_label(self) -> &'static str {
        self.page().map_or(INDEX_PALETTE_LABEL, |p| p.palette_label)
    }

    /// `Cow` because only [`DocId::Index`] has to build its text.
    pub fn source(self) -> Cow<'static, str> {
        match self.page() {
            Some(p) => Cow::Borrowed(p.source),
            None => Cow::Owned(index_source()),
        }
    }

    /// The page a cross-document link names, matched on the file name exactly. No leniency
    /// (the rule [`crate::document::ParsedDoc::heading_line`] follows for headings): a link
    /// that resolves only inside edamame ships broken to GitHub. Paths with a directory
    /// component decline and fall to [`super::link::resolve_doc_reference`]'s GitHub branch;
    /// the index is unreachable here.
    pub fn from_slug(slug: &str) -> Option<Self> {
        ALL_DOCS.iter().find(|p| p.slug == slug).map(|p| p.id)
    }
}

/// Build the index page as plain Markdown, generated at runtime so its bullets cannot
/// drift from [`ALL_DOCS`]; every bullet is an ordinary link the resolver already handles.
fn index_source() -> String {
    let mut out = String::from("# ");
    out.push_str(INDEX_TITLE);
    out.push_str("\n\nThe manual for the version of edamame you are running. Follow a link to open a page; `Alt-Left` or 'Navigate back' in the command palette goes back.\n\n");
    for page in ALL_DOCS {
        out.push_str("- [");
        out.push_str(page.title);
        out.push_str("](");
        out.push_str(page.slug);
        out.push_str(")\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_carries_non_empty_embedded_source() {
        for page in ALL_DOCS {
            assert!(
                !page.source.trim().is_empty(),
                "{} embedded empty",
                page.slug
            );
        }
    }

    #[test]
    fn slugs_are_unique_so_a_link_names_one_page() {
        let mut seen: Vec<&str> = ALL_DOCS.iter().map(|p| p.slug).collect();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "duplicate slug in ALL_DOCS");
    }

    #[test]
    fn from_slug_matches_exactly_and_declines_paths() {
        assert_eq!(DocId::from_slug("security.md"), Some(DocId::Security));
        assert_eq!(DocId::from_slug("dev/theming.md"), None);
        assert_eq!(DocId::from_slug("../SECURITY.md"), None);
        assert_eq!(DocId::from_slug("Security.md"), None);
        assert_eq!(DocId::from_slug("security"), None);
    }

    #[test]
    fn the_index_is_not_reachable_by_file_name() {
        assert_eq!(DocId::from_slug("index.md"), None);
    }

    #[test]
    fn the_index_links_every_embedded_page_by_its_slug() {
        let src = index_source();
        for page in ALL_DOCS {
            assert!(
                src.contains(&format!("]({})", page.slug)),
                "index omits {}",
                page.slug
            );
            assert!(src.contains(page.title), "index omits {}", page.title);
        }
    }

    #[test]
    fn index_source_allocates_but_a_real_page_does_not() {
        assert!(matches!(DocId::Security.source(), Cow::Borrowed(_)));
        assert!(matches!(DocId::Index.source(), Cow::Owned(_)));
    }

    #[test]
    fn palette_labels_are_distinct_and_cover_the_index() {
        let mut labels: Vec<&str> = ALL_DOCS.iter().map(|p| p.palette_label).collect();
        labels.push(DocId::Index.palette_label());
        let before = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(before, labels.len(), "two pages share a palette label");
    }

    #[test]
    fn titles_cover_the_index_too() {
        assert_eq!(DocId::Index.title(), INDEX_TITLE);
        assert_eq!(DocId::Keybindings.title(), "Keybindings");
    }
}
