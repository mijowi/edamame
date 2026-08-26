# Terminal compatibility

edamame runs in any terminal, but a handful of features are dependent on certain features not all terminals have. When one is missing, edamame adapts — it swaps in a theme built for 256 colors, keeps `[Image: …]` placeholders in place of pictures, and tells you which key chords won't work. The command palette (`Ctrl-P`) reaches every command regardless of mouse and key chord support.

This page covers what depends on the terminal, how to find out what yours supports, and what to do about each gap.

---

## What depends on the terminal

| Feature | Needs | Without it |
|---|---|---|
| Images and diagrams | 24-bit color **and** an image protocol | `[Image: alt text]` placeholders; the document still reads |
| Most themes | 24-bit color | A 256-color theme is substituted for the session |
| Mouse selection, table handles | Mouse reporting | Keyboard equivalents for everything; table handles are hidden |
| A handful of chords | The kitty keyboard protocol | Those chords never arrive; use the palette or rebind |
| Box-drawing and marker glyphs | A UTF-8 locale | Garbled table borders and list markers |

## Checking your terminal

Three ways, all showing the same five capabilities:

- **The welcome notice.** When you first open edamame, a terminal capabilities summary is displayed in the welcome screen, which shows what your terminal supports. `Ctrl-P` → "Open welcome / terminal setup" reopens the setup screen, which carries the same summary and lets you change the settings that depend on it.
- **The capabilities notice.** The first time you launch edamame in a terminal it hasn't seen before, it reports what that terminal supports. This appears **once per terminal**, not every launch.
- **`edamame --doctor`.** This command prints the same summary plus system facts, formatted for pasting into a bug report. See [getting-started.md](getting-started.md#command-line-flags).

![The terminal capabilities notice, listing color, image, mouse and keyboard support](https://raw.githubusercontent.com/mijowi/mijowi.com/refs/heads/main/edamame/media/terminal_capabilities.jpg)

---

## Color

edamame detects three tiers: **truecolor** (24-bit), **256 colors**, and **no color**.

Most of the 27 built-in themes pick 24-bit colors, which a 256-color terminal quantizes — routinely landing a foreground and its background on the same entry, which is unreadable rather than merely approximate. So below truecolor edamame switches to whichever of `256 Dark` / `256 Light` matches your current theme's appearance, and tells you why.

**That switch lasts for the session only.** Your `config.toml` keeps the theme you chose, so one config shared between a capable machine and a limited one is never clobbered. `256 Dark`, `256 Light` and `Monochrome Dark` are the three themes designed for weaker terminals; they're exempt from the substitution.

Most terminals advertise truecolor by setting `COLORTERM=truecolor` (or `24bit`). If yours supports it but edamame reports 256 colors, that variable is usually what's missing — check whether it survives your shell startup, and see [tmux and multiplexers](#tmux-and-multiplexers) if you run one.

See [themes.md](themes.md) for the theme list and how to write your own.

## Images and diagrams

Inline images and Mermaid diagrams need **an image protocol** — Kitty graphics, iTerm2 inline images, or Sixel — **and** 24-bit color. Below truecolor edamame declines to render them at all, because the result would be badly quantized; that refusal is session-only too, so a config shared with a capable terminal keeps working.

Where an image can't be shown you get an `[Image: alt text]` placeholder, so the document still reads. Diagrams and images are separate settings — you can enable one without the other. Details in [editing.md](editing.md#images).

## Mouse

With mouse reporting, you get click-to-place-cursor, drag selection, double- and triple-click, wheel scrolling, clickable links and checkboxes, and the drag handles on tables (`⠿` to reorder, `⇔` to resize, `✕` to delete).

Without it, every one of those has a keyboard equivalent, and edamame forces `table.show_buttons` off so it isn't drawing handles nothing can grab.

## Keyboard

Some chords can only be delivered by terminals that support the **kitty keyboard protocol**. The legacy encoding every terminal falls back to has no way to represent `Ctrl` combined with a non-alphabetic key, so those chords never arrive — the terminal usually just beeps.

Affected defaults:

- ``Ctrl-` `` (toggle raw mode)
- `Ctrl-Shift-T` (insert table)
- `Ctrl-B` / `Ctrl-I` (bold / italic — these collide with `Tab` and a control byte rather than vanishing)
- The `Alt-Shift-*` table chords

Terminals with support include kitty, Ghostty, WezTerm, foot, and recent Alacritty. Apple Terminal does not have it.

**Workarounds, in order of least effort:** run the command from the palette (`Ctrl-P`); or rebind to a plain `Ctrl`+letter, which works everywhere — `ToggleRawMode = "ctrl+y"` is a good substitute. See [keybindings.md](keybindings.md#changing-a-keybinding).

### Option and the Alt chords on macOS

The `Option` key is not delivered as `Alt` uniformly, and the arrow chords are where it shows.

| Terminal | `Option-←` / `Option-→` | Works out of the box? |
|---|---|---|
| Ghostty | Sent as the escapes `Alt-B` / `Alt-F` | Yes |
| iTerm2 | Sent as `Alt-B` / `Alt-F` | Yes |
| Apple Terminal | `Option` is dropped — arrives as a plain `←` / `→` | No |

edamame accepts `Alt-B` / `Alt-F` as aliases for `Alt-←` / `Alt-→`, which is what makes the first two rows work: those escapes are the readline word-motion sequences, and both terminals map the chord to them by default rather than sending a modified arrow. The alias is a fallback, so binding `alt+b` or `alt+f` to something of your own takes precedence over it.

Apple Terminal sends nothing that distinguishes `Option-←` from `←`, so no application can tell them apart. To fix it, add the mappings yourself in **Settings → Profiles → Keyboard**: `⌥←` → `\033b` and `⌥→` → `\033f` (using "Send Text"), which lands you on the same escapes the other terminals use. Or you can simply use the command palette for any affected actions.

`Option-↑` / `Option-↓` and the `Option-Shift-*` table chords are a separate matter: no terminal rewrites those, so they arrive as genuine modified arrows wherever `Option` is delivered as `Alt` at all (Ghostty does this by default on U.S. keyboard layouts; elsewhere set `macos-option-as-alt = true`).

## Unicode

Table borders, list markers and the cursor block are drawn with box-drawing and geometric characters. edamame reads your locale (`LANG` / `LC_ALL` / `LC_CTYPE`) to decide whether they're safe; a locale that doesn't name UTF-8 is reported as a ✗ on the capabilities notice. If borders come out as garbage, setting `LANG=en_US.UTF-8` (or your own language's UTF-8 locale) is the fix.

## tmux and multiplexers

edamame works under tmux, with two caveats worth knowing:

- **Capabilities are the multiplexer's, not the outer terminal's.** tmux sits between edamame and your terminal and passes through only what it chooses to. Truecolor generally needs `set -g default-terminal "tmux-256color"` plus an `RGB` or `Tc` terminal-overrides entry; image protocols are passed through only in recent tmux versions, and inconsistently.
- **A reattached session carries stale environment.** `TERM_PROGRAM` and `LC_TERMINAL` aren't in tmux's default `update-environment`, so a session started under one terminal and reattached from another still advertises the first. edamame distrusts those hints inside tmux for exactly that reason, which costs a little image-protocol accuracy in return for not guessing wrong.

`edamame --doctor` reports whether it's running under tmux.

---

## Tested terminals

> **In progress.** This table is being filled in as terminals are tested. A `?` means not yet verified, not "doesn't work" — if you run one of those, or another terminal not listed, `edamame --doctor` and a verification of the output on an [issue](https://github.com/mijowi/edamame/issues) is welcome.

✓ works · ✗ degraded or not supported · ? not yet tested

| Terminal | Color | Images | Mouse | Keyboard | Notes |
|---|---|---|---|---|---|
| kitty | ✓ | ✓ Kitty graphics | ✓ | ✓ | The protocols both features are named after |
| Ghostty | ✓ | ✓ Kitty graphics | ✓ | ✓ | On macOS, `macos-option-as-alt` for the `Alt` chords outside U.S. layouts |
| WezTerm | ✓ | ✓ | ✓ | ✓ |  Add `config.enable_kitty_keyboard = true` to your Wezterm configuration for full keyboard support|
| foot | ? | ? Sixel | ? | ? | Wayland; clipboard needs `wayland-data-control` |
| iTerm2 | ✓ | ✓ iTerm2 inline | ✓ | ✓ | Answers the Kitty graphics query without supporting placements; edamame corrects for this |
| Alacritty | ✓  | ✗ | ✓ | ✓ recent | Halfblocks only |
| Apple Terminal | ✗ 256 | ✗ | ✓ | ✗ | Themes fall back to `256 Dark` / `256 Light`; see the `Option` notes above |
| VS Code terminal | ✓ | ✗ | ✓ |  ✗|  Halfblocks only; legacy keyboard encoding|
| Windows Terminal | ? | ? | ? | ? |  |
| tmux (any host) | ✓ | ✓ | ✓ | ✗ | Depends on configuration — see [above](#tmux-and-multiplexers). Some `Ctrl` chords don't work. |

---

## Troubleshooting

**Colors look wrong or washed out.** Your terminal probably lacks 24-bit color; see [Color](#color).

**Images show as `[Image: …]`.** Either no image protocol, or no truecolor (or you don't have them enabled) — the capabilities notice says which.

**`Ctrl-B` / `Ctrl-I` do nothing, or insert a tab.** The legacy key encoding; see [Keyboard](#keyboard).

**`--doctor` says Images and Keyboard are `unknown`.** Those two are detected by writing a question to the terminal and reading its reply, so both stdout and stdin must be attached to a real terminal. Redirecting either (`edamame --doctor > report.txt`) makes them unanswerable. Copy from the screen instead.

**Nothing about my terminal is right.** `edamame --doctor` on an [issue](https://github.com/mijowi/edamame/issues) is the fastest route — it reports what edamame detected and the environment it detected it from.
