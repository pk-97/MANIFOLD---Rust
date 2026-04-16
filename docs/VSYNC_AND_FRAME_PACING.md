# Frame Pacing Architecture

## Current Design (2026-04-16)

Direct present with timer-based content thread pacing. No CVDisplayLink on the content thread, no IOSurface intermediary for output. The same model used by TouchDesigner (via MoltenVK), Resolume, and every game engine.

```
Content Thread (timer-paced):
  mach_wait_until + 2ms spin → render → nextDrawable → present → commit
  THREAD_TIME_CONSTRAINT_POLICY for real-time kernel scheduling

Output CAMetalLayer:
  displaySyncEnabled = true, 3 drawables
  Metal queues presents to vsync — the ONLY vsync gate

UI Thread:
  UiDisplayLink (CVDisplayLink) fires at MacBook display cadence
  Lightweight: sets AtomicBool flag, wakes CFRunLoop
```

## Content Thread Pacing

**File:** `manifold-app/src/frame_timer.rs`, `manifold-app/src/content_thread.rs`

The content thread runs at `project.settings.frame_rate` using a hybrid timer:

1. **`mach_wait_until(deadline)`** — kernel sleep for the bulk of the frame interval. Zero CPU. Precision limited to ~1ms by macOS software timer resolution.
2. **Spin-wait for final 2ms** — checks `mach_absolute_time()` in a tight loop until the exact deadline. The nanosecond-resolution clock provides sub-microsecond precision.

This is the standard pattern for real-time video on macOS. CoreAudio uses hardware interrupts instead (DMA), but no hardware interrupt is available for arbitrary frame deadlines. The 2ms spin burns ~12% of one core at 60fps — acceptable for a real-time renderer.

### Real-Time Thread Scheduling

The content thread uses `THREAD_TIME_CONSTRAINT_POLICY` (the native macOS real-time API, same as CoreAudio):

```
period:      16.67ms at 60fps (one frame)
computation: 12.5ms  (75% budget for render work)
constraint:  16.67ms (must complete within one period)
preemptible: true
```

This ensures the thread gets immediate CPU time during the spin-wait. `SCHED_RR` (POSIX) was tried first but macOS doesn't honor it — the thread falls back to normal scheduling with 1-2ms of jitter.

### FPS Measurement

EWMA (exponentially weighted moving average) on frame time with tau=0.3s. Updates every frame. Responds to frame drops within ~5 frames while filtering single-frame jitter.

## Output Presentation

**File:** `manifold-app/src/content_pipeline.rs` (render path, lines ~730-773)

The content thread presents directly to the output window's CAMetalLayer:

1. `surface.next_drawable()` — acquire from the 3-drawable pool
2. Aspect-fit blit from compositor output to drawable (fullscreen triangle shader)
3. `encoder.present_drawable(&drawable)` — schedule present on command buffer completion
4. `encoder.commit()` — submit to GPU queue

`displaySyncEnabled = true` on the CAMetalLayer is the **single vsync gate**. Metal queues each present to the next vsync boundary. With 3 drawables, `nextDrawable()` almost never blocks — one drawable is being displayed, one is queued, one is available for rendering.

### Why Not displaySyncEnabled = false?

Tested and rejected. Without display sync, presents land at arbitrary times relative to WindowServer's compositor cycle. This creates phase mismatch — irregular frame display times (16ms, 33ms alternating) perceived as judder. WindowServer prevents tearing regardless, but the timing irregularity is visible.

### Suspend During Display Retarget

`SetOutputPresentSuspended(true)` skips `next_drawable()` during display transitions (fullscreen toggle, window move between monitors). Without this, `next_drawable()` can block for up to 1 second on a transitioning display, stalling the content thread.

## UI Thread VSync (UiDisplayLink)

**File:** `manifold-app/src/display_link.rs`

Lightweight CVDisplayLink (the only remaining display link): sets `AtomicBool` + calls `CFRunLoopWakeUp()`. The winit event loop checks `vsync_ready()` in `about_to_wait()` to decide when to render UI. Retargets when the primary window moves between displays.

## IOSurface Triple Buffer (Workspace Preview Only)

**File:** `manifold-app/src/shared_texture.rs`

The workspace preview (small inset in the editor) uses an IOSurface triple buffer for content → UI frame transport. The output window does NOT use this — it uses direct present.

- 3 IOSurface buffers (zero-copy kernel memory, Rgba16Float)
- Atomic `front_index` tracks which surface is safe to read
- GPU completion handler publishes `front_index` when content GPU work finishes
- UI thread reads latest `front_index` for preview display

## What We Tried and Why It Failed

### CVDisplayLink content thread pacing (removed April 2026)

