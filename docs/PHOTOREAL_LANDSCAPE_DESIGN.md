# Photoreal landscapes in raster

Make a Terrain scene with trees, rocks and grass read as a photograph without
ray tracing, the way open-world games do it (Red Dead Redemption 2, Horizon
Forbidden West, Ghost of Tsushima). Ray tracing stays a final polish on top.
Six phases, in order: light, air, land, density, motion, camera.

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
  and the HDRI's sun. Its "physical daylight sky" moves here if that session
  agrees; until it answers, `scene_shading.wgsl`, `frame_setup.rs` and
  `light.rs` stay untouched.
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
  Range goes away for the Sun. Shafts and caustics read the same entries.
- **Contact shadows.** A short march toward the sun through the depth buffer
  for things smaller than a cascade texel: grass, pebbles, leaves on bark.
- **Daylight sky.** A sky made by an atmosphere model (Hillaire 2020:
  small lookup tables for how light passes through and scatters in air),
  driven by the Sun's direction. The sun's colour and the sky's colour and
  strength follow the sun's height on their own: blue noon, orange low sun,
  deep blue dusk. The sky feeds the irradiance map as an HDRI does, rebuilt
  only when the sun moves. Real units: lux and EV.
- **Sky blocking.** When the land changes, the Terrain works out for every
  cell how much of the sky it sees, from the height map in sixteen
  directions, and which way the open sky lies. Sky light and reflections on
  the ground are scaled by it, so valleys and gullies darken. Copies take the
  value at their root, so trees in a valley sit in its shade. Under a forest,
  copies stamp their canopy into a coarse top-down map that darkens the
  ground beneath them.
- **Bounce.** Probes (points that sample the light around them) in a grid
  round the camera, each marching rays across the height map: lit ground
  bounces its own colour into shade, so a sunlit slope warms the slope facing
  it. A few probes update per frame, and none while sun and land hold still.

## Phase 2: Air

Aerial perspective from the same atmosphere model: a low-resolution volume
over the view holds how much light the air adds and how much of the land it
hides at each distance, so far ridges fade to blue-grey and the sun side
goes warm. It replaces `node.atmosphere`'s fixed fog colour under the
Daylight sky; height fog (valley mist) and light shafts stay.

## Phase 3: Land

- **Shape.** Inigo Quilez's eroded-looking noise (each octave weakened where
  the slope already is steep) replaces the value-noise ridges: sharp crests,
  long smooth valleys, no row of identical peaks. Still live and
  performable.
- **Erosion.** Grid erosion (Mei et al. 2007, water and soil moved on every
  cell at once) replaces the raindrops: rivers carve branching valleys at
  every size, and slumping soil (thermal erosion) piles scree under cliffs.
  It writes where water flowed and where soil settled.
- **Masks to materials.** Covers gain Flow and Deposit next to slope and
  height, so gullies go dark and wet, scree gathers under cliffs and grass
  keeps to the flats.
- Close-up detail is the Lumen session's terrain tiles (its P6) plus the
  materials' parallax.

## Phase 4: Density

- **Grass.** Blades made fresh on the GPU every frame in tiles round the
  camera (Ghost of Tsushima): clumped, bent, thinning with distance into
  the ground material, where Covers allow.
- **Trees and rocks by the hundred thousand.** `node.tree` publishes
  `lods` (fewer segments and leaves). Past the last level, a copy draws as a
  stand-in card baked once per variety from many angles (an octahedral
  impostor). Several tree varieties per forest by seed.
- Cost measured on the forest scene at each step, `--release`.

## Phase 5: Motion

One wind for the World. Grass bends with it; trees sway by branch level
(`node.tree` writes each vertex's level and distance along its branch);
gusts can follow the music.

## Phase 6: Camera

New scenes start with AgX, Exposure from the daylight sky, and gentle Bloom.

## Checks

Every step: a still of the forest scene before and after, checked by eye
before it reaches Peter, and a `--release` frame-time on the same scene.
At most one unit test per step, for math only (cascade fit, horizon angle).
