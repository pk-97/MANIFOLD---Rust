# UI SOTA Upgrade — Implementation Plan

**Status:** spec, ready to build. **Captured:** 2026-06-26.
**Execution SSOT** for taking the UI from "competent but flat" to best-in-class. The *rationale*
for each piece lives in [UI_DESIGN_SYSTEM_AND_INSPECTOR_REDESIGN.md](UI_DESIGN_SYSTEM_AND_INSPECTOR_REDESIGN.md)
(§15/§17/§18/§19/§24); this doc is the **sequenced, grounded build plan** — current state, exact
changes, files, verification, acceptance, and risk per phase.

Grounded against the live code 2026-06-26 (the design doc had stale status markers; this one is
checked, not trusted).

---

## 0. The goal

Three moves carry the whole upgrade:
1. **Colour means something** — chrome goes neutral grey; colour is reserved for *identity*
   (track/clip) and *state* (one ramp per hue).
2. **Depth from value, not flatness** — header column, card headers, and the selected object lift
   via value steps + one soft shadow for floating elements.
3. **Clips you can read** — name strip + content preview + real boundary, on the GPU pipeline.

**Honest caveat (carried from §20):** Phases 1–5 get the *system* to SOTA-grade — consistent,
enforced, complete. They do **not** guarantee the *look* is best-in-class; that's the taste pass
(Phase 6), settled only by eyeballing the running app. The system is the floor, not the ceiling.

---

## 1. Invariants that hold across every phase

- **Tokens, never literals.** Every colour/radius/spacing references a `color.rs` token. The §16
  ratchet ([`tests/design_tokens.rs`](../crates/manifold-ui/tests/design_tokens.rs)) fails CI on new
  raw literals — it's already on.
- **Aliases, not churn.** Re-point existing constants onto new ramps; don't rename call sites. Same
  approach the grey ramp (§4) and lighten/darken dedup already used.
- **Verify headless, sign off live.** Every visual phase is checked by the §23 harness (PNG snapshot
  + tree-bounds assertions); Peter eyeballs taste once at the end of the phase, not each iteration.
- **Shape + colour, never colour alone** (§11). Armed/active states change fill/icon too — colour-blind
  + dark-stage-wash safe.
- **Dark palette.** Distinct value *steps*, not a *bright* UI. A live tool glows in a dark room.
- **One atomic cutover per surface.** No per-call fallback paths; migrate a surface fully, snapshot,
  move on.

---

## 2. Phase 0 — Verification backbone (do first)

**Why:** §23's spike is proven but renders one card. Generalising it removes ~80% of the
"Peter must look" gating that otherwise blocks every visual phase below.

| | |
|---|---|
| **Current** | [`tests/headless_ui_spike.rs`](../crates/manifold-renderer/tests/headless_ui_spike.rs) renders one `ParamCardPanel` headless, injects a click, re-renders. Hard unknowns (windowless render, input injection, build→click→re-render) all answered ✅. |
| **Changes** | Generalise to arbitrary panels (`InspectorCompositePanel`, timeline `heads`+`lanes`, transport, footer). Add tree-assertion helpers: find-node-by-key, rect, no-overlap, column-x-match. Add golden-snapshot save + diff. |
| **Files** | New `manifold-renderer/tests/ui_harness/` (helpers); reuse `GpuDevice::new`, `readback`, `image` crate, `assert_bytewise_equal`. **Reuse the harness bones, not the dead `run_legacy`/`EffectChain` path** (§23.7 caveat). |
| **Verify** | The harness verifies itself: render a known panel, assert known node layout, golden-diff the PNG. |
| **Done** | I can render inspector + timeline + chrome headless, assert layout, and golden-diff. |
| **Risk** | Low — spike answered the unknowns. ~40% new, 60% reuse. |

---

## 3. Phase 1 — Semantic colour ramp (§15)

**Why:** the single change that makes the UI read as *designed*. Highest leverage, lowest risk.

