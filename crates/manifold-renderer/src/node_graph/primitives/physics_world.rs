use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::instance_upload::InstanceSnapshotUpload;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::physics::{BODY_PORTS, MAX_COPIES, POSE_PORTS, RigidSimulation};
use crate::node_graph::primitive::Primitive;
use manifold_physics::FieldValue;
use std::borrow::Cow;

const ZERO_INSTANCE: InstanceTransform = InstanceTransform {
    pos_scale: [0.0; 4],
    rot_pad: [0.0; 4],
};

const TARGETED_ACCELERATION_PORTS: [&str; 65] = [
    "body_acceleration_0",
    "body_acceleration_1",
    "body_acceleration_2",
    "body_acceleration_3",
    "body_acceleration_4",
    "body_acceleration_5",
    "body_acceleration_6",
    "body_acceleration_7",
    "body_acceleration_8",
    "body_acceleration_9",
    "body_acceleration_10",
    "body_acceleration_11",
    "body_acceleration_12",
    "body_acceleration_13",
    "body_acceleration_14",
    "body_acceleration_15",
    "body_acceleration_16",
    "body_acceleration_17",
    "body_acceleration_18",
    "body_acceleration_19",
    "body_acceleration_20",
    "body_acceleration_21",
    "body_acceleration_22",
    "body_acceleration_23",
    "body_acceleration_24",
    "body_acceleration_25",
    "body_acceleration_26",
    "body_acceleration_27",
    "body_acceleration_28",
    "body_acceleration_29",
    "body_acceleration_30",
    "body_acceleration_31",
    "body_acceleration_32",
    "body_acceleration_33",
    "body_acceleration_34",
    "body_acceleration_35",
    "body_acceleration_36",
    "body_acceleration_37",
    "body_acceleration_38",
    "body_acceleration_39",
    "body_acceleration_40",
    "body_acceleration_41",
    "body_acceleration_42",
    "body_acceleration_43",
    "body_acceleration_44",
    "body_acceleration_45",
    "body_acceleration_46",
    "body_acceleration_47",
    "body_acceleration_48",
    "body_acceleration_49",
    "body_acceleration_50",
    "body_acceleration_51",
    "body_acceleration_52",
    "body_acceleration_53",
    "body_acceleration_54",
    "body_acceleration_55",
    "body_acceleration_56",
    "body_acceleration_57",
    "body_acceleration_58",
    "body_acceleration_59",
    "body_acceleration_60",
    "body_acceleration_61",
    "body_acceleration_62",
    "body_acceleration_63",
    "copies_acceleration",
];

fn read_copy_layout(ctx: &EffectNodeContext<'_, '_>) -> f32 {
    let wired = ctx
        .inputs
        .scalar("copy_layout")
        .and_then(|value| match value {
            ParamValue::Float(value) => Some(value),
            ParamValue::Enum(value) => Some(value as f32),
            _ => None,
        });
    wired.unwrap_or_else(|| match ctx.params.get("copy_layout") {
        Some(ParamValue::Enum(value)) => *value as f32,
        Some(ParamValue::Float(value)) => *value,
        _ => 0.0,
    })
}

pub struct InstanceUploadState {
    data: Vec<InstanceTransform>,
    snapshot: InstanceSnapshotUpload,
    next_version: u64,
}

impl Default for InstanceUploadState {
    fn default() -> Self {
        Self {
            data: vec![ZERO_INSTANCE; MAX_COPIES],
            snapshot: InstanceSnapshotUpload::default(),
            next_version: 0,
        }
    }
}

