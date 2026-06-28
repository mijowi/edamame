# Instance-based `Control` refactor

Plan to push `ui::controls` from a shared *skin* (stateless rendering + a
couple of pure logic helpers) toward *addressable* controls that own the
input→value transition, the input→event mapping, and — where the layout
allows — their own hit-rect, so the logic currently copy-pasted across
`settings_overlay`, `welcome`, and `export_html_modal` lives in one place.

## Goal / non-goals

**Goal:** centralize control *behavior*. A control should map a semantic
input to a value change and an event, in one source of truth shared by
every modal.

**Non-goal (explicitly rejected):** a retained-mode widget that owns its
value and runs its own event loop. It fights ratatui's `StatefulWidget`
grain and breaks the settings Config-projection model. We centralize
behavior, not ownership.

## Why a hybrid, not a uniform model

The three consumers store control state in incompatible ways:

- **`welcome` / `export_html`** own local value fields, committed to
  `Config` only later (on Save / Export). A natural fit for value-owning
  controls.
- **`settings`** owns *nothing* — every control is a live projection of
  `Config`, read fresh each frame through `RowKind { read, write_string,
  cycle }` function pointers. Making it own values would add a
  `Config`↔control sync layer for no functional gain.

So settings keeps the Config-projection row table; only its *input path*
routes through the shared transition logic. This is the deliberate hybrid
boundary.

## Duplication being removed

1. **Toggle flip** — written 3×: `rows.rs` cycle closures (`field =
   !field`), `welcome::cycle_focused`, `export::adjust` / `handle_char`.
2. **Pill cycle** — `settings` + `welcome` call `controls::cycle_enum`;
   **`export::cycle_stylesheet` hand-rolls the same `rem_euclid`** (dead
   duplication).
3. **Toggle arrow semantics are already inconsistent**: `settings` /
   `welcome` flip on either arrow; `export` direction-binds (Left=off,
   Right=on). Unification forces one choice (see decision below).
4. **key → control-input mapping** — Left/Right/Space/Enter arms repeated
   in every `handle_key`.
5. **Focus stepping** — `settings` uses `overlay_nav::next_focusable`;
   `welcome::step_focus` and `export::OptFocus::step` reimplement it (both
   *wrapping*; `next_focusable` is *non-wrapping*).

## Core abstraction (new, in `controls.rs`)

```rust
/// Normalized value a control carries, independent of the domain enum it
/// projects (ImagesEnabled, a bool field, a stylesheet index, …).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlValue {
    Toggle(bool),
    Choice(usize),   // index into a Pill's label slice
    Button,          // valueless
}

/// A semantic input aimed at the focused control. The parent maps raw
/// key/mouse events to these; the control maps these to a value change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlInput { Left, Right, Activate }

/// What the control did with the input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlEvent { Changed(ControlValue), Activated, Ignored }

impl Control {
    /// Single source of truth for "what does this input do to this value".
    pub fn apply(&self, current: ControlValue, input: ControlInput) -> ControlEvent { … }
}

/// key → ControlInput (Left/Right/Enter/Space). Returns None for keys the
/// caller should handle itself (Tab, Esc, typing).
pub fn control_input_for(code: KeyCode) -> Option<ControlInput> { … }

/// Single label-column + control composition (pad label via
/// control_label_style, append the control spans).
pub fn control_row_spans(
    label: &str, label_col_w: usize, control: Vec<Span<'static>>,
    focused: bool, disabled: bool, theme: &Theme,
) -> Vec<Span<'static>>;
```

`Control::apply` semantics:

- **Pill:** `Left` → −1, `Right` / `Activate` → +1, wrapping via the
  existing `cycle_enum` math over the label count.
- **Toggle:** `Left` → `Toggle(false)`, `Right` → `Toggle(true)`,
  `Activate` → flip.
- **Button:** `Activate` → `Activated`, else `Ignored`.

### Decision: direction-bound toggle arrows

Toggle arrows become direction-bound everywhere (Left=off / Right=on;
Space/Enter flip). This is the lowest-breakage unification — no existing
test exercises arrows-on-a-toggle except `export`'s, which already expects
direction-bound; settings/welcome toggle tests all drive Enter/Space. It
also makes arrows meaningful (Right always means on). The only behavior
change is settings/welcome arrow-on-toggle going flip→directional, which is
invisible to single-arrow use. Note it in the PR description.

## Phasing

Land each phase as its own commit. Phase 5's settings render change is the
riskiest piece and should be reviewed separately.

### Phase 0 — core types (pure addition, no call sites touched) — ✅ DONE

