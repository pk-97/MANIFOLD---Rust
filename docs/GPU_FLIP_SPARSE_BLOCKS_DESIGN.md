# GPU FLIP sparse blocks — occupied 8³ tiles for the dense lattice passes, then the pressure solve

<!-- index: Design for BUG-jyot (GPU FLIP at 128 for 60 fps), Option C: the GPU FLIP step classifies 8^3 tiles on the GPU each tick (ring distance from particle-holding tiles), runs the dense lattice passes and later the multigrid pressure solve over tile lists by indirect dispatch, keeps every skipped buffer at the dense pass's value, and proves bitwise equality against the dense path on the 64 Dam Break. -->

**Status:** IN PROGRESS · 2026-10-02 · Fable 5.1 (lane) · design for BUG-jyot (GPU FLIP at 128 for 60 fps), Option C. Phase 1 (tile table + lattice passes) is BUG-t7i2 (sparse blocks phase 1), in build; Phase 2 (pressure solve over tiles) waits on its measure. Owed: the measure at 64 and 128. Pointer: GPU_FLIP_STRUCTURAL_OPTIONS.md section 6 (Option C — sparse tiles).
**Execution contract:** no size caps, no quality caps, one step per frame, a capped solve is reported in stats never a node error, block list decided on the GPU each tick, never read back, every reader of a skipped block sees a defined value, all GPU through `manifold-gpu`, no new shared state, output bitwise equal to dense.

The occupied-block rule in the contract comes from BUG-l2h3 (SWASH to a live instrument) item .10, closed 2026-10-02 as deferred to BUG-jyot.

