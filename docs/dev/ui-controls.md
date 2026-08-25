# Unified UI controls

> Part of the edamame contributor deep-dives. Index and project-wide conventions: [`AGENTS.md`](../../AGENTS.md). Sibling docs live in [`docs/dev/`](.).


Interactive elements inside modals/overlays are one family, defined in `ui::controls`. The governing rule: **a control resolves its own styling from `controls`; the parent container only reports whether it is `focused` / `disabled`.** Never hand-roll a focus style in a modal.

- **Four flavors, declared at the definition site.** **Toggle** (`toggle_spans`) — an on/off slider, and the one control whose *widget* keeps its value color when focused (inverting it would destroy the on-is-green reading), so focus shows only in the label column. **Pill** (`pill_spans` over a `&[&str]`, e.g. the shared `ASK_ALWAYS_NEVER`) — a multi-value `‹ value ›` selector cycled with ←/→; on/off is **not** a pill flavor, a binary setting uses the Toggle, and a row declares which via the `Control` enum (don't reintroduce `PillStyle::Toggle`). **Text input** — `controls::text_value_style`, with the blink-stable cursor from `ui::cursor::text_field_spans`. **Button** — `ui::button_row`, styled by `controls::button_style`.
- **Focus is one language; the label column is the single source of truth.** `REVERSED` means "filled affordance". `controls::focused_style` (= `theme.modal_button_focused`) is the shared focus fill, and `controls::control_label_style(focused, disabled, theme)` resolves a labeled row's label column — focused → `modal_item_selected`, disabled → `modal_close_hint`, resting → `modal_item` — called by **both** the settings overlay and the welcome modal. A focused row is one unit: pad the label across the whole column so the fill spans label → widget.
- **Buttons go through `ui::button_row`, never a hand-built literal.** Construct a `Button::bracketed(label)` and let `render_button_row` (centered footer) or `render_button_at` (left-aligned inline) add the `[ … ]`, size it, place it and return the hit-rect.
- **Cycle + cascade logic is shared too.** Pill / toggle inputs route through `controls::Control::apply`, whose pill arm and every index-valued caller delegate wrap-around to `controls::cycle_index`; `controls::apply_images_cascade` (images-`Never` forces remote-`Never`, stashing/restoring the prior choice) is shared by the settings overlay and the welcome modal.

