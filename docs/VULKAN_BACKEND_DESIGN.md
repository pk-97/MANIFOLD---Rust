# Vulkan Backend — Design (Linux first)

**Status:** design approved, not built. The `vulkan/` module is a stub and `manifold-gpu` does not compile with `--features vulkan` (`shadow_caster.rs:87` calls `create_buffer_shared`, which the stub lacks).
**Order:** Linux first, Windows later. The Linux machine (RTX 5080, Ryzen 9 9950X3D) is on order; until it arrives everything is built and tested on the Mac through a Vulkan driver that runs on Metal, and real NVIDIA performance comes last.
**Who reads this:** the lead, who writes worker specs from it. This file holds the rules and decisions. The inventory (what breaks, file by file) and the work order live in `LINUX_VULKAN_PORT_MAP.md`; re-check it at the start of every phase, the Metal side keeps moving.

---

## 1. What "done" means

MANIFOLD runs a full show on Linux with the Vulkan backend: generators, effects, scenes with ray tracing, water, video clips, export and recording, audio input, LED output. Every GPU proof that passes on Metal passes on Vulkan within its stated tolerance, under the Khronos synchronization validation layer with zero errors.

---

## 2. Where the port starts

Backend selection and the shader front end are done: `lib.rs` picks `metal/` or `vulkan/`, and WGSL → naga → SPIR-V → spirv-opt is shared (`shader_common.rs`, `workgroup_zeroing.rs`), so only "SPIR-V → MSL" is Metal's. Everything else, the API surface, Metal-only callers, shaders, app and crate inventory, and the work order, is in `LINUX_VULKAN_PORT_MAP.md`.

---

## 3. Rules

- **`ash` 0.38 only.** No wgpu, no vulkano. `gpu-allocator` 0.28 (built on ash 0.38) for memory, `ash-window` for surfaces. ash 0.38's headers are Vulkan 1.3.281: `VK_EXT_present_timing` needs a newer ash and waits until it ships.
- **Backend differences live inside `manifold-gpu`.** Shared crates never check which backend is active. A Metal-only feature becomes a backend-neutral API with a capability query (`denoiser_available()` returns false on Vulkan until it is built). A feature that is off logs once at boot, by name.
- **OS differences sit in one place each.** Folders and external-tool lookup are std-only helpers in `manifold-foundation` (`app_dirs`, `find_tool`), shared by every crate that needs them. OS code with a single user (keep-awake, thread priority, performance cores) sits in a per-OS module next to that user. Big services (text, audio capture, video, window chrome) keep one seam in their own crate. Shared code never writes `cfg(target_os)` inline.
- **Raw handles stay Metal-only.** `raw_device`, `raw_cmd_buf`, `raw_ptr`, `raw_device_ptr`, `mtl4_bridge` are not exported on Vulkan. Their callers are macOS-gated, so off the Mac the compile error under Vulkan enforces the rule. On the Mac those gates are still true under `--features vulkan`, so Mac Vulkan builds cover `manifold-gpu` and the crates without Metal-only code; `manifold-app`, `manifold-media` and `manifold-recording` build against Vulkan on Linux. Whether the whole app should also run on Vulkan on the Mac is decided at P3.
- **Metal is the reference, within tolerance.** Metal compiles every pipeline with fast math (`MTLMathMode::Fast`, `metal/device.rs:777` and four more), so last-bit differences are expected; each proof states its tolerance. Never loosen a tolerance or change `metal/` to make Vulkan pass. Shared WGSL may change only to remove undefined behaviour that is shown on Metal too. A difference beyond tolerance is a Vulkan bug until shown otherwise.
- **During construction,** a Vulkan method not built yet panics with `todo!("vulkan P1")` or `todo!("vulkan P2")`. The phase that builds it deletes the `todo!`. None survive P3.
- **Both builds stay green:** `cargo clippy -p manifold-gpu --tests -- -D warnings` with and without `--features vulkan`, and the same for each crate a phase touches.

---

## 4. Decisions

