//! `node.matter_to_grid` — the MLS-MPM particle-to-grid transfer
//! (`docs/GPU_MPM_SOLVER_DESIGN.md` section 4.1 step 3, D5, D6). One atomic
//! output; the P1 baseline adds with global integer atomics.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::matter::MatterPoint;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::matter_common::read_lattice;
use super::standalone_pipeline::{active_elements, standalone_pipeline};

/// Generated uniform: params in PARAMS order, then `dispatch_count`, padded.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ToGridUniforms {
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    nodes_x: i32,
    nodes_y: i32,
    nodes_z: i32,
    step_dt: f32,
    lambda: f32,
    cohesion: f32,
    density: f32,
    active_count: i32,
    tick_index: i32,
    substep_in_tick: i32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: MatterToGrid,
    type_id: "node.matter_to_grid",
    purpose: "Transfer material points to the matter grid (MLS-MPM particle-to-grid). Each live point adds mass and momentum, including its water pressure as the affine stress term, to the 27 nodes of its quadratic B-spline stencil, as signed fixed point through integer atomics.",
    inputs: {
        points: Array(MatterPoint) required,
        accum: Array(i32) required,
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
    ],
    depth_rule: Terminal,
    composition_notes: "Region body of the Live Matter group, after node.zero_array clears the accumulator and before node.matter_grid_update resolves it. accum/accum_out alias one Array(i32) of 4 words per lattice node (momentum xyz, mass), provided by node.matter_state. Lattice and material come from node.matter_domain; step_dt from the substep boundary. Points with id 0 are skipped.",
    examples: ["WaterDamBreakMatter", "WaterStillPoolMatter"],
    picker: { label: "Matter to Grid", category: Atom },
    summary: "Spreads each liquid particle's weight and motion onto the simulation grid around it.",
    category: Particles3D,
    role: Filter,
    aliases: ["p2g", "particle to grid", "mpm scatter", "matter scatter"],
    fusion_kind: Boundary,
    boundary_reason: Blocked,
    wgsl_body: include_str!("shaders/matter_to_grid_body.wgsl"),
    input_access: [Coincident, BufferGather],
    atomic_outputs: ["accum_out"],
}

impl Primitive for MatterToGrid {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
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
        let lattice = read_lattice(ctx);
        let step_dt = ctx.scalar_or_param("step_dt", 4.9e-4);
        let lambda = ctx.scalar_or_param("lambda", 1.111e6);
        let cohesion = ctx.scalar_or_param("cohesion", 0.0);
        let density = ctx.scalar_or_param("density", 1000.0);
        let requested = ctx.scalar_or_param("active_count", 0.0).round().max(0.0) as u32;
        let tick_index = ctx.scalar_or_param("tick_index", 0.0).round().max(0.0) as i32;
        let substep_in_tick = ctx.scalar_or_param("substep_in_tick", 0.0).round().max(0.0) as i32;
        // In place on the accumulator input; the GPU is touched on every path.
        let points = ctx.inputs.array("points");
        let accum = ctx.inputs.array("accum");
        let gpu = ctx.gpu_encoder();
        let (Some(points), Some(accum)) = (points, accum) else {
            return;
        };
        let active = active_elements::<MatterPoint>(points.size, requested);
        if active == 0 || step_dt <= 0.0 {
            return;
        }
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = ToGridUniforms {
            lattice_min_x: lattice.min[0],
            lattice_min_y: lattice.min[1],
            lattice_min_z: lattice.min[2],
            cell_size: lattice.cell_size,
            nodes_x: lattice.nodes[0] as i32,
            nodes_y: lattice.nodes[1] as i32,
            nodes_z: lattice.nodes[2] as i32,
            step_dt,
            lambda,
            cohesion,
            density,
            active_count: active as i32,
            tick_index,
            substep_in_tick,
            dispatch_count: active,
            _pad0: 0,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: points, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: accum, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: accum, offset: 0 },
            ],
            [active.div_ceil(256), 1, 1],
            "node.matter_to_grid",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn matter_to_grid_generates_an_atomic_scatter() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<MatterToGrid>()
            .expect("matter_to_grid codegen");
        assert!(wgsl.contains("var<storage, read_write> buf_accum_out: array<atomic<i32>>"), "{wgsl}");
        assert!(wgsl.contains("    body(idx, params.dispatch_count, e_points,"), "{wgsl}");
        assert_eq!(std::mem::size_of::<ToGridUniforms>(), 64);
        assert_eq!(MatterToGrid::PARAMS.len(), 14);
    }

    /// The body inlines D5's scales, mass unit and rounding hash; they must
    /// equal the shared definitions the f64 reference uses.
    #[test]
    fn matter_to_grid_body_pins_fixed_point_constants() {
        use crate::node_graph::matter::{MASS_SCALE, MOMENTUM_SCALE, mass_unit, rounding_hash};
        let body = include_str!("shaders/matter_to_grid_body.wgsl");
        assert_eq!(MASS_SCALE, 65_536.0);
        assert!(body.contains("let to_mass = 65536.0 / mass_unit;"));
        assert_eq!(MOMENTUM_SCALE, 134_217_728.0);
        assert!(body.contains("let to_momentum = 134217728.0 / mass_unit * step_dt * inv_dx;"));
        assert_eq!(mass_unit(2.0), 125.0 * 8.0);
        assert!(body.contains("125.0 * cell_size * cell_size * cell_size"));
        for line in [
            "x = x ^ (x >> 16u);",
            "x = x * 0x7feb352du;",
            "x = x ^ (x >> 15u);",
            "x = x * 0x846ca68bu;",
            "m2g_hash(e_points.id ^ m2g_hash(u32(tick_index) * 4096u + u32(substep_in_tick)))",
            "let fraction = u32((x - whole) * 16777216.0);",
            "let carry = (fraction + (m2g_hash(key ^ slot) >> 8u)) >> 24u;",
        ] {
            assert!(body.contains(line), "{line}");
        }
        // The GPU word-for-word comparison in tests/gpu_proofs/matter_transfer.rs
        // proves the two hashes agree; here, only that it mixes.
        assert_eq!(rounding_hash(0), 0);
        assert_ne!(rounding_hash(1), rounding_hash(2));
    }
}
