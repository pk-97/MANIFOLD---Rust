# Water field wave — plan

<!-- index: Orchestration plan for the GPU FLIP water upgrade, run straight through until done: the mesh replaced by drawing straight from the field (clean cut), the look items, wet surfaces, the sheeting fix, the tick overlap, sparse storage then the infinite domain with the surface on world tiles, Narrow Band rebuilt on tiles, and the pressure speed group. Lane rules (clean cut, no magic numbers, contradictions stop), lanes with model, effort, files and done checklists, merge and review stages, one landing. -->

**Status:** PLAN · 2026-10-23 · reviewed once (Opus), revised. Rulings: `docs/GPU_FLIP_END_GAME_DESIGN.md` D2, D4, D13 to D18. Umbrella: BUG-l2h3 (GPU FLIP live instrument).
**Branch:** `wave/water-field`, cut from `origin/main` after the scene refinement campaign (`wave/refine-p1`) lands; the lead re-checks every anchor below against that cut before briefing. The wave runs straight through until the whole set is done, across the night and into the next day. One landing of the finished set, no interim landings. When the render cutover has merged on the wave branch, Peter gets a launch command to see it before the landing.

## 1. Lane rules (every lane reads this section first)

1. **Slot and branch.** `scripts/agent-worktree.py acquire <label> lane/water-<name>`, then `git -C "$WT" switch -C lane/water-<name> origin/wave/water-field`, and verify the tip against the brief. Release the slot when you report.
2. **One commit, then stop and report.** Pathspec commits only. No push, gate, landing or merge.
3. **Clean cut (D15).** Whatever your lane replaces is deleted in your commit: code, nodes, params, shaders, card rows, tests, fixtures, proofs, scripts, catalog entries and doc lines. Find them with `rg` over `crates scripts tools assets docs .claude` (skip `docs/evidence`) and with the type's own registry, never from memory. No fallback, no switch between old and new, no `unwrap_or` onto an old path, no dead code, no "kept for later". The report lists what you deleted beside what you added.
4. **No magic numbers.** Every constant in new or changed code is one of three things:
   - physics in real units, with its source (D14);
   - derived from another quantity, with the derivation in a one-line comment;
   - one named constant in one place, citing the paper or prototype it comes from.

   No count caps: buffers grow, and a limit exists only where physics or machine memory forces it, with the reason on the control. Look values are automatic, and the card's override slider reaches them (D17).
5. **Contradictions stop the lane.** If this plan, the design doc, the code or another lane's merged work disagree, stop and report both sides with file:line. Never pick one silently.
6. **Files.** Touch only your lane's listed files. RD and S3 also touch whatever their own `rg` finds. Shared files (I8) take one-line entries from any lane. Anything bigger in another lane's file: stop and report.
7. **Reuse first.** Extend the node or helper the brief names; never build a second one beside it. If none fits, stop and report.
8. **Checks before the commit.**
   - `cargo clippy -p <touched crates> --tests -- -D warnings`.
   - Scoped `cargo nextest run -p <touched crate> <filter>` once to the end; fix every failure, then rerun only those failures by name.
   - Regenerate what you made stale: `scripts/dev.py node-catalog`, `scripts/dev.py docs-index`.
   - No `--cheap` landing gate in lanes: the full gate runs once on the finished wave. Workspace-wide checks go through `.claude/scripts/with-build-lock.sh`.
9. **GPU.** Only through `scripts/gpu_queue.py`, one run at a time. GPU proofs use `cargo test`, never nextest. Never open a visible window.
10. **New tests prove real behaviour.** A new test counts only after it has been shown failing on a deliberately broken version; say how in the report. Where a closed form exists (exponential fade, Beer–Lambert), test against it. Committed tests build their own small inputs and never read scratchpad files.
11. **Look.** Opus lanes render headless stills of their own work with `fluid_capture` from their slot's build, through the GPU queue, and look at them. The headless-PNG skill is yours to run. No per-lane speed timings: the machine is busy, and one quiet timing batch runs at the end (section 5).
12. **Where settings live.** Preset recipes override node params. Change a preset's own settings in `crates/manifold-nodes-water/src/presets/gpu_flip.rs` (recipes and card defaults), never only the node.
13. **Port, don't invent.** Where a reference exists (the FLIP Fluids engine in `crates/manifold-fluids/native/flip_engine/`, a named paper), port it and name every deviation with its reason. Where the plan and the doc are silent, behave like Houdini or Blender and name the call in the report.
14. **Shell.** No `cd`, no inline python, no `git stash`, no `git checkout` or `restore` of files you did not edit. Builds and tests run in the foreground; anything over 10 minutes starts in the background and is followed with `scripts/watch_land.py` in the foreground. Never end your turn while your own work runs.
15. **Report**, under 30 lines:
    - the checklist, each item marked done, not done or changed (with the reason);
    - what was deleted and what was added;
    - every new constant and its kind (physics, derived or named);
    - tests and proofs run, with results;
    - images looked at;
    - anything unverified;
    - the commit sha.

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
- **I14 Stop rule.** A merged result that breaks the instrument (water explodes, the frame goes black, the app crashes) stops all merges until it is fixed.
- **Mid-wave rulings** go to section 7 of this plan (append-only, the lead writes, on main so every lane reads the current list at `/Users/peterkiemann/MANIFOLD - Rust/docs/WATER_FIELD_WAVE_PLAN.md`), and into the design doc as a numbered D-entry before the landing.

