# Node Catalog

**Source of truth for what nodes exist.** Regenerate this file by walking [`crates/manifold-renderer/src/node_graph/primitives/`](../crates/manifold-renderer/src/node_graph/primitives/) (one `type_id` per primitive — `pub const *_TYPE_ID` for the composite-effect primitives, `type_id: "node.…"` for the macro-defined atoms) and the two preset directories ([`effect-presets/`](../crates/manifold-renderer/assets/effect-presets/), [`generator-presets/`](../crates/manifold-renderer/assets/generator-presets/)). If you add a primitive or a preset and don't update this catalog, the catalog is stale — fix it.

For *how* to compose these into a generator decomposition, see [DECOMPOSING_GENERATORS.md](DECOMPOSING_GENERATORS.md). For the design rationale behind the primitive shape, see [PRIMITIVE_LIBRARY_DESIGN.md](PRIMITIVE_LIBRARY_DESIGN.md).

---

## 1. Invariants

- **Type IDs are flat:** `node.<name>` for atoms and effects, `system.<name>` for boundary nodes. No category prefix — the category is presentation, not identity.
- **All primitives share one `Primitive` trait.** "Atom" vs "Effect" is how the palette groups them, not a structural split.
- **Port-shadows-param:** any scalar input port whose name matches a primitive `ParamDef` uses the wire when present and the param as the fallback. Standard for `gain`, `amount`, `rotation`, `wet_dry`, control-rate modulation everywhere.
- **Effects can be monolithic shaders, thin atom presets, or atom composites.** Type IDs stay stable across implementation swaps so save files don't break.
- **`Array<T>` outputs declare capacity** via `EffectNode::array_output_capacity`; the CI sweep test enforces this on the registry.
- **Stateful primitives declare** `state_lifecycle` + `state_capture_input_ports` so the StateStore knows where to break cycles. Per-port, not per-node.

---

## 2. Boundary nodes

| Display Name | Type ID | Inputs → Outputs | Purpose |
|---|---|---|---|
| Source | `system.source` | () → (Texture2D) | Effect-chain input — host pre-binds the upstream texture |
| Generator Input | `system.generator_input` | () → (time, beat, aspect, trigger_count, anim_progress) | Generator graph entry — scalar frame context |
| Final Output | `system.final_output` | (Texture2D) → () | Both surfaces — host pre-binds the chain / generator output texture |

---

## 3. Atoms by intent

### 3.1 Control-rate scalar plumbing

Free to evaluate (no GPU dispatch). The scalar wire graph runs every frame with negligible cost; use these for any modulation-shaped value.

| Display Name | Type ID | Purpose |
|---|---|---|
| Value | `node.value` | Constant scalar source — every outer-card slider routes through one |
| Math | `node.math` | Two-input scalar math (Add/Subtract/Multiply/Divide/Min/Max/Atan2/Sin/Cos); `b` ignored for unary ops |
| Affine Scalar | `node.affine_scalar` | `value * scale + offset` — collapses Value+Math+Value+Math chains |
| LFO | `node.lfo` | Low-frequency oscillator (`Musical` follows `beat`, `Free` follows `time`); Sine/Tri/Saw/Square/SH |
| Beat Gate | `node.beat_gate` | Beat-synced square 0/amount gate with `duty` cycle |
| Trigger Gate | `node.trigger_gate` | Emit a single-frame pulse on integer-edge changes of an input scalar |
| Smoothing | `node.smoothing` | One-pole low-pass on a scalar (stateful) |
| Envelope Follower (AR) | `node.envelope_follower_ar` | Attack/release envelope from an impulse (stateful) |
| Envelope Decay | `node.envelope_decay` | Decay-only envelope (stateful) |
| Sample & Hold | `node.sample_and_hold` | Hold the last sampled input until next trigger (stateful) |
| Trigger Ease To | `node.trigger_ease_to` | Snap-and-glide on a scalar: on each trigger edge captures current visible as `prev` and the input as `curr`, then eases over `window_beats` beats via cubic ease-out (stateful) |
| Threshold (scalar) | `node.filter` (id `node.threshold`) | Pass-above-cutoff with hard / soft-knee mode (also wraps a Texture2D variant) |
| Frequency Ratio | `node.frequency_ratio` | Curated 10-row harmonic ratio table indexed by `trigger_count`, uniqueness-enforced |
| Cycle Table Row | `node.cycle_table_row` | Cycle through a curated `Table` of f32 rows; emits the selected row as `Array<f32>` |
| Clip Trigger Cycle | `node.clip_trigger_cycle` | Wraps `ClipTriggerCycle::step` for primitive-internal trigger→variant mapping |
| Clip Trigger Index | `node.clip_trigger_index` | Same as Cycle but emits the integer index directly |
| Inject Burst | `node.inject_burst` | One-shot scalar burst on trigger; decays over a frame window |

### 3.2 Coordinate fields

Procedural textures that emit per-pixel coordinates. Start most procedural graphs from one of these.

| Display Name | Type ID | Purpose |
|---|---|---|
| UV Field | `node.uv_field` | Per-pixel `(u, v, 0, 1)` in `[0, 1]² `texture-space |
| Centered UV | `node.centered_uv` | Same as UV Field but in `[-1, +1]²` aspect-corrected |
| Polar Field | `node.polar_field` | Per-pixel `(angle/τ, radius, 0, 1)` around a configurable center |
| Grid UV Field | `node.grid_uv_field` | Per-instance UVs as `Array<vec2<f32>>` for instanced rendering |

