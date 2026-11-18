use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use ratatui::style::{Color, Modifier, Style};

use super::sections::AppearanceMode;
use super::themes::util::{best_contrast, blend, legible_on};

/// How heavily to mix `code` toward `bg` for the code surface bg.  1.0 = plain `bg`.
const CODE_BG_MIX_TOWARD_BG: f32 = 0.92;

/// How heavily to mix `secondary` toward `bg` for the blockquote surface bg.  Quieter
/// than the code surface: a quote is prose, carrying emphasis, links and code spans that
/// must stay readable on top of it.
const QUOTE_BG_MIX_TOWARD_BG: f32 = 0.94;

/// WCAG contrast floor a `syntax_*` foreground must clear against the code surface.  The
/// 4.5:1 body-text threshold, not the 3:1 UI one: a code token is read character by
/// character, and the plain `code_block_text` it replaces already clears 4.5 everywhere,
/// so less would mean highlighting *lowered* legibility.
///
/// Enforced only where measurable — `legible_on` passes non-RGB colors through, so the
/// indexed built-ins answer for their own numbers.
const SYNTAX_MIN_CONTRAST: f32 = 4.5;

/// Darken `base` by `level` steps for the heading ramp.  Indexed and named colors can't
/// be stepped without shifting hue, so they pass through unchanged and the indexed
/// built-ins pin `h1`–`h6` by hand.
fn dim_color(base: Color, level: u8) -> Color {
    if level == 0 {
        return base;
    }
    match base {
        Color::Rgb(r, g, b) => {
            let factor = 1.0 - 0.18 * level as f32;
            let scale = |c: u8| (c as f32 * factor).clamp(0.0, 255.0) as u8;
            Color::Rgb(scale(r), scale(g), scale(b))
        }
        _ => base,
    }
}

/// Every styled element in the UI, as a precomputed [`Style`] derived from a [`Palette`].
/// User theme files may override the palette, individual styles, or both; see
/// [`super::theme_file`] for the format and merge order, and `docs/dev/theming.md` for
/// the conventions.
///
/// No hardcoded colors exist outside [`Palette::default`] — every UI site reads
/// `theme.<field>`.  Focus / active / disabled affordances layer modifiers (BOLD,
/// REVERSED, DIM) rather than reaching for a second palette slot.
#[derive(Debug, Clone)]
pub struct Theme {
    /// The palette every style is derived from.  Kept on the theme so UI code can reach
    /// for e.g. `bg` as a foreground against a colored fill.
    pub palette: Palette,

    // ── Headings ──────────────────────────────────────────────────
    pub h1: Style,
    pub h1_rule: Style,
    pub h2: Style,
    pub h3: Style,
    pub h4: Style,
    pub h5: Style,
    pub h6: Style,

    // ── Inline formatting ─────────────────────────────────────────
    pub bold: Style,
    pub italic: Style,
    pub strikethrough: Style,
    pub highlight: Style,
    pub code_span: Style,
    /// Inline code inside strikethrough text: [`Self::code_span`] + DIM, so it reads as
    /// struck through without losing the code-span affordance.
    pub code_span_dim: Style,
    /// Web link — `link` fg + underline.
    pub link_text: Style,
    /// File link.  Same shade as `link_text` by default; themes may override.
    pub link_file: Style,
    /// In-document heading link (`#section`) — `link` fg, no underline.
    pub link_heading: Style,
    pub image_placeholder: Style,
    /// Footnote chrome — the bracketed reference marker and a definition's leader glyph.
    pub footnote: Style,

    // ── Block elements ────────────────────────────────────────────
    pub code_block_border: Style,
    pub code_block_lang: Style,
    pub code_block_text: Style,

    // ── Syntax highlighting (fenced code block bodies) ────────────
    //
    // One field per `markdown::highlight::TokenClass`, each *patched over*
    // `code_block_text`.  A field must say only what differs from the code surface and
    // must not restate the background, or a theme that changes `code_block_text`'s bg
    // gets a patchwork of stale ones.  Unclassified text keeps `code_block_text`, so an
    // unknown language renders exactly as it did before highlighting existed.
    /// Control flow, declarations, storage modifiers (`fn`, `if`, `pub`).
    pub syntax_keyword: Style,
    /// String and character literals, including their delimiters.
    pub syntax_string: Style,
    /// Line and block comments.
    pub syntax_comment: Style,
    /// Numeric literals and language constants (`42`, `true`, `nil`).
    pub syntax_number: Style,
    /// Type, class and interface *names* — not the declaring keywords.
    pub syntax_type: Style,
    /// Function and method names, at definition and call sites.
    pub syntax_function: Style,
    /// Markup tags, attribute names, preprocessor directives.
    pub syntax_attribute: Style,

    pub blockquote_bar: Style,
    pub blockquote_text: Style,
    pub rule: Style,

    // ── Frontmatter (YAML / TOML metadata block) ──────────────────
    /// The `---` / `+++` delimiter lines around a metadata block.
    pub frontmatter_delimiter: Style,
    /// The `key:` / `key =` half of a frontmatter line.
    pub frontmatter_key: Style,
    /// The value half, and any line the key/value split doesn't apply to.
    pub frontmatter_value: Style,

    // ── List markers ──────────────────────────────────────────────
    pub list_bullet: Style,
    pub list_number: Style,

    // ── Task list ─────────────────────────────────────────────────
    pub task_unchecked: Style,
    /// The `[✓]` marker for checked items.
    pub task_checked: Style,
    /// The *text* of a completed task.  Distinct from `task_checked` so the marker can
    /// stay green while the text fades.
    pub task_complete_text: Style,
    /// Whether checked item text is struck through.
    pub task_strikethrough: bool,

    // ── Table ─────────────────────────────────────────────────────
    pub table_border: Style,
    pub table_header: Style,
    pub table_header_border: Style,
    pub table_cell: Style,
    /// Fill for even-numbered data rows (row 0 = first data row).  Applied only under
    /// `config.table.row_striping`; the default is bare so opting out changes nothing.
    pub table_row_even: Style,
    /// Fill for odd-numbered data rows.  See `table_row_even`.
    pub table_row_odd: Style,
    /// The drop separator the pointer is currently over during a row / column drag.
    /// Painted as a post-pass over the border, so no buffer mutation is needed.
    pub table_drop_indicator: Style,
    /// The *inert* drop-target separators during that drag — every valid site, with the
    /// hovered one upgrading to `table_drop_indicator`.
    pub table_drop_target: Style,
    /// Reorder (`⠿`) and column-resize (`⇔`) glyphs on the table border.  Distinct from
    /// `table_border` so they read as interactive rather than chrome.
    pub table_handle: Style,
    /// Delete (`✕`) glyphs on the table border; distinct from `table_handle` so the
    /// destructive affordance reads as a warning.
    pub table_handle_delete: Style,

