# Primitive Audit and 2nd-Pass Decomposition Plan

**Status:** Active. Established 2026-05-26 after the post-migration inventory revealed the fused-bundle anti-pattern at scale. The original generator migration ([`GENERATOR_DECOMPOSITION_PLAN.md`](GENERATOR_DECOMPOSITION_PLAN.md)) is closed — 0 Rust generators remain — but several of the primitives shipped during that pass are fused bundles that need to be decomposed under the no-fused-monolith rule.

**Companion docs:** [`DECOMPOSING_GENERATORS.md`](DECOMPOSING_GENERATORS.md) (how-to-think, mandatory pre-read), [`NODE_CATALOG.md`](NODE_CATALOG.md) (what exists today), [`ADDING_PRIMITIVES.md`](ADDING_PRIMITIVES.md) (mechanics of adding atoms).

---

## 0. The rule this plan implements

A primitive does one composable thing — a single GPU dispatch, a single DNN inference, a single FFI call, a single CPU operation. Bundling multiple distinct operations into a "this is the whole effect" or "this is the whole generator" kernel is not permitted. See `CLAUDE.md` hard rules and `DECOMPOSING_GENERATORS.md` §1.1.

The migration agent failure mode that produced the current pile: when given "decompose X" and a parity-test target, the shortcut is to write a fused kernel that bundles 4-6 distinct dispatches into one primitive and pass the parity test. That's been audited and the rule has been tightened — fusing for parity is not the answer. The answer is decomposing with intermediate-format precision specified (typically `Rgba32Float` for the intermediate textures that the legacy register-math produced bit-exact in fp32).

---

## 1. The audit inventory

### 1.1 Fused bundles wearing primitive clothing

Each row: the bundled primitive, the operations it internalizes, the atoms it should decompose into (existing on the shelf in **bold**, new in *italics*).