### 3.3 Procedural noise + texture sources

| Display Name | Type ID | Purpose |
|---|---|---|
| Simplex Noise 2D | `node.simplex_noise_2d` | 2D Ashima simplex, remapped `[0, 1]` to RGB |
| Simplex Field 2D | `node.simplex_field_2d` | 3D simplex sampled at `(uv*scale + offset, z)`, signed output in R |
| Simplex (per instance) | `node.simplex_per_instance` | Per-instance 3D simplex → `Array<f32>` |
| Perlin Noise 2D | `node.perlin_noise_2d` | Classic Perlin gradient noise (different aesthetic from simplex) |
| FBM 2D | `node.fbm_2d` | Octave-summed Perlin (fractional Brownian motion) |
| FBM (per instance) | `node.fbm_per_instance` | Bit-identical to `fbm_2d` but indexed by `Array<vec2<f32>>` |
| Hash Noise 2D | `node.hash_noise_field_2d` | Uncorrelated wang-hash white noise — grain, dust, LIC ink |
| Flow Field Noise | `node.flow_field_noise` | 2-channel flow vectors for advection (Watercolor-style) |
| Voronoi 2D | `node.voronoi_2d` | Worley/Voronoi — F1 (R), F2 (G), F2-F1 (B), per-cell stable hash (A). Foundation for stars, foam, cracked glass, embers, tiles. |
| Checkerboard | `node.checkerboard` | Binary checker pattern at configurable scale |
| Distance to Point | `node.distance_to_point` | Per-pixel distance to a configurable point in UV space |
| Plasma Pattern 2D | `node.plasma_pattern_2d` | Curated family — 8 plasma variants behind a `pattern` enum |
| Basic Shape | `node.basic_shape` | Single-dispatch SDF — Square / Diamond / Octagon picked by static `shape` enum. Three instances + `mux_texture` gives runtime shape selection. |
| Color | `node.color` (id `node.brightness`) | Per-pixel luminance to RGB |

### 3.4 Per-pixel texture math

Compose these for arbitrary procedural fields.

| Display Name | Type ID | Purpose |
|---|---|---|
| Sin Term | `node.sin_term` | `sin((a*r + b*g + c) * freq + time * rate)` — one term of a sum-of-sines |
| Trig Texture | `node.trig_texture` | Per-pixel Sin / Cos / Tan with freq + phase. Both freq and phase have optional texture-shadow inputs (`freq_tex` / `phase_tex` — R channel sampled per pixel) — unlocks per-cell unique trig modulation when fed from per-cell-stable sources like `voronoi_2d.A` via `channel_mix`. |
| Abs Texture | `node.abs_texture` | Per-pixel `abs(rgb)` |
| Fract Texture | `node.fract_texture` | Per-pixel `fract(rgb)` |
| Power Texture | `node.power_texture` | Per-pixel `pow(rgb, exponent)` |
| Smoothstep Texture | `node.smoothstep_texture` | Per-pixel smoothstep contrast curve with low/high edges |
| Scale/Offset Texture | `node.scale_offset_texture` | Per-pixel affine `a*x + b` — the general re-range primitive |
| Field Combine | `node.field_combine` | `a*r + b*g + c` — project a 2-channel field onto a scalar |
| Gain | `node.gain` | Scalar-driven RGB multiplier (port-shadow on `gain`) |
| Invert | `node.invert` | Invert RGB, crossfade by `intensity` |

### 3.5 Color & tone

| Display Name | Type ID | Purpose |
|---|---|---|
| Clamp | `node.clamp_texture` | Per-pixel saturate to [min, max] — the texture-side counterpart of `array_math::Clamp01` |
| Channel Mix | `node.channel_mix` | Per-pixel 4×4 RGBA matrix transform. Default = identity. Use to swizzle channels (A→R for reading cell_hash as a control signal), pull luma, isolate a single channel, or pre-tint for halation-style chains. |
| Color Grade | `node.color_grade` | Gain / saturation / hue / contrast / tint colorize |
| Color LUT | `node.color_lut` | 1D LUT remap via luminance index |
| Infrared | `node.infrared` | Thermal-vision palette (10 baked LUTs) |
| Chroma Key | `node.chroma_key` | Per-pixel RGB-distance mask to a target colour |
| Chromatic Aberration | `node.chromatic_aberration` | RGB channel shift (radial or linear) |
| Chromatic Displace | `node.chromatic_displace` | Per-channel UV displacement by a vector field |
| Tone Map | `node.tone_map` | HDR → SDR/PQ/EDR with ACES / AgX / Khronos Neutral curves |
| Reinhard Tone Map | `node.reinhard_tone_map` | Extended Reinhard, SDR-only; bit-matches FluidSim display |

### 3.6 Image transforms

