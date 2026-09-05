# Scene Mirror — whole-scene reflection as modifier kind #3

**Status:** SHIPPED — P0+P1+P2 all on wave 2026-09-05, landing with the scene-mirror wave · 2026-09-05 · k3 (lead), design session with Peter
**Prerequisites:** SCENE_MODIFIER_FRAMEWORK (P1 loop + fog kinds shipped), SCENE_FX (deformer family + "off is free" pattern shipped), SCENE_LOOP (P4 loop controls shipped).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.

Peter, 2026-09-05, the ask: 3D scene stylisation on top of Scene Loop — *"3D scene mirrors, twists, pulses, explosions, steps"* — with the infra ready so further scene modifier kinds are easy to add. Twist/explode/step live already (SCENE_FX per-object rack); the mirror is the first genuinely scene-wide item and the vehicle for the kind-#3 infra hardening. On stage: DamagedHelmet loops down a corridor; Scene Mirror drops a reflected helmet under the floor plane; Plane Offset rides the kick and the reflection answers.

The governing insight: **a mirror is an instance-array transform, not a render pass.** The scene already renders through `Array<InstanceTransform>` per object (scene_array for the loop, D11 whole-buffer draw); reflecting every instance across a plane is a barrier-free per-element kernel in exactly the same shape. No second render, no camera cloning, no oblique clipping — and it composes with Scene Loop for free because the loop's copies arrive as instances.

Companion docs: [SCENE_MODIFIER_FRAMEWORK_DESIGN.md](SCENE_MODIFIER_FRAMEWORK_DESIGN.md) (the descriptor/plan/generic-command machinery this plugs into), [SCENE_LOOP_DESIGN.md](SCENE_LOOP_DESIGN.md) (the plan-builder pattern + wrap purity), [ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md) (freeze codegen path scope test), [MANIFOLD_GPU_ARCHITECTURE.md](MANIFOLD_GPU_ARCHITECTURE.md) (uniform layout, mesh draw path).

## 1. Audit — what exists (verified 2026-09-05)

Instruction to every phase: **extend, don't redesign.**

| Piece | Where | State |
|---|---|---|
| Modifier descriptor + registry (inventory, zero central edit per kind) | `crates/manifold-renderer/src/node_graph/scene_modifier.rs:32` `SceneModifierDescriptor`, `:162` registry, `:169-174` submits | SHIPPED (2 kinds registered). Picker + cards consume `descriptors()` — `crates/manifold-app/src/ui_bridge/projection/cards.rs:476` |
| Generic apply/remove commands (plan-driven, per-kind-free) | `crates/manifold-editing/src/commands/graph/scene_modifier.rs:200` (repoints), `:224` (splices) | SHIPPED. Fog proved a kind ships with zero framework change |
| Kind #2 template (gate bypass, whitelist rows, trace) | `crates/manifold-renderer/src/node_graph/scene_modifier.rs:809` `scene_modifier_fog` | SHIPPED — the mirror kind copies this shape |
| Instance transform type (pos+uniform scale, XYZ Euler, 32 B) | `crates/manifold-renderer/src/generators/mesh_common.rs:96` `InstanceTransform` | SHIPPED. `rot_pad.w` is documented padding — always 0 today |
| Loop instance atom (capacity≠count lesson, zero-mask surplus) | `crates/manifold-renderer/src/node_graph/primitives/scene_array.rs:1` | SHIPPED on freeze codegen path; BUG-757c (scene-loop-copies-param-inert) fixed the size-by-capacity rule the mirror copies |
| Group splice (interface input + top-level wire per object group) | `crates/manifold-core/src/scene_modifier.rs:31` `GroupSplice`; apply at editing `:224` | SHIPPED — **with the gap P0 closes: wire-add is skipped when the interface input already exists** (editing `:233-235`), so a second kind cannot re-wire an already-spliced port |
| Whole-buffer instance draw | `crates/manifold-renderer/src/node_graph/primitives/render_scene.rs:4909` (D11: `instance_count = buffer_size / 32`) | SHIPPED — surplus slots must self-mask (zero scale), the atom's job |
| Raster cull mode | zero `cull`/`frontFace` hits in `crates/manifold-gpu` (verified `rg -n "cull|front_face|FrontFace"`) | **No culling anywhere** — mirrored (winding-flipped) triangles draw without pipeline change |
| RT path instancing | `render_scene.rs:5483-5491` — accel structure uses each object's single `model` transform; instanced objects get ONE ray-traced copy (documented limitation) | SHIPPED limitation the mirror inherits (D7) |
| Mesh vertex shader Euler application | ⚠ VERIFY-AT-IMPL: read the scene mesh vertex WGSL for the exact `rot_pad` application order before deriving the conjugated-angle formulas — `mesh_common.rs:92` documents "XYZ order" but the shader's multiplication order is the authority |
| Planar-reflection render pass, per-object fold splice | — | Do not exist. Rejected in D1/D2 |

