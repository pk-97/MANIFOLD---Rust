//! `node.grid_to_matter` — the MLS-MPM grid-to-particle transfer and point
//! update (`docs/GPU_MPM_SOLVER_DESIGN.md` section 4.1 step 6, D3's
//! Liveliness blend), finished against the colliders (D29), with the
//! push-out's momentum added to each dynamic body's reaction words (D30).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::matter::{
    MatterGridNode, MatterPoint, REACTION_WORDS, grid_bytes, lattice_nodes, momentum_unit_fits,
};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::standalone_pipeline::{active_elements, standalone_pipeline};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ToMatterUniforms {
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    nodes_x: i32,
    nodes_y: i32,
    nodes_z: i32,
    step_dt: f32,
    liveliness: f32,
    cohesion: f32,
    active_count: i32,
    body_count: i32,
    density: f32,
    momentum_unit: f32,
    tick_index: i32,
    substep_in_tick: i32,
    substeps_per_tick: i32,
    dynamic_count: i32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: GridToMatter,
    type_id: "node.grid_to_matter",
    purpose: "Transfer the resolved matter grid back to its points (MLS-MPM grid-to-particle): each live point gathers velocity and its affine velocity field from its 27-node stencil, blends toward a FLIP update by Liveliness, moves with the gathered velocity and updates its volume ratio. A point that ends inside a collider steps out onto its surface and loses the velocity pointing into it; a dynamic body gains the momentum the point lost, added to its reaction words. A point leaving the lattice is removed.",
    inputs: {
        points: Array(MatterPoint) required,
        grid: Array(MatterGridNode) required,
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        reaction: Array(i32) optional,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        liveliness: ScalarF32 optional,
        cohesion: ScalarF32 optional,
        active_count: ScalarF32 optional,
        body_count: ScalarF32 optional,
        density: ScalarF32 optional,
        momentum_unit: ScalarF32 optional,
        tick_index: ScalarF32 optional,
        substep_in_tick: ScalarF32 optional,
        substeps_per_tick: ScalarF32 optional,
        dynamic_count: ScalarF32 optional,
    },
    outputs: {
        points_out: Array(MatterPoint),
        reaction_out: Array(i32),
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
        ParamDef { name: Cow::Borrowed("liveliness"), label: "Liveliness", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cohesion"), label: "Cohesion", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("active_count"), label: "Active Count", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_000_000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("body_count"), label: "Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("density"), label: "Density (kg/m³)", ty: ParamType::Float, default: ParamValue::Float(1000.0), range: Some((1.0, 1.0e5)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("momentum_unit"), label: "Momentum Unit (m/s)", ty: ParamType::Float, default: ParamValue::Float(128.0), range: Some((1.0e-3, 1.0e9)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_index"), label: "Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substep_in_tick"), label: "Substep in Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substeps_per_tick"), label: "Substeps per Tick", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("dynamic_count"), label: "Dynamic Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 64.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Region body of the Live Matter group, after node.matter_grid_update and node.matter_body_reaction. points/points_out alias the node.matter_state point buffer (updated in place); grid is read as a gather. Liveliness 0 is APIC (calm, dissipative); toward 1 it keeps more of each point's own velocity change (livelier splashes, more noise). Positions always advect with the gathered velocity. bodies comes from node.matter_move_bodies (the substep-end pose node.matter_grid_update also reads); shapes, atlas and body_count from node.matter_domain. With bodies unwired no collider is read. reaction/reaction_out alias the domain's reaction words after node.matter_body_reaction (wire its reaction_out here, and this reaction_out into node.matter_state's reaction_in); density, momentum_unit, substeps_per_tick and dynamic_count come from node.matter_domain, tick_index and substep_in_tick from node.matter_state. Without a reaction slot covering every body, or with dynamic_count 0, no reaction is written.",
    examples: ["WaterFloatingBoxMatter", "WaterDamBreakMatter", "WaterStillPoolMatter"],
    picker: { label: "Grid to Matter", category: Atom },
    summary: "Moves each liquid particle with the grid's velocities and updates how compressed it is.",
    category: Particles3D,
    role: Filter,
    aliases: ["g2p", "grid to particle", "mpm gather", "matter gather"],
    // An atomic output is always a cut (FREEZE_COMPILER_MAP.md section 4
    // (The cut rules — when fusion says no)); the reason matches the other
    // atomic scatter atoms until BUG-1ois (atomic scatter boundary reason)
    // settles it.
    fusion_kind: Boundary,
    boundary_reason: Blocked,
    wgsl_body: include_str!("shaders/grid_to_matter_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    wgsl_includes: [LIQUID_POSE, LIQUID_COLLIDER],
    atomic_outputs: ["reaction_out"],
}

impl Primitive for GridToMatter {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        let input = match port_name {
            "points_out" => "points",
            "reaction_out" => "reaction",
            _ => return None,
        };
        input_capacities.iter().find(|(p, _)| *p == input).map(|&(_, n)| n)
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("points", "points_out"), ("reaction", "reaction_out")]
    }

    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        Some("active_count")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(lattice) = LiquidLattice::from_wires(ctx, "Grid to Matter") else {
            return;
        };
        let step_dt = ctx.scalar_or_param("step_dt", 4.9e-4);
        let liveliness = ctx.scalar_or_param("liveliness", 0.0).clamp(0.0, 1.0);
        let cohesion = ctx.scalar_or_param("cohesion", 0.0).clamp(0.0, 1.0);
        let requested = ctx.scalar_or_param("active_count", 0.0).round().max(0.0) as u32;
        let body_count = ctx.scalar_or_param("body_count", 0.0).round().clamp(0.0, MAX_FLUID_ROLES as f32) as i32;
        let int = |ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32| ctx.scalar_or_param(name, default).round().max(0.0) as i32;
        let density = ctx.scalar_or_param("density", 1000.0);
        let momentum_unit = ctx.scalar_or_param("momentum_unit", 128.0);
        let tick_index = int(ctx, "tick_index", 0.0);
        let substep_in_tick = int(ctx, "substep_in_tick", 0.0);
        let substeps_per_tick = int(ctx, "substeps_per_tick", 1.0).max(1);
        let mut dynamic_count = int(ctx, "dynamic_count", 0.0);
        // In place: the point buffer is mutated whether or not `points_out`
        // is consumed, so the GPU is touched on every path.
        let points = ctx.inputs.array("points");
        let grid = ctx.inputs.array("grid");
        let colliders = (ctx.inputs.array("bodies"), ctx.inputs.array("shapes"), ctx.inputs.array("atlas"));
        let reaction = ctx.inputs.array("reaction");
        let unit_fits = momentum_unit_fits(momentum_unit, lattice.cell_size(), step_dt);
        if dynamic_count > 0 && reaction.is_some() && !unit_fits {
            ctx.error(format!(
                "Grid to Matter: momentum unit {momentum_unit} is not a power of two at or above cell size / step_dt; wire node.matter_domain's momentum_unit"
            ));
        }
        let gpu = ctx.gpu_encoder();
        let (Some(points), Some(grid)) = (points, grid) else {
            return;
        };
        let active = active_elements::<MatterPoint>(points.size, requested);
        if active == 0 || step_dt <= 0.0 {
            return;
        }
        if grid.size < grid_bytes(lattice.nodes()) {
            ctx.error(format!(
                "Grid to Matter: the grid holds fewer than this lattice's {} nodes; wire grid from the node.matter_state fed by the same node.matter_domain",
                lattice_nodes(lattice.nodes())
            ));
            return;
        }
        // Without all three collider arrays the body loop runs zero times over
        // the grid buffer bound in their slots.
        let (bodies, shapes, atlas, body_count) = match colliders {
            (Some(bodies), Some(shapes), Some(atlas)) => {
                let rows = (bodies.size / std::mem::size_of::<LiquidBody>() as u64) as i32;
                (bodies, shapes, atlas, body_count.min(rows))
            }
            _ => (grid, grid, grid, 0),
        };
        // Reaction words are written only with a slot covering every body and
        // a momentum unit the words can hold; the grid stands in otherwise.
        let covered = reaction.is_some_and(|r| r.size >= body_count as u64 * u64::from(REACTION_WORDS) * 4);
        if !covered || !unit_fits || body_count == 0 {
            dynamic_count = 0;
        }
        let reaction = reaction.filter(|_| dynamic_count > 0).unwrap_or(grid);
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = ToMatterUniforms {
            lattice_min_x: lattice.min()[0],
            lattice_min_y: lattice.min()[1],
            lattice_min_z: lattice.min()[2],
            cell_size: lattice.cell_size(),
            nodes_x: lattice.nodes()[0] as i32,
            nodes_y: lattice.nodes()[1] as i32,
            nodes_z: lattice.nodes()[2] as i32,
            step_dt,
            liveliness,
            cohesion,
            active_count: active as i32,
            body_count,
            density,
            momentum_unit,
            tick_index,
            substep_in_tick,
            substeps_per_tick,
            dynamic_count,
            dispatch_count: active,
            _pad0: 0,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: points, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: grid, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: bodies, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: shapes, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: atlas, offset: 0 },
                GpuBinding::Buffer { binding: 6, buffer: reaction, offset: 0 },
                GpuBinding::Buffer { binding: 7, buffer: points, offset: 0 },
                GpuBinding::Buffer { binding: 8, buffer: reaction, offset: 0 },
            ],
            [active.div_ceil(256), 1, 1],
            "node.grid_to_matter",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_to_matter_generates_a_gathering_point_kernel() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<GridToMatter>()
            .expect("grid_to_matter codegen");
        let module = naga::front::wgsl::parse_str(&wgsl).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        assert!(wgsl.contains("var<storage, read> buf_grid: array<Element2>"), "{wgsl}");
        assert!(wgsl.contains("buf_points_out[idx] = body(idx, params.dispatch_count, e_points,"), "{wgsl}");
        for binding in ["buf_bodies", "buf_shapes", "buf_atlas: array<u32>", "buf_reaction_out: array<atomic<i32>>"] {
            assert!(wgsl.contains(binding), "{binding}: {wgsl}");
        }
        assert!(!wgsl.contains("buf_reaction_out[idx]"), "the wrapper never writes the reaction words: {wgsl}");
        assert_eq!(std::mem::size_of::<ToMatterUniforms>(), 80);
        assert!(wgsl.contains("dynamic_count: i32,\n    dispatch_count: u32,\n    _pad0: u32,\n}"), "{wgsl}");
    }

    #[test]
    fn grid_to_matter_is_a_boundary_for_its_reaction_words() {
        use crate::node_graph::EffectNode;
        let node = GridToMatter::new();
        assert_eq!(node.fusion_kind(), crate::node_graph::freeze::classify::FusionKind::Boundary);
        assert_eq!(node.atomic_outputs(), &["reaction_out"]);
    }

    /// The body inlines D3's J bound; it must equal the shared constant the
    /// f64 reference uses.
    #[test]
    fn grid_to_matter_body_pins_the_j_bound() {
        let body = include_str!("shaders/grid_to_matter_body.wgsl");
        assert_eq!(crate::node_graph::matter::COHESIVE_J_MAX, 2.0);
        assert!(body.contains("select(2.0, 1.0, cohesion <= 0.0)"), "{body}");
    }
}
