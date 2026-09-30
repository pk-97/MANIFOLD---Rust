//! `node.krylov_givens` — one GMRES pass's update of the small solver state:
//! the new Hessenberg column rotated by the earlier Givens rotations, the new
//! rotation, and the residual vector g (docs/FFT_WATER_SOLVER_DESIGN.md D10).
//! A per-element atom on the codegen path: each thread re-derives column j
//! (at most 32 rotations), so no thread waits on another.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::int_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Passes the fixed-size local arrays of the Krylov atoms hold.
pub(super) const MAX_PASSES: u32 = 32;

/// Floats in the small state for `passes`: the Hessenberg matrix
/// ((passes + 1) × passes, column-major), cs and sn (passes each), g (passes + 1).
pub(super) fn state_len(passes: u32) -> u32 {
    passes * passes + 4 * passes + 1
}

/// Offset of g inside the small state.
pub(super) fn residual_offset(passes: u32) -> u32 {
    passes * (passes + 1) + 2 * passes
}

pub(super) fn pass_count(params: &ParamValues) -> u32 {
    match params.get("passes") {
        Some(ParamValue::Float(v)) => (v.round().max(1.0) as u32).min(MAX_PASSES),
        _ => 24,
    }
}

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GivensUniforms {
    passes: i32,
    column: i32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: KrylovGivens,
    type_id: "node.krylov_givens",
    purpose: "One GMRES pass on the small solver state (Array<f32>: Hessenberg (passes + 1) × passes column-major, then cs and sn, then g). Column `column` (the pass index) is first + second (the two Gram–Schmidt projections) with norm[0] below; it is rotated by the earlier rotations, a new rotation zeroes its last entry, and g is rotated with it, so |g[column + 1]| is the residual after this pass. Rotations of a zero column are the identity.",
    inputs: {
        state: Array(f32) required,
        first: Array(f32) required,
        second: Array(f32) required,
        norm: Array(f32) required,
        column: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        int_param!("passes", "Passes", 24.0, 1.0, 32.0),
        int_param!("column", "Column", 0.0, 0.0, 31.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Region body of the Krylov loop: state from node.krylov_basis's out, column from its per-pass `pass` scalar, first and second from the two node.dot_products projections, norm from the node.dot_products length of the new vector; out closes back into node.krylov_basis's in. node.krylov_solve reads the final state.",
    examples: [],
    picker: { label: "Krylov Givens", category: Atom },
    summary: "Updates the pressure solver's small bookkeeping table after each round.",
    category: MathAndConvert,
    role: Map,
    aliases: ["gmres", "givens rotation", "hessenberg"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/krylov_givens_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather],
}

impl Primitive for KrylovGivens {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| state_len(pass_count(params)))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let passes = pass_count(ctx.params);
        let column = ctx.scalar_or_param("column", 0.0).round().max(0.0) as u32;
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(state), Some(first), Some(second), Some(norm), Some(out)) = (
            ctx.inputs.array("state"),
            ctx.inputs.array("first"),
            ctx.inputs.array("second"),
            ctx.inputs.array("norm"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let len = state_len(passes);
        let projection_bytes = u64::from(passes + 1) * 4;
        if u64::from(len) * 4 > state.size.min(out.size) || projection_bytes > first.size.min(second.size) || norm.size < 4 {
            ctx.error(format!("Krylov Givens: {passes} passes need a state of {len} and projections of {}", passes + 1));
            return;
        }
        let uniforms = GivensUniforms { passes: passes as i32, column: column as i32, dispatch_count: len, _pad0: 0 };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: state, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: first, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: second, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: norm, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: out, offset: 0 },
            ],
            [len.div_ceil(256), 1, 1],
            "node.krylov_givens",
        );
    }
}
