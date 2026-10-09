//! Table-aware vim scoping: where the cursor's cell begins and ends, and which vim
//! commands respect that boundary.  See `docs/dev/vim-tables.md`.
//!
//! A rendered GFM table's `|` delimiters and alignment row are structure, not prose, but
//! stock vim motions treat a row as an ordinary line — `$` parks on the outer `|`, `w`
//! walks into the next cell, `D` wipes a row's delimiters.  This module narrows the
//! motions that should stay in one cell and re-routes `o`/`O`/`dd`/`cc` onto the
//! structural `table_edit` primitives.
//!
//! **Raw mode is exempt, for free.**  Every query funnels through
//! [`table_edit_ops::current_table`], which is `None` in
//! [`Mode::Raw`](crate::editor::Mode::Raw) — the user must be able to repair a broken
//! table byte by byte — so no call site needs its own mode check.
//!
//! **One derivation of the cell bounds.**  [`cell_scope`] is the only byte→char conversion
//! of `table_edit`'s cell offsets; the motion clamp, the operator-range clamp, and `cc`
//! all read it rather than re-deriving, so they cannot drift.
//!
//! **No bare resolver calls survive in `feed.rs`.**  [`resolve_scoped_motion`] and
//! [`resolve_scoped_op_range`] *replace* the `motion::resolve_*` pair at the input layer
//! rather than wrapping results per call site, so a new operator target cannot forget the
//! clamp.  `motion.rs` stays pure and buffer-only.
//!
//! **Clamping is not the safety net — [`range_breaks_a_table`] is.**  A range can reach a
//! protected row by a route with no cell to clamp against (`2dd`, `dj`, a VisualLine
//! selection whose cursor has left the table), so every mutating path checks that
//! predicate immediately before it runs.
//!
//! **A charwise Visual highlight must cover only the cell's content**, so the horizontal
//! motions clamp harder there via [`CellLimit`] and `h`/`l` use [`visual_cell_step`].  The
//! guarantee is horizontal only; `j`/`k` and the unscoped document motions still leave the
//! cell, and the range guard catches those.

use std::ops::Range;

use crate::document::{next_grapheme_offset, prev_grapheme_offset, EditDelta};
use crate::editor::edit_ops::cursor_byte;
use crate::editor::table_edit::{self, RowKind, TableInfo, TableRow};
use crate::editor::table_edit_ops;
use crate::editor::vim_ops::motion::{resolve_motion, resolve_motion_range, Motion, OpRange};
use crate::editor::vim_ops::operator::{execute_operator, OpResult, Operator};
use crate::editor::EditorState;

/// The char-offset content bounds of the cursor's table cell: `start` is the first content
/// column (past the padding after `|`) and `end` the append position just past the last
/// non-whitespace char — the same two anchors `table_move_horizontal` clamps `h`/`l` to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellScope {
    pub start: usize,
    pub end: usize,
}

impl CellScope {
    fn contains(&self, offset: usize) -> bool {
        offset >= self.start && offset <= self.end
    }

    fn as_range(&self) -> Range<usize> {
        self.start..self.end
    }
}

/// How far right inside a cell the cursor may rest.  The two answers differ by one
/// grapheme; which is right depends on whether the cursor's own position is highlighted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellLimit {
    /// Up to [`CellScope::end`], where `$` parks in Normal and an exclusive-end operator
    /// target belongs: nothing is highlighted, so "type here" is a legitimate rest.
    Append,
    /// Up to the cell's last character.  A charwise Visual span includes the char under
    /// the cursor, so a cursor on the append slot highlights the padding space before the
    /// `|` and hands the operator a range that eats it.  Not a *refusal* — [`table_break`]
    /// measures confinement against the untrimmed cell span, so that padding is inside the
    /// cell as far as the guard is concerned — just cosmetic, and what vim's `$` does in
    /// Visual anyway.
    LastChar,
}

/// The furthest offset a cursor may occupy in `scope`, never below `scope.start` — so an
/// empty cell collapses to its one slot.
fn cell_max_cursor(state: &EditorState, scope: CellScope, limit: CellLimit) -> usize {
    match limit {
        CellLimit::Append => scope.end,
        CellLimit::LastChar => prev_grapheme_offset(&state.buffer, scope.end).max(scope.start),
    }
}

// ── Queries ─────────────────────────────────────────────────────────────────

/// The cursor's cell bounds.  `None` outside a table, in Raw mode, and — deliberately —
/// on the alignment row, which stays hand-editable with plain line semantics (as
/// [`table_edit_ops::table_move_horizontal`] also assumes).
pub fn cell_scope(state: &EditorState) -> Option<CellScope> {
    cell_scope_at(state, state.cursor.offset)
}

/// [`cell_scope`] for a position other than the cursor's — the Visual anchor, which sits
/// in its own cell.
fn cell_scope_at(state: &EditorState, offset: usize) -> Option<CellScope> {
    let byte = state.buffer.rope().char_to_byte(offset);
    let info = table_edit_ops::table_at(state, byte)?;
    let (row, col) = table_edit::cursor_cell(&info, byte)?;
    if info.rows.get(row)?.kind == RowKind::Alignment {
        return None;
    }
    let start_byte = table_edit::cell_cursor_offset(&info, row, col)?;
    let end_byte = table_edit::cell_end_cursor_offset(&info, row, col)?;
    let rope = state.buffer.rope();
    let start = rope.byte_to_char(start_byte);
    let end = rope.byte_to_char(end_byte).max(start);
    Some(CellScope { start, end })
}

