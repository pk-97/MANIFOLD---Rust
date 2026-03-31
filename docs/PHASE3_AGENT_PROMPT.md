# Phase 3 — Convert LayerBitmapGpu to Native Metal

Read `CLAUDE.md` and `docs/NATIVE_METAL_UI_MIGRATION.md` before starting.

You are converting `LayerBitmapGpu` from wgpu to native Metal via `manifold-gpu`. This module uploads CPU pixel buffers to GPU textures and renders them as positioned quads in the viewport. It does NOT participate in the shared wgpu render pass (BlitPipeline + PanelCompositor) — it has its own independent render pass.

**Breaking is fine.** Layer bitmaps (waveform lane, stem lanes) will stop appearing on screen after this phase. This is intentional — Phase 6 wires the native render target to the surface drawable. Do not add compatibility shims. Remove wgpu cleanly.

Read every file you need to modify BEFORE making changes. Work without breaks. Complete all tasks. Build with `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` after all changes. Fix any issues. Commit and push when done.

## Context

**What Phase 1 added to manifold-gpu:**
- `GpuVertexFormat`, `GpuVertexAttribute`, `GpuVertexLayout` — vertex layout types
- `GpuDevice::create_render_pipeline_with_vertex_layout()` — render pipeline with vertex descriptor
- `GpuEncoder::draw_indexed(pipeline, target, bindings, vertex_buf, index_buf, index_count, viewport, load_action, label)` — opens a NEW render pass per call

**What Phase 2 added:**
- `gpu.native_device: manifold_gpu::GpuDevice` field on `GpuContext` — available now

**Key manifold-gpu API facts:**
- `GpuDevice::create_texture(desc)` — Private storage by default. Add `GpuTextureUsage::CPU_UPLOAD` flag to get Shared storage, which allows `replace_region` CPU writes.
- `GpuDevice::upload_texture(texture, data: &[u8])` — synchronous CPU→GPU via `replace_region`. Requires `CPU_UPLOAD` usage on the texture.
- `GpuDevice::create_buffer_shared(size)` — CPU+GPU coherent buffer. Access mapped pointer via `buffer.mapped_ptr.unwrap()`.
- `GpuBinding::Bytes { binding, data }` — inline bytes via Metal `set_bytes`. Use for small uniforms instead of a buffer. Zero allocation.
- `draw_indexed` opens a **new render pass per call** — not a single pass with multiple draws. Each layer call opens its own render pass on the target texture. Use `GpuLoadAction::Load` to preserve existing content.
- **Slot map key = WGSL `@binding(N)` number only, group is ignored.** Two globals at the same binding number (different groups) overwrite each other in the slot map. Use `@group(0)` only with unique binding numbers throughout.

## Critical: WGSL Shader Must Be Updated

The current `BITMAP_SHADER` uses `@group(1)` bindings:
```wgsl
@group(0) @binding(0) var<uniform> globals: Globals;  // binding 0
@group(1) @binding(0) var t_layer: texture_2d<f32>;   // binding 0 — COLLISION with globals
@group(1) @binding(1) var s_layer: sampler;
```

Both `globals` and `t_layer` have binding number `0`. The slot_map key is the binding number — they collide and only one survives. The shader must use unique binding numbers across all groups:

```wgsl
@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var t_layer: texture_2d<f32>;
@group(0) @binding(2) var s_layer: sampler;
```

The vertex and fragment function logic are unchanged — only the resource declarations change.

After this fix, the slot_map will correctly contain:
- binding 0 → buffer slot 0 (globals)
- binding 1 → texture slot 0 (t_layer)
- binding 2 → sampler slot 0 (s_layer)

## Task 1: Convert layer_bitmap_gpu.rs

**File:** `crates/manifold-renderer/src/layer_bitmap_gpu.rs`

Read the full file first. Then replace the entire wgpu implementation.

### Struct layout

