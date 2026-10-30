# Shadows and caustics from every light and the HDRI

Bead BUG-l2h3.30.14. Every light shadows the whole space it lights and casts
caustics through clear surfaces (water, goo, jelly, glass). An HDRI's sun is
a real Sun, with its shadow and caustics, and the HDRI's lighting no longer
counts it a second time.

## What the code does now

- Light kinds are Sun, Spot and Point. Each light that casts shadows takes one
  layer of the shadow map array (`frame_setup.rs`, `resources.rs`), drawn
  through `Light::shadow_view_proj` (`manifold-node-engine/src/scene/light.rs`).
- A casting Point light shadows through six cube faces, six caster entries
  (`Light::cube_faces`); `caster_face` in `scene_shading.wgsl` and
  `shaft_march.wgsl` picks the face. Only the first entry carries rays
  (`ShadowCaster::CUBE`), so ray-traced shadows trace the light once.
- Caustics (`render_scene/caustics.rs`, `caustics.wgsl`) run once per lit
  light whose view may hold a clear surface: a clear mesh's box is tested on
  the CPU against the view, a water's reach only on the GPU. A Sun or Spot
  uses its shadow view. A Point uses one 150° cone toward the clear
  surfaces' centre, not its cube faces, so light bent across a face's edge
  still lands; its photons find the scene in whichever face's shadow map
  holds it. Each light gets its own GPU-narrowed fit, a `CausticFit` at its
  first caster slot, and its own layer of the caustic map array, capped at
  1024 square. Sun photons travel parallel; Spot and Point photons leave the
  light's position. `caustic_light` reads the light's slot and tests "under
  the surface" as a distance in metres along the light's ray.
- HDRI light is image-based lighting only. `render_scene` convolves the
  environment once per content version (`prepasses.rs`). Nothing shadows it
  except screen-space AO, or traced GI rays when ray tracing is on.
  "Aim Sun at HDRI" (`hdri_sun.rs`, `scene_edit.rs`) aims the first Sun once;
  the sun in the HDRI still lights the scene through the IBL as well.

## Phase 3: the Sun is the HDRI's sun

- In HDRI mode, the IBL prepass also finds the HDRI's sun on the GPU when the
  environment changes: the bright region around the peak, its direction
  (centre of mass), angular size and total light. A one-off readback per
  environment change, never per frame, hands that to the Sun.
- The Sun card gets a Follow HDRI switch, shown in HDRI mode, on by default.
  With it on, the first Sun, already wired to the environment, takes that
  direction and colour. The HDRI's rotation turns it live. Its Intensity
  multiplies the measured strength, 1 by default, which fixes HDRIs that clip
  the sun. Its angular size sets the shadow's soft edge.
- With it on, the IBL convolution reads the environment with the sun region
  filled from the sky around it, so the sun lights the scene once, through
  the Sun, with its shadow, caustics and highlight. The visible sky keeps the
  real sun.
- With it off (an HDRI with no sun, or a Sun placed by hand), the Sun is free
  and the IBL uses the whole HDRI, as today.
- "Aim Sun at HDRI" is deleted; the switch replaces it.

## Later: a physical daylight sky

New scenes start with the Sun at intensity 6 against a Softbox sky with
strips at 1.5 (`scene_compile::light::SUN_INTENSITY`, the `emitter_intensity`
row default). That gives about 70% of the light on open ground from the Sun:
the right balance, tuned by hand, in relative units. The physical version
does what realtime engines do: one Sun drives both its direct light and a
sky from an atmosphere model, in real units (lux) with camera exposure in
EV. The sun-to-sky balance and colour then follow the sun's height on their
own. Not scheduled yet.

## Not in this design

- Shadows from the rest of the sky stay with AO, or with traced GI.
- Area lights; a Fire's own point lights stay unshadowed.

## Order and ownership

Phases land in order, each its own commit with stills. `render_scene.wgsl`
and `scene_shading.wgsl` are shared with the terrain and Lumen sessions;
message them before editing, and before any edit to `frame_setup.rs`.
