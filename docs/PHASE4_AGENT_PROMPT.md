# Phase 4 — CoreText Native Text Renderer

Read `CLAUDE.md` and `docs/NATIVE_METAL_UI_MIGRATION.md` before starting.

You are building a native macOS text renderer to replace glyphon. This is new code — no existing wgpu module to convert. The module uses CoreText for text shaping, Core Graphics for glyph rasterization, and manifold-gpu for the GPU atlas texture and draw calls. Zero wgpu.

The renderer will be wired into UIRenderer in Phase 5. In this phase, build it as an independent module with its own public API and verify it compiles, measures text correctly, and has a working glyph atlas.

Read every file referenced below BEFORE writing code. Work without breaks. Complete all tasks. Build with `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` after all changes. Fix any issues. Commit and push when done.

## Context

**What this replaces:** `glyphon` text rendering in `crates/manifold-renderer/src/ui_renderer.rs`. Currently UIRenderer uses glyphon (FontSystem, SwashCache, TextAtlas, TextRenderer, TextBuffer) for all text. Phase 5 will replace those with this module.

**What the current text system does** (glyphon in UIRenderer):
- Loads 3 Inter fonts: Regular, Medium, Bold from `crates/manifold-renderer/assets/fonts/`
- Font sizes: 7-16px (most at 8-12px)
- `measure_text_cached(text, font_size, font_weight) -> Vec2` — cached measurement, LRU eviction every 60 frames
- `draw_text(x, y, text, font_size, color)` — queues a TextCommand
- During `prepare()`: shapes text into TextBuffers, submits to glyphon's TextRenderer
- During `draw()`: glyphon renders text into the wgpu render pass
- TextBuffer cache: keyed by `(text_content, font_size, font_weight)`, ~256 entries, LRU eviction after 120 frames unused
- Clip bounds per text command
- Two render mode pools: Main (base UI) + Overlay (dropdowns/popups)

**Bundled fonts:**
```
crates/manifold-renderer/assets/fonts/Inter-Regular.ttf
crates/manifold-renderer/assets/fonts/Inter-Medium.ttf
crates/manifold-renderer/assets/fonts/Inter-Bold.ttf
```

**manifold-gpu API you'll use:**
- `GpuDevice::create_texture(desc)` — with `CPU_UPLOAD` for atlas updates
- `GpuDevice::upload_texture(texture, data)` — synchronous `replace_region`
- `GpuDevice::create_render_pipeline_with_vertex_layout(wgsl, vs, fs, format, blend, layout, label)`
- `GpuDevice::create_buffer_shared(size)` — CPU+GPU buffer with mapped pointer
- `GpuDevice::create_sampler(desc)` — sampler creation
- `GpuEncoder::draw_indexed(pipeline, target, bindings, vbuf, ibuf, count, viewport, load, label)`
- `GpuBinding::Bytes { binding, data }` — inline uniforms
- `GpuBinding::Texture { binding, texture }`, `GpuBinding::Sampler { binding, sampler }`

**Important manifold-gpu constraint:** WGSL shaders must use `@group(0)` only with unique binding numbers. The slot_map keys on binding number alone — multi-group shaders cause collisions.

**TextMeasure trait** (from `manifold-ui/src/text.rs`):
```rust
pub trait TextMeasure {
    fn measure_text(&self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2;
}
```

**FontWeight** (from `manifold-ui/src/node.rs`):
```rust
pub enum FontWeight { Regular, Medium, Bold }
```

## Architecture