| # | Decision | Choice and reason |
|---|---|---|
| D1 | Version floor | **Vulkan 1.3**, required at boot: `dynamicRendering`, `synchronization2`, `timelineSemaphore`, `robustBufferAccess`. Missing = boot error naming the feature. naga writes unchecked indexing (`shader_common.rs:131-135` takes naga's default bounds policy), so an out-of-bounds write that is harmless on Metal would land in a neighbouring allocation; robust access contains it. NVIDIA, Mesa and both Mac drivers ship 1.3+ |
| D2 | Descriptors | **One descriptor set per dispatch or draw, from a per-encoder pool, written with an update template built from the pipeline's `SlotMap`.** A per-encoder cache keyed by pipeline and bound-resource identities reuses a set when the same bindings repeat, as Metal's `ComputeBindCache` skips repeated binds; water rounds depend on it. The pool resets with the encoder's command pool when its submit retires. The Vulkan `SlotMap` records uniform vs storage buffer and sampled vs storage image, because `VkDescriptorType` needs both. Push descriptors were the old plan, but NVIDIA caps them at 32 per set and the scene renderer needs 53 |
| D3 | `GpuBinding::Bytes` | Host-visible uniform ring per encoder: bump allocator, 256-byte aligned (`replay::BYTES_ALIGN` already uses 256), grows with a warning on overflow |
| D4 | Ordering | **Automatic hazard tracking inside `GpuEncoder`** (section 5). `pipeline_barrier` stays a no-op hint on both backends |
| D5 | Queues | One graphics+compute queue family. Content, UI and the async lane each get a `VkQueue` when the family has enough; otherwise they share one behind a mutex. Submission is mutex-serialised per queue. Whether a second queue overlaps on NVIDIA as Apple's does is measured in P8 |
| D6 | Command pools | Pool per encoder from a free list on the device, reset and reused when its timeline value retires. Encoders record on any thread |
| D7 | `GpuEvent` | Timeline semaphore. `signal_event_value` → signal, `wait_event` → wait (attached at submit; a mid-encoder wait splits the submit). `current_value` is the CPU-side "last value encoded" counter (`metal/types.rs:520`) that `TexturePool` and retire stamps read; it stays a CPU counter (renamed `encoded_value` in the API shrink). `signaled_value` and `is_done` → `vkGetSemaphoreCounterValue` |
| D8 | Completion and lifetime | **One timeline per `VkQueue`**, its value assigned under that queue's submit mutex, so signals never go backwards. One waiter thread per device waits on the queue timelines and runs `add_completed_handler` callbacks in order. `GpuFenceWaiter`, `FrameClock` and `RetireQueue` ride the same timelines. **Every encoder holds an `Arc` clone of each buffer and texture it touches until its submit's value completes.** Metal does this today through retained references (`metal/device.rs:1432`), and the UI device, tools, bakes and proofs rely on it. `RetireMark` then covers only resources reached by GPU address (ray tracing) and cached templates. The async lane is waited on before it reopens; a debug assert checks it |
| D9 | Shaders | SPIR-V from `shader_common` goes to `vkCreateShaderModule` unchanged. `arrayLength` is native `OpArrayLength`; the naga sizes buffer is never emitted (the stub's `shader_compiler.rs:125-134` still emits the slot; P1 removes it). Metal's `arrayLength` counts the whole buffer and ignores the binding offset (`metal/encoder.rs:4392-4416`), Vulkan's counts the bound range: P1 checks no shader binds a runtime-sized array at an offset. `rebind_vertex_output_as_point_size` (`shader_common.rs:69`) is gated off on Vulkan; lift the gate in P2, because point-list draws must write `PointSize` |
| D10 | f16 | No shader uses `f16` and no pipeline compiles with `use_half` (`create_compute_pipeline_half` has no callers). Vulkan compiles f32 only |
| D11 | Pipeline caching | Same three layers and keys as Metal: in-memory by `archive::pipeline_hash`, a `VkPipelineCache` blob on disk in place of `MTLBinaryArchive`, and a SPIR-V disk cache in place of `msl_cache.rs`. Same call sites (`load_pipeline_archive`, `save_pipeline_archive`, `load_msl_cache`) |
| D12 | Render passes | Dynamic rendering only, no `VkRenderPass` objects. Matches `begin_render_pass`/`end_render_pass` |
| D13 | Y-flip | Negative viewport height, so SPIR-V stays identical. Winding flips with it: `DepthMsaaDraw::cull_back_faces` must set the front face to match Metal. Wireframe fill mode needs `fillModeNonSolid` |
| D14 | Memory | Buffers say their use (map section 11): `Gpu` → device-local. `Upload` → device-local host-visible (Resizable BAR on the 5080), else host-visible coherent; write-only from the CPU, because CPU reads from that memory are extremely slow. `Readback` → host-visible cached. All persistently mapped where host-visible. Textures are device-local; memoryless → `LAZILY_ALLOCATED` where it exists |
| D15 | Presentation | `GpuSurface` → `VkSurfaceKHR` + swapchain via `ash-window` (Wayland, X11; `VK_EXT_metal_surface` on the Mac for testing). MAILBOX when offered, else FIFO, logged once. SDR first. `configure_edr`, window level, contents gravity and `presents_with_transaction` report "not supported" on Vulkan |
| D16 | Cross-thread textures | One native device per process, shared; each `GpuDevice` wrapper creates its own queue, as Metal's `new()` does today. Textures cross threads as plain `GpuTexture`s ordered by timeline waits (section 5). The IOSurface bridge becomes plain textures on both backends (section 6.1). No external-memory extensions |
| D17 | Profiling | Timestamp query pools scaled by `timestampPeriod`, behind whatever the GPU-timing consolidation (map section 11) leaves. Comparing GPU with CPU times needs `VK_EXT_calibrated_timestamps` |
| D18 | Memory admission | `modifier_memory_snapshot` feeds resource admission (`admit_candidate_bytes`, 39 call sites). Vulkan fills it from `VK_EXT_memory_budget` heap budget and usage. VERIFY in P1 that both Mac drivers expose it |
| D19 | Faults | `VK_ERROR_DEVICE_LOST` maps to `gpu_fault` kinds; `VK_EXT_device_fault` adds detail where present. Unlike Metal, where a faulting command buffer completes with an error and the app keeps going (`metal/encoder.rs:3491-3495`), device loss kills the shared device for content and UI together and no completion callback fires. On loss the waiter thread fails every pending callback and lease, the output holds its last frame, the error is logged, and the device and its resources are recreated. `firmware_reports` and residency sets have no Vulkan meaning and report empty |
| D20 | Debugging | `VK_EXT_debug_utils` names and labels everything the Metal backend labels. Captures use RenderDoc; Xcode capture scopes are no-ops |
| D21 | Mac drivers | **KosmicKrisp** (Mesa on Metal 4, Vulkan 1.4 conformant, macOS 26+, ships in the LunarG SDK 1.4.363) is the primary test driver; GPU-assisted validation works on it. **MoltenVK** 1.4.2 (Homebrew `molten-vk`) is the second. The instance enables portability enumeration so both are found. Pick one with `VK_DRIVER_FILES`. Both run on the Apple GPU, so NVIDIA filtering, precision and memory types are first exercised on the 5080 |
| D22 | Texture uploads | One upload form (the API shrink merges `device.upload_texture` and `enc.upload_texture`, both CPU `replaceRegion` writes on Metal today). Vulkan has no CPU-writable GPU-optimal image, so an upload is a staging-ring copy recorded on the encoder; the device-level form is a small submit that does not wait. `VK_EXT_host_image_copy` (in ash 0.38) can replace the staging copy where the driver offers it |
| D23 | Room for video | P1's device creation must not rule out P5: FFmpeg's Vulkan hardware context on our device needs the video-decode queue family and its extensions, and zero-copy NVENC needs CUDA interop through external memory, which D16 excludes. P5 decides; P1 only keeps the queue and extension setup open to it |

---

## 5. Hazard tracking

Metal runs commands on a queue in order; every caller in MANIFOLD relies on that. Vulkan orders nothing between commands without barriers. Adding barriers at the call sites is unverifiable, and a missed one corrupts frames on someone else's GPU. So the Vulkan `GpuEncoder` tracks resource state and emits barriers itself. It already sees every access: bindings name each buffer and texture (writability comes from the `SlotMap`), render passes name their attachments, copies name source and destination, indirect calls name their argument buffer.

- **One layout for every image: `GENERAL`.** It is legal for storage, sampling, copies and attachments. The only transitions are `UNDEFINED → GENERAL` at creation and `GENERAL ↔ PRESENT_SRC_KHR` for swapchain images. A per-texture layout updated while recording goes wrong whenever recording order differs from GPU order: encoders dropped without commit, side encoders committed mid-recording, two lanes on two queues, and the shared atlas the content thread writes while the UI samples it (`shared_texture.rs:467-480`). NVIDIA says layouts matter little on its hardware; P8 measures one A/B on the hottest attachments.
- **State is per lane** (one command buffer on one queue), reset when the lane switches, pre-sized so warm frames allocate nothing.
- **State is keyed by the root resource.** `mip_level_view` and `layer_view` make new `GpuTexture`s over the same image (production users: `bokeh_gather.rs:223`, `render_scene/prepasses.rs:473`, `gltf_texture_source.rs:1141`, `render_scene/resources.rs:879`); a view maps to its parent image.
- **Per resource: the last write `{stage, access}` and the union of read stages since that write.** Before each command, for each resource it touches: read after write → memory barrier from the write; write after write → memory barrier; write after read → execution barrier from the accumulated read stages. Read after read never barriers. All barriers for one command go into one `vkCmdPipelineBarrier2`.
- **Accesses include** shader reads and writes, attachments, transfers, indirect arguments (`DRAW_INDIRECT` stage), index and vertex input, and host reads: a `Readback` buffer the GPU wrote gets a GPU → host barrier before the submit's signal. Sync validation does not check CPU access, so the parity kit does.
- **Each command buffer starts with one global barrier** (all writes → all reads and writes). That makes the first touch of any resource correct whatever an earlier submit on the same queue did, without a full drain per resource. Exact tracking takes over after it.
- **Across queues** (content → UI through the bridge, the async lane → main): each publication records the producer queue's timeline value and the consumer's submit waits on it. A wait that is already satisfied costs nothing.
- **`compute_memory_barrier_buffers`** stays an explicit global compute→compute barrier, as on Metal.

---

## 6. Backend-neutral seams (shared code changes)

These change Metal-side and app code too, so they come before the app can run on Vulkan.

### 6.1 Shared textures (the IOSurface bridge)

`SharedTextureBridge` (`manifold-app/src/shared_texture.rs`) keeps three surfaces, an atomic `front_index`, per-slot read leases retired by completion handlers, and a generation counter. That logic has nothing to do with Metal; only the IOSurface storage and import do. Publishing already waits for GPU completion, so plain textures shared through one device are enough on both backends.

- Content and UI share one native device, each wrapper with its own queue (D16). The bridge holds plain `GpuTexture`s and keeps its slot and lease logic; IOSurface goes. The `shared_texture.rs:61` comment that the two devices are separate `MTLDevice`s is corrected with it.
- Prove it on Metal with `bridge-probe` (zero torn reads) before cutting over; on Vulkan it depends on D8 lifetime and the `GENERAL` layout.
- The bridge and every pane on it lose their `cfg(target_os = "macos")` gates, about 200 sites in `content_pipeline.rs`, `app.rs`, `frame/present.rs`, `editor_bridge.rs`, `scene_viewport_capture.rs`, `selection_outline.rs`, `perform_mode/render.rs` and the rest.

### 6.2 Metal-only features become capabilities

| Feature | Neutral API | Vulkan until built |
|---|---|---|
| Ray tracing | `raytrace` keeps its types and `ShadowRayTracer` trait on both backends | Tracer reports unavailable; scenes use raster shadow maps. Built in P7 |
| Denoiser | `denoiser_available()` already exists | false. Built in P7 |
| MetalFX spatial and temporal upscale | One upscaler capability in `manifold-gpu`; `manifold-compositor/src/fsr1.rs` stays the portable spatial path | Unavailable. DLSS or FSR in P7 |
| GPU FFT | `GpuFft` | Unavailable; the two nodes report it. Built in P7 |
| Residency sets, firmware reports, capture scopes | Existing calls | Empty or no-op |

A unit test asserts every capability reports available on Metal, so the cutover can never silently switch ray tracing, the denoiser or MetalFX off on the Mac.

---

## 7. Water: replay spans, gated segments, templates

On Metal, GPU FLIP runs its pressure solve from indirect command buffers. One round is recorded once (`prepare_template`) and the GPU runs it in chunks whose lengths it writes itself (`execute_template`, up to the 900-round cap). The CPU never encodes a round per round. `begin_gated_segment` and `dispatch_compute_gated` do the same for GPU-decided dispatch counts, and node-engine replay spans (`begin_replay`) cache whole stretches.

- **P1 (correct):** replay spans encode directly (`vulkan/replay.rs` already does). A gated dispatch is a plain indirect dispatch from its own `gate`/`gate_offset` arguments. `execute_template` encodes `copies` rounds of those indirect dispatches; the kernels already guard rounds past the stop. Same results, much more CPU encoding: up to 900 rounds times several dispatches per solve, each with a descriptor lookup (D2's cache) and barriers. P1's last step measures water encode time on the Mac.
- **P6 (fast):** one round recorded once into a secondary command buffer, with descriptor sets owned by that recording rather than the encoder's pool; each chunk runs under `VK_EXT_conditional_rendering`, reading the GPU-written range. Replay spans need the same treatment. Conditional rendering does not skip barriers inside skipped rounds, so P6 measures GPU time as well as CPU time. VERIFY driver support first (NVIDIA has it; check both Mac drivers).
- **Water at the first Linux show** depends on the P1 measurement: if it is too slow, P6 moves ahead of the show or water waits for it.

---

## 8. Platform services on Linux

| Service | Mac today | Linux | Work |
|---|---|---|---|
| UI text | CoreText atlas, `manifold-ui-paint/src/native_text.rs` (2,172 lines) behind `manifold_ui::text::TextMeasure` | cosmic-text 0.19 (harfrust shaping, fontdb, swash raster), embedded Inter | Medium. Without it the UI has no text |
| Text generator | CoreText, `manifold-nodes-image/src/text_rasterizer.rs` (689 lines), system families by name; the default is embedded Inter. The terminal glyph atlas asks for "Menlo" (`glyph_atlas.rs:30`) | cosmic-text + fontdb system fonts; the terminal falls back to a bundled or present monospace font | Small-medium |
| Video decode | AVFoundation + VideoToolbox plugin (`MetalVideoDecoderPlugin.m` 765 lines), writes into a raw `MTLTexture` | FFmpeg via `ffmpeg-next` 9 with the Vulkan hwcontext on our own `VkDevice` (D23): decoded frames arrive as `VkImage`s. CPU decode + upload where the driver lacks Vulkan Video (the Mac drivers), logged once | Large. Needs a decoder trait below `ClipRenderer`; `DecoderPool`/`DecodeScheduler` are concrete today. ProRes decodes on Vulkan compute in FFmpeg 8.1+. HAP decodes on the CPU in FFmpeg |
| Export | AVFoundation encoder plugin (670 lines) | FFmpeg encode: NVENC on NVIDIA, x264/x265 otherwise. `audio_muxer.rs` already shells out to ffmpeg | Medium |
| Live recording | AVAssetWriter plugin (839 lines), ProRes Proxy or HEVC Main10 | FFmpeg encode on the recording thread | Medium |
| System-audio capture | CoreAudio process tap (`process_tap.rs`), behind `CaptureBackend` | cpal 0.18's PipeWire host opens a sink as an input for monitor capture. cpal 0.15 → 0.18 everywhere, which includes upgrading kira (0.9.6 pins cpal 0.15) | Small-medium |
| Audio input latency | CoreAudio, small buffers | `cpal_input.rs:88` asks for `BufferSize::Default`, which on ALSA under cpal 0.15 means 25 ms periods and a 100 ms buffer. Request a small fixed buffer from the device's supported range; the kick detector depends on it | Small |
| Audio device list | CoreAudio directory | cpal fallback already exists (`fallback.rs`) | None |
| Frame pacing | CVDisplayLink + `mach_wait_until` | Present-driven with `VK_KHR_present_wait`, `clock_nanosleep(TIMER_ABSTIME)` + short spin. The deadline fallback in `frame_timer.rs:266` already runs off-Mac | Small-medium |
| Output window and displays | Window placed with `with_position` (`app_lifecycle.rs:1356-1371`), fullscreen toggle via `set_outer_position` (`window_input.rs:631-644`), displays matched by ID (`breadcrumb.rs:247`) | On Wayland, winit cannot position windows and `primary_monitor()` is `None`, and off the Mac display matching returns `None`, so output falls back to the largest monitor. Present with `Fullscreen::Borderless(Some(monitor))`; match displays by name and size. XWayland (winit's X11 backend inside a Wayland session) is the fallback | Medium. Show-critical |
| File drag and drop | winit `DroppedFile` (`app.rs:2057`) | winit 0.30 delivers it on X11 only, not Wayland | Until winit adds it: XWayland, or import through dialogs |
| Shortcuts | `command` = the Cmd key (`app.rs:1965`, 92 uses) | That line maps it to Super, which GNOME and KDE take for themselves: save, undo, copy and paste would be gone. Map `command` to Ctrl off the Mac, then check the Ctrl-only gestures (`editor_input.rs:443`, `preview_navigation.rs:332`) | Small |
| Menus | muda, macOS only (`menu.rs:190`: Linux `init_for_gtk_window` not wired) | Seven actions are reachable only from the menu (`app_render.rs:438-467`): Open Recent, Clear Recent, Collect All and Save, Restore Snapshot, Open Snapshot Copy, Import Video, RT Quality. They need in-app entries before the Linux build can drop the menu bar. Collect All and Save is how a project moves from the Mac to Linux | Small-medium |
| Text clipboard | Text fields copy and paste through `macos_pasteboard` (`text_session.rs:539-547`, `window_input.rs:1350-1358`); arboard is used at one site (`app_render.rs:718`) | Text fields go through arboard everywhere (with `wayland-data-control`); `macos_pasteboard` stays for Finder file paste only | Small |
| Dialogs | rfd | rfd's message dialogs call `zenity` on Linux and return Cancel if it is missing, so the unsaved-changes prompt (`alerts.rs:54`) answers Cancel and quitting with unsaved edits does nothing. `zenity` is a runtime dependency | Package |
| Instance lock | `flock`, Mac-gated (`main.rs:358-360`) | `flock` is POSIX: remove the gate, or two instances fight over OSC, MIDI and Art-Net | Trivial |
| Paths | `~/Library/...` in three folder names | `manifold_foundation::app_dirs` with XDG on Linux. The Mac paths stay byte-identical (map section 6) | Small |
| ML plugins | OpenCV `.bundle` Mach-O plugins via libloading (`manifold-native/src/ffi/mod.rs:12`) | Same plugins built as `.so` against `libopencv-dev`; names from `std::env::consts::DLL_PREFIX`/`DLL_SUFFIX` | Small |
| Keep-awake, thread priority | IOPMAssertion; Mac thread policy | Idle inhibit through the XDG portal or `systemd-inhibit`; real-time priority through rtkit or rtprio limits | Small, P4 |
| HDR output | EDR on CAMetalLayer | SDR first | Later |

The Linux compile breaks, file by file, are in `LINUX_VULKAN_PORT_MAP.md` sections 2, 6 and 7.

---

## 9. Phases

Each phase compiles, passes its tests and commits. P4's text work can run alongside P1 and P2, because it touches different files.

| Phase | Builds | Done when |
|---|---|---|
| **P1 Headless compute** | `vulkan/{device,encoder,types}.rs` and siblings: instance and device bring-up (D1, D21, D23), allocator, buffers by use (D14), textures, samplers, uploads (D22), compute pipelines and caches (D11), descriptor sets and their cache (D2), uniform ring (D3), dispatch, indirect dispatch, copies, clears, readbacks, hazard tracker (section 5), timelines, lifetime and the waiter thread (D7, D8), `TexturePool`, `RetireQueue`/`FrameClock`, `FrameFence`, memory snapshot (D18), device-loss handling (D19), the P1 water paths (section 7) | The compute parity kit passes on KosmicKrisp with sync validation clean. The one full proof run at phase end is within tolerance. Water encode time is measured |
| **P2 Render and profiling** | Graphics pipelines (dynamic rendering, MSAA, depth, depth-only, points, wireframe, vertex layouts), draws, render passes and scissor, mipmaps, memoryless, Y-flip and winding (D13), point size (D9), timestamps (D17) | The render parity kit passes with sync validation clean. `capture` stills match the Metal build's. The one full proof run at phase end is within tolerance |
| **P3 Seams and app on Vulkan** | Section 6: shared textures, surface (D15), capabilities. Un-gate the app | Headless subcommands (`ui-snap`, `bridge-probe`, `perf-soak`) run under Vulkan in the Linux VM; bridge-probe shows no tears under sync validation |
| **P4 Linux bring-up** | The map's compile breaks, cosmic-text, frame pacing, output placement, shortcuts, menu-only actions, clipboard, cpal 0.18 and audio buffer size, keep-awake, thread priority, plugins as `.so` | The app boots in a Linux VM and plays a generative project with audio input |
| **P5 Video** | FFmpeg decode behind a decoder trait, export, live recording. The Mac keeps VideoToolbox | Video clips play, export and record on Linux; the Mac paths are unchanged |
| **P6 Water speed** | Section 7 P6 | FLIP proofs unchanged; measured CPU and GPU time per solve drop |
| **P7 Ray tracing and friends** | `VK_KHR_acceleration_structure` + `VK_KHR_ray_query` behind `ShadowRayTracer`; the RT kernels in the chosen language (section 11); denoiser; upscaler; GPU FFT | RT proofs and `rt-quality-matrix` match Metal within the RT tolerances Metal already uses |
| **P8 On the 5080** | Perf and latency on real NVIDIA hardware: Wayland vs XWayland vs direct display, `VK_EXT_present_timing`, readback costs, `GENERAL` layout A/B, async-lane overlap | Measured frame pacing matches the Metal profile shape |

---

## 10. Testing

Test the backend, not every node. The 300-odd node GPU proofs check node math written in WGSL, and that WGSL compiles to the same SPIR-V on both backends. Once the backend primitives are right, the nodes are right within tolerance, so the proofs are not the per-batch check.

- **Per batch:** the compiler, clippy, and the touched crate's unit tests. Moves and renames need nothing more. A merge that takes a value (a pipeline or draw descriptor) compiles even when the value is wrong, so its spec names the presets that use each changed form and the batch renders those stills.
- **Parity kit:** about a dozen tests in `manifold-gpu`, one per backend primitive, each checking values against a closed form on a tiny input. They cover dispatch, indirect dispatch, every copy and clear, read-after-write, write-after-write and write-after-read chains, writes through a mip or layer view read through the parent, the uniform ring past its first block, timeline events, the async lane, a texture written on one thread and read on another, a `Readback` buffer read by the CPU, each texture format, and later render passes, MSAA, depth, points and mipmaps. Every Vulkan batch runs the parts it touches on KosmicKrisp with synchronization validation on. Both Mac drivers sit on Metal, whose own ordering can hide a missing barrier that would corrupt frames on NVIDIA, so a green run without sync validation proves nothing about the tracker.
- **Stills:** from P2 on, render the same few presets (a generator, an effect chain, a scene, a water scene) with `capture` on both builds, with sync validation on, and diff the PNGs. One still exercises hundreds of shaders at once.
- **Full proof run:** the node GPU proofs run once at the end of P1 and P2, in the background, on KosmicKrisp and then MoltenVK, with GPU-assisted validation's out-of-bounds checks on. Only a difference beyond tolerance gets looked at. They never run per batch.
- **The day the 5080 arrives:** the parity kit and the stills run on it first. It is the first NVIDIA hardware the backend sees.
- **Delete, don't port, dead tests.** When a cutover removes a path (the extra timing paths, the IOSurface bridge, the draw variants), its tests go with it. Tests that only pin the old shape are deleted, not converted.
- **Linux VM:** `cargo check` per phase and one boot check at the end of P4. Never GPU suites, because lavapipe is too slow to be worth waiting on.
- **Ray tracing has no Mac driver.** MoltenVK's ray tracing is an unmerged experimental branch, and KosmicKrisp has none. Real checks need NVIDIA hardware.
- **Performance** is measured only on real NVIDIA hardware (P8), with `frame-time` and `perf-soak`. Never quote a predicted number.

---

## 11. Open decisions (Peter)

1. **Desktop session on the 5080.** Ubuntu 26.04 ships GNOME on Wayland only. On Wayland, winit cannot place windows or receive file drops; output placement works through borderless fullscreen on a chosen monitor, and XWayland covers the rest. Decide with the distro whether the show runs native Wayland, XWayland, or a desktop that still offers X11.
2. **Text on the Mac.** cosmic-text ships on Linux first. Recommended: switch the Mac to it too, after a side-by-side `ui-snap` comparison, so the UI looks the same on both.
3. **Ray tracing shader language.** Slang, compiled to both MSL and SPIR-V, replacing the hand-written MSL (one source for both backends), or a GLSL copy of the MSL for Vulkan (two sources that can drift). Recommended: Slang, decided at P7 start with a one-kernel trial.
4. **Denoiser.** DLSS Ray Reconstruction (NVIDIA only; the DLSS SDK ships Linux libraries; NVIDIA RTX SDK licence, which forbids combining with copyleft code) or NRD (same licence, any vendor, needs view-Z, normals and roughness inputs).
5. **GPU FFT.** One WGSL FFT for both backends, replacing MPSGraph on the Mac if it measures as fast, or a Vulkan-only FFT.
6. **HAP.** If shows use HAP clips, decide whether to upload compressed blocks to the GPU ourselves; FFmpeg decodes HAP on the CPU.

---

## 12. Performance watch items

- **Readbacks cross PCIe on a discrete card.** On Apple Silicon a GPU→CPU readback is nearly free. Per-frame readbacks (LED sampling, audio-reactive analysis, thumbnails) must stay asynchronous with a frame of latency, as the LED path already is. Never turn one into a commit-and-wait on the frame loop.
- **The one live GPU wait.** Coupled liquids wait for the GPU mid-frame through `commit_wait_and_continue` (`substep_region.rs:223`). On a discrete card each wait is a PCIe round trip; measure it in P8.
- **Present latency on Linux is unmeasured.** Compare Wayland, XWayland and direct display (`VK_KHR_display`) on the 5080 before choosing the show setup.
- **x86_64 build flags.** No `target-cpu` is set, so the 31 `mul_add` sites (`sheeter.rs`, `cnn.rs`) become library calls at the x86-64 baseline. Try `-C target-cpu=x86-64-v3` on the 5080 machine and measure.
- **Windows** (later) must reach independent flip in borderless fullscreen; verify with PresentMon.
