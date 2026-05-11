use ratatui::style::{Color, Modifier, Style};

use crate::config::theme::{Palette, Theme};

/// Companion light palette to [`super::dark_256`].  Same colour
/// families (warm orange brand, edamame-green success, cool purple
/// chrome) re-tuned for a near-white page: brand colours are darker
/// and more saturated so they pop against the light surface.
/// Inverse-text sites (`fg = bg`) on any saturated brand colour rely
/// on those colours being dark enough to contrast with near-white —
/// yellows are shifted to amber/orange-gold for that reason.
pub fn palette() -> Palette {
    Palette {
        text: Color::Indexed(234),
        text_muted: Color::Indexed(244),
        bg: Color::Indexed(254),
        bg_muted: Color::Indexed(252),
        surface: Color::Indexed(251),
        surface_elevated: Color::Indexed(250),

        // Orange — brand identity.
        primary: Color::Indexed(166),
        // Mid purple — structural chrome.  Cool hue chosen to
        // contrast with the warm `primary` orange.
        secondary: Color::Indexed(97),
        // Medium blue — list markers, table header, selection bg.
        // Pale enough to let the near-black `text` (234) read
        // through when used as selection bg, dark enough to read as
        // a fg on the near-white page.
        accent: Color::Indexed(75),
        // Saturated blue — link foreground.
        link: Color::Indexed(21),

        success: Color::Indexed(28),
        // Amber — shifted darker than the dark palette's yellow so
        // inverse-text sites (highlight, raw-mode chip,
        // task_unchecked text colour) stay legible against the
        // near-white page.
        warning: Color::Indexed(172),
        error: Color::Indexed(124),

        // Inline code — saturated purple foreground on the muted
        // light grey.  Distinct enough from the `secondary` mid
        // purple that inline-code reads as code rather than chrome.
        code: Color::Indexed(91),

        // Reserved for a future diff view; not consumed yet.
        diff_add: Color::Indexed(28),
        diff_delete: Color::Indexed(124),
    }
}

/// Built-in theme: builds a [`Theme`] from [`palette`] and pins the
/// `h1`–`h6` heading ramp to curated 256-cube shades.  See the
/// `dark_256::theme` doc for the rationale.
pub fn theme() -> Theme {
    let mut t = Theme::from_palette(&palette());
    let bold = Modifier::BOLD;
    let underline = Modifier::UNDERLINED;
    // Heading ramp: alternates primary (orange) and secondary (purple),
    // each shade pushed darker than its dark-palette counterpart so
    // it reads on a light page.
    let h1 = Color::Indexed(166); // primary, bright
    let h2 = Color::Indexed(53); // secondary, bright (dark purple)
    let h3 = Color::Indexed(130); // primary, medium
    let h4 = Color::Indexed(97); // secondary, medium
    let h5 = Color::Indexed(94); // primary, dull
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
