use crate::mesh::InstanceTransform;
use crate::exec::effect_node::EffectNodeContext;
use crate::water::fluid::CoupledRigidFrame;
use crate::exec::instance_upload::InstanceSnapshotUpload;
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::water::physics::{BODY_PORTS, MAX_BODIES, MAX_COPIES, POSE_PORTS, ResolvedRigidImpulse, RigidBody, RigidSceneInputs, RigidSceneObservation, RigidSimulation};
use crate::scene::impulse::ImpulseTarget;
use crate::water::physics_events::{map_rigid_receipt, ResolvedNodeImpulse};
use crate::water::node::{PhysicsNode, PhysicsNodeRegistration};
use crate::primitive::Primitive;
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
        gpu: &mut crate::gpu::gpu_encoder::GpuEncoder<'_>,
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
     rigid_scene_observation: Option<RigidSceneObservation> = None,
     coupled_mode: bool = false,
     coupled_frame: Option<CoupledRigidFrame> = None,
     coupled_frame_ready: bool = false,
 },
}
impl PhysicsWorldNode {
    /// CPU pose upload is an IO boundary, so the atom codegen sweep cannot warm it.
    pub fn prewarm_pipeline(device: &manifold_gpu::GpuDevice) {
        InstanceSnapshotUpload::prewarm(device);
    }

