# Photoreal landscapes in raster

Make a Terrain scene with trees, rocks and grass read as a photograph without
ray tracing, the way open-world games do it (Red Dead Redemption 2, Horizon
Forbidden West, Ghost of Tsushima). Ray tracing stays a final polish on top.
Six phases, in order: light, air, land, density, motion, camera. Every
step upgrades a system that already exists; nothing here runs beside one.

## What exists (read 2026-10-30)

- Sun shadow: four cascades round the camera's view out to 2 km
  (`Light::cascades`, `manifold-node-engine/src/scene/light.rs`), each a
  caster entry like a cube face; `caster_face` picks one per point. Soft
  edges come from PCSS (`frame_setup.rs`).
- Sky light: the environment is convolved to an irradiance map once per
  change (`prepasses.rs`) and multiplied by material AO, the parallax crevice
  term and screen-space AO (`node.ssao_gtao`, reach about a metre). Nothing
  stops sky light from reaching the floor of a valley or a forest.
- Environments: Softbox and Gradient skies, HDRI, Sea Horizon
  (`EnvironmentSource`, `scene_doc.rs`). No atmosphere model. New scenes
  start with the Sun at 6 against Softbox strips at 1.5 (04c399e23).
- Fog: `node.atmosphere`, exponential fog with height falloff and light
  shafts (`shaft_march.wgsl`). One colour, so distance never turns blue.
- Camera: Exposure in EV, tone curves including AgX (`node.tone_map`),
  Bloom as an effect, depth of field and motion blur.
- Terrain: value-noise ridges on a 512² grid, worn by raindrops
  (`terrain.wgsl`). Covers pick materials by slope, height and noise
  (`cover_rule.wgsl`). The main pass on a covered terrain is fragment-bound
  (SCENE_GEOMETRY_SCALE_DESIGN.md findings).
- Plants: `node.tree` grows bark and crossed leaf cards, with a Cone row
  for conifers, crown-bent normals and crown shade in its vertex colour,
  a box for culling, and four detail levels (one card a leaf, cards per
  branch, three crown cards, nothing). Copies On Surface scatter by the
  same rules as Covers; on a Terrain each copy takes a hashed place on the
  map, so it rides the ground as it wears. Reach (metres) fills a window of
  cells round the camera instead, full to half the Reach and thinning to
  none at it; each cell keeps its slot as the camera moves, so nothing
  swims. No wind on plants.
- Cost (50k spruces, 1080p, stills profile): cut-out cards are shaded at
  every layer, since `discard` defeats hidden-surface removal; the Lumen
  session is adding a depth pass for cut-out draws. The terrain's Depth
  (parallax) pipeline costs about 20 of its 34 ms; far patches move to the
  plain pipeline (Lumen session).

## Ownership

- `SCENE_GEOMETRY_SCALE_DESIGN.md` (the Lumen session) keeps patches,
  culling, mesh detail levels and terrain tiles by distance (its P6). This
  plan feeds those: trees publish `lods`, the land shape publishes the height
  map the tiles read.

## Phase 1: Light

- **Sun cascades: open.** Blend neighbouring cascades, and retire the
  Sun's Range row, which cascades ignore.
- **Contact shadows.** A short march toward the sun through the depth buffer
  for things smaller than a cascade texel: grass, pebbles, leaves on bark.
- **Daylight sky.** A third Sky style beside Softbox and Gradient, made by
  an atmosphere model (Hillaire 2020: small lookup tables for how light
  passes through and scatters in air), driven by the Sun's direction. The sun's colour and the sky's colour and
  strength follow the sun's height on their own: blue noon, orange low sun,
  deep blue dusk. It feeds the existing irradiance prepass as an HDRI does,
  rebuilt only when the sun moves. Real units: lux and EV.
- **Sky blocking and bounce.** `node.terrain`, when its land or the sun
  changes, marches its own height map from every cell in sixteen directions:
  how much sky the cell sees, and how lit the slopes that block the rest
  are. Both ride on its vertices. Shading folds the first into the existing
  occlusion term, so valleys and gullies darken, and adds the second as
  sunlight bounced off the facing slope. Copies On Surface carry the value
  at their root, so trees in a valley sit in its shade. No probes, no new
  pass.

## Phase 2: Air

`node.atmosphere` stays the one fog and gains aerial perspective from the
daylight model: a low-resolution volume over the view holds how much light
the air adds and how much of the land it hides at each distance, so far
ridges fade to blue-grey and the sun side goes warm. Under the Daylight sky
it replaces the fixed fog colour; height fog (valley mist) and light shafts
stay.

## Phase 3: Land

All inside `node.terrain` and the shared `CoverRule`.

- **Shape.** The ridged octaves, whose lattice lines up the far peaks like
  teeth, give way to gully noise (stripes running down the slope under
  them, each octave following the slope the last one left, so gullies
  branch; Clay John's eroded terrain noise, 2020). Broad hills and the
  existing detail octaves stay. Still live and performable.
- **Rain.** The existing raindrops keep carving the fine channels, and now
  also record where water ran. Soil they laid down is already in the
  erosion buffer.
- **Masks to materials.** The vertices carry Flow and Deposit in their
  second UV set. `CoverRule` gains them beside slope and height, so Covers
  and Copies On Surface both read them: gullies go dark and wet, scree
  gathers under cliffs, grass keeps to the flats.
- Close-up detail is the Lumen session's terrain tiles (its P6) plus the
  materials' parallax.

## Phase 4: Density

- **Copies On Surface by distance.** Built as Reach on a Terrain (see What
  exists). Open: past 65,536 copies a layer, which the copies body caps.
- **Trees and rocks by the hundred thousand.** `node.tree` publishes
  `lods` (fewer segments and leaves) in the Lumen session's layout; its
  last level, a stand-in card baked per variety from many angles (an
  octahedral impostor), is that session's call. Several tree varieties per
  forest by seed.
- Cost measured on the forest scene at each step, `--release`.

## Phase 5: Motion

The existing Wind force drives plants too. Grass bends with it; trees sway
by branch level (`node.tree` writes each vertex's level and distance along
its branch); its gusts can follow the music.

## Phase 6: Camera

Defaults only, on the existing tone curve, Exposure and Bloom: new scenes
start with AgX, Exposure from the daylight sky, and gentle Bloom.

## Checks

Every step: a still of the forest scene before and after, checked by eye
before it reaches Peter, and a `--release` frame-time on the same scene.
At most one unit test per step, for math only (cascade fit, horizon angle).
