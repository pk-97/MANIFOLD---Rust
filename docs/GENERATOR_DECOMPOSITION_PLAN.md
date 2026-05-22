# Generator Decomposition Plan (Batch 9)

**Status:** Draft, 2026-05-20. Per-generator decomposition into JSON-authored graphs using the shipping primitive vocabulary. Companion to `PRIMITIVE_LIBRARY_COMPLETION_PLAN.md` §Batch 9.

## State of play

- **Effects:** 29 JSON presets at [crates/manifold-renderer/assets/effect-presets/](../crates/manifold-renderer/assets/effect-presets/). Schema is `EffectGraphDef` v2 (with `PresetMetadata`). Multi-primitive precedent: [SoftFocusGraph.json](../crates/manifold-renderer/assets/effect-presets/SoftFocusGraph.json).
- **Generators:** 18 active, all using Rust `inventory::submit!` factory registration. **There is no JSON preset path for generators yet** — this is the largest piece of infra to build.
- **Primitives:** ~95 shipped covering particle, mesh, line, procedural-texture math, FFI DNN, plus the WGSL escape hatch (3 texture variants — `0in_1tex`, `1tex_1tex`, `2tex_1tex`).

---

## Design decisions needed before mechanical execution

### D1 — Generator preset directory + boundary node

Effects load from `assets/effect-presets/` and start with `system.source` (the upstream texture). Generators don't have an upstream texture — they produce one — and they need access to `time`, `beat`, `aspect`, `trigger_count`, and `anim_progress` which currently flow through `GeneratorContext`, not `EffectNodeContext`.

**Proposal:**
- New folder: `crates/manifold-renderer/assets/generator-presets/*.json` (same `EffectGraphDef` schema).
- New boundary node: `system.generator_input` — zero inputs, scalar outputs `time / beat / aspect / trigger_count / anim_progress`. Mirror of `system.source` for generator graphs.
- Loader shim: turn each JSON file into a `GeneratorTypeId`-registered generator that executes its graph against the target Texture2D. Replaces the `inventory::submit!` factory for migrated generators.

**Scope:** ~3–4h for the boundary node, the loader, and a "trivial passthrough" smoke-test preset (`generator_input → uv_field → final_output`).

### D2 — N-way mux primitive

Seven generators have trigger-cycled internal variants:

| Generator | Variants |
|---|---|
| Plasma | 8 patterns (Classic, Rings, Diamond, Warp, Cells, Noise, Fractal, Lattice) |
| ConcentricTunnel | 6 shapes (circle, square, triangle, …) |
| WireframeZoo | 5 platonic solids |
| Lissajous | 10 snap presets |
| OscilloscopeXY | 10 snap presets |
| FluidSim3D | 7 snap patterns |
| StrangeAttractor | 5 attractor types (Lorenz, Rössler, Aizawa, Thomas, Halvorsen) |

**Decision:** build the mux. It's a generally useful composition primitive — gates content swaps, A/B blending between sub-graphs, beat-driven branch selection — not just a SNAP workaround.

**Shape:**
- `node.mux_texture` — scalar selector input + N `Texture2D` inputs of the same shape → 1 `Texture2D` output.
- Parallel `node.mux_array` and `node.mux_scalar` for those port types.
- All branches dispatch every frame in v1. Wasted work when only one is selected, but cheap for plasma-class kernels and avoids runtime branching in the executor. Future planner pass can skip unselected branches when the selector is statically known.
- Selector type: scalar `Int` (round to nearest, clamp to `[0, N)`). Sourced from `system.generator_input.trigger_count` for SNAP behaviour.

**Variadic input shape:** novel — existing primitives all have fixed input counts. Two paths:
- Ship a `mux_2 / mux_4 / mux_8` family (simple, repetitive).
- Extend port declarations to support repeated inputs (foundation for other variadic primitives down the road: audio band sum, multi-source compose).

**Recommendation:** variadic ports. ~1 session of focused work covering the macro, the runtime port-validation, and the three mux primitives.

