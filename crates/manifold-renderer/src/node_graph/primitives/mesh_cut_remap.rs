//! Shared dispatch and stasis for mesh and weight cut-map resampling.
use manifold_gpu::{GpuBinding, GpuComputePipeline};

use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

pub(super) fn run<P: Primitive>(
    ctx: &mut EffectNodeContext<'_, '_>,
    pipeline: &mut Option<GpuComputePipeline>,
    last_key: &mut Option<[u64; 7]>,
    output_stride: u64,
) {
    if ["in", "map"].iter().any(|port| {
        ctx.inputs
            .slot(port)
            .is_none_or(|slot| !ctx.inputs.slot_content_ready(slot))
    }) {
        ctx.mark_outputs_pending();
        return;
    }
    let (Some(input), Some(map), Some(output)) = (
        ctx.inputs.array("in"),
        ctx.inputs.array("map"),
        ctx.outputs.array("out"),
    ) else {
        ctx.mark_outputs_pending();
        return;
    };
    let count = map.size / 16;
    if output.size / output_stride != count || count > u64::from(u32::MAX) {
        ctx.mark_outputs_pending();
        ctx.error("cut remap: output must have the map's vertex capacity");
        return;
    }
    let key = [
        ctx.rebuild_epoch,
        ctx.inputs.slot_generation("in").unwrap_or(0),
        ctx.inputs.slot_generation("map").unwrap_or(0),
        input.identity_key() as u64,
        map.identity_key() as u64,
        output.identity_key() as u64,
        count,
    ];
    if *last_key == Some(key) {
        ctx.mark_outputs_unchanged();
        return;
    }
    if count != 0 {
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<P>(pipeline, gpu.device);
        let uniforms = [count as u32, 0, 0, 0];
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: map,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: output,
                    offset: 0,
                },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            P::TYPE_ID,
        );
    }
    *last_key = Some(key);
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::super::taper_mesh::TaperMesh;
    use crate::TestDevice;
    use crate::generators::mesh_common::{MeshVertex, Vec4Vertex};
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
    use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::freeze::classify::CapacityExpr;
    use crate::node_graph::freeze::codegen::{
        FusionRegion, InputSource, RegionNode, generate_fused, standalone_for_spec,
    };
    use crate::node_graph::primitive::{Primitive, PrimitiveSpec};
    use crate::node_graph::primitives::{RemapCutWeights, RemapMeshCut};
    use crate::node_graph::{FrameTime, MetalBackend, NodeInstanceId};
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuBinding;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn vertex(position: [f32; 3], normal: [f32; 3], uv: [f32; 2], tangent: [f32; 4]) -> MeshVertex {
        MeshVertex {
            position,
            _pad0: 17.0,
            normal,
            _pad1: 19.0,
            uv,
            _pad2: [23.0, 29.0],
            tangent,
        }
    }

    fn map(bary: [f32; 3], triangle: f32) -> Vec4Vertex {
        Vec4Vertex {
            position: [bary[0], bary[1], bary[2], triangle],
        }
    }

    fn dispatch_mesh(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        source: &[MeshVertex],
        maps: &[Vec4Vertex],
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "cut-remap-test",
        );
        let source_buf = device.create_buffer_shared(std::mem::size_of_val(source).max(1) as u64);
        let map_buf = device.create_buffer_shared(std::mem::size_of_val(maps).max(1) as u64);
        let output_buf = device
            .create_buffer_shared((maps.len() * std::mem::size_of::<MeshVertex>()).max(1) as u64);
        unsafe {
            source_buf.write(0, bytemuck::cast_slice(source));
            map_buf.write(0, bytemuck::cast_slice(maps));
        }
        let uniforms = [maps.len() as u32, 0, 0, 0];
        let mut encoder = device.create_encoder("cut-remap-test");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &source_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &map_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &output_buf,
                    offset: 0,
                },
            ],
            [(maps.len() as u32).div_ceil(256), 1, 1],
            "cut-remap-test",
        );
        encoder.commit_and_wait_completed();
        let ptr = output_buf.mapped_ptr().expect("shared remap output");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, maps.len()) }.to_vec()
    }

    fn dispatch_weights(
        device: &manifold_gpu::GpuDevice,
        source: &[f32],
        maps: &[Vec4Vertex],
    ) -> Vec<f32> {
        let wgsl = standalone_for_spec::<RemapCutWeights>().expect("weight remap codegen");
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "cut-weight-remap-test",
        );
        let source_buf = device.create_buffer_shared((source.len() * 4).max(1) as u64);
        let map_buf = device.create_buffer_shared((maps.len() * 16).max(1) as u64);
        let output_buf = device.create_buffer_shared((maps.len() * 4).max(1) as u64);
        unsafe {
            source_buf.write(0, bytemuck::cast_slice(source));
            map_buf.write(0, bytemuck::cast_slice(maps));
        }
        let uniforms = [maps.len() as u32, 0, 0, 0];
        let mut encoder = device.create_encoder("cut-weight-remap-test");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &source_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &map_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &output_buf,
                    offset: 0,
                },
            ],
            [(maps.len() as u32).div_ceil(256), 1, 1],
            "cut-weight-remap-test",
        );
        encoder.commit_and_wait_completed();
        let ptr = output_buf.mapped_ptr().expect("shared weight output");
        unsafe { std::slice::from_raw_parts(ptr as *const f32, maps.len()) }.to_vec()
    }

    #[test]
    fn standalone_remap_preserves_corners_interpolates_frames_and_zeros_invalid_maps() {
        let device = crate::test_device();
        let source = vec![
            vertex(
                [0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0],
                [1.0, 0.0, 0.0, 1.0],
            ),
            vertex(
                [2.0, 1.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 0.0],
                [0.0, 1.0, 0.0, -1.0],
            ),
            vertex(
                [0.0, 0.0, 3.0],
                [0.0, 0.0, 1.0],
                [0.0, 1.0],
                [0.0, 0.0, 1.0, 1.0],
            ),
        ];
        let maps = vec![
            map([1.0, 0.0, 0.0], 0.0),
            map([0.25, 0.5, 0.25], 0.0),
            map([1.0, 0.0, 0.0], -1.0),
            map([1.0, 0.0, 0.0], 99.0),
        ];
        let wgsl = standalone_for_spec::<RemapMeshCut>().expect("mesh remap codegen");
        let out = dispatch_mesh(&device, &wgsl, &source, &maps);
        assert_eq!(out[0].position, source[0].position);
        assert_eq!(out[0].normal, source[0].normal);
        assert_eq!(out[0].uv, source[0].uv);
        assert_eq!(out[0].tangent, source[0].tangent);
        assert_eq!(out[1].position, [1.0, 0.5, 0.75]);
        assert_eq!(out[1].uv, [0.5, 0.25]);
        let n = out[1].normal;
        assert!(
            (n[0] - 0.8164966).abs() < 1e-5
                && (n[1] - 0.4082483).abs() < 1e-5
                && (n[2] - 0.4082483).abs() < 1e-5,
            "normal must be normalized barycentric interpolation: {n:?}"
        );
        let t = out[1].tangent;
        assert!(
            (t[0] * t[0] + t[1] * t[1] + t[2] * t[2] - 1.0).abs() < 1e-5,
            "tangent frame must be unit length: {t:?}"
        );
        assert!(
            (t[0] * n[0] + t[1] * n[1] + t[2] * n[2]).abs() < 1e-5,
            "tangent must be orthogonal to normal"
        );
        assert!(
            out[2].position.iter().all(|x| x.is_finite()) && out[2].position == [0.0; 3],
            "invalid padding must be finite zero-area geometry"
        );
        assert!(
            out[3].position.iter().all(|x| x.is_finite()) && out[3].position == [0.0; 3],
            "out-of-range source triangle must be guarded"
        );
    }

    #[test]
    fn standalone_weight_remap_interpolates_and_zeros_invalid_maps() {
        let device = crate::test_device();
        let maps = vec![
            map([1.0, 0.0, 0.0], 0.0),
            map([0.25, 0.5, 0.25], 0.0),
            map([1.0, 0.0, 0.0], -1.0),
            map([1.0, 0.0, 0.0], 100.0),
        ];
        let out = dispatch_weights(&device, &[2.0, 4.0, 8.0], &maps);
        assert_eq!(out[0], 2.0);
        assert!((out[1] - 4.5).abs() < 1e-6);
        assert_eq!(out[2], 0.0);
        assert_eq!(out[3], 0.0);
    }

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct FusedParams {
        axis: u32,
        taper: f32,
        center: f32,
        length: f32,
        weights_len: u32,
        _pad0: u32,
        _pad1: u32,
        _pad2: u32,
    }

    fn fused_remap_then_taper_wgsl() -> String {
        let id = |n| NodeInstanceId(n);
        let region = FusionRegion {
            nodes: vec![
                RegionNode {
                    node_id: id(0),
                    fusion_kind: RemapMeshCut::FUSION_KIND,
                    body: RemapMeshCut::WGSL_BODY.unwrap(),
                    params: RemapMeshCut::PARAMS,
                    inputs: vec![InputSource::External(0), InputSource::External(1)],
                    input_access: RemapMeshCut::INPUT_ACCESS.to_vec(),
                    node_inputs: RemapMeshCut::INPUTS,
                    node_outputs: RemapMeshCut::OUTPUTS,
                    node_includes: RemapMeshCut::WGSL_INCLUDES,
                    derived_uniforms: RemapMeshCut::DERIVED_UNIFORMS,
                    type_id: RemapMeshCut::TYPE_ID.to_string(),
                    derived_camera_ext: None,
                    output_storage: "rgba16float",
                    stencil_fetch: false,
                    quantize_f16: false,
                },
                RegionNode {
                    node_id: id(1),
                    fusion_kind: TaperMesh::FUSION_KIND,
                    body: TaperMesh::WGSL_BODY.unwrap(),
                    params: TaperMesh::PARAMS,
                    inputs: vec![InputSource::Node(id(0)), InputSource::External(2)],
                    input_access: TaperMesh::INPUT_ACCESS.to_vec(),
                    node_inputs: TaperMesh::INPUTS,
                    node_outputs: TaperMesh::OUTPUTS,
                    node_includes: TaperMesh::WGSL_INCLUDES,
                    derived_uniforms: TaperMesh::DERIVED_UNIFORMS,
                    type_id: TaperMesh::TYPE_ID.to_string(),
                    derived_camera_ext: None,
                    output_storage: "rgba16float",
                    stencil_fetch: false,
                    quantize_f16: false,
                },
            ],
            num_external_inputs: 3,
            outputs: vec![(id(1), "out".into())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
            output_capacity: Some(CapacityExpr::Slot(1)),
        };
        let generated = generate_fused(&region).expect("remap+taper fused codegen");
        assert!(
            generated.wgsl.contains("fn n0_body") && generated.wgsl.contains("fn n1_body"),
            "both remap and downstream bodies must be fused"
        );
        assert!(
            generated.wgsl.contains("@fused_output_capacity: s1"),
            "fused output must follow the longer map input"
        );
        assert!(
            generated.wgsl.contains("arrayLength(&src_1)"),
            "map must be the fused count anchor"
        );
        assert!(
            generated.wgsl.contains("let r0 = n0_body(")
                && generated.wgsl.contains("let r1 = n1_body(")
                && generated.wgsl.contains("dst[idx] = r1;"),
            "the generated kernel must execute both bodies and write the downstream result"
        );
        assert!(
            naga::front::wgsl::parse_str(&generated.wgsl).is_ok(),
            "fused remap ABI must parse"
        );
        generated.wgsl
    }

    #[test]
    fn fused_remap_with_longer_map_executes_and_matches_standalone_semantics() {
        let device = crate::test_device();
        let source = vec![
            vertex(
                [0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0],
                [1.0, 0.0, 0.0, 1.0],
            ),
            vertex(
                [2.0, 1.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 0.0],
                [0.0, 1.0, 0.0, -1.0],
            ),
            vertex(
                [0.0, 0.0, 3.0],
                [0.0, 0.0, 1.0],
                [0.0, 1.0],
                [0.0, 0.0, 1.0, 1.0],
            ),
        ];
        let maps = vec![
            map([1.0, 0.0, 0.0], 0.0),
            map([0.0, 1.0, 0.0], 0.0),
            map([0.0, 0.0, 1.0], 0.0),
            map([0.25, 0.5, 0.25], 0.0),
            map([0.5, 0.25, 0.25], 0.0),
            map([0.25, 0.25, 0.5], 0.0),
        ];
        let weights = vec![1.0_f32; maps.len()];
        let wgsl = fused_remap_then_taper_wgsl();
        let standalone = dispatch_mesh(
            &device,
            &standalone_for_spec::<RemapMeshCut>().expect("mesh remap codegen"),
            &source,
            &maps,
        );
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "fused-cut-remap-test",
        );
        let source_buf = device.create_buffer_shared((source.len() * 64) as u64);
        let map_buf = device.create_buffer_shared((maps.len() * 16) as u64);
        let weights_buf = device.create_buffer_shared((weights.len() * 4) as u64);
        let output_buf = device.create_buffer_shared((maps.len() * 64) as u64);
        unsafe {
            source_buf.write(0, bytemuck::cast_slice(&source));
            map_buf.write(0, bytemuck::cast_slice(&maps));
            weights_buf.write(0, bytemuck::cast_slice(&weights));
        }
        let params = FusedParams {
            axis: 1,
            taper: 0.5,
            center: 0.0,
            length: 1.0,
            weights_len: weights.len() as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let mut encoder = device.create_encoder("fused-cut-remap-test");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &source_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &map_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &weights_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &output_buf,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "fused-cut-remap-test",
        );
        encoder.commit_and_wait_completed();
        let ptr = output_buf.mapped_ptr().expect("fused output");
        let out = unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, maps.len()) };
        for (i, (v, remapped)) in out.iter().zip(&standalone).enumerate() {
            let taper = 1.0 - 0.5 * remapped.position[1].clamp(0.0, 1.0);
            assert!(
                (v.position[0] - remapped.position[0] * taper).abs() < 1e-5,
                "fused position x at {i}"
            );
            assert!(
                (v.position[1] - remapped.position[1]).abs() < 1e-5,
                "fused position y at {i}"
            );
            assert!(
                (v.position[2] - remapped.position[2] * taper).abs() < 1e-5,
                "fused position z at {i}"
            );
        }
    }

    fn run_context_once(
        prim: &mut RemapMeshCut,
        backend: &MetalBackend,
        device: &TestDevice,
        input_bindings: &[(&'static str, Slot)],
        output_bindings: &[(&'static str, Slot)],
        generations: &[u64],
        pending: &[bool],
        rebuild_epoch: u64,
    ) -> (bool, bool) {
        let mut scalar_ws = Vec::new();
        let mut camera_ws = Vec::new();
        let mut light_ws = Vec::new();
        let mut material_ws = Vec::new();
        let mut transform_ws = Vec::new();
        let mut atmosphere_ws = Vec::new();
        let mut object_ws = Vec::new();
        let backend_ref: &dyn Backend = backend;
        let inputs =
            NodeInputs::new(input_bindings, backend_ref, generations).with_pending(pending);
        let outputs = NodeOutputs::new(
            output_bindings,
            backend_ref,
            &mut scalar_ws,
            &mut camera_ws,
            &mut light_ws,
            &mut material_ws,
            &mut transform_ws,
            &mut atmosphere_ws,
            &mut object_ws,
        );
        let mut native = device.create_encoder("cut-remap-context-test");
        let (unchanged, pending) = {
            let mut gpu = RendererGpuEncoder::new(&mut native, device);
            let params = ParamValues::default();
            let mut ctx =
                EffectNodeContext::new(frame_time(), &params, inputs, outputs, Some(&mut gpu));
            ctx.rebuild_epoch = rebuild_epoch;
            Primitive::run(prim, &mut ctx);
            (ctx.outputs_unchanged, ctx.outputs_pending)
        };
        native.commit_and_wait_completed();
        (unchanged, pending)
    }

    #[test]
    fn context_cache_tracks_generation_epoch_and_readiness() {
        let device = crate::test_device();
        let mut backend = MetalBackend::new(
            device.arc(),
            1,
            1,
            manifold_gpu::GpuTextureFormat::Rgba8Unorm,
        );
        let source = device.create_buffer_shared(64 * 3);
        let maps = device.create_buffer_shared(16 * 2);
        let output = device.create_buffer_shared(64 * 2);
        let source_slot = backend.pre_bind_array(ResourceId(0), source);
        let map_slot = backend.pre_bind_array(ResourceId(1), maps);
        let output_slot = backend.pre_bind_array(ResourceId(2), output);
        let inputs = [("in", source_slot), ("map", map_slot)];
        let outputs = [("out", output_slot)];
        let mut generations = vec![0_u64; 3];
        let mut pending = vec![false; 3];
        let mut prim = RemapMeshCut::new();
        let first = run_context_once(
            &mut prim,
            &backend,
            &device,
            &inputs,
            &outputs,
            &generations,
            &pending,
            7,
        );
        assert_eq!(first, (false, false), "first ready frame dispatches");
        let second = run_context_once(
            &mut prim,
            &backend,
            &device,
            &inputs,
            &outputs,
            &generations,
            &pending,
            7,
        );
        assert_eq!(
            second,
            (true, false),
            "same generations and epoch use stasis"
        );
        generations[source_slot.0 as usize] = 1;
        let changed = run_context_once(
            &mut prim,
            &backend,
            &device,
            &inputs,
            &outputs,
            &generations,
            &pending,
            7,
        );
        assert_eq!(
            changed,
            (false, false),
            "source generation change dispatches"
        );
        pending[map_slot.0 as usize] = true;
        let not_ready = run_context_once(
            &mut prim,
            &backend,
            &device,
            &inputs,
            &outputs,
            &generations,
            &pending,
            7,
        );
        assert_eq!(
            not_ready,
            (false, true),
            "pending map blocks dispatch and reports readiness"
        );
        pending[map_slot.0 as usize] = false;
        let new_epoch = run_context_once(
            &mut prim,
            &backend,
            &device,
            &inputs,
            &outputs,
            &generations,
            &pending,
            8,
        );
        assert_eq!(
            new_epoch,
            (false, false),
            "new executor epoch invalidates stasis"
        );
    }
}
