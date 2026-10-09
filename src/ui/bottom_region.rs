//! Bottom status region: the contextual [`HintLine`] over the persistent [`StatusBar`].  The hint
//! line adapts to the cursor's context and can be overlaid by a transient message or a modal
//! prompt.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::config::keymap::format_key_compact;
use crate::config::{Action, KeyMap, Theme};
use crate::diff::Decision;
use crate::editor::{EditorState, Mode};

use super::status_bar::{StatusBar, StatusBarState};

/// A keybind chord + label pair (e.g. `^C` + `Copy`).  See [`lay_out_chords`] for the rendering.
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

/// A default-hint payload: an optional plaintext prelude followed by a chord row.  `search_match`
/// is an active search's `(current, total)` counter, rendered as an accent badge ahead of both.
#[derive(Debug, Clone, Default)]
pub struct HintSet {
    pub prelude: Option<String>,
    pub chords: Vec<HintChord>,
    pub search_match: Option<(usize, usize)>,
}

/// What the hint line displays; the variants are mutually exclusive, each replacing the ones
/// above it.  All own their strings so the hint can be built up front and passed by value into
/// [`EditorView`](crate::ui::editor_view::EditorView) without entangling its `&mut self.editor`.
#[derive(Debug, Clone)]
pub enum HintContent {
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
    /// Vim command line (`/` `?` `:`): a prefix glyph plus the typed text with a block cursor at
    /// char index `cursor`.  `cursor_visible` is the blink phase.
    CommandLine {
        prefix: char,
        text: String,
        cursor: usize,
        cursor_visible: bool,
    },
}

/// The UI-layer facts [`hint_line_for`] needs that aren't readable off `EditorState`.  A named
/// struct rather than positional `bool`s so a call site can't transpose them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HintCtx {
    /// True when the `App`'s nav history holds a back or forward entry.
    pub nav_available: bool,
    /// True in vim's VisualLine sub-mode.  A single-line V-LINE selection is charwise-empty
    /// (`anchor == active`) yet paints the whole line, so `selection_size()` can't detect it.
    pub visual_line: bool,
    /// True while the vim handler is active.  Vim consumes `Esc` in every sub-mode and never rests
    /// in Preview, so the `ExitToPreview` chord is dropped from the baseline row.
    pub vim_enabled: bool,
}

