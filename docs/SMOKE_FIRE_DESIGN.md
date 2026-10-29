# Smoke and Fire

Two scene items, Smoke and Fire, on one gas solver and one volume renderer.
Fire burns fuel, glows, gives off smoke and lights the scene. Smoke shades
itself from the sun. Both are pushed by nothing and push nothing back (one-way).
Off costs nothing: a scene without them builds no gas node and runs no pass.

## Grid

A dense box of cells, on the World's grid: the box snaps to whole World
cells and its cell is the World's Cell Size times the item's Quality scale
(Low 2, Medium 1, High ½, Extra High ¼). The item's Transform is the box;
the source sits at the box's floor centre. The box floor is a wall, the
sides and top are open: what leaves fades over the last cells.

Fields, as 3D textures (hardware trilinear for advection and the march):

| texture | format | channels |
|---|---|---|
| velocity A/B | rgba16float | u v w, unused |
| state A/B | rgba16float | density (soot), temperature, fuel, flame |
| detail A/B | rgba16float | advected texture coordinate, phase-A xyz |
| pressure | buffer f32 | warm-started each frame |
| divergence | buffer f32 | |

## Step (once per frame, dt = frame delta × World Speed, at most 1/20 s)

1. Emit: in the source (a squashed sphere, Source Size across) add density
   or fuel, temperature and an upward inflow speed, modulated by animated
   noise so the inflow breaks up.
2. Advect velocity and state: MacCormack with the min/max clamp.
   Velocity is staggered (a MAC grid, one component on each cell face), so
   divergence, pressure and projection share one compact stencil and no
   cell-to-cell zigzag hides from the solve. On cell-centred velocity the
   corrected advection grew one into horizontal streaks.
3. Burn (Fire): `burn = fuel·(1 − e^(−rate·dt))` where temperature is past
   ignition; fuel −= burn, temperature += burn·heat, density += burn·soot,
   flame = max(flame·e^(−dt/τ), burn·k). Burning also expands: the
   pressure solve takes `burn·expansion` as its divergence target.
4. Forces: buoyancy along −gravity, `(buoyancy·T − weight·density)`;
   vorticity confinement `ε·h·(N×ω)` (Swirl) at cell centres, each face
   taking the mean of its two cells; a curl-noise push at half the source's
   size, at least four cells (Turbulence). The staggered solve keeps its own
   turbulence, so both start low: Swirl 0.3 / 1, Turbulence 0.3 / 1 (Smoke /
   Fire).
5. Project: divergence, red-black Gauss-Seidel on the warm-started
   pressure (iterations per Quality), subtract the gradient. Floor closed,
   other faces open (p = 0).
6. Fade: density and temperature decay. A sponge band over the 15% of the
   box next to each side and the top fades the gas at up to 30 per second,
   so it thins out before a face and the box never shows.

The detail coordinates reset to identity every period P on two phases half
a period apart (advected textures): the renderer blends the two, so
detail moves with the flow and never stretches.

## Render (inside render_scene, after the water and its mist)

One draw per gas item over the box's back faces, so only pixels the box
covers run. Per pixel: march from the box entry to the nearer of the box
exit and the opaque depth, jittered by interleaved gradient noise.

Per sample:

- Density with detail: `d · saturate(1 + A·(n − 0.5)·2)`, `n` a tiling
  3-octave noise at the advected coordinates of both phases.
- Sun: the light volume holds the optical depth toward the sun at each
  cell (marched once per frame, half resolution below High). Multiple
  scattering is the octave sum of Wrenninge (2015):
  `Σ aⁱ·σs·L·e^(−bⁱτ)·phase(cosθ, cⁱg)` with a = b = c = ½, 2 to 4
  octaves. Phase is a forward and a back Henyey-Greenstein lobe.
- Scene shadows: the sun's shadow map at the sample (High and up), so
  solids cast light shafts through the smoke.
- Sky: the irradiance from above, dimmed by the optical depth upward.
- Fire glow on smoke: the flame field's mip at about four cells, times the
  flame colour.
- Flame emission: `flame · blackbody(T)` in physical radiance, so the
  scene's exposure and bloom treat it as light. The blackbody colour is a
  256-entry table from Planck's law against the CIE 1931 observer, made
  once on the CPU.

Fire lights the scene: the gas node reads back, two frames late, the total
flame emission and its centre, and the item adds one point light there,
the flame colour, its power following the flames.

## Quality presets (per item)

| | Low | Medium | High | Extra High |
|---|---|---|---|---|
| cell | 2× World | 1× | ½× | ¼× |
| pressure iterations | 20 | 30 | 50 | 80 |
| march samples per cell | 0.75 | 1 | 1.5 | 2 |
| light volume | ½ res, 16 steps | ½ res, 24 | full, 48 | full, 64 |
| detail octaves | 1 | 2 | 3 | 4 |
| scatter octaves | 2 | 3 | 4 | 4 |
| scene shadows in smoke | off | off | on | on |

Measured on an M4 Max, 1080p, a scene with a sun and sky, whole frame less
the same scene without the gas (1.96 ms). Fire box 1.5 × 3 × 1.5 m, Smoke
2 × 4 × 2 m, World Cell Size 0.0625 m.

| | Low | Medium | High | Extra High |
|---|---|---|---|---|
| Fire cells | 12 × 24 × 12 | 24 × 48 × 24 | 48 × 96 × 48 | 96 × 192 × 96 |
| Fire, ms | 0.7 | 1.3 | 4.8 | 30 |
| Smoke cells | 16 × 32 × 16 | 32 × 64 × 32 | 64 × 128 × 64 | 128 × 256 × 128 |
| Smoke, ms | 0.6 | 1.4 | 5.7 | 47 |

Low and Medium are bound by dispatch count (about 60 small dispatches), not
cells. High is the live default; Extra High is for renders.

## Not in this version

Solids pushing smoke, water putting fire out, fire on a mesh's surface,
smoke casting shadows on the scene.
