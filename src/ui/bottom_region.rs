//! Phase 9 — bottom status region.
//!
//! Composes the contextual [`HintLine`] on top of the persistent
//! [`StatusBar`].  The hint line adapts to the cursor's context
//! (default / table / list) and can be overlaid by a transient message
//! or preempted by a modal prompt.  In compact mode only the status
//! line renders.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::config::keymap::format_key_compact;
use crate::config::{Action, KeyMap, StatusBarLayout, Theme};
use crate::diff::Decision;
use crate::editor::{EditorState, Mode};

use super::status_bar::{StatusBar, StatusBarState};

/// A single keybind chord + label pair (e.g. `^C` + `Copy`).  The
/// chord glyph alone renders in the contrasting `hint_chord` theme
/// slot (no surrounding padding inside the badge), followed by a
/// single space, the label in `hint_label`, and a two-space separator
/// between successive hints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintChord {
    pub chord: String,
    pub label: String,
}

impl HintChord {
    pub fn new(chord: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            chord: chord.into(),
            label: label.into(),
        }
    }
}

/// A default-hint payload: an optional leading plaintext hint (e.g.
/// `Press any key to edit` in Preview mode) followed by a chord row.
#[derive(Debug, Clone, Default)]
pub struct HintSet {
    pub prelude: Option<String>,
    pub chords: Vec<HintChord>,
}

/// What the hint line currently displays.  The three variants are
/// mutually exclusive — a transient message replaces the default
/// chords, and a modal prompt replaces both.
///
/// All variants own their strings so the hint can be built up-front
/// and then passed by value into [`EditorView`] without entangling its
/// borrow of `&mut self.editor`.
#[derive(Debug, Clone)]
pub enum HintContent {
    /// Default: contextual keybind chords, with an optional leading
    /// plaintext prelude.
    Chords(HintSet),
    /// Transient overlay (e.g. `Copied`, `Saved`).
    Transient {
        text: String,
        style: ratatui::style::Style,
    },
    /// Modal prompt: a leading prompt string followed by chord options.
    Prompt {
        prompt: String,
        chords: Vec<HintChord>,
    },
}

/// Pick the default hint set for `state`, adapting to the cursor's
/// Markdown context.  Pure function so it can be unit-tested without
/// spinning up a terminal.
///
/// Chord glyphs are looked up dynamically from `keymap` so that
/// rebinds applied through the keybinds overlay (or via
/// `keybindings.toml`) appear in the hint line on the very next
/// frame — no chord text is hardcoded.  Chords for actions that the
/// user has unbound are silently dropped from the row.
///
/// Priority: table (Rendered only) > task-list item > mode-default.
/// Tables don't appear in Raw mode because the table-editing chords
/// don't work against the raw source — the user is editing the plain
/// Markdown and `Tab` / `⌥↑↓` insert characters or do nothing.
pub fn hint_line_for(state: &EditorState, keymap: &KeyMap) -> HintSet {
    match state.mode {
        Mode::Preview => HintSet {
            prelude: Some("Press any key to edit".to_owned()),
            chords: chords_from(
                keymap,
                &[
                    (Action::ShowCommandPalette, "Menu"),
                    (Action::GoToSection, "Go to"),
                    (Action::Copy, "Copy"),
                    (Action::Quit, "Quit"),
                ],
            ),
        },
        Mode::Rendered | Mode::Raw if state.selection_size().is_some() => HintSet {
            prelude: None,
            chords: chords_from(
                keymap,
                &[
                    (Action::Cut, "Cut"),
                    (Action::Copy, "Copy"),
                    // Paste is included so the user can replace the
                    // selection with the clipboard contents in one
                    // chord.  The whole baseline row is suppressed
                    // for the duration of the selection — mirroring
                    // how the table-context row replaces the row
                    // wholesale rather than prepending to it.
                    (Action::Paste, "Paste"),
                ],
            ),
        },
        Mode::Rendered if cursor_in_table(state) => HintSet {
            prelude: None,
            chords: table_chords(keymap),
        },
        Mode::Diff => {
            let all_resolved = state.diff.as_ref().is_some_and(|d| d.all_resolved());
            let focused_resolved = state.diff.as_ref().is_some_and(|d| {
                d.focused_decision()
                    .is_some_and(|dec| dec != Decision::Pending)
            });
            HintSet {
                prelude: None,
                chords: diff_review_chords(all_resolved, focused_resolved),
            }
        }
        Mode::Rendered | Mode::Raw => {
            // Baseline edit-mode hints — Menu anchors the row so
            // the command-palette chord is always the discovery
            // entry; then Paste / Undo / [Redo] / Open / Save /
            // Preview / view-mode toggle / Quit.  Redo is gated on
            // `state.history.can_redo()` so it only appears when
            // there's actually something to redo; the row shifts
            // by one slot when it pops in or out.  Cut / Copy are
            // absent from the baseline because they're only useful
            // with an active selection, which is handled by the
            // selection-override arm above.  The view-mode chord
            // label flips with the current mode (Rendered → "Raw",
            // Raw → "Render") and "Preview" / "Raw" / "Render" are
            // all destination labels — never the current state.
            //
            // Contextual chords (those that only make sense in a
            // specific state) are prepended to the front of the row
            // so the user sees them immediately.  Final order when
            // both are active: Link, Toggle, Menu, ...  Link leads
            // because its trigger is the narrowest (a specific
            // `[text](url)` span); Toggle follows.
            let view_toggle_label = match state.mode {
                Mode::Raw => "Render",
                _ => "Raw",
            };
            let redo_entry = state.history.can_redo().then_some((Action::Redo, "Redo"));
            let baseline = [
                Some((Action::ShowCommandPalette, "Menu")),
                Some((Action::GoToSection, "Go to")),
                Some((Action::Paste, "Paste")),
                Some((Action::Undo, "Undo")),
                redo_entry,
                Some((Action::Open, "Open")),
                Some((Action::Save, "Save")),
                Some((Action::ExitToPreview, "Preview")),
                Some((Action::ToggleRawMode, view_toggle_label)),
                Some((Action::Quit, "Quit")),
            ];
            let entries: Vec<(Action, &str)> = baseline.into_iter().flatten().collect();
            let mut chords = chords_from(keymap, &entries);
            // Insertion order here matters for the final layout:
            // each `insert(0, ..)` pushes the previous head back by
            // one slot, so the LAST insert ends up leftmost.  Order
            // of inserts below is the REVERSE of the desired visual
            // order: Toggle → Link.
            if cursor_on_task_item(state) {
                if let Some(c) = chord_for(keymap, &Action::ToggleCheckbox, "Toggle") {
                    chords.insert(0, c);
                }
            }
            if cursor_on_link(state) {
                if let Some(c) = chord_for(keymap, &Action::FollowLinkUnderCursor, "Open link") {
                    chords.insert(0, c);
                }
            }
            HintSet {
                prelude: None,
                chords,
            }
        }
    }
}

