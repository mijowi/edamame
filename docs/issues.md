# Issue/Feature Tracker

- Add next cell/prev cell to keybindings modal?

- Refactor/clean up source and tests

- Handle paragraphs containing line breaks

- Add/update performance benchmark tests

## Lists
- When a list item is deleted with Ctrl-Backspace, a new line is unintentionally added in its place, making a gap in the list
- Automatic list management is finicky: line spacing, nested lists, sub-lists of different types

## Vim mode
- Ctrl-Backspace and Ctrl-Delete modify the buffer in normal mode. They should not.
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
