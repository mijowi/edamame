# Vim Mode Feature Planning

## 1. Goal and Scope

### Goal

Add Vim-style modal editing to edamame. This is the only modal editing style
edamame targets — Helix, Kakoune, and other schemes are explicitly not goals.
The feature must:

1. Coexist with edamame's existing `Mode::Rendered` / `Mode::Raw` rendering axis. Vim sub-modes (Insert/Visual/VisualLine) are nested inside both `Rendered` and `Raw`. Vim normal mode replaces edamame's `Mode::Preview` since they are fairly similar. Normal / Insert / Visual / VisualLine live on `VimHandler`. Distinct from `EditorState::mode` (Preview / Rendered / Raw), which is the rendering axis.
2. Reuse edamame's Markdown-aware ops (list continuation, list indent, table navigation, GFM renumber).
3. Survive concurrent feature work — i.e. ship behind a config switch (`config.modal.handler = "vim"`) so default users are unaffected.

In scope:
- Modes: Normal, Insert, Visual (character-wise), Visual Line. Mode-switch keys: Esc, i, I, a, A, o, O, v, V.
- Motions: h, j, k, l, w, e, b, 0, $, ^, gg, G, f{c}, F{c}, t{c}, T{c}, ;, ,, % (matching pair), { / } (paragraph / block), n / N (search results.
- Editing primitives (Normal): x, X, dd, D, dw / de / db, cc, C, cw / ce / cb, yy, Y, yw / ye / yb, p, P, >>, <<, J, u, Ctrl-R, ., r{c}, ~.
- Text objects: iw / aw, iW / aW, i" / a", i' / a', i` / a`, i( / a( / i) / a), i[ / a[ / i] / a], i{ / a{ / i} / a}.
- Visual mode (operators on selection): d / x, y, c / s, p, r{c}, ~ / u / U, > / <, J, o (swap cursor and anchor), iw / aw / etc. (text-object selection), v / V (toggle / exit).
- Count prefixes: 3j, 5dw, 2dd, 3>>. Both [count][operator][motion] and [operator][count][motion] shapes.
- Search: /pattern, ?pattern, n, N, * (word under cursor), # (word under cursor reverse). Match highlighting in Rendered + Raw. Reuse some existing search structures, such as match highlighting and result `n/N` display alongside the mode name in the hint line — this needs further specification.
- Ex commands: :w, :q, :wq, :e <path>, :s/pattern/replacement/flags (g, i).
- Substitution: :s, :%s, with regex

Anything else, including the following, is out of scope:
- Named registers ("ay, "ap)
- Macros (q{r} recording, @{r} replay)
- Block-wise Visual mode (Ctrl-V)
- Vim’s full Ex command suite (:bn, :bp, etc. — only the subset above)
- Visual-block-specific operators (I / A / c in block mode)
- Window splits (:sp, :vsp) — edamame doesn’t need them.

## 2. Decisions already made

| Decision | Choice |
|---|---|
| Esc in Vim Insert | Transitions to Normal + cursor `MoveLeft` (vim convention). |
| Ctrl-* keymap chords | Always honored, even in Normal. (Ctrl-S, Ctrl-P, etc. fire from the keymap.) Bare keys are vim motions. Ctrl-* chords consult the keymap. |
| Markdown-aware ops | Reused. `o` continues lists; `dd` renumbers; `>>` indents list items; etc. |
| Diff mode | Not available. Editing is not possible in diff mode, so vim motions are not accessible. |
| How to treat Markdown formatting characters   |   In vim Normal/Visual/Insert, always render the cursor's line (or current block) as raw so motions/edits operate on visible characters, and define explicitly how selection, insertion, and cursor movement each treat hidden syntax.|

## Status bar / hint line

The status bar's mode badge prefers `vim_mode_label` over `mode.to_string()`.Display: `" NORMAL "` / `" INSERT "` / `" VISUAL "` / `" V-LINE "` (replaces `" EDIT " and " RAW "`). The user will almost certainly know if they are in edit or raw mode just by looking at the screen.

Vim `:` commands, which in Vim are shown in the status line, are shown in the hint line in edamame. When in normal mode and `:` is pressed to enter command mode, whatever is on the hint line is replaced with the command being entered. Once the command has completed, the hint line is restored.

## Open Questions

- Are there any vim-editing packages we can use instead of rolling our own custom implementation? Consider `tui-textarea/examples/vim.rs`.
- How will we plan this out into multiple checkpoints, each of which compile cleanly, pass all tests and can be manually sanity-checked?
- Which of edamame's default keybinds, if any, conflict with the Vim motions we intend to support?
- Goal #2 is to "Reuse edamame's Markdown-aware ops". In what specific ways can/should we do that?
- Should we implement dot (`.`) repeat?

