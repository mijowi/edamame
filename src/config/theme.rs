use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use ratatui::style::{Color, Modifier, Style};

use super::sections::AppearanceMode;
use super::themes::util::{best_contrast, blend};

/// How heavily to mix `code` toward `bg` when deriving the code
/// surface bg.  Closer to 1.0 = closer to `bg` (a barely-tinted
/// neutral); closer to 0.0 = closer to the raw `code` shade.
const CODE_BG_MIX_TOWARD_BG: f32 = 0.92;

/// How heavily to mix `secondary` toward `bg` when deriving the
/// blockquote surface bg.  Mixed further toward `bg` than the code
/// surface: a quote is *prose* — it carries emphasis, links and code
/// spans of its own — so its wash has to stay quiet enough for those
/// to read on top of it.
const QUOTE_BG_MIX_TOWARD_BG: f32 = 0.94;

/// Darken `base` by `level` steps for the heading ramp (0 = base,
/// 1 = medium, 2 = dull).  RGB colors are scaled toward black via a
/// fixed lightness factor per step.  Indexed and named colors can't
/// be cleanly stepped without shifting hue, so they're returned
/// unchanged — built-in indexed-color themes pin the ramp manually
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
/// 1. [`Palette`] — a small flat set of semantic colors (brand, accent,
///    link, status colors, surface tones).  Each name maps to a single
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
/// No hardcoded colors exist outside [`Palette::default`] — every UI site
/// reads from `theme.<field>`.
#[derive(Debug, Clone)]
pub struct Theme {
    /// The named brand-color palette every style is derived from.
    /// Stored on the theme so user code (e.g. modal selection rendering)
    /// can reach for `bg` as a fg color against a colored bg.
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
    /// File link (relative or absolute path) — same `link` color as
    /// `link_text`; themes that want a quieter shade can override the
    /// style directly in TOML.
    pub link_file: Style,
    /// In-document heading link (`#section`) — `link` fg, no underline.
    pub link_heading: Style,
    pub image_placeholder: Style,
    /// Footnote chrome — the bracketed reference marker (`[^1]` →
    /// `[1]`) and a definition's leader / return glyph.  `secondary`
    /// so the markers read as structure rather than prose.
    pub footnote: Style,

    // ── Block elements ────────────────────────────────────────────
    pub code_block_border: Style,
    pub code_block_lang: Style,
    pub code_block_text: Style,
    pub blockquote_bar: Style,
    pub blockquote_text: Style,
    pub rule: Style,

    // ── Frontmatter (YAML / TOML metadata block) ──────────────────
    /// The `---` / `+++` delimiter lines around a metadata block.
    pub frontmatter_delimiter: Style,
    /// The `key:` / `key =` half of a frontmatter line — structural, so
    /// it reads as a field name rather than prose.
    pub frontmatter_key: Style,
    /// The value half of a frontmatter line, and any line the key/value
    /// split doesn't apply to (a list entry, a nested block).
    pub frontmatter_value: Style,

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
    /// Vim NORMAL (and Operator-pending) sub-mode badge.  These three
    /// `status_mode_vim_*` fields are the canonical per-vim-mode colors:
    /// the status chip uses them directly, and the editor cursor mirrors
    /// them (the cursor drops the badge's `BOLD`), so chip and cursor can
    /// never drift.  RAW within INSERT is signalled by `status_mode_raw`
    /// instead — the chip keeps its sub-mode label and shows no `(RAW)`.
    pub status_mode_vim_normal: Style,
    /// Vim INSERT sub-mode badge (and the cursor color in INSERT, except
    /// in Raw view where `status_mode_raw` takes over).
    pub status_mode_vim_insert: Style,
    /// Vim VISUAL / V-LINE sub-mode badge (and cursor color).
    pub status_mode_vim_visual: Style,
    pub status_filename: Style,
    pub status_info: Style,
    pub status_modified: Style,
    /// Style for the `›` separator between segments of the section-path
    /// breadcrumb (`notes.md › Checkpoint 1 › Item 1`).  Dimmed so the
    /// segment names read as the structure and the separators recede.
    pub status_breadcrumb_sep: Style,
    /// Style for ancestor segments of the section-path breadcrumb —
    /// every segment except the deepest one (e.g. `Checkpoint 1` when
    /// the cursor is under `Item 1`).  Dimmed so the deepest segment
    /// (rendered with `status_breadcrumb_current`) stands out as the
    /// "you are here" anchor.
    pub status_breadcrumb_ancestor: Style,
    /// Style for the deepest segment of the section-path breadcrumb —
    /// the heading whose scope directly contains the cursor.  Bold +
    /// accent color so it reads as the active location among the
    /// dimmed ancestor chain.
    pub status_breadcrumb_current: Style,

