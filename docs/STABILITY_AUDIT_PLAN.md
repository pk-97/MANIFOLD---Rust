# MANIFOLD — Live Performance Stability Audit Plan

## Goal

Ensure MANIFOLD can run indefinitely without crashes, hitches, memory leaks,
precision drift, or sync issues during a live performance. Every finding must
include a file path and line number. No vague warnings.

## Codebase Stats (2026-03-28)

- 269 Rust files, ~99,270 lines, 12 crates
- ~70 WGSL shader files
- 400 `unsafe` blocks
- 354 `.unwrap()` / `.expect()` calls
- 1,735 numeric casts (`as u32` / `as i32` / `as f32`)
- 8 `autoreleasepool` calls
- 0 `panic::set_hook` — NO CRASH DIAGNOSTICS
- 0 `IOPMAssertion` — NO SLEEP PREVENTION
- 0 `forbid(unsafe_code)` — not even on manifold-core
- 46 `println` / `eprintln` — potential hot-path logging

---

## Agent Assignments

Each agent reads specific files deeply and answers a concrete checklist.
Classification per finding: **CRITICAL** (will crash/corrupt), **WARNING**
(degrades over time), **INFO** (worth noting), or **VERIFIED SAFE** with
one-line reasoning.

---

### STAB-1: Playback Engine & Sync

**Files to read completely:**
- `crates/manifold-playback/src/engine.rs` (1934 lines)
- `crates/manifold-playback/src/scheduler.rs`
- `crates/manifold-playback/src/sync.rs`
- `crates/manifold-playback/src/sync_source.rs`
- `crates/manifold-playback/src/transport_controller.rs`
- `crates/manifold-playback/src/video_time.rs`
- `crates/manifold-playback/src/active_window.rs` (619 lines)
- `crates/manifold-playback/src/renderer.rs`

**Checklist:**
1. What type is the master beat position accumulator? (`f32`, `f64`, or other?)
2. Is beat position ACCUMULATED via `+=` each frame (error compounds) or COMPUTED from an absolute reference (error bounded)?
3. What type are frame counters / tick counters? Can they overflow in a realistic timeframe?
4. Does `sync_clips_to_time()` use absolute or accumulated time?
5. How is delta_time calculated? `Instant::elapsed()` (good) or accumulated float (bad)?
6. What happens when BPM changes mid-playback? Is beat position recalculated or does it jump?
7. Is there a `Repeat` / loop math implementation? Does it use `t - (t/len).floor() * len` (correct) or `t % len` (wrong for negative)?
8. State machine: list all playback states and all transitions. Are any transitions missing? Can the state machine get stuck?
9. What scratch buffers / pre-allocated collections exist? Are any `Vec`s pushed to without clearing between frames?
10. Are there any `f32` values that should be `f64` for precision at large beat counts (>100K)?
11. Every `.unwrap()` and `.expect()` in these files — can any of them actually fail at runtime?
12. Any `as u32` / `as i32` from float without NaN/overflow guards?
13. Is clip progress calculated as `(current - start) / duration`? What happens if duration is zero?
14. `DataVersion` counter type and wrapping behavior?
15. Any HashMap iteration where order matters for determinism?

---

### STAB-2: MIDI, OSC & Live Input

**Files to read completely:**
- `crates/manifold-playback/src/midi_input.rs` (794 lines)
- `crates/manifold-playback/src/midi_clock_sync.rs` (736 lines)
- `crates/manifold-playback/src/live_clip_manager.rs` (926 lines)
- `crates/manifold-playback/src/osc_receiver.rs`
- `crates/manifold-playback/src/osc_sync.rs`
- `crates/manifold-playback/src/osc_param_router.rs`
- `crates/manifold-playback/src/osc_sender.rs`
- `crates/manifold-playback/src/osc_registry.rs`
- `crates/manifold-playback/src/clip_launcher.rs` (630 lines)
- `crates/manifold-playback/src/modulation.rs` (513 lines)
- `crates/manifold-playback/src/tempo_recorder.rs`
- `crates/manifold-playback/src/link_sync.rs`

