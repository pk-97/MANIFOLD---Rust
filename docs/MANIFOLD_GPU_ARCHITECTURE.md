---
name: manifold-gpu Native Backend Architecture
description: Decision to build manifold-gpu crate — native Metal content thread, wgpu UI thread + Windows fallback. Full phase roadmap through raw Metal optimization.
type: project
---

## Decision: 2026-03-25

Replace wgpu on the content thread hot path with a purpose-built `manifold-gpu` crate. Native Metal on macOS, wgpu backend for Windows. UI thread stays wgpu on all platforms.

**Why:** Every optimization since Phase 2 has been bypassing wgpu (hal encoding, shared-memory buffers, MTLSharedEvent, compute migration). The hybrid wgpu+hal architecture adds snatch locks, dual pipeline creation, encoder split constraints, and `#[cfg]` branches everywhere. Professional tools (Resolume Arena, TouchDesigner) use native GPU APIs directly.

**Why:** Windows is a confirmed shipping target (VJ community uses high-end GPUs on Windows).

## Architecture

```
manifold-gpu/
├── lib.rs              — compile-time backend selection (zero-cost, no vtable)
├── types.rs            — shared enums (TextureFormat, WorkgroupSize, etc.)
├── metal/              — native Metal implementation (macOS content thread)
│   ├── device.rs       — GpuDevice (MTLDevice)
│   ├── encoder.rs      — GpuEncoder (MTLCommandBuffer + encoders)
│   ├── texture.rs      — GpuTexture / GpuTextureView (MTLTexture)
│   ├── buffer.rs       — GpuBuffer (MTLBuffer, shared-memory mapped by default)
│   ├── pipeline.rs     — GpuComputePipeline / GpuRenderPipeline (WGSL→SPIR-V→spirv-opt→SPIRV-Cross→MSL)
│   ├── sampler.rs      — GpuSampler
│   ├── sync.rs         — GpuEvent (MTLSharedEvent)
│   ├── heap.rs         — GpuHeap (MTLHeap — memoryless, aliasing, lossy compression)
│   ├── mps.rs          — MPS blur, Sobel, scale kernels
│   ├── metalfx.rs      — MetalFX Spatial/Temporal scalers
│   └── archive.rs      — MTLBinaryArchive (pipeline caching)
└── wgpu_backend/       — wgpu fallback (Windows/Linux + macOS UI thread)
    ├── device.rs       — same API, backed by wgpu::Device
    ├── encoder.rs      — same API, backed by wgpu::CommandEncoder
    └── ...
```

**Compile-time selection:** `#[cfg(target_os = "macos")] pub use metal::*;` — consumer code uses `GpuDevice`, `GpuTexture`, etc. without knowing the backend. Zero overhead.

**API surface:** ~15 methods total. create_texture, create_buffer, create_pipeline, create_sampler, dispatch_compute, begin/end_render_pass, copy_texture, clear_texture, submit, signal_event. Purpose-built for MANIFOLD, not general-purpose.

**Shaders:** WGSL everywhere. On Metal: WGSL → naga → SPIR-V → spirv-opt (22 optimization passes) → SPIRV-Cross → MSL. On wgpu: WGSL→SPIR-V/HLSL via naga. Compilation runs at pipeline creation (startup), not per-frame. MTLBinaryArchive caches compiled GPU binaries to disk.

**UI thread:** Stays on wgpu directly on all platforms. Separate device, separate concern. Does not use manifold-gpu.

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
| **manifold-gpu crate** | Extract hal code into native Metal backend (metal crate, not wgpu::hal). Metal-only, no wgpu fallback on content thread | **Done**        |
| **Resource migration** | All content-thread textures/buffers → manifold_gpu types. Zero wgpu::Device on content thread    | **Done**        |
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

## Migration Strategy

Incremental, not big-bang. Same pattern as Phases 2-4:

1. Build crate with GpuTexture/GpuBuffer wrapping existing hal code
2. Migrate GpuEncoder
3. Migrate pipelines
4. Effects/generators migrate one by one
5. Each step compiles and runs
6. Existing hal code (hal_context.rs, hal_pipeline.rs, ring buffers) becomes the Metal backend — not new code, extracted code

