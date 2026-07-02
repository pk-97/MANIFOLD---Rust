# Vulkan Backend — Design & Implementation Contract

**Status:** APPROVED design, not implemented (Phase 0 scaffold shipped in 0c5dde17).
**Decided 2026-07-02:** cross-platform is 100% required — MANIFOLD is a Mac, Windows, Linux application.
**Supersedes** the "Windows / Linux Backend" section of `MANIFOLD_GPU_ARCHITECTURE.md` — there is **no wgpu interim step**. The Vulkan backend is native `ash` from day one, same as Metal went native `objc2-metal`.

This document is the implementation contract for a Sonnet-class agent. Every judgment call is already made — the tables below say what to build, in what order, and what the oracle is at each step. Where the doc says DECIDED, do not re-litigate; where it says VERIFY, the parity suite is the referee.

---

## 1. Where the port actually starts (audit, 2026-07-02)

The port is further along than "add a Vulkan backend" suggests. Already true on `main`:

| Piece | State | Where |
|---|---|---|
| Backend selection | DONE — cfg-gated, exactly one backend per build, `--features vulkan` | `crates/manifold-gpu/src/lib.rs` |
| Shader pipeline | DONE — WGSL → naga → SPIR-V → spirv-opt is backend-neutral and always compiled; only the final "SPIR-V → X" step diverges | `src/shader_common.rs` |
| Vulkan scaffold | DONE — `SlotMap`, `compile_wgsl_to_spirv_{compute,render}`, `build_slot_map`, type shells | `src/vulkan/` (commit 0c5dde17) |
| `ash` dependency | DONE — pinned 0.38, optional, feature-gated | `Cargo.toml` |
| Barrier seam | API EXISTS — `GpuEncoder::pipeline_barrier(reads, writes)` is a documented no-op on Metal with Vulkan semantics spelled out in its doc comment. **No call sites exist yet** — see §4 for why that's fine | `src/metal/encoder.rs:1516` |
| Metal type leakage outside the crate | NONE — no `metal::`/`objc2` types escape `manifold-gpu` except the cfg(macos) escape hatches (`raw_device_ptr` for DNN FFI, IOSurface fns) which are already platform-gated at their call sites | verified by sweep |
| Specialized pipelines | ALREADY PORTABLE — "specialization" is WGSL source string substitution before compile (`device.rs:504`), not Metal function constants. Works identically on Vulkan | `src/metal/device.rs:502-534` |
| Headless testing | ALREADY TRUE — parity harness and gpu_tests call `GpuDevice::new()` with no window. The whole oracle runs headless | `tests/parity/harness.rs` |

**What remains is exactly three files of real work** — `vulkan/device.rs`, `vulkan/encoder.rs`, `vulkan/types.rs` — plus presentation (Phase 3, mostly `manifold-app`) and the platform-services inventory (§8, separate designs).

**Not ported, ever** (zero consumers outside the crate, verified by sweep):
- `metal/mps.rs` (7 MPS kernels) — dead API, kept Metal-side for future use
- `metal/fft.rs` (`GpuFft`, MPSGraph) — no consumers yet; when FFT primitives land they need a portable story (VkFFT-style compute or a Rust FFT upload), design then
- `metal/metalfx.rs` — `manifold-renderer/src/fsr1.rs` is the portable upscaler; MetalFX stays a macOS bonus
- `GpuHeap` — no external consumers; heap sub-allocation was a deferred Metal optimization. Skip. If it lands later, the Vulkan twin is a `gpu-allocator` pool
- Xcode capture scopes (`install_device_capture_scope` etc.) — no-ops on Vulkan; RenderDoc attaches externally

---

## 2. Policy decisions (all DECIDED)

