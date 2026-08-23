//! The "see the manual" footnote a modal appends to its body.
//!
//! Several modals end with one line pointing at the section of the
//! shipped manual that explains what the modal just said.  The line is
//! built here rather than in each modal for the usual reason: its
//! link's coordinates are `(line index, span index)` into the finished
//! body, and a modal whose body has optional paragraphs (a warning row
//! that only sometimes appears, a folded-in explanation) cannot state
//! that index up front.  Appending through one helper means the index
//! is *observed* rather than assumed, so it cannot drift out of step
//! with the body above it.
//!
//! Only modals that are **informational** carry one.  On a modal that
//! asks a question — the images / diagrams / remote-image prompts,
//! with their Yes / No / Always / Never rows — following a link would
//! close the prompt, and closing those prompts *is* an answer ("no,
//! for this session").  A reader who clicked through to the manual to
//! decide would find the decision already made for them, which is
//! worse than having no link at all.

use ratatui::text::{Line, Span};

use super::types::ModalOutcome;
use crate::config::Theme;
use crate::ui::{controls, ModalLink, ModalLinkTarget};

/// A one-line pointer into the manual, appended below a modal's body.
pub(super) struct DocsFootnote {
    /// The clickable text.  Read as the name of a manual section, so
    /// it should match that section's heading.
    pub label: &'static str,
    /// The section it opens.
    pub target: ModalLinkTarget,
    /// The rest of the sentence, following `label` verbatim — begin it
    /// with a space.
    pub trailer: &'static str,
}

impl DocsFootnote {
    /// Append a blank spacer and the footnote line to `body`, and
    /// return the link list naming the span just written.
    ///
    /// `focused_link` is the modal's current
    /// [`super::chrome::ModalChrome::focused_link`], so the label draws
    /// with the shared focus fill while the Tab ring is parked on it.
    pub(super) fn append_to(
        &self,
        body: &mut Vec<Line<'static>>,
        focused_link: Option<usize>,
        theme: &Theme,
    ) -> Vec<ModalLink> {
        body.push(Line::raw(""));
        let line_idx = body.len();
        body.push(Line::from(vec![
            Span::styled(
                self.label,
                controls::link_style(focused_link == Some(0), theme),
            ),
            Span::raw(self.trailer),
        ]));
        vec![ModalLink::new(line_idx, 0, self.target.clone(), self.label)]
    }
}

/// Close the modal and follow `target`.
///
/// The modal closes rather than staying open behind the manual: the
/// destination is a document, and an overlay left floating over the
/// page the reader just asked for covers the thing they came to read.
/// A modal with dismissal bookkeeping of its own (the capabilities
/// notice records the terminal fingerprint) builds its own outcome
/// instead, so the bookkeeping still runs.
pub(super) fn follow_and_close(target: ModalLinkTarget) -> ModalOutcome {
    ModalOutcome::CloseAnd(Box::new(move |app| app.follow_modal_link(target)))
}
