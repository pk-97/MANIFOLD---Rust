# GPU FLIP — the GPU water solver and its multigrid pressure solve

<!-- index: The GPU water solver (GPU FLIP, formerly SWASH): PIC/FLIP particles on a face grid, one liquid tick of two water steps, and a multigrid-preconditioned conjugate gradient pressure solve that stops on the FLIP Fluids tolerance. The step, the equation, the solve, the Auto iteration rule, the named refusals, the measures against the FLIP Fluids engine, and what is still owed (solids). -->

**Status:** BUILT · 2026-10-01 · the step is one node, `node.gpu_flip_step` (section 1.1 (stage design)) · owed: BUG-4jfv (solids gate: race rows, engine draft, L2 demo); BUG-l2h3 (SWASH to a live instrument) child .10 (occupied-block passes); BUG-h8or (lid slabs) · the retired FFT solve is `docs/archive/FFT_WATER_SOLVER_DESIGN.md`.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) before the solids phase.

GPU FLIP is the liquid water solver: particles carry the water, a face (MAC) grid carries its velocity, and each step makes that velocity divergence-free with one pressure solve. The solve is the textbook multigrid-preconditioned conjugate gradient (McAdams, Sifakis and Teran, "A parallel multigrid Poisson solver for fluids simulation on large grids", 2010). It replaced the FFT capacitance solve on 2026-10-01: the same equation, 3.0× faster at 64³ and 3.7× at 128³, to a smaller residual.

On stage: water that doesn't squash or fizz. A dam collapses, sloshes and settles into a pool that is actually still, on the GPU, at the frame budget.

