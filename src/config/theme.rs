use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use ratatui::style::{Color, Modifier, Style};

use super::sections::AppearanceMode;
use super::themes::util::blend;

/// How heavily to mix `code` toward `bg` when deriving the code
/// surface bg.  Closer to 1.0 = closer to `bg` (a barely-tinted
/// neutral); closer to 0.0 = closer to the raw `code` shade.
const CODE_BG_MIX_TOWARD_BG: f32 = 0.92;

/// Darken `base` by `level` steps for the heading ramp (0 = base,
/// 1 = medium, 2 = dull).  RGB colours are scaled toward black via a
/// fixed lightness factor per step.  Indexed and named colours can't
/// be cleanly stepped without shifting hue, so they're returned
/// unchanged — built-in indexed-colour themes pin the ramp manually
/// in their ctor (see [`BUILTIN_THEMES`]).
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

/// Edamame's two-tier theming model.
///
/// 1. [`Palette`] — a small flat set of semantic colours (brand, accent,
///    link, status colours, surface tones).  Each name maps to a single
///    shade; focus / active / disabled affordances layer text modifiers
///    (BOLD, REVERSED, DIM) on top rather than reaching for a second
///    palette slot.
/// 2. [`Theme`] — every styled element in the UI gets a precomputed
///    [`Style`].  `Theme::default()` derives every style from the default
///    palette, so any change to the palette ripples through the whole UI.
///
/// User theme files (`themes/<name>.toml`) may override the palette,
/// individual styles, or both.  See [`super::theme_file`] for the on-disk
/// format and the merge order.
///
/// No hardcoded colours exist outside [`Palette::default`] — every UI site
/// reads from `theme.<field>`.
#[derive(Debug, Clone)]
pub struct Theme {
    /// The named brand-colour palette every style is derived from.
    /// Stored on the theme so user code (e.g. modal selection rendering)
    /// can reach for `bg` as a fg colour against a coloured bg.
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
    /// Dim variant of [`Self::code_span`] used for inline code that
    /// appears inside strikethrough text (e.g. inside a `Strikethrough`
    /// inline or a checked task item's text).  Derives from
    /// [`Self::code_span`] + `Modifier::DIM` so the snippet reads as
    /// struck-through without losing its code-span affordance.
    pub code_span_dim: Style,
    /// Web link (`http://`, `https://`, `mailto:`, etc.) — `link`
    /// foreground + underline so the URL reads as actionable.
    pub link_text: Style,
    /// File link (relative or absolute path) — same `link` colour as
    /// `link_text`; themes that want a quieter shade can override the
    /// style directly in TOML.
    pub link_file: Style,
    /// In-document heading link (`#section`) — `link` fg, no underline.
    pub link_heading: Style,
    pub image_placeholder: Style,
    /// Footnote / reference marker (deferred renderer feature).  Field
    /// is in place so themes can already style it ahead of the
    /// implementation.
    pub footnote: Style,

    // ── Block elements ────────────────────────────────────────────
    pub code_block_border: Style,
    pub code_block_lang: Style,
    pub code_block_text: Style,
    pub blockquote_bar: Style,
    pub blockquote_text: Style,
    pub rule: Style,

    // ── List markers ──────────────────────────────────────────────
    pub list_bullet: Style,
    pub list_number: Style,

    // ── Task list ─────────────────────────────────────────────────
    pub task_unchecked: Style,
    /// Style applied to the `[✓]` marker for checked items.
    pub task_checked: Style,
    /// Style applied to the *text* of completed tasks (the part after
    /// the checkbox).  Distinct from `task_checked` so the marker can
    /// stay green while the text fades to muted grey.
    pub task_complete_text: Style,
    /// Whether to render checked item text with strikethrough (default: true).
    pub task_strikethrough: bool,

