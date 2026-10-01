# Visual language

> Part of the edamame contributor deep-dives. Index and project-wide conventions: [`AGENTS.md`](../../AGENTS.md). Sibling docs live in [`docs/dev/`](.).

The *why* behind edamame's theming — the conventions a new UI surface has to respect. It deliberately does **not** list which palette slot each element uses; that lives in `Theme::from_palette` (`src/config/theme.rs`), the only source of truth. The palette, the derivations and the on-disk TOML format are documented for users in [docs/themes.md](../themes.md); the built-in registry and indexed-color fallback in [themes.md](themes.md).

## The two-tier model

1. **`Palette`** — a small flat set of semantic slots, one shade per role.
2. **`Theme`** — every styled element in the UI, precomputed from the palette by `Theme::from_palette`.

The rule that makes this work: **focus, active and disabled states are made by layering modifiers (BOLD, REVERSED, DIM) on an existing slot, never by adding a new slot.** That keeps "retint the palette, retint the app" true, and is why the palette has stayed at sixteen colors while the UI has grown to over a hundred styled elements. When a new affordance needs a state, reach for a modifier on the slot that already carries its meaning.

No hardcoded colors exist outside the theme constructors — every UI site reads `theme.<field>`.

## Cursors: block everywhere, color carries context

The cursor is a uniform **block** at every insertion point — the cell is recolored and the character under it stays visible. There is no bar/caret shape; context is signaled by *color*, not shape.

In the editor there is **no dedicated cursor field**. The color is derived from the status-line mode chip so the two can never disagree: every branch reads a `status_mode_*` style minus the chip's `BOLD`, resolved in one place, `app::cursor_style::editor_cursor_style`. Under the default handler the color follows the *view* mode; under the vim handler it follows the *sub-mode* (`status_mode_vim_*`), with `status_mode_raw` surfacing only in INSERT.

Modal text inputs are the one exception: they use `theme.cursor`, a unified `accent` block, because they aren't tied to editor mode. Monochrome falls back to `REVERSED`.

Mechanical consequences worth knowing before touching a painter:

- The cursor is painted onto the resolved cell *after* wrapping, so it never perturbs the word-wrap layout (which is computed from the bare source text).
- A block sitting on a selected or search-highlighted cell **wins** — the cursor color takes the cell, not the wash.
- The cursor slot is always one cell wide in both blink phases (see `ui::cursor::text_field_spans`) so a field never changes width as it blinks.

## Focus vs. persistent selection

Some modals carry a **persistent selection** independent of which row has focus — the export modal's Format radio list and the export-theme modal's highlighted theme name keep their choice marked while focus moves around. This differs from list-style modals (palette, settings, keybinds) where focus and "the chosen row" are the same thing.

| State | Style | Theme field |
|---|---|---|
| Focused | `primary` bg + REVERSED + bold (filled, strongest) | `modal_button_focused` |
| Persistent selection without focus | `secondary` **fg** on `surface_elevated`, bold (outlined, no fill) | `modal_item_selected_unfocused` |
| Neither | plain `text` fg on `surface_elevated` | `modal_item` |

Filled-vs-outlined is the whole point: two filled affordances of the same color read as ambiguous, so focus location and persistent selection have to be independently scannable. Don't reuse `modal_item_selected` for "selected but unfocused" — it is also a filled `primary` bg and collides with the focused affordance. For a composite affordance (radio/checkbox glyph + label), apply the unfocused-selection style to the *glyph only*. Ordinary labeled control rows are *not* this case; their focus styling is `controls::control_label_style` ([ui-controls.md](ui-controls.md)).

Monochrome fallback: `modal_item_selected_unfocused` is plain `DIM` — "marked but quiet", distinct from `BOLD` (focused) and from plain, without needing `REVERSED`, which monochrome already spends on the unselected `modal_item` state.

## Controls

The mechanism — which `ui::controls` function styles what, and how input flows — is in [ui-controls.md](ui-controls.md). The visual rule tying the family together: **`REVERSED` means "filled affordance".** Buttons are filled in both states — they're always a press target. Pills and text inputs are unfilled until focused. Focus is a `primary` fill everywhere *except* the toggle, whose value-colored track would lose its meaning if inverted.

| State | Pill / Text input | Button | Toggle widget |
|---|---|---|---|
| Focused | `primary` fill, REVERSED, bold (`modal_button_focused`) | `primary` fill, REVERSED, bold | track value-colored; the *row label* takes the fill |
| Unfocused | `secondary` fg, no fill | `text` fg on `surface` fill, bold | track value-colored |
| Disabled | `text_muted` fg, no fill, DIM | — (buttons are never disabled) | muted track, DIM |

- **Toggle** — a 3-cell colored track with a sliding handle plus an external `on` / `off` label. `success`-filled on, `text_muted`-filled off, label in the same value color, so "on is green" stays legible regardless of focus. The value survives monochrome via handle position plus the literal `on`/`off` text.
- **Pill** — the current value framed by `‹ value ›` arrows, **always**, focused or not. Arrows mean "cycle to change"; brackets (`[ Save ]`) mean "press to act". Don't give a pill the bracketed look or a button arrows. A two-option setting that isn't on/off (a `dark` / `light` picker, say) is a neutral pill, not a green toggle, so the green never implies a value judgment it shouldn't.

**The command palette's typing row is not a control.** It sits flush against the modal body with no colored bg fill, so it reads as a search affordance rather than a sunken input chip. Its cursor is the same `theme.cursor` block as every other modal input.
