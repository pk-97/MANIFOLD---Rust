//! `node.retype_whitewater` — FLIP's per-tick retype of every live
//! whitewater particle (`docs/GPU_WHITEWATER_DESIGN.md` section 3.9, phase
//! L2). A per-element atom on the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_water_liquid::float_param;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::freeze::classify::FusedOutputCapacity;
use manifold_water_liquid::grid::{LIQUID_FACES, face_len};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_water_liquid::whitewater::{WHITEWATER_COMMON, WhitewaterParticle, cell_total, face_offset, grid_cells, particle_grid};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`,
/// padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RetypeUniforms {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

const FACE_PORTS: [&str; 3] = ["face_u", "face_v", "face_w"];

manifold_node_engine::primitive! {
    name: RetypeWhitewater,
    type_id: "node.retype_whitewater",
    purpose: "Retypes each live whitewater particle after it moves, by FLIP's rule: spray outside FLIP's boundary box, 1.625 cells inside the whitewater grid; else foam within one cell of the liquid surface (the distance read trilinearly at cell centres), bubble deeper, spray higher; foam that would turn bubble stays foam until it is a further cell deep; foam or spray whose cell has no air cell among its 26 neighbours becomes bubble. A bubble that turns foam or spray takes the liquid velocity at its position. Empty slots (kind 3) pass whole.",
    inputs: {
        pool: Array(WhitewaterParticle) required,
        distance: Array(f32) required,
        cells: Array(u32) required,
        face_u: Array(f32) required,
        face_v: Array(f32) required,
        face_w: Array(f32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        face_cells_x: ScalarF32 optional, face_cells_y: ScalarF32 optional, face_cells_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(WhitewaterParticle),
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
        float_param!("face_cells_x", "Face Cells X", 64.0, 1.0, 4096.0),
        float_param!("face_cells_y", "Face Cells Y", 64.0, 1.0, 4096.0),
        float_param!("face_cells_z", "Face Cells Z", 64.0, 1.0, 4096.0),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.advect_whitewater in the GPU whitewater tick, before node.age_whitewater. distance from node.crossing_distance and cells from node.liquid_cells, the grid and face grid as node.advect_whitewater reads them. Uses the same rule as node.whitewater_type, plus FLIP's foam buffer, so new and old particles agree.",
    examples: [],
    summary: "Re-decides whether each whitewater particle is now spray, foam or a bubble after it has moved.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater retype", "foam to bubble", "diffuse particle types"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/retype_whitewater_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "pool" },
    wgsl_includes: [WHITEWATER_COMMON, LIQUID_FACES],
}

impl Primitive for RetypeWhitewater {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "pool").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let face_cells = ["face_cells_x", "face_cells_y", "face_cells_z"].map(|name| ctx.scalar_or_param(name, 64.0).round().max(0.0) as u32);
        let placed = particle_grid(ctx).and_then(|grid| face_offset(grid.nodes, face_cells).map(|_| grid));
        let grid = match placed {
            Ok(grid) => grid,
            Err(refusal) => {
                ctx.error(format!("Retype Whitewater: {refusal}"));
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(pool), Some(distance), Some(cells), Some(out)) =
            (ctx.inputs.array("pool"), ctx.inputs.array("distance"), ctx.inputs.array("cells"), ctx.outputs.array("out"))
        else {
            return;
        };
        let [Some(u), Some(v), Some(w)] = FACE_PORTS.map(|port| ctx.inputs.array(port)) else { return };
        for (axis, buffer) in [u, v, w].into_iter().enumerate() {
            if buffer.size < face_len(face_cells, axis) * 4 {
                ctx.error(format!(
                    "Retype Whitewater: {} holds fewer than the {face_cells:?}-cell grid's {} faces",
                    FACE_PORTS[axis],
                    face_len(face_cells, axis)
                ));
                return;
            }
        }
        let total = grid_cells(grid.nodes).map_or(0, cell_total);
        if total * 4 > distance.size.min(cells.size) {
            ctx.error(format!("Retype Whitewater: a {:?}-node grid is larger than its distance or cells", grid.nodes));
            return;
        }
        let count = (pool.size.min(out.size) / std::mem::size_of::<WhitewaterParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let [center_x, center_y, center_z] = grid.center;
        let [size_x, size_y, size_z] = grid.size;
        let [nodes_x, nodes_y, nodes_z] = grid.nodes.map(|n| n as f32);
        let [face_cells_x, face_cells_y, face_cells_z] = face_cells.map(|n| n as f32);
        let uniforms = RetypeUniforms {
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            nodes_x,
            nodes_y,
            nodes_z,
            face_cells_x,
            face_cells_y,
            face_cells_z,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: pool, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: distance, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: cells, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: u, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: v, offset: 0 },
                GpuBinding::Buffer { binding: 6, buffer: w, offset: 0 },
                GpuBinding::Buffer { binding: 7, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.retype_whitewater",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
