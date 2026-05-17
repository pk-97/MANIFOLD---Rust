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
28. **[shipped]** `EffectChain::apply_chain` is a thin wrapper over `ChainGraph::try_build` + `ChainGraph::run` — the graph-runtime path is the only path. Static elision via `ChainSpec::SkipMode::OnZero` (effects with `amount ≤ 0` are dropped from the plan) and wet/dry sub-graphs via `OpenGroup` + multi-segment `Mix` both ship. Dynamic bypass (per-frame `bypass_predicate` on `ExecutionStep` that would preserve primitive state across `amount=0` crossings without a rebuild) explicitly deferred — current rebuild-on-flip behavior is acceptable. The `EffectChain` shim itself disappears in #31.
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
