# SWASH (Spectral Water via A Surface Helper) — a free-surface pressure solve made of FFTs, raced against the FLIP Fluids engine

<!-- index: Benchmark-gated challenger to the FLIP Fluids CPU engine for water: particles on a face (MAC) grid with pressure solved each step by a capacitance collar, whole-box cosine transforms and a six-view surface-FFT helper inside fixed-pass GMRES. Phases: engine 3D FFT/DCT, the collar solve on saved Dam Break problems, the full liquid step raced end to end against the FLIP Fluids engine, solid objects in the water, the active region, then Peter's call. MPM is secondary information; nothing MPM owns is touched. -->

**Status:** APPROVED · 2026-09-30 · P4 decided 2026-09-30: SWASH is the water solver · P0–P1 built on `feat/fft-water`; P3's step built there, with the density term · owed: the position-only density correction (128³ splash), the race table and clips (record, not gate), BUG-l2h3 (SWASH live-instrument epic) children .1 (mixed-radix lattices) and .2 (collar at its proven bound), BUG-u8io (fft-water-fusion-param-capacity), BUG-m632 (swash-residual-bar) · P3b waits on LIQUID_SOLVER_SEAM_DESIGN.md P7a (SWASH on the contract), which amends D8 and P3b.
**Evidence:** `docs/FFT_CAPACITANCE_PRESSURE_FINDINGS.md` (the research record) and the P1 measurements below.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) before starting any phase.

Peter's decisions, 2026-09-30, not reopened:

- Framing: he chose **"Challenger (Recommended)"** over replacing or merging with MPM. This solver is built as a benchmark-gated challenger. MPM continues untouched meanwhile.
- On MPM's rejection of a GPU FLIP-style solver (GPU_MPM_SOLVER_DESIGN.md D1 and D2): "That was done BEFORE we started any of our research". Those decisions are reopened only if P4 shows this solver winning, and only through a seam brief against that doc.
- P2, a GPU multigrid FLIP built as a benchmark opponent, is dropped (2026-09-30). Nothing multigrid gets built, not even for a race. SWASH races the FLIP Fluids CPU engine only; that is the bar.
- Pass/fail: SWASH wins only if it is faster than the FLIP Fluids engine end to end (ms per tick on the same Dam Break at the same resolution) with accuracy equal or better on three counts, each on the same scene: the incompressibility residual, volume drift over the 300 frames, and look (the three-column side by side in P3, which Peter judges). MPM is secondary information.
- Whitewater (spray, foam, bubbles) is out of scope here: it is BUG-imy3 (GPU whitewater on the particle-frame seam, solver-agnostic). The P3 demo says which part of any gap to FLIP Fluids is whitewater.
- Solid objects in the water are a phase before P4 (P3b): his scenes have boxes and obstacles in the water, and the FLIP Fluids engine handles them with fractional solid face weights, so equal-or-better accuracy can't be judged on an empty tank.
- Skipping empty space is a first-class lever: phase P3c.

Decided 2026-09-30:

- P4: SWASH is the liquid water solver. GPU_MPM_SOLVER_DESIGN.md D1 and D2 are reopened for water through LIQUID_SOLVER_SEAM_DESIGN.md. The race table and the three-column clips are still made, as the look check and the record, not as a gate.
- No hard resolution ceiling. Safety is the CPU extent proof at every size the scene allows, plus named refusals for what can't run: lengths the FFT can't transform (until mixed radix lands) and device memory.

## What it is on stage

Water that does not squash. A dam of water collapses, sloshes and settles into a pool that is actually still, with no fizzing surface and no slow sinking. The bar is the FLIP Fluids engine the show uses today: SWASH has to cost less per tick at the same resolution and be no worse on residual, volume drift or look, with boxes in the water as well as without. Being on the GPU is where the speed would come from; the engine runs on the CPU. MPM, at 54.5 ms per frame on the M4 Max for the Dam Break at 64³ (BUG-u3ov (MPM solver budget)), is secondary information.

## 1. Audit — what exists (verified 2026-09-30)

| Piece | Where | State | Use here |
|---|---|---|---|
| Particle record `FluidParticle` (position+radius, velocity, id; 32 bytes) | `crates/manifold-renderer/src/node_graph/fluid_particles.rs:12` | main | the liquid's particles, unchanged |
| `node.sort_particles_into_cells` → `sorted`, `cell_ranges: Array(CellRange)`, `order` | `primitives/sort_particles_into_cells.rs` | main | every particle→grid transfer is a gather over `cell_ranges` |
| `node.particle_volume` (level set from particles) + GPU surface pipeline | `primitives/particle_volume.rs`, `docs/GPU_FLUID_SURFACE_DESIGN.md` | main | acceptance-demo rendering; consumed, never edited |
| `node.running_total` (prefix scan, barriered, exempt class 1) | `primitives/running_total.rs` | main | collar compaction |
| Substep repeat regions: boundary node + region body run `count` times with per-iteration scalars; never nested | `crates/manifold-renderer/src/node_graph/substeps.rs:1-13` | main | the fixed Krylov pass loop |
| `GpuFft::new_r2c` / `new_c2c` / `encode` — MPSGraph, 1D, `axes = [0]` | `crates/manifold-gpu/src/metal/fft.rs:86,96,129,217` | main | extended to 3D and batched 2D (MPSGraph takes several axes) |
| Face-grid (MAC) prototype atoms: `mac_scatter_mass_momentum` (hand kernel, atomics), `mac_apply_gravity` / `mac_pressure_rows` (codegen Source), `mac_pressure_relax` (hand SOR; sweeps unrolled as parity nodes in the preset), `mac_extrapolate`, `mac_gather_advect` (RK3, codegen Pointwise) | branch `wave/live-water` (`8f3cdd23f`), not main | reference for the face layout and RK3 advect; ported, not merged |
| FLIP Fluids engine: `FluidWorld::step` → `FrameStats { simulation_ms, meshing_ms }`; `node.fluid_surface` in `WaterDamBreak.json` (res 64, 4 m domain) | `crates/manifold-fluids/src/lib.rs:251,696`, `native/flip_engine/` | main | the race opponent. CPU; double-precision PCG to 1e-9 relative (`_pressureSolveTolerance`), 900 iterations at most; second-order (ghost-fluid) surface; fractional solid face weights (`pressuresolver.cpp` `_solidBoundaryWeights`) |
| Native test probes that read the engine's velocity grid | `crates/manifold-fluids/native/coupling_boundary_probe.cpp` | main | pattern for the residual probe on the engine, no engine edit |
| Rigid coupling: `StepCoupling` / `SubstepExchange` | `crates/manifold-physics/src/stepping.rs:14`; owner `advance_with_coupling` at `node_graph/physics.rs:634` | main | P3b moving solids; the FLIP engine (`fluid/coupled/native.rs`) and MPM already implement it |
| MPM Dam Break cost probe | `crates/manifold-renderer/tests/gpu_proofs/matter_cost_probe.rs` | main | secondary rows, run unchanged |
| Codegen scope test and exemption classes | `docs/ADDING_PRIMITIVES.md` lines 101–140 | main | every atom below names its class |

