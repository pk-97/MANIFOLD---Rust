//! Native macOS CoreText/CoreGraphics text renderer.
//!
//! Phase 4 of the Native Metal UI Migration: an independent text renderer
//! with no glyphon/wgpu dependencies. Uses CoreText for text shaping and
//! measurement, CoreGraphics for glyph rasterization into a shelf-packed
//! atlas texture, and manifold-gpu for the GPU atlas upload and draw_indexed.
//!
//! Not yet wired into UIRenderer — that is Phase 5.

use ahash::AHashMap;
use bytemuck::{Pod, Zeroable};
use core_foundation::{
    attributed_string::CFMutableAttributedString,
    base::{CFRange, TCFType},
    string::CFString,
};
use core_graphics::{
    color_space::CGColorSpace,
    context::CGContext,
    data_provider::CGDataProvider,
    font::CGFont,
    geometry::{CGAffineTransform, CGPoint, CGSize},
};
use core_text::{
    font::CTFont,
    font_descriptor::kCTFontOrientationDefault,
    line::CTLine,
    string_attributes::kCTFontAttributeName,
};
use manifold_gpu::{
    GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuBuffer, GpuDevice, GpuEncoder,
    GpuFilterMode, GpuLoadAction, GpuRenderPipeline, GpuSampler, GpuSamplerDesc, GpuTexture,
    GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage, GpuVertexAttribute,
    GpuVertexFormat, GpuVertexLayout,
};
use manifold_ui::{
    node::{FontWeight, Vec2},
    text::TextMeasure,
};
use std::sync::Arc;

// ─── Constants ───────────────────────────────────────────────────────────────

/// Atlas texture dimensions (1024×1024 R8Unorm = 1 MiB).
const ATLAS_SIZE: u32 = 1024;
/// Padding between glyphs in the atlas (prevents texel bleed during bilinear sampling).
const GLYPH_PADDING: u32 = 1;
/// Max cached measurement entries.
const MAX_MEASURE_CACHE: usize = 512;
/// Frames before an unused measurement entry is evicted.
const MEASURE_EVICT_FRAMES: u64 = 120;

// ─── WGSL Shader ─────────────────────────────────────────────────────────────

const TEXT_SHADER: &str = r#"
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
"#;

// ─── GlyphKey ────────────────────────────────────────────────────────────────

/// Cache key for a rasterized glyph. `size_x10` is physical_px × 10.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    glyph_id: u16,
    /// Physical font size × 10 (e.g. 12px physical → 120).
    size_x10: u16,
    weight: FontWeight,
}

// ─── GlyphInfo ───────────────────────────────────────────────────────────────

/// Cached glyph: atlas UV coordinates, raster dimensions, and bearing.
struct GlyphInfo {
    /// UV rect in the atlas (0..1 range).
    uv_x: f32,
    uv_y: f32,
    uv_w: f32,
    uv_h: f32,
    /// Physical pixel dimensions of the rasterized bitmap.
    pixel_w: u32,
    pixel_h: u32,
    /// Distance from the glyph x-origin to the bitmap's left edge (physical px).
    bearing_x: f32,
    /// Distance from baseline to the bitmap's top edge (physical px, positive = above baseline).
    bearing_y: f32,
}

// ─── FontManager ─────────────────────────────────────────────────────────────

/// Loads the three Inter weights and creates CTFont instances on demand.
struct FontManager {
    regular: CGFont,
    medium: CGFont,
    bold: CGFont,
    /// (size_x10_physical, weight) → CTFont cache.
    cache: AHashMap<(u16, FontWeight), CTFont>,
}

impl FontManager {
    fn new() -> Self {
        let regular = Self::load(include_bytes!("../assets/fonts/Inter-Regular.ttf"));
        let medium = Self::load(include_bytes!("../assets/fonts/Inter-Medium.ttf"));
        let bold = Self::load(include_bytes!("../assets/fonts/Inter-Bold.ttf"));
        Self {
            regular,
            medium,
            bold,
            cache: AHashMap::new(),
        }
    }

    fn load(ttf_bytes: &'static [u8]) -> CGFont {
        let data: Vec<u8> = ttf_bytes.to_vec();
        let provider = CGDataProvider::from_buffer(Arc::new(data));
        CGFont::from_data_provider(provider).expect("Failed to create CGFont from Inter TTF")
    }

