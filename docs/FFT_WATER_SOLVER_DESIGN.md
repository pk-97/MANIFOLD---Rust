# FFT Water Solver — a free-surface pressure solve made of FFTs, raced against MPM water

<!-- index: Benchmark-gated challenger to GPU MLS-MPM water: particles on a face (MAC) grid with pressure solved exactly each step by a capacitance collar, whole-box cosine transforms and a six-view surface-FFT helper inside fixed-pass GMRES. Phases: engine 3D FFT/DCT, the collar solve on saved Dam Break problems, the full liquid step raced against the MPM cost probe, a native multigrid baseline, then Peter's call. Touches nothing MPM owns until it wins. -->

**Status:** APPROVED · 2026-09-30 · P0–P4 not built · race outcome is Peter's call at P4, tracked in BUG-wsim (FFT pressure split research).
**Evidence:** `docs/FFT_CAPACITANCE_PRESSURE_FINDINGS.md` (the research record; every number below comes from it).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) before starting any phase.

Peter's decisions, 2026-09-30, not reopened:

- Framing: he chose **"Challenger (Recommended)"** over replacing or merging with MPM. This solver is built as a benchmark-gated challenger to MPM water. MPM continues untouched meanwhile.
- On MPM's rejection of a GPU FLIP-style solver (GPU_MPM_SOLVER_DESIGN.md D1 and D2): "That was done BEFORE we started any of our research". Those decisions are reopened only if P4 shows this solver winning, and only through a seam brief against that doc.

## What it is on stage

Water that does not squash. A dam of water collapses, sloshes and settles into a pool that is actually still, with no fizzing surface and no slow sinking. That is the look gate MPM currently fails (its water is a stiff spring, so it jitters and compresses). The cost target is the Dam Break at 64³ inside a frame budget the MPM path cannot reach today: MPM measured 54.5 ms per frame on the M4 Max (BUG-u3ov (MPM solver budget)).

## 1. Audit — what exists (verified 2026-09-30)

| Piece | Where | State | Use here |
|---|---|---|---|
| Particle record `FluidParticle` (position+radius, velocity, id; 32 bytes) | `crates/manifold-renderer/src/node_graph/fluid_particles.rs:12` | main | the liquid's particles, unchanged |
| `node.sort_particles_into_cells` → `sorted`, `cell_ranges: Array(CellRange)`, `order` | `primitives/sort_particles_into_cells.rs` | main | every particle→grid transfer is a gather over `cell_ranges` |
| `node.particle_volume` (level set from particles) + GPU surface pipeline | `primitives/particle_volume.rs`, `docs/GPU_FLUID_SURFACE_DESIGN.md` | main | acceptance-demo rendering |
| `node.running_total` (prefix scan, barriered, exempt class 1) | `primitives/running_total.rs` | main | collar compaction and chart sorting |
| Substep repeat regions: boundary node + region body run `count` times with per-iteration scalars; nest to depth 2, inner regions clockless (LIQUID_SOLVER_SEAM_DESIGN.md D10, shipped in P5 and P6) | `crates/manifold-renderer/src/node_graph/substeps.rs:1-13` | main | the fixed Krylov pass loop |
| `GpuFft::new_r2c` / `new_c2c` / `encode` — MPSGraph, 1D, `axes = [0]` | `crates/manifold-gpu/src/metal/fft.rs:86,96,129,217` | main | extended to 3D and batched 2D (MPSGraph takes several axes) |
| Face-grid (MAC) prototype atoms: `mac_scatter_mass_momentum` (hand kernel, atomics), `mac_apply_gravity` / `mac_pressure_rows` (codegen Source), `mac_pressure_relax` (hand SOR; sweeps unrolled as parity nodes in the preset), `mac_extrapolate`, `mac_gather_advect` (RK3, codegen Pointwise) | branch `wave/live-water` (`8f3cdd23f`), not main | reference for the face layout and RK3 advect; ported, not merged |
| MPM Dam Break cost probe | `crates/manifold-renderer/tests/gpu_proofs/matter_cost_probe.rs` on `feat/gpu-mpm-build-b` | MPM branch | the race opponent, run unchanged |
| Codegen scope test and exemption classes | `docs/ADDING_PRIMITIVES.md` lines 101–140 | main | every atom below names its class |

Genuinely new: 3D/batched FFT and the cosine transform, collar classification and compaction, the six chart views, the Krylov atoms, gather-form face transfers, a native multigrid baseline (P3 only).

## 2. Decisions

**D1 — Challenger, not replacement.** Separate primitives, separate preset, separate branch off main. No edit to any file `GPU_MPM_SOLVER_DESIGN.md` owns or to `feat/gpu-mpm-build-b`. Rejected: building inside MPM's pipeline now (couples two unproven paths; Peter chose the challenger).

