# One World: one table, one clock, every material

<!-- index: Design for running each Physics World as one shared system: one clock that every solver follows, one tick loop that steps water, Matter, gas and bodies in turn, one world-tile table with a slot per material, and every material pushing every other within the tick through the shared cells. Replaces the per-solver clocks, tables and the frame-late copies between solvers. -->

**Status:** design, 30 Oct night. Peter's goal: "One grid. One clock. All materials and physics." Supersedes D2's "Matter joins later" in [WATER_SHARED_GRID_DESIGN.md](WATER_SHARED_GRID_DESIGN.md). Optimisation waits until everything runs on this.

## Today (read from code, 30 Oct)

Paths are relative to `crates/`.

- **Clocks.** Every solver owns a `SimulationClock`: FLIP `gpu_flip_domain.rs:613` (advanced :1329), Matter `matter_domain.rs:607` (:1650), gas `gas_volume.rs:1001` (:1165), the rigid World `physics.rs:647`. They agree only because each is fed the same played time, Sim Rate and World Speed (`scene_compile/water.rs:626`, `gas.rs:55`). Guards catch drift in the coupled pair (`gpu_flip_domain.rs:1451,1688`, `matter_domain.rs:1800,2016`).
- **Loops.** The engine runs a substep region per solver: a boundary node and a body that repeats (`node-engine/src/exec/execution/substep_region.rs:77`). Regions never nest or chain (`exec/substeps.rs:297,389`). FLIP's region repeats once per tick (`liquid_state.rs:793`) and its step runs up to 6 substeps inside (`gpu_flip_step/primitive.rs:564`). Matter's repeats once per substep, up to 128 a tick (`matter_state.rs:516`). Gas has no region: `gas_volume` encodes all its ticks in one run. So a frame runs all of water's ticks, then all of Matter's, then all of gas's.
- **Tables.** FLIP and Matter use the same `WorldTiles` (`water-liquid/src/world_tiles.rs:309`), same 8³ tiles and keys, but each builds its own instance from its own points: FLIP every substep with reach 2 (`gpu_flip_step/tiles.rs:22`), Matter every 6th substep with reach 1 (`matter_to_grid.rs:213`, `matter.rs:648`). The display builds a third (`surface_tiles.rs`). Gas is a dense box at its own cell (`gas_volume.rs:176`). Cells line up: every solver's cell is the World Cell Size times its Quality scale (`scene_compile/water.rs:839`).
- **Exchange.** Matter reaches water the same frame, after all of Matter's ticks, as a published frame every water substep reads (`water.rs:293`, `forces.wgsl:149`). Water reached Matter a frame late, as one push per frame from the display level set (phase 4 moved it into the tick). Gas reads both a frame late through state capture (`gas_volume.rs:1813`). Bodies step one tick behind the liquid that owns the World (`water-liquid/src/coupling.rs:627`); others hand it their push.

## Decisions

**W1. One clock per World.** The World's clock is the only `SimulationClock`. Every solver in the World takes its `ClockFrame` (ticks, intervals, epoch) instead of advancing its own; the per-solver clocks and the drift guards are deleted. A solver whose setup changes asks the World for a restart, which starts a new epoch for everyone. Each solver still picks its own substeps inside a tick.

**W2. One tick loop.** The executor runs every region of a World tick by tick: tick k of water, then tick k of Matter, then tick k of gas, then the bodies settle tick k, before any solver starts tick k+1. Regions keep their own boundaries and bodies; the World's regions form one co-iterated group whose iteration is the World tick. Within a tick, a later region may read an earlier one's outputs of the same tick. Gas becomes a region like the others.

**W3. One table, a slot per material.** One `WorldTiles` per World: one key space, one hash table, built from every material's points (water particles, Matter points, gas cells, solids). Each entry holds one slot per material that lives there, from that material's own pool, so a sand pile holds no water fields and a tile of both holds both. Reach is per material. A material reads another's slot at the same tile through the shared entry; that is how they push each other. Coarser Quality is a coarser level of the same table (`new_parents`, `gpu_flip_pressure.rs:465`). The per-solver tables are deleted.

**W4. Push within the tick through the shared cells.** Water's tick reads Matter's nodes of the same tick's start; Matter's tick reads water's faces and level set of the same tick's end; gas reads both. Sand is porous in the water's pressure solve (water soaks through, the pile holds water back). `water_on_sand`, the state-capture pushes and the frame-late gas inputs are deleted.

