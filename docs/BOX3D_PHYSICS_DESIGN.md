# Box3D Physics — rigid bodies as a graph citizen

**Status: APPROVED 2026-07-09 (Peter) — design ready, awaiting build (Sonnet, P1–P4); differentiator, not release-gating; box3d MIT license confirmed 2026-07-09 · design 2026-07-07 · Fable**
**Prerequisites: none for P1–P3 (renders through the shipped `node.render_copies`).
P4 (content colliders) wants the depth-estimate primitive, already shipped.**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting
any phase.**

Peter's directives (2026-07-07): box3d over alternatives — "box3d is very appealing
given it's from Erin Catto, I think it would be relatively safe to trust." The vision:
"Real-time physics and simulations on stage that react to the music and interact with
the displays, other objects etc would be amazing and no one else can do that right
now." Risk posture, his call: "Manifold is still early days … Any bugs found from the
early releases of Box3D will be caught via full show run throughs etc so I'm not
worried too much."

The governing insight: MANIFOLD doesn't need to build a physics engine or a physics
*mode* — it needs one stateful atom that owns a `b3World`, description ports going in,
and `Array(InstanceTransform)` coming out. Everything downstream (instanced rendering,
materials, lights, beat modulation, triggers) already exists. The differentiator is
not physics existing (TouchDesigner has Bullet, Notch has rigid bodies); it is physics
as a **musical citizen** — impulses quantized to beats, gravity on a fader, and bodies
colliding with the *content* via depth-estimated heightfields.

Companions: `SIMULATIONS_DESIGN.md` (XPBD lane — this doc supersedes its §8
"GPU rigid bodies" deferral; coupling stays deferred there), `REALTIME_3D_DESIGN.md`
(the scene bodies will eventually join via `instances_n`), `MATERIAL_SYSTEM_DESIGN.md`
(bodies get materials for free through the existing renderers).

---

## 1. Audit — what exists (verified 2026-07-07)

| Piece | Where | State |
|---|---|---|
| Instanced renderer: `Array(MeshVertex)` × `Array(InstanceTransform)` → lit, materialed copies | `primitives/render_instanced_3d_mesh.rs:62` (`node.render_copies`) | **The output surface, shipped.** Physics needs zero new rendering |
| `InstanceTransform` = pos+scale vec4, euler-XYZ+pad vec4, 32 bytes, const-asserted | `generators/mesh_common.rs:93`, euler consumed at `shaders/render_instanced_3d_mesh.wgsl:108–142` | Poses are absolute per frame → quat→euler at the boundary is safe (no cross-frame interpolation) |
| CPU-struct port pattern (Camera / Light / Material) | `node_graph/ports.rs:36–53` | **The precedent for BodySet / ColliderSet ports.** Material M1 is the plumbing pattern-copy |
| Stateful FFI primitive: persistent worker + state in `extra_fields`, CPU→GPU upload via tiny compute pass | `primitives/blob_detect_ffi.rs` (state :77, upload :398–426) | The upload path physics copies verbatim. The *async worker* half is the named wrong move (§D2) |
| Async texture readback at analysis res (staging + cadence + `ReadbackRequest`) | `blob_detect_ffi.rs:314–374`, `gpu_readback.rs` | **Exact precedent for P4 heightfield colliders** — lag-tolerant by design |
| Edge-gated trigger input (recompute on integer edge) | `seed_particles_from_texture.rs:139` (`reset_trigger`) | The impulse/reset trigger pattern; bar-quantization rides existing trigger machinery |
| Dynamic port groups via `reconfigure` | `node.mux_texture`; `render_scene` (REALTIME_3D D2) | The multi-body-set mechanism (P3) |
| Vendored-native build precedent (`build.rs` + build-deps) | `manifold-media/build.rs`, `manifold-playback/build.rs` | House pattern for compiling non-Rust code in-tree |
| Depth estimation from any frame | `primitives/depth_estimate_midas.rs` | P4's height source, shipped |
| Rigid bodies previously deferred | `SIMULATIONS_DESIGN.md` §8 ("GPU rigid bodies … XPBD shape-matching is the cheap version") | **Superseded by this doc** — Peter's 2026-07-07 call |

