# MANIFOLD — Live Performance Stability Audit Agent

You are auditing a Rust codebase for live performance stability — the app must
run indefinitely without crashes, hitches, memory leaks, precision drift, or
sync issues. This is **research-only** — you will NOT modify any code. You will
write all findings to `docs/STABILITY_AUDIT_REPORT.md`.

## CRITICAL INSTRUCTIONS

- **DO NOT MODIFY ANY SOURCE CODE.** Read only. Your output is a report file.
- **DO NOT STOP OR ASK FOR INPUT.** Complete all 13 tasks autonomously.
- **Write findings incrementally** to `docs/STABILITY_AUDIT_REPORT.md` — append each section as you complete it so work is not lost if context limits are hit.
- **Every finding MUST include `file_path:line_number`.** No vague warnings.
- **Classify every finding:** CRITICAL (will crash/corrupt during a show), WARNING (degrades over hours), INFO (worth noting), or VERIFIED SAFE (with one-line reason).
- The codebase root is `/Users/peterkiemann/MANIFOLD - Rust`.
- Read `CLAUDE.md` at the root first for full architectural context.

## CONTEXT

This is a Visual DAW for live performance. Two-thread model: content thread
(playback, rendering, GPU) at 60fps, UI thread (winit event loop). Native Metal
GPU on the content thread (zero wgpu). Communication via crossbeam channels +
IOSurface zero-copy. The app must survive 4+ hour shows without degradation.

## CODEBASE STATS

- 269 Rust files, ~99K lines, 12 crates
- 400 `unsafe` blocks, 354 `.unwrap()`/`.expect()`, 1735 numeric casts
- 8 `autoreleasepool`, 23 `extern "C"`, 14 `thread::spawn`
- 0 `panic::set_hook`, 0 `IOPMAssertion`, 0 `forbid(unsafe_code)`
- 21 `thread::sleep`, 43 `Instant::now`, 46 `println`/`eprintln`
- 6 `Arc<Mutex>`, 43 crossbeam usages, 102 AHashMap usages

---

## TASK 1: Playback Engine & Sync Precision

**Goal:** Determine if the timing system drifts over hours of playback.

**Files to read completely:**
- `crates/manifold-playback/src/engine.rs` (1934 lines)
- `crates/manifold-playback/src/scheduler.rs`
- `crates/manifold-playback/src/sync.rs`
- `crates/manifold-playback/src/sync_source.rs`
- `crates/manifold-playback/src/transport_controller.rs`
- `crates/manifold-playback/src/video_time.rs`
- `crates/manifold-playback/src/active_window.rs`

**Answer these exact questions:**
1. What type is the master beat position? (`f32`/`f64`/other?) — report the exact variable name and line.
2. Is beat position ACCUMULATED via `+=` each frame (error compounds over time) or COMPUTED from an absolute reference like `start_time + elapsed * bpm / 60` (error stays bounded)?
3. What type are frame counters / tick counters? At 60fps, when do they overflow? (`u32` = 828 days, `u64` = 9.8 billion years)
4. How is delta_time calculated? `Instant::elapsed()` (monotonic, good) or float accumulation (drift)?
5. What happens when BPM changes mid-playback? Is beat position recomputed from an anchor or does it jump?
6. Is there loop/repeat math? Does it use `t - (t/len).floor() * len` (correct) or `t % len` (wrong for negatives)?
7. Playback state machine: list ALL states and ALL transitions. Can it get stuck in any state?
8. Are there any pre-allocated scratch buffers that grow without clearing between frames?
9. Every `.unwrap()` in these files — list any that could fail at runtime.
10. Every `as u32`/`as i32`/`as f32` — list any where input could be NaN, Inf, or out of range.
11. Is clip progress `(current - start) / duration`? What if duration == 0?
12. `DataVersion` counter: type? What happens at overflow?

**Write to report, then continue to Task 2.**

