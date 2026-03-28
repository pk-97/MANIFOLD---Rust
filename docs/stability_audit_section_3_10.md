# Stability Audit: Section 3 (Content Thread & Frame Pipeline) + Section 10 (Platform & Live Environment)

Audited: 2026-03-28
Auditor: Claude Opus 4.6 (1M context)
Scope: 14 files across manifold-app, manifold-renderer, manifold-playback

---

## SECTION 3: Content Thread & Frame Pipeline

### 3.1 Frame Pacing Mechanism

**VERIFIED SAFE** — Hybrid sleep + spin-wait with macOS-aware tuning.

The content thread uses a two-phase approach:
- Phase 1: `thread::sleep(remaining - 3ms)` when >4ms remain until deadline
- Phase 2: Spin-wait via `thread::yield_now()` when 100us-4ms remain; tight poll when <100us

`crates/manifold-app/src/content_thread.rs:200-213`

```
let remaining = self.timer.time_until_next_tick();
if remaining > std::time::Duration::from_millis(4) {
    std::thread::sleep(remaining - std::time::Duration::from_millis(3));
} else if remaining > std::time::Duration::from_micros(100) {
    std::thread::yield_now();
}
```

The 3ms margin accounts for macOS `thread::sleep` overshoot (documented as 2-4ms under load). This is a well-tuned approach. The `FrameTimer` itself uses `std::time::Instant` (backed by `mach_absolute_time` on macOS) for sub-microsecond accuracy.

`crates/manifold-app/src/frame_timer.rs:41-43` — `should_tick()` compares elapsed against target frame duration.

**Rating: VERIFIED SAFE** — Good frame pacing strategy; the spin margin handles macOS sleep jitter.

---

### 3.2 Autoreleasepool Wrapping

**VERIFIED SAFE** — Each frame tick is wrapped in an autoreleasepool on macOS.

`crates/manifold-app/src/content_thread.rs:217-221`

```rust
#[cfg(target_os = "macos")]
objc::rc::autoreleasepool(|| {
    self.tick_frame(&state_tx);
});
```

The export loop also wraps each frame iteration:

`crates/manifold-app/src/content_export.rs:207`

```rust
let frame_err: Option<String> = objc::rc::autoreleasepool(|| {
```

**Rating: VERIFIED SAFE** — Both normal and export paths drain autoreleased Metal objects per frame.

---

### 3.3 UI to Content Channels: Bounded, Capacity, Backpressure

`crates/manifold-app/src/app.rs:1065-1066`

```rust
let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<ContentCommand>(64);
let (state_tx, state_rx) = crossbeam_channel::bounded::<ContentState>(4);
```

**Command channel (UI -> Content): bounded(64)**

Backpressure strategy: `try_send()` with log on failure (no panic, no block):

`crates/manifold-app/src/content_command.rs:171-175`
```rust
pub fn send(tx: &crossbeam_channel::Sender<ContentCommand>, cmd: ContentCommand) {
    if let Err(e) = tx.try_send(cmd) {
        log::error!("[UI] Content command dropped: {e}");
    }
}
```

**State channel (Content -> UI): bounded(4)**

Backpressure: `try_send` with silent drop:

`crates/manifold-app/src/content_thread.rs:707`
```rust
let _ = state_tx.try_send(state);
```

**WARNING** — `crates/manifold-app/src/content_command.rs:171-175`: When the command channel is full (64 items), commands are silently dropped with only a log message. During a burst of rapid UI interactions (e.g., rapid MIDI note input, fast scrolling with edits), critical commands like `Execute(Command)` could be lost. The undo stack would become inconsistent with the actual project state. This is unlikely at 64 capacity but is architecturally unsound for editing commands that must not be lost.

**INFO** — `crates/manifold-app/src/content_thread.rs:707`: State channel drops are benign — the UI always uses the latest state, and stale states are intentionally discarded.

**WARNING** — `crates/manifold-app/src/app_lifecycle.rs:277`: The `import_video_files()` method uses `content_tx.send(cmd)` (blocking send, not `try_send`) from a background thread. If the command channel fills to 64, this background thread blocks until the content thread drains. Since the background thread is fire-and-forget, this is acceptable, but the inconsistency in send strategy is worth noting.

---

