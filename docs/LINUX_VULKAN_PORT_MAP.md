# Linux and Vulkan Port — Map

**What this is:** everything in the code, the build and the tooling that must change before MANIFOLD runs a show on the Linux machine (RTX 5080, Ryzen 9 9950X3D). The rules and decisions live in `VULKAN_BACKEND_DESIGN.md`; this file is the inventory and the work order. Delete a row when its work lands.
**Line numbers** are from 28 Oct 2026. Re-check with `rg` before writing a spec from a row. Call-site counts are rough `rg` counts across `crates/` outside `manifold-gpu`.

---

## 0. Where it stands

- **GPU:** `manifold-gpu/src/vulkan/` is 514 lines of stubs against about 30,000 in `metal/`. `cargo check -p manifold-gpu --features vulkan` fails at `shadow_caster.rs:87` (`create_buffer_shared`), and that is only the first error. No crate forwards the `vulkan` feature, so nothing above `manifold-gpu` has ever been checked against the Vulkan surface.
- **Linux:** the workspace has never been compiled for Linux. Off the Mac the app opens a window with no text, no preview and no content render (`manifold-app/src/content_pipeline.rs:1997` logs "Non-macOS render path not available").
- **Tooling:** this Mac has no Vulkan SDK, MoltenVK, KosmicKrisp or Linux VM. There is no CI.
- **Already portable:** shaders (WGSL → SPIR-V is shared), the machine-wide GPU queue lock (`queue.rs` has a Linux branch), audio input and device list through cpal, MIDI (midir), Ableton Link (rusty_link), OSC and Art-Net (std UDP), the prefs path (`user_prefs.rs:38` has an XDG arm), the content-thread deadline sleep (`frame_timer.rs:266`), all in-repo C and C++ (Box3D, the FLIP Fluids oracle, Signalsmith), and the embedded Inter fonts. No Rust code is ARM-only apart from one test assert (`manifold-fluids/src/sheet_oracle.rs:257`).

---

## 1. Setup