```
┌─────────────────────────────────────────────────────┐
│              NativeTextRenderer (public API)         │
│                                                     │
│  draw_text(x, y, text, size, color, weight, clip)   │
│  measure_text(text, size, weight) → Vec2             │
│  prepare(device, vp_w, vp_h, offset, scale) → bool  │
│  render(encoder, target, load_action)                │
│  clear_commands()                                    │
│  begin_frame()                                       │
└─────────┬─────────────────┬────────────────┬────────┘
          │                 │                │
    ┌─────▼──────┐   ┌─────▼──────┐   ┌────▼────────┐
    │ FontManager │   │ GlyphAtlas │   │ TextShaper  │
    │             │   │            │   │             │
    │ CTFont per  │   │ GpuTexture │   │ CTLine →    │
    │ size+weight │   │ shelf pack │   │ glyphs +    │
    │ Inter TTFs  │   │ UV lookup  │   │ positions   │
    │ CGFont ×3   │   │ LRU evict  │   │ measurement │
    └─────────────┘   └────────────┘   └─────────────┘
```

## Task 1: Add Dependencies

**File:** `crates/manifold-renderer/Cargo.toml`

Add under `[target.'cfg(target_os = "macos")'.dependencies]`:
```toml
core-text = "20"
core-graphics = "0.24"
core-foundation = "0.10"
```

Check version compatibility: the project already uses `core-foundation` in manifold-app (via SharedTextureBridge). Use compatible versions. If the workspace already pins a `core-foundation` version, match it. Run `cargo check` after adding to verify no version conflicts.

## Task 2: Create native_text.rs

**File:** `crates/manifold-renderer/src/native_text.rs`

Add `pub mod native_text;` to `crates/manifold-renderer/src/lib.rs` (gated behind `#[cfg(target_os = "macos")]`).

### Overview

Single file, ~600-800 lines. Components:

1. **GlyphKey** — cache key for atlas lookup
2. **GlyphInfo** — cached glyph metrics + atlas UV
3. **FontManager** — loads Inter fonts, creates CTFont per size/weight
4. **GlyphAtlas** — shelf-packed GpuTexture with UV lookup
5. **TextCommand** — queued text draw command
6. **NativeTextRenderer** — public API

### Detailed Design

#### Constants

```rust
/// Atlas texture dimensions.
const ATLAS_SIZE: u32 = 1024;
/// Padding between glyphs in atlas (prevents bleed during sampling).
const GLYPH_PADDING: u32 = 1;
/// Max cached measurement entries before eviction.
const MAX_MEASURE_CACHE: usize = 512;
/// Frames before an unused measurement cache entry is evicted.
const MEASURE_EVICT_FRAMES: u64 = 120;
```

#### GlyphKey

```rust
/// Unique identifier for a cached glyph in the atlas.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    glyph_id: u16,
    /// Font size × 10 for 0.1px precision (e.g. 12.0px → 120).
    size_x10: u16,
    weight: FontWeight,
}
```

#### GlyphInfo

```rust
/// Cached glyph data: metrics + atlas position.
struct GlyphInfo {
    /// UV coordinates in atlas (0.0..1.0).
    uv_x: f32,
    uv_y: f32,
    uv_w: f32,
    uv_h: f32,
    /// Pixel dimensions of the rasterized glyph.
    pixel_w: u32,
    pixel_h: u32,
    /// Bearing: offset from the glyph origin to the top-left of the bitmap.
    bearing_x: f32,
    bearing_y: f32,
}
```

#### FontManager

Loads Inter TTFs at startup. Creates CTFont instances on demand (cached by size+weight).

```rust
struct FontManager {
    /// Base CGFont for each weight (loaded once from TTF data).
    regular_font: CGFont,
    medium_font: CGFont,
    bold_font: CGFont,
    /// CTFont cache: (size_x10, weight) → CTFont.
    /// CTFont is immutable and thread-safe, so caching is straightforward.
    ct_font_cache: ahash::AHashMap<(u16, FontWeight), CTFont>,
}
```

**Font loading** in `FontManager::new()`:
1. Load each TTF via `include_bytes!("../assets/fonts/Inter-Regular.ttf")` etc.
2. Create `CGDataProvider` from the bytes
3. Create `CGFont` from the data provider via `CGFont::from_data_provider`
4. Store the three CGFonts

