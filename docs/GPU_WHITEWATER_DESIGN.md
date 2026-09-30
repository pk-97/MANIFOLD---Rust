# GPU Whitewater — spray, foam and bubbles for any GPU liquid, from FLIP's own lifecycle

<!-- index: Spray, foam and bubbles for SWASH and MPM water: GPU atoms find the emitters and spawn whitewater from the seam's face grid and the surface's level set; the vendored FLIP C++ lifecycle advances it through a fenced shared-memory ring. Builds the liquid seam's P10 grid outputs. -->

**Status:** PROPOSED · 2026-09-30 · Opus 5.5 · P1–P6 not built · owed: approval.
**Prerequisites:** LIQUID_SOLVER_SEAM_DESIGN.md P1 (shared liquid module) merged into `feat/fft-water`; SWASH's full step (FFT_WATER_SOLVER_DESIGN.md P3) on `feat/fft-water`. This design's P1 is the seam's P10 (Grid outputs). The seam's P7a (`node.liquid_frame`) is not built, so SWASH reaches whitewater through its render harness until it is (section 3.6 (Solver feeds)). Branch: `feat/gpu-whitewater` off `feat/fft-water`.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) before starting any phase.

Peter, 2026-09-30, on BUG-imy3 (GPU whitewater, solver-agnostic): "move the spawn search to the GPU and reuse FLIP's own foam and bubble code."

FLIP's whitewater costs 42–55 ms per tick at 64, and about 92% of that is two jobs that are parallel by nature: scanning every liquid particle for emitters, and the curvature grid (BUG-imy3 notes). The life of the few thousand whitewater particles afterwards (advection, buoyancy, drag, collisions, lifetime, removal) costs about 1 ms and is FLIP's tuned behaviour. **So the GPU finds emitters and spawns whitewater from what the seam already publishes, and the unchanged vendored C++ advances it, fed through shared memory after a fence.** Any solver that publishes the seam's face grid gets whitewater; FLIP keeps its native whitewater as the reference (seam D3).

Companions: [LIQUID_SOLVER_SEAM_DESIGN.md](LIQUID_SOLVER_SEAM_DESIGN.md) (the seam; D5 and section 3.2 (Grid outputs) fix the face layout and the level-set source), [GPU_FLUID_SURFACE_DESIGN.md](GPU_FLUID_SURFACE_DESIGN.md) (the particle frame and the level set; its P8 fixes the whitewater output shape), FFT_WATER_SOLVER_DESIGN.md (SWASH, on `feat/fft-water`), [GPU_MPM_SOLVER_DESIGN.md](GPU_MPM_SOLVER_DESIGN.md) (its P6 whitewater plan is superseded here), [DECOMPOSING_GENERATORS.md](DECOMPOSING_GENERATORS.md) and [ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md) (atom rules).

Binding from outside this doc: never a GPU port of FLIP's solver; no FLIP tuning or integration (seam D3); no edit to vendored FLIP here; no fallback modes; a CPU extent proof before every new GPU size, and GPU runs at res 64 only.

## What it does on stage

A SWASH or MPM dam break throws spray off its front, lays foam on breaking crests and churns bubbles under the impact, the way FLIP's does, for about 3 ms a frame instead of 42–55. Pause freezes the foam where it is. Reset clears it with the water. An export matches the preview within one frame. Whitewater trails the water surface by one display frame offline and one or two live, because the live frame never waits for the GPU.

## 1. Audit — what exists (verified 2026-09-30)

Extend, don't redesign. `F/` is `crates/manifold-fluids/native/flip_engine/`, `R/` is `crates/manifold-renderer/src/node_graph/`.

