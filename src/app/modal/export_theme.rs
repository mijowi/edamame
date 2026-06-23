//! "Create custom theme" modal: pick an existing theme, name a new
//! one, write `<config_dir>/themes/<name>.toml`, then apply it.
//!
//! On success we push [`super::ExportSuccessModal`] on top of the
//! stack so the user can immediately open the new file or its
//! containing folder.

use std::any::Any;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use super::ExportSuccessModal;
use crate::app::App;
use crate::config::theme::list_theme_names;
use crate::config::{Config, Theme, ThemeFile};
use crate::ui::{ExportThemeResponse, ExportThemeState, ExportThemeView, ModalKind};

pub struct ExportThemeModal {
    state: ExportThemeState,
}

impl ExportThemeModal {
    pub fn new(themes: Vec<String>, active: &str) -> Self {
        Self {
            state: ExportThemeState::new(themes, active),
        }
    }
}

impl Modal for ExportThemeModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = ExportThemeView {
            theme: ctx.theme,
            cursor_visible: ctx.cursor_visible,
        };
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        let existing = self.state.themes.clone();
        match self.state.handle_key(&key, &existing) {
            ExportThemeResponse::Continue => ModalOutcome::Continue,
            ExportThemeResponse::Cancelled => ModalOutcome::Close,
            ExportThemeResponse::Export { source, new_name } => {
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    perform_export(app, source, new_name);
                }))
            }
        }
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        self.state.paste(text);
        ModalOutcome::Continue
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_state.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16) -> ModalOutcome {
        super::types::close_if_esc_clicked(self.state.esc_button_rect, col, row)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Resolve `source` into a serialisable `ThemeFile`.  Built-ins
/// resolve via the in-memory registry; user themes load through the
/// same `Config::load_theme` path used elsewhere.
fn resolve_source_theme(source: &str, truecolor: bool) -> ThemeFile {
    if let Some(theme) = Theme::builtin(source) {
        (&theme).into()
    } else {
        let (file, _warnings) = Config::load_theme(source, truecolor);
        file
    }
}

/// Errors produced by [`write_exported_theme`].  Carries a
/// user-presentable message so the UI layer can flash it verbatim.
#[derive(Debug)]
pub(crate) enum ExportThemeError {
    AlreadyExists(PathBuf),
    CreateDir(io::Error),
    Serialize(toml::ser::Error),
    Write { path: PathBuf, err: io::Error },
}

impl std::fmt::Display for ExportThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists(p) => {
                write!(f, "A theme file already exists at {}", p.display())
            }
            Self::CreateDir(e) => write!(f, "Failed to create themes dir: {e}"),
            Self::Serialize(e) => write!(f, "Failed to serialize theme: {e}"),
            Self::Write { path, err } => {
                write!(f, "Failed to write {}: {}", path.display(), err)
            }
        }
    }
}

/// Atomic-create write of `theme_file` to
/// `<themes_dir>/<new_name>.toml`.  Uses `O_CREAT | O_EXCL` so a
/// concurrent existing file is rejected in one syscall without a
/// separate `exists()` race.  Pure I/O — no `App` dependency, so this
/// is the unit-testable entry point.
pub(crate) fn write_exported_theme(
    themes_dir: &Path,
    new_name: &str,
    theme_file: &ThemeFile,
) -> Result<PathBuf, ExportThemeError> {
    std::fs::create_dir_all(themes_dir).map_err(ExportThemeError::CreateDir)?;
    let path = themes_dir.join(format!("{new_name}.toml"));
    let serialized = toml::to_string_pretty(theme_file).map_err(ExportThemeError::Serialize)?;
    let mut file = match OpenOptions::new().create_new(true).write(true).open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(ExportThemeError::AlreadyExists(path));
        }
        Err(e) => {
            return Err(ExportThemeError::Write { path, err: e });
        }
    };
    file.write_all(serialized.as_bytes())
        .map_err(|err| ExportThemeError::Write {
            path: path.clone(),
            err,
        })?;
    Ok(path)
}

