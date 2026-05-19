# Modal Revamp — Implementation Plan

Status: planned. See conversation in `/feature-dev` for the design discussion
that led here.

## Goals

1. Replace the 1-cell line border on every modal with same-bg padding.
2. Move the title into a dedicated row at the top of the padding area, with a
   right-aligned `esc` close hint in `text_muted` that doubles as a clickable
   close button.
3. Introduce typed modals (Normal / Warning / Error) that color-code the title
   and decide whether `Esc` (and the `esc` button) can dismiss the modal at all.
4. Dim the editor (status bar + hint line included) behind any open modal.

## Decisions Already Locked In

- `esc` close-hint color: existing `Palette::text_muted`.
- Three modal kinds: `Normal` / `Warning` / `Error`. Dismissability is a
  separate per-modal bool — title color and dismiss gating are independent
  axes.
- Strict gating: every Warning is non-dismissable. Existing `ConfigWarning`
  and `WidthInjectionWarning` flip from "Esc closes" to "must press Ok".
- Dim sweep covers the full terminal area (status bar + hint line included).
- Implementation: `Modifier::DIM` insert via post-render cell-by-cell sweep.
- Keep current footer button rows. The new `esc` button is additive.
- Click routing: extend the `Modal` trait with `handle_click(col, row) ->
  ModalOutcome`.
- Replace `theme.modal_title` cleanly with three kind-specific fields (no
  alias).

## Layout: the New Modal Chrome

```
┌─ pad_h ─┬───── content ─────┬─ pad_h ─┐   ← modal outer rect
│         │                   │         │   row 0    : top padding (modal_bg)
│  Title  │                   │     esc │   row 1    : title row
│         │                   │         │   row 2    : blank spacer (modal_bg)
│         │   body / form     │         │   rows 3..n: body (with optional
│         │                   │         │              scrollbar in last col)
│         │   ...             │         │
│         │   [Ok] [Cancel]   │         │   row n+1  : button row (if any)
│         │                   │         │   row n+2  : bottom padding
└─────────┴───────────────────┴─────────┘
```

- `pad_h` is `((area.width - content_width) / 2).clamp(1, 4)` —
  4-cell maximum, 1-cell minimum when the terminal is narrow.
- Vertical chrome is fixed at 4 rows: 1 top pad + 1 title + 1 spacer + 1
  bottom pad. (Today: 2 rows of border.)
- The `esc` hint sits at column `inner_right - len("esc") + 1` of the title
  row. Its 3-cell rect is cached on `ModalState` for click hit-testing.
- When the body overflows and a scrollbar is needed, the scrollbar paints
  into the rightmost column of the right padding (`area.x + area.width - 1`),
  not into a separate gutter. The body width is `area.width - 2*pad_h` in
  both cases.

## File-by-File Plan

### 1. `src/app/modal/types.rs`

Add `ModalKind`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModalKind {
    #[default]
    Normal,
    Warning,
    Error,
}
```

Extend the `Modal` trait with three defaulted methods:

```rust
fn kind(&self) -> ModalKind { ModalKind::Normal }
fn dismissable(&self) -> bool { true }
fn handle_click(&mut self, _col: u16, _row: u16) -> ModalOutcome {
    ModalOutcome::Continue
}
```

### 2. `src/config/theme.rs`

- Drop the `modal_border` field entirely (no longer rendered).
- Replace the single `modal_title` field with three new fields:

```rust
pub modal_title_normal:  Style,  // primary_bright fg, surface_elevated bg, bold
pub modal_title_warning: Style,  // warning_bright fg, surface_elevated bg, bold
pub modal_title_error:   Style,  // error_bright   fg, surface_elevated bg, bold
pub modal_close_hint:    Style,  // text_muted    fg, surface_elevated bg
```

- Update `Theme::from_palette()` so all four derive from `surface_elevated` +
  the appropriate palette fg, mirroring the existing `modal_title` derivation.
- Update the monochrome theme branch to map all three kinds to bold + an
  appropriate `Modifier::REVERSED` / `DIM` permutation, since the monochrome
  theme can't color-code titles.

### 3. `src/config/theme_file.rs` and `src/config/theme_file/defaults.rs`

- Drop the `[modal_border]` TOML block from the user-authorable theme schema.
- Add three new theme blocks (`modal_title_normal`, `modal_title_warning`,
  `modal_title_error`) and one `modal_close_hint` block, matching the
  existing `StyleSpec` layout.
- Regenerate `themes/default.toml` (it's auto-generated on first run, so just
  removing the file is enough — the next run rewrites it).

### 4. `src/ui/scroll_container.rs`

Rewrite `draw_frame`. Replace the current free function with:

```rust
pub struct FrameOpts<'a> {
    pub title: &'a str,
    pub kind: ModalKind,
    pub show_close_hint: bool,
    pub theme: &'a Theme,
}

