# Markdown TUI Editor

## Why make this
Markdown is an excellent document format. It's great for creating documents with a low or medium amount of complexity, which is the vast majority. The constraints of the language offer just enough features to cover most use cases while keeping things simple and easy to reason about.

Recently, Markdown has been thrust into the limelight as the chosen medium for working with large language models and AI agents (see e.g. Agent Skills). This makes viewing and editing Markdown files both commonplace and essential.

Lots of dedicated Markdown editors and viewers already exist (and there is always the humble text editor), but I feel most of them don't do the job very well. Browser and Electron (aka browser) apps, while great, can be slow and janky. Native apps are few. Many apps have extra features that are beside the point and distracting (see Obsidian) if you just want to edit Markdown. Other apps lack good support for some Markdown features that make the format much more versatile, such as tables. Many apps are good at viewing or editing but not both.

Many of these tools are great, but so far none I've tried has given me speedy, jank-less, featureful, clean Markdown editing and viewing.

## Why choose TUI
Terminal apps are more portable, faster, and easier to develop compared to GUI apps. Multi-platform GUI libraries aren't as performant as native libraries, and going native introduces complexity. Also, Markdown is inherently text-based, so I think it makes sense to use a TUI. 

## Features in order of implementation
0. Basic Markdown rendering and text editing, simple configuration file,
1. Combined editing and rendering—none of this split view stuff that wastes screen real estate. We'll have a raw mode and a rendered mode. You can still edit in the rendered mode; the line the cursor is on is shown raw for editing. If in a table, the current cell is shown raw, not the line/row.
2. Table text wrapping/column resizing. Editing tables should be frictionless. The user should never see the raw table code for the table borders, and shouldn't want to if our text wrapping is good enough.
3. Automatic numbered lists; e.g. if on a line with `2. ` pressing return should start the next line with `3. ` Smarter than just this though, e.g. starting a new blank line in the middle of a list or cutting and pasting a line in a list.
4. Compatibility detection for advanced features (e.g. mouse interaction, image display)
5. Mouse support: clickable and draggable elements, cursor placement, text selection, interactive UI elements (e.g. checkboxes)
6. Drag table rows and columns to reorder them and drag column borders to resize them.
7. Show images
8. Clickable links, including to other files
9. Status bar/menu, including file picker and settings
10. Detect file changes and prompt to reload, save a copy, or save over
---
Implement these later:
- Vim mode (implement this later, but ensure app is architected to support from the start). Ideally design this so that kakoune/helix or some other system could be switched to instead.
- Code syntax highlighting
- Theming: Should we architect theming from the beginning? Ideally themes can be customized in a simple config file.

## Plan 
- Architecture: Rust+ratatui
- Other libraries to consider: tui-markdown, md-tui, tui-textarea, rat-widget, mdttm, edtui, crossterm, ratatui-image, ratatui-code-editor, tui-syntax-highlight, tui-input, pulldown-cmark

