# Tier 3 Fix Plan: `manifold-renderer` Parity Remediation

**Status: COMPLETE** — Implemented 2026-03-18, commit `6ffdfe9`

**Generated:** 2026-03-18 from line-by-line audit of all Unity Compositing/*.cs, Effects/*.cs, Generators/*.cs, and Shaders/*.shader/*.compute against Rust manifold-renderer/src/**/*.rs and *.wgsl

**Methodology:** Every fix below references the exact Unity source file and line numbers. The implementing agent MUST read the Unity source — not this document — as the source of truth. This plan tells you WHAT to fix and WHERE to look, not HOW the code should read.

**Dependency:** Tier 0 fixes (manifold-core) should be completed first, especially `EffectContainer` trait and `EffectDefinitionRegistry`. Some compositor fixes depend on Tier 1 (playback engine callbacks).

**This tier is split into three parts:**
- **Part A:** Compositor + Effect Infrastructure (blend shader, compositing pipeline, effect chain)
- **Part B:** Individual Effects (16 ported effects)
- **Part C:** Generators (18 generators + infrastructure)

---

# PART A: COMPOSITOR + EFFECT INFRASTRUCTURE

---

## Phase 1: Blend Shader Fixes (CRITICAL — Every Composite Operation Is Wrong)

### 1A. Fix Normal blend mode (case 0) — premultiplied alpha-over

**File:** `shaders/compositor_blend.wgsl` line 84-85
**Unity source:** `Shaders/VideoCompositor.shader` line 218-221

**Bug:** Rust uses straight alpha compositing (`mix(base, blend, alpha)` = `base * (1 - alpha) + blend * alpha`). Unity uses premultiplied alpha-over (`result.rgb = blend.rgb + base.rgb * (1 - blend.a)`).

For premultiplied content (ALL generator output), straight alpha compositing multiplies the blend color by alpha AGAIN, causing double-dimming. Generator output at alpha=0.5 will appear at 25% brightness instead of 50%.

**Fix:** Replace the Normal blend case with premultiplied alpha-over:
```wgsl
// Normal: premultiplied alpha-over
blended = f_val + b * (1.0 - bl_a);
out_a = bl_a + ba * (1.0 - bl_a);
```
Match Unity lines 218-221 EXACTLY.

### 1B. Fix Screen blend mode (case 3) — HDR safety

**File:** `compositor_blend.wgsl` line 97
**Unity source:** `VideoCompositor.shader` lines 92-102

**Bug:** Rust uses simple `b + f - b * f`. Unity clamps both inputs to [0,1], applies the formula on clamped values, then adds HDR overflow back: `result = 1 - (1 - clamp(base)) * (1 - clamp(blend)) + max(0, base-1) + max(0, blend-1)`.

**Fix:** Port Unity's HDR-safe screen formula line-by-line.

### 1C. Fix Overlay blend mode (case 4) — HDR safety

**File:** `compositor_blend.wgsl` lines 100-103
**Unity source:** `VideoCompositor.shader` lines 104-117

**Bug:** Same HDR safety issue as Screen. Unity clamps to [0,1] for the overlay computation and adds overflow.

**Fix:** Port Unity's HDR-safe overlay formula line-by-line.

### 1D. Fix Exclusion blend mode (case 8) — missing clamp

**File:** `compositor_blend.wgsl` line 121
**Unity source:** `VideoCompositor.shader` line 131

**Bug:** Missing `max(0, ...)` clamp. Exclusion can go negative with HDR values.

**Fix:** Change to `blended = max(vec3(0.0), b + f_val - 2.0 * b * f_val);`

### 1E. Fix ColorDodge blend mode (case 10) — HDR cap

**File:** `compositor_blend.wgsl` lines 128-133
**Unity source:** `VideoCompositor.shader` lines 144-153

**Bug:** Rust caps at 1.0 (kills HDR highlights). Unity caps at 100.0. Different threshold: Rust uses 1.0, Unity uses 0.999.

