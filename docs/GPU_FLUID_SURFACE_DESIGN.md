# GPU Fluid Surface — live liquid mesh from particle frames on the GPU

<!-- index: Moves FLIP surface reconstruction to GPU atoms (anisotropic level set + marching cubes) and interpolates a slower solver tick to 60 fps through a producer-agnostic particle-frame seam. -->

**Status:** BUILDING · P1, P2, P5, P6, P6b, P6c built; P6d measured, no lever kept; P6e (distance level set) building. The surface meets its re-baselined 6 ms gate (5.3 ms p95 at res 64 ×2); blobs and volume at 4 ms stay a kernel design item (BUG-l24y (GPU liquid surface kernels cost), section 9 P6d). P3 deferred, P4 dropped, P7–P8 not built.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) before starting any phase.
**Superseded in part (2026-09-29):** live water is GPU MLS-MPM per [GPU_MPM_SOLVER_DESIGN.md](GPU_MPM_SOLVER_DESIGN.md); D1's live-FLIP clause, D3, D9 and P4 no longer apply to live. The seam, atoms and interpolation stand.

On stage today the water is a CPU instrument that cannot keep time. At resolution 64 the
FLIP worker spends about 245 ms per 1/60 s tick; 35 ms of that is the surface mesher at
Surface Detail 0, and remeshing at Detail 1 or 2 costs 347 or 800 ms on its own
(measured single runs, 90 ticks, 2026-09-29). This design takes the two stages that do
not have to live on that worker and moves them. **Surface reconstruction becomes a
chain of GPU atoms that turns a particle frame into an ordinary triangle mesh; the
solver may tick at 15–30 Hz while a GPU pass interpolates particles to every display
frame.** The mesh feeds the existing scene, material, RT and volume-optics path exactly
as the CPU mesh does now.

What it does for the show: at a resolution the CPU solver can hold in real time (P1
measures which), the surface looks like Detail 2 or better and moves at 60 fps. It does
not make resolution 64 live — the solver alone stays several times over a 15 Hz budget
there (risk R1). The price is one solver tick of latency: a pour or a hit shows up one
tick later than today, and event quantization grows from 16.7 ms to the solver tick
(33 ms at 30 Hz).

D1–D6 were decided by Peter and the lead on 2026-09-29 and are restated here, not
reopened. D7 onward are this design's calls.

Companions: [WATER_SIMULATION_DESIGN.md](WATER_SIMULATION_DESIGN.md) (current CPU FLIP
contract; its historical GPU proposal is mined only for surface ideas);
[FLUID_ENGINE_INTEGRATION_PLAN.md](FLUID_ENGINE_INTEGRATION_PLAN.md) (worker, coupling,
takes, bake workflow); [DECOMPOSING_GENERATORS.md](DECOMPOSING_GENERATORS.md) and
[ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md) (atom rules, codegen mandate);
[MANIFOLD_GPU_ARCHITECTURE.md](MANIFOLD_GPU_ARCHITECTURE.md) (residency, retirement,
temporary arrays).
Beads: BUG-vglg (CPU FLIP scene physics epic), whose child BUG-vglg.18 is the bake and
cache workflow; BUG-3sta (fluid surfacing: remesh baked motion, tune detached droplets)
is the slot-9 work this design builds on.

## 1. Audit — what exists (verified 2026-09-29 at `b88e4c9cf`)

Paths abbreviated after first use: `R/` = `crates/manifold-renderer/src/node_graph/`,
`F/` = `crates/manifold-fluids/`. Extend, don't redesign.

| Piece | Anchor | State |
|---|---|---|
| Solver worker, fixed 60 Hz | `R/fluid.rs:44` (`TICK`), `:51` (`BATCH = 4`), `:1135` (`advance`), `:1061` (`accept`) | Exists. Live uses `try_recv` (`:1160`); offline blocks on `recv` (`:1153-1157`). Replies carry only the last tick of a batch (`R/fluid/native.rs:175-178`). |
| CPU surface capture per tick | `R/fluid/native.rs:166` (`capture_output`), `:190-201` (SurfaceVertex → MeshVertex: uv from domain xz, white colour) | Exists. The GPU mesh writes the same vertex attributes. |
| Mesh upload | `R/fluid_mesh_upload.rs:15` (50 vertices per dispatch, inline bytes), `:95-117` (zeroed tail) | Exists. The zeroed-tail contract is reused; inline chunking is unusable for particle frames (about 5,000 dispatches per tick at 630k particles). |
| Provided, growable output | `R/primitives/fluid_surface.rs:202-206` (`provides_array_output("vertices")`), `R/execution/array_growth.rs:6`, `R/resource_allocation.rs:21` | Exists. Downstream arrays re-derive capacity when a provided array grows. The particle frame rides this. |
| Solver-independent capture (slot-9) | `F/src/surface.rs:52` (`capture_surface_frame`), `F/native/flip_engine/surfaceframe.h:11` (particles + prepared solid SDF), `3cae04429` (`set_surface_reconstruction_enabled`) | On `codex/fluid-surfacing`, not main. Positions only — no velocity, no identity. Its SDF preparation is this design's input. |
| Isolated-particle radius (slot-9) | `e2a1a3834`, `F/native/flip_engine/surfaceframe.cpp:8-44` | CPU prototype: full radius when a neighbour is within 2r, smoothstep to `isolated_scale` by 3r. Becomes a GPU rule (D14). |
| Upstream mesher | `F/native/flip_engine/particlemesher.cpp` (`_scalarFieldProducerThread`: `dist = length(g - p) - r`, min-reduced) | Union of spheres plus Laplacian mesh smoothing. Not Yu & Turk; live and baked surfaces will differ (R5). |
| Upstream solid rule | `F/native/flip_engine/scalarfield.cpp:439-447` (solid vertices clamped to threshold), `polygonizer3d.cpp:472` (interp limited to the solid boundary) | Exists. The GPU level set matches the clamp (D15). |
| Particle attributes | `F/native/flip_engine/particlesystem.h:92` (`addAttributeULongLong`), `particlesystem.cpp:88` (`removeParticles` compacts every attribute list together) | Exists. A MANIFOLD id attribute survives removal. Upstream's own particle ID is a random `uint16` (`fluidsimulation.h:2365`) — not unique, unusable. |
| Raw particle getters | `F/native/flip_engine/fluidsimulation.h:1533-1534` (position and velocity `DataRange`) | Exists. Writes into caller memory without allocating. |
| Solver dt bound | `F/src/lib.rs:685` (`dt ≤ 1/30`) | Blocks a 15 Hz tick. P1 lifts it. |
| Coupled rigid/liquid tick | `R/fluid/coupled/native.rs:316` (rigid tick must equal `TICK`), `R/primitives/physics_world.rs:556-568`, `:653-656`, `R/execution.rs:2215` | Exists and stays (lockstep). Section 7 states the rule. |
| Atomic scatter atoms | `R/primitives/scatter_particles_3d.rs:95-98` (`Boundary`/`Blocked`, `atomic_outputs`) | Precedent for atomic atoms on standalone codegen. |
| Multi-pass scan atoms | `R/primitives/spawn_from_mesh.rs:119` (`BarrieredReduction`), `R/primitives/shaders/spawn_from_mesh.wgsl:134` (single-thread serial scan) | Precedent for the exemption. No parallel prefix scan exists (`rg -n -i 'prefix|scan_main'`). |
| Buffer generator with gathered inputs | `R/primitives/triangulate_grid.rs:72-74` (`node.make_triangles`: Pointwise, `BufferGather`, capacity from params) | Precedent for per-output-element atoms whose inputs are all gathered. |
| Const tables in a body | `R/primitives/shaders/generate_cube_mesh_body.wgsl` | Precedent for marching-cubes tables in a `wgsl_body`. |
| 3D particle family | `node.draw_particles_3d`, `node.resolve_scatter_3d`, `node.blur_3d`, `node.sample_volume_at_particles`, `node.move_particles_3d`, `node.keep_in_box_3d`, `node.array_feedback` | Exists, all on the 64-byte `Particle` in normalized `[0,1]³` space with toroidal wrap. Wrong record and space for scene-metre FLIP particles. |
| Marching cubes / isosurface | `rg -n -i 'marching|isosurface|polygoniz' crates/manifold-renderer/src crates/manifold-gpu/src` | None. |
| Texture3D sizing | `R/effect_node.rs:1773` (`texture_3d_output_dims`), `R/graph_loader.rs:1830` | Load-time only, from params or input dims. No CPU-to-3D-texture upload in `manifold-gpu` (`device.rs:600` and `encoder.rs:2374` are 2D). |
| CPU-mapped ring reuse | `crates/manifold-gpu/src/metal/frame_fence.rs:59` (`is_completed`), `crates/manifold-renderer/src/clip_thumb_gpu.rs:258` | Exists for UI rings. Content-thread exposure unverified (P2 entry). |
| Bounded GPU timing proof | `crates/manifold-renderer/tests/gpu_proofs/rt_dynamic_perf.rs:31-33`, `crates/manifold-renderer/Cargo.toml:161` (`rt-perf-proofs`) | Exists. The surface budget proof copies it. |
| Headless capture | `crates/manifold-renderer/examples/fluid_capture.rs` | Exists. Produces the L2 artifacts. |
| Add Fluid authoring | `crates/manifold-editing/src/commands/graph/scene/fluid.rs:40` (`AddSceneFluidCommand`), `:450` (fluid `vertices` → object) | Exists. P7 changes what it wires. |
| Live-only rules | `R/fluid.rs:856-861` (roles and fields reject Record/Playback), `:829-830` (coupling) | Exists. GPU-surface graphs follow the same pattern (D12). |

Negative claims were checked with the searches named in the table.

## 2. Decisions

**D1 — Live water is FLIP plus a real triangle mesh (Peter + lead).** The mesh goes
through the existing scene, material, RT and volume-optics contract. Screen-space fluid
rendering is vetoed, not deferred.

**D2 — Surface reconstruction moves to GPU atoms (Peter + lead).** Particles, per-particle
radius and the collision SDF go to GPU buffers. A level set is built at 2–4× the sim
grid with Yu & Turk 2010 anisotropic kernels; marching cubes runs in compute; the output
is the same `Array<MeshVertex>` triangle list the CPU path produces. The CPU mesher stays
for Record and bake. Rejected: porting upstream's sphere-union mesher to the GPU — it
keeps the blobby look slot-9 is fighting and still needs mesh smoothing, which a triangle
soup cannot do. Rejected: the CPU mesher on a second worker — Detail 1/2 costs 347/800 ms
per tick, not live at any tick rate.

**D3 — Solver tick drops to 15–30 Hz with GPU interpolation (Peter + lead).** Particle
frames are double-buffered on the GPU; a compute pass makes a cubic-Hermite particle set
(positions and velocities at both ticks, clamped against the collider SDF) every display
frame. Interpolation only, never extrapolation. One solver tick of latency is accepted.

**D4 — The seam is a particle frame (Peter + lead).** Positions, velocities, ids, radius,
count and tick stamp in GPU buffers, plus the collision SDF. Today the CPU engine fills
it; a future GPU solver writes it directly and nothing downstream changes. Section 3
pins it.

