# Issue/Feature Tracker

- Add next cell/prev cell to keybindings modal?

- Refactor/clean up source and tests

- Nested lists move horizontally when de-rendering

- Add "Insert image" to command palette

- Implement fixes/mitigations for vulnerabilities identified in the security review.

- Add/update performance benchmark tests

## Vim issues
- `Tab` table cell navigation is swallowed in vim normal mode (`Shift-Tab` works)
- How should we treat vim motions in tables? Disable any that don't work between words? e.g. disable `o`, `O`. `A`, `I`, `$`, `^` should act on the *cell*?
- When typing in a table cell that is wider than the column header, and a space character is entered,
- Conventional vim selection instead of half selection?
- `p` pastes after cursor cell instead of before
- `a` at the end of a line starts typing on the following line — puts the cursor after the newline character
- Add a selection paint *flash* to `yy` to signal the yank operation succeeded

## Diff mode
- Finish diff mode plan (editing hunks in diff mode)
- Use Ctrl with diff mode keybinds in order to facilitate editing during review
- Rendered diff mode; use edamame as git difftool?
- Move diff mode status chip `n/N` to hint line?
- When entering diff mode, the first change (`1/N`) should be shown to the user, not the change closest to the document position.
