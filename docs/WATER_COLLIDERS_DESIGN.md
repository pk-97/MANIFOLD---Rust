# Water colliders: exact solids at the water's own cells, only where the water is

<!-- index: Design for replacing the CPU distance lattice every Water, Matter and whitewater reads its solids from: a BVH per geometry with fast winding numbers, static colliders cached per held water tile at the water's cell size, moving bodies in a GPU-built local lattice, direct tree queries elsewhere, so a GLB of any size and triangle count collides exactly. Starts from water falling through large GLB imports on 29 Oct. -->

**Status:** phases 1 to 3 landed 29 Oct (ce61d38fc, 55a58c300, 31603ea40, then the GPU-built lattices). Left: phase 4, Peter's large-GLB still, and the cost of the readers that query trees directly (C5).
**Companions:** [WATER_SHARED_GRID_DESIGN.md](WATER_SHARED_GRID_DESIGN.md) (one table and one budget per World; the static blocks and moving lattices join that budget), [GPU_FLIP_END_GAME_DESIGN.md](GPU_FLIP_END_GAME_DESIGN.md).

## Why

Water falls through large GLB imports. Every solid the water sees is a dense distance lattice per shape, built on the CPU and packed into one f16 atlas:

1. **Spacing ignores the water.** `water_plan` (`manifold-physics/src/collision_shape.rs`) takes `min(longest / 32, max(thinnest / 2, cell))`, where `thinnest` is the smallest side of the mesh's *bounds*. A closed 20 m room with Bounds off samples at 62 cm, so its 10 cm floor is not there.
2. **A node cap coarsens big meshes.** Past `MAX_LATTICE_NODES` (16.7M, `sdf.rs`) the spacing grows by 1.125 until it fits, whatever the walls are.
3. **The build costs nodes × triangles on one thread** (`signed_distance_union_within`, no acceleration): a 500k-triangle scan at 16M nodes would not finish in a show. Until it does, live water holds or reads an older crop.
4. **Inside and outside depend on exact topology.** Closed and open parts are split by exact vertex equality (`closed_triangles`). Open parts become shells sized from the mesh (`SHELL_FRACTION`), closed parts are signed by an unaccelerated winding sum.
5. **The content thread repacks the whole atlas** on any change (`pack_distance_atlas`), and every corner loops over every body with no culling (`bodies_distance`, `solids.wgsl`).

The lattice's size follows the mesh. It should follow the water.

## Decisions

**C1. Distance comes from the triangles, on the GPU, at the water's cell size.** No CPU lattice. Every query is answered from one BVH per geometry, or from blocks built from it on the GPU at the water's own cell size, so a thin floor is as thin as it is.

**C2. One BVH per geometry, built once on a worker thread.** It's a binned-SAH tree over the triangles. Each node also carries first-order fast-winding-number data (area-weighted normal sum, centroid and radius; Barill et al. 2018). It's built where `PreparedFluidGeometry` is prepared, cached with it, and uploaded once to a buffer every Water shares (`manifold-physics/src/mesh_bvh.rs`, phase 1).

**C3. A point's distance follows one rule** (`collider_phi`):
- `d` is the nearest-triangle distance (BVH traversal, pruned at the band) and `w` the winding number.
- Inside is `|w| > ½`, so inside-out and mirrored meshes are solid too.
- Where `|w|` is within ¼ of ½ (beside a hole or an open sheet), the point is a shell, `d` minus one cell, so an open surface is two cells thick.
- Closed parts are never inflated.

This deletes the closed/open split, `SHELL_FRACTION`, `thinnest`, the 32-node rule and the node cap.

**C4. Static colliders are cached per held tile.** A Fixed Collider that isn't moving joins the static solid. Each held sim tile keeps one world-aligned block of 9³ nodes (an apron of one, so a trilinear sample never leaves its tile): the smallest `collider_phi` of the static shapes whose box overlaps the tile, and which shape that was (for friction).
- The block is built only when the slot is fresh (`tiles_fresh_list`), or when the static set changes (a shape added, removed, rescaled or starting to move), and then only where it overlaps.
- A tile farther than the band from every static triangle (one BVH box query) skips the nodes: it takes the sign of one winding query at its centre.
- Memory: about 2.9 KB a tile (f16 distance, u16 shape) against about 300 KB of water fields, so it follows the water, not the mesh. A 200 m terrain costs nothing where there's no water.