**`get_ct_font(&mut self, size: f32, weight: FontWeight) -> &CTFont`:**
1. Compute `size_x10 = (size * 10.0).round() as u16`
2. Check cache for `(size_x10, weight)`
3. If miss: create `CTFont::new_with_graphics_font(&cg_font, size, None, None)` using the appropriate CGFont for the weight
4. Cache and return

#### GlyphAtlas

Shelf-packing algorithm: glyphs are placed left-to-right in rows ("shelves"). When the current row is full, start a new row below. When the atlas is full, evict oldest glyphs.

```rust
struct GlyphAtlas {
    texture: GpuTexture,
    /// All cached glyphs.
    glyphs: ahash::AHashMap<GlyphKey, GlyphInfo>,
    /// Staging CPU buffer for the atlas (written to, then uploaded to GPU).
    pixels: Vec<u8>,
    /// Current shelf: (y_position, row_height, next_x).
    shelf_y: u32,
    shelf_height: u32,
    shelf_x: u32,
    /// True if any new glyphs were rasterized this frame (needs GPU upload).
    dirty: bool,
}
```

**`rasterize_glyph(&mut self, font_mgr: &mut FontManager, key: GlyphKey) -> &GlyphInfo`:**
1. If `self.glyphs.contains_key(&key)`, return cached.
2. Get CTFont from FontManager for the key's size/weight.
3. Get glyph bounding rect: `ct_font.get_bounding_rects_for_glyphs(orientation, &[glyph_id])` — returns `CGRect` with origin + size.
4. Compute pixel dimensions: `ceil(rect.size.width) + 2*GLYPH_PADDING`, same for height.
5. Create a `CGBitmapContext` at glyph dimensions with `CGColorSpace::create_device_gray()` and alpha-only or grayscale format:
   - Width × Height grayscale bitmap (1 byte per pixel)
   - Use `CGImageAlphaInfo::None` with grayscale color space for a single-channel output
   - OR use RGBA and extract just the alpha channel
   - **Simplest approach**: create RGBA context, draw white text on transparent, extract alpha channel
6. Set text drawing properties: `set_allows_font_smoothing(false)`, `set_should_smooth_fonts(false)` for clean alpha-only output.
7. Draw the glyph: use `CTFont::draw_glyphs(ct_font, &[glyph_id], &[position], context)` or create a single-glyph CTLine and `CTLineDraw()`.
   - If `CTFont::draw_glyphs` isn't available in the Rust bindings, use:
     ```rust
     let attr_string = /* single-glyph attributed string */;
     let line = CTLine::new_with_attributed_string(&attr_string);
     CGContext::set_text_position(&ctx, -bearing_x + GLYPH_PADDING, descent + GLYPH_PADDING);
     line.draw(&ctx); // or CTLineDraw
     ```
8. Extract pixel data from the CGBitmapContext.
9. Pack into atlas:
   - Check if current shelf has space: `shelf_x + pixel_w <= ATLAS_SIZE`
   - If not, start new shelf: `shelf_y += shelf_height + GLYPH_PADDING; shelf_x = 0; shelf_height = 0`
   - If `shelf_y + pixel_h > ATLAS_SIZE`, atlas is full — call `evict()` or just clear and start over (simple v1)
   - Copy glyph pixels into `self.pixels` at the correct atlas offset
   - Update `shelf_x`, `shelf_height`
10. Create GlyphInfo with UV coordinates and metrics. Insert into `self.glyphs`.
11. Set `self.dirty = true`.

**`upload_if_dirty(&mut self, device: &GpuDevice)`:**
- If `self.dirty`: `device.upload_texture(&self.texture, &self.pixels)`
- Set `self.dirty = false`

**Simple v1 eviction**: when atlas is full, clear everything (nuke cache). Glyphs will be re-rasterized on demand. For ~200-300 glyphs per frame at 8-12px, a 1024×1024 atlas won't fill up in normal use. Full eviction is fine as a rare fallback.

#### TextCommand (internal)

```rust
struct TextCommand {
    x: f32,
    y: f32,
    text: String,
    font_size: f32,
    color: [u8; 4],
    font_weight: FontWeight,
    clip_bounds: Option<[f32; 4]>,
}
```

