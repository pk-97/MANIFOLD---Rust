# Linux and Vulkan Port — Map

**What this is:** everything in the code, the build and the tooling that must change before MANIFOLD runs a show on the Linux machine (RTX 5080, Ryzen 9 9950X3D). The rules and decisions live in `VULKAN_BACKEND_DESIGN.md`; this file is the inventory and the work order. Delete a row when its work lands.
**Line numbers** are from 28 Oct 2026. Re-check with `rg` before writing a spec from a row. Call-site counts are rough `rg` counts across `crates/` outside `manifold-gpu`.

---

## 0. Where it stands

- **GPU:** `manifold-gpu/src/vulkan/` builds the real device, memory, buffers, textures and samplers (`core.rs`, `device.rs`, `types.rs`); everything else is still `todo!` stubs against about 30,000 lines in `metal/`. The parity kit is `vulkan/parity_tests.rs`. The workspace lints clean with `--features manifold-gpu/vulkan` except `manifold-app`, `manifold-media` and `manifold-recording`, and compiles on Linux.
- **Linux:** the workspace has never been compiled for Linux. Off the Mac the app opens a window with no text, no preview and no content render (`manifold-app/src/content_pipeline.rs:1997` logs "Non-macOS render path not available").
- **Tooling:** installed on 28 Oct (section 1). There is no CI.
- **Already portable:** shaders (WGSL → SPIR-V is shared), the machine-wide GPU queue lock (`queue.rs` has a Linux branch), the audio device list through cpal, MIDI (midir), Ableton Link (rusty_link), OSC and Art-Net (std UDP), the prefs path (`user_prefs.rs:38` has an XDG arm), the content-thread deadline sleep (`frame_timer.rs:266`), all in-repo C and C++ (Box3D, the FLIP Fluids oracle, Signalsmith), the embedded Inter fonts, and every `include_str!`/`include_bytes!` path (case-clean). No Rust code is ARM-only apart from one test assert (`manifold-fluids/src/sheet_oracle.rs:257`).

---

## 1. Setup

**Mac (dev box), installed**
- LunarG Vulkan SDK 1.4.363.0 in `~/VulkanSDK/1.4.363.0`, with its system-global part in `/usr/local`, so the loader, both drivers and the validation layers are found with no environment variables. KosmicKrisp reports Vulkan 1.4.363 (conformant) and MoltenVK 1.4.2, both on the M4 Max. Both drivers enumerate, which shows one GPU as two devices: pick with `VK_DRIVER_FILES=/usr/local/share/vulkan/icd.d/libkosmickrisp_icd.json` (or `MoltenVK_icd.json`), and the backend's device choice is deliberate, never "first found" (design D21). The SDK also brings `slangc` for P7.
- `x86_64-unknown-linux-gnu` Rust target, for `cargo check --target` on pure-Rust crates such as `manifold-foundation`. Crates with C or C++ build steps need the VM.
- `naga` (for `scripts/wgsl_check.py`) and sccache were already there.

