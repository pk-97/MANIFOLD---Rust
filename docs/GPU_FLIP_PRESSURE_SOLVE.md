# GPU FLIP — the GPU water solver and its multigrid pressure solve

<!-- index: The GPU water solver (GPU FLIP, formerly SWASH): PIC/FLIP particles on a face grid, one liquid tick of two water steps, and a multigrid-preconditioned conjugate gradient pressure solve with a fixed iteration count. The step, the equation, the solve, the Auto iteration rule, the named refusals, the measures against the FLIP Fluids engine, and what is still owed (solids). -->

**Status:** BUILT on `feat/gpu-flip-multigrid` · 2026-10-01 · owed: solids in the water (section 8 (owed)), BUG-l2h3 (SWASH to a live instrument) child .10 (occupied-block passes), BUG-h8or (lid slabs) · the retired FFT solve is `docs/archive/FFT_WATER_SOLVER_DESIGN.md`.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) before the solids phase.

GPU FLIP is the liquid water solver: particles carry the water, a face (MAC) grid carries its velocity, and each step makes that velocity divergence-free with one pressure solve. The solve is the textbook multigrid-preconditioned conjugate gradient (McAdams, Sifakis and Teran, "A parallel multigrid Poisson solver for fluids simulation on large grids", 2010). It replaced the FFT capacitance solve on 2026-10-01: the same equation, 3.0× faster at 64³ and 3.7× at 128³, to a smaller residual.

On stage: water that doesn't squash or fizz. A dam collapses, sloshes and settles into a pool that is actually still, on the GPU, at the frame budget.

