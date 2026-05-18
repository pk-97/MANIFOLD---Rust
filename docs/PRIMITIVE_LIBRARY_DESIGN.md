# Primitive Library Design — Phase 4a

**Status:** Draft 1, 2026-05-11. Implements §0 of [`EFFECT_RUNTIME_UNIFICATION.md`](EFFECT_RUNTIME_UNIFICATION.md).

**Goal:** A primitive library that humans and AI agents compose into custom visuals (TouchDesigner-style creative surface), while preserving pixel-exact reproduction of every existing effect and generator. The library is the product.

## 1. Design principles

1. **Pixel-perfect mathematically exact.** Every existing effect and generator must round-trip bit-identical bytes through its decomposed form. Where multi-pass decomposition introduces intermediate `Rgba16Float` rounding that the legacy single-pass shader doesn't have, the primitive is shipped as a *fused composite primitive* (one shader, one pass) rather than split. When the future fusion compiler (Phase 5) can re-merge adjacent primitives into one dispatch, those composites split into atomics.
2. **≥2-use filter for atomics, single-use OK for "the effect IS the primitive".** Don't build `BoxBlur` because nothing uses it. Do build `KaleidoFold` even though only Kaleidoscope uses it — because the effect itself becomes that primitive.
3. **Generators stay mostly monolithic.** Each generator is an algorithm-unique sim or procedural source; their decomposition value is in extracting *shared 3D infra* (camera, projection, line rasterizer, particle scatter/resolve), not in shattering each generator's algorithm.
4. **Monolithic remainders are first-class library members.** `BlobTrack`, `WireframeDepth`, `AutoGain`, `DoF-Depth` (the DNN variant), and most generators are custom `EffectNode`s in the same library, exposed with typed ports + named parameters. TouchDesigner does this too (DNN TOPs).
5. **AI-readable metadata.** Each primitive declares: semantic purpose docstring, typed ports, named parameters with ranges and units, an example preset graph that uses it. Without this metadata the AI composition surface is unusable.

## 2. Primitive catalog (43 primitives)

Format: `Name(in_ports) → out_ports` — parameters with ranges — source shader for pixel-exact math.

### 2.1 Source / Sink (2)

| Primitive | Signature | Notes |
|---|---|---|
| `Source` | `() → tex: Texture2D` | Host pre-binds the chain's input texture. **Exists.** |
| `FinalOutput` | `(tex: Texture2D) → ()` | Host pre-binds the chain's output. **Exists.** |

### 2.2 UV / Spatial (4)

| Primitive | Signature | Parameters | Source shader |
|---|---|---|---|
| `UVTransform` | `(in: T2D) → out: T2D` | `translate: vec2` `scale: vec2` `rotate: f32` `fold_mode: enum{None,X,Y,XY}` | `fx_transform.wgsl`, `fx_quad_mirror.wgsl`, Mirror composite |
| `KaleidoFold` | `(in: T2D) → out: T2D` | `segments: u32 [2,16]` `center: vec2` | `fx_kaleidoscope.wgsl` |
| `ClampStretch` | `(in: T2D) → out: T2D` | `source_width: f32 [0.1,0.9]` `mode: enum{H,V,Both}` | `fx_edge_stretch.wgsl` |
| `Sample` | `(in: T2D, uv: vec2) → rgba: vec4` | `filter: enum{Bilinear,Nearest}` `address: enum{Clamp,Repeat,Mirror}` | infrastructure |

### 2.3 Color (5)

| Primitive | Signature | Parameters | Source shader |
|---|---|---|---|
| `ColorGradeHSV` | `(in: T2D) → out: T2D` | `gain [0,2]` `saturation [0,2]` `hue [-180,180]` `contrast [0,2]` `colorize [0,1]` | `color_grade.wgsl` |
| `Threshold` | `(in: T2D) → out: T2D` | `threshold [0,1]` `knee [0,1]` `gain [0,5]` `mode: enum{Hard,SoftKnee}` | `bloom.wgsl` prefilter, `fx_halation.wgsl`, `hdr_boost_compute.wgsl` |
| `Invert` | `(in: T2D) → out: T2D` | `intensity [0,1]` | `invert_colors.wgsl` |
| `LUT1D` | `(in: T2D) → out: T2D` | `lut: Texture1D` (512×1 Rgba16F) `range [0,2]` `lut_idx: u32` | `fx_infrared.wgsl` |
| `DitherPattern` | `(in: T2D) → out: T2D` | `algorithm: enum{Bayer,Halftone,Lines,XHatch,Noise,Diamond}` `resolution: vec2` | `fx_dither.wgsl` |

### 2.4 Edge / Distortion fused composites (5)

These exist as composite primitives because their multi-pass decomposition would break pixel-exact parity vs the legacy single-pass shader.

| Primitive | Signature | Parameters | Source shader |
|---|---|---|---|
| `EdgeDetect` | `(in: T2D) → out: T2D` | `threshold [0,1]` `mode: enum{Sobel,Laplacian,FreiChen}` | `fx_edge_detect.wgsl` |
| `Glitch` | `(in: T2D) → out: T2D` | `speed [0.1,10]` `scanline [0,1]` `rgb_shift [0,0.05]` `block_size [4,64]` | `fx_glitch.wgsl` |
| `Strobe` | `(in: T2D, beat: f32) → out: T2D` | `rate_idx: u32 (NOTE_RATES)` `mode: enum{Opacity,White,Gain}` | `fx_strobe.wgsl` |
| `VoronoiPrism` | `(in: T2D, beat: f32) → out: T2D` | `cell_count [4,64]` `pop_in: f32 [0,1]` | `fx_voronoi_prism.wgsl` |
| `ChromaticOffset` | `(in: T2D) → out: T2D` | `offset [0,0.05]` `falloff [0,1]` `angle [0,360]` `mode: enum{Radial,Linear}` | `fx_chromatic_aberration.wgsl` |

### 2.5 Edge atomic (1)

| Primitive | Signature | Parameters | Source shader |
|---|---|---|---|
| `Sobel3` | `(in: T2D) → out: T2D` | — | extracted from `fx_edge_detect.wgsl`. Available for AI/user composition; **not** in pixel-exact recipes (parity prefers `EdgeDetect`). |

### 2.6 Blur (1)

| Primitive | Signature | Parameters | Source shader |
|---|---|---|---|
| `SeparableGaussian` | `(in: T2D) → out: T2D` | `kernel_size: enum{9,17,25}` `sigma: f32` `axis: enum{H,V}` | `bloom_compute.wgsl`, `fx_halation_compute.wgsl`, `fx_depth_of_field_compute.wgsl`, `fx_watercolor_compute.wgsl` |

### 2.7 Compositing (2)

| Primitive | Signature | Parameters | Source shader |
|---|---|---|---|
| `Mix` | `(a: T2D, b: T2D) → out: T2D` | `amount [0,1]` `mode: enum{Lerp,Screen,Add,Max,Multiply,Difference,Overlay}` | derived from existing blends; new shader |
| `WetDryMix` | `(dry: T2D, wet: T2D) → out: T2D` | `wet_dry [0,1]` | `wet_dry_lerp_compute.wgsl` |

### 2.8 Multi-pass infrastructure (3)

| Primitive | Signature | Parameters | Source shader |
|---|---|---|---|
| `MipChainDown` | `(in: T2D) → mips: Vec<T2D>` | `levels: u32` `min_size: u32` | `bloom_compute.wgsl` downsample |
| `MipChainUp` | `(mips: Vec<T2D>) → out: T2D` | `radius_at_zero: f32` `radius_at_one: f32` | `bloom_compute.wgsl` upsample |
| `Feedback` | `(in: T2D) → out: T2D` | `decay [0,1]` `transform: UVTransform` `blend: enum{Screen,Add,Max}` | `fx_stylized_feedback.wgsl`, `fx_watercolor_compute.wgsl` feedback path |

### 2.9 Distortion atomic (2)

| Primitive | Signature | Parameters | Source shader |
|---|---|---|---|
| `DisplacementMap` | `(in: T2D, displace: T2D) → out: T2D` | `weight: f32` `channels: enum{RG,RB,GB}` | `fx_watercolor_compute.wgsl` displace pass |
| `VoronoiCells` | `(uv: vec2) → cell_id: f32, cell_uv: vec2` | `cell_count [4,64]` `jitter [0,1]` | extracted from `fx_voronoi_prism.wgsl` |

### 2.10 Noise (1)

| Primitive | Signature | Parameters | Source shader |
|---|---|---|---|
| `PerlinFBM` | `(uv: vec2) → out: T2D` | `octaves [1,8]` `falloff [0,1]` `scale: f32` | `fx_watercolor_compute.wgsl` flow map; `noise_common.wgsl` |

### 2.11 Time / Beat (1)

| Primitive | Signature | Parameters | Source shader |
|---|---|---|---|
| `BeatGate` | `(beat: f32) → gate: f32` | `rate_idx: u32 (NOTE_RATES)` `duty [0,1]` | `fx_strobe.wgsl` |

### 2.12 Procedural sources (5)

Generator-side; each is a generator's algorithm as a graph primitive.

| Primitive | Signature | Source |
|---|---|---|
| `Plasma` | `(uv: vec2, t: f32) → out: T2D` | `plasma_compute.wgsl` (8 pattern variants via function constant) |
| `StarField` | `(uv: vec2, t: f32) → out: T2D` | `star_field.wgsl` |
| `ConcentricShapes` | `(uv: vec2, t: f32, beat: f32) → out: T2D` | `concentric_tunnel_compute.wgsl` |
| `ParametricSDF` | `(p3: vec3) → sdf: f32` (3D volume) | `parametric_surface_bake.wgsl` |
| `BasicShapes` | `(uv: vec2) → out: T2D` | `basic_shapes_snap_compute.wgsl` |

### 2.13 3D infrastructure (8)

| Primitive | Signature | Notes |
|---|---|---|
| `Camera3D` | `(params: CameraParams) → view, proj: mat4` | perspective + orthographic, used by every 3D generator |
| `Rotation3D` | `(axes: vec3) → rot: mat3` | Euler X/Y/Z |
| `Rotation4D` | `(angles: vec3) → rot: mat4` | stereo XY/ZW/XW (Tesseract, Duocylinder) |
| `Camera4D` | `(dist: f32) → proj: mat4` | 4D→3D perspective |
| `Raymarch` | `(volume: Texture3D, ray_origin, ray_dir, steps) → hit: vec4` | `parametric_surface_raymarch.wgsl` |
| `MeshRender` | `(verts: Buffer, indices: Buffer, view, proj) → out: T2D, depth: T2D` | `mesh_pipeline.wgsl`, `nested_cubes.wgsl` |
| `Shadow` | `(scene: Mesh, light: mat4) → shadow: T2D` | PCF; `digital_plants_shadow.wgsl`, `galactic_rock_shadow.wgsl` |
| `LineRasterize` | `(verts: Buffer, width: f32) → out: T2D` | `line_pipeline.wgsl`, `generator_lines.wgsl` |

### 2.14 Particle infrastructure (3)

| Primitive | Signature | Notes |
|---|---|---|
| `ParticleScatter` | `(particles: Buffer, accum: Buffer<u32>)` | atomic per-pixel accumulate; used by FluidSim, StrangeAttractor, BlackHole, GalacticRock, Mycelium |
| `ScatterResolve` | `(accum: Buffer<u32>) → density: T2D Rgba16F` | u32 → f16 normalization |
| `ParticleSimRK2` | `(particles: Buffer, dt: f32) → particles': Buffer` | RK2 ODE integration; StrangeAttractor, BlackHole, FluidSim3D |

## 3. Per-effect decomposition recipes

22 effects audited (Mirror + SoftFocusGraph already graph-backed).

| Effect | Decomposition | Risk |
|---|---|---|
| ChromaticAberration | `ChromaticOffset` | trivial — 1:1 primitive |
| ColorGrade | `ColorGradeHSV` | trivial |
| Dither | `DitherPattern` | trivial |
| EdgeDetect | `EdgeDetect` (fused) | composite — Sobel3+Threshold fused for parity |
| EdgeStretch | `ClampStretch` | trivial |
| Glitch | `Glitch` (fused) | composite |
| HDRBoost | `HighlightBoost` | trivial — distinct from Bloom prefilter's threshold math (boosts excess vs extracts highlights) |
| InvertColors | `Invert` | trivial |
| Kaleidoscope | `KaleidoFold` | trivial |
| QuadMirror | `UVTransform(fold=XY) → Mix` | check parity: legacy is 1 pass, decomposed is 2 — may need fused `QuadMirror` primitive |
| Strobe | `Strobe` (fused) | composite |
| Transform | `UVTransform` | trivial |
| VoronoiPrism | `VoronoiPrism` (fused) | composite |
| Infrared | `LUT1D` (10 palette LUTs as fixed assets) | trivial |
| Bloom | `Threshold(soft-knee, 0 blur) → MipChainDown → SeparableGaussian (per mip H+V) → MipChainUp → WetDryMix(input, blurred)` | multi-pass legacy → multi-pass graph; structure preserved |
| Halation | `Threshold(soft-knee) → ColorGradeHSV(hue+sat tint) → SeparableGaussian(17-tap H) → SeparableGaussian(17-tap V) → Mix(Add)` | structure preserved |
| Watercolor | `PerlinFBM(4 oct) → DisplacementMap → SeparableGaussian → Sobel3(slope) → SeparableGaussian(luma blur) → Feedback(decay) → WetDryMix` | 7-pass legacy → 7-pass graph; structure preserved |
| StylizedFeedback | already graph-backed (`Source → Feedback → FinalOutput`) | shipped |
| Mirror | already graph-backed (`Source → UVTransform(fold) → FinalOutput`) | shipped |
| SoftFocusGraph | already graph-backed | shipped |
| DoF-Geometric (tilt-shift / radial) | `Sobel3(coc) → SeparableGaussian (9/17/25) H → SeparableGaussian V → Mix(coc-weighted)` | new primitive `CoCFromBand` + `CoCFromRadial` may need to fuse with edge ops for parity |
| **AutoGain** | **monolithic** | CPU envelope follower with transient detection |
| **BlobTracking** | **monolithic** | Native plugin + One-Euro filter + font atlas |
| **WireframeDepth** | **monolithic** | 15 passes + 3 DNN workers |
| **DoF-Depth** | **monolithic** | MiDaS DNN |