**D5 — Record/Playback stay mesh caches for now (Peter + lead).** Caching particle frames
instead is deferred with a trigger (section 11).

**D6 — Uncoupled Box3D keeps its own 60 Hz tick and never waits on the fluid worker;
coupled Box3D stays in lockstep with the fluid tick (Peter's call, relayed by the lead,
2026-09-29).** A coupled pair steps on the fluid worker as built and presents at display
time `s` through the same interpolation as the particles (D10). The fluid tick of any
coupled scene has a 30 Hz floor. Section 7 states the rule and its costs.

**D7 — No separate upload atom; `node.fluid_surface` publishes the frame as provided
outputs.** The worker writes particle records straight into a shared GPU buffer slot
handed to it through the existing request channel. Precedent: the provided `vertices`
output (`R/primitives/fluid_surface.rs:202-206`). Rejected: a CPU `ParticleFrame` wire
plus an upload atom — a second 20 MB copy per tick on the content thread, and a GPU
solver would never emit that CPU wire, so the downstream graph would change when the
solver does. That breaks D4.

**D8 — Volumes are flat `Array(f32)` storage buffers; their lattice rides on wires.**
Index `i + nx·(j + ny·k)`; lattice bounds on a `Transform` wire (centre, full size);
node counts on three `ScalarF32` wires. Precedent: the flat accumulator of
`node.draw_particles_3d`. Rejected: `Texture3D` — its dims are fixed at graph load
(`R/effect_node.rs:1773`), but the fluid lattice depends on a domain `Transform` that may
be wired, and `manifold-gpu` has no CPU-to-3D-texture upload. Making `Texture3D`
runtime-sized is an executor and backend project this design does not need.
**Consequences, stated honestly:** no hardware trilinear filtering (bodies do eight
loads), and the level set cannot feed `node.blur_3d` or `node.slice_volume` without a
resolve atom (deferred).

**D9 — Solver rate is a setup setting, default 60 Hz for existing content.**
`solver_rate` ∈ {60, 30, 20, 15} Hz on `node.fluid_surface`; changing it restarts the
world. A rate below 60 requires Live mode, particle outputs consumed, `vertices`
unconsumed and, until P8, whitewater off. A coupled scene may use 60 or 30 Hz, never
lower (D6). Anything else is a named error,
never a silent 60. Add Fluid authors 30 Hz from P7. **Defaulted, with a kill trigger:**
P1 measures ms per simulated second at res 32/48/64 × 60/30/15 Hz. If 30 Hz is not at
least 25% cheaper than 60 Hz at res 48, stop and escalate to Peter before P4 — the
latency and quantization costs would buy nothing. Native `max_substeps` scales by
`60 / rate`, so the per-substep bound, and the numerical regime, match today's 60 Hz × 6.
**Consequence:** the substep count per simulated second does not drop; the saving is
per-tick overhead only. That is why P1 measures before anything is built on it.

**D10 — Display time is one tick behind and drives every solver-time output.** Live and
offline alike: `s = target_time − tick`. With A and B the two newest accepted frames,
`blend = clamp((s − t_A)/(t_B − t_A), 0, 1)` and `span = t_B − t_A`. When the worker
falls behind, `blend` holds at 1 and lag reports as today; while it catches up in
batches, `span` covers several ticks. The same `s` presents the particle frames,
`obstacle_pose` and the coupled rigid frame, so a paddle and the water it pushes stay in
contact. Rejected: live at `target − tick` but export at `target` — Peter's surfacing
rule (BUG-3sta) is that preview and export use the same path; the export picture must be
the rehearsal picture. **Consequence:** fluid timing in export also lags the transport
by one tick. CPU-mesh graphs keep today's behaviour and present tick B.

**D11 — Interpolation runs over B's particles and matches them in A by id.**
Frames are sorted by strictly increasing id within an identity epoch. One output per B
record: binary-search A; found → cubic Hermite with tangents `span·v_A` and `span·v_B`;
not found (a birth) → `x_B − τ·v_B + ½·a·τ²` with `τ = (1 − blend)·span`, radius scaled
by `blend`, so a birth grows in from nothing. Display time never passes `t_B`. `a` is a
port-shadowed param, zero for liquid, gravity for spray. Rejected: rewinding every
particle from B without ids — it pops by `½·a·dt²` at every tick boundary (5 mm under
gravity at 30 Hz, far more in a splash). Rejected: extrapolating past B to hide the
latency — overshoot through walls, and D3 forbids it. Rejected: also emitting A-only
particles fading out — the output count becomes `count_a + count_b`, which the fusion
capacity algebra cannot express (`CapacityExpr` has `Min`, `Mul`, `Slot`; no sum), and
the atom would stop fusing with the clamp. **Consequence:** a particle removed during a
tick (drains, out-of-domain, outlier removal) vanishes at the tick boundary. Drains are
where liquid disappears anyway; if popping shows elsewhere, section 11 carries the fix.

**D12 — GPU-surface graphs are Live-only in V1.** `node.fluid_surface` rejects Record
and Playback when any particle output is consumed, with the same message shape as roles
and fields (`R/fluid.rs:856-861`). Legacy presets keep the CPU mesh and their caches
unchanged. Every Add Fluid scene is already Live-only today (mesh roles), so P7 loses no
working bake path. **Dissent recorded for the lead:** D2 keeps the CPU mesher for Record;
that holds for CPU-mesh graphs. A GPU-surface graph cannot play a mesh cache back without
a graph-level mesh switch by cache mode, which is a second rendering path per liquid.
The real fix is D5's deferred particle-frame cache, whose trigger is the next item on
Peter's list (BUG-vglg.18).

**D13 — The node skips CPU meshing when `vertices` is unconsumed.** It calls
`set_surface_reconstruction_enabled(false)` before the first step. The CPU surface
params keep affecting `vertices` in every state; an unwired output is authoring, not
dead state. Record always meshes. Rejected: a `surface_mode` enum — it makes the CPU
surface params dead in GPU mode.

**D14 — Frame radius is physics; surface shape is a live GPU param.** The frame carries
the native marker radius. Particle scale, stretch, centre smoothing and the isolated
droplet scale live on `node.shape_particle_blobs` and change the surface next frame
without restarting the simulation. The isolated shrink uses slot-9's rule (full radius
when a neighbour is within 2r, smoothstep to `isolated_scale` by 3r), computed from the
same neighbour search as the anisotropy. Yu & Turk's sparse-neighbour rule (isotropic
kernel below N_ε neighbours) also applies.

**D15 — The level set clamps at solids the way upstream does and closes at the lattice
border.** A lattice node whose solid distance is negative is capped at the threshold:
never inside the liquid, so the surface wraps the solid (`scalarfield.cpp:439-447`; with
negative-inside values, `φ = max(φ, 0)`). Border nodes are forced outside so marching
cubes emits a closed, consistently wound surface. The volume-optics path needs a closed mesh
(`volume_geometry` in the current contract of WATER_SIMULATION_DESIGN.md).

**D16 — Marching cubes is three atoms: count, running total, emit per output vertex.**
Count writes triangles per cell; running total is an inclusive scan with a one-frame-late
CPU total; emit runs one thread per output vertex, binary-searches the scan for its cell
and computes the vertex and a gradient normal. Threads past the total write zero
vertices — the zeroed-tail contract of `R/fluid_mesh_upload.rs:95-117` with no clear
pass. If the total exceeds capacity, emit writes an empty mesh and the next frame
reports the error. Rejected: one thread per active cell writing up to 15 vertices — a
scatter write the codegen cannot express and the per-element scope test rejects.

**D17 — Binning is one atom.** `node.sort_particles_into_cells` is atomic count → scan →
atomic scatter → per-bin stabilise (D21), declared `BarrieredReduction` (precedent
`spawn_from_mesh.rs:119`). None of its dispatches is barrier-free, none has another consumer, and the scan kernel
is one Rust module shared with `node.running_total`. Rejected: three graph nodes for one
counting sort — the graph gains nothing from seeing them.

**D18 — The level set is a per-node gather, not an atomic splat.** Since P6e it is the
distance to the nearest blob ellipsoid, not a kernel sum. Rejected: scattering
each kernel's footprint with fixed-point `atomicAdd` like `node.draw_particles_3d` — it
quantizes a smooth field, contends on dense interiors, and cannot fuse. The gather reuses
the bins the anisotropy pass already needs.

**D19 — Frame slots move through the existing channel; reuse is fence-gated; growth is
re-capture.** A ring of shared buffers (default 4) lives on the content thread. A slot is
handed to the worker only when `FrameFence::is_completed` says the last display frame
that read it has retired — a non-blocking check. No free slot means no request this
frame. If a frame outgrows its slot, the worker replies with the required counts and
keeps its state; the content thread grows free slots and requests a capture-only reply
for the same tick. Rejected: `Arc<Mutex<ParticleFrame>>` between threads (hard rule);
GPU allocation on the worker (hands the device to another thread for a rare case); a
content-thread copy (20 MB per tick).

**D20 — Surface budget ≤ 6 ms (lead, re-baselined 2026-09-30 from an unmeasured 3 ms), measured, not argued.** GPU time from the
interpolation dispatch through emit, 64³ sim grid meshed at 2× (135 lattice nodes per
axis on the padded native grid), 1080p scene, M4 Max, p95 over 120 frames after 16
warm-up frames. The proof lives in P6.

**D21 — The surface chain is deterministic (lead, 2026-09-30).** Bakes and
bit-reproducible export need the same input to give the same bytes on every run. The
sort's atomic ranks vary run to run, so an always-on pass sorts each bin's slots by
input index before `sorted` and `order` are written. Every float sum downstream then
adds in a fixed order. Cost: the sort goes from 0.20 to 0.51 ms p95 at res 64 ×2, and
the surface stays inside D20. Proof: `fluid_sort_particles_into_cells_is_deterministic`
(three runs byte-identical, bins in input order, crowded and sparse bins). Rejected: an
opt-in switch, because determinism is an invariant, not a mode.

## 3. The particle-frame contract

### 3.1 Records

`crates/manifold-fluids/src/particles.rs` (new), no new dependency:

```rust
/// Layout shared with the renderer's `FluidParticle`; size and offsets are asserted there.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ParticleRecord {
    /// Scene-space metres; w = physical radius in metres. w = 0 marks an unused slot.
    pub position_radius: [f32; 4],
    /// Scene-space metres per second.
    pub velocity: [f32; 3],
    /// Birth order within `ParticleFrameInfo::identity_epoch`. 0 = no identity.
    pub id: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParticleFrameInfo {
    pub count: u32,
    /// Changes when live ids are renumbered; frames from different epochs never match.
    pub identity_epoch: u32,
    /// Solid-distance node lattice written by this capture.
    pub solid_nodes: [u32; 3],
}

#[derive(Debug)]
pub enum CaptureError {
    /// Caller memory too small; nothing was published. Retry the same tick.
    Capacity { particles: u32, solid: usize },
    Fluid(FluidError),
}

impl FluidWorld {
    /// After a completed step. Writes records (id 0, native order), positions offset by
    /// `offset`, and the prepared solid distances. Never advances time; no allocation
    /// once scratch is warm.
    pub fn capture_particle_frame(
        &mut self,
        offset: [f32; 3],
        particles: &mut [ParticleRecord],
        solid: &mut [f32],
    ) -> Result<ParticleFrameInfo, CaptureError>;
}
```

