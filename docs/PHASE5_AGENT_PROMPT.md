# Phase 5 — Convert UIRenderer + UICacheManager to Native Metal

Read `CLAUDE.md` and `docs/NATIVE_METAL_UI_MIGRATION.md` before starting.

You are converting UIRenderer and UICacheManager from wgpu to native Metal via `manifold-gpu`, and wiring in the CoreText text renderer (NativeTextRenderer) built in Phase 4 to replace glyphon. These two modules must convert together because UICacheManager calls UIRenderer.draw().

**Breaking is fine.** After this phase, the UI atlas (panels) and overlay UI elements will stop appearing on screen. The video blit (BlitPipeline) still works. Everything visual is restored in Phase 6 when the render loop is rewritten around a single GpuEncoder. Do not add compatibility shims. Remove wgpu cleanly.

Read every file you need to modify BEFORE making changes. Work without breaks. Complete all tasks. Build with `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` after all changes. Fix any issues. Commit and push when done.

## Context

**What Phase 4 built** (`native_text.rs` in manifold-renderer):
```rust
pub struct NativeTextRenderer {
    // CoreText font loading, glyph rasterization, atlas management, draw_indexed rendering
}

impl NativeTextRenderer {
    pub fn new(device: &GpuDevice, format: GpuTextureFormat) -> Self;
    pub fn draw_text(&mut self, x, y, text, font_size, color: [u8; 4], font_weight, clip_bounds: Option<[f32; 4]>);
    pub fn measure_text_cached(&mut self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2;
    pub fn begin_frame(&mut self);
    pub fn clear_commands(&mut self);
    pub fn prepare(&mut self, device: &GpuDevice, viewport_w, viewport_h, offset_x, offset_y, scale_factor: f64) -> bool;
    pub fn render(&self, encoder: &mut GpuEncoder, target: &GpuTexture, load_action: GpuLoadAction);
}

impl TextMeasure for NativeTextRenderer { ... }
```

**What Phase 3 converted:** LayerBitmapGpu — now uses GpuDevice/GpuEncoder/GpuTexture. Pattern to follow.

**What Phase 2 added:** `gpu.native_device: GpuDevice` on GpuContext.

**manifold-gpu API reminders:**
- `GpuEncoder::draw_indexed(pipeline, target, bindings, vbuf, ibuf, count, viewport, load_action, label)` — one render pass per call
- `GpuBinding::Bytes { binding, data }` — inline uniforms (no buffer needed for small data)
- `GpuDevice::create_buffer_shared(size)` — CPU+GPU buffer with mapped pointer
- `GpuDevice::create_encoder(label) -> GpuEncoder` + `encoder.commit()` — one encoder per GPU submission
- **Slot map keys on binding number only** — use `@group(0)` with unique binding numbers

**Current render loop in app_render.rs (relevant phases):**
- **Phase 0**: UICacheManager.render_dirty_panels() — each dirty panel creates its own wgpu encoder + submit
- **Phase 1**: Shared render pass — Clear + BlitPipeline.draw_in_pass() + PanelCompositor.draw_atlas() — stays wgpu (Phase 6)
- **Phase 3**: Overlay — UIRenderer.render_overlay/draw_rect/prepare/draw — uses main wgpu encoder

## Task 1: Convert UIRenderer

**File:** `crates/manifold-renderer/src/ui_renderer.rs`

Read the entire file first. Then rewrite it.

### New struct

