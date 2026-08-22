use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorConfig {
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
    /// Fingerprints of terminals (TERM_PROGRAM + TERM + capability tuple)
    /// the user has already seen the capabilities notice for.  The notice
    /// fires once per new fingerprint and is silenced on dismiss; subsequent
    /// launches in the same terminal stay quiet, while launches in a
    /// previously-unseen terminal re-fire the notice.  Built by
    /// [`crate::terminal::Capabilities::fingerprint`].
    pub seen_terminal_fingerprints: Vec<String>,
    /// When true, the first-run welcome modal is shown at startup.  Defaults
    /// to `true` so a fresh install sees the welcome on first launch; the
    /// modal's "Show again next time" toggle (default off) writes `false`
    /// here when the user saves.  Also gates the four legacy startup prompts
    /// (images-enabled, remote-image, diagrams, capabilities notice) — they
    /// are suppressed while the welcome is still pending so the user is never
    /// double-prompted.
    pub show_welcome: bool,
    /// When true (the default), edamame checks GitHub for a newer
    /// release at startup — at most once per 24 h, and silently unless
    /// there is genuinely newer news the user hasn't been shown yet.
    /// Turning it off suppresses only the *automatic* check: the About
    /// page's "Check for updates" button and the command-palette action
    /// always check on request.  See `docs/security.md` for the network
    /// posture, and `app::update_check` for the mechanics.
    pub check_for_updates: bool,
    /// Unix epoch seconds of the last automatic release check, stamped
    /// when the check is *spawned* rather than when it resolves — a
    /// worker that hangs, or a process killed before the result lands,
    /// must not re-check on every launch.  `0` means never checked, so
    /// a fresh install checks on first run.  Bookkeeping written by
    /// edamame, not a knob to hand-edit.
    pub last_update_check: u64,
    /// The release tag the startup notice has already been shown for,
    /// so a user who has seen (or explicitly looked up) a version isn't
    /// told about it again on every launch.  Empty until the first
    /// notice.  Bookkeeping written by edamame, not a knob to
    /// hand-edit.
    pub update_notified_for: String,
    /// The version that last ran, used to show the release notes once
    /// after an upgrade (`app::post_upgrade`).  Read from the bundled
    /// `CHANGELOG.md`, so this is unrelated to the release *check*
    /// above and involves no network.
    ///
    /// Empty means no version has been recorded yet, which a fresh
    /// install and an upgrade from a build predating this field share;
    /// `show_welcome` is what tells them apart, since only a returning
    /// user could have turned it off.  Bookkeeping written by edamame,
    /// not a knob to hand-edit; the About page's `[ Release notes ]`
    /// button is how the notes are read again, and it touches neither
    /// this field nor the notice.
    pub last_version_seen: String,
    /// When true, line numbers are displayed in a left gutter in all three
    /// modes (Preview, Rendered, Raw).  Numbers are right-aligned and styled
    /// with the theme's `line_number` style (derived from `text_muted`).
    /// Default: false.
    pub show_line_numbers: bool,
    /// Lines advanced per mouse-wheel tick.  Default 1 — users can bump this
    /// to 2 or 3 for a coarser, faster feel at the cost of fine-grained
    /// control.  The keyboard `ScrollUp` / `ScrollDown` actions always step
    /// by exactly one line and are not affected by this setting.
    pub mouse_scroll_lines: usize,
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
    /// non-ASCII characters (font8x8 only covers ASCII).  Default: false.
    pub big_h1: bool,
    /// When true (the default), fenced code blocks are syntax
    /// highlighted using the language named in the opening fence
    /// (```` ```rust ````).  There is no auto-detection: a fence with
    /// no language, or one naming a grammar we do not ship, renders as
    /// plain code exactly as it did before this setting existed.
    /// Default: true.
    pub syntax_highlighting: bool,
    /// When true, the buffer is silently written to disk after
    /// `autosave_idle_ms` of typing inactivity.  Only fires for buffers
    /// with an associated file path; an unnamed buffer never autosaves.
    /// Default: false.
    pub autosave_enabled: bool,
    /// Idle window (ms) the user must stop editing for before the
    /// pending dirty buffer is autosaved.  Every keystroke resets the
    /// timer (debounce, not throttle), so a typing burst produces at
    /// most one autosave at the end.  Default: 5000.
    pub autosave_idle_ms: u64,
    /// When true (default), an external write detected while the
    /// buffer is **clean** opens diff-review mode so the change is
    /// surfaced hunk by hunk before it replaces what is on screen.
    /// When false, a clean buffer is silently reloaded from disk
    /// instead.  A **dirty** buffer always prompts the conflict modal
    /// regardless of this setting (whose `[Merge]` button still enters
    /// diff review on demand) — unsaved edits are never discarded
    /// silently.
    pub diff_on_change: bool,
    /// When true (default), the explanatory modal shown on entering
    /// diff-review mode is displayed.  The modal's "Don't show this
    /// again" checkbox flips this off so subsequent reviews open
    /// straight into the diff view.
    pub show_diff_intro: bool,
    /// When true (default), the editor cursor blinks on a fixed
    /// cadence (`cursor_blink_ms`); when false the cursor is drawn
    /// solid and never hidden.  Exposed as an on/off toggle in the
    /// settings overlay; the cadence itself stays file-only.
    pub cursor_blink: bool,
    /// Cursor blink half-period in milliseconds — the cursor toggles
    /// between visible and hidden every `cursor_blink_ms`.  Only
    /// consulted when `cursor_blink` is true.  Default: 530 (the
    /// classic terminal cadence).  File-only; the overlay exposes the
    /// on/off toggle but not this value.
    pub cursor_blink_ms: u64,
}

