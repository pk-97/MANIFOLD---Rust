# Water tile pool — the fine lattice stored in 8³ tiles

<!-- index: Design for S3 of the water field wave (BUG-7wna, sparse tile storage): GPU FLIP's fine-lattice fields move from dense boxes into a pool of 8³ tiles allocated on the GPU every tick, read through the storage helpers, with defined values for absent tiles. Pool layout, the absent-read rule, the GPU allocator and exhausted word, the pass and consumer conversion, and the job split. -->

**Status:** DESIGN, building (S3 of `WATER_FIELD_WAVE_PLAN.md`). Companions: `GPU_FLIP_SPARSE_BLOCKS_DESIGN.md` (the tile table and the cell set C this builds on), `GPU_FLIP_END_GAME_DESIGN.md` D4 (memory follows the water), D15 (clean cut).

## 1. What changes

Today every fine-lattice field is a dense box: cells `n³` (water, φ, rhs, pressure, pockets, solve mask) and records `(n+1)³` (the face grids `g a b f s v`, the published `faces`, the solid corners). Since S1 and S2 every kernel indexes them through the storage helpers (`manifold-water-liquid/src/primitives/shaders/tile_storage.wgsl`, `tile_grid.wgsl`). S3 replaces the helpers' bodies: a field is stored per 8³ tile in a pool, a tile is allocated on the GPU while the water needs it and freed when it does not, and a read of a tile that is not allocated returns the field's defined value. Memory follows the water; with the box still in place (S4 removes it) the saving at 64 and 128 in a full tank is small, and the point is that S4's unbounded domain costs only what is wet.

Not in the pool: the coarse multigrid levels (an eighth of the fine lattice per level, rebuilt every step; they stay dense `ts_grid_*` boxes), the solid distance and the obstacle lattices built by other nodes (S4b moves them), the particle arrays, and the tile table itself.

## 2. Layout

- **Tiles.** 8³ cells (`TS_TILE`), the box tile grid `T_a = ceil(n_a / 8)` as today (`ts_tile_dims`). S4 replaces the box grid with world tile keys; only `ts_slot_of` changes then.
- **Slot table.** `slot_of: array<u32>`, one word per box tile: the tile's pool slot, or `TS_NONE` when absent. Written only by the allocator (section 4).
- **Pool.** Capacity `P` tiles. A pooled field has `P · 512 + 8` elements: tile slot `s` holds its 512 cells at `s · 512 + local(p)`, `local(p) = (p.x & 7) + 8 · ((p.y & 7) + 8 · (p.z & 7))`, and the last 8 elements are the **canonical block** (section 3).
- **Records live with their cell.** Record `p` holds cell `p`'s three low faces (as today), so it is stored at cell `p`'s index. A record with some `p[a] == n[a]` (the far boundary) belongs to no cell and always reads canonical.
- **Inverse table.** `tile_of: array<u32>`, one word per slot: the box tile it holds (`TS_NONE` when free). Written by the allocator beside `slot_of`.
- **Helpers.** `ts_cell_at(p)` and `ts_face(p)` return the pool index, or the canonical index when the tile is absent or the record is past the lattice. `ts_cell_coord(idx)` and `ts_face_coord(idx)` keep their callers: `ts_origin(tile_of[idx >> 9]) + local_coord(idx & 511)` for a pool index (never called on a canonical index). `ts_list_cell(tile, gid)` returns the pool index of the list tile's cell. `ts_cell_count()` and `ts_record_count()` stay the box's counts for passes still over the box (section 5); storage sizes come from `ts_pool_len()` (`P · 512 + 8`).
- **Includer contract.** A shader including `TILE_STORAGE` declares `ts_slot_of: array<u32>` and `ts_tile_of: array<u32>` (read) at bindings of its choosing and defines `fn ts_pool_tiles() -> u32` beside `ts_cells()`. Codegen atoms that read pooled fields (whitewater) take the slot table as an input named `tiles` and the include `tile_pool_atom.wgsl` defines the same helpers on `buf_tiles` and the atom's `pool_tiles` param, the second include allowed to name a buffer (precedent: `marching_cubes_common.wgsl` reads `buf_levelset`).

## 3. Absent reads: the defined-value rule (I1)

An absent tile reads its field's defined value; writers never touch it.
- Cell fields: the canonical index is `P · 512` (mask 0). Water 0, φ `3h` (air three cells out), rhs 0, pressure 0, pocket and labels 0, solve mask 0.
- Record fields: the canonical index is `P · 512 + mask(p)`, `mask` bit `a` set when face `a` of record `p` exists (`face_exists`) and lies on a box wall (`p[a] == 0` or `p[a] == n[a]`). The 8 canonical records are `canonical_face(mask)`: every face absent (velocity 0, weight 0) except the masked axes, closed (velocity 0, weight 1). The same values for every record field (`g a b f s v` and `faces`): absent tiles hold no water, so every pass that writes those fields would leave an absent tile's faces absent and its walls closed. `s` (open fractions) is the exception if a solid can sit in an absent tile: its canonical is fully open (1) on existing faces, closed walls 0; check `open_fractions` and set it per its own formula.
- The canonical block is written once per pool build by `pool_canon` (one thread per field element of the block), and never again. A pass that would write it is a bug the poison proof catches (section 6).
- Writers: every pass writes only cells (and their records) of tiles on its list. A list never holds an absent tile.

## 4. The allocator (GPU, no readback)