    // ── Table ─────────────────────────────────────────────────────
    pub table_border: Style,
    pub table_header: Style,
    pub table_header_border: Style,
    pub table_cell: Style,
    /// Background fill for even-numbered data rows (0-indexed: row 0 = first
    /// data row).  Only applied when `config.table.row_striping` is true; the
    /// default is `Style::default()` so no visible change is produced for
    /// users who haven't opted in.
    pub table_row_even: Style,
    /// Background fill for odd-numbered data rows.  See `table_row_even`.
    pub table_row_odd: Style,
    /// Highlight applied during a row / column drag to mark the destination
    /// separator the cursor is currently hovering.  Painted as a post-pass
    /// over the table border so no buffer mutation is required.
    pub table_drop_indicator: Style,
    /// Style for the *inert* drop-target separators painted during a
    /// row / column drag — every separator that's a valid drop site
    /// gets this style; the one the pointer is currently over upgrades
    /// to `table_drop_indicator`.  Defaults to `primary` + DIM so the
    /// inert sites read as a set of possibilities with one
    /// pointer-tracked highlight.
    pub table_drop_target: Style,
    /// Style for the row/column reorder (`⠿`) and column-resize (`⇔`)
    /// button glyphs painted on top of the table border.  Distinct from
    /// `table_border` so the affordances read as interactive rather
    /// than chrome.
    pub table_handle: Style,
    /// Style for the row/column delete (`✕`) button glyphs painted on
    /// top of the table border.  Distinct from `table_handle` so the
    /// destructive affordance reads as a warning.
    pub table_handle_delete: Style,

    // ── Status bar ────────────────────────────────────────────────
    pub status_bar: Style,
    /// Mode badge in Preview mode.  See also the Rendered/Raw variants.
    pub status_mode_preview: Style,
    /// Mode badge in Rendered mode.
    pub status_mode_rendered: Style,
    /// Mode badge in Raw mode.
    pub status_mode_raw: Style,
    pub status_filename: Style,
    pub status_info: Style,
    pub status_modified: Style,
    /// Style for the selection-size indicator (e.g. ` Sel 42 ch · 3 ln `).
    pub status_selection: Style,

    // ── Hint line (Phase 9) ───────────────────────────────────────
    /// Base background/foreground for the contextual hint line.
    pub hint_bar: Style,
    /// Chord glyph style (e.g. the `^C` in `^C Copy`).  Contrasting
    /// background distinguishes the keybind from its label.
    pub hint_chord: Style,
    /// Label style (e.g. the `Copy` in `^C Copy`).  Blends into the
    /// surrounding hint_bar fill.
    pub hint_label: Style,

    // ── Transient messages (Phase 9) ──────────────────────────────
    /// Neutral notification style — e.g. `Copied`, `Saved`.
    pub transient_info: Style,
    /// Success notification style — e.g. `Autosaved`.
    pub transient_success: Style,
    /// Warning notification style — e.g. `Configuration updated`.
    pub transient_warning: Style,
    /// Error notification style — sticky, dismissed with Escape.
    pub transient_error: Style,

    // ── Modal popups ──────────────────────────────────────────────
    /// Background fill for modal bodies (palette, settings, keybinds,
    /// Save-Copy, Insert-Table, …).  Distinct field so themes can give
    /// modals a different surface from the status bar even when the
    /// default palette uses the same shade for both.
    pub modal_bg: Style,
    /// Title text style for `ModalKind::Normal` — neutral / informational
    /// modals.  `primary` on the modal surface.
    pub modal_title_normal: Style,
    /// Title text style for `ModalKind::Warning` — yellow on the modal
    /// surface.  Used by config / image / quit / dirty-guard prompts.
    pub modal_title_warning: Style,
    /// Title text style for `ModalKind::Error` — red on the modal surface.
    pub modal_title_error: Style,
    /// `text_muted` close-hint label rendered as `esc` on the right edge
    /// of the title row of dismissable modals.  Doubles as the visible
    /// affordance for the clickable close button.
    pub modal_close_hint: Style,
    /// Default style for an unfocused row in a list-style modal.
    pub modal_item: Style,
    /// Right-aligned hint / sub-label on an *unfocused* row (e.g. the
    /// chord shown next to a palette entry, or the value column in
    /// settings / keybinds rows).  Mirrors `modal_item_selected_hint`
    /// for the unfocused state.
    pub modal_item_hint: Style,
    /// Selected row in a list-style modal (palette / settings /
    /// keybinds).  Filled background so the row reads as the focus.
    pub modal_item_selected: Style,
    /// A persistent selection that does NOT currently have focus —
    /// e.g. the active tri-state pill in a row whose label isn't
    /// focused, or a checked toggle whose label isn't the active
    /// element.  Rendered as `secondary` **foreground** (no fill) so
    /// the focused affordance (which uses `primary` *fill*) reads
    /// unambiguously, while the persistent selection still carries a
    /// distinct outlined affordance.  See `docs/theming.md`
    /// §"Focus vs. persistent selection" for the three-tier
    /// convention this field is part of, and the monochrome fallback.
    /// For composite affordances (e.g. `[x] Label`), apply only to
    /// the glyph that carries the selection, not the full row.
    pub modal_item_selected_unfocused: Style,
    /// Right-aligned hint / sub-label on the focused row (e.g. the
    /// chord shown next to a palette entry, or the value column on
    /// settings / keybinds rows).
    pub modal_item_selected_hint: Style,
    /// Pinned-footer description for the focused row (e.g. the
    /// settings overlay's bottom line that explains the focused
    /// setting).  Sits on the modal body's `surface_elevated` rather
    /// than on the row's selection bg, so it gets its own field.
    pub modal_description: Style,
    /// Section heading inside a modal (e.g. `— Editor —` in the
    /// keybinds overlay).  Slightly distinct from a document H2.
    pub modal_section_heading: Style,
    /// Inline editor / text input in an unfocused state.
    pub modal_input_unfocused: Style,
    /// Inline editor / text input while focused for typing.
    pub modal_input_focused: Style,
    /// Modal button when focused for activation.
    pub modal_button_focused: Style,

