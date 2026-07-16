# glTF Animation — imported clips as performable motion (node TRS, skinning, morphs)

**Status:** APPROVED · 2026-07-16 · Fable 5 (Peter approved) · A1 IN PROGRESS
**Prerequisites:** GLB_XFAIL_BURNDOWN_DESIGN.md P2 (owns the BUG-170 crate-bump verdict; its D8 may hand this doc three pointer-animation assets). No dependency on GLTF_MATERIAL_EXTENSIONS_DESIGN.md — the two can execute in either order.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Peter's ask (2026-07-16): *"the ability to import animated glb files and scenes too."* Today animated glbs import as frozen statues: the importer reads no `animations`, `skins`, or morph-target data (verified 2026-07-16 — zero hits for JOINTS/WEIGHTS/animation parsing in `gltf_load.rs`/`gltf_import.rs`; `GLB_CONFORMANCE_DESIGN.md` §7 deferred item 7 scoped it out).

Honesty about certification: this doc barely moves the conformance number — most animated Khronos assets (`BoxAnimated`, `CesiumMan`, `Fox`, `BrainStem`, `RiggedFigure`, `MorphPrimitivesTest`…) already **pass statically**, because the suite's checks render one converged frame. The value is the instrument, not the scoreboard.

**The instrument frame is the design's spine:** an imported animation must land as a *performable* thing, not a video-player timeline. The clip's progress is a param — scrubbable, LFO-able, audio-modulatable, MIDI-triggerable, beat-quantized. A walking character retriggered on every kick, a blooming flower scrubbed by a fader, a loop time-locked to bars. glTF stores seconds; MANIFOLD's primary time model is beats — the seconds→beats mapping is a decision (D3), not an accident.

## 1. Audit — what exists (verified 2026-07-16; RE-DERIVE at execution)

```
rg -n 'JOINTS_0|WEIGHTS_0|read_morph|animations\(\)|skins\(\)' crates/manifold-renderer/src/node_graph/gltf_load.rs   # expect: still zero hits
rg -n 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/{morph_mesh,bend_mesh,displace_mesh,gltf_mesh_source}.rs
rg -n 'anim_progress|trigger_count' crates/manifold-renderer/src/node_graph/effect_runtime.rs | head
```

Snapshot: meshes flow through the graph as first-class data — `gltf_mesh_source` → deform atoms (`morph_mesh`, `bend_mesh`, `displace_mesh` — MESH_DEFORM_AND_CURVE_GEOMETRY, SHIPPED 2026-07-11) → `render_3d_mesh` / `render_scene`. `PresetContext` already carries `time`, `beat`, `anim_progress`, `trigger_count` per frame. Per-object transforms reach `render_scene` as graph params (the import graph wires them), so animating a rigid node = animating params — machinery that exists. Genuinely new: keyframe storage + sampling, skinning (JOINTS_0/WEIGHTS_0 → joint palette → per-vertex blend), and morph-target weight animation.

## 2. Decisions (direction-level; each gets full treatment in the executing phase's re-derivation)