| # | Decision | Choice | Why |
|---|---|---|---|
| D1 | Bindings | `ash` 0.38, raw Vulkan. No wgpu, no vulkano | Zero abstraction tax — the same reason Metal went `objc2-metal`. Already pinned |
| D2 | Version floor | **Vulkan 1.3**, hard requirement. Features required at boot: `dynamicRendering`, `synchronization2`, `timelineSemaphore`, `VK_KHR_push_descriptor` | Every desktop driver since ~2022 (NVIDIA, AMD, Intel, Mesa) ships 1.3; MoltenVK is 1.3-conformant since 2024. Missing feature = loud boot error naming it — no fallback paths to maintain |
| D3 | Memory | `gpu-allocator` crate (Rust VMA equivalent, MIT/Apache, production use in Bevy et al) | Sub-allocation, dedicated-allocation heuristics, and leak reporting for free; hand-rolling VkDeviceMemory management is the classic self-inflicted wound |
| D4 | Descriptors | **Push descriptors** (`vkCmdPushDescriptorSetKHR`), single set 0 | Maps 1:1 onto the per-dispatch `&[GpuBinding]` model — no pools, no free-list lifetime bugs. naga already emits `(set=0, binding=N)` matching WGSL `@binding(N)` |
| D5 | `GpuBinding::Bytes` | Host-visible **uniform ring buffer** per encoder (bump allocator, 256-byte aligned, ~4 MB, grow-with-warn on overflow) | Metal's `set_bytes` has no Vulkan twin; push constants would require shader changes (WGSL declares these as uniform buffers). Ring is the standard answer |
| D6 | Synchronization | **Automatic hazard + layout tracking inside `GpuEncoder`** (§4). `pipeline_barrier` stays a public no-op hint on both backends | The single most important decision in this doc — rationale in §4 |
| D7 | Queues | One graphics+compute queue family. Content and UI each get a `VkQueue` if the family has ≥2, else one queue behind a mutex. Submission always mutex-serialized | Metal's "async compute" (Phase 9) is parallel *recording*, single-queue submission — same shape. Multi-queue is a future optimization, not part of the port |
| D8 | Command pools | Pool-per-encoder from a free-list on `GpuDevice`; pool is reset and recycled when its fence/timeline value retires | Vulkan pools aren't thread-safe and encoders record on arbitrary threads (compositor async compute, LED readback) |
| D9 | `GpuEvent` | **Timeline semaphore** | 1:1 with `MTLSharedEvent`: `signal_event_value` → timeline signal, `wait_event` → timeline wait, CPU-side counter → `vkGetSemaphoreCounterValue` |
| D10 | Completion callbacks | One waiter thread per device: every `commit` signals a device-global timeline value; the thread `vkWaitSemaphores`-es forward and dispatches `add_completed_handler` callbacks in order. `GpuFenceWaiter` rides the same thread | Vulkan has no completion callbacks. A single waiter thread is the standard pattern; callbacks already run off-thread on Metal so caller expectations don't change |
| D11 | Shaders | SPIR-V from `shader_common` → `vkCreateShaderModule`, unmodified. `arrayLength()` uses SPIR-V's native `OpArrayLength` — **the naga sizes buffer is a Metal/MSL-only concept and is never emitted on Vulkan** (`needs_sizes_buffer` stays `false`; `SIZES_BUFFER_BINDING` stays as an inert constant for API parity) | The `buffer_size_buffer_index` pin was a SPIRV-Cross/MSL workaround; Vulkan doesn't have the problem |
| D12 | f16 | `use_half` SPIR-V passes work as-is when the device reports `shaderFloat16`; otherwise compile the f32 variant of that pipeline and log once at boot | Capability adaptation, not a silent fallback — visuals identical, only ALU throughput differs |
| D13 | Pipeline caching | In-memory cache keyed by the existing `archive::pipeline_hash`; on disk, **`VkPipelineCache` blob** (replaces `MTLBinaryArchive`) + a **SPIR-V disk cache** mirroring `msl_cache.rs` (skips naga + spirv-opt on warm boot) | Same three-layer structure as Metal, same cache keys, same load/save call sites (`load_pipeline_archive`/`save_pipeline_archive` map directly) |
| D14 | Render passes | **Dynamic rendering only** (`vkCmdBeginRendering`). No `VkRenderPass` objects anywhere | Matches Metal's descriptor-per-pass model and the existing `begin_render_pass`/`end_render_pass` API exactly |
| D15 | Viewport/scissor | Dynamic state (`VK_DYNAMIC_STATE_VIEWPORT`/`SCISSOR`); Y-flip via **negative viewport height** (`y = height, height = -height`) | Keeps SPIR-V byte-identical across backends (cache-friendly, no per-backend naga flags). Standard DXVK/vkd3d technique. WATCH: winding order flips with the viewport — MANIFOLD's 2D paths don't cull, but `render_3d_mesh` must set front-face to match Metal's behavior. Parity tests are the referee |
| D16 | Storage modes | `Private` → device-local · `Shared` → host-visible + coherent, persistently mapped · `Managed` → not offered (macOS concept; no Vulkan callers) · `Memoryless` → `LAZILY_ALLOCATED` when the heap exists (mobile/TBDR), plain device-local otherwise — correctness identical, only bandwidth differs on desktop | |
| D17 | Presentation | `GpuSurface` → `VkSurfaceKHR` + `VkSwapchainKHR` via `ash-window` + winit raw-window-handle. MAILBOX when available, FIFO otherwise (both are correct; MAILBOX matches the latest-frame triple-buffer semantics). EDR/HDR: **SDR first**; Windows HDR (`VK_EXT_swapchain_colorspace`) is a later, separate item. `presents_with_transaction`, `contents_gravity` → no-ops (CAMetalLayer concepts) | |
| D18 | Cross-thread texture sharing | On Vulkan there is **one `VkInstance`/`VkPhysicalDevice`/`VkDevice` per process** (`Arc`-shared core; `GpuDevice::new()` on the second thread clones it and takes its own queue). The IOSurface triple-buffer bridges become plain `VkImage`s + timeline-semaphore handoff — same atomic `front_index`, explicit sync instead of IOSurface's implicit sync | IOSurface exists to share across two `MTLDevice`s; a shared `VkDevice` makes the whole cross-API machinery unnecessary. External-memory extensions NOT needed |
| D19 | Profiling | `GpuTimestampSampler`/`GpuFrameProfile` → `VkQueryPool` timestamp queries scaled by `timestampPeriod`; `supports_dispatch_profiling` → `timestampComputeAndGraphics` | Direct mapping |
| D20 | Dev iteration on macOS | MoltenVK (`brew install molten-vk`, or Vulkan SDK; point `VK_ICD_FILENAMES` at the MoltenVK ICD). Not a ship path — a way to run the Vulkan backend + full parity suite on the dev box before touching Windows/Linux hardware | Already stated in the scaffold's module docs |

