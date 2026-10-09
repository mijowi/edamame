//! Static row table for the settings overlay.  Each row carries a label, an optional description,
//! and a [`RowKind`] whose function pointers tell the overlay how to read, write, and cycle the
//! underlying config field.  Adding a setting should only touch this file.

use crate::config::sections::{DEFAULT_HANDLER, MAX_WIDTH_COLS_MIN, VIM_HANDLER};
use crate::config::{Config, FiguresEnabled, ImagesEnabled, RemoteImagePolicy};
use crate::ui::controls;

/// Labels referenced from outside this module are constants so the App-level live-update wiring in
/// `app/modal/settings.rs` can't drift from the row table on a copy change.
///
/// This one is the non-focusable header note, matched by string equality in `build_row_lines` so it
/// renders without the usual label/value formatting.
pub(crate) const HEADER_NOTE: &str = "Common options shown below — all others in config.toml";

pub(crate) const LABEL_BIG_H1: &str = "Big H1 headings";
pub(crate) const LABEL_REFLOW: &str = "Reflow paragraphs";
pub(crate) const LABEL_SYNTAX_HIGHLIGHTING: &str = "Syntax highlighting";
pub(crate) const LABEL_VISUAL_LINE_NAV: &str = "Use visual line navigation";
pub(crate) const LABEL_LINE_NUMBERS: &str = "Show line numbers";
pub(crate) const LABEL_OUTLINE: &str = "Show outline";
pub(crate) const LABEL_SCROLL_SPEED: &str = "Scroll speed";
pub(crate) const LABEL_VIM_MODE: &str = "Vim mode";
pub(crate) const LABEL_BLINK_CURSOR: &str = "Blink cursor";
pub(crate) const LABEL_SHOW_IMAGES: &str = "Show images";
pub(crate) const LABEL_SHOW_DIAGRAMS: &str = "Show figures";
pub(crate) const LABEL_MATH_PREVIEW: &str = "  Math edit preview";
pub(crate) const LABEL_SHOW_REMOTE_IMAGES: &str = "  Show remote images";
pub(crate) const LABEL_AUTOSAVE: &str = "Autosave";
pub(crate) const LABEL_LIMIT_WIDTH: &str = "Limit editor width";
pub(crate) const LABEL_DIFF_ON_CHANGE: &str = "Diff when file changes";
pub(crate) const LABEL_TABLE_BUTTONS: &str = "Show table buttons";
pub(crate) const LABEL_CHECK_UPDATES: &str = "Check for updates";
pub(crate) const LABEL_DAILY_TIPS: &str = "Daily tips";

/// Minimum accepted value for [`LABEL_SCROLL_SPEED`].  Rejecting at the input boundary (rather
/// than relying on the dispatcher's clamp) keeps the persisted value and the live wheel step equal.
const MOUSE_SCROLL_LINES_MIN: usize = 1;

#[derive(Clone, Copy, Debug)]
pub(super) enum RowAction {
    /// "Open config.toml in editor" sentinel.
    OpenExternalEditor,
    /// "Open config folder" sentinel — hands the path to the OS file manager.
    OpenConfigFolder,
    /// Enter cycles the value (boolean toggle / enum advance).
    Cycle,
    /// Enter opens an inline text editor (numeric field).
    Edit,
}

/// Read an option row's current value as a normalized [`controls::ControlValue`].  Aliased to keep
/// the `Option<…>` fields below under clippy's type-complexity threshold.
pub(super) type ReadValueFn = fn(&Config) -> controls::ControlValue;
/// Write back the value [`controls::Control::apply`] produced for an option row.
pub(super) type WriteValueFn = fn(&mut Config, controls::ControlValue);