| Display Name | Type ID | Purpose |
|---|---|---|
| Transform | `node.uv` (id `node.transform`) | translate / scale / rotate / fold (None/X/Y/XY) |
| Affine Transform | `node.affine_transform` | Three-scalar-port affine — port-shadow demo for translate_x/y + rotation |
| Rotate 2D | `node.rotate_2d` | Rotate a 2D coordinate field around the origin |
| Quad Mirror | `node.quad_mirror` | Center-symmetric 4-way fold with crossfade blend |
| Kaleidoscope | `node.kaleido_fold` (id `node.kaleidoscope`) | Polar segment mirror — N wedges |
| Mirror Axis | `node.mirror_axis` | Sample input at UVs mirrored across a line through center at `angle` — single-axis 2-fold symmetry (one half visible, other half is mirror) |
| Edge Stretch | `node.clamp_stretch` (id `node.edge_stretch`) | Clamp to a center strip, stretch edge pixels outward |
| UV Displace by Flow | `node.uv_displace_by_flow` | Sample texture at UVs displaced by a 2-channel flow field |

### 3.7 Spatial filters

| Display Name | Type ID | Purpose |
|---|---|---|
| Gaussian Blur | `node.separable_gaussian` (id `node.gaussian_blur`) | Separable Gaussian, one axis per pass |
| Gaussian Blur (variable width) | `node.gaussian_blur_variable_width` | Per-pixel kernel width from a `width` texture (DoF, masked blur) |
| 3D Separable Blur | `node.blur_3d_separable` | Single-axis Gaussian on a Texture3D (volumetric) |
| Downsample | `node.downsample` | Integer-factor box-filter — pyramid front |
| Convolution 2D 9-tap | `node.convolution_2d_9tap` | General 3×3 kernel — Sobel, Laplacian, emboss, custom |
| Sharpen | `node.sharpen` | One-knob Laplacian unsharp mask |
| Edge Detect | `node.edge_detect` | Sobel 3×3 + smoothstep threshold + crossfade |

### 3.8 Compositing

| Display Name | Type ID | Purpose |
|---|---|---|
| Mix | `node.compose` (id `node.mix`) | Blend two textures — Lerp/Screen/Add/Max/Multiply/Difference/Overlay/Divide. Divide guards against b≈0. |
| Masked Mix | `node.masked_mix` | Per-pixel weighted blend driven by mask.r |
| Wet/Dry | `node.wet_dry_mix` (id `node.wet_dry`) | Crossfade processed against original |
| Texture Sum 5 | `node.texture_sum_5` | Weighted sum of 5 textures — collapses long Mix(Add) chains |
| Pack RGBA | `node.pack_channels` | Combine four single-channel textures into one RGBA by reading `.r` of each input into the matching output channel — the recompose-after-atomic-per-channel-processing atom |
| Vignette | `node.vignette` | Soft fade-to-black border — Circle / Ellipse / Rectangle |

### 3.9 Stateful temporal

State lives in the primitive via `extra_fields:` + `state_lifecycle`. StateStore keys by `(owner_key, node_id)` — fresh on graph rebuild.

| Display Name | Type ID | Purpose |
|---|---|---|
| Feedback | `node.temporal` (id `node.feedback`) | Previous-frame texture accumulation with `amount`, `zoom`, `rotation`, mode |
| Array Feedback | `node.array_feedback` | One-frame delay for `Array<Particle>` — closes per-frame loops without graph cycles |
| Smoothing (scalar) | `node.smoothing` | Listed under control-rate; also valid stateful temporal |
| Envelope Follower / Decay / Sample & Hold | (see §3.1) | Scalar-side temporal state |

### 3.10 Texture → scalar bridges

One-frame readback latency. Pair with `Gain`, `Math`, `Feedback.amount`, etc. for image-driven modulation.

| Display Name | Type ID | Purpose |
|---|---|---|
| Brightness (scalar) | `node.luminance` | Average Rec.709 luma of the whole image |
| Peak | `node.peak` | Maximum Rec.709 luma across the image |
| Color Sample | `node.color_sample` | Region-averaged RGB at a configurable UV + luma |

### 3.11 Gradient / vector-field atoms

| Display Name | Type ID | Purpose |
|---|---|---|
| Gradient (central diff) | `node.gradient_central_diff` | Half-difference gradient `(dx, dy)` of a single channel. `scale_mode`: Texel (default) or UV (multiplies by dim/2 per axis). `wrap_mode`: Clamp (default) or Repeat (toroidal — fluid sims). |
| Heightmap to Normal | `node.heightmap_to_normal` | Scalar height → tangent-space normal map via central-diff |
| Length (vec2) | `node.length_vec2` | `length(in.rg)` as a scalar field — vec2 magnitude atom |
| Normalize (vec2) | `node.normalize_vec2` | Safe-normalize RG as a 2D direction field |
| Rotate vec2 (Angle) | `node.rotate_vec2_by_angle` | Per-pixel vec2 rotation by an arbitrary port-shadowed angle (radians); default PI/2. Legacy `node.rotate_vec2_90` type-ID aliases here. |
| Array Unpack vec2 | `node.array_unpack_vec2` | Decompose `Array<vec2<f32>>` into two `Array<f32>` channels |
| Canvas Area Scale | `node.canvas_area_scale` | `(width * height) / reference_area` — resolution-aware brightness compensation |

### 3.12 PBR shading atoms

All operate on tangent-space normal maps and a directional light. Sum the additive ones via `node.mix` mode=Add.

