use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorConfig {
    /// Number of spaces per tab stop.
    pub tab_width: usize,
    /// When true, code block lines that exceed the terminal width are wrapped.
    /// Default: false (long lines extend beyond the visible area without wrapping).
    pub code_block_wrap: bool,
    /// When true, long lines in the document wrap at the terminal width.
    /// Default: true.
    pub line_wrap: bool,
    /// When true, multiple consecutive blank lines in the source are rendered
    /// as multiple blank lines in the output.  Standard Markdown collapses them
    /// to a single blank line; this option preserves the author's intent.
    /// Default: true.
    pub preserve_blank_lines: bool,
    /// When true (default), pressing Up/Down in rendered/hybrid mode moves the
    /// cursor by **visual** lines (accounting for word-wrap), so the cursor
    /// stays at the same horizontal column on the screen.  When false, movement
    /// is by **logical** buffer lines (one `\n`-terminated line per step).
    pub visual_line_nav: bool,
    /// When true, the startup notice that lists unsupported terminal features
    /// is skipped.  Set by the `[Don't show this again]` button on the notice
    /// modal.
    pub suppress_capability_warnings: bool,
    /// When true, the first-run welcome modal is shown at startup.  Defaults
    /// to `true` so a fresh install sees the welcome on first launch; the
    /// modal's "Show again next time" toggle (default off) writes `false`
    /// here when the user saves.  Also gates the four legacy startup prompts
    /// (capability notice, images-enabled, remote-image, diagrams) — they
    /// are suppressed while the welcome is still pending so the user is never
    /// double-prompted.
    pub show_welcome: bool,
    /// Lines advanced per mouse-wheel tick.  Default 1 — users can bump this
    /// to 2 or 3 for a coarser, faster feel at the cost of fine-grained
    /// control.  The keyboard `ScrollUp` / `ScrollDown` actions always step
    /// by exactly one line and are not affected by this setting.
    pub mouse_scroll_lines: usize,
    /// Bottom-region layout.  `"two_line"` (default) renders a hint line
    /// above the persistent status line; `"compact"` collapses to just
    /// the status line, reachable hint chords via the `?` popover.
    pub status_bar: StatusBarLayout,
    /// Duration (milliseconds) that a non-sticky transient message
    /// overlays the hint line before auto-expiring.  Errors ignore this
    /// and remain visible until the user dismisses them with Escape.
    pub transient_ms: u64,
    /// When true, the editor content area is capped to `max_width_cols`
    /// columns and centred horizontally inside the terminal, with the
    /// surrounding gutters painted in `theme.normal`.  Off by default —
    /// the editor uses the full terminal width.  When the terminal is
    /// narrower than the cap the cap has no effect; the full terminal
    /// width is used.  The bottom status / hint region always spans the
    /// full terminal width regardless of this setting.
    pub max_width_enabled: bool,
    /// Maximum content width in columns when `max_width_enabled` is
    /// true.  Clamped to a floor of 20 at use sites to prevent
    /// pathological narrow values that would break layout.  Default: 100.
    pub max_width_cols: usize,
    /// When true, H1 headings render as 4-row "big text" via the
    /// `tui-big-text` widget (Quadrant pixel size — uses ▀▄▌▐ block
    /// glyphs).  Falls back to the regular one-line styled rendering
    /// when the title would exceed the viewport width or contains
    /// non-ASCII characters (font8x8 only covers ASCII).  Default: true.
    pub big_h1: bool,
}

/// Floor applied to `EditorConfig::max_width_cols` at every use site so a
/// stray `0` or single-digit value can't break layout.
pub const MAX_WIDTH_COLS_MIN: usize = 20;

/// User-selected appearance mode.  Independent of `Config::theme`: the
/// mode filters which themes appear in the picker and which counterpart
/// is previewed when the user toggles modes, but does not directly
/// dictate the active theme name.  The picker / settings overlay are
/// responsible for keeping `theme` consistent with `appearance` when
/// the user changes either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppearanceMode {
    #[default]
    Dark,
    Light,
}

