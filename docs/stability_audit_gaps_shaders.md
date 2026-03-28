# Stability Audit: Shader Files & fast_math Impact

Audited: 2026-03-28
Auditor: Claude Opus 4.6 (research-only, no code modifications)

---

## Task A: Shader-by-Shader Audit

### 1. `fluid_scatter_3d.wgsl`

File: `crates/manifold-renderer/src/generators/shaders/fluid_scatter_3d.wgsl`

4 entry points: `splat_3d`, `resolve_3d`, `splat_projected`, `resolve_display`.

#### 1.1 Loops

**VERIFIED SAFE** -- No `loop`, `while`, or `for` statements in any entry point.

#### 1.2 Division

**VERIFIED SAFE** -- The only division is the fixed-point resolve `f32(raw_val) / 4096.0` at lines 91 and 202. Constant divisor, cannot be zero.

Perspective projection at line 149: `dot(rel, cam_right) / (view_z * proj_params.aspect)` is guarded by `view_z <= 0.001` early return at line 145. The `aspect` uniform is computed as `width / height` on the Rust side. If height were 0 the Rust side would divide by zero first, but display dimensions are always positive (derived from render target dimensions).

`crates/manifold-renderer/src/generators/shaders/fluid_scatter_3d.wgsl:149`
`crates/manifold-renderer/src/generators/shaders/fluid_scatter_3d.wgsl:145`

#### 1.3 pow/log/sqrt/atan2

**VERIFIED SAFE** -- None present.

#### 1.4 Texture Bounds

**VERIFIED SAFE** -- All texture access is via `textureStore` with coordinates derived from bounds-checked values:
- `splat_3d` (line 53-57): coordinates modulo `vr`/`vd` via `% vr`/`% vd`, always in range.
- `resolve_3d` (line 83-85): early return if `id.x >= vr || id.y >= vr || id.z >= vd`.
- `splat_projected` (line 175-177): coordinates clamped via `min(u32(...), dim - 1u)`.
- `resolve_display` (line 196-197): early return if `id.x >= w || id.y >= h`.

#### 1.5 Boundary Thread Early Return

**VERIFIED SAFE** -- All 4 compute entry points have early returns for out-of-bounds global invocation IDs:
- `splat_3d` line 41: `if id.x >= splat_params.active_count { return; }`
- `resolve_3d` line 83: `if id.x >= vr || id.y >= vr || id.z >= vd { return; }`
- `splat_projected` line 157: `if id.x >= proj_params.active_count { return; }`
- `resolve_display` line 196: `if id.x >= w || id.y >= h { return; }`

#### 1.6 Feedback / NaN

**VERIFIED SAFE** -- No feedback reads. Accumulator is atomically cleared each frame (self-clearing pattern at lines 95 and 207).

#### 1.7 Workgroup Size

**WARNING** -- `resolve_3d` at line 79 uses `@workgroup_size(8, 8, 8)` = 512 total invocations. The project convention documented in CLAUDE.md states `max_compute_invocations_per_workgroup = 256` and recommends `@workgroup_size(4,4,4)` for 3D.

In practice, Apple Silicon (M1+) supports `maxTotalThreadsPerThreadgroup = 1024`, so 512 works on all current target hardware. However, this violates the project's own documented convention and could fail on future constrained GPU configurations or if the convention reflects actual naga/wgpu validation limits (though this pipeline runs on native Metal, bypassing wgpu).

`crates/manifold-renderer/src/generators/shaders/fluid_scatter_3d.wgsl:79`

Same issue in `fluid_gradient_curl_3d.wgsl:27` (also `@workgroup_size(8, 8, 8)`).

Other entry points are fine:
- `splat_3d` line 39: `@workgroup_size(256, 1, 1)` = 256 OK
- `splat_projected` line 155: `@workgroup_size(256, 1, 1)` = 256 OK
- `resolve_display` line 192: `@workgroup_size(16, 16, 1)` = 256 OK

#### 1.8 Workgroup Memory

**VERIFIED SAFE** -- No `var<workgroup>` declarations.

#### 1.9 Integer Overflow

