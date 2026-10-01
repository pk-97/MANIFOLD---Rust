//! `node.advect_whitewater` — one FLIP tick of every live whitewater
//! particle by its type, with FLIP's collision march against the solid
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.9, phase L1). A per-element
//! atom on the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::liquid::grid::{LIQUID_FACES, face_len};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{WHITEWATER_COMMON, WhitewaterParticle, cell_total, face_offset, particle_grid};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`;
/// 24 words, already a multiple of 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AdvectUniforms {
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
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    dt: f32,
    foam_advection: f32,
    bubble_buoyancy: f32,
    bubble_drag: f32,
    spray_drag: f32,
    spray_drag_variance: f32,
    spray_restitution: f32,
    spray_friction: f32,
    dispatch_count: u32,
}

const _: () = assert!(std::mem::size_of::<AdvectUniforms>() == 96);

const FACE_PORTS: [&str; 3] = ["face_u", "face_v", "face_w"];

crate::primitive! {
    name: AdvectWhitewater,
    type_id: "node.advect_whitewater",
    purpose: "Moves each live whitewater particle one FLIP tick by its type: spray falls under gravity with per-id drag and bounces off solids (restitution on the normal part, friction on the tangent part); bubbles rise against gravity and drag toward the liquid velocity; foam rides the liquid velocity. The liquid velocity is FLIP's MAC trilinear of the face grid at the old position. Every type then marches its path in half-cell steps and stops a quarter cell clear of the solid or inside FLIP's boundary box, 1.625 cells in from the whitewater grid. A particle that ends up moving faster than 1.1 times its new speed, or whose travel is not finite, dies (lifetime -1e6). Empty slots (kind 3) pass whole.",
    inputs: {
        pool: Array(WhitewaterParticle) required,
        face_u: Array(f32) required,
        face_v: Array(f32) required,
        face_w: Array(f32) required,
        solid: Array(f32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        face_cells_x: ScalarF32 optional, face_cells_y: ScalarF32 optional, face_cells_z: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity_y: ScalarF32 optional, gravity_z: ScalarF32 optional,
        dt: ScalarF32 optional,
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
        float_param!("gravity_x", "Gravity X", 0.0, -1000.0, 1000.0),
        float_param!("gravity_y", "Gravity Y", -9.81, -1000.0, 1000.0),
        float_param!("gravity_z", "Gravity Z", 0.0, -1000.0, 1000.0),
        float_param!("dt", "Tick", 1.0 / 60.0, 0.0001, 1.0),
        float_param!("foam_advection", "Foam Advection", 1.0, 0.0, 1.0),
        float_param!("bubble_buoyancy", "Bubble Buoyancy", 4.0, 0.0, 100.0),
        float_param!("bubble_drag", "Bubble Drag", 1.0, 0.0, 1.0),
        float_param!("spray_drag", "Spray Drag", 0.0, 0.0, 100.0),
        float_param!("spray_drag_variance", "Spray Drag Variance", 0.25, 0.0, 1.0),
        float_param!("spray_restitution", "Spray Restitution", 0.2, 0.0, 1.0),
        float_param!("spray_friction", "Spray Friction", 0.0, 0.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The first step of the GPU whitewater tick, on the pool node.array_feedback carries; retyping, lifetimes and removal follow. face_u/v/w and face_cells_x/y/z from the liquid frame's face grid, solid its solid lattice, center/size from node.transform_components on its grid_bounds, nodes_x/y/z its grid_nodes_x/y/z. FLIP's ballistic and kill limit behaviours are not ported: every side collides.",
    examples: [],
    picker: { label: "Advect Whitewater", category: Atom },
    summary: "Moves foam, spray and bubbles one step: spray flies and bounces, bubbles rise, foam rides the water.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater advection", "move whitewater", "diffuse particle advection"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/advect_whitewater_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "pool" },
    wgsl_includes: [WHITEWATER_COMMON, LIQUID_FACES],
}

impl Primitive for AdvectWhitewater {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "pool").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let face_cells = ["face_cells_x", "face_cells_y", "face_cells_z"].map(|name| ctx.scalar_or_param(name, 64.0).round().max(0.0) as u32);
        let placed = particle_grid(ctx).and_then(|grid| face_offset(grid.nodes, face_cells).map(|_| grid));
        let grid = match placed {
            Ok(grid) => grid,
            Err(refusal) => {
                ctx.error(format!("Advect Whitewater: {refusal}"));
                return;
            }
        };
        let gravity = [("gravity_x", 0.0), ("gravity_y", -9.81), ("gravity_z", 0.0)].map(|(name, default)| ctx.scalar_or_param(name, default));
        let dt = ctx.scalar_or_param("dt", 1.0 / 60.0);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(pool), Some(solid), Some(out)) = (ctx.inputs.array("pool"), ctx.inputs.array("solid"), ctx.outputs.array("out")) else {
            return;
        };
        let [Some(u), Some(v), Some(w)] = FACE_PORTS.map(|port| ctx.inputs.array(port)) else { return };
        for (axis, buffer) in [u, v, w].into_iter().enumerate() {
            if buffer.size < face_len(face_cells, axis) * 4 {
                ctx.error(format!(
                    "Advect Whitewater: {} holds fewer than the {face_cells:?}-cell grid's {} faces",
                    FACE_PORTS[axis],
                    face_len(face_cells, axis)
                ));
                return;
            }
        }
        if cell_total(grid.nodes) * 4 > solid.size {
            ctx.error(format!("Advect Whitewater: solid holds fewer than the {:?}-node lattice", grid.nodes));
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
        let [gravity_x, gravity_y, gravity_z] = gravity;
        let uniforms = AdvectUniforms {
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
            gravity_x,
            gravity_y,
            gravity_z,
            dt,
            foam_advection: ctx.scalar_or_param("foam_advection", 1.0),
            bubble_buoyancy: ctx.scalar_or_param("bubble_buoyancy", 4.0),
            bubble_drag: ctx.scalar_or_param("bubble_drag", 1.0),
            spray_drag: ctx.scalar_or_param("spray_drag", 0.0),
            spray_drag_variance: ctx.scalar_or_param("spray_drag_variance", 0.25),
            spray_restitution: ctx.scalar_or_param("spray_restitution", 0.2),
            spray_friction: ctx.scalar_or_param("spray_friction", 0.0),
            dispatch_count: count,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: pool, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: u, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: v, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: w, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: solid, offset: 0 },
                GpuBinding::Buffer { binding: 6, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.advect_whitewater",
        );
    }
}
