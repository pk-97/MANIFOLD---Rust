//! `node.chart_spread` — the surface helper's spread
//! (docs/FFT_WATER_SOLVER_DESIGN.md D4, D11): chart planes back onto the
//! collar entries, each entry reading its six slots weighted by its shares,
//! plus the helper's local term (2/h) × the entry's own value. The constant
//! at the end of a collar vector passes through. A per-element gather on the
//! codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::chart_entries::{plane_len, sheet_count};
use crate::node_graph::fluid_particles::ChartEntry;
use super::collar_cells::cell_lattice;
use super::sort_particles_into_cells::{float_param, int_param};
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpreadUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    sheets: i32,
    cell_size: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: ChartSpread,
    type_id: "node.chart_spread",
    purpose: "The six-view surface helper's spread, one thread per element of a collar vector (the entries, then one constant): out[e] = Σ_v share_v(e) · planes[slot_v(e)] + (2 / cell_size) · value[e], slot_v the entry's chart slot in view v as node.chart_sums lays them out. The element after the entries is value's constant, unchanged; empty entries give 0.",
    inputs: {
        entries: Array(ChartEntry) required,
        planes: Array(f32) required,
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
        float_param!("cell_size", "Cell Size", 0.0625, 1e-6, 1e6),
    ],
    depth_rule: Terminal,
    composition_notes: "Surface helper, back half: planes from the inverse plane transform after node.cosine_surface_scale with offset 2 / cell_size (which removes the local term inside the planes; this atom adds it back per entry), value the same collar vector node.chart_sums read. Output is the helper applied to value, the same length.",
    examples: [],
    picker: { label: "Chart Spread", category: Atom },
    summary: "Brings the smoothed surface back from the six viewing planes onto the water's edge.",
    category: Particles3D,
    role: Filter,
    aliases: ["surface charts", "spread back", "unproject surface"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/chart_spread_body.wgsl"),
    input_access: [BufferGather, BufferGather, Coincident],
}

impl Primitive for ChartSpread {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "value").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Chart Spread: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let sheets = sheet_count(ctx.params);
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625).max(1e-6);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(entries), Some(planes), Some(value), Some(out)) = (
            ctx.inputs.array("entries"),
            ctx.inputs.array("planes"),
            ctx.inputs.array("value"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let count = (value.size.min(out.size) / 4) as u32;
        if plane_len(nodes, sheets) * 4 > planes.size || entries.size < 32 || count == 0 {
            ctx.error(format!("Chart Spread: a {nodes:?} lattice with {sheets} sheets is larger than its arrays"));
            return;
        }
        let uniforms = SpreadUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            sheets: sheets as i32,
            cell_size,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: entries, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: planes, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: value, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.chart_spread",
        );
    }
}
