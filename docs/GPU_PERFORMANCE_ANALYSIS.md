# GPU Performance Analysis — 2026-03-24

## Problem Statement

Unity hits 120 FPS on the "burn out" project (2149 generator clips, 11 layers, 6 master effects, 3456×2234 resolution). Rust/wgpu achieves only 40-50 FPS on the same project, same M4 Max hardware, same resolution. Unity is running inside the editor with additional overhead and still 3x faster.

## Investigation Method

1. Added unconditional `[PERF]` stderr timing to `render_content()` measuring: generator encode, descriptor build, compositor encode, queue.submit, device.poll
2. Tested with/without effects, varying clip counts
3. Tested non-blocking poll (proved GPU is genuinely the bottleneck, not just sync overhead)
4. Captured 5-second Metal System Trace in Xcode Instruments (Game Performance template, Performance Limiters counter set)
5. Analyzed GPU Channel Activity, Metal Encoder Hierarchy, Fragment timeline, resource allocations

## Measured Results

### Per-Pass GPU Cost (Metal Instruments — Fragment track)

| Measurement | Value |
|---|---|
| Blend Pass GPU execution | **629.75 μs** |
| Theoretical bandwidth minimum (3456×2234 × Rgba16Float, read+write) | ~340 μs |
| Per-pass overhead (tile management + shader setup) | ~290 μs |

The 290μs overhead per pass is Metal TBDR tile load/store cost — unavoidable for render passes, but eliminable with compute shaders.

### CPU-to-GPU Scheduling Latency

| Process | Avg CPU→GPU Latency |
|---|---|
| manifold | **5.93 ms** |
| WindowServer | 0.44 ms |

Our process has 13x higher scheduling latency than WindowServer. Caused by the synchronous `device.poll(wait_indefinitely())` cycle that leaves the GPU idle while the CPU encodes the next frame.

### wgpu Internal Overhead

| Encoder | Duration |
|---|---|
| (wgpu internal) Pending #1 | 1.48 ms |
| (wgpu internal) Pending #2 | 665 μs |
| **Total per frame** | **~2.1 ms** |

Caused by `queue.write_buffer()` going through wgpu's staging belt. Each uniform write allocates a fresh Metal staging buffer, copies data, and submits via an internal blit encoder.

### Frame Composition

- 58 Metal command encoders per content frame (with effects)
- ~20-25 are our named render passes
- ~5-7 are UI render passes
- ~5 are wgpu internal (staging, signal, transit)
- Remainder are unnamed command buffers (wgpu internal splits)

### PERF Timing (our instrumentation, no effects, 1 clip)

```
gen=0.0ms desc=0.0ms comp=0.2ms submit=0.5ms poll=10.2ms | total=10.9ms (92fps)
```

CPU work: <2ms. The `poll` (GPU wait) dominates at 10.2ms.

### Confirmed Non-Issues

- Thermal state: Nominal (not throttled)
- GPU performance state: Maximum (full clock)
- CPU overhead: negligible (<2ms)
- Texture format: Rgba16Float matches Unity's ARGBHalf
- Not a regression: old commits have same performance
- Not the profiler: slow without profiling feature enabled

## Root Causes

### 1. Synchronous GPU Stall (~6ms wasted per frame)

```
GPU finishes frame N
  → poll(wait) thread wakeup: 1-2ms
    → CPU encodes frame N+1: 2ms
      → submit frame N+1
        → GPU has been IDLE for 3-4ms waiting for work
```

`device.poll(wait_indefinitely())` serializes CPU and GPU. Unity uses triple-buffered async submission — GPU always has work queued ahead.

### 2. Metal TBDR Tile Overhead (~5.8ms per frame)

Every render pass on Apple Silicon's Tile-Based Deferred Rendering:
1. Allocate on-chip tile memory
2. Load tiles from DRAM (if LoadOp::Load)
3. Run vertex shader → rasterizer → fragment shader
4. Store tiles back to DRAM

For fullscreen 2D blits (which ALL our effects and blend passes are), the tile machinery is unnecessary overhead. There's no geometry, no depth testing, no hardware blending that requires tiles.

Measured: 0.29ms overhead per pass × 20 passes = 5.8ms/frame.

Compute shaders bypass TBDR entirely — direct `textureLoad`/`textureStore`.

### 3. wgpu Staging Belt (~2.1ms per frame)

Every `queue.write_buffer()` call:
1. Allocates a new Metal staging buffer
2. Copies uniform data from CPU to staging buffer
3. Submits an internal blit encoder to copy staging → GPU buffer

