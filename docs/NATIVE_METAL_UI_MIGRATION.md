# Native Metal UI Migration

Migrate the UI thread from wgpu to native Metal via `manifold-gpu`, achieving a single device/queue/command-buffer architecture. Eliminates GPU scheduler contention between wgpu and the output presenter, enables TBDR-optimal rendering, and removes the wgpu dependency entirely.

## Motivation

The output monitor requires a custom CAMetalLayer at project resolution (wgpu forces drawableSize to window backing pixels). A dedicated presenter thread with its own MTLCommandQueue causes GPU scheduler interference — visible as UI frame drops when the output window is in fullscreen. The only real fix is a single command buffer per frame containing all render passes (UI + output present).

## Target Architecture

```
Single GpuDevice (MTLDevice + MTLCommandQueue)
    |
    v
Single GpuEncoder per frame (MTLCommandBuffer)
    |
    +-- Render Pass: Panel cache (dirty panels → atlas texture)
    +-- Render Pass: Clear + Blit compositor + UI atlas
    +-- Render Pass: Layer bitmaps
    +-- Render Pass: Overlays (playhead, popups, text)
    +-- Render Pass: Output presenter (sample IOSurface → drawable)
    |
    v
commit() — one GPU submission, zero scheduler contention
```

## Current State (wgpu)

| Module | File | wgpu Types Used | Boundary |
|--------|------|----------------|----------|
| GpuContext | `manifold-renderer/src/gpu.rs` | Instance, Adapter, Device, Queue | Clean |
| SurfaceWrapper | `manifold-renderer/src/surface.rs` | Surface, SurfaceTexture, SurfaceConfig | Clean |
| BlitPipeline | `manifold-renderer/src/blit.rs` | RenderPipeline, BindGroup, Sampler | Clean |
| PanelCompositor | `manifold-renderer/src/panel_compositor.rs` | RenderPipeline, BindGroup, Sampler | Clean |
| TonemapBlitPipeline | `manifold-renderer/src/tonemap_blit.rs` | RenderPipeline, Buffer, BindGroup | Clean |
| UICacheManager | `manifold-renderer/src/ui_cache_manager.rs` | Texture, CommandEncoder, RenderPass | Clean |
| LayerBitmapGpu | `manifold-renderer/src/layer_bitmap_gpu.rs` | RenderPipeline, Buffer, Texture, BindGroup | Clean |
| UIRenderer | `manifold-renderer/src/ui_renderer.rs` | RenderPipeline, Buffer, BindGroup + **glyphon** | **Glyphon blocker** |
| SharedTextureBridge | `manifold-app/src/shared_texture.rs` | wgpu-hal Metal backend | Clean (already native) |
| app_render | `manifold-app/src/app_render.rs` | CommandEncoder, RenderPass, Surface | Clean |

## manifold-gpu API Coverage

**Already exists (content thread uses today):**
- `GpuDevice` — device + queue + pipeline creation + binary archive
- `GpuEncoder` — auto-managed encoder state (compute/render/blit)
- `GpuTexture`, `GpuBuffer`, `GpuSampler` — all resource types
- `GpuRenderPipeline` with WGSL→MSL + SlotMap binding
- `TexturePool` — frame-stamped recycling
- `draw_fullscreen()` — fullscreen triangle (blit/tonemap/panel pattern)
- `draw_instanced()` — instanced draw with vertex+fragment bindings
- `GpuBlendState` — configurable blend modes
- `GpuBinding` — Buffer/Texture/Sampler/Bytes bindings via slot map

**Needs to be added (Phase 1):**
- Vertex layout support for render pipelines (`GpuVertexLayout`)
- `draw_indexed()` on GpuEncoder (vertex + index buffer draw)
- `GpuSurface` — CAMetalLayer lifecycle, drawable acquisition, present
- `create_render_pipeline_with_vertex_layout()` on GpuDevice

## Phases

### Phase 1: Extend manifold-gpu [NOT STARTED]

Add the missing API surface so the UI thread can use manifold-gpu.

**1a. Vertex layout support**

New types in `types.rs`:
- `GpuVertexFormat` — Float32x2, Float32x4, Uint32, etc.
- `GpuVertexAttribute` — format, offset, shader_location
- `GpuVertexLayout` — stride, step_mode, attributes

