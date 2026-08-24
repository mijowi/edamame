//! The set of documentation pages compiled into the binary.
//!
//! [`ALL_DOCS`] is the single derivation for the generated index page
//! and for cross-document link resolution: both iterate it, so a page
//! added here reaches both without another edit.
//!
//! The command palette is the one consumer that does **not** derive
//! from it — `ui::command_palette::actions::ALL_ACTIONS` is a
//! hand-written list of `Action::OpenDoc(...)` literals, because that
//! array is a `const` of unit-ish `Action` values rather than something
//! built at runtime.  So a page added here needs exactly one more line
//! there, and forgetting it would leave the page reachable only by a
//! link from another page, silently and with nothing failing to
//! compile.  `the_palette_lists_every_embedded_page_exactly_once` pins
//! the two against each other.

use std::borrow::Cow;

/// One page of the shipped manual.
///
/// [`DocId::Index`] is the odd member: it has no file behind it and is
/// built at runtime by [`index_source`], so it is deliberately absent
/// from [`ALL_DOCS`] — that array is "one entry per `include_str!`",
/// which is what makes it the right thing to iterate when generating
/// the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocId {
    /// The generated landing page linking to every other page.
    Index,
    GettingStarted,
    Editing,
    Keybindings,
    TerminalCompatibility,
    Configuration,
    Themes,
    VimMode,
    Security,
}

/// A page's file name, title, and embedded source.
#[derive(Debug, Clone, Copy)]
pub struct DocPage {
    pub id: DocId,
    /// The file name as it appears in a Markdown link *inside* the
    /// docs (`security.md`).  This is the join key cross-document
    /// links resolve against, which is why it carries the extension
    /// rather than being a bare slug.
    pub slug: &'static str,
    /// Human-readable name, used by the status bar.
    pub title: &'static str,
    /// The command palette's label for this page.
    ///
    /// Stored as a literal rather than built from `title`, because
    /// `label_for` hands the palette a `&'static str` and formatting
    /// one at runtime would mean leaking it.  Kept beside `title` so
    /// the two are read and changed together.
    pub palette_label: &'static str,
    /// The page's Markdown, compiled in.
    pub source: &'static str,
}

/// Every embedded page, in the order the generated index lists them —
/// roughly the order a new user should read them, not alphabetical.
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
        id: DocId::Security,
        slug: "security.md",
        title: "Security",
        palette_label: "Docs: Security",
        source: include_str!("../../docs/security.md"),
    },
];

/// The title the index page carries, both as its `# ` heading and as
/// its status-bar label.
const INDEX_TITLE: &str = "Documentation";

/// The index's palette entry.  Named "Help" rather than "Docs" so it
/// sorts away from the seven per-page entries and reads as the way in
/// for someone who does not yet know which page they want.
const INDEX_PALETTE_LABEL: &str = "Help: Documentation";

impl DocId {
    /// The page's entry in [`ALL_DOCS`], or `None` for [`DocId::Index`]
    /// which has none.
    fn page(self) -> Option<&'static DocPage> {
        ALL_DOCS.iter().find(|p| p.id == self)
    }

    /// Human-readable name, shown in the status bar as `Docs: <title>`.
    pub fn title(self) -> &'static str {
        self.page().map_or(INDEX_TITLE, |p| p.title)
    }

    /// The command palette's label for this page.
    pub fn palette_label(self) -> &'static str {
        self.page().map_or(INDEX_PALETTE_LABEL, |p| p.palette_label)
    }

    /// The page's Markdown source.
    ///
    /// `Cow` because [`DocId::Index`] is the only variant that has to
    /// build its text; every real page hands back the `include_str!`d
    /// `&'static str` with no allocation.
    pub fn source(self) -> Cow<'static, str> {
        match self.page() {
            Some(p) => Cow::Borrowed(p.source),
            None => Cow::Owned(index_source()),
        }
    }

    /// The page a cross-document link names, matched on the file name
    /// **exactly**.
    ///
    /// No leniency, for the same reason
    /// [`crate::app::App::heading_line_for_fragment`] allows none: a
    /// link that resolves only inside edamame is one an author ships
    /// broken to GitHub without ever seeing it fail here.  Any path
    /// with a directory component (`dev/theming.md`, `../SECURITY.md`)
    /// declines and falls to the GitHub branch in
    /// [`super::link::resolve_doc_reference`].
    ///
    /// The index is unreachable this way on purpose: it is generated,
    /// so no page links to it by file name.
    pub fn from_slug(slug: &str) -> Option<Self> {
        ALL_DOCS.iter().find(|p| p.slug == slug).map(|p| p.id)
    }
}

/// Build the index page: a heading, a line of orientation, and one
/// bullet per embedded page.
///
/// Generated at runtime rather than committed as a `docs/index.md`
/// because the bullets must agree with [`ALL_DOCS`], and a checked-in
/// file is one more copy to keep in step.  It is plain Markdown, so
/// every bullet is an ordinary link the existing cross-document
/// resolver already handles — the index needs no special case anywhere
/// downstream.
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
        // A directory component must never resolve — those are the
        // contributor pages, which are not embedded.
        assert_eq!(DocId::from_slug("dev/theming.md"), None);
        assert_eq!(DocId::from_slug("../SECURITY.md"), None);
        // No leniency: neither case folding nor a missing extension.
        assert_eq!(DocId::from_slug("Security.md"), None);
        assert_eq!(DocId::from_slug("security"), None);
    }

    #[test]
    fn the_index_is_not_reachable_by_file_name() {
        // It is generated, so nothing links to it; a page claiming the
        // name would shadow a real one.
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