/// Pick the default hint set for `state`, adapting to the cursor's Markdown context.  Pure, so it
/// is unit-testable without a terminal.
///
/// Chord glyphs are always looked up from `keymap` — never hardcoded — so a rebind shows on the
/// next frame, and an unbound action's chord silently drops from the row.
///
/// Priority: active search > table (Rendered only) > task-list item > mode-default.  Tables are
/// absent in Raw because the table-editing chords don't act on the raw source.
///
/// Vim reuses this row unchanged: its modal keys are vim-internal and the status bar already shows
/// the sub-mode.  A vim Visual selection sets the editor `selection` like a mouse drag and so
/// lands on the shared selection row; VisualLine gets [`visual_line_chords`].
pub fn hint_line_for(state: &EditorState, keymap: &KeyMap, ctx: HintCtx) -> HintSet {
    // An active search replaces the row wholesale in every view mode: only the flow keys work.
    if let Some(search) = state.search.as_ref() {
        let mut chords = search_flow_chords(search.is_replace_flow());
        // Undo/redo ride just ahead of `Esc Exit` so a mis-replace is visibly recoverable without
        // leaving the flow.  Redo is `can_redo`-gated, as on the baseline row.
        if search.is_replace_flow() {
            let exit_idx = chords.len().saturating_sub(1);
            if state.history.can_redo() {
                if let Some(c) = chord_for(keymap, &Action::Redo, "Redo") {
                    chords.insert(exit_idx, c);
                }
            }
            if let Some(c) = chord_for(keymap, &Action::Undo, "Undo") {
                chords.insert(exit_idx, c);
            }
        }
        return HintSet {
            prelude: None,
            chords,
            // Absent when the result set is empty, e.g. after a replace-all consumed every match.
            search_match: (!search.matches.is_empty())
                .then(|| (search.focused_idx + 1, search.matches.len())),
        };
    }
    match state.mode {
        Mode::Preview => {
            let mut chords = chords_from(
                keymap,
                &[
                    (Action::ShowCommandPalette, "Menu"),
                    (Action::GoToSection, "Go to"),
                    (Action::OpenSearch, "Find"),
                    (Action::Copy, "Copy"),
                    (Action::Quit, "Quit"),
                ],
            );
            // Preview is browse mode, so history navigation leads the row.  Suppressed inside a
            // table, where `Alt+Left/Right` reorder columns instead (the nav redirect in
            // `app::actions` fires only outside one) — reachable here because the cursor offset
            // persists into Preview.
            if ctx.nav_available && !cursor_in_table(state) {
                chords.insert(0, nav_chord());
            }
            HintSet {
                // A read-only document rests in Preview permanently, so the invitation to edit
                // would advertise the one transition the mode refuses.  The chords are identical.
                prelude: (!state.readonly).then(|| "Press any key to edit".to_owned()),
                chords,
                search_match: None,
            }
        }
        // `selection_size` can't see a single-line V-LINE selection (see [`HintCtx::visual_line`]).
        // The `is_some` conjunct keeps the row self-consistent rather than trusting the App-layer
        // invariant: an advertised Cut with nothing selected would be a dead chord.
        Mode::Rendered | Mode::Raw if ctx.visual_line && state.selection.is_some() => HintSet {
            prelude: None,
            chords: visual_line_chords(keymap),
            search_match: None,
        },
        Mode::Rendered | Mode::Raw if state.selection_size().is_some() => HintSet {
            prelude: None,
            chords: chords_from(
                keymap,
                &[
                    (Action::Cut, "Cut"),
                    (Action::Copy, "Copy"),
                    (Action::Paste, "Paste"),
                    // Bold / italic wrap a selection, so they ride this row rather than baseline.
                    (Action::BoldSelection, "Bold"),
                    (Action::ItalicizeSelection, "Italic"),
                ],
            ),
            search_match: None,
        },
        Mode::Rendered if cursor_in_table(state) => HintSet {
            prelude: None,
            chords: table_chords(keymap),
            search_match: None,
        },
        Mode::Diff => {
            let read_only = state.diff.as_ref().is_some_and(|d| d.read_only);
            let all_resolved = state.diff.as_ref().is_some_and(|d| d.all_resolved());
            let focused_resolved = state.diff.as_ref().is_some_and(|d| {
                d.focused_decision()
                    .is_some_and(|dec| dec != Decision::Pending)
            });
            HintSet {
                prelude: None,
                chords: diff_review_chords(keymap, all_resolved, focused_resolved, read_only),
                search_match: None,
            }
        }
        Mode::Rendered | Mode::Raw => {
            // Baseline edit-mode row, anchored by Menu as the discovery entry.  Cut / Copy are
            // absent: they need a selection, which the arm above handles.  "Preview" / "Raw" /
            // "Render" are destination labels, never the current state.
            let view_toggle_label = match state.mode {
                Mode::Raw => "Render",
                _ => "Raw",
            };
            let redo_entry = state.history.can_redo().then_some((Action::Redo, "Redo"));
            let preview_entry = (!ctx.vim_enabled).then_some((Action::ExitToPreview, "Preview"));
            let baseline = [
                Some((Action::ShowCommandPalette, "Menu")),
                Some((Action::GoToSection, "Go to")),
                Some((Action::OpenSearch, "Find")),
                Some((Action::Paste, "Paste")),
                Some((Action::Undo, "Undo")),
                redo_entry,
                Some((Action::Open, "Open")),
                Some((Action::Save, "Save")),
                preview_entry,
                Some((Action::ToggleRawMode, view_toggle_label)),
                Some((Action::Quit, "Quit")),
            ];
            let entries: Vec<(Action, &str)> = baseline.into_iter().flatten().collect();
            let mut chords = chords_from(keymap, &entries);
            // Each `insert(0, ..)` pushes the previous head back, so these run in REVERSE of the
            // desired visual order: Link, Toggle, Back/fwd — narrowest trigger leftmost.  The
            // `!cursor_in_table` guard matters for Raw, which has no early-returning table arm
            // above; there `Alt+Left/Right` reorder columns rather than navigating history.
            if ctx.nav_available && !cursor_in_table(state) {
                chords.insert(0, nav_chord());
            }
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
                search_match: None,
            }
        }
    }
}

/// The combined back/forward hint.  The glyph is fixed rather than keymap-derived because
/// `NavigateBack` / `NavigateForward` have no binding of their own: `App::normalize_context_action`
/// redirects `Alt+Left/Right` from the table column actions when outside a table.
fn nav_chord() -> HintChord {
    HintChord::new("⌥←→", "Back/fwd")
}

/// Pair `action`'s first bound key with `label`; `None` when unbound, so the row drops it.
fn chord_for(keymap: &KeyMap, action: &Action, label: &str) -> Option<HintChord> {
    let ev = keymap.first_key_event_for(action)?;
    Some(HintChord::new(format_key_compact(&ev), label.to_owned()))
}

/// [`chord_for`] over a slice, collecting the successful lookups in order.
fn chords_from(keymap: &KeyMap, entries: &[(Action, &str)]) -> Vec<HintChord> {
    entries
        .iter()
        .filter_map(|(action, label)| chord_for(keymap, action, label))
        .collect()
}

/// Diff Review hint row.  Glyphs come from the shared `diff_keys` table (via
/// [`crate::input::diff_hint`]), the same source the input handler and overlays read, so the
/// advertised chord can never disagree with the key that fires.
///
/// `Quit` alone comes from `keymap`: it is the rebindable global chord (honored in diff mode via
/// `diff_safe_action`), not a review binding, so it has no glyph in the diff table.
///
/// `Esc Exit` trails the row and appears only once every hunk is resolved, because `Esc` can't
/// exit diff mode before then.
fn diff_review_chords(
    keymap: &KeyMap,
    all_resolved: bool,
    focused_resolved: bool,
    read_only: bool,
) -> Vec<HintChord> {
    let mk = |action: &Action, label: &str| {
        HintChord::new(crate::input::diff_hint(action), label.to_owned())
    };
    // A read-only review (`--diff`) refuses every decision action, so the row is navigation and
    // exit only, and `Esc` is unconditional — nothing can resolve, so the gate below would hide
    // the one way out.  `Quit` rides this row alone because in a difftool walk the two exits
    // differ: `Esc` finishes this file, `Quit` ends the whole walk (`app::difftool::stop_walk`).
    if read_only {
        let mut chords = vec![
            mk(&Action::DiffNext, "Next"),
            mk(&Action::DiffPrev, "Prev"),
            mk(&Action::DiffExit, "Close file"),
        ];
        chords.extend(chords_from(keymap, &[(Action::Quit, "Quit diff")]));
        return chords;
    }
    let mut chords = vec![
        mk(&Action::DiffNext, "Next"),
        mk(&Action::DiffPrev, "Prev"),
        mk(&Action::DiffAcceptHunk, "Accept"),
        mk(&Action::DiffRejectHunk, "Reject"),
        mk(&Action::DiffAcceptAll, "Accept all"),
        mk(&Action::DiffRejectAll, "Reject all"),
    ];
    // Reset is a no-op on a still-`Pending` hunk.
    if focused_resolved {
        chords.push(mk(&Action::DiffResetHunk, "Reset"));
    }
    if all_resolved {
        chords.push(mk(&Action::DiffExit, "Exit"));
    }
    chords
}

