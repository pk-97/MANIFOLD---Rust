//! `node.matter_to_grid` — the MLS-MPM particle-to-grid transfer
//! (`docs/GPU_MPM_SOLVER_DESIGN.md` section 4.1 step 3, D5, D6). Hand WGSL under
//! exclusion 1 (workgroup memory and barriers): with `order` and `ranges` from
//! the tick's cell sort, one workgroup per 4³ block of stencil base nodes
//! accumulates into a workgroup tile; unwired, one thread per point adds with
//! global atomics. Both add the same integers.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuComputePipeline};

use manifold_node_engine::primitives::standalone_pipeline::active_elements;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use crate::fluid_particles::CellRange;
use crate::liquid::lattice::LiquidLattice;
use crate::matter::{MatterPoint, grid_accum_bytes, lattice_nodes, momentum_unit_fits};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

const SHADER: &str = include_str!("shaders/matter_to_grid.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct P2gParams {
    pub(crate) lattice_min_x: f32,
    pub(crate) lattice_min_y: f32,
    pub(crate) lattice_min_z: f32,
    pub(crate) cell_size: f32,
    pub(crate) nodes_x: i32,
    pub(crate) nodes_y: i32,
    pub(crate) nodes_z: i32,
    pub(crate) active_count: u32,
    pub(crate) step_dt: f32,
    pub(crate) lambda: f32,
    pub(crate) cohesion: f32,
    pub(crate) density: f32,
    pub(crate) tick_index: u32,
    pub(crate) substep_in_tick: u32,
    pub(crate) blocks_x: u32,
    pub(crate) blocks_y: u32,
    pub(crate) blocks_z: u32,
    pub(crate) sorted: u32,
    pub(crate) momentum_unit: f32,
    pub(crate) _pad1: u32,
}

manifold_node_engine::primitive! {
    name: MatterToGrid,
    type_id: "node.matter_to_grid",
    purpose: "Transfer material points to the matter grid (MLS-MPM particle-to-grid). Each live point adds mass and momentum, including its water pressure as the affine stress term, to the 27 nodes of its quadratic B-spline stencil, as signed fixed point through integer atomics. With a cell sort's order and ranges it accumulates each 4³ block of nodes in fast workgroup memory first.",
    inputs: {
        points: Array(MatterPoint) required,
        accum: Array(i32) required,
        order: Array(u32) optional,
        ranges: Array(CellRange) optional,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        lambda: ScalarF32 optional,
        cohesion: ScalarF32 optional,
        density: ScalarF32 optional,
        active_count: ScalarF32 optional,
        tick_index: ScalarF32 optional,
        substep_in_tick: ScalarF32 optional,
        momentum_unit: ScalarF32 optional,
        blocks_x: ScalarF32 optional, blocks_y: ScalarF32 optional, blocks_z: ScalarF32 optional,
    },
    outputs: {
        accum_out: Array(i32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("lattice_min_x"), label: "Lattice Min X", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_y"), label: "Lattice Min Y", ty: ParamType::Float, default: ParamValue::Float(-0.1875), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_z"), label: "Lattice Min Z", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_x"), label: "Nodes X", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_y"), label: "Nodes Y", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_z"), label: "Nodes Z", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("step_dt"), label: "Substep (s)", ty: ParamType::Float, default: ParamValue::Float(4.9e-4), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lambda"), label: "Bulk Stiffness (Pa)", ty: ParamType::Float, default: ParamValue::Float(1.111e6), range: Some((0.0, 1.0e9)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cohesion"), label: "Cohesion", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("density"), label: "Density (kg/m³)", ty: ParamType::Float, default: ParamValue::Float(1000.0), range: Some((1.0, 1.0e5)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("active_count"), label: "Active Count", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_000_000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_index"), label: "Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substep_in_tick"), label: "Substep in Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("momentum_unit"), label: "Momentum Unit (m/s)", ty: ParamType::Float, default: ParamValue::Float(128.0), range: Some((1.0e-3, 1.0e9)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("blocks_x"), label: "Blocks X", ty: ParamType::Int, default: ParamValue::Float(18.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("blocks_y"), label: "Blocks Y", ty: ParamType::Int, default: ParamValue::Float(18.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("blocks_z"), label: "Blocks Z", ty: ParamType::Int, default: ParamValue::Float(18.0), range: Some((1.0, 4096.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Region body of the Live Matter group, after node.zero_array clears the accumulator and before node.matter_grid_update resolves it. accum/accum_out alias one Array(i32) of 4 words per lattice node (momentum xyz, mass), provided by node.matter_state. Lattice, material, the block counts and momentum_unit come from node.matter_domain (node.matter_grid_update must read the same momentum_unit); step_dt, tick_index and substep_in_tick from the substep boundary. Wire order and ranges from node.sort_particles_into_cells over node.matter_state's out, sorted once per tick into node.matter_domain's block bins, for the block path. Points with id 0 are skipped.",
    examples: ["WaterDamBreakMatter", "WaterStillPoolMatter"],
    picker: { label: "Matter to Grid", category: Atom },
    summary: "Spreads each liquid particle's weight and motion onto the simulation grid around it.",
    category: Particles3D,
    role: Filter,
    aliases: ["p2g", "particle to grid", "mpm scatter", "matter scatter"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        kernel: Option<GpuComputePipeline> = None,
    },
}

