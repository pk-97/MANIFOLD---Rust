//! Nearest solid-object metadata for FLIP whitewater influence and dust emission.
//! Ported from FLIP Fluids (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid::TICK;
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};
use crate::node_graph::liquid::lattice::{LiquidLattice, PADDING_NODES};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Nearest solid-object properties at a lattice node. Kind: none 0, domain 1, obstacle 2.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WhitewaterSource {
    pub influence: f32,
    pub dust_strength: f32,
    pub kind: u32,
    pub pad: u32,
}
impl crate::node_graph::ports::KnownItem for WhitewaterSource {
    const SPECS: &'static [crate::node_graph::ports::ChannelSpec] = &[
        crate::node_graph::ports::ChannelSpec {
            name: crate::node_graph::ports::ChannelName::from_str("influence"),
            ty: crate::node_graph::ports::ChannelElementType::F32,
        },
        crate::node_graph::ports::ChannelSpec {
            name: crate::node_graph::ports::ChannelName::from_str("dust_strength"),
            ty: crate::node_graph::ports::ChannelElementType::F32,
        },
        crate::node_graph::ports::ChannelSpec {
            name: crate::node_graph::ports::ChannelName::from_str("kind"),
            ty: crate::node_graph::ports::ChannelElementType::U32,
        },
        crate::node_graph::ports::ChannelSpec {
            name: crate::node_graph::ports::ChannelName::from_str("pad0"),
            ty: crate::node_graph::ports::ChannelElementType::U32,
        },
    ];
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ObstacleSourceUniforms {
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    closed_faces: i32,
    wall_inset: f32,
    body_count: i32,
    rows: i32,
    tick_seconds: f32,
    influence: f32,
    dust_strength: f32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: WhitewaterObstacleSource,
    type_id: "node.whitewater_obstacle_source",
    purpose: "Find the nearest closed domain wall or enabled obstacle at every liquid lattice node, using the same posed solid distances as liquid_solid_distance. Emit its whitewater influence, dust strength and kind (none 0, domain 1, obstacle 2). A zero dust strength disables that obstacle source.",
    inputs: {
        bodies: Array(LiquidBody) required,
        shapes: Array(LiquidShape) required,
        atlas: Array(u32) required,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        closed_faces: ScalarF32 optional,
        wall_inset: ScalarF32 optional,
        body_count: ScalarF32 optional,
        rows: ScalarF32 optional,
        tick_seconds: ScalarF32 optional,
        influence: ScalarF32 optional, dust_strength: ScalarF32 optional,
    },
    outputs: {
        solid: Array(WhitewaterSource),
    },
    params: [
        ParamDef { name: Cow::Borrowed("lattice_min_x"), label: "Lattice Min X", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_y"), label: "Lattice Min Y", ty: ParamType::Float, default: ParamValue::Float(-0.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("lattice_min_z"), label: "Lattice Min Z", ty: ParamType::Float, default: ParamValue::Float(-2.1875), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_x"), label: "Nodes X", ty: ParamType::Float, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_y"), label: "Nodes Y", ty: ParamType::Float, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_z"), label: "Nodes Z", ty: ParamType::Float, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_faces"), label: "Closed Faces (bits −X +X −Y +Y −Z +Z)", ty: ParamType::Int, default: ParamValue::Float(63.0), range: Some((0.0, 63.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("wall_inset"), label: "Wall Inset (nodes)", ty: ParamType::Float, default: ParamValue::Float(PADDING_NODES as f32), range: Some((0.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("body_count"), label: "Bodies", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, MAX_FLUID_ROLES as f32)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("rows"), label: "Rows", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16_777_216.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("tick_seconds"), label: "Tick (s)", ty: ParamType::Float, default: ParamValue::Float(TICK as f32), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("influence"), label: "Obstacle Whitewater Influence", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("dust_strength"), label: "Obstacle Dust Strength", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 100.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire the same body, shape, atlas and lattice inputs as liquid_solid_distance; solid feeds whitewater_step.obstacle_source. Domain influence is resolved from the whitewater base level; obstacle controls default to FLIP MeshObject defaults. A custom nearest-object metadata producer may replace this node for per-object properties.",
    examples: ["WaterDamBreakMatter"],
    picker: { label: "Whitewater Obstacle Source", category: Atom },
    summary: "Supplies obstacle properties for dust emission and whitewater influence.",
    category: Particles3D,
    role: Filter,
    aliases: ["dust obstacle source", "whitewater influence source"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/whitewater_obstacle_source_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather],
    output_capacity: crate::node_graph::freeze::classify::FusedOutputCapacity::ParamProduct { params: &["nodes_x", "nodes_y", "nodes_z"], plus: 0 },
    wgsl_includes: [LIQUID_POSE, LIQUID_COLLIDER],
    extra_fields: {
        solid: Option<GpuBuffer> = None,
    },
}

