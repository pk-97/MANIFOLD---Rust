# Water field wave — plan

<!-- index: Orchestration plan for the GPU FLIP water upgrade, executed warm-tree with Haiku/Sonnet-first delegation: the mesh replaced by drawing straight from the field (clean cut), the look items, wet surfaces, the sheeting fix, the tick overlap, sparse storage then the infinite domain with the surface on world tiles, Narrow Band rebuilt on tiles, and the pressure speed group. Workflow (warm-tree, pre-chew/delegate, clean cut, no magic numbers, verification), rulings I1–I13, task routing and specs, final verification. -->

**Status:** PLAN · 2026-10-23, re-planned 2026-10-26 for the shared-checkout way of working. Anchors for the first pieces (S0, R0, R1a, SA, S1) checked against main f8084ab59; later pieces are re-anchored by the lead at handoff, because earlier pieces move their lines. Rulings: `docs/GPU_FLIP_END_GAME_DESIGN.md` D2, D4, D13 to D18. Umbrella: BUG-l2h3 (GPU FLIP live instrument).

## 1. Workflow

- **Main checkout, no worktrees.** Every job edits main directly and commits its own paths (`CLAUDE.md`).
- **The lead does the finding, workers do the edits.** Before a handoff the lead reads the code and writes the job: exact `file:line` anchors, the formula or the deletion list, and one scoped test command. No job says "whatever `rg` finds"; the lead runs the `rg` and hands over the list. `haiku` (the `lane` agent) takes splits, deletions, includes and wiring; `sonnet` takes a self-contained module or shader whose math the lead gives it.
- **Clean cut and no magic numbers (D14, D15).** Replaced code, nodes, params and tests go in the same change. Every constant is physical in real units (cited), derived (one-line comment), or one named constant citing the paper or prototype.
- **Checks per job.** `cargo nextest run -p <package> <filter>` and `cargo clippy -p <package> --tests -- -D warnings` on the packages the job touched, nothing wider. GPU tests go through `scripts/gpu_queue.py`, one at a time. The lead reads the diff and the test output, then commits. Screenshots only where the water looks different (named per job).
- **The MANIFOLD app is closed.** Every GPU run waits 8 minutes for the app to quit, then reports NOT RUN.
- **Prototype.** `/Users/peterkiemann/MANIFOLD - Prototypes/levelset_proto/` (source in `src/`, captures in `cap64*`, `cap128*`, `clip128`, montages in `out*`). The 2026-10-24 lead's anchor notes for R0, R2, SB and RD and the SA probes are beside it in `water-wave-2026-10-24/`; their line numbers are from 3de11dc8e.

### What runs side by side

Two jobs run at once only when neither can break the other's build. Builds share one `target/`, so compiles still queue; the gain is in edit time.
- **Alone:** any job that edits `manifold-core`, `manifold-gpu` or `manifold-node-engine`. Every water and render package depends on all three.
- **Side by side:** one render job in `manifold-nodes-scene` with one water job. Neither depends on the other. Two water jobs never run at once: `manifold-nodes-water` builds on every water package, and `water-liquid` builds on `manifold-fluids`.
- **`manifold-nodes`** (the contract and GPU proof tests) depends on everything; its tests run only when no other job is mid-edit.

### The 2026-10-24 start

The wave was started on 2026-10-24 under the old worktree workflow, then stopped. S0 (`lane/water-s0`, 554a4ae50) and R0 (`lane/water-r0`, 3d426ca8d) were finished there but are 33 commits behind main and conflict with it (`cargo fmt`, the render pass of f8084ab59). Both are redone fresh on main, using those commits as the file layout to copy. Uncommitted half-work in `.claude/worktrees/slot-7` (R1a, 58 files), `slot-1` (S1, 20) and `slot-5` (R4, 13) is reference only. The old `lane/water-*`, `merge/water-wave` and `wave/water-field` branches and the slot worktrees are deleted once Peter agrees.

## 2. Implementation rulings (the lead's, binding on every job)