**WARNING** -- Index calculation at line 58: `coord.z * vr * vr + coord.y * vr + coord.x`. With `vol_res_from_param()` returning at most 256 (see `fluid_simulation_3d.rs:74`), the maximum value is `255 * 256 * 256 + 255 * 256 + 255 = 16,777,215` which fits in u32. Safe for current param range.

However, if `vol_res` were ever increased beyond 1290 (1290^3 > u32::MAX), overflow would silently produce wrong indices and out-of-bounds buffer access. Currently safe because `vol_res_from_param` hard-codes values {64, 128, 256}.

`crates/manifold-renderer/src/generators/shaders/fluid_scatter_3d.wgsl:58`
`crates/manifold-renderer/src/generators/fluid_simulation_3d.rs:72-75`

#### 1.10 select() with NaN

**VERIFIED SAFE** -- No `select()` usage in this shader.

---

### 2. `mycelium_diffuse.wgsl`

File: `crates/manifold-renderer/src/generators/shaders/mycelium_diffuse.wgsl`

2 entry points: `vs_main` (vertex), `fs_main` (fragment).

#### 2.1 Loops

**VERIFIED SAFE** -- No loops. The 3x3 box blur is unrolled (9 texture samples at lines 37-45).

#### 2.2 Division

**VERIFIED SAFE** -- Single division at line 47: `sum / 9.0`. Constant divisor.

#### 2.3 pow/log/sqrt/atan2

**VERIFIED SAFE** -- None present.

#### 2.4 Texture Bounds

**VERIFIED SAFE** -- All texture access uses `textureSample` with sampler clamping. Fragment shader samples at UV offsets within one texel of the fragment position -- sampler address mode handles edge clamping.

#### 2.5 Boundary Threads

**N/A** -- Fragment shader, not compute. Rasterizer handles coverage.

#### 2.6 Feedback / NaN

**INFO** -- This shader reads from a trail texture that is written by the mycelium agent update pass. The trail texture is read, blurred, decayed, and written back each frame. This is a feedback-like pattern. However, the `max(0.0, ...)` at line 48 and the multiplicative `decay` (range 0-1) ensure values cannot grow unboundedly. The `sub_decay` term provides a floor drain. Over time, any stale values decay to 0.

If NaN enters the trail texture (e.g., from a corrupt agent position), `textureSample` would return NaN, the 9-way sum would be NaN, and `max(0.0, NaN)` returns NaN (not 0.0) under IEEE 754. With `fast_math`, NaN may be flushed to 0 (see Task B). Without fast_math, NaN would persist in the trail texture indefinitely.

Risk is low because the agent update pass that writes trail values uses only additions and coordinate lookups (no division), but this is outside the scope of these 5 shaders.

`crates/manifold-renderer/src/generators/shaders/mycelium_diffuse.wgsl:48`

#### 2.7 Workgroup Size

**N/A** -- Fragment shader.

#### 2.8 Workgroup Memory

**N/A** -- Fragment shader.

#### 2.9 Integer Overflow

**VERIFIED SAFE** -- No integer index calculations.

#### 2.10 select() with NaN

**VERIFIED SAFE** -- No `select()` usage.

---

### 3. `fx_blob_tracking.wgsl`

File: `crates/manifold-renderer/src/effects/shaders/fx_blob_tracking.wgsl`

2 entry points: `vs_main` (vertex), `fs_main` (fragment).

#### 3.1 Loops

**VERIFIED SAFE** -- Three `for` loops, all with compile-time constant upper bounds:
- Line 191: `for (var b = 0; b < 16; b++)` -- max 16 iterations, bounded by MAX_BLOBS constant. Early break at line 192 when `b >= uniforms.blob_count`.
- Line 243: `for (var t = 0; t < 4; t++)` -- exactly 4 iterations, tick marks.
- Line 254: `for (var c = 0; c < 16; c++)` -- max 16 iterations, bounded by MAX_BLOBS. Early break at line 255 when `c >= uniforms.connection_count`.

#### 3.2 Division

**WARNING** -- Four divisions by `pixel_size` parameter at lines 106, 113, 128, 143 in helper functions `draw_char`, `draw_3_digits`, `draw_hex_label`, `draw_coord_label`: `let local = (p - origin) / pixel_size;`

