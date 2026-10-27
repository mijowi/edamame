//! Vim modal state. `VimState` lives on `App` inside its `Option<ModalInput>` session (`Some` iff
//! `config.modal.handler == "vim"`) and holds the active sub-mode plus the accumulating
//! multi-key parse. The sub-mode is the *interaction* axis, orthogonal to
//! `EditorState::mode` (the *rendering* axis).

use crate::editor::vim_ops::{FindKind, VisualKind};

/// Upper bound on a count used as a *repetition*, so a held digit key cannot hang the UI.
///
/// Applied by consumers, not the accumulator: `feed::accumulate` saturates at `u32::MAX`
/// because `{count}G` reads the count as a line number. Every reader that drives iteration
/// clamps itself (via `feed::count_of` or the `[count1] op [count2]` products); a new
/// looping consumer owes the clamp. `feed::operand_count` is exempt only because its line
/// number is clamped to the document instead.
pub const COUNT_CAP: u32 = 9999;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VimSubMode {
    #[default]
    Normal,
    /// An operator was entered and awaits a motion or text object.
    OperatorPending,
    Insert,
    /// Charwise.
    Visual,
    VisualLine,
}

/// Operator awaiting a motion / text object (`d c y > <`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingOp {
    Delete,
    Change,
    Yank,
    IndentRight,
    IndentLeft,
}

/// Vim's unnamed register; `linewise` makes `p`/`P` open a new line. Separate from the OS
/// clipboard.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VimRegister {
    pub text: String,
    pub linewise: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdLineKind {
    Ex,
    SearchForward,
    SearchBackward,
}

impl CmdLineKind {
    pub fn prefix(self) -> char {
        match self {
            CmdLineKind::Ex => ':',
            CmdLineKind::SearchForward => '/',
            CmdLineKind::SearchBackward => '?',
        }
    }

    /// `/` and `?`, whose text is a search query in escape syntax rather than an ex command.
    pub fn is_search(self) -> bool {
        matches!(
            self,
            CmdLineKind::SearchForward | CmdLineKind::SearchBackward
        )
    }
}

/// Per-session `:` / search history cap (vim's default is 50).
pub const HISTORY_CAP: usize = 100;

/// The hint-line command-line buffer, active while typing `:` / `/` / `?`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdLineState {
    pub kind: CmdLineKind,
    pub input: String,
    /// Char index within `input`.
    pub cursor: usize,
    /// `Some(i)` while showing `history[i]`; `None` while editing the live draft.
    pub history_idx: Option<usize>,
    /// The live text stashed when history recall begins, restored on Down past the newest.
    pub draft: String,
}

impl CmdLineState {
    pub fn new(kind: CmdLineKind) -> Self {
        Self {
            kind,
            input: String::new(),
            cursor: 0,
            history_idx: None,
            draft: String::new(),
        }
    }

    /// Pre-filled with `input`, cursor at its end (the `'<,'>` range when `:` opens from Visual).
    pub fn with_input(kind: CmdLineKind, input: String) -> Self {
        let cursor = input.chars().count();
        Self {
            kind,
            input,
            cursor,
            history_idx: None,
            draft: String::new(),
        }
    }
}

/// The complete vim state held on `App`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VimState {
    pub sub_mode: VimSubMode,
    /// Leading count (the `3` in `3dw`). Uncapped; see [`COUNT_CAP`].
    pub count: Option<u32>,
    pub pending_op: Option<PendingOp>,
    /// Count between operator and motion (the `2` in `d2w`).
    pub motion_count: Option<u32>,
    /// First `g` of a `gg` sequence.
    pub pending_g: bool,
    pub pending_replace: bool,
    pub pending_find: Option<FindKind>,
    /// `Some(true)` = inner (`i`), `Some(false)` = around (`a`).
    pub pending_text_object: Option<bool>,
    /// Last `f`/`F`/`t`/`T` target, for `;` and `,`.
    pub last_find: Option<(FindKind, char)>,
    /// Char offset of the visual anchor; `Some` in Visual / VisualLine.
    pub visual_anchor: Option<usize>,
    /// Inclusive line span of the last Visual selection, captured when `:` opens from Visual
    /// mode: the bounds a `:'<,'>s` runs over (vim's `'<`/`'>` marks).
    pub last_visual_range: Option<(usize, usize)>,
    pub register: VimRegister,
    pub cmdline: Option<CmdLineState>,
    /// Live incremental-search session (vim `incsearch`), `Some` while a search prompt is
    /// open. See `editor::vim_ops::incsearch`.
    pub incsearch: Option<crate::editor::vim_ops::IncsearchSession>,
    /// Session-only, oldest first.
    pub ex_history: Vec<String>,
    /// Session-only, oldest first; `/` and `?` share it, as in vim.
    pub search_history: Vec<String>,
}

