//! Bounded deterministic selection of complete faces from a mesh.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::{Primitive, PrimitiveSpec};

use super::standalone_pipeline::standalone_pipeline;

pub const SAMPLE_MESH_TRIANGLES_CAPACITY: u32 = 512 * 3;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SampleMeshTrianglesUniforms {
    density: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: SampleMeshTriangles,
    type_id: "node.sample_mesh_triangles",
    purpose: "Select at most 512 complete triangles, evenly distributed by original face index, without changing any vertex attributes. Inactive output triangles are zero. This preserves original face correspondence for sparse mesh diagnostics.",
    inputs: { in: Array(MeshVertex) required, density: ScalarF32 optional },
    outputs: { vertices: Array(MeshVertex) },
    params: [ParamDef { name: Cow::Borrowed("density"), label: "Density", ty: ParamType::Float, default: ParamValue::Float(3.0), range: Some((2.0,8.0)), enum_values: &[] }],
    depth_rule: Terminal,
    composition_notes: "Selects min(density cubed, source face count) complete faces into a fixed 1536-vertex buffer. Original triangle order and all vertex attributes are preserved; selection uses source face index, not synthetic positions. Shared source_face_index defines the same correspondence for downstream weight lookup.",
    examples: [], picker: { label: "Sample Mesh Triangles", category: Atom },
    summary: "Selects a bounded set of real mesh faces for inspection.",
    category: Geometry3D, role: Source, aliases: ["sample mesh", "sample faces"],
    pure: true,
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/sample_mesh_triangles_body.wgsl"),
    input_access: [BufferGather],
    wgsl_includes: [include_str!("shaders/sample_face_common.wgsl")],
}
impl Primitive for SampleMeshTriangles {
    fn array_output_capacity(
        &self,
        port: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        _: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "vertices").then_some(SAMPLE_MESH_TRIANGLES_CAPACITY)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if ctx.inputs.slot("in").is_some_and(|slot| !ctx.inputs.slot_content_ready(slot)) {
            ctx.mark_outputs_pending();
            return;
        }
        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("vertices") else {
            return;
        };
        let density = ctx.scalar_or_param("density", 3.0).round().clamp(2.0, 8.0);
        let count = (dst.size / std::mem::size_of::<MeshVertex>() as u64) as u32;
        let uniform = SampleMeshTrianglesUniforms {
            density,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniform),
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
            [count.div_ceil(256), 1, 1],
            Self::TYPE_ID,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::freeze::classify::{FusionKind, InputAccess};
    use crate::node_graph::freeze::codegen::{
        FusionRegion, InputSource, RegionNode, generate_fused,
    };

    fn source_face_index(sample: u32, source_count: u32, sample_count: u32) -> u32 {
        sample * (source_count / sample_count)
            + sample * (source_count % sample_count) / sample_count
    }

    #[test]
    fn bounded_capacity_and_complete_face_contract() {
        assert_eq!(SAMPLE_MESH_TRIANGLES_CAPACITY, 1536);
        assert_eq!(SampleMeshTriangles::INPUTS.len(), 2);
        assert!(SampleMeshTriangles::INPUTS[0].required);
        assert!(!SampleMeshTriangles::INPUTS[1].required);
        assert_eq!(SampleMeshTriangles::OUTPUTS.len(), 1);
        assert_eq!(
            SampleMeshTriangles::array_output_capacity(
                &SampleMeshTriangles::new(),
                "vertices",
                &Default::default(),
                &[],
            ),
            Some(SAMPLE_MESH_TRIANGLES_CAPACITY)
        );
    }