/// Search-flow hint row.  Glyphs come from the shared `search::search_keys` table (via
/// [`crate::search::search_hint`]), so the advertised chord always matches the one that fires.
fn search_flow_chords(is_replace: bool) -> Vec<HintChord> {
    let mk = |action: &Action, label: &str| {
        HintChord::new(crate::search::search_hint(action), label.to_owned())
    };
    let mut chords = vec![
        mk(&Action::SearchNext, "Next"),
        mk(&Action::SearchPrev, "Prev"),
    ];
    if is_replace {
        chords.push(mk(&Action::SearchReplace, "Replace"));
        chords.push(mk(&Action::SearchReplaceAll, "Replace all"));
    }
    chords.push(mk(&Action::SearchExit, "Exit"));
    chords
}

/// The vim VisualLine hint row: the three clipboard chords the App widens to whole lines
/// (`App::dispatch_visual_line_clipboard`).
///
/// Shorter than the charwise row on purpose: `toggle_wrap` bails on both shapes a V-LINE selection
/// takes — an empty charwise span, or one containing a newline — so Bold / Italic would be
/// advertised no-ops.
fn visual_line_chords(keymap: &KeyMap) -> Vec<HintChord> {
    chords_from(
        keymap,
        &[
            (Action::Cut, "Cut"),
            (Action::Copy, "Copy"),
            (Action::Paste, "Paste"),
        ],
    )
}

/// The table-context hint row; see [`arrow_bundle_chord`] for the collapsed arrow badges.
fn table_chords(keymap: &KeyMap) -> Vec<HintChord> {
    let mut out: Vec<HintChord> = Vec::new();
    // Next-cell is a context dispatch from `InsertTab` in `edit_ops`, so look up that action to
    // stay truthful if the user rebinds Tab.
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

/// One chord glyph for an arrow-driven bundle (`⌥↑↓←→`).  Collapses only when all four chords
/// share modifiers *and* each maps to its expected arrow; otherwise slash-joins the compact
/// chords.  `None` when none of the four is bound.
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

/// Lay out a chord list into spans: `{chord}` in `hint_chord`, ` {label}` in `hint_label`, then a
/// two-space `bar_style` separator.  Labels are never dropped under width pressure — a bare badge
/// isn't useful — so a narrow row simply truncates in the non-wrapping `Paragraph`.
///
/// `bar_style` is the active mode's hint-bar background, [`Theme::hint_bar_diff`] in diff mode.
pub fn lay_out_chords(chords: &[HintChord], theme: &Theme, bar_style: Style) -> Vec<Span<'static>> {
    // Wash the badge and label backgrounds with the bar's bg so the row reads as one bar.  A no-op
    // outside diff mode, where all three slots already share `surface_elevated`.
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

/// Spans for a vim command line `width` cells wide: a leading ` {prefix}` glyph then the typed
/// text with the unified block cursor at char index `cursor`.  The cursor is one blink-stable
/// cell (a space past end-of-line), so an empty `/` still reserves it and the row never jitters
/// on blink; a line wider than the row scrolls (`scroll`) to keep the cursor on screen.
#[allow(clippy::too_many_arguments)]
fn command_line_spans(
    prefix: char,
    text: &str,
    cursor: usize,
    scroll: &mut usize,
    width: usize,
    cursor_visible: bool,
    theme: &Theme,
    bar_style: Style,
) -> Vec<Span<'static>> {
    let base = match bar_style.bg {
        Some(bg) => theme.hint_label.bg(bg),
        None => theme.hint_label,
    };
    let lead = format!(" {prefix}");
    let field_w = width.saturating_sub(Span::raw(lead.as_str()).width());
    let mut spans = vec![Span::styled(lead, base)];
    spans.extend(crate::ui::cursor::scrolled_field_spans(
        text,
        cursor,
        scroll,
        field_w,
        cursor_visible,
        base,
        theme.cursor,
    ));
    spans
}

/// Hint-line state that must outlive a frame: the vim command line's horizontal scroll, so the
/// window moves only as far as the cursor forces it.  Owned by
/// [`EditorViewState`](crate::ui::editor_view::EditorViewState).
#[derive(Debug, Default)]
pub struct HintLineState {
    cmdline_scroll: usize,
}