Companions: GPU_FLIP_PRESSURE_SOLVE.md (the solver as built), GPU_WHITEWATER_DESIGN.md (reads the step's faces), DECOMPOSING_GENERATORS.md section 1.2 (Specialised solvers are stage nodes).

## 0. The finding that shaped this design

The options doc estimated 8³ tiles active at 0.25–0.36 of the lattice at 64 and named one uncertainty: the extend sweeps need a `band_layers`-wide halo (14 cells at 64, 23 at 128), and if that halo lights most tiles the ratio is nearer 0.7. The CPU count on the dumped Dam Break pressure problems (appendix) answers it: the band-wide halo lights 0.64–1.00 of tiles (mean 0.83 at 64, 0.76 at 128). A one-cell halo lights 0.31–0.70 at 64 (mean 0.46) and 0.25–0.49 at 128 (mean 0.34); the Dam Break sloshes thin water across the whole tank from frame 45 on, so the sparse win on this scene is about 2.2× on the cell passes at 64, 2.9× at 128, not the 3–4× estimated.

So the design does not use one halo. Each pass gets the smallest tile set its stencil allows (section 3, the reach rule), the extend runs its first layer dense so its output is dense-bitwise everywhere, and later layers run over rings that grow one tile every eight layers. Whitewater samples face velocity anywhere in the air (`sample_faces_at_particles_body.wgsl:82`, corner weights at its own spray particles), so a narrower extend halo would change what the audience sees; it stays deferred (section 9).

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

**D-2. Classification on the GPU from the sort, no readback, no atomics.** `tiles_classify`: one thread per tile scans `ranges[c].count` over its box grown by CELL_REACH = 2 cells and writes `tile_near[t]` = the Chebyshev cell distance from the box to the nearest particle-holding cell (0 occupied, 1, 2, or 3 for none within reach). Thread 0 flips the ring halves' parity word first, so the parity is GPU state and a replayed encode stays right. `tiles_rings`: one thread per tile reads `tile_near == 0` over the (2·ring_max+1)³ tile neighbourhood and writes `ring[t]` = Chebyshev tile distance to the nearest occupied tile, or ring_max+1 if none within ring_max. `tiles_lists`: one thread (precedent `pocket_round`) counting-sorts tiles by rank, stable in tile order — rank 0 is C (D-3), rank 1 the rest of rings 0 and 1, rank k ≥ 2 ring k — into `tiles_by_ring`, writes the counts (word 0 = |C|, word k ≥ 1 = tiles with ring ≤ k), the indirect triples `[2·count, 1, 1]` (512 threads per tile, 256 per group) for words 0..=ring_max then the retired list (D-5), and the active-fraction stats word. `ring_max(band) = ceil((1+band)/8)` for the band the step runs with (2 at 64, 3 at 128; derived, not a cap). Rejected: atomics for compaction, because the order must be deterministic for bitwise proofs; a workgroup scan, because the 1-thread builder over T³ ≤ 32³ tiles at 256³ is measured before it is optimised (deferred, section 9); CPU readback, because the occupied-block rule (status line) forbids it.

**D-3. One tile set per pass from its stencil reach, not one halo for all.** Every cell pass in scope reads within 2 cells of a particle-holding cell, so they all share C = {ring ≤ 1 and near ≤ 2}: the tiles whose box lies within 2 cells of a particle. Classifying by cell distance rather than tile ring matters: ring ≤ 1 alone lights 0.562 of tiles at 64 frame 0 (appendix, `halo1+1ring`) where the cell passes need 0.312. An extend layer i fills faces at distance ≤ 1+i, so it runs over ring ≤ r(i) = ceil((1+i)/8) with a read cap of r(i−1). Rejected: the band-wide halo for everything, because it lights 0.83 of tiles at 64 (section 0); a narrower extend halo, because whitewater observes faces beyond it (section 9).

**D-4. Extend layer 1 runs dense; later layers run over rings with a read cap.** A read-capped neighbour read returns the canonical record (wall weight 1, velocity 0 on the p[a]==0 or n planes, else the zero record) for any neighbour in a tile beyond the cap. Because layer 1 writes every record of the face grid, the three face outputs (`a` after extend old, `f` after extend new, `out_faces` after extend spread) are dense-bitwise everywhere and need no clear when a tile retires. Expected gain for extend on this scene: 0–30% at 64, ~35% at 128 (ring ≤ 2 is 0.75–1.0 of tiles at 64, 0.48–0.96 at 128). Rejected: skipping layer 1 in inactive tiles, because then face records in inactive tiles would hold the previous tick's values and whitewater would read them.

**D-5. Retire-clear instead of a dense clear per tick.** The ring table is ping-ponged; a tile with `ring_prev ≤ 1` and `ring > 1` is retired this tick and `tiles.retire` writes the canonical values of the cell buffers that cell passes own: `water = 0`, `phi = 3h`, `rhs = 0`. `allocate` fills the same canonical values once. Rejected: a dense clear per tick, because it costs a full lattice write per buffer, which is the work being removed; leaving stale values, because dense readers (`pocket_pin` → `solve_water`, the solver's `is_water`, rows, `coarsen_water`) would see phantom water.

**D-6. The oracle lever is `StepParams.all_tiles`, test-only.** With it set, `tiles.rings` writes ring = 0 for every tile, so every pass runs dense through the same kernels and the same list machinery. It is settable only behind `cfg(feature = "gpu-proofs")` and never a node param. Rejected: keeping the old dense kernels as a second code path, because two paths drift.

**D-7. Phase 2 (the solve) is sketched here and decided at its own gate.** Section 7 fixes the shape; the coarse-level ring rule and the GPU-written `partial_count` are marked VERIFY-AT-IMPL. Rejected: building the solve in the same phase, because its proof (iteration-count equality and bitwise residuals) is a separate gate and the Phase 1 measure decides whether the solve's share justifies it.

## 3. The tile table

Buffers (all `u32`, in `TileTable` beside `LatticeBuffers`, counted in `scratch_bytes` and the extent hold; bindings 27–32):

| Buffer | Length | Writer | Readers |
|---|---|---|---|
| `tile_near` | T³ | `tiles_classify` | `tiles_rings`, `tiles_lists` |
| `tile_ring` | 2·T³, halves by the parity word | `tiles_rings` | every sparse pass (read cap), `tiles_lists` |
| `tiles_by_ring` | T³ | `tiles_lists` | every sparse pass (thread → tile) |
| `tile_counts` | ring_max+4: |C|, ring ≤ k for k = 1..=ring_max+1, retired count, parity | `tiles_lists` (parity: `tiles_classify`) | proofs |
| `tile_args` | 3·(ring_max+2) indirect triples (C, each ring cap, retired) | `tiles_lists` | `dispatch_compute_indirect` |
| `tiles_retired` | T³ | `tiles_lists` | `tiles_retire` |

`tile_ring` and `tile_counts` carry state across steps, so they are shared buffers zero-filled once; the table is rebuilt when the lattice or `ring_max` changes.

Thread mapping in every sparse pass:

```wgsl
let tile = tiles_by_ring[gid / 512u];
let local = vec3<u32>(gid & 7u, (gid >> 3u) & 7u, (gid >> 6u) & 7u);
let cell = tile_origin(tile) + local;
if any(cell >= lattice) { return; }
```

A face thread owns record `cell` plus, on each axis a where `cell[a] == n−1`, the wall records with p[a] = n (up to 8 records in a corner), so the (n+1)³ face grid is covered by the n³ cell mapping.

The reach rule, per pass in Phase 1 scope:

| Pass | Stencil reach d from a particle-holding cell | Tile set |
|---|---|---|
| `particle_distance` | 2 (φ from particles in the 3³ neighbourhood, written for cells within 2) | C |
| `particles_to_faces` | 1 | C |
| `water_from_phi`, `phi_into_solids` | 2 | C |
| `divergence` | water cells only | C |
| `density_source` | water cells only (writes 0 elsewhere → canonical) | C |
| extend layer 1 (old, new, spread) | dense | all |
| extend layer i ≥ 2 | 1+i | ring ≤ r(i), read cap r(i−1) (none for i = 2) |

Out of scope and dense in Phase 1: `face_gravity`, solids (`open_fractions`, `solid_face_velocity`), pockets, the pressure and density solves, `subtract_pressure`, `constrain_solid_faces`, `faces_to_particles`, and the sort.

Committed signatures (Rust):

```rust
pub(crate) const TILE: u32 = 8;
pub(crate) const CELL_REACH: u32 = 2;
pub(crate) fn tile_counts(cells: [u32; 3]) -> [u32; 3];       // ceil(n_a / TILE)
pub(crate) fn ring_max(band: u32) -> u32;                     // ceil((1 + band) / TILE)
pub(crate) fn tile_scratch_bytes(cells: [u32; 3], ring_max: u32) -> u64;
pub(crate) fn scratch_bytes(cells: [u32; 3], slots: u64, ring_max: u32) -> u64;
struct TileTable { ring_max, near, ring, by_ring, counts, args, retired }
fn encode_tiles(enc, pipes, params, t: &TileTable, ranges, capped, tally);
```

WGSL entries: `tiles_classify`, `tiles_rings`, `tiles_lists`, `tiles_retire`; dispatch labels `gpu_flip.step.tiles.classify`, `.tiles.rings`, `.tiles.lists`, `.tiles.retire`. The sparse passes keep their entry names; `Params` carries `all_tiles`, `ring_cap`, `ring_max`, and the bindings gain `tiles_by_ring` and `tile_ring`.

## 4. The defined-value rule

Every lattice buffer, after every pass, holds the dense pass's value in every tile outside that pass's tile set, or is read only through a ring cap that returns the canonical record. Concretely:

- `water`, `phi`, `rhs`: written over C each tick; canonical (0, 3h, 0) elsewhere by `allocate` and `tiles.retire`. Dense readers see dense values.
- `a`, `f`, `out_faces`: dense-bitwise by D-4.
- `s`, `v`, `b`, `corners`, `pressure`, `pocket*`: owned by dense passes in Phase 1, unchanged.
- Intermediate extend layers: read only through the cap; a capped read of a record a dense extend would have filled with a non-canonical value cannot happen, because layer i−1 filled every record within distance i of the liquid and all of those lie within ring r(i−1) (a record at cell distance ≤ i from a particle-holding cell is in a tile at Chebyshev tile distance ≤ ceil(i/8) ≤ r(i−1)).

Machine checks:

1. The bitwise proof (section 6) over 60 Dam Break frames.
2. A NaN-poison proof: the test-only entry `poison_inactive` writes NaN into every cell and face record of tiles with ring > 1 (cells) or beyond r(i) (faces, per layer, via the same cap) after `tiles.lists`; the step then runs; particles and `out_faces` must be bitwise equal to the unpoisoned run. A read that escapes its cap turns into NaN in the output and fails. `rg -n "poison_inactive" crates/ -g "*.rs"` outside `tests/` must return nothing (a test asserts it).
3. Extent CPU proofs: `gpu_flip_*_cover_every_dispatch` extended to the four tile dispatches; `tile_scratch_bytes` is counted in `scratch_bytes` and the extent hold.

## 5. Stats

Word 16: the active cell-set tile fraction, `|C| / T³` as f32 bits, written by `tiles_lists` (`capped` bound at the solver words). `LIQUID_STATS_WORDS` 16 → 17, `SOLVER_WORDS` 6 → 7 (the `tally` offset is derived, not a literal). Every call site in the audit table is touched in the same commit; `liquid_stats.rs` gains the readout. Re-derive the inventory before editing: `rg -n "LIQUID_STATS_WORDS|SOLVER_WORDS" crates/ -g "*.rs" -g "*.wgsl" #grep-ok`.

## 6. Proof plan

| Proof | Oracle | Pass condition |
|---|---|---|
| `gpu_flip_tiles_match_the_cpu_classification` | CPU near/rings from the sort's `ranges` read back once in the test (the test reads back; the step never does) | `tile_near`, `tile_ring`, `tiles_by_ring` order, `tile_counts`, `tile_args` triples, the retired list and the parity flip equal to the CPU model on `dam_break(64)` frames 0, 30, 60; `all_tiles` gives near 0 and ring 0 everywhere |
| `gpu_flip_sparse_step_matches_dense_bitwise` | the same kernels with `all_tiles = true` in the same run | 64 Dam Break, 60 frames, per-frame hash of particles and `out_faces` equal; the HEAD-dense hash from `gpu_flip_frame_perf` reported once in the phase report (not pinned, because it moves with any kernel change) |
| `gpu_flip_sparse_step_survives_poison` | NaN poison (section 4) | particles and `out_faces` bitwise equal to the unpoisoned sparse run over 10 frames |
| `gpu_flip_tile_extent_covers_every_dispatch` (CPU) | extent model | every tile dispatch covered; scratch bytes include the table |
| Existing `gpu_flip_` and `face_grid_tests::` proofs | unchanged | green |

Stats proof: word 16 at frame 0 of `dam_break(64)` equals the CPU count's halo1 fraction for that frame within one tile of 512 (the fixture's frame 0 is the standing column: 0.312).

## 7. Phase 2 sketch — the pressure solve over tiles (VERIFY-AT-IMPL)

- Level 0 `smooth`, `residual`, `apply`, `update`, the dot products: over C.
- Coarse levels: a coarse tile of 8³ coarse cells covers 16³ fine cells; its ring is the minimum over its children, which makes the coarse 1-ring a superset of the fine set's image. Each level carries its own `tiles_by_ring`, counts and triples, built by `tiles.lists` in the same 1-thread kernel (it loops levels).
- `partial_count` and `dot_finalize`'s `Params.color` become GPU-written per tick (`count_le[1]·2` groups per level); `armed_groups` is rewritten per tick from `tile_args` instead of once.
- Vectors are canonical 0 beyond C; `init_main` already masks on `water`, which is canonical by D-5.
- Proof: iteration count and `tally` words equal to the dense solve per frame over the 60-frame run, pressure bitwise equal over C.
- Rows (`prepare`) stay dense in Phase 2a; moving row assembly to C is Phase 2b after the measure.

## 8. Phasing briefs

### Phase 1 — tile table + lattice passes

**Entry state:** branch `feat/solver-occupied-blocks` at 955e47752; this doc and GPU_FLIP_STRUCTURAL_OPTIONS.md committed.
**Read-back:** sections 3–6 of this doc, `gpu_flip_step.rs` `encode` :526 and `extend` :326, the pocket-gate indirect dispatch :378, `gpu_flip_step.wgsl` :1213–1676, `extent.rs:1202`, ADDING_PRIMITIVES.md (stage-node exemption).
**Deliverables:** `TileTable` and the four kernels; `ring_cap` on the cell passes and the extend layers ≥ 2; `all_tiles` behind `gpu-proofs`; `tiles.retire`; stats word 16 with every call site; the four proofs of section 6; `gpu_scope.py` unchanged unless a new file falls outside its prefixes.
**Gate, positive:** all section 6 proofs green through `scripts/gpu_queue.py -- cargo test --manifest-path ".claude/worktrees/slot-5/Cargo.toml" -p manifold-renderer --features gpu-proofs gpu_flip`; CPU extent proofs green before any GPU run; `cargo clippy -p manifold-renderer -- -D warnings` clean.
**Gate, negative:** the poison proof fails when `ring_cap` on `particle_distance` is set to 0 (demonstrated once in the phase report, then reverted); `rg -n "poison_inactive"` outside tests returns nothing.
**Acceptance demo (level: measure):** `gpu_flip_frame_perf` (64) and `gpu_flip_speed_measure` (`dam_break(64)`, `dam_break(128)`) per label, `all_tiles` vs sparse, reported as a table of p50 ms before/after per pass; the stats word 16 trace over the 60 frames. Prediction to check against: cell-pass ratios 0.31–0.70 at 64, 0.25–0.49 at 128; extend 0.7–1.0 at 64, ~0.65 at 128.
**Forbidden moves:** CPU readback of any tile buffer in the step; a size rule on n; a narrower extend halo; a second dense code path; `#[ignore]`; timing asserts in correctness proofs; a node param for `all_tiles`; silent fallbacks when a tile count is zero (an empty list is a legal zero-group dispatch).
**Test scope:** `-p manifold-renderer`, filter `gpu_flip`; the extent and stats CPU tests via nextest.

### Phase 2 — the pressure solve over tiles

Opens after Phase 1's measure. Entry state: Phase 1 landed. Deliverables per section 7; the gate adds iteration-count and `tally` equality per frame and pressure bitwise over C. Forbidden moves as Phase 1 plus: no change to `MAX_ITERATIONS`, no change to the stop rule.

## 9. Deferred

- **Narrower extend halo** (R = travel+2 cells, the engine's `_extrapolateFluidVelocities` on active blocks): changes face values beyond R that whitewater spray samples. Reopens only with a ruling on what whitewater may read beyond the halo.
- **Frontier-only extend** (sweep only records at the current layer's frontier): a different algorithm with its own proof; after the measure.
- **Workgroup scan for `tiles.lists`** if the 1-thread builder measures over 0.3 ms at 256³.
- **Solids, pockets, forces, `subtract_pressure`, `constrain_solid_faces`, `faces_to_particles` over tile sets:** after Phase 2's measure says what is left.
- **Whitewater grid atoms over the same lists:** GPU_WHITEWATER_DESIGN.md's call once the table exists.

## 10. Decided

- 8³ tiles, partial edge tiles, no size rule (D-1).
- Classification from the sort's bin counts by cell distance (`tile_near`), GPU-held parity, deterministic 1-thread list builder, no atomics, no readback (D-2).
- Per-pass tile sets from stencil reach; one set C = {ring ≤ 1, near ≤ 2} for every cell pass (D-3).
- Extend layer 1 dense, later layers ringed with a read cap; face outputs dense-bitwise (D-4).
- Retire-clear of `water`, `phi`, `rhs`; canonical fill at allocation (D-5).
- `all_tiles` is the oracle, test-only (D-6).
- Phase 2 gated on Phase 1's measure (D-7).
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