**D2 — Face grid (MAC) native.** Velocity on cell faces, pressure at cell centres, no node↔face bridge. Evidence (`transfer_check.py`): an exact face solve pushed through a correction-only bridge leaves 1.3e-1 of the divergence; a replace bridge leaves 9.7e-2 and keeps only 0.58 of the speed; eight bridged solves still leave 2.4e-2. Rejected: node velocity with cell pressure (Q1-P0), whose symbol collapses near the checkerboard (Astra).

**D3 — The solve.** Air removed, pressure zero at the surface (first order). Unknowns are sources λ on the one-cell air collar plus one constant c; p = G(f − Jᵀλ − mean) + c with G the whole-box pseudo-inverse (DCT-II on walled axes). Right-preconditioned GMRES on the collar system. Rejected with numbers in the findings doc: air as a real phase (Dodd–Ferrante split, 102–144% motion error at show step sizes), box FFT as the only helper (grows as N^0.7).

**D4 — The six-view surface helper ("charts").** Six signed views (±x, ±y, ±z). Weight per collar cell and view = (outward normal component)² of a Gaussian-smoothed water indicator, σ = 1.5 cells. Key = (sheet index along the view axis from water-run counting, capped at NL = 4, plane position). P = Σ_a B_aᵀ D_a^-½ Q_a D_a^-½ B_a + (2/h)(I − Σ_a B_aᵀB_a), with Q_a = sqrt(−Δ_s + q0²) applied by 2D FFT and D_a the plain cell counts per key (weighted counts made P indefinite: 48–452 passes). Measured: drops 15.0/15.8/16.5 and tower 16.8/19.5/20.5 passes at 32/64/96, versus 17.5/22.0/25.1 and 21.2/29.1/32.0 for the one-view column helper. The column helper is not shipped and not kept as a fallback.

**D5 — Fixed pass count, no readback inside the frame.** The pass count is a param (default 24, floor 12). Givens rotations live on the GPU and guard against early convergence (divisions by values under 1e-30 become zero). The true residual goes to a stats array read one frame late. Rejected: tolerance-driven stopping (a CPU sync per step).

**D6 — Collar compaction.** The Krylov vectors, the Arnoldi basis and the chart gathers run on a compacted collar list (the MLX version ran them on full N³ masks and paid 1.3 ms per pass for charts). Compaction is scan-then-place: `running_total` over a collar flag, then a place atom. The box solve stays full-grid, since the FFT needs the whole box.

**D7 — Gather-form transfers, no atomics.** Particle→face uses `cell_ranges`: each face reads the particles in its neighbouring cells and sums weights and momentum itself. MPM's scatter-with-atomics P2G is 45 of its 54.5 ms (BUG-u3ov (MPM solver budget)); this path has no atomics by construction. Particles use a PIC/FLIP blend (param `flip`, default 0.95) on the existing `FluidParticle`, so no new record. APIC is deferred.