- **D1 — Animation is graph-native: a clip source node + existing param wires.** Import produces `node.gltf_animation_source`: inputs a progress scalar (0..1), outputs the sampled values — per-node TRS as transform params, morph weights as scalars — wired to the same object params the static importer already sets. Sampling is CPU-side, content-thread, per-frame, allocation-free after load (pre-sorted keyframe tracks, binary-search + lerp/slerp/cubic per spec). Rejected: a parallel "animation player" subsystem outside the graph, because the graph IS the modulation system — anything else would re-invent param binding and lose the performance surface for free.
- **D2 — Skinning is a fusable GPU mesh atom: `node.skin_mesh`.** Per-vertex joint blend (4 joints/weights per spec) is a barrier-free pure per-element kernel — it MUST ship on the freeze codegen path per the standing all-nodes-fusable rule (CLAUDE.md; `docs/ADDING_PRIMITIVES.md` §"The codegen path is mandatory"). Joint matrices arrive as a buffer input computed CPU-side from the sampled skeleton pose (matrix palette is small — spec-typical ≤ 256 joints; `BrainStem` is the stress case, re-derive its joint count). `MeshData` grows optional JOINTS_0/WEIGHTS_0 attributes (⚠ VERIFY-AT-IMPL: MeshData's attribute model — extend the way UVs/normals ride today, one owner, no parallel skinned-mesh type). Morph targets reuse the `morph_mesh` shape (positions-delta blend) with imported targets. Rejected: CPU skinning, because a 50k-vertex character per frame on the content thread is hot-path allocation/latency the two-thread model exists to avoid.
- **D3 — Seconds→beats at import, performer-facing progress at runtime.** The clip's duration in seconds is metadata; the runtime surface is normalized progress (0..1) plus a default drive: `progress = wrap(beat × rate / clip_beats)` where `clip_beats = duration_s × bpm / 60` computed live from the transport (beats primary; Seconds only at the glTF edge — the house time invariant). The performer sees: progress (scrub/modulate), rate, loop mode (loop / once / ping-pong), and a retrigger input (trigger resets phase — the phantom-clip/trigger precedent). Rejected: storing a beats conversion at import time, because it bakes a BPM into the asset and goes wrong the moment the tempo changes live — the exact class of bug the beats-primary rule exists to kill.
- **D4 — Multi-clip glbs expose a clip selector param** (glTF `animations[]` is a list; `Fox` ships three). Inline-mux option-table pattern (`feedback_inline_mux_option_table_params`). Blending between clips is Deferred — trigger: a real performance need Peter names, not spec completeness.
- **D5 — BUG-170's pointer-extension assets** (`KHR_animation_pointer`, `KHR_node_visibility`) join this doc's scope only if the burn-down's crate bump fails; animation *pointer* targets (animating arbitrary properties, not just TRS/weights) are Deferred regardless — trigger: an asset Peter actually wants that uses them.

## 3. Phasing (conformance level — one session each; full briefs written by the executing orchestrator against re-derived inventory, per STANDARD §9)

- **A1 — Rigid animation vertical slice.** Parse `animations[]` (TRS channels only), `node.gltf_animation_source`, default beat-drive, progress param. Gate: `BoxAnimated.glb` visibly animates in a headless multi-frame render (PNG sequence at progress 0/0.25/0.5/0.75 — four distinct goldens); save→reload→animate round-trip (STANDARD §5 round-trip gate); performer-gesture line: progress driven by a saw LFO loops the clip cleanly at the wrap point. The vertical slice runs model → graph → pixels before anything else lands — DESIGN_AUTHORING §7.
- **A2 — Skinning.** JOINTS/WEIGHTS through `MeshData`, `node.skin_mesh` (codegen path + generated-vs-hand parity test, mandatory), skeleton pose sampling. Gate: `CesiumMan`/`Fox` animate; parity test green; hot-path check (`MANIFOLD_RENDER_TRACE=1`, no frame >20ms — STANDARD §5 content-thread gate).
- **A3 — Morph-target animation.** Imported targets + weight channels through the `morph_mesh` shape. Gate: `AnimatedMorphCube` + `MorphStressTest` animate; four-phase goldens.
- **A4 — Performance surface.** Clip selector (D4), loop modes, retrigger wiring, perform-UI exposure per `feedback_param_values_is_performance_surface`. Gate: L3 ui-flow driving retrigger + scrub; performer gesture: retrigger on a MIDI note fires the clip from zero within one frame.

## 4. Decided — do not reopen
1. Animation lives in the graph as a source node + param wires; no standalone player subsystem (D1).
2. Skinning is GPU, fusable, codegen-path; no CPU skinning, no plain-WGSL boundary kernel (D2).
3. Runtime time surface is beats/progress; no baked seconds→beats conversion (D3).

## 5. Deferred
- Clip blending/crossfade (D4) · animation-pointer property targets (D5) · IK/retargeting (never in scope for import) · timeline-clip integration (an imported animation as a timeline clip with in/out points — real feature, but it builds ON A1–A4's progress param; trigger: Peter's call after playing with A4).