**Checklist:**
1. `AtomicClockState` — what fields are packed into the `u64`? What bit widths? What `Ordering` is used for reads and writes?
2. Is there a CoreMIDI run loop? What thread is MIDI callback on? Can it race with content thread?
3. Phantom clip lifecycle: NoteOn creates → NoteOff commits. What if NoteOff never arrives (cable disconnected)? Is there a timeout or orphan sweep?
4. 5ms time guard between NoteOn/NoteOff — what happens at exactly 5ms? Off-by-one?
5. Channel filtering: does NoteOff check it came from the same channel as NoteOn?
6. MIDI device disconnect/reconnect — is it detected? Does it clean up in-flight state?
7. Clock tick counter: type? Can it overflow? What happens at rollover?
8. Phase accumulation for MIDI clock sync — accumulated deltas or absolute?
9. OSC receiver: UDP socket — is the receive buffer bounded? What happens if messages arrive faster than they're processed?
10. OSC strings: is there UTF-8 validation on incoming string parameters?
11. Rapid MIDI CC / OSC automation: are parameter updates batched per frame or does each trigger expensive work?
12. Clock source switching (internal ↔ external): does it reset accumulators cleanly?
13. Ableton Link sync: precision model, any accumulation drift?
14. Every `.unwrap()` in these files — any that could fail from external device state?
15. Any unbounded collections that grow with incoming messages?

---

### STAB-3: Content Thread & Pipeline

**Files to read completely:**
- `crates/manifold-app/src/content_thread.rs` (2002 lines)
- `crates/manifold-app/src/content_pipeline.rs` (767 lines)
- `crates/manifold-app/src/content_command.rs`
- `crates/manifold-app/src/content_state.rs`
- `crates/manifold-app/src/shared_texture.rs`
- `crates/manifold-app/src/frame_timer.rs`
- `crates/manifold-app/src/transport_state.rs`

**Checklist:**
1. Frame pacing: what drives the content thread tick? `thread::sleep` (poor precision), spin-wait, `CADisplayLink`, `mach_absolute_time`?
2. Is there an `autoreleasepool` wrapping each frame tick? (Content thread makes Metal API calls — without one, ObjC objects leak forever)
3. What channels exist between UI and content thread? Bounded or unbounded? What's the backpressure strategy?
4. If content thread takes longer than one frame period — does work queue up, drop frames, or block?
5. What happens if the UI thread is blocked (window drag, system dialog)? Does the content thread stall waiting for UI acknowledgment?
6. Lock acquisition order: list every lock acquired by the content thread and the order. Is there any path where order differs → deadlock?
7. Cleanup on playback stop: does `cleanup_stopped_clips()` cover ALL stateful effects and generators? List any gaps.
8. What happens when loading a new project while playing? Is all state torn down atomically?
9. Thread QoS: is the content thread set to `.userInteractive` or similar? Could macOS schedule it on an efficiency core?
10. IOSurface creation and management: reference counting correct across threads?
11. `channel.send()` — what happens if the receiver is dropped (other thread panicked)?
12. Every `thread::sleep` call in these files — how precise is it? Any on the frame-critical path?
13. Content thread shutdown: is it clean? Are GPU resources released? Command buffers committed?
14. Any `println!` / `eprintln!` on per-frame paths?
15. Stall recovery: if content thread stalls for 100ms then resumes — does it try to catch up (GPU overload) or skip to current time?

---

### STAB-4: GPU Core (manifold-gpu)

**Files to read completely:**
- `crates/manifold-gpu/src/metal/mod.rs` (2186 lines)
- `crates/manifold-gpu/src/metal/archive.rs`
- `crates/manifold-gpu/src/metal/mps.rs` (1451 lines)
- `crates/manifold-gpu/src/metal/metalfx.rs`
- `crates/manifold-gpu/src/types.rs`
- `crates/manifold-gpu/src/lib.rs`