**Tally:** 18 decompose, 4 monolithic.

## 4. Per-generator decomposition

Generators are mostly monolithic per design principle #3. Shared 3D infra (camera, rotations, line rasterize, particle scatter/resolve, mesh render, shadow) extracts; the algorithm of each generator stays as one custom node.

| Generator | Status | Uses shared primitives |
|---|---|---|
| Plasma | monolithic node; algorithm is the `Plasma` procedural primitive | — |
| StarField | monolithic node = `StarField` primitive | — |
| ConcentricTunnel | monolithic node = `ConcentricShapes` primitive | `BeatGate` |
| BasicShapesSnap | monolithic node = `BasicShapes` primitive | — |
| Text | monolithic | FFI text rasterizer |
| MriVolume | monolithic | FFI TIFF loader |
| Lissajous | monolithic | `LineRasterize` |
| OscilloscopeXY | monolithic | `LineRasterize` |
| Duocylinder | monolithic | `Rotation4D`, `Camera4D`, `LineRasterize` |
| Tesseract | monolithic | `Rotation4D`, `Camera4D`, `LineRasterize` |
| WireframeZoo | monolithic | `Rotation3D`, `Camera3D`, `LineRasterize` |
| NestedCubes | monolithic | `Camera3D`, `MeshRender` |
| ParametricSurface | monolithic | `ParametricSDF`, `Raymarch`, `Camera3D` |
| DigitalPlants | monolithic | `Camera3D`, `MeshRender`, `Shadow` |
| GalacticRock | monolithic | `Camera3D`, `MeshRender`, `Shadow`, `SeparableGaussian` |
| MetallicGlass | monolithic | `SeparableGaussian`, `Feedback`, `Sobel3` |
| BlackHole | monolithic | `Camera3D`, `ParticleScatter/Resolve`, `ParticleSimRK2`, `SeparableGaussian` |
| FluidSim2D | monolithic | `ParticleScatter/Resolve`, `ParticleSimRK2` |
| FluidSim3D | monolithic | `ParticleScatter/Resolve`, `ParticleSimRK2`, `Camera3D` |
| Mycelium | monolithic | `ParticleScatter/Resolve` |
| OilyFluid | monolithic | `SeparableGaussian`, `Feedback` |
| StrangeAttractor | monolithic | `ParticleScatter/Resolve`, `ParticleSimRK2`, `Camera3D` |
| ParticleText | monolithic | FFI text rasterizer + FluidSim2D internals |

**Tally:** 23 monolithic generators using shared infra primitives.

## 5. Parity test framework

**Goal:** automated, no visual inspection. Per-effect test renders legacy chain and decomposed graph against identical input + parameters, compares output bytes.

### 5.1 Test harness

```rust
// crates/manifold-renderer/tests/parity/mod.rs
fn assert_pixel_exact_parity(
    effect_type: EffectTypeId,
    test_input: TestInput,         // fixed input texture(s) + params + frame_time
) -> ParityResult {
    let gpu = headless_gpu();
    let legacy_out = render_legacy(effect_type, &test_input, &gpu);
    let graph_out  = render_decomposed(effect_type, &test_input, &gpu);
    let legacy_bytes = readback(&gpu, &legacy_out);
    let graph_bytes  = readback(&gpu, &graph_out);
    assert_bytewise_equal(legacy_bytes, graph_bytes)
}
```

### 5.2 Test inputs

Fixed test fixtures live in `crates/manifold-renderer/tests/parity/fixtures/`:

- `noise_512.bin` — deterministic Rgba16Float noise, 512×512.
- `gradient_256.bin` — RGB gradient + alpha sweep.
- `solid_colors.bin` — 8-color palette swatches for color-grade verification.
- `bright_spots.bin` — HDR fixture for threshold/bloom/halation paths.

Each test runs **all four fixtures** × **6 parameter combinations** (min, max, default, plus 3 mid-range) per effect. 24 comparisons per effect × 18 decomposable effects = 432 parity assertions.

### 5.3 Pass criteria

- **Bit-identical:** `legacy_bytes == graph_bytes` (every byte).
- **No tolerance.** A single byte difference fails. The decomposition is then either fixed (fuse primitives) or the effect is reclassified monolithic.

### 5.4 What this catches automatically

- Float ordering differences (e.g., `a*b+c` vs `c+a*b`).
- Constant mismatches (gamma 2.2 vs 2.4).
- Sampler state differences (linear vs nearest, clamp vs repeat).
- Intermediate format rounding (Rgba16Float fp16 quantization on extra passes).
- Workgroup shape differences affecting boundary pixels.

### 5.5 What it does NOT catch

- Performance regression (separate benchmark).
- Visual artifacts at unusual parameter values not in the 6 combinations (mitigated by including extremes).
- GPU-specific bugs that don't reproduce in headless test env.

## 6. Build order

Strict dependency-driven order. Each phase ships independently; rollback safe.

### 6.0 Parity test framework (1 commit)

Build `tests/parity/` infrastructure first. No primitives migrated yet — framework validates against trivial echo effect.

### 6.1 Trivial primitives (1 commit each, ~10 commits)

Single-pass legacy → single primitive. Bit-equal by construction (shader is the legacy shader, just registered as a primitive). Order by complexity:

1. `Invert` ← InvertColors
2. `ColorGradeHSV` ← ColorGrade
3. `ClampStretch` ← EdgeStretch
4. `KaleidoFold` ← Kaleidoscope
5. `UVTransform` (extend existing) ← Transform
6. `ChromaticOffset` ← ChromaticAberration
7. `DitherPattern` ← Dither
8. `LUT1D` ← Infrared
9. `Threshold` ← HDRBoost (no blur), Bloom prefilter
10. `EdgeDetect` (fused) ← EdgeDetect
11. `Strobe` (fused) ← Strobe
12. `VoronoiPrism` (fused) ← VoronoiPrism
13. `Glitch` (fused) ← Glitch

Each commit: primitive code + WGSL + preset graph replacing the effect + parity test asserting bit-equality.

### 6.2 Compositing primitives (2 commits)

14. `Mix` (all 7 blend modes)
15. `WetDryMix` ← existing wet_dry_lerp

### 6.3 Multi-pass primitives + effects (4 commits)

**Update 2026-05-11:** the original recipes assumed Bloom, Halation, and Watercolor could decompose into separable-Gaussian + mip-chain primitives. Auditing the legacy shaders showed:

- **Bloom** uses Unity-style Blur9 tent + Blur13 filmic kernels with a ping-ponging dual mip chain — no separable-Gaussian path.
- **Halation** fuses threshold-tint INTO the H Gaussian (per-tap, not as a pre-pass). Splitting it would store an fp16 intermediate texture and lose bit-exact parity (same reason Glitch was fused in §6.1).
- **Watercolor**'s edge blur is a 2D non-separable 9-tap — no separable-Gaussian path.

All three ship as fused composite primitives (same pattern as Glitch, Strobe, EdgeDetect, VoronoiPrism in §6.1). `MipChainDown` / `MipChainUp` are deferred — no §6.3 customer; they'll land when there's a real use case in §6.7+ or a future Bloom-style preset library. `SeparableGaussian` still ships because it's bit-exact for DoF (§6.4) and is useful as a user-facing composition primitive.

16. `SeparableGaussian` (for DoF + general user composition; not used by Bloom/Halation/Watercolor)
17. **Bloom** fused composite primitive (legacy `bloom_compute.wgsl` wrapped, owns mip pyramid state)
18. **Halation** fused composite primitive (legacy `fx_halation_compute.wgsl` wrapped)
19. **Watercolor** fused composite primitive (legacy `fx_watercolor_compute.wgsl` wrapped, owns ping-pong state)

### 6.4 DoF geometric split (2 commits)

20. Split `DoFGeometric` from `DoFDepth`: geometric variants decompose via Sobel/Gaussian; depth variant stays as monolithic node.
21. New monolithic `DoFDepth` node wrapping the DNN path.

### 6.5 Monolithic remainders as custom nodes (4 commits)

