# Phase 2 — Convert Simple Blit Modules to Native Metal

Read `CLAUDE.md` and `docs/NATIVE_METAL_UI_MIGRATION.md` before starting.

You are converting the UI thread's simple blit/compositor modules from wgpu to native Metal via `manifold-gpu`. Phase 1 (completed) added vertex layout support, `draw_indexed()`, and `GpuSurface` to manifold-gpu. This phase replaces the wgpu implementations of the simple fullscreen-triangle modules with manifold-gpu equivalents.

Read every file you need to modify BEFORE making changes. Read the existing wgpu implementations to understand the exact behavior being replicated.

Work without breaks. Complete all tasks below. Build with `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` after all changes. Fix any issues. Commit and push when done.

## Context

**What Phase 1 added to manifold-gpu:**
- `GpuVertexFormat`, `GpuVertexAttribute`, `GpuVertexLayout` — vertex layout types
- `GpuDevice::create_render_pipeline_with_vertex_layout()` — render pipeline with vertex descriptor
- `GpuEncoder::draw_indexed()` — indexed draw with vertex + index buffers
- `GpuSurface` / `GpuDrawable` — CAMetalLayer wrapper for window presentation
- `GpuEncoder::present_drawable()` — schedule drawable present with command buffer

**What already existed in manifold-gpu:**
- `GpuDevice::create_render_pipeline(wgsl, vs, fs, format, blend, label)` — fullscreen triangle pipeline (no vertex buffer)
- `GpuEncoder::draw_fullscreen(pipeline, target, bindings, clear, store, label)` — fullscreen triangle draw with fragment-only bindings
- `GpuDevice::create_sampler(desc)` — sampler creation
- `GpuDevice::create_buffer_shared(size)` — shared buffer with mapped pointer
- `GpuBinding::Texture`, `GpuBinding::Sampler`, `GpuBinding::Bytes` — resource bindings via SlotMap
- `GpuTextureFormat`, `GpuBlendState`, `GpuBlendFactor`, `GpuBlendOp`, `GpuLoadAction`

**Crate structure:**
- `manifold-renderer` — contains the modules being converted (UI-thread rendering)
- `manifold-gpu` — native Metal backend (do NOT modify in this phase)
- `manifold-app` — application layer, consumes manifold-renderer modules

## Strategy

Each module is a self-contained wgpu pipeline that does a fullscreen triangle blit. The conversion pattern is identical for all:

1. Replace `wgpu::RenderPipeline` with `manifold_gpu::GpuRenderPipeline`
2. Replace `wgpu::Sampler` with `manifold_gpu::GpuSampler`
3. Replace pipeline creation (`device.create_render_pipeline(...)`) with `device.create_render_pipeline(wgsl, vs, fs, format, blend, label)`
4. Replace draw calls with `encoder.draw_fullscreen(pipeline, target, bindings, clear, store, label)`
5. Keep the WGSL shaders unchanged — manifold-gpu compiles WGSL→MSL automatically
6. The public API of each module changes — callers now pass `&GpuDevice` and `&mut GpuEncoder` instead of `&wgpu::Device` and `&mut wgpu::CommandEncoder`

**Important:** These modules are consumed by `manifold-app`. When you change the public API of a module, you MUST update all callers in manifold-app. The code must compile.

## Task 1: Convert BlitPipeline (blit.rs)

**File:** `crates/manifold-renderer/src/blit.rs`

**Current wgpu implementation:** Fullscreen triangle that samples a texture. Used to blit compositor output to the workspace window. Has several methods:
- `new(device, format)` — create pipeline
- `blit(device, encoder, source, target, w, h)` — fullscreen blit with clear
- `blit_to_rect(device, encoder, source, target, x, y, w, h)` — blit to viewport rect (load existing)
- `blit_to_rect_fit(device, encoder, source, target, x, y, w, h, aspect)` — aspect-correct fit
- `prepare_rect_fit(device, source, x, y, w, h, aspect)` — prepare for split draw
- `draw_in_pass(pass)` — issue draw into existing render pass

**WGSL shader** (keep unchanged):
```wgsl
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
```
Fullscreen triangle vertex shader + passthrough fragment shader. Linear sampler. No blend (REPLACE).

**New implementation:**

Replace the struct with:
```rust
pub struct BlitPipeline {
    pipeline: manifold_gpu::GpuRenderPipeline,
    sampler: manifold_gpu::GpuSampler,
}
```

The `prepare_rect_fit` / `draw_in_pass` split pattern exists because the wgpu version needed to create a bind group before the render pass and draw inside an existing pass. With manifold-gpu, `draw_fullscreen()` manages the entire render pass internally. However, the workspace render loop in `app_render.rs` uses the split pattern to draw the blit AND the UI atlas in the SAME render pass (one clear + blit + atlas). This is a critical optimization.

