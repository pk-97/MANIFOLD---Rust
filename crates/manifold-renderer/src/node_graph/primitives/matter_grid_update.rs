//! `node.matter_grid_update` — resolve the matter grid: momentum to velocity,
//! gravity, the scene's forces and impulses (seam P8), closed walls,
//! colliders and the CFL clamp (`docs/GPU_MPM_SOLVER_DESIGN.md` section 4.1
//! step 4).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};
use crate::node_graph::liquid::fields::LIQUID_FIELD;
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::matter::{MatterGridNode, momentum_unit_fits};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::matter_common::MATTER_WALLS;
use super::standalone_pipeline::standalone_pipeline;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GridUpdateUniforms {
    nodes_x: i32,
    nodes_y: i32,
    nodes_z: i32,
    cell_size: f32,
    step_dt: f32,
    gravity_x: f32,
    gravity: f32,
    gravity_z: f32,
    closed_faces: i32,
    momentum_unit: f32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    body_count: i32,
    tick_index: i32,
    substep_in_tick: i32,
    field_nodes_x: i32,
    field_nodes_y: i32,
    field_nodes_z: i32,
    field_spacing: f32,
    force_lattices: i32,
    impulse_tick: i32,
    first_tick: i32,
    dispatch_count: u32,
}

crate::primitive! {
    name: MatterGridUpdate,
    type_id: "node.matter_grid_update",
    purpose: "Resolve the matter grid: turn each node's accumulated momentum into velocity, add gravity and the scene's acceleration field (read from a coarse force lattice), add the scene's impulses once on the first substep of their tick (from a coarse impulse lattice), stop velocity into closed domain walls on the outer three nodes, project velocity out of colliders (a node inside a body moving into it keeps the body's normal velocity and friction-limited sliding), and clamp each component to 0.9 cells per substep. Keeps the pre-force velocity for the Liveliness blend.",
    inputs: {
        accum: Array(i32) required,
        grid: Array(MatterGridNode) required,
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        forces: Array(f32) optional,
        impulses: Array(f32) optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        closed_faces: ScalarF32 optional,
        momentum_unit: ScalarF32 optional,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        body_count: ScalarF32 optional,
        tick_index: ScalarF32 optional,
        substep_in_tick: ScalarF32 optional,
        field_nodes_x: ScalarF32 optional, field_nodes_y: ScalarF32 optional, field_nodes_z: ScalarF32 optional,
        field_spacing: ScalarF32 optional,
        force_lattices: ScalarF32 optional,
        impulse_tick: ScalarF32 optional,
        first_tick: ScalarF32 optional,
    },
    outputs: {
        grid_out: Array(MatterGridNode),
    },
    params: [
        ParamDef { name: Cow::Borrowed("nodes_x"), label: "Nodes X", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_y"), label: "Nodes Y", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_z"), label: "Nodes Z", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("step_dt"), label: "Substep (s)", ty: ParamType::Float, default: ParamValue::Float(4.9e-4), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_faces"), label: "Closed Faces (bits −X +X −Y +Y −Z +Z)", ty: ParamType::Int, default: ParamValue::Float(63.0), range: Some((0.0, 63.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("momentum_unit"), label: "Momentum Unit (m/s)", ty: ParamType::Float, default: ParamValue::Float(128.0), range: Some((1.0e-3, 1.0e9)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_x"), label: "Lattice Min X", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_y"), label: "Lattice Min Y", ty: ParamType::Float, default: ParamValue::Float(-0.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_z"), label: "Lattice Min Z", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("body_count"), label: "Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_index"), label: "Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substep_in_tick"), label: "Substep in Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_nodes_x"), label: "Field Nodes X", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_nodes_y"), label: "Field Nodes Y", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_nodes_z"), label: "Field Nodes Z", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_spacing"), label: "Field Spacing", ty: ParamType::Float, default: ParamValue::Float(0.25), range: Some((1.0e-4, 400.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("force_lattices"), label: "Force Lattices", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("impulse_tick"), label: "Impulse Tick", ty: ParamType::Int, default: ParamValue::Float(-1.0), range: Some((-1.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("first_tick"), label: "First Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Region body of the Live Matter group, between node.matter_to_grid and node.grid_to_matter. grid/grid_out alias the node.matter_state grid array (one MatterGridNode per lattice node, x fastest); accum is read as a gather (4 words per node). Gravity, closed faces, the lattice minimum, body_count, shapes, atlas and momentum_unit (the same value node.matter_to_grid reads) come from node.matter_domain, bodies from node.matter_move_bodies, step_dt, tick_index and substep_in_tick from the substep boundary. forces, impulses and the field scalars (field_nodes_x/y/z, field_spacing, force_lattices, first_tick, impulse_tick) come from node.matter_domain; the field lattices start at the lattice minimum. Every substep adds the force lattice of its tick: one lattice per tick from first_tick, or one for all ticks when force_lattices is 1 (0: no forces); impulses apply once, on substep 0 of tick impulse_tick (−1: none). With bodies unwired no collider is read; with forces or impulses unwired neither is read.",
    examples: ["WaterDamBreakMatter", "WaterStillPoolMatter"],
    picker: { label: "Matter Grid Update", category: Atom },
    summary: "Turns the grid's gathered liquid momentum into velocities, adds gravity and stops the liquid at the walls.",
    category: Particles3D,
    role: Filter,
    aliases: ["grid update", "mpm grid", "resolve grid"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/matter_grid_update_body.wgsl"),
    input_access: [BufferGather, Coincident, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    wgsl_includes: [LIQUID_POSE, LIQUID_COLLIDER, MATTER_WALLS, LIQUID_FIELD],
}

/// The field scalars and lattices an atom binds: wired lattices too small
/// for the wired field are refused by name; unwired ones read nothing.
pub(crate) struct FieldBinding<'a> {
    pub nodes: [i32; 3],
    pub spacing: f32,
    pub force_lattices: i32,
    pub impulse_tick: i32,
    pub first_tick: i32,
    pub forces: Option<&'a manifold_gpu::GpuBuffer>,
    pub impulses: Option<&'a manifold_gpu::GpuBuffer>,
}

