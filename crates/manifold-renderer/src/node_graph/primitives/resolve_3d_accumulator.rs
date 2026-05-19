//! `node.resolve_3d_accumulator` — convert a u32 fixed-point 3D
//! accumulator buffer into a float density Texture3D, with
//! self-clearing back to zero for the next frame.
//!
//! Bit-exact wrap of `generators/shaders/fluid_scatter_3d.wgsl`'s
//! `resolve_3d` entry point via include_str. Pairs with
//! `node.scatter_particles_3d` upstream. First Texture3D-output
//! primitive in node_graph — exercises the new
//! `MetalBackend::pre_bind_texture_3d` path.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Resolve3DUniforms {
    vol_res: u32,
    vol_depth: u32,
    _pad0: u32,
    _pad1: u32,
    // Naga padding to match the largest @binding(2) uniform in the
    // shader module (ProjectedUniforms = 112 bytes).
    _pad2: [u32; 4],
    _pad3: [u32; 4],
    _pad4: [u32; 4],
    _pad5: [u32; 4],
    _pad6: [u32; 4],
    _pad7: [u32; 4],
}

crate::primitive! {
    name: Resolve3DAccumulator,
    type_id: "node.resolve_3d_accumulator",
    purpose: "Read a u32 fixed-point 3D accumulator buffer (produced by node.scatter_particles_3d), divide by 4096 (FluidSim3D's FIXED_POINT_MULTIPLIER), and write the result as a density Texture3D. Self-clears the accumulator to zero atomically as part of the same dispatch so the next frame starts fresh.",
    inputs: {
        accum: Array(u32) required,
    },
    outputs: {
        density: Texture3D,
    },
    params: [
        ParamDef {
            name: "vol_res",
            label: "Volume Resolution",
            ty: ParamType::Int,
            default: ParamValue::Int(128),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "vol_depth",
            label: "Volume Depth",
            ty: ParamType::Int,
            default: ParamValue::Int(128),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "vol_res × vol_res × vol_depth must match the producing ScatterParticles3D primitive. Output Texture3D must be Rgba16Float — the shader writes via texture_storage_3d<rgba16float, write>. The output volume is pre-bound by the chain build at the same dimensions; the accumulator buffer is sized vol_res² × vol_depth × 4 bytes.",
    examples: [],
    picker: { label: "Resolve 3D Accumulator", category: Atom },
}

impl Primitive for Resolve3DAccumulator {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let vol_res = match ctx.params.get("vol_res") {
            Some(ParamValue::Int(n)) => (*n).max(1) as u32,
            _ => 128,
        };
        let vol_depth = match ctx.params.get("vol_depth") {
            Some(ParamValue::Int(n)) => (*n).max(1) as u32,
            _ => 128,
        };

        let Some(accum) = ctx.inputs.array("accum") else {
            return;
        };
        let Some(density) = ctx.outputs.texture_3d("density") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../generators/shaders/fluid_scatter_3d.wgsl"),
                "resolve_3d",
                "node.resolve_3d_accumulator",
            )
        });

        let uniforms = Resolve3DUniforms {
            vol_res,
            vol_depth,
            _pad0: 0,
            _pad1: 0,
            _pad2: [0; 4],
            _pad3: [0; 4],
            _pad4: [0; 4],
            _pad5: [0; 4],
            _pad6: [0; 4],
            _pad7: [0; 4],
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: accum,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: density,
                },
                GpuBinding::Bytes {
                    binding: 2,
                    data: bytemuck::bytes_of(&uniforms),
                },
            ],
            [vol_res.div_ceil(8), vol_res.div_ceil(8), vol_depth.div_ceil(8)],
            "node.resolve_3d_accumulator",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn resolve_3d_declares_array_in_and_texture_3d_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let u32_layout = ArrayType {
            item_size: 4,
            item_align: 4,
        };
        assert_eq!(Resolve3DAccumulator::TYPE_ID, "node.resolve_3d_accumulator");
        assert_eq!(Resolve3DAccumulator::INPUTS.len(), 1);
        assert_eq!(Resolve3DAccumulator::INPUTS[0].name, "accum");
        assert_eq!(
            Resolve3DAccumulator::INPUTS[0].ty,
            PortType::Array(u32_layout)
        );
        assert_eq!(Resolve3DAccumulator::OUTPUTS.len(), 1);
        assert_eq!(Resolve3DAccumulator::OUTPUTS[0].name, "density");
        assert_eq!(
            Resolve3DAccumulator::OUTPUTS[0].ty,
            PortType::Texture3D
        );
    }

    #[test]
    fn resolve_3d_uniform_struct_matches_naga_padding() {
        assert_eq!(std::mem::size_of::<Resolve3DUniforms>(), 112);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Resolve3DAccumulator::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.resolve_3d_accumulator");
    }
}
