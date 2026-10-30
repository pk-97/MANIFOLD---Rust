# One World: one table, one clock, every material

<!-- index: Design for running each Physics World as one shared system: one clock that every solver follows, one tick loop that steps water, Matter, gas and bodies in turn, one world-tile table with a slot per material, and every material pushing every other within the tick through the shared cells. Replaces the per-solver clocks, tables and the frame-late copies between solvers. -->

**Status:** design, 30 Oct night. Peter's goal: "One grid. One clock. All materials and physics." Supersedes D2's "Matter joins later" in [WATER_SHARED_GRID_DESIGN.md](WATER_SHARED_GRID_DESIGN.md). Optimisation waits until everything runs on this.

## Today (read from code, 30 Oct)

Paths are relative to `crates/`.

- **Clocks.** Every solver owns a `SimulationClock`: FLIP `gpu_flip_domain.rs:613` (advanced :1329), Matter `matter_domain.rs:607` (:1650), gas `gas_volume.rs:1001` (:1165), the rigid World `physics.rs:647`. They agree only because each is fed the same played time, Sim Rate and World Speed (`scene_compile/water.rs:626`, `gas.rs:55`). Guards catch drift in the coupled pair (`gpu_flip_domain.rs:1451,1688`, `matter_domain.rs:1800,2016`).
- **Loops.** The engine runs a substep region per solver: a boundary node and a body that repeats (`node-engine/src/exec/execution/substep_region.rs:77`). Regions never nest or chain (`exec/substeps.rs:297,389`). FLIP's region repeats once per tick (`liquid_state.rs:793`) and its step runs up to 6 substeps inside (`gpu_flip_step/primitive.rs:564`). Matter's repeats once per substep, up to 128 a tick (`matter_state.rs:516`). Gas has no region: `gas_volume` encodes all its ticks in one run. So a frame runs all of water's ticks, then all of Matter's, then all of gas's.
- **Tables.** FLIP and Matter use the same `WorldTiles` (`water-liquid/src/world_tiles.rs:309`), same 8³ tiles and keys, but each builds its own instance from its own points: FLIP every substep with reach 2 (`gpu_flip_step/tiles.rs:22`), Matter every 6th substep with reach 1 (`matter_to_grid.rs:213`, `matter.rs:648`). The display builds a third (`surface_tiles.rs`). Gas is a dense box at its own cell (`gas_volume.rs:176`). Cells line up: every solver's cell is the World Cell Size times its Quality scale (`scene_compile/water.rs:839`).
- **Exchange.** Matter reaches water the same frame, after all of Matter's ticks, as a published frame every water substep reads (`water.rs:293`, `forces.wgsl:149`). Water reaches Matter a frame late, as one push per frame: `water_on_sand` samples the display level set and velocity into a state-capture wire (`water.rs:372`, `matter_state.rs:271,457`). Gas reads both a frame late through state capture (`gas_volume.rs:1813`). Bodies step one tick behind the liquid that owns the World (`water-liquid/src/coupling.rs:627`); others hand it their push.

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

## Risks

- W2 changes the executor's region contract: replay caches and the async lane are per region today (`substep_region.rs:150-200`); a co-iterated group must keep both or the frame gets slower.
- W3 changes the table layout every tile reader uses (READ_SIDE): the slot lookup takes a material.
- Matter runs up to 128 substeps a tick and water at most 6, so the loop iterates by tick and each solver keeps its own substeps inside.
