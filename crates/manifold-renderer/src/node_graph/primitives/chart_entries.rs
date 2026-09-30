//! `node.chart_entries` — each collar entry's place in the six-view surface
//! helper (docs/FFT_WATER_SOLVER_DESIGN.md D4, D11): its share of each signed
//! view (±x, ±y, ±z), its sheet in each view, and its cell. Computed once per
//! step; node.chart_sums and node.chart_spread read it every pass. A
//! per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::collar_cells::{cell_count, cell_lattice};
use super::sort_particles_into_cells::{float_param, int_param};
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::ChartEntry;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Sheets per view, 1 to 16 (4 bits each in `ChartEntry::sheets`).
pub(crate) fn sheet_count(params: &ParamValues) -> u32 {
    match params.get("sheets") {
        Some(ParamValue::Float(v)) => v.round().clamp(1.0, 16.0) as u32,
        _ => 4,
    }
}

/// The chart planes are M × M with M the longest lattice side.
pub(super) fn plane_side(nodes: [u32; 3]) -> u32 {
    nodes.into_iter().max().unwrap_or(1)
}

/// Floats in the stacked chart planes: six views × sheets × M².
pub(crate) fn plane_len(nodes: [u32; 3], sheets: u32) -> u64 {
    let side = u64::from(plane_side(nodes));
    6 * u64::from(sheets) * side * side
}

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChartEntryUniforms {
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
    name: ChartEntries,
    type_id: "node.chart_entries",
    purpose: "For each collar entry (a cell index from node.select_flagged), its place in the six signed views of the surface helper. Outward normal n = −∇(smoothed water) by clamped central differences; the share of view ±a is max(±n_a, 0) over √D, D the number of collar cells in the same chart slot. The slot of view ±a is the entry's line along a and its sheet: the water runs wholly on the −a side (+a view) or +a side (−a view), minus one, capped at sheets − 1. Empty entries (past the collar) come out zero with cell 4294967295.",
    inputs: {
        entries: Array(u32) required,
        water: Array(f32) required,
        smoothed: Array(f32) required,
        collar: Array(u32) required,
    },
    outputs: {
        out: Array(ChartEntry),
    },
    params: [
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
        int_param!("sheets", "Sheets", 4.0, 1.0, 16.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Once per water step, after node.select_flagged: water is the 1/0 water lattice, smoothed is it blurred by three node.smooth_lattice (axes 0, 1, 2), collar is node.collar_cells' flags. Feeds node.chart_sums and node.chart_spread, which must use the same lattice and sheets.",
    examples: [],
    picker: { label: "Chart Entries", category: Atom },
    summary: "Works out which way each bit of water surface faces, so the pressure solver can look at it from the right side.",
    category: Particles3D,
    role: Filter,
    aliases: ["surface charts", "surface normals", "sheet index"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/chart_entries_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather],
}

impl Primitive for ChartEntries {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "entries").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Chart Entries: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let sheets = sheet_count(ctx.params);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(entries), Some(water), Some(smoothed), Some(collar), Some(out)) = (
            ctx.inputs.array("entries"),
            ctx.inputs.array("water"),
            ctx.inputs.array("smoothed"),
            ctx.inputs.array("collar"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let cells = cell_count(nodes) * 4;
        let count = (entries.size / 4).min(out.size / 32);
        if cells > water.size.min(smoothed.size).min(collar.size) || count == 0 {
            ctx.error(format!("Chart Entries: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = ChartEntryUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            sheets: sheets as i32,
            dispatch_count: count as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: entries, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: water, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: smoothed, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: collar, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.chart_entries",
        );
    }
}
