//! `node.matter_to_grid` — the MLS-MPM particle-to-grid transfer on world
//! tiles. Hand WGSL under exclusion 1 (workgroup memory and barriers). The
//! node owns the world tile table Matter's grid lives on and the sort of the
//! points into the table's 4³ node blocks, both rebuilt every
//! `REBUILD_SUBSTEPS` substeps; one workgroup per block accumulates into a
//! workgroup tile and flushes into the pooled accumulator. It clears the
//! accumulator over the held tiles first and hands the held-tile list and its
//! dispatch triple to the grid passes, so they pay only for tiles that hold
//! matter. While a material stepping the same tick reads the grid, the
//! tick's last substep also lays the points' bulk on it (`bulk`).

use manifold_gpu::BufferUse;
use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use crate::matter::{
    MATTER_REACH, MatterPoint, REBUILD_SUBSTEPS, grid_accum_bytes, momentum_unit_fits,
};
use manifold_core::liquid_domain::SHARED_TABLE;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::ports::ArrayType;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::active_elements;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, lattice_origin, pool_len};
use manifold_water_liquid::fluid_particles::{SAND_BULK, SAND_BULK_BYTES};
use manifold_water_liquid::primitives::prefix_scan::{ScanLabels, open_gate};
use manifold_water_liquid::primitives::sort_particles_into_cells::{
    ParticleSorter, SortCount, SortJob, SortLabels, block_keys, record_read,
};
use manifold_water_liquid::tile_pool::MAX_POOL_TILES;
use manifold_water_liquid::tile_pool::pool_tiles_wire;
use manifold_water_liquid::world_tiles::{
    Allocation, MATTER_POINTS, PointCount, SharedTable, WorldTiles, allocated_args_offset,
    shared_table_entries,
};

// The kernel after the table's read side it indexes its nodes through.
static SHADER: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{SAND_BULK}\n{}\n{}",
        super::matter_common::MATTER_MATERIAL,
        include_str!("shaders/matter_to_grid.wgsl")
    )
});

