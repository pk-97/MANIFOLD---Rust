# Scene geometry at any scale

<!-- index: How render_scene stops paying for triangles nobody sees: indexed terrain, culled copies in shadows, small patches culled on the GPU, detail levels by screen size, terrain tiles by distance, and NVIDIA cluster ray tracing with the Vulkan port. -->

**Status:** IN PROGRESS. P1, P2, P3, P5a and P5 landed 2026-10-30. P4 and P6 are on hold: on the Mac the measured landscape is pixel-bound (see Findings). P7 waits for the 5080.
**Companions:** [RAYTRACING_DESIGN.md](RAYTRACING_DESIGN.md) (acceleration structures), [VULKAN_BACKEND_DESIGN.md](VULKAN_BACKEND_DESIGN.md) (P7 ray tracing), [GLB_IMPORT_OPTIMIZATION_GUIDE.md](GLB_IMPORT_OPTIMIZATION_GUIDE.md) (the manual decimation this replaces).

## The claim

A scene of any size, and a hero shot up close, holds frame rate without anyone decimating a mesh or thinking about scale. Every step looks identical to today except detail levels, which change pixels by less than one pixel of error. Hidden, off-screen and far-away geometry stops costing vertex work.

## What exists (read 2026-10-30)

- Every mesh draws whole, one draw per object per pass: each shadow view (one per light, six for a point light), the opaque depth snapshot, Pass A, Pass B for blended draws, subsurface, caustics (`render_scene/pass2_draw.rs`, `render_scene/prepasses.rs`).
- Culling is per object on the CPU, against the frustum only (`render_scene/frustum.rs`, `frame_setup.rs` `in_view`). Instanced arrays of 1024+ opaque copies get a GPU frustum cull for camera passes only; shadow passes draw every copy (`shaders/cull_instances.wgsl`).
- No detail levels, patches, occlusion culling or mesh shaders anywhere. Decimation is a manual `gltf-transform` step.
- The draw path already takes indexed meshes: `node.scene_object`'s optional `indices: Array(u32)` reaches every pass, the selection mask, previous positions and the acceleration structure (`frame_setup.rs` around `indices_content`).
- Terrain: `node.terrain` writes a res² vertex grid with flat +Y normals; `node.make_triangles` expands it to a non-indexed (res-1)²×6 list with finite-difference normals (`scene_compile/object.rs` `emit_terrain`). `make_triangles` uses the default `Written` topology rule, so every morph rebuilds the terrain's acceleration structure instead of refitting it.
- Ray tracing builds one acceleration structure per object from its full triangle count (`metal/raytrace/accel.rs`, `render_scene/rt_frame.rs`).

Measured (terrain session, M4 Max, 1080p, flat 512² terrain): 13 ms GPU a frame, 8 to 9 ms of it vertex work that doesn't scale with render size. `make_triangles` costs 0.77 ms every frame even when nothing moved. Photoscan scenes are geometry-bound, not fill- or shadow-texel-bound (GLB guide).

## Decisions