`controls.rs`: add the enums, `Control::apply`, `control_input_for`,
`control_row_spans`. Unit-test flip / cycle / wrap / button + the key map.
Zero risk; nothing depends on it yet.

**Implemented.** Added `ControlValue` / `ControlInput` / `ControlEvent`,
`Control::apply`, `control_input_for`, `control_row_spans`, and 8 unit tests
(toggle direction-binding + activate-flip, pill cycle/wrap both ways,
single-label pill no-op, button activation, mismatched-value-shape no-op, key
mapping, row-span padding + no-truncation).

> **Deviation:** each new item carries a temporary `#[allow(dead_code)]`.
> The bin target re-includes these modules (`main.rs` declares `mod ui;`), so
> an unused `pub` item trips `dead_code` under `clippy --all-targets -D
> warnings` — `pub` only exempts a *library* crate's API. Standalone Phase 0
> has no call sites, so the allows are required to keep the commit CI-green.
> **Remove each `#[allow(dead_code)]` as its item gains a real call site** in
> Phases 1–5 (`apply` + `control_input_for` in Phase 1; `control_row_spans`
> in Phase 1/2; the enums become used transitively).

> **Note:** `apply`'s pill cycle keeps its own `rem_euclid` over the value
> *index*; `cycle_enum` stays for the value-slice callers. Both live in
> `controls.rs`, so there's still one module owning the wrap math — no
> cross-module duplication. (`export::cycle_stylesheet`'s copy is removed in
> Phase 1.)

### Phase 1 — `export_html_modal` (smallest; owns its values) — ✅ DONE

- Replace `adjust` toggle arms + `handle_char` space arms +
  `cycle_stylesheet` with `Control::apply` against a `Control` per
  `OptFocus` field. **Deletes the hand-rolled `rem_euclid`.**
- Map `OptFocus` value ⇄ `ControlValue` (`inline_images` /
  `render_diagrams` → `Toggle`, `stylesheet_idx` → `Choice`).
- `arrows_set_toggle_off_and_on` already matches the new direction-bound
  semantics — likely passes unchanged.
- Route `render_row`'s label+control composition through
  `control_row_spans`.

**Implemented.** `adjust` / `handle_char` / `cycle_stylesheet` are gone,
replaced by `apply_input(ControlInput)` + `push_title_char`. The two toggles
go through `Control::Toggle.apply`; `handle_options_key` routes Left/Right
(any field) and Space (option controls) through `control_input_for` →
`apply_input`. `render_row` now builds its spans with `control_row_spans`.
All 15 export tests pass unchanged; `arrows_set_toggle_off_and_on` and
`stylesheet_pill_cycles_and_wraps` confirmed green. Full suite green;
`clippy --all-targets -D warnings` clean.

> **Deviation — stylesheet pill cannot use `Control::apply`.** `Control::Pill`
> holds a `&'static [&'static str]`, but the export stylesheet list is a
> runtime `Vec<(String, String)>`. So the stylesheet cycles its *index* via a
> new shared primitive `controls::cycle_index(current, len, delta)` instead of
> `Control::apply`. To keep one home for the wrap math, `cycle_enum` and
> `Control::apply`'s pill arm now both delegate to `cycle_index` too. This is
> a small, plan-consistent refinement to the Phase 0 surface (the plan said
> "via the existing `cycle_enum` math"; `cycle_index` *is* that math, factored
> out so a dynamic-length pill can share it). `cycle_stylesheet`'s `rem_euclid`
> duplication is removed as intended. The `ControlInput → signed step`
> mapping (`Left → −1`, `Right`/`Activate → +1`) is likewise factored into
> `controls::input_delta`, shared by `Control::apply`'s pill arm and the
> export stylesheet's direct `cycle_index` call so the direction binding
> isn't repeated.

> **Deviation — `#[allow(dead_code)]` not fully removed.** Phase 0's note said
> to drop the allows as items gain call sites. Removed for `apply`,
> `control_input_for`, `control_row_spans`, and the `ControlInput` /
> `ControlEvent` enums. **Kept (now variant-level) on `ControlValue::Choice`
> and `ControlValue::Button`:** export constructs only `ControlValue::Toggle`,
> and the `Choice` built inside `apply`'s pill arm is reachable only from a
> `Choice` input (a self-referential construction rustc treats as dead).
> `Choice` goes live in Phase 2 (welcome pills call `apply`); `Button` in
> Phase 3 (settings buttons). Remove each variant's allow then.

> **Minor behavior/visual change.** Export's focused option row now fills the
> 2-cell gap between label and control (it was previously an unstyled
> `Span::raw("  ")`), matching the settings/welcome unified composition. Text
> and widths are unchanged; render tests pass.