**Checklist:**
1. Every `unsafe` block in this crate — list each one with: location, what it does, whether invariants are documented, whether they hold.
2. `autoreleasepool` — where are they? Is every frame tick wrapped? Is every thread that calls Metal API wrapped?
3. Metal allocation failure: does `makeTexture()` / `makeBuffer()` return nil check or unwrap? What happens under memory pressure?
4. Encoder state machine: is there protection against encoding after `endEncoding`? Against multiple active encoders on one command buffer?
5. Texture pool: what is the frame-stamp type? Can it overflow? Is there a max pool size cap? What happens on resolution change?
6. Binary archive (`MTLBinaryArchive`): what happens if the cache file is corrupted? Does it fall back to runtime compilation? Is the fallback path tested?
7. Pipeline creation: are all function constant variants pre-compiled at startup? Or can a new combination trigger runtime compilation mid-show?
8. Command buffer completion handlers: what do they capture? Can captured references outlive the originating scope?
9. `MTLEvent` signal/wait: what values are used? Is ordering guaranteed? Can a wait happen before the signal is encoded → GPU deadlock?
10. Drop ordering: if a struct has both `GpuTexture` and `GpuDevice`, is the texture guaranteed to drop before the device?
11. Resource hazard tracking: are any resources created with `untracked` mode? If so, are barriers manually inserted correctly?
12. Compute dispatch sizes: is there validation that threadgroup count > 0? That dispatch size matches buffer size?
13. GPU timeout: if a command buffer takes >5s (stuck shader), is `MTLCommandBufferStatus::Error` handled?
14. Metal capture manager / debug groups: are any left enabled in release builds?
15. `set_fast_math_enabled(true)` — are there any shaders where fast math changes correctness? (NaN handling, Inf propagation, denormal flush)

---

### STAB-5: Compositor & Rendering Pipeline

**Files to read completely:**
- `crates/manifold-renderer/src/layer_compositor.rs` (1229 lines)
- `crates/manifold-renderer/src/compositor.rs`
- `crates/manifold-renderer/src/effect_chain.rs`
- `crates/manifold-renderer/src/effect_registry.rs`
- `crates/manifold-renderer/src/effect.rs`
- `crates/manifold-renderer/src/generator_renderer.rs` (646 lines)
- `crates/manifold-renderer/src/generator.rs`
- `crates/manifold-renderer/src/generator_context.rs`
- `crates/manifold-renderer/src/render_target_pool.rs`
- `crates/manifold-renderer/src/render_target.rs`
- `crates/manifold-renderer/src/uniform_arena.rs`
- `crates/manifold-renderer/src/blit.rs`
- `crates/manifold-renderer/src/gpu.rs`
- `crates/manifold-renderer/src/gpu_encoder.rs`
- `crates/manifold-renderer/src/gpu_types.rs`
- `crates/manifold-renderer/src/gpu_readback.rs`
- `crates/manifold-renderer/src/background_worker.rs`

**Checklist:**
1. Layer ordering: what determines composite order? HashMap iteration (non-deterministic) or explicit sort?
2. Empty composition: zero visible layers → what happens? Black output, or index-out-of-bounds?
3. NaN/Inf propagation: if a generator outputs NaN, does it propagate through the compositor to all subsequent layers?
4. Effect execution order: is it deterministic? Based on what?
5. Mid-frame layer mutation: if a layer is added/removed while compositor is encoding → partial state?
6. Blend mode with extreme inputs: Inf, NaN, negative alpha. Any division by zero in blend math?
7. Opacity == 0.0: is the layer skipped (optimization)? Is the check `< 0.0` or `<= 0.0`?
8. Render target pool: growth cap? Stale format handling after resolution change? Frame stamp overflow?
9. Uniform arena: per-frame allocation or recycled? Growth behavior?
10. Effect added during playback: does pipeline creation happen on the hot path? Can it cause a multi-second hitch?
11. Effect removed during render: is the effect list snapshotted or live?
12. Generator parameter update during GPU dispatch: is the uniform buffer double-buffered?
13. Async compute: per-layer command buffers → how many can be in-flight? Metal limit (~64)?
14. Single-layer fast path: does it correctly skip multi-command-buffer overhead?
15. Per-owner effect cleanup: list every stateful effect. Does `cleanup_stopped_clips()` cover ALL of them?

---

### STAB-6: Effects Deep Dive