impl AppearanceMode {
    /// The mode on the other side of the toggle.  Used by the picker's
    /// Tab / Left / Right / pill-click handlers and by the settings
    /// overlay's Appearance cycle row.
    pub fn opposite(self) -> Self {
        match self {
            AppearanceMode::Dark => AppearanceMode::Light,
            AppearanceMode::Light => AppearanceMode::Dark,
        }
    }
}

/// How the bottom status region is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusBarLayout {
    /// Two rows: hint line above, persistent status below.  Default.
    #[default]
    TwoLine,
    /// One row: persistent status only; hints via the `?` popover.
    Compact,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            tab_width: 4,
            code_block_wrap: false,
            line_wrap: true,
            preserve_blank_lines: true,
            visual_line_nav: true,
            suppress_capability_warnings: false,
            show_welcome: true,
            mouse_scroll_lines: 1,
            status_bar: StatusBarLayout::default(),
            transient_ms: 1500,
            max_width_enabled: false,
            max_width_cols: 100,
            big_h1: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModalConfig {
    /// Which modal handler to use. Currently only "default" is supported.
    pub handler: String,
}

impl Default for ModalConfig {
    fn default() -> Self {
        Self {
            handler: "default".into(),
        }
    }
}

/// Table-editing configuration.
///
/// `show_buttons` governs whether the row/column buttons — the `⠿`
/// reorder grips, the `⇔` resize glyph, and the `✕` row/column delete
/// glyphs — are rendered and hit-tested.  Defaults to `true`: the
/// renderer still checks the terminal's detected `Capabilities::mouse`
/// flag before enabling the feature at runtime, so setting this to
/// `true` on a mouseless terminal is a no-op — `App::new` overrides it
/// to `false` when `capabilities.mouse` is absent so persisted config
/// stays faithful to what the user actually sees.
///
/// `row_striping` (Phase 13): when true, alternating data rows are filled
/// with `Theme::table_row_even` / `Theme::table_row_odd` to aid visual
/// scanning on wide tables.  Off by default so users who prefer plain
/// borders see no change.
///
/// `warn_on_width_injection` (Phase 13): when true, the first column-border
/// drag on a table without a `<!-- tui-columns: [...] -->` comment opens a
/// modal warning that committing the resize will inject the comment into
/// the Markdown source.  Set false (either via the modal's "Continue and
/// don't ask again" button or directly in `config.toml`) to skip the
/// warning on subsequent drags.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TableConfig {
    pub show_buttons: bool,
    pub row_striping: bool,
    pub warn_on_width_injection: bool,
}

impl Default for TableConfig {
    fn default() -> Self {
        Self {
            show_buttons: true,
            row_striping: true,
            warn_on_width_injection: true,
        }
    }
}

/// Policy for fetching images referenced by `http(s)://` URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteImagePolicy {
    /// Prompt the user the first time a document with remote images is opened.
    #[default]
    Ask,
    /// Always fetch remote images without prompting.
    Always,
    /// Never fetch remote images; always fall back to the placeholder.
    Never,
}

/// Master switch for inline image rendering.  `Ask` prompts the user the
/// first time a document with images is opened; `Always` renders without
/// prompting; `Never` keeps the `[Image: alt]` placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesEnabled {
    /// Prompt the user the first time a document with images is opened.
    #[default]
    Ask,
    /// Always render images inline.
    Always,
    /// Never render images — always fall back to the `[Image: alt]` placeholder.
    Never,
}

/// Image-rendering configuration.
///
/// `max_width` / `max_height` are ceilings in terminal cells; each image
/// reserves at most this many rows, and the inline renderer clamps to this
/// width so a single oversized image never takes over the viewport.  Values
/// are applied verbatim by `ratatui_image`'s `Resize::Fit` path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImagesConfig {
    /// Master switch — `"ask"` (default) prompts on first document with
    /// images, `"always"` renders without prompting, `"never"` always
    /// falls back to the placeholder.
    pub enabled: ImagesEnabled,
    /// Maximum width (in terminal cells) for a single image.
    pub max_width: usize,
    /// Maximum height (in terminal cells) for a single image.
    pub max_height: usize,
    /// Policy for fetching `http(s)://` images.
    pub remote_policy: RemoteImagePolicy,
}

