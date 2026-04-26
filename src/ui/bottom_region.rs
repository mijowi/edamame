//! Phase 9 — bottom status region.
//!
//! Composes the contextual [`HintLine`] on top of the persistent
//! [`StatusBar`].  The hint line adapts to the cursor's context
//! (default / table / list) and can be overlaid by a transient message
//! or preempted by a modal prompt.  In compact mode only the status
//! line renders.

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::config::{StatusBarLayout, Theme};
use crate::editor::{EditorState, Mode};

use super::status_bar::{StatusBar, StatusBarState};

/// A single keybind chord + label pair (e.g. `^C` + `Copy`).  The
/// chord glyph renders in the contrasting `hint_chord` theme slot
/// (nano-style badge), followed by a single space, the label in
/// `hint_label`, and a two-space separator between successive hints.
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

    /// Width in cells when chord *and* label are rendered.  Layout:
    /// ` {chord} ` (2-space padding inside the chord badge) +
    /// `{label}` (no padding) + `  ` (2-space separator to the next
    /// hint).
    fn full_width(&self) -> usize {
        self.chord.chars().count() + 2 + self.label.chars().count() + 2
    }

    /// Width when only the chord badge is shown (label dropped under
    /// width pressure): ` {chord} ` + `  ` separator.
    fn chord_only_width(&self) -> usize {
        self.chord.chars().count() + 2 + 2
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
/// Priority: table (Rendered only) > task-list item > mode-default.
/// Tables don't appear in Raw mode because the table-editing chords
/// don't work against the raw source — the user is editing the plain
/// Markdown and `Tab` / `⌥↑↓` insert characters or do nothing.
pub fn hint_line_for(state: &EditorState) -> HintSet {
    match state.mode {
        Mode::Preview => HintSet {
            prelude: Some("Press any key to edit".to_owned()),
            chords: vec![
                HintChord::new("^P", "Menu"),
                HintChord::new("^C", "Copy"),
                HintChord::new("^Q", "Quit"),
            ],
        },
        Mode::Rendered if cursor_in_table(state) => HintSet {
            prelude: None,
            chords: vec![
                HintChord::new("⇥", "Next cell"),
                HintChord::new("⇧⇥", "Prev cell"),
                HintChord::new("⌥↑↓←→", "Move row/col"),
                HintChord::new("⌥⇧↑↓←→", "Insert row/col"),
                HintChord::new("⌥⌫", "Del row"),
                HintChord::new("⌥⇧⌫", "Del col"),
            ],
        },
        Mode::Rendered | Mode::Raw => {
            // Baseline edit-mode hints — Menu anchors the row so
            // Ctrl-P is always the discovery chord; Cut / Copy /
            // Paste in keyboard-shortcut alphabetical order; Save;
            // view-mode toggle; Quit.  The view-mode chord label
            // flips with the current mode (Rendered → "Raw",
            // Raw → "Render") so the label always describes the
            // destination, not the current state.
            //
            // Contextual chords (those that only make sense on
            // specific characters) are prepended to the front of the
            // row so the user sees them immediately — they're the
            // reason to look at the hint line on any given line.
            // Link takes the leading slot because its trigger is the
            // narrowest (a specific `[text](url)` span); Toggle
            // follows because a task-list item extends across a full
            // line and is easier to spot.
            let view_toggle_label = match state.mode {
                Mode::Raw => "Render",
                _ => "Raw",
            };
            let mut chords = vec![
                HintChord::new("^P", "Menu"),
                HintChord::new("^X", "Cut"),
                HintChord::new("^C", "Copy"),
                HintChord::new("^V", "Paste"),
                HintChord::new("^S", "Save"),
                HintChord::new("^`", view_toggle_label),
                HintChord::new("^Q", "Quit"),
            ];
            // Insertion order here matters for the final layout:
            // the later insert pushes the earlier one back by one
            // slot.  Link first, then Toggle → when both are active
            // the final order is Link, Toggle, Menu, ...
            if cursor_on_task_item(state) {
                chords.insert(0, HintChord::new("^Space", "Toggle"));
            }
            if cursor_on_link(state) {
                chords.insert(0, HintChord::new("^↵", "Open link"));
            }
            HintSet {
                prelude: None,
                chords,
            }
        }
    }
}

/// Lay out a chord list into spans that fit `width` cells.  When the
/// full chord+label set doesn't fit, labels are dropped right-to-left
/// so the leftmost (highest-priority) chords keep their labels
/// longest; when even the bare chord badges don't all fit, the
/// rightmost chords drop off entirely.
///
/// Layout per hint: ` {chord} ` in `hint_chord` (2-space padding
/// creates the "badge" appearance), then `{label}` in `hint_label`,
/// then `  ` (two spaces) in `hint_bar` as the separator before the
/// next hint.  That gives 1 visible space between chord and label
/// (the trailing pad inside the chord badge) and 2 between hints,
/// matching the reviewed layout.
pub fn lay_out_chords(chords: &[HintChord], theme: &Theme, width: usize) -> Vec<Span<'static>> {
    if chords.is_empty() || width == 0 {
        return Vec::new();
    }
    let mut show_label = vec![false; chords.len()];
    let mut used: usize = chords.iter().map(HintChord::chord_only_width).sum();
    let mut last_visible = chords.len();
    while used > width && last_visible > 0 {
        last_visible -= 1;
        used -= chords[last_visible].chord_only_width();
    }
    for i in 0..last_visible {
        let delta = chords[i].full_width() - chords[i].chord_only_width();
        if used + delta <= width {
            show_label[i] = true;
            used += delta;
        }
    }

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(last_visible * 3);
    for (i, chord) in chords.iter().enumerate().take(last_visible) {
        // Chord badge: ` ^C ` painted with hint_chord bg so it reads
        // as a discrete pill sitting on the hint bar.
        spans.push(Span::styled(format!(" {} ", chord.chord), theme.hint_chord));
        if show_label[i] {
            // Label sits on hint_label bg (same family as hint_bar)
            // with no extra padding — the 1-space chord trailing
            // padding is the only gap between chord and label.
            spans.push(Span::styled(chord.label.clone(), theme.hint_label));
        }
        // Separator: two spaces of hint_bar between hints (also
        // painted after the last chord, and the trailing-fill step
        // in [`HintLine::render`] reuses the same bg for the rest of
        // the row).
        spans.push(Span::styled("  ".to_string(), theme.hint_bar));
    }
    spans
}