    // ── General text ──────────────────────────────────────────────
    pub normal: Style,

    /// Background style applied to characters inside an active text selection.
    /// Renders on top of the character's own style so colour-coded content
    /// stays legible.
    pub selection: Style,

    /// Find-in-document highlight — distinct from `selection` so search
    /// hits remain visible even when one of them is currently selected.
    pub search_highlight: Style,

    /// Background style applied to the cursor's current line.  Default
    /// is `Style::default()` (no tint) — the active-line highlight is a
    /// deferred feature; the field exists so themes can opt in early.
    pub active_line: Style,

    /// Block cursor when the editor is in Preview mode.  Mirrors the
    /// status-bar mode chip so the cursor reads as the same affordance
    /// in both places.  Preview is read-only so this primarily
    /// applies to ad-hoc cursor indicators (e.g. tooling overlays).
    pub cursor_preview: Style,
    /// Block cursor when the editor is in Rendered mode.  Mirrors the
    /// Rendered mode chip's bg.
    pub cursor_rendered: Style,
    /// Block cursor when the editor is in Raw mode.  Mirrors the Raw
    /// mode chip's bg.
    pub cursor_raw: Style,
    /// Generic input-line cursor (`▏` glyph) used inside modal text
    /// inputs.  Default is `REVERSED` only, which swaps fg/bg of
    /// whatever's underneath — kept distinct from the editor cursor
    /// because modal inputs aren't tied to editor mode.
    pub cursor: Style,

    /// Scrollbar track (the `│` glyph drawn down the gutter behind the
    /// thumb).  The track is only painted when the content overflows.
    pub scrollbar_track: Style,
    /// Scrollbar thumb (the `█` glyph that indicates current position).
    pub scrollbar_thumb: Style,
    /// Scrollbar thumb while the user is hovering the gutter or
    /// dragging the thumb.  Defaults to `primary` + `Modifier::REVERSED`.
    pub scrollbar_thumb_active: Style,
}

/// Edamame's semantic colour palette.  Every theme is built from these
/// eighteen colours plus six heading slots.
///
/// `text` / `bg` are concrete colours rather than terminal defaults
/// because they're used as foregrounds in inverse contexts (e.g. the
/// Rendered-mode mode chip: `primary` bg with `bg` fg), where
/// `Color::Reset` would not produce the right contrast.
///
/// `surface_elevated` is the heavier chrome surface (status bar, modal
/// body, inline-code background); `surface` is a slightly *lighter*
/// elevated surface used for secondary chrome (hint line, transient
/// messages) so the hint row reads as one step lifted from the
/// document area.
///
/// `diff_add` / `diff_delete` are reserved for a future diff view; no
/// styles currently consume them.
#[derive(Debug, Clone)]
pub struct Palette {
    /// Default document foreground.
    pub text: Color,
    /// Peripheral / de-emphasised text — strikethrough body,
    /// completed-task text, modal close hint, Preview-mode chip bg.
    pub text_muted: Color,
    /// Default document background.
    pub bg: Color,
    /// Muted surface for table-row stripes, scrollbar track,
    /// Preview-mode cursor bg.  Inline / fenced code use a tinted
    /// shade derived from [`Self::code`] instead, so a code span on
    /// top of a striped row still reads as code.
    pub bg_muted: Color,
    /// Lifted chrome surface (hint line, transient-message strip).
    pub surface: Color,
    /// Heavier chrome surface (status bar, modal body, code-block bg).
    pub surface_elevated: Color,