    /// Get or create a CTFont at the given physical size and weight.
    fn get_ct_font(&mut self, physical_size: f32, weight: FontWeight) -> &CTFont {
        let size_x10 = (physical_size * 10.0).round() as u16;
        self.cache.entry((size_x10, weight)).or_insert_with(|| {
            let cg_font = match weight {
                FontWeight::Regular => &self.regular,
                FontWeight::Medium => &self.medium,
                FontWeight::Bold => &self.bold,
            };
            core_text::font::new_from_CGFont(cg_font, physical_size as f64)
        })
    }
}

// ─── GlyphAtlas ──────────────────────────────────────────────────────────────

/// Shelf-packed glyph atlas. Writes into a CPU pixel buffer and uploads
/// to a GPU R8Unorm texture when dirty.
struct GlyphAtlas {
    /// GPU texture — None in CPU-only test mode.
    texture: Option<GpuTexture>,
    /// Cached glyphs by key.
    glyphs: AHashMap<GlyphKey, GlyphInfo>,
    /// CPU pixel buffer (R8 grayscale, ATLAS_SIZE × ATLAS_SIZE).
    pixels: Vec<u8>,
    /// Shelf packing state.
    shelf_x: u32,
    shelf_y: u32,
    shelf_height: u32,
    /// True when new glyphs were added and GPU upload is needed.
    dirty: bool,
}

impl GlyphAtlas {
    fn new(device: &GpuDevice) -> Self {
        let texture = device.create_texture(&GpuTextureDesc {
            width: ATLAS_SIZE,
            height: ATLAS_SIZE,
            depth: 1,
            format: GpuTextureFormat::R8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
            label: "TextAtlas",
        });
        Self {
            texture: Some(texture),
            glyphs: AHashMap::new(),
            pixels: vec![0u8; (ATLAS_SIZE * ATLAS_SIZE) as usize],
            shelf_x: 0,
            shelf_y: 0,
            shelf_height: 0,
            dirty: false,
        }
    }

    /// CPU-only constructor for tests (no GPU texture).
    #[cfg(test)]
    fn new_cpu_only() -> Self {
        Self {
            texture: None,
            glyphs: AHashMap::new(),
            pixels: vec![0u8; (ATLAS_SIZE * ATLAS_SIZE) as usize],
            shelf_x: 0,
            shelf_y: 0,
            shelf_height: 0,
            dirty: false,
        }
    }