impl<'a> FieldBinding<'a> {
    pub(crate) fn read(
        ctx: &EffectNodeContext<'_, '_>,
        forces: Option<&'a manifold_gpu::GpuBuffer>,
        impulses: Option<&'a manifold_gpu::GpuBuffer>,
        atom: &str,
    ) -> Result<Self, String> {
        let nodes = ["field_nodes_x", "field_nodes_y", "field_nodes_z"]
            .map(|name| ctx.scalar_or_param(name, 2.0).round().max(2.0) as i32);
        let spacing = ctx.scalar_or_param("field_spacing", 0.25);
        let force_lattices =
            if forces.is_some() { ctx.scalar_or_param("force_lattices", 0.0).round().max(0.0) as i32 } else { 0 };
        let impulse_tick = if impulses.is_some() { ctx.scalar_or_param("impulse_tick", -1.0).round().max(-1.0) as i32 } else { -1 };
        let first_tick = ctx.scalar_or_param("first_tick", 0.0).round().max(0.0) as i32;
        let lattice_bytes = nodes.iter().map(|&n| n as u64).product::<u64>() * 16;
        for (name, buffer, lattices) in
            [("forces", forces, force_lattices.max(0) as u64), ("impulses", impulses, u64::from(impulse_tick >= 0))]
        {
            if lattices > 0 && buffer.is_some_and(|buffer| buffer.size < lattices * lattice_bytes) {
                return Err(format!(
                    "{atom}: the {name} buffer holds fewer than {lattices} lattice(s) of {} × {} × {} field nodes; wire node.matter_domain's {name} and field scalars",
                    nodes[0], nodes[1], nodes[2]
                ));
            }
        }
        if (force_lattices > 0 || impulse_tick >= 0) && !(spacing.is_finite() && spacing > 0.0) {
            return Err(format!("{atom}: field_spacing must be positive"));
        }
        Ok(Self { nodes, spacing, force_lattices, impulse_tick, first_tick, forces, impulses })
    }
}