/// The [`RowKind`] the cursor is on.  Unlike [`cell_scope`] this *does* answer for the
/// alignment row: `dd` must refuse there even though motions stay unscoped.
pub fn cursor_row_kind(state: &EditorState) -> Option<RowKind> {
    let info = table_edit_ops::current_table(state)?;
    let byte = cursor_byte(state);
    let (row, _) = table_edit::cursor_cell(&info, byte)?;
    info.rows.get(row).map(|r| r.kind)
}

// ── The structural guard ────────────────────────────────────────────────────

/// Why an edit can't run as asked, so the caller flashes the reason that applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableBreak {
    /// Takes out a header or alignment row while leaving the rest standing — the
    /// survivors would reparse as paragraph text.
    ProtectedRow,
    /// Reaches across a cell boundary, so a row's `|` delimiters or newline are inside.
    CrossesCells,
}

impl TableBreak {
    pub fn message(self) -> &'static str {
        match self {
            TableBreak::ProtectedRow => "Can't remove a table's header or alignment row",
            TableBreak::CrossesCells => "Can't edit across table cells",
        }
    }
}

/// Would mutating the byte range `start..end` leave a *broken* table behind?
///
/// The single structural guard for the vim mutation paths.  It answers for a range, not
/// the cursor, because that is the only way to catch a `2dd` / `dj` / VisualLine span that
/// reaches a protected row with the cursor elsewhere.
///
/// Allowed: a range swallowing a table whole (deleting a table is legitimate); one
/// covering only complete `Data` rows (`dd`); one confined to a cell's content; one
/// confined to the alignment row's own text, which stays hand-editable.  Everything else
/// is refused.
pub fn range_breaks_a_table(state: &EditorState, start: usize, end: usize) -> Option<TableBreak> {
    let rope = state.buffer.rope();
    let len = rope.len_bytes();
    let start = start.min(len);
    let end = end.min(len).max(start);

    // Table by table: an ordinary-prose probe costs one parse lookup and a
    // table probe jumps past the table, so a whole-document selection stays linear.
    let mut probe = start;
    loop {
        match table_edit_ops::table_at(state, probe) {
            Some(info) => {
                if let Some(reason) = table_break(&info, start, end) {
                    return Some(reason);
                }
                if info.end <= probe {
                    break; // no forward progress possible
                }
                probe = info.end;
            }
            None => {
                let line = rope.byte_to_line(probe);
                if line + 1 >= rope.len_lines() {
                    break;
                }
                probe = rope.line_to_byte(line + 1);
            }
        }
        if probe >= end {
            break;
        }
    }
    None
}

/// [`range_breaks_a_table`] for an operator's range.  The linewise arm mirrors
/// `execute_operator`'s own expansion, so the guard sees exactly the bytes it would remove.
pub fn op_range_breaks_a_table(state: &EditorState, range: &OpRange) -> Option<TableBreak> {
    let rope = state.buffer.rope();
    let (start_char, end_char) = match range {
        OpRange::Chars(r) => (r.start, r.end),
        OpRange::Lines { first, last } => {
            let line_count = state.buffer.line_count();
            let start = state.buffer.line_to_char((*first).min(line_count));
            let end = if last + 1 < line_count {
                state.buffer.line_to_char(last + 1)
            } else {
                state.buffer.len_chars()
            };
            (start, end)
        }
    };
    let len_chars = rope.len_chars();
    let start = rope.char_to_byte(start_char.min(len_chars));
    let end = rope.char_to_byte(end_char.min(len_chars));
    range_breaks_a_table(state, start, end)
}

/// Does line span `first..=last` touch any table?  The blunter question, for commands that
/// reshape lines without deleting them: `J` merges two rows into one malformed line and
/// `>>` indents a row out of its block, so *any* overlap is a refusal — even total cover.
pub fn lines_touch_a_table(state: &EditorState, first: usize, last: usize) -> bool {
    let line_count = state.buffer.line_count();
    if first >= line_count {
        return false;
    }
    let last = last.min(line_count.saturating_sub(1));
    let rope = state.buffer.rope();
    let mut line = first;
    while line <= last {
        let byte = rope.line_to_byte(line);
        if table_edit_ops::table_at(state, byte).is_some() {
            return true;
        }
        line += 1;
    }
    false
}

/// Would this range break `info` specifically?  [`range_breaks_a_table`] carries the policy.
fn table_break(info: &TableInfo, start: usize, end: usize) -> Option<TableBreak> {
    if start <= info.start && end >= info.end {
        return None;
    }
    // `start + 1` keeps an empty range (an `x` covering nothing) attached to its row.
    let touched: Vec<&TableRow> = info
        .rows
        .iter()
        .filter(|r| r.start < end.max(start + 1) && r.end > start)
        .collect();
    if touched.is_empty() {
        return None;
    }
    if touched
        .iter()
        .all(|r| r.kind == RowKind::Data && r.start >= start && r.end <= end)
    {
        return None;
    }
    if let [row] = touched[..] {
        let confined = if row.kind == RowKind::Alignment {
            // Hand-editable within its own text only: running off the end takes the
            // newline with it.
            start >= row.start && end <= row.start + row.raw.len()
        } else {
            row.cells
                .iter()
                .any(|c| start >= row.start + c.content_start && end <= row.start + c.content_end)
        };
        if confined {
            return None;
        }
    }
    // Whole rows but not all data → a protected row is going.  Otherwise the range
    // slices through a row's structure.
    if touched.iter().all(|r| r.start >= start && r.end <= end) {
        Some(TableBreak::ProtectedRow)
    } else {
        Some(TableBreak::CrossesCells)
    }
}

// ── Scoped motion resolution ────────────────────────────────────────────────

