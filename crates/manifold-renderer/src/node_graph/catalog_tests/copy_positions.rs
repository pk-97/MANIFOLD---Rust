    use crate::node_graph::primitives::copy_positions::{CopyPositions, Uniforms};
    use manifold_gpu::GpuBinding;
    use manifold_node_engine::mesh::{InstanceTransform, Vec4Vertex};
    use manifold_node_engine::freeze::codegen::{ENTRY, FusionRegion, InputSource, RegionNode, generate_fused};
    use manifold_node_engine::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::displace_copies::DisplaceCopies;
    use crate::node_graph::primitives::wave_field_3d::WaveField3d;

    fn dispatch(src: &[InstanceTransform]) -> Vec<Vec4Vertex> {
        let device = manifold_gpu::testkit::test_device();
        let wgsl =
            manifold_node_engine::freeze::codegen::standalone_for_spec::<CopyPositions>().unwrap();
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            manifold_node_engine::freeze::codegen::ENTRY,
            "wave-pilot-copy",
        );
        let input = device.create_buffer_shared(std::mem::size_of_val(src) as u64);
        unsafe {
            input.write(0, bytemuck::cast_slice(src));
        }
        let output = device
            .create_buffer_shared(std::mem::size_of::<Vec4Vertex>() as u64 * src.len() as u64);
        let uniforms = Uniforms {
            dispatch_count: src.len() as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let mut enc = device.create_encoder("wave-pilot-copy");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &output,
                    offset: 0,
                },
            ],
            [(src.len() as u32).div_ceil(256), 1, 1],
            "wave-pilot-copy",
        );
        enc.commit_and_wait_completed();
        let ptr = output.mapped_ptr().unwrap();
        unsafe { std::slice::from_raw_parts(ptr as *const Vec4Vertex, src.len()) }.to_vec()
    }

    #[test]
    fn wave_pilot_copy_positions_matches_cpu_and_holes() {
        let src = vec![
            InstanceTransform {
                pos_scale: [1.25, -2.0, 3.5, 2.0],
                rot_pad: [0.1, 0.2, 0.3, 9.0],
            },
            InstanceTransform {
                pos_scale: [-4.0, 5.0, 6.0, 0.0],
                rot_pad: [7.0, 8.0, 9.0, 10.0],
            },
        ];
        let got = dispatch(&src);
        assert_eq!(
            got.iter().map(|v| v.position).collect::<Vec<_>>(),
            vec![[1.25, -2.0, 3.5, 1.0], [-4.0, 5.0, 6.0, 1.0]]
        );
    }

    #[test]
    fn wave_pilot_fused_copy_wave_displace_matches_standalone_and_cpu() {
        let device = manifold_gpu::testkit::test_device();
        let src = vec![
            InstanceTransform {
                pos_scale: [-0.75, 0.5, 1.25, 1.0],
                rot_pad: [0.1, 0.2, 0.3, 4.0],
            },
            InstanceTransform {
                pos_scale: [0.25, -1.0, 0.75, 2.0],
                rot_pad: [0.4, 0.5, 0.6, 5.0],
            },
            InstanceTransform {
                pos_scale: [1.5, 0.25, -0.5, 0.0],
                rot_pad: [0.7, 0.8, 0.9, 6.0],
            },
        ];
        let (frequency, phase, amount) = (0.37_f32, 0.19_f32, 1.6_f32);
        let direction = [0.8_f32, -0.35, 0.2];
        let id = manifold_node_engine::exec::effect_node::NodeInstanceId;
        let node = |node_id,
                    type_id: &'static str,
                    body,
                    params,
                    inputs,
                    input_access,
                    node_inputs,
                    node_outputs,
                    node_includes,
                    derived_uniforms| RegionNode {
            node_id: id(node_id),
            fusion_kind: manifold_node_engine::freeze::classify::FusionKind::Pointwise,
            body,
            params,
            inputs,
            input_access,
            node_inputs,
            node_outputs,
            node_includes,
            derived_uniforms,
            type_id: type_id.to_string(),
            derived_camera_ext: None,
            output_storage: "rgba16float",
            stencil_fetch: false,
            quantize_f16: false,
        };
        let region = FusionRegion {
            nodes: vec![
                node(
                    0,
                    CopyPositions::TYPE_ID,
                    CopyPositions::WGSL_BODY.unwrap(),
                    CopyPositions::PARAMS,
                    vec![InputSource::External(0)],
                    CopyPositions::INPUT_ACCESS.to_vec(),
                    CopyPositions::INPUTS,
                    CopyPositions::OUTPUTS,
                    CopyPositions::WGSL_INCLUDES,
                    CopyPositions::DERIVED_UNIFORMS,
                ),
                node(
                    1,
                    WaveField3d::TYPE_ID,
                    WaveField3d::WGSL_BODY.unwrap(),
                    WaveField3d::PARAMS,
                    vec![InputSource::Node(id(0))],
                    WaveField3d::INPUT_ACCESS.to_vec(),
                    WaveField3d::INPUTS,
                    WaveField3d::OUTPUTS,
                    WaveField3d::WGSL_INCLUDES,
                    WaveField3d::DERIVED_UNIFORMS,
                ),
                node(
                    2,
                    DisplaceCopies::TYPE_ID,
                    DisplaceCopies::WGSL_BODY.unwrap(),
                    DisplaceCopies::PARAMS,
                    vec![InputSource::External(0), InputSource::Node(id(1))],
                    DisplaceCopies::INPUT_ACCESS.to_vec(),
                    DisplaceCopies::INPUTS,
                    DisplaceCopies::OUTPUTS,
                    DisplaceCopies::WGSL_INCLUDES,
                    DisplaceCopies::DERIVED_UNIFORMS,
                ),
            ],
            num_external_inputs: 1,
            outputs: vec![(id(2), "instances".to_string())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
            output_capacity: None,
        };
        let fused = generate_fused(&region).expect("copy → wave → displace buffer region fuses");
        assert!(
            naga::front::wgsl::parse_str(&fused.wgsl).is_ok(),
            "fused wave pilot WGSL must parse"
        );

        let input = device.create_buffer_shared(std::mem::size_of_val(src.as_slice()) as u64);
        unsafe {
            input.write(0, bytemuck::cast_slice(&src));
        }
        let positions = device.create_buffer_shared(16 * src.len() as u64);
        let weights = device.create_buffer_shared(4 * src.len() as u64);
        let standalone_out =
            device.create_buffer_shared(std::mem::size_of_val(src.as_slice()) as u64);
        let cp = device.create_compute_pipeline(
            &manifold_node_engine::freeze::codegen::standalone_for_spec::<CopyPositions>().unwrap(),
            ENTRY,
            "wave-pilot-standalone-copy",
        );
        let wp = device.create_compute_pipeline(
            &manifold_node_engine::freeze::codegen::standalone_for_spec::<WaveField3d>().unwrap(),
            ENTRY,
            "wave-pilot-standalone-wave",
        );
        let dp = device.create_compute_pipeline(
            &manifold_node_engine::freeze::codegen::standalone_for_spec::<DisplaceCopies>().unwrap(),
            ENTRY,
            "wave-pilot-standalone-displace",
        );
        let cu = Uniforms {
            dispatch_count: src.len() as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let wu = crate::node_graph::primitives::wave_field_3d::Uniforms {
            frequency,
            phase,
            direction_x: direction[0],
            direction_y: direction[1],
            direction_z: direction[2],
            dispatch_count: src.len() as u32,
            _pad0: 0,
            _pad1: 0,
        };
        let du = crate::node_graph::primitives::displace_copies::Uniforms {
            amount,
            direction_x: 0.0,
            direction_y: 1.0,
            direction_z: 0.0,
            dispatch_count: src.len() as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let groups = (src.len() as u32).div_ceil(256);
        let mut enc = device.create_encoder("wave-pilot-standalone-chain");
        enc.dispatch_compute(
            &cp,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&cu),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &positions,
                    offset: 0,
                },
            ],
            [groups, 1, 1],
            "wave-pilot-standalone-copy",
        );
        enc.dispatch_compute(
            &wp,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&wu),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &positions,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &weights,
                    offset: 0,
                },
            ],
            [groups, 1, 1],
            "wave-pilot-standalone-wave",
        );
        enc.dispatch_compute(
            &dp,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&du),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &weights,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &standalone_out,
                    offset: 0,
                },
            ],
            [groups, 1, 1],
            "wave-pilot-standalone-displace",
        );
        enc.commit_and_wait_completed();

        let fused_pipeline =
            device.create_compute_pipeline(&fused.wgsl, ENTRY, "wave-pilot-fused-chain");
        let fused_out = device.create_buffer_shared(std::mem::size_of_val(src.as_slice()) as u64);
        let mut words = [0_u32; 12];
        for (slot, value) in [
            frequency,
            phase,
            direction[0],
            direction[1],
            direction[2],
            amount,
            0.0,
            1.0,
            0.0,
        ]
        .iter()
        .enumerate()
        {
            words[slot] = value.to_bits();
        }
        let mut fused_enc = device.create_encoder("wave-pilot-fused-chain");
        fused_enc.dispatch_compute(
            &fused_pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::cast_slice(&words),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &fused_out,
                    offset: 0,
                },
            ],
            [groups, 1, 1],
            "wave-pilot-fused-chain",
        );
        fused_enc.commit_and_wait_completed();
        let standalone_ptr = standalone_out.mapped_ptr().unwrap();
        let fused_ptr = fused_out.mapped_ptr().unwrap();
        let standalone = unsafe {
            std::slice::from_raw_parts(standalone_ptr as *const InstanceTransform, src.len())
        };
        let fused_result =
            unsafe { std::slice::from_raw_parts(fused_ptr as *const InstanceTransform, src.len()) };
        for (i, original) in src.iter().enumerate() {
            let spatial = original.pos_scale[0] * direction[0]
                + original.pos_scale[1] * direction[1]
                + original.pos_scale[2] * direction[2];
            let weight =
                (std::f32::consts::TAU * (spatial * frequency - (phase - phase.floor()))).sin();
            let expected_y = if original.pos_scale[3] == 0.0 {
                original.pos_scale[1]
            } else {
                original.pos_scale[1] + amount * weight
            };
            assert!(
                (standalone[i].pos_scale[1] - expected_y).abs() < 1e-5,
                "CPU oracle mismatch at {i}"
            );
            assert!(
                (fused_result[i].pos_scale[1] - standalone[i].pos_scale[1]).abs() < 1e-5,
                "fused y differs at {i}"
            );
            assert_eq!(fused_result[i].rot_pad, original.rot_pad);
            if original.pos_scale[3] == 0.0 {
                assert_eq!(
                    bytemuck::bytes_of(&fused_result[i]),
                    bytemuck::bytes_of(original)
                );
            }
        }
    }