const SORT_LABELS: SortLabels = SortLabels {
    clear: "node.matter_to_grid.sort.clear",
    count: "node.matter_to_grid.sort.count",
    scan: ScanLabels {
        blocks: "node.matter_to_grid.sort.scan.blocks",
        add: "node.matter_to_grid.sort.scan.add",
    },
    ranges: "node.matter_to_grid.sort.ranges",
    tail: "node.matter_to_grid.sort.tail",
    scatter: "node.matter_to_grid.sort.scatter",
    stabilise: "node.matter_to_grid.sort.stabilise",
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct P2gParams {
    pub(crate) cell_size: f32,
    pub(crate) active_count: u32,
    pub(crate) step_dt: f32,
    pub(crate) tick_index: u32,
    pub(crate) substep_in_tick: u32,
    pub(crate) momentum_unit: f32,
    pub(crate) pool_tiles: u32,
    /// 1 when this substep lays the bulk.
    pub(crate) bulk: u32,
    /// The word of the table's held-slot triple in its args.
    pub(crate) held_word: u32,
    pub(crate) _pad: [u32; 3],
}

manifold_node_engine::primitive! {
    name: MatterToGrid,
    type_id: "node.matter_to_grid",
    purpose: "Transfer material points to the matter grid (MLS-MPM particle-to-grid). Each live point adds mass and momentum, including the Kirchhoff stress node.grid_to_matter left on it as the affine stress term, to the 27 nodes of its quadratic B-spline stencil, as signed fixed point through integer atomics. The node owns the world tile table the grid lives on and sorts the points into its 4³ node blocks, then accumulates each block in fast workgroup memory first.",
    inputs: {
        points: Array(MatterPoint) required,
        accum: Array(i32) required,
        // The material table (node.matter_material): a point's mass is its
        // rest volume times its row's density.
        materials: Array(f32) required,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        active_count: ScalarF32 optional,
        tick_index: ScalarF32 optional,
        substep_in_tick: ScalarF32 optional,
        // 1 on the tick's last substep, which lays the bulk.
        tick_end: ScalarF32 optional,
        momentum_unit: ScalarF32 optional,
        // The World's water table and pool, as the water's last tick left
        // them: this table shares its key set (`world_tiles::SharedTable`).
        shared_tiles: Array(u32) optional,
        shared_pool_tiles: ScalarF32 optional,
    },
    outputs: {
        accum_out: Array(i32),
        // The world tile table of the pooled grid (at least
        // `table_words(pool_tiles)` words: more when it shares its keys).
        tiles: Array(u32),
        // The table's held slots, and the args whose triple at
        // `held_args_offset()` dispatches 512 threads per held slot.
        held: Array(u32),
        held_args: Array(u32),
        // The slots this substep's table build handed out (their triple in
        // `held_args` at `fresh_args_offset`), and 1 when it built, else 0.
        fresh: Array(u32),
        built: ScalarF32,
        // Each node's bulk as the tick's last substep laid it
        // (sand_bulk.wgsl), for a material stepping the same tick; laid
        // only while read.
        bulk: Array(u32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pool_tiles"), label: "Pool Tiles", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((1.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("step_dt"), label: "Substep (s)", ty: ParamType::Float, default: ParamValue::Float(4.9e-4), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("active_count"), label: "Active Count", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_000_000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_index"), label: "Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substep_in_tick"), label: "Substep in Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_end"), label: "Tick End", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("momentum_unit"), label: "Momentum Unit (m/s)", ty: ParamType::Float, default: ParamValue::Float(128.0), range: Some((1.0e-3, 1.0e9)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Region body of the Live Matter group, before node.matter_static_solid and node.matter_grid_update. It clears the accumulator over the held tiles itself, then scatters into it. accum/accum_out alias one Array(i32) of 4 words per pooled node (momentum xyz, mass), provided by node.matter_state for the same pool_tiles. Node p sits at p·cell_size. The tiles output is the world tile table every pooled matter atom reads; it and the point sort are rebuilt on substeps that are a multiple of the rebuild interval. cell_size, pool_tiles and momentum_unit come from node.matter_domain (node.matter_grid_update must read the same momentum_unit); step_dt, tick_index and substep_in_tick from the substep boundary. Points with id 0 are skipped. held/held_args are the held slots and their dispatch triple for the grid passes; fresh and built tell node.matter_static_solid which slots this substep's build handed out. tick_end from the boundary: on the tick's last substep, while bulk is wired, each point also adds its rest volume and its grains' share of it to its nodes (two words a pooled node), which a Water's or a Smoke's step reads with node.matter_grid_update's grid through tiles.",
    examples: ["SandColumn"],
    picker: { label: "Matter to Grid", category: Atom },
    summary: "Spreads each liquid particle's weight and motion onto the simulation grid around it.",
    category: Particles3D,
    role: Filter,
    aliases: ["p2g", "particle to grid", "mpm scatter", "matter scatter"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        kernel: Option<GpuComputePipeline> = None,
        clear: Option<GpuComputePipeline> = None,
        // Writes the scatter's triple, a workgroup per block of every held
        // slot, into `scatter_args`.
        args: Option<GpuComputePipeline> = None,
        scatter_args: Option<GpuBuffer> = None,
        tiles: Option<WorldTiles> = None,
        sorter: ParticleSorter = ParticleSorter::default(),
        order: Option<GpuBuffer> = None,
        gate: Option<GpuBuffer> = None,
        bulk: Option<GpuBuffer> = None,
    },
}

impl Primitive for MatterToGrid {
    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.kernel.is_none() {
            self.kernel = Some(device.create_compute_pipeline(
                SHADER.as_str(),
                "scatter_main",
                "node.matter_to_grid",
            ));
        }
        if self.clear.is_none() {
            self.clear = Some(device.create_compute_pipeline(
                SHADER.as_str(),
                "clear_main",
                "node.matter_to_grid.clear",
            ));
        }
        if self.args.is_none() {
            self.args = Some(device.create_compute_pipeline(
                SHADER.as_str(),
                "args_main",
                "node.matter_to_grid.args",
            ));
        }
        if self.scatter_args.is_none() {
            self.scatter_args = Some(device.create_buffer(12, BufferUse::Gpu));
        }
        self.sorter.prepare_pooled_blocks(device);
        if self.gate.is_none() {
            self.gate = Some(open_gate(device));
        }
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "tiles" | "held" | "held_args" | "fresh" | "bulk")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        if port == "bulk" {
            return self.bulk.as_ref();
        }
        let tiles = self.tiles.as_ref()?;
        match port {
            "tiles" => Some(tiles.table()),
            "held" => Some(&tiles.lists().by_rank),
            "held_args" => Some(tiles.args()),
            "fresh" => Some(&tiles.lists().fresh),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        match port_name {
            "accum_out" => input_capacities
                .iter()
                .find(|(p, _)| *p == "accum")
                .map(|&(_, n)| n),
            // Provided storage, sized to the tile pool at run time.
            "tiles" | "held" | "held_args" | "fresh" | "bulk" => Some(1),
            _ => None,
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("accum", "accum_out")]
    }

    // The water steps after Matter in the World's tick: its table is the one
    // its last tick left.
    fn state_capture_input_ports(&self) -> &[&str] {
        &SHARED_TABLE
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match pool_tiles_wire(|name, default| ctx.scalar_or_param(name, default)) {
            Ok(pool) => pool,
            Err(reason) => {
                ctx.error(format!(
                    "Matter to Grid: {reason}; wire node.matter_domain's pool_tiles"
                ));
                return;
            }
        };
        let count =
            |name: &str, default: f32| ctx.scalar_or_param(name, default).round().max(0.0) as u32;
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let step_dt = ctx.scalar_or_param("step_dt", 4.9e-4);
        let requested = count("active_count", 0.0);
        let tick_index = count("tick_index", 0.0);
        let substep_in_tick = count("substep_in_tick", 0.0);
        let tick_end = ctx.scalar_or_param("tick_end", 1.0) > 0.5;
        let wants_bulk = ctx.outputs.slot("bulk").is_some();
        let momentum_unit = ctx.scalar_or_param("momentum_unit", 128.0);
        // In place on the accumulator input; the GPU is touched on every path.
        let points = ctx.inputs.array("points");
        let accum = ctx.inputs.array("accum");
        let materials = ctx.inputs.array("materials");
        // The water's table, sharing this one's keys: one H sized for both
        // pools. A new H starts an empty table, as a new pool does.
        let shared_tiles = ctx.inputs.array("shared_tiles");
        let shared_pool = shared_tiles.map_or(0, |_| {
            ctx.scalar_or_param("shared_pool_tiles", 0.0)
                .round()
                .clamp(0.0, MAX_POOL_TILES as f32) as u32
        });
        let entries = shared_table_entries(pool, shared_pool);
        let fresh = self
            .tiles
            .as_ref()
            .is_none_or(|t| t.pool_tiles() != pool || t.table_entries() != entries);
        let active = points.map_or(0, |p| active_elements::<MatterPoint>(p.size, requested));
        // A table that has never been built (no ranges yet) builds on whatever
        // substep the points first appear.
        let build = active > 0
            && step_dt > 0.0
            && (fresh
                || self.sorter.ranges().is_none()
                || substep_in_tick.is_multiple_of(REBUILD_SUBSTEPS));
        ctx.outputs
            .set_scalar("built", ParamValue::Float(f32::from(u8::from(build))));
        let gpu = ctx.gpu_encoder();
        let kernel = self
            .kernel
            .as_ref()
            .expect("matter to grid kernel built by prepare_pipelines at install");
        let (Some(points), Some(accum), Some(materials)) = (points, accum, materials) else {
            return;
        };
        if !(cell_size.is_finite() && cell_size > 0.0) {
            ctx.error(format!(
                "Matter to Grid: cell size {cell_size} must be positive"
            ));
            return;
        }
        if accum.size < grid_accum_bytes(pool) {
            ctx.error(format!(
                "Matter to Grid: the accumulator holds fewer than a pool of {pool} tiles' nodes; wire accum from the node.matter_state fed by the same node.matter_domain"
            ));
            return;
        }
        if fresh {
            self.tiles = None;
            match WorldTiles::with_entries(gpu.device, pool, entries, MATTER_REACH, MATTER_POINTS) {
                Ok(mut tiles) => {
                    tiles.prepare(gpu.device);
                    self.tiles = Some(tiles);
                }
                Err(error) => {
                    ctx.error(format!(
                        "Matter to Grid: a pool of {pool} tiles needs storage the device cannot give: {error}. Raise Cell Size."
                    ));
                    return;
                }
            }
        }
        let bulk_bytes = pool_len(pool) * SAND_BULK_BYTES;
        if !wants_bulk {
            self.bulk = None;
        } else if self.bulk.as_ref().is_none_or(|b| b.size != bulk_bytes) {
            self.bulk = None;
            match gpu.device.try_create_buffer(bulk_bytes, BufferUse::Gpu) {
                Ok(bulk) => {
                    // Empty until the tick's last substep lays it.
                    gpu.native_enc.clear_buffer(&bulk);
                    self.bulk = Some(bulk);
                }
                Err(error) => {
                    ctx.error(format!(
                        "Matter to Grid: the bulk of a pool of {pool} tiles needs storage the device cannot give: {error}. Raise Cell Size."
                    ));
                    return;
                }
            }
        }
        let bulk = self.bulk.as_ref().filter(|_| tick_end);
        let clear = self
            .clear
            .as_ref()
            .expect("clear kernel built by prepare_pipelines");
        if active == 0 || step_dt <= 0.0 {
            // The grid passes still run over the last build's held tiles.
            let tiles = self.tiles.as_ref().expect("tiles built above");
            encode_clear(gpu.native_enc, clear, tiles, accum, bulk, pool);
            return;
        }
        if !momentum_unit_fits(momentum_unit, cell_size, step_dt) {
            ctx.error(format!(
                "Matter to Grid: momentum unit {momentum_unit} is not a power of two at or above cell size / step_dt; wire node.matter_domain's momentum_unit"
            ));
            return;
        }
        let tiles = self.tiles.as_ref().expect("tiles built above");
        let point_slots = points.size / std::mem::size_of::<MatterPoint>() as u64;
        let order_bytes = point_slots.max(1) * 4;
        if self.order.as_ref().is_none_or(|o| o.size < order_bytes) {
            self.order = None;
            match gpu.device.try_create_buffer(order_bytes, BufferUse::Gpu) {
                Ok(order) => self.order = Some(order),
                Err(error) => {
                    ctx.error(format!(
                        "Matter to Grid: the sort order for {point_slots} points needs storage the device cannot give: {error}"
                    ));
                    return;
                }
            }
        }
        let order = self.order.as_ref().expect("order allocated above");
        let gate = self.gate.as_ref().expect("gate built by prepare_pipelines");
        let inv_cell = 1.0 / cell_size;
        if build {
            tiles.encode_allocation(
                gpu.native_enc,
                &Allocation {
                    points,
                    count: PointCount::Slots(active),
                    origin: lattice_origin(cell_size),
                    inv_cell,
                    replay_counts: [0; 2],
                    gate,
                    clip: None,
                    kept: None,
                    shared: shared_tiles
                        .filter(|_| shared_pool > 0)
                        .map(|table| SharedTable {
                            table,
                            pool_tiles: shared_pool,
                        }),
                },
            );
            if let Err(error) = self
                .sorter
                .reserve_keys(gpu.device, block_keys(pool))
                .and_then(|()| {
                    self.sorter.encode_pooled(
                        gpu.device,
                        gpu.native_enc,
                        &SortJob {
                            particles: points,
                            read: record_read(&ArrayType::of_known::<MatterPoint>())
                                .expect("a matter point carries a position and an id"),
                            capacity: point_slots.min(u64::from(u32::MAX)) as u32,
                            count: SortCount::Slots(active),
                            bin_min: [0.5 * cell_size; 3],
                            inv_cell,
                            bins: [0; 3],
                            sorted: None,
                            order: Some(order),
                            companion: None,
                            gate: None,
                        },
                        tiles.table(),
                        pool,
                        &SORT_LABELS,
                    )
                })
            {
                ctx.error(format!("Matter to Grid: the point sort failed: {error}"));
                return;
            }
        }
        encode_clear(gpu.native_enc, clear, tiles, accum, bulk, pool);
        let ranges = self
            .sorter
            .ranges()
            .expect("ranges reserved by the first rebuild, which precedes every scatter");
        let uniforms = P2gParams {
            cell_size,
            active_count: active,
            step_dt,
            tick_index,
            substep_in_tick,
            momentum_unit,
            pool_tiles: pool,
            bulk: u32::from(bulk.is_some()),
            held_word: (held_args_offset() / 4) as u32,
            _pad: [0; 3],
        };
        let scatter_args = self
            .scatter_args
            .as_ref()
            .expect("scatter args allocated by prepare_pipelines");
        gpu.native_enc.dispatch_compute(
            self.args
                .as_ref()
                .expect("args kernel built by prepare_pipelines"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 9,
                    buffer: tiles.args(),
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 10,
                    buffer: scatter_args,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "node.matter_to_grid.args",
        );
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: points,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: accum,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: order,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: ranges,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: tiles.table(),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: &tiles.lists().by_rank,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 7,
                buffer: materials,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 8,
                buffer: bulk.unwrap_or(accum),
                offset: 0,
            },
        ];
        gpu.native_enc.dispatch_compute_indirect(
            kernel,
            &bindings,
            scatter_args,
            0,
            "node.matter_to_grid",
        );
    }
}