- **I1 Absent tiles** read their defined value: `GPU_FLIP_SPARSE_BLOCKS_DESIGN.md` section 4 (defined values). Water 0, distance 3 cells (air), face absent.
- **I2 Bounds faces.** With Bounds on, each face is Wall or Open (D4). A Wall is an ordinary solid in the solid distance. An Open face removes water that crosses it. This replaces `closed_faces`, and every box-wall special case in the kernels is deleted.
- **I3 Bounds off.** Water is removed once it is lower than the scene's lowest solid point by more than one particle's drawn radius (Particle Scale × particle radius), so nothing visible is cut. The domain gets that height as one scalar.
- **I4 No saved-show migrations.** Pre-release. `migration/gpu_flip_surface.rs` and its wiring at `blob_bounds.rs:182` are deleted with the mesh chain; `migration/liquid.rs:49` loses its `retired_max_speed` wire with S3.
- **I5 Reference engine.** These go with the mesh chain (RD): `crates/manifold-fluids/src/surface.rs`, `SurfaceOptions`, `decode_surface`, `meshing_ms`, the C shims such as `manifold_fluids_world_capture_surface_frame`, and the oracle tests built on them. `mesh.rs` there is collider meshes and stays. The shims live in our own `native/bridge.cpp` and `bridge.h`, which lose them; the vendored engine under `native/flip_engine/` is never edited.
- **I6 Big files.** `render_scene.rs` (3178 lines) takes only the hook lines that call a new submodule. S0 splits `gpu_flip_step.rs`, `gpu_flip_step_tests.rs` and `gpu_flip_step.wgsl`.
- **I7 The tracer.** Water casts no shadow and stays out of the tracer, as today. The field hit is a later task under BUG-651ho (GPU FLIP surface: one level set, drawn directly).
- **I8 Owners.**
  - R1b owns `presets/gpu_flip.rs`, `scene_family.rs`, `assets/liquid_surface_group.json`, `scene_schema/water.rs` and `scene_compile/water.rs`. Exceptions: R4w deletes the foam draw block at `gpu_flip.rs:632-655`, and R4w, R5w and R7w add their own override rows.
  - R0 owns `render_scene.wgsl` until R0 is committed; after that it takes one-line hook calls only.
  - RD regenerates the six preset fixture JSONs, once.
- **I9 Foam.** The foam field draws surface foam. Foam particles keep simulating as its source; their icosahedron draw is deleted. Spray and bubble draws are unchanged.
- **I10 Thickness.** `volume_optics` stays for other closed transmissive meshes (`render_scene.rs:2306`). Water stops feeding it; the march supplies water's thickness.
- **I11 Surface stays spheres.** The prototype measured stretched blobs bumpier and losing sheets (D2). R1a deletes Stretch and Centre Smoothing from `shape_particle_blobs`, with the anisotropy code they drive. This is a call made for Peter; it goes first in the report.
- **I12 Modifiers.** Water is not a modifier target today (`scene_schema/fields.rs:95`; never in `b.drawn`, `scene_compile/object.rs:170`), so nothing regresses. R1b adds a one-line refusal reason for water in the Targets menu, following the Ocean precedent (`modifier.rs:445-457`). Loop, Mirror, Copies and deformers on water are a later bead.
- **I13 Not in this wave, logged:** the 4K half-resolution search, the tracer hit, traced refraction, field versions of Shatter, Surface Peel and Ordered Recon (BUG-l2h3.30.13), and APIC.

## 3. Tasks & Routing Table

Track: **R** is the render track (`manifold-nodes-scene` only), **W** the water track (water packages), **A** runs alone (section 1). A job with a sim half and a render half is two jobs, one per track.