**Linux VM, installed**
- Lima 2.2.1 instance `manifold`: Ubuntu 26.04.1 arm64, Apple virtualisation, 8 CPUs, 12 GiB, 150 GiB sparse disk. Mesa's lavapipe reports Vulkan 1.4.335, with the validation layer.
- The Mac home folder is mounted read-only at the same path, so the VM builds the working tree as it is, uncommitted edits included, and can never write to it. Builds go to `~/manifold-target` inside the VM. `~/.profile` sets `CARGO_TARGET_DIR`, clears `RUSTC_WRAPPER` (the repo config still names the Mac's sccache path) and skips LFS downloads.
- Rust 1.94.0, the same as the Mac, with clippy and cargo-nextest. `~/.cargo/config.toml` links with `mold`.
- Run a check: `limactl shell manifold -- bash -lc 'source ~/.profile; cd "/Users/peterkiemann/MANIFOLD - Rust" && cargo check --locked <args>'`.
- The mount inherits the Mac disk's case-insensitivity. Every `include_str!`/`include_bytes!` path is case-clean; for a case-sensitivity pass, clone into the VM's own disk. Lima has no audio device, so the P4 audio boot check runs in UTM or on the 5080. It is aarch64, so x86_64 details wait for the real machine.

**Packages (installed in the VM; the same list for the 5080 machine)**
- Build: `build-essential cmake clang libclang-dev pkg-config git git-lfs mold libasound2-dev libopencv-dev` (OpenCV for the ML plugins), and `libgtk-3-dev libxdo-dev` while muda stays on Linux (section 10, item 3).
- Vulkan: `mesa-vulkan-drivers vulkan-tools vulkan-validationlayers libvulkan-dev`. `ash` and `winit` load `libvulkan`, `libxkbcommon`, `libwayland-client` and the X11 libraries at run time.
- Later phases, installed now so nothing blocks: `libpipewire-0.3-dev` (P4 capture), `libavcodec-dev libavformat-dev libavutil-dev libswscale-dev libavfilter-dev libavdevice-dev ffmpeg` (P5), `xvfb` (headless windows), `zenity` (rfd's message dialogs; without it the unsaved-changes prompt answers Cancel).
- On the 5080 machine only: xdg-desktop-portal with a backend for file dialogs, rtkit or rtprio limits for real-time priority, the NVIDIA driver.

**The 5080 machine**
- Distro and desktop session: open (section 10, item 1). It needs a kernel recent enough for the 9950X3D's cache-aware scheduling and NVIDIA's current driver with the open kernel modules, the only kind that supports 50-series cards.
- Resizable BAR on in the firmware; `vulkaninfo` must show a device-local, host-visible heap the size of VRAM (design D14 depends on it).
- Linux Vulkan SDK for the validation layers, and RenderDoc for captures.
- `git lfs pull` for `assets/plugins/DepthEstimator/models/midas_small.onnx`.
- First day: the parity kit and the stills run before anything else (design section 10).

---

## 2. Build switches

| Today | Change |
|---|---|
| `manifold-gpu/src/lib.rs:65-72` selects Vulkan only with `feature = "vulkan"`. On Linux without it the Metal module fails to compile | Linux selects Vulkan by target: `any(feature = "vulkan", not(target_os = "macos"))`, and `ash` becomes a non-Mac dependency. The feature then only means "Vulkan on the Mac", which covers `manifold-gpu` and the crates without Metal-only code (design section 3) |
| `muda` 0.19 pulls GTK 3 and `libxdo` (an unconditional `-lxdo`) on Linux, yet the Linux menu is never attached (`menu.rs:197`) | Mac-only dependency once the seven menu-only actions have in-app entries (P4, section 10 item 3) |

---

## 3. GPU backend: the surface to build

Every row below exists on Metal and is missing on Vulkan. Phases are the design doc's (section 9). Section 11 shrinks several groups on Metal before any of this is ported.

| Group | Items (call sites) | Phase |
|---|---|---|
| Device | `new`, `new_queued` (73), `device_name` (65), `create_encoder` (879), `async_queue`, `prepare_utility_pipelines` | P1 |
| Buffers | `create_buffer(size, use)` and `try_create_buffer` with `BufferUse::{Gpu, Upload, Readback}`; every former shared buffer is `Upload` until classified (section 11). `GpuBuffer`: `mapped_ptr` (409), `write`, `zero_fill` (66), `identity_key` (92), `ptr_eq` | P1 (D14) |
| Textures | `create_texture` (269), `try_create_texture` (9), `mip_level_view`, `layer_view`, `mip_level_count`, `is_shader_readable`. `GpuTexture` also carries `depth` (the stub lacks it) | P1 |
| Texture uploads | `device.upload_texture` and `enc.upload_texture` (127 together); both forms stay | P1 (D22) |
| MSAA and memoryless textures | `create_texture_msaa` (4), `create_texture_msaa_memoryless` (18) | P2 |
| Samplers | `create_sampler` (180), `linear_sampler` (3); anisotropy (`types.rs` note in the stub) | P1 |
| Compute pipelines | `create_compute_pipeline` (354), `create_specialized_compute_pipeline` (23, WGSL string substitution, ports verbatim), `compute_pipeline_cache_len`, `max_threads_per_threadgroup`, `unbound_binding` | P1 |
| Pipeline caches | `load_pipeline_archive`, `save_pipeline_archive`, `load_msl_cache`, `log_msl_cache_stats` (D11) | P1 |
| Compute encoding | `dispatch_compute` (483), `dispatch_compute_indirect` (46), `compute_memory_barrier_buffers` (117), `pipeline_barrier` (no-op hint) | P1 |
| Copies and clears | `clear_buffer` (25), `clear_texture` (120), `copy_buffer_to_buffer` (149), `copy_buffer_range` (14), `copy_texture_to_buffer` (159), `copy_texture_3d_to_buffer` (4), `copy_texture_to_texture` (51), `copy_depth_to_float` (1) | P1 |
| Mipmaps | `generate_mipmaps` (20) | P2 |
| Submission | `commit`, `commit_and_continue`, `commit_wait_and_continue`, `commit_and_wait` (a Result holding the submit's GPU seconds; device loss reports through it, D19), `add_completed_handler`, `add_completed_handler_with_status`, `tap_gpu_time` (every submit's GPU start and end), `GpuSpanTimer` | P1 (D8) |
| Events and lanes | `create_event` (8); `GpuEvent` (`encoded_value`, the CPU counter, see D7; `signaled_value`, `is_done`, `wait_until_done`); `signal_event` (12), `signal_event_value`, `wait_event`; `use_async_lane`, `use_main_lane`, `finish_async_lane`, `wait_async_lane`; `GpuFenceWaiter` | P1 (D5, D7, D8) |
| Frame lifetime | `frame_clock` (31), `FrameClock`, `RetireQueue`, `RetireMark`, `RetireSender`, `set_retirement`, `FrameFence`, `TexturePool` (`acquire`, `release`, `begin_frame`) | P1, after the hoist in section 9 step 4 |
| Memory | `modifier_memory_snapshot` (44, D18), `allocation_counts`, `resource_scope_id` | P1 |
| Water fast paths | `begin_replay`/`end_replay` (stubbed already), `begin_gated_segment`, `dispatch_compute_gated`, `end_gated_segments`, `GpuTemplateStore` (`prepare_template`, `execute_template`), `GatedRecorder`, `TemplateTicket`, `TemplateRanges` | P1 correct, P6 fast (design section 7) |
| Render pipelines | `create_render_pipeline(&RenderPipelineDesc)`; `create_depth_stencil_state` (17), `GpuDepthStencilState` | P2 |
| Draws | `draw(&DrawPass, &[Draw], label)`, the one draw; `begin_render_pass`/`draw_in_render_pass`/`end_render_pass`, `set_scissor_rect` for the UI; types `Draw`, `DrawPass`, `ColorTarget`, `DepthTarget`, `DepthStore`, `AuxColorAttachment`, `DrawCount` | P2 |
| Profiling | `commit_and_wait_profiled`, the per-dispatch profiler, on timestamp query pools | P2 (D17) |
| Presentation | `GpuSurface` (`create_surface`, `next_drawable`, `present`, `resize`, `set_maximum_drawable_count`), `GpuDrawable`, `set_presented_observer`; `configure_edr`, window level, gravity and `set_presents_with_transaction` report "not supported" | P3 (D15) |
| Device sharing | `sibling` (a second wrapper on the same native device with its own queue, D16) | P1 |
| Metal-only, stay Metal | `raw_device`, `raw_device_ptr`, `raw_cmd_buf`, `raw_ptr`, `mtl4_bridge`, `metalfx`, `metalfx_m4`, `defer_drop`, `dispatch_compute_with_accel`; `create_io_surface_bgra8` and `create_texture_from_io_surface`, used only by the analyzer plugin and `tv-led-mirror` | never |

**Shared already.** The frame fence, drop retirement, texture pool, fault registry (`gpu_fault`), firmware reports, profiled-frame records, draw descriptors and ray-tracing data types (`raytrace_types/`) live outside `metal/`; a backend supplies `RetiredResource` and `PoolAllocator`, and re-exports `raytrace_types` from its `raytrace` module.

`span_timer.rs` stays per backend: Metal polls its command buffers, Vulkan reads the encoder's per-submit timestamps.

---

## 4. Metal-only features that callers assume

Each is a backend-neutral API with a capability check on `GpuDevice` (design 6.2): `supports_ray_tracing`, `supports_denoiser`, `supports_upscaler` and `supports_fft` are true on Metal and false on Vulkan until P7.

- Ray tracing: `GpuShadowRayTracer`, `RtAccel` and the `ShadowRayTracer` seam (`raytrace_types/seam.rs`). The scene builds no tracer without the capability and keeps raster shadow maps.
- Upscaling and denoising: `GpuSpatialUpscaler`, `GpuTemporalUpscaler` and `GpuTemporalDenoiser` (`metal/scalers.rs`). The compositor and nodes-scene wrappers hold these and no raw handles; `fsr1.rs` stays the portable spatial path.
- FFT: Inverse FFT 2D reports "no FFT" and water fields lose their ripples.
- Faults, firmware, capture scopes and residency are empty on Vulkan.

The remaining raw-handle users are Mac-gated and stay Metal: `manifold-media/src/video_renderer.rs:405` and `metal_encoder.rs`, `manifold-recording/src/session.rs:102`, `texture_pool.rs:23,109`, `recording_thread.rs:150`, `manifold-app/src/content_export.rs:1625` and `content_pipeline.rs:1394`. Because those gates are still true on the Mac, `manifold-app`, `manifold-media` and `manifold-recording` build against Vulkan on Linux only (design section 3). Their Linux replacements are P5.

---

## 5. Shaders

- **WGSL:** 496 files, of which 267 are fragments joined by `include_str!`, `concat!` or `format!`. None use `f16`, subgroups, float or 64-bit atomics, binding arrays, `read_write` storage textures or a group other than 0. Storage textures are all write-only (132 declarations). Atomics are `atomic<u32>` and `atomic<i32>` only.
- **One BGRA storage write:** `manifold-recording/src/shaders/linear_to_srgb.wgsl:13`. Vulkan does not guarantee storage support for `B8G8R8A8_UNORM`. Check the format on NVIDIA in P5; otherwise write RGBA and swizzle.
- **Binding numbers** reach 72 (water shaders) with few distinct slots. `render_scene.wgsl` uses 37 distinct slots up to 57, and its composed entry points reach 53. Sparse Vulkan bindings are fine, and D2's one-set-per-draw has no 32-slot cap.
- **`arrayLength`:** Metal counts the whole buffer and ignores the binding offset, Vulkan counts the bound range (design D9). P1 checks no shader binds a runtime-sized array at an offset.
- **String substitution:** `create_specialized_compute_pipeline` (21 calls in 6 files) and `create_specialized_render_pipeline_depth_msaa` (`render_scene/pipelines.rs:462`) replace text in WGSL. So do `render_scene/pipelines.rs:189` and the water-surface `interior_fill.rs:27-28` and `surface_tiles.rs:39-40`. All of it is backend-neutral.
- **Point size:** the `shader_common.rs:69-70` gate lifts in P2. The callers are `render_scene/pipelines.rs:376,610` and `render_scene/pass2_draw.rs:634`.
- **Hand-written MSL:** `metal/shadow_rays.msl` (4,610 lines, 20 kernels) and `metal/subsurface.msl` (559 lines, 3 kernels), compiled together at `raytrace/tracer.rs:820`. They use:
  - ray queries
  - `array<texture2d<float>, 64>` and writes to `texture2d_array`
  - GPU addresses inside structs, fed by `GpuBuffer::gpu_address` (21 call sites, all ray tracing)
  - the function constants `HAS_TRANSLUCENCY` and `TRACE_PASS`
  - `atomic_uint`

  They use no threadgroup or simdgroup operations. On Vulkan this needs `VK_KHR_ray_query`, `bufferDeviceAddress` and descriptor indexing. P7, with the language decision in design section 11.

---

## 6. manifold-app

About 525 `target_os` lines in 46 files. Grouped by what they gate:

| Group | Lines | Files | Linux needs | Phase |
|---|---|---|---|---|
| Shared-texture bridge and every pane on it | ~200 | `shared_texture.rs`, `content_pipeline.rs` (66), `app.rs` (49), `frame/present.rs` (22), `scene_viewport_capture.rs` (12), `editor_bridge.rs` (11), `selection_outline.rs` (10), `perform_mode/render.rs` (8), `clip_atlas.rs` (5), `preview_navigation.rs`, `app_render.rs`, `editing_chrome.rs`, `gizmo_overlay.rs`, `scene_viewport.rs`, `content_commands.rs` (4), `main.rs:161,167` | Plain `GpuTexture`s on one device (section 11), then remove the gates | Before P3 |
| Native content render and device plumbing | ~43 | `content_pipeline.rs` (`render_content_native` and friends) | Remove the gates once the GPU types exist on both backends. Without this nothing renders | P3 |
| Window, presentation, display link, pacing | ~107 | `edr_surface.rs` (14), `quit.rs` (13), `frame_timer.rs` (12), `sync_rig_log.rs` (9), `content_command.rs` (5), `app_lifecycle.rs` (5), `window_input.rs` (4), `content_thread.rs` (3), `drag_interpose.rs`, `breadcrumb.rs`, `present_feedback.rs`, `workspace.rs`, `window_registry.rs`, `main.rs:58` (display link) | Stubs exist off-Mac for most. Linux needs present-driven pacing, output on a chosen display (design section 8: Wayland cannot position windows, display matching returns `None` off the Mac), and real-time priority in place of the Mac thread policy (`frame_timer.rs:285,294,403`) | P3, P4 |
| Export, still capture, recording, video and images | ~42 | `content_export.rs` (13), `content_thread.rs` (8), `content_commands.rs` (8), `app.rs` (4). Off-Mac clips get `StubRenderer` (`app.rs:1553`): no video and no images | FFmpeg decode and encode | P5 |
| Clipboard and file paste | ~20 | `input_host.rs` (11), `card_edit.rs`, `ui_snapshot/script.rs`, `window_input.rs`, `app.rs`, `main.rs:111` | Text fields and copy actions use `text_clipboard.rs` (arboard) everywhere. Finder file paste stays Mac-only; winit 0.30 has no file drops on Wayland | P4 |
| Menus | 2 | `menu.rs:187-197`; seven actions exist only in the menu (`app_render.rs:438-467`) | In-app entries for them, then section 10 item 3 | P4 |
| Headless harness and subcommands | ~90 | `headless_harness.rs`, `main.rs` (11 + 11), `content_pipeline.rs` (12), proof modules (66 lines: `bug219_verify`, `corridor_acceptance`, `frame_exact_app`, `gap_start_probe`, `gpu_flip_export_demo`, `import_responsiveness_verify`, `mute_visibility_probe`, `rt_dynamic_export_tests`) | `perf-soak`, `frame-time`, `rt-capture`, `bridge-probe` and `export-repro` are Mac-gated. `ui-snap` needs only text and `new_queued`. The harness boots the normal pipeline after the bridge change | P3 |
| Misc | 2 | `main.rs:379,732` (App Nap, nothing on Linux) | Gate stays | Build hygiene |

**Paths left.** The `.app` bundle lookups (`preset_loader.rs:376-384`, `percussion_backend.rs:210`) wait for Linux packaging (P4).

**Gated until P3 or P5.** On Linux the app compiles with these off: the output surface and its resize and suspend commands, the workspace preview bridge and its size, the native GPU handoff to the content pipeline (`set_native_gpu`), the display link, UI text measurement, video prewarm, video export (reports "not available"), and the tests that boot `headless_harness`.

---

## 7. Other crates

| Crate | What breaks or is missing on Linux | Phase |
|---|---|---|
| manifold-ui-paint | All UI text is CoreText (`native_text.rs`, 2,172 lines, gated at `lib.rs:10`). Off-Mac `ui_renderer.rs:1597,2071` sets `has_text = false`. Five text proofs in `tests/contracts/` are Mac-only | P4: cosmic-text |
| manifold-nodes-image | `text_rasterizer/`: CoreText on macOS (`core_text.rs`, 689 lines); elsewhere every string rasterizes to nothing, and the terminal glyph atlas panics on it (`glyph_atlas.rs` expects a glyph per ASCII character). The Text generator's default is embedded Inter (the "Helvetica" strings are test pins). The terminal glyph atlas asks for "Menlo" (`glyph_atlas.rs:30`), which Linux lacks. The font list comes from CoreText (`available_font_families`) | P4: cosmic-text and fontdb, a monospace fallback |
| manifold-media | Decode and export are Obj-C plugins on VideoToolbox and AVFoundation (`native/*.m`, 1,435 lines). `lib.rs:8-25` gates them. `DecoderPool`, `DecodeScheduler` and `MetalEncoder` are concrete types; the `ClipRenderer` trait (`manifold-playback/src/renderer.rs:10`) is the seam. `metadata.rs:19-29` returns `None` off-Mac. No FFmpeg crate in the graph today; only the `ffmpeg` CLI for audio muxing | P5 |
| manifold-recording | AVAssetWriter plugin (`native/LiveRecordingPlugin.m`, 839 lines). Everything but `config` and `ffi` is gated (`lib.rs:24-39`). Default path is the Desktop (`config.rs:29-37`). `proofs.rs` expects `ffprobe` | P5 |
| manifold-audio | System-audio capture is a CoreAudio process tap (`capture/process_tap.rs`, 792 lines); off-Mac `unsupported_tap.rs` returns an error. Off macOS, input asks for a 256-frame buffer within the device's range (`cpal_input.rs`). The device list already falls back to cpal. Mic permission returns Granted off-Mac | P4: PipeWire monitor capture, cpal 0.15 → 0.18 together with kira |
| manifold-playback | Works as is: midir and cpal on ALSA, rusty_link (needs cmake and libclang), kira (0.9.6 pins cpal 0.15). Stem separation: the bundled demucs runtime exists only for macOS (`tools/audio_analysis/BundledRuntime/macOS`, `stage_runtime_mac.sh`); the device is `auto`, so CUDA is picked on Linux | P4 (runtime) |
| manifold-native | The two ML plugins (BlobDetector, DepthEstimator) load `lib<name>.so` from `assets/plugins` on Linux; each `build.sh` builds it against `libopencv-dev`. `region_ffi.rs` tests that use `codesign` and `otool` stay Mac-only | P4: ship the `.so` files with the Linux package |
| manifold-nodes-scene | Decode cache under `Library/Caches` (`decode_cache.rs:132`) | P3 seams |
| manifold-node-engine | `exec/metal_backend.rs:1562-1640` tests compare `raw_ptr()` (gpu-proofs). Preset paths (section 6) | P3 |
| manifold-nodes | Dead Mac dev-dependencies (section 2) | P3 |
| manifold-physics | Thread count: Mac performance cores, `available_parallelism` elsewhere (`lib.rs:471-485`). On the 9950X3D that is 32 threads over two chiplets, only one with the extra cache | P8: measure |
| manifold-fluids | OpenMP (section 2). The `sheet_oracle.rs:245-260` test asserts an arm64 clang build of the oracle, so its baseline does not hold on x86_64 gcc | Test stays Mac-only |
| No findings | manifold-core, -editing, -foundation, -profiler, -ui, -spectral, -nodes-physics, -water-rigid, -water-liquid, -water-gpu-mpm, -water-surface, -water-whitewater | |

Out of scope: `plugins/` (the analyzer VST, its own workspace, uses IOSurface in `gpu_bridge.rs`) and `tv-led-mirror/` (a Mac tool).

**Project files** hold media paths. `path_resolver.rs` tries the relative path, then a filename-and-size search, both case-exact, so a project moves cleanly when its media travels next to it: Collect All and Save on the Mac first.

---

## 8. Scripts and tests

**Scripts.**
- Portable: `wgsl_check.py`, `dev.py`, `run_ui_flows.py`, `gpu_queue.py` and `bridge_probe_gate.py`.
- `feature_matrix.py` builds its rows from cargo metadata, so `manifold-gpu --features vulkan` is a row.
- Mac-only: `launch_live_ui.py` (builds a `.app`), `rt_dynamic_acceptance.py` (`sysctl`, `system_profiler`, blocked off-Darwin), `sync_rig_report.py` (`~/Library/Logs`), `install-abletonosc-patch.sh` (Dropbox path), `assets/plugins/*/build.sh`, `tools/audio_analysis/stage_runtime_mac.sh` and `plugins/scripts/build-analyzer-vst-plugin.sh`.
- Hardcoded repo path `/Users/peterkiemann/MANIFOLD - Rust`: `rt_noise_gate.py:103`, `rt_quality_matrix.py:28`, `rt_toggle_matrix.py:46` and `claude-pane.sh:47`. Derive it from the script's own location.

**Tests.**
- 304 files are gated on `gpu-proofs` across 17 crates. They test node math, which is the same WGSL on both backends, so they run once at the end of P1 and P2 and once after the buffer-use change, never per batch. The per-batch Vulkan check is the parity kit and stills (design section 10).
- Metal-only on purpose: 22 ray-tracing proofs (11 in `manifold-nodes/tests/gpu_proofs/rt_*`, 11 in `manifold-nodes-scene/tests/gpu_proofs/rt_*`) and `rt_p4_metalfx_temporal.rs` wait for P7. `manifold-gpu/src/metal/{async_lane,pipeline_cache,replay,template,residency}_tests.rs` get Vulkan twins where the behaviour exists; residency does not.
- `raw_ptr()` comparisons in `node-engine/exec/metal_backend.rs:1562-1640` and `manifold-app/tests/.../scene_viewport_frame_order.rs:97,107` compare `identity_key()` instead.
- About 21 Mac-gated test files (app probes, the ui-paint text proofs) come back once cosmic-text lands.

---

## 9. Work order

One batch at a time, and both builds stay green before the next one starts (design section 3). Workers write; the lead builds and tests.

**Before any Vulkan code, all on the Mac.** Each step shrinks or cleans what gets ported. The compiler catches most mistakes, the touched crates' unit tests catch the rest, and steps that change behaviour add the one check named below.

The overnight run does steps 2 to 8 with buffer use after step 5, then P1, GPU timing, P2 and cosmic-text on Linux. It starts only with MANIFOLD closed and no other session working in this folder, commits each batch, and parks a batch it cannot fix in a few tries.

1. **Setup** (section 1). Done 28 Oct.
2. **Build hygiene.** Done 28 Oct.
3. **Folders and tools.** Done 28 Oct (`manifold_foundation::AppDirs`, `find_tool`).
4. **Hoist neutral logic.** Done 28 Oct.
5. **Shrink the GPU API.** Done 29 Oct: one render-pipeline descriptor, one draw (`draw(&DrawPass, &[Draw], label)`, front faces counter-clockwise on every draw), `encoded_value`. Buffer use and GPU timing landed apart (section 11).
6. **Frame bridge on one device.** Done 28 Oct: the content device is the UI device's `sibling` and the bridge holds plain `GpuTexture`s. The headless subcommands booting the normal pipeline is still open (section 11).
7. **Vulkan skeleton.** Done 28 Oct: `vulkan/` mirrors every Metal signature with `todo!` bodies naming their phase, the section 4 callers check capabilities, and `cargo clippy --features manifold-gpu/vulkan --all-targets -- -D warnings` passes for every crate except `manifold-app`, `manifold-media` and `manifold-recording`.
8. **Linux compile.** Done 28 Oct: `manifold-gpu/build.rs` picks Vulkan on every target but macOS, and `cargo check --workspace --all-targets` passes in the VM. Text-field copy and paste moved to arboard on the way.

**Attended, before P1 needs them**
- **Buffer use** (section 11): the API landed 29 Oct with every former shared buffer as `Upload`; classification goes one subsystem at a time with that subsystem's proofs, then one full proof run.

**P1, one batch each, in this order**
9. Done 28 Oct: one shared instance and device (`vulkan/core.rs`), gpu-allocator, buffers, textures and views, samplers, uploads, the memory snapshot and debug names. Private buffers are device-local and shared ones host-visible until the buffer-use API lands (D14). Textures go to `GENERAL` and uploads copy through staging in a submit the caller waits on; step 12 can make both asynchronous (D22).
10–12. Done 28 Oct: compute pipelines with update templates, the set cache, the ring and both disk caches (`vulkan/pipeline.rs`); the encoder with the hazard tracker (`encoder.rs`, `tracker.rs`); per-queue timelines, the waiter thread, recycled frame resources, events, the fence waiter and device loss (`submit.rs`). Still open: `use_async_lane` refuses, so all work runs on one queue; the encoder's `upload_texture` stages through the ring in-stream, the device's still waits.
13. Done 29 Oct: replay spans encode directly, a gated dispatch is an indirect dispatch from its gate, and a template encodes its rounds as those dispatches with its bytes uploaded once (`vulkan/template.rs`). Still open: measure water encode time.

Each P1 batch adds its parity-kit tests and runs them on KosmicKrisp with synchronization validation. P1 is done when the compute kit is clean and the one full proof run at phase end is within tolerance.

**After P1:**
- P2: render and profiling.
- P3: shared textures, the surface, and un-gating the app's native render and headless harness.
- P4: Linux services (text, pacing, output placement, shortcuts, menu-only actions, clipboard, audio buffer and capture, keep-awake, thread priority, plugins as `.so`).
- P5: video.
- P6: water speed.
- P7: ray tracing.
- P8: on the 5080.

P4's text work touches only `manifold-ui-paint` and `manifold-nodes-image`, so it can slot between GPU batches once step 8 lands.

**What a first Linux show needs:** P1 to P4. That covers generators, effects, raster scenes, audio input, MIDI, Link, OSC, LED and the output window. Water is in if step 13's measurement says P1's encoding is fast enough; otherwise P6 comes first. Video clips need P5. Ray-traced scenes need P7.

---

## 10. Decisions for Peter

1. **Distro and desktop session on the 5080 machine.** Ubuntu 26.04 LTS packages NVIDIA's open driver, the LunarG SDK targets Ubuntu, and it stays stable between gigs. Its GNOME desktop is Wayland-only, and winit on Wayland cannot place windows or receive file drops. Output placement still works through borderless fullscreen on a chosen monitor, and XWayland covers the rest (design section 11).
2. **Backend selection on Linux.** Recommended: by target. `cargo build` just works on Linux, and on the Mac `--features vulkan` covers `manifold-gpu` and the crates without Metal-only code.
3. **Menu bar on Linux.** Recommended: none, once the seven menu-only actions (Open Recent, Clear Recent, Collect All and Save, Restore Snapshot, Open Snapshot Copy, Import Video, RT Quality) have in-app entries. muda's Linux menus need GTK windows, which winit doesn't make, and dropping it removes GTK and libxdo from the Linux build.
4. **Text.** cosmic-text ships on Linux first. Recommended: move the Mac to it too after a side-by-side `ui-snap`, so the UI looks the same on both.
5. **No new platform crate.** The earlier `manifold-platform` idea is dropped: only folders and tool lookup are shared, and they need nothing but std, so they live in `manifold-foundation`. The rest has one user each and stays next to it.
6. Ray-tracing shader language, denoiser, GPU FFT and HAP stay where design section 11 puts them (P5 and P7).

---

## 11. Simplify while porting

Every API the Vulkan backend copies gets built twice and tested twice. So shrink it first, on Metal, where the existing checks see the result, then port the smaller surface. Each item is its own batch, and Metal behaviour stays the same unless the row says otherwise.

**One place for each kind of difference** (design section 3)
- **GPU differences** live in `manifold-gpu` only. A missing feature answers through a capability check.
- **Folders and tool lookup** are std-only helpers in `manifold-foundation` (`app_dirs`, `find_tool`), which every crate that needs them already depends on.
- **Single-user OS code** (keep-awake, real-time priority, performance cores) sits in a per-OS module next to its user. Plugin library names come from `std::env::consts`.
- **Big OS services** keep one seam each: text (cosmic-text, no per-OS code), system-audio capture (`CaptureBackend`), video (`ClipRenderer`), window chrome (the app's window module).
- **Target:** shared code never writes `cfg(target_os)` inline. Today `manifold-app` alone has about 525 such lines.

**GPU API reductions**

| Today | After | Checked by | When |
|---|---|---|---|
| `create_buffer(size, BufferUse)` with every former shared buffer as `Upload`: GPU-only scratch, CPU-filled inputs and readbacks alike. It costs nothing on Apple's unified memory; on the 5080 it decides whether a shader reads VRAM or crosses PCIe | Each buffer classified (D14) by evidence: a buffer whose mapping the CPU reads is `Readback`, one it only writes is `Upload` (write-only API), one it never maps is `Gpu`. On Metal, `Gpu` becomes Private. The callers that skip their write silently when `mapped_ptr()` is `None` (`detect_regions.rs:637`, `nested_cubes_geometry.rs:197`, `liquid_state.rs:341-357`, `track_regions.rs:539`, `mesh_cut_map.rs:228`) become hard errors first, so a wrong `Gpu` fails loudly instead of freezing geometry | Each subsystem's proofs, then one full proof run | One subsystem per batch. The biggest NVIDIA speed item in the port |

**Bigger cuts**
- **Headless harness.** `perf-soak`, `frame-time`, `rt-capture`, `bridge-probe` and `export-repro` boot through a Mac-only harness that duplicates app setup. After the bridge change they boot the normal content pipeline.
- **One text engine.** The UI (2,172 lines) and the Text generator (689 lines) are two separate CoreText implementations. One cosmic-text module serves both.

**Where the frame loop waits on the GPU:** only in coupled-liquid substeps (`commit_wait_and_continue`, `substep_region.rs:223`). The other production `commit_and_wait` calls (about 35) sit in warmup, bakes, thumbnails and tools.