---

## TASK 2: MIDI, OSC & Live Input Safety

**Goal:** Determine if MIDI/OSC handling can leak state or crash from real-world input.

**Files to read completely:**
- `crates/manifold-playback/src/midi_input.rs` (794 lines)
- `crates/manifold-playback/src/midi_clock_sync.rs` (736 lines)
- `crates/manifold-playback/src/live_clip_manager.rs` (926 lines)
- `crates/manifold-playback/src/clip_launcher.rs`
- `crates/manifold-playback/src/osc_receiver.rs`
- `crates/manifold-playback/src/osc_sync.rs`
- `crates/manifold-playback/src/osc_param_router.rs`
- `crates/manifold-playback/src/modulation.rs`
- `crates/manifold-playback/src/tempo_recorder.rs`
- `crates/manifold-playback/src/link_sync.rs`

**Answer these exact questions:**
1. `AtomicClockState`: what fields are packed into the u64? Bit widths? What `Ordering` for reads/writes?
2. MIDI callback thread: what thread does it run on? Can it race with content thread reads?
3. Phantom clips: NoteOn creates, NoteOff commits. What happens if NoteOff NEVER arrives (MIDI cable disconnected)? Is there a timeout or orphan sweep?
4. The 5ms time guard: off-by-one at exactly 5ms?
5. Channel filtering: NoteOff checks same channel as NoteOn?
6. MIDI device disconnect: detected? Auto-reconnect? In-flight state cleanup?
7. Clock tick counter type: can it overflow?
8. MIDI clock phase: accumulated deltas (drift) or absolute?
9. OSC UDP receive buffer: bounded? What happens on burst?
10. OSC string parameters: UTF-8 validated?
11. Rapid MIDI CC/OSC automation (1000+/sec): batched per frame or each triggers work?
12. Clock source switch (internal↔external): accumulators reset cleanly?
13. Any unbounded collection that grows with incoming messages?

**Write to report, then continue to Task 3.**

---

## TASK 3: Content Thread & Frame Pipeline

**Goal:** Determine if the content thread can stall, leak, or deadlock.

**Files to read completely:**
- `crates/manifold-app/src/content_thread.rs` (2002 lines)
- `crates/manifold-app/src/content_pipeline.rs` (767 lines)
- `crates/manifold-app/src/content_command.rs`
- `crates/manifold-app/src/content_state.rs`
- `crates/manifold-app/src/shared_texture.rs`
- `crates/manifold-app/src/frame_timer.rs`
- `crates/manifold-app/src/transport_state.rs`

**Answer these exact questions:**
1. Frame pacing mechanism: `thread::sleep` (poor, ~1ms jitter), spin-wait, `mach_absolute_time`, other?
2. Is there an `autoreleasepool` wrapping each frame tick? (Metal API calls create ObjC autoreleased objects — without the pool, they leak FOREVER on a non-main thread)
3. UI↔Content channels: bounded or unbounded? Capacity? Backpressure strategy?
4. Content thread overrun: if a frame takes longer than 16.6ms, what happens? Queue up? Drop? Block?
5. UI thread blocked (window drag, system dialog): does content thread stall waiting?
6. Lock ordering: list every lock acquired, in what order, on what thread. Any conflicting order = deadlock.
7. `cleanup_stopped_clips()`: does it cover ALL stateful effects and generators? List any gaps.
8. Project switch during playback: all state torn down? List what persists.
9. Thread QoS: is the content thread marked userInteractive/userInitiated?
10. `channel.send()` when receiver dropped: caught or panic?
11. Every `thread::sleep` in these files: on the frame-critical path?
12. Content thread shutdown: GPU resources released? Command buffers committed?
13. Every `println!`/`eprintln!` in these files: on per-frame paths?
14. Stall recovery: after 100ms stall, catch-up (GPU flood) or skip-to-current?

**Write to report, then continue to Task 4.**