```rust
use manifold_gpu::{
    GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuBuffer, GpuDevice, GpuEncoder,
    GpuFilterMode, GpuLoadAction, GpuRenderPipeline, GpuSampler, GpuSamplerDesc,
    GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    GpuVertexAttribute, GpuVertexFormat, GpuVertexLayout,
};
use manifold_ui::node::{Color32, Rect};

struct LayerTexture {
    texture: GpuTexture,
    width: u32,
    height: u32,
    // No view or bind_group — manifold-gpu uses slot-based bindings
}

pub struct LayerBitmapGpu {
    textures: Vec<Option<LayerTexture>>,
    /// Per-layer shared GpuBuffer for 4 vertices (64 bytes each).
    /// Grown when a new layer index is encountered — no per-frame allocation after warmup.
    vertex_bufs: Vec<Option<GpuBuffer>>,
    pipeline: GpuRenderPipeline,
    sampler: GpuSampler,
    /// Pre-allocated shared index buffer: [0u32, 1, 2, 0, 2, 3] — one quad.
    index_buf: GpuBuffer,
    /// CPU scratch for building vertex data (no GPU allocation).
    vertices: Vec<BitmapVertex>,
}
```

Zero `wgpu::` references in the final file.

### Updated BITMAP_SHADER

Keep the vertex and fragment logic unchanged. Update only the resource declarations:

```wgsl
struct Globals {
    screen_size: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var t_layer: texture_2d<f32>;
@group(0) @binding(2) var s_layer: sampler;
```

### `new(device: &GpuDevice, format: GpuTextureFormat) -> Self`

1. Build the vertex layout for `BitmapVertex` (stride = 16 bytes, two Float32x2 attributes at offsets 0 and 8).

2. Build alpha blend state (same semantics as `wgpu::BlendState::ALPHA_BLENDING`):
   ```rust
   GpuBlendState {
       src_factor: GpuBlendFactor::SrcAlpha,
       dst_factor: GpuBlendFactor::OneMinusSrcAlpha,
       operation: GpuBlendOp::Add,
       src_alpha_factor: GpuBlendFactor::One,
       dst_alpha_factor: GpuBlendFactor::OneMinusSrcAlpha,
       alpha_operation: GpuBlendOp::Add,
   }
   ```

3. Create render pipeline:
   ```rust
   let pipeline = device.create_render_pipeline_with_vertex_layout(
       BITMAP_SHADER, "vs_main", "fs_main", format, Some(blend), vertex_layout,
       "Bitmap Pipeline",
   );
   ```

4. Create nearest-neighbor sampler (matches Unity `FilterMode.Point`):
   ```rust
   let sampler = device.create_sampler(&GpuSamplerDesc {
       min_filter: GpuFilterMode::Nearest,
       mag_filter: GpuFilterMode::Nearest,
       mip_filter: GpuFilterMode::Nearest,
       ..Default::default()
   });
   ```

5. Create pre-allocated index buffer for one quad:
   ```rust
   let index_data: [u32; 6] = [0, 1, 2, 0, 2, 3];
   let index_buf = device.create_buffer_shared(24); // 6 × 4 bytes
   unsafe {
       std::ptr::copy_nonoverlapping(
           index_data.as_ptr(),
           index_buf.mapped_ptr.unwrap() as *mut u32,
           6,
       );
   }
   ```

6. Return `Self { textures: Vec::new(), vertex_bufs: Vec::new(), pipeline, sampler, index_buf, vertices: Vec::with_capacity(64) }`.

### `upload_layer(&mut self, device: &GpuDevice, layer_index: usize, pixels: &[Color32], tex_w: u32, tex_h: u32)`

Drops `queue: &wgpu::Queue`. Uses `device` only.

1. Return early if `tex_w == 0 || tex_h == 0`.
2. Grow both `textures` and `vertex_bufs` to `layer_index + 1` if needed (`resize_with(layer_index + 1, || None)`).
3. Check if texture needs creation or resize:
   ```rust
   let needs_create = match &self.textures[layer_index] {
       Some(lt) => lt.width != tex_w || lt.height != tex_h,
       None => true,
   };
   ```
4. If `needs_create`: create texture with CPU upload flag:
   ```rust
   let texture = device.create_texture(&GpuTextureDesc {
       width: tex_w, height: tex_h, depth: 1,
       format: GpuTextureFormat::Rgba8UnormSrgb,
       dimension: GpuTextureDimension::D2,
       usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
       label: &format!("Layer Bitmap {layer_index}"),
   });
   self.textures[layer_index] = Some(LayerTexture { texture, width: tex_w, height: tex_h });
   // Create or replace the vertex buffer for this layer
   self.vertex_bufs[layer_index] = Some(
       device.create_buffer_shared(std::mem::size_of::<BitmapVertex>() as u64 * 4)
   );
   ```