Phase-locked content thread wake to vsync boundaries via `GpuVsyncSignal` condvar. Created **double vsync gating**: CVDisplayLink wakes the thread at vsync, then `displaySyncEnabled` gates the present at the next vsync. Under heavy scenes where render time approached the frame budget, these two gates fought — the thread overran one boundary, causing 16ms/33ms oscillation (visible judder).

Additionally, CVDisplayLink is not actually vsync-locked on multi-monitor setups — it degrades to a simple high-resolution timer (documented by Tristan Hume's disassembly of CoreVideo).

**Lesson: a single vsync gate (displaySyncEnabled) is correct. Two gates fight.**

### IOSurface + CAMetalDisplayLink decoupled presenter (removed April 2026)

Content thread wrote to an IOSurface triple buffer. A separate CAMetalDisplayLink-based presenter on the main thread read the latest IOSurface and presented to the output drawable at vsync. Fully decoupled — content timing independent of display timing.

**Failed because:**

1. **GPU contention at high FPS.** At uncapped/high content FPS, the content command queue flooded the GPU with work. The presenter's blit (on a separate command queue) was starved for GPU time, causing missed vsync deadlines and stutter. Running at 100fps looked WORSE than 60fps — the opposite of expected.

2. **Temporal aliasing at non-integer frame ratios.** At 100fps on a 120Hz display, the presenter got fresh frames for 100 out of 120 callbacks. The 20 stale callbacks were scattered irregularly, creating uneven motion. Only frame rates that divide evenly into the display rate (120, 60, 40, 30) looked smooth.

3. **Unnecessary complexity.** The IOSurface intermediary added a GPU blit per frame, a second command queue, and complex lifecycle management — all to solve a problem (heavy-scene judder) that `displaySyncEnabled` with triple drawables already handles naturally (previous drawable stays visible when a frame misses the deadline).

**Lesson: direct present is simpler, faster, and handles all edge cases. Every game engine and VJ tool uses this model for a reason.**

### presentsWithTransaction in CAMetalDisplayLink callback (removed April 2026)

`commit_and_wait_scheduled()` blocks the main thread until the GPU schedules the presenter's blit. On a 120Hz display with high content FPS, the GPU queue was deep and the block took 2-3ms per callback. At 120 callbacks/sec, this consumed 300ms/sec of main thread time — overloading it and causing frame skips.

**Lesson: presenter callbacks must be non-blocking.** But the non-blocking path still had the GPU contention issue — the fundamental problem was the separate command queue, not the blocking.

### displaySyncEnabled = false / mailbox mode (rejected April 2026)

Tested as part of the CVDisplayLink removal. Without display sync, WindowServer still prevents tearing (it composites at its own vsync), but present timing is uncontrolled. Frames arrive at arbitrary phase relative to the compositor cycle, creating irregular display times perceived as judder.

### Unified CVDisplayLink (rejected April 2026)

One CVDisplayLink callback doing content notification + presenter blit + UI signal. Failed because the presenter's `nextDrawable()` + GPU blit (2-5ms) pushed the callback past the 8.3ms vsync deadline at 120Hz. CoreVideo skipped callbacks, starving the content thread. Render FPS dropped from 60 to 48.

### mach_wait_until with small spin margin (rejected April 2026)

`mach_wait_until` with 100-500μs spin margin. `mach_wait_until` is a software timer with ~1ms wake resolution — the spin margin was too small, and the thread consistently woke past the deadline. Result: locked at 57-58fps instead of 60fps. The 2ms margin covers the measured overshoot.

`THREAD_TIME_CONSTRAINT_POLICY` was also tested to improve `mach_wait_until` precision. It helps the thread get CPU immediately when woken, but does NOT improve the wake precision itself (that's governed by the kernel timer subsystem, not the scheduler).

## Key Files

| File | What |
|------|------|
| `manifold-app/src/content_thread.rs` | Content thread run loop, THREAD_TIME_CONSTRAINT, timer wait |
| `manifold-app/src/frame_timer.rs` | `FrameTimer` with mach_wait_until + spin, EWMA FPS |
| `manifold-app/src/content_pipeline.rs` | Direct present path (nextDrawable → blit → present) |
| `manifold-app/src/display_link.rs` | `UiDisplayLink` (CVDisplayLink for UI thread only) |
| `manifold-app/src/shared_texture.rs` | IOSurface triple buffer (workspace preview only) |
| `manifold-app/src/output_presenter.rs` | CAMetalDisplayLink presenter (dead code, kept for reference) |
| `manifold-gpu/src/metal/surface.rs` | `GpuSurface` (CAMetalLayer), `GpuDrawable`, displaySyncEnabled |
| `manifold-gpu/src/metal/vsync.rs` | `GpuVsyncSignal`/`GpuVsyncWaiter` (dead code, kept for reference) |
| `manifold-gpu/src/metal/encoder.rs` | `present_drawable()`, `commit()` |
| `manifold-core/src/settings.rs` | `frame_rate` project setting |