| Job | Worker | What | Track | Needs |
|---|---|---|---|---|
| S0 | haiku | Split the step file, tests and WGSL, copying 554a4ae50's layout | W | — |
| R0 | haiku | Shader helpers into an include, copying 3d426ca8d's layout; look hook stubs | R | — |
| R1a | sonnet | Field build: re-distance, weights, smooth, carry-in; spheres only | W | — |
| R2.0 | haiku | Optional `field` input on `scene_object` | A (`node-engine`) | — |
| R2 | sonnet, lead gives the march math | The march pass, with light through thin water | R | R0, R2.0 |
| R3 | sonnet | Ripple detail | R | R2 |
| SA | lead diagnoses, sonnet ports the fix | Sheeting volume fix (W10 re-distance depth) | W | S0 |
| R1b.1 | haiku | Water rows and the modifier refusal in `manifold-core` | A | R1a, R2 |
| R1b.2 | sonnet | Water graph, card and preset; wires R1a and R2 | A (`nodes-water` and `nodes-scene`) | R1b.1 |
| RD.1–RD.5 | haiku; RD.3 sonnet | Mesh chain deletion, split by package | W; RD.3 A | R1b.2 |
| R4r, R5r, R7r | sonnet | Render halves of foam, mist, wet surfaces | R | R2 |
| S1 | sonnet | Storage helpers + step kernels | W | S0, SA |
| R4w | sonnet | Foam splat into a surface field (whitewater) | W | R4r |
| S2 | haiku, lead gives the line list | Pressure, bodies, whitewater, sheeting onto the helpers | W | S1, R4w |
| R5w, R7w | sonnet | Water halves of mist and wet surfaces | W | S2 |
| SB | sonnet, lead gives the queue design | Next tick overlaps render on its own compute queue (W8) | A | — |
| S3.1–S3.5 | sonnet; S3.5 haiku | Sparse pool, split into five jobs | W | S2 |
| SP | sonnet | Pressure group: fused smoother, warm start, visible tolerance, coarse jitter | W | S3.5 |
| S4.1–S4.5 | sonnet; S4.3 A; S4.5 haiku | Infinite domain, split into five jobs | W | S3.5 |
| S4b | sonnet | Surface, solids, wetness on world tiles | W | S4.5, R1b.2, R7w |
| S5 | sonnet | Narrow Band on tiles | W | S4b, SA |

**Order.** Each step lists what runs at once. A jobs run when both tracks are between commits.
0. Before any job starts, the lead builds the base `fluid_capture` (`scripts/dev.py capture` builds it) and copies the binary to the scratchpad, for the final speed check.
1. R0 with S0. Meanwhile the lead diagnoses SA (read only).
2. R2.0 alone. Then R2 with R1a.
3. R3 with SA.
4. R1b.1, then R1b.2, both alone.
5. RD.1, RD.2 and RD.4 one after another on W, while R runs R4r, R5r and R7r. RD.3 alone. RD.5 last.
6. S1, then R4w, then S2.
7. R5w, then R7w.
8. SB alone.
9. S3.1 to S3.5.
10. SP, then S4.1 to S4.5, then S4b, then S5.
11. Final check (section 5).

The water track is the long pole. One night realistically reaches step 6 or 7; S3 onward is a second night.

## 4. Task Specs

Every job's brief is its entry here plus sections 1 and 2, with the anchors the lead re-checks at handoff. Anchors marked (f8084ab59) were checked on 2026-10-26; the rest come from 5b3a6d255 and have moved.

### R0 Shader helpers

