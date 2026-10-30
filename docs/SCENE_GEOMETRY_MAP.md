# Scene geometry at any scale

<!-- index: How render_scene stops paying for geometry and pixels nobody sees: indexed terrain, culled copies in shadows, GPU patch culling, detail levels for copies and single imports, cut-out depth draws and far parallax on the plain pipeline. -->

A scene of any size, and a hero shot up close, holds frame rate without anyone decimating a mesh. Hidden, off-screen and far geometry costs no vertex work, detail levels change pixels by under one pixel of error, and work nobody can see costs no shading.

Unbuilt next steps (occlusion culling, terrain tiles by distance, cluster ray tracing) are in `IDEAS.md`.

## Indexed terrain

`node.terrain` writes its grid with finite-difference normals (`shaders/terrain.wgsl`), and the pure `node.grid_indices` writes its indices in 8 x 8 quad tiles, wired by `scene_compile/object.rs` `emit_terrain`. Each grid point is shaded once. The terrain declares topology Fixed, so a morph refits the acceleration structure, and a settled terrain marks its outputs unchanged, so it causes no refit and keeps the shadow cache. Measured on k6b: frame GPU about 22 to 15 ms.

## Patches and GPU culling

`render_scene/patch_cull.rs` and `shaders/cull_patches.wgsl`. A patch is a run of 128 consecutive triangles of an indexed mesh, or a cluster of its detail hierarchy. Imports order their triangles so each run is compact (`patch_order`). Single opaque indexed meshes of 8 patches or more take this path. Each patch's box and normal cone are worked out on the GPU once per mesh content. Then each view culls by frustum, and the camera also by back-face cone; the survivors' indices go into that view's own index buffer, drawn indexed indirect. Each view runs its own cull: the camera, and each shadow caster against its own volume. The cull costs about 0.02 ms on a 2.4 km terrain.

Instanced copies use `shaders/cull_instances.wgsl` instead, for the camera and every shadow view, above a copies x triangles work threshold.

## Detail levels

- **Copies:** a mesh source may publish `lods` (`primitives/mesh_lods.rs`), built with meshoptimizer on the loader thread. The copy cull picks each copy's coarsest level whose error projects to at most a pixel and lays copies out level by level, one indirect draw per level.
- **Single imports:** `node.gltf_mesh_source` publishes `clusters` (`primitives/mesh_clusters.rs`, meshoptimizer clusterlod, UV seams protected) for a big mesh drawn once. The patch cull runs one workgroup per cluster: a cluster draws when its own group's error projects over a pixel and the group it was refined from projects at or under one. That cut is crack-free and each view picks its own. Measured on a 943k-triangle cliff scan: 15.1 to 5.8 ms filling the frame, 3.0 to 0.75 ms far away.
- RT, picking and unculled passes keep the full mesh, so traced shadows agree with the drawn surface.

## Pixels nobody sees

- **Cut-outs:** a fragment shader that can discard stops Apple's hidden-surface removal, so stacked leaf cards were each fully shaded. An alpha cut-out drawn filled without parallax now draws twice in Pass A. First comes a depth draw (`shaders/cutout_depth.wgsl`), with the opaque group, that runs the alpha, fade and volume tests. Then comes its shading through the solid-surface pipeline in a depth-Equal group, so each covered sample is shaded once. `DrawPass::later_groups` carries that group and the blend group. Measured: 50k spruces at 2.3 km, render_scene fragment 196 to 123 ms.
- **Far parallax:** the parallax pipeline's registers slow every pixel even where its trace returns at once. The camera's cull of a parallax surface sorts out the patches whose carve spans under 0.75 px square to the view. They go into a far list filled from the end of the same index buffer, and Pass A draws that list through the plain pipeline (`relief_unseen` in `cull_patches.wgsl` explains the bound). Every other pass draws both lists. Measured: bare terrain at 2.3 km 55 to 30 ms, with 50k spruces 116 to 98 ms.

## Tile budget

The scene pass draws into memoryless MSAA targets, which can't spill when the tiler's parameter buffer fills; the GPU fault ends the session. A frame that can submit more than 16M triangles (falling back below 8M) uses real targets (`resources.rs` `TILE_TRIANGLE_BUDGET`).

## Where the time goes

`frame-time` splits each render pass into `Render <label> vertex` and `fragment` spans. Big landscapes on the Mac are pixel-bound: on a low 1080p terrain shot the main pass is about 50 ms fragment against 0.1 to 1.3 ms vertex. The cost is the materials' shading, which is why occlusion culling and terrain tiles wait in `IDEAS.md`. A 50k-spruce forest is still about 100 ms of pixel work at 1080p, mostly lighting tree pixels; cheaper far trees would build on the tree detail levels (`PHOTOREAL_LANDSCAPE_DESIGN.md`).

Imported parts stay indexed under the per-vertex modifier types (`ModifierTypeEntry::per_vertex`). Types that read whole triangles or hash the vertex index, morphs and fragment cuts flatten them (`scene_compile/mesh.rs` `use_indexed`).
