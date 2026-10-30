//! `node.matter_stats` — once per tick, reduce a matter domain's points and
//! grid to 19 words: non-finite and clamp counts, live count, speed, J
//! range, volume, fixed-point headroom, mass, momentum, energy, points the
//! pool ran short of, its held tiles and snow's peak hardening. Each point
//! reads its own row of the material table. A fixed reduction tree keeps the
//! sums deterministic. Exempt from the codegen mandate as a barriered
//! reduction (ADDING_PRIMITIVES.md exclusion 1): the generated per-element
//! wrapper returns early per thread, which would make its barriers
//! non-uniform.

use manifold_gpu::BufferUse;
use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use crate::matter::{MatterGridNode, MatterPoint, STATS_WORDS, grid_accum_bytes, grid_bytes};
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, pool_len, table_words};

use super::matter_frame::FrameParams;

static SHADER: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{}\n{}",
        include_str!("shaders/matter_material.wgsl"),
        include_str!("shaders/matter_stats.wgsl")
    )
});
/// Elements one workgroup folds: 256 threads × 8.
const BLOCK: u32 = 256 * 8;
/// Bytes of one partial record: 18 words.
const PARTIAL_BYTES: u64 = 72;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StatsParams {
    point_count: u32,
    node_count: u32,
    point_groups: u32,
    node_groups: u32,
    field_min_x: f32,
    field_min_y: f32,
    field_min_z: f32,
    tick_index: u32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    _pad0: u32,
}

pub struct StatsPipelines {
    points: GpuComputePipeline,
    grid: GpuComputePipeline,
    finish: GpuComputePipeline,
    view: GpuComputePipeline,
}

impl StatsPipelines {
    fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            points: device.create_compute_pipeline(
                SHADER.as_str(),
                "points_main",
                "node.matter_stats.points",
            ),
            grid: device.create_compute_pipeline(
                SHADER.as_str(),
                "grid_main",
                "node.matter_stats.grid",
            ),
            finish: device.create_compute_pipeline(
                SHADER.as_str(),
                "finish_main",
                "node.matter_stats.finish",
            ),
            view: device.create_compute_pipeline(
                super::matter_frame::SHADER,
                "cs_main",
                "node.matter_stats.view",
            ),
        }
    }
}

manifold_node_engine::primitive! {
    name: MatterStats,
    type_id: "node.matter_stats",
    purpose: "Once per tick (when tick_end is 1), reduce a matter domain's points and grid to 19 statistics words: non-finite count, clamped nodes, live points, max speed, J range, volume, largest accumulator magnitude, mass, momentum, and kinetic, potential and elastic energy, plus the tick index, the points removed for want of a pool tile, the pool's held tiles and snow's peak hardening. Each point's mass and elastic energy come from its row of the material table. Sums run in a fixed order, so they are deterministic.",
    inputs: {
        points: Array(MatterPoint) required,
        grid: Array(MatterGridNode) required,
        accum: Array(i32) required,
        tiles: Array(u32) required,
        stats: Array(u32) required,
        materials: Array(f32) required,
        field_min_x: ScalarF32 optional, field_min_y: ScalarF32 optional, field_min_z: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        active_count: ScalarF32 optional,
        tick_end: ScalarF32 optional,
        tick_index: ScalarF32 optional,
    },
    outputs: {
        stats_out: Array(u32),
        // Every point as an id-sorted FluidParticle at the end of each tick,
        // for a liquid stepping the same tick (node.gpu_flip_step's sand);
        // written only while read. A non-finite tick keeps the last view.
        fluid_view: Array(FluidParticle),
        view_count: ScalarF32,
    },
    params: [
        ParamDef { name: Cow::Borrowed("field_min_x"), label: "Field Min X", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_min_y"), label: "Field Min Y", ty: ParamType::Float, default: ParamValue::Float(-0.1875), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_min_z"), label: "Field Min Z", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pool_tiles"), label: "Pool Tiles", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((1.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("active_count"), label: "Active Count", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_000_000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_end"), label: "Tick End", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_index"), label: "Tick Index", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0e9)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Last atom of the Live Matter region body. fluid_view and view_count feed a Water's node.gpu_flip_step sand and sand_count, which steps each World tick after Matter's (the two regions co-iterate). stats/stats_out alias node.matter_state's stats array, whose final value escapes the region to node.matter_frame (which refuses to publish a non-finite tick) and to matter_state's readback (live count, fault). Gate it with the boundary's tick_end so it runs once per tick; a skipped substep leaves the array as it was. Words: 0 non-finite, 1 clamped nodes, 2 live, 3 max speed, 4 min J, 5 max J, 6 volume, 7 max |accumulator|, 8 mass, 9-11 momentum, 12 kinetic, 13 potential, 14 elastic, 15 tick, 16 points removed because their tile had no pool slot (id 0 with volume ratio 0), 17 held pool tiles, 18 the largest hardening a snow point's packing asks for (0 with no snow). The grid and accum cover 512 * pool_tiles + 1 nodes; the canonical last node is not reduced.",
    examples: ["SandColumn"],
    picker: { label: "Matter Stats", category: Atom },
    summary: "Measures the liquid once per tick: how much there is, how fast it moves, its energy, and whether anything went wrong.",
    category: Particles3D,
    role: Filter,
    aliases: ["matter stats", "mpm diagnostics", "energy"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        pipelines: Option<StatsPipelines> = None,
        partials: Option<GpuBuffer> = None,
        view: Option<GpuBuffer> = None,
        view_count: u32 = 0,
    },
}

