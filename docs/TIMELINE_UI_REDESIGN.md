# Timeline & Layer-Header UI Redesign — Implementation Contract

**Status:** design approved 2026-06-27 (Peter), implementation in progress.
**Provenance:** worked out against a DaVinci Resolve reference + an iterative HTML mockup
(`scratchpad/timeline-mockup.html`, rebuild from this doc if the scratchpad is gone).
Peter explicitly likes the mockup. This document is the authoritative spec — implement to it,
do not re-derive from the old UI.

**North star:** Resolve-grade legibility + Ableton-style solid-colored lanes, on MANIFOLD's
beat-native timeline. The timeline is the root of the instrument; it must read at a glance under
stage conditions. A timing/legibility bug becomes the show.

---

## A. Contrast pass — design TOKENS, UI-WIDE, do this FIRST

**Root problem Peter named:** the whole UI is "muted low contrast on muted colours." The neutral
value scale is compressed (~8 levels, all in the dark band), so state/selection only nudges hue
*inside* the band and disappears. The fix is not a better tint — it is **contrast as an axis**.

**This lands at the token level** (the design-system value scale, `color.rs` / token source), so it
propagates to inspector, graph editor, transport — NOT a timeline-only patch.

Approved token values:

| token | value | role |
|---|---|---|
| `bg` | `#08080a` | deeper black so raised surfaces separate |
| `lane` | `#141418` | lane base |
| `lane-alt` | `#101014` | alternating lane |
| `chrome-0` | `#1e1e23` | header column base — genuinely lifted surface |
| `chrome-1` | `#28282f` | raised elements |
| `chrome-2` | `#37373f` | buttons, clearly raised |
| `line` | `#43434e` | dividers that are actually visible |
| `line-soft` | `#2c2c34` | beat grid lines |
| `line-bar` | `#4c4c57` | bar grid lines |
| `txt` | `#f5f5f7` | primary text |
| `txt-dim` | `#b4b4be` | labels — readable, not faint |
| `txt-faint` | `#80808c` | faint but still legible |
| `select` | `#5aa6ff` | selection (reserved) |
| `solo` | `#56db8c` | solo (reserved) |
| `mute` | `#ff6b6b` | mute (reserved) |
| `record` | `#ff4040` | record (reserved) |
| `playhead` | `#ff4d4d` | playhead (reserved) |
| `chip` | `#1b1b21` | **opaque neutral control surface on coloured headers** |
| `chip-line` | `rgba(255,255,255,.16)` | hairline border for chips |

Four levers: (1) spread the neutral value range; (2) brighter text at every tier; (3) visible
borders/dividers; (4) state escapes the muted band (saturated selection, M/S light up).

---

## B. Layer header — ONE grammar, THREE heights

**One skeleton for every track type** (text / video / generator / group / audio):

- **identity row:** fold `▶/▼` + type-badge + name + `☰` menu
- **mix row:** `M` `S` `L` + blend chip (blend slot → **Gain** on audio tracks)

Type badges: `T`=text, `▦`=video, `◇`=generator, `▤`=group, `∿`=audio.

**Three heights:**

- **collapsed (~26px):** name bar only.
- **compact (~58px):** identity + mix. The default.
- **expanded (~200px):** identity + mix + routing form. Tall is *intended* — Peter likes
  focusing on a single layer. Do not shrink it.

Fold `▶/▼` toggles expanded (reveals routing). Resizable/collapsible lane height already exists
in the app — reuse it; these are its discrete stops.

`M`=mute, `S`=solo, `L`=LED. `L` is greyed (`dead`, disabled-not-absent) where not applicable.
**OPEN:** confirm exact `L` semantics and where it applies before baking in.

**Today's header is the anti-pattern** to replace: ~180px flat stack that mixes live controls with
set-once config, and Gen vs Layer use *different* skeletons. **Delete** the `+ Clip` button and the
`N clip` count — Peter confirmed they are old dev tools.

---

## C. Headers are SOLID identity-coloured (Ableton)

Header background = the layer's identity colour, **full fill** (not a thin accent stripe). The app
already does this and Peter wants it kept.

