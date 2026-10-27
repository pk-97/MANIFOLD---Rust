//! Nearest solid-object metadata for FLIP whitewater influence and dust emission.
//! Ported from FLIP Fluids (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_physics::clock::TICK;
use manifold_water_liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};
use manifold_water_liquid::float_param;
use manifold_water_liquid::whitewater::{WhitewaterPool, whitewater_pool};

/// Nearest solid-object properties at a node of the pooled node arrays. Kind: none 0, domain 1, obstacle 2.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WhitewaterSource {
    pub influence: f32,
    pub dust_strength: f32,
    pub kind: u32,
    pub pad: u32,
}
impl manifold_node_engine::ports::KnownItem for WhitewaterSource {
    const SPECS: &'static [manifold_node_engine::ports::ChannelSpec] = &[
        manifold_node_engine::ports::ChannelSpec {
            name: manifold_node_engine::ports::ChannelName::from_str("influence"),
            ty: manifold_node_engine::ports::ChannelElementType::F32,
        },
        manifold_node_engine::ports::ChannelSpec {
            name: manifold_node_engine::ports::ChannelName::from_str("dust_strength"),
            ty: manifold_node_engine::ports::ChannelElementType::F32,
        },
        manifold_node_engine::ports::ChannelSpec {
            name: manifold_node_engine::ports::ChannelName::from_str("kind"),
            ty: manifold_node_engine::ports::ChannelElementType::U32,
        },
        manifold_node_engine::ports::ChannelSpec {
            name: manifold_node_engine::ports::ChannelName::from_str("pad0"),
            ty: manifold_node_engine::ports::ChannelElementType::U32,
        },
    ];
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ObstacleSourceUniforms {
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
    influence: f32,
    dust_strength: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
}

manifold_node_engine::primitive! {
    name: WhitewaterObstacleSource,
    type_id: "node.whitewater_obstacle_source",
    purpose: "Find the nearest Wall face of the Bounds box (bounds_lo/hi in metres, closed_faces the Wall faces) or enabled obstacle at every node of the liquid step's world tiles (a pooled node array: element p holds the low corner of world cell p), using the same posed solid distances as liquid_solid_distance. Emit its whitewater influence, dust strength and kind (none 0, domain 1, obstacle 2) as source, and the posed solid distance itself as distance, 1e30 where nothing is near. Elements of tiles the table does not hold, and the canonical element, carry zeroed metadata and distance 1e30. A zero dust strength disables that obstacle source.",
    inputs: {
        bodies: Array(LiquidBody) required,
        shapes: Array(LiquidShape) required,
        atlas: Array(u32) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        closed_faces: ScalarF32 optional,
        bounds_lo_x: ScalarF32 optional, bounds_lo_y: ScalarF32 optional, bounds_lo_z: ScalarF32 optional,
        bounds_hi_x: ScalarF32 optional, bounds_hi_y: ScalarF32 optional, bounds_hi_z: ScalarF32 optional,
        body_count: ScalarF32 optional,
        rows: ScalarF32 optional,
        tick_seconds: ScalarF32 optional,
        influence: ScalarF32 optional, dust_strength: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        source: Array(WhitewaterSource),
        distance: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
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
        ParamDef { name: Cow::Borrowed("influence"), label: "Obstacle Whitewater Influence", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("dust_strength"), label: "Obstacle Dust Strength", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 100.0)), enum_values: &[] },
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16777216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire the same body, shape and atlas inputs as liquid_solid_distance, and tiles, pool_tiles and cell_size from node.gpu_flip_step; source feeds whitewater_step.obstacle_source and dust_potential.source, distance their solid. Both outputs are node arrays of pool_len(pool_tiles) elements. Domain influence is resolved from the whitewater base level; obstacle controls default to FLIP MeshObject defaults. A custom nearest-object metadata producer may replace this node for per-object properties.",
    examples: ["WaterDamBreakMatter"],
    picker: { label: "Whitewater Obstacle Source", category: Atom },
    summary: "Supplies obstacle properties for dust emission and whitewater influence.",
    category: Particles3D,
    role: Filter,
    aliases: ["dust obstacle source", "whitewater influence source"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/whitewater_obstacle_source_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather],
    wgsl_includes: [LIQUID_POSE, LIQUID_COLLIDER, READ_SIDE],
    extra_fields: {
        source: Option<GpuBuffer> = None,
        distance: Option<GpuBuffer> = None,
    },
}

