# Optional docked outline — design contract (2026-10-09)

## Decision

Add an **optional left-docked heading outline**, off by default. Keep the existing `Ctrl-G` fuzzy section picker unchanged. The outline shows the current document's parsed H1–H6 headings; it is not a file explorer or an alternate Markdown parser. No overlay, resize handle, folding, search field, or second persisted outline-scroll state.

## Appearance and layout

- Split only the document region horizontally: outline (22–30 terminal cells), one-cell separator, remaining document region. Keep the existing hint/status rows full width. Truncate heading labels by terminal cell width; indent by heading level. Highlight the current section separately from the keyboard-focused row; use existing theme styles, including monochrome. When no headings exist, show `(no headings)`.
- Compute candidate outline width as `min(30, max(22, terminal_width / 4))`. If the resulting document viewport (after its line-number gutter and scrollbar) has fewer than 60 cells, or fewer than three document rows remain, suppress the outline without altering the stored preference. It reappears automatically when the terminal grows. An attempted open on a narrow screen explains that `Ctrl-G` remains available.
- Max-width document centering, scrollbar, image and link geometry, cursor/scroll clamping, and mouse hit-testing all use the *same* post-split document rect. Never subtract the sidebar in only the painter or only the event loop. Turning the panel on/off or resizing the terminal recomputes wrapping and clamps scroll; do not lose the buffer or the Vim sub-mode.
- Hide the outline while diff review is active (retain the preference); read-only embedded docs use their own headings. Ordinary modals remain above both panes and own input while open.

## Keyboard and pointer contract

- `F8`: toggle panel visibility, leaving focus in the editor on open; closing from either pane returns focus to the editor. `F6`: switch editor/outline focus only while the panel is visible. `Esc` while outline-focused returns focus to the editor *without hiding* the panel. `Ctrl-G` continues opening the fuzzy heading picker.
- While outline-focused: Up/Down and Vim `j/k` move only the outline selection; Enter jumps to the selected heading and returns focus to the editor; pointer click jumps and returns focus. A wheel over the outline scrolls the outline list, not the document. Only Enter/click jumps: changing focus does not live-preview document scrolling. Vim editor keystrokes (including Insert-mode typing and pending operators) must not consume the outline keys; Normal/Insert sub-mode is preserved across focus transfer, while an in-progress operator is canceled rather than left armed against the next editor key. F8/F6 remain actionable across Vim modes except while a modal or command prompt is capturing input.
- Reading/scrolling the document updates the current-section marker and keeps it in sight unless the user is manually browsing the outline. In Preview use viewport top; in Rendered/Raw use cursor's source line. Focus and current-section indicators must be distinguishable without color.
- An actual heading jump records one existing in-document navigation entry so the normal back/forward actions restore the origin. Respect existing dirty-buffer and capture-mode gates; the outline never modifies the document and cannot steal input from an open modal.

## State and data

- Persist `[editor] show_outline = false` using the established config/settings save path. One `ToggleOutline` action is exposed in the palette, settings and keybinding config. Focus, list scroll, and highlighted entry are ephemeral App/view state, never written to config.
- Share the heading extraction used by `Ctrl-G`: `ParsedDoc::blocks` + real source ranges and `heading_plain_text`. Treat a heading's source line as its identity in the current parse. Do not persist the modal's `target_scroll`: it is specific to the width and mode at modal-open time. Resolve a clicked/selected heading against the current parse and current document width immediately before jumping. Refresh list contents on parse version or file change, keeping a sensible nearby selection.
- The section picker remains a modal with its existing debounced preview/cancel semantics; do not silently make the sidebar a second `SearchableList` modal. Use the navigation history mechanism already used for in-document anchor jumps.

## Acceptance and verification

1. F8 toggles a left pane without shifting the bottom region; F6/Esc/Enter and pointer select correctly while editor typing and Vim Normal/Insert still work.
2. Headings update after edits and document switches, including empty/duplicate/CJK headings; headings navigate correctly in Preview, Rendered and Raw at the new width, with back/forward restoring the origin.
3. Narrow terminal hides and wide terminal restores the requested outline; resizing keeps mouse, scrollbar, wrapping and image positions consistent.
4. The persisted preference survives a restart; the palette/settings/keybindings help match actual behavior; Ctrl-G still works unchanged; diff and modal interactions remain safe.
5. Run focused behavior checks, the GNU Windows Rust suite and a launched TUI smoke scenario in a real terminal or a representative terminal harness. Do not claim a visual result without observing one.
