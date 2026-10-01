//! `node.dot_products` — dot products of a vector with the rows of a
//! row-major matrix, all on the GPU (the conjugate gradient's r·z and p·s,
//! docs/GPU_FLIP_PRESSURE_SOLVE.md). A barriered two-pass reduction
//! (docs/ADDING_PRIMITIVES.md exclusion 1): workgroup partial sums per row,
//! then one thread per row adds its partials in a fixed order.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use super::sort_particles_into_cells::int_param;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const SHADER: &str = include_str!("shaders/dot_products.wgsl");

/// Rows one node can reduce: the finalize pass is one 64-thread workgroup.
pub(crate) const MAX_ROWS: u32 = 64;
/// Partial sums per row, at most; one per 1024 elements below that.
const MAX_GROUPS: u32 = 64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DotParams {
    length: u32,
    rows: u32,
    groups: u32,
    root: u32,
    has_vector: u32,
    max_rows: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: DotProducts,
    type_id: "node.dot_products",
    purpose: "Dot products of an Array<f32> vector with each of the first `rows` rows of a row-major matrix (row r at matrix[r·row_length ..]), into out[0 .. max_rows); rows past `rows` read 0. With no vector wired, each row's sum. `root` takes the square root of each result (max(result, 0) first), which turns a vector dotted with itself into its length. Everything stays on the GPU; the sums run in a fixed order.",
    inputs: {
        matrix: Array(f32) required,
        vector: Array(f32) optional,
        rows: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        int_param!("row_length", "Row Length", 1024.0, 1.0, 16_777_216.0),
        int_param!("rows", "Rows", 1.0, 0.0, 64.0),
        int_param!("max_rows", "Max Rows", 1.0, 1.0, 64.0),
        int_param!("root", "Square Root", 0.0, 0.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The conjugate gradient's reductions (node.conjugate_gradient): r·z and p·s are one row each (matrix = one vector, vector = the other, rows 1). Also a vector's length (the vector wired to both matrix and vector, root 1), a sum (no vector; row_length says how many leading elements) and projections on several rows at once. Pair with node.divide_by_value for a ratio of two dots, and node.combine_rows, which consumes dots as coefficients.",
    examples: [],
    picker: { label: "Dot Products", category: Atom },
    summary: "Measures how much a list of numbers lines up with each row of a table, all on the GPU.",
    category: MathAndConvert,
    role: Map,
    aliases: ["dot product", "inner product", "norm", "projection", "reduce", "sum"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        partial: Option<GpuComputePipeline> = None,
        finalize: Option<GpuComputePipeline> = None,
        partials: Option<GpuBuffer> = None,
    },
}

fn whole(params: &ParamValues, name: &str, default: u32) -> u32 {
    match params.get(name) {
        Some(ParamValue::Float(v)) => v.round().max(0.0) as u32,
        _ => default,
    }
}

impl Primitive for DotProducts {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| whole(params, "max_rows", 1).clamp(1, MAX_ROWS))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let length = whole(ctx.params, "row_length", 1024).max(1);
        let max_rows = whole(ctx.params, "max_rows", 1).clamp(1, MAX_ROWS);
        let rows = (ctx.scalar_or_param("rows", 1.0).round().max(0.0) as u32).min(max_rows);
        let root = whole(ctx.params, "root", 0).min(1);
        let groups = length.div_ceil(1024).clamp(1, MAX_GROUPS);
        {
            let gpu = ctx.gpu_encoder();
            if self.partial.is_none() {
                self.partial = Some(gpu.device.create_compute_pipeline(SHADER, "partial_main", "node.dot_products"));
                self.finalize = Some(gpu.device.create_compute_pipeline(SHADER, "finalize_main", "node.dot_products"));
            }
            let bytes = u64::from(MAX_ROWS * MAX_GROUPS) * 4;
            if self.partials.is_none() {
                self.partials = Some(gpu.device.create_buffer(bytes));
            }
        }
        let (Some(matrix), Some(out)) = (ctx.inputs.array("matrix"), ctx.outputs.array("out")) else {
            return;
        };
        let vector = ctx.inputs.array("vector");
        if u64::from(rows) * u64::from(length) * 4 > matrix.size
            || vector.is_some_and(|v| u64::from(length) * 4 > v.size)
            || u64::from(max_rows) * 4 > out.size
        {
            ctx.error(format!("Dot Products: {rows} rows of {length} are larger than the arrays"));
            return;
        }
        let uniforms = DotParams {
            length,
            rows,
            groups,
            root,
            has_vector: u32::from(vector.is_some()),
            max_rows,
            _pad0: 0,
            _pad1: 0,
        };
        let partials = self.partials.as_ref().expect("partials allocated");
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: matrix, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: vector.unwrap_or(matrix), offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: partials, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
        ];
        let gpu = ctx.gpu_encoder();
        if rows > 0 {
            gpu.native_enc.dispatch_compute(
                self.partial.as_ref().expect("pipeline created"),
                &bindings,
                [groups, rows, 1],
                "node.dot_products.partial",
            );
        }
        gpu.native_enc.dispatch_compute(
            self.finalize.as_ref().expect("pipeline created"),
            &bindings,
            [1, 1, 1],
            "node.dot_products.finalize",
        );
    }
}
