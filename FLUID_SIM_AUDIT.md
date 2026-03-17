# Fluid Simulation Audit: Unity vs Rust

**Date**: 2026-03-17
**Scope**: Complete pipeline comparison — initialization through final output
**Source of truth**: Unity codebase at `/Users/peterkiemann/MANIFOLD - Render Engine/`

---

## Executive Summary

The **2D fluid simulation** is a faithful port with one significant limitation (particle count cap).
The **3D fluid simulation** has **4 catastrophic bugs**, **9 major issues**, and **5 moderate issues** that collectively make it unrecognizable compared to Unity. The core simulation loop — gradient computation, integration model, particle lifecycle, and density feedback — are all structurally different.

---

## 2D Fluid Simulation (FluidSimulationGenerator)

### Verdict: Faithful port with minor differences

The 2D pipeline matches Unity line-for-line in all critical areas:
- Scatter (splat + resolve) pipeline: identical
- Blur pipeline (downsample → H+V Gaussian → gradient+rotate → H+V Gaussian): identical
- Particle integration shader: identical (verified line-by-line)
- Display tone mapping (Extended Reinhard): identical
- Snap state machine (5 modes, exponential envelope): identical
- Color injection system (4 zones, 120 frames, palette lookup): identical
- Energy scaling, area normalization, density-adaptive noise: all identical

### Differences Found

| # | Issue | Unity | Rust | Visual Impact |
|---|-------|-------|------|---------------|
| 1 | **Max particle count** | 8,000,000 | 2,000,000 | **HIGH** if user sets >2M. Quarter density = sparser, more granular appearance |
| 2 | Texture formats | `RFloat` (density), `RGFloat` (vector) — 32-bit | `Rgba16Float` everywhere — 16-bit | Minor precision differences, 4x bandwidth waste on density |
| 3 | Blur radius rounding | `Mathf.RoundToInt()` → integer | Float passed to shader, truncated by `i32()` | Off-by-one in blur radius occasionally |
| 4 | Redundant buffer clear | Self-clear only (resolve kernel) | `encoder.clear_buffer()` + self-clear | No visual impact (just redundant) |

### Recommendation

