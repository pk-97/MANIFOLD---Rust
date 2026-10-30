# Smoke and Fire

Two scene items, Smoke and Fire, on one gas solver and one volume renderer.
Fire burns fuel, glows, gives off smoke and lights the scene. Smoke shades
itself from the sun and sky. Both flow round the World's solids, water and
sand, are pushed by them and by scene forces, and push nothing back. A scene without them builds no gas node and runs no pass.

## Where it lives

| part | file |
|---|---|
| items, rows, defaults | `manifold-core/src/scene_schema/gas.rs` |
| compile (transform, cell size, domain, tick loop, step, object) | `manifold-nodes-scene/src/node_graph/scene_compile/gas.rs` |
| domain `node.gas_domain` (box, clock, solids, forces, hits, light) | `manifold-gas/src/primitives/gas_domain.rs` |
| tick loop `node.gas_ticks` (substep boundary, published fields) | `manifold-gas/src/primitives/gas_ticks.rs` |
| step `node.gas_volume` (one tick) | `manifold-gas/src/primitives/gas_volume.rs`, `shaders/gas_step.wgsl` |
| object inputs and look params | `manifold-nodes-scene/src/node_graph/primitives/scene_object.rs` (`gas_*`) |
| renderer | `render_scene/gas_objects.rs` (collect), `gas_volume.rs` + `shaders/gas_volume.wgsl`, `gas_light.wgsl` (draw) |
| fire light | `render_scene/frame_setup.rs` (`fire_light`) |
| blackbody table | `manifold-node-engine/src/scene/blackbody.rs` |

## Grid

A dense box on the World's lattice: it snaps to whole World cells and its
cell is the World's Cell Size times the item's Quality scale (Low 2,
Medium 1, High ½, Extra High ¼). The item's Transform is the box; the
source sits at its floor centre. The floor is a wall, the sides and top are
open.

The air is the costly part and stays on these cells. Velocity is staggered
(a MAC grid): node (x, y, z), one more than the cells along each axis, holds
u, v and w on the cell's low x, y and z faces. Divergence, pressure and
projection share one compact stencil, so no cell-to-cell zigzag hides from
the solve.

The state (density, temperature, fuel, flame) lives on a fine grid, `FINE`
(2) cells per air cell along each axis, two packed halves per fine cell. The
air carries it and burning happens at its scale, so the flow stretches smoke
into sheets and flames into fronts thinner than an air cell; drawn from the
air's cells alone they showed as blocks and soft blobs. Each tick the fine
state is averaged back onto the air's cells (`cs_restrict`) for buoyancy,
the flames' expansion and the fire light. Three velocity and three fine
state buffers rotate roles each tick; the frame's last tick writes the fine
result and the air's velocity on the fine grid into the tick loop's
published arrays, which the object draws.

## Step (once a tick of the World's clock, as water and sand step)

The gas runs the World's fixed ticks (`SimulationClock`, at most 1/20 s a
tick), so a force or a hit lands on the same step at any frame rate, and
World Speed and Reset reach it as they reach water. Three nodes, as water
and Matter are built: `node.gas_domain` runs once a frame before the World's
tick loop (it follows the lead's clock, lays the forces, hits and body rows
of every tick, reads the light back) and is the clock owner of
`node.gas_ticks`, a substep region whose body is `node.gas_volume`. With a
water or Matter leading the World the gas's region steps in the World's
group (`docs/ONE_WORLD_DESIGN.md`): tick k of the gas runs after tick k of
the Matter and the water. Each tick reads its own scalars from the domain
(`tick_dt`, `tick_forces`, `tick_impulses`, `tick_solid_base`,
`tick_solid_count`, `tick_last`).

