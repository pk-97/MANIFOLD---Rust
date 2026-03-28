# Stability Audit — Remaining 15 Effect Files

**Date:** 2026-03-28
**Scope:** 15 effect files not covered in the initial stability audit
**Auditor:** Claude Opus 4.6
**Verdict:** No critical issues. All 15 files are well-structured for live performance stability.

---

## Summary

| Severity | Count |
|----------|-------|
| CRITICAL | 0 |
| WARNING | 3 |
| INFO | 6 |
| VERIFIED SAFE | 22 |

---

## Per-File Findings

### 1. `crates/manifold-renderer/src/effects/infrared.rs`

**VERIFIED SAFE** — Per-owner state: None. Stateless single-pass. `infrared.rs:27-29`

**WARNING** — Division by zero in texel_size calculation. If `ctx.width` or `ctx.height` is 0, `1.0 / width` and `1.0 / height` produce `f32::INFINITY`. In practice this cannot happen because the compositor creates textures at project resolution (minimum 1x1), and the width/height come from `self.main.width()` / `self.main.height()` which are set at compositor construction time (`layer_compositor.rs:363`). However, no explicit guard exists at the EffectContext level.
`infrared.rs:71-72`
```rust
texel_size_x: 1.0 / width,
texel_size_y: 1.0 / height,
```

**VERIFIED SAFE** — Uniform alignment: `InfraredUniforms` is 48 bytes (3 x 16). `#[repr(C)]` + `bytemuck::Pod`. `infrared.rs:7-22`

**VERIFIED SAFE** — No per-frame allocations. No `.unwrap()` that could panic. All param access uses `.get().copied().unwrap_or(default)`. `infrared.rs:60-76`

---

### 2. `crates/manifold-renderer/src/effects/chromatic_aberration.rs`

**VERIFIED SAFE** — Per-owner state: None. Stateless single-pass. `chromatic_aberration.rs:20-22`

**INFO** — Float-to-integer cast with `.round()`: `mode` parameter uses `.round() as u32` with `.min(1)` upper clamp. NaN would produce 0 via Rust's saturating cast (safe, maps to Radial mode). Negative values would also saturate to 0.
`chromatic_aberration.rs:53`
```rust
let mode = p.get(2).copied().unwrap_or(0.0).round() as u32;
```

**VERIFIED SAFE** — Uniform alignment: `ChromaticAberrationUniforms` is 32 bytes (2 x 16). `chromatic_aberration.rs:8-16`

**VERIFIED SAFE** — No per-frame allocations. `chromatic_aberration.rs:41-73`

---

### 3. `crates/manifold-renderer/src/effects/dither.rs`

**VERIFIED SAFE** — Per-owner state: None. Stateless single-pass. `dither.rs:18-19`

**INFO** — Float-to-integer cast with `.round()`: `algorithm` uses `.round() as u32` with `.min(5)` upper clamp. NaN saturates to 0 (Bayer, the first algorithm). Safe.
`dither.rs:50`
```rust
algorithm: (p.get(1).copied().unwrap_or(0.0).round() as u32).min(5),
```

**VERIFIED SAFE** — Uniform alignment: `DitherUniforms` is 16 bytes (1 x 16). `dither.rs:8-14`

**VERIFIED SAFE** — No per-frame allocations. `dither.rs:39-62`

---

### 4. `crates/manifold-renderer/src/effects/edge_detect.rs`

**VERIFIED SAFE** — Per-owner state: None. Stateless. Holds 3 specialized pipelines (created once). `edge_detect.rs:24-30`

**WARNING** — Division by zero in texel_size calculation (same pattern as infrared). `ctx.width` and `ctx.height` are u32 from compositor dimensions, effectively guaranteed non-zero at runtime, but no explicit guard.
`edge_detect.rs:78-79`
```rust
texel_size_x: 1.0 / ctx.width as f32,
texel_size_y: 1.0 / ctx.height as f32,
```

**INFO** — Float-to-integer cast without explicit clamp on uniform: `mode_raw.round() as u32` is not clamped before writing to the uniform struct. However, the pipeline selection at line 83-87 uses a match with wildcard default (`_ => &self.pipeline_sobel`), so out-of-range values safely fall through to Sobel. The shader receives the mode via function constants (specialized pipeline), not from the uniform value, so the unclamped uniform is cosmetically imperfect but functionally safe.
`edge_detect.rs:77`
```rust
mode: mode_raw.round() as u32,
```