Files: `crates/manifold-nodes-scene/src/node_graph/primitives/shaders/render_scene.wgsl`, a new `scene_shading.wgsl` beside it, five `look_*.wgsl` stubs, and the include wiring in `render_scene/pipelines.rs`.
Layout to copy: commit 3d426ca8d (`git show 3d426ca8d --stat`). Its `scene_shading.wgsl` holds the uniforms, environment, lights, shadows, BRDF helpers and fog, plus `light_visibility`, `pbr_direct_lighting`, `ibl_reflection`, `ibl_irradiance` and `pbr_ibl` lifted out of `fs_pbr`. Re-apply it to today's file; do not cherry-pick.
Anchors (f8084ab59), `render_scene.wgsl`, 2158 lines: `env_lookup_uv` :247, `light_direction_attenuation` :297, `rt_or_flat_ambient` :570, shadows `shadow_layer_uv` to `shadow_factor` :623-781, `apply_fog` :1504, BRDF helpers :1539-1616, `fs_pbr` :1676, `fs_cel` :2125. `pipelines.rs` builds shader text from `include_str!` at :303, :341, :355, :396, :454, :468 and :537; each gets the include.
1. The lighting, shadow, IBL and fog helpers move into one include that `render_scene.wgsl` and a full-screen pass both use, without the mesh vertex shader.
2. Look hooks as plain WGSL function stubs, one include file per look item, each called from the water shading. One hook is also called from `fs_pbr` for wetness. One bind group, owned by the march, carries the look items' inputs. No text-substitution variants: every look is always on (D17).
3. Pixel-identical renders before and after, on a raster scene and a ray-traced scene. Render the base twice first, so the comparison is known to be deterministic.
4. The `custom_abi_cases.rs` shader path for `RenderSceneUniforms` (to `scene_shading.wgsl`) is the lead's line, since S0 edits the same file.

### R1a Field build

Files:
- `crates/manifold-water-liquid/src/primitives/smooth_lattice.rs` (`:35` already runs the [1, 2, 1] filter per axis) and `redistance_lattice.rs` (the band param `:51` is in metres today);
- `crates/manifold-water-gpu-flip/src/primitives/clamp_liquid_to_solids.rs` (the primitive `:48`);
- `crates/manifold-water-surface/src/primitives/shape_particle_blobs.rs` (Stretch `:31`, `:52`, `:70`);
- new weight primitives in `manifold-water-liquid/src/primitives/`, with their shaders, tests and `mod.rs` entries.
Anchors above are (f8084ab59). Keep `marching_cubes_common.wgsl`: `redistance_lattice.rs:63` uses it.
Reference: the prototype, `/Users/peterkiemann/MANIFOLD - Prototypes/levelset_proto/src/weighted.rs`, and design doc D2 (the field build). The half-done 2026-10-24 attempt in `.claude/worktrees/slot-7` is reference only.
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

R2.0 goes first, alone: the optional `field` input on `crates/manifold-node-engine/src/scene/scene_object.rs` and the `node.scene_object` node (`manifold-nodes-scene/.../primitives/scene_object.rs`), with nothing reading it yet.

Files:
- new `render_scene/water_march.rs`, `shaders/water_march.wgsl`, `shaders/water_shade.wgsl`;
- hook lines in `render_scene.rs` (frame order `:2619-2917`), `pipelines.rs`, `resources.rs`, `construction.rs`;
- `render_scene/frame_setup.rs`, `pass2_draw.rs`, `item_pick.rs`, `selection_mask.rs`, `prepasses.rs` (field kind only). The 4K render pass (f8084ab59) rewrote parts of `pass2_draw.rs` and `selection_mask.rs`; re-anchor these at handoff.
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
7. A GPU proof that the march hits a known sphere field at the right depth and normal. Stills of the Dam Break at 64, frames 45 and 240, beside the prototype's montages in `out4`: same shape, no seams, flush at walls. One still at 128, frame 240.

### R1b Water graph, card and preset owner

Files: per I8, in two jobs, both alone. R1b.1 is the `manifold-core` part: the row changes in `scene_schema/water.rs` (item 2) and the refusal in `scene_schema/modifier.rs` (item 4). R1b.2 is the rest. Anchors:
- `gpu_flip.rs`: `:1029-1049` `surface()`, `:1166-1198` `display_head()`, `:1187-1194` the teal material, `:885-930` the mesher controls, `:577-583` and `:1206-1211` the Physics View switches;
- `scene_schema/water.rs`: `:162-203` rows, `:109` Surface Detail;
- `modifier.rs:445-457`, the refusal precedent.
1. FLIP and MPM water build the field (R1a) and draw it through the march (R2). The group outputs the clamped level set and the velocity lattice. Physics View switches the field.
2. Card (D17):
   - Smoothness is automatic, with one override slider. Its default is the prototype's settings; name the mapping.
   - Ripple detail, foam, mist, thin water and wetness are automatic, with one override slider each. R1b.2 adds the rows for what exists when it runs (ripple detail, thin water); R4w, R5w and R7w each add their own row to these files, so no row ships before its look.
   - Delete the Stretch, Centre Smoothing, Fill Pits and mesh Smoothing rows, and the `offset_lattice` Fill Pits node if nothing else uses it.
   - Surface Detail stays: the field's resolution over the sim's.