impl Primitive for WhitewaterObstacleSource {
    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "source" | "distance")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "source" => self.source.as_ref(),
            "distance" => self.distance.as_ref(),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage, sized to the tile pool at run time.
        matches!(port_name, "source" | "distance").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match whitewater_pool(ctx, "Whitewater Obstacle Source") {
            Ok(pool) => pool,
            Err(refusal) => {
                ctx.error(refusal);
                return;
            }
        };
        let closed_faces = ctx
            .scalar_or_param("closed_faces", 0.0)
            .round()
            .clamp(0.0, 63.0) as i32;
        let bounds = [
            "bounds_lo_x",
            "bounds_lo_y",
            "bounds_lo_z",
            "bounds_hi_x",
            "bounds_hi_y",
            "bounds_hi_z",
        ]
        .map(|name| ctx.scalar_or_param(name, 0.0));
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
            ctx.inputs.array("tiles"),
        );
        // The storage follows the pool the dispatch covers, before it is
        // encoded.
        let device = ctx.gpu_encoder().device;
        let mut refused = None;
        for (buffer, bytes) in [
            (&mut self.source, pool.elements() * SOURCE_BYTES),
            (&mut self.distance, pool.elements() * 4),
        ] {
            if buffer.as_ref().is_some_and(|b| b.size == bytes) {
                continue;
            }
            let created = manifold_node_engine::load::expand::admit_candidate_bytes(
                device.modifier_memory_snapshot(),
                bytes,
            )
            .map_err(|error| error.to_string())
            .and_then(|()| device.try_create_buffer_shared(bytes));
            match created {
                Ok(created) => {
                    created.zero_fill();
                    *buffer = Some(created);
                }
                Err(error) => {
                    *buffer = None;
                    refused = Some(format!(
                        "Whitewater Obstacle Source: a pool of {} tiles needs {bytes} bytes the device cannot give: {error}. Raise Cell Size.",
                        pool.pool_tiles
                    ));
                    break;
                }
            }
        }
        if let Some(error) = refused {
            ctx.error(error);
            return;
        }
        let influence = ctx.scalar_or_param("influence", 1.0);
        let dust_strength = ctx.scalar_or_param("dust_strength", 1.0);
        let gpu = ctx.gpu_encoder();
        let ((Some(bodies), Some(shapes), Some(atlas), Some(tiles)), Some(source), Some(distance)) =
            (inputs, self.source.as_ref(), self.distance.as_ref())
        else {
            return;
        };
        let job = ObstacleSourceJob {
            influence,
            dust_strength,
            pool,
            closed_faces,
            bounds_lo: [bounds[0], bounds[1], bounds[2]],
            bounds_hi: [bounds[3], bounds[4], bounds[5]],
            body_count,
            rows,
            tick_seconds,
            bodies,
            shapes,
            atlas,
            tiles,
            source,
            distance,
        };
        encode_obstacle_source(
            &mut self.pipeline,
            gpu.device,
            gpu.native_enc,
            &job,
            "node.whitewater_obstacle_source",
        );
    }
}

/// Bytes of one [`WhitewaterSource`].
const SOURCE_BYTES: u64 = std::mem::size_of::<WhitewaterSource>() as u64;

/// The nearest-object metadata and posed solid distance of every element of
/// the pooled node arrays of `pool`.
pub(crate) struct ObstacleSourceJob<'a> {
    pub influence: f32,
    pub dust_strength: f32,
    pub pool: WhitewaterPool,
    /// Faces of the Bounds box that are solid Walls (bits −X +X −Y +Y −Z +Z).
    pub closed_faces: i32,
    /// The Bounds box in metres, as placed.
    pub bounds_lo: [f32; 3],
    pub bounds_hi: [f32; 3],
    pub body_count: i32,
    pub rows: i32,
    pub tick_seconds: f32,
    pub bodies: &'a GpuBuffer,
    pub shapes: &'a GpuBuffer,
    pub atlas: &'a GpuBuffer,
    /// The world-tile table of the pool.
    pub tiles: &'a GpuBuffer,
    /// At least one 16-byte WhitewaterSource per pool element.
    pub source: &'a GpuBuffer,
    /// At least one f32 per pool element.
    pub distance: &'a GpuBuffer,
}

/// The node's kernel, for a stage that writes the node arrays inside its own
/// dispatch chain (`node.gpu_flip_step`). `slot` holds the codegen pipeline.
pub(crate) fn encode_obstacle_source(
    slot: &mut Option<GpuComputePipeline>,
    device: &GpuDevice,
    encoder: &mut GpuEncoder,
    job: &ObstacleSourceJob<'_>,
    label: &str,
) {
    let elements = job.pool.elements() as u32;
    let rows = job.rows.min(
        (job.bodies.size / std::mem::size_of::<LiquidBody>() as u64).min(i32::MAX as u64) as i32,
    );
    let pipeline = standalone_pipeline::<WhitewaterObstacleSource>(slot, device);
    let uniforms = ObstacleSourceUniforms {
        cell_size: job.pool.cell_size,
        closed_faces: job.closed_faces,
        bounds_lo_x: job.bounds_lo[0],
        bounds_lo_y: job.bounds_lo[1],
        bounds_lo_z: job.bounds_lo[2],
        bounds_hi_x: job.bounds_hi[0],
        bounds_hi_y: job.bounds_hi[1],
        bounds_hi_z: job.bounds_hi[2],
        body_count: job.body_count,
        rows,
        tick_seconds: job.tick_seconds,
        influence: job.influence,
        dust_strength: job.dust_strength,
        pool_tiles: job.pool.pool_tiles as f32,
        dispatch_count: elements,
        _pad0: 0,
    };
    encoder.dispatch_compute(
        pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: job.bodies,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: job.shapes,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: job.atlas,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: job.tiles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: job.source,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: job.distance,
                offset: 0,
            },
        ],
        [elements.div_ceil(256), 1, 1],
        label,
    );
}

#[cfg(test)]
mod tests {
    use super::WhitewaterSource;
    use manifold_node_engine::ports::{
        ArrayType, ChannelName, KnownItem, MatchMode, std430_channel,
    };
    use std::mem::{offset_of, size_of};

    /// The wire record's channels sit where its repr(C) fields do.
    #[test]
    fn source_record_matches_its_channels() {
        let specs = WhitewaterSource::SPECS;
        let at = |name| {
            std430_channel(specs, ChannelName::from_str(name)).map(|(offset, _)| offset as usize)
        };
        assert_eq!(
            at("influence"),
            Some(offset_of!(WhitewaterSource, influence))
        );
        assert_eq!(
            at("dust_strength"),
            Some(offset_of!(WhitewaterSource, dust_strength))
        );
        assert_eq!(at("kind"), Some(offset_of!(WhitewaterSource, kind)));
        assert_eq!(at("pad0"), Some(offset_of!(WhitewaterSource, pad)));
        assert_eq!(
            ArrayType::of_channels(specs, MatchMode::Exact).item_size as usize,
            size_of::<WhitewaterSource>()
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