/// Look up the first key bound to `action` in `keymap` and pair it
/// with `label`.  Returns `None` when the action is unbound — the
/// caller drops unbound entries from the hint row entirely.
fn chord_for(keymap: &KeyMap, action: &Action, label: &str) -> Option<HintChord> {
    let ev = keymap.first_key_event_for(action)?;
    Some(HintChord::new(format_key_compact(&ev), label.to_owned()))
}

/// Convenience — apply [`chord_for`] over a slice and collect the
/// successful lookups in order.
fn chords_from(keymap: &KeyMap, entries: &[(Action, &str)]) -> Vec<HintChord> {
    entries
        .iter()
        .filter_map(|(action, label)| chord_for(keymap, action, label))
        .collect()
}

/// Diff Review hint row.  The key glyphs come from the shared
/// `diff_keys` table (via [`crate::input::diff_hint`]) — the same source
/// the input handler, keybinds overlay, decision divider, and
/// diff-intro modal read — so the advertised chord can never disagree
/// with the key that actually fires.  The labels are this row's own
/// (terse, to fit the bar).
///
/// `Esc Exit` trails the row, and only once every hunk is resolved:
/// diff mode can't be exited via `Esc` while hunks are still pending
/// (see `Action::DiffExit`), so advertising the chord before then
/// would be misleading.
fn diff_review_chords(all_resolved: bool, focused_resolved: bool) -> Vec<HintChord> {
    let mk = |action: &Action, label: &str| {
        HintChord::new(crate::input::diff_hint(action), label.to_owned())
    };
    let mut chords = vec![
        mk(&Action::DiffNext, "Next"),
        mk(&Action::DiffPrev, "Prev"),
        mk(&Action::DiffAcceptHunk, "Accept"),
        mk(&Action::DiffRejectHunk, "Reject"),
        mk(&Action::DiffAcceptAll, "Accept all"),
        mk(&Action::DiffRejectAll, "Reject all"),
    ];
    // `⌫ Reset` only makes sense once the focused hunk carries a
    // decision — it's a no-op on a still-`Pending` hunk, so advertising
    // it then would be misleading.
    if focused_resolved {
        chords.push(mk(&Action::DiffResetHunk, "Reset"));
    }
    // `Esc Exit` trails the row so the primary review actions lead; it
    // appears only once the whole diff is resolved.
    if all_resolved {
        chords.push(mk(&Action::DiffExit, "Exit"));
    }
    chords
}

