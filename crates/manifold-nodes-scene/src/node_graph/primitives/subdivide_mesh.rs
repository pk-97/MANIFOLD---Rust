//! `node.subdivide_mesh` — split every triangle of an `Array<MeshVertex>`
//! flat triangle list into four by its edge midpoints, `level` times, so a
//! deformer has vertices to bend. The surface keeps its exact shape: every new
//! vertex lies on its source triangle and every attribute interpolates.
//!
//! One thread per output vertex. Output triangle `t` is child `t % 4^level`
//! of source triangle `t / 4^level`; the vertex is the barycentric blend of
//! that source triangle's three vertices (gather), so threads never race.
//! Output count is `floor(in / 3) × 3 × 4^level`; a trailing partial triangle
//! is dropped.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

/// The deepest split, 4^8 = 65,536 triangles per source triangle. GPU memory
/// sets it: at 8 a cube is 2.4 million vertices (189 MB) before any deformer
/// copies it, and each level more is four times that.
const MAX_LEVEL: u32 = 8;

/// Generated-codegen uniform layout: `level` (Int→i32), the derived `in_len`
/// (u32), then the codegen-injected `dispatch_count`, padded to 16 bytes.
/// Matches `standalone_for_spec::<SubdivideMesh>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SubdivideUniforms {
    level: i32,
    in_len: u32,
    dispatch_count: u32,
    _pad0: u32,
}

