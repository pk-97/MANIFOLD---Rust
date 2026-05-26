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
| `node.fluid_simulate` | Euler integration + density-adaptive simplex-noise advection + per-particle diffusion + injection burst + toroidal wrap | **`integrate_particles`** + *`apply_noise_advection`* + *`apply_diffusion`* + *`apply_injection_burst_to_particles`* |
| `node.fluid_simulate_3d` | Same as above in 3D + soft-container SDF repulsion + camera-aware flatten | *`integrate_particles_3d`* + *`apply_noise_advection_3d`* + *`apply_diffusion_3d`* + *`apply_container_repulsion`* + *`apply_camera_flatten`* |
| `node.fluid_gradient_rotate` | Central-difference gradient + 2D rotation (fused for FluidSim parity) | **`gradient_central_diff`** + *`rotate_vec2_by_angle`* (extension of `rotate_vec2_90`) |
| `node.fluid_gradient_curl_3d` | 3D central-difference gradient + curl-rotation by reference axis | *`gradient_central_diff_3d`* + *`curl_from_gradient_3d`* |
| `node.fluid_seed` | 7 curated geometric seed patterns (center cluster, lines, rings, cross, spiral, edge ring) | Decompose math into existing atoms *or* convert to `wgsl_compute`-backed curated family per `DECOMPOSING_GENERATORS.md` §5.6 |
| `node.fluid_seed_3d` | 8 curated 3D seed patterns | Same shape as 2D — curated-via-`wgsl_compute` |
| `node.integrate_particles_attractor` | RK2 + 5 attractor ODEs (Lorenz / Rössler / Aizawa / Thomas / Halvorsen) + 3D→2D projection | **Atom decompose.** `array_eval_ode` (curated enum picks attractor variant, single-dispatch per-particle closed-form math) + elementwise `array_math` / `array_axpy` for RK2 integration steps + `array_project_3d` for camera projection. Five dispatches per substep at sane granularity, all reusable. Shares the `array_math` / `array_axpy` atom family with the Lissajous and FluidSim decompositions. |
| `node.plasma_pattern_2d` | 8 plasma variants (sin-based math) | **Deferred 2026-05-26 pending the graph compiler initiative — see [`GRAPH_COMPILER.md`](GRAPH_COMPILER.md).** Audit landed: 6 of 8 variants atomize cleanly into ~75 nodes via existing atoms (`centered_uv`, `sin_term`, `texture_sum_5`, `rotate_2d`, `distance_to_point`, `compose`, `smoothstep_texture`) plus 1-2 small new vec2-domain helpers. The other 2 variants (Noise, Fractal) are per-pixel iteration loops that today force a choice between (a) ~50-node unroll per variant or (b) single-use bespoke loop atoms (`iterated_sin_fbm_2d`, `iterated_sin_warp_2d`) that are exactly the per-shader-primitive-wrap anti-pattern. The graph compiler — WGSL inliner + `node.for_each_n` wrapper — makes per-pixel loops graph-expressible (Noise becomes a 4-atom body wrapped in `for_each_n`) and as a side effect collapses the 75-node atomized graph to ~8 fused dispatches at runtime. Plasma is the natural test bed: the parity test exists, the legacy primitive is intact for regression, and Noise/Fractal are the smallest interesting loop cases. Decomposition resumes once the compiler lands. |
| `node.shape_2d` | 3 SDF variants (Square / Diamond / Octagon) | **Atom decompose.** Decompose into **`distance_to_point`** + **`math`** + **`smoothstep_texture`** atoms. |
| `node.star_field_2d` | 4-layer parallax hash + threshold + aspect-corrected gaussian halos + multi-frequency twinkle | **Audit.** Hash + threshold + brightness math atomizes (`hash_noise_field_2d` + `node.filter` + `brightness` + `math`); the per-star gaussian halos and the layered depth-stagger may or may not — read the kernel before deciding. Atom decomposition preferred where it reaches. |
| ~~`node.generate_lissajous`~~ | ~~`(sin(a*t+φ), sin(b*t+ψ))` curve eval sampled across [0, 2π], with floor/ceil ratio blending~~ | **Done 2026-05-26** — atom-decomposed to TD-CHOP shape. Shipped atoms: `generate_range` (linspace), `pack_curve_xy` (two `Array<f32>` → `Array<CurvePoint>`, folds `PROJ_SCALE = 0.25`). Extensions: `node.math` += `Floor`/`Ceil` (ops 10/11), `node.array_math` += `Sin`/`Cos`/`Mix` (ops 11/12/13, with `op_is_binary` classifier so the non-contiguous Mix stays binary-truncated). Lissajous.json is now an 18-node inner graph (was 1 node + sealed primitive); the bracket-interp morphing is visible as `math(Floor/Ceil/Sub) → array_math(ScaleOffset+Sin)×2 → array_math(Mix)` per axis. Atoms are reusable for Rose / Hypotrochoid / `polygon_shape` (Tranche 5). Legacy `generate_lissajous.rs` + `.wgsl` deleted. |
| ~~`node.polygon_shape`~~ | ~~Regular polygon vertex generation~~ | **Done 2026-05-26** — atom-decomposed onto the Lissajous atom family. Polygon outline is now `generate_range(end_inclusive=false, active_count=N) → array_math(Cos/Sin) → array_math(ScaleOffset, scale=size) → pack_curve_xy(scale=4.0 cancels PROJ_SCALE)`; closed-loop edges synthesised via new `consecutive_edges` atom. Extension: `generate_range` += `active_count` input + `end_inclusive` param (default true preserves Lissajous bit-exact). Legacy `polygon_shape.rs` deleted. ClipTriggerCycle path was unused by the only consumer (ConcentricTunnel cycles externally); dead-code removal. Mesh fan-triangulation output is dropped (no current consumer); revive as a `fan_triangulate_outline` atom if a future preset needs solid polygon fill. |
| ~~`node.concentric_outlines`~~ | ~~Ring stacker for polygon~~ | **Done 2026-05-26** — atom-decomposed. New atom: `node.array_replicate_polyline_rings(outline, edges, scales) → (outline, edges)`. Per-ring uniform scale on `Array<CurvePoint>`, per-ring index shift on `Array<EdgePair>` with sentinel preservation. Per-ring scales = `generate_range(0..K-1) → math(Floor/Subtract on expansion) + math(Multiply with ring_spacing) → array_math(ScaleOffset)`. Reusable for any "K transformed copies of a polyline" pattern (concentric spirals, replicated curve trails, instanced wireframe outlines). Legacy `concentric_outlines.rs` deleted. |
| `node.wireframe_shape` | Curated 3D wireframe shapes (cube, octahedron, etc.) | **Audit.** If each shape's vertex math is a small closed-form expression, atom decompose with per-shape vertex generators + shared `edges_from_polytope` plumbing. If shape topology requires per-shape branching that doesn't atomize, doorway as `wgsl_compute`-backed curated family. |
| `node.generate_tesseract_vertices` | Tesseract 4D vertex generation | **Audit.** Same as wireframe_shape — closed-form per-shape math probably atomizes (16 vertices of a tesseract is a closed-form list); shape topology may or may not. If atomizable, unify with duocylinder as a generic 4D polytope vertex pipeline. |
| `node.generate_duocylinder_vertices` | Duocylinder 4D vertex generation | **Audit.** Same as Tesseract. |
| `node.nested_cubes_geometry` | Cycled-pose cube instance generator | **`generate_cube_mesh`** + **`cycle_table_row`** + **`mux_array`** + **`generate_instance_transforms`** (all already registered, currently unused) |
| `node.digital_plants_render` | Bespoke terminal renderer for DigitalPlants | **`render_instanced_3d_mesh`** + bespoke lighting via `wgsl_compute` for the plant-specific styling |
| `node.render_3d_mesh_pbr_ibl` | PBR + IBL bundled with mesh render | **`render_3d_mesh`** + **`cook_torrance_specular`** + **`equirect_envmap_sample`** + **`bake_equirect_envmap`** (atoms already exist) |
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
- **For PBR-shaded effects:** `cook_torrance_specular`, `equirect_envmap_sample`
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

