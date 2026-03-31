# Phase 6 — Full Render Loop Conversion

Read `CLAUDE.md` and `docs/NATIVE_METAL_UI_MIGRATION.md` before starting.

**The big one.** You are rewriting the entire workspace render loop from wgpu to a single `GpuEncoder` per frame. All rendering modules have been converted in previous phases — this phase wires them together, replaces the remaining wgpu infrastructure (BlitPipeline, SurfaceWrapper, GpuContext wgpu fields, SharedTextureBridge wgpu imports), and removes wgpu from the project entirely.

After this phase, the workspace window renders via native Metal: one GpuDevice, one GpuEncoder per frame, one command buffer commit, zero wgpu. Visual output is fully restored.

Read every file you need to modify BEFORE making changes. This is a large integration phase — understand the existing render loop thoroughly before rewriting it. Work without breaks. Complete all tasks. Build with `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` after all changes. Fix any issues. Commit and push when done.

## State After Phases 1–5

| Module | State | API |
|--------|-------|-----|
| `LayerBitmapGpu` | Native Metal (Phase 3) | `render_layers(&mut GpuEncoder, &GpuTexture, w, h, rects)` |
| `NativeTextRenderer` | Native Metal (Phase 4) | `prepare(device, ...) → render(encoder, target, load)` |
| `UIRenderer` | Native Metal (Phase 5) | `prepare(device, ...) → render(encoder, target, load)` |
| `UICacheManager` | Native Metal (Phase 5) | `render_dirty_panels(device, ui_renderer, tree, panels)` |
| `BlitPipeline` | **wgpu — REPLACE** | Fullscreen blit with viewport (aspect-fit) |
| `PanelCompositor` | **Deleted in Phase 5** | Was: fullscreen atlas blit (premultiplied alpha) |
| `SurfaceWrapper` | **wgpu — REPLACE** | Per-window wgpu surface |
| `GpuContext` | **wgpu + native — REPLACE** | Has both wgpu and GpuDevice |
| `SharedTextureBridge` | **wgpu import — REPLACE** | `import_texture()` uses wgpu-hal |

## Target Render Loop

```
present_all_windows():
    // ── Panel cache (own encoders, already native from Phase 5) ──
    UICacheManager.render_dirty_panels(device, ui_renderer, tree, panels)

    // ── Acquire drawable ──
    drawable = workspace_surface.next_drawable()
    drawable_texture = drawable → GpuTexture

    // ── Single encoder for the frame ──
    encoder = device.create_encoder("Frame")

    // Pass 1: Clear to black
    encoder.clear_texture(drawable_texture, 0, 0, 0, 1)

    // Pass 2: Blit compositor output into video area (aspect-fit viewport)
    encoder.draw_indexed(blit_pipeline, drawable_texture, [compositor_tex, sampler],
                         quad_vbuf, quad_ibuf, 6, Some(fit_viewport), Load, "Blit")

    // Pass 3: Blit UI atlas fullscreen (premultiplied alpha over video)
    encoder.draw_fullscreen(atlas_pipeline, drawable_texture, [atlas_tex, sampler],
                            false/*load*/, true/*store*/, "Atlas")

    // Pass 4: Layer bitmaps (directly to drawable, not intermediate)
    bitmap_gpu.render_layers(encoder, drawable_texture, w, h, rects)

    // Pass 5: Overlay UI (directly to drawable, not intermediate)
    ui_renderer.render(encoder, drawable_texture, Load)

    // ── Present + commit ──
    encoder.present_drawable(drawable)
    encoder.commit()
```

## Task 1: Add GpuTexture-from-Drawable Helper (manifold-gpu)

**File:** `crates/manifold-gpu/src/metal/surface.rs`

The encoder methods (`draw_indexed`, `draw_fullscreen`) take `&GpuTexture` as the render target. The drawable's backing texture is a `&metal::TextureRef` (borrowed, not owned). We need a way to create a `GpuTexture` from the drawable for the duration of the frame.

Add to `GpuDrawable`:
```rust
/// Create a GpuTexture referencing this drawable's backing texture.
/// The drawable must outlive the returned GpuTexture.
/// Retains the Metal texture — safe to use alongside the drawable.
pub fn gpu_texture(&self, format: GpuTextureFormat) -> GpuTexture {
    let tex_ref = self.texture();
    let w = tex_ref.width() as u32;
    let h = tex_ref.height() as u32;
    // Retain: GpuTexture drops will release, so we need our own +1
    let ptr = tex_ref.as_ptr() as *mut std::ffi::c_void;
    unsafe { super::objc_retain(ptr); }
    let mtl_texture = unsafe { metal::Texture::from_ptr(ptr as *mut _) };
    unsafe {
        GpuTexture::from_raw(mtl_texture, w, h, 1, format)
    }
}
```

