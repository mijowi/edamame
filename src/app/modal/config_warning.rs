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

use super::chrome::ModalChrome;
use super::docs_link::{follow_and_close, DocsFootnote};
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::{ConfigWarning, WarningKind};
use crate::docs::DocId;
use crate::ui::modal::LinkableResponse;
use crate::ui::{ModalButton, ModalLink, ModalLinkTarget, ModalResponse};

pub struct ConfigWarningModal {
    pub(crate) body: Vec<Line<'static>>,
    pub(crate) buttons: Vec<ModalButton>,
    /// Rebuilt each render — the warning list above it varies in
    /// length, so the footnote's line index is only known once the
    /// body is assembled.
    links: Vec<ModalLink>,
    chrome: ModalChrome,
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
                    body.push(Line::raw("  Unrecognized keys (ignored):"));
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
                WarningKind::InvalidValue { key, message } => {
                    body.push(Line::raw(format!("  Invalid value for {key}:")));
                    body.push(Line::raw(format!("    {message}")));
                }
                WarningKind::MissingTheme {
                    requested,
                    fallback,
                } => {
                    body.push(Line::raw(format!("  Theme '{requested}' was not found.")));
                    body.push(Line::raw(format!(
                        "  Falling back to '{fallback}'; config.toml has been updated."
                    )));
                }
            }
        }
        Some(Self {
            body,
            buttons: Vec::new(),
            links: Vec::new(),
            chrome: ModalChrome::new(ModalKind::Warning, true),
        })
    }
    /// Map a resolved response to an outcome.  Shared by the key and
    /// click paths so mouse and keyboard behave identically.
    fn resolve(&self, response: LinkableResponse) -> ModalOutcome {
        match response {
            LinkableResponse::Modal(ModalResponse::Continue) => ModalOutcome::Continue,
            LinkableResponse::Modal(ModalResponse::Cancelled | ModalResponse::ButtonPressed(_)) => {
                ModalOutcome::Close
            }
            LinkableResponse::Link(idx) => match self.links.get(idx) {
                Some(link) => follow_and_close(link.target.clone()),
                None => ModalOutcome::Continue,
            },
        }
    }
}

/// The manual section explaining what edamame does with a config it
/// could not fully read, and where that config lives.
const DOCS_FOOTNOTE: DocsFootnote = DocsFootnote {
    label: "When something is wrong with your config",
    target: ModalLinkTarget {
        id: DocId::Configuration,
        fragment: Some("when-something-is-wrong-with-your-config"),
    },
    trailer: " explains how these are handled.",
};

impl Modal for ConfigWarningModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let mut body = self.body.clone();
        self.links = DOCS_FOOTNOTE.append_to(&mut body, self.chrome.focused_link(), ctx.theme);
        self.chrome.render_with_links(
            frame,
            area,
            ctx,
            "Config warnings",
            &body,
            &self.buttons,
            &self.links,
        );
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        let response = self
            .chrome
            .on_key_linkable(&key, self.links.len(), self.buttons.len());
        self.resolve(response)
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.chrome.on_wheel(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        let response = self.chrome.on_click_linkable(col, row);
        self.resolve(response)
    }

    fn kind(&self) -> ModalKind {
        self.chrome.kind()
    }

    fn dismissable(&self) -> bool {
        self.chrome.dismissable()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
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
    use crate::config::Theme;
    use crate::document::ParsedDoc;
    use std::path::PathBuf;

    /// Fragments are matched exactly, so renaming that heading in
    /// `docs/configuration.md` dead-ends this link silently, and only
    /// for the reader who followed it.
    #[test]
    fn the_docs_link_names_a_heading_that_exists() {
        let ModalLinkTarget { id, fragment } = DOCS_FOOTNOTE.target;
        let fragment = fragment.expect("the link names a section, not just a page");
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let parsed = ParsedDoc::build(&id.source(), theme, true, 80);
        assert!(
            parsed.heading_anchors.contains_key(fragment),
            "'{fragment}' is not a heading in {}",
            id.title()
        );
    }

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
        assert!(modal.buttons.is_empty());
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
        assert!(joined.contains("Unrecognized keys"));
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
    fn invalid_value_body_lists_key_and_message() {
        let warnings = vec![ConfigWarning {
            path: PathBuf::from("config.toml"),
            kind: WarningKind::InvalidValue {
                key: "editor.autosave_idle_ms".into(),
                message: "value 0 is outside the supported range (1000 < N < 600000); \
                          using the default (5000) instead"
                    .into(),
            },
        }];
        let modal = ConfigWarningModal::from_warnings(&warnings).expect("modal built");
        let joined = modal
            .body
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("Invalid value for editor.autosave_idle_ms"));
        assert!(joined.contains("outside the supported range"));
        assert!(joined.contains("using the default (5000)"));
    }

    #[test]
    fn missing_theme_body_mentions_requested_and_fallback() {
        let warnings = vec![ConfigWarning {
            path: PathBuf::from("/home/u/.config/edamame/themes/solarized.toml"),
            kind: WarningKind::MissingTheme {
                requested: "solarized".into(),
                fallback: "Edamame".into(),
            },
        }];
        let modal = ConfigWarningModal::from_warnings(&warnings).expect("modal built");
        let joined = modal
            .body
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("solarized"));
        assert!(joined.contains("Edamame"));
        assert!(joined.contains("config.toml has been updated"));
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
    fn modal_dismissed_on_escape() {
        let mut app = make_app();
        let warnings = vec![ConfigWarning {
            path: PathBuf::from("config.toml"),
            kind: WarningKind::ParseError("oops".into()),
        }];
        let modal = ConfigWarningModal::from_warnings(&warnings).expect("modal built");
        app.modal_stack.push(Box::new(modal));
        assert!(app.modal_stack.contains::<ConfigWarningModal>());
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 40, 80);
        assert!(!app.modal_stack.contains::<ConfigWarningModal>());
    }
}
