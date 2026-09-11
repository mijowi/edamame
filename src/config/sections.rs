use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorConfig {
    /// Wrap code-block lines that exceed the terminal width.  Default: false.
    pub code_block_wrap: bool,
    /// Wrap long document lines at the terminal width.  Default: true.
    pub line_wrap: bool,
    /// Render consecutive blank lines verbatim rather than collapsing them the way CommonMark
    /// does.  Default: true.
    pub preserve_blank_lines: bool,
    /// Up/Down move by **visual** (word-wrapped) lines rather than logical buffer lines.
    pub visual_line_nav: bool,
    /// Terminals the capabilities notice has already fired for.  Built by
    /// [`crate::terminal::Capabilities::fingerprint`]; an unseen fingerprint re-fires the notice.
    pub seen_terminal_fingerprints: Vec<String>,
    /// Show the first-run welcome modal at startup.  Also gates the four legacy startup prompts
    /// (images, remote images, diagrams, capabilities) so the user is never double-prompted.
    pub show_welcome: bool,
    /// Check GitHub for a newer release at startup.  Turning it off suppresses only the
    /// *automatic* check; the explicit entry points always check on request.
    pub check_for_updates: bool,
    /// Unix epoch seconds of the last automatic release check, stamped when the check is
    /// *spawned*, so a hung worker or a killed process can't re-check on every launch.  `0` means
    /// never checked.  Written by edamame, not a knob to hand-edit.
    pub last_update_check: u64,
    /// Release tag the startup notice has already fired for.  Written by edamame.
    pub update_notified_for: String,
    /// The version that last ran, driving the one-time post-upgrade notes (`app::post_upgrade`);
    /// no network involved.  Empty covers both a fresh install and an upgrade from a build
    /// predating the field — `show_welcome` tells them apart, since only a returning user could
    /// have turned it off.  Written by edamame.
    pub last_version_seen: String,
    /// Show line numbers in a left gutter in all three modes.  Default: false.
    pub show_line_numbers: bool,
    /// Lines advanced per mouse-wheel tick.  The keyboard scroll actions always step by one.
    pub mouse_scroll_lines: usize,
    /// How long a non-sticky transient message overlays the hint line.  Errors ignore this and
    /// stay until dismissed.
    pub transient_ms: u64,
    /// Cap the editor content area to `max_width_cols` and center it.  A terminal narrower than
    /// the cap is unaffected, and the bottom status / hint region always spans the full width.
    pub max_width_enabled: bool,
    /// Content width cap in columns; floored at [`MAX_WIDTH_COLS_MIN`] at every use site.
    pub max_width_cols: usize,
    /// Render H1 headings as 4-row big text.  Falls back to the one-line form when the title
    /// would overflow the viewport or contains non-ASCII (font8x8 covers ASCII only).
    pub big_h1: bool,
    /// Syntax-highlight fenced code blocks by the language named in the fence.  There is no
    /// auto-detection: an unlabeled or unknown fence renders as plain code.
    pub syntax_highlighting: bool,
    /// Reflow prose paragraphs: a soft line break inside a paragraph (source hard-wrapping) becomes
    /// a space and the paragraph wraps to the viewport as one flow.  A hard break (trailing two
    /// spaces or a backslash) still forces a row split.  On by default.
    pub reflow: bool,
    /// Autosave after `autosave_idle_ms` of typing inactivity.  Never fires for a buffer with no
    /// file path.
    pub autosave_enabled: bool,
    /// Autosave idle window (ms).  Debounce, not throttle: every keystroke resets the timer, so a
    /// typing burst produces at most one save.
    pub autosave_idle_ms: u64,
    /// Open diff review when an external write is detected while the buffer is **clean**; when
    /// false the buffer is silently reloaded.  A **dirty** buffer always prompts the conflict
    /// modal regardless — unsaved edits are never discarded silently.
    pub diff_on_change: bool,
    /// Show the explanatory modal on entering diff review.
    pub show_diff_intro: bool,
    /// Blink the editor cursor on the `cursor_blink_ms` cadence.
    pub cursor_blink: bool,
    /// Cursor blink half-period (ms); consulted only when `cursor_blink` is true.  File-only — the
    /// settings overlay exposes the toggle but not this value.
    pub cursor_blink_ms: u64,
}

/// Floor applied to `EditorConfig::max_width_cols` so a stray small value can't break layout.
pub const MAX_WIDTH_COLS_MIN: usize = 20;