0. Solids: the World's Colliders and Bodies, taken as a second water takes
   them (`SharedSolids`): from the water that owns the World, or straight
   from the World when no water does (`WorldSolidsPair`, rows at the end of
   the frame's ticks, carried back to each tick). A still Collider's
   mesh is voxelised through its tree only when it or the box changes; a
   Collider whose pose changed since the last frame (one on a driver) is
   read through its tree each tick instead, so it never forces the rest
   to be laid again; bodies and
   hulls are sampled from their lattices each tick and give the cell their
   velocity. A solid cell holds no gas, its faces move with the solid, and
   the solve leaves it out (the count of open neighbours rides in the low
   bits of the divergence). The source sits on the static solids under its
   centre, so a box floor sunk in the ground emits from the ground's top.
   Water and sand as this tick left them are solids too. The first Water's
   step is read through its own world tiles (`gas_water`: its faces and
   distance at each gas cell, as the Matter's water push reads them): a
   cell the water fills at least half moves with the water. The first
   Sand's grains (`node.matter_stats`' `grain_view`) spread their bulk and
   velocity over the eight cells round each (`cs_matter`), and a cell at
   least half full moves with their mean velocity. So smoke rolls over
   water and a splash or a sand slide shoves it within the tick. With no
   World the water and sand run their own clocks and the gas reads neither.
   The kernels are built twice, with and without solids: smoke with no
   solid, water or sand runs none of their code.
1. Advect the air, then the fine state on its own grid by the same air
   (`cs_fine_*`): MacCormack with the min/max clamp. Air that
   comes in through an open face arrives still, so no wind at a face can
   feed itself (with the edge's own velocity, a crosswind grew over seconds
   and laid the fire flat). A scene wind is the air itself moving: every
   point is carried by the wind on top of the gas's own velocity, so a
   plume keeps its rise and bends over (a drag toward the wind held the
   rise back and laid the smoke flat).
2. Sources (per fine cell, `cs_fine_sources`): emit density or fuel in a
   noisy squashed ellipsoid; burn where hot (fuel to heat and soot, flame);
   decay; a sponge band over 15% of the box next to the sides and top fades
   the gas at up to 30 per second, so it thins out before a face and the box
   never shows. Then the fine state is averaged onto the air's cells.
3. Forces (per face): buoyancy along −gravity from the two cells' mean
   temperature and density, the curl-noise push (Turbulence, at half the
   source's size), the scene's pushes (Radial, Vortex, Uniform) and a
   hit's velocity change, both read from a lattice over the box every four
   cells (`TickForces`, one per tick when the field moves), the source's
   inflow; at most four cells a step. A box too small for a wind lets the
   smoke leave through its side.
4. Vorticity confinement (Swirl) at cell centres, each face taking the mean
   of its two cells.
5. Project: divergence less the flames' expansion, red-black SOR on the
   warm-started pressure (sweeps per Quality), subtract the gradient. Open
   faces are atmosphere (p = 0); the floor is closed.
6. Publish: state and centred velocity as packed halves, and the flames'
   emission sum and centre for the fire light (read back two frames late).

The staggered solve keeps its own turbulence, so Swirl and Turbulence start
at 0 for Smoke and 1 for Fire. Swirl on thick smoke that lingers for many
seconds turns it streaky.

## Render (inside render_scene, after the water and its mist)

The packed fields (on the fine grid) are unpacked into 3D textures, with a
copy of the state at half size (each texel the mean of its eight, written by
the same pass). A light volume at the air's scale or coarser holds the
optical depth toward the sun and straight up, marched through the half-size
copy about one light cell a step, and the flames' glow. One
draw per gas over the box's screen rectangle marches from the box entry to
the nearer of the box exit and the opaque depth, jittered per pixel.

- Sun: multiple scattering as Wrenninge's octave sum (a = b = c = ½),
  dual Henyey-Greenstein phase, normalised so a deep white medium is as
  bright as a white Lambert surface. High and up read the sun's shadow map,
  the cascade that holds each sample.
- Sky: the irradiance map above and below, dimmed by the optical depth up.
- Flames: `flame · blackbody(T)` in the scene's light units. Glow on the
  smoke: the flames' mean over sixteen-cell blocks (`cs_gas_glow`, only
  when Glow is above 0), read as a cubic B-spline so the blocks never show,
  baked into the light volume so the march reads it for free.
- Detail (off by default): advected noise below the cells, two phases.
  At High the simulation has its own detail; Detail costs about 1.5 ms.

Fire lights the scene with one unshadowed point light at the flames'
centre, the flame colour, its power following the flames.

## Quality presets (per item)

| | Low | Medium | High | Extra High |
|---|---|---|---|---|
| air cell | 2× World | 1× | ½× | ¼× |
| fine state cell | 1× World | ½× | ¼× | ⅛× |
| pressure sweep pairs | 20 | 30 | 50 | 80 |
| march samples per fine cell | 0.5 | 0.75 | 1 | 1.5 |
| light volume, per fine cells; steps one a light cell, at most | ¼ res, 32 | ¼ res, 64 | ½, 128 | ½, 256 |
| detail octaves (if on) | 1 | 2 | 3 | 4 |
| scatter octaves | 2 | 3 | 4 | 4 |
| scene shadows in smoke | off | off | on | on |

Cost on an M4 Max at 1080p, GPU ms a frame for Fire (1.5 × 3 × 1.5 m) and
Smoke (2 × 4 × 2 m) together, World Cell Size 0.0625 m, a close shot with
the gas over most of the frame:

| | Medium | High |
|---|---|---|
| simulation, both items | 1.5 | 8.8 |
| unpack, glow and light | 0.1 | 1.0 |
| march | 2.1 | 5.2 |

A wide shot of the same at High marches in 2.7 ms. Whole frame with a head
mesh and a driven Paddle: Medium 7.1 ms, High 17.5 ms. High is the live
default; Extra High is for renders.

## Not built

Smoke pushing bodies back, water putting fire out, fire on a mesh's surface,
smoke casting shadows on the scene, nodes owning 3D textures (the solver
publishes packed arrays and the renderer unpacks them).
