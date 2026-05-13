use crate::config::theme::Theme;

/// Monochrome built-in theme — no colour escapes, only text attribute
/// modifiers (bold / italic / underline / reversed / dim).  Recommended
/// for terminals reporting `ColourDepth::Ansi16` or `NoColour`; selected
/// automatically on first launch when colour support is limited.
///
/// No `palette()` ctor is needed — `Theme::monochrome` builds a fully
/// populated theme directly and the dark default palette is carried
/// through unchanged for `Palette::appearance()` classification.
pub fn theme() -> Theme {
    Theme::monochrome()
}