---

## TASK 4: GPU Core Safety (manifold-gpu)

**Goal:** Determine if Metal API usage can crash, leak, or deadlock the GPU.

**Files to read completely:**
- `crates/manifold-gpu/src/metal/mod.rs` (2186 lines)
- `crates/manifold-gpu/src/metal/archive.rs`
- `crates/manifold-gpu/src/metal/mps.rs` (1451 lines)
- `crates/manifold-gpu/src/metal/metalfx.rs`
- `crates/manifold-gpu/src/types.rs`
- `crates/manifold-gpu/src/lib.rs`

**Answer these exact questions:**
1. List every `unsafe` block with: line number, what it does, whether `// SAFETY:` comment exists.
2. `autoreleasepool` placement: where? Is every Metal API call path covered?
3. Texture/buffer allocation: does `makeTexture`/`makeBuffer` return a nil check or unwrap? What under memory pressure?
4. Encoder state: protection against encoding after `endEncoding`? Multiple active encoders?
5. Texture pool: frame-stamp type? Can overflow? Max pool size cap? Resolution change handling?
6. Binary archive corruption: fallback to runtime compilation? Or crash?
7. Function constant pipeline variants: all pre-compiled? Or runtime compilation possible mid-show?
8. Completion handler captures: can references outlive their scope?
9. MTLEvent signal/wait ordering: guaranteed correct? Can wait precede signal → GPU deadlock?
10. Drop ordering: textures before device? Encoders before command buffer?
11. Resource hazard tracking: any `untracked` resources? Barriers correct?
12. Compute dispatch: threadgroup count validated > 0? Matches buffer size?
13. GPU timeout handling: `MTLCommandBufferStatus::Error` caught?
14. Debug groups / capture manager: any left in release builds?

**Write to report, then continue to Task 5.**

---

## TASK 5: Compositor & Rendering Pipeline

**Goal:** Determine if the rendering pipeline can produce wrong output, crash, or leak.

**Files to read completely:**
- `crates/manifold-renderer/src/layer_compositor.rs` (1229 lines)
- `crates/manifold-renderer/src/compositor.rs`
- `crates/manifold-renderer/src/effect_chain.rs`
- `crates/manifold-renderer/src/effect_registry.rs`
- `crates/manifold-renderer/src/effect.rs`
- `crates/manifold-renderer/src/generator_renderer.rs`
- `crates/manifold-renderer/src/generator.rs`
- `crates/manifold-renderer/src/generator_context.rs`
- `crates/manifold-renderer/src/render_target_pool.rs`
- `crates/manifold-renderer/src/render_target.rs`
- `crates/manifold-renderer/src/uniform_arena.rs`
- `crates/manifold-renderer/src/blit.rs`
- `crates/manifold-renderer/src/gpu_encoder.rs`

**Answer these exact questions:**
1. Layer composite order: determined by what? HashMap iteration (non-deterministic!) or explicit sort?
2. Zero visible layers: what output? Black frame or index panic?
3. NaN propagation: generator outputs NaN → does compositor spread it to all layers?
4. Effect execution order: deterministic? Based on what data structure?
5. Mid-frame layer add/remove: snapshotted or live reference?
6. Blend mode edge cases: Inf, NaN, negative alpha inputs?
7. Opacity == 0.0: layer skipped? Check `< 0.0` vs `<= 0.0`.
8. Render target pool: max size? Stale format after resize?
9. Uniform arena: per-frame or recycled? Growth unbounded?
10. Effect added during playback: pipeline creation on hot path?
11. Effect removed during render: race with encoder?
12. Async compute: max in-flight command buffers? Approach Metal limit?
13. Per-owner effect cleanup: list EVERY stateful effect. All covered by cleanup path?

**Write to report, then continue to Task 6.**

---

## TASK 6: Effects & Generators — Feedback & State

**Goal:** Determine if stateful effects/generators can corrupt, leak, or produce NaN.

