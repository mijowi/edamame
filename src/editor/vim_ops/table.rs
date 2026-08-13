//! Table-aware vim scoping — the single place that answers "where does
//! the cursor's table cell begin and end, and which vim commands respect
//! that boundary?".
//!
//! A rendered GFM table is auto-managed chrome: the `|` delimiters and the
//! alignment row are structure, not prose.  Stock vim motions treat a table
//! row as an ordinary line, so `$` parks on the outer `|`, `w` walks into
//! the next cell, and `D` wipes an entire row's delimiters in one keystroke.
//! This module narrows the motions that should stay inside one cell, and
//! re-routes the line-oriented commands (`o`/`O`/`dd`/`cc`) onto the
//! structural `table_edit` primitives.
//!
//! **Raw mode is exempt, for free.**  Every query here funnels through
//! [`table_edit_ops::current_table`], which returns `None` in
//! [`Mode::Raw`](crate::editor::Mode::Raw).  Raw is hand-editable source:
//! the user must be able to repair a broken table one byte at a time, so
//! every vim command behaves exactly as it would on plain text there.  No
//! call site needs its own mode check.
//!
//! **One derivation of the cell bounds.**  [`cell_scope`] is the only
//! byte→char conversion of `table_edit`'s cell offsets.  The motion clamp,
//! the operator-range clamp, and `cc`'s cell-clear all read it rather than
//! re-deriving, so they cannot drift apart the way independently-computed
//! mappings elsewhere in this codebase have.
//!
//! **No bare resolver calls survive in `feed.rs`.**  [`resolve_scoped_motion`]
//! and [`resolve_scoped_op_range`] *replace* `motion::resolve_motion` /
//! `resolve_motion_range` at the input layer rather than wrapping their
//! results at each call site — so a future operator target cannot forget
//! the clamp.  `motion.rs` itself stays pure and buffer-only.  The guard is
//! `rg 'resolve_motion(_range)?\(' src/input/vim/feed.rs`, which should
//! match nothing (prose mentions of the name don't count, so don't grep for
//! the bare identifier).
//!
//! **Clamping is not the safety net — [`range_breaks_a_table`] is.**  The
//! cell clamp shapes the *common* commands, but a range can still reach a
//! protected row by a route with no cell to clamp against: `2dd`, `dj`, a
//! VisualLine selection whose cursor has left the table.  So every vim path
//! that mutates a range checks that one predicate immediately before it
//! runs, and the clamp is left to do only what it is good at — making the
//! ordinary keystroke land in the right place.
//!
//! **A charwise Visual highlight must cover only the cell's content.**  The
//! horizontal motions are therefore clamped *harder* in charwise Visual than
//! in Normal, via [`CellLimit`]: the span is inclusive of the char under the
//! cursor, so the cursor stops on the cell's last character rather than on
//! the append slot past it, and `h`/`l` step within the cell
//! ([`visual_cell_step`]) instead of hopping to the neighbouring one.  The
//! guarantee is horizontal only — `j`/`k` and the deliberately unscoped
//! document motions (`gg`, `G`, `}`) still leave the cell, and the range
//! guard is what catches those.
//!
//! Note what the two clamps protect against, because they are *different*
//! failures.  A highlight that crosses a `|` promises an edit
//! [`range_breaks_a_table`] refuses — the clamp is what keeps the highlight
//! honest.  A highlight that merely reaches the append slot is **not**
//! refused: [`table_break`] tests confinement against `Cell::content_end`,
//! the *untrimmed* span between the pipes, while [`CellScope::end`] is
//! trimmed past the last non-blank — so the padding space in between is
//! fair game to the guard, and the edit silently eats it, leaving the cell
//! abutting its delimiter.  Cosmetic rather than structural, and the
//! one-grapheme pull-back is also just what vim does (`$` in Visual rests
//! on the last character), but don't reach for the guard to explain it.

use std::ops::Range;

use crate::document::{next_grapheme_offset, prev_grapheme_offset, EditDelta};
use crate::editor::edit_ops::cursor_byte;
use crate::editor::table_edit::{self, RowKind, TableInfo, TableRow};
use crate::editor::table_edit_ops;
use crate::editor::vim_ops::motion::{resolve_motion, resolve_motion_range, Motion, OpRange};
use crate::editor::vim_ops::operator::{execute_operator, OpResult, Operator};
use crate::editor::EditorState;