impl InstanceUploadState {
    fn upload(
        &mut self,
        gpu: &mut crate::gpu_encoder::GpuEncoder<'_>,
        out: &manifold_gpu::GpuBuffer,
        active_count: usize,
        version: u64,
        retained: bool,
    ) -> Result<bool, &'static str> {
        self.snapshot
            .upload(gpu, out, &self.data[..active_count], version, retained)
    }

    fn next_version(&mut self) -> u64 {
        let version = self.next_version;
        self.next_version = self.next_version.wrapping_add(1);
        version
    }
}
crate::primitive! {
 name: PhysicsWorldNode,
 type_id: "node.physics_world",
 purpose: "Advance one shared Box3D rigid-body world at fixed 60 Hz ticks and output its body transforms. Sixty-four independently wired body descriptions and an optional reset-latched copies prototype share contacts. Gravity and simulation speed are live controls; an optional continuous acceleration field contributes m/s² at each native microstep. Reset restores the authored starting poses and copy layout.",
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
body_16: RigidBody optional,
body_17: RigidBody optional,
body_18: RigidBody optional,
body_19: RigidBody optional,
body_20: RigidBody optional,
body_21: RigidBody optional,
body_22: RigidBody optional,
body_23: RigidBody optional,
body_24: RigidBody optional,
body_25: RigidBody optional,
body_26: RigidBody optional,
body_27: RigidBody optional,
body_28: RigidBody optional,
body_29: RigidBody optional,
body_30: RigidBody optional,
body_31: RigidBody optional,
body_32: RigidBody optional,
body_33: RigidBody optional,
body_34: RigidBody optional,
body_35: RigidBody optional,
body_36: RigidBody optional,
body_37: RigidBody optional,
body_38: RigidBody optional,
body_39: RigidBody optional,
body_40: RigidBody optional,
body_41: RigidBody optional,
body_42: RigidBody optional,
body_43: RigidBody optional,
body_44: RigidBody optional,
body_45: RigidBody optional,
body_46: RigidBody optional,
body_47: RigidBody optional,
body_48: RigidBody optional,
body_49: RigidBody optional,
body_50: RigidBody optional,
body_51: RigidBody optional,
body_52: RigidBody optional,
body_53: RigidBody optional,
body_54: RigidBody optional,
body_55: RigidBody optional,
body_56: RigidBody optional,
body_57: RigidBody optional,
body_58: RigidBody optional,
body_59: RigidBody optional,
body_60: RigidBody optional,
body_61: RigidBody optional,
body_62: RigidBody optional,
body_63: RigidBody optional,
copies: RigidBody optional,
gravity_x: ScalarF32 optional, gravity_y: ScalarF32 optional, gravity_z: ScalarF32 optional, speed: ScalarF32 optional, reset: ScalarF32 optional,
copy_count: ScalarF32 optional, copy_spacing: ScalarF32 optional, copy_columns: ScalarF32 optional, copy_layout: ScalarF32 optional,
acceleration_field: VectorField optional,
body_acceleration_0: VectorField optional,
body_acceleration_1: VectorField optional,
body_acceleration_2: VectorField optional,
body_acceleration_3: VectorField optional,
body_acceleration_4: VectorField optional,
body_acceleration_5: VectorField optional,
body_acceleration_6: VectorField optional,
body_acceleration_7: VectorField optional,
body_acceleration_8: VectorField optional,
body_acceleration_9: VectorField optional,
body_acceleration_10: VectorField optional,
body_acceleration_11: VectorField optional,
body_acceleration_12: VectorField optional,
body_acceleration_13: VectorField optional,
body_acceleration_14: VectorField optional,
body_acceleration_15: VectorField optional,
body_acceleration_16: VectorField optional,
body_acceleration_17: VectorField optional,
body_acceleration_18: VectorField optional,
body_acceleration_19: VectorField optional,
body_acceleration_20: VectorField optional,
body_acceleration_21: VectorField optional,
body_acceleration_22: VectorField optional,
body_acceleration_23: VectorField optional,
body_acceleration_24: VectorField optional,
body_acceleration_25: VectorField optional,
body_acceleration_26: VectorField optional,
body_acceleration_27: VectorField optional,
body_acceleration_28: VectorField optional,
body_acceleration_29: VectorField optional,
body_acceleration_30: VectorField optional,
body_acceleration_31: VectorField optional,
body_acceleration_32: VectorField optional,
body_acceleration_33: VectorField optional,
body_acceleration_34: VectorField optional,
body_acceleration_35: VectorField optional,
body_acceleration_36: VectorField optional,
body_acceleration_37: VectorField optional,
body_acceleration_38: VectorField optional,
body_acceleration_39: VectorField optional,
body_acceleration_40: VectorField optional,
body_acceleration_41: VectorField optional,
body_acceleration_42: VectorField optional,
body_acceleration_43: VectorField optional,
body_acceleration_44: VectorField optional,
body_acceleration_45: VectorField optional,
body_acceleration_46: VectorField optional,
body_acceleration_47: VectorField optional,
body_acceleration_48: VectorField optional,
body_acceleration_49: VectorField optional,
body_acceleration_50: VectorField optional,
body_acceleration_51: VectorField optional,
body_acceleration_52: VectorField optional,
body_acceleration_53: VectorField optional,
body_acceleration_54: VectorField optional,
body_acceleration_55: VectorField optional,
body_acceleration_56: VectorField optional,
body_acceleration_57: VectorField optional,
body_acceleration_58: VectorField optional,
body_acceleration_59: VectorField optional,
body_acceleration_60: VectorField optional,
body_acceleration_61: VectorField optional,
body_acceleration_62: VectorField optional,
body_acceleration_63: VectorField optional,
copies_acceleration: VectorField optional,
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
pose_16: Transform,
pose_17: Transform,
pose_18: Transform,
pose_19: Transform,
pose_20: Transform,
pose_21: Transform,
pose_22: Transform,
pose_23: Transform,
pose_24: Transform,
pose_25: Transform,
pose_26: Transform,
pose_27: Transform,
pose_28: Transform,
pose_29: Transform,
pose_30: Transform,
pose_31: Transform,
pose_32: Transform,
pose_33: Transform,
pose_34: Transform,
pose_35: Transform,
pose_36: Transform,
pose_37: Transform,
pose_38: Transform,
pose_39: Transform,
pose_40: Transform,
pose_41: Transform,
pose_42: Transform,
pose_43: Transform,
pose_44: Transform,
pose_45: Transform,
pose_46: Transform,
pose_47: Transform,
pose_48: Transform,
pose_49: Transform,
pose_50: Transform,
pose_51: Transform,
pose_52: Transform,
pose_53: Transform,
pose_54: Transform,
pose_55: Transform,
pose_56: Transform,
pose_57: Transform,
pose_58: Transform,
pose_59: Transform,
pose_60: Transform,
pose_61: Transform,
pose_62: Transform,
pose_63: Transform,
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
ParamDef { name: Cow::Borrowed("copy_layout"), label: "Copy Layout", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 1.0)), enum_values: &["Grid", "Pile"] },
 ],
 depth_rule: Terminal,
 composition_notes: "Connect body_N to its matching pose_N consumer and body_acceleration_N to that body's optional acceleration field. The global acceleration_field is added to every body, then the matching targeted field is added to body_N; copies_acceleration is added to every reset-latched copy. A targeted field without its matching body or copies prototype is an error and holds outputs pending. Output transforms already include authored scale: connect directly to Scene Object transform, without applying that transform twice. Optional copies creates reset-latched bodies in the same native world and writes a fixed-capacity instances array plus active_count; copy_count, copy_spacing, copy_columns, and copy_layout are numeric port-shadowed controls and apply on first build, reset, or backwards transport. Grid preserves the centered x/z arrangement; Pile uses a compact deterministic cube-root layout with bounded jitter and index-seeded rotations. Copies require uniform positive scale. State follows the transport clock; pause holds, reset/backward time restores initial poses. Preview batches use the project frame interval as a CPU work budget and retain all unprocessed ticks. Preview may lag under overload; the Physics Lag HUD shows remaining work. Export/offline renders process every pending tick. Both paths use identical fixed steps. Shape/scale/topology edits rebuild this world; contact-property edits preserve motion. Native world stays private; no mutable handle wires.",
 examples: ["PhysicsSolids", "PhysicsBoxes"],
 picker: { label: "Physics World", category: Atom },
 summary: "Simulate colliding objects together under gravity, global and per-body acceleration fields, with speed and reset controls.",
 category: Geometry3D, role: Filter,
 aliases: ["physics", "box3d", "rigid simulation"],
 boundary_reason: NonGpu,
 extra_fields: {
     simulation: RigidSimulation = RigidSimulation::default(),
     upload: InstanceUploadState = InstanceUploadState::default(),
     targeted_acceleration_fields: Vec<Option<FieldValue>> = vec![None; TARGETED_ACCELERATION_PORTS.len()],
 },
}
impl PhysicsWorldNode {
    /// CPU pose upload is an IO boundary, so the atom codegen sweep cannot warm it.
    pub(crate) fn prewarm_pipeline(device: &manifold_gpu::GpuDevice) {
        InstanceSnapshotUpload::prewarm(device);
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
        self.targeted_acceleration_fields.fill(None);
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let mut bodies = std::array::from_fn(|_| None);
        let mut body_inputs_pending = false;
        for (i, port) in BODY_PORTS.iter().enumerate() {
            if let Some(slot) = ctx.inputs.slot(port) {
                body_inputs_pending |= !ctx.inputs.slot_content_ready(slot);
                bodies[i] = ctx.inputs.rigid_body(port);
                body_inputs_pending |= bodies[i].is_none();
            }
        }
        let prototype = ctx.inputs.rigid_body("copies");
        if let Some(slot) = ctx.inputs.slot("copies") {
            body_inputs_pending |= !ctx.inputs.slot_content_ready(slot) || prototype.is_none();
        }
        let acceleration_field = ctx.inputs.vector_field("acceleration_field");
        if let Some(slot) = ctx.inputs.slot("acceleration_field") {
            body_inputs_pending |=
                !ctx.inputs.slot_content_ready(slot) || acceleration_field.is_none();
        }
        for (index, port) in TARGETED_ACCELERATION_PORTS.iter().enumerate() {
            self.targeted_acceleration_fields[index] = None;
            let matching_body_wired = if index < BODY_PORTS.len() {
                ctx.inputs.slot(BODY_PORTS[index]).is_some()
            } else {
                ctx.inputs.slot("copies").is_some()
            };
            let Some(slot) = ctx.inputs.slot(port) else {
                continue;
            };
            if !matching_body_wired {
                ctx.error(format!(
                    "Physics World `{port}` requires its matching body input to be wired"
                ));
                body_inputs_pending = true;
                continue;
            }
            if !ctx.inputs.slot_content_ready(slot) {
                body_inputs_pending = true;
                continue;
            }
            let Some(field) = ctx.inputs.vector_field(port) else {
                body_inputs_pending = true;
                continue;
            };
            self.targeted_acceleration_fields[index] = Some(field);
        }
        if body_inputs_pending {
            self.simulation.hold_pending(ctx.time.seconds);
            ctx.mark_outputs_pending();
            return;
        }
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
        let copy_layout = read_copy_layout(ctx);
        let result = self.simulation.advance_with_targeted_fields(
            bodies.clone(),
            prototype,
            copy_count,
            copy_spacing,
            copy_columns,
            copy_layout,
            gravity,
            ctx.time.seconds,
            speed,
            reset,
            acceleration_field,
            &self.targeted_acceleration_fields,
        );
        if crate::node_graph::physics::authored_sample_only() {
            if let Err(error) = result {
                ctx.error(error);
            }
            return;
        }
        if let Err(error) = result {
            ctx.error(error);
            ctx.outputs.set_scalar("physics_ms", ParamValue::Float(0.0));
        } else {
            crate::node_graph::physics_metrics::record_frame(
                self.simulation.physics_ms,
                (bodies.iter().flatten().count() + self.simulation.active_copy_count) as u32,
                self.simulation.pending_time.0 as f32,
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
        let version = self.upload.next_version();
        let retained = ctx.outputs_retained();
        let result = {
            let Some(gpu) = ctx.gpu.as_deref_mut() else {
                return;
            };
            self.upload.upload(
                gpu,
                out,
                self.simulation.active_copy_count,
                version,
                retained,
            )
        };
        if let Err(error) = result {
            ctx.error(error.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ports::PortType;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn targeted_acceleration_ports_pair_with_all_body_slots_and_copies() {
        let node = PhysicsWorldNode::new();
        assert_eq!(TARGETED_ACCELERATION_PORTS.len(), BODY_PORTS.len() + 1);
        assert_eq!(
            node.targeted_acceleration_fields.len(),
            TARGETED_ACCELERATION_PORTS.len()
        );
        for port in TARGETED_ACCELERATION_PORTS {
            let descriptor = PhysicsWorldNode::INPUTS
                .iter()
                .find(|candidate| candidate.name == port)
                .unwrap_or_else(|| panic!("missing input descriptor for {port}"));
            assert_eq!(descriptor.ty, PortType::VectorField);
            assert!(!descriptor.required);
        }
    }

    #[test]
    fn clear_state_drops_retained_targeted_fields() {
        let mut node = PhysicsWorldNode::new();
        node.targeted_acceleration_fields[0] =
            Some(FieldValue::uniform([1.0, 2.0, 3.0]).expect("finite test field"));
        node.clear_state();
        assert!(
            node.targeted_acceleration_fields
                .iter()
                .all(Option::is_none)
        );
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
            state
                .upload(&mut gpu, &output, 130, 1, true)
                .expect("instance upload");
        }
        encoder.commit_and_wait_completed();
        let first = read_instances(&output);
        assert_eq!(first[126].pos_scale[0], 126.0);
        assert_eq!(first[127].pos_scale[0], 127.0);
        assert_eq!(first[129].rot_pad[3], 129.0);
        assert_eq!(
            bytemuck::bytes_of(&first[130]),
            bytemuck::bytes_of(&ZERO_INSTANCE)
        );

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
            state
                .upload(&mut gpu, &output, 3, 2, true)
                .expect("instance upload");
        }
        encoder.commit_and_wait_completed();
        let shrunk = read_instances(&output);
        assert_eq!(shrunk[0].pos_scale[0], 100.0);
        assert_eq!(shrunk[2].pos_scale[0], 102.0);
        assert_eq!(
            bytemuck::bytes_of(&shrunk[3]),
            bytemuck::bytes_of(&ZERO_INSTANCE)
        );
        assert_eq!(
            bytemuck::bytes_of(&shrunk[MAX_COPIES - 1]),
            bytemuck::bytes_of(&ZERO_INSTANCE)
        );
    }
}