3. The default water material is physical (D2), and the presets use it (D17).
4. Water's one-line refusal reason in the Modifier Targets menu (I12).
5. Sheeting on by default (SA is committed before R1b). Tank presets with Bounds on and every face a Wall are a one-line follow-up after S4.5 (D18).
6. Stills of the Dam Break and an MPM preset through `fluid_capture`.

### RD Mesh chain deletion

Five jobs, split by package. The (f8084ab59) inventory is 78 files from the `rg` in check 1 plus `live_extent`; the lead re-runs it at handoff and gives each job its exact list.
- **RD.1** (haiku, W). `manifold-water-surface`: the six nodes in `src/primitives/` (`volume_surface_mesh`, `smooth_surface_mesh`, `relax_surface_mesh`, `count_surface_triangles`, `count_surface_edges`, `surface_mesh_normals`) with their extents, shaders, `surface_mesh_parity.rs`, `testkit/surface.rs` and `mod.rs` entries. Also the mesh mentions in `manifold-water-liquid` (`running_total.rs`, `smooth_lattice.rs`, `marching_cubes_common.wgsl`) and `clamp_liquid_to_solids.rs`.
- **RD.2** (haiku, W). `manifold-fluids`, per I5: `surface.rs`, `SurfaceOptions` (`lib.rs`, `particles.rs`), `decode_surface`, `meshing_ms`, the shims in `native/bridge.cpp` and `bridge.h`, `examples/boundary_probe.rs`, `examples/surface_tuning.rs`, `tests/boundary_regression.rs`.
- **RD.3** (sonnet, A). Water's published-extent path only, in `manifold-node-engine` (`bindings.rs`, `exec/execution.rs`, `exec/backend.rs`, `exec/metal_backend.rs`, `testkit/array_harness.rs`, `scene/mod.rs`, `scene/live_extent.rs`), `manifold-nodes-image` (`switch_mesh.rs` and its `gpu_tests.rs`), `manifold-nodes-scene` (`scene_object.rs`, `render_scene.rs`, `render_scene/frame_setup.rs`, `render_scene/live_draw_args.rs`), `manifold-gpu/src/metal/shader_compiler.rs`, and `liquid_mesh` in `manifold-core/src/scene_schema/water.rs`. `LiveExtent` itself stays (W9): the platonic mesh publishes its extent (f8084ab59), and `switch_mesh` and the fragment cut read it. The lead marks the water-only lines.
- **RD.4** (haiku, W). `manifold-nodes-water` (the mesh nodes in `assets/liquid_surface_group.json`, `mesh_sink` and `liquid_mesh` in `presets/gpu_flip.rs`, `primitives/liquid_bricks_consumer_tests.rs`, `primitives/liquid_surface_tests.rs`, and the I4 migration); the 22 `manifold-nodes` files (catalog and contract tests, and in `examples/fluid_capture.rs` the `--gpu-surface` and `--dump-mesh` flags, `dump_surface_meshes`, and the vertex count and meshing columns); `manifold-io/tests/fixtures/water_layer_graph_v1160.json` and its users; the mesh sections of `GPU_FLUID_SURFACE_DESIGN.md`.
- **RD.5** (haiku, W). The six preset fixture JSONs and their thumbnails (`scripts/dev.py thumbnails`), regenerated once from the builders.

Checks:
1. `rg -n 'volume_surface_mesh|smooth_surface_mesh|relax_surface_mesh|count_surface_triangles|count_surface_edges|surface_mesh_normals|liquid_mesh|capture_surface_frame|SurfaceOptions' crates scripts tools assets docs .claude` returns only design-doc text that records the removal. `rg -l live_extent` returns only the non-water uses, listed by file in the report.
2. Every test and GPU proof that wired the chain is converted to the field or deleted, and listed.
3. Each job runs nextest and clippy on its own packages. After RD.5, one `cargo check --workspace --tests`, because RD touches thirteen packages.

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

