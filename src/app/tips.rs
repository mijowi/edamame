//! The daily-tip registry: a static list of short pointers to less-obvious features, shown at
//! most once a day (see [`super::tip_notice`]).  Pure data plus one selection helper, so the whole
//! rotation is table-testable without an `App`.
//!
//! A tip is keyed by a stable `u32` [`Tip::id`], recorded in
//! [`State::seen_daily_tips`](crate::config::State::seen_daily_tips) once shown so it never
//! repeats.  [`ALL_TIPS`] is kept in ascending-id order and [`next_unseen`] takes the lowest id the
//! user has not yet seen, so a release that appends new tips surfaces them automatically and never
//! renumbers an old one.

use crate::docs::DocId;

/// One tip: a sentence or three, and an optional "learn more" pointer into the manual.
pub struct Tip {
    /// Stable identity, recorded once the tip is shown.  Never reused or renumbered.
    pub id: u32,
    /// A short label — a few words naming the feature — shown as the row in the "Browse tips"
    /// index ([`crate::app::modal::TipsIndexModal`]).  Not shown in the tip modal itself.
    pub title: &'static str,
    /// The body prose — 1-3 short sentences.  Wrapped by the modal, so it is one flowing string,
    /// not pre-broken lines.
    pub text: &'static str,
    /// A manual page the tip points at, rendered as a footnote link; `None` for a self-contained
    /// tip.
    pub link: Option<TipLink>,
}

/// A tip's "learn more" link into the shipped manual.  The modal renders it as a uniform
/// `See <page title> for more info.` footnote (see [`crate::app::modal::DailyTipModal`]), so the
/// visible link text is always the destination page's title — no per-tip label to author, and it
/// reads evenly across tips.  The `fragment` still lands the reader on the relevant section.
pub struct TipLink {
    /// The page to open; its [`DocId::title`] becomes the link text.
    pub doc: DocId,
    /// The section within it — a GFM heading slug, matched exactly — or `None` for the page top.
    pub fragment: Option<&'static str>,
}

/// Every daily tip, in ascending-id order (the order [`next_unseen`] shows them in).
pub const ALL_TIPS: &[Tip] = &[
    Tip {
        id: 1,
        title: "In-app manual",
        text: "The whole manual ships inside edamame — no browser or network needed. Press \
               Ctrl-P and type \"docs\" to choose a page, or \"Help: Documentation\" for the \
               index. Alt-Left takes you back to your original document.",
        link: Some(TipLink {
            doc: DocId::GettingStarted,
            fragment: Some("the-manual-is-inside-the-app"),
        }),
    },
    Tip {
        id: 2,
        title: "Markdown cheat sheet",
        text: "Forgot how to highlight, add a table, or insert an image? edamame has a built-in \
               Markdown reference. Open the command palette with Ctrl-P and choose \"Show Markdown \
               cheat sheet\".",
        link: None,
    },
    Tip {
        id: 3,
        title: "Format a selection",
        text: "If you select some text on one line, you can easily format it with Ctrl-B (bolds) \
               and Ctrl-I (italics). Both toggle, so the second time strips the markers back off. \
               You can also inline code, strikethrough, and highlight from the command palette.",
        link: Some(TipLink {
            doc: DocId::Editing,
            fragment: Some("formatting-text"),
        }),
    },
    Tip {
        id: 4,
        title: "Table mouse handles",
        text: "With mouse support, put the cursor in a table and it shows handles: drag to reorder a row or \
               column, drag a header divider to resize it, and click the border mark to delete. \
               \"Toggle table buttons\" in the palette hides them.",
        link: Some(TipLink {
            doc: DocId::Editing,
            fragment: Some("with-a-mouse"),
        }),
    },
    Tip {
        id: 5,
        title: "Insert commands",
        text: "edamame can scaffold Markdown for you: search the command palette for \"Insert\" \
               to reach Insert Link, Image, Table, and Footnote. Each drops a ready-made snippet \
               at the cursor, or wraps your current selection, leaving the placeholder selected \
               to type over.",
        link: None,
    },
    Tip {
        id: 6,
        title: "Back and forward",
        text: "When following links in documents, Alt-Left and Alt-Right walk back and \
               forward through everywhere you have been — across files and within one — just \
               like a browser.",
        link: Some(TipLink {
            doc: DocId::Editing,
            fragment: Some("links"),
        }),
    },
    Tip {
        id: 7,
        title: "Table structure edits",
        text: "Inside a table, Alt with an arrow key moves the current row or column; hold Shift \
               to insert one instead. Alt-Backspace deletes the row. One rule: the arrow \
               points the direction, Shift turns a move into an insert.",
        link: Some(TipLink {
            doc: DocId::Editing,
            fragment: Some("changing-the-shape"),
        }),
    },
    Tip {
        id: 8,
        title: "List numbering",
        text: "Ordered lists renumber themselves as you edit: insert an item mid-list and the \
               numbers below follow. If a paste leaves the numbering drifted, \"Fix list \
               numbering\" in the palette re-sequences the list under your cursor in one \
               undoable step.",
        link: Some(TipLink {
            doc: DocId::Editing,
            fragment: Some("lists"),
        }),
    },
    Tip {
        id: 9,
        title: "Footnote jump",
        text: "Put the cursor on a footnote reference like `[^1]` and press Ctrl-Enter to jump \
               to its definition; press it again on the definition to jump back. Ctrl-Enter \
               follows any link under the cursor.",
        link: Some(TipLink {
            doc: DocId::Editing,
            fragment: Some("footnotes"),
        }),
    },
    Tip {
        id: 10,
        title: "Search across line breaks",
        text: "Search and replace (Ctrl-F) understands escapes, so a query can cross line \
               breaks: search `\\n` and replace with a space to join wrapped lines. `\\t`, \
               `\\r`, and `\\\\` work too, and pasted text is escaped for you.",
        link: Some(TipLink {
            doc: DocId::Editing,
            fragment: Some("search-and-replace"),
        }),
    },
    Tip {
        id: 11,
        title: "Git difftool",
        text: "edamame can be a git difftool for Markdown. `edamame --diff <old> <new>` opens the same \
               stacked, read-only review over any two files Git hands it, and it skips \
               non-Markdown files on its own, so it slots in beside whatever diff tool you \
               already use for everything else.",
        link: Some(TipLink {
            doc: DocId::Editing,
            fragment: Some("using-edamame-as-a-git-difftool"),
        }),
    },
    Tip {
        id: 12,
        title: "Autosave",
        text: "Turn on autosave (\"Toggle autosave\" in the palette) and edamame saves after you stop \
               typing. You can tune the idle delay with `autosave_idle_ms` in your config.",
        link: Some(TipLink {
            doc: DocId::Configuration,
            fragment: Some("saving"),
        }),
    },
    Tip {
        id: 13,
        title: "Custom export",
        text: "Export straight to PDF, DOCX, or anything else with an `[[export.custom]]` block in \
               your config: edamame renders to HTML, then runs your own command over it. For example \
               `command = [\"pandoc\", \"{html}\", \"-o\", \"{out}\"]`. Each entry appears as a \
               format in the export modal.",
        link: Some(TipLink {
            doc: DocId::Configuration,
            fragment: Some("exportcustom"),
        }),
    },
    Tip {
        id: 14,
        title: "Custom themes",
        text: "Want your own theme? \"Create custom theme\" in the palette copies any built-in \
               into the `themes/` folder in your config directory with each item explained. \
               All colors are derived from the theme palette, so changing it retints the whole app.",
        link: Some(TipLink {
            doc: DocId::Themes,
            fragment: Some("making-your-own"),
        }),
    },
    Tip {
        id: 15,
        title: "Vim mode",
        text: "edamame has an optional Vim mode: motions, operators, text objects, `/` search, \
               and `:s` substitution, all adapted to live Markdown. It is off by default — turn \
               it on from the command palette (\"Toggle Vim mode\").",
        link: Some(TipLink {
            doc: DocId::VimMode,
            fragment: Some("enabling-vim-mode"),
        }),
    },
    Tip {
        id: 16,
        title: "Paste an image",
        text: "Take a screenshot to the clipboard, then press Ctrl-V: edamame saves the image \
               beside your document, in the folder your other images use, and inserts a \
               reference to it. You confirm the path first, so you can rename it on the spot.",
        link: Some(TipLink {
            doc: DocId::Editing,
            fragment: Some("images"),
        }),
    },
];