```rust
use manifold_gpu::{
    GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuBuffer, GpuDevice,
    GpuEncoder, GpuFilterMode, GpuLoadAction, GpuRenderPipeline, GpuSamplerDesc,
    GpuTexture, GpuTextureFormat, GpuVertexAttribute, GpuVertexFormat, GpuVertexLayout,
};

#[cfg(target_os = "macos")]
use crate::native_text::NativeTextRenderer;

pub struct UIRenderer {
    pipeline: GpuRenderPipeline,

    // Text rendering — Phase 4 CoreText renderer replaces glyphon
    #[cfg(target_os = "macos")]
    text_renderer: NativeTextRenderer,

    // Draw queues (rect commands — text commands go to NativeTextRenderer)
    rect_commands: Vec<RectCommand>,

    // Per-frame vertex/index scratch (CPU side)
    vertices: Vec<UIVertex>,
    indices: Vec<u32>,

    // Pre-allocated shared GpuBuffers (rewritten each prepare, consumed before next overwrite)
    vertex_buf: GpuBuffer,
    index_buf: GpuBuffer,
    vertex_capacity: usize,
    index_capacity: usize,
    prepared_index_count: u32,
    prepared_globals: [f32; 4],  // [viewport_w, viewport_h, offset_x, offset_y]

    // Clip stack for render_tree (mathematical clipping)
    clip_stack: Vec<Rect>,
}
```

Remove ALL:
- `wgpu::` types
- `glyphon::` imports and types (FontSystem, SwashCache, TextAtlas, TextRenderer, TextBuffer, etc.)
- `wgpu::util::DeviceExt`
- `prepared_vertex_buffer: Option<wgpu::Buffer>`, `prepared_index_buffer: Option<wgpu::Buffer>`
- `prepared_globals_bg: Option<wgpu::BindGroup>`
- `globals_buffer`, `globals_bind_group_layout`
- `font_system`, `swash_cache`, `text_cache`, `text_atlas`, `text_renderer`, `overlay_text_renderers`, `viewport`, `text_buffers`
- `text_buffer_cache`, `text_cache_generation`, `text_cache_used`
- `text_commands: Vec<TextCommand>` — text is queued directly in NativeTextRenderer
- `prepared_has_text`, `prepared_text_mode`, `prepared_overlay_idx`

### Keep unchanged

