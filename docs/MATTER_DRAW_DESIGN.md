# Matter draws: each material its own way

<!-- index: How each Matter material (sand, snow, clay, mud, goo, jelly) is drawn: the pieces they share (material coordinates, loose-or-bulk, a mesh carried by the points, the particle surface) and what each needs alone. Replaces drawing every material through the water's surface build. -->

**Status:** design, Peter's go 30 Oct ("more than one mesher, optimise per material"). Not built.

## Why

Every Matter material draws through the water's surface build (sort, stretched blobs, splat, close gaps, three smoothing passes, marching cubes on display tiles), as raw triangles, at Surface Detail 1 for all but sand. On a sand-only scene that build is about 28 of a 68 ms frame (`particle_volume` 20 ms, `shape_particle_blobs` 4, `tile_surface_mesh` 2), and it rounds every material into blobby water. It was made for clear water: thin sheets, glass-smooth smoothing.

## What each material is

| Material | Rule | Over time | What you see | Draw |
|---|---|---|---|---|
| Sand | granular | flows, piles at its angle, sprays | dry, matte; streams and spray show single grains; piles read as textured ground | bulk surface, grains where loose, grain texture pinned to the material |
| Snow | snow (packs, hardens) | breaks into clumps and crumbs, packs where pressed | white, glinting, light glows inside; broken faces crisp | bulk surface, rounded crumbs where loose, glints; packed snow smoother |
| Clay | viscoplastic, firm | holds its shape, folds, creases, tears slowly | satin, sharp creases, tool marks | its fill's own mesh carried by the points |
| Mud | viscoplastic, soft | slumps, splats, merges | dark, wet sheen, lumpy | particle surface at the sim cell, droplets where loose |
| Goo | viscoplastic, runny, clear | flows, coils, strings, merges | glassy amber, threads thinner than a cell | particle surface at the sim cell, stretched blobs keep threads |
| Jelly | elastic | wobbles, bounces, never tears | clear, sharp edges, keeps its shape | its fill's own mesh carried by the points |

Topology decides the draw. Jelly never changes topology and clay rarely does, so a real mesh carried by the material is exact for them. Goo and mud split and merge, so their surface has to come from the particles. Sand and snow are grains: the bulk is a surface, the loose part is pieces.

## Shared pieces

1. **Material coordinates.** Each point's fill position, in a side buffer `node.matter_fill` writes (the point record has no room). The surface carries them onto its vertices, so a texture, grain or tool mark moves with the material instead of swimming through it. Sand, snow, clay and mud use it.
2. **Loose or bulk.** A point with few neighbours is loose. `node.matter_grains` already counts neighbours in an occupancy table. Loose points draw as pieces (sand grains, snow crumbs, mud droplets), bulk points as the surface.
3. **A mesh carried by the points** (`node.matter_skin`). Bound once per fill, since a point keeps its index from fill to Reset: each vertex takes its nearest points of its own item's run and its offset from each at rest. Each frame a vertex is the weighted mean of `point + F · offset` over them, from the display frame's blended positions, and its normal turns by the blended F's cofactor. Its UVs and textures are the fill model's. It writes previous positions for motion blur. Jelly and clay.
4. **The particle surface** for goo, mud and the bulk of sand and snow, with each material's own settings: the sim's cell (Detail 0), no water-only steps, and smooth normals from the field rather than the triangles' facets.
5. **Detail in the shader, not the mesh.** Per-pixel normals from a texture in material coordinates: sand grain, snow crust, clay marks, mud lumps.

## What each needs alone

- **Sand:** tiny grains only where loose; sparkle; the wet look (built).
- **Snow:** crumbs as rounded chunks of mixed sizes; glints; packed snow (high hardening) smoother and greyer than powder. Powder puffs on impact go through the gas later.
- **Clay:** creases and marks stay sharp on the carried mesh. Where it tears, triangles stretch: hide those past a stretch limit, and let loose points there draw as clumps.
- **Mud:** wet sheen (clear coat, built); droplets where loose.
- **Goo:** threads (stretched blobs); clear with caustics (built).
- **Jelly:** clear with refraction (built); the carried mesh keeps its edges sharp.

## Build order

One batch each, measured on the same scenes before and after.

1. `node.matter_skin` for jelly and clay. The biggest change to the look, and it removes the surface build for them.
2. Material coordinates and loose-or-bulk as shared outputs.
3. Sand and snow: bulk surface at Detail 0 plus pieces where loose; snow crumbs.
4. Mud and goo at the sim cell with their own settings.