impl Default for ImagesConfig {
    fn default() -> Self {
        Self {
            enabled: ImagesEnabled::Ask,
            max_width: 100,
            max_height: 24,
            remote_policy: RemoteImagePolicy::Ask,
        }
    }
}

/// Master switch for inline diagram rendering (e.g. mermaid).  `Ask`
/// prompts the user the first time a document with diagrams is opened;
/// `Always` renders without prompting; `Never` keeps the placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagramsEnabled {
    /// Prompt the user the first time a document with diagrams is opened.
    #[default]
    Ask,
    /// Always render diagrams inline.
    Always,
    /// Never render diagrams — always fall back to the placeholder.
    Never,
}

/// Diagram-rendering configuration.  Mirrors [`ImagesConfig::enabled`] —
/// kept separate so a user can opt in to images but not diagrams (or
/// vice-versa).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagramsConfig {
    /// Master switch — `"ask"` (default) prompts on first document with
    /// diagrams, `"always"` renders without prompting, `"never"` always
    /// falls back to the placeholder.
    pub enabled: DiagramsEnabled,
}

impl Default for DiagramsConfig {
    fn default() -> Self {
        Self {
            enabled: DiagramsEnabled::Ask,
        }
    }
}

/// Export configuration (Phase 16).
///
/// HTML is the single built-in export target; it doubles as the intermediate
/// format for user-defined custom commands that produce PDF, DOCX, etc. by
/// piping the generated HTML through an external tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportConfig {
    pub html: HtmlExportConfig,
    /// User-defined extra export entries that appear alongside
    /// `Export HTML` in the command palette.  Each runs an external
    /// command with `{html}` / `{out}` path substitution.
    pub custom: Vec<CustomExportEntry>,
}

/// HTML export settings.  `stylesheet = "builtin"` (the default) uses the
/// compiled-in CSS bundled with edamame.  Any other value is treated as a
/// filesystem path to a user stylesheet.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HtmlExportConfig {
    /// Either the sentinel `"builtin"` or an absolute / home-relative path
    /// to a user CSS file.  Read at export time; parse errors are surfaced
    /// to the user via the export error message.
    pub stylesheet: String,
    /// When true, local `![alt](relative/path.png)` references are read
    /// from disk at export time and embedded as `data:` URIs so the HTML
    /// is fully self-contained.  Default: false (keeps output compact and
    /// portable alongside the asset directory).
    pub inline_images: bool,
    /// Phase 17 — when true (the default), fenced ```mermaid code blocks
    /// are rendered to inline SVG via `mermaid-rs-renderer` and wrapped
    /// in a `<figure class="mermaid-diagram">`.  On render failure the
    /// block falls back to `<pre><code class="language-mermaid">` so the
    /// source is never lost.  Set false to force the code-block form
    /// (e.g. for pipelines that ship their own client-side mermaid.js).
    pub diagrams: bool,
}

impl Default for HtmlExportConfig {
    fn default() -> Self {
        Self {
            stylesheet: "builtin".into(),
            inline_images: false,
            diagrams: true,
        }
    }
}

/// A single user-configured custom-export entry.  Shows up in the
/// command palette as "Export <name>".  `command` is run verbatim with
/// two placeholders substituted:
///
/// * `{html}` — path to the just-generated HTML file (temp file owned
///   by the exporter; deleted after the command exits).
/// * `{out}` — path to the final output file (source-stem with the
///   configured `extension` appended).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomExportEntry {
    /// Human-readable label — appears as "Export <name>" in the palette.
    pub name: String,
    /// argv-style command.  Element 0 is the executable; remaining
    /// elements are arguments with `{html}` / `{out}` substitution.
    pub command: Vec<String>,
    /// Extension (no leading dot) for the output file.
    pub extension: String,
}

/// Developer/diagnostic settings.  Kept separate from `[editor]` because these
/// knobs govern logging and debug tooling, not editing behaviour.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DevConfig {
    /// When true, `tracing` logs are written to the XDG data dir (e.g.
    /// `~/.local/share/edamame/`).  Off by default so the TUI stays silent.
    pub logging: bool,
}
