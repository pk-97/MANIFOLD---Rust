//! `node.draw_rectangles` — instanced filled-rectangle overlay
//! composited onto a source texture.
//!
//! Each item in the input `Channels[X, Y, WIDTH, HEIGHT]` array
//! draws one axis-aligned filled rectangle on top of the source.
//! Zero-size items (width ≤ 0 AND height ≤ 0) are skipped by the
//! shader. Additive blend so overlapping rects accumulate colour.
//!
//! First consumer: Blob Track HUD (gauge fills, center dots as small
//! rects). Reusable for any rect-shaped overlay: selection boxes,
//! debug regions, VU meters, status bars, beat-grid markers.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuLoadAction,
    GpuRenderPipeline, GpuTextureFormat};

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FilledRectsUniforms {
    color: [f32; 3],
    alpha: f32,
    rect_count: u32,
    _pad: [u32; 3],
}

manifold_node_engine::primitive! {
    name: RenderFilledRects,
    type_id: "node.draw_rectangles",
    purpose: "Instanced filled-rectangle overlay composited onto a source texture. Each item in the input Channels[X, Y, WIDTH, HEIGHT] array draws one axis-aligned filled rectangle with the configured color and alpha. Additive blend. Zero-size items are skipped. For gauge fills, center dots, debug regions, VU meters, status bars, selection boxes.",
    inputs: {
        in: Texture2D required,
        rects: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32] required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
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
            default: ParamValue::Float(0.8),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rect_count"),
            label: "Rect Count",
            ty: ParamType::Int,
            default: ParamValue::Float(32.0),
            range: Some((0.0, 256.0)),
            enum_values: &[],
        },
    ],
    // depth_rule: "instanced filled-rectangle overlay" draws genuine filled shapes (real visual weight), unlike the detections-named siblings (draw_dots/draw_markers/draw_gauge/draw_ticks) which stay Terminal as diagnostic annotation
    depth_rule: SourceHeight,
    composition_notes: "Wire a Channels[X, Y, WIDTH, HEIGHT] source (detection regions, gauge rects from a wgsl_compute, or manually authored rects) into the `rects` port. X/Y are rect centre in normalised 0..1 coords; WIDTH/HEIGHT are full extent. rect_count caps iteration — safe to leave at 32 even when the active count is lower (zero-size items are skipped). For outlined rectangles use render_lines instead.",
    examples: [],
    picker: { label: "Draw Rectangles", category: Atom },
    summary: "Draws a batch of filled rectangles onto the image from a list of positions and sizes. Good for bars, blocks, and data overlays.",
    category: Generate,
    role: Filter,
    aliases: ["draw rectangles", "render filled rects", "filled rects", "boxes", "bars"],
    boundary_reason: Blocked,
    extra_fields: {
        render_pipeline: Option<GpuRenderPipeline> = None,
    },
}

const FILLED_RECTS_SHADER: &str = r#"
struct Uniforms {
    color: vec3<f32>,
    alpha: f32,
    rect_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

struct Rect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> rects: array<Rect>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32, @builtin(instance_index) iid: u32) -> VsOut {
    // Skip zero-size rects by placing vertices off-screen.
    let r = rects[iid];
    if r.width <= 0.0001 && r.height <= 0.0001 {
        return VsOut(vec4<f32>(-2.0, -2.0, 0.0, 1.0));
    }
    // Centre + half-extent → clip-space quad corners.
    let hw = r.width * 0.5;
    let hh = r.height * 0.5;
    let x0 = r.x - hw;
    let y0 = r.y - hh;
    let x1 = r.x + hw;
    let y1 = r.y + hh;
    // 6 vertices for a triangle strip quad (0-1-2, 2-1-3).
    var cx: f32;
    var cy: f32;
    switch vid {
        case 0u: { cx = x0; cy = y0; }
        case 1u: { cx = x1; cy = y0; }
        case 2u: { cx = x0; cy = y1; }
        case 3u: { cx = x0; cy = y1; }
        case 4u: { cx = x1; cy = y0; }
        default: { cx = x1; cy = y1; }
    }
    // Map 0..1 → -1..1 clip space.
    let clip = vec4<f32>(cx * 2.0 - 1.0, cy * 2.0 - 1.0, 0.0, 1.0);
    return VsOut(clip);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(u.color * u.alpha, 0.0);
}
"#;

impl Primitive for RenderFilledRects {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let color = match ctx.params.get("color") {
            Some(ParamValue::Color(c)) => [c[0], c[1], c[2]],
            _ => [0.85, 0.92, 1.0],
        };
        let alpha = ctx.param_f32("alpha", 0.8);

        let rect_count = match ctx.params.get("rect_count") {
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => 32,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(rects_buf) = ctx.inputs.array("rects") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 || rect_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();

        // Copy source → output first, then draw rects on top.
        gpu.copy_texture_to_texture(in_tex, out_tex, w, h);

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
                FILLED_RECTS_SHADER,
                "vs_main",
                "fs_main",
                GpuTextureFormat::Rgba16Float,
                Some(blend),
                "node.draw_rectangles",
            )
        });

        let uniforms = FilledRectsUniforms {
            color,
            alpha,
            rect_count,
            _pad: [0; 3],
        };

        gpu.native_enc.draw_instanced(
            pipeline,
            out_tex,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: rects_buf,
                    offset: 0,
                },
            ],
            manifold_gpu::DrawCount::Direct { vertices: 6, instances: rect_count },
            GpuLoadAction::Load,
            "node.draw_rectangles",
        );
    }
}
