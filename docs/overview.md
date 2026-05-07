# edamame

## Why make this
Markdown is an excellent document format. It's great for creating documents with a low or medium amount of complexity, which is the vast majority. The constraints of the language offer just enough features to cover most use cases while keeping things simple and easy to reason about.

Recently, Markdown has been thrust into the limelight as the chosen medium for working with large language models and AI agents (see e.g. Agent Skills). This makes viewing and editing Markdown files both commonplace and essential.

Lots of dedicated Markdown editors and viewers already exist (and there is always the humble text editor), but I feel most of them don't do the job very well. Browser and Electron (aka browser) apps, while great, can be slow and janky. Native apps are few. Many apps have extra features that are great but beside the point and distracting if you just want to edit Markdown. Other apps lack good support for some Markdown features that make the format much more versatile, such as tables. Many apps are good at viewing or editing but not both.

Many of these tools are great, but so far none I've tried has given me speedy, jank-less, full-featured, clean Markdown editing and viewing.

## Why a standalone app

The natural question is: why not build this as a Neovim plugin or a VSCode extension, rather than a new application?

Neovim plugins in this space already exist — `render-markdown.nvim` and `markview.nvim` both do inline Markdown rendering using virtual text and concealment. But they work within Neovim's fundamental constraint: a document is an array of text lines. You can annotate those lines with virtual text and conceal characters, but you can't replace a line's rendered output with something structurally different. That constraint is fine for syntax highlighting and decorative rendering, but it's fatal for this project's core editing model.

The defining feature here is the hybrid rendered/raw view: the document is displayed as fully rendered Markdown — styled, with proper table borders drawn using box-drawing characters — while only the active line or table cell is shown as raw text for editing. This requires total control over how every line on screen is drawn. Neovim's architecture doesn't provide that.

Table editing makes this even clearer. In this editor, a table is always shown as a rendered grid; the user edits one cell at a time, tabs between cells, and can drag to reorder rows or resize columns. In Neovim, a table is just lines of pipe characters. Approximating cell-level editing would mean intercepting Neovim's input and rendering for every table region, to the point of effectively building a separate application hosted inside Neovim — with all the complexity of that integration and none of the control of standalone.

VSCode is ruled out by the stated performance goals. A VSCode custom editor extension runs in a webview, which is Electron inside Electron — exactly the "slow and janky" category the project aims to escape.

A standalone TUI app has the smallest possible dependency footprint (just a terminal), the highest performance ceiling (direct ratatui rendering, no IPC), full control over the editing model, and works anywhere: SSH sessions, minimal Linux environments, macOS, WSL — no editor installation required.

## Why choose TUI
Terminal apps are more portable, faster, and easier to develop compared to GUI apps. Multi-platform GUI libraries aren't as performant as native libraries, and going native introduces complexity. Also, Markdown is inherently text-based, so I think it makes sense to use a TUI. 

## Features in order of implementation
0. Basic Markdown rendering and text editing, a simple file-based configuration architecture, should work on Linux (primary), macOS, and WSL
1. Combined editing and rendering—not split view. We'll have a raw mode and a rendered mode. You can still edit in the rendered mode; the line the cursor is on is shown raw for editing. If in a table, the current cell is shown raw, not the line/row. Files open in preview mode, with no cursor and no raw Markdown shown. Rendered mode is entered when user clicks or starts typing, but NOT just when scrolling.
2. Table text wrapping/column resizing. Editing tables should be frictionless. The user should never see the raw table code for the table borders, and shouldn't want to if our text wrapping is good enough.
3. Automatic numbered lists; e.g. if on a line with `2. ` pressing return should start the next line with `3. ` Smarter than just this though, e.g. starting a new blank line in the middle of a list or cutting and pasting a line in a list.
4. Compatibility detection for advanced features (e.g. mouse interaction, image display)
5. Mouse support: clickable and draggable elements, cursor placement, text selection, interactive UI elements (e.g. checkboxes)
6. Drag table rows and columns to reorder them and drag column borders to resize them.
7. Show images
8. Clickable links, including to other files
9. Status bar/menu, including file picker and settings
10. Detect file changes and prompt to reload, save a copy, or save over. Show inline diff with red deletions and green additions for comparison. Change accept/reject? This feature would specifically help with agentic workflows.

### Deferred features
Implement these later:
- Vim mode (implement this later, but ensure app is architected to support from the start). Ideally design this so that kakoune/helix or some other system could be switched to instead.
- Code syntax highlighting
- Theming: Should we architect theming from the beginning? Ideally themes can be customized in a simple config file.

## Plan 
See [plan.md](./plan.md)