22. `AutoGainNode` — CPU envelope + GPU apply pass wrapped as a single `EffectNode`.
23. `BlobTrackNode` — native plugin + One-Euro + overlay render.
24. `WireframeDepthNode` — full 15-pass pipeline + DNN workers.
25. `DoFDepthNode` — MiDaS-based DoF (already #21, just confirms).

### 6.6 Effect-only runtime cutover + persistence (~6 commits — early play point)

**Strategy decision (2026-05-11): cut over effects first, defer generator decomposition to a separate pass.** Effects sit downstream of generators (texture → chain), so the chain side can be swapped without touching generator code. Smaller cutover batch, real-usage feedback informs the harder generator work that follows.

26. **[shipped]** Graph-JSON preset schema + loader. Bundled presets ship in `crates/manifold-renderer/assets/effect-presets/*.json` (embedded via `include_str!`); user-authored graphs save into the project file (`.manifold` archive) with an optional export-to-standalone-JSON path. One schema, one loader, one validator — built-in vs user graphs differ only in storage location. Drift detection + regenerator live in `tests/bundled_presets_drift.rs`.
27. **[shipped]** Effect save-file refactor: `EffectInstance` already carries `graph: Option<EffectGraphDef>` (`None` = use bundled preset, `Some` = per-card override) and `graph_version: u32` for cache invalidation. Catalog defaults now source from the bundled-preset registry, so per-card divergence is available on every effect (not just Mirror + SoftFocus). Edit commands (`AddGraphNode`, `RemoveGraphNode`, `ConnectPorts`, `DisconnectPorts`, `MoveGraphNode`, `SetGraphNodeParam`) all lift `None → Some(catalog_default)` on first edit. No project-version bump — `graph` is `skip_serializing_if = "Option::is_none"`, so unedited fixtures round-trip byte-identically.
28. **[shipped]** `EffectChain::apply_chain` is a thin wrapper over `ChainGraph::try_build` + `ChainGraph::run` — the graph-runtime path is the only path. Static elision via `ChainSpec::SkipMode::OnZero` (effects with `amount ≤ 0` are dropped from the plan) and wet/dry sub-graphs via `OpenGroup` + multi-segment `Mix` both ship. **Dynamic bypass: explicitly not planned (2026-05-17).** A per-frame `bypass_predicate` on `ExecutionStep` would preserve primitive state (Bloom mip pyramids, Watercolor feedback, Stylized Feedback trails) across `amount=0` crossings without a topology rebuild — but the current behavior (rebuild on flip, state lost) is acceptable for the show. Filed here as a future revisit if a live-perf use case (ducking-as-transition without losing trails) becomes load-bearing. The `EffectChain` shim itself disappears in #31.
29. **[shipped]** `GraphCanvas` editing affordances. Add (palette click → `AddGraphNode`), wire (drag output port → input port → `ConnectPorts`), disconnect (click connected input port → `DisconnectPorts` — gap closed in this commit), delete (Delete key on selected node → `RemoveGraphNode`), move (drag node header → `MoveGraphNode`), parameter set (right-sidebar inspector → `SetGraphNodeParam`). All flow through `manifold_editing::commands::graph::*` → undo stack → `Project` mutation; save-on-change is implicit because the Project is the live model and the standard save path serializes it.
30. **[shipped — minimum viable]** "Reset to Default" affordance in the graph editor header surfaces when the watched effect is diverged from its bundled preset (`instance.graph.is_some()`). One click emits `PanelAction::RevertEffectGraph` → `RevertEffectGraphCommand` (clears the override, undoable). The header label flips to "Live Graph — MODIFIED" so the diverged state is visible alongside the existing pink "MOD" badge on the effect card. The fuller "library browser" with named user-saved presets is deferred — bundled presets are the only library today, and the picker for that library is the implicit "Add Effect" catalog. User-saved named-preset support would add a `Project.preset_library` field + UI; not gated by §6.6.
31. **[shipped — EffectChain deletion]** `crates/manifold-renderer/src/effect_chain.rs` deleted. The shim was a single-field wrapper around `Option<ChainGraph>` with three thin methods (`apply_chain`, `clear_graph_runner_state`, `resize`). Replaced by a free-function module `chain_dispatch.rs` (`dispatch_chain`, `clear_chain_state`, counters + `take_chain_dispatch_stats`). `LayerCompositor` now stores `Option<ChainGraph>` directly in its per-layer / per-group / per-LED maps. Parity tests (29 effects, bit-exact) confirm the dispatch path is byte-identical. The "per-effect `EffectInstance.effect_type` enum surface" part stays as-is: `EffectTypeId` is not an actual enum — it's a `Cow<'static, str>` newtype used as the catalog key for bundled-preset lookup, `EffectMetadata` (OSC prefix, display name), and `ChainSpec` bindings/skip metadata. Its role as a sealed dispatch discriminant was already gone after the graph-runtime cutover.

`GraphSnapshot` and `GraphEditorPanel` already exist; this phase mostly wires them into editing flows and lays down the persistence path.

### 6.7+ Generator pass (separate, later)

Once §6.6 has shipped and we've used the effect graph system in anger, return to generators with that feedback. Plan placeholder (specifics revisit-able after §6.6):

- **G1** Generator shared-infra primitives (~8 commits): `Camera3D`, `Rotation3D/4D`, `LineRasterize`, `MeshRender`, `Shadow`, `ParticleScatter/Resolve`, `ParticleSimRK2`, `Raymarch`.
- **G2** Generator algorithm primitives (~5 commits): `Plasma`, `StarField`, `ConcentricShapes`, `ParametricSDF`, `BasicShapes`.
- **G3** Remaining generators as monolithic library nodes (~13 commits).
- **G4** Generator runtime cutover.

## 7. AI agent surface

Each primitive carries metadata sufficient for an AI to compose graphs:

```rust
pub struct PrimitiveDescription {
    pub name: &'static str,
    pub category: PrimitiveCategory,
    pub purpose: &'static str,                  // one-sentence semantic intent
    pub ports_in: &'static [PortSpec],          // typed input ports
    pub ports_out: &'static [PortSpec],         // typed output ports
    pub params: &'static [ParamSpec],           // named parameters with ranges + units
    pub examples: &'static [&'static str],      // preset graph names that use this primitive
    pub composition_notes: &'static str,        // when to use vs alternatives
}
```

This is the JSON shape an AI agent reads to learn what's available. The composition surface is the existing graph-serialization format (the same one preset graphs use).

## 8. Open questions parked

- **Generator parity testing.** Effects have a clean "render at fixed input + params" surface. Generators are pure outputs — parity tests them at fixed `time`, `beat`, `resolution`. Some generators have RNG state (Mycelium agents, fluid particle init) that needs deterministic seeding for parity. **Resolve in §6.6.**
- **Preset graph format vs project file format.** Preset graphs are a subset of the existing project-file graph format. Whether presets live as embedded JSON in the binary or as files in `assets/effect-presets/` is a §6.1 detail.
- **Versioning of preset graphs.** When a primitive's parameter set changes, old presets break. **Use `ParamAlias` mechanism from Phase 2.**

---

**Next concrete step:** §6.0 — build the parity test framework. Without it nothing downstream is verifiable.

---

## 9. Naming + UX audit (2026-05-17, ongoing)

**Goal:** before the next round of authoring on top of the graph-editor work, sweep the user-facing names, ranges, and defaults so the surface reads coherently. Carried out one layer at a time; audit findings here drive a per-effect / per-primitive rename pass executed through the rename script (built once we know the rename volume).

### 9.1 Layer 1 — Outer-card slider surface (25 effects)

The card UI surface comes from two sources that must agree:

- `EffectMetadata.params: &[ParamSpec]` — the slider definition (id, label, range, default, format, unit string). Drives the card render and OSC.
- `ChainSpec.bindings: &[ParamBinding]` — the routing from each outer slider to an inner-node param. Has its own `id` + `label` (must match `EffectMetadata.params`) and `target.param` (the inner-node param name).

Inventory captured (raw): every shipping effect's outer-card surface. Findings below; per-effect change table in §9.1.4.

#### 9.1.1 Truncated labels — drop the abbreviation tax

Most labels still wear the Unity ~12-pixel-column budget. We no longer have that constraint; the card lays out flexibly. Worth restoring full English everywhere:

| Effect | Current label | Proposed label |
|---|---|---|
| Kaleidoscope | `Segs` | `Segments` |
| Edge Stretch | `Dir` | `Direction` |
| Dither | `Algo` | `Pattern` (more honest than `Algorithm`) |
| HDR Boost | `Thresh` | `Threshold` |
| Edge Detect | `Thresh` | `Threshold` |
| Halation | `Thresh` | `Threshold` |
| Color Grade | `Sat` | `Saturation` |
| Color Grade | `TintHue` | `Tint Hue` |
| Color Grade | `TintSat` | `Tint Saturation` |
| Color Grade | `Focus` | `Tint Focus` (clarifies which "focus" — confusion with DoF.Focus) |
| Halation | `Sat` | `Saturation` |
| Transform | `Rot` | `Rotation` |
| Auto Gain | `Char` | `Character` |
| Auto Gain | `HDR Ret` | `HDR Retention` |
| Blob Track | `Sens` | `Sensitivity` |
| Blob Track | `Smooth` | `Smoothing` |
| Wireframe Depth | `ZScale` | `Z Scale` |
| Wireframe Depth | `WireRes` | `Wire Resolution` |
| Wireframe Depth | `MeshRate` | `Mesh Rate` |
| Wireframe Depth | `EdgeFollow` | `Edge Follow` |
| Glitch | `RGB Shift` | (keep — RGB is canonical) |
| Glitch | `Block` | `Block Size` |

Param **id**s on the wire follow snake_case English: `segments`, `direction`, `pattern`, `threshold`, `saturation`, `tint_hue`, `tint_saturation`, `tint_focus`, `rotation`, `character`, `hdr_retention`, `sensitivity`, `smoothing`, `z_scale`, `wire_resolution`, `mesh_rate`, `edge_follow`, `block_size`.

#### 9.1.2 Outer ↔ inner param name divergence

Today the outer slider has a short id (`thresh`, `algo`, `dir`, `sens`, `rot`) but the binding routes to an inner-node param with the full word (`threshold`, `algorithm`, `mode`, `sens`, `rotation`). The label-rename in §9.1.1 naturally collapses this — the outer id becomes the inner id, and the `target.param` mapping in the binding is a no-op:

```rust
// Before
ParamBinding { id: "thresh", target: HandleNode { param: "threshold" } }