| Display Name | Type ID | Purpose |
|---|---|---|
| Lambert Directional | `node.lambert_directional` | Diffuse shading from normal + light + ambient (base term) |
| Blinn Specular | `node.blinn_specular` | Blinn-Phong specular (additive) |
| Cook-Torrance Specular | `node.cook_torrance_specular` | Physically-based microfacet specular (D_GGX × G_Smith × F_Schlick) — sibling to Blinn, more accurate for metals (additive) |
| Fresnel Rim | `node.fresnel_rim` | Fresnel edge highlight (additive) |
| Matcap Two-Tone | `node.matcap_two_tone` | Cross-axis 4-colour matcap from a normal map |
| Bake Equirect Envmap | `node.bake_equirect_envmap` | Procedural HDR studio environment map at configurable resolution (one-shot persistent output, equirect layout) |
| Env Reflect (Equirect) | `node.equirect_envmap_sample` | Per-pixel IBL reflection — `reflect(-view, normal)` sampled into an equirect env map |

### 3.13 Flow & fluid

Per-frame fluid-sim primitives. Pair upstream with seed + downstream with scatter/resolve.

| Display Name | Type ID | Purpose |
|---|---|---|
| Texture Advect | `node.texture_advect` | Backward semi-Lagrangian advection by a velocity field |
| LIC Integrate | `node.lic_integrate` | Line Integral Convolution — flow visualisation streamlines |
| Fluid Gradient Curl (3D) | `node.fluid_gradient_curl_3d` | Fused 3D gradient + curl — FluidSim3D force field |
| Apply Radial Burst (Particles) | `node.apply_radial_burst_to_particles` | Per-particle radial+tangent impulse around a point — FluidSim2D inject path |
| Scatter Particles Camera | `node.scatter_particles_camera` (alias `node.fluid_project_scatter_2d`) | 3D particles → 2D u32 accumulator via Camera projection. Sibling to `scatter_particles` / `scatter_particles_3d` |
| Sample Volume 2D | `node.sample_volume_2d` | Sample a Texture3D as 2D slice/projection |

### 3.14 3D + 4D geometry pipeline

| Display Name | Type ID | Purpose |
|---|---|---|
| Generate Grid Mesh | `node.generate_grid_mesh` | NxM grid of `MeshVertex` in XZ plane — heightmap-displaced surfaces |
| Generate Cube Mesh | `node.generate_cube_mesh` | Unit cube as 36 `MeshVertex` triangle-list |
| Polytope Vertices | `node.polytope_vertices` | One of the five Platonic solids as `Array<MeshVertex>`, baked to magnitude 0.25 (curated-enum GPU dispatch) |
| Polytope Edges | `node.polytope_edges` | Wireframe edge topology of the selected Platonic solid as `Array<EdgePair>` (curated CPU lookup) — pair with `polytope_vertices` on the same `shape` scalar |
| Generate Tesseract Vertices | `node.generate_tesseract_vertices` | 16 4D corners + 32 edges for 4D wireframe (hypercube bit-flip topology — closed mathematical structure, hand-typed coords + const-fn edge table) |
| Generate Grid UV | `node.generate_grid_uv` | Pattern-CHOP-of-a-grid: emit two `Array<f32>` (u, v) sampling `[0, u_max) × [0, v_max)` at `n` steps each, flattened row-major (`n²` entries). The (u, v)-parametric authoring atom — pair with `array_math` + `pack_vec4` + `edges_from_grid_uv` to author any parametric surface in pure JSON |
| Pack Vec4 | `node.pack_vec4` | Zip four `Array<f32>` (x, y, z, w) into `Array<Vec4Vertex>`. The 4D analogue of `pack_curve_xy`; pure structural transformation (no scale bake — per-shape magnitude is applied upstream via `array_math(ScaleOffset)`) |
| Edges From Grid UV | `node.edges_from_grid_uv` | u-wrap + v-wrap wireframe edge topology for an `n × n` parametric grid as `Array<EdgePair>` (`2n²` edges). Topology counterpart of `polytope_edges` for any (u, v)-sampled surface (torus, Klein, sphere, terrain) |
| Generate Instance Transforms | `node.generate_instance_transforms` | Procedural `Array<InstanceTransform>` (grid/ring/spiral/random) |
| Nested Cubes Geometry | `node.nested_cubes_geometry` | Curated instanced-cube layout for NestedCubes preset |
| Displace Mesh | `node.displace_mesh` | Perturb mesh Y from a heightmap texture, per-vertex UV sample |
| Triangulate Grid | `node.triangulate_grid` | NxM positions → triangle-list with finite-difference normals |
| Rotate 3D / 4D | `node.rotate_3d`, `node.rotate_4d` | Euler XYZ; stereo XY/ZW/XW for 4D |
| Project 3D / 4D | `node.project_3d`, `node.project_4d` | Orthographic / perspective projection to curve-space |
| Cylinder Wrap Field | `node.cylinder_wrap_field` | Lift `Array<vec2>` onto a cylinder surface as `Array<InstanceTransform>` |
| Torus Wrap Field | `node.torus_wrap_field` | Same shape for a torus |
| Render 3D Mesh | `node.render_3d_mesh` | Render `Array<MeshVertex>` triangle list — Lambert + ambient + orbit-camera params. Also emits `world_pos` + `world_normal` G-buffer outputs (always — TouchDesigner / Blender deferred-shading shape) for downstream screen-space PBR / SSAO / SSR atoms |
| Render Instanced 3D Mesh | `node.render_instanced_3d_mesh` | Render N copies of a base mesh via `Array<InstanceTransform>` |
| Render Lines | `node.render_lines` | Anti-aliased capsule line segments from `Array<CurvePoint>`; optional `edges` input |
| Digital Plants Render | `node.digital_plants_render` | Two-pass shadow + instanced cel-shaded cubes (DigitalPlants-specific) |