    // ── Hint line ─────────────────────────────────────────────────
    /// Base background/foreground for the contextual hint line.
    pub hint_bar: Style,
    /// Chord glyph style (e.g. the `^C` in `^C Copy`).  Contrasting
    /// background distinguishes the keybind from its label.
    pub hint_chord: Style,
    /// Label style (e.g. the `Copy` in `^C Copy`).  Blends into the
    /// surrounding hint_bar fill.
    pub hint_label: Style,

    // ── Transient messages ────────────────────────────────────────
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
    /// distinct outlined affordance.  See `docs/dev/theming.md`
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
    /// Renders on top of the character's own style so color-coded content
    /// stays legible.
    pub selection: Style,

    /// Muted variant of `selection`: a washed-out version of the same
    /// hue, used for non-focused search matches so the current match
    /// (painted with `selection` itself) stands out among its
    /// siblings.
    pub selection_muted: Style,

    /// Status-bar match counter (`i/n`) badge while a search flow is
    /// active.  Mirrors `status_mode_diff`'s accent-badge shape.
    pub status_mode_search: Style,

    /// Background style applied to the cursor's current line.  Default
    /// is `Style::default()` (no tint) — the active-line highlight is a
    /// deferred feature; the field exists so themes can opt in early.
    pub active_line: Style,

    /// Unified block cursor used by every modal text input.  Distinct from
    /// the editor cursors because modal inputs aren't tied to editor mode; an
    /// `accent`-colored block by default (monochrome themes fall back to
    /// `REVERSED`).
    pub cursor: Style,

    /// Line-number gutter — right-aligned numbers in `text_muted`.
    pub line_number: Style,

    /// Scrollbar track (the `│` glyph drawn down the gutter behind the
    /// thumb).  The track is only painted when the content overflows.
    pub scrollbar_track: Style,
    /// Scrollbar thumb (the `█` glyph that indicates current position).
    pub scrollbar_thumb: Style,
    /// Scrollbar thumb while the user is hovering the gutter or
    /// dragging the thumb.  RGB themes blend `primary` toward `text`.
    pub scrollbar_thumb_active: Style,

