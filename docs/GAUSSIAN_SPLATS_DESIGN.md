# Gaussian Splats — photoreal scans as a playable instrument

**Status:** APPROVED design, not built · 2026-07-05 · Fable · **baseline-reviewed 2026-07-05, cleared** (zero unlabeled forks; `render_scene` P1 prerequisite re-verified in-tree @ `8daa89fc`; the "no GPU sort exists" negative claim re-run and confirmed; camera audit row pinned — look_at_camera landed, free_camera absent. §10 levels: P1/P2 gate L1 by nature — no visual surface until P3; P3–P6 gates are already L2 with value-level pixel asserts.)
**Prerequisites:** none hard — `node.render_scene` P1 is shipped (`render_scene.rs`,
commit `8daa89fc`), which is all the scene-composite phase (P4) consumes. P1–P3 and
P5–P6 depend on nothing unbuilt.
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before
starting any phase.

Peter's directive (2026-07-04 phone brainstorm): Gaussian splats "will definitely be
important." The frame: static 3D-scan assets should "come alive." Scans increasingly
ship as splats, and per-splat attributes (position/scale/rotation/color/opacity) are
natively modulatable — **a photoreal particle system with no topology problems.** The
governing insight for MANIFOLD specifically: every splat attribute is just a channel
on an array wire, so the entire existing modulation machine — channel operators,
`wgsl_compute`, port-shadowed params, beat ramps, audio envelopes — applies to a
photoreal scan the moment the wire type exists. Splats are not a viewer feature here;
they are a playable surface.

Companions: `REALTIME_3D_DESIGN.md` (the scene splats composite into),
`IMPORT_DESIGN.md` (import doctrine: one door per class, reports over silent drops),
`CHANNEL_TYPE_SYSTEM.md` (the wire vocabulary splats ride),
`docs/MANIFOLD_GPU_ARCHITECTURE.md` (dispatch/render constraints).

---

## 1. Audit — what exists (verified 2026-07-05)

| Piece | Where | State |
|---|---|---|
| Channels wire system — `ArrayType` carries named channel specs; `MatchMode::Exact` per port, `Permissive` for generic operators | `ports.rs:197` (`ArrayType`), CHANNEL_TYPE_SYSTEM §5–§6 | Shipped. `Particle` (64B, `compute_common.rs:11`) and `MeshVertex` (48B, `mesh_common.rs:34`) are the declaration precedents, each with a `_SPECS` constant + drift assertion |
| File-source node precedent — background-thread parse, resident cache, staging upload, path via `stringBindings` | `primitives/gltf_mesh_source.rs` | Shipped (glTF wave, 2026-07-04). **The shape `node.splat_source` copies wholesale** |
| Multi-object scene renderer, shared depth | `primitives/render_scene.rs` (`8daa89fc`) | Shipped P1. As-built outputs: `color` only — the lazy `depth` output promised in REALTIME_3D §3 is **not built**; P4 here builds it |
| Instanced render-pass draws with depth attachment + configurable blend state | `manifold-gpu` `encoder.rs:695` (`draw_instanced_depth`), `device.rs:720` (blend config on pipeline creation) | Shipped. Depth-read-without-write is a `GpuDepthStencilDesc` flag |
| Compute dispatch + WGSL→MSL pipeline, persistent node-owned buffers | `dispatch_compute`, `primitive!` `extra_fields` pattern | Shipped, used by every particle atom |
| GPU sort | — | **Does not exist.** No radix/bitonic sort anywhere in `manifold-gpu` or the graph (verified by search 2026-07-05; MPS bindings cover blur/sobel/histogram/reduction, not sort). P2 builds it |
| 3D particle stack (seed/force/step/render) | `scatter_particles_3d.rs`, `euler_step_particles_3d.rs`, etc. | Shipped. Splats deliberately do NOT reuse it (D5) — no velocity channel, displacement not integration |
| Camera | `node_graph/camera.rs`, `node.orbit_camera` | Shipped. Pinned 2026-07-05 baseline review: BOTH `node.free_camera` and `node.look_at_camera` have landed (`primitives/free_camera.rs` :18, `look_at_camera.rs` :19) — REALTIME_3D P4 is done; the starter preset may use any of the three cameras |

§2.5 audit finding: nothing splat-shaped exists under any name. `scatter_particles_3d`
scatters in a volume, `spawn_from_image` samples a texture, `displace_mesh` displaces
`MeshVertex` — the nearest verbs exist but none reads or writes anisotropic gaussians.
The five atoms in §3 are genuinely new; the sort is genuinely new infrastructure.