**Fix:** Match Unity exactly:
- When `blend >= 0.999`, result = 100.0 (not 1.0)
- Otherwise, `base / (1.0 - blend)` unclamped (don't clamp to 1.0)

### 1F. Verify Stencil blend mode (case 5)

**File:** `compositor_blend.wgsl` lines 106-109
**Unity source:** `VideoCompositor.shader` lines 222-227

**Issue:** Rust multiplies opacity into stencil alpha result. Unity applies opacity via the final lerp. Verify these are equivalent or fix.

---

## Phase 2: Compositor Pipeline Fixes (CRITICAL — Architecture Gaps)

### 2A. Add group-aware compositing

**File:** `layer_compositor.rs`
**Unity source:** `CompositorStack.cs` lines 622-688 (`PerformCompositeGrouped`)

**Bug:** Entirely missing. Layer groups with effects will not composite correctly. All child layers are composited flat; group effects are never applied; group wet/dry doesn't work at group level.

**Fix:** Port `PerformCompositeGrouped()`:
1. Walk layers in reverse order (matching Unity line 624)
2. Detect group layers (`layer.IsGroup`, `layer.ParentLayerId`)
3. Composite children into group-level buffers (`groupPingBuffer`/`groupPongBuffer`)
4. Apply group-level effects to the group buffer
5. Apply group wet/dry lerp
6. Blit group result to main buffer

Add `group_ping_buffer` and `group_pong_buffer` to `LayerCompositor`.

### 2B. Fix single-clip layer effect application

**File:** `layer_compositor.rs` lines 423-466
**Unity source:** `CompositorStack.cs` lines 414-449

**Bug:** When `group.len() == 1`, Rust blits directly to main with clip-level effects only. Unity routes ALL layers that have `HasModularEffects` through the layer buffer path, regardless of clip count.

**Fix:** For single-clip layers WITH layer-level effects, route through the layer buffer:
1. Blit clip to layer buffer
2. Apply layer-level effects to layer buffer
3. Blit layer buffer to main with blend mode

Only skip the layer buffer when the layer has NO effects (neither clip-level nor layer-level).

### 2C. Fix `has_enabled_effects` to check param[0]

**File:** `layer_compositor.rs` line 289
**Unity source:** `CompositorStack.cs` lines 965-974

**Bug:** Rust checks only `fx.enabled`. Unity checks `enabled && GetParam(0) > 0`. Effects with amount=0 are treated as inactive in Unity but active in Rust.

**Fix:** Add `&& fx.param_values.first().copied().unwrap_or(0.0) > 0.0` to the enabled check.

---

## Phase 3: Effect Infrastructure Fixes

### 3A. Add `cleanup_all_owners()` to `StatefulEffect` trait

**File:** `effect.rs` lines 45-51
**Unity source:** `IStatefulEffect.cs` line 18

**Bug:** Missing method. Unity calls `CleanupAllOwners()` during `Clear()` (stop playback), `ResizeBuffers()`, and `WarmupShaders()`. Without it, per-owner GPU state leaks.

**Fix:** Add to trait:
```rust
fn cleanup_all_owners(&mut self, device: &wgpu::Device);
```
Implement on all stateful effects (Bloom, CRT, Feedback, Halation, StylizedFeedback).

### 3B. Add `chain` and `find_chain_param()` to `EffectContext`

**File:** `effect.rs` lines 5-15
**Unity source:** `EffectContext.cs` lines 28-50

**Bug:** Missing `chain` field (list of all effects in current chain) and `FindChainParam(EffectType, paramIndex)` method. Used by VoronoiPrismFX to read EdgeStretch's `sourceWidth` parameter.

**Fix:** Add `chain: &[EffectInstance]` to `EffectContext` and port `FindChainParam()`:
```rust
pub fn find_chain_param(&self, effect_type: EffectType, param_index: usize) -> Option<f32> {
    self.chain.iter()
        .find(|fx| fx.effect_type == effect_type && fx.enabled)
        .and_then(|fx| fx.param_values.get(param_index).copied())
}
```

### 3C. Add default `should_skip` behavior

**File:** `effects/simple_blit_helper.rs`
**Unity source:** `SimpleBlitEffect.cs` line 37

**Issue:** Unity's `SimpleBlitEffect` base class has a default `ShouldSkip` that returns true when `GetParam(0) <= 0`. Each Rust effect must independently implement this. If any effect forgets, it will needlessly process when amount is zero.

**Fix:** Add a helper function or document the requirement:
```rust
pub fn should_skip_default(fx: &EffectInstance) -> bool {
    fx.param_values.first().copied().unwrap_or(0.0) <= 0.0
}
```
Verify all 16 effects call this or have equivalent logic.

### 3D. Add `midChainTap` support to `EffectChain`

**File:** `effect_chain.rs`
**Unity source:** `CompositorStack.cs` lines 864-865, 918-920

**Issue:** Unity's `ApplyEffectChain` accepts optional `midChainTap` callback and `midChainTapIndex` for LED output external tap. Missing from Rust.

**Fix:** Add optional tap parameters to `apply_chain()`. Required for external output (LED walls) feature.

---

# PART B: INDIVIDUAL EFFECTS

---

## Phase 4: StrobeFX — CRITICAL Rate Bug

### 4A. Add NoteRates lookup table

**File:** `effects/strobe.rs` line 51
**Unity source:** `StrobeFX.cs` lines 12-27

**Bug:** Raw param index (0-8) is passed directly as the strobe rate. Unity maps the index through a `NoteRates[]` lookup table: `[0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0]`.

Every strobe rate setting produces the wrong frequency:
- Index 0 ("1/1"): Unity sends 0.25, Rust sends 0.0
- Index 2 ("1/4"): Unity sends 1.0, Rust sends 2.0
- Index 6 ("1/16"): Unity sends 4.0, Rust sends 6.0

**Fix:** Add the constant array and map:
```rust
const NOTE_RATES: [f32; 9] = [0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0];

// In apply():
let rate_idx = p.get(1).copied().unwrap_or(6.0).round() as usize;
let rate = NOTE_RATES[rate_idx.min(NOTE_RATES.len() - 1)];
```

---

## Phase 5: FilmGrainFX — Missing Color Grain

### 5A. Add `_ColorGrain` uniform field

**File:** `effects/film_grain.rs`
**Unity source:** `FilmGrainFX.cs` line 16

**Bug:** Param index 3 (`ColorGrain`, 0..1) is never read. The uniform struct has no `color_grain` field.

**Fix:** Add `color_grain: f32` to the uniform struct. Read `fx.param_values[3]`.

### 5B. Add color grain noise generation to WGSL shader

**File:** `effects/shaders/fx_film_grain.wgsl` lines 50-54
**Unity source:** `Shaders/FilmGrain.shader` lines 82-87

**Bug:** Only monochrome noise exists. Unity generates per-channel R/G/B noise via `hash3seed()` and blends `lerp(mono_noise, color_noise, _ColorGrain)`.

**Fix:** Port the `hash3seed()` function and per-channel noise generation:
1. Add `hash3seed(p: vec2<f32>, seed: f32) -> f32` function
2. Generate `n_r`, `n_g`, `n_b` with seeds 0.0, 1.0, 2.0
3. Blend: `noise = mix(vec3(n_mono), vec3(n_r, n_g, n_b), color_grain)`

### 5C. Fix hash function divergence

**File:** `fx_film_grain.wgsl` lines 32-36
**Unity source:** `FilmGrain.shader` hash function

**Issue:** Rust's `hash()` computes the dot product differently from Unity. Unity does `p3 += dot(p3, p3.yzx + 33.33)` (adds scalar to vector). Rust computes differently.

**Fix:** Translate Unity's hash function line-by-line.

---

## Phase 6: DitherFX — Bayer Matrix

### 6A. Consider replacing procedural Bayer with lookup table

**File:** `fx_dither.wgsl` lines 30-48
**Unity source:** `Dither.shader` — explicit `float bayer8x8[64]` lookup table

**Issue:** Rust uses procedural bit-reversal interleave computation instead of Unity's explicit lookup table. The procedural approach should produce the same standard Bayer 8x8 matrix, but is a structural divergence.

**Recommendation:** Replace with explicit lookup table matching Unity for mechanical translation fidelity. If WGSL supports array constants, use them.

---

# PART C: GENERATORS

---

## Phase 7: Generator Infrastructure Fixes (CRITICAL)

### 7A. Fix `project_4d` — missing second perspective stage

**File:** `generators/generator_math.rs` lines 46-53
**Unity source:** `GeneratorMath.cs` — `Project4D` method

**Bug:** Rust performs only ONE perspective division (4D→2D directly). Unity performs TWO stages:
1. 4D→3D: `f = projDist / (projDist - w)` then `p3 = xyz * f`
2. 3D→2D: `s = projDist / (projDist + p3z)` then `px = p3x * s * PROJ_SCALE`, `py = p3y * s * PROJ_SCALE`

Additionally:
- `pz` in Unity is raw `p3z` (NOT scaled by `s` or `PROJ_SCALE`). Rust scales pz.
- Rust adds a safety clamp `denom.abs() > 0.001` that Unity doesn't have (FM-5).

**Fix:** Port the two-stage projection exactly from Unity:
```rust
pub fn project_4d(x: f32, y: f32, z: f32, w: f32, proj_dist: f32) -> (f32, f32, f32) {
    // Stage 1: 4D → 3D
    let f = proj_dist / (proj_dist - w);
    let p3x = x * f;
    let p3y = y * f;
    let p3z = z * f;

    // Stage 2: 3D → 2D
    let s = proj_dist / (proj_dist + p3z);
    let px = p3x * s * PROJ_SCALE;
    let py = p3y * s * PROJ_SCALE;
    let pz = p3z;  // raw depth, NOT scaled

    (px, py, pz)
}
```

**Affects:** Duocylinder, Tesseract — both will look fundamentally different without the second perspective stage (flat instead of 3D depth foreshortening).

### 7B. Add `dot_scale` parameter to line pipeline

**File:** `generators/line_pipeline.rs`
**Unity source:** `LineGeneratorBase.cs` line 88 — `GetDotScale(ctx)` multiplier

**Bug:** Unity's `LineGeneratorBase.Render()` applies `GetDotScale()` as an extra multiplier on dot radius. Default is 1.0, but Lissajous and OscilloscopeXY override it to 0.5. Rust's `build_vertices` has no `dot_scale` parameter.

**Fix:** Add `dot_scale: f32` parameter to `build_vertices()`. Default to 1.0. Lissajous and OscilloscopeXY should pass 0.5.

---

## Phase 8: Duocylinder & Tesseract — Double Scale

### 8A. Fix Duocylinder double-scale application

**File:** `generators/duocylinder.rs` lines 107-108, 121
**Unity source:** `DuocylinderGenerator.cs` — `Project()` method

**Bug:** Scale is applied TWICE:
1. In the projection loop: `projected_x[i] = px * scale` (line 107)
2. In `build_vertices()` which receives `scale` parameter and applies it again (line 121)

In Unity, `Project()` stores raw projected values. Scale is applied ONCE by `LineGeneratorBase.Render()`.

**Fix:** Remove `* scale` from the projection loop (lines 107-108). Let `build_vertices` handle scale application.

### 8B. Fix Tesseract double-scale application

**File:** `generators/tesseract.rs` lines 99-100
**Unity source:** `TesseractGenerator.cs`

**Bug:** Same double-scale issue as Duocylinder.

**Fix:** Same — remove `* scale` from projection loop.

---

## Phase 9: StrangeAttractor (CPU) — CRITICAL Constant Mismatches

### 9A. Fix all ODE constant mismatches

**File:** `generators/strange_attractor.rs`
**Unity source:** `StrangeAttractorGenerator.cs`

**9 constant mismatches (all FM-11 violations):**

| Attractor | Coefficient | Unity Value | Rust Value | Fix |
|-----------|------------|-------------|------------|-----|
| Lorenz | sigma | `10 + c * 4` | `10 + chaos * 5` | Change 5→4 |
| Lorenz | rho | `28 + c * 8` | `28 + chaos * 10` | Change 10→8 |
| Lorenz | beta | `8/3 + c * 0.5` | `8/3` (constant) | Add `+ chaos * 0.5` |
| Rossler | b | `0.2 + c * 0.1` | `0.2` (constant) | Add `+ chaos * 0.1` |
| Aizawa | a | `0.95 + c * 0.1` | `0.95` (constant) | Add `+ chaos * 0.1` |
| Aizawa | b | `0.7 + c * 0.2` | `0.7` (constant) | Add `+ chaos * 0.2` |
| Aizawa | d | `3.5 + c * 1` | `3.5 + chaos * 1.5` | Change 1.5→1 |
| Thomas | b | `0.208186 - c * 0.05` | `0.208186 + chaos * 0.1` | Fix sign (subtract), change 0.1→0.05 |

The Thomas attractor is the worst — the sign is FLIPPED and the coefficient is wrong. This completely changes the attractor's behavior.

### 9B. Fix projection method

**File:** `generators/strange_attractor.rs` — projection function
**Unity source:** `StrangeAttractorGenerator.cs` — `ProjectPoint()`

**Bug:** Rust uses a full look-at camera matrix with FOV. Unity uses a simple orbiting camera with Y-axis rotation + tilt (0.3) + perspective.

**Fix:** Port Unity's `ProjectPoint()` exactly:
1. Y-rotation by `camAngle`
2. Fixed tilt of 0.3
3. Perspective: `2 / (uvScale * max(depth, 0.3))`

### 9C. Add warmup steps

**Unity source:** `StrangeAttractorGenerator.cs` — 50 warmup steps before rendering
**Rust:** No warmup steps.

**Fix:** Add 50 warmup integration steps on initialization to let trajectories settle onto the attractor.

---

## Phase 10: Fluid Simulation Texture Formats & Particle Caps

### 10A. Fix FluidSimulation texture formats

**File:** `generators/fluid_simulation.rs`
**Unity source:** `FluidSimulationGenerator.cs`

| Texture | Unity Format | Rust Format | Fix |
|---------|-------------|-------------|-----|
| Density | `RFloat` (R32Float) | `Rgba16Float` | Change to `R32Float` |
| Vector field | `RGFloat` (Rg32Float) | `Rgba16Float` | Change to `Rg32Float` |

**Note:** If `R32Float` or `Rg32Float` don't support required usage flags on Metal, add a runtime fallback with a comment referencing the Unity format. The source constant must match Unity.

### 10B. Fix FluidSimulation particle cap

**File:** `generators/fluid_simulation.rs`
**Unity source:** `FluidSimulationGenerator.cs` — `ParticleCount => 8000000`

**Bug:** Rust clamps to `2_000_000`. Unity uses `8_000_000`.

**Fix:** Change cap to `8_000_000`. If Metal's 128MB buffer limit is a concern, add a RUNTIME clamp (not a source-code change) per CLAUDE.md FM-9 and FM-11.

### 10C. Fix FluidSimulation3D particle cap

**File:** `generators/fluid_simulation_3d.rs`
**Unity source:** `FluidSimulation3DGenerator.cs` — `MAX_PARTICLES = 8_000_000`

**Bug:** Same — Rust clamps to `2_000_000`.

**Fix:** Change to `8_000_000`. Same runtime clamp guidance as 10B.

### 10D. Fix Mycelium trail texture format

**File:** `generators/mycelium.rs`
**Unity source:** `MyceliumGenerator.cs`

**Bug:** Trail texture format is `Rgba16Float`. Unity uses `RFloat` (R32Float) — single-channel density storage.

**Fix:** Change `TRAIL_FORMAT` to `R32Float`. Add runtime fallback if needed.

---

## Phase 11: WireframeZoo Fixes

### 11A. Normalize shape vertices

**File:** `generators/wireframe_zoo.rs`
**Unity source:** `WireframeZooGenerator.cs` — `NormalizeShape()` method

**Bug:** Rust uses raw vertex coordinates without normalization. Unity normalizes all vertices to unit distance from origin.

Affected shapes and their raw radii:
- Tetrahedron: sqrt(3) ≈ 1.73x too large
- Cube: sqrt(3) ≈ 1.73x too large
- Icosahedron: ~1.90x too large
- Dodecahedron: ~2.48x too large
- Octahedron: already unit distance (OK)

**Fix:** Either:
1. Port `NormalizeShape()` and apply it to each shape's vertices at init, OR
2. Pre-normalize the hardcoded vertex tables

### 11B. Fix `projected_z` scaling

**File:** `generators/wireframe_zoo.rs` line 195
**Unity source:** `WireframeZooGenerator.cs`

**Bug:** Rust scales `projected_z` by `PROJ_SCALE`. Unity stores raw z without scaling.

**Fix:** Remove `* proj_scale` from the z assignment: `self.helper.projected_z[i] = z;`

---

## Phase 12: Missing Effects (14 not ported)

These effects exist in Unity but have no Rust implementation. Listed for tracking:

| EffectType | Unity Class | Complexity | Priority |
|-----------|------------|------------|----------|
| Transform | TransformFX | Simple (4 params) | LOW — handled by compositor blend shader |
| PixelSort | ComputePixelSortFX | High (compute) | MEDIUM |
| InfiniteZoom | InfiniteZoomFX | Medium (stateful) | MEDIUM |
| VoronoiPrism | VoronoiPrismFX | High (compute) | MEDIUM |
| BlobTracking | BlobTrackingFX | High (compute) | LOW |
| FluidDistortion | FluidDistortionFX | High (compute) | MEDIUM |
| EdgeGlow | EdgeGlowFX | Medium | MEDIUM |
| Datamosh | DatamoshFX | High (stateful) | MEDIUM |
| SlitScan | SlitScanFX | High (stateful) | MEDIUM |
| WireframeDepth | WireframeDepthFX | Medium | LOW |
| GradientMap | GradientMapFX | Simple | HIGH |
| Microscope | MicroscopeFX | Medium | LOW |
| Corruption | CorruptionFX | Medium | MEDIUM |
| Infrared | InfraredFX | Simple | LOW |
| Surveillance | SurveillanceFX | Medium | LOW |
| Redaction | RedactionFX | Medium | LOW |

**Recommendation:** Port GradientMap first (simple, high visual impact). Then compute effects (PixelSort, VoronoiPrism, FluidDistortion) as they're visually distinctive.

---

## Verification Checklist

After implementing all phases:

### Compositor & Blend Shader
- [ ] Normal blend mode uses premultiplied alpha-over (not straight alpha mix)
- [ ] Screen blend mode is HDR-safe (clamp+overflow pattern)
- [ ] Overlay blend mode is HDR-safe
- [ ] Exclusion blend mode clamps negative to 0
- [ ] ColorDodge caps at 100.0, threshold 0.999
- [ ] Group-aware compositing works (children→group buffer→effects→main)
- [ ] Single-clip layers with layer effects go through layer buffer
- [ ] `has_enabled_effects` checks param[0] > 0

### Effect Infrastructure
- [ ] `StatefulEffect` trait has `cleanup_all_owners()`
- [ ] `EffectContext` has `chain` field and `find_chain_param()`
- [ ] Default `should_skip` checks param[0] <= 0

### Individual Effects
- [ ] StrobeFX uses `NoteRates[]` lookup table (not raw index)
- [ ] FilmGrainFX reads param[3] (ColorGrain) and generates per-channel noise
- [ ] FilmGrainFX hash function matches Unity exactly

### Generator Infrastructure
- [ ] `project_4d` uses two-stage perspective (4D→3D→2D)
- [ ] `project_4d` pz is raw depth (not scaled by PROJ_SCALE)
- [ ] `build_vertices` supports `dot_scale` parameter

### Individual Generators
- [ ] Duocylinder: scale applied once (not twice)
- [ ] Tesseract: scale applied once (not twice)
- [ ] StrangeAttractor: all 9 ODE constants match Unity EXACTLY
- [ ] StrangeAttractor: Thomas b coefficient is `0.208186 - chaos * 0.05` (SUBTRACT)
- [ ] StrangeAttractor: projection uses simple orbit camera (not look-at matrix)
- [ ] FluidSimulation: density format is R32Float, vector field is Rg32Float
- [ ] FluidSimulation: particle cap is 8M (not 2M)
- [ ] FluidSimulation3D: particle cap is 8M (not 2M)
- [ ] Mycelium: trail format is R32Float (not Rgba16Float)
- [ ] WireframeZoo: all shape vertices normalized to unit distance
- [ ] WireframeZoo: projected_z not scaled by PROJ_SCALE

### Build
- [ ] `cargo build` succeeds for manifold-renderer
- [ ] `cargo test` passes for manifold-renderer
- [ ] Existing effects still render correctly after compositor changes

---

## Priority Order

**P0 — Everything looks wrong (compositor):**
1. Phase 1A: Normal blend mode premultiplied alpha (every generator composite is wrong)
2. Phase 2B: Single-clip layer effects skipped
3. Phase 2A: Group-aware compositing missing

**P1 — Specific visuals wrong:**
4. Phase 1B-1E: Screen/Overlay/Exclusion/ColorDodge blend fixes
5. Phase 7A: project_4d missing second perspective stage
6. Phase 9A: StrangeAttractor 9 ODE constant mismatches
7. Phase 4A: StrobeFX NoteRates lookup missing
8. Phase 10A-10D: Texture format mismatches (FM-10 violations)
9. Phase 10B-10C: Particle cap 8M→2M (FM-11 violations)

**P2 — Feature/infrastructure gaps:**
10. Phase 3A: cleanup_all_owners missing
11. Phase 5A-5C: FilmGrain color grain missing
12. Phase 8A-8B: Duocylinder/Tesseract double scale
13. Phase 11A-11B: WireframeZoo normalization
14. Phase 9B: StrangeAttractor projection method
15. Phase 7B: dot_scale parameter

**P3 — Completeness:**
16. Phase 2C: has_enabled_effects param[0] check
17. Phase 3B-3D: EffectContext chain, midChainTap
18. Phase 6A: DitherFX Bayer table
19. Phase 12: Missing 14 effects (ongoing)

---

## Files Changed (Summary)

| File | Changes |
|------|---------|
| `shaders/compositor_blend.wgsl` | Fix Normal, Screen, Overlay, Exclusion, ColorDodge blend modes |
| `layer_compositor.rs` | Add group compositing, fix single-clip layer effects, fix has_enabled_effects |
| `effect.rs` | Add cleanup_all_owners to StatefulEffect, add chain to EffectContext |
| `effect_chain.rs` | Add midChainTap support |
| `effects/simple_blit_helper.rs` | Add should_skip_default helper |
| `effects/strobe.rs` | Add NoteRates lookup table |
| `effects/film_grain.rs` | Add color_grain uniform field |
| `effects/shaders/fx_film_grain.wgsl` | Add color grain noise, fix hash |
| `generators/generator_math.rs` | Fix project_4d two-stage projection |
| `generators/line_pipeline.rs` | Add dot_scale parameter |
| `generators/duocylinder.rs` | Remove double scale |
| `generators/tesseract.rs` | Remove double scale |
| `generators/strange_attractor.rs` | Fix 9 ODE constants, fix projection, add warmup |
| `generators/fluid_simulation.rs` | Fix texture formats, fix particle cap |
| `generators/fluid_simulation_3d.rs` | Fix particle cap |
| `generators/mycelium.rs` | Fix trail texture format |
| `generators/wireframe_zoo.rs` | Normalize vertices, fix projected_z |