    /// Brand colour.  Headings (Rendered-mode chip, status info,
    /// modal titles), non-link focus affordances (selected modal
    /// row, modal input fill, button focus, scrollbar thumb).
    pub primary: Color,
    /// Structural chrome colour (section headings, search-highlight
    /// bg, rules, blockquote bar, footnote marker, command-palette
    /// divider).
    pub secondary: Color,
    /// Accent — list markers, table header, modal description /
    /// selected-row hint; also the bg for text selection.
    pub accent: Color,
    /// Link foreground (web link, file link, heading link, image
    /// placeholder).  Reserved for link affordances only.
    pub link: Color,

    pub success: Color,
    pub warning: Color,
    pub error: Color,

    /// Inline-code and code-block-language foreground.
    pub code: Color,

    /// Reserved for a future diff view — added line gutter / fill.
    pub diff_add: Color,
    /// Reserved for a future diff view — removed line gutter / fill.
    pub diff_delete: Color,

    /// `true` when this palette is intended as a light-mode theme; `false`
    /// for dark themes (the common case).  Drives the theme picker's
    /// light/dark filter — every built-in palette sets this explicitly in
    /// its constructor, and user TOML themes opt in via `light = true`
    /// at the top of their `.toml` file.  We don't infer from `bg`
    /// luminance: the flag is the single source of truth so a theme with
    /// a mid-grey bg or an indexed colour still classifies unambiguously.
    pub light: bool,
}

impl Palette {
    /// Classification used by the theme picker's light/dark filter.
    /// Reads the explicit [`Palette::light`] flag — there is no
    /// luminance heuristic, so a theme with an unconventional bg still
    /// classifies unambiguously.
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

/// Constructor for a built-in theme.  Each entry in [`BUILTIN_THEMES`]
/// pairs a reserved theme name with one of these.  Built-ins return a
/// full [`Theme`] rather than just a [`Palette`] so they can pin the
/// `h1`–`h6` heading ramp to curated shades — `Theme::from_palette`
/// derives the ramp algorithmically from `primary` and `secondary`,
/// which works well for RGB themes but produces poor results on
/// indexed-colour built-ins where stepping through the 6×6×6 cube
/// shifts hue.
pub type ThemeCtor = fn() -> Theme;

/// Registry of built-in themes shipped in the binary.  Names listed
/// here are reserved: a user file `themes/<name>.toml` with one of
/// these names is ignored at load time so the built-in always wins.
/// Order is the user-facing cycle order in the settings overlay.
///
/// Each constructor lives in its own file under `src/config/themes/`
/// so adding a theme is a single new file plus an entry here.
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

/// Bidirectional pairings between dark and light variants of the same
/// theme brand.  Used when the user flips appearance mode in the theme
/// picker: if the currently-active theme appears in this table, its
/// sibling is previewed; otherwise the default theme of the new mode
/// (see [`DEFAULT_DARK_THEME`] / [`DEFAULT_LIGHT_THEME`]) is previewed.
///
/// Order is `(dark_name, light_name)` for readability — the lookup
/// helper [`counterpart_theme`] checks both directions.
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

/// Return the cross-mode sibling of `name`, if registered in
/// [`THEME_COUNTERPARTS`].  Bidirectional: passing either half of a
/// pair returns the other.
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

/// Decide which theme to preview when the user toggles appearance mode
/// to `target` while `current` is the active theme.  Strategy:
///
/// 1. If `current` has a counterpart in [`THEME_COUNTERPARTS`] and that
///    counterpart classifies as `target`, return the counterpart.
/// 2. Otherwise return the [`DEFAULT_DARK_THEME`] / [`DEFAULT_LIGHT_THEME`]
///    for the target mode.
///
/// Used by both the theme-picker mode toggle and the settings-overlay
/// Appearance row so the live preview is consistent across both UIs.
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

/// Cache entry for a user theme's classification: the file's mtime
/// (so stale entries are detected when the user edits a theme file
/// mid-session) and the resolved appearance mode.
type AppearanceCacheEntry = (Option<SystemTime>, AppearanceMode);

/// Cache of user-theme classifications, keyed by theme name.  Built-ins
/// are resolved without consulting the cache.
static USER_THEME_APPEARANCE_CACHE: Mutex<Option<HashMap<String, AppearanceCacheEntry>>> =
    Mutex::new(None);

/// Resolve `name` to its [`AppearanceMode`] by loading the theme and
/// reading [`Palette::light`].  Returns `None` if the theme can't be
/// resolved (unknown name, malformed user TOML); callers default to
/// `Dark` in that case so the theme stays visible in the dark list.
///
/// Built-in themes resolve in O(1) via [`Theme::builtin`].  User
/// themes hit a process-wide mtime-keyed cache so repeated calls (the
/// theme-picker filter invokes this once per theme on every mode flip)
/// don't re-read + re-parse the TOML each time.
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
    // Cache miss / mtime mismatch — read the file.  We don't surface
    // parse warnings here; classification is best-effort and silent.
    let text = std::fs::read_to_string(&path).ok()?;
    let file: super::theme_file::ThemeFile = toml::from_str(&text).ok()?;
    let theme: Theme = (&file).into();
    let mode = theme.palette.appearance();
    cache.insert(name.to_owned(), (mtime, mode));
    Some(mode)
}

