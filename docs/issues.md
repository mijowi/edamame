# Issue/Feature Tracker

- Add next cell/prev cell to keybindings modal?

- Refactor/clean up source and tests

- Add support for dynamic cursor (keyboard, not mouse). In edit modes and UI inputs, the cursor should be a caret/vertical line. In preview and future non-edit modes like Vim normal mode, the cursor should be a block. Ensure that the block cursor and caret are separately styleable, with overrides possible for each mode/usage.

- Selection should not be disabled during find/replace
- Exiting find/replace should keep the editor at its current scroll location, not jump back to where it was prior to the find.

- Multiline block quote render/de-render issue

- Incorrect selection paint when search result is in a multiline table cell

- Accept pasted text into modals with text input (e.g. search, command palette) and vim command line

- Fix error when trying to save a path-less new buffer

- Add a contextual hint to navigate back/forward between files when the file history stack is not empty.

## Diff mode
- Finish diff mode plan (editing hunks in diff mode)
- Use Ctrl with diff mode keybinds in order to facilitate editing during review
- Rendered diff mode; use edamame as git difftool
- Move diff mode status chip `n/N` to hint line?
- When entering diff mode, the first change (`1/N`) should be shown to the user, not the change closest to the document position.
- Update welcome modal to include diff mode
