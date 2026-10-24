//! `node.sphere_mesh` — a smooth UV sphere about the origin as a triangle
//! list: a fan at each pole and two triangles per quad between, `segments`
//! around and `rings` from pole to pole, outward wound, with normals out
//! from the centre and longitude-latitude UVs.
//!
//! The vertex for each index is closed-form, so the node is a codegen
//! Source atom like `node.plane_mesh`. Its `source` output is the same
//! sphere as `MeshSource::Sphere`, whose CPU mirror
//! (`mesh_source::sphere_vertex`) is what a collision shape reads.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::mesh_source::{MeshSource, sphere_vertex_count};

const DEFAULT_SEGMENTS: u32 = 32;
const DEFAULT_RINGS: u32 = 16;

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`segments` Int → i32, `rings` Int → i32, `radius` f32) then the
/// codegen-injected `dispatch_count` — one 16-byte uniform.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SphereUniforms {
    segments: i32,
    rings: i32,
    radius: f32,
    dispatch_count: u32,
}

manifold_node_engine::primitive! {
    name: SphereMesh,
    type_id: "node.sphere_mesh",
    purpose: "Emit a smooth UV sphere of `radius` about the origin as a triangle-list Array(MeshVertex): a fan at each pole and two triangles per quad between, `segments` around and `rings` from pole to pole, 6 * segments * (rings - 1) vertices, outward wound, normals out from the centre, UVs as longitude and latitude. The `source` output carries the same sphere for collision shapes.",
    inputs: {
        radius: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(MeshVertex),
        source: MeshSource,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("segments"),
            label: "Segments",
            ty: ParamType::Int,
            default: ParamValue::Float(DEFAULT_SEGMENTS as f32),
            range: Some((3.0, 256.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rings"),
            label: "Rings",
            ty: ParamType::Int,
            default: ParamValue::Float(DEFAULT_RINGS as f32),
            range: Some((2.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("radius"),
            label: "Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The round built-in solid beside node.cube_mesh and node.platonic_solid_mesh; the default radius 0.5 fills the unit box a unit cube fills. Wire `vertices` into node.scene_object and `source` into a rigid body or collider. Segments and Rings size the vertex buffer, so they are build-time params and not port-shadowed; radius is port-shadowed and can be driven live. UVs wrap once around (u) and pole to pole (v), with a duplicated seam column so a texture does not smear across it.",
    examples: [],
    picker: { label: "Sphere Mesh", category: Atom },
    summary: "Builds a smooth round ball of mesh, the sphere among the built-in solids.",
    category: Geometry3D,
    role: Source,
    aliases: ["sphere", "ball", "uv sphere", "globe", "Sphere SOP"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/sphere_mesh_body.wgsl"),
}

fn count_param(params: &ParamValues, name: &str, default: u32, min: u32) -> u32 {
    match params.get(name) {
        Some(ParamValue::Float(n)) if n.is_finite() => (n.round() as u32).max(min),
        _ => default,
    }
}

impl Primitive for SphereMesh {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "vertices").then(|| {
            sphere_vertex_count(
                count_param(params, "segments", DEFAULT_SEGMENTS, 3),
                count_param(params, "rings", DEFAULT_RINGS, 2),
            )
        })
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let segments = count_param(ctx.params, "segments", DEFAULT_SEGMENTS, 3);
        let rings = count_param(ctx.params, "rings", DEFAULT_RINGS, 2);
        let radius = ctx.scalar_or_param("radius", 0.5);
        ctx.outputs.set_mesh_source(
            "source",
            MeshSource::Sphere {
                radius,
                segments,
                rings,
            },
        );

        let Some(dst) = ctx.outputs.array("vertices") else {
            return;
        };
        let capacity = (dst.size / std::mem::size_of::<MeshVertex>() as u64) as u32;
        if capacity == 0 {
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = SphereUniforms {
            segments: segments as i32,
            rings: rings as i32,
            radius,
            dispatch_count: capacity,
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
                    buffer: dst,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.sphere_mesh",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! The generated standalone kernel against the CPU mirror the collision
    //! shape reads, vertex by vertex, plus the padding slots.
    use super::*;
    use manifold_node_engine::scene::mesh_source::sphere_vertex;

    #[test]
    fn generated_sphere_mesh_matches_the_cpu_sphere() {
        let (segments, rings, radius) = (9u32, 5u32, 0.75f32);
        let live = sphere_vertex_count(segments, rings);
        let capacity = live + 4;
        let uniforms = SphereUniforms {
            segments: segments as i32,
            rings: rings as i32,
            radius,
            dispatch_count: capacity,
        };
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<SphereMesh>()
            .expect("sphere_mesh buffer codegen");
        let device = manifold_gpu::testkit::test_device();
        let pipeline = device.create_compute_pipeline(&wgsl, "cs_main", "sphere-mesh-oracle");
        let out =
            device.create_buffer_shared(capacity as u64 * std::mem::size_of::<MeshVertex>() as u64);
        let mut enc = device.create_encoder("sphere-mesh-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &out,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "sphere-mesh-oracle",
        );
        enc.commit_and_wait_completed();
        let ptr = out.mapped_ptr().expect("shared out buffer");
        let got =
            unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, capacity as usize) };
        for (idx, vertex) in got.iter().enumerate().take(live as usize) {
            let want = sphere_vertex(idx as u32, segments, rings, radius);
            let close = |a: &[f32], b: &[f32]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1.0e-5);
            assert!(
                close(&vertex.position, &want.position),
                "vertex {idx} position {:?} vs {:?}",
                vertex.position,
                want.position
            );
            assert!(close(&vertex.normal, &want.normal), "vertex {idx} normal");
            assert!(close(&vertex.uv, &want.uv), "vertex {idx} uv");
            assert!(
                close(&vertex.tangent, &want.tangent),
                "vertex {idx} tangent"
            );
        }
        for (idx, vertex) in got.iter().enumerate().skip(live as usize) {
            assert_eq!(vertex.position, [0.0; 3], "padding {idx}");
            assert_eq!(vertex.normal, [0.0, 1.0, 0.0], "padding normal {idx}");
        }
    }
}