impl VimState {
    /// Clear the in-progress multi-key parse; `sub_mode`, register, last-find, and visual
    /// anchor outlive a single command sequence and are untouched.
    pub fn reset_pending(&mut self) {
        self.count = None;
        self.pending_op = None;
        self.motion_count = None;
        self.pending_g = false;
        self.pending_replace = false;
        self.pending_find = None;
        self.pending_text_object = None;
    }

    fn history_for(&mut self, kind: CmdLineKind) -> &mut Vec<String> {
        match kind {
            CmdLineKind::Ex => &mut self.ex_history,
            CmdLineKind::SearchForward | CmdLineKind::SearchBackward => &mut self.search_history,
        }
    }

    /// Record a submitted (non-empty) command line: a repeat moves to the end, and the list
    /// is capped at [`HISTORY_CAP`].
    pub fn record_command(&mut self, kind: CmdLineKind, cmd: &str) {
        let history = self.history_for(kind);
        if let Some(pos) = history.iter().position(|e| e == cmd) {
            history.remove(pos);
        }
        history.push(cmd.to_owned());
        let overflow = history.len().saturating_sub(HISTORY_CAP);
        if overflow > 0 {
            history.drain(0..overflow);
        }
    }

    pub fn is_visual_line(&self) -> bool {
        self.sub_mode == VimSubMode::VisualLine
    }

    /// The active Visual flavor, which picks the `vim_ops::visual` widening; `None` outside
    /// Visual, where `selection` is a plain half-open span.
    pub fn visual_kind(&self) -> Option<VisualKind> {
        match self.sub_mode {
            VimSubMode::Visual => Some(VisualKind::Char),
            VimSubMode::VisualLine => Some(VisualKind::Line),
            _ => None,
        }
    }

    /// Short uppercase badge for the status bar.
    pub fn mode_label(&self) -> &'static str {
        match self.sub_mode {
            VimSubMode::Normal | VimSubMode::OperatorPending => "NORMAL",
            VimSubMode::Insert => "INSERT",
            VimSubMode::Visual => "VISUAL",
            VimSubMode::VisualLine => "V-LINE",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_label_maps_each_sub_mode() {
        let mut v = VimState::default();
        assert_eq!(v.mode_label(), "NORMAL");
        v.sub_mode = VimSubMode::OperatorPending;
        assert_eq!(v.mode_label(), "NORMAL");
        v.sub_mode = VimSubMode::Insert;
        assert_eq!(v.mode_label(), "INSERT");
        v.sub_mode = VimSubMode::Visual;
        assert_eq!(v.mode_label(), "VISUAL");
        v.sub_mode = VimSubMode::VisualLine;
        assert_eq!(v.mode_label(), "V-LINE");
    }

    #[test]
    fn reset_pending_clears_parse_but_keeps_mode_and_register() {
        let mut v = VimState {
            sub_mode: VimSubMode::Insert,
            count: Some(3),
            pending_op: Some(PendingOp::Delete),
            motion_count: Some(2),
            pending_g: true,
            pending_replace: true,
            pending_find: Some(FindKind::Forward),
            pending_text_object: Some(true),
            register: VimRegister {
                text: "x".into(),
                linewise: true,
            },
            ..Default::default()
        };
        v.reset_pending();
        assert_eq!(v.count, None);
        assert_eq!(v.pending_op, None);
        assert_eq!(v.motion_count, None);
        assert!(!v.pending_g);
        assert!(!v.pending_replace);
        assert_eq!(v.pending_find, None);
        assert_eq!(v.pending_text_object, None);
        assert_eq!(v.sub_mode, VimSubMode::Insert);
        assert_eq!(v.register.text, "x");
    }

    #[test]
    fn record_command_dedups_to_end_and_keeps_search_separate() {
        let mut v = VimState::default();
        v.record_command(CmdLineKind::Ex, "w");
        v.record_command(CmdLineKind::Ex, "q");
        v.record_command(CmdLineKind::Ex, "w");
        assert_eq!(v.ex_history, vec!["q".to_owned(), "w".to_owned()]);
        v.record_command(CmdLineKind::SearchForward, "foo");
        v.record_command(CmdLineKind::SearchBackward, "bar");
        assert_eq!(v.search_history, vec!["foo".to_owned(), "bar".to_owned()]);
        assert_eq!(v.ex_history.len(), 2);
    }

    #[test]
    fn record_command_caps_history_dropping_oldest() {
        let mut v = VimState::default();
        for i in 0..HISTORY_CAP + 5 {
            v.record_command(CmdLineKind::Ex, &format!("cmd{i}"));
        }
        assert_eq!(v.ex_history.len(), HISTORY_CAP);
        assert_eq!(v.ex_history[0], "cmd5");

        assert_eq!(
            v.ex_history[HISTORY_CAP - 1],
            format!("cmd{}", HISTORY_CAP + 4)
        );
    }
}