    // ── Status bar ────────────────────────────────────────────────
    pub status_bar: Style,
    pub status_mode_preview: Style,
    pub status_mode_rendered: Style,
    pub status_mode_raw: Style,
    /// Vim NORMAL (and Operator-pending) sub-mode badge.  These three `status_mode_vim_*`
    /// fields are the canonical per-vim-mode colors: the status chip uses them directly
    /// and the editor cursor mirrors them (dropping the BOLD), so the two cannot drift.
    /// RAW within INSERT is signalled by `status_mode_raw` instead.
    pub status_mode_vim_normal: Style,
    /// Vim INSERT badge, and the INSERT cursor except in Raw view.
    pub status_mode_vim_insert: Style,
    /// Vim VISUAL / V-LINE badge (and cursor color).
    pub status_mode_vim_visual: Style,
    pub status_filename: Style,
    pub status_info: Style,
    pub status_modified: Style,
    /// The `›` separator in the section-path breadcrumb.
    pub status_breadcrumb_sep: Style,
    /// Every breadcrumb segment except the deepest; dimmed so the deepest reads as the
    /// "you are here" anchor.
    pub status_breadcrumb_ancestor: Style,
    /// The deepest breadcrumb segment — the heading directly containing the cursor.
    pub status_breadcrumb_current: Style,

    // ── Hint line ─────────────────────────────────────────────────
    /// Base background/foreground for the contextual hint line.
    pub hint_bar: Style,
    /// Chord glyph (the `^C` in `^C Copy`).
    pub hint_chord: Style,
    /// Label (the `Copy` in `^C Copy`); blends into the hint_bar fill.
    pub hint_label: Style,

    // ── Transient messages ────────────────────────────────────────
    pub transient_info: Style,
    pub transient_success: Style,
    pub transient_warning: Style,
    /// Error notification — sticky, dismissed with Escape.
    pub transient_error: Style,

    // ── Modal popups ──────────────────────────────────────────────
    /// Background fill for modal bodies.  Its own field so themes can give modals a
    /// surface distinct from the status bar.
    pub modal_bg: Style,
    /// Title style for `ModalKind::Normal`.
    pub modal_title_normal: Style,
    /// Title style for `ModalKind::Warning`.
    pub modal_title_warning: Style,
    /// Title style for `ModalKind::Error`.
    pub modal_title_error: Style,
    /// The `esc` close hint on a dismissable modal's title row; doubles as the visible
    /// affordance for the clickable close button.
    pub modal_close_hint: Style,
    /// Unfocused row in a list-style modal.
    pub modal_item: Style,
    /// Right-aligned hint / sub-label on an unfocused row.
    pub modal_item_hint: Style,
    /// Selected row in a list-style modal; filled, so it reads as the focus.
    pub modal_item_selected: Style,
    /// A persistent selection that does NOT have focus — an active pill or checked toggle
    /// in an unfocused row.  Outlined (`secondary` fg, no fill) against the focused
    /// affordance's `primary` fill; see `docs/dev/theming.md` §"Focus vs. persistent
    /// selection" for the three-tier convention and the monochrome fallback.  On a
    /// composite affordance apply it to the selection glyph only, not the whole row.
    pub modal_item_selected_unfocused: Style,
    /// Right-aligned hint / sub-label on the focused row.
    pub modal_item_selected_hint: Style,
    /// Pinned-footer description of the focused row.  Sits on the modal body's surface
    /// rather than the row's selection fill, hence its own field.
    pub modal_description: Style,
    /// Section heading inside a modal (`— Editor —` in the keybinds overlay).
    pub modal_section_heading: Style,
    /// Text input, unfocused.
    pub modal_input_unfocused: Style,
    /// Text input, focused for typing.
    pub modal_input_focused: Style,
    /// Modal button when focused for activation.
    pub modal_button_focused: Style,

    // ── General text ──────────────────────────────────────────────
    pub normal: Style,

    /// Fill behind an active text selection.  Layers over the character's own style so
    /// color-coded content stays legible.
    pub selection: Style,

    /// Washed-out `selection`, for non-focused search matches so the current match stands
    /// out among its siblings.
    pub selection_muted: Style,

    /// Status-bar match counter (`i/n`) badge during a search flow.
    pub status_mode_search: Style,

    /// Fill for the cursor's line.  Bare by default — the active-line highlight is
    /// deferred; the field exists so themes can opt in early.
    pub active_line: Style,

    /// Block cursor for every modal text input.  Distinct from the editor cursors, which
    /// derive from the per-mode status chip.
    pub cursor: Style,

    /// Line-number gutter.
    pub line_number: Style,

    /// Scrollbar track, painted only when the content overflows.
    pub scrollbar_track: Style,
    /// Scrollbar thumb.
    pub scrollbar_thumb: Style,
    /// Scrollbar thumb while hovering the gutter or dragging.
    pub scrollbar_thumb_active: Style,

    // ── Diff mode ─────────────────────────────────────────────────
    /// Full-row fill on add-side diff lines.
    ///
    /// **Set a `bg` (and modifiers), not an `fg`.**  The wash is reused as the Accept
    /// chip's background, and `ui::diff_view::prompt_chip_style` pins that chip's
    /// foreground from `normal` — so an `fg` set here reaches the row but is dropped on
    /// the chip.  A convention, not an invariant: the field is user-authorable, since
    /// `blend` is a no-op on non-RGB colors and a hand-picked `bg` is an indexed palette's
    /// only way to get a focused fill.
    pub diff_add_line: Style,
    /// Full-row fill on delete-side diff lines.  Background-only by the same convention as
    /// `diff_add_line`, and reused as the Reject chip's background.
    pub diff_delete_line: Style,
    /// Add-side fill for non-focused hunks — weaker, so the focused hunk stands out.
    pub diff_add_line_unfocused: Style,
    /// Delete-side fill for non-focused hunks.
    pub diff_delete_line_unfocused: Style,
    /// Word-level highlight inside a focused add line; darker than `diff_add` so light
    /// text keeps contrast.
    pub diff_add_inline: Style,
    /// Word-level highlight inside a focused delete line.
    pub diff_delete_inline: Style,
    /// Word-level add highlight in a non-focused hunk — a muted tint, no bold, so it
    /// matches the faint line wash instead of popping at full saturation.
    pub diff_add_inline_unfocused: Style,
    /// Word-level delete highlight in a non-focused hunk.
    pub diff_delete_inline_unfocused: Style,
    /// Decision divider for the focused hunk while `Pending` — the `> [ ] Reject [n]
    /// Accept [y]` prompt.  The only state the prompt renders in, which is why the
    /// render-time Accept / Reject chips never land on a resolved divider's green/red
    /// foreground.
    pub diff_decision_pending: Style,
    /// Decision divider once `Accepted`.
    pub diff_decision_accepted: Style,
    /// Decision divider once `Rejected`.
    pub diff_decision_rejected: Style,
    /// Decision divider for non-focused hunks — a recessive chrome strip, used as-is while
    /// `Pending`.  Once resolved, `build_line` keeps this background but swaps in the
    /// per-state hue and adds DIM, so the decision still reads by color.
    pub diff_decision_unfocused: Style,
    /// Mode badge for `Mode::Diff`.
    pub status_mode_diff: Style,
    /// The whole status bar shifts color in diff mode so the change can't be missed.
    pub status_bar_diff: Style,
    /// Hint bar in diff mode — the status-bar hue, softer so the text stays readable.
    pub hint_bar_diff: Style,
}

