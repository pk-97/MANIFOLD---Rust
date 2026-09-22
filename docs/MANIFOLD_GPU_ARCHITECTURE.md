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

Render cache identity separates shader translation from pipeline state. The MSL
key includes source, entry points and the point-size output rewrite. Every render
factory uses one complete pipeline key for memory caching and archive insertion:
shader key, color/depth formats, all blend components, sample count,
alpha-to-coverage, ordered auxiliary attachments and the full vertex layout.
Labels and draw-time depth/cull/fill settings are excluded. Versioned key
namespaces leave older ambiguous entries unused; no project migration or manual
cache deletion is required. New descriptor options must extend this key.

**Ownership model:** All Metal objects are owned as `Retained<ProtocolObject<dyn MTLFoo>>` (automatic retain/release via `objc2::rc`). No manual `objc_retain`/`objc_release`, no raw pointer fields on GPU wrappers. Command buffers and encoders are fully typed — no `*mut c_void` cmd_buf tricks.

**Memory preparation:** Retaining a Metal object does not keep its memory ready
for GPU access. The content pipeline owns one `MTLResidencySet` manager and
drains allocation/lifetime changes during warmup and before rendering. Device
textures, buffers, pooled textures and heaps share residency leases across clones
and views; fence retirement retains the lease until submitted work completes.
Other threads enqueue changes without mutating the residency set. External
drawable wraps, memoryless storage and raw RT acceleration structures are not
registered by this manager. Existing resource declarations and hazard tracking
remain authoritative.

Warmup submits outside the ordinary frame loop. After each warmed layer and at
the final loading boundary, with no unsubmitted encoder outstanding, the content
pipeline signals its normal completion event on the same queue and waits for that
checkpoint before draining retirement and residency. Synchronous warmup commits
alone do not advance that event: obsolete images would otherwise remain retained
until playback. Normal playback keeps its nonblocking drain and frame fence.

Within Metal's recommended working-set limit, the manager requests residency
ahead of use and attaches the set to the device's command queue for execution.
The queue attachment carries the set into committed command buffers; a standalone
ahead-of-time request is not a substitute for that execution contract. Over-budget
or empty sets detach from the queue and release their request; re-entering the
budget restores both. Teardown detaches before clearing the set. Budget rejection
is reported and does not prevent project loading. System memory pressure can
still delay GPU access. Warmup telemetry records the request, allocation count,
tracked bytes and budget rather than claiming hitch-free playback.

**Temporary arrays:** `plan_array_allocations` shares an existing physical slot
only between arrays with identical channel layout and byte capacity whose
execution-plan lifetimes do not overlap. Outputs acquire storage before the
current step releases inputs. Each exact layout/capacity bucket retains all distinct
free roots; an explicit alias removes its root from reuse. Array inputs and outputs
of `NonGpu` and `IoBridge` nodes remain dedicated: CPU evaluation can finish before
queued GPU reads/writes execute, and GPU hazard tracking does not order mapped
CPU access. Held, persistent, prebound, atomic, explicit
in-place, canvas-dependent and carried/exported resources remain dedicated. Buffers stay allocated
for the runtime; ordered encoding and native hazard tracking govern GPU access.
Executor storage revisions detect overwritten cached outputs. This changes
neither cross-runtime sharing nor GPU retirement/residency policy.

**RT filter scratch:** The post-accumulation irradiance filter reuses
`rt_irr_full_b` and `rt_normal_full_b` after the pre-accumulation filter's final
GPU reads. These are compatible full-render RGBA16 targets, with no CPU access.
History pairs, current full normal/depth guides, moments and raw capture targets
remain dedicated. The post-filter result lives through the composite; next-frame
scratch writes follow it in the same queue. Native hazard tracking also covers
existing command-buffer checkpoints. Resize refreshes both aliases with their
backing. This removes two allocations without adding waits or changing history.

**RT firefly resolve:** `rt_firefly_scratch` shares the renderable RGBA16
reflection-prefilter scratch `rt_refl_full_b`. The second prefilter pass reads
that backing before the forward scene pass resolves into it; the firefly clamp
then reads scene color and writes a distinct output. Current reflection output,
all histories and the post-filter pair remain separate. These are GPU-only,
ordered uses, including across command-buffer checkpoints. Initial allocation,
trace-size changes and render-size changes refresh the alias together. This
removes one physical texture without changing the clamp or its activation gates.

**RT scalar history:** Each shadow-visibility group keeps its own ping-pong
snap-hold countdown in `R16Float`. Accumulation reads and writes only the red
channel, retaining the previous half-float precision. Both histories remain
persistent and separate; reprojection prevents sharing their backing. The scalar
textures permit render-target clears for diagnostic sentinel injection. Capture
decoders return zero for the absent channels.

**Immutable imported images:** glTF source uploads may share an immutable
RGBA8 texture within one execution thread and device resource scope. The key
includes the decoded pixel SHA256, source dimensions and colour format. Hashing
runs in the existing decode worker. A cache miss publishes only after synchronous
CPU upload completes; published textures are never uploaded into again.
The cache holds weak references, prunes expired entries on upload lookup, and
does not retain image memory after the last source owner releases it. A unique
`GpuDevice::resource_scope_id` prevents sharing across independent queue,
residency and retirement owners, including after a renderer is recreated.

glTF conversion/mipmap outputs can also be immutable and shared. The key extends
source content identity with output dimensions, format, mip count and repack mode.
Each producer writes a fresh texture on a cache miss and signals a GPU event after
conversion and mip generation. Another producer may adopt it only after that
event is complete; it never waits for an unsubmitted command buffer. Concurrent
initial misses may temporarily allocate separate images, then converge on the
ready shared image. Empty or pending sources publish an opaque-black image.

