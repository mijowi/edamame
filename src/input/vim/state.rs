//! Vim modal state.
//!
//! `VimState` lives on `App` as `Option<VimState>` (`Some` iff
//! `config.modal.handler == "vim"`).  It survives across keystrokes and
//! is the single source of truth for the active sub-mode plus the
//! accumulating multi-key parse (counts, pending operator, pending
//! find, …).  It is deliberately orthogonal to `EditorState::mode`,
//! which is the *rendering* axis (Rendered / Raw); the sub-mode here is
//! the *interaction* axis (Normal / Insert / Visual).
//!
//! The full field set is laid down now even though CP1 only exercises a
//! subset, so later checkpoints add behavior without re-shaping the
//! struct.  See `docs/vim-implementation-plan.md` §2.2.

use crate::editor::vim_ops::FindKind;

/// Upper bound on an accumulated count so a held digit key can't grow an
/// unbounded `u32` (and so `3j` style repeats stay sane).
pub const COUNT_CAP: u32 = 9999;

/// Vim sub-mode — orthogonal to `EditorState::mode` (the rendering axis).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VimSubMode {
    #[default]
    Normal,
    /// `d`/`c`/`y`/`>`/`<` entered, awaiting a motion or text object.
    OperatorPending,
    Insert,
    /// Charwise visual selection.
    Visual,
    VisualLine,
}

/// Operator awaiting a motion / text object (`d c y >> <<`).
///
/// `Delete` / `Change` / `Yank` are wired in CP3; `IndentRight` /
/// `IndentLeft` (`>>` / `<<`) are wired in CP4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingOp {
    Delete,
    Change,
    Yank,
    IndentRight,
    IndentLeft,
}

/// Vim's unnamed register, with a charwise/linewise flag.  `dd`/`yy`/
/// visual-line operations set `linewise = true`; `p`/`P` then open a new
/// line for linewise content.  Kept entirely separate from the OS
/// clipboard (which `Ctrl-C`/`Ctrl-V` use).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VimRegister {
    pub text: String,
    pub linewise: bool,
}

/// Which command-line prompt is active (`:` / `/` / `?`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdLineKind {
    /// `:` ex command line (`:w`/`:q`/`:wq`/`:s`/`:%s`).
    Ex,
    SearchForward,
    SearchBackward,
}

impl CmdLineKind {
    /// The leading glyph shown at the start of the command line.
    pub fn prefix(self) -> char {
        match self {
            CmdLineKind::Ex => ':',
            CmdLineKind::SearchForward => '/',
            CmdLineKind::SearchBackward => '?',
        }
    }
}

/// Upper bound on the per-session `:` / search history; older entries are
/// dropped once a kind's history grows past this. Vim's default `history` is
/// 50 — 100 is comfortably more than a single editing session recalls while
/// staying trivially cheap to keep in memory.
pub const HISTORY_CAP: usize = 100;

/// The hint-line command-line buffer, active while typing `:` / `/` / `?`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdLineState {
    pub kind: CmdLineKind,
    pub input: String,
    /// Char index within `input`.
    pub cursor: usize,
    /// History-recall position: `None` while editing the live draft, `Some(i)`
    /// while showing `history[i]` after an Up. Down past the newest entry
    /// returns to `None` (and restores [`draft`](Self::draft)).
    pub history_idx: Option<usize>,
    /// The in-progress text stashed when history recall begins, so stepping
    /// Down past the newest entry restores what the user was typing.
    pub draft: String,
}

impl CmdLineState {
    /// A fresh, empty command line of the given `kind` (cursor at 0, not
    /// browsing history).
    pub fn new(kind: CmdLineKind) -> Self {
        Self {
            kind,
            input: String::new(),
            cursor: 0,
            history_idx: None,
            draft: String::new(),
        }
    }