**W5. Bodies in the loop.** Box3D steps once per World tick inside the loop, after every material has pushed it, not one tick behind.

**W6. Terrain is a solid.** Terrain enters the table as a height-field solid every material reads.

## Order

Each phase compiles, runs and commits on its own; a still sheet ends each phase.

1. **One clock** (W1). The World owns the clock; every solver takes its frame through the existing pair hooks that already hand out shared solids (`graph_install/coupling.rs`, `physics_scene.rs`). Behaviour unchanged.
2. **One loop** (W2). The executor co-iterates a World's regions by tick. Gas moves into a region. Behaviour changes: water reaches Matter within the tick.
3. **One table** (W3). Slot per material; FLIP and Matter build and read the World's table.
4. **Push within the tick** (W4, W5). Exchanges move onto the shared cells; the old paths go.
5. **Gas on the table** as a coarse level (W3 for gas).
6. **Terrain** (W6).

Then review, then optimise.

## Phase 2 in code

Phase 1 landed: the lead's clock reaches every taker through `SharedSolids::follow_clock` / `followed_frame` (`water-liquid/src/coupling.rs`), called from `SharedSolidsPair` and `WorldSolidsPair` (`nodes-water/src/physics_scene.rs`).

- **Groups.** `NodePairBehavior::co_iterates()`, true for `SharedSolidsPair`. Regions whose clock owners such pairs link form one group (`co_iterated_groups`, `exec/substeps.rs`), ordered so a member runs after every member it reads; two members reading each other within the tick is a compile error. The plan contracts a group as one block (`ExecutionPlan::region_groups`).
- **Rules inside a group.** A member's body may read another member's body or boundary outputs; outside a group a body wire still may not leave its region, and one region still may not feed another.
- **Runner.** `run_substep_region` is split into open (boundary frame run, restart, pending), one iteration (begin, finish) and release. `run_region_group` opens every member, then for each tick runs each member's iterations whose clock interval `ordinal` is that tick, in group order, then releases every member's held resources together. The interleaved run is one replay span; the async lane is off inside a group until the optimise pass.
- **Matter to water inside the tick.** The water step reads the first Sand's grid as its tick ended (below, "Sand through the Matter's slots"). `water_wetness` still reads `matter_frame` once a frame.
- **Gas** joined the loop after the table (below).

## Phase 3 in code: what one table needs

Read from code after phase 2. Three things make it more than a build change:

- **A table's size is a function of its own pool.** `table_entries(pool_tiles)` sizes the hash (H = the power of two at or above 4P) and every slot and neighbour row sits at an offset from it. 115 Rust sites and about 30 shaders (the whitewater family's `ww_pool_tiles` formula, Matter's stats, grid update and body reaction, the step's sheeting) derive H from their own P. A shared key space has one H for the World, sized for the union of every material's tiles, so each reader must take H as an input instead.
- **Two builds at two rates write one key set.** FLIP rebuilds every substep (reach rings, the cell set C) and Matter every 6th of up to 128 substeps a tick. Ordered linear probing rebuilds the keys from scratch, so each build inserts the other material's held tiles too, and afterwards re-points the other material's entry column and slot-row entries (its slots and neighbour rows stay). Two passes per build, over the other's held slots.
- **One owner.** The step and Matter's Points to Grid each own a `WorldTiles` today. The shared table needs one owner both builds write in place: the lead domain, which hands the taker the buffers through the pair hook, as it hands the solids.

Plan, each stage compiling and bit-identical until 3c:
1. **3a.** Every table reader takes H from its uniform or the World, never from its own P. Mechanical.
2. **3b.** `WorldTiles` builds into layers it does not own: a World table object in the lead domain with one key set and a layer per material (entry column, slot rows, neighbour rows), each build inserting the other layers' held tiles and re-pointing them.
3. **3c.** Matter's Points to Grid builds its layer of the World table; its own table goes.
4. **3d.** Phase 4's exchanges read the other material's slot through the shared entry instead of rescattering particles.

**3a in code.** Every table carries H in a four-word header (`HEADER_WORDS`, `table_header`): keys start at word 4, and the read side's `ts_table_entries()` reads word 0, so an including shader defines only `ts_pool_tiles()` and `ts_table_word(i)`. The build writes the header when it creates the table; `slot_row_word`, `entry_slot_word` and `table_keys` place everything past it. The uniforms that carried H for the read side are padding now (`_pad_h`). Readers of a second table (the pressure's coarse side, the pockets' level, the whitewater restart, the parents build's child) still take that table's H from a uniform.

