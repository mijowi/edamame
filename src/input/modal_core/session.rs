//! [`ModalInput`]: the live modal session, one variant per handler.  The App, the status bar and
//! the cursor style read a session only through these methods, so a handler's sub-mode type never
//! leaks past this file (tests aside: `vim` / `vim_mut` are `#[cfg(test)]`).

use crossterm::event::KeyEvent;

use crate::config::sections::VIM_HANDLER;
use crate::editor::vim_ops::{end_incsearch, VisualKind};
use crate::editor::EditorState;
use crate::input::vim::feed::cmdline_live_update;
use crate::input::vim::state::CmdLineState;
use crate::input::vim::{vim_feed, VimOutcome, VimState, VimSubMode};

/// Every `config.modal.handler` name [`ModalInput::for_handler`] builds a session for.  A new
/// handler goes in both; the test fails only for a listed name that builds nothing.
const MODAL_HANDLERS: &[&str] = &[VIM_HANDLER];

/// The active modal handler's session.  Built from `config.modal.handler`; the default handler
/// has no session at all (`Option<ModalInput>` is `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalInput {
    Vim(VimState),
}

/// The three interaction states every modal handler maps its sub-modes onto.  Drives the badge
/// color and the block-cursor style, so chip and cursor can never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeFace {
    /// Commands: vim Normal and Operator-pending.
    Normal,
    /// Text entry, delegated to the default editor.
    Insert,
    /// A selection is being shaped: vim Visual / V-LINE.
    Select,
}

/// The status-bar badge: the handler's own label, colored by its [`ModeFace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeBadge {
    pub label: &'static str,
    pub face: ModeFace,
}

impl ModalInput {
    /// A fresh session for `handler` (`config.modal.handler`), or `None` for the default handler
    /// and any name no modal handler claims.
    pub fn for_handler(handler: &str) -> Option<Self> {
        match handler {
            VIM_HANDLER => Some(Self::Vim(VimState::default())),
            _ => None,
        }
    }

    /// Whether `handler` names a modal handler, i.e. [`Self::for_handler`] builds a session.  The
    /// one test for "is a modal handler configured?", so an unknown name never half-enables one.
    pub fn is_modal_handler(handler: &str) -> bool {
        MODAL_HANDLERS.contains(&handler)
    }

