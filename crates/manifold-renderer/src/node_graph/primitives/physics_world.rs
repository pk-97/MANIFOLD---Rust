use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::physics::{BODY_PORTS, MAX_BODIES, MAX_COPIES, POSE_PORTS, RigidSimulation};
use crate::node_graph::primitive::Primitive;
use bytemuck::Zeroable;
use manifold_gpu::GpuBinding;
use std::borrow::Cow;

const INSTANCE_UPLOAD_WGSL: &str = include_str!("shaders/physics_instance_upload.wgsl");
const UPLOAD_TRANSFORMS: usize = 64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InstanceUploadParams {
    start: u32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
    values: [[f32; 4]; UPLOAD_TRANSFORMS * 2],
}

const _: () = assert!(std::mem::size_of::<InstanceUploadParams>() <= 4096);

const ZERO_INSTANCE: InstanceTransform = InstanceTransform {
    pos_scale: [0.0; 4],
    rot_pad: [0.0; 4],
};

pub struct InstanceUploadState {
    pipeline: Option<manifold_gpu::GpuComputePipeline>,
    data: Vec<InstanceTransform>,
    last_output_identity: Option<usize>,
    uploaded_count: usize,
}

impl Default for InstanceUploadState {
    fn default() -> Self {
        Self {
            pipeline: None,
            data: vec![ZERO_INSTANCE; MAX_COPIES],
            last_output_identity: None,
            uploaded_count: 0,
        }
    }
}

impl InstanceUploadState {
    fn install_pipeline(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.pipeline.is_none() {
            self.pipeline = Some(device.create_compute_pipeline(
                INSTANCE_UPLOAD_WGSL,
                "cs_main",
                "node.physics_world.instances",
            ));
        }
    }

