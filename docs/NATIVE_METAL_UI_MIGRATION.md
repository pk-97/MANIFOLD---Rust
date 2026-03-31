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
| BlitPipeline | `manifold-renderer/src/blit.rs` | RenderPipeline, BindGroup, Sampler | **Shared render pass** |
| PanelCompositor | `manifold-renderer/src/panel_compositor.rs` | RenderPipeline, BindGroup, Sampler | **Shared render pass** |
| TonemapBlitPipeline | `manifold-renderer/src/tonemap_blit.rs` | RenderPipeline, Buffer, BindGroup | Dead code (unused) |
| UICacheManager | `manifold-renderer/src/ui_cache_manager.rs` | Texture, CommandEncoder, RenderPass | Clean |
| LayerBitmapGpu | `manifold-renderer/src/layer_bitmap_gpu.rs` | RenderPipeline, Buffer, Texture, BindGroup | Clean |
| UIRenderer | `manifold-renderer/src/ui_renderer.rs` | RenderPipeline, Buffer, BindGroup + **glyphon** | **Glyphon blocker** |
| SharedTextureBridge | `manifold-app/src/shared_texture.rs` | wgpu-hal Metal backend | Clean (already native) |
| app_render | `manifold-app/src/app_render.rs` | CommandEncoder, RenderPass, Surface | Clean |

## Key Constraint: Shared Render Passes

`app_render.rs` creates a **single wgpu render pass** that draws:
1. Clear to black + BlitPipeline (compositor output to video area)
2. PanelCompositor (UI atlas overlay)

Both happen inside one `begin_render_pass()` / `end_render_pass()`. BlitPipeline and PanelCompositor **cannot be converted independently** — they must be converted together with the render loop in Phase 6. This is why the original Phase 2 plan was revised.

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

**Added in Phase 1:**
- `GpuVertexFormat`, `GpuVertexAttribute`, `GpuVertexLayout` — vertex layout types
- `GpuDevice::create_render_pipeline_with_vertex_layout()` — render pipeline with vertex descriptor
- `GpuEncoder::draw_indexed()` — indexed draw with vertex + index buffers
- `GpuSurface` / `GpuDrawable` — CAMetalLayer wrapper for window presentation
- `GpuEncoder::present_drawable()` — schedule drawable present with command buffer

## Phases

### Phase 1: Extend manifold-gpu [IN PROGRESS]

Prompt: `docs/PHASE1_AGENT_PROMPT.md`

Add the missing API surface so the UI thread can use manifold-gpu:
- Vertex layout types + `create_render_pipeline_with_vertex_layout()`
- `draw_indexed()` on GpuEncoder
- `GpuSurface` / `GpuDrawable` — CAMetalLayer wrapper
- Smoke test

### Phase 2: Bridge + dead code cleanup [NOT STARTED]

Prompt: `docs/PHASE2_AGENT_PROMPT.md`

Small setup step — originally planned to convert simple blit modules, but analysis revealed BlitPipeline and PanelCompositor share a wgpu render pass and can't be converted independently. Revised scope:

- [ ] Add `native_device: GpuDevice` field to `GpuContext` (bridge for transition period)
- [ ] Delete `tonemap_blit.rs` (dead code — unused since output presenter rewrite)
- [ ] Update this migration doc

### Phase 3: Convert texture management [NOT STARTED]

Modules that manage their own textures and render passes (don't participate in the shared workspace render pass):

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

### Phase 6: Full render loop conversion [NOT STARTED]

**The big one.** Convert the entire workspace render loop from wgpu to a single GpuEncoder. This is where BlitPipeline, PanelCompositor, and SurfaceWrapper are replaced — they can't be converted earlier because they participate in shared wgpu render passes.

- [ ] Replace `GpuContext` wgpu fields with `GpuDevice` only (remove wgpu Instance/Adapter/Device/Queue)
- [ ] Replace `SurfaceWrapper` with `GpuSurface` for the workspace window
- [ ] Rewrite `present_all_windows()` around a single `GpuEncoder`:
  - One encoder per frame
  - Panel cache render passes (dirty panels → atlas)
  - Clear + blit compositor + UI atlas (replaces BlitPipeline + PanelCompositor)
  - Layer bitmap render passes
  - Overlay render passes (text, playhead, popups)
  - Output presenter render pass (sample IOSurface → output drawable)
  - `commit()` — single GPU submission
- [ ] Output presenter becomes a render pass in the main encoder (not a separate thread)
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
| Shared render pass coupling | Can't convert modules piecemeal | Identified — BlitPipeline/PanelCompositor deferred to Phase 6 |

## Dependencies

```
Phase 1 (manifold-gpu extensions)
    |
    v
Phase 2 (bridge + cleanup) ←── small, fast
    |
    +-- Phase 3 (texture management) ←── parallel
    |
    +-- Phase 4 (UIRenderer rects) ←── parallel
    |
    +-- Phase 5 (CoreText) ←── parallel, independent
    |
    v
Phase 6 (full render loop) ←── needs ALL of 3, 4, 5 complete
    |
    v
Phase 7 (cleanup)
```

Phases 3, 4, 5 can be developed in parallel after Phase 2.

Phase 6 is the integration point — all modules must be ready before the render loop can be rewritten.

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-31 | Go full native Metal for UI thread | Single command buffer eliminates GPU scheduler contention |
| 2026-03-31 | Replace glyphon with CoreText | If going native, go all the way. Native macOS text quality. |
| 2026-03-31 | Surface management in manifold-gpu (not manifold-app) | Clean boundary, cross-platform extensibility |
| 2026-03-31 | New methods only in manifold-gpu (no existing API changes) | Content thread is untouched, zero regression risk |
| 2026-03-31 | Defer BlitPipeline/PanelCompositor to Phase 6 | They share a wgpu render pass — can't convert independently |
| 2026-03-31 | Phase 2 revised to bridge + cleanup only | Original scope (convert blit modules) impossible due to shared render pass coupling |
| 2026-03-31 | Phase 1 with Opus, subsequent phases with Sonnet | Foundation must be correct; mechanical follow-through is well-prompted |
