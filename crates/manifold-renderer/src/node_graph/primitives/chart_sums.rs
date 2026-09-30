//! `node.chart_sums` — the surface helper's gather (docs/FFT_WATER_SOLVER_DESIGN.md
//! D11): one thread per chart plane element walks its line of cells and sums
//! share × value over the collar entries of its sheet. A cell is collar where
//! the running total steps up, and its entry is that total minus one, so there
//! is no sort and no scatter. A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::chart_entries::{plane_len, plane_side, sheet_count};
use crate::node_graph::fluid_particles::ChartEntry;
use super::collar_cells::{cell_count, cell_lattice};
use super::sort_particles_into_cells::{float_param, int_param};
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChartUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    sheets: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: ChartSums,
    type_id: "node.chart_sums",
    purpose: "The six-view surface helper's gather: planes of 6 views × `sheets` × M × M (M the longest lattice side; view v = 2·axis + 0 for +, 1 for −), element (v, s, j, i) the sum over the collar entries on the line along the view's axis through (i, j) in the other two axes (lower axis first) with sheet s in view v of share_v × value[entry]. Planes past a shorter side are 0. Entries come from node.chart_entries; `total` is the collar's inclusive running total, `value` a collar vector (entries first).",
    inputs: {
        total: Array(u32) required,
        entries: Array(ChartEntry) required,
        value: Array(f32) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
        int_param!("sheets", "Sheets", 4.0, 1.0, 16.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Surface helper, forward half: chart_sums → cosine_reorder → fft_3d → cosine_spectrum → cosine_surface_scale → cosine_half_spectrum → inverse_fft_3d → cosine_reorder (direction 1), the transform atoms with axes 2 on nodes M × M × (6 · sheets); then node.chart_spread brings the planes back to the entries.",
    examples: [],
    picker: { label: "Chart Sums", category: Atom },
    summary: "Flattens the water surface onto six viewing planes so the pressure solver can smooth it with a fast transform.",
    category: Particles3D,
    role: Filter,
    aliases: ["surface charts", "column sums", "project surface"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/chart_sums_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather],
}

impl Primitive for ChartSums {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out")
            .then(|| cell_lattice(params).map(|nodes| plane_len(nodes, sheet_count(params)) as u32))
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Chart Sums: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let sheets = sheet_count(ctx.params);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(total), Some(entries), Some(value), Some(out)) = (
            ctx.inputs.array("total"),
            ctx.inputs.array("entries"),
            ctx.inputs.array("value"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let planes = plane_len(nodes, sheets);
        if cell_count(nodes) * 4 > total.size || planes * 4 > out.size || entries.size < 32 || value.size < 4 {
            ctx.error(format!("Chart Sums: a {nodes:?} lattice with {sheets} sheets is larger than its arrays"));
            return;
        }
        debug_assert_eq!(planes, 6 * u64::from(sheets) * u64::from(plane_side(nodes)).pow(2));
        let uniforms = ChartUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            sheets: sheets as i32,
            dispatch_count: planes as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: total, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: entries, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: value, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
            ],
            [(planes as u32).div_ceil(256), 1, 1],
            "node.chart_sums",
        );
    }
}