Genuinely new: 3D/batched FFT and the cosine transform, collar classification and compaction, the six chart views, the Krylov atoms, gather-form face transfers, the face collar for solids (P3b).

## 2. Decisions

**D1 — Challenger, not replacement.** Separate primitives, separate preset, separate branch off main. No edit to any file `GPU_MPM_SOLVER_DESIGN.md` owns. `particle_volume`, `smooth_lattice` and the marching-cubes mesher are consumed, never edited. Rejected: building inside MPM's pipeline now (couples two unproven paths; Peter chose the challenger).

**D2 — Face grid (MAC) native.** Velocity on cell faces, pressure at cell centres, no node↔face bridge. Evidence (`transfer_check.py`): an exact face solve pushed through a correction-only bridge leaves 1.3e-1 of the divergence; a replace bridge leaves 9.7e-2 and keeps only 0.58 of the speed; eight bridged solves still leave 2.4e-2. Rejected: node velocity with cell pressure (Q1-P0), whose symbol collapses near the checkerboard (Astra).

**D3 — The solve.** Air removed, pressure zero at the surface (first order). Unknowns are sources λ on the one-cell air collar plus one constant c; p = G(f − Jᵀλ − mean) + c with G the whole-box pseudo-inverse (DCT-II on walled axes). Right-preconditioned GMRES on the collar system. Rejected with numbers in the findings doc: air as a real phase (Dodd–Ferrante split, 102–144% motion error at show step sizes), box FFT as the only helper (grows as N^0.7).

**D4 — The six-view surface helper ("charts").** Six signed views (±x, ±y, ±z). Weight per collar cell and view = (outward normal component)² of a smoothed water indicator. Key = (sheet index along the view axis from water-run counting, capped at NL = 4, plane position). P = Σ_a B_aᵀ D_a^-½ Q_a D_a^-½ B_a + (2/h)(I − Σ_a B_aᵀB_a), with Q_a = sqrt(−Δ_s + q0²) applied by 2D FFT and D_a the plain cell counts per key (weighted counts made P indefinite: 48–452 passes). Measured: drops 15.0/15.8/16.5 and tower 16.8/19.5/20.5 passes at 32/64/96, versus 17.5/22.0/25.1 and 21.2/29.1/32.0 for the one-view column helper. The column helper is not shipped and not kept as a fallback.
- Built: the normal is minus the central gradient of the water indicator after 3 binomial passes of `smooth_lattice` per axis (σ ≈ 1.22 cells; the reference smooths the same way). Each entry carries a share |n_a|/√D_a, applied on the gather and again on the spread, so the weight is n_a² as above. The local term is applied as (2/h)I plus a −2/h offset inside the symbol: the same operator with one fewer pass over entries.

**D5 — Fixed pass count, no readback inside the frame.** The pass count is a param (default 24, floor 12). Givens rotations live on the GPU and guard against early convergence (divisions by values under 1e-30 become zero). The true residual goes to a stats array read one frame late. Rejected: tolerance-driven stopping (a CPU sync per step).

**D6 — Collar compaction.** The Krylov vectors, the Arnoldi basis and the chart gathers run on a compacted collar list (the MLX version ran them on full N³ masks and paid 1.3 ms per pass for charts). Compaction is scan-then-place: `running_total` over a collar flag, then a place atom. The box solve stays full-grid, since the FFT needs the whole box.

**D7 — Gather-form transfers, no atomics.** Particle→face uses `cell_ranges`: each face reads the particles in its neighbouring cells and sums weights and momentum itself. MPM's scatter-with-atomics P2G is 45 of its 54.5 ms (BUG-u3ov (MPM solver budget)); this path has no atomics by construction. Particles use a PIC/FLIP blend (param `flip`, default 0.95) on the existing `FluidParticle`, so no new record. APIC is deferred.