5. Upload pixels:
   ```rust
   if let Some(lt) = &self.textures[layer_index] {
       let bytes: &[u8] = unsafe {
           std::slice::from_raw_parts(pixels.as_ptr() as *const u8, pixels.len() * 4)
       };
       device.upload_texture(&lt.texture, bytes);
   }
   ```

### `render_layers(&mut self, encoder: &mut GpuEncoder, target: &GpuTexture, screen_w: u32, screen_h: u32, layer_rects: &[(usize, Rect)])`

Drops `device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView`. New signature uses native types only.

1. Return early if `layer_rects.is_empty()`.

2. Compute globals bytes once (used for every layer):
   ```rust
   let globals: [f32; 2] = [screen_w as f32, screen_h as f32];
   let globals_bytes: &[u8] = bytemuck::bytes_of(&globals);
   ```

3. For each `&(layer_idx, rect)` in `layer_rects`:
   - Skip if `layer_idx >= self.textures.len()` or `self.textures[layer_idx].is_none()`.
   - Skip if `rect.width <= 0.0 || rect.height <= 0.0`.
   - Skip if `layer_idx >= self.vertex_bufs.len()` or `self.vertex_bufs[layer_idx].is_none()`.
   - Build 4 vertices and write to the layer's shared vertex buffer:
     ```rust
     let (x0, y0) = (rect.x, rect.y);
     let (x1, y1) = (rect.x + rect.width, rect.y + rect.height);
     let verts = [
         BitmapVertex { position: [x0, y0], uv: [0.0, 0.0] },
         BitmapVertex { position: [x1, y0], uv: [1.0, 0.0] },
         BitmapVertex { position: [x1, y1], uv: [1.0, 1.0] },
         BitmapVertex { position: [x0, y1], uv: [0.0, 1.0] },
     ];
     let vbuf = self.vertex_bufs[layer_idx].as_ref().unwrap();
     unsafe {
         std::ptr::copy_nonoverlapping(
             verts.as_ptr(),
             vbuf.mapped_ptr.unwrap() as *mut BitmapVertex,
             4,
         );
     }
     ```
   - Call `draw_indexed` for this layer:
     ```rust
     let lt = self.textures[layer_idx].as_ref().unwrap();
     encoder.draw_indexed(
         &self.pipeline,
         target,
         &[
             GpuBinding::Bytes { binding: 0, data: globals_bytes },
             GpuBinding::Texture { binding: 1, texture: &lt.texture },
             GpuBinding::Sampler { binding: 2, sampler: &self.sampler },
         ],
         vbuf,
         &self.index_buf,
         6,
         None,
         GpuLoadAction::Load,
         &format!("Bitmap Layer {layer_idx}"),
     );
     ```

### `trim_to_layer_count(&mut self, count: usize)`

Also trim `vertex_bufs`:
```rust
pub fn trim_to_layer_count(&mut self, count: usize) {
    if self.textures.len() > count { self.textures.truncate(count); }
    if self.vertex_bufs.len() > count { self.vertex_bufs.truncate(count); }
}
```

## Task 2: Update app.rs and app_render.rs

### 2a. Add native target field to Application (app.rs)

Read `crates/manifold-app/src/app.rs`. Add to the `Application` struct:
```rust
/// Phase 3-6 transition: intermediate GpuTexture for layer bitmap rendering.
/// Disconnected from the surface until Phase 6 wires it to the GpuDrawable.
#[cfg(target_os = "macos")]
pub(crate) layer_bitmap_native_target: Option<manifold_gpu::GpuTexture>,
```

Initialize to `None` in `Application::new()`.

### 2b. Update all callers in app_render.rs

Read `crates/manifold-app/src/app_render.rs` fully.

**Update `LayerBitmapGpu::new` call site:** Find where `LayerBitmapGpu::new` is called. Check the `target_format` argument — note what wgpu format was used. Replace the call with:
```rust
manifold_renderer::layer_bitmap_gpu::LayerBitmapGpu::new(
    &gpu.native_device,
    manifold_gpu::GpuTextureFormat::Bgra8Unorm,  // or the appropriate format
)
```
Check the existing wgpu surface format at the call site — if it uses `surface.format` or a specific wgpu format, map it to the equivalent `GpuTextureFormat`. For `wgpu::TextureFormat::Bgra8UnormSrgb` or `Bgra8Unorm`, use `GpuTextureFormat::Bgra8Unorm`. For `Rgba8Unorm`, use `GpuTextureFormat::Rgba8Unorm`.