impl Primitive for MatterStats {
    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.pipelines.is_none() {
            self.pipelines = Some(StatsPipelines::new(device));
        }
    }

    fn provides_array_output(&self, port: &str) -> bool {
        port == "fluid_view"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "fluid_view")
            .then_some(self.view.as_ref())
            .flatten()
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "fluid_view" {
            // Provided storage: a one-record hint, grown at run time.
            return Some(1);
        }
        (port_name == "stats_out")
            .then(|| {
                input_capacities
                    .iter()
                    .find(|(p, _)| *p == "stats")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("stats", "stats_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // The potential's datum: potential energy is measured from field_min.
        let field_min = ["field_min_x", "field_min_y", "field_min_z"]
            .map(|name| ctx.scalar_or_param(name, 0.0));
        let pool_tiles = ctx
            .scalar_or_param("pool_tiles", 1.0)
            .round()
            .clamp(1.0, 16_777_216.0) as u32;
        let run_now = ctx.scalar_or_param("tick_end", 1.0) > 0.5;
        let requested = ctx.scalar_or_param("active_count", 0.0).round().max(0.0) as u32;
        let tick_index = ctx.scalar_or_param("tick_index", 0.0).round().max(0.0) as u32;
        let gravity = [
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
        ];
        let points = ctx.inputs.array("points");
        let grid = ctx.inputs.array("grid");
        let accum = ctx.inputs.array("accum");
        let tiles = ctx.inputs.array("tiles");
        let stats = ctx.inputs.array("stats");
        let materials = ctx.inputs.array("materials");
        let wants_view = ctx.outputs.slot("fluid_view").is_some();
        // In place on the stats input; a gated substep leaves it untouched.
        ctx.mark_gpu_accessed();
        if !run_now {
            ctx.outputs
                .set_scalar("view_count", ParamValue::Float(self.view_count as f32));
            return;
        }
        let (Some(points), Some(grid), Some(accum), Some(tiles), Some(stats), Some(materials)) =
            (points, grid, accum, tiles, stats, materials)
        else {
            return;
        };
        if stats.size < u64::from(STATS_WORDS) * 4 {
            ctx.error(format!(
                "Matter stats: the stats array holds fewer than {STATS_WORDS} words"
            ));
            return;
        }
        if grid.size < grid_bytes(pool_tiles)
            || accum.size < grid_accum_bytes(pool_tiles)
            || tiles.size < table_words(pool_tiles) * 4
        {
            ctx.error(format!(
                "Matter stats: the grid, accumulator or tile table is shorter than a pool of {pool_tiles} tiles"
            ));
            return;
        }
        let point_count =
            requested.min((points.size / std::mem::size_of::<MatterPoint>() as u64) as u32);
        // The canonical node (last) is excluded.
        let node_count = (pool_len(pool_tiles) - 1) as u32;
        let point_groups = point_count.div_ceil(BLOCK);
        let node_groups = node_count.div_ceil(BLOCK);
        let partial_bytes = u64::from((point_groups + node_groups).max(1)) * PARTIAL_BYTES;
        let gpu = ctx.gpu_encoder();
        let pipelines = self
            .pipelines
            .as_ref()
            .expect("matter stats pipelines built by prepare_pipelines at install");
        if self
            .partials
            .as_ref()
            .is_none_or(|b| b.size < partial_bytes)
        {
            self.partials = Some(gpu.device.create_buffer(partial_bytes, BufferUse::Gpu));
        }
        let partials = self.partials.as_ref().expect("partials prepared");
        let params = StatsParams {
            point_count,
            node_count,
            point_groups,
            node_groups,
            field_min_x: field_min[0],
            field_min_y: field_min[1],
            field_min_z: field_min[2],
            tick_index,
            gravity_x: gravity[0],
            gravity_y: gravity[1],
            gravity_z: gravity[2],
            _pad0: 0,
        };
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&params),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: points,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: grid,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: accum,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: partials,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: stats,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: tiles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 7,
                buffer: materials,
                offset: 0,
            },
        ];
        if point_groups > 0 {
            gpu.native_enc.dispatch_compute(
                &pipelines.points,
                &bindings,
                [point_groups, 1, 1],
                "node.matter_stats.points",
            );
        }
        if node_groups > 0 {
            gpu.native_enc.dispatch_compute(
                &pipelines.grid,
                &bindings,
                [node_groups, 1, 1],
                "node.matter_stats.grid",
            );
        }
        gpu.native_enc.dispatch_compute(
            &pipelines.finish,
            &bindings,
            [1, 1, 1],
            "node.matter_stats.finish",
        );
        if wants_view && point_count > 0 {
            let bytes = u64::from(point_count) * std::mem::size_of::<FluidParticle>() as u64;
            let mut previous_count = self.view_count;
            if self.view.as_ref().is_none_or(|view| view.size < bytes) {
                self.view = Some(gpu.device.create_buffer(bytes, BufferUse::Gpu));
                previous_count = 0;
            }
            let view = self.view.as_ref().expect("view prepared");
            let params = FrameParams {
                count: point_count,
                previous_count,
                radius_scale: crate::matter::RADIUS_SCALE,
                first: 0,
            };
            // The view is its own previous frame: a non-finite tick keeps it.
            gpu.native_enc.dispatch_compute(
                &pipelines.view,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&params),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: points,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: stats,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: view,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 4,
                        buffer: view,
                        offset: 0,
                    },
                ],
                [point_count.div_ceil(256), 1, 1],
                "node.matter_stats.view",
            );
            self.view_count = point_count;
        }
        ctx.outputs
            .set_scalar("view_count", ParamValue::Float(self.view_count as f32));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_stats_params_match_the_shader_struct() {
        assert_eq!(std::mem::size_of::<StatsParams>(), 48);
        assert!(SHADER.contains("struct StatsParams"));
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