**VERIFIED SAFE** — Legacy param indexing: `p[3]` is guarded by `p.len() >= 4`. `edge_detect.rs:69-73`

**VERIFIED SAFE** — Uniform alignment: `EdgeDetectUniforms` is 32 bytes (2 x 16). `edge_detect.rs:8-16`

**VERIFIED SAFE** — No per-frame allocations. `edge_detect.rs:58-95`

---

### 5. `crates/manifold-renderer/src/effects/edge_stretch.rs`

**VERIFIED SAFE** — Per-owner state: None. Stateless single-pass. `edge_stretch.rs:18-19`

**VERIFIED SAFE** — `source_width` is clamped to `[0.1, 0.9]`, preventing degenerate UV behavior. `edge_stretch.rs:51`

**VERIFIED SAFE** — Float-to-integer cast: `mode` uses `.round() as u32` with `.min(2)`. `edge_stretch.rs:52`

**VERIFIED SAFE** — Uniform alignment: `EdgeStretchUniforms` is 16 bytes (1 x 16). `edge_stretch.rs:8-14`

**VERIFIED SAFE** — No per-frame allocations. `edge_stretch.rs:39-64`

---

### 6. `crates/manifold-renderer/src/effects/color_grade.rs`

**VERIFIED SAFE** — Per-owner state: None. Stateless single-pass. `color_grade.rs:33-35`

**VERIFIED SAFE** — `should_skip` uses epsilon comparison (`EPSILON = 0.001`) matching Unity's `ColorGradeFX.cs:11`. `color_grade.rs:55-71`

**VERIFIED SAFE** — Uniform alignment: `ColorGradeUniforms` is 48 bytes (3 x 16). 9 params + 3 padding fields. `color_grade.rs:14-29`

**VERIFIED SAFE** — No per-frame allocations. No float-to-integer casts. `color_grade.rs:73-108`

---

### 7. `crates/manifold-renderer/src/effects/kaleidoscope.rs`

**VERIFIED SAFE** — Per-owner state: None. Stateless single-pass. `kaleidoscope.rs:18-19`

**INFO** — `segments` clamped with `.max(2.0)` to prevent degenerate geometry (division by zero in polar coordinate math in the shader). Good defensive practice.
`kaleidoscope.rs:50`
```rust
segments: p.get(1).copied().unwrap_or(6.0).max(2.0),
```

**VERIFIED SAFE** — Uniform alignment: `KaleidoscopeUniforms` is 16 bytes (1 x 16). `kaleidoscope.rs:8-14`

**VERIFIED SAFE** — No per-frame allocations. `kaleidoscope.rs:39-62`

---

### 8. `crates/manifold-renderer/src/effects/mirror.rs`

**VERIFIED SAFE** — Per-owner state: None. Stateless single-pass. `mirror.rs:17-19`

**VERIFIED SAFE** — Float-to-integer cast: `mode` uses `.round() as u32` with `.min(2)`. `mirror.rs:48`

**VERIFIED SAFE** — Uniform alignment: `MirrorUniforms` is 16 bytes (1 x 16). `mirror.rs:8-13`

**VERIFIED SAFE** — No per-frame allocations. `mirror.rs:38-63`

---

### 9. `crates/manifold-renderer/src/effects/quad_mirror.rs`

**VERIFIED SAFE** — Per-owner state: None. Stateless single-pass. `quad_mirror.rs:16-17`

**VERIFIED SAFE** — Simplest possible effect: single `amount` parameter, no casts, no division. `quad_mirror.rs:37-60`

**VERIFIED SAFE** — Uniform alignment: `QuadMirrorUniforms` is 16 bytes (1 x 16). `quad_mirror.rs:8-12`

---

### 10. `crates/manifold-renderer/src/effects/strobe.rs`

**VERIFIED SAFE** — Per-owner state: None. Stateless single-pass. `strobe.rs:22-24`