/// The byte offset in `held_args` of the triple over every held slot, 512
/// threads a slot: the grid passes dispatch from it.
pub(crate) fn held_args_offset() -> u64 {
    allocated_args_offset(MATTER_REACH.ring_max)
}

/// Zeroes the accumulator, and the bulk when this substep lays it, over the
/// held tiles and the canonical node.
fn encode_clear(
    enc: &mut manifold_gpu::GpuEncoder,
    clear: &GpuComputePipeline,
    tiles: &WorldTiles,
    accum: &GpuBuffer,
    bulk: Option<&GpuBuffer>,
    pool: u32,
) {
    let uniforms = P2gParams {
        cell_size: 0.0,
        active_count: 0,
        step_dt: 0.0,
        tick_index: 0,
        substep_in_tick: 0,
        momentum_unit: 0.0,
        pool_tiles: pool,
        bulk: u32::from(bulk.is_some()),
        held_word: 0,
        _pad: [0; 3],
    };
    enc.dispatch_compute_indirect(
        clear,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: accum,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: &tiles.lists().by_rank,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 8,
                buffer: bulk.unwrap_or(accum),
                offset: 0,
            },
        ],
        tiles.args(),
        held_args_offset(),
        "node.matter_to_grid.clear",
    );
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(test)]
mod tests {
    use super::*;

    /// The kernel composes with the tile read side and validates, and its
    /// node word key is the CPU's.
    #[test]
    fn matter_to_grid_kernel_validates() {
        let module = naga::front::wgsl::parse_str(&SHADER)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&SHADER)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&SHADER)));
        assert_eq!(std::mem::size_of::<P2gParams>(), 48);
    }
}