impl Primitive for WhitewaterObstacleSource {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "solid"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "solid").then_some(self.solid.as_ref()).flatten()
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "solid").then(|| {
            ["nodes_x", "nodes_y", "nodes_z"]
                .iter()
                .map(|name| {
                    params
                        .get(*name)
                        .and_then(ParamValue::as_scalar)
                        .unwrap_or(71.0)
                        .round()
                        .max(0.0) as u32
                })
                .fold(1u32, u32::saturating_mul)
        })
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(lattice) = LiquidLattice::from_wires(ctx, "Whitewater Obstacle Source") else {
            return;
        };
        let closed_faces = ctx
            .scalar_or_param("closed_faces", 63.0)
            .round()
            .clamp(0.0, 63.0) as i32;
        let wall_inset = ctx
            .scalar_or_param("wall_inset", PADDING_NODES as f32)
            .clamp(0.0, 64.0);
        let body_count = ctx
            .scalar_or_param("body_count", 0.0)
            .round()
            .clamp(0.0, MAX_FLUID_ROLES as f32) as i32;
        let rows = ctx.scalar_or_param("rows", 0.0).round().max(0.0) as i32;
        let tick_seconds = ctx.scalar_or_param("tick_seconds", TICK as f32);
        let inputs = (
            ctx.inputs.array("bodies"),
            ctx.inputs.array("shapes"),
            ctx.inputs.array("atlas"),
        );
        let nodes = lattice.node_count();
        // The storage follows the node count the dispatch covers, before it
        // is encoded.
        let bytes = lattice.solid_bytes() * 4;
        if self.solid.as_ref().is_none_or(|solid| solid.size != bytes) {
            let device = ctx.gpu_encoder().device;
            let created = crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
                device.modifier_memory_snapshot(),
                bytes,
            )
            .map_err(|error| error.to_string())
            .and_then(|()| device.try_create_buffer_shared(bytes));
            match created {
                Ok(buffer) => {
                    buffer.zero_fill();
                    self.solid = Some(buffer);
                }
                Err(error) => {
                    ctx.error(format!(
                        "Whitewater Obstacle Source: the lattice's {nodes} nodes need {bytes} bytes the device cannot give: {error}. Lower Resolution."
                    ));
                    return;
                }
            }
        }
        let influence = ctx.scalar_or_param("influence", 1.0);
        let dust_strength = ctx.scalar_or_param("dust_strength", 1.0);
        let gpu = ctx.gpu_encoder();
        let ((Some(bodies), Some(shapes), Some(atlas)), Some(solid)) =
            (inputs, self.solid.as_ref())
        else {
            return;
        };
        let job = ObstacleSourceJob {
            influence,
            dust_strength,
            min: lattice.min(),
            cell_size: lattice.cell_size(),
            nodes: lattice.nodes(),
            closed_faces,
            wall_inset,
            body_count,
            rows,
            tick_seconds,
            bodies,
            shapes,
            atlas,
            out: solid,
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

/// Nearest-object metadata lattice: `nodes` lattice nodes from `min`, `cell_size` apart.
pub(crate) struct ObstacleSourceJob<'a> {
    pub influence: f32,
    pub dust_strength: f32,
    pub min: [f32; 3],
    pub cell_size: f32,
    pub nodes: [u32; 3],
    /// Box walls that count as solid (bits −X +X −Y +Y −Z +Z), `wall_inset` nodes in from the lattice edge.
    pub closed_faces: i32,
    pub wall_inset: f32,
    pub body_count: i32,
    pub rows: i32,
    pub tick_seconds: f32,
    pub bodies: &'a GpuBuffer,
    pub shapes: &'a GpuBuffer,
    pub atlas: &'a GpuBuffer,
    /// At least one 16-byte WhitewaterSource per node.
    pub out: &'a GpuBuffer,
}

/// The node's kernel, for a stage that writes a solid lattice inside its own
/// dispatch chain (`node.gpu_flip_step`). `slot` holds the codegen pipeline.
pub(crate) fn encode_obstacle_source(
    slot: &mut Option<GpuComputePipeline>,
    device: &GpuDevice,
    encoder: &mut GpuEncoder,
    job: &ObstacleSourceJob<'_>,
    label: &str,
) {
    let nodes = job.nodes.iter().map(|&n| u64::from(n)).product::<u64>() as u32;
    let rows = job.rows.min(
        (job.bodies.size / std::mem::size_of::<LiquidBody>() as u64).min(i32::MAX as u64) as i32,
    );
    let pipeline = standalone_pipeline::<WhitewaterObstacleSource>(slot, device);
    let uniforms = ObstacleSourceUniforms {
        lattice_min_x: job.min[0],
        lattice_min_y: job.min[1],
        lattice_min_z: job.min[2],
        cell_size: job.cell_size,
        nodes_x: job.nodes[0] as f32,
        nodes_y: job.nodes[1] as f32,
        nodes_z: job.nodes[2] as f32,
        closed_faces: job.closed_faces,
        wall_inset: job.wall_inset,
        body_count: job.body_count,
        rows,
        tick_seconds: job.tick_seconds,
        influence: job.influence,
        dust_strength: job.dust_strength,
        dispatch_count: nodes,
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
                buffer: job.out,
                offset: 0,
            },
        ],
        [nodes.div_ceil(256), 1, 1],
        label,
    );
}

#[cfg(test)]
mod tests {
    use super::WhitewaterSource;
    use crate::node_graph::ports::{ArrayType, ChannelName, KnownItem, MatchMode, std430_channel};
    use std::mem::{offset_of, size_of};

    /// The wire record's channels sit where its repr(C) fields do.
    #[test]
    fn source_record_matches_its_channels() {
        let specs = WhitewaterSource::SPECS;
        let at = |name| {
            std430_channel(specs, ChannelName::from_str(name)).map(|(offset, _)| offset as usize)
        };
        assert_eq!(at("influence"), Some(offset_of!(WhitewaterSource, influence)));
        assert_eq!(at("dust_strength"), Some(offset_of!(WhitewaterSource, dust_strength)));
        assert_eq!(at("kind"), Some(offset_of!(WhitewaterSource, kind)));
        assert_eq!(at("pad0"), Some(offset_of!(WhitewaterSource, pad)));
        assert_eq!(
            ArrayType::of_channels(specs, MatchMode::Exact).item_size as usize,
            size_of::<WhitewaterSource>()
        );
    }
}