- `UIVertex` struct (position, uv, color, rect_params, border_color) — 64 bytes
- `UI_SHADER` constant — the WGSL SDF shader. Check: it uses only `@group(0) @binding(0)` for globals. This is fine, no collision.
- `RectCommand` struct
- `TextMode` enum — simplify: remove Main/Overlay distinction. With NativeTextRenderer, one instance handles all text. The buffer aliasing concern is handled by commit-before-overwrite (each UICacheManager panel commits its encoder before the next panel's prepare overwrites the buffer). Keep `TextMode` if callers reference it, or remove if unused after conversion.
- All tree traversal methods: `render_tree`, `render_overlay`, `render_overlay_range`, `render_tree_range`
- `draw_node` — but update text parts (see below)
- `draw_rect`, `draw_rounded_rect`, `draw_bordered_rect` — unchanged (queue RectCommand)
- Helper functions: `intersect_rects`, `clamp_rect_to_clip`

### Updated methods

**`new(device: &GpuDevice, format: GpuTextureFormat) -> Self`:**
1. Create GpuRenderPipeline with UIVertex layout:
   - Stride = 64 bytes
   - Attributes: Float32x2 @0 (loc 0), Float32x2 @8 (loc 1), Float32x4 @16 (loc 2), Float32x4 @32 (loc 3), Float32x4 @48 (loc 4)
   - Blend: alpha blending (SrcAlpha/OneMinusSrcAlpha, same as Phase 3's blend state)
   - `create_render_pipeline_with_vertex_layout(UI_SHADER, "vs_main", "fs_main", format, Some(blend), layout, "UI Pipeline")`
2. Create NativeTextRenderer: `NativeTextRenderer::new(device, format)`
3. Pre-allocate shared vertex/index GpuBuffers (initial capacity for ~256 rects: vertex = 256×4×64 = 65536 bytes, index = 256×6×4 = 6144 bytes)

**`draw_text(&mut self, x, y, text, font_size, color: [u8; 4])`:**
Forward to NativeTextRenderer:
```rust
self.text_renderer.draw_text(x, y, text, font_size, color, FontWeight::Medium, None);
```

**`draw_node(&mut self, node: &UINode)`:**
- Background rects: unchanged (push RectCommand)
- Text section: replace `self.text_commands.push(TextCommand { ... })` with `self.text_renderer.draw_text(text_x, text_y, text, font_size, text_color, font_weight, clip_bounds)`
- Replace `self.measure_text_cached(text, size, weight)` with `self.text_renderer.measure_text_cached(text, size, weight)`

**`measure_text_cached(&mut self, text, font_size, font_weight) -> Vec2`:**
Delegate to NativeTextRenderer:
```rust
pub fn measure_text_cached(&mut self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2 {
    self.text_renderer.measure_text_cached(text, font_size, font_weight)
}
```

**`begin_frame(&mut self)`:**
```rust
pub fn begin_frame(&mut self) {
    self.text_renderer.begin_frame();
}
```

**`prepare(&mut self, device: &GpuDevice, width: u32, height: u32, scale_factor: f64) -> bool`:**

Simplified — no more TextMode (NativeTextRenderer handles all text internally).

1. Store globals: `self.prepared_globals = [width as f32, height as f32, 0.0, 0.0];`
2. Build vertex/index data from `rect_commands` (same quad generation as before).
3. Write to shared GpuBuffers:
   - If `vertices.len() * 64 > vertex_capacity`: reallocate larger shared GpuBuffer
   - Copy via mapped pointer: `std::ptr::copy_nonoverlapping`
   - Same for indices
4. Store `prepared_index_count`.
5. Prepare text: `let has_text = self.text_renderer.prepare(device, width, height, 0.0, 0.0, scale_factor);`
6. Clear `rect_commands`.
7. Return `self.prepared_index_count > 0 || has_text`.

**`prepare_with_offset(&mut self, device, viewport_w, viewport_h, offset_x, offset_y, scale_factor) -> bool`:**

Same as `prepare` but with offset:
1. Store globals: `self.prepared_globals = [viewport_w as f32, viewport_h as f32, offset_x, offset_y];`
2. Same vertex/index build + buffer write.
3. Prepare text with offset: `self.text_renderer.prepare(device, viewport_w, viewport_h, offset_x, offset_y, scale_factor);`
4. Clear and return.

Note: remove the `text_mode: TextMode` parameter. It's no longer needed.

**`render(&self, encoder: &mut GpuEncoder, target: &GpuTexture, load_action: GpuLoadAction)`:**

Replaces the old `draw(&self, pass: &mut wgpu::RenderPass)`.

1. Draw rects (if any):
   ```rust
   if self.prepared_index_count > 0 {
       encoder.draw_indexed(
           &self.pipeline,
           target,
           &[GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&self.prepared_globals) }],
           &self.vertex_buf,
           &self.index_buf,
           self.prepared_index_count,
           None,
           load_action,
           "UI Rects",
       );
   }
   ```
2. Draw text (preserving rects):
   ```rust
   self.text_renderer.render(encoder, target, GpuLoadAction::Load);
   ```
   Text always uses `LoadOp::Load` to preserve the rects drawn above.

**Remove the old `render()` convenience method** (the one that creates its own wgpu encoder + render pass). It's replaced by the new `render()` above.

### TextMeasure impl

```rust
impl TextMeasure for UIRenderer {
    fn measure_text(&self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2 {
        // Delegate to NativeTextRenderer's TextMeasure impl (&self version)
        self.text_renderer.measure_text(text, font_size, font_weight)
    }
}
```

## Task 2: Convert UICacheManager

**File:** `crates/manifold-renderer/src/ui_cache_manager.rs`

Read the entire file first.

### New struct

```rust
use manifold_gpu::{GpuDevice, GpuEncoder, GpuLoadAction, GpuTexture, GpuTextureDesc,
    GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

use crate::ui_renderer::UIRenderer;

pub struct UICacheManager {
    // Atlas texture (full logical screen size × scale_factor)
    atlas_texture: Option<GpuTexture>,
    atlas_physical_w: u32,
    atlas_physical_h: u32,
    atlas_logical_w: u32,
    atlas_logical_h: u32,

    // Per-panel valid flags
    panel_valid: [bool; PANEL_SLOT_COUNT],
    needs_clear: bool,

    format: GpuTextureFormat,
    scale_factor: f64,
}
```

Remove ALL:
- `wgpu::Texture`, `wgpu::TextureView`, `wgpu::BindGroup` fields
- `atlas_view`, `atlas_bind_group`
- All `wgpu::` imports

### Updated methods

**`new(format: GpuTextureFormat, scale_factor: f64) -> Self`:**
Same as before but with GpuTextureFormat. No atlas_view/bind_group fields.

**`ensure_atlas(&mut self, device: &GpuDevice, logical_w: u32, logical_h: u32)`:**
- Remove `compositor: &PanelCompositor` parameter — no more bind group creation
- Create GpuTexture:
  ```rust
  let texture = device.create_texture(&GpuTextureDesc {
      width: w, height: h, depth: 1,
      format: self.format,
      dimension: GpuTextureDimension::D2,
      usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
      label: "UI Atlas",
  });
  self.atlas_texture = Some(texture);
  ```

**`atlas_texture(&self) -> Option<&GpuTexture>`:**
New method replacing `atlas_bind_group()`. Returns the atlas GpuTexture for Phase 6 to use.

**Remove `atlas_bind_group()`** — no more wgpu BindGroup.

**`render_dirty_panels(&mut self, device: &GpuDevice, ui_renderer: &mut UIRenderer, tree: &UITree, panels: &[PanelCacheInfo]) -> usize`:**

Remove `queue: &wgpu::Queue` parameter.

1. Guard: return 0 if no atlas_texture.
2. Clear atlas if `needs_clear`:
   ```rust
   if self.needs_clear {
       let mut enc = device.create_encoder("Atlas Clear");
       enc.clear_texture(atlas_texture, 0.0, 0.0, 0.0, 0.0);
       enc.commit();
       self.needs_clear = false;
   }
   ```
3. For each panel in `panels`:
   - Skip if panel is valid and no dirty nodes.
   - Sub-region incremental path: same logic, but use NativeTextRenderer via UIRenderer.
   - Full panel render: same logic.
   - Call `self.prepare_and_draw(device, atlas_texture, ui_renderer)`.

**`prepare_and_draw(&self, device: &GpuDevice, atlas_texture: &GpuTexture, ui_renderer: &mut UIRenderer) -> bool`:**

1. Call `ui_renderer.prepare_with_offset(device, ...)` with atlas logical size, offset (0,0), scale_factor.
   - Note: `prepare_with_offset` no longer takes `queue` — just `device`.
2. If no content, return false.
3. Create encoder, render, commit:
   ```rust
   let mut enc = device.create_encoder("Panel Cache");
   ui_renderer.render(&mut enc, atlas_texture, GpuLoadAction::Load);
   enc.commit();
   ```
4. Return true.

Each dirty panel gets its own encoder+commit. This ensures the shared vertex buffer data is consumed before the next panel's prepare() overwrites it.

## Task 3: Update PanelCompositor Usage

**File:** `crates/manifold-app/src/app_render.rs`

PanelCompositor.draw_atlas() currently blits the atlas onto the surface in the shared render pass. Since the atlas is now a GpuTexture (not a wgpu BindGroup), PanelCompositor can no longer read it.

**Action**: Remove the PanelCompositor.draw_atlas() call from the shared render pass. The atlas won't appear on screen. This is expected — Phase 6 replaces the entire shared render pass.

In app_render.rs, find:
```rust
let atlas_bg = self.ui_cache_manager.as_ref()
    .and_then(|cm| cm.atlas_bind_group());
```
Remove this.

Find:
```rust
if let (Some(pc), Some(bg)) = (&self.panel_compositor, atlas_bg) {
    pc.draw_atlas(&mut pass, bg);
}
```
Remove this block.

**Optionally**: delete `panel_compositor.rs` entirely and remove the `panel_compositor` field from Application. Or leave it as dead code for Phase 6 to clean up. If deleting, also remove from `lib.rs`.

## Task 4: Update app_render.rs Callers

### 4a. Phase 0 — Panel cache update

Find:
```rust
if let (Some(cm), Some(pc), Some(ui)) = (
    &mut self.ui_cache_manager,
    &self.panel_compositor,
    &mut self.ui_renderer,
) {
    cm.set_scale_factor(scale);
    cm.ensure_atlas(&gpu.device, pc, logical_w, logical_h);
    cm.render_dirty_panels(
        &gpu.device, &gpu.queue, ui, &self.ui_root.tree, &panel_infos,
    );
```

Replace with:
```rust
if let (Some(cm), Some(ui)) = (
    &mut self.ui_cache_manager,
    &mut self.ui_renderer,
) {
    cm.set_scale_factor(scale);
    cm.ensure_atlas(&gpu.native_device, logical_w, logical_h);
    cm.render_dirty_panels(
        &gpu.native_device, ui, &self.ui_root.tree, &panel_infos,
    );
    self.ui_root.tree.clear_dirty();
}
```

### 4b. Phase 3 — Overlay UI

Find the overlay block (~line 1054-1149) that:
1. Queues overlay commands via `ui.render_overlay_range(...)`, `ui.draw_rect(...)`, etc.
2. Calls `ui.prepare(&gpu.device, &gpu.queue, logical_w, logical_h, scale, TextMode::Overlay)`
3. Creates a wgpu render pass on the surface
4. Calls `ui.draw(&mut pass)`

Replace the prepare+draw section with native Metal rendering into an intermediate GpuTexture:

```rust
// Flush all overlay commands via native Metal
if ui.prepare(&gpu.native_device, logical_w, logical_h, scale) {
    // Phase 5 transition: render to intermediate GpuTexture.
    // Disconnected from surface until Phase 6.
    let needs_create = self.overlay_native_target
        .as_ref()
        .is_none_or(|t| t.width != surface_w || t.height != surface_h);
    if needs_create {
        self.overlay_native_target = Some(gpu.native_device.create_texture(
            &manifold_gpu::GpuTextureDesc {
                width: surface_w, height: surface_h, depth: 1,
                format: manifold_gpu::GpuTextureFormat::Bgra8Unorm,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                label: "Overlay Native Target",
            },
        ));
    }
    if let Some(target) = &self.overlay_native_target {
        let mut native_enc = gpu.native_device.create_encoder("Overlay UI");
        ui.render(&mut native_enc, target, manifold_gpu::GpuLoadAction::Load);
        native_enc.commit();
    }
}
```

Remove the wgpu render pass creation and `ui.draw(&mut pass)` call.

### 4c. Update UIRenderer begin_frame call

The `ui.begin_frame()` call stays (it calls NativeTextRenderer.begin_frame()).

## Task 5: Update app.rs

### 5a. Add overlay target field

Add to Application struct:
```rust
#[cfg(target_os = "macos")]
pub(crate) overlay_native_target: Option<manifold_gpu::GpuTexture>,
```
Initialize to `None`.

### 5b. Update UIRenderer construction

Find where UIRenderer is created:
```rust
self.ui_renderer = Some(UIRenderer::new(&device, &queue, format));
```

Replace with:
```rust
self.ui_renderer = Some(UIRenderer::new(&gpu.native_device, manifold_gpu::GpuTextureFormat::Bgra8Unorm));
```

Map the wgpu surface format to the equivalent GpuTextureFormat. Typically `Bgra8Unorm` — check what `format` was at the call site.

### 5c. Update UICacheManager construction

Find:
```rust
self.ui_cache_manager = Some(
    manifold_renderer::ui_cache_manager::UICacheManager::new(format, scale),
);
```

Replace with:
```rust
self.ui_cache_manager = Some(
    manifold_renderer::ui_cache_manager::UICacheManager::new(
        manifold_gpu::GpuTextureFormat::Bgra8Unorm, scale,
    ),
);
```

### 5d. Remove PanelCompositor construction (if deleting panel_compositor.rs)

Remove:
```rust
self.panel_compositor = Some(
    manifold_renderer::panel_compositor::PanelCompositor::new(&device, format),
);
```

And remove the `panel_compositor` field from Application struct if deleting the module.

## Task 6: Remove glyphon from manifold-renderer

**File:** `crates/manifold-renderer/Cargo.toml`

Remove: `glyphon = "0.10"`

Verify no other file in manifold-renderer imports glyphon. After converting UIRenderer, it should be the only consumer.

## Task 7: Clean up panel_compositor (optional but preferred)

If no compile errors result from removing it:
1. Delete `crates/manifold-renderer/src/panel_compositor.rs`
2. Remove `pub mod panel_compositor;` from `lib.rs`
3. Remove `panel_compositor` field from Application struct in app.rs
4. Remove all references in app_render.rs

If removing causes cascading issues beyond what's described, leave it as dead code.

## Task 8: Update Migration Doc

Update `docs/NATIVE_METAL_UI_MIGRATION.md`:
1. Mark Phase 5 as `[DONE]`
2. Note: "UIRenderer converted to GpuRenderPipeline + GpuBuffer. glyphon replaced with NativeTextRenderer (CoreText). UICacheManager atlas is now GpuTexture. PanelCompositor removed — atlas not blitted to surface until Phase 6."
3. Note what Phase 6 still needs to do: replace BlitPipeline, SurfaceWrapper, wire atlas + overlay + layer bitmaps into the single GpuEncoder render loop.

## Task 9: Build and Verify

1. `cargo clippy --workspace -- -D warnings` — must pass
2. `cargo test --workspace` — must pass
3. App launches without crash. Video blit still works. UI panels and overlay text will NOT appear — expected.

## File Summary

| File | Action |
|------|--------|
| `crates/manifold-renderer/src/ui_renderer.rs` | Full wgpu→manifold-gpu conversion, replace glyphon with NativeTextRenderer |
| `crates/manifold-renderer/src/ui_cache_manager.rs` | Full wgpu→manifold-gpu conversion, remove PanelCompositor bind group |
| `crates/manifold-renderer/src/panel_compositor.rs` | Delete (or leave as dead code) |
| `crates/manifold-renderer/src/lib.rs` | Remove panel_compositor module |
| `crates/manifold-renderer/Cargo.toml` | Remove glyphon dependency |
| `crates/manifold-app/src/app.rs` | Add overlay_native_target, update constructors, remove panel_compositor |
| `crates/manifold-app/src/app_render.rs` | Update Phase 0 + Phase 3 callers, remove PanelCompositor draw_atlas |
| `docs/NATIVE_METAL_UI_MIGRATION.md` | Mark Phase 5 done |

## Critical Rules

- Do NOT modify manifold-gpu
- Do NOT modify native_text.rs (Phase 4's output) — only consume its public API
- Do NOT convert BlitPipeline or SurfaceWrapper — they stay wgpu (Phase 6)
- Do NOT try to display the overlay or atlas on the surface — Phase 6
- Remove ALL wgpu and glyphon imports from ui_renderer.rs
- Remove ALL wgpu imports from ui_cache_manager.rs
- `@group(0)` only with unique binding numbers in WGSL shaders
- `cargo clippy --workspace -- -D warnings` must pass
- Commit and push when done