/// The semantic color palette every theme is built from.
///
/// `text` / `bg` are concrete colors rather than terminal defaults because they serve as
/// *foregrounds* in inverse contexts (the Rendered mode chip is `primary` bg with `bg`
/// fg), where `Color::Reset` would give the wrong contrast.
#[derive(Debug, Clone)]
pub struct Palette {
    /// Default document foreground.
    pub text: Color,
    /// Peripheral / de-emphasized text.
    pub text_muted: Color,
    /// Default document background.
    pub bg: Color,
    /// Muted surface for table-row stripes and the scrollbar track.  Code uses a shade
    /// derived from [`Self::code`] instead, so a code span on a striped row still reads
    /// as code.
    pub bg_muted: Color,
    /// Lighter chrome surface (status bar).
    pub surface: Color,
    /// Heavier chrome surface (hint line, transient messages, modal body), so those read
    /// as lifted from both the document and the status bar.
    pub surface_elevated: Color,

    /// Brand color: headings, and non-link focus affordances.
    pub primary: Color,
    /// Structural chrome: section headings, rules, blockquote bar, footnote markers.
    pub secondary: Color,
    /// Accent: list markers, table header, and the text-selection fill.
    pub accent: Color,
    /// Link foreground.  Reserved for link affordances only.
    pub link: Color,

    pub success: Color,
    pub warning: Color,
    pub error: Color,

    /// Inline-code and code-block-language foreground.
    pub code: Color,

    /// Base hue for added diff lines; [`Theme::from_palette`] derives every focused /
    /// unfocused line and inline wash from it.
    pub diff_add: Color,
    /// Base hue for removed diff lines.
    pub diff_delete: Color,

    /// Whether this is a light-mode theme, driving the picker's filter.  Explicit rather
    /// than inferred from `bg` luminance, so a mid-grey or indexed background still
    /// classifies unambiguously.  User TOML themes opt in with `light = true`.
    pub light: bool,
}

impl Palette {
    /// Classification for the theme picker's light/dark filter; reads [`Palette::light`].
    pub fn appearance(&self) -> AppearanceMode {
        if self.light {
            AppearanceMode::Light
        } else {
            AppearanceMode::Dark
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        super::themes::dark_256::palette()
    }
}

/// Constructor for a built-in theme.  Returns a full [`Theme`] rather than a [`Palette`]
/// so it can pin the `h1`–`h6` ramp to curated shades: [`Theme::from_palette`] derives the
/// ramp algorithmically, which works for RGB but shifts hue on the 6×6×6 indexed cube.
pub type ThemeCtor = fn() -> Theme;

/// Built-in themes, in the settings overlay's cycle order.  These names are reserved: a
/// user `themes/<name>.toml` shadowing one is ignored at load time.
pub const BUILTIN_THEMES: &[(&str, ThemeCtor)] = &[
    ("256 Dark", super::themes::dark_256::theme),
    ("256 Light", super::themes::light_256::theme),
    ("Monochrome Dark", super::themes::monochrome_dark::theme),
    ("Ayu", super::themes::ayu::theme),
    ("Catppuccin", super::themes::catppuccin::theme),
    ("Catppuccin Latte", super::themes::catppuccin_latte::theme),
    ("Dracula", super::themes::dracula::theme),
    ("Edamame", super::themes::edamame::theme),
    ("Everforest", super::themes::everforest::theme),
    ("GitHub Dark", super::themes::github_dark::theme),
    ("GitHub Light", super::themes::github_light::theme),
    ("Gruvbox", super::themes::gruvbox::theme),
    ("Gruvbox Light", super::themes::gruvbox_light::theme),
    ("Kanagawa", super::themes::kanagawa::theme),
    ("Monokai", super::themes::monokai::theme),
    ("Nord", super::themes::nord::theme),
    ("One Dark", super::themes::one_dark::theme),
    ("Orng", super::themes::orng::theme),
    ("Rainbow", super::themes::rainbow::theme),
    ("Rosé Pine", super::themes::rose_pine::theme),
    ("Rosé Pine Dawn", super::themes::rose_pine_dawn::theme),
    ("Solarized Dark", super::themes::solarized_dark::theme),
    ("Solarized Light", super::themes::solarized_light::theme),
    ("SynthWave '84", super::themes::synthwave84::theme),
    ("Tokyo Night", super::themes::tokyo_night::theme),
    ("Tokyo Night Day", super::themes::tokyo_night_day::theme),
    ("Zenburn", super::themes::zenburn::theme),
];

/// `(dark, light)` pairings between variants of the same theme brand, consulted when the
/// user flips appearance mode.  [`counterpart_theme`] checks both directions.
pub const THEME_COUNTERPARTS: &[(&str, &str)] = &[
    ("256 Dark", "256 Light"),
    ("Catppuccin", "Catppuccin Latte"),
    ("GitHub Dark", "GitHub Light"),
    ("Gruvbox", "Gruvbox Light"),
    ("Rosé Pine", "Rosé Pine Dawn"),
    ("Solarized Dark", "Solarized Light"),
    ("Tokyo Night", "Tokyo Night Day"),
];

/// Default theme name when the user toggles mode to Dark and the
/// previously-active theme has no counterpart.
pub const DEFAULT_DARK_THEME: &str = "Edamame";

/// Default theme name when the user toggles mode to Light and the
/// previously-active theme has no counterpart.
pub const DEFAULT_LIGHT_THEME: &str = "256 Light";

/// The two built-ins authored against the xterm-256 cube.  Every RGB built-in picks
/// 24-bit colors an indexed terminal quantizes, often to identical fg/bg, so these are the
/// substitution *targets* below truecolor.  See [`indexed_fallback_theme`].
pub const INDEXED_DARK_THEME: &str = "256 Dark";
pub const INDEXED_LIGHT_THEME: &str = "256 Light";

/// The built-in that resolves every palette slot to [`Color::Reset`],
/// deferring entirely to the terminal's own colors.
pub const MONOCHROME_THEME: &str = "Monochrome Dark";

/// Built-ins that render correctly without 24-bit color, so a sub-truecolor terminal must
/// neither substitute nor warn about them.  [`MONOCHROME_THEME`] is safe at *every* depth
/// including `NoColor`.  `indexed_safe_themes_are_registered` pins membership so a rename
/// can't quietly make one substitutable again.
pub const INDEXED_SAFE_THEMES: &[&str] =
    &[INDEXED_DARK_THEME, INDEXED_LIGHT_THEME, MONOCHROME_THEME];

/// The indexed theme to substitute on a terminal without 24-bit color, or `None` when
/// `current` is already [`INDEXED_SAFE_THEMES`] — which is what makes the substitution
/// idempotent across reloads.
///
/// The dark/light choice follows the *current theme's* appearance, so a capability
/// downgrade never flips a light theme to a dark one; `configured` is the fallback for a
/// theme that can't be classified (a deleted user theme file).
pub fn indexed_fallback_theme(current: &str, configured: AppearanceMode) -> Option<&'static str> {
    if INDEXED_SAFE_THEMES.contains(&current) {
        return None;
    }
    Some(match theme_appearance(current).unwrap_or(configured) {
        AppearanceMode::Dark => INDEXED_DARK_THEME,
        AppearanceMode::Light => INDEXED_LIGHT_THEME,
    })
}

