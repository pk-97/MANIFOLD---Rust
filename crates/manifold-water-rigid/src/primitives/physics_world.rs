//! `node.physics_world`: one shared Box3D rigid world. Its `bodies` and
//! `copy_groups` params size its port families when the graph is built or
//! the counts are edited, never per tick: `body_k`, `body_acceleration_k`
//! and `pose_k` for each body, and `copies_k`, `copies_acceleration_k`,
//! `instances_k` and `active_count_k` for each copy group.
//!
//! Composition: connect body_k to its matching pose_k consumer and
//! body_acceleration_k to that body's optional acceleration field. The
//! global acceleration_field is added to every body, then the matching
//! targeted field; copies_acceleration_k is added to every copy of group k.
//! A targeted field without its matching body or copies body is an error and
//! holds outputs pending. Output transforms already include authored scale.
//! Each copies body carries its own spawner (count, layout, spacing,
//! columns, turn); an edit lays its copies out at the next tick start. Its
//! group writes a fixed-capacity instances array plus active_count. The
//! groups share the world's copy budget: later groups make what fits.

use std::borrow::Cow;

use manifold_core::scene_impulse::ImpulseTarget;
use manifold_core::scene_schema::SPEED_RANGE;
use manifold_core::scene_surface::{
    PHYSICS_WORLD_DEFAULT_BODIES, PHYSICS_WORLD_DEFAULT_COPY_GROUPS,
};
use manifold_node_engine::bindings::NodeInputs;
use manifold_node_engine::exec::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, ParamValues,
};
use manifold_node_engine::exec::instance_upload::InstanceSnapshotUpload;
use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::ports::{
    NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
};
use manifold_node_engine::scene::transform::Transform;
use manifold_physics::FieldValue;

use crate::coupled_frame::CoupledRigidFrame;
use crate::node::{PhysicsNode, PhysicsNodeRegistration};
use crate::physics::{
    MAX_COPIES, ResolvedRigidImpulse, RigidSceneInputs, RigidSceneObservation, RigidSimulation,
};
use crate::physics_events::{ResolvedNodeImpulse, map_rigid_receipt};

pub const PHYSICS_WORLD_TYPE_ID: &str = "node.physics_world";

const ZERO_INSTANCE: InstanceTransform = InstanceTransform {
    pos_scale: [0.0; 4],
    rot_pad: [0.0; 4],
};

/// The world's port names, made when its counts grow, so a tick never
/// formats one. They grow with the world and never shrink.
#[derive(Default)]
struct PortNames {
    body: Vec<String>,
    body_acceleration: Vec<String>,
    pose: Vec<String>,
    copies: Vec<String>,
    copies_acceleration: Vec<String>,
    instances: Vec<String>,
    active_count: Vec<String>,
}

impl PortNames {
    /// Make sure every name of `bodies` bodies and `groups` groups exists.
    fn grow(&mut self, bodies: usize, groups: usize) {
        let grow = |names: &mut Vec<String>, prefix: &str, count: usize| {
            for index in names.len()..count {
                names.push(format!("{prefix}{index}"));
            }
        };
        grow(&mut self.body, "body_", bodies);
        grow(&mut self.body_acceleration, "body_acceleration_", bodies);
        grow(&mut self.pose, "pose_", bodies);
        grow(&mut self.copies, "copies_", groups);
        grow(
            &mut self.copies_acceleration,
            "copies_acceleration_",
            groups,
        );
        grow(&mut self.instances, "instances_", groups);
        grow(&mut self.active_count, "active_count_", groups);
    }
}

const SCALAR_INPUTS: [&str; 7] = [
    "gravity_x",
    "gravity_y",
    "gravity_z",
    "speed",
    "reset",
    "air_drag",
    "air_resistance",
];

