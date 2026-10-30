# Shadows and caustics from every light and the HDRI

Bead BUG-l2h3.30.14. Every light shadows the whole space it lights and casts
caustics through clear surfaces (water, goo, jelly, glass). An HDRI's sun is
a real Sun, with its shadow and caustics, and the HDRI's lighting no longer
counts it a second time.

## What the code does now

- Light kinds are Sun, Spot and Point. Each light that casts shadows takes one
  layer of the shadow map array (`frame_setup.rs`, `resources.rs`), drawn
  through `Light::shadow_view_proj` (`manifold-node-engine/src/scene/light.rs`).
- A Point light's shadow is one 90° face toward its aim. `shadow_factor` in
  `scene_shading.wgsl` reads anything outside that face as lit, so a point
  light casts no shadow over three quarters of its space.
- Caustics (`render_scene/caustics.rs`, `caustics.wgsl`) run from the first
  casting Sun only: one GPU-narrowed fit, one map, one `CausticFit`, read by
  `caustic_light` for that one caster slot.
- HDRI light is image-based lighting only. `render_scene` convolves the
  environment once per content version (`prepasses.rs`). Nothing shadows it
  except screen-space AO, or traced GI rays when ray tracing is on.
  "Aim Sun at HDRI" (`hdri_sun.rs`, `scene_edit.rs`) aims the first Sun once;
  the sun in the HDRI still lights the scene through the IBL as well.

## Phase 1: Point lights shadow in every direction

- A casting Point light takes six consecutive caster entries and layers, one
  per cube face, each a 90° perspective view. Its caster slot is the first.
- `shadow_factor` picks the face by the largest axis of
  `world_pos - light_pos`, then reads that face like any perspective caster.
- The per-caster dirty hash already skips unchanged layers, so a still light
  over a still scene redraws nothing. A face with no geometry in its frustum
  draws nothing.
- Ray-traced shadows are unchanged: rays already go in every direction.
- Check: a still of a point light in a room with objects on every side. One
  unit test covers face selection.

## Phase 2: Caustics from every casting light

- `caustics.rs` runs per casting light (each Point face counts as one) whose
  view holds a clear surface. Lights with none in view cost nothing.
- Each light gets its own fit, narrowed on the GPU in that light's clip space
  as today, and its own layer in a caustic map array. The fit buffer becomes
  one `CausticFit` per caster slot; `caustic_light` reads its slot's entry.
- Spot and Point photons leave from the light's position, one direction per
  texel; Sun photons stay parallel. The under-the-surface bias becomes a
  distance along the light's ray, so it works in perspective depth.
- Map size per light: its shadow resolution, capped at 1024 as now.
- Cost: measured per light with `render-import --passes` on a pool scene
  before and after, reported with the phase.

## Phase 3: the Sun is the HDRI's sun

- In HDRI mode, the IBL prepass also finds the HDRI's sun on the GPU when the
  environment changes: the bright region around the peak, its direction
  (centre of mass), angular size and total light. A one-off readback per
  environment change, never per frame, hands that to the Sun.
- The first Sun, already wired to the environment, takes that direction and
  colour. The HDRI's rotation turns it live. Its Intensity multiplies the
  measured strength, 1 by default, which fixes HDRIs that clip the sun. Its
  angular size sets the shadow's soft edge.
- The IBL convolution reads the environment with the sun region filled from
  the sky around it, so the sun lights the scene once, through the Sun, with
  its shadow, caustics and highlight. The visible sky keeps the real sun.
- "Aim Sun at HDRI" is deleted; the Sun follows the HDRI instead.
- An HDRI with no clear sun (overcast) measures a weak Sun, close to off.

## Not in this design

- Shadows from the rest of the sky stay with AO, or with traced GI.
- Area lights; a Fire's own point lights stay unshadowed.

## Order and ownership

Phases land in order, each its own commit with stills. Phase 1 fixes point
shadows on its own. `render_scene.wgsl` and `scene_shading.wgsl` are shared
with the terrain and Lumen sessions; message them before editing, and before
any edit to `frame_setup.rs`. When phase 1 lands, fix the stale "at most 4
shadow lights" lines in `REALTIME_3D_DESIGN.md` and
`SCENE_GEOMETRY_SCALE_DESIGN.md`.
