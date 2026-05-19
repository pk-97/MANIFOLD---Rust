//! `node.render_lines` — render-pass primitive that draws an
//! `Array<LinePoint>` as connected capsule line segments with
//! 4× MSAA + additive blending.
//!
//! Phase C of `BUFFER_PORT_PLAN`. The second render-pass primitive
//! in node_graph (follows Render3DMesh). Each segment connects
//! point[i] → point[i+1]; `closed_loop` adds a wrap segment from
//! point[N-1] → point[0]. Capsule SDF in the fragment shader
//! gives consistent line thickness with anti-aliased round caps.

use manifold_gpu::{GpuBinding, GpuLoadAction};

use crate::generators::mesh_common::LinePoint;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const MSAA_SAMPLE_COUNT: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LineRenderUniforms {
    rt_width: f32,
    rt_height: f32,
    half_thickness: f32,
    closed_loop: u32,
    color: [f32; 4],
    num_points: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: RenderLines,
    type_id: "node.render_lines",
    purpose: "Draw an Array<LinePoint> as connected capsule line segments with 4x MSAA and additive blending. closed_loop toggles whether the last segment wraps back to point[0]. Capsule SDF with fwidth() AA gives clean thick lines at any size. Pair with node.generate_parametric_curve or node.audio_input upstream.",
    inputs: {
        points: Array(LinePoint) required,
    },
    outputs: {
        color: Texture2D,
    },
    params: [
        ParamDef {
            name: "thickness",
            label: "Thickness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.004),
            range: Some((0.0001, 0.1)),
            enum_values: &[],
        },
        ParamDef {
            name: "closed_loop",
            label: "Closed Loop",
            ty: ParamType::Bool,
            default: ParamValue::Bool(true),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: "color_r",
            label: "Color R",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "color_g",
            label: "Color G",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "color_b",
            label: "Color B",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "color_a",
            label: "Color A",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "thickness is half-thickness in screen-fraction units (≈0.004 = ~1px at 1080p). Color above 1.0 produces HDR bloom-friendly output (additive blending). Number of segments = N-1 (open) or N (closed). N derives from the input Array<LinePoint>'s capacity, not active_count — upstream must size the buffer to match the intended line.",
    examples: [],
    picker: { label: "Render Lines", category: Atom },
    extra_fields: {
        render_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        msaa_texture: Option<manifold_gpu::GpuTexture> = None,
        msaa_width: u32 = 0,
        msaa_height: u32 = 0,
    },
}

impl RenderLines {
    fn ensure_msaa_texture(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.msaa_width == width
            && self.msaa_height == height
            && self.msaa_texture.is_some()
        {
            return;
        }
        self.msaa_texture = Some(device.create_texture_msaa_memoryless(
            width,
            height,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            MSAA_SAMPLE_COUNT,
            "node.render_lines MSAA",
        ));
        self.msaa_width = width;
        self.msaa_height = height;
    }
}

impl Primitive for RenderLines {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let thickness = match ctx.params.get("thickness") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.004,
        };
        let closed_loop = matches!(ctx.params.get("closed_loop"), Some(ParamValue::Bool(true)));
        let color_r = match ctx.params.get("color_r") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let color_g = match ctx.params.get("color_g") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let color_b = match ctx.params.get("color_b") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let color_a = match ctx.params.get("color_a") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };

        let Some(points) = ctx.inputs.array("points") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("color") else {
            return;
        };
        let width = target.width;
        let height = target.height;
        if width == 0 || height == 0 {
            return;
        }
        let item_size = std::mem::size_of::<LinePoint>() as u64;
        let num_points = (points.size / item_size) as u32;
        if num_points < 2 {
            let gpu = ctx.gpu_encoder();
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            return;
        }
        let segments = if closed_loop { num_points } else { num_points - 1 };

        let half_thickness_px = thickness * (height as f32);

        let uniforms = LineRenderUniforms {
            rt_width: width as f32,
            rt_height: height as f32,
            half_thickness: half_thickness_px,
            closed_loop: if closed_loop { 1 } else { 0 },
            color: [color_r, color_g, color_b, color_a],
            num_points,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        let gpu = ctx.gpu_encoder();
        if self.render_pipeline.is_none() {
            let blend = manifold_gpu::GpuBlendState {
                src_factor: manifold_gpu::GpuBlendFactor::One,
                dst_factor: manifold_gpu::GpuBlendFactor::One,
                operation: manifold_gpu::GpuBlendOp::Max,
                src_alpha_factor: manifold_gpu::GpuBlendFactor::One,
                dst_alpha_factor: manifold_gpu::GpuBlendFactor::One,
                alpha_operation: manifold_gpu::GpuBlendOp::Max,
            };
            self.render_pipeline = Some(gpu.device.create_render_pipeline_msaa(
                include_str!("shaders/render_lines.wgsl"),
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                Some(blend),
                MSAA_SAMPLE_COUNT,
                "node.render_lines",
            ));
        }
        self.ensure_msaa_texture(gpu.device, width, height);

        let pipeline = self.render_pipeline.as_ref().expect("just inserted");
        let msaa_tex = self.msaa_texture.as_ref().expect("just inserted");

        gpu.native_enc.draw_instanced_msaa(
            pipeline,
            msaa_tex,
            target,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: points,
                    offset: 0,
                },
            ],
            6,
            segments,
            GpuLoadAction::Clear,
            "node.render_lines",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn render_lines_declares_linepoint_input_and_texture_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType {
            item_size: std::mem::size_of::<LinePoint>() as u32,
            item_align: std::mem::align_of::<LinePoint>() as u32,
        };

        assert_eq!(RenderLines::TYPE_ID, "node.render_lines");
        assert_eq!(RenderLines::INPUTS.len(), 1);
        assert_eq!(RenderLines::INPUTS[0].name, "points");
        assert!(RenderLines::INPUTS[0].required);
        assert_eq!(RenderLines::INPUTS[0].ty, PortType::Array(layout));
        assert_eq!(RenderLines::OUTPUTS.len(), 1);
        assert_eq!(RenderLines::OUTPUTS[0].name, "color");
        assert_eq!(RenderLines::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn render_lines_has_thickness_loop_color_params() {
        let names: Vec<&str> = RenderLines::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec!["thickness", "closed_loop", "color_r", "color_g", "color_b", "color_a"]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = RenderLines::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.render_lines");
    }
}
