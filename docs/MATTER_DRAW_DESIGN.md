# Matter draws: each material its own way

<!-- index: How each Matter material (sand, snow, clay, mud, goo, jelly) is drawn: the pieces they share (material coordinates, loose-or-bulk, scanned texture sets, the particle surface) and what each needs alone. Replaces drawing every material through the water's surface build. -->

**Status:** design, Peter's go 30 Oct ("more than one mesher, optimise per material"). Not built.

## Why

Every Matter material draws through the water's surface build (sort, stretched blobs, splat, close gaps, three smoothing passes, marching cubes on display tiles), as raw triangles, at Surface Detail 1 for all but sand. On a sand-only scene that build is about 28 of a 68 ms frame (`particle_volume` 20 ms, `shape_particle_blobs` 4, `tile_surface_mesh` 2), and it rounds every material into blobby water. It was made for clear water: thin sheets, glass-smooth smoothing.

## What each material is

| Material | Rule | Over time | What you see | Draw |
|---|---|---|---|---|
| Sand | granular | flows, piles at its angle, sprays | dry, matte; streams and spray show single grains; piles read as textured ground | bulk surface, grains where loose, grain texture pinned to the material |
| Snow | snow (packs, hardens) | breaks into clumps and crumbs, packs where pressed | white, glinting, light glows inside; broken faces crisp | bulk surface, rounded crumbs where loose, glints; packed snow smoother |
| Clay | viscoplastic, firm | holds its shape, folds, creases, tears slowly | satin, finger smears and folds | particle surface at the sim cell, scanned clay set |
| Mud | viscoplastic, soft | slumps, splats, merges | dark, wet sheen, lumpy | particle surface at the sim cell, droplets where loose |
| Goo | viscoplastic, runny, clear | flows, coils, strings, merges | glassy amber, threads thinner than a cell | particle surface at the sim cell, stretched blobs keep threads |
| Jelly | elastic | wobbles, bounces, never tears | clear, keeps its shape | particle surface at the sim cell, smoothed glassy |

Every material draws from the particle surface, since forces (a swarm, an audio hit) can tear any of them apart: a mesh carried by the points was tried for jelly and clay and stretched into spikes wherever they tore. Sand and snow are grains: the bulk is a surface, the loose part is pieces.

## Shared pieces

1. **Material coordinates.** Each point's fill position, in a side buffer `node.matter_fill` writes (the point record has no room). The surface carries them onto its vertices, so a texture, grain or tool mark moves with the material instead of swimming through it. Sand, snow, clay and mud use it.
2. **Loose or bulk.** A point with few neighbours is loose. `node.matter_grains` already counts neighbours in an occupancy table. Loose points draw as pieces (sand grains, snow crumbs, mud droplets), bulk points as the surface.
3. **Scanned texture sets** (`scene_schema::matter_set`): sand, snow, clay and mud start on a starter material (colour, roughness, normal and height pictures), box mapped at its real size and carved by parallax. Their colour and roughness come from the pictures.
4. **The particle surface** for goo, mud and the bulk of sand and snow, with each material's own settings: the sim's cell (Detail 0), no water-only steps, and smooth normals from the field rather than the triangles' facets.
5. **Detail in the shader, not the mesh.** Per-pixel normals from a texture in material coordinates: sand grain, snow crust, clay marks, mud lumps.

## What each needs alone

- **Sand:** tiny grains only where loose; sparkle; the wet look (built).
- **Snow:** crumbs as rounded chunks of mixed sizes; glints; packed snow (high hardening) smoother and greyer than powder. Powder puffs on impact go through the gas later.
- **Clay:** finger smears from its scanned set; loose points as clumps.
- **Mud:** wet sheen (clear coat, built); droplets where loose.
- **Goo:** threads (stretched blobs); clear with caustics (built).
- **Jelly:** clear with refraction (built).

## Build order

One batch each, measured on the same scenes before and after.

1. Material coordinates and loose-or-bulk as shared outputs.
2. Sand and snow: bulk surface at Detail 0 plus pieces where loose; snow crumbs.
3. Mud, goo, clay and jelly at the sim cell with their own settings.