pub(super) struct RowKind {
    pub(super) focusable: bool,
    pub(super) action: RowAction,
    pub(super) read: fn(&Config, &[String]) -> String,
    pub(super) write_string: fn(&mut Config, &str) -> Result<(), String>,
    /// Read / write the row's value as a [`controls::ControlValue`].  `Some` on option rows
    /// (toggle / pill), `None` on numeric, button, and display-only rows.  The input path routes
    /// the value through [`controls::Control::apply`], so the cycle math lives in `controls`.
    pub(super) read_value: Option<ReadValueFn>,
    pub(super) write_value: Option<WriteValueFn>,
    /// Control spec for option-style rows: booleans use [`controls::Control::Toggle`], tri-states
    /// [`controls::Control::Pill`].  `None` renders the row as a single-value display.
    pub(super) options: Option<controls::Control>,
    /// When it returns true, the row renders inert and focus navigation skips it.  Used by the
    /// remote-images row, which the images-`Never` cascade locks (mirroring `ui::welcome`).
    pub(super) disabled: Option<fn(&Config) -> bool>,
}

/// Display string for a boolean setting; the [`controls::Control::Toggle`] slider reads `"on"`.
fn bool_label(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}

/// One settings row.  `read` formats the current value for display; `write_string` handles the
/// inline-editor confirm path.
pub(super) struct RowDef {
    pub(super) label: &'static str,
    pub(super) description: Option<&'static str>,
    /// Dynamic description, formatted from live config and taking precedence over `description` —
    /// used to embed a file-only numeric value (e.g. the blink cadence) in the footer copy.
    pub(super) describe: Option<fn(&Config) -> String>,
    pub(super) kind: RowKind,
}

impl RowDef {
    /// Footer description for `config`: dynamic when present, else the static string.
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

    /// Whether focus may land on this row right now.
    pub(super) fn focus_eligible(&self, config: &Config) -> bool {
        self.kind.focusable && !self.is_disabled(config)
    }
}

fn no_write(_: &mut Config, _: &str) -> Result<(), String> {
    Err("row is not editable in place".to_owned())
}

/// A non-focusable display-only row: the header note and blank dividers.
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
            read_value: None,
            write_value: None,
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

const IMAGES_ENABLED_ORDER: &[ImagesEnabled] = &[
    ImagesEnabled::Ask,
    ImagesEnabled::Always,
    ImagesEnabled::Never,
];

const FIGURES_ENABLED_ORDER: &[FiguresEnabled] = &[
    FiguresEnabled::Ask,
    FiguresEnabled::Always,
    FiguresEnabled::Never,
];

const REMOTE_POLICY_ORDER: &[RemoteImagePolicy] = &[
    RemoteImagePolicy::Ask,
    RemoteImagePolicy::Always,
    RemoteImagePolicy::Never,
];

/// Index of `value` in its ordered enum table — the [`controls::ControlValue::Choice`] index.
fn order_index<T: PartialEq>(order: &[T], value: T) -> usize {
    order.iter().position(|v| *v == value).unwrap_or(0)
}

/// Inverse of [`order_index`], clamped to the last entry for an out-of-range index.
fn order_value<T: Copy>(order: &[T], i: usize) -> T {
    order[i.min(order.len().saturating_sub(1))]
}

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

fn diagrams_enabled_label(v: FiguresEnabled) -> &'static str {
    match v {
        FiguresEnabled::Ask => "Ask",
        FiguresEnabled::Always => "Always",
        FiguresEnabled::Never => "Never",
    }
}

fn parse_diagrams_enabled(s: &str) -> Result<FiguresEnabled, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "ask" => Ok(FiguresEnabled::Ask),
        "always" => Ok(FiguresEnabled::Always),
        "never" => Ok(FiguresEnabled::Never),
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

