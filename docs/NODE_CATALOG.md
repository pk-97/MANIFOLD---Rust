# Node Catalog

**Source of truth for what nodes exist.** Regenerate this file by walking [`crates/manifold-renderer/src/node_graph/primitives/`](../crates/manifold-renderer/src/node_graph/primitives/) (one `type_id` per primitive — `pub const *_TYPE_ID` for the composite-effect primitives, `type_id: "node.…"` for the macro-defined atoms) and the two preset directories ([`effect-presets/`](../crates/manifold-renderer/assets/effect-presets/), [`generator-presets/`](../crates/manifold-renderer/assets/generator-presets/)). If you add a primitive or a preset and don't update this catalog, the catalog is stale — fix it.

For *how* to compose these into a generator decomposition, see [DECOMPOSING_GENERATORS.md](DECOMPOSING_GENERATORS.md). For the design rationale behind the primitive shape, see [PRIMITIVE_LIBRARY_DESIGN.md](PRIMITIVE_LIBRARY_DESIGN.md).

---

## 1. Invariants

- **Type IDs are flat:** `node.<name>` for atoms and effects, `system.<name>` for boundary nodes. No category prefix — the category is presentation, not identity.
- **All primitives share one `Primitive` trait.** "Atom" vs "Effect" is how the palette groups them, not a structural split.
- **Port-shadows-param:** any scalar input port whose name matches a primitive `ParamDef` uses the wire when present and the param as the fallback. Standard for `gain`, `amount`, `rotation`, `wet_dry`, control-rate modulation everywhere.
- **Effects are JSON presets composed of atoms** (decomposed graphs in `effect-presets/`), drillable in the editor — not monolithic palette nodes. The fused effect-monolith bundles were deleted 2026-05-30; the lone surviving legacy `PostProcessEffect` wrapper is `node.wireframe_depth` (decomposition in flight). Preset names stay stable across implementation swaps so save files don't break.
- **Array port wires carry a Channels signature.** The macro syntax in this catalog uses `Array(T)` for typed families that have a `KnownItem` impl (`Particle`, `MeshVertex`, `EdgePair`, etc.) — equivalent to `Channels<T>`, both expand to an `ArrayType::of_known::<T>()` whose `specs` field carries the canonical channel list. For ad-hoc shapes, the inline form is `Channels[name: Type, ...]`. See [CHANNEL_TYPE_SYSTEM.md](CHANNEL_TYPE_SYSTEM.md) §4.1 for the type contract and §7 for the `well_known` channel-name registry that the canonical names resolve through.
- **`Array<T>` / `Channels<T>` outputs declare capacity** via `EffectNode::array_output_capacity`; the CI sweep test enforces this on the registry. Outputs also declare a non-empty Channels signature (either through `KnownItem::SPECS` or inline `Channels[...]`); the `every_conventional_array_port_declares_a_channels_signature` invariant gates that.
- **Stateful primitives declare** `state_lifecycle` + `state_capture_input_ports` so the StateStore knows where to break cycles. Per-port, not per-node.

### 1.1 Well-known channel-name registry (one-line overview)

Channels signatures reference `crate::node_graph::channel_names::well_known::*` constants for canonical names — `POSITION`, `VELOCITY`, `NORMAL`, `UV`, `WIDTH`, `HEIGHT`, `X`, `Y`, `Z`, `W`, `R`, `G`, `B`, `A`, `COLOR`, `A_INDEX`, `B_INDEX`, `LIFE`, `AGE`, `SEED`, `POS_SCALE`, `ROT`, `VALUE`, `T`, `INDEX`, `MAGNITUDE`, `PHASE`, `FREQ`, `CONFIDENCE`, `WEIGHT`. Adding a new canonical name: append one line inside the `well_known_channels!` macro invocation in [`crates/manifold-renderer/src/node_graph/channel_names.rs`](../crates/manifold-renderer/src/node_graph/channel_names.rs); the constant declaration and the collision-check test are generated from the same source list. Non-canonical names (one-off shapes, `wgsl_compute` shader field names) declare via inline string literals or naga's field-name walk and live in the runtime registry — they display correctly in editor tooltips, just aren't part of the canonical vocabulary.

---

## 2. Boundary nodes

| Display Name | Type ID | Inputs → Outputs | Purpose |
|---|---|---|---|
| Source | `system.source` | () → (Texture2D) | Effect-chain input — host pre-binds the upstream texture |
| Generator Input | `system.generator_input` | () → (time, beat, aspect, trigger_count, anim_progress) | Generator graph entry — scalar frame context |
| Final Output | `system.final_output` | (Texture2D) → () | Both surfaces — host pre-binds the chain / generator output texture |

---

## Registered node index (generated — authoritative)

This block is **generated from the node registry** by `gen_node_catalog` (`cargo run -p manifold-renderer --bin gen_node_catalog`) and is the drift-guarded source of truth for *what exists* — a registry change that isn't reflected here fails `cargo test`. The hand-curated "Atoms by intent" grouping below (§3) adds human structure and prose; once `category` / `role` are filled across the library, that grouping regenerates from those fields too. The full machine artifact — ports, params, complete descriptions, for the AI composition surface — is [`node_catalog.json`](node_catalog.json).

<!-- BEGIN GENERATED: registered-node-index — do not edit; run `cargo run -p manifold-renderer --bin gen_node_catalog` -->

_Generated from the node registry — do not hand-edit. 203 nodes registered. `category` / `role` are filled incrementally by the naming pass; blank shows as `—`._

### Atoms (162)