    /// Ensure glyph is in the atlas, rasterizing if needed. Returns its GlyphInfo.
    fn rasterize_glyph(
        &mut self,
        font_mgr: &mut FontManager,
        key: GlyphKey,
    ) -> Option<&GlyphInfo> {
        if self.glyphs.contains_key(&key) {
            return self.glyphs.get(&key);
        }

        let physical_size = key.size_x10 as f32 / 10.0;
        let ct_font = font_mgr.get_ct_font(physical_size, key.weight);
        let ascent = ct_font.ascent() as f32;
        let descent = ct_font.descent().abs() as f32;

        // Get advance width for this glyph.
        let glyph_id = key.glyph_id;
        let mut advance_size = CGSize::new(0.0, 0.0);
        let advance_w = unsafe {
            ct_font.get_advances_for_glyphs(
                kCTFontOrientationDefault,
                &glyph_id,
                &mut advance_size,
                1,
            )
        } as f32;

        // Bitmap dimensions in physical pixels.
        // Width = advance + horizontal padding; height = full line height + vertical padding.
        let bitmap_w = (advance_w.ceil() as u32).max(1) + 2 * GLYPH_PADDING;
        let bitmap_h = (ascent + descent).ceil() as u32 + 2 * GLYPH_PADDING;

        // Shelf packing: advance to next row if this glyph doesn't fit.
        if self.shelf_x + bitmap_w > ATLAS_SIZE {
            self.shelf_y += self.shelf_height + GLYPH_PADDING;
            self.shelf_x = 0;
            self.shelf_height = 0;
        }
        // Atlas full — clear everything and restart (rare fallback).
        if self.shelf_y + bitmap_h > ATLAS_SIZE {
            self.pixels.fill(0);
            self.glyphs.clear();
            self.shelf_x = 0;
            self.shelf_y = 0;
            self.shelf_height = 0;
        }

        let atlas_x = self.shelf_x;
        let atlas_y = self.shelf_y;

        // Rasterize into a caller-owned pixel buffer, copy into atlas.
        if let Some(glyph_pixels) = rasterize_glyph_bitmap(
            ct_font,
            glyph_id,
            bitmap_w,
            bitmap_h,
            ascent,
        ) {
            for row in 0..bitmap_h {
                let src_start = (row * bitmap_w) as usize;
                let dst_start = ((atlas_y + row) * ATLAS_SIZE + atlas_x) as usize;
                let count = bitmap_w as usize;
                self.pixels[dst_start..dst_start + count]
                    .copy_from_slice(&glyph_pixels[src_start..src_start + count]);
            }
        }

        self.shelf_x += bitmap_w + GLYPH_PADDING;
        self.shelf_height = self.shelf_height.max(bitmap_h);
        self.dirty = true;

        // UV coordinates normalised to 0..1.
        let uv_x = atlas_x as f32 / ATLAS_SIZE as f32;
        let uv_y = atlas_y as f32 / ATLAS_SIZE as f32;
        let uv_w = bitmap_w as f32 / ATLAS_SIZE as f32;
        let uv_h = bitmap_h as f32 / ATLAS_SIZE as f32;

        // bearing_x: left padding offset from glyph origin to bitmap left.
        let bearing_x = -(GLYPH_PADDING as f32);
        // bearing_y: distance from baseline (y-down) to the bitmap's TOP edge.
        // In y-down screen space: baseline = line_top + ascent.
        // Bitmap top = baseline - (ascent + GLYPH_PADDING).
        // So bearing_y = ascent + GLYPH_PADDING (subtract from baseline to get bitmap top).
        let bearing_y = ascent + GLYPH_PADDING as f32;

        let info = GlyphInfo {
            uv_x,
            uv_y,
            uv_w,
            uv_h,
            pixel_w: bitmap_w,
            pixel_h: bitmap_h,
            bearing_x,
            bearing_y,
        };
        self.glyphs.insert(key, info);
        self.glyphs.get(&key)
    }

    /// Upload atlas to GPU if any new glyphs were rasterized this frame.
    fn upload_if_dirty(&mut self, device: &GpuDevice) {
        if !self.dirty {
            return;
        }
        if let Some(ref texture) = self.texture {
            device.upload_texture(texture, &self.pixels);
        }
        self.dirty = false;
    }
}

/// Rasterize a single glyph into a caller-owned grayscale buffer (R8, y-down).
///
/// Uses a pre-allocated Vec as the CGBitmapContext backing store. After
/// `draw_glyphs` consumes and releases the context, the pixel data
/// remains in the returned Vec because CGBitmapContext does not free
/// caller-provided memory.
fn rasterize_glyph_bitmap(
    ct_font: &CTFont,
    glyph_id: u16,
    bitmap_w: u32,
    bitmap_h: u32,
    ascent: f32,
) -> Option<Vec<u8>> {
    let w = bitmap_w as usize;
    let h = bitmap_h as usize;

    // Pre-allocate the pixel buffer. CGBitmapContext will write into this.
    let mut pixels = vec![0u8; w * h];

    // Grayscale bitmap context backed by our pixel buffer.
    // kCGImageAlphaNone = 0: single-channel grayscale, no alpha.
    let color_space = CGColorSpace::create_device_gray();
    let ctx = CGContext::create_bitmap_context(
        // Safety: pixels lives until the end of this function; the context
        // is consumed by draw_glyphs before this function returns.
        Some(pixels.as_mut_ptr() as *mut std::ffi::c_void),
        w,
        h,
        8,   // bits per component
        w,   // bytes per row (1 byte per pixel)
        &color_space,
        0u32, // kCGImageAlphaNone
    );

    // Y-flip transform: makes y=0 correspond to the TOP of the bitmap.
    // Default CGContext has y-up (origin at bottom-left), but CGBitmapContextGetData
    // stores rows top-first. With this flip, drawing at y=baseline_from_top
    // directly maps to the correct data row.
    let flip = CGAffineTransform {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: -1.0,
        tx: 0.0,
        ty: h as f64,
    };
    ctx.concat_ctm(flip);

    // White glyph on black background — R channel becomes the coverage alpha.
    ctx.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
    ctx.set_allows_font_smoothing(false);
    ctx.set_should_smooth_fonts(false);

    // In the flipped (y-down) coordinate system:
    // - Top of bitmap = y=0, bottom = y=h
    // - Baseline is at y = ascent + GLYPH_PADDING (rows below the top padding)
    let baseline_y = (ascent + GLYPH_PADDING as f32) as f64;
    let origin_x = GLYPH_PADDING as f64;
    let position = CGPoint::new(origin_x, baseline_y);

    // draw_glyphs consumes the context by value. After this call, ctx is
    // released, but `pixels` retains the bitmap data.
    ct_font.draw_glyphs(&[glyph_id], &[position], ctx);

    Some(pixels)
}

