# Primitive Library Completion Plan

**Status:** Draft, 2026-05-20 (revised). Honest per-primitive ground truth: reference shader paths, extraction vs. new code, hour-level estimates. Most "new" primitives are extractions of existing shader code from shipping generators and effects — they need a `primitive!` macro wrapper, port declarations, smoke tests, not greenfield design.

**Why this exists:** The primitive library is a generative vocabulary for AI agents (via MCP/API) and external users to author novel effects and generators. See [[project-primitive-library-for-ai-authoring]]. Build the full vocabulary first, then do JSON migration in a single pass.

Companion docs:
- [BUFFER_PORT_PLAN.md](BUFFER_PORT_PLAN.md) — Phase A/B/C ship status
- [PRIMITIVE_LIBRARY_DESIGN.md](PRIMITIVE_LIBRARY_DESIGN.md) — port-type semantics, design philosophy
- [NODE_GRAPH_SYSTEM.md](NODE_GRAPH_SYSTEM.md) — runtime architecture

---

## Current state (shipped)

- **Phase A** — particle family (5 primitives, 2026-05-19)
- **Phase B** — mesh family (5 primitives, 2026-05-20)
- **Phase C partial** — line family (2 of 3 primitives, 2026-05-20)
- **~50 texture-domain atoms** — long-shipped (Mix, Blur, Feedback, GaussianBlur, MipChain, Threshold, Sample, Transform, ColorGrade, EdgeDetect, KaleidoFold, QuadMirror, Math, LFO, Smoothing, BeatGate, Strobe, MaskedMix, ColorSample, ColorRamp, ChannelMix, Brightness, Gain, ClampStretch, ChromaticOffset, Compose/Blend, DitherPattern, HighlightBoost, Invert, Luminance, Lut1d, Peak, Temporal/Feedback, UV, Value, WetDryMix, plus fused composites: Bloom, Halation, Watercolor, DepthOfField, VoronoiPrism, Infrared, ChromaKey, Glitch, Mirror, SoftFocus, StrobeOpacity)

---

## Primitives to build

Conventions:
- **Status:** `extract` (reference WGSL shipping today, just needs wrapping), `partial` (some code exists, some new), `new` (no reference, design from scratch)
- **Est:** hours of focused work to ship + tests + commit. Realistic per-primitive throughput is 4-6 primitives per session for `extract` work, 2-3 per session for `partial`, 1-2 per session for `new`.

### General-purpose compute atoms