`pixel_size` is computed in the fragment shader at line 183 as `digit_size = px_u * 2.0`, where `px_u = uniforms.texel_size.x = 1.0 / resolution.x`. For any valid render target (resolution >= 1 pixel), `px_u > 0` and `digit_size > 0`. However, if `resolution.x` were 0 (impossible in practice since texture dimensions are always positive), `texel_size.x` would be Inf and `digit_size` would be Inf, making the division result 0 (not NaN).

Also at line 268: `t_val * len / (px_u * 12.0)` -- same analysis, `px_u > 0` for valid targets.

The `line_seg` function has a guard at line 51: `if len_sq < 0.000001 { return 0.0; }` which prevents divide-by-near-zero at line 52.

`crates/manifold-renderer/src/effects/shaders/fx_blob_tracking.wgsl:106`
`crates/manifold-renderer/src/effects/shaders/fx_blob_tracking.wgsl:51-52`

**VERIFIED SAFE** in practice -- all divisions guarded or operating on known-positive values derived from render target dimensions.

#### 3.3 pow/log/sqrt/atan2

**VERIFIED SAFE** -- None present.

#### 3.4 Texture Bounds

**VERIFIED SAFE** -- All texture access via `textureSample`/`textureSampleLevel` with sampler. The font atlas sampling at line 101 uses `textureSampleLevel(font_tex, point_sampler, atlas_uv, 0.0)` where `atlas_uv` is constructed from floored integer coordinates. The `sample_glyph` function at line 91 has an explicit bounds check `if local_px.x < 0.0 || local_px.x >= 5.0 || local_px.y < 0.0 || local_px.y >= 7.0 { return 0.0; }`.

#### 3.5 Boundary Threads

**N/A** -- Fragment shader.

#### 3.6 Feedback / NaN

**VERIFIED SAFE** -- No feedback reads. Pure overlay effect, reads source texture and font atlas.

#### 3.7 Workgroup Size / Memory

**N/A** -- Fragment shader.

#### 3.8 Integer Overflow

**VERIFIED SAFE** -- No integer index calculations beyond loop counters with small constant bounds.

#### 3.9 select() with NaN

**VERIFIED SAFE** -- Single `select` at line 247: `select(px_u * 6.0, px_u * 12.0, (u32(t) % 2u) == 0u)`. The condition is a pure integer comparison, and both branches produce finite values. No NaN risk.

---

### 4. `fx_wireframe_depth.wgsl`

File: `crates/manifold-renderer/src/effects/shaders/fx_wireframe_depth.wgsl`

16 entry points (passes 0-14 + vertex shader). This is the most complex shader in the audit.

#### 4.1 Loops

**VERIFIED SAFE** -- No `for`, `loop`, or `while` statements. All neighbor sampling is fully unrolled.

#### 4.2 Division

**INFO** -- Multiple divisions, all either guarded or safe by construction:

1. Line 233: `let persp = 1.0 / (1.0 + z * 1.6)` -- denominator minimum is 1.0 (when z=0), since depth is clamped 0..1 and `depth_scale` is a user parameter. If `depth_scale` were negative and large enough to make `z * 1.6 < -1.0`, the denominator could cross zero. However, `depth_scale` is a UI parameter in range [0, 2] (default 1.0) -- verified from the effect definition. Safe.

2. Line 242: Same pattern: `let persp_raw = 1.0 / (1.0 + z_raw * 1.6)` -- same analysis.

3. Line 395: `let denom = ix * ix + iy * iy + 0.0008;` and line 397: `it / denom` -- the epsilon 0.0008 ensures minimum denominator. Safe.

4. Line 472: `delta * (max_step / max(d_len, 1e-5))` -- guarded by `max(d_len, 1e-5)`.

5. Line 477-478: `flow_uv.x / max(texel.x, 1e-5)` -- guarded.

6. Line 568-569: `(c * keep_w + lap * smooth_w + rigid_center * rigid_w) / w_sum` where `w_sum = max(keep_w + smooth_w + rigid_w, 1e-4)` -- guarded.