/// List theme names whose appearance matches `mode`.  Resolves each
/// name from [`list_theme_names`] and filters by [`Palette::light`].
/// Themes that fail to resolve are treated as `Dark` (so they remain
/// visible in the dark list rather than silently disappearing).
pub fn list_theme_names_for_mode(mode: AppearanceMode) -> Vec<String> {
    list_theme_names()
        .into_iter()
        .filter(|name| theme_appearance(name).unwrap_or(AppearanceMode::Dark) == mode)
        .collect()
}

/// List every theme name available to the user: the compiled-in
/// [`BUILTIN_THEMES`] (in their declared order) followed by any
/// user-authored `<config_dir>/themes/*.toml` stems whose name doesn't
/// shadow a built-in.  Built-ins are always present so the picker
/// works even when the user has no custom themes installed.
pub fn list_theme_names() -> Vec<String> {
    let mut out: Vec<String> = BUILTIN_THEMES
        .iter()
        .map(|(n, _)| (*n).to_owned())
        .collect();

    if let Some(dir) = super::config::Config::config_dir() {
        let themes = dir.join("themes");
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
    /// Look up a built-in theme by name.  Returns `None` for names not
    /// in [`BUILTIN_THEMES`], in which case the caller falls back to
    /// reading `themes/<name>.toml` from the user's config directory.
    pub fn builtin(name: &str) -> Option<Theme> {
        BUILTIN_THEMES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, ctor)| ctor())
    }
}

