# The vim command line

> Part of the edamame contributor deep-dives. Index and project-wide conventions: [`AGENTS.md`](../../AGENTS.md). Sibling docs live in [`docs/dev/`](.).


- **Every way a paste can reach an open `/` `?` `:` prompt goes through `App::paste_into_cmdline`.** There are two: a terminal bracketed paste (`Event::Paste`) and edamame's own paste chord, which used not to fire at all — an open command line captures every key, so the global keymap never ran and `Action::Paste` was swallowed by `feed_cmdline` (issue #17). `dispatch_single_key` now resolves *that one action* against the live keymap ahead of the vim feed — against the keymap, not a hardcoded `Ctrl-V`, so a rebound key works. Everything else stays captured.
- **The prompt is a single line, so a paste is transformed and bounded, in that order.** `cmdline::paste_str` escapes first on a *search* prompt (so a pasted break survives as `\n`) and drops any break the transform didn't consume; an `:` prompt keeps the plain strip. `PASTE_CHAR_CAP` is applied by `paste_into_cmdline` *before* handing the text over, and deliberately **not** via `ui::sanitize_paste`, which strips control characters and would eat the newlines the escape exists to preserve; the call site also keeps `input` below `ui` in the layer order. The cap is not cosmetic — `paste_str` inserts char by char through an O(n) `byte_index` scan, so an uncapped paste is quadratic (a 200 KB clipboard measured ~9 s of frozen UI).
- **A paste re-derives the live preview, exactly as typing does.** `paste_into_cmdline` calls `feed::cmdline_live_update` with the pre-paste input, so the `:s` preview and incsearch resolve against the new line; mutating `cl.input` without it leaves the preview describing text that is no longer in the prompt.