Per active step, after `tiles_rings`:
1. `tiles_free` (one thread per box tile): a tile with a slot whose ring is past `ring_max` pushes its slot onto the free stack (`free_list`, `free_top` atomic) and writes `TS_NONE` into `slot_of[tile]` and `tile_of[slot]`.
2. `tiles_alloc` (one thread per box tile): a tile with ring ≤ `ring_max` and no slot takes one: `k = atomicAdd(&pop, 1)`, slot `free_list[top_after_free - 1 - k]` when `k < top_after_free`, else the tile stays absent and `exhausted` is set. It writes `slot_of[tile]` and `tile_of[slot]` and records the tile on the `fresh` list.
3. `tiles_settle` (one thread): `free_top = top_after_free - min(pop, top_after_free)`, `pop = 0`; writes the `fresh` list's indirect triple.
4. `tiles_fresh` (512 threads a fresh tile, indirect): every pooled field's cells and records of the tile set to the canonical values (what `tiles_fill` does today for the whole box, now per new tile).
5. `tiles_lists` as today, plus the list A (ring ≤ `ring_max` and allocated) and its triple; C keeps its rule and is filtered to allocated tiles.

Slot numbers depend on atomic order; no value does: reductions are indexed by box tile (the pressure partials, `2 · tile + half`), the sort keys on box cells until S3.3. So runs stay bitwise reproducible.

- **Allocated set.** A = tiles with ring ≤ `ring_max` (the extension band's reach, 2 at 64 and 128). C ⊆ ring ≤ 1 ⊆ A. Every read a pass over A or C makes lands in A or reads canonical at A's far edge, at least `8 · ring_max - CELL_REACH` cells from any particle.
- **Capacity and the exhausted word.** `P = min(T³, POOL_BUDGET_BYTES / tile_bytes)`, `tile_bytes` the pooled fields' bytes per tile, `POOL_BUDGET_BYTES` the D4 live budget for the lattice (1.5 GiB: 128 live under 2 GB total, the rest particles and the render). At 64 and 128 in today's box `P = T³`. `exhausted` is a word in `tile_counts`; S3.4 makes sources stop and the card say why.
- **First step after a pool build:** every slot free, every tile absent; the first `tiles_alloc` allocates A and `tiles_fresh` fills it. `tiles_fill` (the dense fill) and the dense `filled` flag are deleted.

## 5. Passes and consumers

- **C passes** (particle_distance, particles_to_faces, phi_into_solids, water_from_phi, divergence, density_source): unchanged lists, indices through the helpers.
- **Dense passes become A passes** (512 threads a tile over list A, indirect): the extend layers (the gated replay segments keep their gate, over A), face_gravity, open_fractions, solid_face_velocity and solid_extrapolate, pocket seed/check/pin/condition/mean, subtract_pressure, constrain, the density solve's dense passes, sheet distance, remove_crowded, the solver's `rows` and `init_main`. A record pass over A visits each allocated cell's own record; the far-boundary records are canonical and never written.
- **Line passes** (pocket rounds sweep whole lines): stay one thread per line over the box; a step along the line through an absent tile reads canonical (no water) and writes nothing there.
- **Retire.** `tiles_retire` stays for tiles leaving C but staying in A (C's canonical rule); tiles leaving A are freed instead.
- **Outputs.** The step's `faces` and `distance` outputs are pooled buffers; the step adds output `tiles` (the slot table, `T³` words) and scalar `pool_tiles`. Readers index through the pool: `node.liquid_state` (its faces copy becomes a pooled buffer plus its own slot-table copy, both copied together), whitewater's distance and packed faces (`whitewater_fused.wgsl:87`, `:1589`), `face_sample_component_body.wgsl:26` (the substep history), `node.liquid_frame`'s face copies (deleted in S3.6 with the frame-history face copies if no reader remains).
- **Extent.** Every pooled field holds `ts_pool_len() · element` bytes; the step's hold adds the slot table, the free list and the fresh list.

## 6. Proofs

- **S3.1 (layout, every tile allocated):** with `ring_max` forced past the box (A = every tile, a test-only lever like `all_tiles`), particles and `faces` match the pre-S3 step bit for bit over 120 Dam Break steps at 64 (`gpu_flip_step` CPU-reference tests plus a two-commit capture compare, run by the lead).
- **Poison:** NaN into every free slot's cells and records after `tiles_fresh` (test-only lever, as today's `poison_inactive`); a read through a stale slot shows in the particles or `faces`. A second check reads the canonical block back after a run: unchanged, so no pass wrote it.
- **S3.2 (allocation follows the water):** Dam Break inside the volume test's tolerance, `matter_look_volume_drift` passes, memory reported at 64 and 128.

## 7. Jobs

| Job | Worker | What |
|---|---|---|
| S3.0 | sonnet ×3 | Narrow Band and the interior field deleted before S3.1 (a whole dense subsystem the pool would otherwise carry). Done. |
| S3.1a | sonnet | Helpers' pool bodies, slot table, pool sizing and canonical block, allocator with A = every tile, the step's GS shaders (C and A lists) |
| S3.1b | sonnet | Pressure, Lentine, bodies, sheeting and sheet distance on the pool (slot binding, A lists for their dense loops) |
| S3.1c | sonnet | Consumers: liquid_state, whitewater, face_sample_component, liquid_frame |
| S3.2 | sonnet | A = ring ≤ ring_max: free, alloc, fresh fill; P from the budget; exhausted word |
| S3.3 | sonnet | Sorter keyed on (slot, local cell) for FLIP and Matter; extend on the frontier only |
| S3.4 | sonnet | Particle pool grows with the water; every particle pass dispatches from the live count |
| S3.5 | sonnet | Full pool pauses sources; the card says why |
| S3.6 | haiku | Deletions (plan S3 check 4) from the lead's list |