**Identity.** A producer with identity writes records sorted by strictly increasing id
within an epoch; when its next id would pass `u32::MAX` it renumbers live particles 1..n
in current order and bumps `identity_epoch`. A producer without identity writes id 0 on
every record and epoch 0; consumers that match by id treat every record as a birth. The
MLS-MPM solver has identity (its D9). The FLIP capture below does not: it is a test feed
for this surface, and the `manifold_id` attribute is not built (P1, dropped items).

**FLIP capture (built).** The bridge fills records in native storage order from the
`DataRange` position and velocity getters through a fixed 4,096-particle scratch, so a
capture allocates nothing once warm. The solid distances are the SDF `captureSurfaceFrame`
prepares (obstacle meshing offset and domain boundary applied), produced by the
MANIFOLD method `FluidSimulation::captureParticleFrameSolid` into a scratch
`MeshLevelSet` reused across captures; a meshing volume is rejected because it would
filter particles already written. Positions apply upstream's `domainScale`/`domainOffset`
through the getter, then MANIFOLD's native-origin offset (`R/fluid/domain.rs:103-110`),
passed in as `offset`. Radius and velocity are scaled by `domainScale` (1 in MANIFOLD).

`crates/manifold-renderer/src/node_graph/fluid_particles.rs` (new):

```rust
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FluidParticle {
    pub position_radius: [f32; 4],
    pub velocity: [f32; 3],
    pub id: u32,
}
// KnownItem specs: position_radius Vec4F, velocity Vec3F, id U32 (vec3 + u32 packs to 16
// bytes, as Particle's velocity/life does). Compile-time asserts: size 32, and every field
// offset equals ParticleRecord's (core::mem::offset_of!).

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FluidBlob {
    /// Smoothed centre xyz; w = support radius in metres, 0 = inactive.
    pub center_radius: [f32; 4],
    /// Symmetric shape matrix G: xx, yy, zz; w = det(G).
    pub shape_diag: [f32; 4],
    /// G: xy, xz, yz; w = 0.
    pub shape_off: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CellRange { pub start: u32, pub count: u32 }
```

Volumes are `Array(f32)` (D8); triangle counts and scans are `Array(u32)`.

### 3.2 Outputs added to `node.fluid_surface`

| Port | Type | Meaning |
|---|---|---|
| `particles_a`, `particles_b` | `Array(FluidParticle)`, provided, growable | The two newest accepted frames. With one frame, both ports publish it and `blend` is 1. |
| `count_a`, `count_b` | `ScalarF32` | Live record counts. |
| `identity_a`, `identity_b` | `ScalarF32` | Identity epochs (integers carried in floats). |
| `solid_a`, `solid_b` | `Array(f32)`, provided | Solid distance lattices of frames A and B. |
| `grid_bounds` | `Transform` | Scene AABB of the solid lattice (the padded native grid). Every volume in the chain spans exactly this box. |
| `grid_nodes_x`, `grid_nodes_y`, `grid_nodes_z` | `ScalarF32` | Solid lattice node counts. |
| `blend`, `span` | `ScalarF32` | D10. |

Any of the four arrays wired switches the node into publishing particle frames; an
unwired output has no plan resource, which the node reads through `ctx.outputs.slot`.
The arrays are provided storage with a capacity hint of one record; downstream
capacity re-derives as the ring grows. `obstacle_pose` and the coupled rigid frame
present tick B until P3 builds interpolation (P2, deviation).

Lattice convention, stated once: node (i, j, k) sits at
`bounds.min + (i, j, k) · size / (nodes − 1)`. A volume at scale `m` has
`(nodes − 1)·m + 1` nodes per axis over the same bounds.

### 3.3 Ownership and threads

The content thread owns the ring, the accepted A/B pair, `blend`, and all publication.
The worker owns the native world and, while a request is in flight, one ring slot moved
to it inside `Request`; it writes the slot through `GpuBuffer::mapped_ptr` and moves it
back in `Reply`. `GpuBuffer` is `Send + Sync`
(`crates/manifold-gpu/src/metal/types.rs:204-205`). No new thread, channel,
`Arc<Mutex>` or `Arc<RwLock>`.

A GPU solver implements the same outputs by writing the arrays on the GPU. It owns its
own ring and keeps the `blend`/`span` semantics; downstream atoms do not change.

## 4. The atom chain

```text
fluid_surface ─ particles_a/b, counts, identities, blend, span ─► interpolate_particle_frames ─► push_out_of_solid ─┐
      │         solid_a, solid_b, blend ─► mix_arrays ─► solid_now ───────────────────────────────┘ (gather)       │
      │                                                                                                            ▼
      │                                                                  sort_particles_into_cells ─► sorted, cell_ranges
      │                                                                                                            │
      │                                                                   shape_particle_blobs ◄───────────────────┘
      │                                                                             │ blobs
      └─ grid_bounds, grid_nodes_x/y/z ─────────────────────────► particle_volume ◄─┘ + solid_now ─► level set (Array f32)
                                                                          │
                                        count_surface_triangles ◄─────────┘ ─► running_total ─► volume_surface_mesh
                                                                                                        │
                                                                           scene_object.vertices ◄──────┘ (existing)
```

Section 2.5 audit, per DECOMPOSING_GENERATORS.md section 2.5 (audit by analogy).
Survey: `rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/ -g '*.rs'`.
Reference presets read end to end: `WaterBasin.json` (fluid → scene object, obstacle
pose wiring), `WaterDamBreak.json` (whitewater instances and counts), `FluidSim3D.json`
(particles → flat 3D accumulator → volume → field sampling).

| Candidate | Finding | Shape and argument |
|---|---|---|
| Particle-frame upload | **One wire away** | Provided outputs on `node.fluid_surface` (D7). CPU data enters the GPU at the FFI node that already owns the worker; a separate atom needs a CPU wire and a second copy. |
| Per-particle radius | **One wire away** | A channel of `FluidParticle`. |
| Interpolation | **New** — `node.interpolate_particle_frames` | Pointwise buffer atom, one output per `particles_b` slot (`FromInput { input: "particles_b" }`); slots at or past `count_b` write radius 0. `particles_a` is `BufferGather` (binary search over `count_a`). Inputs: `particles_a` (optional; unwired means move-from-B only, used by whitewater), `particles_b`, the counts, identities, `blend`, `span`, and `acceleration_x/y/z` (port-shadowed). No existing atom interpolates keyed records. |
| SDF clamp | **New** — `node.push_out_of_solid` | Pointwise buffer atom over its particle input; `solid` is `BufferGather` (manual trilinear and central-difference gradient) with `bounds` and `nodes_x/y/z` inputs; moves `x` out along the gradient where `φ < 0`. `node.keep_in_box_3d` keeps `Particle` inside analytic containers in `[0,1]³`; extending it would change its record, its space and its meaning. Reusable by any particle system near a sampled solid. |
| Display-time solid | **New** — `node.mix_arrays` | `a + (b − a)·amount` over two `Array(f32)` of equal capacity; `MultiInputCoincident`. `node.array_math` has Mix, but it is CPU-only on the content thread by design, and a per-frame CPU write of a GPU-read array races in-flight frames. This is its fusable GPU sibling. |
| Spatial binning | **New** — `node.sort_particles_into_cells` | D17. Outputs `sorted: Array(FluidParticle)` and `cell_ranges: Array(CellRange)`; `cell_size` param (metres, port-shadowed); bins cover `grid_bounds`. `node.draw_particles_3d` is nearest-voxel energy in wrapped unit space — not a sort. |
| Anisotropic kernels | **New** — `node.shape_particle_blobs` | Pointwise over sorted particles; `sorted` and `cell_ranges` are `BufferGather`. Yu & Turk weighted mean, covariance, eigen-decomposition with stretch clamp, isotropic fallback below N_ε, centre smoothing, D14's isolated radius. Support is clamped to the bin `cell_size` input — one home for the search radius. Params: `particle_scale`, `stretch`, `smoothing`, `isolated_scale`, `min_neighbours`. |
| Kernels → level set | **New** — `node.particle_volume` (a distance field since P6e) | One thread per lattice node; `blobs`, `cell_ranges` and `solid` are `BufferGather`. Writes the capped distance to the nearest blob ellipsoid (negative inside, P6e) and applies D15. Param `resolution_scale` ∈ {2, 3, 4}; outputs `nodes_x/y/z` for the rest of the chain. Capacity is the solid's capacity × `resolution_scale`³, an upper bound on `((n − 1)·m + 1)³`, so it grows with the provided solid array. Shape precedent `node.make_triangles`. |
| Level-set smoothing | **Exists, not usable** | `node.blur_3d` is `Texture3D`-only (D8). Yu & Turk kernels are already smooth; `smoothing` and `particle_scale` carry the look. The resolve atom is deferred. |
| MC classify | **New** — `node.count_surface_triangles` | One thread per cell; level set is `BufferGather`; writes the case table's triangle count. |
| Prefix scan | **New** — `node.running_total` | Inclusive multi-level scan of `Array(u32)`, `BarrieredReduction`. `total: ScalarF32` is the last element read back one frame late (the `color_sample` readback pattern). Shares its scan module with the sort. |
| Lattice box as scalars | **New, two users** — `node.transform_components` | CPU atom, the inverse of `node.transform_3d`. Buffer codegen binds params and arrays only, and a `Transform` wire into a GPU atom is a fusion cut, so the seam keeps `grid_bounds: Transform` and the surface and MLS-MPM atoms read its centre and size as scalars. |
| MC emit | **New** — `node.volume_surface_mesh` | D16. `capacity` param (vertices, multiple of 3); `scan` and level set are `BufferGather`; `total` input drives the error. Writes the attributes `R/fluid/native.rs:190-201` writes. |
| Mesh consumer | **Exists** | `node.scene_object.vertices` → `node.render_scene`, unchanged. |
| Whitewater to instances | **Built under GPU_MPM_SOLVER_DESIGN.md P1 (Water kernel, look gates and the cost probe)** — `node.particles_to_copies` | Pointwise `FluidParticle` → `InstanceTransform` (`pos_scale` = position, radius), with a `live_count` input that turns slots past the producer's count into holes. `node.copy_positions` goes the other way. |
| One `gpu_fluid_mesher` node | **Forbidden** | DECOMPOSING_GENERATORS.md section 1.1 (No fused single-effect or single-generator monoliths). |

Every atom that reads the lattice takes its box as centre and size scalars and its
`nodes_x/y/z: ScalarF32`; the lattice has no other home. Eleven new atoms is the
honest count. The graph ships as one node group, "Liquid Surface", per
GROUPING_GRAPHS.md, so presets and Add Fluid insert one box.

### 4.1 Codegen classification