    // ── Diff mode ─────────────────────────────────────────────────
    /// Full-row bg fill on add-side diff lines.  Subtle (30 %-toward
    /// `diff_add`) so the foreground text stays legible.
    ///
    /// **Set a `bg` (and modifiers), not an `fg`.**  The wash is reused
    /// at render time as the Accept chip's background on the decision
    /// divider, and `ui::diff_view::prompt_chip_style` pins that chip's
    /// foreground from `normal` — so a foreground set here reaches the
    /// full-row fill but is dropped on the chip.  The rule is a
    /// convention, not an invariant: the field is user-authorable
    /// (`blend` is a no-op on non-RGB colors, so on an indexed palette a
    /// hand-picked `bg` is the only way to get a focused fill at all),
    /// and the built-ins that hand-pick it — `dark_256`, `light_256`,
    /// `monochrome_dark` — honor it.
    pub diff_add_line: Style,
    /// Full-row bg fill on delete-side diff lines.  Background-only by
    /// the same convention as `diff_add_line`, and reused as the Reject
    /// chip's background.
    pub diff_delete_line: Style,
    /// Add-side bg for hunks that are *not* the focused one — a weaker
    /// tint than `diff_add_line` so the focused hunk's color stands out.
    pub diff_add_line_unfocused: Style,
    /// Delete-side bg for non-focused hunks.  Weaker than `diff_delete_line`.
    pub diff_delete_line_unfocused: Style,
    /// Darkened bg + bold for word-level highlights inside an add line.
    /// Darker than `diff_add` so light text keeps enough contrast.
    pub diff_add_inline: Style,
    /// Darkened bg + bold for word-level highlights inside a delete line.
    pub diff_delete_inline: Style,
    /// Word-level add highlight for hunks that are *not* focused — a
    /// muted tint (no bold) so the within-line change matches the faint
    /// `diff_add_line_unfocused` wash instead of popping at full
    /// saturation.
    pub diff_add_inline_unfocused: Style,
    /// Word-level delete highlight for non-focused hunks.  See
    /// `diff_add_inline_unfocused`.
    pub diff_delete_inline_unfocused: Style,
    /// Decision divider for the focused hunk while still `Pending` —
    /// the `> [ ] Reject [n] Accept [y]` prompt (reject first, mirroring
    /// the old-above-new stacking).  A `secondary` foreground (plus the
    /// caret and bold added at render time) makes the call to action
    /// pop.  This is the only state the prompt renders in, which is why
    /// the render-time Accept / Reject chips — washed in `diff_add_line`
    /// / `diff_delete_line` — never land on a resolved divider's
    /// green/red foreground.
    pub diff_decision_pending: Style,
    /// Decision divider once the hunk is `Accepted` (`[Y] Accepted`).
    pub diff_decision_accepted: Style,
    /// Decision divider once the hunk is `Rejected` (`[N] Rejected`).
    pub diff_decision_rejected: Style,
    /// Decision divider for hunks that are *not* focused — a recessive
    /// chrome strip (`surface` bg, muted fg, no bold).  Used as-is while
    /// the hunk is `Pending`; for `Accepted` / `Rejected` hunks
    /// `build_line` keeps this background but swaps in the per-state
    /// green/red hue and adds `DIM` (see `ui::diff_view`), so a resolved
    /// unfocused divider still signals its decision by color while
    /// staying dimmer than the focused one.
    pub diff_decision_unfocused: Style,
    /// Mode badge for `Mode::Diff`.  Mirrors `status_mode_raw` shape
    /// but on `warning` so the diff session reads as a distinct state.
    pub status_mode_diff: Style,
    /// Whole status bar shifts color in diff mode so the user never
    /// misses the mode change.
    pub status_bar_diff: Style,
    /// Hint bar matches status-bar hue with a softer bg so the hint
    /// text stays readable.
    pub hint_bar_diff: Style,
}

/// Edamame's semantic color palette.  Every theme is built from these
/// eighteen colors plus six heading slots.
///
/// `text` / `bg` are concrete colors rather than terminal defaults
/// because they're used as foregrounds in inverse contexts (e.g. the
/// Rendered-mode mode chip: `primary` bg with `bg` fg), where
/// `Color::Reset` would not produce the right contrast.
///
/// `surface` is the lighter chrome surface (status bar); `surface_elevated`
/// is the heavier chrome surface (hint line, transient messages, modal body)
/// so those layers read as lifted from both the document area and the
/// status bar.
///
/// `diff_add` / `diff_delete` are the base hues for diff review; the
/// focused / unfocused line and inline-span washes are all derived from
/// them in [`Theme::from_palette`] and consumed by `ui::diff_view`.
#[derive(Debug, Clone)]
pub struct Palette {
    /// Default document foreground.
    pub text: Color,
    /// Peripheral / de-emphasized text — strikethrough body,
    /// completed-task text, modal close hint, Preview-mode chip bg.
    pub text_muted: Color,
    /// Default document background.
    pub bg: Color,
    /// Muted surface for table-row stripes, scrollbar track,
    /// Preview-mode cursor bg.  Inline / fenced code use a tinted
    /// shade derived from [`Self::code`] instead, so a code span on
    /// top of a striped row still reads as code.
    pub bg_muted: Color,
    /// Lighter chrome surface (status bar).
    pub surface: Color,
    /// Heavier chrome surface (hint line, transient messages, modal body).
    pub surface_elevated: Color,