- **D1 Patches.** A patch is at most 128 triangles with a bounding sphere and a normal cone. Static meshes are cut once per topology at import, with meshoptimizer's meshlet builder. Generated meshes come out of their generator already tiled (Terrain tiles). A mesh under one patch is one patch.
- **D2 Drawing.** Each object keeps its own draw per pass, now an indexed indirect draw over a per-view index buffer that a compute pass fills with the triangles of visible patches. Indexed drawing keeps the GPU's vertex reuse, which a vertex-pulling instanced draw loses (about 5x the vertex work). Rejected: mesh shaders (WGSL has no mesh stage and the shader chain goes through spirv-cross); merging all objects into one draw per material (draw count is not the measured cost; revisit only if a scene's CPU encode time shows it).
- **D3 Culling.** On the GPU, per patch, per view: frustum, backface cone, and for the camera, occlusion against a depth pyramid. Occlusion is two-phase: draw last frame's visible patches, build the pyramid, then test the rest against this frame's pyramid and draw the newly visible. Nothing drops for a frame on a cut or a fast move, because phase two always re-tests against the current frame. Shadow views cull by their own frustum and cone, never the camera's.
- **D4 Detail levels.** For static and rigid meshes, a patch hierarchy built at import with meshoptimizer: group neighbouring patches, simplify each group with its border locked, re-cut into patches, repeat. Each patch stores its own error and its parent group's error. At runtime a patch draws when its own projected error is at most one pixel and its parent's is above. Each patch decides on its own, so the cut is crack-free and parallel. Deforming meshes draw their finest level, with patch bounds refreshed on the GPU when positions change.
- **D5 Terrain.** Indexed now (P1). Later, tiles by distance from the height map: a quadtree of fixed-size tiles at screen-error-driven resolution, neighbours at most one level apart, and a finer tile's edge vertices snapped onto its coarser neighbour's edge so no cracks open.
- **D6 Ray tracing.** On the Mac the acceleration structure stays at full detail, so traced shadows agree with the drawn surface. Tracing a coarser level would bring self-shadow speckle and shifted contact shadows. A coarser traced level for far objects comes only if a measured scene needs the memory or build time. On the 5080, the Vulkan ray tracing port builds per-patch structures with `VK_NV_cluster_acceleration_structure` (NVIDIA RTX Mega Geometry).
- **D7 Pay only for active work.** A settled mesh rewrites nothing, so it causes no refit, no shadow-cache miss and no culling-buffer rebuild. With no patched mesh in a scene, the culling dispatches don't run.
- **D8 Out of scope.** The water and sand marching-cubes mesh (pending Peter's call on BUG-651ho; the water session owns `tile_surface_mesh` and `particle_volume`), blended and transmissive draws (stay per object and sorted), sand grains and whitewater, smoke and fire.

## Checks for every phase

- **Identical look:** a still of a real scene before and after, compared pixel by pixel. P1 to P4 must match exactly (or within float noise on the same GPU). P5 and P6 are checked by eye on Peter's scenes, up close and far.
- **Speed:** a back-to-back `perf-soak` on the same scene before and after (M4 Max thermals drift; only adjacent runs compare).
- **Tests:** at most one per phase, and only for math (index winding, cone test, error projection).

## Phases

### P1 Indexed terrain, and a settled terrain costs nothing (landed)

Measured on the k6b terrain scene, M4 Max, 1080p: frame GPU time about 22 ms to 15 ms, stills identical within the rain's run-to-run noise.

Same look, same triangles; each grid point is shaded once instead of six times, and `make_triangles` leaves the terrain path.

1. `shaders/terrain.wgsl` `vertices_main` writes the finite-difference normal `make_triangles` computes today (`shaders/triangulate_grid_body.wgsl` `tg_compute_normal`): neighbours clamped to the grid, `n = normalize(cross(p(c, r+1) - p(c, r-1), p(c+1, r) - p(c-1, r)))`, +Y when the cross product is shorter than 1e-8. Neighbour positions are built exactly as the vertex's own position, so normals stay bit-identical.
2. A new pure primitive `node.grid_indices` (params `cols`, `rows`; output `indices: Array(u32)`, capacity (cols-1)×(rows-1)×6). Quad (c, r) writes `(c,r) (c,r+1) (c+1,r) (c,r+1) (c+1,r+1) (c+1,r)` as `row*cols + col`, the winding `make_triangles` uses (counter-clockwise from +Y). It is pure, so with unchanged params it never runs again and its content version holds, which keeps the acceleration structure's topology hint still.
3. `node.terrain` declares `mesh_output_rule("vertices")` as topology `Fixed`, positions `Written`: a morph refits the acceleration structure instead of rebuilding it.
4. `node.terrain` calls `ctx.mark_outputs_unchanged()` on frames where it doesn't dispatch `vertices_main`, so a settled terrain causes no refit and keeps the shadow-map cache.
5. `scene_compile/object.rs` `emit_terrain` wires `node.terrain` → object `vertices` and `node.grid_indices` → object `indices`, with cols and rows set from the same resolution. `make_triangles` stays in the catalogue for other graphs.
6. Before the edit, every reader of the terrain's vertices beyond the draw (physics, water colliders, scatter) is listed and checked to accept an indexed mesh.

Check: terrain still identical; `perf-soak` on a terrain scene; one test that `node.grid_indices` on a 3×3 grid matches the `make_triangles` corner order.

The Ocean draws its 640×360 projected grid through `make_triangles` every frame too (`scene_compile/ocean.rs`). It takes the same cut once terrain is proven: `node.grid_indices` for the indices, with the finite-difference normals written by a pass over the displaced grid.

### P2 Copies culled in shadows (landed)

Measured: swarmTestv3 shadows 3.7 to 3.0 ms, with the cull itself 0.5 ms dearer, so about 0.2 ms net; sceneLoopBugs unchanged on a fixed frame clock. Stills identical. The cull kernel's per-copy cost now matters as much as the copies it drops; P3's per-patch cull replaces it for meshes with patches.

`cull_instances` also runs for each shadow light's view, against that light's clip volume. The 1024-copy threshold (`CULL_MIN_INSTANCES`) becomes a work threshold, copies × triangles, so a Scene Loop corridor of a few dozen cells of a heavy scan is culled too. Same look. Spec at phase start.

### P3 Patches and GPU frustum and cone culling (landed)

As built, without new graph ports: a patch is a run of 128 consecutive triangles of an object's index buffer. Imports reorder their triangles at load (`render_scene/patch_cull.rs` `patch_order`: meshoptimizer's spatial sort, then a vertex-cache pass per run) and `node.grid_indices` writes Terrain in 8 x 8 quad tiles, so each run is compact. `shaders/cull_patches.wgsl` works out each patch's box and normal cone on the GPU once per mesh content, and per view copies the visible patches' indices into that view's index buffer, drawn indexed-indirect. Single opaque indexed meshes of 8 patches or more take this path; instanced draws keep P2's copy cull. The cull costs about 0.02 ms on the 2.4 km terrain.