Bead: BUG-l2h3.30.7 (foam field on the surface). Two jobs:
- **R4r** (R): its look include and `manifold-nodes-scene` module, reading the foam field through the march's look bind group. Its test feeds a hand-made field.
- **R4w** (W): the foam splat from the whitewater foam particles into a surface field (new files in `manifold-water-whitewater`), and the foam draw block at `gpu_flip.rs:632-655` (I8 exception).
1. Foam is carried by the flow, gathers where water converges, and fades exponentially with a physical lifetime. A test checks it against the closed form.
2. The foam icosahedron draw is deleted; the foam particle sim stays as the source.
3. Stills of the Dam Break settling and of a breaking wave.

### R5 Mist

Bead: mist, under BUG-l2h3.30 (GPU FLIP water rendering). Two jobs: **R5r** (R), its look include and the march's accumulation, tested on a hand-made density; **R5w** (W), the spray-to-fog density on the water tiles, in `manifold-water-whitewater`.
1. Thin fog density from spray, with droplet size and density per metre, accumulated by the march under Beer–Lambert. A test checks it against the closed form.
2. Stills of a splash with mist.

### R7 Wet surfaces

Bead: BUG-l2h3.30.10 (wet surfaces). Two jobs: **R7r** (R), the `fs_pbr` hook from R0 and its look include, tested on a hand-made wetness field; **R7w** (W), the wetness field on the water's lattice (a new module; the lead picks the water package) and its writer from water touching solids. Materials are shared, so wetness is sampled by world position, never stored in a material node.
1. Everything the water touches darkens and dries at a physical rate: a thin film's evaporation, a named constant with its source. Automatic, with one override slider.
2. A test checks drying against the exponential closed form.
3. Stills of the tank floor as the Dam Break wave passes, and 10 s later.

### S0 Split the step files

Files: `crates/manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs` (4114 lines), `gpu_flip_step_tests.rs` (4618) and `shaders/gpu_flip_step.wgsl` (2882), all (f8084ab59). Mechanical only: no change to logic, names or behaviour.
Layout to copy: commit 554a4ae50 (`git show 554a4ae50 --stat`). It splits the step into `gpu_flip_step/` (`emit`, `encode`, `extend`, `lattice`, `narrow_band`, `pipelines`, `pockets`, `primitive`, `sheeting`, `state`, `tests`, `tiles`), the tests into `gpu_flip_step_tests/` (`cells`, `density`, `emit`, `motion`, `pockets`, `solids`, `step_order`) and the shader into `shaders/gpu_flip_step/` (`common` plus one file per pass). Main has changed these files since (`cargo fmt`, deleted tests), so re-apply the layout to today's files; do not cherry-pick.
1. Clippy and the crate's nextest pass, with the same test count before and after.
2. Every new file is under 1500 lines.
3. The `custom_abi_cases.rs` shader path for `StepParams` (to `shaders/gpu_flip_step/common.wgsl`) is the lead's line, since R0 edits the same file.

### SA Sheeting volume fix

Bead: BUG-a8eh6 (Sheet Fill Rate 0.5 grows the Dam Break water by 61% in 4 seconds). Files: `gpu_flip_sheeting.rs`, `.wgsl` and their tests; the sheeting submodule from S0; `liquid_stats.rs`.
Sheeting goes beyond FLIP Fluids on depth: native FLIP Fluids grows the Dam Break by 62% in 4 s with sheeting at 0.5 too, because its level set is a union of spheres re-distanced short of its own 3 dx target, so deep calm water passes the thin-water test; ours re-distances to a depth derived from the sheet test's threshold, so only thin water seeds.
Suspects, ranked:
- The border clear counts from the grid edge, not the wall, so a thin floor pool re-seeds every substep (`gpu_flip_sheeting.wgsl:298-306` `feather_border`, its 3-cell band at `:305`; the cell count in `manifold-water-liquid/src/lattice.rs:71-77`, all (f8084ab59)).- Volume Projection is off in the preset (`gpu_flip.rs:161` (f8084ab59)).
- Seeding runs per GPU substep; the engine seeds per step (`fluidsimulation.cpp:11053-11057`).
The lead names the root cause with evidence (read only, during step 1; the 2026-10-24 probes are in `water-wave-2026-10-24/sa/`), then hands sonnet the fix to port.
1. Root cause named with evidence, and the fix ported from the reference.
2. A volume proof: a resting pool seeds nothing; Dam Break drift is reported, not gated.
3. Sheet births dropped for lack of room reach the card through the stats word.

