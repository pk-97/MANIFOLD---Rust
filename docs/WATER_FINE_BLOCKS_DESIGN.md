# Water fine blocks: a finer surface where the water is thin

**Status:** design, not started. Needs Peter's go-ahead: about two to three days of work.

## Why

The display field is one lattice at Surface Detail's cell, one node per sim cell by default. A sheet of water one or two particles thick is thinner than that cell, so the splat can either draw it with blobs big enough to touch (thick, gummy sheets) or small enough to be thin (sheets full of holes). Stretched (Yu and Turk anisotropic) blobs on that lattice fail for the same reason: a blob thinner than one cell falls between the lattice nodes. Surface Detail 1 everywhere fixes the sampling but costs 12.7 ms on the dam. Thin water is a small part of most frames, so only it should pay.

## What

A second display table at half the cell, holding only the tiles where the water is thin, with its own field build. The renderer reads the fine field wherever a fine tile is held and the coarse one elsewhere.

1. **Thin particles.** A per-particle pass on the coarse smoothed field: a liquid particle is thin when the field 1.5 coarse cells inward along its normal is air (the sheeter's depth test on the display field). Output: the particles with every non-thin one's radius 0, so every existing particle node can read it.
2. **Fine table.** A second `node.surface_tiles` at half the display cell, built from the thin particles only, with its usual one-tile ring. Its tiles cover thin water and a margin.
3. **Fine field.** `node.particle_volume` over all liquid particles into the fine table (a particle outside it writes nothing), then Close Gaps and the smoothing, as the coarse chain. Stretched blobs go here, once the plain fine splat is measured: the neighbour pass that gives each particle its stretch reads the display particles sorted by fine tile.
4. **Render.** `FieldSurface` gains an optional fine field (levelset, table, pool tiles, cell). The march and the cubic hit sample the fine field inside a held fine tile and step at its cell; the normal comes from the field the hit is on.
5. **Seam.** Where fine meets coarse the two zero sets differ by up to half a coarse cell. The fine table's outer ring blends: a fine node in a tile with an unheld neighbour takes `mix(fine, coarse, w)`, `w` rising to 1 across that ring, written once by the fine chain's last pass, so the march reads one value with no seam test.
6. **Card.** A Thin Water Detail switch under Surface, on by default if the measurement allows.

## Cost to measure

Dam, waterTestV2 and open ground, 1080p frame clock, against today: the fine chain scales with thin area, not water volume. Off costs nothing (no table, no passes). Memory: one fine table and three fine fields, capped by the display pool budget.

## Risks

- The seam blend hides a step but not a mismatch in topology: a sheet the fine field holds and the coarse one breaks shows its holes at the ring. The thin flag must reach one tile past the sheet for this to stay off screen.
- Thin water that tears into drops leaves fine tiles that must go when the drops land; the table rebuilds every frame, so it does.
- The march's tile flags (`TILE_AIR`/`TILE_WATER`) need a fine copy, or the march loses its empty-tile skip inside fine regions.