Controls sitting on a coloured header use the **opaque neutral `chip`** surface — never a
translucent darken of the hue (a 28%-black overlay on purple is just darker purple → reads
hue-on-hue, the same low-contrast trap). **Rule: header = identity colour; controls = neutral
chrome, never tinted by the hue.** Active states fill solid: `M`→`mute`, `S`→`solo`, `L`→`solo`
(LED active). White/light glyphs on the neutral chips.

---

## D. Routing lives on the LAYER, expanded-only

`Folder` / `MIDI` / `Channel` / `Device` render as an aligned label/value form: a fixed-width
label column, values aligned in a second column, one dropdown vocabulary (`▾`). Shown **only when
the lane is expanded.**

**Architecture (Peter's call):** routing is a property of the *layer* → it belongs on the layer
controls. The **inspector is for effect/generator cards ONLY** — do **not** push routing to the
inspector. Spell labels out at expanded width: `CH`→`Channel`, `DEV`→`Device`.

---

## E. Clips = inset cards, layer-coloured (Ableton)

- **Preview (thumbnail) on TOP, name strip on BOTTOM** (Premiere/FCP convention — Peter's call).
- **Strip + border + card frame = the LAYER's identity colour** (Ableton clip-colour). The
  thumbnail *body stays as content* (untinted) so the clip is still readable — this is the
  Resolve/Premiere way of doing Ableton clip-colour when you also have thumbnails. Optional future:
  a subtle layer-colour wash over the thumbnail if Peter wants it stronger.
- **Selected clip** = crisp border + lift (rises above neighbours).
- Clip thumbnails already exist (§24 5c snapshot-on-play atlas work).

---

## F. Aspect-locked preview window (NET-NEW, future layer onto the cards)

Thumbnail width must = **lane_height × project aspect ratio**, decoupled from bar width. Today the
thumbnail is "1 bar wide", so its shape is a slave to zoom — at 2px/beat a bar is ~8px, so you get
~18 squished slivers instead of one clean window. Rule:

- long clip zoomed in → **tile** fixed-aspect windows (filmstrip);
- zoomed out → one window + layer colour for the rest;
- short clip → clamp/crop, then fall back to a colour block when there is no room.
- Show **output aspect** (what the audience sees), not source aspect.

Not in the mockup yet. Layers onto the clip cards from §E.

---

## G. Group nesting (NET-NEW feature, scope on its own)

Children **indent** under their parent with a vertical **spine**; the parent shows a `N layers`
summary row. Today the timeline only has folder *assignment* (the `Folder` dropdown) — not visual
nesting. This is real feature work: it touches lane layout **and** the data the header reads, not
just styling. Respect the `nodeId` safety invariant from the grouping docs.

---

## H. Selection treatment

Because headers are solid identity-coloured, selection **cannot be a colour fill** (it would fight
the identity colour). Therefore:

- **Selected LAYER** = bright focus **ring** (inset ~2px `#e6f0ff`) + a lift on the coloured header,
  plus bright top/bottom edges on the lane row. Reads as a band spanning header + lane.
- **Selected CLIP** = crisp border + lift on the one card.

Same concept, two scopes — band (layer) vs box (clip). **OPEN:** final ring strength; may push
brighter/thicker for stage legibility.

Colour reservations: red/blue/green = playhead / selection / solo. The only other saturated colours
in the timeline are the identity colours (headers + clips).

---

## I. Grid, playhead, depth

- Grid lines subtle, drawn **behind opaque clip cards** so they never bleed through content. (The
  "grid through clips" Claude first saw was actually the 1-bar thumbnail tiling — the §F
  aspect-window fix removes it.)
- Playhead red, on top of everything; selected clip lifts above neighbours.

---

## Implementation plan (filled after the code audit — see §J)

Split every item above into one of three buckets:

1. **Token change** (A) — value-scale revision in the token source; verify it propagates UI-wide.
2. **Pure restyle** (B header grammar, C solid headers + neutral chips, D routing form layout,
   E clip card colours, H selection) — rework existing render code, no new model data.
3. **Net-new** (F aspect window, G group nesting) — needs new layout + new data the header/clip
   reads. Scope and land each independently, after the restyle.

Order: **A (tokens) → B/C/D/E/H (timeline restyle) → G (nesting) → F (aspect window).**
Build + `clippy` + focused tests after each step; full workspace test on the token change and any
shared-render change. Verify visually by rendering the native UI headless → PNG → read it
(see `reference_ui_headless_png_verification`).

## §J. Code audit — manifold-ui rendering architecture

**Can we build it: YES.** Everything in the mockup maps to existing capabilities. Much is already
built; the work is targeted restyle + a real selection treatment + a token tune, not a rewrite.

**Render paths.** Two. (1) **UITree** — declarative `tree.add_button/add_panel/add_label` with a
`UIStyle`; the renderer (`manifold-renderer::ui_renderer::UIRenderer`) rasterizes it. Layer headers
+ chrome use this. (2) **Immediate `Painter`** (`draw.rs`) — graph canvas only. Headers = UITree.

**`UIStyle` capabilities** (`node.rs`): `bg_color`, `hover/pressed_bg_color`, `text_color`,
`border_color`, `border_width`, `corner_radius`, `font_size`, `font_weight`, `text_align`.
→ selection ring, clip-card border, chip hairline all = `border_*` on a node. **No box-shadow** in
UIStyle (clip lift-shadow is GPU-side, §24 5b) — fine, the ring carries selection.

**Tokens** (`color.rs`): `Color32` consts, one documented source of truth, with a **deliberate
dark-stage philosophy**: "high contrast = distinct LEVELS, not a bright UI; a bright UI is fatiguing
on stage and glows in a dark room." Grey ramp `BG_0..BG_3` = 13/22/31/42 (already a §15 spread).
→ Phase A is a **TUNE** (wider spread, brighter text `txt-dim/faint`, distinct selection, neutral
`chip`), NOT a wholesale swap to the mockup's bright hexes. Keep it dark; validate by eye. Guarded by
`tests/design_tokens.rs` + the ramp PNG in `manifold-renderer/tests/ui_color_swatches.rs`.

**Layer header** (`panels/layer_header.rs`): a `LayerControl` enum (29 variants:
Background/AccentBar/Connector/BottomBorder/Chevron/TypeBadge/Name/DragHandle/GenType/Mute/Solo/Led/
Blend/Separator/Info/Folder/PathLabel/NewClip/Midi*/Ch*/Dev*/AddGenClip/Gain/Send/Analysis) drives
layout (`compute_layer_row`), build (`build_layer_row`), and hit-test from one descriptor list.
Already true to the mockup:
- **Headers are already solid layer-coloured** — `bg_style()` sets `bg_color = layer.color`.
- **Type badges exist** (§24 5d, `TypeBadge` + `badge_icon()`).
- **Group nesting partly exists** — `AccentBar` + `Connector` + `BottomBorder` + `CHILD_INDENT`(20)
  already draw the indent/spine. §G is mostly styling, not net-new.
- Controls already use neutral-ish surfaces (`state_button_style`, `field_style`=`LAYER_ROW_BG`).
Gaps to fix:
- **Selection = `lighten(layer_color, 30)`** (`bg_style`) — THIS is the "muted on muted"
  non-distinct selection. Replace with a focus **ring** (`border_color/width` on `Background`) + the
  `FOCUS_LIFT_STEP` lift. Biggest single win.
- **Remove `NewClip` + `Info`(clip count)** controls (the "+ Clip" / "N clips" dev tools).
- **Routing relayout** — Midi/Ch/Dev/Folder are crammed into shared rows; restyle into the aligned
  label/value form, expanded-only.
- Verify control-chip distinctness on coloured headers (neutral `chip`, not hue-tinted).

**Heights** (`coordinate_mapper.rs`): `TrackHeight::{Collapsed 48, Normal 140, Tall 200}`. `Tall`
defined but never selected. Two functional tiers exist today (collapsed = identity+mix; normal =
full). §B three-tier maps onto these; wire `Tall` for the roomy expanded state if wanted.

**Clips:** currently **neutral grey** (`CLIP_NORMAL` 173,168,163), NOT layer-coloured. Rendered
GPU-side as SDF rounded rects (§24 5b: body gradient + border + lift; luminance-aware label). So
§E "clips match layer colour" = feed `layer.color` into the clip body (real change, GPU/viewport
side); title-position + selection border live there too.

**Headless render → PNG → compare loop** (the verification the redesign rides on):
`manifold-renderer/tests/headless_ui_spike.rs::render_to_png(&device, &mut ui, &tree, path)` —
`GpuDevice::new()` windowless + `UIRenderer` rasterizes a `UITree` → PNG. Build a `LayerHeaderPanel`
with mockup-like `LayerInfo` rows → render → `Read` the PNG → diff against `timeline-mockup.html`.
Re-render after every change.

### Buckets + order
- **Token change (A):** `color.rs` tune — ramp spread, `txt-dim/faint` up, selection colour, `chip`.
- **Restyle (B/C/D/E/H):** `layer_header.rs` (selection ring, drop NewClip/Info, routing form, chip
  check), clip GPU render (layer colour, title-bottom, selection border).
- **Net-new (F, G-polish):** F aspect-locked thumbnail window (GPU/viewport); G nesting = polish.

**Order:** A (tokens) → **H selection ring** (biggest visible fix) → B/C/D header restyle →
E clips → F aspect window. Render→PNG→compare against the mockup after each. Build + `clippy` +
focused tests each step; full workspace on the token change + shared-render changes.

### Progress log
- **§H shipped** (commit 69253f5): selection = bright `SELECTED_LAYER_RING` ring + small lift,
  replacing `lighten(30)`. Render-confirmed; the ring reads clearly on any header hue.
- **§C shipped** (commit bb6be36): dropped `Info` (clip count) + `NewClip` + `AddGenClip` from
  `compute_layer_row` **and** its `oracle_row` equivalence gate (kept rect-equal), widened the folder
  path label, removed the dead width consts. `layout_matches_frozen_oracle` + 426 lib tests pass.
- **Render harness**: `cargo test -p manifold-renderer --test timeline_header_preview` →
  `scratchpad/native_header_baseline.png`. Uses `ScreenLayout` with `timeline_split_ratio = 0.96`
  and a 256×1100 texture to crop to the bottom-anchored layer-controls panel. `Read` the PNG to
  compare against `timeline-mockup.html`.

### §E status (clips — mostly already done)
Clips render GPU-side via `manifold-renderer/src/clip_draw.rs` (`ClipBody` → SDF rounded rect: body
gradient + border + lift, §24 5b), built by the viewport panel. **Audit correction:** clips are
ALREADY layer-coloured — `get_clip_color` (`bitmap_painter.rs`) returns the layer colour for a normal
clip; selected = `lighten(30)` **plus** a blue `CLIP_BORDER_SELECTED` outline (a distinct signal, so
clip selection is fine). `CLIP_NORMAL` grey is just a fallback. So §E "layer-coloured clips" needs no
work.

Remaining delta: **title position.** `emit_clip_names` currently CENTRES the label
(`ty = rect.y + (h-font)/2`); the mockup wants it on the BOTTOM. The change is one line
(`ty = (rect.y + rect.height - font - 3).max(rect.y)`) but it affects EVERY clip and the centred
behaviour is deliberate — so verify it on a render first, per Peter's PNG-compare rule. The verify
harness is heavier than the header one: it needs `ClipBody` (trivial) **and** `ClipScreenRect` for the
names (`ClipId`, `Beats`, `Arc<str>` name, `waveform: None`, the audio fields). Build that harness or
check on the running app before shipping the title move. Optionally re-style clip selection to match
the new layer focus-ring. Tuning knobs: `CLIP_*` / `CLIP_LABEL_*` in color.rs.

### Net assessment
The native UI already implements most of the mockup (solid layer-coloured headers, type badges,
layer-coloured clips, group indent/spine, resizable track heights). The mockup was largely a
reproduction of existing behaviour plus the two real gaps that are now **shipped** (§H distinct
selection, §C declutter). What remains: title-bottom (verify-then-ship), the routing-form relayout
(§D, coupled with the §B tall tier), the UI-wide contrast/text tune (§A — needs Peter's eye on the
running app), and the genuinely net-new aspect-locked thumbnail window (§F).

### Known issues (pre-existing, not from this work)
- `tests/design_tokens.rs` guard is at **133** vs baseline **132** — an untokenized `Color32::new`
  drifted in on the `feat/multi-selection-ux` base this branch forked from. Find + tokenize it (or
  bump the baseline if legitimate) as a separate cleanup; it is inherited, not introduced here.
