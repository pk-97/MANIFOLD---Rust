//! `node.matter_stats` — once per tick, reduce a matter domain's points and
//! grid to 16 words: non-finite and clamp counts, live count, speed, J
//! range, volume, fixed-point headroom, mass, momentum and energy
//! (`docs/GPU_MPM_SOLVER_DESIGN.md` D14). A fixed reduction tree keeps the
//! sums deterministic. Exempt from the codegen mandate as a barriered
//! reduction (ADDING_PRIMITIVES.md exclusion 1): the generated per-element
//! wrapper returns early per thread, which would make its barriers
//! non-uniform.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use crate::exec::effect_node::EffectNodeContext;
use crate::water::liquid::lattice::LiquidLattice;
use crate::water::matter::{MatterGridNode, MatterPoint, STATS_WORDS};
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::primitive::Primitive;

const SHADER: &str = include_str!("shaders/matter_stats.wgsl");
/// Elements one workgroup folds: 256 threads × 8.
const BLOCK: u32 = 256 * 8;
/// Bytes of one partial record.
const PARTIAL_BYTES: u64 = 64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StatsParams {
    point_count: u32,
    node_count: u32,
    point_groups: u32,
    node_groups: u32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    lambda: f32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    cohesion: f32,
    density: f32,
    tick_index: u32,
    _pad0: u32,
    _pad1: u32,
}

pub struct StatsPipelines {
    points: GpuComputePipeline,
    grid: GpuComputePipeline,
    finish: GpuComputePipeline,
}

impl StatsPipelines {
    fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            points: device.create_compute_pipeline(SHADER, "points_main", "node.matter_stats.points"),
            grid: device.create_compute_pipeline(SHADER, "grid_main", "node.matter_stats.grid"),
            finish: device.create_compute_pipeline(SHADER, "finish_main", "node.matter_stats.finish"),
        }
    }
}

crate::primitive! {
    name: MatterStats,
    type_id: "node.matter_stats",
    purpose: "Once per tick (when tick_end is 1), reduce a matter domain's points and grid to 16 statistics words: non-finite count, clamped nodes, live points, max speed, J range, volume, largest accumulator magnitude, mass, momentum, and kinetic, potential and elastic energy, plus the tick index. Sums run in a fixed order, so they are deterministic.",
    inputs: {
        points: Array(MatterPoint) required,
        grid: Array(MatterGridNode) required,
        accum: Array(i32) required,
        stats: Array(u32) required,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        lambda: ScalarF32 optional,
        cohesion: ScalarF32 optional,
        density: ScalarF32 optional,
        active_count: ScalarF32 optional,
        tick_end: ScalarF32 optional,
        tick_index: ScalarF32 optional,
    },
    outputs: {
        stats_out: Array(u32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("lattice_min_x"), label: "Lattice Min X", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_y"), label: "Lattice Min Y", ty: ParamType::Float, default: ParamValue::Float(-0.1875), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_z"), label: "Lattice Min Z", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_x"), label: "Nodes X", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_y"), label: "Nodes Y", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_z"), label: "Nodes Z", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lambda"), label: "Bulk Stiffness (Pa)", ty: ParamType::Float, default: ParamValue::Float(1.111e6), range: Some((0.0, 1.0e9)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cohesion"), label: "Cohesion", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("density"), label: "Density (kg/m³)", ty: ParamType::Float, default: ParamValue::Float(1000.0), range: Some((1.0, 1.0e5)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("active_count"), label: "Active Count", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_000_000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_end"), label: "Tick End", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_index"), label: "Tick Index", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0e9)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Last atom of the Live Matter region body. stats/stats_out alias node.matter_state's stats array, whose final value escapes the region to node.matter_frame (which refuses to publish a non-finite tick) and to matter_state's readback (live count, fault). Gate it with the boundary's tick_end so it runs once per tick; a skipped substep leaves the array as it was. Words: 0 non-finite, 1 clamped nodes, 2 live, 3 max speed, 4 min J, 5 max J, 6 volume, 7 max |accumulator|, 8 mass, 9-11 momentum, 12 kinetic, 13 potential, 14 elastic, 15 tick.",
    examples: ["WaterDamBreakMatter", "WaterStillPoolMatter"],
    picker: { label: "Matter Stats", category: Atom },
    summary: "Measures the liquid once per tick: how much there is, how fast it moves, its energy, and whether anything went wrong.",
    category: Particles3D,
    role: Filter,
    aliases: ["matter stats", "mpm diagnostics", "energy"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        pipelines: Option<StatsPipelines> = None,
        partials: Option<GpuBuffer> = None,
    },
}