| | |
|---|---|
| **Current** | ~25 hand-picked state colours in [color.rs](../crates/manifold-ui/src/color.rs): `PLAYHEAD_RED` 217,64,56 · `STOP_RED` 128,51,51 · `RECORD_RED` 107,38,38 · `RECORD_ACTIVE` 209,46,46 · `EXPORT_ACTIVE` 184,56,56 · `BPM_CLEAR_ACTIVE` 133,51,51 · `MUTE_BTN_ACTIVE` 199,102,56 · `SOLO_BTN_ACTIVE` 217,191,64 · `PLAY_GREEN` 56,115,66 · `PLAY_ACTIVE` 64,184,82 · … `COLOR_BASELINE = 145`. |
| **Changes** | Define **7 hues × 3 steps** (idle/base/active) per the §15.2 table: RED, GREEN, AMBER, ORANGE, BLUE, CYAN, PURPLE. Re-point the ~25 constants as thin aliases onto the ramp. Tune the warm trio (red/amber/orange) so mute/solo stay distinct when adjacent. |
| **Files** | `color.rs` only (ramp + aliases). No call-site churn. |
| **Verify** | §16 ratchet — lower `COLOR_BASELINE` as the raw count drops (it *must* fall here). Harness snapshot of transport/footer/inspector for warm-trio adjacency. Peter eyeballs the warm trio live. |
| **Done** | One definition per hue; same red everywhere; `COLOR_BASELINE` lowered; ratchet green. |
| **Risk** | Low (aliases). Only judgment is value-tuning on the running app. |

---

## 4. Phase 2 — Elevation & separation (§17)

**Why:** depth instead of flatness — the "lift" in the mockups.

