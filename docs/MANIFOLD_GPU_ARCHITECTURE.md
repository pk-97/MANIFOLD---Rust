---
name: manifold-gpu Native Metal Architecture
description: manifold-gpu crate architecture — native Metal on all threads, zero wgpu. Phase roadmap through raw Metal optimization.
type: project
---

<!-- index: The manifold-gpu native Metal backend: async compute, function constants, texture pool, uniform layout, texture formats. Read before touching shaders or uniforms. -->

## Decision: 2026-03-25 (objc2-metal migration completed 2026-04-19)

Purpose-built `manifold-gpu` crate on typed `objc2-metal` bindings. Native Metal on all threads (content + UI). Zero wgpu anywhere in the codebase. Zero dependency on the unmaintained gfx-rs `metal` crate, `objc 0.2`, or `block 0.1`.

**Why:** wgpu submission overhead was 8-15ms. Native Metal brought it to 4.5-5.5ms. Professional tools (Resolume Arena, TouchDesigner) use native GPU APIs directly.

## Architecture

```
manifold-gpu/
├── lib.rs              — crate entry; re-exports metal::*
├── types.rs            — shared enums (TextureFormat, WorkgroupSize, etc.)
└── metal/              — native Metal implementation (objc2-metal bindings)
    ├── mod.rs          — public re-exports, SlotMap (WGSL @binding → Metal arg index)
    ├── device.rs       — GpuDevice (MTLDevice + MTLCommandQueue, pipeline + resource factories)
    ├── encoder.rs      — GpuEncoder (MTLCommandBuffer + compute/render/blit encoders, bind caches)
    ├── types.rs        — GpuTexture/Buffer/Sampler/Pipeline/DepthStencil/Event/Heap/FenceWaiter
    ├── format.rs       — GpuTextureFormat → MTLPixelFormat mappers
    ├── shader_compiler.rs — WGSL → naga → SPIR-V → spirv-opt → SPIRV-Cross → MSL + slot map
    ├── msl_cache.rs    — on-disk MSL compilation cache (skip WGSL frontend on warm launch)
    ├── surface.rs      — GpuSurface / GpuDrawable (CAMetalLayer + EDR configuration)
    ├── texture_pool.rs — frame-stamped MTLHeap-backed texture recycling
    ├── archive.rs      — MTLBinaryArchive (compiled pipeline binaries on disk)
    ├── metalfx.rs      — MetalFX Spatial scaler
    └── mps.rs          — MPS kernels (blur, Sobel, scale, histogram, reduction, ...)
```

**CVDisplayLink** lives in `manifold-app/src/display_link.rs`, not in manifold-gpu — each window owns its own display link.

**API surface:** ~15 core methods. create_texture, create_buffer, create_pipeline, create_sampler, dispatch_compute, begin/end_render_pass, copy_texture, clear_texture, submit, signal_event. Purpose-built for MANIFOLD, not general-purpose.

**Shaders:** WGSL everywhere. Pipeline: WGSL → naga → SPIR-V → spirv-opt (22 optimization passes) → SPIRV-Cross → MSL. Intermediate MSL cached on disk (`msl_cache.rs`). Compiled GPU binaries cached via MTLBinaryArchive. Compilation runs at pipeline creation (startup), not per-frame.

**Ownership model:** All Metal objects are owned as `Retained<ProtocolObject<dyn MTLFoo>>` (automatic retain/release via `objc2::rc`). No manual `objc_retain`/`objc_release`, no raw pointer fields on GPU wrappers. Command buffers and encoders are fully typed — no `*mut c_void` cmd_buf tricks.

**All threads use manifold-gpu.** Content thread and UI thread both use native Metal. Zero wgpu anywhere in the codebase.

**Dependency policy:** manifold-gpu pulls only `objc2`, `block2`, `objc2-foundation`, `objc2-metal`, `objc2-metal-fx`, `objc2-metal-performance-shaders`. No `metal` crate, no `objc 0.2`, no `block 0.1`, no `core-graphics-types`. Raw-window-handle is the only non-objc2 macOS dep (winit interop).

## Phase Roadmap