**Read the following files (focus on stateful effects with feedback):**
- `crates/manifold-renderer/src/effects/stylized_feedback.rs`
- `crates/manifold-renderer/src/effects/blob_tracking.rs` (820 lines)
- `crates/manifold-renderer/src/effects/wireframe_depth.rs` (1981 lines)
- `crates/manifold-renderer/src/effects/bloom.rs`
- `crates/manifold-renderer/src/effects/halation.rs`
- `crates/manifold-renderer/src/effects/depth_of_field.rs`
- `crates/manifold-renderer/src/effects/glitch.rs`
- `crates/manifold-renderer/src/generators/fluid_simulation.rs` (868 lines)
- `crates/manifold-renderer/src/generators/fluid_simulation_3d.rs` (724 lines)
- `crates/manifold-renderer/src/generators/mycelium.rs` (552 lines)
- `crates/manifold-renderer/src/generators/stateful_base.rs`
- `crates/manifold-renderer/src/generators/compute_common.rs`

**Answer per component:**
1. Does it have persistent state across frames (feedback textures, simulation buffers)? If yes:
   - Is there a NaN/Inf guard on feedback reads? One bad frame = permanent corruption.
   - Is the state stored per-owner (`AHashMap<i64, State>`)? Is there a cleanup path?
2. Compute dispatch: threadgroup count validated? Matches buffer/texture dimensions?
3. Uniform alignment: `#[repr(C)]`, 16-byte, pad fields, order matches WGSL?
4. Division in parameter calculations: any `1.0 / param` where param could be 0.0?
5. `as u32` from float params: `.round()` first? NaN guard?
6. Resolution scaling: half-res texture dimensions guaranteed non-zero?
7. Multi-pass: correct pass count? Intermediate texture lifecycle correct?
8. Iterative loops in simulation: bounded? Maximum iteration count?

**Write to report, then continue to Task 7.**

---

## TASK 7: Shader Safety (WGSL)

**Goal:** Determine if shaders can hang the GPU or produce permanent corruption.

**Read the highest-risk shaders:**
- `crates/manifold-renderer/src/generators/shaders/fluid_simulate.wgsl`
- `crates/manifold-renderer/src/generators/shaders/fluid_simulate_3d.wgsl`
- `crates/manifold-renderer/src/generators/shaders/fluid_scatter.wgsl`
- `crates/manifold-renderer/src/generators/shaders/mycelium_agent_update.wgsl`
- `crates/manifold-renderer/src/effects/shaders/fx_stylized_feedback.wgsl`
- `crates/manifold-renderer/src/effects/shaders/fx_stylized_feedback_compute.wgsl`
- `crates/manifold-renderer/src/effects/shaders/fx_blob_tracking_compute.wgsl`
- `crates/manifold-renderer/src/effects/shaders/fx_wireframe_depth_compute.wgsl`
- `crates/manifold-renderer/src/generators/shaders/compositor_blend.wgsl`

**Answer per shader:**
1. Every `loop` / `while`: guaranteed exit? Max iterations?
2. Every division: can divisor be zero from uniforms/parameters?
3. Every `pow`, `log`, `sqrt`, `atan2`: edge case inputs (0, negative)?
4. `textureLoad` / `textureStore`: bounds checked for out-of-range coordinates?
5. Boundary threads in compute: early-return for threads beyond texture dimensions?
6. Feedback texture reads: any NaN sanitization?
7. Workgroup size: total invocations ≤ 256?
8. `var<workgroup>` memory: total ≤ 32KB?

**Write to report, then continue to Task 8.**

---

## TASK 8: Unsafe & FFI Audit

**Goal:** Determine if unsafe code can cause memory corruption, segfaults, or UB.

**Files to read (all FFI-heavy files):**
- `crates/manifold-gpu/src/metal/mod.rs` (already read in Task 4 — reference your earlier notes)
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