New method on `GpuDevice`:
- `create_render_pipeline_with_vertex_layout(wgsl, vs, fs, format, blend, layout, label)` — builds MTLVertexDescriptor from GpuVertexLayout

Vertex layouts needed by UI:
- UIRenderer: stride 48 — pos(Float32x2) + uv(Float32x2) + color(Float32x4) + params(Float32x4)
- LayerBitmapGpu: stride 32 — pos(Float32x2) + uv(Float32x2) + color(Float32x2) + params(Float32x2)

**1b. draw_indexed() on GpuEncoder**

New method:
```rust
fn draw_indexed(
    &mut self,
    pipeline: &GpuRenderPipeline,
    target: &GpuTexture,
    bindings: &[GpuBinding],
    vertex_buffer: &GpuBuffer,
    index_buffer: &GpuBuffer,
    index_count: u32,
    viewport: Option<(f32, f32, f32, f32)>,  // x, y, w, h
    load_action: GpuLoadAction,
    label: &str,
)
```

Sets bindings on both vertex and fragment stages (like draw_instanced). Supports optional viewport override for sub-region rendering.

**1c. GpuSurface — CAMetalLayer wrapper**

New type:
```rust
pub struct GpuSurface {
    layer_ptr: *mut c_void,  // retained CAMetalLayer
    drawable_width: u32,
    drawable_height: u32,
}
```

Methods:
- `GpuDevice::create_surface(window, width, height, format, vsync) -> GpuSurface`
- `surface.resize(width, height)`
- `surface.next_drawable() -> Option<GpuDrawable>`
- `surface.configure_edr()` — set colorspace + wantsExtendedDynamicRangeContent
- `surface.set_contents_gravity_resize_aspect()` — letterbox
- `surface.set_background_color(r, g, b, a)`

New type:
```rust
pub struct GpuDrawable {
    // wraps CAMetalDrawable
}
```

Methods:
- `drawable.texture() -> &metal::TextureRef` — for use as render target
- `GpuEncoder::present_drawable(drawable)` — schedule present
- Or: `drawable.present()` after encoder commit

**1d. Smoke test**

Minimal test in manifold-gpu that:
- Creates a GpuDevice
- Creates a GpuSurface (headless or with a test window)
- Creates a render pipeline with vertex layout
- Draws an indexed quad to a texture
- Verifies pixel output

### Phase 2: Convert simple blit modules [NOT STARTED]

Replace wgpu with manifold-gpu in the straightforward modules:

- [ ] `BlitPipeline` → use `draw_fullscreen()` (already exists)
- [ ] `PanelCompositor` → use `draw_fullscreen()` with premultiplied alpha blend
- [ ] `TonemapBlitPipeline` → use `draw_fullscreen()` with uniform for tonemap mode
- [ ] `GpuContext` → replaced by `GpuDevice` (already exists)
- [ ] `SurfaceWrapper` → replaced by `GpuSurface` (Phase 1)

Each module is independently testable. Existing WGSL shaders work unchanged — manifold-gpu compiles WGSL→MSL automatically.

### Phase 3: Convert texture management [NOT STARTED]

- [ ] `UICacheManager` — atlas texture + incremental render. Replace wgpu Texture/CommandEncoder with GpuTexture/GpuEncoder. Load/DontCare/Clear actions map directly.
- [ ] `LayerBitmapGpu` — per-layer textures, indexed quad rendering. Uses `draw_indexed()` from Phase 1. Pixel upload via `GpuDevice::upload_texture()`.
- [ ] `SharedTextureBridge` — strip wgpu-hal wrapper. Already native Metal internally — just remove the wgpu import layer and return `GpuTexture` directly.

### Phase 4: UIRenderer — rects [NOT STARTED]

Convert the UIRenderer's rectangle/SDF rendering to manifold-gpu:

- [ ] Replace wgpu RenderPipeline with `create_render_pipeline_with_vertex_layout()`
- [ ] Replace wgpu Buffer (vertex/index/uniform) with GpuBuffer
- [ ] Replace render pass recording with `draw_indexed()`
- [ ] Keep same WGSL shader (SDF rounded rects + borders)

Text rendering stays on glyphon temporarily during this phase.