Section 2.5 audit statement (DECOMPOSING_GENERATORS.md): the reflection family was surveyed — `node.fold_mesh` mirrors ONE mesh across a plane through the origin along one axis (exists, rejected D2); no atom transforms an `Array<InstanceTransform>` by reflection (camera-family and array-family primitives surveyed via `rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/`). Verdict: **genuinely new** — one new atom, one new kind.

## 2. Decisions

- **D1 — Mirror mechanism = instance-array reflection (new atom `node.reflect_array`), not a reflection render pass.** Rejected: planar reflection pass (mirrored camera + oblique near-plane + second scene render — doubles the scene render cost, needs render_scene surgery, and the RT path already provides physical reflections for users who want correctness). The stylised mirror look Peter asked for is geometry, not correctness: reflected instances shaded by the same lights.
- **D2 — Not per-object `fold_mesh` splices.** Rejected: fold operates in mesh-local space across a plane through the origin; a world-space mirror plane maps into each object's local frame differently per object and is only axis-aligned for unrotated objects — fragile at apply time, silently wrong after the user rotates an object. Instance-level reflection is in world space by construction and immune to object edits.
- **D3 — Atom shape.** `node.reflect_array`: inputs `{ in: Array(InstanceTransform) optional }`, outputs `{ out: Array(InstanceTransform) }`; params `axis` (Enum, the 6 `AXIS_LABELS` from scene_array), `plane_offset` (Float, world distance along the axis from origin), `enabled` (Float 0/1, the toggle target). Unwired `in` = one identity instance at the origin (same convention as scene_array count semantics). Source atom on the freeze codegen path: `wgsl_body` + `fusion_kind`/`input_access` in the `primitive!`, pipeline from `standalone_for_spec::<Self>()`, value-level `gpu_tests` vs CPU-computed expected (ADDING_PRIMITIVES scope test — mandatory, not exemptible).
- **D4 — Capacity contract (BUG-757c (scene-loop-copies-param-inert) applied up front).** Output buffer is FIXED at 2× input capacity (loop scenes: 16 slots from the array's 8). Slots `[0, cap)`: originals, live where `< count`, zero-scale otherwise. Slots `[cap, 2cap)`: mirrored copies of slots `[0, cap)`, live where `< count`, zero-scale otherwise. `count`/`plane_offset`/`enabled` are live card writes — never trigger a rebuild; the draw reads the whole buffer and surplus slots draw nothing.
- **D5 — Reflection math + the shading fix.** Plane: unit axis `â` (from `axis`), point `d·â` (from `plane_offset`); `M = I − 2ââᵀ`. Per instance: `pos' = M(pos − dâ) + dâ`; uniform scale negates (`w' = −w`); rotation stored as the PROPER rotation `R' = M·R·M` (det +1, Euler-representable; for axis-aligned M the conjugation is a closed-form sign/permutation of the three angles — derive against the shader's actual Euler order, ⚠ anchor in section 1). `rot_pad.w` becomes an in-band marker: `0` = original slot; `k > 0` = mirrored across the plane perpendicular to component `k−1` — a THREE-value space ({1,2,3}), not six: `M = I − 2ââᵀ` is sign-blind, so +X and −X are the same plane family and the marker encodes the plane component only (`axis/2 + 1` from the 6-value axis param). **P1 amendment (adjudicated at implementation, 2026-09-05):** the flip lands BEFORE the stored rotation, on position AND normal AND tangent, and the stored scale stays POSITIVE. The design's original "flip after, negate scale" was algebraically wrong (negated uniform scale is central inversion, not plane reflection; flip-after gives `R·M·n ≠ M·R·n`). The exact construction: stored `R' = M·R·M` (proper; for axis-aligned M and the shader's `R = Rz·Ry·Rx`: `θx' = my·mz·θx`, `θy' = mx·mz·θy`, `θz' = mx·my·θz` — the angle about the mirror normal keeps its sign, the other two negate), stored `t' = M(t − dâ) + dâ`, scale unchanged, and the vertex shader multiplies the local position/normal/tangent component `k−1` by −1 before applying the stored rotation: `world' = R'(w·M·v) + t' = M·R·(w·v) + t'`, exactly the planar reflection; normals follow identically (`R'(M·n) = M·R·n`). Branchless in the shader (`msign` vector; unmarked instances get all-ones — byte-identical to the pre-mirror shader). The mesh vertex shader change is this conditional; the shadow-depth pass (shadow_depth.wgsl) is untouched — see Deferred for the silhouette consequence. Rejected alternatives (pre-amendment): per-draw uniform flag (buffer side-channel the draw path can't see without plumbing), two-sided lighting for all draws (touches every scene's shading for one feature), flip-after with negated scale (the algebra above). Consequences, stated honestly: `rot_pad.w` semantics change from "always zero" to "producer-defined marker" — every existing producer already writes 0 (bytemuck zero-fill + explicit writes), and the marker is only meaningful on a buffer this atom produced; a future second marker consumer must coordinate.
- **D6 — Apply order is declared, not solved: Loop first, then Mirror.** `scene_mirror` applicability requires: exactly one render_scene, no kind in `SlotGroup::Objects` applied, and every object group's `instances` port fed by at most ONE shared top-level producer (the loop's scene_array) or none. Loop-after-mirror must also be safe: the loop's splice with `replace_existing: false` would leave the reflect feed in place and wire nothing (silent break). P0 adds `GroupSplice::replace_existing`; the loop descriptor sets it `true` (its scene_array legitimately takes over whatever fed instances — the camera-switch precedent), which makes loop-after-mirror replace the reflect→group wire with scene_array→group. The reflect node remains in-graph inert; mirror remove's repoint restore re-wires reflect→group only while the loop is not also being removed — remove order mirror-then-loop is enforced by the applicability check above (loop remove after mirror remove is the only supported tear-down). Rejected: chain-insert semantics (loop array feeds reflect feeds objects) — one modifier owning one port per direction is the framework's invariant; insert-order bookkeeping in the remove arm is exactly the desync class the inv gates exist to catch.
- **D7 — RT inherits the instancing limitation.** Mirrored copies render in the raster path only; RT reflections/shadows/GI keep seeing the object's base transform (the documented `render_scene.rs:5483` single-transform accel build). Accept for v1: the mirror is a raster-path stylisation, and RT-primary scenes already live with this for loop copies. Trigger to revisit: a shipped look that needs mirrored geometry IN RT reflections.
- **D8 — Defaults derived from scene bounds, like the loop's cell.** `axis = +Y` (floor), `plane_offset = scene_bounds min-Y` (the floor), fallback 0.0 without bounds — the apply-time derivation pattern from `build_scene_loop_plan` (`scene_modifier.rs:530-537`). Stamped range for the offset row is scene-scaled (`scene_exposure.rs` `scene_scaled_range`, the orbit `near`/`far` pattern).
- **D9 — Toggle = gate-style, off is free.** `enabled` rides the plan like fog's gate: off zero-fills the mirrored half, originals pass through byte-identical (INV-MR1) — one dispatch always, matching fog's always-on multiply. EnableDecl::NodeParam { param: "enabled", on: 1.0, off: 0.0 }.

## 3. Design body

### 3.1 The atom

```rust
// crates/manifold-renderer/src/node_graph/primitives/reflect_array.rs
crate::primitive! {
    name: ReflectArray,
    type_id: "node.reflect_array",
    inputs:  { r#in: Array(InstanceTransform) optional },
    outputs: { out: Array(InstanceTransform) },
    params: [ axis (Enum, labels = scene_array::AXIS_LABELS),
              plane_offset (Float, default 0.0),
              enabled (Float, default 1.0) ],
    // freeze codegen path: wgsl_body, fusion_kind = Map, input_access = Indexed;
    // capacity-mapped output (2× input capacity) — declared in the spec.
}
```

Kernel semantics (pseudocode, one thread per OUTPUT slot, `cap` = input capacity; **P1-amended off semantics**: `enabled == 0` zeroes ONLY the mirrored half — originals pass through byte-identical unconditionally, per INV-MR1/D9):

- slot `j < cap`: copy input `j`; if input slot `j` has zero scale (dead, per INV-MR5) → zero-scale.
- slot `cap + j`: if input slot `j` has nonzero scale and `enabled != 0` → mirrored transform per D5 (amended), marker `= axis/2 + 1`; else zero-scale.
- unwired `in`: single identity instance (nonzero scale) at the origin.

### 3.2 The mesh vertex shader change

One conditional in the scene mesh vertex stage: after the stored Euler rotation is applied to the vertex normal, flip the normal component `int(rot.w + 0.5) − 1` when `rot.w > 0.5` (marker from D5). Positions are exact via the stored TRS; winding is irrelevant (no culling, section 1). ⚠ VERIFY-AT-IMPL: locate the exact normal-transform line in the scene mesh WGSL; the flip happens there, before lighting. The RT path does not read this shader (accel build is transform-level, D7).

### 3.3 Liveness signal

The mirror cannot see scene_array's `count` param (separate nodes, separate uniforms, fusion boundary). Liveness is read in-band: an input slot with `pos_scale.w == 0` is dead — scene_array's surplus mask is exactly zero-scale, and the D11 draw already treats zero-scale as "draws nothing". A mirrored copy exists iff its source slot has nonzero scale (INV-MR5). No uniform sharing, no cross-node coupling.

### 3.4 The kind (`scene_mirror`)

`crates/manifold-renderer/src/node_graph/scene_modifier.rs` gains `mod scene_modifier_mirror` (fog-shaped):

- `SCENE_MIRROR_DESCRIPTOR`: `kind_id "scene_mirror"`, `display_name "Scene Mirror"`, `slot_group SlotGroup::Objects`, gate enable (D9), trace = every minted node (fog precedent: all minted nodes are traced), row whitelist: Enabled (toggle-curated like fog), Axis, Plane Offset.
- `build_scene_mirror_plan(def, render_scene_node_id)`:
  1. Applicability per D6 (one render_scene; no Objects-slot kind applied; shared-or-none instances producer across all object groups).
  2. Instances producer detection: top-level wires `to (group, "instances")` per object group; all groups must share one producer node (or none).
  3. Mint `mirror_reflect` (`node.reflect_array`, params stamped: axis=+Y, plane_offset from scene bounds (D8), enabled=1). No existing producer → also mint `mirror_base` (`node.scene_array`, count=1, cell_size=0, axis=+X — one live identity instance).
  4. `repoints`: per group, `PortRepoint { target: group, port: "instances", new_producer: mirror_reflect, restore_types: &["node.scene_array", "node.reflect_array"] }` — restore covers the loop ordering per D6.
  5. `new_wires`: producer.out (or `mirror_base.out`) → `mirror_reflect.in`; `mirror_reflect.out` → `group.instances` per group.
  6. One `GroupSplice` per group with `replace_existing: true` (P0 field) and source = mirror_reflect — present purely so the interface input + group_input exist when the loop never spliced; when they already exist the splice adds/replaces only the wire.
  7. Exposures via `plan_skeleton` + whitelist; Enabled row toggle-curated (fog pattern at `scene_modifier.rs:849-862`).
- Remove arm is the generic command re-deriving this plan: drops minted nodes, restores the pre-mirror producer → group.instances wire via `restore_types`.

### 3.5 Infra change (P0) — `GroupSplice::replace_existing`

`crates/manifold-core/src/scene_modifier.rs:31` gains `pub replace_existing: bool`. Apply semantics (editing `scene_modifier.rs:224` splice step) split the conflated behaviour: (a) interface input + group_input: add only if missing (unchanged); (b) top-level wire: when `replace_existing`, drop all wires `to (group, inner_port)` not from `source_doc_id`, then add `source → group.inner_port`; when false and the input pre-exists, the PLAN BUILD fails loud (build-time `None`, never a silent skip — the current `continue` is the kind-#3 blocker). The loop descriptor sets `replace_existing: true` (D6); its apply behaviour on the happy path is byte-identical to today (no prior instances wire exists when the loop applies to a fresh scene). Enforcement: editing tests both arms + all existing loop/fog gates green.

## 4. Invariants & enforcement

- **INV-MR1 (off is free):** `enabled == 0` → output `[0, cap)` byte-identical to input, `[cap, 2cap)` zero-scale. *Enforcement:* gpu_test `identity_at_off` (positions + markers), SCENE_FX D3 pattern.
- **INV-MR2 (exact reflection):** a live mirrored slot is the exact planar reflection of its source: `pos' = M(pos − dâ) + dâ`, scale negated, stored rotation proper (det +1) with world action equal to `M·R`. *Enforcement:* gpu_test vs CPU-computed expected transforms for all 6 axes + a nonzero offset (ADDING_PRIMITIVES value-level proof, freeze codegen path, standalone AND fused).
- **INV-MR3 (marker discipline):** `rot_pad.w ∈ {0, 1, 2, 3}`; `0` = original, `k > 0` = mirrored across the plane perpendicular to component `k−1` (P1-amended from the design's 6-value space — the reflection matrix is sign-blind); only `node.reflect_array` writes nonzero. *Enforcement:* gpu_test asserts markers; phase-brief negative `rg` gate on other primitives touching the marker word.
- **INV-MR4 (capacity contract):** output buffer fixed at 2× input capacity; `count`/`plane_offset`/`enabled` writes never rebuild. *Enforcement:* gpu_test buffer-size assertion + live `enabled`/`plane_offset` writes without structural rebuild (the BUG-757c (scene-loop-copies-param-inert) test shape).
- **INV-MR5 (liveness by scale):** a mirrored copy exists iff its source slot has nonzero scale. *Enforcement:* gpu_test with a partially masked (some zero-scale) input.
- **INV-MR6 (loop seam untouched):** the mirror changes no camera input; loop wrap purity (SCENE_LOOP INV-3) holds with a mirror applied. *Enforcement:* existing loop wrap/roundtrip gates extended with mirror-applied graphs, all green.
- **INV-MR7 (remove restores):** mirror remove returns the graph to the exact pre-mirror wiring (loop present or not); loop remove after mirror remove is unchanged behaviour. *Enforcement:* `scene_modifier_inv_gate` extension + editing round-trip test (apply → save → reload → remove → diff graph).
- **INV-MR8 (one splice owner):** at most one modifier kind owns `(group, instances)` at a time — D6 ordering + P0's fail-loud build check. *Enforcement:* editing test — a second splice without `replace_existing` errors, not skips.

## 5. Phasing

### P0 — Splice take-over infra (`GroupSplice::replace_existing`)

- **Entry:** wave/scene-mirror at the design commit. Re-verify anchors: `GroupSplice` at core `scene_modifier.rs:31`; splice apply at editing `scene_modifier.rs:224-260`; loop splice construction at renderer `scene_modifier.rs:571`.
- **Read-back:** decisions D6, section 3.5; forbidden: changing loop apply behaviour on the happy path (its `replace_existing: true` must be behaviour-identical to today), any retention of the silent skip.
- **Deliverables:** `replace_existing` field (core); apply split (a)/(b) + fail-loud build check (editing); loop descriptor sets `true`; editing tests: replace arm, fail-loud arm, loop gates untouched.
- **Gate (positive):** `cargo nextest run -p manifold-editing -p manifold-renderer scene_modifier` green; loop round-trip + inv gates green unchanged. **Gate (negative):** `rg -n "continue;$" crates/manifold-editing/src/commands/graph/scene_modifier.rs` in the splice loop — the silent-skip pattern is gone.
- **Demo:** none — L1 (no user surface).

### P1 — `node.reflect_array` + vertex-shader marker

- **Entry:** P0 merged. Re-verify: no-cull anchor (section 1), scene_array capacity pattern (`scene_array.rs:13-21`), Euler-order ⚠ anchor — READ the scene mesh vertex WGSL FIRST, derive the conjugated-angle closed form against it, restate the formula in a comment + test.
- **Read-back:** D3, D4, D5, section 3.1–3.3; forbidden: touching the RT path, per-draw uniforms, or any producer other than the new atom writing markers; no new `Arc<Mutex>`; no `create_compute_pipeline(include_str!)` runtime kernel — freeze codegen only.
- **Deliverables:** `reflect_array.rs` (primitive + `wgsl_body` + gpu_tests INV-MR1/2/3/4/5); vertex-shader marker flip (one conditional); freeze/fusion proofs (standalone AND fused, per ADDING_PRIMITIVES); Euler derivation test (all 6 axes, nonzero offset, CPU expected).
- **Gate (positive):** `scripts/gpu_proofs_gate.py` green (touched primitive kernel — mandatory); value-level proofs vs CPU-computed expected. **Gate (negative):** `rg -n "rot_pad\[3\]" crates/manifold-renderer/src/node_graph/primitives/` — zero hits outside `reflect_array.rs`; `rg -n "create_compute_pipeline" crates/manifold-renderer/src/node_graph/primitives/reflect_array.rs` — zero hits.
- **Demo:** headless render of a lit sphere + floor-plane mirror to PNG (artifact for the record only; acceptance is the computed region-mean luminance probe — mirrored hemisphere lit. No agent judges an image; Peter looks live in the app).

### P2 — `scene_mirror` kind

- **Entry:** P1 merged. Re-verify: fog template anchor (`scene_modifier.rs:809`), cards registry anchor (`cards.rs:476`), scene_bounds derivation anchor (`scene_modifier.rs:530-537`).
- **Read-back:** D6, D8, D9, section 3.4; forbidden: UI/app crate edits (the picker and card chrome are registry-driven — a kind that needs UI work has failed the descriptor contract); no per-kind logic in editing commands.
- **Deliverables:** `scene_modifier_mirror` module + inventory submit; inv-gate extension (apply loop→mirror, remove mirror, remove loop — graph diff vs golden); round-trip gate (apply → save → reload → modulate Plane Offset → remove); card chrome parity test (rows: Enabled/Axis/Plane Offset; toggle chrome); e2e import test on DamagedHelmet (`scene_loop_e2e_import` pattern): mirrored copy visible — computed probe (region below the floor plane non-background), no human-look gate.
- **Gate (positive):** all above named tests; `landing_gate.py` touched-crate green. **Gate (negative):** `rg -n "scene_mirror|Scene Mirror" crates/manifold-app/src crates/manifold-ui/src` — zero hits (proves no UI leak).
- **Performer gesture:** *"loop the helmet, drop the mirror under it, ride Plane Offset on the kick."* Gate exercises it: e2e applies loop+mirror, writes Plane Offset via the modulation path AFTER a project reload, asserts the reflected region moves.
- **Round-trip gate:** e2e saves and reloads the `.manifold` project mid-test; card rows survive and stay writable (BUG-pvbu (scene-loop-panel-params-dropped-on-reload) class).
- **Demo:** `cargo xtask ui-snap gltfscene`-class headless shot with the Scene Mirror card open (L3 via ui-flow if the flow driver reaches the modifier menu; otherwise L2 PNG byproduct + Peter live).

## 6. Decided — do not reopen

1. Mirror = instance-array reflection atom, not a render pass (D1), not fold_mesh splices (D2).
2. `rot_pad.w` in-band marker + vertex-shader component flip (D5) — the shared-bytes contract change is accepted and scoped.
3. Apply order Loop→Mirror, enforced by applicability; `replace_existing` is the take-over mechanism, chain-insert is rejected (D6).
4. RT path: raster-only visual, documented inheritance of the single-transform accel limitation (D7).
5. Zero UI/app crate changes for the kind — registry-driven picker and cards are the contract (P2 negative gate).
6. Toggle is gate-style with off-is-free identity output (D9).
7. Flip-before construction with positive stored scale and the three-value marker — the P1-amended D5 math is the committed behavior; flip-after/negated-scale is disproven, do not re-derive it.
8. Fusion is BLOCKED-tracked (standalone-only + region-exclusion proof), never a quiet exemption — BUG-orm4 (scene-mirror-blocked-output-multiplier-capacity) + BUG-x72p (scene-mirror-blocked-gather-input-fusion).

## 7. Deferred

- **Blend amount** (partial mirror morph) — trigger: a shipped look asks for half-mirrors.
- **Arbitrary plane orientation** (non-axis-aligned) — trigger: kaleidoscope-mirror requests; fold_mesh covers mesh-local kaleidoscopes today.
- **Multiple mirrors per scene** (Objects-slot multiplicity) — trigger: second concrete use; slot-group exclusivity makes this a framework change, not a kind change.
- **Mirrored instances in RT reflections/shadows** — trigger: D7's named load-bearing case.
- **Closed-loop 3D feedback** — tracked as BUG-q4h2 (3d-scene-feedback-and-scene-space-mirror); needs the render_scene-output seam designed separately.
- **Mirrored shadow-depth silhouettes** (P1-known inconsistency): `shadow_depth.wgsl` reads the raw TRS, so a marked instance casts the UNFLIPPED shape at the mirrored position (stored scale is positive by the D5 amendment). Visible only where mirrored copies cast shadows. Trigger: a shipped look where the silhouette mismatch reads; fix is the same marker conditional in the shadow-depth vertex stage.
- **Fused-region reflect_array** — the freeze compiler cannot express 2× output capacity or fuse BufferGather-input atoms today; shipped standalone-only with a region-exclusion proof. Tracked: BUG-orm4 (scene-mirror-blocked-output-multiplier-capacity), BUG-x72p (scene-mirror-blocked-gather-input-fusion). Trigger: either compiler gap closes → add the fused numerical proof and retire the exclusion test.
- **Editing-seam follow-ups from P2** — the generic remove's splice-strip is per-kind-blind (P2 ships a renderer-side workaround in the remove-re-derived plan) and `EnableDecl::Gate` carries unpopulated amount/target fields for gate-by-param kinds. Tracked: BUG-6y91 (scene-modifier-kind3-editing-seam-followups).
- **Mirrored shadow casting as a toggle** — trigger: a look where mirrored copies must NOT cast.
