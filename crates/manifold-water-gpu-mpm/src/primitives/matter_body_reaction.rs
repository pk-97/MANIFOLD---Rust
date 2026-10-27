//! `node.matter_body_reaction` — gather what the grid's collider projection
//! took from the liquid into each dynamic body's reaction words

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::matter_common::MATTER_BOUNDS;
use crate::matter::{MatterGridNode, REACTION_WORDS, grid_bytes, momentum_unit_fits};
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, pool_len, table_words};
use manifold_nodes_physics::force_lattice::{FORCE_LATTICE, ForceLatticeBinding};
use manifold_water_liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BodyReactionUniforms {
    cell_size: f32,
    step_dt: f32,
    gravity_x: f32,
    gravity: f32,
    gravity_z: f32,
    momentum_unit: f32,
    field_min_x: f32,
    field_min_y: f32,
    field_min_z: f32,
    body_count: i32,
    tick_index: i32,
    substep_in_tick: i32,
    substeps_per_tick: i32,
    dynamic_count: i32,
    field_nodes_x: i32,
    field_nodes_y: i32,
    field_nodes_z: i32,
    field_spacing: f32,
    force_lattices: i32,
    impulse_tick: i32,
    first_tick: i32,
    pool_tiles: i32,
    wall_faces: i32,
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
    bounds_hi_x: f32,
    bounds_hi_y: f32,
    bounds_hi_z: f32,
    wall_velocity_lo_x: f32,
    wall_velocity_lo_y: f32,
    wall_velocity_lo_z: f32,
    wall_velocity_hi_x: f32,
    wall_velocity_hi_y: f32,
    wall_velocity_hi_z: f32,
    dispatch_count: u32,
}

