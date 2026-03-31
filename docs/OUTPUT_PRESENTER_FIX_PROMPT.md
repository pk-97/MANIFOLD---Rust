# Fix: Integrate Output Presenter into Main Frame Encoder

Read `CLAUDE.md` before starting.

## Problem

The output monitor window runs on a dedicated thread (`PresenterThread`) with its own `MTLCommandQueue`. This causes GPU scheduler contention with the UI thread's command queue — visible as flickering and frame drops when the output window is fullscreen. This was the original motivation for the full native Metal migration.

## Solution

Move the output presenter's rendering into the main frame encoder in `present_all_windows()`. Both the workspace drawable and the output drawable are rendered and presented from the same `GpuEncoder` → single `commit()` → single command queue → zero contention.

Metal supports presenting multiple drawables from one command buffer. Call `encoder.present_drawable()` for each drawable before `commit()`.

## Architecture

**Before (two queues, contention):**
```
UI Thread GpuDevice (Queue A)        Output Thread (Queue B)
  workspace encoder → commit()         output encoder → commit()
  GPU schedules both → contention      ← separate command queue
```

**After (one queue, no contention):**
```
UI Thread GpuDevice (single queue)
  frame encoder:
    Pass 1-5: workspace rendering
    Pass 6: output blit (IOSurface → output drawable)
    present_drawable(workspace_drawable)
    present_drawable(output_drawable)
    commit()  ← single submission
```

## Files to Read First

1. `crates/manifold-app/src/output_presenter.rs` — current implementation (thread + pipeline)
2. `crates/manifold-app/src/app_render.rs` — where the output pass will be added
3. `crates/manifold-app/src/app.rs` — Application struct, output_presenter field
4. `crates/manifold-app/src/app_lifecycle.rs` — where output window is created

## Implementation

### 1. Create OutputBlitter (replaces PresenterThread)

The output presenter currently has:
- A dedicated thread that polls for new content
- Its own `MTLCommandQueue`
- A hand-written MSL fullscreen-triangle shader
- A `CAMetalLayer` at project resolution with EDR support
- `displaySyncEnabled = true` for vsync on the output display

Replace the threaded architecture with a simple struct that holds the layer and pipeline:

```rust
pub struct OutputBlitter {
    /// CAMetalLayer on the output window (project resolution, EDR, vsync).
    layer: manifold_gpu::GpuSurface,  // or keep raw CAMetalLayer if GpuSurface doesn't fit
    /// Fullscreen blit pipeline for the output window.
    pipeline: manifold_gpu::GpuRenderPipeline,
    sampler: manifold_gpu::GpuSampler,
    /// Last front_index blitted — skip if unchanged (no new content).
    last_front_index: u32,
}
```

The blit shader can reuse the same WGSL fullscreen-triangle shader as the workspace blit (already exists on Application). Or create the pipeline from the existing MSL source.

**Key properties to preserve from the current output presenter:**
- `drawableSize` = project resolution (NOT window backing pixels)
- `contentsScale = 1.0` (pixel-perfect)
- `displaySyncEnabled = true` (vsync on the output display)
- `wantsExtendedDynamicRangeContent = true` + ExtendedLinearSRGB colorspace (EDR/HDR)
- Black background color for letterbox bars
- Pixel format: `Rgba16Float` (HDR)

**Important:** The `CAMetalLayer` must use the UI thread's `GpuDevice`'s underlying MTLDevice — NOT a separate device. This ensures the drawables can be presented from the main encoder's command buffer.

### 2. Modify present_all_windows()

After the workspace rendering (Passes 1-5), add an output pass:

```rust
// Pass 6: Output presenter (blit IOSurface to output drawable)
if let Some(output) = &mut self.output_blitter {
    let current_front = self.shared_texture_bridge.as_ref()
        .map(|b| b.front_index())
        .unwrap_or(0);

    // Only blit if new content available
    if current_front != output.last_front_index {
        output.last_front_index = current_front;

        if let Some(compositor_tex) = self.ui_shared_textures[current_front as usize].as_ref() {
            if let Some(drawable) = output.layer.next_drawable() {
                let output_tex = drawable.gpu_texture(manifold_gpu::GpuTextureFormat::Rgba16Float);
                encoder.draw_fullscreen(
                    &output.pipeline,
                    &output_tex,
                    &[
                        manifold_gpu::GpuBinding::Texture { binding: 0, texture: compositor_tex },
                        manifold_gpu::GpuBinding::Sampler { binding: 1, sampler: &output.sampler },
                    ],
                    true,  // clear to black (letterbox bars)
                    true,
                    "Output Blit",
                );
                encoder.present_drawable(&drawable);
            }
        }
    }
}

// Present workspace drawable + commit (both drawables in same command buffer)
encoder.present_drawable(&workspace_drawable);
encoder.commit();
```

Note: the `ui_shared_textures` are already imported as GpuTexture (Rgba16Float IOSurface). The same textures work for both the workspace blit (sampled into Bgra8Unorm drawable) and the output blit (sampled into Rgba16Float drawable). The output gets the full HDR range.

### 3. Handle Output Window Lifecycle

In `app_lifecycle.rs` where `open_output_window()` is called:
- Instead of creating a `NativeOutputPresenter` (which spawns a thread), create an `OutputBlitter`
- The `OutputBlitter` creates its `CAMetalLayer` attached to the output window's NSView
- Configure the layer with the same properties as the current output presenter (resolution, EDR, vsync, etc.)
- Store it on Application

In `close_output_window()`: drop the `OutputBlitter`.

### 4. Remove the Threaded Output Presenter

Once the `OutputBlitter` is working:
- Remove `PresenterThread` and the thread-spawning code from `output_presenter.rs`
- Remove the `NativeOutputPresenter` handle (or repurpose it as `OutputBlitter`)
- Remove the separate `MTLCommandQueue` creation
- Remove the 500μs poll loop
- Keep the `CAMetalLayer` setup code (reuse for `OutputBlitter`)

### 5. Skip Output Blit When No New Content

The current presenter polls at ~2000Hz (500μs sleep) and only blits when front_index changes. In the main frame encoder (running at 120Hz UI refresh), we check if front_index changed since last blit:
- If changed: acquire drawable, blit, present
- If unchanged: skip (don't acquire drawable, don't present) — the output display holds the last frame

This is more efficient than the polling approach — no wasted CPU cycles, no empty command buffers.

## Verification

1. `cargo clippy --workspace -- -D warnings` — must pass
2. `cargo test --workspace` — must pass
3. Open output monitor window (fullscreen preferred)
4. Play content — output should show compositor output at project resolution
5. **No flickering or frame drops** on EITHER the workspace or output window
6. HDR/EDR should work if the output display supports it
7. Closing the output window should clean up without crash

## Critical Rules

- The output `CAMetalLayer` MUST use the same MTLDevice as the UI thread's GpuDevice
- `displaySyncEnabled = true` on the output layer (vsync)
- `drawableSize` = project resolution, NOT window backing pixels
- Output pixel format = `Rgba16Float` (HDR)
- Only blit when front_index changes (skip redundant presents)
- Both workspace and output drawables presented in the SAME `commit()`
- `cargo clippy --workspace -- -D warnings` must pass
- Commit and push when done