**D8 — Loop shape.** One simulation step = one copy of the step subgraph. The Krylov passes are the substep region (the pass index is its per-iteration scalar). Steps per frame is fixed at 2 in the preset by two copies of the step subgraph. Rejected: nesting regions (the compiler forbids it and MPM's D7 owns that contract); unrolling 24 passes as nodes (the preset becomes unreadable, which is where `mac_pressure_relax`'s parity-node unroll was already heading).
- Amended by LIQUID_SOLVER_SEAM_DESIGN.md D10 (SWASH's tick loop is a substep region): the tick loop becomes a substep region with the Krylov regions nested inside it, at most two deep, once seam P5 and P6 (nested regions) are built. The two step copies go in seam P7a (SWASH on the contract).

**D9 — Exemption classes, named per atom.** 3D FFT, batched 2D FFT: class 1 (multi-pass cross-element transform), one MPSGraph call each. Dot products and norms for Arnoldi: class 1 (barriered reduction). Compaction place: class 1 (scan-then-place, precedent `spawn_from_mesh`). Everything else is a barrier-free per-element atom on the codegen path with a CPU-value `gpu_tests` proof: cosine-transform permutation and twiddle, eigenvalue divide, collar source build, collar gather, chart gather-sum and spread, symbol scale, axpy, Givens update, cell classification, face gather, divergence, pressure-gradient update, particle gather and advect.

**D10 — Krylov passes inside one region, basis owned by the boundary.** The boundary `node.krylov_basis` (`CrossFrameState`: the basis must survive the region's iterations) owns two provided arrays: `basis`, `(passes + 1) × length` f32 row-major, and `current`, the row the next pass starts from. Every Krylov vector has `length` = collar capacity + 1: collar entries first, zeros past the live count, the constant c last. The zero tail keeps every dot and update exact with no count on the GPU. The region does not opt into host syncs: its boundary ports declare `clock: None`.
- Evaluate: copy `start` (v0 = b/β) into basis row 0 and `current`; clear the small state `out`, then copy β from `seed` into its g0 slot. Offset copies are a `manifold-gpu` blit, not a shader.
- Iteration scalars: `pass` = j and `rows` = j + 1, for j < `passes`.
- Captures: `in` ← the new small state, `next_in` ← v(j+1). Late capture copies `in` to `out` and blits `next_in` into basis row j+1 and into `current`.
- Small state, f32: Hessenberg column-major `(passes + 1) × passes`, then cs, sn (`passes` each), then g (`passes + 1`).
- One pass (region body): helper on `current` → z (D11); z scattered to the grid, box solve, gathered at the collar minus c, last element Σz/n³ → w; CGS2 as `node.dot_products` against rows 0..j then `node.combine_rows` (w − V h), twice; ‖w‖ by `node.dot_products`; w/‖w‖ by `node.combine_rows` with a divisor → `next_in`; `node.krylov_givens` → `in`. Givens is one thread per state element, each re-deriving column j's rotations (at most 32), so no thread waits on another. `node.dot_products` with no vector wired returns plain sums (Σz, Σf).
- Before the region: G f by one box solve; b = G f at the collar with last element Σf/n³; β = ‖b‖; start = b/β.
- After the region: `node.krylov_solve` (back-substitution, one thread per coefficient, each running the whole solve); u = V y by `node.combine_rows`; λ = helper(u); p = (G f − G Jᵀλ + c) on water cells, reusing G f.
- Rejected: MINRES (1.5–2× the passes, findings doc); modified Gram–Schmidt (j + 1 reductions per pass, CGS2 is two); unnormalized basis rows scaled by stored norms (every reader pays a divide to save one dispatch); flexible GMRES (stores P V too, doubling the basis to skip one helper call per solve).

**D11 — Chart sums by column walk, no sort.** One thread per plane element (view, sheet, row, column) walks its column along the view axis and sums share × value over the collar entries with that sheet index. A cell is collar exactly where the collar running total steps up, and its entry is that total minus one, so no per-view sort, place or scatter exists. Shares and sheet indices are per entry, once per step (`node.chart_entries`: a short walk along each view axis counting water-run ends or starts, capped at NL − 1; the same walk counts D_a, so shares carry 1/√D_a and `chart_sums` has no count mode). Planes are M × M with M the longest lattice side; all six views and NL sheets go through one `[6·NL, M, M]` 2D cosine transform each way. A shorter side pads with zeros: the helper is a preconditioner, so padding changes its accuracy near the short walls, never the answer. The 2·NL threads of a column read the same cells, which stay in cache. Rejected: a counting sort per view (six scans and places per step to save reads the measurements have not asked for); scatter-add into planes (atomics); one transform per axis (three times the vendor FFT calls, about 0.25 ms more per pass).

## 3. The step

Per step, in order. A number in brackets is the measured MLX cost at 64³ per step where one exists.

1. Sort particles into cells (`sort_particles_into_cells`, bins = grid cells).
2. Classify cells: water if the cell holds a particle, else air; walls are the box faces. Flag the collar (air cells with a water neighbour).
3. Gather particles to faces (D7), add gravity, zero wall faces, keep a copy for FLIP. [4.9 ms, scatter form]
4. Divergence per water cell → f.
5. Setup once per step: compact the collar (D6); smooth the water indicator; per entry and view, the sheet index and the share (D4, D11).
6. Krylov region, `passes` iterations. Each pass: charts helper (gather-sum per slot, batched 2D FFT, symbol scale, inverse, spread back plus the local term) → box solve (build the source grid from λ, DCT-II 3D, divide by the Laplacian eigenvalues, inverse, gather at the collar) → Arnoldi against the stored basis → Givens update. [P1 on the GPU: 0.37 ms per pass at 64³]
7. Back-substitute the small triangular system, form λ, then p over the water cells.
8. Subtract the pressure gradient on faces touching water; extrapolate two layers into air.
9. Density solve on the same collar (setup reused), 8 passes: the right-hand side is `node.density_source`'s crowding target (below). Its gradient is subtracted from the projected faces into a separate `advect` grid, extended two layers.
10. Gather faces to particles (PIC/FLIP) from the projected faces, advect by RK3 through `advect`, clamp to the box. [6.1 ms]

The density solve exists because particles bunch as FLIP moves them, and a divergence-free velocity field does nothing about it: with no correction the 64³ Dam Break's water sank 22% by frame 300. Each water cell asks for −rate·e, with e = count/8 − 1: two-sided inside the water, spread-only at the surface, where a part-full cell is not sparse. The correction moves particles and never becomes their velocity. Folded into the main solve instead, FLIP kept the push as speed, and the 128³ splash ran p99 6.90 m/s against the engine's 4.73. The share removed per step (rate × step dt) is 1 (`SPREAD_PER_STEP`).

Memory: the collar capacity is 8n² entries (32,768 at 64³, 131,072 at 128³; the largest Dam Break collars are 18,655 and 94,154). The pressure basis is 25 rows × (capacity + 1) floats at 24 passes (3.3 MB at 64³, 13.1 MB at 128³); the density basis is 9 rows. The box fields are a few n³ grids. Nothing scales with the particle count except the particles themselves.

## 4. Invariants & enforcement

| # | Invariant | Machine check |
|---|---|---|
| I1 | No node-grid velocity anywhere in the liquid path | `rg -n "node_vel\|NodeVelocity\|matter_" crates/manifold-renderer/src/node_graph/primitives -g "{cosine_,fft_3d,collar_,chart_,krylov_,dot_products,combine_rows,select_flagged}*"` returns zero |
| I2 | No CPU readback inside a frame; stats one frame late only | `rg -n "read_back\|readback\|wait_until_completed"` over the I1 files returns zero outside the stats atom |
| I3 | Every per-element atom on the codegen path (D9) | `every_boundary_atom_declares_its_reason` (`freeze/classify.rs:482`) passes: each exempt atom declares `boundary_reason` naming its class; each codegen atom has its `gpu_tests` value proof |
| I4 | MPM untouched (D1) | `git diff --stat origin/main...HEAD -- docs/GPU_MPM_SOLVER_DESIGN.md $(git ls-tree -r --name-only origin/main -- crates/manifold-renderer/src/node_graph \| rg matter)` is empty |
| I5 | Water volume is kept | `fft_water_still_pool` proof: particle count constant, fastest particle under 1 mm/s after 2 s at 64³ |
| I6 | Every buffer covers every dispatch before the GPU sees it | a CPU test per preset and size, the pattern of `fft_water_pressure_arrays_cover_every_dispatch` (`primitives/swash_extent_tests.rs`), over every lattice in its `LATTICES` (16–256); dynamic dispatch counts are clamped to capacity on the CPU |
| I7 | The density correction moves particles and never becomes velocity | `swash_faces_to_particles_blends_flip_and_moves_by_rk3` feeds three different grids and checks velocity against `faces`/`old` only, position against `advect` only |
| I8 | The water is not compressed | the race probe's share of particles past 8 per cell, reported beside the meshed volume; the meshed volume measures the surface (it loses thin sheets), this measures the water |

## 5. Phasing

All phases on one branch `feat/fft-water` off main, via the slot ring. Order: P0 → P1 → P3 → P3b (solids) → P3c (active region) → P4. Test scope for every phase: `cargo nextest run -p manifold-renderer fft_water` plus `scripts/gpu_proofs_gate.py` (every phase touches primitive kernels). Clippy `-p manifold-renderer -p manifold-gpu`.

GPU safety, every phase: an out-of-bounds write can freeze the whole Mac (naga's SPIR-V bounds policy is unchecked, and `arrayLength` ignores a binding's offset). Before the first GPU run of a new atom or preset at any size, a CPU test proves every buffer covers the extent its dispatch and indexing reach (I6). Step 64³ before 128³. Other solvers' water scenes (MPM, and the shared liquid surface) run at 64³ only until BUG-gwe4 (staged GPU check of the water presets above res 64) passes.

### P0 — Engine 3D FFT and cosine transform

- **Entry state:** `rg -n "axes = nsnumber_array" crates/manifold-gpu/src/metal/fft.rs` still shows the 1D plan at line 217.
- **Read-back:** this doc sections 1–2; `docs/ADDING_PRIMITIVES.md` whole; `fft.rs` whole; `mlx_dct.py` from the findings artifact (the Makhoul permutation and twiddle, verified against scipy to 1e-7). Restate D9 and the forbidden moves.
- **Deliverables:** `GpuFft::new_nd(device, kind, shape, axes)` with `FftKind::HermiteanToReal` (inverse, scaled by 1/volume). Atoms `node.cosine_reorder` (both directions), `node.cosine_spectrum` (half spectrum → DCT-II, four gathers), `node.cosine_half_spectrum` (DCT-II → half spectrum, eight gathers), `node.cosine_poisson_divide` (codegen); `node.fft_3d`, `node.inverse_fft_3d` (class 1). The 3D cosine transform is one real 3D FFT between a reorder and a twiddle, three dispatches each way. Proofs in `primitives/swash_tests.rs`: DCT-II against a direct f64 reference, round trip, and a box solve checked by applying the walled Laplacian back.
- **Gate:** proofs pass with max relative error under 1e-5. Report: box-solve time at 64³ and 128³. **Measured (M4 Max, 50 solves in one command buffer): 0.41–0.46 ms at 64³, 1.18–1.29 ms at 128³ for the whole solve** (MLX took 0.42 and 1.30 ms for the FFT round trip alone).
- **Kill check:** box solve at 64³ over 1.0 ms → stop and escalate to Peter before P1.
- **Demo:** none — L1.
- **Forbidden:** a hand WGSL cosine transform; a CPU FFT fallback; the y-mirror trick (it doubles the work).

### P1 — The collar solve on saved Dam Break problems

- **Entry state:** P0 landed on the branch. `rg -n "never nested" crates/manifold-renderer/src/node_graph/substeps.rs` still matches. Confirm two sibling regions in one graph compile, with a unit test in `substeps.rs`'s test module. If they don't, stop and escalate: D8 depends on it.
- **Read-back:** findings doc sections "The method" and "Why splashes cost more passes"; `cap3d.py` (`build_charts`, `chart_apply`) and `dambreak_mlx.py` (`charts_setup`, `charts_apply`, `pressure`, the GMRES guards). Restate D3–D6, D8, D10 and D11.
- **Deliverables:**
  - Setup atoms: `node.collar_cells` (air cells with a water neighbour, as u32 flags); `node.select_flagged` (entry → cell by binary search of a running total, sentinel past the total); `node.chart_entries` (share and sheet index per entry and view, D counted by the same walk). Reused: `node.running_total`, `node.smooth_lattice`.
  - Box-solve atoms: `node.collar_source` (entry values onto the grid) and `node.collar_gather` (grid values at the entries minus c, last element sum/volume).
  - Helper atoms: `node.chart_sums` (D11), `node.cosine_surface_scale` (× sqrt(λ₁ + λ₂ + q0²) plus an offset) and `node.chart_spread` (planes back to entries, plus the local term). The cosine atoms and `node.fft_3d` gain a plane mode: the last two axes transformed, the first batched.
  - Krylov (D10): `node.krylov_basis`, `node.dot_products`, `node.combine_rows`, `node.krylov_givens`, `node.krylov_solve`, and an offset buffer copy on `GpuEncoder`.
  - Preset fragment `fft_water_pressure.json`, with a CPU proof that every array covers its dispatch at 64³ and 128³ for 12–32 passes.
  - Fixture: the seven Dam Break problems from `dambreak_snaps.npz` (frames 0, 15, 30, 45, 60, 90, 120) as a zstd binary under `crates/manifold-renderer/tests/fixtures/` (water bits, f32 divergence). The 128³ problems are these refined 2× in the test, each cell becoming eight, for the pass-count and timing trend; no 128³ bytes are stored.
  - Proof `fft_water_matches_reference`: at 24 passes, the true masked-equation residual per fixture is within 2× of the f64 reference, recomputed from the same fixture by `scripts/swash_reference.py` and pinned in the test. Fused-versus-unfused proof for the fusable pairs (`cosine_spectrum` → `cosine_poisson_divide`, `cosine_spectrum` → `cosine_surface_scale`), run through the frozen and unfrozen preset: **BLOCKED** on BUG-u8io (fft-water-fusion-param-capacity), since `cosine_spectrum`'s param-sized output has no capacity the fusion planner accepts; `fft_water_pressure_has_no_fused_region` guards the gap.
- **Gate:** proofs pass. Report per fixture, at 64³ and 128³: residual, ms per solve, and ms per pass split into helper, box solve and Krylov. Target 24 passes in under 10 ms at 64³ (MLX: 20.0 ms with the column helper).
- **Measured (M4 Max, quiet GPU, one solve per command buffer after 20 warm-up solves, 24 passes):** residual against the f64 reference 1.00–1.22× on all seven problems at 64³ and 1.00× at 128³. 9.2–10.2 ms per solve at 64³ (median 9.5; the splash frames take 10.1–10.2), 50–53 ms at 128³; CPU encode 5.6 ms at both sizes. Per pass at 64³: helper 0.120, box solve 0.141, Krylov 0.106 ms, and 6.4% of the solve outside the loop. At 128³: 0.643, 1.110 and 0.210 ms, 8.9% outside. The target is met with no margin. The 0.43 ms box solve this section used to plan with was wall time including CPU encode; on the GPU it is 0.14 ms. Levers left: fusion (BUG-u8io), the Krylov dispatch count at 64³, and the box solve at 128³ (half the time). Fewer passes costs accuracy:

  | Size | Passes | Median residual | Worst | ms per solve |
  |---|---|---|---|---|
  | 64³ | 12 | 7.14e-2 | 2.12e-1 | 4.72 |
  | 64³ | 16 | 2.19e-2 | 7.25e-2 | 6.70 |
  | 64³ | 24 | 1.15e-3 | 8.22e-3 | 9.39 |
  | 64³ | 32 | 5.70e-5 | 8.30e-4 | 12.45 |
  | 128³ | 12 | 3.26e-1 | 7.97e-1 | 27.1 |
  | 128³ | 16 | 1.37e-1 | 3.20e-1 | 38.7 |
  | 128³ | 24 | 1.34e-2 | 4.49e-2 | 51.5 |
  | 128³ | 32 | 2.34e-3 | 7.25e-3 | 75.1 |

- **Deviations:** the reference is `scripts/swash_reference.py` (numpy, f64), because the findings artifact lacks the MLX scripts. It agrees with the MLX record at every pass count (7.14e-2 against 7.2e-2 at 12, 2.19e-2 against 2.4e-2 at 16, 1.15e-3 against 1.1e-3 at 24, 5.3e-5 against 4.2e-5 at 32). D is counted in `node.chart_entries`, not by a count mode on `chart_sums` (D11). Report only: weighted counts measured better here (24-pass median 7.4e-4 against 1.15e-3); D4 keeps plain counts.
- **Demo:** none — L1.
- **Forbidden:** full-grid Krylov vectors (D6); a tolerance loop; keeping the column helper "for comparison" in shipped code (the comparison lives in the Python record).

### P2 — dropped

Peter, 2026-09-30: no GPU multigrid FLIP is built, not even as a benchmark. The race is against the FLIP Fluids engine only (P3).

### P3 — The full step, raced against the FLIP Fluids engine

- **Entry state:** P1 on the branch. Merge `origin/main` first (GPU MPM and `matter_cost_probe.rs` are on main); after the merge `KRYLOV_BASIS_PORTS` declares `clock: None` (D10). `rg -n "simulation_ms" crates/manifold-fluids/src/lib.rs` still shows the engine's per-frame timing.
- **Read-back:** section 3; the `wave/live-water` sources of `mac_gather_advect` and `mac_extrapolate` (`git show wave/live-water:<path>`); `sort_particles_into_cells.rs`; `WaterDamBreak.json` and how `node.fluid_surface` seeds its Dam Break (tank, block, fill height); `pressuresolver.cpp` `_calculateMatrixCoefficientsThread` (the engine's surface condition, so the residual rows are read correctly). Restate D2, D7, I1–I6.
- **Deliverables:**
  - Atoms for steps 1–4 and 8–9.
  - Preset `FftWaterDamBreak.json`: the engine's Dam Break scene without its obstacle box (4 m domain, walls on all six sides, the `initial_column` block and the 0.16 m fill; where it differs from the P1 fixture's scene, the engine's wins, since accuracy is judged on the same scene), 8 particles per cell, 2 steps per frame, 24 passes, at 64³; and its 128³ twin. The shipped Dam Break has a box in the water (`obstacle_transform`, 0.6 × 1.16 × 0.85 m at (0.35, 0.58, −0.1)); SWASH has no solids until P3b, so P3's race runs the engine with the obstacle unwired and P3b races the scene as shipped.
  - Cost probe `fft_water_cost_probe.rs` on the MPM probe's timing method: GPU ms and CPU encode ms per tick, step and surface separately.
  - Engine probe: the same Dam Break through `FluidWorld::step` at the same resolution and frames: wall clock per tick, the stage split from a sampling profile (macOS `sample`), settings stated, whitewater off for the race and on for the look. `meshing_ms` double-counts and is never used.
  - Accuracy oracles, judged by outcome and measured the same way on both solvers. Divergence: the face velocity field after projection, on the shared 64³ (or 128³) grid, one CPU divergence operator for both; RMS and max over each solver's water cells, per tick. The engine's field is read by a native test probe beside `coupling_boundary_probe.cpp` (a MANIFOLD-authored probe, one line in `PROVENANCE.md`, no engine edit). Volume: the signed volume inside each solver's surface mesh per frame, relative to frame 0. Each solver's internal residual is reported as information only.
  - Occupancy report, per frame over the 300-frame Dam Break: the fraction of the box that is water, the fraction of 8³ blocks holding water, and the height of the water's bounding box in cells. These size P3c. Also the collar size per frame against its capacity (8n²): a collar past capacity drops entries safely but solves the wrong problem, so the stats count overflow and the proofs require zero; splashes grow the collar past the P1 fixtures' 18,655, and the Krylov cost scales with capacity, so capacity is set from the measured maximum.
  - Still-pool proof (I5); momentum proof (a box of water in free fall keeps g within 1%).
- **Gate:** both proofs pass. Race table, same machine, same session, at 64³ and 128³:

  | Row | SWASH | FLIP Fluids engine |
  |---|---|---|
  | ms per tick, end to end (primary) | step + surface, GPU | wall clock around `FluidWorld::step`, whitewater off, settings stated (Detail, substeps) |
  | CPU encode ms per frame (content thread) | both steps + surface | — |
  | of which surface | surface pipeline | the mesher stage from the sampling profile; never `meshing_ms`, which double-counts |
  | divergence after projection, median and max over 300 frames | the oracle above | the oracle above |
  | volume drift: max \|V/V₀ − 1\| over 300 frames, and at frame 300 | surface mesh | surface mesh |
  | whitewater cost (information) | none | `simulation_ms` with whitewater on minus off |

  Secondary rows: MPM `matter_cost_probe` frame ms at 64³ (128³ waits for BUG-gwe4).
- **CPU encode:** P1 spends 5.6 ms of CPU per solve on encoding, and a frame runs two steps inside the content thread's 16.6 ms. Every P3 table reports encode ms per frame next to GPU ms, with the lever named (fewer dispatches per pass, a pre-encoded or indirect command buffer for the pass loop, or caching the encoded pass). None is built in P3 unless it is cheap and clearly the root cause.
- **Open (BUG-m632 (swash-residual-bar), Peter's call):** the engine solves in double to 1e-9 relative when it converges within 900 iterations. SWASH is single precision (f32) and reaches about 1e-3 at 24 passes and 6e-5 at 32. f32 keeps about 7 digits, so no f32 solve can even store a pressure good to 1e-9: an internal residual count read literally is lost before the race starts. The lead recommends judging accuracy by outcome (the divergence and volume rows above), measured the same way on both.
- **Engine reference numbers (inventory worker, 64³ Dam Break, 343k particles, Detail 1, whitewater on):** 392–407 ms median per tick: mesher 175–184 (55–65 at Detail 0), whitewater 42–55, pressure about 52, advection about 57, level set about 43. Taken with other agents loading the machine; the race re-measures quiet.
- **Whitewater adapter layouts (for BUG-imy3 (GPU whitewater on the particle-frame seam), which reuses the engine's C++ particle lifecycle fed from a MAC grid and a liquid SDF through shared memory):**
  - Faces. The engine keeps three float arrays, U at (n+1)·n·n, V at n·(n+1)·n, W at n·n·(n+1), each indexed i + width·(j + height·k). SWASH keeps one `FaceSample` array (32 bytes: velocity vec4, weight vec4) over (n+1)³ padded cells, indexed i + (n+1)·(j + (n+1)·k). Face positions agree (U(i, j, k) sits at (i, j+½, k+½)·h on both), and both are in m/s. The strides and the interleave differ, so the adapter is one gather pass per component, not an alias. SWASH's grid starts at `lattice_min`; the engine's at its own origin.
  - Liquid field. The engine's `ParticleLevelSet` is n³ cell-centred at (i+½)·h, negative inside, a true distance capped at its max distance. SWASH's `particle_volume` (once feat/fluid-surface-look is on main) is nodal on the refined surface lattice: the distance to the nearest blob, negative inside, capped at 0.1 of a bin outside. The adapter resamples to cell centres; redistancing is needed only past the cap, where whitewater's crest and curvature tests would read a clamped value.
- **Kill check:** SWASH's volume drift over 2× the engine's at 64³ → stop and escalate before P3b: speed levers can't buy that back.
- **Demo:** L2 — three columns, 300 frames each, headless to PNGs: SWASH through `particle_volume` and the surface pipeline; the FLIP Fluids engine's own Dam Break from `WaterDamBreak.json` (CPU-meshed, whitewater as shipped) at the same resolution and frames; MPM's Dam Break at 64³. Peter's exports `~/Downloads/waterExportTestVert80Res.mp4` and `~/Downloads/waterBoxTest64.mp4` are the look reference: read them, never write there. The demo says which part of any look gap to the engine is whitewater (BUG-imy3). **Performer gesture:** drop the block and watch the pool settle; the gate is the still-pool number after the slosh.
- **Forbidden:** atomics in particle→face (D7); importing any `matter_*` type (I4); timing either solver on a contended GPU or CPU (re-run outliers); editing the engine to measure it.

### P3b — Solid objects in the water

- **Amended by LIQUID_SOLVER_SEAM_DESIGN.md D7 (bodies inside the pressure solve) and D12 (solids through the shared distance lattice):** body mass goes inside the pressure solve, since FLIP measured 16–24× body energy growth with the body held fixed during the solve; solids come from the shared distance lattice; the analytic box clip below is rejected. P3b starts only after seam P7a (SWASH on the contract) has landed. Where this section and the seam doc disagree, the seam doc wins.
- **Why here:** Peter's scenes have boxes and obstacles in the water. The FLIP Fluids engine handles them with fractional solid face weights (`pressuresolver.cpp` `_solidBoundaryWeights`: each face carries the fraction of it open to fluid), so equal-or-better accuracy can't be judged on an empty tank. This phase comes before P4.
- **Entry state:** P3 landed. `rg -n "_solidBoundaryWeights" crates/manifold-fluids/native/flip_engine/pressuresolver.cpp` and `rg -n "pub trait StepCoupling" crates/manifold-physics/src/stepping.rs` still match.
- **Read-back:** `pressuresolver.cpp` `_calculateMatrixCoefficientsThread`, `_solidBoundaryWeights` and `computeSolidPressureImpulse`; `GPU_MPM_SOLVER_DESIGN.md` D12 and section 5 (the coupling protocol); `stepping.rs` whole; `advance_with_coupling`. Restate D3, the face collar below and I6.
- **The face collar (the collar on the solid boundary).** The engine's operator is a weighted Laplacian: face f carries its open fraction w_f in [0, 1]. The box transform inverts only the unweighted one. At a water cell i the two differ by Σ over its faces with w_f < 1 of (1 − w_f)(q_j − q_i)/h², where j is the cell across f. So each face with w_f < 1 that touches a water cell gets one unknown ν_f, a dipole source: +ν_f in the water cell, −ν_f in the cell across (for a water–water face both sides need it; in a solid cell it is harmless; in an air collar cell λ absorbs it). Its condition row is ν_f − (1 − w_f)(q_j − q_i)/h² = 0. With the air collar rows and the constant row unchanged, q on water satisfies the engine's weighted equation exactly, by the uniqueness argument of P3c. A face into a fully solid cell has w_f = 0. The solid's velocity enters f the way the engine's does.
- **How the helper treats a solid face:** face unknowns skip the charts. For the infinite lattice, a unit dipole across a face gives q_j − q_i = h²/3 (the Green's function drops by h²/6 per step), so the ν block's diagonal is 1 − (1 − w_f)/3, between 2/3 and 1. The helper divides each face entry by it. The block is well conditioned, so the pass count should barely move; the Python record checks that before any atom exists.
- **Deliverables:**
  - The face collar in `scripts/swash_reference.py` on a submerged-box fixture (the P1 Dam Break with the shipped Dam Break's obstacle box): exactness against a direct weighted solve, and passes to the empty tank's residual with the diagonal helper. This comes first.
  - Atoms: face weights and solid face velocities from Box3D box poses (analytic clip of each face square against each box, codegen); face-collar flag, compaction and gather/source atoms, or a face mode on the collar atoms if the audit shows one wire away; the diagonal helper scale (codegen); the pressure impulse and torque per body as a class 1 reduction, no atomics.
  - Moving solids through the existing exchange: SWASH implements `StepCoupling` / `SubstepExchange` (`manifold-physics`, no `matter_*` import). `exchange` reads the rigid poses and velocities from `PhysicsWorld`, encodes the step, and applies last tick's reaction through `PhysicsWorld::apply_impulses`; the reaction crosses back through a fenced readback one tick late, as MPM D12 does. `finish` captures the paired rigid state.
  - I6 CPU size proof with the face collar at its capacity (the solids' total surface area in faces, clamped).
- **Gate:**
  - Python: exact against the direct weighted solve to 1e-10; passes to the empty tank's residual up at most 25%.
  - Hydrostatics: a fixed box fully under a still pool feels a lift of ρgV within 2%, SWASH and the engine both reported.
  - Floating: a Box3D box at half water density dropped into the pool settles at its analytic draft within one cell, and within one cell of the engine's.
  - The P3 race rows on the Dam Break as shipped, obstacle box included, SWASH against the engine at 64³ and 128³.
  - Body force against passes (Peter, 2026-09-30). A body's boundary faces join the collar, where the leftover error lives, so its pressure is the least-converged part of the answer. For (a) a submerged fixed box and (b) a floating Box3D box, record the net pressure force and torque at 6, 8, 12, 16, 24 and 32 passes over a few hundred frames: the error against the 32-pass value (or the f64 reference) and the frame-to-frame force jitter at each count. Report the smallest pass count whose force is within 1% of converged with no visible jitter in the coupled box's motion; that sets the live pass count when bodies are present.
- **Kill check:** passes to the empty tank's residual up more than 50% with the box, in Python or on the GPU → stop and escalate with the numbers; the diagonal helper is the part to rethink.
- **Demo:** L2 — the Dam Break as shipped, obstacle box included, SWASH beside the engine, 300 frames headless; Peter looks. **Performer gesture:** the wave breaks around the box, then the floating box rides the slosh.
- **Forbidden:** whole-cell solids (the engine uses fractions, so the scenes would differ); a CPU wait for the reaction inside the tick; atomics in the per-body reduction; editing the engine.

### P3c — Active region: work only where water is

- **Entry state:** P3 landed, with its occupancy report. The report gives this phase's ceiling: the water fraction and occupied-block fraction bound what skipping air can save.
- **Read-back:** the P3 occupancy report; `substeps.rs` and the planner's array sizing (arrays are pinned at exact sizes; a buffer is reused only at an identical byte size); how the stats array reaches the CPU one tick late (D5); `GpuFft` plan creation cost. Restate D5, D6, I6 and the derivation below.
- **Deliverables, part (a) — occupied blocks:** the particle, transfer and grid steps (section 3 steps 1–4, 8–9) dispatch only over occupied 8³ blocks: a block counts if it or a neighbour held water last tick, so the collar and the one-layer extrapolation stay inside. The CPU sizes each dispatch from last tick's occupied-block count or water bounds, read one tick late through shared memory with no wait inside the region. Buffers stay allocated at full-box size, so nothing reallocates per tick; every dynamic dispatch count is clamped to capacity on the CPU before encoding.
- **Deliverables, part (b) — the clipped box solve:** the box solve runs on B′, the water's bounds from last tick padded by one 8³ block (more than 4× the fastest per-tick travel the Dam Break needs: about 1.7 cells per frame at 6.3 m/s) plus the collar, clamped to the real box and each side snapped up to an FFT-friendly size from a short ladder (multiples of 8 at 64³). A small cache holds one FFT plan per box shape: the full box and the first tick's shape are built at load, a new shape grows the box at once, and a shrink waits until the smaller shape has held for 30 ticks so the plan doesn't flap. The helper's planes use B′'s longest side.
- **Why the answer doesn't change:** let W be the water cells and p* the solution of the masked problem (the step's equation on W, zero on air, walled at the real box faces). Take any box B′ that holds W, the collar and, after P3b, every cell across a face unknown, walled on all its faces. Set q = p* on W and 0 elsewhere in B′. At a water cell every neighbour is water, collar, or a real wall; if water touches a real wall, B′ reaches that wall, so the wall is the same one. Hence Δ_B′ q = f on W. On a non-collar air cell every neighbour is air, so Δ_B′ q = 0. So f − Δ_B′ q lives on the collar, its total is zero (a walled box's Laplacian sums to zero), and q = G′(f − Jᵀλ) + c for that collar source and c = the mean of q. Conversely, any solution of the collar system on B′ solves the masked problem, which has one solution because every water body touches air. So p on water is p* whatever B′ is. The top boundary above the highest water is the case that matters: the artificial ceiling sits in air past the collar, and no water cell sees it. What does change is the collar operator (G′ in place of G), and with it the pass count at a fixed residual. That is what the kill check guards.
- **Water outside B′:** a tick whose water leaves last tick's padded bounds is wrong. The classify step counts water cells outside B′ into the stats array; any nonzero count makes the next tick use the full box, and the proof requires zero such ticks over the 300-frame Dam Break.
- **Gate:** on the P1 fixtures clipped to their bounds, the residual at 24 passes is within 2× of the full box. Over the 300-frame Dam Break: zero ticks with water outside B′; the P3 race table re-run with the active region, at 64³ and 128³, with the per-frame B′ size and plan-cache misses.
- **Kill check:** the clipped residual at 24 passes over 2× the full box's on any fixture, or the passes to reach the full box's residual up more than 25% → stop and escalate: the answer is provably the same, so the loss is the preconditioner on the smaller box. Also a plan-cache miss costing more than a frame (16 ms) on the Dam Break → report before shipping the cache as is.
- **Demo:** none — L1, plus the re-run race table.
- **Forbidden:** a CPU wait for this tick's bounds; reallocating buffers per tick; a dispatch count the CPU hasn't clamped to capacity; dropping the pad or the collar from B′.

### P4 — Decided 2026-09-30: SWASH is the water solver

- The water presets move from the FLIP Fluids engine to SWASH through LIQUID_SOLVER_SEAM_DESIGN.md, which reopens `GPU_MPM_SOLVER_DESIGN.md` D1 and D2 for water.
- Still made, as the record and the look check, not as a gate: the P3 race table and three-column clips, and the P3b and P3c tables as those phases land, on one page in BUG-wsim (FFT pressure split research).

## 6. Decided — do not reopen

1. Challenger framing; MPM untouched until P4 (Peter, 2026-09-30).
2. Face-grid native; no node↔face bridge.
3. Air removed with pressure zero; no air phase.
4. Six-view helper with plain counts; no column-helper fallback.
5. Fixed pass count; no readback inside the frame.
6. Gather-form transfers; no atomics.
7. The opponent is the FLIP Fluids engine: faster per tick, with residual, volume drift and look equal or better; MPM is information only (Peter, 2026-09-30).
8. No multigrid FLIP, not even as a benchmark (Peter, 2026-09-30).

## 7. Deferred

| Item | Revives when |
|---|---|
| APIC on faces (needs an affine record) | P4 win and a visible PIC/FLIP noise complaint in the P3 demo |
| Snow, sand and goo coupling through augmented MPM's volumetric split | P4 win and the MPM seam brief |
| Nested substep regions (steps per frame above 2) | the P3 demo needs more than 2 steps to stay stable |
| Subcell (ghost-fluid) surface, as the engine has, to cut splash passes and match its surface | the P3 look or residual rows lose to the engine at the surface |
| NL = 4 sheet-cap aliasing check | a residual miss on fixtures with stacked sheets |
| Vulkan and large-GPU scaling (dispatch count per pass is the limit there) | the Vulkan backend exists |
| Wrap-around (torus) axes: a per-axis wrap switch on the transform atoms (plain FFT, no reorder or twiddle; eigenvalue 4 sin²(πk/N)), wrapping particle transfers and chart views. Endless ocean = wrap x and z over a floor; a full torus needs force fields in place of gravity, or water accelerates forever. Peter asked for it 2026-09-30 | P3 race done, win or lose |
