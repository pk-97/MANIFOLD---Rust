# Photoreal landscapes in raster

Make a Terrain scene with trees, rocks and grass read as a photograph without
ray tracing, the way open-world games do it (Red Dead Redemption 2, Horizon
Forbidden West, Ghost of Tsushima). Ray tracing stays a final polish on top.
Six phases, in order: light, air, land, density, motion, camera. Every
step upgrades a system that already exists; nothing here runs beside one.

## What exists (read 2026-10-30)

- Sun shadow: one orthographic map per casting light, a box of half-size
  Range round its aim (`Light::shadow_proj`, `manifold-node-engine/src/scene/light.rs`),
  default 30 m, at most 200 m, at most 4096 texels. Soft edges come from PCSS
  (`frame_setup.rs`). On a 2 km landscape almost nothing casts a shadow.
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
- Plants: `node.tree` grows bark and crossed leaf cards; Copies On Surface
  scatter by the same rules as Covers. Copies can draw detail levels from a
  `lods` array (`primitives/mesh_lods.rs`). No grass blades, no far-away
  stand-in cards, no wind on plants.

## Ownership

- `LIGHT_TRANSPORT_DESIGN.md` (the light session) keeps caustics per light
  and the HDRI's sun (Follow HDRI, HDRI mode only). Its "physical daylight
  sky" moves here, replacing the hand-tuned Sun 6 and strips 1.5. The
  cascades land on top of its caustics phase, once that is committed.
- `SCENE_GEOMETRY_SCALE_DESIGN.md` (the Lumen session) keeps patches,
  culling, mesh detail levels and terrain tiles by distance (its P6). This
  plan feeds those: trees publish `lods`, the land shape publishes the height
  map the tiles read.

## Phase 1: Light

- **Sun cascades.** The Sun's one box becomes four shadow maps that follow
  the camera, each covering a slice of the view: sharp for the first metres,
  wide out to a Shadow Distance (default 2 km). Built the way Point lights
  take six cube faces: one caster entry per cascade (`Light::cascades` beside
  `Light::cube_faces`). Each cascade is a sphere snapped to its own texels so
  shadows don't shimmer as the camera turns, and neighbouring cascades blend.
  Range goes away for the Sun. Cascades get their own caster kind with real
  colour (caustics skip black entries; only the first carries rays, as with
  cube faces), and shading picks one through `caster_face`, so shafts and
  caustics follow with no extra work. Only the nearest one or two cascades
  run caustics, since each costs a view.
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

- **Copies On Surface by distance.** Copies thin with distance from the
  camera, placed by a stable hash per triangle so they never swim, which
  allows millions near the camera: grass clumps, pebbles, ferns, by the same
  grow rules.
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