| Piece | Where | State |
|---|---|---|
| FLIP whitewater tick | `F/diffuseparticlesimulation.cpp:55` (`update`): material grid, then emission only if enabled (`:80`), then advance `:2152`, types `:2033`, lifetimes `:2101`, removal `:2761` | lifecycle reused unchanged; emission disabled |
| Emitter scan | `:1497` (emitters), `:1571` (jitter `:1557`, band 1.5 cells `diffuseparticlesimulation.h:463`, bordering air), `:1688` (surface emitters), `:1718` (wavecrest), `:1768` (energy) | ported to GPU atoms |
| Emission | `:1989` (count, `(int)(n + 0.5)` per emitter per substep), `:1912` (cylinder placement, 0.25-cell solid buffer, lifetime) | ported |
| Type rule | `:2056` (spray outside the boundary; foam within 1 cell of the surface; bubble below; foam or spray not bordering air become bubble) | ported for spawn; the lifecycle keeps its own |
| Boundary box | `:2159` (the grid shrunk 3 cells) | drives D2 |
| Curvature | `F/particlelevelset.cpp:196` (reinit `:263`, valid nodes `:692`, formula `:728`); 3 extension layers, band 3, out-of-range 5 cells (`particlelevelset.h:143-145`) | formula ported; reinit replaced (D4) |
| Upwind reinit | `F/levelsetsolver.cpp:82`, step `:315` | not used (D4) |
| FLIP's inputs | `F/fluidsimulation.cpp:6947` (`_updateDiffuseMaterial`), per substep in `_stepFluid` `:10995` | 1.00 substeps per 1/60 s tick at 64 (measured 2026-09-30) |
| Marker radius | `F/fluidsimulation.cpp:4517`: cbrt(3h³/(32π)) ≈ 0.31h | the emitter cylinder is 8× this |
| Engine arrays | `F/macvelocityfield.h:92` (`getRawArrayU/V/W`), `F/particlelevelset.h:71` (`getPhiGrid`), `F/meshlevelset.h:101` (`constructMinimalLevelSet`), `:156` (`getPhiArray3d`), `F/array3d.h:391` (`getRawArray`) | whole-array copy targets (D6) |
| Load path | `F/diffuseparticlesimulation.cpp:1480` (`loadDiffuseParticles`) never refreshes the cached size (`F/particlesystem.h:72`); `update` returns early on size 0 (`:95`) | trap: the glue calls `getDiffuseParticles()->update()` after every load |
| FLIP-native path | `crates/manifold-fluids/native/bridge.cpp:1367` (options), `:665` (refresh); `R/primitives/fluid_surface.rs:123` (foam, bubbles, spray, counts) | the reference, unchanged |
| Fade rule | `R/fluid.rs:319` (`WhitewaterFrame::fill`: scale √clamp(lifetime/0.2)) | one shared function (D7) |
| Output shape plan | GPU_FLUID_SURFACE_DESIGN.md P8 (Whitewater at 60 fps): `FluidParticle` per population, id 0, radius = fade | adopted (D7) |
| Copies | `R/primitives/particles_to_copies.rs:26` (`live_count` holes) | exists |
| Particle frame | `R/primitives/matter_frame.rs:80` (outputs), `R/liquid/frame_ring.rs:12` (3 slots) | read |
| Clock | `R/liquid/clock.rs:10` (`ClockFrame { ticks, epoch }`); `R/primitives/matter_domain.rs:167-169` (`ticks`, `epoch` outputs); `R/fluid.rs:48` (`TICK` = 1/60) | read (D12) |
| Level set | `R/primitives/particle_volume.rs:54`: nearest-blob distance, capped at 0.1 bin outside, bounded near −a inside | not a distance past the cap (D4) |
| Surface group | `WaterDamBreakGpu.json` group `liquid_surface`: particle_volume → 3 × smooth_lattice → mesh | exports `level_set` in P1 |
| Face contract | LIQUID_SOLVER_SEAM_DESIGN.md section 3.2 (Grid outputs), P10 (Grid outputs) | built here as P1 |
| SWASH faces | `R/primitives/swash_preset.rs:609` (`new`: projected faces extended 2 layers) | P1 reads it |
| SWASH render harness | `R/primitives/swash_preset.rs:465` (`render_def`), `:437` (whitewater objects left out) | P6 wires them back |
| Scan | `R/primitives/running_total.rs:31` | exists |
| Gather by running total | `R/primitives/select_flagged.rs:36` (binary search) | precedent for spawn |
| GPU→CPU ring | `R/primitives/matter_state.rs:49` (`ReadbackSlot`), `:118` (poll), `:311` (capture; skip when all in flight) | precedent (D6) |
| CPU→GPU ring | `R/fluid/particle_ring.rs:1` (read stamps, admission, growth) | precedent (D7) |
| Frame clock | `crates/manifold-gpu/src/metal/retire.rs:159` (`stamp` `:176`, `is_complete` `:181`, `wait` `:189`) | used |
| Instance upload | `R/instance_upload.rs:30` (64 instances per dispatch) | not used (D7) |
| Record types | `R/fluid_particles.rs:126` (`FaceSample`, `KnownItem` `:141`) | precedent for `WhitewaterSpawn` |
| Extent proofs | `R/primitives/swash_extent_tests.rs` | precedent |
| Cost and look | BUG-imy3 notes: whitewater 1.3–2.3% of pixels, almost all wavecrest foam; bubbles ≤ 0.23%, spray ≤ 0.09%; turbulence emission inert at rate 175 | D9 |

### 1.1 Primitive audit

DECOMPOSING_GENERATORS.md section 2.5 (primitive audit): survey `rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/ -g "*.rs"`; reference presets `WaterDamBreakGpu.json` (FLIP whitewater into the three copies objects, ids 44, 47, 50) and `WaterDamBreakMatter.json`, read end to end.

| Job | Verdict | Nearest existing, and why it isn't it |
|---|---|---|
| Faces from SWASH, from MPM | new (seam P10) | `faces_to_particles` is SWASH's own transfer on its padded lattice |
| Level set out of the surface group | one wire away | a group output |
| Crossings, nearest crossing, signed distance | new | `smooth_lattice` smooths; `sample_volume_at_particles` reads Texture3D only |
| Liquid, air and solid cells | new | `cells_with_particles` counts particles; `collar_cells` has no solid and no shrink |
| Curvature, one layer of extension | new | `edge_slope_3d` is a Texture3D gradient; `extend_faces` extends `FaceSample` faces only |
| Jitter | new | `position_jitter` is simplex noise on `InstanceTransform`; `spread_out` kicks 2D `Particle` velocity |
| Faces at particles | new | none samples the seam arrays |
| Energy, wavecrest, emission count, type | new | none |
| Scan | exists | `running_total` |
| Spawn by running total | new | `select_flagged` is the search precedent; it places nothing |
| Lifecycle | new, one FFI node | `fluid_surface` runs a whole FLIP world |
| Copies | exists | `particles_to_copies` |

## 2. Decisions

**D1 — The split is at the emitter.** GPU atoms find emitters and write spawn records; the vendored `DiffuseParticleSimulation`, emission disabled, advances them. Peter's words above. Rejected: a GPU lifecycle, because it discards FLIP's tuned behaviour and is the port Peter declined. Rejected: FLIP's CPU emitter fed GPU fields, because the scan is the cost.