/// Build the table-context hint row.  When the four arrow-driven
/// actions of a bundle (`Move row/col`, `Insert row/col`) all share
/// modifiers and arrow key codes, we collapse them into a single
/// glyph (`⌥↑↓←→`) — the visually compact shape the user already
/// learns from the default keymap.  When the user has rebound any of
/// the four to a non-arrow chord, we fall back to listing the four
/// chords joined by `/` so the badge still reflects what's actually
/// bound.
fn table_chords(keymap: &KeyMap) -> Vec<HintChord> {
    let mut out: Vec<HintChord> = Vec::new();
    // "Next cell" reuses InsertTab's chord because table next-cell is
    // a context dispatch from InsertTab in `edit_ops`.  Looking up
    // InsertTab keeps the displayed chord truthful even if the user
    // rebinds Tab to something exotic.
    if let Some(c) = chord_for(keymap, &Action::InsertTab, "Next cell") {
        out.push(c);
    }
    if let Some(c) = chord_for(keymap, &Action::TablePrevCell, "Prev cell") {
        out.push(c);
    }
    if let Some(badge) = arrow_bundle_chord(
        keymap,
        &Action::TableMoveRowUp,
        &Action::TableMoveRowDown,
        &Action::TableMoveColumnLeft,
        &Action::TableMoveColumnRight,
    ) {
        out.push(HintChord::new(badge, "Move row/col"));
    }
    if let Some(badge) = arrow_bundle_chord(
        keymap,
        &Action::TableInsertRowAbove,
        &Action::TableInsertRowBelow,
        &Action::TableInsertColumnLeft,
        &Action::TableInsertColumnRight,
    ) {
        out.push(HintChord::new(badge, "Insert row/col"));
    }
    if let Some(c) = chord_for(keymap, &Action::TableDeleteRow, "Del row") {
        out.push(c);
    }
    if let Some(c) = chord_for(keymap, &Action::TableDeleteColumn, "Del col") {
        out.push(c);
    }
    out
}

/// Compose a single chord glyph for an arrow-driven bundle (e.g.
/// `⌥↑↓←→` for the four `TableMoveRow*` / `TableMoveColumn*`
/// actions).  Returns the bundled glyph when all four chords share
/// modifiers AND each maps to its expected arrow direction; falls
/// back to a slash-joined list of compact chords otherwise.  Returns
/// `None` only when none of the four actions is bound — there's
/// nothing to display in that case.
fn arrow_bundle_chord(
    keymap: &KeyMap,
    up: &Action,
    down: &Action,
    left: &Action,
    right: &Action,
) -> Option<String> {
    let bound: Vec<KeyEvent> = [up, down, left, right]
        .iter()
        .filter_map(|a| keymap.first_key_event_for(a))
        .collect();
    if bound.is_empty() {
        return None;
    }
    let modifiers_match = bound.iter().all(|e| e.modifiers == bound[0].modifiers);
    let arrows_match = bound.len() == 4
        && bound[0].code == KeyCode::Up
        && bound[1].code == KeyCode::Down
        && bound[2].code == KeyCode::Left
        && bound[3].code == KeyCode::Right;
    if modifiers_match && arrows_match {
        let mut prefix = String::new();
        if bound[0].modifiers.contains(KeyModifiers::CONTROL) {
            prefix.push('^');
        }
        if bound[0].modifiers.contains(KeyModifiers::ALT) {
            prefix.push('⌥');
        }
        if bound[0].modifiers.contains(KeyModifiers::SHIFT) {
            prefix.push('⇧');
        }
        return Some(format!("{prefix}↑↓←→"));
    }
    Some(
        bound
            .iter()
            .map(format_key_compact)
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Lay out a chord list into spans.  Always renders every chord with
/// its label — if the row is too narrow, the trailing chords are
/// truncated by ratatui's non-wrapping `Paragraph`.  A bare chord
/// badge with no label isn't useful, so we don't bother dropping
/// labels under width pressure.
///
/// Layout per hint: `{chord}` in `hint_chord` (the badge is exactly
/// the chord glyph — no surrounding padding gets the badge bg), then
/// ` {label}` in `hint_label` (a single leading space separates label
/// from chord), then `  ` (two spaces) in `bar_style` as the separator
/// before the next hint.
///
/// `bar_style` is the hint-bar background for the active mode — normally
/// [`Theme::hint_bar`], but [`Theme::hint_bar_diff`] while in diff mode
/// so the inter-chord separators match the recolored bar instead of
/// punching the default hue through every gap.
pub fn lay_out_chords(chords: &[HintChord], theme: &Theme, bar_style: Style) -> Vec<Span<'static>> {
    // Wash the chord-badge and label backgrounds with the active bar's
    // bg so the whole hint row reads as one bar.  In every mode except
    // diff this is a no-op (`hint_bar`, `hint_chord` and `hint_label`
    // all share `surface_elevated`); in diff mode it extends the
    // recolored `hint_bar_diff` wash across the badges instead of
    // leaving them on the default surface.
    let chord_style = match bar_style.bg {
        Some(bg) => theme.hint_chord.bg(bg),
        None => theme.hint_chord,
    };
    let label_style = match bar_style.bg {
        Some(bg) => theme.hint_label.bg(bg),
        None => theme.hint_label,
    };
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(chords.len() * 3);
    for chord in chords {
        spans.push(Span::styled(chord.chord.clone(), chord_style));
        spans.push(Span::styled(format!(" {}", chord.label), label_style));
        spans.push(Span::styled("  ".to_string(), bar_style));
    }
    spans
}

/// The hint-line widget.  Renders chords / transient / prompt onto a
/// single row, with a trailing fill using `bar_style`.
pub struct HintLine<'a> {
    pub content: HintContent,
    pub theme: &'a Theme,
    /// Background style for the bar fill and inter-chord separators.
    /// [`Theme::hint_bar`] in every mode except diff, which uses
    /// [`Theme::hint_bar_diff`] so the recolored bar signals the mode
    /// change (Phase 1 §7).
    pub bar_style: Style,
}

