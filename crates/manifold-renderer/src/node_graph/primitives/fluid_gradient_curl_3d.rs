//! `node.fluid_gradient_curl_3d` — fused 3D gradient + curl force
//! field generator. Bit-exact wrap of
//! `generators/shaders/fluid_gradient_curl_3d.wgsl` via include_str.
//!
//! Reads a scalar density Texture3D, computes 6-tap central-difference
//! gradient with toroidal wrap (XY/Z separate), crosses with a
//! rotating reference axis to produce curl, and combines curl
//! (tangential orbit) with slope (radial push/pull) into a vec3
//! force field written to an output Texture3D.
//!
//! The pass is intentionally fused for FluidSim3D parity. Splitting
//! gradient and curl into separate primitives would introduce a
//! Texture3D storage write between them and break bit-exact
//! behaviour.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradientCurl3DUniforms {
    vol_res: u32,
    vol_depth: u32,
    _pad0: u32,
    _pad1: u32,
    curl_strength: f32,
    slope_strength: f32,
    ref_axis_x: f32,
    ref_axis_y: f32,
    ref_axis_z: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
}

crate::primitive! {
    name: FluidGradientCurl3D,
    type_id: "node.fluid_gradient_curl_3d",
    purpose: "Fused 3D gradient + curl force field for the FluidSim3D family. Reads a scalar density Texture3D, computes 6-tap central-difference gradient with toroidal wrap, crosses with a rotating reference axis for curl (tangential orbit), combines with slope (radial). Writes vec3 force to an output Texture3D. Fused for bit-exact FluidSim3D parity.",
    inputs: {
        density: Texture3D required,
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
    composition_notes: "FluidSim3D computes curl_strength = flow * 500 * sin(curl_angle) and slope_strength = flow * 500 * cos(curl_angle) on the CPU side, with ref_axis = (rotating vector based on time × 0.3). The primitive normalizes ref_axis internally before passing to the shader — graph wires can emit raw sin/cos components without worrying about unit length. Drive curl_strength / slope_strength via Math nodes if you want to expose angle/flow params.",
    examples: [],
    picker: { label: "Fluid Gradient Curl 3D", category: Atom },
}

impl Primitive for FluidGradientCurl3D {
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
        // tracks `curl_strength` exactly. Without this normalize,
        // graph wires that produce sin/cos components let the axis
        // length drift (e.g. FluidSim3D's `(sin(0.3t), cos(0.21t),
        // sin(0.15t))` swings between ~1.0 and ~1.7) and the swirl
        // strength secretly breathes by up to 70% on a slow phase
        // cycle independent of the user-facing slider. Zero-length
        // input falls back to (0, 1, 0) so the cross product stays
        // well-defined instead of dropping to a degenerate axis.
        let raw_len_sq =
            raw_axis_x * raw_axis_x + raw_axis_y * raw_axis_y + raw_axis_z * raw_axis_z;
        let (ref_axis_x, ref_axis_y, ref_axis_z) = if raw_len_sq < 1e-10 {
            (0.0, 1.0, 0.0)
        } else {
            let inv_len = raw_len_sq.sqrt().recip();
            (raw_axis_x * inv_len, raw_axis_y * inv_len, raw_axis_z * inv_len)
        };

        let Some(density) = ctx.inputs.texture_3d("density") else {
            return;
        };
        let Some(force) = ctx.outputs.texture_3d("force") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../generators/shaders/fluid_gradient_curl_3d.wgsl"),
                "main",
                "node.fluid_gradient_curl_3d",
            )
        });

        let uniforms = GradientCurl3DUniforms {
            vol_res,
            vol_depth,
            _pad0: 0,
            _pad1: 0,
            curl_strength,
            slope_strength,
            ref_axis_x,
            ref_axis_y,
            ref_axis_z,
            _pad2: 0.0,
            _pad3: 0.0,
            _pad4: 0.0,
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
                    texture: density,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: force,
                },
            ],
            [vol_res.div_ceil(8), vol_res.div_ceil(8), vol_depth.div_ceil(8)],
            "node.fluid_gradient_curl_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn fluid_gradient_curl_3d_declares_texture_3d_in_and_out() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(FluidGradientCurl3D::TYPE_ID, "node.fluid_gradient_curl_3d");
        assert_eq!(FluidGradientCurl3D::INPUTS[0].name, "density");
        assert_eq!(FluidGradientCurl3D::INPUTS[0].ty, PortType::Texture3D);
        assert!(FluidGradientCurl3D::INPUTS[0].required);
        // The remaining inputs are port-shadow scalar overrides for
        // every param the FluidSim3D family wants to drive from the graph
        // (curl/slope are time-varying angle decompositions, ref_axis
        // rotates over time).
        for input in &FluidGradientCurl3D::INPUTS[1..] {
            assert!(!input.required, "{} should be optional port-shadow", input.name);
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(FluidGradientCurl3D::OUTPUTS.len(), 1);
        assert_eq!(FluidGradientCurl3D::OUTPUTS[0].name, "force");
        assert_eq!(FluidGradientCurl3D::OUTPUTS[0].ty, PortType::Texture3D);
    }

    #[test]
    fn fluid_gradient_curl_3d_has_full_param_surface() {
        let names: Vec<&str> = FluidGradientCurl3D::PARAMS
            .iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(
            names,
            vec![
                "vol_res",
                "vol_depth",
                "curl_strength",
                "slope_strength",
                "ref_axis_x",
                "ref_axis_y",
                "ref_axis_z",
            ]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FluidGradientCurl3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.fluid_gradient_curl_3d");
    }
}