## 2. Decisions

- **D1 — `Splat` is a canonical channel struct on the existing Array wire. No new
  port kind.** 64-byte std430 layout, same discipline as `Particle`:

  `Channels[position: Vec3F, mask: F32, rotation: Vec4F, scale: Vec3F, opacity: F32, color: Vec4F]`

  position @0, mask @12 (packs after vec3, the `Particle.life` trick), rotation
  (unit quaternion, xyzw) @16, scale (per-axis σ, linear not log) @32, opacity
  (linear 0–1, sigmoid already applied at parse) @44, color (RGBA, SH DC already
  evaluated to linear RGB at parse) @48. Stride 64 — same as `Particle`; 1M splats
  = 64MB on unified memory. Rejected: a dedicated `PortType::Splats` — the channel
  system exists precisely so new data shapes are specs constants, not port kinds.
- **D2 — The `mask` channel ships in the struct from day one.** This is the
  segmentation direction from the same brainstorm made native: a scalar 0–1 channel,
  1.0 at import, written by mask atoms (color distance, bounds), consumed by
  modulators and the renderer. Riding IN the signature (instead of a second wire or
  an extended signature) keeps `MatchMode::Exact` matching trivial — every splat atom
  speaks one signature — and costs 4 bytes that pure padding would otherwise eat.
  Rejected: emitting `Array(Splat+mask)` from mask atoms — every consumer would need
  two signatures or a strip atom; the graph noise buys nothing.
- **D3 — Import doors v1: `.ply` (INRIA layout) and `.splat` (antimatter15).** `.ply`
  is what every scan tool exports (Polycam, Luma, Scaniverse, postshot, nerfstudio);
  `.splat` is the de-facto compact web format and its parser is ~30 lines (32B
  records: pos 3×f32, scale 3×f32, color 4×u8, rot 4×u8). Both parsers are
  hand-written pure Rust in the node (IMPORT D4 doctrine; the `ply-rs` crate is
  stale and the INRIA layout is a fixed header + packed binary — parsing it is not
  worth a dependency). Parse-time transforms are part of the format contract:
  `opacity = sigmoid(raw)`, `scale = exp(raw)`, `color = 0.5 + 0.28209479 · f_dc`,
  quaternion normalized and reordered to xyzw. Rejected for v1 with triggers in §8:
  `.spz`/SOGS (compressed), SH bands ≥1.
- **D4 — Spherical harmonics: DC only in v1.** `f_rest_*` bands are dropped at parse
  with a logged report line (IMPORT D9: never a silent drop). Full SH3 is 45 extra
  floats per splat (+180B, 4× memory) for view-dependent sheen that stage content
  at performance distance doesn't read. The antimatter15 `.splat` format dropped SH
  entirely and became the web standard — the precedent that DC-only looks right.
  SH1 is deferred with a trigger (§8).
- **D5 — Splats displace; they do not integrate.** There is no velocity channel and
  no euler stepper for splats. A scan is a rest pose the graph re-derives every
  frame: `splat_source` (stable cached buffer) → modulators (pure per-frame
  transforms) → renderer. Displacement from rest is deterministic — it survives
  export (the offline audio-reactive path) and it always comes home when the fader
  drops, which is what a performed scan needs. The plausible-wrong architecture,
  forbidden by name: **do not bolt velocity/age onto `Splat` and clone the particle
  steppers.** When a splat needs ballistic physics it becomes a particle (the
  splats→particles bridge is deferred, §8, with its trigger).
- **D6 — Renderer = `node.render_splats`: depth-sorted instanced quads through the
  existing render pipeline.** Per frame, internally: (1) one compute pass projects
  splats through the wired `Camera`, computes the 2D covariance (Σ = R·S·S·Rᵀ
  conjugated by the view Jacobian — standard EWA splatting), emits clip-space quad
  extents + a view-depth sort key, and prunes off-frustum/near-zero-opacity splats;
  (2) GPU radix sort on (depth key, splat index) pairs; (3) one
  `draw_instanced_depth` call — 6 vertices, N instances, vertex shader reads the
  sorted index buffer and expands quads, fragment shader evaluates the gaussian
  falloff × opacity, back-to-front over-blend, **depth test against the optional
  scene depth input, depth write off.** Rejected: the INRIA tile-based compute
  rasterizer — better peak throughput but a much larger, riskier build; it is the
  v2 perf upgrade (§8) once real scenes exceed the quad path's budget, and the
  sorted-quad path is the proven production approach in Metal splat renderers.
  Also rejected, by name: **CPU sorting per frame** (millions of keys on the content
  thread — never) and **sort-every-N-frames** (visible popping exactly when the
  camera rides a beat ramp, which is the whole point of cameras here).