**D2 — One whitewater grid: the frame's solid lattice cells.** At res n that is n+6 cells a side from m − 3h (70³ at 64), whose nodes are the solid lattice exactly. The level set, the solid and the faces all sit on it at integer offsets, and FLIP's boundary box (3 cells in) lands on the tank walls. Rejected: the seam's face grid (n cells at m), because the 3-cell box would kill whitewater within 19 cm of every wall at 64. Rejected: FLIP's own grid (n+3 cells at m − 1.5h), because it sits half a cell off every GPU lattice. **Consequence, stated honestly:** FLIP's box sits 1.5 cells inside the walls; ours reaches 1.5 cells closer to them.

**D3 — The liquid field is the Liquid Surface group's level set, re-distanced on the whitewater grid** (seam D5). It is the field the mesh is drawn from, after smoothing, so foam sits on the surface the audience sees. Rejected: FLIP's particle level set rebuilt from `particles_b`, because it is a second distance field (seam D5) and foam would float on a surface nobody sees. **Consequence:** a smoothed surface has calmer curvature than FLIP's union of spheres, so small crests may emit less. The side-by-side decides; tuning is Peter's call (section 8).

**D4 — Re-distance by nearest crossing.** Crossings come from the refined lattice, where the field is a true distance up to the cap; three passes spread each cell's nearest crossing to its 26 neighbours; the signed distance is clamped at 4 cells. FLIP's liquid-into-solid rule (`F/particlelevelset.cpp:170`) is kept: solid cells touching liquid read negative, and edges touching solid carry no crossing, so a submerged wall is not a surface. Rejected: FLIP's upwind reinit from the capped field, because its sign speed outside the cap is about 0.1, so 6–8 passes leave the air side near half a cell and spray reads as foam. Rejected: a brute-force search of every edge within 3 cells, at about 50× the reads.

**D5 — Emission is per tick, from the frame's last tick.** n = T · (int)(rate · Ie · Iwc · TICK · 8/ppc + 0.5) with T the domain clock's ticks this frame and ppc the solver's particles per cell. FLIP's rate is calibrated at 8 markers per cell, so the 8/ppc factor keeps the amount independent of a solver's sampling density. Rejected: emitting inside the tick region, because the lifecycle runs on the CPU between ticks and live cannot stop the GPU for it. **Consequence:** when a frame runs T > 1 ticks (30 fps export), all T ticks' spawns come from the last tick's state; 30 and 60 fps exports agree statistically, not tick for tick.

**D6 — Handoff: a GPU snapshot into a ring of shared slots, read after its fence, copied whole into the engine.** Section 3.5 has the rules. Rejected: reading graph arrays in place, because they are pooled and the next frame's GPU work overwrites them while the CPU reads. Rejected: aliasing the engine's arrays onto shared memory, because `Array3d` owns its storage and changing that edits vendored FLIP. **Consequence, stated honestly:** "no copies" becomes "no CPU re-layout": one GPU blit per tick (9.2 MB at 64) and whole-array CPU copies into the engine (7.2 MB, faces row-strided into the padded grid), measured in P6.

**D7 — Output is GPU_FLUID_SURFACE_DESIGN.md P8's whitewater-frame shape.** `foam_particles`, `bubble_particles`, `spray_particles` as `FluidParticle` (id 0, radius = the fade scale of `WhitewaterFrame::fill`, velocity kept), written by the CPU straight into a ring of shared buffers (`fluid/particle_ring.rs` precedent), turned into copies by `node.particles_to_copies`. Rejected: `InstanceSnapshotUpload`, about 1,600 dispatches at 100,000 particles. Rejected: `InstanceTransform` outputs, because P8 gives FLIP's whitewater this shape and one shape serves both.

**D8 — Capacity is FLIP's budget, never an error.** Spawn slots = the lifecycle capacity C (default 100,000, FLIP's). Past C, slot j takes emission index ⌊j · total / C⌋, a uniform subset; loads are trimmed to C − live the same way. Both counts are reported as `thinned`.

**D9 — Dropped:** turbulence and inside emitters (inert at 175), dust, the obstacle influence grid (uniform 1), the spray emission speed factor (1 is a no-op), the emitter generation coin (rate 1), foam preservation (off in FLIP's defaults).

**D10 — Randomness:** stateless hashes of (index, seed, epoch) on the GPU; the lifecycle's own RNG seeded with the epoch (`setRandomSeed`). There is no bit-exact oracle; the FLIP oracles are statistical over seeds (BUG-imy3 notes).

**D11 — The lifecycle runs in the node's `run` on the content thread.** No new thread. Defaulted, with a trigger: if `lifecycle_ms` p95 exceeds 3 ms at 64, stop and escalate (a worker thread needs Peter's approval).

**D12 — Ticks, epoch and gravity come from the domain.** Every GPU liquid domain exposes `ticks` and `epoch` (the `LiquidClock` frame) and its gravity; the whitewater never infers time from the particle frame. A changed epoch clears the population; T = 0 holds everything.

**D13 — The chain is one node group, "Whitewater",** authored in a reference preset (section 3.4) and copied into scenes like the Liquid Surface group. Scenes wire ports, never atoms.

## 3. Design body

### 3.1 Grids

At res n with cell size h and tank minimum m (64: h = 0.0625 m):

| Grid | Size | Origin | Source |
|---|---|---|---|
| Seam faces | U (n+1)·n·n, V n·(n+1)·n, W n·n·(n+1) | m | `face_u/v/w` |
| Whitewater grid | n+6 cells a side | m − 3h | the frame's `grid_bounds`, `grid_nodes_x/y/z` minus one |
| Solid | n+7 nodes a side | m − 3h | the frame's `solid_b` |
| Level set | (n+6)·s + 1 nodes a side | m − 3h | the group's `level_set`, s = its refinement |