`EffectNode::provides_texture_output` reserves a dedicated held slot with a
descriptor instead of a duplicate writable render target. The executor publishes
`provided_texture_output` after evaluation and before revision commit or downstream
reads. `NodeOutputs::texture_2d` does not expose these images as writable outputs.
The backend never puts them in a writable pool or feedback swap. Feedback
back-edges and host-prebound destinations retain the ordinary writable path.
Effect-chain slot assignment reserves intermediate provided outputs without
allocating or pinning writable targets; host source and final slots stay writable.
Staged resize keeps compatible immutable images and clears incompatible candidate
bindings without changing live storage. Content versions remain per logical
output; adopting identical pixels in different physical storage changes the
storage identity without inventing a content change. Weak caches retain no image
after their last producer releases it; GPU handles still use normal retirement
and residency leases. This does not add inactive-scene eviction or reduce quality.

**Parked thumbnails:** A cold thumbnail runtime is retained only while its clip
is visible and has no captured atlas cell. Runtime preparation and frame validity
must be complete before exposing its texture. Atlas copy encoding occurs before
runtime eviction; the normal content-frame retirement fence protects in-flight
resources. This does not evict live layer generator state. An atlas cache entry
means capture was encoded, not that the GPU fence has already completed.

**Deleted effects:** Empty authored effect lists release their cached layer,
group, LED-group and master runtimes before the compositor's empty-frame return.
Disabled or zero-amount effects are not deletion. Pool entries and last-use
stamps retain the existing layer-deletion and grace-period pruning policy.

**Resolution changes:** The content thread prepares compositor, upscaler, generator,
effect-chain and Math View replacements before publishing new dimensions. GPU
allocation/admission errors discard the candidate and preserve the live renderer,
project settings and undo/redo history. Compatible non-atomic array storage is
retained; new atomic storage is separately initialized. Math View candidates bind
to their candidate parent's buffers. Commit resets executor readiness and
simulation state only after every owner has prepared successfully. Texture and
buffer admission includes the live/candidate overlap; it does not guarantee that
later lazy primitive allocations will fit. Existing GPU retirement owns resources
still referenced by submitted work.

**All threads use manifold-gpu.** Content thread and UI thread both use native Metal. Zero wgpu anywhere in the codebase.

**Dependency policy:** manifold-gpu pulls only `objc2`, `block2`, `objc2-foundation`, `objc2-metal`, `objc2-metal-fx`, `objc2-metal-performance-shaders`. No `metal` crate, no `objc 0.2`, no `block 0.1`, no `core-graphics-types`. Raw-window-handle is the only non-objc2 macOS dep (winit interop).

## Failure diagnostics

RT à-trous passes bind parameters through inline bytes. Each encoded dispatch
owns its step/settings snapshot; a later pass must not overwrite parameters
that an earlier GPU dispatch has yet to read. The existing buffer argument is
retained for caller compatibility, but these two passes no longer upload it.

Command buffers request Metal encoder execution status. On failure the session
log records encoder labels, error states, and dispatch signposts when Metal
supplies them. RT trace and the `node.render_scene RT*` postprocess stages
(upsample, à-trous, and accumulate) each use a separate labelled compute
encoder, so an RT-A3a trace can be distinguished from a later stage. This
changes encoder boundaries, not submission order, and the labels identify
completed, affected, or pending encoders rather than guaranteeing the exact shader fault or its earlier
resource/synchronization cause. Blocking production warmup uses
`try_commit_and_wait_completed` and propagates GPU failure instead of treating
a logged error as successful work.

### Incident capture

`MANIFOLD_GPU_DIAGNOSTICS=1` enables command-buffer correlation IDs,
creation/scheduling/completion events, available GPU durations, allocation
footprints, RT dispatch sizes, and bound-buffer identities/sizes. This is an
opt-in diagnostic mode: logging and validation overhead may change timing.
Ray validation covers primary, shadow/sun, AO, GI, emissive-shadow and reflection
queries. Invalid inputs are recorded and replaced with a finite short ray only
in this mode. Eight preallocated slots retain their first invalid inputs;
exclusive ownership lasts through completion readback. Failed commands and slot
exhaustion log validation as unavailable. Positive infinite maximum distance is
valid. Geometry logs check flat vertex-buffer extents; indexed hit bounds and
full resource lifetime correctness remain unverified.
Fatal GPU reports embed a bounded session-log tail; the adjacent full session
is the authoritative timeline. Buffer states and missing GPU timestamps are
reported as evidence, not inferred causes. Resource metadata does not prove
lifetime safety, and driver-internal traversal is not observable here.

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
Vulkan; there is no wgpu interim step. Phase 0 scaffolding (`vulkan/` module, cfg-gated backend selection, shared
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

Metal executes command buffers from the same queue in **commit order**. Per-layer CBs read generator textures — so the generator CB MUST be committed BEFORE per-layer CBs. If generators and effects were on the same command buffer, per-layer CBs would be committed before the generator writes were visible, causing:
- Cross-layer texture contamination (effects reading stale/wrong generator output)
- Effects appearing to not apply (reading uninitialized texture)
- Intermittent single-frame glitches at clip boundaries where multiple layers are active

The fix was splitting generators into their own CB (`gen_enc`) committed first. Per-layer CBs are committed next (Metal guarantees they see gen_enc's writes). The compositor CB is committed last and waits on all per-layer MTLEvent signals before blending.

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
