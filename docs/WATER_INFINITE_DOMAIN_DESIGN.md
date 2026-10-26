# Water infinite domain — world tiles, Bounds and cell size

<!-- index: Design for S4, S4b and M3 of the water field wave (BUG-zdiuw, GPU FLIP infinite domain): the tile pool keyed by world tile through a GPU hash table rebuilt every step (ordered linear probing, deterministic), 27-slot neighbour rows so hot kernels never hash, world f32 positions, Bounds faces as ordinary solids or removal planes, the kill height with Bounds off, cell size in metres, live Bounds moves, the coarse multigrid levels on world tiles, the S4.5 deletion list and the job split. -->

**Status:** DESIGN (S4, S4b and M3 of `WATER_FIELD_WAVE_PLAN.md`). Builds on `WATER_TILE_POOL_DESIGN.md` (the pool, built) and on S3.3 to S3.5 (sort by pool index, tiles marked from particles, live-count dispatch, full pool pauses sources: `WATER_PARTICLE_POOL_DESIGN.md`, being written). Rulings: plan I1 to I3; end-game D4, D14, D15, D18. Anchors are from the working tree of 2026-10-27 with S3.2 written and S3.3 in flight; S3.3 to S3.5 and SP move them, so the lead re-anchors each job at handoff.

**Peter's calls** (the card or behaviour on stage):
1. Card: Resolution becomes Cell Size in metres, default 6.25 cm (the Dam Break's 4 m / 64), range 1 mm to 1 m. Changing it restarts the water, as Resolution does today.
2. Card: a Bounds switch, off by default, and six face rows (Left, Right, Floor, Ceiling, Back, Front), each Wall or Open, shown when Bounds is on. Size X/Y/Z show only with Bounds on and lose their 0.5 to 20 m limit.
3. Switching Bounds on or off restarts the water. Moving, resizing and Wall/Open switches are live and never restart it.
4. Bounds off in a scene with no solid at all: water is removed once it falls a drawn radius below its lowest source (Initial Fill or Inflow).
5. When memory is full, a particle whose tile gets no memory is removed at once and counted on the card ("Water memory is full: N particles removed."), instead of hanging still in the air.
6. A tank whose faces sit on whole cells keeps its walls mid-cell, as today. A tank placed between cells has its walls exactly where it is placed, up to one cell from today's.

**Lead's calls:**
- Positions stay f32 world metres, not tile-relative (section 3). This departs from the plan's S4.1 line and from D5's 16-bit storage.
- SP's fused bottom of the V-cycle assumes small dense coarse boxes; section 7 makes the coarse levels sparse, so S4.1c replaces it. SP's warm start survives as built.
- Until S4b the surface, whitewater and the particle frame keep the Water item's box. Water outside it simulates but is not drawn. S4b removes that box.

## 1. What changes

Today the slot table has one word per box tile. Every kernel counts cells from the box minimum, and the walls, the open band, the boundary clamp, the sort's clamp, the emission sites and the solver's coarse levels all assume the box. S4 keys the pool by world tile through a hash table, so a tile exists wherever the water is and nowhere else. The box survives only as Bounds: six planes that are solids (Wall) or removal planes (Open). Memory follows the water. The budget bounds how much water there is, never where it goes.

## 2. Keys and lookups (decision 1)

**Decision: a GPU hash table keyed by world tile coordinate, rebuilt every step from the particles, plus a row of 27 neighbour slots per pool slot.** A window moving over a fixed table (toroidal addressing) costs one read per lookup, but it caps how far water spreads, so it is a box again and fails S4b's floor check. The neighbour rows take the hash out of the hot kernels, which removes the window's one advantage.

**World cells.** Cell p (i32 per axis, any sign) is centred on p·h: it spans [(p − ½)·h, (p + ½)·h). The lattice origin is −h/2 on every axis, a constant. A tile is 8³ cells: tile t = p >> 3 (floor for negatives too), local index (p & 7) as today. Every world tile is whole: no partial edge tiles. The half-cell origin puts a wall on a whole multiple of h at a cell centre, where FLIP Fluids' grid puts walls (today `FlipSolverGrid` gets that with its 1.5-cell offset). Matter's nodes sit on p·h, the same points.

**Table** (one `tiles` buffer, published by the step as today):
- words [0, H): keys. Key = pack(t) + 1, pack = x in 11 bits, y in 9, z in 11, each wrapped in two's complement; 0 is empty. So 2048 × 512 × 2048 tiles fit before a wrap: 1 km × 256 m × 1 km at 6.25 cm cells, 164 m × 41 m × 164 m at 1 cm.
- [H, 2H): the slot of each entry, TS_NONE when none.
- [2H, 2H + 4P): per slot, its tile coordinate (x, y, z as i32) and its entry index.
- [2H + 4P, 2H + 31P): per slot, the slots of its 27 neighbour tiles, k = (dx + 1) + 3(dy + 1) + 9(dz + 1), k = 13 itself, TS_NONE where absent.
- H = the power of two at or above 4P: 65,536 at P = 8,456. The table is 1.5 MB; its build scratch (ring, near and coordinate per entry, the seed list) about 1.6 MB more.

**Hash and probing.** h(key) = (key · 2654435769) >> (32 − log2 H), Knuth's multiplicative hash. Ordered linear probing (Amble and Knuth 1974, "Ordered hash tables"): along a probe run keys sit in decreasing order, so a lookup stops at the first smaller key, and the table's layout is the same whatever order keys arrive in. The insert is Shun and Blelloch's phase-concurrent version (SPAA 2014, "Phase-concurrent hash tables for determinism"): walk from h(k); an empty word or a smaller key is swapped for k by `atomicCompareExchangeWeak` and the displaced key walks on; an equal key stops. Inserts and lookups never share a pass, so a lookup never meets a key in motion. The layout must be deterministic because slot numbers order the sort, and so the order of every sum over a cell's particles.

**Build per active step** (`world_tiles.wgsl`, shared with Matter; an inactive clock slot runs none of it and keeps the table, as today):
1. `wt_clear` (H threads): keys 0, every payload to its start value.
2. `wt_seed` (one thread a live particle): insert the particle's own tile.
3. `wt_seeds` (H threads): every key into the seed list by atomic append (order is free: each seed's work is order-independent), its count into the next pass's indirect triple.
4. `wt_dilate` (125 threads a seed at ring_max 2, (2·ring_max + 1)³): insert every tile within ring_max.
5. `wt_rings` (same shape): look each up, `atomicMin` the Chebyshev tile distance into its ring, and write its world coordinate, first writer by compare-exchange. A writer finding a different coordinate has met an alias (two live tiles a span apart) and sets the alias bit of the exhausted word.
6. `wt_near` (one thread a live particle): `atomicMin` the Chebyshev cell distance from the particle's cell into each tile within CELL_REACH cells (at most 8), exactly as S3.3's `tiles_mark`. C = ring ≤ 1 and near ≤ CELL_REACH, as `rank_of` (`tiles.wgsl:345`).
7. `wt_carry` (P threads): a held slot looks its tile up. Found with ring ≤ alloc_ring, the entry takes the slot back; otherwise the slot is freed.
8. Free and allocate by prefix scan (`manifold-water-liquid/src/primitives/prefix_scan.rs`): freed slots are pushed in slot order; entries wanting a slot pop in table order, ring ≤ 1 first, then rings 2 to alloc_ring. An entry finding the stack empty stays absent and sets the exhausted word. This is S3.2's priority (`tiles.wgsl:107-165`), now parallel, with the same result on every run.
9. `wt_slots` (P threads): coordinates and entry per slot, the neighbour rows (27 lookups a slot), the rank per slot, the lists by rank, retired (held slots in C last step and not now), fresh (newly held), and the indirect triples at today's offsets (`retired_args_offset`, `allocated_args_offset`, `fresh_args_offset` in `gpu_flip_step/tiles.rs:151-170`). Lists are built by atomic append.

Rule for every reduction: partials are indexed by slot, never by list position, and summed in slot order (the pressure's `2 · tile + half` becomes `2 · slot + half`). Then list order changes no result, and runs stay bitwise reproducible.

No single-thread pass remains. Today `tiles_allocate` and `tiles_lists` loop over every box tile on one thread (`tiles.wgsl:118`, `:361`); that would not scale to an unbounded set. The build is about 20 dispatches a step against 7 today, each sized by the particles, the table (H) or the pool (P), never by space.

**Lookups** (the read side, `world_tiles_read.wgsl`):
- `ts_cell_at(p)`, `ts_face(p)`, `ts_cell_stored(p)` and `ts_record_stored(p)` keep their names and meaning: the pool index, or the canonical element when the tile is absent. With no box there are no records past the lattice and no wall masks, so a record is always its cell's, `ts_face` is `ts_cell_at`, and the canonical block is one element: `CANON_ELEMENTS = 1`, `pool_len(P) = 512·P + 1`.
- Each finds the slot of tile t through a home. `ts_home(idx)` stores, in private variables, the slot and tile of any pool index. Then: t is the home tile, no read; t is a neighbour of it, one read of the home's neighbour row; anything else, or no home set, a hash lookup (the hash, then about 1.2 probes at load ¼, ½(1 + 1/(1 − α)) for ordered probing).
- A list kernel calls `ts_home(idx)` once after `ts_list_cell`; a particle kernel calls `ts_home(ts_cell_at(cell))` once, its one hash. After that a neighbour read costs nothing inside the tile and one cached read across a tile face; a ±1 step leaves the tile one time in eight. A kernel that forgets `ts_home` is still right, only slower.
- `ts_cell_coord(idx)` = the slot's tile coordinate · 8 + local. `ts_list_cell(slot, gid)` = slot · 512 + (gid & 511): lists hold slots.
- Includer contract: `ts_pool_tiles()` (P), `ts_table_entries()` (H) and `ts_table_word(i)`.
- Gone: `ts_cells`, `ts_records`, `ts_cell_count`, `ts_record_count`, `ts_tile_dims`, `ts_tile_count`, `ts_origin`, `ts_slot`, `ts_tile_of_cell`, `ts_wall_mask`, `ts_box_cell`, `ts_box_record`, `ts_dense_cell`, `ts_corner`, and the mask argument of `ts_canon`. `TILE_GRID` (`ts_grid_*`) stays for the arrays that are still dense boxes (whitewater until S4b, Lentine, `liquid_cells`).
- The read side lives in `manifold-node-engine` (`scene/world_tiles.rs`), because the scene renderer marches the field in S4b and render and water do not depend on each other. The write side (the build passes) lives in `manifold-water-liquid`.

**Full table, full pool, alias.** Pass 4 inserts only while fewer than H/2 keys are held (an atomic count of keys placed in empty words), so band tiles can never crowd out the water's own tiles, inserted in pass 2, and probe runs stay short. Pass 2 alone fills the table only when the water's tiles outnumber 4P, four times what the pool holds; an insert then walks the whole table, drops its key and sets the exhausted word, so the GPU never hangs. With the pool short, ring ≤ 1 gets slots before ring 2, so the extension band goes before the water's tiles. A particle whose own tile still gets no slot is removed in the move and counted in a stats word (Peter's call 5); sources stop as S3.5 rules. An alias is reported the same way, never silent.

**Past the old box.** Tiles exist wherever particles are; nothing bounds where. Water poured over a ledge falls past where the box ended and spreads on a floor (S4b's check), as soon as the surface is on world tiles too.

**Cost of a lone droplet.** A tile with water alone costs 125 tiles at ring 2 and 27 at ring 1, at 190 KB a tile (372 bytes a cell). A splash of droplets in separate tiles fills the 1.5 GiB pool long before the water does; the ring priority then drops the bands first. If memory reports show bands dominating, the lever is a second pool for band tiles holding only the face fields the band passes use. Not in S4.

## 3. Positions (decision 2)

**Decision: a particle keeps its f32 world position in metres (`FluidParticle` unchanged). Kernels count world cells from the fixed lattice origin.** Reason: within about a kilometre of the world origin the precision is finer than anything visible, and tile-relative storage would change the layout every particle reader decodes (33 shaders, the display blend, the CPU readers) for no stage case. D5's 16-bit tile offset stays the memory item it is, behind BUG-0cbzb (the default has no long-run agreement gate).

**Precision.** A stored f32 at distance d from the origin moves in steps of 2^(⌊log2 d⌋ − 23): 7.6 µm at 64 to 128 m, 61 µm at 512 m to 1 km, 1 mm at 8 to 16 km. Water slower than half a step per substep stops: at 60 steps a second that is 0.2 mm/s within 128 m, 1.8 mm/s at 1 km and 29 mm/s at 10 km (four substeps a frame make it four times that). A river followed by the camera at 2 m/s for a ten-minute set ends 1.2 km out, still clean. Beyond a few kilometres slow water creeps in visible steps; S4 refuses nothing for it.

**Readers that change.** Only those that turn a position into a box cell: `(x − box_min) / h` becomes `(x − origin) / h`, with no clamp, and cell coordinates go signed.
- FLIP step (S4.1b): `tiles.wgsl:285` (the mark's bin), `transfer.wgsl:54`, `particle_distance.wgsl:18, :45-46`, `density.wgsl:64`, `move.wgsl:270-272` and the write at its end, `emit.wgsl:20, :45, :67, :73, :106`, `forces.wgsl:99`, `solids.wgsl:337-366`.
- Sort and fill (S4.1e): `sort_particles_into_cells.wgsl:94` (`bin_of`'s clamp goes for the pooled key; dense users keep theirs until S4b and M3), `liquid_fill_body.wgsl:89-99, :154`.
- Sheeting and bodies (S4.1d): `gpu_flip_sheeting.wgsl:179` (`local`), `gpu_flip_bodies.wgsl:36, :157-158`.
- Matter (M3): `matter_to_grid.wgsl:114-117`, `grid_to_matter_body.wgsl:90-93, :196`, `matter_grid_update_body.wgsl:100`, `matter_body_reaction_body.wgsl:120`, `matter_stats.wgsl:147`, `matter_fill_body.wgsl:97`.
- Surface, whitewater and render (S4b): `surface_weights.wgsl:58-59, :121`, `particle_volume_body.wgsl:62-106`, `carry_liquid_into_solids.wgsl:32-90`, `ww_grid_position` (`whitewater_common.wgsl:37`) and its callers (`dust_potential`, `inside_turbulence_potential`, `retype_whitewater`, `sample_faces_at_particles`, `spawn_whitewater`, `turbulence_emission_count`, `wavecrest_potential`, `whitewater_emitter_velocity`, `whitewater_type`, `whitewater_fused`), `foam_cover.wgsl:65-71`, `spray_mist.wgsl:69-75`, `preserve_foam_body.wgsl:66-67`, `keep_whitewater_body.wgsl:72`, `advect_whitewater_body.wgsl:182`, `whitewater_fused.wgsl:1111`, `whitewater_obstacle_source_body.wgsl:28-32`, `water_march_core.wgsl:78`, `look_wet_surface.wgsl:57`, `foam_field.wgsl:26`, `water_cover.wgsl:29`, `water_mist.wgsl:80`.

**Readers that change nothing** (they use the world position as it is): `gpu_flip_clock.wgsl`, `liquid_stats.wgsl`, `particle_identity.wgsl`, `push_out_of_solid_body.wgsl` (body-local lattices), `shape_particle_blobs_body.wgsl`, `interpolate_particle_frames_body.wgsl`, `particles_to_copies_body.wgsl`, and the whitewater bodies that read positions without a lattice (`emission_count`, `energy_potential`, `jitter_particles`, `whitewater_step`'s population scan).

## 4. Bounds (decision 3, I2 and I3)

Bounds is the Water item's box (its Transform: position and size) with six faces, each Wall or Open. Off by default; tank presets turn it on with every face a Wall (D4, D18).

- **Wall: an ordinary solid.** The solid distance at x is the smaller of every body's distance and the distance to the union of the Wall half-spaces (the space beyond each Wall face). That is today's per-axis formula (`liquid_solid_distance_body.wgsl:59-78`, CPU twin `lattice.rs:228-277`) over the Wall faces only, with the planes at the Bounds faces instead of `wall_inset` cells in from a grid edge. Every pass that handles solids (open fractions, solid velocity, the constraint, the particles' collision and push-out, pockets, emission's solid test) then handles walls with no case of its own. The rigid world keeps one slab per Wall face (`DomainWalls`, `coupling.rs:122-213`).
- **Open: a removal plane.** Water whose move ends beyond an Open face plane is removed (radius 0), as an Outflow removes it (`move.wgsl:301-310`). Beyond it is air, so the solve puts a free surface there. This replaces `open_band` and `closed_faces`.
- **Edges.** A point beyond both a Wall and an Open face is inside the Wall's solid: the collision pushes it to the Wall plane, and if it is still beyond the Open plane it is removed.
- **Bounds off (I3).** Water whose move ends below the kill height is removed. Kill height = the lowest solid point minus the drawn particle radius. The lowest solid point is the lowest world-box minimum y over this frame's Fixed body rows; with no Fixed body, over every body row; with no body, the lowest Initial Fill or Inflow at the epoch's start (Peter's call 4). The drawn radius is Particle Scale × the particle radius for a Surface draw, the dots' size × the radius for a Particles draw; the scene compiler wires the matching row's value node into the domain. The domain computes the height on the CPU each frame and sends one scalar; with Bounds on it is −∞ and nothing reads it. Fixed bodies, not dynamic ones, because a body falling forever would drag the kill plane down with it.
- **Fill Height** fills the Bounds box from its floor. With Bounds off a Fill Height above 0 is refused by name; the Initial Fill is the starting water. Tank presets keep Fill Height with Bounds on.
- **The force and impulse lattices** cover the box today (`gpu_flip_domain.rs:259-262`). They cover the scene box instead: the Bounds box, or with Bounds off the box round every body row, region and the Initial Fill, laid by `ForceLattice::over_box` (`manifold-physics/src/force_lattice.rs:141`) at today's node count along the Dam Break's longest side. Reads past it clamp, as they do today (`force_lattice.wgsl:26-41`).
- **Body distance lattices** are cropped to the box today (`bodies.rs:760-840`, `local_domain` `:1264`). With Bounds on they stay cropped to the Bounds box; with Bounds off the crop is `None`, which `water_plan` (`manifold-physics/src/collision_shape.rs:436`) already takes: the whole body at its own longest side over `DISTANCE_NODES_ALONG_LONGEST`. Known limit: a large detailed body under Bounds-off water is sampled coarser; put Bounds round the action for detail.
- `fluid_role_source.rs` (named by the plan for S4.2) needs nothing: roles carry no box.

## 5. Cell size in metres (decision 4, D14)

- **Card.** The Resolution row (`scene_schema/water.rs:234-239`, "cells across the longest side") becomes Cell Size, `Unit::Metres`, default 0.0625, limits 0.001 to 1.0 (Peter's call 1). The scene compiler's `resolution` value node (`scene_compile/water.rs:87-88`) becomes `cell_size`. The three whitewater capacity rows (`water.rs:269-302`) go: the whitewater pool holds a fixed WHITEWATER_SLOTS = WHITEWATER_PER_CELL × 64 = 102,400, today's default, and a full pool still replaces its oldest particles.
- **Both solvers.** `domain_layout(domain, domain_size, resolution)` (`fluid_domain.rs:30-100`) becomes the cell size and the Bounds box (or none). No cell counts, no 32-bit grid check (no grid), no DOMAIN_EXTENT_METRES. The far-from-origin check (`fluid_domain.rs:86-92`) stays, at the Bounds or Initial Fill position. `domain_size` goes. Matter takes the same row; its `grid_budget_mcells` and `admit_lattice` (`matter_domain.rs:194`, `:285`, `:467`) give way to the pool budget (M2, M3).
- **What restarts the water:** Cell Size, the solver, Reset, the Bounds switch (Peter's call 3), the Initial Fill and Fill Height, and for Matter Points per Cell and Seed. Not: Bounds position, size or faces, Speed, gravity, sources that move, or any Surface row.
- **What the budget bounds.** The pool's P tiles hold the water and its band, 512·P·h³ of space: 1,057 m³ at 6.25 cm and 16.5 m³ at 1.56 cm for P = 8,456. Halving the cell size divides it by eight. The particle pool (S3.4) bounds the water itself, slots · h³ / 8. When either fills, sources stop and the card says why (S3.5). D4's budgets per resolution become one budget whatever the cell size: the lattice stays under POOL_BUDGET_BYTES (1.5 GiB), and every memory report lists tiles held against P.

## 6. Moving Domain Position or Bounds live (decision 5)

- **What moves:** the Wall solids, in the next substep's solid distance; the Open planes; the rigid world's slabs, edited in place; the scene box the force lattices cover; and the Water item's child sources, which follow their parent like any child.
- **Walls carry the box's velocity:** each face's plane displacement over the tick divided by its length, written to the solid face velocity where a Wall is the nearest solid. Today a wall face gets zero velocity (`closest_body` returns −1, `solids.wgsl:291-310`); −1 then reads the nearest Wall face's velocity. A sliding tank sloshes its water instead of shoving it.
- **What stays:** every particle (world positions), the table and pool (world keyed), the whitewater, the clock and epoch, the rigid bodies' state, and the Initial Fill, which seeds only at a restart.
- So `GpuFlipSetup` (`gpu_flip_domain.rs:61-73`) and `MatterSetup` (`matter_domain.rs:61-71`) drop the lattice and the closed faces and keep the cell size, the fill and the capacity (and Points per Cell and Seed).
- **The rigid owner.** Today a walls change fails `matches` and `admit` (`coupling.rs:326`, `:359`), builds a fresh owner, and `owner_fresh` restarts the water (`gpu_flip_domain.rs:1083`). New: walls leave both checks. A walls change edits the six slab bodies in place through a new `set_walls` on the rigid worker (`manifold-water-rigid/src/physics/worker.rs`): a move re-poses them, a resize or a Wall/Open switch rebuilds only those Fixed slabs, which hold no state. Dynamic bodies are untouched, so no fresh world is needed.

## 7. Coarse levels on world tiles

The solver's coarse levels are dense boxes today (`gpu_flip_pressure.rs:399-500`); with no box they cannot exist, so S4.1 must move them.
- Each coarse level L is a world-tiled lattice of its own: cell size 2^L·h, tiles of 8³ of its cells, its own table, pool and neighbour rows from the shared build, and a children row per slot (8 slots of level L − 1). Its tiles are the parents (coordinate >> 1) of level L − 1's held tiles, rebuilt from scratch every step in table order; coarse fields are rebuilt every step, so nothing carries over.
- Pool per level: P_L = ⌈P_{L−1} / 2⌉, enough for compact water, sheets and lines. A level that runs short leaves some tiles without a coarse correction; restriction and prolongation both skip the same tiles, so the preconditioner stays symmetric and the solve stays right, only slower. The shortfall is reported. The coarse fields (water, touched, faces, rows, rhs, e: 80 bytes a cell) join the per-tile bytes P is sized from.
- Depth: the levels today's `level_lattices` (`gpu_flip_pressure.rs:81`) gives a cube of the pool's size (⌈P^⅓⌉ tiles a side), so compact water that fills the pool coarsens as deep as a box of its size does today. Fixed per pool, so the encode never reads the GPU.
- The coarsest level is smoothed, COARSEST_SWEEPS red-black sweeps each way, instead of solved by the dense inverse (`coarse_inverse.wgsl`, deleted), since it is no longer a box of 64 cells or fewer. The value is set by the convergence proofs at 64 and 128.
- Solve Level is checked against the fixed depth. The pocket labels coarsened for Solve Level above 0 (`pockets.wgsl:440-541`) use the coarse levels' pools.

## 8. Deletions (decision 6, S4.5)

From `rg` on the working tree of 2026-10-27; S4.1 to S4.4 delete some of these as they go, and the lead re-runs every pattern at handoff. Fixture JSONs are regenerated from the builders (as RD.5), never hand-edited.

```
closed_faces | closed_neg_* | closed_pos_*
  manifold-core/src/liquid_domain.rs: 83-88, 107-112
  manifold-nodes-water/src/migration/liquid.rs: 982
  manifold-nodes-water/src/presets/gpu_flip.rs: 107, 118-123, 150, 157, 159, 527, 592, 607, 1450
  manifold-nodes-water/src/testkit/conformance.rs: 634-639
  manifold-nodes/tests/contracts/node_graph/catalog_tests/gpu_flip_preset.rs: 104
  manifold-nodes/tests/gpu_proofs/matter_bodies.rs: 382, 606, 804
  manifold-nodes/tests/gpu_proofs/matter_scene.rs: 143, 222, 334-339, 528
  manifold-nodes/tests/gpu_proofs/matter_transfer.rs: 194
  manifold-water-gpu-flip/src/primitives/gpu_flip_domain.rs: 48, 274, 425, 476-481, 1057, 1268
  manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs: 68, 250, 465, 528
  manifold-water-gpu-flip/src/primitives/gpu_flip_step/encode.rs: 108
  manifold-water-gpu-flip/src/primitives/gpu_flip_step/primitive.rs: 172, 333
  manifold-water-gpu-flip/src/primitives/gpu_flip_step/read_params.rs: 22
  manifold-water-gpu-flip/src/primitives/gpu_flip_step/tests.rs: 257-260, 263
  manifold-water-gpu-flip/src/primitives/gpu_flip_step_tests.rs: 161
  manifold-water-gpu-flip/src/primitives/gpu_flip_step_tests/motion.rs: 658, 663, 690
  manifold-water-gpu-flip/src/primitives/gpu_flip_step_tests/pockets.rs: 112, 403
  manifold-water-gpu-flip/src/primitives/liquid_solid_distance.rs: 29, 51, 68, 115, 169, 190, 223
  manifold-water-gpu-flip/src/primitives/shaders/gpu_flip_step/common.wgsl: 72-74
  manifold-water-gpu-flip/src/primitives/shaders/gpu_flip_step/move.wgsl: 232-233
  manifold-water-gpu-flip/src/primitives/shaders/gpu_flip_step/pockets.wgsl: 58-59, 615-616
  manifold-water-gpu-flip/src/primitives/shaders/liquid_solid_distance_body.wgsl: 49, 64
  manifold-water-gpu-mpm/src/primitives/matter_body_reaction.rs: 31, 69, 95, 163, 226
  manifold-water-gpu-mpm/src/primitives/matter_domain.rs: 51, 65, 88, 104, 214, 246, 287-292, 499, 1058
  manifold-water-gpu-mpm/src/primitives/matter_frame.rs: 46, 61, 123-124, 146, 151
  manifold-water-gpu-mpm/src/primitives/matter_grid_update.rs: 32, 66, 90, 151-152, 229
  manifold-water-gpu-mpm/src/primitives/shaders/matter_body_reaction_body.wgsl: 95, 142
  manifold-water-gpu-mpm/src/primitives/shaders/matter_grid_update_body.wgsl: 66, 122
  manifold-water-liquid/src/coupling.rs: 123 (the mask becomes the Bounds Wall mask)
  manifold-water-liquid/src/lattice.rs: 22-39 (fn closed_faces), 228-237, 258, 263, 442
  manifold-water-surface/src/primitives/liquid_frame.rs: 61, 79, 209-210, 261, 305
  manifold-water-whitewater/src/primitives/shaders/whitewater_obstacle_source_body.wgsl: 19, 35
  manifold-water-whitewater/src/primitives/whitewater_emitter_gpu_tests.rs: 309, 359, 390
  manifold-water-whitewater/src/primitives/whitewater_obstacle_source.rs: 56, 78, 96, 156-157, 213, 241, 275
  fixtures: matter_solver.json, presets OceanCliff, WaterDamBreakGpuFlip, WaterDamBreakMatter,
    WaterDamBreakParticles, WaterFloatingBoxMatter, WaterSeaWallGpuFlip, WaterStillPoolMatter
wall_inset (and FLIP_WALL_EPSILON, the 1.5-cell grid offset)
  manifold-nodes-water/src/migration/liquid.rs: 487, 491, 495, 500, 506, 572, 574, 599, 610, 661, 664, 666, 718, 725, 727, 1139, 1143, 1226
  manifold-nodes-water/src/presets/gpu_flip.rs: 559, 598
  manifold-nodes/tests/contracts/node_graph/catalog_tests/runtime_generator_runtime.rs: 82, 89
  manifold-water-gpu-flip/src/primitives/gpu_flip_domain.rs: 126, 317, 457, 1643
  manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs: 264-266, 419
  manifold-water-gpu-flip/src/primitives/gpu_flip_step/encode.rs: 109
  manifold-water-gpu-flip/src/primitives/gpu_flip_step/primitive.rs: 348, 372
  manifold-water-gpu-flip/src/primitives/gpu_flip_step/tests.rs: 208
  manifold-water-gpu-flip/src/primitives/gpu_flip_step_tests.rs: 92
  manifold-water-gpu-flip/src/primitives/liquid_fill.rs: 122, 132, 147, 173, 222-224, 304, 331
  manifold-water-gpu-flip/src/primitives/liquid_solid_distance.rs: 30, 52, 69, 116, 170, 189, 191, 224
  manifold-water-gpu-flip/src/primitives/shaders/gpu_flip_step/common.wgsl: 92-94
  manifold-water-gpu-flip/src/primitives/shaders/gpu_flip_step/move.wgsl: 3-4, 8, 230
  manifold-water-gpu-flip/src/primitives/shaders/gpu_flip_step/solids.wgsl: 213-217
  manifold-water-gpu-flip/src/primitives/shaders/liquid_fill_body.wgsl: 18-19, 126, 159
  manifold-water-gpu-flip/src/primitives/shaders/liquid_solid_distance_body.wgsl: 3, 50, 59
  manifold-water-liquid/src/lattice.rs: 42-43, 70-75 (FlipSolverGrid::wall_inset), 233-234, 318-470 (tests)
  manifold-water-surface/src/primitives/liquid_frame.rs: 82, 233; liquid_frame/extent.rs: 56
  manifold-water-whitewater/src/primitives/shaders/whitewater_obstacle_source_body.wgsl: 20, 30
  manifold-water-whitewater/src/primitives/whitewater_emitter_gpu_tests.rs: 310, 360, 391
  manifold-water-whitewater/src/primitives/whitewater_obstacle_source.rs: 57, 79, 97, 160-161, 214, 240, 242, 276
box_offset (the solid faces' tolerance becomes 8ε·|face centre| from the face itself)
  manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs: 243
  manifold-water-gpu-flip/src/primitives/gpu_flip_step/primitive.rs: 326
  manifold-water-gpu-flip/src/primitives/gpu_flip_step/tests.rs: 205
  manifold-water-gpu-flip/src/primitives/gpu_flip_step_tests/solids.rs: 276
  manifold-water-gpu-flip/src/primitives/shaders/gpu_flip_step/common.wgsl: 62-63
  manifold-water-gpu-flip/src/primitives/shaders/gpu_flip_step/solids.wgsl: 178
MAX_SIDE, lattice_refusal (no lattice sides remain)
  manifold-water-gpu-flip/src/primitives/gpu_flip_pressure.rs: 53-54, 230-236, 792, 2026, 2035-2037, 2121
  manifold-water-gpu-flip/src/primitives/gpu_flip_domain.rs: 22, 196
  manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs: 44
  manifold-water-gpu-flip/src/primitives/gpu_flip_step/extent.rs: 3, 29-33
  manifold-water-gpu-flip/src/primitives/gpu_flip_step/primitive.rs: 93-96
  (consecutive_edges.rs:28 is POLYGON_MAX_SIDES, unrelated)
DOMAIN_EXTENT_METRES
  manifold-core/src/fluid_domain.rs: 13-16, 57
  manifold-core/src/scene_schema/water.rs: 27, 173-174
  manifold-nodes-scene/src/node_graph/scene_surface.rs: 9, 50-51
  manifold-nodes-scene/src/node_graph/viewport_gizmo/tests.rs: 4, 475, 487
the boundary clamp (BOUNDARY_MARGIN, boundary_edge, inside_boundary, clamp_boundary, inside_grid)
  manifold-water-gpu-flip/src/primitives/shaders/gpu_flip_step/move.wgsl: 1-27, 145-146, 159, 170-172, 209-210, 277, 285-286
  manifold-water-gpu-flip/src/primitives/gpu_flip_step_tests.rs: 91, 208-216, 247-249, 265, 280-282
  manifold-water-gpu-flip/src/primitives/gpu_flip_step_tests/motion.rs: 9
open_band, OPEN_BOUNDARY_WIDTH
  manifold-water-gpu-flip/src/primitives/shaders/gpu_flip_step/move.wgsl: 220-238, 294
  manifold-water-gpu-flip/src/primitives/shaders/gpu_flip_step/pocket_bindings.wgsl: 7
  manifold-water-gpu-flip/src/primitives/gpu_flip_step_tests/motion.rs: 634
the wall special cases
  tile_storage.wgsl: 70-83 (ts_wall_mask), 105-115 (ts_face's mask); tile_storage.rs: 18-20 (CANON_RECORDS 8 → 1), 125-136
  gpu_flip_step/tiles.wgsl: 49-60 (canonical_face per mask), 88-105 (pool_canon over 8 masks), 176, 190
  gpu_flip_step/common.wgsl: 270-275 (face_exists) and its callers transfer.wgsl: 21, forces.wgsl: 108,
    projection.wgsl: 34, 85, pockets.wgsl: 563, solids.wgsl: 180, 342, 421
  gpu_flip_step/solids.wgsl: 180, 342 (p[a] == 0 || p[a] == n[a]), 386-390 (solid_border), 213-218 (wall_distance), 310
  gpu_flip_step/pockets.wgsl: 58-62 and 611-622 (open box faces as air), 587-605 (dry_floor_hole)
  gpu_flip_step/encode.rs: 72-78 (comment), 108-109 (closed_faces 63, wall_inset)
  liquid_fill_body.wgsl: 87-99 (the native box solid)
  whitewater_obstacle_source_body.wgsl: 19-35
  matter_walls.wgsl (whole file); matter_grid_update_body.wgsl: 9-10, 122; matter_body_reaction_body.wgsl: 6, 142;
    matter_grid_update.rs: 9, 117; matter_body_reaction.rs: 8, 125; matter_common.rs: 5; tile_grid.wgsl: 21-30 (ts_node_*)
  lattice.rs: 225-277 (wall_distance, flip_wall_distance, wall_distance_inset) and its callers
    matter_frame.rs: 151, liquid_frame.rs: 305
```

S4b.4 deletes the Water box the consumers keep until then: the domain's lattice wires (`lattice_min_*`, `nodes_*`, `mesh_*`, `grid_bounds`, `grid_nodes_*`, `face_cells_*`, `face_valid_layers`), `LiquidLattice::surface`, `FlipSolverGrid`, `MAX_LATTICE_NODES`, `PADDING_NODES` and `SURFACE_*` in `lattice.rs`, `LiquidLattice::from_wires` and its callers (the `rg -l` list in S4b.4), and `liquid_lattice_box` in `assets/liquid_surface_group.json`.

## 9. Jobs (decision 7)

Order: S4.1a; then S4.1b, S4.1p, S4.1c, S4.1d and S4.1e written at once (separate files) and built together; S4.2; S4.3a with S4.3b; S4.4; M3; S4.5; then S4b.0, S4b.1, S4b.2 and S4b.3 at once; S4b.4. Workers write code only (plan section 1). Tests are small CPU or GPU checks of real math, each under 2 s.

**S4.1a** (sonnet, A: `manifold-node-engine`). The shared tables.
- Files: new `manifold-node-engine/src/scene/world_tiles.rs` + `world_tiles_read.wgsl` (pack, hash, lookup, home, the `ts_*` cell helpers of section 2); new `manifold-water-liquid/src/world_tiles.rs` + `primitives/shaders/world_tiles.wgsl` (the insert and build passes 1 to 9, with `WorldTiles::{new, prepare, encode_allocation, encode_parents, table, lists, args, counts, pool_tiles, table_entries}`; the particle read is a composed WGSL snippet, as `SortJob::read` is); `tile_storage.rs` keeps only the sizes (`TILE`, `TILE_CELLS`, `CANON_ELEMENTS`, `pool_len`, `table_entries(P)`, `table_words(P)`) and `TILE_GRID`; `tile_storage.wgsl` is deleted (its read side moved), and `tile_counts`, `tile_total` go once S4.1b and S4.1c stop calling them.
- Composers of `TILE_STORAGE` / `TILE_POOL` import the read side from node-engine: `gpu_flip_pressure.rs:40`, `gpu_flip_bodies.rs:38`, `gpu_flip_step_tests/density.rs:4`, `gpu_flip_sheet_distance.rs:27`, `gpu_flip_step/pipelines.rs:4`, `whitewater_step.rs:42`, `gpu_flip_sheeting.rs:48`, `sort_particles_into_cells.rs:20`, `face_sample_component.rs:12`.
- Tests: a CPU mirror of pack, hash and ordered insert gives the same table words for one key set inserted in three shuffled orders, including negative tiles, the wrap and a full table; a GPU build from a synthetic particle set matches the mirror over three steps with a moving set (each held slot's entry points back to it, freed slots in slot order, ring ≤ 1 allocated first, exhausted and alias bits set when forced); for every cell of a 3×3×3 block of tiles the home, neighbour-row and hash paths of `ts_cell_at` agree; naga validates the composed read side and the build shader (as `tile_storage.rs:76-110` does today).

**S4.1b** (sonnet, W). The FLIP step on world tiles.
- Files: `gpu_flip_step.rs` (`StepParams` `:225-276`: drop `n`, `box_offset`, `alloc_ring`'s box meaning; add `origin`, `table_entries`; keep `closed_faces`, `wall_inset` and a `walls_lo/hi` box in metres for S4.2 to replace), `gpu_flip_step/{tiles,state,extent,primitive,encode,lattice,extend,pipelines}.rs`, `shaders/gpu_flip_step/{common,tiles,tile_bindings,transfer,particle_distance,density,divergence,forces,projection,solids,move,emit,extend,marker_removal}.wgsl`, and their tests in `gpu_flip_step_tests*` and `manifold-nodes/tests/contracts/water/primitives/gpu_flip_tile_tests.rs`.
- Steps: `TileTable` (`tiles.rs:120-247`) becomes a `WorldTiles` plus FLIP's own passes (`pool_canon`, `tiles_fresh`, `tiles_retire`, the poisons) with one canonical element; `tiles_allocate` (`tiles.wgsl:107-165`), `tile_ring` (`:193-198`), `poison_inactive` (`:200-220`), `tiles_clear` (`:252-267`), `tiles_mark` (`:269-304`), `tiles_rings` (`:306-341`), `rank_of` (`:343-350`) and `tiles_lists` (`:352-413`) go; the shared build replaces them. Every box dispatch becomes a list dispatch, 512 threads a slot: `encode.rs:253` (forces), `:270` (open fractions), `:284` and `:309` (solid velocity and extrapolation), `:360` (pocket pin), `:378` (pocket condition), `:508` (project), `:557` (constrain), `:761` (remove crowded), the poison `:199`. Every kernel calls `ts_home` once. The solid corners (`l.corners`, `ts_corner`) become a pooled field (corner p stored with cell p), written by a step pass over the allocated list from the shared solid-distance math in `LIQUID_COLLIDER`; `node.liquid_solid_distance` keeps its dense driver until S4b. Emission enumerates each region's own site box (the domain publishes `region_sites`, one min site and size per region row, `emit.wgsl:1-10` and `encode.rs:798-870`); `emit_site_taken` checks `ts_cell_stored` before reading a range, so it never reads the canonical bucket; emission's solid test uses the solid-distance math, not the pooled field, so it is right where no tile exists yet. `marker_removal` runs over C. Stats word 6 becomes |C| / P, the pool's fill. Readers of positions per section 3. The test lever `all_tiles` becomes "allocate every tile of a given box", so the proofs keep an oracle. Particles whose own tile has no slot are removed in the move and counted (stats word named with S3.5).
- Tests: the existing CPU-reference kernel tests pass with the lattice moved off the origin to negative coordinates; a tile build test where two particles sit across tile 0 and tile −1 gives both slots and the right neighbour rows; the Dam Break (still on the old walls) keeps its volume inside the volume test's tolerance; the poison proof (`poison_free`) stays green.

**S4.1p** (sonnet, W). Pockets on world tiles.
- Files: `gpu_flip_step/pockets.rs`, `shaders/gpu_flip_step/{pockets,pocket_bindings}.wgsl`, `gpu_flip_step_tests/pockets.rs`.
- Steps: the cell passes (`pockets.rs:68`, `:82`, `:162`, `:178`, `:254-281`, `:316`) run over the lists. The line sweeps (`pockets.rs:115-141`, `pockets.wgsl:134-178`) start at every slot whose neighbour on the low side of the axis is absent, 64 lines a tile, and each line walks through the neighbour rows until it leaves the held tiles, so a line is still swept whole in one thread. A label is the cell's pool index (deterministic, since slots are); `pocket_leader[label]` replaces `ts_cell_at(ts_grid_coord(label, n))` (`:524`, `:536`). `pocket_tally` loops over C. The coarsened labels use S4.1c's `level_tiles(level)`.
- Tests: the pocket tests' sealed and open cases on a lattice placed across the origin, with a pocket spanning two tiles found in one round.

**S4.1c** (sonnet, W). The pressure solver's levels on world tiles (section 7).
- Files: `gpu_flip_pressure.rs` (`level_lattices :81`, `max_solve_level :93`, `level_refusal :98`, `tile_total :116`, `tile_bases :121`, `scratch_bytes :138`, `Water :246`, `Params :265`, `Level :399`, `Buffers :413`, `prepare :791`, `solve :912`), `shaders/gpu_flip_pressure.wgsl` (`tile_cell_in :179`, `pooled() :310`, `cell_idx :322`, `rows_main :382`, `restrict_main :611`, `zero_main :678`, `prolong_main :693`, `coarsen_water_main :762`, `classify_main :795`, `lists_main :835`, `coarsen_faces_main :900`, `coarse_solve_main :1020`), `coarse_inverse.wgsl` (deleted), `gpu_flip_pressure_tests.rs`. Lentine stays a dense standalone (`TILE_GRID` only; not on the step's path).
- Steps: every level a `WorldTiles` (`encode_parents` from the level below), children rows for restriction and prolongation, partials by slot, the coarsest smoothed COARSEST_SWEEPS each way, Solve Level against the fixed depth, `level_tiles(level)` for the pockets. SP's fused bottom goes; the warm start keeps reading the pooled `pressure`.
- Tests: the saved Dam Break problems at 64 and 128 converge on the engine tolerance with the iteration counts printed beside today's; a CPU mirror of the parent build (child tiles to parent keys and children rows) on a sheet and on a line; the V-cycle stays symmetric (⟨M x, y⟩ = ⟨x, M y⟩ to f32 rounding on two random vectors over a small sparse set).

**S4.1d** (sonnet, W). Sheeting, sheet distance and bodies on world tiles.
- Files: `gpu_flip_sheeting.rs` + `shaders/gpu_flip_sheeting.wgsl` (box cell passes `:239`, `:302`, `:312`, `:325`, `:509`; the dense 2-cell buckets `:343`, `:401`, `:533` and `bucket_bytes` `gpu_flip_sheeting.rs:130` become 64 buckets per slot; `local` `:179`), `gpu_flip_sheet_distance.rs` + `.wgsl` (`:60`, `:72`), `gpu_flip_bodies.rs` + `.wgsl` (the record pass `:401-404`, `lattice_min :36, :157`, partials by slot), their tests.
- Tests: the existing sheeting CPU-reference tests and the body proofs, on a lattice across the origin.

**S4.1e** (sonnet, W). The domain, the sort, the fill and the consumers' bridge.
- Files: `gpu_flip_domain.rs` (outputs: cell size, origin, P, H, `region_sites`; the Water box still published for consumers, snapped to world cells), `liquid_fill.rs` + `shaders/liquid_fill_body.wgsl` (sites in world half-cells; the native box solid `:87-99` becomes the walls box from the domain until S4.2), `sort_particles_into_cells.rs` + `.wgsl` (the pooled key is `ts_cell_at` of the unclamped world cell; dense users unchanged), `face_sample_component.rs` + `_body.wgsl` and `manifold-water-whitewater/src/primitives/{whitewater_step.rs, shaders/gather_distance.wgsl}` (read `ts_cell_at(box_origin + f)`, the box origin in world cells a new param), `substep_history.rs` (unchanged in shape; its face component gets the box origin).
- Tests: the pooled sort test (S3.3's) over a permuted table and negative cells; the fill places the Dam Break's sites at the same world positions as before; the face component reads the right record across a tile face and canonical zero past the held tiles.

**S4.2** (sonnet, W). Bounds per I2 and I3 (section 4).
- Files: `shaders/gpu_flip_step/{common,solids,move,pockets}.wgsl` and `StepParams` (bounds lo/hi, Wall mask, Open mask, kill height); `liquid_solid_distance.rs` + `shaders/liquid_solid_distance_body.wgsl`; `gpu_flip_domain.rs` (params `bounds` and six faces Wall/Open, the kill height, the scene box for the force lattices, the body crop); `manifold-water-liquid/src/coupling.rs` (`DomainWalls` from Bounds: no slabs with Bounds off); `liquid_fill_body.wgsl` and `whitewater_obstacle_source*` (walls from Bounds); `lattice.rs:228-277` becomes a `bounds_distance(x)` CPU twin for the tests.
- Steps: the deletions under closed_faces, wall_inset, box_offset, the boundary clamp, open_band and the wall special cases in the step shaders; Open removal and the kill height in `faces_to_particles` beside the Outflow test (`move.wgsl:301-310`).
- Tests: `bounds_distance` matches the shader's solid distance on a 4³ sample with mixed Wall and Open faces, including an edge point beyond a Wall and an Open face; a particle moved past an Open plane and one below the kill height are removed, one inside a Wall is pushed to the plane; the kill height from three body rows (one Fixed, one dynamic lower, none) follows the rule; the Dam Break with Bounds on and every face a Wall keeps its volume inside the volume test's tolerance and its wave front within one cell of the pre-S4 run at 1 s (the plan's S4 check).

**S4.3a** (sonnet, A: `manifold-core`, `manifold-nodes-scene`). The card and the shared layout.
- Files: `manifold-core/src/fluid_domain.rs` (`:13-16`, `:30-100`), `scene_schema/water.rs` (rows `:205-239`, `:173-174`, the whitewater rows `:269-302`, `WHITEWATER_PER_CELL :143`, Bounds and face rows), `liquid_domain.rs:68-115`, `manifold-nodes-scene/src/node_graph/scene_compile/water.rs:87-88`, `scene_vm/setup_check.rs:900, :1339`, `scene_surface.rs:50-51`, `viewport_gizmo/tests.rs:475-487`.
- Tests: `domain_layout` from a cell size and a box gives the box and cell size unchanged, refuses a non-finite or non-positive cell size and a box past the far-origin check; the card shows Size and the faces only with Bounds on.

**S4.3b** (sonnet, W). Cell size through the solvers and tools.
- Files: `gpu_flip_domain.rs` (`:193-194`, params `:464`, `:468`), `matter_domain.rs` (`:187-191`, `:194`, `:284-285`, `:467`), `lattice.rs` (`from_layout :92`), `manifold-nodes-water/src/presets/gpu_flip.rs` (`:316`, `:522`, `:1320`), `testkit/{conformance,preset_extents}.rs`, `manifold-nodes/examples/fluid_capture.rs:418`, `manifold-app/src/frame_time.rs:57`, the fixtures (regenerated).
- Tests: every preset builder gives the cell size its old Resolution gave (4 m / 64 = 0.0625 and so on); the Dam Break's capacity and fill counts are unchanged.

**S4.4** (sonnet, W). Live moves (section 6).
- Files: `gpu_flip_domain.rs` (`GpuFlipSetup :61-73`, the restart `:1083`, the walls `:1057`), `manifold-water-liquid/src/coupling.rs` (`matches :317-336`, `admit :346-380`), `manifold-water-rigid/src/physics/worker.rs` (`set_walls`), `shaders/gpu_flip_step/solids.wgsl` (the Wall face velocity at `closest_body :291-310`).
- Tests: moving Bounds between two frames keeps the epoch and every particle; a box sliding at 1 m/s gives its Wall faces 1 m/s in the solid face velocity; `set_walls` re-poses the slabs and leaves a dynamic body's position and velocity bit for bit; a resize and a face switch rebuild only the slabs.

**M3** (sonnet, W). Matter on world tiles, after M2.
- Files: `matter_domain.rs`, `matter_common.rs`, `matter_{to_grid,grid_update,body_reaction,frame,stats,fill}.rs` and their shaders, `matter_walls.wgsl` (deleted), `tile_grid.wgsl:21-30` (`ts_node_*` deleted). Matter's node p sits at p·h, tiles of 8³ nodes, built by `WorldTiles` from Matter's particles at its B-spline reach. Bounds Walls are solids in `matter_grid_update`'s collision, as bodies are; Open faces and the kill height remove material as in FLIP; live moves per section 6.
- Tests: the plan's M3 proof (WaterStillPoolMatter stays still, WaterFloatingBoxMatter floats, `matter_look_volume_drift` passes), plus a P2G scatter across tile 0 and tile −1 matching the CPU reference.

**S4.5** (haiku, W). The deletions of section 8 that S4.1 to S4.4 left, from the lead's re-run list, then the clean-cut `rg` in the plan's final check.

**S4b.0** (haiku, A). The scene object's `field` input takes the world-tiled field: pool, table, P, H, cell size and origin (`manifold-node-engine/src/scene/scene_object.rs`, the R2.0 port).

**S4b.1** (sonnet, W). The surface on world tiles.
- Files: `assets/liquid_surface_group.json` (`liquid_lattice_box`, `liquid_lattice_cells`, `liquid_bin_size`), `manifold-water-surface/src/primitives/{shape_particle_blobs, particle_volume, surface_weights, blob_bounds, carry_liquid_into_solids, liquid_frame}` and shaders, `manifold-water-liquid/src/primitives/{redistance_lattice, smooth_lattice, field_smooth_blend, liquid_bricks}`, `frame_history.rs` (`FIELD_SOLID`), `liquid_solid_distance.rs` (pooled; its dense driver goes).
- Rule: a field built from a particle set gets its own world table from those particles through `WorldTiles` (the surface from the frame's blended particles, at the surface's cell size); the solid distance and wetness the surface reads live on that table.
- Tests: R1a's CPU references on a field across a tile face and across the origin; blobs from one particle each side of a tile face give the same field as one dense box.

**S4b.2** (sonnet, W). Whitewater, foam, mist and wetness on world tiles.
- Files: `manifold-water-whitewater/src/primitives/*` (every `TILE_GRID` composer listed by `rg -l TILE_GRID crates/manifold-water-whitewater`), `foam_cover`, `spray_mist`, the R7w wetness writer, `manifold-water-liquid/src/substep_history.rs`.
- Steps: whitewater's lattice fields live on the step's table and its cell passes run over the step's lists; the substep history keeps each substep's pooled face velocities with a copy of that substep's table words, so substep k is read through its own table (slots can change between substeps); foam and mist get tables from their own particles; wetness keeps its tiles while wet and frees them dry.
- Tests: the whitewater CPU references on a lattice across the origin; a history read of substep 0 after a slot changed in substep 1 returns substep 0's velocity.

**S4b.3** (sonnet, R). The march and the look readers.
- Files: `manifold-nodes-scene/src/node_graph/primitives/shaders/{water_march_core, water_march, look_wet_surface, foam_field, water_cover, water_mist}.wgsl` and their Rust (`render_scene/water_march.rs`).
- Steps: samples go through the read side of section 2 (one hash per ray step, the home and neighbour rows for the cubic's taps); an absent tile is skipped whole (8 cells at a time), replacing `lattice_bricks`' empty-space skipping.
- Tests: R2's sphere-field proof on a field split over four tiles with one absent between them; a ray through an absent tile steps over it in one step. Stills: water poured from an inflow over a ledge falls past where the old box ended and spreads on a floor; the memory report grows with the water; walls stay flush and wet.

**S4b.4** (haiku, W). The Water box the consumers kept (end of section 8).
