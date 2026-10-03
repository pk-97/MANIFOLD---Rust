//! One upwind reinitialisation sweep, ported from FLIP Fluids levelsetsolver.cpp.
//! Copyright Ryan L. Guy & Dennis Fassbaender (MIT); THIRD_PARTY_NOTICES.md.
//! Section 2.5 audit: redistance_lattice computes nearest triangle distance,
//! which is not the engine upwind rule. This atom is a pure stencil sweep;
//! whitewater owns convergence reduction and calls its generated pipeline.
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use manifold_gpu::GpuBinding;
use std::borrow::Cow;

crate::primitive! {
    name: UpwindDistance,
    type_id: "node.upwind_distance",
    purpose: "One FLIP Fluids upwind signed-distance sweep on valid cells, with recomputed smoothed sign, clamped neighbours and pseudo-time h/2. Invalid cells retain their input.",
    inputs: {
        levelset: Array(f32) required,
        valid: Array(u32) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
    },
    outputs: { out: Array(f32), },
    params: [
        float_param!("nodes_x", "Nodes X", 8.0, 2.0, 4096.0),
        float_param!("nodes_y", "Nodes Y", 8.0, 2.0, 4096.0),
        float_param!("nodes_z", "Nodes Z", 8.0, 2.0, 4096.0),
        float_param!("cell_size", "Cell Size", 1.0, 0.0001, 100.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Compose repeated sweeps with an engine convergence reduction; whitewater uses six maximum sweeps including the final converging sweep. This operation does not perform geometric nearest-triangle redistancing.",
    examples: [],
    picker: { label: "Upwind Distance", category: Atom },
    summary: "One FLIP Fluids upwind signed-distance sweep on valid cells, with recomputed smoothed sign, clamped neighbours and pseudo-time h/2. Invalid cells retain their input.",
    category: Particles3D,
    role: Filter,
    aliases: ["distance field", "upwind reinitialisation"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/upwind_distance_body.wgsl"),
    input_access: [BufferGather, BufferGather],
}

impl Primitive for UpwindDistance {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "levelset")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(input), Some(output)) = (ctx.inputs.array("levelset"), ctx.outputs.array("out"))
        else {
            return;
        };
        let count = (input.size.min(output.size) / 4) as u32;
        if count == 0 {
            return;
        }
        let dims = ["nodes_x", "nodes_y", "nodes_z"].map(|n| ctx.scalar_or_param(n, 8.0));
        let h = ctx.scalar_or_param("cell_size", 1.0);
        let Some(valid) = ctx.inputs.array("valid") else {
            return;
        };
        if dims
            .iter()
            .any(|n| !n.is_finite() || *n < 2.0 || n.fract() != 0.0)
            || !h.is_finite()
            || h <= 0.0
            || dims.iter().product::<f32>() as u64 * 4 > input.size.min(output.size).min(valid.size)
        {
            ctx.error("Upwind Distance: incomplete lattice or invalid spacing");
            return;
        }
        let uniforms = [
            dims[0].to_bits(),
            dims[1].to_bits(),
            dims[2].to_bits(),
            h.to_bits(),
            count,
            0,
            0,
            0,
        ];
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::cast_slice(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: valid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: output,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.upwind_distance",
        );
    }
}