/// Whether `motion` is confined to the cursor's cell.
///
/// Scoped: everything that reads as "move within this piece of text" — char steps, word
/// motions, line anchors, character finds.  Excluded: the motions whose purpose is to
/// *leave* the current context (`gg`/`G`, `{`/`}`, `%`).  A new `Motion` variant defaults
/// to unscoped; `cell_scoped_motions_match_the_spec` pins the split both ways.
fn motion_is_cell_scoped(motion: Motion) -> bool {
    match motion {
        Motion::Left
        | Motion::Right
        | Motion::WordForward
        | Motion::WordEnd
        | Motion::WordBackward
        | Motion::CurrentWordEnd
        | Motion::CurrentBigWordEnd
        | Motion::BigWordForward
        | Motion::BigWordEnd
        | Motion::BigWordBackward
        | Motion::LineStart
        | Motion::LineFirstNonBlank
        | Motion::LineEnd
        | Motion::FindChar(..) => true,
        Motion::DocStart
        | Motion::DocEnd
        | Motion::GoToLine(_)
        | Motion::ParagraphForward
        | Motion::ParagraphBackward
        | Motion::MatchingPair => false,
    }
}

/// Confine an already-resolved `target` to the cursor's cell, up to `limit`.  A no-op
/// outside a table, on the alignment row, or for an unscoped motion.
///
/// Two overshoot shapes: `f`/`t`/`;`/`,` **fail** (a find whose target is in another cell
/// has no match, and landing on the cell edge would pretend it succeeded); everything else
/// **clamps** (the user asked to travel as far as this direction goes, and that is now the
/// cell edge).
///
/// Exposed for the `;` / `,` replay path, which resolves through `resolve_find_repeat`.
pub fn scope_offset(state: &EditorState, motion: Motion, target: usize, limit: CellLimit) -> usize {
    if !motion_is_cell_scoped(motion) {
        return target;
    }
    let Some(scope) = cell_scope(state) else {
        return target;
    };
    let max = cell_max_cursor(state, scope, limit);
    if target >= scope.start && target <= max {
        return target;
    }
    if matches!(motion, Motion::FindChar(..)) {
        return state.cursor.offset;
    }
    target.clamp(scope.start, max)
}

/// The drop-in replacement for `motion::resolve_motion` at the input layer; identical to
/// it outside a table.
pub fn resolve_scoped_motion(
    state: &EditorState,
    motion: Motion,
    count: u32,
    limit: CellLimit,
) -> usize {
    let target = resolve_motion(motion, count, state.cursor.offset, &state.buffer);
    scope_offset(state, motion, target, limit)
}

/// `h` / `l` in charwise Visual: one grapheme step held inside the cursor's cell.
/// `false` when there is no cell to hold it in, so the caller falls back to the ordinary
/// cell-to-cell step.
///
/// Cell-to-cell stepping is right in Normal — it is how you cross a table — but in
/// charwise Visual it grows the highlight over the `|` and [`range_breaks_a_table`] then
/// refuses the edit that highlight promised.
pub fn visual_cell_step(state: &mut EditorState, forward: bool) -> bool {
    let Some(scope) = cell_scope(state) else {
        return false;
    };
    let cursor = state.cursor.offset;
    if !scope.contains(cursor) {
        return false;
    }
    let target = if forward {
        next_grapheme_offset(&state.buffer, cursor)
    } else {
        prev_grapheme_offset(&state.buffer, cursor)
    };
    // `.max(cursor)`: a cursor that entered Visual on the append slot must not be
    // dragged backwards by a forward step.
    let max = cell_max_cursor(state, scope, CellLimit::LastChar).max(cursor);
    state.cursor.offset = target.clamp(scope.start, max);
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
    true
}

/// Pull a charwise-Visual endpoint off its cell's append slot onto the last character, so
/// the first highlight already covers content rather than the padding before the `|`.
/// `None` means "leave it exactly where it is".
///
/// Both ends need this, each against its *own* cell: `V`→`v` inherits an anchor and cursor
/// that may sit in different cells, either parked on an append slot by `$`.
pub fn visual_endpoint_in_cell(state: &EditorState, offset: usize) -> Option<usize> {
    let scope = cell_scope_at(state, offset)?;
    if !scope.contains(offset) {
        return None;
    }
    Some(offset.min(cell_max_cursor(state, scope, CellLimit::LastChar)))
}

/// The drop-in replacement for `motion::resolve_motion_range`.
///
/// `OpRange::Lines` passes through untouched — no cell-scoped motion produces one, and
/// `dj` / `dgg` are meant to leave the row.  A failed find yields an empty range at the
/// cursor, so `df(` for a `(` in the next cell deletes nothing rather than eating up to
/// the cell edge.
pub fn resolve_scoped_op_range(state: &EditorState, motion: Motion, count: u32) -> OpRange {
    let cursor = state.cursor.offset;
    let range = resolve_motion_range(motion, count, cursor, &state.buffer);
    let OpRange::Chars(chars) = range else {
        return range;
    };
    if !motion_is_cell_scoped(motion) {
        return OpRange::Chars(chars);
    }
    let Some(scope) = cell_scope(state) else {
        return OpRange::Chars(chars);
    };
    if matches!(motion, Motion::FindChar(..)) {
        let dest = resolve_motion(motion, count, cursor, &state.buffer);
        if !scope.contains(dest) {
            return OpRange::Chars(cursor..cursor);
        }
    }
    let start = chars.start.clamp(scope.start, scope.end);
    let end = chars.end.clamp(scope.start, scope.end).max(start);
    OpRange::Chars(start..end)
}

// ── Structural commands ─────────────────────────────────────────────────────