| Atom | Class | Proof |
|---|---|---|
| `interpolate_particle_frames`, `push_out_of_solid`, `mix_arrays`, `shape_particle_blobs`, `particle_volume`, `count_surface_triangles`, `volume_surface_mesh`, `particles_to_copies` | Barrier-free per element: `wgsl_body` + `fusion_kind` + `input_access`, pipeline from `standalone_for_spec::<Self>()` | Value `gpu_tests` against CPU-computed expected output. Fused-vs-unfused proof for every adjacent pair `graph-tool fusion` places in one region; `interpolate_particle_frames → push_out_of_solid` is expected to (both are `FromInput` over the particle stream). The generators with parameter-derived capacity (`particle_volume`, `count_surface_triangles`, `volume_surface_mesh`) are expected to run standalone, as `node.make_triangles` does. |
| `sort_particles_into_cells` | Named exemption, exclusion 1 of the ADDING_PRIMITIVES.md scope test (barriered reduction / multi-pass scan): hand multi-entry kernels (count, scan levels, scatter), the `node.spawn_from_mesh` precedent. `standalone_for_boundary_spec` has no buffer variant, BUG-vdvg (buffer boundary spec gap). | Permutation and range value tests; Params reflected against the hand shader. |
| `running_total` | Named exemption, exclusions 1 (scan) and 3 (readback bridge); shares the sort's hand scan. | Values against CPU scans at sizes 1, 255, 256, 257, 2²⁰+3 and 2²⁴+3 (four levels); `total` lags exactly one frame. |

If the region builder refuses to fuse a declared-fusable atom (buffer generators with
only gathered inputs are the least-tested shape), that is the compiler's call and
`graph-tool fusion` records it. If `standalone_for_spec` cannot express an atom at all,
it is BLOCKED: file a `bd` bug naming the missing read-path and declare
`boundary_reason: Blocked` — never a quiet exemption.

## 5. Plausible-wrong turns, forbidden by name

- You will want one `gpu_fluid_mesher` node that runs interpolation through emit in one
  kernel. No — section 4.
- You will want to extrapolate from frame B to hide the latency. No — D3, D11.
- You will want to copy particles into a `Vec` on the worker and upload with
  `fluid_mesh_upload`'s inline chunks. No — D7, D19.
- You will want an `Arc<Mutex<…>>` holding "the latest frame" for both threads. No —
  slots move through `Request` and `Reply`.
- You will want `FrameFence::guard_slot` because it exists. No — it waits up to 50 ms.
  Use `is_completed` and skip the request.
- You will want to fall back to the CPU mesh when the GPU mesh overflows, or when
  Playback has no particles. No — empty mesh plus error (D16), explicit rejection (D12).
- You will want `Texture3D` for the volumes. No — D8.
- You will want to repurpose `Particle`'s padding for id and radius so the 3D particle
  atoms work. No — `FluidParticle` is its own record.
- You will want to route the surface params through the solver so they "match the CPU
  path". No — D14; they are live GPU params.

## 6. Cost and the instrument

Per display frame: one `mix_arrays`, one fused interpolate + push-out, the sort (count,
scan levels, scatter), blobs, volume, count, scan levels, emit — about fifteen dispatches.
The content thread only manages the ring and computes `blend`, the obstacle pose and the
coupled rigid pose. Per tick, the worker loses CPU meshing and gains one capture
(memcpy-class, about 20 MB at 630k particles).

**Consequences, stated honestly:**
- One solver tick of added latency on everything fluid, export included (D10).
- Events quantize to the solver tick: 33 ms at 30 Hz. The quantization rule is
  FLUID_ENGINE_INTEGRATION_PLAN.md section 5 (Timing, events and lifecycle).
- A deforming 60 fps mesh changes RT acceleration structures and volume-optics inputs
  every display frame instead of every published tick. P6 measures and reports this; it
  is outside the surface gate.
- Raster passes draw only live triangles; ray tracing builds over a CPU bound about
  2× live (P6b).
- Live (anisotropic GPU) and baked (sphere-union CPU) surfaces look different until
  particle frames are cached (R5).

## 7. Coupling rule

**Rule.** A scene without a fluid coupling runs Box3D on its own fixed tick inside
`node.physics_world` (`R/primitives/physics_world.rs:653` onward) and never reads the
fluid worker. Nothing in this design touches that path. A coupled pair stays in
lockstep with the fluid tick, as built: the rigid owner's tick equals the pair's solver
rate (P4 turns the `TICK` check at `R/fluid/coupled/native.rs:316` into that), which is
60 or 30 Hz, never lower. The coupled rigid frame keeps the previous accepted pair and
presents at `s` with the particles, interpolated by the same `blend` (lerp translation
and scale, slerp rotation). There are no fallback modes.

**Consequences, stated honestly:** at 30 Hz a coupled body's physics advances in 33 ms
steps and its motion between them is interpolated; impulses on it quantize to the fluid
tick; when the worker falls behind, the body holds with the water. Decoupling was
considered and is decided against (section 10).

**As built (read-through 2026-09-29).** A coupled scene has no independent Box3D tick. `set_coupled_physics(true)` drops the node's private simulation
(`physics_world.rs:556-568`); `run` only republishes the pair accepted with a liquid
reply (`physics_world.rs:653-656`, latched at `R/execution.rs:2215`); the worker steps
Box3D inside each 1/60 s liquid tick and rejects any other duration
(`R/fluid/coupled/native.rs:316`); publication requires matching epoch and tick
(`R/fluid.rs:1073-1095`). In Live the content thread does not block — `advance(false)`
uses `try_recv` (`R/fluid.rs:1160`) — but rigid bodies move only when a liquid reply
lands, jump up to `BATCH = 4` ticks per reply (`R/fluid.rs:51`) and freeze between
replies. Offline, `advance(true)` blocks on the worker (`R/fluid.rs:1153-1157`), as
expected. The integration plan chose this ("a coupled scene presents at its slowest
required solver's accepted time" —
FLUID_ENGINE_INTEGRATION_PLAN.md section 3 (Architecture and ownership)). This design
keeps it; display-time interpolation removes the visible stepping when the worker keeps
up.

## 8. Invariants and enforcement

| Invariant | Enforcement |
|---|---|
| Frames are id-sorted; ids stable across removal and unique within an identity epoch | `particle_frame_ids_sorted_through_inflow_and_drain` (manifold-fluids); `debug_assert!` on the worker before publishing |
| Capture never changes the simulation | `particle_frame_capture_leaves_solver_state_bit_identical` |
| Display time never passes the newest tick; `blend` ∈ [0, 1] | `fluid_display_time_never_passes_newest_tick`; GPU value test: `blend = 0` reproduces A's surviving particles and `blend = 1` reproduces B exactly |
| All solver-time outputs present at one `s` | `fluid_presentation_outputs_share_display_time` (particles, `obstacle_pose`, coupled rigid frame) — deferred with P3; until then every output presents tick B |
| Live never blocks on the worker; ring exhaustion skips a request | `fluid_particle_ring_exhaustion_never_blocks`; negative gate: `rg -n '\.recv\(\)' crates/manifold-renderer/src/node_graph/fluid.rs` shows only the offline branch |
| No new shared locks | Negative gate: `git diff origin/main -- crates \| rg '^\+.*Arc<(Mutex\|RwLock)'` returns nothing |
| Unsupported rate, mode and coupling combinations are named errors | `fluid_solver_rate_rejects_record_playback_engine_mesh_and_coupled_below_30`, `fluid_particle_outputs_reject_record_and_playback` |
| Mesh overflow is an empty mesh plus an error, never a truncated mesh | `volume_surface_mesh_overflow_writes_empty` (GPU), `fluid_surface_overflow_reports_error` |
| The emitted mesh is closed and consistently wound | `volume_surface_mesh_sphere_is_watertight` (weld by position; every edge shared twice; consistent orientation) |
| Interpolated particles stay out of solids | `fluid_push_out_penetration_bounded`: max `−φ` ≤ 0.1 · cell after push-out |
| Every new barrier-free atom is on codegen | The existing classify source scans plus each atom's value test; `graph-tool fusion` output recorded in P6 |
| Surface stage ≤ 6 ms (D20) | `fluid_surface_perf` (P6), gated on p95; it fails if the gated configuration overflows, so it never times an empty mesh |
| An unwired count means the whole array, at any size | `count` is a wire-only input on the sort and the running total (no numeric default); `fluid_running_total_matches_cpu_scan_and_total_lags_one_frame` at 2²⁴+3 |
| No lattice yet is silence, not an error | `fluid_sort_particles_into_cells_is_silent_before_the_first_frame`; volume, count and mesh skip on zero nodes |
| Lattices past 65,535 threadgroups are covered | `fluid_count_surface_triangles_reaches_cells_past_65535_threadgroups` |
| Every captured frame has a non-empty, finite mesh | `fluid_capture --gpu-surface` reads the mesh back each offline frame and fails on a non-finite vertex or an empty surface |
| Emit writes only live and last frame's vertices; slots past live stay zero | `fluid_volume_surface_mesh_writes_only_live_and_last_frame_vertices` |
| Raster passes draw only live triangles; ray tracing never passes the bound | `live_draw_args_are_whole_live_triangles_within_capacity`, `indirect_dispatch_and_draw_match_direct`, `object_wire_carries_the_mesh_live_extent` |
| Uncoupled Box3D advances while the fluid worker stalls | `physics_world_uncoupled_advances_while_fluid_worker_stalls` |

## 9. Phasing

Test scope for every phase: focused crate tests and clippy on touched crates. GPU phases
add `scripts/gpu_proofs_gate.py --filter fluid_ --filter water_` (cargo test, never
nextest). New renderer tests carry a `fluid_` prefix so that filter selects them.
Verify once, at the end of the phase.

**Build order after Peter's pivot (2026-09-29, relayed by the lead).** The live solver is
the GPU MLS-MPM of GPU_MPM_SOLVER_DESIGN.md, writing the particle-frame seam at 60 Hz.
FLIP stays the bake engine and becomes a test feed for this surface. So: P1 is cut to the
capture the test feed needs; P2 builds the seam as specified; P3 is deferred (section 11);
P4 is dropped; P5 and P6 build the surface. Every atom downstream of the seam reads
`particles_b` and `solid_b` directly until P3 exists.

### P1 — Native particle-frame capture (manifold-fluids)

**Built (2026-09-29).** `F/src/particles.rs` (`ParticleRecord`, `ParticleFrameInfo`,
`CaptureError`, `FluidWorld::capture_particle_frame`); bridge entry
`manifold_fluids_world_capture_particle_frame` (one call: counts, capacity check, solid,
records) and the test-only `manifold_fluids_surface_frame_solid`; MANIFOLD methods
`captureParticleFrameSolid` and `getMarkerParticleRadius` on `FluidSimulation`; a
PROVENANCE.md entry. Tests: `particle_frame_matches_marker_state`,
`particle_frame_capture_leaves_solver_state_bit_identical`,
`particle_frame_capacity_reports_required_counts`,
`particle_frame_solid_matches_surface_frame`, `particle_frame_requires_a_completed_step`.
**Dropped, with reasons:** the `manifold_id` attribute, renumbering, worker-side sort and
their two tests (FLIP is a test feed; ids exist only for interpolation, deferred with P3 —
revive them with P3 if FLIP must feed interpolation); the 1/15 dt bound,
`fluid_step_accepts_fifteen_hertz_ticks`, `tick_rate_probe` and the D9 kill check (they
existed for P4, dropped).

### P1 brief (as designed)

