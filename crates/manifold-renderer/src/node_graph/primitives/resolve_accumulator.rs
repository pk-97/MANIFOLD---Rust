//! `node.resolve_accumulator` — convert a u32 fixed-point
//! accumulator buffer (produced by `node.scatter_particles`) into
//! a float density texture.
//!
//! Phase A.7 of `BUFFER_PORT_PLAN`. Reads each accumulator cell,
//! divides by `fixed_point_scale` (default 4096, matching FluidSim),
//! and writes the result as a uniform RGB density into an
//! `Rgba16Float` storage texture. Output alpha is always 1.0.
//!
//! This is the bridge from the Array(u32) wire family back to the
//! Texture2D wire family — downstream Mix / Blur / Feedback /
//! display primitives can consume the result as a normal texture.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResolveUniforms {
    width: u32,
    height: u32,
    inv_scale: f32,
    _pad: f32,
}

crate::primitive! {
    name: ResolveAccumulator,
    type_id: "node.resolve_accumulator",
    purpose: "Read a u32 fixed-point accumulator buffer (produced by node.scatter_particles), divide by `fixed_point_scale`, and write the result as a grayscale density texture. The bridge from Array(u32) back to Texture2D for downstream texture-domain primitives.",
    inputs: {
        accum: Array(u32) required,
    },
    outputs: {
        density: Texture2D,
    },
    params: [
        ParamDef {
            name: "width",
            label: "Width",
            ty: ParamType::Int,
            default: ParamValue::Int(960),
            range: Some((16.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "height",
            label: "Height",
            ty: ParamType::Int,
            default: ParamValue::Int(540),
            range: Some((16.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "fixed_point_scale",
            label: "Fixed-Point Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(4096.0),
            range: Some((1.0, 65536.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "width × height must match the producing ScatterParticles primitive. fixed_point_scale = scatter's scaled_energy gives unit-density output. Output texture must be Rgba16Float — the shader writes via texture_storage_2d<rgba16float, write>.",
    examples: [],
    picker: { label: "Resolve Accumulator", category: Atom },
}

impl Primitive for ResolveAccumulator {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let width = match ctx.params.get("width") {
            Some(ParamValue::Int(n)) => (*n).max(1) as u32,
            _ => 960,
        };
        let height = match ctx.params.get("height") {
            Some(ParamValue::Int(n)) => (*n).max(1) as u32,
            _ => 540,
        };
        let inv_scale = match ctx.params.get("fixed_point_scale") {
            Some(ParamValue::Float(f)) if *f > 0.0 => 1.0 / *f,
            _ => 1.0 / 4096.0,
        };

        let Some(accum) = ctx.inputs.array("accum") else {
            return;
        };
        let Some(density_out) = ctx.outputs.texture_2d("density") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/resolve_accumulator.wgsl"),
                "cs_main",
                "node.resolve_accumulator",
            )
        });

        let uniforms = ResolveUniforms {
            width,
            height,
            inv_scale,
            _pad: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: accum,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: density_out,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.resolve_accumulator",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn resolve_accumulator_declares_array_in_and_texture_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let u32_layout = ArrayType {
            item_size: 4,
            item_align: 4,
        };

        assert_eq!(ResolveAccumulator::TYPE_ID, "node.resolve_accumulator");
        assert_eq!(ResolveAccumulator::INPUTS.len(), 1);
        assert_eq!(ResolveAccumulator::INPUTS[0].name, "accum");
        assert_eq!(
            ResolveAccumulator::INPUTS[0].ty,
            PortType::Array(u32_layout)
        );

        assert_eq!(ResolveAccumulator::OUTPUTS.len(), 1);
        assert_eq!(ResolveAccumulator::OUTPUTS[0].name, "density");
        assert_eq!(ResolveAccumulator::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ResolveAccumulator::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.resolve_accumulator");
    }
}