// ─── TextCommand ─────────────────────────────────────────────────────────────

struct TextCommand {
    x: f32,
    y: f32,
    text: String,
    font_size: f32,
    color: [u8; 4],
    font_weight: FontWeight,
    clip_bounds: Option<[f32; 4]>,
}

// ─── TextVertex ──────────────────────────────────────────────────────────────

/// Vertex for a textured glyph quad. Stride = 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TextVertex {
    position: [f32; 2], // @location(0), offset 0
    uv: [f32; 2],       // @location(1), offset 8
    color: [f32; 4],    // @location(2), offset 16
}

fn text_vertex_layout() -> GpuVertexLayout {
    GpuVertexLayout {
        stride: 32,
        attributes: vec![
            GpuVertexAttribute {
                format: GpuVertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            },
            GpuVertexAttribute {
                format: GpuVertexFormat::Float32x2,
                offset: 8,
                shader_location: 1,
            },
            GpuVertexAttribute {
                format: GpuVertexFormat::Float32x4,
                offset: 16,
                shader_location: 2,
            },
        ],
    }
}

// ─── NativeTextRenderer ──────────────────────────────────────────────────────

/// Standalone CoreText-based text renderer.
///
/// API mirrors UIRenderer's text methods. Will be wired in during Phase 5.
pub struct NativeTextRenderer {
    font_manager: FontManager,
    atlas: GlyphAtlas,
    pipeline: GpuRenderPipeline,
    sampler: GpuSampler,

    // Draw queue.
    commands: Vec<TextCommand>,

    // Fresh GpuBuffers created each prepare() call — avoids aliasing with in-flight GPU work.
    prepared_vertex_buf: Option<GpuBuffer>,
    prepared_index_buf: Option<GpuBuffer>,
    prepared_index_count: u32,
    prepared_globals: [f32; 4], // [viewport_w, viewport_h, offset_x, offset_y]

    // CPU scratch (reused each frame).
    vertices: Vec<TextVertex>,
    indices: Vec<u32>,

    // Measurement cache: (text, font_size_x10_logical, weight) → Vec2.
    measure_cache: AHashMap<(String, u16, FontWeight), Vec2>,
    measure_used: AHashMap<(String, u16, FontWeight), u64>,
    frame_generation: u64,
}

impl NativeTextRenderer {
    /// Create the renderer. Call once at startup.
    pub fn new(device: &GpuDevice, format: GpuTextureFormat) -> Self {
        let font_manager = FontManager::new();
        let atlas = GlyphAtlas::new(device);

        let blend = GpuBlendState {
            src_factor: GpuBlendFactor::SrcAlpha,
            dst_factor: GpuBlendFactor::OneMinusSrcAlpha,
            operation: GpuBlendOp::Add,
            src_alpha_factor: GpuBlendFactor::One,
            dst_alpha_factor: GpuBlendFactor::OneMinusSrcAlpha,
            alpha_operation: GpuBlendOp::Add,
        };
        let layout = text_vertex_layout();
        let pipeline = device.create_render_pipeline_with_vertex_layout(
            TEXT_SHADER,
            "vs_main",
            "fs_main",
            format,
            Some(blend),
            &layout,
            "TextRenderer",
        );

        let sampler = device.create_sampler(&GpuSamplerDesc {
            min_filter: GpuFilterMode::Linear,
            mag_filter: GpuFilterMode::Linear,
            ..Default::default()
        });

        Self {
            font_manager,
            atlas,
            pipeline,
            sampler,
            commands: Vec::new(),
            prepared_vertex_buf: None,
            prepared_index_buf: None,
            prepared_index_count: 0,
            prepared_globals: [0.0; 4],
            vertices: Vec::new(),
            indices: Vec::new(),
            measure_cache: AHashMap::new(),
            measure_used: AHashMap::new(),
            frame_generation: 0,
        }
    }

