//! `node.matter_grid_update` — resolve the matter grid: momentum to velocity,
//! gravity, the scene's forces and impulses (seam P8), the Bounds box's walls,
//! colliders and the CFL clamp.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::matter_common::{COLLIDER_BVH, MATTER_BOUNDS, MATTER_TREE};
use crate::matter::{MatterGridNode, grid_accum_bytes, grid_bytes, momentum_unit_fits};
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::Vec4Vertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, pool_len, table_words};
use manifold_nodes_physics::force_lattice::{FORCE_LATTICE, ForceLatticeBinding};
use manifold_water_liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GridUpdateUniforms {
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
    _pad0: u32,
    _pad1: u32,
}

manifold_node_engine::primitive! {
    name: MatterGridUpdate,
    type_id: "node.matter_grid_update",
    purpose: "Resolve the matter grid: turn each node's accumulated momentum into velocity, add gravity and the scene's acceleration field (read from a coarse force lattice), add the scene's impulses once on the first substep of their tick (from a coarse impulse lattice), stop velocity into the Bounds box's Wall faces on every node on or past the plane (relative to the wall's speed), project velocity out of colliders (a node inside a body moving into it keeps the body's normal velocity and friction-limited sliding), and clamp each component to 0.9 cells per substep. Keeps the pre-force velocity for the Liveliness blend.",
    inputs: {
        accum: Array(i32) required,
        grid: Array(MatterGridNode) required,
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        collider_nodes: Array(Vec4Vertex) optional,
        collider_triangles: Array(Vec4Vertex) optional,
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
        grid_out: Array(MatterGridNode),
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
    composition_notes: "Region body of the Live Matter group, between node.matter_to_grid and node.grid_to_matter. grid/grid_out alias the node.matter_state grid array (one MatterGridNode per world-tile node, pooled: 512 per tile slot and a canonical node last; node p sits at world p·cell_size); accum is read as a gather (4 words per node) and tiles is the world-tile table the pool is found through, with pool_tiles slots. There are no padding nodes. Gravity, the Bounds box (bounds_lo/hi, wall_faces, wall_velocity_lo/hi), the lattice minimum, body_count, shapes, atlas and momentum_unit (the same value node.matter_to_grid reads) come from node.matter_domain, bodies from node.matter_move_bodies, step_dt, tick_index and substep_in_tick from the substep boundary. forces, impulses and the field scalars (field_nodes_x/y/z, field_spacing, force_lattices, first_tick, impulse_tick) come from node.matter_domain; field_min_x/y/z are now only the force field lattice's origin, the field lattices start there. Every substep adds the force lattice of its tick: one lattice per tick from first_tick, or one for all ticks when force_lattices is 1 (0: no forces); impulses apply once, on substep 0 of tick impulse_tick (−1: none). With bodies unwired no collider is read; with forces or impulses unwired neither is read.",
    examples: ["WaterDamBreakMatter", "WaterStillPoolMatter"],
    picker: { label: "Matter Grid Update", category: Atom },
    summary: "Turns the grid's gathered liquid momentum into velocities, adds gravity and stops the liquid at the walls.",
    category: Particles3D,
    role: Filter,
    aliases: ["grid update", "mpm grid", "resolve grid"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/matter_grid_update_body.wgsl"),
    input_access: [BufferGather, Coincident, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    wgsl_includes: [READ_SIDE, LIQUID_POSE, LIQUID_COLLIDER, COLLIDER_BVH, MATTER_TREE, MATTER_BOUNDS, FORCE_LATTICE],
}

impl Primitive for MatterGridUpdate {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "grid_out")
            .then(|| {
                input_capacities
                    .iter()
                    .find(|(p, _)| *p == "grid")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("grid", "grid_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let pool_tiles = ctx
            .scalar_or_param("pool_tiles", 1.0)
            .round()
            .clamp(1.0, 16_777_216.0) as u32;
        let wall_faces = ctx
            .scalar_or_param("wall_faces", 0.0)
            .round()
            .clamp(0.0, 63.0) as i32;
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
        let step_dt = ctx.scalar_or_param("step_dt", 4.9e-4);
        let gravity = [
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
        ];
        let momentum_unit = ctx.scalar_or_param("momentum_unit", 128.0);
        let body_count = ctx
            .scalar_or_param("body_count", 0.0)
            .round()
            .clamp(0.0, MAX_LIQUID_BODIES as f32) as i32;
        // In place on the grid input; the GPU is touched on every path.
        let accum = ctx.inputs.array("accum");
        let grid = ctx.inputs.array("grid");
        let tiles = ctx.inputs.array("tiles");
        let colliders = (
            ctx.inputs.array("bodies"),
            ctx.inputs.array("shapes"),
            ctx.inputs.array("atlas"),
            ctx.inputs.array("collider_nodes"),
            ctx.inputs.array("collider_triangles"),
        );
        let tick_index = ctx.scalar_or_param("tick_index", 0.0).round().max(0.0) as i32;
        let substep_in_tick = ctx.scalar_or_param("substep_in_tick", 0.0).round().max(0.0) as i32;
        let field = ForceLatticeBinding::read(
            ctx,
            ctx.inputs.array("forces"),
            ctx.inputs.array("impulses"),
            "Matter Grid Update",
        );
        let gpu = ctx.gpu_encoder();
        let (Some(accum), Some(grid), Some(tiles)) = (accum, grid, tiles) else {
            return;
        };
        if !cell_size.is_finite() || cell_size <= 0.0 {
            ctx.error(format!(
                "Matter Grid Update: cell_size is {cell_size}; it must be a finite number above 0"
            ));
            return;
        }
        let field = match field {
            Ok(field) => field,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        if step_dt <= 0.0 {
            return;
        }
        let Ok(nodes) = u32::try_from(pool_len(pool_tiles)) else {
            ctx.error(format!(
                "Matter Grid Update: a pool of {pool_tiles} tiles has more nodes than a dispatch can count"
            ));
            return;
        };
        if grid.size < grid_bytes(pool_tiles)
            || accum.size < grid_accum_bytes(pool_tiles)
            || tiles.size < table_words(pool_tiles) * 4
        {
            ctx.error(format!(
                "Matter Grid Update: grid, accum or tiles is smaller than a pool of {pool_tiles} tiles needs"
            ));
            return;
        }
        if !momentum_unit_fits(momentum_unit, cell_size, step_dt) {
            ctx.error(format!(
                "Matter Grid Update: momentum unit {momentum_unit} is not a power of two at or above cell size / step_dt; wire node.matter_domain's momentum_unit"
            ));
            return;
        }
        // Without all five collider arrays the body loop runs zero times over
        // the grid buffer bound in their slots.
        let (bodies, shapes, atlas, collider_nodes, collider_triangles, body_count) =
            match colliders {
                (Some(bodies), Some(shapes), Some(atlas), Some(nodes), Some(triangles)) => {
                    let rows = (bodies.size / std::mem::size_of::<LiquidBody>() as u64) as i32;
                    (
                        bodies,
                        shapes,
                        atlas,
                        nodes,
                        triangles,
                        body_count.min(rows),
                    )
                }
                _ => (grid, grid, grid, grid, grid, 0),
            };
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let impulse_bytes = field.nodes.iter().map(|&n| n as u64).product::<u64>() * 16;
        let impulse_offset = tick_index
            .checked_sub(field.impulse_tick)
            .filter(|&ordinal| field.impulse_tick >= 0 && ordinal >= 0)
            .map(|ordinal| ordinal as u64 * impulse_bytes)
            .filter(|&offset| {
                field
                    .impulses
                    .is_some_and(|buffer| offset + impulse_bytes <= buffer.size)
            });
        let uniforms = GridUpdateUniforms {
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
            field_nodes_x: field.nodes[0],
            field_nodes_y: field.nodes[1],
            field_nodes_z: field.nodes[2],
            field_spacing: field.spacing,
            force_lattices: field.force_lattices,
            impulse_tick: if impulse_offset.is_some() {
                tick_index
            } else {
                -1
            },
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
            _pad0: 0,
            _pad1: 0,
        };
        // An unwired lattice is never read (force_lattices 0, impulse_tick −1).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: accum,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: grid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: bodies,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: shapes,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: atlas,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: collider_nodes,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 7,
                    buffer: collider_triangles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 8,
                    buffer: field.forces.unwrap_or(grid),
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 9,
                    buffer: field.impulses.unwrap_or(grid),
                    offset: impulse_offset.unwrap_or(0),
                },
                GpuBinding::Buffer {
                    binding: 10,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 11,
                    buffer: grid,
                    offset: 0,
                },
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
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<MatterGridUpdate>()
            .expect("matter_grid_update codegen");
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