    /// Brand color.  Headings (Rendered-mode chip, status info,
    /// modal titles), non-link focus affordances (selected modal
    /// row, modal input fill, button focus, scrollbar thumb).
    pub primary: Color,
    /// Structural chrome color (section headings, search-highlight
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
    /// a mid-grey bg or an indexed color still classifies unambiguously.
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
/// indexed-color built-ins where stepping through the 6×6×6 cube
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

/// The two built-in themes authored against the xterm-256 cube rather
/// than in RGB.  Every other RGB built-in picks 24-bit colors that an
/// indexed terminal quantizes — often to the point of illegibility
/// (identical fg/bg after rounding) — so these are the substitution
/// *targets* below truecolor.  See [`indexed_fallback_theme`].
pub const INDEXED_DARK_THEME: &str = "256 Dark";
pub const INDEXED_LIGHT_THEME: &str = "256 Light";

/// The built-in that resolves every palette slot to [`Color::Reset`],
/// deferring entirely to the terminal's own colors.
pub const MONOCHROME_THEME: &str = "Monochrome Dark";

/// Built-ins that already render correctly without 24-bit color, so a
/// terminal below truecolor must neither substitute them nor warn about
/// them: the two `256 *` themes are authored against the xterm-256 cube,
/// and [`MONOCHROME_THEME`] emits `Color::Reset` everywhere, which is
/// safe at *every* depth including `NoColor`.
///
/// Membership is asserted against [`BUILTIN_THEMES`] by
/// `indexed_safe_themes_are_registered` so a rename can't silently turn
/// one of these back into a substitution candidate.
pub const INDEXED_SAFE_THEMES: &[&str] =
    &[INDEXED_DARK_THEME, INDEXED_LIGHT_THEME, MONOCHROME_THEME];

/// Pick the indexed-color theme to substitute on a terminal without
/// 24-bit color, or `None` when `current` is already one of
/// [`INDEXED_SAFE_THEMES`] (nothing to do) — which is also what makes
/// the substitution idempotent across reloads.
///
/// The dark/light choice follows the *current theme's* appearance so
/// a user on a light theme doesn't get flipped to a dark one by a
/// capability downgrade; `configured` (the `appearance` config key) is
/// the fallback for a theme that can't be classified, e.g. a user
/// theme file that has since been deleted.
pub fn indexed_fallback_theme(current: &str, configured: AppearanceMode) -> Option<&'static str> {
    if INDEXED_SAFE_THEMES.contains(&current) {
        return None;
    }
    Some(match theme_appearance(current).unwrap_or(configured) {
        AppearanceMode::Dark => INDEXED_DARK_THEME,
        AppearanceMode::Light => INDEXED_LIGHT_THEME,
    })
}

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

    // A `--no-config` run offers built-ins only.  This list feeds the
    // theme picker, the settings overlay's cycle, and the export-theme
    // source list, so without the gate a session started specifically to
    // rule the user's config out could still pick a `themes/*.toml` off
    // disk and apply it — the read half of the flag, enforced at the
    // read site.  See [`crate::config::persistence`].
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
    /// is derived from a palette entry; this function is the single
    /// source of truth for those assignments (`docs/dev/theming.md`
    /// carries the conventions behind them, not the mapping itself).
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

        // Blockquote surface: the bar's own hue, mixed almost all the
        // way to `bg`.  Same `blend` caveat as `code_bg` — it returns
        // `p.secondary` unchanged for non-RGB palettes, so the
        // indexed-cube built-ins pin `blockquote_text` by hand after
        // `from_palette` returns.
        let quote_bg = blend(p.secondary, p.bg, QUOTE_BG_MIX_TOWARD_BG);

        // Heading ramp alternates `primary` and `secondary`, getting
        // progressively duller / darker with each level.  RGB themes
        // get a tinted ramp; indexed / named colors fall back to the
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

            // Blockquote — a subtle background wash rather than a text
            // attribute.  It used to be a blanket ITALIC, which left
            // `*emphasis*` inside a quote with nothing to say (issue
            // #33) and read as a claim about the quoted text's tone.  A
            // wash marks the region instead, the way the code surface
            // does, and leaves every inline style free.
            blockquote_bar: Style::default().fg(p.secondary),
            blockquote_text: Style::default().bg(quote_bg),

            // Horizontal rule
            rule: Style::default().fg(p.secondary),

            // Frontmatter — quiet by design: it is data about the
            // document rather than part of it, so it must not compete
            // with the first heading below it.
            frontmatter_delimiter: Style::default()
                .fg(p.text_muted)
                .add_modifier(Modifier::DIM),
            frontmatter_key: Style::default().fg(p.secondary),
            frontmatter_value: Style::default().fg(p.text_muted),

            // List markers — accent so bullets / numbers carry a hint
            // of brand color without competing with body text.
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
            // Per-vim-mode badge colors, mirrored by the editor cursor:
            // NORMAL = primary (resting/navigation home), INSERT = success
            // (the green-for-insert vim convention), VISUAL / V-LINE =
            // secondary (selection).  `fg = bg` keeps the cursor's
            // underlying glyph legible when the cursor reads these.
            status_mode_vim_normal: Style::default().bg(p.primary).fg(p.bg).add_modifier(bold),
            status_mode_vim_insert: Style::default().bg(p.success).fg(p.bg).add_modifier(bold),
            status_mode_vim_visual: Style::default().bg(p.secondary).fg(p.bg).add_modifier(bold),
            // Filename rendered bold so it anchors the left side of the
            // status bar alongside the bold accented "current section"
            // chip on its right; the two together frame the rest of the
            // breadcrumb chain.
            status_filename: Style::default().fg(p.text).bg(p.surface).add_modifier(bold),
            // Positional reference data (cursor pos, line count, %).
            // Muted on purpose so the lone primary accent on the bar is
            // the current-section breadcrumb — the two no longer compete.
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
            // Focused input is filled (`primary` bg, inverse-text fg,
            // bold) so it clearly reads as the active field.  Unfocused
            // input is outlined (`primary` fg, no fill) — same
            // "filled vs outlined" convention used for selected items
            // (see `modal_item_selected_unfocused`).  Without this
            // contrast, an unfocused input on first render is easily
            // mistaken for a focused button.
            modal_input_unfocused: Style::default().fg(p.primary).bg(p.surface_elevated),
            modal_input_focused: Style::default().fg(p.bg).bg(p.primary).add_modifier(bold),
            modal_button_focused: Style::default()
                .fg(p.primary)
                .add_modifier(Modifier::REVERSED | bold),

            // General — concrete `text` / `bg` so the document area
            // renders with the theme's "blank page" colors rather
            // than letting the terminal's defaults show through.
            // Themes that prefer the terminal's own bg can set
            // `[normal] fg = "Reset"` and `bg = "Reset"` in their TOML.
            normal: Style::default().fg(p.text).bg(p.bg),

            // Selection: `accent` bg with whichever of `text` / `bg`
            // contrasts better against it, so a theme whose `accent`
            // sits near its `text` luminance (e.g. GitHub's cyan on
            // light-grey ink) doesn't render selected text as
            // low-contrast mud.  Indexed / named colors can't be
            // measured and fall back to `text` (the prior behavior).
            selection: Style::default()
                .bg(p.accent)
                .fg(best_contrast(p.accent, p.text, p.bg)),

            // Muted selection: the selection hue washed toward the
            // surface so non-focused search matches recede behind the
            // `selection`-painted current match.  Same contrast pick
            // against the washed bg.
            selection_muted: {
                let bg = blend(p.surface, p.accent, 0.45);
                Style::default().bg(bg).fg(best_contrast(bg, p.text, p.bg))
            },

            // Search match-counter badge — secondary accent so it
            // reads apart from the warning-hued diff badge.
            status_mode_search: Style::default()
                .bg(p.secondary)
                .fg(p.bg)
                .add_modifier(Modifier::BOLD),

            // Active-line highlight is deferred — leave the field in
            // place so themes can opt in.
            active_line: Style::default(),

            // Unified modal input cursor — a solid `accent` block shared by
            // every modal text field, so typing in a prompt looks the same
            // everywhere and reads as its own context, distinct from the
            // editor cursor (which derives from the per-mode status chip).
            cursor: Style::default().bg(p.accent).fg(p.bg),

            // Line-number gutter — muted fg on the document bg so
            // numbers recede behind the content.
            line_number: Style::default().fg(p.text_muted),

            // Scrollbar — track in muted bg, thumb in `primary`; the
            // active state blends toward `text` so the thumb pops
            // while the user hovers / drags the gutter.
            scrollbar_track: Style::default().fg(p.bg_muted),
            scrollbar_thumb: Style::default().fg(p.primary),
            scrollbar_thumb_active: Style::default().fg(blend(p.primary, p.text, 0.35)),

            // Diff mode — line / inline / status bar / hint bar.
            // Line bg is 30 % toward the saturated diff color, mixed
            // with `surface` so it reads as a chrome tint rather than
            // a saturated stripe.  Inline highlights use the
            // saturated palette color + bold.  Falls back to plain
            // styles on non-Rgb palettes — `blend` returns the
            // first argument unchanged in that case, which is the
            // best we can do without inventing a hue.
            // Focused hunk: stronger fill so the active change stands
            // out; non-focused hunks: a faint wash so they recede.  The
            // focused fill is then pulled back toward `bg` so it sits a
            // shade darker than the saturated inline-change highlight —
            // that contrast is what makes within-line edits legible
            // against the surrounding row.
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
            // Inline highlights darken the saturated diff color toward
            // the bg so light foreground text keeps enough contrast.
            diff_add_inline: Style::default()
                .bg(blend(p.diff_add, p.bg, 0.35))
                .add_modifier(bold),
            diff_delete_inline: Style::default()
                .bg(blend(p.diff_delete, p.bg, 0.35))
                .add_modifier(bold),
            // Unfocused inline highlights are a surface-derived tint
            // (like the `_line_unfocused` washes) rather than the
            // darkened-saturated focused style, and drop the bold — so a
            // changed word reads as a slightly deeper patch within the
            // faint hunk (0.20 vs. the 0.07 line wash) without competing
            // with the focused hunk.
            diff_add_inline_unfocused: Style::default().bg(blend(p.surface, p.diff_add, 0.20)),
            diff_delete_inline_unfocused: Style::default().bg(blend(
                p.surface,
                p.diff_delete,
                0.20,
            )),
            // Decision divider carries a full-width neutral chrome
            // background so the accept/reject checkbox reads as the
            // actionable strip between the delete and add sides rather
            // than a bare gap.  The background is a plain surface (not a
            // `secondary` tint) so the colored foregrounds keep full
            // contrast: the focused divider uses the heavier
            // `surface_elevated` and a `secondary` foreground on the
            // pending prompt (the call to action pops); the resolved
            // states keep their green/red hue so color still encodes the
            // decision.
            diff_decision_pending: Style::default().fg(p.secondary).bg(p.surface_elevated),
            diff_decision_accepted: Style::default()
                .fg(p.diff_add)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            diff_decision_rejected: Style::default()
                .fg(p.diff_delete)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            // Unfocused divider: the lighter `surface` (vs. the focused
            // `surface_elevated`) so it recedes a step while still
            // reading as a chrome strip, with a muted fg and no bold.
            // `build_line` derives the resolved unfocused styling from
            // this plus the per-state hue + `DIM` (see `diff_view`).
            diff_decision_unfocused: Style::default().fg(p.text_muted).bg(p.surface),
            status_mode_diff: Style::default().bg(p.warning).fg(p.bg).add_modifier(bold),
            // Bottom region in diff mode: a muted red wash on the hint
            // line (top) and a muted green wash on the status line
            // (bottom) — mirroring the deletes-above / adds-below
            // stacking in the document.  Tints, not fills, so the bars
            // read as "diff" without being mistaken for an in-document
            // hunk and without sacrificing text legibility.
            status_bar_diff: Style::default()
                .bg(blend(p.surface, p.diff_add, 0.22))
                .fg(p.text),
            hint_bar_diff: Style::default()
                .bg(blend(p.surface_elevated, p.diff_delete, 0.22))
                .fg(p.text),
        }
    }

    /// The "blank page" background color — exposed so UI code that
    /// blends or composites against the document surface (e.g. the
    /// modal-dim pass) doesn't have to reach into `palette` directly.
    pub fn default_bg(&self) -> Color {
        self.palette.bg
    }

    /// Foreground color for muted text — used as the Ansi256 fallback
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
            Diff => self.status_mode_diff,
        }
    }

    /// Build a `Theme` from a user-authored [`crate::config::theme_file::ThemeFile`].
    ///
    /// When `monochrome` is true the file is ignored and the compiled-in
    /// monochrome fallback ([`super::themes::monochrome_dark::theme`]) is
    /// returned — preserves the contract that `ColorDepth::NoColor`
    /// terminals never emit color escapes, even if a colorful theme file is
    /// installed.
    pub fn from_file(file: &super::theme_file::ThemeFile, monochrome: bool) -> Self {
        if monochrome {
            super::themes::monochrome_dark::theme()
        } else {
            file.into()
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

    /// The picker, the settings cycle, and the export-theme source list
    /// all build from this, so a `--no-config` run offering a user theme
    /// here is how the flag's read half leaks: selecting it loads the
    /// very `themes/*.toml` the run exists to rule out.  The second half
    /// proves the omission came from the gate, not from a missed folder.
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

    /// The built-in theme registry, pinned with each theme's light/dark
    /// classification.
    ///
    /// `docs/themes.md` lists these by name, split into Dark and Light
    /// groups, and a user searching the picker for a theme the docs
    /// promised is a bad first impression.  Accepting a change to this
    /// snapshot is the reminder to update that list.
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
        // color-typed fields.  New color fields go here; new non-
        // color fields are covered by every palette ctor needing to
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
        // with the same color table.
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
        // No two color slots within a built-in palette should hold the
        // same value — duplicates make a theme look monochromatic in the
        // affected affordance pair (e.g. when `accent == error`, all
        // text selections render in the error color).
        for (name, ctor) in BUILTIN_THEMES {
            // Monochrome intentionally collapses every palette slot to
            // `Color::Reset` so any site that reads `palette.<x>`
            // directly emits a terminal-default escape.  The duplicate
            // check doesn't apply.
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
        // A user on a light theme must not be flipped to a dark one by
        // a capability downgrade, and vice versa.  The `configured`
        // argument is deliberately the *opposite* of each theme's own
        // appearance here to prove it isn't what's consulted.
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
        // An unresolvable name (deleted user theme file) can't be
        // classified, so the `appearance` config key decides.
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
        // Idempotence for the two `256 *` targets: the substituted theme
        // must not itself trigger a substitution, or a reload would fire
        // the notice forever.  And `Monochrome Dark` is already correct
        // at any depth, so swapping it — for a *less* safe palette, and
        // with a modal to explain the swap — would be pure noise.
        for name in INDEXED_SAFE_THEMES {
            assert_eq!(indexed_fallback_theme(name, AppearanceMode::Dark), None);
            assert_eq!(indexed_fallback_theme(name, AppearanceMode::Light), None);
        }
    }

    #[test]
    fn indexed_safe_themes_are_registered() {
        // A rename in BUILTIN_THEMES that misses this list would leave a
        // safe theme silently substitutable.
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