7. Line 616: `(flow_r - flow_l) / max(2.0 * step_uv.x, 1e-5)` -- guarded.

`crates/manifold-renderer/src/effects/shaders/fx_wireframe_depth.wgsl:233`
`crates/manifold-renderer/src/effects/shaders/fx_wireframe_depth.wgsl:395`

**VERIFIED SAFE** -- All divisions have epsilon guards or provably positive denominators.

#### 4.3 pow/log/sqrt/atan2

**VERIFIED SAFE** --

- `sqrt` at lines 110, 443, 664: all arguments are sums of squares (x^2 + y^2), always >= 0.
- `atan2` at lines 544-545: `atan2(p_ex.y, p_ex.x)` and `atan2(c_ex.y, c_ex.x)`. When both arguments are 0, WGSL/Metal `atan2(0,0)` returns 0. The resulting rotation matrix becomes identity (cos(0)=1, sin(0)=0). No NaN risk.

`crates/manifold-renderer/src/effects/shaders/fx_wireframe_depth.wgsl:544-545`

#### 4.4 Texture Bounds

**VERIFIED SAFE** -- All texture access via `textureSample` with sampler. The sampler handles edge clamping for UV offsets that extend beyond [0,1].

#### 4.5 Boundary Threads

**N/A** -- All entry points are fragment shaders.

#### 4.6 Feedback / NaN

**INFO** -- This effect has extensive temporal feedback through multiple persistent textures (prev_analysis, prev_depth, history, prev_mesh_coord, prev_surface_cache). Values are propagated frame-to-frame via:
- Temporal smoothing (mix with previous values)
- Flow-based advection (sampling previous coordinate maps at warped UVs)
- Surface cache persistence

All temporal values are explicitly clamped: `clamp(trust_out, 0.0, 1.0)` (line 503), `clamp(coord, vec2(0), vec2(1))` (line 581), `clamp(age, 0.0, 1.0)` (line 751). These clamps prevent unbounded growth. However, under fast_math, if an intermediate produces NaN, `clamp(NaN, 0.0, 1.0)` behavior is implementation-defined (see Task B).

`crates/manifold-renderer/src/effects/shaders/fx_wireframe_depth.wgsl:503`
`crates/manifold-renderer/src/effects/shaders/fx_wireframe_depth.wgsl:581`

#### 4.7 Workgroup Size / Memory

**N/A** -- All entry points are fragment shaders.

#### 4.8 Integer Overflow

**VERIFIED SAFE** -- No integer index calculations.

#### 4.9 select() with NaN

**VERIFIED SAFE** -- `select()` usage at lines 327-329 (blend overlay): both branches are pure arithmetic on clamped `[0,1]` inputs (`base_col`, `blend_col`). No NaN risk from the branch values.

`crates/manifold-renderer/src/effects/shaders/fx_wireframe_depth.wgsl:327-329`

---

### 5. `compositor_blend_compute.wgsl`

File: `crates/manifold-renderer/src/generators/shaders/compositor_blend_compute.wgsl`

1 compute entry point: `cs_main`.

#### 5.1 Loops

**VERIFIED SAFE** -- No loops.

#### 5.2 Division

**INFO** -- Multiple divisions:

1. Line 46: `blend_uv /= s_val` where `s_val = max(u.scale_val, 0.01)` at line 45. Guarded -- minimum 0.01.

2. Lines 64-65: Unpremultiply `blend.rgb / max(blend.a, 0.01)` guarded by `blend.a > 0.001` check. If `blend.a` is in (0.001, 0.01), `max(blend.a, 0.01)` = 0.01. Safe.

3. Lines 118-120: ColorDodge `b / (1.0 - f_val)`. Guarded by `select(..., 100.0, f_val >= 0.999)`. When `f_val` is in [0.0, 0.999), denominator is in (0.001, 1.0]. Safe.

`crates/manifold-renderer/src/generators/shaders/compositor_blend_compute.wgsl:45-46`
`crates/manifold-renderer/src/generators/shaders/compositor_blend_compute.wgsl:64-65`
`crates/manifold-renderer/src/generators/shaders/compositor_blend_compute.wgsl:118-120`

