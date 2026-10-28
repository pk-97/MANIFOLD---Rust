# Vulkan Backend — Design (Linux first)

**Status:** design approved, not built. The `vulkan/` module is a stub and `manifold-gpu` does not compile with `--features vulkan` (`shadow_caster.rs:87` calls `create_buffer_shared`, which the stub lacks).
**Order:** Linux first (Wayland and X11), Windows later. The Linux machine (RTX 5080, Ryzen 9 9950X3D) is on order; until it arrives everything is built and tested on the Mac through a Vulkan driver that runs on Metal, and real NVIDIA performance comes last.
**Who reads this:** the lead, who writes worker specs from it. This file holds the rules and decisions. The inventory (what breaks, file by file) and the work order live in `LINUX_VULKAN_PORT_MAP.md`; re-check it at the start of every phase, the Metal side keeps moving.

---

## 1. What "done" means

MANIFOLD runs a full show on Linux with the Vulkan backend: generators, effects, scenes with ray tracing, water, video clips, export and recording, audio input, LED output. Every GPU proof that passes on Metal passes on Vulkan with the same values, under the Khronos synchronization validation layer with zero errors.

---

## 2. Where the port starts

Backend selection and the shader front end are done: `lib.rs` picks `metal/` or `vulkan/`, and WGSL → naga → SPIR-V → spirv-opt is shared (`shader_common.rs`, `workgroup_zeroing.rs`), so only "SPIR-V → MSL" is Metal's. Everything else, the API surface, Metal-only callers, shaders, app and crate inventory, and the work order, is in `LINUX_VULKAN_PORT_MAP.md`.

---

## 3. Rules

- **`ash` 0.38 only.** No wgpu, no vulkano. `gpu-allocator` 0.28 (built on ash 0.38) for memory, `ash-window` for surfaces. ash 0.38's headers are Vulkan 1.3.281: `VK_EXT_present_timing` needs a newer ash and waits until it ships.
- **Backend differences live inside `manifold-gpu`.** Shared crates never check which backend is active. A Metal-only feature becomes a backend-neutral API with a capability query (`denoiser_available()` returns false on Vulkan until it is built). A feature that is off logs once at boot, by name.
- **`target_os` gates only OS services:** text, video, audio, window chrome, paths. Small services live in `manifold-platform` and big ones keep one seam in their own crate (port map section 11); shared code never writes `cfg(target_os)`. An OS service that touches raw GPU objects (`manifold-media`, `manifold-recording`) also forwards a `vulkan` feature to pick its implementation.
- **Raw handles stay Metal-only.** `raw_device`, `raw_cmd_buf`, `raw_ptr`, `raw_device_ptr`, `mtl4_bridge` are not exported on Vulkan. Their callers are macOS-gated today; the compile error under `--features vulkan` enforces it.
- **Metal is the oracle.** Never change `metal/`, shared WGSL or test tolerances to make Vulkan pass. A difference is a Vulkan bug until shown otherwise.
- **During construction,** a Vulkan method not built yet panics with `todo!("vulkan P2")`. The phase that builds it deletes the `todo!`. None survive P3.
- **Both builds stay green:** `cargo clippy -p manifold-gpu --tests -- -D warnings` with and without `--features vulkan`, and the same for each crate a phase touches.

---

## 4. Decisions

