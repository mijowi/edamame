use ratatui::style::{Color, Modifier, Style};

use crate::config::theme::{Palette, Theme};

/// Edamame's default palette: warm orange brand on a near-black
/// background, with a fresh edamame-bean green for success and a
/// cool purple for chrome (contrasting with the warm `primary` so the
/// alternating heading ramp reads as two distinct families).
pub fn palette() -> Palette {
    Palette {
        text: Color::Indexed(253),
        text_muted: Color::Indexed(245),
        bg: Color::Indexed(233),
        bg_muted: Color::Indexed(235),
        surface: Color::Indexed(236),
        surface_elevated: Color::Indexed(237),

        // Orange — brand identity, headings, mode chip.
        primary: Color::Indexed(208),
        // Mid purple — structural chrome (rules, blockquote bar,
        // section headings, search-highlight bg).  Cool hue chosen
        // to contrast with the warm `primary` orange.
        secondary: Color::Indexed(97),
        // Mid blue — list markers, table header, selection bg.
        // Dark enough to carry `text` (253) as a fg when used as
        // selection bg, saturated enough to read as a fg colour on
        // the document surface.
        accent: Color::Indexed(25),
        // Bright blue — link foreground.
        link: Color::Indexed(39),

        success: Color::Indexed(76),
        warning: Color::Indexed(220),
        error: Color::Indexed(196),

        // Inline code and code-block language line.  Lighter purple
        // than `secondary` so inline-code reads distinct from
        // chrome / section-heading colour.
        code: Color::Indexed(140),

        // Reserved for a future diff view; not consumed yet.
        diff_add: Color::Indexed(76),
        diff_delete: Color::Indexed(196),
    }
}

/// Built-in theme: builds a [`Theme`] from [`palette`] and pins the
/// `h1`–`h6` heading ramp to curated 256-cube shades.  The default
/// `Theme::from_palette` darkening only works for RGB colours;
/// stepping indexed colours through the 6×6×6 cube shifts hue, so
/// 256-colour built-ins curate the ramp explicitly.
pub fn theme() -> Theme {
    let mut t = Theme::from_palette(&palette());
    let bold = Modifier::BOLD;
    let underline = Modifier::UNDERLINED;
    // Heading ramp: alternates primary (orange) and secondary (purple),
    // dulling / darkening with each level.
    let h1 = Color::Indexed(208); // primary, bright
    let h2 = Color::Indexed(99); // secondary, bright violet
    let h3 = Color::Indexed(172); // primary, medium
    let h4 = Color::Indexed(97); // secondary, medium
    let h5 = Color::Indexed(130); // primary, dull
    let h6 = Color::Indexed(60); // secondary, dull
    t.h1 = Style::default().fg(h1).add_modifier(bold);
    t.h1_rule = Style::default().fg(h1);
    t.h2 = Style::default().fg(h2).add_modifier(bold | underline);
    t.h3 = Style::default().fg(h3).add_modifier(bold | underline);
    t.h4 = Style::default().fg(h4).add_modifier(bold | underline);
    t.h5 = Style::default().fg(h5).add_modifier(bold | underline);
    t.h6 = Style::default().fg(h6).add_modifier(bold | underline);
    t
}