#### Text Vertex

```rust
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TextVertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}
```
Stride = 32 bytes. Attributes: Float32x2 @ 0 (loc 0), Float32x2 @ 8 (loc 1), Float32x4 @ 16 (loc 2).

#### WGSL Shader

```wgsl
struct Globals {
    viewport_size: vec2<f32>,
    offset: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var t_atlas: texture_2d<f32>;
@group(0) @binding(2) var s_atlas: sampler;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let ndc_x = ((in.position.x - globals.offset.x) / globals.viewport_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - ((in.position.y - globals.offset.y) / globals.viewport_size.y) * 2.0;
    out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let alpha = textureSample(t_atlas, s_atlas, in.uv).r;
    if alpha < 0.004 {
        discard;
    }
    return vec4<f32>(in.color.rgb, in.color.a * alpha);
}
```

Note: atlas format is `R8Unorm` — `textureSample` returns `(r, 0, 0, 1)`. We use `.r` as the glyph coverage alpha.

Blend state (standard alpha blending, same as UIRenderer):
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

#### NativeTextRenderer (public API)

```rust
pub struct NativeTextRenderer {
    font_manager: FontManager,
    atlas: GlyphAtlas,
    pipeline: GpuRenderPipeline,
    sampler: GpuSampler,

    // Draw queue
    commands: Vec<TextCommand>,

    // Prepared state (survives between prepare and render)
    vertex_buf: GpuBuffer,
    index_buf: GpuBuffer,
    prepared_index_count: u32,
    vertex_capacity: usize,
    index_capacity: usize,

    // CPU scratch
    vertices: Vec<TextVertex>,
    indices: Vec<u32>,

    // Measurement cache
    measure_cache: ahash::AHashMap<(String, u16, FontWeight), Vec2>,
    measure_used: ahash::AHashMap<(String, u16, FontWeight), u64>,
    frame_generation: u64,
}
```

**`new(device: &GpuDevice, format: GpuTextureFormat) -> Self`:**
1. Create FontManager (loads Inter fonts).
2. Create GlyphAtlas (1024×1024 R8Unorm GpuTexture with `SHADER_READ | CPU_UPLOAD`).
3. Create render pipeline with TextVertex layout.
4. Create linear sampler (glyphs look better with bilinear filtering).
5. Pre-allocate vertex/index GpuBuffers (shared, reasonable initial capacity — e.g. 4096 vertices × 32 bytes, 6144 indices × 4 bytes).
6. Initialize empty caches.

**`draw_text(&mut self, x, y, text, font_size, color: [u8; 4], font_weight, clip_bounds: Option<[f32; 4]>)`:**
Queue a TextCommand. Same signature as UIRenderer's text API.

**`measure_text_cached(&mut self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2`:**
1. Check `measure_cache` for `(text, font_size, font_weight)`.
2. If hit: update `measure_used`, return cached value.
3. If miss: create CTFont, create CFAttributedString, create CTLine, call `CTLine::get_typographic_bounds()` → `(width, ascent, descent, leading)`.
4. Result: `Vec2::new(width as f32, (ascent + descent) as f32)`. Ensure height is at least `font_size` (matches glyphon behavior).
5. Cache and return.

**`begin_frame(&mut self)`:**
Increment `frame_generation`. Evict stale measurement cache entries (unused for >120 frames). Same pattern as UIRenderer.

**`clear_commands(&mut self)`:**
Clear `self.commands`.

**`prepare(&mut self, device: &GpuDevice, viewport_w: u32, viewport_h: u32, offset_x: f32, offset_y: f32, scale_factor: f64) -> bool`:**

This is the heavy method. Must be called after queuing text commands and before `render()`.