**Also grep across entire codebase for:** `unsafe`, `extern "C"`, `transmute`, `from_raw`, `into_raw`, `Box::leak`, `mem::forget`

**Answer:**
1. Every `extern "C"` function: Rust-side signature matches C/ObjC side? Types correct?
2. Native plugins: C++ exceptions caught before FFI boundary? Null return checked?
3. Plugin output validation: data from BlobDetector/DepthEstimator validated before rendering use?
4. Raw pointer lifetimes: any pointer that could outlive its data? (completion handlers, MIDI callbacks)
5. `transmute` usage: same size and alignment?
6. Double-free risk: `Box::from_raw` called on same pointer twice anywhere?
7. Mutable aliasing: two `*mut T` to same memory, both written?
8. `autoreleasepool` coverage: every thread with ObjC calls wrapped?
9. IOSurface: retain/release balanced across threads?
10. Drop ordering: textures/buffers before device in struct field order?
11. NSException risk: any ObjC call that could throw without catch?

**Write to report, then continue to Task 9.**

---

## TASK 9: Numeric Cast Audit (Hot Paths Only)

**Goal:** Find numeric operations that could corrupt or crash under extreme values.

**Grep-based across hot-path crates:** `manifold-playback`, `manifold-renderer`, `manifold-gpu`

**Step 1:** Grep for `as u32` in these crates. For EACH result, read the surrounding 5 lines and classify:
- SAFE: input bounded by prior clamp/check
- DANGEROUS: input could be NaN/Inf/negative/out-of-range
Report all DANGEROUS with file:line and what the input could be.

**Step 2:** Same for `as i32`.

**Step 3:** Grep for `as f32` — specifically look for beat positions or time values being downcast from f64. These lose precision after ~16M (f32 mantissa = 23 bits). At 120 BPM that's ~2.2 hours.

**Step 4:** Grep for `Duration::from_secs_f64` and `Duration::from_secs_f32` — input NaN or negative = panic.

**Step 5:** Grep for `instant_a - instant_b` patterns (or `.duration_since`) — if b > a = panic.

**Write to report, then continue to Task 10.**

---

## TASK 10: Platform & Live Environment

**Goal:** Determine if the app is hardened for a live show environment.

**Files to read:**
- `crates/manifold-app/src/main.rs`
- `crates/manifold-app/src/app.rs` (1981 lines)
- `crates/manifold-app/src/app_lifecycle.rs`
- `crates/manifold-app/src/app_render.rs`
- `crates/manifold-app/src/window_registry.rs`
- `crates/manifold-renderer/src/surface.rs`
- `crates/manifold-renderer/src/ui_renderer.rs`

**Also grep for:** `panic::set_hook`, `IOPMAssertion`, `beginActivity`, `NSProcessInfo`, `setQualityOfService`, `SIGPIPE`, `signal`, `CAMetalLayer`, `nextDrawable`

**Answer:**
1. `panic::set_hook`: exists? Without it + `panic=abort`, crashes produce zero diagnostics.
2. Power assertion (`IOPMAssertion`): exists? Without it, macOS dims/sleeps during the show.
3. App Nap: disabled? (`beginActivity` or `NSProcessInfo` API)
4. wgpu surface lost/outdated: caught and recovered?
5. wgpu device lost: error handler exists? Recovery?
6. Display scale change: handled without crash?
7. Fullscreen transitions: stable?
8. `CAMetalLayer` drawable exhaustion: can `nextDrawable` block UI thread?
9. `SIGPIPE` handling: ignored? (default kills the process)
10. Thread QoS for content thread: what level? Can macOS demote it?
11. Multiple instances: any protection?
12. Core dump availability: with stripped symbols + panic=abort, any diagnostics possible?

**Write to report, then continue to Task 11.**

---

## TASK 11: Media Decode Safety

**Goal:** Determine if video decode can crash, leak, or stall.

