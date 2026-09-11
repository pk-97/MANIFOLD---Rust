# Live Water — solver replacement and scene integration

<!-- index: Dedicated MLS-MPM water: bounded graph substeps, persistent layer state, moving colliders, scene depth/refraction, and the pool-and-cube prototype. -->

**Status:** IMPLEMENTED ON `wave/live-water`, RELEASE BLOCKED by BUG-01vr · 2026-09-11. Production at `bf9e56965` retains density-field rendering, dielectric shading and collocated MLS-MPM physics. Timestep convergence and late resting motion remain unresolved. The user authorized a researched solver replacement. APIC/MAC is under numerical validation, not accepted production behavior. No main landing while the blocker is open.

## Replacement decision — 2026-09-11

The replacement candidate is **incompressible MAC-grid APIC**, following
Jiang et al. 2015 section 6, equations 12–14, and the complete free-surface
pipeline in the maintained FLIP Fluids engine. This supersedes the earlier
MLS-MPM-only decision for this authorized replacement. The numbered MLS-MPM
formulas below describe the existing implementation until replacement; they
are not formulas to mix into APIC.

The implementation reference is `rlguy/Blender-FLIP-Fluids` commit
`70a0e954018fe39e1f9c3631264989569752bb7a`, dated 2026-08-24. Engine files
declare MIT; the separately GPL Blender add-on is not an import source.
No Blender dependency is introduced. Adapted source must retain applicable
notices. The f64 references independently implement the stated equations.