manifold_node_engine::primitive! {
    name: SubdivideMesh,
    type_id: "node.subdivide_mesh",
    purpose: "Split every triangle of an Array<MeshVertex> flat triangle list into four by its edge midpoints, `level` times (4^level triangles per source triangle). One thread PER OUTPUT VERTEX gathers its source triangle's 3 verts (base = 3*(t / 4^level)) and writes their barycentric blend: position, uv, uv1 and color interpolate; normal and tangent direction interpolate and renormalise. The shape is unchanged; only the vertex count grows. Output count = floor(in/3)*3*4^level.",
    inputs: {
        in: Array(MeshVertex) required,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("level"),
            label: "Level",
            ty: ParamType::Int,
            default: ParamValue::Float(1.0),
            range: Some((0.0, MAX_LEVEL as f32)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire between a low-poly mesh source (cube, plane, platonic solid) and a per-vertex deformer (bend, twist, wave, peel) so the deformer has vertices to move; a cube has only its 8 corners otherwise and bends as a rigid box. Each level multiplies the vertex count by 4, so pick the smallest level that gives the deformer a few thousand vertices. Flat triangle lists only (no index buffer). Buffer-gather form → a fusion boundary; it stands alone in the graph compiler.",
    examples: [],
    picker: { label: "Subdivide Mesh", category: Atom },
    summary: "Splits every triangle of a mesh into smaller ones without changing its shape, so a bend or a wave has enough points to curve smoothly.",
    category: Geometry3D,
    role: Filter,
    aliases: ["subdivide", "tessellate", "mesh detail", "split triangles"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/subdivide_mesh_body.wgsl"),
    input_access: [BufferGather],
    derived_uniforms: ["in_len:u32"],
}

fn level_of(params: &manifold_node_engine::exec::effect_node::ParamValues) -> u32 {
    match params.get("level") {
        Some(ParamValue::Float(n)) => (n.round().max(0.0) as u32).min(MAX_LEVEL),
        _ => 1,
    }
}

impl Primitive for SubdivideMesh {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        let input = input_capacities
            .iter()
            .find(|(p, _)| *p == "in")
            .map(|(_, n)| *n)?;
        Some((input / 3 * 3).saturating_mul(1 << (2 * level_of(params))))
    }

    /// The split is fixed by the source triangles and `level`, a setup value
    /// whose change rebuilds the graph: topology tracks the input topology
    /// and positions track the input positions.
    fn mesh_output_rule(
        &self,
        port: &str,
    ) -> manifold_node_engine::scene::mesh_change::MeshOutputRule<'_> {
        use manifold_node_engine::scene::mesh_change::{
            MeshAspect, MeshDependency, MeshOutputRule, MeshRevisionRule,
        };
        if port == "out" {
            return MeshOutputRule {
                topology: MeshRevisionRule::Dependencies(&[MeshDependency {
                    input: Cow::Borrowed("in"),
                    aspect: MeshAspect::Topology,
                }]),
                positions: MeshRevisionRule::Dependencies(&[MeshDependency {
                    input: Cow::Borrowed("in"),
                    aspect: MeshAspect::Positions,
                }]),
            };
        }
        MeshOutputRule {
            topology: MeshRevisionRule::Written,
            positions: MeshRevisionRule::Written,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let level = level_of(ctx.params);
        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let in_len = (src.size / vertex_size) as u32;
        let out_count = ((dst.size / vertex_size) as u32)
            .min((in_len / 3 * 3).saturating_mul(1 << (2 * level)));
        if out_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = SubdivideUniforms {
            level: level as i32,
            in_len,
            dispatch_count: out_count,
            _pad0: 0,
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
                    buffer: src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [out_count.div_ceil(256), 1, 1],
            "node.subdivide_mesh",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value test of the generated standalone kernel.
    use super::*;

    fn vertex(position: [f32; 3], uv: [f32; 2]) -> MeshVertex {
        MeshVertex {
            position,
            _pad0: 0.0,
            normal: [0.0, 0.0, 1.0],
            _pad1: 0.0,
            uv,
            _pad2: [0.0, 0.0],
            tangent: [1.0, 0.0, 0.0, 1.0],
            color: [1.0; 4],
        }
    }

    fn dispatch(src: &[MeshVertex], level: u32) -> Vec<MeshVertex> {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<SubdivideMesh>()
            .expect("subdivide_mesh buffer codegen");
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            manifold_node_engine::freeze::codegen::ENTRY,
            "subdivide-mesh-test",
        );
        let out_count = (src.len() as u32 / 3 * 3) << (2 * level);
        let sbuf = device.create_buffer_shared(std::mem::size_of_val(src) as u64);
        unsafe {
            sbuf.write(0, bytemuck::cast_slice(src));
        }
        let dbuf = device
            .create_buffer_shared(u64::from(out_count) * std::mem::size_of::<MeshVertex>() as u64);
        let uniforms = SubdivideUniforms {
            level: level as i32,
            in_len: src.len() as u32,
            dispatch_count: out_count,
            _pad0: 0,
        };
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &sbuf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &dbuf,
                offset: 0,
            },
        ];
        let mut enc = device.create_encoder("subdivide-mesh-test");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [out_count.div_ceil(256), 1, 1],
            "subdivide-mesh-test",
        );
        enc.commit_and_wait_completed();
        let ptr = dbuf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, out_count as usize) }.to_vec()
    }

    fn signed_area(t: &[MeshVertex]) -> f32 {
        let [a, b, c] = [t[0].position, t[1].position, t[2].position];
        ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])) * 0.5
    }

    /// Level 2 splits one triangle into 16 that tile it exactly: same winding,
    /// equal areas summing to the source's, every vertex on the source plane
    /// with uv following position.
    #[test]
    fn level_two_tiles_the_source_triangle() {
        let src = [
            vertex([0.0, 0.0, 0.0], [0.0, 0.0]),
            vertex([4.0, 0.0, 0.0], [1.0, 0.0]),
            vertex([0.0, 4.0, 0.0], [0.0, 1.0]),
        ];
        let out = dispatch(&src, 2);
        assert_eq!(out.len(), 48);
        let whole = signed_area(&src);
        let mut sum = 0.0;
        for t in out.chunks(3) {
            let area = signed_area(t);
            assert!((area - whole / 16.0).abs() < 1e-5, "child area {area}");
            sum += area;
            for v in t {
                assert_eq!(v.position[2], 0.0);
                assert!(
                    (v.uv[0] - v.position[0] / 4.0).abs() < 1e-6
                        && (v.uv[1] - v.position[1] / 4.0).abs() < 1e-6
                );
                assert!((v.normal[2] - 1.0).abs() < 1e-6);
            }
        }
        assert!((sum - whole).abs() < 1e-4);
        // The first child is the corner child of vertex a.
        assert_eq!(out[0].position, [0.0, 0.0, 0.0]);
        assert_eq!(out[1].position, [1.0, 0.0, 0.0]);
        assert_eq!(out[2].position, [0.0, 1.0, 0.0]);
    }
}