### 3.15 2D curves

| Display Name | Type ID | Purpose |
|---|---|---|
| Generate Range | `node.generate_range` | Pattern-CHOP linspace: `Array<f32>` of N samples over `[start, end]`. `end_inclusive` toggles between closed (Lissajous) and exclusive (regular N-gons) sampling; `active_count` port-shadows the runtime sample count for variable-N curves |
| Pack Curve XY | `node.pack_curve_xy` | Zip two `Array<f32>` (x, y) into `Array<CurvePoint>`; folds the `PROJ_SCALE = 0.25` screen-fit constant. Curve-pipeline counterpart to `array_unpack_vec2` |
| Consecutive Edges | `node.consecutive_edges` | Synthesise polyline edge topology `[(0,1), (1,2), …]` from a vertex count; optional closing `(N-1, 0)` edge. Inactive tail is `EdgePair::SENTINEL` for variable-N polygons |
| Replicate Polyline Rings | `node.array_replicate_polyline_rings` | Stack K transformed copies of a polyline (outline + edges) — per-ring uniform scale on points, per-ring index shift on edges (sentinel-preserving). The concentric / stacked-curve atom |

### 3.16 Particle / instance simulation

| Display Name | Type ID | Purpose |
|---|---|---|
| Seed Particles | `node.seed_particles` | Wang-hash uniform `Array<Particle>` seed (EveryFrame or OnceOnReset) |
| Seed Particles from Texture | `node.seed_particles_from_texture` | Seed particles weighted by an input texture's brightness |
| Sample Texture at Particles | `node.sample_texture_at_particles` | Per-particle bilinear sample of a Texture2D at `position.xy` → `Array<vec2<f32>>` of RG samples |
| Euler Step Particles | `node.euler_step_particles` | Apply `position.xy += forces * speed * (delta * 60)` per live particle. Aliased in/out. |
| Wrap Particles (Torus) | `node.wrap_particles_torus` | Per-particle toroidal wrap `position.xy = fract(position.xy + 1)`. Cyclic-boundary policy atom. |
| Diffuse Particles | `node.array_diffuse_particles` | Hash-based random kick on `Particle.velocity` (generic Brownian noise — ODE-state diffusion) |
| Anti-Clump Particles | `node.anti_clump_particles` | Modulator-weighted hash kick on `Particle.position.xy` — optional scalar Texture2D `strength_modulator` concentrates the kick (FluidSim wires density; works with any scalar map). Unwired = plain uniform Brownian jitter. |
| Simplex Noise Force at Particles | `node.simplex_noise_force_at_particles` | Per-particle 2D simplex noise force added in-place to an `Array<vec2<f32>>` buffer. Optional scalar Texture2D `amplitude_modulator` adds capped density-style amplitude boost (legacy density-adaptive noise). Resolution-independent replacement for per-pixel simplex noise texture chains. |
| Radial Burst Force Field | `node.radial_burst_force_field` | Per-pixel vec2 force texture for a radial+tangent impulse around a point with falloff envelope. Sum into a velocity field for "impulse around a point" particle behaviour. |
| Scatter Particles | `node.scatter_particles` | Atomic-add splat into 2D u32 accumulator (Wrap / Discard boundary) |
| Scatter Particles 3D | `node.scatter_particles_3d` | Same shape for `Texture3D` accumulator |
| Resolve Accumulator | `node.resolve_accumulator` | u32 grid → float Texture2D |
| Resolve 3D Accumulator | `node.resolve_3d_accumulator` | u32 grid → float Texture3D |
| Scalar Array Accumulator | `node.scalar_array_accumulator` | Stateful running sum of an `Array<f32>` |
| Array Math | `node.array_math` | Element-wise math on `Array<f32>` (Add/Sub/Mul/Div/Min/Max/ScaleOffset/Shape/MirrorRamp/Clamp01/Abs/Sin/Cos/Mix) |
| Instance Position Jitter | `node.instance_position_jitter` | Per-instance position offset from a noise field |
| Instance Rotation Jitter | `node.instance_rotation_jitter` | Per-instance rotation jitter |
| Lerp Instance Fields | `node.lerp_instance_fields` | Per-field interpolation between two `Array<InstanceTransform>` |
| Neighbor Smooth | `node.neighbor_smooth` | 5-point cross-neighborhood smoothing on an NxN grid of InstanceTransforms |

### 3.17 Routing

Variadic N-way selectors driven by a scalar.

| Display Name | Type ID | Purpose |
|---|---|---|
| Mux Scalar | `node.mux_scalar` | 8-way scalar selector |
| Mux Texture | `node.mux_texture` | 8-way `Texture2D` selector |
| Mux Array | `node.mux_array` | 8-way `Array<T>` selector |

### 3.18 Native / FFI / host-side sources

These wrap native plugins, CPU work, or background workers as primitives.

