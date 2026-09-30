//! `node.combine_rows` — a vector plus a weighted sum of matrix rows: the
//! conjugate gradient's x − α p and r − α s (docs/GPU_FLIP_PRESSURE_SOLVE.md).
//! A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::{float_param, int_param};
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CombineUniforms {
    row_length: i32,
    rows: i32,
    scale: f32,
    base_scale: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: CombineRows,
    type_id: "node.combine_rows",
    purpose: "out[e] = base_scale · base[e] + scale · Σ_{i < rows} coef[i] · matrix[i·row_length + e] over Array<f32>s, for e < row_length: a vector plus a weighted sum of the first `rows` rows of a row-major matrix, the weights read from another array on the GPU.",
    inputs: {
        base: Array(f32) required,
        matrix: Array(f32) required,
        coef: Array(f32) required,
        rows: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        int_param!("row_length", "Row Length", 1024.0, 1.0, 16_777_216.0),
        int_param!("rows", "Rows", 1.0, 0.0, 64.0),
        float_param!("scale", "Row Scale", 1.0, -1e6, 1e6),
        float_param!("base_scale", "Base Scale", 1.0, -1e6, 1e6),
    ],
    depth_rule: Terminal,
    composition_notes: "The conjugate gradient's vector updates (node.conjugate_gradient): x − α p is base = x, matrix = p, coef = α from node.divide_by_value, rows 1, scale −1. With node.dot_products and several rows it is Gram–Schmidt: w − V h is base = w, matrix = the basis, coef = the dots, scale −1.",
    examples: [],
    picker: { label: "Combine Rows", category: Atom },
    summary: "Adds weighted rows of a table onto a list of numbers.",
    category: MathAndConvert,
    role: Map,
    aliases: ["axpy", "linear combination", "gram schmidt", "matrix vector"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/combine_rows_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather],
}

fn whole(params: &ParamValues, name: &str, default: u32) -> u32 {
    match params.get(name) {
        Some(ParamValue::Float(v)) => v.round().max(0.0) as u32,
        _ => default,
    }
}

impl Primitive for CombineRows {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| whole(params, "row_length", 1024).max(1))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let length = whole(ctx.params, "row_length", 1024).max(1);
        let rows = ctx.scalar_or_param("rows", 1.0).round().clamp(0.0, 64.0) as u32;
        let scale = ctx.scalar_or_param("scale", 1.0);
        let base_scale = ctx.scalar_or_param("base_scale", 1.0);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(base), Some(matrix), Some(coef), Some(out)) = (
            ctx.inputs.array("base"),
            ctx.inputs.array("matrix"),
            ctx.inputs.array("coef"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let bytes = u64::from(length) * 4;
        if bytes > base.size.min(out.size) || bytes * u64::from(rows) > matrix.size || u64::from(rows) * 4 > coef.size {
            ctx.error(format!("Combine Rows: {rows} rows of {length} are larger than the arrays"));
            return;
        }
        let uniforms = CombineUniforms {
            row_length: length as i32,
            rows: rows as i32,
            scale,
            base_scale,
            dispatch_count: length,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: base, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: matrix, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: coef, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
            ],
            [length.div_ceil(256), 1, 1],
            "node.combine_rows",
        );
    }
}