Decompose `nested_cubes_geometry` (atoms already exist unused — fastest single mesh decomposition), then `render_3d_mesh_pbr_ibl` (activates the PBR-IBL atom family), then `digital_plants_render` (partial — the geometry path collapses onto `render_instanced_3d_mesh`, the styling stays as `wgsl_compute`-backed bespoke shader). Unify `cylinder_wrap_field` + `torus_wrap_field` into `wrap_field` with surface enum in the same tranche.

### Tranche 5 — 4D + curve unifications

Audit `generate_tesseract_vertices` + `generate_duocylinder_vertices` + `wireframe_shape` per §5.6 — if each shape's vertex math splits into closed-form atoms (likely for the 4D polytopes since their vertex lists are short closed-form expressions), atom decompose and unify under a shared vertex-pipeline atom family. Fall back to `wgsl_compute`-backed curated family only if shape topology genuinely doesn't atomize. Atom-decompose `polygon_shape` + `concentric_outlines` — both are array-math operations that should share atoms with the Lissajous decomposition (Tranche 3).

### Tranche 6 — Per-particle pipelines

Decompose `fluid_simulate`, `fluid_simulate_3d`, and `integrate_particles_attractor`. Each needs new atoms; many can be shared across the three.

- **`integrate_particles_attractor`** — `array_eval_ode` (curated enum picks attractor variant) + elementwise array math / `array_axpy` for RK2 integration + `array_project_3d` for camera projection. Shares the `array_math` / `array_axpy` atom family with Lissajous (Tranche 3) and the fluid sims.
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

## 6. `wgsl_compute` infrastructure (deferred indefinitely)

The agent that explored the Lissajous-as-`wgsl_compute`-backend path surfaced two real generalizations the `wgsl_compute` primitive would need to host non-particle typed buffers: a kind-by-name registry (replacing the stride-based Particle detection) and a read_write usage disambiguation pass (telling apart aliased read+write from declared-read_write-but-write-only). **This work is deferred indefinitely.** The audit framework converged on "every curated kernel atom-decomposes" — there's no curated kernel in the inventory that genuinely needs the `wgsl_compute`-backed-curated-family pattern. `wgsl_compute` remains the escape hatch for novel user-authored kernels (BlackHole's existing use), and that use case doesn't need these generalizations because it's been working with Particle buffers and aliased read+write. If a future situation actually demands non-Particle typed output from `wgsl_compute` (a user authoring a custom curve generator who doesn't want to compose from atoms), these generalizations land then. Not as part of any audit chat.