**Files to read completely (all effects):**
- `crates/manifold-renderer/src/effects/blob_tracking.rs` (820 lines)
- `crates/manifold-renderer/src/effects/wireframe_depth.rs` (1981 lines)
- `crates/manifold-renderer/src/effects/stylized_feedback.rs`
- `crates/manifold-renderer/src/effects/depth_of_field.rs` (570 lines)
- `crates/manifold-renderer/src/effects/bloom.rs`
- `crates/manifold-renderer/src/effects/halation.rs`
- `crates/manifold-renderer/src/effects/glitch.rs`
- `crates/manifold-renderer/src/effects/infrared.rs`
- `crates/manifold-renderer/src/effects/chromatic_aberration.rs`
- `crates/manifold-renderer/src/effects/dither.rs`
- `crates/manifold-renderer/src/effects/edge_detect.rs`
- `crates/manifold-renderer/src/effects/edge_stretch.rs`
- `crates/manifold-renderer/src/effects/color_grade.rs`
- `crates/manifold-renderer/src/effects/kaleidoscope.rs`
- `crates/manifold-renderer/src/effects/mirror.rs`
- `crates/manifold-renderer/src/effects/quad_mirror.rs`
- `crates/manifold-renderer/src/effects/strobe.rs`
- `crates/manifold-renderer/src/effects/transform.rs`
- `crates/manifold-renderer/src/effects/invert_colors.rs`
- `crates/manifold-renderer/src/effects/voronoi_prism.rs`
- `crates/manifold-renderer/src/effects/fragment_blit_helper.rs`
- `crates/manifold-renderer/src/effects/compute_dual_blit_helper.rs`

**Checklist (per effect):**
1. Feedback loops (stylized_feedback, any with persistent state): is there a NaN guard on the feedback texture read? Can one NaN frame corrupt all subsequent frames forever?
2. Per-owner state: stored in `AHashMap<i64, T>`? Is there a cleanup path when the owner (clip) stops?
3. Compute dispatch: is threadgroup count validated > 0? Does dispatch size match buffer/texture dimensions?
4. Uniform struct alignment: `#[repr(C)]`, 16-byte aligned, `_pad` fields present, field order matches WGSL?
5. Texture creation: format correct? Usage flags include needed capabilities? Allocation failure handled?
6. Division in shader parameter calculation: any `1.0 / param` where param could be 0?
7. `as u32` / `as i32` casts from float parameters: any without `.round()` first? Any that could receive NaN?
8. Resolution scaling: half-res textures — is dimension guaranteed non-zero? (width=1 → 0.5 → 0 after cast)
9. Multi-pass effects: is the pass count correct? Is intermediate texture lifecycle correct?
10. Texture size calculation: does it use source texture dimensions or target? (Texel size from SOURCE)

---

### STAB-7: Generators Deep Dive

**Files to read completely (all generators):**
- `crates/manifold-renderer/src/generators/fluid_simulation.rs` (868 lines)
- `crates/manifold-renderer/src/generators/fluid_simulation_3d.rs` (724 lines)
- `crates/manifold-renderer/src/generators/mycelium.rs` (552 lines)
- `crates/manifold-renderer/src/generators/parametric_surface.rs`
- `crates/manifold-renderer/src/generators/plasma.rs`
- `crates/manifold-renderer/src/generators/basic_shapes_snap.rs`
- `crates/manifold-renderer/src/generators/concentric_tunnel.rs`
- `crates/manifold-renderer/src/generators/tesseract.rs`
- `crates/manifold-renderer/src/generators/wireframe_zoo.rs`
- `crates/manifold-renderer/src/generators/lissajous.rs`
- `crates/manifold-renderer/src/generators/oscilloscope_xy.rs`
- `crates/manifold-renderer/src/generators/duocylinder.rs`
- `crates/manifold-renderer/src/generators/mri_volume.rs`
- `crates/manifold-renderer/src/generators/mri_volume_loader.rs`
- `crates/manifold-renderer/src/generators/stateful_base.rs`
- `crates/manifold-renderer/src/generators/compute_common.rs`
- `crates/manifold-renderer/src/generators/line_pipeline.rs`
- `crates/manifold-renderer/src/generators/generator_math.rs`
- `crates/manifold-renderer/src/generators/registry.rs`