impl Theme {
    /// Build a fully-populated [`Theme`] from `palette`.  Every style
    /// is derived from a palette entry per the rules in `theming.md`.
    /// Used both by [`Theme::default`] and by the on-disk theme loader
    /// after applying user palette overrides.
    pub fn from_palette(palette: &Palette) -> Self {
        let bold = Modifier::BOLD;
        let italic = Modifier::ITALIC;
        let underline = Modifier::UNDERLINED;
        let p = palette.clone();

        // Code surface: a desaturated, bg-tinted shade of `code` —
        // distinguishable from `bg_muted` (striped-row bg) so a code
        // span inside a stripe still reads as code.  `blend` returns
        // `p.code` unchanged for non-RGB palettes; the 256-cube
        // built-ins compensate by overriding the four code styles
        // after `from_palette` returns.
        let code_bg = blend(p.code, p.bg, CODE_BG_MIX_TOWARD_BG);

        // Heading ramp alternates `primary` and `secondary`, getting
        // progressively duller / darker with each level.  RGB themes
        // get a tinted ramp; indexed / named colours fall back to the
        // base shade and rely on built-ins to override h1–h6.
        let h1c = dim_color(p.primary, 0);
        let h2c = dim_color(p.secondary, 0);
        let h3c = dim_color(p.primary, 1);
        let h4c = dim_color(p.secondary, 1);
        let h5c = dim_color(p.primary, 2);
        let h6c = dim_color(p.secondary, 2);

        Self {
            palette: p.clone(),

            // Headings: bold + underline + alternating primary/secondary.
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

            // Inline
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

            // Code block — surface_elevated background reads as a single
            // unit across border, language label, and body.
            code_block_border: Style::default().fg(p.text).bg(code_bg),
            code_block_lang: Style::default()
                .fg(p.code)
                .bg(p.surface)
                .add_modifier(italic),
            code_block_text: Style::default().fg(p.text).bg(code_bg),

            // Blockquote
            blockquote_bar: Style::default().fg(p.secondary),
            blockquote_text: Style::default().add_modifier(italic),

            // Horizontal rule
            rule: Style::default().fg(p.secondary),

            // List markers — accent so bullets / numbers carry a hint
            // of brand colour without competing with body text.
            list_bullet: Style::default().fg(p.accent),
            list_number: Style::default().fg(p.accent),

            // Task list
            task_unchecked: Style::default().fg(p.warning),
            task_checked: Style::default().fg(p.success),
            task_complete_text: Style::default()
                .fg(p.text_muted)
                .add_modifier(Modifier::CROSSED_OUT),
            task_strikethrough: true,

            // Table
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

            // Status bar — surface fill.  Mode chip swaps fg/bg
            // depending on Mode so each mode reads at a glance.
            status_bar: Style::default().bg(p.surface).fg(p.text),
            status_mode_preview: Style::default()
                .bg(p.text_muted)
                .fg(p.surface)
                .add_modifier(bold),
            status_mode_rendered: Style::default().bg(p.primary).fg(p.bg).add_modifier(bold),
            status_mode_raw: Style::default().bg(p.warning).fg(p.bg).add_modifier(bold),
            status_filename: Style::default().fg(p.text).bg(p.surface),
            status_info: Style::default().fg(p.primary).bg(p.surface),
            status_modified: Style::default()
                .fg(p.warning)
                .bg(p.surface)
                .add_modifier(bold),
            status_selection: Style::default()
                .fg(p.primary)
                .bg(p.surface)
                .add_modifier(bold),

            // Hint line — surface_elevated background. Chord badges are
            // primary on the hint surface for readability.
            hint_bar: Style::default().bg(p.surface_elevated).fg(p.text),
            hint_chord: Style::default()
                .fg(p.primary)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            hint_label: Style::default().fg(p.text).bg(p.surface_elevated),

            // Transient messages — escalate in salience.  All sit on
            // the hint_bar surface so they layer cleanly over the
            // chord row.
            transient_info: Style::default().fg(p.text).bg(p.surface_elevated),
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

            // Modal popups
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
            // Use `bg` (the document background) as the fg instead of
            // `text`: most themes have a light `text` and a saturated /
            // light `primary`, so a light-on-light row reads as washed
            // out.  Dark text on the primary fill matches the inverse-
            // text pattern already used by `modal_input_*`.
            modal_item_selected: Style::default().bg(p.primary).fg(p.bg).add_modifier(bold),
            // Persistent selection without focus.  `secondary` as a
            // foreground (no fill) so the affordance reads "marked"
            // without competing with the focused element, which uses
            // a filled `primary` background.  Sits on the modal body's
            // surface so it composes cleanly inside `modal_bg` rows.
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
            // Focused / unfocused inputs share the `primary` bg; the
            // focused variant adds BOLD so the affordance still reads
            // as the active field.
            modal_input_unfocused: Style::default().fg(p.bg).bg(p.primary),
            modal_input_focused: Style::default().fg(p.bg).bg(p.primary).add_modifier(bold),
            modal_button_focused: Style::default()
                .fg(p.primary)
                .add_modifier(Modifier::REVERSED | bold),

            // General — concrete `text` / `bg` so the document area
            // renders with the theme's "blank page" colours rather
            // than letting the terminal's defaults show through.
            // Themes that prefer the terminal's own bg can set
            // `[normal] fg = "Reset"` and `bg = "Reset"` in their TOML.
            normal: Style::default().fg(p.text).bg(p.bg),

            // Selection: `accent` bg with the document `text` fg so
            // colour-coded content stays legible inside the highlight.
            selection: Style::default().bg(p.accent).fg(p.text),

            // Search highlight: `secondary` bg, `bg` fg — distinct
            // from selection so a search hit inside the selection
            // still reads as a hit.
            search_highlight: Style::default().bg(p.secondary).fg(p.bg),

            // Active-line highlight is deferred — leave the field in
            // place so themes can opt in.
            active_line: Style::default(),

            // Editor block cursor — bg mirrors the status-bar mode chip
            // so the cursor reads as the same affordance in both
            // places (per theming.md).  Each variant pairs the chip's
            // bg with a contrasting fg so the underlying character
            // stays legible.
            cursor_preview: Style::default().bg(p.bg_muted).fg(p.surface_elevated),
            cursor_rendered: Style::default().bg(p.primary).fg(p.bg),
            cursor_raw: Style::default().bg(p.warning).fg(p.bg),
            // Generic input cursor — REVERSED so the `▏` glyph inside
            // a modal text input inverts whatever's underneath without
            // needing to know the surrounding bg.
            cursor: Style::default().add_modifier(Modifier::REVERSED),

            // Scrollbar — track in muted bg, thumb in `primary`; the
            // active state inverts via REVERSED so the thumb pops
            // while the user hovers / drags the gutter.
            scrollbar_track: Style::default().fg(p.bg_muted),
            scrollbar_thumb: Style::default().fg(p.primary),
            scrollbar_thumb_active: Style::default().fg(blend(p.primary, p.text, 0.35)),
        }
    }

