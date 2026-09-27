//! Value descriptions on graph wires; native simulation ownership stays in the world node.
use manifold_core::Seconds;
use manifold_physics::{
    input::{input_span, input_span_before, AppliedEvent, EventQueue, HistoryWrite, InputHistory, Timestamped},
    BodyConfig, BodyHandle, BodyKind, FieldInput, FieldValue, PhysicsWorld, VectorField,
};
use std::sync::Arc;

use super::transform::Transform;
use crate::generators::platonic_geometry::platonic_points;

mod targeted_fields;
mod impulses;

pub use impulses::{ResolvedRigidImpulse, RigidImpulseTargets};
use targeted_fields::{TargetedFieldHistory, TARGET_SLOTS};

thread_local! {
    // A preview budget only yields work; it never discards simulation time.
    static PREVIEW_STEP_BUDGET: std::cell::Cell<Option<std::time::Duration>> = const { std::cell::Cell::new(None) };
    static SAMPLE_AUTHORED_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static HISTORY_DRAIN_REQUESTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn authored_sample_only() -> bool {
    SAMPLE_AUTHORED_ONLY.with(std::cell::Cell::get)
}

/// Fluid workers use the same preview/offline scope as rigid bodies.
pub(crate) fn offline_simulation() -> bool {
    PREVIEW_STEP_BUDGET.with(|budget| budget.get().is_none())
}

pub(crate) fn history_drain_requested() -> bool {
    HISTORY_DRAIN_REQUESTED.with(std::cell::Cell::get) && offline_simulation()
}

/// Evaluate historical physics inputs without publishing graph outputs. Native
/// state is retained unless an explicit offline history-drain scope is active.
#[must_use]
pub struct PhysicsAuthoredSampleScope {
    previous: bool,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl PhysicsAuthoredSampleScope {
    pub fn new() -> Self {
        let previous = SAMPLE_AUTHORED_ONLY.with(|current| current.replace(true));
        Self {
            previous,
            _thread_bound: std::marker::PhantomData,
        }
    }
}

impl Default for PhysicsAuthoredSampleScope {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PhysicsAuthoredSampleScope {
    fn drop(&mut self) {
        SAMPLE_AUTHORED_ONLY.with(|current| current.set(self.previous));
    }
}

/// Allow a bounded historical input batch to advance the native simulation.
/// Live preview remains observe-only even if a caller accidentally holds this
/// scope, and the scope never changes graph output publication policy.
#[must_use]
pub(crate) struct PhysicsHistoryDrainScope {
    previous: bool,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl PhysicsHistoryDrainScope {
    pub(crate) fn new() -> Self {
        let previous = HISTORY_DRAIN_REQUESTED.with(|current| current.replace(true));
        Self {
            previous,
            _thread_bound: std::marker::PhantomData,
        }
    }
}

impl Drop for PhysicsHistoryDrainScope {
    fn drop(&mut self) {
        HISTORY_DRAIN_REQUESTED.with(|current| current.set(self.previous));
    }
}

/// Bound preview work batches so commands remain serviceable between frames.
/// Export drains all due ticks; both paths use the same fixed timestep.
#[must_use]
pub struct PhysicsStepScope {
    previous: Option<std::time::Duration>,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl PhysicsStepScope {
    pub fn for_render(export_mode: bool) -> Self {
        Self::with_preview_budget(export_mode, std::time::Duration::from_secs_f64(1.0 / 60.0))
    }

    /// A running native tick cannot be interrupted, even with a zero budget.
    pub fn with_preview_budget(export_mode: bool, budget: std::time::Duration) -> Self {
        let previous =
            PREVIEW_STEP_BUDGET.with(|current| current.replace((!export_mode).then_some(budget)));
        Self {
            previous,
            _thread_bound: std::marker::PhantomData,
        }
    }
}

impl Drop for PhysicsStepScope {
    fn drop(&mut self) {
        PREVIEW_STEP_BUDGET.with(|budget| budget.set(self.previous));
    }
}

pub const MAX_BODIES: usize = 64;
pub const MAX_COPIES: usize = 4_000;
pub(crate) const AUTHORED_HISTORY_CAPACITY: usize = 256;
const IMPULSE_CAPACITY: usize = 256;
const FIXED_TICK: Seconds = Seconds(1.0 / 60.0);
pub const BODY_PORTS: [&str; MAX_BODIES] = [
    "body_0", "body_1", "body_2", "body_3", "body_4", "body_5", "body_6", "body_7", "body_8",
    "body_9", "body_10", "body_11", "body_12", "body_13", "body_14", "body_15", "body_16",
    "body_17", "body_18", "body_19", "body_20", "body_21", "body_22", "body_23", "body_24",
    "body_25", "body_26", "body_27", "body_28", "body_29", "body_30", "body_31", "body_32",
    "body_33", "body_34", "body_35", "body_36", "body_37", "body_38", "body_39", "body_40",
    "body_41", "body_42", "body_43", "body_44", "body_45", "body_46", "body_47", "body_48",
    "body_49", "body_50", "body_51", "body_52", "body_53", "body_54", "body_55", "body_56",
    "body_57", "body_58", "body_59", "body_60", "body_61", "body_62", "body_63",
];
pub const POSE_PORTS: [&str; MAX_BODIES] = [
    "pose_0", "pose_1", "pose_2", "pose_3", "pose_4", "pose_5", "pose_6", "pose_7", "pose_8",
    "pose_9", "pose_10", "pose_11", "pose_12", "pose_13", "pose_14", "pose_15", "pose_16",
    "pose_17", "pose_18", "pose_19", "pose_20", "pose_21", "pose_22", "pose_23", "pose_24",
    "pose_25", "pose_26", "pose_27", "pose_28", "pose_29", "pose_30", "pose_31", "pose_32",
    "pose_33", "pose_34", "pose_35", "pose_36", "pose_37", "pose_38", "pose_39", "pose_40",
    "pose_41", "pose_42", "pose_43", "pose_44", "pose_45", "pose_46", "pose_47", "pose_48",
    "pose_49", "pose_50", "pose_51", "pose_52", "pose_53", "pose_54", "pose_55", "pose_56",
    "pose_57", "pose_58", "pose_59", "pose_60", "pose_61", "pose_62", "pose_63",
];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum CopyLayout {
    #[default]
    Grid,
    Pile,
}

impl CopyLayout {
    fn from_scalar(value: f32) -> Self {
        if value.round().clamp(0.0, 1.0) >= 1.0 {
            Self::Pile
        } else {
            Self::Grid
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColliderGeometry {
    pub hulls: Vec<Vec<[f32; 3]>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RigidBody {
    pub transform: Transform,
    pub enabled: bool,
    /// Release event count for an intact body. Fragment children carry their
    /// parent index and use this count as the shared activation trigger.
    pub release_count: f32,
    /// Parent body index for a prepared fragment, or `None` for an ordinary
    /// body. This metadata never changes collider topology.
    pub fragment_parent: Option<usize>,
    pub shape: u32,
    pub kind: u32,
    pub mass: f32,
    pub friction: f32,
    pub bounce: f32,
    pub collider: Option<Arc<ColliderGeometry>>,
}

/// An authored graph sample retained until all fixed ticks that can use it
/// have been replayed. Between rendered samples, position and the raw Euler
/// angles are interpolated linearly. This does not reconstruct nonlinear
/// upstream animation between rendered samples.
#[derive(Clone)]
struct AuthoredPoseSample {
    time: f64,
    bodies: [Option<RigidBody>; MAX_BODIES],
    prototype: Option<RigidBody>,
    gravity: [f32; 3],
    acceleration_field: Option<FieldValue>,
}

impl Timestamped for AuthoredPoseSample {
    fn time(&self) -> manifold_physics::Seconds {
        manifold_physics::Seconds(self.time)
    }
}

#[derive(Clone)]
struct DeferredAnimatedEdit {
    time: f64,
    body: RigidBody,
}

impl Default for RigidBody {
    fn default() -> Self {
        Self {
            transform: Transform::default(),
            enabled: true,
            release_count: 0.0,
            fragment_parent: None,
            shape: 1,
            kind: 1,
            mass: 1.0,
            friction: 0.5,
            bounce: 0.15,
            collider: None,
        }
    }
}

impl RigidBody {
    fn config(&self) -> BodyConfig {
        let pose = pose_from_transform(self.transform);
        BodyConfig {
            kind: match self.kind {
                0 => BodyKind::Fixed,
                2 => BodyKind::Animated,
                _ => BodyKind::Dynamic,
            },
            position: pose.position,
            rotation: pose.rotation,
            mass: self.mass,
            friction: self.friction,
            restitution: self.bounce,
        }
    }
}

/// Shared scene Rz * Ry * Rx convention, with quaternion stored xyzw.
pub(crate) fn pose_from_transform(transform: Transform) -> manifold_physics::BodyPose {
    let [x, y, z] = transform.rot_euler;
    let (sx, cx) = (x * 0.5).sin_cos();
    let (sy, cy) = (y * 0.5).sin_cos();
    let (sz, cz) = (z * 0.5).sin_cos();
    manifold_physics::BodyPose {
        position: transform.pos,
        rotation: [
            sx * cy * cz - cx * sy * sz,
            cx * sy * cz + sx * cy * sz,
            cx * cy * sz - sx * sy * cz,
            cx * cy * cz + sx * sy * sz,
        ],
    }
}

fn same_collider(left: &RigidBody, right: &RigidBody) -> bool {
    match (&left.collider, &right.collider) {
        (None, None) => true,
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        _ => false,
    }
}

fn same_body(left: &RigidBody, right: &RigidBody) -> bool {
    left.transform == right.transform
        && left.enabled == right.enabled
        && left.shape == right.shape
        && left.kind == right.kind
        && left.mass == right.mass
        && left.friction == right.friction
        && left.bounce == right.bounce
        && same_collider(left, right)
}

fn same_authored_body(left: &RigidBody, right: &RigidBody) -> bool {
    same_body(left, right)
        && left.release_count == right.release_count
        && left.fragment_parent == right.fragment_parent
}

fn scale_platonic_points(points: &[[f32; 3]], scale: [f32; 3]) -> [[f32; 3]; 20] {
    let mut scaled = [[0.0; 3]; 20];
    for (dst, src) in scaled.iter_mut().zip(points) {
        for axis in 0..3 {
            dst[axis] = src[axis] * scale[axis];
        }
    }
    scaled
}

fn scale_hulls(geometry: &ColliderGeometry, scale: [f32; 3]) -> Vec<Vec<[f32; 3]>> {
    geometry
        .hulls
        .iter()
        .map(|hull| {
            hull.iter()
                .map(|point| {
                    [
                        point[0] * scale[0],
                        point[1] * scale[1],
                        point[2] * scale[2],
                    ]
                })
                .collect()
        })
        .collect()
}

fn add_body_geometry(world: &mut PhysicsWorld, body: &RigidBody) -> Result<BodyHandle, String> {
    if let Some(geometry) = body.collider.as_ref() {
        let scaled = scale_hulls(geometry, body.transform.scale);
        world
            .add_hulls(&scaled, body.config())
            .map_err(|error| error.to_string())
    } else {
        let points = platonic_points(body.shape);
        let scaled = scale_platonic_points(points, body.transform.scale);
        world
            .add_hull(&scaled[..points.len()], body.config())
            .map_err(|error| error.to_string())
    }
}

pub struct RigidSimulation {
    world: Option<PhysicsWorld>,
    handles: [Option<BodyHandle>; MAX_BODIES],
    descriptions: [Option<RigidBody>; MAX_BODIES],
    bullet_enabled: [bool; MAX_BODIES],
    /// Paused authoring edits remain visible while older preview ticks drain.
    /// The native teleport happens only after those ticks, so the edit cannot
    /// sweep through objects that lay between the old and new pose.
    deferred_animated_edit: [Option<DeferredAnimatedEdit>; MAX_BODIES],
    copy_handles: Vec<Option<BodyHandle>>,
    field_handles: Vec<BodyHandle>,
    copy_description: Option<RigidBody>,
    copy_bullet_enabled: Vec<bool>,
    deferred_copy_animated_edit: Option<DeferredAnimatedEdit>,
    latched_copy_count: usize,
    latched_copy_spacing: f32,
    latched_copy_columns: usize,
    latched_copy_layout: CopyLayout,
    fragment_active: [bool; MAX_BODIES],
    fragment_release_latched: [f32; MAX_BODIES],
    fragment_parent_released: [bool; MAX_BODIES],
    last_time: Option<Seconds>,
    accumulator: f64,
    authored_time: f64,
    physics_time: f64,
    authored_samples: InputHistory<AuthoredPoseSample>,
    targeted_fields: TargetedFieldHistory,
    reset_count: Option<f32>,
    pub poses: [Transform; MAX_BODIES],
    pub copy_poses: Vec<Transform>,
    pub active_copy_count: usize,
    pub physics_ms: f32,
    /// Whole fixed ticks still owed after the last preview work batch.
    pub pending_time: Seconds,
    last_overload_warning: Option<std::time::Instant>,
    impulse_queue: Option<EventQueue<ResolvedRigidImpulse>>,
    impulse_receipts: Vec<AppliedEvent<ResolvedRigidImpulse>>,
    impulse_tick_events: Vec<AppliedEvent<ResolvedRigidImpulse>>,
    impulse_epoch: Option<u64>,
    impulse_failure: Option<String>,
    impulse_overflow_latched: bool,
    accepted_observation: Option<(f64, f64)>,
}

impl Default for RigidSimulation {
    fn default() -> Self {
        Self {
            world: None,
            handles: std::array::from_fn(|_| None),
            descriptions: std::array::from_fn(|_| None),
            bullet_enabled: [false; MAX_BODIES],
            deferred_animated_edit: std::array::from_fn(|_| None),
            copy_handles: vec![None; MAX_COPIES],
            field_handles: Vec::with_capacity(MAX_BODIES + MAX_COPIES),
            copy_description: None,
            copy_bullet_enabled: vec![false; MAX_COPIES],
            deferred_copy_animated_edit: None,
            latched_copy_count: 0,
            latched_copy_spacing: 1.25,
            latched_copy_columns: 16,
            latched_copy_layout: CopyLayout::Grid,
            fragment_active: [false; MAX_BODIES],
            fragment_release_latched: [0.0; MAX_BODIES],
            fragment_parent_released: [false; MAX_BODIES],
            last_time: None,
            accumulator: 0.0,
            authored_time: 0.0,
            physics_time: 0.0,
            authored_samples: InputHistory::with_capacity(AUTHORED_HISTORY_CAPACITY)
                .expect("the fixed authored history capacity is valid"),
            targeted_fields: TargetedFieldHistory::default(),
            reset_count: None,
            poses: [Transform::default(); MAX_BODIES],
            copy_poses: vec![Transform::default(); MAX_COPIES],
            active_copy_count: 0,
            physics_ms: 0.0,
            pending_time: Seconds::ZERO,
            last_overload_warning: None,
            impulse_queue: None,
            impulse_receipts: Vec::with_capacity(IMPULSE_CAPACITY),
            impulse_tick_events: Vec::with_capacity(IMPULSE_CAPACITY),
            impulse_epoch: None,
            impulse_failure: None,
            impulse_overflow_latched: false,
            accepted_observation: None,
        }
    }
}

impl RigidSimulation {
    /// Hold the transport clock while an upstream collider is still being
    /// prepared. The next ready frame starts from its current authored time
    /// instead of replaying time that elapsed with incomplete geometry.
    pub fn hold_pending(&mut self, now: Seconds) {
        self.accepted_observation = None;
        if now.0.is_finite() {
            self.last_time = Some(now);
        }
        self.pending_time = Seconds::ZERO;
    }

    /// 60 Hz outer ticks, four Box3D substeps each. A paused transport contributes no time.
    pub fn advance(
        &mut self,
        bodies: [Option<RigidBody>; MAX_BODIES],
        gravity: [f32; 3],
        now: Seconds,
        speed: f32,
        reset_count: f32,
    ) -> Result<(), String> {
        self.advance_with_copies(
            bodies,
            None,
            0.0,
            1.25,
            16.0,
            gravity,
            now,
            speed,
            reset_count,
        )
    }

    /// Advance the shared world, optionally adding a reset-latched grid of
    /// copies of `prototype`. Copy controls are sampled when the world is
    /// first built, after a reset, or when transport moves backwards. Editing
    /// count, spacing, or columns while the simulation is running leaves the
    /// active world and its poses untouched until one of those latch points.
    #[allow(clippy::too_many_arguments)]
    pub fn advance_with_copies(
        &mut self,
        bodies: [Option<RigidBody>; MAX_BODIES],
        prototype: Option<RigidBody>,
        copy_count: f32,
        copy_spacing: f32,
        copy_columns: f32,
        gravity: [f32; 3],
        now: Seconds,
        speed: f32,
        reset_count: f32,
    ) -> Result<(), String> {
        self.advance_with_copy_layout(
            bodies,
            prototype,
            copy_count,
            copy_spacing,
            copy_columns,
            0.0,
            gravity,
            now,
            speed,
            reset_count,
        )
    }

    /// Advance the shared world with a reset-latched copy layout. `layout` is
    /// zero for the legacy centered grid and one for the compact pile.
    #[allow(clippy::too_many_arguments)]
    pub fn advance_with_copy_layout(
        &mut self,
        bodies: [Option<RigidBody>; MAX_BODIES],
        prototype: Option<RigidBody>,
        copy_count: f32,
        copy_spacing: f32,
        copy_columns: f32,
        layout: f32,
        gravity: [f32; 3],
        now: Seconds,
        speed: f32,
        reset_count: f32,
    ) -> Result<(), String> {
        self.advance_with_fields(
            bodies,
            prototype,
            copy_count,
            copy_spacing,
            copy_columns,
            layout,
            gravity,
            now,
            speed,
            reset_count,
            None,
        )
    }

    /// Advance the shared world with an optional retained acceleration field.
    /// Field edits are sampled through the same fixed-tick authored history as
    /// gravity and applied at every native microstep.
    #[allow(clippy::too_many_arguments)]
    pub fn advance_with_fields(
        &mut self,
        bodies: [Option<RigidBody>; MAX_BODIES],
        prototype: Option<RigidBody>,
        copy_count: f32,
        copy_spacing: f32,
        copy_columns: f32,
        layout: f32,
        gravity: [f32; 3],
        now: Seconds,
        speed: f32,
        reset_count: f32,
        acceleration_field: Option<FieldValue>,
    ) -> Result<(), String> {
        self.advance_with_targeted_fields(
            bodies,
            prototype,
            copy_count,
            copy_spacing,
            copy_columns,
            layout,
            gravity,
            now,
            speed,
            reset_count,
            acceleration_field,
            &[],
        )
    }

    /// Advance with optional per-body and copy acceleration fields. Entries
    /// zero through `MAX_BODIES - 1` target ordinary bodies; the final entry
    /// targets every active copy.
    #[allow(clippy::too_many_arguments)]
    pub fn advance_with_targeted_fields(
        &mut self,
        bodies: [Option<RigidBody>; MAX_BODIES],
        prototype: Option<RigidBody>,
        copy_count: f32,
        copy_spacing: f32,
        copy_columns: f32,
        layout: f32,
        gravity: [f32; 3],
        now: Seconds,
        speed: f32,
        reset_count: f32,
        acceleration_field: Option<FieldValue>,
        targeted_fields_input: &[Option<FieldValue>],
    ) -> Result<(), String> {
        self.accepted_observation = None;
        if !targeted_fields_input.is_empty() && targeted_fields_input.len() != TARGET_SLOTS {
            return Err(format!(
                "Physics: targeted fields require exactly {TARGET_SLOTS} entries"
            ));
        }
        let cleared_targeted_fields: [Option<FieldValue>; TARGET_SLOTS];
        let has_targeted_field = targeted_fields_input.iter().any(Option::is_some);
        let targeted_fields = if has_targeted_field
            || (!targeted_fields_input.is_empty() && self.targeted_fields.is_connected())
        {
            Some(targeted_fields_input)
        } else if self.targeted_fields.is_connected() {
            cleared_targeted_fields = std::array::from_fn(|_| None);
            Some(cleared_targeted_fields.as_slice())
        } else {
            None
        };
        if !now.0.is_finite()
            || !speed.is_finite()
            || !(0.0..=4.0).contains(&speed)
            || !reset_count.is_finite()
            || gravity.iter().any(|v| !v.is_finite())
        {
            return Err("Physics: non-finite clock/control or speed outside 0–4".into());
        }
        if !copy_count.is_finite()
            || !copy_spacing.is_finite()
            || copy_spacing <= 0.0
            || !copy_columns.is_finite()
            || !layout.is_finite()
        {
            return Err("Physics: copy count, spacing, columns, and layout must be finite; spacing must be positive".into());
        }
        validate_fragments(&bodies)?;
        let requested_copy_count = if prototype.as_ref().is_some_and(|body| body.enabled) {
            copy_count.round().clamp(0.0, MAX_COPIES as f32) as usize
        } else {
            0
        };
        let requested_copy_columns = copy_columns.round().clamp(1.0, 64.0) as usize;
        let requested_copy_layout = CopyLayout::from_scalar(layout);
        if let Some(prototype) = prototype.as_ref().filter(|body| body.enabled) {
            validate_copy_prototype(prototype)?;
        }
        for b in bodies.iter().flatten() {
            if !b.enabled {
                continue;
            }
            if b.shape >= 5
                || b.kind > 2
                || b.transform.billboard
                || b.transform
                    .scale
                    .iter()
                    .any(|s| !s.is_finite() || *s <= 0.0 || *s > 100.0)
            {
                return Err(
                    "Physics: use a Platonic shape, positive scale up to 100, and no billboard"
                        .into(),
                );
            }
        }
        let topology_changed =
            bodies
                .iter()
                .zip(self.descriptions.iter())
                .any(|(a, b)| match (a, b) {
                    (Some(a), Some(b)) => {
                        a.shape != b.shape
                            || a.transform.scale != b.transform.scale
                            || a.enabled != b.enabled
                            || !same_collider(a, b)
                    }
                    (None, None) => false,
                    _ => true,
                });
        let reset = self.reset_count.is_some_and(|old| old != reset_count)
            || self.last_time.is_some_and(|last| now.0 < last.0);
        let copy_topology_changed = match (prototype.as_ref(), self.copy_description.as_ref()) {
            (Some(current), Some(previous)) => {
                current.shape != previous.shape
                    || current.transform.scale != previous.transform.scale
                    || current.enabled != previous.enabled
                    || !same_collider(current, previous)
            }
            (None, None) => false,
            _ => true,
        };
        let prototype_activation_changed = prototype.as_ref().map(|body| body.enabled)
            != self.copy_description.as_ref().map(|body| body.enabled);
        let rebuild = self.world.is_none() || topology_changed || copy_topology_changed || reset;
        if !rebuild {
            if let Some(error) = &self.impulse_failure {
                return Err(error.clone());
            }
            if self.impulse_overflow_latched {
                return Err("Physics: impulse history is full; restart the simulation or bake the scene".into());
            }
        }
        if SAMPLE_AUTHORED_ONLY.with(std::cell::Cell::get) {
            // A topology edit or seek rebuilds at the next full graph frame;
            // old trajectories cannot safely be spliced into a new world.
            if self.world.is_none() || topology_changed || copy_topology_changed || reset {
                return Ok(());
            }
            if !history_drain_requested() {
                let elapsed = now.0 - self.last_time.unwrap_or(now).0;
                let elapsed_simulation = elapsed * f64::from(speed);
                let authored_time = self.authored_time + elapsed_simulation;
                self.ensure_targeted_history(targeted_fields)?;
                self.record_authored_sample(
                    authored_time,
                    bodies.clone(),
                    prototype.clone(),
                    gravity,
                    acceleration_field.clone(),
                    targeted_fields,
                )?;
                self.authored_time = authored_time;
                self.accumulator += elapsed_simulation;
                self.last_time = Some(now);
                self.accepted_observation = Some((now.0, self.authored_time));
                return Ok(());
            }
        }
        let next_impulse_epoch = if rebuild {
            Some(
                self.impulse_epoch
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or("Physics: impulse epoch exhausted")?,
            )
        } else {
            None
        };
        if rebuild {
            self.copy_poses.fill(Transform::default());
            let prototype_added = self.copy_description.is_none() && prototype.is_some();
            let active_copy_count = if prototype.as_ref().is_none_or(|body| !body.enabled) {
                0
            } else if self.world.is_none()
                || reset
                || prototype_added
                || prototype_activation_changed
            {
                requested_copy_count
            } else {
                self.latched_copy_count
            };
            let active_copy_spacing = if self.world.is_none() || reset || prototype_added {
                copy_spacing
            } else {
                self.latched_copy_spacing
            };
            let active_copy_columns = if self.world.is_none() || reset || prototype_added {
                requested_copy_columns
            } else {
                self.latched_copy_columns
            };
            let active_copy_layout = if self.world.is_none() || reset || prototype_added {
                requested_copy_layout
            } else {
                self.latched_copy_layout
            };
            if let Some(prototype) = prototype.as_ref().filter(|body| body.enabled) {
                validate_copy_prototype(prototype)?;
            }
            let mut world = PhysicsWorld::new(gravity).map_err(|e| e.to_string())?;
            let mut handles = std::array::from_fn(|_| None);
            for (i, body) in bodies.iter().enumerate() {
                let Some(body) = body.as_ref().filter(|body| body.enabled) else { continue };
                let handle = add_body_geometry(&mut world, body)?;
                if body.fragment_parent.is_some() {
                    world.set_enabled(handle, false).map_err(|e| e.to_string())?;
                }
                handles[i] = Some(handle);
            }
            let mut copy_handles = vec![None; active_copy_count];
            if let Some(prototype) = prototype.as_ref().filter(|body| body.enabled) {
                let scaled_geometry = prototype
                    .collider
                    .as_ref()
                    .map(|geometry| scale_hulls(geometry, prototype.transform.scale));
                for (index, handle) in copy_handles.iter_mut().enumerate() {
                    let mut copy = prototype.clone();
                    copy = copy_transform_for_layout(
                        copy,
                        prototype.transform.pos,
                        index,
                        active_copy_count,
                        active_copy_columns,
                        active_copy_spacing,
                        active_copy_layout,
                    );
                    *handle = Some(if let Some(hulls) = scaled_geometry.as_deref() {
                        world
                            .add_hulls(hulls, copy.config())
                            .map_err(|e| e.to_string())?
                    } else {
                        let points = platonic_points(copy.shape);
                        let scaled = scale_platonic_points(points, copy.transform.scale);
                        world
                            .add_hull(&scaled[..points.len()], copy.config())
                            .map_err(|e| e.to_string())?
                    });
                }
            }
            self.reset_impulse_runtime(
                next_impulse_epoch.expect("rebuild has a planned impulse epoch"),
            )?;
            self.world = Some(world);
            self.handles = handles;
            self.bullet_enabled.fill(false);
            self.deferred_animated_edit.fill(None);
            self.copy_handles.fill(None);
            self.copy_bullet_enabled.fill(false);
            self.deferred_copy_animated_edit = None;
            self.fragment_active.fill(false);
            self.fragment_release_latched.fill(0.0);
            self.fragment_parent_released.fill(false);
            for (index, body) in bodies.iter().enumerate() {
                self.fragment_active[index] = body
                    .as_ref()
                    .is_some_and(|body| body.enabled && body.fragment_parent.is_none());
            }
            self.copy_handles[..active_copy_count].copy_from_slice(&copy_handles);
            self.field_handles.clear();
            self.field_handles
                .extend(self.handles.iter().flatten().copied());
            self.field_handles
                .extend(copy_handles.into_iter().flatten());
            self.descriptions = bodies.clone();
            self.copy_description = prototype.clone();
            self.active_copy_count = active_copy_count;
            self.latched_copy_count = active_copy_count;
            self.latched_copy_spacing = active_copy_spacing;
            self.latched_copy_columns = active_copy_columns;
            self.latched_copy_layout = active_copy_layout;
            self.last_time = Some(now);
            self.accumulator = 0.0;
            self.authored_time = 0.0;
            self.physics_time = 0.0;
            self.authored_samples.clear();
            self.targeted_fields.clear();
            self.authored_samples
                .record(
                    AuthoredPoseSample {
                        time: 0.0,
                        bodies: bodies.clone(),
                        prototype: prototype.clone(),
                        gravity,
                        acceleration_field: acceleration_field.clone(),
                    },
                    Seconds::ZERO,
                )
                .map_err(|error| format!("Physics: failed to seed input history: {error}"))?;
            if let Some(targeted_fields) = targeted_fields {
                self.ensure_targeted_history(Some(targeted_fields))?;
                self.targeted_fields
                    .record(targeted_fields, HistoryWrite::Replaced)?;
            }
            for (index, body) in bodies.iter().enumerate() {
                let Some(body) = body.as_ref().filter(|body| {
                    body.enabled && body.fragment_parent.is_none() && body.release_count > 0.0
                }) else {
                    continue;
                };
                self.release_fragments(index, body, body.release_count)?;
            }
        }
        let elapsed = now.0 - self.last_time.unwrap_or(now).0;
        // Preserve all elapsed time. Preview can yield with ticks still queued.
        let elapsed_simulation = elapsed * f64::from(speed);
        let stationary_edit = elapsed_simulation == 0.0;
        let authored_time = self.authored_time + elapsed_simulation;
        self.ensure_targeted_history(targeted_fields)?;
        self.record_authored_sample(
            authored_time,
            bodies.clone(),
            prototype.clone(),
            gravity,
            acceleration_field.clone(),
            targeted_fields,
        )?;
        self.authored_time = authored_time;
        let accumulated = self.accumulator + elapsed_simulation;
        const TICK: f64 = FIXED_TICK.0;
        let due_steps = ((accumulated + 1e-9) / TICK).floor() as usize;
        let preview_budget = PREVIEW_STEP_BUDGET.with(std::cell::Cell::get);
        let steps = if speed == 0.0 && preview_budget.is_some() {
            0
        } else {
            due_steps
        };
        {
            let world = self.world.as_mut().expect("world constructed above");
            for (i, body) in bodies.iter().enumerate() {
                let (Some(body), Some(handle)) = (body, self.handles[i]) else {
                    continue;
                };
                let old = self.descriptions[i].as_ref();
                if old.is_none_or(|old| !same_body(old, body)) {
                    let pose_changed = old.is_none_or(|old| old.transform != body.transform);
                    let animated_edit = body.kind == 2 && stationary_edit && pose_changed;
                    if animated_edit && due_steps > 0 {
                        self.deferred_animated_edit[i] = Some(DeferredAnimatedEdit {
                            time: self.authored_time,
                            body: body.clone(),
                        });
                    }
                    let move_pose =
                        pose_changed && (body.kind != 2 || (animated_edit && due_steps == 0));
                    world
                        .update_body(handle, body.config(), move_pose)
                        .map_err(|e| e.to_string())?;
                    if body.kind == 1 && old.is_some_and(|old| old.kind != 1) {
                        world.set_bullet(handle, false).map_err(|e| e.to_string())?;
                        self.bullet_enabled[i] = false;
                    }
                }
            }
            if let Some(prototype) = prototype.as_ref() {
                let old = self.copy_description.as_ref();
                if old.is_none_or(|old| !same_body(old, prototype)) {
                    let pose_changed = old.is_some_and(|old| old.transform != prototype.transform);
                    let animated_edit = prototype.kind == 2 && stationary_edit && pose_changed;
                    if animated_edit && due_steps > 0 {
                        self.deferred_copy_animated_edit = Some(DeferredAnimatedEdit {
                            time: self.authored_time,
                            body: prototype.clone(),
                        });
                    }
                    let move_pose =
                        pose_changed && (prototype.kind != 2 || (animated_edit && due_steps == 0));
                    for index in 0..self.active_copy_count {
                        let Some(handle) = self.copy_handles[index] else {
                            continue;
                        };
                        let copy = copy_transform_for_layout(
                            prototype.clone(),
                            prototype.transform.pos,
                            index,
                            self.active_copy_count,
                            self.latched_copy_columns,
                            self.latched_copy_spacing,
                            self.latched_copy_layout,
                        );
                        world
                            .update_body(handle, copy.config(), move_pose)
                            .map_err(|e| e.to_string())?;
                        if prototype.kind == 1 && old.is_some_and(|old| old.kind != 1) {
                            world.set_bullet(handle, false).map_err(|e| e.to_string())?;
                            self.copy_bullet_enabled[index] = false;
                        }
                    }
                }
            }
        }
        self.sync_released_fragment_properties(&bodies)?;
        let physics_start = std::time::Instant::now();
        let mut completed = 0;
        for _ in 0..steps {
            let result = (|| -> Result<(), String> {
                let tick_gravity = self.interpolated_gravity(self.physics_time);
                let span = input_span(self.authored_samples.iter(), Seconds(self.physics_time))
                    .expect("authored input history is seeded before stepping");
                let field_before = span.before.acceleration_field.clone();
                let field_after = span.after.acceleration_field.clone();
                let field_alpha = span.alpha;
                let sampled_field = crate::node_graph::vector_field::ContinuousField {
                    before: field_before.as_ref(),
                    after: field_after.as_ref(),
                    alpha: field_alpha,
                    origin: [0.0; 3],
                };
                let targeted_indices = self
                    .targeted_fields
                    .is_connected()
                    .then_some((span.before_index, span.after_index, span.alpha));
                self.world
                    .as_mut()
                    .expect("world constructed above")
                    .set_gravity(tick_gravity)
                    .map_err(|e| e.to_string())?;
                self.begin_impulse_tick()?;
                self.apply_impulse_tick()?;
                let dynamic_microsteps = self.configure_fast_bodies(
                    &bodies,
                    prototype.as_ref(),
                    tick_gravity,
                    if sampled_field.is_empty() {
                        None
                    } else {
                        Some(&sampled_field)
                    },
                    targeted_indices,
                    TICK,
                )?;
                let (animated_microsteps, animated_speed) =
                    self.animated_microsteps(&bodies, prototype.as_ref(), TICK);
                let microsteps = animated_microsteps.max(dynamic_microsteps);
                self.world
                    .as_mut()
                    .expect("world constructed above")
                    .set_max_linear_speed(animated_speed.max(400.0))
                    .map_err(|e| e.to_string())?;
                let microstep_time = TICK / microsteps as f64;
                let solver_substeps = 4;
                for microstep in 1..=microsteps {
                    let target_time = self.physics_time + microstep_time * microstep as f64;
                    self.prepare_substep(
                        &bodies,
                        prototype.as_ref(),
                        Seconds(target_time),
                        Seconds(microstep_time),
                        &sampled_field,
                        targeted_indices,
                    )?;
                    let world = self.world.as_mut().expect("world constructed above");
                    world
                        .step(Seconds(microstep_time), solver_substeps)
                        .map_err(|e| e.to_string())?;
                }
                completed += 1;
                self.physics_time += TICK;
                self.apply_due_authored_edits()?;
                self.process_fragment_releases(self.physics_time, &bodies)?;
                Ok(())
            })();
            // Delivery records survive a failed native step too. Starting a
            // tick consumes its inputs, but does not assert successful physics.
            self.finish_impulse_tick();
            if let Err(error) = result {
                self.impulse_failure = Some(error.clone());
                return Err(error);
            }
            // A native tick cannot be preempted. Yield before starting another.
            if preview_budget.is_some_and(|budget| physics_start.elapsed() >= budget) {
                break;
            }
        }
        self.pending_time = Seconds((due_steps - completed) as f64 * TICK);
        self.apply_due_authored_edits()?;
        if completed == 0 {
            self.process_fragment_releases(self.physics_time, &bodies)?;
        }
        self.prune_authored_samples()?;
        if self.pending_time.0 > 0.0
            && speed > 0.0
            && self
                .last_overload_warning
                .is_none_or(|last| last.elapsed().as_secs() >= 2)
        {
            log::warn!(
                "Physics preview: {:.1} ms still queued after {completed} ticks; preserving every physics step",
                self.pending_time.0 * 1000.0
            );
            self.last_overload_warning = Some(std::time::Instant::now());
        }
        // Remove only completed ticks. Later preview batches or export drain the rest.
        self.accumulator = (accumulated - completed as f64 * TICK).max(0.0);
        self.last_time = Some(now);
        self.reset_count = Some(reset_count);
        self.descriptions = bodies.clone();
        // Resolve ordinary and released body poses first; inactive fragments
        // inherit their parent's current native pose below.
        for (i, body) in bodies.iter().enumerate().filter(|(i, body)| {
            body.as_ref().is_none_or(|body| {
                body.fragment_parent.is_none() || self.fragment_active[*i]
            })
        }) {
            let Some(body) = body else {
                self.poses[i] = Transform::default();
                continue;
            };
            let Some(handle) = self.handles[i] else {
                self.poses[i] = body.transform;
                continue;
            };
            let pose = self
                .world
                .as_ref()
                .expect("world constructed above")
                .pose(handle)
                .map_err(|e| e.to_string())?;
            let rot_euler = super::primitives::quat_to_render_scene_euler(pose.rotation);
            self.poses[i] = Transform {
                pos: pose.position,
                rot_euler,
                scale: body.transform.scale,
                billboard: false,
            };
            if self.deferred_animated_edit[i].is_some() {
                self.poses[i].pos = body.transform.pos;
                self.poses[i].rot_euler = body.transform.rot_euler;
            }
        }
        for (i, body) in bodies.iter().enumerate() {
            let Some(body) = body else {
                continue;
            };
            let Some(parent_index) = body.fragment_parent else {
                continue;
            };
            if self.fragment_active[i] {
                continue;
            }
            self.poses[i] = Transform {
                pos: self.poses[parent_index].pos,
                rot_euler: self.poses[parent_index].rot_euler,
                scale: body.transform.scale,
                billboard: false,
            };
        }
        if let Some(prototype) = prototype.as_ref() {
            for index in 0..self.active_copy_count {
                let Some(handle) = self.copy_handles[index] else {
                    continue;
                };
                let pose = self
                    .world
                    .as_ref()
                    .expect("world constructed above")
                    .pose(handle)
                    .map_err(|e| e.to_string())?;
                self.copy_poses[index] = Transform {
                    pos: pose.position,
                    rot_euler: super::primitives::quat_to_render_scene_euler(pose.rotation),
                    scale: prototype.transform.scale,
                    billboard: false,
                };
                if self.deferred_copy_animated_edit.is_some() {
                    let authored = copy_transform_for_layout(
                        prototype.clone(),
                        prototype.transform.pos,
                        index,
                        self.active_copy_count,
                        self.latched_copy_columns,
                        self.latched_copy_spacing,
                        self.latched_copy_layout,
                    );
                    self.copy_poses[index].pos = authored.transform.pos;
                    self.copy_poses[index].rot_euler = authored.transform.rot_euler;
                }
            }
        }
        self.copy_description = prototype.clone();
        self.physics_ms = physics_start.elapsed().as_secs_f32() * 1000.0;
        self.accepted_observation = Some((now.0, self.authored_time));
        Ok(())
    }

    fn release_fragments(
        &mut self,
        parent_index: usize,
        parent: &RigidBody,
        release_count: f32,
    ) -> Result<(), String> {
        if self.fragment_parent_released[parent_index] {
            self.fragment_release_latched[parent_index] = release_count;
            return Ok(());
        }
        if !parent.enabled {
            return Ok(());
        }
        let Some(parent_handle) = self.handles[parent_index] else {
            return Ok(());
        };
        let child_count = self
            .descriptions
            .iter()
            .flatten()
            .filter(|body| body.fragment_parent == Some(parent_index))
            .count();
        if child_count == 0 {
            self.fragment_release_latched[parent_index] = release_count;
            return Ok(());
        }

        let (parent_pose, parent_angular, child_velocities) = {
            let world = self.world.as_ref().expect("world constructed above");
            let pose = world.pose(parent_handle).map_err(|e| e.to_string())?;
            let angular = world.angular_velocity(parent_handle).map_err(|e| e.to_string())?;
            let mut velocities = [[0.0; 3]; MAX_BODIES];
            for (index, body) in self.descriptions.iter().enumerate() {
                if body.as_ref().is_some_and(|body| body.fragment_parent == Some(parent_index)) {
                    let Some(handle) = self.handles[index] else { continue };
                    let center = world.local_center_of_mass(handle).map_err(|e| e.to_string())?;
                    velocities[index] = world
                        .velocity_at_local_point(parent_handle, center)
                        .map_err(|e| e.to_string())?;
                }
            }
            (pose, angular, velocities)
        };

        // Remove the intact collider before enabling any prepared fragment.
        let world = self.world.as_mut().expect("world constructed above");
        world
            .set_enabled(parent_handle, false)
            .map_err(|e| e.to_string())?;
        self.fragment_parent_released[parent_index] = true;
        self.fragment_release_latched[parent_index] = release_count;
        let child_mass = parent.mass / child_count as f32;
        for (index, body) in self.descriptions.iter().enumerate() {
            if body.as_ref().is_none_or(|body| body.fragment_parent != Some(parent_index)) {
                continue;
            }
            let Some(handle) = self.handles[index] else { continue };
            let body = body.as_ref().expect("fragment description exists");
            let mut config = body.config();
            config.position = parent_pose.position;
            config.rotation = parent_pose.rotation;
            config.mass = child_mass;
            config.friction = parent.friction;
            config.restitution = parent.bounce;
            world
                .update_body(handle, config, true)
                .map_err(|e| e.to_string())?;
            world.set_enabled(handle, true).map_err(|e| e.to_string())?;
            world
                .set_velocity(handle, child_velocities[index], parent_angular)
                .map_err(|e| e.to_string())?;
            self.fragment_active[index] = true;
        }
        Ok(())
    }

    fn sync_released_fragment_properties(
        &mut self,
        bodies: &[Option<RigidBody>; MAX_BODIES],
    ) -> Result<(), String> {
        for (parent_index, released) in self.fragment_parent_released.iter().copied().enumerate() {
            if !released {
                continue;
            }
            let Some(parent) = bodies[parent_index].as_ref() else {
                continue;
            };
            let parent_properties_changed = self.descriptions[parent_index]
                .as_ref()
                .is_none_or(|previous| {
                    previous.mass != parent.mass
                        || previous.friction != parent.friction
                        || previous.bounce != parent.bounce
                });
            if !parent_properties_changed {
                continue;
            }
            let child_count = bodies
                .iter()
                .flatten()
                .filter(|body| body.fragment_parent == Some(parent_index))
                .count();
            if child_count == 0 {
                continue;
            }
            let child_mass = parent.mass / child_count as f32;
            for (index, body) in bodies.iter().enumerate() {
                let Some(body) = body.as_ref() else {
                    continue;
                };
                if body.fragment_parent != Some(parent_index) || !self.fragment_active[index] {
                    continue;
                }
                let Some(handle) = self.handles[index] else {
                    continue;
                };
                let mut config = body.config();
                config.mass = child_mass;
                config.friction = parent.friction;
                config.restitution = parent.bounce;
                self.world
                    .as_mut()
                    .expect("world constructed above")
                    .update_body(handle, config, false)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    fn authored_release_count(&self, index: usize, time: f64) -> Option<f32> {
        let first = self.authored_samples.front()?.bodies[index].as_ref()?;
        let mut count = first.release_count;
        for sample in self.authored_samples.iter().skip(1) {
            if sample.time > time + 1.0e-12 {
                break;
            }
            if let Some(body) = sample.bodies[index].as_ref() {
                count = body.release_count;
            }
        }
        Some(count)
    }

    fn process_fragment_releases(
        &mut self,
        time: f64,
        bodies: &[Option<RigidBody>; MAX_BODIES],
    ) -> Result<(), String> {
        for (index, body) in bodies.iter().enumerate() {
            let Some(body) = body.as_ref() else { continue };
            if !body.enabled
                || body.fragment_parent.is_some()
                || self.fragment_parent_released[index]
                || body.release_count <= self.fragment_release_latched[index]
            {
                continue;
            }
            let count = self.authored_release_count(index, time).unwrap_or(body.release_count);
            if count > self.fragment_release_latched[index] {
                self.release_fragments(index, body, count)?;
            }
        }
        Ok(())
    }

    fn apply_due_authored_edits(&mut self) -> Result<(), String> {
        const EPSILON: f64 = 1.0e-12;
        let world = self.world.as_mut().expect("world constructed above");
        for (index, edit_time) in self.deferred_animated_edit.iter_mut().enumerate() {
            let Some(edit) = edit_time.as_ref() else {
                continue;
            };
            if self.physics_time + EPSILON < edit.time {
                continue;
            }
            if let Some(handle) = self.handles[index] {
                world
                    .update_body(handle, edit.body.config(), true)
                    .map_err(|e| e.to_string())?;
            }
            *edit_time = None;
        }
        if let Some(edit) = self
            .deferred_copy_animated_edit
            .as_ref()
            .filter(|edit| self.physics_time + EPSILON >= edit.time)
        {
            let prototype = &edit.body;
            for index in 0..self.active_copy_count {
                let Some(handle) = self.copy_handles[index] else {
                    continue;
                };
                let copy = copy_transform_for_layout(
                    prototype.clone(),
                    prototype.transform.pos,
                    index,
                    self.active_copy_count,
                    self.latched_copy_columns,
                    self.latched_copy_spacing,
                    self.latched_copy_layout,
                );
                world
                    .update_body(handle, copy.config(), true)
                    .map_err(|e| e.to_string())?;
            }
            self.deferred_copy_animated_edit = None;
        }
        Ok(())
    }

    fn record_authored_sample(
        &mut self,
        time: f64,
        bodies: [Option<RigidBody>; MAX_BODIES],
        prototype: Option<RigidBody>,
        gravity: [f32; 3],
        acceleration_field: Option<FieldValue>,
        targeted_fields: Option<&[Option<FieldValue>]>,
    ) -> Result<(), String> {
        if self.authored_samples.is_exhausted() {
            return Err(
                "Physics: authored input history is exhausted; reset the simulation to continue"
                    .into(),
            );
        }
        if self.authored_samples.back().is_some_and(|last| {
            last.time == time
                && same_body_arrays(&last.bodies, &bodies)
                && same_optional_body(last.prototype.as_ref(), prototype.as_ref())
                && last.gravity == gravity
                && last.acceleration_field == acceleration_field
                && targeted_fields.is_none_or(|fields| self.targeted_fields.fields_equal(fields))
        }) {
            return Ok(());
        }
        let write = self
            .authored_samples
            .record(
                AuthoredPoseSample {
                    time,
                    bodies,
                    prototype,
                    gravity,
                    acceleration_field,
                },
                Seconds(self.physics_time),
            )
            .map_err(|error| format!("Physics: input history rejected authored sample: {error}"))?;
        if let Some(fields) = targeted_fields {
            self.targeted_fields
                .record(fields, write)?;
        }
        Ok(())
    }

    fn ensure_targeted_history(
        &mut self,
        targeted_fields: Option<&[Option<FieldValue>]>,
    ) -> Result<(), String> {
        if targeted_fields.is_some() && !self.targeted_fields.is_connected() {
            self.targeted_fields
                .ensure_aligned(self.authored_samples.len())?;
        }
        Ok(())
    }

    fn configure_fast_bodies(
        &mut self,
        bodies: &[Option<RigidBody>; MAX_BODIES],
        prototype: Option<&RigidBody>,
        gravity: [f32; 3],
        acceleration_field: Option<&dyn VectorField>,
        targeted_indices: Option<(usize, usize, f32)>,
        tick: f64,
    ) -> Result<usize, String> {
        // Box3D skips bullet targets during the bullet pass. When two fast
        // individual Dynamics share a world, use smaller outer steps instead
        // of making each invisible to the other's continuous pass.
        let targeted_span = match targeted_indices {
            Some((before, after, alpha)) => {
                Some(self.targeted_fields.span(before, after, alpha)?)
            }
            None => None,
        };
        let world = self.world.as_mut().expect("world constructed above");
        let mut fast_count = 0;
        let mut fast_steps = 1;
        if self.active_copy_count == 0 {
            for (index, body) in bodies.iter().enumerate() {
                let (Some(body), Some(handle)) = (body, self.handles[index]) else {
                    continue;
                };
                if (body.fragment_parent.is_some() && !self.fragment_active[index])
                    || self.fragment_parent_released[index]
                {
                    continue;
                }
                if !body.enabled || body.kind != 1 {
                    continue;
                }
                let velocity = world.linear_velocity(handle).map_err(|e| e.to_string())?;
                let targeted = targeted_span.as_ref().map(|span| span.field(index));
                let acceleration = summed_acceleration(
                    world,
                    handle,
                    gravity,
                    acceleration_field,
                    targeted.as_ref().filter(|field| !field.is_empty()).map(|field| field as &dyn VectorField),
                )?;
                if needs_bullet(body, velocity, acceleration, tick) {
                    fast_count += 1;
                    let extent = body_min_extent(body);
                    fast_steps = fast_steps.max(
                        (predicted_dynamic_travel(velocity, acceleration, tick) / (extent * 0.5))
                            .ceil()
                            .clamp(1.0, 512.0) as usize,
                    );
                }
            }
        }
        for (index, body) in bodies.iter().enumerate() {
            let (Some(body), Some(handle)) = (body, self.handles[index]) else {
                continue;
            };
            if (body.fragment_parent.is_some() && !self.fragment_active[index])
                || self.fragment_parent_released[index]
            {
                continue;
            }
            if !body.enabled || body.kind != 1 {
                self.bullet_enabled[index] = false;
                continue;
            }
            let velocity = world.linear_velocity(handle).map_err(|e| e.to_string())?;
            let targeted = targeted_span.as_ref().map(|span| span.field(index));
            let acceleration = summed_acceleration(
                world,
                handle,
                gravity,
                acceleration_field,
                targeted.as_ref().filter(|field| !field.is_empty()).map(|field| field as &dyn VectorField),
            )?;
            let enabled = fast_count < 2 && needs_bullet(body, velocity, acceleration, tick);
            if self.bullet_enabled[index] != enabled {
                world
                    .set_bullet(handle, enabled)
                    .map_err(|e| e.to_string())?;
                self.bullet_enabled[index] = enabled;
            }
        }
        if let Some(prototype) = prototype.filter(|body| body.enabled && body.kind == 1) {
            for index in 0..self.active_copy_count {
                let Some(handle) = self.copy_handles[index] else {
                    continue;
                };
                let velocity = world.linear_velocity(handle).map_err(|e| e.to_string())?;
                let targeted = targeted_span.as_ref().map(|span| span.field(MAX_BODIES));
                let acceleration = summed_acceleration(
                    world,
                    handle,
                    gravity,
                    acceleration_field,
                    targeted.as_ref().filter(|field| !field.is_empty()).map(|field| field as &dyn VectorField),
                )?;
                let enabled = needs_bullet(prototype, velocity, acceleration, tick);
                if self.copy_bullet_enabled[index] != enabled {
                    world
                        .set_bullet(handle, enabled)
                        .map_err(|e| e.to_string())?;
                    self.copy_bullet_enabled[index] = enabled;
                }
            }
        } else {
            self.copy_bullet_enabled[..self.active_copy_count].fill(false);
        }
        Ok(if fast_count >= 2 { fast_steps } else { 1 })
    }

    /// Queue this interval's authored motion and existing continuous fields.
    /// The native step consumes forces; a coupled owner can use this same
    /// preparation before each accepted fluid/rigid subdivision.
    fn prepare_substep(
        &mut self,
        bodies: &[Option<RigidBody>; MAX_BODIES],
        prototype: Option<&RigidBody>,
        target_time: Seconds,
        dt: Seconds,
        global: &crate::node_graph::vector_field::ContinuousField<'_>,
        indices: Option<(usize, usize, f32)>,
    ) -> Result<(), String> {
        let mut targets: [Option<RigidBody>; MAX_BODIES] = std::array::from_fn(|_| None);
        for (i, body) in bodies.iter().enumerate() {
            let Some(body) = body else { continue };
            if (body.fragment_parent.is_some() && !self.fragment_active[i])
                || self.fragment_parent_released[i]
            {
                continue;
            }
            if body.enabled && body.kind == 2 {
                targets[i] = Some(
                    self.interpolated_body(i, target_time.0, body.clone())
                        .unwrap_or_else(|| body.clone()),
                );
            }
        }
        let copy_target = prototype
            .filter(|prototype| prototype.enabled && prototype.kind == 2)
            .map(|prototype| {
                self.interpolated_prototype(target_time.0, prototype.clone())
                    .unwrap_or_else(|| prototype.clone())
            });
        let world = self.world.as_mut().expect("world constructed above");
        for (i, target) in targets.into_iter().enumerate() {
            let Some(target) = target else { continue };
            let Some(handle) = self.handles[i] else {
                continue;
            };
            world
                .set_animated_target(handle, target.config(), dt)
                .map_err(|e| e.to_string())?;
        }
        if let Some(prototype) = copy_target {
            for index in 0..self.active_copy_count {
                let Some(handle) = self.copy_handles[index] else {
                    continue;
                };
                let target = copy_transform_for_layout(
                    prototype.clone(),
                    prototype.transform.pos,
                    index,
                    self.active_copy_count,
                    self.latched_copy_columns,
                    self.latched_copy_spacing,
                    self.latched_copy_layout,
                );
                world
                    .set_animated_target(handle, target.config(), dt)
                    .map_err(|e| e.to_string())?;
            }
        }
        self.apply_sampled_fields(dt, global, indices)
    }

    fn apply_sampled_fields(
        &mut self,
        dt: Seconds,
        global: &crate::node_graph::vector_field::ContinuousField<'_>,
        indices: Option<(usize, usize, f32)>,
    ) -> Result<(), String> {
        let Some((before, after, alpha)) = indices else {
            if global.is_empty() {
                return Ok(());
            }
            return self.world.as_mut().expect("world constructed above")
                .apply_fields(&self.field_handles, &[FieldInput {
                    field: global, acceleration: 1.0, delta_velocity: 0.0,
                }], dt).map_err(|error| error.to_string());
        };
        let span = self.targeted_fields.span(before, after, alpha)?;
        let fields: [_; TARGET_SLOTS] = std::array::from_fn(|index| span.field(index));
        let inputs: [[FieldInput<'_>; 2]; TARGET_SLOTS] = std::array::from_fn(|index| [
            FieldInput { field: global, acceleration: 1.0, delta_velocity: 0.0 },
            FieldInput { field: &fields[index], acceleration: 1.0, delta_velocity: 0.0 },
        ]);
        let recipients = self.handles.iter().enumerate().filter_map(|(index, handle)| {
            handle.map(|handle| (handle, inputs[index].as_slice()))
        }).chain(self.copy_handles[..self.active_copy_count].iter().flatten()
            .map(|&handle| (handle, inputs[MAX_BODIES].as_slice())));
        // Validate the complete batch before any body receives a force. Copies
        // share one input slice and one linear validation pass.
        self.world.as_mut().expect("world constructed above")
            .apply_fields_by_target(recipients, dt)
            .map_err(|error| error.to_string())
    }

    fn animated_microsteps(
        &self,
        bodies: &[Option<RigidBody>; MAX_BODIES],
        prototype: Option<&RigidBody>,
        tick: f64,
    ) -> (usize, f32) {
        // Box3D bullet CCD does not sweep Animated motion. Smaller outer
        // steps put fast moving/rotating colliders into contact with Dynamics.
        const MAX_MICROSTEPS: usize = 512;
        let dynamic_extent = bodies
            .iter()
            .enumerate()
            .filter_map(|(index, body)| {
                body.as_ref().filter(|body| {
                    body.enabled
                        && body.kind == 1
                        && (body.fragment_parent.is_none() || self.fragment_active[index])
                        && !self.fragment_parent_released[index]
                })
            })
            .map(body_min_extent)
            .chain(
                prototype
                    .filter(|body| body.enabled && body.kind == 1 && self.active_copy_count > 0)
                    .map(body_min_extent),
            )
            .fold(f32::INFINITY, f32::min);
        if !dynamic_extent.is_finite() {
            return (1, 400.0);
        }
        let start_time = self.physics_time;
        let end_time = start_time + tick;
        let mut travel: f32 = 0.0;
        let mut required_speed: f32 = 400.0;
        for (index, body) in bodies.iter().enumerate() {
            let Some(body) = body.as_ref() else { continue };
            if !body.enabled
                || body.kind != 2
                || (body.fragment_parent.is_some() && !self.fragment_active[index])
                || self.fragment_parent_released[index]
            {
                continue;
            }
            let start = self
                .interpolated_body(index, start_time, body.clone())
                .unwrap_or_else(|| body.clone());
            let mut previous = start;
            let mut previous_time = start_time;
            let mut path = 0.0;
            for sample in self
                .authored_samples
                .iter()
                .filter(|sample| sample.time > start_time && sample.time < end_time)
            {
                if let Some(next) = sample.bodies[index].as_ref() {
                    if sample.time - previous_time <= 1.0e-9 {
                        previous = next.clone();
                        continue;
                    }
                    let segment = animated_sweep_distance(&previous, next);
                    path += segment;
                    required_speed =
                        required_speed.max(segment / (sample.time - previous_time) as f32);
                    previous = next.clone();
                    previous_time = sample.time;
                }
            }
            let end = self
                .interpolated_body(index, end_time, body.clone())
                .unwrap_or_else(|| body.clone());
            let segment = animated_sweep_distance(&previous, &end);
            if end_time - previous_time > 1.0e-9 {
                path += segment;
                required_speed = required_speed.max(segment / (end_time - previous_time) as f32);
            }
            travel = travel.max(path);
        }
        if let Some(body) = prototype.filter(|body| body.enabled && body.kind == 2 && self.active_copy_count > 0) {
            let start = self
                .interpolated_prototype(start_time, body.clone())
                .unwrap_or_else(|| body.clone());
            let mut previous = start;
            let mut previous_time = start_time;
            let mut path = 0.0;
            for sample in self
                .authored_samples
                .iter()
                .filter(|sample| sample.time > start_time && sample.time < end_time)
            {
                if let Some(next) = sample.prototype.as_ref() {
                    if sample.time - previous_time <= 1.0e-9 {
                        previous = next.clone();
                        continue;
                    }
                    let segment = animated_sweep_distance(&previous, next);
                    path += segment;
                    required_speed =
                        required_speed.max(segment / (sample.time - previous_time) as f32);
                    previous = next.clone();
                    previous_time = sample.time;
                }
            }
            let end = self
                .interpolated_prototype(end_time, body.clone())
                .unwrap_or_else(|| body.clone());
            let segment = animated_sweep_distance(&previous, &end);
            if end_time - previous_time > 1.0e-9 {
                path += segment;
                required_speed = required_speed.max(segment / (end_time - previous_time) as f32);
            }
            travel = travel.max(path);
        }
        let safe_step = (dynamic_extent * 0.5).max(0.001);
        (
            (travel / safe_step)
                .ceil()
                .clamp(1.0, MAX_MICROSTEPS as f32) as usize,
            required_speed,
        )
    }

    fn interpolated_body(
        &self,
        index: usize,
        time: f64,
        mut current: RigidBody,
    ) -> Option<RigidBody> {
        let span = input_span_before(self.authored_samples.iter(), Seconds(time))?;
        let previous = span.before.bodies[index].clone()?;
        let next = span.after.bodies[index].clone()?;
        let pose = interpolate_body(previous, next, span.alpha).transform;
        current.transform.pos = pose.pos;
        current.transform.rot_euler = pose.rot_euler;
        Some(current)
    }

    fn interpolated_prototype(&self, time: f64, mut current: RigidBody) -> Option<RigidBody> {
        let span = input_span_before(self.authored_samples.iter(), Seconds(time))?;
        let previous = span.before.prototype.clone()?;
        let next = span.after.prototype.clone()?;
        let pose = interpolate_body(previous, next, span.alpha).transform;
        current.transform.pos = pose.pos;
        current.transform.rot_euler = pose.rot_euler;
        Some(current)
    }

    fn interpolated_gravity(&self, time: f64) -> [f32; 3] {
        let span = input_span(self.authored_samples.iter(), Seconds(time))
            .expect("authored input history is seeded before stepping");
        std::array::from_fn(|axis| {
            span.before.gravity[axis]
                + (span.after.gravity[axis] - span.before.gravity[axis]) * span.alpha
        })
    }

    fn prune_authored_samples(&mut self) -> Result<(), String> {
        let removed = self
            .authored_samples
            .prune_before(Seconds(self.physics_time + 1.0e-12))
            .map_err(|error| format!("Physics: failed to prune input history: {error}"))?;
        self.targeted_fields.prune(removed)?;
        Ok(())
    }
}

fn same_optional_body(left: Option<&RigidBody>, right: Option<&RigidBody>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => same_authored_body(left, right),
        _ => false,
    }
}

fn same_body_arrays(
    left: &[Option<RigidBody>; MAX_BODIES],
    right: &[Option<RigidBody>; MAX_BODIES],
) -> bool {
    left.iter()
        .zip(right)
        .all(|(left, right)| same_optional_body(left.as_ref(), right.as_ref()))
}

fn interpolate_body(mut previous: RigidBody, next: RigidBody, alpha: f32) -> RigidBody {
    for axis in 0..3 {
        previous.transform.pos[axis] = previous.transform.pos[axis]
            + (next.transform.pos[axis] - previous.transform.pos[axis]) * alpha;
        previous.transform.rot_euler[axis] = previous.transform.rot_euler[axis]
            + (next.transform.rot_euler[axis] - previous.transform.rot_euler[axis]) * alpha;
    }
    previous
}

fn animated_sweep_distance(start: &RigidBody, end: &RigidBody) -> f32 {
    let mut linear_squared = 0.0;
    let mut angular = 0.0;
    for axis in 0..3 {
        let delta = end.transform.pos[axis] - start.transform.pos[axis];
        linear_squared += delta * delta;
        angular += (end.transform.rot_euler[axis] - start.transform.rot_euler[axis]).abs();
    }
    let radius = body_collision_radius(start);
    linear_squared.sqrt() + angular * radius
}

fn summed_acceleration(
    world: &PhysicsWorld,
    handle: BodyHandle,
    gravity: [f32; 3],
    field: Option<&dyn VectorField>,
    targeted_field: Option<&dyn VectorField>,
) -> Result<[f32; 3], String> {
    let mut acceleration = gravity;
    if let Some(field) = field {
        let position = world
            .pose(handle)
            .map_err(|error| error.to_string())?
            .position;
        let sample = field.sample(position);
        for axis in 0..3 {
            acceleration[axis] += sample[axis];
        }
    }
    if let Some(field) = targeted_field {
        let sample = field.sample(
            world
                .pose(handle)
                .map_err(|error| error.to_string())?
                .position,
        );
        for axis in 0..3 {
            acceleration[axis] += sample[axis];
        }
    }
    if acceleration.iter().all(|component| component.is_finite()) {
        Ok(acceleration)
    } else {
        Err("Physics: acceleration field result must be finite".into())
    }
}

fn needs_bullet(body: &RigidBody, velocity: [f32; 3], acceleration: [f32; 3], tick: f64) -> bool {
    let predicted_travel = predicted_dynamic_travel(velocity, acceleration, tick);
    let extent = body_min_extent(body);
    predicted_travel > extent * 0.5
}

fn body_min_extent(body: &RigidBody) -> f32 {
    let Some(geometry) = body.collider.as_ref() else {
        return body
            .transform
            .scale
            .into_iter()
            .fold(f32::INFINITY, f32::min);
    };
    let mut minimum = [f32::INFINITY; 3];
    let mut maximum = [f32::NEG_INFINITY; 3];
    for hull in &geometry.hulls {
        for point in hull {
            for axis in 0..3 {
                let scaled = point[axis] * body.transform.scale[axis];
                minimum[axis] = minimum[axis].min(scaled);
                maximum[axis] = maximum[axis].max(scaled);
            }
        }
    }
    let extent = minimum
        .into_iter()
        .zip(maximum)
        .map(|(min, max)| (max - min).abs() * 0.5)
        .fold(f32::INFINITY, f32::min);
    if extent.is_finite() && extent > 0.0 {
        extent
    } else {
        body.transform
            .scale
            .into_iter()
            .fold(f32::INFINITY, f32::min)
    }
}

fn body_collision_radius(body: &RigidBody) -> f32 {
    let Some(geometry) = body.collider.as_ref() else {
        return body.transform.scale.into_iter().fold(0.0, f32::max);
    };
    let radius = geometry
        .hulls
        .iter()
        .flat_map(|hull| hull.iter())
        .map(|point| {
            point
                .iter()
                .enumerate()
                .map(|(axis, value)| value * body.transform.scale[axis])
                .map(|value| value * value)
                .sum::<f32>()
                .sqrt()
        })
        .fold(0.0, f32::max);
    if radius > 0.0 && radius.is_finite() {
        radius
    } else {
        body.transform.scale.into_iter().fold(0.0, f32::max)
    }
}

fn predicted_dynamic_travel(velocity: [f32; 3], acceleration: [f32; 3], tick: f64) -> f32 {
    let speed = velocity.into_iter().map(|v| v * v).sum::<f32>().sqrt();
    let acceleration = acceleration.into_iter().map(|v| v * v).sum::<f32>().sqrt();
    let tick = tick as f32;
    speed * tick + 0.5 * acceleration * tick * tick
}

fn validate_fragments(bodies: &[Option<RigidBody>; MAX_BODIES]) -> Result<(), String> {
    for (index, body) in bodies.iter().enumerate() {
        let Some(body) = body else { continue };
        if !body.release_count.is_finite() || body.release_count < 0.0 {
            return Err(format!("Physics: body {index} release count must be finite and non-negative"));
        }
        let Some(parent) = body.fragment_parent else {
            continue;
        };
        if parent >= MAX_BODIES || parent == index {
            return Err(format!("Physics: body {index} has an invalid fragment parent"));
        }
        let Some(parent_body) = bodies[parent].as_ref() else {
            return Err(format!("Physics: body {index} references a missing fragment parent"));
        };
        if parent_body.fragment_parent.is_some() {
            return Err(format!("Physics: fragment parent chains are not supported (body {index})"));
        }
    }
    Ok(())
}

fn validate_copy_prototype(prototype: &RigidBody) -> Result<(), String> {
    if prototype.shape >= 5
        || prototype.kind > 2
        || prototype.transform.billboard
        || prototype
            .transform
            .scale
            .iter()
            .any(|s| !s.is_finite() || *s <= 0.0 || *s > 100.0)
    {
        return Err("Physics: copy prototype needs a Platonic shape, positive scale up to 100, and no billboard".into());
    }
    let scale = prototype.transform.scale[0];
    if prototype
        .transform
        .scale
        .iter()
        .any(|axis| (*axis - scale).abs() > 1.0e-5)
    {
        return Err("Physics: copy prototype scale must be uniform".into());
    }
    Ok(())
}

fn copy_transform_for_layout(
    mut copy: RigidBody,
    origin: [f32; 3],
    index: usize,
    count: usize,
    columns: usize,
    spacing: f32,
    layout: CopyLayout,
) -> RigidBody {
    copy.transform.pos = match layout {
        CopyLayout::Grid => copy_position(origin, index, count, columns, spacing),
        CopyLayout::Pile => pile_position(origin, index, count, columns, spacing),
    };
    if layout == CopyLayout::Pile {
        let offsets = pile_rotation(index);
        for (axis, offset) in offsets.into_iter().enumerate() {
            copy.transform.rot_euler[axis] += offset;
        }
    }
    copy
}

fn pile_position(
    origin: [f32; 3],
    index: usize,
    count: usize,
    copy_columns: usize,
    spacing: f32,
) -> [f32; 3] {
    let columns = ceil_cuberoot(count).min(copy_columns.max(1));
    let column = index % columns;
    let row = (index / columns) % columns;
    let layer = index / (columns * columns);
    let center = (columns as f32 - 1.0) * 0.5;
    [
        origin[0] + (column as f32 - center) * spacing + bounded_jitter(index, 0, spacing),
        origin[1] + layer as f32 * spacing + bounded_jitter(index, 1, spacing),
        origin[2] + (row as f32 - center) * spacing + bounded_jitter(index, 2, spacing),
    ]
}

fn ceil_cuberoot(count: usize) -> usize {
    let mut columns: usize = 1;
    while columns.saturating_mul(columns).saturating_mul(columns) < count {
        columns += 1;
    }
    columns
}

fn pile_rotation(index: usize) -> [f32; 3] {
    [
        full_turn_hash(index, 3),
        full_turn_hash(index, 4),
        full_turn_hash(index, 5),
    ]
}

fn bounded_jitter(index: usize, axis: u32, spacing: f32) -> f32 {
    (index_hash_unit(index, axis) * 2.0 - 1.0) * 0.04 * spacing
}

fn full_turn_hash(index: usize, salt: u32) -> f32 {
    index_hash_unit(index, salt) * std::f32::consts::TAU
}

fn index_hash_unit(index: usize, salt: u32) -> f32 {
    let mut value = (index as u32).wrapping_add(salt.wrapping_mul(0x9e37_79b9));
    value ^= value >> 16;
    value = value.wrapping_mul(0x85eb_ca6b);
    value ^= value >> 13;
    value = value.wrapping_mul(0xc2b2_ae35);
    value ^= value >> 16;
    value as f32 / u32::MAX as f32
}

fn copy_position(
    origin: [f32; 3],
    index: usize,
    count: usize,
    columns: usize,
    spacing: f32,
) -> [f32; 3] {
    let rows = count.div_ceil(columns).min(columns);
    let column = index % columns;
    let row = (index / columns) % columns;
    let layer = index / (columns * columns);
    [
        origin[0] + (column as f32 - (columns as f32 - 1.0) * 0.5) * spacing,
        origin[1] + layer as f32 * spacing,
        origin[2] + (row as f32 - (rows as f32 - 1.0) * 0.5) * spacing,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRAVITY: [f32; 3] = [0.0, -9.8, 0.0];
    const FRAME: f64 = 1.0 / 60.0;

    #[test]
    fn full_copy_grid_stays_over_floor_and_stacks_in_layers() {
        let first = copy_position([0.0, 4.0, 0.0], 0, MAX_COPIES, 16, 1.5);
        let next_layer = copy_position([0.0, 4.0, 0.0], 256, MAX_COPIES, 16, 1.5);
        assert_eq!(next_layer, [first[0], 5.5, first[2]]);
        for index in 0..MAX_COPIES {
            let p = copy_position([0.0, 4.0, 0.0], index, MAX_COPIES, 16, 1.5);
            assert!(p[0].abs() <= 11.25 && p[2].abs() <= 11.25);
            assert!((4.0..=26.5).contains(&p[1]));
        }
    }

    fn body(position: [f32; 3]) -> RigidBody {
        RigidBody {
            transform: Transform {
                pos: position,
                ..Transform::default()
            },
            ..RigidBody::default()
        }
    }

    fn one_body(position: [f32; 3]) -> [Option<RigidBody>; MAX_BODIES] {
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(body(position));
        bodies
    }

    #[test]
    fn fixed_tick_results_are_frame_partition_invariant() {
        let bodies = one_body([0.0, 4.0, 0.0]);
        let mut per_frame = RigidSimulation::default();
        let mut partitioned = RigidSimulation::default();
        per_frame
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        partitioned
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();

        for frame in 1..=60 {
            per_frame
                .advance(
                    bodies.clone(),
                    GRAVITY,
                    Seconds(frame as f64 * FRAME),
                    1.0,
                    0.0,
                )
                .unwrap();
        }
        for half_frame in 1..=120 {
            partitioned
                .advance(
                    bodies.clone(),
                    GRAVITY,
                    Seconds(half_frame as f64 * FRAME / 2.0),
                    1.0,
                    0.0,
                )
                .unwrap();
        }

        assert!((per_frame.poses[0].pos[1] - partitioned.poses[0].pos[1]).abs() < 1.0e-5);
        assert!((per_frame.poses[0].pos[0] - partitioned.poses[0].pos[0]).abs() < 1.0e-5);
    }

    #[test]
    fn uniform_field_is_mass_independent() {
        let field = FieldValue::uniform([0.0, -4.0, 0.0]).unwrap();
        let mut light_body = one_body([0.0, 8.0, 0.0]);
        let mut heavy_body = one_body([10.0, 8.0, 0.0]);
        light_body[0].as_mut().unwrap().mass = 1.0;
        heavy_body[0].as_mut().unwrap().mass = 7.0;
        let mut light = RigidSimulation::default();
        let mut heavy = RigidSimulation::default();
        light
            .advance_with_fields(
                light_body.clone(),
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Seconds::ZERO,
                1.0,
                0.0,
                Some(field.clone()),
            )
            .unwrap();
        heavy
            .advance_with_fields(
                heavy_body.clone(),
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Seconds::ZERO,
                1.0,
                0.0,
                Some(field.clone()),
            )
            .unwrap();
        light
            .advance_with_fields(
                light_body,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Seconds(1.0),
                1.0,
                0.0,
                Some(field.clone()),
            )
            .unwrap();
        heavy
            .advance_with_fields(
                heavy_body,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Seconds(1.0),
                1.0,
                0.0,
                Some(field),
            )
            .unwrap();

        assert!((light.poses[0].pos[1] - heavy.poses[0].pos[1]).abs() < 1.0e-5);
        let light_velocity = light
            .world
            .as_ref()
            .unwrap()
            .linear_velocity(light.handles[0].unwrap())
            .unwrap();
        let heavy_velocity = heavy
            .world
            .as_ref()
            .unwrap()
            .linear_velocity(heavy.handles[0].unwrap())
            .unwrap();
        for (actual, expected) in light_velocity.iter().zip(heavy_velocity) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
        assert!((light_velocity[1] + 4.0).abs() < 0.1);
    }

    #[test]
    fn fixed_bodies_ignore_field_while_copies_receive_it() {
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(RigidBody {
            kind: 0,
            transform: Transform {
                pos: [0.0, -100.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        let prototype = body([0.0, 8.0, 0.0]);
        let field = FieldValue::uniform([0.0, -4.0, 0.0]).unwrap();
        let mut simulation = RigidSimulation::default();
        simulation
            .advance_with_fields(
                bodies.clone(),
                Some(prototype.clone()),
                1.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Seconds::ZERO,
                1.0,
                0.0,
                Some(field.clone()),
            )
            .unwrap();
        simulation
            .advance_with_fields(
                bodies,
                Some(prototype),
                1.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Seconds(1.0),
                1.0,
                0.0,
                Some(field),
            )
            .unwrap();

        assert_eq!(simulation.poses[0].pos, [0.0, -100.0, 0.0]);
        assert!(simulation.copy_poses[0].pos[1] < 8.0);
    }

    fn run_uniform_field_trace(fps: usize) -> ([f32; 3], [f32; 3]) {
        let bodies = one_body([0.0, 8.0, 0.0]);
        let field = FieldValue::uniform([1.0, -4.0, 0.5]).unwrap();
        let mut simulation = RigidSimulation::default();
        simulation
            .advance_with_fields(
                bodies.clone(),
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Seconds::ZERO,
                1.0,
                0.0,
                Some(field.clone()),
            )
            .unwrap();
        for frame in 1..=fps {
            simulation
                .advance_with_fields(
                    bodies.clone(),
                    None,
                    0.0,
                    1.25,
                    16.0,
                    0.0,
                    [0.0; 3],
                    Seconds(frame as f64 / fps as f64),
                    1.0,
                    0.0,
                    Some(field.clone()),
                )
                .unwrap();
        }
        let velocity = simulation
            .world
            .as_ref()
            .unwrap()
            .linear_velocity(simulation.handles[0].unwrap())
            .unwrap();
        (simulation.poses[0].pos, velocity)
    }

    #[test]
    fn uniform_field_trace_is_render_partition_invariant() {
        let traces = [24, 30, 60].map(run_uniform_field_trace);
        for trace in traces.iter().skip(1) {
            for (actual, expected) in trace.0.iter().zip(traces[0].0) {
                assert!((actual - expected).abs() < 1.0e-5);
            }
            for (actual, expected) in trace.1.iter().zip(traces[0].1) {
                assert!((actual - expected).abs() < 1.0e-5);
            }
        }
    }

    #[test]
    fn changed_field_waits_behind_preview_debt() {
        let bodies = one_body([0.0, 8.0, 0.0]);
        let old_field = FieldValue::uniform([0.0, -4.0, 0.0]).unwrap();
        let new_field = FieldValue::uniform([0.0, 8.0, 0.0]).unwrap();
        let mut expected = RigidSimulation::default();
        expected
            .advance_with_fields(
                bodies.clone(),
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Seconds::ZERO,
                1.0,
                0.0,
                Some(old_field.clone()),
            )
            .unwrap();
        expected
            .advance_with_fields(
                bodies.clone(),
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Seconds(0.5),
                1.0,
                0.0,
                Some(old_field.clone()),
            )
            .unwrap();
        expected
            .advance_with_fields(
                bodies.clone(),
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Seconds(0.5),
                1.0,
                0.0,
                Some(new_field.clone()),
            )
            .unwrap();

        let mut queued = RigidSimulation::default();
        queued
            .advance_with_fields(
                bodies.clone(),
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Seconds::ZERO,
                1.0,
                0.0,
                Some(old_field.clone()),
            )
            .unwrap();
        {
            let _scope = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
            queued
                .advance_with_fields(
                    bodies.clone(),
                    None,
                    0.0,
                    1.25,
                    16.0,
                    0.0,
                    [0.0; 3],
                    Seconds(0.5),
                    1.0,
                    0.0,
                    Some(old_field),
                )
                .unwrap();
            assert!(queued.pending_time.0 > 0.0);
            let body_identity = queued.handles[0];
            let physics_time_before_edit = queued.physics_time;
            let pose_before_edit = queued.poses[0].pos;
            queued
                .advance_with_fields(
                    bodies.clone(),
                    None,
                    0.0,
                    1.25,
                    16.0,
                    0.0,
                    [0.0; 3],
                    Seconds(0.5),
                    1.0,
                    0.0,
                    Some(new_field.clone()),
                )
                .unwrap();
            assert!(queued.physics_time > physics_time_before_edit);
            assert_ne!(queued.poses[0].pos, pose_before_edit);
            assert_eq!(queued.handles[0], body_identity, "field edits must retain the native body");
        }
        while queued.pending_time.0 > 0.0 {
            queued
                .advance_with_fields(
                    bodies.clone(),
                    None,
                    0.0,
                    1.25,
                    16.0,
                    0.0,
                    [0.0; 3],
                    Seconds(0.5),
                    1.0,
                    0.0,
                    Some(new_field.clone()),
                )
                .unwrap();
        }

        assert_eq!(queued.poses, expected.poses);
        let queued_velocity = queued
            .world
            .as_ref()
            .unwrap()
            .linear_velocity(queued.handles[0].unwrap())
            .unwrap();
        let expected_velocity = expected
            .world
            .as_ref()
            .unwrap()
            .linear_velocity(expected.handles[0].unwrap())
            .unwrap();
        for (actual, expected) in queued_velocity.iter().zip(expected_velocity) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
    }

    fn target_slots(index: usize, field: FieldValue) -> [Option<FieldValue>; TARGET_SLOTS] {
        let mut fields = std::array::from_fn(|_| None);
        fields[index] = Some(field);
        fields
    }

    fn advance_targeted(
        simulation: &mut RigidSimulation,
        bodies: [Option<RigidBody>; MAX_BODIES],
        prototype: Option<RigidBody>,
        now: f64,
        gravity: [f32; 3],
        targeted_fields: &[Option<FieldValue>],
    ) {
        simulation
            .advance_with_targeted_fields(
                bodies,
                prototype,
                1.0,
                1.25,
                16.0,
                0.0,
                gravity,
                Seconds(now),
                1.0,
                0.0,
                None,
                targeted_fields,
            )
            .unwrap();
    }

    #[test]
    fn targeted_field_moves_selected_body_only() {
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(body([0.0, 8.0, 0.0]));
        bodies[1] = Some(body([10.0, 8.0, 0.0]));
        let fields = target_slots(0, FieldValue::uniform([0.0, -4.0, 0.0]).unwrap());
        let mut simulation = RigidSimulation::default();
        advance_targeted(&mut simulation, bodies.clone(), None, 0.0, [0.0; 3], &fields);
        assert_eq!(
            simulation.targeted_fields.span(0, 0, 0.0).unwrap().field(0).sample([0.0; 3]),
            [0.0, -4.0, 0.0],
            "the seeded target must align with the first authored sample",
        );
        advance_targeted(&mut simulation, bodies, None, 1.0, [0.0; 3], &fields);

        assert!(simulation.poses[0].pos[1] < 8.0);
        assert_eq!(simulation.poses[1].pos, [10.0, 8.0, 0.0]);
    }

    #[test]
    fn targeted_field_adds_to_global_acceleration() {
        let bodies = one_body([0.0, 8.0, 0.0]);
        let fields = target_slots(0, FieldValue::uniform([0.0, -2.0, 0.0]).unwrap());
        let mut simulation = RigidSimulation::default();
        simulation
            .advance_with_targeted_fields(
                bodies.clone(),
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0, -3.0, 0.0],
                Seconds::ZERO,
                1.0,
                0.0,
                Some(FieldValue::uniform([0.0, -1.0, 0.0]).unwrap()),
                &fields,
            )
            .unwrap();
        simulation
            .advance_with_targeted_fields(
                bodies,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0, -3.0, 0.0],
                Seconds(1.0),
                1.0,
                0.0,
                Some(FieldValue::uniform([0.0, -1.0, 0.0]).unwrap()),
                &fields,
            )
            .unwrap();
        let velocity = simulation
            .world
            .as_ref()
            .unwrap()
            .linear_velocity(simulation.handles[0].unwrap())
            .unwrap();
        assert!((velocity[1] + 6.0).abs() < 0.1);
    }

    #[test]
    fn targeted_copy_field_maps_to_all_active_copies() {
        let prototype = body([0.0, 8.0, 0.0]);
        let fields = target_slots(MAX_BODIES, FieldValue::uniform([0.0, -4.0, 0.0]).unwrap());
        let mut simulation = RigidSimulation::default();
        for time in [0.0, 1.0] {
            simulation.advance_with_targeted_fields(
                std::array::from_fn(|_| None), Some(prototype.clone()),
                4.0, 2.0, 2.0, 0.0, [0.0; 3], Seconds(time), 1.0, 0.0, None, &fields,
            ).unwrap();
        }
        assert_eq!(simulation.active_copy_count, 4);
        for pose in &simulation.copy_poses[..4] {
            assert!(pose.pos[1] < 7.0, "every copy must receive the field");
        }
    }

    #[test]
    fn targeted_field_ignores_static_body() {
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(RigidBody {
            kind: 0,
            transform: Transform {
                pos: [0.0, 8.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        let fields = target_slots(0, FieldValue::uniform([0.0, -4.0, 0.0]).unwrap());
        let mut simulation = RigidSimulation::default();
        advance_targeted(&mut simulation, bodies.clone(), None, 0.0, [0.0; 3], &fields);
        advance_targeted(&mut simulation, bodies, None, 1.0, [0.0; 3], &fields);
        assert_eq!(simulation.poses[0].pos, [0.0, 8.0, 0.0]);
    }

    #[test]
    fn all_none_target_input_stays_unallocated_and_connected_storage_is_reused() {
        let bodies = one_body([0.0, 8.0, 0.0]);
        let empty: [Option<FieldValue>; TARGET_SLOTS] = std::array::from_fn(|_| None);
        let mut simulation = RigidSimulation::default();
        advance_targeted(&mut simulation, bodies.clone(), None, 0.0, [0.0; 3], &empty);
        assert!(!simulation.targeted_fields.is_connected());
        assert_eq!(simulation.targeted_fields.capacity(), 0);

        let fields = target_slots(0, FieldValue::uniform([0.0, -4.0, 0.0]).unwrap());
        advance_targeted(&mut simulation, bodies.clone(), None, 0.0, [0.0; 3], &fields);
        let capacity = simulation.targeted_fields.capacity();
        let storage_ptr = simulation.targeted_fields.storage_ptr();
        advance_targeted(&mut simulation, bodies.clone(), None, 0.5, [0.0; 3], &fields);
        simulation
            .advance_with_targeted_fields(
                bodies,
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Seconds(0.5),
                1.0,
                1.0,
                None,
                &fields,
            )
            .unwrap();
        assert_eq!(simulation.targeted_fields.capacity(), capacity);
        assert_eq!(simulation.targeted_fields.storage_ptr(), storage_ptr);
    }

    fn run_targeted_trace(fps: usize) -> ([f32; 3], [f32; 3]) {
        let bodies = one_body([0.0, 8.0, 0.0]);
        let fields = target_slots(0, FieldValue::uniform([1.0, -4.0, 0.5]).unwrap());
        let mut simulation = RigidSimulation::default();
        advance_targeted(&mut simulation, bodies.clone(), None, 0.0, [0.0; 3], &fields);
        for frame in 1..=fps {
            advance_targeted(
                &mut simulation,
                bodies.clone(),
                None,
                frame as f64 / fps as f64,
                [0.0; 3],
                &fields,
            );
        }
        let velocity = simulation
            .world
            .as_ref()
            .unwrap()
            .linear_velocity(simulation.handles[0].unwrap())
            .unwrap();
        (simulation.poses[0].pos, velocity)
    }

    #[test]
    fn targeted_field_trace_is_render_partition_invariant() {
        let traces = [24, 30, 60].map(run_targeted_trace);
        for trace in traces.iter().skip(1) {
            for (actual, expected) in trace.0.iter().zip(traces[0].0) {
                assert!((actual - expected).abs() < 1.0e-5);
            }
            for (actual, expected) in trace.1.iter().zip(traces[0].1) {
                assert!((actual - expected).abs() < 1.0e-5);
            }
        }
    }

    #[test]
    fn targeted_fields_preserve_pending_intervals_on_first_connection_and_paused_edits() {
        let bodies = one_body([0.0, 8.0, 0.0]);
        let empty: [Option<FieldValue>; TARGET_SLOTS] = std::array::from_fn(|_| None);
        let old = target_slots(0, FieldValue::uniform([2.0, 0.0, 0.0]).unwrap());
        let new = target_slots(0, FieldValue::uniform([-4.0, 0.0, 0.0]).unwrap());
        for initial in [&empty, &old] {
            let mut expected = RigidSimulation::default();
            advance_targeted(&mut expected, bodies.clone(), None, 0.0, [0.0; 3], initial);
            advance_targeted(&mut expected, bodies.clone(), None, 0.5, [0.0; 3], initial);
            advance_targeted(&mut expected, bodies.clone(), None, 0.5, [0.0; 3], &new);
            let mut queued = RigidSimulation::default();
            advance_targeted(&mut queued, bodies.clone(), None, 0.0, [0.0; 3], initial);
            let identity = queued.handles[0];
            {
                let _budget = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
                advance_targeted(&mut queued, bodies.clone(), None, 0.5, [0.0; 3], initial);
                assert!(queued.pending_time.0 > 0.0);
                advance_targeted(&mut queued, bodies.clone(), None, 0.5, [0.0; 3], &new);
            }
            advance_targeted(&mut queued, bodies.clone(), None, 0.5, [0.0; 3], &new);
            assert_eq!(queued.pending_time, Seconds::ZERO);
            assert_eq!(queued.handles[0], identity);
            assert_eq!(queued.poses, expected.poses, "edit rewrote an unfinished interval");
            for simulation in [&mut queued, &mut expected] {
                advance_targeted(simulation, bodies.clone(), None, 0.75, [0.0; 3], &new);
            }
            assert_eq!(queued.poses, expected.poses, "edited endpoint was lost");
        }
    }

    fn varying_gravity(sample: usize) -> [f32; 3] {
        let phase = sample as f32 * 0.09;
        [
            phase.sin() * 2.0,
            -9.8 + phase.cos() * 3.0,
            phase.sin() * -0.75,
        ]
    }

    fn run_varying_gravity_trace(fps: usize) -> ([f32; 3], [f32; 3]) {
        assert_eq!(240 % fps, 0);
        let bodies = one_body([0.0, 4.0, 0.0]);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), varying_gravity(0), Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        let samples_per_frame = 240 / fps;
        for frame in 1..=fps {
            let end_sample = frame * samples_per_frame;
            {
                let _authored = PhysicsAuthoredSampleScope::new();
                for sample in ((frame - 1) * samples_per_frame + 1)..=end_sample {
                    simulation
                        .advance(
                            bodies.clone(),
                            varying_gravity(sample),
                            Seconds(sample as f64 / 240.0),
                            1.0,
                            0.0,
                        )
                        .unwrap();
                }
            }
            simulation
                .advance(
                    bodies.clone(),
                    varying_gravity(end_sample),
                    Seconds(end_sample as f64 / 240.0),
                    1.0,
                    0.0,
                )
                .unwrap();
        }
        let velocity = simulation
            .world
            .as_ref()
            .expect("trace builds a native world")
            .linear_velocity(simulation.handles[0].expect("trace body has a handle"))
            .unwrap();
        (simulation.poses[0].pos, velocity)
    }

    #[test]
    fn retained_gravity_trace_is_render_partition_invariant() {
        let traces = [24, 30, 60].map(run_varying_gravity_trace);
        for trace in traces.iter().skip(1) {
            for (actual, expected) in trace.0.iter().zip(traces[0].0) {
                assert!((actual - expected).abs() < 1.0e-5);
            }
            for (actual, expected) in trace.1.iter().zip(traces[0].1) {
                assert!((actual - expected).abs() < 1.0e-5);
            }
        }
    }

    #[test]
    fn changed_gravity_waits_behind_preview_debt() {
        let bodies = one_body([0.0, 4.0, 0.0]);
        let old_gravity = [0.0, -9.8, 0.0];
        let new_gravity = [0.0, 8.0, 0.0];
        let mut expected = RigidSimulation::default();
        expected
            .advance(bodies.clone(), old_gravity, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        expected
            .advance(bodies.clone(), old_gravity, Seconds(0.5), 1.0, 0.0)
            .unwrap();
        expected
            .advance(bodies.clone(), new_gravity, Seconds(0.5), 1.0, 0.0)
            .unwrap();

        let mut queued = RigidSimulation::default();
        queued
            .advance(bodies.clone(), old_gravity, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        let _scope = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
        queued
            .advance(bodies.clone(), old_gravity, Seconds(0.5), 1.0, 0.0)
            .unwrap();
        assert!(queued.pending_time.0 > 0.0);
        queued
            .advance(bodies.clone(), new_gravity, Seconds(0.5), 1.0, 0.0)
            .unwrap();
        while queued.pending_time.0 > 0.0 {
            queued
                .advance(bodies.clone(), new_gravity, Seconds(0.5), 1.0, 0.0)
                .unwrap();
        }

        assert_eq!(queued.poses, expected.poses);
        let queued_velocity = queued
            .world
            .as_ref()
            .expect("queued simulation builds a native world")
            .linear_velocity(queued.handles[0].expect("queued body has a handle"))
            .unwrap();
        let expected_velocity = expected
            .world
            .as_ref()
            .expect("expected simulation builds a native world")
            .linear_velocity(expected.handles[0].expect("expected body has a handle"))
            .unwrap();
        for (actual, expected) in queued_velocity.iter().zip(expected_velocity) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
    }

    #[test]
    fn exhausted_authored_history_preserves_state_until_reset() {
        let bodies = one_body([0.0, 4.0, 0.0]);
        let gravity = [0.0, -9.8, 0.0];
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), gravity, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        simulation.authored_samples = InputHistory::with_capacity(2).unwrap();
        simulation
            .record_authored_sample(0.0, bodies.clone(), None, gravity, None, None)
            .unwrap();
        simulation
            .record_authored_sample(FRAME, bodies.clone(), None, gravity, None, None)
            .unwrap();
        let accepted_len = simulation.authored_samples.len();
        let accepted_pose = simulation.poses;
        let accepted_time = simulation.authored_time;
        let accepted_debt = simulation.pending_time;

        let error = simulation
            .advance(bodies.clone(), gravity, Seconds(2.0 * FRAME), 1.0, 0.0)
            .unwrap_err();
        assert!(error.contains("exhausted") || error.contains("full"));
        assert!(simulation.authored_samples.is_exhausted());
        assert_eq!(simulation.authored_samples.len(), accepted_len);
        assert_eq!(simulation.poses, accepted_pose);
        assert_eq!(simulation.authored_time, accepted_time);
        assert_eq!(simulation.pending_time, accepted_debt);

        let retry = simulation
            .record_authored_sample(FRAME, bodies.clone(), None, gravity, None, None)
            .unwrap_err();
        assert!(retry.contains("exhausted"));
        assert_eq!(simulation.authored_samples.len(), accepted_len);

        simulation
            .advance(bodies, gravity, Seconds(2.0 * FRAME), 1.0, 1.0)
            .unwrap();
        assert!(!simulation.authored_samples.is_exhausted());
        assert_eq!(simulation.authored_samples.len(), 1);
    }

    #[test]
    fn zero_speed_does_not_advance_until_transport_resumes() {
        let bodies = one_body([0.0, 4.0, 0.0]);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        let authored = simulation.poses[0].pos;
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(0.5), 0.0, 0.0)
            .unwrap();
        assert_eq!(simulation.poses[0].pos, authored);
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        assert!(simulation.poses[0].pos[1] < authored[1] - 0.1);
    }

    #[test]
    fn disabled_body_keeps_authored_pose_and_restores_contacts_when_enabled() {
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(RigidBody {
            enabled: false,
            kind: 0,
            transform: Transform {
                pos: [0.0, 0.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        bodies[1] = Some(body([0.0, 1.5, 0.0]));
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(1.0), 1.0, 0.0)
            .unwrap();

        assert_eq!(simulation.poses[0].pos, [0.0, 0.0, 0.0]);
        assert!(simulation.poses[1].pos[1] < 0.0);

        bodies[0].as_mut().unwrap().enabled = true;
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(2.0), 1.0, 0.0)
            .unwrap();
        simulation
            .advance(bodies, GRAVITY, Seconds(3.0), 1.0, 0.0)
            .unwrap();
        assert!(simulation.poses[0].pos[1].abs() < 1.0e-5);
        assert!(simulation.poses[1].pos[1] > 0.5);
    }

    #[test]
    fn prepared_fragments_follow_parent_then_inherit_motion_on_release() {
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(RigidBody {
            transform: Transform {
                pos: [0.0, 4.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        bodies[1] = Some(RigidBody {
            transform: Transform {
                pos: [0.0, 4.0, 0.0],
                scale: [0.7, 0.7, 0.7],
                ..Transform::default()
            },
            fragment_parent: Some(0),
            ..RigidBody::default()
        });
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(0.5), 1.0, 0.0)
            .unwrap();
        assert!(!simulation.fragment_parent_released[0]);
        assert!(!simulation.fragment_active[1]);
        assert!((simulation.poses[1].pos[1] - simulation.poses[0].pos[1]).abs() < 1.0e-5);
        let parent_before_release = simulation.poses[0].pos;

        bodies[0].as_mut().unwrap().release_count = 1.0;
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        simulation
            .advance(bodies, GRAVITY, Seconds(1.5), 1.0, 0.0)
            .unwrap();
        assert!(simulation.fragment_parent_released[0]);
        assert!(simulation.fragment_active[1]);
        assert!((simulation.poses[1].pos[1] - parent_before_release[1]).abs() > 0.02);
        assert!(simulation.poses[1].pos[1] < parent_before_release[1]);
    }

    #[test]
    fn disabled_parent_does_not_release_prepared_fragments() {
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(RigidBody {
            enabled: false,
            release_count: 1.0,
            transform: Transform {
                pos: [0.0, 4.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        bodies[1] = Some(RigidBody {
            fragment_parent: Some(0),
            transform: Transform {
                pos: [0.0, 4.0, 0.0],
                scale: [0.7, 0.7, 0.7],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        simulation
            .advance(bodies, GRAVITY, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        assert!(!simulation.fragment_parent_released[0]);
        assert!(!simulation.fragment_active[1]);
        assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
        assert_eq!(simulation.poses[1].pos, [0.0, 4.0, 0.0]);
    }

    #[test]
    fn fragment_release_reset_restores_intact_state_and_allows_retrigger() {
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(RigidBody {
            transform: Transform {
                pos: [0.0, 4.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        bodies[1] = Some(RigidBody {
            fragment_parent: Some(0),
            transform: Transform {
                pos: [0.0, 4.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        bodies[0].as_mut().unwrap().release_count = 1.0;
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(0.5), 1.0, 0.0)
            .unwrap();
        assert!(simulation.fragment_parent_released[0]);

        bodies[0].as_mut().unwrap().release_count = 0.0;
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(0.5), 1.0, 1.0)
            .unwrap();
        assert!(!simulation.fragment_parent_released[0]);
        assert!(!simulation.fragment_active[1]);
        assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
        assert_eq!(simulation.poses[1].pos, [0.0, 4.0, 0.0]);

        bodies[0].as_mut().unwrap().release_count = 1.0;
        simulation
            .advance(bodies, GRAVITY, Seconds(0.5), 1.0, 1.0)
            .unwrap();
        assert!(simulation.fragment_parent_released[0]);
        assert!(simulation.fragment_active[1]);
    }

    #[test]
    fn fragment_release_inherits_spinning_parent_pose_and_point_velocity() {
        let child_geometry = Arc::new(ColliderGeometry {
            hulls: vec![vec![
                [0.35, -0.2, -0.2],
                [0.85, -0.2, -0.2],
                [0.35, 0.2, -0.2],
                [0.35, -0.2, 0.2],
            ]],
        });
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(RigidBody {
            transform: Transform {
                pos: [0.0, 4.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        bodies[1] = Some(RigidBody {
            fragment_parent: Some(0),
            collider: Some(child_geometry),
            transform: Transform {
                pos: [0.0, 4.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        bodies[2] = Some(body([5.0, 4.0, 0.0]));

        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        let parent_handle = simulation.handles[0].unwrap();
        let child_handle = simulation.handles[1].unwrap();
        let unrelated_handle = simulation.handles[2].unwrap();
        let parent_linear = [1.0, 0.5, 0.0];
        let parent_angular = [0.0, 2.0, 0.0];
        simulation
            .world
            .as_mut()
            .unwrap()
            .set_velocity(parent_handle, parent_linear, parent_angular)
            .unwrap();
        let (parent_pose, expected_angular, expected_child_velocity, unrelated_pose, unrelated_velocity) = {
            let world = simulation.world.as_ref().unwrap();
            let child_center = world.local_center_of_mass(child_handle).unwrap();
            (
                world.pose(parent_handle).unwrap(),
                world.angular_velocity(parent_handle).unwrap(),
                world
                    .velocity_at_local_point(parent_handle, child_center)
                    .unwrap(),
                world.pose(unrelated_handle).unwrap(),
                world.linear_velocity(unrelated_handle).unwrap(),
            )
        };
        assert!(expected_child_velocity[2].abs() > 0.5);

        bodies[0].as_mut().unwrap().release_count = 1.0;
        simulation
            .advance(bodies, [0.0; 3], Seconds::ZERO, 0.0, 0.0)
            .unwrap();

        let world = simulation.world.as_ref().unwrap();
        let child_pose = world.pose(child_handle).unwrap();
        let child_velocity = world.linear_velocity(child_handle).unwrap();
        let child_angular = world.angular_velocity(child_handle).unwrap();
        let after_unrelated_pose = world.pose(unrelated_handle).unwrap();
        let after_unrelated_velocity = world.linear_velocity(unrelated_handle).unwrap();
        for (actual, expected) in child_pose.position.iter().zip(parent_pose.position) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
        for (actual, expected) in child_pose.rotation.iter().zip(parent_pose.rotation) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
        for (actual, expected) in child_velocity.iter().zip(expected_child_velocity) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
        for (actual, expected) in child_angular.iter().zip(expected_angular) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
        for (actual, expected) in after_unrelated_pose.position.iter().zip(unrelated_pose.position) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
        for (actual, expected) in after_unrelated_velocity.iter().zip(unrelated_velocity) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
    }

    #[test]
    fn queued_preview_does_not_release_fragments_before_authored_event_tick() {
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(body([0.0, 4.0, 0.0]));
        bodies[1] = Some(RigidBody {
            fragment_parent: Some(0),
            transform: Transform {
                pos: [0.0, 4.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        let mut regular = RigidSimulation::default();
        regular
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        let mut released = bodies.clone();
        released[0].as_mut().unwrap().release_count = 1.0;
        regular
            .advance(released.clone(), GRAVITY, Seconds(0.5), 1.0, 0.0)
            .unwrap();
        assert!(regular.fragment_parent_released[0]);

        let mut queued = RigidSimulation::default();
        queued
            .advance(bodies, GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        {
            let _scope = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
            queued
                .advance(released.clone(), GRAVITY, Seconds(0.5), 1.0, 0.0)
                .unwrap();
            assert!(!queued.fragment_parent_released[0]);
            while queued.pending_time.0 > 0.0 {
                queued
                    .advance(released.clone(), GRAVITY, Seconds(0.5), 1.0, 0.0)
                    .unwrap();
            }
        }
        assert!(queued.fragment_parent_released[0]);
        assert_eq!(queued.poses, regular.poses);
    }

    #[test]
    fn animated_body_pushes_a_dynamic_body_instead_of_teleporting_through_it() {
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(RigidBody {
            kind: 2,
            transform: Transform {
                pos: [-1.3, 0.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        bodies[1] = Some(body([0.4, 0.0, 0.0]));
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();

        for frame in 1..=30 {
            bodies[0].as_mut().unwrap().transform.pos[0] = -1.3 + frame as f32 * 0.05;
            simulation
                .advance(
                    bodies.clone(),
                    [0.0; 3],
                    Seconds(frame as f64 * FRAME),
                    1.0,
                    0.0,
                )
                .unwrap();
        }

        assert!(
            simulation.poses[1].pos[0] > 0.6,
            "moving collider failed to push body: {:?}",
            simulation.poses[1].pos
        );
    }

    #[test]
    fn rotating_animated_body_contacts_a_dynamic_body() {
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(RigidBody {
            kind: 2,
            transform: Transform {
                scale: [3.0, 0.3, 0.3],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        let start = [1.0, 0.0, -1.0];
        bodies[1] = Some(body(start));
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();

        for frame in 1..=60 {
            bodies[0].as_mut().unwrap().transform.rot_euler[1] =
                frame as f32 * std::f32::consts::FRAC_PI_2 / 60.0;
            simulation
                .advance(
                    bodies.clone(),
                    [0.0; 3],
                    Seconds(frame as f64 * FRAME),
                    1.0,
                    0.0,
                )
                .unwrap();
        }

        let result = simulation.poses[1].pos;
        assert!(
            (result[0] - start[0]).abs() + (result[2] - start[2]).abs() > 0.1,
            "rotating collider missed body: {result:?}"
        );
    }

    #[test]
    fn reset_or_backward_time_restores_authored_pose() {
        let bodies = one_body([0.0, 4.0, 0.0]);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(0.75), 1.0, 0.0)
            .unwrap();
        assert!(simulation.poses[0].pos[1] < 4.0);

        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(0.75), 1.0, 1.0)
            .unwrap();
        assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(0.5), 1.0, 1.0)
            .unwrap();
        assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
    }

    #[test]
    fn material_edit_preserves_the_current_falling_pose() {
        let bodies = one_body([0.0, 4.0, 0.0]);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(0.5), 1.0, 0.0)
            .unwrap();
        let falling_pose = simulation.poses[0].pos;

        let mut edited = bodies;
        let mut edited_body = edited[0].clone().unwrap();
        edited_body.friction = 0.9;
        edited_body.mass = 2.0;
        edited_body.bounce = 0.4;
        edited[0] = Some(edited_body);
        simulation
            .advance(edited.clone(), GRAVITY, Seconds(0.5), 1.0, 0.0)
            .unwrap();
        assert!((simulation.poses[0].pos[1] - falling_pose[1]).abs() < 1.0e-5);

        simulation
            .advance(edited.clone(), GRAVITY, Seconds(0.75), 1.0, 0.0)
            .unwrap();
        assert!(simulation.poses[0].pos[1] < falling_pose[1] - 0.05);
    }

    #[test]
    fn multi_hull_geometry_rebuilds_on_reset_and_material_edits_keep_pose() {
        let geometry = Arc::new(ColliderGeometry {
            hulls: vec![
                vec![
                    [-0.5, -0.5, -0.5],
                    [0.5, -0.5, -0.5],
                    [0.0, 0.5, -0.5],
                    [0.0, 0.0, 0.5],
                ],
                vec![
                    [-0.5, -0.5, 0.5],
                    [0.5, -0.5, 0.5],
                    [0.0, 0.5, 0.5],
                    [0.0, 0.0, -0.5],
                ],
            ],
        });
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(RigidBody {
            collider: Some(geometry.clone()),
            transform: Transform {
                pos: [0.0, 4.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(0.5), 1.0, 0.0)
            .unwrap();
        let falling_pose = simulation.poses[0].pos;

        let mut edited = bodies.clone();
        let mut edited_body = edited[0].take().expect("body exists");
        edited_body.friction = 0.9;
        edited_body.mass = 2.0;
        edited_body.collider = Some(geometry.clone());
        edited[0] = Some(edited_body);
        simulation
            .advance(edited.clone(), GRAVITY, Seconds(0.5), 1.0, 0.0)
            .unwrap();
        assert!((simulation.poses[0].pos[1] - falling_pose[1]).abs() < 1.0e-5);

        simulation
            .advance(edited, GRAVITY, Seconds(0.5), 1.0, 1.0)
            .unwrap();
        assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
    }

    #[test]
    fn all_scaled_platonic_hulls_settle_on_the_ground() {
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(RigidBody {
            transform: Transform {
                pos: [0.0, -1.0, 0.0],
                scale: [20.0, 1.0, 20.0],
                ..Transform::default()
            },
            kind: 0,
            mass: 0.0,
            ..RigidBody::default()
        });
        for (index, x) in [-8.0, -4.0, 0.0, 4.0, 8.0].into_iter().enumerate() {
            bodies[index + 1] = Some(RigidBody {
                transform: Transform {
                    pos: [x, 4.0, 0.0],
                    scale: [0.7 + index as f32 * 0.15, 0.8 + index as f32 * 0.1, 0.7],
                    ..Transform::default()
                },
                shape: index as u32,
                ..RigidBody::default()
            });
        }

        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        // A dodecahedron can still roll between faces at four seconds.
        // Give every shape time to sleep before measuring stable contact.
        for frame in 1..=720 {
            simulation
                .advance(
                    bodies.clone(),
                    GRAVITY,
                    Seconds(frame as f64 * FRAME),
                    1.0,
                    0.0,
                )
                .unwrap();
        }
        let settled = simulation.poses;
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds(12.5), 1.0, 0.0)
            .unwrap();
        for (index, before) in settled.iter().enumerate().take(6).skip(1) {
            assert!(
                (simulation.poses[index].pos[1] - before.pos[1]).abs() < 0.03,
                "shape={} before={:?} after={:?}",
                index - 1,
                before,
                simulation.poses[index]
            );
            assert!(simulation.poses[index].pos[1] > -1.0);
        }
    }

    #[test]
    fn long_catch_up_matches_regular_ticks_and_keeps_running() {
        let bodies = one_body([0.0, 4.0, 0.0]);
        let mut caught_up = RigidSimulation::default();
        let mut regular = RigidSimulation::default();
        caught_up
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        for frame in 0..=181 {
            regular
                .advance(
                    bodies.clone(),
                    GRAVITY,
                    Seconds(frame as f64 * FRAME),
                    1.0,
                    0.0,
                )
                .unwrap();
        }
        caught_up
            .advance(bodies.clone(), GRAVITY, Seconds(180.0 * FRAME), 1.0, 0.0)
            .unwrap();
        caught_up
            .advance(bodies.clone(), GRAVITY, Seconds(181.0 * FRAME), 1.0, 0.0)
            .unwrap();
        assert_eq!(caught_up.poses, regular.poses);
        caught_up
            .advance(bodies.clone(), GRAVITY, Seconds(181.0 * FRAME), 1.0, 1.0)
            .unwrap();
        assert_eq!(caught_up.poses[0].pos, [0.0, 4.0, 0.0]);
    }

    #[test]
    fn preview_backlog_is_retained_and_eventually_matches_export() {
        // Force one tick per call without relying on machine speed.
        let _scope = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
        let mut bodies = one_body([0.0, 4.0, 0.0]);
        let mut floor = body([0.0, -1.0, 0.0]);
        floor.kind = 0;
        floor.transform.scale = [20.0, 1.0, 20.0];
        bodies[1] = Some(floor);
        let mut preview = RigidSimulation::default();
        let mut export = RigidSimulation::default();
        for sim in [&mut preview, &mut export] {
            sim.advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
                .unwrap();
        }
        let now = Seconds(3.0 + FRAME / 2.0);
        preview
            .advance(bodies.clone(), GRAVITY, now, 1.0, 0.0)
            .unwrap();
        assert!((preview.pending_time.0 - 179.0 * FRAME).abs() < 1e-9);
        let held = preview.poses;
        preview
            .advance(bodies.clone(), GRAVITY, now, 0.0, 0.0)
            .unwrap();
        assert_eq!(preview.poses, held);
        for _ in 1..180 {
            preview
                .advance(bodies.clone(), GRAVITY, now, 1.0, 0.0)
                .unwrap();
        }
        assert_eq!(preview.pending_time, Seconds::ZERO);
        assert!((preview.accumulator - FRAME / 2.0).abs() < 1e-9);
        {
            let _export = PhysicsStepScope::for_render(true);
            export
                .advance(bodies.clone(), GRAVITY, now, 1.0, 0.0)
                .unwrap();
        }
        assert_eq!(
            preview.poses, export.poses,
            "chunking must not change collision results"
        );
        let next = Seconds(3.0 + FRAME);
        preview
            .advance(bodies.clone(), GRAVITY, next, 1.0, 0.0)
            .unwrap();
        export
            .advance(bodies.clone(), GRAVITY, next, 1.0, 0.0)
            .unwrap();
        assert_eq!(preview.poses, export.poses);
    }

    #[test]
    fn animated_pose_timeline_matches_export_for_moving_and_rotating_contacts() {
        let _preview_scope =
            PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);

        let mut moving = std::array::from_fn(|_| None);
        moving[0] = Some(RigidBody {
            kind: 2,
            transform: Transform {
                pos: [-1.3, 0.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        moving[1] = Some(body([0.4, 0.0, 0.0]));
        let mut moving_preview = RigidSimulation::default();
        let mut moving_export = RigidSimulation::default();
        moving_preview
            .advance(moving.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        moving_export
            .advance(moving.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        moving[0].as_mut().unwrap().transform.pos[0] = -0.55;
        moving_preview
            .advance(moving.clone(), [0.0; 3], Seconds(0.5), 1.0, 0.0)
            .unwrap();
        {
            let _export_scope = PhysicsStepScope::for_render(true);
            moving_export
                .advance(moving.clone(), [0.0; 3], Seconds(0.5), 1.0, 0.0)
                .unwrap();
        }
        moving[0].as_mut().unwrap().transform.pos[0] = 0.2;
        moving_preview
            .advance(moving.clone(), [0.0; 3], Seconds(1.0), 1.0, 0.0)
            .unwrap();
        {
            let _export_scope = PhysicsStepScope::for_render(true);
            moving_export
                .advance(moving.clone(), [0.0; 3], Seconds(1.0), 1.0, 0.0)
                .unwrap();
        }
        while moving_preview.pending_time.0 > 0.0 {
            moving_preview
                .advance(moving.clone(), [0.0; 3], Seconds(1.0), 1.0, 0.0)
                .unwrap();
        }
        assert_eq!(moving_preview.poses, moving_export.poses);

        let mut rotating = std::array::from_fn(|_| None);
        rotating[0] = Some(RigidBody {
            kind: 2,
            transform: Transform {
                scale: [3.0, 0.3, 0.3],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        rotating[1] = Some(body([1.0, 0.0, -1.0]));
        let mut rotating_preview = RigidSimulation::default();
        let mut rotating_export = RigidSimulation::default();
        rotating_preview
            .advance(rotating.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        rotating_export
            .advance(rotating.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        rotating[0].as_mut().unwrap().transform.rot_euler[1] = std::f32::consts::FRAC_PI_4;
        rotating_preview
            .advance(rotating.clone(), [0.0; 3], Seconds(0.5), 1.0, 0.0)
            .unwrap();
        {
            let _export_scope = PhysicsStepScope::for_render(true);
            rotating_export
                .advance(rotating.clone(), [0.0; 3], Seconds(0.5), 1.0, 0.0)
                .unwrap();
        }
        rotating[0].as_mut().unwrap().transform.rot_euler[1] = std::f32::consts::FRAC_PI_2;
        rotating_preview
            .advance(rotating.clone(), [0.0; 3], Seconds(1.0), 1.0, 0.0)
            .unwrap();
        {
            let _export_scope = PhysicsStepScope::for_render(true);
            rotating_export
                .advance(rotating.clone(), [0.0; 3], Seconds(1.0), 1.0, 0.0)
                .unwrap();
        }
        while rotating_preview.pending_time.0 > 0.0 {
            rotating_preview
                .advance(rotating.clone(), [0.0; 3], Seconds(1.0), 1.0, 0.0)
                .unwrap();
        }
        assert_eq!(rotating_preview.poses, rotating_export.poses);
    }

    #[test]
    fn paused_authored_edit_keeps_pose_sample_needed_by_preview_backlog() {
        let _scope = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
        let mut bodies = std::array::from_fn(|_| None);
        let mut animated = body([-2.0, 0.0, 0.0]);
        animated.kind = 2;
        bodies[0] = Some(animated);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        bodies[0].as_mut().unwrap().transform.pos[0] = -0.5;
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds(0.5), 1.0, 0.0)
            .unwrap();
        bodies[0].as_mut().unwrap().transform.pos[0] = 2.0;
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds(0.5), 1.0, 0.0)
            .unwrap();
        let owed_pose = simulation
            .interpolated_body(0, 0.25, bodies[0].clone().unwrap())
            .unwrap();
        assert!((owed_pose.transform.pos[0] + 1.25).abs() < 1.0e-4);
        let closing_pose = simulation
            .interpolated_body(0, 0.5, bodies[0].clone().unwrap())
            .unwrap();
        assert!((closing_pose.transform.pos[0] + 0.5).abs() < 1.0e-4);
    }

    #[test]
    fn paused_animated_pose_edit_and_undo_move_only_the_authored_body() {
        let mut bodies = std::array::from_fn(|_| None);
        let mut animated = body([0.0, 0.0, 0.0]);
        animated.kind = 2;
        bodies[0] = Some(animated.clone());
        bodies[1] = Some(body([10.0, 0.0, 0.0]));
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        let dynamic_before = simulation.poses[1];

        bodies[0].as_mut().unwrap().transform.pos[0] = 5.0;
        bodies[0].as_mut().unwrap().transform.rot_euler[2] = 0.4;
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        assert!((simulation.poses[0].pos[0] - 5.0).abs() < 1.0e-5);
        assert!((simulation.poses[0].rot_euler[2] - 0.4).abs() < 1.0e-5);
        assert_eq!(simulation.poses[1], dynamic_before);
        assert_eq!(simulation.pending_time, Seconds::ZERO);

        bodies[0] = Some(animated);
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        assert!(simulation.poses[0].pos[0].abs() < 1.0e-5);
        assert!(simulation.poses[0].rot_euler[2].abs() < 1.0e-5);
        assert_eq!(simulation.poses[1], dynamic_before);
    }

    #[test]
    fn paused_animated_copy_edit_and_undo_update_every_copy_without_ticks() {
        let mut prototype = body([0.0, 0.0, 0.0]);
        prototype.kind = 2;
        let mut simulation = RigidSimulation::default();
        let mut advance = |prototype: RigidBody| {
            simulation
                .advance_with_copies(
                    std::array::from_fn(|_| None),
                    Some(prototype.clone()),
                    2.0,
                    1.25,
                    2.0,
                    [0.0; 3],
                    Seconds::ZERO,
                    1.0,
                    0.0,
                )
                .unwrap();
            simulation.copy_poses[..2].to_vec()
        };
        let before = advance(prototype.clone());
        prototype.transform.pos[0] = 5.0;
        prototype.transform.rot_euler[1] = 0.25;
        let edited = advance(prototype.clone());
        for (old, new) in before.iter().zip(&edited) {
            assert!((new.pos[0] - old.pos[0] - 5.0).abs() < 1.0e-5);
            assert!((new.rot_euler[1] - old.rot_euler[1] - 0.25).abs() < 1.0e-5);
        }
        prototype.transform.pos[0] = 0.0;
        prototype.transform.rot_euler[1] = 0.0;
        let undone = advance(prototype.clone());
        assert_eq!(undone, before);
    }

    #[test]
    fn paused_edit_during_backlog_stays_visible_without_sweeping_a_dynamic_body() {
        let _scope = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
        let mut bodies = std::array::from_fn(|_| None);
        let mut animated = body([-2.0, 0.0, 0.0]);
        animated.kind = 2;
        bodies[0] = Some(animated);
        bodies[1] = Some(body([2.0, 0.0, 0.0]));
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        bodies[0].as_mut().unwrap().transform.pos[0] = -1.0;
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds(0.5), 1.0, 0.0)
            .unwrap();
        assert!(simulation.pending_time.0 > 0.0);
        bodies[0].as_mut().unwrap().transform.pos[0] = 5.0;
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds(0.5), 0.0, 0.0)
            .unwrap();
        assert!((simulation.poses[0].pos[0] - 5.0).abs() < 1.0e-5);
        assert!(simulation.pending_time.0 > 0.0);
        while simulation.pending_time.0 > 0.0 {
            simulation
                .advance(bodies.clone(), [0.0; 3], Seconds(0.5), 1.0, 0.0)
                .unwrap();
        }
        assert!((simulation.poses[0].pos[0] - 5.0).abs() < 1.0e-5);
        assert!((simulation.poses[1].pos[0] - 2.0).abs() < 1.0e-3);
    }

    #[test]
    fn fast_animated_sweep_uses_outer_steps_to_reach_dynamic_body() {
        let mut bodies = std::array::from_fn(|_| None);
        let mut animated = body([-2.0, 0.0, 0.0]);
        animated.kind = 2;
        bodies[0] = Some(animated);
        bodies[1] = Some(body([0.0, 0.0, 0.0]));
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        bodies[0].as_mut().unwrap().transform.pos[0] = 2.0;
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds(FRAME), 1.0, 0.0)
            .unwrap();
        assert!(
            simulation.poses[1]
                .pos
                .iter()
                .any(|value| value.abs() > 0.01),
            "fast animated body passed through the dynamic body"
        );
    }

    #[test]
    fn nonlinear_animated_round_trip_is_sampled_inside_one_render_frame() {
        let mut bodies = std::array::from_fn(|_| None);
        let mut animated = body([-2.0, 0.0, 0.0]);
        animated.kind = 2;
        bodies[0] = Some(animated);
        bodies[1] = Some(body([0.0, 0.0, 0.0]));
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();

        // The authored curve returns to its starting position by the next
        // rendered frame. Endpoint interpolation alone sees zero movement.
        for quarter in 1..4 {
            let t = FRAME * f64::from(quarter) / 4.0;
            bodies[0].as_mut().unwrap().transform.pos[0] =
                -2.0 * (std::f32::consts::TAU * quarter as f32 / 4.0).cos();
            let _sample = PhysicsAuthoredSampleScope::new();
            simulation
                .advance(bodies.clone(), [0.0; 3], Seconds(t), 1.0, 0.0)
                .unwrap();
        }
        bodies[0].as_mut().unwrap().transform.pos[0] = -2.0;
        assert!(simulation.animated_microsteps(&bodies, None, FRAME).0 > 1);
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds(FRAME), 1.0, 0.0)
            .unwrap();
        assert!(
            simulation.poses[1].pos.iter().any(|value| value.abs() > 0.01),
            "nonlinear Animated collider missed the Dynamic body: {:?}",
            simulation.poses[1].pos
        );
    }

    #[test]
    fn nonlinear_contacts_match_regular_irregular_and_preview_export_delivery() {
        fn trajectory(tick: usize) -> [Option<RigidBody>; MAX_BODIES] {
            let mut bodies = std::array::from_fn(|_| None);
            let mut animated = body([
                -2.0 * (std::f32::consts::TAU * tick as f32 / 8.0).cos(),
                0.0,
                0.0,
            ]);
            animated.kind = 2;
            animated.transform.rot_euler[2] = 0.6 * (tick as f32 / 8.0).sin();
            bodies[0] = Some(animated);
            bodies[1] = Some(body([0.0, 0.0, 0.0]));
            bodies
        }

        fn run(irregular: bool, preview: bool) -> [f32; 3] {
            let mut simulation = RigidSimulation::default();
            simulation
                .advance(trajectory(0), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
                .unwrap();
            let _preview = preview
                .then(|| PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO));
            for sample in 1..=32 {
                let time = Seconds(sample as f64 / 240.0);
                if irregular && sample != 32 {
                    let _authored = PhysicsAuthoredSampleScope::new();
                    simulation
                        .advance(trajectory(sample), [0.0; 3], time, 1.0, 0.0)
                        .unwrap();
                } else {
                    simulation
                        .advance(trajectory(sample), [0.0; 3], time, 1.0, 0.0)
                        .unwrap();
                }
            }
            if preview {
                assert!(simulation.pending_time.0 > 0.0);
                let _export = PhysicsStepScope::for_render(true);
                simulation
                    .advance(trajectory(32), [0.0; 3], Seconds(32.0 / 240.0), 0.0, 0.0)
                    .unwrap();
                assert_eq!(simulation.pending_time, Seconds::ZERO);
            }
            simulation.poses[1].pos
        }

        let regular = run(false, false);
        let irregular = run(true, false);
        let catch_up = run(true, true);
        assert!(regular.iter().any(|value| value.abs() > 0.01));
        for (reference, actual) in regular.into_iter().zip(irregular) {
            assert!((reference - actual).abs() < 1.0e-3, "regular {reference}, irregular {actual}");
        }
        for (reference, actual) in regular.into_iter().zip(catch_up) {
            assert!((reference - actual).abs() < 1.0e-3, "regular {reference}, catch-up {actual}");
        }
    }

    #[test]
    fn fast_dynamic_body_uses_bullet_collision_against_animated_body() {
        let mut bodies = std::array::from_fn(|_| None);
        bodies[0] = Some(body([0.0, 3.0, 0.0]));
        let mut animated = body([0.0, 0.0, 0.0]);
        animated.kind = 2;
        bodies[1] = Some(animated);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(
                bodies.clone(),
                [0.0, -20_000.0, 0.0],
                Seconds::ZERO,
                1.0,
                0.0,
            )
            .unwrap();
        simulation
            .advance(
                bodies.clone(),
                [0.0, -20_000.0, 0.0],
                Seconds(FRAME),
                1.0,
                0.0,
            )
            .unwrap();
        assert!(
            simulation.poses[0].pos[1] > 0.9,
            "fast dynamic body passed through the animated body: {:?}",
            simulation.poses[0].pos
        );
    }

    #[test]
    fn two_fast_dynamics_collide_without_bullet_target_skip() {
        let mut bodies = std::array::from_fn(|_| None);
        let mut floor = body([0.0, -2.0, 0.0]);
        floor.kind = 0;
        bodies[0] = Some(floor);
        bodies[1] = Some(body([0.0, -0.5, 0.0]));
        bodies[2] = Some(body([0.0, 3.0, 0.0]));
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(
                bodies.clone(),
                [0.0, -20_000.0, 0.0],
                Seconds::ZERO,
                1.0,
                0.0,
            )
            .unwrap();
        simulation
            .advance(
                bodies.clone(),
                [0.0, -20_000.0, 0.0],
                Seconds(FRAME),
                1.0,
                0.0,
            )
            .unwrap();
        assert!(
            simulation.poses[2].pos[1] > 0.0,
            "fast Dynamic passed through another Dynamic: {:?}",
            simulation.poses[2].pos
        );
    }

    #[test]
    fn extreme_animated_translation_uses_automatic_outer_steps() {
        let mut bodies = std::array::from_fn(|_| None);
        let mut animated = body([-100.0, 0.0, 0.0]);
        animated.kind = 2;
        bodies[0] = Some(animated);
        bodies[1] = Some(body([0.0, 0.0, 0.0]));
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        bodies[0].as_mut().unwrap().transform.pos[0] = 100.0;
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds(FRAME), 1.0, 0.0)
            .unwrap();
        assert!(
            simulation.poses[1].pos.iter().any(|value| value.abs() > 0.01),
            "extreme Animated translation missed the Dynamic body: animated={:?}, dynamic={:?}",
            simulation.poses[0].pos,
            simulation.poses[1].pos
        );
    }

    #[test]
    fn preview_backlog_can_be_completed_by_export_or_cleared_by_reset() {
        let _scope = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
        let bodies = one_body([0.0, 4.0, 0.0]);
        let mut sim = RigidSimulation::default();
        sim.advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        sim.advance(bodies.clone(), GRAVITY, Seconds(3.0), 1.0, 0.0)
            .unwrap();
        assert!(sim.pending_time.0 > 2.9);
        {
            let _export = PhysicsStepScope::for_render(true);
            sim.advance(bodies.clone(), GRAVITY, Seconds(3.0), 0.0, 0.0)
                .unwrap();
        }
        assert_eq!(sim.pending_time, Seconds::ZERO);
        sim.advance(bodies.clone(), GRAVITY, Seconds(6.0), 1.0, 0.0)
            .unwrap();
        assert!(sim.pending_time.0 > 2.9);
        sim.advance(bodies.clone(), GRAVITY, Seconds(6.0), 1.0, 1.0)
            .unwrap();
        assert_eq!(sim.pending_time, Seconds::ZERO);
        assert_eq!(sim.poses[0].pos, [0.0, 4.0, 0.0]);
    }

    #[test]
    fn render_scope_restores_live_and_export_policy() {
        assert_eq!(PREVIEW_STEP_BUDGET.with(std::cell::Cell::get), None);
        {
            let budget = std::time::Duration::from_millis(33);
            let _live = PhysicsStepScope::with_preview_budget(false, budget);
            assert_eq!(PREVIEW_STEP_BUDGET.with(std::cell::Cell::get), Some(budget));
            {
                let _export = PhysicsStepScope::for_render(true);
                long_catch_up_matches_regular_ticks_and_keeps_running();
                assert_eq!(PREVIEW_STEP_BUDGET.with(std::cell::Cell::get), None);
            }
            assert_eq!(PREVIEW_STEP_BUDGET.with(std::cell::Cell::get), Some(budget));
        }
        assert_eq!(PREVIEW_STEP_BUDGET.with(std::cell::Cell::get), None);
    }

    #[test]
    fn offline_history_drain_scope_guards_live_preview_and_restores_state() {
        assert!(!history_drain_requested());
        {
            let _drain = PhysicsHistoryDrainScope::new();
            assert!(history_drain_requested());
            {
                let _live =
                    PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
                assert!(!history_drain_requested());
            }
            assert!(history_drain_requested());
        }
        assert!(!history_drain_requested());
    }

    #[test]
    fn offline_history_drain_keeps_authored_history_bounded() {
        let bodies = one_body([0.0, 4.0, 0.0]);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies.clone(), GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();

        for sample in 1..=1024 {
            let _authored = PhysicsAuthoredSampleScope::new();
            let time = Seconds(sample as f64 / 240.0);
            if sample % 64 == 0 {
                let _drain = PhysicsHistoryDrainScope::new();
                simulation
                    .advance(bodies.clone(), GRAVITY, time, 1.0, 0.0)
                    .unwrap();
            } else {
                simulation
                    .advance(bodies.clone(), GRAVITY, time, 1.0, 0.0)
                    .unwrap();
            }
            assert!(simulation.authored_samples.len() <= AUTHORED_HISTORY_CAPACITY);
        }

        assert!(simulation.physics_time > 0.0);
        assert!(simulation.poses[0].pos[1] < 4.0);
    }

    #[test]
    fn copies_are_reset_latched_and_shrink_tail_is_inactive() {
        let bodies = std::array::from_fn(|_| None);
        let prototype = body([0.0, 4.0, 0.0]);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance_with_copies(
                bodies.clone(),
                Some(prototype.clone()),
                130.0,
                1.25,
                16.0,
                GRAVITY,
                Seconds::ZERO,
                1.0,
                0.0,
            )
            .unwrap();
        assert_eq!(simulation.active_copy_count, 130);
        assert_ne!(simulation.copy_poses[129], Transform::default());

        simulation
            .advance_with_copies(
                bodies.clone(),
                Some(prototype.clone()),
                3.0,
                2.0,
                1.0,
                GRAVITY,
                Seconds(FRAME),
                1.0,
                0.0,
            )
            .unwrap();
        assert_eq!(simulation.active_copy_count, 130);

        simulation
            .advance_with_copies(
                bodies.clone(),
                Some(prototype.clone()),
                3.0,
                2.0,
                1.0,
                GRAVITY,
                Seconds(FRAME),
                1.0,
                1.0,
            )
            .unwrap();
        assert_eq!(simulation.active_copy_count, 3);
        assert_eq!(simulation.copy_poses[3], Transform::default());
    }

    #[test]
    fn pile_layout_is_reset_deterministic_and_property_latched() {
        let prototype = body([0.0, 9.0, 0.0]);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance_with_copy_layout(
                std::array::from_fn(|_| None),
                Some(prototype.clone()),
                32.0,
                1.85,
                16.0,
                1.0,
                GRAVITY,
                Seconds::ZERO,
                0.0,
                0.0,
            )
            .unwrap();
        let initial = simulation.copy_poses[..32].to_vec();

        simulation
            .advance_with_copy_layout(
                std::array::from_fn(|_| None),
                Some(prototype.clone()),
                3.0,
                0.5,
                1.0,
                0.0,
                GRAVITY,
                Seconds(FRAME),
                0.0,
                0.0,
            )
            .unwrap();
        assert_eq!(simulation.active_copy_count, 32);
        assert_eq!(&simulation.copy_poses[..32], initial.as_slice());

        simulation
            .advance_with_copy_layout(
                std::array::from_fn(|_| None),
                Some(prototype.clone()),
                3.0,
                0.5,
                1.0,
                0.0,
                GRAVITY,
                Seconds(FRAME),
                0.0,
                1.0,
            )
            .unwrap();
        assert_eq!(simulation.active_copy_count, 3);

        simulation
            .advance_with_copy_layout(
                std::array::from_fn(|_| None),
                Some(prototype.clone()),
                32.0,
                1.85,
                16.0,
                1.0,
                GRAVITY,
                Seconds(FRAME),
                0.0,
                2.0,
            )
            .unwrap();
        assert_eq!(&simulation.copy_poses[..32], initial.as_slice());
    }

    #[test]
    fn pile_layout_has_bounded_jitter_distinct_rotations_and_clearance() {
        let prototype = body([0.0, 9.0, 0.0]);
        let first = copy_transform_for_layout(
            prototype.clone(),
            prototype.transform.pos,
            0,
            256,
            16,
            1.85,
            CopyLayout::Pile,
        );
        let second = copy_transform_for_layout(
            prototype.clone(),
            prototype.transform.pos,
            1,
            256,
            16,
            1.85,
            CopyLayout::Pile,
        );
        assert_ne!(first.transform.rot_euler, second.transform.rot_euler);
        assert_eq!(
            first.transform,
            copy_transform_for_layout(
                prototype.clone(),
                prototype.transform.pos,
                0,
                256,
                16,
                1.85,
                CopyLayout::Pile,
            )
            .transform
        );

        for count in [256, MAX_COPIES] {
            let columns = ceil_cuberoot(count).min(16);
            let mut positions = vec![[0.0; 3]; count];
            for (index, position) in positions.iter_mut().enumerate() {
                *position = pile_position(prototype.transform.pos, index, count, 16, 1.85);
                let column = index % columns;
                let row = (index / columns) % columns;
                let layer = index / (columns * columns);
                let center = (columns as f32 - 1.0) * 0.5;
                let lattice = [
                    prototype.transform.pos[0] + (column as f32 - center) * 1.85,
                    prototype.transform.pos[1] + layer as f32 * 1.85,
                    prototype.transform.pos[2] + (row as f32 - center) * 1.85,
                ];
                for axis in 0..3 {
                    assert!((position[axis] - lattice[axis]).abs() <= 0.04 * 1.85 + 1.0e-6);
                }
            }

            let mut closest_squared = f32::MAX;
            for (index, left) in positions.iter().enumerate() {
                for right in positions.iter().skip(index + 1) {
                    let distance_squared = (0..3)
                        .map(|axis| (left[axis] - right[axis]).powi(2))
                        .sum::<f32>();
                    closest_squared = closest_squared.min(distance_squared);
                }
            }
            let sphere_diameter: f32 = 1.6;
            assert!(
                closest_squared > sphere_diameter * sphere_diameter,
                "count={count} closest_squared={closest_squared}"
            );
        }
    }

    #[test]
    fn bulk_boxes_share_floor_contacts_and_reset_after_catch_up() {
        let mut bodies = std::array::from_fn(|_| None);
        let mut ground = body([0.0, -0.28867513, 0.0]);
        ground.kind = 0;
        ground.transform.scale = [20.0, 0.5, 20.0];
        bodies[0] = Some(ground);
        let mut prototype = body([0.0, 3.0, 0.0]);
        prototype.transform.scale = [0.5; 3];
        let mut sim = RigidSimulation::default();
        for frame in 0..=240 {
            sim.advance_with_copies(
                bodies.clone(),
                Some(prototype.clone()),
                32.0,
                1.25,
                4.0,
                GRAVITY,
                Seconds(frame as f64 * FRAME),
                1.0,
                0.0,
            )
            .unwrap();
        }
        assert_eq!(sim.active_copy_count, 32);
        for pose in &sim.copy_poses[..32] {
            assert!(
                (0.2..1.3).contains(&pose.pos[1]),
                "box must settle on floor/another box: {pose:?}"
            );
        }
        let held = sim.copy_poses.clone();
        sim.advance_with_copies(
            bodies.clone(),
            Some(prototype.clone()),
            64.0,
            1.25,
            4.0,
            GRAVITY,
            Seconds(5.0),
            0.0,
            0.0,
        )
        .unwrap();
        assert_eq!(
            sim.copy_poses, held,
            "zero speed holds and count edit stays pending"
        );
        sim.advance_with_copies(
            bodies.clone(),
            Some(prototype.clone()),
            64.0,
            1.25,
            4.0,
            GRAVITY,
            Seconds(8.0),
            1.0,
            0.0,
        )
        .unwrap();
        sim.advance_with_copies(
            bodies.clone(),
            Some(prototype.clone()),
            64.0,
            1.25,
            4.0,
            GRAVITY,
            Seconds(8.0),
            1.0,
            1.0,
        )
        .unwrap();
        assert_eq!(sim.active_copy_count, 64);
        assert_eq!(sim.copy_poses[0].pos[1], 3.0);
    }

    #[test]
    fn bulk_fixed_ticks_match_across_frame_partitions() {
        let mut full = RigidSimulation::default();
        let mut half = RigidSimulation::default();
        for (simulation, frames, dt) in [(&mut full, 60, FRAME), (&mut half, 120, FRAME / 2.0)] {
            for frame in 0..=frames {
                simulation
                    .advance_with_copies(
                        std::array::from_fn(|_| None),
                        Some(body([0.0, 8.0, 0.0])),
                        20.0,
                        2.0,
                        4.0,
                        GRAVITY,
                        Seconds(frame as f64 * dt),
                        1.0,
                        0.0,
                    )
                    .unwrap();
            }
        }
        assert_eq!(full.copy_poses, half.copy_poses);
    }
}