    fn resolve_rigid_scene_observation(
        &self,
        ctx: &EffectNodeContext<'_, '_>,
        validate_controls: bool,
    ) -> Result<Option<RigidSceneObservation>, String> {
        let mut bodies = std::array::from_fn(|_| None);
        let mut targeted_fields: [Option<FieldValue>; MAX_BODIES + 1] =
            std::array::from_fn(|_| None);
        let mut body_inputs_pending = ctx.inputs.any_pending();
        for (i, port) in BODY_PORTS.iter().enumerate() {
            if ctx.inputs.slot(port).is_some() {
                bodies[i] = ctx.inputs.cpu_value::<RigidBody>(port);
                body_inputs_pending |= bodies[i].is_none();
            }
        }
        let prototype = ctx.inputs.cpu_value::<RigidBody>("copies");
        if ctx.inputs.slot("copies").is_some() {
            body_inputs_pending |= prototype.is_none();
        }
        let acceleration_field = ctx.inputs.cpu_value::<FieldValue>("acceleration_field");
        if ctx.inputs.slot("acceleration_field").is_some() {
            body_inputs_pending |= acceleration_field.is_none();
        }
        for (index, port) in TARGETED_ACCELERATION_PORTS.iter().enumerate() {
            let matching_body_wired = if index < BODY_PORTS.len() {
                ctx.inputs.slot(BODY_PORTS[index]).is_some()
            } else {
                ctx.inputs.slot("copies").is_some()
            };
            if ctx.inputs.slot(port).is_none() {
                continue;
            }
            if !matching_body_wired {
                return Err(format!(
                    "Physics World `{port}` requires its matching body input to be wired"
                ));
            }
            let Some(field) = ctx.inputs.cpu_value::<FieldValue>(port) else {
                body_inputs_pending = true;
                continue;
            };
            targeted_fields[index] = Some(field);
        }
        if body_inputs_pending {
            return Ok(None);
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
        if validate_controls {
            if !ctx.time.seconds.0.is_finite()
                || !speed.is_finite()
                || !(0.0..=4.0).contains(&speed)
                || !reset.is_finite()
                || gravity.iter().any(|value| !value.is_finite())
            {
                return Err("Physics: non-finite clock/control or speed outside 0–4".into());
            }
            if !copy_count.is_finite()
                || !copy_spacing.is_finite()
                || copy_spacing <= 0.0
                || !copy_columns.is_finite()
                || !copy_layout.is_finite()
            {
                return Err("Physics: copy count, spacing, columns, and layout must be finite; spacing must be positive".into());
            }
        }
        Ok(Some(RigidSceneObservation {
            inputs: RigidSceneInputs {
                bodies,
                prototype,
                copy_count,
                copy_spacing,
                copy_columns,
                layout: copy_layout,
                gravity,
                acceleration_field,
                targeted_fields,
            },
            transport: ctx.time.seconds,
            speed,
            reset,
        }))
    }

    fn publish_coupled_frame(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if crate::water::physics::authored_sample_only() {
            return;
        }
        if !self.coupled_frame_ready {
            ctx.mark_outputs_pending();
            return;
        }
        let Some(frame) = self.coupled_frame.as_ref() else {
            ctx.mark_outputs_pending();
            return;
        };
        let poses = frame.poses;
        let copies = &frame.copies;
        let body_count = self
            .rigid_scene_observation
            .as_ref()
            .map_or(0, |observation| observation.inputs.bodies.iter().flatten().count());
        crate::water::physics_metrics::record_frame(
            0.0,
            (body_count + copies.len()) as u32,
            0.0,
        );
        ctx.outputs.set_scalar("physics_ms", ParamValue::Float(0.0));
        for (port, pose) in POSE_PORTS.iter().zip(poses) {
            ctx.outputs.set_transform(port, pose);
        }
        ctx.outputs
            .set_scalar("active_count", ParamValue::Float(copies.len() as f32));
        for (dst, pose) in self.upload.data.iter_mut().zip(copies.iter().copied()) {
            *dst = InstanceTransform {
                pos_scale: [pose.pos[0], pose.pos[1], pose.pos[2], pose.scale[0]],
                rot_pad: [pose.rot_euler[0], pose.rot_euler[1], pose.rot_euler[2], 0.0],
            };
        }
        let Some(out) = ctx.outputs.array("instances") else {
            return;
        };
        let version = self.upload.next_version();
        let retained = ctx.outputs_retained();
        let result = {
            let Some(gpu) = ctx.gpu.as_deref_mut() else {
                return;
            };
            self.upload
                .upload(gpu, out, copies.len(), version, retained)
        };
        if let Err(error) = result {
            ctx.error(error.to_string());
            ctx.mark_outputs_pending();
            if let Some(gpu) = ctx.gpu.as_deref_mut() {
                gpu.merge_frame_status(crate::runtime::frame_status::FrameRenderStatus::Failed(
                    crate::runtime::frame_status::FrameRenderFailure::Simulation,
                ));
            }
        }
    }
}

impl Primitive for PhysicsWorldNode {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::exec::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "instances").then_some(MAX_COPIES as u32)
    }

    fn clear_state(&mut self) {
        self.simulation = RigidSimulation::default();
        self.rigid_scene_observation = None;
        self.coupled_frame_ready = false;
    }
    // While a body is pending the simulation clock holds instead of jumping
    // when the body lands; coupled mode publishes the liquid's frame.
    fn runs_with_pending_inputs(&self) -> bool {
        true
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if self.coupled_mode {
            self.publish_coupled_frame(ctx);
            return;
        }
        self.rigid_scene_observation = None;
        let observation = match self.resolve_rigid_scene_observation(ctx, false) {
            Ok(Some(observation)) => observation,
            Ok(None) => {
                self.simulation.hold_pending(ctx.time.seconds);
                ctx.mark_outputs_pending();
                return;
            }
            Err(error) => {
                ctx.error(error);
                self.simulation.hold_pending(ctx.time.seconds);
                ctx.mark_outputs_pending();
                return;
            }
        };
        let body_count = observation.inputs.bodies.iter().flatten().count();
        let result = self.simulation.advance_with_targeted_fields(
            observation.inputs.bodies.clone(),
            observation.inputs.prototype.clone(),
            observation.inputs.copy_count,
            observation.inputs.copy_spacing,
            observation.inputs.copy_columns,
            observation.inputs.layout,
            observation.inputs.gravity,
            observation.transport,
            observation.speed,
            observation.reset,
            observation.inputs.acceleration_field.clone(),
            &observation.inputs.targeted_fields,
        );
        if result.is_ok()
            && self
                .simulation
                .impulse_stamp(observation.transport, 0)
                .is_ok()
        {
            self.rigid_scene_observation = Some(observation);
        }
        if crate::water::physics::authored_sample_only() {
            if let Err(error) = result {
                ctx.error(error);
            }
            return;
        }
        if let Err(error) = result {
            ctx.error(error);
            ctx.outputs.set_scalar("physics_ms", ParamValue::Float(0.0));
        } else {
            crate::water::physics_metrics::record_frame(
                self.simulation.physics_ms,
                (body_count + self.simulation.active_copy_count) as u32,
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
            self.rigid_scene_observation = None;
            ctx.error(error.to_string());
        }
    }
}

