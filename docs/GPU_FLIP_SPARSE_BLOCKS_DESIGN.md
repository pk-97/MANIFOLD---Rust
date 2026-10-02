# GPU FLIP sparse blocks — occupied 8³ tiles for the dense lattice passes, then the pressure solve

<!-- index: Design for BUG-jyot (GPU FLIP at 128 for 60 fps), Option C: the GPU FLIP step classifies 8^3 tiles on the GPU each tick (ring distance from particle-holding tiles), runs the dense lattice passes and later the multigrid pressure solve over tile lists by indirect dispatch, keeps every skipped buffer at the dense pass's value, and proves bitwise equality against the dense path on the 64 Dam Break. -->

**Status:** IN PROGRESS · 2026-10-02 · Fable 5.1 (lane) · design for BUG-jyot (GPU FLIP at 128 for 60 fps), Option C. Phase 1 (tile table + lattice passes) is BUG-t7i2 (sparse blocks phase 1), in build: the table and the six cell passes over C are in, the extend stays dense (D-4), measured at 64 and 128. Phase 2a (the solve's lattice passes over each level's own active tiles, D-8) is in build, proven bitwise. Owed: the Phase 2 measure; Phase 2b (rows over tiles). Pointer: GPU_FLIP_STRUCTURAL_OPTIONS.md section 6 (Option C — sparse tiles).
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
- Phase 2b after the measure: rows and the coarsenings over the sets.

## 8. Phasing briefs

### Phase 1 — tile table + lattice passes

**Entry state:** branch `feat/solver-occupied-blocks` at 955e47752; this doc and GPU_FLIP_STRUCTURAL_OPTIONS.md committed.
**Read-back:** sections 3–6 of this doc, `gpu_flip_step.rs` `encode` :526 and `extend` :326, the pocket-gate indirect dispatch :378, `gpu_flip_step.wgsl` :1213–1676, `extent.rs:1202`, ADDING_PRIMITIVES.md (stage-node exemption).
**Deliverables:** `TileTable` and the four kernels; the cell passes over C (the extend stays dense, D-4); `all_tiles` behind `gpu-proofs`; `tiles.retire`; stats word 16 with every call site; the four proofs of section 6; `gpu_scope.py` unchanged unless a new file falls outside its prefixes.
**Gate, positive:** all section 6 proofs green through `scripts/gpu_queue.py -- cargo test --manifest-path ".claude/worktrees/slot-5/Cargo.toml" -p manifold-renderer --features gpu-proofs gpu_flip`; CPU extent proofs green before any GPU run; `cargo clippy -p manifold-renderer -- -D warnings` clean.
**Gate, negative:** the poison proof fails when `particle_distance` is dispatched dense instead of over C (demonstrated once in the phase report, then reverted); `rg -n "poison_inactive"` outside tests returns nothing.
**Acceptance demo (level: measure):** `gpu_flip_frame_perf` (64) and `gpu_flip_speed_measure` (`dam_break(64)`, `dam_break(128)`) per label, `all_tiles` vs sparse, reported as a table of p50 ms before/after per pass; the stats word 16 trace over the 60 frames. Prediction to check against: cell-pass ratios 0.31–0.70 at 64, 0.25–0.49 at 128; extend 0.7–1.0 at 64, ~0.65 at 128.
**Forbidden moves:** CPU readback of any tile buffer in the step; a size rule on n; a narrower extend halo; a second dense code path; `#[ignore]`; timing asserts in correctness proofs; a node param for `all_tiles`; silent fallbacks when a tile count is zero (an empty list is a legal zero-group dispatch).
**Test scope:** `-p manifold-renderer`, filter `gpu_flip`; the extent and stats CPU tests via nextest.

### Phase 2 — the pressure solve over tiles

Opened on Phase 1's measure (the solve was 62% of the tick at 64). Phase 2a, the lattice passes over the solve's own sets (D-8, section 7), is in build with its proofs. Phase 2b, rows and coarsenings over the sets, opens on the Phase 2a measure. Forbidden moves as Phase 1 plus: no change to `MAX_ITERATIONS`, no change to the stop rule.

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
- Stats word 16 = active cell-set tile fraction (section 5).

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