| # | Decision | Choice and reason |
|---|---|---|
| D1 | Version floor | **Vulkan 1.3**, required at boot: `dynamicRendering`, `synchronization2`, `timelineSemaphore`. Missing = boot error naming the feature. NVIDIA, Mesa and both Mac drivers ship 1.3+ |
| D2 | Descriptors | **One descriptor set per dispatch or draw, from a per-encoder pool, written with an update template built from the pipeline's `SlotMap`.** The pool resets and recycles with the encoder's command pool when its submit retires. Push descriptors were the old plan, but NVIDIA caps them at 32 per set and the scene renderer needs 53. One path for every pipeline, no cap |
| D3 | `GpuBinding::Bytes` | Host-visible uniform ring per encoder: bump allocator, 256-byte aligned (`replay::BYTES_ALIGN` already uses 256), grows with a warning on overflow |
| D4 | Ordering | **Automatic hazard and layout tracking inside `GpuEncoder`** (section 5). `pipeline_barrier` stays a no-op hint on both backends |
| D5 | Queues | One graphics+compute queue family. Content, UI and the water sim's compute work each get a `VkQueue` when the family has enough; otherwise they share one behind a mutex. Submission is mutex-serialised per queue |
| D6 | Command pools | Pool per encoder from a free list on the device, reset and reused when its timeline value retires. Encoders record on any thread |
| D7 | `GpuEvent` | Timeline semaphore. `signal_event_value` → signal, `wait_event` → wait (attached at submit; a mid-encoder wait splits the submit), `current_value` → `vkGetSemaphoreCounterValue` |
| D8 | Completion callbacks | One waiter thread per device. Every commit signals a device timeline value; the thread waits forward and runs `add_completed_handler` callbacks in order. `GpuFenceWaiter`, `FrameClock` and `RetireQueue` ride the same timeline |
| D9 | Shaders | SPIR-V from `shader_common` goes to `vkCreateShaderModule` unchanged. `arrayLength` is native `OpArrayLength`; the naga sizes buffer is never emitted. `rebind_vertex_output_as_point_size` (`shader_common.rs:69`) is gated off on Vulkan today; lift the gate in P2, because point-list draws must write `PointSize` |
| D10 | f16 | No shader uses `f16` and no pipeline compiles with `use_half` (`create_compute_pipeline_half` has no callers). Vulkan compiles f32 only. If half precision returns, it needs `shaderFloat16` and 16-bit storage, with an f32 fallback logged once |
| D11 | Pipeline caching | Same three layers and keys as Metal: in-memory by `archive::pipeline_hash`, a `VkPipelineCache` blob on disk in place of `MTLBinaryArchive`, and a SPIR-V disk cache in place of `msl_cache.rs`. Same call sites (`load_pipeline_archive`, `save_pipeline_archive`, `load_msl_cache`) |
| D12 | Render passes | Dynamic rendering only, no `VkRenderPass` objects. Matches `begin_render_pass`/`end_render_pass` |
| D13 | Y-flip | Negative viewport height, so SPIR-V stays identical. Winding flips with it: `DepthMsaaDraw::cull_back_faces` must set the front face to match Metal. Wireframe fill mode needs `fillModeNonSolid` |
| D14 | Storage modes | `Private` → device-local. `Shared` → host-visible, coherent, persistently mapped. `Memoryless` → `LAZILY_ALLOCATED` where it exists, device-local otherwise. `Managed` is not offered |
| D15 | Presentation | `GpuSurface` → `VkSurfaceKHR` + swapchain via `ash-window` (Wayland, X11; `VK_EXT_metal_surface` on the Mac for testing). MAILBOX when offered, else FIFO, logged once. SDR first. `configure_edr`, window level, contents gravity and `presents_with_transaction` report "not supported" on Vulkan |
| D16 | Cross-thread textures | One `VkInstance`/`VkDevice` per process, `Arc`-shared; each `GpuDevice::new()` clones it and takes its own queue. The IOSurface bridge becomes a backend-neutral shared texture (section 6.1). No external-memory extensions |
| D17 | Profiling | `GpuTimestampSampler`, `GpuFrameProfile`, `GpuSpanTimer` → timestamp query pools scaled by `timestampPeriod` |
| D18 | Memory admission | `modifier_memory_snapshot` feeds resource admission (`admit_candidate_bytes`, 39 call sites). Vulkan fills it from `VK_EXT_memory_budget` heap budget and usage. VERIFY in P1 that both Mac drivers expose it |
| D19 | Faults | `VK_ERROR_DEVICE_LOST` maps to `gpu_fault` kinds; `VK_EXT_device_fault` adds detail where present. `firmware_reports` and residency sets have no Vulkan meaning and report empty |
| D20 | Debugging | `VK_EXT_debug_utils` names and labels everything the Metal backend labels. Captures use RenderDoc; Xcode capture scopes are no-ops |
| D21 | Mac drivers | **KosmicKrisp** (Mesa on Metal 4, Vulkan 1.4 conformant, macOS 26+, ships in the LunarG SDK 1.4.363) is the primary test driver; GPU-assisted validation works on it. **MoltenVK** 1.4.2 (Homebrew `molten-vk`) is the second. The instance enables portability enumeration so both are found. Pick one with `VK_DRIVER_FILES` |

---

## 5. Hazard and layout tracking

Metal runs commands on a queue in order; every caller in MANIFOLD relies on that. Vulkan orders nothing between commands without barriers and image layout changes. Adding barriers at the call sites is unverifiable, and a missed one corrupts frames on someone else's GPU. So the Vulkan `GpuEncoder` tracks resource state and emits barriers itself. It already sees every access: bindings name each buffer and texture (writability comes from the `SlotMap`), render passes name their attachments, copies name source and destination.

