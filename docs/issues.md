# Issue/Feature Tracker

- Add next cell/prev cell to keybindings modal?

- Refactor/clean up source and tests

- Add support for dynamic cursor (keyboard, not mouse). In edit modes and UI inputs, the cursor should be a caret/vertical line. In preview and future non-edit modes like Vim normal mode, the cursor should be a block. Ensure that the block cursor and caret are separately styleable, with overrides possible for each mode/usage.

- Multiline block quote render/de-render issue

- Fix error when trying to save a path-less new buffer

## Diff mode
- Finish diff mode plan (editing hunks in diff mode)
- Use Ctrl with diff mode keybinds in order to facilitate editing during review
- Rendered diff mode; use edamame as git difftool
- Move diff mode status chip `n/N` to hint line?
- When entering diff mode, the first change (`1/N`) should be shown to the user, not the change closest to the document position.
- Update welcome modal to include diff mode