### 3.4 Content Thread Overrun (Frame > 16.6ms)

**VERIFIED SAFE** — Skip-to-current behavior, no queue-up.

When a frame takes longer than the target interval, the `FrameTimer::should_tick()` method returns true immediately on the next loop iteration (since `elapsed >= target`), and `consume_tick()` returns the actual elapsed dt. The engine uses this actual dt for time advancement.

`crates/manifold-app/src/frame_timer.rs:52-66` — `consume_tick()` records actual wall-clock dt and computes `missed_ticks` for diagnostics, but does NOT attempt catch-up. There is no "queue multiple ticks" or "process N frames to catch up" logic.

The content thread renders one frame per loop iteration regardless of how long the previous frame took. If a frame takes 50ms, the next frame simply starts at the current wall-clock time with a larger dt.

**Rating: VERIFIED SAFE** — Natural skip-to-current. No GPU flood after stalls.

---

### 3.5 UI Thread Blocked (Window Drag, System Dialog)

**VERIFIED SAFE** — Content thread is fully independent.

The content thread has its own loop (`ContentThread::run()` at `content_thread.rs:106`). It reads commands via `try_recv()` (non-blocking) and manages its own frame pacing independently. When the UI thread is blocked by macOS window drag or a system dialog:

1. Commands from UI stop arriving (no new `try_recv` data) — content thread continues with its current state
2. State pushes from content thread are `try_send` — they simply fail silently when the UI's bounded(4) channel fills up
3. No lock is shared between the two threads on the frame-critical path

**Additionally**: File dialogs explicitly send `PauseRendering` before opening and `ResumeRendering` after:

`crates/manifold-app/src/app_lifecycle.rs:44,53`
```rust
self.send_content_cmd(ContentCommand::PauseRendering);
// ... dialog ...
self.send_content_cmd(ContentCommand::ResumeRendering);
```

When paused, the content thread sleeps 16ms and only drains commands:

`crates/manifold-app/src/content_thread.rs:195-197`
```rust
if self.rendering_paused {
    std::thread::sleep(std::time::Duration::from_millis(16));
    continue;
}
```

**Rating: VERIFIED SAFE** — Content thread never blocks on the UI thread. Intentional pause during dialogs avoids GPU contention.

---

### 3.6 Lock Ordering

**VERIFIED SAFE** — Minimal cross-thread locking with no conflicting order.

Locks identified across these files:

1. **SharedOutputView** (`content_pipeline.rs:20-55`):
   - `view: RwLock<Option<wgpu::TextureView>>` — Content writes, UI reads. Only used on non-macOS fallback path (dead_code on macOS).
   - `dimensions: RwLock<(u32, u32)>` — Content writes, UI reads. Brief lock, no nesting.

2. **SharedTextureBridge** (`shared_texture.rs:41-53`):
   - `io_surfaces: RwLock<[IOSurface; 3]>` — Read-locked during `import_texture` (init/resize only, not per-frame). Write-locked only during `resize()`.
   - `front_index: AtomicU32` — Lock-free.
   - `width/height: AtomicU32` — Lock-free.
   - `generation: AtomicU64` — Lock-free.

3. **No parking_lot locks in content_thread.rs or content_pipeline.rs** — The content thread owns all mutable state exclusively. No Mutex/RwLock is acquired on the per-frame hot path.

Lock ordering analysis: The only RwLock that both threads touch (`io_surfaces`) is only acquired at init and resize (not per-frame). The `dimensions` RwLock is trivially safe (single lock, no nesting). No conflicting acquisition order exists.

**Rating: VERIFIED SAFE** — Effectively lock-free on the hot path via IOSurface atomics and single-owner content thread.

---

### 3.7 cleanup_stopped_clips() Coverage

`crates/manifold-app/src/content_thread.rs:368-370`
```rust
if !tick_result.stopped_clips.is_empty() {
    self.content_pipeline.cleanup_stopped_clips(&tick_result.stopped_clips);
}
```

The cleanup chain:
1. `ContentPipeline::cleanup_stopped_clips` -> `Compositor::cleanup_clip_owner(clip_id)` (`content_pipeline.rs:600-603`)
2. `LayerCompositor::cleanup_clip_owner` -> `EffectRegistry::cleanup_clip_owner(owner_key)` (`layer_compositor.rs:1214-1215`)
3. `EffectRegistry::cleanup_clip_owner` iterates ALL registered processors and calls `cleanup_owner_state(owner_key)` (`effect_registry.rs:131-134`)

