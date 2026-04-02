# VSync & Frame Pacing Architecture

## Decision: 2026-04-02

Replace the sleep-based FPS limiter on the content thread with true display-synchronized rendering via CVDisplayLink. Three independent CVDisplayLinks drive three independent consumers, each with its own lightweight callback.

## Architecture: Three Independent CVDisplayLinks

```
Physical Display (120Hz)
    │
    ├── CVDisplayLink 1 → GpuVsyncSignal     → Content thread (condvar notify)
    │                     manifold-gpu           Wakes frame production
    │
    ├── CVDisplayLink 2 → DisplayLinkPresenter → Output window present
    │                     display_link.rs         Fullscreen: blit in callback
    │                                             Windowed: flag → main thread blit
    │
    └── CVDisplayLink 3 → UiDisplayLink       → UI thread (request_redraw)
                          display_link.rs         Wakes winit event loop
```

**Each callback is lightweight (<1μs) and never misses its vsync deadline.** This is the critical invariant — CVDisplayLink skips the next callback if the current one overruns the vsync interval (~8ms at 120Hz).

### Why Three Independent Links, Not One Unified

A unified CVDisplayLink callback that does content notification + presenter blit + UI signal was attempted and **rejected**. The presenter's `nextDrawable()` + GPU blit takes 2-5ms. At 120Hz (8.3ms per vsync), this pushes the callback past the deadline, causing CoreVideo to skip the next callback — starving the content thread and dropping render FPS from 60 to 48.

**Lesson: never put heavy GPU work in a vsync callback that also serves as the timing source for other consumers.** This matches how Resolume Arena and TouchDesigner handle multi-output live performance — every part of the pipeline is an independent, minimal unit with its own scheduling.

## Content Thread VSync (GpuVsyncSignal)

**File:** `manifold-gpu/src/metal/vsync.rs`

The content thread blocks on a `Condvar` instead of sleeping. A dedicated CVDisplayLink fires at the display's refresh rate, increments a counter, and notifies the condvar. The content thread wakes, checks the frame divisor, and renders or skips.

### Two-Part Design

- **GpuVsyncSignal** (UI thread): Owns the CVDisplayLink. Handles retargeting when windows move between displays. Supports headless mode (no CVDisplayLink — external code calls `notify_vsync()`).
- **GpuVsyncWaiter** (content thread): Blocks on the shared `Condvar`. No CVDisplayLink access. Content thread code is unchanged — just calls `waiter.wait(last_count)`.

### Frame Divisor (Clean VSync Rounding)

When the project FPS differs from the display refresh rate, the content thread renders every Nth vsync:

```
divisor = max(1, round(display_hz / target_fps))
actual_fps = display_hz / divisor
```

| Display | Project FPS | Divisor | Actual FPS |
|---------|-------------|---------|------------|
| 120Hz   | 60          | 2       | 60         |
| 120Hz   | 30          | 4       | 30         |
| 120Hz   | 40          | 3       | 40         |
| 60Hz    | 30          | 2       | 30         |
| 60Hz    | 24          | 3       | 20 (!)     |

### Display Hz Detection