    /// The `config.modal.handler` name this session was built from.
    pub fn handler(&self) -> &'static str {
        match self {
            Self::Vim(_) => VIM_HANDLER,
        }
    }

    /// Feed one key press to the live handler.
    pub fn feed(
        &mut self,
        editor: &mut EditorState,
        key: KeyEvent,
        viewport_height: usize,
        viewport_width: usize,
    ) -> VimOutcome {
        match self {
            Self::Vim(vim) => vim_feed(vim, editor, key, viewport_height, viewport_width),
        }
    }

    /// Text entry is delegated to the default editor: paste inserts, typing coalesces.
    pub fn is_insert(&self) -> bool {
        self.face() == ModeFace::Insert
    }

    /// The handler's sub-mode folded onto the shared three; vim's Operator-pending reads as Normal.
    pub fn face(&self) -> ModeFace {
        match self {
            Self::Vim(vim) => match vim.sub_mode {
                VimSubMode::Normal | VimSubMode::OperatorPending => ModeFace::Normal,
                VimSubMode::Insert => ModeFace::Insert,
                VimSubMode::Visual | VimSubMode::VisualLine => ModeFace::Select,
            },
        }
    }

    /// The status-bar badge: the handler's own label, colored by [`Self::face`].
    pub fn badge(&self) -> ModeBadge {
        let label = match self {
            Self::Vim(vim) => vim.mode_label(),
        };
        ModeBadge {
            label,
            face: self.face(),
        }
    }

    /// The widening `vim_ops::visual_span` applies to the stored selection, for the highlight
    /// and the clipboard; `None` paints the half-open span as stored.
    pub fn visual_kind(&self) -> Option<VisualKind> {
        match self {
            Self::Vim(vim) => vim.visual_kind(),
        }
    }

    /// A one-line V-LINE selection is charwise-empty, so the hint line can't infer it.
    pub fn is_visual_line(&self) -> bool {
        self.visual_kind() == Some(VisualKind::Line)
    }

    /// The open `/` `?` `:` prompt, if any.
    pub fn cmdline(&self) -> Option<&CmdLineState> {
        match self {
            Self::Vim(vim) => vim.cmdline.as_ref(),
        }
    }

    /// Mutable [`Self::cmdline`], for the App's prompt paste.
    pub fn cmdline_mut(&mut self) -> Option<&mut CmdLineState> {
        match self {
            Self::Vim(vim) => vim.cmdline.as_mut(),
        }
    }

    /// Re-derive the open prompt's live `:s` / incsearch preview after the App changed its text
    /// (a paste); `before` is the text it replaced.  See `vim::feed::cmdline_live_update`.
    pub fn cmdline_live_update(
        &mut self,
        editor: &mut EditorState,
        before: &str,
        viewport_height: usize,
        viewport_width: usize,
    ) {
        match self {
            Self::Vim(vim) => {
                cmdline_live_update(vim, editor, before, viewport_height, viewport_width)
            }
        }
    }

    /// Drop back to Normal after the App consumed the selection (a Visual Cut or Paste).  Leaves
    /// `editor.selection` to the caller.
    pub fn leave_select(&mut self) {
        match self {
            Self::Vim(vim) => {
                vim.sub_mode = VimSubMode::Normal;
                vim.visual_anchor = None;
            }
        }
    }

    /// End every half-typed command before diff review takes over the keys: close the prompt,
    /// end a live incsearch session (restoring its saved view), and drop the pending parse.
    pub fn reset_for_diff_entry(&mut self, editor: &mut EditorState) {
        match self {
            Self::Vim(vim) => {
                end_incsearch(editor, &mut vim.incsearch);
                vim.cmdline = None;
                vim.reset_pending();
            }
        }
    }

    #[cfg(test)]
    pub fn vim(&self) -> Option<&VimState> {
        match self {
            Self::Vim(vim) => Some(vim),
        }
    }

    #[cfg(test)]
    pub fn vim_mut(&mut self) -> Option<&mut VimState> {
        match self {
            Self::Vim(vim) => Some(vim),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vim(sub_mode: VimSubMode) -> ModalInput {
        ModalInput::Vim(VimState {
            sub_mode,
            ..VimState::default()
        })
    }

    #[test]
    fn only_a_modal_handler_builds_a_session() {
        for name in MODAL_HANDLERS {
            assert!(ModalInput::is_modal_handler(name));
            assert!(
                ModalInput::for_handler(name).is_some_and(|m| m.handler() == *name),
                "{name} is listed but builds no session of its own"
            );
        }
        for name in ["default", "unknown", "Vim"] {
            assert!(!ModalInput::is_modal_handler(name), "{name}");
            assert!(ModalInput::for_handler(name).is_none(), "{name}");
        }
    }

    #[test]
    fn vim_sub_modes_map_onto_badges() {
        for (sub_mode, face) in [
            (VimSubMode::Normal, ModeFace::Normal),
            (VimSubMode::OperatorPending, ModeFace::Normal),
            (VimSubMode::Insert, ModeFace::Insert),
            (VimSubMode::Visual, ModeFace::Select),
            (VimSubMode::VisualLine, ModeFace::Select),
        ] {
            let m = vim(sub_mode);
            let label = m.vim().unwrap().mode_label();
            assert_eq!(m.badge(), ModeBadge { label, face }, "{sub_mode:?}");
        }
    }

    #[test]
    fn only_v_line_is_visual_line() {
        assert!(vim(VimSubMode::VisualLine).is_visual_line());
        assert!(!vim(VimSubMode::Visual).is_visual_line());
    }

    #[test]
    fn leave_select_returns_to_normal_and_drops_the_anchor() {
        let mut m = ModalInput::Vim(VimState {
            sub_mode: VimSubMode::VisualLine,
            visual_anchor: Some(4),
            ..VimState::default()
        });
        m.leave_select();
        assert_eq!(m.face(), ModeFace::Normal);
        assert_eq!(m.vim().unwrap().visual_anchor, None);
    }
}