// After
ParamBinding { id: "threshold", target: HandleNode { param: "threshold" } }
```

The remaining intentional divergences (where the outer name is genuinely different from the inner) are few:

- `EdgeStretch.dir` → `ClampStretch.mode` — the outer asks "which direction does this stretch?", the inner is named "mode" because the primitive is a generic clamp-stretch with 3 modes. Either keep this divergence (outer label = "Direction", inner param = `mode`), or rename the inner `mode` → `direction`. Recommended: rename the primitive's param. Inner naming should describe what the param means, not what it is.

#### 9.1.3 Display name ↔ type id mismatches

- `EffectTypeId::HDR_BOOST` (file `hdr_boost.rs`, on-disk string `HdrBoost`) but display name is `"Highlight Boost"`. The internal name and the user-facing name disagree. **Recommendation:** rename internally to `HighlightBoost`. The save file gets a `legacy_value_aliases`-style migration: old `"HdrBoost"` → new `"HighlightBoost"`.
- `EffectTypeId::EDGE_DETECT` value is `"EdgeGlow"` but the const is `EDGE_DETECT` and the display name is `"Edge Detect"`. The string is from a long-ago rename. **Recommendation:** migrate the on-disk type id from `"EdgeGlow"` to `"EdgeDetect"` (matches display + const).
- `EffectTypeId::new("Watercolor")` (file `watercolor.rs`) uses raw `EffectTypeId::new` rather than a `pub const WATERCOLOR`. Minor — add the const for consistency with every other effect.

#### 9.1.4 Categories — small, mostly OK

Current categories: `Spatial`, `Post-Process`, `Filmic`, `Surveillance`. Findings:

- `Spatial` has only Transform and InvertColors. **Invert is a color operation, not a spatial one.** Move it to a new `Color` category — joined by ColorGrade, Infrared, Dither (which is technically Color+Spatial but renders as a color-quantization pass).
- `Surveillance` has only Infrared, plus Blob Track is `Post-Process` despite the surveillance-y aesthetic. Either rename `Surveillance` → `Scientific` and pull Blob Track + Wireframe Depth into it, or fold Infrared into `Color`. Recommendation: **fold Infrared into Color**, **rename Surveillance → Diagnostic**, **move Blob Track + Wireframe Depth into Diagnostic** (they both annotate the image with overlays, that's the genre).
- `Filmic` (Bloom, Halation, Chromatic Aberration, HDR Boost / Highlight Boost, DoF, Glitch) reads consistently — keep.
- `Post-Process` is the dump-everything-else category. Consider splitting:
  - `Stylize` (Watercolor, Stylized Feedback, Soft Focus, Kaleidoscope, Mirror, Quad Mirror, Edge Stretch, Voronoi Prism, Strobe, Auto Gain)
  - `Color` (Color Grade, Invert, Infrared, Dither)
  - `Spatial` (Transform — alone, but it's load-bearing)
  - `Diagnostic` (Edge Detect, Blob Track, Wireframe Depth)
- `Edge Detect` is currently `Post-Process` — recommend `Diagnostic` (it's a "see the structure" tool, not a stylize).

Proposed final categories: **Spatial**, **Color**, **Stylize**, **Filmic**, **Diagnostic**. Five buckets, each populated.

#### 9.1.5 Default `amount` — pick a rule

Today inconsistent. About half default to 0 (so adding does nothing visible), half default to non-zero:

| Default | Effects |
|---|---|
| `amount = 0` (effect invisible on add) | Kaleidoscope, Edge Detect, Dither, Halation, Glitch, Strobe, Voronoi Prism, Chromatic Aberration, Color Grade, Infrared, HDR Boost, DoF, Blob Track |
| `amount = 1.0` (effect at full strength on add) | Invert, Edge Stretch, Mirror, Quad Mirror, Wireframe Depth |
| `amount = 0.5` (subtle/strong continuum) | Stylized Feedback, Soft Focus, Auto Gain, Watercolor |
| Other | Bloom (`0.187`) |
| No `amount` slider | Transform (4 params: x, y, zoom, rotation — each at identity default) |

The principle worth picking: **"Adding an effect should produce a visible result."** Otherwise the user has to drag a slider just to confirm the effect is wired. Recommend:

- Default `amount = 1.0` for effects with a binary "off / on at full strength" reading (Invert, Edge Stretch, Mirror, Quad Mirror, Wireframe Depth, Kaleidoscope, Strobe, Voronoi Prism, Color Grade, Infrared, Edge Detect, Chromatic Aberration, Glitch, HDR Boost, DoF, Dither, Halation).
- Default `amount = 0.5` for effects with subtle/strong continuum where 1.0 would visually overwhelm (Bloom — currently 0.187, but 0.5 reads better; Stylized Feedback, Soft Focus, Auto Gain, Watercolor, Blob Track).

This is **the largest UX win** in the audit — every effect becomes self-demonstrating on add.

#### 9.1.6 Unit strings — inconsistent

The 6th `ParamSpec::continuous` field is the unit string. Today it's a mess:

- Empty strings: most `amount` params, Invert's everything.
- Pascal-case English: `"Threshold"`, `"Knee"`, `"BlockSize"`, `"FocusPosition"`, `"BlurStrength"`, `"TiltAngle"` — these read as documentation labels, not units.
- Domain units: `"px"` (Soft Focus's radius — the only effect using actual unit notation).
- Repeated names: most are just the param's own label repeated (`"Gain"`, `"Saturation"`, `"Speed"` — informational, not unit).

The field is meant to be a unit (`Hz`, `px`, `°`, `dB`). **Recommendation:** drop unit strings that aren't actual units. Audit each: keep `"px"`, `"°"`, `"Hz"`, `"dB"`; clear everything else. Maybe add `"°"` to Transform.rotation, ColorGrade.hue, Halation.hue, DoF.angle.

#### 9.1.7 Range oddities

A few values worth questioning:

- Edge Stretch `Width` range `[0.1, 0.9]`, default `0.433` — why not `0.5`? Random-looking number, probably ported from a Unity asset's saved value. **Recommend default 0.5.**
- Voronoi Prism `Cell Size` (source_width) range `[0.1, 1.0]`, default `0.5625` — same as above. **Recommend default 0.5.**
- Bloom `amount` default `0.187` — looks like a saved value, not a designed default. **Recommend default 0.5** (per §9.1.5).
- Wireframe Depth `width` default `1.335` — same family. **Recommend default 1.0 or 1.5.**
- Wireframe Depth `subject` default `0.52` — same. **Recommend default 0.5.**
- Wireframe Depth `smooth` default `0.90` upper bound `0.98` — that's a strange ceiling. If the param is bounded `[0, 1]` everywhere else, allow the full range. **Recommend `[0, 1]` with default `0.9`.**
- Watercolor `displace` range `[0.0001, 0.01]` — tiny range, requires F4 format. Consider scaling: range `[0, 1]` internally with a `0.0001 + value * 0.0099` transform inside the primitive. Removes a magic-number "where do I set this slider" question.
- Watercolor `decay` range `[0.9, 1.0]` — same, narrow range. Consider scaling.

#### 9.1.8 Per-effect change table (preview)

This is the **action list** the rename script consumes. Fields: effect, change-kind, old, new.

| Effect | Kind | Old | New |
|---|---|---|---|
| Kaleidoscope | label+id | `segs` / `Segs` | `segments` / `Segments` |
| Edge Stretch | label+id | `dir` / `Dir` | `direction` / `Direction` |
| Edge Stretch | default | `width=0.433` | `width=0.5` |
| Edge Stretch | category | `Post-Process` | `Spatial` |
| Dither | label+id | `algo` / `Algo` | `pattern` / `Pattern` |
| Dither | category | `Post-Process` | `Color` |
| HDR Boost | rename | type_id `HdrBoost` | type_id `HighlightBoost` (with alias) |
| HDR Boost | label+id | `thresh` / `Thresh` | `threshold` / `Threshold` |
| Edge Detect | label+id | `thresh` / `Thresh` | `threshold` / `Threshold` |
| Edge Detect | type_id-rename | `EdgeGlow` | `EdgeDetect` (with alias) |
| Edge Detect | category | `Post-Process` | `Diagnostic` |
| Halation | label+id | `thresh` / `Thresh` | `threshold` / `Threshold` |
| Halation | label+id | `sat` / `Sat` | `saturation` / `Saturation` |
| Color Grade | label+id | `sat` / `Sat` | `saturation` / `Saturation` |
| Color Grade | label+id | `tint_hue` / `TintHue` | (keep id, label → `Tint Hue`) |
| Color Grade | label+id | `tint_sat` / `TintSat` | `tint_saturation` / `Tint Saturation` |
| Color Grade | label+id | `focus` / `Focus` | `tint_focus` / `Tint Focus` |
| Color Grade | category | `Post-Process` | `Color` |
| Transform | label+id | `rot` / `Rot` | `rotation` / `Rotation` |
| Auto Gain | label+id | `char` / `Char` | `character` / `Character` |
| Auto Gain | label+id | `hdr_ret` / `HDR Ret` | `hdr_retention` / `HDR Retention` |
| Blob Track | label+id | `sens` / `Sens` | `sensitivity` / `Sensitivity` |
| Blob Track | label+id | `smooth` / `Smooth` | `smoothing` / `Smoothing` |
| Blob Track | category | `Post-Process` | `Diagnostic` |
| Wireframe Depth | label+id | `z_scale` / `ZScale` | (keep id, label → `Z Scale`) |
| Wireframe Depth | label+id | `wire_res` / `WireRes` | `wire_resolution` / `Wire Resolution` |
| Wireframe Depth | label+id | `mesh_rate` / `MeshRate` | (keep id, label → `Mesh Rate`) |
| Wireframe Depth | label+id | `edge_follow` / `EdgeFollow` | (keep id, label → `Edge Follow`) |
| Wireframe Depth | default | `width=1.335` | `width=1.5` |
| Wireframe Depth | default | `subject=0.52` | `subject=0.5` |
| Wireframe Depth | range | `smooth=[0,0.98]` | `smooth=[0,1]` |
| Wireframe Depth | category | `Post-Process` | `Diagnostic` |
| Glitch | label+id | `block` / `Block` | `block_size` / `Block Size` |
| Voronoi Prism | default | `source_width=0.5625` | `source_width=0.5` |
| Voronoi Prism | category | `Post-Process` | `Stylize` |
| Bloom | default | `amount=0.187` | `amount=0.5` |
| Invert | category | `Spatial` | `Color` |
| Infrared | category | `Surveillance` | `Color` |
| Mirror | category | `Post-Process` | `Spatial` |
| Quad Mirror | category | `Post-Process` | `Spatial` |
| Kaleidoscope | category | `Post-Process` | `Spatial` |
| Strobe | category | `Post-Process` | `Stylize` |
| Stylized Feedback | category | `Post-Process` | `Stylize` |
| Soft Focus | category | `Post-Process` | `Stylize` |
| Watercolor | category | `Post-Process` | `Stylize` |
| (13 effects with `amount=0`) | default | `amount=0` | `amount=1.0` or `0.5` per §9.1.5 |

**Rename volume preview:** ~40 label+id renames, ~12 category moves, ~17 default changes, 2 type_id renames. Estimated ~70 mechanical edits — worth a script.

### 9.2 Layer 2 — Inner primitive params (26 primitives)

The inner-node surface is what the user sees in the graph editor's right-sidebar panel when they click a node. Most primitives mirror the outer-card param shape 1:1 (since the bundled preset wires outer ↔ inner directly), but a handful have their own param names that diverge from the outer card. Plus there are non-card-backed primitives (Mix, Blend, Threshold, Blur, GaussianBlur, Sample, MipChain, WetDry, Brightness, ChannelMix, ColorRamp, MipChain, Feedback, Transform — these are pure building blocks the AI/user composes with).

#### 9.2.1 Inner names that lag behind §9.1's outer renames

After Layer 1 lifts every outer abbreviation to full English, four primitives still have inner-name abbreviations that need to follow:

| Primitive | Current inner param | Proposed |
|---|---|---|
| `node.blob_track` | `thresh`, `sens`, `smooth` | `threshold`, `sensitivity`, `smoothing` |
| `node.auto_gain` | `hdr_ret`, `char` | `hdr_retention`, `character` |
| `node.wireframe_depth` | `smooth` | `smoothing` |
| `node.clamp_stretch` | `mode` (enum: Horiz/Vert/Both) | `direction` (matches outer "Direction" label) |

Once renamed, the binding's `target.param` field collapses to a no-op identity (outer id = inner param name).

#### 9.2.2 Primitive type-id / struct-name / file-name inconsistencies

- `lut1d.rs` defines a struct named `ColorLut` with type id `node.color_lut`. Three different names for the same thing. **Recommendation:** rename the file to `color_lut.rs`, keep struct `ColorLut`, type id `node.color_lut`. (`lut1d` was an early atomic-primitive name; the only consumer is Infrared, which is barely "1D LUT" — `color_lut` reads better.)
- `blob_tracking.rs` has type id `node.blob_track` — singular vs. file's `tracking`. **Recommendation:** rename type id to `node.blob_tracking` (or rename file). The user never sees the type id; the inconsistency is internal-only, but worth fixing once during this pass.

#### 9.2.3 Building-block primitive labels — small fixes

- `node.threshold` has param `level` with label `"Threshold"`. The id and label disagree. **Recommendation:** rename param `level` → `threshold` (then the label is just the title-cased id).
- `node.wet_dry` has param label `"Wet / Dry"` — the space-slash-space is unusual. **Recommendation:** `"Wet/Dry"`.
- `node.chromatic_aberration` (the ChromaticOffset primitive) has label `"Angle (deg)"` — only primitive with the unit baked into the label. Inconsistent with everything else. **Recommendation:** drop `(deg)`, add `°` to the unit field instead (see §9.1.6).
- `node.gaussian_blur` enum param `kernel_size` has label `"Kernel"` — the primitive's label is shorter than the id. Either expand label to `"Kernel Size"` or shorten id to `kernel`. **Recommendation:** label → `"Kernel Size"`.
- `node.affine_transform`'s params `translate_x`, `translate_y` are split because the outer card wires them as two scalars. The standalone primitive `node.transform` uses `translate: Vec2`. Two parallel primitives doing similar work. **Open question:** is this duplication load-bearing, or should we collapse to one `Transform` primitive with Vec2 params? Today the outer card can't drive Vec2 sliders (only scalars + enums), so the split exists for the binding shim. Defer until the binding shim grows Vec2 support.

#### 9.2.4 Defaults in primitives that mirror outer-card magic numbers

These need to flip in lockstep with the outer-card defaults (§9.1.7) so the bundled preset's "no override" path produces the same value the user sees:

| Primitive | Param | Current default | Proposed |
|---|---|---|---|
| `node.clamp_stretch` | `source_width` | `0.433` | `0.5` |
| `node.voronoi_prism` | `source_width` | `0.5625` | `0.5` |
| `node.bloom` | `amount` | `0.187` | `0.5` |
| `node.wireframe_depth` | `width` | `1.335` | `1.5` |
| `node.wireframe_depth` | `subject` | `0.52` | `0.5` |
| `node.wireframe_depth` | `smooth` (range upper bound) | `0.98` | `1.0` |

The bundled preset JSON files also need to re-emit these — the regenerator (`tests/bundled_presets_drift.rs --ignored`) does this in one pass once the primitive defaults change.

#### 9.2.5 Inner labels with weird casing / abbreviations

- `node.auto_gain` param `hdr_ret` has label `"HDR Retention"` — the label is right; the id is what's wrong (lifted in §9.2.1).
- `node.auto_gain` param `char` has label `"Character"` — same.
- `node.wireframe_depth` param `z_scale` has label `"Z Scale"` — looks fine.

After §9.2.1 renames, all primitive labels are full English. Nothing else to clean up.

#### 9.2.6 Param `id` casing convention — confirmed `snake_case`

Every primitive that uses multi-word ids uses `snake_case` (`block_size`, `rgb_shift`, `cell_count`, `kernel_size`, `source_width`). Adopt this as the rule for any new ids introduced by the rename.

#### 9.2.7 Layer 2 change table

| Primitive | Kind | Old | New |
|---|---|---|---|
| `node.blob_track` | id+label | `thresh` / `Threshold` | `threshold` / `Threshold` |
| `node.blob_track` | id+label | `sens` / `Sensitivity` | `sensitivity` / `Sensitivity` |
| `node.blob_track` | id+label | `smooth` / `Smoothing` | `smoothing` / `Smoothing` |
| `node.auto_gain` | id+label | `hdr_ret` / `HDR Retention` | `hdr_retention` / `HDR Retention` |
| `node.auto_gain` | id+label | `char` / `Character` | `character` / `Character` |
| `node.wireframe_depth` | id+label | `smooth` / `Smooth` | `smoothing` / `Smoothing` |
| `node.clamp_stretch` | id+label | `mode` / `Direction` | `direction` / `Direction` |
| `node.threshold` | id+label | `level` / `Threshold` | `threshold` / `Threshold` |
| `node.wet_dry` | label | `Wet / Dry` | `Wet/Dry` |
| `node.chromatic_aberration` | label | `Angle (deg)` | `Angle` + unit `°` |
| `node.gaussian_blur` | label | `Kernel` | `Kernel Size` |
| `node.color_lut` | type-id-rename | `node.color_lut` | (keep — but rename file `lut1d.rs` → `color_lut.rs`) |
| `node.blob_track` | type-id-rename | `node.blob_track` | `node.blob_tracking` (internal only) |
| `node.clamp_stretch` | default | `source_width=0.433` | `source_width=0.5` |
| `node.voronoi_prism` | default | `source_width=0.5625` | `source_width=0.5` |
| `node.bloom` | default | `amount=0.187` | `amount=0.5` |
| `node.wireframe_depth` | default | `width=1.335` | `width=1.5` |
| `node.wireframe_depth` | default | `subject=0.52` | `subject=0.5` |
| `node.wireframe_depth` | range | `smooth=[0,0.98]` | `smoothing=[0,1]` |

**Rename volume:** ~12 inner id+label renames, 2 primitive type-id / file renames, 6 default/range changes. Smaller than Layer 1 because most primitive params already use the long English forms.

### 9.3 Layer 3 — Generator params (23 generators)

22 generators register `GeneratorMetadata` in `manifold-core/src/generator_metadata_submissions.rs`; Strange Attractor registers from its own file. All use the same `ParamSpec` shape as effects, so the audit lens is identical: label clarity, id casing, range correctness, default sensibility.

#### 9.3.1 Truncated labels — same Unity-era abbreviation tax

| Generator | Current label | Proposed |
|---|---|---|
| Tesseract / Duocylinder / Lissajous / Oscilloscope XY / Wireframe Zoo | `Verts` | `Vertices` |
| Tesseract / Duocylinder / Lissajous / Oscilloscope XY / Wireframe Zoo | `VSize` | `Vertex Size` |
| Tesseract / Duocylinder | `Dist` | `Distance` |
| Tesseract / Duocylinder / Lissajous / Oscilloscope XY | `Anim` | `Animate` |
| Mycelium | `SensDist` | `Sensor Distance` |
| Mycelium | `SensAngle` | `Sensor Angle` |
| Galactic Rock / Metallic Glass / Black Hole / Digital Plants | `Cam Dist`, `Cam Orbit`, `Cam Tilt`, `Cam FOV` | `Camera Distance`, `Camera Orbit`, `Camera Tilt`, `Camera FOV` |
| Galactic Rock / Metallic Glass / Digital Plants | `Light Int` | `Light Intensity` |
| Metallic Glass | `Edge Str` | `Edge Strength` |
| Metallic Glass | `Noise Scale` / `Noise Speed` | (keep — already full) |
| Oily Fluid | `VelDamp`, `VelDisp`, `ColDisp` | `Velocity Damp`, `Velocity Displace`, `Color Displace` |
| Oily Fluid | `Sat`, `Bright` | `Saturation`, `Brightness` |
| Black Hole | `Disk Inner`, `Disk Outer`, `Disk Glow` | (keep — `Disk` is the right scoping prefix) |
| Black Hole | `Cam Velocity` | `Camera Velocity` |
| Fluid Sim 3D | `Ctr Scale` | `Container Scale` |
| Fluid Sim 3D | `Vol Res` | `Volume Resolution` |
| Galactic Rock | `Wave Amp` / `Wave Freq` | `Wave Amplitude` / `Wave Frequency` |
| Star Field | `Drift Speed`, `Drift X`, `Drift Y` | (keep — already full Title Case) |
| Strange Attractor | `Type` | `Attractor Type` (the param means a choice between 5 named attractors) |
| Plasma | `Pattern` | (keep — clear) |
| Digital Plants | `Anim Speed`, `Petal Amp`, `Rot Speed`, `Base Radius`, `Torus Radius`, `Box Scale` | (drop the `Amp` / `Rot` abbreviations: `Animation Speed`, `Petal Amplitude`, `Rotation Speed`. Rest keep) |
| Tesseract / Duocylinder / Wireframe Zoo | `XY`, `ZW`, `XW` | (keep — well-known 4D plane labels) |

Multi-generator pattern: **`Cam *` → `Camera *`**. This appears on 4 generators; renaming once gets all of them.

#### 9.3.2 `Count (M)` — the millions-unit hack

Four generators (Fluid Simulation, Fluid Simulation 3D, Strange Attractor, Particle Text) carry a `Count (M)` slider that means "particle count in millions" (so `2.0` = 2 million). The `(M)` is jammed into the label as a fake unit.

**Recommendation:** rename id `count_m` → `particle_count_m` (clearer), label → `Particle Count`, add a real unit string `"M"`. Or — better — translate inside the generator (param value is `0.1..8.0`, multiply by 1e6 internally) and label as `Particle Count` with unit `"M"`. The user sees `Particle Count: 2.0 M`, not `Count (M): 2.0`.

#### 9.3.3 `Snap` default inconsistency

Most generators with a `snap` toggle default it `0.0` (disabled): Basic Shapes Snap, Concentric Tunnel, Fluid Sim, Fluid Sim 3D, Nested Cubes, MRI Volume, Particle Text, Strange Attractor. But **Plasma**, **Lissajous**, **Oscilloscope XY**, **Parametric Surface** default it `1.0` (enabled). Probably an artifact — beat-snap looks impressive in the demo but is intrusive when you're shaping a base look.

**Recommendation:** default `snap = 0.0` everywhere. User enables it intentionally; matches the "subtle defaults" principle.

#### 9.3.4 Display name vs file/id mismatches

- `GeneratorTypeId::COMPUTE_STRANGE_ATTRACTOR` value `"ComputeStrangeAttractor"`. The `Compute` prefix is internal-only ("the compute-shader implementation of") — confusing to see in save files. **Recommendation:** rename the on-disk type id `"ComputeStrangeAttractor"` → `"StrangeAttractor"` with alias.
- `Fluid Simulation` (2D) vs `Fluid Sim 3D` — abbreviated for the 3D but not the 2D. **Recommendation:** rename display name to `Fluid Simulation 3D` for consistency.
- `Basic Shapes Snap` — `Snap` here means "snap to beat" (an option) but the name suggests it's an inherent property of the generator. The generator itself is just "Basic Shapes" with an optional `snap` param. **Recommendation:** rename display name to `Basic Shapes`. The `snap` toggle remains a param.

#### 9.3.5 Range / default oddities

- **Plasma.contrast** default `0.63` — looks like a saved magic number. Range `[0, 1]`. Try `0.5`.
- **Lissajous.freq_x = 0.13`, `freq_y = 0.09`, `phase = 0.07`, `speed = 2.67`, `window = 0.74`, `scale = 1.55`** — every default is a saved magic number. The Liveschool show probably has a Lissajous instance saved with these values; the defaults match. **Recommendation:** round to clean values (`freq_x=0.1`, `freq_y=0.1`, `phase=0.0`, `speed=1.0`, `window=0.5`, `scale=1.0`). Less-impressive on add but more learnable.
- **Mycelium.color** default `0.08` — same magic-number pattern. Try `0.5` or `0.0`.
- **Fluid Simulation.flow** range `[-0.1, -0.001]` — negative-only with no zero. **Recommendation:** either rename `flow` → `decay` (semantics match a decay coefficient), or change range to `[0, 0.1]` and invert sign inside the generator. Today asking the user to dial in a negative value with no zero point reads strange.
- **Fluid Simulation.curl** range `[30, 90]` default `85` — narrow upper range, very specific defaults. Probably angle in degrees. Add unit `"°"`.
- **Oily Fluid.feedback** range `[0.95, 0.9999]` — same narrow-range pattern as Watercolor. Consider rescaling to `[0, 1]` internally.
- **Black Hole.freefall** format `"F0"` but range `[0, 1]` — F0 means 0 decimal places, which makes 0.5 render as "0" or "1". Should be `"F2"`.
- **Black Hole.steps** range `[50, 500]` default `150` — clear.
- **Strange Attractor.chaos** default `0.0` — does nothing on add. Try `0.3`.
- **Tesseract / Duocylinder** default `xy / zw / xw` look like saved magic numbers. Try `0.5 / 0.3 / 0.2`.
- **Oily Fluid.hue** default `0.0` is fine if hue is normalized [0, 1]; the F2 format suggests yes.

