# GPU FLIP sparse blocks — occupied 8³ tiles for the dense lattice passes, then the pressure solve

<!-- index: Design for BUG-jyot (GPU FLIP at 128 for 60 fps), Option C: the GPU FLIP step classifies 8^3 tiles on the GPU each tick (ring distance from particle-holding tiles), runs the dense lattice passes and later the multigrid pressure solve over tile lists by indirect dispatch, keeps every skipped buffer at the dense pass's value, and proves bitwise equality against the dense path on the 64 Dam Break. -->

**Status:** IN PROGRESS · 2026-10-03 · Fable 5.1 (lane) · BUG-jyot (GPU FLIP at 128 for 60 fps), Option C; Phase 1 is BUG-t7i2 (sparse blocks phase 1). Built, bitwise: tile table, cell passes over C, extend dense (D-4), solve (D-8), body passes (D-9); 2c transfers value-level. Owed: all_tiles-forced run, Phase 2 measure. Pointer: GPU_FLIP_STRUCTURAL_OPTIONS.md section 6 (Option C — sparse tiles).
**Execution contract:** no size caps, no quality caps, one step per frame, a capped solve is reported in stats never a node error, block list decided on the GPU each tick, never read back, every reader of a skipped block sees a defined value, all GPU through `manifold-gpu`, no new shared state, output bitwise equal to dense.

The occupied-block rule in the contract comes from BUG-l2h3 (SWASH to a live instrument) item .10, closed 2026-10-02 as deferred to BUG-jyot.