**D8 — Loop shape.** One simulation step = one copy of the step subgraph. The Krylov passes are the substep region (the pass index is its per-iteration scalar). Steps per frame is fixed at 2 in the preset by two copies of the step subgraph. Rejected: nesting regions (the compiler forbade it when this was written; LIQUID_SOLVER_SEAM_DESIGN.md D10 amends this and depth-2 nesting is now shipped); unrolling 24 passes as nodes (the preset becomes unreadable, which is where `mac_pressure_relax`'s parity-node unroll was already heading).

**D9 — Exemption classes, named per atom.** 3D FFT, batched 2D FFT: class 1 (multi-pass cross-element transform), one MPSGraph call each. Dot products and norms for Arnoldi: class 1 (barriered reduction). Compaction place and chart sort: class 1 (scan-then-place, precedent `spawn_from_mesh`). Everything else is a barrier-free per-element atom on the codegen path with a CPU-value `gpu_tests` proof: cosine-transform permutation and twiddle, eigenvalue divide, collar source build, collar gather, chart gather-sum and spread, symbol scale, axpy, Givens update, cell classification, face gather, divergence, pressure-gradient update, particle gather and advect.

## 3. The step

Per step, in order. A number in brackets is the measured MLX cost at 64³ per step where one exists.

1. Sort particles into cells (`sort_particles_into_cells`, bins = grid cells).
2. Classify cells: water if the cell holds a particle, else air; walls are the box faces. Flag the collar (air cells with a water neighbour).
3. Gather particles to faces (D7), add gravity, zero wall faces, keep a copy for FLIP. [4.9 ms, scatter form]
4. Divergence per water cell → f.
5. Setup once per step: compact the collar (D6); compute smoothed normals, weights, sheet keys and plain counts per view; sort collar entries by key per view so every chart slot owns a contiguous range.
6. Krylov region, `passes` iterations. Each pass: charts helper (gather-sum per slot, batched 2D FFT, symbol scale, inverse, spread back plus the local term) → box solve (build the source grid from λ, DCT-II 3D, divide by the Laplacian eigenvalues, inverse, gather at the collar) → Arnoldi against the stored basis → Givens update. [11.7 ms for 12 passes with the column helper]
7. Back-substitute the small triangular system, form λ, then p over the water cells.
8. Subtract the pressure gradient on faces touching water; extrapolate one layer into air.
9. Gather faces to particles (PIC/FLIP), advect by RK3 through the face field, clamp to the box. [6.1 ms]

Memory at 64³ with 24 passes and a compacted collar of about 40k entries: the basis is 24 × 40k floats (3.8 MB). The box fields are a few 1 MB grids. Nothing scales with the particle count except the particles themselves.

## 4. Invariants & enforcement

| # | Invariant | Machine check |
|---|---|---|
| I1 | No node-grid velocity anywhere in the liquid path | `rg -n "node_vel\|NodeVelocity\|matter_" crates/manifold-renderer/src/node_graph/primitives/fft_water_*.rs` returns zero |
| I2 | No CPU readback inside a frame; stats one frame late only | `rg -n "read_back\|readback\|wait_until_completed" crates/manifold-renderer/src/node_graph/primitives/fft_water_*.rs` returns zero outside the stats atom |
| I3 | Every per-element atom on the codegen path (D9) | `every_boundary_atom_declares_its_reason` (`freeze/classify.rs:482`) passes: each exempt atom declares `boundary_reason` naming its class; each codegen atom has its `gpu_tests` value proof |
| I4 | MPM untouched (D1) | `git diff --stat origin/main...HEAD -- docs/GPU_MPM_SOLVER_DESIGN.md $(git ls-tree -r --name-only feat/gpu-mpm-build-b -- crates/manifold-renderer/src/node_graph/primitives \| rg matter_)` is empty |
| I5 | Water volume is kept | `fft_water_still_pool` proof: particle count constant, fastest particle under 1 mm/s after 2 s at 64³ |

## 5. Phasing

All phases on one branch `feat/fft-water` off main, via the slot ring. Test scope for every phase: `cargo nextest run -p manifold-renderer fft_water` plus `scripts/gpu_proofs_gate.py` (every phase touches primitive kernels). Clippy `-p manifold-renderer -p manifold-gpu`.

### P0 — Engine 3D FFT and cosine transform

- **Entry state:** `rg -n "axes = nsnumber_array" crates/manifold-gpu/src/metal/fft.rs` still shows the 1D plan at line 217.
- **Read-back:** this doc sections 1–2; `docs/ADDING_PRIMITIVES.md` whole; `fft.rs` whole; `mlx_dct.py` from the findings artifact (the Makhoul permutation and twiddle, verified against scipy to 1e-7). Restate D9 and the forbidden moves.
- **Deliverables:** `GpuFft::new_r2c_nd` / `new_c2c_nd` taking a shape and an axis list, with a batched 2D form. Atoms `node.dct_permute`, `node.dct_twiddle`, `node.idct_twiddle`, `node.idct_permute` (codegen Pointwise), `node.fft_nd` (class 1). A `gpu_tests` proof per atom against CPU values, plus a 3D DCT-II round-trip proof and a Poisson box-solve proof (random f, solve, apply the 7-point Laplacian, compare).
- **Gate:** proofs pass with max relative error under 1e-5. Report: box-solve round-trip time at 64³ and 128³ (MLX: 0.42 ms and 1.30 ms for the FFT alone, with a mirror that doubled the cost).
- **Kill check:** box solve at 64³ over 1.0 ms → stop and escalate to Peter before P1.
- **Demo:** none — L1.
- **Forbidden:** a hand WGSL cosine transform; a CPU FFT fallback; the y-mirror trick (it doubles the work).

### P1 — The collar solve on saved Dam Break problems

- **Entry state:** P0 landed on the branch. Regions nest to depth 2 on main (`MAX_REGION_DEPTH` in `crates/manifold-renderer/src/node_graph/substeps.rs`), and sibling regions still compile. Confirm two sibling regions in one graph compile, with a unit test in `substeps.rs`'s test module. If they don't, stop and escalate: D8 depends on it.
- **Read-back:** findings doc sections "The method" and "Why splashes cost more passes"; `cap3d.py` (`build_charts`, `chart_apply`) and `dambreak_mlx.py` (`charts_setup`, `charts_apply`, `pressure`, the GMRES guards). Restate D3–D6 and D8.
- **Deliverables:** atoms for step 2's collar flag, step 5, steps 6–7. Preset fragment `fft_water_pressure.json`. Fixture: the seven Dam Break problems from `dambreak_snaps.npz` (frames 0, 15, 30, 45, 60, 90, 120), converted to a binary fixture under `crates/manifold-renderer/tests/fixtures/`. Proof `fft_water_matches_reference`: at 24 passes, the true masked-equation residual per fixture is within 2× of the MLX number.
- **Gate:** proof passes. Report per fixture: passes, residual, ms per pass, ms per solve. Target 24 passes in under 10 ms at 64³ (MLX: 20.0 ms with the column helper).
- **Demo:** none — L1.
- **Forbidden:** full-grid Krylov vectors (D6); a tolerance loop; keeping the column helper "for comparison" in shipped code (the comparison lives in the Python record).

### P2 — The full step, raced against MPM

- **Entry state:** P1 landed on the branch; `feat/gpu-mpm-build-b` still has `matter_cost_probe.rs`.
- **Read-back:** section 3; the `wave/live-water` sources of `mac_gather_advect` and `mac_extrapolate` (`git show wave/live-water:<path>`), `sort_particles_into_cells.rs`. Restate D2, D7, I1–I5.
- **Deliverables:** atoms for steps 1–4 and 8–9; preset `FftWaterDamBreak.json` (64³, 4 m, walls on all six sides, pool of 3 cells plus the block x 10..29, y 0..33, z 4..60, 8 particles per cell, 353,664 particles, 2 steps per frame, 24 passes); cost probe `fft_water_cost_probe.rs` using the MPM probe's timing method; still-pool proof (I5); a momentum proof (a box of water in free fall keeps g within 1%).
- **Gate:** both proofs pass. Race table, same machine, same session: frame ms for this preset versus `matter_cost_probe` on MPM's branch, and max residual over 300 frames.
- **Demo:** L2 — 300 frames of the Dam Break through `particle_volume` and the surface pipeline, headless to PNGs, side by side with MPM's Dam Break; Peter looks. **Performer gesture:** drop the block and watch the pool settle; the gate is the still-pool number after the slosh.
- **Forbidden:** atomics in particle→face (D7); importing any `matter_*` type (I4).

### P3 — Native multigrid baseline

- **Entry state:** P2 landed on the branch.
- **Read-back:** `dambreak_mlx.py` `pressure_mg`; McAdams, Sifakis, Teran 2010. Restate that this is a benchmark opponent, not shipped code.
- **Deliverables:** multigrid-preconditioned CG as atoms (damped Jacobi 2+2, trilinear prolongation and its transpose, coarse cell water if any child is, 30 sweeps at 4³) in a test-only preset. Race on the P1 fixtures.
- **Gate:** race table at equal residual (about 1e-1, 6e-3, 2.5e-4). MLX reference: FFT 11.6/20.0/30.8 ms against multigrid 16.0/31.3/47.2 ms.
- **Demo:** none — L1.
- **Forbidden:** handicapping the baseline. Tune the smoother and coarse solve until a change stops helping, and report the tuning.

### P4 — Peter's call

- **Deliverables:** one page in BUG-wsim (FFT pressure split research) with the P2 and P3 tables and the side-by-side PNGs; a `decision` bead for Peter.
- **If it wins:** a seam brief against `GPU_MPM_SOLVER_DESIGN.md` reopening D1 and D2 for water, written with the MPM lead. **If it loses:** this doc and the branch go to `docs/archive/` with the numbers.
- **Demo:** the P2 side-by-side.

## 6. Decided — do not reopen

1. Challenger framing; MPM untouched until P4 (Peter, 2026-09-30).
2. Face-grid native; no node↔face bridge.
3. Air removed with pressure zero; no air phase.
4. Six-view helper with plain counts; no column-helper fallback.
5. Fixed pass count; no readback inside the frame.
6. Gather-form transfers; no atomics.

## 7. Deferred

| Item | Revives when |
|---|---|
| Solid objects inside the water (collar on the solid boundary; the helper divides by \|k\| there) | P4 win |
| APIC on faces (needs an affine record) | P4 win and a visible PIC/FLIP noise complaint in the P2 demo |
| Snow, sand and goo coupling through augmented MPM's volumetric split | P4 win and the MPM seam brief |
| A step-count loop around the step subgraph (steps per frame above 2; the compiler now nests regions to depth 2, LIQUID_SOLVER_SEAM_DESIGN.md D10) | the P2 demo needs more than 2 steps to stay stable |
| Subcell (ghost-fluid) surface to cut splash passes | P1 misses its pass target on the violent fixtures |
| NL = 4 sheet-cap aliasing check | P1 residual misses on the fixtures with stacked sheets |
| Vulkan and large-GPU scaling (dispatch count per pass is the limit there) | the Vulkan backend exists |