/// Build the static row table.  Order is the user-facing display order; nothing else depends on it.
pub(super) fn build_rows() -> Vec<RowDef> {
    vec![
        display_only_row(HEADER_NOTE),
        display_only_row(""),
        RowDef {
            label: "Open config folder",
            description: Some("\nPress Enter to open in file manager"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::OpenConfigFolder,
                // The path can be long, so the `[ Open ]` button is the only affordance.
                read: |_, _| String::new(),
                write_string: no_write,
                read_value: None,
                write_value: None,
                options: Some(controls::Control::Button("Open")),
                disabled: None,
            },
        },
        RowDef {
            label: "Open config.toml",
            description: Some("\nPress Enter to open in default editor"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::OpenExternalEditor,
                read: |_, _| String::new(),
                write_string: no_write,
                read_value: None,
                write_value: None,
                options: Some(controls::Control::Button("Open")),
                disabled: None,
            },
        },
        // Blank divider setting the "open externally" pair apart from the editable settings.
        display_only_row(""),
        // ── Editable settings, alphabetical by label, except `Show line numbers`, which sits
        //    below the image-visibility rows so the image group stays contiguous ──
        RowDef {
            label: LABEL_AUTOSAVE,
            description: Some("\nAutomatically save changes when idle"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.editor.autosave_enabled).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.editor.autosave_enabled)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.editor.autosave_enabled = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_BIG_H1,
            description: Some("\nRender H1 titles as large block-character text"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.editor.big_h1).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.editor.big_h1)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.editor.big_h1 = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_REFLOW,
            description: Some("\nReflow soft-wrapped prose paragraphs to the viewport width"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.editor.reflow).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.editor.reflow)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.editor.reflow = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_BLINK_CURSOR,
            // Static fallback; `describe` embeds the file-only cadence.
            description: Some("\nBlink the editor cursor"),
            describe: Some(|c| format!("\nBlink cursor every {} ms", c.editor.cursor_blink_ms)),
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.editor.cursor_blink).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.editor.cursor_blink)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.editor.cursor_blink = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_CHECK_UPDATES,
            description: Some("\nCheck GitHub for a new release at startup, once a day"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.editor.check_for_updates).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.editor.check_for_updates)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.editor.check_for_updates = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_DAILY_TIPS,
            description: Some("\nShow an occasional startup tip about a feature, once a day"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.editor.daily_tips).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.editor.daily_tips)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.editor.daily_tips = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_LIMIT_WIDTH,
            description: Some("\nLimit the editor content width"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.editor.max_width_enabled).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.editor.max_width_enabled)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.editor.max_width_enabled = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
        RowDef {
            label: "  Char limit",
            description: Some("\nMaximum content width when limit is on"),
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
                read_value: None,
                write_value: None,
                options: None,
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_SCROLL_SPEED,
            description: Some("\nLines per mouse-wheel tick"),
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
                read_value: None,
                write_value: None,
                options: None,
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_DIFF_ON_CHANGE,
            // Two-line footer: the on/off meanings differ enough to spell out separately.
            description: Some(
                "On: Review external changes hunk by hunk\n\
                 Off: Silently reload a clean buffer",
            ),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.editor.diff_on_change).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.editor.diff_on_change)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.editor.diff_on_change = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_SHOW_DIAGRAMS,
            description: Some("\nRender mermaid diagrams and $$…$$ math inline"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| diagrams_enabled_label(c.figures.enabled).to_owned(),
                write_string: |c, v| {
                    c.figures.enabled = parse_diagrams_enabled(v)?;
                    Ok(())
                },
                read_value: Some(|c| {
                    controls::ControlValue::Choice(order_index(
                        FIGURES_ENABLED_ORDER,
                        c.figures.enabled,
                    ))
                }),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Choice(i) = v {
                        c.figures.enabled = order_value(FIGURES_ENABLED_ORDER, i);
                    }
                }),
                options: Some(controls::Control::Pill(controls::ASK_ALWAYS_NEVER)),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_MATH_PREVIEW,
            description: Some("\nEditing a $$…$$ block opens its source below the formula"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.figures.math_preview).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.figures.math_preview)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.figures.math_preview = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                // Inert when figures are off: with `[figures].enabled =
                // "never"` no `$$...$$` block is promoted, so there is no
                // reveal for the preview to affect.  Mirrors the
                // remote-images row locking to images-`Never`.
                disabled: Some(|c| matches!(c.figures.enabled, FiguresEnabled::Never)),
            },
        },
        RowDef {
            label: LABEL_SHOW_IMAGES,
            description: Some("\nShow images in preview and render mode"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| images_enabled_label(c.images.enabled).to_owned(),
                write_string: |c, v| {
                    c.images.enabled = parse_images_enabled(v)?;
                    Ok(())
                },
                read_value: Some(|c| {
                    controls::ControlValue::Choice(order_index(
                        IMAGES_ENABLED_ORDER,
                        c.images.enabled,
                    ))
                }),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Choice(i) = v {
                        c.images.enabled = order_value(IMAGES_ENABLED_ORDER, i);
                    }
                }),
                options: Some(controls::Control::Pill(controls::ASK_ALWAYS_NEVER)),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_SHOW_REMOTE_IMAGES,
            description: Some("\nFetch images from http(s):// URLs"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| remote_policy_label(c.images.remote_policy).to_owned(),
                write_string: |c, v| {
                    c.images.remote_policy = parse_remote_policy(v)?;
                    Ok(())
                },
                read_value: Some(|c| {
                    controls::ControlValue::Choice(order_index(
                        REMOTE_POLICY_ORDER,
                        c.images.remote_policy,
                    ))
                }),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Choice(i) = v {
                        c.images.remote_policy = order_value(REMOTE_POLICY_ORDER, i);
                    }
                }),
                options: Some(controls::Control::Pill(controls::ASK_ALWAYS_NEVER)),
                // Locked while images are off — mirrors the welcome modal's cascade.
                disabled: Some(|c| matches!(c.images.enabled, ImagesEnabled::Never)),
            },
        },
        RowDef {
            label: LABEL_LINE_NUMBERS,
            description: Some("\nShow line numbers in the left gutter"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.editor.show_line_numbers).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.editor.show_line_numbers)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.editor.show_line_numbers = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_OUTLINE,
            description: Some(
                "\nShow a heading outline beside the document (hidden on narrow screens)",
            ),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.editor.show_outline).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.editor.show_outline)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.editor.show_outline = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_TABLE_BUTTONS,
            description: Some("\nShow table row/column move/resize glyphs"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.table.show_buttons).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.table.show_buttons)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.table.show_buttons = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_SYNTAX_HIGHLIGHTING,
            description: Some("\nColor code blocks using the language on the fence"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.editor.syntax_highlighting).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.editor.syntax_highlighting)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.editor.syntax_highlighting = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_VISUAL_LINE_NAV,
            description: Some("\nUp/Down move by visual lines (vs. logical)"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                read: |c, _| bool_label(c.editor.visual_line_nav).to_owned(),
                write_string: no_write,
                read_value: Some(|c| controls::ControlValue::Toggle(c.editor.visual_line_nav)),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.editor.visual_line_nav = b;
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
        RowDef {
            label: LABEL_VIM_MODE,
            description: Some("\nUse Vim-style modal editing"),
            describe: None,
            kind: RowKind {
                focusable: true,
                action: RowAction::Cycle,
                // Vim mode is stored as the modal handler name, not a bool, so these translate
                // between the pills and `config.modal.handler`.
                read: |c, _| bool_label(c.modal.handler == VIM_HANDLER).to_owned(),
                write_string: no_write,
                read_value: Some(|c| {
                    controls::ControlValue::Toggle(c.modal.handler == VIM_HANDLER)
                }),
                write_value: Some(|c, v| {
                    if let controls::ControlValue::Toggle(b) = v {
                        c.modal.handler = if b { VIM_HANDLER } else { DEFAULT_HANDLER }.to_owned();
                    }
                }),
                options: Some(controls::Control::Toggle),
                disabled: None,
            },
        },
    ]
}