/// The hint-line widget.  Renders chords / transient / prompt onto a
/// single row, with a trailing fill using [`Theme::hint_bar`].
pub struct HintLine<'a> {
    pub content: HintContent,
    pub theme: &'a Theme,
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
                let mut remaining = width;
                // Prelude — plain text on the hint bar, followed by a
                // two-space gap that acts as a separator before the
                // chord row.  Rendered as hint_bar bg + hint_label fg
                // so it reads as a sentence, not another chord.
                if let Some(prelude) = &set.prelude {
                    let text = format!(" {}  ", prelude);
                    let w = text.chars().count();
                    v.push(Span::styled(text, self.theme.hint_label));
                    remaining = remaining.saturating_sub(w);
                }
                v.extend(lay_out_chords(&set.chords, self.theme, remaining));
                v
            }
            HintContent::Transient { text, style } => {
                vec![Span::styled(format!(" {} ", text), *style)]
            }
            HintContent::Prompt { prompt, chords } => {
                let prompt_text = format!(" {}  ", prompt);
                let prompt_w = prompt_text.chars().count();
                let prompt_span = Span::styled(prompt_text, self.theme.transient_warning);
                let chord_spans =
                    lay_out_chords(chords, self.theme, width.saturating_sub(prompt_w));
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
            all_spans.push(Span::styled(" ".repeat(width - used), self.theme.hint_bar));
        }

        Paragraph::new(Line::from(all_spans))
            .style(self.theme.hint_bar)
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
                HintLine {
                    content: self.hint,
                    theme: self.theme,
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
    use crate::config::Theme;
    use crate::document::Buffer;
    use crate::editor::{EditorState, Mode};
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn state(text: &str) -> EditorState {
        EditorState::new(Buffer::from_str(text), theme())
    }

    // ── hint_line_for ─────────────────────────────────────────────

    #[test]
    fn preview_hint_has_prelude_and_menu_first() {
        let st = state("hello");
        let set = hint_line_for(&st);
        assert_eq!(set.prelude.as_deref(), Some("Press any key to edit"));
        assert_eq!(set.chords[0].chord, "^P");
        assert_eq!(set.chords[0].label, "Menu");
        assert!(set.chords.iter().any(|c| c.label == "Quit"));
    }

    #[test]
    fn rendered_hint_has_save_and_copy_and_raw() {
        let mut st = state("hello");
        st.mode = Mode::Rendered;
        let set = hint_line_for(&st);
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(set.chords[0].chord, "^P", "Menu is always first");
        assert!(labels.contains(&"Cut"));
        assert!(labels.contains(&"Copy"));
        assert!(labels.contains(&"Paste"));
        assert!(labels.contains(&"Save"));
        assert!(
            labels.contains(&"Raw"),
            "Rendered-mode chord toggles TO Raw"
        );
        assert!(labels.contains(&"Quit"));
        // Plain paragraph has no link, so "Open link" must not appear.
        assert!(
            !labels.contains(&"Open link"),
            "link hint must stay hidden when the cursor isn't on a link"
        );
        // No prelude in edit mode.
        assert!(set.prelude.is_none());
    }

    #[test]
    fn raw_mode_flips_view_toggle_label_to_render() {
        let mut st = state("hello");
        st.mode = Mode::Raw;
        let set = hint_line_for(&st);
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
        let on_link = hint_line_for(&st);
        assert_eq!(
            on_link.chords[0].label, "Open link",
            "contextual link hint must lead the row"
        );
        // Cursor in the trailing plain-text tail → not on a link.
        st.cursor.offset = 32;
        let off_link = hint_line_for(&st);
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
        let set = hint_line_for(&st);
        assert_eq!(set.chords[0].label, "Open link");
        assert_eq!(set.chords[1].label, "Toggle");
        assert_eq!(
            set.chords[2].label, "Menu",
            "baseline Menu chord follows the contextual block"
        );
    }

    #[test]
    fn cut_comes_before_copy_in_edit_hints() {
        let mut st = state("hello");
        st.mode = Mode::Rendered;
        let set = hint_line_for(&st);
        let cut_idx = set.chords.iter().position(|c| c.label == "Cut").unwrap();
        let copy_idx = set.chords.iter().position(|c| c.label == "Copy").unwrap();
        assert!(cut_idx < copy_idx, "Cut must be listed before Copy");
    }

    #[test]
    fn plain_list_item_does_not_show_toggle_chord() {
        // Cursor at byte 2 — inside `- a` (a regular bullet, NOT a task).
        let mut st = state("- a\n- b\n");
        st.mode = Mode::Rendered;
        st.cursor.offset = 2;
        let set = hint_line_for(&st);
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
        let set = hint_line_for(&st);
        assert_eq!(set.chords[0].chord, "^Space");
        assert_eq!(set.chords[0].label, "Toggle");
    }

    #[test]
    fn raw_mode_suppresses_table_hints() {
        let source = "| a | b |\n| - | - |\n| c | d |\n";
        let mut st = state(source);
        st.mode = Mode::Raw;
        st.cursor.offset = 22;
        let set = hint_line_for(&st);
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert!(
            !labels.iter().any(|l| l.contains("cell")),
            "raw mode shows baseline edit hints, not table hints: {labels:?}"
        );
        assert!(labels.contains(&"Save"));
    }

    #[test]
    fn rendered_table_cursor_shows_table_chords() {
        let source = "| a | b |\n| - | - |\n| c | d |\n";
        let mut st = state(source);
        st.mode = Mode::Rendered;
        st.cursor.offset = 22;
        let set = hint_line_for(&st);
        let labels: Vec<_> = set.chords.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.iter().any(|l| l.contains("cell")));
        assert!(labels.iter().any(|l| l.contains("row")));
    }

    // ── lay_out_chords ────────────────────────────────────────────

    #[test]
    fn lay_out_drops_labels_when_tight() {
        let chords = vec![
            HintChord::new("^A", "Alpha"),
            HintChord::new("^B", "Bravo"),
            HintChord::new("^C", "Charlie"),
        ];
        // Generous width: all three get labels.
        let wide = lay_out_chords(&chords, theme(), 80);
        let concat: String = wide.iter().map(|s| s.content.to_string()).collect();
        assert!(concat.contains("Alpha"));
        assert!(concat.contains("Charlie"));

        // Chord-only baseline width (3 chords × 6 cells each) is 18.
        // At width 24, budget after baseline is 6 — enough for one
        // label (Alpha at 5 cells).  Bravo and Charlie stay bare.
        let medium = lay_out_chords(&chords, theme(), 24);
        let concat: String = medium.iter().map(|s| s.content.to_string()).collect();
        assert!(concat.contains("Alpha"));
        assert!(!concat.contains("Charlie"), "got: {:?}", concat);

        // Tight width: the chord badges still render but every label
        // is dropped.
        let tight = lay_out_chords(&chords, theme(), 19);
        let concat: String = tight.iter().map(|s| s.content.to_string()).collect();
        assert!(concat.contains("^A"));
        assert!(!concat.contains("Alpha"), "got: {:?}", concat);
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