This is the ONLY manifold-gpu modification in this phase.

## Task 2: Replace GpuContext

**File:** `crates/manifold-renderer/src/gpu.rs`

Remove ALL wgpu fields. GpuContext becomes a thin wrapper around GpuDevice:

```rust
pub struct GpuContext {
    pub device: manifold_gpu::GpuDevice,
}

impl GpuContext {
    pub fn new() -> Self {
        Self { device: manifold_gpu::GpuDevice::new() }
    }
}
```

Remove:
- `wgpu::Instance`, `wgpu::Adapter`, `Arc<wgpu::Device>`, `Arc<wgpu::Queue>`
- The async `new()` method — replace with a simple `fn new() -> Self`
- The `create_device_from_adapter` helper
- All `wgpu` imports

Find ALL callers of `GpuContext::new()` in manifold-app and update them. The old code used `pollster::block_on(GpuContext::new(...))` — replace with `GpuContext::new()`.

All code that accessed `gpu.device` (wgpu Device), `gpu.queue` (wgpu Queue), `gpu.instance`, `gpu.adapter` must be updated. After Phase 5, the only remaining wgpu consumers should be:
- `BlitPipeline` creation and usage — being replaced in this phase
- `SurfaceWrapper` creation and usage — being replaced in this phase
- `SharedTextureBridge::import_texture()` — being replaced in this phase

