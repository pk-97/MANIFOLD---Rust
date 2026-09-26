# Subsurface materials — diffusion and volumetric random walks

**Status:** IN PROGRESS · 2026-09-26 · Codex · BUG-5l4o. Both transport modes and shared controls are implemented. Focused analytic, instance-isolation, layer-isolation and backlighting GPU proofs pass. Broader reference/production qualification remains open; section 8 records measured evidence and limits.
**Prerequisites:** the material fidelity corrections in GLTF_MATERIAL_EXTENSIONS_DESIGN.md section 7.
**Execution contract:** read DESIGN_DOC_STANDARD.md sections 5–6 before implementing a phase.

Peter requested "More accurate volumetric scattering, accepting a much higher
GPU cost", then "Ideally we support both so you can do cheap real-time or
accurate higher cost". Both modes therefore use one physical material payload.
Quality changes the transport estimator, not the material identity. Extend the
existing renderer and Metal acceleration lifecycle; do not add another renderer.

## 1. Audit — verified 2026-09-26

| Existing piece | Source anchor | Reuse |
|---|---|---|
| Material values and generated parameter surface | `node_graph/material.rs` (`Material`), `primitives/pbr_material.rs` (`PbrMaterial`) | Add one zero-default scattering payload; existing node edits and serialization remain authoritative. |
| Current-frame geometry and instance tables | `primitives/render_scene.rs` (`rt_accel_maintenance`), `manifold-gpu/src/metal/raytrace/accel.rs` (`RtAccel`) | Same admission, build/refit, instance identity, lifetime pins and failures. |
| Alpha-aware primary and shadow queries | `manifold-gpu/src/metal/shadow_rays.msl` (`walk_with_alpha_test`) | Same texture table and coverage rules. |
| Optional rendering resources | `primitives/render_scene.rs` (`ensure_rt_irradiance`) | Cached textures sized on change; no allocation or dispatch while scattering is disabled. |
| Diffuse/specular separation | `shaders/render_scene.wgsl` (`fs_pbr`, `diffuse_component`) | Replace the requested fraction of diffuse; retain reflection, sheen, coat and emission. |

At the start of this work there was no diffusion or random-walk SSS
implementation. Thin diffuse transmission and volume absorption remain separate
material effects.

## 2. Decisions

- **D1 — Two explicit modes.** `Diffusion` uses a bounded spatial diffusion
  profile projected onto the object's actual surfaces. `RandomWalk` samples
  free flights and Henyey–Greenstein scattering inside the same instance.
  Rejected: relabelling wrapped backlighting as SSS; it does not transport light
  between surface points.
- **D2 — Native Metal geometry reuse.** Both modes use the existing acceleration
  structure. SSS may request its maintenance without enabling RT reflection,
  GI, AO or shadow substitution. Rejected: a screen blur of the final image,
  because it spreads highlights/emission and cannot see the back of an object.
- **D3 — Homogeneous BSSRDF.** Scattering colour is single-scattering albedo,
  radius is the RGB transport mean-free path in world units, and anisotropy is
  the phase mean cosine. Geometry bounds the volume. Surface entry/exit is a
  diffuse boundary model; this is not a general refractive volume/caustics path
  tracer. Existing PBR Fresnel accounts for the visible surface reflection.
- **D4 — Explicit bounds.** Random walks have a maximum of 256 events, with
  Russian roulette after event 8. Missing boundaries and exhausted walks produce
  an invalid-result marker, not a silent diffusion fallback. The renderer must
  expose the invalid result. Closed meshes are required for RandomWalk.
- **D5 — Per-frame estimates.** Samples are independent per frame; no new
  temporal history, locks or thread. Reuse existing frame jitter/blue-noise
  utilities. Sample count trades GPU work for variance. Diffusion is the cheaper
  approximation; section 8 records the bounded measurement.

## 3. Data and rendering seam

Renderer-owned CPU data in `node_graph/material.rs`:

```rust
pub enum SubsurfaceMode { Diffusion, RandomWalk }
pub struct Subsurface {
    pub weight: f32,
    pub radius: [f32; 3],
    pub color: [f32; 3],
    pub anisotropy: f32,
    pub mode: SubsurfaceMode,
    pub samples: u32,
}
```