/// The char-offset content bounds of the table cell the cursor sits in.
///
/// `start` is the cell's first content column (the padding space after `|`
/// is skipped) and `end` is the append position just past the last
/// non-whitespace character — the same two anchors `table_move_horizontal`
/// clamps `h`/`l` to, so a clamped motion can never land somewhere the
/// existing cell-stepping wouldn't.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellScope {
    pub start: usize,
    pub end: usize,
}

impl CellScope {
    /// Is `offset` inside this cell's content (bounds inclusive)?
    fn contains(&self, offset: usize) -> bool {
        offset >= self.start && offset <= self.end
    }

    /// This scope as a half-open char range.
    fn as_range(&self) -> Range<usize> {
        self.start..self.end
    }
}

/// How far right inside a cell the *cursor* may come to rest.
///
/// The two answers differ by exactly one grapheme, and which one is right
/// depends on whether the cursor's own position is part of a highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellLimit {
    /// Up to [`CellScope::end`], the append position past the cell's last
    /// character.  This is where `$` parks in Normal and where an
    /// exclusive-end operator target belongs — nothing is highlighted, and
    /// "type here" is a legitimate place to be.
    Append,
    /// Up to the cell's last character.  A charwise Visual span is
    /// *inclusive* of the char under the cursor
    /// (`visual::visual_charwise_range`), so a cursor on the append slot
    /// highlights the padding space before the `|` and hands the operator a
    /// range that eats it — `d` leaves the cell's content abutting the
    /// delimiter, `r` overwrites the space outright.  Not a refusal:
    /// [`table_break`] measures confinement against the *untrimmed* cell
    /// span, so that padding is inside the cell as far as the guard is
    /// concerned.  Stopping one grapheme short keeps the highlight over
    /// content only — and is what vim's own `$` does in Visual.
    LastChar,
}

/// The furthest offset a cursor may occupy in `scope` under `limit`.
/// Never below `scope.start`, so an empty cell collapses to its one slot.
fn cell_max_cursor(state: &EditorState, scope: CellScope, limit: CellLimit) -> usize {
    match limit {
        CellLimit::Append => scope.end,
        CellLimit::LastChar => prev_grapheme_offset(&state.buffer, scope.end).max(scope.start),
    }
}

// ── Queries ─────────────────────────────────────────────────────────────────

/// The cursor's cell bounds, in char offsets.
///
/// `None` outside a table, in Raw mode, and — deliberately — on the
/// alignment row (`|---|---|`).  That row is a structural artefact the user
/// edits by hand when a table's alignment needs changing, so it keeps plain
/// line semantics; this mirrors [`table_edit_ops::table_move_horizontal`],
/// which likewise declines to cell-step there.
pub fn cell_scope(state: &EditorState) -> Option<CellScope> {
    cell_scope_at(state, state.cursor.offset)
}

/// [`cell_scope`] for a position other than the cursor's — the Visual
/// *anchor*, which sits in its own cell and so needs its own bounds.
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

/// The [`RowKind`] of the row the cursor is on, or `None` outside a table.
///
/// Unlike [`cell_scope`] this *does* answer for the alignment row — `dd`
/// must refuse there, even though motions stay unscoped.
pub fn cursor_row_kind(state: &EditorState) -> Option<RowKind> {
    let info = table_edit_ops::current_table(state)?;
    let byte = cursor_byte(state);
    let (row, _) = table_edit::cursor_cell(&info, byte)?;
    info.rows.get(row).map(|r| r.kind)
}

// ── The structural guard ────────────────────────────────────────────────────

/// Why an edit can't run as asked.  Carried back so the caller can flash the
/// reason that actually applies rather than one generic refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableBreak {
    /// The edit takes out a header or alignment row while leaving the rest
    /// of the table standing — the survivors would reparse as paragraph text.
    ProtectedRow,
    /// The edit reaches across a cell boundary, so the row's `|` delimiters
    /// (or a row's newline) are inside the range.
    CrossesCells,
}

impl TableBreak {
    /// The message to flash for this refusal.
    pub fn message(self) -> &'static str {
        match self {
            TableBreak::ProtectedRow => "Can't remove a table's header or alignment row",
            TableBreak::CrossesCells => "Can't edit across table cells",
        }
    }
}

