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
│   ├── pipeline.rs     — GpuComputePipeline / GpuRenderPipeline (naga WGSL→MSL)
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

**Shaders:** WGSL everywhere, compiled via naga (WGSL→MSL on Metal, WGSL→SPIR-V/HLSL on wgpu). Naga runs at pipeline creation (startup), not per-frame. MTLBinaryArchive caches compiled GPU binaries to disk.

**UI thread:** Stays on wgpu directly on all platforms. Separate device, separate concern. Does not use manifold-gpu.

## Phase Roadmap

| Phase                  | What                                                                                             | Status          |
| ---------------------- | ------------------------------------------------------------------------------------------------ | --------------- |
| 1                      | Foundation types (GpuEncoder wrapper)                                                            | **Done**        |
| 2                      | HAL pipeline + ComputeBlitHelper                                                                 | **Done**        |
| 3                      | All effects + generators to HAL                                                                  | **Done**        |
| 4                      | MTLSharedEvent sync                                                                              | **Done**        |
| 4B                     | All-compute pipeline (TBDR elimination)                                                          | **Done**        |
| 4.5                    | Generators → hal, single submission                                                              | **In progress** |
| 4.6                    | LinePipeline hal render + native readbacks → zero wgpu on content hot path                       | Next            |
| **manifold-gpu crate** | Extract hal code into proper native backend + wgpu fallback crate                                | After 4.6       |
| 5                      | MTLHeap + memoryless + aliasing + lossy compression                                              | **Done**        |
| 6                      | MPS kernels (27 types: blur, scale, edge, threshold, arithmetic, stats, keypoints, random)       | **Done**        |
| 7                      | MetalFX Scaler (Spatial confirmed; Temporal needs depth — only works when WireframeDepth active) | After 6         |
| 8                      | f16 math + pass fusion + function constants + MTLBinaryArchive                                   | After 7         |

## Metal Version Target

- **Minimum:** Metal 2.4 (all Apple Silicon Macs, macOS Monterey+)
- **MetalFX:** requires Metal 3.0 / macOS Ventura (all Apple Silicon supports it)
- **Metal 4 unified encoders:** macOS 26+ (2025) — future opportunity, not a dependency

## Key Constraints

- **Resource lifetime:** No wgpu refcounting on native Metal. Must manually ensure textures/buffers survive in-flight command buffers (2-3 frames with triple buffering).
- **Ring buffer overflow:** Uniform ring buffers need either generous sizing or fence-based wraparound protection.
- **MetalFX Temporal:** Needs depth + motion vectors. MANIFOLD is 2D — only available when WireframeDepth effect is active (provides depth + flow). Spatial Scaler works unconditionally.
- **Profiling needed:** Still at ~40 FPS despite theoretical ~14.5ms savings. Must profile with Metal Instruments before further optimization to verify current wins and identify actual bottleneck.

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