//! `node.rotate_vec2_90` — rotate the RG vec2 field by ±90°.
//!
//! Tiny but ubiquitous in flow work: rotating a gradient by 90° gives
//! the perpendicular direction, which is the velocity component of a
//! curl (divergence-free flow). The standard atom for "gradient →
//! curl forcing" in reaction-diffusion / fluid sims.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const ROTATE_VEC2_DIRECTIONS: &[&str] = &["+90° CCW", "-90° CW"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RotateUniforms {
    direction: u32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: RotateVec2_90,
    type_id: "node.rotate_vec2_90",
    purpose: "Rotate the input's RG vec2 field by ±90° per pixel. +90° CCW: (x, y) → (-y, x); -90° CW: (x, y) → (y, -x). The curl-from-gradient atom — rotating a gradient by 90° gives the perpendicular direction, which is the velocity component of a divergence-free curl flow.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "direction",
            label: "Direction",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: ROTATE_VEC2_DIRECTIONS,
        },
    ],
    composition_notes: "BA of the input are ignored; output BA = (0, 1). Chain: `gradient_central_diff → (optional normalize_vec2) → rotate_vec2_90` is the curl-forcing pattern used by oily-fluid's velocity step. For larger-angle rotations use `node.rotate_2d` (UV transform, different operation).",
    examples: [],
    picker: { label: "Rotate Vec2 90°", category: Atom },
}

impl Primitive for RotateVec2_90 {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let direction = match ctx.params.get("direction") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(1),
            _ => 0,
        };

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (target.width, target.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/rotate_vec2_90.wgsl"),
                "cs_main",
                "node.rotate_vec2_90",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = RotateUniforms {
            direction,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: src,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.rotate_vec2_90",
        );
    }
}