## 3. Lanes

Opus lanes run at high effort unless marked medium. S0 is mechanical but splits kernel files, so it stays on Opus (AGENT_ROUTING: no GPU or kernel work on Sonnet).

| Lane | Model | What | Needs |
|---|---|---|---|
| R0 | Opus medium | Shader helpers into an include; plain look hook stubs | — |
| R1a | Opus | Field build: extend re-distance, smoothing, carry-in; weights; velocity lattice; spheres only | — |
| R2 | Opus | The march pass, with light through thin water | R0 |
| R1b | Opus | Water graph, card and preset owner; wires R1a and R2 for FLIP and MPM | R1a, R2 |
| RD | Opus | Mesh chain deletion and fixture conversion | R1b |
| R3 | Opus | Ripple detail | R2 |
| R4 | Opus | Foam field | R2 |
| R5 | Opus medium | Mist | R2 |
| R7 | Opus | Wet surfaces | R2 |
| S0 | Opus medium | Split the step file, its tests and its WGSL into submodules | — |
| SA | Opus | Sheeting volume fix (diagnosis from round 1, edits after S0) | S0 |
| SB | Opus | Next tick overlaps this frame's render | — |
| S1 | Opus | `TileTable` grown into the storage helpers; step kernels onto them | S0 |
| S2 | Opus | Pressure, bodies, whitewater and sheeting onto the helpers | S1, SA, R4 |
| S3 | Opus | Sparse pool, growable particle pool, live-count dispatch; dense storage and Narrow Band deleted | S2 |
| S4 | Opus | Infinite domain: world-keyed table, Bounds faces, removal, live Domain Position, cell size in metres | S3 |
| S4b | Opus | Surface field, solid distance and wetness on world tiles; the march reads tiles | S4, R1b, R7 |
| SP | Opus | Pressure group: fused smoother, warm start, visible tolerance, coarse level jitter | S3 |
| S5 | Opus | Narrow Band rebuilt on tiles | S4b, SA |

**Order and slots.** The number of lanes holding a built slot is set by disk, re-checked before each launch: free space minus the 30 GB reserve, divided by 60 GB per built slot. That is three until the refinement campaign lands and frees its slots, then about five. Read-only work (prep, reviews, audits) needs no slot and runs alongside. The next lane starts as soon as one reports, from a brief written while the others ran. Look items split along their seam: the simulation half (foam splat, spray-to-mist density, wetness writer) starts as soon as a slot is free, proven on CPU references; the shading half joins once R2 merges. The render chain (R0, R2, R1b, RD) and the simulation chain (S0, S1, S2, S3, S4, S4b) each keep a slot busy; the third slot serves SB, SA, then R3, R4, R5, R7, SP and S5 as their needs merge. The simulation chain is the long pole.

The whole set must finish before the landing. Nothing is stretch, because the wave runs until it is done.

**Phases.**
1. **Clear of the refinement wave:** S0, SA diagnosis, R1a, S1.
2. **After refinement lands:** main merged into the wave, then R0 and SB.
3. **Drawing and storage:** R2, then R1b; S2 alongside. Peter gets a launch command once R1b merges.
4. **Cutover:** RD and S3; R3, R4, R5 and R7 fill free slots.
5. **Open water:** S4 and SP; the render, look and storage reviews.
6. **Last pieces:** S4b and S5; the domain and app reviews; fix lanes.
7. **Audits:** the final audits in section 5.
8. **Simplify and clean up** (section 5), then the quiet timing batch, the full gate and the one landing.