**Mac (dev box)**
- LunarG Vulkan SDK for macOS: KosmicKrisp, MoltenVK, the validation layers and `vulkaninfo`. Pick a driver with `VK_DRIVER_FILES` (design D21).
- A Linux VM (Lima or UTM, Ubuntu arm64, Mesa's lavapipe software driver). It checks the Linux code paths: compile, window, audio, paths, packages. It is aarch64, so x86_64 details wait for the real machine. Keep `CARGO_TARGET_DIR` inside the VM, never the Mac's `target/`.
- `naga-cli` (for `scripts/wgsl_check.py`) and `sccache`.

**Packages, VM and 5080 machine (Debian names)**
- Build: `build-essential cmake clang libclang-dev pkg-config libasound2-dev git-lfs`. Add `libgtk-3-dev libxdo-dev` only if muda stays on Linux (section 10, item 3).
- Vulkan: `mesa-vulkan-drivers vulkan-tools vulkan-validationlayers`. `ash` and `winit` load `libvulkan`, `libxkbcommon`, `libwayland-client` and the X11 libraries at run time, so they need no dev packages.
- Runtime: xdg-desktop-portal plus a backend for file dialogs (rfd uses the portal), PipeWire for system-audio capture (P4), FFmpeg libraries (P5).

**The 5080 machine**
- Distro: open (section 10, item 1). It needs a kernel recent enough for the 9950X3D's cache-aware scheduling and NVIDIA's current driver with the open kernel modules, the only kind that supports 50-series cards.
- Linux Vulkan SDK for the validation layers, and RenderDoc for captures.
- `git lfs pull` for `assets/plugins/DepthEstimator/models/midas_small.onnx`.

---

## 2. Build switches

| Today | Change |
|---|---|
| `manifold-gpu/src/lib.rs:65-72` selects Vulkan only with `feature = "vulkan"`. On Linux without it the Metal module fails to compile | Linux selects Vulkan by target: `any(feature = "vulkan", not(target_os = "macos"))`, and `ash` becomes a non-Mac dependency. The feature then only means "Vulkan on the Mac" (section 10, item 2) |
| No crate forwards `vulkan` | `manifold-app` gains `vulkan = ["manifold-gpu/vulkan"]` so the whole app builds against Vulkan on the Mac |
| `spirv-cross2` (MSL output) is an unconditional `manifold-gpu` dependency | Move it under the Mac target dependencies |
| `.cargo/config.toml:15` sets `rustc-wrapper = "/opt/homebrew/bin/sccache"` for every OS | `"sccache"`, found on `PATH` |
| `manifold-media/build.rs:1-22` and `manifold-recording/build.rs:2-17` gate with `#[cfg(target_os)]`, which is the host OS | Read `CARGO_CFG_TARGET_OS` |
| `manifold-fluids/native/flip_engine/pressuresolver.cpp:29-31,978-1075` includes `omp.h` and calls OpenMP when `!__APPLE__`. `build.rs` never passes `-fopenmp`, so expect a Linux link error. The Jacobi path is never used (`useJacobiSolve = false`, :925) | Take the Apple branch on every OS |
| `manifold-media/src/lib.rs:24` builds `still_exporter` everywhere, but its `image` dependency is Mac-only (`Cargo.toml:14-18`) | Make `image` unconditional |
| `muda` 0.19 pulls GTK 3 and `libxdo` (an unconditional `-lxdo`) on Linux, yet the Linux menu is never attached (`menu.rs:197`) | Mac-only dependency, if no menu bar on Linux is chosen (section 10, item 3) |
| `manifold-nodes/Cargo.toml:75-79` lists Mac dev-dependencies (`core-text`, `core-graphics`, `core-foundation`, `objc2`) nothing uses | Delete |
| `arboard` has only its X11 backend enabled | Enable `wayland-data-control` for the clipboard on Wayland |

---

## 3. GPU backend: the surface to build

Every row below exists on Metal and is missing on Vulkan. Phases are the design doc's (section 9). Section 11 shrinks several groups on Metal before any of this is ported.

| Group | Items (call sites) | Phase |
|---|---|---|
| Device | `new`, `new_queued` (73), `device_name` (65), `create_encoder` (879), `clone_queue`, `async_queue`, `prepare_utility_pipelines` | P1 |
| Buffers | `create_buffer` (57), `create_buffer_shared` (752), `try_create_buffer` (32), `try_create_buffer_shared` (36). `GpuBuffer`: `mapped_ptr` (409), `write`, `zero_fill` (66), `identity_key` (92), `ptr_eq` | P1 |
| Textures | `create_texture` (269), `try_create_texture` (9), `upload_texture` (127), `mip_level_view`, `layer_view`, `mip_level_count`, `is_shader_readable`. `GpuTexture` also carries `depth` (the stub lacks it) | P1 |
| MSAA and memoryless textures | `create_texture_msaa` (4), `create_texture_msaa_memoryless` (18), `create_texture_memoryless` | P2 |
| Samplers | `create_sampler` (180), `linear_sampler` (3); anisotropy (`types.rs` note in the stub) | P1 |
| Compute pipelines | `create_compute_pipeline` (354), `create_specialized_compute_pipeline` (23, WGSL string substitution, ports verbatim), `compute_pipeline_cache_len`, `max_threads_per_threadgroup`, `unbound_binding` | P1 |
| Pipeline caches | `load_pipeline_archive`, `save_pipeline_archive`, `load_msl_cache`, `log_msl_cache_stats` (D11) | P1 |
| Compute encoding | `dispatch_compute` (483), `dispatch_compute_indirect` (46), `compute_memory_barrier_buffers` (117), `pipeline_barrier` (no-op hint) | P1 |
| Copies and clears | `clear_buffer` (25), `clear_texture` (120), `copy_buffer_to_buffer` (149), `copy_buffer_range` (14), `copy_texture_to_buffer` (159), `copy_texture_3d_to_buffer` (4), `copy_texture_to_texture` (51), `copy_depth_to_float` (1) | P1 |
| Mipmaps | `generate_mipmaps` (20) | P2 |
| Submission | `commit` (497), `commit_and_wait_completed` (806), `try_commit_and_wait_completed` (8), `commit_and_wait_completed_timed` (17), `commit_and_continue` (6), `commit_wait_and_continue` (2), `commit_and_wait_scheduled` (2), `add_completed_handler` (9), `add_completed_handler_with_status`, `add_gpu_time_handler` (5), `tap_gpu_time` (5) | P1 (D8) |
| Events and lanes | `create_event` (8); `GpuEvent` (`current_value` 69, `signaled_value`, `is_done`, `wait_until_done`); `signal_event` (12), `signal_event_value`, `wait_event`; `use_async_lane`, `use_main_lane`, `finish_async_lane`, `wait_async_lane`; `GpuFenceWaiter` | P1 (D5, D7) |
| Frame lifetime | `frame_clock` (31), `FrameClock`, `RetireQueue`, `RetireMark`, `RetireSender`, `set_retirement`, `FrameFence`, `TexturePool` (`acquire`, `release`, `begin_frame`) | P1, after the hoist in section 9 step 4 |
| Memory | `modifier_memory_snapshot` (44, D18), `allocation_counts`, `resource_scope_id` | P1 |
| Water fast paths | `begin_replay`/`end_replay` (stubbed already), `begin_gated_segment`, `dispatch_compute_gated`, `end_gated_segments`, `GpuTemplateStore` (`prepare_template`, `execute_template`), `GatedRecorder`, `TemplateTicket`, `TemplateRanges` | P1 correct, P6 fast (design section 7) |
| Render pipelines | `create_render_pipeline` (35), `_depth` (10), `_depth_only` (10), `_msaa` (13), `_depth_msaa` (2), `_depth_msaa_point_size` (4), `_with_vertex_layout` (6), `create_specialized_render_pipeline_depth_msaa` (1), `create_depth_stencil_state` (17), `GpuDepthStencilState` | P2 |
| Draws | `draw_fullscreen` (15), `draw_fullscreen_viewport` (14), `draw_instanced` (12), `draw_instanced_msaa` (12), `depth_msaa_draw` (15) and its `_fill_mode`/`_points` forms, eight `draw_instanced_depth*` forms, `draw_indexed` (5), `begin_render_pass`/`draw_in_render_pass`/`end_render_pass`, `set_scissor_rect` (7); types `DepthMsaaDraw`, `DepthMsaaPassDesc`, `AuxColorAttachment`, `DrawCount` | P2 |
| Profiling | `create_timestamp_sampler` (7), `GpuTimestampSampler`, `GpuFrameProfile`, `GpuProfiledSpan`, `GpuWorkKind`, `ProfileGranularity`, `GpuSpanTimer`, `enable_dispatch_profiling` (8), `enable_profiling_at`, `profile_next_encoder` (6), `set_profile_tag` (14), `commit_and_wait_profiled` (19), `note_scope` | P2 (D17) |
| Presentation | `GpuSurface` (`create_surface`, `next_drawable`, `present`, `resize`, `set_maximum_drawable_count`), `GpuDrawable`, `set_presented_observer`; `configure_edr`, window level, gravity and `set_presents_with_transaction` report "not supported" | P3 (D15) |
| Shared textures | `create_io_surface_bgra8`, `create_texture_from_io_surface` become the neutral shared texture | P3 (design 6.1) |
| Metal-only, stay Metal | `raw_device`, `raw_device_ptr`, `raw_cmd_buf`, `raw_ptr`, `mtl4_bridge`, `metalfx`, `metalfx_m4`, `defer_drop`, `dispatch_compute_with_accel`, `create_compute_pipeline_half` (no callers, delete) | never |

**Hoist before porting.** These live in `metal/` but are backend-neutral logic that touches Metal in a handful of lines. Move them to shared modules with small backend hooks, so Vulkan does not copy them:
- `frame_fence.rs` (249 lines, no Metal calls)
- `retire.rs` (486, 7 Metal lines)
- `texture_pool.rs` (499, keeps a retained `MTLDevice`)
- `span_timer.rs` (241, reads command-buffer status)
- the ray-tracing data types in `raytrace/params.rs` (1,005) and `raytrace/emissive.rs` (692), plus the error, update and debug-ray types callers name (section 4)

---

## 4. Metal-only features that callers assume

Each becomes a backend-neutral API with a capability check (design 6.2). The Vulkan build then compiles without `target_os` gates in shared crates.

| Feature | Callers that break the Vulkan build | On Vulkan until built |
|---|---|---|
| Ray tracing | 61 ungated lines in 11 files: `manifold-nodes-scene/src/node_graph/primitives/render_scene.rs:181,182,1446,1447,1487-1491,1903,1911,1921`, `render_scene/{rt_frame,rt_trace,resources,subsurface,post_passes,rt_changes}.rs`, `manifold-node-engine/src/gpu/gpu_encoder.rs:30`, `manifold-nodes/src/registry.rs:74` (tracer prewarm), `manifold-app/src/content_export.rs:122`, `content_pipeline.rs:586` | Tracer unavailable, scenes use raster shadow maps. P7 |
| Denoiser | `render_scene.rs:180`, `render_scene/resources.rs:147,158`; the wrapper `manifold-nodes-scene/src/denoiser.rs` is Mac-only inside but declared ungated (`lib.rs:6`) and used ungated (`render_scene.rs:1188`, `resources.rs:1083`) | `denoiser_available()` false, stub type. P7 |
| MetalFX temporal upscale | `manifold-nodes-scene/src/metalfx_temporal_upscaler.rs`, used ungated at `render_scene.rs:1196`, `resources.rs:1123`, `render_scene/frame_setup.rs:1047` | One upscaler capability, unavailable. P7 |
| MetalFX spatial upscale | `manifold-compositor/src/metalfx_upscaler.rs` (declared ungated, `lib.rs:11`), used Mac-gated at `content_pipeline.rs:421,3620` | `fsr1.rs` stays the portable path |
| GPU FFT | `manifold-nodes-image/src/node_graph/primitives/inverse_fft_2d.rs:6,94`, `render_scene/ripple_detail.rs:106,156` (glob import), `ocean_spectrum.rs:627` (test) | Unavailable, the nodes say so. P7 |
| GPU faults and firmware | `gpu_fault` ungated at `manifold-app/src/content_commands.rs:270,313,722,727,754`, `content_export.rs:19,1367,1521`, `main.rs:504,526`; `firmware_reports` at `main.rs:533` | Both modules exist: device-lost maps to fault kinds (D19), firmware reports empty. P1 |
| Capture scopes | `manifold-app/src/app.rs:1593` | No-op (D20) |
| Residency | `content_pipeline.rs:785,1356,1372` (Mac-gated) | Empty |
| Raw handle in shared code | `render_scene/gltf_mesh_source.rs:979` uses `raw_cmd_buf()` as an id inside a trace log | Use a backend-neutral encoder id |

The other raw-handle users are already Mac-gated and stay Metal: the compositor and nodes-scene MetalFX and denoiser wrappers, `manifold-media/src/video_renderer.rs:405` and `metal_encoder.rs`, `manifold-recording/src/session.rs:102`, `texture_pool.rs:23,109`, `recording_thread.rs:150`, `manifold-app/src/content_export.rs:1625` and `content_pipeline.rs:1394`. Their Linux replacements are P5.

---

## 5. Shaders

- **WGSL:** 496 files, of which 267 are fragments joined by `include_str!`, `concat!` or `format!`. None use `f16`, subgroups, float or 64-bit atomics, binding arrays, `read_write` storage textures or a group other than 0. Storage textures are all write-only (132 declarations). Atomics are `atomic<u32>` and `atomic<i32>` only.
- **One BGRA storage write:** `manifold-recording/src/shaders/linear_to_srgb.wgsl:13`. Vulkan does not guarantee storage support for `B8G8R8A8_UNORM`. Check the format on NVIDIA in P5; otherwise write RGBA and swizzle.
- **Half precision:** `use_half` exists in `shader_common.rs`, but its only entry, `create_compute_pipeline_half` (`metal/device.rs:717`), has no callers. Design D10 has nothing to port.
- **Binding numbers** reach 72 (water shaders) with few distinct slots. `render_scene.wgsl` uses 37 distinct slots up to 57, and its composed entry points reach 53. Sparse Vulkan bindings are fine, and D2's one-set-per-draw has no 32-slot cap.
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

523 `target_os` lines in 51 files. Grouped by what they gate:

| Group | Lines | Files | Linux needs | Phase |
|---|---|---|---|---|
| Shared-texture bridge and every pane on it | ~199 | `shared_texture.rs`, `content_pipeline.rs` (66), `app.rs` (49), `frame/present.rs` (22), `scene_viewport_capture.rs` (12), `editor_bridge.rs` (11), `selection_outline.rs` (10), `perform_mode/render.rs` (8), `clip_atlas.rs` (5), `preview_navigation.rs`, `app_render.rs`, `editing_chrome.rs`, `gizmo_overlay.rs`, `scene_viewport.rs`, `content_commands.rs` (4), `main.rs:161,167` | Neutral shared texture in `manifold-gpu`, then remove the gates | P3 |
| Native content render and device plumbing | ~43 | `content_pipeline.rs` (`render_content_native` and friends) | Remove the gates once the GPU types exist on both backends. Without this nothing renders | P3 |
| Window, presentation, display link, pacing | ~107 | `edr_surface.rs` (14), `quit.rs` (13), `frame_timer.rs` (12), `sync_rig_log.rs` (9), `content_command.rs` (5), `app_lifecycle.rs` (5), `window_input.rs` (4), `content_thread.rs` (3), `drag_interpose.rs`, `breadcrumb.rs`, `present_feedback.rs`, `workspace.rs`, `window_registry.rs`, `main.rs:58` (display link) | Stubs exist off-Mac for most. Linux needs present-driven pacing, a fullscreen output window on a chosen display, and real-time thread priority in place of the Mac thread policy (`frame_timer.rs:285,294,403`) | P3, P4 |
| Keep the machine awake | 2 | `main.rs` IOPMAssertion lines | Idle/screensaver inhibit through the XDG portal or systemd-inhibit. Show-critical: a projector going to sleep mid-set | P4 |
| Export, still capture, recording, video and images | ~42 | `content_export.rs` (13), `content_thread.rs` (8), `content_commands.rs` (8), `app.rs` (4). Off-Mac clips get `StubRenderer` (`app.rs:1553`): no video and no images | FFmpeg decode and encode | P5 |
| Clipboard and Finder paste | ~20 | `input_host.rs` (11), `card_edit.rs`, `ui_snapshot/script.rs`, `window_input.rs`, `app.rs`, `main.rs:111` | Text clipboard is arboard everywhere. Pasting files from the file manager is Mac-only; on Linux it would come from a drag-and-drop URI list | P4 |
| Menus | 2 | `menu.rs:187-197` | Section 10, item 3 | P4 |
| Headless harness and subcommands | ~90 | `headless_harness.rs`, `main.rs` (11 + 11), `content_pipeline.rs` (12), proof modules (66 lines: `bug219_verify`, `corridor_acceptance`, `frame_exact_app`, `gap_start_probe`, `gpu_flip_export_demo`, `import_responsiveness_verify`, `mute_visibility_probe`, `rt_dynamic_export_tests`) | `perf-soak`, `frame-time`, `rt-capture`, `bridge-probe` and `export-repro` are Mac-gated. `ui-snap` needs only text and `new_queued`. The harness becomes backend-neutral with the bridge | P3 |
| Misc | 4 | `main.rs:352` (SIGPIPE, Linux needs it too), `main.rs:379,732` (App Nap, nothing on Linux) | Ungate SIGPIPE | P4 |

**Paths.** One per-OS directory helper should replace all of these (prefs, logs, caches, data, presets):
- Application Support: `user_prefs.rs:36` (already has a Linux arm), `main.rs:604,664`, `user_library.rs:83`
- Logs: `main.rs:480`, `session_log.rs:47,224`, `app_render.rs:267`
- Caches: `app.rs:1535-1538` (`pipeline_cache.metallib`, ungated), `headless_harness.rs:33-35`, `clip_thumb_cache.rs:181`
- Other tools and folders: Blender at `blender_import.rs:103`, `open` for Reveal at `user_library.rs:329` (use `xdg-open`), the `Desktop`/`Movies`/`Music` fallbacks at `file_dialogs.rs:434-436`
- Outside the app: `manifold-recording/src/config.rs:29-37`, `manifold-node-engine/src/load/preset_loader.rs:15,376-384,428-434` (`.app/Contents/Resources` and Application Support), `manifold-nodes-scene/.../decode_cache.rs:132`, `manifold-playback/src/percussion_backend.rs:210,485-496`
- Homebrew `ffmpeg` paths: `manifold-media/src/audio_muxer.rs:136`, `percussion_backend.rs:174`, `audio_sync.rs:135`, `journey_proof.rs:338`

**Linux compile breaks in manifold-app** (an ungated use of a Mac-only item; each needs a neutral type or a gate):
- **Text:** `CachedTextMeasure` at `ui_root/mod.rs:556,581` and `ui_snapshot/render.rs:656`; `text_rasterizer` at `app_render.rs:926`.
- **Surface:** `create_surface`, `configure_edr` and `surface.resize` at `app.rs:1181-1195,1897,1927` and `app_lifecycle.rs:1521-1537`; `GpuSurface` at `window_registry.rs:19`.
- **Pasteboard:** `macos_pasteboard` at `text_session.rs:539-547` and `window_input.rs:1350-1358`.
- **Panes:** `texture_pane` at `ui_frame.rs:144,889,1315,1392`; `ui_display_link` at `app_render.rs:2478`.
- **Frame fence:** `FrameFence` at `app.rs:304,1395` and `app_render.rs:2460,2471` (this one disappears once Vulkan has the type).
- **Native device:** `native_device()` at `content_thread.rs:964` and `content_commands.rs:530,922,1778`; `set_native_gpu` at `app.rs:1595`; `workspace_preview_size` at `app.rs:1439,1446`; `resize_workspace_preview` at `content_commands.rs:1514`.
- **Mac-gated content commands used ungated:** `SurfaceReady` at `content_thread.rs:350,354`, `ResizeOutputSurface` at `window_input.rs:676` and `app.rs:1910`, `SetOutputPresentSuspended` at `app.rs:2262`.
- **Export and video:** `run_export` at `content_thread.rs:334`; `video_renderer` at `content_thread.rs:810`.
- **Struct field:** `sync_arbiter` is an ungated field (`content_thread.rs:131`) with a Mac-gated initializer (`app.rs:1700-1701`).
- **Tests:** `headless_harness` is used ungated at `content_commands.rs:2200,2235,2269` and `content_thread/tests.rs:4`.

---

## 7. Other crates

| Crate | What breaks or is missing on Linux | Phase |
|---|---|---|
| manifold-ui-paint | All UI text is CoreText (`native_text.rs`, 2,172 lines, gated at `lib.rs:10`). Off-Mac `ui_renderer.rs:1597,2071` sets `has_text = false`. Five text proofs in `tests/contracts/` are Mac-only | P4: cosmic-text |
| manifold-nodes-image | `text_rasterizer.rs` (689 lines, CoreText) is gated, but `primitives/render_text.rs:26` and `glyph_atlas.rs:8` import it ungated (compile break). Default fonts "Helvetica" (`render_text.rs:506`, `text_rasterizer.rs:597`) and "Menlo" (`glyph_atlas.rs:30`) don't exist on Linux. The font list comes from CoreText (`:191-203`). `inverse_fft_2d.rs` uses `GpuFft` | P4: cosmic-text and fontdb, bundled or present defaults |
| manifold-media | Decode and export are Obj-C plugins on VideoToolbox and AVFoundation (`native/*.m`, 1,435 lines). `lib.rs:8-25` gates them. `DecoderPool`, `DecodeScheduler` and `MetalEncoder` are concrete types; the `ClipRenderer` trait (`manifold-playback/src/renderer.rs:10`) is the seam. `metadata.rs:19-29` returns `None` off-Mac. No FFmpeg crate in the graph today; only the `ffmpeg` CLI for audio muxing | P5 |
| manifold-recording | AVAssetWriter plugin (`native/LiveRecordingPlugin.m`, 839 lines). Everything but `config` and `ffi` is gated (`lib.rs:24-39`). Default path is the Desktop (`config.rs:29-37`). `proofs.rs` expects `ffprobe` | P5 |
| manifold-audio | System-audio capture is a CoreAudio process tap (`capture/process_tap.rs`, 792 lines); off-Mac `unsupported_tap.rs` returns an error. The device list already falls back to cpal. Mic permission returns Granted off-Mac | P4: PipeWire monitor capture (cpal 0.15 → 0.18, design section 8) |
| manifold-playback | Works as is: midir and cpal on ALSA, rusty_link (needs cmake and libclang), kira. Stem separation: the bundled demucs runtime exists only for macOS (`tools/audio_analysis/BundledRuntime/macOS`, `stage_runtime_mac.sh`). `percussion_backend.rs:440-448` picks `mps` on the Mac and `cpu` elsewhere, which should be `cuda` on the 5080 | P4 (runtime), P8 (CUDA) |
| manifold-native | The two ML plugins are Mach-O `.bundle`s with OpenCV dylibs (BlobDetector 32 MB, DepthEstimator 72 MB). The loader hardcodes `<name>.bundle/Contents/MacOS/<name>` (`ffi/mod.rs:12`). Their C++ sources are portable (`assets/plugins/BlobDetector/BlobDetectorPlugin.cpp`, 520 lines; `DepthEstimator/DepthEstimatorPlugin.cpp`, 1,464 lines); only `build.sh` is Mac-only. `region_ffi.rs:253-327` tests use `codesign` and `otool` | P4: build `lib<name>.so` against distro OpenCV, per-OS loader name |
| manifold-nodes-scene | Section 4 (ray tracing, denoiser, MetalFX, FFT). Decode cache under `Library/Caches` (`decode_cache.rs:132`) | P3 seams |
| manifold-node-engine | Ray-tracing types ungated (section 4). `exec/metal_backend.rs:1562-1640` tests compare `raw_ptr()` (gpu-proofs). Preset paths (section 6) | P3 |
| manifold-nodes | `registry.rs:74` prewarms the Metal tracer ungated. Dead Mac dev-dependencies (section 2) | P3 |
| manifold-nodes-water | `testkit/whitewater_scene.rs:528,666` call `objc2::rc::autoreleasepool` ungated | Gate it |
| manifold-compositor | MetalFX spatial wrapper (section 4) | P3 |
| manifold-physics | Thread count: Mac performance cores, `available_parallelism` elsewhere (`lib.rs:471-485`). On the 9950X3D that is 32 threads over two chiplets, only one with the extra cache | P8: measure |
| manifold-fluids | OpenMP (section 2). The `sheet_oracle.rs:245-260` test asserts an arm64 clang build of the oracle, so its baseline does not hold on x86_64 gcc | Test stays Mac-only |
| No findings | manifold-core, -editing, -foundation, -profiler, -ui, -spectral, -nodes-physics, -water-rigid, -water-liquid, -water-gpu-mpm, -water-surface, -water-whitewater | |

Out of scope: `plugins/` (the analyzer VST, its own workspace, uses IOSurface in `gpu_bridge.rs`) and `tv-led-mirror/` (a Mac tool).

---

## 8. Scripts and tests

**Scripts.**
- Portable: `wgsl_check.py`, `dev.py`, `run_ui_flows.py`, `gpu_queue.py` and `bridge_probe_gate.py`.
- `feature_matrix.py` checks `manifold-gpu --features vulkan` on its own. Once the skeleton lands, add an app-level Vulkan row.
- Mac-only: `launch_live_ui.py` (builds a `.app`), `rt_dynamic_acceptance.py` (`sysctl`, `system_profiler`, blocked off-Darwin), `sync_rig_report.py` (`~/Library/Logs`), `install-abletonosc-patch.sh` (Dropbox path), `assets/plugins/*/build.sh`, `tools/audio_analysis/stage_runtime_mac.sh` and `plugins/scripts/build-analyzer-vst-plugin.sh`.
- Hardcoded repo path `/Users/peterkiemann/MANIFOLD - Rust`: `rt_noise_gate.py:103`, `rt_quality_matrix.py:28`, `rt_toggle_matrix.py:46` and `claude-pane.sh:47`. Derive it from the script's own location.

**Tests.**
- 304 files are gated on `gpu-proofs` across 17 crates. They test node math, which is the same WGSL on both backends, so they run once at the end of P1 and P2, never per batch. The per-batch Vulkan check is the parity kit and stills (design section 10).
- Metal-only on purpose: 22 ray-tracing proofs (11 in `manifold-nodes/tests/gpu_proofs/rt_*`, 11 in `manifold-nodes-scene/tests/gpu_proofs/rt_*`) and `rt_p4_metalfx_temporal.rs` wait for P7. `manifold-gpu/src/metal/{async_lane,pipeline_cache,replay,template,residency}_tests.rs` get Vulkan twins where the behaviour exists; residency does not.
- `raw_ptr()` comparisons in `node-engine/exec/metal_backend.rs:1562-1640` and `manifold-app/tests/.../scene_viewport_frame_order.rs:97,107` compare `identity_key()` instead.
- About 21 Mac-gated test files (app probes, the ui-paint text proofs) come back once cosmic-text lands.

---

## 9. Work order

One batch at a time, and both builds stay green before the next one starts (design section 3). Workers write; the lead builds and tests.

**Before any Vulkan code, all on the Mac.** Each step shrinks or cleans what gets ported. The compiler catches most mistakes, the touched crates' unit tests catch the rest, and the steps that change behaviour add one still or probe each (section 11).
1. **Setup** (section 1). Peter installs the Vulkan SDK; the lead sets up the Linux VM.
2. **Build hygiene**, one batch: everything in section 2 except backend selection and the app feature, the dead functions and the stem-separation device from section 11, and gating the nodes-water `autoreleasepool`. Done when the Mac build and clippy are unchanged.
3. **Platform crate** (section 11): `manifold-platform` with directories, tool lookup, reveal, keep-awake, thread priority and performance cores. The scattered path and tool sites move onto it.
4. **Hoist neutral logic** out of `metal/` (section 3, "Hoist before porting"). Done when the build, clippy and `manifold-gpu`'s own tests pass.
5. **Shrink the GPU API** (section 11), one batch each: buffer use, the render-pipeline descriptor, the draw descriptor, commit forms, GPU timing.
6. **Frame bridge on one device** (section 11), proven with `bridge-probe`; the headless subcommands then boot the normal pipeline.
7. **Vulkan skeleton.**
   - Every section 3 row exists in `vulkan/` with the Metal signature and a `todo!("vulkan P1")` body.
   - Ray tracing, the denoiser, the upscaler and the FFT exist as unavailable capabilities.
   - Faults, firmware, capture scopes and residency are empty.
   - `manifold-app` gets the `vulkan` feature, and the section 4 callers stop assuming Metal.
   - Done when `cargo check --workspace --features manifold-app/vulkan` passes on the Mac. From here the compiler holds the remaining list.
8. **Linux compile.** In the VM, select the backend by target and run `cargo check --workspace`. Fix the compile breaks in sections 6 and 7 with gates or neutral stubs, no features yet. Done when the workspace compiles on Linux.

**P1, one batch each, in this order**
9. Instance, device and queues (D1, D5, D21), gpu-allocator, buffers by use, textures, samplers, memory snapshot (D18), debug names (D20).
10. Compute pipelines from SPIR-V, descriptor sets with update templates (D2), uniform ring (D3), pipeline and SPIR-V disk caches (D11).
11. Encoder: dispatch, indirect dispatch, copies, clears, uploads, readbacks, and the hazard and layout tracker (design section 5).
12. Submission: command pools (D6), timeline events, the waiter thread and completion handlers (D7, D8), async lanes, and the Vulkan hooks for the hoisted frame-lifetime types.
13. Water paths: replay spans, gated segments, templates as indirect dispatches (design section 7).

Each P1 batch adds its parity-kit tests and runs them on KosmicKrisp with synchronization validation. P1 is done when the compute kit is clean and the one full proof run at phase end shows no difference from Metal.

**After P1:**
- P2: render and profiling.
- P3: shared textures, the surface, and un-gating the app's native render and headless harness.
- P4: Linux services (text, pacing, paths, menus, keep-awake, system audio, plugins as `.so`).
- P5: video.
- P6: water speed.
- P7: ray tracing.
- P8: on the 5080.

P4's text work touches only `manifold-ui-paint` and `manifold-nodes-image`, so it can slot between GPU batches once step 8 lands.

**What a first Linux show needs:** P1 to P4. That covers generators, effects, raster scenes, water, audio input, MIDI, Link, OSC, LED and the output window. Video clips need P5. Ray-traced scenes need P7.

---

## 10. Decisions for Peter

1. **Distro on the 5080 machine.** Recommended: Ubuntu 26.04 LTS. It packages NVIDIA's open driver, the LunarG SDK targets Ubuntu, and it stays stable between gigs.
2. **Backend selection on Linux.** Recommended: by target. `cargo build` just works on Linux, and the `vulkan` feature only means "Vulkan on the Mac".
3. **Menu bar on Linux.** Recommended: none. muda's Linux menus need GTK windows, which winit doesn't make, and dropping it removes GTK and libxdo from the Linux build. Shortcuts and in-app menus cover the rig.
4. **Text on the Mac** (design 11.1). Recommended: cosmic-text everywhere after a side-by-side `ui-snap`.
5. Ray-tracing shader language, denoiser, GPU FFT and HAP stay where design section 11 puts them (P5 and P7).

---

## 11. Simplify while porting

Every API the Vulkan backend copies gets built twice and tested twice. So shrink it first, on Metal, where the GPU proofs already check the result, then port the smaller surface. Each item is its own batch, and Metal behaviour stays the same unless the row says otherwise.

**One place for each kind of difference**
- **GPU differences** live in `manifold-gpu` only (design section 3). Shared crates never ask which backend runs. A missing feature answers through a capability check.
- **OS differences** live in one new crate, `manifold-platform`. It holds plain functions with one signature each, with bodies per OS in `mac.rs`, `linux.rs` and later `windows.rs`, selected by `cfg` on the `mod` lines only. It owns:
  - app directories: prefs, logs, caches, data, presets
  - finding external tools: ffmpeg, ffprobe, the audio-analysis runtime
  - reveal in the file manager
  - keep-awake during a show
  - real-time thread priority
  - performance-core count
  - plugin library names
  - SIGPIPE

  It replaces the path and tool sites scattered through sections 6 and 7.
- **Big OS services** keep one seam each in their own crate:
  - text: cosmic-text everywhere means no per-OS code at all
  - system-audio capture: `CaptureBackend`, which already exists
  - video: `ClipRenderer`
  - window chrome: EDR and window level, in the app's window module
- **Target:** `target_os` appears only in `manifold-gpu`'s backend selection, `manifold-platform` and those seams. Today `manifold-app` alone has 523.

**GPU API reductions**

| Today | After | Checked by |
|---|---|---|
| `create_buffer`, `create_buffer_shared`, `try_create_buffer`, `try_create_buffer_shared` (581 production calls). Shared memory is the default for GPU-only scratch, CPU-filled inputs and readbacks alike. That costs nothing on Apple's unified memory; on the 5080 it decides whether a shader reads VRAM or crosses PCIe | One `create_buffer(size, use)` plus `try_`, where use is `Gpu`, `Upload` or `Readback`. On Metal, `Gpu` is Private and the other two are Shared. On Vulkan they are VRAM, CPU-writable VRAM (Resizable BAR, else system memory) and cached system memory | Mapping a `Gpu` buffer panics in debug builds, so a misclassified buffer fails loudly; then stills of the four `capture` presets. This is the biggest NVIDIA speed item in the port |
| 8 render-pipeline constructors | One `RenderPipelineDesc`: depth, MSAA, point size, vertex layout, specialisation | Compiler, then stills and one `ui-snap` |
| About 20 draw forms (`draw_instanced_depth*` alone has 8) | One draw call taking a descriptor: pass, viewport, depth, instance batch, topology | Same stills |
| 8 commit forms, one with no callers (`commit_and_wait_scheduled`) | `commit`, `commit_and_continue`, and `commit_and_wait` returning a Result | Compiler |
| 5 ways to read GPU time: `tap_gpu_time`, `add_gpu_time_handler`, `commit_and_wait_completed_timed`, `commit_and_wait_profiled`, `GpuSpanTimer` (plus `raytrace/trace_timing.rs`) | One per-submit GPU time stream that the profiler, the span timer and the perf tools all read, plus the per-dispatch profiler | One profiler session before and after |
| No callers: `create_compute_pipeline_half`, `create_texture_memoryless`, `supports_denoiser`, `set_motion_vector_scale`, `defer_probe_enabled` | Delete | Build |

**Bigger cuts**
- **Frame bridge on one device.** The content→UI handoff stores frames in IOSurfaces so two `GpuDevice`s can share them, and that drives about 199 of the app's Mac gates. Both devices call `MTLCreateSystemDefaultDevice` (`metal/device.rs:242`), so they are the same GPU. If both threads share one Metal device, each with its own queue (`clone_queue` already exists), the bridge can hold plain `GpuTexture`s and keep its slot and lease logic. That is also design D16's shape for Vulkan, so IOSurface goes and the bridge is the same code on both backends. Prove it with `bridge-probe` (zero torn reads) before cutting over.
- **Headless harness.** `perf-soak`, `frame-time`, `rt-capture`, `bridge-probe` and `export-repro` boot through a Mac-only harness that duplicates app setup. After the bridge change they boot the normal content pipeline.
- **One text engine.** The UI (2,172 lines) and the Text generator (689 lines) are two separate CoreText implementations. One cosmic-text module serves both.
- **MetalFX and denoiser wrappers** reach into raw Metal handles from `manifold-compositor` and `manifold-nodes-scene`. They move into `manifold-gpu` behind the upscaler and denoiser capabilities (design 6.2), and those crates lose all raw-handle code.
- **Stem-separation device.** `manifold-playback/src/percussion_backend.rs:439-448` hardcodes `mps` on the Mac and `cpu` elsewhere. The Python side already picks `mps`, `cuda` or `cpu` itself when given `auto` (`manifold_audio/external_tools.py:185-190`). Pass `auto` and delete the function, and the 5080 runs it on CUDA.

**Checked, not a problem:** the frame loop never waits on the GPU. The production `commit_and_wait_completed` calls (about 35) sit in warmup, bakes, thumbnails and tools.