**3b in code.** Water and Matter share one key set; each keeps its own table buffer (a layer: header, keys, entry column, slot and neighbour rows), so every reader stays as it is.
- **Header.** Word 0 is H, word 1 the table's own pool P. Both sides size H from the sum of the two pools (`shared_table_entries`); a change in either pool changes H, and each side recreates its table fresh (`WorldTiles::with_entries`), as on its own growth.
- **Build.** `Allocation::shared` names the other table. After the seed, `wt_foreign` inserts every tile the other table holds as a key with no slot here; after `wt_finish`, `wt_share` copies the keys into the other table and clears its entry column, and `wt_repoint` points each of its held slots back at its entry. The other table's slots and rows stay. The passes run only while both headers carry the same H, and stop at the other header's P.
- **Wiring.** `share_tables` (`scene_compile/water.rs`) wires Matter's Points to Grid `tiles` and pool into the first Water step's `shared_tiles` and `shared_pool_tiles`, and the step's back into Points to Grid's, as state captures there (`liquid_domain::SHARED_TABLE`). Matter's tick runs first, so the two agree on H from the second frame.
- **H off the header everywhere.** The substep history copies `table_words_at(H, P)`; the whitewater restart saves slot rows in a pass that reads H from the header (`save_slot_rows`). CPU size checks (`table_words(P)`) stay lower bounds.
- **Cost.** The share passes take 0.003–0.006 ms each in water's prepare. H grows with the sum of both pools, so the per-entry passes cost more in sand-and-water scenes (sandwater frame 109.7 → 113.3 ms, measured once); step 5 measures it.
- **Not yet.** Cells line up tile for tile only when both materials run at the same Quality; at different Quality an entry is the same tile key but not the same place, until a coarser Quality reads the shared table's parents level. 3c (one buffer) is no longer needed: a shared key set gives every material the same entry for a tile.

## Phase 4 in code: water pushes Matter within the tick

Landed before the table, on the tick loop:

- **Back edges inside a group.** A body node in a co-iterated group may have state-capture inputs (`Primitive::state_capture_input_ports`) that read another member's body: they take that member's outputs as its last tick left them, and they do not order the group (`exec/substeps.rs`). A capture that reads outside the group stays a compile error.
- **Water on Matter.** `node.matter_water_push` (`water-gpu-mpm/src/primitives/matter_water_push.rs`) sits in the Matter's tick body between `node.matter_state`'s out and Points to Grid, in place, and runs once a tick at its first substep. It reads the first Water's step (`faces`, `distance`, `tiles`, `pool_tiles`, `cell_size`; `liquid_domain::WATER_PUSH_PORTS`) through state-capture inputs, so in tick k the Matter steps against the water of tick k−1's end, and the water steps against the Matter of tick k's end. The lift and drag law is the old one, exact over the tick.
- **Gone.** `node.water_on_sand` and its sampling of the display level set, and `node.matter_state`'s once-a-frame drag (`water_push`, `matter_water_drag.wgsl`).

## W5 in code: bodies in the loop

- **One wait per tick boundary.** A co-iterated group waits for the GPU once between two ticks, before any member starts the next one (`settle_group_tick`, `exec/execution/substep_region.rs`), when a member's clock owner asks for a host step. The lead's host step settles the tick that ended and steps Box3D over it. A region outside a group still waits inside its own iterations.
- **Pairs around the host step.** `NodePairBehavior::before_tick` and `after_tick` run around those host steps for each co-iterating pair. `SharedSolidsPair` (`nodes-water/src/physics_scene.rs`) hands the owner the other liquid's push over the tick that ended (`SharesSolids::tick_push`) before it settles, then hands the other liquid the owner's rows for the next tick (`SharesSolids::take_tick`), which it writes into its body rows and starts its push from zero.
- **So** every liquid pushes the bodies each tick, and every liquid meets them where Box3D put them that tick, not where the first tick's rows carried them. The frame's last tick still reaches the owner through `take_observed_push` before its frame-start settle. Bodies stay one tick behind the liquids' push (explicit coupling); Box3D steps once per tick.

## Gas in the loop

