use ratatui::style::Color;

use crate::config::theme::Palette;

/// Companion light palette to [`super::default_dark`].  Same colour
/// families (warm orange brand, edamame-green success, gold chrome)
/// re-tuned for a near-white page: brights are darker and more
/// saturated so they pop against the light surface, dims are lighter
/// so they recede.  Inverse-text sites (`fg = default_bg`) on any
/// saturated brand colour rely on those colours being dark enough to
/// contrast with near-white — yellows are shifted to amber/orange-gold
/// for that reason.
pub fn palette() -> Palette {
    Palette {
        default_text: Color::Indexed(234),
        default_bg: Color::Indexed(254),

        // Orange — brand identity.
        primary_bright: Color::Indexed(166),
        primary_dim: Color::Indexed(173),

        // Cyan — emphasis.
        emphasis_bright: Color::Indexed(31),
        emphasis_dim: Color::Indexed(38),

        // Gold — structural chrome.
        structural_bright: Color::Indexed(136),
        structural_dim: Color::Indexed(94),

        // Blue — links, focus.  `interactive_dim` is pulled darker
        // than its dark-theme counterpart because `from_palette` uses
        // it as the bg for inverse-text sites
        // (`modal_input_unfocused`, `modal_item_selected`) that paint
        // `fg = default_bg`.  On a light page that fg is near-white,
        // so the bg has to be saturated/dark enough for the text to
        // read.
        interactive_bright: Color::Indexed(27),
        interactive_dim: Color::Indexed(18),
        // Selection bg is split from `interactive_dim` on the light
        // theme: a pale tint lets the near-black `default_text` read
        // through the highlight, the way selection traditionally
        // looks on a light page.
        selection_bg: Color::Indexed(153),

        // Green — success.
        success_bright: Color::Indexed(28),
        success_dim: Color::Indexed(70),

        // Amber — warnings.  Shifted darker than the dark palette's
        // yellow so inverse-text sites (highlight, raw-mode chip,
        // task_unchecked text colour) stay legible against a
        // near-white page.
        warning_bright: Color::Indexed(172),
        warning_dim: Color::Indexed(130),

        // Red — errors.
        error_bright: Color::Indexed(124),
        error_dim: Color::Indexed(167),

        // Greys — chrome.  Surfaces step *darker* than the page
        // because lifting a card off a light background reads as
        // "more grey", mirroring the dark palette's "lift = lighter".
        text_muted: Color::Indexed(244),
        muted: Color::Indexed(252),
        surface: Color::Indexed(251),
        surface_elevated: Color::Indexed(250),

        // Headings — same yellow → orange → purple → gold ramp as the
        // dark palette, with each shade pushed darker so it reads on
        // a light page.
        h1: Color::Indexed(178),
        h2: Color::Indexed(166),
        h3: Color::Indexed(91),
        h4: Color::Indexed(130),
        h5: Color::Indexed(172),
        h6: Color::Indexed(97),

        // Inline code — purple foreground on the muted light grey.
        code_bright: Color::Indexed(91),
        code_dim: Color::Indexed(97),
    }
}
