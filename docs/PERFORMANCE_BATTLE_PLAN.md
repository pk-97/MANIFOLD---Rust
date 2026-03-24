# Performance Battle Plan — Closing the Unity Gap

**Status:** Phase 1 (async pipeline) and Phase 2 (compute effects) implemented.
**Current:** ~50 FPS at 3840×2160 with effects. Unity: 120 FPS same project.
**Target:** Match or exceed Unity.

## What We Know (Metal Instruments, 2026-03-24)

- Per-pass GPU cost: **629μs** (Blend Pass at 3840×2160 Rgba16Float)
- Bandwidth floor: **~240μs** per pass (M4 Max, 546 GB/s)
- Per-pass overhead: **~390μs** (wgpu encoding + Metal driver + staging)
- wgpu staging belt: **2.1ms/frame** (measured "(wgpu internal) Pending" encoders)
- CPU-to-GPU latency: **5.93ms** (with sync poll — now using async fence)
- Bind groups created per frame: **50-120**
- Total passes per frame (full project): **40-70**
- GPU is NOT thermal throttled, running at Maximum performance state

## What We've Done

| Change | Result |
|---|---|
| Async double-buffered pipeline | ✅ Correct architecture, minimal FPS impact (GPU-bound) |
| Compute effects (13 simple + blend) | ✅ Correct, needs Instruments validation of per-pass savings |
| Zero-clip early exit | ✅ Saves 6ms on empty frames |

## What Remains — The Full Battle Plan

### TIER 1: Reduce Per-Pass Overhead (~3-5ms estimated)

These attack the 390μs gap between actual per-pass cost (629μs) and bandwidth floor (240μs).

#### 1A. Global Uniform Arena

**Problem:** ~120 `queue.write_buffer()` calls per frame, each creating a wgpu staging buffer. Measured: 2.1ms in "(wgpu internal) Pending" encoders.

**Fix:** Single large uniform buffer (64KB). CPU writes all uniforms into a staging Vec, single `queue.write_buffer` at frame start. Each pass uses `BufferBinding { offset }` to read its slice.

**New file:** `crates/manifold-renderer/src/uniform_arena.rs`

```rust
pub struct UniformArena {
    buffer: wgpu::Buffer,       // 64KB, UNIFORM | COPY_DST
    cpu_staging: Vec<u8>,       // CPU mirror
    cursor: u64,
    min_align: u64,             // device.limits().min_uniform_buffer_offset_alignment
}

impl UniformArena {
    pub fn reset(&mut self);
    pub fn push<T: bytemuck::Pod>(&mut self, data: &T) -> (u64, &wgpu::Buffer);  // returns (offset, buffer_ref)
    pub fn flush(&self, queue: &wgpu::Queue);  // single write_buffer
}
```

**Integration:** Thread `&UniformArena` through render_content → generators → compositor → effects. Each currently calls `queue.write_buffer(own_buffer, ...)`. Change to `arena.push(&uniforms)` and use returned offset in bind group.

**Files affected:**
- New: `uniform_arena.rs`
- `content_pipeline.rs` — create arena, call reset/flush per frame
- `simple_blit_helper.rs` / `compute_blit_helper.rs` — accept optional arena
- `layer_compositor.rs` — BlendResources uses arena
- `tonemap.rs` — use arena
- All 21 effect `.rs` files — pass arena through
- All generator `.rs` files — use arena for uniform writes

**Estimated savings:** 1.5-2ms (eliminates most of the 2.1ms staging overhead)

#### 1B. Bind Group Caching

**Problem:** 50-120 `device.create_bind_group()` calls per frame. Each is a heap allocation + Metal descriptor set creation.

**Fix:** Cache bind groups keyed by their texture views. Only recreate when textures change (resize, clip start/stop). Most effects use the same source/target textures across consecutive frames.