/// Exclusive lower bound for `EditorConfig::autosave_idle_ms`, so a small or zero value can't
/// autosave on every keystroke.  Enforced at load time with a warning.
pub const AUTOSAVE_IDLE_MS_MIN_EXCLUSIVE: u64 = 1000;
/// Exclusive upper bound (10 minutes); past this the user wants autosave off outright.
pub const AUTOSAVE_IDLE_MS_MAX_EXCLUSIVE: u64 = 600_000;
/// Default debounce window, also the loader's out-of-range fallback.  Kept beside the bounds.
pub const AUTOSAVE_IDLE_MS_DEFAULT: u64 = 5000;

/// User-selected appearance mode.  Independent of `Config::theme` — it filters the picker's list
/// rather than dictating the active theme; the picker keeps the two consistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppearanceMode {
    #[default]
    Dark,
    Light,
}

impl AppearanceMode {
    /// The mode on the other side of the toggle.
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
            max_width_enabled: true,
            max_width_cols: 100,
            big_h1: false,
            syntax_highlighting: true,
            reflow: true,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TableConfig {
    /// Render and hit-test the row/column grip, resize, and delete glyphs.  `App::new` forces
    /// this to `false` on a mouseless terminal so persisted config matches what the user sees.
    pub show_buttons: bool,
    /// Fill alternating data rows with `Theme::table_row_even` / `table_row_odd`.
    pub row_striping: bool,
    /// Warn before the first column-border drag injects a `<!-- tui-columns: [...] -->` comment
    /// into the Markdown source.
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
    /// Prompt the first time a document with remote images is opened.
    #[default]
    Ask,
    Always,
    Never,
}

/// Master switch for inline image rendering; `Never` keeps the `[Image: alt]` placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesEnabled {
    /// Prompt the first time a document with images is opened.
    #[default]
    Ask,
    Always,
    Never,
}

/// Image-rendering configuration.  The two ceilings are in terminal cells and are applied
/// verbatim by `ratatui_image`'s `Resize::Fit` path, so one oversized image can't take the
/// viewport.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImagesConfig {
    pub enabled: ImagesEnabled,
    pub max_width: usize,
    pub max_height: usize,
    pub remote_policy: RemoteImagePolicy,
    /// Directory where pasted screenshots are saved.  Empty — the default
    /// — means the global per-user directory beside edamame's logs; a
    /// relative value resolves against the open document, an absolute
    /// value is used verbatim.  Overridden by the `EDAMAME_IMAGES_DIR`
    /// environment variable when set.
    pub save_dir: String,
}

impl Default for ImagesConfig {
    fn default() -> Self {
        Self {
            enabled: ImagesEnabled::Ask,
            max_width: 100,
            max_height: 24,
            remote_policy: RemoteImagePolicy::Ask,
            save_dir: String::new(),
        }
    }
}

/// Master switch for inline diagram rendering (e.g. mermaid).  `Ask`
/// prompts the user the first time a document with diagrams is opened;
/// `Always` renders without prompting; `Never` keeps the placeholder.
/// Master switch for inline *figure* rendering — mermaid diagrams and `$$...$$` display math,
/// which share this gate.  `Never` keeps the placeholder / source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FiguresEnabled {
    /// Prompt the first time a document with a figure is opened.
    #[default]
    Ask,
    /// Always render figures inline.
    Always,
    /// Never render figures — always fall back to the placeholder / source.
    Never,
}

/// Figure-rendering configuration (mermaid diagrams and display math).  Mirrors
/// [`ImagesConfig::enabled`], kept separate so a user can opt in to images but not figures.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FiguresConfig {
    /// Master switch — see [`FiguresEnabled`].
    pub enabled: FiguresEnabled,
    /// When true (the default), the cursor entering a `$$...$$` math block keeps the rendered
    /// formula in place and opens its editable source just below it, re-rendered on every
    /// keystroke; when false, the reveal shows the source alone, like a mermaid fence.  Math-only:
    /// mermaid's reveal is always source-only, and ordinary images have nothing to preview.
    pub math_preview: bool,
}

impl Default for FiguresConfig {
    fn default() -> Self {
        Self {
            enabled: FiguresEnabled::Ask,
            math_preview: true,
        }
    }
}