| Primitive | Status | Reference | Est |
|---|---|---|---|
| `node.gradient_2d` | extract | `generators/shaders/fluid_gradient_rotate.wgsl` (split the fused gradient from the rotate) | 1.5h |
| `node.rotate_2d` | extract | same source, second half | 1h |
| `node.reinhard_tone_map` | extract | `generators/shaders/fluid_display_compute.wgsl` (32 lines) | 0.5h |
| `node.aces_tone_map` | extract | `effects/shaders/aces_tonemap_compute.wgsl` (companion) | 0.5h |
| `node.neighbor_smooth` | extract | `generators/shaders/digital_plants_smooth.wgsl` (52 lines, generic 5-point cross average over Array<T>) | 1h |
| `node.sobel` | extract | `generators/shaders/metallic_glass_process.wgsl` (Sobel section) — pure Sobel without threshold | 1h |
| `node.mirror_axis` | extract | `generators/shaders/metallic_glass_process.wgsl` (45° mirror at arbitrary angle, distinct from `quad_mirror`'s fixed 4-way) | 1h |
| `node.linear_to_pq` | extract | `effects/shaders/linear_to_pq_compute.wgsl` (HDR display transform — useful for HDR pipelines) | 0.5h |

**Already covered, no new primitive needed:**
- `histogram_analyze` → `node.luminance` already exists (average luminance reduction, same shape as `auto_gain_measure.wgsl`)
- `exposure_compensate` → `node.luminance` → `node.smoothing` → `node.gain` chain using existing primitives

### Particle family completions

| Primitive | Status | Reference | Est |
|---|---|---|---|
| `node.seed_particles_from_texture` | extract | `generators/shaders/fluid_text_seed.wgsl` (108 lines, samples bright texels to spawn particles) | 2h |
| `node.integrate_particles_attractor` | extract | `generators/shaders/strange_attractor_simulate.wgsl` (282 lines, 5 attractor types). Could be merged into existing `integrate_particles` with a `mode` enum. | 3h |
| `node.integrate_particles_curl_noise` | extract | `generators/shaders/oily_fluid.wgsl` (curl-noise velocity field section) | 2h |
| `node.fluid_simulate` | extract | `generators/shaders/fluid_simulate.wgsl` (the main FluidSimCore integrator) | 2-3h |
| `node.fluid_seed` | extract | `generators/shaders/fluid_seed.wgsl` (7 legacy seed patterns: cluster, lines, rings, cross, spiral, edge, uniform) | 2h |
| `node.point_sprite_render` | new | small render-pass primitive analogous to existing `render_lines`. Each particle = one screen-space sprite. | 2-3h |

### Mesh family completions

| Primitive | Status | Reference | Est |
|---|---|---|---|
| `node.triangulate_grid` | new | Convert NxM `Array<MeshVertex>` grid → triangle-list. No reference; trivial compute kernel. | 2h |
| `node.displace_mesh` | partial | Reference in `generators/shaders/metallic_glass_render.wgsl` vertex shader (displacement section); needs standalone compute kernel form. | 2h |
| `node.generate_cube_mesh` | partial | Cube vertex data in `generators/mesh_pipeline.rs` (procedural in vertex shader from `vertex_index`); extract to standalone compute producer with 36 hardcoded verts. | 1.5h |
| `node.generate_platonic_solid` | partial | Vertex data in `generators/wireframe_zoo.rs` (CPU); port to a producer compute kernel. | 2h |
| `node.rotate_3d` | extract | `generators/generator_math.rs::rotate_3d` (CPU) — port to WGSL. | 0.5h |
| `node.project_3d` | extract | `generators/generator_math.rs::project_3d` — port to WGSL. | 0.5h |
| `node.project_4d` | extract | `generators/generator_math.rs::project_4d` — port to WGSL. | 0.5h |
| `node.render_shadow_pass` | extract | `generators/shaders/digital_plants_shadow.wgsl` (76 lines, depth-only instanced render). | 2h |
| `node.render_instanced_with_shadow` | extract | `generators/shaders/digital_plants_render.wgsl` (158 lines, instanced render with PCF shadow sampling). Could extend existing `render_instanced_3d_mesh`. | 2.5h |
| `node.bake_hdr_envmap` | extract | `generators/shaders/metallic_glass_envmap.wgsl` (46 lines, procedural HDR sky). | 1h |
| `node.pbr_render_3d_mesh` | extract | `generators/shaders/metallic_glass_render.wgsl` (203 lines, Cook-Torrance BRDF + IBL). | 3h |

### Line family completion

| Primitive | Status | Reference | Est |
|---|---|---|---|
| `node.audio_input` | **parked** | needs audio sample channel design (see "open architectural decisions" below) | — |

### Effect-side primitives (decompose fused composites + legacy wrappers)

| Primitive | Status | Reference | Est |
|---|---|---|---|
| `node.gaussian_blur_variable_width` | extract | `effects/shaders/fx_depth_of_field_compute.wgsl` (CoC-modulated Gaussian section) | 2h |
| `node.convolution_2d_9tap` | extract | `effects/shaders/fx_watercolor_compute.wgsl` (diffusion pass — non-separable 9-tap) | 1.5h |
| `node.flow_field_from_luma` | extract | `effects/shaders/fx_watercolor_compute.wgsl` (flow generation pass) | 2h |
| `node.uv_slope_displace` | extract | `effects/shaders/fx_watercolor_compute.wgsl` (slope displacement pass) | 1.5h |
| `node.depth_estimate_midas` | extract | `manifold-native::DepthEstimator` (FFI plugin already shipping, used by `DepthOfField` and `WireframeDepth`). Wrap as primitive. | 2h |
| `node.optical_flow_estimate` | extract | Native plugin path used by `WireframeDepth`. Wrap as primitive. | 2h |
| `node.blob_detect_ffi` | extract | `manifold-native::BlobDetector` FFI plugin. Wrap as primitive. | 2h |
| `node.blob_overlay_render` | extract | `effects/shaders/fx_blob_overlay_render.wgsl` | 1.5h |
| `node.wireframe_extract_from_depth` | extract | `effects/shaders/fx_wireframe_depth.wgsl` / `fx_wireframe_depth_compute.wgsl` (15-pass pipeline; pick the wireframe-extract step) | 2h |
| `node.midas_mesh_pyramid` | extract | same source, mesh pyramid pass | 2h |

### 3D volume family

**Prerequisite:** Texture3D resource pool in `node_graph/metal_backend.rs` — `manifold-gpu` already supports 3D textures fully (used by FluidSim3D's atomic implementation today via direct `GpuDevice::create_texture`). Pool wiring + `pre_bind_texture_3d` + `texture_3d` accessor: **~3-4h focused work**.

| Primitive | Status | Reference | Est |
|---|---|---|---|
| `node.scatter_particles_3d` | extract | `generators/shaders/fluid_scatter_3d.wgsl` | 2h |
| `node.resolve_3d_accumulator` | new | Adapt 2D `resolve_accumulator.wgsl` to 3D | 1h |
| `node.blur_3d_separable` | extract | `generators/shaders/fluid_blur_3d.wgsl` | 1.5h |
| `node.gradient_3d` | extract | `generators/shaders/fluid_gradient_curl_3d.wgsl` (gradient section) | 1.5h |
| `node.curl_3d` | extract | same source, curl section | 1.5h |
| `node.project_particles_3d` | extract | `generators/fluid_simulation_3d.rs` ProjectedScatter pass | 2h |
| `node.sample_volume_2d` | extract | `generators/shaders/mri_slice_compute.wgsl` (volume slice sampling) | 1h |

### Procedural texture math family (NEW — required to decompose Plasma / BasicShapesSnap / ConcentricTunnel / StarField cleanly)

Originally I claimed these four single-shader generators "can't decompose" — that was the old atomic-vs-composite framing where shader complexity dictated atomicity. Wrong. Each is a per-pixel math function of `(uv, time)` and decomposes naturally into compositions of field-generator and per-pixel-math primitives, the same way Substance Designer / TouchDesigner / shader-graph tools work.

**Critical framing constraint** (per Peter, 2026-05-20): the WGSL escape hatch is **not** to be used as a lazy fallback for simple per-pixel math. It's reserved for genuinely irreducible kernels (BlackHole's relativistic geodesic tracing, OilyFluid's coupled reaction-diffusion). Decomposing Plasma must happen through composable primitives, not by embedding 10 lines of WGSL.

**Field generators** (zero inputs → Texture2D field):

| Primitive | What it does | Est |
|---|---|---|
| `node.uv_field` | R = u, G = v at each pixel. The foundation — most other field generators are math-on-uv. | 0.5h |
| `node.distance_to_point` | Per-pixel scalar distance from a center (param: cx, cy). | 0.5h |
| `node.polar_field` | R = angle (atan2), G = radius. Polar-coord building block. | 0.5h |
| `node.simplex_noise_2d` | 2D simplex noise. The workhorse procedural noise. | 1h |
| `node.perlin_noise_2d` | Perlin noise (different aesthetic than simplex). | 1h |
| `node.fbm_2d` | Octave-summed fBM (fractional Brownian motion). | 1h |
| `node.voronoi_2d` | Worley/voronoi noise (cellular patterns). | 1.5h |
| `node.checkerboard` | Alternating pattern at configurable scale. | 0.5h |

**Per-pixel single-input math** (Texture2D → Texture2D):

| Primitive | What it does | Est |
|---|---|---|
| `node.sin_texture` | Per-pixel sin(rgb). | 0.3h |
| `node.cos_texture` | Per-pixel cos(rgb). | 0.3h |
| `node.fract_texture` | Per-pixel fract(rgb). | 0.3h |
| `node.abs_texture` | Per-pixel abs(rgb). | 0.3h |
| `node.power_texture` | Per-pixel pow(rgb, exponent). | 0.3h |
| `node.scale_offset_texture` | Per-pixel a*x + b (uniform scale + uniform offset). | 0.5h |

**Already exists from earlier work, no new primitive needed:**
- Binary per-pixel arithmetic (add / multiply / max / screen) → `node.compose` covers this
- Domain warping (sample with offset) → `node.uv_displace_by_flow` covers this
- Color lookup (scalar → color via 1D LUT) → `node.lut1d` covers this

**Decomposition examples:**
- Plasma "Classic" ≈ `distance_to_point → sin_texture → compose:add(uv_field → sin_texture) → lut1d` (4-5 nodes)
- ConcentricTunnel ≈ `distance_to_point → scale_offset_texture → sin_texture → abs_texture → lut1d`
- StarField ≈ `voronoi_2d → fract_texture → power_texture(high) → compose:multiply(constant color)`
- BasicShapesSnap ≈ depends on shape — for circles: `distance_to_point → scale_offset_texture → step (compose with threshold)`

**Net new: ~13 primitives, ~9 hours work total.** Each is small (~30-80 lines of WGSL + the standard primitive!-macro wrapping). Cumulatively they unlock every procedural texture generator and a vast novel-composition surface for AI agents (procedural textures are one of the highest-reach building-block families in shader-graph tooling).

Ships **before** Batch 6 (WGSL escape hatch). With these in place, the escape hatch genuinely is reserved for the irreducible 5% (BlackHole, OilyFluid, novel agent-authored kernels with no compositional path).

### WGSL escape hatch (CRITICAL — agent-authored kernels for vocabulary gaps)

Two real design questions worth a decision before building:
1. **API shape:** Fixed-shape variants (`wgsl_compute_1in_1out_tex`, etc.) vs. one dynamic-port primitive with ports declared in the JSON. Recommendation: ship 6 fixed-shape variants first; mechanical with existing macro infrastructure. Add dynamic-port version later if agents struggle with the fixed shapes.
2. **Persistent state:** If an agent's kernel needs feedback or a particle buffer between frames, how does it declare it? Three options: (a) a state-handle param the kernel reads/writes, (b) a separate `wgsl_compute_stateful` variant that pairs the kernel with an `ArrayFeedback`-like state buffer, (c) the kernel composes with existing `array_feedback` / `temporal_feedback` upstream nodes via wires.

Recommendation: (c) for now — the existing feedback primitives already handle state cleanly, and the WGSL kernel stays stateless. Revisit if patterns emerge that need (b).

| Variant | Shape | Est |
|---|---|---|
| `node.wgsl_compute_0in_1tex` | Pure generator: WGSL writes to one Texture2D output | 1h |
| `node.wgsl_compute_1tex_1tex` | Texture-to-texture filter | 1h |
| `node.wgsl_compute_2tex_1tex` | Two-input composite | 1h |
| `node.wgsl_compute_1arr_1arr` | Array-to-array (particle-style) | 1h |
| `node.wgsl_compute_1arr_1tex` | Array → Texture (custom resolvers) | 1h |
| `node.wgsl_compute_1tex_1arr` | Texture → Array (custom samplers) | 1h |
| Naga error-to-string formatting for LLM consumption | shared infra | 1.5h |

**Per-variant interface (uniform across all six):**
- `wgsl_source: String` — the kernel source (entry point: `fn cs_main`)
- 8 generic scalar uniform slots accessible in WGSL as `u.f0` ... `u.f7`
- `workgroup_x / y / z: Int` params
- Output dispatch dimensions default to "match output texture/buffer size"
- Pipeline cached by `hash(wgsl_source + workgroup_dims)`
- Validation: Naga parse + pipeline-create at chain build time. Errors returned with line/column, formatted for self-correction.

### Audio family

**Parked.** Source-policy decision needed (live mic / project audio track / Ableton OSC / analyzer plugin / multi-source enum). Once settled, the audio sample channel through `EffectNodeContext` is ~2-3h of work and the primitives below are all extractions or trivial:

- `node.audio_input` — expose samples as Array<f32>
- `node.audio_fft` — Cooley-Tukey GPU FFT
- `node.audio_envelope_follow` — attack/release envelope → Scalar(f32)
- `node.audio_band_energy` — FFT-bin sum over frequency range → Scalar(f32)

---

## Build order

Order is by primitive-independence and reuse. Most batches can ship in a single session.

**Batch 1 — General atoms (1 session)**
gradient_2d, rotate_2d, reinhard_tone_map, aces_tone_map, neighbor_smooth, sobel, mirror_axis, linear_to_pq. ~7h work.

**Batch 2 — Particle family (1 session)**
seed_particles_from_texture, integrate_particles_attractor, integrate_particles_curl_noise, fluid_simulate, fluid_seed, point_sprite_render. ~12-15h, possibly split across two sessions.

**Batch 3 — Mesh family completions (1-2 sessions)**
triangulate_grid, displace_mesh, generate_cube_mesh, generate_platonic_solid, rotate_3d, project_3d, project_4d, render_shadow_pass, render_instanced_with_shadow, bake_hdr_envmap, pbr_render_3d_mesh. ~18h.

**Batch 4 — Effect-side primitives (1 session)**
gaussian_blur_variable_width, convolution_2d_9tap, flow_field_from_luma, uv_slope_displace, depth_estimate_midas, optical_flow_estimate, blob_detect_ffi, blob_overlay_render, wireframe_extract_from_depth, midas_mesh_pyramid. ~18h.

**Batch 5 — Texture3D backend + 3D primitives (1 session)**
Backend wiring (~3-4h) + 7 primitives (~11h) = ~15h.

**Batch 5.5 — Procedural texture math family (1 session, NEW)**
8 field generators (uv_field, distance_to_point, polar_field, simplex/perlin/fbm/voronoi noise, checkerboard) + 6 per-pixel math ops (sin/cos/fract/abs/power/scale_offset on textures). ~9h. Ships **before** Batch 6 so the WGSL escape hatch genuinely is reserved for irreducible kernels. Unlocks decomposition of Plasma / BasicShapesSnap / ConcentricTunnel / StarField — they were misclassified as "atomic by design" earlier; they're actually math-on-fields compositions.

**Batch 6 — WGSL escape hatch (1 session)**
6 variants + Naga error formatting. ~7-8h. Use ONLY for genuinely irreducible kernels (BlackHole, OilyFluid, novel agent-authored kernels with no compositional path through the existing vocabulary). NOT a lazy default — the procedural texture math family covers the bulk of "per-pixel math" that would otherwise be tempting to embed as one-off WGSL.

**Batch 7 — LLM-audience pass (1 session)**
Audit every primitive's `purpose` / `composition_notes` / `examples` for AI clarity. Audit validation error messages for self-correction quality. Add `node.describe` introspection endpoint returning the full primitive catalog as structured JSON for agent consumption. ~6h.

**Batch 8 — Audio family (1 session, deferred until source-policy decision)**
Channel plumbing + 4 primitives. ~6h once policy is settled.

**Batch 9 — JSON migration (3-6 sessions)**
- Decomposed compositions of procedural textures (with Batch 5.5 in place): Plasma, BasicShapesSnap, ConcentricTunnel, StarField — these are math-on-fields, NOT atomic wraps
- Decomposed compositions: StrangeAttractor, ParticleText, DigitalPlants, MetallicGlass, FluidSim2D (the existing `atomic.fluid_sim_2d`), Watercolor, DepthOfField, AutoGain, BlobTracking, WireframeDepth
- Re-author Tesseract / Duocylinder / WireframeZoo / Lissajous / OscilloscopeXY as GPU-side JSON compositions (current CPU implementations remain as reference for parity testing)
- Wrap genuinely irreducible kernels (OilyFluid reaction-diffusion, BlackHole geodesic tracing) using `wgsl_compute_*` primitives embedding their existing shaders
- FluidSim3D and MriVolume now decomposable since Texture3D backend exists
- Audio-driven graphs once audio batch ships

---

## Honest scope

Roughly **7-9 sessions of primitive-building work** (Batches 1-7), or **8-10 with audio** (Batch 8 once unblocked).

JSON migration (Batch 9) is another **3-6 sessions**.

**Total: ~11-16 sessions** for a complete primitive vocabulary + full migration. This is significantly less than initial estimates because the work is overwhelmingly extraction, not greenfield design.

---

## Open architectural decisions

The only items that actually require your input:

1. **Audio source policy** — multi-source enum recommended (live mic / project audio / Ableton OSC / analyzer plugin), first-cut launch with whichever source is easiest (probably project audio since the timeline already exposes audio clips). Settles Batch 8 timing.

2. **WGSL escape hatch state declaration** — recommend option (c): kernels stay stateless, state lives upstream via existing `array_feedback` / `temporal_feedback` primitives. Confirm or redirect before Batch 6.

3. **`atomic.fluid_sim_2d` decomposition fidelity** — JSON-composed equivalent uses `node.fluid_simulate` + `node.fluid_seed` + existing primitives; will not be bit-exact with the legacy because of fp16 boundary intermediates. Acceptable trade-off? (My recommendation: yes — the post-migration system becomes the source of truth.)

---

## References

- [PRIMITIVE_LIBRARY_DESIGN.md](PRIMITIVE_LIBRARY_DESIGN.md) — design philosophy
- [BUFFER_PORT_PLAN.md](BUFFER_PORT_PLAN.md) — Phase A/B/C ship status
- [NODE_GRAPH_SYSTEM.md](NODE_GRAPH_SYSTEM.md) — runtime architecture
- [ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md) — `primitive!` macro guide
- Memory: `project_primitive_library_for_ai_authoring.md` — AI-authoring framing