Both are `Clone + Copy + Debug + PartialEq`; mode also derives `Eq`. Defaults:
weight 0, radius `[0.01, 0.005, 0.0025]`, colour `[0.9, 0.8, 0.7]`, anisotropy 0,
Diffusion, 8 samples. The PBR node has scalar ports for continuous values and
enum/integer parameters for mode/samples. Samples clamp to 1–64, phase to
−0.95–0.95, radius to 0.00001–10, weight/colour to 0–1. Nonfinite inputs use
defaults. No experimental glTF extension is claimed as a ratified SSS format.

GPU data in `manifold-gpu/src/metal/raytrace/params.rs`:

```rust
#[repr(C)]
pub struct SubsurfaceMaterial {
    pub color_weight: [f32; 4],
    pub radius_phase: [f32; 4],
    pub config: [u32; 4], // mode (0 diffusion, 1 random walk), samples, 0, 0
}
#[repr(C)]
pub struct SubsurfaceParams {
    pub inv_view_proj: [[f32; 4]; 4],
    pub camera_pos: [f32; 4],
    pub render_size: [u32; 2],
    pub frame_index: u32,
    pub slot_row_base: u32,
    pub light_count: u32,
    pub material_count: u32,
    pub query_units_per_pixel: u32,
    pub _pad: u32,
}
```

Both derive `Clone + Copy + Debug + bytemuck::Pod + bytemuck::Zeroable` and have
48/112-byte size assertions. Material rows use canonical object order; committed
instance IDs select `RtNormalSource`, whose `object_index` selects the material.
Samples never walk into another instance, including another copy of the same mesh.

`MetalShadowRayTracer::dispatch_subsurface` takes encoder, device, params, accel,
normal-source buffer, scattering-material buffer, canonical GI-material buffer,
existing light buffer, current object slice, material texture slice, camera-depth
texture, environment texture and output texture. It uses the existing
`dispatch_compute_with_accel` residency/tiling path. Query work is bounded by
`1 + max(samples * (mode_event_limit + light_count + 1))` per pixel, and regions
are separated by the existing command-buffer continuation mechanism. The cached pipeline is added
to `RtPipelines`; there is no per-frame compilation. A separate
`subsurface.msl` is concatenated after `shadow_rays.msl` so helpers have one home.
Keep the original source constant intact for existing source-ownership tests.

Binding ABI: buffers 0 accel, 1 params, 2 normal sources, 3 scattering materials,
4 lights, 5 canonical GI materials, 6 tile region. Textures 0 camera depth,
1 environment, 2 output, 4–67 material textures. Output is full render-resolution
RGBA16F: RGB scattering radiance, A canonical object index + 1; zero means no
scattering, −1 means invalid geometry or exhausted work. Raster reads only the
matching object row and substitutes `weight * (1-metallic)`, with Fresnel energy
reduction. Transparent/point draws cannot use this opaque depth result.

Transport coefficients per channel, with colour `a`, phase `g`, radius `r`:
`sigma_t = 1 / (r * (1 - a*g))`, `sigma_s = a*sigma_t`,
`sigma_a = (1-a)*sigma_t`. The random walk chooses RGB hero channels uniformly
and compensates by 3. It starts with an inward cosine direction; samples
exponential free-flight distances; multiplies throughput by albedo at a volume
event; samples HG for the next direction; and evaluates illumination at the
first boundary exit. Roulette divides throughput by survival probability.
No contribution is silently assigned to an unfinished path.

Exit illumination includes all wired direct lights with their existing attenuation
and shadow policy, plus a cosine-sampled environment visibility ray. An environment
ray hitting emissive geometry reads the canonical emission. Multiple surface
interreflection is outside this homogeneous BSSRDF estimator.

Diffusion uses reduced coefficients, the classic dipole profile and three
projection axes (normal 1/2, tangent/bitangent 1/4 each). Sampled projected
points are ray-intersected with the same instance, with bounded candidate
selection. Its finite support and diffusion approximation are intentional;
thin features and low-albedo media should use RandomWalk.