- **Layout lives on the texture.** `GpuTexture` is `Arc`-backed and carries an `AtomicU32` holding its current `VkImageLayout`, because textures cross encoders and threads.
- **Access state lives on the encoder:** a map from resource id to last `{stage, access}`, reset per encoder, pre-sized so warm frames allocate nothing.
- **First touch is conservative:** a barrier from `ALL_COMMANDS / MEMORY_WRITE`. That is correct whatever an earlier submit on the same queue did. After first touch the tracked state gives exact masks.
- **Before each command,** for each resource it touches: if the last access wrote, or the layout must change, add a barrier. All barriers for one command go into one `vkCmdPipelineBarrier2`. Read after read never barriers.
- **Layout targets:** storage → `GENERAL`; sampled → `SHADER_READ_ONLY_OPTIMAL`; colour attachment → `COLOR_ATTACHMENT_OPTIMAL`; depth → `DEPTH_ATTACHMENT_OPTIMAL`; copies → `TRANSFER_{SRC,DST}_OPTIMAL`; present → `PRESENT_SRC_KHR`. Sampled and storage in one dispatch → `GENERAL`.
- **Across queues** (content → UI through the shared-texture bridge, water compute → render), first-touch barriers are not enough. Each publication records the producer's timeline value; the consumer's submit waits on it. A wait that is already satisfied costs nothing.
- **`compute_memory_barrier_buffers`** stays an explicit global compute→compute barrier, as on Metal.

---

## 6. Backend-neutral seams (shared code changes)

These change Metal-side and app code too, so they come before the app can run on Vulkan. They don't depend on Vulkan internals and can run alongside P1 and P2.

### 6.1 Shared textures (the IOSurface bridge)

`SharedTextureBridge` (`manifold-app/src/shared_texture.rs`, 561 lines) keeps three surfaces, an atomic `front_index`, per-slot read leases retired by completion handlers, and a generation counter. That logic has nothing to do with Metal; only the surface storage and import do.

- `manifold-gpu` gains a shared-texture type: create one on the content device, import it on the UI device. Metal implements it with IOSurface (the creation and import code moves out of `manifold-app`). Vulkan uses the same `VkImage`, because there is one device, plus the timeline-value handoff from section 5.
- The bridge and every pane on it lose their `cfg(target_os = "macos")` gates: about 190 sites in `content_pipeline.rs`, `app.rs`, `frame/present.rs`, `editor_bridge.rs`, `scene_viewport_capture.rs`, `selection_outline.rs`, `perform_mode/render.rs` and the rest.

### 6.2 Metal-only features become capabilities

| Feature | Neutral API | Vulkan until built |
|---|---|---|
| Ray tracing | `raytrace` keeps its types and `ShadowRayTracer` trait on both backends | Tracer reports unavailable; scenes use raster shadow maps. Built in P7 |
| Denoiser | `denoiser_available()` already exists | false. Built in P7 |
| MetalFX spatial and temporal upscale | One upscaler capability in `manifold-gpu`; `manifold-compositor/src/fsr1.rs` stays the portable spatial path | Unavailable. DLSS or FSR in P7 |
| GPU FFT | `GpuFft` | Unavailable; the two nodes report it. Built in P7 |
| Residency sets, firmware reports, capture scopes | Existing calls | Empty or no-op |

The compositor's and nodes-scene's MetalFX wrappers keep their `target_os` gates only until this table lands; then they call the neutral API.

---

## 7. Water: replay spans, gated segments, templates

On Metal, GPU FLIP runs its pressure solve from indirect command buffers. One round is recorded once (`prepare_template`) and the GPU runs it in chunks whose lengths it writes itself (`execute_template`, up to the 900-round cap). The CPU never encodes a round per round. `begin_gated_segment` and `dispatch_compute_gated` do the same for GPU-decided dispatch counts, and node-engine replay spans (`begin_replay`) cache whole stretches.

- **P1 (correct):** replay spans encode directly (`vulkan/replay.rs` already does). A gated dispatch is a plain indirect dispatch from its own `gate`/`gate_offset` arguments. `execute_template` encodes `copies` rounds of those indirect dispatches; the kernels already guard rounds past the stop. Same results, much more CPU encoding.
- **P6 (fast):** one round recorded once into a secondary command buffer; each chunk runs under `VK_EXT_conditional_rendering`, reading the GPU-written range. VERIFY driver support first (NVIDIA has it; check both Mac drivers). Measure against P1 before keeping it.

---

## 8. Platform services on Linux

