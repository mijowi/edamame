# Issue/Feature Tracker

- Add next cell/prev cell to keybindings modal?

- Refactor/clean up source and tests

- Handle lines containing line breaks

- Automatic list management is finicky: line spacing, nested lists, sub-lists of different types

- Add/update performance benchmark tests

- Paint search result selection as it's typed, like `:s` 
- Don't paint until 2 or more characters are entered, or until user presses Enter?
- Performance degradation when search results are highlighted in long documents

## Vim issues
- DIFF is not displayed in the status line when using vim mode. Maybe we should display it in the hint line?
- How should we treat vim motions in tables? I'm thinking we no-op some and adjust others, such as these:
    - `o`/`O`: Add new row below/above
    - `I`/`A`/`$`/`^`: move to the beginning/end of the *cell*
- When typing in a table cell that is wider than the column header, and a space character is entered,
- Conventional vim selection instead of half selection?
- `p` pastes after cursor cell instead of before

## Diff mode
- Finish diff mode plan (editing hunks in diff mode)
- Use Ctrl with diff mode keybinds in order to facilitate editing during review
- Rendered diff mode; use edamame as git difftool?
- Move diff mode status chip `n/N` to hint line?
- When entering diff mode, the first change (`1/N`) should be shown to the user, not the change closest to the document position.