Derived at run time, never assumed: pad = ((grid_nodes − 1) − face_cells)/2, an integer and equal on every axis; s = (level_set_nodes − 1)/(grid_nodes − 1), an integer. Anything else is a named refusal. At 64: 70³ = 343,000 cells, 357,911 solid nodes, 211³ level-set nodes, 266,240 faces per axis.

### 3.2 What the whitewater reads

These are the ports the seam's P10 entry asks this design to name.

| Port | From | Use |
|---|---|---|
| `particles_b`, `count_b` | the particle frame | emitter candidates |
| `solid_b`, `grid_bounds`, `grid_nodes_x/y/z` | the particle frame | solid, whitewater grid |
| `face_u/v/w`, `face_cells_x/y/z`, `face_valid_layers` | the frame (seam `FACE_GRID_PORTS`) | velocity; `face_valid_layers` < 1 is a named refusal |
| `level_set`, `level_set_nodes_x/y/z` | the Liquid Surface group | liquid field |
| `ticks`, `epoch`, `gravity_x/gravity/gravity_z`, `points_per_cell`, `simulation_time` (seed) | the domain | D5, D10, D12 |

### 3.3 Atoms

Every atom is `fusion_kind: Pointwise` on the codegen path with `BufferGather` inputs, unless marked. Grid atoms run once per whitewater cell (343,000 at 64), particle atoms once per `particles_b` slot, spawn atoms once per spawn slot (C).

| Atom | Runs over | In → out | Rule |
|---|---|---|---|
| `node.surface_crossings` | grid | level set, solid → `Array(Vec4)` | xyz: nearest zero crossing on refined edges inside the cell's footprint (144 edges at s = 3), edges touching solid skipped; none = (1e6, 1e6, 1e6). w: the level set at the cell centre, trilinear |
| `node.nearest_crossing` | grid | crossings → crossings | the nearest of the cell's and its 26 neighbours' crossings; w kept. Run 3 times |
| `node.crossing_distance` | grid | crossings, solid → `Array(f32)` | sign(w) · min(distance, 4h); a solid cell with a liquid 6-neighbour reads −0.5h (D4) |
| `node.liquid_cells` | grid | distance, solid → `Array(u32)` | solid if the solid at the centre (8-node mean) < 0, else liquid if distance < 0, else air; then FLIP's shrink: liquid with an air 6-neighbour becomes air (`F/diffuseparticlesimulation.cpp:1609`) |
| `node.lattice_curvature` | grid | distance → `Array(Vec4)` | x: FLIP's formula (`F/particlelevelset.cpp:728`), clamped ±1/h, on cells where it and its 6 neighbours have \|d\| < 2h, off the border; y: 1 there, else 0 |
| `node.extend_lattice` | grid | `Vec4` → `Vec4` | one layer of FLIP's extrapolation: an unknown cell with known 6-neighbours takes their mean. Run 3 times |
| `node.jitter_particles` | particles | `FluidParticle` → same | position ± 0.25·(1 − 1e-3)·h per axis, uniform (`:1557`) |
| `node.sample_faces_at_particles` | particles | particles, faces → particles | velocity = FLIP's MAC trilinear (`F/macvelocityfield.h` `evaluateVelocityAtPositionLinear`): out-of-range corners read 0 |
| `node.energy_potential` | particles | particles → `Array(f32)` | Ie = (clamp(½\|v\|², min, max) − min)/(max − min); 0.1 and 60 by default (`:1768`) |
| `node.wavecrest_potential` | particles | particles, distance, curvature, cells → `Array(f32)` | 0 unless \|d\| < 1.5h and the cell borders air (26 neighbours); else FLIP's `:1718`: k·h ≥ 0.4, clamped at 1, v̂·n ≥ 0.4 |
| `node.emission_count` | particles | energy, wavecrest, particles → `Array(u32)` | D5; 0 when \|v\| < 1e-3 |
| `node.running_total` | particles | counts → offsets | exists; boundary `BarrieredReduction` |
| `node.spawn_whitewater` | spawn slots | offsets, particles, energy, faces, solid → `Array(WhitewaterSpawn)` | slot j < min(total, C): emitter by binary search (D8 stride past C); cylinder of radius 8 · 0.31h · √Xr about v̂, height Xh · \|v\| · TICK; rejected outside the grid or where the solid < 0.25h; lifetime = min + Ie · (max − min) ± variance (0, 7, 3), rejected ≤ 0; velocity from the faces. Rejected and unused slots write lifetime 0 |
| `node.whitewater_type` | spawn slots | spawns, distance, cells → spawns | FLIP's `:2056` for a fresh particle |
| `node.whitewater_lifecycle` | CPU | section 3.4 | `boundary_reason: CrossFrameState`; FFI |

P1's atoms (the seam's P10):

| Atom | Rule |
|---|---|
| `node.face_sample_component` | one axis of SWASH's `FaceSample` lattice into the seam array, padding skipped; param `axis` |
| `node.matter_face_component` | one axis from the MPM grid: the mean of the four nodes around each face centre, after the lattice padding (`R/matter.rs:397`, `:402`); param `axis` |