    /// Queue a text draw command.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        font_size: f32,
        color: [u8; 4],
        font_weight: FontWeight,
        clip_bounds: Option<[f32; 4]>,
    ) {
        if text.is_empty() {
            return;
        }
        self.commands.push(TextCommand {
            x,
            y,
            text: text.to_string(),
            font_size,
            color,
            font_weight,
            clip_bounds,
        });
    }

    /// Accurate text measurement via CoreText CTLine. Cached.
    pub fn measure_text_cached(
        &mut self,
        text: &str,
        font_size: u16,
        font_weight: FontWeight,
    ) -> Vec2 {
        let key = (text.to_string(), font_size, font_weight);
        if let Some(&size) = self.measure_cache.get(&key) {
            self.measure_used.insert(key, self.frame_generation);
            return size;
        }

        let size = measure_text_ct(&mut self.font_manager, text, font_size as f32, font_weight);

        if self.measure_cache.len() >= MAX_MEASURE_CACHE {
            self.evict_oldest_measure();
        }
        self.measure_cache.insert(key.clone(), size);
        self.measure_used.insert(key, self.frame_generation);
        size
    }

    /// Advance the frame counter and evict stale measurement cache entries.
    pub fn begin_frame(&mut self) {
        self.frame_generation += 1;
        let frame_gen = self.frame_generation;
        if frame_gen.is_multiple_of(60) {
            self.measure_used
                .retain(|_, &mut last| frame_gen.saturating_sub(last) <= MEASURE_EVICT_FRAMES);
            self.measure_cache
                .retain(|k, _| self.measure_used.contains_key(k));
        }
    }

    /// Clear the draw queue (call after render()).
    pub fn clear_commands(&mut self) {
        self.commands.clear();
    }

    /// Shape all queued text, ensure glyphs are in atlas, build vertex/index buffers.
    /// Returns true if there is anything to render.
    pub fn prepare(
        &mut self,
        device: &GpuDevice,
        viewport_w: u32,
        viewport_h: u32,
        offset_x: f32,
        offset_y: f32,
        scale_factor: f64,
    ) -> bool {
        self.vertices.clear();
        self.indices.clear();
        self.prepared_index_count = 0;

        if self.commands.is_empty() {
            return false;
        }

        self.prepared_globals = [viewport_w as f32, viewport_h as f32, offset_x, offset_y];
        let scale = scale_factor as f32;

        let commands: Vec<TextCommand> = std::mem::take(&mut self.commands);

        for cmd in &commands {
            let physical_size = cmd.font_size * scale;
            let size_x10 = (physical_size * 10.0).round() as u16;

            let ct_font = self.font_manager.get_ct_font(physical_size, cmd.font_weight);
            let glyphs_and_positions = shape_line(ct_font, &cmd.text);
            let Some((glyph_ids, positions_ct)) = glyphs_and_positions else {
                continue;
            };

            let ascent = ct_font.ascent() as f32;

            // Baseline in y-down screen space. cmd.y is the top of the line box.
            let baseline_y = cmd.y + ascent / scale;

            let color = [
                cmd.color[0] as f32 / 255.0,
                cmd.color[1] as f32 / 255.0,
                cmd.color[2] as f32 / 255.0,
                cmd.color[3] as f32 / 255.0,
            ];

            for (i, &glyph_id) in glyph_ids.iter().enumerate() {
                let key = GlyphKey {
                    glyph_id,
                    size_x10,
                    weight: cmd.font_weight,
                };
                let Some(info) = self.atlas.rasterize_glyph(&mut self.font_manager, key) else {
                    continue;
                };

                // Quad dimensions in logical (screen) pixels.
                let bw = info.pixel_w as f32 / scale;
                let bh = info.pixel_h as f32 / scale;
                let bearing_x = info.bearing_x / scale;
                let bearing_y = info.bearing_y / scale;

                // Glyph origin x from the CTRun position (in logical px).
                let glyph_origin_x = cmd.x + positions_ct[i].x as f32 / scale;

                // Quad top-left in screen space.
                let x0 = glyph_origin_x + bearing_x;
                let y0 = baseline_y - bearing_y;
                let x1 = x0 + bw;
                let y1 = y0 + bh;

                // Clip: skip glyph if entirely outside clip rect.
                if let Some([cx, cy, cw, ch]) = cmd.clip_bounds
                    && (x1 < cx || y1 < cy || x0 > cx + cw || y0 > cy + ch)
                {
                    continue;
                }

                let base = self.vertices.len() as u32;
                // v0=top-left, v1=top-right, v2=bottom-right, v3=bottom-left.
                self.vertices.push(TextVertex {
                    position: [x0, y0],
                    uv: [info.uv_x, info.uv_y],
                    color,
                });
                self.vertices.push(TextVertex {
                    position: [x1, y0],
                    uv: [info.uv_x + info.uv_w, info.uv_y],
                    color,
                });
                self.vertices.push(TextVertex {
                    position: [x1, y1],
                    uv: [info.uv_x + info.uv_w, info.uv_y + info.uv_h],
                    color,
                });
                self.vertices.push(TextVertex {
                    position: [x0, y1],
                    uv: [info.uv_x, info.uv_y + info.uv_h],
                    color,
                });
                self.indices
                    .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }

        if self.vertices.is_empty() {
            return false;
        }

        self.atlas.upload_if_dirty(device);

        // Create fresh GpuBuffers each prepare() — avoids aliasing with in-flight GPU work.
        let vdata = bytemuck::cast_slice::<TextVertex, u8>(&self.vertices);
        let vbuf = device.create_buffer_shared(vdata.len() as u64);
        unsafe { vbuf.write(0, vdata); }

        let idata = bytemuck::cast_slice::<u32, u8>(&self.indices);
        let ibuf = device.create_buffer_shared(idata.len() as u64);
        unsafe { ibuf.write(0, idata); }

        self.prepared_vertex_buf = Some(vbuf);
        self.prepared_index_buf = Some(ibuf);
        self.prepared_index_count = self.indices.len() as u32;
        true
    }

    /// Issue the draw_indexed call. Must call `prepare()` first.
    pub fn render(
        &self,
        encoder: &mut GpuEncoder,
        target: &GpuTexture,
        load_action: GpuLoadAction,
    ) {
        if self.prepared_index_count == 0 {
            return;
        }
        let Some(ref atlas_texture) = self.atlas.texture else {
            return;
        };

        encoder.draw_indexed(
            &self.pipeline,
            target,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::cast_slice(&self.prepared_globals),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: atlas_texture,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
            ],
            self.prepared_vertex_buf.as_ref().unwrap(),
            self.prepared_index_buf.as_ref().unwrap(),
            self.prepared_index_count,
            None,
            load_action,
            "TextRenderer",
        );
    }

    fn evict_oldest_measure(&mut self) {
        let oldest = self
            .measure_used
            .iter()
            .min_by_key(|(_, v)| *v)
            .map(|(k, _)| k.clone());
        if let Some(key) = oldest {
            self.measure_cache.remove(&key);
            self.measure_used.remove(&key);
        }
    }
}

