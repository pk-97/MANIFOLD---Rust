#![allow(private_interfaces)]

//! `node.value_overlay` — lightweight bitmap-font numeric
//! labels at multiple positions, composited onto a source texture.
//!
//! Renders one text label per item in the `positions` array using an
//! embedded 5×7 pixel glyph atlas (0-9, A-F, X, Y, period, colon,
//! percent — same glyphs as the legacy BlobTrackingFX). The `format`
//! param selects how to generate the displayed text:
//!
//! - **Index**: decimal array index (0, 1, 2, …)
//! - **Hex**: "0X" + hex of array index (0X00, 0X01, …)
//! - **Coord**: "xxx,yyy" from the item's X, Y × 999
//! - **Float3**: first value channel × 1000, 3 digits
//!
//! First consumer: Blob Track HUD (hex IDs, coord labels, distance
//! labels). Reusable for any data-viz numeric annotation layer.

use std::borrow::Cow;

use manifold_gpu::{
    GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuFilterMode, GpuLoadAction,
    GpuRenderPipeline, GpuSampler, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
    GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const MAX_QUADS: usize = 512;
const FORMATS: &[&str] = &["Index", "Hex", "Coord", "Float3"];
const ANCHORS: &[&str] = &["TopLeft", "TopRight", "BottomLeft", "BottomRight", "Center"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlyphQuad {
    rect: [f32; 4],       // clip-space x0, y0, x1, y1
    atlas_rect: [f32; 4], // atlas UVs u0, v0, u1, v1
    alpha: f32,
    _pad: [f32; 3],
}

const _: () = assert!(std::mem::size_of::<GlyphQuad>() == 48);

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayUniforms {
    color: [f32; 3],
    alpha: f32,
}

fn glyph_atlas_rect(char_code: f32) -> [f32; 4] {
    let c = (char_code + 0.5).floor();
    let col = (c % 16.0).floor();
    let row = (c / 16.0).floor();
    let u0 = (col * 5.0) / 80.0;
    let v0 = (row * 7.0) / 14.0;
    let u1 = (col * 5.0 + 5.0) / 80.0;
    let v1 = (row * 7.0 + 7.0) / 14.0;
    [u0, v0, u1, v1]
}

fn uv_to_clip(x: f32, y: f32) -> (f32, f32) {
    // UV convention: (0, 0) is top-left of the image, y grows downward.
    // Clip-space convention: (-1, -1) is bottom-left, y grows upward.
    // The Y mapping needs the sign flip; X is a direct linear remap.
    (x * 2.0 - 1.0, 1.0 - y * 2.0)
}

fn push_glyph(
    quads: &mut Vec<GlyphQuad>,
    x0: f32, y0: f32, x1: f32, y1: f32,
    char_code: f32, alpha: f32,
) {
    if quads.len() >= MAX_QUADS {
        return;
    }
    let (cx0, cy0) = uv_to_clip(x0, y0);
    let (cx1, cy1) = uv_to_clip(x1, y1);
    quads.push(GlyphQuad {
        rect: [cx0, cy0, cx1, cy1],
        atlas_rect: glyph_atlas_rect(char_code),
        alpha,
        _pad: [0.0; 3],
    });
}

fn push_digits_3(
    quads: &mut Vec<GlyphQuad>,
    value: f32, x: f32, y: f32,
    glyph_w: f32, glyph_h: f32, digit_w: f32, alpha: f32,
) {
    let v = value.floor().clamp(0.0, 999.0);
    let h = (v / 100.0).floor();
    let t = ((v % 100.0) / 10.0).floor();
    let o = v % 10.0;
    for &(ch, off) in &[(h, 0.0_f32), (t, 6.0), (o, 12.0)] {
        let gx = x + off * digit_w;
        push_glyph(quads, gx, y, gx + glyph_w, y + glyph_h, ch, alpha);
    }
}

crate::primitive! {
    name: RenderValueOverlay,
    type_id: "node.value_overlay",
    purpose: "Lightweight bitmap-font numeric labels at multiple positions, composited onto a source texture. Embedded 5×7 glyph atlas (0-9, A-F, hex, coords). Format enum selects display: Index (decimal), Hex (0Xnn), Coord (xxx,yyy from X/Y channels), Float3 (value×1000). For diagnostic HUDs, data-viz annotation layers, debug overlays.",
    inputs: {
        in: Texture2D required,
        positions: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32] required,
        values: Channels[VALUE: F32] optional,
        alpha: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("format"),
            label: "Format",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, (FORMATS.len() - 1) as f32)),
            enum_values: FORMATS,
        },
        ParamDef {
            name: Cow::Borrowed("color"),
            label: "Color",
            ty: ParamType::Color,
            default: ParamValue::Color([0.85, 0.92, 1.0, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("alpha"),
            label: "Alpha",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("font_scale"),
            label: "Font Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.25, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("label_count"),
            label: "Label Count",
            ty: ParamType::Int,
            default: ParamValue::Float(32.0),
            range: Some((0.0, 256.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_x"),
            label: "Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-0.5, 0.5)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_y"),
            label: "Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-0.5, 0.5)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("anchor"),
            label: "Anchor",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, (ANCHORS.len() - 1) as f32)),
            enum_values: ANCHORS,
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire detection regions (Channels[X, Y, WIDTH, HEIGHT]) into `positions`; optionally wire a Channels[VALUE: F32] into `values` for Float3 format. Labels are positioned at each item's (X, Y) + (offset_x, offset_y). format=Index shows array index, Hex shows '0X' + hex, Coord shows 'xxx,yyy' from X/Y × 999, Float3 shows value × 1000 as 3 digits. For the Blob Track HUD, use multiple instances with different formats and offsets.",
    examples: [],
    picker: { label: "Value Overlay", category: Atom },
    summary: "Prints small numeric labels onto the image at given spots using a built-in font. A quick readout for values flowing through a graph.",
    category: Generate,
    role: Filter,
    aliases: ["value overlay", "render value overlay", "debug text", "readout", "numbers"],
    boundary_reason: DrawCall,
    extra_fields: {
        render_pipeline: Option<GpuRenderPipeline> = None,
        font_atlas: Option<GpuTexture> = None,
        font_sampler: Option<GpuSampler> = None,
        quad_buf: Option<manifold_gpu::GpuBuffer> = None,
        quads: Vec<GlyphQuad> = Vec::new(),
    },
}

fn create_font_atlas(device: &manifold_gpu::GpuDevice) -> GpuTexture {
    const GW: usize = 5;
    const GH: usize = 7;
    const COLS: usize = 16;
    const ROWS: usize = 2;
    let tex_w = COLS * GW;
    let tex_h = ROWS * GH;

    let glyphs: &[&[&str]] = &[
        &[".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###."], // 0
        &["..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###."], // 1
        &[".###.", "#...#", "....#", "..##.", ".#...", "#....", "#####"], // 2
        &[".###.", "#...#", "....#", "..##.", "....#", "#...#", ".###."], // 3
        &["...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#."], // 4
        &["#####", "#....", "####.", "....#", "....#", "#...#", ".###."], // 5
        &[".###.", "#....", "#....", "####.", "#...#", "#...#", ".###."], // 6
        &["#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#..."], // 7
        &[".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###."], // 8
        &[".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##.."], // 9
        &[".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"], // A
        &["####.", "#...#", "#...#", "####.", "#...#", "#...#", "####."], // B
        &[".###.", "#...#", "#....", "#....", "#....", "#...#", ".###."], // C
        &["####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####."], // D
        &["#####", "#....", "#....", "####.", "#....", "#....", "#####"], // E
        &["#####", "#....", "#....", "####.", "#....", "#....", "#...."], // F
        &["#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#"], // X (16)
        &["#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#.."], // Y (17)
        &[".....", ".....", ".....", ".....", ".....", ".....", "..#.."], // . (18)
        &[".....", "..#..", "..#..", ".....", "..#..", "..#..", "....."], // : (19)
        &["##..#", "##.#.", "..#..", "..#..", "..#..", ".#.##", "#..##"], // % (20)
    ];

    let mut pixels = vec![[0u8; 4]; tex_w * tex_h];
    for (c, glyph) in glyphs.iter().enumerate() {
        let base_x = (c % COLS) * GW;
        let base_y = (c / COLS) * GH;
        for row in 0..GH {
            let tex_y = base_y + (GH - 1 - row);
            let line = glyph[row];
            for col in 0..GW {
                if col < line.len() && line.as_bytes()[col] == b'#' {
                    pixels[tex_y * tex_w + base_x + col] = [255, 255, 255, 255];
                }
            }
        }
    }

    let texture = device.create_texture(&GpuTextureDesc {
        width: tex_w as u32,
        height: tex_h as u32,
        depth: 1,
        format: GpuTextureFormat::Rgba8Unorm,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_DST
            | GpuTextureUsage::CPU_UPLOAD,
        label: "render_value_overlay FontAtlas",
        mip_levels: 1,
    });
    let flat: Vec<u8> = pixels.iter().flat_map(|p| p.iter().copied()).collect();
    device.upload_texture(&texture, &flat);
    texture
}

const OVERLAY_SHADER: &str = r#"
struct Uniforms {
    color: vec3<f32>,
    alpha: f32,
};

struct GlyphQuad {
    rect: vec4<f32>,
    atlas_rect: vec4<f32>,
    alpha_val: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> quads: array<GlyphQuad>;
@group(0) @binding(2) var font_tex: texture_2d<f32>;
@group(0) @binding(3) var font_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha_val: f32,
    @location(2) has_atlas: f32,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32, @builtin(instance_index) iid: u32) -> VsOut {
    let q = quads[iid];
    var cx: f32; var cy: f32;
    var tu: f32; var tv: f32;
    switch vid {
        case 0u: { cx = q.rect.x; cy = q.rect.y; tu = q.atlas_rect.x; tv = q.atlas_rect.w; }
        case 1u: { cx = q.rect.z; cy = q.rect.y; tu = q.atlas_rect.z; tv = q.atlas_rect.w; }
        case 2u: { cx = q.rect.x; cy = q.rect.w; tu = q.atlas_rect.x; tv = q.atlas_rect.y; }
        case 3u: { cx = q.rect.x; cy = q.rect.w; tu = q.atlas_rect.x; tv = q.atlas_rect.y; }
        case 4u: { cx = q.rect.z; cy = q.rect.y; tu = q.atlas_rect.z; tv = q.atlas_rect.w; }
        default: { cx = q.rect.z; cy = q.rect.w; tu = q.atlas_rect.z; tv = q.atlas_rect.y; }
    }
    let has_a = select(0.0, 1.0, q.atlas_rect.x != 0.0 || q.atlas_rect.y != 0.0 || q.atlas_rect.z != 0.0 || q.atlas_rect.w != 0.0);
    return VsOut(vec4<f32>(cx, cy, 0.0, 1.0), vec2<f32>(tu, tv), q.alpha_val, has_a);
}

@fragment
fn fs_main(v: VsOut) -> @location(0) vec4<f32> {
    var a = v.alpha_val * u.alpha;
    if v.has_atlas > 0.5 {
        let tex_a = textureSample(font_tex, font_sampler, v.uv).a;
        a *= tex_a;
    }
    return vec4<f32>(u.color * a, 0.0);
}
"#;

impl Primitive for RenderValueOverlay {
    // Data-driven skip: zero detections means zero labels — the executor
    // aliases `in` → `out` (live source flows through at zero GPU cost)
    // while the wired positions array stays empty.
    fn empty_skip_input_ports(&self) -> &'static [&'static str] {
        &["positions"]
    }

    fn skip_passthrough_ports(&self) -> Option<(&'static str, &'static str)> {
        Some(("in", "out"))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let format_idx = match ctx.params.get("format") {
            Some(ParamValue::Enum(v)) => *v,
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => 0,
        };
        let color = match ctx.params.get("color") {
            Some(ParamValue::Color(c)) => [c[0], c[1], c[2]],
            _ => [0.85, 0.92, 1.0],
        };
        let alpha = ctx.scalar_or_param("alpha", 1.0);
        let font_scale = match ctx.params.get("font_scale") {
            Some(ParamValue::Float(f)) => f.max(0.1),
            _ => 1.0,
        };
        let label_count = match ctx.params.get("label_count") {
            Some(ParamValue::Float(f)) => f.round().max(0.0) as usize,
            _ => 32,
        };
        let offset_x = match ctx.params.get("offset_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let offset_y = match ctx.params.get("offset_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let anchor_idx = match ctx.params.get("anchor") {
            Some(ParamValue::Enum(v)) => *v,
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => 0,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else { return };
        let Some(pos_buf) = ctx.inputs.array("positions") else { return };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else { return };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 || label_count == 0 { return }

        let item_size = 16u64;
        let pos_capacity = (pos_buf.size / item_size) as usize;
        let pos_ptr = pos_buf.mapped_ptr()
            .expect("render_value_overlay: positions must be shared-memory");
        let pos_floats: &[f32] = unsafe {
            std::slice::from_raw_parts(pos_ptr as *const f32, pos_capacity * 4)
        };

        let values_buf = ctx.inputs.array("values");
        let val_floats: Option<&[f32]> = values_buf.map(|vb| {
            let vp = vb.mapped_ptr().expect("shared-memory");
            let n = (vb.size / 4) as usize;
            unsafe { std::slice::from_raw_parts(vp as *const f32, n) }
        });

        let dpi_scale = h as f32 / 1080.0;
        let px_u = (1.0 / w as f32) * dpi_scale * font_scale;
        let px_v = (1.0 / h as f32) * dpi_scale * font_scale;
        let digit_w = px_u;
        let digit_h = px_v;
        let glyph_w = 5.0 * digit_w;
        let glyph_h = 7.0 * digit_h;

        self.quads.clear();
        let count = label_count.min(pos_capacity);

        for i in 0..count {
            if self.quads.len() >= MAX_QUADS { break }
            let px = pos_floats[i * 4];
            let py = pos_floats[i * 4 + 1];
            let pw = pos_floats[i * 4 + 2];
            let ph = pos_floats[i * 4 + 3];
            if pw <= 0.0001 && ph <= 0.0001 { continue }

            // Anchor: select which corner / center of the (px, py, pw, ph)
            // rectangle the label's top-left text origin sits on. Static
            // offset_x / offset_y stack on top after anchor resolution.
            let (anchor_x, anchor_y) = match anchor_idx {
                1 => (px + pw, py),            // TopRight
                2 => (px, py + ph),            // BottomLeft
                3 => (px + pw, py + ph),       // BottomRight
                4 => (px + pw * 0.5, py + ph * 0.5), // Center
                _ => (px, py),                  // TopLeft (default)
            };
            let lx = anchor_x + offset_x;
            let ly = anchor_y + offset_y;

            match format_idx {
                0 => {
                    // Index: decimal 3-digit
                    push_digits_3(&mut self.quads, i as f32, lx, ly,
                        glyph_w, glyph_h, digit_w, 1.0);
                }
                1 => {
                    // Hex: "0X" + 2 hex digits
                    let hex_id = (i as f32 * 17.0 + 48.0).floor().clamp(0.0, 255.0);
                    let hi = (hex_id / 16.0).floor();
                    let lo = hex_id % 16.0;
                    for &(ch, px_off) in &[
                        (0.0_f32, 0.0_f32), (16.0, 6.0), (hi, 13.0), (lo, 19.0),
                    ] {
                        let gx = lx + px_off * digit_w;
                        push_glyph(&mut self.quads, gx, ly, gx + glyph_w, ly + glyph_h, ch, 1.0);
                    }
                }
                2 => {
                    // Coord: "xxx,yyy" — show the rect's CENTER (matches
                    // legacy BlobTrackingFX, where the label position
                    // was anchored at a corner but the displayed coords
                    // were the smoothed centre).
                    let cx = px + pw * 0.5;
                    let cy = py + ph * 0.5;
                    let x_val = (cx * 999.0).floor().clamp(0.0, 999.0);
                    let y_val = (cy * 999.0).floor().clamp(0.0, 999.0);
                    let x_h = (x_val / 100.0).floor();
                    let x_t = ((x_val % 100.0) / 10.0).floor();
                    let x_o = x_val % 10.0;
                    let y_h = (y_val / 100.0).floor();
                    let y_t = ((y_val % 100.0) / 10.0).floor();
                    let y_o = y_val % 10.0;
                    for &(ch, px_off) in &[
                        (x_h, 0.0_f32), (x_t, 6.0), (x_o, 12.0),
                        (18.0, 17.0), // separator (period char code 18)
                        (y_h, 22.0), (y_t, 28.0), (y_o, 34.0),
                    ] {
                        let gx = lx + px_off * digit_w;
                        push_glyph(&mut self.quads, gx, ly, gx + glyph_w, ly + glyph_h, ch, 1.0);
                    }
                }
                _ => {
                    // Float3: value × 1000, 3 digits
                    let v = val_floats
                        .and_then(|vf| vf.get(i).copied())
                        .unwrap_or(0.0);
                    push_digits_3(&mut self.quads, v * 1000.0, lx, ly,
                        glyph_w, glyph_h, digit_w, 1.0);
                }
            }
        }

        let gpu = ctx.gpu_encoder();

        gpu.copy_texture_to_texture(in_tex, out_tex, w, h);

        let quad_count = self.quads.len();
        if quad_count == 0 { return }

        let font_atlas = self.font_atlas.get_or_insert_with(|| create_font_atlas(gpu.device));
        let point_sampler = self.font_sampler.get_or_insert_with(|| {
            gpu.device.create_sampler(&GpuSamplerDesc {
                min_filter: GpuFilterMode::Nearest,
                mag_filter: GpuFilterMode::Nearest,
                ..GpuSamplerDesc::default()
            })
        });
        let quad_buf = self.quad_buf.get_or_insert_with(|| {
            gpu.device.create_buffer_shared(
                (MAX_QUADS * std::mem::size_of::<GlyphQuad>()) as u64,
            )
        });
        let pipeline = self.render_pipeline.get_or_insert_with(|| {
            let blend = GpuBlendState {
                src_factor: GpuBlendFactor::One,
                dst_factor: GpuBlendFactor::One,
                operation: GpuBlendOp::Add,
                src_alpha_factor: GpuBlendFactor::Zero,
                dst_alpha_factor: GpuBlendFactor::One,
                alpha_operation: GpuBlendOp::Add,
            };
            gpu.device.create_render_pipeline(
                OVERLAY_SHADER,
                "vs_main",
                "fs_main",
                GpuTextureFormat::Rgba16Float,
                Some(blend),
                "node.value_overlay",
            )
        });

        let quad_bytes = bytemuck::cast_slice(&self.quads[..quad_count]);
        unsafe { quad_buf.write(0, quad_bytes) };

        let uniforms = OverlayUniforms { color, alpha };
        gpu.native_enc.draw_instanced(
            pipeline,
            out_tex,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: quad_buf, offset: 0 },
                GpuBinding::Texture { binding: 2, texture: font_atlas },
                GpuBinding::Sampler { binding: 3, sampler: point_sampler },
            ],
            6,
            quad_count as u32,
            GpuLoadAction::Load,
            "node.value_overlay",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn render_value_overlay_declares_io() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(RenderValueOverlay::TYPE_ID, "node.value_overlay");
        assert_eq!(RenderValueOverlay::INPUTS.len(), 4);
        assert_eq!(RenderValueOverlay::INPUTS[0].name, "in");
        assert_eq!(RenderValueOverlay::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(RenderValueOverlay::INPUTS[1].name, "positions");
        assert!(matches!(RenderValueOverlay::INPUTS[1].ty, PortType::Array(_)));
        assert_eq!(RenderValueOverlay::INPUTS[2].name, "values");
        assert!(!RenderValueOverlay::INPUTS[2].required);
        assert_eq!(RenderValueOverlay::INPUTS[3].name, "alpha");
        assert_eq!(RenderValueOverlay::INPUTS[3].ty, PortType::Scalar(ScalarType::F32));
        assert!(!RenderValueOverlay::INPUTS[3].required);
        assert_eq!(RenderValueOverlay::OUTPUTS.len(), 1);
    }

    #[test]
    fn render_value_overlay_registers() {
        let prim = RenderValueOverlay::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.value_overlay");
    }

    #[test]
    fn glyph_atlas_rect_returns_valid_uvs() {
        let r = glyph_atlas_rect(0.0); // '0'
        assert!((r[0] - 0.0).abs() < 1e-5);
        assert!((r[2] - 5.0 / 80.0).abs() < 1e-5);
        let r_a = glyph_atlas_rect(10.0); // 'A'
        assert!((r_a[0] - 50.0 / 80.0).abs() < 1e-5);
    }

    #[test]
    fn glyph_quad_is_48_bytes() {
        assert_eq!(std::mem::size_of::<GlyphQuad>(), 48);
    }
}
