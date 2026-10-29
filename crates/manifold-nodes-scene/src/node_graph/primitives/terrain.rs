//! `node.terrain` — a square grid of `MeshVertex` items across `size` metres
//! in the XZ plane, lifted by eroded-looking noise each frame.
//!
//! The height is value-noise fBm whose octaves fade where the land is already
//! steep (Inigo Quilez's "Elevated" terrain), so ridges stay sharp and valleys
//! smooth out without simulating erosion. The land is fixed to the object, so
//! Size grows or shrinks the patch shown without moving the hills, and octaves
//! finer than the grid fade out instead of aliasing. Feed it to
//! `node.make_triangles`, which works out the normals.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

/// Generated-codegen uniform layout: scalar params in PARAMS order (Int →
/// i32), then the codegen-injected `dispatch_count`, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TerrainUniforms {
    resolution: i32,
    size: f32,
    height: f32,
    feature_size: f32,
    roughness: f32,
    weathering: f32,
    seed: i32,
    dispatch_count: u32,
}

const DEFAULT_RESOLUTION: u32 = 512;

fn resolution(params: &manifold_node_engine::exec::effect_node::ParamValues) -> u32 {
    match params.get("resolution") {
        Some(ParamValue::Float(n)) => n.round().clamp(2.0, 4096.0) as u32,
        _ => DEFAULT_RESOLUTION,
    }
}

manifold_node_engine::primitive! {
    name: Terrain,
    type_id: "node.terrain",
    purpose: "Emit a resolution × resolution grid of MeshVertex items across size metres in the XZ plane, lifted by eroded-looking value-noise fBm (octaves fade where the land is already steep). Pair with node.make_triangles (src_cols = src_rows = resolution) for drawable triangles with normals.",
    inputs: {},
    outputs: {
        vertices: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("resolution"),
            label: "Resolution",
            ty: ParamType::Int,
            default: ParamValue::Float(512.0),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("size"),
            label: "Size",
            ty: ParamType::Float,
            default: ParamValue::Float(600.0),
            range: Some((1.0, 20_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("height"),
            label: "Height",
            ty: ParamType::Float,
            default: ParamValue::Float(120.0),
            range: Some((0.0, 5000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("feature_size"),
            label: "Feature Size",
            ty: ParamType::Float,
            default: ParamValue::Float(250.0),
            range: Some((1.0, 10_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("roughness"),
            label: "Roughness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 0.95)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("weathering"),
            label: "Weathering",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("seed"),
            label: "Seed",
            ty: ParamType::Int,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 999.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "size is the grid's side in metres; the land is fixed to the object, so Size reveals more or less of the same hills. feature_size is the widest hill's wavelength. roughness is each octave's amplitude ratio. weathering 0 is plain fBm; higher flattens detail on steep ground for an eroded look. Height scales the result, which spans about ±height.",
    examples: [],
    picker: { label: "Terrain", category: Atom },
    summary: "Builds a landscape of hills and mountains from noise that looks eroded, as a grid of points ready to triangulate.",
    category: Geometry3D,
    role: Source,
    aliases: ["terrain", "landscape", "mountains", "heightfield"],
    pure: true,
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/terrain_body.wgsl"),
}

impl Primitive for Terrain {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &manifold_node_engine::exec::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "vertices").then(|| {
            let res = resolution(params);
            res * res
        })
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let res = resolution(ctx.params);
        let seed = match ctx.params.get("seed") {
            Some(ParamValue::Float(n)) => n.round() as i32,
            _ => 1,
        };
        let Some(out_buf) = ctx.outputs.array("vertices") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let capacity = (out_buf.size / vertex_size) as u32;
        if capacity == 0 {
            return;
        }

        let uniforms = TerrainUniforms {
            resolution: res as i32,
            size: ctx.param_f32("size", 600.0),
            height: ctx.param_f32("height", 120.0),
            feature_size: ctx.param_f32("feature_size", 250.0),
            roughness: ctx.param_f32("roughness", 0.5),
            weathering: ctx.param_f32("weathering", 1.0),
            seed,
            dispatch_count: capacity,
        };

        let gpu = ctx.gpu_encoder();
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
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.terrain",
        );
    }
}
