# Water field wave — plan

<!-- index: Orchestration plan for the GPU FLIP water upgrade, executed warm-tree with Haiku/Sonnet-first delegation: the mesh replaced by drawing straight from the field (clean cut), the look items, wet surfaces, the sheeting fix, the tick overlap, sparse storage then the infinite domain with the surface on world tiles, Narrow Band rebuilt on tiles, and the pressure speed group. Workflow (warm-tree, pre-chew/delegate, clean cut, no magic numbers, verification), rulings I1–I13, task routing and specs, final verification. -->

**Status:** PLAN · 2026-10-23. Rulings: `docs/GPU_FLIP_END_GAME_DESIGN.md` D2, D4, D13 to D18. Umbrella: BUG-l2h3 (GPU FLIP live instrument).

## 1. Workflow

- **Warm-tree execution.** All tasks run directly in the main checkout against the shared warm `target/` directory on strictly disjoint file sets (`CLAUDE.md`, `docs/AGENT_ROUTING.md`). No worktrees or slot reservations.
- **Pre-chew, then delegate down.** The lead pins down exact `file:line` anchors and formulas before spawning a worker, then delegates to `haiku` / `opencode-go/beam-free` (mechanical splits, deletions, includes, wiring) or `sonnet` (self-contained shaders/modules with provided formulas).
- **Clean cut & no magic numbers (D14, D15).** Delete replaced code/nodes/params/tests in the same change (`rg` across `crates scripts tools assets docs`). Every constant is either physical in real units (cited), derived (one-line comment), or a single named constant citing the paper/prototype.
- **Verification.** Run `cargo nextest run -p <touched_crate> <filter>` and `cargo clippy -p <touched_crate> --tests -- -D warnings`. GPU tests run one at a time via `scripts/gpu_queue.py`. The lead verifies `git diff` directly after each task—no planted-fault rituals or multi-stage audit swarms.

## 2. Implementation rulings (the lead's, binding on lanes)

- **I1 Absent tiles** read their defined value: `GPU_FLIP_SPARSE_BLOCKS_DESIGN.md` section 4 (defined values). Water 0, distance 3 cells (air), face absent.
- **I2 Bounds faces.** With Bounds on, each face is Wall or Open (D4). A Wall is an ordinary solid in the solid distance. An Open face removes water that crosses it. This replaces `closed_faces`, and every box-wall special case in the kernels is deleted.
- **I3 Bounds off.** Water is removed once it is lower than the scene's lowest solid point by more than one particle's drawn radius (Particle Scale × particle radius), so nothing visible is cut. The domain gets that height as one scalar.
- **I4 No saved-show migrations.** Pre-release. `migration/gpu_flip_surface.rs` and its wiring at `blob_bounds.rs:182` are deleted with the mesh chain; `migration/liquid.rs:49` loses its `retired_max_speed` wire with S3.
- **I5 Reference engine.** These go with the mesh chain (RD): `crates/manifold-fluids/src/surface.rs`, `SurfaceOptions`, `decode_surface`, `meshing_ms`, the C shims such as `manifold_fluids_world_capture_surface_frame`, and the oracle tests built on them. `mesh.rs` there is collider meshes and stays. The vendored C++ under `native/` is never edited.
- **I6 Big files.** `render_scene.rs` (2953 lines) takes only the hook lines that call a new submodule. S0 splits `gpu_flip_step.rs`, `gpu_flip_step_tests.rs` and `gpu_flip_step.wgsl`.
- **I7 The tracer.** Water casts no shadow and stays out of the tracer, as today. The field hit is a later task under BUG-651ho (GPU FLIP surface: one level set, drawn directly).
- **I8 Owners.**
  - R1b owns `presets/gpu_flip.rs`, `scene_family.rs`, `assets/liquid_surface_group.json`, `scene_schema/water.rs` and `scene_compile/water.rs`. Exception: R4 deletes the foam draw block at `gpu_flip.rs:632-655`.
  - R0 owns `render_scene.wgsl` until it merges; after that it takes one-line hook calls only.
  - RD regenerates the six preset fixture JSONs, once.
