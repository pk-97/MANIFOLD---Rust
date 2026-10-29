//! `node.grid_to_matter` — the MLS-MPM grid-to-particle transfer and point
//! update, finished against the colliders, with the
//! push-out's momentum added to each dynamic body's reaction words (D30).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::matter_common::{COLLIDER_BVH, MATTER_MATERIAL, MATTER_TREE};
use crate::matter::{MatterGridNode, MatterPoint, REACTION_WORDS, grid_bytes, momentum_unit_fits};
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::Vec4Vertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::{active_elements, standalone_pipeline};
use manifold_node_engine::scene::world_tiles::{READ_SIDE, table_words};
use manifold_water_liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ToMatterUniforms {
    cell_size: f32,
    step_dt: f32,
    liveliness: f32,
    mu: f32,
    lambda: f32,
    alpha: f32,
    cohesion: f32,
    softening: f32,
    active_count: i32,
    body_count: i32,
    density: f32,
    momentum_unit: f32,
    tick_index: i32,
    substep_in_tick: i32,
    substeps_per_tick: i32,
    dynamic_count: i32,
    pool_tiles: i32,
    open_faces: i32,
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
    bounds_hi_x: f32,
    bounds_hi_y: f32,
    bounds_hi_z: f32,
    kill_height: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

manifold_node_engine::primitive! {
    name: GridToMatter,
    type_id: "node.grid_to_matter",
    purpose: "Transfer the resolved matter grid back to its points (MLS-MPM grid-to-particle): each live point gathers velocity and its affine velocity field from its 27-node stencil, blends toward a FLIP update by Liveliness, moves with the gathered velocity, and runs its material stage: its elastic deformation advects with the gathered velocity gradient, the Drucker-Prager return map keeps it inside the sand's yield cone (cohesion shifts the cone's tip into tension and softens with plastic strain), and the point keeps its new deformation, volume ratio, plastic state and Kirchhoff stress. A point that ends inside a collider steps out onto its surface and loses the velocity pointing into it; a dynamic body gains the momentum the point lost, added to its reaction words. A point whose cell has no tile in the pool is removed (id 0, volume ratio 0, counted by node.matter_stats); a point beyond an Open face of the bounds box, or below the kill height, is removed too (id 0, volume ratio kept).",
    inputs: {
        points: Array(MatterPoint) required,
        grid: Array(MatterGridNode) required,
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        collider_nodes: Array(Vec4Vertex) optional,
        collider_triangles: Array(Vec4Vertex) optional,
        reaction: Array(i32) optional,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        liveliness: ScalarF32 optional,
        mu: ScalarF32 optional,
        lambda: ScalarF32 optional,
        alpha: ScalarF32 optional,
        cohesion: ScalarF32 optional,
        softening: ScalarF32 optional,
        active_count: ScalarF32 optional,
        body_count: ScalarF32 optional,
        density: ScalarF32 optional,
        momentum_unit: ScalarF32 optional,
        tick_index: ScalarF32 optional,
        substep_in_tick: ScalarF32 optional,
        substeps_per_tick: ScalarF32 optional,
        dynamic_count: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
        open_faces: ScalarF32 optional,
        bounds_lo_x: ScalarF32 optional, bounds_lo_y: ScalarF32 optional, bounds_lo_z: ScalarF32 optional,
        bounds_hi_x: ScalarF32 optional, bounds_hi_y: ScalarF32 optional, bounds_hi_z: ScalarF32 optional,
        kill_height: ScalarF32 optional,
    },
    outputs: {
        points_out: Array(MatterPoint),
        reaction_out: Array(i32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("step_dt"), label: "Substep (s)", ty: ParamType::Float, default: ParamValue::Float(4.9e-4), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("liveliness"), label: "FLIP Ratio", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("mu"), label: "Shear Modulus (Pa)", ty: ParamType::Float, default: ParamValue::Float(1.36e5), range: Some((0.0, 1.0e9)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lambda"), label: "Lamé λ (Pa)", ty: ParamType::Float, default: ParamValue::Float(2.04e5), range: Some((0.0, 1.0e9)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("alpha"), label: "Friction α", ty: ParamType::Float, default: ParamValue::Float(0.386), range: Some((0.0, 2.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cohesion"), label: "Cohesion Strain", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("softening"), label: "Cohesion Softening", ty: ParamType::Float, default: ParamValue::Float(0.05), range: Some((1.0e-6, 10.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("active_count"), label: "Active Count", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_000_000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("body_count"), label: "Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("density"), label: "Density (kg/m³)", ty: ParamType::Float, default: ParamValue::Float(1600.0), range: Some((1.0, 1.0e5)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("momentum_unit"), label: "Momentum Unit (m/s)", ty: ParamType::Float, default: ParamValue::Float(128.0), range: Some((1.0e-3, 1.0e9)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_index"), label: "Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substep_in_tick"), label: "Substep in Tick", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("substeps_per_tick"), label: "Substeps per Tick", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("dynamic_count"), label: "Dynamic Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pool_tiles"), label: "Pool Tiles", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((1.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("open_faces"), label: "Open Faces", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 63.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_lo_x"), label: "Bounds Min X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_lo_y"), label: "Bounds Min Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_lo_z"), label: "Bounds Min Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_hi_x"), label: "Bounds Max X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_hi_y"), label: "Bounds Max Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_hi_z"), label: "Bounds Max Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("kill_height"), label: "Kill Height", ty: ParamType::Float, default: ParamValue::Float(f32::MIN), range: Some((f32::MIN, f32::MAX)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Removal is a short pool (the point's cell has no stored tile), an Open face of the bounds box or the kill height. Wire tiles from the same step as grid. Region body of the Live Matter group, after node.matter_grid_update and node.matter_body_reaction. points/points_out alias the node.matter_state point buffer (updated in place); grid is read as a gather. Liveliness 0 is APIC (calm, dissipative); toward 1 it keeps more of each point's own velocity change (livelier splashes, more noise). Positions always advect with the gathered velocity. bodies comes from node.matter_move_bodies (the substep-end pose node.matter_grid_update also reads); shapes, atlas and body_count from node.matter_domain. With bodies unwired no collider is read. reaction/reaction_out alias the domain's reaction words after node.matter_body_reaction (wire its reaction_out here, and this reaction_out into node.matter_state's reaction_in); density, momentum_unit, substeps_per_tick and dynamic_count come from node.matter_domain, tick_index and substep_in_tick from node.matter_state. Without a reaction slot covering every body, or with dynamic_count 0, no reaction is written.",
    examples: ["SandBoxDrop", "SandColumn"],
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
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    wgsl_includes: [READ_SIDE, LIQUID_POSE, LIQUID_COLLIDER, COLLIDER_BVH, MATTER_TREE, MATTER_MATERIAL],
    atomic_outputs: ["reaction_out"],
}

impl Primitive for GridToMatter {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        let input = match port_name {
            "points_out" => "points",
            "reaction_out" => "reaction",
            _ => return None,
        };
        input_capacities
            .iter()
            .find(|(p, _)| *p == input)
            .map(|&(_, n)| n)
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("points", "points_out"), ("reaction", "reaction_out")]
    }

    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        Some("active_count")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        if cell_size <= 0.0 {
            ctx.error(format!(
                "Grid to Matter: cell_size is {cell_size}; it must be above 0"
            ));
            return;
        }
        let pool_tiles = ctx
            .scalar_or_param("pool_tiles", 1.0)
            .round()
            .clamp(1.0, 16_777_216.0) as u32;
        let bounds_lo = ["bounds_lo_x", "bounds_lo_y", "bounds_lo_z"]
            .map(|name| ctx.scalar_or_param(name, 0.0));
        let bounds_hi = ["bounds_hi_x", "bounds_hi_y", "bounds_hi_z"]
            .map(|name| ctx.scalar_or_param(name, 0.0));
        let open_faces = ctx
            .scalar_or_param("open_faces", 0.0)
            .round()
            .clamp(0.0, 63.0) as i32;
        let kill_height = ctx.scalar_or_param("kill_height", f32::MIN);
        let step_dt = ctx.scalar_or_param("step_dt", 4.9e-4);
        let liveliness = ctx.scalar_or_param("liveliness", 0.0).clamp(0.0, 1.0);
        let [mu, lambda, alpha, cohesion, softening] = [
            ("mu", 1.36e5),
            ("lambda", 2.04e5),
            ("alpha", 0.386),
            ("cohesion", 0.0),
            ("softening", 0.05),
        ]
        .map(|(name, default)| ctx.scalar_or_param(name, default));
        if !(mu > 0.0 && lambda > 0.0 && alpha >= 0.0 && cohesion >= 0.0 && softening > 0.0) {
            ctx.error(format!(
                "Grid to Matter: the material (μ {mu}, λ {lambda}, α {alpha}, cohesion {cohesion}, softening {softening}) is not physical; wire node.matter_domain's"
            ));
            return;
        }
        let requested = ctx.scalar_or_param("active_count", 0.0).round().max(0.0) as u32;
        let body_count = ctx
            .scalar_or_param("body_count", 0.0)
            .round()
            .clamp(0.0, MAX_LIQUID_BODIES as f32) as i32;
        let int = |ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32| {
            ctx.scalar_or_param(name, default).round().max(0.0) as i32
        };
        let density = ctx.scalar_or_param("density", 1600.0);
        let momentum_unit = ctx.scalar_or_param("momentum_unit", 128.0);
        let tick_index = int(ctx, "tick_index", 0.0);
        let substep_in_tick = int(ctx, "substep_in_tick", 0.0);
        let substeps_per_tick = int(ctx, "substeps_per_tick", 1.0).max(1);
        let mut dynamic_count = int(ctx, "dynamic_count", 0.0);
        // In place: the point buffer is mutated whether or not `points_out`
        // is consumed, so the GPU is touched on every path.
        let points = ctx.inputs.array("points");
        let grid = ctx.inputs.array("grid");
        let tiles = ctx.inputs.array("tiles");
        let colliders = (
            ctx.inputs.array("bodies"),
            ctx.inputs.array("shapes"),
            ctx.inputs.array("atlas"),
            ctx.inputs.array("collider_nodes"),
            ctx.inputs.array("collider_triangles"),
        );
        let reaction = ctx.inputs.array("reaction");
        let unit_fits = momentum_unit_fits(momentum_unit, cell_size, step_dt);
        if dynamic_count > 0 && reaction.is_some() && !unit_fits {
            ctx.error(format!(
                "Grid to Matter: momentum unit {momentum_unit} is not a power of two at or above cell size / step_dt; wire node.matter_domain's momentum_unit"
            ));
        }
        let gpu = ctx.gpu_encoder();
        let (Some(points), Some(grid), Some(tiles)) = (points, grid, tiles) else {
            return;
        };
        let active = active_elements::<MatterPoint>(points.size, requested);
        if active == 0 || step_dt <= 0.0 {
            return;
        }
        if grid.size < grid_bytes(pool_tiles) || tiles.size < table_words(pool_tiles) * 4 {
            ctx.error(format!(
                "Grid to Matter: the grid or tile table is shorter than a pool of {pool_tiles} tiles; wire grid and tiles from the same step, fed by the same node.matter_domain"
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
        // Reaction words are written only with a slot covering every body and
        // a momentum unit the words can hold; the grid stands in otherwise.
        let covered =
            reaction.is_some_and(|r| r.size >= body_count as u64 * u64::from(REACTION_WORDS) * 4);
        if !covered || !unit_fits || body_count == 0 {
            dynamic_count = 0;
        }
        let reaction = reaction.filter(|_| dynamic_count > 0).unwrap_or(grid);
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = ToMatterUniforms {
            cell_size,
            step_dt,
            liveliness,
            mu,
            lambda,
            alpha,
            cohesion,
            softening,
            active_count: active as i32,
            body_count,
            density,
            momentum_unit,
            tick_index,
            substep_in_tick,
            substeps_per_tick,
            dynamic_count,
            pool_tiles: pool_tiles as i32,
            open_faces,
            bounds_lo_x: bounds_lo[0],
            bounds_lo_y: bounds_lo[1],
            bounds_lo_z: bounds_lo[2],
            bounds_hi_x: bounds_hi[0],
            bounds_hi_y: bounds_hi[1],
            bounds_hi_z: bounds_hi[2],
            kill_height,
            dispatch_count: active,
            _pad0: 0,
            _pad1: 0,
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
                    buffer: reaction,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 9,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 10,
                    buffer: points,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 11,
                    buffer: reaction,
                    offset: 0,
                },
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
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<GridToMatter>()
            .expect("grid_to_matter codegen");
        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
    }

    #[test]
    fn grid_to_matter_is_a_boundary_for_its_reaction_words() {
        use manifold_node_engine::exec::effect_node::EffectNode;
        let node = GridToMatter::new();
        assert_eq!(
            node.fusion_kind(),
            manifold_node_engine::freeze::classify::FusionKind::Boundary
        );
        assert_eq!(node.atomic_outputs(), &["reaction_out"]);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
