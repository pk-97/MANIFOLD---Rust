//! `node.blur_3d` — separable 3D Gaussian blur along one
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

use std::borrow::Cow;

use manifold_gpu::{GpuAddressMode, GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const BLUR_3D_MODES: &[&str] = &["Scalar (density)", "Vector (force field)"];
pub const BLUR_3D_AXES: &[&str] = &["X", "Y", "Z"];

// Standalone-codegen uniform layout: PARAMS order (mode, axis, vol_res, radius).
// The hand fluid_blur_3d.wgsl selected scalar/vector via two entry points and
// carried {vol_res, axis, radius, _pad}; the generated kernel branches on a
// runtime `mode` and reads vol_res/radius from here.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Blur3DUniforms {
    mode: u32,
    axis: u32,
    vol_res: i32,
    radius: f32,
}

crate::primitive! {
    name: Blur3DSeparable,
    type_id: "node.blur_3d",
    purpose: "Single-axis separable Gaussian blur on a Texture3D. Mode selects scalar (samples .r, writes single channel) or vector (samples .rgba, writes all channels). For a full 3-axis separable blur, wire three instances with ping-pong textures (axis=X(a→b), axis=Y(b→a), axis=Z(a→b)). Bilinear tap-pairing halves the sample count vs a naive Gaussian.",
    inputs: {
        in: Texture3D required,
        radius: ScalarF32 optional,
    },
    outputs: {
        out: Texture3D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: BLUR_3D_MODES,
        },
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: BLUR_3D_AXES,
        },
        ParamDef {
            name: Cow::Borrowed("vol_res"),
            label: "Volume Resolution",
            ty: ParamType::Int,
            default: ParamValue::Float(128.0),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("radius"),
            label: "Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.5, 16.0)),
            enum_values: &[],
        },
    ],
    // depth_rule: Texture3D-domain fluid-sim blur — outside the 2D depth companion channel entirely (buffer/volume domain), treated like other Texture3D atoms
    depth_rule: Terminal,
    composition_notes: "vol_res must match the input/output Texture3D dimensions. radius is in voxel units; sigma = max(radius/2.5, 0.5). Scalar mode preserves green/blue/alpha as zero/zero/one (writes single channel via .r). Vector mode preserves all four channels. Sampler is repeat-mode (toroidal wrap on edges).",
    examples: [],
    picker: { label: "Blur (3D)", category: Atom },
    summary: "Blurs a 3D volume one axis at a time, softening a density or flow field. Run it on each axis for an even blur in all directions.",
    category: BlurAndSharpen,
    role: Filter,
    aliases: ["blur 3d", "volume blur", "smooth"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/blur_3d_separable_body.wgsl"),
    input_access: [Gather],
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
        let radius = ctx.scalar_or_param("radius", 2.0);

        let Some(src) = ctx.inputs.texture_3d("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.texture_3d("out") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        // Single-source 3D Gather: `in` is a Texture3D gathered along one axis. The
        // generated kernel branches on a runtime `mode` (scalar vs vector) in one
        // kernel and binds uniform(0)/tex(1)/samp(2)/dst(3). fluid_blur_3d.wgsl's
        // two entry points are the parity oracle.
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.blur_3d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.blur_3d",
            )
        });
        // Repeat, not clamp: the sim volume is toroidal, and every other volume
        // stage wraps (particle containment, the 3D splat, the central-diff
        // gradient). The legacy generator built this exact Repeat sampler; a
        // clamp sampler here piles density against the six faces while the
        // wrapping gradient reads the pile as a huge seam gradient — wrong
        // forces along every face of the containerless (toroidal) mode.
        let sampler = self.sampler.get_or_insert_with(|| {
            gpu.device.create_sampler(&GpuSamplerDesc {
                address_mode_u: GpuAddressMode::Repeat,
                address_mode_v: GpuAddressMode::Repeat,
                address_mode_w: GpuAddressMode::Repeat,
                ..Default::default()
            })
        });

        let uniforms = Blur3DUniforms {
            mode,
            axis,
            vol_res: vol_res as i32,
            radius,
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
            [
                vol_res.div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
                vol_res.div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
                vol_res.div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
            ],
            "node.blur_3d",
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
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(Blur3DSeparable::TYPE_ID, "node.blur_3d");
        assert_eq!(Blur3DSeparable::INPUTS[0].name, "in");
        assert_eq!(Blur3DSeparable::INPUTS[0].ty, PortType::Texture3D);
        assert!(Blur3DSeparable::INPUTS[0].required);
        // `radius` port-shadow lets the JSON drive blur width from an
        // upstream scalar (e.g. a deg→rad-style affine on the feather
        // slider for FluidSim3D).
        let radius_port = Blur3DSeparable::INPUTS
            .iter()
            .find(|p| p.name == "radius")
            .expect("radius port-shadow input must exist");
        assert!(!radius_port.required);
        assert_eq!(radius_port.ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(Blur3DSeparable::OUTPUTS.len(), 1);
        assert_eq!(Blur3DSeparable::OUTPUTS[0].name, "out");
        assert_eq!(Blur3DSeparable::OUTPUTS[0].ty, PortType::Texture3D);
    }

    #[test]
    fn blur_3d_has_mode_axis_res_radius_params() {
        let names: Vec<&str> = Blur3DSeparable::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["mode", "axis", "vol_res", "radius"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Blur3DSeparable::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.blur_3d");
    }
}