1. Return false if `commands.is_empty()`.
2. For each TextCommand:
   a. Create CTFont for the command's size/weight (via FontManager).
   b. Create attributed string → CTLine.
   c. Iterate CTLine's glyph runs (CTRun):
      - For each glyph in the run: `GlyphKey { glyph_id, size_x10, weight }`
      - Ensure glyph is in atlas (rasterize if not).
      - Look up GlyphInfo for UV coordinates.
      - Compute screen position: `(cmd.x + run_position.x + bearing_x, cmd.y + run_position.y - bearing_y)` × scale_factor.
      - Apply clip bounds: skip glyph if entirely outside clip rect.
      - Build 4 TextVertex values (quad) and 6 indices.
3. Upload atlas if dirty.
4. Write vertex/index data to GpuBuffers:
   - If data exceeds current capacity, reallocate (create new larger shared GpuBuffer).
   - Copy via mapped pointer.
5. Store `prepared_index_count`.
6. Clear `self.commands`.
7. Return true.

**`render(&self, encoder: &mut GpuEncoder, target: &GpuTexture, load_action: GpuLoadAction)`:**
1. If `prepared_index_count == 0`, return.
2. Call `encoder.draw_indexed(pipeline, target, bindings, vertex_buf, index_buf, count, None, load_action, "Text")`.
3. Bindings:
   - `GpuBinding::Bytes { binding: 0, data: globals_bytes }` — viewport + offset
   - `GpuBinding::Texture { binding: 1, texture: &atlas.texture }`
   - `GpuBinding::Sampler { binding: 2, sampler: &self.sampler }`

Wait — `render()` needs the viewport/offset data. Store it from `prepare()`:
```rust
prepared_globals: [f32; 4],  // [viewport_w, viewport_h, offset_x, offset_y]
```

**Note on scale_factor:** Glyph rasterization should happen at the PHYSICAL pixel size (font_size × scale_factor) for crisp rendering. Vertex positions are in LOGICAL pixels (pre-offset, pre-NDC-transform). The NDC transform in the shader handles the mapping. When computing glyph quads, positions are in logical pixels and glyph pixel dimensions are divided by scale_factor to get logical sizes.

#### Implement TextMeasure

```rust
impl TextMeasure for NativeTextRenderer {
    fn measure_text(&self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2 {
        // TextMeasure requires &self (not &mut self).
        // Use a quick approximate measurement like the current UIRenderer fallback.
        // The accurate measurement is via measure_text_cached(&mut self).
        let em = font_size as f32;
        let avg_char_width = match font_weight {
            FontWeight::Bold => em * 0.56,
            FontWeight::Medium => em * 0.54,
            FontWeight::Regular => em * 0.52,
        };
        Vec2::new(text.len() as f32 * avg_char_width, em)
    }
}
```

This matches the current UIRenderer::TextMeasure impl. The `&self` constraint means we can't call CoreText (which needs mutable state). The accurate measurement path is `measure_text_cached(&mut self)` which UIRenderer calls directly during `draw_node()`.

## Task 3: Register Module

**File:** `crates/manifold-renderer/src/lib.rs`

Add:
```rust
#[cfg(target_os = "macos")]
pub mod native_text;
```

## Task 4: Tests

Add a test in the module (or in `tests/`):

```rust
#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;
    use manifold_ui::node::FontWeight;

    #[test]
    fn font_loading() {
        let fm = FontManager::new();
        // Verify all three fonts loaded (no panic)
    }

    #[test]
    fn text_measurement() {
        let mut fm = FontManager::new();
        let ct_font = fm.get_ct_font(12.0, FontWeight::Regular);
        // Create attributed string, CTLine, measure
        // Width should be > 0 for non-empty text
        // Height should be approximately the font size
    }

    #[test]
    fn glyph_atlas_packing() {
        // Create a GlyphAtlas (CPU-only, no GpuTexture needed for packing test)
        // Rasterize a few glyphs, verify they have non-zero pixel dimensions
        // Verify shelf packing produces non-overlapping UV rects
    }
}
```

Make tests compile and pass. The font loading and measurement tests should work without a GPU device. For atlas tests that need GPU, use `#[ignore]` if no device is available, or skip the GPU upload.

## Task 5: Update Migration Doc