- **I9 Foam.** The foam field draws surface foam. Foam particles keep simulating as its source; their icosahedron draw is deleted. Spray and bubble draws are unchanged.
- **I10 Thickness.** `volume_optics` stays for other closed transmissive meshes (`render_scene.rs:2306`). Water stops feeding it; the march supplies water's thickness.
- **I11 Surface stays spheres.** The prototype measured stretched blobs bumpier and losing sheets (D2). R1a deletes Stretch and Centre Smoothing from `shape_particle_blobs`, with the anisotropy code they drive. This is a call made for Peter; it goes first in the report.
- **I12 Modifiers.** Water is not a modifier target today (`scene_schema/fields.rs:95`; never in `b.drawn`, `scene_compile/object.rs:170`), so nothing regresses. R1b adds a one-line refusal reason for water in the Targets menu, following the Ocean precedent (`modifier.rs:445-457`). Loop, Mirror, Copies and deformers on water are a later bead.
- **I13 Not in this wave, logged:** the 4K half-resolution search, the tracer hit, traced refraction, field versions of Shatter, Surface Peel and Ordered Recon (BUG-l2h3.30.13), and APIC.

## 3. Tasks & Routing Table

| Lane | Seat | What | Needs |
|---|---|---|---|
| S0 | haiku / free | Split the step file, tests and WGSL into submodules | — |
| R0 | haiku / free | Shader helpers into an include; plain look hook stubs | — |
| RD | haiku / free | Mesh chain deletion and fixture conversion | R1b |
| R1a | sonnet | Field build: re-distance, weights, smooth, carry-in; spheres only | — |
| R2 | sonnet / lead | The march pass, with light through thin water | R0 |
| R1b | haiku / sonnet | Water graph, card and preset owner; wires R1a and R2 | R1a, R2 |
| R3 | sonnet / haiku | Ripple detail | R2 |
| R4 | sonnet / haiku | Foam field | R2 |
| R5 | sonnet / haiku | Mist | R2 |
| R7 | sonnet / haiku | Wet surfaces | R2 |
| SA | sonnet | Sheeting volume fix (W10 re-distance depth) | S0 |
| SB | sonnet / lead | Next tick overlaps render on compute queue (W8) | — |
| S1 | sonnet | Storage helpers + step kernels | S0 |
| S2 | haiku / sonnet | Pressure, bodies, whitewater, sheeting onto helpers | S1, SA, R4 |
| S3 | sonnet / lead | Sparse pool, growable particle pool, live-count dispatch | S2 |
| S4 | sonnet / lead | Infinite domain: world-keyed table, Bounds faces, metres | S3 |
| S4b | sonnet | Surface, solids, wetness on world tiles | S4, R1b, R7 |
| SP | sonnet | Pressure group: fused smoother, warm start, visible tolerance, coarse jitter | S3 |
| S5 | sonnet | Narrow Band on tiles | S4b, SA |

**Order.** The render chain (R0, R2, R1b, RD) and the simulation chain (S0, S1, S2, S3, S4, S4b) each keep work moving; SB, SA, then R3, R4, R5, R7, SP and S5 follow their needs. The simulation chain is the long pole. The whole set finishes before the landing; nothing is stretch.

Phases:
1. S0, SA diagnosis, R1a, S1.
2. R0 and SB.
3. R2, then R1b; S2 alongside.
4. RD and S3; R3, R4, R5 and R7 run alongside.5. S4 and SP.
6. S4b and S5.
7. Final verification (section 5).

## 4. Task Specs

Every lane's brief is its entry here plus sections 1 and 2. Anchors come from the 2026-10-23 inventories against main 5b3a6d255; the lead re-verifies them on the cut.

### R0 Shader helpers