**Approach:** For Phase 2, convert the simple standalone methods (`blit`, `blit_to_rect`, `blit_to_rect_fit`) to use manifold-gpu directly. For the split `prepare_rect_fit` / `draw_in_pass` pattern used by the workspace render loop — these will be addressed in Phase 6 (integration) when the entire render loop is restructured around a single GpuEncoder. For now, keep wgpu versions of `prepare_rect_fit` and `draw_in_pass` alongside the new Metal methods. The struct holds both backends:

```rust
pub struct BlitPipeline {
    // Native Metal path (for standalone blits)
    pipeline: manifold_gpu::GpuRenderPipeline,
    sampler: manifold_gpu::GpuSampler,

    // wgpu path (for split prepare/draw in shared render pass — Phase 6 removes this)
    wgpu_pipeline: wgpu::RenderPipeline,
    wgpu_sampler: wgpu::Sampler,
    wgpu_bind_group_layout: wgpu::BindGroupLayout,
    prepared_bind_group: Option<wgpu::BindGroup>,
    prepared_viewport: Option<(f32, f32, f32, f32)>,
}
```

**Constructor** `new(gpu_device: &manifold_gpu::GpuDevice, wgpu_device: &wgpu::Device, format: GpuTextureFormat, wgpu_format: wgpu::TextureFormat)`:
- Create the manifold-gpu pipeline using `gpu_device.create_render_pipeline(BLIT_SHADER, "vs_main", "fs_main", format, None, "Blit Pipeline")`
- Create the manifold-gpu sampler using `gpu_device.create_sampler(&GpuSamplerDesc { min_filter: Linear, mag_filter: Linear, ..Default::default() })`
- Also create the wgpu pipeline and sampler (same as current code) for the split draw path

**Standalone methods** — new signatures using manifold-gpu:
- `blit_native(&self, encoder: &mut manifold_gpu::GpuEncoder, source: &manifold_gpu::GpuTexture, target: &manifold_gpu::GpuTexture)` — uses `draw_fullscreen` with clear=true, store=true
- `blit_to_rect_native(...)` — not directly possible with `draw_fullscreen` (it doesn't support viewport). For now, skip this — it's only used by the output presenter path which will be restructured in Phase 6.

Actually, looking at this more carefully — the BlitPipeline is heavily entangled with the wgpu render pass model (split prepare/draw, shared passes). **Converting it partially creates more complexity than it saves.**

**Revised approach for BlitPipeline:** SKIP conversion in Phase 2. BlitPipeline will be fully replaced in Phase 6 when the entire render loop moves to a single GpuEncoder. Mark it in the migration doc as "Phase 6".

## Task 1 (Revised): Convert PanelCompositor (panel_compositor.rs)

**File:** `crates/manifold-renderer/src/panel_compositor.rs`

**Current wgpu implementation:** Fullscreen triangle that samples the UI atlas texture with premultiplied alpha blending. Simple module with clean API:
- `new(device, format)` — create pipeline
- `bind_group_layout()` — expose layout for creating bind groups externally
- `sampler()` — expose sampler for external bind group creation
- `draw_atlas(pass, bind_group)` — draw into existing render pass

**Problem:** Same issue as BlitPipeline — `draw_atlas` draws into an existing wgpu render pass. The caller (app_render.rs) creates a single render pass and draws both the blit AND the atlas in it.

**Same conclusion:** SKIP — will be fully replaced in Phase 6 when the render loop is restructured.

## Revised Task 1: Convert TonemapBlitPipeline (tonemap_blit.rs)

**File:** `crates/manifold-renderer/src/tonemap_blit.rs`

**Current wgpu implementation:** Fullscreen triangle with ACES tonemap, controlled by a uniform (mode: 0=passthrough, 1=ACES). Used by the output presenter for SDR displays.

**Current usage:** NOT used by manifold-app (the old wgpu output presenter code was removed). The output_presenter.rs now has its own MSL shader for this. This module is dead code.

**Action:** Verify it's truly unused by grepping for `TonemapBlitPipeline` and `tonemap_blit` across the entire workspace. If unused, delete the file and remove it from `lib.rs`. If still used somewhere, convert it.

## Revised Task 2: Convert GpuContext (gpu.rs)

**File:** `crates/manifold-renderer/src/gpu.rs`

**Current:** Creates wgpu Instance/Adapter/Device/Queue. Used by manifold-app as the central GPU context.

**Problem:** GpuContext is used by BOTH the UI thread (for wgpu rendering) AND passed to content_thread/content_pipeline (for wgpu-hal IOSurface import). Until ALL wgpu consumers are migrated, GpuContext must keep its wgpu fields.

**Action for Phase 2:** Add a `manifold_gpu::GpuDevice` field to GpuContext so both APIs are available during the transition. The GpuDevice should be created from the SAME underlying MTLDevice as wgpu uses.

```rust
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    /// Native Metal device sharing the same MTLDevice as wgpu.
    /// Used by modules migrated to manifold-gpu during the transition.
    #[cfg(target_os = "macos")]
    pub native_device: manifold_gpu::GpuDevice,
}
```

**Creating the native device from wgpu's device:** The wgpu device wraps a Metal device internally. We need to extract it and create a GpuDevice from the same MTLDevice. However, `GpuDevice::new()` creates a fresh device via `Device::system_default()`. On macOS there is only one Metal device (unless the system has discrete + integrated GPUs), so `GpuDevice::new()` will return the same physical device.

**Simpler approach:** Just call `GpuDevice::new()` — on Apple Silicon (single GPU), it's the same device. The command queue is different (which is fine — it's a new queue on the same device).