/// What a doubled operator (`dd` / `cc`) made of its table interpretation.  One enum for
/// both, so a caller cannot collapse "in a table, refused" into "not a table, fall
/// through" — the bug that let `cc` blank the alignment row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableOpOutcome {
    /// The edit ran; fold the [`OpResult`] to fill the register.
    Applied(OpResult),
    /// The cursor is on a row this edit must not destroy.
    Refused(TableBreak),
    /// Not in a table (or in Raw mode): fall back to the ordinary linewise behavior.
    NotATable,
}

/// `o` / `O` inside a table: insert a structural row and land on its first cell.  `false`
/// outside a table, so the caller falls back to `open_list_continue` / a plain newline —
/// which here would split the row in two, as stock vim's `o` does.
pub fn open_table_row(
    state: &mut EditorState,
    below: bool,
    viewport_height: usize,
    viewport_width: usize,
) -> bool {
    if table_edit_ops::current_table(state).is_none() {
        return false;
    }
    table_edit_ops::table_insert_row(state, below, viewport_height, viewport_width);
    true
}

/// `dd` inside a table: remove the row structurally, refusing on the header and alignment
/// rows, whose loss turns the remaining rows back into paragraph text.  The raw text goes
/// to the unnamed register as a linewise yank, so `dd` then `p` moves a row.
pub fn delete_table_row(
    state: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) -> TableOpOutcome {
    let Some(info) = table_edit_ops::current_table(state) else {
        return TableOpOutcome::NotATable;
    };
    let byte = cursor_byte(state);
    let Some((row_idx, _)) = table_edit::cursor_cell(&info, byte) else {
        return TableOpOutcome::NotATable;
    };
    let Some(row) = info.rows.get(row_idx) else {
        return TableOpOutcome::NotATable;
    };
    if row.kind != RowKind::Data {
        return TableOpOutcome::Refused(TableBreak::ProtectedRow);
    }
    let register_text = format!("{}\n", row.raw);
    table_edit_ops::table_delete_row(state, viewport_height, viewport_width);
    TableOpOutcome::Applied(OpResult {
        register_text,
        linewise: true,
        enter_insert: false,
    })
}

/// `cc` inside a table: clear the cursor's *cell* and enter Insert.  The cell is the
/// table's equivalent of a line; clearing the raw line would take the `|` delimiters with
/// it.  Routed through `execute_operator` so the single-delta / register / enter-Insert
/// contract is not duplicated.
///
/// Refuses on the alignment row rather than falling through: it has no cell scope, but a
/// linewise `cc` there would blank the line defining the table's shape.
pub fn clear_table_cell(state: &mut EditorState) -> TableOpOutcome {
    if table_edit_ops::current_table(state).is_none() {
        return TableOpOutcome::NotATable;
    }
    let Some(scope) = cell_scope(state) else {
        return TableOpOutcome::Refused(TableBreak::ProtectedRow);
    };
    TableOpOutcome::Applied(execute_operator(
        state,
        Operator::Change,
        OpRange::Chars(scope.as_range()),
    ))
}

// ── Paste ───────────────────────────────────────────────────────────────────

/// Where `p` / `P` should put the register when the cursor is in a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TablePaste {
    /// Insert at this char offset — a legal row boundary, not the one the ordinary
    /// linewise paste would pick.
    RowsAt(usize),
    /// The register can't land here without breaking the table.
    Refused,
    /// Not in a table: use the ordinary paste path unchanged.
    NotATable,
}

/// How `p` / `P` should behave inside a table.  Two hazards, both reachable from the
/// register `dd` fills: a linewise paste "after the cursor's line" would insert a data row
/// above the alignment row that declares the columns (so the target clamps below it, as
/// [`table_edit::insert_row`] does); and a register that isn't table rows — or a charwise
/// one carrying a `|` or newline — would split the row it lands in, so it is refused.
pub fn table_paste_plan(
    state: &EditorState,
    text: &str,
    linewise: bool,
    after: bool,
) -> TablePaste {
    let Some(info) = table_edit_ops::current_table(state) else {
        return TablePaste::NotATable;
    };
    if !linewise {
        // A charwise register just widens the cell, unless it carries structure.
        return if text.contains('|') || text.contains('\n') {
            TablePaste::Refused
        } else {
            TablePaste::NotATable
        };
    }
    if !text
        .lines()
        .all(|l| l.trim().is_empty() || table_edit::has_cell_pipe(l))
    {
        return TablePaste::Refused;
    }
    let byte = cursor_byte(state);
    let Some((row_idx, _)) = table_edit::cursor_cell(&info, byte) else {
        return TablePaste::NotATable;
    };
    let target = if after { row_idx + 1 } else { row_idx };
    // Never above the alignment row: a data row there reads as part of the header block.
    let target = target.clamp(2, info.rows.len());
    let byte_at = if target < info.rows.len() {
        info.rows[target].start
    } else {
        info.end
    };
    TablePaste::RowsAt(state.buffer.rope().byte_to_char(byte_at))
}