**Files to read:**
- `crates/manifold-media/src/decode_scheduler.rs`
- `crates/manifold-media/src/video_renderer.rs`
- `crates/manifold-media/src/decoder.rs`
- `crates/manifold-media/src/decoder_ffi.rs`
- `crates/manifold-media/src/metal_ffi.rs`
- `crates/manifold-media/src/metal_encoder.rs`

**Answer:**
1. Decode backpressure: slower than playback → unbounded queue? Frame drop? Strategy?
2. Decode error: corrupted frame, codec error → crash or fallback?
3. Seek accuracy: keyframe-only or decode-to-target for loops?
4. Per-frame allocation or buffer reuse?
5. FFI null checks on all C function returns?
6. Multiple simultaneous clips: thread safety?
7. File handle lifetime: open for entire session?
8. Memory-mapped files: SIGBUS risk if file deleted?

**Write to report, then continue to Task 12.**

---

## TASK 12: Data Integrity & Serialization

**Goal:** Determine if project save/load can corrupt data or crash.

**Files to read:**
- `crates/manifold-io/src/saver.rs`
- `crates/manifold-io/src/loader.rs`
- `crates/manifold-io/src/archive.rs`
- `crates/manifold-io/src/migrate.rs`
- `crates/manifold-io/src/path_resolver.rs`
- `crates/manifold-core/src/types.rs`
- `crates/manifold-core/src/id.rs`

**Answer:**
1. Save atomicity: temp file + rename, or in-place write? (crash during save = data loss?)
2. `fsync` after write?
3. Autosave during playback: state snapshotted or live read? (race condition?)
4. Untrusted project files: deeply nested JSON → stack overflow? Huge strings → OOM?
5. serde attributes consistent? `rename_all = "camelCase"` everywhere?
6. `#[serde(default)]` on fields added after V1?
7. Empty string typed IDs: guarded?
8. Unicode normalization in file paths (macOS NFD)?
9. Disk full during save: handled?

**Write to report, then continue to Task 13.**

---

## TASK 13: Memory Growth & Long-Running Accumulation

**Goal:** Determine if anything grows unboundedly over a multi-hour session.

**Grep-based across entire codebase:**

**Step 1: AHashMap growth.** Grep for `AHashMap` (102 usages). For each:
- Is there a corresponding `remove` or `clear` path?
- Could entries accumulate without cleanup? (e.g., keyed by clip ID where clips are created/destroyed over time)
List any map that only has `insert` without corresponding `remove`.

**Step 2: Vec growth.** Grep for `Vec::new()` and `.push(` in hot-path crates. Are any Vecs appended to per-frame without clearing?

**Step 3: Channel backlog.** For each unbounded channel: what prevents the sender from outpacing the receiver over hours?

**Step 4: Texture pool size.** Read `render_target_pool.rs`. Is there a maximum pool size? What happens if many different resolutions/formats are used over a session — does the pool grow with each unique configuration?

**Step 5: String allocations.** Grep for `format!`, `.to_string()`, `String::from` in per-frame code paths. Do any allocate strings every frame?

**Step 6: Log output accumulation.** The 46 println/eprintln calls — if any are per-frame, they fill stdout buffer and eventually disk.

**Write to report.**

---

## FINAL STEP

After all 13 tasks, prepend a summary to the TOP of the report:

```markdown
# MANIFOLD — Live Performance Stability Audit Report

**Date:** [today]
**Scope:** 13 stability audits, ~99K lines, focus on infinite-runtime safety

## Executive Summary
- Total findings: N
- CRITICAL: N (will crash or corrupt during a show)
- WARNING: N (degrades over hours)
- INFO: N
- VERIFIED SAFE: N

## Top 10 Most Dangerous Findings
1. [file:line] — description — impact
2. ...

## Confirmed Safe (no issues found)
- [list categories that passed with clean bill of health]
```