/// Serialise the chosen source theme to TOML, write it to
/// `<config_dir>/themes/<new_name>.toml`, persist
/// `config.theme = <new_name>`, reapply, and open the success modal.
fn perform_export(app: &mut App, source: String, new_name: String) {
    let truecolor = app.capabilities.color_depth == crate::terminal::ColorDepth::TrueColor;
    let theme_file = resolve_source_theme(&source, truecolor);

    let Some(config_dir) = Config::config_dir() else {
        app.notify(
            "No config directory available; cannot export theme",
            ModalKind::Error,
        );
        return;
    };
    let themes_dir = config_dir.join("themes");
    let path = match write_exported_theme(&themes_dir, &new_name, &theme_file) {
        Ok(p) => p,
        Err(e) => {
            app.notify(e.to_string(), ModalKind::Error);
            return;
        }
    };

    app.config.theme = new_name.clone();
    if let Err(e) = app.config.save() {
        app.notify(
            format!("Theme exported but config save failed: {e}"),
            ModalKind::Warning,
        );
    }
    app.apply_active_theme();

    app.push_export_success(path);
}

impl App {
    /// Push the export-theme modal.  Builds the available-theme list
    /// from the live registry so the picker sees every built-in plus
    /// any user-authored theme in `themes/`.
    pub fn open_export_theme_modal(&mut self) {
        let themes = list_theme_names();
        let active = self.config.theme.clone();
        self.modal_stack
            .push(Box::new(ExportThemeModal::new(themes, &active)));
    }

    /// Push the "theme exported" success modal.  Public because the
    /// export handler in this file constructs it after a successful
    /// write.
    pub(super) fn push_export_success(&mut self, path: PathBuf) {
        self.modal_stack
            .push(Box::new(ExportSuccessModal::new(path)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_theme_file() -> ThemeFile {
        let theme = Theme::builtin("Ayu").expect("Ayu builtin theme exists");
        (&theme).into()
    }

    #[test]
    fn writes_toml_into_themes_dir_and_returns_path() {
        let tmp = TempDir::new().unwrap();
        let themes_dir = tmp.path().join("themes");
        let result = write_exported_theme(&themes_dir, "my-custom", &sample_theme_file());
        let path = result.expect("write should succeed");
        assert_eq!(path, themes_dir.join("my-custom.toml"));
        assert!(path.exists(), "file should be written");
        let bytes = std::fs::read_to_string(&path).unwrap();
        assert!(!bytes.is_empty(), "file should have content");
        // Round-trip parse to confirm valid TOML matching ThemeFile.
        toml::from_str::<ThemeFile>(&bytes).expect("written file parses back to ThemeFile");
    }

    #[test]
    fn create_new_rejects_collision_atomically() {
        let tmp = TempDir::new().unwrap();
        let themes_dir = tmp.path().join("themes");
        let first = write_exported_theme(&themes_dir, "dup", &sample_theme_file());
        assert!(first.is_ok());
        let second = write_exported_theme(&themes_dir, "dup", &sample_theme_file());
        match second {
            Err(ExportThemeError::AlreadyExists(p)) => {
                assert_eq!(p, themes_dir.join("dup.toml"));
            }
            other => panic!("expected AlreadyExists, got {other:?}"),
        }
    }

    #[test]
    fn creates_missing_themes_dir() {
        let tmp = TempDir::new().unwrap();
        let nested = tmp.path().join("nested").join("themes");
        assert!(!nested.exists());
        let path = write_exported_theme(&nested, "fresh", &sample_theme_file()).unwrap();
        assert!(path.exists());
        assert!(nested.is_dir());
    }

    #[test]
    fn resolve_source_theme_handles_builtin() {
        let file = resolve_source_theme("Ayu", true);
        let _round_trip = toml::to_string(&file).expect("builtin theme serialises");
    }
}