**Approach for SimpleBlitHelper / ComputeBlitHelper:**
```rust
struct CachedBindGroup {
    bind_group: wgpu::BindGroup,
    source_id: wgpu::Id<wgpu::TextureView>,
    target_id: wgpu::Id<wgpu::TextureView>,  // only for compute
    buffer_offset: u64,
}
```

Check if source/target views match previous frame → reuse bind group. Invalidate on resize.

**Approach for BlendResources:**
The blend pass changes textures every call (ping-pong), so caching per-call is hard. But the bind group LAYOUT is shared. Use dynamic offsets more aggressively — one bind group per (source, blend, target) triple, reused across frames.

**Files affected:**
- `simple_blit_helper.rs` — add cache field
- `compute_blit_helper.rs` — add cache field
- `dual_texture_blit_helper.rs` — add cache field
- `layer_compositor.rs` — BlendResources cache

**Estimated savings:** 0.5-1ms (reduces CPU overhead and Metal descriptor allocation)

#### 1C. Fix Generator LoadOp

**Problem:** Several generators use `LoadOp::Load` where `LoadOp::Clear` would suffice.

**Audit result:** All 8 `LoadOp::Load` instances are JUSTIFIED (stateful generators that need previous frame data). No easy wins here.

**Estimated savings:** 0ms (already correct)

### TIER 2: Reduce Pass Count (~3-8ms estimated)

Each eliminated pass saves a full 629μs. This is the highest-leverage optimization.

#### 2A. Effect Chain Internal Blit Elimination

**Problem:** Effect chain copies input into its own ping buffer via a render pass blit before processing. Costs 1 extra pass per chain invocation. With 3 chains (clip, layer, master), that's 3 × 629μs = ~1.9ms.

**Fix:** Skip the internal blit when input format matches chain format (Rgba16Float — which it always is for our pipeline). Pass the external input view directly to the first effect.

**File:** `crates/manifold-renderer/src/effect_chain.rs`

Currently (line ~303):
```rust
self.internal_blit.as_ref().unwrap().blit(device, encoder, input_view, self.source_view());
```

Change to:
```rust
// First effect reads directly from input_view (no copy needed)
// Subsequent effects use chain's internal ping-pong
```

The first effect in the chain receives `input_view` as its source instead of `chain.source_view()`. After the first effect writes to `chain.target_view()`, the chain swaps and continues normally.

**Estimated savings:** ~1.9ms (3 passes eliminated)

#### 2B. State Copy Blit → Texture Copy

**Problem:** StylizedFeedback and Feedback copy their result to a state buffer using a full render pass blit. Unity uses `Graphics.CopyTexture` (GPU memcpy, zero shader cost).

**Fix:** Change `PostProcessEffect::apply()` signature to include `target_texture: &wgpu::Texture`. Then replace the blit with `encoder.copy_texture_to_texture()`.

**Files:**
- `crates/manifold-renderer/src/effect.rs` — add `target_texture` param to trait
- All 21 effect `.rs` files — add param (mechanical, most ignore it)
- `stylized_feedback.rs` — replace `copy_blit.draw()` with texture copy
- `feedback.rs` — same
- `effect_chain.rs` — pass target texture from its ping-pong RenderTarget

**Estimated savings:** ~1.3ms (2 passes eliminated when StylizedFeedback active)

#### 2C. Master Effect Blend-Back → Texture Copy

**Problem:** After master effects, the compositor does an Opaque blend pass to copy the chain result back into `tonemap.output`. This is a full render pass for what should be a GPU memcpy.

**Fix:** Replace with `encoder.copy_texture_to_texture()`. Requires exposing the effect chain's source texture.

**Files:**
- `effect_chain.rs` — add `pub fn source_texture(&self) -> &wgpu::Texture`
- `layer_compositor.rs` — replace Opaque blend with texture copy

**Estimated savings:** ~629μs (1 pass eliminated)

#### 2D. Effect Pass Merging (Advanced)

**Problem:** Adjacent simple effects in a chain each do: read texture → process → write texture. Each pass has 390μs overhead. Merging N effects into 1 pass saves (N-1) × 390μs.