**Update `upload_layer` calls:** Find all calls to `bitmap_gpu.upload_layer(&gpu.device, &gpu.queue, ...)`. Change to:
```rust
bitmap_gpu.upload_layer(&gpu.native_device, layer_idx, pixels, tw as u32, th as u32);
```
There are three call sites (regular layers, waveform lane index 1000, stem lanes index 1001).

**Replace `render_layers` call:** Find the block:
```rust
if !rects.is_empty() {
    bitmap_gpu.render_layers(
        &gpu.device, &gpu.queue, &mut encoder, &surface_view,
        logical_w, logical_h, &rects,
    );
}
```

Replace with the native version:
```rust
if !rects.is_empty() {
    // Phase 3 transition: render into intermediate GpuTexture.
    // Visual output disconnected from surface until Phase 6.
    let needs_create = self.layer_bitmap_native_target
        .as_ref()
        .is_none_or(|t| t.width != surface_w || t.height != surface_h);
    if needs_create {
        self.layer_bitmap_native_target = Some(gpu.native_device.create_texture(
            &manifold_gpu::GpuTextureDesc {
                width: surface_w,
                height: surface_h,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Bgra8Unorm,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                label: "Layer Bitmap Native Target",
            },
        ));
    }
    if let Some(target) = &self.layer_bitmap_native_target {
        let mut native_enc = gpu.native_device.create_encoder("Layer Bitmaps");
        bitmap_gpu.render_layers(&mut native_enc, target, logical_w, logical_h, &rects);
        native_enc.commit();
    }
}
```

Use the same format in both the target creation and the pipeline (`new` call). Keep the formats consistent.

## Task 3: Remove wgpu from layer_bitmap_gpu.rs

After conversion, verify zero `wgpu::` references remain in `layer_bitmap_gpu.rs`. Remove:
- `use wgpu::util::DeviceExt;`
- All `wgpu::` type references
- `wgpu` from `[dependencies]` in `manifold-renderer/Cargo.toml` only if no other file in that crate still uses wgpu. **Do NOT remove wgpu from manifold-renderer if other files (ui_renderer.rs, ui_cache_manager.rs, blit.rs, etc.) still depend on it** — they do, so leave Cargo.toml unchanged.

## Task 4: Update Migration Doc

Update `docs/NATIVE_METAL_UI_MIGRATION.md`:
1. Mark Phase 3 as `[DONE]`
2. Add note: "LayerBitmapGpu renders to `layer_bitmap_native_target` (intermediate GpuTexture). Layer bitmap visuals disconnected from surface until Phase 6."
3. Note the shader fix: "BITMAP_SHADER updated to `@group(0)` throughout — manifold-gpu slot_map keys on binding number only, multi-group shaders cause collisions."
4. Update Phase 4: rename from "UIRenderer — rects" to "CoreText text renderer (new independent module)" — it's more logical to build the text backend first before converting UIRenderer
5. Update Phase 5: "Convert UIRenderer + UICacheManager (depends on CoreText from Phase 4)"

## Task 5: Build and Verify

1. `cargo clippy --workspace -- -D warnings` — must pass
2. `cargo test --workspace` — must pass
3. App launches and runs without crash. Layer bitmaps (waveform lane, stem lane overlays) will not appear — expected.

## File Summary

| File | Action |
|------|--------|
| `crates/manifold-renderer/src/layer_bitmap_gpu.rs` | Full wgpu→manifold-gpu conversion |
| `crates/manifold-app/src/app.rs` | Add `layer_bitmap_native_target` field |
| `crates/manifold-app/src/app_render.rs` | Update new/upload_layer/render_layers call sites |
| `docs/NATIVE_METAL_UI_MIGRATION.md` | Mark Phase 3 done, revise Phase 4/5 sequence |

## Critical Rules

- Do NOT modify manifold-gpu
- Do NOT convert UICacheManager, UIRenderer, BlitPipeline, or PanelCompositor — they stay wgpu
- Do NOT attempt to display `layer_bitmap_native_target` on screen — that is Phase 6
- Remove ALL `wgpu::` imports from `layer_bitmap_gpu.rs`
- Do NOT remove wgpu from Cargo.toml — other files in manifold-renderer still use it
- `cargo clippy --workspace -- -D warnings` must pass
- Commit and push when done