| Service | Mac today | Linux | Work |
|---|---|---|---|
| UI text | CoreText atlas, `manifold-ui-paint/src/native_text.rs` (2,172 lines) behind `manifold_ui::text::TextMeasure` | cosmic-text 0.19 (harfrust shaping, fontdb, swash raster), embedded Inter | Medium. Without it the UI has no text |
| Text generator | CoreText, `manifold-nodes-image/src/text_rasterizer.rs` (689 lines), system families by name | cosmic-text + fontdb system fonts | Small-medium |
| Video decode | AVFoundation + VideoToolbox plugin (`MetalVideoDecoderPlugin.m` 765 lines), writes into a raw `MTLTexture` | FFmpeg via `ffmpeg-next` 9 with the Vulkan hwcontext on our own `VkDevice`: decoded frames arrive as `VkImage`s (zero copy). CPU decode + upload where the driver lacks Vulkan Video (the Mac drivers), logged once | Large. Needs a decoder trait below `ClipRenderer`; `DecoderPool`/`DecodeScheduler` are concrete today. ProRes decodes on Vulkan compute in FFmpeg 8.1+. HAP decodes on the CPU in FFmpeg |
| Export | AVFoundation encoder plugin (670 lines) | FFmpeg encode: NVENC on NVIDIA, x264/x265 otherwise. `audio_muxer.rs` already shells out to ffmpeg | Medium |
| Live recording | AVAssetWriter plugin (839 lines), ProRes Proxy or HEVC Main10 | FFmpeg encode on the recording thread | Medium |
| System-audio capture | CoreAudio process tap (`process_tap.rs`), behind `CaptureBackend` | cpal 0.18's PipeWire host opens a sink as an input for monitor capture. Upgrade cpal 0.15 → 0.18 everywhere | Small |
| Audio device list | CoreAudio directory | cpal fallback already exists (`fallback.rs`) | None |
| Frame pacing | CVDisplayLink + `mach_wait_until` | Present-driven with `VK_KHR_present_wait`, `clock_nanosleep(TIMER_ABSTIME)` + short spin. The deadline fallback in `frame_timer.rs:266` already runs off-Mac | Small-medium |
| Menus | muda, macOS only (`menu.rs:190`: Linux `init_for_gtk_window` not wired) | muda on GTK, or no menu bar on the show rig | Small |
| Paths, process lock | `~/Library/...` (`main.rs:667`, `clip_thumb_cache.rs:180`), `$HOME/Desktop` (`manifold-recording/src/config.rs:28`) | XDG dirs | Small |
| ML plugins | OpenCV `.bundle` Mach-O plugins via libloading (`manifold-native/src/ffi/mod.rs:12`) | Same plugins built as `.so` | Small |
| Clipboard, dialogs, input | arboard, rfd, winit | Already portable (rfd needs GTK or the XDG portal) | None |
| HDR output | EDR on CAMetalLayer | SDR first | Later |

The Linux compile breaks, file by file, are in `LINUX_VULKAN_PORT_MAP.md` sections 2, 6 and 7.

---

## 9. Phases

Each phase compiles, passes its tests and commits. P3's seams and P4's text work can run alongside P1 and P2, because they touch different files.

| Phase | Builds | Done when |
|---|---|---|
| **P1 Headless compute** | `vulkan/{device,encoder,types}.rs` and siblings: instance and device bring-up (D1, D21), allocator, buffers, textures, samplers, compute pipelines and caches (D11), descriptor sets (D2), uniform ring (D3), dispatch, indirect dispatch, copies, clears, uploads, readbacks, hazard tracker (section 5), timeline events and waiter thread (D7, D8), `TexturePool`, `RetireQueue`/`FrameClock`, `FrameFence`, memory snapshot (D18), the P1 water paths (section 7) | The compute parity kit passes on KosmicKrisp with sync validation clean. The one full proof run at phase end is clean |
| **P2 Render and profiling** | Graphics pipelines (dynamic rendering, MSAA, depth, depth-only, points, wireframe, vertex layouts), the `draw_*` family, render passes and scissor, mipmaps, memoryless, Y-flip and winding (D13), point size (D9), timestamps (D17) | The render parity kit passes with sync validation clean. `capture` stills and `ui-snap` PNGs match the Metal build's. The one full proof run at phase end is clean |
| **P3 Seams and app on Vulkan** | Section 6: shared textures, surface (D15), capabilities. Un-gate the app. The whole app runs on the Mac under `--features vulkan` | Headless subcommands (`ui-snap`, `bridge-probe`, `perf-soak`) run under Vulkan; bridge-probe shows no tears |
| **P4 Linux bring-up** | The map's compile breaks, cosmic-text, frame pacing, paths, menus, cpal 0.18, plugins as `.so` | The app boots in a Linux VM on lavapipe and plays a generative project with audio input |
| **P5 Video** | FFmpeg decode behind a decoder trait, export, live recording. The Mac keeps VideoToolbox | Video clips play, export and record on Linux; the Mac paths are unchanged |
| **P6 Water speed** | Section 7 P6 | FLIP proofs unchanged; measured CPU encode time per solve drops |
| **P7 Ray tracing and friends** | `VK_KHR_acceleration_structure` + `VK_KHR_ray_query` behind `ShadowRayTracer`; the RT kernels in the chosen language (section 11); denoiser; upscaler; GPU FFT | RT proofs and `rt-quality-matrix` match Metal within the RT tolerances Metal already uses |
| **P8 On the 5080** | Perf and latency on real NVIDIA hardware: Wayland vs X11 vs direct display, `VK_EXT_present_timing`, readback costs | Measured frame pacing matches the Metal profile shape |

