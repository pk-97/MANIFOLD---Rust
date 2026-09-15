# Scene Render Mode — wireframe / solid / points as a scene modifier card

**Status:** SHIPPED — P1–P3 (2026-09-16, k3 lead / k27 lanes). Owed: VD bead — modifier round-trip verified L1, L3 journey open. Card rows + gates are the cited contract.
**Prerequisites:** none — the file-authored scene-modifier regime this design
rides is on main (`assets/scene-modifier-presets/SceneFog.json` et al.).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.

Render modes — Blender's viewport shading modes (wireframe, solid, points) —
as a **scene modifier card**: applied like an effect, tuned from the inspector,
enabled/bypassed, MIDI-mapped, clip-triggered. On stage this makes the
geometry-vs-surface axis playable: drop a scene to wireframe on a pad, drive
line brightness off the kick, automate solid→rendered over eight bars.

Peter's directives, verbatim:

- "maybe they just become scene modifiers? … I want it as a scene modifier.
  Please. It is used like an effect" (2026-09-15) — the card UX is the
  product call; the mechanism conforms to it.
- "the triggering and param changes etc that can sync with music and stuff
  would be super cool with these render modes" (2026-09-15) — performability
  is the point, not editor convenience.

Companions: `SCENE_MODIFIER_FRAMEWORK_DESIGN.md` (the card/descriptor
contract this rides — D5 gate-family enable, D4 card home),
`SCENE_MODIFIER_PRESET_ARCHITECTURE.md` (file-authored recipes; **no new Rust
registrations**), `REALTIME_3D_DESIGN.md` D5 (the atmosphere port precedent),
`ADDING_PRIMITIVES.md` (the one new atom).

---

## 1. Audit — what exists (verified 2026-09-15)

| Piece | Where | State |
|---|---|---|
| File-authored modifier recipes — JSON, `presetMetadata.sceneModifier`, catalog-loaded, no Rust registration | `crates/manifold-renderer/assets/scene-modifier-presets/SceneFog.json`; loader `node_graph/bundled_presets.rs:106` | **Shipped. The authoring path this design uses.** |
| Gate-family enable — `enabledParam` + `node.value` × `node.math` Mul into a port-shadowed scalar | SceneFog.json nodes `fog_enabled`/`fog_amount`/`fog_mul` | Shipped; copied wire-for-wire here |
| Stage→endpoint wiring — recipe stage declares `outputs: [{port, endpoint}]`; endpoint resolves to a `render_scene` input | SceneFog.json `stages[0]`; resolution in `node_graph/scene_vm.rs` | Shipped for `atmosphere`; must learn `render_mode` (⚠ VERIFY-AT-IMPL: how stage endpoints resolve — read `scene_vm.rs` stage wiring before P1) |
| CPU wire-value port precedent — `Atmosphere` struct, `PortType::Atmosphere`, unwired = default = byte-identical to no port | `node_graph/atmosphere.rs:23`, `node_graph/ports.rs:68` | Shipped; `RenderMode` mirrors it exactly |
| Producer atom precedent — `node.atmosphere`: `primitive!`, port-shadowed scalars, one struct output, CPU-only, `boundary_reason: NonGpu` | `node_graph/primitives/atmosphere.rs:28` | Shipped; `node.render_mode` mirrors it exactly |
| Triangle fill mode — `GpuTriangleFillMode::{Fill, Lines}`, threaded to `MTLTriangleFillMode`, exposed on `draw_instanced_depth_ex` | `manifold-gpu/src/types.rs:245`, `metal/encoder.rs:1385`, `metal/format.rs:95` | **Exists, used only as `Fill`. Wireframe = one flag per color-pass draw.** |
| Material dispatch — per-object `MaterialKind` selects pipeline + fragment shader (`fs_unlit/phong/pbr/cel`), pipelines keyed on kind | `render_scene.rs:7727` (`pipeline_for`), cache at `render_scene.rs:702` | Shipped; Solid mode substitutes a synthesized clay `Material` where per-object materials are gathered (⚠ VERIFY-AT-IMPL: exact gather site inside the 8702-line `render_scene.rs`) |
| Point primitives — **none.** Every draw is `MTLPrimitiveType::Triangle` | `metal/encoder.rs` (all `drawPrimitives` sites) | Genuinely new for Points mode: topology param + vertex-shader `[[point_size]]` |
| RT path — `render_scene` can rasterize via the RT tracer | `render_scene.rs` `rt_enabled` | Render modes do not apply to RT (D4) |