### Phase 5: CoreText text renderer [NOT STARTED]

Replace glyphon with native macOS text rendering:

**Architecture:**
```
CoreText (CPU)     →  Glyph Atlas (GPU)     →  Metal Render Pass
  CTFont               MTLTexture (RGBA8)       textured quads
  CTLine               pack glyphs              per-glyph UVs
  CTRun                LRU eviction             batch draw_indexed
```

**Components:**
- `FontManager` — load Inter TTFs via CTFontCreateWithGraphicsFont, create CTFont per size/weight
- `TextShaper` — CTLineCreateWithAttributedString → glyph IDs + positions + advances
- `GlyphRasterizer` — CGBitmapContext → rasterize glyphs to RGBA bitmap
- `GlyphAtlas` — pack glyphs into MTLTexture, UV lookup table, LRU eviction
- `TextRenderer` — generate vertex/index buffers from shaped runs, batch draw_indexed
- `TextMeasure` — CTLineGetTypographicBounds for exact measurement

**Font requirements:**
- 3 weights: Inter Regular, Medium, Bold
- Sizes: 7-16px (most at 8-12px)
- Unicode support via system font fallback
- ~200-300 unique glyphs per frame
- Atlas: 512x512 or 1024x1024 RGBA8

**Current glyphon features to replicate:**
- `draw_text(x, y, text, size, color)` — queue text command
- `measure_text_cached(text, size, weight) -> Vec2` — cached measurement
- TextBuffer cache (256 entries, LRU eviction)
- Clip bounds per text command
- Main + Overlay render modes (separate TextRenderer instances)
- Advanced shaping (handled by CoreText)

### Phase 6: app_render.rs integration [NOT STARTED]

Wire everything together:

- [ ] Single `GpuDevice` shared across UI + content (via Arc, or content creates its own)
- [ ] Single `GpuEncoder` per frame in `present_all_windows()`
- [ ] All render passes encoded sequentially into one command buffer
- [ ] Output presenter becomes a render pass (not a separate thread)
- [ ] One `commit()` at the end of the frame
- [ ] Remove wgpu from manifold-app Cargo.toml
- [ ] Remove wgpu from manifold-renderer Cargo.toml

### Phase 7: Cleanup [NOT STARTED]

- [ ] Remove wgpu, wgpu-hal, wgpu-types workspace dependencies
- [ ] Remove glyphon dependency
- [ ] Delete deprecated output_presenter thread code
- [ ] Update CLAUDE.md architecture docs
- [ ] Performance validation — compare frame times before/after

## Risk Register

| Risk | Impact | Mitigation |
|------|--------|------------|
| CoreText glyph quality differs from glyphon | Visual regression | A/B comparison during Phase 5, tune rasterization params |
| Single command buffer too long | Frame time spike | Profile — if >8.3ms, split into 2 submissions |
| WGSL shader incompatibility | Build failure | manifold-gpu already compiles WGSL for content thread — proven path |
| Text measurement differences | Layout shifts | Side-by-side comparison tool, test with all font sizes |
| Missing manifold-gpu feature | Blocked | Each phase identifies needed API surface upfront |

## Dependencies

```
Phase 1 (manifold-gpu extensions)
    |
    +-- Phase 2 (simple blits) ←── can start immediately after Phase 1
    |
    +-- Phase 3 (texture management) ←── can start in parallel with Phase 2
    |
    +-- Phase 4 (UIRenderer rects) ←── needs Phase 1 (draw_indexed)
    |
    +-- Phase 5 (CoreText) ←── independent, can start in parallel with Phase 2-4
    |
    v
Phase 6 (integration) ←── needs ALL of Phases 2-5 complete
    |
    v
Phase 7 (cleanup)
```

Phases 2, 3, 4, 5 can be developed in parallel after Phase 1 completes.

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-31 | Go full native Metal for UI thread | Single command buffer eliminates GPU scheduler contention |
| 2026-03-31 | Replace glyphon with CoreText | If going native, go all the way. Native macOS text quality. |
| 2026-03-31 | Surface management in manifold-gpu (not manifold-app) | Clean boundary, cross-platform extensibility |
| 2026-03-31 | New methods only in manifold-gpu (no existing API changes) | Content thread is untouched, zero regression risk |