Companions: `LIQUID_SOLVER_SEAM_DESIGN.md` (the scene contract the domain speaks; P7a (GPU FLIP on the contract)), `GPU_WHITEWATER_DESIGN.md` (spray, foam and bubbles on GPU FLIP's face grid), `GPU_FLUID_SURFACE_DESIGN.md` (the surface the particles are meshed into).

## 1. The step

The builder is `crates/manifold-renderer/src/node_graph/primitives/gpu_flip_preset.rs` (`water_step`); the shipped preset is its Dam Break at 64 (`WaterDamBreakGpuFlip.json`, "Water — Dam Break (GPU FLIP)"). `node.gpu_flip_domain` owns the clock: `node.liquid_state`'s tick region runs one 60 Hz tick of `STEPS_PER_TICK` = 2 water steps, the density solve on the last, then `node.liquid_stats`. Per step:

1. Sort particles into cells (`node.sort_particles_into_cells`, bins = grid cells).
2. Water cells: a cell is water if it holds a particle (`node.cells_with_particles`); walls are the box faces.
3. Particles to faces by gather, no atomics (`node.particles_to_faces`): each face reads the particles in its neighbouring cells. Extend two layers into air (`node.extend_faces`) and keep the copy for FLIP (`old`).
4. Gravity, walls zeroed (`node.face_gravity`).
5. Divergence per water cell → f (`node.face_divergence`).
6. The pressure solve, section 3.
7. Subtract the pressure gradient on faces touching water (`node.subtract_pressure`), extend two layers (`new`).
8. On the tick's last step, the density solve: `node.density_source` asks each water cell for −rate·e with e = count/8 − 1, solved like section 3 at `DENSITY_ITERATIONS`, subtracted from the projected faces into a separate `advect` grid. A cell is inside, and gets the two-sided source, only when each of its six neighbours holds at least half of rest (4) or is a wall; any other cell is at the surface and only spreads. Counting any particle as water made a part-full surface cell under a stray particle a sink, which pulled the surface down and packed the water below it. The share per solve, rate × step dt, is 1 (`SPREAD_PER_STEP`). It moves particles and never becomes their velocity: kept as velocity, a fast splash's correction became speed.
9. Faces to particles (`node.faces_to_particles`): PIC/FLIP velocity from `new` and `old`; the RK3 move through `new` plus the density correction step dt · (`advect` − `new`), capped at half a cell so a particle never overshoots the cell it is spreading from; clamped 0.2 cells off the walls.

## 2. The equation

On the water cells W of an n_x × n_y × n_z lattice with cell size h:

- L p = f on W, p = 0 on air cells, walls closed (no flux through a box face).
- (L q)_i = (Σ over in-box neighbours j of (j ∈ W ? q_j : 0) − inside_i · q_i) / h², where inside_i counts i's in-box neighbours.

Air at zero pressure is the first-order free surface. A water body that touches no air (a closed box full of water) makes L singular. It is tolerated, not refused: the density source's right-hand side need not sum to zero there, so no exact solution exists; the conjugate gradient removes the part it can and leaves the rest as residual, and the coarsest level pins one cell of each such body at zero instead of dividing by zero (section 3).

## 3. The solve

Conjugate gradient in the L form, from x = 0, r = f, p = 0, rz = 0. Each iteration: z = V(r); rz_new = r·z; β = rz_new / rz_old (0 when rz_old is 0); p = z + βp; s = −Lp (`node.pressure_residual` with rhs 0); α = rz_new / (p·s); x −= αp; r −= αs. `node.conjugate_gradient` is the loop boundary: a region of fixed iteration count with no readback, owning x, r, p and rz across iterations. `node.dot_products` reduces (two passes, barriered), `node.divide_by_value` makes β and α on the GPU, `node.combine_rows` does the axpy updates.

V(r) is one V-cycle for L e = r, from e = 0:

- Levels halve while every side is even and one side is over 4 (`coarse_inverse::multigrid_levels`): 64³ ends at 4³, 96³ at 3³. A coarse cell is water only if all eight children are water (`node.coarsen_water`); a coarse cell with an air child is air.
- Pre-smooth: 2 rounds of red-black Gauss-Seidel (red then black, `node.pressure_smooth`), from a zero lattice (`node.zero_lattice`).
- Residual r − Le (`node.pressure_residual`), full-weighting restriction to the next level (`node.restrict_lattice`, the transpose of prolongation, masked to coarse water).
- Recurse. The coarsest level, at most 64 cells, is solved exactly: `node.coarse_inverse` builds A⁻¹ (L = −A/h²) once per water lattice, outside the loop, by symmetric elimination in one workgroup, and each V-cycle applies it with `node.combine_rows` (e = −h² A⁻¹ r). The inverse is exactly symmetric, so the V-cycle stays a symmetric preconditioner. A water body with no air pins one cell at zero. Sweeps were not enough: on a deep pool (the coarsest level water but for its top row) 8 sweeps each way left a 1e-1 coarse residual and 512 were needed for 1e-9, while a 512-cell inverse does not fit one workgroup, so the levels go one halving deeper instead.
- Prolong-add the correction, trilinear (3/4 and 1/4 per axis, clamped at the box) (`node.prolong_lattice`), masked to water.
- Post-smooth: 2 rounds, black then red, so the cycle is symmetric and the preconditioner is too.

Everything per element is a codegen atom with a CPU value proof (`gpu_flip_atom_tests.rs`). Nothing in the solve or the step fuses in the shipped graph: every atom gathers from neighbours or fans out (`gpu_flip_solve_and_step_do_not_fuse`). The fusable pairs are proved bit for bit fused against unfused.

## 4. Iteration counts — the Auto rule

The counts are build params with an Auto rule: the smallest count at which the f64 reference (`scripts/mgpcg_reference.py`) reaches the retired FFT solve's residual on every committed Dam Break problem (`tests/fixtures/dambreak_pressure_problems.bin.zst`, 7 frames) and on dumped splash solves at 64³ and 128³, plus one. Deep water has fixtures too, written by `gpu_flip_write_deep_pool_fixtures`: a 3 m still pool's main solve (`deep_pool_pressure_problems.bin.zst`) and the density solves of a block dropped into it (`deep_pool_density_problems.bin.zst`). With no FFT record there, their target is the tightest FFT residual at that lattice: 5.5e-5 (64³) and 1.7e-3 (128³) for the main solve, 4.2e-2 and 0.35 for the density solve. A multigrid preconditioner's count does not grow with the lattice, so one count serves every size.

- `PRESSURE_ITERATIONS` = 8: the reference needed at most 7 at 64³ and 5 at 128³ on the Dam Break, 6 and 5 on the deep pool.
- `DENSITY_ITERATIONS` = 3: the reference needed 2 at 64³ and 1 at 128³ on the splash solves, 2 and 1 on the deep drop.

Re-run 2026-10-01 for the exact coarsest solve on the Dam Break problems and the deep pool; the splash dumps were not kept, and the Dam Break counts did not move.

The count changes only with this rule re-run, on the same fixtures, with the new count in this section.

## 5. Limits and refusals

Every limit is a named refusal at build; nothing is clamped silently.

- A lattice whose coarsest level is over 64 cells: "every side must halve evenly down to 4 or less", naming the odd sides that stopped the halving (`multigrid_refusal`, surfaced by `node.gpu_flip_domain` as "GPU FLIP: … Change Resolution."). An odd side stops the halving where it stands, so 15 and 63 are refused, and so is 80 (it halves to 5³, 125 cells). 16, 24, 32, 48, 64, 96 and 128 run.
- `node.coarse_inverse` refuses a lattice past 64 cells by name at build and at run; its index walk is `coarse_inverse_indices_stay_in_bounds`.
- `node.prolong_lattice` on an odd side is refused by the extent check, and its kernel writes nothing there.
- More particles than a count carries exactly (2²⁴): refused by name; Resolution 256 is refused by it.
- The solver's lattice is baked into its atoms until seam P7b (wired lattice) lands: another Resolution or Domain Size is refused by name.
- Every array covers every dispatch before the GPU sees it: `LIQUID_EXTENT_RULES` in `node_graph/liquid/extent.rs` holds a rule for every solve atom, walked at 16, 24, 32, 48, 64, 96 and 128 (`gpu_flip_*_cover_every_dispatch`), the pressure graph also at 256.

## 6. Measures

Machine: M4 Max. Load averages were 6–37 during these runs (other sessions on the GPU); ms rows are contended and read as ratios, not budgets.

Solve alone, GPU ms per main solve at the shipped 8 iterations against the FFT solve at its shipped 24 passes (FFT from its record, quiet GPU):

| Lattice | GPU FLIP | FFT | GPU FLIP median residual |
|---|---|---|---|
| 64³ | 3.1 | 9.4 | 6.8e-6 |
| 128³ | 13.8 | 51.5 | 1.1e-5 |

Stage split of the solve (`gpu_flip_solve_stage_split`, `_refined`):

| Stage | 64³ ms (share) | 128³ ms (share) |
|---|---|---|
| smoothing | 1.41 (47%) | 7.54 (54%) |
| residual | 0.13 (4%) | 0.80 (6%) |
| restriction and prolongation | 0.30 (10%) | 1.62 (12%) |
| coarsest level (apply the inverse) | 0.19 (6%) | 0.12 (1%) |
| CG vectors (dots, divides, axpys) | 0.81 (27%) | 3.64 (26%) |
| setup (coarse water, the inverse, zeros) | 0.14 (5%) | 0.26 (2%) |

Iteration trend, median residual and GPU ms per solve (`gpu_flip_iteration_trend`, `_refined`):

| Iterations | 64³ | 128³ |
|---|---|---|
| 4 | 3.0e-3, 1.7 ms | 4.6e-3, 7.2 ms |
| 6 | 1.3e-4, 2.4 ms | 1.2e-4, 10.8 ms |
| 8 (shipped) | 6.8e-6, 3.1 ms | 1.1e-5, 13.8 ms |
| 12 | 3.1e-6, 4.5 ms | 1.1e-5, 20.6 ms |

The residual stops falling near 1e-5: that is f32.

On solves dumped from a running FFT Dam Break (splash frames 50, 70 and 90, both steps, `gpu_flip_real_frames_against_fft`), the GPU residual at the shipped counts is below the FFT solve's on every solve: main 9.0e-6 to 5.2e-5 against 1.4e-3 to 3.8e-3 at 64³, and 7.0e-5 to 4.7e-4 against 4.4e-2 to 8.4e-2 at 128³; density 1.8e-3 to 8.5e-3 against 4.2e-2 to 0.11 at 64³, and 2.3e-3 to 2.2e-2 against 0.35 to 0.92 at 128³.

The water race, 300 frames of the Dam Break without its obstacle, measured as the FFT solve was (`gpu_flip_cost_probe`, `_refined`; the engine's side is `race_probe.rs`). FFT numbers are its record:

| Row | GPU FLIP 64³ | FFT 64³ | GPU FLIP 128³ | FFT 128³ | Engine 64³ / 128³ |
|---|---|---|---|---|---|
| particles past rest, worst / last 30 frames | 14.7% / 6.3% | 14.7% / 6.7% | 17.1% / 7.5% | 16.2% / 7.4% | 20.6% / 12.6%; 24.0% / 15.0% |
| particles missing inside, worst / last 30 frames | 9.3% / 5.3% | 10.3% / 5.3% | 10.4% / 6.9% | 12.0% / 6.7% | 22.2% / 21.6%; 22.8% / 22.5% |
| divergence left, rms median / max worst (/s) | 6.7e-6 / 4.0e-3 | 1.3e-4 / 0.33 | 7.4e-5 / 0.13 | 2.7e-2 / 39 | not measured |
| meshed volume, max / last | 4.25% / +2.39% | 3.10% / −0.99% | 4.65% / +1.30% | 5.14% / −1.00% | 13.9% / +13.9%; 11.1% / +10.4% |
| top speed over the run | 12.3 m/s | 11.8 m/s | 19.0 m/s | 14.4 m/s | 12.2; 17.3 m/s |
| settled speed p99, last 30 frames | 1.33 m/s | 1.36 m/s | 1.51 m/s | 1.51 m/s | 1.75; 1.40 m/s |
| GPU ms per frame, step / meshed (contended) | 15.0 / 19.7 | — / 37 | 91 / 126 | 158 / 276 | 509; 2978 wall |

Read it this way. The water measures (particles past rest and missing) are equal, within a point either way. The projection leaves 19× less divergence at 64³ and 360× less at 128³ (rms median). The FFT solve's leftover divergence sat at the surface and damped the splash, so GPU FLIP throws faster at 128³, nearer the engine's 17.3 m/s. The meshed-volume row is not same-tree: the surface changed after the FFT record (the solid clamp, `a86ae55fd`), and its frame-0 skin moved from 30.2 to 35.3 mm at 64³, so that row is a look item here, not a solver comparison.

The density share is 1 per step (`SPREAD_PER_STEP`). Share 1 once overshot at 128³ (48% of particles past rest at frame 119). The cause was the surface rule, not the share: a part-full surface cell under a stray particle counted as inside and became a sink, and a finer lattice has more of them. With the half-full rule and the half-cell cap on the correction (section 1, steps 8 and 9), share 1 gives 12.3% worst past rest at 128³ (`gpu_flip_refined_density_causes` runs share 1 and 0.5 at 64³ and 128³). The race table above predates that change; its rows are re-measured with the wall fixes.

## 7. Invariants & enforcement

| # | Invariant | Machine check |
|---|---|---|
| I1 | No node-grid velocity in the liquid path | `rg -n "node_vel\|NodeVelocity\|matter_" crates/manifold-renderer/src/node_graph/primitives -g "{gpu_flip_,pressure_,coarse_,coarsen_,restrict_,prolong_,zero_lattice,conjugate_,dot_products,combine_rows,divide_by_value}*"` returns zero |
| I2 | No CPU readback inside a frame; the iteration count is fixed | the same files hold no `read_back`, `readback` or `wait_until_completed`; `node.conjugate_gradient`'s count is a build param |
| I3 | Every per-element atom on the codegen path with a value proof | `every_boundary_atom_declares_its_reason`; `gpu_flip_atom_tests.rs` |
| I4 | The GPU solve matches the f64 reference | `gpu_flip_solve_matches_reference`, `_refined` and `_deep_pool`: at 3 iterations within 10% of the reference, at 8 within 2× of it or of the f32 floor (the larger of 3e-5 and the residual after 16 iterations; a deep pool's is 2.9e-4 at 128³, since hydrostatic pressure grows with depth), and on the Dam Break never worse than the FFT solve's pinned residual |
| I5 | Water volume is kept | `gpu_flip_still_pool`, `gpu_flip_still_pool_keeps_its_meshed_volume`, `gpu_flip_free_fall_keeps_g` |
| I6 | Every buffer covers every dispatch before the GPU sees it | `LIQUID_EXTENT_RULES`; `gpu_flip_*_cover_every_dispatch`; `liquid_presets_all_extent_checked` walks `WaterDamBreakGpuFlip.json` |
| I7 | The density correction never becomes velocity | `gpu_flip_faces_to_particles_blends_flip_and_moves_by_rk3` |
| I8 | No atomics in the step or the solve | the liquid conformance row's atomic-free list (`liquid/conformance.rs`); `coarse_inverse_uses_no_atomics` for the hand shader |
| I10 | The coarsest level is solved exactly and symmetrically | `gpu_flip_coarse_inverse_matches_cpu`: bitwise symmetric, A·M the identity on unpinned water, a deep pool and an all-water box among the cases |
| I9 | The domain meets the liquid contract | the liquid conformance suite (`liquid_conformance_covers_every_domain`, `tests/gpu_proofs/liquid_conformance.rs`) |

## 8. Owed

### Solids in the water

Peter's scenes have boxes and obstacles in the water, and the Dam Break as shipped has one. The domain refuses Collider roles and a physics world by name, and the five coupled conformance checks are exempt in the GPU FLIP row (`GPU_FLIP_OWES_SOLIDS`). This phase lifts both.

- **Amended by** LIQUID_SOLVER_SEAM_DESIGN.md D7 (bodies inside the pressure solve) and D12 (solids through the shared distance lattice): body mass goes inside the pressure solve; solids come from the shared distance lattice; an analytic box clip is rejected. Where this section and the seam doc disagree, the seam doc wins.
- **Why it is simpler now:** the FLIP Fluids engine's operator is a weighted Laplacian, each face carrying its open fraction w_f in [0, 1] (`pressuresolver.cpp` `_solidBoundaryWeights`). Multigrid takes weights directly: L gains w_f per face, the smoother and residual read the weights, and restriction averages them. The FFT solve needed a face collar to reach the same operator.
- **Deliverables:** face weights and solid face velocities from the distance lattice (codegen); the weighted L in `pressure_smooth`, `pressure_residual`, `coarse_inverse` and the coarse weights in a restriction atom; the body rows of D7 in the reference script first; the pressure impulse and torque per body as a barriered reduction, no atomics; `StepCoupling` / `SubstepExchange` through `manifold-physics`, no `matter_*` import.
- **Gate:** the reference exact against a direct weighted solve to 1e-10; iterations to the empty tank's residual up at most 25% with the box. Hydrostatic lift of a fixed submerged box within 2% of ρgV; a half-density Box3D box settles at its analytic draft within one cell, and within one cell of the engine's. The race rows on the Dam Break as shipped, obstacle included, at 64³ and 128³. Body force against iteration count: the net force and torque at 4, 6, 8, 12 and 16 iterations on a submerged and a floating box; the smallest count within 1% of converged with no visible jitter sets the count when bodies are present.
- **Open, settled by measurement:** whether a body's pose updates every step or once per tick (`STEPS_PER_TICK` stays a builder constant so either fits).
- **Kill check:** iterations to the empty tank's residual up more than 50% with the box → stop and report the numbers.
- **Demo:** L2 — the Dam Break as shipped, obstacle included, GPU FLIP beside the engine, 300 frames headless. **Performer gesture:** the wave breaks around the box, then a floating box rides the slosh.
- **Forbidden:** whole-cell solids; a CPU wait for the reaction inside the tick; atomics in the per-body reduction; editing the engine.

### Tracked in beads

- Occupied-block passes (only blocks near water dispatch): BUG-l2h3 child .10.
- Lid slabs: BUG-h8or.

## 9. Decided — do not reopen

1. Face-grid native; no node↔face bridge.
2. Air removed with pressure zero; no air phase.
3. Fixed iteration count; no readback inside the frame.
4. Gather-form transfers; no atomics.
5. The density correction moves particles only.
6. Multigrid-preconditioned CG replaces the FFT capacitance solve (2026-10-01, the MGPCG swap brief). This reverses the FFT record's decided item 8, "No multigrid FLIP".
7. The opponent is the FLIP Fluids engine; MPM is information only.

## 10. Deferred

| Item | Revives when |
|---|---|
| Warm start from last step's pressure | a measured iteration saving at the worst frame (it saved none for the FFT solve) |
| Fewer dispatches per iteration (fused smoother levels, a single-pass dot) | the CG vectors row passes a third of the solve, or Vulkan's dispatch cost |
| APIC on faces | a visible PIC/FLIP noise complaint |
| Subcell (ghost-fluid) surface | the look or residual rows lose to the engine at the surface |
| Wrap-around (torus) axes | the endless-ocean scene is asked for; multigrid wraps with periodic transfers |