Companions: GPU_FLIP_PRESSURE_SOLVE.md (the solver as built), GPU_WHITEWATER_DESIGN.md (reads the step's faces), DECOMPOSING_GENERATORS.md section 1.2 (Specialised solvers are stage nodes).

## 0. The finding that shaped this design

The options doc estimated 8³ tiles active at 0.25–0.36 of the lattice at 64 and named one uncertainty: the extend sweeps need a `band_layers`-wide halo (14 cells at 64, 23 at 128), and if that halo lights most tiles the ratio is nearer 0.7. The CPU count on the dumped Dam Break pressure problems (appendix) answers it: the band-wide halo lights 0.64–1.00 of tiles (mean 0.83 at 64, 0.76 at 128). A one-cell halo lights 0.31–0.70 at 64 (mean 0.46) and 0.25–0.49 at 128 (mean 0.34); the Dam Break sloshes thin water across the whole tank from frame 45 on, so the sparse win on this scene is about 2.2× on the cell passes at 64, 2.9× at 128, not the 3–4× estimated.

So the design does not use one halo. Each pass gets the smallest tile set its stencil allows (section 3, the reach rule), and the extend stays dense (D-4: its sources are the walls and gravity as well as the liquid, so a ringed layer cannot be bitwise). Whitewater samples face velocity anywhere in the air (`sample_faces_at_particles_body.wgsl:82`, corner weights at its own spray particles), so a narrower extend halo would change what the audience sees; it stays deferred (section 9).

## 1. Audit — verified 2026-10-02 at 955e47752

| Piece | Where | What matters here |
|---|---|---|
| The step | `crates/manifold-renderer/src/node_graph/primitives/gpu_flip_step.rs` `encode` :526 | passes in order: `particle_distance` → `particles_to_faces` → extend old → `face_gravity` → solids → `water_from_phi` → pockets → `divergence` → pressure → `subtract_pressure` → `constrain_solid_faces` ×2 → extend new → `density_source` → density solve → project → extend → `faces_to_particles` |
| Dispatch shape | `gpu_flip_step.rs` `groups` :311, `dispatch_pass` :142 | every lattice pass `@workgroup_size(256)`, one thread per cell (`cell_total()`) or face record (`face_total()`); `groups(threads) = [ceil(threads/256).max(1),1,1]` |
| Extend | `gpu_flip_step.rs` `extend` :326, `band: band_layers(travel).max(FACE_VALID_LAYERS)` :1050 | `band_layers(travel) = ceil(√3·travel)+3`: 14 at 64, 23 at 128; three runs per step (old, new, spread) |
| Indirect dispatch | `manifold-gpu/src/metal/encoder.rs` `dispatch_compute_indirect` ~:502; `gpu_flip_step.rs` :378 (pocket gate, `POCKET_GATE_WORDS = 11`) | precedent: `pocket_round` is a 1-thread kernel that writes the gate triples; the solver's `Gate`/`armed`/`dispatch_compute_gated` replay stop-gated dispatches |
| Lattice buffers | `gpu_flip_step.rs` `LatticeBuffers` :219, `allocate` :251, `cell_bytes` :81, `scratch_bytes` :90 | `water, phi, rhs, pressure, corners, a, b, f, s, v, pocket, pocket_gate, pocket_label, pocket_sum, solve_water`; face grid `(n+1)³` `FaceSample{face_velocity: vec4, face_weight: vec4}`, record p holds the low faces of cell p; wall faces (p[a]==0 or n) closed: velocity 0, weight 1 |
| Sort | shared `ParticleSorter`, bins = cells, `ranges: array<CellRange{start,count}>` | live particle = `position_radius.w > 0`; the tile classifier reads `ranges` only |
| Kernels | `shaders/gpu_flip_step.wgsl` :1213–1676 (`subtract_pressure`, `constrain_solid_faces`, `density_source`, `sample`, `faces_to_particles`) | `density_source` writes 0 for air cells; `sample` excludes weight-0 faces; `faces_to_particles` reads the new grid at q0 and three RK3 stages up to `travel` cells away |
| Pressure solver | `gpu_flip_pressure.rs` (964 lines): `prepare`, `solve`, `tally`, `init_main`, `v_cycle`, `smooth`, `dot_finalize`, `check`, `partial_count(n) = groups(cells(n))[0]` | `init_main` reads `rhs[idx]` only for water cells with open faces (`is_water` reads `water`); `level_lattices` halves to ≤4; `MAX_ITERATIONS = 64`; `partial_count` is a CPU constant passed as `Params.color` |
| Extent | `extent.rs` `fn gpu_flip_step` :1202 | `provide("faces")`, holds sort ranges + faces + `pressure_scratch_bytes` + `step_scratch_bytes(cells, slots)`; CPU proofs `gpu_flip_*_cover_every_dispatch` |
| Stats | `liquid_stats.rs`; `LIQUID_STATS_WORDS = 17`, `SOLVER_WORDS = 7` (words 10–16, `tally = 2*capacity*4` in `capped`) | call sites: conformance.rs:21,415; extent.rs:52,1147,1166,1192; gpu_flip_step.rs:32,460,904,938; liquid_state.rs:14,113,157,187,298; gpu_flip_scene_tests.rs:13,279; liquid_frame.rs:11,145 |
| Whitewater | `whitewater.rs:186 require_extended_faces`; `shaders/sample_faces_at_particles_body.wgsl:82` | needs face velocity ≥1 layer past the liquid and samples faces at spray/foam/bubble particles anywhere in the air |
| Perf oracles | `tests/gpu_proofs/gpu_flip_frame_perf.rs` (shipped `WaterDamBreakGpuFlip.json`, 64, per-label p50/p95, output hashes); `gpu_flip_scene_tests.rs::gpu_flip_speed_measure` (`dam_break(64)`, `still_pool(64)`, `dam_break(128)`, per label) | the 128 measure path is `gpu_flip_speed_measure` |
| Test harness | `gpu_flip_step_tests.rs:85–135` `Pass` (`bind`, `run(entry, params, out, len, threads)`) | per-kernel runs over `dispatch_pass` |
| Fixture | `tests/fixtures/dambreak_pressure_problems.bin.zst` (SWFX v1, 64³, frames 0,15,30,45,60,90,120) | water masks for the CPU tile count |
| Scope map | `scripts/gpu_scope.py:113–121` | `primitives/gpu_flip_`, `shaders/gpu_flip_` → proofs `gpu_flip_` and `face_grid_tests::`; new files under those prefixes map without edits |

Extend, don't redesign: one stage node, one solver module, the particle-frame seam unchanged, buffers stay full-lattice (no paging). Only the work becomes sparse.

## 2. Decisions

**D-1. Tile = 8³ cells, T_a = ceil(n_a/8) per axis, partial edge tiles allowed.** A thread in a partial tile whose cell index ≥ n returns. Rejected: requiring n to be a multiple of 8, because that is a size rule.

**D-2. Classification on the GPU from the sort, no readback, no atomics.** `tiles_classify`: one thread per tile scans `ranges[c].count` over its box grown by CELL_REACH = 2 cells and writes `tile_near[t]` = the Chebyshev cell distance from the box to the nearest particle-holding cell (0 occupied, 1, 2, or 3 for none within reach). Thread 0 flips the ring halves' parity word first, so the parity is GPU state and a replayed encode stays right. `tiles_rings`: one thread per tile reads `tile_near == 0` over the (2·ring_max+1)³ tile neighbourhood, takes the ring = Chebyshev tile distance to the nearest occupied tile (ring_max+1 if none within ring_max), and writes the tile's rank — 0 in C (D-3), 1 for the rest of rings 0 and 1, else the ring — into the current half of `tile_rank`; the rank is what both the lists and the retire need, and ring ≤ k is rank ≤ k for every k ≥ 1. `tiles_lists`: one thread (precedent `pocket_round`) counting-sorts tiles by rank, stable in tile order, into `tiles_by_ring`, writes the counts (word 0 = |C|, word k ≥ 1 = tiles with ring ≤ k), the indirect triples `[2·count, 1, 1]` (512 threads per tile, 256 per group) for words 0..=ring_max then the retired list (D-5), and the active-fraction stats word. `ring_max(band) = ceil((1+band)/8)` for the band the step runs with (2 at 64, 3 at 128; derived, not a cap). Rejected: atomics for compaction, because the order must be deterministic for bitwise proofs; a workgroup scan, because the 1-thread builder over T³ ≤ 32³ tiles at 256³ is measured before it is optimised (deferred, section 9); CPU readback, because the occupied-block rule (status line) forbids it.

**D-3. One tile set per pass from its stencil reach, not one halo for all.** Every cell pass in scope reads within 2 cells of a particle-holding cell, so they all share C = {ring ≤ 1 and near ≤ 2}: the tiles whose box lies within 2 cells of a particle. Classifying by cell distance rather than tile ring matters: ring ≤ 1 alone lights 0.562 of tiles at 64 frame 0 (appendix, `halo1+1ring`) where the cell passes need 0.312. The extend is dense (D-4). Rejected: the band-wide halo for everything, because it lights 0.83 of tiles at 64 (section 0); a narrower extend halo, because whitewater observes faces beyond it (section 9).

**D-4. The extend stays dense in Phase 1; every layer writes every record.** The liquid is not the extend's only source. Every box-wall face carries weight 1 (`particles_to_faces` and `canonical_face`), and `extend_faces` takes any weight > 0 neighbour as a source, so each layer grows a zero-velocity shell one cell deeper off every wall, band deep, through tiles at any ring. `extend_new` and `extend_spread` start from `f`, which `face_gravity` (g·dt on every existing face), `subtract_pressure` and `constrain_solid_faces` write densely, so their far field is not canonical either, and a body far from the liquid is one more source through constrain. A layer run over ring ≤ r(i) with a canonical read cap leaves those shells unwritten in the tiles it skips, and the ping-pong through `b` leaves a skipped record holding a value two layers or one tick old. First differing record on `dam_break(64)` frame 1: cell (50, 0, 0), component x, in `a` and then `out_faces` (dense: filled from the x = n wall at layer 14; ringed: tile x = 6 has ring 3 > ring_max). So the three face outputs (`a`, `f`, `out_faces`) are dense by construction and need no clear when a tile retires, and `Params.ring_cap` stays unused. The extend is 3 × ~0.4 ms of 24.5 ms at 64. Rejected: a closed-form per-layer wall shell as the capped read, because it covers `extend_old` only; skipping layer 1 in inactive tiles, because face records there would hold the previous tick's values and whitewater would read them. The frontier-only extend is the sparse form if one is ever needed (section 9).

**D-5. Retire-clear instead of a dense clear per tick.** The rank table is ping-ponged; exactly the tiles with `rank_prev == 0` and `rank != 0` are retired this tick, and `tiles_retire` (512 threads a tile, indirect) writes the canonical values of the buffers the cell passes own: `water = 0`, `phi = 3h`, `rhs = 0`, and the gathered face record of each cell (`canonical_face`: every face absent, a box wall face closed). The gather has its own buffer `g` for this: `a` is the extend's output and is dense-bitwise by D-4, so it is never canonical and could not be the sparse gather's target. `tiles_fill` writes the same canonical values over every record and cell once per lattice, on the first step after `reserve` builds the lattice (`StepState.filled`); the records past the lattice (p[a] = n) belong to no cell and keep that fill. The zeroed first "previous" half retires every tile outside C on the first step, harmlessly. Rejected: a dense clear per tick, because it costs a full lattice write per buffer, which is the work being removed; leaving stale values, because dense readers (`pocket_pin` → `solve_water`, the solver's `is_water`, rows, `coarsen_water`) would see phantom water; the superset retire rule (`ring_prev ≤ 1`), because it rewrote every rank-1 tile every tick.

**D-6. The oracle lever is `StepParams.all_tiles`, test-only.** With it set, `tiles.rings` writes ring = 0 for every tile, so every pass runs dense through the same kernels and the same list machinery. It is settable only behind `cfg(feature = "gpu-proofs")` and never a node param. Rejected: keeping the old dense kernels as a second code path, because two paths drift.

**D-7. Phase 2 (the solve) is sketched here and decided at its own gate.** Section 7 fixed the shape; the coarse-level ring rule and the GPU-written `partial_count` it sketched were both replaced at implementation (D-8). Rejected: building the solve in the same phase, because its proof (iteration-count equality and bitwise residuals) is a separate gate and the Phase 1 measure decides whether the solve's share justifies it.

**D-8. The solve owns its tile sets: one per level, from that level's water, not the step's table.** The reference solver spans fluid cells only (`pressuresolver.cpp` `_pressureCells`); the solve's every stencil read is of a water neighbour, restriction reads one fine cell past a coarse water cell's children, prolongation reads one coarse cell past a fine water cell's parent. So a level's tile is active when a touched cell lies in its box grown by one cell, touched being water on the fine level and any touched child on a coarse one (`coarsen_water_main` writes it beside the coarse water). That set is inside C and closed under every read the solve makes, which the poison proof checks. Rejected: feeding the step's table down the levels (the coarse-ring sketch), because the solve then depends on a table its own `Rig` proofs do not have, and C is wider than the solve needs. Rejected: a GPU-written `partial_count`, because partials indexed by tile (two a tile, inactive tiles read as 0 in the finalize, which is bitwise-safe: a dense run's inactive partials are +0.0) make every reduction a fixed tree with no count to carry. `init_main` stays dense so pressure and r are the dense solve's zero everywhere; z, p, the residual scratch and the coarse right-hand sides and corrections are stale outside the set and never read. Lists come from one 256-thread workgroup integer scan per level, deterministic; the level's gate triple is written by that kernel. Under replay a recorded dispatch carries every tile's group count and the kernels return whole workgroups past the live count, so the saving there is the work, not the launches; encoded directly the launches shrink too.

**D-9. Phase 2b is the body passes over the solve's fine active tiles; rows and the coarsenings ride along.** The Phase 2a stamped measure at 64 put `rows` and the coarsenings at 0.12 ms together and the body passes (`bodies.impulse`, `bodies.react`, the operator's body term) at about 5.8 ms, so the re-scope follows the time. The impulse partial pass runs one workgroup per (fine tile half, body) over the solver's lists, `if !listed` returning whole; each thread takes one cell and its three low faces, so the pass walks the n³ cell lattice instead of the old (n+1)³ record loop (every inner face is some cell's low face, and the old loop's filters are the same `p[a] ≥ 1` and owner test). Its partial slot is the solver's (`2·lists[tile] + half`), so the finalize is a fixed tree per body over the fine slots, skipping `flags == 0`. Bitwise safety is the D-8 argument: an owned face pushes only beside water, every water-adjacent cell is in an active tile by the grown-box rule, and a dense run's inactive partials are +0.0, which adding or skipping leaves bitwise alone. Stated plainly: the finalize's reduction order changed against the old dense pass, so sparse-vs-all_tiles is bitwise through the new kernels and old-vs-new is checked by `gpu_flip_body_push_against_iterations` (force and torque within 1% of the 64-iteration solve). `body_product` walks the listed cells; `velocity_change` stays dense (one thread a record, no water read). Partials are sized by the lattice, `2·ΠT_a` slots a body, grown only at `prepare`. The tile buffers reach the passes as a parameter, not through `Bodies`, so a merge on the body row shape stays clear of the dispatch shape. Rejected: rows and the coarsenings first (0.12 ms); a body-indexed atomic reduction (atomics are integer-only here and order-dependent in float).