This covers all stateful effects registered in the EffectRegistry (Feedback, Bloom, PixelSort, FluidDistortion, etc.).

**Generator renderer cleanup**: Handled separately via `ClipRenderer::stop_clip()` in the engine tick:

`crates/manifold-renderer/src/generator_renderer.rs:573-581`
```rust
fn stop_clip(&mut self, clip_id: &str) {
    if let Some(active) = self.active_clips.remove(clip_id) {
        // Return full-res RTs to pool
        if let Some(upscale_rt) = active.upscale_target {
            self.available_rts.push(upscale_rt);
        } else {
            self.available_rts.push(active.render_target);
        }
    }
}
```

**WARNING** — `crates/manifold-renderer/src/generator_renderer.rs:62`: Per-layer `LayerGeneratorState` (which holds `Box<dyn Generator>` with particle state, attractor positions, etc.) is NOT cleaned up when clips stop. The `layer_generators` HashMap grows monotonically during a session. While generator state is keyed by `LayerId` (bounded by layer count), the internal state of generators (particle buffers, feedback textures) persists even when no clips are playing on that layer. This is intentional for temporal continuity but means GPU memory for generator state textures is held indefinitely. Over a 4-hour show with many layer/generator type changes, this could accumulate.

**INFO** — `crates/manifold-playback/src/engine.rs:296-318`: `PlaybackEngine::initialize()` does NOT call `release_all()` on renderers. Active clips from the previous project persist until `sync_clips_to_time` naturally stops them on the next tick. The engine resets time to 0.0, so all clips should stop naturally. However, there is a brief window where stale clip state coexists with the new project.

---

### 3.8 Project Switch During Playback

`crates/manifold-app/src/content_commands.rs:169-201` — `LoadProject` handler:

State torn down:
- Audio sync: `audio_sync.reset_audio()` (line 171)
- Stem audio: `stem_audio.reset_stems()` (line 174)
- Link beat offset: reset to NaN (line 178)
- Tempo recorder: `tempo_recorder.reset()` (line 179)
- Engine: `engine.initialize(project)` — resets time, beat, sync state (line 180)
- Content pipeline: `resize()` to new project dimensions (line 185)
- Frame timer: synced to new project FPS (line 189)
- MIDI config: updated from new project (line 194)
- OSC routes: rebuilt (line 199)

**WARNING** — `crates/manifold-app/src/content_commands.rs:169-201`: The `LoadProject` handler does NOT explicitly call `clear_all_effect_state()` on the compositor or `release_all()` on renderers. Effect state from the old project (Feedback textures, PixelSort state) persists until new clips trigger `cleanup_stopped_clips()`. If the old project had 20 active effects and the new project starts with none, those GPU resources remain allocated until the old clip IDs are naturally stopped. The `engine.initialize()` resets time to 0, which will cause `sync_clips_to_time` to stop all old clips on the next tick, but this is implicit rather than explicit.

**INFO** — `crates/manifold-app/src/content_commands.rs:180`: `engine.initialize()` calls `renderer.on_project_loaded()` on all renderers. The `GeneratorRenderer` has no `on_project_loaded` override (uses default no-op from trait). The `VideoRenderer` does implement it to update its video library cache. Generator `active_clips` and `layer_generators` are NOT cleared on project load.

---

### 3.9 Thread QoS / Priority

**VERIFIED SAFE** — Content thread uses real-time scheduling (SCHED_RR, priority 47).

`crates/manifold-app/src/content_thread.rs:117-131`

```rust
let pthread = unsafe { libc::pthread_self() };
let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
param.sched_priority = 47;
let ret = unsafe { libc::pthread_setschedparam(pthread, libc::SCHED_RR, &param) };
```

Priority 47 is near the top of macOS real-time scheduling (max 48 for audio). This prevents macOS from demoting the content thread or applying App Nap-style throttling.

