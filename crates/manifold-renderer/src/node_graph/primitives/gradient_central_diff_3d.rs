//! `node.edge_slope_3d` — 6-tap central-difference gradient
//! of a scalar density `Texture3D`, written as a vec3 `Texture3D`.
//!
//! The 3D sibling of `node.edge_slope` (the 2D atom). Reads
//! a scalar density volume, computes a 6-tap central-difference gradient
//! with toroidal wrap (XY use `vol_res`, Z uses `vol_depth`), scales by
//! 0.5 (integer voxel-space central difference), and writes the vec3
//! gradient to an output `Texture3D`.
//!
//! Decomposed out of the legacy fused `node.fluid_gradient_curl_3d` —
//! the gradient half. Pair downstream with `node.swirl_force_3d`
//! (cross-with-axis curl + slope combine) to reconstruct the FluidSim3D
//! force field, or use the raw gradient directly for any 3D
//! displacement / normal / flow pipeline.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Gradient3DUniforms {
    vol_res: u32,
    vol_depth: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: GradientCentralDiff3D,
    type_id: "node.edge_slope_3d",
    purpose: "6-tap central-difference gradient of a scalar density Texture3D, written as a vec3 Texture3D. Toroidal wrap (XY use vol_res, Z uses vol_depth); gradient = float3(dx, dy, dz) * 0.5 in integer voxel space. 3D sibling of node.edge_slope. Decomposed from the gradient half of the legacy fused node.fluid_gradient_curl_3d; pair with node.swirl_force_3d for the FluidSim3D force field.",
    inputs: {
        density: Texture3D required,
    },
    outputs: {
        gradient: Texture3D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("vol_res"),
            label: "Volume Resolution",
            ty: ParamType::Int,
            default: ParamValue::Float(128.0),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("vol_depth"),
            label: "Volume Depth",
            ty: ParamType::Int,
            default: ParamValue::Float(128.0),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
    ],
    // depth_rule: Texture3D-domain gradient — outside the 2D depth channel
    depth_rule: Terminal,
    composition_notes: "Output Texture3D dims follow vol_res / vol_depth (the default texture_3d_output_dims source). The output is the raw 6-tap gradient at integer voxel scale (×0.5) — feed it to node.swirl_force_3d to cross with a rotating reference axis (curl) and combine with slope, exactly as the legacy FluidSim3D force field did. Generic enough for any volumetric gradient need (normals from a heightfield volume, flow from a density field).",
    examples: ["FluidSim3D"],
    picker: { label: "Edge Slope (3D)", category: Atom },
    summary: "Measures how fast a value changes through a 3D volume, giving a direction at every point. Used to find flow and forces inside a fluid sim.",
    category: FieldsAndCoordinates,
    role: Filter,
    aliases: ["gradient 3d", "edge slope", "gradient central diff 3d", "volume gradient"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/gradient_central_diff_3d_body.wgsl"),
    input_access: [GatherTexel],
}

impl Primitive for GradientCentralDiff3D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let vol_res = match ctx.params.get("vol_res") {
            Some(ParamValue::Float(n)) => n.round().max(1_f32) as u32,
            _ => 128,
        };
        let vol_depth = match ctx.params.get("vol_depth") {
            Some(ParamValue::Float(n)) => n.round().max(1_f32) as u32,
            _ => 128,
        };

        let Some(density) = ctx.inputs.texture_3d("density") else {
            return;
        };
        let Some(gradient) = ctx.outputs.texture_3d("gradient") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // `density` is a 3D GatherTexel input (6-tap integer textureLoad with
            // toroidal wrap, no sampler). Generated kernel binds uniform(0)/tex(1)/
            // dst(2) — identical to the hand layout. gradient_central_diff_3d.wgsl
            // is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.edge_slope_3d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.edge_slope_3d",
            )
        });

        let uniforms = Gradient3DUniforms {
            vol_res,
            vol_depth,
            _pad0: 0,
            _pad1: 0,
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
                    texture: gradient,
                },
            ],
            // Grid must match the GENERATED kernel's 4x4x4 workgroup
            // (codegen::VOLUME_WORKGROUP_3D), not the hand shader's 8x8x8 -
            // div_ceil(8) covered only an eighth of the volume.
            [
                vol_res.div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
                vol_res.div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
                vol_depth.div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
            ],
            "node.edge_slope_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_texture_3d_in_and_out() {
        use crate::node_graph::ports::PortType;
        assert_eq!(GradientCentralDiff3D::TYPE_ID, "node.edge_slope_3d");
        assert_eq!(GradientCentralDiff3D::INPUTS.len(), 1);
        assert_eq!(GradientCentralDiff3D::INPUTS[0].name, "density");
        assert_eq!(GradientCentralDiff3D::INPUTS[0].ty, PortType::Texture3D);
        assert!(GradientCentralDiff3D::INPUTS[0].required);
        assert_eq!(GradientCentralDiff3D::OUTPUTS.len(), 1);
        assert_eq!(GradientCentralDiff3D::OUTPUTS[0].name, "gradient");
        assert_eq!(GradientCentralDiff3D::OUTPUTS[0].ty, PortType::Texture3D);
    }

    #[test]
    fn uniform_struct_is_16_bytes() {
        assert_eq!(std::mem::size_of::<Gradient3DUniforms>(), 16);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GradientCentralDiff3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.edge_slope_3d");
    }
}