impl Primitive for MatterGridUpdate {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "grid_out")
            .then(|| input_capacities.iter().find(|(p, _)| *p == "grid").map(|&(_, n)| n))
            .flatten()
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("grid", "grid_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let lattice = LiquidLattice::from_wires(ctx);
        let step_dt = ctx.scalar_or_param("step_dt", 4.9e-4);
        let gravity = [
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
        ];
        let closed_faces = ctx.scalar_or_param("closed_faces", 63.0).round().clamp(0.0, 63.0) as i32;
        let momentum_unit = ctx.scalar_or_param("momentum_unit", 128.0);
        let body_count = ctx.scalar_or_param("body_count", 0.0).round().clamp(0.0, MAX_FLUID_ROLES as f32) as i32;
        // In place on the grid input; the GPU is touched on every path.
        let accum = ctx.inputs.array("accum");
        let grid = ctx.inputs.array("grid");
        let colliders = (ctx.inputs.array("bodies"), ctx.inputs.array("shapes"), ctx.inputs.array("atlas"));
        let tick_index = ctx.scalar_or_param("tick_index", 0.0).round().max(0.0) as i32;
        let substep_in_tick = ctx.scalar_or_param("substep_in_tick", 0.0).round().max(0.0) as i32;
        let field = FieldBinding::read(ctx, ctx.inputs.array("forces"), ctx.inputs.array("impulses"), "Matter Grid Update");
        let gpu = ctx.gpu_encoder();
        let (Some(accum), Some(grid)) = (accum, grid) else {
            return;
        };
        let field = match field {
            Ok(field) => field,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let nodes = lattice
            .node_count()
            .min((grid.size / std::mem::size_of::<MatterGridNode>() as u64) as u32)
            .min((accum.size / 16) as u32);
        if nodes == 0 || step_dt <= 0.0 {
            return;
        }
        if !momentum_unit_fits(momentum_unit, lattice.cell_size(), step_dt) {
            ctx.error(format!(
                "Matter Grid Update: momentum unit {momentum_unit} is not a power of two at or above cell size / step_dt; wire node.matter_domain's momentum_unit"
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
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = GridUpdateUniforms {
            nodes_x: lattice.nodes()[0] as i32,
            nodes_y: lattice.nodes()[1] as i32,
            nodes_z: lattice.nodes()[2] as i32,
            cell_size: lattice.cell_size(),
            step_dt,
            gravity_x: gravity[0],
            gravity: gravity[1],
            gravity_z: gravity[2],
            closed_faces,
            momentum_unit,
            lattice_min_x: lattice.min()[0],
            lattice_min_y: lattice.min()[1],
            lattice_min_z: lattice.min()[2],
            body_count,
            tick_index,
            substep_in_tick,
            field_nodes_x: field.nodes[0],
            field_nodes_y: field.nodes[1],
            field_nodes_z: field.nodes[2],
            field_spacing: field.spacing,
            force_lattices: field.force_lattices,
            impulse_tick: field.impulse_tick,
            first_tick: field.first_tick,
            dispatch_count: nodes,
        };
        // An unwired lattice is never read (force_lattices 0, impulse_tick −1).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: accum, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: grid, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: bodies, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: shapes, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: atlas, offset: 0 },
                GpuBinding::Buffer { binding: 6, buffer: field.forces.unwrap_or(grid), offset: 0 },
                GpuBinding::Buffer { binding: 7, buffer: field.impulses.unwrap_or(grid), offset: 0 },
                GpuBinding::Buffer { binding: 8, buffer: grid, offset: 0 },
            ],
            [nodes.div_ceil(256), 1, 1],
            "node.matter_grid_update",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_grid_update_generates_a_gathering_node_kernel() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<MatterGridUpdate>()
            .expect("matter_grid_update codegen");
        let module = naga::front::wgsl::parse_str(&wgsl).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        assert!(wgsl.contains("var<storage, read> buf_accum: array<i32>"), "{wgsl}");
        assert!(wgsl.contains("buf_grid_out[idx] = body(idx, params.dispatch_count, e_grid,"), "{wgsl}");
        assert_eq!(std::mem::size_of::<GridUpdateUniforms>(), 96);
        assert!(wgsl.contains("first_tick: i32,\n    dispatch_count: u32,\n}"), "{wgsl}");
        for binding in ["buf_bodies", "buf_shapes", "buf_atlas: array<u32>", "buf_forces: array<f32>", "buf_impulses: array<f32>"] {
            assert!(wgsl.contains(binding), "{binding}: {wgsl}");
        }
        let body = include_str!("shaders/matter_grid_update_body.wgsl");
        assert_eq!(crate::node_graph::matter::MASS_SCALE, 65_536.0);
        assert_eq!(crate::node_graph::matter::MOMENTUM_SCALE, 134_217_728.0);
        assert!(body.contains("/ m_norm * (momentum_unit * (65536.0 / 134217728.0));"));
        assert!(body.contains("m_norm / 65536.0 * mass_unit") && body.contains("0.9 * vel_unit"));
        assert_eq!(crate::node_graph::matter::VELOCITY_CLAMP_CFL, 0.9);
    }
}
