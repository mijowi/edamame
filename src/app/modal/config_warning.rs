//! Surfaces non-fatal problems detected while reading `config.toml`,
//! `keybindings.toml`, or the active theme file.  Shown at startup
//! when [`crate::config::Config::load`] reports any [`ConfigWarning`]s,
//! and re-shown after the post-external-editor reload when those
//! reloads surface fresh warnings.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::{ConfigWarning, WarningKind};
use crate::ui::{ModalButton, ModalResponse, ModalState, ModalView};

pub struct ConfigWarningModal {
    pub(crate) body: Vec<Line<'static>>,
    pub(crate) buttons: Vec<ModalButton>,
    pub(crate) state: ModalState,
}

impl ConfigWarningModal {
    /// Build a warning modal from a list of warnings.  Returns `None`
    /// when the list is empty — callers can `if let Some(m) = ...`
    /// without an extra emptiness check.
    ///
    /// Body lines are grouped by file: each group leads with the file
    /// path (header style), followed by indented detail lines describing
    /// what went wrong.  Multiple warnings against the same file get
    /// separate groups in load order so the user can scroll through
    /// them.
    pub fn from_warnings(warnings: &[ConfigWarning]) -> Option<Self> {
        if warnings.is_empty() {
            return None;
        }
        let mut body: Vec<Line<'static>> = Vec::new();
        body.push(Line::raw(
            "Some configuration files had problems. Defaults were used for the affected entries.",
        ));
        body.push(Line::raw(""));
        for (idx, warning) in warnings.iter().enumerate() {
            if idx > 0 {
                body.push(Line::raw(""));
            }
            body.push(Line::raw(format!("• {}", warning.path.display())));
            match &warning.kind {
                WarningKind::ParseError(msg) => {
                    body.push(Line::raw("  Parse error:"));
                    for line in msg.lines() {
                        body.push(Line::raw(format!("    {line}")));
                    }
                }
                WarningKind::UnknownKeys(keys) => {
                    body.push(Line::raw("  Unrecognised keys (ignored):"));
                    for k in keys {
                        body.push(Line::raw(format!("    {k}")));
                    }
                }
                WarningKind::InvalidKeybindings(errs) => {
                    body.push(Line::raw("  Invalid keybinding entries (skipped):"));
                    for e in errs {
                        body.push(Line::raw(format!("    {e}")));
                    }
                }
            }
        }
        Some(Self {
            body,
            buttons: vec![ModalButton::new("Ok")],
            state: ModalState::new(),
        })
    }
}

impl Modal for ConfigWarningModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = ModalView {
            title: "Config warnings",
            body: &self.body,
            buttons: &self.buttons,
            theme: ctx.theme,
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
        match self.state.handle_key(&key, self.buttons.len()) {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.state.scroll_by(delta);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    //! `ConfigWarningModal::from_warnings` composes the body of the
    //! warning popup from a slice of [`ConfigWarning`].  These tests
    //! exercise the body shape directly so a regression in the
    //! formatting shows up without rendering through ratatui.

    use super::*;
    use std::path::PathBuf;

    #[test]
    fn empty_warnings_returns_none() {
        assert!(ConfigWarningModal::from_warnings(&[]).is_none());
    }

    #[test]
    fn parse_error_body_contains_path_and_message() {
        let warnings = vec![ConfigWarning {
            path: PathBuf::from("/home/u/.config/edamame/config.toml"),
            kind: WarningKind::ParseError("expected integer, found string at line 3".into()),
        }];
        let modal = ConfigWarningModal::from_warnings(&warnings).expect("modal built");
        let joined = modal
            .body
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("/home/u/.config/edamame/config.toml"));
        assert!(joined.contains("Parse error"));
        assert!(joined.contains("line 3"));
        assert_eq!(modal.buttons.len(), 1);
        assert_eq!(modal.buttons[0].label, "Ok");
    }

    #[test]
    fn unknown_keys_body_lists_each_path() {
        let warnings = vec![ConfigWarning {
            path: PathBuf::from("config.toml"),
            kind: WarningKind::UnknownKeys(vec!["editor.tab_widht".into(), "boguss".into()]),
        }];
        let modal = ConfigWarningModal::from_warnings(&warnings).expect("modal built");
        let joined = modal
            .body
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("Unrecognised keys"));
        assert!(joined.contains("editor.tab_widht"));
        assert!(joined.contains("boguss"));
    }

    #[test]
    fn invalid_keybindings_body_lists_each_error() {
        let warnings = vec![ConfigWarning {
            path: PathBuf::from("keybindings.toml"),
            kind: WarningKind::InvalidKeybindings(vec![
                "Quitt = \"ctrl+x\": unknown action name: 'Quitt'".into(),
            ]),
        }];
        let modal = ConfigWarningModal::from_warnings(&warnings).expect("modal built");
        let joined = modal
            .body
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("Invalid keybinding entries"));
        assert!(joined.contains("Quitt"));
    }

    #[test]
    fn multiple_warnings_separated_by_blank_lines() {
        let warnings = vec![
            ConfigWarning {
                path: PathBuf::from("a.toml"),
                kind: WarningKind::ParseError("oops".into()),
            },
            ConfigWarning {
                path: PathBuf::from("b.toml"),
                kind: WarningKind::UnknownKeys(vec!["x".into()]),
            },
        ];
        let modal = ConfigWarningModal::from_warnings(&warnings).expect("modal built");
        let joined = modal
            .body
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("a.toml"));
        assert!(joined.contains("b.toml"));
    }

    // ── App-level wiring ─────────────────────────────────────────────
    //
    // A warning that flows through `App::new` (or is pushed onto the
    // stack later) ends up on the modal stack, and dispatching Enter or
    // Escape pops it.  The body-content invariants are owned by the
    // builder tests above.

    use crate::app::test_utils::make_app;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn modal_dismissed_on_button_press() {
        let mut app = make_app();
        let warnings = vec![ConfigWarning {
            path: PathBuf::from("config.toml"),
            kind: WarningKind::ParseError("oops".into()),
        }];
        let modal = ConfigWarningModal::from_warnings(&warnings).expect("modal built");
        app.modal_stack.push(Box::new(modal));
        assert!(app.modal_stack.contains::<ConfigWarningModal>());
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 40, 80);
        assert!(!app.modal_stack.contains::<ConfigWarningModal>());
    }

    #[test]
    fn modal_dismissed_on_escape() {
        let mut app = make_app();
        let warnings = vec![ConfigWarning {
            path: PathBuf::from("config.toml"),
            kind: WarningKind::UnknownKeys(vec!["bogus".into()]),
        }];
        let modal = ConfigWarningModal::from_warnings(&warnings).expect("modal built");
        app.modal_stack.push(Box::new(modal));
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 40, 80);
        assert!(!app.modal_stack.contains::<ConfigWarningModal>());
    }
}
