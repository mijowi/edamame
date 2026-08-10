# Issue/Feature Tracker

- Add next cell/prev cell to keybindings modal?

- Refactor/clean up source and tests
- Add/update performance benchmark tests

- Two separate modal content-width systems exist. The 20 `ModalChrome`-backed modals now share an opt-in cap — `ModalView::with_max_content_width` / `ModalChrome::with_max_content_width`, defaulting to size-to-content, with `PROSE_CONTENT_WIDTH` (64 columns of text) as the house value for prose bodies. The 9 overlays that bypass `ModalView` and build `ContentSize` directly (`welcome`, `settings_overlay`, `keybinds_overlay`, `search_modal`, `save_copy_modal`, `insert_table_modal`, `export_html_modal`, `diff_intro_modal`, `searchable_list`) each hand-roll their own sizing instead — e.g. `welcome` pins `const CONTENT_WIDTH = 64`, `keybinds_overlay` raises `KEYBINDS_MAX_PAD_H` to 8. Unify by lifting the cap onto `ContentSize` (where `max_pad_h` already lives) so `centered_rect_for_content` clamps for every caller and `ModalView` merely forwards to it; then the 9 bespoke overlays can drop their private constants. Touches 9 call sites, so it wants its own change.

- Handle paragraphs containing line breaks

- Add a documentation page for the terminal upgrade notice and a link to the page from within edamame

## Vim mode
- Table scoping is in (`vim_ops::table`): cell-scoped motions, `o`/`O` as structural rows, `dd` as a row, `cc` as a cell, and a `range_breaks_a_table` guard on both operator funnels. Raw mode is exempt throughout. Open questions left over from it:
    - Which *other* motions should be tailored for tables? `{`/`}`/`gg`/`G`/`%` deliberately stay unscoped (they exist to leave the current context); everything else that means "move within this text" is clamped to the cell.
    - A charwise Visual selection can still be *made* across cells (`l` steps cell to cell) — it is the mutation that refuses, not the selection. Should `v`+`l` stop at the cell edge instead, so the highlight can never promise an edit that will be refused?
    - `p` inside a table now inserts whole rows at a legal boundary and refuses a register that isn't rows. It does not yet reflow a pasted row whose column count differs from the table's.
- When typing in a table cell that is wider than the column header, and a space character is entered,

## Diff mode
- ==DIFF is not displayed in the status line when using vim mode. The vim mode (e.g. NORMAL) supersedes it. We should supress the display of the vim mode during diffs.==
- Implement editing while in diff mode - This will be complicated - do I actually want to do it? If yes, use Ctrl with diff mode keybinds so they don't conflict with editor keybinds.
- Rendered diff mode; use edamame as git difftool?
- Add up/down arrow glyphs to indicate old version/new version changes