**WARNING** — `crates/manifold-app/src/content_thread.rs:121-127`: `pthread_setschedparam(SCHED_RR, 47)` requires elevated privileges (or the process must have the `com.apple.security.cs.allow-jit` or similar entitlement). On standard user accounts, this returns EPERM (error code 1). The code logs a warning and continues with default priority, which means the content thread runs at normal QoS — vulnerable to macOS throttling during background/App Nap scenarios. The app MUST be run with the appropriate entitlements or as root for guaranteed real-time scheduling.

---

### 3.10 channel.send() When Receiver Dropped

**VERIFIED SAFE** — All channel sends are caught.

Content thread state push: `let _ = state_tx.try_send(state);` — `crates/manifold-app/src/content_thread.rs:707`. The `let _` discards the error (including `Disconnected`). No panic.

UI command send: `ContentCommand::send()` uses `try_send()` with `log::error!` on failure — `crates/manifold-app/src/content_command.rs:172`. No panic on disconnected.

Content thread command receive: `try_recv()` at `content_thread.rs:176-191` — handles `TryRecvError::Disconnected` explicitly by returning (clean shutdown).

Application shutdown: `Drop` impl sends `Shutdown` via `tx.send()` (blocking) — `crates/manifold-app/src/app.rs:1948-1949`. Uses `let _` to discard error. Safe.

**Rating: VERIFIED SAFE** — No panic paths on channel disconnection.

---

### 3.11 Every thread::sleep in Frame-Critical Files

1. **`content_thread.rs:196`** — `thread::sleep(16ms)` when `rendering_paused`. NOT on frame-critical path (rendering is paused, only draining commands). **VERIFIED SAFE**.

2. **`content_thread.rs:207`** — `thread::sleep(remaining - 3ms)` for frame pacing. ON the frame-critical path but intentional — this is the primary frame pacing mechanism. **VERIFIED SAFE**.

No other `thread::sleep` calls found in the audited files.

---

### 3.12 Content Thread Shutdown: GPU Resources

`crates/manifold-app/src/app.rs:1271-1281` — `CloseRequested` handler:

```rust
if let Some(tx) = self.content_tx.take() {
    let _ = tx.send(ContentCommand::Shutdown);
}
if let Some(handle) = self.content_thread_handle.take() {
    let _ = handle.join();
}
```

The `Shutdown` command causes `ContentThread::run()` to return at `content_thread.rs:183`. The `ContentThread` struct is then dropped, which drops:
- `ContentPipeline` (drops compositor, GPU textures, texture pool, native device, native event)
- `PlaybackEngine` (drops renderers, which drop their GPU resources)
- All other content-side state

`crates/manifold-app/src/app.rs:1943-1953` — `Drop` impl on Application provides a safety net, sending `Shutdown` even on abnormal exit.

**INFO** — `crates/manifold-app/src/content_pipeline.rs:504-507`: The last frame's command buffer is committed (`native_enc.commit()`) and signal/event tracking is updated, but there is no explicit `waitUntilCompleted` on the final frame before shutdown. Metal will complete in-flight command buffers, but the textures they reference are about to be dropped. In practice this is fine because process exit releases all GPU resources, but for clean shutdown it could produce Metal validation warnings.

---

### 3.13 println!/eprintln! on Per-Frame Paths

**VERIFIED SAFE** — No `println!` or `eprintln!` found in any of the audited files:
- `content_thread.rs` — none
- `content_pipeline.rs` — none
- `content_commands.rs` — none
- `content_command.rs` — none
- `content_state.rs` — none
- `shared_texture.rs` — none
- `frame_timer.rs` — none

All logging uses the `log` crate (`log::info!`, `log::warn!`, `log::debug!`, etc.), which can be disabled at compile time.

**INFO** — `crates/manifold-app/src/frame_timer.rs:103`: `log::debug!("FPS: {:.1}", self.current_fps)` fires every 1 second. At debug level, this is compiled out in release builds. Safe.

---

### 3.14 Stall Recovery (After 100ms Stall)

**VERIFIED SAFE** — Skip-to-current (no GPU flood).

As analyzed in 3.4, after a stall the content thread simply processes the next frame with the actual elapsed dt. The `FrameTimer` tracks `missed_ticks` for diagnostics:

`crates/manifold-app/src/frame_timer.rs:59-64`
```rust
self.missed_ticks = if target_secs > 0.0 {
    ((dt / target_secs).floor() as u64).saturating_sub(1)
} else {
    0
};
```