- **D7 — The sort is renderer-internal, not a graph atom.** The cut rule: the sort
  order depends on the camera, not on the data — no other consumer of `Array(Splat)`
  wants camera-dependent ordering on the wire. This is not the fused-monolith
  anti-pattern: source, masks, and displacement are all separate wire-visible atoms;
  the renderer's interior passes (project/sort/draw) are one composable render
  operation, exactly as `render_mesh`'s internal depth pass is not an atom.
- **D8 — The radix sort is shared infrastructure in `manifold-renderer`**
  (`node_graph/gpu_sort.rs`): `radix_sort_pairs(encoder, keys, values, count)` over
  u32 key/value pairs, ≤4 passes of 8-bit digits (histogram → prefix scan → scatter),
  persistent ping-pong buffers owned by the caller, zero per-frame allocation. It
  lives in the renderer crate, not `manifold-gpu` — it is WGSL compute dispatched
  through the existing ~15-method API, and that API stays small. Future consumers
  it is deliberately shaped for: particle depth sorting, the tile rasterizer.
- **D9 — Scene composition is a depth-input wire, not a `render_scene` object
  type.** `render_splats` takes optional `scene_color` + `scene_depth` inputs; wired,
  splats blend over the scene and depth-test against its geometry (mesh occludes
  splat, splat blends over mesh). P4 builds `render_scene`'s lazy `depth` output —
  already promised in REALTIME_3D §3, additive, unwired = never rendered. Unwired,
  `render_splats` stands alone over transparent black. Rejected: splat object groups
  inside `render_scene` — mixing port types per object group complicates its dynamic
  reconfigure for zero authoring win; a wire does it.
- **D10 — Placement is renderer params, camera is a port.** TRS params
  (`pos_x/y/z, rot_x/y/z, scale`) on `render_splats`, port-shadowed — the same
  per-node TRS-params shape `render_mesh`/`render_copies` use (re-anchored,
  coherence audit F4a, 2026-07-10: NOT the `render_scene` per-object-group
  convention this doc originally cited — `SCENE_BUILD_AND_GROUP_PARAMS` D3
  deletes that convention in favor of `node.transform_3d` + a `Transform`
  port), so a scan's placement is beat-addressable like everything else.
  **Open question, not resolved here (audit F4b):** whether `render_splats`
  should follow suit with a `transform: Transform` port instead of param-TRS —
  without it, placement is invisible to REALTIME_3D P6's viewport gizmos,
  which only grab `node.transform_3d` outputs (SCENE_BUILD §8). Decide before
  P3 ships if P6 gizmos are expected to reach splats. Plus the two knobs every
  splat tool has, also port-shadowed: `global_opacity` and `splat_scale`
  (multiplier on all footprints) — a performable dissolve and a performable
  "particle-ize" (scale → 0 turns a photoreal scan into a dust of points) for free.
- **D11 — Mask consumption v1: `mask_weight` on `node.displace_splats`,
  `mask_emission` on `node.render_splats` (0 = off).** Everything richer (mask-gated
  scatter, per-mask color grade) is `wgsl_compute` territory — the open family stays
  open (per `curated-via-wgsl-compute-backend`). The renderer gets exactly one mask
  consumer because emission is a render-time property; it is a param, not a bundled
  effect.

## 3. The atoms (all new — §1 audit)

| Atom | Signature | One thing it does |
|---|---|---|
| `node.splat_source` | params: path (stringBindings Browse), max_capacity (default 2M, max 8M) → `splats: Array(Splat)` | Parse `.ply`/`.splat` on a background thread (gltf_mesh_source shape: mpsc + resident cache + staging re-upload only on change). Truncation over capacity and dropped SH bands are logged report lines |
| `node.mask_splats_by_color` | `splats` → `splats`; params: target color, tolerance, softness (port-shadowed) | One pointwise dispatch: hue/color distance → writes `mask`. Tolerance on a fader = the selected region grows/shrinks live |
| `node.mask_splats_by_bounds` | `splats` → `splats`; params: box center + extents, invert (port-shadowed) | One pointwise dispatch: AABB test → writes `mask`. Doubles as scan cleanup (crop the junk floor) and as a performable reveal volume |
| `node.displace_splats` | `splats` → `splats`; params: field enum {simplex, radial}, amount, frequency, time, center, mask_weight (port-shadowed) | One pointwise dispatch: displacement from rest position, mask-weighted. Precedent: `displace_mesh.rs`. Closed two-member field family = enum param (per `inline-mux-option-table-params`), not two atoms |
| `node.render_splats` | inputs: `splats`, `camera: Camera` required, `scene_color`/`scene_depth: Texture2D` optional → `color: Texture2D`; params per D10/D11 | D6's three internal passes. Perspective and ortho cameras both supported (ortho Jacobian is the trivial affine case) |

