//! Static row table for the settings overlay.
//!
//! Each row carries a label, an optional description, and a [`RowKind`]
//! whose function-pointer fields tell the overlay how to read, write, and
//! cycle the underlying config field.  Pulling the table out of
//! `settings_overlay.rs` keeps the parent file focused on widget
//! plumbing — adding a new setting only touches this file.

use crate::config::sections::MAX_WIDTH_COLS_MIN;
use crate::config::{Config, DiagramsEnabled, ImagesEnabled, RemoteImagePolicy};

/// Row labels for the settings overlay, exported as constants so the
/// App-level live-update wiring in `app/modal/settings.rs` and the
/// row table here can't drift out of sync on a copy change.  Only
/// labels that are referenced from outside this module need a
/// constant; the rest stay as inline string literals.
///
/// Explanatory header rendered as a styled note above the rows.
/// Non-focusable; identified in `build_row_lines` by string equality
/// against this constant so it can be rendered without the usual
/// label/value formatting.
pub(crate) const HEADER_NOTE: &str = "Common options shown below — edit config.toml for all others";

pub(crate) const LABEL_BIG_H1: &str = "Big H1 headings";
pub(crate) const LABEL_VISUAL_LINE_NAV: &str = "Use visual line navigation";
pub(crate) const LABEL_LINE_NUMBERS: &str = "Show line numbers";
pub(crate) const LABEL_SCROLL_SPEED: &str = "Scroll speed";
pub(crate) const LABEL_VIM_MODE: &str = "Vim mode";
pub(crate) const LABEL_BLINK_CURSOR: &str = "Blink cursor";
pub(crate) const LABEL_SHOW_IMAGES: &str = "Show images";
pub(crate) const LABEL_SHOW_REMOTE_IMAGES: &str = "Show remote images";

/// Handler name written to `config.modal.handler` when vim mode is on.
const VIM_HANDLER: &str = "vim";
/// Handler name written to `config.modal.handler` when vim mode is off.
const DEFAULT_HANDLER: &str = "default";

/// Minimum accepted value for [`LABEL_SCROLL_SPEED`].  The
/// dispatcher additionally clamps zero to one as a safety net, but
/// rejecting at the input boundary keeps the persisted value and
/// the live wheel_step in agreement.
const MOUSE_SCROLL_LINES_MIN: usize = 1;

#[derive(Debug)]
pub(super) enum RowAction {
    /// "Open config.toml in default editor" sentinel.
    OpenExternalEditor,
    /// "Open Config folder" sentinel — fires the `OpenConfigFolder`
    /// action via the OS file manager (`xdg-open` on Linux,
    /// `open` on macOS, `explorer` on Windows).
    OpenConfigFolder,
    /// Enter cycles the value (boolean toggle / enum advance).
    Cycle,
    /// Enter opens an inline text editor (numeric field).
    Edit,
}

/// `(config, delta, theme_names) -> changed?`.  Aliased so the
/// `Option<…>` field below stays under clippy's complexity threshold.
pub(super) type CycleFn = fn(&mut Config, i32, &[String]) -> bool;

pub(super) struct RowKind {
    pub(super) focusable: bool,
    pub(super) action: RowAction,
    pub(super) read: fn(&Config, &[String]) -> String,
    pub(super) write_string: fn(&mut Config, &str) -> Result<(), String>,
    pub(super) cycle: Option<CycleFn>,
    /// Pill labels for option-style rows (booleans + Ask/Always/Never
    /// tri-states).  When `Some`, the overlay renders every option
    /// inline and styles the one matching `read(..)` with the
    /// persistent-selection palette (`modal_item_selected_unfocused`,
    /// upgraded to `modal_button_focused` when the row has focus).
    /// When `None`, the row falls back to the legacy single-value
    /// display (numeric / path / external-action rows).
    pub(super) options: Option<&'static [&'static str]>,
    /// When `Some` and the fn returns true for the current config, the
    /// row is rendered inert (dimmed label + pills) and skipped by focus
    /// navigation / cycling.  Used by the remote-images row, which the
    /// images-`Never` cascade locks to `Never` (mirrors the welcome
    /// modal's cascade — see `ui::welcome`).
    pub(super) disabled: Option<fn(&Config) -> bool>,
}

pub(super) const BOOL_OPTIONS: &[&str] = &["true", "false"];
pub(super) const ASK_ALWAYS_NEVER_OPTIONS: &[&str] = &["Ask", "Always", "Never"];

