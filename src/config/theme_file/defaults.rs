use crate::config::theme::{PaletteRole, Theme, PALETTE_ROLES};

use super::ThemeFile;

/// Build the contents of `themes/default.toml` from the compiled-in
/// [`Theme::default`] and [`crate::config::theme::Palette::default`].
///
/// The output has three parts, concatenated in order:
///
/// 1. The static prose header from `default_theme_header.txt` —
///    intro, authoring instructions, merge order, override reference,
///    worked examples.  Anything that doesn't depend on the specific
///    set of [`Palette`](crate::config::theme::Palette) fields lives
///    there.
/// 2. A *generated* "Palette roles" section built from
///    [`PALETTE_ROLES`].  This is the part that used to drift from the
///    code; now adding or renaming a palette field is a tracked
///    `PALETTE_ROLES` edit, and the role-coverage test guards against
///    unlisted fields.
/// 3. A `[palette]` section in which every colour entry is *commented
///    out*, followed by bare empty per-element style sections so users
///    can discover the available override slots.
///
/// Called from [`crate::config::init::ensure_default_files_in`] on first
/// run.  There is no checked-in `config/themes/default.toml` — the file
/// is generated at startup so it can never drift from the code-side
/// defaults.
pub fn default_theme_toml() -> String {
    let theme = Theme::default();

    // Build a ThemeFile carrying the default palette + default
    // task_strikethrough plus all-empty style specs.  Serializing it
    // produces the full skeleton (palette values + bare `[<element>]`
    // headers); we then comment out every line inside `[palette]`.
    let file = ThemeFile {
        palette: (&theme.palette).into(),
        task_strikethrough: theme.task_strikethrough,
        ..ThemeFile::default()
    };
    let body = toml::to_string_pretty(&file).expect("serialize default ThemeFile");

    // A blank line or the next `[...]` header ends the palette block;
    // anything else within the block is a palette entry we comment out so
    // it documents the default without overriding it.
    let mut skeleton = String::new();
    let lines = body.lines().scan(false, |in_palette, line| {
        let was_in_palette = *in_palette;
        if was_in_palette && (line.is_empty() || line.starts_with('[')) {
            *in_palette = false;
        }
        let comment = was_in_palette && !line.is_empty() && !line.starts_with('[');
        if line == "[palette]" {
            *in_palette = true;
        }
        Some((line, comment))
    });
    for (line, comment) in lines {
        if comment {
            skeleton.push_str("# ");
        }
        skeleton.push_str(line);
        skeleton.push('\n');
    }

    let header = include_str!("../default_theme_header.txt");
    let footer = include_str!("../default_theme_footer.txt");
    let roles = render_palette_roles(PALETTE_ROLES);
    format!("{header}\n{roles}\n{footer}\n{skeleton}")
}

/// Render [`PALETTE_ROLES`] as a TOML comment block in the same visual
/// style as the rest of the static header — a section heading, a one-
/// line lead-in, then `name` / indented body for each role.  Every line
/// is prefixed with `# ` so the output is valid TOML.
fn render_palette_roles(roles: &[PaletteRole]) -> String {
    let mut out = String::new();
    out.push_str("# ─── Palette roles ────────────────────────────────────────────────────\n#\n");
    out.push_str("# Each role names a group of related palette fields and explains\n");
    out.push_str("# what UI elements draw on it.  Most colours have a `bright_*` /\n");
    out.push_str("# `dim_*` pair: the bright shade is the \"loud\" version (headings,\n");
    out.push_str("# primary chord glyph, mode chip fill); the dim shade is the\n");
    out.push_str("# \"quiet\" version (heading anchors, borders, secondary surfaces).\n");
    out.push_str("# Aim for roughly 30% lightness contrast between the two so both\n");
    out.push_str("# read on a dark surface.  A few roles (`text_muted`, `muted`,\n");
    out.push_str("# `surface`, `surface_elevated`, `headings`) don't follow the\n");
    out.push_str("# bright/dim split — their fields are listed individually below.\n#\n");

    for (idx, role) in roles.iter().enumerate() {
        if idx > 0 {
            out.push_str("#\n");
        }
        out.push_str("#   ");
        out.push_str(role.name);
        out.push('\n');
        // List the actual field names so authors can see what to edit
        // in the [palette] section below.
        out.push_str("#       fields: ");
        for (i, f) in role.fields.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(f);
        }
        out.push('\n');
        for line in role.body.lines() {
            out.push_str("#       ");
            out.push_str(line);
            out.push('\n');
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_section_mentions_every_field() {
        let rendered = render_palette_roles(PALETTE_ROLES);
        for role in PALETTE_ROLES {
            for f in role.fields {
                assert!(
                    rendered.contains(f),
                    "rendered role section is missing field `{f}`"
                );
            }
            assert!(
                rendered.contains(role.name),
                "rendered role section is missing role `{}`",
                role.name
            );
        }
    }

    #[test]
    fn rendered_section_is_all_comments() {
        // The role section sits inside a TOML file; every non-empty
        // line must start with `#` so the file still parses.
        let rendered = render_palette_roles(PALETTE_ROLES);
        for line in rendered.lines() {
            assert!(
                line.is_empty() || line.starts_with('#'),
                "non-comment line in role section: {line:?}"
            );
        }
    }

    #[test]
    fn full_default_toml_still_parses() {
        // End-to-end: header + generated roles + skeleton must produce
        // valid TOML that resolves to Theme::default().  Mirrors the
        // contract of `default_theme_toml_resolves_to_default_theme`
        // in theme_file.rs but exercises the generated-roles path.
        let s = default_theme_toml();
        let parsed: ThemeFile = toml::from_str(&s).expect("parse generated default.toml");
        let theme: Theme = (&parsed).into();
        assert_eq!(theme.h1, Theme::default().h1);
    }
}
