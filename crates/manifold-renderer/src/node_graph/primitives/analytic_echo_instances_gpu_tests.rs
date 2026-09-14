//! Numerical GPU proofs for `node.analytic_echo_instances`.

use super::{AnalyticEchoInstances, ECHO_CAPACITY, EchoUniforms as Uniforms};
use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::freeze::codegen::{ENTRY, standalone_for_spec};
use manifold_gpu::GpuBinding;

fn zero_instance() -> InstanceTransform {
    InstanceTransform {
        pos_scale: [0.0; 4],
        rot_pad: [0.0; 4],
    }
}

fn assert_instance_eq(actual: InstanceTransform, expected: InstanceTransform) {
    assert_eq!(actual.pos_scale, expected.pos_scale);
    assert_eq!(actual.rot_pad, expected.rot_pad);
}

fn cpu_echo(src: InstanceTransform, echo_idx: usize, uniforms: Uniforms) -> InstanceTransform {
    if echo_idx == 0 {
        return src;
    }
    if src.pos_scale[3] == 0.0 {
        return zero_instance();
    }
    let active = uniforms.count.clamp(1, ECHO_CAPACITY as i32) as usize;
    if echo_idx >= active || uniforms.enabled <= 0.0 {
        return zero_instance();
    }
    if uniforms.radius == 0.0 && uniforms.rise == 0.0 {
        return zero_instance();
    }
    let u = echo_idx as f32 / 7.0;
    let angle = (uniforms.phase + uniforms.arc * u) * std::f32::consts::TAU;
    let factor = (1.0 - uniforms.taper.clamp(0.0, 1.0) * u).max(0.001);
    let source_offset = [
        uniforms.source_offset_x,
        uniforms.source_offset_y,
        uniforms.source_offset_z,
    ];
    let mut pos = [0.0; 3];
    for axis in 0..3 {
        pos[axis] = src.pos_scale[axis] * factor + source_offset[axis] * (factor - 1.0);
    }
    pos[0] += angle.cos() * uniforms.radius * uniforms.scene_radius * u;
    pos[1] += uniforms.rise * uniforms.scene_radius * u;
    pos[2] += angle.sin() * uniforms.radius * uniforms.scene_radius * u;
    InstanceTransform {
        pos_scale: [pos[0], pos[1], pos[2], src.pos_scale[3] * factor],
        rot_pad: src.rot_pad,
    }
}

fn dispatch(src: &[InstanceTransform], uniforms: Uniforms) -> Vec<InstanceTransform> {
    let device = crate::test_device();
    let wgsl =
        standalone_for_spec::<AnalyticEchoInstances>().expect("analytic echo standalone codegen");
    let pipeline =
        device.create_compute_pipeline(&wgsl, ENTRY, "structured-modifier-analytic-echo");
    let input = device
        .create_buffer_shared(src.len() as u64 * std::mem::size_of::<InstanceTransform>() as u64);
    unsafe {
        input.write(0, bytemuck::cast_slice(src));
    }
    let output_count = src.len() * ECHO_CAPACITY as usize;
    let output = device.create_buffer_shared(
        output_count as u64 * std::mem::size_of::<InstanceTransform>() as u64,
    );
    let mut encoder = device.create_encoder("structured-modifier-analytic-echo");
    encoder.dispatch_compute(
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
        [(output_count as u32).div_ceil(256), 1, 1],
        "structured-modifier-analytic-echo",
    );
    encoder.commit_and_wait_completed();
    let ptr = output.mapped_ptr().expect("shared output buffer");
    unsafe { std::slice::from_raw_parts(ptr as *const InstanceTransform, output_count) }.to_vec()
}

