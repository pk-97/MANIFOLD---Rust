//! `node.blur_3d_separable` — separable 3D Gaussian blur along one
//! axis. Bit-exact wrap of `generators/shaders/fluid_blur_3d.wgsl`
//! via include_str (two entry points: `blur_scalar` for density-like
//! fields, `blur_vector` for force-field volumes).
//!
//! Each instance blurs along ONE axis (X / Y / Z). For a full
//! separable 3-pass blur, the graph wires three instances with
//! ping-pong texture allocation: axis=X(a→b), axis=Y(b→a),
//! axis=Z(a→b). Bilinear tap-pairing halves the sample count
//! relative to a naive Gaussian (matches FluidSim3D's behaviour
//! exactly).

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const BLUR_3D_MODES: &[&str] = &["Scalar (density)", "Vector (force field)"];
pub const BLUR_3D_AXES: &[&str] = &["X", "Y", "Z"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Blur3DUniforms {
    vol_res: u32,
    axis: u32,
    radius: f32,
    _pad: u32,
}

crate::primitive! {
    name: Blur3DSeparable,
    type_id: "node.blur_3d_separable",
    purpose: "Single-axis separable Gaussian blur on a Texture3D. Mode selects scalar (samples .r, writes single channel) or vector (samples .rgba, writes all channels). For a full 3-axis separable blur, wire three instances with ping-pong textures (axis=X(a→b), axis=Y(b→a), axis=Z(a→b)). Bilinear tap-pairing halves the sample count vs a naive Gaussian.",
    inputs: {
        in: Texture3D required,
    },
    outputs: {
        out: Texture3D,
    },
    params: [
        ParamDef {
            name: "mode",
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: BLUR_3D_MODES,
        },
        ParamDef {
            name: "axis",
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: BLUR_3D_AXES,
        },
        ParamDef {
            name: "vol_res",
            label: "Volume Resolution",
            ty: ParamType::Int,
            default: ParamValue::Float(128.0),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "radius",
            label: "Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.5, 16.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "vol_res must match the input/output Texture3D dimensions. radius is in voxel units; sigma = max(radius/2.5, 0.5). Scalar mode preserves green/blue/alpha as zero/zero/one (writes single channel via .r). Vector mode preserves all four channels. Sampler is repeat-mode (toroidal wrap on edges).",
    examples: [],
    picker: { label: "Blur 3D Separable", category: Atom },
    extra_fields: {
        vector_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
    },
}

impl Primitive for Blur3DSeparable {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
        };
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
        };
        let vol_res = match ctx.params.get("vol_res") {
            Some(ParamValue::Float(n)) => n.round().max(1_f32) as u32,
            _ => 128,
        };
        let radius = match ctx.params.get("radius") {
            Some(ParamValue::Float(f)) => *f,
            _ => 2.0,
        };

        let Some(src) = ctx.inputs.texture_3d("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.texture_3d("out") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        // Mode 0 = scalar (uses self.pipeline), Mode 1 = vector (uses self.vector_pipeline).
        let pipeline = if mode == 1 {
            self.vector_pipeline.get_or_insert_with(|| {
                gpu.device.create_compute_pipeline(
                    include_str!("../../generators/shaders/fluid_blur_3d.wgsl"),
                    "blur_vector",
                    "node.blur_3d_separable.vector",
                )
            })
        } else {
            self.pipeline.get_or_insert_with(|| {
                gpu.device.create_compute_pipeline(
                    include_str!("../../generators/shaders/fluid_blur_3d.wgsl"),
                    "blur_scalar",
                    "node.blur_3d_separable.scalar",
                )
            })
        };
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = Blur3DUniforms {
            vol_res,
            axis,
            radius,
            _pad: 0,
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
                    texture: dst,
                },
            ],
            [vol_res.div_ceil(4), vol_res.div_ceil(4), vol_res.div_ceil(4)],
            "node.blur_3d_separable",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn blur_3d_declares_texture_3d_in_and_out() {
        use crate::node_graph::ports::PortType;
        assert_eq!(Blur3DSeparable::TYPE_ID, "node.blur_3d_separable");
        assert_eq!(Blur3DSeparable::INPUTS.len(), 1);
        assert_eq!(Blur3DSeparable::INPUTS[0].name, "in");
        assert_eq!(Blur3DSeparable::INPUTS[0].ty, PortType::Texture3D);
        assert_eq!(Blur3DSeparable::OUTPUTS.len(), 1);
        assert_eq!(Blur3DSeparable::OUTPUTS[0].name, "out");
        assert_eq!(Blur3DSeparable::OUTPUTS[0].ty, PortType::Texture3D);
    }

    #[test]
    fn blur_3d_has_mode_axis_res_radius_params() {
        let names: Vec<&str> = Blur3DSeparable::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["mode", "axis", "vol_res", "radius"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Blur3DSeparable::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.blur_3d_separable");
    }
}