box3d itself (v0.1.0, released 2026-06-30, MIT, C17): `b3CreateWorld/b3World_Step
(worldId, timeStep, subStepCount)`, body create/destroy with `b3Quat` poses, shapes
(sphere/capsule/hull/mesh/**heightfield**), joints (revolute/prismatic/distance/motor/
weld/wheel), `b3World_SetWorkerCount`, record/replay determinism validation.
`⚠ VERIFY-AT-IMPL: exact decl signatures — copy from the vendored headers at
crates/manifold-physics/vendor/box3d/include/box3d/*.h, never from this doc or memory.`

Classification: the safe wrapper crate and the two description ports are **genuinely
new**; everything else is **wiring into shipped surfaces**.

## 2. Decisions

- **D1 — box3d, vendored and pinned, statically linked. New crate `manifold-physics`.**
  Vendor the v0.1.0 source tree at an exact commit under
  `crates/manifold-physics/vendor/box3d/`; `build.rs` + `cc` compiles it (C17);
  hand-written `#[repr(C)]` decls in a `sys` module, **copied from the vendored
  headers** with a const size/align assert per struct (`mesh_common.rs:98` pattern).
  A safe wrapper (`PhysicsWorld`) is the only thing the rest of the workspace sees —
  no `b3*` type crosses the crate boundary. Crate depends on nothing internal (like
  `core`/`gpu`); `manifold-renderer` gains the dependency.
  Rejected: **rapier3d** (pure Rust, no FFI) — Peter's call on Catto's pedigree and
  the data-oriented perf profile; recorded so nobody re-proposes it as a "safer"
  swap mid-build. Rejected: **runtime dylib bundle** (the DepthEstimator pattern) —
  physics is an engine dependency, not an optional plugin; a bundle-resolution
  failure at showtime is an unacceptable failure mode for a set piece, and static
  linking removes it. Rejected: **bindgen at build time** — heavy build-dep for a
  stable pinned header set; the const-assert tests catch drift on version bumps.
  **Version bumps are deliberate sessions**: re-vendor, rerun the full P1 gate suite,
  never a casual `git pull` of vendor.
- **D2 — The world is node state, not a wire, and the step is synchronous.**
  One stateful atom `node.physics_world` owns the `b3World` in `extra_fields`
  (blob-tracker precedent, `blob_detect_ffi.rs:157`), keyed per the two-cache rules;
  `Drop` calls `b3DestroyWorld`. Each frame, inside `run()` on the content thread:
  apply forces → step → read poses into a pre-allocated scratch → upload via the
  blob-pattern compute pass. The world **rebuilds only on description-hash change or
  reset edge** — never per frame.
  Rejected: **a World-handle port** threaded through mutator nodes ("add_bodies →
  add_collider → step") — wires carry values; a mutable handle makes sibling
  execution order load-bearing and breaks the graph's value semantics. Rejected:
  **async background-worker stepping** — you will be tempted to copy the blob
  worker whole; don't. Blob results may lag frames invisibly; body poses lagging
  the render is jitter on every beat hit, and async stepping kills determinism.
  The step is cheap (§6); it runs inline.
- **D3 — Inputs are CPU-struct description ports:** `body_set: BodySet` (from
  `node.body_set`) and `colliders: ColliderSet optional` (from `node.collider_set`,
  P3). Same lifetime model as Camera/Light/Material (`ports.rs:36`); plumbing is a
  pattern-copy of Material M1. `node.body_set` is a pure emitter: shape (sphere/
  box/capsule), count, spawn region (grid/box-volume/point), density, friction,
  restitution, initial velocity, seed. P3 adds `body_sets: Int` dynamic port groups
  (`reconfigure`, mux/render_scene precedent). v1 caps: **4 body sets × 1024
  bodies** — constants, bumpable, HUD-visible.
  Rejected: bodies as an `Array(Particle)`-style GPU wire — body state lives inside
  the C world and is not meaningfully addressable as a GPU buffer; faking it buys a
  readback for nothing.
- **D4 — Output is `Array(InstanceTransform)` per body set (`transforms_n`). No
  private renderer** (SIMULATIONS D7 doctrine). Wire into `node.render_copies`
  today; `render_scene`'s deferred `instances_n` slot later. Quat→euler-XYZ at the
  FFI boundary (poses absolute per frame — the shader rebuilds the matrix, so no
  continuity artifacts; precision at gimbal poles is the honest, mild cost).
  Uniform scale per body (the 32-byte layout allows nothing else; non-uniform
  scale of a rigid body is physically meaningless anyway).
- **D5 — Fixed-substep time, mirroring SIMULATIONS D4.** dt = content-clock delta ×
  `time_scale` (port-shadowed → slow-motion collapse on a beat_ramp), fed to
  `b3World_Step` with a fixed `substeps` param (default 4). `worker_count` default
  **1** in v1 — determinism first; raising it is a perf-phase decision gated on
  re-running the determinism tests at the new count. Deterministic re-runs on the
  same build/seed/settings (export re-renders reproduce). Cross-machine determinism:
  not claimed.
- **D6 — Beat-native surface, zero new modulation machinery.** Gravity xyz,
  time_scale, wind: port-shadowed params on `node.physics_world`. `impulse_trigger`
  + `reset_trigger`: edge-gated scalar inputs (`seed_particles_from_texture.rs:139`
  precedent), so a bar-quantized trigger or a kick-follower fires them like anything
  else. Impulse shape params: origin xyz, radius, strength (radial burst v1).
  Performer gestures this exists for: **kick → radial impulse** (bodies jump on the
  beat), **fader → gravity** (weightlessness on the breakdown), **bar-quantized
  reset** (the stack rebuilds itself every 8 bars).
- **D7 — Content as collider (P4, the differentiator).** `node.heightfield_collider`:
  `Texture2D` in (depth-estimate output or any luma), async readback at analysis
  res on a cadence — the *exact* blob-tracker readback recipe, where lag is
  acceptable and stated (a collider surface updating ~5–15 Hz under falling bodies
  reads as live; poses never lag, only the terrain refresh). Emits heightfield data
  in `ColliderSet`; the world swaps the heightfield shape on refresh (per-cadence
  action, not per-frame). `⚠ VERIFY-AT-IMPL: whether box3d supports in-place
  heightfield sample updates vs shape recreate — read the vendored heightfield
  header; recreate-per-refresh is the acceptable fallback.`
  On stage: bodies rolling off the video content itself — the footage becomes
  terrain. Nobody has this live.
- **D8 — Supersedes SIMULATIONS_DESIGN §8's "GPU rigid bodies" deferral** (Peter,
  2026-07-07). XPBD stays the plan for cloth/liquids/grains; rigid contact/stacking
  is box3d's job. Two-way coupling (cloth ↔ rigid) remains deferred in SIMULATIONS
  §8. The XPBD lane's `node.collide_shapes` and this doc's `node.collider_set` get
  reconciled at that lane's §2.5 audit — do not pre-unify the types here.

## 3. Data model (committed)

```rust
// crates/manifold-physics/src/lib.rs — safe wrapper, no b3* types exported
pub struct PhysicsWorld { /* b3WorldId + owned defs; !Send is fine (node-resident) */ }
pub struct BodyPose { pub pos: [f32; 3], pub quat: [f32; 4] } // quat xyzw
impl PhysicsWorld {
    pub fn new(def: &WorldDef) -> Option<Self>;            // None = init failure, logged
    pub fn step(&mut self, dt: Seconds, substeps: u32);
    pub fn spawn_set(&mut self, set: &BodySetDef) -> SetHandle;
    pub fn set_colliders(&mut self, colliders: &ColliderSetDef);
    pub fn apply_radial_impulse(&mut self, origin: [f32; 3], radius: f32, strength: f32);
    pub fn poses_into(&self, set: SetHandle, out: &mut Vec<BodyPose>); // no alloc after warm-up
}

