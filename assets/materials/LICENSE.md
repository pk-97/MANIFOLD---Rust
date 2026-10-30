# Starter materials: licence

Every material in this folder but PineTwig (made here, below) comes from
[ambientCG](https://ambientcg.com) or [Poly Haven](https://polyhaven.com)
and is released under the
[Creative Commons CC0 1.0 Universal](https://creativecommons.org/publicdomain/zero/1.0/)
licence. That covers the pictures, the `.mtlx` files and the preview renders.

What CC0 allows, from ambientCG's licence page
(<https://docs.ambientcg.com/license/>) and Poly Haven's
(<https://polyhaven.com/license>):

- Copy, change, share and use the files for any purpose, commercial
  included, without asking.
- Ship the raw files inside an app, as MANIFOLD does here.
- No credit is required. Both ask for it as a courtesy, so we give it
  below.

Nothing here limits how MANIFOLD, its shows or its renders are used.

## What we changed

From each 2K-JPG download we kept the `.mtlx`, the pictures it names and the
preview. We dropped the DirectX normal map, the ambient occlusion picture and
the Blender, USD and Godot files. In each `.mtlx`, the colour picture gains a
`realworldimagesize` (metres one repeat covers). For natural materials it
becomes a `hextiledimage`, so they start with Break Repeats on. The
displacement's `scale` gains a real depth in metres, which parallax carves
the surface by (our estimate of each material's relief). Sizes come
from ambientCG where it lists one. Where it doesn't, we estimated from the
picture (marked *est.* below).

## Credits

Created using the following materials from ambientCG.com, licensed under the
Creative Commons CC0 1.0 Universal License.

| Material | Size (m) | Repeats | Source |
|---|---|---|---|
| Bricks090 | 1.5 | kept | <https://ambientcg.com/a/Bricks090> |
| Tiles141 | 2.0 | kept | <https://ambientcg.com/a/Tiles141> |
| PavingStones128 | 3.5 | kept | <https://ambientcg.com/a/PavingStones128> |
| WoodFloor051 | 1.8 | kept | <https://ambientcg.com/a/WoodFloor051> |
| Wood051 | 0.8 | kept | <https://ambientcg.com/a/Wood051> |
| MetalPlates006 | 2.0 *est.* | kept | <https://ambientcg.com/a/MetalPlates006> |
| DiamondPlate009 | 1.0 *est.* | kept | <https://ambientcg.com/a/DiamondPlate009> |
| Fabric061 | 0.4 | kept | <https://ambientcg.com/a/Fabric061> |
| Rock064 | 2.0 *est.* | kept | <https://ambientcg.com/a/Rock064> |
| Bark014 | 1.2 | kept | <https://ambientcg.com/a/Bark014> |
| Concrete046 | 2.4 | broken | <https://ambientcg.com/a/Concrete046> |
| Ground106 | 2.5 | broken | <https://ambientcg.com/a/Ground106> |
| Grass004 | 1.4 | broken | <https://ambientcg.com/a/Grass004> |
| Gravel023 | 1.5 | broken | <https://ambientcg.com/a/Gravel023> |
| Asphalt033 | 2.5 | broken | <https://ambientcg.com/a/Asphalt033> |
| ScatteredLeaves009 | 1.7 | broken | <https://ambientcg.com/a/ScatteredLeaves009> |
| Moss002 | 1.0 *est.* | broken | <https://ambientcg.com/a/Moss002> |
| Marble012 | 1.5 *est.* | broken | <https://ambientcg.com/a/Marble012> |
| Plaster007 | 2.0 *est.* | broken | <https://ambientcg.com/a/Plaster007> |
| Snow010A | 2.0 *est.* | broken | <https://ambientcg.com/a/Snow010A> |
| Ground097 | 1.0 *est.* | kept | <https://ambientcg.com/a/Ground097> |
| Ground094C | 1.0 *est.* | kept | <https://ambientcg.com/a/Ground094C> |
| Clay006 | 0.2 | broken | <https://ambientcg.com/a/Clay006> |
| Leaf001 | one leaf | cut-out | <https://ambientcg.com/a/Leaf001> |

Leaf001 has an opacity picture, so it is a cut-out: one leaf drawn across
each leaf card's own UVs, never repeated, so its `.mtlx` is left as
downloaded. Give it to a Tree's Leaves as their Material.

## Made here

PineTwig is a fir frond drawn from scratch by `scripts/pine_twig.py`, with
no photograph in it, and released under CC0 1.0 like the rest. It is a
cut-out like Leaf001: give it to a Tree's Leaves for spruces and firs. Far
away, the tree's detail levels stretch it over whole branches and crowns.

The Poly Haven materials are drone scans of ground tens of metres across,
for terrain seen from the air. From each 4K download we kept the colour,
OpenGL normal, roughness and displacement pictures as JPG and Poly Haven's
thumbnail as the preview, and wrote the `.mtlx` in the same shape as the
ambientCG ones: a `hextiledimage` colour picture sized to the scan's own
width, and a displacement depth in metres (our estimate, *est.*).

Created using the following materials from polyhaven.com, licensed under
the Creative Commons CC0 1.0 Universal License.

| Material | Size (m) | Repeats | Depth (m) | Source |
|---|---|---|---|---|
| rocky_terrain_02 | 90 | broken | 1.0 *est.* | <https://polyhaven.com/a/rocky_terrain_02> |
| aerial_rocks_04 | 80 | broken | 2.0 *est.* | <https://polyhaven.com/a/aerial_rocks_04> |
| snow_field_aerial | 80 | broken | 0.5 *est.* | <https://polyhaven.com/a/snow_field_aerial> |
| marble_cliff_05 | 20 | broken | 0.8 *est.* | <https://polyhaven.com/a/marble_cliff_05> |

## Adding more

Only add materials whose source states CC0 (ambientCG, Poly Haven). Add a row
above for each one. Anything under another licence (CC-BY, store assets)
needs its own terms checked and noted here before it ships.
