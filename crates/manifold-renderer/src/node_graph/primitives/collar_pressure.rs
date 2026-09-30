//! `node.collar_pressure` — the water's pressure from the collar solve
//! (docs/FFT_WATER_SOLVER_DESIGN.md D3, D10): p = G f − G Jᵀλ + c in water
//! cells, 0 in air, reusing the box solve of the divergence from before the
//! Krylov loop. A per-element atom on the codegen path.

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: no params, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PressureUniforms {
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: CollarPressure,
    type_id: "node.collar_pressure",
    purpose: "Pressure on a water lattice from the collar solve: out[c] = solved[c] − correction[c] + constant where water[c] > 0.5, else 0. constant is the last element of the solved collar vector λ (its c).",
    inputs: {
        water: Array(f32) required,
        solved: Array(f32) required,
        correction: Array(f32) required,
        vector: Array(f32) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "The FFT water solve's last step: solved is the box solve of the divergence (made before the Krylov loop), correction the box solve of node.collar_source(λ), vector λ itself (node.chart_spread of the Krylov solution). Air cells read 0: the free surface.",
    examples: [],
    picker: { label: "Collar Pressure", category: Atom },
    summary: "Finishes the water's pressure from the solver's answer, zero in the air.",
    category: Particles3D,
    role: Filter,
    aliases: ["pressure", "free surface pressure"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/collar_pressure_body.wgsl"),
    input_access: [Coincident, Coincident, Coincident, BufferGather],
}

impl Primitive for CollarPressure {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "water").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(water), Some(solved), Some(correction), Some(vector), Some(out)) = (
            ctx.inputs.array("water"),
            ctx.inputs.array("solved"),
            ctx.inputs.array("correction"),
            ctx.inputs.array("vector"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let count = (water.size.min(solved.size).min(correction.size).min(out.size) / 4) as u32;
        if count == 0 || vector.size < 4 {
            ctx.error("Collar Pressure: empty arrays".to_string());
            return;
        }
        let uniforms = PressureUniforms { dispatch_count: count, _pad0: 0, _pad1: 0, _pad2: 0 };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: water, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: solved, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: correction, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: vector, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.collar_pressure",
        );
    }
}