#[test]
fn structured_modifier_echo_gpu_preserves_source_and_coherent_slots() {
    let source = [
        InstanceTransform {
            pos_scale: [10.0, 20.0, 30.0, -2.0],
            rot_pad: [0.1, 0.2, 0.3, 7.0],
        },
        // A second active part of the same imported object. Its relative
        // separation must be scaled by the same taper factor.
        InstanceTransform {
            pos_scale: [14.0, 20.0, 30.0, 1.0],
            rot_pad: [0.4, 0.5, 0.6, 7.0],
        },
        InstanceTransform {
            pos_scale: [-3.0, 1.0, 4.0, 0.0],
            rot_pad: [9.0, 8.0, 7.0, 6.0],
        },
    ];
    let uniforms = Uniforms {
        count: 3,
        radius: 2.0,
        rise: 1.5,
        phase: 0.25,
        arc: 1.5,
        taper: 0.4,
        enabled: 1.0,
        scene_radius: 2.0,
        source_offset_x: 1.0,
        source_offset_y: 2.0,
        source_offset_z: 3.0,
        dispatch_count: (source.len() * ECHO_CAPACITY as usize) as u32,
    };
    let got = dispatch(&source, uniforms);
    for (echo_idx, actual) in got.iter().take(ECHO_CAPACITY as usize).enumerate() {
        let expected = cpu_echo(source[0], echo_idx, uniforms);
        for axis in 0..4 {
            assert!(
                (actual.pos_scale[axis] - expected.pos_scale[axis]).abs() < 1e-5,
                "slot {echo_idx} pos_scale[{axis}] GPU={} CPU={}",
                actual.pos_scale[axis],
                expected.pos_scale[axis]
            );
        }
        for axis in 0..4 {
            assert_eq!(
                actual.rot_pad[axis].to_bits(),
                expected.rot_pad[axis].to_bits(),
                "slot {echo_idx} rot_pad[{axis}]"
            );
        }
    }
    let factor = 1.0 - uniforms.taper * (1.0 / 7.0);
    for axis in 0..3 {
        let separation = got[ECHO_CAPACITY as usize + 1].pos_scale[axis] - got[1].pos_scale[axis];
        assert!(
            (separation - (source[1].pos_scale[axis] - source[0].pos_scale[axis]) * factor).abs()
                < 1e-5
        );
    }
    assert_instance_eq(got[ECHO_CAPACITY as usize * 2], source[2]);
    for slot in &got[ECHO_CAPACITY as usize * 2 + 1..] {
        assert_instance_eq(*slot, zero_instance());
    }
}

#[test]
fn structured_modifier_echo_gpu_disabled_and_count_one_keep_only_present_copy() {
    let source = [InstanceTransform {
        pos_scale: [1.0, -2.0, 3.0, 1.0],
        rot_pad: [0.4, 0.5, 0.6, 2.0],
    }];
    let uniforms = Uniforms {
        count: 8,
        radius: 100.0,
        rise: 100.0,
        phase: 0.0,
        arc: 2.0,
        taper: 1.0,
        enabled: 0.0,
        scene_radius: 3.0,
        source_offset_x: 50.0,
        source_offset_y: 60.0,
        source_offset_z: 70.0,
        dispatch_count: ECHO_CAPACITY,
    };
    let got = dispatch(&source, uniforms);
    assert_instance_eq(got[0], source[0]);
    for slot in &got[1..] {
        assert_instance_eq(*slot, zero_instance());
    }

    let mut one = uniforms;
    one.enabled = 1.0;
    one.count = 1;
    let got = dispatch(&source, one);
    assert_instance_eq(got[0], source[0]);
    for slot in &got[1..] {
        assert_instance_eq(*slot, zero_instance());
    }

    let mut neutral = uniforms;
    neutral.enabled = 1.0;
    neutral.radius = 0.0;
    neutral.rise = 0.0;
    let got = dispatch(&source, neutral);
    assert_instance_eq(got[0], source[0]);
    for slot in &got[1..] {
        assert_instance_eq(*slot, zero_instance());
    }
}

use crate::node_graph::NodeInstanceId;
use crate::node_graph::freeze::classify::{FusionKind, InputAccess};
use crate::node_graph::freeze::codegen::{FusionRegion, InputSource, RegionNode, generate_fused};
use crate::node_graph::primitive::PrimitiveSpec;

#[test]
fn structured_modifier_echo_indexed_gather_is_fusion_boundary() {
    let id = NodeInstanceId;
    let region = FusionRegion {
        nodes: vec![RegionNode {
            node_id: id(0),
            fusion_kind: FusionKind::Pointwise,
            body: AnalyticEchoInstances::WGSL_BODY.unwrap(),
            params: AnalyticEchoInstances::PARAMS,
            inputs: vec![InputSource::External(0)],
            input_access: vec![InputAccess::BufferGather],
            node_inputs: AnalyticEchoInstances::INPUTS,
            node_outputs: AnalyticEchoInstances::OUTPUTS,
            node_includes: AnalyticEchoInstances::WGSL_INCLUDES,
            derived_uniforms: AnalyticEchoInstances::DERIVED_UNIFORMS,
            type_id: AnalyticEchoInstances::TYPE_ID.to_string(),
            derived_camera_ext: None,
            output_storage: "rgba32float",
            stencil_fetch: false,
            quantize_f16: false,
        }],
        num_external_inputs: 1,
        outputs: vec![(id(0), "instances".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(),
        camera_externals: 0,
    };
    assert!(
        generate_fused(&region).is_err(),
        "indexed expansion must remain a standalone boundary"
    );
}