manifold_node_engine::primitive! {
    name: MatterBodyReaction,
    type_id: "node.matter_body_reaction",
    purpose: "Add the liquid's push on each dynamic body for this substep: repeat the grid's collider projection node by node and atomically add the momentum each body took from the node (and its turning moment about the body's centre of mass) to that body's reaction words, plus the same terms weighted by the substep's place in the tick.",
    inputs: {
        grid: Array(MatterGridNode) required,
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        reaction: Array(i32) optional,
        forces: Array(f32) optional,
        impulses: Array(f32) optional,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        momentum_unit: ScalarF32 optional,
        field_min_x: ScalarF32 optional, field_min_y: ScalarF32 optional, field_min_z: ScalarF32 optional,
        body_count: ScalarF32 optional,
        tick_index: ScalarF32 optional,
        substep_in_tick: ScalarF32 optional,
        substeps_per_tick: ScalarF32 optional,
        dynamic_count: ScalarF32 optional,
        field_nodes_x: ScalarF32 optional, field_nodes_y: ScalarF32 optional, field_nodes_z: ScalarF32 optional,
        field_spacing: ScalarF32 optional,
        force_lattices: ScalarF32 optional,
        impulse_tick: ScalarF32 optional,
        first_tick: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
        wall_faces: ScalarF32 optional,
        bounds_lo_x: ScalarF32 optional, bounds_lo_y: ScalarF32 optional, bounds_lo_z: ScalarF32 optional,
        bounds_hi_x: ScalarF32 optional, bounds_hi_y: ScalarF32 optional, bounds_hi_z: ScalarF32 optional,
        wall_velocity_lo_x: ScalarF32 optional, wall_velocity_lo_y: ScalarF32 optional, wall_velocity_lo_z: ScalarF32 optional,
        wall_velocity_hi_x: ScalarF32 optional, wall_velocity_hi_y: ScalarF32 optional, wall_velocity_hi_z: ScalarF32 optional,
    },
    outputs: {
        reaction_out: Array(i32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("step_dt"), label: "Substep (s)", ty: ParamType::Float, default: ParamValue::Float(4.9e-4), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("momentum_unit"), label: "Momentum Unit (m/s)", ty: ParamType::Float, default: ParamValue::Float(128.0), range: Some((1.0e-3, 1.0e9)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_min_x"), label: "Field Min X", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_min_y"), label: "Field Min Y", ty: ParamType::Float, default: ParamValue::Float(-0.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_min_z"), label: "Field Min Z", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("body_count"), label: "Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_index"), label: "Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substep_in_tick"), label: "Substep in Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substeps_per_tick"), label: "Substeps per Tick", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("dynamic_count"), label: "Dynamic Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_nodes_x"), label: "Field Nodes X", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_nodes_y"), label: "Field Nodes Y", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_nodes_z"), label: "Field Nodes Z", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((2.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("field_spacing"), label: "Field Spacing", ty: ParamType::Float, default: ParamValue::Float(0.25), range: Some((1.0e-4, 400.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("force_lattices"), label: "Force Lattices", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("impulse_tick"), label: "Impulse Tick", ty: ParamType::Int, default: ParamValue::Float(-1.0), range: Some((-1.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("first_tick"), label: "First Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pool_tiles"), label: "Pool Tiles", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((1.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("wall_faces"), label: "Wall Faces (bits −X +X −Y +Y −Z +Z)", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 63.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_lo_x"), label: "Bounds Lo X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_lo_y"), label: "Bounds Lo Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_lo_z"), label: "Bounds Lo Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_hi_x"), label: "Bounds Hi X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_hi_y"), label: "Bounds Hi Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_hi_z"), label: "Bounds Hi Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("wall_velocity_lo_x"), label: "Wall Velocity Lo X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e3, 1.0e3)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("wall_velocity_lo_y"), label: "Wall Velocity Lo Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e3, 1.0e3)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("wall_velocity_lo_z"), label: "Wall Velocity Lo Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e3, 1.0e3)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("wall_velocity_hi_x"), label: "Wall Velocity Hi X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e3, 1.0e3)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("wall_velocity_hi_y"), label: "Wall Velocity Hi Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e3, 1.0e3)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("wall_velocity_hi_z"), label: "Wall Velocity Hi Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e3, 1.0e3)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Region body of the Live Matter group, after node.matter_grid_update (its grid_out is this grid) and before node.grid_to_matter. reaction/reaction_out alias node.matter_domain's reaction array (16 words per body, the slot the domain cleared for this tick and reads back fenced); reaction_out feeds node.grid_to_matter's reaction, whose reaction_out closes into node.matter_state's reaction_in so the region runs this node every substep; bodies from node.matter_move_bodies; shapes, atlas, tiles and pool_tiles (the world-tile pool the grid is stored in; node p sits at world p·cell_size, no padding nodes), the Bounds box (bounds_lo/hi, wall_faces, wall_velocity_lo/hi), the force field lattice's origin (field_min_x/y/z, only that), gravity, momentum_unit, body_count, dynamic_count and substeps_per_tick from node.matter_domain; tick_index, substep_in_tick and step_dt from node.matter_state; forces, impulses and the field scalars from node.matter_domain, wired exactly as node.matter_grid_update's so both start from the same velocity. Skips its dispatch when dynamic_count is 0 or reaction is unwired.",
    examples: ["WaterFloatingBoxMatter", "WaterDamBreakMatter", "WaterStillPoolMatter"],
    picker: { label: "Matter Body Reaction", category: Atom },
    summary: "Measures how hard the liquid pushes on each floating object so the physics world can move it.",
    category: Particles3D,
    role: Filter,
    aliases: ["body reaction", "two-way coupling", "buoyancy", "fluid force"],
    fusion_kind: Boundary,
    boundary_reason: Blocked,
    wgsl_body: include_str!("shaders/matter_body_reaction_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    wgsl_includes: [READ_SIDE, LIQUID_POSE, LIQUID_COLLIDER, MATTER_BOUNDS, FORCE_LATTICE],
    atomic_outputs: ["reaction_out"],
}