impl PhysicsNode for PhysicsWorldNode {
    fn set_coupled_physics(&mut self, enabled: bool) {
        if self.coupled_mode == enabled {
            return;
        }
        self.coupled_mode = enabled;
        self.simulation = RigidSimulation::default();
        self.rigid_scene_observation = None;
        self.coupled_frame_ready = false;
        self.coupled_frame = enabled.then(|| CoupledRigidFrame {
            copies: Vec::with_capacity(MAX_COPIES),
            ..CoupledRigidFrame::default()
        });
    }

    fn rigid_scene_observation(&self) -> Option<&RigidSceneObservation> {
        self.rigid_scene_observation.as_ref()
    }

    fn physics_impulse_epoch(&self) -> Option<u64> {
        if self.coupled_mode {
            return None;
        }
        self.simulation.impulse_epoch()
    }

    fn physics_impulse_stamp(
        &self,
        transport: manifold_core::Seconds,
        sequence: u64,
    ) -> Result<manifold_physics::input::EventStamp, String> {
        if self.coupled_mode {
            return Err("Physics World coupled mode has no private impulse clock".into());
        }
        self.simulation.impulse_stamp(transport, sequence)
    }

    fn enqueue_physics_impulse(
        &mut self,
        stamp: manifold_physics::input::EventStamp,
        impulse: ResolvedNodeImpulse,
    ) -> Result<manifold_physics::TickStamp, String> {
        if self.coupled_mode {
            return Err("Physics World coupled mode routes impulses through the paired fluid worker".into());
        }
        let ImpulseTarget::Rigid(targets) = impulse.target else {
            return Err("Physics World cannot accept a fluid impulse".into());
        };
        self.simulation.enqueue_impulse(
            stamp,
            ResolvedRigidImpulse {
                field: impulse.field,
                targets,
            },
        )
    }

    fn drain_physics_impulses(
        &mut self,
        consume: &mut dyn FnMut(
            manifold_physics::input::AppliedEvent<ResolvedNodeImpulse>,
        ),
    ) {
        if self.coupled_mode {
            return;
        }
        for event in self.simulation.drain_applied_impulses() {
            map_rigid_receipt(event, consume);
        }
    }

    fn capture_coupled_rigid(&mut self, ctx: &mut EffectNodeContext<'_, '_>) -> Result<(), String> {
        self.set_coupled_physics(true);
        self.rigid_scene_observation = None;
        self.coupled_frame_ready = false;
        let Some(observation) = self.resolve_rigid_scene_observation(ctx, true)? else {
            ctx.mark_outputs_pending();
            return Ok(());
        };
        self.rigid_scene_observation = Some(observation);
        Ok(())
    }

    fn accept_coupled_rigid_frame(&mut self, frame: Option<&CoupledRigidFrame>) {
        if !self.coupled_mode {
            return;
        }
        let Some(frame) = frame else {
            self.coupled_frame_ready = false;
            return;
        };
        assert!(
            frame.copies.len() <= MAX_COPIES,
            "CoupledRigidFrame copies exceed MAX_COPIES"
        );
        let retained = self.coupled_frame.get_or_insert_with(|| CoupledRigidFrame {
            copies: Vec::with_capacity(MAX_COPIES),
            ..CoupledRigidFrame::default()
        });
        retained.stamp = frame.stamp;
        retained.poses = frame.poses;
        retained.copies.clear();
        retained.copies.extend_from_slice(&frame.copies);
        self.coupled_frame_ready = true;
    }
}