**Approach — Uber-Shader for Simple Effects:**

For effects that are purely per-pixel (no spatial sampling beyond the current pixel's UV transform):
- ColorGrade, InvertColors, Brightness, Saturate, HueShift

These can be merged into a single compute dispatch that applies all enabled effects in sequence.

For effects that sample at different UVs (Mirror, EdgeStretch, ChromaticAberration, Transform):
- Cannot merge because each needs the previous effect's output at a potentially different UV

**Practical first step:** Merge the "color correction" effects (ColorGrade + InvertColors + Saturate + Brightness + HueShift) into one pass when multiple are enabled on the same chain. This is common on master effects.

**Files:**
- New: `effects/shaders/color_uber_compute.wgsl`
- New: `effects/color_uber.rs`
- `effect_chain.rs` — detect mergeable adjacent effects, invoke uber instead

**Estimated savings:** 1-3ms (depends on how many effects can merge)

### TIER 3: Reduce Expensive Effect Passes

These target the heaviest unconverted effects.

#### 3A. WireframeDepth Optimization

**12 passes per frame** when active. Largest single effect cost.

**Opportunities:**
- Pool the 3+ RenderTargets created per frame in `apply()` (analysis_rt, line_mask, history_next)
- Pool the 3 UBOs created per frame (ubo_analysis, ubo_wire, ubo_source) — use arena
- Convert single-pass sub-passes to compute where possible (analysis, wireframe mask, composite)
- The heuristic depth estimation and flow lock passes are harder to convert

**Files:** `effects/wireframe_depth.rs`, `effects/shaders/fx_wireframe_depth.wgsl`

**Estimated savings:** 2-4ms (texture pooling + compute conversion of sub-passes)

#### 3B. Bloom Pass Reduction

**6 passes** (Prefilter + 2 Down + 2 Up + Composite). Unity does this in 4 passes.

**Investigation needed:** Compare Unity's BloomFX.cs pass structure to ours. If Unity combines prefilter+first-down or last-up+composite, we should match.

**Files:** `effects/bloom.rs`, `effects/shaders/bloom.wgsl`

**Estimated savings:** ~1.3ms (2 fewer passes)

#### 3C. Halation Pass Reduction

**4 passes** (ThresholdTint, BlurH, BlurV, Composite). Unity does 3.

**Fix:** Combine ThresholdTint + first blur into one pass (sample source, apply threshold+tint, blur horizontally in one shader).

**Files:** `effects/halation.rs`, `effects/shaders/fx_halation.wgsl`

**Estimated savings:** ~629μs (1 fewer pass)

### TIER 4: Resource Management

#### 4A. Texture Pool

**Problem:** WireframeDepth creates 3+ RenderTargets per frame. BlobTracking creates 1 per frame.

**Fix:** `RenderTargetPool` that caches textures by (width, height, format). `get_temporary()` returns a pooled texture, `release()` returns it to the pool.

**New file:** `crates/manifold-renderer/src/render_target_pool.rs`

**Files affected:** `wireframe_depth.rs`, `blob_tracking.rs`

**Estimated savings:** Reduces GPU allocation churn (measured: 1.55 GiB over 5 seconds). Indirect FPS impact from reduced Metal driver overhead.

#### 4B. Generator Compute Conversion

**Problem:** Simple generators (Plasma, BasicShapesSnap, ConcentricTunnel, FractalZoom, NumberStation) use render passes for fullscreen procedural generation. These are purely per-pixel — no geometry.

**Fix:** Convert to compute dispatches using the same pattern as ComputeBlitHelper. These generators don't read a source texture — they generate content purely from uniforms + math.

**Files:** Each generator `.rs` + new `_compute.wgsl` shader variant.

**Estimated savings:** ~390μs per generator per frame. With 2-5 generators active, ~1-2ms.

### TIER 5: Validation & Profiling

**CRITICAL: Profile after EACH tier to measure actual impact.**

#### 5A. Per-Pass A/B Comparison

After all changes, capture Metal Instruments trace. Compare:
- Compute effect pass cost vs render effect pass cost (validate Phase 2)
- Staging belt duration with arena vs without (validate Tier 1A)
- Total encoders per frame (validate pass count reduction)
- Bind group allocation count (validate Tier 1B)

#### 5B. Unity Parity Benchmark

Run the EXACT same project in both Unity and Rust:
- Same resolution (3840×2160)
- Same effects enabled/disabled
- Same clip count at the same beat position
- Compare: total GPU time, pass count, per-pass cost

## Implementation Priority

```
IMMEDIATE (biggest impact per effort):
  Tier 2A — Effect chain blit elimination (~1.9ms, low effort)
  Tier 2B — State copy → texture copy (~1.3ms, moderate effort)
  Tier 2C — Master blend-back → texture copy (~0.6ms, low effort)

NEXT (measured overhead, good ROI):
  Tier 1A — Global uniform arena (~1.5-2ms, moderate effort)

THEN (cumulative gains):
  Tier 1B — Bind group caching (~0.5-1ms, moderate effort)
  Tier 3A — WireframeDepth optimization (~2-4ms, high effort)
  Tier 3B — Bloom pass reduction (~1.3ms, moderate effort)
  Tier 4B — Generator compute conversion (~1-2ms, moderate effort)

ADVANCED (architectural):
  Tier 2D — Effect pass merging (~1-3ms, high effort)
  Tier 4A — Texture pool (indirect savings, moderate effort)

ALWAYS:
  Tier 5 — Profile after each tier
```

## Projected Cumulative Savings

| After | Est. Savings | Est. Frame Time | Est. FPS |
|---|---|---|---|
| Current | — | ~20ms | ~50 |
| Tier 2 (pass elimination) | ~3.8ms | ~16ms | ~62 |
| Tier 1A (uniform arena) | ~2ms | ~14ms | ~71 |
| Tier 1B (bind group cache) | ~0.7ms | ~13.3ms | ~75 |
| Tier 3 (heavy effect opt) | ~4ms | ~9.3ms | ~107 |
| Tier 4B (gen compute) | ~1.5ms | ~7.8ms | ~128 |
| Tier 2D (pass merging) | ~2ms | ~5.8ms | ~172 |

**These are estimates.** Each tier MUST be validated with Metal Instruments before proceeding to the next.

## File Index

| File | Tiers | Changes |
|---|---|---|
| `manifold-renderer/src/uniform_arena.rs` | 1A | NEW |
| `manifold-renderer/src/render_target_pool.rs` | 4A | NEW |
| `manifold-renderer/src/effects/color_uber.rs` | 2D | NEW (optional) |
| `manifold-renderer/src/effect.rs` | 2B | Trait signature change |
| `manifold-renderer/src/effect_chain.rs` | 2A, 2B, 2C | Skip internal blit, pass target_texture |
| `manifold-renderer/src/layer_compositor.rs` | 2C, 1A | Texture copy blend-back, arena |
| `manifold-renderer/src/tonemap.rs` | 1A | Use arena |
| `manifold-renderer/src/effects/simple_blit_helper.rs` | 1A, 1B | Arena integration, bind group cache |
| `manifold-renderer/src/effects/compute_blit_helper.rs` | 1A, 1B | Arena integration, bind group cache |
| `manifold-renderer/src/effects/stylized_feedback.rs` | 2B | State copy → texture copy |
| `manifold-renderer/src/effects/feedback.rs` | 2B | State copy → texture copy |
| `manifold-renderer/src/effects/bloom.rs` | 3B | Pass reduction |
| `manifold-renderer/src/effects/halation.rs` | 3C | Pass reduction |
| `manifold-renderer/src/effects/wireframe_depth.rs` | 3A, 4A | Pool textures, optimize passes |
| All 21 effect `.rs` files | 2B | Mechanical: add target_texture param |
| `manifold-app/src/content_pipeline.rs` | 1A | Create + manage arena per frame |
