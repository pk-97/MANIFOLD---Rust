//! Shared dispatch and stasis for mesh and weight cut-map resampling.
use manifold_gpu::{GpuBinding, GpuComputePipeline};

use manifold_node_engine::content_revision::ContentVersion;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

pub(super) fn run<P: Primitive>(
    ctx: &mut EffectNodeContext<'_, '_>,
    pipeline: &mut Option<GpuComputePipeline>,
    last_key: &mut Option<[u64; 5]>,
    output_stride: u64,
) {
    if ["in", "map"]
        .iter()
        .any(|port| ctx.inputs.slot(port).is_none())
    {
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
    // The map's live records: the kernel zeroes the rest of the output.
    let Some(live) = ctx.inputs.live_counts("map") else {
        ctx.mark_outputs_pending();
        return;
    };
    let hash_content = |content: Option<ContentVersion>| {
        use std::hash::{Hash, Hasher};
        let mut hasher = ahash::AHasher::default();
        content.hash(&mut hasher);
        hasher.finish()
    };
    let in_wired = ctx.inputs.slot("in").is_some();
    let map_wired = ctx.inputs.slot("map").is_some();
    let in_content = ctx.inputs.content_version("in");
    let map_content = ctx.inputs.content_version("map");
    let content_known =
        (!in_wired || in_content.is_some()) && (!map_wired || map_content.is_some());
    let key = [
        ctx.rebuild_epoch,
        hash_content(in_content),
        hash_content(map_content),
        output.identity_key() as u64,
        count,
    ];
    let content_unchanged = content_known
        && last_key.is_some_and(|mut previous| {
            previous[3] = key[3]; // Destination identity affects copying, not content.
            previous == key
        });
    if content_unchanged && *last_key == Some(key) && ctx.outputs_retained() {
        ctx.mark_outputs_unchanged();
        return;
    }
    if count != 0 {
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<P>(pipeline, gpu.device);
        let uniforms = [count as u32, live.per_item, 0, 0];
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
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &live.counts,
                    offset: live.offset,
                },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            P::TYPE_ID,
        );
    }
    *last_key = Some(key);
    if content_unchanged {
        ctx.mark_output_content_unchanged();
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::super::taper_mesh::TaperMesh;
    use crate::node_graph::primitives::{
        remap_cut_weights::RemapCutWeights, remap_mesh_cut::RemapMeshCut,
    };
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuBinding;
    use manifold_gpu::testkit::TestDevice;
    use manifold_node_engine::bindings::{NodeInputs, NodeOutputs, Slot};
    use manifold_node_engine::content_revision::ContentVersion;
    use manifold_node_engine::exec::backend::Backend;
    use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
    use manifold_node_engine::exec::execution_plan::ResourceId;
    use manifold_node_engine::exec::{
        effect_node::FrameTime, effect_node::NodeInstanceId, metal_backend::MetalBackend,
    };
    use manifold_node_engine::freeze::classify::CapacityExpr;
    use manifold_node_engine::freeze::codegen::{
        FusionRegion, InputSource, RegionNode, generate_fused, standalone_for_spec,
    };
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use manifold_node_engine::mesh::{MeshVertex, Vec4Vertex};
    use manifold_node_engine::primitive::{Primitive, PrimitiveSpec};

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
            color: [1.0; 4],
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
        dispatch_mesh_live(device, wgsl, source, maps, None)
    }

    /// `live`: the map's live record count the kernel stops at, or `None` for
    /// a map with no extent (the whole dispatch is live).
    fn dispatch_mesh_live(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        source: &[MeshVertex],
        maps: &[Vec4Vertex],
        live: Option<u32>,
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            manifold_node_engine::freeze::codegen::ENTRY,
            "cut-remap-test",
        );
        let source_buf = device.create_buffer(std::mem::size_of_val(source).max(1) as u64, manifold_gpu::BufferUse::Upload);
        let map_buf = device.create_buffer(std::mem::size_of_val(maps).max(1) as u64, manifold_gpu::BufferUse::Upload);
        let output_buf = device
            .create_buffer((maps.len() * std::mem::size_of::<MeshVertex>()).max(1) as u64, manifold_gpu::BufferUse::Upload);
        let counts_buf = device.create_buffer(4, manifold_gpu::BufferUse::Upload);
        unsafe {
            source_buf.write(0, bytemuck::cast_slice(source));
            map_buf.write(0, bytemuck::cast_slice(maps));
            // Pre-fill the output so a record the kernel skips would show.
            output_buf.write(
                0,
                &vec![0xAB_u8; maps.len() * std::mem::size_of::<MeshVertex>()],
            );
            counts_buf.write(0, bytemuck::bytes_of(&live.unwrap_or(0)));
        }
        let uniforms = [maps.len() as u32, u32::from(live.is_some()), 0, 0];
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
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &counts_buf,
                    offset: 0,
                },
            ],
            [(maps.len() as u32).div_ceil(256), 1, 1],
            "cut-remap-test",
        );
        encoder.commit_and_wait().expect("GPU work completes");
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
            manifold_node_engine::freeze::codegen::ENTRY,
            "cut-weight-remap-test",
        );
        let source_buf = device.create_buffer((source.len() * 4).max(1) as u64, manifold_gpu::BufferUse::Upload);
        let map_buf = device.create_buffer((maps.len() * 16).max(1) as u64, manifold_gpu::BufferUse::Upload);
        let output_buf = device.create_buffer((maps.len() * 4).max(1) as u64, manifold_gpu::BufferUse::Readback);
        unsafe {
            source_buf.write(0, bytemuck::cast_slice(source));
            map_buf.write(0, bytemuck::cast_slice(maps));
        }
        // No extent: `live_per_item` 0, and the counts binding is never read.
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
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &map_buf,
                    offset: 0,
                },
            ],
            [(maps.len() as u32).div_ceil(256), 1, 1],
            "cut-weight-remap-test",
        );
        encoder.commit_and_wait().expect("GPU work completes");
        let ptr = output_buf.mapped_ptr().expect("shared weight output");
        unsafe { std::slice::from_raw_parts(ptr as *const f32, maps.len()) }.to_vec()
    }

    #[test]
    fn standalone_remap_preserves_corners_interpolates_frames_and_zeros_invalid_maps() {
        let device = manifold_gpu::testkit::test_device();
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

    /// A kernel with a live extent writes the records below the count exactly
    /// as without one and zeroes every record from the count on, whatever the
    /// map holds there.
    #[test]
    fn standalone_remap_zeroes_records_past_the_live_count() {
        let device = manifold_gpu::testkit::test_device();
        let source = vec![
            vertex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0], [1.0, 0.0, 0.0, 1.0]),
            vertex([2.0, 1.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0], [0.0, 1.0, 0.0, -1.0]),
            vertex([0.0, 0.0, 3.0], [0.0, 0.0, 1.0], [0.0, 1.0], [0.0, 0.0, 1.0, 1.0]),
        ];
        // Seven records, three live: the tail holds valid maps that must still
        // come out zero.
        let maps = vec![
            map([1.0, 0.0, 0.0], 0.0),
            map([0.0, 1.0, 0.0], 0.0),
            map([0.0, 0.0, 1.0], 0.0),
            map([0.25, 0.5, 0.25], 0.0),
            map([0.5, 0.25, 0.25], 0.0),
            map([0.25, 0.25, 0.5], 0.0),
            map([1.0, 0.0, 0.0], 0.0),
        ];
        let wgsl = standalone_for_spec::<RemapMeshCut>().expect("mesh remap codegen");
        assert!(
            wgsl.contains("live_counts") && wgsl.contains("live_per_item"),
            "the map anchor carries the live guard"
        );
        let whole = dispatch_mesh(&device, &wgsl, &source, &maps);
        let live = dispatch_mesh_live(&device, &wgsl, &source, &maps, Some(3));
        assert_eq!(bytemuck::cast_slice::<_, u8>(&live[..3]), bytemuck::cast_slice::<_, u8>(&whole[..3]));
        assert!(
            bytemuck::cast_slice::<_, u8>(&live[3..]).iter().all(|byte| *byte == 0),
            "records past the live count are all zero"
        );
        let none = dispatch_mesh_live(&device, &wgsl, &source, &maps, Some(0));
        assert!(
            bytemuck::cast_slice::<_, u8>(&none).iter().all(|byte| *byte == 0),
            "a live count of zero zeroes everything"
        );
    }

    #[test]
    fn standalone_weight_remap_interpolates_and_zeros_invalid_maps() {
        let device = manifold_gpu::testkit::test_device();
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
            live_anchor: None,
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
        let device = manifold_gpu::testkit::test_device();
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
            manifold_node_engine::freeze::codegen::ENTRY,
            "fused-cut-remap-test",
        );
        let source_buf =
            device.create_buffer((source.len() * std::mem::size_of::<MeshVertex>()) as u64, manifold_gpu::BufferUse::Upload);
        let map_buf = device.create_buffer((maps.len() * 16) as u64, manifold_gpu::BufferUse::Upload);
        let weights_buf = device.create_buffer((weights.len() * 4) as u64, manifold_gpu::BufferUse::Upload);
        let output_buf =
            device.create_buffer((maps.len() * std::mem::size_of::<MeshVertex>()) as u64, manifold_gpu::BufferUse::Readback);
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
        encoder.commit_and_wait().expect("GPU work completes");
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
        let mut render_mode_ws = Vec::new();
        let mut object_ws = Vec::new();
        let backend_ref: &dyn Backend = backend;
        let max_slot = input_bindings
            .iter()
            .map(|(_, slot)| slot.0 as usize)
            .max()
            .unwrap_or(0);
        let mut content_versions = vec![None; max_slot + 1];
        for (_, slot) in input_bindings {
            let revision = generations.get(slot.0 as usize).copied().unwrap_or(0) + 1;
            content_versions[slot.0 as usize] =
                Some(ContentVersion::new(1, ResourceId(slot.0), revision));
        }
        let inputs = NodeInputs::new(input_bindings, backend_ref, generations)
            .with_pending(pending)
            .with_content_versions(&content_versions);
        let outputs = NodeOutputs::new(
            output_bindings,
            backend_ref,
            &mut scalar_ws,
            &mut camera_ws,
            &mut light_ws,
            &mut material_ws,
            &mut transform_ws,
            &mut atmosphere_ws,
            &mut render_mode_ws,
            &mut object_ws,
        );
        let mut native = device.create_encoder("cut-remap-context-test");
        let (unchanged, pending) = {
            let mut gpu = RendererGpuEncoder::new(&mut native, device);
            let params = ParamValues::default();
            let mut ctx =
                EffectNodeContext::new(frame_time(), &params, inputs, outputs, Some(&mut gpu))
                    .with_outputs_retained(true);
            ctx.rebuild_epoch = rebuild_epoch;
            Primitive::run(prim, &mut ctx);
            (ctx.outputs_unchanged, ctx.outputs_pending)
        };
        native.commit_and_wait().expect("GPU work completes");
        (unchanged, pending)
    }

    #[test]
    fn context_cache_tracks_generation_epoch_and_readiness() {
        let device = manifold_gpu::testkit::test_device();
        let mut backend = MetalBackend::new(
            device.arc(),
            1,
            1,
            manifold_gpu::GpuTextureFormat::Rgba8Unorm,
        );
        let source = device.create_buffer(std::mem::size_of::<MeshVertex>() as u64 * 3, manifold_gpu::BufferUse::Upload);
        let maps = device.create_buffer(16 * 2, manifold_gpu::BufferUse::Upload);
        let output = device.create_buffer(std::mem::size_of::<MeshVertex>() as u64 * 2, manifold_gpu::BufferUse::Upload);
        let source_slot = backend.pre_bind_array(ResourceId(0), source);
        let map_slot = backend.pre_bind_array(ResourceId(1), maps);
        let output_slot = backend.pre_bind_array(ResourceId(2), output);
        let inputs = [("in", source_slot), ("map", map_slot)];
        let outputs = [("out", output_slot)];
        let mut generations = vec![0_u64; 3];
        let pending = vec![false; 3];
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
        // A pending map never reaches this node: the executor holds it.
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

#[cfg(test)]
mod live_guard_tests {
    use crate::node_graph::primitives::{
        remap_cut_weights::RemapCutWeights, remap_mesh_cut::RemapMeshCut,
    };
    use manifold_node_engine::exec::effect_node::NodeInstanceId;
    use manifold_node_engine::freeze::classify::CapacityExpr;
    use manifold_node_engine::freeze::codegen::{
        FusionRegion, InputSource, RegionNode, generate_fused, standalone_for_spec,
    };
    use manifold_node_engine::primitive::PrimitiveSpec;

    /// A one-member remap region: `in` gathered (external 0), `map` read per
    /// element (external 1).
    fn remap_region(
        output_capacity: Option<CapacityExpr>,
        live_anchor: Option<usize>,
    ) -> FusionRegion<'static> {
        FusionRegion {
            nodes: vec![RegionNode {
                node_id: NodeInstanceId(0),
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
            }],
            num_external_inputs: 2,
            outputs: vec![(NodeInstanceId(0), "out".into())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
            live_anchor,
            output_capacity,
        }
    }

    fn line_starting<'a>(wgsl: &'a str, prefix: &str) -> &'a str {
        wgsl.lines()
            .find(|line| line.trim_start().starts_with(prefix))
            .unwrap_or_else(|| panic!("no line starting `{prefix}` in:\n{wgsl}"))
            .trim()
    }

    /// Without a capacity expression the count follows the arrays read per
    /// element: the gathered `in` (src_0) does not set it, and the fresh dst's
    /// own length still caps it.
    #[test]
    fn fused_count_anchor_ignores_gathered_externals() {
        let wgsl = generate_fused(&remap_region(None, None)).expect("remap region").wgsl;
        assert_eq!(
            line_starting(&wgsl, "let count ="),
            "let count = min(arrayLength(&src_1), arrayLength(&dst));"
        );
        assert!(
            !wgsl.contains("live_counts") && !wgsl.contains("live_per_item"),
            "no live anchor, no live binding or uniform"
        );
        assert!(naga::front::wgsl::parse_str(&wgsl).is_ok(), "parses:\n{wgsl}");
    }

    /// A live-anchored fused kernel binds the anchor's counts after `dst`,
    /// carries the `live_per_item` uniform, and zeroes the output past the
    /// live count before it reads any element.
    #[test]
    fn fused_kernel_with_a_live_anchor_stops_at_the_live_count() {
        let wgsl = generate_fused(&remap_region(Some(CapacityExpr::Slot(1)), Some(1)))
            .expect("anchored remap region")
            .wgsl;
        assert!(naga::front::wgsl::parse_str(&wgsl).is_ok(), "parses:\n{wgsl}");
        assert!(wgsl.contains("// @live_counts: src_1"), "marker names the anchor port:\n{wgsl}");
        assert!(
            wgsl.contains("var<storage, read> live_counts: array<u32>;"),
            "counts binding:\n{wgsl}"
        );
        assert!(wgsl.contains("live_per_item: u32"), "uniform field:\n{wgsl}");
        assert_eq!(line_starting(&wgsl, "let count ="), "let count = arrayLength(&src_1);");
        let guard = wgsl
            .find("idx >= min(count, live_counts[0] * params.live_per_item)")
            .expect("live guard");
        let zero_write = wgsl[guard..].find("dst[idx] = ").map(|at| guard + at).unwrap();
        let first_read = wgsl.find("let e_1 = src_1[idx];").expect("map pre-read");
        assert!(
            guard < zero_write && zero_write < first_read,
            "the tail is zeroed before any read:\n{wgsl}"
        );
        let count_guard = wgsl.find("if idx >= count {").expect("count guard");
        assert!(count_guard < guard, "the dispatch bound comes first");
    }

    /// Standalone kernels anchored on a cut map carry the same guard.
    #[test]
    fn standalone_remap_kernels_carry_the_live_guard() {
        for (name, wgsl) in [
            ("mesh", standalone_for_spec::<RemapMeshCut>().expect("mesh remap")),
            ("weights", standalone_for_spec::<RemapCutWeights>().expect("weights remap")),
        ] {
            assert!(naga::front::wgsl::parse_str(&wgsl).is_ok(), "{name} parses:\n{wgsl}");
            assert!(
                wgsl.contains("var<storage, read> live_counts: array<u32>;"),
                "{name} counts binding:\n{wgsl}"
            );
            let guard = wgsl
                .find("params.live_per_item != 0u && idx >= min(params.dispatch_count, live_counts[0] * params.live_per_item)")
                .unwrap_or_else(|| panic!("{name} live guard:\n{wgsl}"));
            let read = wgsl
                .find("let e_map = buf_map[idx];")
                .unwrap_or_else(|| panic!("{name} map pre-read:\n{wgsl}"));
            let between = &wgsl[guard..read];
            assert!(
                between.contains("buf_out[idx] = ") && between.contains("();"),
                "{name}: the output element is zeroed before any read:\n{wgsl}"
            );
        }
    }
}