Update the `GpuContext::new()` method to also create a `GpuDevice`:
```rust
#[cfg(target_os = "macos")]
let native_device = manifold_gpu::GpuDevice::new();
```

Update all constructors of GpuContext in manifold-app to include the new field.

## Revised Task 3: Convert SurfaceWrapper (surface.rs)

**File:** `crates/manifold-renderer/src/surface.rs`

**Current:** Per-window wgpu surface state. Used for the workspace window.

**Problem:** The workspace window still uses wgpu for all UI rendering (UIRenderer, UICacheManager, LayerBitmapGpu). Until those are migrated, the workspace needs a wgpu surface.

**Action for Phase 2:** Add a `GpuSurface` variant alongside the existing wgpu surface. The output window will use `GpuSurface`, the workspace window continues using the wgpu surface.

Actually — the output window already has its own custom CAMetalLayer in `output_presenter.rs`. The workspace window needs wgpu until Phase 6. So there's nothing to convert here yet.

**SKIP** — will be replaced in Phase 6.

## Revised Scope

After careful analysis, **most of the original Phase 2 modules can't be converted independently** because they draw into shared wgpu render passes managed by `app_render.rs`. The conversion needs to happen as a unit in Phase 6.

What CAN be done in Phase 2:

### Task 1: Add GpuDevice to GpuContext

**File:** `crates/manifold-renderer/src/gpu.rs`

Add a `manifold_gpu::GpuDevice` field to `GpuContext` so both wgpu and native Metal APIs are available during the transition period.

1. Read `crates/manifold-renderer/src/gpu.rs`
2. Add `#[cfg(target_os = "macos")] pub native_device: manifold_gpu::GpuDevice` to `GpuContext`
3. In `GpuContext::new()`, add `#[cfg(target_os = "macos")] native_device: manifold_gpu::GpuDevice::new()` to the struct construction
4. Find ALL places in manifold-app where GpuContext is constructed (search for `GpuContext {`) and add the new field

### Task 2: Clean Up Dead Code

1. Grep for `TonemapBlitPipeline` and `tonemap_blit` across the entire workspace
2. If `tonemap_blit.rs` is unused (no imports outside its own module), remove it from `crates/manifold-renderer/src/lib.rs` and delete the file
3. Check for any other dead wgpu code from the old output presenter path

### Task 3: Update Migration Doc

Update `docs/NATIVE_METAL_UI_MIGRATION.md`:

1. Mark Phase 2 as revised — note that BlitPipeline, PanelCompositor, and SurfaceWrapper cannot be converted independently because they participate in shared wgpu render passes
2. Move BlitPipeline, PanelCompositor, and SurfaceWrapper conversion to Phase 6
3. Phase 2 actual scope: add GpuDevice to GpuContext + dead code cleanup
4. Add a note that the Phase 6 integration will convert ALL wgpu rendering in one pass — the shared render pass model means piecemeal conversion isn't practical

### Task 4: Build and Verify

1. `cargo clippy --workspace -- -D warnings` — must pass
2. `cargo test --workspace` — must pass
3. No behavioral changes — the app runs identically, the new `native_device` field is unused for now

## File Summary

| File | Action |
|------|--------|
| `crates/manifold-renderer/src/gpu.rs` | Add `native_device: GpuDevice` field to GpuContext |
| `crates/manifold-renderer/src/tonemap_blit.rs` | Delete if unused |
| `crates/manifold-renderer/src/lib.rs` | Remove tonemap_blit module if deleted |
| `crates/manifold-app/src/app.rs` | Add native_device field to GpuContext construction sites |
| `crates/manifold-app/src/content_thread.rs` | Add native_device field if GpuContext is constructed there |
| `docs/NATIVE_METAL_UI_MIGRATION.md` | Update Phase 2 scope, move blit modules to Phase 6 |

## Critical Rules

- Do NOT modify manifold-gpu — it was completed in Phase 1
- Do NOT change any rendering behavior — the app must run identically
- Do NOT convert modules that draw into shared wgpu render passes — that's Phase 6
- Follow existing code style
- `cargo clippy --workspace -- -D warnings` must pass
- Commit and push when done
