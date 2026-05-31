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
    purpose: "Read a u32 fixed-point accumulator buffer (produced by node.scatter_particles), divide by `fixed_point_scale`, and write the result as a grayscale density texture. The bridge from Array(u32) back to Texture2D for downstream texture-domain primitives. Dimensions are taken from the output Texture2D — which the backend allocates at canvas size — so resolve always covers every pixel of the density texture, matching whatever scatter wrote (also canvas-sized via `canvas_sized_array_outputs()`).",
    inputs: {
        accum: Array(u32) required,
    },
    outputs: {
        density: Texture2D,
    },
    params: [
        ParamDef {
            name: "fixed_point_scale",
            label: "Fixed-Point Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(4096.0),
            range: Some((1.0, 65536.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Output texture must be Rgba16Float — the shader writes via texture_storage_2d<rgba16float, write>. Dispatch dimensions match the output texture (allocated by the backend at canvas size), so paired ScatterParticles + ResolveAccumulator automatically span the full canvas without param tuning. fixed_point_scale = scatter's scaled_energy gives unit-density output.",
    examples: [],
    picker: { label: "Resolve Scatter", category: Atom },
    summary: "Reads back the buffer that Draw Particles wrote into and turns it into a normal image. The pickup step after a particle splat.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["resolve scatter", "accumulator", "read back"],
}

impl Primitive for ResolveAccumulator {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
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
        // Dimensions come from the output texture (allocated by the
        // backend at canvas size). Matches scatter's canvas-sized
        // accumulator so the resolve covers every pixel.
        let width = density_out.width.max(1);
        let height = density_out.height.max(1);

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
        let u32_layout = ArrayType::of_known::<u32>();

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