// manifold-renderer/src/node_graph/physics.rs — CPU port structs
// (plumbing pattern-copy of material.rs M1; PortType::BodySet, PortType::ColliderSet)
pub struct BodySet {
    pub shape: BodyShape,            // Sphere | Box | Capsule (+ half_extents / radius)
    pub count: u32,                  // clamped to the set cap (1024)
    pub spawn: SpawnRegion,          // Grid | BoxVolume | Point (+ extents, spacing)
    pub density: f32, pub friction: f32, pub restitution: f32,
    pub initial_velocity: [f32; 3], pub seed: u32,
}
pub struct ColliderSet {
    pub shapes: Vec<ColliderShape>,  // Plane | Box | Sphere, each with TRS
    pub heightfield: Option<HeightfieldData>, // P4; None until then
}
```

`node.physics_world` (P1 single-set; P3 dynamic groups): inputs `body_set: BodySet
required`, `colliders: ColliderSet optional`, `impulse_trigger: ScalarF32 optional`,
`reset_trigger: ScalarF32 optional`; params `gravity_x/y/z`, `time_scale`, `substeps`,
`ground_plane: Bool` (a free floor so the first preset needs zero collider atoms),
impulse origin/radius/strength, `worker_count`; outputs `transforms: Array(InstanceTransform)`.
Description-hash of (BodySet, ColliderSet-minus-heightfield, seed) decides world
rebuild; heightfield refresh and port-shadowed forces are live updates, not rebuilds.

## 4. What it buys on stage

- A wall of bricks stacked at the top of the set; the drop hits, one bar-quantized
  impulse, and it collapses in time — then rebuilds itself on the next phrase
  (reset trigger).
- Gravity on a fader: the debris field goes weightless in the breakdown, slams back
  at the drop. Slow-motion collapse via `time_scale` on a beat_ramp.
- Bodies raining onto the *video content* — depth-estimated footage as terrain, so
  the performer's silhouette or the visual itself catches and sheds the debris (P4).
- Every body is a lit, materialed instanced mesh — PBR bricks, cel-shaded tumbling
  totems — because the render path is the one that already exists.

## 5. Phasing (Sonnet-executable)

Forbidden, all phases: async/deferred stepping (D2) · World-handle wires (D2) ·
private renderers (D4) · per-frame world rebuild or per-frame allocation in the
step/readout path (pre-allocated scratch only) · editing vendored box3d source ·
synthesizing FFI decls from memory instead of the vendored headers · `Arc<Mutex>`
anywhere · new modulation machinery (D6).

- **P1 — Crate + vertical slice.** `manifold-physics` (vendored pinned box3d, cc
  build, sys decls with per-struct const asserts, safe wrapper, unit tests),
  `PortType::BodySet` plumbing, `node.body_set`, `node.physics_world` (single set,
  `ground_plane` on), bundled **Falling Bricks** preset through `node.render_copies`.
  Update the CLAUDE.md crate table + docs index in the landing.
  Read-back: this doc; `blob_detect_ffi.rs` end-to-end; `material.rs` M1 port
  plumbing; DECOMPOSING_GENERATORS §2.5 (audit statement in the phase notes).
  Gate (positive): wrapper unit tests — world create/step/destroy, poses land
  inside expected bounds after N steps, **determinism: two identical runs, byte-
  identical pose streams**; headless PNG at t=0 and t≈3 s shows bricks fallen and
  stacked (L2 demo, actually read). `MANIFOLD_RENDER_TRACE=1` run with 1024 bodies:
  no frame >20 ms, step time reported in the phase notes (BUG-035 gate).
  Gate (negative): `rg 'Arc<Mutex' crates/manifold-physics crates/manifold-renderer/src/node_graph/physics*` → zero;
  `rg 'wgpu' crates/manifold-physics` → zero; vendor tree byte-identical to the
  pinned upstream commit (diff against the recorded SHA). Round-trip: save/reload
  the preset → bodies re-simulate from descriptions (sim state is intentionally
  ephemeral — reload resets the sim; this is the designed behavior, stated here so
  nobody "fixes" it). Test scope: full workspace sweep (new crate = infra). CPU-only
  — default sweep covers it; no gpu-proofs run needed.
- **P2 — The performance surface.** Impulse + reset triggers (edge-gated), gravity/
  time_scale port-shadowing verified live, perf HUD line (bodies × substeps ×
  step-ms). Performer-gesture gate (BUG-039 lesson): a full-range LFO on gravity_y
  and a saw beat_ramp on time_scale both behave (no clamp surprises); demo flow —
  a trigger-driven impulse preset, PNG pair before/after the hit (L2). Focused
  tests (`-p manifold-renderer --lib` + `-p manifold-physics`).
- **P3 — Colliders + multiple body sets.** `node.collider_set` (plane/box/sphere,
  port-shadowed TRS → kinematic movement: a sweeping bar that bats bodies away),
  `PortType::ColliderSet`, `body_sets` dynamic groups per `reconfigure`, caps as
  constants. Gate: kinematic collider PNG sequence (body deflected); dynamic-port
  reconfigure round-trips preset save/load (round-trip gate); determinism holds
  with moving colliders. Focused tests.
- **P4 — Content colliders (the differentiator).** `node.heightfield_collider` per
  D7 (readback cadence param, analysis dims param), `HeightfieldData` in
  `ColliderSet`. Demo (L2, the show-piece): bodies dropped onto a depth-estimated
  frame, PNG shows them resting on the image's ridges. Gate: refresh happens at
  cadence not per frame (frame-trace shows no per-frame readback submit); worker
  poses never wait on readback (assert no blocking read on the frame path).
  Focused tests + a render-trace run.

## 6. Performance (stated honestly)

Step cost is CPU, on the content thread, inline (D2): box3d is built for "large
piles" with SIMD; at the v1 cap (4096 bodies, substeps 4, worker_count 1) the
expected step is well under 2 ms on Apple Silicon — **expected, not measured; P1's
render-trace gate produces the real number** and the HUD keeps it visible live.
Budget context: content render baseline is 4.5–5.5 ms in a 16.6 ms frame. Pose
readout + quat→euler + upload at 4096 bodies is ~130 KB/frame through the existing
compute-upload path — noise. If the measured step blows the budget, the knobs are
(in order): body caps, substeps, then worker_count >1 with determinism re-gated —
never async stepping.

## 7. Decided — do not reopen

1. box3d, vendored @ v0.1.0 pin, static link, new `manifold-physics` crate; rapier3d
   and runtime-dylib rejected (D1). Version bumps are deliberate gated sessions.
2. World = node state; step = synchronous in `run()`; no World-handle wires, no
   async stepping (D2).
3. Descriptions in via CPU-struct ports (BodySet / ColliderSet), transforms out as
   `Array(InstanceTransform)`; existing renderers only (D3, D4).
4. Fixed substeps; worker_count 1 until a perf phase re-gates determinism (D5).
5. Beat surface = port-shadowed params + edge-gated triggers; nothing new (D6).
6. Content-as-collider via async heightfield readback; pose path never waits (D7).
7. SIMULATIONS §8 rigid-body deferral superseded; XPBD lane otherwise untouched (D8).

## 8. Deferred (with triggers)

- **Joints** (chains, pendulums, articulated set pieces — box3d has the full set):
  first design pass after P1–P4 are perform-tested; a `node.joint_chain` recipe is
  cheap once the world atom exists. Trigger: a look demands articulation.
- **Contact events as a data wire** (flash on collision, contact-driven visuals):
  needs a Channels signature + cap policy. Trigger: a set piece wants hit-reactive
  shading. (Contacts as a *modulation source* is a bigger conversation — perform
  surface, not graph — park it.)
- **Mesh-shaped bodies / convex hulls from `Array(MeshVertex)`**: needs GPU→CPU
  vertex readback at spawn time (per-trigger, so the blob readback pattern fits).
  Trigger: tumbling imported glTF props.
- **Scene-mesh static colliders** (bodies colliding with `render_scene` objects):
  shares the readback need above; reconcile with SIMULATIONS' scene-SDF deferral
  when either revives.
- **`render_scene` `instances_n` input** (bodies inside the lit scene with shadows):
  already REALTIME_3D §8's deferred huge-scenes lever; physics just becomes another
  `Array(InstanceTransform)` producer when it lands.
- **worker_count > 1** (perf): trigger is a measured step over budget at a body
  count a show actually needs; re-gate determinism.