This happens for every blend pass uniform, every effect uniform, every generator uniform — 20+ times per frame. The two "(wgpu internal) Pending" encoders cost 1.48ms + 665μs = 2.1ms total.

Fix: use a persistent mapped ring buffer and write uniforms directly via dynamic offsets.

### 4. Extra Render Passes vs Unity (~1.5-3ms per frame)

| Issue | Extra Passes | Per-Pass Cost |
|---|---|---|
| StylizedFeedback state copy (blit vs CopyTexture) | 1-2 | 0.63ms each |
| Feedback state copy (same issue) | 1 | 0.63ms |
| Effect chain blend-back to compositor | 1-2 | 0.63ms each |
| Halation separable blur (4 vs Unity's 3) | 1 | 0.63ms |

Unity's `Graphics.CopyTexture()` is a GPU memcpy with zero shader cost. Our state copies use a full render pass with a passthrough shader.

Unity's effect chain operates in-place on the compositor's ping-pong buffers. Ours uses a separate buffer pool, requiring a blend-back pass after each chain invocation.

### 5. Per-Frame Resource Allocation Churn

- 50-70 bind groups created and destroyed per content frame
- WireframeDepth creates 3+ RenderTargets + 3 UBOs per frame in `apply()`
- BlobTracking creates a downsample RenderTarget per frame
- Unity pools all temporary textures via `RenderTexture.GetTemporary()`
- 1.55 GiB total Metal resource allocations measured over 5 seconds

## Fix Plan

### Phase 1: Async Pipeline (estimated ~6ms savings)

Replace `device.poll(wait_indefinitely())` with double-buffered async completion:
- Two output buffers (or two IOSurface textures)
- Content thread encodes frame N+1 while GPU renders frame N
- Semaphore or fence prevents getting more than 2 frames ahead
- UI stays decoupled on its own device (no architectural change)
- GPU transitions directly from frame N to N+1 with near-zero idle gap

### Phase 2: Compute-Based Effects (estimated ~5ms savings, UNTESTED)

Convert SimpleBlitHelper and blend passes from render passes to compute dispatches:
- Same shader math, different API: `textureLoad`/`textureStore` instead of `textureSample`/fragment output
- Eliminates TBDR tile overhead (0.29ms per pass × 20 passes = 5.8ms)
- Each effect shader needs WGSL compute variant
- **This is untested theory — needs prototype to validate**

### Phase 3: Eliminate wgpu Staging (~2ms savings)

Replace per-pass `queue.write_buffer()` with a persistent mapped ring buffer:
- One large uniform buffer, sub-allocate per pass with dynamic offsets
- Direct CPU writes via persistent mapping (no staging belt)
- Eliminates the two "(wgpu internal) Pending" encoders

### Phase 4: Eliminate Extra Passes (~1.5ms savings)

- Replace StylizedFeedback/Feedback state copy blits with `copy_texture_to_texture`
  - Requires passing target `Texture` (not just `TextureView`) through the `apply()` API
- Merge effect chain into compositor (effects operate on compositor ping-pong directly)
- Review Halation pass count vs Unity

### Phase 5: Resource Pooling

- Cache bind groups by (texture view, sampler) key — recreate only on resize/clip change
- Implement `RenderTarget::get_temporary(w, h, format)` texture pool
- Eliminate per-frame allocation in WireframeDepth, BlobTracking

## Projected Outcome

| State | Frame Time | FPS |
|---|---|---|
| Current | ~20ms | ~50 |
| After Phase 1 (async) | ~14ms | ~71 |
| After Phase 2 (compute) | ~9ms | ~111 |
| After Phase 3 (staging) | ~7ms | ~143 |
| After Phase 4 (passes) | ~5.5ms | ~182 |

**Note:** Phase 2 estimates are theoretical. Compute dispatch overhead through wgpu is untested.

## Files Changed During Investigation

- `crates/manifold-app/src/content_pipeline.rs` — added [PERF] timing instrumentation
- `crates/manifold-renderer/src/layer_compositor.rs` — zero-clip early exit (skip master effects on empty playback)
- `crates/manifold-renderer/src/tonemap.rs` — added `clear()` method for empty playback path
- `crates/manifold-renderer/src/effects/stylized_feedback.rs` — documented state copy TODO
- `crates/manifold-renderer/src/gpu.rs` — forced `Backends::METAL` + adapter logging (reverted)

## Reference: Metal Instruments Capture

Captured with Game Performance template, Performance Limiters counter set, 5-second duration.
Project: "burn out DEBUG TESTING.manifold" (simplified, no FluidSim).
Hardware: M4 Max MacBook Pro, macOS, Thermal Nominal, GPU Maximum performance state.