## 3. The tile table

Buffers (all `u32`, in `TileTable` beside `LatticeBuffers`, counted in `scratch_bytes` and the extent hold; bindings 27–32; the fill, retire and poison bind the gathered faces `g` at 4 and `water`, `phi`, `rhs` at 33–35):

| Buffer | Length | Writer | Readers |
|---|---|---|---|
| `tile_near` | T³ | `tiles_classify` | `tiles_rings` |
| `tile_rank` | 2·T³, halves by the parity word | `tiles_rings` | `tiles_lists` (both halves), the poison; Phase 2's per-level lists |
| `tiles_by_ring` | T³ | `tiles_lists` | every sparse pass (thread → tile) |
| `tile_counts` | ring_max+4: |C|, ring ≤ k for k = 1..=ring_max+1, retired count, parity | `tiles_lists` (parity: `tiles_classify`) | proofs |
| `tile_args` | 3·(ring_max+2) indirect triples (C, each ring cap, retired) | `tiles_lists` | `dispatch_compute_indirect` |
| `tiles_retired` | T³ | `tiles_lists` | `tiles_retire` |

`tile_rank` and `tile_counts` carry state across steps, so they are shared buffers zero-filled once; the table is rebuilt when the lattice or `ring_max` changes.

Thread mapping in every sparse pass (`list_cell`, `c_cell_index`):

```wgsl
let tile = tiles_by_ring[gid >> 9u];
let cell = unflatten(tile, tile_dims()) * 8 + unflatten(gid & 511u, vec3(8));
if any(cell >= lattice) { return NO_CELL; }
```

The gather maps a C cell to its own record (`c_face_index`). The records past the lattice (p[a] = n) belong to no cell and are constants — the wall face closed, the others absent — written by `tiles_fill` and never again.

The reach rule, per pass in Phase 1 scope:

| Pass | Stencil reach d from a particle-holding cell | Tile set |
|---|---|---|
| `particle_distance` | 2 (φ from particles in the 3³ neighbourhood, written for cells within 2) | C |
| `particles_to_faces` | 1 | C |
| `water_from_phi`, `phi_into_solids` | 2 | C |
| `divergence` | water cells only | C |
| `density_source` | water cells only (writes 0 elsewhere → canonical) | C |
| extend (old, new, spread), every layer | walls, gravity and the liquid (D-4) | dense |

Out of scope and dense in Phase 1: `face_gravity`, solids (`open_fractions`, `solid_face_velocity`), pockets, the pressure and density solves, `subtract_pressure`, `constrain_solid_faces`, `faces_to_particles`, and the sort.

Committed signatures (Rust):

```rust
pub(crate) const TILE: u32 = 8;
pub(crate) const CELL_REACH: u32 = 2;
pub(crate) fn tile_counts(cells: [u32; 3]) -> [u32; 3];       // ceil(n_a / TILE)
pub(crate) fn ring_max(band: u32) -> u32;                     // ceil((1 + band) / TILE)
pub(crate) fn tile_scratch_bytes(cells: [u32; 3], ring_max: u32) -> u64;
pub(crate) fn scratch_bytes(cells: [u32; 3], slots: u64, ring_max: u32) -> u64;
struct TileTable { ring_max, near, rank, by_ring, counts, args, retired }
struct LatticeBuffers { …, g /* the gather's faces, canonical outside C */, a /* g extended */, … }
fn encode_tiles(enc, pipes, params, t: &TileTable, ranges, capped, tally);
```