**C5. Every other reader asks the tree directly.** The step's own corners and samples inside held tiles read the per-tile cache. Elsewhere `collider_phi` runs on the tree: emission sites outside held tiles, the display's solid, whitewater, Matter and the fill. It's exact everywhere and costs only for the points that are there. Phase 4 measures the display and Matter, the readers with the most points. If either costs too much, it reads the step's cache instead.

**C6. Moving bodies keep a body-local lattice, built on the GPU.** Box3D bodies and Colliders that move keep a dense f16 lattice in their own frame, like today, sampled by the same `liquid_shape_distance`. But it's built by one GPU pass from the BVH at the water's cell size: no CPU build, no stand-in, no 32-node rule and no node cap. It's rebuilt only when the geometry, scale or cell size changes. Its bytes go through the Water's memory budget, and a refusal names the body on the card. If big moving bodies at fine cells measure too costly, sparse local bricks come later.

**C7. Per-tile shape lists.** Each substep, after `pose_bodies`, each held tile lists the moving bodies whose posed box, grown by the band, overlaps it. `bodies_distance`, `closest_body` and their twins read the static block, then loop over that list instead of every body.

**C8. Every consumer keeps its maths and changes its read.** The consumers are:
- FLIP: `solids.wgsl`, `forces.wgsl`, `liquid_solid_distance_body.wgsl`, `liquid_fill_body.wgsl`
- Matter: `grid_to_matter_body.wgsl`, `matter_fill_body.wgsl`, `matter_grid_update_body.wgsl`, `matter_body_reaction_body.wgsl`, with Matter's own tiles
- Whitewater: `keep_whitewater_body.wgsl`, `whitewater_obstacle_source_body.wgsl`

**C9. Replace, don't layer.** The cut deletes:
- `signed_distance_union*`, `water_plan`, `water_lattice` and `LatticePlan`
- `MAX_LATTICE_NODES`, crop and `CROP_MARGIN`, `resolve_lattice` and its stand-in, `pack_distance_atlas`'s whole-atlas repack on the content thread
- the CPU `RoleLattice` cache and the lattice half of `BodiesStatus`

The live hold remains only while a new geometry's BVH builds. Box3D's convex hulls for body-to-body contact are untouched: they're rigid physics, not water.

**Why not one hash table of local bricks for everything:** it needs a second table per Water. And Metal's 64-bit atomics are min and max only, with no return value and no swap, so a 64-bit-keyed insert can't learn it was evicted. Per-tile blocks reuse the table the water already has, and the hot corner pass reads them with no lookup.

## Phases

1. **BVH and winding data** (`manifold-physics`, CPU). Unit tests: on small meshes (a cube, an open box, a subdivided cube), the BVH's nearest distance and winding number match brute force.
2. **GPU queries and builds** (`manifold-water-liquid`). The WGSL port of the BVH queries and `collider_phi`, the static block build, and the moving-lattice build. A GPU proof checks both against the phase 1 CPU queries.
3. **Cut every consumer over** (FLIP, Matter, whitewater in one change) and delete the old path (C9).
4. **Measure.**
   - The GLB scene where water falls through: water on it stays on it.
   - Dam Break and waterBoxTests: frame time with `--frame-clock` and GPU memory, before and after, in one build.
   - BVH build time on the largest import.

Phase 2 mirrors phase 1's layout (`gpu_nodes`, `gpu_triangles`). Phase 3 waits for both.

## Open

- **Thin closed walls.** A closed wall thinner than one cell can still fall between two nodes. C3's shell catches open sheets, not thin closed ones. Measure on real assets before choosing a fix, such as a minimum thickness or a node shift.
- **Moving-lattice memory.** A big moving body at a fine cell size is dense. Measure in phase 4 before choosing sparse bricks.
- **64-bit tile keys** have the same Metal limit, so they need a different insert than one atomic, such as repeated max rounds.