/// The hint-line widget: one row of chords / transient / prompt with a trailing `bar_style` fill.
pub struct HintLine<'a> {
    pub content: HintContent,
    pub theme: &'a Theme,
    /// Bar fill and inter-chord separator background; [`Theme::hint_bar_diff`] in diff mode.
    pub bar_style: Style,
}

impl<'a> StatefulWidget for HintLine<'a> {
    type State = HintLineState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let width = area.width as usize;

        let spans: Vec<Span<'_>> = match &self.content {
            HintContent::Chords(set) => {
                let mut v: Vec<Span<'_>> = Vec::new();
                // The ` n/N ` badge leads the line; a trailing bar-styled space makes its gap
                // match the inter-chord one.
                if let Some((current, total)) = set.search_match {
                    v.push(Span::styled(
                        format!(" {}/{} ", current, total),
                        self.theme.status_mode_search,
                    ));
                    v.push(Span::styled(" ".to_string(), self.bar_style));
                }
                // `hint_label` fg on the bar bg, so the prelude reads as a sentence, not a chord.
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
            HintContent::CommandLine {
                prefix,
                text,
                cursor,
                cursor_visible,
            } => command_line_spans(
                *prefix,
                text,
                *cursor,
                &mut state.cmdline_scroll,
                width,
                *cursor_visible,
                self.theme,
                self.bar_style,
            ),
        };

        // Pad the trailing fill with the bar background, else the terminal's own shows through.
        let used: usize = spans.iter().map(Span::width).sum();
        let mut all_spans = spans;
        if used < width {
            all_spans.push(Span::styled(" ".repeat(width - used), self.bar_style));
        }

        Paragraph::new(Line::from(all_spans))
            .style(self.bar_style)
            .render(area, buf);
    }
}

/// Composite widget: a [`HintLine`] above a persistent [`StatusBar`].
pub struct BottomRegion<'a> {
    pub status: StatusBarState<'a>,
    pub hint: HintContent,
    pub theme: &'a Theme,
}

impl<'a> BottomRegion<'a> {
    /// Rows this region needs; `EditorView` partitions the terminal area with it.
    pub fn height() -> u16 {
        2
    }
}

impl<'a> StatefulWidget for BottomRegion<'a> {
    type State = HintLineState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.height == 0 {
            return;
        }
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);
        // Diff mode recolors the hint bar to match the status bar's mode shift.
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
        .render(chunks[0], buf, state);
        StatusBar {
            state: self.status,
            theme: self.theme,
        }
        .render(chunks[1], buf);
    }
}

/// True when the cursor sits inside a Markdown table.  Mirrors the App-internal helper so pure
/// hint-line code needn't reach into app state.
fn cursor_in_table(state: &EditorState) -> bool {
    // A read-only document has no column to reorder (`readonly_safe_action` denies every table
    // command), so `Alt+Left/Right` must stay the Back/Forward chord even when a section jump
    // parks the cursor in one of `keybindings.md`'s many tables.
    if state.readonly {
        return false;
    }
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    crate::editor::table_edit_ops::locate_table(state, cursor_byte).is_some()
}

/// True when the cursor is on a *task* list item; plain bullets have no checkbox to toggle.
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