**VERIFIED SAFE** -- All divisions guarded.

#### 5.3 pow/log/sqrt/atan2

**VERIFIED SAFE** -- None present.

#### 5.4 Texture Bounds

**VERIFIED SAFE** -- `textureStore` at lines 81, 106, 110, 137 uses `vec2<i32>(id.xy)` which is within bounds due to early return at line 24: `if id.x >= dims.x || id.y >= dims.y { return; }`. All `textureSampleLevel` calls use UVs in [0,1] range (blend_uv bounds-checked at line 52, base UV constructed from `id.xy / dims`).

`crates/manifold-renderer/src/generators/shaders/compositor_blend_compute.wgsl:24`

#### 5.5 Boundary Thread Early Return

**VERIFIED SAFE** -- Line 24: `if id.x >= dims.x || id.y >= dims.y { return; }`.

#### 5.6 Feedback / NaN

**VERIFIED SAFE** -- No feedback. Reads two input textures, writes one output per frame.

#### 5.7 Workgroup Size

**VERIFIED SAFE** -- `@workgroup_size(16, 16)` = 256 total invocations. Within limit.

`crates/manifold-renderer/src/generators/shaders/compositor_blend_compute.wgsl:21`

#### 5.8 Workgroup Memory

**VERIFIED SAFE** -- No `var<workgroup>` declarations.

#### 5.9 Integer Overflow

**VERIFIED SAFE** -- No integer index calculations beyond `id.xy`.

#### 5.10 select() with NaN

**WARNING** -- ColorDodge blend (case 10, lines 118-120):

```wgsl
select(b.r / (1.0 - f_val.r), 100.0, f_val.r >= 0.999),
```

In WGSL, `select(false_val, true_val, condition)` evaluates BOTH branches before selecting. If `f_val.r >= 0.999` (condition true), the result should be 100.0. But the `false_val` expression `b.r / (1.0 - f_val.r)` is still evaluated. When `f_val.r = 1.0` exactly, `1.0 - 1.0 = 0.0`, producing `b.r / 0.0 = Inf` (or NaN if `b.r = 0.0`).

Under IEEE 754: `select` returns the correct branch value regardless of the other branch's value, so the output is 100.0 even if the false branch is Inf/NaN. **The result is correct.**

Under `fast_math`: The compiler may optimize this differently. If it reorders or uses a conditional move that checks the Inf/NaN, behavior is implementation-defined. In practice, Metal's fast_math on Apple Silicon still handles `select` correctly because it compiles to a ternary instruction. **Low risk but theoretically fragile.**

Same pattern exists in the fragment version at `compositor_blend.wgsl:145-147`.

`crates/manifold-renderer/src/generators/shaders/compositor_blend_compute.wgsl:118-120`
`crates/manifold-renderer/src/generators/shaders/compositor_blend.wgsl:145-147`

---

## Task B: `set_fast_math_enabled(true)` Impact Analysis

### Where fast_math is set

`set_fast_math_enabled(true)` is called in exactly 2 locations:

1. `crates/manifold-gpu/src/metal/device.rs:160` -- compute pipeline compilation
2. `crates/manifold-gpu/src/metal/device.rs:298` -- render pipeline compilation

This means **every single shader pipeline** compiled through `GpuDevice` has fast_math enabled. There is no per-pipeline opt-out.

### What fast_math does on Metal/MSL

Metal's fast_math enables the following transformations:
- **Flush denormals to zero** -- very small floating-point values become 0.0
- **Reorder floating-point operations** -- may change results due to rounding
- **NaN/Inf may not propagate correctly** -- `isnan()` and `isinf()` may return false always
- **Assume no NaN/Inf inputs** -- the compiler may optimize based on this assumption
- **`clamp(NaN, a, b)` behavior is undefined** -- may return NaN, a, b, or garbage
- **`max(0.0, NaN)` may return 0.0** instead of NaN (implementation-defined)
- **`min`/`max` with NaN** -- result is implementation-defined

### Shader-specific impact

#### StylizedFeedback (`fx_stylized_feedback_compute.wgsl`)

