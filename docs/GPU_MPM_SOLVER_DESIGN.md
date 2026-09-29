# GPU MLS-MPM Solver — live liquid (then goo, snow, sand, lava) as GPU atoms writing particle frames

<!-- index: Replaces CPU FLIP as the live liquid solver with a GPU MLS-MPM built from graph atoms in a repeated substep region; writes the GPU surface design's particle-frame seam; rides the existing scene, role, force and Box3D coupling systems; look and speed are gated; materials, whitewater, bake and demo scenes as later phases. -->

**Status:** IN PROGRESS · P0a–P0b on main · P1 work on `feat/gpu-mpm-build-b` · P1 partial: D5 amended for BUG-m9g8 (MPM D5 fixed point loses momentum); J bounded for BUG-8akp (MPM water J grows without bound); A2, A4, A6 and the still pool fail as look findings for Peter · P1b in progress (L1 built) · kill check fired (53 ms against 12 ms), budget at P4 in BUG-u3ov (MPM solver budget) · P2–P8 not built · phase notes under each brief in section 13.
**Prerequisites:** GPU_FLUID_SURFACE_DESIGN.md P1–P3 before P1; its P5–P6 before P4.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) before starting any phase.

Peter's decision, relayed by the lead on 2026-09-29 and restated here, not reopened:
the live liquid solver becomes a GPU MLS-MPM (Hu et al. 2018, "A Moving Least Squares
Material Point Method with Displacement Discontinuity and Two-Way Rigid Body Coupling")
writing into the particle-frame seam of
[GPU_FLUID_SURFACE_DESIGN.md](GPU_FLUID_SURFACE_DESIGN.md). FLIP Fluids
(`crates/manifold-fluids`) stays as the bake and reference engine, and its look is the
visual reference. Liquids are always a real mesh; screen-space rendering is vetoed.
Peter's priority for this design: **"MPM has to look GOOD and run well."** Both are
gates here, not aspirations (D19, D20). **This design supersedes the surface design's D1
clause that live water is FLIP, and drops its D3/D9 solver-rate work and P4 for live;
its seam, atoms and interpolation stand.** FLIP appears in this document only as a test
feed for the surface and as the reference half of the side-by-side; nothing here tunes
or integrates FLIP (D22).

Why, measured by the lead on 2026-09-29: CPU FLIP at 64³ on the 90-tick Dam Break spends
152 ms solving, 242 ms meshing and 5.6 ms uploading per 1/60 s tick (medians); at 32³
it is 29 ms solve and 34 ms mesh. The CPU solver is ten times over a live budget at the
resolution the show needs.

**The governing insight: MLS-MPM is local and explicit.** A substep is a handful of
small dispatches over flat arrays — clear the grid, scatter particles to it, update grid
velocities, gather back to particles and move them. There is no global pressure solve,
no iteration count, no convergence to tune, and the same solver runs water, goo, snow,
sand and lava by swapping the stress function. Each dispatch is a graph atom; a new
executor feature repeats the region of atoms `substeps × ticks` times per frame. The
state becomes the surface design's particle frame, and the surface chain turns it into
the ordinary mesh the scene, material, RT and volume-optics path already draw.
**Everything the scene already knows how to do to a liquid — roles on objects, forces,
impulses on the beat, Box3D coupling, Add Fluid — reaches the new solver through the
existing systems, because the new domain node speaks the FLIP domain node's scene
contract (D17).**

**What it does for the show.** Water that simulates at 60 Hz in real time at 64³, reacts
to a hit on the tick it lands, pushes and is pushed by Box3D objects, and later becomes
goo, snow, sand or lava — with Melt on a fader — by changing a material, not a solver.
**The price, stated up front:** weakly compressible water is slightly springy; one tick
of display latency (the surface design's D10, already accepted); and the speed gate
Peter set — 500k particles in ≤ 6 ms — sits at the memory-bandwidth roofline of an
M4 Max (section 8, Speed). The design measures early (P1 kill check), optimises in its
own phase (P1b), and hands Peter priced levers if the gate misses.

Binding constraints, per DESIGN_AUTHORING.md section 1 (The intake): **hot path**
(the solver is 140–210 GPU dispatches per frame inside the render budget); **thread
residency** (everything encodes on the content thread, no worker, no lock); **time
model** (fixed 60 Hz physics ticks in `Seconds`, the documented physics exception);
**persistence** (graph JSON only until the P7 bake); **performance surface** (gravity,
speed, reset, forces, impulses, stiffness, cohesion, viscosity, liveliness and melt are
live params).

Companions: [GPU_FLUID_SURFACE_DESIGN.md](GPU_FLUID_SURFACE_DESIGN.md) (the seam this
writes and the surface atoms that mesh it);
[WATER_SIMULATION_DESIGN.md](WATER_SIMULATION_DESIGN.md) (its historical "Earlier GPU
proposal" is an MLS-MPM design this mines for the substep seam and lessons);
[WATER_IMPLEMENTATION_PLAN.md](WATER_IMPLEMENTATION_PLAN.md) (the superseded S1–S8
briefs that built that prototype);
[FLUID_ENGINE_INTEGRATION_PLAN.md](FLUID_ENGINE_INTEGRATION_PLAN.md) (the scene, role,
force, timing and coupling contract this rides; mapped feature by feature below);
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
| Surface atoms | GPU_FLUID_SURFACE_DESIGN.md section 4 (The atom chain) | PROPOSED, not built: interpolate, push-out, sort into cells, blobs, level set, marching cubes, `particles_to_copies`, the "Liquid Surface" group. |
| FLIP domain node | `R/primitives/fluid_surface.rs:35` (`node.fluid_surface`), `:38-101` (`role_0..role_63: FluidRole`), `:102` (`acceleration_field: VectorField`), `:103` (`domain`, legacy `initial_volume` Transforms), `:115-121` (outputs), `:123-145` (params: seed, resolution, grid budget, six closed faces, fill height, density, gravity XYZ, speed, reset, …) | Exists. Its scene-facing ports and params are the contract `node.matter_domain` mirrors (D17). |
| Scene layer keyed on the FLIP type id | `rg -n '"node\.fluid_surface"' crates -g '*.rs'` → 94 hits in 46 files at `c8961489d`; 53 in 25 non-test files, about 40 of them outside inline test modules, across `manifold-core` (scene-object migration, exposure tables, file loader), `manifold-editing` (Add Fluid, role commands), `manifold-app` (domain gizmo, scene projection), `manifold-renderer` (physics sampling, sources, carry, impulses, scene exposure, scene VM, force recipients, graph loader) | Exists. Every site tests a string literal; there is no liquid-domain predicate. P3a's seam (D17). |
| Role wire | `R/fluid_role.rs:20-25` (`FluidRoleKind`: InitialFill, Inflow, Outflow, Collider), `:36-44` (payload: `Arc<PreparedFluidGeometry { meshes: Vec<TriangleMesh> }>`, transform, enabled, velocity, inherit_motion, friction) | Exists, solver-agnostic. Consumed unchanged. |
| Add Fluid and role authoring | `crates/manifold-editing/src/commands/graph/scene/fluid.rs:40` (`AddSceneFluidCommand`), `…/scene/fluid/roles.rs:24`; param surfaces `crates/manifold-ui/src/param_surface.rs` | Exists. P4b extends the command; role commands ride the predicate. |
| Force recipients | `R/scene_modifier_expand/acceleration.rs:110` (a fluid is found as the producer of a scene object's `vertices`), `:267` (`ImpulseTarget::Fluid`) | Exists. Assumes the domain node itself feeds the object's mesh — false once a surface group sits in between (for FLIP too, after surface P7). P3a fixes the walk. |
| Domain layout oracle | `R/fluid/domain.rs:15` (`domain_layout`) | Exists. The matter lattice uses it, so both solvers share cell size in the side-by-side. |
| Shared physics vocabulary | `P/input.rs` (`InputHistory`, `EventQueue`), `P/interaction.rs:5` (`VectorField`), `:11` (`FieldInput`), `:21` (`TickStamp`), `:238` (`SampledField`); `P/mesh.rs:9` (`TriangleMesh`), `:53` (`hull_meshes`); `P/lib.rs:958` (`dynamics`), `:1053` (`apply_impulses`) | Exists. Reused for pose history, events, fields, geometry and reactions. |
| Impulse owners | `R/fluid/impulses.rs:60`, `R/physics/impulses.rs:130` (`enqueue_impulse`) | Exists. `node.matter_domain` implements the same hooks. |
| Rigid tick owner | `R/physics.rs:355` (`RigidSimulation`), `:609` (`advance_with_coupling`), `:136-140` (`AdvancementPolicy::Worker { max_ticks }`); `P/stepping.rs:14` (`StepCoupling`), `:30` (`SubstepExchange`) | Exists. The coupling implements `StepCoupling` (D12). |
| Coupled-scene pairing | `R/scene_modifier_expand/coupling.rs:29` (`prepare_coupled_scenes`); `R/effect_node.rs:1143-1194` (coupling hooks); `R/physics/worker.rs:51` (`RigidSceneObservation`); `R/fluid/coupled.rs:50` (`CoupledRigidFrame`); `R/primitives/physics_world.rs:372-374`, `:556` | Exists for FLIP. `node.matter_domain` implements the same hooks. |
| Atomic scatter atom | `R/primitives/scatter_particles_3d.rs:95-98` (`fusion_kind: Boundary`, `boundary_reason: Blocked`, `atomic_outputs`), `:147` (`standalone_pipeline`) | Precedent for the scatter atoms. |
| Boundary codegen, reasons | `R/freeze/codegen/entry_points.rs:81` (`standalone_for_boundary_spec`); `R/primitives/standalone_pipeline.rs:18`; `R/freeze/classify.rs:305-341` | Exists. |
| Aliased, captured and provided arrays | `R/effect_node.rs:802` (`aliased_array_io`), `:909` (`state_capture_input_ports`), `:920` (`persistent_output_ports`), `:1054` (`provides_array_output`); `R/execution/array_growth.rs` | Exists — the same machinery `node.array_feedback` uses. Point state and lattice arrays ride it. |
| 3D particle family and `node.array_feedback` | `R/primitives/scatter_particles_3d.rs` (`node.draw_particles_3d`), `euler_step_particles_3d.rs` (`node.move_particles_3d`), `container_bounds_3d.rs`, `sample_texture_3d_at_particles.rs`, `array_feedback.rs:32-50` | Exists, all on the 64-byte `Particle` in unit space; `array_feedback` is a one-frame delay typed on `Particle`. Not reused for simulation state — the surface design decided that record and space are wrong for scene-metre liquid; section 10 states how this design still rides their machinery. |
| Readback | `R/primitives/color_sample.rs:12` (one frame late, no fence check); `crates/manifold-gpu/src/metal/frame_fence.rs:59` (`is_completed`) | Coupling needs the fenced form; content-thread fence exposure is the surface design's P2 entry check. |
| Headless capture | `crates/manifold-renderer/examples/fluid_capture.rs` | Exists. Gains look metrics (P1, P4). |
| Executor repeat region | none on main — `rg -n -i 'substep\|repeat_region' crates/manifold-renderer/src/node_graph -g '*.rs'` hits only FLIP and Box3D native substeps | New on main; exists on a paused branch (1.2). |
| Mesh signed distance | none — `rg -l -i 'signed.?distance\|mesh_sdf\|distance_field' crates -g '*.rs'` hits only analytic masks | **Correction to the brief:** Box3D hulls and FLIP roles produce `TriangleMesh`, not distance fields; FLIP's level sets live inside its C++ `MeshLevelSet`. The builder is new and lands in `manifold-physics` (D11). |

### 1.2 The MLS-MPM prototype (branch `wave/live-water`, paused)

Peter stopped it on 2026-09-11 "because of cost and visual regressions"; the archival
tag is `codex/water-checkpoints/2026-09-11/current-wip`. It forks main at `4acf10b7c`,
808 main commits ago; `R/execution.rs` has since grown by 2,638 lines, so nothing on it
cherry-picks. Read it for reference only.

| Piece on the branch | Files | What it did |
|---|---|---|
| Substep region seam | `R/substeps.rs` (1,814 lines), commits `8e003cbd6`, `e54db937f`, `7f0938782`, `478eb23de`, `b9630e64d`, `115720c56`, `6a1228c85`, `1f6e12325`, `82006b602` | Boundary ports, region derivation and validation, executor repeat with one extracted step evaluator, freeze membership proof. Green on its own tests. |
| State boundary with clock | `R/primitives/water_state.rs` (845), `9c2c0773f` | Fixed 960 Hz clock, accepted/candidate buffers, reset. |
| Transfer kernels | `shaders/mpm_scatter_mass_momentum.wgsl`, `mpm_scatter_stress.wgsl` (hand kernels), `mpm_grid_velocity_body.wgsl`, `mpm_gather_advect_body.wgsl`, `clear_grid_body.wgsl`, `seed_water_body.wgsl`, `water_validate.wgsl`, `water_commit_body.wgsl`, `water_common.wgsl` | Two scatter passes (mass/momentum, then density/stress from grid mass); signed i32 fixed point at Q = 2^20. The scatters were hand kernels because each carried two atomic outputs, which generated codegen could not express. |
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
| dt-halving trajectory gate | Chaotic flow never converges pointwise; weeks spent (BUG-01vr) | Gate on conserved quantities, experiment data and named look artefacts (D18, D19) |
| Two atomic outputs per scatter | Hand kernels outside codegen | One atomic output per scatter atom; diagnostics in a separate reduction (D14) |
| Screen-space splats, then a density isosurface | Blobby look confounded the solver verdict | The surface design's mesh, identical for FLIP and matter, so the look gate compares solvers, not surfaces |
| Look judged only by eye at the end | "Gel-like" found after the fact | Named artefacts measured from P1, compared with FLIP numerically (D19) |

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
without a global solve, and pays with weak compressibility and many small substeps.

**D3 — Water is weakly compressible, tracked by a per-particle volume ratio J, with three
look dials.** Each substep `J ← J·(1 + dt·tr C)` (Taichi `mpm3d.py`); water stress is the
Kirchhoff pressure `τ = λ·J·(J − 1)·I` with shear modulus zero (taichi_elements), plus
`μ_v·(C + Cᵀ)` when Viscosity > 0. One scatter pass carries mass, momentum and stress.
The dials, all port-shadowed params on `node.matter_domain`:
- **Stiffness** s ∈ [0.5, 3], default 1: multiplies the wave speed (λ scales by s²). The
  substep count follows it every tick (D4).
- **Cohesion** κ ∈ [0, 1], default 0: fraction of tension kept for J > 1. FLIP's free
  surface carries no tension and FLIP is the look. taichi's implicit κ = 1 (cohesive
  blobs, the gel look) is rejected as the default.
- **Liveliness** β ∈ [0, 1], default 0: FLIP-style blend in the gather,
  `v_p ← β·(v_p + Σ w·(v_i − v_i_before)) + (1 − β)·Σ w·v_i`, trading APIC's grid-scale
  dissipation for livelier splashes (Fei et al. 2021, "Revisiting integration in the
  material point method"; ⚠ VERIFY-AT-IMPL the paper's exact blend and position update
  before P1). The default stays 0 until Peter picks it at P4 from the side-by-side.
Rejected: Tait EOS on density recomputed from grid mass (the prototype and WebGPU-Ocean) —
a second scatter pass and noisier density.
**J is bounded after every update:** `J ← min(J·(1 + dt·tr C), J_max)` with J_max = 1 at
Cohesion 0 (water without tension stores no expansion; the usual no-negative-pressure
treatment) and J_max = 2 at Cohesion > 0 (the tension κ·λ·J(J − 1) pulls a stretched
point back; one stretched to twice its rest volume is torn, and the bound keeps
λ·J(J − 1) finite). A point whose scatter inputs are not finite contributes nothing, and
`matter_stats` counts it, so D14 halts the publish; it never reaches the integer cast.
**Amended 2026-09-30 for BUG-8akp (MPM water J grows without bound).** With J unbounded,
the Dam Break's largest J was 2.2 at tick 20, 100 at tick 65 and 8.7e17 at tick 115,
where λ·J(J − 1) overflowed f32 and the Cohesion-0 product inf·0 became NaN. The NaN
momentum was cast to i32::MIN, a finite garbage velocity, so D14 never saw it and the
water exploded about 1.9 s in.
**Consequences:** J drifts slowly because particle and grid divergence disagree; the
volume-drift gate measures it (D19). A hydrostatic pool compresses by ρgH/λ (1.8% at 2 m
depth), so the surface sits about 2 cm lower than FLIP's at that depth.

**D4 — The substep count comes from the stiffness/CFL rule, recomputed every tick.**
Inherited from taichi_elements `mpm_solver.py`: `E = 1e6·L` Pa with L the longest domain
side in metres, ν = 0.2, so the water bulk term is `λ = Eν/((1+ν)(1−2ν)) = 2.78e5·L` Pa
and the wave speed `c = √(λ/ρ0) = 16.7·√L` m/s at Stiffness 1. Because free-fall speed
also grows as √L, the Mach number stays ≈ 0.27 at any domain size. Each tick, on the
CPU, from the current parameters (no readback):

```text
v_est   = max(√(2 · 9.81 · H), fastest authored source or prescribed-collider speed)
dt_f    = (1/3) · dx / (c + v_est)                        acoustic CFL 1/3; c includes Stiffness
dt_b    = min over coupled dynamic bodies of 0.5 · (dx / c) · √(m_b / (ρ0 · A_b · dx))
dt_v    = ρ0 · dx² / (6 · μ_v)                            only when Viscosity > 0
n       = ceil(tick / min(dt_f, dt_b, dt_v)),  dt = tick / n,  n ≤ 128
```

H is the domain height; `m_b` and `A_b` are a coupled body's mass and surface area; model
hardening bounds raise c for P5 materials (D10). At the Dam Break setup (L = H = 4 m,
64 cells, dx = 0.0625 m, Stiffness 1) n = 34. Turning Stiffness to 0.5 gives n = 21;
to 2, n = 61. Live gravity above 9.81 m/s² only eats acoustic headroom (0.33 → 0.42 at
20 m/s², below mpm88's 0.51). n is capped at 128. When a live Stiffness or Viscosity
value would need more, the solver uses the largest value that fits, publishes it on the
domain's `limited_by_substeps` output and raises a node warning — limited, never
silently. (At dx = 0.0625 m the cap allows Viscosity up to about 5 Pa·s.) A coupled body
too light to fit is a named error when the coupled scene is prepared. Rejected: adapting
n from a read-back velocity maximum — readback arrival timing varies, so live
trajectories would stop being deterministic. Rejected: the prototype's fixed 960 Hz.

**D5 — The grid is flat arrays with the lattice on wires; accumulation is signed fixed
point.** Per the surface design's D8. The lattice is the `domain_layout` box grown by 3
nodes per side (taichi `padding = 3`), node (i, j, k) at `min + (i, j, k)·dx`. Arrays:
`grid_accum: Array(i32)` (4 per node: momentum xyz, mass) and `grid: Array(MatterGridNode)`
(resolved and pre-update velocity, for Liveliness). Accumulation is `atomicAdd` on i32 in
normalized units — mass in `m_unit = 1000·dx³/8` kg scaled by Q_m = 2^16, momentum in
`m_unit·dx/dt` scaled by Q_p = 2^27 — with unbiased rounding per contribution:
`encode(x) = floor(x·Q + u)`, where u ∈ [0, 1) is a 24-bit hash of the point id, node
index, tick, substep and word, and the carry out of the fraction is taken in integers
(`matter::encode_fixed`), so f32 addition cannot bias it. The expected encoding equals x,
so small contributions no longer always round to zero. Integer addition is order-independent and u depends only on those
indices, so the solver is bit-deterministic on one machine and build.
**Amended 2026-09-30 for BUG-m9g8 (MPM D5 fixed point loses momentum).** The original
scale, Q = 2^20 for both mass and momentum with round-to-nearest, made one momentum LSB
worth dx/dt (about 127 m/s at the Dam Break) times one mass LSB. Small-weight
contributions kept their mass and lost their momentum, so low-mass edge nodes read slow.
On the f64 reference (`matter_reference_fixed_point_free_flight_momentum`: dx = 1/16 m,
n = 34, 300 substeps, v = (1, 0.5, −0.25) m/s) a translating blob lost 0.82%, 2.1% and
5.6% of its x, y, z momentum; on the GPU, 6%, 15% and 39% per second, with J growing.
Rebalanced scales alone left +6e-4 per component; with unbiased rounding it is −2.2e-5,
−2.5e-5, −2.8e-5 (f64 without rounding: 2e-16). Q = 2^20 had been the prototype's
measured mass choice (0–0.0125% mass error, where Q = 4096 gave 2.4–4.8%); that
measurement never checked momentum. The arrays are provided outputs of `node.matter_state` that grow on setup changes
(surface design precedent, `R/primitives/fluid_surface.rs:202-206`), written in place
through `aliased_array_io`. Rejected: `Texture3D` (surface D8; no float atomics on storage
textures in WGSL). Rejected: a float compare-exchange loop (slower in the prototype,
order-dependent). Rejected: Metal float atomics (not portable).

**D6 — P2G is one atom; P1 builds the plain global-atomic scatter, P1b replaces its
interior with cell-sorted block-local accumulation.** One thread per particle adding to
27 nodes with global atomics is the prototype's proven path and the correctness
baseline. P1b sorts particles by 4³-cell block once per tick (the surface design's
`node.sort_particles_into_cells`, D17 there, gaining an `order: Array(u32)` output), and
`node.matter_to_grid` then accumulates each block's particles into `var<workgroup>`
atomics over the block's 6³-node tile and flushes each tile node once; a particle that
has drifted out of its tile since the sort adds directly with global atomics. Integer
sums make the two paths bit-identical. Core WGSL only. **The `MatterPoint` storage order
is never changed** — ids must stay sorted for the seam (D9). Rejected for now: subgroup
(warp) reductions — an optional WGSL feature whose naga → SPIR-V → MSL and Vulkan support
is unverified (Deferred).

**D7 — Substeps run in an executor repeat region.** The historical SubstepBoundary seam
(WATER_SIMULATION_DESIGN.md section 4 (Fixed substeps and graph compiler seam)) fits
unchanged in shape: a boundary node declares ports; the compiler contracts the nodes
between its state outputs and its capture inputs into a region; the executor runs the
boundary once, then the region body `count` times with per-iteration scalars; only the
final state escapes; freeze never fuses across the region border. Re-implemented on
current main in P0a/P0b with the branch as reference. Rejected: one `mpm_solver` node that
dispatches everything (the no-monolith rule). Rejected: unrolling N copies of the atoms
(N changes with Stiffness). Rejected: repeating the whole frame per substep.

**D8 — Fixed 60 Hz ticks, owned by the domain node; live never spirals; export never
drops.** Tick = 1/60 s, equal to Box3D's fixed tick, stamped with the shared `TickStamp`
and fed by the shared `EventQueue` and `InputHistory` exactly as FLIP's runtime is.
`node.matter_domain` owns the clock (transport, Speed, Reset, epochs) and publishes this
frame's tick count; `node.matter_state` repeats `ticks × n`. Live runs at most
`ceil(project_frame_interval / tick)` ticks per display frame (1 at 60 fps, 2 at 30 fps);
due time beyond that is dropped, counted and published on `dropped_seconds`. Export and
Record run every due tick and may wait on GPU fences. Display follows the surface
design's D10: s = target − tick for every solver-time output. Transport behaviour
(pause, stop, seek, loop, clip edges, reset) follows FLUID_ENGINE_INTEGRATION_PLAN.md section 5 (Timing, events and lifecycle) unchanged.
Rejected: FLIP's retained unbounded debt for this solver — one tick costs about a frame
of GPU time, so catching up spirals into more missed frames. Under overload the water
plays in slow motion, visibly reported, instead of stalling the show.

**D9 — Particles live in one capacity pool; births append, drains mark, compaction keeps
order; points per cell is an explicit control.** Each point carries `id` = birth ordinal
within an identity epoch; 0 marks an unused slot. Fills and inflows append at a cursor in
lattice order through a prefix scan (deterministic, no atomic counter); drains set id 0;
once per tick, when dead slots exceed 1/8 of the used range or the cursor needs room, an
order-preserving compaction runs. The published frame is always id-sorted, which the
seam's interpolation needs (surface D11); near `u32::MAX` the frame renumbers and bumps
the identity epoch (the surface design's section 3.1 rule). **Points per Cell** is a
setup Enum on `node.matter_domain` — 8 (2³ jittered sub-cells, the default for water, the
FLIP Fluids upstream density) or 27 (3³, finer sheets and splashes at 3.4× the cost).
Changing it restarts the simulation.

**D10 — One material per domain in v1; constitutive branches; Melt is the phase-change
control.** `node.matter_domain` carries a setup **Material** Enum (Water, Goo, Snow, Sand,
Lava) and publishes one `MatterMaterial` entry. `node.matter_to_grid` switches on the
model: water (D3), fixed-corotated goo, Stomakhin snow, Drucker–Prager sand, Bingham lava.
Models with shape memory keep an `Array(MatterDeformation)` (F rows) updated by
`node.matter_update_deformation`; water graphs leave it unwired. **Melt** m ∈ [0, 1] is a
live, beat-able param for Goo, Snow and Lava: each substep the elastic part of F relaxes
toward its rotation, `F ← R + e^{−k(m)·dt}·(F − R)` with `k(m) = m / ((1 − m + 0.01) · 0.1 s)`
— a Maxwell relaxation. At m = 0 the material holds its shape; at m = 1 it has no shape
memory and flows like its liquid pressure; lowering m freezes the current shape as the new
rest shape. Pressure keeps using J, so volume is never lost to melting. Mixed materials in
one domain are deferred.

**D11 — Boundaries are domain walls plus distance lattices derived from the existing role
geometry.** Walls: nodes within 3 of a closed face lose velocity into the face, with
taichi_elements' friction rule. Colliders, fills, inflows and drains: each `FluidRole`
mesh gets a body-local signed-distance lattice built by a new CPU module,
`P/sdf.rs`, and stored as a derived, non-serialized field of the existing
`PreparedFluidGeometry`, computed where that geometry is prepared — **no matter-side
geometry cache**. Coupled Box3D bodies use `hull_meshes` through the same builder, once
per coupling epoch. Lattices are packed into one atlas (`pack2x16float`, D21) and
sampled per substep through each body's pose. Grid nodes inside a collider take its
velocity in the normal direction and keep friction-limited tangential velocity. Box3D
bodies that are fixed or animated act as prescribed colliders; dynamic ones couple two
ways (D12). Rejected: a GPU mesh-to-SDF atom (a second upload path for geometry that is
CPU `TriangleMesh`). Rejected for v1: the paper's colored-distance-field compatibility
test (CPIC proper), which exists for thin shells and cutting; every role and hull here is
a closed volume (Deferred, with a thinness warning).

**D12 — Two-way coupling: Box3D in lockstep per tick; each body advanced every substep on
the GPU; the reaction crosses back through a fenced readback.** Section 5 is the
protocol. The paper advances rigid bodies with the MPM substep; Box3D cannot step per
substep without a GPU round trip, so a small GPU body integrator (`node.matter_move_bodies`)
advances each coupled body every substep from Box3D's tick-start state with gravity,
fields and the fluid's reaction. At tick end the fluid's net impulse is read back and
applied to Box3D through `apply_impulses`, and Box3D steps once with contacts. The rigid
owner is the existing `RigidSimulation`, driven through `StepCoupling`.
**How this avoids the integration plan's 16–24× energy gain.** That candidate applied a
full incompressible pressure impulse to a body after a 1/60 s solve that assumed the body
did not respond — the added-mass instability, independent of dt. Here the body responds
inside every substep, and weakly compressible pressure acts as a stiff spring. Pressing a
body of mass m_b and area A_b into the liquid compresses about one cell layer, so the
contact stiffness is k ≈ λ·A_b/dx and the body oscillates at
ω = c·√(ρ0·A_b/(dx·m_b)). Explicit exchange is stable for ω·dt < 2; the `dt_b` term of D4
keeps ω·dt ≤ 0.5. The derivation is this design's; P2b's energy tests at body/liquid
density ratios 0.1, 1 and 10 prove it with the criterion that rejected FLIP's candidate.

**D13 — Forces and impulses arrive through the existing routes as coarse lattices.**
`node.matter_domain` takes the same `acceleration_field: VectorField` input as
`node.fluid_surface` and implements the same impulse hooks; the Add Force cards, Targets
chooser, trigger modes, MIDI/OSC and beat modulation therefore reach it unchanged once
the scene layer recognises it (D17). Per tick it samples the field at a quarter of the
grid resolution per axis (17³ nodes at 64³) into a lattice, and resolved impulses into a
second lattice applied once on the tick's first substep. Rejected: evaluating CPU fields
at every grid node per substep (357,911 nodes × 34 substeps on the content thread).
**Consequence:** force detail is limited to four cells.

**D14 — Only non-finite state faults; excess speed is clamped and counted.** Grid velocity
is clamped per component to 0.9·dx/dt (taichi_elements `g2p2g_allowed_cfl`);
`node.matter_stats` reduces non-finite counts, clamp counts, speed, J range, volume,
mass, momentum, energy and fixed-point headroom once per tick. `node.matter_frame` reads
that tick's stats on the GPU and does not publish a frame whose stats show a non-finite
value; the solver halts with a node error until Reset. Persistent clamping shows as a
"liquid too fast for its substeps" diagnostic. Rejected: the prototype's velocity and
density faults, and its candidate/accepted double buffer (40 MB copies per substep).

**D15 — Determinism.** Same machine, build, seed and input stream → bit-identical
particles: integer atomics, fixed reduction trees, seeded hash jitter, substep counts
computed from parameters, Box3D at one worker. Cross-GPU identity is not claimed.

**D16 — Everything runs on the content thread.** Atoms encode into the normal command
buffer. CPU work per frame: clock, pose table (≤ 64 bodies), field lattice (4,913
samples), material entry, coupling readback. Distance lattices build where role geometry
is prepared today (⚠ VERIFY-AT-IMPL in P2a: read `R/fluid/roles.rs` and
`R/physics_mesh.rs`; if that is the content thread, stop and escalate — a 32³ ×
1,000-triangle lattice is ~0.2 s). No new thread, channel, `Arc<Mutex>` or `Arc<RwLock>`.

**D17 — `node.matter_domain` speaks the FLIP domain node's scene contract, and the scene
layer asks one predicate.** Its scene-facing inputs and params carry `node.fluid_surface`'s
names, types and meanings: `domain`, `initial_volume`, `resolution`, `grid_budget_mcells`,
six closed faces, `fill_height`, `seed`, `liquid_density`, `gravity_x`, `gravity`,
`gravity_z`, `speed`, `reset`, `role_0..role_63`, `acceleration_field`, plus the impulse
and coupled-scene hooks. Every scene-layer site that tests `== "node.fluid_surface"` to
mean "a liquid domain" is rewritten to `manifold_core::liquid_domain::is_liquid_domain`,
and the scene-object-to-domain walk goes upstream through the Liquid Surface group to the
first node satisfying it (`liquid_domain_of`). That makes Scene Panel role assignment,
Add Force targeting, impulses, World gravity/speed/reset sharing, the domain gizmo,
physics sampling and coupled-scene pairing work on matter domains with no second list,
registry or authoring flow. The CPU work behind that contract — role and pose
preparation, field sampling, the clock and the coupling exchange — is one CPU bridge,
precedent `node.fluid_surface` itself (the node the scene layer already speaks to);
ADDING_PRIMITIVES.md "The codegen path is mandatory" scope test, exclusion 3.
**Consequences:** `node.matter_domain` is the largest node in the design. Splitting its
scene-facing ports across several nodes would put a matter branch at every one of the ~40
scene-layer sites — the parallel path Peter forbade.

**D18 — Correctness is proven by invariants and experiment, never by dt-halving
trajectories.** Exact mass conservation, momentum in free flight, a still pool that
settles, dam-break energy never growing, the dam-break front against Martin & Moyce
(1952), GPU against an f64 CPU reference at small N, determinism. Free-surface flow is
chaotic; pointwise trajectory convergence is not a valid gate (BUG-01vr).

**D19 — Look is a gate.** Section 7 names the artefacts, their metrics and thresholds,
and the one dial for each look risk. P1 gates the solver's own artefacts on particle
data (grid-aligned ridges, bounce after settling, volume drift, the dam-break front,
splash and sheet retention against FLIP particles). P4 gates the same scene through the
same GPU surface against FLIP (sheet area, detached splashes, axis-aligned surface
normals, settle time, volume). Metrics are computed numbers, per DESIGN_DOC_STANDARD.md section 5 (Phase briefs) — no agent judges an image; Peter's look call at P4 is the final
go. A failed metric stops the phase and goes to Peter with the dial table; no executor
retunes a dial or a threshold.

**D20 — Speed is a gate, with its own optimisation phase.** 64³, ≥ 500k particles,
≤ 6 ms solver GPU time per 60 Hz frame at the D4 substep count, 1080p scene, M4 Max;
stretch 128³ at 30 Hz, reported not gated. P1 measures cost per point-substep and stops
early if the projection is hopeless; P1b profiles every kernel and applies the
look-neutral levers of section 8 with before/after numbers; P4 is the final gate.
Section 8 shows the gate at the bandwidth roofline; the levers that change the look are
Peter's.

**D21 — Half precision is storage-only and only where it cannot accumulate.** The
distance-lattice atlas and the force and impulse lattices are stored as `pack2x16float`
in `Array(u32)` from their first phase (read-only, never integrated). Storing C in half
precision is a P1b experiment accepted only if every P1 invariant and look gate passes
with unchanged thresholds (it saves 18% of point traffic, but tr C feeds J every
substep). Positions, velocities, J, F, grid and accumulators stay 32-bit. This respects
MANIFOLD_GPU_ARCHITECTURE.md's rejection of f16 math; nothing here computes in half
precision.

**D22 — FLIP is a test feed only; FLIP scenes stay FLIP.** FLIP particle frames (captured
by the surface design's P1–P2) feed the surface and the reference half of the P1/P4
comparisons, always at their preset defaults. This document tunes nothing in FLIP, adds
no FLIP feature, migrates no FLIP scene, and never falls back to FLIP. P3a's predicate
rewrite leaves every FLIP behaviour unchanged, gated by the existing FLIP tests.

**D23 — Add Fluid switches to matter only after Peter's go (reorder of the brief).**
Switching authoring before the P4 look and speed call would have to be undone on a no-go.
P3b builds full role parity through the existing Scene Panel role commands (they ride the
predicate); the command switch is P4b, entered on Peter's recorded go.
**Dissent recorded for the lead:** if Peter wants Add Fluid on matter earlier for
hands-on play, P4b's brief runs unchanged right after P3b.

**D24 — Live-only until P7; whitewater, bake and demos are later phases on the same
seam.** Matter graphs have no cache mode before P7, so nothing silently falls back. P6
classifies spray, foam and bubbles from grid potentials with FLIP Fluids' own constants
and publishes the surface design's whitewater frames. P7 records particle frames per tick
through the existing cache writer. P8 ships one demo scene per capability.

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
    pub affine_z: [f32; 4],   // C row 2; w = 0
}
// Specs: position Vec3F, id U32, velocity Vec3F, volume_ratio F32, affine_x/y/z Vec4F.
// Mass = V0 · material density.

/// Resolved grid node. 32 bytes.
pub struct MatterGridNode {
    pub velocity_mass: [f32; 4],   // after forces and boundaries; w = mass kg (0 = empty)
    pub velocity_before: [f32; 4], // momentum / mass before forces (Liveliness); w = 0
}

/// Deformation gradient for Goo, Snow and Lava (P5). 48 bytes.
pub struct MatterDeformation {
    pub f_x: [f32; 4], pub f_y: [f32; 4], pub f_z: [f32; 4], // F rows; w = 0
}

/// The domain's material. 48 bytes, capacity 1 in v1.
pub struct MatterMaterial {
    /// x = model (0 water, 1 goo, 2 snow, 3 sand, 4 lava), y = rest density kg/m³,
    /// z = μ0 Pa, w = λ0 Pa (Stiffness already applied).
    pub model_density_mu_lambda: [f32; 4],
    /// x = cohesion κ, y = viscosity μ_v Pa·s, z = liveliness β, w = melt m.
    pub dials: [f32; 4],
    /// Model-specific, section 4.4.
    pub model_params: [f32; 4],
}

/// A collider, source, drain or coupled body during a tick. 128 bytes. Up to 64.
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
    pub dims_offset: [u32; 4],         // nodes x/y/z; w = offset into the packed atlas
}
```

Per-body reaction accumulator: `Array(i32)`, 8 per body (linear xyz, angular xyz, 2
padding), fixed point in `m_unit·dx/dt` and `m_unit·dx²/dt` at D5's momentum scale and rounding. Per-tick stats: `Array(u32)`,
16 words (non-finite count, clamp count, live count, max speed, min and max J, volume,
max accumulator magnitude, mass, momentum xyz, kinetic, potential and elastic energy, tick
index; floats as bits). Sums use a fixed reduction tree.

### 3.2 Atoms and ports

Scalars are `ScalarF32`; every numeric param is port-shadowed (DECOMPOSING_GENERATORS.md section 6.2 (Extend before you build)). "Lattice wires" means `grid_bounds: Transform`,
`grid_nodes_x/y/z`, `cell_size`.

| Atom | Inputs → outputs | Phase |
|---|---|---|
| `node.matter_domain` (scene-facing CPU bridge) | the `node.fluid_surface` scene contract of D17, plus params Material (Enum), Points per Cell (Enum 8/27), Stiffness, Cohesion, Viscosity, Liveliness, Melt, and model params → lattice wires, `ticks`, `substeps_per_tick`, `epoch`, `points_per_cell`, `material: Array(MatterMaterial)`, `bodies: Array(MatterBody)`, `shapes: Array(MatterShape)`, `atlas: Array(u32)`, `forces: Array(u32)`, `impulses: Array(u32)`, `simulation_time`, `dropped_seconds`, `lag_seconds` | P1 (domain, walls, clock, water dials, box fill), P2a–P3d (bodies, roles, fields, impulses, coupling), P5 (Melt, models) |
| `node.matter_state` (substep boundary) | `seed: Array(MatterPoint)`, capture `in: Array(MatterPoint)`, capture `stats_in: Array(u32)`, lattice wires, `ticks`, `substeps_per_tick`, `epoch` → `out: Array(MatterPoint)`; provided `grid_accum: Array(i32)`, `grid: Array(MatterGridNode)`; per-iteration `step_dt`, `step_index`, `substep_in_tick`, `tick_start`, `tick_end`; `live_count`, `fault` | P1 |
| `node.matter_fill` | lattice wires, `bodies`, `shapes`, `atlas`, `material`, `points_per_cell`, `epoch` → `seed: Array(MatterPoint)`. Params `seed`, `max_capacity` | P1 (box and fill height), P3b (mesh fills) |
| `node.zero_array` | `in: Array(i32)` → `out` (aliased) | P1 |
| `node.matter_to_grid` | `points`, `material`, lattice wires, `step_dt`, optional `deformation`, optional `order: Array(u32)` (P1b), `accum: Array(i32)` → `accum_out` (aliased, the one atomic output) | P1, P1b |
| `node.matter_grid_update` | `accum`, `grid` (aliased target), lattice wires, `step_dt`, `gravity_x/y/z`, closed-face mask, optional `bodies`, `shapes`, `atlas`, `forces`, `impulses`, `tick_start` → `grid_out` | P1 walls, P2a colliders, P3c forces |
| `node.grid_to_matter` | `points`, `grid` (BufferGather), `material`, lattice wires, `step_dt` → `points_out` | P1 |
| `node.matter_stats` | `points`, `grid`, `accum`, `material`, `tick_end` → `stats: Array(u32)` | P1 |
| `node.matter_frame` | `points`, `stats`, `material`, lattice wires, optional `solid`, `simulation_time` → the surface seam outputs: `particles_a`, `particles_b`, `count_a/b`, `identity_a/b`, `solid_a/b`, `grid_bounds`, `grid_nodes_x/y/z`, `blend`, `span` | P1 |
| `node.matter_move_bodies` | `bodies`, `reaction: Array(i32)`, `step_dt`, `substep_in_tick` → `bodies_out` | P2a |
| `node.matter_solid_distance` | `bodies`, `shapes`, `atlas`, lattice wires, closed-face mask → `solid: Array(f32)` | P2a |
| `node.matter_body_reaction` | `accum`, `grid`, `bodies`, `shapes`, `atlas`, lattice wires, `reaction: Array(i32)` → `reaction_out` (aliased atomic) | P2b |
| `node.matter_emit` | `points`, `bodies`, `shapes`, `atlas`, lattice wires, `tick_start`, `material`, `points_per_cell` → `points_out` | P3b |
| `node.matter_drain` | `points`, `bodies`, `shapes`, `atlas`, lattice wires → `points_out` | P3b |
| `node.matter_compact` | `points`, `tick_end` → `points_out` | P3b |
| `node.matter_update_deformation` | `points`, `deformation`, `material`, `step_dt` → `deformation_out` | P5a |
| whitewater atoms, frame cache | section 13, P6 and P7 | P6, P7 |

The graph ships as one node group, **"Live Matter"**, per GROUPING_GRAPHS.md: the domain
feeds fill and state; the region body is `zero_array → matter_move_bodies →
matter_to_grid → matter_grid_update → matter_body_reaction → grid_to_matter →
matter_drain → matter_emit → matter_compact → matter_stats`; `matter_frame` feeds the
surface design's "Liquid Surface" group, which feeds the scene object.

### 3.3 Ownership and threads

The content thread owns every runtime value. `node.matter_domain` owns the clock, the
prepared-geometry references, the pose table, the coupled `RigidSimulation` for a coupled
pair, the event queue and the reaction readback ring. `node.matter_state` owns the
persistent point buffer, the provided lattice arrays and the stats readback ring.
`physics_world` in coupled mode republishes the accepted rigid frame as it does for FLIP.
Serialized state is the graph JSON only; reload restarts the simulation (as FLIP and
Box3D do).

## 4. Numerical recipe

### 4.1 One substep

Notation: x_p, v_p, C_p, J_p, V0_p per point; m_p = V0_p·ρ; dx cell size; quadratic
B-spline weights per axis with `q = (x − lattice_min)/dx`, `base = floor(q − 0.5)`,
`f = q − base`, `w0 = 0.5(1.5 − f)²`, `w1 = 0.75 − (f − 1)²`, `w2 = 0.5(f − 0.5)²`,
over the 27 nodes `i = base + {0,1,2}³`, `d_i = (i·dx + lattice_min) − x_p`.

1. **Clear** `grid_accum` (`node.zero_array`).
2. **Move bodies** (`node.matter_move_bodies`, P2a+): prescribed bodies take the pose
   interpolated between their tick-start and tick-end poses at `substep_in_tick` (lerp
   translation, slerp rotation) and the matching velocities; dynamic coupled bodies apply
   the previous substep's reaction (Δv = J·m⁻¹, Δω = I⁻¹·L) plus `accel·dt`, then
   integrate position and rotation.
3. **P2G** (`node.matter_to_grid`): stress τ_p from the model (4.4). For each node,
   `mass += w·m_p` and `momentum += w·(m_p·v_p + (m_p·C_p − dt·V0_p·(4/dx²)·τ_p)·d_i)`
   (MLS-MPM fused stress term, Hu 2018; `4/dx²` is Dp⁻¹ for quadratic splines, as in
   Taichi `mpm3d.py` and `mls-mpm88`). Fixed point per D5.
4. **Grid update** (`node.matter_grid_update`), for each node with mass > 0:
   `v_before = momentum/mass`; `v = v_before + dt·(g + a_field(x_i))`, plus the impulse
   lattice once when `tick_start = 1`. Walls: for each closed face within 3 nodes, the
   normal component into the face becomes 0 and the tangential part becomes
   `t̂·max(0, |t| + v_n·friction)` (taichi_elements). Colliders: for each body whose
   distance φ at the node is below 0 (sampled through the body pose), with
   n = ∇φ/|∇φ| and v_rel = v − v_body(x_i): if v_rel·n < 0, remove the normal part and
   apply the same friction rule, then `v = v_body + v_rel'`. Clamp each component to
   ±0.9·dx/dt and count clamps. Write `grid`.
5. **Reaction** (`node.matter_body_reaction`, P2b): for nodes a coupled body touched,
   atomically add `mass·(v_before' − v)` and `(x_i − x_com) × mass·(v_before' − v)` to that
   body's accumulator, where `v_before'` is the velocity after forces and before the
   collider projection.
6. **G2P** (`node.grid_to_matter`): `v_pic = Σ w·v_i`,
   `v_p ← β·(v_p + Σ w·(v_i − v_before_i)) + (1 − β)·v_pic`,
   `C_p = (4/dx²)·Σ w·v_i ⊗ d_i`, `x_p += dt·v_pic`, `J_p ← J_p·(1 + dt·tr C_p)`.
7. **Deformation** (P5 models): `F ← (I + dt·C_p)·F`, then Melt relaxation (D10), then the
   model's return mapping.

Once per tick, gated by `tick_start` or `tick_end` (skipped aliased dispatches call `mark_gpu_accessed`, per FREEZE_COMPILER_MAP.md section 9 (Executor contracts fusion leans on)): drain (P3b), emit (P3b), compaction (P3b), stats.

### 4.2 Seeding

Fills place 2³ or 3³ points per cell (Points per Cell), each jittered inside its
sub-cell by a hash of (seed, lattice index), V0 = dx³/ppc, J = 1, C = 0, velocity from
the role. Points closer than 1.5 cells to the lattice edge are not seeded; walls keep
them inside.

### 4.3 Fixed-point encoding and headroom

`encode(x) = floor(x · Q + u)` after normalizing by `m_unit` (mass, Q_m = 2^16) or
`m_unit·dx/dt` (momentum, Q_p = 2^27), u the D5 hash. A node velocity is
`p_raw / m_raw · dx/dt · Q_m / Q_p`. A node holds about 8 `m_unit` at rest density, so its
momentum at the velocity clamp (0.9·dx/dt) is about 2^30, and it reaches 2^31 only when
compressed to J ≈ 0.5 at the clamp. Real flows sit far below the clamp. `matter_stats`
records the largest accumulator magnitude every tick, and `matter_fixed_point_headroom`
requires it below 2^30 on the Dam Break.

### 4.4 Constitutive branches

| Model | Stress | Parameters (defaults) |
|---|---|---|
| 0 Water | `τ = λJ(J−1)I` for J < 1, `κ·λJ(J−1)I` for J ≥ 1, plus `μ_v·(C + Cᵀ)` | λ = 2.78e5·L·s² Pa; ρ0 = 1000; κ = 0; μ_v = 0; β = 0 |
| 1 Goo (P5a) | fixed corotated `τ = 2μ(F−R)Fᵀ + λJ(J−1)I`, Melt relaxation | E = 1e6·L, ν = 0.2, μ and λ scaled by 0.3; m = 0 |
| 2 Snow (P5b) | fixed corotated with hardening `e^{ξ(1−Jp)}`, singular values clamped to [1−θc, 1+θs], Jp in [0.6, 20], Melt relaxation | θc = 2.5e-2, θs = 7.5e-3, ξ = 10, E0 = 1.4e5 Pa, ν = 0.2, ρ = 400 |
| 3 Sand (P5c) | Drucker–Prager return mapping on Hencky strain, α = √(2/3)·2 sin φ / (3 − sin φ) | φ = 45°; E, ν, ρ per Klár 2016 |
| 4 Lava (P5d) | water pressure plus Bingham deviatoric stress (yield τ_y, plastic viscosity), Melt scales τ_y and viscosity toward zero | τ_y, viscosity, density per Yue et al. 2015 |

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
| Points per cell | 8 default, 27 option | FLIP Fluids upstream seeding (parity); `mpm3d.py` uses 2, noted |
| Fixed-point scales | mass Q_m = 2^16, momentum Q_p = 2^27, unbiased hashed rounding | D5 amendment (BUG-m9g8 (MPM D5 fixed point loses momentum)); the prototype's Q = 2^20 measured mass only |
| Liveliness blend | FLIP-style blend in G2P | Fei et al. 2021. ⚠ VERIFY-AT-IMPL before P1 |
| Goo scale | μ, λ × 0.3 | taichi_elements (`h = 0.3` for elastic) |
| Snow θc, θs, ξ, Jp clamp | 2.5e-2, 7.5e-3, 10, [0.6, 20] | `mls-mpm88` constants (from Stomakhin et al. 2013); taichi_elements uses θs = 4.5e-3, noted and not taken |
| Snow E0, ν, ρ | 1.4e5 Pa, 0.2, 400 kg/m³ | Stomakhin et al. 2013 reference parameters. ⚠ VERIFY-AT-IMPL against the paper's parameter table before P5b |
| Sand φ, α | 45°, formula above | taichi_elements (`friction_angle = radians(45)`) |
| Sand E, ν, ρ | per Klár et al. 2016 | ⚠ VERIFY-AT-IMPL from the paper before P5c |
| Lava yield model | Bingham | Yue et al. 2015. ⚠ VERIFY-AT-IMPL before P5d |
| Melt relaxation time | 0.1 s at m = 0.5 | this design (D10); P5a gesture test sets the feel |
| Viscous explicit limit | dt ≤ ρ·dx²/(6·μ_v) | standard explicit diffusion bound (D4) |
| Body substep term | `0.5·(dx/c)·√(m_b/(ρ0·A_b·dx))` | derived in D12; proven by P2b tests |
| Whitewater rates and energies | wavecrest 175, turbulence 175, energy 0.1–60 | FLIP Fluids defaults as authored in `WaterDamBreak.json` |
| Gravity | authored (World), default (0, −9.81, 0) | the integration plan's first committed shared API |

Worked numbers at the Dam Break setup (L = H = 4 m, 64 cells, Stiffness 1): dx = 0.0625 m,
λ = 1.11e6 Pa, c = 33.3 m/s, v_est = 8.9 m/s, dt_f = 4.94e-4 s, n = 34, dt = 4.90e-4 s.
Test `matter_substep_rule_matches_worked_example` pins this.

## 5. Coupling protocol

Box3D steps once per 1/60 s tick; the fluid's substeps see each body move every substep;
the fluid's net impulse for tick k reaches Box3D before Box3D steps tick k. With display
one tick behind (surface D10), the content thread never waits.

Per display frame N, inside the contracted coupled group (liquid side first, as
`prepare_coupled_scenes` orders it today):

1. `node.matter_domain` checks the reaction ring slot of fluid tick k (encoded in frame
   N−1) with `FrameFence::is_completed`. Not complete → publish `ticks = 0`; the pair
   holds and republishes the previous pair. Lag grows and is reported.
2. Complete → read the per-body accumulators, convert to world impulses, and run
   `RigidSimulation::advance_with_coupling` with `AdvancementPolicy::Worker { max_ticks: 1 }`
   and a `StepCoupling` implementation (`MatterCoupling`) whose single `exchange` applies
   the impulses through `PhysicsWorld::apply_impulses` and whose `finish` captures each
   body's pose, velocities, inverse mass, world inverse inertia and predicted external
   acceleration (`PhysicsWorld::dynamics`, as FLIP's exchange does). Box3D steps tick k
   with its own contact substeps.
3. Upload the body table for fluid tick k+1 (poses at the end of Box3D tick k) and publish
   `ticks = 1`.
4. The GPU runs fluid tick k+1: every substep moves the bodies (4.1 step 2) and
   accumulates reaction (step 5). The reaction slot for tick k+1 is ready for frame N+1.
5. Presentation at s = t_A: the frame's `particles_a` is the fluid at the end of tick k,
   and the rigid frame accepted in step 2 is Box3D at the end of tick k. They match.

The GPU body integrator's free-flight baseline equals Box3D's integration of the same
gravity and fields, and the read-back impulse contains only the fluid's reaction, so
nothing is counted twice. Uncoupled scenes skip steps 1–3 and are free-running under D8.

**Consequences, stated honestly:** a coupled scene advances at most one tick per display
frame, so at a 30 fps project it runs at half speed live (export is unaffected); a missed
readback leaves a permanent one-tick lag until Reset; Box3D contacts act once per tick
while the fluid feels the body every substep, so a body pinned against a wall by water
can jitter by up to one tick of fluid push.

## 6. The interaction surface

Every row reaches `node.matter_domain` through a route that exists today. "Just works"
means no new port, command, list or UI once the P3a predicate is in.

| Control | Route | Status |
|---|---|---|
| Gravity XYZ, Simulation Speed, Reset, Seed, shared with World | same param names as `node.fluid_surface`; World `node.value` wires; exposure migration | Just works (P3a) |
| Keyframes, LFOs, beat drivers, envelopes, audio modulation, MIDI/OSC mapping on any exposed param | existing modulation and mapping on port-shadowed params | Just works |
| Force fields: Add Force (uniform, radial, vortex, masks, composition), Targets chooser | `acceleration_field: VectorField`; Scene Forces recipes; recipient resolver via `liquid_domain_of` | Just works (P3a walk, P3c sampling) |
| Impulses: Fire, Clip Edge, audio transient, Both; MIDI/OSC-mapped Fire; beat-quantized triggers | existing impulse hooks, `EventQueue`, `TriggerFireMode`, trigger primitives | Just works once `node.matter_domain` implements the hooks (P3c) |
| Box3D bodies in the scene: fixed and animated as colliders, dynamic as two-way bodies | `prepare_coupled_scenes` membership and the coupling hooks | Just works (P2b, P3a) |
| Roles on scene objects: Initial Fill, Inflow (enabled, velocity, inherit motion), Drain (enabled), Collider (friction) | `FluidRole` wires and the Scene Panel role commands | Just works (P3a predicate, P3b behaviour) |
| Pour on/off | the role's `enabled` | Just works |
| Domain position and size, six closed faces, resolution, grid budget, fill height, density | setup params with FLIP's names; domain gizmo | Just works (setup edits restart, as FLIP) |
| Points per Cell, Material | new setup Enums on `node.matter_domain` | New params, P1 and P5 (restart) |
| Stiffness, Cohesion, Viscosity, Liveliness | new port-shadowed params on `node.matter_domain`, shown through `param_surface.rs` rows | New ports, P1 (water dials) and P3d (live verification, Viscosity) |
| Melt (melt and freeze on the beat) | new port-shadowed param on `node.matter_domain` | New port, P5a |
| Model params (hardening, friction angle, yield) | new params on `node.matter_domain` | New, P5 phases |

## 7. Look — artefacts, metrics and dials

**The honest look risks, each with its one dial:**

| Risk against FLIP | Cause | Dial | Cost of turning it |
|---|---|---|---|
| Dissipation: calmer water, splashes die early, small eddies vanish | APIC/PIC transfers smooth at grid scale | Liveliness β | more particle noise; high β can look grainy |
| Springiness: the pool bounces, waves ring | weak compressibility (Mach ≈ 0.27) | Stiffness s | substeps scale with s (s = 2 → n = 61) |
| Fake tension: water clumps into gel blobs | tension retained for J > 1 | Cohesion κ (default 0) | 0 means no surface tension at all; real tension is deferred |
| Thin sheets tear into droplets early | too few points to hold a sheet | Points per Cell (8 → 27) | 3.4× cost |
| Volume drift | particle and grid divergence disagree | none — gated, escalated | — |
| Grid-aligned ridges | lattice-aligned seeding or transfer artefacts | none — gated, escalated | — |

**Named artefacts that fail a gate.** Every metric is computed by the look-metrics mode
of `examples/fluid_capture.rs` (P1 adds particle metrics, P4 adds mesh metrics), from the
same captured frames Peter looks at.

| Artefact | Metric | Fails when | Phase |
|---|---|---|---|
| A1 Grid-aligned ridges in the particles | 16-bin histogram of fractional cell coordinates of interior points, per axis, at t = 1.5 s and t = 8 s | any bin > 1.5 × the mean | P1 |
| A2 Bounce after settling | settle time = first t with mean speed < 0.02 m/s; afterwards, peak-to-peak of the mean free-surface height over any 2 s window | settle later than 10 s, or ringing > 5 mm | P1 |
| A3 Volume drift | Σ V0·J against the initial volume | > 3% at any tick of the Dam Break; > 1% after settling; > 1% over 60 s of still pool | P1 |
| A4 Wrong dam-break speed | front position against Martin & Moyce 1952 | > 10% off for T in [1, 3] | P1 |
| A5 Splash loss (particles) | fraction of points detached from the main body (connected at 1.5 × point spacing), max over t in [0.5, 3] s | < 0.6 × FLIP's | P1 |
| A6 Sheet loss (particles) | fraction of points with a planar neighbourhood (smallest/largest covariance eigenvalue < 0.1, ≥ 6 neighbours within 2 spacings), max over t in [0.5, 3] s | < 0.6 × FLIP's | P1 |
| A7 Sheet loss (surface) | peak surface mesh area over t in [0.5, 3] s | < 0.8 × FLIP's | P4 |
| A8 Splash loss (surface) | detached mesh components of at least one cell of volume, summed over frames in t in [0.5, 3] s; airborne mesh volume | components < 0.5 × FLIP's, or airborne volume < 0.6 × FLIP's | P4 |
| A9 Grid-aligned ridges on the surface | fraction of surface area whose normal is within 5° of a grid axis | > FLIP's + 5 percentage points | P4 |
| A10 Slow settle | settle time as A2 | > 1.25 × FLIP's | P4 |
| A11 Volume against FLIP | enclosed mesh volume after settling | differs from FLIP's by > 3% | P4 |

FLIP runs at its preset defaults for these comparisons, at the same domain, column,
resolution, points per cell and Liquid Surface settings. Thresholds are defaults: if
Peter's visual call contradicts a pass or fail, the threshold is re-baselined from his
call and recorded in the phase's decision bead. P1 and P4 capture at Liveliness 0 and
0.9; the gate is evaluated at the default, and both sets of numbers go to Peter.

## 8. Speed — roofline, profile and levers

Per substep: 4 dispatches (clear, P2G, grid update, G2P), plus 2 with bodies; per tick,
5 more (sort, drain, emit, compaction, stats). At n = 34: about 140 dispatches per frame
uncoupled and 210 coupled, plus the surface chain.

Memory traffic per frame at the gate (bytes from DRAM, perfect caching of the grid):

| Term | Value |
|---|---|
| Points × substeps | 500,000 × 34 |
| Point traffic per point-substep | 176 B (P2G reads 80; G2P reads 16, writes 80) |
| Lattice nodes (64 cells + 1 + 2 × 3 padding per axis) | 71³ = 357,911 |
| Lattice traffic per substep | ~64 B per node (clear 16, resolve read 16 and write 32) |
| Total per frame | 2.99 GB + 0.78 GB = 3.77 GB |
| At 546 GB/s (M4 Max, 40-core GPU) | **6.9 ms** |
| At 410 GB/s (M4 Max, 32-core GPU) | **9.2 ms** |
| Global atomic adds per frame, P1 baseline (not in the bytes above) | 500,000 × 34 × 108 = 1.84e9; Apple GPU atomic throughput is unmeasured here |

**The 6 ms gate is at or beyond the bandwidth roofline before atomics and dispatch
overhead.** Real kernels reach 40–70% of peak bandwidth, so expect 10–20 ms before
optimisation. The 128³ stretch is 16× the work per simulated second and is out of reach;
P1b and P4 report it. The only published real-time MLS-MPM numbers found are WebGPU-Ocean
(about 100,000 points on integrated graphics, about 300,000 on desktop GPUs, 2 steps per
frame at a timestep its author reports as occasionally unstable) and Zhao et al. 2021
(1.33M snow points at 68.5 fps on four V100s). The brief's "about 1M particles at 60 fps
on M-series" was not found in any source (section 16, R1).

**P1b's per-kernel profile** reports GPU time per frame for clear, sort, P2G, grid update,
G2P, stats and the per-tick bookkeeping, plus dispatch count and the out-of-tile fraction.

**Levers, in P1b's order, with expected wins (estimates; P1b measures each):**

| Lever | Look | Expected win | Basis |
|---|---|---|---|
| L1 Cell-sorted block-local P2G (D6), sort once per tick | none (bit-identical) | global atomics fall from 108 to about 2 per point (864 tile flushes for 512 points per block); P2G 2–4× faster if atomics dominate | arithmetic; Gao et al. 2018 for the scheme |
| L2 Shared-memory grid tiles in G2P | none (bit-identical) | 1.2–1.5× on G2P; the 27-node gather mostly hits cache already | arithmetic |
| L3 Half-precision C storage (D21) | none if gates hold | −18% point traffic (176 → 144 B) | arithmetic; accepted only if every P1 gate passes unchanged |
| L4 Substeps from the rule (D4) | — | a calm or soft setting pays for what it needs (Stiffness 0.5 → 21 substeps) | already the rule; P1b verifies it tracks Stiffness live |
| L5 Dispatch trimming | none | measured; fusion of adjacent barrier-free atoms per `graph-tool fusion` | FREEZE_COMPILER_MAP.md |

Levers only Peter can pull, priced for P4:

| Lever | Effect | Look cost |
|---|---|---|
| Points per Cell stays 8 but fewer cells (resolution 48) | ~0.4× | coarser everything |
| Stiffness 0.5 | ~0.6× | visibly springy (A2 risk) |
| Budget 6 → 12 ms | none | frame budget for the rest of the show |
| Fused G2P2G kernel (Zhao 2021) | fewer dispatches, ~10% bytes | none, but needs a no-monolith exemption |
| Velocity-aware substeps from read-back speed | up to −20% in calm scenes | live runs stop being bit-deterministic |

**Instrument consequences:** one tick of display latency (D10); slow motion instead of
lag under overload (D8); the cost moves with Stiffness, so a Stiffness sweep is also a
cost sweep; a coupled scene at 30 fps runs at half speed (section 5).

## 9. Section 2.5 audit and codegen classification

Per DECOMPOSING_GENERATORS.md section 2.5 (Precondition: audit by analogy before workflow step 1). Survey: `rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/ -g '*.rs'` (322 registered type ids at `c8961489d`). Reference presets read end to end:
`WaterDamBreak.json` (FLIP node, column Transform, moving box, whitewater wiring),
`FluidSim3D.json` (particles into a flat 3D accumulator, fixed-point resolve).

| Candidate | Finding | Shape and argument |
|---|---|---|
| Scene-facing domain | **One wire away** — `node.matter_domain` mirrors `node.fluid_surface`'s scene contract | New node, existing contract (D17). |
| Grid clear | **New**, generic — `node.zero_array` | No clear or fill atom exists. Reusable for any accumulator. |
| P2G | **New** — `node.matter_to_grid` | `node.draw_particles_3d` splats nearest-voxel energy in wrapped unit space with one u32; P2G needs a 27-node signed stencil and the stress term. |
| Grid update | **New** — `node.matter_grid_update` | No per-node velocity-resolve atom. |
| G2P | **New** — `node.grid_to_matter` | No gather from a lattice to scene-space points with an affine output; `node.sample_volume_at_particles` samples `Texture3D` for unit-space `Particle`. |
| State boundary | **New**, riding the state-capture and persistent-resource machinery `node.array_feedback` uses | `array_feedback` is a one-frame `Particle` delay, not a clocked substep boundary; extending it would change its purpose (DECOMPOSING_GENERATORS.md section 6.2 (Extend before you build)). |
| Initial fill | **New** — `node.matter_fill` | `node.spawn_particles` emits unit-space `Particle` at hashed positions. |
| Emit, drain, compaction | **New** — three atoms | `node.spawn_from_mesh` is the scan-and-place precedent (`BarrieredReduction`). |
| Stats reduction | **New** — `node.matter_stats` | Precedent `peak`/`luminance`. |
| Frame publication | **One wire away** — `node.matter_frame` writes the surface seam's exact outputs | Specified by the surface design's section 3.2. |
| Distance lattices | **New** CPU module `P/sdf.rs`, derived on `PreparedFluidGeometry` | No mesh SDF exists; input geometry is one wire away. |
| Roles, forces, impulses, coupling | **One wire away** — existing wires and hooks | Section 6. |
| Body integrator, reaction, solid lattice | **New** — three atoms | Box3D is CPU and steps per tick. |
| Cell sort (P1b) | **One wire away** — the surface design's `node.sort_particles_into_cells` plus an `order` output | If surface P5 has not landed, P1b builds it to that design's D17 spec with the extra output; surface P5 then finds it built. |
| Visual particles | **Exists** — `FluidParticle` → `particles_to_copies` → scene objects (surface P3) | No re-encoding into the 64-byte `Particle` family. |
| One `mpm_solver` node | **Forbidden** | DECOMPOSING_GENERATORS.md section 1.1 (No fused single-effect or single-generator monoliths). |

Fifteen new atoms through P3d (sixteen with P5a's deformation update) plus the SDF
module; the honest count.

| Atom | Class | Proof |
|---|---|---|
| `zero_array`, `matter_grid_update`, `grid_to_matter`, `matter_move_bodies`, `matter_solid_distance`, `matter_drain`, `matter_update_deformation` | Barrier-free per element: `wgsl_body` + `fusion_kind` + `input_access` (`BufferGather` for lattice, atlas and body reads), pipeline from `standalone_for_spec::<Self>()` | Value `gpu_tests` against CPU-computed expected output; fused-vs-unfused proof for every pair `graph-tool fusion` places in one region. |
| `matter_to_grid`, `matter_body_reaction` | Atomic scatter, one atomic output each, declared exactly as `scatter_particles_3d.rs:95-98` (Boundary, `atomic_outputs`, standalone codegen); after L1, `matter_to_grid` uses workgroup memory and barriers (exclusion 1). ⚠ VERIFY-AT-IMPL the `boundary_reason` the precedent carries | Values against the f64 reference with the fixed-point tolerance; bit-identity between baseline and L1. |
| `matter_emit`, `matter_compact`, `matter_stats` | Exempt, exclusion 1 of the ADDING_PRIMITIVES.md "The codegen path is mandatory" scope test (multi-pass scan, barriered reduction), `standalone_for_boundary_spec` | Values against CPU scans and sums, including sizes 1, 255, 256, 257 and 2²⁰+3. |
| `matter_state`, `matter_frame` | Exempt, exclusion 2 (cross-frame state) | Clock and ring unit tests; frame value tests. |
| `matter_domain` | Exempt, exclusion 3 (CPU bridge) | CPU unit tests; upload round-trip GPU test. |

Helper functions shared by several bodies (stencil weights, fixed-point encode) are
duplicated with an atom-specific prefix and pinned equal by a source test, unless P1
read-back finds that fused codegen already namespaces member helpers (⚠ VERIFY-AT-IMPL:
read `R/freeze/codegen/fused.rs`). A body the codegen cannot express is BLOCKED: file a
`bd` bug naming the missing read path and declare `boundary_reason: Blocked` — never a
quiet exemption.

## 10. Reuse contract and forbidden moves

**Every seam rides an existing system:**

| Need | Rides on | Never |
|---|---|---|
| "Is this a liquid domain" | one `is_liquid_domain` predicate replacing the literals (P3a) | a matter branch at each site |
| Scene object → its domain | `liquid_domain_of`, walking upstream through the surface group | a matter-object registry |
| Roles on objects | `FluidRole`, `FluidRoleKind`, the Scene Panel role commands | matter role kinds or commands |
| Add Fluid | `AddSceneFluidCommand`, extended in P4b | an Add Matter command or panel |
| Inspector and controls | param surfaces (`param_surface.rs`), scene exposure tables | bespoke rows, drawers or a matter inspector |
| Animated pose history | `manifold-physics::input::InputHistory`, `physics_sampling` | a matter pose buffer |
| Forces | `VectorField`, `acceleration_field`, Scene Forces recipes | matter force nodes |
| Impulses and triggers | `EventQueue`, the impulse hooks, `TriggerFireMode` | a matter trigger router |
| Distance fields | `manifold-physics::sdf` on `TriangleMesh`, derived on `PreparedFluidGeometry` | a renderer SDF or a second geometry cache |
| Rigid coupling | `RigidSimulation`, `StepCoupling`, `RigidSceneObservation`, `CoupledRigidFrame`, `dynamics`, `apply_impulses`, `prepare_coupled_scenes` | a private Box3D world or matter rigid owner |
| Surface and visuals | the particle-frame seam, `FluidParticle`, every surface atom, the Liquid Surface group, `particles_to_copies` | matter surface atoms or a private renderer |
| State across frames | the state-capture and persistent-resource machinery `node.array_feedback` uses | a new persistence mechanism |
| Cell sort | the surface design's `sort_particles_into_cells` | a matter sort |
| Clock, ticks, transport | `TickStamp`, the integration plan's timing table | a second transport policy |
| Bake | `fluid_cache.rs` writer and the take journal | a second cache format |

**Forbidden moves, by the temptation you will feel:**
- "If the GPU solver is unavailable or slow, fall back to FLIP." No availability branch
  exists; the solver is a graph node the scene wires. Faults are named errors; overload
  is visible slow motion.
- "Add a solver dropdown to `node.fluid_surface`." No — separate nodes, one predicate.
- "Add `|| type_id == "node.matter_domain"` at each site." No — the predicate, once.
- "Copy FLIP's role preparation into the domain node." No — consume the prepared
  `FluidRole` payload; extract shared preparation in place if a piece is FLIP-coupled.
- "Keep a matter SDF cache keyed by mesh hash." No — the lattice is derived on the
  prepared geometry it describes.
- "Auto-lower resolution or particles when frames drop." No — the Speed gate and Peter's
  levers.
- "Re-encode matter into the 64-byte `Particle` so the old particle atoms work." No —
  `FluidParticle` and `particles_to_copies`.
- One `mpm_solver` node; a float compare-exchange loop; recomputing density from grid
  mass; velocity or density faults; damping to hide springiness; a dt-halving
  trajectory test; blocking on the reaction readback; stepping Box3D per substep or
  exchanging after the fact without the GPU body integrator; `Texture3D` for the grid;
  reordering `MatterPoint` storage; a second atomic output for diagnostics; the
  prototype's constants; subgroup operations; tuning a look dial or threshold to pass a
  gate.

## 11. FLIP feature map

Every feature in FLUID_ENGINE_INTEGRATION_PLAN.md is ported, deferred with a trigger, or
FLIP-only.

| FLIP feature | Here |
|---|---|
| Domain Transform, six closed faces, resolution, grid budget, fill height, legacy `initial_volume` | Ported P1 (same names, same `domain_layout`) |
| Mesh roles: Initial Fill, Inflow, Outflow, Collider; enable, velocity, inherit motion, friction | Ported P2a (colliders), P3b (fills, inflows, drains) |
| Legacy `emitter`/`obstacle` Transform inputs | FLIP-only (legacy presets) |
| Gravity XYZ, Simulation Speed, Reset, Seed, World sharing | Ported P1, P3a |
| Liquid density for coupling | Ported P2b |
| Shared force fields and impulses, 24/30/60 fps input equality | Ported P3c |
| Two-way Box3D coupling, paired presentation | Ported P2b |
| Viscosity | Ported P3d |
| Surface tension (native) | FLIP-only; Cohesion is the fake (D3); real tension deferred |
| Whitewater | Ported P6 |
| CPU mesher, Surface Detail, mesh smoothing | FLIP-only; the surface group replaces them |
| PIC/FLIP `transfer` blend | Liveliness is the matter counterpart (D3) |
| Record/Playback caches, takes, cache identity | P7 adds matter frame caches on the same writer |
| Quality tiers (integration plan P10), custom GPU fields (P11) | Deferred with those plans' triggers |
| Add Fluid, Scene Panel roles, domain gizmo | P3a (predicate), P4b (command) |

## 12. Invariants and enforcement

| Invariant | Enforcement |
|---|---|
| GPU transfers match the f64 reference at small N | `matter_transfer_matches_reference` (one substep, 512 points, 16³: positions within 1e-5 m and velocities within 2e-5 m/s of the f64 reference with Q = 2^20 fixed-point rounding; velocities within 2e-4 m/s of the continuous f64 reference, because one mass LSB at a low-mass stencil-edge node moves its velocity by about 1e-4 m/s); `matter_hundred_substeps_match_reference` (affine field fixture) |
| Mass is exact; grid mass matches particle mass | `matter_grid_mass_matches_particle_mass` (relative 1e-5 per substep) |
| Momentum in free flight | `matter_momentum_conserved_free_blob` (zero gravity, no walls touched, relative change ≤ 1e-4 over 60 ticks) |
| A still pool settles | `matter_still_pool_settles` (after 5 s: mean speed < 0.01 m/s; bottom-quarter J matches 1 − ρgd/λ within 20%) |
| Dam-break energy never grows | `matter_dam_break_energy_bounded` (kinetic + potential + elastic ≤ 1.01 × initial at every tick) |
| Look artefacts A1–A6 | `matter_look_lattice_alignment`, `matter_look_settles_without_ringing`, `matter_look_volume_drift`, `matter_dam_break_front_matches_martin_moyce`, `matter_look_splash_retention`, `matter_look_sheet_retention` (section 7). ⚠ VERIFY-AT-IMPL: transcribe the Martin & Moyce aspect-2 series and its T definition, with the citation, into the test |
| Look artefacts A7–A11 | `matter_surface_look_against_flip` (P4) |
| Determinism | `matter_deterministic_under_seed` (two runs, 120 ticks, bit-identical points); `matter_seed_changes_jitter`; `matter_block_p2g_bit_identical` (P1b) |
| Fixed-point headroom | `matter_fixed_point_headroom` (Dam Break, max accumulator magnitude < 2^30) |
| A non-finite tick is never published | `matter_nonfinite_tick_not_published` |
| Substep rule | `matter_substep_rule_matches_worked_example` (n = 34); `matter_substeps_follow_stiffness` (0.5 → 21, 2 → 61); `matter_dials_limited_to_substep_cap` (a request needing n > 128 runs at the largest fitting value and reports it) |
| Live never spirals; export never drops | `matter_live_caps_ticks_per_frame`; `matter_export_runs_every_tick` |
| Frames id-sorted, ids unique in an epoch | `matter_frame_ids_strictly_increasing` through fill, emit, drain, compaction; `matter_identity_epoch_renumbers_near_limit` |
| Collider penetration bounded | `matter_collider_penetration_bounded` (rotating box: particle φ ≥ −0.5·dx) |
| Coupling | `matter_coupling_hydrostatic_force` (within 5%), `matter_coupling_floating_equilibrium` (density 0.5 settles at the waterline ± 0.5·dx), `matter_coupling_energy_light_body` (ratios 0.1/1/10: body energy never above 1.01 × initial total over 8 ticks), `matter_coupling_free_flight_matches_box3d`, `matter_coupling_presentation_shares_display_time` |
| Coupled pair never blocks live | `matter_coupled_holds_when_reaction_pending`; negative gate: `rg -n 'wait_until_completed\|commit_and_wait' crates/manifold-renderer/src/node_graph/primitives/matter_*.rs` returns nothing |
| Forces and impulses | `matter_impulse_once_per_tick_across_substeps`; `matter_force_lattice_matches_field`; `matter_input_stream_24_30_60` |
| One liquid-domain predicate | negative gate: `rg -n '"node\.fluid_surface"' crates -g '*.rs'` returns hits only in `manifold-core/src/liquid_domain.rs`, `R/primitives/fluid_surface.rs` and test code |
| No FLIP fallback | negative gate: `rg -n -i 'fallback\|fall back' crates/manifold-renderer/src/node_graph/primitives/matter_*.rs` returns nothing |
| Every barrier-free atom on codegen | the existing classify source scans plus each atom's value test; `graph-tool fusion` output recorded per phase |
| No new shared locks | negative gate: `git diff origin/main -- crates \| rg '^\+.*Arc<(Mutex\|RwLock)'` returns nothing |
| Solver budget | `matter_solver_perf` (P1b, P4), p95 against 6 ms |

## 13. Phasing

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
- **Gate:** `cargo nextest run -p manifold-renderer substeps_` green; clippy clean; every
  existing `execution_plan` test green.
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
  setting per-iteration scalars before each run, through one extracted step evaluator (no
  recursive `execute_frame_with_state`); region resources held for the whole repeat;
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
- **Phase notes (built 2026-09-30, Opus 5.5 worker):**
  - One step evaluator: the frame loop body became `Executor::run_step`; the frame
    pass and the region driver (`R/execution/substep_region.rs`) both call it.
    `capture_step` serves the frame-end late pass and each iteration's capture, and
    now drains typed writes like `evaluate` does (no existing `late_capture` writes
    one, so feedback behaviour is unchanged).
  - Deviation: `substep_iteration(iteration, scalars: &mut [f32]) -> bool` fills one
    value per declared iteration scalar, matching P0a's list, instead of the
    historical fixed triple. The boundary resolves its count in `evaluate`.
  - Per-iteration uniforms: primitives pass uniforms as `GpuBinding::Bytes`, which
    Metal copies at encode time (`setBytes`), so every iteration's dispatch already
    owns its bytes; there is no shared arena to slice. The Vulkan backend must keep
    that per-dispatch copy (push constants or a per-dispatch ring).
  - Per-frame diagnostics (the wire tap, preview scalars, dumps) record a step on its
    first visit only. A physics sample never runs a region. A boundary asking for
    more than 4,096 iterations in a frame stops with a graph error.
  - Freeze: `partition_regions` refuses unions and stencil absorption across a
    substep border, using the plan compiler's own `substeps::region_body`, and the
    in-place loop test accepts a boundary's state port as a loop head, so a fused
    body writes the state in place exactly as the unfused one does.
  - Tests: `substeps_count_order_and_zero_steps`, `substeps_final_state_escapes`,
    `substeps_no_recycle_between_iterations`, `substeps_execute_post_once`,
    `substeps_physics_sample_never_advances_region`,
    `substeps_freeze_never_fuses_across_border` (with a no-boundary control); GPU
    `substeps_uniforms_distinct_per_iteration` and `substeps_frozen_unfrozen_match`
    (bit-identical, both in place) in `tests/gpu_proofs/substeps.rs`, over the
    shared fixtures in `substeps::test_nodes`.

### P1 — Water kernel, look gates and the cost probe

- **Entry state:** P0b merged; surface P1–P3 merged: `rg -n 'fn capture_particle_frame' crates/manifold-fluids/src`, `rg -n 'pub struct FluidParticle' crates/manifold-renderer/src/node_graph/fluid_particles.rs`, `rg -n 'node.particles_to_copies' crates/manifold-renderer/src/node_graph/primitives`. Content-thread frame fence reachable (surface P2).
- **Read-back:** D3–D10, D14, D15, D17–D21; sections 3, 4, 7, 9, 10; ADDING_PRIMITIVES.md
  whole; the branch kernels in section 1.2 (read only). Restate the forbidden moves.
- **Deliverables:** `R/matter.rs` records, fixed-point helpers and `pub fn
  substeps_per_tick(dx: f32, wave_speed: f32, v_est: f32, body_limit: Option<f32>,
  viscous_limit: Option<f32>) -> u32`; `R/matter/reference.rs`; atoms `matter_domain`
  (domain, walls, box fill, fill height, clock, gravity, speed, reset, seed, Points per
  Cell, Stiffness, Cohesion, Liveliness), `matter_fill`, `matter_state`, `zero_array`,
  `matter_to_grid` (global atomics), `matter_grid_update` (walls, gravity),
  `grid_to_matter`, `matter_stats`, `matter_frame` (solid lattice = walls only).
  Presets `WaterStillPoolMatter.json` and `WaterDamBreakMatter.json` ("Water — Dam Break
  (Live GPU)"), particle view through `particles_to_copies`, same domain, column and
  resolution as `WaterDamBreak.json`. The particle look-metrics mode of
  `examples/fluid_capture.rs` (A1–A6 on matter and on FLIP particle frames). A `matter_`
  entry in `scripts/landing_gate.py` GPU proof scope (⚠ VERIFY-AT-IMPL its structure).
  Tests from section 12 (reference, mass, momentum, still pool, energy, A1–A6,
  determinism, headroom, non-finite, substep rule, tick cap, frame ids). Probe
  `tests/gpu_proofs/matter_cost_probe.rs`: ns per point-substep at 500,000 points.
- **Gate:** tests green; A1–A6 pass at Liveliness 0 (numbers at 0.9 reported); clippy;
  `cargo run -p manifold-renderer --bin check-presets`; `cargo run -p manifold-renderer
  --bin graph-tool -- validate <preset> --kind generator` and `fusion` for both presets,
  output recorded.
- **Kill check:** projected Dam Break solver time = probe × 500,000 × 34 + lattice term.
  Above 12 ms: stop and escalate to Peter with section 8's tables before P1b.
- **Demo:** `fluid_capture --preset WaterDamBreakMatter --frames 600 --stills-every 15`
  into `/tmp/manifold_matter_p1`, and FLIP's Dam Break particle view into
  `/tmp/manifold_flip_p1`. L2: Peter looks at both.
- **Gesture:** pause and resume transport mid-splash; the water holds exactly and resumes
  without a jump.
- **Forbidden:** everything in section 10; a CPU fallback; colliders (P2a); tuning a dial
  or threshold to pass A1–A6.
- **Phase notes (partial, 2026-09-30, Opus 5.5 worker):** the kill check fired and the
  lead took the proceed option: P1b runs as designed, and the budget is decided at P4.
  The budget call is BUG-u3ov (MPM solver budget); what P1 still owes is BUG-g93n (MPM P1 remaining deliverables).
  - Built and green: the records, substep rule, clock and f64 reference in `R/matter.rs`;
    the atoms `matter_domain`, `matter_fill`, `matter_state`, `zero_array`,
    `matter_to_grid`, `matter_grid_update`, `grid_to_matter`, `matter_stats`,
    `matter_frame`; 15 GPU proofs (`scripts/gpu_proofs_gate.py --filter matter_
    --filter substep`): reference, mass, hundred substeps, still pool, energy,
    determinism, seed jitter, headroom, non-finite tick and gravity, frame ids, live
    Stiffness, the probe.
  - Kill check, from `tests/gpu_proofs/matter_cost_probe.rs` on an M4 Max with no other
    GPU work (load average 18.6 from CPU jobs). 64³ cells, 4 m, Stiffness 1, n = 34,
    medians of 30 frames, ms per frame, kernels profiled one encoder per dispatch:

    | Points | Clear | Sort | P2G | Grid update | G2P | Stats | Production frame | ns per point-substep |
    |---|---|---|---|---|---|---|---|---|
    | 131,072 | 1.38 | n/a | 11.44 | 0.88 | 1.99 | 0.35 | 15.76 | 3.01 |
    | 262,144 | 1.39 | n/a | 21.96 | 0.98 | 3.71 | 0.39 | 28.07 | 2.88 |
    | 524,288 | 1.39 | n/a | 44.95 | 0.95 | 7.54 | 0.55 | 54.50 | 2.94 |

    Projection: 2.94 ns × 500,000 × 34 + lattice 2.34 + stats 0.55 = **53 ms against
    12 ms: fires.** P2G is bound by its 108 global atomic adds per point (about 4.3e10
    adds per second). G2P already runs at about 420 GB/s, near section 8's 546 GB/s
    roofline. With every P2G atomic removed, the rest still costs about 12.6 ms.
  - Deviations: the lattice and material reach the atoms as scalar wires from
    `matter_domain`. The boundary serves six iteration scalars (step_dt, step_index,
    substep_in_tick, tick_start, tick_end, tick_index). Walls act on the face node plus
    the three padding nodes; without the face node a still pool sinks a cell and
    splashes. `matter_stats` is hand WGSL under exclusion 1. Live runs allow three ticks
    per frame and carry one tick of jitter debt. `matter_domain` declares `NonGpu`. A
    point whose stencil leaves the lattice is removed (id 0). The fill rounds the fill
    height to whole cells. The frame ring uses shared storage.
  - Tolerance: section 12 now names both oracles. Measured gaps are 5.4e-6 m/s against
    the fixed-point f64 reference and 1.05e-4 m/s against the continuous one.
  - Verified: the Liveliness blend and position update follow Fei et al. 2021 as Blatny
    & Gaume 2025 implement it (positions advect with v_pic). The scatter precedent
    `scatter_particles_3d.rs` carries `boundary_reason: Blocked` with no tracked gap,
    now BUG-1ois (atomic scatter atoms declare Blocked). Fused codegen does not
    namespace member helpers, so shared helpers are duplicated with an atom prefix.
  - Built since: `particles_to_copies` (moved here from the surface design's deferred
    P3) and `matter_to_particles` (index-keeping records for D6's sort), each with
    value and fused proofs; `WaterDamBreakMatter.json` and `WaterStillPoolMatter.json` on
    the Live Matter group (the moving box and whitewater wait for P2a and P6; the pool
    rounds to whole cells); `matter::look` and the gates in
    `tests/gpu_proofs/matter_look.rs`; helper-copy source tests.
  - Verified: `landing_gate.py` scopes GPU proofs by rows of path substrings and test
    filters, and any uncovered GPU path runs the full suite; the matter row filters
    `matter_` and `substeps_`. Martin & Moyce 1952 Figure 3, n² = 2, a = 2.25 in,
    transcribed from PySPH's `db_exp_data.py` into `matter::look`.
  - Gates after the D5 amendment, Dam Break as the preset: A1 1.02–1.03 (pass); A5 1.44%
    against FLIP's 1.34% (pass); A6 0.85% against FLIP's 27.3% (fail); A4 11.0% at
    Liveliness 0 and 2.0% at 0.9, ahead of the experiment by 8–10% from T ≈ 1.3 (fail);
    free-blob momentum 1.2e-4 against 1e-4 (fail, cause not found). A2, A3, the still
    pool and headroom fail through BUG-8akp (MPM water J grows without bound): at
    Cohesion 0, λ·J(J−1)·0 turns NaN near t = 1.9 s.
  - Owed with BUG-g93n (MPM P1 remaining deliverables): the `fluid_capture` metrics
    mode, after the surface branch merge; every gate green after the J ruling; the demo.

### P1b — Profile and optimise

- **Entry state:** P1 merged and its kill check passed.
- **Read-back:** D6, D20, D21; section 8.
- **Deliverables:** `tests/gpu_proofs/matter_solver_perf.rs` behind a new
  `matter-perf-proofs = ["gpu-proofs"]` feature shaped like `rt-perf-proofs`
  (`crates/manifold-renderer/Cargo.toml`, `tests/gpu_proofs/rt_dynamic_perf.rs`): Dam
  Break Matter at 64³ with the pool deepened until the live count is ≥ 500,000 (count
  reported), 16 warm-up and 120 measured frames, per-kernel GPU time via
  `GpuTimestampSampler` (the `src/bin/freeze_profile.rs` pattern), dispatch count,
  out-of-tile fraction, and the 128³ / 30 Hz stretch. Levers L1–L5 of section 8 applied
  in order, each with a before/after row in the phase report. L1 needs the cell sort
  (section 9 row). L3 is kept only if every P1 test and look gate passes with unchanged
  thresholds; otherwise it is reverted in this phase and its numbers recorded.
- **Gate:** every P1 test and look gate green; `matter_block_p2g_bit_identical`; the
  profile table and p95 against 6 ms reported. A miss here is carried to P4, not fixed by
  look levers.
- **Demo:** none — L1, plus the profile table.
- **Forbidden:** reordering `MatterPoint`; subgroup operations; changing Stiffness,
  Points per Cell, resolution or any threshold for speed.
- **Phase notes (in progress, 2026-09-30, Opus 5.5 worker):** started on the lead's call
  after P1's kill check fired (the entry state's "kill check passed" does not hold).
  - L1: `node.matter_to_grid` is hand WGSL (exclusion 1) with one entry point: per-point
    global atomics, or, with `order`/`ranges`, one workgroup per 4³ block of stencil base
    nodes into a 6³-node workgroup tile. One call into the contribution code serves both
    modes; two entry points differed in 5 of 1370 words under fast math.
    `node.matter_to_particles` feeds the surface's `node.sort_particles_into_cells`, boxed
    by `node.matter_domain`'s block bins. `matter_block_p2g_bit_identical` passes with
    points in and out of their sorted blocks. Owed: the sort needs a gate on the tick
    start (it runs every substep today) and must run with `sorted` unwired (asked of the
    surface worker through the lead); the Live Matter group and presets wait for that.
  - L2 built and measured slower, reverted: G2P at 524k points went from 7.6 ms per frame
    (codegen) to 11.5 ms (hand kernel, per point) and 14.8 ms (tiled), bound by point
    traffic.
  - L3 not built: its acceptance needs every P1 gate green, and A2, A4, A6 and the still
    pool fail today; it would also change `MatterPoint` across every atom. L4 holds
    (`matter_substeps_follow_stiffness_live`). L5: `graph-tool fusion` finds no fusable
    neighbours in the matter graph.
  - Probe, M4 Max, 64³, ms per frame at 524,288 points (sort ungated; gated it costs
    about a 34th of the shown sort time):

    | Stiffness | n | P2G path | Clear | Sort | P2G | Grid | G2P | Stats | Production | ns/point-substep |
    |---|---|---|---|---|---|---|---|---|---|---|
    | 1 | 34 | per point | 1.40 | 0 | 47.18 | 1.00 | 7.88 | 0.55 | 57.13 | 3.09 |
    | 1 | 34 | block | 1.64 | 13.22 | 19.57 | 1.21 | 8.40 | 0.55 | 42.72 | 1.57 |
    | 0.5 | 21 | per point | 0.87 | 0 | 29.53 | 0.58 | 4.79 | 0.60 | 35.50 | 3.12 |
    | 0.5 | 21 | block | 1.02 | 7.76 | 11.90 | 0.69 | 4.71 | 0.54 | 26.19 | 1.51 |

    With the sort gated, Stiffness 1 projects to about 30 ms and 0.5 to about 19 ms. The
    rest of P2G's cost is workgroup-atomic contention: 256 threads add into one 216-node
    tile. A candidate beyond D6: sort by stencil base cell and sum each cell's points in
    registers, one tile add per node per cell (8× fewer atomics at 8 points per cell).
  - Owed: `tests/gpu_proofs/matter_solver_perf.rs` behind `matter-perf-proofs` (its
    out-of-tile fraction needs the gated sort).

### P2a — Colliders

- **Entry state:** P1b merged. Anchors: `rg -n 'pub struct FluidRole|pub struct PreparedFluidGeometry' crates/manifold-renderer/src/node_graph/fluid_role.rs`, `rg -n 'pub fn hull_meshes' crates/manifold-physics/src/mesh.rs`. Answer D16's VERIFY before code.
- **Read-back:** D11, D16, D21; section 4.1 steps 2 and 4; FLUID_ENGINE_INTEGRATION_PLAN.md section 3.2 (Geometry and solver capabilities).
- **Deliverables:** `P/sdf.rs`: `pub fn signed_distance_lattice(mesh: &TriangleMesh,
  spacing: f32, padding: f32) -> Result<DistanceLattice, PhysicsError>` (closed-mesh
  validation shared with FLIP roles, sign by generalized winding number); the derived
  lattice on `PreparedFluidGeometry` (⚠ VERIFY-AT-IMPL that a non-serialized derived
  field leaves take and cache identity unchanged: read how prepared geometry is hashed);
  Collider roles and prescribed motion on `node.matter_domain` through `InputHistory`;
  the packed atlas; `matter_move_bodies`, `matter_solid_distance`; collider projection
  in `matter_grid_update`; a thinness warning for colliders under 2 cells. Tests:
  `sdf_box_matches_analytic`, `sdf_concave_bowl_sign`, `sdf_rejects_open_mesh`,
  `matter_collider_penetration_bounded`, `matter_prescribed_pose_interpolates`,
  `matter_solid_lattice_matches_bodies`.
- **Gate:** tests; `cargo nextest run -p manifold-physics sdf_`; GPU filter; clippy on
  physics and renderer; Dam Break Matter gains the moving box as a Collider role;
  check-presets and graph-tool clean.
- **Demo:** P1's capture on the updated preset into `/tmp/manifold_matter_p2a`. L2.
- **Gesture:** sweep the paddle fast through the pool; water parts and nothing leaks.
- **Forbidden:** a GPU mesh-to-SDF atom; a matter geometry cache; box-only fallbacks for
  rejected meshes; CPIC sidedness.

### P2b — Two-way Box3D coupling

- **Entry state:** P2a merged. Anchors: `rg -n 'pub fn advance_with_coupling|enum AdvancementPolicy' crates/manifold-renderer/src/node_graph/physics.rs`, `rg -n 'fn set_coupled_rigid_inputs|fn accept_coupled_rigid_frame' crates/manifold-renderer/src/node_graph/effect_node.rs`. ⚠ VERIFY-AT-IMPL that `RigidSceneObservation` carries everything `advance_with_coupling` needs (read `R/physics/worker.rs` whole); a missing field is an escalation.
- **Read-back:** D12; section 5; FLUID_ENGINE_INTEGRATION_PLAN.md section 8 (Phasing), its phase P8b (Two-way liquid/rigid coupling), including the rejected candidate.
- **Deliverables:** `node.matter_body_reaction`; `MatterCoupling: StepCoupling`; the
  coupling hooks on `node.matter_domain`; the fenced reaction ring; the D4 body term;
  liquid density for coupling. Tests: the coupling rows of section 12,
  `matter_coupled_holds_when_reaction_pending`, and every existing `physics_` and
  `fluid::coupled` test unchanged.
- **Gate:** tests and GPU filter green; clippy; content-thread gate: the orchestrating
  session runs the app with `MANIFOLD_RENDER_TRACE=1` on the coupled demo for 60 s, no
  frame over 20 ms.
- **Demo:** preset `WaterFloatingBoxMatter.json`: a density-0.5 box dropped into the pool,
  into `/tmp/manifold_matter_p2b`; computed check: final box height within 0.5·dx of the
  waterline. L2.
- **Gesture:** drop a light box into the pool, then fire a force impulse at it; it bobs,
  spins and settles.
- **Forbidden:** blocking readback; Box3D per substep; damping or mass changes to pass
  energy tests; FLIP-style after-the-fact exchange.

### P3a — The liquid-domain seam (seam brief)

- **Entry state:** P2b merged. Re-derive the inventory: `rg -n '"node\.fluid_surface"' crates -g '*.rs'`. Snapshot at `c8961489d`: 94 hits in 46 files; 53 in 25 non-test files, about 40 outside inline test modules (section 1.1 row). If the count differs, list the new sites before touching anything.
- **Read-back:** D17, D22; section 10.
- **Old → new:** each production test `type_id == "node.fluid_surface"` meaning "a liquid
  domain" → `manifold_core::liquid_domain::is_liquid_domain(type_id)`, with
  `pub const FLIP_DOMAIN_TYPE_ID: &str = "node.fluid_surface"` and
  `pub const MATTER_DOMAIN_TYPE_ID: &str = "node.matter_domain"` in
  `crates/manifold-core/src/liquid_domain.rs` (the only place the literals live). The
  force-recipient walk (`R/scene_modifier_expand/acceleration.rs:110`, `:267`) becomes
  `liquid_domain_of(object)`: follow the object's `vertices` producer upstream to the
  first node satisfying the predicate. Sites whose meaning is FLIP-only (FLIP param tables
  in `manifold-core/src/scene_exposure.rs:69` and `R/scene_exposure.rs:97`, the cache
  folder in `manifold-core/src/file_loader.rs:81`, `R/scene_exposure/fluid_quality.rs`)
  switch to `FLIP_DOMAIN_TYPE_ID`, and where a table is per-type the matter row is added
  to the same table. `metadata_for_node_type("node.fluid_surface")` calls in
  `manifold-app` read the found node's own type instead. Worked example:
  `R/preset_runtime/physics_sources.rs:65` `.any(|node| node.type_id == "node.fluid_surface")`
  → `.any(|node| is_liquid_domain(&node.type_id))`.
- **Deliverables:** the predicate module, the walk, the rewrites; matter domains appear in
  the Scene Panel's domain lists, role targets, force targets, World sharing and the
  domain gizmo. Tests: `liquid_domain_predicate_covers_both`,
  `scene_physics_force_targets_matter_domain_through_surface_group`,
  `scene_physics_roles_assign_to_matter_domain`, and every existing `scene_physics_`,
  `fluid_` and FLIP UI flow unchanged.
- **Gate:** the negative gate of section 12 (one literal home); every existing FLIP test
  and `scene-fluid-*` UI flow on disk passes (count them); focused core, editing, app and
  renderer clippy.
- **Demo:** UI flow `scripts/ui-flows/scene-matter-roles.json`: a matter preset scene,
  assign an Inflow role to an object, add a Force targeting the domain, undo/redo,
  save/reload. L3.
- **Forbidden:** a matter branch at any site; changing any FLIP behaviour; a second list
  of domains.

### P3b — Sources, drains and fills on scene objects

- **Entry state:** P3a merged.
- **Read-back:** D9, D11; section 4.2; FLUID_ENGINE_INTEGRATION_PLAN.md section 4 (Scene Panel and creative workflow). ⚠ VERIFY-AT-IMPL how `R/fluid/roles.rs` samples role motion history; share it, extracting in place if it is FLIP-coupled.
- **Deliverables:** mesh fills in `matter_fill`; `matter_emit` (inflow velocity, inherit
  motion, per-tick rate); `matter_drain`; `matter_compact`; epoch renumbering. Matter
  versions of `WaterBasin.json` (pour) and the Dam Break with every role kind. Tests:
  `matter_initial_fill_volume_matches_mesh` (within 3%),
  `matter_inflow_rate_matches_authored` (within 5% over 2 s),
  `matter_drain_removes_and_compacts_preserving_id_order`, and a held-out concave role
  mesh the builder did not develop against.
- **Gate:** tests; GPU filter; clippy; check-presets and graph-tool clean.
- **Demo:** UI flow `scripts/ui-flows/scene-matter-pour.json`: assign Inflow and Drain to
  scene objects, play, move the source, toggle the inflow's enabled. L3.
- **Gesture:** move the pouring source while it pours into a basin with a floor drain.
- **Forbidden:** atomic birth counters; reordering storage; matter role kinds.

### P3c — Forces and impulses (MIDI, OSC, beats)

- **Entry state:** P3b merged. Anchors: `rg -n 'pub fn enqueue_impulse' crates/manifold-renderer/src/node_graph/fluid/impulses.rs` (`:60` at `c8961489d`), `rg -n 'acceleration_field' crates/manifold-renderer/src/node_graph/primitives/fluid_surface.rs`.
- **Read-back:** D13; section 6; FLUID_ENGINE_INTEGRATION_PLAN.md section 5 (Timing, events and lifecycle).
- **Deliverables:** field sampling and the impulse hooks on `node.matter_domain` over the
  shared `EventQueue`; force and impulse lattices in `matter_grid_update`. Tests:
  `matter_impulse_once_per_tick_across_substeps`, `matter_force_lattice_matches_field`,
  `matter_input_stream_24_30_60`.
- **Gate:** tests; GPU filter; clippy; `scene-forces-controls` flow passes; a new flow
  `scripts/ui-flows/scene-matter-forces.json` binds a radial impulse to a clip edge,
  rebinds it to a MIDI-mapped Fire, saves, reloads and fires again, asserting one receipt
  per fire. L3.
- **Demo:** the flow, plus a capture of a clip-edge splash into `/tmp/manifold_matter_p3c`.
- **Gesture:** map a pad to Fire on a radial impulse; the pool splashes on every hit.
- **Forbidden:** per-node CPU field evaluation; a matter-only force system or trigger
  router.

### P3d — Live material controls

- **Entry state:** P3c merged.
- **Read-back:** D3, D4; section 6.
- **Deliverables:** Viscosity in the water branch with the D4 viscous limit; per-tick
  substep recompute under modulation of Stiffness and Viscosity; the Material section of
  the domain's param surface (Stiffness, Cohesion, Viscosity, Liveliness) through the
  scene exposure tables. Tests: `matter_substeps_follow_stiffness`,
  `matter_viscous_decay_matches_analytic` (shear-layer decay within 5%),
  `matter_live_dials_modulate_after_reload`.
- **Gate:** tests; GPU filter; clippy; a flow modulating Stiffness from an LFO after
  save/reload.
- **Demo:** a capture sweeping Stiffness from 0.5 to 2 on a beat. L2.
- **Gesture:** ride Stiffness on a fader through a splash — soft and bouncy to tight.
- **Forbidden:** bespoke rows; clamping a dial silently; a dial with no effect in some
  state.

### P4 — Look and speed gate, and the side-by-side (the go/no-go)

- **Entry state:** P3d merged; surface P5 and P6 merged: `rg -n 'node.volume_surface_mesh' crates/manifold-renderer/src/node_graph/primitives`.
- **Read-back:** D19, D20; sections 7 and 8.
- **Deliverables:** the mesh look-metrics mode of `fluid_capture` (A7–A11); the
  side-by-side: FLIP (preset defaults) and matter on the same domain, column, resolution,
  points per cell and Liquid Surface settings, 600 frames each, at Liveliness 0 and 0.9,
  into `/tmp/manifold_matter_vs_flip/{flip,matter_b0,matter_b09}` plus a contact sheet
  and the metrics JSON; the P1b perf proof re-run on the final solver.
- **Gate:** `matter_surface_look_against_flip` (A7–A11) and `cargo test -p manifold-renderer --features matter-perf-proofs --test gpu_proofs matter_solver_perf` (p95 against 6.0 ms, machine, OS and build recorded). A miss on either stops the phase and goes to Peter with the dial table and the Peter-only lever table. Peter's look call, the Liveliness default and the go/no-go are recorded in a `decision` bead.
- **Demo:** the side-by-side. L2, Peter's call.
- **Gesture:** scrub the Dam Break back to the start and trigger it again with both
  solvers in view.
- **Forbidden:** changing dials, particle counts, resolution or thresholds to pass;
  comparing through different surfaces; any FLIP change.

### P4b — Add Fluid authors matter (after Peter's go)

- **Entry state:** a closed `decision` bead with Peter's go from P4. Anchor: `rg -n 'pub struct AddSceneFluidCommand' crates/manifold-editing/src/commands/graph/scene/fluid.rs`.
- **Read-back:** D17, D22, D23; GROUPING_GRAPHS.md.
- **Deliverables:** `AddSceneFluidCommand` inserts the Live Matter and Liquid Surface
  groups instead of `node.fluid_surface`, with Peter's Liveliness default; if the
  surface design's P7 already landed, its FLIP producer is replaced in the same command.
  Test `scene_physics_add_fluid_matter_undo_reload`; flow
  `scripts/ui-flows/scene-fluid-matter.json` (add → play → move source → undo/redo →
  save/reload → play); every `scene-fluid-*` flow on disk passes (count them).
- **Gate:** the test and every counted flow; focused editing, app and renderer clippy.
- **Demo:** the flow. L3. Hand Peter the worktree launch command.
- **Gesture:** Add Fluid into an existing scene and drag the source while it pours.
- **Forbidden:** migrating FLIP scenes; a solver dropdown; a second Add command.

### P5a–P5d — Materials, one phase each, goo first

Shared entry: P4 go recorded. Shared read-back: D10, section 4.4, the cited paper.
Shared deliverables: the Material Enum value, its branch, its params on the domain's
param surface, a preset, and the D4 hardening bound on c. Shared gate: the phase tests,
the GPU filter, check-presets and graph-tool, P1's A1–A3 on the new material, an L2
capture for Peter. Shared forbidden: a second solver, damping to hide instability, new
record types beyond `MatterDeformation`.

| Phase | Deliverables | Test | Gesture |
|---|---|---|---|
| P5a Goo and Melt | `matter_update_deformation`; 3×3 SVD in WGSL (McAdams et al. 2011 form, as Taichi `ti.svd`) with value tests; fixed-corotated branch; Melt (D10) for every model with F; `GooDropMatter.json` | `matter_goo_cube_rebounds` (rest shape within 2% after 3 s at Melt 0); `matter_melt_freezes_current_shape` (Melt 1 → 0: the new rest shape is the melted shape within 2%) | ride Melt from solid to goo on the build and freeze it on the drop |
| P5b Snow | hardening and plasticity branch | `matter_snow_ball_fractures` (clump count > 1 after impact); Jp stays in [0.6, 20] | throw a snowball at the floor on the snare |
| P5c Sand | Drucker–Prager branch | `matter_sand_pile_angle` (settled slope within 5° of the expected angle) | flip gravity on a fader and watch a pile avalanche |
| P5d Lava | Bingham branch with Melt scaling the yield stress | `matter_bingham_flow_stops_below_yield` | lava flows down a slope, crusts when Melt drops, flows again on the beat |

### P6 — Whitewater

- **Entry state:** P4 go; the surface design's P8 whitewater path exists.
- **Read-back:** D24; FLIP whitewater params in `WaterDamBreak.json`.
- **Deliverables:** grid-based potentials (trapped air from velocity difference, wave
  crest from mass-gradient curvature and velocity, kinetic energy), emission by scan into
  a separate pool, ballistic spray, advected foam, buoyant bubbles, with the FLIP
  constants of section 4.5; outputs `foam_particles`, `bubble_particles`,
  `spray_particles` as `FluidParticle` frames with id 0. Test
  `matter_spray_follows_ballistic_arc`.
- **Gate:** the test and GPU filter; check-presets and graph-tool clean.
- **Demo:** Dam Break Matter with whitewater; L2 beside FLIP's whitewater.
- **Gesture:** trigger the dam break; spray arcs at 60 fps.
- **Forbidden:** ids for whitewater; screen-space foam; a whitewater renderer.

### P7 — Particle-frame bake

- **Entry state:** P4 go. ⚠ VERIFY-AT-IMPL the `fluid_cache.rs` writer and the take journal (`R/fluid/take.rs`) before pinning the payload.
- **Read-back:** D24; the surface design's D12.
- **Deliverables:** Record and Playback for matter graphs: per-tick `FluidParticle` frames
  (and whitewater frames) through the existing atomic per-tick file writer as a new
  payload kind with its own manifest version; the cache folder joins
  `manifold-core/src/file_loader.rs` through `MATTER_DOMAIN_TYPE_ID`; playback feeds the
  same seam; the surface design's D12 lifts for matter graphs; Record waits on fences.
  Tests: `matter_cache_round_trip`, `matter_cache_rejects_changed_setup`,
  `matter_cache_playback_never_steps_solver`.
- **Gate:** the tests; the round-trip gate of DESIGN_DOC_STANDARD.md section 5 (Phase briefs): record, reload, play, modulate after reload.
- **Demo:** record a take with a kick-driven impulse and export it at 30 fps. L2.
- **Gesture:** record a take live, then export it with a different material colour.
- **Forbidden:** caching `MatterPoint` state (frames only); a second cache format;
  silently loading mismatched caches.

### P8 — Demo scenes, one per capability

- **Entry state:** P5d, P6 and P7 merged.
- **Read-back:** sections 6 and 7; every phase's gesture line.
- **Deliverables:** five bundled generator presets, each with its exposed card built
  through the scene exposure tables and its gesture bound to named params, and one project
  `Matter Demos.manifold` (built with `project_tool`, one layer per preset, 120 BPM,
  clips placed on bars; ⚠ VERIFY-AT-IMPL that `project_tool` can author layers and clips —
  if not, stop and escalate; never hand-edit the ZIP):

| Preset | Scene | Gesture Peter performs |
|---|---|---|
| "Matter — Water and Boxes on Beats" | a pool with three coupled Box3D boxes; a radial impulse on the kick, a lift field on the snare | play a drum pattern; boxes jump and splash on every kick; gravity on a fader for the breakdown |
| "Matter — Goo and Blocks" | a Goo blob in a basin, Box3D blocks dropping in | ride Melt from solid to goo on the build; freeze on the drop with the blocks stuck in it |
| "Matter — Sand Pour" | a sand source pouring onto Box3D ramps | move the source; flip gravity on a fader to avalanche the pile |
| "Matter — Snow" | snowballs launched by impulses at a wall and floor | fire a snowball on each snare; they burst into clumps |
| "Matter — Lava and Melt" | a Lava block on a slope | Melt follows a beat ramp: it crusts between bars and flows on the downbeat |

  Tests: check-presets, graph-tool validate and fusion for all five; `matter_demo_presets_run_ten_seconds` (no fault, no non-finite frame, live count within 5% of expected); the project loads through the project loader.
- **Gate:** the tests; each preset's 600-frame capture passes A1–A3 for its material; the
  orchestrating session runs each demo in the app with `MANIFOLD_RENDER_TRACE=1` for 60 s,
  no frame over 20 ms.
- **Demo:** L4 — Peter performs each gesture live; the click-script goes in the merge
  commit.
- **Forbidden:** demo-only nodes or params; hidden state that makes a demo work only from
  its saved position.

Phasing completeness: every behaviour this document commits to lands in one phase above
or in section 15.

## 14. Decided — do not reopen

1. Live liquid is GPU MLS-MPM writing the surface seam; FLIP is bake, reference and test feed only; real mesh only (Peter).
2. MLS-MPM over GPU FLIP/APIC because it avoids the global pressure solve; not claimed cheaper for plain water.
3. J-tracked weakly compressible water with three dials: Stiffness, Cohesion (default 0), Liveliness (default 0 until Peter picks at P4).
4. Substeps come from the stiffness/CFL rule every tick, from parameters, never from readback.
5. Flat arrays with the lattice on wires; i32 fixed point, mass at 2^16 and momentum at 2^27 with hashed unbiased rounding; deterministic.
6. One P2G atom; cell-sorted block-local accumulation from P1b; storage order never changes.
7. Executor repeat region re-implemented from the historical seam.
8. Fixed 60 Hz ticks owned by the domain node; live caps ticks per frame and reports dropped time; export runs every tick; display one tick behind.
9. Append-only births, order-preserving compaction, id-sorted frames; Points per Cell 8 by default, 27 optional.
10. One material per domain in v1; constitutive branches; Melt is the beat-able phase change.
11. Distance lattices built in `manifold-physics`, derived on the existing prepared geometry.
12. Coupling: GPU body integration per substep, Box3D lockstep per tick, fenced readback, hold on miss.
13. Forces and impulses through the existing routes; only non-finite state faults.
14. `node.matter_domain` speaks `node.fluid_surface`'s scene contract; one liquid-domain predicate.
15. Look and speed are gates with named metrics; no executor tunes a dial or threshold.
16. Half precision only for read-only lattices, plus the gated C-storage experiment.
17. Add Fluid switches only after Peter's P4 go.

## 15. Deferred, with triggers

| Item | Revive when |
|---|---|
| Subgroup (warp) P2G reductions | P1b misses and subgroup support is verified through naga → SPIR-V → MSL and on the Vulkan backend |
| Fused G2P2G kernel | P4 misses after P1b and Peter grants a no-monolith exemption |
| Velocity-aware substeps | Peter accepts non-deterministic live runs for the win (section 8) |
| CPIC colored-distance-field compatibility (thin shells, cutting) | A collider thinner than 2 cells leaks in a show scene, or Peter wants cutting |
| Real surface tension | Peter judges Cohesion wrong for a named look |
| Mixed materials in one domain | A scene needs two materials that touch in one domain |
| More than one coupled tick per frame (30 fps projects) | A coupled scene is needed at a project rate below 60 fps |
| Particle-level collider push-out | `matter_collider_penetration_bounded` fails at grid resolution |
| Sparse or adaptive grids, 128³ live | P4's stretch report and a named scene need it |
| Sparse volume tiles (Wu et al. 2018; NVIDIA GVDB) | Domains beyond 128³ are wanted |
| Multiple matter domains exchanging material | A scene needs two interacting domains |
| Quality tiers for matter | P4b lands and Peter asks for tiers |
| GPU mesh-to-SDF | Deformable (skinned) colliders are needed |
| Vulkan runtime proof | The Vulkan backend runs the GPU proof suite |

## 16. Risk register

| # | Risk | Detection | Response |
|---|---|---|---|
| R1 | **Speed: the gate sits at the bandwidth roofline** (section 8); the brief's 1M-at-60-fps reference was not found | P1 kill check; P1b profile; P4 gate | Look-neutral levers in P1b; the rest to Peter |
| R2 | Springiness reads bouncy against FLIP | A2, A10 | Stiffness, priced in substeps; never damping |
| R3 | Dissipation makes water calmer than FLIP | A5–A8 | Liveliness; Peter picks the default at P4 |
| R4 | J drift loses or gains volume over minutes | A3 | Escalate; a periodic J reset from grid density is a design change |
| R5 | Thin sheets tear at 8 points per cell | A6, A7 | Points per Cell 27 costs R1; Peter's call |
| R6 | Grid-aligned ridges | A1, A9 | Escalate with the capture; jittered seeding is already in |
| R7 | Atomic contention on Metal | P1b profile | L1 |
| R8 | Light bodies need too many substeps | the D4 body term; P2b tests | The Stiffness and density ranges keep n ≤ 128; revisit with the CPIC trigger |
| R9 | Dispatch overhead (140–210 per frame) | P1b profile | L5; fewer, larger atoms need Peter |
| R10 | Readback misses make coupled scenes lag | lag readout; P2b trace | Reported; Reset recovers; multi-tick deferred |
| R11 | The re-implemented executor seam breaks feedback or freeze behaviour | P0b runs every execution, feedback and freeze test | Fix before landing |
| R12 | The P3a rewrite changes a FLIP behaviour | every FLIP test and flow in P3a's gate | Fix before landing |
| R13 | CPU distance-lattice build too slow for photoscan roles | P2a timing on a held-out 100k-triangle mesh | Proxy hulls, or the GPU SDF trigger |
| R14 | Unverified constants and papers (Liveliness blend, snow, sand, lava, Martin & Moyce) | VERIFY-AT-IMPL markers | Transcribe at phase entry; a mismatch is an escalation |