This is reporting-only. The engine tick uses the real dt, advancing the playhead proportionally. No catch-up frames are queued. After a 100ms stall at 60fps, the engine advances ~6 frames of time in one tick. Generators and effects see a larger dt but process a single GPU frame. This is correct for a VJ application — skip ahead rather than queue up.

**Rating: VERIFIED SAFE** — Clean skip-to-current behavior.

---

## SECTION 10: Platform & Live Environment

### 10.1 panic::set_hook

**CRITICAL** — `crates/manifold-app/src/main.rs:29-38`: No `panic::set_hook` is installed.

```rust
fn main() {
    env_logger::init();
    log::info!("MANIFOLD starting...");
    let event_loop = winit::event_loop::EventLoop::new().unwrap();
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
    let mut application = app::Application::new();
    event_loop.run_app(&mut application).unwrap();
}
```

Combined with `panic = "abort"` in release profile (`Cargo.toml:62`), a panic produces zero diagnostics — the process terminates immediately with no stack trace, no log message, no crash file. During a 4-hour live show, if any panic occurs, the operator gets an instant black screen with no way to diagnose what happened.

This is the single most critical missing piece for live show hardening. A custom panic hook should:
1. Log the panic message and backtrace to a file before abort
2. Optionally display a visible error (flash the screen, write to stderr)

---

### 10.2 Power Assertion (IOPMAssertion)

**CRITICAL** — No IOPMAssertion or equivalent found anywhere in the codebase.

Searched for: `IOPMAssertion`, `beginActivity`, `NSProcessInfo`, `setQualityOfService` — zero hits in source code (only in doc/plan files).

Without an IOPMAssertion, macOS WILL dim the display and eventually sleep during a live show if the user isn't touching the keyboard/mouse. A 4-hour generative visual performance (no mouse input needed) would be interrupted by the system sleep timer.

Required: `IOPMAssertionCreateWithName(kIOPMAssertionTypePreventUserIdleDisplaySleep)` at startup.

---

### 10.3 App Nap

**WARNING** — No explicit App Nap disabling found.

macOS App Nap can throttle applications that are not the frontmost app or whose windows are fully occluded. During a live show, the operator may switch to another app (Ableton, mixer) while the visual output continues on a second display. Without `NSProcessInfo.beginActivity(.userInitiated)`, macOS may throttle the content thread's timers and reduce its CPU priority.

The content thread's `SCHED_RR` priority at level 47 (`content_thread.rs:121`) may partially mitigate this IF the real-time priority was successfully set. However, if the `pthread_setschedparam` call failed (EPERM on unprivileged accounts, see 3.9), App Nap applies fully.

---

### 10.4 wgpu Surface Lost/Outdated

**VERIFIED SAFE** — Both `Lost` and `Outdated` are caught and recovered.

`crates/manifold-app/src/app_render.rs:853-867`

```rust
let surface_texture = match ws.surface.get_current_texture() {
    Ok(t) => t,
    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
        ws.surface.resize(
            &gpu.device,
            ws.surface.width,
            ws.surface.height,
            ws.surface.scale_factor,
        );
        continue;
    }
    Err(e) => {
        log::error!("Surface error: {e}");
        continue;
    }
};
```

On `Lost` or `Outdated`, the surface is reconfigured and the frame is skipped (continue to next window). Other errors are logged and the frame is skipped. No panic path.

**Rating: VERIFIED SAFE** — Graceful recovery from all surface errors.

---

### 10.5 wgpu Device Lost

**WARNING** — `crates/manifold-renderer/src/gpu.rs` (referenced from `app.rs`): No `device.on_uncaptured_error()` handler or device lost callback was found in the codebase.

wgpu's Metal backend can trigger device loss on GPU hang/timeout (rare but possible with complex compute shaders or driver bugs). Without a handler, the default behavior is to log to stderr and potentially panic, which combined with `panic = "abort"` would be an instant, undiagnosed crash during a show.

---

### 10.6 Display Scale Change

**VERIFIED SAFE** — Handled without crash.

`crates/manifold-app/src/app.rs:1305-1318`