| Phase                  | What                                                                                             | Status          |
| ---------------------- | ------------------------------------------------------------------------------------------------ | --------------- |
| 1                      | Foundation types (GpuEncoder wrapper)                                                            | **Done**        |
| 2                      | HAL pipeline + ComputeBlitHelper                                                                 | **Done**        |
| 3                      | All effects + generators to HAL                                                                  | **Done**        |
| 4                      | MTLSharedEvent sync                                                                              | **Done**        |
| 4B                     | All-compute pipeline (TBDR elimination)                                                          | **Done**        |
| 4.5                    | Generators → hal, single submission                                                              | **Done**        |
| 4.6                    | LinePipeline hal render + native readbacks → zero wgpu on content hot path                       | **Done**        |
| **manifold-gpu crate** | Extract hal code into native Metal backend (metal crate wrapper, not wgpu::hal). Metal-only, no wgpu fallback on content thread | **Done**        |
| **Resource migration** | All content-thread textures/buffers → manifold_gpu types. Zero wgpu::Device on content thread    | **Done**        |
| **objc2-metal migration** | Replace gfx-rs `metal` crate with typed `objc2-metal` bindings. Drops `objc 0.2`, `block 0.1`, `core-graphics-types` from dep graph | **Done** (2026-04-19) |
| 5                      | Frame-stamped texture recycling pool (zero per-frame allocations after 3-frame warmup)           | **Done**        |
| 6                      | MPS API (27 operations behind manifold-gpu). Effects use compound shaders — API available for future use | **Done**        |
| 7                      | MetalFX Frame Interpolation (Metal 4 / macOS Tahoe). Master output level — render at 90 FPS, interpolate to 120. Requires 2 frames + depth + motion vectors. Depth available when WireframeDepth active. Without depth/motion: spatial-only fallback. | Future          |
| 8                      | Function constants (bloom 4-way, compositor 13 blend modes, plasma 5-way, feedback 3-way, edge glow 3-way, fluid display 2-way) + MTLBinaryArchive pipeline caching | **Done**        |
| 9                      | Async compute — parallel command buffers for independent layer generator+effect chains. Serial: N×2ms. Parallel: 2ms. Scales with layer count. | **Done**        |
| 10                     | Indirect command buffers (ICB) — GPU-driven compositor encoding. CPU sends layer list, GPU encodes all blend dispatches in one shot. Eliminates per-layer CPU→GPU round-trips. Scales with layer count. | After 9 |

## Metal Version Target

- **Minimum:** Metal 2.4 (all Apple Silicon Macs, macOS Monterey+)
- **MetalFX:** requires Metal 3.0 / macOS Ventura (all Apple Silicon supports it)
- **Metal 4:** macOS 26 Tahoe (2025) — MetalFX Frame Interpolation, unified encoders. Future opportunity for Phase 7.
- **f16 math:** Investigated and rejected — nearly all shaders accumulate across taps/passes/frames, causing visible banding and jitter in f16. Not viable without per-shader empirical validation.

## Key Constraints

- **Resource lifetime:** No wgpu refcounting on native Metal. Must manually ensure textures/buffers survive in-flight command buffers (2-3 frames with triple buffering).
- **Ring buffer overflow:** Uniform ring buffers need either generous sizing or fence-based wraparound protection.
- **MetalFX Temporal:** Needs depth + motion vectors. MANIFOLD is 2D — only available when WireframeDepth effect is active (provides depth + flow). Spatial Scaler works unconditionally.
- **Current performance:** 5-7ms GPU frame times (~140-200 FPS GPU throughput) after native Metal migration. Zero "(wgpu internal) Signal" overhead on content thread. Profile after each remaining phase to verify gains.

## Windows / Linux Backend

Cross-platform (Mac, Windows, Linux) is a hard requirement as of 2026-07-02. The full
design — policy decisions, API contract, hazard-tracking architecture, phasing, and
platform-services inventory — lives in **`docs/VULKAN_BACKEND_DESIGN.md`**. Native `ash`
Vulkan; there is no wgpu interim step (an earlier version of this section proposed one —
superseded). Phase 0 scaffolding (`vulkan/` module, cfg-gated backend selection, shared
WGSL→SPIR-V pipeline) already ships in the crate.



## MTL HEAP OPTIMISATIONS
| **TO DO LATER**|
Layer 2 (MTLHeap backing): When startup time matters. Right now the pool warms up by calling device.create_texture() 10-30 times over the first 3 frames — kernel allocator calls that take microseconds each. Heap sub-allocation replaces those with nanosecond pointer bumps. The difference is maybe 1-2ms total during the first 3 frames of playback. You'd do this when launch-to-first-frame speed matters for live performance (show starts, you hit play, visuals need to appear instantly). Not urgent.