WGSL entries: `tiles_classify`, `tiles_rings`, `tiles_lists`, `tiles_fill`, `tiles_retire`; dispatch labels `gpu_flip.step.tiles.classify`, `.tiles.rings`, `.tiles.lists`, `.tiles.fill`, `.tiles.retire`. The sparse passes keep their entry names and labels; `Params` carries `all_tiles`, `ring_max` and `ring_cap` (unused while the extend is dense, D-4; Phase 2's coarse levels take it), and the bindings gain `tiles_by_ring` and `tile_rank`.

## 4. The defined-value rule

Every lattice buffer, after every pass, holds the dense pass's value in every tile outside that pass's tile set, or is read only through a ring cap that returns the canonical record. Concretely:

- `water`, `phi`, `rhs`, and the gathered faces `g`: written over C each tick; canonical (0, 3h, 0, `canonical_face`) elsewhere by `tiles_fill` and `tiles_retire`. Dense readers see dense values.
- `a`, `f`, `out_faces`: dense-bitwise by D-4.
- `s`, `v`, `b`, `corners`, `pressure`, `pocket*`: owned by dense passes in Phase 1, unchanged.
- Intermediate extend layers: dense, every record written every layer (D-4); no cap, nothing to read through.

Machine checks:

1. The bitwise proof (section 6) over 60 Dam Break frames.
2. A NaN-poison proof: the test-only entry `poison_inactive` writes NaN into `water`, `phi` and `rhs` of every cell of tiles with ring > 1 after `tiles_retire` (dispatched by the step under the test-only `POISON` lever, the same approval as `all_tiles`); the step then runs; particles and `out_faces` must be bitwise equal to the unpoisoned run. A read that escapes C turns into NaN in the output and fails. The face records are not poisoned: `g` feeds the dense extend (D-4), so NaN there would reach `out_faces` by construction, not by a leak. `rg -n "poison_inactive" crates/ -g "*.rs"` outside `*_tests.rs` must return nothing (`gpu_flip_poison_entry_is_named_only_in_tests` asserts it; the step builds the pipeline from the tests' `POISON_ENTRY` constant).
3. Extent CPU proofs: `gpu_flip_*_cover_every_dispatch` extended to the four tile dispatches; `tile_scratch_bytes` is counted in `scratch_bytes` and the extent hold.

## 5. Stats

Word 16: the active cell-set tile fraction, `|C| / T³` as f32 bits, written by `tiles_lists` (`capped` bound at the solver words). `LIQUID_STATS_WORDS` 16 → 17, `SOLVER_WORDS` 6 → 7 (the `tally` offset is derived, not a literal). Every call site in the audit table is touched in the same commit; `liquid_stats.rs` gains the readout. Re-derive the inventory before editing: `rg -n "LIQUID_STATS_WORDS|SOLVER_WORDS" crates/ -g "*.rs" -g "*.wgsl" #grep-ok`.

## 6. Proof plan

| Proof | Oracle | Pass condition |
|---|---|---|
| `gpu_flip_tiles_match_the_cpu_classification` | CPU near/ranks from the sort's `ranges` read back once in the test (the test reads back; the step never does) | `tile_near`, `tile_rank`, `tiles_by_ring` order, `tile_counts`, `tile_args` triples, the retired list and the parity flip equal to the CPU model on `dam_break(64)` frames 0, 30, 60; `all_tiles` gives near 0 and rank 0 everywhere |
| `gpu_flip_sparse_step_matches_dense_bitwise` | the same kernels with `all_tiles = true` in the same run | 64 Dam Break, 60 frames, per-frame hash of particles and `out_faces` equal; the HEAD-dense hash from `gpu_flip_frame_perf` reported once in the phase report (not pinned, because it moves with any kernel change) |
| `gpu_flip_sparse_step_survives_poison` | NaN poison (section 4) | particles and `out_faces` bitwise equal to the unpoisoned sparse run over 10 frames |
| `gpu_flip_tile_extent_covers_every_dispatch` (CPU) | extent model | every tile dispatch covered; scratch bytes include the table |
| Existing `gpu_flip_` and `face_grid_tests::` proofs | unchanged | green |

Stats proof: word 16 at frame 0 of `dam_break(64)` equals the CPU count's halo1 fraction for that frame within one tile of 512 (the fixture's frame 0 is the standing column: 0.312).

## 7. Phase 2 — the pressure solve over tiles (D-8)

- Every level's `smooth`, `residual`, `apply`, `restrict`, `prolong`, `direction`, `update` and the dot-product partial run over that level's active tiles: 512 threads a tile in list order, `listed_cell` through `lists[list_base + tile]`, `NO_CELL` past the lattice. A level's `Params` carry `list_base` and `level` (its gate triple).
- Active set per level: `classify_main` (one thread a tile, the box grown by one cell over the level's touched mask) into `flags`, `lists_main` (one workgroup, integer scan) into `lists` and the level's triple of `armed`, both at `prepare` after the level's water. Touched: the fine water; a coarse level's any-touched-child, written by `coarsen_water_main`.
- Partials: two a tile of the fine level, `partial_count = 2·T³`; `dot_finalize` and `check` read an inactive tile's as 0 (`fine_partial`).
- `init_main`, `rows`, the coarsenings, the inverse, `coarse_solve`, `dot_finalize`, `check`, `arm`, `tally`: dense or single-group, unchanged in shape.
- Oracle: the step's `all_tiles` lever reaches the classifier (every tile active). Proofs: `pressure_module_sparse_matches_all_tiles` (pressure and stop record bitwise at 64³ and 25³, on the stop and at a fixed count, direct and replayed, with NaN poisoned outside the set), `gpu_flip_sparse_step_matches_dense_bitwise` (the solver words per frame over the 60-frame run).
- Phase 2b (D-9): the body passes over the fine active tiles. `impulse_partial` one workgroup per (fine tile half, body) over the lists, `impulse_finalize` one workgroup per body over the fine slots skipping unflagged tiles, `body_product` over the listed cells, `velocity_change` dense. The tile triple reaches `apply` and `react` as a parameter from `PressureSolver::tiles()`. Proofs: `gpu_flip_body_passes_sparse_match_all_tiles` (the two bodies on `[17,16,15]` with water in one tile column: sums, s, the solid velocity and the reaction bitwise against all tiles, and again with NaN in x, s and every partial slot outside the set), `gpu_flip_body_step_sparse_matches_all_tiles` (the SubmergedBox and FloatingBox scenes through the preset runtime, 90 ticks each: the body row, the reaction, the particles and the solver's six words bitwise against all tiles, and again under the step's poison), `gpu_flip_body_push_against_iterations` as the old-vs-new bar. Rows and the coarsenings stay dense (0.12 ms at 64, measured in the 2a run).
- Phase 2b measure (`frame-time /tmp/waterFunv3.manifold --frames 300 --resolution 64 --splash-frames 100 --stamp-every 5`, p50 ms, 2a → 2b): `bodies.partial` 2.35 → 1.50, `bodies.finalize` 2.34 → 0.92, `bodies.product` 1.14 → 0.96, the body passes 5.83 → 3.37; plain-frame true GPU total 28.86 → 25.87. The solve's passes moved within noise (+0.25 on `smooth`, the rest under 0.05).
- Phase 2c, the transfer kernels: `restrict_main` and `prolong_main` already ran over the per-level lists; their cost was the gather (64 taps a coarse cell, 8 a fine cell, each a global read). Each half-tile workgroup now stages its taps in workgroup memory once (fine footprint 18×18×10 for restrict, coarse 6×6×4 for prolong; entries past the lattice zeroed, never read) and gathers from the stage. The operator is the paper's trilinear prolongation and its transpose over 8 (McAdams, Sifakis & Teran 2010, the form `scripts/mgpcg_reference.py` mirrors; the FLIP Fluids engine is PCG+MIC(0) and has no transfer form), unchanged, in the same tap order with the same weights — only where a tap is read from changed. Not bitwise against the gather kernels: the Metal compiler contracts the products differently in the two bodies, so pressures differ at the last place (dam break 64³ and 25³, every saved problem: first differing cell 0, 1.8482413 vs 1.8482416; max |Δp| 2.1e-5 on |p| up to 22.7, max relative 1.9e-5). Proven at value level against the f64 reference (`pressure_module_matches_reference_*`: within 10% at 3 iterations, 2× at 8, every pin 1.000×); iterations to tolerance identical to the gather kernels, 0 capped: SubmergedBox 17, FloatingBox 12, dam break 64³ 12/12/13/12, 25³ 10/9/10/10, pinned sealed box 14. `pressure_module_sparse_matches_all_tiles`, `gpu_flip_sparse_step_matches_dense_bitwise`, `gpu_flip_body_step_sparse_matches_all_tiles` green.
- Phase 2c measure (same `frame-time` run, p50 ms, 2b → 2c): `pressure.restrict` 2.39 → 1.14, `pressure.prolong` 2.08 → 0.57; `smooth` 4.17 → 4.34 and `residual` 0.50 → 0.52 (noise). Plain-frame true GPU total 25.87 → 25.88: the stamped saving (2.8 ms over the two passes) does not reach the plain frame. Whole-buffer stamped GPU 81.55 → 81.19. Open: why the plain frame does not move when its stamped passes do — the stamped mode is one encoder per dispatch, so the two measures disagree on what a transfer pass costs inside a single encoder.
- Reading the two tables: the per-dispatch stamped table (`--stamp-granularity dispatch`, the default) opens one encoder per dispatch, so it ranks passes only relative to each other inside one encoder and never adds up to the frame. The per-node table (`--stamp-granularity node`, one sampled encoder per graph step, replay off) is the plain oracle: its per-node-type p50 is the plain frame's breakdown and a cut is chosen from it. The fixture couples a body, so it is run with `--stamp-every 1` (an interleaved plain frame leaves its reaction in flight and the next stamped frame skips the step; `frame_time.rs` module doc).
- Node measure after 2c (`frame-time /tmp/waterFunv3.manifold --frames 300 --splash-frames 100 --stamp-every 1 --stamp-granularity node`, calm p50 ms). 64: whole buffer 22.06; `gpu_flip_step` 15.61, `render_scene` 8.66, `whitewater_step` 1.05, `particle_volume` 1.01, the surface chain (`smooth_lattice` 0.51 down to `liquid_solid_distance` 0.03) 1.6, compositor 0.24. 128: whole buffer 98.04; `gpu_flip_step` 67.75, `render_scene` 16.10, `particle_volume` 6.99, `whitewater_step` 5.00, the surface chain 12.5, `liquid_state` 0.52, `liquid_frame` 0.42. The node sums exceed the whole buffer (28.7 at 64, 110 at 128): the render passes overlap the compute on the GPU, so `render_scene` is partly hidden. Plain frames with replay on and off at 64: 27.18 and 29.86, so replay is worth 2.7 ms there.
- Open after 2b: one stamped run with `all_tiles` forced, to settle whether the lever's dense path still matches the pre-tiles frame time. The lever is `cfg(all(test, feature = "gpu-proofs"))` (D-6), so `frame-time` cannot reach it; running it needs a ruling on a probe-build lever (recorded here when run).

## 8. Phasing briefs

### Phase 1 — tile table + lattice passes

**Entry state:** branch `feat/solver-occupied-blocks` at 955e47752; this doc and GPU_FLIP_STRUCTURAL_OPTIONS.md committed.
**Read-back:** sections 3–6 of this doc, `gpu_flip_step.rs` `encode` :526 and `extend` :326, the pocket-gate indirect dispatch :378, `gpu_flip_step.wgsl` :1213–1676, `extent.rs:1202`, ADDING_PRIMITIVES.md (stage-node exemption).
**Deliverables:** `TileTable` and the four kernels; the cell passes over C (the extend stays dense, D-4); `all_tiles` behind `gpu-proofs`; `tiles.retire`; stats word 16 with every call site; the four proofs of section 6; `gpu_scope.py` unchanged unless a new file falls outside its prefixes.
**Gate, positive:** all section 6 proofs green through `scripts/gpu_queue.py -- cargo test --manifest-path ".claude/worktrees/slot-5/Cargo.toml" -p manifold-renderer --features gpu-proofs gpu_flip`; CPU extent proofs green before any GPU run; `cargo clippy -p manifold-renderer -- -D warnings` clean.
**Gate, negative:** the poison proof fails when `particle_distance` is dispatched dense instead of over C (demonstrated once in the phase report, then reverted); `rg -n "poison_inactive"` outside tests returns nothing.
**Acceptance demo (level: measure):** `gpu_flip_frame_perf` (64) and `gpu_flip_speed_measure` (`dam_break(64)`, `dam_break(128)`) per label, `all_tiles` vs sparse, reported as a table of p50 ms before/after per pass; the stats word 16 trace over the 60 frames. Prediction to check against: cell-pass ratios 0.31–0.70 at 64, 0.25–0.49 at 128; extend 0.7–1.0 at 64, ~0.65 at 128. Probe limitation: the stamped per-pass table (`frame-time --stamp-every 5`) carries the step's rows at 64 only; at 128 the step's chunk is missing from it, so the 128 oracle is the plain-totals span run (`--stamp-every 100000`) until the frame-time probe covers it.
**Forbidden moves:** CPU readback of any tile buffer in the step; a size rule on n; a narrower extend halo; a second dense code path; `#[ignore]`; timing asserts in correctness proofs; a node param for `all_tiles`; silent fallbacks when a tile count is zero (an empty list is a legal zero-group dispatch).
**Test scope:** `-p manifold-renderer`, filter `gpu_flip`; the extent and stats CPU tests via nextest.

### Phase 2 — the pressure solve over tiles

Opened on Phase 1's measure (the solve was 62% of the tick at 64). Phase 2a, the lattice passes over the solve's own sets (D-8, section 7), is built and proven. Phase 2b, the body passes over the fine active tiles (D-9, section 7), is built and proven; rows and the coarsenings stay dense on the 2a measure (0.12 ms at 64). Forbidden moves as Phase 1 plus: no change to `MAX_ITERATIONS`, no change to the stop rule.

## 9. Deferred

- **Narrower extend halo** (R = travel+2 cells, the engine's `_extrapolateFluidVelocities` on active blocks): changes face values beyond R that whitewater spray samples. Reopens only with a ruling on what whitewater may read beyond the halo.
- **Frontier-only extend** (sweep only records at the current layer's frontier, every source included): the only sparse extend that can be bitwise, because the dense extend's sources are the walls and the gravity-written far field as well as the liquid (D-4) — a tile-ringed layer with a canonical read cap misses the wall shells. A different algorithm with its own proof; after Phase 2's measure, and only if the extend's ~1.2 ms at 64 is then worth it.
- **Workgroup scan for `tiles.lists`** if the 1-thread builder measures over 0.3 ms at 256³.
- **Solids, pockets, forces, `subtract_pressure`, `constrain_solid_faces`, `faces_to_particles` over tile sets:** after Phase 2's measure says what is left.
- **Whitewater grid atoms over the same lists:** GPU_WHITEWATER_DESIGN.md's call once the table exists.

## 10. Decided

- 8³ tiles, partial edge tiles, no size rule (D-1).
- Classification from the sort's bin counts by cell distance (`tile_near`), GPU-held parity, deterministic 1-thread list builder, no atomics, no readback (D-2).
- Per-pass tile sets from stencil reach; one set C = {ring ≤ 1, near ≤ 2} for every cell pass (D-3).
- The extend stays dense; its sources are the walls and gravity as well as the liquid (D-4).
- Retire-clear of `water`, `phi`, `rhs` and the gathered faces `g`, exactly the tiles leaving C; canonical fill on the first step of a lattice (D-5).
- `all_tiles` is the oracle, test-only (D-6).
- Phase 2 gated on Phase 1's measure (D-7).
- The solve's tile sets are its own, one per level from that level's water, tile-indexed partials, init dense, workgroup-scan lists (D-8).
- Phase 2b is the body passes over the fine active tiles, the finalize a fixed tree over the solver's slots; rows and the coarsenings stay dense (D-9).
- Stats word 16 = active cell-set tile fraction (section 5).

## 11. Solve Level — the pressure solve one level down (built; ruling 2026-10-03: (a) Rᵀ, (b) Pᵀ B P, (c) the pocket label coarsened)

The cut the node table points at (section 7, the node measure): the step is 71% of the 64 frame and 69% at 128, and inside the step the solve is 37% and the solve plus the body passes 51% of the stamped dispatch time (the 2c per-dispatch table at 64: `gpu_flip.pressure.*` 9.22 ms, `gpu_flip.bodies.*` 3.59 ms, the rest of the step 12.42 ms). The form is `GPU_FLIP_STRUCTURAL_OPTIONS.md` section 7 (Option D — coarse-grid projection), Lentine, Zheng and Fedkiw 2010, without its local velocity correction; the FLIP Fluids engine has no coarse solve to port.

**The param.** `solve_level` (label Solve Level, Int, default 0, range 0 to 4) on GPU FLIP Domain, next to Resolution, published to every step with the lattice. 0 is today's solve on the fine lattice. `k` runs the conjugate gradient on V-cycle level `k` (every side halved `k` times, rounding up): the pressure has `2^k` fine cells a side per unknown, the particles, transfers, advection, extend, surface and whitewater keep the fine lattice. A `k` past the coarsest level is a node error (`levels − 1` is the most the lattice has), never clamped. Live, like Resolution: a change restarts nothing, the next tick solves at the new level. Default unchanged is the whole quality story: the audience never sees the coarse pressure unless Peter turns it.

**Where it plugs in.** The solver's levels are the existing ones (`PressureSolver::prepare` builds every level's water, faces, rows, tiles and the coarse inverse today, unchanged). `solve` gains the level: its "fine" `View` is `view(k)` — the level's lattice, cell size `2^k h`, water, rows, tile list base and gate triple — and `v_cycle` runs from `k` down instead of from 0. The operator at level `k` is that level's existing rows (the re-discretised plain rows: an air coarse cell holds pressure 0 at its centre; the cell kind is coarsened by the rule in "As built" below), the smoother, transfers, coarse solve and stop are the existing kernels; the partials, the dot products and the stop's `|r|∞` are already tile-indexed per level, so they run over level `k`'s list with `.over(view(k))` where today they pass level 0. Nothing in the V-cycle below `k` changes. Not a second solver: one new branch in `solve` (which level the gradient starts on), zero new kernels inside the cycle.

**Around the solve, three passes move and two transfers appear.**
- Into the solve: the fine divergence `f` (the step's `divergence` pass, fine faces, unchanged) is carried to level `k` by `k` applications of the existing `restrict_main` (the transpose of prolongation over 8, masked to the level's water), the same operator the V-cycle applies to its residuals, so the coarse right-hand side is `Rᵀ f` at the scaling the reference script already mirrors.
- Out of the solve: the coarse pressure `p_k` is carried to the fine lattice by `k` applications of the existing `prolong_main` (trilinear, into zeroed fine `pressure`), and the fine `project` (`subtract_pressure`), `constrain`, `extend_new`, the density projection's `density_project` and the body reaction read the fine pressure exactly as today. The free-surface ghost condition is applied at the fine faces by `subtract_pressure` from the fine φ, as today; at level `k` the rows are plain (air cell centre at 0), which is Option D's named risk: a sheet thinner than `2^k` cells has no coarse water cell and no pressure of its own, only its neighbours' prolonged value.
- The density solve (`density_source` → the same solver on plain rows) follows the same level with the same two transfers.
- Bodies stay on the fine grid. With dynamic bodies the operator gains their term ρh·G M⁻¹ Gᵀ p, computed by the fine body passes (`bodies.partial`, `finalize`, `product` over the fine tiles). At level `k` each iteration's direction `p` is prolonged to the fine lattice (`k` prolongs into a scratch), the fine body passes run on it unchanged, and their product is restricted back (`k` restricts) and added to `s`: the coarse operator is `L_k + Pᵀ B P`, symmetric, so the conjugate gradient's guarantees hold and the reaction (`react`, fine pressure, fine faces) is untouched. The cost is one prolong and one restrict per iteration at fine size, which the table prices at 1.7 ms at 64 (`prolong` 0.57, `restrict` 1.14 per cycle); the alternative, coarse body rows (open fractions averaged as `coarsen_faces` does), takes the body passes down 8× too but is a second body operator with its own reference, and is deferred until this cut's measure says the 1.7 ms matters.
- Sealed pockets (`encode_pocket_mean`): the fine right-hand side has each sealed pocket's mean removed so a pure-Neumann pocket is solvable. Restriction masked to coarse water drops the fine water children of coarse air cells, so a pocket's coarse right-hand side no longer sums to zero and a sealed pocket's coarse solve is inconsistent (it drifts until the cap and reports capped, never an error). The fix in this cut: the pocket label is coarsened with the water (a coarse cell takes its children's label when all eight agree, the existing `coarsen_water` pass gains the label as a second output) and `pocket_accumulate`/`pocket_remove` run once more on the coarse right-hand side over level `k`'s list. Two existing passes dispatched at one more level; what they remove is added to the same solver word.

**What stays on the fine grid, verbatim:** the sort, the tile table, `particles_to_faces`, forces, solids, `open_fractions`, `divergence`, both extends, `subtract_pressure`, `constrain`, the body reaction and velocity change, `move`, the surface distance, `liquid_frame`, the mesher and the whitewater. The step's pass order is unchanged except the transfers wrapped around the two solves.

**Proof shape.**
1. Solve Level 0 is today, bitwise: `gpu_flip_sparse_step_matches_dense_bitwise`, `pressure_module_sparse_matches_all_tiles` and every reference pin stay green unchanged, and a new `gpu_flip_solve_level_zero_is_the_fine_step` runs the saved dam-break problems through `solve` with the level at 0 and asserts the pressure and the stop record bitwise against a run of the pre-change code path (the branch is `k == 0` → the same dispatch list; the proof is the dispatch list and the output both unchanged).
2. The coarse solve against the CPU reference at the coarse level: `scripts/mgpcg_reference.py` gains `--solve-level k` (build the `Multigrid` as today, take `levels[k]`, right-hand side `f·w` restricted `k` times by the script's own restriction, the conjugate gradient from that level, the V-cycle below it), and `pressure_module_solve_level_matches_reference_{64,128,odd_sides}` compares the GPU `p_k` against it at the fine pins' bars (within 10% at 3 iterations, 2× at 8, every pin 1.000×), plus the fine pressure `P p_k` against the script's prolongation. With `all_tiles` forced the same proof runs dense; sparse must match it bitwise (`pressure_module_sparse_matches_all_tiles` extended with the level).
3. The divergence bound on the fine faces. What the coarse solve guarantees exactly: the coarse residual `Rᵀ(f − L_0 P p_k)` is zero to the stop tolerance, i.e. the fine divergence left after `project` averages to zero over every coarse water cell (the measure the reference script checks). What it does not: the fine-scale remainder. `gpu_flip_solve_level_divergence_bound` runs the saved problems through the fine `divergence` pass after `project` at level 1 and level 0 and records `|div_after|∞ / |div_before|∞` and the coarse-cell means; the design pins the ceiling from the first run (the number is measured, not guessed) and any later change that raises it fails. The bound is stated in the doc with the run that set it.
4. Bodies: `body_solve` in the reference gains `P` (the body product on the prolonged direction, restricted), `gpu_flip_body_passes_sparse_match_all_tiles` and `gpu_flip_body_step_sparse_matches_all_tiles` run at level 1 as well as 0, and the impulse error against the reference at level 1 is pinned the way `body_gate` pins it today.
5. Iteration counts to tolerance at level 1 on every saved problem, 0 capped, recorded next to the fine counts in section 7; the pocket proof (`pocket_flux_pressure`) at level 1 with a sealed pocket, the coarse right-hand side summing to zero.

**Expected number, from the node table.** At 64 the stamped solve is 9.22 ms of the step's 25.23 ms dispatch time; one level down the per-iteration passes at fine size (four smooths, residual, restrict, prolong, direction, apply, update, the dots and the check: about 7.5 ms of the 9.22) shrink by the coarse-to-fine active-tile ratio, about 1/6 on the fixture (not 1/8: the halo tiles do not halve), to about 1.3 ms; prepare's per-level passes (rows, classify, lists, the coarsenings, the inverse: about 1.7 ms) stay. The two transfers around each solve add about 0.3 ms. So the solve goes from 9.2 to about 3.3 ms of stamped time, 5.9 ms off a 25.2 ms stamped step, 23%; the node step 15.6 → about 12.0 ms, the 64 frame 22.1 → about 18.5 ms. With the fixture's bodies the per-iteration prolong and restrict add 1.7 ms back: step about 13.7, frame about 20.2 ms. At 128 there is no per-dispatch table (the 128 step was never stamped per dispatch; it is the next dispatch-mode run to queue, stamp-every 1); if the solve's share holds, the step goes 67.8 → about 52 ms (about 59 with bodies), the frame 98 → about 82 (89). The honest reading: Solve Level is worth 3 to 4 ms at 64 and 10 to 15 at 128 on this fixture, because sparse tiles already took the solve's dense cost and the bodies' fine passes do not shrink; the step's other half (extend_old 1.8, pocket_sweep 1.6, particles_to_faces 1.6, density_source 1.1, distance 0.8, the tile passes 1.3 at 64) is the same size as what this cut removes, and section 9's deferred items are its list.

**Forks, ruled 2026-10-03 (a) Rᵀ, (b) Pᵀ B P, (c) coarsen the label; the text stays as the record.** (a) The right-hand side restriction: `Rᵀ` (the V-cycle's transfer, zero new kernels, the Galerkin pairing with `P`) as designed, or the box mean of the eight children (the exact coarse flux balance, Lentine's form, one `restrict_main` mode). The reference script's `coarsen` is the box mean; the design picks `Rᵀ` for one operator pair and asks the proof in step 3 to say whether the box mean leaves less fine divergence; if it does, the mode is one flag. (b) Bodies via `Pᵀ B P` (designed) or coarse body rows (deferred). (c) The pocket label coarsening as designed, or refusing Solve Level > 0 while a sealed pocket exists (a loud node error, no drift) as the first cut.


**As built (2026-10-03).** `solve_level` on GPU FLIP Domain (card Solve Level, 0 to 4), published to the step; the solver's `Solve { level, coarse_rhs }` starts the gradient on level `k` with the right-hand side restricted `k` times by `restrict_main` (Rᵀ, masked to the level's water) and prolongs the pressure back by `prolong_main`; the body term is `Pᵀ B P` (the direction prolonged to the fine lattice, the fine body passes, the product restricted); the pocket label is coarsened per level (`pocket_leader_clear`, `pocket_coarsen`, `pocket_relabel`: sealed when every in-lattice child is sealed under one label, the label the lowest coarse cell of that pocket) and the mean comes off the coarse right-hand side by the existing pocket passes at the coarse cell size. One deviation from the text above: the deepest level is `levels − 2`, not `levels − 1` — the coarsest level is solved exactly by its inverse and is never a gradient level (64³ → 64, 32, 16, 8, 4: levels 0 to 3; 25³ → 25, 13, 7, 4: 0 to 2). A level past that is a node error at param validation and at the solve, by name, never clamped.

**The coarsening rule (one rule, one code path, the preconditioner and the solve level alike).** McAdams, Sifakis and Teran 2010: a coarse cell is air if any child is air, solid only if every child is solid, otherwise water; a virtual child past an odd side is solid. The fine lattice has no solid kind of its own, so `coarsen_water_main` reads one from the faces: a non-water fine cell whose six face weights are all zero is solid (`kind()` in `gpu_flip_pressure.wgsl`; wall faces read closed through `face_weight`, never the raw buffer). Coarse levels store −1 for solid, 0 air, 1 water; the smoother, rows, transfers and the coarse inverse read `> 0.5` as before, so a solid coarse cell is a closed cell exactly like air at the operator and only differs in the next coarsening. The pocket coarsening (`pocket_coarsen`, `cell_solid` from the open fractions) skips solid children the same way and a coarse cell of solid children only is dry. `scripts/mgpcg_reference.py` mirrors it (`fine_solid`, `coarsen_water`). On stage: the cells a body cuts stay water at the coarse level instead of turning to air, so the coarse pressure reaches the body's faces.

Level 0 is no longer bitwise with the pre-rule solver: the rule changes every coarse level of the preconditioner, so the V-cycle's intermediate values move while the solve it preconditions does not. The pin is value-level instead — `pressure_module_matches_reference_64` against the f64 reference (every pin 1.000× at 3 iterations, under the f32 floor at 8), `gpu_flip_solve_level_zero_is_the_fine_step` asserting iteration counts within ±1 of the pre-rule counts on the saved problems (dam break 12/12/13 at frames 0/15/30, measured identical) plus a deterministic re-run bitwise against itself, and `pressure_module_solve_level_converges_on_the_engine_tolerance` carrying the whole pre-rule count table at level 0 (dam break 12, 12, 13, 14, 13, 12, 14; pools 11–12), 0 capped. Measured after the rule: every level-0 count identical, level 1 one under level 0 everywhere, and the body step's SubmergedBox solve went 17 → 11 iterations at level 0 (the rule removes the coarse air the body used to punch into the preconditioner).

Proofs (all green on this GPU, `gpu_flip_pressure_tests.rs`, `gpu_flip_step_tests.rs`, `gpu_flip_body_tests.rs`): level 0 as above; level 1 against `scripts/mgpcg_reference.py --solve-level 1` at 64³, 128³, 25³, 37³, 40³, every pin 1.000× at 3 iterations and under the f32 floor at 8; sparse = dense = poisoned bitwise at level 1 (`pressure_module_sparse_matches_all_tiles`); the body step bitwise sparse against dense at level 1 over 90 ticks on both box scenes; the coarse pockets against a CPU model and the coarse mean summing to zero, the flux at the coarse h³; convergence on every shipped problem at levels 0, 1 and 3 (level 1 one iteration under level 0 everywhere: dam break 11–12 against 12–14, pools 10 against 11; level 3 5–8).

The divergence bound (`gpu_flip_solve_level_divergence_bound`, 64³ dam break, engine stop): the fine remainder `f − L₀(P p₁)` is |r|∞/|f|∞ 0.97–1.47 and |r|₂/|f|₂ 0.90–1.10, the largest box mean over a coarse cell's water children 0.52–1.31 of |f|∞ against 0.41–0.88 for `f` itself; level 0 leaves 1e-6 to 1e-5. The coarse solve zeroes the Rᵀ-weighted means, and a particle divergence is almost all finer than a coarse cell, so at level 1 the cell-scale divergence stays in the faces: the water compresses and drifts at the two-cell scale, which the PNG pair is for. The box-mean restriction mode (fork (a)'s alternative) is not built; the measured box means say the Rᵀ solve leaves the worst box where `f` had it (a coarse air cell with water children holds no unknown).

Bodies at level 1 under the paper's rule: `scripts/mgpcg_reference.py --body 32,0.5,0.4,0.5,0.12,0.12,0.12,0.6 --solve-level 1` reports the converged impulse off the fine direct solve by 8.6% (ratio 0, held), 33.9% (0.1), 6.4% (1), 4.5% (10), against 0.00% at level 0 for every ratio (before the rule: 73–96%). The ratio-0 row has no body term at all — the 8.6% is the coarse pressure's own discretisation at the body's faces (two fine cells per unknown, prolonged linearly across a solid boundary), so the fine bar (1% at the engine stop) cannot be met at level 1 by any body coupling; it is the price of the level, not a defect of `Pᵀ B P`. No GPU proof pins the level-1 impulse against the reference at the fine bar for that reason; the GPU proof at level 1 is the body step sparse against dense bitwise over 90 ticks (both box scenes, 12 iterations, 0 capped). On stage a floating body at Solve Level 1 rides a few percent low on its buoyancy and a light one (ratio 0.1) a third low; Solve Level 0 is still the setting for a scene whose bodies have to float right.

Measured node tables at level 0 and 1 on `/tmp/waterFunv3.manifold`: the level-1 run is blocked — a saved generator layer carries its own graph snapshot and that snapshot is the manifest authority (`gather_known_params`), so a project saved before this card has neither the card nor the domain→step wire, and `frame-time --solve-level` refuses by name. The class fix is a saved-graph migration that adds the param, the wire and the binding, not a frame-time override.

## Appendix — the tile count on the fixture

Script (`/tmp/tile_halo_count.py`, run 2026-10-02 against `tests/fixtures/dambreak_pressure_problems.bin.zst` decompressed with `zstd -d`; the 128 rows refine the 64 masks 2× per axis):

```python
#!/usr/bin/env python3
"""Active-tile fractions: the Dam Break pressure problems' water masks, 8^3 tiles,
several halo rules, at 64 and refined to 128 (each cell -> 2^3)."""
import struct
import numpy as np

TILE = 8
raw = open("/tmp/dambreak_problems.bin", "rb").read()
assert raw[:4] == b"SWFX"
ver, nx, ny, nz, count = struct.unpack_from("<5I", raw, 4)
assert ver == 1 and nx == ny == nz
n = nx
cells = n ** 3
at = 24
problems = []
for _ in range(count):
    frame, wet = struct.unpack_from("<2I", raw, at)
    at += 8
    bits = np.frombuffer(raw[at:at + cells // 8], dtype=np.uint8)
    at += cells // 8
    water = np.unpackbits(bits, bitorder="little")[:cells].astype(bool)
    at += 4 * wet
    problems.append((frame, water.reshape(n, n, n)))  # x fastest -> [z, y, x]


def dilate(mask, k):
    """Chebyshev dilation by k cells: separable running max per axis."""
    out = mask.copy()
    for axis in range(3):
        acc = out.copy()
        for d in range(1, k + 1):
            for sign in (1, -1):
                shifted = np.zeros_like(out)
                src = [slice(None)] * 3
                dst = [slice(None)] * 3
                src[axis] = slice(d, None) if sign > 0 else slice(None, -d)
                dst[axis] = slice(None, -d) if sign > 0 else slice(d, None)
                shifted[tuple(dst)] = out[tuple(src)]
                acc |= shifted
        out = acc
    return out


def tiles(mask):
    t = mask.shape[0] // TILE
    return mask.reshape(t, TILE, t, TILE, t, TILE).any(axis=(1, 3, 5))


def report(label, water, band):
    total = (water.shape[0] // TILE) ** 3
    wet, halo1, halo_band = tiles(water), tiles(dilate(water, 1)), tiles(dilate(water, band))
    ring1, ring2 = dilate(halo1, 1), dilate(halo1, 2)
    f = lambda t: t.sum() / total
    print(f"{label}: water {water.mean():.3f} | tiles: wet {f(wet):.3f} halo1 {f(halo1):.3f} "
          f"halo{band} {f(halo_band):.3f} halo1+1ring {f(ring1):.3f} halo1+2rings {f(ring2):.3f}")
    return f(halo1), f(halo_band), f(ring1)


sums = {64: [], 128: []}
for frame, water in problems:
    sums[64].append(report(f"n=64  frame {frame:3d}", water, 14))
    refined = water.repeat(2, axis=0).repeat(2, axis=1).repeat(2, axis=2)
    sums[128].append(report(f"n=128 frame {frame:3d}", refined, 23))
for label, s in sums.items():
    a = np.array(s)
    print(f"mean n={label}: halo1 {a[:,0].mean():.3f} band halo {a[:,1].mean():.3f} "
          f"halo1+1ring {a[:,2].mean():.3f} max band halo {a[:,1].max():.3f}")
```

Output:

```
n=64  frame   0: water 0.169 | tiles: wet 0.312 halo1 0.312 halo14 0.656 halo1+1ring 0.562 halo1+2rings 0.750
n=128 frame   0: water 0.169 | tiles: wet 0.227 halo1 0.250 halo23 0.594 halo1+1ring 0.375 halo1+2rings 0.500
n=64  frame  15: water 0.173 | tiles: wet 0.277 halo1 0.293 halo14 0.656 halo1+1ring 0.500 halo1+2rings 0.672
n=128 frame  15: water 0.173 | tiles: wet 0.216 halo1 0.259 halo23 0.516 halo1+1ring 0.374 halo1+2rings 0.480
n=64  frame  30: water 0.178 | tiles: wet 0.314 halo1 0.332 halo14 0.641 halo1+1ring 0.531 halo1+2rings 0.711
n=128 frame  30: water 0.178 | tiles: wet 0.231 halo1 0.263 halo23 0.565 halo1+1ring 0.375 halo1+2rings 0.475
n=64  frame  45: water 0.185 | tiles: wet 0.381 halo1 0.422 halo14 0.852 halo1+1ring 0.721 halo1+2rings 0.941
n=128 frame  45: water 0.185 | tiles: wet 0.280 halo1 0.312 halo23 0.743 halo1+1ring 0.485 halo1+2rings 0.639
n=64  frame  60: water 0.192 | tiles: wet 0.514 halo1 0.545 halo14 0.994 halo1+1ring 0.922 halo1+2rings 1.000
n=128 frame  60: water 0.192 | tiles: wet 0.351 halo1 0.389 halo23 0.940 halo1+1ring 0.628 halo1+2rings 0.818
n=64  frame  90: water 0.198 | tiles: wet 0.637 halo1 0.701 halo14 1.000 halo1+1ring 0.984 halo1+2rings 1.000
n=128 frame  90: water 0.198 | tiles: wet 0.407 halo1 0.491 halo23 0.990 halo1+1ring 0.802 halo1+2rings 0.955
n=64  frame 120: water 0.196 | tiles: wet 0.596 halo1 0.639 halo14 1.000 halo1+1ring 0.969 halo1+2rings 1.000
n=128 frame 120: water 0.196 | tiles: wet 0.369 halo1 0.446 halo23 0.981 halo1+1ring 0.731 halo1+2rings 0.905
mean n=64: halo1 0.463 band halo 0.828 halo1+1ring 0.741 max band halo 1.000
mean n=128: halo1 0.344 band halo 0.761 halo1+1ring 0.538 max band halo 0.990
```

The water fraction per frame (0.17–0.20) is from the fixture; the options doc's 0.16 was geometric.