**INFO** — Array index with bounds clamp: `rate_idx.min(NOTE_RATES.len() - 1)` prevents out-of-bounds panic on the `NOTE_RATES` lookup. `NOTE_RATES` has 9 elements (constant, never empty), so `len() - 1` cannot underflow.
`strobe.rs:53-54`
```rust
let rate_idx = p.get(1).copied().unwrap_or(6.0).round().max(0.0) as usize;
let rate = NOTE_RATES[rate_idx.min(NOTE_RATES.len() - 1)];
```

**VERIFIED SAFE** — Float-to-integer cast: `mode` uses `.round() as u32` with `.min(2)`. `strobe.rs:58`

**VERIFIED SAFE** — Uniform alignment: `StrobeUniforms` is 16 bytes (1 x 16). `strobe.rs:8-14`

**VERIFIED SAFE** — No per-frame allocations. `strobe.rs:43-69`

---

### 11. `crates/manifold-renderer/src/effects/transform.rs`

**VERIFIED SAFE** — Per-owner state: None. Stateless single-pass. `transform.rs:49-51`

**WARNING** — Division by zero in aspect_ratio calculation: `ctx.width as f32 / ctx.height as f32`. If `ctx.height` were 0, this would produce `f32::INFINITY`. Same mitigation as infrared/edge_detect: compositor guarantees non-zero dimensions at construction time, but no explicit guard at the EffectContext level.
`transform.rs:107`
```rust
let aspect_ratio = ctx.width as f32 / ctx.height as f32;
```

**VERIFIED SAFE** — `should_skip` uses `approximately()` with `1e-5` epsilon, matching Unity's `Mathf.Approximately`. `transform.rs:28-30, 72-83`

**VERIFIED SAFE** — Clip-level passthrough: correctly sends identity uniforms when `ctx.is_clip_level` is true, matching Unity's `TransformFX.cs:18`. `transform.rs:95-105`

**VERIFIED SAFE** — Uniform alignment: `TransformUniforms` is 32 bytes (2 x 16). `transform.rs:32-43`

**VERIFIED SAFE** — No per-frame allocations. `transform.rs:85-127`

---

### 12. `crates/manifold-renderer/src/effects/invert_colors.rs`

**VERIFIED SAFE** — Per-owner state: None. Stateless single-pass. `invert_colors.rs:16-17`

**VERIFIED SAFE** — Simplest effect in the codebase: single `intensity` parameter, no casts, no division, no conditional logic. `invert_colors.rs:37-58`

**VERIFIED SAFE** — Uniform alignment: `InvertUniforms` is 16 bytes (1 x 16). `invert_colors.rs:8-12`

---

### 13. `crates/manifold-renderer/src/effects/voronoi_prism.rs`

**VERIFIED SAFE** — Per-owner state: None. Stateless single-pass. `voronoi_prism.rs:23-25`

**INFO** — `aspect_ratio` computed as `ctx.width as f32 / ctx.height as f32`. Same zero-height concern as transform.rs, same mitigation (compositor guarantees non-zero).
`voronoi_prism.rs:57`

**VERIFIED SAFE** — `edge_stretch_width` comes from `EffectContext`, precomputed by `effect_chain.rs:142-147` using `find_chain_param()` with fallback `0.5625`. No risk of uninitialized or invalid values. `voronoi_prism.rs:58`

**VERIFIED SAFE** — Uniform alignment: `VoronoiPrismUniforms` is 32 bytes (2 x 16). `voronoi_prism.rs:8-18`

**VERIFIED SAFE** — No per-frame allocations. `voronoi_prism.rs:44-71`

---

### 14. `crates/manifold-renderer/src/effects/fragment_blit_helper.rs`

**VERIFIED SAFE** — No state beyond pipeline and sampler (both created once at init). `fragment_blit_helper.rs:19-22`

**VERIFIED SAFE** — No per-frame allocations. Binding array is stack-allocated (3 elements). `fragment_blit_helper.rs:62-79, 94-114`

**VERIFIED SAFE** — No `.unwrap()` calls. No division. No float-to-integer casts. `fragment_blit_helper.rs:1-116`

**VERIFIED SAFE** — `dispatch_with` method cleanly supports function-constant-specialized pipelines (used by EdgeDetect). `fragment_blit_helper.rs:48-80`

---

### 15. `crates/manifold-renderer/src/effects/compute_dual_blit_helper.rs`

**VERIFIED SAFE** — No state beyond pipeline and sampler (both created once at init). `compute_dual_blit_helper.rs:14-17`