Update `docs/NATIVE_METAL_UI_MIGRATION.md`:
1. Mark Phase 4 as `[DONE]`
2. Note: "CoreText renderer built in `native_text.rs`. Font loading, text shaping, glyph rasterization, atlas management, and draw_indexed rendering. Not yet wired into UIRenderer (Phase 5)."

## Task 6: Build and Verify

1. `cargo clippy --workspace -- -D warnings` — must pass
2. `cargo test --workspace` — must pass (including the new tests)
3. The app runs unchanged — this module is not wired into anything yet

## Implementation Notes

**CoreText API usage patterns (Rust crate bindings):**

```rust
// Font loading
use core_graphics::data_provider::CGDataProvider;
use core_graphics::font::CGFont;
use core_text::font::CTFont;

let data = CGDataProvider::from_buffer(ttf_bytes);
let cg_font = CGFont::from_data_provider(data).unwrap();
let ct_font = CTFont::new_with_graphics_font(&cg_font, size as f64, ptr::null(), None);

// Text measurement
use core_foundation::attributed_string::CFMutableAttributedString;
use core_foundation::string::CFString;
use core_text::line::CTLine;
use core_text::string_attributes::kCTFontAttributeName;

let cf_text = CFString::new(text);
let mut attr_str = CFMutableAttributedString::new();
attr_str.replace_str(&cf_text, CFRange::init(0, 0));
let range = CFRange::init(0, cf_text.char_len());
attr_str.set_attribute(range, unsafe { kCTFontAttributeName }, &ct_font);
let line = CTLine::new_with_attributed_string(attr_str.as_concrete_TypeRef() as _);
let (width, ascent, descent, _leading) = line.get_typographic_bounds();

// Glyph rasterization
use core_graphics::context::CGContext;
use core_graphics::color_space::CGColorSpace;

let color_space = CGColorSpace::create_device_gray();
let ctx = CGContext::create_bitmap_context(
    None, glyph_w as usize, glyph_h as usize,
    8, // bits per component
    glyph_w as usize, // bytes per row (1 byte per pixel for grayscale)
    &color_space,
    core_graphics::base::kCGImageAlphaNone,
);
// Set white text color for grayscale rendering
ctx.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
// Position and draw
ctx.set_text_position(padding_x as f64, padding_y as f64);
// Draw via CTLine for the single glyph
line.draw(&ctx);
// Extract pixels: ctx.data() gives &[u8] of the bitmap
```

**If the Rust bindings don't expose exactly these APIs:** use `objc` `msg_send!` for any missing CoreText/CoreGraphics calls. The project already uses `objc` extensively (manifold-gpu, SharedTextureBridge). Example:
```rust
use objc::{msg_send, sel, sel_impl};
let result: CGRect = unsafe { msg_send![ct_font_ref, boundingRectForGlyphRange:...] };
```

**Do whatever it takes to get the glyph rasterized.** The exact API calls matter less than getting correct grayscale bitmaps into the atlas. The fonts are simple Latin + basic symbols (▶▼≡ via system fallback). No emoji support needed.

## File Summary

| File | Action |
|------|--------|
| `crates/manifold-renderer/Cargo.toml` | Add core-text, core-graphics, core-foundation deps |
| `crates/manifold-renderer/src/native_text.rs` | New module — CoreText text renderer |
| `crates/manifold-renderer/src/lib.rs` | Add `native_text` module |
| `docs/NATIVE_METAL_UI_MIGRATION.md` | Mark Phase 4 done |

## Critical Rules

- Do NOT modify manifold-gpu
- Do NOT wire NativeTextRenderer into UIRenderer — that is Phase 5
- Do NOT remove glyphon from UIRenderer — that is Phase 5
- Do NOT remove glyphon from Cargo.toml — UIRenderer still uses it
- Use `@group(0)` only with unique binding numbers in the WGSL shader
- `#[cfg(target_os = "macos")]` gate the entire module
- `cargo clippy --workspace -- -D warnings` must pass
- Commit and push when done