    /// The "blank page" background colour — exposed so UI code that
    /// blends or composites against the document surface (e.g. the
    /// modal-dim pass) doesn't have to reach into `palette` directly.
    pub fn default_bg(&self) -> Color {
        self.palette.bg
    }

    /// Foreground colour for muted text — used as the Ansi256 fallback
    /// foreground for the modal-dim sweep.
    pub fn text_muted(&self) -> Color {
        self.palette.text_muted
    }

    /// Return the appropriate heading style for a heading level (1–6).
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

    /// Pick the Mode-specific status mode chip style.
    pub fn status_mode_style(&self, mode: crate::editor::Mode) -> Style {
        use crate::editor::Mode::*;
        match mode {
            Preview => self.status_mode_preview,
            Rendered => self.status_mode_rendered,
            Raw => self.status_mode_raw,
        }
    }

    /// Build a `Theme` from a user-authored [`ThemeFile`].
    ///
    /// When `monochrome` is true the file is ignored and the compiled-in
    /// monochrome fallback is returned — preserves the contract that
    /// `ColourDepth::NoColour` terminals never emit colour escapes, even if
    /// a colourful theme file is installed.
    pub fn from_file(file: &super::theme_file::ThemeFile, monochrome: bool) -> Self {
        if monochrome {
            Self::monochrome()
        } else {
            file.into()
        }
    }

    /// Monochrome fallback theme — used when the terminal reports no colour
    /// support (e.g. `TERM=dumb`).  All colours are stripped; text attributes
    /// (bold, italic, underline, strikethrough) are preserved because they
    /// work over SGR regardless of colour depth.
    pub fn monochrome() -> Self {
        let default = Self::default();
        let strip = |s: Style| -> Style {
            // Keep only the modifier bits (bold/italic/underline/etc).
            Style::default().add_modifier(s.add_modifier)
        };

        Self {
            // Palette is preserved verbatim so user code that reads it
            // (e.g. `default_bg` as a fg) still compiles, but no style
            // actually consumes it.
            palette: default.palette.clone(),

            h1: strip(default.h1),
            h1_rule: Style::default(),
            h2: strip(default.h2),
            h3: strip(default.h3),
            h4: strip(default.h4),
            h5: strip(default.h5),
            h6: strip(default.h6),

            bold: strip(default.bold),
            italic: strip(default.italic),
            strikethrough: strip(default.strikethrough),
            highlight: Style::default().add_modifier(Modifier::REVERSED),
            code_span: Style::default().add_modifier(Modifier::REVERSED),
            code_span_dim: Style::default().add_modifier(Modifier::REVERSED | Modifier::DIM),
            link_text: Style::default().add_modifier(Modifier::UNDERLINED),
            link_file: Style::default().add_modifier(Modifier::UNDERLINED),
            link_heading: Style::default().add_modifier(Modifier::UNDERLINED),
            image_placeholder: Style::default().add_modifier(Modifier::ITALIC),
            footnote: Style::default(),

            code_block_border: Style::default(),
            code_block_lang: Style::default().add_modifier(Modifier::ITALIC),
            code_block_text: Style::default(),
            blockquote_bar: Style::default(),
            blockquote_text: Style::default().add_modifier(Modifier::ITALIC),
            rule: Style::default(),

            list_bullet: Style::default(),
            list_number: Style::default(),

            task_unchecked: Style::default(),
            task_checked: Style::default().add_modifier(Modifier::BOLD),
            task_complete_text: Style::default().add_modifier(Modifier::CROSSED_OUT),
            task_strikethrough: true,

            table_border: Style::default(),
            table_header: Style::default().add_modifier(Modifier::BOLD),
            table_header_border: Style::default(),
            table_cell: Style::default(),
            table_row_even: Style::default(),
            table_row_odd: Style::default().add_modifier(Modifier::DIM),
            table_drop_indicator: Style::default()
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            table_drop_target: Style::default().add_modifier(Modifier::REVERSED),
            table_handle: Style::default(),
            table_handle_delete: Style::default().add_modifier(Modifier::BOLD),

            status_bar: Style::default().add_modifier(Modifier::REVERSED),
            status_mode_preview: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            status_mode_rendered: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            status_mode_raw: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            status_filename: Style::default().add_modifier(Modifier::REVERSED),
            status_info: Style::default().add_modifier(Modifier::REVERSED),
            status_modified: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            status_selection: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),

            hint_bar: Style::default(),
            hint_chord: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            hint_label: Style::default(),

            transient_info: Style::default().add_modifier(Modifier::REVERSED),
            transient_success: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            transient_warning: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            transient_error: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),