/// Floor applied to `EditorConfig::max_width_cols` at every use site so a
/// stray `0` or single-digit value can't break layout.
pub const MAX_WIDTH_COLS_MIN: usize = 20;

/// Exclusive lower bound for `EditorConfig::autosave_idle_ms`.  Values
/// at or below this are rejected at load time with a warning so an
/// accidental small / zero value can't autosave on every keystroke.
pub const AUTOSAVE_IDLE_MS_MIN_EXCLUSIVE: u64 = 1000;
/// Exclusive upper bound for `EditorConfig::autosave_idle_ms` (10
/// minutes).  Past this the feature is effectively off and the user
/// almost certainly wants to disable autosave outright instead.
pub const AUTOSAVE_IDLE_MS_MAX_EXCLUSIVE: u64 = 600_000;
/// Default debounce window used by `EditorConfig::default` and by the
/// loader's out-of-range fallback in `validate_main_config`.  Kept
/// alongside the bounds so all three numbers move together.
pub const AUTOSAVE_IDLE_MS_DEFAULT: u64 = 5000;

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
    /// The mode on the other side of the toggle.  Used by the theme
    /// picker's Tab / Left / Right / slider-click handlers.  (The
    /// settings overlay has no Appearance row — the picker is the only
    /// surface that changes this.)
    pub fn opposite(self) -> Self {
        match self {
            AppearanceMode::Dark => AppearanceMode::Light,
            AppearanceMode::Light => AppearanceMode::Dark,
        }
    }
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            code_block_wrap: false,
            line_wrap: true,
            preserve_blank_lines: true,
            visual_line_nav: true,
            seen_terminal_fingerprints: Vec::new(),
            show_welcome: true,
            check_for_updates: true,
            last_update_check: 0,
            update_notified_for: String::new(),
            last_version_seen: String::new(),
            show_line_numbers: false,
            mouse_scroll_lines: 1,
            transient_ms: 1500,
            max_width_enabled: false,
            max_width_cols: 100,
            big_h1: false,
            syntax_highlighting: true,
            autosave_enabled: false,
            autosave_idle_ms: AUTOSAVE_IDLE_MS_DEFAULT,
            diff_on_change: true,
            show_diff_intro: true,
            cursor_blink: true,
            cursor_blink_ms: 530,
        }
    }
}

/// Handler name written to `config.modal.handler` for vim modal editing.
pub const VIM_HANDLER: &str = "vim";
/// Handler name for the default (non-modal) editing handler.
pub const DEFAULT_HANDLER: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModalConfig {
    /// Which modal handler to use: [`DEFAULT_HANDLER`] or [`VIM_HANDLER`].
    pub handler: String,
}

impl Default for ModalConfig {
    fn default() -> Self {
        Self {
            handler: DEFAULT_HANDLER.into(),
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
/// `row_striping`: when true (the default), alternating data rows are
/// filled with `Theme::table_row_even` / `Theme::table_row_odd` to aid
/// visual scanning on wide tables.
///
/// `warn_on_width_injection`: when true, the first column-border
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

/// Export configuration.
///
/// HTML is the only export target that has shipped.  It is *designed* to
/// double as the intermediate format for user-defined custom commands that
/// produce PDF, DOCX, etc., and the machinery for that exists
/// ([`CustomExportEntry`], [`export::spawn_custom_export`]) — but nothing
/// calls it yet, so `custom` is inert.
///
/// [`export::spawn_custom_export`]: crate::export::spawn_custom_export
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportConfig {
    pub html: HtmlExportConfig,
    /// User-defined extra export entries.  **Parsed but not yet wired up:**
    /// nothing reads this field, so entries a user writes here have no
    /// effect and produce no warning.  `config/config.toml` documents no
    /// `[[export.custom]]` block for that reason — restore it, and the
    /// palette entries described on [`CustomExportEntry`], when the feature
    /// actually ships.
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
    /// When true (the default), fenced ```mermaid code blocks
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

/// A single user-configured custom-export entry.
///
/// **Not reachable yet.** The intent is for each entry to show up in the
/// command palette as `Export <name>`, but nothing constructs those palette
/// items or calls [`export::spawn_custom_export`], so an entry in
/// `config.toml` is parsed and then ignored.  Everything below describes the
/// runner's contract, which is implemented and tested — only the UI hookup
/// is missing.
///
/// `command` is run verbatim with two placeholders substituted:
///
/// * `{html}` — path to the just-generated HTML file (temp file owned
///   by the exporter; deleted after the command exits).
/// * `{out}` — path to the final output file (source-stem with the
///   configured `extension` appended).
///
/// [`export::spawn_custom_export`]: crate::export::spawn_custom_export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomExportEntry {
    /// Human-readable label — intended to render as `Export <name>` in the
    /// palette once the entries are surfaced there.
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