## Windows / Linux Backend

**Goal:** Native Vulkan backend for industry-competitive performance on Windows and Linux. MANIFOLD is a professional tool competing with Resolume Arena and TouchDesigner — wgpu overhead is not acceptable if it costs frames.

**Strategy:** wgpu serves as an interim fallback to get Windows running quickly. The `manifold-gpu` API boundary is designed so the wgpu backend can be replaced with native Vulkan without touching any effect or generator code. Profile on Windows with wgpu first — if performance is insufficient (likely for complex projects), build the native Vulkan backend.

**Native Vulkan backend scope:**

- Same ~15 method API as Metal backend
- VMA (Vulkan Memory Allocator) for memory management
- Vulkan descriptor sets + push descriptors for bindings
- SPIR-V shaders via naga (WGSL→SPIR-V, same as WGSL→MSL for Metal)
- VK_KHR_dynamic_rendering (no render pass objects)
- Linux support comes free with Vulkan

**Platform-specific equivalents:**

- MTLHeap → VMA pool allocations
- MPS blur/Sobel → compute shader fallbacks (no Vulkan equivalent)
- MetalFX → FSR (AMD FidelityFX) or DLSS (NVIDIA) if available
- Memoryless → VK_MEMORY_PROPERTY_LAZILY_ALLOCATED_BIT
- Lossy compression → vendor-specific extensions (VK_EXT_image_compression_control)
- MTLBinaryArchive → VkPipelineCache (pipeline caching to disk)



## MTL HEAP OPTIMISATIONS
| **TO DO LATER**|
Layer 2 (MTLHeap backing): When startup time matters. Right now the pool warms up by calling device.create_texture() 10-30 times over the first 3 frames — kernel allocator calls that take microseconds each. Heap sub-allocation replaces those with nanosecond pointer bumps. The difference is maybe 1-2ms total during the first 3 frames of playback. You'd do this when launch-to-first-frame speed matters for live performance (show starts, you hit play, visuals need to appear instantly). Not urgent.

Layer 3 (intra-frame aliasing): When GPU memory pressure is a problem. If you're running complex projects with WireframeDepth (10 intermediates) + Fluid3D (10+ 3D volumes) + multiple feedback effects (persistent state buffers per clip) and hitting VRAM limits or causing eviction — aliasing reduces peak memory by letting non-overlapping textures share physical memory. On an M4 Max with 64-128GB unified memory, you're unlikely to hit this. You'd do this if you target lower-end Apple Silicon (M1/M2 MacBook Air with 8GB unified memory) where a complex project could genuinely run out.

## PHASE 9: ASYNC COMPUTE

**Problem:** The content thread currently encodes one serial command buffer: generator 1 → effect chain 1 → generator 2 → effect chain 2 → ... → compositor. Each step waits for the previous one. With N layers running heavy generators, frame time scales linearly: N × generator_cost.

**Solution:** Split independent layer work into parallel command buffers with explicit dependencies via MTLEvent:

```
Command Buffer A: Generator 1 → Effect Chain 1 ─┐
Command Buffer B: Generator 2 → Effect Chain 2 ─┼→ Compositor (waits for all)
Command Buffer C: Generator 3 → Effect Chain 3 ─┘
```

Generators on different layers are independent — they don't read each other's output. They can execute concurrently on different GPU compute units. The compositor signals a wait on all generator command buffers before blending.

**Impact:** 8 layers each with 2ms generators: serial = 16ms, parallel = 2ms + compositor overhead. Scales with layer count — the more layers, the bigger the win.

**Implementation:**
- One `MTLCommandBuffer` per layer (generator + effect chain)
- `MTLEvent` signals between layer command buffers and compositor command buffer
- CPU encodes all layer command buffers, commits all, then encodes compositor after all complete
- Compositor command buffer uses `waitForEvent` on each layer's completion signal

**When:** After function constants + binary archive. Profile first to confirm layer-parallel GPU time is the bottleneck vs CPU encoding time.

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