---

## 10. Testing

Test the backend, not the nodes. The 300-odd node GPU proofs check node math written in WGSL, and that WGSL compiles to the same SPIR-V for both backends. Once the backend primitives are right, the nodes are right, so the proofs are not the per-batch check.

- **Per batch:** the compiler, clippy, and the touched crate's unit tests. Moves, renames and API merges need nothing more, because a wrong call does not compile.
- **Parity kit:** about a dozen tests in `manifold-gpu`, one per backend primitive. They cover dispatch, indirect dispatch, every copy and clear, write-then-read chains (the hazard tracker), the uniform ring past its first block, timeline events and the async lane, each texture format, and later render passes, MSAA, depth, points and mipmaps. Each checks values against a closed form on a tiny input. Every Vulkan batch runs the parts it touches on KosmicKrisp with synchronization validation on. Both Mac drivers sit on Metal, whose own ordering can hide a missing barrier that would corrupt frames on NVIDIA, so a green run without sync validation proves nothing about the tracker.
- **Stills:** from P2 on, render the same few presets (a generator, an effect chain, a scene, a water scene) with `capture` on both builds and diff the PNGs. One still exercises hundreds of shaders at once. `ui-snap` does the same for the UI.
- **Full proof run:** the node GPU proofs run once at the end of P1 and P2, in the background, on KosmicKrisp and then MoltenVK. Only a difference gets looked at. They never run per batch.
- **Delete, don't port, dead tests.** When a cutover removes a path (the extra timing paths, the IOSurface bridge, the draw variants), its tests go with it. Tests that only pin the old shape are deleted, not converted.
- **Linux VM:** `cargo check` per phase and one boot check at the end of P4. Never GPU suites, because lavapipe is too slow to be worth waiting on.
- **Ray tracing has no Mac driver.** MoltenVK's ray tracing is an unmerged experimental branch, and KosmicKrisp has none. Real checks need NVIDIA hardware.
- **Performance** is measured only on real NVIDIA hardware (P8), with `frame-time` and `perf-soak`. Never quote a predicted number.

---

## 11. Open decisions (Peter)

1. **Text on the Mac.** Use cosmic-text everywhere (one text path, same look on both platforms) or keep CoreText on the Mac. Recommended: cosmic-text everywhere, after a side-by-side `ui-snap` comparison.
2. **Ray tracing shader language.** Slang, compiled to both MSL and SPIR-V, replacing the hand-written MSL (one source for both backends), or a GLSL copy of the MSL for Vulkan (two sources that can drift). Recommended: Slang, decided at P7 start with a one-kernel trial.
3. **Denoiser.** DLSS Ray Reconstruction (NVIDIA only; the DLSS SDK ships Linux libraries; NVIDIA RTX SDK licence, which forbids combining with copyleft code) or NRD (same licence, any vendor, needs view-Z, normals and roughness inputs).
4. **GPU FFT.** One WGSL FFT for both backends, replacing MPSGraph on the Mac if it measures as fast, or a Vulkan-only FFT.
5. **HAP.** If shows use HAP clips, decide whether to upload compressed blocks to the GPU ourselves; FFmpeg decodes HAP on the CPU.

---

## 12. Performance watch items

- **Readbacks cross PCIe on a discrete card.** On Apple Silicon a GPU→CPU readback is nearly free. Per-frame readbacks (LED sampling, audio-reactive analysis, thumbnails) must stay asynchronous with a frame of latency, as the LED path already is. Never turn one into a commit-and-wait on the frame loop.
- **Present latency on Linux is unmeasured.** Compare Wayland, X11 and direct display (`VK_KHR_display`) on the 5080 before choosing the show setup.
- **Windows** (later) must reach independent flip in borderless fullscreen; verify with PresentMon.
