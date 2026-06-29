# Editor UI caps at ~80fps during playback — present serializes behind the content GPU render

**Status:** diagnosed, not fixed (decision: leave it — see §6). 2026-06-29.

**One line:** the editor window can't hold 120fps *while a project plays* because the
UI's present command buffer serializes behind the content thread's per-frame GPU
render on the shared GPU. It is **not** the timeline redesign.

## 1. Symptom

The editor (main window) UI used to feel like a locked 120fps and now reads
~77–80fps in the perf HUD during playback. The projected **output** window is on
its own path and is unaffected — this is operator-surface feel only.

## 2. The diagnostic tool

`MANIFOLD_UI_FRAME_PROFILE=1` enables a per-frame breakdown of the main-window
frame, printed every 60 frames. Implementation: [`crates/manifold-app/src/ui_frame_profile.rs`](../crates/manifold-app/src/ui_frame_profile.rs),
wired into `tick_and_render` / `present_all_windows` in
[`app_render.rs`](../crates/manifold-app/src/app_render.rs). Zero cost when unset
(every record short-circuits on the `enabled` flag).

It attributes the frame to every pass (state drain, event processing, tree
rebuild, repaint/upload, and each present sub-pass: clip bodies, waveforms,
thumbnails, panels, overlay, commit, `next_drawable`, blit). The header reports:

- `display link XXHz` — the CVDisplayLink's live actual refresh.
- `cpu` — measured wall time of the frame body.
- `UI offscreen GPU` — the **true** GPU execution time of the UI offscreen
  "Frame" buffer (`GPUEndTime − GPUStartTime`, captured async via
  `GpuEncoder::add_gpu_time_handler`).
- `vsync/idle wait` = `dt − cpu`. ≈0 ⇒ the thread is parked *inside* the frame
  body (e.g. blocked in a Metal call), not idle.

## 3. What the numbers showed (M4 Max, built-in 120Hz panel)

| State | display link | UI offscreen GPU | `next_drawable` block | fps |
|---|---|---|---|---|
| Empty project | 120.0Hz | 0.53ms | ~2.5ms | **115** |
| Project playing | 120.0Hz | ~0.9ms | ~8–11ms | **80** |

Every UI CPU pass is sub-0.1ms (tree rebuild ~0.005ms, clip bodies ~0.01ms,
thumbnails ~0.01ms). The actual blit is ~0.02ms. **The entire lost time is the UI
thread blocking in `surface.next_drawable()`** — `vsync/idle wait ≈ 0`, so it is
parked in that call, not idle.

## 4. Root cause

The display is genuinely at 120Hz (not ProMotion demotion — the live refresh
reads 120.0Hz, and an empty project hits 115fps). The block is **drawable
starvation** caused by GPU scheduling, not by anything the UI computes:

- The content thread renders large IOSurfaces (8192×1152 + 2048×1152 +
  1984×1116) every frame on the **shared** GPU.
- The UI reads the *already-completed* front IOSurface via an atomic handoff
  ([`shared_texture.rs`](../crates/manifold-app/src/shared_texture.rs)) — there is
  no logical GPU wait there.
- But the UI's present command buffer is submitted to the **same physical GPU**.
  It queues behind content's in-flight render and cannot start until that work
  clears. The CAMetalLayer drawable is not released until the UI present
  *completes*. So the next `next_drawable()` blocks ≈ **content's per-frame GPU
  time**.
- The UI's own GPU execution stays ~1ms because once it runs it is instant — it
  just runs *late*. Even the fast re-blit path blocks, because its tiny blit is
  also queued behind content.

This matches: empty project → content GPU ≈ 0 → 115fps; playing → content GPU
~8ms → `next_drawable` ~8–11ms → 80fps.

Present config is not at fault: `maximumDrawableCount=3`, `displaySyncEnabled=false`,
CVDisplayLink as pacer (see [`app_lifecycle.rs`](../crates/manifold-app/src/app_lifecycle.rs)
/ [`surface.rs`](../crates/manifold-gpu/src/metal/surface.rs)).

## 5. The timeline redesign is exonerated

Measured on **both** axes: the new clip-SDF tiles, per-clip thumbnails, and
waveform textures cost microseconds of CPU to encode, and the whole UI offscreen
render is ~1ms of GPU execution. The 120→80 change tracks content GPU activity,
not anything the redesign added.

## 6. Not measured / open

The one number that turns "confident" into "proven" is the content thread's
**actual per-frame GPU time** — the same `GPUStartTime/GPUEndTime` handler on the
content render buffer. Expectation: it ≈ the `next_drawable` block. Not yet run.

## 7. Fix options (none taken — leave it for now)

1. **Split the content command buffer** so the GPU scheduler can interleave the
   UI present. **Not only a net positive:** splitting *within* a render pass
   forces a tile-memory store/load (bandwidth hit on Apple Silicon TBDR); more
   buffers add submit/flush overhead that can make the **show** render slower;
   and interleaving is not guaranteed (UI present at 120Hz and content at 60fps
   are async — it lowers the *average* block, not deterministically). Wrong risk
   direction for a live rig.
2. **Render the editor preview smaller** — cut content's preview-surface GPU cost
   directly. Helps editor *and* show, no scheduling gamble. The safer win if we
   ever act.
3. **Leave it (chosen)** — the editor runs ~80fps *only while a project plays*;
   the projected output is unaffected. Zero risk, zero work.

If revisited, start by confirming §6, then prefer option 2 over option 1.