- **Entry state:** `git merge-base --is-ancestor cb8cc7a12 origin/main && git merge-base --is-ancestor 3cae04429 origin/main` passes; otherwise stop — the prerequisite is Peter's visual approval of slot-9. Re-run the anchors: `rg -n 'fn capture_surface_frame|fn set_surface_reconstruction_enabled' crates/manifold-fluids/src`, `rg -n 'addAttributeULongLong' crates/manifold-fluids/native/flip_engine/particlesystem.h`, `rg -n 'DataRange' crates/manifold-fluids/native/flip_engine/fluidsimulation.h`. List every native particle birth site in the phase notes before editing (`rg -n 'addParticle|push_back|_addMarker' crates/manifold-fluids/native/flip_engine/fluidsimulation.cpp`, then read each hit).
- **Read-back:** D4, D9, D11, D14; section 3.1; `F/native/PROVENANCE.md`. Restate: capture never advances or allocates; ids are bookkeeping, not numerics.
- **Deliverables:** `F/src/particles.rs` with section 3.1's types and `capture_particle_frame`; bridge functions that write into caller memory through the `DataRange` getters; the `manifold_id` attribute and renumbering; worker-side sort when native order is unsorted; a PROVENANCE.md entry; the dt bound at `F/src/lib.rs:685` raised to 1/15 with its message. Tests: `particle_frame_ids_sorted_through_inflow_and_drain`, `particle_frame_capture_leaves_solver_state_bit_identical`, `particle_frame_matches_marker_state`, `particle_frame_capacity_reports_required_counts`, `particle_frame_identity_epoch_renumbers_near_limit` (a test hook sets the counter near `u32::MAX`), `particle_frame_solid_matches_surface_frame`, `fluid_step_accepts_fifteen_hertz_ticks`. Measurement example `F/examples/tick_rate_probe.rs`: deferred meshing, 3 simulated seconds of a dam-break fill, res 32/48/64 × 60/30/15 Hz with `max_substeps` 6/12/24; prints ms per simulated second, substeps, capture ms.
- **Gate:** positive — `cargo nextest run -p manifold-fluids particle_frame fluid_step_accepts` green; `cargo clippy -p manifold-fluids -- -D warnings` clean; the probe table reported verbatim. Negative — `rg -n 'getMarkerParticle(Positions|Velocities)\(\)' crates/manifold-fluids/native/bridge.cpp` has no hit in the capture path.
- **Kill check:** D9's trigger runs on the probe table. If it fires, stop and escalate; P4 is not briefed.
- **Demo:** none — L1, plus the measurement table.
- **Forbidden:** touching solver numerics; using upstream's `uint16` particle ID; allocating per capture; a second scene-space conversion pass anywhere else.

### P2 — Frame publication and display clock (renderer)

**Built (2026-09-29).** Section 3.2's outputs on `node.fluid_surface`; `FluidParticle`
in `R/fluid_particles.rs` (layout asserted equal to `ParticleRecord`); the ring in
`R/fluid/particle_ring.rs` (four slots loaned inside `Request.outputs`, returned in
`Reply.outputs`); `blend`/`span` from `display_blend(target − tick, t_A, t_B)`;
deferred meshing (`set_outputs`, restart on a meshing change, Record always meshes);
the Record/Playback rejection. Entry-check resolutions:
- **Content frame fence.** No FrameFence exists on the content thread. The content
  pipeline's per-frame completion event already drives texture-pool recycling and
  drop retirement, so the ring reads that clock through a read-only
  `manifold_gpu::FrameClock` (`GpuDevice::frame_clock()`), not a second fence.
  Live checks `is_complete` and skips the request; offline waits on the oldest
  reader, an earlier committed frame.
- **Growth and late wiring.** One rule covers both: when the published tick is not
  the completed tick, the next request is capture-only for that tick. A too-small
  slot reports its counts, the ring grows free slots on the next prepare, and the
  capture-only reply publishes the same tick without republishing the mesh.
- **Unconsumed outputs** get no resource (`consumed_outputs` in `R/execution_plan.rs`);
  the node reads consumption from `ctx.outputs.slot`.
**Deviation, pending the lead:** `obstacle_pose` and the coupled rigid frame still present
tick B. With P3 deferred the surface reads `particles_b`, so presenting them at `s` would
put the paddle up to a tick behind the water it pushes. Presentation at `s` moves into
P3 with the interpolation atom; `fluid_presentation_outputs_share_display_time` goes
with it. Tests built: `fluid_display_time_never_passes_newest_tick`,
`fluid_engine_mesh_skipped_when_vertices_unconsumed`,
`fluid_particle_outputs_reject_record_and_playback`,
`physics_world_uncoupled_advances_while_fluid_worker_stalls`; GPU proofs
`fluid_particle_frame_reaches_gpu` (GPU copy of `particles_b` equals an independent P1
capture bit for bit), `fluid_particle_ring_exhaustion_never_blocks`,
`fluid_particle_ring_growth_recaptures_same_tick`. The `MANIFOLD_RENDER_TRACE` app gate
is the orchestrating session's.

- **Entry state:** P1 merged. Anchors: `rg -n 'fn capture_output|fn process' crates/manifold-renderer/src/node_graph/fluid/native.rs`, `rg -n 'fn accept|fn advance|const BATCH' crates/manifold-renderer/src/node_graph/fluid.rs`, `rg -n 'fn provides_array_output' crates/manifold-renderer/src/node_graph/primitives/fluid_surface.rs`. ⚠ VERIFY-AT-IMPL the content frame fence: `rg -n 'frame_fence|FrameFence|completed_frame' crates/manifold-renderer/src/gpu_encoder.rs crates/manifold-app/src/content_pipeline.rs`. If no counter is reachable from `EffectNodeContext`, add a read-only `GpuEncoder::frame_fence() -> &FrameFence` fed the way `clip_thumb_gpu.rs:258` is fed; any other shape is an escalation.
- **Read-back:** D7, D10, D12, D13, D19; sections 3.2–3.3; the forbidden list in section 5.
- **Deliverables:** section 3.2's outputs on `node.fluid_surface`; the ring in `FluidRuntime` (slots in `Request`/`Reply`, fence-gated reuse, growth by capture-only retry); A/B acceptance, `blend`, `span`; `obstacle_pose` and `CoupledRigidFrame` at `s` (lerp translation and scale, slerp rotation); deferred meshing when `vertices` is unconsumed; Record/Playback rejection when particle outputs are consumed. Tests: `fluid_display_time_never_passes_newest_tick`, `fluid_presentation_outputs_share_display_time`, `fluid_particle_ring_exhaustion_never_blocks`, `fluid_particle_ring_growth_recaptures_same_tick`, `fluid_particle_outputs_reject_record_and_playback`, `fluid_engine_mesh_skipped_when_vertices_unconsumed`, `physics_world_uncoupled_advances_while_fluid_worker_stalls`; GPU proof `fluid_particle_frame_reaches_gpu` (mapped readback equals the P1 capture).
- **Gate:** the tests and the GPU filter green; renderer clippy clean; section 8's two negative gates; content-thread gate — the orchestrating session runs the app with `MANIFOLD_RENDER_TRACE=1` on Water Dam Break for 60 s with no frame over 20 ms.
- **Demo:** none — L1. Nothing visible yet.
- **Forbidden:** `guard_slot`; any content-thread copy of particle data; changing CPU-mesh graph behaviour (they still present tick B).

### P3 — Interpolation atoms and the particle view (first pixels)

**DEFERRED (2026-09-29).** Trigger: a sub-60 Hz particle producer exists. At 60 Hz the
newest frame is at most one tick from display time, so the surface reads `particles_b`.
The `FluidParticle` records moved to P2 (they are the contract).
`node.particles_to_copies` left this phase: it is built under GPU_MPM_SOLVER_DESIGN.md P1
(Water kernel, look gates and the cost probe).

- **Entry state:** P2 merged; `rg -n 'particles_a' crates/manifold-renderer/src/node_graph/primitives/fluid_surface.rs` shows the ports.
- **Read-back:** D8, D11; sections 4 and 4.1; ADDING_PRIMITIVES.md whole.
- **Deliverables:** `fluid_particles.rs` records; atoms `interpolate_particle_frames`, `push_out_of_solid`, `mix_arrays`, each with value `gpu_tests`. Preset `WaterDamBreakParticles.json` ("Water — Dam Break (Particle View)"): the Dam Break scene with liquid particles drawn as copies of a small sphere. Tests `fluid_push_out_penetration_bounded` and `fluid_interpolated_motion_is_even`: over 60 display frames with frames arriving every other display frame, the coefficient of variation of mean per-frame particle displacement is below 0.25 (near 1 without interpolation).
- **Gate:** the tests above; `cargo run -p manifold-renderer --bin check-presets`; `cargo run -p manifold-renderer --bin graph-tool -- validate crates/manifold-renderer/assets/generator-presets/WaterDamBreakParticles.json --kind generator` and the same with `fusion`, output recorded in the phase report. Interpolate and push-out are expected in one region, with their fused-vs-unfused proof; if they are not, record the builder's reason and file a `bd` bug when it is a codegen gap.
- **Demo:** `cargo build --profile test --features gpu-proofs --example fluid_capture`, then run it with `--preset WaterDamBreakParticles --frames 120 --stills-every 10` into `/tmp/manifold_particle_view`. L2: Peter looks at the stills.
- **Gesture:** pause and resume transport mid-splash; the particles hold exactly and resume without a jump.
- **Forbidden:** extrapolation; reusing `Particle`; a fallback when A is missing (A unwired is the move-from-B path by design).

### P4 — Solver rate (seam brief)

**DROPPED (2026-09-29).** The live solver is MLS-MPM at 60 Hz; FLIP keeps its fixed 60 Hz
tick as the bake engine. The brief below is kept only as the record of what was designed.