| Primary source / implementation | Date and applicability | Evidence limits / decision |
|---|---|---|
| [APIC paper](https://www.andyselle.com/papers/24/apic.pdf), [publication record](https://doi.org/10.1145/2766996) | Published 2015-07-27. Section 6 defines trilinear staggered-face transfers. | Figure 15 reports offline timing, not 1080p real time. Selected transfer formulation. |
| [FLIP Fluids engine](https://github.com/rlguy/Blender-FLIP-Fluids/tree/70a0e954018fe39e1f9c3631264989569752bb7a/src/engine), [APIC comparisons](https://github.com/rlguy/Blender-FLIP-Fluids/wiki/Domain-Advanced-Settings) | Maintained through August 2026; APIC introduced April 2021. Pressure, SDF, transfers, extrapolation and collision inspected together. | Offline CPU implementation. Dam-break example uses 1221 APIC versus 1427 FLIP timesteps, not a frame-rate benchmark. Selected coherent reference. |
| [Batty et al. coupling](https://www.cs.ubc.ca/labs/imager/tr/2007/Batty_VariationalFluids/) | SIGGRAPH 2007. Fractional solid boundaries and compatible pressure projection; supplied liquid sample includes ghost-fluid conditions. | Reduced-resolution interactive example is not our performance proof. |
| [ST-FLIP](https://ge.in.tum.de/publications/spatiotemporal-flip/) | SIGGRAPH 2026. Spacetime deposition and phase-field pressure address large-step temporal aliasing. | Authors report 2–8x speedups on large offline simulations. Deposition and pressure change together; not selected or mixed into APIC. |
| [Leapfrog Flow Maps](https://yuchen-sun-cg.github.io/projects/lfm/) | SIGGRAPH 2025. Real-time vortical flows and GPU AMGPCG. | Fire and aerodynamic examples do not establish particle free-surface water behavior. |
| [SPlisHSPlasH](https://github.com/InteractiveComputerGraphics/SPlisHSPlasH), [DFSPH](https://animation.rwth-aachen.de/media/papers/2015-SCA-DFSPH.pdf) | Maintained MIT library implementing the 2015 divergence/density pressure method and later boundary work. | Credible alternative with a different neighborhood and boundary formulation; no SPH corrections mixed into APIC. |
| [gl-pic-fluid](https://github.com/loganzartman/gl-pic-fluid) | MIT interactive 3D GPU PIC/FLIP. | Author reports biased wall pressures and corner explosions. Rejected as a correctness reference. |
| [Particles4All](https://github.com/matsuoka-601/Particles4All) | Current MIT interactive PBD fluid/rigid demo. | Different constraint method; no matched long-run/refinement/performance evidence for this task. |

### Coupled numerical contract

1. Trilinear P2G accumulates `m*w` and `m*w*(v_axis+C_row dot displacement)`
   on staggered faces. G2P uses the same weights and their gradients (APIC
   equation 14). The old quadratic MAC prototype is not a drop-in transfer.
2. The physics liquid SDF is a particle sphere union, radius `sqrt(3)*h/2`,
   with the pinned engine's `0.005*h` near-zero conditioning and solid
   extension. It is separate from the retained density-field renderer.
3. Extrapolate face velocities by bounded six-neighbor layers, apply gravity,
   then solve fractional MAC pressure. With `q=dt*p/rho`, RHS is negative
   divergence of face flux `openFace*uFluid + (openCenter-openFace)*uSolid`
   evaluated with each liquid cell’s open-volume fraction, matching the pinned
   engine’s moving-solid correction. Coefficients are open fractions divided
   by `h²`. Matrix and gradient use identical liquid-air ghost
   distance ratios, capped at 25 as in the pinned engine. Never apply the
   open fraction twice or multiply the q-gradient by dt/rho again.
4. Zero-pressure air, fractional solid apertures and solid normal velocities
   form one boundary discretization. The cell-volume correction is retained
   even when the solid velocity extension is not divergence-free. Closed
   incompatible pockets and pressure nonconvergence are explicit failures.
5. Extrapolate pressure-valid velocities and enforce solid normal flux.
   The CPU box fixture uses an explicit free-slip sampling extension: even
   tangential and odd normal reflection through aligned static basin walls.
   This intentionally differs from the reference engine's zero-valued samples
   inside static solids, which introduce tangential drag in particle gathering.
   The pressure aperture constraints remain unchanged. Gather APIC velocity/
   affine state, use reference RK3 advection, then geometric particle collision.
   No affine suppression or PIC blend. General moving-obstacle sampling remains
   unimplemented.
6. Pressure iteration limits require a checked residual and fault on
   exhaustion. CFL safety and timestep refinement govern timestep selection;
   the old 960 Hz default is not inherited as evidence.

The target is **1920×1080 at 30 FPS for the complete scene**. None of these
sources proves that target on Peter's Mac. Explicit sketch/offline tiers
must report settings and cannot silently discard simulation time. Earlier
60 FPS targets below are historical. Measure the combined solver and retained
rendering before claiming a real-time tier.

Validation runs from independent f64 kernels through a combined particle/SDF/
projection timestep, native GPU parity, physical cases and observed renders.
Required cases include 60-second rest and post-impact settling, small-amplitude
wave frequency/decay, impacts, translating boundaries, particle mass and
reconstructed volume, and timestep refinement. Lower kinetic energy alone
does not pass. Stage-only tests cannot close BUG-01vr or permit main landing.

### Current numerical evidence

The coupled CPU fixture is experimental and its acceptance tests remain red.
At fixed basin width 1.25 m and water depth 0.375 m, refining h from 0.125 to
0.0625 to 0.03125 m substantially improves wave dispersion. With the final
free-slip sampling correction at h=0.03125 m and dt=1/120 s, the measured
period is 1.45614 s versus theoretical 1.47462 s, and the later peak envelope
retains 96.7% of its initial height. These pass the 10% period and 50% retention
bounds. The 60→120 Hz and 120→240 Hz normalized waveform differences are
2.42% and 3.45% respectively: both satisfy the original 10% agreement bound,
but successive timestep refinement does not improve agreement. The additional
monotonic-refinement gate failed before and after the wall correction; further
solver changes stopped at the repository attempt limit. No passing coupled
acceptance or moving-object validation is claimed. The GPU scene integration
below does not transfer the 96.7% small-wave result to another grid or scene.

### Native APIC scene integration — 2026-09-11

`WaterWaveTankApic.json` is a non-bundled scene fixture with the original
3.3×2.25 m basin, stationary offset breakwater, camera and density-field water
renderer. It uses the new trilinear APIC solver entirely through the normal
graph/substep/Metal path, without a CPU simulation cache. The project-scoped
demonstration embeds this graph rather than changing the shipping preset.

The fixed grid has 64³ cells at h=0.0625 m and 65³ padded component-face
storage. Each substep composes separate atoms: checked Q20 P2G, resolve,
five extrapolation layers, gravity, particle-sphere SDF, fractional pressure
rows, zero pressure, 128 red/black SOR pairs (omega 1.7), residual validation,
matching pressure gradient, five extrapolation layers, free-slip wall sample
extension, trilinear APIC gather/RK3, collision, validation and commit. The
existing WaterState owns the clock at 120 Hz, capped at eight steps per output
frame; rendering remains outside the repeated region.

Pressure rows and RHS are both scaled by h²; q still means dt*p/rho.
Each row must satisfy an absolute divergence residual of 0.001 s⁻¹ plus
1e-4 times its RHS divergence magnitude. Failure sets sticky status bit 32
before particle acceptance. There is no unchecked fixed-iteration fallback.
This initial GPU linear solver uses SOR on the same fractional operator that
the f64 oracle solves with PCG; algorithm equivalence is established by the
native pressure/gradient comparison, not by iteration-count equivalence.

Geometry is stationary axis-aligned basin-minus-box intersection: exact
face areas and cell volumes, including exact closure rather than subtraction
roundoff in fully solid cells. Open pressure faces are preserved by the wall
sample extension. Only fully blocked sample faces receive even tangential /
odd normal reflection. Moving-body fractional flux and general moving-wall
sampling remain unimplemented in this GPU fixture.

The original Naga `Expression [97] is not cached` panic was caused by passing
a dynamically indexed array element by pointer into a WGSL helper. Using a
local stencil base and then assigning it to the array fixes native compilation
without changing transfer equations. Modules, channel names and hand-kernel
startup prewarm are restored. Native proofs now pass for transfer, extrapolation,
trilinear gather/RK3, fractional pressure and failed-solve rejection, particle
SDF, box fractions and wall sampling. Full-scene timing and physical acceptance
must still be assessed separately. That capture used the raster water overlay.
The later native water RT integration and its open acceptance gaps are described
in section 7; this earlier capture does not establish ray-traced water lighting.

The 900-frame, 30-second APIC wave-obstacle capture completed without a
runtime solver fault at 1920x1080. Mean encode/submit/GPU-completion time was
85.485 ms (PNG readback/encoding excluded); this misses the 33.33 ms target
and does not establish native-app FPS. Peter judged the motion improved but
the material insufficiently water-like. Operator proofs are not whole-scene
physical acceptance.

The preceding optics-only comparison kept the APIC group and density reconstruction
unchanged; the current reconstruction is specified below. The old scene pass always multiplied thickness by 1/10.9, the
additive-splat correction; density isosurface thickness already measures metres.
The renderer now consumes metres directly; there is no calibration multiplier
or representation selector. Peter explicitly confirmed that these water scenes
are WIP and need no legacy compatibility. The APIC fixture's deformation-driven
foam defaults to zero and environment emitter intensity to zero, retaining the
broad dome fill and direct sun rather than the three bright strip reflections.

APIC material absorption uses representative red/green/blue wavelengths
650/550/450 nm with coefficients 0.340/0.0565/0.00922 m^-1 from Pope and Fry
(1997), as reproduced in the [corrected NASA Table 1.1](https://oceancolor.gsfc.nasa.gov/files/resources/docs/technical/volivch1err1.pdf).
At attenuation distance 1 m the authored linear attenuation colours are
exp(-coefficient). This is a three-wavelength RGB approximation, not spectral
integration, scattering, turbidity, or a complete freshwater optical model.
IOR remains1.333; the optics-only comparison used reconstruction support radius0.10m. No viscosity,
surface-tension or particle-motion parameters changed in this appearance pass.

The metre-thickness native graph proof matches independent Beer-Lambert and
Fresnel values for a known 1 m path. The focused scene gate is still red:
five proofs pass, while `water_scene_occlusion_and_depth` reports r/b 0.995578
at pixel (41,78) against its <=0.995 threshold. The occlusion fixture now
supplies a known 0.12 m optical path; no assertion was relaxed. This failure
is tracked in BUG-01vr and blocks landing.

The neutral clear-water capture and one project-scoped daylight HDRI comparison
both complete 900 frames. The daylight graph reuses `node.hdri_source` with the
existing Kloppenheim pure-sky EXR; it is not a new renderer or solver. Coating
and studio contour artifacts are removed, but smooth surface reconstruction
and limited scene reflections remain visually unaccepted. The current pass
does not claim full-RT water or a finished realistic-water material.

The previously reported 5% amplitude retention sampled at the theoretical
period despite the phase error; it was not a peak-decay measurement. The
fixture now reports peak envelopes separately. Occupied-cell volume is only
a geometric proxy, not a proof of volume conservation.

**Prerequisites:** existing scene renderer, material system and native Metal backend. No cloth, ropes, baked-cache import or generic physics engine prerequisite.
**Execution contract:** [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md) sections 5–6 and 8; executable assignments are in [WATER_IMPLEMENTATION_PLAN.md](WATER_IMPLEMENTATION_PLAN.md).

The performer hits a pool with a cube on beats. Each hit acts on the water already
there. Water pours, splashes, settles and stays in the same scene as ordinary objects.
Peter: “I agree we should split between water and cloth and ropes”; “The ‘paddle’ can
just be a basic cube for testing”. This supersedes the PBF-first water lane in
[SIMULATIONS_DESIGN.md](SIMULATIONS_DESIGN.md); XPBD remains that document's cloth/rope
solver. Astra authors the architecture; Sol High owns implementation, diagnosis,
review and landing with bounded Luna Low assignments. Seat mapping for execution
(Astra review 2026-09-09): Sol is the k3 lead seat in this repo's fleet, Luna
lanes are K2.7 with two concurrent maximum, and Astra reviews escalations only.

## 1. Audit — what exists (verified 2026-09-09)

Snapshot base: `fd0dd5a96`. Anchors use symbols where line numbers would decay.
Re-derive before editing. These are source findings, not runtime observations.

| Piece | Anchor | Classification |
|---|---|---|
| Per-layer generator ownership | `crates/manifold-renderer/src/generator_renderer.rs`: `LayerGeneratorState`, `render_all`, `stop_clip`, `release_all` | Reuse. `layer_generators` is keyed by `LayerId`; clip stop removes the clip target, not the layer's generator. Structural removal evicts absent layers. |
| Graph lifecycle | `crates/manifold-renderer/src/preset_runtime/core.rs`: `render`, `reset_state`, `clear_state`, `clear_trigger_state` | Reuse. Full reset clears nodes and `StateStore`; trigger-only clearing must not erase water. |
| Persistent buffers | `crates/manifold-renderer/src/node_graph/state_store.rs`: `StateStore`, `NodeState`; `primitives/array_feedback.rs`: `ArrayFeedback` | Extend by analogy. Keys remain `(NodeInstanceId, OwnerKey)` inside the owning runtime. Existing feedback is frame-based and specifically `Particle`, not a generic substep solver. |
| Graph execution | `node_graph/execution_plan.rs`: `ExecutionPlan`, `ExecutionStep`; `node_graph/execution.rs`: `execute_frame_with_state`, `compute_live_steps` | New bounded substep-region support required. Today there is one frame traversal, frame-level late capture, hoisting and resource recycling. |
| Grouping | `manifold-core/src/effect_graph_def.rs`: `GroupDef`; `manifold-core/src/flatten.rs`: `flatten_groups` | Reuse for visual organisation only. Groups flatten; they are not runtime loops. No new group serialization required by this design. |
| Typed GPU channels | `node_graph/ports.rs`: `KnownItem`, `ArrayType`; `generators/compute_common.rs`: `Particle`, `PARTICLE_SPECS` | Reuse mechanism; add water records. Existing Particle is 64 bytes and has no affine matrix. Do not repurpose its colour/padding. |
| Atomic scatter | `node_graph/primitive.rs`: `atomic_outputs`; `freeze/codegen/standalone.rs`: atomic bindings; `primitives/scatter_particles.rs` | Reuse signed integer atomic support and generated dispatch infrastructure. Existing energy scale 4096 is a precedent, not a universally safe water scale. |
| GPU submission | `manifold-gpu/src/metal/encoder.rs`: `dispatch_compute`; `manifold-renderer/src/gpu_encoder.rs` | Reuse `manifold-gpu`, one encoder and preallocated uniforms. No WebGPU runtime, raw Metal bypass, new queue, thread or mutex. |
| Scene surface | `primitives/render_scene.rs`: `RenderScene`, `evaluate`, `force_consumed_outputs`; `shaders/render_scene.wgsl`: `sample_transmission` | Extend. Shared depth, opaque colour snapshot and a transmissive pass already exist. Water must explicitly request snapshots even with no glass objects. |
| Camera and depth | `node_graph/camera.rs`: `Camera::proj`, `view_proj`; `generators/shaders/depth_common.wgsl` | Reuse right-handed camera, Metal clip depth [0,1], UV Y flip and reconstruction conventions. |
| Surface filter | `primitives/bilateral_blur.rs`: `BilateralBlur` | Reuse algorithm/codegen helpers; extend coverage handling. Existing filter does not know an empty liquid pixel from far-plane depth. |
| Scene authoring | `node_graph/scene_vm.rs`: `SceneVm::from_def_with_layers`; `scene_modifier.rs`: plan builders | Ordinary graph topology, not a separate SceneObject document database. MVP is a bundled scene preset with exposed controls. |
| Controls | `assets/generator-presets/SceneStarter.json`, `OilyFluid.json`; `system.generator_input`; `primitives/transform_3d.rs` | Reuse scene assembly, feedback/control composition, stable NodeId bindings and parameter surface. Both presets' nodes and wires were audited. |
| Transport | `manifold-playback/src/engine.rs`: `seek_to`, `stop`, `set_time`, `advance_time`; `manifold-app/src/content_pipeline.rs`: `render_all` call | Add explicit simulation-frame context. Generic wall-clock dt and trigger counts do not reliably describe pause, seek and export. |

Paths abbreviated after their first occurrence above are relative to
`crates/manifold-renderer/src/`. The effect-chain grace eviction policy is NOT the
generator lifetime policy: do not invent a water cache to work around that unrelated
cache. The per-layer generator already provides the intended home.

Research basis: [MLS-MPM paper](https://yzhu.io/publication/mpmmls2018siggraph/paper.pdf),
[APIC paper](https://www.math.ucla.edu/~jteran/papers/JSSTS15.pdf),
[WebGPU-Ocean implementation](https://github.com/matsuoka-601/WebGPU-Ocean), and
[screen-space fluid rendering](https://developer.download.nvidia.com/presentations/2010/gdc/Direct3D_Effects.pdf).
Use the papers for transfer/stress equations. WebGPU-Ocean is implementation evidence,
not a dependency or a benchmark for this app: its author explicitly reports occasional
instability at the demonstrated large timestep. Its SPH comparison is not a comparison
with every modern SPH solver. Record the commit and licence of any code actually
adapted; do not port demo constants without the unit conversion below.

## 2. Decisions

**D1 — Dedicated MLS-MPM liquid system.** Use quadratic B-spline APIC transfers,
an explicit weakly compressible liquid stress model and a bounded uniform grid.
Recompute density from grid mass each substep. No solid deformation tensor, snow
plasticity or PBF density constraints. The first numerical proof decides whether this
chosen formulation is viable at the stated quality/cost; it does not silently select
another solver. XPBD cloth/ropes remain independent.

**D2 — Visible operations, one bounded repeat region.** Water is a graph of seeding,
emission/impulse, particle-to-grid transfer, stress, grid motion, grid-to-particle
transfer, collision and surface operations. The executor repeats only the simulation
region; cameras, scene draws and post effects run once per output frame. Rejected:
`water_sim` hiding every dispatch; repeating the whole scene N times; unrolling a
fixed number of copied water graphs. Bounded region support is a real prerequisite,
not “free” reuse of the existing feedback node.

**D3 — Layer lifetime, explicit reset.** The runtime instance owns water. Adjacent
clips on the same generator layer can change controls without replacing water.
Gaps/mute/occlusion freeze it; they do not integrate unseen elapsed time on return.
Deleting the layer, replacing its generator, changing topology/capacity or loading a
project starts fresh. Ordinary parameter changes do not rebuild simulation buffers.

**D4 — World-space physics, shared transforms.** One world unit is one metre in the
prototype. Y is up. Cube mesh and cube collider consume the SAME `Transform` wire.
V1 moving collider is a translating, fixed-size, axis-aligned box. Rotation, changing
scale, arbitrary meshes and two-way rigid-body response are deferred explicitly.
Do not expose controls that the collision implementation ignores.

**D5 — Screen-space water, integrated scene depth.** Surface reconstruction is
separate graph work. The existing scene renderer shades the resulting surface after
opaque objects and before scene post processing. Reflection uses the scene environment;
refraction uses its opaque colour snapshot. V1 supports an above-water perspective
camera, opaque/masked objects and ONE water surface set. No claim of recursive glass
refraction, underwater rendering, water ray tracing, caustics or liquid shadow casting.

**D6 — A numerical failure is observable.** No wrapping fixed-point momentum, NaNs,
silent particle deletion, arbitrary velocity clamp or automatic solver replacement.
Bounded GPU diagnostics retain the last valid state and report a water fault through
the existing node error path. Reset restarts; it does not hide a recurring fault.

**D7 — Bounded proof before product polish.** Numerical transfer/stability checks come
first, then the pool/cube scene. Peter judges realism from motion, not a particle-count
headline. No calendar promise, FPS claim or “SOTA” label before that checkpoint.

## 3. Data and ownership contracts

New module: `manifold-renderer/src/node_graph/water.rs`. Runtime records are not
serialized. Use `KnownItem` channel specs with the following exact field order and
std430 layout (all six fields Vec4F, 96-byte stride):

```rust
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WaterParticle {
    pub position_mass: [f32; 4], // world xyz; mass kg, zero means inactive
    pub velocity_density: [f32; 4], // m/s xyz; density kg/m^3
    pub affine_x: [f32; 4], // row 0 of C (1/s); w = 0
    pub affine_y: [f32; 4], // row 1; w = 0
    pub affine_z: [f32; 4], // row 2; w = 0
    pub previous_position: [f32; 4], // previous accepted substep xyz; w = 0
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WaterGridCell {
    pub velocity_mass: [f32; 4], // resolved velocity xyz; mass
}
```

Channel names are the field names, interned through the existing channel-name
mechanism. Shader structs use the same field order. Compile-time size and channel
stride checks are mandatory. Water display nodes consume `Channels<WaterParticle>`
directly; do not pretend these are ordinary 64-byte particles.

Grid accumulation uses a flat `Channels< i32 >` wire of `4 * nx * ny * nz` items:
`4*g+0..2` momentum xyz, `4*g+3` mass. Grid indexing is
`g = x + nx*(y + ny*z)`. The resolved grid is `Channels<WaterGridCell>`.
Status is one separate `Channels<u32>` word, sticky until reset. Domain dimensions,
capacity and grid spacing are build-time configuration; allocation is independent
of canvas resolution. No f16 in the physics loop.

The boundary owns three preallocated particle buffers: accepted, candidate and seed.
Temporary graph wires may alias only after the lifetime analysis proves safety.
`candidate` never aliases `accepted`: fault rejection needs the latter unchanged.
Grid, accumulation and bounded diagnostic/readback slots are allocated at install.
Use StateStore and existing cleanup/warmup contracts; no new global water manager.

Graph documents store node params/wires and stable NodeIds through the existing
camelCase JSON format. They do not store live particles, grid contents, clocks or
GPU handles. Save/reload restores the authored seed/configuration, then starts fresh.

## 4. Fixed substeps and graph compiler seam

Add `node.water_state` as the water-typed state boundary, analogous to
`ArrayFeedback`, with required `seed` and state-capture `in`, both
`Channels<WaterParticle>`. Outputs: `out` (same channels), `step_count`, `step_dt`,
`step_time`, `step_index` (ScalarF32). Inputs `time_scale` (default 1) and
`reset_trigger` (default 0) are scalar-shadowed params. First reset observation arms;
a subsequent integer change resets even while paused. Params `step_hz=960`,
`max_substeps=32` are install-time positive integers, not performance knobs.

New `node_graph/substeps.rs` owns the scheduling types:

```rust
#[derive(Clone, Copy)]
pub struct SubstepBoundaryPorts {
    pub seed: &'static str,
    pub capture: &'static str,
    pub state: &'static str,
    pub count: &'static str,
    pub delta: &'static str,
    pub time: &'static str,
    pub index: &'static str,
    pub results: &'static [SubstepResultPorts],
}

#[derive(Clone, Copy)]
pub struct SubstepResultPorts {
    pub capture: &'static str,
    pub output: &'static str,
}

pub struct SubstepRegion {
    pub boundary: NodeInstanceId,
    pub steps: Vec<usize>, // indices into ExecutionPlan.steps, install-time only
    pub held_resources: Vec<ResourceId>,
}

#[derive(Clone, Copy)]
pub struct SimulationFrame {
    pub frame_id: u64,
    pub delta: manifold_core::Seconds,
    pub epoch: u64,
    pub advancing: bool,
    pub exporting: bool,
}
```

Add `fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> { None }` to
`EffectNode` and `Primitive`, forwarding through the blanket impl. WaterState returns
the names above. Other nodes remain unchanged. Add
`ExecutionPlan::substep_regions(&self) -> &[SubstepRegion]` and store its regions at
compile time. Keep `ExecutionStep` and the public execute entry signatures intact.
Add `Executor::set_simulation_frame(&mut self, frame: SimulationFrame)` and forward
from `PresetRuntime::set_simulation_frame` and `GeneratorRenderer::set_simulation_frame`.
The context receives that optional frame without changing `FrameTime` semantics for
existing effects. Missing context on a graph with water is a reported error; tests
and warmup must provide it explicitly.

**Region derivation and validation:** cut the declared capture back-edge, as for
feedback. The region is the boundary plus nodes that are both descendants of its
state/step outputs AND ancestors of any primary/result capture producer. Each step-dependent atom,
including grid clear, must receive `step_dt` or another region output as an ordering
dependency. External inputs (seed, camera, controls, collider target transforms) are
evaluated once before the region. Contract each region to one vertex for outer
topological sorting. Disallow nested/overlapping regions, another feedback boundary
inside it, outside readers of intermediate wires, and render/IO atoms inside it.
Only the boundary's final primary/result outputs may escape. A malformed region is a
compile error with NodeIds, not a fallback to ordinary traversal.

The executor runs the boundary once to seed/resolve the CPU clock, then the body
`step_count` times. Before each iteration it sets scalar step outputs. After each
iteration, the boundary capture accepts the candidate into the persistent accepted
buffer; outside consumers read the FINAL accepted state, not the frame-start state.
At zero steps, `out` still exposes the accepted/seed state. Region capture must not
also run in frame-end late capture. Region resources remain held for the entire
repeat; no per-iteration pool churn. Do not call `execute_frame_with_state` recursively.
Reuse the existing single-step evaluation/binding routine after extracting it from
the outer traversal, so liveness, diagnostics and GPU tracking have one implementation.

Freeze must preserve region membership and never fuse across a region boundary.
Within a region, eligible pure per-element stages use generated code and normal
fusion. Atomic/global-dependency stages remain boundaries. Hoisting may cache external
constants but must not skip a step-dependent stage because its frame params appear
unchanged. Dirty epochs, uniforms and slot contents advance per substep. Uniform
allocations use distinct arena slices so all submitted steps do not read the last dt.

**Clock:** accumulate `SimulationFrame.delta * time_scale` in f64 Seconds. Consume
integer ticks of `h_t=1/step_hz`; keep only the fractional remainder. `time_scale` is
bounded [0,1] for V1: slow/freeze/normal, not unproven fast-forward. At the cap, live
mode drops excess WHOLE ticks, records dropped simulation time and visibly reports
overload; never enlarge dt or build an unbounded backlog. Export treats overload as
an error rather than producing a silently slower simulation. Duplicate frame_id must
not advance twice. A skipped-frame gap discards elapsed inactive time. An epoch change
resets seed, clock, collider history, emitter cursor, event latches and diagnostics.

**Cost:** this runtime seam is the largest non-water prerequisite. It gets its own
small synthetic test before shader work depends on it. It must not turn into a generic
nested-programming-language project or alter ordinary frame feedback semantics.

## 5. Numerical recipe and bounded fault handling

Coordinates, mass, velocity and pressure use metres, kilograms, seconds and pascals.
Initial proof defaults: domain origin `(-2,0,-2)`, 64^3 nodes, spacing `h=0.0625 m`,
rest density `rho0=1000`, sound-speed parameter `c0=10 m/s`, EOS exponent 7,
dynamic viscosity `mu=0.001 Pa.s`. Seed lattice spacing h/2, particle mass
`rho0*(h/2)^3`. 65,536 active particles, capacity 131,072. These are initial test
settings, not a promised performance tier. Inactive slots have mass zero.

For particle x, `q=(x-origin)/h`, `base=floor(q-0.5)`, `f=q-base`.
In each axis use quadratic weights
`w0=0.5*(1.5-f)^2`, `w1=0.75-(f-1)^2`, `w2=0.5*(f-0.5)^2`.
Visit all 27 tensor-product neighbours; `d=(base+offset-q)*h`.
Never silently discard stencil mass at a grid edge: contain particles within a
two-cell guard shell, enforced by collision and tested.

One substep, in this order:

1. **Emit/impulse.** Deterministic lattice emission into unused prefix slots;
   apply a latched event once, not once per substep. No recycling, drains or particle
   death in V1. Emit rate is particles/s; residual fractional births carry forward.
   Capacity exhaustion stops emission and reports Full while existing water continues.
2. **Clear grid.** Zero the accumulation wire each substep. Sticky fault status is
   cleared only by reset. Dispatch ordering, not workgroup barriers, separates stages.
3. **P2G mass/momentum.** Accumulate `w*m` and `w*m*(v+C*d)` on the grid.
4. **Density/stress.** Read the completed grid mass:
   `rho_p=sum(w*m_i)/h^3`, `V_p=m_p/rho_p`.
   `p=max(0, rho0*c0^2/7*((rho_p/rho0)^7-1))`.
   Zero negative pressure is the explicit free-surface approximation; no tension or
   artificial cohesion in V1. Stress is `sigma=-p*I + mu*(C+transpose(C))`.
   Add stress momentum `-4*dt*V_p/h^2 * w * sigma*d` to grid momentum.
   Recomputed density is written to a separate candidate record; no in-place
   cross-thread particle mutation. Density and stress share one particle operation
   because stress depends directly on that particle's reconstructed volume.
5. **Grid velocity.** Resolve mass/momentum, `v_i=momentum_i/m_i + gravity*dt`
   for nonempty cells; empty cells are zero. Apply no-penetration boundary velocities
   relative to the translating collider. Tangential velocity is free-slip in V1.
6. **G2P/advection.** `v_p=sum(w*v_i)`,
   `C_p=4/h^2 * sum(w*outer(v_i,d))`, `x_next=x+dt*v_p`.
   Store previous accepted position. Apply particle boundary projection as a separate
   collision stage to close grid-resolution leakage; use the same collider geometry.
7. **Validate/commit.** Validate candidate finiteness, positive live mass, full stencil
   containment and supported kinematics. After the global status write completes,
   copy candidate to accepted only when no sticky fault is present. Otherwise retain
   the last valid state. Subsequent steps with a fault are no-ops.

Named shader operations: `water_emit`, `water_impulse`, `clear_grid`,
`mpm_scatter_mass_momentum`, `mpm_scatter_stress`, `mpm_grid_velocity`,
`mpm_gather_advect`, `water_collide_box`, `water_validate`, `water_commit`.
Use `node.*` type IDs with those names; seed is `node.seed_water` and the state
boundary is `node.water_state`. Audit whether clear and collision can use existing
operations before registration; the current particle suite has no MLS affine state.
Density output must flow from stress to gather to preserve it; do not lose it through
a parallel copy of pre-stress particle records.

**Fixed point:** signed i32, initial scale Q=1,048,576 (2^20) for mass and momentum. Round each
contribution to nearest integer consistently; divide by Q on resolve. Negative
momentum remains signed. Use checked atomic accumulation, with
overflow setting a sticky status bit. The paired grid scratch is invalid and
discarded whenever status is nonzero; no wrapped atomicAdd can reach accepted
particle state. A direct 27-weight mass check rejected Q=4096
(2.4–4.8% error on simple lattice positions); Q=2^20 gave 0–0.0125% on those
fixtures. This is not the S1 transfer proof. Q is a documented proof parameter: compare against an
f64 reference before approving it. The kernel must also detect float-to-int overflow
before conversion. If quantisation fails the transfer tests, Sol reports the numerical
evidence to Astra; it does not guess another encoding in a Luna lane.

Fault bits: 1 nonfinite, 2 integer overflow, 4 outside supported domain/stencil,
8 reserved unsupported kinematics, 16 invalid density. Bounds for proof:
`|v|<=4 m/s`, Frobenius `|C|<=64/s`, `0<rho<=4*rho0`. Finite velocity or
affine excess warns; exceeding the density bound faults;
these are not clamping controls. The rest-density CFL check at installation is
`dt*(c0+v_max)/h<=0.25`; at the defaults it is about 0.233. This is a guard,
not a mathematical guarantee of stability of the whole discretisation. The EOS
wave speed grows as c0*(rho/rho0)^3; the rest-density check does not bound that
growth. At 1.15*rho0 the sound speed is about 15.2 m/s, which puts the default
step at CFL ~0.32 — above the 0.25 rest-density guard. Zero fault bits plus a
rest-density CFL pass is therefore not stability evidence. S1 implements and
reports the density-dependent acoustic CFL `dt*(c(rho)+|v|)/h`. S4 adds a
bounded half-timestep comparison: the default pool and impact fixtures rerun at
dt/2 must agree with dt in density field, particle motion and settling outcome
within recorded tolerances. Softness is classified as expected compressibility
only after that comparison passes; a mismatch is a numerical failure escalated
to Astra/Peter before S7, not a lane tuning task. The density fault bound alone
is not a stability guarantee.

GPU particle rejection remains same-substep. CPU reporting uses a bounded ring and
completed submissions only; final-substep export status is scheduled after the
candidate/status copy and queried after the export completion wait, while live
reporting remains nonblocking. When a fault completes, the visible collider
restores the last verified frame pose with bounded notification latency. There is
no same-substep CPU pose guarantee: collider and particle publication are not fully
atomic on the live path, and we never wait for same-frame readback. Include the
first fault bit and node in `ctx.error` once per transition. Full and live-overload
are nonfatal statuses, not shader faults.

## 6. Colliders, events and lifecycle

Cube collision takes a `Transform` wire and the cube mesh's fixed half extents. The
same source is fanned to the scene object; no copied position sliders. Previous and
target translation are retained by the water state. Interpolate over this frame's
accepted substeps, use `(target-previous)/simulated_seconds` for collider velocity,
and use relative normal velocity at contact. At a reset/reappearance, initialise both
translations to the current target (no artificial launch). While time_scale=0, hold
the collider target for the next advancing frame; do not move visible collision
geometry through frozen water. The displayed cube consumes the accepted collider
transform, emitted by `node.water_collider_motion`, and the target transform remains
the authored input. This is intentional physical state, not a second authoring model.
Translation speed above 4 m/s faults visibly; no teleport sweep claimed in V1.

`node.water_collider_motion` runs inside the substep region and emits its final
`Transform` through a declared region result (implementation plan section 2.1
specifies typed result resources). It has no independent clock. Boundary planes for the
basin are authored from the SAME dimensions as its visible opaque walls.

Impulse is a velocity change, in m/s, not force multiplied again by dt. Within radius
R of centre use `max(0,1-distance/R)^2 * impulse_vector`. An integer trigger-count
change supplies the event multiplicity; rollback rearms, it never creates negative
events. Queue an event across fractional frames with zero due substeps; consume on
the first actual substep. Explicit pause/zero time-scale discards incoming events and
rearms, preventing a burst on resume. Cap pending multiplicity at 32 with a reported
overflow status. Reset dominates emission and impulses on the same frame.

| Event | V1 behaviour |
|---|---|
| Adjacent clip starts on same water layer | Preserve state; existing trigger wiring may strike cube/inject impulse. |
| Clip ends / gap / muted or skipped layer | Freeze persistent state. Stop emission while not evaluated; no catch-up on return. |
| Transport pause / water speed zero | Render current water; no simulation or queued beat backlog. Camera may still move. |
| Transport stop | Preserve accepted water; clear event latches and elapsed accumulator. Play resumes that water unless reset/seek occurred. |
| Explicit seek, including forward seek | Reset to authored initial state at destination. No historical reconstruction promise. |
| Global timeline loop implemented through seek | Same reset as seek. Not a seamless physical loop. |
| Source-media clip loop | Does not reset water. Existing clip-edge events remain the only musical input. |
| Small continuous sync correction | No reset; physical stepping follows supplied forward dt, not subtraction of corrected timeline positions. |
| Load / generator replacement / topology or capacity change | Fresh seed. Parameter-only edits preserve state. |
| Export | Fresh seed at export range start, fixed steps, sequential export. Repeatable for same build/hardware/settings/input event schedule; not cross-device bit identity. |
| Finish/cancel export | Reset live water at restored timeline position; pre-export live particles are not restored in V1. |

Transport seam: add engine-owned `simulation_epoch: u64` plus getter; increment on
`seek_to` and project replacement. Stop does NOT increment it (table above is
authoritative). Continuous `set_time`/sync nudges do not increment. The direct
Play-from-position call in `content_commands.rs` must explicitly mark a seek epoch
when it relocates the playhead. Source-player loops are excluded. Use the existing
content thread to forward `SimulationFrame` before `GeneratorRenderer::render_all`;
delta is zero when not advancing, fixed export dt when exporting, otherwise the
accepted forward playback interval. Track stop through advancing transition plus
trigger clearing; no new ContentCommand or thread/channel.

The existing outer parameter surface exposes emission rate, impulse strength, cube
stroke, simulation speed and reset trigger through stable NodeId bindings. Beat
effects use existing modulation and easing nodes. Editing goes through the existing
graph edit/EditingService commands. A convenience “Add Water” scene action and a
dedicated reset button are deferred; V1 loads the Water prototype preset and uses
the existing exposed scalar/reset-trigger surface.

## 7. Surface and scene integration

Surface graph, evaluated once after all substeps (2026-09-11 replacement):

```text
water_state final particles ──► water_particle_bins ──┐
                           └─► water_foam ──────────┤
particles + bins ──► water_surface_fit ──► support-bound reduction
particles + bins + foam + shapes + bound ──► water_density_field
                                         └─► volume_isosurface
shared Camera + accepted collider ──────────► volume_isosurface
isosurface depth + thickness + normals + foam ──► render_scene ──► post
```

`node.water_density_field` evaluates the normalized cubic-spline field from
[Yu and Turk (2010), Eq. 8](https://faculty.cc.gatech.edu/~turk/my_papers/sph_surfaces.pdf):
sum of particle mass divided by sampled particle density, multiplied by the
anisotropic kernel. Fitted axes retain their absolute sizes and determinant.
Foam in G is weighted by those same field contributions. The APIC solver is
unchanged; the reconstruction density is computed only for rendering.

The cubic convention follows [Becker and Teschner (2007)](https://cg.informatik.uni-freiburg.de/publications/2007_SCA_SPH.pdf):
support radius 2h. The APIC fixture uses h=0.0625m, twice its 0.03125m particle
spacing, and center blend lambda=0.95. This replaces the earlier poly6 field,
lambda=0.5 fit, and per-particle constant-determinant normalization. Those
approximations were not the complete published reconstruction.

The existing 32³ linked bins remain the spatial index. An existing
`node.wgsl_compute` performs one barriered maximum reduction of the fitted
support bounds each frame; the density gather uses that measured bound about
the ORIGINAL particle centers. It cannot assume a fixed maximum axis after
removing determinant normalization. This indexing differs from the paper's
ellipsoid-AABB hash but evaluates the same supported contributions. No new
primitive, CPU readback, per-frame allocation in the changed primitives, or
solver feedback is introduced. Shapes require their bound input. Unwired
shapes use a spherical cubic with the explicit radius and rest density1000.

The field occupies [-2,0,-2] to [2,4,2], independently of the 64³ physics grid.
Resolution remains 128³; `vol_res` and `vol_depth` are graph-build allocation
settings. The generated Source shader writes Rgba16Float once per frame.
Raycasting this volume is an engine adaptation of the paper's marching cubes.
Neither Yu–Turk paper specifies the extraction isovalue; 0.5 remains an explicit
engineering choice, not a claimed published constant. Agreement with the
kernel equations does not establish extracted-volume conservation or visual
acceptance at this voxel resolution.

`node.volume_isosurface` raycasts the 0.5 level set with trilinear sampling and
refined crossings, deriving outward gradient normals and optical thickness in
metres from the sum of liquid intervals. Air gaps and the exact collider interval
are excluded. The five outputs are full-canvas Rgba32Float: raw clip depth
(empty=1), thickness (empty=0), +z-forward view normals, coverage and foam.
The existing scene water pass consumes them without a depth-blur or particle-splat
stage. The translating box uses the same half-extents as grid/particle collision.
Ray marching supports parallel slab axes and a ray beginning inside the volume;
this does not imply a complete underwater camera shading model.

Both new operations use graph-compiler shader generation. The raycast remains an
explicit fusion boundary because the derived-uniform registry cannot currently
recompute Transform inputs; it still runs normally in compiled/frozen graphs.
Existing splat primitives remain readable for serialized graph compatibility;
the bundled water graphs use the density-field path exclusively.

The target is 1920×1080 at 30 FPS (33.33 ms per complete frame), including 32
physics substeps at the current 960 Hz. Headless timings include encode, submission
and GPU completion, but exclude PNG work and do not establish native-app FPS.
Lower reconstruction resolution and higher offline detail are quality settings,
not permission to silently drop physics time. Surface validation and numerical
solver acceptance remain separate; a smooth render cannot close BUG-01vr.

### Offline APIC render bridge (2026-09-11)

`tests/support/water_offline_export.rs` exports the experimental CPU APIC
reference at h=0.03125 m and fixed dt=1/120 s. At 30 FPS, each cache interval
contains four simulation steps. Frame zero is the initial state; 900 frames
cover a 30-second movie without looping or resetting the simulation. The
little-endian records preserve the existing 96-byte WaterParticle layout,
including mass, velocity, affine rows and previous positions. This is offline
reference tooling, not a live generator or an accepted GPU solver.

`water-offline-render` reconstructs the same normalized poly6 density kernel,
with radius 0.0625 m, isovalue 0.5 and fixed 97×97×65 grid at 0.015625 m spacing.
Compact-support particle scatter accumulates density and analytic gradients;
marching tetrahedra emits ordinary MeshVertex triangles with outward normals.
The mesh enters `scene_object` and the production `render_scene` material path.
No dedicated water-overlay inputs are connected, so the existing overlay's
scene-wide RT exclusion does not apply.

The water material uses **Blend** alpha mode with transmission 1, IOR 1.333,
roughness 0.04, and the existing volume attenuation. Opaque mode is invalid for
this bridge: it lacks the opaque-scene snapshot needed by transmission and
produces black refraction. The production Blend path keeps GGX environment
specular and samples the RT-lit opaque scene for screen-space refraction.
**Blend water is excluded from the RT acceleration structure.** This bridge
therefore establishes full scene rendering with RT opaque-scene lighting;
it does not establish ray-traced water reflections, refraction, caustics, or
water shadow casting.

Each output frame holds the cached geometry fixed during bounded RT settling.
The tool requires eight ticks with both reflection and shadow/AO trace-channel
captures before saving; it fails after 48 ticks instead of saving a raster
fallback. Fresh per-frame bindings keep memory bounded. This deliberately
blocking capture path does not establish 1080p30 live performance. The existing
solver acceptance failures remain open under BUG-01vr.

Add OPTIONAL `render_scene` inputs:

```text
water_depth: Texture2D        water_thickness: Texture2D
water_normals: Texture2D      water_material: Material
water_camera: Camera
```

All five are required as a set when any is wired. `water_camera` is the actual
surface camera wire, used to validate equality of view/projection with the scene
camera at the current aspect: all view and projection matrix elements must be finite
and satisfy abs(a-b) <= 1e-6*max(1,abs(a),abs(b)). Compare values, not NodeIds or
pointers. No new opaque Liquid handle or scene identity map.
V1 water material is PBR dielectric, IOR 1.333, transmission 1, metallic 0; use
existing material fields for roughness and volume attenuation. Unsupported material
features produce an error, not an ignored control. Default attenuation distance 2 m,
attenuation colour (0.70,0.90,0.95), roughness 0.04. Direct sunlight uses dielectric
GGX with correlated Smith visibility and IOR-derived Schlick Fresnel. The
roughness-to-alpha mapping and environment latitude follow the scene PBR
conventions; water reuses `pbr_equirect_uv` so reflections match the environment
baker. Beer-Lambert transmission and depth-safe refraction remain separate from
the direct reflection term. Shading changes do not waive numerical acceptance.

**Pass order:** existing shadows → opaque/masked scene → resolve opaque colour/depth
snapshots → water fullscreen depth-tested shading pass → refresh public depth with
water surface depth → existing supported post processing. Reuse the current E2a
snapshot allocation/load path, broadening its condition to `has_transmission ||
has_water || rt_enabled`, preserving the existing RT branch for ordinary scenes.
When water requests `rt_enabled`, the current implementation requires the
`water_density` volume used by the primary isosurface. `water_isovalue` shares
the same scalar wire (default 0.5). Native secondary water rays reuse the scene
TLAS, material tables, alpha-test walk and hit-lighting helpers. They trace
opaque geometry for reflection and transmission, locate the liquid exit in
the density field, apply Snell refraction at entry/exit and Beer absorption
along the refracted path, and return first-Sun opaque-object visibility.
The primary fullscreen pass still publishes the reconstructed liquid depth.
The analytical native tests and 900-frame RT capture pass, but visual acceptance
fails: the full scene retains round blobs and gains dark stippling. The preceding
2010 video used raster. BUG-vglg.1 tracks the mismatched primary/secondary entry
bracketing and curved-surface validation; BUG-vglg.2 tracks omitted later liquid
intervals. Neither defect has been fixed or proved to explain all dark pixels.

Four internal dielectric interfaces bound continuation, including total internal
reflection; residual energy is truncated at that limit. Exit marching uses
half-voxel steps and eight bisections, bounded to 4096 steps for volumes no
larger than 512³ in the fixed four-metre domain. Dispatch regions reuse the
engine query-work planner with a conservative 96-query budget per pixel.
Water itself is not TLAS geometry: other water surfaces are not intersected by
the exterior scene rays, and water self-shadowing/caustics are not implemented.
RT off uses the existing screen-space/IBL path. An unready or stale scene TLAS
temporarily uses that same path with a once-per-transition diagnostic; readiness
and topology gates match the opaque RT path, so stale water radiance is not read.
After Pass A resolve, retain the existing single-sample `opaque_depth_snapshot` and
opaque colour snapshot unchanged for refraction. Load a distinct single-sample
Depth32Float water-pass attachment initialized from opaque depth; water fragments
write depth there. After that pass, call the existing `copy_depth_to_float` into
the public R32Float depth output. Never bind that output for simultaneous sampling
and writing. Keep both depth resources alive until the final copy completes.
Water fragments behind opaque depth are discarded. Visible fragments write the
reconstructed clip depth and shaded colour, so later depth-aware effects see water.
No water inputs means no additional resources, dispatches or colour/depth changes.

Lighting shares the existing scene-light packing, environment sampling, BRDF and
shadow lookup helpers; extract helpers if needed rather than copying a second lighting
engine. Water receives opaque-object shadows on direct light but does not cast shadows
or caustics. With RT disabled, refract the opaque scene with IOR and thickness;
shorten thickness by distance to the first opaque hit. Screen-space displaced
samples are clamped/rejected when they would
pull a foreground opaque object through the water. Beer-Lambert attenuation is
`exp(-sigma_a * thickness)`, with coefficients derived from existing material fields.
Fresnel blends reflected and transmitted light; do not add two full-energy images.

**Explicit V1 compatibility limits:** reject a water scene with any Blend object,
temporal upscaling/denoising, volumetric shafts, or multiple water sets. An RT
request without the matching density volume is an explicit validation error.
Reject partial water input sets during graph validation/rebuild before allocation.
Validate camera, material and dynamic compatibility at evaluation, before capability
fallbacks or pass encoding. Use `EffectNodeContext::error`, clear colour to magenta
(the existing scene error convention), clear depth to 1 and auxiliary outputs to
zero, then return. Never retain stale water output or silently disable a user setting.
Depth-aware spatial post effects can consume
the updated depth; temporal motion feeds are not claimed. These limits are visible
in the preset description. Supporting intersecting transparent objects and reliable
liquid motion vectors is later work, not a fake MVP implementation.

### Fitted particle surface

`node.water_surface_fit` implements Yu–Turk 2010 Eqs. 6 and 9–16 after the
accepted simulation state. The covariance neighbourhood has radius R=2h and
weights 1-(distance/R)^3. Self is included in the sums and neighbour count;
the paper's sums do not exclude self, but its count convention is not explicit.
All centers relocate by lambda times the weighted-mean offset for rendering
only. More than25 samples uses covariance eigenvalues directly, with each
clamped to at least the largest/4. Otherwise modified covariance is0.5I.
Exactly zero covariance also uses that finite sphere as an explicit degenerate
input convention. Five cyclic Jacobi sweeps supply orthogonal eigenvectors.

The paper calibrates ks to preserve approximately the kernel size of a full
interior neighbourhood. Its example1400 has no reported metric reference
scale and cannot be copied as a universal inverse-square-metre constant.
Here ks=20/(3R²): integrating the published weights over a uniform 3D ball
gives covariance eigenvalue3R²/20. This declared dimensional calibration
preserves uniform-interior size while retaining local kernel-size variation.
It scales as1/length²; no per-particle determinant normalization remains.

The output remains four vec4 channels (64bytes per particle):
`surface_center_radius` stores the relocated center and maximum FULL support
semiaxis; axis_x/y/z.xyz store the orthogonal FULL support vectors (columns of
2G^-1). axis_x.w stores sum_j mass_j W(x_j-x_i,h), evaluated on original
positions with the isotropic cubic spline. axis_y.w stores maximum support
semiaxis plus center displacement, the conservative reach about the original
particle. axis_z.w is zero. Inactive particles emit zero records. The optional
particle-splat path consumes xyz vectors and continues to interpret them as
full ellipsoid semiaxes.

Independent native proofs compare centers, covariance-derived support tensors,
density and reach against f64 sums and a converged eigensolver. Fixtures cover
bulk lattice, rotated plane, sparse pair, isolated self contribution, the25/26
threshold, and geometry/h scaling by2 with fixed mass (density scales by1/8).
Density tests use an independent inverse-matrix cubic oracle with variable
determinants and non-rest densities; reach tests cover strided tails and reset.
GPU input bytes must remain unchanged.

The [2013 TOG extension](https://faculty.cc.gatech.edu/~turk/my_papers/particle_surfaces_tog.pdf)
also labels connected components to prevent separate surfaces from blending.
The current extension uses three GPU operations: seed identity parents, union
original-position neighbors within the nominal particle spacing (0.03125 m),
then resolve canonical roots. Monotone atomic-min union-find connects arbitrary
chain lengths without CPU convergence readback or a fixed relaxation-pass cap.
Eq.17 filters covariance and center relocation to the same component; SPH
density still includes all original neighbors. The optional labels preserve the
2010 fit when unwired. Independent BFS labels, shuffled chains, separated sheets
and a connecting bridge are the new native proof cases. The demo reconstruction
volume is now 256³ (1.5625 cm voxels); the 64³ physics grid and dam-break seed
remain unchanged. These additions require their own validation; the results
below refer to the previous 2010 baseline. Neither paper promises
universal correctness, a prescribed visual result at arbitrary resolution, or
real-time performance. Whole-scene visual and physical acceptance remain
separate from equation-level verification.

The paper-baseline verification completed900frames at1920×1080/30fps without
solver fault. Six focused native proofs, clippy, build and graph/project loading
passed. Matching frames90 and480 show a smoother broad surface but remain short
of visual acceptance. Mean headless encode/submit/GPU-wait cost is64.196ms
versus108.163ms for the prior simplified fit; this excludes PNG work and does
not establish native-app FPS. Existing optics and refinement gates remain red.

### Optional foam

`node.water_foam` consumes accepted particles after the repeated water region
and updates a persistent foam buffer immediately after dispatch. It is not a
`late_capture` operation and has no feedback back-edge. `SimulationFrame`
epoch/reset dominates state; duplicate frame IDs do not advance it, and
paused `dt=0` frames leave it unchanged.

Foam strain is the Frobenius norm of the symmetric, trace-free part of affine
`C`. The source is `smoothstep(2, 10, strain) × smoothstep(0.15, 1, speed) ×
gain` (primitive default `3`). The bounded source/decay recurrence uses
`rate = source + ln(2) / half_life` and
`F_next = source/rate + (F_previous - source/rate) × exp(-rate × dt)`.
The demos use gain `12` and half-life `0.75 s`. `node.particle_foam` uses maximum coverage, radius
`0.046875` (matching the water surface), and a near-surface mask of `2r` against view-axis depth. Shading
may blend coverage as white foam. This is a deformation-driven visual
approximation, not foam fluid, bubbles, or spray.

`WaterPrototype` exposes continuous pour plus cube interaction;
`WaterImpact` is the impact-only variation. Both use the existing raster water
path and documented compatibility limits.

## 8. Prototype and acceptance

Preset `WaterPrototype.json`, display name **Water — Prototype**, category Sim.
Use SceneStarter's camera/light/environment/object wiring. One basin with opaque
walls and contrasting floor, one 0.5 m cube, one water volume. Initial pool is a
centred 2×0.5×2 m lattice raised above the grid's guard shell; walls enclose it with
room for splashes. Cube stroke is vertical, smooth and speed-bounded; a visible floor
pattern and a partly submerged cube make refraction/occlusion judgeable.

At 120 BPM: show still water, a short pour, four cube strikes one beat apart, then
stop driving it and show settling. Orbit the above-water camera. Also show two
adjacent clips, pause/resume and explicit reset. No unrelated show-wide render sweep.

Acceptance has two owners: Sol runs computed tests and frame-cost measurements;
Peter judges the motion and image. Required artifacts are a short real-scene sequence,
the project/preset, numerical results and timing/memory report. A pretty still is not
evidence of stable physical interaction. A green build is not a visual result.

Initial performance target: 1920×1080, 60 FPS, 65,536 active / 131,072 capacity,
water incremental GPU cost p95 <=6 ms and total scene GPU p95 <=12 ms, memory
increment <=128 MiB. These are budget targets to measure on Peter's available Mac,
whose exact chip/OS/build must be recorded. No 4K or larger-particle promise. A miss
stops expansion; report the slow stage rather than silently reducing quality.

Closeout benchmark evidence (2026-09-10): the corrected 1280×720 WaterPrototype
cutaway measured 15.911 ms median and 16.867 ms p95 over frames 60..599 with no
fault. The earlier baseline was 26.683 ms median and 27.721 ms p95 (frames 60..89);
the accepted atomic-only comparison was 14.569/14.763 ms over the same early window.
These are headless wall timings before native-app overhead, so the p95 still misses
the 60 FPS frame budget. The surface A/B retained the original radius and accepted
bilateral spatial step 2; the rejected radius reduction is not a design baseline.

Numerical acceptance: partition-of-unity <=1e-6; GPU/f64 one-step velocity error
<=1e-3 m/s and position error <=1e-5 m for the transfer fixture; mass error <=0.5%
for accumulated grid vs particles. Closed-basin tests lose ZERO live particles and
keep total particle mass unchanged. Interior hydrostatic density median within 5%
of rest, p95 within 15% (exclude the two-cell free-surface/boundary band). Collider
penetration <=0.1*h after projection. No fault bits in the default 10-second sequence.
Half-timestep comparison passes on the pool and impact fixtures: density field,
particle motion and settling outcome at dt/2 agree with dt within recorded
tolerances (thresholds recorded from the first passing run, then held).
Record actual values. Thresholds are acceptance targets, not observed results; a
failing target is evidence for Astra, not permission for Luna to loosen it.

## 9. Invariants & enforcement

| Invariant | Named check delivered by the plan |
|---|---|
| Same solver state survives clip edges/gaps | `water_lifecycle_preserves_clip_edges_and_gaps` |
| One physical advance per output frame, fixed dt | `substeps_count_order_and_duplicate_frame`, `substeps_pause_gap_overload` |
| Rendering outside repeat | `substeps_execute_post_once` |
| Correct final-state visibility and resource lifetime | `substeps_final_state_and_zero_steps`, `substeps_no_recycle_between_iterations` |
| Freeze preserves step semantics | `substeps_frozen_unfrozen_match`, existing GPU proof gate |
| No signed overflow or invalid state committed | `water_signed_scatter_and_overflow`, `water_fault_retains_last_valid_state` |
| Fixed dt resolves supported motion | `water_timestep_halving_stability` |
| New typed layouts match WGSL | `water_channel_layouts_match` |
| Shared cube geometry and collision | `water_cube_transform_and_collision_match` |
| Opaque occlusion and scene depth include water | `water_scene_occlusion_and_depth`, no-water parity test |
| Reset/load/export semantics | `water_lifecycle_seek_stop_export`, `water_preset_roundtrip_modulates` |
| No hidden unsupported feature | `water_scene_rejects_unsupported_combinations` |
| Zero new hot-loop allocation/readback stalls | preallocation source audit plus bounded trace in prototype; existing unrelated allocations are not claimed fixed |

## 10. Phasing

The complete entry checks, lane ownership, exact test commands and seam inventories
live in [WATER_IMPLEMENTATION_PLAN.md](WATER_IMPLEMENTATION_PLAN.md). S1 proves the
numerics; S2/S3 establish scheduling and clock; S4/S5 implement solver and colliders;
S6/S7 surface and scene; S8 packages and validates the instrument. No phase is declared
complete by this document. Later phases re-derive upstream anchors before dispatch.

## 11. Decided — do not reopen

1. MLS-MPM water; no PBF prerequisite and no cloth dependency.
2. Composable stages and bounded executor substeps; one scene render per frame.
3. Persistent per-layer generator state; no second water manager.
4. Shared physical/display cube transform; one-way translation collision in MVP.
5. Screen-space surface, existing scene lighting/depth, explicit compatibility limits.
6. Sol High leads Luna Low implementation; Astra reviews architecture conflicts and
   the first prototype evidence, not routine patches.
7. Numerical defaults must pass the named proof; changing formulation or thresholds
   is an Astra/Peter decision, not a worker's tuning task.

## 12. Deferred, with triggers

- Rotating/scaling/mesh/SDF colliders and two-way coupling: after translating-cube
  interaction passes, when a named performance scene requires them.
- Foam, spray, bubbles, surface tension and viscous artistic materials: after water
  motion and surface pass Peter's eye; each requires an explicit physical/visual model.
- Underwater camera, overlapping glass, multiple bodies, liquid shadows/caustics,
  RT participation and temporal motion: after the opaque-scene MVP passes; amend
  the render contract before enabling each.
- Arbitrary-time reconstruction, checkpoints, seamless physical loops and restoring
  live state after export: when timeline authoring requires historical replay.
- Scene-panel Add Water action: once the graph ABI is proven; reuse scene plan edits
  and exposure machinery, not a bespoke water panel.
- Half-resolution surface, sparse grids, sorting/scatter optimisation and more capacity:
  only after a measured stage misses the prototype budget.