#### 9.3.6 Generator categories

Generators have no `category` field in their metadata today. They're listed flat in the picker. Worth grouping (5–6 buckets — similar to the effect category split):

| Category | Generators |
|---|---|
| **Procedural** | Plasma, Basic Shapes, Concentric Tunnel, Star Field |
| **3D** | Tesseract, Duocylinder, Wireframe Zoo, Nested Cubes, Parametric Surface, Galactic Rock, Metallic Glass, Black Hole, Digital Plants |
| **Lines** | Lissajous, Oscilloscope XY |
| **Particles** | Mycelium, Fluid Simulation, Fluid Simulation 3D, Strange Attractor, Oily Fluid, Particle Text |
| **Volumetric** | MRI Volume |
| **Text** | Text, Particle Text (already in Particles too — assign primary) |

Add a `category: &'static str` field to `GeneratorMetadata` mirroring `EffectMetadata.category`, then populate. The picker UI groups by category. (Today the picker is flat — see open question §9.4 about grouped pickers.)

#### 9.3.7 Layer 3 change table (summary)

| Generator | Kind | Change |
|---|---|---|
| 5 generators | label | `Verts` → `Vertices`, `VSize` → `Vertex Size` |
| 4 generators | label | `Cam Dist`/`Cam Orbit`/`Cam Tilt`/`Cam FOV` → `Camera *` |
| 2 generators | label | `Dist` → `Distance`, `Anim` → `Animate` |
| Mycelium | label | `SensDist`/`SensAngle` → `Sensor Distance`/`Sensor Angle` |
| Oily Fluid | label | `VelDamp`/`VelDisp`/`ColDisp`/`Sat`/`Bright` → full names |
| 3 generators | label | `Light Int` → `Light Intensity`, `Edge Str` → `Edge Strength` |
| Black Hole | label | `Cam Velocity` → `Camera Velocity` |
| Fluid Sim 3D | label | `Ctr Scale` → `Container Scale`, `Vol Res` → `Volume Resolution` |
| Galactic Rock | label | `Wave Amp`/`Wave Freq` → `Wave Amplitude`/`Wave Frequency` |
| 4 generators | id+label | `count_m` / `Count (M)` → `particle_count_m` / `Particle Count` (with `M` unit) |
| Strange Attractor | type-id-rename | `ComputeStrangeAttractor` → `StrangeAttractor` (with alias) |
| Strange Attractor | label | `Type` → `Attractor Type` |
| Fluid Simulation 3D | display | `Fluid Sim 3D` → `Fluid Simulation 3D` |
| Basic Shapes Snap | display | `Basic Shapes Snap` → `Basic Shapes` |
| 4 generators | default | `snap = 1.0` → `snap = 0.0` (Plasma, Lissajous, Oscilloscope XY, Parametric Surface) |
| Plasma | default | `contrast = 0.63` → `0.5` |
| Lissajous | default | `freq_x=0.13`, `freq_y=0.09`, `phase=0.07`, `speed=2.67`, `window=0.74`, `scale=1.55` → round numbers |
| Mycelium | default | `color = 0.08` → `0.5` |
| Fluid Simulation / 3D | param | `flow` range `[-0.1, -0.001]` → either rename to `decay` or invert sign |
| Fluid Simulation / 3D | unit | `curl` add `"°"` unit |
| Black Hole | format | `freefall` format `F0` → `F2` |
| Strange Attractor | default | `chaos = 0.0` → `chaos = 0.3` |
| All generators | new field | add `category: &'static str` to `GeneratorMetadata` |

**Rename volume:** ~30 label renames, ~10 default changes, 3 display-name fixes, 1 type-id rename, 1 metadata-shape change. Comparable to Layer 1 in scale.

### 9.4 Open questions

- **Param `id` casing convention.** Most current ids are `snake_case` (`tint_hue`, `block_size`). Some are single words (`amount`, `gain`). Confirm `snake_case` everywhere as the rule.
- **Display label casing.** Current mix: `Title Case` (`Edge Detect`, `Block Size`), `PascalCase` (`TintHue`, `ZScale`), abbreviations (`HDR Ret`). Confirm `Title Case With Spaces` as the rule.
- **Type id rename migration shape.** `EffectValueAliasMetadata` exists for enum-value remaps. For type id renames we need a sibling: `EffectTypeAliasMetadata` mapping old type id strings to new. Stamp this once; reuse for the HDR Boost / Edge Detect / Strange Attractor renames.
- **Grouped picker UI.** Today both the "Add Effect" and "Add Generator" pickers are flat alphabetical. Once categories land (§9.1.4, §9.3.6), the picker should group by category with collapsible sections. UX call: scope this into the rename pass, or defer?
- **Camera primitive.** `Cam Dist`/`Cam Orbit`/`Cam Tilt`/`Cam FOV`/`Look Y` repeats across 4 generators. Tempting to factor into a shared `Camera3D` primitive (which §6.7 G1 already plans). Don't do it now — wait for the generator decomposition pass. Just align the names today so the future primitive lands without further migration.

### 9.5 Rotation / angular slider loop convention

**Rule:** any parameter whose semantic is "an absolute rotation angle" must have its range chosen so the slider's min position is visually identical to its max position. Driving the slider from one extreme to the other (e.g. with an LFO) then produces a clean continuous rotation rather than a "rotate then snap back" discontinuity.

Two acceptable ranges satisfy this:

- **`[-180, 180]`** with units in degrees, default `0`. `-180°` and `+180°` are the same orientation (pointing opposite the +X axis).
- **`[0, 360]`** with units in degrees. `0°` and `360°` are the same orientation.
- **`[0, 1]`** with the param scaled to one full turn internally. `0` and `1` are both "no rotation" / "full rotation back to start".

**Audit result — all current rotation params already satisfy the rule:**

| Param | Range | OK |
|---|---|---|
| `Transform.rot` | `[-180, 180]` | ✅ |
| `ColorGrade.hue` | `[-180, 180]` | ✅ |
| `Halation.hue` | `[0, 360]` | ✅ |
| `ChromaticAberration.angle` | `[0, 360]` | ✅ |
| `DepthOfField.angle` | `[0, 360]` | ✅ |
| `Black Hole.rotate` | `[-180, 180]` | ✅ |
| `Galactic Rock.cam_orbit` | `[-180, 180]` | ✅ |
| `Metallic Glass.cam_orbit` | `[-180, 180]` | ✅ |
| `Digital Plants.cam_orbit` | `[-180, 180]` | ✅ |
| `Oily Fluid.hue` | `[0, 1]` | ✅ (normalized; ×360° internally) |
| `ColorGrade.colorize_hue` | `[0, 360]` | ✅ |

**Excluded — not absolute angles, do not apply the rule:**

- `Stylized Feedback.rotate` (`[-10, 10]`) — rotation **rate** in units per frame, not an angle. `-10` and `+10` are opposite spin directions, visually different.
- Any `*_tilt` (e.g., `cam_tilt [-90, 90]`) — partial elevation angle, not a full rotation.
- Metallic Glass `mirror [0, 90]` — half-rotation symmetry parameter, not a full rotation.

**Where this lives in code:** the rule applies at the `ParamSpec` declaration site. No runtime enforcement — convention only, validated by this audit. New rotation params added in future should follow the same rule.

### 9.6 Deferred — type-id renames + future migration tool

**Decision (2026-05-17):** the three internal type-id renames called for in §9.1.3 / §9.2.2 / §9.3.4 — `HdrBoost → HighlightBoost`, `EdgeGlow → EdgeDetect`, `ComputeStrangeAttractor → StrangeAttractor` — are **deferred indefinitely**. Not killed; revisit only when there's a real reason to (a confusing debugging session, a related refactor, etc.).

**Why deferred.** The type-id string is internal-only. The user-facing display names ("Highlight Boost", "Edge Detect", "Strange Attractor") are already correct after this audit. The legacy strings appear in three places only:
- `.manifold` save files, where users never look.
- The renderer's `EffectTypeId::HDR_BOOST` etc. constants, whose value happens to be `"HdrBoost"` — a code-readability mismatch, not a behavior bug.
- Bundled-preset filenames under `assets/effect-presets/` (e.g., `HdrBoost.json`).

The cost of the rename is asymmetric to its value: three string changes, plus a new project-file migration mechanism, plus a bundled-preset filename shuffle, plus migration tests against `Liveschool Live Show V6 LEDS.manifold` to make sure the show still loads. The risk of getting the migration wrong is silent effect-instance loss on load — same failure mode as a timing bug in the engine. Not worth it for a code-cleanup-grade win.

**The right tool to build, when this comes back.** The Phase 7 `EffectAliasMetadata` infrastructure works for *param-id renames within one effect*. Type-id renames need a different shape because the type-id is itself the dispatch key — the loader needs to translate `"HdrBoost"` → `"HighlightBoost"` *before* deciding which effect's deserializer to call. Sketch:

```rust
// crates/manifold-core/src/effect_type_id_aliases.rs (new)
pub struct EffectTypeAliasMetadata {
    pub from: &'static str,   // legacy on-disk string
    pub to: EffectTypeId,     // current const-mapped EffectTypeId
}
inventory::collect!(EffectTypeAliasMetadata);

// Hooked into manifold-io/src/migrate.rs as a JSON-rewrite pass
// that runs before EffectInstance deserialization. Walks every
// "effectType": "..." occurrence in the JSON document; if the
// string matches a registered legacy alias, rewrite to the new
// type-id string.
```

Plus the bundled-preset registry in `bundled_presets.rs` needs a parallel alias table so `bundled_preset_def(&EffectTypeId::new("HdrBoost"))` still works for old saves that haven't been resaved yet.