/// Insert `text` as whole rows at `at` (from [`table_paste_plan`]) and land on the new
/// row's first cell.  Adds the separating newline itself when the table has no trailing
/// one, so appending can't glue the pasted row onto the last.
pub fn insert_table_rows(state: &mut EditorState, at: usize, text: &str) {
    let needs_separator = at > 0 && state.buffer.rope().char(at - 1) != '\n';
    let payload = if needs_separator {
        format!("\n{}", text.strip_suffix('\n').unwrap_or(text))
    } else {
        text.to_owned()
    };
    let landing = if needs_separator { at + 1 } else { at };
    state.apply_delta(EditDelta {
        offset: at,
        removed: String::new(),
        inserted: payload,
    });
    state.place_cursor(landing.min(state.buffer.len_chars()));
    if let Some(scope) = cell_scope(state) {
        state.place_cursor(scope.start);
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::Buffer;
    use crate::editor::vim_ops::motion::FindKind;
    use crate::editor::Mode;

    const TABLE: &str = "| alpha | bravo |\n|---|---|\n| one | two |\n";

    fn state_at(offset: usize) -> EditorState {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let mut st = EditorState::new(Buffer::from_str(TABLE), theme);
        st.mode = Mode::Rendered;
        st.cursor.offset = offset;
        st.update_cursor_block();
        st
    }

    fn at(needle: &str) -> usize {
        TABLE.find(needle).expect("needle present in fixture")
    }

    #[test]
    fn cell_scope_spans_only_the_cursors_cell() {
        let st = state_at(at("alpha"));
        let scope = cell_scope(&st).expect("cursor is in a header cell");
        assert_eq!(scope.start, at("alpha"));
        assert_eq!(scope.end, at("alpha") + "alpha".len());
    }

    #[test]
    fn cell_scope_is_none_on_the_alignment_row() {
        let st = state_at(at("|---|") + 2);
        assert!(cell_scope(&st).is_none());
        // …but it is a known row kind, so `dd` can refuse on it.
        assert_eq!(cursor_row_kind(&st), Some(RowKind::Alignment));
    }

    #[test]
    fn cell_scope_is_none_outside_a_table() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let mut st = EditorState::new(Buffer::from_str("just a paragraph\n"), theme);
        st.mode = Mode::Rendered;
        st.update_cursor_block();
        assert!(cell_scope(&st).is_none());
        assert!(cursor_row_kind(&st).is_none());
    }

    /// Raw mode must behave exactly like plain text.
    #[test]
    fn raw_mode_has_no_cell_scope() {
        let mut st = state_at(at("alpha"));
        st.mode = Mode::Raw;
        assert!(cell_scope(&st).is_none());
        assert!(cursor_row_kind(&st).is_none());
        let scoped = resolve_scoped_motion(&st, Motion::LineEnd, 1, CellLimit::Append);
        let bare = resolve_motion(Motion::LineEnd, 1, st.cursor.offset, &st.buffer);
        assert_eq!(scoped, bare);
    }

    /// Pinned both ways so a new `Motion` variant can't silently join or miss the set.
    #[test]
    fn cell_scoped_motions_match_the_spec() {
        for m in [
            Motion::Left,
            Motion::Right,
            Motion::WordForward,
            Motion::WordEnd,
            Motion::WordBackward,
            Motion::CurrentWordEnd,
            Motion::CurrentBigWordEnd,
            Motion::BigWordForward,
            Motion::BigWordEnd,
            Motion::BigWordBackward,
            Motion::LineStart,
            Motion::LineFirstNonBlank,
            Motion::LineEnd,
            Motion::FindChar('x', FindKind::Forward),
        ] {
            assert!(motion_is_cell_scoped(m), "{m:?} must be cell-scoped");
        }
        for m in [
            Motion::DocStart,
            Motion::DocEnd,
            Motion::GoToLine(3),
            Motion::ParagraphForward,
            Motion::ParagraphBackward,
            Motion::MatchingPair,
        ] {
            assert!(!motion_is_cell_scoped(m), "{m:?} must escape the cell");
        }
    }

    #[test]
    fn line_end_clamps_to_the_cell_not_the_row() {
        let st = state_at(at("alpha"));
        let target = resolve_scoped_motion(&st, Motion::LineEnd, 1, CellLimit::Append);
        assert_eq!(target, at("alpha") + "alpha".len());
    }

    #[test]
    fn word_forward_stops_at_the_cell_edge() {
        let st = state_at(at("alpha"));
        let bare = resolve_motion(Motion::WordForward, 1, st.cursor.offset, &st.buffer);
        assert!(bare > at("alpha") + "alpha".len());
        let scoped = resolve_scoped_motion(&st, Motion::WordForward, 1, CellLimit::Append);
        assert_eq!(scoped, at("alpha") + "alpha".len());
    }

    /// A find whose target is in another cell is a *failed* find, not a partial move.
    #[test]
    fn find_outside_the_cell_does_not_move_the_cursor() {
        let st = state_at(at("alpha"));
        let motion = Motion::FindChar('b', FindKind::Forward);
        assert_eq!(
            resolve_scoped_motion(&st, motion, 1, CellLimit::Append),
            st.cursor.offset
        );
        assert_eq!(
            resolve_scoped_op_range(&st, motion, 1),
            OpRange::Chars(st.cursor.offset..st.cursor.offset)
        );
    }

    #[test]
    fn find_inside_the_cell_still_resolves() {
        let st = state_at(at("alpha"));
        let motion = Motion::FindChar('h', FindKind::Forward);
        assert_eq!(
            resolve_scoped_motion(&st, motion, 1, CellLimit::Append),
            at("alpha") + 3
        );
    }

    /// `D` and `C` must stop at the cell's content end so the `|` delimiters survive.
    #[test]
    fn line_end_op_range_stops_at_the_cell_edge() {
        let st = state_at(at("alpha"));
        assert_eq!(
            resolve_scoped_op_range(&st, Motion::LineEnd, 1),
            OpRange::Chars(at("alpha")..at("alpha") + "alpha".len())
        );
    }

    /// One step right of the last character, `x` covers nothing rather than eating the `|`.
    #[test]
    fn right_op_range_never_reaches_the_delimiter() {
        let last = at("alpha") + "alpha".len() - 1;
        let st = state_at(last);
        assert_eq!(
            resolve_scoped_op_range(&st, Motion::Right, 1),
            OpRange::Chars(last..last + 1)
        );

        let past = at("alpha") + "alpha".len();
        let st = state_at(past);
        assert_eq!(
            resolve_scoped_op_range(&st, Motion::Right, 1),
            OpRange::Chars(past..past)
        );
    }

    // ── The charwise-Visual tightening ──────────────────────────────────

    /// Under `LastChar` the inclusive charwise span ends on the cell's content.
    #[test]
    fn the_visual_limit_stops_one_grapheme_short_of_the_append_slot() {
        let st = state_at(at("alpha"));
        let last = at("alpha") + "alpha".len();
        assert_eq!(
            resolve_scoped_motion(&st, Motion::LineEnd, 1, CellLimit::Append),
            last
        );
        assert_eq!(
            resolve_scoped_motion(&st, Motion::LineEnd, 1, CellLimit::LastChar),
            last - 1
        );
    }

    /// A whole grapheme back, not a char: a combining sequence is one selectable unit.
    #[test]
    fn the_visual_limit_steps_back_a_whole_grapheme() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let src = "| ae\u{301} | b |\n|---|---|\n| one | two |\n";
        let mut st = EditorState::new(Buffer::from_str(src), theme);
        st.mode = Mode::Rendered;
        st.cursor.offset = 2;
        st.update_cursor_block();
        let scope = cell_scope(&st).expect("cursor is in a cell");
        assert_eq!(scope.end, 5, "content is `ae\u{301}`");
        assert_eq!(
            resolve_scoped_motion(&st, Motion::LineEnd, 1, CellLimit::LastChar),
            3,
            "`e\u{301}` is one grapheme, so the limit is the `e`, not the accent"
        );
    }

    #[test]
    fn visual_cell_step_holds_the_cursor_inside_the_cell() {
        let mut st = state_at(at("alpha"));
        for _ in 0..9 {
            assert!(visual_cell_step(&mut st, /*forward=*/ true));
        }
        assert_eq!(st.cursor.offset, at("alpha") + "alpha".len() - 1);
        for _ in 0..9 {
            assert!(visual_cell_step(&mut st, /*forward=*/ false));
        }
        assert_eq!(st.cursor.offset, at("alpha"));
    }

    /// A cursor that entered Visual on the append slot must not be dragged backwards.
    #[test]
    fn visual_cell_step_forward_never_moves_backwards() {
        let mut st = state_at(at("alpha") + "alpha".len());
        assert!(visual_cell_step(&mut st, /*forward=*/ true));
        assert_eq!(st.cursor.offset, at("alpha") + "alpha".len());
    }

    /// No cell to hold the step in → the caller falls back to the cell-to-cell move.
    #[test]
    fn visual_cell_step_declines_outside_a_cell() {
        let mut st = state_at(at("|---|") + 2); // the alignment row
        assert!(!visual_cell_step(&mut st, true));

        let mut st = state_at(at("alpha"));
        st.mode = Mode::Raw;
        assert!(!visual_cell_step(&mut st, true));
    }

    #[test]
    fn visual_endpoint_pulls_back_from_the_append_slot_only() {
        let st = state_at(at("alpha") + "alpha".len());
        assert_eq!(
            visual_endpoint_in_cell(&st, st.cursor.offset),
            Some(at("alpha") + "alpha".len() - 1)
        );
        let st = state_at(at("alpha") + 2);
        assert_eq!(
            visual_endpoint_in_cell(&st, st.cursor.offset),
            Some(at("alpha") + 2)
        );
        let st = state_at(at("|---|") + 2);
        assert_eq!(visual_endpoint_in_cell(&st, st.cursor.offset), None);
    }

    /// Resolved against *its own* cell — what `V`→`v` needs for a displaced anchor.
    #[test]
    fn visual_endpoint_answers_for_a_cell_the_cursor_is_not_in() {
        let st = state_at(at("alpha"));
        let bravo_append = at("bravo") + "bravo".len();
        assert_eq!(
            visual_endpoint_in_cell(&st, bravo_append),
            Some(bravo_append - 1)
        );
        let two_append = at("two") + "two".len();
        assert_eq!(
            visual_endpoint_in_cell(&st, two_append),
            Some(two_append - 1)
        );
    }

    /// The operator range is exclusive-ended, so it keeps the `Append` bound.
    #[test]
    fn the_operator_range_keeps_the_append_bound() {
        let st = state_at(at("alpha"));
        assert_eq!(
            resolve_scoped_op_range(&st, Motion::LineEnd, 1),
            OpRange::Chars(at("alpha")..at("alpha") + "alpha".len())
        );
    }

    #[test]
    fn document_motions_are_not_clamped() {
        let st = state_at(at("one"));
        assert_eq!(
            resolve_scoped_motion(&st, Motion::DocStart, 1, CellLimit::Append),
            resolve_motion(Motion::DocStart, 1, st.cursor.offset, &st.buffer)
        );
    }

    #[test]
    fn delete_table_row_refuses_header_and_alignment() {
        let mut st = state_at(at("alpha"));
        assert_eq!(
            delete_table_row(&mut st, 40, 80),
            TableOpOutcome::Refused(TableBreak::ProtectedRow)
        );
        assert_eq!(st.buffer.contents(), TABLE);

        let mut st = state_at(at("|---|") + 2);
        assert_eq!(
            delete_table_row(&mut st, 40, 80),
            TableOpOutcome::Refused(TableBreak::ProtectedRow)
        );
        assert_eq!(st.buffer.contents(), TABLE);
    }

    #[test]
    fn delete_table_row_removes_a_data_row_and_fills_the_register() {
        let mut st = state_at(at("one"));
        let outcome = delete_table_row(&mut st, 40, 80);
        let TableOpOutcome::Applied(res) = outcome else {
            panic!("expected the data row to be deleted, got {outcome:?}");
        };
        assert_eq!(res.register_text, "| one | two |\n");
        assert!(res.linewise);
        assert!(!res.enter_insert);
        assert!(!st.buffer.contents().contains("one"));
        assert!(st.buffer.contents().contains("| alpha | bravo |"));
    }

    #[test]
    fn delete_table_row_outside_a_table_declines() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let mut st = EditorState::new(Buffer::from_str("paragraph\n"), theme);
        st.mode = Mode::Rendered;
        st.update_cursor_block();
        assert_eq!(delete_table_row(&mut st, 40, 80), TableOpOutcome::NotATable);
    }

    #[test]
    fn clear_table_cell_empties_only_that_cell() {
        let mut st = state_at(at("alpha"));
        let TableOpOutcome::Applied(res) = clear_table_cell(&mut st) else {
            panic!("cursor is in a cell");
        };
        assert_eq!(res.register_text, "alpha");
        assert!(res.enter_insert);
        assert!(!res.linewise);
        assert!(st.buffer.contents().starts_with("|  | bravo |\n"));
        assert!(st.buffer.contents().contains("| one | two |"));
    }

    /// No cell scope, but `cc` there would blank the row declaring the columns.
    #[test]
    fn clear_table_cell_refuses_on_the_alignment_row() {
        let mut st = state_at(at("|---|") + 2);
        assert_eq!(
            clear_table_cell(&mut st),
            TableOpOutcome::Refused(TableBreak::ProtectedRow)
        );
        assert_eq!(st.buffer.contents(), TABLE);
    }

    #[test]
    fn clear_table_cell_outside_a_table_declines() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let mut st = EditorState::new(Buffer::from_str("paragraph\n"), theme);
        st.mode = Mode::Rendered;
        st.update_cursor_block();
        assert_eq!(clear_table_cell(&mut st), TableOpOutcome::NotATable);
    }

    #[test]
    fn open_table_row_inserts_a_structural_row() {
        let mut st = state_at(at("one"));
        assert!(open_table_row(&mut st, /*below=*/ true, 40, 80));
        let contents = st.buffer.contents();
        assert_eq!(contents.lines().count(), 4);
        assert!(contents.lines().nth(3).is_some_and(|l| l.contains('|')));
    }

    #[test]
    fn open_table_row_declines_outside_a_table() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let mut st = EditorState::new(Buffer::from_str("paragraph\n"), theme);
        st.mode = Mode::Rendered;
        st.update_cursor_block();
        assert!(!open_table_row(&mut st, true, 40, 80));
    }

    // ── The structural guard ────────────────────────────────────────────

    /// Lines `first..=last` as a linewise operator would take them.
    fn line_span(st: &EditorState, first: usize, last: usize) -> (usize, usize) {
        let rope = st.buffer.rope();
        let start = rope.line_to_byte(first);
        let end = if last + 1 < rope.len_lines() {
            rope.line_to_byte(last + 1)
        } else {
            rope.len_bytes()
        };
        (start, end)
    }

    #[test]
    fn deleting_a_whole_data_row_is_allowed() {
        let st = state_at(at("one"));
        let (s, e) = line_span(&st, 2, 2);
        assert_eq!(range_breaks_a_table(&st, s, e), None);
    }

    #[test]
    fn deleting_the_header_or_alignment_row_breaks_the_table() {
        let st = state_at(at("one"));
        for line in [0, 1] {
            let (s, e) = line_span(&st, line, line);
            assert_eq!(
                range_breaks_a_table(&st, s, e),
                Some(TableBreak::ProtectedRow),
                "line {line} carries the table's shape"
            );
        }
    }

    /// `2dd`: the guard keys on the span, not on the cursor's own row.
    #[test]
    fn a_counted_span_over_protected_rows_breaks_the_table() {
        let st = state_at(at("alpha"));
        let (s, e) = line_span(&st, 0, 1);
        assert_eq!(
            range_breaks_a_table(&st, s, e),
            Some(TableBreak::ProtectedRow)
        );
    }

    /// Deleting the table entirely leaves no half-table to be broken.
    #[test]
    fn deleting_the_whole_table_is_allowed() {
        let st = state_at(at("alpha"));
        let (s, e) = line_span(&st, 0, 2);
        assert_eq!(range_breaks_a_table(&st, s, e), None);
    }

    /// The hole a cursor-keyed predicate had: a selection whose cursor left the table.
    #[test]
    fn a_span_reaching_in_from_outside_the_table_still_breaks_it() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let src = format!("para\n{TABLE}");
        let mut st = EditorState::new(Buffer::from_str(&src), theme);
        st.mode = Mode::Rendered;
        st.update_cursor_block();
        // Lines 0..=1 are the paragraph and the header row; the cursor is on the former.
        let (s, e) = line_span(&st, 0, 1);
        assert_eq!(
            range_breaks_a_table(&st, s, e),
            Some(TableBreak::ProtectedRow)
        );
    }

    #[test]
    fn an_edit_inside_one_cell_is_allowed() {
        let st = state_at(at("alpha"));
        let rope = st.buffer.rope();
        let s = rope.char_to_byte(at("alpha"));
        let e = rope.char_to_byte(at("alpha") + "alpha".len());
        assert_eq!(range_breaks_a_table(&st, s, e), None);
    }

    /// A drag from one cell into the next puts the `|` inside the range.
    #[test]
    fn an_edit_across_two_cells_breaks_the_table() {
        let st = state_at(at("alpha"));
        let rope = st.buffer.rope();
        let s = rope.char_to_byte(at("alpha"));
        let e = rope.char_to_byte(at("bravo") + 2);
        assert_eq!(
            range_breaks_a_table(&st, s, e),
            Some(TableBreak::CrossesCells)
        );
    }

    /// Hand-editable within its own text — the reason it has no cell scope — but not
    /// past the newline.
    #[test]
    fn the_alignment_rows_own_text_stays_editable() {
        let st = state_at(at("|---|") + 2);
        let rope = st.buffer.rope();
        let s = rope.char_to_byte(at("|---|") + 1);
        let e = rope.char_to_byte(at("|---|") + 4);
        assert_eq!(range_breaks_a_table(&st, s, e), None);

        let (s, e) = line_span(&st, 1, 1);
        assert_eq!(
            range_breaks_a_table(&st, s, e),
            Some(TableBreak::ProtectedRow)
        );
    }

    #[test]
    fn a_range_in_ordinary_prose_never_breaks_anything() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let mut st = EditorState::new(Buffer::from_str("one\ntwo\nthree\n"), theme);
        st.mode = Mode::Rendered;
        st.update_cursor_block();
        assert_eq!(range_breaks_a_table(&st, 0, 13), None);
    }

    #[test]
    fn raw_mode_never_reports_a_break() {
        let mut st = state_at(at("alpha"));
        st.mode = Mode::Raw;
        let (s, e) = line_span(&st, 0, 1);
        assert_eq!(range_breaks_a_table(&st, s, e), None);
    }

    #[test]
    fn op_range_lines_are_measured_like_the_operator_measures_them() {
        let st = state_at(at("one"));
        assert_eq!(
            op_range_breaks_a_table(&st, &OpRange::Lines { first: 0, last: 0 }),
            Some(TableBreak::ProtectedRow)
        );
        assert_eq!(
            op_range_breaks_a_table(&st, &OpRange::Lines { first: 2, last: 2 }),
            None
        );
    }

    #[test]
    fn lines_touch_a_table_spots_any_overlap() {
        let st = state_at(at("one"));
        assert!(lines_touch_a_table(&st, 0, 0));
        assert!(lines_touch_a_table(&st, 2, 2));
        // Total cover still counts: `J` and `>>` reshape rows rather than removing them.
        assert!(lines_touch_a_table(&st, 0, 2));

        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let mut prose = EditorState::new(Buffer::from_str("one\ntwo\n"), theme);
        prose.mode = Mode::Rendered;
        prose.update_cursor_block();
        assert!(!lines_touch_a_table(&prose, 0, 1));
    }

    // ── Paste ───────────────────────────────────────────────────────────

    /// The ordinary linewise landing spot is between the header and the alignment row.
    #[test]
    fn a_row_pasted_on_the_header_lands_below_the_alignment_row() {
        let st = state_at(at("alpha"));
        let plan = table_paste_plan(
            &st,
            "| x | y |\n",
            /*linewise=*/ true,
            /*after=*/ true,
        );
        let TablePaste::RowsAt(offset) = plan else {
            panic!("expected a row insertion, got {plan:?}");
        };
        assert_eq!(
            offset,
            at("| one"),
            "the row must land on the first data row's boundary, not above the alignment row"
        );
    }

    #[test]
    fn pasting_prose_into_a_table_is_refused() {
        let st = state_at(at("one"));
        assert_eq!(
            table_paste_plan(&st, "just a paragraph\n", true, true),
            TablePaste::Refused
        );
        assert_eq!(
            table_paste_plan(&st, "a | b", false, true),
            TablePaste::Refused
        );
        assert_eq!(
            table_paste_plan(&st, "text", false, true),
            TablePaste::NotATable
        );
    }

    #[test]
    fn paste_plans_nothing_outside_a_table() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let mut st = EditorState::new(Buffer::from_str("paragraph\n"), theme);
        st.mode = Mode::Rendered;
        st.update_cursor_block();
        assert_eq!(
            table_paste_plan(&st, "| x | y |\n", true, true),
            TablePaste::NotATable
        );
    }

    #[test]
    fn insert_table_rows_lands_on_the_new_rows_first_cell() {
        let mut st = state_at(at("one"));
        let plan = table_paste_plan(&st, "| x | y |\n", true, true);
        let TablePaste::RowsAt(offset) = plan else {
            panic!("expected a row insertion, got {plan:?}");
        };
        insert_table_rows(&mut st, offset, "| x | y |\n");
        assert_eq!(
            st.buffer.contents(),
            "| alpha | bravo |\n|---|---|\n| one | two |\n| x | y |\n"
        );
        assert_eq!(
            st.buffer
                .slice_to_string(st.cursor.offset, st.cursor.offset + 1),
            "x",
            "the cursor lands on the pasted row's first cell"
        );
    }

    /// With no trailing newline, appending a row must not glue it onto the last.
    #[test]
    fn insert_table_rows_adds_its_own_separator() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let mut st = EditorState::new(Buffer::from_str("| a | b |\n|---|---|\n| 1 | 2 |"), theme);
        st.mode = Mode::Rendered;
        st.cursor.offset = 24; // on the data row
        st.update_cursor_block();
        let plan = table_paste_plan(&st, "| x | y |\n", true, true);
        let TablePaste::RowsAt(offset) = plan else {
            panic!("expected a row insertion, got {plan:?}");
        };
        insert_table_rows(&mut st, offset, "| x | y |\n");
        assert_eq!(
            st.buffer.contents(),
            "| a | b |\n|---|---|\n| 1 | 2 |\n| x | y |"
        );
    }
}