    fn upload(
        &mut self,
        gpu: &mut crate::gpu_encoder::GpuEncoder<'_>,
        out: &manifold_gpu::GpuBuffer,
        active_count: usize,
    ) {
        let pipeline = self
            .pipeline
            .as_ref()
            .expect("physics world instance upload pipeline must be installed before upload");
        let output_identity = out.identity_key();
        let full_upload = self.last_output_identity != Some(output_identity);
        let ranges = if full_upload {
            [(0, MAX_COPIES), (0, 0)]
        } else if active_count < self.uploaded_count {
            [(0, active_count), (active_count, self.uploaded_count)]
        } else {
            [(0, active_count), (0, 0)]
        };
        let mut params = InstanceUploadParams::zeroed();
        for (start_index, end_index) in ranges {
            for start in (start_index..end_index).step_by(UPLOAD_TRANSFORMS) {
                let count = (end_index - start).min(UPLOAD_TRANSFORMS);
                params.start = start as u32;
                params.count = count as u32;
                for index in 0..count {
                    let transform = if start + index < active_count {
                        self.data[start + index]
                    } else {
                        ZERO_INSTANCE
                    };
                    params.values[index * 2] = transform.pos_scale;
                    params.values[index * 2 + 1] = transform.rot_pad;
                }
                gpu.native_enc.dispatch_compute(
                    pipeline,
                    &[
                        GpuBinding::Bytes {
                            binding: 0,
                            data: bytemuck::bytes_of(&params),
                        },
                        GpuBinding::Buffer {
                            binding: 1,
                            buffer: out,
                            offset: 0,
                        },
                    ],
                    [count.div_ceil(64) as u32, 1, 1],
                    "node.physics_world.instances",
                );
            }
        }
        self.last_output_identity = Some(output_identity);
        self.uploaded_count = active_count;
    }
}
crate::primitive! {
 name: PhysicsWorldNode,
 type_id: "node.physics_world",
 purpose: "Advance one shared Box3D rigid-body world at fixed 120 Hz ticks and output its body transforms. Sixteen independently wired body descriptions and an optional reset-latched copies prototype share contacts. Gravity and simulation speed are live controls; Reset restores the authored starting poses and copy grid.",
 inputs: {
body_0: RigidBody optional,
body_1: RigidBody optional,
body_2: RigidBody optional,
body_3: RigidBody optional,
body_4: RigidBody optional,
body_5: RigidBody optional,
body_6: RigidBody optional,
body_7: RigidBody optional,
body_8: RigidBody optional,
body_9: RigidBody optional,
body_10: RigidBody optional,
body_11: RigidBody optional,
body_12: RigidBody optional,
body_13: RigidBody optional,
body_14: RigidBody optional,
 body_15: RigidBody optional,
copies: RigidBody optional,
gravity_x: ScalarF32 optional, gravity_y: ScalarF32 optional, gravity_z: ScalarF32 optional, speed: ScalarF32 optional, reset: ScalarF32 optional,
copy_count: ScalarF32 optional, copy_spacing: ScalarF32 optional, copy_columns: ScalarF32 optional,
 },
 outputs: {
pose_0: Transform,
pose_1: Transform,
pose_2: Transform,
pose_3: Transform,
pose_4: Transform,
pose_5: Transform,
pose_6: Transform,
pose_7: Transform,
pose_8: Transform,
pose_9: Transform,
pose_10: Transform,
pose_11: Transform,
pose_12: Transform,
pose_13: Transform,
pose_14: Transform,
pose_15: Transform,
instances: Array(InstanceTransform),
active_count: ScalarF32,
physics_ms: ScalarF32,
 },
 params: [
ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("gravity_y"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("speed"), label: "Simulation Speed", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("reset"), label: "Reset", ty: ParamType::Trigger, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("copy_count"), label: "Copy Count", ty: ParamType::Int, default: ParamValue::Float(100.0), range: Some((0.0, MAX_COPIES as f32)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("copy_spacing"), label: "Copy Spacing", ty: ParamType::Float, default: ParamValue::Float(1.25), range: Some((0.01, 100.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("copy_columns"), label: "Copy Columns", ty: ParamType::Int, default: ParamValue::Float(16.0), range: Some((1.0, 64.0)), enum_values: &[] },
 ],
 depth_rule: Terminal,
 composition_notes: "Connect body_N to its matching pose_N consumer. Output transforms already include authored scale: connect directly to Scene Object transform, without applying that transform twice. Optional copies creates reset-latched bodies in the same native world and writes a fixed-capacity instances array plus active_count; copy_count, copy_spacing, and copy_columns are numeric port-shadowed controls and apply on first build, reset, or backwards transport. Copies use a centered x/z grid, preserve the copies rotation, and require uniform positive scale. State follows the transport clock; pause holds, reset/backward time restores initial poses. More than 128 pending ticks reports an error instead of silently dropping time. Shape/scale/topology edits rebuild this world; contact-property edits preserve motion. Native world stays private; no mutable handle wires.",
 examples: ["PhysicsSolids", "PhysicsBoxes"],
 picker: { label: "Physics World", category: Atom },
 summary: "Simulate colliding objects together under gravity, with speed and reset controls.",
 category: Geometry3D, role: Filter,
 aliases: ["physics", "box3d", "rigid simulation"],
 boundary_reason: NonGpu,
 extra_fields: { simulation: RigidSimulation = RigidSimulation::default(), upload: InstanceUploadState = InstanceUploadState::default(), },
}
impl PhysicsWorldNode {
    /// CPU pose upload is an IO boundary, so the atom codegen sweep cannot warm it.
    pub(crate) fn prewarm_pipeline(device: &manifold_gpu::GpuDevice) {
        device.create_compute_pipeline(
            INSTANCE_UPLOAD_WGSL,
            "cs_main",
            "node.physics_world.instances",
        );
    }
}

impl Primitive for PhysicsWorldNode {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "instances").then_some(MAX_COPIES as u32)
    }

    fn clear_state(&mut self) {
        self.simulation = RigidSimulation::default();
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let mut bodies = [None; MAX_BODIES];
        for (i, port) in BODY_PORTS.iter().enumerate() {
            bodies[i] = ctx.inputs.rigid_body(port);
        }
        let prototype = ctx.inputs.rigid_body("copies");
        let gravity = [
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity_y", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
        ];
        let speed = ctx.scalar_or_param("speed", 1.0);
        let reset = ctx.scalar_or_param("reset", 0.0);
        let copy_count = ctx.scalar_or_param("copy_count", 100.0);
        let copy_spacing = ctx.scalar_or_param("copy_spacing", 1.25);
        let copy_columns = ctx.scalar_or_param("copy_columns", 16.0);
        let result = self.simulation.advance_with_copies(
            bodies,
            prototype,
            copy_count,
            copy_spacing,
            copy_columns,
            gravity,
            ctx.time.seconds,
            speed,
            reset,
        );
        if let Err(error) = result {
            ctx.error(error);
            ctx.outputs.set_scalar("physics_ms", ParamValue::Float(0.0));
        } else {
            crate::node_graph::physics_metrics::record_frame(
                self.simulation.physics_ms,
                (bodies.iter().flatten().count() + self.simulation.active_copy_count) as u32,
            );
            ctx.outputs
                .set_scalar("physics_ms", ParamValue::Float(self.simulation.physics_ms));
        }
        for (port, pose) in POSE_PORTS.iter().zip(self.simulation.poses) {
            ctx.outputs.set_transform(port, pose);
        }
        ctx.outputs.set_scalar(
            "active_count",
            ParamValue::Float(self.simulation.active_copy_count as f32),
        );
        let Some(out) = ctx.outputs.array("instances") else {
            return;
        };
        for (dst, pose) in self.upload.data.iter_mut().zip(
            self.simulation
                .copy_poses
                .iter()
                .copied()
                .take(self.simulation.active_copy_count),
        ) {
            *dst = InstanceTransform {
                pos_scale: [pose.pos[0], pose.pos[1], pose.pos[2], pose.scale[0]],
                rot_pad: [pose.rot_euler[0], pose.rot_euler[1], pose.rot_euler[2], 0.0],
            };
        }
        let Some(gpu) = ctx.gpu.as_deref_mut() else {
            return;
        };
        self.upload.install_pipeline(gpu.device);
        self.upload
            .upload(gpu, out, self.simulation.active_copy_count);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::gpu_encoder::GpuEncoder;

    fn read_instances(buffer: &manifold_gpu::GpuBuffer) -> Vec<InstanceTransform> {
        let ptr = buffer.mapped_ptr().expect("shared output buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const InstanceTransform, MAX_COPIES).to_vec() }
    }

    #[test]
    fn instance_upload_crosses_inline_chunk_and_zeroes_shrunk_tail() {
        let device = crate::test_device();
        let output = device
            .create_buffer_shared((MAX_COPIES * std::mem::size_of::<InstanceTransform>()) as u64);
        let mut state = InstanceUploadState::default();
        for (index, value) in state.data.iter_mut().take(130).enumerate() {
            *value = InstanceTransform {
                pos_scale: [index as f32, 2.0, 3.0, 1.0],
                rot_pad: [0.1, 0.2, 0.3, index as f32],
            };
        }
        let mut encoder = device.create_encoder("physics-world-upload-proof");
        {
            let mut gpu = GpuEncoder::new(&mut encoder, &device);
            state.install_pipeline(&device);
            state.upload(&mut gpu, &output, 130);
        }
        encoder.commit_and_wait_completed();
        let first = read_instances(&output);
        assert_eq!(first[126].pos_scale[0], 126.0);
        assert_eq!(first[127].pos_scale[0], 127.0);
        assert_eq!(first[129].rot_pad[3], 129.0);
        assert_eq!(bytemuck::bytes_of(&first[130]), bytemuck::bytes_of(&ZERO_INSTANCE));

        state.data.fill(ZERO_INSTANCE);
        for (index, value) in state.data.iter_mut().take(3).enumerate() {
            *value = InstanceTransform {
                pos_scale: [100.0 + index as f32, 4.0, 5.0, 1.0],
                rot_pad: [0.4, 0.5, 0.6, 0.0],
            };
        }
        let mut encoder = device.create_encoder("physics-world-upload-shrink-proof");
        {
            let mut gpu = GpuEncoder::new(&mut encoder, &device);
            state.install_pipeline(&device);
            state.upload(&mut gpu, &output, 3);
        }
        encoder.commit_and_wait_completed();
        let shrunk = read_instances(&output);
        assert_eq!(shrunk[0].pos_scale[0], 100.0);
        assert_eq!(shrunk[2].pos_scale[0], 102.0);
        assert_eq!(bytemuck::bytes_of(&shrunk[3]), bytemuck::bytes_of(&ZERO_INSTANCE));
        assert_eq!(bytemuck::bytes_of(&shrunk[MAX_COPIES - 1]), bytemuck::bytes_of(&ZERO_INSTANCE));
    }
}
