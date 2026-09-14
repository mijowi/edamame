# Partial image rendering — the visible band as the interface

Branch: `image-band-rendering` (this branch implements **M1: the Kitty backend**, **M4: Kitty direct placement**, and **M2: the Sixel backend**)
Base: `unreleased` (`a537f2c`), the branch the pull request targets. The line
references below were taken against `713baaf` (`main`, v0.1.4) and have drifted
since — read them as anchors, not offsets.
Issue: [mijowi/edamame#50](https://github.com/mijowi/edamame/issues/50)

## Problem

`paint_images` (`src/ui/image_view.rs:303`) gates the native graphics protocol on
the image's **full reserved rect** fitting inside the viewport (`:320,348`):

```rust
let fully_visible = top >= viewport_top && bottom <= viewport_bottom;
let use_native = fully_visible && !ctx.is_scrolling && !ctx.modal_open;
if use_native { paint_native(...) } else { paint_scratch_partial(...) }
```

Any partial visibility therefore takes `paint_scratch_partial` (`:485`), which
cell-copies the pre-rendered **halfblocks** scratch — 1 pixel per column, 2 per
row. Kitty / Sixel / iTerm2 would hand the terminal the native payload;
halfblocks downsamples it to the text grid. That is the reported blur.

Two of the three triggers are permanent rather than transient:

| Trigger | Duration |
|---|---|
| partially scrolled (top above, or bottom below, the viewport) | while clipped |
| reserved height > viewport height (`images.max_height` defaults to 24, `src/config/sections.rs:221`, and is not clamped to the document area) | **forever** — `fully_visible` can never be true |
| within `SCROLL_QUIESCE` (150 ms, `src/app/frame_timer.rs:12`) | transient |

The third case is now partly mitigated, not fixed: `editor.max_width_enabled`
defaults to `true` with `max_width_cols = 100` (`src/config/sections.rs:123-124`),
and `clamp_doc_area_to_max_width` (`src/ui/editor_view.rs:100`) narrows the
document area, so the reserved height is computed against a ≤100-column box on a
wide terminal. That reduces how often an image exceeds the viewport, but a tall
portrait image on a short terminal still hits the permanent case.

The clipping is **vertical only** — `rect.x == area.x` and `rect.width ==
area.width` always (`build_snapshots` rect construction, `src/ui/image_view.rs:161`).
So the whole problem reduces to a row band, and the horizontal dimension never
needs any arithmetic at all.

## The interface: one band, every protocol

### The band

Every protocol has to answer the same two questions: *which rows of the image
should the terminal draw*, and *which cells should they land in*. The answers
are derived entirely from data the snapshot already carries — `natural_top`
(`isize`, deliberately negative once the top has scrolled out) and
`rect.height`, which is the reserved height `R`:

```
skip     = max(0, V0 - T)                        rows clipped off the top
visible  = min(T + R, V1) - max(T, V0)           rows that can be drawn
dst      = Rect(rect.x, max(T, V0), rect.width, visible)
```

`paint_scratch_partial` already computes a fragment of this (`clip_top`,
`src/ui/image_view.rs:506`) — it just spends it on a halfblocks cell copy
instead of on the protocol's row offset.

### The band is the only path, not a branch

A fully visible image yields `skip = 0, dst = rect` — **the band path degenerates
to exactly today's behaviour**. So the band should not be a second path beside
the `fully_visible` check; it should *be* the path, and `fully_visible` (`:348`)
should be deleted rather than joined.

`is_scrolling` does **not** retire, though — a correction to an earlier revision of
this plan, which read the gate as being about re-encoding. Its stated purpose is
the re-*composite*: during scroll every protocol falls back to
position-independent halfblocks because Kitty's placeholders "still re-composite
at each new cell position, the dominant source of scroll lag on image-heavy
documents" (the scroll gate's own comment in `src/ui/image_view.rs`). Row
addressing removes the re-encode but not the re-composite, so the scroll window
still applies to Kitty. What changes is what happens when scrolling *stops*: the
band paints at whatever offset the view came to rest at, instead of requiring the
image to have become fully visible again.

### Why the band must stay a render-time parameter

This is the property that makes the interface cheap, and it is worth stating
because the obvious alternative violates it.

`get_protocol_pair` caches protocol objects by `(url, width, height)`
(`src/image/cache.rs:255`). With the band as a *render-time* parameter, the
protocol's height stays the full reserved size, so scrolling changes none of the
key — the protocol is built once and reused, and a scroll tick costs one integer.
Band re-encoding (the M3 approach) makes the band part of the *encoding*, so the
key grows a dimension, the cache churns on every scroll settle, and the pair must
be rebuilt — which, for Kitty, is not merely expensive (limitation 1).

**Keep the band out of the cache key.** "Rebuild triggers" below records why the
geometry the key holds is in fact stable, and the one case where it is not.

## Backends

`ratatui_image::sliced` is already this interface for four of the five routes
below — one `SlicedProtocol` variant per protocol, plus `SignedPosition` (an
`i16` position, so a negative top is expressible) and `SlicedImage::skip_and_drop`
(private, and already unit-tested upstream against negative positions). The
exception is Kitty *direct placement*, which `sliced` cannot express and we would
write ourselves. The routes differ in build cost, build timing, and payload
accounting — and the last of those is what decides iTerm2.

| Protocol | Implementation | Build | Render | Accounting | Verdict |
|---|---|---|---|---|---|
| **Kitty** | `Kitty::render_with_skip` (row addressing via unicode placeholders) | 1 raw-RGBA transmit string (~1.3 MB for a full-width image) | no re-encode; per-row placeholder symbol | **none needed** — `Arc<AtomicBool>` transmit latch | **adopt (M1)** |
| **Kitty `a=p`** | hand-written direct placement with a source rect (see "A fourth route") | 1 transmit string | one short escape per band change | one placement cell; nothing is re-sent | adopt (M4) for WezTerm-class terminals |
| **Sixel** | `SlicedSixel` (splices the payload's 6-px bands at draw time) | 1 sixel encode + band split | no re-encode; one string build | none | **adopt (M2)** — the route for Windows Terminal, which has no Kitty graphics at all |
| **iTerm2** | crop the visible rows and re-send them (see "iTerm2 — M3") | **1 PNG encode per band change** | the PNG is re-sent, so the image blanks and redraws | one payload cell, but that re-send is the flash `NativePaint` / `mark_rect_skipped` exist to prevent | adopt (M3), iTerm2 proper only |
| **Halfblocks** | `Halfblocks::render_with_skip` (row copy) | 1 encode | no re-encode | none | **never** — zero fidelity gain; `paint_scratch_partial` already does exactly this |

The capability is therefore *not* what separates the protocols; the cost profile
is. Row addressing (Kitty), direct placement (`a=p`) and band splicing (Sixel) all
get the feature for free; iTerm2 pays one re-send — and its flash — per band
change; halfblocks has nothing to gain.

### A fourth route: Kitty *direct placement* (WezTerm today)

The three backends above assume a terminal either renders through `U=1` unicode
placeholders or does not. WezTerm is a third thing, and it matters because it is
a common Windows terminal: it implements the Kitty protocol's **direct placement**
(`a=p`) with a **source rectangle**, but not the placeholder mode that
ratatui-image's Kitty backend renders *exclusively* through.

Verified in the WezTerm tree (`d2f3f05`):

- `wezterm-escape-parser/src/apc.rs:1022` parses `a='p'` as
  `KittyImage::Display { image_id, image_number, placement, verbosity }`, and
  `KittyImagePlacement` (`:593`) carries `x/y/w/h` (the **source rect**),
  `x_offset/y_offset`, `columns/rows`, `do_not_move_cursor`, `placement_id`,
  `z_index`.
- `term/src/terminalstate/kitty.rs:239` **renders** it (`kitty_img_place`), and
  `:241` handles `KittyImageDelete::ByImageId { image_id, placement_id, … }`, so
  one placement can be dropped without deleting the image data.
- `10EEEE` appears **nowhere** in `term/`, `wezterm-escape-parser/src`,
  `wezterm-gui/src` or `config/src` — the placeholder mode is absent, which is
  exactly what a forced-Kitty run showed (Verification 8: literal placeholder
  glyphs, no image composited).

So on such a terminal a band can be had for **free**: transmit once with `a=t`,
then place the *same stored image* each frame with
`x=0, y=skip*font_h, w=W, h=visible*font_h, c=rect.width, r=visible`. The
terminal crops from what it already holds, so moving the band costs one short
escape — no encode, no re-transmit. The price is a hand-written sequence writer:
ratatui-image's Kitty backend cannot express this, so the transmit/place/delete
escapes, their cursor dance, and the `a=d` cleanup on eviction and resize are all
ours to write.

**Why this beats M3 for WezTerm.** `Iterm2::encode` begins with `clear_area` — an
ECH sweep of its own rows — and then re-sends the whole PNG. Every band change
therefore **blanks and redraws the image**: precisely the flash `NativePaint` and
`mark_rect_skipped` exist to prevent (see `docs/dev/media-export.md`). M3 buys
sharpness at rest at the cost of a flash per band change, because on iTerm2 the
band can only change by re-sending. The direct-placement route does not re-send at
all.

The two are not alternatives for the same audience: direct placement needs `a=p`
(WezTerm-class terminals), while M3 is the only option for iTerm2 proper and any
terminal that speaks nothing but OSC 1337.

### Kitty — M1

Row addressing: the transmit payload contains every row, and the placeholder
grid addresses image rows by diacritic index (`row_y = y + skip_line_count`,
`ratatui-image-11.0.6/src/protocol/kitty.rs:186`). Drawing a band is a matter of
starting the grid at `skip` — no re-encode, no re-transmit, pixel-exact.

`render_with_skip(area, buf, skip)` takes no `drop`; the destination height
encodes it, which is consistent with the invocation below.

### Sixel — M2 (taken)

The private `sixel_slice::SlicedSixel` (its module is *not* `pub`, so the type
cannot be named from this crate — but `SlicedProtocol::Sixel(…)` is a public
variant built by `SlicedProtocol::new_with_resize`, which is all M2 needs)
deconstructs the sixel payload into its native 6-pixel bands at build time and
skips/truncates them at draw time. Not pixel-accurate (6-px granularity), which
upstream documents as "good enough". The module comment also explains why the
generic `Sliced(Vec<…>)` path is *not* used for sixel: it glitches in foot.

**This is Windows Terminal's route, and the reason M2 stopped being optional.**
WT is the one terminal the four routes above cannot share a mechanism with: it
has **no Kitty graphics protocol at all** (its parser has no APC handler; the
only "kitty" in the tree is the *keyboard* protocol, `CSI u`) and it is
therefore not an M1, M4 or M3 target. What it has is Sixel, and it advertises it
in DA1 (`?61;4;…c`, `adaptDispatch.cpp` in `microsoft/terminal`), which is what
ratatui-image's capability query reads — so an edamame in WT resolves to
`ImageProtocol::Sixel` with no routing hint of any kind. Read against
`microsoft/terminal` at main (2026-09-11) and WT 1.24:

- **ConPTY passes the application's VT through unmodified** (commit `450eec48d`,
  "Goodbye VtEngine Edition" — "any VT output that an application generates will
  now be given to the terminal unmodified … opening the path towards … sixels").
  So the band escapes reach WT exactly as written; there is no ConPTY re-encode
  between edamame and the terminal.
- **WT rasterises a sixel into per-row `ImageSlice` pixels in its text buffer**
  (`SixelParser::_maybeFlushImageBuffer` → `ROW::SetImageSlice`, drawn by
  `AtlasEngine::PaintImageSlice` per visible row) and **erases the slice where
  text is written** (`TextBuffer::Replace` → `ImageSlice::EraseCells`). Clipping
  is therefore the terminal's own, which is what makes a re-sliced payload
  enough — and the erase is the same primitive the scratch path and the
  payload's `clear_area` sweep already rely on.
- **A sixel is drawn from the cursor**, and WT's default DECSDM is reset, so a
  sequence that does not fit below the cursor **scrolls the text buffer** to make
  room. In a TUI that would push the whole document up, which is why the band —
  bounded by `image_band` to the visible rows — is not just a fidelity choice but
  the only safe payload. Sending the full image and letting the terminal clip is
  not an option here.
- **WT's sixel cell size is virtual and reported**: `CellSizeForLevel(9) = {10, 20}`,
  and `CSI 16 t` answers with that same size, so the picker's `font_size` and
  WT's pixel↔cell mapping agree — the image lands at the intended width and
  height rather than being rescaled by the terminal.

The implementation is the M1 one with the gate widened: `image::build_sliced`
(was `build_kitty_sliced`) accepts `Kitty | Sixel`, `ProtocolPair::sliced`
(was `kitty_sliced`) is the backend for both, and `paint_images` routes
`ImageProtocol::Sixel` through `paint_sliced` under the same scroll and modal
gates. `native` stays `None` on Sixel too — the pair's band already *is* the
payload, so a threaded encode would be a second copy of the same bytes, never
read. No WT-specific escape writer, no hint, no config surface: unlike M4, the
terminal supports exactly the protocol `SlicedProtocol` already speaks.

Measured in this tree (a scratch probe, since deleted): a `ProtocolType::Sixel`
picker builds `SlicedProtocol::Sixel` for a 4×4-cell, 40×80 px fixture and the
band for `skip = 1` text row comes out **11 sixel bands, bottom-aligned with the
full payload's last band, opening with an ECH sweep sized to the band (3 rows)**
rather than to the image. A 20 px cell height is 3⅓ sixel bands, so the skip
lands on 18 px: that ~2 px of slop (≤ 6 px in general) is M2's known cost, and
the only fidelity it gives up against M1/M4.

### iTerm2 — M3 (crop and re-send)

iTerm2 has neither row addressing nor a source rectangle, and `Iterm2::encode`
opens with `clear_area` — an ECH sweep — before re-sending the whole PNG. A band
can therefore only change by re-sending, which fixes the mechanism:

```
crop the visible rows out of the *already-resized* bitmap, then
Iterm2::new(cropped, Size::new(width, visible), is_tmux).render(dst, buf)
```

`dst` and `skip` are the same values `image_band` produces for every other
backend. Cropping the **resized** bitmap rather than the original is
load-bearing: a band's aspect ratio is not the image's, so `Fit` on a cropped
original would rescale, and the image would visibly change scale as it scrolls.

Two costs, both real:

- **The flash.** Every re-send blanks and redraws the image — precisely what
  `NativePaint` / `mark_rect_skipped` exist to suppress
  (`docs/dev/media-export.md`). M3 therefore buys sharpness at rest and pays one
  flash per band change, which is why direct placement is preferred wherever the
  terminal has `a=p`.
- **One encode per band change, on a worker.** The band is a scroll-time
  artifact, so unlike M1 it cannot ride the decode worker's one-time prebuilt.

The upstream alternative is rejected on accounting grounds rather than cost:
`SlicedProtocol::Sliced(Vec<Protocol>)` pre-slices one PNG per text row, free at
render time but with a payload in **every** row's cell, reviving the
`Buffer::diff` `invalidated` cascade edamame already fought. One band is one
payload cell, so the existing suppression machinery applies unchanged.

M3 is the only route for iTerm2 proper, and for any terminal that speaks nothing
but OSC 1337.

## Why the existing path cannot be extended

`StatefulKitty::render_with_skip` (`protocol/kitty.rs:66`) is **`pub(crate)`**,
and `StatefulProtocol` exposes no skip entry point at all. edamame holds a
`ThreadProtocol` (which wraps `StatefulProtocol`), so the row-addressing
primitive is unreachable from this crate — for every protocol, not just Kitty.
Adding a skip parameter to `paint_native` is therefore not an option; the public
`sliced` module is the vehicle, and `pub mod sliced` is **not** feature-gated
(`lib.rs:162`), so it is available under the current
`default-features = false, features = ["crossterm"]`.

## Upstream mechanics that constrain the design

1. **The Kitty transmit payload is raw RGBA, not PNG.** `transmit_virtual`
   (`protocol/kitty.rs:224`) does `img.to_rgba8()` and base64-chunks the raw
   bytes with `f=32,t=d`. For a full-width image at 80×24 cells × 8×16 px font
   that is 640×384 px = 983 KB raw → **~1.3 MB of escape string**, built
   **synchronously** inside `Kitty::new`.
2. **`SlicedProtocol::new*` allocates a fresh random id per build.**
   `picker.new_protocol_raw` uses `rand::random()` (`picker.rs:245-250`), and
   there is **no `d=I` delete sequence anywhere in `protocol/kitty.rs`**. A
   rebuild therefore leaks the previous image in the terminal's graphics store.
   Today's `StatefulKitty` avoids this by *reusing* its id across
   `resize_encode`. ⇒ **Build the sliced protocol once per geometry; a band
   change must never trigger a rebuild.**
3. **`Picker::is_tmux` has no public accessor** (`protocol_type()` and
   `font_size()` do), so a hand-rolled `Kitty::new` would have to re-derive tmux
   detection. Use the public `SlicedProtocol::new_with_resize`, which reads it
   internally, and accept the random id (limitation 1).
4. **`SlicedImage::render(area, buf)` takes the whole area plus a signed
   position**, and computes skip/drop itself with `area_top` hard-coded to 0.

## Design (M1 — the Kitty backend)

### Data flow

```
decode worker (src/app/image_dispatch.rs:551)     ← mirrors the existing scratch build
  SlicedProtocol::new_with_resize(picker, image, Size::new(width, rows), Resize::Fit(None))
  → LoadedImage.sliced = Some((rect, sliced))

get_protocol_pair (UI, cold path)
  claims prebuilt_sliced[(url, w, rows)]          ← same claim-once pattern as prebuilt_scratches
  (sync fallback only on a key miss — see "Rebuild triggers")

paint_images (UI, per frame)
  clear_visible_reserved_rect(snap, …)            ← unchanged; still needed for placeholder bleed
  SlicedImage::new(&sliced, SignedPosition { x: 0, y: -(skip as i16) })
      .render(dst, buf)
```

### The invocation, precisely

`SlicedImage::render(area, buf)` treats **`area` as the clipping window**, not as
"where the document is": it computes `skip' = max(0, -position.y)` and
`drop = max(0, position.y + size.height - area.height)`, then paints
`size.height - skip' - drop` rows at `area.y + max(0, position.y)`.

So with the protocol built at `S = (width, R)` and `area = dst` (height `V`):

```rust
// skip = rows of the image above the visible band; dst = the clamped visible rect
SlicedImage::new(sliced, SignedPosition { x: 0, y: -(skip as i16) }).render(dst, buf);
```

gives `skip' = skip`, `drop = R - skip - V`, and therefore exactly `V` rows
painted at `dst.y` — the band. Passing `ctx.area` instead of `dst` also works
for pure scrolling, but `dst` is the form that generalizes (next subsection).

### Rebuild triggers: the rect height is stable

An earlier revision of this plan flagged the `$$...$$` live preview as a rebuild
trigger, on the theory that `build_snapshots` shrinks the image rect mid-reveal
(`src/ui/image_view.rs:136`, `ImageReveal` at `src/editor/state.rs:121`). It does
not, and the reason is worth recording, because the implementation keys the
protocol as `(url, width, height)` like everything else *because* of it:

- A revealed `$$...$$` block's row override returns
  `reveal.rows + reveal.preview_rows` (`src/editor/state.rs:969`), and
  `build_snapshots` subtracts `source_rows_below = reveal.rows`, leaving
  `preview_rows`.
- `preview_rows` is `images.aspect_rows(url, …)` (`src/editor/state_cursor_block.rs:202`),
  documented as "same row count the renderer's override gives the image outside
  the reveal, so it doesn't resize when the reveal opens".
- A block that is *not* revealed takes `images.reserved_rows(url, …)`
  (`src/editor/state.rs:982`).
- For a decoded image those two are the same call: `reserved_rows` and
  `aspect_rows` both return `aspect_rows_of(…)`, differing only in what they
  answer for a `Failed` decode.

So the image rect has the same height before and during the reveal; the reveal
only adds source rows *below* it. No new key, no rebuild, no geometry
indirection — `paint_images` resolves the pair by `snap.rect.height`.

(The equality holds for a decoded image. For a `Failed` one they differ, since
`reserved_rows` collapses it to a single row — but a failed decode has no protocol
at all, so `get_protocol_pair` answers `None` before the key matters.)

**Accepting one synchronous build per image per new geometry.** A terminal resize
is the remaining trigger, and it cannot be avoided without new plumbing:

- `on_resize` (`src/app/event_loop.rs:607`) calls only
  `invalidate_native_paints()`. It does **not** clear `protocols` or
  `prebuilt_scratches`.
- `request` (`src/image/cache.rs:202`) is a no-op once a URL is decoded, so the
  decode worker never re-runs and **no fresh prebuilt is ever produced for a new
  geometry** — stale-keyed entries simply sit unmatched.
- Every `(image, geometry)` pair therefore pays exactly one synchronous build, in
  the first frame that paints it. That is the path already calibrated as "~5-20 ms
  sync encode here, rare enough not to regress scroll" (`src/image/cache.rs:276`).

For the halfblocks scratch, ~5-20 ms per image per resize is today's accepted
cost. For Kitty the same moment builds the transmit string; the estimate is the
same order (a `to_rgba8` of ~1 MB plus base64 of the same, plus `String` growth) —
**estimated, not measured**, and on the UI thread. Kitty's exposure is also wider
than the scratch path's, since `fully_visible` no longer gates it (though
`is_scrolling` still does, for the reason below).

Both synchronous paths are now instrumented (`tracing::debug!` under the existing
`[dev] logging` flag, carrying `micros`), so the number is measurable rather than
assumed. If it measures badly, the fix is to re-derive the prebuilt off-thread
from the already-cached `Arc<DynamicImage>` — a "rebuild prebuilt for
`(url, w, h)`" job on the **existing** decode worker, which would also remove
today's scratch hitch, and which stays inside this document's rejection of a
*second* channel.

### Threading: build on the decode worker

`SlicedProtocol::new*` builds the transmit string synchronously, so it must not
run on the UI thread — today that cost sits on the encoder worker. The decode
worker already does exactly this kind of one-time derived work and already holds
every input:

- `LoadedImage { url, image, scratch: Option<(Rect, Buffer)> }` (`src/image/loader.rs:25`)
- the scratch build at `src/app/image_dispatch.rs:551`, inside
  `dispatch_image_decodes_for` (`:408`), alongside `scratch_picker`,
  `scratch_width`, `max_cells`, `font_size`, and wrapped in
  `ExpectedPanic::new()` + `catch_unwind`

So: add `sliced: Option<(Rect, SlicedProtocol)>` to `LoadedImage`, populate it in
that same block **only when `scratch_picker.protocol_type() ==
ProtocolType::Kitty`**, and give `ImageCache` a `prebuilt_sliced` map mirroring
`prebuilt_scratches` (`src/image/cache.rs:146`, claimed at `:280`).

This deliberately does **not** touch the existing encoder channel or
`ThreadProtocol`. The channel type is upstream's concrete
`Sender<ratatui_image::thread::ResizeRequest>`, and `ThreadProtocol::new`
requires exactly that type; widening it to an edamame-owned enum would drag in
every `ThreadProtocol` user (`app.rs` field, `event_loop.rs` worker, `nav.rs`
attach, the `cache.rs` FIFO routing, and the `image_view.rs` test harness).

`get_protocol_pair` (`src/image/cache.rs:255`) needs **no new parameter**: the
protocol test is `native_picker.protocol_type() == ProtocolType::Kitty`, and the
picker has already had `resolve_protocol`'s Kitty→Iterm2 override applied to it
(`src/terminal/capabilities.rs:276`).

### Accounting

`NativePaint` (`src/image/cache.rs:100`) / `mark_rect_skipped`
(`src/ui/image_view.rs:473`) exist because iTerm2 re-emits the whole PNG on every
render. Kitty has an `Arc<AtomicBool>` transmit latch
(`KittyProtoState::make_transmit`), so the sliced path needs none of it: it
writes the placeholder cells each frame and ratatui's diff drops them because the
content is identical. iTerm2 / Sixel keep `paint_native`
(`src/ui/image_view.rs:398`) unchanged.

The symbol-width concern does not bite: a placeholder row's symbol is roughly
`area.width` display columns, so `Buffer::diff`'s `invalidated` stays bounded by
the area width — the same bound today's Kitty path already has.

### What is deliberately not done

- No `is_tmux` re-derivation, no hand-rolled `Kitty::new`, no id control.
- No `d=I` cleanup on eviction (needs a deferred-escape queue; limitation 1).
- No change to the halfblocks scratch, `paint_native`, or
  `paint_scratch_partial` for the other protocols.
- No new tuning knob or config surface.
- No geometry indirection for the pair lookup: unnecessary, per "Rebuild
  triggers".
- `image_band()` *was* extracted, contrary to this document's first guess. It is
  not duplication of `SlicedImage` — that widget derives `drop` from the area it
  is handed, while `image_band` produces the *inputs* (`skip` and the destination
  rect). Keeping them separate is what makes the clip arithmetic unit-testable at
  all, since `skip_and_drop` is private upstream.

## Design (M4 — direct placement)

### Rerouting, and why the probe cannot do it

`edamame --doctor` inside WezTerm reports `Images: iTerm2 inline images`. That is
not the iTerm2 *hint* path: `TERM_PROGRAM=WezTerm` does not contain `iTerm`, so
`is_iterm2_app()` is false and `resolve_protocol` leaves the probe alone. The probe
itself lands on iTerm2 — and WezTerm *does* answer the Kitty query (`a=q` →
`KittyImage::Query` → a `"OK"` response, `term/src/terminalstate/kitty.rs:181`), so
what decides is the response ordering inside `Picker::from_query_stdio`, and it
decides iTerm2.

Routing therefore has to come from the environment hint, exactly as
`iterm2_hint_is_trustworthy()` already does for iTerm2:

```rust
fn is_wezterm() -> bool {   // the terminals with `a=p` and no `U=1`
    env::var("TERM_PROGRAM").is_ok_and(|v| v.contains("WezTerm"))
        || env::var("WEZTERM_PANE").is_ok()
}
```

Like the iTerm2 hint, it is **distrusted inside tmux**: `update-environment` does
not carry `TERM_PROGRAM`, so a stale hint would pin a protocol the pane cannot
speak, and M4's escapes would need passthrough wrapping — which upstream enables by
*spawning* `tmux set -p allow-passthrough on`, a subprocess edamame will not spawn.
In tmux the pane keeps whatever the probe said: today's iTerm2 path, unchanged.

### What upstream gives us, and what it does not

| Need | Upstream | Verdict |
|---|---|---|
| the resized bitmap | `Resize::resize(…)` — `pub` (`lib.rs:409`) | **reuse**: the same call the M1/iTerm2 paths make, `Fit(None)` and `Nearest` (`:496`), so M4's pixels match theirs and the band's pixel grid is cell-aligned by construction |
| the cell geometry | `Resize::size_for(…)` — `pub` (`:436`) | **reuse** |
| the transmit string | `transmit_virtual` — private (`kitty.rs:224`); `Kitty::render` reachable only through the `pub(crate)` `ProtocolTrait` | **write ours** |
| the placement | placeholders only, and WezTerm has no `U=1` | **write ours** |

The transmit is ~25 lines (chunked base64 over `to_rgba8`, `f=32,t=d`; `base64` is
already a direct dependency) and buys control of both the id and the erase.
Reaching into upstream for it would mean rendering into a throwaway `Buffer` and
splitting the transmit prefix back out of the symbol — brittle, for no gain.

### The escape

Everything goes in the band's **first** cell; the rest are `Skip`. Three parts:

```
// 1 — erase the band on the terminal: ECH per row, absolutely positioned
for r in 0..rows  →  "\x1b[{dst.y+r+1};{dst.x+1}H"  "\x1b[{cols}X"

// 2 — the one-time transmit, carried by the first placement
"\x1b_Gq=2,i={id},a=t,f=32,t=d,s={W},v={H},m=1;{b64}\x1b\\"     per 3072-byte chunk

// 3 — the placement: source rect in image pixels, extent in cells
"\x1b[{dst.y+1};{dst.x+1}H"
"\x1b_Gq=2,i={id},p={pid},a=p,x=0,y={skip*font_h},w={W},h={rows*font_h},c={cols},r={rows},C=1\x1b\\"
"\x1b[{dst.y+1};{dst.x+2}H"
```

WezTerm's own field names are the spec being written against:
`assign_image_to_cells{ source_width: w, source_height: h, source_origin_x: x,
source_origin_y: y, columns, rows, z_index, do_not_move_cursor }`
(`term/src/terminalstate/kitty.rs:116-132`).

Three details are load-bearing, each with its why:

- **The cursor dance is not decoration.** ratatui-crossterm tracks a *cell*
  coordinate, not a display column (`last_pos = Some(Position { x, y })`,
  `ratatui-crossterm-0.1.2/src/lib.rs:243-247`), so a symbol that moves the cursor
  corrupts the next cell unless it ends where the backend believes the cursor is.
  The escape therefore opens with an absolute move — correct even if some other
  symbol moved the cursor — and closes at `(x+1, y)`.
- **ECH, not blank cells, erases the placeholder.** `[Image: alt]` is a *glyph* and
  glyphs draw above images, so blanks would leave it visible; and emitting blanks is
  not reliable anyway, because a cell marked `Skip` is dropped from the update stream
  whether or not its content changed (`ratatui-core-0.1.2/src/buffer/diff.rs`).
  WezTerm supports ECH (`EraseCharacter`, `wezterm-escape-parser/src/csi.rs:1170`) —
  the same primitive upstream's `clear_area` uses for iTerm2 and Sixel
  (`protocol.rs:290`).
- **`ForcedWidth(1)` on the carrying cell.** The symbol is kilobytes wide; without
  the forced width, `Buffer::diff` advances its position by `cell_width()` and eats
  the rest of the row.

### Replacement, deletion, and the ghost problem

A placement is anchored to **screen cells, not to the content**. That is what makes
the band free, and it is also what makes deletion mandatory. Re-placing the same
`(id, pid)` is a replacement — WezTerm removes the old placement on entry
(`:104`) and keys them by that pair (`:26`) — so a moving band cannot double up. But
when the image *stops* being placed, nothing removes it, and it would sit there while
the document scrolls out from under it:

| Band stops being placed because | What removes the placement |
|---|---|
| scrolling (`is_scrolling`) | the delete rides the **first scratch cell**: during scroll the scratch repaints every frame, so the carrier is guaranteed to be emitted |
| a modal is open | same carrier |
| the reserved rect left the viewport entirely | nothing is needed: the placement is off-screen too. It is deleted on the frame it next becomes visible *and* unplaced |
| the block is gone (edit, nav, reparse) | the delete is queued, then appended to the document area's **first cell** marked `ForcedWidth(1)` — appending changes that cell's symbol, and a changed non-skipped cell is always emitted, so the queue drains; the appended escapes neither draw nor move the cursor |

`paint_images` already runs once per frame with the full snapshot list, so the queue
is a set difference over the URLs it placed this frame versus last — the same
"honored only on the immediately following frame" discipline `NativePaint` uses.

### What M4 does not need

- **No `ThreadProtocol`.** The pair's `native` stays `None`: there is no PNG to
  encode and none to re-send, which is exactly M3's flash this route exists to
  avoid. The halfblocks scratch still builds, unchanged, for the scroll and modal
  gates.
- **No rebuild on a band change.** The band is a placement parameter, so the
  transmit string is built once per `(url, width, rows)` and re-used. The image id
  hashes the URL **and the geometry**, so a rebuild re-transmits into the *same*
  id instead of leaking a new one — strictly better than M1's limitation 1 — and
  two blocks showing one image at one size share a transmit while placing under
  **their own** placement ids (`placement_id(block_idx)`). A single constant there
  would be wrong twice: the second block of a repeated image would replace the
  first's placement and render nothing, and a delete could not name one placement
  without risking a sibling's.
- **No new config surface, no `o=z` compression** (upstream sends raw RGBA too),
  **no sub-cell `X`/`Y` offsets** (WezTerm parses them as `u32`; the band is always
  cell-aligned), **no tmux** (out of scope above).

## Changes by file

| File | Change |
|---|---|
| `src/image/loader.rs` | `LoadedImage` gains `sliced: Option<(Rect, SlicedProtocol)>` |
| `src/app/image_dispatch.rs` | populate `sliced` in the existing scratch-build block (`:551`), gated on the picker speaking a band protocol (`Kitty \| Sixel`), inside the same `catch_unwind` |
| `src/image/cache.rs` | `ImageCache` gains `prebuilt_sliced: HashMap<(String, u16, u16), SlicedProtocol>`; `ProtocolPair` gains `sliced: Option<SlicedProtocol>`; the band cold path claims the prebuilt entry and **skips building the `ThreadProtocol`**, so there is no wasted encode and no duplicate 1.3 MB payload |
| `src/ui/image_view.rs` | new `image_band` (the clip arithmetic) and `paint_sliced`; `paint_images` routes the band protocols through it before the `use_native` gate. `fully_visible` stops applying to them; `is_scrolling` and `modal_open` still do |
| `src/image/mod.rs` | re-export `SlicedProtocol` / `SignedPosition` |
| `src/app/event_loop.rs` | pass `loaded.sliced` into `set_decoded_with_prebuilt` alongside `loaded.scratch` |
| `docs/dev/media-export.md` | add a bullet to that file's invariants list recording that the band protocols paint at paint time and that the partial-visibility → scratch fallback no longer applies to them |

The M2 rows — the M1 identifiers above were renamed in the same pass, because one
field and one paint function now serve both protocols:

| File | Change |
|---|---|
| `src/image/cache.rs` | `is_band_protocol(ProtocolType)` becomes the single gate that `build_sliced` and the cold path both consult, so the two cannot drift; `build_kitty_sliced` → `build_sliced`, `ProtocolPair::kitty_sliced` → `sliced`; `native` stays `None` for Sixel exactly as for Kitty |
| `src/ui/image_view.rs` | the `paint_images` routing arm takes `ImageProtocol::KittyGraphics \| ImageProtocol::Sixel`; `paint_kitty_sliced` → `paint_sliced`. The band arithmetic is untouched — `image_band` is protocol-agnostic by construction, which is the interface paying off |
| `docs/terminal-compatibility.md`, `docs/dev/windows.md` | the Windows Terminal row and the platform note: what sixel terminals get, and that nobody has watched it there |

The M4 rows:

| File | Change |
|---|---|
| `src/image/kitty_direct.rs` (new) | the writer — `transmit`, `place`, `delete` and the ECH sweep as pure string builders, plus the cell/pixel geometry they encode. Every function is a pure function of `(id, geometry, band)`, which is what makes M4 testable without a terminal |
| `src/image/loader.rs` | `LoadedImage` gains `direct: Option<(Rect, DirectPlacement)>` |
| `src/app/image_dispatch.rs` | populate `direct` in the same block that builds the scratch and the sliced protocol, inside the same `catch_unwind` |
| `src/image/cache.rs` | `ImageCache` gains `prebuilt_direct` keyed like `prebuilt_sliced`, the per-URL id map, and the live-placement record the delete queue is a set difference over; `ProtocolPair` gains `kitty_direct: Option<DirectPlacement>` and its `native` stays `None` |
| `src/terminal/capabilities.rs` | `ImageProtocol::KittyDirect`, the `is_wezterm()` hint with its tmux distrust, and the doctor label |
| `src/ui/image_view.rs` | `paint_direct_placement` (band → one escape in the first cell, `Skip` the rest), the delete queue and its carrier, and the `paint_images` routing arm |
| `docs/terminal-compatibility.md` | the user-facing row: what WezTerm gets, and what it costs |
| `docs/dev/media-export.md` | the invariants bullet for the placement lifecycle — who removes a placement, and why nothing else will |

## Verification

1. **The clip arithmetic** (unit, `image_band_reports_the_visible_slice`): six
   cases — fully visible, top-clipped, bottom-clipped, both-clipped, off the top,
   off the bottom — plus two against a viewport that does not start at row zero,
   which is what catches an implementation measuring against the screen instead of
   the document area. `skip_and_drop` is private upstream, so this tests the inputs
   we derive, not upstream's `drop`.
2. **Paint routing** (integration, `kitty_paints_a_clipped_image_as_a_band`): a
   clipped snapshot writes a `\u{10EEEE}` placeholder — the row-addressed path ran,
   not the halfblocks scratch.
3. **No rebuild across a band change** (same test): the first frame carries the
   payload (`_Gq=2`) and the clipped frame does not, which is only possible if the
   same `SlicedProtocol` was reused. That pins the no-rebuild property and
   limitation 1 together, and it subsumes the reveal case — a reveal does not move
   the key at all ("Rebuild triggers").
4. **The two surviving gates**
   (`kitty_yields_the_band_while_scrolling_and_under_a_modal`): asserted with a
   *fully visible* image, so that only the gate under test can explain a scratch
   paint.
5. **The protocol gate and the prebuilt claim**
   (`build_sliced_answers_for_the_band_protocols_only`,
   `kitty_prebuilt_is_claimed_at_matching_dims_and_skips_the_threaded_protocol`) —
   the second of which also pins that Kitty builds no threaded protocol.
6. **Cold-path cost instrumentation** — landed as its own commit *before* the
   feature (`perf: time the synchronous halfblocks scratch fallback`), covering
   the scratch and, once it existed, the band build, under the existing
   `[dev] logging` flag. The numbers have not been read: no Kitty terminal was
   available for it.
7. **Regression**: existing assertions
   (`two_native_images_transmit_once_then_go_quiet`,
   `a_scratch_frame_forces_the_next_native_frame_to_retransmit`, …) pass
   unchanged — the `]1337;File=`-based ones are iTerm2-only. `cargo test
   --no-fail-fast` gives 3188 passed / 0 failed / 12 ignored against a 3183/0/12
   baseline for M1 — exactly the five added tests, so nothing else moved. The
   tree with M4 and M2 on it reports 3207/0/12, of which M2's share is exactly two
   tests (3205 with `--skip sixel`); the one M2-era rename is
   `build_kitty_sliced_only_answers_for_kitty` → `build_sliced_answers_for_the_band_protocols_only`,
   not a new test. `cargo clippy --all-targets -- -D warnings` is clean, as is
   `cargo fmt`.
8. **The manual check was attempted on real hardware and could not be completed —
   and the attempt is worth recording.** WezTerm was available for the attempt and
   does implement the Kitty graphics protocol, yet it is not a usable target:
   `edamame --doctor` inside it reports `Images: iTerm2 inline images`, so
   `native_picker.protocol_type()` is never `Kitty` and the band path never
   engages. Forcing the picker to Kitty (a throwaway patch, reverted) produced
   literal `\u{10EEEE}` placeholder glyphs with **no image composited at all** —
   in both the fully-visible and the clipped case. So WezTerm does not implement
   the unicode-placeholder extension, which is exactly why ratatui-image's own
   environment inference classifies it as iTerm2. Verifying M1 needs a terminal
   `edamame` resolves to Kitty — kitty or ghostty — and neither was available.

   What the attempt *did* establish, on real hardware:
   - the unrepaired case, reproduced: a fully visible image renders sharply
     through the iTerm2 path, while the same image only partly on screen renders
     as a coarse halfblocks mosaic;
   - WezTerm lands on the iTerm2 backend, so a WezTerm user is the **direct
     placement (M4)** audience — not the M1 one, and not necessarily M3's either.

9. **M4's escape geometry** (unit, in `src/image/kitty_direct.rs`): the band's
    source rect (`y = skip*font_h`, `h = rows*font_h`), the cell extent, the ECH
    sweep per row, the chunking (3072-byte payloads, `m=1` on all but the last) and
    the closing cursor position. Pure string assertions — the one part of M4 that
    can be pinned without a terminal.
10. **The band is a placement parameter** (integration): a clipped frame emits
    `a=p` with a non-zero `y` and **no** transmit, while the first frame carries the
    transmit. The M1 no-rebuild property, re-checked for M4.
11. **The delete queue drains** (integration): a frame that stops placing emits the
    delete, and the next frame's queue is empty.
12. **On real hardware — the check M1 could not complete.** WezTerm was available
    and is M4's one target, and the run is recorded. `--log` reports
    `image_protocol=Some(KittyDirect)` and the decode worker finishing `ok=true`;
    a partially scrolled image then renders at native fidelity in the window — the
    dog photo from `tests/fixtures`, sharp, with no `[Image: alt]` text over it and
    no halfblocks mosaic, which is the blur this document exists to fix. M1's own
    check is still outstanding: it needs a terminal edamame resolves to Kitty
    (kitty or ghostty), and neither was available.
13. **What the hardware run did *not* cover.** The delete path was exercised only
    against the terminal's *store*: scroll-away and scroll-back were not driven
    by hand, so "no ghost left behind" rests on the escape being what WezTerm's
    `KittyImageDelete::ByImageId` expects and on `reconcile_placements` being
    called every painted frame (`an_unpainted_direct_placement_is_deleted`), not
    on having watched it. Driving a TUI's scroll needs synthesized input, which
    is doable but not reliable enough to call proof.
14. **M2, the Sixel band** (`sixel_paints_a_clipped_image_as_a_band`): a clipped
    snapshot's cell carries a `\x1bP` payload rather than a halfblock glyph, so
    the band path ran; the top-clipped payload differs from the fully visible
    one, so the band is a function of the clip; and a reserved rect *taller than
    the viewport* — the permanent case the issue opened with — carries exactly
    the bands the same image fully on screen does, and nowhere near the bands its
    own encoding has. The test was checked against the pre-fix code (the routing
    arm restricted to Kitty) and fails there with a halfblock glyph in the cell,
    which is the regression it defends. `build_sliced_answers_for_the_band_protocols_only`
    and `sixel_prebuilt_is_claimed_before_it_becomes_a_threaded_encode` pin the
    build gate and the skipped `ThreadProtocol`.
15. **M2 on real hardware — Windows Terminal 1.24.11911.0.** With
    `COLORTERM=truecolor`, a partly scrolled image there now renders at its true
    resolution instead of the coarse halfblock mosaic, which is the check this
    route exists for. WT is the one target none of the other routes can reach: it
    implements no Kitty graphics (no APC handler; its only "kitty" is the keyboard
    protocol) and it does advertise Sixel in DA1, which is why `edamame --doctor`
    already reported Sixel there before any of this branch.
16. **What M2 was *not* verified against.** foot and xterm with sixel enabled —
    the other terminals the route covers — and the band's 6-px slop, which is
    reasoned from the format and measured in the payload rather than eyeballed:
    while an image is clipped it can sit up to one band off the cell grid. The
    suite proves the payload and the routing, not the terminals' rendering of
    them; scroll-back after an image leaves the viewport was not driven by hand.

## Known limitations

1. **Random id per build, no delete.** Every rebuild leaves the previous image id
   resident in the terminal until Kitty evicts it. The build-geometry decision
   above leaves a terminal resize as the only trigger — removing the
   reveal-driven rect change is exactly what it buys — so the leak is bounded by
   resize count rather than by cursor movement. Fixing it properly needs a
   deferred `d=I` queue flushed on the next frame.
2. **`SlicedProtocol` must be `Send`** to cross the decode worker's channel.
   Expected (the `Kitty` payload is `Arc<AtomicBool>` + `String` + `Size`), but
   it is the first thing to confirm in code — if it fails, the fallback is to
   ship the `Kitty` alone and wrap it into `SlicedProtocol::Kitty` on the UI
   side (the enum's variants are public).
3. **Kitty-compatible terminals without unicode placeholders.** M1 renders
   *nothing* on them — the placeholders are the drawing mechanism — so
   `resolve_protocol` (`src/terminal/capabilities.rs`) maps a probed `Kitty` to
   `Iterm2` under the iTerm2 hint, and ratatui-image's own environment inference
   already lands WezTerm there (Verification 8: forcing Kitty on it yields literal
   placeholder glyphs and no image). M4 is the answer for such a terminal, but only
   for one the hint names (WezTerm): the routing is an environment hint rather than
   a query, because the terminal supports the protocol and lacks a sub-feature of
   it. Another terminal with the same gap keeps M1's outcome — halfblocks whenever
   the image is not fully visible — until it is added to the hint.
4. The sliced path renders at most one viewport's worth of rows natively; an
   image taller than the terminal cannot show more than a screenful at once.
   That is inherent, not a regression.
5. Sixel band granularity is 6 px, so its `skip` is approximate (M2). Measured:
   a 20 px cell height is 3⅓ sixel bands, so a one-row scroll skips 18 px — 3 of
   the fixture's 14 bands where the model says 4 — leaving the image up to 6 px
   off while it is clipped. The band's destination row and its `clear_area` sweep
   stay exact; only the image content inside them can slide. It is the one
   fidelity M1 and M4 give up nothing on.
6. **One synchronous build per image per new geometry.** After a resize nothing
   re-derives the prebuilt map (`request` is a no-op once a URL is decoded), so
   each visible image pays one synchronous Kitty build on the UI thread. Bounded
   and once-per-resize, but estimated rather than measured — Verification item 4
   is the gate, and "Rebuild triggers" holds the alternative.
7. **M4 has no data-delete path.** Its deletes are placements-only (`d=i`), so an
   evicted image stays resident in the terminal — the same leak as limitation 1,
   and it would take the same fix (a deferred `d=I` queue). The choice is
   deliberate: `d=I` would also mean re-transmitting on every scroll back.
8. **M4 is not covered under tmux.** The hint is distrusted there on purpose:
   passthrough would have to be enabled by spawning `tmux set -p
   allow-passthrough on`, which edamame does not do. A tmux pane under WezTerm
   keeps the iTerm2 path — working, and blurry when partially visible.
9. **Terminals with a different `--class`-invisible identity.** The hint reads
   `TERM_PROGRAM`/`WEZTERM_PANE`, so running edamame *over ssh into* a WezTerm
   desktop does not get M4 (neither variable is forwarded by default) — correct,
   since the graphics go to whatever terminal is local.

## Alternatives considered

- **Poke a skip parameter into `paint_native`.** Impossible: the primitive is
  `pub(crate)` and `StatefulProtocol` has no skip entry. Would require vendoring
  the protocol writers.
- **Band re-encode as the *interface* (i.e. for every protocol).** Uniform, but it
  puts the band in the cache key and re-encodes on every scroll settle — needless
  where row addressing or direct placement makes the band free. Rejected as the
  interface; kept as the iTerm2 backend (M3), where it is the only option.
- **A second encoder channel for the sliced build.** Rejected: the decode worker
  already produces the analogous `prebuilt_scratch` and already holds every
  input, so a second channel and worker are pure duplication.
- **Synchronous build in `get_protocol_pair`.** Rejected — it would put the
  1.3 MB string build on the UI thread. Kept only as the rare-fallback path, and
  even there it should be avoided (see "Rebuild triggers").
- **Clamp `images.max_height` to the document area.** Removes the permanent case
  cheaply, but changes layout semantics, forces a reparse, and still leaves the
  scrolled case blurry — and the band interface subsumes it.
- **Switch halfblocks to the sliced backend too.** No fidelity gain; the
  existing scratch path is the same row copy.

## Staging

| | Scope | Trigger to do it |
|---|---|---|
| **M1** | Kitty backend (this branch) | now |
| **M2** | Sixel backend (`SlicedSixel`); `image_band()` shared with M1 rather than extracted separately | **taken — this branch.** Windows Terminal has no Kitty graphics at all and advertises Sixel in DA1, so it was the terminal the blur still applied to, and the route needed no WT-specific code |
| **M3** | iTerm2 band: crop the visible rows and re-send them. The only route for iTerm2 proper | not first — see M4, and the flash cost it carries |
| **M4** | Kitty **direct placement** (`a=p` with a source rect) — the *free* band for terminals that have `a=p` but not `U=1`, WezTerm being the one to hand | **taken — this branch.** It is the route for the very terminal the blur was reported on, and the only one of the four that can be verified end-to-end here |

**M1, M2 and M4 are taken on this branch; M3 is what remains, and for iTerm2
alone.** M1 and M4 cover Kitty-shaped terminals and M2 the Sixel ones, so between
them the bug is fixed for every terminal that has *some* band mechanism — which
leaves iTerm2 the only protocol that must re-send, and its users still get the
blur until M3 lands.

Side by side, because the difference is easy to lose:

| | **M3** — crop and re-send (iTerm2) | **M4** — direct placement (`a=p`) |
|---|---|---|
| Where the band lives | **in the encoding** — a band is a new PNG | **in the placement** — a source rect on an image already sent |
| One-time cost | 1 encode for the full image | 1 encode + 1 transmit string |
| Cost per band change | 1 PNG encode on a worker **+ a full re-send** | one short escape, written at render time |
| Visible artifact | **the flash** — `clear_area` (ECH) blanks the rows, then the PNG redraws | none; nothing is re-sent |
| Threading | cannot be render-time → worker, channel, stale-request handling | **render-time**, on the UI thread — no worker |
| Upstream support | **reused** — `Iterm2::new(cropped, …).render(dst, buf)` | **none** — ratatui-image's Kitty backend is placeholder-only and `render_with_skip` is `pub(crate)`, so the APC sequences are ours |
| Accounting | one payload cell per band; `NativePaint` / `mark_rect_skipped` apply unchanged | one placement cell; needs `a=d` on eviction/resize and placement-id bookkeeping |
| Terminal support | **universal** — anything that speaks OSC 1337, iTerm2 proper included | only `a=p`-with-source-rect terminals → WezTerm today |
| Correctness trap | the crop must come from the **resized** bitmap, not the original (a band's aspect ratio is not the image's, so `Fit` would rescale it) | none — the rect is in image pixels and the terminal crops |

The one-sentence root: **M3 makes the band part of the encoding; M4 makes it a
parameter of a placement.** That is why M4 is free and M3 is not — and why M4
cannot exist for iTerm2, whose protocol has no source rectangle at all.

Also worth landing independently of all three, as measurement rather than
mechanism, and distinct from Verification item 4's fallback-cost log: log the
`:348` decision (`protocol`, `fully_visible`, `is_scrolling`, band numbers, image
pixel height) under the existing `[dev] logging` flag, so the split between the
transient scroll window and the permanent cases is known rather than assumed.

## Resolved decisions

- Build strategy: on the **decode worker**, mirroring `prebuilt_scratch` — not
  synchronous in `get_protocol_pair`, and not a new encoder channel. Keeps the
  encode off the UI thread, matching the existing invariant and precedent.
- Interface: the **band is a render-time parameter shared by all backends**, and
  the only paint path — `fully_visible` is deleted rather than branched around.
- The Kitty protocol is keyed by the snapshot's `(url, width, height)` **like
  everything else**. The geometry indirection this document originally called for
  turned out to be unnecessary: a reveal does not change the image rect's height
  ("Rebuild triggers" carries the proof), so a resize is the only miss.
- `is_scrolling` and `modal_open` still gate Kitty. The planned removal of the
  scroll gate was wrong: it exists for re-compositing, not for re-encoding.
- `get_protocol_pair` takes **no new parameter**; the Kitty test is
  `native_picker.protocol_type()`, which already reflects `resolve_protocol`'s
  override.
- The post-resize synchronous build is **accepted and measured** rather than
  engineered around up front; re-deriving the prebuilt off-thread (on the
  existing decode worker) is the documented follow-up if the number is bad.
