//! `node.grid_mesh` — emit a regular NxM grid of
//! `MeshVertex` items laid out as a flat plane in XZ.
//!
//! Phase B of `BUFFER_PORT_PLAN`. First primitive in the mesh
//! family — zero inputs, one Array(MeshVertex) output. Params
//! drive grid resolution and world-space size; the chain build
//! pre-allocates `max_capacity` vertices and the runtime
//! initialises `resolution_x * resolution_y` of them per frame.
//!
//! Downstream pairing: feed into `node.render_mesh` for direct
//! rendering, or into a future `node.push_mesh` primitive
//! that perturbs Y by a Texture2D sample (the path that unlocks
//! MetallicGlass-style feedback-displacement on arbitrary
//! source textures).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::mesh::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::standalone_pipeline::standalone_pipeline;

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`max_capacity` Int → i32 [allocation-only, the shader ignores it but it
/// occupies a uniform word], `resolution_x`/`resolution_y` Int → i32,
/// `size_x`/`size_y` f32) then the codegen-injected `dispatch_count` (=
/// output capacity, the guard), padded to 16 bytes. `origin_x`/`origin_z`
/// are always 0.0 in the hand kernel and are not params, so they're not
/// threaded through the generated uniform. 8 words = 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GridUniforms {
    max_capacity: i32,
    resolution_x: i32,
    resolution_y: i32,
    size_x: f32,
    size_y: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: GenerateGridMesh,
    type_id: "node.grid_mesh",
    purpose: "Emit a regular NxM grid of MeshVertex items in the XZ plane, sized in world units. Pair with a displacement primitive that perturbs Y from a Texture2D, then route to node.render_mesh. The unlock for MetallicGlass-shaped graphs where the displacement source is wire-controlled.",
    inputs: {
        size_x: ScalarF32 optional,
        size_y: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(2_097_152.0),
            range: Some((1024.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("resolution_x"),
            label: "Resolution X",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("resolution_y"),
            label: "Resolution Y",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("size_x"),
            label: "Size X",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.01, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("size_y"),
            label: "Size Y",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.01, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "max_capacity ≥ resolution_x × resolution_y. The chain build pre-allocates max_capacity × 32 bytes and triggers a rebuild when changed; resolution sliders only write uniforms. Default 256×256 = 65k vertices ≈ 2 MB. size_x / size_y are port-shadows-param: aspect-correct the mesh by wiring `system.generator_input.aspect → math.multiply(b=2.0) → size_x` (matches the legacy MetallicGlass mesh that spans [-aspect, +aspect] in X).",
    examples: [],
    picker: { label: "Grid Mesh", category: Atom },
    summary: "Builds a flat grid of points as a 3D mesh, the base for terrain, cloth, and displacement looks. Pair it with Surface Bumps or Push Mesh.",
    category: Geometry3D,
    role: Source,
    aliases: ["grid mesh", "generate grid mesh", "plane", "terrain", "Grid SOP"],
    pure: true,
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/generate_grid_mesh_body.wgsl"),
}

impl Primitive for GenerateGridMesh {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let max_capacity = match ctx.params.get("max_capacity") {
            Some(ParamValue::Float(n)) => n.round() as i32,
            _ => 2_097_152,
        };
        let resolution_x = match ctx.params.get("resolution_x") {
            Some(ParamValue::Float(n)) => n.round().max(2_f32) as u32,
            _ => 256,
        };
        let resolution_y = match ctx.params.get("resolution_y") {
            Some(ParamValue::Float(n)) => n.round().max(2_f32) as u32,
            _ => 256,
        };
        let size_x = ctx.scalar_or_param("size_x", 2.0);
        let size_y = ctx.scalar_or_param("size_y", 2.0);

        let Some(out_buf) = ctx.outputs.array("vertices") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let capacity = (out_buf.size / vertex_size) as u32;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);

        let uniforms = GridUniforms {
            max_capacity,
            resolution_x: resolution_x as i32,
            resolution_y: resolution_y as i32,
            size_x,
            size_y,
            dispatch_count: capacity,
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
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.grid_mesh",
        );
    }
}