| Bundled primitive | Bundles | Decomposes into |
|---|---|---|
| ~~`node.fluid_simulate`~~ | ~~Euler integration + density-adaptive simplex-noise advection + per-particle diffusion + injection burst + toroidal wrap~~ | **Done 2026-05-27** — atom-decomposed across multiple commits. Per-particle dispatch chain: `sample_texture_at_particles → anti_clump_particles → euler_step_particles → wrap_particles_torus → apply_radial_burst_to_particles`. Force-field assembly fully texture-domain: `gradient_central_diff(scale=UV, wrap=Repeat) + scale_offset + rotate_vec2_by_angle → blur ×2 → mix(Add field, noise_force) → mix(Add combined, NEXT) → sample_at_particles`. Density-adaptive simplex noise advection composed from `simplex_field_2d` ×2 + `pack_channels` + `scale_offset_texture` (for capped_density via `mix(Divide)`) + adaptive_amp multiplied by the noise vec2. Bundle Rust + WGSL deleted in the ParticleText migration commit. |
| `node.fluid_simulate_3d` | Same as above in 3D + soft-container SDF repulsion + camera-aware flatten | *`integrate_particles_3d`* + *`apply_noise_advection_3d`* + *`apply_diffusion_3d`* + *`apply_container_repulsion`* + *`apply_camera_flatten`* |
| ~~`node.fluid_gradient_rotate`~~ | ~~Central-difference gradient + 2D rotation (fused for FluidSim parity)~~ | **Done 2026-05-27** — atom-decomposed. `gradient_central_diff` extended with `scale_mode: Texel | UV` + `wrap_mode: Clamp | Repeat` (defaults preserve oily-fluid behaviour). `rotate_vec2_90` migrated to `rotate_vec2_by_angle` with port-shadowed angle param (legacy type-ID aliased). Decomposed shape: `gradient_central_diff(UV, Repeat) → scale_offset_texture(slope × area_scale) → rotate_vec2_by_angle(angle)`. Bundle Rust + WGSL deleted. |
| `node.fluid_gradient_curl_3d` | 3D central-difference gradient + curl-rotation by reference axis | *`gradient_central_diff_3d`* + *`curl_from_gradient_3d`* |
| ~~`node.fluid_seed`~~ | ~~7 curated geometric seed patterns (center cluster, lines, rings, cross, spiral, edge ring)~~ | **Done 2026-05-27** — direct Array<Particle> write via `wgsl_compute(particles, pattern, trigger_count, active_count)` aliased on the seed_particles allocator + `cast_as_particle` bridge. Per-pattern math is a verbatim port of legacy fluid_seed.wgsl (CLT Gaussian / H-lines / V-lines / rings / cross / spiral / edge ring). Adding patterns: append a `case` to the switch + bump `pattern_cycle.modulus`. Bundle Rust + WGSL deleted. |
| `node.fluid_seed_3d` | 8 curated 3D seed patterns | Same shape as 2D — curated-via-`wgsl_compute` |
| ~~`node.integrate_particles_attractor`~~ | ~~RK2 + 5 attractor ODEs (Lorenz / Rössler / Aizawa / Thomas / Halvorsen) + 3D→2D projection~~ | **Done 2026-05-26** — atomized onto `node.wgsl_compute` (JSON-editable shader escape hatch, BlackHole-shape) with `switch (attractor_type)` covering all 5 variants. Per-attractor centre / scale / base_dt live as `switch` helpers inside the WGSL string. Per-particle first-frame init (zero-velocity detect → hash seed + warmup steps to converge on manifold) replaces the bundled `cs_seed` kernel without external gating — `seed_particles(OnceOnReset)` zeroes velocity, the simulate shader detects and inits per particle. NaN/divergence guard (one comparison per substep) replaces the 50-step escape respawn loop. Diffusion extracted to new `node.array_diffuse_particles` atom (generic, reusable by fluid sims and other particle systems). Adding Sprott / Chen / Chua is a JSON edit (append a `case` to the switch + entries to per-attractor const tables). The audit-doc proposal of `array_eval_ode` + `array_axpy` + `array_project_3d` was too granular per §1 ("single dispatch with irreducible math doesn't get clearer split into atoms — Lorenz ODE step is the example") — and TouchDesigner's TOPs+Feedback shape uses the same granularity. clip_trigger via `clip_trigger_cycle` + `mux_scalar` (manual vs trigger-driven). Bundle Rust + WGSL deleted. |
| `node.plasma_pattern_2d` | 8 plasma variants (sin-based math) | **Deferred 2026-05-26 pending the graph compiler initiative — see [`GRAPH_COMPILER.md`](GRAPH_COMPILER.md).** Audit landed: 6 of 8 variants atomize cleanly into ~75 nodes via existing atoms (`centered_uv`, `sin_term`, `texture_sum_5`, `rotate_2d`, `distance_to_point`, `compose`, `smoothstep_texture`) plus 1-2 small new vec2-domain helpers. The other 2 variants (Noise, Fractal) are per-pixel iteration loops that today force a choice between (a) ~50-node unroll per variant or (b) single-use bespoke loop atoms (`iterated_sin_fbm_2d`, `iterated_sin_warp_2d`) that are exactly the per-shader-primitive-wrap anti-pattern. The graph compiler — WGSL inliner + `node.for_each_n` wrapper — makes per-pixel loops graph-expressible (Noise becomes a 4-atom body wrapped in `for_each_n`) and as a side effect collapses the 75-node atomized graph to ~8 fused dispatches at runtime. Plasma is the natural test bed: the parity test exists, the legacy primitive is intact for regression, and Noise/Fractal are the smallest interesting loop cases. Decomposition resumes once the compiler lands. |
| ~~`node.shape_2d`~~ | ~~3 SDF variants (Square / Diamond / Octagon) with trigger-cycled fills~~ | **Done 2026-05-27** — atom-decomposed (hybrid). New atoms: `node.basic_shape` (single-dispatch SDF compiled-enum, one shape per instance — three instances + `mux_texture` give runtime shape selection, graph-visible) and `node.trigger_ease_to` (generic stateful CPU atom: snap-on-trigger-edge, cubic-ease-out glide over a beat-clocked window — reusable for any "retarget on retrigger and tween" surface). Extension: `node.math` += `Modulo` (op 12) for the cycling math. Cycling decompose lives in JSON: `clip_trigger_index` + `mux_scalar` (modulus, signed-rotation table, fill-mode wireframe table) + `math(Modulo/Divide/Floor)` derive `(shape_idx, is_wireframe, rot_step)`. Audit corrected the original `distance_to_point` suggestion — Square is Chebyshev-with-corner-correction, Diamond is L1/√2, Octagon is reflection-based, none Euclidean. Closed-by-curation framing was also relaxed: future shape additions are a WGSL `case` add in `basic_shape` + a new instance in the mux. Documented parity delta: clip_trigger OFF→ON re-enable eases over the quarter beat instead of snapping (legacy reset behaviour was load-bearing only on `reset_trigger_state`; the new graph keeps `trigger_ease_to` running and gates at the output). Bundle Rust + WGSL deleted. |
| `node.star_field_2d` | 4-layer parallax hash + threshold + aspect-corrected gaussian halos + multi-frequency twinkle | **Audit.** Hash + threshold + brightness math atomizes (`hash_noise_field_2d` + `node.filter` + `brightness` + `math`); the per-star gaussian halos and the layered depth-stagger may or may not — read the kernel before deciding. Atom decomposition preferred where it reaches. |
| ~~`node.generate_lissajous`~~ | ~~`(sin(a*t+φ), sin(b*t+ψ))` curve eval sampled across [0, 2π], with floor/ceil ratio blending~~ | **Done 2026-05-26** — atom-decomposed to TD-CHOP shape. Shipped atoms: `generate_range` (linspace), `pack_curve_xy` (two `Array<f32>` → `Array<CurvePoint>`, folds `PROJ_SCALE = 0.25`). Extensions: `node.math` += `Floor`/`Ceil` (ops 10/11), `node.array_math` += `Sin`/`Cos`/`Mix` (ops 11/12/13, with `op_is_binary` classifier so the non-contiguous Mix stays binary-truncated). Lissajous.json is now an 18-node inner graph (was 1 node + sealed primitive); the bracket-interp morphing is visible as `math(Floor/Ceil/Sub) → array_math(ScaleOffset+Sin)×2 → array_math(Mix)` per axis. Atoms are reusable for Rose / Hypotrochoid / `polygon_shape` (Tranche 5). Legacy `generate_lissajous.rs` + `.wgsl` deleted. |
| ~~`node.polygon_shape`~~ | ~~Regular polygon vertex generation~~ | **Done 2026-05-26** — atom-decomposed onto the Lissajous atom family. Polygon outline is now `generate_range(end_inclusive=false, active_count=N) → array_math(Cos/Sin) → array_math(ScaleOffset, scale=size) → pack_curve_xy(scale=4.0 cancels PROJ_SCALE)`; closed-loop edges synthesised via new `consecutive_edges` atom. Extension: `generate_range` += `active_count` input + `end_inclusive` param (default true preserves Lissajous bit-exact). Legacy `polygon_shape.rs` deleted. ClipTriggerCycle path was unused by the only consumer (ConcentricTunnel cycles externally); dead-code removal. Mesh fan-triangulation output is dropped (no current consumer); revive as a `fan_triangulate_outline` atom if a future preset needs solid polygon fill. |
| ~~`node.concentric_outlines`~~ | ~~Ring stacker for polygon~~ | **Done 2026-05-26** — atom-decomposed. New atom: `node.array_replicate_polyline_rings(outline, edges, scales) → (outline, edges)`. Per-ring uniform scale on `Array<CurvePoint>`, per-ring index shift on `Array<EdgePair>` with sentinel preservation. Per-ring scales = `generate_range(0..K-1) → math(Floor/Subtract on expansion) + math(Multiply with ring_spacing) → array_math(ScaleOffset)`. Reusable for any "K transformed copies of a polyline" pattern (concentric spirals, replicated curve trails, instanced wireframe outlines). Legacy `concentric_outlines.rs` deleted. |
| ~~`node.wireframe_shape`~~ | ~~Curated 3D wireframe shapes (cube, octahedron, etc.)~~ | **Done 2026-05-26** — atom-decomposed. Two new curated-enum atoms: `node.polytope_vertices` (one GPU dispatch, closed-form Platonic coordinates baked + 0.25 PROJ_SCALE per §6.4) and `node.polytope_edges` (one CPU lookup, sentinel-padded edge table). Both atoms read a shared `shape` scalar so per-frame vertices and edges agree. WireframeZoo.json drives the selector from `value` + `clip_trigger_cycle` through `mux_scalar`; downstream `rotate_3d → project_3d → render_lines` (with the existing `edges` input) unchanged. The five Platonic solids are a mathematically closed set, so the curated enum is the correct atom floor — analogous to `node.math`'s op enum. `PLATONIC_SHAPES` + per-shape edge tables moved into `generators::mesh_common` as the shared schema; legacy `wireframe_shape.rs` + WGSL deleted; legacy `node.generate_platonic_solid` type-ID alias dropped (no saved project referenced inner-graph type IDs). Future non-Platonic shape sources (loaded meshes, geodesic spheres, parametric prisms) ship as siblings that reuse the same `Array<MeshVertex>` + `Array<EdgePair>` wire contract. |
| `node.generate_tesseract_vertices` | Tesseract 4D vertex generation | **Deferred 2026-05-26.** Audit converged: Tesseract is a closed mathematical object (regular 4-polytope, 16 corners at `(±1)⁴`, 32 hypercube bit-flip edges) — same class as the 3D Platonic solids, not the parametric-surface class. Future unification is `hypercube_vertices(d)` + `edges_from_hypercube(d)` (covers square / cube / tesseract / penteract / n-cube) — a sibling family to the grid-uv atoms shipped for Duocylinder. Defer until either user-authored n-cubes becomes a real ask or n-dim rotate/project lands. The current bundled primitive is at single-dispatch granularity; the only fusion smell is the paired vertex+edge emission, which the future split would clean up alongside the hypercube atom shipping. |
| ~~`node.generate_duocylinder_vertices`~~ | ~~Duocylinder 4D vertex generation~~ | **Done 2026-05-26** — atom-decomposed onto a new parametric-surface atom family rather than a parallel vertex/edge split. The (u, v) authoring path the audit converged on: **`node.generate_grid_uv`** (Pattern-CHOP-of-a-grid: emits two `Array<f32>` sampling `[0, u_max) × [0, v_max)` at `n²` row-major entries) + **`node.pack_vec4`** (zips four `Array<f32>` into `Array<Vec4Vertex>`; pure structural — no scale bake) + **`node.edges_from_grid_uv`** (u-wrap + v-wrap topology, `2n²` `EdgePair`s — the topology atom is reusable across any (u,v)-sampled surface). The Duocylinder.json inner graph is now `generate_grid_uv → array_math(Cos|Sin) × 4 → array_math(ScaleOffset, 0.176776695) × 4 → pack_vec4 → rotate_4d → project_4d → render_lines`, with `edges_from_grid_uv → render.edges`. Legacy `generate_duocylinder_vertices.rs` + WGSL deleted. The new atom family is the foundation for future user-authored parametric surfaces (torus, Klein, geodesic sphere, terrain mesh) without per-shape Rust atoms — substantially higher leverage than the originally-planned parallel vertex/edge split. |
| `node.nested_cubes_geometry` | Cycled-pose cube instance generator | **`generate_cube_mesh`** + **`cycle_table_row`** + **`mux_array`** + **`generate_instance_transforms`** (all already registered, currently unused) |
| `node.digital_plants_render` | Bespoke terminal renderer for DigitalPlants | **`render_instanced_3d_mesh`** + bespoke lighting via `wgsl_compute` for the plant-specific styling |
| ~~`node.render_3d_mesh_pbr_ibl`~~ | ~~PBR + IBL bundled with mesh render~~ | **Done 2026-05-27** — atom-decomposed via deferred-shading split (TouchDesigner / Blender shape, no MRT infrastructure). Geometry: `render_3d_mesh` extended with `world_pos` + `world_normal` G-buffer outputs (two extra back-to-back single-attachment render passes, ~250k vertex invocations × 3 — cheap). Shading: PBR atoms grew an optional `world_pos: Texture2D` input that switches them from constant-V flat-screen mode to per-pixel-V 3D-mesh mode (view scalars carry camera world position, light scalars carry light world position in 3D mode); `attenuation_scale` folds in the legacy `1/(1+d²/25)` falloff on `cook_torrance_specular`. Per-pixel roughness via new `roughness_map: Texture2D` optional input on both atoms. Fresnel + roughness coupling folded into `equirect_envmap_sample` (was a TODO in its composition_notes). `heightmap_to_normal` grew `coord_space` enum (TangentZ default | WorldYUp new) + `aspect` input to recover the legacy "full-resolution reflection trick" from the height field. `camera_orbit` grew `pos_x` / `pos_y` / `pos_z` scalar outputs. MetallicGlass.json terminal is now: `render_3d_mesh` → (world_pos + height_field → heightmap_to_normal) + (edge_field → scale_offset_texture → roughness_map) → `cook_torrance_specular` + `equirect_envmap_sample` → `mix Add` → `reinhard_tone_map`. The fused bundle's Rust + WGSL deleted. Documented parity deltas: dropped second packed-material temporal_blend (each PBR atom samples height/edge from pre-pack chain; primary 0.98 feedback loop unchanged); atom-decomposition reassociation ≤1 ULP per pixel. No parallel PBR atoms shipped — the brief's primary success metric (activating the three shelf atoms in their right shape) met. |
| `node.cylinder_wrap_field` + `node.torus_wrap_field` | Two bespoke wrap-onto-surface variants | Unify into `node.wrap_field` with surface enum (Cylinder / Torus / Sphere / Klein / …) |