| | |
|---|---|
| **Current** | 5 near-identical neutral borders: `RACK_BORDER` 56 · `CARD_BORDER` 46 · `CARD_BORDER_C32` 55 · `DROPDOWN_BORDER` 58 · `GEN_CARD_BORDER_C32` 58 (purple-tinted). **No shadow primitive** in [ui_renderer.rs](../crates/manifold-renderer/src/ui_renderer.rs). |
| **Changes** | (a) Collapse the 5 → **one `BORDER` hairline token** (gen-card purple tint folds into §15's PURPLE). (b) **Value-step depth** via the existing `BG_0..BG_3` ramp — card header a step brighter than body, header column lifts over lanes (no new primitive). (c) Add **one soft drop-shadow** to the GPU rect pipeline (`RectCommand` + fragment SDF outer-term), for **floating elements only** (dropdown, browser_popup, mod drawer). One step, not a Material ramp. |
| **Files** | `color.rs` (BORDER token); `ui_renderer.rs` (shadow param + fragment); floating call sites opt in. |
| **Verify** | Harness PNG of a dropdown/drawer (shadow present, subtle); tree unaffected. |
| **Done** | One border token; floating elements lift with one subtle shadow; in-panel grouping stays fill-level. |
| **Risk** | Low–medium — first shader change. Keep it subtle (dark-room rule). |

---

## 5. Phase 3 — Apply the component kit everywhere (§18)

**Why:** one grammar. A half-standardised UI is *worse* than none (§10 — forced relearning).

**Status (2026-06-26): chrome bars + popups DONE; only the param-card `*_btn_style` one-offs remain.** Added [`state_button`](../crates/manifold-ui/src/chrome/components.rs) — the standalone latching/momentary button (on = filled semantic hue + lighten(30)/darken(20); off = neutral `BUTTON_DIM` chip), the generalisation of `toggle` (accent special-case). The button mechanic was copy-pasted six times across the chrome; now centralised. Migrated: **footer** (button_secondary/segment), **transport** (`button_style` → kit; neutral buttons unified to the `BUTTON_DIM` chip), **layer-card mixer** (`mute/solo/led/analysis` → one `state_btn` shim on the carve-out hues, zero visual change), **header** (zoom + Audio/Perform/Monitor → one neutral chip, fixed a within-bar 59-vs-71 split). **Popups:** one shared [`panels::popup_shell`](../crates/manifold-ui/src/panels/popup_shell.rs) (scrim + a single rounded 1px-bordered container) now backs `dropdown`/`browser_popup`/`ableton_picker` — pickers' fake outer+inner border → a real border, three scrim dims → `PopupStyle::DROPDOWN`/`MODAL`, picker literals hoisted to `MODAL_*` tokens (ratchet 145 → 139). **Plumbing, not a look change** — visually near-identical, so the later visual upgrade lands in one place. Each verified by headless renders in [`ui_color_swatches.rs`](../crates/manifold-renderer/tests/ui_color_swatches.rs).

| | |
|---|---|
| **Current** | The kit ([chrome/components.rs](../crates/manifold-ui/src/chrome/components.rs): toggle/**state_button**/button/icon_button/segment/dropdown_trigger/reset/mod_badge) now owns every **chrome-bar** button. Popups (`dropdown`/`browser_popup`/`ableton_picker`) still hand-roll their shells. |
| **Changes** | ✅ transport / header / footer / layer_header buttons+toggles onto the kit; bespoke `*_style` fns deleted or collapsed to thin font/radius shims. ⬜ Popups onto one shared `popup_shell` (§22.2), colours from §15, depth from §17. |
| **Files** | ✅ `transport.rs`, `header.rs`, `footer.rs`, `layer_header.rs`. ⬜ the three popups. |
| **Verify** | Harness snapshot per panel (panel-by-panel, one atomic cutover each); ratchet catches stray literals; tree assertions for layout. |
| **Done** | No bespoke button/toggle/dropdown styling; the kit owns the look; local helpers gone. **Chrome buttons there; popups pending.** |
| **Risk** | Medium (broad). Mitigate by going panel-by-panel with a snapshot gate each. |

---

## 6. Phase 4 — Hierarchy + micro-motion (§19)

**Why:** SOTA inspectors emphasise the object you're editing and recede the rest; restrained motion
confirms actions.

| | |
|---|---|
| **Current** | Every card equal visual weight; no feedback on press / arm / collapse / commit. |
| **Changes** | Focused section lifts (fill +1, subtle accent edge), rest recede — pairs with collapse-by-default. Restrained micro-motion: fast press flash, arm-state pulse, collapse ease (60fps, cheap). Define empty / error / loading states once. Timeline echo: focused track gets the same emphasis. |
| **Files** | Inspector card build + a small tween helper; timeline focused-track path. |
| **Verify** | Harness snapshot focused vs unfocused; motion is Peter's eyeball. |
| **Done** | The edited card/track is clearly emphasised; motion confirms actions, never idles. |
| **Risk** | Medium — motion taste. Keep restrained; no decorative idle animation (distracting on stage). |

---

## 7. Phase 5 — Timeline visual upgrade (§24) — the structural spine

**Why:** the biggest perceived lift (readable clips) and the only real rendering-architecture move.

> **Dead gate (removed):** the old "perform-mode timeline treatment" gate was invalid — perform mode
> does not display clips. Do not raise it again.

**5a — Gradient primitive. ✅ DONE (2026-06-26).** `UIRenderer::draw_gradient_rect` + a linear-gradient
body in the shared rect shader (`ui_renderer.rs`: `UIVertex` grew `color2` + `grad`; fragment mixes
`color`→`color2` along `grad.xy`, every existing draw stays gradient-off). Plumbing only — nothing
calls it yet, so zero visual change. Verified headless (`gradient_demo`). Benefits chrome *and* clips.

**5b — Clips → GPU SDF quads. ✅ DONE (2026-06-26).** Clips are no longer baked into the per-layer
bitmap; they render as GPU SDF rounded rects through the shared rect pipeline.
- **Renderer:** [`clip_draw.rs`](../crates/manifold-renderer/src/clip_draw.rs) — `emit_clips` (lift
  shadow on select → rounded gradient body → border, two-phase so a selected clip's shadow sits under
  every neighbour) and `emit_clip_names` (overlay text, luminance-picked contrast, scissor-clipped;
  ellipsis is a Phase-6 polish — currently a hard cut). Styling lives in design tokens (`CLIP_RADIUS`,
  `CLIP_GRADIENT_LIGHTEN`, `CLIP_SHADOW*`, `CLIP_BORDER_*`, `CLIP_LABEL_*`) so the look is one-line
  tunable in the Phase-6 eye pass.
- **Pass model (`app_render.rs`):** the per-layer bitmap split into two buffers
  ([`bitmap_renderer.rs`](../crates/manifold-ui/src/bitmap_renderer.rs)) — **bg** (grid + top separator)
  drawn before the clip pass, **front** (waveform + region + cursor + markers) after — with the GPU clip
  cycle (own `UIRenderer` prepare/render) between them, and names in the Pass-5 overlay. Two
  `LayerBitmapGpu` instances (bg / front). `draw_clip` + its clip-fill consts/tests retired.
- **Geometry:** [`viewport::visible_clip_rects`](../crates/manifold-ui/src/panels/viewport.rs) rebuilds
  on-screen clip rects each frame from the same `beat_to_pixel`/`track_y`/`CLIP_VERTICAL_PAD` the
  hit-tester uses, so the drawn body and the clickable region can't drift.
- **Model:** `TimelineClip.color_override: Option<Color>` (skip-serialize when `None`; old projects
  round-trip — unit-tested) resolved into `ViewportClip.color` at the core↔UI boundary; `get_clip_color`
  state logic unchanged.
- **Deviations from the original sketch (both deliberate, for the live show):** the bitmap was *split*,
  not replaced (waveform stays a bitmap — rich per-pixel data — so audio clips never regressed); and the
  **pixel-shift scroll optimisation was KEPT, not retired** — at 4K×~53 layers the bg-grid upload
  bandwidth on auto-scroll still pays for it, and auto-scroll is the live-performance case.
- **Verify:** headless `clip_body_sheet` (bodies + names + states), serde round-trip unit tests, the
  29 bitmap dirty-check tests, workspace sweep + clippy. The *look* itself is a Phase-6 eye pass on the
  running app — not claimed done here.

**5c — Thumbnail pipeline.** Generator previews first (reuse the authoring-time
[`preview_request`](../crates/manifold-renderer/src/layer_compositor.rs#L471) scaffolding → cache a
small per-clip texture); then **video poster frames** (new: extract a representative decoded frame →
downscale → cache per clip → upload → sample; invalidate on trim/source change). Audio waveform
already exists ([waveform_renderer.rs](../crates/manifold-ui/src/waveform_renderer.rs)).

**5d — One header grammar + type badges.** Collapse the four `coordinate_mapper::layer_height`
grammars (140/48/62/70) into one with height presets (collapsed/normal/tall) applied the same way to
every type; push type into an **icon-glyph badge** (video/text/generator/group/audio — the PUA glyph
system the LFO arm button uses).

**5e — One clear "now" + nav.** Resolve playhead vs insert-cursor so playback position is unmissable;
add scroll-to-zoom and a draggable scrollbar thumb.

| | |
|---|---|
| **Files** | `ui_renderer.rs`, `bitmap_painter.rs`/`bitmap_renderer.rs` (retire/replace), `layer_bitmap_gpu.rs`, `coordinate_mapper.rs`, model + `manifold-io` (clip colour), `manifold-media` (poster extraction), `layer_compositor.rs` (gen-preview wiring), `viewport/*` (cursor + nav). |
| **Verify** | Harness snapshots + GPU readback (parity-style); perf check at 2928-clip scale. |
| **Done** | Clips readable (name+preview+boundary), rounded, lift, colour-independent; one header grammar + badges; one clear playhead; faster at scale. |
| **Risk** | **High** — the big structural change. clips→GPU is the gate; the serialization change needs migration care (skip-if-default; round-trip test on the canonical fixture). |

---

## 8. Phase 6 — Taste & tuning (Peter's eye)

The system can be perfect and still look ordinary. Tune on the running app: ramp values, hierarchy
emphasis weight, shadow weight, spacing rhythm, badge glyphs. **Done = Peter signs off it looks SOTA.**

---

## 9. Sequence & rationale

```
0 Harness ─┬─ underwrites verification for all visual phases
           │
1 Colour ──┤  cheap, high-leverage, harness-verifiable, on existing infra
2 Depth ───┤  (tokens + one shadow primitive)
3 Kit ─────┤  coverage — one grammar everywhere
4 Hierarchy┘  polish

5 Timeline   structural spine: 5a gradient ✅ → 5b clips→GPU ✅ → 5c thumbnails → 5d headers → 5e nav
6 Taste      final tuning pass on the running app
```

- **Tokens/coverage (1–4) before structural (5):** they lift the *whole* UI on infra you already
  shipped, are all harness-verifiable, and are low-to-medium risk. Do them first for fast, safe wins.
- **Timeline (5) last among the build phases:** it's the heavy structural lift. 5a/5b are done; 5c–5e
  remain.
- **§16 guard runs throughout** so nothing re-drifts as we go.

## 10. Decision gates (need Peter)

1. **Clip colour-override** — ✅ shipped in 5b (`TimelineClip.color_override: Option<Color>`,
   skip-serialize when `None`, round-trip unit-tested). No open gate.

## 11. Acceptance — "SOTA looks like"

- Neutral chrome; colour means identity + state, one definition per hue.
- Consistent value-based depth; floating elements lift with one subtle shadow.
- Clips are readable: name + preview + boundary, rounded, colour-independent.
- One control grammar everywhere (kit owns the look); one header grammar + type badges.
- One unmissable playhead.
- **Enforced** (ratchet, can't re-drift) · **verified** (harness, didn't regress) · **Peter's eye says pro**.