```rust
WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
    if let Some(gpu) = &self.gpu
        && let Some(ws) = self.window_registry.get_mut(&window_id) {
            let size = ws.window.inner_size();
            ws.surface.resize(&gpu.device, size.width, size.height, scale_factor);
            if is_primary {
                let logical_w = size.width as f32 / scale_factor as f32;
                let logical_h = size.height as f32 / scale_factor as f32;
                self.ui_root.resize(logical_w, logical_h);
            }
        }
}
```

Surface is reconfigured, UI is rebuilt at new logical dimensions. No panic path.

**Rating: VERIFIED SAFE** — Clean handling.

---

### 10.7 Fullscreen Transitions

**INFO** — `crates/manifold-app/src/app.rs`: No explicit fullscreen handling code found (no `Fullscreen` enum usage). Output windows are opened as regular decorated windows. The user can use macOS's native fullscreen (green button) which triggers `Resized` events. These are handled correctly (see 10.6).

No fullscreen-specific logic means the app relies on macOS's window server for fullscreen transitions. This is generally stable but:
- Surface `Lost`/`Outdated` events during transition are handled (10.4)
- No protection against the "dead zone" during macOS fullscreen animation where the app may not receive redraw events

**Rating: VERIFIED SAFE** — Relies on macOS native fullscreen; surface errors are caught.

---

### 10.8 CAMetalLayer Drawable Exhaustion

**INFO** — `crates/manifold-renderer/src/surface.rs:45`: `desired_maximum_frame_latency: 2`. This tells Metal to keep 2 drawables in flight.

The UI thread calls `get_current_texture()` per window per frame. If all drawables are in flight (GPU behind), `nextDrawable` blocks until one becomes available. With the content thread on a separate Metal command queue, this is unlikely but possible if the GPU is fully saturated.

Since the UI thread uses `ControlFlow::Poll` (`main.rs:34`) and `about_to_wait` calls `tick_and_render` followed by `request_redraw`, a blocked `get_current_texture` would stall the entire UI thread until a drawable is available (typically < 16ms). This is acceptable — it's natural GPU backpressure.

**Rating: VERIFIED SAFE** — Natural vsync backpressure; content thread unaffected.

---

### 10.9 SIGPIPE Handling

**WARNING** — `crates/manifold-app/src/main.rs`: No SIGPIPE handling found. Searched for `SIGPIPE` and `signal` — no results in app source files.

The default behavior on macOS is to terminate the process on SIGPIPE. If the ArtNet/LED output (`manifold-led`) sends UDP packets and the socket encounters an error, or if any piped output (stdout/stderr redirected to a dead pipe) triggers SIGPIPE, the process dies silently.

For a live performance app, `signal(SIGPIPE, SIG_IGN)` should be set at startup.

---

### 10.10 Thread QoS for Content Thread

Covered in detail in 3.9.

Summary: `SCHED_RR` at priority 47 is requested. If the call succeeds, macOS cannot demote the thread. If it fails (likely on unprivileged accounts), the thread runs at default QoS and IS subject to demotion.

**WARNING** — No QoS escalation fallback. If `SCHED_RR` fails, consider falling back to `pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE)` which doesn't require privileges but still prevents significant throttling.

---

### 10.11 Multiple Instances

**WARNING** — `crates/manifold-app/src/main.rs`: No single-instance protection. No file lock, no named pipe, no socket check.

Two instances of MANIFOLD can run simultaneously, both fighting for:
- The same Metal GPU queues (GPU contention → frame drops)
- The same MIDI devices (one will fail to open)
- The same ArtNet ports (UDP bind conflict)
- The same pipeline cache file (`~/Library/Caches/com.latentspace.manifold/pipeline_cache.metallib`)

During a live show, accidentally launching a second instance could degrade performance catastrophically.

---

### 10.12 Core Dump / Diagnostics Availability

**CRITICAL** — With `strip = "symbols"` + `panic = "abort"` (both in `Cargo.toml:61-62`), and no `panic::set_hook` (10.1), a crash produces:

