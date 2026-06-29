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

### Phase 2 — `welcome` — ✅ DONE

- `cycle_focused` becomes: resolve the focused field's `Control` + current
  `ControlValue`, call `apply`, write the result back (the images path
  still goes through `set_images` for the cascade). Removes the three
  inline `cycle_enum` blocks and the two `= !` flips.
- Keep the bespoke scratch-buffer render + scroll-translated hit-rects —
  unifying that is high-effort, low-value (out of scope).
- `render_control_row` label-span building → `control_row_spans`.

**Implemented.** `cycle_focused(delta)` is replaced by
`apply_input(ControlInput)`: the three pills go through
`Control::Pill(ASK_ALWAYS_NEVER).apply` (with new `*_from_index` inverses of
the existing `*_index` maps to convert the returned `Choice` back to the
domain enum), and the two toggles through `Control::Toggle.apply`; the images
arm still routes through `set_images` for the cascade. `handle_key` now
collapses the per-key Left/Right/Space/Enter arms into a single
`control_input_for` catch-all — only the Activate-on-`Theme`/`Save` rows keep
explicit arms (they fire `OpenThemePicker` / `Save` rather than mutating a
control). `handle_click` routes the pill *and* toggle hits through
`apply_input(Activate)`, removing the two inline `= !` flips there too.
`render_control_row` builds its label+control spans via `control_row_spans`
(one combined `Line` instead of two separately-rendered rects). All 12
pre-existing welcome tests pass unchanged; one test
(`toggle_arrows_are_direction_bound`) added to cover the behavior change.
Full suite green; `clippy --all-targets -D warnings` and `fmt --check` clean.

