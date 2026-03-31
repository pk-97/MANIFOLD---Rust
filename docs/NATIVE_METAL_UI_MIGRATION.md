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

### Phase 1: Extend manifold-gpu [DONE]

Prompt: `docs/PHASE1_AGENT_PROMPT.md`

Add the missing API surface so the UI thread can use manifold-gpu:
- Vertex layout types + `create_render_pipeline_with_vertex_layout()`
- `draw_indexed()` on GpuEncoder
- `GpuSurface` / `GpuDrawable` — CAMetalLayer wrapper
- Smoke test

### Phase 2: Bridge + dead code cleanup [DONE]

Prompt: `docs/PHASE2_AGENT_PROMPT.md`

Small setup step — originally planned to convert simple blit modules, but analysis revealed BlitPipeline and PanelCompositor share a wgpu render pass and can't be converted independently. Revised scope:

- [x] Add `native_device: GpuDevice` field to `GpuContext` (bridge for transition period)
- [x] Delete `tonemap_blit.rs` (dead code — unused since output presenter rewrite)
- [x] Update this migration doc

### Phase 3: Convert LayerBitmapGpu [DONE]

Prompt: `docs/PHASE3_AGENT_PROMPT.md`

`LayerBitmapGpu` is the only Phase 3 module with a clean enough boundary to convert independently. Analysis revealed:
- `UICacheManager` calls `UIRenderer.draw()` with a wgpu RenderPass — can't convert until UIRenderer is native (Phase 5).
- `SharedTextureBridge` already has `import_texture_native()` — the wgpu `import_texture()` stays until Phase 6 removes wgpu from the UI thread.
- `LayerBitmapGpu` has its own independent render pass and owns its textures — clean conversion target.

**"Breaking is fine"** — layer bitmap visuals (waveform lane, stem lanes) will stop appearing on screen until Phase 6 wires the output to the surface drawable.

Key technical notes:
- BITMAP_SHADER updated to `@group(0)` throughout — manifold-gpu slot_map keys on binding number only, multi-group shaders cause collisions.
- `LayerBitmapGpu` renders to `layer_bitmap_native_target` (intermediate GpuTexture). Layer bitmap visuals disconnected from surface until Phase 6.
- `GpuBuffer::mapped_ptr` is a method (not a field) — call with `()`.

- [x] Convert `LayerBitmapGpu` — textures, pipeline, upload, draw_indexed
- [x] Update callers in app_render.rs — pass `&gpu.native_device`, intermediate GpuTexture target
- [x] Fix BITMAP_SHADER binding groups

### Phase 4: CoreText text renderer [NOT STARTED]

New independent module — zero wgpu, pure manifold-gpu. Unblocks Phase 5 (UIRenderer conversion).

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

### Phase 5: UIRenderer + UICacheManager [NOT STARTED]

Convert UIRenderer's SDF/rect rendering and glyphon text to manifold-gpu, then convert UICacheManager which depends on UIRenderer. These two must be converted together.

- [ ] Replace UIRenderer wgpu RenderPipeline/Buffer with `create_render_pipeline_with_vertex_layout()` + GpuBuffer
- [ ] Replace UIRenderer text rendering (glyphon) with CoreText renderer from Phase 4
- [ ] Replace UICacheManager wgpu Texture/CommandEncoder with GpuTexture/GpuEncoder
- [ ] Atlas becomes GpuTexture — remove PanelCompositor bind group (breaking until Phase 6)

**Breaking**: UI panels stop rendering until Phase 6 wires the atlas GpuTexture into the new render loop.

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
Phase 2 (bridge + cleanup) ←── DONE
    |
    v
Phase 3 (LayerBitmapGpu) ←── independent, breaking ok
    |
    v
Phase 4 (CoreText renderer) ←── independent, new code
    |
    v
Phase 5 (UIRenderer + UICacheManager) ←── needs CoreText from Phase 4
    |
    v
Phase 6 (full render loop) ←── needs ALL of 3, 4, 5 complete
    |
    v
Phase 7 (cleanup)
```

Phases 3 and 4 can be developed in parallel (both independent).

Phase 5 depends on Phase 4 (CoreText). Phase 6 is the integration point.

**Sequencing rationale:** Phases 3, 4, 5 are no longer parallel — analysis revealed UICacheManager can't convert without UIRenderer (wgpu RenderPass coupling), and UIRenderer can't convert without CoreText (text backend). The serial chain 4→5 is necessary. Phase 3 (LayerBitmapGpu) is independent and can run in parallel with 4.

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
| 2026-03-31 | "Breaking is fine" policy adopted | Simplifies phases — no compatibility shims, wgpu removed cleanly, visual regressions accepted until Phase 6 integrates |
| 2026-03-31 | Phase 3 narrowed to LayerBitmapGpu only | UICacheManager blocked by UIRenderer (wgpu RenderPass), SharedTextureBridge already has native path |
| 2026-03-31 | Phase 4 = CoreText (was Phase 5), Phase 5 = UIRenderer+UICacheManager | CoreText must exist before UIRenderer can drop glyphon; UIRenderer and UICacheManager must convert together |
| 2026-03-31 | BITMAP_SHADER must use @group(0) only | manifold-gpu slot_map keys on binding number alone — multi-group shaders cause collisions |