/// The cross-mode sibling of `name` from [`THEME_COUNTERPARTS`], in either direction.
pub fn counterpart_theme(name: &str) -> Option<&'static str> {
    for (a, b) in THEME_COUNTERPARTS {
        if *a == name {
            return Some(b);
        }
        if *b == name {
            return Some(a);
        }
    }
    None
}

/// The theme to preview when appearance mode flips to `target`: `current`'s counterpart
/// if it classifies as `target`, else the mode's default.  Shared by the theme picker and
/// the settings overlay so both previews agree.
pub fn resolve_theme_for_mode_switch(current: &str, target: AppearanceMode) -> String {
    if let Some(sibling) = counterpart_theme(current) {
        if theme_appearance(sibling) == Some(target) {
            return sibling.to_owned();
        }
    }
    match target {
        AppearanceMode::Dark => DEFAULT_DARK_THEME.to_owned(),
        AppearanceMode::Light => DEFAULT_LIGHT_THEME.to_owned(),
    }
}

/// A user theme's cached classification, keyed on mtime so a mid-session edit invalidates.
type AppearanceCacheEntry = (Option<SystemTime>, AppearanceMode);

/// Keyed by theme name; built-ins never consult it.
static USER_THEME_APPEARANCE_CACHE: Mutex<Option<HashMap<String, AppearanceCacheEntry>>> =
    Mutex::new(None);

/// Resolve `name` to its [`AppearanceMode`], or `None` for an unknown name or malformed
/// user TOML (callers then default to `Dark`, so the theme stays visible somewhere).
///
/// User themes hit [`USER_THEME_APPEARANCE_CACHE`]: the picker's filter calls this once
/// per theme on every mode flip, and re-parsing each TOML that often is not free.
pub fn theme_appearance(name: &str) -> Option<AppearanceMode> {
    if let Some(t) = Theme::builtin(name) {
        return Some(t.palette.appearance());
    }
    let dir = super::config::Config::config_dir()?;
    let path = dir.join("themes").join(format!("{name}.toml"));
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

    let mut guard = USER_THEME_APPEARANCE_CACHE.lock().ok()?;
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some((cached_mtime, cached_mode)) = cache.get(name) {
        if *cached_mtime == mtime {
            return Some(*cached_mode);
        }
    }
    // Classification is best-effort: parse warnings are not surfaced here.
    let text = std::fs::read_to_string(&path).ok()?;
    let file: super::theme_file::ThemeFile = toml::from_str(&text).ok()?;
    let theme: Theme = (&file).into();
    let mode = theme.palette.appearance();
    cache.insert(name.to_owned(), (mtime, mode));
    Some(mode)
}

/// [`list_theme_names`] filtered to `mode`.  Unresolvable themes count as `Dark` so they
/// remain visible somewhere rather than silently disappearing.
pub fn list_theme_names_for_mode(mode: AppearanceMode) -> Vec<String> {
    list_theme_names()
        .into_iter()
        .filter(|name| theme_appearance(name).unwrap_or(AppearanceMode::Dark) == mode)
        .collect()
}

/// [`BUILTIN_THEMES`] in declared order, then any `<config_dir>/themes/*.toml` stems that
/// don't shadow a built-in.
pub fn list_theme_names() -> Vec<String> {
    let mut out: Vec<String> = BUILTIN_THEMES
        .iter()
        .map(|(n, _)| (*n).to_owned())
        .collect();

    // A `--no-config` run offers built-ins only: this list feeds the picker, the settings
    // cycle, and the export-theme source list, so without the gate a session started to
    // rule the user's config out could still load a `themes/*.toml`.
    let user_themes_dir = super::config::Config::config_dir()
        .filter(|_| super::persistence::config_reads_allowed())
        .map(|dir| dir.join("themes"));

    if let Some(themes) = user_themes_dir {
        if let Ok(read) = std::fs::read_dir(&themes) {
            let mut user: Vec<String> = read
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                        return None;
                    }
                    let stem = path.file_stem().and_then(|s| s.to_str())?.to_owned();
                    if out.contains(&stem) {
                        return None;
                    }
                    if stem == "default" {
                        return None;
                    }
                    Some(stem)
                })
                .collect();
            user.sort();
            user.dedup();
            out.extend(user);
        }
    }
    out
}

impl Theme {
    /// `None` for names outside [`BUILTIN_THEMES`], where the caller falls back to
    /// reading `themes/<name>.toml`.
    pub fn builtin(name: &str) -> Option<Theme> {
        BUILTIN_THEMES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, ctor)| ctor())
    }
}

