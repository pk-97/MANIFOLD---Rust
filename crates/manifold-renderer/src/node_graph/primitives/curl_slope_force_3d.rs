//! `node.curl_slope_force_3d` — combine a vec3 gradient `Texture3D`
//! into a force field via curl (cross with a rotating reference axis)
//! plus slope (gradient scaled).
//!
//! Reads a vec3 gradient volume (typically from
//! `node.gradient_central_diff_3d`), crosses it with a unit reference
//! axis to produce curl (tangential orbit around density peaks), and
//! combines curl with the gradient scaled by slope (radial push/pull):
//!
//! ```text
//! curl_force = cross(gradient, ref_axis)
//! force      = curl_force * curl_strength + gradient * slope_strength
//! ```
//!
//! The second half of the decomposed `node.fluid_gradient_curl_3d`. The
//! `ref_axis` is normalized CPU-side before reaching the shader so the
//! swirl magnitude tracks `curl_strength` exactly even when graph wires
//! feed raw `sin`/`cos` components whose length drifts.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

// Standalone-codegen uniform layout: PARAMS order (vol_res, vol_depth, curl_
// strength, slope_strength, ref_axis_x/y/z) padded to 32 bytes — contiguous,
// unlike the hand uniform which padded vol_res/vol_depth to 16. ref_axis is the
// CPU-normalized unit vector (run() normalizes; the body uses it directly).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CurlSlope3DUniforms {
    vol_res: u32,
    vol_depth: u32,
    curl_strength: f32,
    slope_strength: f32,
    ref_axis_x: f32,
    ref_axis_y: f32,
    ref_axis_z: f32,
    _pad: f32,
}

crate::primitive! {
    name: CurlSlopeForce3D,
    type_id: "node.curl_slope_force_3d",
    purpose: "Combine a vec3 gradient Texture3D into a force field: cross the gradient with a unit reference axis for curl (tangential orbit around density peaks) and add the gradient scaled by slope (radial push/pull). force = cross(gradient, ref_axis) * curl_strength + gradient * slope_strength. Writes a vec3 force Texture3D. The curl+slope half of the decomposed node.fluid_gradient_curl_3d; pair downstream of node.gradient_central_diff_3d.",
    inputs: {
        gradient: Texture3D required,
        curl_strength: ScalarF32 optional,
        slope_strength: ScalarF32 optional,
        ref_axis_x: ScalarF32 optional,
        ref_axis_y: ScalarF32 optional,
        ref_axis_z: ScalarF32 optional,
    },
    outputs: {
        force: Texture3D,
    },
    params: [
        ParamDef {
            name: "vol_res",
            label: "Volume Resolution",
            ty: ParamType::Int,
            default: ParamValue::Float(128.0),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "vol_depth",
            label: "Volume Depth",
            ty: ParamType::Int,
            default: ParamValue::Float(128.0),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "curl_strength",
            label: "Curl Strength",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "slope_strength",
            label: "Slope Strength",
            ty: ParamType::Float,
            default: ParamValue::Float(-500.0),
            range: Some((-5000.0, 5000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "ref_axis_x",
            label: "Ref Axis X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "ref_axis_y",
            label: "Ref Axis Y",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "ref_axis_z",
            label: "Ref Axis Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Output Texture3D dims follow vol_res / vol_depth. FluidSim3D computes curl_strength = flow * 500 * sin(curl_angle) and slope_strength = flow * 500 * cos(curl_angle) in graph Math nodes, with ref_axis = a rotating vector (sin/cos of time × 0.3). The primitive normalizes ref_axis internally before the cross product — graph wires can emit raw sin/cos components without worrying about unit length (zero-length falls back to (0,1,0) so the cross stays well-defined). Pair upstream with node.gradient_central_diff_3d.",
    examples: ["FluidSimulation3D"],
    picker: { label: "Swirl Force (3D, curl)", category: Atom },
    summary: "Turns a 3D gradient field into a swirling, divergence-free force, the move that makes 3D particles curl into smoke-like eddies.",
    category: Particles3D,
    role: Filter,
    aliases: ["swirl force", "curl", "vortex", "smoke"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/curl_slope_force_3d_body.wgsl"),
    input_access: [CoincidentTexel],
}

impl Primitive for CurlSlopeForce3D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let vol_res = match ctx.params.get("vol_res") {
            Some(ParamValue::Float(n)) => n.round().max(1_f32) as u32,
            _ => 128,
        };
        let vol_depth = match ctx.params.get("vol_depth") {
            Some(ParamValue::Float(n)) => n.round().max(1_f32) as u32,
            _ => 128,
        };
        let curl_strength = ctx.scalar_or_param("curl_strength", 0.0);
        let slope_strength = ctx.scalar_or_param("slope_strength", -500.0);
        let raw_axis_x = ctx.scalar_or_param("ref_axis_x", 0.0);
        let raw_axis_y = ctx.scalar_or_param("ref_axis_y", 1.0);
        let raw_axis_z = ctx.scalar_or_param("ref_axis_z", 0.0);
        // Shader contract: `ref_axis` is unit-length so curl magnitude
        // tracks `curl_strength` exactly. Zero-length input falls back
        // to (0, 1, 0) so the cross product stays well-defined.
        let raw_len_sq =
            raw_axis_x * raw_axis_x + raw_axis_y * raw_axis_y + raw_axis_z * raw_axis_z;
        let (ref_axis_x, ref_axis_y, ref_axis_z) = if raw_len_sq < 1e-10 {
            (0.0, 1.0, 0.0)
        } else {
            let inv_len = raw_len_sq.sqrt().recip();
            (raw_axis_x * inv_len, raw_axis_y * inv_len, raw_axis_z * inv_len)
        };

        let Some(gradient) = ctx.inputs.texture_3d("gradient") else {
            return;
        };
        let Some(force) = ctx.outputs.texture_3d("force") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // `gradient` is a 3D CoincidentTexel input (own-voxel integer
            // textureLoad, no sampler). Generated kernel binds uniform(0)/tex(1)/
            // dst(2). curl_slope_force_3d.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.curl_slope_force_3d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.curl_slope_force_3d",
            )
        });

        let uniforms = CurlSlope3DUniforms {
            vol_res,
            vol_depth,
            curl_strength,
            slope_strength,
            ref_axis_x,
            ref_axis_y,
            ref_axis_z,
            _pad: 0.0,
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
                    texture: gradient,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: force,
                },
            ],
            [vol_res.div_ceil(8), vol_res.div_ceil(8), vol_depth.div_ceil(8)],
            "node.curl_slope_force_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_gradient_in_force_out_and_port_shadow_scalars() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(CurlSlopeForce3D::TYPE_ID, "node.curl_slope_force_3d");
        assert_eq!(CurlSlopeForce3D::INPUTS[0].name, "gradient");
        assert_eq!(CurlSlopeForce3D::INPUTS[0].ty, PortType::Texture3D);
        assert!(CurlSlopeForce3D::INPUTS[0].required);
        for input in &CurlSlopeForce3D::INPUTS[1..] {
            assert!(!input.required, "{} should be optional port-shadow", input.name);
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(CurlSlopeForce3D::OUTPUTS.len(), 1);
        assert_eq!(CurlSlopeForce3D::OUTPUTS[0].name, "force");
        assert_eq!(CurlSlopeForce3D::OUTPUTS[0].ty, PortType::Texture3D);
    }

    #[test]
    fn uniform_struct_is_48_bytes() {
        assert_eq!(std::mem::size_of::<CurlSlope3DUniforms>(), 48);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = CurlSlopeForce3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.curl_slope_force_3d");
    }
}
