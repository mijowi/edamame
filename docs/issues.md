# Issue/Feature Tracker

- Add next cell/prev cell to keybindings modal?

- Refactor/clean up source and tests
- Add/update performance benchmark tests

- Two separate modal content-width systems exist. The 20 `ModalChrome`-backed modals now share an opt-in cap — `ModalView::with_max_content_width` / `ModalChrome::with_max_content_width`, defaulting to size-to-content, with `PROSE_CONTENT_WIDTH` (64 columns of text) as the house value for prose bodies. The 9 overlays that bypass `ModalView` and build `ContentSize` directly (`welcome`, `settings_overlay`, `keybinds_overlay`, `search_modal`, `save_copy_modal`, `insert_table_modal`, `export_html_modal`, `diff_intro_modal`, `searchable_list`) each hand-roll their own sizing instead — e.g. `welcome` pins `const CONTENT_WIDTH = 64`, `keybinds_overlay` raises `KEYBINDS_MAX_PAD_H` to 8. Unify by lifting the cap onto `ContentSize` (where `max_pad_h` already lives) so `centered_rect_for_content` clamps for every caller and `ModalView` merely forwards to it; then the 9 bespoke overlays can drop their private constants. Touches 9 call sites, so it wants its own change.

- Clicking in a paragraph with inline code does not account for the render/raw character offset, resulting in unexpected cursor placement.

- Handle paragraphs containing line breaks

## Vim mode
- DIFF is not displayed in the status line when using vim mode. Maybe we should display it in the hint line?
- How should we treat vim motions in tables? I'm thinking we no-op some and adjust others for better UX, such as the ones below. What others should we tailor for tables? Don't change the behavior for raw mode; no elements get special treatment there. All vim motions should work as they would on raw text.
    - `o`/`O`: Add new row below/above, cursor lands on first cell in row
    - `I`/`A`/`$`/`^`: move cursor to the beginning/end of the *cell*
- When typing in a table cell that is wider than the column header, and a space character is entered,
- Conventional vim selection instead of half selection?
- `p` pastes after cursor cell instead of before

## Diff mode
- Finish diff mode plan (editing hunks in diff mode)
- Use Ctrl with diff mode keybinds in order to facilitate editing during review
- Rendered diff mode; use edamame as git difftool?
- Move diff mode status chip `n/N` to hint line?
- When entering diff mode, the first change (`1/N`) should be shown to the user, not the change closest to the document position.