- **Entry state:** P1's kill check did not fire; P3 merged. Re-derive the inventory: `rg -c '\bTICK\b' crates/manifold-renderer/src/node_graph/fluid.rs crates/manifold-renderer/src/node_graph/fluid crates/manifold-renderer/src/node_graph/fluid_cache.rs crates/manifold-renderer/src/node_graph/primitives/fluid_surface.rs`. Snapshot 2026-09-29: 167 hits in 13 files — 23 production sites in 7 files (`fluid.rs` 8, `fluid/take.rs` 4, `fluid_cache.rs` 3, `fluid/coupled/native.rs` 3, `fluid/native.rs` 2, `fluid/impulses.rs` 2, `fluid/roles.rs` 1) and 144 in tests. `physics.rs` has its own local `TICK` (Box3D's `FIXED_TICK`) and is out of scope. If the counts differ, list the new sites before touching anything.
- **Old → new:** `pub const TICK: f64 = 1.0 / 60.0` (`R/fluid.rs:44`) → `pub enum SolverRate { Hz60, Hz30, Hz20, Hz15 }` with `fn seconds(self) -> f64`, stored as `FluidSettings::solver_rate` with a serde default of `Hz60`. `simulation_tick(time)` → `simulation_tick(time, rate)`. Production sites read `settings.solver_rate.seconds()`. Tests rewrite mechanically: `TICK` → `SolverRate::Hz60.seconds()` (worked example: the expected times in `fluid/take/tests.rs`). The cache writer and reader (`fluid_cache.rs:405`, `:459`) keep their 60 Hz check; Record and Playback reject other rates before a cache opens. Coupled observation rejects rates below 30 Hz before the worker sees them; the rigid owner's tick check (`R/fluid/coupled/native.rs:316`) compares against the pair's `solver_rate` instead of `TICK`. Native `max_substeps` scales by `60 / rate`. New `node.fluid_surface` param `solver_rate` (Enum "60 Hz"/"30 Hz"/"20 Hz"/"15 Hz", default 60 Hz, Simulation section).
- **Technique:** delete `TICK` first; the build errors are the checklist. Deletion gate: `rg -n '\bTICK\b' crates/manifold-renderer/src/node_graph/fluid.rs crates/manifold-renderer/src/node_graph/fluid crates/manifold-renderer/src/node_graph/fluid_cache.rs` returns zero.
- **Deliverables:** the seam; D9's combination errors; tests `fluid_solver_rate_rejects_record_playback_engine_mesh_and_coupled_below_30`, `fluid_solver_rate_round_trip` (save at 30 Hz, reload, the world restarts once and runs at 30 Hz), `fluid_thirty_hertz_particle_view_is_even` (P3's metric on a real 30 Hz solver).
- **Gate:** the tests plus the existing renderer `fluid_` and `water_` suite green; clippy clean; deletion gate zero.
- **Demo:** P3's capture command after setting `WaterDamBreakParticles.json`'s `solver_rate` to 30 Hz. L2.
- **Gesture:** choose 30 Hz, pour, and watch the motion stay smooth at 60 fps.
- **Forbidden:** keeping `TICK` as an alias; accepting a non-60 rate in any cache path, or a coupled rate below 30 Hz; any coupled fallback mode; exposing substeps or CFL.

### P5 — Level-set atoms

**Built (2026-09-30).** `node.sort_particles_into_cells`, `node.running_total` (their
scan is one module, `R/primitives/prefix_scan.rs`), `node.shape_particle_blobs`,
`node.particle_volume`; records `FluidBlob` and `CellRange` (channel names registered in
`well_known`). Value tests against f64 references in `R/primitives/liquid_surface_tests.rs`:
the binned permutation and contiguous ranges, the Max Cells error, scans at 1, 255, 256,
257 and 2²⁰+3 with the one-frame total, blob shapes (line, cloud, isolated, pair at
2.5 r), and the level set against a brute-force sum over every blob with a half-space
solid. Decisions made while building:
- **Hand kernels for the sort and the scan.** Exclusion 1, the `node.spawn_from_mesh`
  precedent. `standalone_for_boundary_spec` emits texture kernels only, and the count
  and scatter passes share the scan's storage and a rank scratch.
- **The lattice reaches kernels as scalars.** Buffer codegen binds params and arrays
  only, and a `Transform` wire into a GPU atom is a fusion cut, so the seam keeps
  `grid_bounds: Transform` and the group splits it with a new CPU atom,
  `node.transform_components` (the inverse of `node.transform_3d`; the MLS-MPM lattice
  wires need it too). Bin size is the simulation cell times a factor, one math chain
  feeding sort, blobs and volume.
- **Kernel shape.** Axis lengths follow the square roots of the covariance's
  eigenvalues, their ratio capped at `stretch`, rescaled to keep the isotropic kernel's
  volume; the kernel is `(1 − |G·r|²)³`. Reach is capped at one bin from the particle
  (`cell_size − |centre − particle|`), so a node's ±1-bin search is exact. The isolated
  rule measures physical radii (neighbour within 2 r → 3 r), because the search reaches
  one bin, not slot-9's three meshing radii. Yu & Turk's `k_s`/`k_n` constants are not
  used.
- **No lattice yet.** Before its first frame the producer publishes zero nodes; the
  volume, count and mesh atoms then emit nothing, without an error.

- **Entry state:** P3 merged. Anchors: `rg -n 'BarrieredReduction' crates/manifold-renderer/src/node_graph/primitives/spawn_from_mesh.rs`, `rg -n 'atomic_outputs' crates/manifold-renderer/src/node_graph/primitives/scatter_particles_3d.rs`, `rg -n 'input_access' crates/manifold-renderer/src/node_graph/primitives/triangulate_grid.rs`.
- **Read-back:** D8, D14, D15, D17, D18; section 4.1; the Yu & Turk 2010 sections on anisotropy and centre smoothing.
- **Deliverables:** `sort_particles_into_cells`, `running_total` (its scan module shared with the sort), `shape_particle_blobs`, `particle_volume`. Value tests against CPU f64 references: permutation and contiguous ranges; scans at section 4.1's sizes; blob shapes for a line of particles (stretched along the line), a uniform cloud (isotropic), an isolated particle (radius scaled by `isolated_scale`), a pair at 2.5 r (in between); volume sums on a random fixture within 1e-4 relative; the solid clamp against a half-space solid.
- **Gate:** value tests and the GPU filter green; clippy clean.
- **Demo:** none — L1. The level set has no view until P6.
- **Forbidden:** `Texture3D`; an atomic kernel splat; one atom that both sorts and sums.

### P6 — Marching cubes, the Liquid Surface group, and the budget

**Built (2026-09-30).** `node.count_surface_triangles` and `node.volume_surface_mesh`
share `marching_cubes_common.wgsl`: FLIP Fluids' polygonizer corner, edge and triangle
tables, packed four bits per edge. The emit interpolates every lattice edge from its
lower-indexed node, so the two cells sharing an edge write bit-identical vertices and
the mesh welds closed. Value tests: the sphere against a CPU f64 marching cubes whose
table is parsed from the vendored upstream source (positions within 1e-5, outward
normals, area within 1%), `volume_surface_mesh_sphere_is_watertight`,
`volume_surface_mesh_overflow_writes_empty`, `fluid_surface_overflow_reports_error`.
Preset `WaterDamBreakGpu.json` ("Water — Dam Break (GPU Surface)") is the Dam Break with
the "Liquid Surface" group feeding the water object, at 60 Hz (P4 dropped). Surface
Detail 0/1/2 binds `resolution_scale` 2/3/4 on the volume and the mesh; Surface Particle
Scale binds the blobs; the cache card params are gone because particle outputs are
Live-only (D12). `graph-tool fusion` places no surface atom in a region: blobs and
count gather everything, volume publishes lattice scalars, the mesh takes the CPU-only
total, and sort and running total are barriered. There is no fused pair to prove.
Mesh Capacity is 8,388,606 vertices: the preset's own defaults (res 64, Detail 1) need
7.03 M by tick 180. `fluid_capture --gpu-surface` reads the mesh back every offline
frame and reports its live vertices.

**Budget.** The gate is 6.0 ms p95 at res 64 ×2 (D20; the lead re-baselined it on
2026-09-30 from the 3 ms set before any measurement). With P6b and P6c the Liquid Surface
measures 5.29 ms p95 at res 64 ×2, tick 90, load 3–7: sort 0.20, blobs 2.03, volume 1.95,
smoothing 0.45, count 0.15, running total 0.31, emit 0.24; 1080p frame 13.2 ms. Blobs and
volume are a kernel design item (BUG-l24y (GPU liquid surface kernels cost)). The three
levers priced for it were measured in P6d; none paid.

