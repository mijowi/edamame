# Visual language

Contributor-facing rationale for edamame's theming. This is the *why* — the
conventions a new UI surface has to respect. It deliberately does **not**
list which palette slot each element uses; that lives in
`Theme::from_palette` (`src/config/theme.rs`), which is the only source of
truth and can't drift from itself. For the user-facing view of the palette,
the derivations, and the on-disk TOML format, see
[docs/themes.md](../themes.md).

## The two-tier model

1. **`Palette`** — a small flat set of semantic slots, one shade per role.
2. **`Theme`** — every styled element in the UI, precomputed from the palette
   by `Theme::from_palette`.

The rule that makes this work: **focus, active and disabled states are made by
layering modifiers (BOLD, REVERSED, DIM) on an existing slot, never by adding a
new slot.** That is what keeps "retint the palette, retint the app" true, and
it is why the palette has stayed at sixteen colors while the UI has grown to
over a hundred styled elements. When a new affordance needs a state, reach for
a modifier on the slot that already carries its meaning.

No hardcoded colors exist outside the theme constructors — every UI site reads
`theme.<field>`.

## Cursors: block everywhere, color carries context

The cursor is a uniform **block** at every insertion point — the cell is
recolored and the character under it stays visible. There is no bar/caret
shape and no `CursorShape` enum; context is signalled by *color*, not shape.

In the editor there is **no dedicated cursor field**. The color is derived
from the status-line mode chip so the two can never disagree: every branch
reads a `status_mode_*` style minus the chip's `BOLD`, resolved in one place,
`app::cursor_style::editor_cursor_style`. Under the default handler the color
follows the *view* mode; under the vim handler it follows the *sub-mode*
(`status_mode_vim_*`), with `status_mode_raw` surfacing only in INSERT.

Modal text inputs are the one exception: they use `theme.cursor`, a unified
`accent` block, because they aren't tied to editor mode. Monochrome falls back
to `REVERSED`.

Two mechanical consequences worth knowing before touching a painter:

- The cursor is painted onto the resolved cell *after* wrapping, so it never
  perturbs the word-wrap layout (which is computed from the bare source text).
- A block sitting on a selected or search-highlighted cell **wins** — the
  cursor color takes the cell, not the wash.
- The cursor slot is always one cell wide in both blink phases (see
  `ui::cursor::text_field_spans`) so a field never changes width as it blinks.

## Focus vs. persistent selection

Some modals carry a **persistent selection** independent of which row has
focus — the welcome modal's tri-state rows remember `Ask | Always | Never`
while focus moves around. This differs from list-style modals (palette,
settings, keybinds) where focus and "the chosen row" are the same thing.

Three tiers:

| State | Style | Theme field |
|---|---|---|
| Focused | `primary` bg + REVERSED + bold (filled, strongest) | `modal_button_focused` |
| Persistent selection without focus | `secondary` **fg** on `surface_elevated`, bold (outlined, no fill) | `modal_item_selected_unfocused` |
| Neither | plain `text` fg on `surface_elevated` | `modal_item` |

Filled-vs-outlined is the whole point: two filled affordances of the same color read as ambiguous, so focus location and persistent selection have to be independently scannable. Don't reuse `modal_item_selected` for "selected but unfocused" — it is also a filled `primary` bg and collides with the focused affordance.

Monochrome fallback: `modal_item_selected_unfocused` is plain `DIM` — "marked
but quiet", distinct from `BOLD` (focused) and from plain, without needing
`REVERSED`, which monochrome already spends on the unselected `modal_item`
state.

## Unified controls (`ui::controls`)

Interactive controls share one language: each is a **label plus a widget
rendered as one unit**, and the owning container aligns a column of them by
reserving a fixed label width. The label owns the padding, so a focused row's
whole label column takes the focus fill.

Four flavors — **toggle** (on/off slider), **pill** (2+ value selector), **text input**, and **button** (usually label-less; the label *is* the value inside the widget, see `ui::button_row`).

One rule ties the family together: **`REVERSED` means "filled affordance".**
Buttons are filled in both states — they're always a press target. Pills and
text inputs are unfilled until focused. Focus is a `primary` fill everywhere
*except* the toggle, whose value-colored track would lose its meaning if
inverted.

| State | Pill / Text input | Button | Toggle widget |
|---|---|---|---|
| Focused | `primary` fill, REVERSED, bold (`modal_button_focused`) | `primary` fill, REVERSED, bold | track value-colored; the *row label* takes the fill |
| Unfocused | `secondary` fg, no fill | `secondary` fill, REVERSED | track value-colored |
| Disabled | `text_muted` fg, no fill, DIM | `text_muted` fg, no fill, DIM | track no fill, DIM |

- **Toggle** — a 4-cell colored track with a sliding knob plus an external
  `on` / `off` label. `success`-filled on, `text_muted`-filled off, label in
  the same value color, so "on is green" stays legible regardless of focus.
  It is the deliberate focus exception: the widget never changes on focus (the
  row's label column carries it), and the value survives monochrome via knob
  position plus the literal `on`/`off` text.
- **Pill** — the current value framed by `‹ value ›` arrows, **always**,
  focused or not. Arrows mean "cycle to change"; brackets (`[ Save ]`) mean
  "press to act". Don't give a pill the bracketed look or a button arrows. The
  flavor is chosen explicitly via the `controls::Control` enum, **not** by
  option count — a two-option setting that isn't on/off (a `dark` / `light`
  picker, say) is a neutral pill, not a green toggle, so the green never
  implies a value judgment it shouldn't.

The option-set data (`Control`, `ASK_ALWAYS_NEVER`) and the cycle / cascade
logic (`cycle_index`, `apply_images_cascade`) live in `ui::controls`, shared by
the settings overlay and the welcome modal — pill / toggle input flows through
`Control::apply`.

**The command palette's typing row is not a control.** It sits flush against
the modal body with no colored bg fill, so it reads as a search affordance
rather than a sunken input chip. Its cursor is the same unified `theme.cursor`
block as every other modal input.
