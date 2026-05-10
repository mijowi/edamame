use ratatui::style::Color;

use crate::config::theme::Palette;

/// Edamame's default palette: warm orange brand on a near-black
/// background, with a fresh edamame-bean green for success and a
/// complementary lavender for chrome.  Bright/dim pairs are tuned for
/// ~30% lightness contrast so both variants read on a dark surface.
pub fn palette() -> Palette {
    Palette {
        default_text: Color::Indexed(253),
        default_bg: Color::Indexed(233),

        // Orange — brand identity, headings, mode chip.
        primary_bright: Color::Indexed(208),
        primary_dim: Color::Indexed(172),

        // Blue — emphasis
        emphasis_bright: Color::Indexed(117),
        emphasis_dim: Color::Indexed(45),

        // Gold — structural chrome (frames, dividers, asides).
        structural_bright: Color::Indexed(136),
        structural_dim: Color::Indexed(94),

        // Blue — links, focus.
        interactive_bright: Color::Indexed(39),
        interactive_dim: Color::Indexed(25),
        // Selection bg coincides with interactive_dim on the dark
        // theme: dark blue carries both light text (inverse-text
        // sites) and the near-white default_text well.
        selection_bg: Color::Indexed(25),

        // Green — success, completed tasks, edamame.
        success_bright: Color::Indexed(76),
        success_dim: Color::Indexed(28),

        // Yellow — warnings.
        warning_bright: Color::Indexed(220),
        warning_dim: Color::Indexed(178),

        // Red — errors.
        error_bright: Color::Indexed(196),
        error_dim: Color::Indexed(124),

        // Greys — UI chrome and muted items
        text_muted: Color::Indexed(245), // Muted text, e.g. strikethrough
        muted: Color::Indexed(235),      // Muted background, e.g. table row stripes
        surface_elevated: Color::Indexed(237), // Elevated surface, e.g. dialogs
        surface: Color::Indexed(236),    // Surface, e.g. panels, dialogs

        // Headings — bright color 1/2/3, dim color 1/2/3
        h1: Color::Indexed(220),
        h2: Color::Indexed(208),
        h3: Color::Indexed(135),
        h4: Color::Indexed(136),
        h5: Color::Indexed(172),
        h6: Color::Indexed(140),

        // Inline code and code block language line
        code_bright: Color::Indexed(140),
        code_dim: Color::Indexed(60),
    }
}