/// Would mutating the byte range `start..end` leave a *broken* table behind?
///
/// This is the single structural guard for the vim mutation paths, and it
/// answers for a range rather than for the cursor, because that is the only
/// way to catch the routes that reach a protected row without the cursor
/// sitting on one — `2dd`, `dj`, a VisualLine selection whose cursor has
/// moved out of the table.
///
/// What is *allowed*, and why:
///   * a range that swallows a table whole — deleting an entire table is a
///     legitimate edit, not corruption;
///   * a range that covers only complete `Data` rows — that is `dd`;
///   * a range confined to one cell's content, delimiters untouched;
///   * a range confined to the alignment row's own text, which stays
///     hand-editable so a user can retype the dashes.
///
/// Everything else breaks something and is refused.
pub fn range_breaks_a_table(state: &EditorState, start: usize, end: usize) -> Option<TableBreak> {
    let rope = state.buffer.rope();
    let len = rope.len_bytes();
    let start = start.min(len);
    let end = end.min(len).max(start);

    // Sweep the range table by table.  Most probes land on ordinary prose
    // and cost one `is_table_line` test, and a probe that does find a table
    // jumps straight past it, so a whole-document selection stays linear.
    let mut probe = start;
    loop {
        match table_edit_ops::table_at(state, probe) {
            Some(info) => {
                if let Some(reason) = table_break(&info, start, end) {
                    return Some(reason);
                }
                if info.end <= probe {
                    break; // no forward progress is possible — bail out
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

/// [`range_breaks_a_table`] for the range an operator is about to run over.
/// The linewise arm mirrors `execute_operator`'s own expansion — first line
/// start through the line *after* `last` — so the guard sees exactly the
/// bytes the operator would remove.
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

/// Does the inclusive buffer-line span `first..=last` touch any table at all?
///
/// The blunter question [`range_breaks_a_table`] can't answer, for the
/// commands that reshape lines without deleting them: `J` merges two rows
/// into one malformed line and `>>` / `<<` indents a row out of its block,
/// so *any* overlap with a table is a refusal — including one that covers
/// the table completely.
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

/// Would this range break `info` specifically?  See
/// [`range_breaks_a_table`] for the policy each branch implements.
fn table_break(info: &TableInfo, start: usize, end: usize) -> Option<TableBreak> {
    // The whole table is inside the range: there is nothing left to break.
    if start <= info.start && end >= info.end {
        return None;
    }
    // Rows the range reaches.  The `start + 1` keeps an empty range (an `x`
    // that covers nothing) attached to the row it sits in.
    let touched: Vec<&TableRow> = info
        .rows
        .iter()
        .filter(|r| r.start < end.max(start + 1) && r.end > start)
        .collect();
    if touched.is_empty() {
        return None;
    }
    // Complete data rows only — an ordinary row deletion.
    if touched
        .iter()
        .all(|r| r.kind == RowKind::Data && r.start >= start && r.end <= end)
    {
        return None;
    }
    if let [row] = touched[..] {
        let confined = if row.kind == RowKind::Alignment {
            // Hand-editable, but only within its own text: a range running
            // off the end of the line takes the newline with it.
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
    // Whole rows that aren't all data → a protected row is going away.
    // Anything else slices through the row's structure.
    if touched.iter().all(|r| r.start >= start && r.end <= end) {
        Some(TableBreak::ProtectedRow)
    } else {
        Some(TableBreak::CrossesCells)
    }
}

// ── Scoped motion resolution ────────────────────────────────────────────────

/// Whether `motion` is confined to the cursor's table cell.
///
/// The scoped set is everything that reads as "move within this piece of
/// text": the char steps, the word motions, the line-anchor motions, and the
/// character finds.  Deliberately excluded are the motions whose entire
/// purpose is to *leave* the current context — `gg` / `G` / `{count}G`
/// (document), `{` / `}` (paragraph, i.e. jump clear of the table) and `%`
/// (bracket matching, which is about pairing, not layout).  A new `Motion`
/// variant defaults to unscoped and must be added here consciously;
/// `cell_scoped_motions_match_the_spec` pins the classification both ways.
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

/// Confine an already-resolved motion `target` to the cursor's cell, up to
/// `limit` (see [`CellLimit`] — `Append` for a Normal-mode move, `LastChar`
/// in charwise Visual).
///
/// A no-op outside a table, on the alignment row, or for an unscoped
/// motion.  Two different failure shapes, because the motions mean
/// different things when they overshoot:
///
///   * `f` / `t` / `;` / `,` **fail**.  A find whose target lives in
///     another cell is a find with no match, and vim leaves the cursor
///     untouched on a failed find — landing on the cell edge instead would
///     silently pretend the search succeeded.
///   * everything else **clamps**.  `$` on a long cell, `w` past the last
///     word: the user asked to travel as far as this direction goes, and
///     the cell edge is now as far as it goes.
///
/// Exposed for the `;` / `,` replay path, which resolves its own target
/// through `resolve_find_repeat` rather than `resolve_motion`.
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

/// Resolve `motion` from the cursor, confined to the cursor's table cell.
/// The drop-in replacement for `motion::resolve_motion` at the input layer;
/// identical to it outside a table.
pub fn resolve_scoped_motion(
    state: &EditorState,
    motion: Motion,
    count: u32,
    limit: CellLimit,
) -> usize {
    let target = resolve_motion(motion, count, state.cursor.offset, &state.buffer);
    scope_offset(state, motion, target, limit)
}

/// `h` / `l` in charwise Visual: one grapheme step held inside the cursor's
/// cell.  Returns `false` when there is no cell to hold it in (outside a
/// table, in Raw mode, on the alignment row) so the caller falls back to the
/// ordinary cell-to-cell step.
///
/// Stepping cell to cell is right in Normal — it is how you cross a table —
/// but in charwise Visual it grows the highlight over the `|` between the
/// two cells, and [`range_breaks_a_table`] then refuses the edit that
/// highlight just promised.  So `l` stops on the cell's last character
/// ([`CellLimit::LastChar`]) and `h` on its first.  The last-character stop
/// (rather than the append slot) is the separate, milder concern documented
/// on [`CellLimit::LastChar`].
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
    // `.max(cursor)` for a cursor that entered Visual already parked on the
    // append slot: a forward step must not drag it *backwards*.
    let max = cell_max_cursor(state, scope, CellLimit::LastChar).max(cursor);
    state.cursor.offset = target.clamp(scope.start, max);
    state.cursor.preferred_col = state.cursor.cell_col(&state.buffer);
    true
}

/// Pull `offset` — an endpoint a charwise Visual span is about to be built
/// from — back off its cell's append slot onto the cell's last character,
/// so the very first highlight already covers content instead of the
/// padding space before the `|`.  `None` when `offset` is not in a cell,
/// meaning "leave it exactly where it is".
///
/// Both ends need this, and each against its own cell: `v` opens a span
/// whose two ends are the cursor, but `V`→`v` inherits a linewise anchor
/// and cursor that may sit in different cells — and either of them may have
/// been parked on an append slot by `$`.
pub fn visual_endpoint_in_cell(state: &EditorState, offset: usize) -> Option<usize> {
    let scope = cell_scope_at(state, offset)?;
    if !scope.contains(offset) {
        return None;
    }
    Some(offset.min(cell_max_cursor(state, scope, CellLimit::LastChar)))
}

/// Resolve `motion` as an operator target, confined to the cursor's table
/// cell.  The drop-in replacement for `motion::resolve_motion_range`.
///
/// `OpRange::Lines` spans pass through untouched — no cell-scoped motion
/// produces one, and the linewise targets (`dj`, `dgg`) are meant to leave
/// the row.  A failed find yields an empty range at the cursor, so `df(`
/// for a `(` in the next cell deletes nothing rather than eating up to the
/// cell edge.
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

/// What a doubled operator (`dd` / `cc`) did with its table interpretation,
/// so the caller can pick an outcome.  Shared by [`delete_table_row`] and
/// [`clear_table_cell`]: both have the same three answers, and keeping them
/// on one enum is what stops a caller from collapsing "in a table, refused"
/// into "not a table, fall through" — the bug that let `cc` blank the
/// alignment row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableOpOutcome {
    /// The edit ran; fold the [`OpResult`] to fill the register.
    Applied(OpResult),
    /// The cursor is on a row this edit must not destroy — the caller
    /// flashes `reason`.
    Refused(TableBreak),
    /// Not in a table (or in Raw mode): the caller falls back to the
    /// ordinary linewise behavior.
    NotATable,
}

/// `o` / `O` inside a table: insert a structural row below / above and land
/// on its first cell.  Returns `false` outside a table so the caller falls
/// back to `open_list_continue` / a plain newline.
///
/// A bare `\n` here would split the row in two and break the table, which
/// is what stock vim's `o` does today.
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

/// `dd` inside a table: remove the whole row structurally, refusing on the
/// header and alignment rows (which carry the table's shape — losing either
/// turns the remaining rows back into paragraph text).
///
/// The deleted row's raw text goes to the unnamed register as a linewise
/// yank, so `dd` then `p` moves a row exactly as it does for an ordinary
/// line.
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

/// `cc` inside a table: clear the cursor's *cell* and enter Insert.
///
/// Vim's `cc` changes a line; the cell is the table's equivalent unit, and
/// clearing the raw line would take the row's `|` delimiters with it.
/// Routed through `execute_operator` so the single-delta / register /
/// enter-Insert contract is the existing one rather than a second copy.
///
/// Refuses on the alignment row rather than falling through.  That row has
/// no cell scope (it stays hand-editable, so motions and `x` are unscoped
/// there), but a plain linewise `cc` would blank the line that defines the
/// table's shape — the very thing `dd` refuses to do one row up.
pub fn clear_table_cell(state: &mut EditorState) -> TableOpOutcome {
    if table_edit_ops::current_table(state).is_none() {
        return TableOpOutcome::NotATable;
    }
    let Some(scope) = cell_scope(state) else {
        // In a table but with no cell: the alignment row.
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
    /// Insert the register at this char offset — a legal row boundary,
    /// which is not the same as the one the ordinary linewise paste picks.
    RowsAt(usize),
    /// The register can't land here without breaking the table.
    Refused,
    /// Not in a table: use the ordinary paste path unchanged.
    NotATable,
}

/// Decide how `p` / `P` should behave with the cursor inside a table.
///
/// Two hazards, both reachable from the register `dd` now fills:
///   * a linewise paste lands "after the cursor's line", which between the
///     header and the alignment row inserts a data row above the row that
///     declares the columns — so the target index is clamped below the
///     alignment row, exactly as [`table_edit::insert_row`] does;
///   * a register that isn't made of table rows (or a charwise one carrying
///     a `|` or a newline) splits the row it lands in, so it is refused.
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
        // A charwise register just widens the cell — unless it carries
        // structure of its own.
        return if text.contains('|') || text.contains('\n') {
            TablePaste::Refused
        } else {
            TablePaste::NotATable
        };
    }
    if !text
        .lines()
        .all(|l| l.trim().is_empty() || table_edit::is_table_line(l))
    {
        return TablePaste::Refused;
    }
    let byte = cursor_byte(state);
    let Some((row_idx, _)) = table_edit::cursor_cell(&info, byte) else {
        return TablePaste::NotATable;
    };
    let target = if after { row_idx + 1 } else { row_idx };
    // Never above the alignment row: a data row there would be read as part
    // of the header block and the table would lose its shape.
    let target = target.clamp(2, info.rows.len());
    let byte_at = if target < info.rows.len() {
        info.rows[target].start
    } else {
        info.end
    };
    TablePaste::RowsAt(state.buffer.rope().byte_to_char(byte_at))
}

/// Insert `text` as whole table rows at char offset `at` (from
/// [`table_paste_plan`]) and land the cursor on the new row's first cell.
///
/// Adds the separating newline itself when `at` is the end of a table that
/// has no trailing one, so appending to the last row can't glue the pasted
/// row onto it.
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

    /// Char offset of the first occurrence of `needle` in [`TABLE`].
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
        // The alignment row stays hand-editable, so no cell scoping there.
        let st = state_at(at("|---|") + 2);
        assert!(cell_scope(&st).is_none());
        // …but it is still a known row kind, so `dd` can refuse on it.
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

    /// Raw mode must behave exactly like plain text — every table query
    /// short-circuits through `current_table`, which is `None` there.
    #[test]
    fn raw_mode_has_no_cell_scope() {
        let mut st = state_at(at("alpha"));
        st.mode = Mode::Raw;
        assert!(cell_scope(&st).is_none());
        assert!(cursor_row_kind(&st).is_none());
        // …so a scoped motion resolves identically to the bare resolver.
        let scoped = resolve_scoped_motion(&st, Motion::LineEnd, 1, CellLimit::Append);
        let bare = resolve_motion(Motion::LineEnd, 1, st.cursor.offset, &st.buffer);
        assert_eq!(scoped, bare);
    }

    /// The classification is the whole contract of the clamp — pin it in
    /// both directions so a new `Motion` variant can't silently join (or
    /// miss) the scoped set.
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
        // Bare `w` would cross the `|` into `bravo`.
        let bare = resolve_motion(Motion::WordForward, 1, st.cursor.offset, &st.buffer);
        assert!(bare > at("alpha") + "alpha".len());
        let scoped = resolve_scoped_motion(&st, Motion::WordForward, 1, CellLimit::Append);
        assert_eq!(scoped, at("alpha") + "alpha".len());
    }

    /// A find whose target is in another cell is a *failed* find — vim
    /// leaves the cursor put rather than moving it partway.
    #[test]
    fn find_outside_the_cell_does_not_move_the_cursor() {
        let st = state_at(at("alpha"));
        let motion = Motion::FindChar('b', FindKind::Forward);
        assert_eq!(
            resolve_scoped_motion(&st, motion, 1, CellLimit::Append),
            st.cursor.offset
        );
        // And as an operator target it covers nothing at all.
        assert_eq!(
            resolve_scoped_op_range(&st, motion, 1),
            OpRange::Chars(st.cursor.offset..st.cursor.offset)
        );
    }

    /// A find *within* the cell still works normally.
    #[test]
    fn find_inside_the_cell_still_resolves() {
        let st = state_at(at("alpha"));
        let motion = Motion::FindChar('h', FindKind::Forward);
        assert_eq!(
            resolve_scoped_motion(&st, motion, 1, CellLimit::Append),
            at("alpha") + 3
        );
    }

    /// `D` (and `C`) must stop at the cell's content end so the row's `|`
    /// delimiters survive.
    #[test]
    fn line_end_op_range_stops_at_the_cell_edge() {
        let st = state_at(at("alpha"));
        assert_eq!(
            resolve_scoped_op_range(&st, Motion::LineEnd, 1),
            OpRange::Chars(at("alpha")..at("alpha") + "alpha".len())
        );
    }

    /// `x` on the last character of a cell deletes that character; one step
    /// further right it covers nothing rather than eating the `|`.
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

    /// Under `LastChar` the cursor stops one grapheme short of the append
    /// slot, so the inclusive charwise span ends on the cell's content.
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

    /// The limit is a whole grapheme back, not a char back — a combining
    /// sequence at the cell end is one selectable unit.
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
        // Right, over and over: never past the last content char.
        for _ in 0..9 {
            assert!(visual_cell_step(&mut st, /*forward=*/ true));
        }
        assert_eq!(st.cursor.offset, at("alpha") + "alpha".len() - 1);
        // …and back, never before the first.
        for _ in 0..9 {
            assert!(visual_cell_step(&mut st, /*forward=*/ false));
        }
        assert_eq!(st.cursor.offset, at("alpha"));
    }

    /// A cursor that entered Visual on the append slot must not be dragged
    /// backwards by a forward step.
    #[test]
    fn visual_cell_step_forward_never_moves_backwards() {
        let mut st = state_at(at("alpha") + "alpha".len());
        assert!(visual_cell_step(&mut st, /*forward=*/ true));
        assert_eq!(st.cursor.offset, at("alpha") + "alpha".len());
    }

    /// No cell to hold the step in → the caller falls back to the ordinary
    /// cell-to-cell move.
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
        // Anywhere inside the content it leaves the offset alone.
        let st = state_at(at("alpha") + 2);
        assert_eq!(
            visual_endpoint_in_cell(&st, st.cursor.offset),
            Some(at("alpha") + 2)
        );
        // And it declines where there is no cell.
        let st = state_at(at("|---|") + 2);
        assert_eq!(visual_endpoint_in_cell(&st, st.cursor.offset), None);
    }

    /// The endpoint is resolved against *its own* cell, not the cursor's —
    /// what `V`→`v` needs for an anchor left in a different cell.
    #[test]
    fn visual_endpoint_answers_for_a_cell_the_cursor_is_not_in() {
        let st = state_at(at("alpha"));
        let bravo_append = at("bravo") + "bravo".len();
        assert_eq!(
            visual_endpoint_in_cell(&st, bravo_append),
            Some(bravo_append - 1)
        );
        // Including a cell on another row.
        let two_append = at("two") + "two".len();
        assert_eq!(
            visual_endpoint_in_cell(&st, two_append),
            Some(two_append - 1)
        );
    }

    /// The operator range is exclusive-ended, so it keeps the `Append`
    /// bound — `D` in a cell must still clear the whole content.
    #[test]
    fn the_operator_range_keeps_the_append_bound() {
        let st = state_at(at("alpha"));
        assert_eq!(
            resolve_scoped_op_range(&st, Motion::LineEnd, 1),
            OpRange::Chars(at("alpha")..at("alpha") + "alpha".len())
        );
    }

    /// Unscoped motions keep crossing the table freely.
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
        // The header and alignment rows survive.
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
        // The delimiters, the sibling cell, and the data row are untouched.
        assert!(st.buffer.contents().starts_with("|  | bravo |\n"));
        assert!(st.buffer.contents().contains("| one | two |"));
    }

    /// The alignment row has no cell scope, but `cc` there would blank the
    /// row that declares the table's columns — the same loss `dd` refuses.
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
        // The new row carries the table's delimiters, not a bare blank line.
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

    /// The byte range of buffer lines `first..=last`, the way a linewise
    /// operator would take them (through the newline that ends `last`).
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

    /// `2dd` on the header: the count is what reaches the protected rows,
    /// and the cursor's own row being a header is not what the guard keys
    /// on — the span is.
    #[test]
    fn a_counted_span_over_protected_rows_breaks_the_table() {
        let st = state_at(at("alpha"));
        let (s, e) = line_span(&st, 0, 1);
        assert_eq!(
            range_breaks_a_table(&st, s, e),
            Some(TableBreak::ProtectedRow)
        );
    }

    /// Deleting the table entirely is a legitimate edit — there is no
    /// half-table left behind to be broken.
    #[test]
    fn deleting_the_whole_table_is_allowed() {
        let st = state_at(at("alpha"));
        let (s, e) = line_span(&st, 0, 2);
        assert_eq!(range_breaks_a_table(&st, s, e), None);
    }

    /// The hole the cursor-keyed predicate had: a VisualLine selection
    /// anchored on the header whose cursor has moved up out of the table.
    #[test]
    fn a_span_reaching_in_from_outside_the_table_still_breaks_it() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let src = format!("para\n{TABLE}");
        let mut st = EditorState::new(Buffer::from_str(&src), theme);
        st.mode = Mode::Rendered;
        st.update_cursor_block();
        // Lines 0..=1 are the paragraph and the table's header row; the
        // cursor sits on the paragraph, outside the table entirely.
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

    /// A charwise Visual drag from one cell into the next: the `|` between
    /// them is inside the range.
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

    /// The alignment row stays hand-editable within its own text — that is
    /// the whole reason it has no cell scope — but not past the newline.
    #[test]
    fn the_alignment_rows_own_text_stays_editable() {
        let st = state_at(at("|---|") + 2);
        let rope = st.buffer.rope();
        let s = rope.char_to_byte(at("|---|") + 1);
        let e = rope.char_to_byte(at("|---|") + 4);
        assert_eq!(range_breaks_a_table(&st, s, e), None);

        // …but a range running off its end takes the newline with it.
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
        // Covering the table completely still counts: `J` and `>>` reshape
        // the rows rather than removing them.
        assert!(lines_touch_a_table(&st, 0, 2));

        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let mut prose = EditorState::new(Buffer::from_str("one\ntwo\n"), theme);
        prose.mode = Mode::Rendered;
        prose.update_cursor_block();
        assert!(!lines_touch_a_table(&prose, 0, 1));
    }

    // ── Paste ───────────────────────────────────────────────────────────

    /// `dd` a data row, move to the header, `p`: the ordinary linewise
    /// landing spot is between the header and the alignment row.
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
        // A charwise register carrying its own delimiter would split the cell.
        assert_eq!(
            table_paste_plan(&st, "a | b", false, true),
            TablePaste::Refused
        );
        // Ordinary charwise text just widens the cell.
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

    /// A table with no trailing newline: appending a row must not glue it
    /// onto the last one.
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
