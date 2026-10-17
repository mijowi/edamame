# Helix mode

This initial version provides **basic selection-first editing with one selection**: Normal/Insert/Select, common movements, selection actions, registers, undo/redo, literal search and a small set of file commands. The key tables on this page define the supported Helix commands.

## Scope and limitations

Many native Helix commands and prefix modes are unavailable. In particular, text objects such as `mi(` and `ma"`, matching/surround commands, and multiple selections are not implemented. See [Unsupported Helix commands](#unsupported-helix-commands) for the other missing command families and [Future multiple selections](#future-multiple-selections) for the next-stage goal.

**An unrecognized prefix does not capture the rest of a sequence.** In Normal/Select, an unsupported ordinary key is consumed by itself; subsequent keys are handled normally. For example, starting with `abc` and its first `a` selected:

| Native Helix sequence | Current edamame behavior |
| --- | --- |
| `mi(` | `m` is ignored, `i` enters Insert, and `(` is inserted: `(abc` |
| `ma"` | `m` is ignored, `a` enters Insert after the selection, and `"` is inserted: `a"bc` |
| `md"` | `m` is ignored, `d` deletes the current selection, and `"` is ignored: `bc` |

Use the supported commands below instead of trying other native sequences on text you want to preserve. This applies to unrecognized prefix keys such as `m`, `Space`, `[` and `z`. The implemented `g` prefix does consume its following key: an unsupported combination such as `gd` is discarded. Unsupported `:` commands are rejected as a whole, as described under [Search and file commands](#search-and-file-commands).

## Enable Helix mode

Choose **Helix** in the **Editing mode** pill in Settings or Welcome, or set this in `config.toml`:

```toml
[modal]
handler = "helix"
```

The command palette also offers **Toggle Helix mode**. Choose **Default** or **Vim** in the same pill to switch handlers. See [Configuration](configuration.md) and [Vim mode](vim-mode.md).

## Available keys

Helix currently has three modes:

- **Normal** is the starting mode. Press `v` to enter Select mode.
- **Select** holds one oriented text selection. Press `v` to return to Normal while keeping the selection. Press `i` to begin Insert at the start of the selection.
- **Insert** delegates text editing to Edamame's regular Markdown editor. Press `Esc` to return to Normal and collapse the selection to the cursor.

In Normal or Select, `Esc` returns to Normal. `;` collapses the selection to the cursor's grapheme. `Alt+;` reverses the selection direction without changing the text. The cursor stays on the active end of the selection.

Only one selection is supported today. Read [Scope and limitations](#scope-and-limitations) before using native Helix sequences not listed here.

## Select, then act

`w e b` select a path to a word boundary; `W E B` use whitespace-separated WORDs, keeping punctuation inside. Normal replaces the previous selection, while Select keeps its fixed end. From the start of `one two three`:

| Keys | Selection or result |
| --- | --- |
| `w` | `one `, including its space |
| `wd` | deletes `one `, leaving `two three` |
| `ec` | deletes `one`, leaves ` two three`, and enters Insert |
| `2w` | `two `: Normal keeps the last movement's range |
| `v2w` | `one two `: Select extends from the original anchor |

`d` deletes, `c` changes (delete and enter Insert), and `y` yanks to Helix's internal register. `y` keeps the selection and cursor. The register is separate from the system clipboard.

## Movement and selection

| Keys | Effect |
| --- | --- |
| `h j k l` / arrows | Move to a grapheme; Select extends the selection |
| Home / `gh` | Start of line |
| End / `gl` | Last content grapheme of line |
| `gs` | First nonblank of line |
| `gg` / `ge` | First / last source line |
| `42gg` | Source line 42, clamped to the document |
| `f` / `F`, then character | Select through the next / previous occurrence |
| `t` / `T`, then character | Select up to, excluding, that occurrence |
| `x` | Select the current source line, including its final newline if present |
| `xx` / `2x` | Extend through the following line |
| `X` | Expand to the boundaries of lines already touched |
| `%` | Select the whole document |
| `;` / `Alt+;` | Collapse / reverse selection |

A count repeats a movement (up to 9999 steps); `gg` uses the count directly as a source line number. `Esc` cancels a pending count, `g` prefix or character find. An absent character leaves the selection unchanged. Vertical movement follows the **Visual line navigation** setting and the current wrap/reflow layout.

In rendered tables, horizontal, word and character-find movements stay inside the current cell. `x/X` still select source lines: deletion of data rows is allowed, while a partial header/alignment-row removal or a selection cutting through cell separators is refused. Selecting the whole table allows deleting it. Raw permits literal source editing. See [Editing](editing.md).

## Editing and registers

| Keys | Effect |
| --- | --- |
| `d` / `c` / `y` | Delete / change / yank the exact selection |
| `r`, then character | Replace each selected grapheme with that character, keeping line breaks |
| `R` | Replace the selection with the internal register, keeping the register |
| `p` / `P` | Paste the internal register after / before the selection |
| `>` / `<` | Indent / unindent the source lines touched |
| `~` | Toggle case in the selection |
| `J` | Join the source lines touched; a single line is unchanged |
| `u` / `U` | Undo / redo |
| `i` / `a` | Insert before / after the normalized selection |
| `I` / `A` | Insert at the cursor line's first nonblank / content end |
| `o` / `O` | Open below the selection's last line / above its first line |

`d/c/y` keep the exact nonempty selected text in one internal register. A selection made with `x/X` carries linewise metadata: `p/P` open whole lines, adding a separator at EOF when needed. Other movement or mouse selection returns to characterwise text. An empty register makes paste and replace no-ops. Counted paste is refused if its expanded payload exceeds 64 MiB.

Copy/Cut/Paste retain edamame's configurable shortcuts. Copy and Cut use the selection; clipboard or terminal Paste replaces it and selects the inserted text. These operations leave the internal Helix register alone. In Insert, paste uses the ordinary insertion behavior. The clipboard fallback remains edamame's kill-ring when no external text is available. See [Keybindings](keybindings.md).

Range actions return to Normal, except `c`, which enters Insert. A yank preserves the selection and cursor. Each range edit is one undo step, including ordered-list renumbering; typing in Insert keeps edamame's existing contiguous grouping. `Esc` after Insert selects the grapheme at the insertion point, using the preceding one only at a nonempty EOF.

In Insert, typing, Enter, Tab, Backspace and paste use the regular Markdown editor: list continuation/indentation and table navigation are unchanged. `o/O` continue a list item or insert a structural table row. In rendered tables, `I/A` use cell boundaries. Range edits and global editing chords, including formatting, image/link snippets and plain Tab insertion, validate the actual replacement before writing: pipes/newlines cannot be pasted into a cell; complete table rows can be pasted at data-row boundaries. Indent/unindent and join refuse rendered table lines. Empty rendered cells keep an empty selection. Raw allows literal Markdown structure edits.

## Search and file commands

`/` opens forward search, `?` backward search. Type a query, then Enter; `n` selects the next match and `N` the previous one, wrapping at the document ends. Normal selects the whole match; Select extends one range to include it. An absent match leaves the selection unchanged and reports it. Matches refresh after edits.

Search is edamame's **literal** search, with smartcase: a lowercase query ignores case; a query with an uppercase character is case-sensitive. `\n`, `\t`, `\r` and `\\` are the available escapes. This is an adaptation: Helix's regex and multiple-match selection commands are not available. See [Editing](editing.md).

`:` opens the file-command prompt. Only these commands are accepted, without arguments:

| Commands | Effect |
| --- | --- |
| `w`, `write` | Save, or open Save As for an unnamed buffer |
| `q`, `quit` | Quit through the usual unsaved-changes confirmation |
| `wq`, `write-quit` | Save, then quit only after a successful write |

A failed or cancelled save leaves the document open. There are no forced commands (`q!`, `w!`), path arguments, substitutions or shell commands in this subset. Use edamame's Save As and search/replace interfaces for those supported application operations.

An open `/`, `?` or `:` prompt captures all keys, including Ctrl chords. The configured Paste action and terminal paste fill the prompt, never the document. Search paste escapes line breaks; command paste removes them. Prompt text is capped at 1024 characters after transformation, keeping the existing draft. `Esc` restores the selection, cursor and viewport from before the prompt. Outside prompts, edamame's Ctrl shortcuts keep their configured actions, including save, clipboard and the command palette.

## Unsupported Helix commands

The following native command families are unavailable. Examples identify missing Helix behavior; they are **not safe no-op sequences**. Unrecognized prefixes can leave later keys to perform the edits explained above. The supported key tables earlier on this page are the command reference for this version.

| Native Helix feature | Examples not implemented here |
| --- | --- |
| Text objects, including simple delimiter/word/paragraph objects | `mi(`, `ma"`, `miw`, `map` |
| Matching brackets and adding/replacing/deleting surrounds | `mm`, `ms(`, `mr("`, `md"` |
| Multiple selections and selection generation/filtering | `s`, `S`, `C`, `K`, `,`; additional cursors and regex-generated selections |
| Search from the current selection | `*`, `Alt+*`; use `/` or `?` to enter a literal query |
| Regex search and substitution | Regex patterns, capture groups and `:s`; `/` and `?` use edamame's literal search |
| Named registers and native clipboard sequences | `"a`, `Space+y`, `Space+p`; use the single internal register or edamame's Copy/Cut/Paste chords |
| Macros and repeat | `q`, `Q`, `.`, `Alt+.` |
| Undo/redo timeline navigation | `Alt+u`, `Alt+U`; `u/U` provide the supported undo/redo actions |
| Additional navigation and view modes | `G`, other `g` commands, `[`/`]` commands, `z`/`Z` modes; only `gg/ge/gh/gl/gs` are implemented under `g` |
| Space menu, window/split and buffer pickers | `Space` sequences and Helix window commands; use edamame's command palette and navigation |
| Additional case/number transformations | Lowercase/uppercase-only bindings and native numeric increment/decrement; `~` is the supported case action |
| Language and syntax-aware editing | LSP completion, definitions, refactoring and Tree-sitter text objects/motions |
| Shell and other file commands | Pipelines, shell execution, forced commands, path arguments and commands outside `w/write/q/quit/wq/write-quit` |
| Prompt history, completion and live previews | No query/command history, completion or incremental/substitution preview; search runs on Enter |
| Native Helix configuration | Helix configuration/keymap files are not read; edamame's keybindings configure application actions, not the internal modal keys |

## Differences from native Helix

- **Application shortcuts:** outside prompts, Ctrl/Super chords keep their edamame actions. With default bindings, `Ctrl-S` saves, `Ctrl-C` copies, `Ctrl-X` cuts and `Ctrl-A` selects all. They do not perform Helix's native actions. Alt chords other than `Alt+;` in Normal/Select also pass to the application keymap; their effect depends on that map.
- **Markdown editing and layout:** Insert uses edamame's list/table helpers; vertical navigation follows the configured source/visual layout. Rendered-table motions and edits obey the cell/structural restrictions described above. Raw permits literal source edits.
- **History:** each range action is one undo step; Insert typing keeps edamame's contiguous grouping rather than promising one undo step for an entire Insert session.
- **Search and commands:** searches are literal, file commands are limited to the six names above, and prompts capture application shortcuts. The documented count, prompt and expanded-paste limits apply.

For the full editor's commands, see the [official Helix keymap](https://docs.helix-editor.com/keymap.html) and [text-object documentation](https://docs.helix-editor.com/textobjects.html). These describe Helix itself; use this page to determine which commands edamame implements.

## Future multiple selections

Only one selection is available today. Multiple simultaneous selections are a goal for a separate change, not commands delivered by this version.

That change must define a primary selection and overlap/merge rules, remap all positions after edits, and validate every range against the same document version before changing any text. One action across all selections must produce one undo transaction. Rendering, scrolling, clipboard, search, mouse and Markdown table guards must handle all selections together. See the [contributor design](dev/helix-mode.md#future-multiple-selections).