impl Theme {
    /// The single source of truth for palette-slot → style assignments; `docs/dev/theming.md`
    /// carries the conventions behind them.  Used by the built-in themes and by the on-disk
    /// loader after user palette overrides are applied.
    pub fn from_palette(palette: &Palette) -> Self {
        let bold = Modifier::BOLD;
        let italic = Modifier::ITALIC;
        let underline = Modifier::UNDERLINED;
        let p = palette.clone();

        // A bg-tinted shade of `code`, distinct from `bg_muted` so a code span inside a
        // striped row still reads as code.  `blend` needs two RGB colors and would otherwise
        // return `code` itself, painting code text on its own color — so an indexed, named,
        // or half-specified palette gets `surface` instead.  The 256-cube built-ins pin
        // their own code styles after this returns.
        let code_bg = match (p.code, p.bg) {
            (Color::Rgb(..), Color::Rgb(..)) => blend(p.code, p.bg, CODE_BG_MIX_TOWARD_BG),
            _ => p.surface,
        };

        // Same `blend` caveat as `code_bg`; the indexed built-ins pin `blockquote_text`.
        let quote_bg = blend(p.secondary, p.bg, QUOTE_BG_MIX_TOWARD_BG);

        // Every `syntax_*` foreground goes through here — see the fields' block comment.
        let syntax_fg = |c: Color| legible_on(code_bg, c, p.text, SYNTAX_MIN_CONTRAST);

        // Alternates `primary` and `secondary`, dulling with each level.  Indexed / named
        // colors fall back to the base shade and rely on the built-ins to override h1–h6.
        let h1c = dim_color(p.primary, 0);
        let h2c = dim_color(p.secondary, 0);
        let h3c = dim_color(p.primary, 1);
        let h4c = dim_color(p.secondary, 1);
        let h5c = dim_color(p.primary, 2);
        let h6c = dim_color(p.secondary, 2);

        Self {
            palette: p.clone(),

            h1: Style::default().fg(h1c).add_modifier(bold),
            h1_rule: Style::default().fg(h1c), // H1 has a rule instead of an underline
            h2: Style::default()
                .fg(h2c)
                .add_modifier(bold)
                .add_modifier(underline),
            h3: Style::default()
                .fg(h3c)
                .add_modifier(bold)
                .add_modifier(underline),
            h4: Style::default()
                .fg(h4c)
                .add_modifier(bold)
                .add_modifier(underline),
            h5: Style::default()
                .fg(h5c)
                .add_modifier(bold)
                .add_modifier(underline),
            h6: Style::default()
                .fg(h6c)
                .add_modifier(bold)
                .add_modifier(underline),

            bold: Style::default().add_modifier(bold),
            italic: Style::default().add_modifier(italic),
            strikethrough: Style::default()
                .fg(p.text_muted)
                .add_modifier(Modifier::CROSSED_OUT),
            highlight: Style::default().bg(p.warning).fg(p.bg),
            code_span: Style::default().fg(p.code).bg(code_bg),
            code_span_dim: Style::default()
                .fg(p.code)
                .bg(code_bg)
                .add_modifier(Modifier::DIM),
            link_text: Style::default().fg(p.link).add_modifier(underline),
            link_file: Style::default().fg(p.link).add_modifier(underline),
            link_heading: Style::default().fg(p.link),
            image_placeholder: Style::default().fg(p.link).add_modifier(italic),
            footnote: Style::default().fg(p.secondary),

            code_block_border: Style::default().fg(p.text).bg(code_bg),
            code_block_lang: Style::default()
                .fg(p.code)
                .bg(p.surface)
                .add_modifier(italic),
            code_block_text: Style::default().fg(p.text).bg(code_bg),

            // Seven classes over the palette's seven *text-carrying* slots (`primary`,
            // `text_muted`, `link`, `success`, `warning`, `error`, `code`), so no new
            // `Palette` slot is owed.  Each sets `fg` only; `code_block_text` owns the bg.
            //
            // Drawing on the fill/chrome slots instead is the trap: `syntax_type` was once
            // `secondary` and `syntax_function` `accent`, measuring 1.99:1 and 1.51:1 on
            // `dark_256`'s code surface against 6.97:1 for the plain text they replaced —
            // highlighting made code *less* readable, on the very theme every indexed
            // terminal is substituted into.
            //
            // `legible_on` is the backstop: a slot chosen for its role on the *page*
            // background can still land too close to the code wash, so each color is
            // lifted toward `text` until it clears `SYNTAX_MIN_CONTRAST`, keeping its hue.
            // A no-op for non-RGB palettes, which is why the two 256-cube built-ins pin all
            // seven by hand.  `syntax_contrast_clears_the_floor_for_every_builtin_theme`
            // holds the arrangement to account.
            syntax_keyword: Style::default().fg(syntax_fg(p.primary)).add_modifier(bold),
            syntax_string: Style::default().fg(syntax_fg(p.success)),
            syntax_comment: Style::default()
                .fg(syntax_fg(p.text_muted))
                .add_modifier(italic),
            syntax_number: Style::default().fg(syntax_fg(p.warning)),
            syntax_type: Style::default().fg(syntax_fg(p.code)),
            syntax_function: Style::default().fg(syntax_fg(p.link)),
            syntax_attribute: Style::default().fg(syntax_fg(p.error)),

            // A wash, not a text attribute: the former blanket ITALIC left `*emphasis*`
            // inside a quote with nothing to say (issue #33).
            blockquote_bar: Style::default().fg(p.secondary),
            blockquote_text: Style::default().bg(quote_bg),

            rule: Style::default().fg(p.secondary),

            // Frontmatter is data *about* the document, so it must not compete with the
            // first heading below it.
            frontmatter_delimiter: Style::default()
                .fg(p.text_muted)
                .add_modifier(Modifier::DIM),
            frontmatter_key: Style::default().fg(p.secondary),
            frontmatter_value: Style::default().fg(p.text_muted),

            list_bullet: Style::default().fg(p.accent),
            list_number: Style::default().fg(p.accent),

            task_unchecked: Style::default().fg(p.warning),
            task_checked: Style::default().fg(p.success),
            task_complete_text: Style::default()
                .fg(p.text_muted)
                .add_modifier(Modifier::CROSSED_OUT),
            task_strikethrough: true,

            table_border: Style::default().fg(p.surface_elevated),
            table_header: Style::default().add_modifier(bold).fg(p.accent),
            table_header_border: Style::default().fg(p.surface_elevated),
            table_cell: Style::default(),
            table_row_even: Style::default(),
            table_row_odd: Style::default().bg(p.bg_muted),
            table_drop_indicator: Style::default().fg(p.primary),
            table_drop_target: Style::default().fg(p.primary).add_modifier(Modifier::DIM),
            table_handle: Style::default().fg(p.primary).add_modifier(Modifier::DIM),
            table_handle_delete: Style::default().fg(p.error),

            status_bar: Style::default().bg(p.surface).fg(p.text),
            status_mode_preview: Style::default()
                .bg(p.text_muted)
                .fg(p.surface)
                .add_modifier(bold),
            status_mode_rendered: Style::default().bg(p.primary).fg(p.bg).add_modifier(bold),
            status_mode_raw: Style::default().bg(p.warning).fg(p.bg).add_modifier(bold),
            // Mirrored by the editor cursor; `fg = bg` keeps the glyph under the cursor
            // legible.
            status_mode_vim_normal: Style::default().bg(p.primary).fg(p.bg).add_modifier(bold),
            status_mode_vim_insert: Style::default().bg(p.success).fg(p.bg).add_modifier(bold),
            status_mode_vim_visual: Style::default().bg(p.secondary).fg(p.bg).add_modifier(bold),
            // Bold, so it and the accented current-section chip frame the breadcrumb chain.
            status_filename: Style::default().fg(p.text).bg(p.surface).add_modifier(bold),
            // Muted on purpose, so the bar's lone primary accent is the current-section
            // breadcrumb.
            status_info: Style::default().fg(p.text_muted).bg(p.surface),
            status_modified: Style::default()
                .fg(p.warning)
                .bg(p.surface)
                .add_modifier(bold),
            status_breadcrumb_sep: Style::default().fg(p.text_muted).bg(p.surface),
            status_breadcrumb_ancestor: Style::default().fg(p.text_muted).bg(p.surface),
            status_breadcrumb_current: Style::default()
                .fg(p.primary)
                .bg(p.surface)
                .add_modifier(bold),

            hint_bar: Style::default().bg(p.surface_elevated).fg(p.text),
            hint_chord: Style::default()
                .fg(p.primary)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            hint_label: Style::default().fg(p.text).bg(p.surface_elevated),

            // All sit on the hint_bar surface so they layer cleanly over the chord row.
            transient_info: Style::default()
                .fg(p.text)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            transient_success: Style::default()
                .fg(p.success)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            transient_warning: Style::default()
                .fg(p.warning)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            transient_error: Style::default()
                .fg(p.error)
                .bg(p.surface_elevated)
                .add_modifier(bold),

            modal_bg: Style::default().bg(p.surface_elevated).fg(p.text),
            modal_title_normal: Style::default()
                .fg(p.primary)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            modal_title_warning: Style::default()
                .fg(p.warning)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            modal_title_error: Style::default()
                .fg(p.error)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            modal_close_hint: Style::default().fg(p.text_muted).bg(p.surface_elevated),
            modal_item: Style::default().fg(p.text).bg(p.surface_elevated),
            modal_item_hint: Style::default().fg(p.primary).bg(p.surface_elevated),
            // `bg` as the fg, not `text`: most themes pair a light `text` with a light
            // `primary`, and light-on-light reads as washed out.
            modal_item_selected: Style::default().bg(p.primary).fg(p.bg).add_modifier(bold),
            // Outlined, not filled, so it reads "marked" without competing with the
            // focused element's `primary` fill.
            modal_item_selected_unfocused: Style::default()
                .fg(p.secondary)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            modal_item_selected_hint: Style::default().fg(p.bg).bg(p.primary),
            modal_description: Style::default().fg(p.accent).bg(p.surface_elevated),
            modal_section_heading: Style::default()
                .fg(p.secondary)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            // Filled vs. outlined, the same convention as
            // `modal_item_selected_unfocused`: without the contrast an unfocused input on
            // first render is easily mistaken for a focused button.
            modal_input_unfocused: Style::default().fg(p.primary).bg(p.surface_elevated),
            modal_input_focused: Style::default().fg(p.bg).bg(p.primary).add_modifier(bold),
            modal_button_focused: Style::default()
                .fg(p.primary)
                .add_modifier(Modifier::REVERSED | bold),

            // Concrete colors, so the document area is the theme's "blank page" rather
            // than the terminal's default.  Themes wanting the terminal's own can set
            // `[normal] fg = "Reset"` / `bg = "Reset"`.
            normal: Style::default().fg(p.text).bg(p.bg),

            // Whichever of `text` / `bg` contrasts better against `accent`, so a theme
            // whose accent sits near its text luminance doesn't render selections as mud.
            // Unmeasurable colors fall back to `text`.
            selection: Style::default()
                .bg(p.accent)
                .fg(best_contrast(p.accent, p.text, p.bg)),

            selection_muted: {
                let bg = blend(p.surface, p.accent, 0.45);
                Style::default().bg(bg).fg(best_contrast(bg, p.text, p.bg))
            },

            // `secondary`, so it reads apart from the warning-hued diff badge.
            status_mode_search: Style::default()
                .bg(p.secondary)
                .fg(p.bg)
                .add_modifier(Modifier::BOLD),

            active_line: Style::default(),

            cursor: Style::default().bg(p.accent).fg(p.bg),

            line_number: Style::default().fg(p.text_muted),

            scrollbar_track: Style::default().fg(p.bg_muted),
            scrollbar_thumb: Style::default().fg(p.primary),
            scrollbar_thumb_active: Style::default().fg(blend(p.primary, p.text, 0.35)),

            // Diff washes mix the saturated diff hue with `surface` so a row reads as a
            // chrome tint rather than a stripe.  The focused fill is then pulled toward
            // `bg` so it sits a shade darker than the inline-change highlight — that gap
            // is what makes within-line edits legible against their row.  On non-RGB
            // palettes `blend` returns its first argument, which is the best available
            // without inventing a hue.
            diff_add_line: Style::default().bg(blend(
                blend(p.surface, p.diff_add, 0.42),
                p.bg,
                0.30,
            )),
            diff_delete_line: Style::default().bg(blend(
                blend(p.surface, p.diff_delete, 0.42),
                p.bg,
                0.30,
            )),
            diff_add_line_unfocused: Style::default().bg(blend(p.surface, p.diff_add, 0.07)),
            diff_delete_line_unfocused: Style::default().bg(blend(p.surface, p.diff_delete, 0.07)),
            diff_add_inline: Style::default()
                .bg(blend(p.diff_add, p.bg, 0.35))
                .add_modifier(bold),
            diff_delete_inline: Style::default()
                .bg(blend(p.diff_delete, p.bg, 0.35))
                .add_modifier(bold),
            // A surface-derived tint like the `_line_unfocused` washes, no bold: a changed
            // word is a slightly deeper patch (0.20 vs. the 0.07 line wash) within a faint
            // hunk, not a competitor to the focused one.
            diff_add_inline_unfocused: Style::default().bg(blend(p.surface, p.diff_add, 0.20)),
            diff_delete_inline_unfocused: Style::default().bg(blend(
                p.surface,
                p.diff_delete,
                0.20,
            )),
            // A full-width neutral chrome strip, so the accept/reject prompt reads as
            // actionable rather than a gap.  Plain surface, not a `secondary` tint, so the
            // colored foregrounds keep full contrast.
            diff_decision_pending: Style::default().fg(p.secondary).bg(p.surface_elevated),
            diff_decision_accepted: Style::default()
                .fg(p.diff_add)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            diff_decision_rejected: Style::default()
                .fg(p.diff_delete)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            // The lighter `surface`, so it recedes a step; `build_line` derives the
            // resolved unfocused styling from this plus the per-state hue and DIM.
            diff_decision_unfocused: Style::default().fg(p.text_muted).bg(p.surface),
            status_mode_diff: Style::default().bg(p.warning).fg(p.bg).add_modifier(bold),
            // Red on the hint line (top), green on the status line (bottom), mirroring the
            // document's deletes-above / adds-below stacking.  Tints, not fills, so a bar
            // is never mistaken for an in-document hunk.
            status_bar_diff: Style::default()
                .bg(blend(p.surface, p.diff_add, 0.22))
                .fg(p.text),
            hint_bar_diff: Style::default()
                .bg(blend(p.surface_elevated, p.diff_delete, 0.22))
                .fg(p.text),
        }
    }

