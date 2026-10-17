use crate::document::{Cursor, Selection};
use crate::editor::EditorState;
use crate::input::vim::state::CmdLineState;

pub const PROMPT_CHAR_CAP: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HelixSubMode {
    #[default]
    Normal,
    Insert,
    Select,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HelixPending {
    #[default]
    None,
    G,
    Find(char),
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HelixState {
    pub sub_mode: HelixSubMode,
    pub count: Option<u32>,
    pub pending: HelixPending,
    pub prompt: Option<HelixPrompt>,
    pub selection_linewise: bool,
    /// Helix's internal register, independent of Vim and the system clipboard.
    pub register: HelixRegister,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelixPrompt {
    pub line: CmdLineState,
    pub origin: PromptOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HelixRegister {
    pub text: String,
    pub linewise: bool,
}

impl HelixState {
    /// Close the prompt; restore its origin only when allowed and still on the same buffer version.
    pub fn cancel_prompt(&mut self, editor: &mut EditorState, restore: bool) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        let origin = prompt.origin;
        if restore && editor.buffer.version() == origin.version {
            editor.selection = origin.selection;
            editor.cursor = origin.cursor;
            editor.scroll = origin.scroll;
            editor.update_cursor_block();
        }
    }

    /// Clear the in-progress key sequence without changing the interaction mode.
    pub fn reset_pending(&mut self) {
        self.count = None;
        self.pending = HelixPending::None;
    }

    pub fn mode_label(&self) -> &'static str {
        match self.sub_mode {
            HelixSubMode::Normal => "HELIX NORMAL",
            HelixSubMode::Insert => "HELIX INSERT",
            HelixSubMode::Select => "HELIX SELECT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptOrigin {
    pub selection: Option<Selection>,
    pub cursor: Cursor,
    pub scroll: usize,
    pub version: u64,
}
