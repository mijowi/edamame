//! The numbers GitHub gives a document's footnote references, which heading slugs use
//! (`# Intro[^a] x` is `#intro1-x`).  The numbering rules are in `docs/dev/footnotes.md`;
//! `tests/heading_anchors.rs` pins them to GitHub's output.

use std::collections::HashMap;

use pulldown_cmark::Event;

use super::label::normalize_label;

/// Referenced footnote labels, each numbered by its first reference in source order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FootnoteNumbers {
    /// Normalized label → its 1-based number.
    numbers: HashMap<String, usize>,
}

impl FootnoteNumbers {
    /// Number every `Event::FootnoteReference` in `events`, which must be the whole document's
    /// events in order.  pulldown-cmark emits that event only for a label with a definition; an
    /// undefined `[^x]` stays text and takes no number, as on GitHub.
    pub fn from_events<'a, 'e: 'a>(events: impl IntoIterator<Item = &'a Event<'e>>) -> Self {
        let mut numbers = Self::default();
        for event in events {
            numbers.observe(event);
        }
        numbers
    }

    /// Number `event`'s label if it is a footnote reference to a label not yet seen.
    pub(crate) fn observe(&mut self, event: &Event<'_>) {
        if let Event::FootnoteReference(label) = event {
            let key = normalize_label(label);
            let next = self.numbers.len() + 1;
            self.numbers.entry(key).or_insert(next);
        }
    }

    /// The number GitHub renders a reference to `label` as.  Labels match case-insensitively,
    /// as pulldown-cmark resolves them.
    pub fn get(&self, label: &str) -> Option<usize> {
        self.numbers.get(&normalize_label(label)).copied()
    }
}

#[cfg(test)]
mod tests {
    use pulldown_cmark::{Options, Parser};

    use super::*;

    fn numbers(md: &str) -> FootnoteNumbers {
        let events: Vec<Event> = Parser::new_ext(md, Options::ENABLE_FOOTNOTES).collect();
        FootnoteNumbers::from_events(&events)
    }

    /// First reference wins the number; repeats and case variants reuse it.
    #[test]
    fn numbers_by_first_reference_in_source_order() {
        let n = numbers("x[^b] y[^a] z[^B][^missing]\n\n[^a]: A.\n[^b]: B.\n");
        assert_eq!(n.get("b"), Some(1));
        assert_eq!(n.get("a"), Some(2));
        assert_eq!(n.get("B"), Some(1));
        assert_eq!(n.get("missing"), None, "undefined references stay text");
    }

    /// A reference inside a definition is numbered where the definition sits in the source.
    #[test]
    fn a_reference_inside_a_definition_numbers_in_place() {
        let n = numbers("[^z]: z[^in].\n\nBody[^z].\n\n[^in]: i.\n");
        assert_eq!(n.get("in"), Some(1));
        assert_eq!(n.get("z"), Some(2));
    }
}
