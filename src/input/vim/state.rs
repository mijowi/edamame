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
#[allow(dead_code)] // the command line is wired in CP8 / CP9
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdLineKind {
    Ex,
    SearchForward,
    SearchBackward,
}

/// The hint-line command-line buffer, active while typing `:` / `/` / `?`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdLineState {
    pub kind: CmdLineKind,
    pub input: String,
    /// Char index within `input`.
    pub cursor: usize,
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
    pub register: VimRegister,
    /// Active while typing a `:` / `/` / `?` command line.
    pub cmdline: Option<CmdLineState>,
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
}