Shared WGSL, via `wgsl_includes` (`R/liquid/bodies.rs` precedent): `liquid_faces.wgsl` (FLIP's MAC trilinear on the seam arrays) and `whitewater_common.wgsl` (hash, grid trilinear, 26-neighbour air test). The particle chain from `jitter_particles` to `emission_count` must fuse into at most two dispatches and `spawn_whitewater` + `whitewater_type` into one (⚠ VERIFY-AT-IMPL: `cargo run -p manifold-renderer --bin graph-tool -- fusion` on the reference preset).

### 3.4 Committed types and ports

```rust
// crates/manifold-fluids/src/whitewater.rs — one definition; the renderer
// implements KnownItem for it (R/fluid_particles.rs, beside FaceSample).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WhitewaterSpawn {
    /// Scene metres; w is lifetime in seconds, ≤ 0 marks an empty slot.
    pub position_lifetime: [f32; 4],
    /// m/s.
    pub velocity: [f32; 3],
    /// FLIP's DiffuseParticleType: 0 bubble, 1 foam, 2 spray.
    pub kind: u32,
}
const _: () = assert!(std::mem::size_of::<WhitewaterSpawn>() == 32);

pub struct WhitewaterGrid { pub cells: [u32; 3], pub cell_size: f32, pub origin: [f32; 3] }

pub struct WhitewaterFields<'a> {
    pub face_u: &'a [f32], pub face_v: &'a [f32], pub face_w: &'a [f32], // seam layout
    pub face_cells: [u32; 3],
    pub face_offset: [u32; 3],   // pad, in whitewater cells
    pub level: &'a [f32],        // whitewater cell centres
    pub solid: &'a [f32],        // whitewater nodes
    pub gravity: [f32; 3],
}

pub struct WhitewaterLifecycle { /* owns the native DiffuseParticleSimulation and its grids */ }
impl WhitewaterLifecycle {
    pub fn new(grid: WhitewaterGrid, capacity: u32, seed: u64) -> Result<Self, FluidError>;
    pub fn clear(&mut self, seed: u64);
    pub fn set_fields(&mut self, fields: &WhitewaterFields<'_>) -> Result<(), FluidError>;
    /// Loads live records (lifetime > 0) up to capacity − live, stride-thinned; returns (loaded, thinned).
    pub fn load(&mut self, spawns: &[WhitewaterSpawn]) -> Result<(u32, u32), FluidError>;
    pub fn step(&mut self, dt: f64) -> Result<(), FluidError>;
    pub fn particles(&mut self, out: &mut Vec<WhitewaterParticle>) -> Result<(), FluidError>;
}
```

Bridge entries are new glue in `bridge.cpp` (`manifold_fluids_whitewater_*`); no file under `flip_engine/` changes. The oracle entries (`manifold_fluids_oracle_curvature`, `manifold_fluids_oracle_emit`) build only under the `whitewater-oracle` cargo feature and call public FLIP API: `ParticleLevelSet::calculateCurvatureGrid`, and `DiffuseParticleSimulation::update` with emission on, turbulence rate 0, lifetime variance 0.

`node.whitewater_lifecycle` ports:

| Inputs | Outputs | Params |
|---|---|---|
| `spawns` Array(WhitewaterSpawn), `offsets` Array(u32), `count` (emitter slots), `face_u/v/w`, `face_cells_x/y/z`, `level` Array(f32), `solid` Array(f32), `grid_bounds`, `grid_nodes_x/y/z`, `ticks`, `epoch`, `gravity_x/gravity/gravity_z` | `foam_particles`, `bubble_particles`, `spray_particles` Array(FluidParticle); `foam_count`, `bubble_count`, `spray_count`, `emitted`, `thinned`, `dropped_ticks`, `lifecycle_ms` ScalarF32 | `capacity` Int 1–250,000, default 100,000 |

The "Whitewater" group exposes the ports of section 3.2 as inputs, the lifecycle's outputs, and params Capacity, Wavecrest Emission (175), Min Energy (0.1), Max Energy (60). Its source of truth is `assets/reference-presets/WaterDamBreakMatterWhitewater.json` (WaterDamBreakMatter plus the group and the three copies objects; the loader does not scan the folder, so nothing ships). The SWASH harness copies the group from it, as it copies the Liquid Surface group from `WaterDamBreakGpu.json` (`swash_preset.rs:426`).

### 3.5 Handoff and fence rules

The lifecycle node owns two rings (clock: `gpu.device.frame_clock()`).

- **Snapshot ring**, 3 slots, each shared buffers for spawns (C × 32 B), the scan's last entry, the three face arrays, the distance and the solid (9.2 MB at 64), plus stamp, epoch, ticks and a pending flag (`matter_state.rs:49` precedent).
- **Output ring**, 3 slots, each three shared `FluidParticle` buffers grown in 4,096-record steps up to C, plus a read stamp (`fluid/particle_ring.rs` precedent; admission through `admit_candidate_bytes`).

`run` does, in this order:

1. **Consume.** For each pending slot in stamp order: live, stop at the first whose `is_complete(stamp)` is false; offline, `wait(stamp)`. A slot from an older epoch is discarded. Otherwise: epoch changed → `clear`; `set_fields`; `load`; `step(TICK)` × the slot's ticks; the slot is free. A slot is consumed once.
2. **Publish.** If the population changed, write it into an output slot whose read stamp is complete: fade and split by `WhitewaterFrame::fill`'s rule (one shared function), positions in scene space. If none is complete, keep the previous slot. The slot provided this frame takes `stamp()` as its read stamp.
3. **Capture.** If ticks > 0: take a free snapshot slot, encode GPU copies of this frame's inputs into it, record stamp, epoch and ticks. No free slot → `dropped_ticks += ticks`, reported, never silent. Ticks = 0 captures nothing.

Rules: live never calls `wait`; the CPU touches a snapshot slot only after its stamp completes; the GPU writes a slot only when it is free; no lock, no thread. Offline export waits, so an export is the same at any speed.

### 3.6 Solver feeds

- **SWASH:** `face_sample_component` × 3 on the last step's `new` faces (`swash_preset.rs:609`). Until the seam's P7a builds `node.liquid_frame`, the render harness wires them straight to the group with ticks 1, the generator input's trigger count as epoch, gravity −9.81, 8 particles per cell. When P7a lands, `liquid_frame` publishes `FACE_GRID_PORTS` and the harness wiring goes.
- **MPM:** `matter_frame` publishes `FACE_GRID_PORTS` from `matter_face_component` on the region's final grid, one copy per ring slot beside `solid_*`, only when wired, held while paused (⚠ VERIFY-AT-IMPL: the grid escapes the region as a boundary result — `R/primitives/matter_state.rs:157`). `face_valid_layers` is what the kernel support gives (⚠ VERIFY-AT-IMPL: measure; 1 expected).
- **FLIP:** no grid (seam D3); its native whitewater stays the reference.

### 3.7 Proofs

- **Per atom:** a value-level `gpu_tests` proof against CPU-computed expected output and, for every fusable atom, fused against unfused (ADDING_PRIMITIVES.md).
- **O1, field against FLIP** (`whitewater_curvature_matches_flip`): exact distance fields of spheres (radius 4h, 8h, 16h, off-grid centres) and a sine surface (amplitude 2h, wavelength 16h) on the whitewater grid. Our curvature against FLIP's `calculateCurvatureGrid` on the same field: 99th percentile |Δ(k·h)| ≤ 0.05 over nodes FLIP marks valid; sphere means within 5% of 2/R. Redistance (`whitewater_redistance_matches_distance`): the same spheres written in `particle_volume`'s capped form on the refined lattice; |φ − d| ≤ 0.1h wherever |d| ≤ 3h.
- **O2, emitter against FLIP** (`whitewater_emitter_matches_flip`): SWASH Dam Break at 64, frames 30, 60, 90 and 120. The same particles, faces, distance, curvature and solid go to FLIP's emitter (through the oracle) and to our atoms; both then take one lifecycle step. Lifetime variance 0 on both, so lifetime encodes Ie. Over 16 seeds a side: total within 5%; each type within 10% where FLIP's mean is ≥ 200; spatial histogram (8³ bins over the tank) L1 ≤ 0.15; lifetime histogram (10 bins) L1 ≤ 0.1. If the seed spread is over half a tolerance, add seeds; tolerances only tighten.
- **Lifecycle** (CPU, manifold-fluids): spray dropped in a closed tank falls and rebounds at restitution 0.2; a bubble rises; foam follows the faces; lifetimes fall by 2, 0.333 and 1 per second; loaded spawns advance on the first step (the size trap).
- **Handoff** (renderer, `gpu-proofs`): pause holds; epoch change clears; four frames without completion drop the fourth frame's ticks and count them; offline runs `wait`, live never does; C overflow thins and counts.
- **Extents** (`whitewater_extent_tests.rs`, CPU): every atom's dispatch and array lengths at 64, and the named refusals for a misplaced face grid, a fractional refinement and `face_valid_layers` < 1, before any GPU run at that size.
- **Cost:** GPU ms per whitewater node from the frame timestamps, snapshot blit ms, and `lifecycle_ms`; p50 and p95 over 300 frames at 64, beside FLIP's whitewater ms (simulation ms with whitewater on minus off, the method of FFT_WATER_SOLVER_DESIGN.md P3). Defaulted targets with triggers: GPU ≤ 2 ms p95; CPU per D11.

### 3.8 Wrong turns, forbidden by name

- A GPU lifecycle: advection, collisions or lifetimes in WGSL.
- Any edit under `flip_engine/`: `Array3d` aliasing, a `friend`, a jitter setter, `#define private public`.
- The CPU reading a graph array in place, or `wait` on the live path.
- A liquid field rebuilt from particles, or a solver publishing one.
- A whitewater atom or the lifecycle branching on which solver fed it, or importing `matter_*`, `swash_*` or `fluid_surface` items.
- `InstanceSnapshotUpload` for whitewater; `InstanceTransform` outputs.
- Silent clipping at capacity or on a full ring.
- A thread, a channel or `Arc<Mutex>` for the lifecycle.
- FLIP's upwind reinit on the capped field.
- Retuning FLIP's constants toward a look.
- A whitewater renderer or screen-space foam.

## 4. Invariants & enforcement

| # | Invariant | Enforcement |
|---|---|---|
| I1 | Whitewater reads only seam ports | negative gate: `rg -n -e matter_ -e swash_ -e fluid_surface -e "type_id ==" crates/manifold-renderer/src/node_graph/primitives/whitewater_*.rs crates/manifold-renderer/src/node_graph/primitives/*crossing*.rs` → 0 |
| I2 | Grid placement is derived | `whitewater_refuses_misplaced_face_grid`, `whitewater_refuses_fractional_refinement`, `whitewater_refuses_unextended_faces` |
| I3 | Live never waits on the GPU | `whitewater_live_holds_until_fence` |
| I4 | A snapshot is consumed once, in order, after its fence; a dropped one is counted | `whitewater_ring_overflow_counts_dropped_ticks` |
| I5 | Ticks 0 hold population and outputs, and capture nothing | `whitewater_pause_holds_population` |
| I6 | A new epoch clears before any load | `whitewater_epoch_restart_clears` |
| I7 | Emission rounds per tick | `emission_count_rounds_per_tick` (gpu_tests) |
| I8 | No vendored edit | `git diff --stat origin/feat/fft-water -- crates/manifold-fluids/native/flip_engine crates/manifold-fluids/native/PROVENANCE.md` → empty |
| I9 | A CPU extent proof precedes every new GPU size | `whitewater_extents_at_64`, `face_grid_extents_at_64` |
| I10 | Capacity thinning is reported | `whitewater_capacity_thins_and_reports` |
| I11 | Loaded spawns advance | `whitewater_lifecycle_advances_loaded_spawns` |
| I12 | The GPU emitter matches FLIP's on the same inputs | `whitewater_emitter_matches_flip` (O2) |
| I13 | Curvature and distance match FLIP's and the exact field | the O1 tests |
| I14 | Fused equals unfused | per-atom fused proofs; `scripts/gpu_proofs_gate.py` |
| I15 | No new lock or thread | `git diff -U0 origin/feat/fft-water -- crates/manifold-renderer crates/manifold-fluids/src`, added lines searched with `rg -e "Arc<Mutex" -e "Arc<RwLock" -e "thread::spawn" -e crossbeam` → 0 |
| I16 | The seam face layout holds for every producer (the seam's I16) | `liquid_face_grid_layout` |

## 5. Phasing

Order: P1 → P2 → P3 → P4 → P5 → P6, all on `feat/gpu-whitewater`. Every phase: commit and push at green; GPU runs at 64 only, sharing the GPU (never kill a process); scratch output under `/tmp`.

### P1 — Grid outputs (the seam's P10)

- **Entry state:** this design approved; `origin/feat/fft-water` merged into the branch; anchors `swash_preset.rs:609`, `matter_state.rs:157`, `R/matter.rs:397` re-read.
- **Read-back:** LIQUID_SOLVER_SEAM_DESIGN.md section 3.2 (Grid outputs) and P10 (Grid outputs); ADDING_PRIMITIVES.md; this doc's section 3.1 (Grids) and section 3.6 (Solver feeds). Restate D2, the seam's P10 forbidden list, and the entry findings.
- **Deliverables:** `R/liquid/grid.rs` with `FACE_GRID_PORTS`; `node.face_sample_component`, `node.matter_face_component`; `matter_frame` inputs and outputs for the grid; group outputs `level_set`, `level_set_bounds`, `level_set_nodes_x/y/z` in every preset that embeds the Liquid Surface group (`rg -l '"liquid_surface"' crates/manifold-renderer/assets/generator-presets`), thumbnails regenerated; `face_grid_extent_tests.rs`; `liquid_face_grid_layout` (uniform and linear-shear fields, both producers, within 1e-5 of the seam positions).
- **Gate:** `cargo nextest run -p manifold-renderer face_grid liquid_face_grid_layout`; `scripts/gpu_proofs_gate.py` green; `graph-tool validate` clean on every touched preset. Negative: I1's pattern on the two new atoms → 0.
- **Demo:** L2, the seam's P10 demo: a face-speed slice of SWASH and MPM Dam Break at the same tick, side by side, PNG.
- **Forbidden:** a consumer switching on solver; node velocities as the contract; publishing every tick; `liquid_frame` (P7a's).
- **Test scope:** focused renderer; GPU proofs.

### P2 — Liquid field on the whitewater grid

- **Entry state:** P1 on the branch; `F/particlelevelset.cpp:196` and `:728` re-read.
- **Read-back:** D2–D4; section 3.1, 3.3 (grid atoms), 3.7 O1. Restate them.
- **Deliverables:** `surface_crossings`, `nearest_crossing`, `crossing_distance`, `liquid_cells`, `lattice_curvature`, `extend_lattice` with gpu_tests and fused proofs; `whitewater_common.wgsl`; the `whitewater-oracle` feature with `manifold_fluids_oracle_curvature`; the O1 tests; the grid half of `whitewater_extent_tests.rs`.
- **Gate:** O1 green; `scripts/gpu_proofs_gate.py` green; `cargo clippy -p manifold-renderer -p manifold-fluids --features manifold-fluids/whitewater-oracle -- -D warnings`. Negative: I8's diff empty.
- **Demo:** none — L1 (fields only; P6 shows them).
- **Forbidden:** FLIP's reinit on the capped field; a particle-built field; widening an O1 tolerance.
- **Test scope:** focused renderer and manifold-fluids; GPU proofs.

### P3 — Lifecycle and handoff

- **Entry state:** P2 on the branch; `F/diffuseparticlesimulation.cpp:55`, `:1480`, `F/particlesystem.h:72`, `retire.rs:159` re-read.
- **Read-back:** D1, D6–D8, D10–D12; section 3.4, 3.5. Restate them and the size trap.
- **Deliverables:** `WhitewaterSpawn`, `WhitewaterGrid`, `WhitewaterFields`, `WhitewaterLifecycle` and their bridge glue; `node.whitewater_lifecycle` with both rings; the lifecycle and handoff proofs of section 3.7 (spawns from a test source); I2–I6, I10, I11.
- **Gate:** `cargo nextest run -p manifold-fluids whitewater`; `cargo nextest run -p manifold-renderer whitewater`; the handoff proofs under `scripts/gpu_proofs_gate.py`. Negative: I8, I15.
- **Demo:** none — L1.
- **Forbidden:** `wait` on the live path; a thread; reading graph arrays in place; any `flip_engine/` edit.
- **Test scope:** focused manifold-fluids and renderer; GPU proofs.

### P4 — Emitter potentials

- **Entry state:** P3 on the branch; `F/diffuseparticlesimulation.cpp:1557`, `:1571`, `:1718`, `:1768`, `:1989` re-read.
- **Read-back:** D5, D9, D10; section 3.3 (particle atoms). Restate them.
- **Deliverables:** `jitter_particles`, `sample_faces_at_particles` (with `liquid_faces.wgsl`), `energy_potential`, `wavecrest_potential`, `emission_count`, each with gpu_tests and fused proofs; the particle half of `whitewater_extent_tests.rs`; I7.
- **Gate:** `scripts/gpu_proofs_gate.py` green; `graph-tool fusion` shows the chain in at most two dispatches.
- **Demo:** none — L1.
- **Forbidden:** a solver-specific emission rate; dropping the 8/ppc factor; rounding over a frame instead of a tick.
- **Test scope:** focused renderer; GPU proofs.

### P5 — Spawn, the group and the FLIP oracle

- **Entry state:** P4 on the branch; `F/diffuseparticlesimulation.cpp:1912`, `:2056` re-read.
- **Read-back:** D8, D13; section 3.3 (spawn atoms), 3.4, 3.7 O2. Restate them.
- **Deliverables:** `spawn_whitewater`, `whitewater_type` with gpu_tests and fused proofs; `manifold_fluids_oracle_emit`; O2; the Whitewater group in `assets/reference-presets/WaterDamBreakMatterWhitewater.json`; `matter_whitewater_emits` (MPM Dam Break at 64, 90 frames: foam > 0 at 1.5 s, no refusal); I12.
- **Gate:** O2 green; `scripts/gpu_proofs_gate.py` green; `graph-tool validate --kind generator` and `fusion` clean on the reference preset; the preset loads, saves and reloads with its params (round trip).
- **Demo:** L2: MPM Dam Break with whitewater at 1.5 s and 3 s, PNG.
- **Forbidden:** widening an O2 tolerance; shipping the preset (moving it into `generator-presets` is Peter's call).
- **Test scope:** focused renderer and manifold-fluids; GPU proofs.

### P6 — Side by side and cost

- **Entry state:** P5 on the branch; `swash_preset.rs:437` and `:465` re-read; the FLIP render path of FFT_WATER_SOLVER_DESIGN.md P3's demo found (⚠ VERIFY-AT-IMPL: its demo command).
- **Read-back:** this doc's section 3.6 (Solver feeds) and section 3.7 (Proofs); the demo rules of DESIGN_DOC_STANDARD.md section 5 (Phase briefs). Restate them.
- **Deliverables:** the SWASH harness wired to the group and the three copies objects; `whitewater_side_by_side` writing `side_by_side.mp4` (1080p, 211 frames at 60 fps, FLIP engine with native whitewater left, SWASH with GPU whitewater right, `WaterDamBreakGpu.json`'s camera, the studio floor removed on both) and `counts.png` (foam, bubble and spray counts over time, both); the cost table in this phase's notes.
- **Gate:** the test exits 0 and writes both files; cost measured and reported against the targets; the GPU and CPU targets met or escalated per D11.
- **Demo:** L2 for Peter: `side_by_side.mp4` and `counts.png`. Command: `WHITEWATER_DEMO_DIR=/tmp/whitewater cargo test -p manifold-renderer --features gpu-proofs --lib whitewater_side_by_side -- --nocapture`.
- **Gesture:** pause mid-splash; the foam freezes with the water and moves on when play resumes.
- **Forbidden:** tuning FLIP's constants toward the look; judging the look by agent.
- **Test scope:** focused renderer; GPU proofs.

Phasing completeness: every behaviour in sections 3.1–3.7 lands in one phase above or in section 7.

## 6. Decided — do not reopen

1. GPU emitter, vendored FLIP lifecycle (D1; Peter, 2026-09-30).
2. The whitewater grid is the solid lattice's cells (D2).
3. The field is the surface group's level set, re-distanced by nearest crossing (D3, D4; seam D5).
4. Emission per tick, from the last tick, normalised to 8 particles per cell (D5).
5. Snapshot ring, fenced reads, whole-array copies; live never waits (D6).
6. Output in the surface design's P8 shape (D7).
7. Capacity thins and reports (D8).
8. Turbulence, dust, influence, speed factor, generation coin and foam preservation dropped (D9).
9. Statistical oracles against FLIP's own code through its public API (D10).
10. No thread; escalate past 3 ms (D11).
11. Time, epoch and gravity from the domain (D12).
12. One Whitewater group, sourced from a reference preset (D13).

## 7. Deferred

| Item | Revives when |
|---|---|
| Turbulence and inside emitters | Peter wants turbulence foam; needs recalibration first |
| Forces and impulses on whitewater | the seam's P8 (Forces and impulses for GPU liquids) lands |
| Obstacle influence grid | GPU liquids get obstacle roles |
| Presenting whitewater at display time | the side-by-side shows foam trailing the front |
| A lifecycle worker thread | `lifecycle_ms` p95 > 3 ms at 64, or a `MANIFOLD_RENDER_TRACE=1` frame over 20 ms |
| `liquid_frame` publishing the grid | the seam's P7a lands |
| Resolutions above 64 | the resolution campaign, one size at a time with extent proofs |
| Whitewater in the frame cache | GPU liquids get a bake |
| The content-thread trace gate | the phase that first wires whitewater into a shipped preset |

## 8. Calls only Peter makes

1. The side-by-side verdict (P6).
2. Wiring whitewater into shipped presets: the MPM presets now, SWASH after P7a. The `MANIFOLD_RENDER_TRACE=1` gate runs then.
3. Emission tuning (wavecrest rate, curvature window) if the look differs from FLIP's; FLIP's defaults until then.
