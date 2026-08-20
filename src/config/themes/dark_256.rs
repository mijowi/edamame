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
        // selection bg, saturated enough to read as a fg color on
        // the document surface.
        accent: Color::Indexed(25),
        // Bright blue — link foreground.
        link: Color::Indexed(39),

        success: Color::Indexed(76),
        warning: Color::Indexed(220),
        error: Color::Indexed(196),

        // Inline code and code-block language line.  Lighter purple
        // than `secondary` so inline-code reads distinct from
        // chrome / section-heading color.
        code: Color::Indexed(140),

        // Reserved for a future diff view; not consumed yet.  Picked
        // distinct from `success` / `error` so the palette has no
        // duplicate slot values.
        diff_add: Color::Indexed(34),
        diff_delete: Color::Indexed(160),

        light: false,
    }
}

/// Built-in theme: builds a [`Theme`] from [`palette`] and pins the
/// `h1`–`h6` heading ramp to curated 256-cube shades.  The default
/// `Theme::from_palette` darkening only works for RGB colors;
/// stepping indexed colors through the 6×6×6 cube shifts hue, so
/// 256-color built-ins curate the ramp explicitly.
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

    // Code surface: a slightly-lifted neutral grey distinct from
    // `bg_muted` (235, the striped-row bg) so a code span inside a
    // stripe still reads as code.  Indexed-cube stepping of `code`
    // (140, mid purple) shifts hue, so we pick the shade by hand.
    let code_bg = Color::Indexed(238);
    t.code_span = Style::default().fg(palette().code).bg(code_bg);
    t.code_span_dim = Style::default()
        .fg(palette().code)
        .bg(code_bg)
        .add_modifier(Modifier::DIM);
    t.code_block_border = Style::default().fg(palette().text).bg(code_bg);
    t.code_block_text = Style::default().fg(palette().text).bg(code_bg);

    // Blockquote surface: the faintest lift the greyscale ramp offers
    // over `bg` (233) — one step below the striped-row bg (235) and
    // well below the code surface (238), so a code span inside a quote
    // still reads as code.  Derived from `secondary` (97, mid purple)
    // via `blend`, which is a no-op for indexed colors and would have
    // left the wash as that full-strength purple.
    t.blockquote_text = Style::default().bg(Color::Indexed(234));

    // Muted selection (non-focused search matches).  The derived
    // version blends `surface` toward `accent`, which is a no-op for
    // indexed colors — it would leave the highlight as the bare
    // `surface` grey (236), barely separable from `bg` (233).  Pick a
    // dark navy instead: clearly a highlight against the near-black
    // page, and clearly recessive against the focused match's brighter
    // `accent` blue (25).
    let selection_muted_bg = Color::Indexed(18);
    t.selection_muted = Style::default().bg(selection_muted_bg).fg(palette().text);

    // Diff washes.  Same `blend` no-op as `selection_muted`, but worse:
    // every `diff_*_line` / `diff_*_inline` style collapses to the bare
    // `surface` grey, and `diff_view` paints add / delete rows with *no
    // gutter* — "distinguished by background color alone" — so on an
    // indexed terminal an addition and a deletion render identically.
    //
    // Two things constrain the shades.  (1) A line wash sits behind
    // whatever the markdown painted, so heading orange / code purple
    // have to stay legible on top of it; only the cube's darkest tints
    // leave them room.  (2) The cube's greens carry far more luminance
    // than its reds: against `text` (253), 22 measures 5.7:1 but 28 only
    // 3.4:1 and 34 just 2.1:1, while 52 / 88 / 124 all stay above 5:1.
    //
    // So the washes take the faintest hued shade of each hue — 22 / 52,
    // which are also the darkest the 6×6×6 cube can express (nothing
    // sits between them and black in-hue).  That leaves no second wash
    // level for green, so focused and non-focused rows share a wash and
    // focus is carried instead by the inline highlights below and by the
    // decision divider (elevated bg, `>` caret, bold).  Prioritising the
    // wash hierarchy over legibility would mean 28 behind body text.
    let add_wash = Color::Indexed(22); // #005f00
    let delete_wash = Color::Indexed(52); // #5f0000
    t.diff_add_line = Style::default().bg(add_wash);
    t.diff_add_line_unfocused = Style::default().bg(add_wash);
    t.diff_delete_line = Style::default().bg(delete_wash);
    t.diff_delete_line_unfocused = Style::default().bg(delete_wash);

    // Inline (within-line) change highlights sit *on* the wash, so they
    // step one shade deeper; the non-focused pair drops the bold, as in
    // the derived styles.  The focused pair is a saturated fill, and
    // pins the foreground `best_contrast` would have picked if it could
    // measure indexed colors — near-black on the bright green (6.4:1),
    // `text` on the dark red (5.3:1).
    t.diff_add_inline_unfocused = Style::default().bg(Color::Indexed(28));
    t.diff_delete_inline_unfocused = Style::default().bg(Color::Indexed(88));
    t.diff_add_inline = Style::default()
        .bg(Color::Indexed(34))
        .fg(palette().bg)
        .add_modifier(Modifier::BOLD);
    t.diff_delete_inline = Style::default()
        .bg(Color::Indexed(124))
        .fg(palette().text)
        .add_modifier(Modifier::BOLD);

    // Bottom region in diff mode — green tint on the status line, red on
    // the hint line, mirroring the adds-below / deletes-above stacking.
    // Reuses the faint wash shades so the bars read as the same language
    // as the document rows.
    t.status_bar_diff = Style::default().bg(add_wash).fg(palette().text);
    t.hint_bar_diff = Style::default().bg(delete_wash).fg(palette().text);
    t
}