impl<'a> Widget for HintLine<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let width = area.width as usize;

        let spans: Vec<Span<'_>> = match &self.content {
            HintContent::Chords(set) => {
                let mut v: Vec<Span<'_>> = Vec::new();
                // Prelude — plain text on the hint bar, followed by a
                // two-space gap that acts as a separator before the
                // chord row.  Rendered as hint_bar bg + hint_label fg
                // so it reads as a sentence, not another chord.
                if let Some(prelude) = &set.prelude {
                    let text = format!(" {}  ", prelude);
                    let prelude_style = match self.bar_style.bg {
                        Some(bg) => self.theme.hint_label.bg(bg),
                        None => self.theme.hint_label,
                    };
                    v.push(Span::styled(text, prelude_style));
                }
                v.extend(lay_out_chords(&set.chords, self.theme, self.bar_style));
                v
            }
            HintContent::Transient { text, style } => {
                vec![Span::styled(format!(" {} ", text), *style)]
            }
            HintContent::Prompt { prompt, chords } => {
                let prompt_text = format!(" {}  ", prompt);
                let prompt_span = Span::styled(prompt_text, self.theme.transient_warning);
                let chord_spans = lay_out_chords(chords, self.theme, self.bar_style);
                let mut v = vec![prompt_span];
                v.extend(chord_spans);
                v
            }
        };

        // Sum what we've rendered so we can pad the trailing fill with
        // the hint_bar background — otherwise the terminal shows its
        // own default background in the gap.
        let used: usize = spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum::<usize>();
        let mut all_spans = spans;
        if used < width {
            all_spans.push(Span::styled(" ".repeat(width - used), self.bar_style));
        }

        Paragraph::new(Line::from(all_spans))
            .style(self.bar_style)
            .render(area, buf);
    }
}

/// Composite widget owning the bottom region layout.  Renders a
/// [`HintLine`] above a [`StatusBar`] in two-line mode; collapses to
/// just the status bar in compact mode.
pub struct BottomRegion<'a> {
    pub status: StatusBarState<'a>,
    pub hint: HintContent,
    pub layout: StatusBarLayout,
    pub theme: &'a Theme,
}

impl<'a> BottomRegion<'a> {
    /// Height in rows that [`BottomRegion`] requires.  Consulted by
    /// `EditorView` to partition the terminal area.
    pub fn height(layout: StatusBarLayout) -> u16 {
        match layout {
            StatusBarLayout::TwoLine => 2,
            StatusBarLayout::Compact => 1,
        }
    }
}

impl<'a> Widget for BottomRegion<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        match self.layout {
            StatusBarLayout::Compact => {
                StatusBar {
                    state: self.status,
                    theme: self.theme,
                }
                .render(area, buf);
            }
            StatusBarLayout::TwoLine => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(1), Constraint::Length(1)])
                    .split(area);
                // Diff mode recolors the whole hint bar to match the
                // status bar's mode shift (§7).
                let bar_style = if matches!(self.status.mode, Mode::Diff) {
                    self.theme.hint_bar_diff
                } else {
                    self.theme.hint_bar
                };
                HintLine {
                    content: self.hint,
                    theme: self.theme,
                    bar_style,
                }
                .render(chunks[0], buf);
                StatusBar {
                    state: self.status,
                    theme: self.theme,
                }
                .render(chunks[1], buf);
            }
        }
    }
}