**When to revisit.** Most likely triggers:
- A bug surfaces where the internal/external name mismatch causes confusion (a colleague asks "why does the code say HdrBoost when the card says Highlight Boost?").
- We need to add `EffectTypeAliasMetadata` for a different reason (e.g., genuinely renaming an effect because its function changed), and the infrastructure becomes free to apply to these three too.
- An LLM-driven refactor pass on the codebase keeps tripping on the inconsistency.

Whoever picks this up: the rename script (`scripts/audit_rename.py`) is *not* the right tool — it does source-code find/replace, not project-file migration. Build the alias metadata + JSON-rewrite pass first, then the source-code changes ride alongside as a single coherent commit gated by a Liveschool round-trip test.

---

**End of §9.** Audit applied across 7 phases (1, 2, 3, 4, 6, 7a, 7b, 7c); ~237 source edits + tooling. Phase 5 deferred per above.

### 9.4 Open questions

- **Param `id` casing convention.** Most current ids are `snake_case` (`tint_hue`, `block_size`). Some are single words (`amount`, `gain`). Confirm `snake_case` everywhere as the rule.
- **Display label casing.** Current mix: `Title Case` (`Edge Detect`, `Block Size`), `PascalCase` (`TintHue`, `ZScale`), abbreviations (`HDR Ret`). Confirm `Title Case With Spaces` as the rule.
- **Type id rename migration shape.** `EffectValueAliasMetadata` exists for enum-value remaps. For type id renames we need a sibling: `EffectTypeAliasMetadata` mapping old type id strings to new. Stamp this once; reuse for the HDR Boost and Edge Detect renames.

---

## 10. Toward a real composition surface (2026-05-17)

The §9 audit finished the cosmetic and structural cleanup. This section is about what the catalog needs *next* — the substantive expressiveness work that turns the graph editor from "fancy serial chain of fused effects" into a real composition surface where new aesthetic operators can emerge.

### 10.1 The diagnosis

The current "primitive library" is two strata that have been collapsed:

- **Aesthetic operators** (~18 nodes): `bloom`, `watercolor`, `glitch`, `halation`, `voronoi_prism`, `infrared`, `color_grade`, `kaleido_fold`, `quad_mirror`, `chromatic_offset`, `auto_gain`, `blob_track`, `wireframe_depth`, `highlight_boost`, `depth_of_field`, `clamp_stretch`, `strobe`, `dither_pattern`. Each is a full shader effect with one or two opinionated knobs. They carry an *aesthetic stance* — `watercolor` isn't "small kernel blur + noise + edge enhance," it's *watercolor*. Internal composition doesn't matter to the user; the look does.
- **Atomic operators** (~13 nodes): `mix`, `blend`, `wet_dry`, `blur`, `gaussian_blur`, `mip_chain`, `threshold`, `edge_detect`, `transform`, `sample`, `affine_transform`, `feedback`, `brightness`, `channel_mix`, `color_ramp`, `color_lut`, `invert`. The building blocks. Mostly scalar-uniform, single-texture-in single-texture-out.

The aesthetic layer is the differentiator from TouchDesigner — TD has overwhelmingly atomic catalogs, so user compositions converge on a recognizable "TD look" (feedback + bloom + chromatic + raymarched SDFs + particles). MANIFOLD's aesthetic operators carry taste that prevents that convergence *as long as the catalog keeps growing*. If it stagnates, users either accept Peter's taste as a ceiling or descend to the atomic layer — at which point the TD look returns.

The atomic layer, meanwhile, is too thin to compose interesting new effects without writing shaders. There's no per-pixel masking (`mix` / `blend` only take scalar amounts), no displacement, no per-pixel math, no procedural mask generation. The graph editor is currently a fancy way to wire pre-built effects together, not a real composition surface.

### 10.2 The vision

Two stratification rules:

1. **Aesthetic operators are the default surface.** Picker shows them front-and-center. Most users live here — drag onto a card, twiddle one or two knobs, done. Catalog grows by deliberate authoring (Peter, AI agents, eventually community), each new operator carrying a distinct stylistic stance (riso, ink-bleed, thermal, oil-on-water, halftone, lo-fi broadcast, photocopier, etc.).
2. **Atomic operators are the extensibility layer.** Behind an "Advanced" disclosure. For authoring new aesthetic operators, for power users who want TouchDesigner-class composition, and most importantly *for Peter and Claude to compose new shipping aesthetic operators faster than writing one-off shaders.*

The graph editor is the **TouchDesigner / Resolume Wire layer** of MANIFOLD. The effect cards + timeline are the live-performance layer. Both surfaces draw on the same primitive library; they just expose it differently. Complexity in the graph editor is the point, not a cost — that's the surface where users build new aesthetic operators that get saved as presets and appear as cards on the timeline.

The wedge against TouchDesigner: **the in-MANIFOLD graph editor's only defensible reason to exist is its live-performance integration.** Beat/bar/phase as native node types. Presets that are clips on a timeline. MIDI mappings that work the same in the graph and on the cards. Edits that go through the same undo stack as everything else. The ability to fix a buggy graph mid-show without leaving the app. If the graph editor's narrative is "compose effects that are beat-aware and arrangement-integrated" — *not* "general visual programming environment" — it's a genuinely different product than TD, even where the surface looks similar.

### 10.3 The plan — five phases

#### Phase A — Texture primitives (~1-2 weeks, low risk)

Six new atomic primitives. Pure additive to the existing catalog, no architectural change. Each = WGSL shader + Rust wrapper + `primitive!` macro + smoke test + one hand-authored preset that uses it.

| Primitive | Signature | What it unlocks |
|---|---|---|
| `masked_mix` | `(a: T2D, b: T2D, mask: T2D, amount: f32) → T2D` | Per-pixel mask compositing. Single biggest unlock. Enables luma-keyed grades, edge-gated effects, threshold-bloom-in-shadows. |
| `displacement_map` | `(source: T2D, displace: T2D, strength: f32) → T2D` | Sample source at `uv + displace.rg * strength`. Heat haze, refraction, organic warping, displacement-mapped feedback. |
| `math_op` | `(a: T2D, b: T2D, op: enum) → T2D` | Per-pixel add/sub/mul/div/min/max. Difference imaging (motion detection), additive light passes, multiplicative tinting. |
| `luma_key` | `(source: T2D, range: vec2, softness: f32) → mask` | Texture → mask. The "where do I want to apply this" generator. |
| `noise` | `(scale: vec2, octaves: int, time: f32, type: enum) → T2D` | Perlin / Simplex / Worley / value noise. Generative masks, organic distortion maps (feed into displacement_map), procedural dither. |
| `sdf_shape` | `(shape: enum, size: vec2, smoothness: f32) → T2D` | Procedural geometric masks. Circles, boxes, lines, radial gradients. Vignettes, spotlights, beat-synced reveals. |

*Validation*: each gets a smoke test (runs without panic) and a determinism test (same input + params = same output bytes). No legacy parity test possible — these are new shaders.

*Phase A is the validation that the primitive surface is the right shape before any architectural work begins.* If one of the six is awkward in practice or needs an extra port, that's cheaper to discover in week 1 than in week 6.

#### Phase B — Control wire architecture (~3-4 weeks, high risk, design-heavy)

The architectural lift. Where almost all the design risk lives.

- **B1: Port type extension.** Add `PortType::Float / Vec2 / Vec4` alongside `Texture2D`. Validation rules. Float→VecN broadcast (replicate Float across components, by default).
- **B2: Two-pass per-frame runtime.** Control signals evaluate in topological order *before* texture nodes. Results stored in a frame-local `ControlValues` map keyed by `(node_id, port_name)`, pre-allocated at chain-build time, reused per frame (hot-path discipline — no per-frame allocation).
- **B3: `ParamSource::Wire(node, port)` alongside `ParamSource::Constant`.** Per-parameter wire support. Wired parameters pull from the frame's `ControlValues` map; unwired parameters use their `Constant` value as today.
- **B4: StateStore extension for stateful control nodes.** LFO phase accumulator, smoothing's previous value, sequencer position — same pattern as `Feedback`'s state today.
- **B5: EffectGraphDef v1 → v2 schema bump.** Wire records added. Existing v1 presets keep loading; new fields default-empty; upgrade on first save.

*What it unlocks for the instrument*: effects whose parameters react to their own content, to audio, to MIDI, to other layers in the project. This is the actual transition from "image pipeline" to "responsive instrument." Combined with Phase A's six texture primitives, this is the full TouchDesigner-class composition surface.

#### Phase C — Control-rate node catalog (~6-8 weeks, batched)

Each batch ships independently. Each subsequent batch builds on what came before.

- **C1 Foundation** (5 nodes): `constant`, `lfo`, `math`, `smoothing`, `range_map`. Smallest set that validates the architecture end-to-end. Ship and live with this before C2-C5.
- **C2 Sources** (6 nodes): `time`, `beat`, `phase`, `midi_cc`, `midi_note`, `osc`, `random`. The "outside world into the graph" set.
- **C3 Generators** (5 nodes): `envelope`, `step_sequencer`, `ramp`, `math_expression`, `sample_hold`.
- **C4 Operators** (~8 nodes): `trig`, `curve`, `clamp`, `compare`, `select`, `logic`, `mux`/`demux`, `quantize`.
- **C5 Bridges** (5 nodes): `brightness`, `peak`, `centroid`, `motion_energy`, `color_sample`. Texture→Control measurements. New infrastructure — these read a texture and produce a scalar/vec2/vec4. Internal frame buffer for `motion_energy` (same pattern as `feedback`).
- **C6 Audio** (3 nodes): `audio_band`, `audio_amplitude`, `audio_onset`. *Requires `manifold-audio` to become real (currently stub).* Separate sub-project — don't gate the rest of C on it.

*Wire type discipline* (kept minimal):
- `Texture2D` (existing)
- `Float`
- `Vec2`
- `Vec4` — skip `Vec3` (color is usually rgba; pack into Vec4 with unused alpha if needed)

*Gates* are convention-on-Float (`0 = off, >0 = on`), not a distinct type. Permits free type compatibility — you can multiply a gate by anything, smooth it, average it. The cost is zero compile-time enforcement that you wired a gate where one was expected; mitigated by lint, not by type system. TouchDesigner does this — all CHOP signals are floats; "gate" is purely semantic.

#### Phase D — Editor surface updates (~2-3 weeks, interleaved with C)

- Type-aware pin rendering. Texture pins thick/curved; control pins thin/colored by type.
- Wired-parameter indicator on sliders. **Both** in the graph editor and on the effect cards on the timeline — so on stage you can see which knobs are being driven from inside the graph and won't respond to MIDI / cards.
- Right-click to disconnect a wire from a parameter.
- Picker organization: aesthetic primitives front-and-center, atomic primitives behind "Advanced" disclosure (or via search). Category headers (Stylize, Color, Spatial, Filmic, Diagnostic + new Generative for noise/sdf).
- Type-mismatch warnings before connection commit.

Interleaves with Phase C rather than landing all at once — each new node category surfaces its own UI gaps to fill.

#### Phase E — Aesthetic operator authoring practice (ongoing, no end date)

- Commit to ~3 new aesthetic operators per release cycle.
- Each carries a deliberate stylistic stance — riso print, ink-bleed, thermal scan, oil-on-water, halftone, lo-fi broadcast, photocopier, datamosh, oxidation, etc. Different lineages (printmaking, broadcast video, photographic, painterly) rather than all-shader-aesthetic.
- Tag by stylistic family in the catalog so users browse by lineage rather than alphabetically.
- AI-authoring infrastructure (primitive-metadata export tool, LLM composition workflow, generated-preset preview loop) joins this phase when Phase B/C/D are stable. The §7 primitive metadata schema is the LLM-readable foundation; the open question is the generation tool itself.

The catalog *must* keep growing or the design philosophy collapses — users exhaust the aesthetic catalog, descend to atomic, the TD look returns. This isn't a phase that completes; it's a practice that has to become routine.

### 10.4 Cross-cutting

- **EffectGraphDef v1 → v2 migration** when Phase B lands. Existing presets keep loading; new fields default-empty; upgrade on first save.
- **Documentation**: every primitive needs a one-paragraph picker card (purpose, ports, params, example presets). Every aesthetic operator gets an "example presets that use this" cross-reference.
- **Hot-path discipline**: the per-frame two-pass evaluator (Phase B.2) can't allocate. Pre-allocate the `ControlValues` map at chain-build time, reuse each frame. Same rule as today.
- **Testing**: parity tests for Phase A texture primitives stay smoke + determinism (no legacy baseline). Control nodes (Phase C) get unit tests for math correctness — control evaluation is CPU-side, doesn't need GPU harness.

### 10.5 Open design questions (resolve before Phase B starts)

1. **Gates as a distinct type or convention-on-Float?** Lean convention. Lint, don't enforce.
2. **Float→VecN auto-coercion by replication?** Lean yes; permissive wiring.
3. **Bridge synchrony.** Does this frame's brightness measurement drive this frame's render (synchronous, two-pass evaluator) or last frame's (one-frame-delayed, simpler runtime)? Lean synchronous despite the cost — one frame of lag is perceptible on stage and breaks the "responsive" promise.
4. **Audio scope.** C6 in the first wave or punted? Lean punt; the rest of C is large enough already and audio DSP is its own beast.
5. **Picker UI for aesthetic/atomic split.** Tabs, disclosure toggle, or search-only? Lean search-first with category headers in results, plus an explicit "show atomic primitives" toggle.
6. **Project compatibility.** Clean v2 break or maintain v1 forever? Lean v1 keeps loading read-only, upgrades on first save.