1. No stack trace (symbols stripped)
2. No panic message logged (abort before any handler)
3. No core dump (macOS doesn't generate core dumps by default; `panic = "abort"` calls `abort()` which may or may not produce one depending on `kern.corefile` settings)
4. No crash report (Apple's crash reporter captures the signal but symbol names are gone)

This means a mid-show crash is completely opaque. The only diagnostic available is the macOS Console.app crash report with hex addresses (no symbol names).

**Combined with 10.1**: This is the most serious live-show risk. A panic hook that writes the message + `std::backtrace::Backtrace::force_capture()` to a log file before aborting would make crashes diagnosable.

---

## SUMMARY TABLE

| # | Finding | Severity | File:Line |
|---|---------|----------|-----------|
| 3.1 | Frame pacing: hybrid sleep+spin, well-tuned | VERIFIED SAFE | content_thread.rs:200-213 |
| 3.2 | Autoreleasepool wraps every frame tick | VERIFIED SAFE | content_thread.rs:217-221 |
| 3.3a | Command channel bounded(64), try_send drops on full | WARNING | content_command.rs:171-175 |
| 3.3b | State channel bounded(4), try_send drops silently | VERIFIED SAFE | content_thread.rs:707 |
| 3.3c | import_video_files uses blocking send from bg thread | INFO | app_lifecycle.rs:277 |
| 3.4 | Frame overrun: skip-to-current, no catch-up | VERIFIED SAFE | frame_timer.rs:52-66 |
| 3.5 | UI block: content thread fully independent | VERIFIED SAFE | content_thread.rs:106-223 |
| 3.6 | Lock ordering: effectively lock-free on hot path | VERIFIED SAFE | shared_texture.rs:41-53 |
| 3.7a | Effect cleanup covers all EffectRegistry processors | VERIFIED SAFE | effect_registry.rs:131-134 |
| 3.7b | Generator layer_generators not cleaned on clip stop | WARNING | generator_renderer.rs:62 |
| 3.8a | Project switch resets audio/sync/timer | VERIFIED SAFE | content_commands.rs:169-201 |
| 3.8b | No explicit clear_all_effect_state on project load | WARNING | content_commands.rs:180 |
| 3.8c | GeneratorRenderer active_clips not cleared on project load | INFO | engine.rs:296-318 |
| 3.9 | SCHED_RR priority 47, may fail without privileges | WARNING | content_thread.rs:121-127 |
| 3.10 | All channel sends handle disconnection safely | VERIFIED SAFE | content_command.rs:172, content_thread.rs:707 |
| 3.11 | thread::sleep only in pacing and pause paths | VERIFIED SAFE | content_thread.rs:196,207 |
| 3.12 | Shutdown joins content thread, GPU resources dropped | VERIFIED SAFE | app.rs:1271-1281, 1943-1953 |
| 3.13 | No println!/eprintln! on per-frame paths | VERIFIED SAFE | all audited files |
| 3.14 | Stall recovery: skip-to-current, single-frame advance | VERIFIED SAFE | frame_timer.rs:59-64 |
| 10.1 | No panic::set_hook — zero crash diagnostics | CRITICAL | main.rs:29-38 |
| 10.2 | No IOPMAssertion — macOS will sleep during show | CRITICAL | (missing from codebase) |
| 10.3 | No App Nap disabling — throttled when not frontmost | WARNING | (missing from codebase) |
| 10.4 | Surface Lost/Outdated caught and recovered | VERIFIED SAFE | app_render.rs:853-867 |
| 10.5 | No wgpu device lost handler | WARNING | (missing from codebase) |
| 10.6 | Display scale change handled cleanly | VERIFIED SAFE | app.rs:1305-1318 |
| 10.7 | Fullscreen relies on macOS native, surface errors caught | VERIFIED SAFE | app.rs, app_render.rs |
| 10.8 | CAMetalLayer drawable: natural backpressure | VERIFIED SAFE | surface.rs:45 |
| 10.9 | No SIGPIPE handling — default kills process | WARNING | main.rs (missing) |
| 10.10 | QoS: SCHED_RR may fail, no fallback | WARNING | content_thread.rs:121-127 |
| 10.11 | No multiple-instance protection | WARNING | main.rs (missing) |
| 10.12 | Stripped symbols + panic=abort = zero diagnostics | CRITICAL | Cargo.toml:61-62 |

### Severity Counts
- **CRITICAL**: 3 (panic hook, power assertion, crash diagnostics)
- **WARNING**: 8
- **INFO**: 3
- **VERIFIED SAFE**: 14