Companions: `LIQUID_SOLVER_SEAM_DESIGN.md` (the scene contract the domain speaks; P7a (GPU FLIP on the contract)), `GPU_WHITEWATER_DESIGN.md` (spray, foam and bubbles on GPU FLIP's face grid), `GPU_FLUID_SURFACE_DESIGN.md` (the surface the particles are meshed into).

## 1. The step

The builder is `crates/manifold-renderer/src/node_graph/primitives/gpu_flip_preset.rs` (`water_step`); the shipped preset is its Dam Break at 64 (`WaterDamBreakGpuFlip.json`, "Water — Dam Break (GPU FLIP)"). `node.gpu_flip_domain` owns the clock: `node.liquid_state`'s tick region runs one 60 Hz tick: one `node.gpu_flip_step`, which runs its Steps substeps (`STEPS_PER_TICK` = 1 by default) inside itself, then `node.liquid_stats`. Each substep runs these passes (`shaders/gpu_flip_step.wgsl`, one entry point each; the solve is `gpu_flip_pressure.rs`):

1. Sort particles into cells (the shared particle sorter of `node.sort_particles_into_cells`, bins = grid cells).
2. The particles' signed distance φ at the cell centres (`particle_distance`, section 2, the free surface). Water is every cell with φ < 0 (`water_from_phi`, after the solids' `phi_into_solids` in step 4), the engine's liquid cells: a centre within √3·h/2 of a particle. Walls are the box faces.
3. Particles to faces by gather, no atomics (`particles_to_faces`): each face reads the particles in its neighbouring cells. A box wall face is closed: velocity 0 (section 2, walls). Extend `band_layers` layers into air (`extend_faces`) and keep the copy for FLIP (`old`).
4. Gravity plus the scene's forces, the wall faces held at 0 (`face_gravity`). The forces and impulses come from the domain's coarse lattices (LIQUID_SOLVER_SEAM_DESIGN.md P8 (Forces and impulses for GPU liquids)), read at each face's centre; an impulse lands once, on the first step of its tick. Solids: the open fraction of every face (`open_fractions`) and the closest body's velocity on it (`solid_face_velocity`); see "Solids in the water" below.
5. Divergence per water cell → f (`divergence`).
6. The pressure solve, section 3.
7. Subtract the pressure gradient on faces touching water (`subtract_pressure`), the air side of a surface face at its ghost pressure; wall faces stay 0. Constrain the solid faces and the walls (`constrain_solid_faces`). Extend `band_layers` layers (`new`), the engine's count; every RK3 stage of step 9 samples valid faces.
8. Every step, the density projection of Kugelstadt et al. 2019, "Implicit Density Projection for Volume Conserving Liquids" (`density_source`; credit in `gpu_flip_step.rs`). Each water cell's density ρ is the tent-kernel sum of the particles within a cell of its centre, rest 8; solids are sampled with particles as the paper does, on the same rest lattice of eight sites a cell: each of the 26 neighbours outside the box adds what its eight sites would (0.5625 a face, 0.09375 an edge, 0.015625 a corner; blub keeps only the face term), and near a body every site inside it (`solid_at` < 0) adds its tent weight, so a cell the body only cuts weighs its solid part; a cell with any neighbour holding no particles reads at least rest, so a part-full surface cell only spreads (the paper's particle-deficiency clamp; air is an empty cell, not the level-set mask, which also covers the empty cell above the surface). A pool at rest on the seeded lattice reads exactly rest everywhere, so it is never pushed. The source −(1/dt)·clamp(ρ/8 − 1, ±½) (the paper's displacement limit, ρ/ρ0 in [0.5, 1.5]; blub builds both the same way) is solved like section 3 with air at zero, its gradient taken off a copy of the projected faces into `spread`. It is the whole error every step, no per-step share, and it moves particles only: it never becomes velocity, so it adds no speed. Off with Volume Projection 0.
9. Faces to particles (`faces_to_particles`): PIC/FLIP velocity from `new` and `old`; the RK3 move through `new` plus the density move step dt · (`spread` − `new`), uncapped (the source clamp bounds it); clamped 0.2 cells off the walls.

The step's time rules:

- **Step count.** The step node's Steps param, 1 by default (1/60 s); Peter's ruling, no auto-picked count (adaptive steps: BUG-jyot (adaptive GPU FLIP steps)). The substeps run inside the one node; each body sees every substep through the in-place reaction. At one step a 64³ splash crosses about 4 cells a step, so the CFL guard below fires; the stats count it and Liquid State says so.
- **The CFL guard.** `TOP_SPEED` = 20 m/s is the fastest water a step is built for. `travel_cells` is how far that moves in one step, rounded up (3 at 64³, 6 at 128³, two steps). Each RK3 stage moves at most that far; faster water keeps its speed and moves only that far that step. Every shortened stage is counted (liquid_stats word 8) and Liquid State reports it by name; a solid push-out refused past `SOLID_PUSH` = 5 cells is counted too (word 9). Both face grids, `old` after the transfer and `new` after the solve, are extended ⌈√3 · travel⌉ + 3 layers (`band_layers`: 9 at 64³ two steps, 14 at 64³ one step and at 128³ two steps). That is FLIP Fluids' `_extrapolateFluidVelocities`, ⌈√3 · CFL⌉ + 3, which gives 12 at its CFL 5. The one deviation: our travel stands in for the engine's CFL number, because the engine sizes its substeps to that CFL and ours are fixed by Steps (adaptive steps: BUG-jyot (adaptive GPU FLIP steps)).
- **FLIP share per step.** `flip` is the FLIP share every step keeps, 0.95, as the engine's `_ratioPICFLIP` = 0.05 blends PIC into every substep. More Steps means more PIC damping per second, as in the engine.

## 1.1 Stage design — decided 2026-10-01

Peter, 2026-10-01: GPU FLIP is a specialised solver (DECOMPOSING_GENERATORS.md section 1.2 (Specialised solvers are stage nodes)). Users play it through params (forces, emitters, solids, look) and never rewire its internals, so the graph shows only what a user plugs something into, and kernels fuse freely inside. The atom graph costs the show: in the frozen 64³ smoke (2026-10-01) the coarsest level, 64 one-dispatch sweeps of a 4³ lattice per V-cycle, takes 7.5 ms of a 24.2 ms GPU frame, and CPU encode runs at 14 ms p50.

**The cut.** These stay as they are: `gpu_flip_domain` (params, clock, forces), `liquid_fill` (the emitter), `liquid_state` (the clock's region, pause and export determinism, clearing), `liquid_solid_distance` (solids), `liquid_stats`, `liquid_frame` (the contract every consumer reads), `face_sample_component`, and the Liquid Surface group. Everything else in the water step becomes one node, `node.gpu_flip_step`, in the tick region: sort → classify → particles to faces → extend → forces → divergence → pressure solve → subtract → constrain → extend → density solve → faces to particles and advect. Whitewater gets its own node, `node.whitewater_step`, owned by the whitewater work (GPU_WHITEWATER_DESIGN.md). `liquid_state` and `liquid_stats` stay outside the step. The Liquid Surface group stays a group for now (BUG-twnl (Liquid Surface group as one node)). There is no standalone pressure node: the pressure solver is a `pub(crate)` module inside the step, below. The preset goes from 490 nodes to about 90; the rest is the look and the surface group.

**`node.gpu_flip_step`.**

- Inputs: particles and count (the state's), the lattice and the forces from the domain, the solid distance lattice and bodies.
- Outputs: particles, and the projected faces for the state's `faces_in`.
- Params: flip share, iterations (Auto), spread rate, top speed.
- The lattice outputs are provided storage, resized when Resolution changes, as `liquid_state` holds its faces. That closes BUG-o65k (GPU FLIP lattice wiring) with no new machinery.
- One extent rule for the node. Its inner sizes come from the same `pub(crate)` sizing functions it dispatches with, as `sort_particles_into_cells` does.
- Every internal pass carries its own dispatch label, so the profiler's stage split still reads per pass.
- The liquid conformance suite's atomic-free check covers its hand shaders, as `coarse_inverse_uses_no_atomics` does.

**The pressure solver** is a `pub(crate)` Rust module, used by the step's pressure and density solves and by the solids' face weights. The V-cycle depth is chosen at run time: halve each side, rounding up, until every side is 4 or less, then solve that level exactly with the coarse inverse. Any Resolution runs with no rebuild, and the multiple-of-16 refusal goes. The odd-side restriction weights are proven in `scripts/mgpcg_reference.py` before any GPU code. The module is proven at its boundary against that reference, odd sides included. The fixed iteration counts and the Auto rule (section 4) carry over.

**Tested at the boundary.** The existing solve, still-pool, free-fall and race proofs gate the node, plus the extent rule. Kernel bodies move from the atoms' `wgsl_body` fragments into the step's kernels; the CPU mirrors and fixtures stay. Then the folded atoms, their per-atom proofs and their conformance rows are deleted and the preset regenerated. `node.conjugate_gradient` goes with them, with any nested-region runtime support nothing else reaches; `matter_state`'s substep region and `liquid_state`'s tick region stay.

**As built (2026-10-01).** The preset is 93 nodes, from 471. At 64³ a tick takes 22.7 ms GPU (p95 26.2) against 24.2, and CPU encode p50 3.4 ms against 14.3. Passes per tick: 336, 426, 426, 516 and 516 at Resolution 24, 40, 64, 100 and 128. Breakup means 0.40% (64³) and 0.62% (128³) against the 0.42% and 0.65% baselines. Resolution 64 → 32 → 100 resizes at runtime with whitewater on (345,792, 42,528 and 1,311,200 particles; GPU p50 22.4, 9.5 and 64.4 ms). Substep regions no longer nest: a boundary in another region's body is a compile error. The four body coupling atoms were folded into the step's body passes (section 8, bodies in the solve).

**`node.whitewater_step`** settles three things the atom chain could not:

1. **The pool's first frame.** The node owns its pool, an id counter and a full-pool count. On creation and on every epoch change one pass writes every slot empty (kind 3) and zeroes both counters, so the header slot goes.
2. **The spawn count.** The scan writes its total to an internal counter that append reads, at any buffer size, not the scan's last entry.
3. **The lattice.** The whitewater lattice is a required input, refused by name when missing; `keep_whitewater` and `preserve_foam` no longer skip the per-cell cap without one.

**Shared, so they stay catalog atoms:** `running_total`, `sort_particles_into_cells`, `smooth_lattice`, `math`, `value`, `transform_components`, `particles_to_copies`, `tone_map` and every render node. None of the folded atoms is used outside GPU FLIP: SWASH is GPU FLIP's former name, and MPM's `matter_*` nodes share only the surface group and the seam. Replacing the Liquid Surface group with one node across the four water presets is its own job: BUG-twnl (Liquid Surface group as one node).

## 1.2 Inflow and outflow

A role of kind Inflow or Outflow reaches the step as a region row (`regions`, `region_count` from `node.gpu_flip_domain`), sharing the bodies' shapes and atlas; it is not a solid. Ported from FLIP Fluids `fluidsimulation.cpp`:

- **Pool.** The domain's Particle Capacity is the slot count `liquid_fill` allocates, 0 meaning the fill's count. No upper bound past the exact-count refusal (section 5). A full pool emits nothing and shows as the live count reaching the slots.
- **Emit.** After the first sort, `emit_flags` marks each half-cell site (eight a cell, the fill's lattice) inside an inflow, outside the solid, with no live particle in its half cell (`_addNewFluidCells` 8566-8603, `_addNewFluidCellsThread` 8771-8811). The scan ranks them and `emit_write` writes each at the live count plus its rank, the live count being the sorted prefix's end; a second sort places them. Deterministic, no atomics. New particles sit at the site, no jitter, id slot + 1.
- **Constrained velocity.** A particle inside an inflow takes the inflow's velocity plus Inherit Motion's share of its rigid motion (`_constrainMarkerParticleVelocities` 7397-7451), and faces inside an inflow take no body force (`_getInflowConstrainedVelocityComponents` 6112).
- **Drain.** A particle whose moved position an outflow holds below zero dies (radius 0, 9011-9031); the next sort drops it. Inverted outflows are not supported.

## 2. The equation

On the water cells W of an n_x × n_y × n_z lattice with cell size h:

- L p = f on W, p = 0 on the free surface, walls closed (no flux through a box face).
- (L q)_i = (Σ over in-box neighbours j of w_ij · (j ∈ W ? q_j : 0) − d_i · q_i) / h², where w_ij in [0, 1] is the open fraction of the face between i and j (section 8, solids), d_i = Σ over in-box neighbours j of w_ij − Σ over air neighbours j of w_ij · θ_ij, and θ_ij = clamp(φ_j / (min(φ_i, −0.005h) + 1e-9), −25, 25). θ ≤ 0, so d_i ≥ 0; a cell with d_i = 0 is sealed by solids and out of the system.

**The free surface (ghost fluid).** Pressure is zero where φ crosses zero between a water cell and its air neighbour, not at the air cell's centre. The air side then reads the ghost pressure θ·p_i, which folds into the diagonal: an air neighbour with φ_j ≥ 0 and φ_i < 0 has θ ≤ 0 and adds |θ| to d_i, so the operator stays symmetric and the closer the surface sits to the water cell, the harder it pins the pressure there. The step's `subtract_pressure` pass uses the same θ, so the projection leaves exactly the solve's residual. With φ = 0 everywhere θ = 0 and the rows are the plain Dirichlet ones, so one set of kernels serves both. This is the engine's rule (`pressuresolver.cpp`, matrix rows and velocity update), ported line by line; the shaders carry its MIT credit and `THIRD_PARTY_NOTICES.md` lists them.

φ is the engine's particle level set (`particlelevelset.cpp`): fill 3h, take |c − p| − r over the live particles near each centre, snap |φ| < 0.005h to ±0.005h. r is the engine's √3·h/2 (half a cell's diagonal, its default scale), and a particle reaches the cells of the box 2r around it. 2r < 2h, so the `particle_distance` pass gathers over the 125 bins around each cell with the same box test and gets the engine's field (`gpu_flip_particle_distance_is_the_engines_level_set` checks both against each other). It reads the 27 bins first and the outer ring only where the ring can still lower φ (above 1.5h − r). At that radius every cell holding a particle reads φ ≤ −0.005h.

Deviations from the engine, each named:

| Engine | GPU FLIP | Why |
|---|---|---|
| pressure cells skip the border cells | every cell | the engine's border cells are solid; our walls are the outermost faces, so every cell is inside |
| φ_i < 0 for every liquid cell | a water cell's φ taken at most −0.005h, an air cell's at least 0 | with water = φ < 0 both hold already; the clamps are for φ that is not a level set (zero φ, the plain rows) |
| φ from every particle whose box reaches the cell | a cell with no live particle in its 27 neighbouring bins stays 3h | the solve reads φ only at water cells and their neighbours, which always have one; skipping the outer ring elsewhere took the gather from 1.67 to 1.40 ms a profiled 64³ frame |
| ε 1e-6 in the velocity update, 1e-9 in the matrix | 1e-9 in both | against a water φ of −0.005h the 1e-6 shifts θ by 0.3%, and the projection leaves that much of the surface pressure as divergence |
| ghost rows on every level | the finest level and its conjugate gradient only; coarse levels plain Dirichlet | coarse water is all-eight-children water, so a coarse surface has no φ; the V-cycle stays symmetric, only a weaker preconditioner at the surface |
| no density solve | Kugelstadt's density projection, plain Dirichlet | it restores volume by moving particles, never velocity; the engine has no volume control |
| surface tension, density ratio | none here | off in the scenes we race |
| skips the last inner face of each axis in the velocity update | every inner face | the skip is an engine boundary quirk; our wall faces are their own rule |

On a seeded still pool (8 particles a cell at the half-cell sites) the top occupied cell reads φ = −0.433h and the empty cell above −0.037h, so that cell is water too, as in the engine, and the surface sits 0.54h above the seeded top. The ghost rows move the surface only where the water is thin: sheets, drops and a surface cell with few particles.

Why not r = h/2, which puts the still pool's surface 0.33h below the seeded top and needs no air floor: a cell holding one particle then reads φ > 0 whenever the particle sits outside the cell's inscribed ball, about half the time. The water floor turns that into θ = −25 on exactly the thin cells of a splash. On the 64³ Dam Break that threw water 40% faster than the plain rows (21.9 against 15.6 m/s), put 2.4× as much on the lid and broke the sheets into twice as many pieces.

**Walls.** The box walls sit exactly on the outermost faces and are the engine's closed wall: a static solid in the solid distance, open fraction 0 in the operator and the divergence, velocity 0 (`fluidsimulation.cpp` `_updateWeightGridThread`, `_constrainVelocityFields`). Every pass that writes a face writes a wall face as 0: `old` (`particles_to_faces`), the forced field (`face_gravity`), the projection and the constraint, so the FLIP change on a wall face is always 0. The divergence and the operator agree that no flux crosses a wall. An earlier separating wall kept the outward part of a wall face's velocity as a prescribed flux while the operator treated the face as closed; the mismatch pushed momentum into the water at every wall touch.

A water body that touches no air (a closed box full of water, or water a solid seals off) makes L singular: only a right-hand side summing to zero over the pocket is solvable, and even then f32 never carries the residual to the stop, so the solve runs to its cap. The step removes the singularity before solving, in both solves. It subtracts each sealed pocket's mean from the right-hand side, so the source is compatible (sealed water cannot change volume, so the net flux is not physical), and it pins the pocket's leader cell (its lowest index) to p = 0 by leaving it out of the solve's water mask, so L is nonsingular. Pressure in a sealed pocket is defined only up to a constant; the pin picks it. No step divides by a zero: a cell whose diagonal is 0 (every face closed by a solid) is out of the system and reads 0.

## 3. The solve

Conjugate gradient in the L form, from x = 0, r = f, p = 0, rz = 0. Each iteration: z = V(r); rz_new = r·z; β = rz_new / rz_old (0 when rz_old is 0); p = z + βp; s = −Lp; α = rz_new / (p·s); x −= αp; r −= αs. The loop is the solver module's (`gpu_flip_pressure.rs`, `PressureSolver::solve`): no readback, every scalar on the GPU. It stops the way FLIP Fluids' PCG does (`pcgsolver.h`): after each iteration, when |r|∞ ≤ min(1e-9 · |f|∞, 1.0 s⁻¹), f being the divergence in 1/s; and before the first, with p = 0, when |f|∞ < 1e-9. The stop is on the GPU: every dispatch of the solve is indirect, its group counts in a gate buffer that the check pass zeroes, so the passes after a stop run no groups. `MAX_ITERATIONS` = 64 is the cap; a solve that reaches it unconverged is counted in the tick's solver words (`node.liquid_stats` words 10 to 12) and Liquid State raises it as a named error. The f32 recursive residual reaches the engine's 1e-9 unchanged: 11 iterations on a still pool, 12 to 14 on the Dam Break splash frames (`pressure_module_converges_on_the_engine_tolerance`). Dots reduce in two barriered passes in a fixed order; β and α are made on the GPU.

V(r) is one V-cycle for L e = r, from e = 0:

- The depth follows the lattice: each level halves every side, rounding up, until every side is 4 or less (`level_lattices`). An odd side's extra coarse half-cell is solid. A coarse cell is water only if all its children are water; a coarse face's open fraction is the mean of the fine faces it covers. `PressureSolver::prepare` builds the coarse levels once per water, and both solves of a step reuse them.
- Pre-smooth: 2 rounds of red-black Gauss-Seidel (red then black), from zero. The finest level's sweeps and residual, and the conjugate gradient's −Lp, read φ (the ghost rows of section 2); every coarser level runs zero φ.
- Residual r − Le, full-weighting restriction to the next level (the transpose of prolongation, masked to coarse water).
- Recurse. The coarsest level, at most 4³ = 64 cells, is solved exactly by its inverse (`shaders/coarse_inverse.wgsl`, one workgroup), built at prepare.
- Prolong-add the correction, trilinear (3/4 and 1/4 per axis, clamped at the box), masked to water.
- Post-smooth: 2 rounds, black then red, so the cycle is symmetric and the preconditioner is too.

`scripts/mgpcg_reference.py` is the oracle, odd sides included; the module is proven against it at its boundary (`pressure_module_matches_reference_64`, `_128`, `_odd_sides`, `_deep_pool`, `pressure_module_solves_the_ghost_rows`).

## 4. Iteration counts — the Auto rule

Auto (Iterations 0, the default) is the convergence stop of section 3 under the `MAX_ITERATIONS` cap; an explicit Iterations value runs exactly that many. A solve that reaches the cap is reported in the stats (`unconverged`) and the water keeps moving with the pressure it has, as the engine does: a body pressed into water a solid seals off from air has nowhere to send it, and the water looks squeezed rather than halting the show. It is never a node error (Peter's ruling, 2026-10-02). The engine's particle removal is a strict inside-solid sign test after the push-out (`_resolveCollision`, then `_removeMarkerParticles`), so a slow lid crowds the water under it rather than removing it, in the engine and in the port alike. Tests assert `unconverged` = 0 wherever physics guarantees convergence: every solve with air, and a sealed pocket until the squeeze kills particles inside a solid. The fixed counts below are what the explicit values and the reference proofs use. They were picked as the smallest count at which the f64 reference (`scripts/mgpcg_reference.py`) reaches the retired FFT solve's residual on every committed Dam Break problem (`tests/fixtures/dambreak_pressure_problems.bin.zst`, 7 frames) and on dumped splash solves at 64³ and 128³, plus one. Deep water has fixtures too, written by the atom graph's fixture writer, deleted with the atoms (BUG-2o3c (deep-pool fixtures cannot be regenerated)): a 3 m still pool's main solve (`deep_pool_pressure_problems.bin.zst`) and the density solves of a block dropped into it (`deep_pool_density_problems.bin.zst`). With no FFT record there, their target is the tightest FFT residual at that lattice: 5.5e-5 (64³) and 1.7e-3 (128³) for the main solve, 4.2e-2 and 0.35 for the density solve. A multigrid preconditioner's count does not grow with the lattice, so one count serves every size.

- `PRESSURE_ITERATIONS` = 8: the reference needed at most 7 at 64³ and 5 at 128³ on the Dam Break, 6 and 5 on the deep pool.
- The density projection runs the step's pressure stop (Iterations; Auto is the convergence stop). The retired `DENSITY_ITERATIONS` = 3 rule (the reference needed 2 at 64³ and 1 at 128³) was for the old density solve, not this one.
- An Iterations value past the solver's `MAX_ITERATIONS` = 64 is refused by name, never clamped.

Re-run 2026-10-01 for the fixed five levels with a smoothed coarsest level (`--depth 5 --coarse-sweeps 16`) on the Dam Break problems and the deep pool; the splash dumps were not kept, and no count moved.

The count changes only with this rule re-run, on the same fixtures, with the new count in this section.

## 5. Limits and refusals

Every limit is a named refusal at build; nothing is clamped silently.

- Any lattice side from 1 to 1024 runs, odd sides included; outside that the solver refuses by name (`lattice_refusal`, "GPU FLIP: every lattice side must be 1 to 1024 … Lower Resolution.").
- More particle slots than a count carries exactly (2²⁴), the fill or Particle Capacity whichever is larger: refused by name; Resolution 256 is refused by it.
- Resolution and Domain Size apply at runtime: the step's lattice arrays are its own storage, reallocated when the lattice changes (`gpu_flip_resolution_card_resizes_at_runtime`).
- Every array covers every dispatch before the GPU sees it: the step's one extent rule in `node_graph/liquid/extent.rs` sizes its inner arrays with the functions it dispatches with, walked at even and odd lattices (`gpu_flip_*_cover_every_dispatch`, `gpu_flip_any_resolution_walks_on_the_built_graph`).

## 6. Measures

Machine: M4 Max. Load averages were 6–37 during these runs (other sessions on the GPU); ms rows are contended and read as ratios, not budgets.

Closed walls (engine wall model), Dam Break at 64³, `gpu_flip_wall_feel_64`: a hydrostatic column rests 300 frames with wall faces exactly 0 and inner vertical faces at 0.001% of g·dt. Dam Break top speed 13.6 m/s at 1 step a frame (15.4 at 2), run-up reaches the lid (3.99 m by frame 74), front 3.4 m/s over frames 6–24 (wet bed, so Ritter's 8.7 m/s is only a ceiling), volume drift max 9.0%, last frame +4.7%.

Solve alone, GPU ms per main solve at the shipped 8 iterations against the FFT solve at its shipped 24 passes (FFT from its record, quiet GPU):

| Lattice | GPU FLIP | FFT | GPU FLIP median residual |
|---|---|---|---|
| 64³ | 3.7 | 9.4 | 6.8e-6 |
| 128³ | 14.8 | 51.5 | 1.1e-5 |

Stage split of the atom graph's solve, five levels with the coarsest smoothed (its probes were deleted with the atoms):

| Stage | 64³ ms (share) | 128³ ms (share) |
|---|---|---|
| smoothing | 0.85 (23%) | 6.13 (41%) |
| residual | 0.10 (3%) | 0.74 (5%) |
| restriction and prolongation | 0.25 (7%) | 1.30 (9%) |
| coarsest level (16 sweep pairs each way) | 1.79 (49%) | 2.66 (18%) |
| CG vectors (dots, divides, axpys) | 0.66 (18%) | 3.91 (26%) |
| setup (coarse water, zeros) | 0.02 (0%) | 0.08 (1%) |

The coarsest level is 512 dispatches per solve on a 4³ or 8³ lattice, so its cost is dispatch overhead, about 3.5 µs each. The exact coarse solve's stage was 0.19 ms at 64³; per 64³ tick (two main solves and a 3-iteration density solve) the smoothed coarsest level cost about 3.8 ms more. The solver module now solves the coarsest level exactly at a depth that follows the lattice, so this split is the atom graph's, kept as the record. The iteration trend below predates the fixed depth.

Iteration trend of the atom graph's solve, median residual and GPU ms per solve:

| Iterations | 64³ | 128³ |
|---|---|---|
| 4 | 3.0e-3, 1.7 ms | 4.6e-3, 7.2 ms |
| 6 | 1.3e-4, 2.4 ms | 1.2e-4, 10.8 ms |
| 8 (shipped) | 6.8e-6, 3.1 ms | 1.1e-5, 13.8 ms |
| 12 | 3.1e-6, 4.5 ms | 1.1e-5, 20.6 ms |

The residual stops falling near 1e-5: that is f32.

On solves dumped from a running FFT Dam Break (splash frames 50, 70 and 90, both steps; the atom graph's probe, deleted with it), the GPU residual at the shipped counts is below the FFT solve's on every solve: main 9.0e-6 to 5.2e-5 against 1.4e-3 to 3.8e-3 at 64³, and 7.0e-5 to 4.7e-4 against 4.4e-2 to 8.4e-2 at 128³; density 1.8e-3 to 8.5e-3 against 4.2e-2 to 0.11 at 64³, and 2.3e-3 to 2.2e-2 against 0.35 to 0.92 at 128³.

The water race, 300 frames of the Dam Break without its obstacle, measured as the FFT solve was (`gpu_flip_cost_probe`, `_refined`; the engine's side is `race_probe.rs`). FFT numbers are its record:

| Row | GPU FLIP 64³ | FFT 64³ | GPU FLIP 128³ | FFT 128³ | Engine 64³ / 128³ |
|---|---|---|---|---|---|
| particles past rest, worst / last 30 frames | 10.4% / 5.8% | 14.7% / 6.7% | 12.1% / 7.2% | 16.2% / 7.4% | 20.6% / 12.6%; 24.0% / 15.0% |
| particles missing inside, worst / last 30 frames | 13.8% / 6.9% | 10.3% / 5.3% | 14.5% / 7.8% | 12.0% / 6.7% | 22.2% / 21.6%; 22.8% / 22.5% |
| divergence left, rms median / max worst (/s) | 4.2e-5 / 9.2e-3 | 1.3e-4 / 0.33 | 5.1e-4 / 0.29 | 2.7e-2 / 39 | not measured |
| meshed volume, max / last (raw mesh at the last frame) | 11.6% / +6.0% (+1.2%) | 3.10% / −0.99% | 7.6% / +3.2% (+2.6%) | 5.14% / −1.00% | 13.9% / +13.9%; 11.1% / +10.4% |
| top speed over the run | 15.8 m/s | 11.8 m/s | 23.4 m/s | 14.4 m/s | 12.2; 17.3 m/s |
| settled speed p99, last 30 frames | 1.49 m/s | 1.36 m/s | 1.52 m/s | 1.51 m/s | 1.75; 1.40 m/s |
| GPU ms per frame, step / meshed | 12.2 / 17.2 | — / 37 | 77 / 330 (contended) | 158 / 276 | 509; 2978 wall |

Read it this way. Fewer particles pack past rest than under the FFT solve (10.4% against 14.7% worst at 64³). More go missing inside: 3.5 points worse at the worst frame and 1.6 points worse over the last 30 frames at 64³; at 128³ the gaps are 2.5 and 1.1 points. The density share went from 0.5 to 1 between those records, which trades one for the other. The projection leaves 3× less divergence than the FFT solve at 64³ and 50× less at 128³ (rms median). Now that a wall face carries the leaving water's flux, the solve has more to remove there, and the leftover rose from 1.0e-5 at 64³. GPU FLIP throws faster than the FFT record: the FFT solve's leftover divergence damped the splash, and since the walls change a step keeps the PIC damping per second. The meshed-volume row is not same-tree: the surface changed after the FFT record (the solid clamp, `a86ae55fd`), and its frame-0 skin moved from 30.2 to 35.3 mm at 64³. The skin-corrected figure peaks during the splash at frame 29, before any lid contact. The raw mesh at the last frame is within 1.2% at 64³ and 2.6% at 128³, so the water keeps its volume. The 64³ step-alone ms is a quiet profiled frame (`gpu_flip_frame_by_node_type`); the other ms rows ran with other sessions on the GPU.

**Volume, Kugelstadt projection** (`gpu_flip_dam_break_settles_to_its_volume`, the 64³ Dam Break, 1800 frames). Before it, the settled interior read 7.41 particles a cell against 8 (−7.3%) and the pool stood 13–21% high. With it:

| Row | 1 step | 2 steps |
|---|---|---|
| column depth against 0.6556 m | +2.38% | +2.42% |
| interior density against 8 | −0.37% | −0.37% |
| worst frame-to-frame energy rise from frame 400 | +8.5e-5 E0 | +1.1e-5 E0 |
| energy above E0 | none | none |

The standing wave's period is 2.817 s against 2.80 s. A still pool and the pool round a static box rest at 1e-6 and 5e-5 m/s, as before the projection. The column stands about 2% high because a surface cell beside an empty cell is only ever spread, never pulled together (the paper's clamp); measuring air from the level-set mask instead pulled the surface in to +0.2% but pushed a pool at rest every step. The rows above this one predate the projection.

**Walls and steps** (`gpu_flip_wall_feel_64`, the meshed 64³ Dam Break, 300 frames; the engine's column is the parity audit's `/tmp/flip_feel/engine.csv`). Run-up is the highest particle within 25 cm of the far wall. Lid contact counts particle-frames within 10 cm of the lid over frames 50–130.

| Row | Engine | Before the wall fix, 2 steps | 2 steps | 1 step (shipped) |
|---|---|---|---|---|
| run-up at frames 59 / 74 / 89 | 2.00 / 3.06 / 3.61 m | 3.02 / 3.99 / 3.99 m | 3.19 / 3.99 / 3.97 m | 2.81 / 3.99 / 3.98 m |
| lid contact | 5,088 | 41,291 | 12,764 | 12,451 |
| lid clear | from frame 127 | from frame 153 | empty at frame 104 and every 15th frame to 269; a late throw touches it at 284–290 | from frame 104 |
| meshed volume, max / last | — | 9.7% / +5.0% | 11.6% / +6.0% | 9.2% / +4.9% |
| fastest water, cells a step | about 3 | 1.6 | 2.1 | 4.3 |

One step and two now touch the lid about equally, so extra steps no longer make water stick. One step a tick ships (Peter, 2026-10-02); at one step a 64³ splash crosses 4.3 cells, so the CFL guard fires and is reported (section 1). GPU FLIP's wave still runs up faster and higher than the engine's. That is livelier water, not water held at the lid: the lid is empty by frame 104 in both. Matching the engine's damping is not a goal.

**Pool height: do not tune toward the engine.** The engine's settled pool stands about 16% above the true depth, because its water gains volume: it marks liquid from a particle distance that reaches about 0.37 cells past the particles, and it has no volume control. GPU FLIP settles at the true depth (3.24 against 3.217 by the parity audit's level measure). The engine's meshed volume is +13.9% at 64³ and +10.4% at 128³ in the race table. A GPU FLIP pool lower than the engine's is correct.

**The free surface** (`gpu_flip_ghost_fluid_64`, `_refined`: the meshed Dam Break, 300 frames, the same tree with the ghost rows off and on; the engine from `gpu_flip_engine_splash_64` and `gpu_flip_engine_race_refined`). Breakup is the share of particles detached from the main body, over frames 30–150; pieces counts the detached clumps.

| Row | Plain 64³ | Ghost 64³ | Engine 64³ | Plain 128³ | Ghost 128³ | Engine 128³ |
|---|---|---|---|---|---|---|
| breakup peak / mean | 0.56% / 0.30% | 0.70% / 0.38% | 0.93% / 0.42% | 1.17% / 0.56% | 1.17% / 0.58% | 1.69% / 0.86% |
| pieces peak / mean | 578 / 359 | 659 / 425 | 439 / 289 | 6,113 / 3,538 | 6,244 / 3,658 | 8,378 / 5,147 |
| particles past rest, worst / last 30 | 10.3% / 6.0% | 10.5% / 5.9% | 20.6% / 18.3% | 12.2% / 7.1% | 12.0% / 7.1% | 24.0% / 15.0% |
| particles missing inside, worst / last 30 | 13.7% / 7.2% | 13.7% / 7.0% | 13.7% / 11.8% | 14.5% / 7.7% | 14.3% / 7.7% | 22.8% / 22.5% |
| divergence left, rms median / worst | 4.2e-5 / 2.9e-4 | 3.7e-5 / 2.9e-4 | — | 5.0e-4 / 4.8e-3 | 4.9e-4 / 4.4e-3 | — |
| water volume drift, max / last | 11.8% / +6.3% | 12.0% / +6.0% | 7.8% / +2.7% | 7.5% / +3.4% | 7.4% / +3.4% | 11.1% / +10.4% |
| top speed | 15.6 m/s | 15.3 m/s | 12.2 m/s | 26.0 m/s | 24.0 m/s | 17.3 m/s |
| settled speed p99, last 30 | 1.46 m/s | 1.61 m/s | 4.72 m/s | 1.64 m/s | 1.66 m/s | 1.40 m/s |
| lid contact (particle-frames, frames 50–130) | 12,134 | 20,379 | 5,088 | 142,399 | 173,198 | — |
| last frame at the lid | 184 | 102 | — | 299 | 299 | — |
| GPU ms per frame, median | 17.0 | 18.1 | — | 101 | 106 | — |

Read it this way. At 64³ the ghost rows move the breakup toward the engine's (mean 0.30% to 0.38%, against 0.42%); at 128³ they barely move it (0.56% to 0.58%, against 0.86%). Packing, holes, divergence and volume are the same or a little better. More water reaches the lid (1.7× at 64³, 1.2× at 128³), but at 64³ it leaves sooner: the lid is clear from frame 102, against 184 with plain rows. 16 iterations instead of 8 change none of this, so the shipped count does not hold the surface back. The surface costs 1.1 ms a frame at 64³ and 5 ms at 128³, most of it the particle distance gather.

**The transfer kernel** (`gpu_flip_transfer_kernel_64`, `_refined`: the same scene with the ghost rows, the tent against the engine's Wyvill kernel at r = √3·h/2). The tent reached the box corners and about 1.15× further along each axis, averaging over more particles and blurring the velocity differences that tear a sheet.

| Row | Tent 64³ | Wyvill 64³ | Tent 128³ | Wyvill 128³ |
|---|---|---|---|---|
| breakup peak / mean | 0.70% / 0.38% | 0.77% / 0.42% | 1.17% / 0.58% | 1.24% / 0.65% |
| pieces peak / mean | 659 / 425 | 731 / 469 | 6,244 / 3,658 | 6,761 / 4,180 |
| particles past rest, worst / last 30 | 10.5% / 5.9% | 10.3% / 6.0% | 12.0% / 7.1% | 11.9% / 7.2% |
| particles missing inside, worst / last 30 | 13.7% / 7.0% | 13.6% / 6.8% | 14.3% / 7.7% | 14.5% / 7.8% |
| divergence left, rms median / worst | 3.7e-5 / 2.9e-4 | 3.2e-5 / 3.1e-4 | 4.9e-4 / 4.4e-3 | 3.5e-4 / 4.2e-3 |
| water volume drift, max / last | 12.0% / +6.0% | 12.9% / +5.5% | 7.4% / +3.4% | 7.8% / +3.1% |
| top speed | 15.3 m/s | 15.5 m/s | 24.0 m/s | 23.9 m/s |
| settled speed p99, last 30 | 1.61 m/s | 1.35 m/s | 1.66 m/s | 1.62 m/s |
| lid contact / last frame at the lid | 20,379 / 102 | 20,933 / 103 | 173,198 / 299 | 190,955 / 299 |
| GPU ms per frame, median | 18.1 | 17.8 | 106 | 110 |

Breakup moves toward the engine's at both sizes: at 64³ the mean now matches it (0.42%), at 128³ it closes a quarter of the gap. The other rows hold within a point; the settled pool is calmer, the peak volume drift is 0.9 points higher at 64³, and 10% more water touches the lid at 128³. Kept. Where no particle reaches a face, extension fills it; beside a side wall it copies the wall face's held zero, so a thin film on that wall carries none across it (`face_grid_demo_gpu_flip_and_matter_side_by_side` allows 1% of a layer for these).

### Against the FLIP Fluids engine

Matched on purpose: liquid cells are the particle level set's φ < 0, dilated √3·h/2 past the particles (`pressuresolver.cpp` `_initialize`); the Wyvill particle-to-face kernel at radius √3·h/2 (`velocityadvector.cpp`); RK3 advection with 2/9, 3/9, 4/9 weights; particles kept 0.2 cells off solids; closed free-slip walls, velocity 0 on every wall face; extension by the mean of finished neighbours; the 95% FLIP blend per 1/60 s; one wall constraint written into both the FLIP reference and the current field (the engine's `_constrainVelocityFields` sets both, `fluidsimulation.cpp` 6933–6934).

Different on purpose:

| Stage | Engine | GPU FLIP | Why |
|---|---|---|---|
| Wall position | the zero face sits half a cell past the wall | exactly on the wall | the engine's offset lets water creep half a cell into the wall; it feels less sticky only by accident |
| Step | CFL 5 (up to 5 cells a step), 12 extension layers | one 1/60 s step by default, a counted 20 m/s guard, ⌈√3 · travel⌉ + 3 layers (14 at 64³) | big steps are an accuracy shortcut; ours keeps travel to a few cells |
| Volume | no correction | Kugelstadt et al. 2019's density projection | without it the interior thins 7% and the pool stands 13–21% high; see "Volume and energy" |
| Wall collision | `_resolveCollision` marches each particle's move against the solid (`fluidsimulation.cpp` 8303–8378) | the walls are closed faces and the move is clamped 0.2 cells inside | deliberate non-port: judged on physics, not engine match. The march is what lowers the engine's run-up; our energy proof shows nothing in our step adds energy, so a higher run-up is not a fault |

## 7. Invariants & enforcement

| # | Invariant | Machine check |
|---|---|---|
| I1 | No node-grid velocity in the liquid path | `rg -n "node_vel\|NodeVelocity\|matter_" crates/manifold-renderer/src/node_graph/primitives -g "{gpu_flip_,coarse_inverse,dot_products,divide_by_value}*"` returns zero |
| I2 | No CPU readback inside a frame; the stop is on the GPU | the same files hold no `read_back`, `readback` or `wait_until_completed` |
| I3 | Every pass of the step and the solver has a value proof against a CPU reference | `gpu_flip_step_tests.rs` (one test per pass), `gpu_flip_pressure_tests.rs`; `step_shader_validates_with_every_entry` |
| I4 | The GPU solve matches the f64 reference | `pressure_module_matches_reference_64`, `_128`, `_odd_sides` and `_deep_pool`: the solver against `scripts/mgpcg_reference.py` at the shipped counts, within the f32 floor |
| I5 | Water volume is kept | `gpu_flip_still_pool`, `gpu_flip_still_pool_keeps_its_meshed_volume`, `gpu_flip_free_fall_keeps_g` |
| I6 | Every buffer covers every dispatch before the GPU sees it | `LIQUID_EXTENT_RULES`; `gpu_flip_*_cover_every_dispatch`; `transfers_stay_inside_their_lattices`; `liquid_presets_all_extent_checked` walks `WaterDamBreakGpuFlip.json` |
| I7 | The density correction never becomes velocity | `gpu_flip_faces_to_particles_blends_flip_and_moves_by_rk3` |
| I8 | No atomic exchange or compare-exchange anywhere in the step, the solve or the body passes; integer atomicAdd/Min/Max only, each site named in the guard's allowlist, and only where the result is exact and independent of thread order (fixed-point sums with carry). Amended 2026-10-02 for the sealed-pocket sums. | `step_shader_atomics_are_allowlisted_integer_sums`, `pocket_carry_sum_is_exact_in_any_order`, `pressure_solver_uses_no_atomics`, `body_shader_uses_no_atomics` |
| I10 | The V-cycle depth follows the lattice, halving each side rounding up until every side is 4 or less; the coarsest level is solved exactly | `levels_halve_rounding_up_to_four`, `pass_counts_follow_the_levels`, `pressure_module_passes_match_the_count` |
| I11 | A box wall face is 0 in `old`, the forced field, the projection and the constraint | `gpu_flip_particles_to_faces_matches_the_wyvill_sum`, `gpu_flip_face_gravity_adds_gravity_and_holds_the_walls`, `gpu_flip_subtract_pressure_projects_faces_touching_water` (walls held and kept both drawn) |
| I12 | No RK3 stage moves past the CFL guard, and `new` is extended far enough for it | `gpu_flip_faces_to_particles_blends_flip_and_moves_by_rk3` (stages within and past the guard); `gpu_flip_band_follows_the_cfl_guard` |
| I13 | The ghost rows are the engine's: θ clamped to ±25, a zero diagonal out of the system, the water side's φ at most −0.005h, the air side's at least 0; zero φ gives the plain rows; the projection uses the solve's θ, leaving exactly its residual; φ is the engine's level set at r = √3·h/2 | `pressure_module_solves_the_ghost_rows`, `gpu_flip_subtract_pressure_projects_faces_touching_water` (plain and ghost each; divergence left equals div − L p per water cell), `gpu_flip_particle_distance_is_the_engines_level_set` |
| I9 | The domain meets the liquid contract | the liquid conformance suite (`liquid_conformance_covers_every_domain`, `tests/gpu_proofs/liquid_conformance.rs`) |

## 8. Owed

### Solids in the water

Peter's scenes have boxes and obstacles in the water, and the engine's Dam Break has one. The domain takes Collider roles (static and moved by their transform) and owns a Box3D world for two-way coupling. The coupled conformance checks run on the GPU FLIP row except Collision, which needs open faces: in a closed tank the walls take the box's momentum at once (BUG-0d7t (GPU FLIP open-face boundaries)). The race rows, the draft against the engine's and the demo are owed: BUG-4jfv (GPU FLIP solids gate).

- **Amended by** LIQUID_SOLVER_SEAM_DESIGN.md D7 (bodies inside the pressure solve) and D12 (solids through the shared distance lattice): body mass goes inside the pressure solve; solids come from the shared distance lattice; an analytic box clip is rejected. Where this section and the seam doc disagree, the seam doc wins.
- **Why it is simpler now:** the FLIP Fluids engine's operator is a weighted Laplacian, each face carrying its open fraction w_f in [0, 1] (`pressuresolver.cpp` `_solidBoundaryWeights`). Multigrid takes weights directly: L gains w_f per face, the smoother and residual read the weights, and restriction averages them. The FFT solve needed a face collar to reach the same operator.
- **Deliverables:** face weights and solid face velocities from the distance lattice (codegen); the weighted L in the smoother and the residual and the coarse weights in the restriction; the body rows of D7 in the reference script first; the pressure impulse and torque per body as a barriered reduction, no atomics; `StepCoupling` / `SubstepExchange` through `manifold-physics`, no `matter_*` import.
- **Gate:** the reference exact against a direct weighted solve to 1e-10; iterations to the empty tank's residual up at most 25% with the box. Hydrostatic lift of a fixed submerged box within 2% of ρgV; a half-density Box3D box settles at its analytic draft within one cell, and within one cell of the engine's. The race rows on the Dam Break as shipped, obstacle included, at 64³ and 128³. Body force against iteration count: the net force and torque at 4, 6, 8, 12 and 16 iterations on a submerged and a floating box; the smallest count within 1% of converged with no visible jitter sets the count when bodies are present.
- **Settled:** a body's pose updates every substep; the moving-obstacle push holds at Steps 1 and 2 (`gpu_flip_moving_obstacle_pushes_the_pool`).
- **Kill check:** iterations to the empty tank's residual up more than 50% with the box → stop and report the numbers.
- **Demo:** L2 — the Dam Break as shipped, obstacle included, GPU FLIP beside the engine, 300 frames headless. **Performer gesture:** the wave breaks around the box, then a floating box rides the slosh.
- **Face weights, as built** (the step's `open_fractions` pass, the solver's `coarsen_faces_main`). Measured by `scripts/mgpcg_reference.py --box`: on the seven committed Dam Break problems with a submerged box, the reference matches a direct sparse solve to 3e-15, and the iterations to the empty tank's residual rise by at most one (+25% at 4, +17% at 6, +12% at 8). Where it departs from the engine:
  - A coarse face's weight is the mean of the four fine faces it covers. The engine has no multigrid, so this is ours.
  - A water cell with every face closed drops out: the smoother and residual give 0 there, the coarse inverse pins it.
  - The box walls are closed faces of the solid like any other: open fraction 0 in the divergence and the operator, velocity 0 after the constraint.
  - A closed face (weight 0) keeps its velocity through the projection and stays valid for extension; `constrain_solid_faces` then sets it.
  - The distance lattice is sampled at the step's pose, once per step.
- **Moving solids, as built** (the step's `solid_face_velocity` and `constrain_solid_faces` passes). Divergence adds the engine's C·v_s term, (c − w)·v_s through each inner face with c the cell's open volume (`open_fractions` writes it in weight w, as `_getCellWeight` takes it). After the projection the step constrains both the projected and the saved faces, as the engine does: a closed face takes v_s, a cut face f·v_s + (1 − f)·u. Where it departs from the engine:
  - v_s is the closest body's rigid velocity, v + ω × (x − c), at the face centre. The engine interpolates the nearest triangle's vertex velocities from its mesh level set. For rigid bodies the two agree at the surface.
  - Friction f comes from the bodies only; the engine's domain-wall friction is not ported (its default is 0, free slip).
  - The density solve's subtract is not constrained again: it moves particles through `advect` and is never kept as velocity.
  - Water a solid seals off from air takes no push from it (`_conditionSolidVelocityField`, pressuresolver.cpp line 214; the step's `pocket_*` passes). Air spreads from dry cells through faces open at least 1e-6; every water cell it never reaches has all six solid face velocities zeroed. Ported as the engine does it: the pass is skipped while bodies are in the solve (pressuresolver.cpp lines 57-61, only without `_rigidCoupling`), and a sealed pocket of one cell keeps its velocity (the `group.size() == 1` skip, about line 312). Departures: an open tank face counts as air; the spread is axis line sweeps, forward and back, repeated until nothing changes, at most one round per cell of the lattice's longest side; a step whose spread is still changing at that cap is counted in stats word 13 and Liquid State refuses the tick by name, so an unfinished spread is never read as sealed or open. Tested by `gpu_flip_sealed_pockets_zero_the_solid_velocity_as_the_engine`, `gpu_flip_pocket_spread_reports_an_unfinished_cap` and `gpu_flip_box_pressed_into_a_full_tank_converges_and_escapes_when_open`.
  - Which water reaches air is found every step, bodies or not; only the solid velocity's zeroing is skipped under bodies. The same line sweeps label each sealed pocket by its lowest cell index.
  - Our departure from the engine: in both the pressure and the density solve, each sealed pocket's mean is taken off its right-hand side. With no air cell a pocket's solve is pure Neumann and has a solution only for a right-hand side summing to zero, which a body pushing in, or the crowded layer it sweeps, breaks (BUG-n3od (density solve reaches its cap under a pressed lid)). Physically, sealed water compresses evenly by what the body swept, and the density projection, which is ours and not the engine's, restores it once the pocket reaches air again. Sums are 64-bit fixed point, so the result does not depend on thread order. The volume rate removed is stats words 14 (pressure) and 15 (density). Tested by `gpu_flip_sealed_pockets_zero_the_solid_velocity_as_the_engine` and the pressed-lid scene.
- **Collider roles, as built** (`liquid_fill`, the step's `phi_into_solids`, `water_into_solids` and `faces_to_particles`). Tested by `gpu_flip_dam_break_flows_around_the_obstacle`, `gpu_flip_still_pool_rests_round_a_static_obstacle` and `gpu_flip_moving_obstacle_pushes_the_pool`. The engine's rules, ported:
  - The fill leaves a site inside a solid at the epoch's pose dead (radius 0), as seeding keeps only sites where the solid distance is positive.
  - A particle's move is marched in 0.1-cell steps; at the first sample inside a solid it is pushed out along the distance's gradient to 0.2 cells outside, or kept at the last outside sample when the push lands inside or moves more than 5 cells (`_resolveCollision`). A particle a moving solid sweeps over is removed (`_removeMarkerParticles`).
  - A cell within half a cell of the water whose solid centre distance is negative takes φ = −h/2 and counts as water (`ParticleLevelSet::postProcessSignedDistanceField`). It makes the waterline creep up a static box's sides: about 0.1 m/s at its peak, 1.6 cm/s after 2 s at 64³, level unchanged.
  - The box walls are in the solid distance with the bodies, as the engine's inverted domain object is. Without them, a body flush with a wall leaves a sub-cell open channel between its lattice distance and the wall, and the still pool round a floor-resting box blew up by frame 23. Departure: our walls sit on the lattice edge, the engine's three cells in.
- **Bodies in the solve, as built** (`gpu_flip_bodies.rs`, `shaders/gpu_flip_bodies.wgsl`, run by the step when `dynamic_bodies` is above 0). The engine's order (`rigidfluidcoupling.cpp`): predict each body's velocity as v + a·t + M⁻¹·reaction in `solid_face_velocity`; solve with A + ρh·G·M⁻¹·Gᵀ, the bodies' share added to the operator on every search direction (`rigidpressurecoupling.h`); take the pressure's impulse per body; add the resulting velocity change to the solid faces (`rigidboundaryvelocity.cpp`); constrain. Tested by `gpu_flip_body_operator_matches_cpu`, `gpu_flip_body_reaction_matches_cpu` and `gpu_flip_solid_face_velocity_matches_cpu`. Contracts:
  - `solid_face_velocity` writes each solid face's owner code in velocity w: Σ over axes a of (b + 1)·256^a, b the body's index from the tick's first row. At most 64 bodies (`MAX_FLUID_ROLES`, a compile-time assert).
  - The reaction is 8 floats per body (linear then angular impulse, w = 0), aliased step to step and added to over the tick; the domain clears it at the tick's start. A step with dynamic bodies and no reaction wired, or one too short, is a named error.
  - The per-body sums are a fixed two-pass reduction (partials, then one workgroup of 64), so the result is the same run to run.
  - The friction reaction, ρh³·w·f·(u − v_s) per cut face, is ours: the engine keeps none.
  - The density solve stays uncoupled.
- **Coupled, as built** (`gpu_flip_domain.rs`, `liquid/coupling.rs`). The domain owns the scene's Box3D world through the shared rigid owner: it steps the world once per settled tick, chains the reaction through the tick's steps, and hands it back at the host sync. Measured on the conformance scenes at 32³ (`tests/gpu_proofs/liquid_conformance.rs`):
  - A box as dense as the water, held under it, feels 627.1 N against ρgV 627.8 N (−0.12%).
  - A half-density box dropped tilted settles with its centre 0.26 cells under the waterline.
  - Before contact the coupled box is drawn exactly where Box3D alone puts it; the world steps once per tick at 60 and 30 fps.
  - Body force against iteration count (`gpu_flip_body_push_against_iterations`): at 4, 6, 8, 12 and 16 iterations the mean force and torque on both boxes are within 0.8% of 64 iterations, and the extra shake is under 0.003 cells. 4 is the smallest steady count, so Auto (8) stands with bodies. 16 matches 64 bit for bit: the solve reaches the f32 floor by then.
- **Forbidden:** whole-cell solids; a CPU wait for the reaction inside the tick; atomics in the per-body reduction; editing the engine.

### Tracked in beads

- Occupied-block passes (only blocks near water dispatch): BUG-l2h3 child .10.
- Lid slabs: BUG-h8or.

## 9. Decided — do not reopen

1. Face-grid native; no node↔face bridge.
2. Air removed with pressure zero; no air phase.
3. No readback inside the frame. The iteration count is the engine's convergence stop under a fixed cap, decided by BUG-l2h3.23 (the engine ports), not a fixed count.
4. Gather-form transfers; atomics only as I8 allows.
5. The density correction moves particles only.
6. Multigrid-preconditioned CG replaces the FFT capacitance solve (2026-10-01, the MGPCG swap brief). This reverses the FFT record's decided item 8, "No multigrid FLIP".
7. The opponent is the FLIP Fluids engine; MPM is information only.

## 10. Deferred

| Item | Revives when |
|---|---|
| Warm start from last step's pressure | a measured iteration saving at the worst frame (it saved none for the FFT solve) |
| Fewer dispatches per iteration (fused smoother levels, a single-pass dot) | the CG vectors row passes a third of the solve, or Vulkan's dispatch cost |
| APIC on faces | a visible PIC/FLIP noise complaint |
| Wrap-around (torus) axes | the endless-ocean scene is asked for; multigrid wraps with periodic transfers |
