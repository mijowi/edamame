# Issue/Feature Tracker

- Add next cell/prev cell to keybindings modal?

- Refactor/clean up source and tests

- Nested lists move horizontally when de-rendering

- Implement HTML export. Add an export modal with options including "Export inlined images as data:URIs or leave as link" and "Export diagrams as inlined SVG or leave as code" (improve the wording).

- The editor cursor is hidden when the terminal window loses focus, but the cursor is still shown in modals. Thread window focus into modals as well.

- Zenburn green more green and bg darker
- Nord green more green and bg darker

- Add note to "Entering diff mode" modal that says diff mode can be disabled in settings. Should we just add the option to disable diff mode in the intro modal?
- Update welcome modal feature notes to include diff mode

- Make "Create custom theme" modal searchable, just like the theme picker

## Security
- Ensure everything necessary is sanitized to prevent code execution, namely mermaid code blocks and remote images. Anywhere else?
- Are there other security concerns?

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