Classification: wire type, atom, recipe, card, enable — **exist one template
away** (Fog). Wireframe mechanism — **one wire away** (fill-mode threading).
Solid — **one substitution away**. Points topology — **genuinely new**, small.

## 2. Decisions

- **D1 — Render modes are a file-authored scene modifier, kind `RenderMode`,
  recipe `assets/scene-modifier-presets/RenderMode.json`.** Peter's call,
  quoted above. The recipe mirrors SceneFog.json: singleton, `enabledParam`,
  one group stage whose output wires to a new `render_mode` endpoint on
  `render_scene`. No Rust descriptor registration — the framework doc's
  standing rule ("do not add new per-look registrations as the long-term
  authoring mechanism") is satisfied by construction.
  Rejected: mode params directly on `render_scene` (the param-only shape
  discussed 2026-09-15) — no card, no apply/remove, no enable toggle, and
  params clutter every scene whether used or not. Peter chose the card.
  Rejected: viewport-only preview state (Blender's shape) — invisible to
  MIDI/clips/serialization; worthless on stage, which is the point.

- **D2 — One new CPU wire value `RenderMode`, one new `PortType::RenderMode`,
  one new optional `render_scene` input.** Shaped exactly like `Atmosphere`
  (`atmosphere.rs:23`): a `Copy` struct of plain scalars, no GPU resource on
  the wire. Unwired = `RenderMode::default()` = `mode: Rendered` =
  **byte-identical to no input** — the zero-cost contract, same as fog.

  ```rust
  // crates/manifold-renderer/src/node_graph/render_mode.rs
  pub struct RenderMode {
      /// 0 = Rendered, 1 = Solid, 2 = Wireframe, 3 = Points.
      /// RENDERED MUST STAY 0 — the enable gate multiplies (INV-R2).
      pub mode: u32,
      /// Solid: flat clay color (rgb; a reserved). Inert in other modes.
      pub clay_color: [f32; 4],
      /// Wireframe/Points: line/point color (rgb; a reserved). Inert in Rendered/Solid.
      pub line_color: [f32; 4],
      /// Wireframe/Points: brightness gain, `[0, 4]`, default 1.
      pub line_brightness: f32,
      /// Points: point size in px, `[1, 16]`, default 2. Inert elsewhere.
      pub point_size: f32,
  }
  ```

  Inert-member precedent: `Material` (`material.rs:20` — "metallic is unread
  when kind = Phong"). One struct serves all modes; per-mode params that
  don't apply are simply unread.

- **D3 — One new atom `node.render_mode`, mirroring `node.atmosphere`.**
  `primitive!`, `boundary_reason: NonGpu` (CPU-only — the freeze-codegen
  fusable rule governs per-element GPU atoms and does not apply, same as
  `node.atmosphere`). Params: `mode` (Enum, labels Rendered/Solid/Wireframe/
  Points) plus the D2 floats, each port-shadowed by a same-named optional
  scalar input so every row is drivable. The mode scalar port is what the
  enable gate multiplies (D5).

- **D4 — Modes act on the raster path only; the RT path ignores the wire.**
  `rt_enabled` + mode ≠ Rendered → rendered normally, no wireframe RT.
  Rejected: RT wireframe preview — real cost (RT has no fill mode; it would
  be a separate debug integrator), zero stage demand named. Deferred with
  trigger (section 7).

- **D5 — Enable gates `mode` only: `enabled × mode` via `node.math` Mul into
  the atom's port-shadowed `mode` scalar.** Framework D5's gate family,
  SceneFog's exact wiring. `enabled = 0` → mode 0 → Rendered. Per-mode
  floats are NOT gated — they're inert when their mode isn't active (D2), so
  gating them buys nothing. **Consequences, stated honestly:** mode is a
  continuous float on the wire inside the gate, rounded/clamped by the atom;
  a modulated `enabled` at 0.5 floors the mode index — acceptable, same
  behavior as fog's gate at partial enable.

- **D6 — Wireframe = `GpuTriangleFillMode::Lines` on the color pass, drawn
  with the scene's normal lighting OFF (unlit line color × brightness).**
  Fill mode is encoder state, not a pipeline variant — no new pipeline, no
  prewarm cost. The color-pass draw entry needs a `fill_mode` parameter
  threaded like `draw_instanced_depth_ex` already has
  (`metal/encoder.rs:1385`). **The depth prepass and shadow passes always
  fill** — lines-only depth would break occlusion, shadows, and any
  depth-reading effect downstream.

- **D7 — Solid = synthesized clay `Material` substituted at material-gather
  time.** Every object renders with `MaterialKind::Phong`, flat `clay_color`,
  neutral roughness, scene lights still lighting it. No shader change — the
  Phong pipeline exists. The substitution happens where render_scene resolves
  each object's material, upstream of `pipeline_for`, so pipeline caching is
  untouched.

- **D8 — Points = the same vertex buffers drawn as `MTLPrimitiveType::Point`.**
  `MeshVertex` position sits at offset 0, so the existing buffers draw
  unmodified; one new draw entry (or a topology param on the existing color
  entry), and the vertex shader writes `[[point_size]]` from a uniform. This
  is the only mode touching shaders — its phase carries the gpu-proofs gate.

## 3. The recipe (card shape)

`RenderMode.json`, schema v3, shaped on SceneFog.json:

- `presetMetadata`: id `RenderMode`, displayName "Render Mode", category
  `Atmosphere` (⚠ VERIFY-AT-IMPL: category string drives picker grouping —
  check whether a new category is warranted or `Atmosphere` suffices).
- Params (card rows): `enabled` (toggle, drives `rm_enabled`),
  `mode` (enum 0–3 — ⚠ VERIFY-AT-IMPL: card rows are manifest-backed;
  confirm the modifier card host renders enum params as dropdowns, else mode
  rides as a slider row), `clay_color_r/g/b`, `line_color_r/g/b`,
  `line_brightness`, `point_size`.
- Bindings: each param → its `node.value` atom (gate wiring for `mode`:
  `rm_enabled` × `rm_mode` → Mul → `node.render_mode.mode` scalar input; all
  other params bind straight to the atom's port-shadow inputs — no gate).
- `sceneModifier`: `singleton: true`, `enabledParam: "enabled"`, one stage
  whose output `{port: render_mode, endpoint: render_mode}` wires to
  `render_scene`.

Stage behavior: apply → card appears with rows; rows are ordinary manifest
rows, so MIDI/OSC/envelopes/audio-mods address them while applied (framework
D7); bypass → Rendered, byte-identical; remove → graph restored, zero
residue. Serialization is the existing modifier-instance path — old projects
without the port load byte-identical (unwired default).

## 4. Invariants & enforcement

- **INV-R1 — Unwired `render_mode` input, or mode = Rendered, is pixel-identical
  to today.** Enforcement: `render_mode.rs` test `default_is_rendered` (struct
  default) + a headless-render parity test in `render_scene/tests.rs`
  (same scene, port unwired vs wired-with-Rendered, buffers compared).
- **INV-R2 — `Rendered` is mode index 0 forever** (the enable gate's multiply
  depends on it). Enforcement: `const _: ()` assertion on the enum-label
  table + test `rendered_is_index_zero` in the atom's tests.
- **INV-R3 — Depth prepass and shadow passes always fill, regardless of
  mode.** Enforcement: test in `render_scene/tests.rs` asserting the depth
  entry points receive `Fill` under mode = Wireframe (encoder mock records
  the flag).
- **INV-R4 — RT path ignores the wire.** Enforcement: test — `rt_enabled`
  with mode = Wireframe produces the Rendered uniform set (no fill-mode
  flag reaches any RT draw).
- **INV-R5 — No Rust descriptor registration for this kind.** Enforcement:
  `rg -n 'RenderMode' crates/manifold-renderer/src/node_graph/scene_modifier*` —
  zero hits outside `scene_modifier_preset` schema code.

## 5. Phasing

One mechanism per phase — the modes genuinely differ (fill flag / material
substitution / topology), so phasing by mode IS phasing by layer here.

### P1 — Wireframe (the vertical slice)

The whole pipe once: wire type, port, atom, recipe, card, enable gate,
fill-mode threading. Wireframe is the proving mode because it's the cheapest
mechanism (no shader, no pipeline).

- **Entry state:** `RenderMode.json` does not exist; `rg -n 'render_mode'
  crates/manifold-renderer/src/node_graph/ports.rs` — zero hits. Re-verify
  the section-1 anchors for `ports.rs:68`, `atmosphere.rs`, SceneFog.json,
  `encoder.rs:1385`.
- **Read-back:** this doc's D1–D6; SceneFog.json whole;
  `primitives/atmosphere.rs` whole; the stage-endpoint resolution in
  `scene_vm.rs` (naming the exact function in the phase notes).
- **Deliverables:** `node_graph/render_mode.rs`; `PortType::RenderMode`;
  `primitives/render_mode.rs` (`node.render_mode`); `render_scene` optional
  `render_mode` input + wireframe branch; `fill_mode` param threaded through
  the color-pass draw entry; `assets/scene-modifier-presets/RenderMode.json`
  (mode rows for all four modes present from P1 — the atoms don't care);
  atom tests (defaults, clamping, port-shadow precedence); INV-R1/R2/R3/R5
  checks.
- **Forbidden moves:** registering a Rust descriptor for the kind · gating
  the per-mode floats · letting the depth/shadow pass take the fill mode ·
  adding params to `render_scene` itself · a viewport-local override switch.
- **Gate:** `cargo nextest run -p manifold-renderer render_mode` green;
  `scripts/gpu_proofs_gate.py` green (render_scene touched); the INV-R5 `rg`
  zero-hit; `MANIFOLD_RENDER_TRACE=1` run — no frame >20ms attributable to
  the mode branch (content-thread gate).
- **Round-trip gate:** apply modifier → save → reload → card present, mode
  switch still works, a binding on `line_brightness` modulates **after**
  reload.
- **Acceptance demo (L3):** `scripts/ui-flows/` flow — apply Render Mode to a
  scene with a GLB object, set Wireframe via the card row, assert the
  `node.render_mode` instance exists and `render_scene`'s input resolves;
  headless PNG of wireframe + solid-rendered pair for Peter to look at (L2
  artifact, agent gate is the flow exit code).
- **Performer gesture:** mode dropdown on a MIDI pad, flipped mid-playback —
  the round-trip gate's binding exercise covers it.
- **Test scope:** `-p manifold-renderer` focused; `-p manifold-gpu` if the
  encoder signature changes.

### P2 — Solid (clay)

- **Entry state:** P1 landed; mode 1 rows already on the card (P1 recipe).
- **Deliverables:** clay `Material` synthesis + substitution at
  material-gather (D7); test asserting every object's effective kind is
  Phong under mode = Solid; INV-R1 parity extended to Solid-off.
- **Forbidden moves:** a new shader or pipeline (Phong exists) · per-object
  albedo tinting (Deferred, section 7).
- **Gate:** `cargo nextest run -p manifold-renderer render_mode` green;
  headless PNG of a multi-material scene in Solid — every object one clay
  color, lighting intact (Peter looks; agent gate is a region-mean probe:
  two different-albedo objects' region means within stated tolerance).
- **Acceptance demo:** same PNG pair. **Performer gesture:** automate
  Rendered→Solid over 8 bars on a clip envelope.
- **Test scope:** `-p manifold-renderer`.

### P3 — Points

- **Entry state:** P1 landed. Re-verify no point topology exists
  (`rg -n 'MTLPrimitiveType::Point' crates/manifold-gpu` — zero hits).
- **Deliverables:** topology param or point draw entry in `manifold-gpu`;
  `[[point_size]]` written from uniform in the scene vertex shader; points
  branch in `render_scene`; pipeline prewarm at load (BUG-037 (glp-first-render-stall) rule — first
  Points frame must not compile).
- **Forbidden moves:** a separate point vertex buffer or mesh copy (the
  existing buffers draw as-is) · point sprites/round points (Deferred).
- **Gate:** `cargo nextest run -p manifold-renderer render_mode` green;
  `scripts/gpu_proofs_gate.py` green (shader touched); headless PNG — GLB
  scan as a point cloud (Peter looks; agent gate: non-zero pixel count above
  background in a stated region).
- **Acceptance demo:** the point-cloud PNG. **Performer gesture:**
  `point_size` on an audio mod, kick-driven.
- **Test scope:** `-p manifold-renderer` + `-p manifold-gpu`.

## 6. Decided — do not reopen

1. Modifier card, file-authored recipe, no Rust registration (Peter, D1).
2. One `RenderMode` wire struct, one port, unwired = Rendered = byte-identical (D2).
3. Raster only; RT ignores the wire (D4).
4. Enable gates `mode` only, via the SceneFog gate wiring (D5).
5. Depth/shadow passes always fill (D6).
6. Solid reuses the Phong pipeline; no new shaders except Points' point_size (D7/D8).

## 7. Deferred

- **RT render-mode previews** — trigger: Peter asks to audition wireframe on
  an RT scene, or RT becomes the default raster path.
- **Per-object albedo tint in Solid** (Blender's "material color" solid
  option) — trigger: sculpting scenes where flat clay hides object identity.
- **Normals/depth/UV debug views** — diagnostic overlays, not performable
  modes; trigger: a 3D authoring-debugging push.
- **Round/point sprites, per-vertex point size** — trigger: point-cloud looks
  graduating from audition to a show piece.
- **Line width control** — Metal renders lines at 1px; wider lines need
  quad-expanded edges (a geometry mechanism, not a fill mode). Trigger: a
  show needs fat wireframe — priced then, not now.