    /// A command line pre-filled with `input`, cursor parked at its end — used
    /// for the `'<,'>` range vim inserts when `:` opens from Visual mode, so
    /// the user types the rest of the command after it.
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
    /// Leading count (the `3` in `3dw`); capped at [`COUNT_CAP`].
    pub count: Option<u32>,
    pub pending_op: Option<PendingOp>,
    /// Count between operator and motion (the `2` in `d2w`).
    pub motion_count: Option<u32>,
    /// First `g` of a `gg` sequence.
    pub pending_g: bool,
    /// `r` was pressed and is awaiting the replacement character (`r{c}`).
    pub pending_replace: bool,
    /// `f`/`F`/`t`/`T` was pressed and is awaiting the target character.
    pub pending_find: Option<FindKind>,
    /// `Some(true)` = inner (`i`), `Some(false)` = around (`a`).
    pub pending_text_object: Option<bool>,
    /// Last `f`/`F`/`t`/`T` target, for `;` and `,`.
    pub last_find: Option<(FindKind, char)>,
    /// Char offset of the visual anchor; `Some` in Visual / VisualLine.
    pub visual_anchor: Option<usize>,
    /// Inclusive buffer-line span (`first`, `last`) of the most recent Visual
    /// selection, captured when `:` opens the ex prompt from Visual mode — the
    /// concrete bounds a `:'<,'>s` substitution runs over (vim's `'<`/`'>`
    /// marks).  `None` until the first Visual `:`.
    pub last_visual_range: Option<(usize, usize)>,
    pub register: VimRegister,
    /// Active while typing a `:` / `/` / `?` command line.
    pub cmdline: Option<CmdLineState>,
    /// Session-only `:` ex-command history, oldest first, newest last.
    /// Recalled with Up/Down while the `:` prompt is open.
    pub ex_history: Vec<String>,
    /// Session-only search history (`/` and `?` share one register, as in
    /// vim), oldest first.
    pub search_history: Vec<String>,
}

impl VimState {
    /// Clear the in-progress multi-key parse (counts, pending operator,
    /// pending `g`, pending `r`, pending find, pending text-object).  Leaves
    /// `sub_mode`, the register, the last-find, and any visual anchor
    /// untouched — those have lifetimes independent of a single command
    /// sequence.
    pub fn reset_pending(&mut self) {
        self.count = None;
        self.pending_op = None;
        self.motion_count = None;
        self.pending_g = false;
        self.pending_replace = false;
        self.pending_find = None;
        self.pending_text_object = None;
    }

    /// The session history list for a command-line `kind`. `/` and `?` share
    /// the search history, as they do in vim.
    fn history_for(&mut self, kind: CmdLineKind) -> &mut Vec<String> {
        match kind {
            CmdLineKind::Ex => &mut self.ex_history,
            CmdLineKind::SearchForward | CmdLineKind::SearchBackward => &mut self.search_history,
        }
    }

    /// Record a submitted command line into the matching session history.
    /// A repeat of an existing entry is moved to the end (so Up walks distinct
    /// commands, newest first), and the list is capped at [`HISTORY_CAP`].
    /// `cmd` is assumed non-empty — empty submits are not recorded.
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

    /// Whether the active sub-mode is VisualLine — drives the render-path
    /// `visual_line_mode` line-expansion and the App-layer clipboard
    /// widening.
    pub fn is_visual_line(&self) -> bool {
        self.sub_mode == VimSubMode::VisualLine
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
        // Untouched.
        assert_eq!(v.sub_mode, VimSubMode::Insert);
        assert_eq!(v.register.text, "x");
    }

    #[test]
    fn record_command_dedups_to_end_and_keeps_search_separate() {
        let mut v = VimState::default();
        v.record_command(CmdLineKind::Ex, "w");
        v.record_command(CmdLineKind::Ex, "q");
        v.record_command(CmdLineKind::Ex, "w"); // repeat moves to the end
        assert_eq!(v.ex_history, vec!["q".to_owned(), "w".to_owned()]);
        // `/` and `?` share one history, separate from `:`.
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
        // The five oldest were dropped from the front.
        assert_eq!(v.ex_history[0], "cmd5");
        assert_eq!(
            v.ex_history[HISTORY_CAP - 1],
            format!("cmd{}", HISTORY_CAP + 4)
        );
    }
}