/// True when the editor's cursor sits inside a Markdown table.  Mirror
/// of the App-internal helper so pure hint-line code doesn't depend on
/// private app state.
fn cursor_in_table(state: &EditorState) -> bool {
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    let source = state.buffer.contents();
    crate::editor::table_edit::find_table_at(&source, cursor_byte).is_some()
}

/// True when the cursor sits on a Markdown *task* list item (i.e. a
/// list item whose marker is followed by `[ ]` / `[x]`).  Regular
/// bullet / ordered items return false — they have no checkbox to
/// toggle, so offering `^Space Toggle` would just confuse the user.
fn cursor_on_task_item(state: &EditorState) -> bool {
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    let source = state.buffer.contents();
    let Some(list) = crate::editor::list_edit::find_list_at(&source, cursor_byte) else {
        return false;
    };
    list.items
        .iter()
        .find(|it| cursor_byte >= it.start && cursor_byte <= it.end)
        .is_some_and(|it| it.task.is_some())
}

/// True when the cursor sits inside a `[text](url)` link on the
/// current line.  Reuses the Phase 8 `link_at_offset` scan that
/// `mouse_ops` and `App::resolve_link_at_cursor` use, so hint
/// visibility and the actual `FollowLinkUnderCursor` dispatch agree
/// on what counts as a link.
fn cursor_on_link(state: &EditorState) -> bool {
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    let source = state.buffer.contents();
    crate::editor::mouse_ops::link_at_offset(&source, cursor_byte).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{KeyBindingOverrides, Theme};
    use crate::document::Buffer;
    use crate::editor::{EditorState, Mode};
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn state(text: &str) -> EditorState {
        EditorState::new(Buffer::from_str(text), theme())
    }

    fn keymap() -> KeyMap {
        KeyMap::build(&KeyBindingOverrides::default()).unwrap()
    }

    // ── hint_line_for ─────────────────────────────────────────────

    #[test]
    fn preview_hint_has_prelude_and_menu_first() {
        let st = state("hello");
        let set = hint_line_for(&st, &keymap());
        assert_eq!(set.prelude.as_deref(), Some("Press any key to edit"));
        assert_eq!(set.chords[0].chord, "^P");
        assert_eq!(set.chords[0].label, "Menu");
        assert!(set.chords.iter().any(|c| c.label == "Quit"));
    }

    #[test]
    fn diff_hint_gates_exit_on_full_resolution() {
        // Pending hunks: no `Esc Exit` hint (diff can't be exited yet),
        // and the navigation/decision chords lead the row.
        let pending = diff_review_chords(false, false);
        assert!(
            !pending.iter().any(|c| c.label == "Exit"),
            "Exit hint must be hidden while hunks are pending",
        );
        assert_eq!(pending[0].label, "Next", "Tab/Next leads when pending");

        // All resolved: the review actions still lead, and `Esc Exit`
        // appears at the very end of the row.
        let resolved = diff_review_chords(true, true);
        assert_eq!(resolved[0].label, "Next", "review actions lead the row");
        let last = resolved.last().expect("non-empty row");
        assert_eq!(last.chord, "Esc");
        assert_eq!(last.label, "Exit");
    }

    #[test]
    fn diff_reset_hint_only_when_focused_hunk_resolved() {
        // Focused hunk still pending → no `Reset` chord (it'd be a
        // no-op there).
        let pending = diff_review_chords(false, false);
        assert!(
            !pending.iter().any(|c| c.label == "Reset"),
            "Reset hint must be hidden while the focused hunk is pending",
        );
        // Focused hunk decided → `⌫ Reset` is offered.
        let decided = diff_review_chords(false, true);
        let reset = decided
            .iter()
            .find(|c| c.label == "Reset")
            .expect("Reset hint must appear once the focused hunk is decided");
        assert_eq!(reset.chord, "⌫");
    }

    #[test]
    fn rendered_hint_has_save_and_paste_and_raw() {
        let mut st = state("hello");
        st.mode = Mode::Rendered;
        let set = hint_line_for(&st, &keymap());
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(set.chords[0].chord, "^P", "Menu is always first");
        assert!(labels.contains(&"Paste"));
        assert!(labels.contains(&"Save"));
        assert!(
            labels.contains(&"Raw"),
            "Rendered-mode chord toggles TO Raw"
        );
        assert!(labels.contains(&"Quit"));
        // Cut / Copy are selection-gated — no selection here.
        assert!(
            !labels.contains(&"Cut"),
            "Cut must stay hidden without an active selection"
        );
        assert!(
            !labels.contains(&"Copy"),
            "Copy must stay hidden without an active selection"
        );
        // Plain paragraph has no link, so "Open link" must not appear.
        assert!(
            !labels.contains(&"Open link"),
            "link hint must stay hidden when the cursor isn't on a link"
        );
        // No prelude in edit mode.
        assert!(set.prelude.is_none());
    }

    #[test]
    fn redo_hint_only_appears_when_history_can_redo() {
        use crate::document::history::EditDelta;

        let mut st = state("hello");
        st.mode = Mode::Rendered;

        // Fresh history → no redo entry yet.
        let labels: Vec<_> = hint_line_for(&st, &keymap())
            .chords
            .iter()
            .map(|c| c.label.clone())
            .collect();
        assert!(
            !labels.contains(&"Redo".to_string()),
            "Redo must be hidden when history.can_redo() is false: {labels:?}"
        );

        // Record an edit and undo it — now redo is available.
        st.buffer.insert(5, "!");
        st.history.record(EditDelta {
            offset: 5,
            removed: String::new(),
            inserted: "!".into(),
        });
        st.history.undo(&mut st.buffer).unwrap();
        assert!(st.history.can_redo(), "test premise");

        let labels: Vec<_> = hint_line_for(&st, &keymap())
            .chords
            .iter()
            .map(|c| c.label.clone())
            .collect();
        assert!(
            labels.contains(&"Redo".to_string()),
            "Redo must appear once history.can_redo() is true: {labels:?}"
        );

        // Recording a fresh edit clears the redo stack — Redo vanishes.
        st.buffer.insert(0, "X");
        st.history.record(EditDelta {
            offset: 0,
            removed: String::new(),
            inserted: "X".into(),
        });
        assert!(!st.history.can_redo(), "test premise");
        let labels: Vec<_> = hint_line_for(&st, &keymap())
            .chords
            .iter()
            .map(|c| c.label.clone())
            .collect();
        assert!(
            !labels.contains(&"Redo".to_string()),
            "Redo must disappear after the redo stack is cleared: {labels:?}"
        );
    }

    #[test]
    fn raw_mode_flips_view_toggle_label_to_render() {
        let mut st = state("hello");
        st.mode = Mode::Raw;
        let set = hint_line_for(&st, &keymap());
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert!(
            labels.contains(&"Render"),
            "Raw-mode chord toggles TO Render, got: {labels:?}"
        );
        assert!(!labels.contains(&"Raw"));
    }

    #[test]
    fn link_hint_appears_only_when_cursor_on_link() {
        let mut st = state("a [site](https://example.com) rest");
        st.mode = Mode::Rendered;
        // Cursor in the "site" link text → on a link.
        st.cursor.offset = 5;
        let on_link = hint_line_for(&st, &keymap());
        assert_eq!(
            on_link.chords[0].label, "Open link",
            "contextual link hint must lead the row"
        );
        // Cursor in the trailing plain-text tail → not on a link.
        st.cursor.offset = 32;
        let off_link = hint_line_for(&st, &keymap());
        assert!(
            !off_link.chords.iter().any(|c| c.label == "Open link"),
            "Open link hint leaked outside the link span"
        );
    }

    #[test]
    fn contextual_hints_lead_with_link_before_toggle() {
        // Task item that also contains a link — both contextual
        // chords should be at the front, with Open link first and
        // Toggle second, ahead of every baseline chord.
        let mut st = state("- [ ] see [docs](https://example.com)\n");
        st.mode = Mode::Rendered;
        st.cursor.offset = 14; // inside "docs"
        let set = hint_line_for(&st, &keymap());
        assert_eq!(set.chords[0].label, "Open link");
        assert_eq!(set.chords[1].label, "Toggle");
        assert_eq!(
            set.chords[2].label, "Menu",
            "baseline Menu chord follows the contextual block"
        );
    }

    #[test]
    fn selection_replaces_baseline_with_cut_copy_paste() {
        use crate::document::Selection;
        let mut st = state("hello world");
        st.mode = Mode::Rendered;
        st.selection = Some(Selection {
            anchor: 0,
            active: 5,
        });
        let set = hint_line_for(&st, &keymap());
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Cut", "Copy", "Paste"],
            "active selection must replace the baseline row with Cut/Copy/Paste only"
        );
    }

    #[test]
    fn clearing_selection_restores_baseline_hints() {
        use crate::document::Selection;
        let mut st = state("hello world");
        st.mode = Mode::Rendered;
        st.selection = Some(Selection {
            anchor: 0,
            active: 5,
        });
        assert_eq!(
            hint_line_for(&st, &keymap())
                .chords
                .iter()
                .map(|c| c.label.clone())
                .collect::<Vec<_>>(),
            vec!["Cut", "Copy", "Paste"]
        );
        // Clearing the selection must drop the row back to the
        // baseline edit-mode chords with Menu leading.
        st.selection = None;
        let set = hint_line_for(&st, &keymap());
        assert_eq!(set.chords[0].label, "Menu");
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert!(!labels.contains(&"Cut"));
        assert!(!labels.contains(&"Copy"));
    }

    #[test]
    fn plain_list_item_does_not_show_toggle_chord() {
        // Cursor at byte 2 — inside `- a` (a regular bullet, NOT a task).
        let mut st = state("- a\n- b\n");
        st.mode = Mode::Rendered;
        st.cursor.offset = 2;
        let set = hint_line_for(&st, &keymap());
        assert!(
            !set.chords.iter().any(|c| c.label == "Toggle"),
            "regular list items have no checkbox to toggle"
        );
    }

    #[test]
    fn task_list_item_shows_toggle_chord_first() {
        // Cursor inside a task-list item: `- [ ] todo`.
        let mut st = state("- [ ] todo\n");
        st.mode = Mode::Rendered;
        st.cursor.offset = 8;
        let set = hint_line_for(&st, &keymap());
        assert_eq!(set.chords[0].chord, "^Space");
        assert_eq!(set.chords[0].label, "Toggle");
    }

    #[test]
    fn raw_mode_suppresses_table_hints() {
        let source = "| a | b |\n| - | - |\n| c | d |\n";
        let mut st = state(source);
        st.mode = Mode::Raw;
        st.cursor.offset = 22;
        let set = hint_line_for(&st, &keymap());
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert!(
            !labels.iter().any(|l| l.contains("cell")),
            "raw mode shows baseline edit hints, not table hints: {labels:?}"
        );
        assert!(labels.contains(&"Save"));
    }

    #[test]
    fn rebinding_action_updates_chord_in_hint_line() {
        // Move ShowCommandPalette off Ctrl-P onto F1 via the same
        // `KeyMap::rebind` call the keybinds overlay uses — which
        // (unlike the load-time merge in `KeyMap::build`) drops the
        // prior key for the action.  The Menu chord must follow.
        let mut km = keymap();
        let mut overrides = KeyBindingOverrides::default();
        km.rebind(&Action::ShowCommandPalette, "f1", &mut overrides)
            .unwrap();
        let mut st = state("hello");
        st.mode = Mode::Rendered;
        let set = hint_line_for(&st, &km);
        let menu = set
            .chords
            .iter()
            .find(|c| c.label == "Menu")
            .expect("Menu hint must still appear after rebind");
        assert_eq!(
            menu.chord, "F1",
            "hint chord must reflect the live binding, got: {menu:?}"
        );
        assert!(
            !set.chords.iter().any(|c| c.chord == "^P"),
            "stale ^P chord leaked into hint row: {:?}",
            set.chords
        );
    }

    #[test]
    fn unbinding_an_action_drops_its_chord_from_the_row() {
        // Steal Save's `Ctrl-S` slot by rebinding Quit onto it; the
        // override hands Ctrl-S to Quit and leaves Save without a
        // binding.  The Save chord must vanish from the hint row
        // entirely (not render as a blank chord).
        let mut overrides = KeyBindingOverrides::default();
        overrides.0.insert("Quit".into(), "ctrl+s".into());
        let km = KeyMap::build(&overrides).unwrap();
        assert!(
            km.first_key_event_for(&Action::Save).is_none(),
            "test premise: Save must be orphaned by the rebind"
        );
        let mut st = state("hello");
        st.mode = Mode::Rendered;
        let set = hint_line_for(&st, &km);
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert!(
            !labels.contains(&"Save"),
            "unbound Save must drop from the hint row, got: {labels:?}"
        );
    }

    #[test]
    fn arrow_bundle_falls_back_to_slash_list_when_modifiers_diverge() {
        // Rebind one of the four `Move row/col` arrow actions through
        // the in-app rebind path so the prior arrow binding is
        // dropped — the bundle can no longer collapse to `⌥↑↓←→`,
        // and the badge must list the four bound chords joined by
        // `/`.
        let mut km = keymap();
        let mut overrides = KeyBindingOverrides::default();
        km.rebind(&Action::TableMoveRowUp, "ctrl+shift+u", &mut overrides)
            .unwrap();
        let source = "| a | b |\n| - | - |\n| c | d |\n";
        let mut st = state(source);
        st.mode = Mode::Rendered;
        st.cursor.offset = 22;
        let set = hint_line_for(&st, &km);
        let move_chord = set
            .chords
            .iter()
            .find(|c| c.label == "Move row/col")
            .expect("Move row/col bundle must remain");
        assert!(
            move_chord.chord.contains('/'),
            "fallback bundle must slash-join individual chords, got: {move_chord:?}"
        );
        assert!(
            move_chord.chord.contains("^⇧U"),
            "rebound chord must appear in the bundle, got: {move_chord:?}"
        );
    }

    #[test]
    fn rendered_table_cursor_shows_table_chords() {
        let source = "| a | b |\n| - | - |\n| c | d |\n";
        let mut st = state(source);
        st.mode = Mode::Rendered;
        st.cursor.offset = 22;
        let set = hint_line_for(&st, &keymap());
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.iter().any(|l| l.contains("cell")));
        assert!(labels.iter().any(|l| l.contains("row")));
    }

    // ── lay_out_chords ────────────────────────────────────────────

    #[test]
    fn lay_out_always_includes_labels() {
        let chords = vec![
            HintChord::new("^A", "Alpha"),
            HintChord::new("^B", "Bravo"),
            HintChord::new("^C", "Charlie"),
        ];
        let spans = lay_out_chords(&chords, theme(), theme().hint_bar);
        let concat: String = spans.iter().map(|s| s.content.to_string()).collect();
        assert!(concat.contains("Alpha"));
        assert!(concat.contains("Bravo"));
        assert!(concat.contains("Charlie"));
    }

    // ── BottomRegion rendering ────────────────────────────────────

    fn render_region(
        width: u16,
        layout: StatusBarLayout,
        hint: HintContent,
        sel: Option<(usize, usize)>,
    ) -> String {
        let t = theme();
        let height = BottomRegion::height(layout);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let region = BottomRegion {
                    status: StatusBarState {
                        mode: Mode::Rendered,
                        filename: "test.md",
                        line_count: 3,
                        modified: false,
                        scroll: 0,
                        cursor_line: Some(1),
                        cursor_col: Some(1),
                        selection_size: sel,
                        section_path: Vec::new(),
                        diff_progress: None,
                    },
                    hint,
                    layout,
                    theme: t,
                };
                frame.render_widget(region, frame.area());
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                let sym = buf
                    .cell((x, y))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '));
                out.push(sym);
            }
            out.push('\n');
        }
        out
    }

    fn chord_set(chords: Vec<HintChord>) -> HintSet {
        HintSet {
            prelude: None,
            chords,
        }
    }

    #[test]
    fn two_line_mode_renders_both_rows() {
        let set = chord_set(vec![HintChord::new("^S", "Save")]);
        let out = render_region(40, StatusBarLayout::TwoLine, HintContent::Chords(set), None);
        assert!(out.contains("Save"), "out: {out}");
        assert!(out.contains("test.md"), "out: {out}");
    }

    #[test]
    fn compact_mode_omits_hint_line() {
        let set = chord_set(vec![HintChord::new("^S", "Save")]);
        let out = render_region(40, StatusBarLayout::Compact, HintContent::Chords(set), None);
        assert!(!out.contains("Save"), "out: {out}");
        assert!(out.contains("test.md"), "out: {out}");
    }

    #[test]
    fn transient_overlay_replaces_chords() {
        let out = render_region(
            40,
            StatusBarLayout::TwoLine,
            HintContent::Transient {
                text: "Copied".to_owned(),
                style: Theme::default().transient_info,
            },
            None,
        );
        assert!(out.contains("Copied"), "out: {out}");
        // Chords shouldn't bleed through.
        assert!(!out.contains("Save"), "out: {out}");
    }

    #[test]
    fn prompt_shows_prompt_text_before_chords() {
        let chords = vec![HintChord::new("R", "Reload"), HintChord::new("I", "Ignore")];
        let out = render_region(
            60,
            StatusBarLayout::TwoLine,
            HintContent::Prompt {
                prompt: "File changed on disk.".to_owned(),
                chords,
            },
            None,
        );
        assert!(out.contains("File changed"), "out: {out}");
        assert!(out.contains("Reload"), "out: {out}");
    }

    #[test]
    fn selection_size_renders_in_two_line() {
        let set = chord_set(vec![HintChord::new("^S", "Save")]);
        let out = render_region(
            80,
            StatusBarLayout::TwoLine,
            HintContent::Chords(set),
            Some((42, 3)),
        );
        assert!(out.contains("Sel 42 ch"), "out: {out}");
        assert!(out.contains("3 ln"), "out: {out}");
    }

    #[test]
    fn prelude_appears_before_chords() {
        let set = HintSet {
            prelude: Some("Press any key to edit".to_owned()),
            chords: vec![HintChord::new("^P", "Menu")],
        };
        let out = render_region(80, StatusBarLayout::TwoLine, HintContent::Chords(set), None);
        let first_line = out.lines().next().unwrap();
        let prelude_idx = first_line.find("Press any key").unwrap();
        let menu_idx = first_line.find("^P").unwrap();
        assert!(prelude_idx < menu_idx, "prelude must precede chords");
    }
}
