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

### Deferred features
- Code syntax highlighting