/// Static table of rows.  `read` formats the field's current value
/// for display; `cycle` is `Some` for fields whose value cycles on
/// Left/Right or Enter (booleans, enum-valued fields, theme name);
/// `write_string` handles the inline-editor confirm path.
pub(super) struct RowDef {
    pub(super) label: &'static str,
    pub(super) description: Option<&'static str>,
    /// Dynamic description override.  When `Some`, it is formatted from
    /// live config and takes precedence over `description` — used to
    /// embed a file-only numeric value (e.g. the blink cadence) in the
    /// pinned footer copy.  Most rows leave this `None`.
    pub(super) describe: Option<fn(&Config) -> String>,
    pub(super) kind: RowKind,
}

impl RowDef {
    /// Resolve the row's footer description for the given config —
    /// dynamic (`describe`) when present, else the static string.
    pub(super) fn resolved_description(&self, config: &Config) -> Option<String> {
        match self.describe {
            Some(f) => Some(f(config)),
            None => self.description.map(|s| s.to_owned()),
        }
    }

    /// Whether this row is currently inert (cascade- or capability-locked).
    pub(super) fn is_disabled(&self, config: &Config) -> bool {
        self.kind.disabled.map(|f| f(config)).unwrap_or(false)
    }

    /// Whether focus may land on this row right now: focusable in
    /// principle and not currently disabled.
    pub(super) fn focus_eligible(&self, config: &Config) -> bool {
        self.kind.focusable && !self.is_disabled(config)
    }
}

fn no_write(_: &mut Config, _: &str) -> Result<(), String> {
    Err("row is not editable in place".to_owned())
}

/// Build a non-focusable display-only row.  Used for the explanatory
/// header note and blank dividers — anything that participates in the
/// row list for layout but never takes focus or fires an action.
fn display_only_row(label: &'static str) -> RowDef {
    RowDef {
        label,
        description: None,
        describe: None,
        kind: RowKind {
            focusable: false,
            action: RowAction::Cycle,
            read: |_, _| String::new(),
            write_string: no_write,
            cycle: None,
            options: None,
            disabled: None,
        },
    }
}

fn parse_usize(s: &str) -> Result<usize, String> {
    s.trim()
        .parse::<usize>()
        .map_err(|e| format!("invalid number: {e}"))
}

/// Cycle through `order` by `delta` (signed step), wrapping at both
/// ends.  Returns the value at `current`'s index plus delta, modulo
/// the order length.  Falls back to the first element when `current`
/// isn't in the order.  Empty `order` is a programmer error and
/// returns `current` unchanged.
fn cycle_enum<T: PartialEq + Copy>(current: T, order: &[T], delta: i32) -> T {
    if order.is_empty() {
        return current;
    }
    let i = order.iter().position(|v| *v == current).unwrap_or(0) as i32;
    let n = order.len() as i32;
    order[((i + delta).rem_euclid(n)) as usize]
}

const IMAGES_ENABLED_ORDER: &[ImagesEnabled] = &[
    ImagesEnabled::Ask,
    ImagesEnabled::Always,
    ImagesEnabled::Never,
];

const DIAGRAMS_ENABLED_ORDER: &[DiagramsEnabled] = &[
    DiagramsEnabled::Ask,
    DiagramsEnabled::Always,
    DiagramsEnabled::Never,
];

const REMOTE_POLICY_ORDER: &[RemoteImagePolicy] = &[
    RemoteImagePolicy::Ask,
    RemoteImagePolicy::Always,
    RemoteImagePolicy::Never,
];

fn images_enabled_label(v: ImagesEnabled) -> &'static str {
    match v {
        ImagesEnabled::Ask => "Ask",
        ImagesEnabled::Always => "Always",
        ImagesEnabled::Never => "Never",
    }
}

fn remote_policy_label(v: RemoteImagePolicy) -> &'static str {
    match v {
        RemoteImagePolicy::Ask => "Ask",
        RemoteImagePolicy::Always => "Always",
        RemoteImagePolicy::Never => "Never",
    }
}

fn parse_images_enabled(s: &str) -> Result<ImagesEnabled, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "ask" => Ok(ImagesEnabled::Ask),
        "always" => Ok(ImagesEnabled::Always),
        "never" => Ok(ImagesEnabled::Never),
        other => Err(format!("expected Ask/Always/Never, got {other:?}")),
    }
}