**VERIFIED SAFE** — No per-frame allocations. Binding array is stack-allocated (5 elements). `compute_dual_blit_helper.rs:63-89, 120-146`

**VERIFIED SAFE** — Workgroup dispatch uses `div_ceil(16)` which correctly rounds up, preventing missed edge pixels. `compute_dual_blit_helper.rs:87, 144`

**VERIFIED SAFE** — `dispatch_a_only` and `dispatch_a_only_with` correctly bind source to both binding slots (1 and 2), documented as intentional for single-source modes. `compute_dual_blit_helper.rs:33-47, 95-106`

**VERIFIED SAFE** — No `.unwrap()` calls. No division. No float-to-integer casts. `compute_dual_blit_helper.rs:1-148`

---

## Cross-Cutting Findings

### Division by Zero — EffectContext Width/Height (WARNING)

Three files compute `1.0 / ctx.width as f32` or `ctx.width as f32 / ctx.height as f32`:
- `infrared.rs:71-72` — `1.0 / width`, `1.0 / height`
- `edge_detect.rs:78-79` — `1.0 / ctx.width as f32`, `1.0 / ctx.height as f32`
- `transform.rs:107` — `ctx.width as f32 / ctx.height as f32`
- `voronoi_prism.rs:57` — `ctx.width as f32 / ctx.height as f32`

**Mitigation:** `ctx.width` and `ctx.height` are sourced from `self.main.width()` / `self.main.height()` in `layer_compositor.rs:446-447`, which are set at compositor construction (`layer_compositor.rs:363`) from project resolution. The project resolution is always >= 1x1. Additionally, `f32::INFINITY` from a zero denominator would not cause a crash -- it would produce visual artifacts but not a panic or UB.

**Risk:** Extremely low. Would require a fundamental initialization failure that would break many other systems first.

### Float-to-Integer Casts (VERIFIED SAFE)

All `as u32` casts from float params use `.round()` first, consistent with Unity's `Mathf.RoundToInt()`. Rust's saturating cast semantics (since 1.45) mean NaN -> 0 and overflow -> saturated value. All mode casts have upper-bound clamps via `.min(N)`.

Files with float-to-integer casts:
- `chromatic_aberration.rs:53` — `.round() as u32` + `.min(1)`
- `dither.rs:50` — `.round() as u32` + `.min(5)`
- `edge_detect.rs:77` — `.round() as u32` (no clamp on uniform, but pipeline selection uses match wildcard)
- `edge_stretch.rs:52` — `.round() as u32` + `.min(2)`
- `mirror.rs:48` — `.round() as u32` + `.min(2)`
- `strobe.rs:53` — `.round().max(0.0) as usize` + `.min(len-1)` for array index
- `strobe.rs:58` — `.round() as u32` + `.min(2)`

### Uniform Alignment (VERIFIED SAFE)

All 13 uniform structs across the 15 files are:
- `#[repr(C)]` annotated
- `bytemuck::Pod` + `bytemuck::Zeroable` derived
- 16-byte aligned (sizes: 16, 32, or 48 bytes)
- Padded with explicit `_pad` fields

### Per-Frame Allocations (VERIFIED SAFE)

None of the 15 audited files contain `Vec::new()`, `String::new()`, `format!()`, `.collect()`, `.push()`, or `.extend()` on the hot path. All allocations occur at initialization time only.

### Per-Owner State / Cleanup (VERIFIED SAFE)

None of the 15 audited files maintain per-owner state (`AHashMap` or similar). All are stateless single-pass effects (or stateless helpers). No cleanup paths needed.

### Unwrap Safety (VERIFIED SAFE)

No bare `.unwrap()` calls in any of the 15 files. All parameter access uses the safe `.get(N).copied().unwrap_or(default)` pattern or `.first().copied().unwrap_or(default)`.

---

## Conclusion

These 15 effect files are among the most stable code in the codebase. They follow a consistent, minimal pattern:
1. Read params with safe defaults
2. Build a `#[repr(C)]` uniform struct (16-byte aligned, pod-safe)
3. Call `FragmentBlitHelper::dispatch()` or `ComputeDualBlitHelper::dispatch()`

The only theoretical risk is the division-by-zero path for texel sizes and aspect ratios, which is guarded by the compositor's initialization guarantees. No action is required for live performance stability.