impl Primitive for MatterToGrid {
    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.kernel.is_none() {
            self.kernel = Some(device.create_compute_pipeline(SHADER, "scatter_main", "node.matter_to_grid"));
        }
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "accum_out")
            .then(|| input_capacities.iter().find(|(p, _)| *p == "accum").map(|&(_, n)| n))
            .flatten()
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("accum", "accum_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(lattice) = LiquidLattice::from_wires(ctx, "Matter to Grid") else {
            return;
        };
        let count = |name: &str, default: f32| ctx.scalar_or_param(name, default).round().max(0.0) as u32;
        let step_dt = ctx.scalar_or_param("step_dt", 4.9e-4);
        let lambda = ctx.scalar_or_param("lambda", 1.111e6);
        let cohesion = ctx.scalar_or_param("cohesion", 0.0);
        let density = ctx.scalar_or_param("density", 1000.0);
        let requested = count("active_count", 0.0);
        let tick_index = count("tick_index", 0.0);
        let substep_in_tick = count("substep_in_tick", 0.0);
        let momentum_unit = ctx.scalar_or_param("momentum_unit", 128.0);
        let blocks = [count("blocks_x", 18.0), count("blocks_y", 18.0), count("blocks_z", 18.0)];
        // In place on the accumulator input; the GPU is touched on every path.
        let points = ctx.inputs.array("points");
        let accum = ctx.inputs.array("accum");
        let sorted = ctx.inputs.array("order").zip(ctx.inputs.array("ranges"));
        let gpu = ctx.gpu_encoder();
        let kernel = self.kernel.as_ref().expect("matter to grid kernel built by prepare_pipelines at install");
        let (Some(points), Some(accum)) = (points, accum) else {
            return;
        };
        let active = active_elements::<MatterPoint>(points.size, requested);
        if active == 0 || step_dt <= 0.0 {
            return;
        }
        if !momentum_unit_fits(momentum_unit, lattice.cell_size(), step_dt) {
            ctx.error(format!(
                "Matter to Grid: momentum unit {momentum_unit} is not a power of two at or above cell size / step_dt; wire node.matter_domain's momentum_unit"
            ));
            return;
        }
        if accum.size < grid_accum_bytes(lattice.nodes()) {
            ctx.error(format!(
                "Matter to Grid: the accumulator holds fewer than this lattice's {} nodes; wire accum from the node.matter_state fed by the same node.matter_domain",
                lattice_nodes(lattice.nodes())
            ));
            return;
        }
        let uniforms = P2gParams {
            lattice_min_x: lattice.min()[0],
            lattice_min_y: lattice.min()[1],
            lattice_min_z: lattice.min()[2],
            cell_size: lattice.cell_size(),
            nodes_x: lattice.nodes()[0] as i32,
            nodes_y: lattice.nodes()[1] as i32,
            nodes_z: lattice.nodes()[2] as i32,
            active_count: active,
            step_dt,
            lambda,
            cohesion,
            density,
            tick_index,
            substep_in_tick,
            blocks_x: blocks[0].max(1),
            blocks_y: blocks[1].max(1),
            blocks_z: blocks[2].max(1),
            sorted: u32::from(sorted.is_some()),
            momentum_unit,
            _pad1: 0,
        };
        let block_total = uniforms.blocks_x * uniforms.blocks_y * uniforms.blocks_z;
        // Unsorted, the kernel never reads order or ranges; the accumulator
        // keeps their slots bound.
        let (order, ranges, groups) = match sorted {
            Some((order, ranges)) => {
                if ranges.size < u64::from(block_total) * std::mem::size_of::<CellRange>() as u64 {
                    ctx.error(format!(
                        "Matter to Grid: the ranges cover fewer than the {block_total} blocks of this lattice; sort into node.matter_domain's block bins"
                    ));
                    return;
                }
                (order, ranges, block_total)
            }
            None => (accum, accum, active.div_ceil(256)),
        };
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: points, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: accum, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: order, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: ranges, offset: 0 },
        ];
        gpu.native_enc.dispatch_compute(kernel, &bindings, [groups, 1, 1], "node.matter_to_grid");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_to_grid_params_are_eighty_bytes() {
        assert_eq!(std::mem::size_of::<P2gParams>(), 80);
    }

    /// The kernel inlines D5's scales, mass unit and rounding hash; they must
    /// equal the shared definitions the f64 reference uses.
    #[test]
    fn matter_to_grid_body_pins_fixed_point_constants() {
        use crate::matter::{MASS_SCALE, MOMENTUM_SCALE, mass_unit, rounding_hash};
        assert_eq!(MASS_SCALE, 65_536.0);
        assert!(SHADER.contains("let to_mass = 65536.0 * inv_mass_unit;"));
        assert_eq!(MOMENTUM_SCALE, 134_217_728.0);
        // `matter_momentum_unit_round_trips` runs this arithmetic in f32.
        assert!(SHADER.contains("let to_momentum = 134217728.0 / params.momentum_unit * inv_mass_unit;"));
        assert_eq!(mass_unit(2.0), 125.0 * 8.0);
        assert!(SHADER.contains("let inv_mass_unit = 1.0 / (125.0 * cell_size * cell_size * cell_size);"));
        for line in [
            "x = x ^ (x >> 16u);",
            "x = x * 0x7feb352du;",
            "x = x ^ (x >> 15u);",
            "x = x * 0x846ca68bu;",
            "hash(pt.id ^ hash(params.tick_index * 4096u + params.substep_in_tick))",
            "let fraction = u32((x - whole) * 16777216.0);",
            "let carry = (fraction + (hash(key ^ slot) >> 8u)) >> 24u;",
        ] {
            assert!(SHADER.contains(line), "{line}");
        }
        // The GPU word-for-word comparison in tests/gpu_proofs/matter_transfer.rs
        // proves the two hashes agree; here, only that it mixes.
        assert_eq!(rounding_hash(0), 0);
        assert_ne!(rounding_hash(1), rounding_hash(2));
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