| Display Name | Type ID | Purpose |
|---|---|---|
| Depth Estimate (MiDaS) | `node.depth_estimate_midas` | Monocular depth via FFI plugin — background worker, ~2-3 frame latency |
| Blob Detect (FFI) | `node.blob_detect_ffi` | Sparse blob detection — emits `Array<Blob>` |
| Blob Overlay Render | `node.blob_overlay_render` | Draws blob bounding boxes |
| Optical Flow | `node.optical_flow_estimate` | Per-pixel optical flow vectors |
| Image Folder | `node.image_folder` | Scrub through a folder of images via a position scalar |
| Render Text | `node.render_text` | CoreText glyph rasterizer wrapped as a primitive — composite a text string into the output with position / scale / aspect / alignment |
| Auto Gain Apply | `node.auto_gain_apply` | GPU side of AutoGain — pairs with the CPU envelope follower |

### 3.19 WGSL escape hatches

Reserved for genuinely irreducible kernels (see DECOMPOSING_GENERATORS §5 before reaching).

| Display Name | Type ID | Inputs → Outputs |
|---|---|---|
| WGSL Compute (0→1) | `node.wgsl_compute_0in_1tex` | () → Texture2D |
| WGSL Compute (1→1) | `node.wgsl_compute_1tex_1tex` | (Texture2D) → Texture2D |
| WGSL Compute (2→1) | `node.wgsl_compute_2tex_1tex` | (Texture2D, Texture2D) → Texture2D |

---

## 4. Effects — named visual looks

24 entries shipping as nodes in the effect palette. Implementation kind: **shader** (one WGSL kernel), **preset** (thin atom wrap), **composite** (multi-pass primitive — bundle awaiting atomization), **bundle** (fused legacy `PostProcessEffect` wrapped by the primitive — *all bundles are decomposition targets under the no-fused-monolith rule, see `docs/PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md`*).

