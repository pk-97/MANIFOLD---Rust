# Smoke and Fire

Two scene items, Smoke and Fire, on one gas solver and one volume renderer.
Fire burns fuel, glows, gives off smoke and lights the scene. Smoke shades
itself from the sun and sky. Both are pushed by nothing and push nothing
back. A scene without them builds no gas node and runs no pass.

## Where it lives

| part | file |
|---|---|
| items, rows, defaults | `manifold-core/src/scene_schema/gas.rs` |
| compile (transform, cell size, gas node, object) | `manifold-nodes-scene/src/node_graph/scene_compile/gas.rs` |
| solver node `node.gas_volume` | `manifold-gas/src/primitives/gas_volume.rs`, `shaders/gas_step.wgsl` |
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

State (density, temperature, fuel, flame) is one vec4 per cell. Velocity is
staggered (a MAC grid): node (x, y, z), one more than the cells along each
axis, holds u, v and w on the cell's low x, y and z faces. Divergence,
pressure and projection share one compact stencil, so no cell-to-cell
zigzag hides from the solve. Three velocity and three state buffers rotate
roles each frame.

## Step (once a tick of the World's clock, as water and sand step)

The gas runs the World's fixed ticks (`SimulationClock`, at most 1/20 s a
tick), so a force or a hit lands on the same step at any frame rate, and
World Speed and Reset reach it as they reach water.

0. Solids: the World's Colliders and Bodies, taken as a second water takes
   them (`SharedSolids`): from the water that owns the World, or straight
   from the World when no water does (`WorldSolidsPair`, rows at the end of
   the frame's ticks, carried back to each tick). A Collider's mesh is
   voxelised through its tree only when it or the box changes; bodies and
   hulls are sampled from their lattices each tick and give the cell their
   velocity. A solid cell holds no gas, its faces move with the solid, and
   the solve leaves it out (the count of open neighbours rides in the low
   bits of the divergence). The source sits on the static solids under its
   centre, so a box floor sunk in the ground emits from the ground's top.
   The kernels are built twice, with and without solids: smoke with no
   solid runs none of their code.
1. Advect velocity and state: MacCormack with the min/max clamp. Air that
   comes in through an open face arrives still, so no wind at a face can
   feed itself (with the edge's own velocity, a crosswind grew over seconds
   and laid the fire flat). A scene wind is the air itself moving: every
   point is carried by the wind on top of the gas's own velocity, so a
   plume keeps its rise and bends over (a drag toward the wind held the
   rise back and laid the smoke flat).
2. Sources (per cell): emit density or fuel in a noisy squashed ellipsoid;
   burn where hot (fuel to heat and soot, flame); decay; a sponge band over
   15% of the box next to the sides and top fades the gas at up to 30 per
   second, so it thins out before a face and the box never shows.
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

The packed fields are unpacked into 3D textures (state with mips), and a
light volume holds the optical depth toward the sun and straight up. One
draw per gas over the box's screen rectangle marches from the box entry to
the nearer of the box exit and the opaque depth, jittered per pixel.

- Sun: multiple scattering as Wrenninge's octave sum (a = b = c = ½),
  dual Henyey-Greenstein phase, normalised so a deep white medium is as
  bright as a white Lambert surface. High and up read the sun's shadow map.
- Sky: the irradiance map above and below, dimmed by the optical depth up.
- Flames: `flame · blackbody(T)` in the scene's light units; glow on the
  smoke from the flame field's mip 3.
- Detail (off by default): advected noise below the cells, two phases.
  At High the simulation has its own detail; Detail costs about 1.5 ms.

Fire lights the scene with one unshadowed point light at the flames'
centre, the flame colour, its power following the flames.

## Quality presets (per item)

| | Low | Medium | High | Extra High |
|---|---|---|---|---|
| cell | 2× World | 1× | ½× | ¼× |
| pressure sweep pairs | 20 | 30 | 50 | 80 |
| march samples per cell | 0.75 | 1 | 1.5 | 2 |
| light volume | ½ res, 16 steps | ½ res, 24 | full, 48 | full, 64 |
| detail octaves (if on) | 1 | 2 | 3 | 4 |
| scatter octaves | 2 | 3 | 4 | 4 |
| scene shadows in smoke | off | off | on | on |

Cost on an M4 Max at 1080p, whole frame less the same scene without the
gas (2.0 ms), World Cell Size 0.0625 m, Fire box 1.5 × 3 × 1.5 m, Smoke 2 × 4 × 2 m:

| | Low | Medium | High | Extra High |
|---|---|---|---|---|
| Fire cells | 12 × 24 × 12 | 24 × 48 × 24 | 48 × 96 × 48 | 96 × 192 × 96 |
| Fire, ms | 0.5 | 0.9 | 3.2 | 20 |
| Smoke cells | 16 × 32 × 16 | 32 × 64 × 32 | 64 × 128 × 64 | 128 × 256 × 128 |
| Smoke, ms | 0.4 | 1.0 | 4.9 | 42 |

Low and Medium are bound by dispatch count (about 60 small dispatches),
not cells. High is the live default; Extra High is for renders.

## Not built

Smoke pushing bodies back, water putting fire out, fire on a mesh's surface,
smoke casting shadows on the scene, nodes owning 3D textures (the solver
publishes packed arrays and the renderer unpacks them).