**This is the #1 concern.** The previous audit (stability_audit_section_5_6_7.md, Q3) identified NaN propagation through the feedback loop as the highest-risk finding.

**Analysis with fast_math:**

The feedback loop at lines 57-66 blends `current + prev * amt` (additive mode) or `max(current, prev * amt)` (max mode). If NaN enters `prev` (the state buffer from the previous frame), the behavior depends entirely on fast_math's NaN handling:

**Optimistic scenario (fast_math helps):** Apple Silicon's fast_math implementation often flushes NaN results to 0.0 in practice. If `prev` contains NaN:
- `prev * amt` might become 0.0 (NaN flushed)
- `max(current, 0.0)` returns `current` (recovered)
- The feedback loop self-heals within 1-2 frames

**Pessimistic scenario (fast_math hurts):** fast_math may compile away the implicit NaN safety of `clamp` operations:
- `clamp(NaN, 0.0, 1.0)` could return NaN instead of clamping
- The NaN persists in the state buffer and amplifies

**Verdict: UNCERTAIN** -- fast_math makes NaN behavior **implementation-defined rather than IEEE 754 deterministic**. On current Apple Silicon (M1/M2/M3/M4), empirical testing suggests fast_math tends to flush NaN to 0.0, which would make the feedback loop self-healing. But this is **not guaranteed by the Metal spec** and could change between GPU driver versions or future hardware.

The Rust-side clamp at `stylized_feedback.rs:109` caps `feedback_amount` at 0.98 (preventing gain > 1), and the shader's edge_mask (lines 45-47) zeros samples near UV boundaries. These Rust-side guards limit the blast radius but don't prevent NaN once it enters the state buffer.

`crates/manifold-renderer/src/effects/shaders/fx_stylized_feedback_compute.wgsl:57-66`
`crates/manifold-renderer/src/effects/stylized_feedback.rs:109`

**CRITICAL** -- The zoom uniform at line 31 (`transformed_uv = transformed_uv / uniforms.zoom`) has no shader-side guard against zero. The Rust-side `zoom` parameter is read directly from `param_values` at `stylized_feedback.rs:110` with default 0.95 and registry range [0.9, 1.1]. If a user (or OSC/MIDI input) sets zoom to exactly 0.0, this produces Inf/NaN which enters the feedback state buffer and persists indefinitely. The registry min is 0.9 (see `effect_definition_registry.rs:328`), but OSC messages can bypass registry bounds.

`crates/manifold-renderer/src/effects/shaders/fx_stylized_feedback_compute.wgsl:31`
`crates/manifold-renderer/src/effects/stylized_feedback.rs:110`
`crates/manifold-core/src/effect_definition_registry.rs:328`

#### Compositor Blend (`compositor_blend.wgsl` / `compositor_blend_compute.wgsl`)

**INFO** -- The ColorDodge `select()` pattern (discussed in 5.10 above) could theoretically be affected by fast_math optimizations. In practice, Metal compiles `select` to branchless conditional moves that don't depend on NaN semantics of the unselected branch. The explicit `max(blend.a, 0.01)` guard for unpremultiply (line 65/74) is safe regardless of fast_math because it operates on positive values.

No `isnan()`/`isinf()` calls exist in any compositor shader -- verified via codebase-wide search. Therefore fast_math's "NaN checks become unreliable" concern does not apply.

`crates/manifold-renderer/src/generators/shaders/compositor_blend_compute.wgsl:65`
`crates/manifold-renderer/src/generators/shaders/compositor_blend.wgsl:74`

#### Wireframe Depth (`fx_wireframe_depth.wgsl`)

**INFO** -- The extensive temporal feedback in this effect (14 persistent textures across passes) could theoretically accumulate fast_math rounding drift over many frames. However, all temporal values pass through explicit `clamp(..., 0.0, 1.0)` calls, and the flow estimate denominator has an epsilon guard (`+ 0.0008` at line 395).