/// Export configuration.  HTML is the built-in target and the intermediate format for the
/// user-defined custom commands ([`CustomExportEntry`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportConfig {
    pub html: HtmlExportConfig,
    /// User-defined extra export entries, in `config.toml` order.  One palette entry opens the
    /// export modal, whose Format list offers HTML plus each entry here — there is no
    /// per-converter palette row or [`Action`].
    ///
    /// The modal resolves that list *once, at open time*, cloning each entry rather than storing
    /// an index: returning from the external editor reloads config wholesale, so a captured index
    /// could name a different converter by the time `[ Export ]` is pressed.
    ///
    /// Unrunnable entries are warned about at load and then *ignored* — they stay in this vector
    /// so a later save preserves the user's block.  See [`CustomExportEntry::config_problem`].
    ///
    /// [`Action`]: crate::config::Action
    pub custom: Vec<CustomExportEntry>,
}

/// HTML export settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HtmlExportConfig {
    /// The sentinel `"builtin"` (compiled-in CSS) or a path to a user stylesheet, read at export
    /// time.
    pub stylesheet: String,
    /// Embed local image references as `data:` URIs so the HTML is self-contained.
    pub inline_images: bool,
    /// Render *figures* — fenced ```mermaid code blocks and `$$...$$` display math — to PNG
    /// embedded as `<img>` inside a `<figure>` (`mermaid-diagram` / `math-formula`).  A render
    /// failure falls back to the block's source form so it is never lost; set false to leave every
    /// figure as source (e.g. for pipelines shipping their own mermaid.js or MathJax).
    ///
    /// On disk the key is `figures`; `alias = "diagrams"` keeps configs written before math export
    /// existed loading unchanged, and matches the `[figures]` consent section.
    #[serde(alias = "diagrams")]
    pub figures: bool,
}

impl Default for HtmlExportConfig {
    fn default() -> Self {
        Self {
            stylesheet: "builtin".into(),
            inline_images: false,
            figures: true,
        }
    }
}

/// A single user-configured custom-export entry.  The export modal renders the document to HTML,
/// then runs `command` verbatim with two placeholders substituted:
///
/// * `{html}` — the just-generated HTML (a temp file, deleted after the command exits).
/// * `{out}` — the final output file (source stem plus the configured `extension`).
///
/// **Every field defaults, and the validator is what enforces them.**  A required field would be
/// a hard `toml::de::Error`, and the loader answers a parse failure with `Config::default()` — so
/// one typo here used to silently reset the user's *entire* config for that launch.  Defaulting
/// keeps the blast radius to the entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomExportEntry {
    /// Human-readable label, rendered as `Export <name>`.
    pub name: String,
    /// argv-style command; element 0 is the executable.
    pub command: Vec<String>,
    /// Output-file extension; normalized by [`Self::output_extension`].
    pub extension: String,
}

impl CustomExportEntry {
    /// `extension` trimmed of whitespace and a single leading dot, so `"pdf"`, `" pdf "` and
    /// `".pdf"` agree.  `""` and `"."` yield *no* extension, which is why
    /// [`Self::config_problem`] rejects them — the export would otherwise overwrite the document.
    pub fn output_extension(&self) -> &str {
        let trimmed = self.extension.trim();
        trimmed.strip_prefix('.').unwrap_or(trimmed)
    }

    /// Why this entry cannot produce a working export, or `None` if it is runnable.  One
    /// predicate behind two decisions — the startup warning and the modal's Format list — so a
    /// warned-about entry can never also be offered as a row that fails after the user fills in
    /// the form.  The rules cover exactly what the runner cannot recover from.
    pub fn config_problem(&self) -> Option<&'static str> {
        if self.name.trim().is_empty() {
            Some("`name` is empty; it is what labels the command-palette entry")
        } else if self.command.is_empty() {
            Some("`command` is empty; it needs at least the program to run")
        } else if self.output_extension().is_empty() {
            Some("`extension` is empty; the export would replace the document's own name")
        } else if self.output_extension().contains(['/', '\\']) {
            Some("`extension` contains a path separator; it must name a suffix, not a location")
        } else {
            None
        }
    }
}

/// Developer/diagnostic settings, kept out of `[editor]`: logging and debug tooling, not editing
/// behavior.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DevConfig {
    /// Write `tracing` logs to the XDG data dir.  Off by default so the TUI stays silent.
    pub logging: bool,
}