**Checklist (per generator):**
1. Stateful generators (fluid, mycelium, etc.): per-owner state cleanup when clip stops?
2. NaN/Inf in simulation: can any simulation step produce NaN? (div by zero, sqrt of negative, log of zero) Does NaN propagate through feedback?
3. Iterative solvers (fluid simulation): is the loop bounded? Can it run forever?
4. Compute dispatch dimensions: validated > 0? Matches allocated texture/buffer size?
5. 3D volume textures: after N blur passes, is the result in the expected texture (temp vs main)?
6. Time/beat value used in shader: type? Precision at large values?
7. Resolution scaling factor: can it produce zero dimensions?
8. Uniform buffer: double-buffered or written while GPU reads?
9. Texture pool usage: correctly recycled? Frame stamp updated?
10. Every `as u32` / `as f32` cast — safe inputs guaranteed?

---

### STAB-8: Shader Safety (WGSL)

**Files to read completely (highest-risk shaders):**
- `crates/manifold-renderer/src/generators/shaders/fluid_simulate.wgsl` (241 lines)
- `crates/manifold-renderer/src/generators/shaders/fluid_simulate_3d.wgsl` (525 lines)
- `crates/manifold-renderer/src/generators/shaders/fluid_scatter.wgsl` (168 lines)
- `crates/manifold-renderer/src/generators/shaders/fluid_scatter_3d.wgsl` (208 lines)
- `crates/manifold-renderer/src/generators/shaders/mycelium_agent_update.wgsl`
- `crates/manifold-renderer/src/generators/shaders/mycelium_diffuse.wgsl`
- `crates/manifold-renderer/src/effects/shaders/fx_stylized_feedback.wgsl`
- `crates/manifold-renderer/src/effects/shaders/fx_stylized_feedback_compute.wgsl`
- `crates/manifold-renderer/src/effects/shaders/fx_blob_tracking.wgsl` (297 lines)
- `crates/manifold-renderer/src/effects/shaders/fx_blob_tracking_compute.wgsl`
- `crates/manifold-renderer/src/effects/shaders/fx_wireframe_depth.wgsl` (839 lines)
- `crates/manifold-renderer/src/effects/shaders/fx_wireframe_depth_compute.wgsl` (869 lines)
- `crates/manifold-renderer/src/generators/shaders/compositor_blend.wgsl` (176 lines)
- `crates/manifold-renderer/src/generators/shaders/compositor_blend_compute.wgsl`

**Checklist:**
1. Every `loop` and `while` — is there a guaranteed exit condition? Maximum iteration count?
2. Every division — can the divisor be zero? (especially from uniform parameters)
3. Every `pow()`, `log()`, `sqrt()`, `atan2()` — edge case inputs?
4. Texture sampling: bounds checking in compute kernels? `textureLoad` with out-of-bounds coordinates?
5. Workgroup size: ≤256 total invocations? (16×16 for 2D, 4×4×4 for 3D on Apple Silicon)
6. Boundary threads: in compute shaders, do edge threads (beyond texture dimensions) early-return?
7. Feedback reads: is there any NaN sanitization on values read from previous frame?
8. `var<workgroup>` total memory: does it exceed 32KB device limit?
9. Integer overflow in index calculations?
10. Any `select()` or conditional where both branches are evaluated and one could produce Inf/NaN?

---

### STAB-9: Unsafe & FFI Audit

**Cross-crate — grep for `unsafe`, `extern "C"`, `msg_send`, raw pointers.**

**Files with highest unsafe density (read completely):**
- `crates/manifold-gpu/src/metal/mod.rs` (2186 lines — likely bulk of unsafe)
- `crates/manifold-gpu/src/metal/mps.rs` (1451 lines)
- `crates/manifold-media/src/decoder_ffi.rs`
- `crates/manifold-media/src/metal_ffi.rs`
- `crates/manifold-media/src/metal_encoder.rs`
- `crates/manifold-native/src/ffi/blob_ffi.rs`
- `crates/manifold-native/src/ffi/depth_ffi.rs`
- `crates/manifold-native/src/ffi/mod.rs`
- `crates/manifold-native/src/blob_detector.rs`
- `crates/manifold-native/src/depth_estimator.rs`
- `crates/manifold-app/src/edr_surface.rs`
- `crates/manifold-app/src/shared_texture.rs`