Under fast_math, `clamp(NaN, 0.0, 1.0)` is implementation-defined. If NaN enters any of the 14 temporal textures (prev_analysis, prev_depth, flow, mesh_coord, surface_cache, history), the clamp might not catch it. However, the input to this effect is already through the compositor, and there are no division-by-zero paths within the wireframe shader itself (all divisions are epsilon-guarded). Risk is **very low** -- NaN would have to enter from outside (corrupt generator output).

`crates/manifold-renderer/src/effects/shaders/fx_wireframe_depth.wgsl:395`

#### Shaders with no isnan/isinf

**VERIFIED SAFE** -- A codebase-wide search of all `.wgsl` files found zero uses of `isnan()`, `isinf()`, `is_nan()`, or `is_inf()`. No shader relies on explicit NaN/Inf detection that fast_math would break.

### Global Assessment: Should fast_math be disabled for specific pipelines?

**RECOMMENDATION (WARNING):**

The global `set_fast_math_enabled(true)` is appropriate for the vast majority of shaders (generators, single-pass effects, compositing) where the performance benefit is real and NaN/Inf inputs are practically impossible.

For the **StylizedFeedback** effect specifically, fast_math creates an ambiguous safety profile: it might help (flushing NaN to zero) or hurt (making clamp unreliable). The correct fix is not to disable fast_math but to add an explicit NaN sanitization clamp in the shader before writing to the state buffer:

```wgsl
// Before textureStore:
result = max(result, vec4<f32>(0.0));  // Flush any NaN/negative to 0
result = min(result, vec4<f32>(100.0)); // Cap HDR range
```

This would make the feedback loop provably stable regardless of fast_math behavior, at negligible performance cost (one max + one min per pixel).

---

## Summary of Findings

| # | Severity | Shader | Finding |
|---|----------|--------|---------|
| S-1 | **CRITICAL** | `fx_stylized_feedback_compute.wgsl:31` | Division by `uniforms.zoom` with no zero guard. OSC can bypass registry min. NaN enters persistent feedback buffer. |
| S-2 | WARNING | `fluid_scatter_3d.wgsl:79` | `@workgroup_size(8,8,8)` = 512 exceeds project convention of 256. Works on Apple Silicon but violates documented constraint. Also affects `fluid_gradient_curl_3d.wgsl:27`. |
| S-3 | WARNING | `compositor_blend_compute.wgsl:118-120` | `select()` evaluates division branch even when condition is true. Produces Inf/NaN in false branch. Correct under IEEE 754 but theoretically fragile under fast_math. Same in `compositor_blend.wgsl:145-147`. |
| S-4 | WARNING | `set_fast_math_enabled(true)` global | Makes `clamp(NaN, ...)` behavior implementation-defined across all shaders. Feedback loops (StylizedFeedback, WireframeDepth) depend on clamp for stability. |
| S-5 | INFO | `mycelium_diffuse.wgsl:48` | Trail feedback uses `max(0.0, ...)` which returns NaN under IEEE 754 if input is NaN. fast_math may flush to 0 (helpful). |
| S-6 | INFO | `fx_wireframe_depth.wgsl:503,581` | Temporal feedback through 14 textures relies on `clamp` for value bounding. All divisions epsilon-guarded. NaN risk only from external input. |
| S-7 | INFO | `fluid_scatter_3d.wgsl:58` | Integer index `z*vr*vr + y*vr + x` safe for current vol_res range {64,128,256} but would overflow u32 if vol_res > 1290. |
| S-8 | VERIFIED SAFE | All 5 shaders | No `loop`/`while` constructs. All `for` loops have compile-time constant bounds. |
| S-9 | VERIFIED SAFE | All 5 shaders | No `pow`, `log`, `log2` usage. `sqrt` arguments are always sums-of-squares (non-negative). `atan2(0,0)` returns 0 on Metal (identity rotation). |
| S-10 | VERIFIED SAFE | All 5 shaders | No `isnan()`/`isinf()` calls anywhere in the codebase's WGSL files. fast_math cannot break non-existent NaN checks. |
| S-11 | VERIFIED SAFE | All 5 shaders | No `var<workgroup>` memory declarations. |
| S-12 | VERIFIED SAFE | All compute shaders | Boundary threads have early-return guards in all compute entry points. |