Each view gets its own cull: the camera (frustum and back-face cone, for meshes that skip back faces) and each shadow light (its own frustum). The CPU per-object frustum test stays as the first reject. Not yet covered: per-patch culling inside instanced draws (copies, swarms, Scene Loop cells), and meshes a modifier deforms (see Findings).

### P5a Detail levels for copies (landed)

A mesh producer may publish `lods: Array(u32)` (layout in `primitives/mesh_lods.rs`: a 16-word header of up to four levels, each its first word, index count and simplification error in mesh units, then each level's indices into the same vertices). `node.gltf_mesh_source` builds them on its loader thread with meshoptimizer when the output is wired; the scene compiler wires them only for imported meshes drawn as copies. The copy cull (`cull_instances.wgsl`) picks each surviving copy's coarsest level whose error projects to at most one pixel at the copy's nearest point, counts copies per level, and lays the survivors out level by level so each level is one indexed indirect draw (its block's base instance points at its run). Each view picks its own levels: the camera by render size, each shadow caster by its map size. RT, picking and every unculled pass keep the full mesh.

### Tile budget (landed)

The scene pass draws into memoryless MSAA targets, which cannot spill when the tiler's parameter buffer fills; the GPU fault ends the session. A frame that can submit more than 16M triangles (falling back below 8M) uses real targets instead (`resources.rs` `TILE_TRIANGLE_BUDGET`). Measured: 8000 copies of a 35k-triangle tree render at 150 ms instead of aborting; the 80k-copy terrain keeps 19.8 ms and pays about 140 MB.

### P4 Occlusion culling (on hold)

Depth pyramid and the two-phase draw of D3, camera views only.

### P5 Detail levels for single imports (landed)

Measured on the 943k-triangle coastal cliff scan alone, M4 Max, 1080p: render_scene 15.1 to 5.8 ms filling the frame, 3.0 to 0.75 ms far away. Stills differ only by fine noise spread over the surface, no seams.

As built: `node.gltf_mesh_source` publishes `clusters: Array(u32)` (layout in `primitives/mesh_clusters.rs`) on its loader thread with meshoptimizer's clusterlod, UV seams protected. The scene compiler wires it for an imported mesh drawn once, not as copies, when it is big enough (`worth_clusters`). The patch cull (`cull_patches.wgsl`) then runs one workgroup per cluster from the array's own dispatch words, instead of runs of the index buffer: a cluster draws when its own group's error projects over a pixel and the group it was refined from projects at or under one (D4), each view by its own size, so the camera and each shadow map pick their own cut. The view's index buffer keeps the full mesh's size, so a cut that would overflow it is clamped. A mesh the hierarchy can't be built for is laid out as runs that are always on the cut. RT, picking and unculled passes keep the full mesh.

### P6 Terrain tiles by distance (on hold)

D5's quadtree reads the height map (`node.terrain` gains the height output in the same change). It replaces P1's single grid.

### P7 Cluster ray tracing on the 5080

With the Vulkan ray tracing port: per-patch acceleration structures and a partitioned top level for large changing scenes.

## Findings (2026-10-30)

- The terrain landscape is pixel-bound, not geometry-bound. frame-time splits each render pass into vertex and fragment spans (`Render <label> vertex` / `fragment`). On a low 1080p shot of the d1 terrain the main pass is 49.7 ms fragment against 0.13 ms vertex bare, and 45.0 ms against 1.3 ms with its 80k copies (profiled, so ratios only). Occlusion culling (P4) and terrain tiles (P6) cut vertex work, so neither pays here; the cost is the terrain material's shading.

- Imported parts stay indexed under the per-vertex modifier types (`ModifierTypeEntry::per_vertex`: Bend, Twist, Taper, Rotate, Morph, Boil, Ripple, Melt, Slice, Voxelize, Fold, Wave Shear, Shake, Spatial Echoes, Elastic Sculpture, Math View). Types whose stages read whole triangles or hash the vertex index (Inflate, Displace by Texture, Surface Waves, Facet Normals, Shatter Mesh, Cut Out Box, Glitch Jitter, the fragment types), morphs and fragment cuts still flatten them (`scene_compile/mesh.rs` `use_indexed`).
- In an RT scan scene the ray tracer, not triangle count, was the cost (83 ms AO+GI, 56 ms reflections, 13 ms raster); RT reuse waits for the 5080.
- The b1 terrain's 50 ms main pass is fragment work (the covers' material shading), not geometry; patch culling cannot reach it with the whole terrain in view.