**Exception:** `integrate_particles_attractor` already encodes the 5-way switch as an enum param. StrangeAttractor decomposes through that, no mux needed there.

### D3 — Per-pass escape hatches with normal feedback wiring

Several generators are multi-pass with persistent frame-to-frame state. Earlier draft treated their feedback as "special" — incorrect. The feedback model is identical across them: ping-pong textures (`temporal_feedback` primitive), particle buffers (`array_feedback` primitive). What actually distinguishes these generators isn't feedback shape, it's **per-pass shader complexity** — reaction-diffusion math, geodesic tracing, Cook-Torrance PBR are too tightly coupled to express as compositions of per-pixel-math primitives.

**Decision:** each pass becomes its own `wgsl_compute_*` node (the existing shader code, lifted verbatim). Frame-to-frame state flows through `temporal_feedback` and `array_feedback` like any other graph. The "escape hatch" is per-pass, not per-generator.

| Generator | Passes/frame | State (wired through feedback primitives) |
|---|---|---|
| BlackHole | ~14 | particle buffer + deflection maps + density feedback ×4 |
| OilyFluid | 7 | ping-pong color/velocity |
| MetallicGlass | 6 | feedback ping-pong + processed ping-pong + env map |
| FluidSim2D | 5–7 | particle buffer + 4 intermediates |
| FluidSim3D | 7–14 | 800K particles + 3D volume textures |
| ParticleText | 5–7 | fluid state + text bitmap |

Each pass's WGSL gets lifted into a `wgsl_compute_*` node. Wires connect them in JSON; feedback primitives close the frame-to-frame loops. Graph size is 7–15 nodes per generator, mechanical work once the per-pass shaders are lifted.

**Genuinely-decomposable stateful generators** (Tier 2 below) are the ones whose per-pass math IS expressible through existing primitives:
- StrangeAttractor (`integrate_particles_attractor → scatter_particles → resolve_accumulator → reinhard_tone_map`)
- DigitalPlants (instanced render + shadow pass)
- MriVolume (sampling)
- NestedCubes (instanced render)

### D4 — Native precision preserved inside escape-hatch nodes

The fluid sims (and reaction-diffusion, geodesic tracing, PBR) earned their stability through hand-tuned per-pass texture formats — `rgba16float` here, `r32float` there, `rgba32float` for the velocity buffer. Frame-to-frame feedback loops compound any precision loss at each pass boundary, so the format mix matters.

**Decision:** keep native precision by lifting shaders into `wgsl_compute_*` nodes verbatim AND lifting the graph runtime's current "one format per backend" constraint (see D5). The format declarations stay inside each node's `texture_storage_2d<...>` binding; the runtime allocates intermediate textures matching each producer's declared format.

This is the same conclusion as D3 from a different angle: per-pass shaders stay verbatim, only the wiring becomes JSON. Bit-exact parity is preserved automatically because the shader code is the same and the intermediate texture formats match the legacy pipeline exactly.

### D5 — Per-slot format declaration in the backend

The graph runtime's [`MetalBackend`](../crates/manifold-renderer/src/node_graph/metal_backend.rs) currently allocates every Texture2D slot at one shared format (set at backend construction, `Rgba16Float` in practice). The slot-recycling pool is keyed by `PortType` only. This was a "ship the simple version" call when the runtime was built — fine for everything that's shipped, fatal for fluid sims if we try to decompose them.

**Decision:** lift the constraint. Add `fn output_format(&self, port: &str) -> Option<GpuTextureFormat>` to the `Primitive` trait (defaults to `None` = backend default). Add an optional `outputFormats` map on `EffectGraphNode` so JSON-authored nodes (especially the `wgsl_compute_*` escape hatches) can override. Rekey the backend's slot recycling on `(PortType, GpuTextureFormat)`.