### 10.6 First concrete step

**Phase A.1: build `masked_mix`.** Single shader, two texture inputs + one mask + one scalar amount. Smoke test + determinism test + one hand-authored preset that demos it ("Bloom Only In Shadows" — `Bloom` output as one input, `Source` as the other, `luma_key(threshold=0.5, inverted)` as the mask). About a day of work. If it composes cleanly with existing primitives in the editor, the rest of Phase A is the same template six times and the plan above is validated against reality.

### 10.7 Where this points

The endpoint isn't a TouchDesigner clone. It's a system where:

- Users drag opinionated aesthetic operators onto clips on a timeline and play them like an instrument.
- The aesthetic catalog grows continuously through deliberate authoring (Peter + AI).
- Power users who want to build their own effects open the graph editor and compose at either layer — usually aesthetic operators with control wires between them; occasionally atomic primitives for the genuinely novel.
- New presets save as cards. The user's personal library grows. The aesthetic catalog is the *toolbox*; the preset library is the *instrument*.
- Beat / bar / phase / MIDI / OSC / audio flow as control signals through the graph the same way they flow through the timeline's perform-mode bindings. Same musical model, two surfaces.

That's the wedge nobody else is building. The graph editor convergence with TouchDesigner is fine — the *integration* with arrangement and live performance is the part that's genuinely MANIFOLD's.

---

## 11. Unified authoring registry — pre-implementation research (2026-05-18)

> **Status: complete, 2026-05-18.** Landed across 14 commits over two sessions. Chain runtime, editor snapshot, and primitive registry are all single-path; ~4500 lines of legacy deleted; every shipping effect's metadata + canonical graph lives in `assets/effect-presets/*.json` with `presetMetadata` populated. Adding a new effect is now a JSON drop. Manual UI walkthrough (picker, MIDI mapping on Liveschool fixture) is the one remaining check.

Before starting the JSON-authoritative migration sketched at the end of §10, this section captures an audit of the existing registries and consumers, with refinements to the original plan. The architectural target stays the same — *one source of truth per category, no hand-maintained lists* — but the migration is more nuanced than first stated.

### 11.1 What "registry" currently means — three overlapping systems

There are **three** effect registries in `manifold-core`, all populated from the same `inventory::submit!(EffectMetadata)` source, each consumed by a different layer:

- **`EffectMetadata`** (`effect_registration.rs`) — the raw `inventory::collect!` shape. Fields: `id`, `display_name`, `category`, `available`, `osc_prefix`, `legacy_discriminant`, `params: &[ParamSpec]`. Plus three sidecar submissions: `EffectAliasMetadata` (param renames), `EffectNodeAliasMetadata` (node-handle renames), `EffectValueAliasMetadata` (enum-value remaps). Cached via `metadata.rs::metadata_by_id()` in the renderer.

- **`EffectDef`** (`effect_definition_registry.rs`) — computed view built from each `EffectMetadata` via `to_effect_def()`. Adds `id_to_index: AHashMap<String, usize>` (the addressing table for OSC / Ableton / driver / project storage), `param_ids: Vec<&'static str>`, and merged `legacy_*_aliases` slices. This is the **load-bearing registry** for the rest of the codebase — almost every consumer in `effects.rs` and `project.rs` goes through `effect_definition_registry::try_get(&id)`.

- **`EffectTypeRegistration`** (`effect_type_registry.rs`) — the picker/UI surface. `display_name`, `category`, `available`. Consumed by `manifold-app/ui_bridge` (state sync, inspector, picker), `ui_root` (effect browser popup).

Plus a **legacy fourth** registry (`effect_category_registry.rs`) — hand-maintained `HashMap<EffectTypeId, &str>`. Largely superseded by `effect_type_registry` but still compiled in.

**Renderer-side** there are two more pieces:

- **`ChainSpec`** (`node_graph/chain_spec.rs`) — the splice fn that builds the canonical graph, plus `bindings`, plus `skip` mode. Consumed in `effect_chain_graph.rs` (5 callsites — the load-bearing chain runtime), `effect_registry.rs` (legacy snapshot path), `bundled_presets.rs` (drift check).

- **`EffectFactory`** (`effects/registration.rs`) — `(id, create: fn(&GpuDevice) -> Box<dyn PostProcessEffect>)`. Consumed by `effect_registry.rs` for two surviving roles: **editor snapshot lookup** (`graph_snapshot_for`, used by `layer_compositor::graph_snapshot_for`) and **plugin warmup** (`flush_all_background_work` — pre-export drain of background workers in DepthEstimator / BlobDetector).

### 11.2 The per-effect Rust audit — what migrates cleanly, what doesn't

Audit of all 25 effect files in `crates/manifold-renderer/src/effects/`:

**20 of 25 migrate cleanly to JSON-authoritative:**

- **16 STANDARD** (template-shaped, `atomic_chain_spec!` macro, one primitive): `chromatic_aberration`, `color_grade`, `dither`, `edge_detect`, `edge_stretch`, `glitch`, `hdr_boost`, `invert_colors`, `kaleidoscope`, `quad_mirror`, `strobe`, `transform`, `voronoi_prism`, `bloom`, `halation`, `infrared`. Each compiles down to: metadata → JSON fields, single-primitive splice → graph node, bindings → JSON binding list. Their corresponding **primitives stay in Rust** (where stateful machinery lives — Bloom's mip pyramid, Halation's blur chain, Infrared's LUT cache). The effect file gets deleted; the JSON carries metadata + the one-node canonical graph.

- **4 COMPOSITE** (hand-written splice fns wiring 2-3 primitives): `mandala`, `edge_stretch_by_color`, `mirror`, `soft_focus_graph`. Migrate just as cleanly — their canonical JSON snapshot already contains the full multi-node topology; the JSON IS the splice's output. `mirror` carries `EffectValueAliasMetadata` for legacy mode remaps; those move into a JSON `valueAliases` field.

**5 effects need attention:**

- **`auto_gain`** — Per-owner CPU envelope state (`AutoGainOwnerState`: measure buffer + EMA state + frame count). *But* the matching `AutoGain` primitive in `node_graph/primitives/` already owns the per-owner state via `StateStore`. The legacy effect file's state is **dead code** in the post-cutover render path (ChainGraph → primitives doesn't call `PostProcessEffect::apply()`). Effect file can be deleted; primitive carries state forward.

- **`blob_tracking`** — Spawns native `BlobDetector` plugin as background worker, owns font atlas texture, 512-quad overlay instance buffer, One-Euro smoothing state, blob matching. *Worker creation happens in the legacy effect's `new(device)`.* For migration: either move worker init into the primitive's lazy first-run, or keep a small `PluginPrewarm` inventory specifically for the plugin-using effects (see §11.5).

- **`depth_of_field`** — Spawns MiDaS depth-estimation worker, manages readback→inference→upload pipeline, 3 focus modes. Same shape as blob_tracking — worker init is in the effect file; needs preserving via prewarm path.

- **`watercolor`** — 7-pass feedback pipeline with intermediate textures. *State lives in the primitive*, not in the legacy effect file. Effect file deletion is fine; primitive carries the multi-pass machinery.

- **`wireframe_depth`** — Massive multi-worker pipeline (3+ DNN workers, optical flow buffers, cut-score temporal coherence). State in the primitive. Workers initialized in the effect file's `new(device)` — same prewarm question as blob_tracking and depth_of_field.

- **`stylized_feedback`** — Boundary case flagged by the audit. Uses `atomic_chain_spec!` with the `Feedback` primitive directly; state lives in the primitive. Migrates as STANDARD, no special handling.

- **`node_graph_test`** — Diagnostic test composition, no real GPU work. Can probably delete entirely or convert to a test-only JSON if it's still useful.

**Bottom line:** 24 of 25 effect files delete cleanly. The 25th — node_graph_test — is a test artifact. **Three** effects (blob_tracking, depth_of_field, wireframe_depth) need their worker initialization preserved through a separate mechanism, because workers need to start at process boot, not at first chain dispatch.

### 11.3 The composite effects make the splice abstraction unnecessary

In the JSON-authoritative model, the distinction between "atomic" (one primitive) and "composite" (multiple primitives) collapses. Both are just: load JSON → instantiate listed nodes via `PrimitiveRegistry::create()` → wire listed edges. The `atomic_chain_spec!` macro and the hand-written `splice` fn become the same code path — `EffectGraphDef::instantiate(&primitive_registry)`, which already exists.

That means `ChainSpec` as a type can disappear entirely. Its three fields:
- `splice: fn` → JSON's `nodes` + `wires`
- `bindings: &[ParamBinding]` → JSON's `bindings` field
- `skip: SkipMode` → JSON's `skipMode` field

…all live in the JSON. The `chain_spec_by_id()` function gets replaced by `loaded_preset_by_id()`. Same 5 callsites in `effect_chain_graph.rs` rewire mechanically.

### 11.4 `EffectGraphDef` already exists and is close to ready

The current schema (v1) carries `version`, `name`, `description`, `nodes`, `wires`. To absorb everything from `EffectMetadata` + `ChainSpec` + the three alias sidecars, it needs new top-level fields:

```rust
pub struct EffectGraphDef {
    pub version: u32,                    // bump to 2
    pub id: EffectTypeId,                // was implicit (filename); make explicit
    pub display_name: String,
    pub category: String,
    pub osc_prefix: String,
    pub legacy_discriminant: Option<i32>,
    pub available: bool,
    pub params: Vec<ParamSpec>,          // outer-card slider list
    pub bindings: Vec<ParamBinding>,     // outer→inner routing
    pub skip_mode: SkipMode,
    pub param_aliases: Vec<ParamAlias>,
    pub node_aliases: Vec<ParamAlias>,
    pub value_aliases: Vec<ValueAliasEntry>,
    pub name: Option<String>,            // existing
    pub description: Option<String>,     // existing
    pub nodes: Vec<EffectGraphNode>,     // existing
    pub wires: Vec<EffectGraphWire>,     // existing
}
```

All the new types (`ParamSpec`, `ParamBinding`, `SkipMode`, `ParamAlias`, `ValueAliasEntry`) already exist with serde derives or are trivially serializable. V1 documents (the existing 25 JSON snapshots) lack the new fields; serde defaults handle them at load time. Existing per-instance graph overrides on `EffectInstance` (which are also `EffectGraphDef`) just don't populate the new fields — they're not preset definitions, they're override deltas.

### 11.5 Plugin warmup needs a separate inventory channel