**Checklist:**
1. Categorize every `unsafe` block: objc messaging, raw pointer deref, FFI call, transmute, pointer arithmetic, other.
2. For each: is there a `// SAFETY:` comment? Are the stated invariants actually upheld?
3. Every `extern "C"` function: is the Rust-side signature correct? Types match C/ObjC side?
4. Native plugins (BlobDetector, DepthEstimator): are C++ exceptions caught before crossing FFI? Can the plugin return null?
5. Plugin output validation: is data from plugins (blob positions, depth maps) validated before use in rendering?
6. Raw pointer lifetimes: any pointer that could outlive the data it points to? (especially in Metal completion handlers, MIDI callbacks)
7. `transmute` usage: do source and target types have the same size and alignment?
8. Double-free risk: any `Box::from_raw` that could be called twice?
9. Mutable aliasing: any two `*mut T` that could point to the same memory?
10. `NSException` risk: any ObjC method call that could throw? Is it caught?
11. Signal handler conflicts: do any plugins install signal handlers?
12. Drop ordering of FFI resources: if a Rust struct holds both a Metal device and Metal textures, do textures drop first?
13. `autoreleasepool` coverage: is every thread that makes ObjC calls wrapped? Especially the content thread frame loop?
14. IOSurface: retain/release balanced? Can one thread drop while another reads?
15. ABI compatibility: are native plugin dylibs compiled with matching SDK?

---

### STAB-10: Numeric Cast Audit

**Cross-crate — grep for `as u32`, `as i32`, `as f32`, `as usize`.**
Focus on hot paths: `manifold-playback`, `manifold-renderer`, `manifold-gpu`.

**Checklist:**
1. Every `float as u32` / `float as i32`: can the float be NaN? Infinity? Negative (for unsigned)?
2. Every `f64 as f32`: is precision loss acceptable? (especially beat positions fed to shader uniforms)
3. Every `i32 as usize`: can the i32 be negative → massive usize?
4. Every `usize as u32`: can the usize exceed u32::MAX?
5. Every integer arithmetic on counters/indices: can it overflow in release mode (silent wrap)?
6. Signed/unsigned mixing in index calculations?
7. Float equality comparisons (`== 0.0`) where epsilon comparison is needed?
8. Integer division where Unity equivalent used float division with rounding?
9. `Duration::from_secs_f64` — can the input be NaN or negative? (panics)
10. `Instant` subtraction — can `b > a`? (panics)

---

### STAB-11: Platform, OS & Live Environment

**Files to read:**
- `crates/manifold-app/src/main.rs`
- `crates/manifold-app/src/app.rs` (1981 lines)
- `crates/manifold-app/src/app_lifecycle.rs` (632 lines)
- `crates/manifold-app/src/app_render.rs` (1192 lines)
- `crates/manifold-app/src/window_registry.rs`
- `crates/manifold-renderer/src/surface.rs`
- `crates/manifold-renderer/src/ui_renderer.rs` (893 lines)
- `crates/manifold-renderer/src/tonemap_blit.rs`
- `crates/manifold-renderer/src/layer_bitmap_gpu.rs` (425 lines)

**Checklist:**
1. `panic::set_hook` — does it exist? If not, crashes produce zero diagnostics with `panic = "abort"`.
2. `IOPMAssertion` or equivalent — does it exist? Without it, macOS will sleep/dim during the show.
3. App Nap prevention: is it disabled? (`NSProcessInfo.processInfo.beginActivity`)
4. wgpu surface lost/outdated handling: does `get_current_texture()` error get caught and recovered?
5. wgpu device lost: is there an error handler? Recovery path?
6. Display scale factor change: handled without crash?
7. Fullscreen transitions: does winit handle them cleanly on macOS? Any known issues?
8. Window resize during playback: is the resize event handled without blocking content thread?
9. `CAMetalLayer` drawable exhaustion: can `nextDrawable()` block the UI thread?
10. File descriptor count: how many IOSurfaces, Metal resources, open files could accumulate?
11. Thread count: how many threads can be spawned? (14 `thread::spawn` found — but nested?)
12. Core dump / backtrace availability in release build (stripped symbols + panic=abort)?
13. Environment variable sensitivity: does the app check `METAL_DEVICE_WRAPPER_TYPE`, `MTL_DEBUG_LAYER`?
14. macOS notification / system dialog stealing focus: any protection?
15. Multiple app instances: any protection against running two copies?