After D5 lands:
- Most existing primitives don't declare a format — they keep using `rgba16float` (correct for color/video).
- Tier 3 escape-hatch nodes declare their native formats (`r32float`, `rgba32float`, whatever the legacy pass used). Boundary precision matches legacy exactly.
- Tier 1 / Tier 2 parity bars upgrade from "≤ 5% mean abs error" to bit-exact, because the boundary format matches the legacy pipeline.

**~4h** of careful work in the infra session. Touches: `Primitive` trait, `MetalBackend` allocate path + slot-recycling map, `EffectGraphNode` schema, persistence round-trip tests.

---

## Per-generator decomposition

### Tier 1 — Trivial (single-shader, no persistent state, mux handles variants)

Decomposes through existing primitives. ~5–80 nodes per graph (variant-switching generators are bigger because each variant is its own sub-graph muxed at the output).

| Generator | Topology |
|---|---|
| **BasicShapesSnap** | `generator_input → distance_to_point → scale_offset_texture → threshold → final_output` (~6). 4 shape sub-graphs muxed by SNAP. |
| **Plasma** | 8 sub-graphs (Classic = `sin_texture ×4 → compose:add ×3 → lut1d`; Rings/Diamond/Warp/Cells/Noise/Fractal/Lattice each ~10–30 nodes) → `mux_texture(8) → final_output`. Selector = `trigger_count`. ~120 nodes total but mechanical. |
| **ConcentricTunnel** | 6 shape sub-graphs muxed → `final_output`. Circle = `distance_to_point → scale_offset → sin_texture → abs_texture → threshold`. ~50 nodes. |
| **StarField** | `voronoi_2d → fract_texture → power_texture → scale_offset → compose:multiply (warmth tint) → final_output` (~8). No variants. |
| **Lissajous** | `generator_input → generate_parametric_curve → render_lines → final_output` (~5). 10 sub-graphs (different freq ratios) muxed by SNAP. |
| **OscilloscopeXY** | **Skipped — likely future removal or rework.** Not used in the canonical Liveschool fixture (zero layers) and the math doesn't decompose cleanly into the existing vocabulary: it's two superposed Lissajous curves (main + 0.3 × harmonic with axis-asymmetric phase scaling — `phase × 1.3` on x, `phase × 0.7` on y) where the beat-driven mode hash-walks a custom 10-row ratio table with linear interpolation between adjacent beats (no existing primitive, and the table is different from `node.frequency_ratio`'s Lissajous table). A genuine decomposition would need two new speculative-reuse primitives (`beat_random_ratio` for the hash-walk-with-interp, `linepoint_blend` for the two-curve mix) plus a `phase_y` extension to `generate_lissajous` plus a second ratio table — all to migrate one generator that isn't used in the show. Per §1's "don't decompose for its own sake" rule, the right call is to revisit the visual goal (is the audio-scope look one Peter wants to keep? Or replace it with a different curve generator that DOES compose from the existing vocabulary?) before paying the migration tax. **Status: legacy Rust generator remains in place until then.** Skipped 2026-05-22. |
| **Tesseract** | `generate_tesseract_vertices [NEW] → rotate_4d → project_4d → render_lines → final_output` (~6) |
| **Duocylinder** | `generate_duocylinder_vertices [NEW] → rotate_4d → project_4d → render_lines → final_output` (~6) |
| **WireframeZoo** | `generate_platonic_solid → rotate_3d → project_3d → render_lines → final_output` (~5). 5 solid sub-graphs muxed. |
| **NestedCubes** | `generate_cube_mesh → generate_instance_transforms (preset poses) → render_instanced_3d_mesh → final_output` (~5) |

**New primitives for this tier (built in the infra step):**
- `node.mux_texture`, `node.mux_array`, `node.mux_scalar` — see D2.
- `node.generate_tesseract_vertices` — 32-edge tesseract topology, port from [tesseract.rs](../crates/manifold-renderer/src/generators/tesseract.rs). ~1h.
- `node.generate_duocylinder_vertices` — duocylinder topology, port from [duocylinder.rs](../crates/manifold-renderer/src/generators/duocylinder.rs). ~1h.

Tier 1 total: ~12h including the new primitives. One session (long).

### Tier 2 — Stateful, decomposable through existing primitives

Per-pass math is robust at fp16 boundaries; existing primitives cover the shape.

| Generator | Topology sketch |
|---|---|
| **StrangeAttractor** | `seed_particles → integrate_particles_attractor (enum: Lorenz/Rössler/…) → scatter_particles → resolve_accumulator → reinhard_tone_map → final_output`. Particle state through `array_feedback` between frames. ~10 nodes. |
| **DigitalPlants** | `simplex_noise_2d → generate_instance_transforms (custom layout via noise) → neighbor_smooth → render_shadow_pass → render_instanced_with_shadow → final_output`. Instance buffer through `array_feedback`. ~15 nodes. |
| **MriVolume** | `mri_loader [NEW] → sample_volume_2d → channel_mix (window/level) → final_output`. Loader is async; primitive wraps [mri_volume_loader.rs](../crates/manifold-renderer/src/generators/mri_volume_loader.rs). ~5 nodes. |
| **NestedCubes** | `generate_cube_mesh → generate_instance_transforms (preset poses, EMA-smoothed angle param) → render_instanced_3d_mesh → final_output`. ~5 nodes. (Lives in Tier 1 too — listed here because preset-pose smoothing is its only "stateful" element.) |

**New primitives for this tier:**
- `node.mri_loader` — wraps `MriVolumeLoader` as a `Texture2D` producer with async slice loading. ~2h.

Tier 2 total: ~8h. One session.

### Tier 3 — Native-precision escape-hatch chains

These generators rely on hand-tuned per-pass texture formats and tightly-coupled shader math. Each pass becomes its own `wgsl_compute_*` node embedding the existing shader verbatim (preserves native precision per D4); frame-to-frame state flows through `temporal_feedback` / `array_feedback` wires.

| Generator | Pass chain |
|---|---|
| **FluidSim2D** | `fluid_seed_wgsl → fluid_simulate_wgsl → scatter_wgsl → resolve_wgsl → display_wgsl`. Particle buffer through `array_feedback`. ~7 nodes. |
| **FluidSim3D** | 7–14 passes (particle sim → scatter 3D → blur X/Y/Z → gradient/curl → blur vector X/Y/Z → sim → scatter 2D → resolve → display). ~14 nodes. |
| **ParticleText** | `text_rasterize [NEW] → seed_from_text → fluid_simulate_wgsl → resolve_wgsl → display_wgsl`. ~6 nodes. |
| **BlackHole** | ~14 passes (deflection bake → 6 blur passes → particle sim/scatter/resolve → polar blur ×2 → display). ~14 nodes. |
| **OilyFluid** | 7 passes (downsample → blur H/V → velocity → color → render) wired through ping-pong `temporal_feedback`. ~7 nodes. |
| **MetallicGlass** | 6 passes (blend → blur H/V → process → envmap → render displaced grid) wired through feedback + processed ping-pongs. ~6 nodes. |

**New primitives for this tier:**
- `node.text_rasterize` — wraps the CPU text rasterizer at [text.rs](../crates/manifold-renderer/src/generators/text.rs) as a `Texture2D` producer. ~2h.

Tier 3 work per generator: lift each pass's WGSL into a `wgsl_compute_*` node (preserving format declarations), wire them in JSON, close feedback loops with `temporal_feedback` / `array_feedback`. Mechanical but careful — texture format declarations and binding order matter. ~2h per generator + ~2h for `text_rasterize`. Total ~14h. Two sessions.

---

## Execution order

Each step ends with `cargo clippy --workspace -- -D warnings && cargo test -p manifold-renderer --lib` green and a commit.

1. **Infra session.** Generator preset directory + `system.generator_input` boundary node + loader shim (D1) + the three mux primitives + variadic port support (D2) + per-slot format declaration (D5) + `node.generate_tesseract_vertices` + `node.generate_duocylinder_vertices`. Smoke test: trivial passthrough generator (`generator_input → uv_field → final_output`) plus a 2-input `mux_texture` test plus a 2-node `rgba32float` chain that verifies the per-slot format path. **~12h, 1 session (long).**

2. **Tier 1 session.** All trivial generators with their mux'd variant sub-graphs. Order: Plasma Classic first (proves the procedural-math vocabulary) → BasicShapesSnap → StarField → ConcentricTunnel Circle → ConcentricTunnel full (mux'd) → Plasma full (mux'd) → Lissajous → OscilloscopeXY → Tesseract → Duocylinder → WireframeZoo → NestedCubes. **~12h, 1 session (long).**

3. **Tier 2 session.** StrangeAttractor (decomposition proof for stateful generators) → DigitalPlants → MriVolume → NestedCubes refinement. **~8h, 1 session.**

4. **Tier 3 session 1.** Fluid family: FluidSim2D → FluidSim3D → ParticleText. Pass-lifting + feedback wiring for the precision-critical generators. **~8h.**

5. **Tier 3 session 2.** BlackHole → OilyFluid → MetallicGlass. **~6h.**

6. **Cutover.** Delete the migrated generators' `inventory::submit!` factory entries and the Rust files. JSON presets become source of truth. **~1h, sweeps in with the last Tier 3 commit.**

---

## Validation gates

After each preset lands:
- `cargo test -p manifold-renderer --lib` green
- Manual UI smoke test: outer-card sliders modulate, picker shows the preset, SNAP works (where applicable)
- Visual parity against legacy output via `Liveschool Live Show V6 LEDS.manifold` canonical fixture (per `project_canonical_fixture_liveschool.md`)

**Parity bars by tier** (D5 makes all three achievable):
- **Tier 1** (decomposed math): bit-exact parity. Primitives compute at fp32 in-shader; intermediate texture format matches the legacy pipeline's `rgba16float` output. Drift indicates a math bug.
- **Tier 2** (primitive decomposition of stateful generators): bit-exact parity, modulo particle-seed RNG ordering. Verify by matching seeds and stepping deterministically.
- **Tier 3** (native-precision escape-hatch chains): bit-exact parity. Same shader code in each `wgsl_compute_*` node as the legacy pipeline; per-pass formats declared via D5; only the inter-pass wiring is JSON.

---

## Risks / open questions

- **`system.generator_input` plumbing.** Threading `trigger_count` through scalar wires changes how SNAP works at the primitive layer — currently the generator reads `ctx.trigger_count` directly. Primitive-side consumes it as a wired scalar output from `system.generator_input`, matching the FluidSim2D existing pattern.
- **Anim progress return.** Generators return a `f32 anim_progress` value (drives picker UI). JSON-graph generators need a way to surface this. Recommend a `system.generator_output` complement that takes both `Texture2D in` and `Scalar anim_progress`.
- **Variadic ports.** The mux primitive needs port declarations to support repeated inputs (per D2 recommendation). The macro and runtime currently assume fixed input counts; this is real infra work in the infra session.
- **Slot recycling under per-format pool.** D5 rekeys the slot pool on `(PortType, GpuTextureFormat)`. Most graphs will be uniform `rgba16float` so the pool behaviour matches today; the divergent-format case (Tier 3 fluid chains) needs at least one integration test to confirm slots aren't aliased across formats.

---

## Estimated scope

**~5–6 sessions total** for full migration:
- 1 session — infra (D1 + D2 + D5 + 2 vertex primitives)
- 1 session — Tier 1 (all trivial + mux'd variant generators)
- 1 session — Tier 2 (StrangeAttractor + DigitalPlants + MriVolume + NestedCubes)
- 2 sessions — Tier 3 (fluid family, then BlackHole/OilyFluid/MetallicGlass)
- Cutover sweeps in with the last commit

Aligned with parent plan's 3–6 session estimate for Batch 9.
