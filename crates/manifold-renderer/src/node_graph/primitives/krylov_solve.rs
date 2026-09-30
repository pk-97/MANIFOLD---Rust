//! `node.krylov_solve` — the GMRES coefficients y from the final small state:
//! back-substitution through the rotated Hessenberg matrix
//! (docs/FFT_WATER_SOLVER_DESIGN.md D10). A per-element atom on the codegen
//! path: each of the `passes` threads runs the whole back-substitution (at
//! most [`MAX_PASSES`]² steps) and keeps its own coefficient.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::krylov_givens::{MAX_PASSES, pass_count, pass_refusal, state_len};
use super::sort_particles_into_cells::int_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SolveUniforms {
    passes: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: KrylovSolve,
    type_id: "node.krylov_solve",
    purpose: "GMRES coefficients from the small solver state node.krylov_givens leaves: solve R y = g by back-substitution, R the rotated Hessenberg matrix's top passes × passes. A zero diagonal gives a zero coefficient, so a solve that finished early just ignores its later passes.",
    inputs: {
        state: Array(f32) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        int_param!("passes", "Passes", 24.0, 1.0, MAX_PASSES as f32),
    ],
    depth_rule: Terminal,
    composition_notes: "After the Krylov loop: state from node.krylov_basis's out; wire out as coef into node.combine_rows over the basis (rows = passes, base_scale 0) for the solution.",
    examples: [],
    picker: { label: "Krylov Solve", category: Atom },
    summary: "Works out how much of each round's guess goes into the pressure solver's answer.",
    category: MathAndConvert,
    role: Map,
    aliases: ["gmres", "back substitution", "triangular solve"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/krylov_solve_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for KrylovSolve {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| pass_count(params))
    }

    fn params_refusal(&self, params: &ParamValues) -> Option<String> {
        pass_refusal(params)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if let Some(reason) = pass_refusal(ctx.params) {
            ctx.error(format!("Krylov Solve: {reason}"));
            return;
        }
        let passes = pass_count(ctx.params);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(state), Some(out)) = (ctx.inputs.array("state"), ctx.outputs.array("out")) else {
            return;
        };
        if u64::from(state_len(passes)) * 4 > state.size || u64::from(passes) * 4 > out.size {
            ctx.error(format!("Krylov Solve: {passes} passes need a state of {}", state_len(passes)));
            return;
        }
        let uniforms = SolveUniforms { passes: passes as i32, dispatch_count: passes, _pad0: 0, _pad1: 0 };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: state, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [passes.div_ceil(256), 1, 1],
            "node.krylov_solve",
        );
    }
}
