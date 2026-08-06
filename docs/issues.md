# Issue/Feature Tracker

- Add next cell/prev cell to keybindings modal?

- Refactor/clean up source and tests
- Add/update performance benchmark tests

- Handle paragraphs containing line breaks

- Add a newline to all the selected item hints in the settings modal, except for "Diff when file changes", which already is two lines long, so that the modal does not resize when the diff item is selected.

- Images take up full space when disabled with "never"

- In the "It looks like you're using a new terminal application" modal, we should add a quick path for the user to change their settings to accommodate the new terminal if e.g. terminal capabilities have degraded or improved. How should we do this? Add a button that links to the settings modal? Or to the welcome modal?
- This modal also incorrectly shows true color support for Apple Terminal, diverging from the welcome modal's color support detection. Do they not use the same path?
- Any time a terminal without true color support is opened, and the theme is not 256 dark or 256 light, edamame should set the theme to 256 dark (with or without changing the config file?) and show a warning modal.

## Compatibility
- macOS Apple Terminal, iTerm2

- iTerm2 has image display issues, even though it supports the kitty image protocol:
  1. Incorrectly renders images on top of modals. This was witnessed with the welcome modal
  2. iTerm2 image rendering also causes the program to stutter and lag.
  3. Images are not rendered at all (just a blank space is shown) when the image is fully visible within the editor's content area, i.e. the image is not clipped or partially visible. When the user scrolls, the image flashes with full fidelity as each line is scrolled, then disappears. Halfblocks should be rendered when the image is partially visible or when scrolling, but halfblocks are not rendered at all in iTerm2.

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