impl Primitive for MatterBodyReaction {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "reaction_out")
            .then(|| {
                input_capacities
                    .iter()
                    .find(|(p, _)| *p == "reaction")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("reaction", "reaction_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let pool_tiles = ctx
            .scalar_or_param("pool_tiles", 1.0)
            .round()
            .clamp(1.0, 16_777_216.0) as u32;
        let vec3 = |ctx: &EffectNodeContext<'_, '_>, name: &str| {
            [
                ctx.scalar_or_param(&format!("{name}_x"), 0.0),
                ctx.scalar_or_param(&format!("{name}_y"), 0.0),
                ctx.scalar_or_param(&format!("{name}_z"), 0.0),
            ]
        };
        let bounds_lo = vec3(ctx, "bounds_lo");
        let bounds_hi = vec3(ctx, "bounds_hi");
        let wall_velocity_lo = vec3(ctx, "wall_velocity_lo");
        let wall_velocity_hi = vec3(ctx, "wall_velocity_hi");
        let field_min = [
            ctx.scalar_or_param("field_min_x", -2.1875),
            ctx.scalar_or_param("field_min_y", -0.1875),
            ctx.scalar_or_param("field_min_z", -2.1875),
        ];
        let int = |ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32| {
            ctx.scalar_or_param(name, default).round().max(0.0) as i32
        };
        let step_dt = ctx.scalar_or_param("step_dt", 4.9e-4);
        let gravity = [
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
        ];
        let wall_faces = int(ctx, "wall_faces", 0.0).min(63);
        let momentum_unit = ctx.scalar_or_param("momentum_unit", 128.0);
        let body_count = int(ctx, "body_count", 0.0).min(MAX_LIQUID_BODIES as i32);
        let tick_index = int(ctx, "tick_index", 0.0);
        let substep_in_tick = int(ctx, "substep_in_tick", 0.0);
        let substeps_per_tick = int(ctx, "substeps_per_tick", 1.0).max(1);
        let dynamic_count = int(ctx, "dynamic_count", 0.0);
        let grid = ctx.inputs.array("grid");
        let tiles = ctx.inputs.array("tiles");
        let colliders = (
            ctx.inputs.array("bodies"),
            ctx.inputs.array("shapes"),
            ctx.inputs.array("atlas"),
        );
        let reaction = ctx.outputs.array("reaction_out");
        let field = ForceLatticeBinding::read(
            ctx,
            ctx.inputs.array("forces"),
            ctx.inputs.array("impulses"),
            "Matter Body Reaction",
        );
        // The GPU is touched on every path so the aliased reaction array keeps
        // its place in the frame's hazard order.
        let gpu = ctx.gpu_encoder();
        let (Some(grid), Some(tiles), (Some(bodies), Some(shapes), Some(atlas)), Some(reaction)) =
            (grid, tiles, colliders, reaction)
        else {
            return;
        };
        if !cell_size.is_finite() || cell_size <= 0.0 {
            ctx.error(format!(
                "Matter Body Reaction: cell_size is {cell_size}; it must be a finite number above 0"
            ));
            return;
        }
        if dynamic_count == 0 || body_count == 0 || step_dt <= 0.0 {
            return;
        }
        let field = match field {
            Ok(field) => field,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let Ok(nodes) = u32::try_from(pool_len(pool_tiles)) else {
            ctx.error(format!(
                "Matter Body Reaction: a pool of {pool_tiles} tiles has more nodes than a dispatch can count"
            ));
            return;
        };
        if grid.size < grid_bytes(pool_tiles) || tiles.size < table_words(pool_tiles) * 4 {
            ctx.error(format!(
                "Matter Body Reaction: grid or tiles is smaller than a pool of {pool_tiles} tiles needs"
            ));
            return;
        }
        let body_rows = (bodies.size / std::mem::size_of::<LiquidBody>() as u64)
            .min(reaction.size / (REACTION_WORDS as u64 * 4)) as i32;
        let body_count = body_count.min(body_rows);
        if body_count == 0 {
            return;
        }
        if !momentum_unit_fits(momentum_unit, cell_size, step_dt) {
            ctx.error(format!(
                "Matter Body Reaction: momentum unit {momentum_unit} is not a power of two at or above cell size / step_dt; wire node.matter_domain's momentum_unit"
            ));
            return;
        }
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = BodyReactionUniforms {
            cell_size,
            step_dt,
            gravity_x: gravity[0],
            gravity: gravity[1],
            gravity_z: gravity[2],
            momentum_unit,
            field_min_x: field_min[0],
            field_min_y: field_min[1],
            field_min_z: field_min[2],
            body_count,
            tick_index,
            substep_in_tick,
            substeps_per_tick,
            dynamic_count,
            field_nodes_x: field.nodes[0],
            field_nodes_y: field.nodes[1],
            field_nodes_z: field.nodes[2],
            field_spacing: field.spacing,
            force_lattices: field.force_lattices,
            impulse_tick: field.impulse_tick,
            first_tick: field.first_tick,
            pool_tiles: pool_tiles as i32,
            wall_faces,
            bounds_lo_x: bounds_lo[0],
            bounds_lo_y: bounds_lo[1],
            bounds_lo_z: bounds_lo[2],
            bounds_hi_x: bounds_hi[0],
            bounds_hi_y: bounds_hi[1],
            bounds_hi_z: bounds_hi[2],
            wall_velocity_lo_x: wall_velocity_lo[0],
            wall_velocity_lo_y: wall_velocity_lo[1],
            wall_velocity_lo_z: wall_velocity_lo[2],
            wall_velocity_hi_x: wall_velocity_hi[0],
            wall_velocity_hi_y: wall_velocity_hi[1],
            wall_velocity_hi_z: wall_velocity_hi[2],
            dispatch_count: nodes,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: grid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: bodies,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: shapes,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: atlas,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: reaction,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: field.forces.unwrap_or(grid),
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 7,
                    buffer: field.impulses.unwrap_or(grid),
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 8,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 9,
                    buffer: reaction,
                    offset: 0,
                },
            ],
            [nodes.div_ceil(256), 1, 1],
            "node.matter_body_reaction",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_body_reaction_generates_an_atomic_node_kernel() {
        let wgsl =
            manifold_node_engine::freeze::codegen::standalone_for_spec::<MatterBodyReaction>()
                .expect("matter_body_reaction codegen");
        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