fn diagrams_enabled_label(v: DiagramsEnabled) -> &'static str {
    match v {
        DiagramsEnabled::Ask => "Ask",
        DiagramsEnabled::Always => "Always",
        DiagramsEnabled::Never => "Never",
    }
}

fn parse_diagrams_enabled(s: &str) -> Result<DiagramsEnabled, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "ask" => Ok(DiagramsEnabled::Ask),
        "always" => Ok(DiagramsEnabled::Always),
        "never" => Ok(DiagramsEnabled::Never),
        other => Err(format!("expected Ask/Always/Never, got {other:?}")),
    }
}

fn parse_remote_policy(s: &str) -> Result<RemoteImagePolicy, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "ask" => Ok(RemoteImagePolicy::Ask),
        "always" => Ok(RemoteImagePolicy::Always),
        "never" => Ok(RemoteImagePolicy::Never),
        other => Err(format!("expected Ask/Always/Never, got {other:?}")),
    }
}

/// Build the static row table.  Order is the user-facing display
/// order; nothing else depends on it.  See [`crate::config::Config`]
/// for each field's persistence semantics.
pub(super) fn build_rows() -> Vec<RowDef> {
    vec![
        display_only_row(HEADER_NOTE),
        display_only_row(""),
        RowDef {
            label: "Open config folder",
            description: Some("Press Enter to open externally"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::OpenConfigFolder,
                read: |_, _| {
                    Config::config_dir()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                },
                write_string: no_write,
                cycle: None,
                options: None,
                disabled: None,
            },
        },
        RowDef {
            label: "Open config.toml in default editor",
            description: Some("Press Enter to open externally"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::OpenExternalEditor,
                read: |_, _| String::new(),
                write_string: no_write,
                cycle: None,
                options: None,
                disabled: None,
            },
        },
        // Blank divider — sets the "open externally" pair apart
        // from the editable settings beneath.  Non-focusable so
        // arrow-key navigation skips it; the View renders an empty
        // line for any non-focusable row with an empty label.
        display_only_row(""),
        // ── Editable settings, alphabetical by label, except
        //    `Show line numbers` sits below the two image-visibility
        //    rows so the image group stays contiguous ──
        RowDef {
            label: "Autosave",
            description: Some("Automatically save changes when idle"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| c.editor.autosave_enabled.to_string(),
                write_string: no_write,
                cycle: Some(|c, _, _| {
                    c.editor.autosave_enabled = !c.editor.autosave_enabled;
                    true
                }),
                options: Some(BOOL_OPTIONS),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_BIG_H1,
            description: Some("Render H1 titles as large block-character text"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| c.editor.big_h1.to_string(),
                write_string: no_write,
                cycle: Some(|c, _, _| {
                    c.editor.big_h1 = !c.editor.big_h1;
                    true
                }),
                options: Some(BOOL_OPTIONS),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_BLINK_CURSOR,
            // Static fallback; `describe` embeds the file-only cadence.
            description: Some("Blink the editor cursor"),
            describe: Some(|c| format!("Blink cursor every {} ms", c.editor.cursor_blink_ms)),
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| c.editor.cursor_blink.to_string(),
                write_string: no_write,
                cycle: Some(|c, _, _| {
                    c.editor.cursor_blink = !c.editor.cursor_blink;
                    true
                }),
                options: Some(BOOL_OPTIONS),
                disabled: None,
            },
        },
        RowDef {
            label: "Editor max width",
            description: Some("Maximum content width in characters when limit is on"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Edit,
                read: |c, _| c.editor.max_width_cols.to_string(),
                write_string: |c, v| {
                    let n = parse_usize(v)?;
                    if n < MAX_WIDTH_COLS_MIN {
                        return Err(format!("must be at least {MAX_WIDTH_COLS_MIN}"));
                    }
                    c.editor.max_width_cols = n;
                    Ok(())
                },
                cycle: None,
                options: None,
                disabled: None,
            },
        },
        RowDef {
            label: "Limit editor width",
            description: Some("Cap the editor content to a fixed width"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| c.editor.max_width_enabled.to_string(),
                write_string: no_write,
                cycle: Some(|c, _, _| {
                    c.editor.max_width_enabled = !c.editor.max_width_enabled;
                    true
                }),
                options: Some(BOOL_OPTIONS),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_SCROLL_SPEED,
            description: Some("Lines per mouse-wheel tick (also applies to touchpads)"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Edit,
                read: |c, _| c.editor.mouse_scroll_lines.to_string(),
                write_string: |c, v| {
                    let n = parse_usize(v)?;
                    if n < MOUSE_SCROLL_LINES_MIN {
                        return Err(format!("must be at least {MOUSE_SCROLL_LINES_MIN}"));
                    }
                    c.editor.mouse_scroll_lines = n;
                    Ok(())
                },
                cycle: None,
                options: None,
                disabled: None,
            },
        },
        RowDef {
            label: "Show diagrams",
            description: Some("Render Mermaid code blocks as inline diagrams"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| diagrams_enabled_label(c.diagrams.enabled).to_owned(),
                write_string: |c, v| {
                    c.diagrams.enabled = parse_diagrams_enabled(v)?;
                    Ok(())
                },
                cycle: Some(|c, delta, _| {
                    c.diagrams.enabled =
                        cycle_enum(c.diagrams.enabled, DIAGRAMS_ENABLED_ORDER, delta);
                    true
                }),
                options: Some(ASK_ALWAYS_NEVER_OPTIONS),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_SHOW_IMAGES,
            description: Some("Show images in preview and render mode"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| images_enabled_label(c.images.enabled).to_owned(),
                write_string: |c, v| {
                    c.images.enabled = parse_images_enabled(v)?;
                    Ok(())
                },
                cycle: Some(|c, delta, _| {
                    c.images.enabled = cycle_enum(c.images.enabled, IMAGES_ENABLED_ORDER, delta);
                    true
                }),
                options: Some(ASK_ALWAYS_NEVER_OPTIONS),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_SHOW_REMOTE_IMAGES,
            description: Some("Fetch and display remote images"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| remote_policy_label(c.images.remote_policy).to_owned(),
                write_string: |c, v| {
                    c.images.remote_policy = parse_remote_policy(v)?;
                    Ok(())
                },
                cycle: Some(|c, delta, _| {
                    c.images.remote_policy =
                        cycle_enum(c.images.remote_policy, REMOTE_POLICY_ORDER, delta);
                    true
                }),
                options: Some(ASK_ALWAYS_NEVER_OPTIONS),
                // Locked to Never (and skipped by focus) while images are
                // off — mirrors the welcome modal's images→remote cascade.
                disabled: Some(|c| matches!(c.images.enabled, ImagesEnabled::Never)),
            },
        },
        RowDef {
            label: LABEL_LINE_NUMBERS,
            description: Some("Show line numbers in the left gutter"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| c.editor.show_line_numbers.to_string(),
                write_string: no_write,
                cycle: Some(|c, _, _| {
                    c.editor.show_line_numbers = !c.editor.show_line_numbers;
                    true
                }),
                options: Some(BOOL_OPTIONS),
                disabled: None,
            },
        },
        RowDef {
            label: "Show table buttons",
            description: Some("Show table row/column move/resize glyphs"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| c.table.show_buttons.to_string(),
                write_string: no_write,
                cycle: Some(|c, _, _| {
                    c.table.show_buttons = !c.table.show_buttons;
                    true
                }),
                options: Some(BOOL_OPTIONS),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_VISUAL_LINE_NAV,
            description: Some("Up/Down move by visual lines (vs. logical)"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| c.editor.visual_line_nav.to_string(),
                write_string: no_write,
                cycle: Some(|c, _, _| {
                    c.editor.visual_line_nav = !c.editor.visual_line_nav;
                    true
                }),
                options: Some(BOOL_OPTIONS),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_VIM_MODE,
            description: Some("Vim-style modal editing (Normal / Insert / Visual)"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                // Vim mode is stored as the modal handler name, not a
                // bool — `read`/`cycle` translate between the
                // `true`/`false` pills and `config.modal.handler`.  The
                // App-level live-update arm rebuilds the VimState so the
                // toggle takes effect without a restart.
                read: |c, _| (c.modal.handler == VIM_HANDLER).to_string(),
                write_string: no_write,
                cycle: Some(|c, _, _| {
                    c.modal.handler = if c.modal.handler == VIM_HANDLER {
                        DEFAULT_HANDLER.to_owned()
                    } else {
                        VIM_HANDLER.to_owned()
                    };
                    true
                }),
                options: Some(BOOL_OPTIONS),
                disabled: None,
            },
        },
    ]
}