---

## 3. API contract

The Vulkan module must export **exactly** the backend-neutral surface of `metal/mod.rs:28-37` minus the Metal-only islands (§1). Anything in shared code that touches a Metal-only API must fail to compile under `--features vulkan` — that compile error is the enforcement mechanism, and CI builds both features (§7).

### 3.1 `GpuDevice` (from `metal/device.rs`)

| Method | Vulkan implementation | Phase |
|---|---|---|
| `new()` | Instance (portability enumeration flag for MoltenVK) → pick discrete-then-integrated physical device → device with D2's features → queue(s) → `gpu-allocator` → linear sampler. Process-global `Arc` core per D18 | P1 |
| `device_name()` | `VkPhysicalDeviceProperties::deviceName` | P1 |
| `create_texture(desc)` | `VkImage` + `VkImageView` via allocator; usage flags from `GpuTextureUsage` (RENDER_TARGET→COLOR_ATTACHMENT, SHADER_READ→SAMPLED, SHADER_WRITE→STORAGE, COPY_SRC/DST→TRANSFER_SRC/DST; CPU_UPLOAD→staging path per §5). Initial layout UNDEFINED; tracker owns it from there | P1 |
| `create_buffer(size)` / `create_buffer_shared(size)` | Device-local / host-visible+coherent persistently mapped (`mapped_ptr` already stubbed in `vulkan/types.rs`) | P1 |
| `create_sampler(desc)` | `VkSampler`; `ClampToZero` → `CLAMP_TO_BORDER` with transparent-black border; `compare` → `compareEnable` + op | P1 |
| `upload_texture(tex, data)` | Staging buffer → one-shot command buffer → copy → wait. (Metal's `replaceRegion` is synchronous; match that) | P1 |
| `create_compute_pipeline[_half]` | `shader_common` SPIR-V → shader module → pipeline layout from `SlotMap` (push-descriptor set layout) → `VkComputePipeline`. Cache per D13. Workgroup size comes from naga reflection exactly as the scaffold's `compile_wgsl_to_spirv_compute` returns it | P1 |
| `create_specialized_*` | Identical string-substitution wrappers as Metal (`device.rs:504`) — copy verbatim | P1 |
| `create_render_pipeline{,_msaa,_with_vertex_layout,_depth}` | `VkGraphicsPipeline` with `VkPipelineRenderingCreateInfo` (dynamic rendering), blend/format/msaa/depth from args, dynamic viewport+scissor, vertex input from `GpuVertexLayout` | P2 |
| `create_depth_stencil_state` | Fold into pipeline state (Vulkan has no separate DS object); `GpuDepthStencilState` becomes a plain config struct the pipeline ctor consumes | P2 |
| `create_encoder(label)` | Command pool from free-list (D8) + primary command buffer, begin | P1 |
| `create_event()` | Timeline semaphore (D9) | P1 |
| `create_texture_pool(fif)` | `TexturePool` is backend-neutral bookkeeping over `create_texture` + frame stamps — port the ~290 lines essentially verbatim | P1 |
| `create_texture_memoryless` / `_msaa_memoryless` | D16 | P2 |
| `load_pipeline_archive`/`save_pipeline_archive`/`load_msl_cache`/`log_msl_cache_stats` | `VkPipelineCache` blob + SPIR-V disk cache (D13), same signatures | P1 |
| `linear_sampler`, `create_timestamp_sampler`, `supports_dispatch_profiling` | D19 | P1/P2 |
| `raw_device`, `raw_queue`, `clone_queue`, `raw_device_ptr`, capture scopes, `create_io_surface_bgra8`, `create_texture_from_io_surface` | **Not exported on Vulkan.** All existing call sites are already `#[cfg(target_os = "macos")]` or inside Metal-only files | — |

### 3.2 `GpuEncoder` (from `metal/encoder.rs`)

Metal's encoder juggles compute/render/blit sub-encoders (`end_current`/`ensure_compute`); Vulkan records everything into **one `VkCommandBuffer`** — the sub-encoder machinery disappears, replaced by the hazard tracker (§4) at each command.

| Method | Vulkan implementation | Phase |
|---|---|---|
| `dispatch_compute(pipeline, bindings, workgroups, label)` | Tracker barriers for `bindings` → bind pipeline → `vkCmdPushDescriptorSetKHR` (buffers/textures/samplers; `Bytes` via ring, D5) → `vkCmdDispatch`. Debug label via `VK_EXT_debug_utils` when present | P1 |
| `compute_memory_barrier_buffers()` | Global `vkCmdPipelineBarrier2` (compute→compute, SHADER_WRITE→SHADER_READ) | P1 |
| `clear_texture` / `clear_buffer` | Tracker → `vkCmdClearColorImage` / `vkCmdFillBuffer(0)` | P1 |
| `copy_texture_to_texture` / `copy_buffer_to_buffer` / `copy_texture_to_buffer` / `copy_texture_3d_to_buffer` | Tracker → `vkCmdCopyImage` / `vkCmdCopyBuffer` / `vkCmdCopyImageToBuffer`. **Preserve exact crop semantics** — `copy_texture_to_texture` crops, it does not scale (see `copy-texture-crops-use-resize-sample` memory) | P1 |
| `upload_texture` | Staging chunk from the encoder ring → `vkCmdCopyBufferToImage` | P1 |
| `generate_mipmaps` | Successive `vkCmdBlitImage` per mip level with per-level barriers (the standard loop) | P2 |
| `draw_fullscreen{,_viewport}`, `draw_instanced{,_msaa,_depth,_depth_ex}`, `draw_indexed` | Tracker transitions attachments → `vkCmdBeginRendering` → dynamic viewport (negative height, D15) + scissor → bind + push descriptors → draw → end. Mirror Metal's pass-per-call structure exactly | P2 |
| `begin_render_pass` / `draw_in_render_pass` / `set_scissor_rect` / `end_render_pass` | Same, with the pass held open across `draw_in_render_pass` calls | P2 |
| `signal_event[_value]` / `wait_event` | `VkTimelineSemaphoreSubmitInfo` on the *submit* (Vulkan signals/waits at submit boundaries, not mid-buffer). The encoder records pending signal/wait lists; `commit` attaches them. Mid-encoder `wait_event` splits the submission — matches Metal's `end_current` + encodeWait semantics | P1 |
| `commit`, `commit_and_wait_scheduled`, `commit_and_wait_completed[_timed]` | End CB → mutex-guarded `vkQueueSubmit2` with device timeline signal (D10) → optional `vkWaitSemaphores`. `_timed` uses begin/end timestamp queries | P1 |
| `add_completed_handler[_with_status]`, `add_gpu_time_handler` | Registered on the waiter thread against this submit's timeline value (D10) | P1 |
| `enable_dispatch_profiling`, `set_profile_tag`, `commit_and_wait_profiled` | Timestamp query pool spans (D19) | P2 |
| `pipeline_barrier(reads, writes)` | Keep as no-op hint (§4). Optionally use it to pre-warm tracker state — never required for correctness | P1 |
| `raw_cmd_buf` | Not exported on Vulkan | — |

---

## 4. Hazard & layout tracking (the one new piece of engineering)

**The problem.** Metal guarantees intra-queue ordering; every call site in MANIFOLD (chain runtime, compositor, readbacks) is written against that guarantee. Vulkan guarantees nothing between commands without explicit `vkCmdPipelineBarrier2` + image layout transitions. `pipeline_barrier` exists as an API but has **zero call sites** — and retrofitting explicit barriers into every caller would be unverifiable (a missed barrier is a heisenbug on someone else's GPU at a gig).

**DECIDED: the Vulkan `GpuEncoder` tracks resource state internally and emits barriers automatically.** The encoder is the right place because it already sees every access: `GpuBinding` slices name every buffer/texture a dispatch touches (with `writable` known from the `SlotMap`), render passes name their attachments, copies name src/dst. This makes every existing call site correct with zero modification, and new call sites can't forget anything.

Mechanics:

- **Layout lives on the texture.** `GpuTexture` (Arc-backed, `Clone` = refcount, mirroring Metal's retain) carries an `AtomicU32` current `VkImageLayout`. Global, because textures cross encoders and threads (triple buffers, async compute); an encoder inheriting a texture must know the layout the last encoder left it in.
- **Access state lives on the encoder.** An `AHashMap<resource-id, LastAccess { stage, access }>`, reset per encoder. Metal's encoder already keeps per-encoder bind caches — same pattern, same invalidation discipline (see `encoder-bind-cache-invalidation` memory).
- **First touch is conservative.** First time an encoder sees a resource, barrier from `ALL_COMMANDS / MEMORY_WRITE` — correct regardless of what any earlier encoder did, because same-queue submission order lets a barrier in this command buffer cover commands in previously submitted ones. After first touch, tracked state gives precise stage/access masks.
- **Before each command:** for every resource about to be accessed, if (previous access included a write) or (layout must change): record a `VkImageMemoryBarrier2`/`VkBufferMemoryBarrier2`; batch all of them into one `vkCmdPipelineBarrier2` per command. Read-after-read never barriers.
- **Layout targets:** storage read/write → `GENERAL` · sampled → `SHADER_READ_ONLY_OPTIMAL` · color attachment → `COLOR_ATTACHMENT_OPTIMAL` · depth → `DEPTH_ATTACHMENT_OPTIMAL` · copy → `TRANSFER_{SRC,DST}_OPTIMAL` · present → `PRESENT_SRC_KHR`. If a resource is both sampled and storage in one dispatch, `GENERAL` wins.
- **Cost:** one or two hash lookups per binding per dispatch — noise next to a dispatch. No allocation after warmup (pre-sized maps, per hot-path discipline).
- **Threading:** parallel encoders (async compute) touch disjoint resources by contract — that contract is unchanged from Metal. The atomic layout on shared textures plus `GpuEvent` timeline waits (already in the compositor/LED paths) cover the sanctioned crossings.

`pipeline_barrier` remains a no-op hint on both backends. Do not make correctness depend on callers.

---

## 5. Buffer upload paths

- `create_buffer_shared` + `mapped_ptr` writes — already the uniform hot path; on Vulkan this is persistent-mapped host-visible memory, identical usage. On desktop discrete GPUs this lands in BAR/system memory exactly like Metal's Shared on unified memory — no code change for callers.
- `CPU_UPLOAD` textures (`replaceRegion` on Metal) — Vulkan textures are never host-mapped; route through the staging ring transparently. Callers keep the same API.
- Readbacks (`copy_texture_to_buffer` + wait + read `mapped_ptr`) — unchanged; buffer must be host-visible, which `create_buffer_shared` already guarantees.

---

## 6. Phasing (each phase compiles, tests green, commits)

**P1 — headless compute core.** `vulkan/{device,encoder,types}.rs`: device bring-up, allocator, buffers/textures/samplers, compute pipelines + caches, dispatch + copies + clears + upload, hazard tracker, timeline events + waiter thread, `TexturePool` port.
*Oracle:* per-primitive gpu_tests + compute-path parity tests, `cargo test -p manifold-renderer --features manifold-gpu/vulkan` under MoltenVK on the dev box. Every parity failure is a Vulkan-backend bug until proven otherwise — never adjust tolerances or shared WGSL to pass (per `shared-shader-topology`, `value-level-parity`).
*Mandatory from P1 on:* the parity suite must also run with the **Khronos synchronization validation layer** enabled (`VK_LAYER_KHRONOS_validation` + sync-val setting), zero errors. MoltenVK sits on Metal, whose implicit ordering can visually mask a missing barrier that would corrupt frames on NVIDIA/AMD — sync-val catches those at the API level regardless of the hardware underneath. A green parity run without sync-val proves nothing about the hazard tracker.

**P2 — render + profiling.** Graphics pipelines (dynamic rendering), the `draw_*` family, depth/MSAA/mipmaps/scissor, negative-viewport Y-flip, timestamp profiling.
*Oracle:* full parity suite + `ui_color_swatches` + headless-PNG goldens (`reference_ui_headless_png_verification` memory). Explicitly check orientation (Y-flip) and winding on `render_3d_mesh`.

**P3 — presentation + app bring-up (mostly `manifold-app`).** `GpuSurface` on swapchain (D17), shared-device restructure of the IOSurface triple-buffer bridges (D18), frame pacing port: `mach_wait_until` + 2 ms spin → `clock_nanosleep(TIMER_ABSTIME)` + spin on Linux, high-resolution waitable timers + spin on Windows; CVDisplayLink → swapchain-present-driven cadence (`VK_GOOGLE_display_timing`/`VK_EXT_present_timing` where exposed, plain FIFO pacing otherwise).
*Oracle:* app boots, plays the canonical Liveschool fixture, frame-pacing telemetry matches the Metal profile shape.

**P4 — platform services.** Each row of §8 is its own design/implementation unit gated on P3. None block the GPU port.

**CI matrix from P1 onward:** macOS-Metal (existing) · macOS-MoltenVK (parity on real Apple GPU) · Linux with lavapipe (Mesa software Vulkan 1.3 — CI-only; expect a small tolerance profile for software rasterization, kept separate from GPU tolerances) · Windows when hardware runners exist.

**Perf-portability watch items** (correct on all hardware; these are about not losing frames on discrete GPUs):

- **Readbacks cross PCIe on discrete GPUs.** Apple Silicon unified memory makes GPU→CPU readback ~free; on a discrete card `copy_texture_to_buffer` + wait is a PCIe round trip. The per-frame readback paths — LED/Art-Net sampling (`manifold-led`), audio-reactive analysis, thumbnails — must stay **async with a frame of latency** (the `GpuEvent` pattern the LED path already uses). Never "simplify" a readback into a synchronous commit-and-wait on the frame loop; that's a per-frame stall on every PC.
- **Windows presentation must reach independent flip.** The DWM compositor adds a frame of latency and pacing jitter unless borderless-fullscreen presentation qualifies for independent flip (correct swapchain size/format/alpha, no occlusion). This is the Windows twin of the macOS Direct Display work (`direct-display-cadence`) and is show-critical — verify with PresentMon during P3, not by eyeball.

---

## 7. Guardrails for the implementing agent

- **No wgpu. No vulkano.** `ash` only. This is a hard rule, same force as CLAUDE.md's "no wgpu."
- **Never modify `metal/`, shared WGSL, or parity tolerances to make Vulkan pass.** Metal's output is the oracle. Divergence = Vulkan bug until root-caused otherwise.
- **No silent fallbacks** (per `no-silent-fallbacks-or-interim-stopgaps`): a missing D2 requirement is a boot error naming the feature. The only sanctioned capability adaptations are the documented ones: Memoryless→device-local (D16), f16→f32 (D12), MAILBOX→FIFO (D17) — each logs once at boot.
- **Public surface stays identical.** A change to any shared signature must compile under both features and be called out in the PR.
- **cfg discipline:** the `vulkan` cargo feature is the only backend selector. No `#[cfg(target_os)]` backend forks in shared crates.
- **Both-features build in CI:** `cargo clippy -p manifold-gpu` and `cargo clippy -p manifold-gpu --features vulkan` (plus dependents) both gate — the compile error when shared code touches a Metal-only API is the enforcement mechanism for §3's "not exported" rows.
- **Commit cadence:** per phase or finer; parity suite green before each push (workspace sweep rules per CLAUDE.md apply — this is infrastructure, so full sweeps are warranted at phase boundaries).

---

## 8. Tier 2 — platform coupling beyond the GPU (inventory, one page)

Verified by sweep 2026-07-02. Each row is a separate future design; none block P1–P3.

| Subsystem | macOS today (where) | Cross-platform answer | Effort |
|---|---|---|---|
| Video decode | AVFoundation/VideoToolbox (`manifold-media/src/decoder*.rs`, `decode_scheduler.rs`) | FFmpeg/libavcodec with hwaccel (D3D11VA on Windows, VAAPI on Linux, NVDEC where present) behind the existing decoder seam. Keep VideoToolbox on macOS — it's zero-copy into Metal and works | Large — own design doc |
| Encode / export / recording | VideoToolbox (`manifold-media/src/metal_encoder.rs`, `manifold-recording`) | FFmpeg encode: NVENC/AMF/QSV hardware paths, x264 fallback | Large — same design doc as decode |
| Text rendering | CoreText (`manifold-renderer/src/text_rasterizer.rs`, `native_text.rs`, `render_text` primitive; `manifold-ui/src/text.rs`) | **cosmic-text** (fontdb + rustybuzz + swash) — pure Rust, shaping + fallback + rasterization, the current SOTA Rust text stack | Medium |
| Audio capture | cpal input devices (portable already) + CoreAudio process/system taps (`manifold-audio`, macOS 14.4+) | Taps: WASAPI loopback (Windows — easier than the macOS version was), PipeWire monitor sources (Linux). cpal input path needs nothing | Medium |
| Frame pacing / display link | `mach_wait_until`, CVDisplayLink (`manifold-app/src/display_link.rs`, `frame_timer.rs`) | §6 P3 | In P3 |
| Screen capture source | ScreenCaptureKit (tv-led-mirror path) | Windows.Graphics.Capture / PipeWire screencast | Small-medium |
| DNN plugins | Vision/CoreML via FFI (`manifold-native`, `raw_device_ptr` interop) | ONNX Runtime backend — **already designed**, ML nodes design P6 | Designed |
| Camera | AVFoundation | Media Foundation (Windows) / V4L2 (Linux) — evaluate `nokhwa` first | Small-medium |
| EDR / HDR output | `edr_surface.rs`, CAMetalLayer EDR | SDR first (D17); Windows HDR10 via swapchain colorspace later | Deferred |
| MIDI / OSC / Art-Net | midir / UDP | Already portable | None |
| Windowing / input | winit | Already portable; audit macOS-specific chrome (menus, dock) during P3 | In P3 |

---

## 9. Decided items (do not reopen)

1. Native `ash` Vulkan, no wgpu interim — supersedes the old `MANIFOLD_GPU_ARCHITECTURE.md` strategy section.
2. Vulkan 1.3 hard floor with the D2 feature set; loud boot failure, no fallback matrix.
3. Automatic hazard/layout tracking inside the Vulkan encoder; `pipeline_barrier` stays a no-op hint everywhere.
4. One `VkDevice` per process shared across content/UI threads; IOSurface bridges become VkImages + timeline handoff; no external-memory extensions.
5. Push descriptors + uniform ring for `Bytes`; no descriptor pools.
6. Timeline semaphores for all GPU↔GPU and GPU↔CPU sync; one waiter thread for callbacks.
7. Dynamic rendering only; negative-viewport Y-flip; SPIR-V byte-identical across backends.
8. `arrayLength` via native `OpArrayLength` — no sizes buffer on Vulkan.
9. MPS / MPSGraph FFT / MetalFX / GpuHeap / capture scopes: not ported (no consumers or already-portable alternatives exist).
10. Platform services (§8) are separate designs gated behind P3, ONNX excepted (already designed).