static PARAMS: [ParamDef; 9] = [
    ParamDef {
        name: Cow::Borrowed("gravity_x"),
        label: "Gravity X",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((-20.0, 20.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("gravity_y"),
        label: "Gravity Y",
        ty: ParamType::Float,
        default: ParamValue::Float(-9.81),
        range: Some((-20.0, 20.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("gravity_z"),
        label: "Gravity Z",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((-20.0, 20.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("speed"),
        label: "Speed",
        ty: ParamType::Float,
        default: ParamValue::Float(1.0),
        range: Some((0.0, 4.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("reset"),
        label: "Reset",
        ty: ParamType::Trigger,
        default: ParamValue::Float(0.0),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("air_drag"),
        label: "Air Drag",
        ty: ParamType::Float,
        default: ParamValue::Float(1.0),
        range: Some((0.0, 4.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("air_resistance"),
        label: "Air Resistance",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((0.0, 4.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("bodies"),
        label: "Bodies",
        ty: ParamType::Int,
        default: ParamValue::Float(PHYSICS_WORLD_DEFAULT_BODIES as f32),
        range: None,
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("copy_groups"),
        label: "Copy Groups",
        ty: ParamType::Int,
        default: ParamValue::Float(PHYSICS_WORLD_DEFAULT_COPY_GROUPS as f32),
        range: None,
        enum_values: &[],
    },
];

fn port(name: impl Into<Cow<'static, str>>, ty: PortType, kind: PortKind) -> NodePort {
    NodePort {
        name: name.into(),
        ty,
        kind,
        required: false,
    }
}

fn instances_type() -> PortType {
    PortType::Array(manifold_node_engine::ports::ArrayType::of_known::<
        InstanceTransform,
    >())
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
        gpu: &mut manifold_node_engine::gpu::gpu_encoder::GpuEncoder<'_>,
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

    /// Upload `poses` to `port` if a consumer reads it.
    fn publish(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        port: &str,
        poses: &[Transform],
    ) -> Result<(), &'static str> {
        for (dst, pose) in self.data.iter_mut().zip(poses) {
            *dst = InstanceTransform {
                pos_scale: [pose.pos[0], pose.pos[1], pose.pos[2], pose.scale[0]],
                rot_pad: [pose.rot_euler[0], pose.rot_euler[1], pose.rot_euler[2], 0.0],
            };
        }
        let Some(out) = ctx.outputs.array(port) else {
            return Ok(());
        };
        let version = self.next_version();
        let retained = ctx.outputs_retained();
        let Some(gpu) = ctx.gpu.as_deref_mut() else {
            return Ok(());
        };
        self.upload(gpu, out, poses.len(), version, retained)
            .map(|_| ())
    }
}

pub struct PhysicsWorldNode {
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
    bodies: usize,
    groups: usize,
    pub simulation: RigidSimulation,
    /// One per copy group.
    uploads: Vec<InstanceUploadState>,
    /// The latest resolved inputs, refilled in place every frame.
    observation: RigidSceneObservation,
    /// Whether `observation` is the latest successful evaluation.
    observation_valid: bool,
    coupled_mode: bool,
    coupled_frame: Option<CoupledRigidFrame>,
    coupled_frame_ready: bool,
    names: PortNames,
    /// The refusals the card shows, as world input and reason, rebuilt only
    /// when `refusals_shown` stops matching the world's.
    refusal_rows: Vec<manifold_core::physics_members::BodyRefusalRow>,
    refusals_shown: Vec<crate::physics::BodyRefusal>,
}

impl Default for PhysicsWorldNode {
    fn default() -> Self {
        Self::new()
    }
}

impl PhysicsWorldNode {
    pub fn new() -> Self {
        let mut node = Self {
            inputs: Vec::new(),
            outputs: Vec::new(),
            bodies: 0,
            groups: 0,
            simulation: RigidSimulation::default(),
            uploads: Vec::new(),
            observation: RigidSceneObservation {
                inputs: RigidSceneInputs::with_counts(0, 0),
                transport: manifold_core::PlayedTime::ZERO,
                speed: 1.0,
                reset: 0.0,
            },
            observation_valid: false,
            coupled_mode: false,
            coupled_frame: None,
            coupled_frame_ready: false,
            names: PortNames::default(),
            refusal_rows: Vec::new(),
            refusals_shown: Vec::new(),
        };
        node.rebuild(
            PHYSICS_WORLD_DEFAULT_BODIES as usize,
            PHYSICS_WORLD_DEFAULT_COPY_GROUPS as usize,
        );
        node
    }

    /// Bring the card's refusal rows in line with the world's refusals. A
    /// standing refusal allocates nothing; only a change rebuilds the rows.
    fn sync_refusal_rows(&mut self) {
        let refusals = self.simulation.refusals();
        if refusals == self.refusals_shown.as_slice() {
            return;
        }
        self.refusals_shown.clear();
        self.refusals_shown.extend_from_slice(refusals);
        self.refusal_rows.clear();
        self.refusal_rows.extend(refusals.iter().map(|refusal| {
            let input = match refusal.member {
                crate::physics::Member::Body(k) => format!("body_{k}"),
                crate::physics::Member::Copies(k) => format!("copies_{k}"),
            };
            manifold_core::physics_members::BodyRefusalRow {
                input: std::sync::Arc::from(input),
                param: refusal.reason.param(),
                reason: std::sync::Arc::from(refusal.reason.to_string()),
            }
        }));
    }

    /// Body ports this world exposes.
    pub fn body_count(&self) -> usize {
        self.bodies
    }

    /// Copy groups this world exposes.
    pub fn copy_group_count(&self) -> usize {
        self.groups
    }

    /// Size the port families and every per-body and per-group buffer for
    /// `bodies` bodies and `groups` copy groups.
    fn rebuild(&mut self, bodies: usize, groups: usize) {
        self.names.grow(bodies, groups);
        let names = &self.names;
        let field = PortType::VectorField;
        let scalar = PortType::Scalar(ScalarType::F32);
        self.inputs.clear();
        self.inputs.extend(
            names.body[..bodies]
                .iter()
                .map(|name| port(name.clone(), PortType::RigidBody, PortKind::Input)),
        );
        self.inputs.extend(
            names.copies[..groups]
                .iter()
                .map(|name| port(name.clone(), PortType::RigidBody, PortKind::Input)),
        );
        self.inputs.extend(
            SCALAR_INPUTS
                .iter()
                .map(|name| port(*name, scalar, PortKind::Input)),
        );
        self.inputs
            .push(port("acceleration_field", field, PortKind::Input));
        self.inputs.extend(
            names.body_acceleration[..bodies]
                .iter()
                .map(|name| port(name.clone(), field, PortKind::Input)),
        );
        self.inputs.extend(
            names.copies_acceleration[..groups]
                .iter()
                .map(|name| port(name.clone(), field, PortKind::Input)),
        );
        self.outputs.clear();
        self.outputs.extend(
            names.pose[..bodies]
                .iter()
                .map(|name| port(name.clone(), PortType::Transform, PortKind::Output)),
        );
        for group in 0..groups {
            self.outputs.push(port(
                names.instances[group].clone(),
                instances_type(),
                PortKind::Output,
            ));
            self.outputs.push(port(
                names.active_count[group].clone(),
                scalar,
                PortKind::Output,
            ));
        }
        self.outputs
            .push(port("physics_ms", scalar, PortKind::Output));
        self.bodies = bodies;
        self.groups = groups;
        self.uploads
            .resize_with(groups, InstanceUploadState::default);
        self.observation.inputs.resize(bodies, groups);
        self.observation_valid = false;
        if let Some(frame) = self.coupled_frame.as_mut() {
            frame.poses.reserve(bodies);
        }
    }

    /// CPU pose upload is an IO boundary, so the atom codegen sweep cannot warm it.
    pub fn prewarm_pipeline(device: &manifold_gpu::GpuDevice) {
        InstanceSnapshotUpload::prewarm(device);
    }

    /// Refill `self.observation` from this frame's inputs. A wired input
    /// still pending keeps its last value, and one never seen is absent: a
    /// body whose shape is still being built joins when it is ready, and the
    /// world never pauses for it. Speed clamps to its range.
    fn resolve_rigid_scene_observation(
        &mut self,
        ctx: &EffectNodeContext<'_, '_>,
        validate_controls: bool,
    ) -> Result<(), String> {
        let names = &self.names;
        let inputs = &mut self.observation.inputs;
        for (body, port) in inputs
            .bodies
            .iter_mut()
            .zip(&names.body)
            .chain(inputs.groups.iter_mut().zip(&names.copies))
        {
            latest(body, &ctx.inputs, port);
        }
        latest(
            &mut inputs.acceleration_field,
            &ctx.inputs,
            "acceleration_field",
        );
        let bodies = self.bodies;
        let targeted_ports = names.body_acceleration[..bodies]
            .iter()
            .zip(&names.body)
            .chain(
                names.copies_acceleration[..self.groups]
                    .iter()
                    .zip(&names.copies),
            );
        for (field, (port, matching)) in inputs.targeted_fields.iter_mut().zip(targeted_ports) {
            if ctx.inputs.slot(port).is_none() {
                *field = None;
                continue;
            }
            if ctx.inputs.slot(matching).is_none() {
                return Err(format!(
                    "Physics World `{port}` requires its matching body input to be wired"
                ));
            }
            latest(field, &ctx.inputs, port);
        }
        // Every field as the bodies feel it: the air it carries scaled by Air
        // Drag (0 ignores the air). The World's still air reaches every body
        // once, through the global field (D-114).
        let air_drag = ctx.scalar_or_param("air_drag", 1.0);
        let air_resistance = ctx.scalar_or_param("air_resistance", 0.0);
        let felt = |field: FieldValue| {
            field
                .with_air_drag(air_drag)
                .map_err(|error| format!("Physics World: {error}"))
        };
        let acceleration_field = FieldValue::felt_in_still_air(
            inputs.acceleration_field.as_ref(),
            air_resistance,
            air_drag,
        )
        .map_err(|error| format!("Physics World: {error}"))?;
        inputs.acceleration_field = acceleration_field;
        for field in &mut inputs.targeted_fields {
            if let Some(value) = field.take() {
                *field = Some(felt(value)?);
            }
        }
        inputs.gravity = [
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity_y", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
        ];
        let speed = ctx
            .scalar_or_param("speed", 1.0)
            .clamp(SPEED_RANGE.0, SPEED_RANGE.1);
        let reset = ctx.scalar_or_param("reset", 0.0);
        if validate_controls
            && (!ctx.played().is_finite()
                || !speed.is_finite()
                || !reset.is_finite()
                || inputs.gravity.iter().any(|value| !value.is_finite()))
        {
            return Err("Physics: non-finite clock or control".into());
        }
        self.observation.transport = ctx.played();
        self.observation.speed = speed;
        self.observation.reset = reset;
        Ok(())
    }

    fn publish_coupled_frame(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if ctx.sim_step.authored_sample_only {
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
        let body_count = if self.observation_valid {
            self.observation.inputs.bodies.iter().flatten().count()
        } else {
            0
        };
        ctx.sim_metrics
            .record_frame(0.0, (body_count + frame.copies.len()) as u32, 0.0);
        ctx.outputs.set_scalar("physics_ms", ParamValue::Float(0.0));
        let names = &self.names;
        for (port, pose) in names.pose.iter().zip(&frame.poses) {
            ctx.outputs.set_transform(port, *pose);
        }
        for (group, upload) in self.uploads.iter_mut().enumerate() {
            let copies = frame.group(group);
            ctx.outputs.set_scalar(
                &names.active_count[group],
                ParamValue::Float(copies.len() as f32),
            );
            if let Err(error) = upload.publish(ctx, &names.instances[group], copies) {
                ctx.error(error.to_string());
                ctx.mark_outputs_pending();
                if let Some(gpu) = ctx.gpu.as_deref_mut() {
                    gpu.merge_frame_status(manifold_node_engine::runtime::frame_status::FrameRenderStatus::Failed(
                        manifold_node_engine::runtime::frame_status::FrameRenderFailure::Simulation,
                    ));
                }
                return;
            }
        }
    }

    fn reset_coupled_storage(&mut self) {
        self.coupled_frame = Some(CoupledRigidFrame {
            poses: Vec::with_capacity(self.bodies),
            copies: Vec::with_capacity(MAX_COPIES),
            group_copies: Vec::with_capacity(self.groups),
            ..CoupledRigidFrame::default()
        });
    }
}

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: std::sync::OnceLock<EffectNodeType> = std::sync::OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(PHYSICS_WORLD_TYPE_ID))
}

fn count_param(params: &ParamValues, name: &str, default: u32) -> usize {
    params
        .get(name)
        .and_then(|value| value.as_scalar())
        .filter(|value| value.is_finite())
        .map_or(default as usize, |value| value.round().max(0.0) as usize)
}

impl EffectNode for PhysicsWorldNode {
    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }

    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }

    fn outputs(&self) -> &[NodeOutput] {
        &self.outputs
    }

    fn parameters(&self) -> &[ParamDef] {
        &PARAMS
    }

    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }

    fn boundary_reason(&self) -> Option<manifold_node_engine::freeze::classify::BoundaryReason> {
        Some(manifold_node_engine::freeze::classify::BoundaryReason::NonGpu)
    }

    /// `bodies` and `copy_groups` size the live port lists, as many as the
    /// scene has: neither is capped.
    fn param_contract(&self, param_name: &str) -> Option<manifold_core::effects::RangeContract> {
        let min = match param_name {
            "bodies" => 1.0,
            "copy_groups" => 0.0,
            _ => return None,
        };
        Some(manifold_core::effects::RangeContract {
            min: Some(min),
            max: None,
            reason: manifold_core::effects::RangeReason::Count,
        })
    }

    fn reconfigure(&mut self, params: &ParamValues) {
        let bodies = count_param(params, "bodies", PHYSICS_WORLD_DEFAULT_BODIES).max(1);
        let groups = count_param(params, "copy_groups", PHYSICS_WORLD_DEFAULT_COPY_GROUPS);
        if bodies != self.bodies || groups != self.groups {
            self.rebuild(bodies, groups);
        }
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        self.names.instances[..self.groups]
            .iter()
            .any(|name| name == port_name)
            .then_some(MAX_COPIES as u32)
    }

    fn clear_state(&mut self) {
        self.simulation = RigidSimulation::default();
        self.sync_refusal_rows();
        self.observation_valid = false;
        self.coupled_frame_ready = false;
    }

    // A pending body keeps its last description and the world keeps
    // stepping; coupled mode publishes the liquid's frame.
    fn runs_with_pending_inputs(&self) -> bool {
        true
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.simulation.set_step(ctx.sim_step);
        if self.coupled_mode {
            self.publish_coupled_frame(ctx);
            return;
        }
        self.observation_valid = false;
        if let Err(error) = self.resolve_rigid_scene_observation(ctx, false) {
            ctx.error(error);
            ctx.mark_outputs_pending();
            return;
        }
        let observation = &self.observation;
        let body_count = observation.inputs.bodies.iter().flatten().count();
        let result = self.simulation.advance_scene(
            &observation.inputs,
            observation.transport,
            observation.speed,
            observation.reset,
        );
        self.sync_refusal_rows();
        ctx.sim_metrics.merge(&self.simulation.take_metrics());
        if result.is_ok()
            && self
                .simulation
                .impulse_stamp(self.observation.transport, 0)
                .is_ok()
        {
            self.observation_valid = true;
        }
        if ctx.sim_step.authored_sample_only {
            if let Err(error) = result {
                ctx.error(error);
            }
            return;
        }
        if let Err(error) = result {
            ctx.error(error);
            ctx.outputs.set_scalar("physics_ms", ParamValue::Float(0.0));
        } else {
            ctx.sim_metrics.record_frame(
                self.simulation.physics_ms,
                (body_count + self.simulation.total_active_copies()) as u32,
                self.simulation.pending_time.0 as f32,
            );
            ctx.outputs
                .set_scalar("physics_ms", ParamValue::Float(self.simulation.physics_ms));
        }
        let names = &self.names;
        for (port, pose) in names.pose.iter().zip(&self.simulation.poses) {
            ctx.outputs.set_transform(port, *pose);
        }
        for (group, upload) in self.uploads.iter_mut().enumerate() {
            let copies = self.simulation.group_poses(group);
            ctx.outputs.set_scalar(
                &names.active_count[group],
                ParamValue::Float(copies.len() as f32),
            );
            if let Err(error) = upload.publish(ctx, &names.instances[group], copies) {
                self.observation_valid = false;
                ctx.error(error.to_string());
                return;
            }
        }
    }
}

/// Take a wired input's value when it has one; a wired input still pending
/// keeps its last value, and an unwired one is absent.
fn latest<T: Clone + Send + 'static>(slot: &mut Option<T>, inputs: &NodeInputs<'_>, port: &str) {
    if inputs.slot(port).is_none() {
        *slot = None;
    } else if !inputs.port_pending(port)
        && let Some(value) = inputs.cpu_value::<T>(port)
    {
        *slot = Some(value);
    }
}

impl PhysicsNode for PhysicsWorldNode {
    fn body_refusals(&self) -> &[manifold_core::physics_members::BodyRefusalRow] {
        &self.refusal_rows
    }

    // A coupled world steps inside its liquid, on the liquid's samples.
    fn request_physics_samples(
        &mut self,
        from: manifold_core::PlayedTime,
        until: manifold_core::PlayedTime,
        out: &mut Vec<manifold_core::PlayedTime>,
    ) {
        if !self.coupled_mode {
            self.simulation.request_samples(from, until, out);
        }
    }

    fn set_coupled_physics(&mut self, enabled: bool) {
        if self.coupled_mode == enabled {
            return;
        }
        self.coupled_mode = enabled;
        self.simulation = RigidSimulation::default();
        self.sync_refusal_rows();
        self.observation_valid = false;
        self.coupled_frame_ready = false;
        self.coupled_frame = None;
        if enabled {
            self.reset_coupled_storage();
        }
    }

    fn rigid_scene_observation(&self) -> Option<&RigidSceneObservation> {
        self.observation_valid.then_some(&self.observation)
    }

    // From the groups the world was handed, so a liquid-coupled world, whose
    // bodies live in the liquid's solver, reports the same shortfall.
    fn copies_not_made(&self) -> u32 {
        if !self.observation_valid {
            return 0;
        }
        let not_made = crate::physics::copy_pool_each(
            self.observation.inputs.groups.iter().map(Option::as_ref),
            |_| {},
        );
        u32::try_from(not_made).unwrap_or(u32::MAX)
    }

    fn physics_impulse_epoch(&self) -> Option<u64> {
        if self.coupled_mode {
            return None;
        }
        self.simulation.impulse_epoch()
    }

    fn physics_impulse_readiness(&self) -> Result<bool, String> {
        if self.coupled_mode {
            return Ok(true);
        }
        self.simulation.impulse_readiness()
    }

    fn physics_impulse_stamp(
        &self,
        played: manifold_core::PlayedTime,
        sequence: u64,
        _step: crate::physics::SimStep,
    ) -> Result<manifold_physics::input::EventStamp, String> {
        if self.coupled_mode {
            return Err("Physics World coupled mode has no private impulse clock".into());
        }
        self.simulation.impulse_stamp(played, sequence)
    }

    fn enqueue_physics_impulse(
        &mut self,
        stamp: manifold_physics::input::EventStamp,
        impulse: ResolvedNodeImpulse,
    ) -> Result<manifold_physics::TickStamp, String> {
        if self.coupled_mode {
            return Err(
                "Physics World coupled mode routes impulses through the paired fluid worker".into(),
            );
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
        consume: &mut dyn FnMut(manifold_physics::input::AppliedEvent<ResolvedNodeImpulse>),
    ) {
        if self.coupled_mode {
            return;
        }
        for event in self.simulation.drain_applied_impulses() {
            map_rigid_receipt(event, consume);
        }
    }

    fn drain_discarded_impulses(
        &mut self,
        consume: &mut dyn FnMut(manifold_physics::input::EventStamp),
    ) -> u64 {
        for stamp in self.simulation.drain_discarded_impulses() {
            consume(stamp);
        }
        self.simulation.take_discarded_beyond()
    }

    fn capture_coupled_rigid(&mut self, ctx: &mut EffectNodeContext<'_, '_>) -> Result<(), String> {
        self.set_coupled_physics(true);
        self.simulation.set_step(ctx.sim_step);
        self.observation_valid = false;
        self.coupled_frame_ready = false;
        self.resolve_rigid_scene_observation(ctx, true)?;
        self.observation_valid = true;
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
        if self.coupled_frame.is_none() {
            self.reset_coupled_storage();
        }
        self.coupled_frame
            .as_mut()
            .expect("coupled storage was just made")
            .clone_from(frame);
        self.coupled_frame_ready = true;
    }
}

inventory::submit! { PhysicsNodeRegistration::new::<PhysicsWorldNode>() }

inventory::submit! {
    manifold_node_engine::persistence::PrimitiveFactory {
        type_id: PHYSICS_WORLD_TYPE_ID,
        create: || Box::new(PhysicsWorldNode::new()),
        picker: Some(manifold_node_engine::palette::PickerInfo {
            label: "Physics World",
            category: manifold_node_engine::palette::PaletteCategory::Atom,
        }),
    }
}

inventory::submit! {
    manifold_node_engine::descriptor::NodeDescriptor {
        type_id: PHYSICS_WORLD_TYPE_ID,
        purpose: "Advance one shared Box3D rigid-body world at fixed 60 Hz ticks and output its body transforms. The bodies param (1–256, default 64) sets how many individually wired body descriptions it takes; the copy_groups param (0–32, default 1) sets how many copies bodies it lays out as live copy groups, all sharing contacts and one budget of 4000 copies. Gravity and simulation speed are live controls; an optional continuous acceleration field contributes m/s² at each native microstep. Reset restores the authored starting poses and copy layouts.",
        summary: "Simulate colliding objects together under gravity, global and per-body acceleration fields, with speed and reset controls.",
        category: manifold_node_engine::descriptor::Category::Geometry3D,
        role: manifold_node_engine::descriptor::Role::Filter,
        aliases: &["physics", "box3d", "rigid simulation"],
        examples: &["PhysicsSolids", "PhysicsBoxes"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::RigidBody;
    use crate::physics::SimStep;
    use crate::physics_events::ResolvedNodeImpulse;
    use manifold_core::PlayedTime;
    use manifold_core::scene_impulse::RigidImpulseTargets;
    use manifold_core::{Beats, Seconds};
    use manifold_node_engine::bindings::{NodeInputs, NodeOutputs, Slot};
    use manifold_node_engine::exec::backend::{Backend, MockBackend};
    use manifold_node_engine::exec::effect_node::{EffectNode, FrameTime, ParamValues};
    use manifold_node_engine::exec::execution_plan::ResourceId;
    use manifold_node_engine::ports::{PortType, ScalarType};
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
        evaluate_mock_world_under(
            node,
            transport,
            body_shape,
            field_state,
            invalid_speed,
            coupled,
            SimStep::default(),
        )
    }

    fn evaluate_mock_world_under(
        node: &mut PhysicsWorldNode,
        transport: f64,
        body_shape: u32,
        field_state: MockFieldState,
        invalid_speed: bool,
        coupled: bool,
        step: SimStep,
    ) -> Option<RigidSceneObservation> {
        let mut backend = MockBackend::new();
        let mut wire_slots: Vec<(&'static str, Slot)> = Vec::new();
        let mut pending = Vec::new();
        let mut next_resource = 0;
        // Shape 0 is the body switched off: a structural edit that rebuilds.
        // Any other shape swaps in place.
        let mut body = RigidBody {
            collider: Some(crate::physics::platonic_solid(body_shape.max(1))),
            enabled: body_shape != 0,
            ..RigidBody::default()
        };
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
        prototype.spawn = crate::physics::CopySpawn {
            count: 2.0,
            spacing: 2.0,
            columns: 2.0,
            layout: 1.0,
            turn: [0.0; 3],
        };
        let prototype_slot = acquire_mock_wire(
            &mut backend,
            &mut wire_slots,
            &mut next_resource,
            "copies_0",
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
            "copies_acceleration_0",
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
        let mut ctx =
            EffectNodeContext::new(time, &params, inputs, outputs, None).with_sim_step(step);
        let graph_node: &mut dyn EffectNode = node;
        if coupled {
            let native = crate::node::get_mut(graph_node)
                .expect("PhysicsWorldNode has a native PhysicsNode registration");
            let _ = native.capture_coupled_rigid(&mut ctx);
        } else {
            graph_node.evaluate(&mut ctx);
        }
        crate::node::get(graph_node)
            .expect("PhysicsWorldNode has a native PhysicsNode registration")
            .rigid_scene_observation()
            .cloned()
    }

    fn evaluate_coupled_outputs(
        node: &mut PhysicsWorldNode,
    ) -> (
        Option<manifold_node_engine::scene::transform::Transform>,
        Option<ParamValue>,
    ) {
        let mut backend = MockBackend::new();
        let pose_slot = backend.acquire(ResourceId(10_000), PortType::Transform, None, (0, 0));
        let active_slot = backend.acquire(
            ResourceId(10_001),
            PortType::Scalar(ScalarType::F32),
            None,
            (0, 0),
        );
        let output_bindings: &[(&'static str, Slot)] =
            &[("pose_0", pose_slot), ("active_count_0", active_slot)];
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
        node.observation_valid = true;
        node.observation = RigidSceneObservation {
            inputs: RigidSceneInputs {
                targeted_fields: vec![Some(
                    FieldValue::uniform([1.0, 2.0, 3.0]).expect("finite test field"),
                )],
                ..RigidSceneInputs::default()
            },
            transport: PlayedTime::ZERO,
            speed: 1.0,
            reset: 0.0,
        };
        EffectNode::clear_state(&mut node);
        assert!(PhysicsNode::rigid_scene_observation(&node).is_none());
    }

    #[test]
    fn effect_node_observation_captures_fields_controls_and_single_native_force() {
        let mut node = PhysicsWorldNode::new();
        let first = evaluate_mock_world(&mut node, 0.0, 1, MockFieldState::Complete, false, false)
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

        assert_eq!(
            first.inputs.bodies[0].as_ref().unwrap().collider,
            Some(crate::physics::platonic_solid(1))
        );
        assert_eq!(
            first.inputs.groups[0].as_ref().unwrap().collider,
            Some(crate::physics::platonic_solid(1))
        );
        assert_eq!(
            first.inputs.groups[0].as_ref().unwrap().spawn,
            crate::physics::CopySpawn {
                count: 2.0,
                spacing: 2.0,
                columns: 2.0,
                layout: 1.0,
                turn: [0.0; 3]
            },
            "the copies body carries its spawner to the world"
        );
        assert_eq!(first.inputs.gravity, [0.0; 3]);
        assert_eq!(
            first.inputs.acceleration_field,
            Some(FieldValue::uniform([1.0, 0.0, 0.0]).unwrap())
        );
        assert_eq!(
            first.inputs.targeted_fields[0],
            Some(FieldValue::uniform([2.0, 0.0, 0.0]).unwrap())
        );
        assert_eq!(
            first.inputs.targeted_fields[node.body_count()],
            Some(FieldValue::uniform([0.5, 0.0, 0.0]).unwrap())
        );
        assert_eq!(second.transport, PlayedTime::from_played(1.0 / 60.0));
        assert_eq!(second.speed, 1.0);
        assert_eq!(second.reset, 0.0);

        let mut expected = RigidSimulation::default();
        for (observation, now) in [(&first, Seconds::ZERO), (&second, Seconds(1.0 / 60.0))] {
            expected
                .advance_scene(
                    &observation.inputs,
                    PlayedTime::from_played(now.0),
                    observation.speed,
                    observation.reset,
                )
                .expect("ordinary rigid advancement baseline");
        }
        let position = node.simulation.poses[0].pos[0];
        assert!(
            position > 0.0,
            "captured acceleration must move the native body"
        );
        assert_eq!(node.simulation.poses[0], expected.poses[0]);
        let handle = node.simulation.native_handles()[0].expect("native dynamic body handle");
        let actual_velocity = node
            .simulation
            .native_world()
            .expect("native world")
            .linear_velocity(handle)
            .expect("native velocity");
        let expected_handle = expected.native_handles()[0].expect("baseline dynamic body handle");
        let expected_velocity = expected
            .native_world()
            .expect("baseline native world")
            .linear_velocity(expected_handle)
            .expect("baseline velocity");
        assert_eq!(
            actual_velocity, expected_velocity,
            "captured force should be applied once"
        );
    }

    #[test]
    fn effect_node_observation_keeps_late_fields_and_clears_for_invalid_controls() {
        for field_state in [MockFieldState::MissingTarget, MockFieldState::PendingGlobal] {
            let mut node = PhysicsWorldNode::new();
            let complete =
                evaluate_mock_world(&mut node, 0.0, 1, MockFieldState::Complete, false, false)
                    .expect("complete rigid observation");
            let late = evaluate_mock_world(&mut node, 1.0 / 60.0, 1, field_state, false, false)
                .expect("a late field never stops the world");
            assert_eq!(
                late.inputs.acceleration_field, complete.inputs.acceleration_field,
                "a late field keeps its last value"
            );
            assert_eq!(
                late.inputs.targeted_fields, complete.inputs.targeted_fields,
                "a late field keeps its last value"
            );
        }
        let mut node = PhysicsWorldNode::new();
        assert!(
            evaluate_mock_world(&mut node, 0.0, 1, MockFieldState::Complete, false, false)
                .is_some()
        );
        assert!(
            evaluate_mock_world(
                &mut node,
                1.0 / 60.0,
                1,
                MockFieldState::Complete,
                true,
                false
            )
            .is_none()
        );
    }

    #[test]
    fn authored_withheld_topology_sample_does_not_publish_observation() {
        let mut node = PhysicsWorldNode::new();
        assert!(
            evaluate_mock_world(&mut node, 0.0, 1, MockFieldState::Complete, false, false,)
                .is_some()
        );
        assert!(
            evaluate_mock_world_under(
                &mut node,
                0.0,
                0,
                MockFieldState::Complete,
                false,
                false,
                SimStep::default().authored_sample(),
            )
            .is_none()
        );
    }

    #[test]
    fn coupled_capture_has_no_native_owner_keeps_late_fields_and_clears_invalid_observations() {
        let mut node = PhysicsWorldNode::new();
        let observation =
            evaluate_mock_world(&mut node, 0.0, 1, MockFieldState::Complete, false, true)
                .expect("coupled capture observation");
        assert_eq!(observation.transport, PlayedTime::ZERO);
        assert!(node.simulation.native_world().is_none());
        let graph_node: &mut dyn EffectNode = &mut node;
        let native = crate::node::get(graph_node)
            .expect("PhysicsWorldNode has a native PhysicsNode registration");
        assert!(native.physics_impulse_epoch().is_none());
        assert!(
            native
                .physics_impulse_stamp(PlayedTime::ZERO, 0, Default::default())
                .is_err()
        );

        assert!(
            evaluate_mock_world(
                &mut node,
                1.0 / 60.0,
                1,
                MockFieldState::PendingGlobal,
                false,
                true,
            )
            .is_some_and(
                |late| late.inputs.acceleration_field == observation.inputs.acceleration_field
            ),
            "a late field keeps its last value"
        );
        assert!(
            evaluate_mock_world(
                &mut node,
                1.0 / 60.0,
                1,
                MockFieldState::Complete,
                true,
                true,
            )
            .is_none()
        );
        assert!(PhysicsNode::rigid_scene_observation(&node).is_none());
    }

    #[test]
    fn coupled_capture_of_a_late_field_keeps_reserved_copies_and_publishes_nothing() {
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
        assert!(
            evaluate_mock_world(
                &mut node,
                0.0,
                1,
                MockFieldState::PendingGlobal,
                false,
                true,
            )
            .is_some_and(|first| first.inputs.acceleration_field.is_none()),
            "a field never seen is absent"
        );
        let retained = node.coupled_frame.as_ref().expect("retained storage");
        assert_eq!(retained.copies.capacity(), capacity);
        assert!(!node.coupled_frame_ready);
        assert_eq!(evaluate_coupled_outputs(&mut node), (None, None));
    }

    #[test]
    fn coupled_accept_publishes_supplied_pose_and_retained_copies() {
        let mut node = PhysicsWorldNode::new();
        evaluate_mock_world(&mut node, 0.0, 1, MockFieldState::Complete, false, true)
            .expect("coupled capture observation");
        let mut frame = CoupledRigidFrame {
            stamp: manifold_physics::TickStamp { epoch: 9, tick: 4 },
            ..CoupledRigidFrame::default()
        };
        frame.poses =
            vec![manifold_node_engine::scene::transform::Transform::default(); node.body_count()];
        frame.poses[0].pos = [7.0, 8.0, 9.0];
        frame.group_copies = vec![1];
        frame
            .copies
            .push(manifold_node_engine::scene::transform::Transform {
                pos: [2.0, 3.0, 4.0],
                ..manifold_node_engine::scene::transform::Transform::default()
            });
        let graph_node: &mut dyn EffectNode = &mut node;
        crate::node::get_mut(graph_node)
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
        evaluate_mock_world(&mut node, 0.0, 1, MockFieldState::Complete, false, true)
            .expect("coupled capture observation");
        let mut frame = CoupledRigidFrame::default();
        frame
            .copies
            .push(manifold_node_engine::scene::transform::Transform::default());
        frame.group_copies.push(1);
        PhysicsNode::accept_coupled_rigid_frame(&mut node, Some(&frame));
        EffectNode::clear_state(&mut node);
        assert!(node.coupled_mode);
        assert!(PhysicsNode::rigid_scene_observation(&node).is_none());
        assert!(node.coupled_frame.is_some());
        assert!(!node.coupled_frame_ready);
        assert!(node.simulation.native_world().is_none());
    }

    #[test]
    fn physics_world_effect_node_impulse_dispatch_preserves_receipt_and_retry_sequence() {
        use manifold_node_engine::exec::effect_node::EffectNode;
        let mut node = PhysicsWorldNode::new();
        let mut bodies = vec![None; 64];
        bodies[0] = Some(RigidBody::default());
        node.simulation
            .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
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
            target: ImpulseTarget::Rigid(RigidImpulseTargets::body(0).unwrap()),
        };

        {
            let graph_node: &mut dyn EffectNode = &mut node;
            let native = crate::node::get_mut(graph_node)
                .expect("PhysicsWorldNode has a native PhysicsNode registration");
            assert!(
                native
                    .enqueue_physics_impulse(stamp, wrong)
                    .expect_err("wrong target must be rejected before queue admission")
                    .contains("fluid")
            );
            assert_eq!(
                native
                    .enqueue_physics_impulse(stamp, valid)
                    .expect("same producer sequence must remain valid"),
                manifold_physics::TickStamp { epoch, tick: 0 }
            );
        }

        node.simulation
            .advance(
                &bodies,
                [0.0; 3],
                PlayedTime::from_played(1.0 / 60.0),
                1.0,
                0.0,
            )
            .expect("native rigid tick");

        let mut receipts = Vec::new();
        let graph_node: &mut dyn EffectNode = &mut node;
        crate::node::get_mut(graph_node)
            .expect("PhysicsWorldNode has a native PhysicsNode registration")
            .drain_physics_impulses(&mut |event| receipts.push(event));
        assert_eq!(receipts.len(), 1);
        let receipt = receipts.pop().expect("one rigid receipt");
        assert_eq!(receipt.source, stamp);
        assert_eq!(
            receipt.applied,
            manifold_physics::TickStamp { epoch, tick: 0 }
        );
        assert_eq!(receipt.lateness, Seconds::ZERO);
        assert_eq!(receipt.value.field, field);
        assert_eq!(
            receipt.value.target,
            ImpulseTarget::Rigid(RigidImpulseTargets::body(0).unwrap())
        );
        crate::node::get_mut(graph_node)
            .expect("PhysicsWorldNode has a native PhysicsNode registration")
            .drain_physics_impulses(&mut |_| panic!("receipt drained twice"));
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;

    fn read_instances(buffer: &manifold_gpu::GpuBuffer) -> Vec<InstanceTransform> {
        let ptr = buffer.mapped_ptr().expect("shared output buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const InstanceTransform, MAX_COPIES).to_vec() }
    }

    #[test]
    fn instance_upload_crosses_inline_chunk_and_zeroes_shrunk_tail() {
        let device = manifold_gpu::testkit::test_device();
        let output = device.create_buffer(
            (MAX_COPIES * std::mem::size_of::<InstanceTransform>()) as u64,
            manifold_gpu::BufferUse::Upload,
        );
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

#[cfg(test)]
mod port_tests {
    use super::*;

    fn counts(bodies: f32, groups: f32) -> ParamValues {
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("bodies"), ParamValue::Float(bodies));
        params.insert(Cow::Borrowed("copy_groups"), ParamValue::Float(groups));
        params
    }

    fn has_input(node: &PhysicsWorldNode, name: &str) -> bool {
        node.inputs().iter().any(|port| port.name == name)
    }

    fn has_output(node: &PhysicsWorldNode, name: &str) -> bool {
        node.outputs().iter().any(|port| port.name == name)
    }

    #[test]
    fn a_world_with_no_counts_keeps_the_old_sixty_four_bodies_and_one_group() {
        let mut node = PhysicsWorldNode::new();
        node.reconfigure(&ParamValues::default());
        assert_eq!((node.body_count(), node.copy_group_count()), (64, 1));
        assert!(has_input(&node, "body_63") && !has_input(&node, "body_64"));
        assert!(has_input(&node, "copies_0") && has_input(&node, "copies_acceleration_0"));
        assert!(has_output(&node, "instances_0") && has_output(&node, "active_count_0"));
        assert!(!has_input(&node, "copies_1"));
    }

    #[test]
    fn reconfigure_sizes_every_port_family_to_any_count() {
        let mut node = PhysicsWorldNode::new();
        node.reconfigure(&counts(256.0, 32.0));
        assert_eq!((node.body_count(), node.copy_group_count()), (256, 32));
        assert!(has_input(&node, "body_255") && has_input(&node, "body_acceleration_255"));
        assert!(has_output(&node, "pose_255") && has_output(&node, "instances_31"));
        assert_eq!(node.observation.inputs.bodies.len(), 256);
        assert_eq!(node.observation.inputs.targeted_fields.len(), 288);
        assert_eq!(
            node.array_output_capacity("instances_31", &ParamValues::default(), &[]),
            Some(MAX_COPIES as u32)
        );

        node.reconfigure(&counts(9_999.0, 999.0));
        assert_eq!(
            (node.body_count(), node.copy_group_count()),
            (9_999, 999),
            "no count is capped"
        );
        assert!(has_input(&node, "body_9998") && has_output(&node, "instances_998"));

        node.reconfigure(&counts(3.0, 0.0));
        assert_eq!((node.body_count(), node.copy_group_count()), (3, 0));
        assert!(!has_input(&node, "body_3") && !has_output(&node, "instances_0"));
        assert_eq!(
            node.array_output_capacity("instances_0", &ParamValues::default(), &[]),
            None
        );
    }
}