| type_id | label | category | role | purpose |
|---|---|---|---|---|
| `node.abs_texture` | Abs Texture | — | — | Per-pixel abs(input.rgb). |
| `node.anti_clump_particles` | Anti-Clump Particles | — | — | Modulator-weighted Brownian kick on each live particle's position.xy. |
| `node.apply_radial_burst_3d_to_particles` | Apply Radial Burst 3D (Particles) | — | — | Per-particle 3D injection burst around one of four hardcoded tetrahedron-vertex zones. |
| `node.apply_radial_burst_to_particles` | Apply Radial Burst (Particles) | — | — | Per-particle radial impulse around `(point_x, point_y)` — evaluates the radial + tangent + noise-perturbed-radial + falloff math at each particle's exact UV an… |
| `node.array_diffuse_particles` | Diffuse Particles | — | — | Apply a per-particle hash-based random kick to `Particle.velocity`. |
| `node.array_feedback` | Array Feedback | — | — | One-frame delay for Array<Particle>: this frame's input becomes next frame's output. |
| `node.array_math` | Array Math | — | — | Element-wise math over Array<f32>. |
| `node.array_replicate_polyline_rings` | Replicate Polyline Rings | — | — | Stack K transformed copies of a polyline (outline + edge topology) into one concatenated polyline. |
| `node.array_unpack_vec2` | Array Unpack Vec2 | — | — | Split an Array<vec2<f32>> into two Array<f32>s, one per component (`x`, `y`). |
| `node.bake_equirect_envmap` | Bake Equirect Envmap | — | — | Procedurally bake an HDR studio environment map at the given resolution. |
| `node.basic_shape` | Basic Shape | — | — | Single-dispatch 2D SDF shape — Square / Diamond / Octagon — rasterised into an RGBA16F texture with anti-aliased edges. |
| `node.blinn_specular` | Blinn Specular | — | — | Blinn-Phong specular from a tangent-space normal map + directional light + view: `h = normalize(light + view); spec = pow(max(dot(n, h), 0), power)`. |
| `node.blob_detect_ffi` | Blob Detect (FFI) | — | — | Sparse blob detection (bright-region tracking) via the manifold_native BlobDetector FFI plugin. |
| `node.blob_overlay_render` | Blob Overlay | — | — | Draw hollow rectangles around each blob in an Array<Blob> on top of a source Texture2D. |
| `node.block_displace_field` | Block Displace Field | — | — | Generator for a per-block random UV-offset field (the datamosh / block-glitch building block). |
| `node.blur` | Blur | — | — | Separable Gaussian blur — a horizontal then a vertical pass through a per-instance ping-pong texture. |
| `node.blur_3d_separable` | Blur 3D Separable | — | — | Single-axis separable Gaussian blur on a Texture3D. |
| `node.box_mask` | Box Mask | — | — | Rotated rectangular SDF mask (Chebyshev distance). |
| `node.brightness` | Brightness | — | — | Pixel-local brightness multiply: out.rgb = in.rgb * brightness; alpha passes through. |
| `node.cel_material` | Cel Material | — | — | Cel-shaded material — Lambert N·L quantized into `cel_bands` discrete bands. |
| `node.centered_uv` | Centered UV | — | — | UV recentered around (cx, cy) with per-axis scale. |
| `node.channel_mix` | Channel Mix | — | — | 4×4 RGBA matrix transform — each output channel is a weighted sum of the input RGBA plus a constant. |
| `node.checkerboard` | Checkerboard | — | — | Pure generator. |
| `node.chromatic_displace` | Chromatic Displace | — | — | 3-tap RGB sample of `in` displaced by `velocity` (RG). |
| `node.clamp_texture` | Clamp | — | — | Per-pixel clamp on RGB: out.rgb = clamp(in.rgb, min, max). |
| `node.clip_trigger_index` | Clip Trigger Index | — | — | Emit `trigger_count % modulus` as a scalar via the idempotence-safe ClipTriggerCycle gate. |
| `node.color_ramp` | Color Ramp | — | — | Map a scalar / luma input through a two-stop colour gradient (Color A → Color B). |
| `node.colorize` | Colorize | — | — | Tint an image toward a hue, masked per-pixel by (brightness × neutrality × focus): a selective colorize/duotone toward highlights. |
| `node.consecutive_edges` | Consecutive Edges | — | — | Generate consecutive-pair edge topology [(0,1), (1,2), …, (N-2, N-1)] from a vertex count, optionally closed via (N-1, 0). |
| `node.container_bounds_3d` | Container Bounds 3D | — | — | Post-integration hard containment for 3D particles: toroidal wrap (container = None) or SDF reflect + clamp (Cube/Sphere/Torus). |
| `node.container_repel_force_3d` | Container Repel Force 3D | — | — | Soft container-boundary repulsion added in-place to an Array<[f32; 3]> force buffer. |
| `node.contrast` | Contrast | — | — | Pivot-around-0.5 contrast: out = (c - 0.5) * contrast + 0.5. |
| `node.convolution_2d_9tap` | Convolution 2D (9-tap) | — | — | General 3×3 non-separable convolution with a user-supplied kernel (9 float weights k0..k8 in row-major order, k4 = center). |
| `node.curl_slope_force_3d` | Curl + Slope Force 3D | — | — | Combine a vec3 gradient Texture3D into a force field: cross the gradient with a unit reference axis for curl (tangential orbit around density peaks) and add th… |
| `node.cylinder_wrap_field` | Cylinder Wrap Field | — | — | Lift an Array<vec2<f32>> of UVs onto a cylindrical surface and emit Array<InstanceTransform>. |
| `node.depth_estimate_midas` | MiDaS Depth | — | — | MiDaS monocular depth estimation via FFI native plugin, wrapped as a primitive. |
| `node.diffuse_force_3d_at_particles` | Diffuse Force 3D at Particles | — | — | Per-particle incoherent 3D random kick added in-place to an Array<[f32; 3]> force buffer, weighted by local density. |
| `node.digital_plants_render` | Digital Plants Render | — | — | Fused two-pass DigitalPlants renderer: shadow pass (depth-only from light POV) into an internal shadow map, then main pass with instanced cel-shaded cubes + 5-… |
| `node.displace_mesh` | Displace Mesh | — | — | Perturb the Y component of an Array<MeshVertex> positions grid by sampling a height Texture2D at each vertex's UV. |
| `node.distance_to_point` | Distance to Point | — | — | Pure generator. |
| `node.dither` | Dither | — | — | Luminance-preserving dither quantize driven by an external threshold pattern. |
| `node.dither_pattern` | Dither Pattern | — | — | Pure generator. |
| `node.downsample` | Downsample | — | — | Integer-factor (2x / 4x / 8x) box-filter downsample of a Texture2D. |
| `node.edges_from_grid_uv` | Edges From Grid UV | — | — | Emit the u-wrap + v-wrap wireframe edge topology for an n × n parametric grid as Array<EdgePair>. |
| `node.ellipse_mask` | Ellipse Mask | — | — | Rotated elliptical SDF mask. |
| `node.euler_step_particles` | Euler Step Particles | — | — | Apply one Euler integration step to each live particle's position.xy by a per-particle 2D force. |
| `node.euler_step_particles_3d` | Euler Step Particles 3D | — | — | Apply one Euler integration step to each live particle's position.xyz by a per-particle 3D force. |
| `node.fbm_2d` | fBM 2D | — | — | Pure generator. |
| `node.fbm_per_instance` | FBM Per Instance | — | — | Sample fractal Brownian motion (multi-octave 3D simplex) at each UV in an Array<vec2<f32>>, emit Array<f32>. |
| `node.feedback` | Feedback | — | — | 1-frame texture delay. |
| `node.field_combine` | Field Combine | — | — | Per-pixel scalar field: out.rgb = a * in.r + b * in.g + c, alpha = 1. |
| `node.film_grain` | Film Grain | — | — | Multiplicative white-noise grain: out.rgb = src.rgb * (1 - amount * (1 - white_noise(pixel))). |
| `node.flash` | Flash | — | — | Modulate image brightness by a scalar `amount` in one of three modes: Opacity (col*(1-amount), toward black), White (mix toward white), Gain (col*mix(1,3,amoun… |
| `node.flatten_to_camera_plane` | Flatten to Camera Plane | — | — | Compress particles toward the camera viewing plane. |
| `node.flow_field_noise` | Flow Field Noise | — | — | Generate a 2D flow vector field from domain-warped fBM Perlin noise. |
| `node.fract_texture` | Fract Texture | — | — | Per-pixel fract(input.rgb * scale). |
| `node.fresnel_rim` | Fresnel Rim | — | — | Fresnel-based edge highlight from a tangent-space normal map: `f = pow(1 - max(dot(n, view), 0), power)`, output = color.rgb * f. |
| `node.gain` | Gain | — | — | Multiply the input texture's RGB by a scalar gain. |
| `node.gaussian_blur` | Gaussian Blur | — | — | Single-axis Gaussian blur. |
| `node.gaussian_blur_variable_width` | Gaussian Blur (Variable Width) | — | — | Separable Gaussian blur where the per-pixel kernel width is sampled from a `width` Texture2D's R channel. |
| `node.generate_cube_mesh` | Generate Cube Mesh | — | — | Emit a unit cube as 36 triangle-list MeshVertex entries (6 faces × 2 triangles × 3 vertices) with per-face outward normals. |
| `node.generate_grid_mesh` | Generate Grid Mesh | — | — | Emit a regular NxM grid of MeshVertex items in the XZ plane, sized in world units. |
| `node.generate_grid_uv` | Generate Grid UV | — | — | Emit two Array<f32> outputs (u_values, v_values) sampling a 2D parameter domain [0, u_max) × [0, v_max) at grid_size steps along each axis, flattened to grid_s… |
| `node.generate_instance_transforms` | Generate Instance Transforms | — | — | Emit an Array<InstanceTransform> filled with a procedural layout (grid / ring / spiral / random). |
| `node.generate_range` | Generate Range | — | — | Emit an Array<f32> of `count` samples linearly spaced over `[start, end]`. |
| `node.generate_tesseract_vertices` | Generate Tesseract Vertices | — | — | Emit the 16 corner vertices of a 4D hypercube (tesseract) scaled to magnitude 0.25 plus its 32-edge wireframe topology as paired Array<Vec4Vertex> + Array<Edge… |
| `node.gradient_central_diff` | Gradient (Central Diff) | — | — | Per-pixel central-difference gradient of a single input channel. |
| `node.gradient_central_diff_3d` | Gradient (Central Diff 3D) | — | — | 6-tap central-difference gradient of a scalar density Texture3D, written as a vec3 Texture3D. |
| `node.gradient_ramp` | Gradient Ramp | — | — | General N-stop gradient / LUT generator. |
| `node.grid_uv_field` | Grid UV Field | — | — | Emit an Array<vec2<f32>> of UV positions on an N×N grid in [0,1]² space, sampling each cell at its centre: for idx = row*N + col, uv = ((col+0.5)/N, (row+0.5)/… |
| `node.hash_field_by_seed` | Hash Field by Seed | — | — | Hash an input value-field's RG channels with an added scalar seed: seeded = field.rg + seed·(seed_x, seed_y); Hash2 (mode 0) → out.rg = hash2(seeded) in [0,1]^… |
| `node.hash_noise_field_2d` | Hash Noise Field 2D | — | — | Pure generator. |
| `node.hdr_retention_mix` | HDR Retention Mix | — | — | Preserve a reference texture's above-1.0 highlight energy through a compressed texture's gain adjustment. |
| `node.heightmap_to_normal` | Heightmap → Normal | — | — | Scalar height field (read from `in.r`) → unit normal map (RGB) via central-difference gradient. |
| `node.hue_saturation` | Hue / Saturation | — | — | HSV colour adjust: rotate hue (degrees), scale saturation, scale value. |
| `node.image_folder` | Image Folder | — | — | Scrub through a folder of images via a position scalar (0..1). |
| `node.instance_position_jitter` | Instance Position Jitter | — | — | Add 3-axis 3D-simplex position noise to each InstanceTransform's pos.xyz, leaving scale and rotation unchanged. |
| `node.instance_rotation_jitter` | Instance Rotation Jitter | — | — | Add hash-driven per-instance Euler-rotation jitter to each InstanceTransform's rot_pad.xyz; positions and scale pass through. |
| `node.lambert_directional` | Lambert (Directional) | — | — | Lambert (diffuse) shading from a tangent-space normal map and a directional light: `out = max(dot(n, normalize(light_dir)), 0) * (1-ambient) + ambient`, multip… |
| `node.length_vec2` | Length (vec2) | — | — | Per-pixel `length(in.rg)` as a scalar field in the R channel (GBA = 0, 0, 1). |
| `node.lerp_instance_fields` | Lerp Instance Fields | — | — | Elementwise linear interpolation between two Array<InstanceTransform>s. |
| `node.levels` | Levels | — | — | Fused tone-shape atom: out.rgb = pow(clamp(in.rgb * scale + offset, lo, hi), gamma). |
| `node.lic_integrate` | LIC Integrate | — | — | Line Integral Convolution. |
| `node.linear_gradient` | Linear Gradient | — | — | Directional 0→1 ramp in UV space. |
| `node.matcap_two_tone` | Matcap Two-Tone | — | — | Cross-axis 4-colour matcap from a tangent-space normal map. |
| `node.mirror_axis` | Mirror Axis | — | — | Sample input at UVs mirrored across a line through center at `angle` radians. |
| `node.mirror_fold_uv` | Mirror Fold UV | — | — | Mirror/fold coordinate generator: rewrites the per-pixel UV via an axis flip or kaleidoscope-style fold (Identity / Mirror / MirrorX / MirrorY / FlipY / QuadMi… |
| `node.mix` | Mix | — | — | Combine two textures with one of 8 blend modes (Lerp, Screen, Add, Max, Multiply, Difference, Overlay, Divide), crossfaded back against A by `amount`. |
| `node.mux_array` | Mux (array) | — | — | N-way Array<f32> selector. |
| `node.mux_scalar` | Mux (scalar) | — | — | N-way scalar selector. |
| `node.mux_texture` | Mux (texture) | — | — | Dynamic N-way Texture2D selector — `num_inputs` sets how many in_0..in_N ports exist and a rounded, clamped `selector` forwards the matching input. |
| `node.neighbor_smooth` | Neighbor Smooth | — | — | 5-point cross-neighborhood smoothing of an Array<InstanceTransform> arranged as an NxN grid. |
| `node.nested_cubes_geometry` | Nested Cubes Geometry | — | — | Render a 5-instance gap-face cube field with EMA-smoothed per-instance Y rotation, per-face scatter, and a per-face envelope-driven kick on each trigger. |
| `node.normalize_vec2` | Normalize Vec2 | — | — | Per-pixel safe-normalize of the input's RG channels treated as a vec2. |
| `node.optical_flow_estimate` | Optical Flow | — | — | Dense optical flow (Farneback + global motion compensation) via the MiDaS native plugin. |
| `node.pack_channels` | Pack RGBA | — | — | Pack four single-channel textures into one RGBA output by reading the R channel of each input into the matching output channel. |
| `node.pack_curve_xy` | Pack Curve XY | — | — | Combine two Array<f32> (x channel, y channel) into one Array<CurvePoint>. |
| `node.pack_vec4` | Pack Vec4 | — | — | Combine four Array<f32> (x, y, z, w channels) into one Array<Vec4Vertex>. |
| `node.pbr_material` | PBR Material | — | — | Cook-Torrance microfacet PBR (D_GGX × G_Smith × F_Schlick) + IBL reflection material. |
| `node.perlin_noise_2d` | Perlin Noise 2D | — | — | Pure generator. |
| `node.person_segment` | Person Segment | — | — | Person / human segmentation via the native plugin's process_subject_mask API. |
| `node.phong_material` | Phong Material | — | — | Lambert diffuse + Blinn-Phong specular material. |
| `node.polar_field` | Polar Field | — | — | Pure generator. |
| `node.polytope_edges` | Polytope Edges | — | — | Emit the wireframe edge topology of one of the five Platonic solids as Array<EdgePair>. |
| `node.polytope_vertices` | Polytope Vertices | — | — | Emit the vertex set of one of the five Platonic solids (Tetrahedron / Cube / Octahedron / Icosahedron / Dodecahedron) as Array<MeshVertex>. |
| `node.posterize` | Posterize | — | — | Posterize: quantize each RGB channel to `levels` discrete steps (round to nearest, endpoints included). |
| `node.power_texture` | Power Texture | — | — | Per-pixel pow(max(input.rgb, 0), exponent). |
| `node.project_3d` | Project 3D | — | — | Project an Array<MeshVertex> (3D positions) to an Array<CurvePoint> (2D pre-aspect curve space) with either orthographic or perspective projection. |
| `node.project_4d` | Project 4D | — | — | Project an Array<Vec4Vertex> to Array<CurvePoint> via two-stage perspective (4D → 3D collapse with f = proj_dist / (proj_dist - w), then 3D → 2D with s = proj_… |
| `node.radial_burst_force_field` | Radial Burst Force Field | — | — | Produces a per-pixel vec2 force texture for a radial impulse burst around (point_x, point_y) within `radius`. |
| `node.radial_fold_uv` | Radial Fold UV | — | — | Kaleidoscope coordinate generator: folds the plane into `segments` mirrored wedges around (cx, cy) and emits the per-pixel sample UV (R = folded_u, G = folded_… |
| `node.radial_offset_field` | Radial Offset Field | Distort | Map | Directional displacement field generator. |
| `node.reinhard_tone_map` | Reinhard Tone Map | — | — | Reinhard tone mapping for HDR display in one of two curves: Extended (default — `x*(1+x/9)/(1+x)`, matches FluidSim bit-for-bit, preserves highlights) or Simpl… |
| `node.remap` | Remap | — | — | Resample `source` at the per-pixel UV coordinates in `uv_field`'s R/G channels (TouchDesigner's Remap TOP). |
| `node.render_3d_mesh` | Render 3D Mesh | — | — | Bundled 3D mesh renderer (TouchDesigner / Blender shape). |
| `node.render_filled_rects` | Filled Rects | — | — | Instanced filled-rectangle overlay composited onto a source texture. |
| `node.render_instanced_3d_mesh` | Render Instanced 3D Mesh | — | — | Bundled instanced 3D mesh renderer. |
| `node.render_lines` | Render Lines | — | — | Draw an Array<CurvePoint> as anti-aliased capsule line segments with 4x MSAA and additive blending. |
| `node.render_text` | Render Text | — | — | Render a text string to the output texture. |
| `node.render_value_overlay` | Value Overlay | — | — | Lightweight bitmap-font numeric labels at multiple positions, composited onto a source texture. |
| `node.resolve_3d_accumulator` | Resolve 3D Accumulator | — | — | Read a u32 fixed-point 3D accumulator buffer (produced by node.scatter_particles_3d), divide by 4096 (FluidSim3D's FIXED_POINT_MULTIPLIER), and write the resul… |
| `node.resolve_accumulator` | Resolve Accumulator | — | — | Read a u32 fixed-point accumulator buffer (produced by node.scatter_particles), divide by `fixed_point_scale`, and write the result as a grayscale density text… |
| `node.rotate_2d` | Rotate 2D | — | — | Rotate a 2D coordinate field around the origin by `angle` (radians). |
| `node.rotate_3d` | Rotate 3D | — | — | Apply XYZ Euler rotation to an Array<MeshVertex>. |
| `node.rotate_4d` | Rotate 4D | — | — | Apply 4D rotation (XY, ZW, XW planes) to an Array<Vec4Vertex>. |
| `node.rotate_vec2_by_angle` | Rotate Vec2 (Angle) | — | — | Rotate the input's RG vec2 field by an arbitrary angle (radians) per pixel. |
| `node.sample_texture_3d_at_particles` | Sample Texture 3D at Particles | — | — | Per-particle trilinear sample of a vec3 Texture3D at each particle's position.xyz. |
| `node.sample_texture_at_particles` | Sample Texture at Particles | — | — | Per-particle bilinear sample of a Texture2D at each particle's position.xy. |
| `node.sample_volume_2d` | Sample Volume 2D | — | — | Sample a Texture3D at a fixed Z slice to produce a Texture2D. |
| `node.saturation` | Saturation | Color | Filter | Luma-based saturation: out = mix(vec3(rec709_luma), c, saturation). |
| `node.scale_offset_texture` | Scale + Offset | — | — | Per-pixel affine remap `a * x + b` on RGB. |
| `node.scanline_jitter_field` | Scanline Jitter Field | — | — | Generator for a per-row random horizontal-offset field (the VHS / horizontal-tearing building block). |
| `node.scatter_particles` | Scatter Particles | — | — | Atomic-add splat of particles into a u32 fixed-point accumulator buffer sized to the host's canvas. |
| `node.scatter_particles_3d` | Scatter Particles 3D | — | — | Atomic-add splat of an Array<Particle> into a u32 3D accumulator buffer sized vol_res × vol_res × vol_depth. |
| `node.scatter_particles_camera` | Scatter Particles Camera | — | — | Fused 3D→2D camera projection + atomic-add scatter. |
| `node.seed_particles` | Seed Particles | — | — | Emit a fresh Array<Particle> sized by `max_capacity` (chain-build-time ceiling). |
| `node.seed_particles_from_texture` | Seed Particles From Texture | — | — | Exact-placement particle seeding from a Texture2D density mask. |
| `node.sharpen` | Sharpen | — | — | Single-knob 4-neighbour Laplacian unsharp mask. |
| `node.simplex_field_2d` | Simplex Field 2D | — | — | Pure generator. |
| `node.simplex_noise_2d` | Simplex Noise 2D | — | — | Pure generator. |
| `node.simplex_noise_force_3d_at_particles` | Simplex Noise Force 3D at Particles | — | — | Per-particle 3D simplex noise advection added in-place to an Array<[f32; 3]> force buffer. |
| `node.simplex_noise_force_at_particles` | Simplex Noise Force at Particles | — | — | Per-particle 2D simplex noise force added in-place to an Array<vec2<f32>> force buffer. |
| `node.simplex_per_instance` | Simplex Per Instance | — | — | Sample 3D Ashima simplex noise at each UV in an Array<vec2<f32>>, emit Array<f32>. |
| `node.sin_term` | Projected Sin Term | — | — | Fused linear-projection + sin term: out = sin((a*field.r + b*field.g + c) * freq * freq_scale + time * time_scale). |
| `node.slope_displace` | Slope Displace | — | — | Emboss-style displacement: soft-light-blend `base` over `image`, take the luminance Sobel gradient of the blend at a `step`-pixel offset, and displace `image` … |
| `node.smoothstep_texture` | Smoothstep | — | — | Per-pixel smoothstep contrast curve on RGB, alpha pass-through. |
| `node.texture_advect` | Texture Advect | — | — | Backward (semi-Lagrangian) advection of a texture by a 2D velocity field. |
| `node.texture_sum_5` | Texture Sum 5 | — | — | Per-pixel weighted-sum of five textures: out = (a+b+c+d+e) / divisor. |
| `node.threshold` | Threshold | — | — | Pixel-local luma threshold with a smoothstep falloff of width `softness` — isolates bright regions for bloom / highlight masks. |
| `node.tone_map` | Tone Map | — | — | HDR → display tone mapping with selectable curve (Narkowicz ACES / Hill ACES / AgX / Khronos PBR Neutral) and output mode (SDR / PQ for HDR10 export / EDR for … |
| `node.torus_wrap_field` | Torus Wrap Field | — | — | Lift an Array<vec2<f32>> of UVs onto a torus surface, emit Array<InstanceTransform>. |
| `node.triangulate_grid` | Triangulate Grid | — | — | Convert a positions-only NxM Array<MeshVertex> grid into a triangle-list (N-1)*(M-1)*6 vertex stream with finite-difference normals. |
| `node.trig_texture` | Trig Texture | — | — | Per-pixel trigonometric remap: out = trig_mode(input.rgb * freq + phase). |
| `node.unlit_material` | Unlit Material | — | — | Flat-colour material — no lighting math, no shadow term. |
| `node.uv_displace_by_flow` | UV Displace by Flow | — | — | Sample a source texture at UVs displaced by a 2D flow vector field. |
| `node.uv_field` | UV Field | — | — | Pure generator. |
| `node.uv_strip_clamp` | UV Strip Clamp | — | — | Edge-stretch coordinate generator: clamps the per-pixel UV to a center strip of width `width` on the selected axis (Horiz / Vert / Both) and emits it (R = clam… |
| `node.vignette` | Vignette | — | — | Soft fade-to-black border. |
| `node.voronoi_2d` | Voronoi 2D | Noise | Source | Pure generator. |
| `node.wet_dry` | Wet/Dry | — | — | Crossfade a processed `wet` texture back over the original `dry` texture by a `wet_dry` factor [0,1]. |
| `node.wgsl_compute` | WGSL Compute | — | — | User-authored WGSL compute escape hatch — the shader is the contract: ports, uniform layout, workgroup size, binding map and output formats are all derived fro… |
| `node.wrap_particles_torus` | Wrap Particles (Torus) | — | — | Per-particle toroidal wrap: position.xy = fract(position.xy + 1). |

### Drivers (28)

| type_id | label | category | role | purpose |
|---|---|---|---|---|
| `node.affine_scalar` | Affine Scalar | — | — | Scalar affine remap: out = a * scale + offset. |
| `node.array_connect_nearest` | Connect Nearest | — | — | For each item in a Channels[X, Y, WIDTH, HEIGHT] array, find its nearest neighbour within max_distance and emit an EdgePair (A_INDEX, B_INDEX). |
| `node.beat_gate` | BeatGate | — | — | Beat-synced square gate. |
| `node.beat_ramp` | BeatRamp | — | — | Per-beat attack envelope: out = clamp(fract(beats·rate) / attack, 0, 1). |
| `node.camera_orbit` | Orbit Camera | — | — | Orbit-style perspective camera source. |
| `node.canvas_area_scale` | Canvas Area Scale | — | — | Emit (width * height) / reference_area as a scalar. |
| `node.clip_trigger_cycle` | Clip Trigger Cycle | — | — | Defense-in-depth `trigger_count % modulus` cycle: emits a value in [0, modulus) on each new trigger_count, advancing past would-be repeats so consecutive emiss… |
| `node.color_sample` | ColorSample | — | — | Read a single pixel from the input texture at the configured `uv`. |
| `node.compressor_envelope` | Compressor Envelope | — | — | Audio-compressor envelope path applied to a scalar signal level — log-domain, program-dependent attack/release with ratio compression toward a target; out is a… |
| `node.cycle_table_row` | Cycle Table Row | — | — | Cycle through a curated `Table` of f32 rows on each clip trigger, emitting the selected row as `Array<f32>`. |
| `node.envelope_decay` | Envelope Decay | — | — | Exponential one-shot decay — snaps to 1.0 on each integer-edge change of `trigger`, then decays frame-rate-independently (env *= exp(-decay_rate · dt)). |
| `node.envelope_follower_ar` | Envelope Follower (A/R) | — | — | Asymmetric attack/release envelope follower on a scalar — switches time constant on rising (`attack`) vs falling (`release`) input. |
| `node.frequency_ratio` | Frequency Ratio | — | — | Emit two scalars from a curated table of small-integer harmonic ratios. |
| `node.inject_burst` | Inject Burst | — | — | Fixed-duration burst state machine — on each new `trigger` (when enabled) runs a burst for `duration` seconds emitting active=1, a 0→1 phase ramp, and a stable… |
| `node.lfo` | LFO | — | Control | Low-frequency oscillator. |
| `node.light` | Light | — | — | Single light source for 3D lighting pipelines. |
| `node.luminance` | Luminance | — | — | Average Rec. |
| `node.math` | Math | — | — | Scalar arithmetic. |
| `node.one_euro_filter` | One Euro Filter | — | — | Adaptive temporal low-pass (1€ filter) on a Channels array. |
| `node.peak` | Peak | — | — | Peak (max) Rec. |
| `node.sample_and_hold` | Sample & Hold | — | — | Capture an input scalar on each trigger-edge and hold it until the next edge — freezes the trigger-time value so mid-decay slider moves don't leak through. |
| `node.scalar_array_accumulator` | Scalar Array Accumulator | — | — | Add `increment` to every element of an internal Array<f32> accumulator on each clip trigger; emit the accumulator. |
| `node.smoothing` | Smoothing | — | — | Exponential one-pole smoothing on a scalar wire — response time ≈ `time_constant` seconds, frame-rate-independent. |
| `node.texture_dimensions` | Texture Dimensions | — | — | Read the input texture's pixel dimensions. |
| `node.track_persist` | Track Persist | — | — | Greedy nearest-neighbour identity tracking with grace-period retention. |
| `node.trigger_ease_to` | Trigger Ease To | — | — | Beat-clocked snap-and-glide — on each trigger edge eases from the current value to the incoming `target` along a cubic ease-out over `window_beats` beats, then… |
| `node.trigger_gate` | Trigger Gate | — | — | Gate a trigger_count scalar stream. |
| `node.value` | Value | — | — | Emit a constant scalar value on the `out` port. |

### Unlisted (registered, not in palette) (13)

| type_id | label | category | role | purpose |
|---|---|---|---|---|
| `node.affine_transform` | — | — | — | 2D UV affine: translate, scale, rotate around the center. |
| `node.chroma_key` | — | — | — | Produce a per-pixel mask describing how close each pixel is to a target colour (RGB Euclidean distance, soft falloff at the tolerance edge). |
| `node.color_lut` | — | — | — | 1D LUT remap: sample a W×1 LUT texture indexed by BT.601 luminance (with contrast adjust), then crossfade against the source. |
| `node.edge_detect` | — | — | — | Sobel 3×3 edge detection with smoothstep threshold, crossfaded against the source by amount. |
| `node.fluid_project_scatter_2d` | — | — | — | Legacy type-ID alias of node.scatter_particles_camera (FluidSim3D's camera-projection + 2D scatter display path); retained so older projects load. |
| `node.invert` | — | — | — | Inverts RGB channels and blends against the source by intensity. |
| `node.masked_mix` | — | — | — | Per-pixel blend of two textures, weighted by a third texture's red channel. |
| `node.rotate_vec2_90` | — | — | — | Rotate the RG vec2 field by 90°. |
| `node.watercolor` | — | — | — | Pixel-exact wrap of the legacy WatercolorFX composite — seven sequential passes (grain+max → flow → displacement → diffusion blur → slope displace → luma blur … |
| `node.wireframe_depth` | — | — | — | Wraps the legacy WireframeDepthFX 15-pass pipeline (MiDaS depth DNN + optional optical flow + mesh pyramid) as a monolithic primitive — too tightly state-coupl… |
| `system.final_output` | — | — | — | Output boundary for both effect chains and generators — the host pre-binds the final output texture here. |
| `system.generator_input` | — | — | — | Generator graph entry boundary — emits the per-frame scalar context: time, beat, aspect, trigger_count, anim_progress. |
| `system.source` | — | — | — | Effect-chain input boundary — the host pre-binds the upstream texture here. |

### Effect & generator presets (45)

| id | name | kind | category | params |
|---|---|---|---|---|
| `AutoGain` | Auto Gain | effect | Stylize | 4 |
| `BasicShapes` | Basic Shapes | generator | Procedural | 4 |
| `BlackHole` | Black Hole | generator | Procedural | 15 |
| `BlobTracking` | Blob Track | effect | Diagnostic | 5 |
| `Bloom` | Bloom | effect | Filmic | 1 |
| `ChromaticAberration` | Chromatic Aberration | effect | Filmic | 5 |
| `ColorCompass` | Color Compass | effect | Spatial | 2 |
| `ColorGrade` | Color Grade | effect | Color | 9 |
| `ComputeStrangeAttractor` | Strange Attractor | generator | Procedural | 11 |
| `ConcentricTunnel` | Concentric Tunnel | generator | Procedural | 6 |
| `DepthOfField` | Depth of Field | effect | Filmic | 8 |
| `DigitalPlants` | Digital Plants | generator | Procedural | 14 |
| `Dither` | Dither | effect | Color | 2 |
| `Duocylinder` | Duocylinder | generator | Procedural | 11 |
| `EdgeGlow` | Edge Detect | effect | Diagnostic | 3 |
| `EdgeStretch` | Edge Stretch | effect | Spatial | 3 |
| `FluidSimulation` | Fluid Sim 2D | generator | Generator | 13 |
| `FluidSimulation3D` | Fluid Sim 3D | generator | Generator | 20 |
| `Glitch` | Glitch | effect | Filmic | 5 |
| `HdrBoost` | Highlight Boost | effect | Filmic | 4 |
| `Infrared` | Infrared | effect | Color | 3 |
| `InvertColors` | Invert | effect | Color | 1 |
| `Kaleidoscope` | Kaleidoscope | effect | Spatial | 2 |
| `Lissajous` | Lissajous | generator | Procedural | 11 |
| `MetallicGlass` | Metallic Glass | generator | Generator | 13 |
| `Mirror` | Mirror | effect | Spatial | 2 |
| `MriVolume` | MRI Volume | generator | Source | 8 |
| `NestedCubes` | Nested Cubes | generator | Procedural | 6 |
| `NodeGraphTest` | Node Graph Test | effect | Diagnostic | 1 |
| `OilyFluid` | Oily Fluid | generator | Generator | 14 |
| `ParticleText` | Particle Text | generator | Generator | 15 |
| `Plasma` | Plasma | generator | Procedural | 6 |
| `QuadMirror` | Quad Mirror | effect | Spatial | 1 |
| `SoftFocusGraph` | Soft Focus | effect | Stylize | 2 |
| `StarField` | Star Field | generator | Procedural | 8 |
| `Strobe` | Strobe | effect | Stylize | 3 |
| `StylizedFeedback` | Stylized Feedback | effect | Stylize | 3 |
| `Tesseract` | Tesseract | generator | Procedural | 11 |
| `Text` | Text | generator | Source | 8 |
| `Transform` | Transform | effect | Spatial | 4 |
| `VoronoiPrism` | Voronoi Prism | effect | Stylize | 3 |
| `Watercolor` | Watercolor | effect | Stylize | 4 |
| `WireframeDepth` | Wireframe Depth | effect | Diagnostic | 12 |
| `WireframeDepthGraph` | Wireframe Depth (Graph) | effect | Diagnostic | 8 |
| `WireframeZoo` | Wireframe | generator | Procedural | 9 |

<!-- END GENERATED: registered-node-index -->

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
| Beat Ramp | `node.beat_ramp` | Per-beat attack envelope — snaps to 0 each beat, ramps to 1 over the first `attack` fraction; seek-safe |
| Trigger Gate | `node.trigger_gate` | Emit a single-frame pulse on integer-edge changes of an input scalar |
| Smoothing | `node.smoothing` | One-pole low-pass on a scalar (stateful) |
| Envelope Follower (AR) | `node.envelope_follower_ar` | Attack/release envelope from an impulse (stateful) |
| Compressor Envelope | `node.compressor_envelope` | Audio-compressor envelope path applied to a scalar signal level — log-domain program-dependent A/R + ratio compression toward a `target`, out is a gain multiplier in [0.1, 10.0] (stateful; AutoGain) |
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
| Hash Field by Seed | `node.hash_field_by_seed` | Re-hash a value field's RG by an added scalar seed (Hash2 → RG, Hash1 → RGB) — per-cell randoms that re-roll each beat |
| Flow Field Noise | `node.flow_field_noise` | 2-channel flow vectors for advection (Watercolor-style) |
| Voronoi 2D | `node.voronoi_2d` | Worley/Voronoi — F1 (R), F2 (G), F2-F1 (B), per-cell stable hash (A). Foundation for stars, foam, cracked glass, embers, tiles. |
| Checkerboard | `node.checkerboard` | Binary checker pattern at configurable scale |
| Distance to Point | `node.distance_to_point` | Per-pixel distance to a configurable point in UV space |
| Dither Pattern | `node.dither_pattern` | Per-pixel ordered-dither / halftone threshold field — six algorithms (Bayer 8×8, Halftone, Lines, CrossHatch, Blue Noise, Diamond); pairs with `node.dither` |
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
| Flash | `node.flash` | Modulate brightness by a scalar `amount` — Opacity / White / Gain mode; Strobe's apply half (wire beat_gate into `amount`) |

### 3.5 Color & tone

| Display Name | Type ID | Purpose |
|---|---|---|
| Clamp | `node.clamp_texture` | Per-pixel saturate to [min, max] — the texture-side counterpart of `array_math::Clamp01` |
| Channel Mix | `node.channel_mix` | Per-pixel 4×4 RGBA matrix transform. Default = identity. Use to swizzle channels (A→R for reading cell_hash as a control signal), pull luma, isolate a single channel, or pre-tint for halation-style chains. |
| Levels | `node.levels` | Fused per-channel `pow(clamp(in*scale+offset, lo, hi), gamma)` — collapses scale_offset → clamp → power into one dispatch |
| Contrast | `node.contrast` | Pivot-around-0.5 contrast `(c-0.5)*contrast+0.5`; HDR-safe affine (no gamma NaN) |
| Saturation | `node.saturation` | Luma-based saturation `mix(luma, c, saturation)` — pulls toward perceptual grey (the Color Grade look) |
| Hue / Saturation | `node.hue_saturation` | HSV adjust — rotate hue (deg), scale saturation + value; Color Grade composes from this |
| Colorize | `node.colorize` | Selective tint toward a hue, masked per-pixel by brightness × neutrality × focus (duotone toward highlights) |
| Posterize | `node.posterize` | Quantize each RGB channel to `levels` discrete steps; Dither composes from this |
| Film Grain | `node.film_grain` | Multiplicative white-noise grain `src*(1-amount*(1-noise))` — paper-texture pass of Watercolor |
| Gradient Ramp | `node.gradient_ramp` | N-stop (≤16) 1D gradient / LUT generator with last-segment HDR extrapolation; luminance LUT for `color_lut` |
| HDR Retention Mix | `node.hdr_retention_mix` | Preserve a reference texture's above-1.0 highlight energy through a compressed texture's gain adjustment |
| Color LUT | `node.color_lut` | 1D LUT remap via luminance index |
| Chroma Key | `node.chroma_key` | Per-pixel RGB-distance mask to a target colour |
| Chromatic Displace | `node.chromatic_displace` | Per-channel UV displacement by a vector field |
| Tone Map | `node.tone_map` | HDR → SDR/PQ/EDR with ACES / AgX / Khronos Neutral curves |
| Reinhard Tone Map | `node.reinhard_tone_map` | Extended Reinhard, SDR-only; bit-matches FluidSim display |

### 3.6 Image transforms

The UV-warp family below is `coordinate-field → node.remap → node.mix` (TouchDesigner's Remap-TOP shape): a coordinate generator emits per-pixel sample UVs, `node.remap` resamples the source at them, `node.mix` crossfades. This visible graph replaced the fused whole-effect kernels — `radial_fold_uv` ⇐ Kaleidoscope, `mirror_fold_uv` ⇐ Mirror / QuadMirror, `uv_strip_clamp` ⇐ Edge Stretch, `radial_offset_field` + `chromatic_displace` ⇐ Chromatic Aberration. The affine half (translate/scale/rotate) stays in `node.affine_transform`.

| Display Name | Type ID | Purpose |
|---|---|---|
| Remap | `node.remap` | Resample `source` at per-pixel UVs from a coordinate field (TD Remap TOP); Absolute / Relative field mode, Clamp/Repeat/Mirror wrap. The generic UV-warp atom |
| Affine Transform | `node.affine_transform` | Three-scalar-port affine — port-shadow demo for translate_x/y + rotation |
| Rotate 2D | `node.rotate_2d` | Rotate a 2D coordinate field around the origin |
| Radial Fold UV | `node.radial_fold_uv` | Kaleidoscope coordinate generator — folds the plane into N mirrored wedges and emits the sample UV |
| Mirror Fold UV | `node.mirror_fold_uv` | Mirror/fold coordinate generator (Identity / Mirror / MirrorX/Y / FlipY / QuadMirror / Fold modes) — emits the folded sample UV |
| UV Strip Clamp | `node.uv_strip_clamp` | Edge-stretch coordinate generator — clamps UV to a center strip (Horiz/Vert/Both) so resampling stretches edge pixels outward |
| Radial Offset Field | `node.radial_offset_field` | Directional displacement field (Radial outward-with-falloff or Linear at `angle`) — feeds chromatic_displace / uv_displace_by_flow / texture_advect |
| Block Displace Field | `node.block_displace_field` | Per-block random UV-offset field (datamosh building block) — emits gated `offset` (RG) + per-block `hash`; feed into `node.remap` (Relative) |
| Scanline Jitter Field | `node.scanline_jitter_field` | Per-row random horizontal-offset field (VHS tearing) — gated `offset`; feed into `node.remap` (Relative) |
| Slope Displace | `node.slope_displace` | Emboss-style displacement along a soft-light luminance Sobel gradient — Watercolor's pigment-pooling edge pull |
| Mirror Axis | `node.mirror_axis` | Sample input at UVs mirrored across a line through center at `angle` — single-axis 2-fold symmetry (one half visible, other half is mirror) |
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

### 3.8a Mask sources

SDF / gradient mask generators (RGB = mask value, smoothstep falloff). Pair downstream with `masked_mix`, or `node.invert` to flip polarity.

| Display Name | Type ID | Purpose |
|---|---|---|
| Box Mask | `node.box_mask` | Rotated rectangular SDF mask (Chebyshev) — band masks for tilt-shift / scanlines / letterboxes |
| Ellipse Mask | `node.ellipse_mask` | Rotated elliptical SDF mask — industry-standard masking convention |
| Linear Gradient | `node.linear_gradient` | Directional 0→1 ramp in UV space — fades / wipes; pairs with masked_mix |

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
| Texture Dimensions | `node.texture_dimensions` | Read input texture `width` / `height` / `aspect` as scalars — no GPU dispatch, zero latency (feed aspect-correction downstream) |

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
| Fresnel Rim | `node.fresnel_rim` | Fresnel edge highlight (additive) |
| Matcap Two-Tone | `node.matcap_two_tone` | Cross-axis 4-colour matcap from a normal map |
| Bake Equirect Envmap | `node.bake_equirect_envmap` | Procedural HDR studio environment map at configurable resolution (one-shot persistent output, equirect layout) — wire into `node.render_3d_mesh`'s `envmap` for PBR-IBL |

#### Material wires (consumed by the mesh renderers)

One `Material` per node, wired into `render_3d_mesh` / `render_instanced_3d_mesh`. See [MATERIAL_SYSTEM_DESIGN.md](MATERIAL_SYSTEM_DESIGN.md).

| Display Name | Type ID | Purpose |
|---|---|---|
| Unlit Material | `node.unlit_material` | Flat-colour material — no lighting / shadow term; renderer writes base + emission directly. No `light` input required |
| Phong Material | `node.phong_material` | Lambert diffuse + Blinn-Phong specular + ambient floor — cheap lit baseline (requires a `light`) |
| Cel Material | `node.cel_material` | Cel-shaded — Lambert N·L quantized into `cel_bands` discrete bands (the DigitalPlants look; requires a `light`) |

*Photoreal PBR (Cook-Torrance + IBL) lives inside `node.render_3d_mesh`'s `node.pbr_material`, not as standalone wireable atoms — the standalone `cook_torrance_specular` / `equirect_envmap_sample` were removed 2026-05-30 (zero references; below the level any tool exposes, cf. Blender's Principled BSDF). The à-la-carte shading atoms above stay for stylized / NPR looks (no canonical answer to compose).*

### 3.13 Flow & fluid

Per-frame fluid-sim primitives. Pair upstream with seed + downstream with scatter/resolve.

| Display Name | Type ID | Purpose |
|---|---|---|
| Texture Advect | `node.texture_advect` | Backward semi-Lagrangian advection by a velocity field |
| LIC Integrate | `node.lic_integrate` | Line Integral Convolution — flow visualisation streamlines |
| Gradient (Central Diff 3D) | `node.gradient_central_diff_3d` | 6-tap central-diff gradient of a density Texture3D → vec3 Texture3D (toroidal wrap, ×0.5). 3D sibling of `gradient_central_diff` |
| Curl + Slope Force 3D | `node.curl_slope_force_3d` | `cross(gradient, ref_axis)*curl + gradient*slope` → vec3 force Texture3D. Pairs with `gradient_central_diff_3d` (the decomposed FluidSim3D force field) |
| Sample Texture 3D at Particles | `node.sample_texture_3d_at_particles` | Per-particle trilinear sample of a vec3 Texture3D at position.xyz → `Array<[f32;3]>` forces. 3D sibling of `sample_texture_at_particles` |
| Simplex Noise Force 3D at Particles | `node.simplex_noise_force_3d_at_particles` | 3-plane density-adaptive simplex advection added to the force buffer |
| Diffuse Force 3D at Particles | `node.diffuse_force_3d_at_particles` | Density-weighted incoherent random kick added to the force buffer |
| Container Repel Force 3D | `node.container_repel_force_3d` | Soft SDF boundary cushion (Cube/Sphere/Torus) added to the force buffer (pre-integration) |
| Euler Step Particles 3D | `node.euler_step_particles_3d` | `position.xyz += forces * speed * (dt*60)`. 3D sibling of `euler_step_particles` |
| Container Bounds 3D | `node.container_bounds_3d` | Post-integration hard containment: toroidal wrap (None) or SDF reflect+clamp. 3D sibling of `wrap_particles_torus` |
| Flatten to Camera Plane | `node.flatten_to_camera_plane` | Compress particles toward the camera viewing plane (reads `cam.fwd`) |
| Apply Radial Burst (Particles) | `node.apply_radial_burst_to_particles` | Per-particle radial+tangent impulse around a point — FluidSim2D inject path |
| Apply Radial Burst 3D (Particles) | `node.apply_radial_burst_3d_to_particles` | Per-particle 3D injection burst around 4 tetrahedron zones + vortex ring — FluidSim3D inject path |
| Scatter Particles Camera | `node.scatter_particles_camera` (alias `node.fluid_project_scatter_2d`) | 3D particles → 2D u32 accumulator via Camera projection. Sibling to `scatter_particles` / `scatter_particles_3d` |
| Sample Volume 2D | `node.sample_volume_2d` | Sample a Texture3D as 2D slice/projection |

### 3.14 3D + 4D geometry pipeline

| Display Name | Type ID | Purpose |
|---|---|---|
| Orbit Camera | `node.camera_orbit` | Orbit-style perspective `Camera` from five port-shadowed scalars (orbit/tilt/distance/fov_y/look_y); also emits `pos_x/y/z` for PBR shading. One wire replaces N per-renderer camera params |
| Light | `node.light` | Single Sun / Point light → `Light` wire consumed by shading atoms + shadow-aware mesh renderers; all params port-shadowed (one node per light) |
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
| Connect Nearest | `node.array_connect_nearest` | For each item in a `Channels[X, Y, WIDTH, HEIGHT]` array find its nearest neighbour within `max_distance` and emit an `EdgePair` — sparse nearest-neighbour graph for `render_lines` connection-line viz |

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
| Person Segment | `node.person_segment` | Human/person segmentation via native plugin — R=G=B = person probability mask (~2-3 frame latency); same channel pack as depth_estimate_midas |
| Blob Detect (FFI) | `node.blob_detect_ffi` | Sparse blob detection — emits `Array<Blob>` |
| Blob Overlay Render | `node.blob_overlay_render` | Draws blob bounding boxes |
| Optical Flow | `node.optical_flow_estimate` | Per-pixel optical flow vectors |
| Track Persist | `node.track_persist` | Greedy nearest-neighbour identity tracking with grace-period retention on `Channels[X, Y, WIDTH, HEIGHT]` detections — stable IDs across frames (prereq for one_euro_filter) |
| One Euro Filter | `node.one_euro_filter` | Adaptive temporal low-pass (1€ filter) on a Channels array — heavy smoothing when still, responsive when fast; per-channel per-sample |
| Render Filled Rects | `node.render_filled_rects` | Instanced filled-rectangle overlay from a `Channels[X, Y, WIDTH, HEIGHT]` array (additive) — gauges, debug regions, VU meters |
| Render Value Overlay | `node.render_value_overlay` | Bitmap-font numeric labels at multiple positions (5×7 atlas; Index/Hex/Coord/Float3 format) — diagnostic HUDs |
| Image Folder | `node.image_folder` | Scrub through a folder of images via a position scalar |
| Render Text | `node.render_text` | CoreText glyph rasterizer wrapped as a primitive — composite a text string into the output with position / scale / aspect / alignment |
| Auto Gain Apply | `node.auto_gain_apply` | GPU side of AutoGain — pairs with the CPU envelope follower |

### 3.19 WGSL escape hatch

Reserved for genuinely irreducible kernels (see DECOMPOSING_GENERATORS §5 before reaching).

| Display Name | Type ID | Purpose |
|---|---|---|
| WGSL Compute | `node.wgsl_compute` | Naga-introspected user compute kernel — ports/uniforms/array fields derived from the shader source; arbitrary texture + `Array<T>` in/out. Replaces the three fixed-arity `wgsl_compute_*in_*tex` variants. First consumers: BlackHole, ComputeStrangeAttractor, FluidSim3D |

---

## 4. Effects — named visual looks

**Effects are JSON presets, not palette nodes.** Every effect now ships as a decomposed atom graph in [`assets/effect-presets/`](../crates/manifold-renderer/assets/effect-presets/), runs `system.source → atoms → system.final_output`, and is drillable in the graph editor (open the effect, see its atoms, rewire them). The pre-migration model — effects as monolithic `shader` / `composite` / `bundle` nodes in the palette — is gone: the fused legacy effect-monolith bundles were **deleted on 2026-05-30**, and the named-look kernels (Kaleidoscope, Quad Mirror, Edge Stretch, Chromatic Aberration, Color Grade, Infrared, Plasma, Bloom, Strobe, …) were replaced by composable atoms (the §3.6 UV-warp family, the §3.5 colour atoms, etc.).

**One legacy wrapper remains: Wireframe Depth** (`node.wireframe_depth`, `WIREFRAME_DEPTH_TYPE_ID`) still wraps the legacy `WireframeDepthFX: PostProcessEffect` Rust impl. It is the lone remaining fused effect node and an active decomposition target — its 48-node `WireframeDepthGraph.json` atom-graph replacement is in flight (depth + person-segment DNN atoms + edge_detect + wireframe primitives). Until that lands, `WireframeDepth.json` is a thin wrap of the legacy node and `WireframeDepthGraph.json` is the parallel atom-graph build-out.

The effect presets are listed in §5.

---

## 5. Effect presets

26 JSON files at [`assets/effect-presets/`](../crates/manifold-renderer/assets/effect-presets/). Each is a decomposed atom graph (drillable in the editor); the atom composition is noted. The only thin-wrap-of-a-legacy-node is `WireframeDepth` (wraps `node.wireframe_depth`); `WireframeDepthGraph` is its in-flight atom-graph replacement.

| Preset | Atom shape |
|---|---|
| AutoGain | `luminance` → `compressor_envelope` → `gain` → `hdr_retention_mix` → `wet_dry` |
| BlobTracking | `blob_detect_ffi` → `track_persist` → `one_euro_filter` → `array_connect_nearest` → `render_value_overlay`; `wgsl_compute` ×8 + `affine_scalar` ×2 |
| Bloom | `threshold` → `downsample` → `blur` → `mix` |
| ChromaticAberration | `radial_offset_field` + `math` → `chromatic_displace` → `mix` |
| ColorCompass | 4× `color_sample` → `math` → `smoothing` → `affine_transform` — texture-to-scalar bridge closing the loop into image transform |
| ColorGrade | `contrast` → `saturation` → `hue_saturation` → `colorize` → `gain` → `clamp_texture` → `mix` |
| DepthOfField | `depth_estimate_midas` / `box_mask` / `ellipse_mask` + CoC math → `gaussian_blur_variable_width` ×2 → `masked_mix` |
| Dither | `dither_pattern` → `dither` |
| EdgeGlow | `edge_detect` standalone |
| EdgeStretch | `uv_strip_clamp` → `remap` → `mix` |
| Glitch | `block_displace_field` + `scanline_jitter_field` + `radial_offset_field` → `remap` → `chromatic_displace` + per-block `invert` via `masked_mix`, gated by `value`/`math`/`mix` |
| HdrBoost | `threshold` → `gain` → `math` ×2 → `mix` |
| Infrared | `gradient_ramp` ×10 → `mux_texture` → `color_lut` (thermal palette as N-stop ramps) |
| InvertColors | `invert` standalone |
| Kaleidoscope | `radial_fold_uv` → `remap` → `mix` (verbatim fold port of the legacy bundle) |
| Mirror | `mirror_fold_uv` → `remap` → `mix` |
| NodeGraphTest | test fixture (`mix`) |
| QuadMirror | `centered_uv` → `abs_texture` → `scale_offset_texture` → `remap` → `mix` |
| SoftFocusGraph | `blur` → `mix` |
| Strobe | `beat_gate` → `flash` |
| StylizedFeedback | `feedback` → `affine_transform` → `gain` → `vignette` → `mix` |
| Transform | `affine_transform` standalone |
| VoronoiPrism | `voronoi_2d` → `hash_field_by_seed` ×2 + `beat_ramp` → `uv_strip_clamp` → `remap` → per-cell beat-driven `mix` composite |
| Watercolor | `flow_field_noise` → `uv_displace_by_flow` → `slope_displace` → `feedback` + `blur` ×2 + `masked_mix` |
| WireframeDepth | thin wrap of `node.wireframe_depth` (legacy `PostProcessEffect`) |
| WireframeDepthGraph | in-flight atom-graph decomposition: `depth_estimate_midas` + `person_segment` + `optical_flow_estimate` + `wgsl_compute` ×13 + `feedback` ×5 + math/value scaffolding |

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
| FluidSim3D | volumetric particle fluid sim (fully atom-decomposed): `seed_particles` → `wgsl_compute` (8-pattern seed) → `array_feedback` → `scatter_particles_3d` → `resolve_3d_accumulator` → `blur_3d_separable` ×3 (density) → `gradient_central_diff_3d` → `curl_slope_force_3d` → `blur_3d_separable` ×3 (field) → per-particle chain (`sample_texture_3d_at_particles` → `simplex_noise_force_3d_at_particles` → `diffuse_force_3d_at_particles` → `container_repel_force_3d` → `euler_step_particles_3d` → `container_bounds_3d` → `flatten_to_camera_plane` → `apply_radial_burst_3d_to_particles`) → `scatter_particles_camera` → `resolve_accumulator` → `reinhard_tone_map`, with `camera_orbit` + `inject_burst` + `clip_trigger_cycle` drivers |
| Lissajous | parametric curve, fully atomized: `lfo` ×3 + `frequency_ratio` + `mux_scalar` ×2 → per-axis `math(Floor/Ceil/Subtract)` bracket + `generate_range` → `array_math(ScaleOffset+Sin)` ×4 + `array_math(Mix)` ×2 → `pack_curve_xy` → `render_lines`. The TouchDesigner Pattern→Math→Function→Merge→To-SOP shape; bracket-interp is graph-visible. |
| MetallicGlass | feedback-displacement metallic surface, fully atomized: `simplex_field_2d` + `scale_offset` → `feedback` ping-pong with `mix Difference`+`mix Lerp 0.98` → `gaussian_blur` H/V → split into (height/levels chain) and (`mirror_axis`+`convolution_2d_9tap`×2+`pack_channels`+`length_vec2` Sobel chain). Geometry: `generate_grid_mesh` → `displace_mesh(height=height_levels)` → `triangulate_grid` → `render_3d_mesh` (forward PBR pass). Shading: `gain(height × displace) → heightmap_to_normal(coord_space=WorldYUp, aspect=system.aspect)` → `normal_map`; `scale_offset_texture(edge, scale=0.15, offset=0.05)` → `roughness_map`; `bake_equirect_envmap` → `envmap`. `render_3d_mesh`'s `pbr_material` does Cook-Torrance (D_GGX × G_Smith × F_Schlick) + IBL internally, sampling normal/roughness at mesh UV and writing linear colour straight to `final_output` (no standalone specular / envmap-sample / tone-map nodes — refactored 2026-05-27, the standalone atoms removed 2026-05-30). Activates the PBR-on-3D-mesh path (`render_3d_mesh` material=pbr, `heightmap_to_normal` WorldYUp, `bake_equirect_envmap`, `camera_orbit.pos_xyz`) — reusable for any perspective-correct PBR generator. |
| MriVolume | volumetric scrubbing: `image_folder` ×3 → `mux_texture` → `sharpen` → `smoothstep_texture` → `invert` |
| ParticleText | FluidSim2D base + text-force branch (`render_text → gaussian_blur H+V → gradient_central_diff → rotate_vec2_by_angle → gain → mix(Add) into the force chain`). The glyphs are baked into the force field as a perpendicular-curl flow, particles continuously stream along the text shape instead of being seeded at it |
| NestedCubes | instanced mesh with cycled poses: `trigger_gate` → `scalar_array_accumulator` → `cycle_table_row` → `mux_array` → `nested_cubes_geometry` |
| OilyFluid | screen-space fluid + atomized PBR: `feedback` ×2 + gradient atoms + `texture_advect` + `simplex_field_2d` → `heightmap_to_normal` → `lambert_directional` + `matcap_two_tone` + `fresnel_rim` + `blinn_specular` summed via `mix` |
| Plasma | open family on the introspected escape hatch: `clip_trigger_cycle` + `mux_scalar` → `wgsl_compute` (8 plasma variants via `switch`) — decoupled from the deleted `plasma_pattern_2d` enum primitive |
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