The table below is P6 as first measured, before live-triangle draws and smoothing:
`fluid_surface_perf`, M4 Max, macOS 26.6.2, load 14–16 (another session's tests), tick
90, 16 warm-up and 120 measured frames, p95 in ms. The surface column is the p95 of the
per-frame sum.

| Sim res | Scale | Sort | Blobs | Volume | Count | Total | Emit | Surface | 1080p frame | Live vertices |
|---|---|---|---|---|---|---|---|---|---|---|
| 32 | 2 | 0.08 | 0.26 | 0.34 | 0.02 | 0.06 | 4.14 | 4.86 | 15.7 | 0.30 M |
| 32 | 3 | 0.09 | 0.26 | 0.82 | 0.08 | 0.15 | 4.19 | 5.54 | 17.9 | 0.79 M |
| 32 | 4 | 0.07 | 0.26 | 1.69 | 0.17 | 0.35 | 4.10 | 6.59 | 21.0 | 1.40 M |
| 48 | 2 | 0.11 | 0.84 | 0.89 | 0.07 | 0.15 | 3.52 | 5.55 | 18.0 | 0.99 M |
| 48 | 3 | 0.13 | 0.85 | 2.45 | 0.22 | 0.46 | 3.59 | 7.62 | 24.1 | 2.60 M |
| 48 | 4 | 0.14 | 0.85 | 5.33 | 0.56 | 1.24 | 3.75 | 11.78 | 29.9 | 4.62 M |
| **64** | **2** | 0.21 | 2.03 | 1.94 | 0.15 | 0.31 | 3.54 | **8.07** | 25.9 | 2.34 M |
| 64 | 3 | 0.21 | 2.02 | 5.55 | 0.53 | 1.17 | 3.99 | 13.39 | 33.9 | 6.11 M |
| 64 | 4 | 0.21 | 2.04 | 12.19 | 1.23 | 2.83 | overflow | 21.83 | 32.4 | 10.88 M |

Open, for the lead:
- **Capacity drove cost.** Emit wrote every slot and the scene drew and ray-traced the
  zeroed tail (25.9 ms frames at 8.39 M capacity). P6b removed that; Detail 2 at res 64
  still needs more than 12.6 M vertices by tick 172.
- **Blobs and volume.** Two milliseconds each at res 64; the volume grows with scale³.
- **Kernel reach sets the look.** FLIP's marker radius is 0.31 cell, so at Surface
  Particle Scale 2.2 the kernel reaches 0.68 cell, about 1.4 particle spacings, and the
  surface shows particle rows: ridges along the flow at res 32 and a crinkled pool at
  res 64. Particle scale 4 (1.24 cells) with bins of two cells is smooth and glassy with
  2.4× fewer vertices at res 32 scale 4, but search cost grows with reach³. With
  one-cell bins the card's Surface Particle Scale stops acting above about 3.2, less
  where centre smoothing moves the kernel.

- **Entry state:** P4 and P5 merged.
- **Read-back:** D2, D15, D16, D20; GROUPING_GRAPHS.md; the vertex attributes at `R/fluid/native.rs:190-201`.
- **Deliverables:** `count_surface_triangles` and `volume_surface_mesh`; value tests against a CPU marching-cubes reference on a sphere SDF (positions within 1e-5, area within 1% of analytic); `volume_surface_mesh_sphere_is_watertight`, `volume_surface_mesh_overflow_writes_empty`, `fluid_surface_overflow_reports_error`; `graph-tool fusion` output for the group recorded, with a fused-vs-unfused proof for every pair it places in one region. The "Liquid Surface" group. Preset `WaterDamBreakGpu.json` ("Water — Dam Break (GPU Surface)"): Dam Break at 30 Hz with the group feeding the water object; the outer "Surface Detail" 0/1/2 maps to `resolution_scale` 2/3/4. Perf proof `tests/gpu_proofs/fluid_surface_perf.rs` behind a new `fluid-perf-proofs = ["gpu-proofs"]` feature shaped like `rt-perf-proofs` (`Cargo.toml:161`, `rt_dynamic_perf.rs:31-33`): seeded res-64 dam break, 90 ticks captured through P1 into memory, the group at scales 2/3/4, fused and unfused, 16 warm-up and 120 measured frames, per-stage GPU time via `GpuTimestampSampler` (the `src/bin/freeze_profile.rs:1269` pattern), the full scene at 1920×1080 reported alongside; machine, OS and build recorded.
- **Gate:** tests green; check-presets and graph-tool validate/fusion clean on `WaterDamBreakGpu.json`; `cargo test -p manifold-renderer --features fluid-perf-proofs --test gpu_proofs fluid_surface_perf` reports p95 ≤ 6.0 ms at scale 2 on M4 Max (D20). A miss stops the phase and reports the slowest stage — never a silent quality cut. The RT and volume-optics per-frame cost is reported, not gated.
- **Demo:** `fluid_capture --preset WaterDamBreakGpu --frames 180 --stills-every 15` into `/tmp/manifold_gpu_surface`. Computed checks on the run: every frame after the first tick has a nonzero triangle count and no non-finite vertex. L2: Peter compares the stills with the CPU Dam Break at Detail 2.
- **Gesture:** raise Surface Detail from 0 to 2 mid-splash; the surface sharpens the next frame and the simulation does not restart.
- **Forbidden:** vertex welding or mesh smoothing passes (deferred); tuning thresholds to pass the budget without reporting it.

### P6b — Live triangles only (BUG-j9cy (GPU liquid mesh live-only draw and emit))

Lead call, 2026-09-30: emit and draw only live triangles, so nothing downstream touches
the zeroed tail; preset defaults stay. **Built (2026-09-30); its budget verdict stops the
phase** (below).

- **Shape.** `node.running_total`'s one-thread total kernel also writes an `extent`
  output: the grand total, then an indirect grid of 256-thread groups covering
  max(total, last frame's total) × `per_item` elements, with last frame's total kept in
  a buffer the node owns. `node.volume_surface_mesh` takes `extent`, dispatches its
  generated kernel over that grid (a new vertex buffer is written whole once), and
  publishes the array's live extent: the extent's total × 3 on the GPU, plus a CPU bound
  (the late `total` × 3 × 2.0 plus one grain, rounded up to 3·16,384 vertices, clamped
  to capacity; capacity for the first two frames). Slots past live stay zero for every
  consumer. `node.render_scene` writes each such object's draw arguments with one small
  dispatch and draws every raster pass indirectly; volume optics does the same; ray
  tracing builds over the bound, because Metal builds triangle acceleration structures
  from a CPU count. The 2.0× margin (lead call) covers the dam break's measured
  worst growth, 1.19× in one tick and 1.42× over two, with room for a splash impact:
  the total is read a tick late. Growth past the bound within that lag truncates ray
  tracing, not raster, for that frame.
- **Seam, as built.**
  - `manifold-gpu`: `DepthMsaaDraw` carries `count: DrawCount<'a>` (`Direct { vertices,
    instances }` or `Indirect { args, offset }`, Metal's four-word draw arguments); the
    constructors build `Direct` and `DepthMsaaDraw::indirect(args, offset)` switches one
    draw. `draw_instanced` takes a `DrawCount` in place of its two counts (11 callers,
    9 mechanical). New `dispatch_compute_indirect`, sharing `dispatch_compute`'s binding
    code.
  - Renderer: `LiveExtent { counts, offset, per_item, bound }`
    (`node_graph/live_extent.rs`), published with `NodeOutputs::set_live_extent` and
    read with `NodeInputs::live_extent_slot`, stored per slot by the backend and drained
    by the executor like mesh sources. `node.scene_object` forwards the vertices slot
    unchanged, so wiring is unchanged.
  - `render_scene.rs`: `mesh_vertex_count` is gone; each `ObjectDraw` carries
    `vertex_count` and `point_count` (the bound with a live extent) and its arguments;
    `ObjectDraw::live` and `ObjectDraw::draw_count` pick indirect or direct.
- **Call sites, as executed.** render_scene 2797 (pass hash), 2864 and 2940 (depth-only
  passes), 4266 and 4635 (colour pass), 4907 (depth pass), 5506 (ray-tracing triangle
  count); volume_optics 150, 158, 174. The weights-length check stays against capacity,
  because indirect draws can reach it.
- **Proofs.** `indirect_dispatch_and_draw_match_direct` (manifold-gpu),
  `fluid_running_total_extent_covers_this_and_last_frame`,
  `fluid_volume_surface_mesh_writes_only_live_and_last_frame_vertices` (a sentinel past
  last frame's extent survives; vacated slots clear),
  `live_draw_args_are_whole_live_triangles_within_capacity`,
  `object_wire_carries_the_mesh_live_extent`. The Dam Break at its defaults renders the
  same stills as before P6b (11 and 45 pixels of 921,600 differ, by at most 2/255).
- **Budget.** `fluid_surface_perf`, res 64 ×2, load 6–7: emit 1.04 ms (3.54 before),
  Liquid Surface 5.62 ms p95 (8.07), 1080p frame 17.6 ms p95 (25.9). Blobs 2.02 ms plus
  volume 1.94 ms exceeded the original 3 ms on their own; the lead re-baselined the gate
  to 6 ms (P6, D20) and made the kernel cost a design item (BUG-l24y (GPU liquid surface
  kernels cost)).
- **Content-thread gate, headless.** `fluid_capture --gpu-surface` with its preview
  pass: steady-state CPU encode time per frame (`render_cpu_ms` in `preview.csv`) under
  20 ms, checked with `awk -F, 'NR>31 && $5>20 {n++} END {exit n>0}' preview.csv`. The
  first 30 frames are excluded because cold start spikes under load (34 ms at frame 8,
  load 18–32, on a landing seat; steady-state max 8 ms). At the defaults: max 6.75 ms,
  mean 1.04 ms.
- **Deletion gate.** `rg -n 'fn mesh_vertex_count' crates/manifold-renderer/src/node_graph/primitives/render_scene.rs` and `rg -U 'pub struct DepthMsaaDraw[^}]*vertex_count' crates/manifold-gpu/src/metal/encoder.rs` both find nothing.

### P6c — Level-set smoothing

Lead ruling, 2026-09-30: kernel reach is not the lever (scale 3 or 4 with two-cell
bins stays washboard and lumpy at 4× the blob and volume cost). The CPU mesher looked
smooth because it ran smoothing iterations; the GPU chain gets a smoothing stage
between the level set and marching cubes. Particle scale 2.2 and one-cell bins stay the
defaults. **Built (2026-09-30).**

- **Audit** (DECOMPOSING_GENERATORS.md section 2.5 (the primitive audit)): no existing
  atom smooths an `Array(f32)` lattice. `node.blur_3d_separable` is `Texture3D`-only
  (D8), `node.neighbor_smooth` works on instance arrays, the rest are 2D. New.
- **Atom.** `node.smooth_lattice`: one axis of the binomial blur, `passes` rounds of
  [1, 2, 1] / 4 applied as one (2·passes + 1)-tap gather with edge-clamped indices,
  fusable per element (`BufferGather`). The Liquid Surface group chains axes x, y, z
  between the volume and the count and mesh atoms; one `node.value` ("Smoothing
  Passes") feeds all three and is the group param `smoothing_passes` (default 2).
  Because the weights are a product of per-axis rows and the clamp is per axis, the
  chain equals the full 3D binomial blur: `fluid_smooth_lattice_matches_binomial_reference_and_passes_through`
  checks the chain against an f64 (2p + 1)³ reference for 0–3 passes. A single 3D
  gather cost 2.0 ms at 2 passes; the chain costs 0.45 ms.
- **Result**, res 64 ×2, tick 90, load 3–7 (stills `r64_d0_smooth{1,2,3}_t{30,90}.png`):

| Passes | Live vertices t30 / t90 | Smoothing | Emit | Liquid Surface p95 | 1080p frame p95 |
|---|---|---|---|---|---|
| none (P6b) | 1.80 M / 2.34 M | — | 1.04 | 5.62 | 17.6 |
| 1 | 0.48 M / 0.79 M | 0.43 | 0.34 | 5.39 | 13.9 |
| 2 (default) | 0.40 M / 0.54 M | 0.45 | 0.24 | 5.29 | 13.2 |
| 3 | 0.38 M / 0.48 M | 0.47 | 0.21 | 5.27 | 13.0 |

  Smoothing pays for itself: fewer triangles make emit, draw and ray tracing cheaper.
  Two passes turn the crinkled pool into broad smooth waves; a softened band of
  regular ridges remains at the back of the pool, and faint vertical stripes along the
  near wall's waterline. Peter judges the set. Nodes inside solids can pick up liquid
  from their neighbours after smoothing, so the surface may sit fractionally inside a
  wall; the wall hides it.

### P6d — Blob and volume kernel levers

Lead brief, 2026-09-30: measure BUG-l24y (GPU liquid surface kernels cost)'s levers one
at a time at res 64 ×2; keep a lever only if it pays for itself and passes the same
proofs; record the rest. **Done: no lever kept.** The surface stays 5.27 ms p95.

| Lever | Measured | Verdict |
|---|---|---|
| Anisotropy once per tick | The volume already reads each particle's stored ellipsoid (`FluidBlob`, built once per frame by `node.shape_particle_blobs`); a cell visit costs one 3×3 multiply, nothing to hoist. At the 60 Hz producer tick equals frame. | Already so; nothing to gain at 60 Hz |
| Bin-local shared memory for the volume gather | Tiled kernel (one workgroup per 4×4×4 node block, the block's bins copied to workgroup memory, same sum order) passed the brute-force proof but took 30.4 ms against 1.95 ms at ×2, 103 ms against 5.5 at ×3: the tile needs 31 KB of the core's 32 KB, leaving one 64-thread workgroup per core. | Dropped |
| Half-precision neighbour reads | Scalar model at res 64 ×2 scales over 2,000 nodes: f16 blob fields move the level set by up to 9.1e-2 with world-space centres and 1.4e-3 with bin-relative ones, 10⁷–10⁹ f32 ULPs against a 1-LSB bar. | Dropped |

### P6e — Surface look: a distance level set

Lead brief, 2026-09-30: make the live surface read as water and come close to the FLIP
bake surface, working on the particle-frame seam, inside the 6 ms gate.

**Measured, before.** `fluid_capture --dump-mesh` writes the GPU mesh, the CPU FLIP mesh
of the same particles (a hidden second water object keeps the CPU mesher running) and
the particle frame. The oracle is the top surface as a height map at the lattice
spacing (dx/3), its slope split by wavelength with an FFT, in units of the simulation
cell dx. The CPU mesh is the reference: same particles, FLIP's own surface. Dam Break
(GPU Surface), res 64, Detail 1, region x −1.75…−0.3:

| Time | Mesh | Slope rms, wavelength 1–2 dx | 2–4 dx | over 4 dx | Curvature std (1/m) |
|---|---|---|---|---|---|
| 10 s | CPU | 7.2° | 4.0° | 11.4° | 12 |
| 10 s | GPU | 12.3° | 8.6° | 11.8° | 24 |
| 15 s | CPU | 7.0° | 5.1° | 15.8° | 10 |
| 15 s | GPU | 12.6° | 10.3° | 16.4° | 21 |
| 30 s | CPU | 4.9° | 4.2° | 3.2° | 9 |
| 30 s | GPU | 11.1° | 9.2° | 4.5° | 20 |

A still pool of undisturbed particles is flat in both (0.02° GPU): the roughness comes
from disordered particles. The long waves match; the GPU surface carries twice FLIP's
slope at 1–4 cells and twice its curvature. That is the orange peel. A settle scene (a
small drop into a pool, 8 s) shows the same: GPU 12.8° and 9.8° against CPU 6.1° and 3.4°.

**Root cause.** `node.particle_volume` places the surface where a sum of kernels crosses
a threshold. Where particles are disordered, how many kernels overlap a point varies from
place to place, so the surface height follows the local particle count, not the particle
positions: bumps two to eight particle spacings wide. Level-set smoothing tied to lattice
nodes (P6c) reaches a third of a cell at Detail 1 and cannot remove them; anisotropy
neither causes nor cures them. FLIP builds a distance field instead (the distance to the
nearest particle sphere), whose surface sits a fixed distance from the particles.

**Evidence** (an f64 replica of sort, blobs, volume and smoothing run on the dumped
particles; it matches the GPU surface at correlation 0.92 above 2 cells). Settle scene at
8 s, CPU FLIP reference 6.1°, 3.4°, 5.1°, curvature 10:

| Level set | 1–2 dx | 2–4 dx | over 4 dx | Curvature |
|---|---|---|---|---|
| Kernel sum, 2 passes (today) | 23.1° | 14.5° | 7.3° | 42 |
| Kernel sum, 6 passes | 4.8° | 8.9° | 7.5° | 7 |
| Kernel sum, covariance over a whole cell | 29.6° | 19.6° | 8.7° | 59 |
| Isotropic kernel sum | 22.0° | 14.6° | 7.3° | 37 |
| Distance to the blob ellipsoids, 2 passes (this phase) | 8.4° | 5.5° | 5.3° | 10 |
| Same, centre smoothing 0 | 6.8° | 4.4° | 5.2° | 8 |
| Same, isotropic blobs (FLIP's sphere union) | 6.1° | 3.8° | 5.1° | 7 |

More smoothing passes flatten only the shortest bumps and thicken the liquid; a wider
covariance makes the anisotropy noisier, not calmer.

**Shape.** `node.particle_volume` writes `min(band, min over blobs of a·(|G·(x − c)| − 1))`,
where `a` is the blob's longest axis and `band` is a tenth of a bin: the distance to the
nearest blob ellipsoid (exact for spheres, scaled by the long axis for stretched blobs),
negative inside, capped a tenth of a bin outside. `threshold` goes. D15 stands: solid
nodes are `max(φ, 0)`, border nodes are `band`. The cap is exact because of one contract
between the two atoms: `node.shape_particle_blobs` caps a blob's reach at `0.9·bin −
|centre − particle|`, so any blob a node's ±1-bin search misses is at least `band` away.
The volume value test checks it against a brute force over every blob. D2 stands: the
kernels are still Yu & Turk's, and FLIP's sphere union is this atom fed isotropic blobs
(stretch 1, smoothing 0, isolated scale 1) — a look choice for Peter, not a code path.

- **Entry state:** P6c built; `rg -n 'threshold - sum' crates/manifold-renderer/src/node_graph/primitives/shaders/particle_volume_body.wgsl` finds the kernel sum.
- **Read-back:** D2, D14, D15, D18; P5's kernel-shape notes; P6c.
- **Deliverables:** the volume and blob kernels above; value tests `fluid_particle_volume_matches_brute_force_distance_and_solid_clamp` and the blob reference with the new reach cap; `threshold` removed from the four presets that carry the Liquid Surface group; the capture tool's `--dump-mesh`.
- **Gate:** the tests and `scripts/gpu_proofs_gate.py --filter fluid_ --filter water_` green; renderer clippy clean; check-presets and graph-tool validate/fusion clean on `WaterDamBreakGpu.json`; `fluid_surface_perf` p95 ≤ 6.0 ms at res 64 ×2, the cost reported either way.
- **Demo:** `fluid_capture --gpu-surface --dump-mesh` on the Dam Break and the settle scene, before and after, stills plus the table above re-measured on the GPU mesh. L2: Peter judges the stills.
- **Gesture:** drag Surface Particle Scale mid-splash; the water swells and thins smoothly with no restart.
- **Forbidden:** raising smoothing passes or kernel reach to hide the bumps (measured above: neither removes them); a second level-set atom beside `particle_volume`; any MPM scene above res 64 on the GPU (BUG-bnp9 (MPM matter hard lock)).

### P7 — Add Fluid authors the GPU surface

- **Entry state:** P6 merged. Anchors: `rg -n 'pub struct AddSceneFluidCommand|scene_build_wire\(fluid_id, "vertices"' crates/manifold-editing/src/commands/graph/scene/fluid.rs`.
- **Read-back:** D9, D12, D13; FLUID_ENGINE_INTEGRATION_PLAN.md section 4 (Scene Panel and creative workflow); GROUPING_GRAPHS.md.
- **Deliverables:** `AddSceneFluidCommand` inserts the Liquid Surface group between `node.fluid_surface` and the water object, at 30 Hz, with Surface Detail mapped as in P6. Existing scenes are not migrated. Test `scene_physics_add_fluid_gpu_surface_undo_reload`. UI flow `scripts/ui-flows/scene-fluid-gpu-surface.json`: add fluid → play → raise Surface Detail → undo/redo → save/reload → play again, asserting the group exists and the water object's `vertices` producer is `node.volume_surface_mesh`.
- **Gate:** the test, the new flow and every `scene-fluid-*` flow on disk pass (count them); focused editing/app/renderer clippy clean.
- **Demo:** the flow. L3. Hand Peter the worktree launch command.
- **Gesture:** Add Fluid into an existing scene and drag the source while it pours.
- **Forbidden:** migrating saved scenes; a second fluid inspector; solver rate on the performance card.

### P8 — Whitewater at 60 fps

- **Entry state:** P4 and P6 merged.
- **Read-back:** D11 (the move-from-B branch); `R/fluid.rs:279-322` (`WhitewaterFrame`).
- **Deliverables:** whitewater frame outputs `foam_particles`, `bubble_particles`, `spray_particles` (`FluidParticle`, id 0, radius = lifetime fade) and their counts; `WaterDamBreakGpu.json` routes each through `interpolate_particle_frames` (A unwired; acceleration = gravity for spray, zero otherwise) → `particles_to_copies` → the existing copies objects. D9's whitewater restriction lifts. Test `fluid_whitewater_rewind_matches_ballistic` (spray against a closed-form arc).
- **Gate:** tests and the GPU filter green; check-presets and graph-tool clean.
- **Demo:** P6's capture command. L2.
- **Gesture:** trigger the dam break with whitewater on; spray arcs move at 60 fps.
- **Forbidden:** ids for whitewater; stashing velocity in `InstanceTransform` padding.

Phasing completeness: every behaviour this document commits to lands in one phase above
or in section 11.

## 10. Decided — do not reopen

1. FLIP plus a triangle mesh; screen-space rendering vetoed.
2. GPU surface: Yu & Turk anisotropic level set at 2–4×, marching cubes in compute, existing mesh consumers.
3. Solver 15–30 Hz with Hermite interpolation; never extrapolate; one tick of latency.
4. Section 3's particle frame is the seam for every solver.
5. The CPU mesher stays for CPU-mesh graphs and their Record/Playback.
6. Uncoupled Box3D never waits on the fluid worker; coupled pairs stay in lockstep with the fluid tick, 30 Hz floor, presented at `s`; no fallback modes.
7. No upload atom: provided outputs, worker-written slots, fence-gated ring.
8. Volumes are `Array(f32)` with their lattice on wires, not `Texture3D`.
9. One display time for live and export, and for every solver-time output.
10. GPU-surface graphs are Live-only until particle frames are cached.
11. Surface stage ≤ 6 ms p95 is the proof gate (D20).
12. Coupled Box3D is not decoupled from the fluid tick (Peter, 2026-09-29): lockstep is stable by construction, and at 30 Hz the rigid rate cost is within the water's own granularity.

## 11. Deferred, with triggers

| Item | Revive when |
|---|---|
| Particle-frame cache for Record/Playback (smaller, remeshable after bake); lifts D12 | The BUG-vglg.18 bake-workflow design session starts, or Peter wants to bake a GPU-surfaced scene |
| GPU solver writing the frame directly | P1/P4 measurements show the CPU solver cannot hold a show scene at a live resolution, and Peter approves a solver project |
| Vertex welding, shared-vertex output, mesh smoothing | Measured vertex bandwidth or RT build cost matters, or Peter wants CPU-style mesh smoothing |
| `Array(f32)` → `Texture3D` resolve atom (debug slice, `blur_3d` reuse) | Authoring needs to see or blur the level set |
| Migrating legacy presets to the GPU surface | Particle-frame caching lands |
| Live versus baked look parity | Peter judges the difference unacceptable before particle caching lands |
| P3: `interpolate_particle_frames`, `push_out_of_solid`, `mix_arrays`, the Particle View preset, and presenting `obstacle_pose` and the coupled rigid frame at `s` | A sub-60 Hz particle producer exists |
| FLIP particle identity (`manifold_id`, renumbering, worker-side sort) | FLIP frames must feed P3's interpolation |
| Fade-out of particles removed during a tick (D11) | Popping shows away from drains; needs a summed capacity expression in the fusion compiler first |

## 12. Risk register

| # | Risk | Detection | Response |
|---|---|---|---|
| R1 | The CPU solver is still too slow at useful resolutions; interpolation just holds | P1 probe table; P4 demo lag readout | Escalate with the table; GPU solver trigger |
| R2 | Native particle order is not id-sorted; sorting costs worker time | P1 capture ms | Sort stays on the worker; report the cost |
| R3 | Hermite overshoot near fast colliders shows through walls | `fluid_push_out_penetration_bounded`; P3 stills | Push-out bound holds; if exceeded, escalate before loosening |
| R4 | The surface misses its budget at 2× | P6 perf proof | Report the slowest stage; candidate fixes (empty-bin early out, support clamp) go to Peter as visible changes |
| R5 | Live and baked surfaces look different | Peter's P6 comparison | Particle-frame cache trigger |
| R6 | Mesh capacity overflow at 3–4× | Empty mesh plus error | Raise capacity in the group; never truncate |
| R7 | Per-frame RT and volume-optics rebuild cost from a 60 fps mesh | P6 report | Live-triangle draws (P6b); the welding trigger |
| R8 | Content-thread cost of ring and pose work | P2 render-trace gate | Fix before landing |
| R9 | A buffer generator with only gathered inputs cannot fuse, or cannot be expressed | `graph-tool fusion`; standalone codegen failure | Record the region result; BLOCKED plus a `bd` bug, never a quiet exemption |
| R10 | Doubled latency and quantization at 30 Hz feel wrong on stage | Peter at L4 | Rate is a setup setting; 60 Hz with the GPU surface stays available |