inventory::submit! { PhysicsNodeRegistration::new::<PhysicsWorldNode>() }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::backend::{Backend, MockBackend};
    use crate::bindings::{NodeInputs, NodeOutputs, Slot};
    use crate::exec::effect_node::{EffectNode, FrameTime, ParamValues};
    use crate::exec::execution_plan::ResourceId;
    use crate::scene::impulse::RigidImpulseTargets;
    use crate::water::physics::RigidBody;
    use crate::water::physics::PhysicsAuthoredSampleScope;
    use crate::water::physics_events::ResolvedNodeImpulse;
    use crate::ports::{PortType, ScalarType};
    use manifold_core::{Beats, Seconds};
    use manifold_physics::input::EventStamp;
    use std::borrow::Cow;

    #[derive(Clone, Copy)]
    enum MockFieldState {
        Complete,
        MissingTarget,
        PendingGlobal,
    }

    fn acquire_mock_wire(
        backend: &mut MockBackend,
        wire_slots: &mut Vec<(&'static str, Slot)>,
        next_resource: &mut u32,
        port: &'static str,
        ty: PortType,
    ) -> Slot {
        let slot = backend.acquire(ResourceId(*next_resource), ty, None, (0, 0));
        *next_resource += 1;
        wire_slots.push((port, slot));
        slot
    }

    fn evaluate_mock_world(
        node: &mut PhysicsWorldNode,
        transport: f64,
        body_shape: u32,
        field_state: MockFieldState,
        invalid_speed: bool,
        coupled: bool,
    ) -> Option<RigidSceneObservation> {
        let mut backend = MockBackend::new();
        let mut wire_slots: Vec<(&'static str, Slot)> = Vec::new();
        let mut pending = Vec::new();
        let mut next_resource = 0;
        let mut body = RigidBody { shape: body_shape, ..RigidBody::default() };
        body.transform.pos = [0.0, 8.0, 0.0];
        let body_slot = acquire_mock_wire(
            &mut backend,
            &mut wire_slots,
            &mut next_resource,
            "body_0",
            PortType::RigidBody,
        );
        backend.cpu_values_mut().set(body_slot, body.clone());
        let mut prototype = body.clone();
        prototype.transform.pos = [4.0, 8.0, 0.0];
        let prototype_slot = acquire_mock_wire(
            &mut backend,
            &mut wire_slots,
            &mut next_resource,
            "copies",
            PortType::RigidBody,
        );
        backend.cpu_values_mut().set(prototype_slot, prototype);

        let scalar_ty = PortType::Scalar(ScalarType::F32);
        for (port, value) in [
            ("gravity_x", 0.0),
            ("gravity_y", 0.0),
            ("gravity_z", 0.0),
            ("speed", if invalid_speed { f32::NAN } else { 1.0 }),
            ("reset", 0.0),
            ("copy_count", 2.0),
            ("copy_spacing", 2.0),
            ("copy_columns", 2.0),
            ("copy_layout", 1.0),
        ] {
            let slot = acquire_mock_wire(
                &mut backend,
                &mut wire_slots,
                &mut next_resource,
                port,
                scalar_ty,
            );
            backend.set_scalar(slot, ParamValue::Float(value));
        }

        let global_slot = acquire_mock_wire(
            &mut backend,
            &mut wire_slots,
            &mut next_resource,
            "acceleration_field",
            PortType::VectorField,
        );
        let target_slot = acquire_mock_wire(
            &mut backend,
            &mut wire_slots,
            &mut next_resource,
            "body_acceleration_0",
            PortType::VectorField,
        );
        let copy_target_slot = acquire_mock_wire(
            &mut backend,
            &mut wire_slots,
            &mut next_resource,
            "copies_acceleration",
            PortType::VectorField,
        );
        let global = FieldValue::uniform([1.0, 0.0, 0.0]).expect("finite global field");
        let target = FieldValue::uniform([2.0, 0.0, 0.0]).expect("finite target field");
        let copy_target = FieldValue::uniform([0.5, 0.0, 0.0]).expect("finite copy field");
        backend.cpu_values_mut().set(global_slot, global);
        if !matches!(field_state, MockFieldState::MissingTarget) {
            backend.cpu_values_mut().set(target_slot, target);
        }
        backend.cpu_values_mut().set(copy_target_slot, copy_target);
        if matches!(field_state, MockFieldState::PendingGlobal) {
            pending.resize(backend.slot_count() as usize, false);
            pending[global_slot.0 as usize] = true;
        }

        let mut params = ParamValues::default();
        for (name, value) in [
            ("gravity_x", ParamValue::Float(0.0)),
            ("gravity_y", ParamValue::Float(0.0)),
            ("gravity_z", ParamValue::Float(0.0)),
            ("speed", ParamValue::Float(1.0)),
            ("reset", ParamValue::Float(0.0)),
            ("copy_count", ParamValue::Float(2.0)),
            ("copy_spacing", ParamValue::Float(2.0)),
            ("copy_columns", ParamValue::Float(2.0)),
            ("copy_layout", ParamValue::Enum(0)),
        ] {
            params.insert(Cow::Borrowed(name), value);
        }

        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut render_mode_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let inputs = NodeInputs::new(&wire_slots, &backend, &[]).with_pending(&pending);
        let outputs = NodeOutputs::new(
            &[],
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut render_mode_scratch,
            &mut object_scratch,
        );
        let time = FrameTime {
            beats: Beats(transport),
            seconds: Seconds(transport),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        let graph_node: &mut dyn EffectNode = node;
        if coupled {
            let native = crate::water::node::get_mut(graph_node)
                .expect("PhysicsWorldNode has a native PhysicsNode registration");
            let _ = native.capture_coupled_rigid(&mut ctx);
        } else {
            graph_node.evaluate(&mut ctx);
        }
        crate::water::node::get(graph_node)
            .expect("PhysicsWorldNode has a native PhysicsNode registration")
            .rigid_scene_observation()
            .cloned()
    }

    fn evaluate_coupled_outputs(
        node: &mut PhysicsWorldNode,
    ) -> (Option<crate::scene::transform::Transform>, Option<ParamValue>) {
        let mut backend = MockBackend::new();
        let pose_slot = backend.acquire(
            ResourceId(10_000),
            PortType::Transform,
            None,
            (0, 0),
        );
        let active_slot = backend.acquire(
            ResourceId(10_001),
            PortType::Scalar(ScalarType::F32),
            None,
            (0, 0),
        );
        let output_bindings: &[(&'static str, Slot)] =
            &[("pose_0", pose_slot), ("active_count", active_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut render_mode_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let inputs = NodeInputs::new(&[], &backend, &[]);
        let outputs = NodeOutputs::new(
            output_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut render_mode_scratch,
            &mut object_scratch,
        );
        let params = ParamValues::default();
        let time = FrameTime {
            beats: Beats::ZERO,
            seconds: Seconds::ZERO,
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        let graph_node: &mut dyn EffectNode = node;
        graph_node.evaluate(&mut ctx);
        (
            transform_scratch
                .into_iter()
                .find(|(slot, _)| *slot == pose_slot)
                .map(|(_, value)| value),
            scalar_scratch
                .into_iter()
                .find(|(slot, _)| *slot == active_slot)
                .map(|(_, value)| value),
        )
    }

    #[test]
    fn clear_state_drops_retained_rigid_scene_observation() {
        let mut node = PhysicsWorldNode::new();
        node.rigid_scene_observation = Some(RigidSceneObservation {
            inputs: RigidSceneInputs {
                targeted_fields: std::array::from_fn(|index| {
                    (index == 0)
                        .then(|| FieldValue::uniform([1.0, 2.0, 3.0]).expect("finite test field"))
                }),
                ..RigidSceneInputs::default()
            },
            transport: Seconds::ZERO,
            speed: 1.0,
            reset: 0.0,
        });
        EffectNode::clear_state(&mut node);
        assert!(node.rigid_scene_observation.is_none());
    }

    #[test]
    fn effect_node_observation_captures_fields_controls_and_single_native_force() {
        let mut node = PhysicsWorldNode::new();
        let first = evaluate_mock_world(
            &mut node,
            0.0,
            1,
            MockFieldState::Complete,
            false,
            false,
        )
        .expect("initial complete rigid observation");
        let second = evaluate_mock_world(
            &mut node,
            1.0 / 60.0,
            1,
            MockFieldState::Complete,
            false,
            false,
        )
        .expect("fixed tick complete rigid observation");

        assert_eq!(first.inputs.bodies[0].as_ref().unwrap().shape, 1);
        assert_eq!(first.inputs.prototype.as_ref().unwrap().shape, 1);
        assert_eq!(first.inputs.copy_count, 2.0);
        assert_eq!(first.inputs.copy_spacing, 2.0);
        assert_eq!(first.inputs.copy_columns, 2.0);
        assert_eq!(first.inputs.layout, 1.0, "wired layout shadows the enum parameter");
        assert_eq!(first.inputs.gravity, [0.0; 3]);
        assert_eq!(first.inputs.acceleration_field, Some(FieldValue::uniform([1.0, 0.0, 0.0]).unwrap()));
        assert_eq!(first.inputs.targeted_fields[0], Some(FieldValue::uniform([2.0, 0.0, 0.0]).unwrap()));
        assert_eq!(first.inputs.targeted_fields[MAX_BODIES], Some(FieldValue::uniform([0.5, 0.0, 0.0]).unwrap()));
        assert_eq!(second.transport, Seconds(1.0 / 60.0));
        assert_eq!(second.speed, 1.0);
        assert_eq!(second.reset, 0.0);

        let mut expected = RigidSimulation::default();
        for (observation, now) in [(&first, Seconds::ZERO), (&second, Seconds(1.0 / 60.0))] {
            expected
                .advance_with_targeted_fields(
                    observation.inputs.bodies.clone(),
                    observation.inputs.prototype.clone(),
                    observation.inputs.copy_count,
                    observation.inputs.copy_spacing,
                    observation.inputs.copy_columns,
                    observation.inputs.layout,
                    observation.inputs.gravity,
                    now,
                    observation.speed,
                    observation.reset,
                    observation.inputs.acceleration_field.clone(),
                    &observation.inputs.targeted_fields,
                )
                .expect("ordinary rigid advancement baseline");
        }
        let position = node.simulation.poses[0].pos[0];
        assert!(position > 0.0, "captured acceleration must move the native body");
        assert_eq!(node.simulation.poses[0], expected.poses[0]);
        let handle = node
            .simulation
            .native_handles()
            .0[0]
            .expect("native dynamic body handle");
        let actual_velocity = node
            .simulation
            .native_world()
            .expect("native world")
            .linear_velocity(handle)
            .expect("native velocity");
        let expected_handle = expected
            .native_handles()
            .0[0]
            .expect("baseline dynamic body handle");
        let expected_velocity = expected
            .native_world()
            .expect("baseline native world")
            .linear_velocity(expected_handle)
            .expect("baseline velocity");
        assert_eq!(actual_velocity, expected_velocity, "captured force should be applied once");
    }

    #[test]
    fn effect_node_observation_clears_for_unavailable_or_invalid_inputs() {
        for (field_state, invalid_speed) in [
            (MockFieldState::MissingTarget, false),
            (MockFieldState::PendingGlobal, false),
            (MockFieldState::Complete, true),
        ] {
            let mut node = PhysicsWorldNode::new();
            assert!(evaluate_mock_world(
                &mut node,
                0.0,
                1,
                MockFieldState::Complete,
                false,
                false,
            )
            .is_some());
            assert!(evaluate_mock_world(
                &mut node,
                1.0 / 60.0,
                1,
                field_state,
                invalid_speed,
                false,
            )
            .is_none());
        }
    }

    #[test]
    fn authored_withheld_topology_sample_does_not_publish_observation() {
        let mut node = PhysicsWorldNode::new();
        assert!(evaluate_mock_world(
            &mut node,
            0.0,
            1,
            MockFieldState::Complete,
            false,
            false,
        )
        .is_some());
        let _scope = PhysicsAuthoredSampleScope::new();
        assert!(evaluate_mock_world(
            &mut node,
            0.0,
            2,
            MockFieldState::Complete,
            false,
            false,
        )
        .is_none());
    }

    #[test]
    fn coupled_capture_has_no_native_owner_and_clears_pending_or_invalid_observations() {
        let mut node = PhysicsWorldNode::new();
        let observation = evaluate_mock_world(
            &mut node,
            0.0,
            1,
            MockFieldState::Complete,
            false,
            true,
        )
        .expect("coupled capture observation");
        assert_eq!(observation.transport, Seconds::ZERO);
        assert!(node.simulation.native_world().is_none());
        let graph_node: &mut dyn EffectNode = &mut node;
        let native = crate::water::node::get(graph_node)
            .expect("PhysicsWorldNode has a native PhysicsNode registration");
        assert!(native.physics_impulse_epoch().is_none());
        assert!(native
            .physics_impulse_stamp(Seconds::ZERO, 0)
            .is_err());

        assert!(evaluate_mock_world(
            &mut node,
            1.0 / 60.0,
            1,
            MockFieldState::PendingGlobal,
            false,
            true,
        )
        .is_none());
        assert!(node.rigid_scene_observation.is_none());
        assert!(evaluate_mock_world(
            &mut node,
            1.0 / 60.0,
            1,
            MockFieldState::Complete,
            true,
            true,
        )
        .is_none());
        assert!(node.rigid_scene_observation.is_none());
    }

    #[test]
    fn coupled_pending_capture_keeps_reserved_copies_and_publishes_nothing() {
        let mut node = PhysicsWorldNode::new();
        PhysicsNode::set_coupled_physics(&mut node, true);
        let capacity = node
            .coupled_frame
            .as_ref()
            .expect("coupled storage")
            .copies
            .capacity();
        assert_eq!(capacity, MAX_COPIES);
        assert!(!node.coupled_frame_ready);
        assert_eq!(evaluate_coupled_outputs(&mut node), (None, None));
        assert!(evaluate_mock_world(
            &mut node,
            0.0,
            1,
            MockFieldState::PendingGlobal,
            false,
            true,
        )
        .is_none());
        let retained = node.coupled_frame.as_ref().expect("retained storage");
        assert_eq!(retained.copies.capacity(), capacity);
        assert!(!node.coupled_frame_ready);
        assert_eq!(evaluate_coupled_outputs(&mut node), (None, None));
    }

    #[test]
    fn coupled_accept_publishes_supplied_pose_and_retained_copies() {
        let mut node = PhysicsWorldNode::new();
        evaluate_mock_world(
            &mut node,
            0.0,
            1,
            MockFieldState::Complete,
            false,
            true,
        )
        .expect("coupled capture observation");
        let mut frame = CoupledRigidFrame {
            stamp: manifold_physics::TickStamp { epoch: 9, tick: 4 },
            ..CoupledRigidFrame::default()
        };
        frame.poses[0].pos = [7.0, 8.0, 9.0];
        frame.copies.push(crate::scene::transform::Transform {
            pos: [2.0, 3.0, 4.0],
            ..crate::scene::transform::Transform::default()
        });
        let graph_node: &mut dyn EffectNode = &mut node;
        crate::water::node::get_mut(graph_node)
            .expect("PhysicsWorldNode has a native PhysicsNode registration")
            .accept_coupled_rigid_frame(Some(&frame));
        let (pose, active_count) = evaluate_coupled_outputs(&mut node);
        assert_eq!(pose.expect("published pose").pos, [7.0, 8.0, 9.0]);
        assert_eq!(active_count, Some(ParamValue::Float(1.0)));
        let retained = node.coupled_frame.as_ref().expect("retained coupled frame");
        assert_eq!(retained.stamp, frame.stamp);
        assert_eq!(retained.copies, frame.copies);
        assert!(node.simulation.native_world().is_none());
    }

    #[test]
    fn coupled_clear_state_retains_grouped_mode_without_private_owner() {
        let mut node = PhysicsWorldNode::new();
        evaluate_mock_world(
            &mut node,
            0.0,
            1,
            MockFieldState::Complete,
            false,
            true,
        )
        .expect("coupled capture observation");
        let mut frame = CoupledRigidFrame::default();
        frame.copies.push(crate::scene::transform::Transform::default());
        PhysicsNode::accept_coupled_rigid_frame(&mut node, Some(&frame));
        EffectNode::clear_state(&mut node);
        assert!(node.coupled_mode);
        assert!(node.rigid_scene_observation.is_none());
        assert!(node.coupled_frame.is_some());
        assert!(!node.coupled_frame_ready);
        assert!(node.simulation.native_world().is_none());
    }

    #[test]
    fn physics_world_effect_node_impulse_dispatch_preserves_receipt_and_retry_sequence() {
        use crate::exec::effect_node::EffectNode;
        let mut node = PhysicsWorldNode::new();
        let mut bodies: [Option<RigidBody>; crate::water::physics::MAX_BODIES] =
            std::array::from_fn(|_| None);
        bodies[0] = Some(RigidBody::default());
        node.simulation
            .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .expect("native rigid world initialization");
        let epoch = PhysicsNode::physics_impulse_epoch(&node).expect("native impulse epoch");
        let stamp = EventStamp {
            epoch,
            time: Seconds::ZERO,
            sequence: 17,
        };
        let field = FieldValue::uniform([1.25, -2.5, 3.75]).expect("finite impulse field");
        let wrong = ResolvedNodeImpulse {
            field: field.clone(),
            target: ImpulseTarget::Fluid,
        };
        let valid = ResolvedNodeImpulse {
            field: field.clone(),
            target: ImpulseTarget::Rigid(RigidImpulseTargets {
                bodies: 1,
                copies: false,
            }),
        };

        {
            let graph_node: &mut dyn EffectNode = &mut node;
            let native = crate::water::node::get_mut(graph_node)
                .expect("PhysicsWorldNode has a native PhysicsNode registration");
            assert!(native
                .enqueue_physics_impulse(stamp, wrong)
                .expect_err("wrong target must be rejected before queue admission")
                .contains("fluid"));
            assert_eq!(
                native
                    .enqueue_physics_impulse(stamp, valid)
                    .expect("same producer sequence must remain valid"),
                manifold_physics::TickStamp { epoch, tick: 0 }
            );
        }

        node.simulation
            .advance(bodies, [0.0; 3], Seconds(1.0 / 60.0), 1.0, 0.0)
            .expect("native rigid tick");

        let mut receipts = Vec::new();
        let graph_node: &mut dyn EffectNode = &mut node;
        crate::water::node::get_mut(graph_node)
            .expect("PhysicsWorldNode has a native PhysicsNode registration")
            .drain_physics_impulses(&mut |event| receipts.push(event));
        assert_eq!(receipts.len(), 1);
        let receipt = receipts.pop().expect("one rigid receipt");
        assert_eq!(receipt.source, stamp);
        assert_eq!(receipt.applied, manifold_physics::TickStamp { epoch, tick: 0 });
        assert_eq!(receipt.lateness, Seconds::ZERO);
        assert_eq!(receipt.value.field, field);
        assert_eq!(
            receipt.value.target,
            ImpulseTarget::Rigid(RigidImpulseTargets {
                bodies: 1,
                copies: false,
            })
        );
        crate::water::node::get_mut(graph_node)
            .expect("PhysicsWorldNode has a native PhysicsNode registration")
            .drain_physics_impulses(&mut |_| panic!("receipt drained twice"));
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::gpu::gpu_encoder::GpuEncoder;

    fn read_instances(buffer: &manifold_gpu::GpuBuffer) -> Vec<InstanceTransform> {
        let ptr = buffer.mapped_ptr().expect("shared output buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const InstanceTransform, MAX_COPIES).to_vec() }
    }

    #[test]
    fn instance_upload_crosses_inline_chunk_and_zeroes_shrunk_tail() {
        let device = manifold_gpu::testkit::test_device();
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

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