    #[test]
    fn source_face_mapping_is_identity_or_evenly_distributed() {
        assert_eq!(
            (0..4)
                .map(|sample| source_face_index(sample, 4, 4))
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            (0..8)
                .map(|sample| source_face_index(sample, 10, 8))
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 5, 6, 7, 8]
        );
    }

    #[test]
    fn standalone_formula_and_zero_inactive_slots_are_declared() {
        let body = SampleMeshTriangles::WGSL_BODY.expect("sample mesh triangles body");
        assert!(body.contains("source_face_index(idx / 3u,total,sample_count)"));
        assert!(body.contains("Element(v.position,v.normal,v.uv,v.uv1,v.tangent,v.color)"));
        assert!(body.contains("if idx / 3u >= sample_count"));
        assert!(
            body.contains(
                "Element(vec3<f32>(0.0),vec3<f32>(0.0),vec2<f32>(0.0),vec2<f32>(0.0),vec4<f32>(0.0), vec4<f32>(1.0))",
            )
        );
        assert_eq!(SampleMeshTriangles::WGSL_INCLUDES.len(), 1);
    }

    /// BUG-x72p: a `BufferGather` input no longer forces Boundary — the fused
    /// buffer codegen binds the gathered wire as an external `src_<slot>` the
    /// body indexes itself (proven by the freeze codegen tests). What keeps
    /// THIS atom out of fused regions is its output capacity: a FIXED 1536
    /// vertices, a non-identity function of the input capacity. `build_region`
    /// probes every member's `array_output_capacity` when a `BufferGather`
    /// input is present and refuses anything that isn't the input minimum, so
    /// a region containing this atom renders unfused — always correct.
    #[test]
    fn fixed_output_capacity_refuses_the_gather_identity_probe() {
        let id = |n| crate::node_graph::effect_node::NodeInstanceId(n);
        let region = FusionRegion {
            nodes: vec![RegionNode {
                node_id: id(0),
                fusion_kind: FusionKind::Pointwise,
                body: SampleMeshTriangles::WGSL_BODY.unwrap(),
                params: SampleMeshTriangles::PARAMS,
                inputs: vec![InputSource::External(0)],
                input_access: vec![InputAccess::BufferGather],
                node_inputs: SampleMeshTriangles::INPUTS,
                node_outputs: SampleMeshTriangles::OUTPUTS,
                node_includes: SampleMeshTriangles::WGSL_INCLUDES,
                derived_uniforms: SampleMeshTriangles::DERIVED_UNIFORMS,
                type_id: SampleMeshTriangles::TYPE_ID.to_string(),
                derived_camera_ext: None,
                output_storage: "rgba32float",
                stencil_fetch: false,
                quantize_f16: false,
            }],
            num_external_inputs: 1,
            outputs: vec![(id(0), "vertices".to_string())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
            output_capacity: None,
        };
        // The codegen can EMIT the gathered kernel (the mechanism is general);
        // the refusal this atom needs is the finder's identity probe:
        // 1536 != the input capacity, so `build_region` drops any region
        // containing it and it renders unfused.
        assert!(generate_fused(&region).is_ok());
        let prim = SampleMeshTriangles::new();
        let node: &dyn crate::node_graph::effect_node::EffectNode = &prim;
        assert_eq!(
            node.array_output_capacity("vertices", &Default::default(), &[("in", 1009)]),
            Some(SAMPLE_MESH_TRIANGLES_CAPACITY),
            "fixed capacity is the non-identity the probe refuses"
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::node_graph::freeze::codegen::ENTRY;

    fn vertex(face: u32, corner: u32) -> MeshVertex {
        let n = face as f32 * 3.0 + corner as f32;
        MeshVertex {
            position: [n + 0.1, n + 0.2, n + 0.3],
            _pad0: 17.0 + n,
            normal: [n + 1.1, n + 1.2, n + 1.3],
            _pad1: -19.0 - n,
            uv: [n + 2.1, n + 2.2],
            _pad2: [23.0 + n, -29.0 - n],
            tangent: [
                n + 3.1,
                n + 3.2,
                n + 3.3,
                if corner == 0 { -1.0 } else { 1.0 },
            ],
            color: [1.0; 4],
        }
    }

    fn dispatch(
        wgsl: &str,
        source: &[MeshVertex],
        output_len: usize,
        label: &str,
    ) -> Vec<MeshVertex> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, label);
        let src = device.create_buffer_shared(std::mem::size_of_val(source) as u64);
        let dst = device
            .create_buffer_shared(output_len as u64 * std::mem::size_of::<MeshVertex>() as u64);
        unsafe {
            src.write(0, bytemuck::cast_slice(source));
        }
        let uniform = SampleMeshTrianglesUniforms {
            density: 2.0,
            dispatch_count: output_len as u32,
            _pad0: 0,
            _pad1: 0,
        };
        let mut encoder = device.create_encoder(label);
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniform),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &dst,
                    offset: 0,
                },
            ],
            [(output_len as u32).div_ceil(256), 1, 1],
            label,
        );
        encoder.commit_and_wait_completed();
        let ptr = dst.mapped_ptr().expect("shared output");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, output_len) }.to_vec()
    }

    fn assert_attrs_eq(actual: &MeshVertex, expected: &MeshVertex) {
        assert_eq!(actual.position, expected.position);
        assert_eq!(actual.normal, expected.normal);
        assert_eq!(actual.uv, expected.uv);
        assert_eq!(actual.tangent, expected.tangent);
        assert_eq!(actual.color, expected.color);
    }

    #[test]
    fn standalone_sampling_preserves_attributes_maps_faces_and_zeroes_inactive() {
        let source: Vec<_> = (0..10)
            .flat_map(|face| (0..3).map(move |corner| vertex(face, corner)))
            .collect();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<SampleMeshTriangles>()
            .expect("sample mesh triangles codegen");
        let sampled = dispatch(&wgsl, &source, 24, "sample-mesh-triangles");
        let selected_faces = [0, 1, 2, 3, 5, 6, 7, 8];
        for (sample_face, source_face) in selected_faces.into_iter().enumerate() {
            for corner in 0..3 {
                assert_attrs_eq(
                    &sampled[sample_face * 3 + corner],
                    &source[source_face * 3 + corner],
                );
            }
        }

        let source_small: Vec<_> = (0..1)
            .flat_map(|face| (0..3).map(move |corner| vertex(face, corner)))
            .collect();
        let sampled_small = dispatch(&wgsl, &source_small, 12, "sample-mesh-triangles-small");
        for (vertex, source) in sampled_small[..3].iter().zip(&source_small) {
            assert_attrs_eq(vertex, source);
        }
        for vertex in &sampled_small[3..] {
            assert_eq!(vertex.position, [0.0; 3]);
            assert_eq!(vertex.normal, [0.0; 3]);
            assert_eq!(vertex.uv, [0.0; 2]);
            assert_eq!(vertex.tangent, [0.0; 4]);
            assert_eq!(vertex.color, [1.0; 4]);
        }
    }
}