    /// The "blank page" background, for UI code compositing against the document surface.
    pub fn default_bg(&self) -> Color {
        self.palette.bg
    }

    /// Muted text — the Ansi256 fallback foreground for the modal-dim sweep.
    pub fn text_muted(&self) -> Color {
        self.palette.text_muted
    }

    /// The heading style for a level (1–6).
    pub fn heading_style(&self, level: pulldown_cmark::HeadingLevel) -> Style {
        use pulldown_cmark::HeadingLevel::*;
        match level {
            H1 => self.h1,
            H2 => self.h2,
            H3 => self.h3,
            H4 => self.h4,
            H5 => self.h5,
            H6 => self.h6,
        }
    }

    /// The status mode chip style for `mode`.
    pub fn status_mode_style(&self, mode: crate::editor::Mode) -> Style {
        use crate::editor::Mode::*;
        match mode {
            Preview => self.status_mode_preview,
            Rendered => self.status_mode_rendered,
            Raw => self.status_mode_raw,
            Diff => self.status_mode_diff,
        }
    }

    /// Build a `Theme` from a user-authored [`crate::config::theme_file::ThemeFile`].
    /// Under `monochrome` the file is ignored entirely, preserving the contract that a
    /// `ColorDepth::NoColor` terminal emits no color escapes however colorful the
    /// installed theme is.
    pub fn from_file(file: &super::theme_file::ThemeFile, monochrome: bool) -> Self {
        if monochrome {
            super::themes::monochrome_dark::theme()
        } else {
            file.into()
        }
    }
}