---

### STAB-12: Media & Video Decode

**Files to read completely:**
- `crates/manifold-media/src/decode_scheduler.rs` (444 lines)
- `crates/manifold-media/src/video_renderer.rs` (637 lines)
- `crates/manifold-media/src/decoder.rs`
- `crates/manifold-media/src/decoder_ffi.rs`
- `crates/manifold-media/src/metal_ffi.rs`
- `crates/manifold-media/src/metal_encoder.rs`
- `crates/manifold-media/src/export_session.rs`
- `crates/manifold-media/src/audio_muxer.rs`

**Checklist:**
1. Video decode backpressure: if decode is slower than playback rate, what happens? Unbounded queue? Frame drop?
2. Decode error recovery: corrupted frame, codec error, disk read error → crash or graceful fallback?
3. Seek accuracy: for looping short clips, is seek-to-keyframe sufficient or is decode-to-target needed?
4. Memory growth: does the decode pipeline allocate per-frame or reuse buffers?
5. FFI safety: all C function calls validated? Null return checks?
6. Metal hardware decoder integration: is the texture from hardware decode correctly integrated into the render pipeline?
7. Multiple simultaneous video clips: resource contention? Thread safety?
8. Video file handle: is it kept open for the entire session? File descriptor accumulation?
9. Memory-mapped files: is the video file mmap'd? What happens if the file is moved/deleted during playback? (SIGBUS)
10. Export session: can it run alongside live playback without contention?

---

### STAB-13: Data Integrity & Serialization

**Files to read completely:**
- `crates/manifold-io/src/saver.rs`
- `crates/manifold-io/src/loader.rs`
- `crates/manifold-io/src/archive.rs` (754 lines)
- `crates/manifold-io/src/migrate.rs`
- `crates/manifold-io/src/manifest.rs`
- `crates/manifold-io/src/path_resolver.rs`
- `crates/manifold-core/src/types.rs` (909 lines)
- `crates/manifold-core/src/id.rs`
- `crates/manifold-core/src/project.rs`

**Checklist:**
1. Save atomicity: does save write to temp file + atomic rename, or in-place? (Crash during save = lost project?)
2. `fsync` after write?
3. Serialization thread safety: if autosave runs during playback, is the project state snapshotted or read live?
4. Deserialization of untrusted project files: deeply nested JSON → stack overflow? Huge strings → OOM?
5. serde attributes consistency: all serialized structs have `rename_all = "camelCase"`?
6. Default values on fields added after V1: `#[serde(default)]` present?
7. Typed ID edge cases: empty string `""`, very long strings, special characters?
8. Version migration: can every old format be loaded? Are there gaps?
9. File path handling: Unicode normalization (NFD vs NFC on macOS)?
10. Disk full during save: handled or silent corruption?

---

## Execution Order

**Batch 1 (highest risk, independent):**
STAB-1 (playback engine), STAB-3 (content thread), STAB-4 (GPU core) — in parallel

**Batch 2 (rendering pipeline):**
STAB-5 (compositor), STAB-6 (effects), STAB-7 (generators) — in parallel

**Batch 3 (cross-cutting):**
STAB-9 (unsafe/FFI), STAB-10 (numeric casts), STAB-8 (shaders) — in parallel

**Batch 4 (supporting systems):**
STAB-2 (MIDI/OSC), STAB-11 (platform), STAB-12 (media), STAB-13 (data) — in parallel

## Output Format

Each agent produces a report with sections:

```
## CRITICAL (will crash or corrupt)
- [file:line] Description of issue

## WARNING (degrades over time)
- [file:line] Description of issue

## INFO (worth noting)
- [file:line] Description of issue

## VERIFIED SAFE
- [Category] One-line reasoning why this is safe
```