### SB Next tick overlaps this frame

Runs alone. Files: `crates/manifold-node-engine/src/exec/execution.rs` (6683 lines (f8084ab59); the lead writes a line map into the brief), `substeps.rs`, `liquid_state.rs`, `liquid_frame.rs`.
1. The water simulation runs on its own compute queue through `manifold-gpu`, so the GPU computes the next tick while this frame draws. The render reads retired history slots, never the live state, so the only ordering point is the publication copy, fenced by a shared event. The API is shaped so Vulkan's queue and timeline semaphore map onto it (`docs/VULKAN_BACKEND_DESIGN.md`). The live app never waits on the GPU per frame; sim and render share one command buffer and run in order.
2. Measured by live tick interval on the Dam Break at 64, interleaved A/B under the GPU lock (memory: frame-timing method), never by `fluid_capture`'s `submit_wait_ms`, which wraps sim and render in one wait. `submit_wait_ms` measured the capture tool's own wait, so SB is judged by live tick interval and commits nothing if it gains nothing.
3. The existing timing and frame-exact tests pass; export stays exact. Reading the past never changes it.

### S1 Storage helpers and step kernels

Files: `TileTable` (`gpu_flip_step.rs:531-590` (f8084ab59), in `gpu_flip_step/tiles.rs` after S0), grown in place into the storage helpers in `crates/manifold-water-liquid/` (Rust plus one WGSL include); the S0 step submodules and the step WGSL. Design: `GPU_FLIP_SPARSE_BLOCKS_DESIGN.md` sections 3 and 4.
1. The helpers cover only what callers use today: `ts_list_cell`, `ts_neighbour`, `ts_face`, `ts_cell_at`, `ts_origin`. Their first version indexes today's dense lattice. Pool reservation and the exhausted word come with S3.
2. Every step kernel goes through them.
3. Bitwise identical on the Dam Break over 120 steps (proof).

### S2 Pressure, bodies, whitewater and sheeting onto the helpers

Files: `gpu_flip_pressure.rs` and `.wgsl`, `gpu_flip_lentine.wgsl`, `gpu_flip_bodies.rs` and `.wgsl`, `liquid_faces.wgsl`, the `whitewater_*` files, the sheeting files.
1. No indexing helper exists outside the storage helpers. These return nothing outside them: `rg -n 'p\.x \+ n\.x \*|fn flatten|fn flat\b|fn nb_index|fn ww_cell_index|fn lf_face_index|fn c_cell_index|fn index\('`.
2. Whitewater reads one distance source; the `unwrap_or(level_set)` fallback at `whitewater_step.rs:708` is deleted.
3. Bitwise identical on the Dam Break over 120 steps against the S1 tip.

### S3 Sparse pool