Generic channel operators (`channel_math`, `select_channels`, rename/reorder — all
Permissive) and `wgsl_compute` consume `Array(Splat)` with zero new code: the day-one
power surface. Alpha convention of the `color` output: match `render_mesh`'s resolve
(⚠ VERIFY-AT-IMPL: read `render_3d_mesh.rs`'s output blend/alpha handling before
writing the fragment shader, per `alpha-standardisation` — do not assume).

## 4. What it buys on stage

- Phone-scan your studio, your gear, your face. Drag the `.ply` in. It renders
  photoreal, lit by nothing — splats carry their own light — and the camera dollies
  through it on a beat ramp.
- `mask_splats_by_color` on the neon sign in the scan, `mask_emission` bound to the
  kick envelope: the sign in your photoreal room pulses with the track.
- `displace_splats` simplex at amount 0 → riser sweeps it up → the room dissolves
  into a blizzard of itself and reassembles on the drop (displacement always comes
  home, D5).
- `splat_scale` on a fader: photoreal ↔ pointillist dust, one knob.
- Scene composite: a glTF hero mesh orbits INSIDE the scanned room, correctly
  occluded (D9) — import wave meets splat wave in one graph.

## 5. Performance (stated honestly)

- Memory at 1M splats: 64MB wire buffer + 8MB key/index pairs ×2 (sort ping-pong)
  + one 64MB output buffer per modulator atom in the chain. A three-atom chain on a
  2M-splat scan ≈ 500MB — fine on unified memory, real on 8GB machines; the
  `max_capacity` param is the honest ceiling, HUD shows resident splat bytes.
- Sort: 4×(histogram+scan+scatter) on 1M pairs is well under 1ms on M-series;
  projection is one cheap pointwise pass. **The real cost is fill rate** —
  overlapping gaussian quads at 4K is overdraw-bound. Production Metal splat
  renderers hold 1–2M splats at 60fps at 1080p–1440p; treat 4K + multi-megasplat as
  a headline load the perf HUD must show per-pass, and the tile rasterizer (§8) as
  the lever when a real scene misses budget. The frame budget owner is still the
  4K-margin campaign.
- Content-thread cost: zero per-frame allocation (persistent buffers, pool
  discipline); parse never touches the content tick (background thread, D3).

## 6. Phasing (Sonnet-executable)

Forbidden, all phases: velocity/age channels on `Splat` or any splat stepper (D5) ·
CPU per-frame sort or sort-every-N-frames (D6) · new deps for parsing (D3) ·
`Arc<Mutex>` anywhere · fusing source/mask/displace/render into one node (§1 audit
+ D7 give the cut lines) · touching `render_scene` beyond the additive lazy depth
output (P4).

- **P1 — `Splat` type + `node.splat_source`.** `SPLAT_SPECS` + `#[repr(C)]` struct
  (pattern: `compute_common.rs` Particle, incl. drift assertion) in a new
  `generators/splat_common.rs`; the source node shaped file-for-file like
  `gltf_mesh_source.rs`; both parsers with the D3 parse-time transforms. Fixtures:
  tests construct `.ply`/`.splat` bytes in-memory (no binary files committed);
  Peter-hands item: one real phone-scan `.ply` dropped somewhere convenient for
  eyeball checks from P3 on. Gate (positive): parser unit tests — a hand-built
  3-splat `.ply` and `.splat` yield exact channel values post-transform (sigmoid/
  exp/SH-DC verified against hand-computed numbers); drift assertion compiles.
  Gate (negative): `rg 'Arc<Mutex' ` on new files → zero; `rg 'ply_rs|ply-rs'
  Cargo.toml` → zero. Scope: focused (`-p manifold-renderer --lib`).
- **P2 — `gpu_sort.rs` radix sort.** D8's contract. Gate: CPU-parity test — 100k
  random u32 pairs, GPU result == `sort_by_key` reference, plus the already-sorted
  and all-equal-keys edge cases; buffer-reuse test proves no allocation on second
  call. Scope: focused.