Layer 3 (intra-frame aliasing): When GPU memory pressure is a problem. If you're running complex projects with WireframeDepth (10 intermediates) + Fluid3D (10+ 3D volumes) + multiple feedback effects (persistent state buffers per clip) and hitting VRAM limits or causing eviction — aliasing reduces peak memory by letting non-overlapping textures share physical memory. On an M4 Max with 64-128GB unified memory, you're unlikely to hit this. You'd do this if you target lower-end Apple Silicon (M1/M2 MacBook Air with 8GB unified memory) where a complex project could genuinely run out.

## PHASE 9: ASYNC COMPUTE

**Problem:** With N layers running heavy effects, frame time scales linearly: N × effect_cost. Per-layer effect chains are independent and can execute concurrently.

**Solution:** Split per-layer effect work into parallel command buffers with explicit dependencies via MTLEvent. Generators remain on a separate shared command buffer because all generators render into their own textures sequentially (they share the uniform arena and layer state).

```
Command Buffer 0 (gen_enc):    All generators ──────────────────── committed first
Command Buffer 1 (Layer 0 CB): Effect Chain 0 ─┐
Command Buffer 2 (Layer 1 CB): Effect Chain 1 ─┼── committed next
Command Buffer 3 (Layer 2 CB): Effect Chain 2 ─┘
Command Buffer 4 (compositor): Wait for layers → Blend all ───── committed last
```

**CRITICAL: Command buffer commit ordering (hard-won lesson)**

Metal executes command buffers from the same queue in **commit order**. Per-layer CBs read generator textures — so the generator CB MUST be committed BEFORE per-layer CBs. If generators and effects were on the same command buffer (as they were originally), per-layer CBs would be committed before the generator writes were visible, causing:
- Cross-layer texture contamination (effects reading stale/wrong generator output)
- Effects appearing to not apply (reading uninitialized texture)
- Intermittent single-frame glitches at clip boundaries where multiple layers are active

The fix was splitting generators into their own CB (`gen_enc`) committed first. Per-layer CBs are committed next (Metal guarantees they see gen_enc's writes). The compositor CB is committed last and waits on all per-layer MTLEvent signals before blending.

This bug was invisible with single-layer playback, required 2+ active layers with effects, and manifested as rare single-frame visual artifacts that looked like "layer bleeding." Diagnosed via eprintln instrumentation of texture pointers, clip/layer state per frame, and beat-backward detection, then confirmed by forcing the serial path (which uses a single CB and is immune to the ordering issue).

**Impact:** Per-layer effect chains run concurrently. 8 layers each with 2ms effects: serial = 16ms, parallel = 2ms + compositor overhead.

**Implementation (actual):**
- `gen_enc` command buffer for ALL generators — committed first
- One `MTLCommandBuffer` per active layer (effect chain only, not generators)
- `MTLEvent` signals between per-layer command buffers and compositor command buffer
- CPU encodes all per-layer command buffers, commits all, then encodes compositor
- Compositor command buffer uses `encodeWaitForEvent` on the final layer's completion signal
- Serial fast path for single-layer frames (no parallel overhead)

## PHASE 10: INDIRECT COMMAND BUFFERS (ICB)

**Problem:** The compositor encodes blend passes one by one from the CPU: set pipeline, set textures, dispatch, repeat per layer. Each `encoder.dispatch()` call has CPU overhead — pipeline state validation, resource tracking, encoder state machine transitions. With many layers, this CPU encoding time becomes significant.

**Solution:** Build the entire compositor command list on the GPU using `MTLIndirectCommandBuffer`. The CPU provides a buffer of layer descriptors (blend mode, source texture, opacity), and a single GPU compute shader encodes all blend dispatches:

```
CPU: "Here are 32 layers with these blend modes and textures" (one buffer write)
GPU: Encodes 32 blend dispatches in one shot (single ICB execute)
```

**Impact:** Eliminates per-layer CPU→GPU round-trips for compositor encoding. Most significant with high layer counts (32+). Also enables GPU-driven culling — layers with zero opacity can be skipped without CPU involvement.

**Implementation:**
- `MTLIndirectCommandBuffer` with compute dispatch commands
- Layer descriptor buffer: array of (blend_mode, source_texture_index, opacity, enabled)
- Argument buffer for texture array (all layer outputs)
- Single compute kernel that reads descriptors and encodes blend dispatches
- CPU commits the ICB execution as a single command

**When:** After async compute. Profile to confirm CPU compositor encoding is a bottleneck at high layer counts. ICB shines at 32+ layers — at 8 layers the CPU encoding is likely <100μs.

**Prerequisite:** Async compute (Phase 9) should be done first — ICB compositing needs all layer outputs available, which async compute provides via parallel generation.