### Phase 2 — `welcome`

- `cycle_focused` becomes: resolve the focused field's `Control` + current
  `ControlValue`, call `apply`, write the result back (the images path
  still goes through `set_images` for the cascade). Removes the three
  inline `cycle_enum` blocks and the two `= !` flips.
- Keep the bespoke scratch-buffer render + scroll-translated hit-rects —
  unifying that is high-effort, low-value (out of scope).
- `render_control_row` label-span building → `control_row_spans`.

### Phase 3 — `settings` (input unification only; keep Config-projection)

- Add `read_value(&Config) -> ControlValue` / `write_value(&mut Config,
  ControlValue)` to `RowKind`, replacing the per-row `cycle` fn-pointers.
  `cycle_focused` becomes read→`apply`→write, with the `LABEL_SHOW_IMAGES`
  cascade hook preserved.
- The row table does **not** become a `Vec<Control>` of owned values —
  source of truth stays `Config`.
- Watch the borrow choreography: `cycle_focused` already copies row bits
  out before mutating `config`; `read_value` / `write_value` must keep that
  ordering.

### Phase 4 — shared focus stepper

Add `next_focusable_wrapping` beside `next_focusable` (or a `wrap: bool`
param) and migrate `welcome::step_focus` + `export::OptFocus::step` onto it.

### Phase 5 — click support (the real work)

Both `settings` and `export_html` are keyboard-only today. `welcome`
already proves the pattern: cache a hit-rect per control during render,
hit-test in `handle_click`, route to the same transition logic. With the
Phase 0 primitives a click is `Control::apply(value, ControlInput::Activate)`.

**`export_html`** (cheap — render already does per-row geometry):

- In `render_row`, capture the control's rect (`x = inner.x + label_w + 2`,
  `width = control_w`, the row's `y`) into a new `OptFocus → Option<Rect>`
  set on state, plus the title and button rects.
- Add `handle_click(col,row)`: hit-test → set `focus`, then for a control
  field call `Control::apply(.., Activate)`; for `Title` just focus; for
  the button `submit()`. Mirror `welcome::rect_contains`.
- Wire the modal adapter (`app::modal::export_html`) to forward clicks, as
  the welcome adapter does.

**`settings`** (its render must change):

- Today rows are flattened to `Vec<Line>` and drawn as **one `Paragraph`**
  after `skip(scroll).take(visible_rows)`, so no per-row rect exists. To
  hit-test, capture each *visible* focusable row's rect during render:
  absolute `y = table_area.y + (idx - scroll)`, control `x = inner.x +
  FOCUS_MARKER_WIDTH + LABEL_PAD`, `width` from the control kind
  (`toggle_width` / `pill_width` / `button_width`). Store `Vec<(usize /*row
  idx*/, Rect)>` on `SettingsState` each frame.
- Lowest-risk approach: keep the single-`Paragraph` draw (don't restructure
  to per-row `Paragraph`s) and *derive* the rects from the known geometry
  alongside it. That avoids touching the scroll/skip logic and the existing
  render tests.
- Add `handle_click(col,row, &mut Config)`: resolve row → `move_focus`
  equivalent (respect `is_disabled`), open/commit drafts as a focus change
  would, then `Control::apply(.., Activate)` for option rows (Edit rows just
  focus → draft opens; external-action rows fire their `SettingsResponse`).
- Wire the settings modal adapter to forward clicks (it currently doesn't).

## Net effect after all phases

- `Control::apply` + `cycle_enum` are the only place toggle/pill
  transitions live; `export::cycle_stylesheet`'s hand-rolled `rem_euclid`
  is gone.
- `control_input_for` replaces the repeated Left/Right/Space/Enter arms.
- One wrapping focus stepper (`next_focusable_wrapping`) backs all three
  modals.
- `control_row_spans` is the single label+control composition.
- All three modals are clickable, through one transition path.
- Settings stays a Config projection — no value mirror, the deliberate
  hybrid boundary.

## Risks / watch-items

- **Settings borrow choreography** (Phase 3) — preserve the copy-before-
  mutate ordering in `cycle_focused`.
- **`export` Enter** — Enter on a non-button field advances focus (doesn't
  `Activate` the control). Keep that arm in the modal; only Left/Right/Space
  route through `apply`.
- **Settings render restructuring** (Phase 5) — the highest-risk change;
  deriving rects alongside the existing single-`Paragraph` draw keeps the
  scroll/skip logic and render tests untouched.
- **Toggle semantics change** — the one behavior change (see decision
  above); low-breakage, note it in the PR.
