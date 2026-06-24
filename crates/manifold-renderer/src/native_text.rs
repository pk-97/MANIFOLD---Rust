//! Native macOS CoreText/CoreGraphics text renderer.
//!
//! Uses CoreText for text shaping and measurement, CoreGraphics for glyph
//! rasterization into a shelf-packed atlas texture, and manifold-gpu for
//! the GPU atlas upload and draw_indexed.

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
    geometry::{CGPoint, CGSize},
};
use core_text::{
    font::CTFont, font_descriptor::kCTFontOrientationDefault, line::CTLine,
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
use std::cell::RefCell;
use std::sync::Arc;

use crate::ui_renderer::Depth;

// ─── Constants ───────────────────────────────────────────────────────────────

/// Atlas texture dimensions (1024×1024 R8Unorm = 1 MiB).
const ATLAS_SIZE: u32 = 1024;
/// Padding between glyphs in the atlas (prevents texel bleed during bilinear sampling).
const GLYPH_PADDING: u32 = 1;
/// Max cached measurement entries.
const MAX_MEASURE_CACHE: usize = 512;

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
    /// Set when the atlas overflows and clears. Consumed by NativeTextRenderer
    /// to re-inject waveform icons whose UV regions were destroyed by the clear.
    was_cleared: bool,
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
            mip_levels: 1,
        });
        Self {
            texture: Some(texture),
            glyphs: AHashMap::new(),
            pixels: vec![0u8; (ATLAS_SIZE * ATLAS_SIZE) as usize],
            shelf_x: 0,
            shelf_y: 0,
            shelf_height: 0,
            dirty: false,
            was_cleared: false,
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
            was_cleared: false,
        }
    }

    /// Ensure glyph is in the atlas, rasterizing if needed. Returns its GlyphInfo.
    fn rasterize_glyph(&mut self, font_mgr: &mut FontManager, key: GlyphKey) -> Option<&GlyphInfo> {
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
        // Sets was_cleared so NativeTextRenderer re-injects waveform icons.
        if self.shelf_y + bitmap_h > ATLAS_SIZE {
            self.pixels.fill(0);
            self.glyphs.clear();
            self.shelf_x = 0;
            self.shelf_y = 0;
            self.shelf_height = 0;
            self.was_cleared = true;
        }

        let atlas_x = self.shelf_x;
        let atlas_y = self.shelf_y;

        // Rasterize into a caller-owned pixel buffer, copy into atlas.
        if let Some(glyph_pixels) =
            rasterize_glyph_bitmap(ct_font, glyph_id, bitmap_w, bitmap_h, descent)
        {
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

/// Rasterize a single glyph into a caller-owned grayscale buffer (R8).
///
/// Uses CG's native y-up coordinate system (NO Y-flip). The bitmap memory
/// is laid out top-row-first, and CG y=0 is at the bottom. So ascenders
/// (high CG y) end up in the top rows of memory — right-side-up glyphs.
///
/// `descent` is the font's descent (positive value, distance below baseline).
fn rasterize_glyph_bitmap(
    ct_font: &CTFont,
    glyph_id: u16,
    bitmap_w: u32,
    bitmap_h: u32,
    descent: f32,
) -> Option<Vec<u8>> {
    let w = bitmap_w as usize;
    let h = bitmap_h as usize;

    let mut pixels = vec![0u8; w * h];

    let color_space = CGColorSpace::create_device_gray();
    let ctx = CGContext::create_bitmap_context(
        Some(pixels.as_mut_ptr() as *mut std::ffi::c_void),
        w,
        h,
        8, // bits per component
        w, // bytes per row (1 byte per pixel)
        &color_space,
        0u32, // kCGImageAlphaNone
    );

    // NO Y-flip. CG native y-up: y=0 at bottom, y=h at top.
    // Bitmap memory: row 0 = top of image = CG y=h.
    //
    // Place baseline at y = descent + GLYPH_PADDING (from bottom).
    // Ascent extends upward, descent extends downward — both fit in the bitmap.

    ctx.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
    ctx.set_allows_font_smoothing(false);
    ctx.set_should_smooth_fonts(false);

    let baseline_y = (descent + GLYPH_PADDING as f32) as f64;
    let origin_x = GLYPH_PADDING as f64;
    let position = CGPoint::new(origin_x, baseline_y);

    ct_font.draw_glyphs(&[glyph_id], &[position], ctx);

    Some(pixels)
}

// ─── Icon Atlas Support ──────────────────────────────────────────────────────

/// UV rect for an icon injected into the glyph atlas.
#[derive(Clone, Copy)]
struct IconInfo {
    uv_x: f32,
    uv_y: f32,
    uv_w: f32,
    uv_h: f32,
}

impl GlyphAtlas {
    /// Inject a pre-rendered alpha bitmap into the atlas. Returns its UV info.
    fn inject_icon(&mut self, pixels: &[u8], w: u32, h: u32) -> IconInfo {
        // Shelf packing (same as rasterize_glyph)
        if self.shelf_x + w > ATLAS_SIZE {
            self.shelf_y += self.shelf_height + GLYPH_PADDING;
            self.shelf_x = 0;
            self.shelf_height = 0;
        }
        if self.shelf_y + h > ATLAS_SIZE {
            // Shouldn't happen with 5 small icons, but guard anyway
            self.pixels.fill(0);
            self.glyphs.clear();
            self.shelf_x = 0;
            self.shelf_y = 0;
            self.shelf_height = 0;
        }

        let ax = self.shelf_x;
        let ay = self.shelf_y;
        for row in 0..h {
            let src = (row * w) as usize;
            let dst = ((ay + row) * ATLAS_SIZE + ax) as usize;
            self.pixels[dst..dst + w as usize].copy_from_slice(&pixels[src..src + w as usize]);
        }
        self.shelf_x += w + GLYPH_PADDING;
        self.shelf_height = self.shelf_height.max(h);
        self.dirty = true;

        IconInfo {
            uv_x: ax as f32 / ATLAS_SIZE as f32,
            uv_y: ay as f32 / ATLAS_SIZE as f32,
            uv_w: w as f32 / ATLAS_SIZE as f32,
            uv_h: h as f32 / ATLAS_SIZE as f32,
        }
    }
}

// ─── Waveform Icon Generation ───────────────────────────────────────────────

/// Icon IDs for the driver waveforms + UI glyphs. Exported for use by UIRenderer.
/// Each maps to a PUA codepoint U+E000 + id; the renderer draws the atlas icon
/// for any text whose first char falls in that range.
pub const ICON_WAVE_SINE: u8 = 0;
pub const ICON_WAVE_TRIANGLE: u8 = 1;
pub const ICON_WAVE_SAWTOOTH: u8 = 2;
pub const ICON_WAVE_SQUARE: u8 = 3;
pub const ICON_WAVE_RANDOM: u8 = 4;
/// Cog / gear — the "hide modulation settings" toggle (U+E005). The UI font has
/// no ⚙ glyph (renders as tofu), so it's a procedurally-drawn atlas icon.
pub const ICON_COG: u8 = 5;
pub const ICON_COUNT: usize = 6;

/// Size of generated waveform icon bitmaps (physical pixels).
/// 64px covers 2x retina for ~22px logical buttons with clarity to spare.
const ICON_SIZE: u32 = 64;
const ICON_PADDING: f32 = 5.0;
const ICON_LINE_THICKNESS: f32 = 2.8;
const ICON_AA_WIDTH: f32 = 1.4;
/// Vertical margin: waveform values are remapped from 0..1 to MARGIN..1-MARGIN
/// so peaks don't touch the icon edge.
const ICON_V_MARGIN: f32 = 0.1;

/// Generate the atlas icons: the 5 waveform SDF icons (ported from Unity
/// DriverWaveformIcons.cs) plus the cog at [`ICON_COG`].
fn generate_waveform_icons() -> [Vec<u8>; ICON_COUNT] {
    std::array::from_fn(|i| {
        if i == ICON_COG as usize {
            generate_cog_icon()
        } else {
            generate_single_waveform(i)
        }
    })
}

/// Procedural cog/gear icon — a filled annulus (body with a center hole) plus
/// eight radial teeth, antialiased by 4×4 supersampled coverage. R8 like the
/// waveform icons (alpha = coverage). Used for the "hide modulation settings"
/// toggle, since the UI font carries no ⚙ glyph.
fn generate_cog_icon() -> Vec<u8> {
    const TEETH: usize = 8;
    const R_HOLE: f32 = 0.17; // center hole radius (normalized)
    const R_BODY: f32 = 0.30; // gear body radius (between teeth)
    const R_TEETH: f32 = 0.43; // outer radius at a tooth
    const TOOTH_HALF: f32 = 0.28; // tooth half-width as a fraction of one period
    const SS: usize = 4; // supersample grid per axis
    let period = std::f32::consts::TAU / TEETH as f32;
    let size = ICON_SIZE as usize;
    let mut pixels = vec![0u8; size * size];

    for py in 0..size {
        for px in 0..size {
            let mut inside = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let nx = (px as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let ny = (py as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    let dx = nx - 0.5;
                    let dy = ny - 0.5;
                    let r = (dx * dx + dy * dy).sqrt();
                    if !(R_HOLE..=R_TEETH).contains(&r) {
                        continue;
                    }
                    // Radius at this angle: full tooth radius inside a tooth arc,
                    // body radius in the gaps between teeth.
                    let frac = dy.atan2(dx).rem_euclid(period) / period; // 0..1 within a tooth period
                    let r_eff = if (frac - 0.5).abs() < TOOTH_HALF {
                        R_TEETH
                    } else {
                        R_BODY
                    };
                    if r <= r_eff {
                        inside += 1;
                    }
                }
            }
            let coverage = inside as f32 / (SS * SS) as f32;
            pixels[py * size + px] = (coverage * 255.0 + 0.5) as u8;
        }
    }
    pixels
}

fn generate_single_waveform(idx: usize) -> Vec<u8> {
    // Remap helper: maps v from 0..1 into MARGIN..(1-MARGIN) for breathing room.
    let remap = |v: f32| ICON_V_MARGIN + v * (1.0 - 2.0 * ICON_V_MARGIN);

    let points: Vec<(f32, f32)> = match idx {
        0 => {
            // Sine — 128 samples for smooth curves
            (0..128)
                .map(|i| {
                    let t = i as f32 / 127.0;
                    let v = (t * std::f32::consts::TAU).sin() * 0.5 + 0.5;
                    (t, remap(v))
                })
                .collect()
        }
        1 => vec![(0.0, remap(0.0)), (0.5, remap(1.0)), (1.0, remap(0.0))],
        2 => vec![(0.0, remap(0.0)), (1.0, remap(1.0)), (1.0, remap(0.0))],
        3 => vec![
            (0.0, remap(1.0)),
            (0.5, remap(1.0)),
            (0.5, remap(0.0)),
            (1.0, remap(0.0)),
        ],
        4 => vec![
            (0.0, remap(0.3)),
            (0.2, remap(0.3)),
            (0.2, remap(0.85)),
            (0.4, remap(0.85)),
            (0.4, remap(0.1)),
            (0.6, remap(0.1)),
            (0.6, remap(0.65)),
            (0.8, remap(0.65)),
            (0.8, remap(0.45)),
            (1.0, remap(0.45)),
        ],
        _ => vec![(0.0, 0.5), (1.0, 0.5)],
    };

    let size = ICON_SIZE as usize;
    let draw_size = ICON_SIZE as f32 - ICON_PADDING * 2.0;
    let half_thick = ICON_LINE_THICKNESS * 0.5;
    let aa_outer = half_thick + ICON_AA_WIDTH * 0.5;
    let aa_inner = half_thick - ICON_AA_WIDTH * 0.5;

    let mut pixels = vec![0u8; size * size];

    for py in 0..size {
        // Flip Y: py=0 is top of bitmap, but waveform y=0 is bottom.
        let ny = 1.0 - (py as f32 - ICON_PADDING) / draw_size;
        for px in 0..size {
            let nx = (px as f32 - ICON_PADDING) / draw_size;

            // Min distance to polyline
            let mut min_dist = f32::MAX;
            for i in 0..points.len() - 1 {
                let d = dist_to_segment(nx, ny, points[i], points[i + 1]);
                if d < min_dist {
                    min_dist = d;
                }
            }
            let pixel_dist = min_dist * draw_size;

            if pixel_dist > aa_outer {
                continue;
            }

            let alpha = if pixel_dist <= aa_inner {
                1.0
            } else {
                let t = (pixel_dist - aa_inner) / (aa_outer - aa_inner);
                let t_smooth = t * t * (3.0 - 2.0 * t); // smoothstep
                1.0 - t_smooth
            };

            pixels[py * size + px] = (alpha * 255.0 + 0.5) as u8;
        }
    }
    pixels
}

/// Shortest distance from point to line segment in normalized space.
/// Ported from Unity DriverWaveformIcons.DistToSegment.
fn dist_to_segment(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let abx = b.0 - a.0;
    let aby = b.1 - a.1;
    let len_sq = abx * abx + aby * aby;

    if len_sq < 0.000001 {
        let dx = px - a.0;
        let dy = py - a.1;
        return (dx * dx + dy * dy).sqrt();
    }

    let t = ((px - a.0) * abx + (py - a.1) * aby) / len_sq;
    let t = t.clamp(0.0, 1.0);

    let cx = a.0 + abx * t - px;
    let cy = a.1 + aby * t - py;
    (cx * cx + cy * cy).sqrt()
}

// ─── TextCommand ─────────────────────────────────────────────────────────────

struct TextCommand {
    x: f32,
    y: f32,
    /// Byte offset into `text_arena`.
    text_offset: u32,
    /// Byte length in `text_arena`.
    text_len: u16,
    font_size: f32,
    color: [u8; 4],
    font_weight: FontWeight,
    clip_bounds: Option<[f32; 4]>,
    depth: Depth,
}

/// Command to draw an icon from the atlas.
struct IconCommand {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    icon_id: u8,
    color: [u8; 4],
    clip_bounds: Option<[f32; 4]>,
    depth: Depth,
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
/// API mirrors UIRenderer's text methods.
pub struct NativeTextRenderer {
    font_manager: FontManager,
    atlas: GlyphAtlas,
    pipeline: GpuRenderPipeline,
    sampler: GpuSampler,
    /// Pre-rendered waveform icons injected into the atlas at startup.
    /// Re-injected after atlas overflow clears the pixel buffer.
    icon_infos: [Option<IconInfo>; ICON_COUNT],
    /// Retained waveform bitmaps for re-injection after atlas clear.
    icon_bitmaps: [Vec<u8>; ICON_COUNT],

    // Draw queues.
    commands: Vec<TextCommand>,
    icon_commands: Vec<IconCommand>,
    /// Per-frame string arena for TextCommand text. Cleared (not deallocated)
    /// each frame. TextCommands store (offset, len) into this buffer.
    text_arena: String,

    // Ring-buffered GPU buffers — prevents aliasing between prepare/commit
    // cycles within the same frame AND across frames in flight.
    vbuf_ring: Vec<Option<GpuBuffer>>,
    ibuf_ring: Vec<Option<GpuBuffer>>,
    ring_idx: usize,
    prepared_slot: usize,
    prepared_index_count: u32,
    prepared_globals: [f32; 4], // [viewport_w, viewport_h, offset_x, offset_y]

    // CPU scratch (reused each frame).
    vertices: Vec<TextVertex>,
    indices: Vec<u32>,

    /// Per-depth (depth, first_index, index_count) ranges into the prepared
    /// index buffer, for text quads and icon quads respectively, ascending by
    /// depth. Commands are stable-sorted by depth during prepare so each
    /// depth's quads are contiguous; `render_depth_in_pass` draws just one
    /// depth's ranges.
    text_depth_ranges: Vec<(Depth, u32, u32)>,
    icon_depth_ranges: Vec<(Depth, u32, u32)>,
    /// Distinct depths that have text or icons this frame, ascending. The
    /// owning `UIRenderer` merges this into its render walk.
    depths: Vec<Depth>,

    /// Measurement cache: u64 hash of (text, font_size, weight) → Vec2.
    /// Uses hash keys to avoid String allocations on cache hits.
    measure_cache: AHashMap<u64, Vec2>,
    frame_generation: u64,
}

impl NativeTextRenderer {
    /// Create the renderer. Call once at startup.
    pub fn new(device: &GpuDevice, format: GpuTextureFormat) -> Self {
        let font_manager = FontManager::new();
        let mut atlas = GlyphAtlas::new(device);

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

        // Generate and inject waveform icons into the atlas.
        // Bitmaps are retained so they can be re-injected after atlas overflow.
        let icon_bitmaps = generate_waveform_icons();
        let mut icon_infos = [None; ICON_COUNT];
        for (i, bmp) in icon_bitmaps.iter().enumerate() {
            icon_infos[i] = Some(atlas.inject_icon(bmp, ICON_SIZE, ICON_SIZE));
        }
        // Upload icons to GPU immediately.
        atlas.upload_if_dirty(device);

        const TEXT_RING_SIZE: usize = 32;
        let vbuf_ring = (0..TEXT_RING_SIZE).map(|_| None).collect();
        let ibuf_ring = (0..TEXT_RING_SIZE).map(|_| None).collect();

        Self {
            font_manager,
            atlas,
            pipeline,
            sampler,
            icon_infos,
            icon_bitmaps,
            commands: Vec::new(),
            icon_commands: Vec::new(),
            text_arena: String::with_capacity(4096),
            vbuf_ring,
            ibuf_ring,
            ring_idx: 0,
            prepared_slot: 0,
            prepared_index_count: 0,
            prepared_globals: [0.0; 4],
            vertices: Vec::new(),
            indices: Vec::new(),
            text_depth_ranges: Vec::with_capacity(8),
            icon_depth_ranges: Vec::with_capacity(8),
            depths: Vec::with_capacity(8),
            measure_cache: AHashMap::new(),
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
        depth: Depth,
    ) {
        if text.is_empty() {
            return;
        }
        let text_offset = self.text_arena.len() as u32;
        let text_len = text.len().min(u16::MAX as usize) as u16;
        self.text_arena.push_str(text);
        self.commands.push(TextCommand {
            x,
            y,
            text_offset,
            text_len,
            font_size,
            color,
            font_weight,
            clip_bounds,
            depth,
        });
    }

    /// Accurate text measurement via CoreText CTLine. Cached.
    pub fn measure_text_cached(
        &mut self,
        text: &str,
        font_size: u16,
        font_weight: FontWeight,
    ) -> Vec2 {
        // Use a u64 hash key to avoid allocating a String for cache lookups.
        // Most frames hit the cache (labels don't change), so this eliminates
        // 200-500 String allocations per frame on cache hits.
        use std::hash::{Hash, Hasher};
        let mut hasher = ahash::AHasher::default();
        text.hash(&mut hasher);
        font_size.hash(&mut hasher);
        font_weight.hash(&mut hasher);
        let hash = hasher.finish();

        if let Some(&size) = self.measure_cache.get(&hash) {
            return size;
        }

        let size = measure_text_ct(&mut self.font_manager, text, font_size as f32, font_weight);

        if self.measure_cache.len() >= MAX_MEASURE_CACHE {
            self.measure_cache.clear();
        }
        self.measure_cache.insert(hash, size);
        size
    }

    /// Advance the frame counter and cap measurement cache size.
    pub fn begin_frame(&mut self) {
        self.frame_generation += 1;
        // Cap cache size — clear when it gets too large (rare).
        if self.measure_cache.len() > MAX_MEASURE_CACHE * 2 {
            self.measure_cache.clear();
        }
    }

    /// Queue an icon draw command.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_icon(
        &mut self,
        icon_id: u8,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: [u8; 4],
        clip_bounds: Option<[f32; 4]>,
        depth: Depth,
    ) {
        if (icon_id as usize) < ICON_COUNT && self.icon_infos[icon_id as usize].is_some() {
            self.icon_commands.push(IconCommand {
                x,
                y,
                w,
                h,
                icon_id,
                color,
                clip_bounds,
                depth,
            });
        }
    }

    /// Clear the draw queues (call after render()).
    pub fn clear_commands(&mut self) {
        self.commands.clear();
        self.icon_commands.clear();
        self.text_arena.clear(); // reuses capacity
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
        self.text_depth_ranges.clear();
        self.icon_depth_ranges.clear();
        self.depths.clear();

        if self.commands.is_empty() && self.icon_commands.is_empty() {
            return false;
        }

        self.prepared_globals = [viewport_w as f32, viewport_h as f32, offset_x, offset_y];
        let scale = scale_factor as f32;

        let mut commands: Vec<TextCommand> = std::mem::take(&mut self.commands);
        let arena = std::mem::take(&mut self.text_arena);
        // Stable sort: within a depth, insertion order is preserved, so a
        // single-depth frame builds the identical buffer to the pre-depth
        // renderer.
        commands.sort_by_key(|c| c.depth);

        for cmd in &commands {
            let range_start = self.indices.len() as u32;
            let text = &arena
                [cmd.text_offset as usize..(cmd.text_offset as usize + cmd.text_len as usize)];
            let physical_size = cmd.font_size * scale;
            let size_x10 = (physical_size * 10.0).round() as u16;

            let ct_font = self
                .font_manager
                .get_ct_font(physical_size, cmd.font_weight);
            let glyphs_and_positions = shape_line(ct_font, text);
            let Some((glyph_ids, positions_ct)) = glyphs_and_positions else {
                continue;
            };

            // cmd.y is the top of the text bounding box (vertically centered by
            // UIRenderer). The measurement height = font_size (logical px).
            // Place baseline so the glyph content is centered within that box:
            //   baseline = cmd.y + font_size * BASELINE_FRACTION
            // where BASELINE_FRACTION positions the baseline within the em square.
            // For Inter, ~0.76 places ascenders near the top and descenders near
            // the bottom of the font_size box.
            let baseline_y = cmd.y + cmd.font_size * 0.76;

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

                // Clip glyph to clip bounds — partial pixel-level clipping.
                // Adjusts quad positions and UVs so glyphs are cut cleanly
                // at the clip boundary instead of popping in/out whole.
                let (mut qx0, mut qy0, mut qx1, mut qy1) = (x0, y0, x1, y1);
                let (mut u0, mut v0) = (info.uv_x, info.uv_y);
                let (mut u1, mut v1) = (info.uv_x + info.uv_w, info.uv_y + info.uv_h);
                if let Some([clip_x0, clip_y0, clip_x1, clip_y1]) = cmd.clip_bounds {
                    if x1 <= clip_x0 || y1 <= clip_y0 || x0 >= clip_x1 || y0 >= clip_y1 {
                        continue;
                    }
                    let gw = x1 - x0;
                    let gh = y1 - y0;
                    if qx0 < clip_x0 {
                        u0 = info.uv_x + (clip_x0 - x0) / gw * info.uv_w;
                        qx0 = clip_x0;
                    }
                    if qy0 < clip_y0 {
                        v0 = info.uv_y + (clip_y0 - y0) / gh * info.uv_h;
                        qy0 = clip_y0;
                    }
                    if qx1 > clip_x1 {
                        u1 = info.uv_x + (clip_x1 - x0) / gw * info.uv_w;
                        qx1 = clip_x1;
                    }
                    if qy1 > clip_y1 {
                        v1 = info.uv_y + (clip_y1 - y0) / gh * info.uv_h;
                        qy1 = clip_y1;
                    }
                }

                let base = self.vertices.len() as u32;
                self.vertices.push(TextVertex {
                    position: [qx0, qy0],
                    uv: [u0, v0],
                    color,
                });
                self.vertices.push(TextVertex {
                    position: [qx1, qy0],
                    uv: [u1, v0],
                    color,
                });
                self.vertices.push(TextVertex {
                    position: [qx1, qy1],
                    uv: [u1, v1],
                    color,
                });
                self.vertices.push(TextVertex {
                    position: [qx0, qy1],
                    uv: [u0, v1],
                    color,
                });
                self.indices.extend_from_slice(&[
                    base,
                    base + 1,
                    base + 2,
                    base,
                    base + 2,
                    base + 3,
                ]);
            }

            // Extend this depth's text range (commands are depth-sorted, so the
            // range stays contiguous). Skip if this command emitted no glyphs.
            let added = self.indices.len() as u32 - range_start;
            if added > 0 {
                match self.text_depth_ranges.last_mut() {
                    Some((d, _, count)) if *d == cmd.depth => *count += added,
                    _ => self
                        .text_depth_ranges
                        .push((cmd.depth, range_start, added)),
                }
            }
        }

        // Restore arena capacity for next frame (contents will be cleared in clear_commands).
        self.text_arena = arena;

        // If the atlas was cleared during glyph rasterization above, the waveform
        // icon pixels were destroyed. Re-inject them so icon_infos point to valid
        // atlas regions again.
        if self.atlas.was_cleared {
            for (i, bmp) in self.icon_bitmaps.iter().enumerate() {
                self.icon_infos[i] = Some(self.atlas.inject_icon(bmp, ICON_SIZE, ICON_SIZE));
            }
            self.atlas.was_cleared = false;
        }

        // Emit quads for icon commands (depth-sorted, like text above).
        let mut icon_cmds: Vec<IconCommand> = std::mem::take(&mut self.icon_commands);
        icon_cmds.sort_by_key(|c| c.depth);
        for cmd in &icon_cmds {
            let range_start = self.indices.len() as u32;
            let Some(info) = self.icon_infos[cmd.icon_id as usize] else {
                continue;
            };
            let color = [
                cmd.color[0] as f32 / 255.0,
                cmd.color[1] as f32 / 255.0,
                cmd.color[2] as f32 / 255.0,
                cmd.color[3] as f32 / 255.0,
            ];
            let (x0, y0, x1, y1) = (cmd.x, cmd.y, cmd.x + cmd.w, cmd.y + cmd.h);

            // Clip icon to clip bounds — partial pixel-level clipping.
            let (mut qx0, mut qy0, mut qx1, mut qy1) = (x0, y0, x1, y1);
            let (mut u0, mut v0) = (info.uv_x, info.uv_y);
            let (mut u1, mut v1) = (info.uv_x + info.uv_w, info.uv_y + info.uv_h);
            if let Some([clip_x0, clip_y0, clip_x1, clip_y1]) = cmd.clip_bounds {
                if x1 <= clip_x0 || y1 <= clip_y0 || x0 >= clip_x1 || y0 >= clip_y1 {
                    continue;
                }
                let gw = x1 - x0;
                let gh = y1 - y0;
                if qx0 < clip_x0 {
                    u0 = info.uv_x + (clip_x0 - x0) / gw * info.uv_w;
                    qx0 = clip_x0;
                }
                if qy0 < clip_y0 {
                    v0 = info.uv_y + (clip_y0 - y0) / gh * info.uv_h;
                    qy0 = clip_y0;
                }
                if qx1 > clip_x1 {
                    u1 = info.uv_x + (clip_x1 - x0) / gw * info.uv_w;
                    qx1 = clip_x1;
                }
                if qy1 > clip_y1 {
                    v1 = info.uv_y + (clip_y1 - y0) / gh * info.uv_h;
                    qy1 = clip_y1;
                }
            }

            let base = self.vertices.len() as u32;
            self.vertices.push(TextVertex {
                position: [qx0, qy0],
                uv: [u0, v0],
                color,
            });
            self.vertices.push(TextVertex {
                position: [qx1, qy0],
                uv: [u1, v0],
                color,
            });
            self.vertices.push(TextVertex {
                position: [qx1, qy1],
                uv: [u1, v1],
                color,
            });
            self.vertices.push(TextVertex {
                position: [qx0, qy1],
                uv: [u0, v1],
                color,
            });
            self.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);

            let added = self.indices.len() as u32 - range_start;
            if added > 0 {
                match self.icon_depth_ranges.last_mut() {
                    Some((d, _, count)) if *d == cmd.depth => *count += added,
                    _ => self
                        .icon_depth_ranges
                        .push((cmd.depth, range_start, added)),
                }
            }
        }

        // Union of text and icon depths, ascending — handed to the UIRenderer
        // so it visits every depth that carries glyphs.
        self.depths.extend(self.text_depth_ranges.iter().map(|r| r.0));
        self.depths.extend(self.icon_depth_ranges.iter().map(|r| r.0));
        self.depths.sort_unstable();
        self.depths.dedup();

        if self.vertices.is_empty() {
            return false;
        }

        self.atlas.upload_if_dirty(device);

        // Ring-buffered GPU buffers — see UIRenderer for details.
        let ring_size = self.vbuf_ring.len();
        let slot = self.ring_idx % ring_size;
        self.ring_idx += 1;

        let vdata = bytemuck::cast_slice::<TextVertex, u8>(&self.vertices);
        let vbuf = match self.vbuf_ring[slot].take() {
            Some(buf) if buf.size >= vdata.len() as u64 => buf,
            _ => device.create_buffer_shared(vdata.len() as u64),
        };
        unsafe {
            vbuf.write(0, vdata);
        }

        let idata = bytemuck::cast_slice::<u32, u8>(&self.indices);
        let ibuf = match self.ibuf_ring[slot].take() {
            Some(buf) if buf.size >= idata.len() as u64 => buf,
            _ => device.create_buffer_shared(idata.len() as u64),
        };
        unsafe {
            ibuf.write(0, idata);
        }

        self.vbuf_ring[slot] = Some(vbuf);
        self.ibuf_ring[slot] = Some(ibuf);
        self.prepared_slot = slot;
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
        let vbuf = self.vbuf_ring[self.prepared_slot].as_ref().unwrap();
        let ibuf = self.ibuf_ring[self.prepared_slot].as_ref().unwrap();

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
            vbuf,
            0,
            ibuf,
            self.prepared_index_count,
            None,
            load_action,
            "TextRenderer",
        );
    }

    /// Distinct depths carrying text or icons this frame, ascending. The
    /// owning `UIRenderer` merges this into its depth walk so a text-only
    /// depth is still visited.
    pub fn depths(&self) -> &[Depth] {
        &self.depths
    }

    /// Draw one depth's text + icon quads into an already-active render pass.
    /// A no-op for a depth with no glyphs.
    pub fn render_depth_in_pass(&self, encoder: &mut GpuEncoder, depth: Depth) {
        if self.prepared_index_count == 0 {
            return;
        }
        let Some(ref atlas_texture) = self.atlas.texture else {
            return;
        };
        let range_for = |ranges: &[(Depth, u32, u32)]| {
            ranges
                .iter()
                .find(|(d, _, _)| *d == depth)
                .map(|&(_, first, count)| (first, count))
                .unwrap_or((0, 0))
        };
        let text_range = range_for(&self.text_depth_ranges);
        let icon_range = range_for(&self.icon_depth_ranges);
        if text_range.1 == 0 && icon_range.1 == 0 {
            return;
        }
        let vbuf = self.vbuf_ring[self.prepared_slot].as_ref().unwrap();
        let ibuf = self.ibuf_ring[self.prepared_slot].as_ref().unwrap();
        let bindings = [
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
        ];
        for (first, count) in [text_range, icon_range] {
            if count == 0 {
                continue;
            }
            encoder.draw_in_render_pass(
                &self.pipeline,
                &bindings,
                vbuf,
                0,
                ibuf,
                count,
                (first as usize * std::mem::size_of::<u32>()) as u64,
                None,
                "TextRenderer",
            );
        }
    }

    /// Draw text into an already-active render pass.
    pub fn render_in_pass(&self, encoder: &mut GpuEncoder) {
        if self.prepared_index_count == 0 {
            return;
        }
        let Some(ref atlas_texture) = self.atlas.texture else {
            return;
        };
        let vbuf = self.vbuf_ring[self.prepared_slot].as_ref().unwrap();
        let ibuf = self.ibuf_ring[self.prepared_slot].as_ref().unwrap();

        encoder.draw_in_render_pass(
            &self.pipeline,
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
            vbuf,
            0,
            ibuf,
            self.prepared_index_count,
            0,
            None,
            "TextRenderer",
        );
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

/// Accurate, GPU-free text measurer for the UI build path.
///
/// Wraps only the CoreText [`FontManager`] — no atlas, no GPU device — so the
/// app can construct one and install it on a [`UITree`](manifold_ui::tree::UITree)
/// via `set_text_measure`, giving panels real glyph-width measurement at build
/// time instead of the [`HeuristicTextMeasure`](manifold_ui::text::HeuristicTextMeasure)
/// fallback. Font lookup caches, the only mutation, lives behind a `RefCell` so
/// measurement satisfies `TextMeasure`'s `&self` contract.
pub struct CoreTextMeasure {
    fonts: RefCell<FontManager>,
}

impl CoreTextMeasure {
    pub fn new() -> Self {
        Self {
            fonts: RefCell::new(FontManager::new()),
        }
    }
}

impl Default for CoreTextMeasure {
    fn default() -> Self {
        Self::new()
    }
}

impl TextMeasure for CoreTextMeasure {
    fn measure_text(&self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2 {
        measure_text_ct(&mut self.fonts.borrow_mut(), text, font_size as f32, font_weight)
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
    // Use font_size as height (matches glyphon behavior). CTLine's ascent+descent
    // includes inflated hhea line metrics which are too tall for UI centering.
    Vec2::new(w, font_size)
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
        assert!(
            bounds.width > 0.0,
            "text width should be positive, got {}",
            bounds.width
        );
        assert!(
            bounds.ascent > 0.0,
            "ascent should be positive, got {}",
            bounds.ascent
        );
        assert!(
            bounds.descent >= 0.0,
            "descent should be non-negative, got {}",
            bounds.descent
        );
        let height = bounds.ascent + bounds.descent;
        assert!(
            height >= 8.0,
            "line height should be at least 8px, got {height}"
        );
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
        // (`ct_font` is a &CTFont; the borrow on `fm` ends at its
        // last use above — no explicit drop needed.)

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

        assert_eq!(
            rects.len(),
            glyph_chars.len(),
            "all glyphs should be in atlas"
        );

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