### 1.2 Legacy effects with wrapped Rust pipelines

Each effect's `node.*` primitive currently wraps a legacy `PostProcessEffect`. Decomposing produces a graph of single-purpose primitives; the DNN / FFI / CPU work stays at primitive granularity but the fused outer kernel is deleted.

| Effect | Decomposes into |
|---|---|
| `node.auto_gain` | **`luminance`** (or new `node.luminance_sparse_sample` if the existing one isn't fast enough) + **`envelope_follower_ar`** + **`gain`** + `node.character_color` (5-variant curated-via-`wgsl_compute` family for Clean / Warm / Film / Vivid / Grit) |
| `node.blob_track` | **`blob_detect_ffi`** + *`one_euro_filter`* (new CPU primitive) + **`blob_overlay_render`** |
| `node.wireframe_depth` | **`depth_estimate_midas`** + edge detection primitives + wireframe rendering primitives + composite |
| `node.depth_of_field` | Per branch: DNN (`depth_estimate_midas` + CoC primitive + separable Gaussian + composite); Tilt-Shift (`tilt_shift_mask` primitive + CoC + …); Radial (`radial_mask` primitive + CoC + …) |
| `node.infrared` | Single-pass shader — decomposable into existing color/threshold/blend atoms |
| `node.quad_mirror` | Single-pass shader — decomposable into existing transform/mirror atoms |

### 1.3 Composite effect presets still wrapping monolithic effect nodes

Most of the 22 thin-wrap effect presets in `assets/effect-presets/` are still single-node graphs around a composite `node.*` primitive. Atomization candidates beyond the legacy-Rust six above:

| Preset | Current shape | Decomposes into |
|---|---|---|
| Bloom | `system.source → node.bloom → system.final_output` | Threshold + **`mip_chain`** (registered, unused!) + **`separable_gaussian`** + **`mix`** |
| Halation | Same shape with `node.halation` | Shares Bloom's atom set + spectral kernel shift atom |
| Watercolor | Same with `node.watercolor` | **`flow_field_noise`** + **`uv_displace_by_flow`** (both registered, unused) + existing atoms |
| Glitch | Same with `node.glitch` | `node.hash_noise_field_2d` + block displace primitive + scanline primitive + chromatic offset |
| Kaleidoscope | Same with `node.kaleidoscope` | Decomposes onto **`kaleido_fold`** atom (already exists as the underlying impl) |
| Color Grade | Same with `node.color_grade` | Decomposes into channel-mix + curves + saturation atoms |
| Chromatic Aberration | Same with `node.chromatic_aberration` | **`chromatic_displace`** atom + RG/RB offset primitive |
| Dither | Same with `node.dither` | **`dither_pattern`** atom + threshold composition |
| Edge Stretch | Same with `node.edge_stretch` | **`edge_detect`** + stretch primitive |
| Highlight Boost | Same with `node.highlight_boost` | Threshold + gain composition |
| Strobe | Same with `node.strobe` | `node.beat_gate` (registered, unused!) + gain |
| Transform | Same with `node.transform_effect` | Decomposes onto **`affine_transform`** atom |
| Voronoi Prism | Same with `node.voronoi_prism` | **`voronoi_2d`** (registered, unused) + prism shading atoms |

### 1.4 Atoms on the shelf

Registered primitives with zero current uses that are decomposition targets, not deletion candidates. These will get wired in by the bundles above:

- **For Bloom / Halation:** `mip_chain`
- **For Watercolor:** `flow_field_noise`, `uv_displace_by_flow`
- ~~**For PBR-shaded effects:** `cook_torrance_specular`, `equirect_envmap_sample`~~ — both activated 2026-05-27 by the MetallicGlass decomposition (3D-mesh mode via wired world_pos input). For future PBR-on-flat-surface presets they remain available in their original constant-V mode (no world_pos wired).
- **For SDF / coordinate work:** `centered_uv`, `polar_field`, `distance_to_point`, `field_combine`
- **For procedural texture:** `fbm_2d`, `perlin_noise_2d`, `simplex_noise_2d`, `voronoi_2d`, `checkerboard`
- **For per-pixel texture math:** `sin_term`, `trig_texture`, `power_texture`, `fract_texture`
- **For audio / temporal:** `envelope_follower_ar`, `peak`
- **For motion-aware effects:** `optical_flow_estimate`
- **For DNN / FFI bundles:** `depth_estimate_midas`, `blob_detect_ffi`, `blob_overlay_render`
- **For Strobe:** `beat_gate`
- **For mesh generators:** `render_3d_mesh`, `render_instanced_3d_mesh`, `generate_cube_mesh`, `generate_platonic_solid`, `generate_instance_transforms`
- **For particle systems:** `integrate_particles`

### 1.5 Deletion candidates (genuinely superseded)

These appear unused because they've been displaced by better primitives. Confirm-and-delete pass:

- `node.wgsl_compute_0in_1tex`, `node.wgsl_compute_1tex_1tex`, `node.wgsl_compute_2tex_1tex` — superseded by the introspected `node.wgsl_compute` used by BlackHole.
- Verify and delete if confirmed superseded: `node.tone_map` (vs `node.reinhard_tone_map`), `node.wet_dry` (vs `node.mix`), `node.sample` (general texture sample — check whether something specific still needs it).
- Likely retain: `node.color_lut`, `node.color_ramp`, `node.channel_mix` (registered for future color-grading workflows; not deletion candidates yet).

---

## 2. Tranche order

The tranches are sequenced by parity-test difficulty and atom-activation payoff, cheapest first.

### Tranche 1 — Texture-domain fused pairs (afternoon-scale each)

Decompose `fluid_gradient_rotate` and `fluid_gradient_curl_3d`. Both are pure texture-in / texture-out operations with no per-particle state, so parity testing is straightforward. Each needs one new small atom (`rotate_vec2_by_angle`, `curl_from_gradient_3d`) and otherwise composes from existing atoms.

### Tranche 2 — Deletable orphans

Delete the three superseded `wgsl_compute_*` variants. Confirm-then-delete `node.tone_map`, `node.wet_dry`, `node.sample` if redundant. Update presets if any reference them (the unused list says none do today, but verify in the same PR). Pure deletion, no parity concern.

### Tranche 3 — Curated math kernels (atom decomposition first)

Decompose `plasma_pattern_2d`, `shape_2d`, `star_field_2d`, `generate_lissajous`. **Default to atom decomposition** per `DECOMPOSING_GENERATORS.md` §5.6 — TD's CHOP-pattern is the reference (Lissajous in TD is Pattern → Math → Function → Merge as visible graph nodes, not a sealed curated kernel). Each decomposition activates several unused noise/math/coordinate atoms (`centered_uv`, `sin_term`, `hash_noise_field_2d`, `distance_to_point`, etc.) and may require new small array-math atoms (`generate_range`, `array_trig`, `pack_curve_xy`) that become permanent vocabulary for future parametric curves and procedural patterns. Only fall back to `wgsl_compute`-backed curated family for variants whose math is genuinely tightly-coupled register state (rare in this tranche — the attractor family and PBR specular are the canonical doorway cases, not this group).

**`plasma_pattern_2d` is deferred from this tranche pending the graph compiler initiative ([`GRAPH_COMPILER.md`](GRAPH_COMPILER.md)).** Its Noise + Fractal variants are per-pixel iteration loops that force a choice between unrolling into ~50 nodes each or shipping single-use bespoke loop atoms — both bad shapes. The compiler's `node.for_each_n` makes per-pixel loops graph-expressible and Plasma becomes its first real test bed. The other three entries (`shape_2d`, `star_field_2d`, `generate_lissajous`) don't hit the loop wall and proceed independently.

### Tranche 4 — Mesh monoliths

Decompose `nested_cubes_geometry` (atoms already exist unused — fastest single mesh decomposition), then ~~`render_3d_mesh_pbr_ibl`~~ (**done 2026-05-27** — activated the PBR-IBL atom family in 3D-mesh mode; see §1.1 entry), then `digital_plants_render` (partial — the geometry path collapses onto `render_instanced_3d_mesh`, the styling stays as `wgsl_compute`-backed bespoke shader). Unify `cylinder_wrap_field` + `torus_wrap_field` into `wrap_field` with surface enum in the same tranche.

### Tranche 5 — 4D + curve unifications

Audit `generate_tesseract_vertices` + `generate_duocylinder_vertices` + `wireframe_shape` per §5.6 — if each shape's vertex math splits into closed-form atoms (likely for the 4D polytopes since their vertex lists are short closed-form expressions), atom decompose and unify under a shared vertex-pipeline atom family. Fall back to `wgsl_compute`-backed curated family only if shape topology genuinely doesn't atomize. Atom-decompose `polygon_shape` + `concentric_outlines` — both are array-math operations that should share atoms with the Lissajous decomposition (Tranche 3).

**Audit converged 2026-05-26: the 4D pair splits across two mathematical classes.** Duocylinder is a (u, v)-parametric surface — decomposed onto a new authoring atom family (`generate_grid_uv` + `pack_vec4` + `edges_from_grid_uv`) shared with future parametric surfaces (torus / Klein / geodesic sphere / terrain). Tesseract is a closed polytope (regular 4-polytope, hypercube bit-flip topology) — same class as the 3D Platonic solids, deferred pending a sibling `hypercube_vertices(d)` + `edges_from_hypercube(d)` atom family. The "generic 4D polytope vertex pipeline" framing was wrong because the two shapes have structurally different parameterizations (Duocylinder needs `n²` grid sampling, Tesseract needs `2^d` corner enumeration). Atom inventory recorded in §1.1 above.

### Tranche 6 — Per-particle pipelines

Decompose `fluid_simulate`, `fluid_simulate_3d`, and `integrate_particles_attractor`. Each needs new atoms; many can be shared across the three.

- ~~**`integrate_particles_attractor`**~~ — **Done 2026-05-26.** Atomized onto `node.wgsl_compute` (JSON-editable shader, BlackHole-shape) with `switch (attractor_type)` covering all 5 variants + new `node.array_diffuse_particles` atom for the extracted diffusion. The originally-proposed `array_eval_ode` + `array_axpy` + `array_project_3d` decomposition was too granular per §1 — Lorenz ODE step is the explicit example of irreducible-math-at-single-dispatch-granularity. The wgsl_compute shape matches TouchDesigner's TOPs+Feedback pattern (one dispatch per substep, math in shader code, state in aliased buffer). Adding new attractors is a JSON edit. See §1.1 entry above.
- **`fluid_simulate`** — `integrate_particles` (already registered, unused) + `apply_noise_advection` (new) + `apply_diffusion` (new) + `apply_injection_burst` (new). Per-particle dispatches at sane granularity, all reusable for non-fluid particle effects (sparks, fountains, swarms).
- **`fluid_simulate_3d`** — same shape with 3D-domain atoms.

Parity testing requires bounded-not-bit-exact tolerance for particle-sim accumulation drift; verify visually against the canonical fixture. Heaviest tranche; week-scale per pipeline.

### Tranche 7 — Legacy Rust effects

Decompose the six wrapped legacy effects (`auto_gain`, `blob_track`, `wireframe_depth`, `depth_of_field`, `infrared`, `quad_mirror`). DNN / FFI / CPU work activates already-registered primitives; the rest of each effect composes from existing atoms plus one or two new ones per effect (`one_euro_filter`, `tilt_shift_mask`, `radial_mask`, `character_color`).

### Tranche 8 — Composite effect presets

Decompose the thin-wrap composite presets — Bloom, Halation, Watercolor, Glitch, Kaleidoscope, Color Grade, Chromatic Aberration, Dither, Edge Stretch, Highlight Boost, Strobe, Transform, Voronoi Prism. Many decompose onto atoms already shipped (Kaleidoscope onto `kaleido_fold`, Transform onto `affine_transform`, Strobe onto `beat_gate`, etc.). The bigger ones (Bloom, Halation, Watercolor) activate the high-value unused atoms (`mip_chain`, `flow_field_noise`, `uv_displace_by_flow`).

---

## 3. Parity-test strategy

Each decomposition needs:

1. **gpu_tests parity** on every new atom against a constant table or computed reference (CPU mirror is not the right shape — see [DECOMPOSING_GENERATORS.md §9 / `gpu_tests` module pattern](DECOMPOSING_GENERATORS.md)).
2. **End-to-end parity** of the decomposed graph against the legacy bundled primitive. Bit-exact where the legacy code's register precision can be matched with `Rgba32Float` intermediates; numerically-bounded with documented tolerance for stochastic sims (particle accumulation, atomic-add reshuffling).
3. **Visual sanity** against the canonical fixture `Liveschool Live Show V6 LEDS.manifold` before the bundle is deleted.
4. **`check-presets`** sub-second validator (`cargo run -p manifold-renderer --bin check-presets`) before app launch on any JSON edit, per the standard iteration loop.

The "fuse-for-parity" shortcut is not available. When parity drift appears at decomposition, the fix is intermediate-format precision (almost always `Rgba32Float` for the in-flight texture between two atoms that were previously fused), not re-fusing the primitives.

---

## 4. Workflow per bundle

For every entry in §1, follow:

1. Read the bundled primitive's `purpose:` and source end-to-end.
2. Identify which atoms it decomposes into (existing on shelf vs new). Reconcile against §1.4.
3. Build any new atoms in their own commit with `gpu_tests` parity.
4. Author the decomposed JSON preset (or update consumer presets to use the new atoms directly).
5. Run focused tests only — see §4.1.
6. Peter does visual sanity check vs the canonical fixture in the running app.
7. Delete the bundled primitive in the same commit as the consumer-rewire.
8. Commit + push.

Renames + `paramAliases` for any outer-card param renames so saved projects load unchanged.

### 4.1 Test discipline for the pass (focused only — no workspace runs)

Per `feedback_prefer_focused_tests`, **do not run `cargo test --workspace` per chat in this pass.** Workspace tests take 30+ minutes; across ~14 generators that's hours of waste for marginal additional coverage. Batch the workspace run at the end of the whole pass.

Per-chat test set:

- **`cargo run -p manifold-renderer --bin check-presets`** — sub-second JSON validator, run after every preset edit. Catches `UnknownParam`, `UnknownTypeId`, `ParamTypeMismatch`, cycles, etc.
- **`cargo test -p manifold-renderer --test parity <generator>::`** — focused parity for the generator under work. Bit-exact where the legacy register precision matches the intermediate texture format; numerically-bounded with documented tolerance for stochastic sims.
- **`cargo test -p manifold-renderer --lib node_graph::primitives::<atom>::gpu_tests::`** — for any new or extended atom, run its gpu_tests scope.
- **`cargo clippy -p manifold-renderer -- -D warnings`** — crate-scoped clippy. Fast and catches the relevant warnings for the renderer-only work.

Skip per chat:

- `cargo test --workspace` — batch at end of pass.
- `cargo clippy --workspace -- -D warnings` — crate-scoped clippy above is sufficient during the pass.

Peter does manual app sanity checks during the pass (loading the canonical fixture, eyeballing the visual). That + focused tests + check-presets is the per-chat correctness contract.

When the whole pass is done (all bundles in §1 cleared), one workspace run + workspace clippy as the final gate.

---

## 5. Open questions

- **Array-elementwise atom family** — Lissajous (Tranche 3) is the first consumer; the attractor family (Tranche 6) and the fluid sims (Tranche 6) reuse the same atoms. The first generator decomposed in this family establishes the shape (port types, capacity declaration, port-shadow conventions); subsequent generators ride on it.
- **One-euro filter** — needed for the Blob Track decomposition. CPU primitive, no GPU surface. Confirm the design before authoring (port shape, state lifecycle, AlphaBeta tuning surface).
- **Tilt-shift and radial mask primitives** — needed for DoF geometric branches. Probably one curated `node.focus_mask` primitive with a `mode` enum (Tilt-Shift / Radial) since the user knob shape is identical.
- **Spectral kernel shift** — needed for Halation's wavelength-shifted blur. Could be a new atom or a parameter on the existing `separable_gaussian`. Audit per `DECOMPOSING_GENERATORS.md §6.2` (extend before you build).
- **CoC primitive** — Circle-of-Confusion generation for DoF. Single curated primitive or decomposable into existing atoms? Audit on first DoF decomposition.

These are noted here as future-me reminders, not blocking — they get resolved when their tranche lands.

## 6. `wgsl_compute` generic outputs + `node.cast_as_*` atoms (done 2026-05-26)

**Status:** Shipped. `wgsl_compute` is now truly generic — emits `Array<Anonymous>` for every struct array output (including 64-byte Particle layouts and atomic-u32 accumulators). Six per-type cast atoms (`cast_as_particle`, `cast_as_u32`, `cast_as_mesh_vertex`, `cast_as_curve_point`, `cast_as_edge_pair`, `cast_as_instance_transform`) live in [`crates/manifold-renderer/src/node_graph/primitives/cast_array.rs`](../crates/manifold-renderer/src/node_graph/primitives/cast_array.rs) as explicit type-discipline nodes for users who want visible type-conversion boundaries in their graphs.

**Architectural decisions that actually shipped:**

- **Per-type cast atoms, not a configurable primitive with `target_type` enum.** The enum approach would have required dynamic output port types (param-driven), which the existing static `primitive!` macro doesn't support; adding that infra was disproportionate to the benefit. Each cast atom is ~30 lines, mostly boilerplate. Adding a new typed buffer is a ~30-line copy-paste block in `cast_array.rs`, same workload as the alternative would have been (one enum entry there vs. one block here).

- **Asymmetric wire validator relaxation — typed → Anonymous accepted, Anonymous → typed REJECTED.** The validator accepts `Array(KnownKind, size, align)` → `Array(Anonymous, same_size, same_align)` (typed producer flowing into a byte-buffer consumer is safe — the bytes are still what they are), but rejects the reverse direction (silently reinterpreting raw bytes as a specific struct layout is the dangerous one — `MyCustomVertex` getting read as `Particle` would be a silent semantic bug). See [`validation.rs::port_types_compatible`](../crates/manifold-renderer/src/node_graph/validation.rs#L216). Postel's-law shape: liberal in, conservative out. Cast atoms are **required** at every Anonymous → typed boundary, visible in the graph as `cast_as_particle` / `cast_as_u32` / etc. Two typed kinds (Particle ↔ MeshVertex) still don't connect — semantic mismatch.

- **Existing presets migrated** to insert cast atoms at every Anonymous → typed boundary. ComputeStrangeAttractor inserts one `cast_as_particle` between the integrator wgsl_compute and `array_diffuse_particles`. BlackHole inserts two `cast_as_u32` nodes between the polar splat's atomic accumulator outputs and the resolve_accumulators. The type-boundary is now visible in the graph editor.

- **`ArrayAnonymous(T)` macro arm added** to `__primitive_port_type!` — expands to `ArrayType::of::<T>()` (Anonymous-kinded, sized from T's layout). Used by the cast atoms to declare their input ports as Anonymous-of-specific-size via stub `Blob4` / `Blob8` / `Blob32` / `Blob64` Pod types. Lets cast atoms stay inside the `primitive!` macro instead of hand-writing the `EffectNode` trait impl.

**Migration impact:** Zero. The relaxed validator means BlackHole.json and ComputeStrangeAttractor.json work unchanged. Future presets can use cast atoms for explicit type discipline at wgsl_compute boundaries, or rely on implicit coercion.

**What's now unblocked:** any open-family `wgsl_compute` preset that wants to write a non-Particle typed buffer — wireframe-attractor extension (`Array<MeshVertex>`), parametric curve generators (`Array<CurvePoint>`), topology generators (`Array<EdgePair>`), instance-layout generators (`Array<InstanceTransform>`) — has the bridge it needs. Either the implicit-coercion path or the explicit cast-atom path works.

**Deferred (genuinely don't need yet):**

- **read_write usage disambiguation pass** — distinguishing aliased read+write from declared-read_write-but-write-only at the binding level. Separate concern; not part of this work. If a future case demands it, it lands then.
- **Dynamic-output-type infra** — would let one `cast_array` primitive carry all 6 type variants. Not built; the per-type-atom shape covers the same UX (each atom shows its target type in the palette and node title).