/// The lowest-id tip the user has not seen, or `None` once every tip has been shown.
pub fn next_unseen(seen: &[u32]) -> Option<&'static Tip> {
    ALL_TIPS.iter().find(|t| !seen.contains(&t.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::ParsedDoc;

    #[test]
    fn ids_are_unique_and_ascending() {
        // `next_unseen` returns the *first* unseen tip, so "lowest unseen" only holds while the
        // list is sorted; unique ids keep the seen-set unambiguous.
        for pair in ALL_TIPS.windows(2) {
            assert!(pair[0].id < pair[1].id, "ALL_TIPS must be ascending by id");
        }
    }

    #[test]
    fn every_tip_has_non_empty_text_and_title() {
        for tip in ALL_TIPS {
            assert!(!tip.text.trim().is_empty(), "tip {} has empty text", tip.id);
            assert!(
                !tip.title.trim().is_empty(),
                "tip {} has empty title",
                tip.id
            );
        }
    }

    /// A tip's link must name a real page, and its fragment must be a real heading on that page.
    /// A missing anchor would only open the page top — a silent papercut — so it is pinned here:
    /// resolution is exactly `ParsedDoc::heading_anchors`, the same table
    /// [`App::heading_line_for_fragment`](crate::app::App) consults.
    #[test]
    fn every_link_resolves_to_a_page_and_a_heading() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        for tip in ALL_TIPS {
            let Some(link) = &tip.link else { continue };
            let parsed = ParsedDoc::build(&link.doc.source(), theme, true, 80);
            assert!(
                !parsed.blocks.is_empty(),
                "tip {} links {} which embeds empty",
                tip.id,
                link.doc.title(),
            );
            if let Some(fragment) = link.fragment {
                assert!(
                    parsed.heading_anchors.contains_key(fragment),
                    "tip {} links {}#{fragment}, which is not a heading on that page",
                    tip.id,
                    link.doc.title(),
                );
            }
        }
    }

    #[test]
    fn next_unseen_walks_in_order_then_stops() {
        assert_eq!(next_unseen(&[]).map(|t| t.id), Some(1));
        assert_eq!(next_unseen(&[1]).map(|t| t.id), Some(2));
        // An out-of-order seen set still skips what it names.
        assert_eq!(next_unseen(&[2]).map(|t| t.id), Some(1));
        let all: Vec<u32> = ALL_TIPS.iter().map(|t| t.id).collect();
        assert!(next_unseen(&all).is_none(), "exhausted");
    }
}