/// `256 Dark`, whose hand-pinned shades replace the RGB-only derivations [`Theme::from_palette`]
/// can't apply to its indexed palette.
impl Default for Theme {
    fn default() -> Self {
        super::themes::dark_256::theme()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::themes::util::contrast_ratio;

    /// Issue #113: `blend` is a no-op unless both colors are RGB, so the code surface once
    /// collapsed onto `code` itself for an indexed palette or a hex one mixed with indexed
    /// defaults.
    #[test]
    fn code_surface_never_collapses_onto_code() {
        let indexed = Palette::default();
        let mixed = Palette {
            code: Color::Rgb(0xc0, 0x80, 0xff),
            ..Palette::default()
        };
        let named = Palette {
            code: Color::Magenta,
            bg: Color::Black,
            ..Palette::default()
        };
        let themes = [
            ("default", Theme::default()),
            ("indexed", Theme::from_palette(&indexed)),
            ("mixed", Theme::from_palette(&mixed)),
            ("named", Theme::from_palette(&named)),
        ];
        for (label, t) in themes {
            assert_ne!(t.code_span.fg, t.code_span.bg, "{label}");
            assert_ne!(t.code_block_text.bg, Some(t.palette.code), "{label}");
        }
        // `Monochrome Dark` sets no colors at all, marking code by modifier instead.
        for (name, ctor) in BUILTIN_THEMES {
            let t = ctor();
            if t.code_span.fg.is_some() {
                assert_ne!(t.code_span.fg, t.code_span.bg, "{name}");
            }
        }
    }

    /// A user theme offered here is how the `--no-config` read half leaks: selecting it
    /// loads the very file the run exists to rule out.  The second half proves the
    /// omission came from the gate, not a missed folder.
    #[test]
    fn user_themes_are_not_listed_while_the_config_dir_is_disabled() {
        let _lock = crate::test_env::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _xdg = crate::test_env::EnvGuard::set("XDG_CONFIG_HOME", dir.path());
        std::fs::create_dir_all(dir.path().join("edamame/themes")).unwrap();
        std::fs::write(
            dir.path().join("edamame/themes/mine.toml"),
            "[h1]\nfg = \"red\"\n",
        )
        .unwrap();

        {
            let _disabled = super::super::persistence::SuppressGuard::new();
            let names = list_theme_names();
            assert!(!names.iter().any(|n| n == "mine"), "{names:?}");
            assert_eq!(names.len(), BUILTIN_THEMES.len(), "{names:?}");
        }
        assert!(list_theme_names().iter().any(|n| n == "mine"));
    }

    /// `docs/themes.md` lists these by name, split into Dark and Light groups; accepting
    /// a change to this snapshot is the reminder to update that list.
    #[test]
    fn builtin_themes_are_pinned_for_the_docs() {
        let rows: Vec<String> = BUILTIN_THEMES
            .iter()
            .map(|(name, ctor)| {
                let appearance = if ctor().palette.light {
                    "light"
                } else {
                    "dark"
                };
                format!("{name} ({appearance})")
            })
            .collect();
        insta::assert_snapshot!(rows.join("\n"));
    }

    /// Every color field on [`Palette`], in declaration order.  Add new ones here so
    /// `palette_fields_list_matches_struct` keeps the count honest.
    const PALETTE_FIELDS: &[&str] = &[
        "text",
        "text_muted",
        "bg",
        "bg_muted",
        "surface",
        "surface_elevated",
        "primary",
        "secondary",
        "accent",
        "link",
        "success",
        "warning",
        "error",
        "code",
        "diff_add",
        "diff_delete",
        // The `light: bool` field is not color-typed and so is not listed; a new
        // non-color field is covered by every palette ctor needing to compile.
    ];

    #[test]
    fn palette_fields_list_matches_struct() {
        // A renamed or removed field fails to compile; an added one fails the length
        // assertion below.
        let p = Palette::default();
        let _all = [
            p.text,
            p.text_muted,
            p.bg,
            p.bg_muted,
            p.surface,
            p.surface_elevated,
            p.primary,
            p.secondary,
            p.accent,
            p.link,
            p.success,
            p.warning,
            p.error,
            p.code,
            p.diff_add,
            p.diff_delete,
        ];
        assert_eq!(_all.len(), PALETTE_FIELDS.len());
    }

    #[test]
    fn light_palette_has_distinct_default_bg() {
        use super::super::themes::{dark_256, light_256};
        assert_ne!(dark_256::palette().bg, light_256::palette().bg);
    }

    #[test]
    fn builtin_lookup_resolves_registered_names() {
        assert!(Theme::builtin("256 Dark").is_some());
        assert!(Theme::builtin("256 Light").is_some());
        assert!(Theme::builtin("nonexistent").is_none());
    }

    #[test]
    fn only_light_256_is_classified_as_light() {
        // The central registry of the expectation: a new palette ctor missing `light:
        // true` (or carrying a stray one) fails here.
        let expected_light: &[&str] = &[
            "256 Light",
            "Catppuccin Latte",
            "GitHub Light",
            "Gruvbox Light",
            "Rosé Pine Dawn",
            "Solarized Light",
            "Tokyo Night Day",
        ];
        for (name, ctor) in BUILTIN_THEMES {
            let appearance = ctor().palette.appearance();
            let should_be_light = expected_light.contains(name);
            assert_eq!(
                appearance == AppearanceMode::Light,
                should_be_light,
                "theme {name:?} classified as {appearance:?} but expected light={should_be_light}",
            );
        }
    }

    #[test]
    fn builtin_palettes_have_no_duplicate_slots() {
        // Duplicate slots make a theme monochromatic in the affected affordance pair —
        // with `accent == error`, every text selection renders in the error color.
        for (name, ctor) in BUILTIN_THEMES {
            // Monochrome deliberately collapses every slot to `Color::Reset`.
            if *name == "Monochrome Dark" {
                continue;
            }
            let p = ctor().palette;
            let slots: &[(&str, Color)] = &[
                ("text", p.text),
                ("text_muted", p.text_muted),
                ("bg", p.bg),
                ("bg_muted", p.bg_muted),
                ("surface", p.surface),
                ("surface_elevated", p.surface_elevated),
                ("primary", p.primary),
                ("secondary", p.secondary),
                ("accent", p.accent),
                ("link", p.link),
                ("success", p.success),
                ("warning", p.warning),
                ("error", p.error),
                ("code", p.code),
                ("diff_add", p.diff_add),
                ("diff_delete", p.diff_delete),
            ];
            for (i, (a, ca)) in slots.iter().enumerate() {
                for (b, cb) in &slots[i + 1..] {
                    assert_ne!(
                        ca, cb,
                        "theme {name:?}: slot {a} and {b} share the same color",
                    );
                }
            }
        }
    }

    #[test]
    fn indexed_fallback_follows_the_current_theme_appearance() {
        // `configured` is deliberately the opposite of each theme's own appearance, to
        // prove it isn't what gets consulted.
        assert_eq!(
            indexed_fallback_theme("Dracula", AppearanceMode::Light),
            Some("256 Dark"),
        );
        assert_eq!(
            indexed_fallback_theme("GitHub Light", AppearanceMode::Dark),
            Some("256 Light"),
        );
    }

    #[test]
    fn indexed_fallback_uses_configured_appearance_for_unknown_themes() {
        assert_eq!(
            indexed_fallback_theme("no-such-theme", AppearanceMode::Light),
            Some("256 Light"),
        );
        assert_eq!(
            indexed_fallback_theme("no-such-theme", AppearanceMode::Dark),
            Some("256 Dark"),
        );
    }

    #[test]
    fn indexed_fallback_is_a_noop_for_the_indexed_safe_themes() {
        // Idempotence: a substituted theme must not itself trigger a substitution, or a
        // reload would fire the notice forever.
        for name in INDEXED_SAFE_THEMES {
            assert_eq!(indexed_fallback_theme(name, AppearanceMode::Dark), None);
            assert_eq!(indexed_fallback_theme(name, AppearanceMode::Light), None);
        }
    }

    #[test]
    fn indexed_safe_themes_are_registered() {
        for name in INDEXED_SAFE_THEMES {
            assert!(
                BUILTIN_THEMES.iter().any(|(n, _)| n == name),
                "{name} is not a registered built-in",
            );
        }
    }

    #[test]
    fn counterpart_theme_is_bidirectional() {
        assert_eq!(counterpart_theme("256 Dark"), Some("256 Light"));
        assert_eq!(counterpart_theme("256 Light"), Some("256 Dark"));
        assert_eq!(counterpart_theme("Edamame"), None);
        assert_eq!(counterpart_theme("nonexistent"), None);
    }

    #[test]
    fn resolve_theme_for_mode_switch_uses_counterpart_when_available() {
        assert_eq!(
            resolve_theme_for_mode_switch("256 Dark", AppearanceMode::Light),
            "256 Light",
        );
        assert_eq!(
            resolve_theme_for_mode_switch("256 Light", AppearanceMode::Dark),
            "256 Dark",
        );
    }

    #[test]
    fn resolve_theme_for_mode_switch_falls_back_to_default() {
        assert_eq!(
            resolve_theme_for_mode_switch("Edamame", AppearanceMode::Light),
            DEFAULT_LIGHT_THEME,
        );
        assert_eq!(
            resolve_theme_for_mode_switch("Dracula", AppearanceMode::Light),
            DEFAULT_LIGHT_THEME,
        );
    }

    #[test]
    fn list_theme_names_for_mode_filters() {
        let dark = list_theme_names_for_mode(AppearanceMode::Dark);
        let light = list_theme_names_for_mode(AppearanceMode::Light);
        assert!(dark.iter().any(|n| n == "Edamame"));
        assert!(!dark.iter().any(|n| n == "256 Light"));
        assert!(light.iter().any(|n| n == "256 Light"));
        assert!(!light.iter().any(|n| n == "Edamame"));
    }

    // ── Syntax highlighting contrast ──────────────────────────────

    /// Resolve an xterm-256 index to its spec-fixed RGB value, so an indexed theme's
    /// choices can be measured.  Indices 0–15 are the terminal's user-configurable ANSI
    /// slots and have no fixed value; the test below treats a hit there as a *failure*
    /// rather than a skip, so a future edit can't opt out of the floor by reaching for one.
    fn xterm_rgb(i: u8) -> Option<(u8, u8, u8)> {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        match i {
            0..=15 => None,
            16..=231 => {
                let i = i - 16;
                Some((
                    LEVELS[(i / 36) as usize],
                    LEVELS[((i % 36) / 6) as usize],
                    LEVELS[(i % 6) as usize],
                ))
            }
            _ => {
                let v = 8 + 10 * (i as u16 - 232);
                Some((v as u8, v as u8, v as u8))
            }
        }
    }

    fn as_rgb(c: Color) -> Option<Color> {
        match c {
            Color::Rgb(..) => Some(c),
            Color::Indexed(i) => xterm_rgb(i).map(|(r, g, b)| Color::Rgb(r, g, b)),
            _ => None,
        }
    }

    /// The invariant behind `SYNTAX_MIN_CONTRAST`, the slot choices in `from_palette`,
    /// and the 256-cube themes' hand-picked syntax colors.
    ///
    /// The floor is `min(SYNTAX_MIN_CONTRAST, plain code text)`, not a flat 4.5, because
    /// `legible_on` lifts toward `text` and saturates there: a theme cannot make a token
    /// more legible than its own body text (Solarized Light's plain code text is 4.49:1).
    /// The real promise is that highlighting never makes a code block *less* readable
    /// than leaving it plain.
    ///
    /// `legible_on` is a no-op for indexed palettes, which is where the regression that
    /// prompted this lived.  A theme with no RGB-resolvable colors is skipped — today only
    /// `Monochrome Dark`, which sets no syntax foreground at all.
    #[test]
    fn syntax_contrast_clears_the_floor_for_every_builtin_theme() {
        let mut failures = Vec::new();
        for (name, ctor) in BUILTIN_THEMES {
            let theme = ctor();
            let (Some(bg), Some(plain_fg)) = (
                theme.code_block_text.bg.and_then(as_rgb),
                theme.code_block_text.fg.and_then(as_rgb),
            ) else {
                continue;
            };
            let plain = contrast_ratio(plain_fg, bg).expect("both sides are RGB");
            let floor = SYNTAX_MIN_CONTRAST.min(plain);
            for (field, style) in [
                ("syntax_keyword", theme.syntax_keyword),
                ("syntax_string", theme.syntax_string),
                ("syntax_comment", theme.syntax_comment),
                ("syntax_number", theme.syntax_number),
                ("syntax_type", theme.syntax_type),
                ("syntax_function", theme.syntax_function),
                ("syntax_attribute", theme.syntax_attribute),
            ] {
                // No foreground means `code_block_text`, i.e. the `plain` baseline.
                let Some(fg) = style.fg else { continue };
                match as_rgb(fg).and_then(|fg| contrast_ratio(fg, bg)) {
                    Some(ratio) if ratio >= floor => {}
                    Some(ratio) => failures.push(format!(
                        "{name}.{field}: {ratio:.2}:1 (floor {floor:.2}, plain text {plain:.2})"
                    )),
                    None => failures.push(format!(
                        "{name}.{field}: {fg:?} has no fixed value to measure"
                    )),
                }
            }
        }
        assert!(
            failures.is_empty(),
            "syntax colours below their theme's contrast floor:\n  {}",
            failures.join("\n  ")
        );
    }
}