Cost: one cached full-resolution RGBA16F output (about 16 MiB at 1080p), a
48-byte table row per opaque object, existing acceleration maintenance, and
per-pixel queries proportional to samples and scattering events. Diffusion
still has a geometry-query cost; it is not free. Zero weight avoids the SSS
allocation and dispatch. Accurate mode can be much slower and noisy at low
sample counts. The small-fixture timing below is not a full-resolution frame-rate
promise.

## 4. Invariants and enforcement

| Invariant | Required machine check |
|---|---|
| Existing materials remain off | `subsurface_defaults_are_disabled` CPU evaluation proof. |
| Both modes share coefficients | `subsurface_modes_preserve_shared_controls` CPU proof. |
| ABI and binding agreement | Size/offset assertions and a real Metal material-row selection proof. |
| Zero weight has no dispatch/resource cost | Executor capture with SSS off/on/off. |
| Boundaries belong to the same instance | Enclosed foreign-instance fixture leaves both transport modes unchanged. |
| Radius changes spatial transport | Small patterned light patch on a closed slab, narrow/wide radii comparison. |
| Absorption and event sampling are physical | Pure-absorption Beer–Lambert and homogeneous white-environment energy proofs. |
| RandomWalk sees hidden surfaces | Backlit closed slab/sphere compared with no-scattering and diffusion controls. |
| Layer energy stays separate | Specular-only and emissive-only controls remain invariant as scattering weight changes. |
| Invalid work is visible | Open geometry and exhausted-step fixtures assert the invalid marker. |

The checks above describe acceptance targets. Section 8 distinguishes executed
proofs from remaining visual/reference coverage.

## 5. Phasing

1. **Controls and ABI:** exact types above, finite-input handling, actual node
   evaluation tests. No unused controls may land without their renderer.
2. **Transport and integration:** cached Metal pipeline, current-frame geometry,
   output texture, diffuse substitution and visible invalid-result handling.
   Run the named numeric Metal proofs, scoped Clippy and CPU regressions.
3. **Acceptance:** bounded same-scene timings for both modes and sample counts;
   observed renders of the named backlit and layer-separation fixtures. L1
   metrics do not substitute for Peter's L2 artistic review. Existing landing
   gate and GPU proof gate remain required.

## 6. Decided — do not reopen

Two modes, shared physical controls; geometry-based transport; zero-default
activation; reuse current acceleration lifetime; no silent quality fallback;
surface layers remain outside scattering.

## 7. Deferred

Heterogeneous volume textures, spectral transport, arbitrary DCC shader networks,
refractive volume caustics and nested participating media require a broader volume
integrator. Revisit when an asset or product request requires those capabilities.

## 8. Focused evidence (2026-09-26)

`render_scene_subsurface` passes zero-weight output/no-dispatch, pure-absorption
Beer–Lambert, white-environment random-walk energy, positive diffusion output,
open-quad invalid transport, and emission/metal-reflection isolation checks.
An enclosed foreign black instance leaves both modes byte-identical to their
nonzero-scattering controls. The absorption oracle independently integrates the cosine-weighted slab
transmittance; it is not a comparison against MANIFOLD goldens.

A closed backlit slab at 64×64, eight frames, gives the following mean warm
CPU-encode plus GPU-completion times, excluding initial compilation and readback:

| Mode | Samples | Radius 0.03 | Radius 0.3 |
|---|---:|---:|---:|
| Diffusion | 8 | 3.70 ms | 2.39 ms |
| RandomWalk | 64 | 127.87 ms | 40.71 ms |

Both modes transmit more light with the wider radius. The four captured renders
were inspected: the wider medium spreads the backlight, and the volumetric
estimates show sampling noise. These timings compare two useful quality
settings, not equal sample counts or full-app throughput. There is no 1080p
real-time claim. The implementation traces at full render resolution; cost
depends strongly on resolution, sample count, albedo, thickness and radius.

Peter's artistic acceptance and independent reference comparisons for curved
and heterogeneous-looking assets remain outside this numeric evidence. The
exhausted-event marker is implemented but does not yet have a deterministic
GPU fixture; the open-surface marker is exercised. Point primitives and
transparent draws reject SSS explicitly because the opaque depth/boundary
contract cannot represent them.