`CVDisplayLinkGetActualOutputVideoRefreshPeriod` returns 0 before the first callback fires (the link hasn't measured the period yet). **Hz is derived from the CVTimeStamp's `video_refresh_period` / `video_time_scale` fields inside the callback** — always accurate from the first vsync. The content thread waits for the first callback if Hz is still 0 at startup.

### Fallback Chain

1. VSync signal present + Hz > 0 → vsync-driven pacing
2. VSync signal present but Hz = 0 → wait for first callback (32ms timeout)
3. No VSync signal (export mode, headless, non-macOS) → timer-based sleep pacing

### VSync Wait Timeout

The condvar wait has a **32ms timeout** (~1 frame at 30Hz). During display transitions (fullscreen animation, display sleep/disconnect), the CVDisplayLink may stop firing temporarily. The short timeout ensures the content thread degrades gracefully to timer-based pacing instead of stalling.

## Output Presenter (DisplayLinkPresenter)

**File:** `manifold-app/src/display_link.rs`

Two modes based on `presentation` flag:

### Fullscreen (Direct Display)

CVDisplayLink callback does the full blit + present every vsync:
1. Read `front_index` from IOSurface triple buffer
2. `nextDrawable()` → acquire CAMetalLayer drawable
3. Blit IOSurface → drawable (single fullscreen draw)
4. `presentDrawable` on command buffer → `commit()`

**Must present on every vsync** — even re-presenting the same frame. Missing a single present causes WindowServer to drop Direct Display mode, thrashing ALL displays (including the MacBook). This is why the callback does the present directly rather than deferring to the main thread.

### Windowed (Compositor-Synchronized)

CVDisplayLink callback is lightweight — sets `AtomicBool` flag + calls `request_redraw()` (same pattern as `UiDisplayLink`). The main thread does the blit in `about_to_wait()`:

1. Check `present_if_ready()` → consumes the atomic flag
2. Blit IOSurface → drawable
3. `commit_and_wait_scheduled()` → block until GPU has the blit queued
4. `drawable.present_after_scheduled()` → present directly into Core Animation transaction

**`presentsWithTransaction = true`** on the CAMetalLayer. This syncs the present with WindowServer's compositor schedule, reducing phase mismatch judder. Requires the present to happen on the main thread where CA transactions exist — `presentsWithTransaction` silently discards presents from background threads (CVDisplayLink callbacks).

**Tradeoff:** ~1 frame additional latency in windowed mode (present happens on the next event loop iteration). Invisible for a preview window. Fullscreen has zero additional latency.

## UI Thread VSync (UiDisplayLink)

**File:** `manifold-app/src/display_link.rs`

Lightweight: sets `AtomicBool` + calls `request_redraw()`. The winit event loop checks `vsync_ready()` in `about_to_wait()` to decide when to render. Replaces the free-running `FrameTimer` for UI thread pacing.

## IOSurface Triple Buffer

**File:** `manifold-app/src/shared_texture.rs`

Content thread → UI/presenter frame transport:
- 3 IOSurface buffers (zero-copy kernel memory)
- Atomic `front_index` tracks which surface is safe to read
- GPU completion handler (`add_completed_handler`) publishes `front_index` asynchronously when the content thread's GPU work finishes — decoupled from the content thread's sleep/wake cycle
- Presenter reads whatever `front_index` is current — never waits for the content thread

## Display Retargeting

When windows move between displays, CVDisplayLinks retarget to the new display:

- **Content VSync (`GpuVsyncSignal`):** Retargets to output display when output window opens, back to primary when it closes. Updated on screen-change notifications.
- **Presenter (`DisplayLinkPresenter`):** Retargets to whatever display the output window is on.
- **UI (`UiDisplayLink`):** Retargets to whatever display the primary window is on.

`CVDisplayLinkSetCurrentCGDisplay` is safe to call while the link is running (Apple docs). At most 1 vsync fires at the old display's timing — acceptable.

## Project Settings

- `vsync_enabled: bool` (default `true`) — enables/disables content thread VSync pacing
- `frame_rate: f32` (default `60.0`) — target FPS, snapped to nearest clean display divisor when VSync is active
- UI: footer bar shows `[VSYNC]` toggle button + `→60` resolved FPS label

## What We Tried and Why It Failed

### Unified CVDisplayLink (rejected)

One CVDisplayLink callback doing content notification + presenter blit + UI signal. **Failed because the presenter's `nextDrawable()` + GPU blit (2-5ms) pushed the callback past the 8.3ms vsync deadline at 120Hz.** CoreVideo skipped callbacks, starving the content thread. Render FPS dropped from 60 to 48 with heavy oscillation.

### presentsWithTransaction from CVDisplayLink callback (rejected)

Set `presentsWithTransaction = true` on CAMetalLayer and called `drawable.present()` from the CVDisplayLink background thread. **Black window.** Core Animation transactions only exist on the main thread's run loop. The present was silently discarded because there was no CA transaction context on the background thread.

### presentsWithTransaction without waitUntilScheduled (rejected)

Set `presentsWithTransaction = true` and used the standard `encoder.present_drawable()` + `commit()` path. **Black window.** With transactions enabled, the standard present path doesn't work — you must commit without presentDrawable, call `waitUntilScheduled`, then present the drawable manually.

## Key Files

| File | What |
|------|------|
| `manifold-gpu/src/metal/vsync.rs` | `GpuVsyncSignal`, `GpuVsyncWaiter`, CVDisplayLink FFI, `display_id_for_window` |
| `manifold-app/src/display_link.rs` | `DisplayLinkPresenter` (fullscreen + windowed), `UiDisplayLink` |
| `manifold-app/src/content_thread.rs` | Content thread run loop (vsync wait + timer fallback) |
| `manifold-app/src/frame_timer.rs` | `FrameTimer` with vsync mode (divisor, actual_fps) |
| `manifold-app/src/content_pipeline.rs` | GPU completion handler publishes `front_index` |
| `manifold-app/src/shared_texture.rs` | IOSurface triple buffer + atomic front_index |
| `manifold-core/src/settings.rs` | `vsync_enabled`, `frame_rate` project settings |
| `manifold-gpu/src/metal/encoder.rs` | `commit_and_wait_scheduled()` for CA transactions |
| `manifold-gpu/src/metal/surface.rs` | `present_after_scheduled()` for CA transactions |
