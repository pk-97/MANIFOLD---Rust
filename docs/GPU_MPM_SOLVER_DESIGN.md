# GPU MLS-MPM Solver — live liquid (then jelly, snow, sand, honey, lava) as GPU atoms writing particle frames

<!-- index: Replaces CPU FLIP as the live liquid solver with a GPU MLS-MPM built from graph atoms in a repeated substep region; writes the GPU surface design's particle-frame seam; two-way Box3D coupling; material zoo, whitewater and particle-frame bake as later phases. -->

**Status:** IN PROGRESS · P0a built on `feat/gpu-mpm-build` (not on main) · P0b–P7 not built · phase notes under each brief in section 11.
**Prerequisites:** GPU_FLUID_SURFACE_DESIGN.md P3 (particle records) before P1; its P1–P2, P5–P6 before P4.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) before starting any phase.

Peter's decision, relayed by the lead on 2026-09-29 and restated here, not reopened:
the live liquid solver becomes a GPU MLS-MPM (Hu et al. 2018, "A Moving Least Squares
Material Point Method with Displacement Discontinuity and Two-Way Rigid Body Coupling")
writing into the particle-frame seam of
[GPU_FLUID_SURFACE_DESIGN.md](GPU_FLUID_SURFACE_DESIGN.md). FLIP Fluids
(`crates/manifold-fluids`) stays as the bake and reference engine, and its look is the
visual reference. Liquids are always a real mesh; screen-space rendering is vetoed.
**This design supersedes the surface design's D1 clause that live water is FLIP, and
drops its D3/D9 solver-rate work and P4 for live; its seam, atoms and interpolation
stand.**

Why, measured by the lead on 2026-09-29: CPU FLIP at 64³ on the 90-tick Dam Break spends
152 ms solving, 242 ms meshing and 5.6 ms uploading per 1/60 s tick (medians); at 32³
it is 29 ms solve and 34 ms mesh. The CPU solver is ten times over a live budget at the
resolution the show needs.

**The governing insight: MLS-MPM is local and explicit.** A substep is a handful of
small dispatches over flat arrays — clear the grid, scatter particles to it, update grid
velocities, gather back to particles and move them. There is no global pressure solve,
no iteration count, no convergence to tune, and the same solver runs water, jelly, snow,
sand and honey by swapping the stress function per particle. Each dispatch is a graph
atom; a new executor feature repeats the region of atoms `substeps × ticks` times per
frame. The accepted state becomes the surface design's particle frame, and the surface
chain turns it into the ordinary mesh the scene, material, RT and volume-optics path
already draw.

**What it does for the show.** Water that simulates at 60 Hz in real time at 64³, reacts
to a hit on the tick it lands, pushes and is pushed by Box3D objects, and later becomes
jelly, snow, sand or honey by changing a material, not a solver. **The price, stated
up front:** weakly compressible water is slightly springy (density moves a few percent
at peak speed); one tick of display latency (the surface design's D10, already
accepted); and the proof gate Peter set — 500k particles in ≤ 6 ms — sits at the memory
bandwidth roofline of an M4 Max (section 8, Cost and the roofline). The design builds
toward that gate, measures early, and hands Peter priced levers if it misses.

Binding constraints, per DESIGN_AUTHORING.md section 1 (The intake): **hot path**
(the solver is 140–210 GPU dispatches per frame inside the render budget); **thread
residency** (everything encodes on the content thread, no worker, no lock); **time
model** (fixed 60 Hz physics ticks in `Seconds`, the documented physics exception);
**persistence** (graph JSON only until the P7 bake); **performance surface** (gravity,
speed, reset, forces, impulses and materials are live params).

Companions: [GPU_FLUID_SURFACE_DESIGN.md](GPU_FLUID_SURFACE_DESIGN.md) (the seam this
writes and the surface atoms that mesh it);
[WATER_SIMULATION_DESIGN.md](WATER_SIMULATION_DESIGN.md) (its historical "Earlier GPU
proposal" is an MLS-MPM design this mines for the substep seam and lessons);
[WATER_IMPLEMENTATION_PLAN.md](WATER_IMPLEMENTATION_PLAN.md) (the superseded S1–S8
briefs that built that prototype);
[FLUID_ENGINE_INTEGRATION_PLAN.md](FLUID_ENGINE_INTEGRATION_PLAN.md) (the FLIP feature
set and coupling contract, mapped feature by feature below);
[BOX3D_PHYSICS_DESIGN.md](BOX3D_PHYSICS_DESIGN.md) (the rigid solver it couples to);
[DECOMPOSING_GENERATORS.md](DECOMPOSING_GENERATORS.md) and
[ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md) (atom rules, codegen mandate);
[FREEZE_COMPILER_MAP.md](FREEZE_COMPILER_MAP.md) (what fuses, aliasing and
state-capture contracts). Beads: BUG-vglg (CPU FLIP scene physics epic); BUG-kjxf
(water perf direction after the S8 42 ms miss) and BUG-01vr (water dt-halving gate) hold
the prototype's measurements.

## 1. Audit — what exists (verified 2026-09-29 at `c8961489d`)

Paths abbreviated after first use: `R/` = `crates/manifold-renderer/src/node_graph/`,
`P/` = `crates/manifold-physics/src/`. Extend, don't redesign.

### 1.1 In the tree

| Piece | Anchor | State |
|---|---|---|
| Particle-frame seam | GPU_FLUID_SURFACE_DESIGN.md section 3 (The particle-frame contract) | PROPOSED, not built. `FluidParticle` (32 B: `position_radius`, `velocity`, `id`) and the `particles_a/b`, `blend`, `span`, `solid_a/b`, lattice outputs are defined there. This design writes them. |
| Surface atoms | GPU_FLUID_SURFACE_DESIGN.md section 4 (The atom chain) | PROPOSED, not built: interpolate, push-out, sort into cells, blobs, level set, marching cubes, the "Liquid Surface" group. |
| FLIP worker and tick | `R/fluid.rs:44` (`TICK = 1/60`), `:51` (`BATCH = 4`) | Exists. Stays for FLIP. |
| Role wire | `R/fluid_role.rs:20-25` (`FluidRoleKind`: InitialFill, Inflow, Outflow, Collider), `:36-44` (payload: `Arc<PreparedFluidGeometry { meshes: Vec<TriangleMesh> }>`, transform, enabled, velocity, inherit_motion, friction) | Exists, solver-agnostic. The matter solver consumes the same wires (D11). |
| Role inputs on FLIP | `R/primitives/fluid_surface.rs:38-101` (`role_0..role_63`), `:102` (`acceleration_field: VectorField`), `:103` (`domain: Transform`) | Exists. Shape precedent for `node.matter_roles`. |
| Domain layout oracle | `R/fluid/domain.rs:15` (`domain_layout`) | Exists. The matter grid uses it so FLIP and matter share cell size for the side-by-side (D5). |
| Shared physics vocabulary | `P/interaction.rs:5` (`VectorField`), `:11` (`FieldInput`), `:21` (`TickStamp`), `:238` (`SampledField`); `P/mesh.rs:9` (`TriangleMesh`), `:53` (`hull_meshes`); `P/lib.rs:958` (`dynamics`), `:1053` (`apply_impulses`) | Exists. Reused for fields, collider geometry and reactions. |
| Rigid tick owner | `R/physics.rs:355` (`RigidSimulation`), `:609` (`advance_with_coupling`), `:136-140` (`AdvancementPolicy::Worker { max_ticks }`); `P/stepping.rs:14` (`StepCoupling`), `:30` (`SubstepExchange`) | Exists. The coupling implements `StepCoupling` (D12). |
| Coupled-scene pairing | `R/scene_modifier_expand/coupling.rs:29` (`prepare_coupled_scenes`); `R/effect_node.rs:1143-1194` (coupling hooks); `R/physics/worker.rs:51` (`RigidSceneObservation`); `R/fluid/coupled.rs:50` (`CoupledRigidFrame`); `R/primitives/physics_world.rs:372-374`, `:556` (coupled mode) | Exists for FLIP. The matter solver plugs into the same hooks. |
| FLIP coupled duration check | `R/fluid/coupled/native.rs:316` (`duration != Seconds(TICK)`) | FLIP only; untouched. |
| Atomic scatter atom | `R/primitives/scatter_particles_3d.rs:95-98` (`fusion_kind: Boundary`, `boundary_reason: Blocked`, `atomic_outputs`), `:147` (`standalone_pipeline`) | Precedent for the scatter atoms. |
| Boundary codegen | `R/freeze/codegen/entry_points.rs:81` (`standalone_for_boundary_spec`), `R/primitives/standalone_pipeline.rs:18` | Exists. |
| Boundary reasons | `R/freeze/classify.rs:305-341` | `BarrieredReduction`, `CrossFrameState`, `IoBridge`, `Blocked`, … |
| Aliased and provided arrays | `R/effect_node.rs:802` (`aliased_array_io`), `:909` (`state_capture_input_ports`), `:920` (`persistent_output_ports`), `:1054` (`provides_array_output`); `R/execution/array_growth.rs` | Exists. Grid arrays ride these (D5). |
| Cross-frame array state | `R/primitives/array_feedback.rs:32-50` | `Particle` only. Shape precedent for `node.matter_state`, not reusable as is. |
| Readback | `R/primitives/color_sample.rs:12` (one frame late, reads without a fence check); `crates/manifold-gpu/src/metal/frame_fence.rs:59` (`is_completed`) | Coupling needs the fenced form; content-thread fence exposure is the surface design's P2 entry check. |
| Executor repeat region | none on main — `rg -n -i 'substep\|repeat_region' crates/manifold-renderer/src/node_graph -g '*.rs'` hits only FLIP and Box3D native substeps | Genuinely new on main; exists on a paused branch (1.2). |
| Mesh signed distance | none — `rg -l -i 'signed.?distance\|mesh_sdf\|distance_field' crates -g '*.rs'` hits only analytic masks (`R/primitives/mesh_spatial_mask.rs`) | **Correction to the brief:** Box3D hulls and FLIP roles produce `TriangleMesh`, not distance fields. FLIP's level sets live inside its C++ `MeshLevelSet` and are not exposed. The SDF builder is new (D11). |

### 1.2 The MLS-MPM prototype (branch `wave/live-water`, paused)

Peter stopped it on 2026-09-11 "because of cost and visual regressions"; the archival
tag is `codex/water-checkpoints/2026-09-11/current-wip`. It forks main at `4acf10b7c`,
808 main commits ago; `R/execution.rs` has since grown by 2,638 lines, so nothing on it
cherry-picks. Read it for reference only.

| Piece on the branch | Files | What it did |
|---|---|---|
| Substep region seam | `R/substeps.rs` (1,814 lines), commits `8e003cbd6`, `e54db937f`, `7f0938782`, `478eb23de`, `b9630e64d`, `115720c56`, `6a1228c85`, `1f6e12325`, `82006b602` | Boundary ports, region derivation and validation, executor repeat with one extracted step evaluator, freeze membership proof. Green on its own tests. |
| State boundary with clock | `R/primitives/water_state.rs` (845), `9c2c0773f` | Fixed 960 Hz clock, accepted/candidate buffers, reset. |
| Transfer kernels | `shaders/mpm_scatter_mass_momentum.wgsl`, `mpm_scatter_stress.wgsl` (hand kernels), `mpm_grid_velocity_body.wgsl`, `mpm_gather_advect_body.wgsl`, `clear_grid_body.wgsl`, `seed_water_body.wgsl`, `water_validate.wgsl`, `water_commit_body.wgsl`, `water_common.wgsl` | Two scatter passes (mass/momentum, then density/stress from grid mass); signed i32 fixed point at Q = 2^20. The scatters were hand kernels because they carried two atomic outputs (accumulator and status), which generated codegen could not express. |
| Constants | `R/water.rs:110-139` on the branch | 64³ nodes, h = 0.0625 m, ρ0 = 1000, c0 = 10 m/s, Tait exponent 7, dt = 1/960 s, velocity fault at 4 m/s, density fault at 4ρ0. |
| Late pivot | `mac_*` primitives and `water_mac_*` proofs | Moved toward APIC with a MAC pressure projection (FLIP on the GPU) after the look failed. |

| Measurement | Value | Source |
|---|---|---|
| S8 prototype, still water, 1080p | ~42 ms/frame; MPM region at 16 substeps ~15 ms (per-dispatch split inflates it) | BUG-kjxf (water perf direction after the S8 42 ms miss) |
| After native `atomicAdd` fixed point, 720p cutaway | 15.91 ms median / 16.87 ms p95 whole frame, 41,760 particles | BUG-kjxf closeout comment |
| Kinematic faults in ordinary flows | first fault at step 194: 4.007 m/s vs the 4 m/s bound, density 1156.8, acoustic CFL 0.325 | BUG-01vr (water dt-halving gate) |
| dt-halving convergence | trajectory differences do not shrink under refinement (ratio 0.70–0.79) | BUG-01vr |
| MAC pressure probe | 2.06 ms median per projection, 32×8×32 pool, 96 SOR pairs for ≤ 1% divergence residual | BUG-01vr comment 2026-09-10 |
| Look | "gel-like", "rounded sheets", "smooth/sheet-like water persists" even after a Yu–Turk surface | `WORKTREE_HANDOFF.md` on the branch |

**What the prototype taught, and what changes:**

| Prototype choice | Consequence | This design |
|---|---|---|
| c0 = 10 m/s in a 4 m domain | Mach 0.4–0.9 at dam-break speeds; bouncy, gel-like | Stiffness scales with domain size (taichi_elements rule): c ≈ 33 m/s at 4 m, Mach ≤ 0.27 (D4) |
| Density recomputed from grid mass (two scatter passes, Tait γ = 7) | Twice the atomic traffic; noisy density | Volume ratio J carried per particle; one scatter pass (D3) |
| Velocity and density bounds that fault | Normal dam breaks faulted | Only non-finite values fault; velocity is clamped at the CFL limit and counted (D14) |
| dt-halving trajectory gate | Chaotic flow never converges pointwise; weeks spent (BUG-01vr) | Gate on conserved quantities, energy, a still pool and dam-break experiment data (D18) |
| Two atomic outputs per scatter | Hand kernels outside codegen | Exactly one atomic output per scatter atom; diagnostics in a separate per-tick reduction (D14) |
| Screen-space splats, then a density isosurface | Blobby look confounded the solver verdict | The surface design's mesh, identical for FLIP and matter, so P4 compares solvers, not surfaces |

## 2. Decisions

**D1 — Live liquid is GPU MLS-MPM writing the particle-frame seam (Peter, restated).**
FLIP stays for bake and reference; its look is the target. The surface design's seam,
atoms and D10 display clock are reused unchanged.

**D2 — MLS-MPM over APIC or FLIP on the GPU; the pressure solve is the reason.**
FLIP and APIC are incompressible: every step needs a global pressure solve. On the GPU
that is dozens to hundreds of Jacobi or SOR sweeps (the prototype needed 96 SOR pairs for
a 1% residual on a small pool), the sweep count grows with resolution, multigrid is a
project of its own, and two-way coupling needs the body's mass inside that solve (the
integration plan's mass-aware PCG in its phase P8b). MLS-MPM is explicit and local: cost
is linear in particles and substeps, nothing iterates to a tolerance, rigid coupling is a
per-substep exchange, and one solver covers every material Peter listed. Rejected: FLIP
or APIC with a GPU pressure projection.
**Consequences, stated honestly:** MLS-MPM is not cheaper for plain water. At 64³ a GPU
FLIP with two steps per frame is plausibly in the same 5–10 ms band as MLS-MPM at 34
substeps. The choice buys the material zoo, simple coupling and resolution scaling
without a global solve, and pays with weak compressibility (D3) and many small substeps.

**D3 — Water is weakly compressible, tracked by a per-particle volume ratio J.**
Each substep `J ← J·(1 + dt·tr C)` (Taichi `mpm3d.py`), and water stress is the
Kirchhoff pressure `τ = λ·J·(J − 1)·I` with shear modulus zero (taichi_elements). One
scatter pass carries mass, momentum and stress together. Tension (J > 1) is scaled by a
**Cohesion** factor κ ∈ [0, 1], default 0: FLIP's free surface carries no tension and
FLIP is the look. Rejected: Tait EOS on density recomputed from grid mass (the prototype
and WebGPU-Ocean) — a second scatter pass and noisier density. Rejected: taichi's
implicit κ = 1 — cohesive blobs, the gel look.
**Consequences:** J drifts slowly because particle divergence and grid divergence
disagree; the still-pool invariant measures it (section 10). A hydrostatic pool
compresses by ρgH/λ (1.8% at 2 m depth), so the surface sits about 2 cm lower than FLIP's
at that depth.

**D4 — Stiffness scales with domain size; substeps are fixed per setup.**
Inherited from taichi_elements `mpm_solver.py`: `E = 1e6·L` Pa with L the longest domain
side in metres, ν = 0.2, so the water bulk term is `λ = Eν/((1+ν)(1−2ν)) = 2.78e5·L` Pa
and the wave speed `c = √(λ/ρ0) = 16.7·√L` m/s. Because free-fall speed also grows as √L,
the Mach number stays ≈ 0.27 at any domain size. The substep count per 1/60 s tick is

```text
v_est   = √(2 · 9.81 · H)                       H = domain height (m)
dt_f    = (1/3) · dx / (c_max + v_est)           acoustic CFL 1/3
dt_b    = min over coupled dynamic bodies of 0.5 · (dx / c_max) · √(m_b / (ρ0 · A_b · dx))
n       = ceil(tick / min(dt_f, dt_b)),  dt = tick / n,  n ≤ 128
```

`c_max` is the stiffest material's wave speed including hardening bounds (D10); `m_b`
and `A_b` are a coupled body's mass and surface area. At the Dam Break setup (L = H = 4 m,
64 cells, dx = 0.0625 m) this is n = 34. n is recomputed only on setup edits, so the GPU
cost per tick is fixed and predictable on stage. Live gravity above 9.81 m/s² only eats
acoustic headroom (0.33 → 0.42 at 20 m/s², still below mpm88's 0.51). n > 128 is a named
setup error, never a silent cap. Rejected: adapting n from a read-back velocity maximum —
cost that swings mid-show, plus a readback on the uncoupled path. Rejected: the
prototype's fixed 960 Hz.

**D5 — The grid is flat arrays with the lattice on wires; accumulation is signed fixed
point.** Per the surface design's D8. The lattice is the `domain_layout` box grown by 3
nodes per side (taichi `padding = 3`), node (i, j, k) at `min + (i, j, k)·dx`. Arrays:
`grid_accum: Array(i32)` (4 per node: momentum xyz, mass) and `grid: Array(MatterGridNode)`.
Accumulation is `atomicAdd` on i32 in normalized units — mass in `m_unit = 1000·dx³/8` kg,
momentum in `m_unit·dx/dt` — scaled by Q = 2^20 with round-to-nearest per contribution.
Q = 2^20 is the prototype's measured choice (0–0.0125% mass error on its fixtures, where
Q = 4096 gave 2.4–4.8%). Integer addition is order-independent, so the solver is
bit-deterministic on one machine and build. Rejected: `Texture3D` (surface D8; no float
atomics on storage textures in WGSL). Rejected: a float compare-exchange loop (slower in
the prototype, order-dependent). Rejected: Metal float atomics (not portable).
**Consequences:** the lattice arrays are sized from a runtime layout; they are provided
outputs of `node.matter_state` that grow on setup changes (surface design precedent,
`R/primitives/fluid_surface.rs:202-206`), and atoms write them in place through aliased
input/output pairs (`aliased_array_io`).

**D6 — P2G is one fixed-point atomic scatter over unsorted particles in P1.** One thread
per particle adds to its 27 stencil nodes. This is the prototype's proven path with half
its atomics. Block-local accumulation — particles visited in cell-block order through a
permutation, summed in `var<workgroup>` atomics, flushed once per node — is the standard
GPU MPM optimization (Gao et al. 2018) and uses only core WGSL. It is phase P4-opt, entered
only if P4 shows the scatter dominating. The permutation comes from the surface design's
`node.sort_particles_into_cells`, extended to emit an `order: Array(u32)`. **The
`MatterPoint` storage order is never changed** — ids must stay sorted for the seam (D9).
Rejected for now: subgroup (warp) reductions — an optional WGSL feature whose
naga → SPIR-V → MSL and Vulkan support is unverified (Deferred).

**D7 — Substeps run in an executor repeat region.** The historical SubstepBoundary seam
(WATER_SIMULATION_DESIGN.md section 4 (Fixed substeps and graph compiler seam)) fits
unchanged in shape: a boundary node declares ports; the compiler contracts the nodes
between its state outputs and its capture inputs into a region; the executor runs the
boundary once, then the region body `count` times with per-iteration scalars; only the
final state escapes; freeze never fuses across the region border. It is re-implemented on
current main in P0a/P0b with the branch as reference. Rejected: one `mpm_solver` node that
dispatches everything (the no-monolith rule). Rejected: unrolling N copies of the atoms
in the graph (N changes with setup). Rejected: repeating the whole frame per substep.

**D8 — Fixed 60 Hz ticks; live never spirals; export never drops.** Tick = 1/60 s,
equal to Box3D's fixed tick. Live runs at most `ceil(project_frame_interval / tick)`
ticks per display frame (1 at 60 fps, 2 at 30 fps); due time beyond that is dropped,
counted and published on `dropped_seconds`. Export and Record run every due tick and may
wait on GPU fences. Display follows the surface design's D10: s = target − tick for every
solver-time output. Rejected: FLIP's retained unbounded debt for this solver — one tick
costs about a frame of GPU time, so catching up spirals into more missed frames. Under
overload the water plays in slow motion, visibly reported, instead of stalling the show.

**D9 — Particles live in one capacity pool; births append, drains mark, compaction keeps
order.** Each point carries `id` = birth ordinal within an identity epoch; 0 marks an
unused slot. Fills and inflows append at a cursor in lattice order through a prefix scan
(deterministic, no atomic counter). Drains set id 0. Once per tick, when dead slots exceed
1/8 of the used range or the cursor needs room, an order-preserving compaction runs. The
published frame is therefore always id-sorted, which the seam's interpolation needs
(surface D11). When the next birth ordinal would pass `u32::MAX`, the frame renumbers
1..n in current order and bumps the identity epoch (the surface design's section 3.1 rule).

**D10 — Materials are per-particle slots into a table; one solver.** A particle stores a
material slot. `node.matter_materials` packs up to 8 `MatterMaterial` entries and
publishes `max_wave_speed` for D4. `node.matter_to_grid` switches on the slot's model to
compute stress: water (D3), then fixed-corotated jelly, Stomakhin snow, Drucker–Prager
sand, Newtonian honey and viscoplastic lava in P5. Non-fluid models add an optional
`Array(MatterDeformation)` (F rows) updated by `node.matter_update_deformation`. Water
graphs leave it unwired and pay nothing.

**D11 — Boundaries are domain walls plus collider distance lattices built from the
existing role geometry.** Walls: nodes within 3 of a closed face lose velocity into the
face, with taichi_elements' friction rule. Colliders: every `FluidRole` Collider mesh and
every coupled Box3D body (`hull_meshes`) is turned, once per geometry revision, into a
body-local signed-distance lattice by a new CPU builder in `manifold-physics`
(`P/sdf.rs`), packed into one `Array(f32)` atlas and sampled per substep through the
body's pose. Grid nodes inside a collider band take the collider's velocity in the normal
direction and keep friction-limited tangential velocity. The same lattices define fill,
inflow and drain regions. Rejected: a GPU mesh-to-SDF atom — the role geometry is CPU
`TriangleMesh` and would need a second upload path; the CPU builder keeps geometry in the
shared crate FLIP and Box3D already use. Rejected: the paper's colored-distance-field
compatibility test (CPIC proper) for v1 — it exists for thin shells and cutting; every
role and hull here is a closed volume (Deferred, with a thinness warning).

**D12 — Two-way coupling: Box3D in lockstep per tick; each body advanced every substep on
the GPU; the reaction crosses back through a fenced readback.** Section 5 is the
protocol. The paper advances rigid bodies with the MPM substep; Box3D cannot step per
substep without a GPU round trip, so a small GPU body integrator (`node.matter_move_bodies`)
advances each coupled body every substep from Box3D's tick-start state with gravity,
fields and the fluid's reaction. At tick end the fluid's net impulse is read back and
applied to Box3D, which then steps once with contacts.
**How this avoids the integration plan's 16–24× energy gain.** That candidate applied a
full incompressible pressure impulse to a body after a 1/60 s solve that assumed the body
did not respond — the added-mass instability, independent of dt. Here the body responds
inside every substep, and weakly compressible pressure acts as a stiff spring, not a
constraint impulse. Pressing a body of mass m_b and area A_b into the liquid by δ
compresses about one cell layer, so the contact stiffness is k ≈ λ·A_b/dx and the body
oscillates at ω = √(k/m_b) = c·√(ρ0·A_b/(dx·m_b)). Explicit exchange is stable for
ω·dt < 2; the `dt_b` term of D4 keeps ω·dt ≤ 0.5. That derivation is this design's, and
P2b's energy tests at body/liquid density ratios 0.1, 1 and 10 prove it with the same
criterion that rejected FLIP's candidate. Bodies needing n > 128 are rejected at setup
with a named error.

**D13 — Forces and impulses arrive as coarse lattices sampled on the CPU per tick.**
The shared `VectorField` programs (integration plan P6) are sampled at a quarter of the
grid resolution per axis (17³ nodes at 64³) into `Array(f32)` and trilinearly read in the
grid update. Impulses use the existing event queue semantics and a second lattice applied
once, on the first substep of their tick. Rejected: evaluating CPU fields at every grid
node per substep (357,911 nodes × 34 substeps on the content thread).
**Consequence:** force detail is limited to four cells; a field sharper than that is
smoothed.

**D14 — Only non-finite state faults; excess speed is clamped and counted.** Grid
velocity is clamped per component to 0.9·dx/dt (taichi_elements `g2p2g_allowed_cfl`);
`node.matter_stats` reduces non-finite counts, clamp counts, speed, J range, mass,
momentum, energy and fixed-point headroom once per tick. `node.matter_frame` reads that
tick's stats on the GPU and does not publish a frame whose stats show a non-finite value;
the solver then halts with a node error until Reset. Persistent clamping shows as a
"liquid too fast for its substeps" diagnostic. No candidate/accepted double buffer
(40 MB copies per substep). Rejected: the prototype's velocity and density faults.

**D15 — Determinism.** Same machine, build, seed and input stream → bit-identical
particles: integer atomics, fixed reduction trees, seeded hash jitter, Box3D at one
worker. Cross-GPU identity is not claimed.

**D16 — Everything runs on the content thread.** Atoms encode into the normal command
buffer. CPU work per frame: clock, role pose table (≤ 64 bodies), field lattice (4,913
samples), coupling readback. SDF building runs where role geometry preparation runs today
(⚠ VERIFY-AT-IMPL in P2a: read `R/fluid/roles.rs` and `R/physics_mesh.rs`; if that is the
content thread, stop and escalate — a 32³ × 1,000-triangle lattice is ~0.2 s). No new
thread, channel, `Arc<Mutex>` or `Arc<RwLock>`.

**D17 — Live-only until P7.** Matter graphs have no cache mode before the bake phase;
Record and Playback do not exist for them, so nothing silently falls back.

**D18 — Proof by invariants and experiment, never by dt-halving trajectories.** Gates:
exact mass conservation, momentum in free flight, a still pool that settles, dam-break
energy never growing, the dam-break front against Martin & Moyce (1952), GPU against an
f64 CPU reference at small N, determinism. Free-surface flow is chaotic; pointwise
trajectory convergence is not a valid gate (BUG-01vr).

**D19 — The perf gate is Peter's numbers; misses go back to Peter priced.** 64³, ≥ 500k
particles, ≤ 6 ms solver GPU time per 60 Hz frame at the D4 substep count, 1080p scene,
M4 Max; stretch 128³ at 30 Hz, reported not gated. Section 8 shows the gate at the
bandwidth roofline. Levers that change the look (fewer particles, lower stiffness, a
bigger budget, a fused G2P2G exemption) are Peter's call; the one look-neutral lever
(P4-opt) is pre-authorized with an entry condition.

**D20 — Add Fluid switches to matter only after Peter's go (reorder of the brief).**
The brief put Add Fluid parity in P3, before the P4 go/no-go. Switching authoring before
the look call would have to be undone on a no-go. P3a builds full role parity and
matter-shaped presets; the command switch is P4b, entered on Peter's recorded go.
**Dissent recorded for the lead:** if Peter wants Add Fluid on matter earlier for
hands-on play, P4b's brief runs unchanged right after P3a.

**D21 — Existing FLIP scenes stay FLIP.** No migration. After P4b, new Add Fluid domains
are matter domains; FLIP remains in its presets and old projects. Converting a matter
fluid to FLIP for a reference bake is deferred with a trigger.

**D22 — Whitewater and bake are later phases on the same seam.** P6 classifies spray,
foam and bubbles from grid potentials with FLIP Fluids' own constants and publishes the
surface design's whitewater frames. P7 records particle frames per tick and plays them
back through the same surface — the surface design's deferred particle-frame cache made
real for matter graphs.

## 3. Data model and atoms

### 3.1 Records

`crates/manifold-renderer/src/node_graph/matter.rs` (new; test-only f64 reference in
`matter/reference.rs`). Every record is `#[repr(C)]`, `Pod`, with a `KnownItem` channel
spec and a compile-time size assert, following `WaterParticle` on the branch and
`FluidParticle` in the surface design.

```rust
/// One material point. 80 bytes.
pub struct MatterPoint {
    pub position: [f32; 3],   // scene metres
    pub id: u32,              // birth ordinal in the identity epoch; 0 = unused slot
    pub velocity: [f32; 3],   // m/s
    pub volume_ratio: f32,    // J = current / rest volume
    pub affine_x: [f32; 4],   // C row 0 (1/s); w = plastic volume ratio Jp (1 when unused)
    pub affine_y: [f32; 4],   // C row 1; w = rest volume V0 (m³)
    pub affine_z: [f32; 4],   // C row 2; w = material slot (small integer as f32)
}
// Specs: position Vec3F, id U32, velocity Vec3F, volume_ratio F32, affine_x/y/z Vec4F.
// Mass = V0 · density of the slot's material.

/// Resolved grid node. 16 bytes.
pub struct MatterGridNode {
    pub velocity_mass: [f32; 4], // m/s xyz; w = mass kg (0 = empty)
}

/// Deformation gradient for non-fluid materials (P5). 48 bytes.
pub struct MatterDeformation {
    pub f_x: [f32; 4], pub f_y: [f32; 4], pub f_z: [f32; 4], // F rows; w = 0
}

/// One material slot. 48 bytes. Up to MAX_MATERIALS = 8.
pub struct MatterMaterial {
    /// x = model (0 water, 1 jelly, 2 snow, 3 sand, 4 viscous, 5 viscoplastic),
    /// y = rest density kg/m³, z = μ0 Pa, w = λ0 Pa.
    pub model_density_mu_lambda: [f32; 4],
    pub params_a: [f32; 4], // model-specific, section 4.4
    pub params_b: [f32; 4], // model-specific, section 4.4
}

/// A collider or coupled body during a tick. 128 bytes. Up to MAX_BODIES = 64.
pub struct MatterBody {
    pub position_inv_mass: [f32; 4],   // world centre of mass; w = 1/m (0 = prescribed)
    pub rotation: [f32; 4],            // quaternion xyzw
    pub linear_velocity: [f32; 4],     // w = friction
    pub angular_velocity: [f32; 4],    // w = role (0 collider, 1 fill, 2 inflow, 3 drain)
    pub inv_inertia_x: [f32; 4],       // world inverse inertia rows at tick start
    pub inv_inertia_y: [f32; 4],
    pub inv_inertia_z: [f32; 4],
    pub accel_shape: [f32; 4],         // xyz predicted external acceleration; w = shape index
}

/// Body-local distance lattice descriptor. 32 bytes.
pub struct MatterShape {
    pub origin_spacing: [f32; 4],      // local lattice min xyz; w = spacing (m)
    pub dims_offset: [u32; 4],         // nodes x/y/z; w = offset into the SDF atlas
}
```

Per-body fluid impulse accumulator: `Array(i32)`, 8 per body (linear xyz, angular xyz,
2 padding), fixed point in units of `m_unit·dx/dt` and `m_unit·dx²/dt`.

Per-tick stats: `Array(u32)`, 16 words — non-finite count, clamp count, live count, max
speed (f32 bits), min J, max J, max |accumulator| (headroom), total mass, momentum xyz,
kinetic, potential and elastic energy (f32 bits), tick index. Sums use a fixed reduction
tree, so they are deterministic.

### 3.2 Atoms and ports

Scalars are `ScalarF32`; every numeric param is port-shadowed (DECOMPOSING_GENERATORS.md section 6.2 (Extend before you build)). "Lattice wires" means `grid_bounds: Transform`,
`grid_nodes_x/y/z`, `cell_size`.

| Atom | Inputs → outputs | Phase |
|---|---|---|
| `node.matter_state` (substep boundary) | `seed: Array(MatterPoint)`, capture `in: Array(MatterPoint)`, capture `stats_in: Array(u32)`, `domain: Transform`, `wave_speed`, `body_substep_limit` (optional), `advance` (optional), `speed`, `reset` → `out: Array(MatterPoint)`; provided `grid_accum: Array(i32)`, `grid: Array(MatterGridNode)`; lattice wires; per-iteration `step_dt`, `step_index`, `step_count`, `substep_in_tick`, `tick_start` (1 on a tick's first substep), `tick_end`; `simulation_time`, `dropped_seconds`, `substeps_per_tick`, `live_count`, `fault`. Params: `resolution` (Int, 64, 8–256), `grid_budget_mcells`, six closed-face Bools, `seed`, `max_capacity` | P1 |
| `node.matter_materials` | params per slot → `materials: Array(MatterMaterial)`, `max_wave_speed` | P1 (water), P5 |
| `node.matter_fill` | `volume: Transform`, lattice wires, `materials`, P3a: `roles: Array(MatterBody)`, `shapes`, `sdf` → `seed: Array(MatterPoint)`. Params `material_slot`, `seed`, `max_capacity` | P1 box, P3a meshes |
| `node.zero_array` | `in: Array(i32)` → `out` (aliased) | P1 |
| `node.matter_to_grid` | `points`, `materials`, lattice wires, `step_dt`, optional `deformation: Array(MatterDeformation)`, `accum: Array(i32)` → `accum_out` (aliased, the one atomic output) | P1 |
| `node.matter_grid_update` | `accum`, `grid` (aliased target), lattice wires, `step_dt`, `gravity_x/y/z`, closed-face mask, optional `bodies`, `shapes`, `sdf`, `forces`, `impulses`, `tick_start` → `grid_out` | P1 walls, P2a colliders, P3b forces |
| `node.grid_to_matter` | `points`, `grid` (BufferGather), lattice wires, `step_dt` → `points_out` | P1 |
| `node.matter_stats` | `points`, `grid`, `accum`, `materials`, `tick_end` → `stats: Array(u32)` | P1 |
| `node.matter_frame` | `points`, `stats`, `materials`, lattice wires, `solid` (optional), `simulation_time` → the surface seam outputs: `particles_a`, `particles_b`, `count_a/b`, `identity_a/b`, `solid_a/b`, `grid_bounds`, `grid_nodes_x/y/z`, `blend`, `span` | P1 |
| `node.matter_roles` | `role_0..role_63: FluidRole`, coupled-scene hooks, lattice wires → `bodies: Array(MatterBody)`, `shapes: Array(MatterShape)`, `sdf: Array(f32)`, `body_substep_limit`, `advance` | P2a, P2b |
| `node.matter_move_bodies` | `bodies`, `reaction: Array(i32)`, `step_dt`, `substep_in_tick` → `bodies_out` | P2a |
| `node.matter_solid_distance` | `bodies`, `shapes`, `sdf`, lattice wires, closed-face mask → `solid: Array(f32)` | P2a |
| `node.matter_body_reaction` | `accum`, `grid`, `bodies`, `shapes`, `sdf`, lattice wires, `reaction: Array(i32)` → `reaction_out` (aliased atomic) | P2b |
| `node.matter_emit` | `points`, `bodies`, `shapes`, `sdf`, lattice wires, `tick_start`, `materials` → `points_out` | P3a |
| `node.matter_drain` | `points`, `bodies`, `shapes`, `sdf`, lattice wires → `points_out` | P3a |
| `node.matter_compact` | `points`, `tick_end` → `points_out` | P3a |
| `node.matter_forces` | `acceleration_field: VectorField`, impulse hooks, lattice wires → `forces: Array(f32)`, `impulses: Array(f32)` | P3b |
| `node.matter_update_deformation` | `points`, `deformation`, `materials`, `step_dt` → `deformation_out` | P5a |
| whitewater atoms, frame cache | section 11, P6 and P7 | P6, P7 |

The graph ships as one node group, **"Live Matter"**, per GROUPING_GRAPHS.md: fill and
materials feed the state, the region body is `zero_array → matter_move_bodies →
matter_to_grid → matter_grid_update → matter_body_reaction → grid_to_matter →
matter_drain → matter_emit → matter_compact → matter_stats`, and `matter_frame` feeds the
surface design's "Liquid Surface" group.

### 3.3 Ownership and threads

The content thread owns every runtime value. `node.matter_state` owns the persistent point
buffer, the provided lattice arrays, the clock and the stats readback ring.
`node.matter_roles` owns prepared SDF lattices (keyed by geometry revision), the body
table, the coupled `RigidSimulation` for a coupled pair, and the reaction readback ring.
`physics_world` in coupled mode republishes the accepted rigid frame as it does for FLIP.
Serialized state is the graph JSON only; reload restarts the simulation (as FLIP and
Box3D do).

## 4. Numerical recipe

### 4.1 One substep

Notation: x_p, v_p, C_p, J_p, V0_p per point; m_p = V0_p·ρ(slot); dx cell size;
weights are quadratic B-splines per axis, with `q = (x − lattice_min)/dx`,
`base = floor(q − 0.5)`, `f = q − base`,
`w0 = 0.5(1.5 − f)²`, `w1 = 0.75 − (f − 1)²`, `w2 = 0.5(f − 0.5)²`, over the 27 nodes
`i = base + {0,1,2}³`, `d_i = (i·dx + lattice_min) − x_p`.

1. **Clear** `grid_accum` (`node.zero_array`).
2. **Move bodies** (`node.matter_move_bodies`, P2a+): prescribed bodies take the pose
   interpolated between their tick-start and tick-end poses at `substep_in_tick`
   (lerp translation, slerp rotation) and the matching velocities; dynamic coupled
   bodies apply the previous substep's reaction (Δv = J·m⁻¹, Δω = I⁻¹·L) plus
   `accel·dt`, then integrate position and rotation.
3. **P2G** (`node.matter_to_grid`): stress τ_p from the slot's model (4.4). For each node,
   `mass += w·m_p` and `momentum += w·(m_p·v_p + (m_p·C_p − dt·V0_p·(4/dx²)·τ_p)·d_i)`
   (MLS-MPM fused stress term, Hu 2018; `4/dx²` is Dp⁻¹ for quadratic splines; Taichi
   `mpm3d.py` and `mls-mpm88` use the same factor). Fixed point per D5.
4. **Grid update** (`node.matter_grid_update`), for each node with mass > 0:
   `v = momentum/mass + dt·(g + a_field(x_i))`, plus the impulse lattice once when
   `tick_start = 1`. Walls: for each closed face within 3 nodes, the normal component
   into the face becomes 0 and the tangential part becomes
   `t̂·max(0, |t| + v_n·friction)` (taichi_elements). Colliders: for each body whose
   distance φ at the node is below 0 (sampled through the body pose from its lattice),
   with n = ∇φ/|∇φ| and v_rel = v − v_body(x_i): if v_rel·n < 0, remove the normal part
   and apply the same friction rule, then `v = v_body + v_rel'`. Clamp each component to
   ±0.9·dx/dt and count clamps. Write `grid`.
5. **Reaction** (`node.matter_body_reaction`, P2b): for nodes a coupled body touched,
   atomically add `mass·(v_before − v_after)` and `(x_i − x_com) × mass·(v_before − v_after)`
   to that body's accumulator.
6. **G2P** (`node.grid_to_matter`): `v_p = Σ w·v_i`,
   `C_p = (4/dx²)·Σ w·v_i ⊗ d_i`, `x_p += dt·v_p`, `J_p ← J_p·(1 + dt·tr C_p)`.
7. **Deformation** (P5, non-fluid slots): `F ← (I + dt·C_p)·F`, then the model's return
   mapping.

Once per tick, gated by `tick_start` or `tick_end` (skipped aliased dispatches call `mark_gpu_accessed`, per FREEZE_COMPILER_MAP.md section 9 (Executor contracts fusion leans on)): drain (P3a), emit (P3a), compaction (P3a), stats.

### 4.2 Seeding

Fills place 2 × 2 × 2 points per cell (8 per cell, the FLIP Fluids upstream density, so
both solvers carry the same count at the same grid), each jittered inside its sub-cell by
a hash of (seed, lattice index), V0 = dx³/8, J = 1, C = 0, velocity from the role. Points
closer than 1.5 cells to the lattice edge are not seeded; walls keep them inside.

### 4.3 Fixed-point encoding and headroom

`encode(x) = round(x · Q)` with Q = 2^20 after normalizing by `m_unit` and `dx/dt`.
Per-node sums stay far below 2^31 whenever J ≥ 0.1; `matter_stats` records the largest
|accumulator| every tick and the invariant `matter_fixed_point_headroom` requires it below
2^30 on the Dam Break.

### 4.4 Constitutive branches

| Model | Stress | Parameters (defaults) |
|---|---|---|
| 0 water | `τ = λJ(J−1)I` for J < 1; `κ·λJ(J−1)I` for J ≥ 1 | λ = 2.78e5·L Pa; ρ0 = 1000; κ (Cohesion) = 0 |
| 1 jelly (P5a) | fixed corotated `τ = 2μ(F−R)Fᵀ + λJ(J−1)I` | E = 1e6·L, ν = 0.2, μ and λ scaled by 0.3 |
| 2 snow (P5b) | fixed corotated with hardening `e^{ξ(1−Jp)}`, singular values clamped to [1−θc, 1+θs], Jp clamped to [0.6, 20] | θc = 2.5e-2, θs = 7.5e-3, ξ = 10, E0 = 1.4e5 Pa, ν = 0.2, ρ = 400 |
| 3 sand (P5c) | Drucker–Prager return mapping on Hencky strain, α = √(2/3)·2 sin φ / (3 − sin φ) | φ = 45°; E, ν, ρ per Klár 2016 |
| 4 viscous (P5d) | water pressure plus `μ_v·(C + Cᵀ)` | μ_v in Pa·s |
| 5 viscoplastic (P5e) | water pressure plus Bingham/Herschel–Bulkley deviatoric stress | yield stress, consistency, flow index |

### 4.5 Constants and their sources

Tuning is inherited, not rediscovered. Sources fetched 2026-09-29: taichi_elements
`engine/mpm_solver.py` (master); Taichi `python/taichi/examples/simulation/mpm3d.py`
(master); `yuanming-hu/taichi_mpm` `mls-mpm88-explained.cpp`.

| Quantity | Value here | Source |
|---|---|---|
| Transfer weights, 27-node stencil, `base = floor(q − 0.5)` | quadratic B-spline | Hu 2018; `mpm3d.py`; `mls-mpm88` |
| MLS/APIC inverse moment Dp⁻¹ | 4/dx² | `mpm3d.py` (`4 * E * p_vol * (J−1) / dx**2`); `mls-mpm88` (`4 * inv_dx * inv_dx`) |
| Stress in P2G | `−dt·V0·(4/dx²)·τ`, added to `m·C` as the affine term | `mpm3d.py`; taichi_elements (`affine = stress + mass * C`) |
| J update | `J ← J(1 + dt·tr C)` | `mpm3d.py` |
| Water stress | μ = 0, `τ = λJ(J−1)I` | taichi_elements (`mu = 0.0` for water) |
| Young's modulus, Poisson | E = 1e6·L Pa, ν = 0.2 | taichi_elements (`1e6 * size * E_scale`, `nu = 0.2`) |
| Lamé | μ0 = E/(2(1+ν)), λ0 = Eν/((1+ν)(1−2ν)) | taichi_elements |
| Rest density, water | 1000 kg/m³ | taichi_elements `p_rho` |
| Wall padding | 3 nodes; velocity into a wall zeroed | taichi_elements `padding=3`; `mpm3d.py` `bound = 3` |
| Wall and collider friction | `t̂·max(0, |t| + v_n·friction)` | taichi_elements surface colliders |
| Acoustic CFL | 1/3 | taichi_elements default dt at L = 1 (`2e-2·dx/size` gives c·dt/dx = 0.33); bracketed by `mpm3d.py` (0.26) and `mls-mpm88` (0.51) |
| Grid velocity clamp | 0.9·dx/dt per component | taichi_elements `g2p2g_allowed_cfl = 0.9` |
| Points per cell | 8 | FLIP Fluids upstream seeding (parity); `mpm3d.py` uses 2, noted |
| Fixed-point scale | Q = 2^20 | prototype S1 measurement (section 1.2) |
| Jelly scale | μ, λ × 0.3 | taichi_elements (`h = 0.3` for elastic) |
| Snow θc, θs, ξ, Jp clamp | 2.5e-2, 7.5e-3, 10, [0.6, 20] | `mls-mpm88` constants (from Stomakhin et al. 2013); taichi_elements uses θs = 4.5e-3, noted and not taken |
| Snow E0, ν, ρ | 1.4e5 Pa, 0.2, 400 kg/m³ | Stomakhin et al. 2013 reference parameters. ⚠ VERIFY-AT-IMPL against the paper's parameter table before P5b |
| Sand φ, α | 45°, formula above | taichi_elements (`friction_angle = radians(45)`) |
| Sand E, ν, ρ | per Klár et al. 2016 | ⚠ VERIFY-AT-IMPL from the paper before P5c |
| Viscous explicit limit | dt ≤ ρ·dx²/(6·μ_v) joins D4 | standard explicit diffusion bound, P5d |
| Viscoplastic model | Bingham / Herschel–Bulkley | Yue et al. 2015. ⚠ VERIFY-AT-IMPL before P5e |
| Body substep term | `0.5·(dx/c)·√(m_b/(ρ0·A_b·dx))` | derived in D12; proven by P2b tests |
| Whitewater rates and energies | wavecrest 175, turbulence 175, energy 0.1–60 | FLIP Fluids defaults as authored in `WaterDamBreak.json` |
| Gravity | authored (World), default (0, −9.81, 0) | the integration plan's first committed shared API |

Worked numbers at the Dam Break setup (L = H = 4 m, 64 cells): dx = 0.0625 m,
λ = 1.11e6 Pa, c = 33.3 m/s, v_est = 8.9 m/s, dt_f = 4.94e-4 s, n = 34, dt = 4.90e-4 s.
Test `matter_substep_rule_matches_worked_example` pins this.

## 5. Coupling protocol

Box3D steps once per 1/60 s tick; the fluid's substeps see each body move every substep;
the fluid's net impulse for tick k reaches Box3D before Box3D steps tick k. With display
one tick behind (surface D10), the content thread never waits.

Per display frame N, inside the contracted coupled group (liquid side first, as
`prepare_coupled_scenes` orders it today):

1. `node.matter_roles` checks the reaction ring slot of fluid tick k (encoded in frame
   N−1) with `FrameFence::is_completed`. Not complete → set `advance = 0`; the pair holds;
   the frame republishes the previous pair. Lag grows and is reported.
2. Complete → read the per-body accumulators, convert to world impulses, and run
   `RigidSimulation::advance_with_coupling` with `AdvancementPolicy::Worker { max_ticks: 1 }`
   and a `StepCoupling` implementation (`MatterCoupling`) whose single `exchange` applies
   those impulses through `PhysicsWorld::apply_impulses` and whose `finish` captures each
   body's pose, velocities, inverse mass, world inverse inertia and predicted external
   acceleration (`PhysicsWorld::dynamics`, as FLIP's exchange does). Box3D steps tick k
   with its own contact substeps.
3. Upload the body table for fluid tick k+1 (poses at the end of Box3D tick k) and set
   `advance = 1`. `node.matter_state` schedules exactly one tick.
4. The GPU runs fluid tick k+1: every substep moves the bodies (4.1 step 2) and
   accumulates reaction (step 5). The reaction slot for tick k+1 is ready for frame N+1.
5. Presentation at s = t_A: the frame's `particles_a` is the fluid at the end of tick k,
   and the rigid frame accepted in step 2 is Box3D at the end of tick k. They match.

The GPU body integrator's free-flight baseline must equal Box3D's integration of the same
gravity and fields, and the read-back impulse contains only the fluid's reaction, so
nothing is counted twice. Uncoupled scenes skip steps 1–3 and are free-running under D8.

**Consequences, stated honestly:** a coupled scene advances at most one tick per display
frame, so at a 30 fps project it runs at half speed live (export is unaffected); a
missed readback leaves a permanent one-tick lag until Reset; Box3D contacts act once per
tick while the fluid feels the body every substep, so a body pinned against a wall by
water can jitter by up to one tick of fluid push.

## 6. Section 2.5 audit and codegen classification

Per DECOMPOSING_GENERATORS.md section 2.5 (Precondition: audit by analogy before workflow step 1). Survey: `rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/ -g '*.rs'` (322 registered type ids at `c8961489d`). Reference presets read end to end:
`WaterDamBreak.json` (FLIP node, column Transform, moving box, whitewater wiring),
`FluidSim3D.json` (particles into a flat 3D accumulator, fixed-point resolve).

| Candidate | Finding | Shape and argument |
|---|---|---|
| Grid clear | **New**, generic — `node.zero_array` | No clear or fill atom exists (`rg` for clear/fill/zero type ids returns none). Reusable for any accumulator (grid, body reaction, future histograms). |
| P2G | **New** — `node.matter_to_grid` | `node.draw_particles_3d` splats nearest-voxel energy in wrapped unit space with one u32; P2G needs a 27-node signed stencil and the constitutive term. |
| Grid update | **New** — `node.matter_grid_update` | No per-node velocity-resolve atom. |
| G2P | **New** — `node.grid_to_matter` | No gather from a lattice to arbitrary scene-space points with an affine output. `node.sample_volume_at_particles` samples a `Texture3D` for 64-byte `Particle` in unit space. |
| State boundary | **New**, precedent `node.array_feedback` | `array_feedback` is `Particle`-only and frame-delayed, not a substep boundary with a clock. |
| Initial fill | **New** — `node.matter_fill` | `node.spawn_particles` emits unit-space `Particle` at hashed positions; the fill needs lattice points inside a box or SDF in scene metres. |
| Emit, drain, compaction | **New** — three atoms | `node.spawn_from_mesh` is the scan-and-place precedent (`BarrieredReduction`), not reusable for SDF regions and id bookkeeping. |
| Stats reduction | **New** — `node.matter_stats` | Precedent `peak`/`luminance` (workgroup reductions). |
| Frame publication | **One wire away** — `node.matter_frame` writes the surface seam's exact outputs | The ports are specified by the surface design's section 3.2; only the producer is new. |
| Collider SDF | **New** CPU module `P/sdf.rs`, consumed by `node.matter_roles` | No mesh SDF exists (section 1.1 correction). Input geometry is one wire away: `FluidRole` meshes and `hull_meshes`. |
| Role consumption | **One wire away** — same `FluidRole` wires FLIP consumes | Scene Panel role authoring carries over unchanged. |
| Body integrator | **New** — `node.matter_move_bodies` | Box3D is CPU and steps per tick; nothing integrates rigid bodies on the GPU. |
| Reaction accumulation | **New** — `node.matter_body_reaction` | — |
| Solid lattice for the surface | **New** — `node.matter_solid_distance` | The surface design's `solid_a/b` come from FLIP's native capture; a GPU producer needs its own. |
| Forces and impulses | **One wire away** — `VectorField` wires and the existing impulse hooks, sampled by `node.matter_forces` | `SampledField` is the CPU-side precedent. |
| Material table | **New** — `node.matter_materials` | — |
| Spatial sort (P4-opt) | **One wire away** — surface design `node.sort_particles_into_cells` plus an `order` output | Not built yet; P4-opt entry checks it. |
| One `mpm_solver` node | **Forbidden** | DECOMPOSING_GENERATORS.md section 1.1 (No fused single-effect or single-generator monoliths). |

Sixteen new atoms through P3b, plus the CPU SDF module. That is the honest count.

| Atom | Class | Proof |
|---|---|---|
| `zero_array`, `matter_grid_update`, `grid_to_matter`, `matter_move_bodies`, `matter_solid_distance`, `matter_drain`, `matter_update_deformation` | Barrier-free per element: `wgsl_body` + `fusion_kind` + `input_access` (`BufferGather` for lattice, SDF and body reads), pipeline from `standalone_for_spec::<Self>()` | Value `gpu_tests` against CPU-computed expected output; fused-vs-unfused proof for every pair `graph-tool fusion` places in one region. Aliased in-place outputs follow the existing `aliased_array_io` contract. |
| `matter_to_grid`, `matter_body_reaction` | Atomic scatter, exactly one atomic output each, declared exactly as `scatter_particles_3d.rs:95-98` (Boundary, `atomic_outputs`, standalone codegen). ⚠ VERIFY-AT-IMPL the `boundary_reason` that precedent carries | Values against the f64 reference with the fixed-point tolerance; determinism across two dispatch orders. |
| `matter_emit`, `matter_compact`, `matter_stats` | Exempt, exclusion 1 of the ADDING_PRIMITIVES.md "The codegen path is mandatory" scope test (multi-pass scan / barriered reduction), `standalone_for_boundary_spec` | Values against CPU scans and sums, including sizes 1, 255, 256, 257 and 2²⁰+3. |
| `matter_state`, `matter_frame` | Exempt, exclusion 2 (cross-frame state) | Clock and ring unit tests; frame value tests. |
| `matter_materials`, `matter_roles`, `matter_forces` | Exempt, exclusion 3 (CPU bridges) | CPU unit tests; upload round-trip GPU test. |

Helper functions shared by several bodies (stencil weights, fixed-point encode) are
duplicated textually with an atom-specific prefix and pinned equal by a source test,
unless read-back finds that fused codegen already namespaces member helpers (⚠
VERIFY-AT-IMPL in P1: read `R/freeze/codegen/fused.rs`). A body the codegen cannot express
is BLOCKED: file a `bd` bug naming the missing read path and declare
`boundary_reason: Blocked` — never a quiet exemption.

## 7. Plausible-wrong turns, forbidden by name

- You will want one `mpm_solver` node that loops substeps inside `run()`. No — D7.
- You will want a float compare-exchange loop because `atomicAdd` on f32 is missing. No —
  fixed point, D5.
- You will want to recompute density from grid mass each substep, as the prototype did.
  No — J tracking, D3.
- You will want velocity or density bounds that fault. No — the prototype faulted on
  normal dam breaks; D14.
- You will want to add damping when the water looks springy. No — stiffness and substeps
  are Peter's levers, D19.
- You will want a dt-halving trajectory test. No — D18.
- You will want to read back the reaction and wait for it. No — fenced ring, hold on
  miss, section 5.
- You will want to step Box3D per substep, or apply the reaction after the fact without
  the GPU body integrator. No — the latter is the rejected 16–24× candidate; D12.
- You will want `Texture3D` for the grid. No — D5.
- You will want to sort `MatterPoint` storage for locality. No — ids must stay sorted;
  permutation only, D6.
- You will want a second scatter output for diagnostics. No — one atomic output; stats
  are a separate reduction, D14.
- You will want to copy the prototype's constants (c0 = 10, γ = 7, 960 Hz, 4 m/s). No —
  section 4.5.
- You will want a second fluid inspector or new role kinds. No — `FluidRole` wires, D11.
- You will want subgroup operations for P2G. No — Deferred.
- You will want to fall back to FLIP when the GPU solver faults or lags. No — named error
  or visible slow motion, D8 and D14.

## 8. Cost and the roofline

Per substep: 4 dispatches (clear, P2G, grid update, G2P), plus 2 when coupled; per tick,
4 more (drain, emit, compaction, stats). At n = 34: about 140 dispatches per frame
uncoupled and 210 coupled, plus the surface chain.

Memory traffic per frame at the gate (bytes from DRAM, perfect caching of the grid):

| Term | Value |
|---|---|
| Points × substeps | 500,000 × 34 |
| Point traffic per point-substep | 176 B (P2G reads 80; G2P reads 16, writes 80) |
| Lattice nodes (64 cells + 1 + 2 × 3 padding per axis) | 71³ = 357,911 |
| Lattice traffic per substep | ~48 B per node (clear 16, resolve read 16 and write 16) |
| Total per frame | 2.99 GB + 0.58 GB = 3.58 GB |
| At 546 GB/s (M4 Max, 40-core GPU) | **6.55 ms** |
| At 410 GB/s (M4 Max, 32-core GPU) | **8.72 ms** |
| Atomic adds per frame (not in the bytes above) | 500,000 × 34 × 108 = 1.84e9; Apple GPU atomic throughput is unmeasured here |

**The 6 ms gate is at or beyond the bandwidth roofline before atomics and dispatch
overhead.** Real kernels reach 40–70% of peak bandwidth, so expect 10–20 ms. The 128³
stretch is 16× the work per simulated second (8× points, 2× substeps per tick) and is out
of reach; P4 reports it. For scale, the only published real-time MLS-MPM numbers found
are WebGPU-Ocean (about 100,000 points on integrated graphics, about 300,000 on desktop
GPUs, 2 steps per frame at a timestep its author reports as occasionally unstable) and
Zhao et al. 2021 (1.33M snow points at 68.5 fps on four V100s). The brief's "about 1M
particles at 60 fps on M-series" was not found in any source (section 14, R1).

Levers, priced for Peter's P4 call:

| Lever | Effect | Look cost |
|---|---|---|
| P4-opt block-local P2G (pre-authorized) | Removes most global atomics; no bandwidth change | none |
| 4 points per cell (≈ 250k) | ~0.5× | coarser splashes, thinner sheets tear sooner |
| Stiffness ×0.25 (c 33 → 17 m/s, n 34 → 21) | ~0.6× | Mach ≈ 0.5, visibly springy water |
| Budget 6 → 12 ms | none | frame budget for the rest of the show |
| Fused G2P2G kernel (Zhao 2021) | fewer dispatches, ~10% bytes | none, but needs a no-monolith exemption |

**Instrument consequences:** one tick of display latency (D10); slow motion instead of
lag under overload (D8); fixed per-setup cost (D4) so a scene that holds in rehearsal
holds on stage; a coupled scene at 30 fps runs at half speed (section 5).

## 9. FLIP feature map

Every feature in FLUID_ENGINE_INTEGRATION_PLAN.md is ported, deferred with a trigger, or
FLIP-only.

| FLIP feature | Here |
|---|---|
| Axis-aligned domain Transform, six closed faces, resolution, grid budget | Ported P1 (same `domain_layout`) |
| Legacy `initial_volume` box | Ported P1 (`matter_fill.volume`) |
| Mesh roles: Initial Fill, Inflow (velocity, inherit motion), Outflow, Collider (friction), enable | Ported P3a (fills, inflow, drain), P2a (colliders) |
| Legacy `emitter`/`obstacle` Transform inputs | FLIP-only (legacy presets) |
| Gravity XYZ, Simulation Speed, Reset, Seed (World shared) | Ported P1 |
| Liquid density authoring for coupling | Ported P2b (material slot density) |
| Shared force fields and impulses, 24/30/60 fps input equality | Ported P3b |
| Two-way Box3D coupling, paired presentation | Ported P2b |
| Viscosity | Ported P5d (honey) |
| Surface tension (native) | FLIP-only; the matter Cohesion fake is D3; real tension Deferred |
| Whitewater (foam, bubbles, spray) | Ported P6 |
| CPU mesher, Surface Detail, mesh smoothing, surface particle scale | FLIP-only; the surface design's group replaces them for matter |
| PIC/FLIP `transfer` blend | FLIP-only (MLS-MPM is APIC-equivalent) |
| Record/Playback caches v3–v10, takes, cache identity | FLIP-only until P7; P7 adds a matter frame cache |
| Quality tiers (integration plan P10) | Deferred until matter is the Add Fluid default (P4b) and a tier request exists |
| Custom GPU fields (integration plan P11) | Deferred with that plan's trigger |
| Add Fluid, Scene Panel role assignment, domain gizmo | P4b switches Add Fluid; role assignment carries over through `FluidRole` |

## 10. Invariants and enforcement

| Invariant | Enforcement |
|---|---|
| GPU transfers match the f64 reference at small N | `matter_transfer_matches_reference` (one substep, 512 points, 16³: positions within 1e-5 m, velocities within 2e-5 m/s) and `matter_hundred_substeps_match_reference` (affine field fixture) |
| Mass is exact; grid mass matches particle mass | `matter_grid_mass_matches_particle_mass` (relative 1e-5 per substep) |
| Momentum is conserved in free flight | `matter_momentum_conserved_free_blob` (zero gravity, no walls touched, relative change ≤ 1e-4 over 60 ticks) |
| A still pool settles | `matter_still_pool_settles` (after 5 s: mean speed < 0.01 m/s, max < 0.1 m/s; mean J in the bottom quarter matches 1 − ρgd/λ within 20%) |
| J does not drift | `matter_still_pool_volume_stable` (60 s: mean J within 1% of its 5 s value) |
| Dam-break energy never grows | `matter_dam_break_energy_bounded` (kinetic + potential + elastic ≤ 1.01 × initial at every tick) |
| Dam-break front matches experiment | `matter_dam_break_front_matches_martin_moyce` (column width a = 0.5 m, height 1.0 m, dry floor; front Z = x/a against Martin & Moyce 1952 for T in [1, 3], within 10%). ⚠ VERIFY-AT-IMPL: transcribe the aspect-2 series and the paper's T definition, with the citation, into the test |
| Determinism | `matter_deterministic_under_seed` (two runs, 120 ticks, bit-identical points); `matter_seed_changes_jitter` |
| Fixed-point headroom | `matter_fixed_point_headroom` (Dam Break, max accumulator magnitude < 2^30) |
| A non-finite tick is never published | `matter_nonfinite_tick_not_published` (injected NaN: frame unchanged, node error, halts until Reset) |
| Substep rule | `matter_substep_rule_matches_worked_example` (n = 34 at L = H = 4, 64 cells); `matter_substeps_over_limit_rejected` |
| Live never spirals; export never drops | `matter_live_caps_ticks_per_frame` (dropped time reported); `matter_export_runs_every_tick` |
| Frames are id-sorted, ids unique in an epoch | `matter_frame_ids_strictly_increasing` through fill, emit, drain, compaction; `matter_identity_epoch_renumbers_near_limit` |
| Collider penetration bounded | `matter_collider_penetration_bounded` (rotating box through a pool: particle φ ≥ −0.5·dx) |
| Coupling: hydrostatics, floating, energy, baseline, pairing | `matter_coupling_hydrostatic_force` (within 5%), `matter_coupling_floating_equilibrium` (density 0.5 settles at the waterline ± 0.5·dx), `matter_coupling_energy_light_body` (ratios 0.1/1/10: body energy never above 1.01 × initial total over 8 ticks), `matter_coupling_free_flight_matches_box3d` (no liquid: reaction 0, end state within 1e-5), `matter_coupling_presentation_shares_display_time` |
| Coupled pair never blocks live | `matter_coupled_holds_when_reaction_pending`; negative gate: `rg -n 'wait_until_completed\|commit_and_wait' crates/manifold-renderer/src/node_graph/primitives/matter_*.rs` returns nothing |
| Forces and impulses | `matter_impulse_once_per_tick_across_substeps`; `matter_force_lattice_matches_field`; `matter_input_stream_24_30_60` |
| Every barrier-free atom on codegen | the existing classify source scans plus each atom's value test; `graph-tool fusion` output recorded per phase |
| No new shared locks | negative gate: `git diff origin/main -- crates \| rg '^\+.*Arc<(Mutex\|RwLock)'` returns nothing |
| Solver budget | `matter_solver_perf` (P4), p95 reported against 6 ms |

## 11. Phasing

Test scope for every phase: focused crate tests and clippy on touched crates. CPU tests
carry a `matter_` prefix: `cargo nextest run -p manifold-renderer matter_`. GPU phases add
`scripts/gpu_proofs_gate.py --filter matter_` (cargo test, never nextest). Verify once,
at the end of the phase.

### P0a — Substep regions: types and compiler

- **Entry state:** `rg -n -i 'SubstepBoundary' crates/manifold-renderer/src` returns
  nothing. Re-derive the plan-consumer inventory: `rg -n 'plan.steps\(\)|late_capture_step_indices|hoistable|persistent_resources' crates/manifold-renderer/src`; write the list into the phase notes.
- **Read-back:** WATER_SIMULATION_DESIGN.md section 4 (Fixed substeps and graph compiler seam) whole; D7; the branch commits `8e003cbd6`, `e54db937f`, `7f0938782`, `478eb23de`, `b9630e64d` via `git show`; FREEZE_COMPILER_MAP.md section 9 (Executor contracts fusion leans on). Restate: regions are compile-time; no nesting; only final outputs escape.
- **Deliverables:** `R/substeps.rs` with `SubstepBoundaryPorts`, `SubstepResultPorts`,
  `SubstepRegion` as the historical design pins them; `fn substep_boundary(&self) ->
  Option<SubstepBoundaryPorts>` on `EffectNode` and `Primitive` (default `None`);
  `ExecutionPlan::substep_regions()`; region derivation, contraction and validation with
  compile errors naming NodeIds. Tests `substeps_region_*` from the branch, re-written
  against main.
- **Gate:** `cargo nextest run -p manifold-renderer substeps_` green; clippy clean;
  every existing `execution_plan` test green.
- **Demo:** none — L1.
- **Forbidden:** cherry-picking from `wave/live-water`; executor changes (P0b); nested
  regions.
- **Phase notes (built 2026-09-29, Opus 5.5 worker):**
  - Plan-consumer inventory at `f892fb846`: 57 files. Lifetime deciders are
    `execution_plan.rs` (78 hits), `execution.rs` (39), `resource_allocation.rs` (12),
    `preset_runtime/build.rs` (3). Array scratch reuse and the chain slot planner
    both return storage only through `free_after`, so region resources stay
    dedicated there. The rest read steps for lookup, profiling or physics carry.
  - Deviation: the historical `count`/`delta`/`time`/`index` fields became
    `iteration_scalars: &'static [&'static str]`, because `node.matter_state` sets
    six per-iteration scalars. The capture list is `capture` plus `results`, and
    each capture port must also be a declared state-capture input.
  - Regions contract through the coupled-scene group contraction
    (`physics_scene.rs`, now `contracted_execution_order`), not a second reorder,
    so coupled step indices stay valid. Region steps are never hoistable.
  - The render/IO rule rejects draw calls and the final output. It cannot reject
    `IoBridge`: fused region kernels are `node.wgsl_compute` nodes that report it.
    Coupled-scene participants inside a body are rejected.

### P0b — Substep regions: executor repeat and freeze

- **Entry state:** P0a merged. Re-derive: `rg -n 'fn execute_frame_inner|fn compute_live_steps|late_capture' crates/manifold-renderer/src/node_graph/execution.rs`.
- **Read-back:** D7; branch commits `115720c56`, `6a1228c85`, `1f6e12325`, `82006b602`; FREEZE_COMPILER_MAP.md section 4 (The cut rules — when fusion says no) and section 9 (Executor contracts fusion leans on).
- **Deliverables:** the executor runs the boundary once, then the region `count` times,
  setting per-iteration scalars before each run, through one extracted step evaluator
  (no recursive `execute_frame_with_state`); region resources held for the whole repeat;
  per-iteration uniforms in distinct arena slices; region capture excluded from frame-end
  late capture; freeze preserves membership and never fuses across the border. Tests
  `substeps_count_order_and_zero_steps`, `substeps_final_state_escapes`,
  `substeps_no_recycle_between_iterations`, `substeps_uniforms_distinct_per_iteration`,
  `substeps_frozen_unfrozen_match` (GPU), `substeps_execute_post_once`.
- **Gate:** tests green; `scripts/gpu_proofs_gate.py --filter substeps_`; every existing
  feedback, freeze and execution test green (`cargo nextest run -p manifold-renderer
  execution freeze feedback`).
- **Demo:** none — L1.
- **Forbidden:** a second executor; changing `node.feedback` or `node.array_feedback`
  semantics; dynamic pipeline builds.

### P1 — Water kernel writing the particle frame

- **Entry state:** P0b merged; `rg -n 'pub struct FluidParticle' crates/manifold-renderer/src/node_graph/fluid_particles.rs` and `rg -n 'node.particles_to_copies' crates/manifold-renderer/src/node_graph/primitives` hit (surface P3). Content-thread frame fence reachable (surface P2 entry check).
- **Read-back:** D3–D10, D14, D15, D18; sections 3, 4, 6; ADDING_PRIMITIVES.md whole; the
  branch kernels in section 1.2 (read only). Restate the forbidden list of section 7.
- **Deliverables:** `R/matter.rs` records, fixed-point helpers and
  `pub fn substeps_per_tick(dx: f32, wave_speed: f32, domain_height: f32, body_limit:
  Option<f32>) -> Result<u32, String>`; `R/matter/reference.rs`; atoms
  `matter_materials` (water), `matter_fill` (box), `matter_state`, `zero_array`,
  `matter_to_grid`, `matter_grid_update` (walls, gravity), `grid_to_matter`,
  `matter_stats`, `matter_frame` (solid lattice = domain walls only). Presets
  `WaterStillPoolMatter.json` and `WaterDamBreakMatter.json` ("Water — Dam Break (Live
  GPU)"), particle view through `particles_to_copies`, same domain, column and
  resolution as `WaterDamBreak.json`. A `matter_` entry in `scripts/landing_gate.py`
  GPU proof scope (⚠ VERIFY-AT-IMPL its mapping structure). Tests from section 10:
  reference, mass, momentum, still pool, volume, energy, Martin & Moyce, determinism,
  headroom, non-finite, substep rule, tick cap, frame ids. Probe
  `tests/gpu_proofs/matter_cost_probe.rs`: ns per point-substep at 500,000 points on the
  still pool.
- **Gate:** tests green; clippy; `cargo run -p manifold-renderer --bin check-presets`;
  `cargo run -p manifold-renderer --bin graph-tool -- validate <preset> --kind generator`
  and `fusion` for both presets, output recorded.
- **Kill check:** projected Dam Break solver time = probe ns × 500,000 × 34 + lattice
  term. Above 12 ms: stop, report the table, and escalate to Peter with section 8's lever
  table before P2a.
- **Demo:** `fluid_capture --preset WaterDamBreakMatter --frames 180 --stills-every 15`
  into `/tmp/manifold_matter_p1`. Computed checks: live count constant, no non-finite
  point, mass exact. L2: Peter looks at the stills.
- **Gesture:** pause and resume transport mid-splash; the water holds exactly and resumes
  without a jump.
- **Forbidden:** everything in section 7; a CPU fallback; colliders (P2a).

### P2a — Colliders

- **Entry state:** P1 merged. Anchors: `rg -n 'pub struct FluidRole' crates/manifold-renderer/src/node_graph/fluid_role.rs`, `rg -n 'pub fn hull_meshes' crates/manifold-physics/src/mesh.rs`. Answer D16's VERIFY before code.
- **Read-back:** D11, D16; section 4.1 steps 2 and 4; FLUID_ENGINE_INTEGRATION_PLAN.md section 3.2 (Geometry and solver capabilities).
- **Deliverables:** `P/sdf.rs`: `pub fn signed_distance_lattice(mesh: &TriangleMesh,
  spacing: f32, padding: f32) -> Result<DistanceLattice, PhysicsError>` (closed-mesh
  validation shared with FLIP roles, sign by generalized winding number, no allocation
  beyond the result); `node.matter_roles` (Collider kind, prescribed motion from role
  transforms with the existing input history); `matter_move_bodies`,
  `matter_solid_distance`; collider projection in `matter_grid_update`. A thinness
  warning for colliders under 2 cells. Tests: `sdf_box_matches_analytic`,
  `sdf_concave_bowl_sign`, `sdf_rejects_open_mesh`, `matter_collider_penetration_bounded`,
  `matter_prescribed_pose_interpolates`, `matter_solid_lattice_matches_bodies`.
- **Gate:** tests; `cargo nextest run -p manifold-physics sdf_`; GPU filter; clippy on
  physics and renderer; Dam Break Matter gains the moving box as a Collider role;
  check-presets and graph-tool clean.
- **Demo:** P1's capture on the updated preset into `/tmp/manifold_matter_p2a`. L2.
- **Gesture:** sweep the paddle fast through the pool; water parts around it and nothing
  leaks through.
- **Forbidden:** a GPU mesh-to-SDF atom; box-only fallbacks for rejected meshes; CPIC
  sidedness.

### P2b — Two-way Box3D coupling

- **Entry state:** P2a merged. Anchors: `rg -n 'pub fn advance_with_coupling|enum AdvancementPolicy' crates/manifold-renderer/src/node_graph/physics.rs`, `rg -n 'fn set_coupled_rigid_inputs|fn accept_coupled_rigid_frame' crates/manifold-renderer/src/node_graph/effect_node.rs`. ⚠ VERIFY-AT-IMPL that `RigidSceneObservation` carries everything `advance_with_coupling` needs (read `R/physics/worker.rs` whole); a missing field is an escalation.
- **Read-back:** D12; section 5; FLUID_ENGINE_INTEGRATION_PLAN.md section 8 (Phasing), its phase P8b (Two-way liquid/rigid coupling), including the rejected candidate.
- **Deliverables:** `node.matter_body_reaction`; `MatterCoupling: StepCoupling`; the
  coupling hooks on `node.matter_roles`; the fenced reaction ring; the D4 body term and
  its setup error; material slot density for coupling. Tests: the five coupling tests of
  section 10, `matter_coupled_holds_when_reaction_pending`, and every existing
  `physics_` and `fluid::coupled` test unchanged.
- **Gate:** tests and GPU filter green; clippy; content-thread gate: the orchestrating
  session runs the app with `MANIFOLD_RENDER_TRACE=1` on the coupled demo for 60 s, no
  frame over 20 ms.
- **Demo:** preset `WaterFloatingBoxMatter.json`: a density-0.5 box dropped into the pool.
  Capture into `/tmp/manifold_matter_p2b`; computed check: final box height within
  0.5·dx of the waterline. L2.
- **Gesture:** drop a light box into the pool, then fire a force impulse at it; it bobs,
  spins and settles.
- **Forbidden:** blocking readback; Box3D per substep; damping or mass changes to pass
  energy tests; FLIP-style after-the-fact exchange.

### P3a — Fills, inflows and drains

- **Entry state:** P2b merged.
- **Read-back:** D9, D11; section 4.2; FLUID_ENGINE_INTEGRATION_PLAN.md section 4 (Scene Panel and creative workflow).
- **Deliverables:** mesh fills in `matter_fill`; `matter_emit` (inflow velocity, inherit
  motion, per-tick rate); `matter_drain`; `matter_compact`; epoch renumbering. Matter
  versions of `WaterBasin.json` (pour) and the Dam Break with every FLIP role kind.
  Tests: `matter_initial_fill_volume_matches_mesh` (within 3%),
  `matter_inflow_rate_matches_authored` (within 5% over 2 s),
  `matter_drain_removes_and_compacts_preserving_id_order`, and a held-out concave role
  mesh the builder did not develop against.
- **Gate:** tests; GPU filter; clippy; check-presets and graph-tool clean.
- **Demo:** the pour preset into `/tmp/manifold_matter_p3a`. L2.
- **Gesture:** move the pouring source while it pours into a basin with a floor drain.
- **Forbidden:** atomic birth counters (non-deterministic order); reordering storage.

### P3b — Forces and impulses

- **Entry state:** P3a merged. Anchors: `rg -n 'pub fn enqueue_impulse' crates/manifold-renderer/src/node_graph/fluid/impulses.rs` (`:60` at `c8961489d`), `rg -n 'acceleration_field' crates/manifold-renderer/src/node_graph/primitives/fluid_surface.rs`.
- **Read-back:** D13; FLUID_ENGINE_INTEGRATION_PLAN.md section 5 (Timing, events and lifecycle).
- **Deliverables:** `node.matter_forces` with the existing impulse hooks and event queue;
  force and impulse lattices in `matter_grid_update`. Tests from section 10.
- **Gate:** tests; GPU filter; clippy; the `scene-forces-controls` UI flow still passes.
- **Demo:** a radial impulse on a clip edge splashing the pool, into
  `/tmp/manifold_matter_p3b`. L2.
- **Gesture:** bind a kick-triggered radial impulse; the pool splashes on the beat.
- **Forbidden:** per-node CPU field evaluation; a matter-only force system.

### P4 — Perf gate and the side-by-side (the go/no-go)

- **Entry state:** P3b merged; surface design P1–P3, P5, P6 merged (FLIP particle capture
  and the Liquid Surface group): `rg -n 'fn capture_particle_frame' crates/manifold-fluids/src`, `rg -n 'node.volume_surface_mesh' crates/manifold-renderer/src/node_graph/primitives`.
- **Read-back:** D19; section 8; the surface design's P6 perf proof.
- **Deliverables:** `tests/gpu_proofs/matter_solver_perf.rs` behind a new
  `matter-perf-proofs = ["gpu-proofs"]` feature shaped like `rt-perf-proofs`: seeded Dam
  Break Matter at 64³ with the pool deepened until the live count is ≥ 500,000 (count
  reported), 16 warm-up and 120 measured frames, per-stage GPU time, the 1080p scene
  alongside; the 128³ / 30 Hz stretch reported. Side-by-side capture: FLIP and matter on
  the same domain, column, resolution and Liquid Surface settings, 300 frames each, into
  `/tmp/manifold_matter_vs_flip/{flip,matter}` plus a contact sheet.
- **Gate:** `cargo test -p manifold-renderer --features matter-perf-proofs --test gpu_proofs matter_solver_perf` reports p95 against 6.0 ms on M4 Max, with machine, OS and build. A miss stops the phase and reports the slowest stage and the lever table. Peter's look call and go/no-go are recorded in a `decision` bead.
- **Demo:** the side-by-side. L2, Peter's call.
- **Gesture:** scrub the Dam Break back to the start and trigger it again with both
  solvers in view.
- **Forbidden:** changing constants, particle counts or stiffness to pass; comparing
  through different surfaces.

### P4-opt — Block-local P2G (conditional entry)

- **Entry state:** P4's per-stage profile shows `matter_to_grid` above 40% of solver time
  and the total above 6 ms; surface `node.sort_particles_into_cells` exists. Otherwise
  this phase is skipped and the skip recorded.
- **Read-back:** D6; the P4 profile.
- **Deliverables:** an `order: Array(u32)` output on the sort atom; `matter_to_grid`
  interior switched to block-local workgroup accumulation over that order; the P4 perf
  proof re-run. Results bit-identical to P1's scatter (integer sums).
- **Gate:** `matter_transfer_matches_reference` and `matter_deterministic_under_seed`
  unchanged; the perf proof reported.
- **Demo:** none — L1.
- **Forbidden:** reordering `MatterPoint`; subgroup operations.

### P4b — Add Fluid authors matter (after Peter's go)

- **Entry state:** a closed `decision` bead with Peter's go from P4. Anchor:
  `rg -n 'pub struct AddSceneFluidCommand' crates/manifold-editing/src/commands/graph/scene/fluid.rs`.
- **Read-back:** D20, D21; GROUPING_GRAPHS.md.
- **Deliverables:** `AddSceneFluidCommand` inserts the Live Matter group and the Liquid
  Surface group instead of `node.fluid_surface`; Scene Panel domain discovery accepts
  matter domains; role assignment unchanged. If the surface design's P7 already landed,
  replace its FLIP producer in the same command. Test
  `scene_physics_add_fluid_matter_undo_reload`; UI flow
  `scripts/ui-flows/scene-fluid-matter.json` (add → play → move source → undo/redo →
  save/reload → play); every `scene-fluid-*` flow on disk passes (count them).
- **Gate:** the test and every counted flow pass; focused editing, app and renderer
  clippy.
- **Demo:** the flow. L3. Hand Peter the worktree launch command.
- **Gesture:** Add Fluid into an existing scene and drag the source while it pours.
- **Forbidden:** migrating FLIP scenes; a solver dropdown.

### P5a–P5e — Materials, one phase each, jelly first

Shared entry: P4 go recorded. Shared read-back: D10, section 4.4, the cited paper for the
model. Shared forbidden: a second solver, damping to hide instability, new record types
beyond `MatterDeformation`. Each phase gates on its tests, the GPU filter, check-presets
and graph-tool, with an L2 capture for Peter.

| Phase | Deliverables | Test | Gesture |
|---|---|---|---|
| P5a jelly | `matter_update_deformation`; 3×3 SVD in WGSL (McAdams et al. 2011 form, the one Taichi `ti.svd` uses) with value tests; fixed-corotated branch; `JellyDropMatter.json` | `matter_jelly_cube_rebounds` (rest shape recovered within 2% after 3 s) | drop a jelly cube onto a paddle and squash it on the beat |
| P5b snow | hardening and plasticity branch | `matter_snow_ball_fractures` (clump count > 1 after impact); Jp stays in [0.6, 20] | throw a snowball at the floor |
| P5c sand | Drucker–Prager branch | `matter_sand_pile_angle` (settled slope within 5° of the expected angle) | flip gravity on a fader and watch a pile avalanche |
| P5d honey | viscous branch and the D4 viscous limit | `matter_viscous_decay_matches_analytic` (shear layer decay within 5%) | pour honey and watch it coil |
| P5e lava | viscoplastic branch | `matter_bingham_flow_stops_below_yield` | lava flows down a slope and stops |

### P6 — Whitewater

- **Entry state:** P4 go; the surface design's P8 `particles_to_copies` whitewater path exists.
- **Read-back:** D22; FLIP whitewater params in `WaterDamBreak.json`.
- **Deliverables:** grid-based potentials (trapped air from velocity difference, wave
  crest from mass-gradient curvature and velocity, kinetic energy), emission by scan into
  a separate pool, ballistic spray, advected foam, buoyant bubbles, with the FLIP
  constants of section 4.5; outputs `foam_particles`, `bubble_particles`,
  `spray_particles` as `FluidParticle` frames with id 0. Test
  `matter_spray_follows_ballistic_arc`.
- **Gate:** the test and GPU filter; check-presets and graph-tool clean.
- **Demo:** Dam Break Matter with whitewater; L2 beside FLIP's whitewater.
- **Gesture:** trigger the dam break; spray arcs at 60 fps.
- **Forbidden:** ids for whitewater; screen-space foam.

### P7 — Particle-frame bake

- **Entry state:** P4 go. ⚠ VERIFY-AT-IMPL the `fluid_cache.rs` writer and the take journal (`R/fluid/take.rs`) before pinning the payload.
- **Read-back:** D17, D22; the surface design's D12.
- **Deliverables:** Record and Playback for matter graphs: per-tick `FluidParticle` frames
  (and whitewater frames) through the existing atomic per-tick file writer as a new
  payload kind with its own manifest version; playback feeds the same seam; the surface
  design's D12 lifts for matter graphs; Record waits on fences (offline). Tests:
  `matter_cache_round_trip` (save, reload, play, frames identical),
  `matter_cache_rejects_changed_setup`, `matter_cache_playback_never_steps_solver`.
- **Gate:** the tests; the round-trip gate per DESIGN_DOC_STANDARD.md section 5 (Phase briefs): record, reload the project, play, modulate after reload.
- **Demo:** record a take with a kick-driven impulse and export it at 30 fps. L2.
- **Gesture:** record a take live, then export it with a different material colour.
- **Forbidden:** caching the `MatterPoint` state (frames only); silently loading
  mismatched caches.

Phasing completeness: every behaviour this document commits to lands in one phase above
or in section 13.

## 12. Decided — do not reopen

1. Live liquid is GPU MLS-MPM writing the surface seam; FLIP is bake and reference; real mesh only (Peter).
2. MLS-MPM over GPU FLIP/APIC because it avoids the global pressure solve; not claimed cheaper for plain water.
3. J-tracked weakly compressible water; Cohesion default 0.
4. Stiffness `λ = 2.78e5·L`; acoustic CFL 1/3; substeps fixed per setup; ≤ 128.
5. Flat arrays with the lattice on wires; i32 fixed point at Q = 2^20; deterministic.
6. One atomic scatter over unsorted points in P1; storage order never changes.
7. Executor repeat region re-implemented from the historical seam.
8. Fixed 60 Hz ticks; live caps ticks per frame and reports dropped time; export runs every tick; display one tick behind.
9. Append-only births, order-preserving compaction, id-sorted frames.
10. One solver, per-particle material slots, constitutive branches.
11. Collider SDFs built on the CPU in `manifold-physics` from existing role and hull meshes; grid-node projection with friction.
12. Coupling: GPU body integration per substep, Box3D lockstep per tick, fenced readback, hold on miss.
13. Only non-finite state faults; speed is clamped and counted.
14. Proof by invariants, energy, still pool and Martin & Moyce; no dt-halving gate.
15. Add Fluid switches only after Peter's P4 go.

## 13. Deferred, with triggers

| Item | Revive when |
|---|---|
| Subgroup (warp) P2G reductions | P4-opt misses and subgroup support is verified through naga → SPIR-V → MSL and on the Vulkan backend |
| Fused G2P2G kernel | P4 misses after P4-opt and Peter grants a no-monolith exemption |
| CPIC colored-distance-field compatibility (thin shells, cutting) | A collider thinner than 2 cells leaks in a show scene, or Peter wants cutting |
| Real surface tension | Peter judges the Cohesion fake wrong for a named look |
| Converting a matter fluid to FLIP for a reference bake | Peter wants a FLIP bake of a scene authored live |
| More than one coupled tick per frame (30 fps projects) | A coupled scene is needed at a project rate below 60 fps |
| Adaptive substeps from read-back speed | Fixed substeps cost more than 20% over need in a measured show scene |
| Particle-level collider push-out | `matter_collider_penetration_bounded` fails at grid resolution |
| Sparse or adaptive grids, 128³ live | P4's stretch report and a named scene need it |
| Multiple matter domains exchanging material | A scene needs two interacting domains |
| Quality tiers for matter | P4b lands and Peter asks for tiers |
| Per-material surfaces in one domain | P5 materials share a scene and need separate meshes |
| GPU mesh-to-SDF | Deformable (skinned) colliders are needed |
| Vulkan runtime proof | The Vulkan backend runs the GPU proof suite |

## 14. Risk register

| # | Risk | Detection | Response |
|---|---|---|---|
| R1 | **Perf: the gate sits at the bandwidth roofline** (section 8); the brief's 1M-at-60-fps reference was not found | P1 kill check; P4 gate | Escalate with the lever table; P4-opt is the only pre-authorized change |
| R2 | Springiness: water reads bouncy against FLIP | P4 side-by-side | Peter's call on stiffness versus substeps; never damping |
| R3 | J drift loses or gains volume over minutes | `matter_still_pool_volume_stable` | Escalate; candidate fix is a periodic J reset from grid density, a design change |
| R4 | Atomic contention on Metal at dense interiors | P4 per-stage profile | P4-opt |
| R5 | Thin-sheet tearing at 8 points per cell: splashes break into droplets sooner than FLIP | P4 side-by-side | More points per cell costs R1; Peter's call |
| R6 | Light bodies need too many substeps | D4 setup error on bodies below about 0.05 density ratio | Stated limit; revisit with the CPIC trigger |
| R7 | APIC dissipation makes water calmer than FLIP | P4 side-by-side | Peter's call; PolyPIC is a later research option |
| R8 | Dispatch overhead (140–210 per frame) | P4 per-stage profile | Fewer, larger atoms need Peter (monolith rule) |
| R9 | Readback misses make coupled scenes lag | Lag readout; P2b trace | Reported; Reset recovers; multi-tick deferred |
| R10 | The re-implemented executor seam breaks existing feedback or freeze behaviour | P0b gate runs every execution, feedback and freeze test | Fix before landing |
| R11 | CPU SDF build too slow for photoscan roles | P2a timing on a held-out 100k-triangle mesh | Proxy hulls, or the GPU SDF trigger |
| R12 | Unverified constants (snow E0 and density, sand, lava, Martin & Moyce data) | VERIFY-AT-IMPL markers | Transcribe from the papers at phase entry; a mismatch is an escalation |
