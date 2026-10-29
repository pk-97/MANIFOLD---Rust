//! `node.liquid_solid_distance` — the display's solid for the particle-frame
//! seam: walls and bodies as one signed distance per node of the display's
//! world tiles.

use manifold_gpu::BufferUse;
use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer};

use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_physics::clock::TICK;
use manifold_water_liquid::bodies::{COLLIDER_BVH, LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_pool::tile_pool;

/// Codegen uniform layout: the params in `PARAMS` order, then the injected
/// element count and padding to a 16-byte multiple.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SolidDistanceUniforms {
    cell_size: f32,
    closed_faces: i32,
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
    bounds_hi_x: f32,
    bounds_hi_y: f32,
    bounds_hi_z: f32,
    body_count: i32,
    rows: i32,
    tick_seconds: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

const _: () = assert!(std::mem::size_of::<SolidDistanceUniforms>() == 64);

/// Bytes of the clock plan the body reads (`gpu_flip_clock.wgsl`'s Plan): zeros,
/// so poses use the tick wired in.
pub(crate) const CLOCK_PLAN_BYTES: u64 = 48;

manifold_node_engine::primitive! {
    name: LiquidSolidDistance,
    type_id: "node.liquid_solid_distance",
    purpose: "Write the display's solid: per node of the display's world tiles (node p at (p - 1/2) cells, element ts_cell_at(p)), the smaller of the distance to the nearest Wall face of the Bounds box (bounds_lo/hi in metres, closed_faces the Wall faces; 1e30 with none) and every enabled body's signed distance at the end of this frame's last tick (positive in free space, negative inside a solid). Bodies are sampled from their shapes' lattices in the atlas through their pose, in metres along each shape's stretch at the surface. A free slot and the canonical element read 1e30, the solid of a tile the table does not hold.",
    inputs: {
        bodies: Array(LiquidBody) required,
        shapes: Array(LiquidShape) required,
        atlas: Array(u32) required,
        collider_nodes: Array(manifold_node_engine::mesh::Vec4Vertex) required,
        collider_triangles: Array(manifold_node_engine::mesh::Vec4Vertex) required,
        tiles: Array(u32) required,
        closed_faces: ScalarF32 optional,
        bounds_lo_x: ScalarF32 optional, bounds_lo_y: ScalarF32 optional, bounds_lo_z: ScalarF32 optional,
        bounds_hi_x: ScalarF32 optional, bounds_hi_y: ScalarF32 optional, bounds_hi_z: ScalarF32 optional,
        body_count: ScalarF32 optional,
        rows: ScalarF32 optional,
        tick_seconds: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        solid: Array(f32),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        ParamDef { name: Cow::Borrowed("closed_faces"), label: "Closed Faces (bits −X +X −Y +Y −Z +Z)", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 63.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_lo_x"), label: "Bounds Min X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_lo_y"), label: "Bounds Min Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_lo_z"), label: "Bounds Min Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_hi_x"), label: "Bounds Max X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_hi_y"), label: "Bounds Max Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds_hi_z"), label: "Bounds Max Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0e4, 1.0e4)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("body_count"), label: "Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, MAX_LIQUID_BODIES as f32)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("rows"), label: "Rows", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_seconds"), label: "Tick (s)", ty: ParamType::Float, default: ParamValue::Float(TICK as f32), range: Some((0.0, 1.0)), enum_values: &[] },
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Once per frame, after the liquid's tick region. bodies, shapes, atlas, body_count and rows come from the liquid's domain (node.matter_domain, node.gpu_flip_domain); the Bounds box and the Wall faces from node.gpu_flip_domain (unwired: no walls); tiles, pool_tiles and cell_size from node.surface_tiles. solid holds exactly one value per pool element, sized every frame from the same pool the dispatch covers; a pool the device cannot hold is a named error. Feed solid to the display atoms that stop at solids (node.push_out_of_solid) and to the water object's solid.",
    examples: ["SandColumn"],
    picker: { label: "Liquid Solid Distance", category: Atom },
    summary: "Marks where the walls and solid objects are around a liquid, so its surface stops at them.",
    category: Particles3D,
    role: Filter,
    aliases: ["solid lattice", "collider distance", "solid field"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/liquid_solid_distance_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    wgsl_includes: [READ_SIDE, LIQUID_POSE, LIQUID_COLLIDER, COLLIDER_BVH],
    extra_fields: {
        solid: Option<GpuBuffer> = None,
        clock_plan: Option<GpuBuffer> = None,
    },
}

impl Primitive for LiquidSolidDistance {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "solid"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "solid").then_some(self.solid.as_ref()).flatten()
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-value hint, sized to the pool at run time.
        (port_name == "solid").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Liquid Solid Distance") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let closed_faces = ctx
            .scalar_or_param("closed_faces", 0.0)
            .round()
            .clamp(0.0, 63.0) as i32;
        let bounds = [
            ctx.scalar_or_param("bounds_lo_x", 0.0),
            ctx.scalar_or_param("bounds_lo_y", 0.0),
            ctx.scalar_or_param("bounds_lo_z", 0.0),
            ctx.scalar_or_param("bounds_hi_x", 0.0),
            ctx.scalar_or_param("bounds_hi_y", 0.0),
            ctx.scalar_or_param("bounds_hi_z", 0.0),
        ];
        let body_count = ctx
            .scalar_or_param("body_count", 0.0)
            .round()
            .clamp(0.0, MAX_LIQUID_BODIES as f32) as i32;
        let rows = ctx.scalar_or_param("rows", 0.0).round().max(0.0) as i32;
        let tick_seconds = ctx.scalar_or_param("tick_seconds", TICK as f32);
        let inputs = (
            ctx.inputs.array("bodies"),
            ctx.inputs.array("shapes"),
            ctx.inputs.array("atlas"),
            ctx.inputs.array("collider_nodes"),
            ctx.inputs.array("collider_triangles"),
            ctx.inputs.array("tiles"),
        );
        let elements = pool.elements();
        let Ok(count) = u32::try_from(elements) else {
            ctx.error(format!(
                "Liquid Solid Distance: a pool of {} tiles has more elements than an index can address",
                pool.pool_tiles
            ));
            return;
        };
        // The storage follows the element count the dispatch covers, before
        // it is encoded.
        let bytes = elements * 4;
        if self.solid.as_ref().is_none_or(|solid| solid.size != bytes) {
            let device = ctx.gpu_encoder().device;
            let created = manifold_node_engine::load::expand::admit_world_bytes(
                device.modifier_memory_snapshot(),
                bytes,
            )
            .map_err(|error| error.to_string())
            .and_then(|()| device.try_create_buffer(bytes, BufferUse::Upload));
            match created {
                Ok(buffer) => {
                    buffer.zero_fill();
                    self.solid = Some(buffer);
                }
                Err(error) => {
                    ctx.error(format!(
                        "Liquid Solid Distance: a pool of {} tiles needs {bytes} bytes the device cannot give: {error}. Lower Pool Scale.",
                        pool.pool_tiles
                    ));
                    return;
                }
            }
        }
        if self.clock_plan.is_none() {
            let plan = ctx
                .gpu_encoder()
                .device
                .try_create_buffer(CLOCK_PLAN_BYTES, BufferUse::Upload);
            match plan {
                Ok(plan) => {
                    plan.zero_fill();
                    self.clock_plan = Some(plan);
                }
                Err(error) => {
                    ctx.error(format!(
                        "Liquid Solid Distance: could not allocate its clock plan: {error}"
                    ));
                    return;
                }
            }
        }
        let gpu = ctx.gpu_encoder();
        let (
            (Some(bodies), Some(shapes), Some(atlas), Some(nodes), Some(triangles), Some(tiles)),
            Some(solid),
            Some(plan),
        ) =
            (inputs, self.solid.as_ref(), self.clock_plan.as_ref())
        else {
            return;
        };
        let rows = rows.min(
            (bodies.size / std::mem::size_of::<LiquidBody>() as u64).min(i32::MAX as u64) as i32,
        );
        let uniforms = SolidDistanceUniforms {
            cell_size: pool.cell_size,
            closed_faces,
            bounds_lo_x: bounds[0],
            bounds_lo_y: bounds[1],
            bounds_lo_z: bounds[2],
            bounds_hi_x: bounds[3],
            bounds_hi_y: bounds[4],
            bounds_hi_z: bounds[5],
            body_count,
            rows,
            tick_seconds,
            pool_tiles: pool.pool_tiles as f32,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: bodies,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: shapes,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: atlas,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: nodes,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: triangles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 7,
                    buffer: solid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 8,
                    buffer: plan,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.liquid_solid_distance",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquid_solid_distance_generates_a_gathering_node_kernel() {
        let wgsl =
            manifold_node_engine::freeze::codegen::standalone_for_spec::<LiquidSolidDistance>()
                .expect("liquid_solid_distance codegen");
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

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