// ─── TextMeasure impl ────────────────────────────────────────────────────────

impl TextMeasure for NativeTextRenderer {
    /// Approximate measurement using a character-count heuristic.
    ///
    /// For accurate layout measurement, call `measure_text_cached(&mut self, ...)` directly.
    fn measure_text(&self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2 {
        let em = font_size as f32;
        let avg_char_width = match font_weight {
            FontWeight::Bold => em * 0.56,
            FontWeight::Medium => em * 0.54,
            FontWeight::Regular => em * 0.52,
        };
        Vec2::new(text.chars().count() as f32 * avg_char_width, em)
    }
}

// ─── CoreText helpers ────────────────────────────────────────────────────────

/// Measure text using CoreText CTLine typographic bounds.
fn measure_text_ct(
    font_mgr: &mut FontManager,
    text: &str,
    font_size: f32,
    weight: FontWeight,
) -> Vec2 {
    if text.is_empty() {
        return Vec2::new(0.0, font_size);
    }
    let ct_font = font_mgr.get_ct_font(font_size, weight);
    let line = make_ct_line(ct_font, text);
    let bounds = line.get_typographic_bounds();
    let w = bounds.width as f32;
    let h = ((bounds.ascent + bounds.descent) as f32).max(font_size);
    Vec2::new(w, h)
}

/// Create a CTLine from a text string and a CTFont.
fn make_ct_line(ct_font: &CTFont, text: &str) -> CTLine {
    let cf_text = CFString::new(text);
    let mut attr_str = CFMutableAttributedString::new();
    attr_str.replace_str(&cf_text, CFRange::init(0, 0));
    let range = CFRange::init(0, cf_text.char_len());
    // Safety: kCTFontAttributeName is a valid CoreText attribute key (a static CFStringRef).
    unsafe {
        attr_str.set_attribute(range, kCTFontAttributeName, ct_font);
    }
    CTLine::new_with_attributed_string(attr_str.as_concrete_TypeRef())
}

/// Shape text into (glyph_ids, positions_in_line).
fn shape_line(ct_font: &CTFont, text: &str) -> Option<(Vec<u16>, Vec<CGPoint>)> {
    let line = make_ct_line(ct_font, text);
    let runs = line.glyph_runs();

    let mut glyph_ids: Vec<u16> = Vec::new();
    let mut positions: Vec<CGPoint> = Vec::new();

    for run in runs.iter() {
        let count = run.glyph_count() as usize;
        if count == 0 {
            continue;
        }
        let run_glyphs = run.glyphs();
        let run_positions = run.positions();
        glyph_ids.extend_from_slice(&run_glyphs);
        positions.extend_from_slice(&run_positions);
    }

    if glyph_ids.is_empty() {
        None
    } else {
        Some((glyph_ids, positions))
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_loading() {
        // Verify all three Inter fonts load without panic.
        let _fm = FontManager::new();
    }

    #[test]
    fn text_measurement() {
        let mut fm = FontManager::new();
        let ct_font = fm.get_ct_font(12.0, FontWeight::Regular);
        let line = make_ct_line(ct_font, "Hello");
        let bounds = line.get_typographic_bounds();
        assert!(bounds.width > 0.0, "text width should be positive, got {}", bounds.width);
        assert!(bounds.ascent > 0.0, "ascent should be positive, got {}", bounds.ascent);
        assert!(bounds.descent >= 0.0, "descent should be non-negative, got {}", bounds.descent);
        let height = bounds.ascent + bounds.descent;
        assert!(height >= 8.0, "line height should be at least 8px, got {height}");
    }

    #[test]
    fn glyph_atlas_packing() {
        let mut fm = FontManager::new();
        let mut atlas = GlyphAtlas::new_cpu_only();

        // Rasterize glyphs A-E at 12px physical.
        let size_x10 = 120u16;
        let glyph_chars = ['A', 'B', 'C', 'D', 'E'];
        let ct_font = fm.get_ct_font(12.0, FontWeight::Regular);
        let glyph_ids: Vec<u16> = glyph_chars
            .iter()
            .map(|c| ct_font.get_glyph_with_name(&c.to_string()))
            .collect();
        // Drop ct_font borrow before calling rasterize_glyph.
        drop(ct_font);

        let mut rects: Vec<(f32, f32, f32, f32, u32, u32)> = Vec::new();
        for &gid in &glyph_ids {
            let key = GlyphKey {
                glyph_id: gid,
                size_x10,
                weight: FontWeight::Regular,
            };
            if let Some(info) = atlas.rasterize_glyph(&mut fm, key) {
                rects.push((
                    info.uv_x,
                    info.uv_y,
                    info.uv_w,
                    info.uv_h,
                    info.pixel_w,
                    info.pixel_h,
                ));
            }
        }

        assert_eq!(rects.len(), glyph_chars.len(), "all glyphs should be in atlas");

        // Non-zero pixel dimensions.
        for &(_, _, _, _, pw, ph) in &rects {
            assert!(pw > 0, "pixel_w should be positive");
            assert!(ph > 0, "pixel_h should be positive");
        }

        // Non-overlapping UV rects.
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let (x0, y0, w0, h0, _, _) = rects[i];
                let (x1, y1, w1, h1, _, _) = rects[j];
                let overlap_x = x0 < x1 + w1 && x0 + w0 > x1;
                let overlap_y = y0 < y1 + h1 && y0 + h0 > y1;
                assert!(
                    !(overlap_x && overlap_y),
                    "glyph UV rects {i} and {j} overlap"
                );
            }
        }
    }
}