pub struct FrameLayout {
    pub body: Rect,                   // inner area for body + pinned regions
    pub esc_hit_rect: Option<Rect>,   // for click hit-testing
    pub right_pad_col: Option<u16>,   // x of the rightmost padding column,
                                      // for scrollbar callers
}

pub fn draw_frame(area: Rect, buf: &mut Buffer, opts: FrameOpts<'_>)
    -> FrameLayout;
```

Implementation outline:

1. `Clear.render(area, buf)`.
2. Fill `area` with `theme.modal_bg` via a styleless `Block`.
3. Compute `pad_h = ((area.width.saturating_sub(content_width)) / 2)
   .clamp(1, 4)` — caller passes the natural content width into a new
   `FrameOpts` field (or we recompute it from `area.width.saturating_sub(2)`
   if the caller hasn't sized to content).
4. Title row at `area.y + 1`: render `opts.title` left-aligned at
   `area.x + pad_h`, using the kind-appropriate title style. If
   `show_close_hint`, render `"esc"` right-aligned ending at
   `area.x + area.width - pad_h - 1` in `theme.modal_close_hint`. Cache the
   3-cell rect.
5. Body rect: `Rect { x: area.x + pad_h, y: area.y + 3, width: area.width -
   2*pad_h, height: area.height - 4 }`.
6. Right-padding column index for scrollbar callers.

Update `modal_dimensions_for` so the chrome budget is **4 rows + 2*pad_h
columns** (vs. today's 2+2). `centered_rect_for_content` and
`top_anchored_rect_for_content` callers don't change shape; only the inner
math does.

Update unit tests in this module: `centered_rect_grows_to_content_when_terminal_is_large`
expects `width = content + 4` today; that becomes `width = content + 2*pad_h`
where `pad_h = 4` for tall terminals, so the width still ends up `content +
8` rather than `content + 4`. **This is a deliberate widening — capture it
in the test.**

### 5. `src/ui/modal.rs`

`ModalView`:

- Add `pub kind: ModalKind` and `pub dismissable: bool` fields. Caller-supplied;
  no defaults on the struct (each `Modal` impl wires them through).
- Construct `FrameOpts` with `show_close_hint: dismissable`.

`ModalState`:

- Add `pub esc_button_rect: Option<Rect>`, populated each render.
- Add `pub fn click_in_esc(&self, col: u16, row: u16) -> bool`.
- In `handle_key`, gate the `KeyCode::Esc => Cancelled` arm behind a new
  `dismissable: bool` parameter (or fold it into the surrounding modal — see
  below). The `'n'`/`'N'` shortcut also goes away when not dismissable.

Drop the `if self.buttons.is_empty() { return; }` early return — we still
require a `Modal` impl to be dismissable somehow (button or esc-hint), but
the widget shouldn't refuse to draw a button-less modal.

Scrollbar wiring: pass `right_pad_col` from `FrameLayout` into
`scrollbar::render_for_scroll_state` and use `body_outer.width` (no internal
gutter split).

Tests to update:
- `render_draws_title_body_and_buttons` — no border characters; check title
  position and esc text.
- `render_paints_scrollbar_when_body_overflows` — scrollbar lives in the
  right padding column now, not inside the body gutter.
- New: `escape_does_not_dismiss_when_not_dismissable`.
- New: `render_omits_esc_hint_when_not_dismissable`.
- New: `click_in_esc_returns_true_for_correct_coords`.

### 6. `src/ui/command_palette.rs`, `src/ui/settings_view.rs`, `src/ui/keybinds_view.rs`, `src/ui/cheat_sheet.rs` (and any other `draw_frame` caller)

Update each call site to pass the new `FrameOpts`. All four are kind=Normal,
dismissable=true. Settings and Keybinds keep their dedicated input/error
handling — only the chrome changes.

### 7. `src/app/event_loop.rs`

Add the dim sweep in `App::draw_frame`, between editor render and modal render:

```rust
if !self.modal_stack.is_empty() {
    let area = frame.area();
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.modifier.insert(Modifier::DIM);
            }
        }
    }
}
```

The modal's own `Clear` + bg fill overwrites its cells cleanly — the dimmed
modifier on those cells is replaced by the modal's styles.

Add click routing in `App::dispatch_modal_event`:

```rust
crossterm::event::MouseEventKind::Down(MouseButton::Left) => {
    let Some(mut top) = self.modal_stack.pop() else { return false };
    let outcome = top.handle_click(col, row);
    self.apply_modal_outcome(outcome, top);
    true
}
```

### 8. `src/app/modal/*.rs` per-modal updates

Each concrete modal gets `kind()`, `dismissable()`, and `handle_click()`
implementations.

| File | kind | dismissable |
|---|---|---|
| `startup_notice.rs` | Normal | true |
| `cheat_sheet.rs` | Normal | true |
| `command_palette.rs` | Normal | true |
| `settings.rs` | Normal | true |
| `keybinds.rs` | Normal | true |
| `insert_table.rs` | Normal | true |
| `save_copy.rs` | Normal | true |
| `config_warning.rs` | Warning | **false** |
| `width_injection.rs` | Warning | **false** |
| `images_enabled.rs` | Warning | false |
| `remote_image.rs` | Warning | false |
| `quit_confirm.rs` | Warning | true |
| `dirty_guard.rs` | Warning | true |

> **Pattern note (post-implementation).** `kind` and `dismissable` are stored
> as fields on each modal struct (set once in the constructor) and read from
> `self` at three sites: the `ModalView { kind, dismissable }` literal, the
> `state.handle_key(.., self.dismissable)` call, and the trait methods
> `fn kind()` / `fn dismissable()`. The `state.handle_key` `dismissable`
> parameter and the `ModalView.dismissable` field were originally wired
> separately and could disagree — always route both through `self.dismissable`.
> Modals that don't render through `ModalView` (palette, settings, keybinds,
> save_copy, insert_table) inherit the trait defaults; don't add no-op
> overrides.

`handle_click` for every modal that uses `ModalView`: forward to a shared
helper that checks `state.click_in_esc(col, row)` and returns
`ModalOutcome::Close` if true and `dismissable()`, else
`ModalOutcome::Continue`.

For Phase-10 overlays (Palette, Settings, Keybinds) — they use bespoke
widgets, not `ModalView`. They each need to surface their own
`esc_button_rect: Option<Rect>` so click hit-testing works the same way.

Note that for the two Warning prompts that previously treated Esc as "Block"
or "Cancel" (`ConfigWarning`, `WidthInjectionWarning`), we are removing that
fallback — the user must explicitly press Ok. Verify that Ok is the only
sensible response on each.

### 9. Snapshot tests

Every committed `.snap` for modal rendering will change. After
implementation:

```sh
cargo test
cargo insta review   # accept all modal-related snapshot updates
```

## Build sequence

1. `ModalKind` + trait extensions (compile-only; no behavior change yet).
2. Theme field swap; update all `theme.modal_title` / `theme.modal_border`
   references; regenerate the default theme TOML.
3. `draw_frame` rewrite + `FrameLayout` return type; update all callers.
4. `ModalView` chrome + `esc` hit-test + dismissable gating.
5. Per-modal `kind()` / `dismissable()` / `handle_click()` impls.
6. Editor dim sweep + click routing in `App`.
7. Update Phase-10 overlays (Palette/Settings/Keybinds) for the new chrome
   and `handle_click`.
8. Snapshot review pass.

## Risks / footguns

- **Phase-10 overlays don't go through `ModalView`**: they call `draw_frame`
  directly. Each needs explicit `kind` / dismissable / esc-button rect
  plumbing — easy to miss in a grep-driven sweep.
- **`Modifier::DIM` rendering varies by terminal**. Some terminals render
  DIM as a no-op. Acceptable today (we already rely on it for monochrome
  styles), but document in `docs/theming.md`.
- **`pad_h` vs. content_width chicken-and-egg**: `centered_rect_for_content`
  already wants to know `content_width` to size the modal — we just reuse
  that to compute `pad_h` inside `draw_frame`. No new measurement pass
  needed.
- **Dim sweep happens before modal `Clear`**: that means the modal's own
  cells get dimmed and then immediately overwritten. Cheap and correct; no
  need to compute the modal-area complement.
- **Strict gating changes UX for two warnings**: `ConfigWarning` and
  `WidthInjectionWarning` were previously closeable with Esc. Make sure
  their Ok button is reachable via the keyboard (Tab/Enter) and that the
  first-render focus lands on Ok.