> **Behavior change (the documented one).** Welcome toggle arrows
> (Vim mode / Don't-show-again) go from flip-on-either-arrow to direction-
> bound: Left = off, Right = on (Space/Enter still flip). No existing test
> exercised arrows on a welcome toggle, so nothing broke; the new test locks
> it in. Pill behavior, cascade, and the Theme/Save responses are unchanged.

> **Deviation — `handle_click` also unified (slightly beyond the bullet).**
> The plan's `cycle_focused` bullet only called out the *keyboard* `= !`
> flips, but `handle_click` carried two more (`vim` / `show-again`). Routing
> those clicks through the same `apply_input(Activate)` keeps one transition
> path for keyboard and mouse and removes the remaining hand-rolled flips —
> consistent with the refactor's goal. Click-on-toggle is `Activate` (flip),
> identical to the prior `= !`.

> **Deviation — `ControlValue::Choice` allow removed.** Phase 1 kept a
> variant-level `#[allow(dead_code)]` on `ControlValue::Choice`, to be dropped
> "in Phase 2 (welcome pills call `apply`)". Removed: the welcome pills now
> construct `Choice` and feed it to `apply`. `ControlValue::Button`'s allow
> stays until Phase 3.

### Phase 3 — `settings` (input unification only; keep Config-projection) — ✅ DONE

- Add `read_value(&Config) -> ControlValue` / `write_value(&mut Config,
  ControlValue)` to `RowKind`, replacing the per-row `cycle` fn-pointers.
  `cycle_focused` becomes read→`apply`→write, with the `LABEL_SHOW_IMAGES`
  cascade hook preserved.
- The row table does **not** become a `Vec<Control>` of owned values —
  source of truth stays `Config`.
- Watch the borrow choreography: `cycle_focused` already copies row bits
  out before mutating `config`; `read_value` / `write_value` must keep that
  ordering.

**Implemented.** `RowKind.cycle: Option<CycleFn>` is replaced by
`read_value: Option<ReadValueFn>` + `write_value: Option<WriteValueFn>`
(`fn(&Config) -> ControlValue` / `fn(&mut Config, ControlValue)`), `Some`
on toggle/pill rows and `None` on numeric/edit, button, and display rows.
Each toggle row reads/writes `ControlValue::Toggle(bool)`; each pill row maps
its enum to a `ControlValue::Choice(index)` and back via two new
`rows.rs` helpers `order_index` / `order_value` (mirroring welcome's
`pill_index`/`pill_value`) over the existing `*_ORDER` arrays. The vim-mode
row keeps its handler-name special case inside `write_value`.
`settings_overlay::cycle_focused(config, delta)` became
`apply_control_input(config, ControlInput)`: it copies the row's `Copy` bits
(`&'static str` label, `Option<Control>`, two `fn` pointers, `disabled`) out
before mutating, then `control.apply(read_value(config), input)` → on
`Changed(next)` calls `write_value` + the preserved `LABEL_SHOW_IMAGES`
cascade and returns `FieldChanged`; `Ignored`/`Activated` → `Continue`. The
key handler routes `Left`/`Right` to `ControlInput::Left`/`Right` and
`activate_focused`'s `Cycle` arm to `ControlInput::Activate`. Borrow ordering
(copy-before-mutate) preserved. All settings tests pass; one added
(`toggle_arrows_are_direction_bound`) for the behavior change below.

> **Behavior change (the documented one).** Settings toggle arrows go from
> flip-on-either-arrow (the old `cycle` closures ignored `delta` and did
> `field = !field`) to direction-bound: Left = off, Right = on (Enter/Space
> still flip). All existing toggle tests drive Enter, so nothing broke; the
> new test locks it in. Pill rows still cycle on Left/Right unchanged.

> **Deviation — `cycle_enum` removed (was: "kept as the value-slice
> primitive").** After Phase 2 (welcome) and this phase both routed through
> `Control::apply`, `controls::cycle_enum` had zero remaining callers, so it
> tripped `dead_code` under `clippy --all-targets -D warnings`. The plan's
> net-effect text expected it to survive, but no value-slice caller remains,
> so it was deleted (binary-crate-internal, not public API) and its two doc
> references (module header + `cycle_index` doc) repointed to `cycle_index`,
> which is now the sole wrap-around primitive. No test covered `cycle_enum`.
> This does not affect Phase 5 (which uses `Control::apply(.., Activate)`).

> **Note — `write_string` retained on pill rows.** Pill rows still carry
> their `write_string` parse fns (e.g. `parse_images_enabled`); they are only
> invoked by `commit_draft` for `RowAction::Edit` rows and so are inert on
> `Cycle` pills, but removing them was out of scope (they remain a required
> `RowKind` field for the numeric edit rows). `read` (display string) is
> likewise unchanged — Phase 3 touches only the *input* path, not rendering.

### Phase 4 — shared focus stepper — ✅ DONE

Add `next_focusable_wrapping` beside `next_focusable` (or a `wrap: bool`
param) and migrate `welcome::step_focus` + `export::OptFocus::step` onto it.

**Implemented.** Added `overlay_nav::next_focusable_wrapping(rows, current,
delta, is_focusable) -> Option<usize>` — a sibling of `next_focusable`
(chose a separate fn over a `wrap: bool` param; the two have different
"nothing found" semantics and read clearer apart). It steps `delta` at a
time with `rem_euclid` wrap, walking at most `rows.len()` steps so an
all-disabled ring terminates with `None`. `welcome::step_focus` now resolves
the current `FOCUS_ORDER` index, calls the shared stepper with `|f|
!self.row_disabled(*f)`, and applies the returned index to both `self.focused`
and `focus_offsets[i]` (its `delta` param narrowed `isize` → `i32`).
`export_html_modal::OptFocus::step` resolves its `ORDER` index and calls the
stepper with an always-true predicate (every form field is focusable),
mapping the result back to the variant. Both modals' existing focus tests
pass unchanged; 6 unit tests added for the stepper (wrap forward/back, skip
+ wrap, lone-focusable, all-disabled, zero-delta/empty).

> **Note — lone-focusable returns `Some(current)`, not `None`.** The walk
> steps `1..=len` positions (like welcome's original loop), so when only
> `current` is focusable it is reached on the final wrap step and returned
> unchanged — a harmless no-op move (focus stays put, `ensure_visible`
> re-targets the same row). This exactly matches the pre-refactor welcome
> behavior; export never hits it (all fields focusable).

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