Bead: BUG-7wna (sparse tile storage). Five jobs in order, all W. Since S1 and S2 every kernel reads lattice fields through the storage helpers, so moving the helpers onto the pool moves the kernels with them. Each job leaves the Dam Break inside the volume test's tolerance; where one can't stand alone, the lead joins it to the next.
- **S3.1** (sonnet). The tile pool: lattice fields live in tiles allocated and freed on the GPU with no readback, with pool reservation and the exhausted word; the helpers index the pool. Files: the storage helpers in `manifold-water-liquid`, `gpu_flip_domain.rs`.
- **S3.2** (sonnet). The sorter keys on (slot, local cell), for FLIP and MPM: `sort_particles_into_cells.rs` and `.wgsl` in `manifold-water-liquid`, shared with MPM through `matter_common.rs:16`. Extend runs on the frontier only.
- **S3.3** (sonnet). The particle pool grows with the water, so sheeting works without an Inflow. Every particle pass dispatches from the live count through a GPU-written indirect argument; no CPU readback sizes any pass, including `history_slots` from `retired_max_speed`. Files: `liquid_state.rs`, `frame_history.rs`, `liquid_frame.rs`, `migration/liquid.rs`.
- **S3.4** (sonnet). A full pool pauses sources and the card says why, never a crash (D4). Sheet births dropped for lack of room reach the card through the stats word (`liquid_stats.rs`).
- **S3.5** (haiku). The deletions below, from the lead's exact list.

Checks:
1. Lattice fields live in the pool, the sorter keys on (slot, local cell), extend runs on the frontier only.
2. No CPU readback sizes any pass.
3. A full pool pauses sources with the card's reason.
4. Deleted:
   - the dense `LatticeBuffers` and the dense scratch terms;
   - `tiles_fill` and the canonical rewrite;
   - the `all_tiles` dense oracle and the box-indexed bases;
   - the dense `faces` output and the frame-history face copies;
   - dense emit sites and dense sheeting scratch;
   - Narrow Band entirely, with the frame history's `FIELD_INTERIOR`, liquid_frame's interior and `GPU_FLIP_NARROW_BAND_DESIGN.md`.
5. Memory reported against D4's budget at 64 and 128. No stills: the water should look the same, and the volume test checks that it moves the same.

### S4 Infinite domain

Bead: BUG-zdiuw (GPU FLIP infinite domain). Five jobs in order:
- **S4.1** (sonnet, W). The page table is keyed by world tile, and positions are tile-relative. Files: the storage helpers, `gpu_flip_domain.rs`, the step submodules.
- **S4.2** (sonnet, W). Bounds per I2, off by default; Bounds off removes water per I3. Files: `manifold-water-gpu-flip/src/primitives/liquid_solid_distance.rs`, `manifold-water-liquid/src/primitives/fluid_role_source.rs`, the step submodules.
- **S4.3** (sonnet, A). Resolution becomes a cell size in metres (D14), for FLIP and MPM. Files: `manifold-core/src/fluid_domain.rs` (shared with MPM), `matter_domain.rs`, `gpu_flip_domain.rs`, `manifold-water-liquid/src/lattice.rs`, the card rows.
- **S4.4** (sonnet, W). Moving Domain Position or Bounds live never restarts the water.
- **S4.5** (haiku, W). Deleted: the wall special cases, `closed_faces` (`lattice.rs:24`), `wall_inset` (`lattice.rs:88`), `box_offset`, `MAX_SIDE`, `lattice_refusal`, `DOMAIN_EXTENT_METRES`, the boundary clamp and `open_band`, from the lead's exact list.

Check: tanks behave as today: the Dam Break with Bounds on and every face a Wall keeps its volume within the existing volume test's tolerance, and its wave front within one cell of the pre-S4 run at 1 s.

### S4b Surface, solids and wetness on world tiles

Files: the surface group's lattice box (`liquid_lattice_box` in `assets/liquid_surface_group.json`, an R1b file now owned by this job), `liquid_solid_distance.rs` (`:44`), `frame_history.rs` (`FIELD_SOLID`), the R1a field primitives, R7's wetness writer, and R2's march read path.
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

- Regenerate the node catalog (`scripts/dev.py node-catalog`).
- Clean-cut `rg` sweep: every deleted name returns zero across `crates scripts tools assets docs` (design-doc removal records excepted), with no dead code and no unused params or rows.
- One speed and memory check, with nothing else running: `fluid_capture` on the Dam Break at 64 and 128, the base binary from step 1 against today's build, alternating, two runs each, reporting `gpu_ms`, `frame_ms` and memory.