impl Primitive for MatterStats {
    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.pipelines.is_none() {
            self.pipelines = Some(StatsPipelines::new(device));
        }
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "stats_out")
            .then(|| input_capacities.iter().find(|(p, _)| *p == "stats").map(|&(_, n)| n))
            .flatten()
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("stats", "stats_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(lattice) = LiquidLattice::from_wires(ctx, "Matter Stats") else {
            return;
        };
        let run_now = ctx.scalar_or_param("tick_end", 1.0) > 0.5;
        let requested = ctx.scalar_or_param("active_count", 0.0).round().max(0.0) as u32;
        let tick_index = ctx.scalar_or_param("tick_index", 0.0).round().max(0.0) as u32;
        let lambda = ctx.scalar_or_param("lambda", 1.111e6);
        let cohesion = ctx.scalar_or_param("cohesion", 0.0);
        let density = ctx.scalar_or_param("density", 1000.0);
        let gravity = [
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
        ];
        let points = ctx.inputs.array("points");
        let grid = ctx.inputs.array("grid");
        let accum = ctx.inputs.array("accum");
        let stats = ctx.inputs.array("stats");
        // In place on the stats input; a gated substep leaves it untouched.
        ctx.mark_gpu_accessed();
        if !run_now {
            return;
        }
        let (Some(points), Some(grid), Some(accum), Some(stats)) = (points, grid, accum, stats)
        else {
            return;
        };
        if stats.size < u64::from(STATS_WORDS) * 4 {
            ctx.error("Matter stats: the stats array holds fewer than 16 words");
            return;
        }
        let point_count = requested.min((points.size / std::mem::size_of::<MatterPoint>() as u64) as u32);
        let node_count = lattice
            .node_count()
            .min((grid.size / 32) as u32)
            .min((accum.size / 16) as u32);
        let point_groups = point_count.div_ceil(BLOCK);
        let node_groups = node_count.div_ceil(BLOCK);
        let partial_bytes = u64::from((point_groups + node_groups).max(1)) * PARTIAL_BYTES;
        let gpu = ctx.gpu_encoder();
        let pipelines = self.pipelines.as_ref().expect("matter stats pipelines built by prepare_pipelines at install");
        if self.partials.as_ref().is_none_or(|b| b.size < partial_bytes) {
            self.partials = Some(gpu.device.create_buffer(partial_bytes));
        }
        let partials = self.partials.as_ref().expect("partials prepared");
        let params = StatsParams {
            point_count,
            node_count,
            point_groups,
            node_groups,
            lattice_min_x: lattice.min()[0],
            lattice_min_y: lattice.min()[1],
            lattice_min_z: lattice.min()[2],
            lambda,
            gravity_x: gravity[0],
            gravity_y: gravity[1],
            gravity_z: gravity[2],
            cohesion,
            density,
            tick_index,
            _pad0: 0,
            _pad1: 0,
        };
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
            GpuBinding::Buffer { binding: 1, buffer: points, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: grid, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: accum, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: partials, offset: 0 },
            GpuBinding::Buffer { binding: 5, buffer: stats, offset: 0 },
        ];
        if point_groups > 0 {
            gpu.native_enc.dispatch_compute(&pipelines.points, &bindings, [point_groups, 1, 1], "node.matter_stats.points");
        }
        if node_groups > 0 {
            gpu.native_enc.dispatch_compute(&pipelines.grid, &bindings, [node_groups, 1, 1], "node.matter_stats.grid");
        }
        gpu.native_enc.dispatch_compute(&pipelines.finish, &bindings, [1, 1, 1], "node.matter_stats.finish");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_stats_params_match_the_shader_struct() {
        assert_eq!(std::mem::size_of::<StatsParams>(), 64);
        assert!(SHADER.contains("struct StatsParams"));
        assert_eq!(STATS_WORDS, 16);
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