            modal_bg: Style::default().add_modifier(Modifier::REVERSED),
            modal_title_normal: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            // Monochrome can't colour-code urgency; warning/error fall
            // back to BOLD + REVERSED + DIM so the title still reads as
            // distinct chrome on dim-aware terminals.
            modal_title_warning: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED | Modifier::DIM),
            modal_title_error: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED | Modifier::DIM),
            modal_close_hint: Style::default().add_modifier(Modifier::REVERSED | Modifier::DIM),
            modal_item: Style::default().add_modifier(Modifier::REVERSED),
            modal_item_hint: Style::default().add_modifier(Modifier::REVERSED),
            modal_item_selected: Style::default().add_modifier(Modifier::BOLD),
            // Monochrome can't colour-code distinction; `DIM` reads as
            // "marked but quiet" — distinct from BOLD (focused selection)
            // and plain (unselected) without using REVERSED (which is
            // already the unselected `modal_item` state in monochrome).
            modal_item_selected_unfocused: Style::default().add_modifier(Modifier::DIM),
            modal_item_selected_hint: Style::default().add_modifier(Modifier::BOLD),
            modal_description: Style::default().add_modifier(Modifier::REVERSED),
            modal_section_heading: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            modal_input_unfocused: Style::default().add_modifier(Modifier::REVERSED),
            modal_input_focused: Style::default().add_modifier(Modifier::BOLD),
            modal_button_focused: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),

            normal: Style::default(),
            selection: Style::default().add_modifier(Modifier::REVERSED),
            search_highlight: Style::default().add_modifier(Modifier::REVERSED),
            active_line: Style::default(),
            cursor_preview: Style::default().add_modifier(Modifier::REVERSED),
            cursor_rendered: Style::default().add_modifier(Modifier::REVERSED),
            cursor_raw: Style::default().add_modifier(Modifier::REVERSED),
            cursor: Style::default().add_modifier(Modifier::REVERSED),

            // Scrollbar — glyphs alone disambiguate track from thumb;
            // active state inverts so monochrome users still see it.
            scrollbar_track: Style::default(),
            scrollbar_thumb: Style::default(),
            scrollbar_thumb_active: Style::default().add_modifier(Modifier::REVERSED),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_palette(&Palette::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every field on [`Palette`] in the order it appears in the struct
    /// definition.  When you add a field to `Palette`, add it here so
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
        // NOTE: the `_all` Color array below can't include the new
        // `light: bool` field, so the length check still tracks only
        // colour-typed fields.  New colour fields go here; new non-
        // colour fields are covered by every palette ctor needing to
        // compile.
    ];

    #[test]
    fn palette_fields_list_matches_struct() {
        // Construct a Palette from defaults and read each field.  If a
        // field is renamed or removed, this won't compile.  If a field
        // is added without updating PALETTE_FIELDS, the length
        // assertion at the bottom fails.
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
        // Sanity check that the light built-in is actually distinct
        // from the dark default — otherwise we shipped two themes
        // with the same colour table.
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
        // Iterate every built-in and assert exactly the expected one(s)
        // classify as light.  When new light themes ship this list
        // becomes the central place to register the expectation —
        // forgetting `light: true` in a new palette ctor will fail
        // here (and forgetting `light: false` on a dark theme would
        // too).
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
        // No two colour slots within a built-in palette should hold the
        // same value — duplicates make a theme look monochromatic in the
        // affected affordance pair (e.g. when `accent == error`, all
        // text selections render in the error colour).
        for (name, ctor) in BUILTIN_THEMES {
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
                        "theme {name:?}: slot {a} and {b} share the same colour",
                    );
                }
            }
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
        // Themes with no registered counterpart fall back to the
        // mode default.
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
}
