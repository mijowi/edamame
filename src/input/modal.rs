//! The mutually exclusive modal input session, independent of the rendering mode.

use crate::config::sections::{HELIX_HANDLER, VIM_HANDLER};

use super::{HelixState, VimState};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ModalInput {
    #[default]
    Default,
    Vim(Box<VimState>),
    Helix(Box<HelixState>),
}

impl ModalInput {
    pub fn from_handler(handler: &str) -> Self {
        match handler {
            VIM_HANDLER => Self::Vim(Box::default()),
            HELIX_HANDLER => Self::Helix(Box::default()),
            _ => Self::Default,
        }
    }

    pub fn vim(&self) -> Option<&VimState> {
        match self {
            Self::Vim(state) => Some(state),
            _ => None,
        }
    }

    pub fn vim_mut(&mut self) -> Option<&mut VimState> {
        match self {
            Self::Vim(state) => Some(state),
            _ => None,
        }
    }

    pub fn helix(&self) -> Option<&HelixState> {
        match self {
            Self::Helix(state) => Some(state),
            _ => None,
        }
    }

    pub fn helix_mut(&mut self) -> Option<&mut HelixState> {
        match self {
            Self::Helix(state) => Some(state),
            _ => None,
        }
    }

    /// UI identity and interaction mode; Vim keeps its established labels.
    pub fn mode_label(&self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Vim(state) => Some(state.mode_label()),
            Self::Helix(state) => Some(state.mode_label()),
        }
    }

    pub fn cmdline(&self) -> Option<&super::vim::state::CmdLineState> {
        match self {
            Self::Vim(state) => state.cmdline.as_ref(),
            Self::Helix(state) => state.prompt.as_ref().map(|prompt| &prompt.line),
            Self::Default => None,
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(self, Self::Default)
    }

    pub fn handler(&self) -> &'static str {
        match self {
            Self::Default => crate::config::sections::DEFAULT_HANDLER,
            Self::Vim(_) => VIM_HANDLER,
            Self::Helix(_) => HELIX_HANDLER,
        }
    }
}