- **P3 — `node.render_splats`, standalone.** D6 passes 1–3, D10 params, mask_emission
  (D11). Gate: headless PNGs, value-level — a known 3-splat scene renders with
  asserted center-pixel colors; two overlapping splats: flipping the camera 180°
  reverses the blend result (sort correctness as a pixel assert, not an eyeball);
  ortho camera renders; report the measured frame cost at 1M random splats.
  Scope: focused + `check_presets`.
- **P4 — Scene composite.** `render_scene` lazy `depth` output (additive — REALTIME_3D
  §3 promised it; follow `render_mesh`'s lazy-output rule) + `scene_color`/
  `scene_depth` consumption in `render_splats`. Gate: PNG — splat behind a cube is
  occluded, splat in front blends over it; negative: with `depth` unwired,
  `render_scene` encodes no extra pass (frame capture or dispatch-count assert);
  existing bundled 3D presets byte-identical. Scope: **full workspace sweep**
  (touches the shipped scene renderer = infrastructure).
- **P5 — Mask + displace atoms.** The three modulators from §3. Gate: value-level —
  bounds mask on known positions yields exact 0/1 channel values; color mask
  tolerance=0 selects only exact-color splats; displace at amount 0 is
  byte-identical passthrough; displace mask_weight=0 ignores mask. PNG: masked
  emission on a two-color fixture lights only the target region. Scope: focused.
- **P6 — Starter preset + catalog.** Bundled generator preset "Splat Scene"
  (`splat_source` → color mask → displace → `render_splats` + `orbit_camera`), path
  via Browse stringBindings (image_folder convention); NODE_CATALOG regen; perf-HUD
  splat line. Gate: `check_presets` clean; headless PNG of the preset on the
  Peter-hands scan; **this phase runs the wave's single full workspace sweep**
  (per DESIGN_DOC_STANDARD §5 batching).

## 7. Decided — do not reopen

1. Splat = 64B canonical channel struct with `mask` inside (D1/D2); no new port kind.
2. Doors: `.ply` + `.splat`, hand-written parsers, parse-time sigmoid/exp/SH-DC (D3).
3. DC color only; SH bands dropped loudly, deferred behind a trigger (D4).
4. Splats displace from rest, never integrate; no velocity channel, ever (D5).
5. Sorted instanced quads via render pipeline; GPU radix sort every frame;
   camera-dependent sort stays renderer-internal (D6/D7).
6. Sort = shared `gpu_sort.rs` in manifold-renderer; `manifold-gpu` API untouched (D8).
7. Scene composition = depth-input wire; `render_scene` grows only its promised lazy
   depth output (D9).
8. Placement/opacity/scale = port-shadowed renderer params (D10); mask consumers v1 =
   exactly `mask_weight` + `mask_emission` (D11); everything else via wgsl_compute.

## 8. Deferred (with triggers)

- **`.spz` / SOGS compressed formats** — when Peter's actual scan pipeline emits
  them or a scan exceeds `.ply` practicality (>4M splats). ⚠ VERIFY-AT-IMPL: check
  the spec versions current at build time (web check) — the compressed-format race
  was not settled as of 2026-07.
- **SH band 1** (view-dependent tint, +9 floats) — if a side-by-side against a
  reference viewer on Peter's real scans shows DC visibly flat. Never SH2/3.
- **Tile-based compute rasterizer** — when a real show scene misses frame budget on
  the quad path at the resolution Peter performs at (measured, not assumed). D8's
  sort is already its building block.
- **Splats→particles bridge** (`node.spawn_from_splats`, positions+colors seed the
  existing particle stack) — when a scan needs ballistic physics, not displacement;
  shape it exactly like `node.spawn_from_mesh` (REALTIME_3D §9).
- **Mesh albedo-segmentation at import** (direction 1 of the 2026-07-04 brainstorm)
  — deliberately NOT folded in: `MeshVertex` has no color channel, and Exact
  matching makes widening it a corpus-wide migration; the right shape there is
  probably a baked mask *texture* riding the import path, which is IMPORT's
  domain. Trigger: Peter wants the emission/displacement-by-color move on an
  imported *mesh* specifically. The splat mask atoms ship the same stage concept
  natively first.
- **Splat editing/cleanup UI** (lasso delete, floater removal) — scan-prep is the
  scan app's job; `mask_splats_by_bounds` covers the crop-the-floor case. Revisit
  on real friction.