Files: `crates/manifold-nodes-scene/src/node_graph/primitives/shaders/render_scene.wgsl`, a new include beside it, and the include wiring in `pipelines.rs`.
1. The lighting, shadow, IBL and fog helpers move into one include that `render_scene.wgsl` and a full-screen pass both use, without the mesh vertex shader.
2. Look hooks as plain WGSL function stubs, one include file per look item, each called from the water shading. One hook is also called from `fs_pbr` for wetness. One bind group, owned by the march, carries the look items' inputs. No text-substitution variants: every look is always on (D17).
3. Pixel-identical renders before and after, on a raster scene and a ray-traced scene. First render the base twice to prove the comparison is deterministic.

### R1a Field build

Files:
- `crates/manifold-water-liquid/src/primitives/smooth_lattice.rs` (`:35` already runs the [1 2 1] filter per axis), `redistance_lattice.rs`, `clamp_liquid_to_solids.rs` (`:50`), `shape_particle_blobs.rs`;
- new weight primitives beside them, with their shaders, tests and `mod.rs` entries.
Keep `marching_cubes_common.wgsl`: `redistance_lattice.rs:63` uses it.
Reference: the prototype, `/private/tmp/claude-501/-Users-peterkiemann-MANIFOLD---Rust/d59e90fa-adea-4085-bebf-0d8ba5fa7877/scratchpad/levelset_proto/src/weighted.rs`, and design doc D2 (the field build).
1. Re-distance depth in cells, derived: the smoothing's reach plus the leash, rounded up (4 cells at the prototype's settings). The band moves from metres to cells.
2. Neighbour weights, in cells:
   - each particle counts its neighbours within the blob search radius (1.5 cells; FLIP Fluids' 1.5 radii, a named constant citing the prototype);
   - counts are carried to grid nodes from particles within the blob support;
   - the weight fades from 0 to 1 between two fractions of the expected bulk count, derived from particles per cell times the search volume. That is 0.18 and 0.36 of about 110 in the prototype;
   - the count and carry radii are in cells (1.5 and 2 cells, named constants citing the prototype), so Particle Scale never shifts the weights;
   - the weights are a pure function of the frame's blended particles, with no cross-frame display state;
   - the weights are blurred once in space. In time any blend is per sim step only, never per display frame.
3. Weighted smoothing as an extension of `smooth_lattice`: weight and leash inputs, with a strength in cells. The prototype's 3 passes are a named constant citing it. The leash is half a cell, derived as the finest change the grid can represent.
4. Carry-in as an extension of `clamp_liquid_to_solids`: the field is extrapolated into drawn solids by the smoothing's reach (carry radius 2 cells), and clamped elsewhere with no margin. `clamp_liquid_to_solids` runs twice (carry into all solids early, keep drawn solids late), and its drawn-solids input is wired by R1b.
5. A display-time velocity lattice for exact motion vectors.
6. Stretch, Centre Smoothing and their anisotropy code are deleted (I11); `FluidBlob` shrinks to centre plus radius.
7. Tests on small in-test particle sets: weights, smoothing and carry-in match a CPU reference written in the test to f32 rounding. The comparison with the prototype's 64 capture goes in the report, not a test.

### R2 The march pass, with light through thin water

Files:
- new `render_scene/water_march.rs`, `shaders/water_march.wgsl`, `shaders/water_shade.wgsl`;
- hook lines in `render_scene.rs` (frame order `:2619-2917`), `pipelines.rs`, `resources.rs`, `construction.rs`;
- `crates/manifold-node-engine/src/scene/scene_object.rs` (an optional `field` input) and the `node.scene_object` node;
- `render_scene/frame_setup.rs`, `pass2_draw.rs`, `item_pick.rs`, `selection_mask.rs`, `prepasses.rs` (field kind only).
Empty-space skipping reuses `lattice_bricks` (`:252`).
1. A field draw kind beside `ObjectDraw` (`render_scene.rs:1771`): clamped level set, origin, spacing, dimensions, bricks, velocity lattice, slot, material, transforms.
2. The march:
   - a step factor below 1 near the surface (the prototype's 0.6 within 2 cells is a named constant citing it);
   - bisection on the sign change down to a named precision in cells;
   - cubic normals at the hit;
   - stopped by the opaque depth snapshot.
3. Outputs: depth into the shared depth, normal, thickness, velocity, slot, and entry and exit distance. A depth-and-slot mode serves the pick pass and the selection mask, so a click on the water selects it.
4. Shading in Pass B through the R0 include, with the look stubs called. Water binds its Material like any object.
5. Light through thin water: Beer–Lambert absorption per metre from the material along the marched thickness. The test checks transmittance at 0.1 m and at 5 m against 0.34, 0.057 and 0.009 per metre.
6. Motion vectors from the velocity lattice.
7. A GPU proof that the march hits a known sphere field at the right depth and normal. Stills of the Dam Break at 64 and 128, frames 45 and 240, against the prototype's montages: same shape, no seams, flush at walls.

### R1b Water graph, card and preset owner

Files: per I8. Anchors:
- `gpu_flip.rs`: `:1029-1049` `surface()`, `:1166-1198` `display_head()`, `:1187-1194` the teal material, `:885-930` the mesher controls, `:577-583` and `:1206-1211` the Physics View switches;
- `scene_schema/water.rs`: `:162-203` rows, `:109` Surface Detail;
- `modifier.rs:445-457`, the refusal precedent.
1. FLIP and MPM water build the field (R1a) and draw it through the march (R2). The group outputs the clamped level set and the velocity lattice. Physics View switches the field.
2. Card (D17):
   - Smoothness is automatic, with one override slider. Its default is the prototype's settings; name the mapping.
   - Ripple detail, foam, mist, thin water and wetness are automatic, with one override slider each.
   - Delete the Stretch, Centre Smoothing, Fill Pits and mesh Smoothing rows, and the `offset_lattice` Fill Pits node if nothing else uses it.
   - Surface Detail stays: the field's resolution over the sim's.
3. The default water material is physical (D2), and the presets use it (D17).
4. Water's one-line refusal reason in the Modifier Targets menu (I12).
5. One-line entries: sheeting on by default once SA merges; tank presets with Bounds on and every face a Wall once S4 merges (D18).
6. Stills of the Dam Break and an MPM preset through `fluid_capture`.

### RD Mesh chain deletion

Files: whatever its `rg` finds. The 2026-10-23 inventory lists about 84 files, including:
- the 6 nodes in `crates/manifold-water-surface/src/primitives/` with their extents, 11 shaders, `surface_mesh_parity.rs` and `testkit/surface.rs`;
- the live-extent plumbing: `manifold-node-engine/src/scene/live_extent.rs`, `bindings.rs:430-561`, `exec/execution.rs:180,2320-2503`, `exec/backend.rs:266-274`, `switch_mesh.rs:158-181`, `scene_object.rs:437-504`, and the extent half of `render_scene/live_draw_args.rs`;
- `shader_compiler.rs`, `metal_backend.rs:180-967`, `testkit/array_harness.rs`, `testkit/liquid_extents.rs`, the `mesh_sink` (`gpu_flip.rs:534`, a one-line exception to I8), the catalog tests, `freeze/codegen.rs`, `water/coupling.rs:219`, `liquid_indexed.rs`, `uniform_layout_proof.rs`, `wgsl_validation.rs`;
- the scripts: `test_gpu_scope.py`, `gate_policy.py`, `gpu_test_times.json`, `fixtures/gate-p1.json`;
- `fluid_capture.rs`: `--gpu-surface`, `--dump-mesh`, `dump_surface_meshes`, and the vertex count and meshing columns;
- I4 and I5 items;
- `manifold-io/tests/fixtures/water_layer_graph_v1160.json` and its users;
- the six preset fixture JSONs and their thumbnails;
- the mesh sections of `GPU_FLUID_SURFACE_DESIGN.md`.
1. `rg -n 'volume_surface_mesh|smooth_surface_mesh|relax_surface_mesh|count_surface_triangles|count_surface_edges|surface_mesh_normals|live_extent|liquid_mesh|capture_surface_frame|SurfaceOptions' crates scripts tools assets docs .claude` returns only design-doc text that records the removal.
2. `LiveExtent` stays as a general type (the fragment cut, `frame_setup.rs:108-111`); RD deletes only water's published-extent path, and its rg check lists the remaining non-water uses by file instead of expecting none.
3. Fixtures and thumbnails are regenerated once from the builders.
4. Every test and GPU proof that wired the chain is converted to the field or deleted, and listed.
5. `cargo check --workspace --tests` through `with-build-lock.sh` is clean.

### R3 Ripple detail

Files: its look include and Rust module. Bead: BUG-l2h3.30.9 (water detail normals). Per D13:
- projected on three axes;
- moved along the sim's velocity, with two crossfading copies;
- strength from the flow, masked by foam;
- sourced from the FFT ocean's spectrum;
- sizes in metres.
1. Strength is automatic from flow speed and scale, with one override slider.
2. A test that the normal change across the crossfade swap is no larger than the frame-to-frame change elsewhere.
3. Stills at a close and a far camera.

### R4 Foam field

Files: its look include and module; a foam splat from the whitewater foam particles into a surface field (new files in the whitewater crates); and the foam draw block at `gpu_flip.rs:632-655` (I8 exception). Bead: BUG-l2h3.30.7 (foam field on the surface).
1. Foam is carried by the flow, gathers where water converges, and fades exponentially with a physical lifetime. A test checks it against the closed form.
2. The foam icosahedron draw is deleted; the foam particle sim stays as the source.
3. Stills of the Dam Break settling and of a breaking wave.

### R5 Mist

Files: its look include and module; a spray-to-fog density on the water tiles. Bead: mist, under BUG-l2h3.30 (GPU FLIP water rendering).
1. Thin fog density from spray, with droplet size and density per metre, accumulated by the march under Beer–Lambert. A test checks it against the closed form.
2. Stills of a splash with mist.

### R7 Wet surfaces

Files: a wetness field on the water's lattice (a new module), its writer from water touching solids, and its `fs_pbr` hook from R0. Bead: BUG-l2h3.30.10 (wet surfaces). Materials are shared, so wetness is sampled by world position, never stored in a material node.
1. Everything the water touches darkens and dries at a physical rate: a thin film's evaporation, a named constant with its source. Automatic, with one override slider.
2. A test checks drying against the exponential closed form.
3. Stills of the tank floor as the Dam Break wave passes, and 10 s later.

### S0 Split the step files

Files: `crates/manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs`, `gpu_flip_step_tests.rs` and `shaders/gpu_flip_step.wgsl`, split into submodules and includes: lattice, tiles, extend, pockets, narrow band, the sheeting block, emit. Mechanical only: no change to logic, names or behaviour.
1. Clippy and the crate's nextest pass, with an identical test count.
2. Every new file is under 1500 lines.

### SA Sheeting volume fix

Bead: BUG-a8eh6 (Sheet Fill Rate 0.5 grows the Dam Break water by 61% in 4 seconds). Files: `gpu_flip_sheeting.rs`, `.wgsl` and their tests; the sheeting submodule from S0; `liquid_stats.rs`.
Sheeting goes beyond FLIP Fluids on depth: native FLIP Fluids grows the Dam Break by 62% in 4 s with sheeting at 0.5 too, because its level set is a union of spheres re-distanced short of its own 3 dx target, so deep calm water passes the thin-water test; ours re-distances to a depth derived from the sheet test's threshold, so only thin water seeds.
Suspects, ranked:
- The border clear counts from the grid edge, not the wall, so a thin floor pool re-seeds every substep (`gpu_flip_sheeting.wgsl:261`, `:171-174`, `:305`; `lattice.rs:76-78`). Report the `lattice.rs` change to the lead; it is in S1's crate.
- Volume Projection is off in the preset (`gpu_flip.rs:151`).
- Seeding runs per GPU substep; the engine seeds per step (`fluidsimulation.cpp:11053-11057`).
1. Root cause named with evidence, and the fix ported from the reference.
2. A volume proof: a resting pool seeds nothing; Dam Break drift is reported, not gated.
3. Sheet births dropped for lack of room reach the card through the stats word.

### SB Next tick overlaps this frame

Files: `crates/manifold-node-engine/src/exec/execution.rs` (6027 lines; the lead writes a line map into the brief), `substeps.rs`, `liquid_state.rs`, `liquid_frame.rs`.
1. The water simulation runs on its own compute queue through `manifold-gpu`, so the GPU computes the next tick while this frame draws. The render reads retired history slots, never the live state, so the only ordering point is the publication copy, fenced by a shared event. The API is shaped so Vulkan's queue and timeline semaphore map onto it (`docs/VULKAN_BACKEND_DESIGN.md`). The live app never waits on the GPU per frame; sim and render share one command buffer and run in order.
2. Measured by live tick interval on the Dam Break at 64, interleaved A/B under the GPU lock (memory: frame-timing method), never by `fluid_capture`'s `submit_wait_ms`, which wraps sim and render in one wait. `submit_wait_ms` measured the capture tool's own wait, so SB is judged by live tick interval and commits nothing if it gains nothing.
3. The existing timing and frame-exact tests pass; export stays exact. Reading the past never changes it.

### S1 Storage helpers and step kernels

Files: `TileTable` (`gpu_flip_step` submodules, `:117-217`), grown in place into the storage helpers in `crates/manifold-water-liquid/` (Rust plus one WGSL include); the S0 step submodules and the step WGSL. Design: `GPU_FLIP_SPARSE_BLOCKS_DESIGN.md` sections 3 and 4.
1. The helpers cover only what callers use today: `ts_list_cell`, `ts_neighbour`, `ts_face`, `ts_cell_at`, `ts_origin`. Their first version indexes today's dense lattice. Pool reservation and the exhausted word come with S3.
2. Every step kernel goes through them.
3. Bitwise identical on the Dam Break over 120 steps (proof).

### S2 Pressure, bodies, whitewater and sheeting onto the helpers

Files: `gpu_flip_pressure.rs` and `.wgsl`, `gpu_flip_lentine.wgsl`, `gpu_flip_bodies.rs` and `.wgsl`, `liquid_faces.wgsl`, the `whitewater_*` files, the sheeting files.
1. No indexing helper exists outside the storage helpers. These return nothing outside them: `rg -n 'p\.x \+ n\.x \*|fn flatten|fn flat\b|fn nb_index|fn ww_cell_index|fn lf_face_index|fn c_cell_index|fn index\('`.
2. Whitewater reads one distance source; the `unwrap_or(level_set)` fallback at `whitewater_step.rs:708` is deleted.
3. Bitwise identical on the Dam Break over 120 steps against the S1 tip.

### S3 Sparse pool

Bead: BUG-7wna (sparse tile storage). Files: whatever its `rg` finds across the storage helpers, the step, pressure, bodies, whitewater, sheeting, the sorter (`sort_particles_into_cells.rs` and `.wgsl`, shared with MPM through `matter_common.rs:16`), `frame_history.rs`, `liquid_frame.rs`, `liquid_state.rs`, `liquid_stats.rs`, `gpu_flip_domain.rs`, and `migration/liquid.rs`.
1. Lattice fields live in a tile pool allocated and freed on the GPU with no readback. The sorter keys on (slot, local cell). Extend runs on the frontier only.
2. The particle pool grows with the water, so sheeting works without an Inflow; sheet births dropped for lack of particle room reach the card through the stats word.
3. Every particle pass dispatches from the live count through a GPU-written indirect argument. No CPU readback sizes any pass, including `history_slots` from `retired_max_speed`.
4. A full pool pauses sources and the card says why; never a crash (D4).
5. Deleted:
   - the dense `LatticeBuffers` and the dense scratch terms;
   - `tiles_fill` and the canonical rewrite;
   - the `all_tiles` dense oracle and the box-indexed bases;
   - the dense `faces` output and the frame-history face copies;
   - dense emit sites and dense sheeting scratch;
   - Narrow Band entirely, with the frame history's `FIELD_INTERIOR`, liquid_frame's interior and `GPU_FLIP_NARROW_BAND_DESIGN.md`.
6. Memory reported against D4's budget at 64 and 128. Stills of the Dam Break and an MPM preset look unchanged.

### S4 Infinite domain

Bead: BUG-zdiuw (GPU FLIP infinite domain). Files: whatever its `rg` finds across the storage helpers, `gpu_flip_domain.rs`, `fluid_domain.rs` (shared with MPM), `lattice.rs`, `matter_domain.rs`, the step submodules, `liquid_solid_distance.rs` and `fluid_role_source.rs`.
1. The page table is keyed by world tile, and positions are tile-relative.
2. Bounds per I2, off by default. Bounds off removes water per I3.
3. Moving Domain Position or Bounds live never restarts the water.
4. Resolution becomes a cell size in metres (D14), for FLIP and MPM.
5. Deleted: the wall special cases, `closed_faces`, `wall_inset`, `box_offset`, `MAX_SIDE`, `lattice_refusal`, `DOMAIN_EXTENT_METRES`, the boundary clamp and `open_band`.
6. Tanks behave as today: the Dam Break with Bounds on and every face a Wall keeps its volume within the existing volume test's tolerance, and its wave front within one cell of the pre-S4 run at 1 s.

### S4b Surface, solids and wetness on world tiles

Files: the surface group's lattice box (`liquid_lattice_box` in `assets/liquid_surface_group.json`, an R1b file now owned by this lane), `liquid_solid_distance.rs` (`:44`), `frame_history.rs` (`FIELD_SOLID`), the R1a field primitives, R7's wetness writer, and R2's march read path.
1. The surface field, solid distance and wetness live on the same world tiles as the water, with no dense box.
2. The march reads them through `ts_cell_at` and skips absent tiles.
3. Stills: water poured from an inflow over a ledge falls past where the old box ended and spreads on a floor; the memory report grows with the water; walls stay flush and wet.

### SP Pressure group

In order:
1. The fused smoother.
2. Warm start from last tick's pressure, persisting in the pool.
3. A solve tolerance tied to what is visible: a velocity error in m/s derived from the cell size and frame time (one cell per frame is the visible limit); name the derivation.
4. The coarse level jitter, BUG-8koo0 (Solve Level jitters above 0).
Each step keeps the existing solver proofs green.

### S5 Narrow Band on tiles

Ferstl et al. 2016, rebuilt on the storage helpers, working with sheeting, the field build and the speed changes. Particles live only in a band near the surface; deep water comes from the grid. The band width is derived from the CFL band the step already computes (`band_layers`).
1. A deep-pool proof: the particle count drops, and the surface and volume match the full-particle run within the volume test's tolerance.

## 5. Final Verification

- Regenerate `node-catalog` and `docs-index` (`scripts/dev.py node-catalog`, `scripts/dev.py docs-index`).
- Clean-cut `rg` sweep: every deleted name returns zero across `crates scripts tools assets docs` (design-doc removal records excepted), with no dead code and no unused params or rows.
- Quiet A/B timing and memory: `fluid_capture` at 64 and 128 on the Dam Break, main vs wave, alternating, three runs each, reporting `gpu_ms`, `frame_ms` and memory. Run with no lanes active.