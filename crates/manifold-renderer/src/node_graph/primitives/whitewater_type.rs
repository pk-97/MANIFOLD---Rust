//! `node.whitewater_type` — spray, foam or bubble for each new whitewater
//! particle, by FLIP's own rule (`docs/GPU_WHITEWATER_DESIGN.md` section
//! 3.3). A per-element atom on the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_fluids::WhitewaterSpawn;
use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{WHITEWATER_COMMON, cell_total, grid_cells, particle_grid};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`,
/// padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TypeUniforms {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: WhitewaterType,
    type_id: "node.whitewater_type",
    purpose: "Sort each new whitewater particle into bubble (0), foam (1) or spray (2) as FLIP types a fresh particle: spray outside FLIP's boundary box, 1.625 cells inside the whitewater grid; else foam within one cell of the liquid surface (the distance read trilinearly at cell centres), bubble deeper, spray higher; and foam or spray whose cell has no air cell among its 26 neighbours becomes bubble. Slots with lifetime 0 pass whole.",
    inputs: {
        spawns: Array(WhitewaterSpawn) required,
        distance: Array(f32) required,
        cells: Array(u32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(WhitewaterSpawn),
    },
    params: [
        float_param!("center_x", "Grid Center X", 0.0, -1.0e4, 1.0e4),
        float_param!("center_y", "Grid Center Y", 0.0, -1.0e4, 1.0e4),
        float_param!("center_z", "Grid Center Z", 0.0, -1.0e4, 1.0e4),
        float_param!("size_x", "Grid Size X", 4.375, 0.0001, 1.0e4),
        float_param!("size_y", "Grid Size Y", 4.375, 0.0001, 1.0e4),
        float_param!("size_z", "Grid Size Z", 4.375, 0.0001, 1.0e4),
        float_param!("nodes_x", "Solid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 71.0, 3.0, 4096.0),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.spawn_whitewater, before node.whitewater_lifecycle's spawns. distance from node.crossing_distance and cells from node.liquid_cells, the grid as node.spawn_whitewater reads it. The lifecycle retypes every particle by the same rule each tick, so FLIP's foam depth and boundary box are fixed here, not params.",
    examples: [],
    picker: { label: "Whitewater Type", category: Atom },
    summary: "Decides whether each new whitewater particle is spray, foam or a bubble, from where it sits against the water surface.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater type", "foam spray bubble", "diffuse particle type"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/whitewater_type_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "spawns" },
    wgsl_includes: [WHITEWATER_COMMON],
}

impl Primitive for WhitewaterType {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "spawns").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let grid = match particle_grid(ctx) {
            Ok(grid) => grid,
            Err(refusal) => {
                ctx.error(format!("Whitewater Type: {refusal}"));
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(spawns), Some(distance), Some(cells), Some(out)) =
            (ctx.inputs.array("spawns"), ctx.inputs.array("distance"), ctx.inputs.array("cells"), ctx.outputs.array("out"))
        else {
            return;
        };
        let total = grid_cells(grid.nodes).map_or(0, cell_total);
        if total * 4 > distance.size.min(cells.size) {
            ctx.error(format!("Whitewater Type: a {:?}-node grid is larger than its distance or cells", grid.nodes));
            return;
        }
        let record = std::mem::size_of::<WhitewaterSpawn>() as u64;
        let count = (spawns.size.min(out.size) / record) as u32;
        if count == 0 {
            return;
        }
        let [center_x, center_y, center_z] = grid.center;
        let [size_x, size_y, size_z] = grid.size;
        let [nodes_x, nodes_y, nodes_z] = grid.nodes.map(|n| n as f32);
        let uniforms = TypeUniforms {
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            nodes_x,
            nodes_y,
            nodes_z,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: spawns, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: distance, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: cells, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.whitewater_type",
        );
    }
}
