# Issue/Feature Tracker

- Add next cell/prev cell to keybindings modal?

- Refactor/clean up source and tests

- We have a lot of true/false and triple ask/always/never settings throughout edamame. True/false options are currently displayed as `[ true ] [ false ]` or as `[x]`/[ ]`. I think we should 1) unify these two different UX patterns into one, and 2) improve on the design. We could possibly implement something that looks like a slider widget instead, with a label. Any ideas for how to improve the triple-option settings as well?

- Make these changes to the settings overlay modal:
1. Alphabetize settings items
2. Remove hint duration — this can be config file-only
3. Add "Blink cursor". The hint says "Blink cursor every N ms", which reads from the config file (not hardcoded)
4. Flip `Show remote images` to Never and disable it when `Show images` is set to Never. Double check that this changes matches the behavior in the welcome modal.
5. Remove "Export inlined images" and "Export diagrams as SVG". We'll move them to a modal shown during the export flow.
6. Any other recommendations for changes to make or settings to add to this modal? The idea is that users don't have to reach far for settings they are most likely to change.

- Nested lists move when de-rendering

- Implement HTML export. Add an export modal with options including "Export inlined images as data:URIs or leave as link" and "Export diagrams as inlined SVG or leave as code" (improve the wording).

## Diff mode
- Finish diff mode plan (editing hunks in diff mode)
- Use Ctrl with diff mode keybinds in order to facilitate editing during review
- Rendered diff mode; use edamame as git difftool?
- Move diff mode status chip `n/N` to hint line?
- When entering diff mode, the first change (`1/N`) should be shown to the user, not the change closest to the document position.
- Update welcome modal to include diff mode