Raise the particle cap to 8M (or at least 4M if Metal buffer limits are a concern — 8M × 48 bytes = 384MB, which exceeds Metal's 128MB `max_storage_buffer_binding_size` on some GPUs). Consider `Rgba32Float` or `R32Float` / `Rg32Float` for density and vector field textures to match Unity's precision.

---

## 3D Fluid Simulation (FluidSimulation3DGenerator)

### Verdict: Structurally broken — requires line-by-line correction

### CATASTROPHIC Issues (each independently destroys the simulation)

#### 1. Gradient Magnitude is ~128x Too Strong

**Unity** `FluidGradientCurl3D.compute:43`:
```hlsl
float3 gradient = float3(dx, dy, dz) * 0.5;  // central difference scale
```

**Rust** `fluid_gradient_curl_3d.wgsl:40`:
```wgsl
let texel = 1.0 / f32(vr);
let gradient = vec3<f32>(dR - dL, dU - dD, dF - dB) / (2.0 * texel);
```

At `vol_res=128`: Unity gradient = `(dR-dL) * 0.5`. Rust gradient = `(dR-dL) * 64.0`.
Both then multiply by `flow * 500.0 * sin/cos(angle)`. Forces are **128x too strong**.

**Fix**: Replace `/ (2.0 * texel)` with `* 0.5`.

---

#### 2. Particles Die — Unity Particles Are Permanent

**Unity** `FluidSimulation3DSimulate.compute`:
```hlsl
p.position = newPos;
// Life is NEVER decremented — particles are permanent (life=1.0 forever)
p.color = float4(0.005, 0.005, 0.005, 1.0);
```

**Rust** `fluid_simulate_3d.wgsl:264-265`:
```wgsl
p.life -= params.dt * 0.1;
p.age += params.dt;
```

At 60fps (dt=0.016), particles die in 3-6 seconds. Dead particles are skipped by scatter
(`if p.life <= 0.0 return`), so visible particle count decays continuously.
Coherent fluid structures dissolve before they can form.

**Fix**: Remove `p.life -= params.dt * 0.1;` and `p.age += params.dt;`. Set `p.life = 1.0` on respawn (not 0.5+random).

---

#### 3. Density Capping Formula is Wrong

**Unity** `FluidSimulation3DSimulate.compute:140`:
```hlsl
float cappedDensity = localDensity / (1.0 + localDensity); // soft clamp: 0→0, ∞→1
```

**Rust** `fluid_simulate_3d.wgsl:116`:
```wgsl
let capped_density = min(density_val, 5.0);
```

Unity's sigmoid asymptotes to 1.0. Rust's hard clamp allows values up to 5.0 — making all
density-dependent effects (turbulence, diffusion, respawn) **up to 5x too strong** in dense regions.

**Fix**: Change to `density_val / (1.0 + density_val)`.

---

#### 4. Integration Multiplies by dt — Unity Does Not

**Unity** `FluidSimulation3DSimulate.compute:214`:
```hlsl
float3 newPos = pos + force * _Speed;
```

**Rust** `fluid_simulate_3d.wgsl:188,258`:
```wgsl
var total_force = (field_force + turb_force + diffusion) * params.speed;
// ...
p.position = clamp(p.position + total_force * params.dt, ...);
```

Rust multiplies force by `speed` AND by `dt`. At 60fps, effective step is **~62x smaller** than Unity.
This partially counteracts bug #1 but makes the simulation frame-rate-dependent.

**Fix**: Remove `* params.dt` from integration. Use `p.position + total_force` directly (force already includes `* speed`).

---

### MAJOR Issues (clearly visible differences)

#### 5. Noise Spatial Frequency is 4x Too High

**Unity**: `noisePos = pos * 2.0`
**Rust**: `noise_scale = 8.0`

4x higher frequency = chaotic micro-scale motion instead of large-scale coherent swirling.

**Fix**: Change `noise_scale` to `2.0`.

---

#### 6. Different Simplex Noise Implementation

The 2D Rust fluid correctly ports `SimplexNoise2D` from `ParticleCommon.cginc` (8-direction gradient table, `*35.0 + 0.5` scaling, output in [0,1]).

The 3D Rust fluid uses a **completely different** simplex noise: hash-based random gradients, `*70.0` scaling, different output range.

**Fix**: Reuse the correct `SimplexNoise2D` implementation from the 2D fluid shader (`fluid_simulate.wgsl`).

---

#### 7. Noise Time Offset Uses frame_count Instead of Time

**Unity**: `noiseTime = _Time2 * 0.1` (wall time)
**Rust**: `time_offset = f32(params.frame_count) * 0.01` (frame count)

Frame-based noise evolution is frame-rate-dependent. At 60fps: `frame*0.01 ≈ time*0.6`, **6x faster** than Unity's `time*0.1`.

**Fix**: Pass `ctx.time * 0.1` as noise time offset. Use time-based offsets `+100.0` and `+200.0` (not `+17.0, +31.0, +43.0, +59.0`).

---

#### 8. Container Boundary Margin is 5x Too Tight

**Unity**: `margin = 0.1`
**Rust**: `margin = 0.02`

Particles pile up at container walls. Unity's wider margin provides smoother boundary repulsion.

**Fix**: Change margin to `0.1`.

---

#### 9. Container Boundary Force Formula Differs

**Unity**: Quadratic ramp `t = saturate((d + margin) / margin)`, force `n * t * t * 0.15`
**Rust**: Smoothstep with coefficient `0.1`

**Fix**: Use `let t = clamp((sdf + margin) / margin, 0.0, 1.0); total_force -= normal * t * t * 0.15;`

---

#### 10. Flatten Scale is 20x Different and Applied at Wrong Stage

**Unity** (post-integration position correction):
```hlsl
newPos -= _CamFwd * depthFromCenter * _Flatten * 0.1;
```

**Rust** (pre-integration force):
```wgsl
total_force -= cam_fwd * depth * params.flatten * 2.0;
```

20x magnitude difference (0.1 vs 2.0) AND applied at different pipeline stages.

**Fix**: Apply as position correction after integration with multiplier `0.1`. Match Unity exactly.

---

#### 11. 3D Blur Radius Scaling is ~5x Too Large

**Unity**: `resScale = volumeRes / 640.0` → at 128: `scaledRadius = round(20 * 0.2) = 4`
**Rust**: `density_radius = blur_radius.min(vol_res/4).max(1.0)` → at 128: `density_radius = 20`

Over-smoothing washes out all fine density structure.

**Fix**: Scale blur radius by `vol_res as f32 / 640.0`, matching Unity exactly.

---

#### 12. Projected Display Gets Extra 2D Blur (Unity Has None)

**Unity**: Projected scatter → display density → **directly to tone mapping**
**Rust**: Projected scatter → display density → **2D Gaussian blur H+V** → tone mapping

This softens the crisp per-particle display that is the whole point of the projected scatter technique.

**Fix**: Remove the 2D blur pass on projected density (Rust pass 6c).

---

#### 13. Orthographic Projection Doesn't Wrap Toroidally

**Unity**:
```hlsl
screenUV.x = frac(dot(worldPos, _CamRight) + 0.5);
screenUV.y = frac(dot(worldPos, _CamUp) + 0.5);
return true;  // never cull — toroidal display
```

**Rust**:
```wgsl
screen_uv = vec2(dot(world_pos, cam_right) + 0.5, dot(world_pos, cam_up) + 0.5);
// Then culled if outside [0,1]
```

Particles near volume boundaries don't wrap on screen → visible edge discontinuities.

**Fix**: Add `fract()` wrapping in ortho mode and skip the [0,1] cull check.

---

### MODERATE Issues

#### 14. Respawn Position Model Differs

| Scenario | Unity | Rust |
|----------|-------|------|
| Toroidal | Random volume point | Random cube FACE (edges only!) |
| Container | Rejection-sampled inside SDF (8 attempts) | Random volume point (no rejection) |
| Flatten | Applied to respawn position | NOT applied to respawn |

**Fix**: Toroidal → random volume point. Container → rejection sampling. Apply flatten to respawn.

---

#### 15. Camera Tilt Not Converted

**Unity**: `tiltAngle = camTilt * PI * 0.5` (param 0.3 → 0.47 rad)
**Rust simulate shader**: Uses `params.cam_tilt` raw (0.3 → 0.3 rad)

**Fix**: Convert in host code: `cam_tilt * PI * 0.5`.

---

#### 16. Snap/Trigger System is Stub-Level

Unity has 5 snap modes (noise blast, rotation flip, slope flip, pattern reset, color inject) with exponential envelope decay and trigger-count-based activation.

Rust only supports pattern seed on snap parameter edge. No envelope, no modal behavior.

**Fix**: Port the full snap state machine from Unity's `FluidSimulation3DGenerator.cs`.

---

#### 17. Color Injection System is Missing

Unity 3D has complete color injection: 3D tetrahedron injection points, color palettes, projected color scatter, and color display path.

Rust 3D is always mono-only.

**Fix**: Port the injection state machine, shader code, and projected color scatter from Unity.

---

#### 18. `_UseVectorField` Fallback Missing

Unity has Phase 1 fallback (`_UseVectorField=0`) for noise-only motion when the volume pipeline isn't ready. Rust always samples the vector field, even when uninitialized.

**Fix**: Add a use_vector_field flag; use noise-only when field hasn't been computed yet.

---

#### 19. 3D Scatter Energy Includes Splat Size (Unity Doesn't)

Unity 3D scatter: `energy = 0.005 * (1M / count) * resScale` — no splat size term
Rust 3D scatter: includes `* (particle_size / 3.0)` — wrong

**Fix**: Remove `* (particle_size / 3.0)` from 3D volume scatter energy. Only apply it to projected 2D scatter.

---

## Fix Priority Order

For the 3D simulation, fix in this order (each fix makes the next one's effect visible):

1. **Gradient magnitude** (#1) — without this, all other forces are noise
2. **Remove dt from integration** (#4) — restore correct step size
3. **Density capping** (#3) — restore bounded density-dependent effects
4. **Remove particle life decrement** (#2) — restore permanent particles
5. **Noise scale + implementation** (#5, #6, #7) — restore correct advection
6. **Blur radius scaling** (#11) — restore fine density structure
7. **Remove projected display blur** (#12) — restore crisp output
8. **Ortho projection wrapping** (#13) — fix toroidal display
9. **Container boundary** (#8, #9) — fix containment
10. **Flatten** (#10) — fix 2D compression
11. **Remaining moderate issues** (#14-19)