All code that accessed `gpu.native_device` should now access `gpu.device` instead (since there's only one device now).

## Task 3: Replace SurfaceWrapper with GpuSurface

**File:** `crates/manifold-app/src/app.rs`, `crates/manifold-app/src/app_render.rs`

Replace the `workspace_surface: Option<SurfaceWrapper>` field with `workspace_surface: Option<manifold_gpu::GpuSurface>`.

**Surface creation:** Find where `SurfaceWrapper::new(...)` is called. Replace with:
```rust
let surface = gpu.device.create_surface(&window, width, height, format, true/*vsync*/);
```
Use `GpuTextureFormat::Bgra8Unorm` as the format (standard macOS surface format).

**Surface resize:** Replace `surface.resize(&gpu.device, w, h, scale)` with `surface.resize(w, h)`. Store `scale_factor` separately if needed (GpuSurface doesn't track it — add a local field or read from winit).

**Drawable acquisition:** Replace `surface.get_current_texture()` with `surface.next_drawable()`. Handle `None` (no drawable available) by skipping the frame.

**Delete** `crates/manifold-renderer/src/surface.rs` and remove `pub mod surface;` from `lib.rs`.

## Task 4: Replace SharedTextureBridge UI-side Imports

**File:** `crates/manifold-app/src/shared_texture.rs`, `crates/manifold-app/src/app.rs`, `crates/manifold-app/src/app_render.rs`

The UI thread currently imports IOSurface textures as wgpu::Texture via `bridge.import_texture(&gpu.device, i)`. Replace with the native path that already exists:

```rust
let textures: [manifold_gpu::GpuTexture; SURFACE_COUNT] =
    std::array::from_fn(|i| unsafe { bridge.import_texture_native(&gpu.device, i) });
```

Update ALL call sites in `app.rs` and `app_render.rs` where `import_texture()` is called.

The `ui_shared_textures` and `ui_shared_views` fields on Application (which stored wgpu::Texture and wgpu::TextureView) should be replaced with `[Option<manifold_gpu::GpuTexture>; SURFACE_COUNT]`.

**Remove `import_texture()` from SharedTextureBridge** (the wgpu version). Also remove all wgpu, wgpu-hal, wgpu-types imports from shared_texture.rs. The only import method left is `import_texture_native()`.

## Task 5: Create Native Blit Pipeline

The compositor output needs to be blitted to the drawable at an aspect-fit viewport rect. `draw_fullscreen()` doesn't support viewports, so use `draw_indexed()` with a fullscreen quad and a viewport parameter.

Create the pipeline and resources in Application (or a small struct):

**Blit WGSL shader** (same as current BLIT_SHADER but with vertex inputs for the quad):
```wgsl
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(in.position, 0.0, 1.0);
    out.uv = in.uv;
    return out;
}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_source, s_source, in.uv);
}
```

- Pipeline: `create_render_pipeline_with_vertex_layout(...)` with no blend (opaque replace = `None`).
- Vertex layout: Float32x2 @0 (position) + Float32x2 @8 (uv), stride 16.
- Pre-allocated shared vertex buffer: 4 vertices = 64 bytes (fullscreen NDC quad: [-1,-1], [1,-1], [1,1], [-1,1] with UVs [0,1], [1,1], [1,0], [0,0]).
- Pre-allocated shared index buffer: [0,1,2,0,2,3] = 24 bytes.
- Linear sampler.

**Usage in render loop:**
```rust
// Compute aspect-fit viewport
let (fit_x, fit_y, fit_w, fit_h) = aspect_fit(video_rect, source_aspect);
encoder.draw_indexed(
    &blit_pipeline, &drawable_tex,
    &[
        GpuBinding::Texture { binding: 0, texture: &compositor_output },
        GpuBinding::Sampler { binding: 1, sampler: &blit_sampler },
    ],
    &blit_vbuf, &blit_ibuf, 6,
    Some((fit_x, fit_y, fit_w, fit_h)),
    GpuLoadAction::Load, // surface was already cleared
    "Blit Compositor",
);
```

## Task 6: Create Native Atlas Blit

The atlas needs to be blitted fullscreen over the video with premultiplied alpha.

Use `draw_fullscreen()` — no vertex buffer needed, no viewport needed (atlas covers full screen).

**Atlas blit shader** — same fullscreen-triangle pattern already used by manifold-gpu:
```wgsl
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
```
The fullscreen triangle vertex shader is generated internally by `draw_fullscreen`.

- Pipeline: `create_render_pipeline(ATLAS_BLIT_SHADER, "vs_main", "fs_main", format, Some(premultiplied_blend), "Atlas Blit")`
- Premultiplied blend: `src=One, dst=OneMinusSrcAlpha` (both color and alpha).
- Nearest sampler (matches old PanelCompositor: pixel-perfect atlas sampling).

**Usage:**
```rust
if let Some(atlas) = ui_cache_manager.atlas_texture() {
    encoder.draw_fullscreen(
        &atlas_pipeline, &drawable_tex,
        &[
            GpuBinding::Texture { binding: 0, texture: atlas },
            GpuBinding::Sampler { binding: 1, sampler: &atlas_sampler },
        ],
        false, true, "Atlas Blit",
    );
}
```

## Task 7: Rewrite present_all_windows()

**File:** `crates/manifold-app/src/app_render.rs`

This is the core change. Read the ENTIRE existing `present_all_windows()` method before rewriting.

**New flow:**

```rust
fn present_all_windows(&mut self) {
    let Some(gpu) = &self.gpu else { return };
    let Some(surface) = &self.workspace_surface else { return };

    // ── Panel cache update (own encoders, already native) ──
    if let (Some(cm), Some(ui)) = (&mut self.ui_cache_manager, &mut self.ui_renderer) {
        let scale = self.scale_factor; // or wherever scale is stored
        let panel_infos = self.ui_root.panel_cache_info();
        cm.set_scale_factor(scale);
        cm.ensure_atlas(&gpu.device, logical_w, logical_h);
        cm.render_dirty_panels(&gpu.device, ui, &self.ui_root.tree, &panel_infos);
        self.ui_root.tree.clear_dirty();
    }

    // ── Acquire drawable ──
    let Some(drawable) = surface.next_drawable() else {
        log::warn!("No drawable available — skipping frame");
        return;
    };
    let drawable_tex = drawable.gpu_texture(manifold_gpu::GpuTextureFormat::Bgra8Unorm);

    // ── Build the frame ──
    let mut encoder = gpu.device.create_encoder("Frame");

    // Pass 1: Clear to black
    encoder.clear_texture(&drawable_tex, 0.0, 0.0, 0.0, 1.0);

    // Pass 2: Blit compositor output to video area
    // (get compositor IOSurface texture, compute aspect-fit viewport, draw_indexed)
    // ... same aspect-fit logic as the old blit_to_rect_fit ...

    // Pass 3: Atlas blit (premultiplied alpha over video)
    // ... draw_fullscreen with atlas_pipeline ...

    // Pass 4: Layer bitmaps (directly to drawable)
    if let Some(bitmap_gpu) = &mut self.layer_bitmap_gpu {
        let rects = /* same rect collection as before */;
        if !rects.is_empty() {
            bitmap_gpu.render_layers(&mut encoder, &drawable_tex, logical_w, logical_h, &rects);
        }
    }

    // Pass 5: Overlay UI (directly to drawable)
    if let Some(ui) = &mut self.ui_renderer {
        // ... queue overlay commands same as before ...
        if ui.prepare(&gpu.device, logical_w, logical_h, scale) {
            ui.render(&mut encoder, &drawable_tex, manifold_gpu::GpuLoadAction::Load);
        }
    }

    // ── Present + commit ──
    encoder.present_drawable(&drawable);
    encoder.commit();
}
```

**Important details:**
- `logical_w`, `logical_h`, `scale`: compute from surface dimensions and scale_factor. The GpuSurface stores width/height but not scale_factor — store scale_factor on Application.
- Compositor output: read from `self.ui_shared_textures[front_index]` (now GpuTexture, not wgpu).
- Aspect-fit computation: same math as old `blit_to_rect_fit()` — compute fit_x, fit_y, fit_w, fit_h from video_rect and source_aspect.
- All overlay command queuing (render_overlay_range, draw_rect for playhead, perf HUD, dropdown, etc.) stays the same — just the prepare/render at the end changes.

**Remove:**
- The wgpu `CommandEncoder` creation
- The wgpu `begin_render_pass` / `end_render_pass` calls
- The `BlitPipeline.prepare_rect_fit()` / `draw_in_pass()` calls
- The `PanelCompositor.draw_atlas()` call (already removed in Phase 5)
- The `surface_texture.present()` call (replaced by encoder.present_drawable)
- The `gpu.queue.submit()` call (replaced by encoder.commit)

**Remove intermediate targets:**
- `self.layer_bitmap_native_target` — no longer needed, LayerBitmapGpu draws to drawable directly
- `self.overlay_native_target` — no longer needed, UIRenderer draws to drawable directly

## Task 8: Delete Dead wgpu Modules

| File | Action |
|------|--------|
| `crates/manifold-renderer/src/blit.rs` | Delete |
| `crates/manifold-renderer/src/surface.rs` | Delete |
| `crates/manifold-renderer/src/gpu.rs` | Rewrite (GpuDevice only, no wgpu) |
| `crates/manifold-renderer/src/lib.rs` | Remove `blit`, `surface` modules |

## Task 9: Remove wgpu Dependencies

**`crates/manifold-renderer/Cargo.toml`:**
- Remove `wgpu = { workspace = true }`
- If any file still imports wgpu, fix it first

**`crates/manifold-app/Cargo.toml`:**
- Remove `wgpu`, `wgpu-hal`, `wgpu-types` dependencies
- Remove `pollster` if it was only used for `GpuContext::new()` async

**Workspace `Cargo.toml`:**
- Remove `wgpu`, `wgpu-hal`, `wgpu-types` from `[workspace.dependencies]` if no other crate uses them

## Task 10: Update Migration Doc

Update `docs/NATIVE_METAL_UI_MIGRATION.md`:
1. Mark Phase 6 as `[DONE]`
2. Note: "Full render loop converted. Single GpuEncoder per frame. wgpu removed from manifold-renderer and manifold-app. BlitPipeline, SurfaceWrapper, PanelCompositor all deleted. SharedTextureBridge import_texture wgpu path removed."
3. Update Phase 7 checklist with remaining cleanup items.

## Task 11: Build and Verify

1. `cargo clippy --workspace -- -D warnings` — must pass
2. `cargo test --workspace` — must pass
3. **App must render correctly:**
   - Video output visible in the workspace window (compositor blit)
   - UI panels visible (atlas blit with premultiplied alpha)
   - Layer bitmaps visible (waveform, stem lanes)
   - Overlay UI visible (playhead, perf HUD, dropdowns, text)
   - Output presenter window still works (its own Metal thread, unchanged)
4. No wgpu in the dependency tree (except as transient dep if any other crate pulls it in)

## File Summary

| File | Action |
|------|--------|
| `crates/manifold-gpu/src/metal/surface.rs` | Add `GpuDrawable::gpu_texture()` |
| `crates/manifold-renderer/src/gpu.rs` | Remove wgpu, keep GpuDevice only |
| `crates/manifold-renderer/src/blit.rs` | Delete |
| `crates/manifold-renderer/src/surface.rs` | Delete |
| `crates/manifold-renderer/src/lib.rs` | Remove blit, surface modules |
| `crates/manifold-renderer/Cargo.toml` | Remove wgpu |
| `crates/manifold-app/src/shared_texture.rs` | Remove import_texture wgpu version |
| `crates/manifold-app/src/app.rs` | Replace SurfaceWrapper/GpuContext/BlitPipeline with native types |
| `crates/manifold-app/src/app_render.rs` | Rewrite present_all_windows() |
| `crates/manifold-app/Cargo.toml` | Remove wgpu, wgpu-hal, wgpu-types |
| `Cargo.toml` (workspace) | Remove wgpu workspace deps if unused |
| `docs/NATIVE_METAL_UI_MIGRATION.md` | Mark Phase 6 done |

## Critical Rules

- Read the ENTIRE present_all_windows() before rewriting — understand every render pass
- The overlay command queuing logic (which panels to render, playhead, perf HUD, dropdown, etc.) is UNCHANGED — only the GPU submission changes
- Preserve the render order: clear → video blit → atlas → layer bitmaps → overlays
- The output presenter (NativeOutputPresenter) is UNCHANGED — it runs on its own thread with its own Metal device/queue
- `cargo clippy --workspace -- -D warnings` must pass
- Commit and push when done