Smoke and Fire are built as water and Matter are: `node.gas_domain` (clock owner, outside the loop: the box, the lead's clock, forces, hits, body rows, light readback), `node.gas_ticks` (the substep boundary; it owns the published state and velocity) and `node.gas_volume` (the body: one tick of the fields). The `SharedSolidsPair` from the World's lead to the gas domain co-iterates, so the gas's region joins the World's group: each tick runs the Matter's, then the water's, then the gas's.

- **Per tick.** The domain's `substep_clock_interval` gives one interval a tick with its scalars (`tick_dt`, `tick_index`, `tick_last`, `tick_forces`, `tick_impulses`, `tick_solid_base`, `tick_solid_count`). The moving Colliders' bits are a domain array (`moving`), not uniforms, so the step needs no CPU data the domain holds.
- **Water and sand within the tick.** The step reads the first Water's step (`faces`, `distance`, `tiles`, `pool_tiles` and its cell) through the water's own tiles at every gas cell (`gas_water`, the Matter water push's sampling), and the first Sand's grid each tick (`sand_at`). The frame-late particle frames and their state captures are gone. With no World item nothing is grouped, so the gas reads neither.
- **Published.** The step writes the fine state and velocity into the loop's arrays on the frame's last tick (`cs_publish_velocity`), and the light sums into the domain's ring slot.
- **Not yet.** The gas is still a dense box at its own cell, not on the table (phase 5).

## Porous sand in the pressure solve

The water's solve treats solids by Batty's cut cells: each face's open fraction `w` (`l.s`, `open_fractions`) and the solid's velocity `v_s` on the part it covers (`l.v`, `solid_face_velocity` then `solid_extrapolate`); the divergence holds the mixture's flux `w·u + (c − w)·v_s` (`divergence.wgsl`), the pressure weights are `w`, and the projection holds covered faces to `v_s`. Sand joins as a porous moving solid. Each grain carries its material's solid share `s`, the share of its bulk its grains fill (`matter.rs` `solid_share`, row float 13: 0.6 for sand, ice's share for snow). The water reads the sand's bulk, solid volume and velocity at each cell's low corner from the Sand's own grid (`sand_at`, below); after the solids' velocity, `sand_porous` (`forces.wgsl`) closes each of the cell's faces no body or wall cuts (`w = 1`) by the solid share there, never more than the packing (the solid sum over the bulk once the bulk passes a full cell), and sets `v_s` there to the sand's mean velocity, with no friction and no body code. The solve then makes the water give way to sand coming in and pass a pile only through its pores; `sand_drag` stays as the drag inside the pores. Faces a body cuts keep the body's `w` and `v_s`, so the body reaction is unchanged.

Only porous materials close faces. A continuous one (solid share 1: clay, mud, goo, jelly, liquid metal, lava) would seal the water inside it off into pockets: closed faces cost the pocket pass 13 times its time and the full scene three times its frame, as the hard-solid sand once did (MATTER_SAND_DESIGN.md). They keep their drag only.

## Phase 6 in code: Terrain as a solid

W6 landed before the table, as a solid every solver already reads:

- **Ground out of the Terrain.** While a Collider reads `node.terrain`'s `source`, the settled land's heights are read back once (through the snapshot buffer, taken when its command buffer completes, so the load's warm-up lands it too) and published as `MeshSource::HeightField` (`node-engine/src/scene/mesh_source.rs`). Rain frames publish nothing new; a changed land reads back again once it settles.
- **Bodies.** `rigid_body` turns it into `Solid::HeightField` (`physics/src/collision_shape.rs`), cooked as a Box3D height field on a fixed body (`cooked.rs`, `native/box3d/bridge.c`). Box3D's grid starts at its body's origin, so the body sits at the grid's first point (`CookedSolid::placement`, added by `add_cooked` and `update_body`, taken off again by `PhysicsWorld::pose`): poses go in and come out as authored, and the Terrain draws at its body's pose like any Collider. The body's centre of mass (`dynamics`) stays Box3D's, the frame the water's geometry is built in.
- **Water, Matter, smoke.** The coupling builds `PreparedFluidGeometry::ground` (`water-liquid/src/fluid_role.rs`): its tree is `MeshBvh::height_field`, one root with radius -1 holding the grid, three heights to a vec4. `cb_phi` (`collider_bvh.wgsl`) answers it with `cb_height_phi`: the bilinear ground's distance, solid below, nothing past the grid's edge. Every solver reads solids through `cb_phi`, so all of them collide with it.
- **Limits.** Terrain takes Collider, never Body. A Terrain moved live gets a lattice, as any moving collider; at Terrain sizes that lattice is large.

## What is left of the goal

Every solver now runs one clock and one loop, each tick in order (Matter, water, gas, then the bodies settle), and no solver reads another a frame late. Water and Matter share one key set; the gas is on world tiles of its own (below). One piece keeps the table from being the World's only grid (sand through the Matter's slots landed, below):

- **One solid layer.** Each solver still lays the scene's solids at its own cells: the water caches static corners per held slot (`static_corners`, laid for fresh slots), the Matter reads the trees and the height field at its nodes (`matter_tree.wgsl`), the gas lays a solid field on its own tiles. The table form is a solid layer beside the material layers: a slot for every key any material holds, static corners laid once per fresh slot from `cb_phi` (Colliders and the Terrain's height field), the moving bodies still sampled from their lattices each tick. The water's static corners and the Matter's node solids then read it through the shared entry, and the gas samples it at its cells. Needs a union pool (the sum of both pools, as H is) and a pass after both builds; a different Quality reads it through the parents level.

## Gas on world tiles

Smoke and Fire keep their fields in their own `WorldTiles` table at the air cell, the table water and Matter keep theirs in (docs/SMOKE_AND_FIRE_MAP.md, "Grid"). Every stepped tick rebuilds it from the source's tiles (`cs_source_points`) and every tile still holding gas (the build's kept words, marked by `cs_fine_sources`), with a ring of one tile, clipped to the box; the pool is every tile the clip can hold. Every kernel runs over the held slots from the table's indirect triple, a record per cell (the fine state as FINE³ planes of the records), and a tile the table does not hold is open air: pressure 0, still, no gas. A slot the build hands out is zeroed (`cs_fresh`) and gets its still solids (`cs_solids_static` over the fresh list). Fine kernels run a thread a fine cell (`cs_fine_args` lays FINE³ times the held triple), and the pressure solves a tile per workgroup, even tiles then odd, in place. The frame's last tick clears the published dense fields and scatters the held cells into them, so the renderer is unchanged. The water's table is read under `ws_` and the Sand's under `ms_`; the gas's own is `ts_`.

The gas's air cell is twice the World's cell, so its tile keys name other places than water's and Matter's; it does not share their key set, which no reader uses yet (step 5 decides that set).

## Sand through the Matter's slots

The Sand's grid is the only place its bulk lives; water and gas read it there instead of a copy of every grain.

- **Bulk.** On the tick's last substep, while a reader is wired, `node.matter_to_grid` adds each point's rest volume and the part of it a porous material's grains fill (solid share under 1; continuous materials add none) to the same 27 nodes as its mass, in Matter cells at `SAND_FIXED` (`bulk`, two words a pooled node, `sand_bulk.wgsl`). The clear zeroes it over the held tiles on that substep only. The template wires the boundary's `tick_end` into Points to Grid.
- **Read.** `sand_at` (`water-liquid/.../sand_read.wgsl`) samples the eight nodes round a world point through the Sand's own table: the bulk's share of space, the grains' share, and the bulk-weighted mean of the grid's velocities after forces and boundaries (`node.matter_grid_update`'s nodes). A point whose base node's tile the table does not hold costs one lookup. A shader holding two tables composes the second read side under its own prefix (`world_tiles::read_side_as("ms_")`); the Sand's pool is its table's header word 1.
- **Water.** `sand_drag` and `sand_porous` each read `sand_at` at the cell's low corner: the drag, the closed share and the solid velocity are the old law. `sand_corners`, the step's five-word share field and the grains it scattered every substep are gone.
- **Gas.** `cs_solids` reads `sand_at` at each cell centre: a cell at least half full of sand moves with it. `cs_matter` and the gas's grain field are gone.
- **Gone.** `node.matter_stats`' `grain_view`, `MatterGrain` and `matter_grain_view.wgsl`. `MatterGridNode` moved to `water-liquid` (`fluid_particles.rs`) so water and gas name it.
- **Wiring.** `wire_sand` (`scene_compile/water.rs`) finds the grid, Points to Grid, the pool and the cell through the Sand's tick-end stats and wires them into each Water's step and each Smoke or Fire (`liquid_domain::SAND_GRID`).
- **Cells.** The read works at any pair of Qualities: it samples the Sand's nodes at the reader's world point, so the water and the Sand need not share a cell. The Sand's quadratic weights spread a grain's bulk over a cell more than the old trilinear corners did.

## Risks

- W2 changes the executor's region contract: replay caches and the async lane are per region today (`substep_region.rs:150-200`); a co-iterated group must keep both or the frame gets slower.
- W3 changes the table layout every tile reader uses (READ_SIDE): the slot lookup takes a material.
- Matter runs up to 128 substeps a tick and water at most 6, so the loop iterates by tick and each solver keeps its own substeps inside.