Three effects (`blob_tracking`, `depth_of_field`, `wireframe_depth`) create background workers in their `new(device)`. The workers must:
- Start at process boot (so first render isn't blocked on plugin initialization)
- Be drained before each export frame (so export is deterministic)

These needs survive the deletion of the effect Rust files only if we either:

**(a) Move worker init into the primitive's lazy `run()`** — first dispatch triggers worker creation. Pros: kills the EffectFactory cleanly. Cons: first-frame stutter when a clip with one of these effects starts; uneven warmup across primitives.

**(b) Keep a minimal `PluginPrewarm` inventory** — a new `inventory::collect!`-able struct `{ id: EffectTypeId, prewarm: fn(&GpuDevice) }` that the renderer runs at startup. Only the three plugin-using effects submit one. The rest of the EffectFactory pattern dies.

I lean (b). Cleaner separation: plugin warmup is its own concern, doesn't pollute the primitive's `run()` with init-time-only code. The inventory list is tiny (3 entries) and explicit. A new `crates/manifold-renderer/src/plugin_prewarm.rs` or similar lives alongside the primitive registry; `LayerCompositor::new()` iterates the prewarm submissions during construction.

### 11.6 Editor snapshot — does it still need `EffectFactory`?

The other surviving role of `EffectFactory` is `graph_snapshot_for(type_id) -> Snapshot` (`layer_compositor::graph_snapshot_for`, called from `compositor.rs:114`). Today: every legacy effect can render a `Source → Effect → FinalOutput` preview for the editor canvas. Implemented by holding singleton `Box<dyn PostProcessEffect>` instances in `EffectRegistry` and calling their `apply()`.

**In the JSON-authoritative world this is replaced by `ChainGraph::build_and_render(loaded_preset.graph_def)`.** The canonical graph is what the chain runs anyway; rendering a snapshot is one frame of that graph against the editor's preview input. `EffectFactory` doesn't need to exist for this purpose. The snapshot path migrates to use the same code path that the live chain uses.

That fully eliminates `EffectFactory`. The `PluginPrewarm` channel from §11.5 covers the only remaining startup-time concern. `EffectRegistry` itself can be deleted.

### 11.7 `EffectDef` is the right shape for the unified loaded preset

The original §10.5 proposal sketched a `LoadedPreset` struct with all the metadata. Looking at the actual codebase, `EffectDef` in `effect_definition_registry.rs` is already 90% that struct — `display_name`, `param_count`, `param_defs`, `osc_prefix`, `id_to_index`, `param_ids`, `legacy_param_aliases`, `legacy_node_aliases`, `legacy_value_aliases`. It just needs to absorb the graph topology fields (the existing `EffectGraphDef::nodes` + `wires`), the bindings, the skip mode, and the category. Then `EffectDef` becomes the unified runtime view of a loaded preset.

This is a happy finding — the load-bearing addressing infrastructure (`id_to_index` map walked by every OSC / driver / project-storage lookup) doesn't need to be rebuilt. It already exists with the right shape; it just changes its data source from `EffectMetadata::to_effect_def()` to `LoadedPreset::to_effect_def()`. The 50+ callsites in `effects.rs` and `project.rs` that go through `effect_definition_registry::try_get()` continue to work unchanged.

### 11.8 Build.rs precedent

The workspace already uses build.rs in `manifold-media` and `manifold-recording`. Adding one to `manifold-renderer` (or `manifold-core`, depending on where the codegen target lives) is precedented. The build.rs will:

1. Scan `crates/manifold-renderer/assets/effect-presets/*.json`
2. For each, parse into `EffectGraphDef` and validate
3. Validate that every `typeId` referenced in `nodes` corresponds to a registered primitive (via inventory iteration at build time — but build.rs runs *before* the crate compiles, so this check happens at runtime startup instead; build.rs only does schema-shape validation)
4. Emit `target/<crate>/generated/effect_type_constants.rs` with `pub const FOO: EffectTypeId = EffectTypeId::new("Foo");` for each preset
5. Emit a `bundled_presets!` macro invocation or const slice with `include_str!`-embedded JSON for runtime loading

The "every typeId references a registered primitive" check has to happen at runtime startup because build.rs can't iterate `inventory::*` (the inventory crate works at link time, after build.rs). That's fine — runtime startup fail is acceptable, and the build.rs schema check catches most authoring errors earlier.

### 11.9 Legacy discriminant — JSON-resident

Each preset's JSON gets a `legacyDiscriminant: Option<i32>` field (already in `EffectMetadata::legacy_discriminant`, just moves into JSON). At startup, after loading all presets, `EffectTypeId::from_legacy_discriminant(v)` builds a reverse map `i32 → EffectTypeId` from the loaded set. The hand-coded `match v { 0 => Self::TRANSFORM, 1 => Self::INVERT_COLORS, ... }` table in `effect_type_id.rs` deletes entirely.

`GeneratorTypeId::from_legacy_discriminant` follows the same pattern but is out of scope for this migration (generators aren't presets, they're standalone procedural sources).

### 11.10 Refined scope

Original §10.5 estimate: 1-2 weeks. Adjusted with these findings: **~2 weeks**, in this order:

1. **`EffectGraphDef` v2 schema** (1d). Add new fields with serde defaults so v1 documents still parse. Bump version constant. Migration tests.
2. **JSON loader → `EffectDef`** (1d). New `loaded_preset_to_effect_def()` builder that takes a parsed `EffectGraphDef` and produces an `EffectDef`. Update `effect_definition_registry::build_definitions()` to iterate loaded presets instead of `inventory::iter::<EffectMetadata>()`. All consumers via `effect_definition_registry::try_get()` keep working unchanged.
3. **`build.rs` codegen** (1d). Schema validation + constant generation + embedded JSON table.
4. **Migrate 25 effect JSON files** (2-3d). For each, populate the new metadata fields from the soon-to-be-deleted Rust file. Most are mechanical; the four composite effects keep their existing multi-node graphs. A per-effect test confirms the loaded preset behaves identically to what the splice fn used to produce.
5. **`PluginPrewarm` channel** (0.5d). New inventory type, three submissions (blob_tracking, depth_of_field, wireframe_depth), startup hook.
6. **Rewire `effect_chain_graph.rs`** (0.5d). Replace `chain_spec_by_id()` callsites with `loaded_preset_by_id()`. Delete the splice-fn invocation in favor of `EffectGraphDef::instantiate()`.
7. **Rewire snapshot path** (0.5d). `graph_snapshot_for()` uses the same `ChainGraph` path as live render.
8. **Delete legacy code** (1d). All 25 effect Rust files (after their JSON is verified). `ChainSpec` type. `EffectFactory`. `EffectRegistry`. `effect_category_registry.rs`. The `BUNDLED_PRESETS` table. The drift test. The `register_via_spec!` macro. `from_legacy_discriminant` const tables.
9. **Primitive auto-registration via inventory** (1d). Each `primitive!` macro emits a `PrimitiveFactoryEntry` submission. Hand-written primitives get one-line additions. `PrimitiveRegistry::with_builtin()` iterates inventory. Delete the manual list.
10. **Verification pass** (1d). Full workspace tests, parity tests, real Liveschool project load, picker walkthrough, MIDI mapping smoke test.

### 11.11 Decisions to settle before starting

The audit changes nothing about the architectural target. But three implementation-level choices are worth making explicit:

1. **Plugin warmup mechanism — (b) `PluginPrewarm` inventory.** Lazy-init in primitive `run()` causes uneven first-frame stutter; a separate channel is cleaner.

2. **Snapshot rendering — through ChainGraph, not EffectFactory.** Same path as live chain. Delete `EffectFactory` and `EffectRegistry` entirely.

3. **`EffectDef` stays as the runtime view.** It's the unified consumer-facing shape; only its data source changes (loaded presets instead of inventory). All 50+ consumers in `effects.rs` / `project.rs` keep working unchanged.

After this migration the system has *one source of truth per category*:

- **Primitives** — Rust+WGSL files in `primitives/`, auto-registered via inventory.
- **Presets** — JSON files in `assets/effect-presets/` (bundled) or `Project.preset_library` (user-saved), loaded into `EffectDef`s at startup, consumed identically regardless of origin.
- **Plugin warmup** — `inventory::submit!(PluginPrewarm)` from the 3 plugin-using primitives, period.

No hand-maintained lists. No drift tests. No "did you remember to update X." Adding a primitive = drop 2 files + 1 `mod` line. Adding a preset = drop 1 JSON file. Same shape whether it's authored by Peter, Claude, an AI agent, or eventually a user via the graph editor's "Save Preset" affordance.


## 12. Node-type taxonomy (2026-05-18)

The §10 plan organised work into phases (A–E). This section is the orthogonal cut — **what kinds of nodes exist in the graph language**, in the shape they need to converge to so users can decompose every effect down to a small set of composable primitives. Reference for future authoring decisions.

### 12.1 What shipped post-Phase B kickoff

- **Control wire plumbing** (`cc6d0856`) — `PortType::Scalar(ScalarType)`, `Backend::set_scalar`, `NodeOutputs::set_scalar` with per-step scratch drain. Macro learned `ScalarF32`/`ScalarVec2`/etc. port types. Convention: when a primitive declares an optional `Scalar` input port with the same name as a same-named `ParamDef`, the wire shadows the param when present (FluidSim pattern). First wired consumer: `wet_dry_mix.wet_dry`.
- **Control producers** (`239877fb`) — `node.value` (constant scalar), `node.lfo` (beat-locked oscillator, sine/triangle/saw/square, stateless), `node.math` (binary op, divide-by-zero clamps to 0).
- **Auto-populated palette** (`3de11521`) — `PrimitiveFactory` carries `picker: Option<PickerInfo>`; macro accepts `picker: { label, category }`; `palette_atoms()` walks inventory. New nodes appear in the editor by declaring picker info at their definition site, not by editing a central list.

### 12.2 Remaining V1 node categories

Five categories round out the V1 surface. Combined with the texture and scalar/math primitives already shipped, these cover the bulk of 2D-image + control-rate effect authoring.

**Texture→Scalar bridges.** Read a texture, emit a scalar. Brightness/peak/centroid measurement, motion energy from frame-to-frame difference, color sample at a UV, FFT band extraction from an audio spectrum texture. New flow direction (image → control). Without these, control wires can only carry external/time signals, never anything derived from the image stream — and "this effect reacts to its own content" is half the responsive-instrument promise.

**External source nodes.** Driver nodes wrapping MIDI CC, OSC, Ableton macros, audio FFT bands, and beat/bar/phase as scalar outputs. The plumbing already exists in the renderer's modulation/binding system; surfacing them as first-class driver nodes means external inputs flow through the same graph language as everything else instead of being a special-case slider mapping. Beat/bar/phase deserve dedicated nodes (`node.beat`, `node.phase`, `node.bar`) rather than being implicit via `ctx.time` — same musical model, same graph language.

**Stateful control nodes.** The scalar equivalent of `Feedback`. Smoothing (one-pole filter with a time-constant param), Sample-and-Hold (latch value on trigger), Envelope (attack/decay/sustain/release, triggered on a gate scalar), Step Sequencer (N values, advance on a beat input). All use the existing `StateStore` pattern Feedback uses today — same indexing by `node_id + owner_key`, same `clear_state` for seek/pause.

**Convolution with kernel-as-input.** One atomic primitive whose kernel weights are an editable input (1D for separable axes, 2D for general). Gaussian Blur, Box Blur, Sharpen, Edge Detect, Sobel all become "Convolution + a different kernel." Users open the node and see the actual weights; can edit them. Replaces today's hand-coded kernel constants inside `node.gaussian_blur` with a generic primitive plus a kernel-weights input. The KernelInput widget is its own UI piece — a small float-grid editor with optional preset kernels.

**WGSL Shader escape hatch.** A `node.shader` primitive that takes a small WGSL fragment as a string param. The graph language can never fully match WGSL's expressiveness (per-pixel runtime-varying kernel sizes like DoF, iterative state, gather from computed offsets — what shaders exist to express), so eventually some leaf needs to drop to WGSL. Houdini does this with VEX snippets; Substance Designer with the Pixel Processor node. This is the bottom of the ladder; below it is the GPU. Open UX problems: how does the editor surface compile errors (probably inline indicator + passthrough on failure), and what's the input/output port shape — fixed `(in: Texture2D, params: scalars, out: Texture2D)` is probably the V1 shape; multi-input/multi-output is a future extension.

### 12.3 V2-deferred axes

Three categories of node intentionally outside the V1 scope. Each is a real gap; each has a tractable workaround for V1 (usually via the WGSL Shader node).

**Buffer / array data.** Particle positions, audio sample buffers, mesh vertex arrays. Today these live inside opaque atomic generators (OscilloscopeXY's audio buffer, ParametricSurface's vertex array, Mycelium's particle list). The V1 port types can't carry array data — they're scalars or textures. Encoding arrays as 1×N textures works for audio waveforms but is ugly for mesh data with multiple per-vertex attributes. A `Buffer` port type is the missing axis. Listed as V2 in the original §10 port-type discussion.

**Loop / subgraph iteration.** Some effects iterate per frame — fluid sim runs ~20 pressure-projection passes per frame, multi-bounce refraction iterates the lens equation. Today only the WGSL Shader node can express that. A `SubgraphIterate(N)` primitive that runs a subgraph N times with output feedback would let those effects fully decompose, but it's a graph-runtime feature (the executor needs to support running a subgraph as one step with internal state), not just a new primitive.

**3D volume primitives.** `PortType::Texture3D` exists, `FluidSim3D` uses it, but no primitives operate on volumes today (no `Sample3D`, no `SliceVolume → Texture2D`, no per-voxel math). If volumetric effects are ever on the roadmap, that's a parallel family of texture primitives — mirror of the existing 2D set but on volumes.

### 12.4 Decomposition + fusion-on-compile

The architectural stance the §10 plan was implicitly aiming at, now explicit:

**Decompose everything authored from now on into the smallest primitives that compose cleanly.** The graph editor is the teaching surface — users learn how effects work by opening them up. Aesthetic operators get authored as visible compositions of primitives + kernel inputs + occasional WGSL Shader nodes at the leaves. No new effect should ship as a single opaque shader if it can be expressed as a graph of existing primitives.

**Accept the per-dispatch overhead during authoring.** Each primitive is a separate GPU dispatch with its own encoder boundary and intermediate texture. For the V1 catalog this is fine — the chain runtime is fast enough that single-primitive overhead doesn't dominate. Measure before pre-optimising.

**Add a fusion-on-compile pass when perf demands it.** Recognise common atomic chains (per-pixel ops with no gather between them) and emit a single fused shader for the hot path. The editor still sees small primitives; the GPU runs fused. Gather chains (Blur, MipChain, anything with a non-trivial neighbourhood read) stay as real intermediate textures because the producer has to materialise — those effects are already composites anyway.

The §10.4 "EffectGraphDef v1→v2 migration" cross-cutting note is the entry point for fusion: once the v2 schema is in place, the compile pass takes a v2 graph + a fusion ruleset and emits either a sequence of dispatches or a fused shader per fusable chain.

### 12.5 The fp16-as-blocker myth (correcting §6.1)

§6.1's parity-migration notes mention that some effects (Strobe specifically) had to ship as fused composites because decomposing them broke pixel-exact parity through fp16 intermediate textures. That framing assumed all wires were `Rgba16Float` texture wires. With scalar wires (`PortType::Scalar(F32)`, `ParamValue::Float(f32)`) the constraint dissolves for scalar data: BeatGate's amount flows through an f32 scalar wire, never touches an fp16 texture, and reaches Mix at full precision. Pixel-exact parity is achievable without fusion-on-compile *for any decomposition where the inter-primitive value is scalar*.

The remaining fp16 case — where one primitive's *texture* output feeds another's *texture* input — is real and intrinsic: textures are `Rgba16Float` and the round-trip quantises. But every effect with gather-based texture intermediates (Bloom, Halation, Watercolor — the MipChain/Blur composites) already pays this cost today and the parity tests already cover them. fp16 isn't a fundamental decomposition blocker; it's an artifact of using texture wires for scalar data, which scalar wires now fix.

### 12.6 What we deliberately avoid

The temptation to ship a "purpose-built" primitive every time something feels slightly awkward. The whole pitch only works if the primitive set stays small and composable. Every time we're tempted to add `node.special_thing_for_one_effect`, the right question is: *could this be Convolution + a kernel? Math + an LFO? a WGSL Shader fragment?* If yes, ship it that way. If we end up with 80 primitives, we've just rebuilt the flat catalog one level lower.

