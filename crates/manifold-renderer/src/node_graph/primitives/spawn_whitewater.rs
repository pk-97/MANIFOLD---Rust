//! `node.spawn_whitewater` — one new whitewater particle per spawn slot,
//! placed by FLIP's own emitter rule around the liquid particle that emits
//! it (`docs/GPU_WHITEWATER_DESIGN.md` D8, section 3.3). A per-element atom
//! on the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_fluids::WhitewaterSpawn;
use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use super::whitewater_lifecycle::{DEFAULT_CAPACITY, MAX_CAPACITY};
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::liquid::grid::{LIQUID_FACES, face_len};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{WHITEWATER_COMMON, cell_total, face_offset, particle_grid};

/// FLIP's lifetime rule (`_minDiffuseParticleLifetime`,
/// `_maxDiffuseParticleLifetime`, `_lifetimeVariance`), seconds.
pub(crate) const MIN_LIFETIME: f32 = 0.0;
pub(crate) const MAX_LIFETIME: f32 = 7.0;
pub(crate) const LIFETIME_VARIANCE: f32 = 3.0;

/// More emitter slots than any particle array holds: unwired, the whole
/// offsets array counts.
const ALL_EMITTERS: f32 = 16_777_216.0;

const CAPACITY_PARAMS: [&str; 1] = ["capacity"];

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpawnUniforms {
    capacity: f32,
    emitters: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    seed: f32,
    epoch: f32,
    min_lifetime: f32,
    max_lifetime: f32,
    lifetime_variance: f32,
    dt: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

const FACE_PORTS: [&str; 3] = ["face_u", "face_v", "face_w"];

crate::primitive! {
    name: SpawnWhitewater,
    type_id: "node.spawn_whitewater",
    purpose: "Place this frame's new whitewater particles, one per spawn slot, by FLIP's emitter rule. The running total's last entry is the frame's emission count; slot j takes emission j, or past Capacity an even subset (emission ⌊j · total / Capacity⌋). Its emitter is the liquid particle whose running total first passes it. The new particle lands at random in a cylinder about that particle's velocity, 8 of FLIP's marker radii wide and Duration seconds of its travel long. It is dropped outside the whitewater grid or within a quarter cell of a solid; its lifetime is Min Lifetime + energy × (Max − Min) ± Lifetime Variance, dropped at or below 0; its velocity is the liquid's at its position. Dropped and unused slots have lifetime 0. Kind is left 0 for node.whitewater_type.",
    inputs: {
        offsets: Array(u32) required,
        particles: Array(FluidParticle) required,
        energy: Array(f32) required,
        face_u: Array(f32) required,
        face_v: Array(f32) required,
        face_w: Array(f32) required,
        solid: Array(f32) required,
        emitters: ScalarF32 optional,
        face_cells_x: ScalarF32 optional, face_cells_y: ScalarF32 optional, face_cells_z: ScalarF32 optional,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        seed: ScalarF32 optional,
        epoch: ScalarF32 optional,
        dt: ScalarF32 optional,
    },
    outputs: {
        out: Array(WhitewaterSpawn),
    },
    params: [
        float_param!("capacity", "Capacity", DEFAULT_CAPACITY as f32, 1.0, MAX_CAPACITY as f32),
        float_param!("emitters", "Emitter Slots", ALL_EMITTERS, 0.0, ALL_EMITTERS),
        float_param!("face_cells_x", "Face Cells X", 64.0, 1.0, 4096.0),
        float_param!("face_cells_y", "Face Cells Y", 64.0, 1.0, 4096.0),
        float_param!("face_cells_z", "Face Cells Z", 64.0, 1.0, 4096.0),
        float_param!("center_x", "Grid Center X", 0.0, -1.0e4, 1.0e4),
        float_param!("center_y", "Grid Center Y", 0.0, -1.0e4, 1.0e4),
        float_param!("center_z", "Grid Center Z", 0.0, -1.0e4, 1.0e4),
        float_param!("size_x", "Grid Size X", 4.375, 0.0001, 1.0e4),
        float_param!("size_y", "Grid Size Y", 4.375, 0.0001, 1.0e4),
        float_param!("size_z", "Grid Size Z", 4.375, 0.0001, 1.0e4),
        float_param!("nodes_x", "Solid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 71.0, 3.0, 4096.0),
        float_param!("seed", "Seed", 0.0, -1.0e9, 1.0e9),
        float_param!("epoch", "Epoch", 0.0, 0.0, 1.0e9),
        float_param!("min_lifetime", "Min Lifetime", MIN_LIFETIME, 0.0, 1.0e3),
        float_param!("max_lifetime", "Max Lifetime", MAX_LIFETIME, 0.0, 1.0e3),
        float_param!("lifetime_variance", "Lifetime Variance", LIFETIME_VARIANCE, 0.0, 1.0e3),
        float_param!("dt", "Duration (s)", 1.0 / 60.0, 0.0, 1.0e3),
    ],
    depth_rule: Terminal,
    composition_notes: "offsets from node.running_total over node.emission_count's counts, emitters the same count the running total ran over (the particle frame's count). particles from node.sample_faces_at_particles and energy from node.energy_potential, so each emitter is read as it was scored. face_u/v/w, face_cells_x/y/z, solid and the grid (center/size from node.transform_components on grid_bounds, nodes_x/y/z from grid_nodes_x/y/z) as the rest of the whitewater chain reads them; seed and epoch as node.jitter_particles takes them. Capacity is the lifecycle's. Feed node.whitewater_type, then node.whitewater_lifecycle's spawns.",
    examples: [],
    picker: { label: "Spawn Whitewater", category: Atom },
    summary: "Places the new foam, spray and bubble particles around the breaking water that throws them off.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater spawn", "emit whitewater", "diffuse particle spawn"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/spawn_whitewater_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::ParamProduct { params: &CAPACITY_PARAMS, plus: 0 },
    wgsl_includes: [WHITEWATER_COMMON, LIQUID_FACES],
}

/// Spawn slots for a param set: the Capacity param as the fused kernel
/// counts it.
fn spawn_slots(params: &ParamValues) -> u32 {
    match params.get("capacity") {
        Some(ParamValue::Float(v)) => v.round().max(0.0) as u32,
        _ => DEFAULT_CAPACITY,
    }
}

impl Primitive for SpawnWhitewater {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| spawn_slots(params))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let face_cells = ["face_cells_x", "face_cells_y", "face_cells_z"].map(|name| ctx.scalar_or_param(name, 64.0).round().max(0.0) as u32);
        let placed = particle_grid(ctx).and_then(|grid| face_offset(grid.nodes, face_cells).map(|_| grid));
        let grid = match placed {
            Ok(grid) => grid,
            Err(refusal) => {
                ctx.error(format!("Spawn Whitewater: {refusal}"));
                return;
            }
        };
        let slots = spawn_slots(ctx.params);
        let emitters = ctx.scalar_or_param("emitters", ALL_EMITTERS);
        let seed = ctx.scalar_or_param("seed", 0.0);
        let epoch = ctx.scalar_or_param("epoch", 0.0);
        let min_lifetime = ctx.param_f32("min_lifetime", MIN_LIFETIME);
        let max_lifetime = ctx.param_f32("max_lifetime", MAX_LIFETIME);
        let lifetime_variance = ctx.param_f32("lifetime_variance", LIFETIME_VARIANCE);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(offsets), Some(particles), Some(energy), Some(solid), Some(out)) = (
            ctx.inputs.array("offsets"),
            ctx.inputs.array("particles"),
            ctx.inputs.array("energy"),
            ctx.inputs.array("solid"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let [Some(u), Some(v), Some(w)] = FACE_PORTS.map(|port| ctx.inputs.array(port)) else { return };
        for (axis, buffer) in [u, v, w].into_iter().enumerate() {
            if buffer.size < face_len(face_cells, axis) * 4 {
                ctx.error(format!(
                    "Spawn Whitewater: {} holds fewer than the {face_cells:?}-cell grid's {} faces",
                    FACE_PORTS[axis],
                    face_len(face_cells, axis)
                ));
                return;
            }
        }
        if cell_total(grid.nodes) * 4 > solid.size {
            ctx.error(format!("Spawn Whitewater: solid holds fewer than the {:?}-node lattice", grid.nodes));
            return;
        }
        let count = u64::from(slots).min(out.size / std::mem::size_of::<WhitewaterSpawn>() as u64) as u32;
        if count == 0 || offsets.size == 0 || particles.size == 0 || energy.size == 0 {
            return;
        }
        let [face_cells_x, face_cells_y, face_cells_z] = face_cells.map(|n| n as f32);
        let [center_x, center_y, center_z] = grid.center;
        let [size_x, size_y, size_z] = grid.size;
        let [nodes_x, nodes_y, nodes_z] = grid.nodes.map(|n| n as f32);
        let uniforms = SpawnUniforms {
            capacity: slots as f32,
            emitters,
            face_cells_x,
            face_cells_y,
            face_cells_z,
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            nodes_x,
            nodes_y,
            nodes_z,
            seed,
            epoch,
            min_lifetime,
            max_lifetime,
            lifetime_variance,
            dt: ctx.scalar_or_param("dt", 1.0 / 60.0),
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
                GpuBinding::Buffer { binding: 1, buffer: offsets, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: particles, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: energy, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: u, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: v, offset: 0 },
                GpuBinding::Buffer { binding: 6, buffer: w, offset: 0 },
                GpuBinding::Buffer { binding: 7, buffer: solid, offset: 0 },
                GpuBinding::Buffer { binding: 8, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.spawn_whitewater",
        );
    }
}