/// True when the cursor sits inside a `[text](url)` link.  Uses the same `link_at_offset` scan as
/// `mouse_ops` and `App::resolve_link_at_cursor`, so the hint and the dispatch agree.
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
        let set = hint_line_for(&st, &keymap(), HintCtx::default());
        assert_eq!(set.prelude.as_deref(), Some("Press any key to edit"));
        assert_eq!(set.chords[0].chord, "^P");
        assert_eq!(set.chords[0].label, "Menu");
        assert!(set.chords.iter().any(|c| c.label == "Quit"));
    }

    /// A read-only document differs from Preview in one way: no "Press any key to edit".
    #[test]
    fn the_read_only_row_drops_only_the_edit_invitation() {
        let mut st = state("hello");
        let editable = hint_line_for(&st, &keymap(), HintCtx::default());
        st.readonly = true;
        let set = hint_line_for(&st, &keymap(), HintCtx::default());

        assert_eq!(editable.prelude.as_deref(), Some("Press any key to edit"));
        assert_eq!(set.prelude, None);
        let labels =
            |s: &HintSet| -> Vec<String> { s.chords.iter().map(|c| c.label.clone()).collect() };
        assert_eq!(labels(&set), labels(&editable));
        // Nothing that writes the document may appear.
        for denied in ["Paste", "Cut", "Save", "Undo", "Redo"] {
            assert!(
                !set.chords.iter().any(|c| c.label == denied),
                "the read-only row must not advertise {denied}"
            );
        }
    }

    /// Regression: the Back chord used to vanish whenever a section jump parked the cursor in one
    /// of `keybindings.md`'s many tables.
    #[test]
    fn the_back_chord_survives_a_table_in_a_read_only_document() {
        let mut st = state("| a | b |\n|---|---|\n| 1 | 2 |\n");
        st.readonly = true;
        st.cursor.offset = 3;
        let ctx = HintCtx {
            nav_available: true,
            ..HintCtx::default()
        };
        let set = hint_line_for(&st, &keymap(), ctx);
        assert_eq!(
            set.chords[0].chord, "⌥←→",
            "Back/fwd must lead the row even inside a table"
        );
    }

    /// `Esc Close file` must appear even with nothing resolved: it is the one way out.
    #[test]
    fn a_read_only_diff_row_offers_only_navigation_and_the_two_exits() {
        let labels: Vec<String> = diff_review_chords(&keymap(), false, false, true)
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert_eq!(labels, vec!["Next", "Prev", "Close file", "Quit diff"]);
    }

    /// In a difftool walk `Esc` advances to the next file and `Quit` stops it, so the two exits
    /// need distinct chords.
    #[test]
    fn a_read_only_diff_row_names_a_distinct_chord_for_each_exit() {
        let row = diff_review_chords(&keymap(), false, false, true);
        let exit = row
            .iter()
            .find(|c| c.label == "Close file")
            .expect("Esc Close file");
        let stop = row
            .iter()
            .find(|c| c.label == "Quit diff")
            .expect("Quit chord");
        assert_ne!(exit.chord, stop.chord);
        assert!(!stop.chord.is_empty(), "the quit chord must be nameable");
    }

    #[test]
    fn diff_hint_gates_exit_on_full_resolution() {
        let pending = diff_review_chords(&keymap(), false, false, false);
        assert!(
            !pending.iter().any(|c| c.label == "Exit"),
            "Exit hint must be hidden while hunks are pending",
        );
        assert_eq!(pending[0].label, "Next", "Tab/Next leads when pending");

        let resolved = diff_review_chords(&keymap(), true, true, false);
        assert_eq!(resolved[0].label, "Next", "review actions lead the row");
        let last = resolved.last().expect("non-empty row");
        assert_eq!(last.chord, "Esc");
        assert_eq!(last.label, "Exit");
    }

    #[test]
    fn diff_reset_hint_only_when_focused_hunk_resolved() {
        let pending = diff_review_chords(&keymap(), false, false, false);
        assert!(
            !pending.iter().any(|c| c.label == "Reset"),
            "Reset hint must be hidden while the focused hunk is pending",
        );
        let decided = diff_review_chords(&keymap(), false, true, false);
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
        let set = hint_line_for(&st, &keymap(), HintCtx::default());
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(set.chords[0].chord, "^P", "Menu is always first");
        assert!(labels.contains(&"Paste"));
        assert!(labels.contains(&"Save"));
        assert!(
            labels.contains(&"Raw"),
            "Rendered-mode chord toggles TO Raw"
        );
        assert!(labels.contains(&"Quit"));
        assert!(
            !labels.contains(&"Cut"),
            "Cut must stay hidden without an active selection"
        );
        assert!(
            !labels.contains(&"Copy"),
            "Copy must stay hidden without an active selection"
        );
        assert!(
            !labels.contains(&"Bold") && !labels.contains(&"Italic"),
            "Bold/Italic must stay hidden without an active selection"
        );
        assert!(
            !labels.contains(&"Open link"),
            "link hint must stay hidden when the cursor isn't on a link"
        );
        assert!(set.prelude.is_none());
    }

    #[test]
    fn redo_hint_only_appears_when_history_can_redo() {
        use crate::document::history::EditDelta;

        let mut st = state("hello");
        st.mode = Mode::Rendered;

        let labels: Vec<_> = hint_line_for(&st, &keymap(), HintCtx::default())
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

        let labels: Vec<_> = hint_line_for(&st, &keymap(), HintCtx::default())
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
        let labels: Vec<_> = hint_line_for(&st, &keymap(), HintCtx::default())
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
        let set = hint_line_for(&st, &keymap(), HintCtx::default());
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
        st.cursor.offset = 5;
        let on_link = hint_line_for(&st, &keymap(), HintCtx::default());
        assert_eq!(
            on_link.chords[0].label, "Open link",
            "contextual link hint must lead the row"
        );
        st.cursor.offset = 32;
        let off_link = hint_line_for(&st, &keymap(), HintCtx::default());
        assert!(
            !off_link.chords.iter().any(|c| c.label == "Open link"),
            "Open link hint leaked outside the link span"
        );
    }

    #[test]
    fn contextual_hints_lead_with_link_before_toggle() {
        let mut st = state("- [ ] see [docs](https://example.com)\n");
        st.mode = Mode::Rendered;
        st.cursor.offset = 14; // inside "docs"
        let set = hint_line_for(&st, &keymap(), HintCtx::default());
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
        let set = hint_line_for(&st, &keymap(), HintCtx::default());
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Cut", "Copy", "Paste", "Bold", "Italic"],
            "active selection must replace the baseline row with the selection chords only"
        );
    }

    #[test]
    fn visual_line_shows_selection_hints_despite_empty_charwise_span() {
        use crate::document::Selection;
        // A single-line V-LINE leaves the charwise selection empty, so only the flag surfaces it.
        let mut st = state("hello world");
        st.mode = Mode::Rendered;
        st.selection = Some(Selection {
            anchor: 3,
            active: 3,
        });
        assert!(
            st.selection_size().is_none(),
            "an anchor == active selection is charwise-empty"
        );
        let baseline = hint_line_for(&st, &keymap(), HintCtx::default());
        assert_ne!(
            baseline.chords.first().map(|c| c.label.as_str()),
            Some("Cut"),
            "without the V-LINE flag an empty selection shows the baseline row"
        );
        let set = hint_line_for(
            &st,
            &keymap(),
            HintCtx {
                visual_line: true,
                ..Default::default()
            },
        );
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Cut", "Copy", "Paste"],
            "V-LINE must show its clipboard row even with an empty charwise span"
        );
    }

    #[test]
    fn visual_line_row_omits_bold_and_italic() {
        use crate::document::Selection;
        // `toggle_wrap` bails on both V-LINE shapes, so Bold / Italic would be no-ops there.
        let mut st = state("alpha\nbeta\n");
        st.mode = Mode::Rendered;
        st.selection = Some(Selection {
            anchor: 2,
            active: 8,
        });
        let charwise = hint_line_for(&st, &keymap(), HintCtx::default());
        let charwise_labels: Vec<_> = charwise.chords.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            charwise_labels,
            vec!["Cut", "Copy", "Paste", "Bold", "Italic"]
        );
        let v_line = hint_line_for(
            &st,
            &keymap(),
            HintCtx {
                visual_line: true,
                ..Default::default()
            },
        );
        let v_line_labels: Vec<_> = v_line.chords.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            v_line_labels,
            vec!["Cut", "Copy", "Paste"],
            "V-LINE must not advertise the wrap chords it can't run"
        );
    }

    #[test]
    fn visual_line_flag_without_a_selection_falls_through() {
        // A desynced sub-mode must not advertise a Cut with nothing to cut.
        let mut st = state("hello world");
        st.mode = Mode::Rendered;
        assert!(st.selection.is_none(), "test premise");
        let set = hint_line_for(
            &st,
            &keymap(),
            HintCtx {
                visual_line: true,
                ..Default::default()
            },
        );
        assert_eq!(
            set.chords[0].label, "Menu",
            "no selection → the baseline row, whatever the sub-mode"
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
            hint_line_for(&st, &keymap(), HintCtx::default())
                .chords
                .iter()
                .map(|c| c.label.clone())
                .collect::<Vec<_>>(),
            vec!["Cut", "Copy", "Paste", "Bold", "Italic"]
        );
        st.selection = None;
        let set = hint_line_for(&st, &keymap(), HintCtx::default());
        assert_eq!(set.chords[0].label, "Menu");
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert!(!labels.contains(&"Cut"));
        assert!(!labels.contains(&"Copy"));
    }

    #[test]
    fn plain_list_item_does_not_show_toggle_chord() {
        let mut st = state("- a\n- b\n");
        st.mode = Mode::Rendered;
        st.cursor.offset = 2;
        let set = hint_line_for(&st, &keymap(), HintCtx::default());
        assert!(
            !set.chords.iter().any(|c| c.label == "Toggle"),
            "regular list items have no checkbox to toggle"
        );
    }

    #[test]
    fn task_list_item_shows_toggle_chord_first() {
        let mut st = state("- [ ] todo\n");
        st.mode = Mode::Rendered;
        st.cursor.offset = 8;
        let set = hint_line_for(&st, &keymap(), HintCtx::default());
        assert_eq!(set.chords[0].chord, "^Space");
        assert_eq!(set.chords[0].label, "Toggle");
    }

    /// The table hints show in any table the parse finds, whatever its rows' pipes or
    /// container, and not in prose holding a `|`.
    #[test]
    fn table_hints_show_in_tables_without_edge_pipes_and_nested_ones() {
        let has_cell_hint = |source: &str, at: &str| {
            let mut st = state(source);
            st.mode = Mode::Rendered;
            st.cursor.offset = source[..source.find(at).unwrap()].chars().count();
            let set = hint_line_for(&st, &keymap(), HintCtx::default());
            set.chords.iter().any(|c| c.label.contains("cell"))
        };
        assert!(has_cell_hint("a | b\n--|--\n1 | 2\n", "2"));
        assert!(has_cell_hint("> a | b\n> --|--\n> 1 | 2\n", "2"));
        assert!(has_cell_hint("| a | b |\n|---|---|\n| 1\n", "1"));
        assert!(!has_cell_hint("a | b is prose\n", "b"));
    }

    #[test]
    fn raw_mode_suppresses_table_hints() {
        let source = "| a | b |\n| - | - |\n| c | d |\n";
        let mut st = state(source);
        st.mode = Mode::Raw;
        st.cursor.offset = 22;
        let set = hint_line_for(&st, &keymap(), HintCtx::default());
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert!(
            !labels.iter().any(|l| l.contains("cell")),
            "raw mode shows baseline edit hints, not table hints: {labels:?}"
        );
        assert!(labels.contains(&"Save"));
    }

    #[test]
    fn rebinding_action_updates_chord_in_hint_line() {
        // `KeyMap::rebind` (the overlay's path) drops the action's prior key, unlike the load-time
        // merge in `KeyMap::build`.
        let mut km = keymap();
        let mut overrides = KeyBindingOverrides::default();
        km.rebind(&Action::ShowCommandPalette, "f1", &mut overrides)
            .unwrap();
        let mut st = state("hello");
        st.mode = Mode::Rendered;
        let set = hint_line_for(&st, &km, HintCtx::default());
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
        // Rebinding Quit onto Ctrl-S orphans Save, whose chord must vanish rather than blank out.
        let mut overrides = KeyBindingOverrides::default();
        overrides.0.insert("Quit".into(), "ctrl+s".into());
        let km = KeyMap::build(&overrides).unwrap();
        assert!(
            km.first_key_event_for(&Action::Save).is_none(),
            "test premise: Save must be orphaned by the rebind"
        );
        let mut st = state("hello");
        st.mode = Mode::Rendered;
        let set = hint_line_for(&st, &km, HintCtx::default());
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert!(
            !labels.contains(&"Save"),
            "unbound Save must drop from the hint row, got: {labels:?}"
        );
    }

    #[test]
    fn arrow_bundle_falls_back_to_slash_list_when_modifiers_diverge() {
        // Rebinding one of the four drops its arrow binding, so the bundle can no longer collapse.
        let mut km = keymap();
        let mut overrides = KeyBindingOverrides::default();
        km.rebind(&Action::TableMoveRowUp, "ctrl+shift+u", &mut overrides)
            .unwrap();
        let source = "| a | b |\n| - | - |\n| c | d |\n";
        let mut st = state(source);
        st.mode = Mode::Rendered;
        st.cursor.offset = 22;
        let set = hint_line_for(&st, &km, HintCtx::default());
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
    fn nav_hint_appears_only_when_history_available() {
        let mut st = state("hello");
        st.mode = Mode::Rendered;
        let off = hint_line_for(&st, &keymap(), HintCtx::default());
        assert!(
            !off.chords.iter().any(|c| c.label == "Back/fwd"),
            "Back/fwd must stay hidden with an empty history stack"
        );
        let on = hint_line_for(
            &st,
            &keymap(),
            HintCtx {
                nav_available: true,
                ..Default::default()
            },
        );
        let nav = on
            .chords
            .iter()
            .position(|c| c.label == "Back/fwd")
            .expect("Back/fwd hint must appear when history is available");
        assert_eq!(on.chords[nav].chord, "⌥←→");
        let menu = on
            .chords
            .iter()
            .position(|c| c.label == "Menu")
            .expect("Menu still present");
        assert_eq!(nav + 1, menu, "Back/fwd must sit immediately before Menu");
    }

    #[test]
    fn nav_hint_suppressed_in_table() {
        let source = "| a | b |\n| - | - |\n| c | d |\n";
        let mut st = state(source);
        st.mode = Mode::Rendered;
        st.cursor.offset = 22;
        let set = hint_line_for(
            &st,
            &keymap(),
            HintCtx {
                nav_available: true,
                ..Default::default()
            },
        );
        assert!(
            !set.chords.iter().any(|c| c.label == "Back/fwd"),
            "Back/fwd must stay hidden while the cursor is in a table"
        );
    }

    #[test]
    fn nav_hint_suppressed_in_table_raw_mode() {
        // Raw has no early-returning table arm, so suppression rests entirely on the explicit
        // `!cursor_in_table` guard.
        let source = "| a | b |\n| - | - |\n| c | d |\n";
        let mut st = state(source);
        st.mode = Mode::Raw;
        st.cursor.offset = 22;
        let set = hint_line_for(
            &st,
            &keymap(),
            HintCtx {
                nav_available: true,
                ..Default::default()
            },
        );
        assert!(
            !set.chords.iter().any(|c| c.label == "Back/fwd"),
            "Back/fwd must stay hidden in a table even in Raw mode"
        );
    }

    #[test]
    fn preview_chord_hidden_under_vim() {
        for mode in [Mode::Rendered, Mode::Raw] {
            let mut st = state("hello");
            st.mode = mode;
            let default = hint_line_for(&st, &keymap(), HintCtx::default());
            assert!(
                default.chords.iter().any(|c| c.label == "Preview"),
                "{mode:?}: Preview must show under the default handler"
            );
            let vim = hint_line_for(
                &st,
                &keymap(),
                HintCtx {
                    vim_enabled: true,
                    ..Default::default()
                },
            );
            assert!(
                !vim.chords.iter().any(|c| c.label == "Preview"),
                "{mode:?}: Preview must be hidden under vim"
            );
            let without_preview: Vec<&str> = default
                .chords
                .iter()
                .map(|c| c.label.as_str())
                .filter(|l| *l != "Preview")
                .collect();
            let vim_labels: Vec<&str> = vim.chords.iter().map(|c| c.label.as_str()).collect();
            assert_eq!(
                vim_labels, without_preview,
                "{mode:?}: only Preview may differ"
            );
        }
    }

    #[test]
    fn nav_hint_leads_preview_row() {
        let st = state("hello");
        let set = hint_line_for(
            &st,
            &keymap(),
            HintCtx {
                nav_available: true,
                ..Default::default()
            },
        );
        assert_eq!(set.chords[0].label, "Back/fwd");
        assert_eq!(set.chords[0].chord, "⌥←→");
        assert_eq!(
            set.chords[1].label, "Menu",
            "baseline Menu follows the nav hint"
        );
    }

    #[test]
    fn nav_hint_trails_link_and_toggle() {
        let mut st = state("- [ ] see [docs](https://example.com)\n");
        st.mode = Mode::Rendered;
        st.cursor.offset = 14; // inside "docs"
        let set = hint_line_for(
            &st,
            &keymap(),
            HintCtx {
                nav_available: true,
                ..Default::default()
            },
        );
        assert_eq!(set.chords[0].label, "Open link");
        assert_eq!(set.chords[1].label, "Toggle");
        assert_eq!(set.chords[2].label, "Back/fwd");
        assert_eq!(set.chords[3].label, "Menu");
    }

    #[test]
    fn rendered_table_cursor_shows_table_chords() {
        let source = "| a | b |\n| - | - |\n| c | d |\n";
        let mut st = state(source);
        st.mode = Mode::Rendered;
        st.cursor.offset = 22;
        let set = hint_line_for(&st, &keymap(), HintCtx::default());
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.iter().any(|l| l.contains("cell")));
        assert!(labels.iter().any(|l| l.contains("row")));
    }

    // ── active search ─────────────────────────────────────────────

    #[test]
    fn active_search_replaces_the_row() {
        use crate::search::SearchState;
        let mut st = state("foo foo foo");
        st.mode = Mode::Rendered;
        let search = SearchState::new("foo".to_owned(), None).unwrap();
        st.enter_search(search);
        let set = hint_line_for(&st, &keymap(), HintCtx::default());
        assert!(set.search_match.is_some(), "match counter must lead");
        assert!(set.chords.iter().any(|c| c.label == "Next"));
        assert!(
            !set.chords.iter().any(|c| c.label == "Menu"),
            "baseline chords must yield to the active search row"
        );
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

    #[test]
    fn a_long_command_line_scrolls_to_keep_the_cursor_in_view() {
        let t = theme();
        let text = format!("s/start{}/tail", "x".repeat(40));
        let end = text.chars().count();
        let line = |cursor: usize, scroll: &mut usize| -> String {
            command_line_spans(':', &text, cursor, scroll, 20, true, t, t.hint_bar)
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };
        let mut scroll = 0;
        let at_end = line(end, &mut scroll);
        assert_eq!(
            Line::from(at_end.as_str()).width(),
            20,
            "exactly the row width"
        );
        assert!(at_end.starts_with(" :"), "the prefix stays: {at_end:?}");
        assert!(
            at_end.ends_with("/tail "),
            "the cursor end is in view: {at_end:?}"
        );

        // A step left stays inside the window, so the text does not move.
        let first = scroll;
        let _ = line(end - 3, &mut scroll);
        assert_eq!(
            scroll, first,
            "the window moves only when the cursor forces it"
        );
        let at_start = line(0, &mut scroll);
        assert!(at_start.starts_with(" :s/start"), "{at_start:?}");
    }

    fn render_region(width: u16, hint: HintContent) -> String {
        let t = theme();
        let height = BottomRegion::height();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let region = BottomRegion {
                    status: StatusBarState {
                        mode: Mode::Rendered,
                        filename: "test.md",
                        line_count: 3,
                        scroll_total: 3,
                        viewport_rows: 1,
                        modified: false,
                        scroll: 0,
                        cursor_line: Some(1),
                        cursor_col: Some(1),
                        section_path: Vec::new(),
                        diff_progress: None,
                        vim_mode_label: None,
                    },
                    hint,
                    theme: t,
                };
                frame.render_stateful_widget(region, frame.area(), &mut HintLineState::default());
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
            search_match: None,
        }
    }

    #[test]
    fn bottom_region_renders_both_rows() {
        let set = chord_set(vec![HintChord::new("^S", "Save")]);
        let out = render_region(40, HintContent::Chords(set));
        assert!(out.contains("Save"), "out: {out}");
        assert!(out.contains("test.md"), "out: {out}");
    }

    #[test]
    fn transient_overlay_replaces_chords() {
        let out = render_region(
            40,
            HintContent::Transient {
                text: "Copied".to_owned(),
                style: Theme::default().transient_info,
            },
        );
        assert!(out.contains("Copied"), "out: {out}");
        assert!(!out.contains("Save"), "out: {out}");
    }

    #[test]
    fn prompt_shows_prompt_text_before_chords() {
        let chords = vec![HintChord::new("R", "Reload"), HintChord::new("I", "Ignore")];
        let out = render_region(
            60,
            HintContent::Prompt {
                prompt: "File changed on disk.".to_owned(),
                chords,
            },
        );
        assert!(out.contains("File changed"), "out: {out}");
        assert!(out.contains("Reload"), "out: {out}");
    }

    #[test]
    fn search_match_badge_leads_the_hint_line() {
        let set = HintSet {
            prelude: None,
            chords: vec![HintChord::new("⏎", "Next")],
            search_match: Some((2, 3)),
        };
        let out = render_region(80, HintContent::Chords(set));
        let first_line = out.lines().next().unwrap();
        let counter_idx = first_line.find("2/3").expect("match counter must render");
        let next_idx = first_line.find("Next").expect("chord must render");
        assert!(
            counter_idx < next_idx,
            "match counter must lead the hint line, line: {first_line:?}"
        );
    }

    #[test]
    fn no_search_match_badge_without_an_active_search() {
        let set = chord_set(vec![HintChord::new("^S", "Save")]);
        let out = render_region(80, HintContent::Chords(set));
        assert!(
            !out.contains('/'),
            "no counter when search_match is None: {out}"
        );
    }

    #[test]
    fn prelude_appears_before_chords() {
        let set = HintSet {
            prelude: Some("Press any key to edit".to_owned()),
            chords: vec![HintChord::new("^P", "Menu")],
            search_match: None,
        };
        let out = render_region(80, HintContent::Chords(set));
        let first_line = out.lines().next().unwrap();
        let prelude_idx = first_line.find("Press any key").unwrap();
        let menu_idx = first_line.find("^P").unwrap();
        assert!(prelude_idx < menu_idx, "prelude must precede chords");
    }
}