## 4. Lane briefs and done checklists

Every lane's brief is its entry here plus sections 1 and 2. Anchors come from the 2026-10-23 inventories against main 5b3a6d255; the lead re-verifies them on the cut.

### R0 Shader helpers (Opus, medium)
Files: `crates/manifold-nodes-scene/src/node_graph/primitives/shaders/render_scene.wgsl`, a new include beside it, and the include wiring in `pipelines.rs`.
1. The lighting, shadow, IBL and fog helpers move into one include that `render_scene.wgsl` and a full-screen pass both use, without the mesh vertex shader.
2. Look hooks as plain WGSL function stubs, one include file per look item, each called from the water shading. One hook is also called from `fs_pbr` for wetness. One bind group, owned by the march, carries the look items' inputs. No text-substitution variants: every look is always on (D17).
3. Pixel-identical renders before and after, on a raster scene and a ray-traced scene. First render the base twice to prove the comparison is deterministic.

### R1a Field build (Opus)
Files:
- `crates/manifold-water-liquid/src/primitives/smooth_lattice.rs` (`:35` already runs the [1 2 1] filter per axis), `redistance_lattice.rs`, `clamp_liquid_to_solids.rs` (`:50`), `shape_particle_blobs.rs`;
- new weight primitives beside them, with their shaders, tests and `mod.rs` entries.
Keep `marching_cubes_common.wgsl`: `redistance_lattice.rs:63` uses it.
Reference: the prototype, `/private/tmp/claude-501/-Users-peterkiemann-MANIFOLD---Rust/d59e90fa-adea-4085-bebf-0d8ba5fa7877/scratchpad/levelset_proto/src/weighted.rs`, and design doc D2 (the field build).
1. Re-distance depth in cells, derived: the smoothing's reach plus the leash, rounded up (4 cells at the prototype's settings). The band moves from metres to cells.
2. Neighbour weights, in cells:
   - each particle counts its neighbours within the blob search radius (`shape_particle_blobs` cites FLIP Fluids' 1.5 radii);
   - counts are carried to grid nodes from particles within the blob support;
   - the weight fades from 0 to 1 between two fractions of the expected bulk count, derived from particles per cell times the search volume. That is 0.18 and 0.36 of about 110 in the prototype.
   - The weights are blurred once in space. In time they are blended with a time constant of one sim step, never per display frame.
3. Weighted smoothing as an extension of `smooth_lattice`: weight and leash inputs, with a strength in cells. The prototype's 3 passes are a named constant citing it. The leash is half a cell, derived as the finest change the grid can represent.
4. Carry-in as an extension of `clamp_liquid_to_solids`: the field is extrapolated into drawn solids by the smoothing's reach, and clamped elsewhere with no margin.
5. A display-time velocity lattice for exact motion vectors.
6. Stretch, Centre Smoothing and their anisotropy code are deleted (I11).
7. Tests on small in-test particle sets: weights, smoothing and carry-in match a CPU reference written in the test to f32 rounding. Each test is shown failing on a planted fault. The comparison with the prototype's 64 capture goes in the report, not a test.

### R2 The march pass, with light through thin water (Opus)
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

### R1b Water graph, card and preset owner (Opus)
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

### RD Mesh chain deletion (Opus)
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
2. Fixtures and thumbnails are regenerated once from the builders.
3. Every test and GPU proof that wired the chain is converted to the field or deleted, and listed.
4. `cargo check --workspace --tests` through `with-build-lock.sh` is clean.

### R3 Ripple detail (Opus)
Files: its look include and Rust module. Bead: BUG-l2h3.30.9 (water detail normals). Per D13:
- projected on three axes;
- moved along the sim's velocity, with two crossfading copies;
- strength from the flow, masked by foam;
- sourced from the FFT ocean's spectrum;
- sizes in metres.
1. Strength is automatic from flow speed and scale, with one override slider.
2. A test that the normal change across the crossfade swap is no larger than the frame-to-frame change elsewhere.
3. Stills at a close and a far camera.

### R4 Foam field (Opus)
Files: its look include and module; a foam splat from the whitewater foam particles into a surface field (new files in the whitewater crates); and the foam draw block at `gpu_flip.rs:632-655` (I8 exception). Bead: BUG-l2h3.30.7 (foam field on the surface).
1. Foam is carried by the flow, gathers where water converges, and fades exponentially with a physical lifetime. A test checks it against the closed form.
2. The foam icosahedron draw is deleted; the foam particle sim stays as the source.
3. Stills of the Dam Break settling and of a breaking wave.

### R5 Mist (Opus, medium)
Files: its look include and module; a spray-to-fog density on the water tiles. Bead: mist, under BUG-l2h3.30 (GPU FLIP water rendering).
1. Thin fog density from spray, with droplet size and density per metre, accumulated by the march under Beer–Lambert. A test checks it against the closed form.
2. Stills of a splash with mist.

### R7 Wet surfaces (Opus)
Files: a wetness field on the water's lattice (a new module), its writer from water touching solids, and its `fs_pbr` hook from R0. Bead: BUG-l2h3.30.10 (wet surfaces). Materials are shared, so wetness is sampled by world position, never stored in a material node.
1. Everything the water touches darkens and dries at a physical rate: a thin film's evaporation, a named constant with its source. Automatic, with one override slider.
2. A test checks drying against the exponential closed form.
3. Stills of the tank floor as the Dam Break wave passes, and 10 s later.

### S0 Split the step files (Opus, medium)
Files: `crates/manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs`, `gpu_flip_step_tests.rs` and `shaders/gpu_flip_step.wgsl`, split into submodules and includes: lattice, tiles, extend, pockets, narrow band, the sheeting block, emit. Mechanical only: no change to logic, names or behaviour.
1. Clippy and the crate's nextest pass, with an identical test count.
2. Every new file is under 1500 lines.

### SA Sheeting volume fix (Opus)
Bead: BUG-a8eh6 (Sheet Fill Rate 0.5 grows the Dam Break water by 61% in 4 seconds). Diagnosis starts in round 1, read and probe only; edits start once S0 merges.
Files: `gpu_flip_sheeting.rs`, `.wgsl` and their tests; the sheeting submodule from S0; `liquid_stats.rs`.
Suspects, ranked:
- The border clear counts from the grid edge, not the wall, so a thin floor pool re-seeds every substep (`gpu_flip_sheeting.wgsl:261`, `:171-174`, `:305`; `lattice.rs:76-78`). Report the `lattice.rs` change to the lead; it is in S1's crate.
- Volume Projection is off in the preset (`gpu_flip.rs:151`).
- Seeding runs per GPU substep; the engine seeds per step (`fluidsimulation.cpp:11053-11057`).
1. Root cause named with evidence, and the fix ported from the reference.
2. A volume proof: the Dam Break with Sheet Fill Rate 0.5 drifts in particle volume over 4 s no more than the same run with sheeting off. It is shown failing before the fix.
3. Sheet births dropped for lack of room reach the card through the stats word.

### SB Next tick overlaps this frame (Opus)
Files: `crates/manifold-node-engine/src/exec/execution.rs` (6027 lines; the lead writes a line map into the brief), `substeps.rs`, `liquid_state.rs`, `liquid_frame.rs`.
1. The water simulation runs on its own compute queue through `manifold-gpu`, so the GPU computes the next tick while this frame draws. The render reads retired history slots, never the live state, so the only ordering point is the publication copy, fenced by a shared event. The API is shaped so Vulkan's queue and timeline semaphore map onto it (`docs/VULKAN_BACKEND_DESIGN.md`).
2. Measured by live tick interval on the Dam Break at 64, interleaved A/B under the GPU lock (memory: frame-timing method), never by `fluid_capture`'s `submit_wait_ms`, which wraps sim and render in one wait. If the A/B shows no gain, report the numbers and commit nothing.
3. The existing timing and frame-exact tests pass; export stays exact. Reading the past never changes it.

### S1 Storage helpers and step kernels (Opus)
Files: `TileTable` (`gpu_flip_step` submodules, `:117-217`), grown in place into the storage helpers in `crates/manifold-water-liquid/` (Rust plus one WGSL include); the S0 step submodules and the step WGSL. Design: `GPU_FLIP_SPARSE_BLOCKS_DESIGN.md` sections 3 and 4.
1. The helpers cover only what callers use today: `ts_list_cell`, `ts_neighbour`, `ts_face`, `ts_cell_at`, `ts_origin`. Their first version indexes today's dense lattice. Pool reservation and the exhausted word come with S3.
2. Every step kernel goes through them.
3. Bitwise identical on the Dam Break over 120 steps (proof).

### S2 Pressure, bodies, whitewater and sheeting onto the helpers (Opus)
Files: `gpu_flip_pressure.rs` and `.wgsl`, `gpu_flip_lentine.wgsl`, `gpu_flip_bodies.rs` and `.wgsl`, `liquid_faces.wgsl`, the `whitewater_*` files, the sheeting files.
1. No indexing helper exists outside the storage helpers. These return nothing outside them: `rg -n 'p\.x \+ n\.x \*|fn flatten|fn flat\b|fn nb_index|fn ww_cell_index|fn lf_face_index|fn c_cell_index|fn index\('`.
2. Whitewater reads one distance source; the `unwrap_or(level_set)` fallback at `whitewater_step.rs:708` is deleted.
3. Bitwise identical on the Dam Break over 120 steps against the S1 tip.

### S3 Sparse pool (Opus)
Bead: BUG-7wna (sparse tile storage). Files: whatever its `rg` finds across the storage helpers, the step, pressure, bodies, whitewater, sheeting, the sorter (`sort_particles_into_cells.rs` and `.wgsl`, shared with MPM through `matter_common.rs:16`), `frame_history.rs`, `liquid_frame.rs`, `liquid_state.rs`, `liquid_stats.rs`, `gpu_flip_domain.rs`, and `migration/liquid.rs`.
1. Lattice fields live in a tile pool allocated and freed on the GPU with no readback. The sorter keys on (slot, local cell). Extend runs on the frontier only.
2. The particle pool grows with the water, so sheeting works without an Inflow.
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

### S4 Infinite domain (Opus)
Bead: BUG-zdiuw (GPU FLIP infinite domain). Files: whatever its `rg` finds across the storage helpers, `gpu_flip_domain.rs`, `fluid_domain.rs` (shared with MPM), `lattice.rs`, `matter_domain.rs`, the step submodules, `liquid_solid_distance.rs` and `fluid_role_source.rs`.
1. The page table is keyed by world tile, and positions are tile-relative.
2. Bounds per I2, off by default. Bounds off removes water per I3.
3. Moving Domain Position or Bounds live never restarts the water.
4. Resolution becomes a cell size in metres (D14), for FLIP and MPM.
5. Deleted: the wall special cases, `closed_faces`, `wall_inset`, `box_offset`, `MAX_SIDE`, `lattice_refusal`, `DOMAIN_EXTENT_METRES`, the boundary clamp and `open_band`.
6. Tanks behave as today: the Dam Break with Bounds on and every face a Wall keeps its volume within the existing volume test's tolerance, and its wave front within one cell of the pre-S4 run at 1 s.

### S4b Surface, solids and wetness on world tiles (Opus)
Files: the surface group's lattice box (`liquid_lattice_box` in `assets/liquid_surface_group.json`, an R1b file now owned by this lane), `liquid_solid_distance.rs` (`:44`), `frame_history.rs` (`FIELD_SOLID`), the R1a field primitives, R7's wetness writer, and R2's march read path.
1. The surface field, solid distance and wetness live on the same world tiles as the water, with no dense box.
2. The march reads them through `ts_cell_at` and skips absent tiles.
3. Stills: water poured from an inflow over a ledge falls past where the old box ended and spreads on a floor; the memory report grows with the water; walls stay flush and wet.

### SP Pressure group (Opus)
In order:
1. The fused smoother.
2. Warm start from last tick's pressure, persisting in the pool.
3. A solve tolerance tied to what is visible: a velocity error in m/s derived from the cell size and frame time (one cell per frame is the visible limit); name the derivation.
4. The coarse level jitter, BUG-8koo0 (Solve Level jitters above 0).
Each step keeps the existing solver proofs green.

### S5 Narrow Band on tiles (Opus)
Ferstl et al. 2016, rebuilt on the storage helpers, working with sheeting, the field build and the speed changes. Particles live only in a band near the surface; deep water comes from the grid. The band width is derived from the CFL band the step already computes (`band_layers`).
1. A deep-pool proof: the particle count drops, and the surface and volume match the full-particle run within the volume test's tolerance.

## 5. Merging, review and landing

- **Merge as lanes finish.** The lead skims each commit against its checklist and the no-magic-numbers rule, merges it into `wave/water-field`, runs `cargo check` on the touched crates plus the lane's named tests on the merged tree, pushes the wave branch, and frees the slot.
- **Reviews by area, each starting behind the lanes it covers.** Read-only Opus agents at high effort, each with a budget of about 120 tool calls and a partial report at half:
  - render, after R2, R1b and RD;
  - look items, after R3, R4, R5 and R7;
  - storage, after S3;
  - domain, after S4 and S4b;
  - app integration: card rows, undo, saving and loading, Physics View, threads and export.

  Each lists what it did not read; anything unread gets its own scoped review. A reviewer's cost claim gets one measurement before anyone fixes it.
- **Fix lanes** take grouped findings, one commit each, under the same rules.
- **Final audits** (read-only Opus):
  - invented rules across the wave;
  - contradictions between the plan, the design doc, the code and the lanes' named calls;
  - the clean-cut sweep: every deleted name returns zero, with no dead code and no unused params or rows;
  - a literal and cap sweep: every numeric literal and `MAX_*` added in the wave diff is physics, derived or named and sourced.
- **Simplify and clean up (phase 8).** After the audits' fixes merge, three read-only Opus reviews (render, simulation, app and tests) read the whole wave diff for one thing: is this as simple and clean as it can be? Each finding names the simpler version and what it loses, if anything. They look for:
  - more machinery than the job needs: helpers used once, interfaces wider than their callers, duplicated logic, abstractions with one user;
  - anything left over from replaced systems: dead code, unused params and rows, stale comments and doc lines, leftover probes, prints and debug flags;
  - tests that protect nothing, and tests that duplicate each other;
  - files grown past 3000 lines, which get one split here;
  - names and comments that no longer match what the code does.

  Cleanup lanes (Opus, one commit each) apply the findings. Then a last sweep confirms that the clean-cut and literal audits still return zero. Branch and file cleanup comes after the landing:
  - delete the merged lane branches and the prototype branches (`proto/level-set-*`);
  - release and scrub every slot;
  - delete the scratchpad captures.
- **One quiet timing batch** after the last merge, with no lanes running. `fluid_capture` at 64 and 128 on the Dam Break: main against the wave, alternating, three runs each, reporting `gpu_ms`, `frame_ms` and memory.
- **Landing.** `scripts/land_branch.py wave/water-field` once, followed with `scripts/watch_land.py --kind land`. Green lands on main. A red gets a typed bead and a fix lane. A red proven on main is named with `--named-red` and `--reason`.
- **Blockers that need Peter:** decide, log in `decisions.md` and the report, keep going.

## 6. Report to Peter

- What landed, with the launch command.
- Calls made for him, listed first (I11 among them).
- What was deleted.
- The timing and memory batch.
- Images for each look item.
- Anything parked, with beads.

## 7. Decisions log (append-only)

Lanes read this section from the main checkout before pausing on any fork.

- **W1. One landing of the finished set, no interim landings; no per-lane `--cheap` gate.** The full gate runs once on the finished wave, and it lands on main when green.
- **W2. Opus reviews by area, the audits and the phase 8 simplify-and-clean-up pass; no Fable final review for this wave.**
- **W3. At most three lanes hold a built slot at once.** Disk has about 180 GB free, and a built slot takes 45 to 60 GB.
- **W4. No magic numbers, and contradictions stop the lane** (section 1, rules 4 and 5).
- **W5. Blockers are decided by the lead, logged here and in the report, and work continues.**
- **W6. W3 is replaced by the disk rule in section 3** (free space minus the reserve, 60 GB per built slot, re-checked at each launch). Read-only work runs alongside without a slot, and look items start their simulation half before the march merges.
- **W7. Field build forks (R1a).** Count and carry radii are in cells as D2 says (1.5 and 2 cells, named constants citing the prototype), so Particle Scale never shifts the weights. The weights are a pure function of the frame's blended particles, with no cross-frame display state: a blend over display frames would make a frame depend on how it was reached. Flicker without the time blend is measured against the prototype; a per-tick blend on the sim side follows only if it flickers. `clamp_liquid_to_solids` runs twice (carry into all solids early, keep drawn solids late), and its drawn-solids input is wired by R1b. `FluidBlob` shrinks to centre plus radius in R1a (I11).
- **W8. SB is the simulation on its own compute queue.** The live app never waits on the GPU per frame; sim and render share one command buffer and run in order. `submit_wait_ms` measured the capture tool's own wait, so SB is judged by live tick interval and commits nothing if it gains nothing.
- **W9. `LiveExtent` stays.** The refinement made it a general type (the fragment cut, `frame_setup.rs:108-111`). RD deletes only water's published-extent path, and its rg check lists the remaining non-water uses by file instead of expecting none.