| # | Display Name | Type ID | Impl |
|---|---|---|---|
| 1 | Auto Gain | `node.auto_gain` | bundle (target — luminance + envelope_follower_ar + gain + character_color) |
| 2 | Blob Track | `node.blob_track` | bundle (target — blob_detect_ffi + one_euro_filter + blob_overlay_render) |
| 3 | Bloom | `node.bloom` | composite (target — mip_chain + separable_gaussian + mix) |
| 4 | Chromatic Aberration | `node.chromatic_aberration` | shader |
| 5 | Color Grade | `node.color_grade` | shader |
| 6 | Depth of Field | `node.depth_of_field` | bundle (target — depth_estimate_midas / tilt_shift_mask / radial_mask + CoC + separable_gaussian + composite) |
| 7 | Dither | `node.dither` | shader |
| 8 | Edge Detect | `node.edge_detect` | shader |
| 9 | Edge Stretch | `node.edge_stretch` | shader |
| 10 | Glitch | `node.glitch` | shader |
| 11 | Halation | `node.halation` | composite (target — shares Bloom's atom set + spectral kernel shift) |
| 12 | Highlight Boost | `node.highlight_boost` | shader |
| 13 | Infrared | `node.infrared` | shader (atomization candidate — single-pass, decomposable into existing atoms) |
| 14 | Invert | `node.invert` | shader |
| 15 | Kaleidoscope | `node.kaleidoscope` | shader |
| 16 | Mirror | `node.mirror` | preset (one `transform` atom, fold=X) |
| 17 | Quad Mirror | `node.quad_mirror` | shader (atomization candidate — single-pass, decomposable) |
| 18 | Soft Focus | `node.soft_focus` | preset (one `gaussian_blur` atom) |
| 19 | Strobe | `node.strobe` | shader |
| 20 | Stylized Feedback | `node.stylized_feedback` | preset (one `feedback` atom) |
| 21 | Transform | `node.transform_effect` | shader (legacy semantics; the `transform` atom is the generic variant) |
| 22 | Voronoi Prism | `node.voronoi_prism` | shader |
| 23 | Watercolor | `node.watercolor` | composite (target — flow_field_noise + uv_displace_by_flow + existing atoms) |
| 24 | Wireframe Depth | `node.wireframe_depth` | bundle (target — depth_estimate_midas + edge_detect + wireframe primitives) |

Note: the six bundles plus Infrared / Quad Mirror are decomposition targets, not permanent. The no-fused-monolith rule (`CLAUDE.md` hard rules) requires every effect to be a graph of single-purpose primitives, including DNN / FFI / CPU work. The DNN and FFI atoms (`depth_estimate_midas`, `blob_detect_ffi`, `blob_overlay_render`, `optical_flow_estimate`, `envelope_follower_ar`) already exist as registered primitives — they're starving on the shelf because the bundles internalize their work. Decomposition activates them.

---

## 5. Effect presets

JSON files at [`assets/effect-presets/`](../crates/manifold-renderer/assets/effect-presets/). Most are 1-node thin wraps. The non-trivial multi-atom compositions are noted.

| Preset | Shape |
|---|---|
| AutoGain | thin wrap |
| BlobTracking | thin wrap |
| Bloom | thin wrap |
| ChromaticAberration | thin wrap |
| **ColorCompass** | 4× `color_sample` → `math` → `smoothing` → `affine_transform` — texture-to-scalar bridge closing the loop into image transform |
| ColorGrade | thin wrap |
| DepthOfField | thin wrap |
| Dither | thin wrap |
| **EdgeGlow** | `edge_detect` standalone |
| EdgeStretch | thin wrap |
| **EdgeStretchByColor** | `chroma_key` → `edge_stretch` → `masked_mix` — apply an effect only where a colour matches |
| Glitch | thin wrap |
| Halation | thin wrap |
| HdrBoost | thin wrap |
| Infrared | thin wrap |
| InvertColors | thin wrap |
| Kaleidoscope | thin wrap |
| **Mandala** | `kaleidoscope` → `feedback` → `affine_transform` → `gain` → `vignette` → `mix` → `chromatic_aberration` — multi-atom user-style composite |
| Mirror | thin wrap (atom preset) |
| NodeGraphTest | test fixture |
| QuadMirror | thin wrap |
| **SmearMosh** | `feedback` + `gain` + `vignette` + `masked_mix` driven by `luminance` → `smoothing` — feedback gated by image brightness |
| SoftFocusGraph | `blur` + `mix` (atom preset) |
| Strobe | thin wrap |
| StylizedFeedback | thin wrap (atom preset) |
| Transform | thin wrap |
| VoronoiPrism | thin wrap |
| Watercolor | thin wrap |
| WireframeDepth | thin wrap |

---

## 6. Generators

All shipping generators are JSON-defined sub-graphs at [`assets/generator-presets/`](../crates/manifold-renderer/assets/generator-presets/), running from `system.generator_input` to `system.final_output`. Zero `inventory::submit!` generators remain; [`crates/manifold-renderer/src/generators/`](../crates/manifold-renderer/src/generators/) is now runtime infrastructure only (loader, registry, mesh/line pipelines, math, stateful base).

### 6.1 JSON-defined

| Preset | Topology shape |
|---|---|
| BasicShapes | trigger-cycled SDF shapes, atomized: `clip_trigger_index` (variant cycle, modulus mux'd 3/6/3 on fill) + `math(Modulo/Divide/Floor)` derive `shape_idx`/`rot_step`/`is_wireframe`; 8-row `mux_scalar` table → signed rotation snap; `trigger_ease_to(window_beats=0.25)` glides between snaps over a quarter beat; three `basic_shape` instances (Square / Diamond / Octagon) → `mux_texture` selected by shape_idx. Shape selection is graph-visible; rotation-easing atom is generic (any snap-on-trigger glide). |
| BlackHole | Kerr black hole with relativistic geodesic lensing: 4× `wgsl_compute` (deflection bake → 3 tex out; Schwarzschild orbit integrator with aliased `Array<Particle>`; polar+hemisphere particle splat with dual atomic accums; cinematic compositor reading deflection + polar density + sky) + `seed_particles` (active_count=0 → simulate self-seeds) + `resolve_accumulator` ×2 + `gaussian_blur` ×10 (deflection H/V ×3 + polar density H/V ×2) + `affine_scalar` ×2 (deg→rad) + `math` (Reciprocal for scale→uv_scale). First consumer of the naga-introspected dynamic escape hatch. |
| ComputeStrangeAttractor | particle sim, atomized onto `wgsl_compute`: `seed_particles(OnceOnReset) → wgsl_compute(attractor_simulate — switch on attractor_type for Lorenz/Rössler/Aizawa/Thomas/Halvorsen, RK2 substeps + first-frame init/warmup + NaN guard, integrate + project bundled in one dispatch) → array_diffuse_particles → scatter_particles(Discard) → resolve_accumulator → reinhard_tone_map`. Adding a new attractor is a JSON edit (append a `case` to the switch + entries to the per-attractor center/scale/dt tables). clip_trigger via `clip_trigger_cycle` + `mux_scalar` (manual vs trigger-driven). Brightness compensated by canvas_area_scale. |
| ConcentricTunnel | mux'd polygon + ring stacker, fully atomized: `mux_scalar` ×many (N selection + trigger-mode gating + cycle [3,4,5,6,8,12]) → `generate_range(end_inclusive=false, active_count=N)` → `array_math(Cos/Sin + ScaleOffset)` ×4 → `pack_curve_xy(scale=4.0 cancels PROJ_SCALE)` → outline; `consecutive_edges(closed=true, count=N)` → edges; per-ring scales via `generate_range(0..15) → math(Floor/Sub/Mul)` + `array_math(ScaleOffset)` → `array_replicate_polyline_rings` → `render_lines`. Polygon math is graph-visible; the shipped atoms are reusable for any closed parametric curve. |
| DigitalPlants | instanced 3D mesh with procedural layout: `grid_uv_field` → `simplex_per_instance` + `fbm_per_instance` → `cylinder_wrap_field` / `torus_wrap_field` → instance jitters → `neighbor_smooth` → `digital_plants_render` |
| Duocylinder | 4D parametric-surface graph: `generate_grid_uv(n=24, [0,TAU)²) → array_math(Cos|Sin) × 4 axes → array_math(ScaleOffset, 0.176776695) × 4 → pack_vec4 → rotate_4d → project_4d → render_lines`; `edges_from_grid_uv(n=24)` wires the u/v-wrap topology into `render_lines.edges`. The `generate_grid_uv` + `array_math` + `pack_vec4` + `edges_from_grid_uv` family authors any (u, v)-parametric surface without a per-shape Rust atom |
| FluidSim2D | particle fluid sim: `fluid_seed` → `fluid_simulate` → `scatter_particles` → `resolve_accumulator` → `feedback` → `downsample` → `gaussian_blur` ×4 → `fluid_gradient_rotate` → `reinhard_tone_map` |
| FluidSim3D | volumetric particle fluid sim: `fluid_seed_3d` → `scatter_particles_3d` → `resolve_3d_accumulator` → `blur_3d_separable` ×3 (density) → `fluid_gradient_curl_3d` → `blur_3d_separable` ×3 (field) → `fluid_simulate_3d` → `scatter_particles_camera` → `resolve_accumulator` → `reinhard_tone_map`, with `camera_orbit` + `inject_burst` + `clip_trigger_cycle` drivers |
| Lissajous | parametric curve, fully atomized: `lfo` ×3 + `frequency_ratio` + `mux_scalar` ×2 → per-axis `math(Floor/Ceil/Subtract)` bracket + `generate_range` → `array_math(ScaleOffset+Sin)` ×4 + `array_math(Mix)` ×2 → `pack_curve_xy` → `render_lines`. The TouchDesigner Pattern→Math→Function→Merge→To-SOP shape; bracket-interp is graph-visible. |
| MetallicGlass | feedback-displacement metallic surface, fully atomized: `simplex_field_2d` + `scale_offset` → `feedback` ping-pong with `mix Difference`+`mix Lerp 0.98` → `gaussian_blur` H/V → split into (height/levels chain) and (`mirror_axis`+`convolution_2d_9tap`×2+`pack_channels`+`length_vec2` Sobel chain). Geometry: `generate_grid_mesh` → `displace_mesh(height=height_levels)` → `triangulate_grid` → `render_3d_mesh` (G-buffer mode — world_pos consumed downstream, color path unused). Shading: `gain(height × displace) → heightmap_to_normal(coord_space=WorldYUp, aspect=system.aspect)` → surface normal; `scale_offset_texture(edge, scale=0.15, offset=0.05)` → roughness map. `cook_torrance_specular(normal, world_pos, roughness_map, view=cam.pos, light=fixed_pos, attenuation_scale=25)` + `equirect_envmap_sample(normal, env_map=bake_equirect_envmap, world_pos, roughness_map, view=cam.pos)` → `mix mode=Add` → `reinhard_tone_map`. Activates the PBR-on-3D-mesh atom family (`render_3d_mesh.world_pos/normal`, `cook_torrance` + `equirect` in 3D-mesh mode, `heightmap_to_normal` WorldYUp, `camera_orbit.pos_xyz`) — reusable for any future perspective-correct PBR generator. |
| MriVolume | volumetric scrubbing: `image_folder` ×3 → `mux_texture` → `sharpen` → `smoothstep_texture` → `invert` |
| ParticleText | FluidSim2D base + text-force branch (`render_text → gaussian_blur H+V → gradient_central_diff → rotate_vec2_by_angle → gain → blend Add into the force chain`). The glyphs are baked into the force field as a perpendicular-curl flow, particles continuously stream along the text shape instead of being seeded at it |
| NestedCubes | instanced mesh with cycled poses: `trigger_gate` → `scalar_array_accumulator` → `cycle_table_row` → `mux_array` → `nested_cubes_geometry` |
| OilyFluid | screen-space fluid + atomized PBR: `feedback` ×2 + gradient atoms + `texture_advect` + `simplex_field_2d` → `heightmap_to_normal` → `lambert_directional` + `matcap_two_tone` + `fresnel_rim` + `blinn_specular` summed via `mix` |
| Plasma | single curated family primitive: `plasma_pattern_2d` |
| StarField | fully atomized: `system.generator_input.time` → `math` ×3 (drift_t → offset_x/y) → `voronoi_2d` (per-cell distance + cell_hash on A) → (`scale_offset_texture` invert + `power_texture` spike) || (`channel_mix` A→R cell_hash → `smoothstep_texture` density mask + `scale_offset` ×2 freq/phase tables) → `mix Multiply` core × mask → `trig_texture` (per-pixel sin via `freq_tex`/`phase_tex` shadows) → `scale_offset` to [0,1] → `mix Multiply` apply twinkle → `scale_offset` brightness. Single-layer (cinematic 4-layer parallax dropped; revivable by duplicating the inner chain and `mix Add`-summing). Per-star unique twinkle preserved via the trig_texture texture-shadow extension. Activates `voronoi_2d` cell_hash on A + `channel_mix` GPU shader (was no-op stub) + `trig_texture.freq_tex`/`phase_tex` shadows. |
| Tesseract | 4D wireframe: `generate_tesseract_vertices` → `rotate_4d` → `project_4d` → `render_lines` |
| Text | single-primitive wrap of the CoreText glyph rasterizer: `node.render_text` |
| TrivialPassthrough | smoke test: `uv_field` |
| WireframeZoo | 3D wireframe (atom-decomposed): `clip_trigger_cycle` + `value` → `mux_scalar` → (`polytope_vertices` + `polytope_edges`) → `rotate_3d` → `project_3d` → `render_lines` |

### 6.2 Rust-defined

Empty. The migration completed in May 2026 — see [GENERATOR_DECOMPOSITION_PLAN.md](GENERATOR_DECOMPOSITION_PLAN.md) for the per-generator history.

---

## 7. Keeping this catalog honest

- After adding a new primitive: add a row to §3 under the right family and bump nothing else; the AI agent reads §3 to know what's available.
- After adding a new preset: add a row to §5 or §6.1 with the topology shape; downstream readers learn the analogue from this entry.
- After deleting a primitive: remove the row; don't leave it as "deprecated."
- Validate by running `cargo run -p manifold-renderer --bin check-presets` (loads + compiles every preset, sub-second, no GPU); a green run means every primitive referenced by every preset is registered.
