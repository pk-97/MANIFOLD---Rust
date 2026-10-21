//! Value descriptions on graph wires; native simulation ownership stays in the world node.
use manifold_core::{PlayedTime, Seconds};
use manifold_physics::stepping::{
    box3d_substep_count, StepCoupling, StepInterval, SubstepExchange, Uncoupled,
};
use manifold_physics::{
    input::{
        input_span, input_span_before, AppliedEvent, EventQueue, EventStamp, InputHistory,
        Timestamped,
    },
    BodyConfig, BodyHandle, BodyKind, FieldInput, FieldValue, PhysicsWorld, VectorField,
};
use std::sync::Arc;

use manifold_node_engine::scene::transform::Transform;
use manifold_core::scene_impulse::RigidImpulseTargets;
use manifold_node_engine::platonic::platonic_points;

mod body_history;
mod copy_group;
mod copy_layout;
#[cfg(test)]
mod copy_layout_tests;
#[cfg(test)]
mod coupling_tests;
#[cfg(test)]
mod group_tests;
mod impulses;
#[cfg(test)]
mod parity_tests;
mod targeted_fields;
#[cfg(test)]
mod test_scene;
#[cfg(test)]
mod tests;
mod worker;
#[cfg(test)]
mod world_edit_tests;

use body_history::PackedRows;
use copy_group::{validate_copy_prototype, CopyGroup};
pub(crate) use copy_group::validate_spawn;
use copy_layout::{requested_count, Arrangement};
pub use impulses::ResolvedRigidImpulse;
use targeted_fields::{TargetedFieldHistory, TargetedFields};
pub use worker::{RigidSceneInputs, RigidSceneObservation};
#[cfg(test)]
use test_scene::{copies_scene, targets};

pub use manifold_node_engine::exec::effect_node::SimStep;
use manifold_node_engine::exec::sim_metrics::SimMetricsSink;

/// Shared particle duration with the same advisory HUD path as other live sims.
pub fn particle_frame_duration(delta: Seconds, step: SimStep, metrics: SimMetricsSink<'_>) -> f32 {
    let outcome = manifold_physics::particle_duration::scaled(delta, step.offline());
    if outcome.diagnostic.is_some() {
        metrics.record_simulation(0.0, 0.0, false, true);
    }
    outcome.value
}

/// The physics clock's view of this frame's live load; None in export.
pub fn live_load(step: SimStep) -> Option<manifold_physics::clock::LiveLoad> {
    step.load().map(|load| manifold_physics::clock::LiveLoad { previous: load.previous, budget: load.budget })
}

#[cfg(feature = "gpu-proofs")]
thread_local! {
    static NATIVE_TICKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// GPU proofs: fixed Box3D ticks every world has completed on this thread
/// (the liquid conformance suite's I4 counter).
#[cfg(feature = "gpu-proofs")]
#[doc(hidden)]
pub fn native_ticks_on_this_thread() -> u64 {
    NATIVE_TICKS.get()
}


/// Transport paused or simulation speed zero. A capped simulation timestamp
/// can remain unchanged while transport advances, so it cannot diagnose pause.
/// Only full render observations judge; reset with the simulation.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct HeldClock {
    observed: Option<f64>,
    held: bool,
}

impl HeldClock {
    pub(crate) fn observe(&mut self, transport: f64, speed: f32, authored_sample_only: bool) {
        if !authored_sample_only {
            self.held = self.observed == Some(transport) || speed == 0.0;
            self.observed = Some(transport);
        }
    }

    pub(crate) fn is_held(self) -> bool {
        self.held
    }

}

/// The most individually wired bodies one world takes, one per body port.
pub const MAX_BODIES: usize = RigidImpulseTargets::BODY_CAPACITY;
/// The fixed slabs a coupled liquid's own copy of the world adds past a
/// full world's bodies: one per face of its domain box. They are never
/// authored, wired or hit.
pub const MAX_WALL_BODIES: usize = 6;
/// The body slots a world's simulation holds: the authored bodies, then a
/// coupled liquid's walls.
pub(crate) const MAX_BODY_ROWS: usize = MAX_BODIES + MAX_WALL_BODIES;
/// The most copy groups one world takes, one per copy-group port.
pub const MAX_COPY_GROUPS: usize = RigidImpulseTargets::GROUP_CAPACITY;
/// A field or hit target is a body slot, then a copy group.
pub(crate) const MAX_TARGETS: usize = MAX_BODY_ROWS + MAX_COPY_GROUPS;
/// The most copies one world makes across all its groups: the measured budget.
pub const MAX_COPIES: usize = 4_000;
pub const AUTHORED_HISTORY_CAPACITY: usize = 256;
const IMPULSE_CAPACITY: usize = 256;
const FIXED_TICK: Seconds = Seconds(1.0 / 60.0);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum AdvancementPolicy {
    #[default]
    Preview,
    Worker {
        max_ticks: usize,
    },
}

impl AdvancementPolicy {
    fn is_worker(self) -> bool {
        matches!(self, Self::Worker { .. })
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColliderGeometry {
    pub hulls: Vec<Vec<[f32; 3]>>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
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
    /// kg: the mass the card sets, whatever the body's size. Where the engine
    /// needs a density it derives it from the installed hull's volume.
    pub mass: f32,
    pub friction: f32,
    pub bounce: f32,
    pub collider: Option<Arc<ColliderGeometry>>,
    /// A generated domain wall (`DomainWalls::install`, liquid and matter).
    /// Runtime-only: never saved, so authored data and take history are
    /// unchanged.
    #[serde(skip)]
    pub wall: bool,
    /// How many copies a Physics World makes of this body when it is wired
    /// into one of the world's copy-group inputs, and how it lays them out.
    /// Any other body ignores it.
    #[serde(default)]
    pub spawn: CopySpawn,
}

/// A copies body's spawner. Layout is 0 Grid, 1 Scatter, 2 Line, 3 Ring;
/// spacing is a multiple of the copy's box width along the layout axis;
/// turn is each copy's own rotation inside the layout. A change lays the
/// copies out again at the next tick start (`physics/copy_layout.rs`).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopySpawn {
    pub count: f32,
    pub spacing: f32,
    pub columns: f32,
    pub layout: f32,
    #[serde(default)]
    pub turn: [f32; 3],
}

impl Default for CopySpawn {
    fn default() -> Self {
        Self { count: 100.0, spacing: manifold_core::copies_body::SIZE_SPACING_DEFAULT, columns: 16.0, layout: 0.0, turn: [0.0; 3] }
    }
}

/// An authored graph sample retained until all fixed ticks that can use it
/// have been replayed. Its bodies and copies bodies live in the parallel
/// `authored_bodies` rows. Between rendered samples, position and the raw
/// Euler angles are interpolated linearly. This does not reconstruct
/// nonlinear upstream animation between rendered samples.
#[derive(Clone)]
struct AuthoredPoseSample {
    time: f64,
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
            mass: DEFAULT_MASS_KG,
            friction: 0.5,
            bounce: 0.15,
            collider: None,
            wall: false,
            spawn: CopySpawn::default(),
        }
    }
}

/// A rigid body's mass when none is set, kg: a cubic metre of wood, which floats.
pub const DEFAULT_MASS_KG: f32 = 600.0;

impl RigidBody {
    /// Box3D's config for this body.
    fn config(&self) -> BodyConfig {
        self.config_at(pose_from_transform(self.transform))
    }

    /// [`Self::config`] at `pose` instead of the authored transform.
    fn config_at(&self, pose: manifold_physics::BodyPose) -> BodyConfig {
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
            wall: self.wall,
        }
    }
}

/// Shared scene Rz * Ry * Rx convention, with quaternion stored xyzw.
#[inline]
pub fn pose_from_transform(transform: Transform) -> manifold_physics::BodyPose {
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
        && left.wall == right.wall
        && left.spawn == right.spawn
        && same_collider(left, right)
}

fn same_authored_body(left: &RigidBody, right: &RigidBody) -> bool {
    same_body(left, right)
        && left.release_count == right.release_count
        && left.fragment_parent == right.fragment_parent
}

/// Whether a body slot's edit needs a fresh native world. A body entering an
/// empty or disabled slot is installed into the live world instead
/// (`install_added_bodies`), so it never resets the bodies already moving;
/// removing a body, or changing an installed body's shape or scale, still
/// rebuilds, and so does a collider change on a body that can move. A Fixed
/// body's new collider is swapped in place (`apply_authored_edits`), so
/// dragging a part of an imported scan never resets the world. A fragment
/// child is always built with its parent.
fn slot_needs_rebuild(installed: bool, old: Option<&RigidBody>, new: Option<&RigidBody>) -> bool {
    let Some(new) = new.filter(|body| body.enabled) else {
        return installed;
    };
    if !installed {
        return new.fragment_parent.is_some();
    }
    !same_topology_body(old, Some(new)) && !is_fixed_collider_swap(old, new)
}

/// Whether `new` differs from `old` only by the collider of a Fixed body.
fn is_fixed_collider_swap(old: Option<&RigidBody>, new: &RigidBody) -> bool {
    old.is_some_and(|old| {
        old.kind == 0
            && new.kind == 0
            && old.collider.is_some()
            && new.collider.is_some()
            && old.shape == new.shape
            && old.transform.scale == new.transform.scale
            && old.enabled == new.enabled
    })
}

/// Whether two bodies need the same native geometry: a change in any of
/// these rebuilds the world.
fn same_topology_body(left: Option<&RigidBody>, right: Option<&RigidBody>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.shape == right.shape
                && left.transform.scale == right.transform.scale
                && left.enabled == right.enabled
                && same_collider(left, right)
        }
        _ => false,
    }
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

/// Install `body` with its card mass; returns the handle and the volume of
/// the hulls Box3D installed, which a fragment's density derives from. A scale
/// or shape change rebuilds the world, so the volume holds for the install's
/// life.
fn add_body_geometry(
    world: &mut PhysicsWorld,
    body: &RigidBody,
) -> Result<(BodyHandle, f32), String> {
    add_copy(world, body, pose_from_transform(body.transform))
}

/// [`add_body_geometry`] for a copy of `prototype` resting at `pose`.
fn add_copy(
    world: &mut PhysicsWorld,
    prototype: &RigidBody,
    pose: manifold_physics::BodyPose,
) -> Result<(BodyHandle, f32), String> {
    let handle = if let Some(geometry) = prototype.collider.as_ref() {
        let scaled = scale_hulls(geometry, prototype.transform.scale);
        world.add_hulls(&scaled, prototype.config_at(pose))
    } else {
        let points = platonic_points(prototype.shape);
        let scaled = scale_platonic_points(points, prototype.transform.scale);
        world.add_hull(&scaled[..points.len()], prototype.config_at(pose))
    }
    .map_err(|error| error.to_string())?;
    Ok((handle, installed_volume(world, handle)?))
}

/// How many copies each group makes from the world's pool of `MAX_COPIES`,
/// in port order, and how many the pool left unmade. A group whose physics
/// is off makes no bodies and takes nothing from the pool.
pub(crate) fn copy_pool<'a>(
    groups: impl Iterator<Item = Option<&'a RigidBody>>,
) -> ([usize; MAX_COPY_GROUPS], usize) {
    let mut made = [0; MAX_COPY_GROUPS];
    let mut pool = MAX_COPIES;
    let mut not_made = 0;
    for (index, prototype) in groups.enumerate() {
        let Some(prototype) = prototype.filter(|body| body.enabled) else { continue };
        let requested = requested_count(prototype.spawn);
        made[index] = requested.min(pool);
        not_made += requested - made[index];
        pool -= made[index];
    }
    (made, not_made)
}

/// The signed volume of a body's installed hulls, m³: the sum over each
/// outward-wound triangle of the tetrahedron it spans with the origin.
pub(crate) fn installed_volume(world: &PhysicsWorld, handle: BodyHandle) -> Result<f32, String> {
    let meshes = world
        .hull_meshes(handle)
        .map_err(|error| error.to_string())?;
    let mut volume = 0.0f64;
    for mesh in &meshes {
        for triangle in &mesh.triangles {
            let [a, b, c] = triangle.map(|i| mesh.vertices[i as usize].map(f64::from));
            volume += a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]);
        }
    }
    let volume = (volume / 6.0) as f32;
    if volume.is_finite() && volume > 0.0 {
        Ok(volume)
    } else {
        Err(format!("a rigid body's hulls hold no volume ({volume} m³)"))
    }
}

/// One frame's authored inputs, borrowed: `bodies` is one entry per body
/// port and `groups` one per copy-group port. The world takes its body and
/// group counts from their lengths; a change in either rebuilds it.
#[derive(Clone, Copy)]
struct SceneRef<'a> {
    bodies: &'a [Option<RigidBody>],
    groups: &'a [Option<RigidBody>],
    gravity: [f32; 3],
    acceleration_field: Option<&'a FieldValue>,
    /// Empty, or one per body then one per group.
    targeted_fields: &'a [Option<FieldValue>],
}

impl<'a> SceneRef<'a> {
    fn of(scene: &'a RigidSceneInputs) -> Self {
        Self {
            bodies: &scene.bodies,
            groups: &scene.groups,
            gravity: scene.gravity,
            acceleration_field: scene.acceleration_field.as_ref(),
            targeted_fields: &scene.targeted_fields,
        }
    }
}

pub struct RigidSimulation {
    clock: manifold_physics::clock::SimulationClock,
    world: Option<PhysicsWorld>,
    // One entry per body port, sized when the world is built with a new
    // body count.
    handles: Vec<Option<BodyHandle>>,
    descriptions: Vec<Option<RigidBody>>,
    /// Each installed body's hull volume, m³: a fragment's mass is its parent's mass over the parent's volume, times this.
    volumes: Vec<f32>,
    bullet_enabled: Vec<bool>,
    /// Paused authoring edits remain visible while older preview ticks drain.
    /// The native teleport happens only after those ticks, so the edit cannot
    /// sweep through objects that lay between the old and new pose.
    deferred_animated_edit: Vec<Option<DeferredAnimatedEdit>>,
    fragment_active: Vec<bool>,
    fragment_release_latched: Vec<f32>,
    fragment_parent_released: Vec<bool>,
    /// One per copy-group port.
    groups: Vec<CopyGroup>,
    /// Copies the groups asked for past the world's `MAX_COPIES`.
    copies_not_made: usize,
    field_handles: Vec<BodyHandle>,
    last_time: Option<Seconds>,
    accumulator: f64,
    authored_time: f64,
    physics_time: f64,
    authored_samples: InputHistory<AuthoredPoseSample>,
    /// Each authored sample's bodies, then its copies bodies.
    authored_bodies: PackedRows<Option<RigidBody>>,
    targeted_fields: TargetedFieldHistory,
    reset_count: Option<f32>,
    pub poses: Vec<Transform>,
    pub physics_ms: f32,
    /// Fixed ticks owed by an explicitly bounded worker; live frames leave zero.
    pub pending_time: Seconds,
    last_overload_warning: Option<std::time::Instant>,
    impulse_queue: Option<EventQueue<ResolvedRigidImpulse>>,
    impulse_receipts: Vec<AppliedEvent<ResolvedRigidImpulse>>,
    impulse_tick_events: Vec<AppliedEvent<ResolvedRigidImpulse>>,
    impulse_epoch: Option<u64>,
    impulse_failure: Option<String>,
    /// Stamps of hits a held clock discarded, undrained, at most
    /// `IMPULSE_CAPACITY` so a held fire never allocates. They hold no slot.
    impulse_discarded: Vec<EventStamp>,
    /// Held discards past that bound, counted without a stamp.
    impulse_discarded_beyond: u64,
    accepted_observation: Option<(f64, f64)>,
    held: HeldClock,
    dropped_time: super::physics_metrics::DroppedTimeTracker,
    /// Completion telemetry since the owner last drained it.
    metrics: manifold_node_engine::exec::sim_metrics::SimMetrics,
    worker_epoch: Option<u64>,
    advancement_policy: AdvancementPolicy,
    /// The owner's simulation step for the next advance; see [`Self::set_step`].
    step: SimStep,
    /// Transport times the history replay samples before the next frame, from
    /// [`Self::request_samples`]: authored inputs are recorded only there.
    requests: Vec<f64>,
    /// A history replay drives this world's inputs. Without one (a direct
    /// caller), each frame records its own inputs.
    replay_driven: bool,
    /// The origin of a grid a frame started before its own time (a placement
    /// it landed past): nothing past it is simulated, and the replay samples
    /// from there, so every knot goes at the origin until a replay records
    /// at or past it or a frame moves transport on.
    owed_origin: Option<PlayedTime>,
}

impl Default for RigidSimulation {
    fn default() -> Self {
        Self {
            world: None,
            handles: Vec::new(),
            descriptions: Vec::new(),
            volumes: Vec::new(),
            bullet_enabled: Vec::new(),
            deferred_animated_edit: Vec::new(),
            fragment_active: Vec::new(),
            fragment_release_latched: Vec::new(),
            fragment_parent_released: Vec::new(),
            groups: Vec::new(),
            copies_not_made: 0,
            field_handles: Vec::new(),
            last_time: None,
            accumulator: 0.0,
            authored_time: 0.0,
            physics_time: 0.0,
            authored_samples: InputHistory::with_capacity(AUTHORED_HISTORY_CAPACITY)
                .expect("the fixed authored history capacity is valid"),
            authored_bodies: PackedRows::new(None),
            targeted_fields: TargetedFieldHistory::default(),
            reset_count: None,
            poses: Vec::new(),
            physics_ms: 0.0,
            pending_time: Seconds::ZERO,
            last_overload_warning: None,
            impulse_queue: None,
            impulse_receipts: Vec::with_capacity(IMPULSE_CAPACITY),
            impulse_tick_events: Vec::with_capacity(IMPULSE_CAPACITY),
            impulse_epoch: None,
            impulse_failure: None,
            impulse_discarded: Vec::with_capacity(IMPULSE_CAPACITY),
            impulse_discarded_beyond: 0,
            accepted_observation: None,
            clock: Default::default(),
            held: HeldClock::default(),
            dropped_time: Default::default(),
            metrics: Default::default(),
            worker_epoch: None,
            advancement_policy: AdvancementPolicy::Preview,
            step: SimStep::default(),
            requests: Vec::new(),
            replay_driven: false,
            owed_origin: None,
        }
    }
}

impl RigidSimulation {
    /// The step every following advance runs under. The owner sets it from
    /// its frame context before each advance, so no step outlives its frame.
    pub fn set_step(&mut self, step: SimStep) {
        self.step = step;
        self.clock.set_live_load(live_load(step));
        self.clock.set_played_target(step.played_target);
    }

    /// [`Self::set_step`] for chained construction in tests and tools.
    pub fn with_step(mut self, step: SimStep) -> Self {
        self.set_step(step);
        self
    }

    pub fn step(&self) -> SimStep {
        self.step
    }

    /// The completion telemetry recorded since the last call; the owning
    /// node folds it into its frame's metrics slot.
    pub fn take_metrics(&mut self) -> manifold_node_engine::exec::sim_metrics::SimMetrics {
        self.metrics.take()
    }

    /// Copy group `group`'s active copies' poses, in copy order.
    pub fn group_poses(&self, group: usize) -> &[Transform] {
        self.groups.get(group).map_or(&[], |state| &state.poses[..state.shown])
    }

    /// Copy group `group`'s active copies.
    pub fn active_copies(&self, group: usize) -> usize {
        self.groups.get(group).map_or(0, |state| state.active)
    }

    /// Active copies across every copy group.
    pub fn total_active_copies(&self) -> usize {
        self.groups.iter().map(|state| state.active).sum()
    }

    /// Copies the groups asked for that the world's `MAX_COPIES` left unmade
    /// at its last build; the later groups, by port, make what fits.
    pub fn copies_not_made(&self) -> usize {
        self.copies_not_made
    }

    /// Hold the transport clock while an upstream collider is still being
    /// prepared. The next ready frame starts from its current authored time
    /// instead of replaying time that elapsed with incomplete geometry.
    pub fn hold_pending(&mut self, now: PlayedTime) {
        self.accepted_observation = None;
        if now.is_finite() {
            self.last_time = Some(Seconds(now.seconds()));
        }
        self.pending_time = Seconds::ZERO;
    }

    /// Advance a world of individually wired bodies with no copies or
    /// fields: 60 Hz outer ticks, four Box3D substeps each. A paused
    /// transport contributes no time.
    pub fn advance(
        &mut self,
        bodies: &[Option<RigidBody>],
        gravity: [f32; 3],
        now: PlayedTime,
        speed: f32,
        reset_count: f32,
    ) -> Result<(), String> {
        let scene = SceneRef { bodies, groups: &[], gravity, acceleration_field: None, targeted_fields: &[] };
        self.advance_inner(scene, Seconds(now.seconds()), speed, reset_count, None, None, &mut Uncoupled)
    }

    /// Advance the shared world with every authored input: bodies, copy
    /// groups, the global field and the per-target fields. A copy group's
    /// Count, Layout, Spacing, Columns, Turn or group-transform edit lays its
    /// copies out at rest at the next tick start, without a rebuild.
    pub fn advance_scene(
        &mut self,
        scene: &RigidSceneInputs,
        now: PlayedTime,
        speed: f32,
        reset_count: f32,
    ) -> Result<(), String> {
        self.advance_inner(SceneRef::of(scene), Seconds(now.seconds()), speed, reset_count, None, None, &mut Uncoupled)
    }

    /// Run another native solver inside the existing rigid tick owner. Its
    /// prepared body bindings must belong to this world and epoch. Authored
    /// histories, edge events, contacts and publication retain their usual path.
    /// `now` is the owner's played time.
    pub fn advance_with_coupling<C: StepCoupling>(
        &mut self,
        scene: &RigidSceneInputs,
        now: PlayedTime,
        speed: f32,
        reset_count: f32,
        coupling: &mut C,
    ) -> Result<(), String> {
        self.advance_inner(SceneRef::of(scene), Seconds(now.seconds()), speed, reset_count, None, None, coupling)
    }

    /// Size every per-body and per-group vector for a world of `bodies`
    /// bodies and `groups` copy groups, once, when the world is built with
    /// new counts.
    fn resize_for(&mut self, bodies: usize, groups: usize) {
        if self.handles.len() == bodies
            && self.groups.len() == groups
            && self.authored_bodies.stride() == bodies + groups
        {
            return;
        }
        self.handles.resize(bodies, None);
        self.descriptions.resize(bodies, None);
        self.volumes.resize(bodies, 0.0);
        self.bullet_enabled.resize(bodies, false);
        self.deferred_animated_edit.resize(bodies, None);
        self.fragment_active.resize(bodies, false);
        self.fragment_release_latched.resize(bodies, 0.0);
        self.fragment_parent_released.resize(bodies, false);
        self.poses.resize(bodies, Transform::default());
        self.groups.resize_with(groups, CopyGroup::default);
        self.authored_bodies.configure(bodies + groups, AUTHORED_HISTORY_CAPACITY);
        self.targeted_fields.set_targets(bodies + groups);
    }

    #[allow(clippy::too_many_arguments)]
    fn advance_inner<C: StepCoupling>(
        &mut self,
        scene: SceneRef<'_>,
        now: Seconds,
        speed: f32,
        reset_count: f32,
        assigned_impulses: Option<&[AppliedEvent<ResolvedRigidImpulse>]>,
        accepted_interval: Option<StepInterval>,
        coupling: &mut C,
    ) -> Result<(), String> {
        let SceneRef { bodies, groups, gravity, targeted_fields: targeted_fields_input, .. } = scene;
        self.accepted_observation = None;
        if bodies.len() > MAX_BODY_ROWS || groups.len() > MAX_COPY_GROUPS {
            return Err(format!(
                "Physics: a world takes at most {MAX_BODIES} bodies, {MAX_WALL_BODIES} domain walls and {MAX_COPY_GROUPS} copy groups"
            ));
        }
        let target_slots = bodies.len() + groups.len();
        if !targeted_fields_input.is_empty() && targeted_fields_input.len() != target_slots {
            return Err(format!(
                "Physics: targeted fields require exactly {target_slots} entries"
            ));
        }
        let has_targeted_field = targeted_fields_input.iter().any(Option::is_some);
        let targeted_fields = if has_targeted_field
            || (!targeted_fields_input.is_empty() && self.targeted_fields.is_connected())
        {
            Some(TargetedFields::Values(targeted_fields_input))
        } else if self.targeted_fields.is_connected() {
            Some(TargetedFields::Cleared)
        } else {
            None
        };
        if !now.0.is_finite()
            || !speed.is_finite()
            || !(0.0..=4.0).contains(&speed)
            || !reset_count.is_finite()
            || gravity.iter().any(|v| !v.is_finite())
        {
            if !self.step.offline() {
                self.metrics.record_simulation(
                    self.authored_time, self.physics_time, false, true,
                );
                return Ok(());
            }
            return Err("Physics: non-finite clock/control or speed outside 0–4".into());
        }
        if let Some(interval) = accepted_interval {
            let duration = interval.duration().0;
            if !interval.start.0.is_finite()
                || !interval.end.0.is_finite()
                || !duration.is_finite()
                || duration <= 0.0
            {
                self.metrics.record_simulation(self.physics_time, self.physics_time, false, true);
                return Ok(());
            }
            if (interval.start.0 - self.physics_time).abs() > 1e-9 {
                self.metrics.record_simulation(interval.end.0, self.physics_time, false, true);
                return Ok(());
            }
        }
        for prototype in groups.iter().flatten() {
            validate_spawn(prototype.spawn)?;
        }
        validate_fragments(bodies)?;
        for prototype in groups.iter().flatten().filter(|body| body.enabled) {
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
        let topology_changed = bodies.len() != self.descriptions.len()
            || bodies.iter().zip(self.descriptions.iter()).enumerate().any(|(slot, (new, old))| {
                slot_needs_rebuild(self.handles.get(slot).is_some_and(Option::is_some), old.as_ref(), new.as_ref())
            });
        // A sample behind the last frame is a seek back, except the replay
        // opening at an owed origin: the frame that started that grid landed
        // past it, and the opening samples the span it skipped, Speed at its
        // tick starts included.
        let opening = self.step.authored_sample_only
            && self.owed_origin.is_some_and(|origin| now.0 >= origin.seconds() - 1e-9);
        let reset = self.reset_count.is_some_and(|old| old != reset_count)
            || (self.last_time.is_some_and(|last| now.0 < last.0) && !opening);
        let copy_topology_changed = groups.len() != self.groups.len()
            || groups
                .iter()
                .zip(&self.groups)
                .any(|(current, state)| !same_topology_body(current.as_ref(), state.description.as_ref()));
        let rebuild = self.world.is_none() || topology_changed || copy_topology_changed || reset
            || (accepted_interval.is_none() && self.clock.rate_changed(self.step.interval.0));
        if !rebuild && let Some(error) = &self.impulse_failure {
            return Err(error.clone());
        }
        if self.step.authored_sample_only && !self.advancement_policy.is_worker() {
            // A topology edit or seek rebuilds at the next full graph frame;
            // old trajectories cannot safely be spliced into a new world.
            if self.world.is_none() || topology_changed || copy_topology_changed || reset {
                return Ok(());
            }
            if !self.step.history_drain_active() {
                let authored_time = self.clock.observe_speed(PlayedTime::from_played(now.0), speed);
                let elapsed_simulation = authored_time - self.authored_time;
                self.ensure_targeted_history(targeted_fields)?;
                // A world no replay asks for samples keeps every observation,
                // as the full frame does.
                if self.replay_driven {
                    self.record_requested_samples(now.0, &scene, targeted_fields)?;
                } else {
                    let knot_time = self.owed_origin.map_or(authored_time, |origin| self.clock.simulation_at(origin, live_load(self.step)));
                    self.record_authored_sample(knot_time, &scene, targeted_fields)?;
                }
                self.authored_time = authored_time;
                self.accumulator += elapsed_simulation;
                self.last_time = Some(now);
                self.accepted_observation = Some((now.0, self.authored_time));
                return Ok(());
            }
        }
        let next_impulse_epoch = if rebuild {
            Some(if self.world.is_none() {
                if let Some(epoch) = self.worker_epoch {
                    epoch
                } else {
                    self.impulse_epoch
                        .unwrap_or(0)
                        .checked_add(1)
                        .ok_or("Physics: impulse epoch exhausted")?
                }
            } else {
                self.impulse_epoch
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or("Physics: impulse epoch exhausted")?
            })
        } else {
            None
        };
        if rebuild {
            self.rebuild(scene, next_impulse_epoch.expect("rebuild has a planned impulse epoch"), now)?;
        }
        // Workers receive simulation time already accepted by their owner.
        // Applying the display-frame cap again would truncate retained input
        // history, including a zero-step pose edit after coupled intervals.
        let clock_frame = if !self.advancement_policy.is_worker() {
            Some(self.clock.advance(PlayedTime::from_played(now.0), self.step.interval.0, speed, reset_count, rebuild,
                self.step.offline()))
        } else {
            None
        };
        let authored_time = if self.advancement_policy.is_worker() {
            now.0
        } else {
            self.clock.simulation_at(PlayedTime::from_played(now.0), live_load(self.step))
        };
        let elapsed_simulation = authored_time - self.authored_time;
        let stationary_edit = elapsed_simulation == 0.0;
        self.ensure_targeted_history(targeted_fields)?;
        self.record_requested_samples(now.0, &scene, targeted_fields)?;
        // Under a replay, inputs are knotted only at the times it was asked to
        // sample, which no frame rate moves; a held frame still records an
        // edit made while time stands still, and a frame that started a new
        // grid (a restart, a seek, an overload reanchor) starts its first tick
        // from its own inputs. That knot goes at the grid's origin, and every
        // knot past the origin's simulated time goes: it was recorded for
        // transport the new grid abandoned. A frame that landed past its
        // placement's target has simulated nothing past the target, so until
        // a replay samples from there, or transport moves on, every knot goes
        // at the origin, held frames included.
        let held_frame = clock_frame.as_ref().is_some_and(|frame| frame.held);
        let new_origin = clock_frame
            .as_ref()
            .filter(|frame| frame.restarted || frame.reanchored)
            .and_then(|_| self.clock.tick_start(self.clock.ticks_done()))
            .filter(|start| start.seconds() <= now.0 + 1e-9);
        if let Some(start) = new_origin {
            self.drop_authored_samples_after(self.clock.simulation_at(start, live_load(self.step)))?;
            self.owed_origin = (start.seconds() < now.0 - 1e-9).then_some(start);
        } else if clock_frame.is_some() && !held_frame {
            self.owed_origin = None;
        }
        if self.advancement_policy.is_worker() || !self.replay_driven || held_frame || new_origin.is_some() {
            let origin = new_origin.or(self.owed_origin);
            let knot_time = origin.map_or(authored_time, |start| self.clock.simulation_at(start, live_load(self.step)));
            self.record_authored_sample(knot_time, &scene, targeted_fields)?;
        }
        self.authored_time = authored_time;
        self.held.observe(now.0, speed, self.step.authored_sample_only);
        let accumulated = self.accumulator + elapsed_simulation;
        const TICK: f64 = FIXED_TICK.0;
        let due_steps = ((accumulated + 1e-9) / TICK).floor() as usize;
        let preview_budget = self.step.budget();
        // Live and export consume the same accepted rate intervals. Explicit
        // fixed workers remain the cache-recording compatibility path.
        let scheduled_frame = clock_frame.as_ref().filter(|_| matches!(self.advancement_policy, AdvancementPolicy::Preview));
        let steps = if let Some(frame) = scheduled_frame {
            frame.ticks as usize
        } else if accepted_interval.is_some() {
            1
        } else {
            match self.advancement_policy {
                AdvancementPolicy::Worker { max_ticks } => due_steps.min(max_ticks),
                AdvancementPolicy::Preview if speed == 0.0 && preview_budget.is_some() => 0,
                AdvancementPolicy::Preview => due_steps,
            }
        };
        self.apply_authored_edits(bodies, groups, stationary_edit, due_steps)?;
        self.sync_released_fragment_properties(bodies)?;
        let physics_start = std::time::Instant::now();
        let mut completed = 0;
        for ordinal in 0..steps {
            let accepted_interval = scheduled_frame.and_then(|frame| frame.interval(ordinal as u64))
                .or(accepted_interval);
            let result = (|| -> Result<(), String> {
                let (step_start, step_end) = accepted_interval.map_or_else(
                    || (self.physics_time, self.physics_time + FIXED_TICK.0),
                    |interval| (interval.start.0, interval.end.0),
                );
                let step_duration = if accepted_interval.is_some() { step_end - step_start } else { TICK };
                self.relayout_due(step_start, bodies.len())?;
                let tick_gravity = self.interpolated_gravity(step_start);
                let span = input_span(self.authored_samples.iter(), Seconds(self.physics_time))
                    .expect("authored input history is seeded before stepping");
                let field_before = span.before.acceleration_field.clone();
                let field_after = span.after.acceleration_field.clone();
                let field_alpha = span.alpha;
                let sampled_field = crate::vector_field::ContinuousField {
                    before: field_before.as_ref(),
                    after: field_after.as_ref(),
                    alpha: field_alpha,
                    origin: [0.0; 3],
                };
                let targeted_indices = self.targeted_fields.is_connected().then_some((
                    span.before_index,
                    span.after_index,
                    span.alpha,
                ));
                self.world
                    .as_mut()
                    .expect("world constructed above")
                    .set_gravity(tick_gravity)
                    .map_err(|e| e.to_string())?;
                let interval_receipt_start = if accepted_interval.is_some() {
                    self.impulse_receipts.len()
                } else {
                    0
                };
                let tick_stamp = if let Some(interval) = accepted_interval {
                    self.impulse_tick_events.clear();
                    let tick = self
                        .impulse_queue
                        .as_ref()
                        .expect("initialized world has an impulse queue")
                        .next_tick();
                    self.impulse_queue
                        .as_mut()
                        .expect("initialized world has an impulse queue")
                        .begin_interval(tick, interval, |event| {
                            self.impulse_receipts.push(event);
                        })
                        .map_err(|error| {
                            format!("Physics: failed to begin impulse interval: {error}")
                        })?;
                    if let Some(events) = assigned_impulses {
                        self.impulse_receipts.extend(events.iter().map(|event| AppliedEvent {
                            source: event.source,
                            applied: event.applied,
                            lateness: event.lateness,
                            value: event.value.clone(),
                        }));
                    }
                    tick
                } else {
                    self.begin_impulse_tick()?
                };
                if accepted_interval.is_none() {
                    if let Some(assigned_impulses) = assigned_impulses {
                        for event in assigned_impulses {
                            self.impulse_tick_events.push(AppliedEvent {
                                source: event.source,
                                applied: event.applied,
                                lateness: event.lateness,
                                value: event.value.clone(),
                            });
                        }
                    }
                    self.apply_impulse_tick()?;
                }
                let dynamic_microsteps = self.configure_fast_bodies(
                    bodies,
                    groups,
                    tick_gravity,
                    if sampled_field.is_empty() {
                        None
                    } else {
                        Some(&sampled_field)
                    },
                    targeted_indices,
                    step_duration,
                )?;
                let (animated_microsteps, animated_speed) =
                    self.animated_microsteps(bodies, groups, step_duration);
                let microsteps = animated_microsteps.max(dynamic_microsteps);
                self.world
                    .as_mut()
                    .expect("world constructed above")
                    .set_max_linear_speed(animated_speed.max(400.0))
                    .map_err(|e| e.to_string())?;
                let solver_substeps = box3d_substep_count(Seconds(step_duration)).value;
                let mut exchange = coupling
                    .begin_tick(tick_stamp, Seconds(step_duration))
                    .map_err(|error| format!("Physics coupling: {error}"))?;
                if accepted_interval.is_some() {
                    let event_count = self.impulse_receipts.len();
                    let mut cursor = step_start;
                    let mut frame_remaining = step_duration;
                    let mut index = interval_receipt_start;
                    while index < event_count {
                        let event_time = self.impulse_receipts[index].source.time.0.max(step_start);
                        self.integrate_interval_segment(
                            &mut exchange,
                            cursor,
                            event_time,
                            &mut frame_remaining,
                            false,
                            microsteps,
                            bodies,
                            groups,
                            &sampled_field,
                            targeted_indices,
                        )?;
                        self.impulse_tick_events.clear();
                        while index < event_count
                            && self.impulse_receipts[index].source.time.0.max(step_start)
                                == event_time
                        {
                            let event = &self.impulse_receipts[index];
                            self.impulse_tick_events.push(AppliedEvent {
                                source: event.source,
                                applied: event.applied,
                                lateness: event.lateness,
                                value: event.value.clone(),
                            });
                            index += 1;
                        }
                        self.apply_impulse_tick()?;
                        self.impulse_tick_events.clear();
                        cursor = event_time;
                    }
                    self.integrate_interval_segment(
                        &mut exchange,
                        cursor,
                        step_end,
                        &mut frame_remaining,
                        true,
                        microsteps,
                        bodies,
                        groups,
                        &sampled_field,
                        targeted_indices,
                    )?;
                } else {
                    // Explicit fixed-tick workers retain their recording schedule.
                    let microstep_time = TICK / microsteps as f64;
                    let mut tick_remaining = TICK;
                    for microstep in 1..=microsteps {
                        let target_time = self.physics_time + microstep_time * microstep as f64;
                        let mut remaining = if microstep == microsteps {
                            tick_remaining
                        } else {
                            microstep_time
                        };
                        while remaining > 0.0 {
                            self.prepare_substep(
                                bodies,
                                groups,
                                Seconds(target_time),
                                Seconds(remaining),
                                &sampled_field,
                                targeted_indices,
                            )?;
                            let world = self.world.as_mut().expect("world constructed above");
                            let duration = exchange.next_substep(world, Seconds(remaining))
                                .map_err(|error| format!("Physics coupling: {error}"))?;
                            if !duration.0.is_finite() || duration.0 <= 0.0
                                || duration.0 > remaining || remaining - duration.0 == remaining
                                || tick_remaining - duration.0 == tick_remaining
                            {
                                return Err("Physics coupling returned an invalid substep duration".into());
                            }
                            exchange.exchange(world, duration)
                                .map_err(|error| format!("Physics coupling: {error}"))?;
                            world.step(duration, solver_substeps).map_err(|error| error.to_string())?;
                            remaining -= duration.0;
                            tick_remaining -= duration.0;
                        }
                    }
                }
                exchange
                    .finish(self.world.as_ref().expect("world constructed above"))
                    .map_err(|error| format!("Physics coupling: {error}"))?;
                #[cfg(feature = "gpu-proofs")]
                NATIVE_TICKS.set(NATIVE_TICKS.get() + 1);
                completed += 1;
                self.physics_time = step_end;
                self.apply_due_authored_edits()?;
                self.process_fragment_releases(self.physics_time, bodies)?;
                Ok(())
            })();
            // Delivery records survive a failed native step too. Starting a
            // tick consumes its inputs, but does not assert successful physics.
            if accepted_interval.is_some() {
                // Interval receipts were recorded at begin_interval, including
                // failed native delivery; scratch must not append them twice.
                self.impulse_tick_events.clear();
            } else {
                self.finish_impulse_tick();
            }
            if let Err(error) = result {
                self.impulse_failure = Some(error.clone());
                return Err(error);
            }
        }
        self.pending_time = if accepted_interval.is_some() || scheduled_frame.is_some() {
            Seconds::ZERO
        } else {
            Seconds((due_steps - completed) as f64 * TICK)
        };
        self.apply_due_authored_edits()?;
        // The next tick starts where this frame's ticks ended.
        self.relayout_due(self.physics_time, bodies.len())?;
        if completed == 0 {
            self.process_fragment_releases(self.physics_time, bodies)?;
        }
        self.prune_authored_samples()?;
        if self.pending_time.0 > 0.0
            && speed > 0.0
            && self
                .last_overload_warning
                .is_none_or(|last| last.elapsed().as_secs() >= 2)
        {
            log::warn!(
                "Physics fixed worker: {:.1} ms still queued after {completed} ticks; preserving every physics step",
                self.pending_time.0 * 1000.0
            );
            self.last_overload_warning = Some(std::time::Instant::now());
        }
        // Live accepted spans finish here; bounded fixed workers retain their remainder.
        self.accumulator = if scheduled_frame.is_some() {
            (self.authored_time - self.physics_time).max(0.0)
        } else if accepted_interval.is_some() {
            0.0
        } else {
            (accumulated - completed as f64 * TICK).max(0.0)
        };
        self.last_time = Some(now);
        self.reset_count = Some(reset_count);
        self.descriptions.clone_from_slice(bodies);
        self.publish_poses(bodies, groups)?;
        self.physics_ms = physics_start.elapsed().as_secs_f32() * 1000.0;
        if !self.step.offline() {
            if let Some(frame) = &clock_frame {
                self.dropped_time.record(&mut self.metrics, self.authored_time, self.physics_time,
                    frame.dropped_seconds, false, frame.numerical_error);
            } else {
                self.metrics.record_simulation(self.authored_time, self.physics_time, false, false);
            }
        }
        self.accepted_observation = Some((now.0, self.authored_time));
        Ok(())
    }

    /// Build a fresh native world from `scene` at its authored poses, every
    /// copy group in its arrangement, so Reset returns copies to formation.
    /// The groups share the world's copy pool: the later groups, by port,
    /// make what fits.
    fn rebuild(
        &mut self,
        scene: SceneRef<'_>,
        epoch: u64,
        now: Seconds,
    ) -> Result<(), String> {
        let SceneRef { bodies, groups, gravity, acceleration_field, .. } = scene;
        let (made, not_made) = copy_pool(groups.iter().map(Option::as_ref));
        let mut world = PhysicsWorld::new(gravity).map_err(|e| e.to_string())?;
        self.resize_for(bodies.len(), groups.len());
        self.handles.fill(None);
        for (i, body) in bodies.iter().enumerate() {
            let Some(body) = body.as_ref().filter(|body| body.enabled) else {
                continue;
            };
            let (handle, volume) = add_body_geometry(&mut world, body)?;
            if body.fragment_parent.is_some() {
                world
                    .set_enabled(handle, false)
                    .map_err(|e| e.to_string())?;
            }
            self.handles[i] = Some(handle);
            self.volumes[i] = volume;
        }
        for (index, (prototype, state)) in groups.iter().zip(self.groups.iter_mut()).enumerate() {
            state.reset_for(prototype.as_ref());
            if let Some(prototype) = prototype.as_ref().filter(|body| body.enabled) {
                let arrangement = Arrangement::of(prototype, made[index]);
                state.arrangement = Some(arrangement);
                state.handles.resize(arrangement.count, None);
                state.bullet_enabled.resize(arrangement.count, false);
                for copy_index in 0..arrangement.count {
                    let (installed, volume) = add_copy(&mut world, prototype, state.rest(copy_index))?;
                    state.handles[copy_index] = Some(installed);
                    state.volume = volume;
                }
                state.active = arrangement.count;
                state.shown = arrangement.count;
                if state.poses.len() < arrangement.count {
                    state.poses.resize(arrangement.count, Transform::default());
                }
            } else if let Some(prototype) = prototype.as_ref() {
                state.show_rest(&Arrangement::of(prototype, requested_count(prototype.spawn)), prototype.transform.scale);
            }
            state.description.clone_from(prototype);
        }
        self.copies_not_made = not_made;
        self.reset_impulse_runtime(epoch)?;
        self.world = Some(world);
        self.bullet_enabled.fill(false);
        self.deferred_animated_edit.fill(None);
        self.fragment_active.fill(false);
        self.fragment_release_latched.fill(0.0);
        self.fragment_parent_released.fill(false);
        for (index, body) in bodies.iter().enumerate() {
            self.fragment_active[index] = body
                .as_ref()
                .is_some_and(|body| body.enabled && body.fragment_parent.is_none());
        }
        self.field_handles.clear();
        self.field_handles
            .extend(self.handles.iter().flatten().copied());
        for state in &self.groups {
            self.field_handles.extend(state.handles.iter().flatten().copied());
        }
        self.descriptions.clone_from_slice(bodies);
        self.last_time = Some(now);
        self.accumulator = 0.0;
        self.authored_time = 0.0;
        self.held = HeldClock::default();
        self.dropped_time.reset();
        self.physics_time = 0.0;
        self.authored_samples.clear();
        self.authored_bodies.clear();
        self.targeted_fields.clear();
        let write = self
            .authored_samples
            .record(
                AuthoredPoseSample {
                    time: 0.0,
                    gravity,
                    acceleration_field: acceleration_field.cloned(),
                },
                Seconds::ZERO,
            )
            .map_err(|error| format!("Physics: failed to seed input history: {error}"))?;
        self.authored_bodies
            .record(write, bodies.iter().chain(groups))
            .map_err(|error| format!("Physics: authored body history {error}"))?;
        if let Some(targeted_fields) = scene_targeted(scene, &self.targeted_fields) {
            self.ensure_targeted_history(Some(targeted_fields))?;
            self.targeted_fields
                .record(targeted_fields, manifold_physics::input::HistoryWrite::Replaced)?;
        }
        for (index, body) in bodies.iter().enumerate() {
            let Some(body) = body.as_ref().filter(|body| {
                body.enabled && body.fragment_parent.is_none() && body.release_count > 0.0
            }) else {
                continue;
            };
            self.release_fragments(index, body, body.release_count)?;
        }
        Ok(())
    }

    /// Put every enabled body that has no native body yet into the live
    /// world at its authored pose, at the tick start that first sees it.
    /// Bodies already moving keep their state.
    fn install_added_bodies(&mut self, bodies: &[Option<RigidBody>]) -> Result<(), String> {
        let world = self.world.as_mut().expect("world constructed above");
        let mut added = false;
        for (slot, body) in bodies.iter().enumerate() {
            let Some(body) = body.as_ref().filter(|body| body.enabled && body.fragment_parent.is_none()) else {
                continue;
            };
            if self.handles[slot].is_some() {
                continue;
            }
            let (handle, volume) = add_body_geometry(world, body)?;
            self.handles[slot] = Some(handle);
            self.volumes[slot] = volume;
            self.bullet_enabled[slot] = false;
            self.deferred_animated_edit[slot] = None;
            self.fragment_active[slot] = true;
            self.descriptions[slot] = Some(body.clone());
            added = true;
        }
        if added {
            self.field_handles.clear();
            self.field_handles.extend(self.handles.iter().flatten().copied());
            for state in &self.groups {
                self.field_handles.extend(state.handles[..state.active].iter().flatten().copied());
            }
        }
        Ok(())
    }

    /// Bring every installed body and copy in line with this frame's
    /// authored description: contact properties, kind and teleports.
    fn apply_authored_edits(
        &mut self,
        bodies: &[Option<RigidBody>],
        groups: &[Option<RigidBody>],
        stationary_edit: bool,
        due_steps: usize,
    ) -> Result<(), String> {
        self.install_added_bodies(bodies)?;
        let world = self.world.as_mut().expect("world constructed above");
        for (i, body) in bodies.iter().enumerate() {
            let (Some(body), Some(handle)) = (body, self.handles[i]) else {
                continue;
            };
            let old = self.descriptions[i].as_ref();
            if old.is_none_or(|old| !same_body(old, body)) {
                if let (Some(old), Some(geometry)) = (old, body.collider.as_ref())
                    && !same_collider(old, body)
                {
                    world
                        .replace_hulls(handle, &scale_hulls(geometry, body.transform.scale), body.config())
                        .map_err(|e| e.to_string())?;
                    self.volumes[i] = installed_volume(world, handle)?;
                }
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
        // Contact and kind edits apply here. A layout or group-transform edit
        // lays the copies out at the next tick start (`relayout_due`), except
        // that an Animated group follows its transform kinematically.
        for (prototype, state) in groups.iter().zip(self.groups.iter_mut()) {
            let (Some(prototype), Some(arrangement)) = (prototype.as_ref(), state.arrangement) else { continue };
            let old = state.description.as_ref();
            if old.is_some_and(|old| same_body(old, prototype)) {
                continue;
            }
            let animated = prototype.kind == 2;
            let pose_changed = animated && old.is_some_and(|old| old.transform != prototype.transform);
            let animated_edit = stationary_edit && pose_changed;
            if animated_edit && due_steps > 0 {
                state.deferred_edit = Some(DeferredAnimatedEdit {
                    time: self.authored_time,
                    body: prototype.clone(),
                });
            }
            let move_pose = animated_edit && due_steps == 0;
            if move_pose {
                state.arrangement = Some(Arrangement {
                    origin: prototype.transform.pos,
                    rotation: prototype.transform.rot_euler,
                    ..arrangement
                });
            }
            let kind_became_dynamic = prototype.kind == 1 && old.is_some_and(|old| old.kind != 1);
            for index in 0..state.active {
                let Some(handle) = state.handles[index] else {
                    continue;
                };
                world
                    .update_body(handle, state.config(prototype, state.rest(index)), move_pose)
                    .map_err(|e| e.to_string())?;
                if kind_became_dynamic {
                    world.set_bullet(handle, false).map_err(|e| e.to_string())?;
                    state.bullet_enabled[index] = false;
                }
            }
        }
        Ok(())
    }

    /// Lay out again, at the tick starting at `tick_start`, every copy group
    /// whose authored arrangement in effect then differs from the one its
    /// bodies rest in: the copies move to their new rest poses at zero
    /// velocity, tail bodies are enabled or disabled for a new Count, and
    /// bodies are installed only past a group's high-water mark. The
    /// arrangement in effect is the latest authored sample at or before the
    /// tick start, so the change lands on the same tick at any frame rate,
    /// live and in export. Other groups and bodies keep moving.
    fn relayout_due(&mut self, tick_start: f64, body_count: usize) -> Result<(), String> {
        if self.groups.iter().all(|state| state.arrangement.is_none()) {
            return Ok(());
        }
        let mut in_effect = None;
        for (index, sample) in self.authored_samples.iter().enumerate() {
            if sample.time > tick_start + 1.0e-9 {
                break;
            }
            in_effect = Some(index);
        }
        let Some(sample) = in_effect else { return Ok(()) };
        let groups = self.groups.len();
        let (made, not_made) = copy_pool((0..groups).map(|group| {
            self.authored_cell(sample, body_count + group)
                .filter(|_| self.groups[group].arrangement.is_some())
        }));
        let mut changed = false;
        for (group, &made) in made.iter().enumerate().take(groups) {
            let Some(installed) = self.groups[group].arrangement else { continue };
            let Some(prototype) = self.authored_cell(sample, body_count + group).filter(|body| body.enabled) else {
                continue;
            };
            let target = Arrangement::of(prototype, made);
            let same = if prototype.kind == 2 { installed.same_shape(&target) } else { installed == target };
            if same {
                continue;
            }
            let prototype = prototype.clone();
            self.lay_out(group, &prototype, target)?;
            changed = true;
        }
        if changed {
            self.copies_not_made = not_made;
            self.field_handles.clear();
            self.field_handles.extend(self.handles.iter().flatten().copied());
            for state in &self.groups {
                self.field_handles.extend(state.handles[..state.active].iter().flatten().copied());
            }
        }
        Ok(())
    }

    /// Put group `group`'s copies at rest in `target`.
    fn lay_out(&mut self, group: usize, prototype: &RigidBody, target: Arrangement) -> Result<(), String> {
        let world = self.world.as_mut().expect("world constructed above");
        let state = &mut self.groups[group];
        let installed = state.handles.len();
        if target.count > installed {
            state.handles.resize(target.count, None);
            state.bullet_enabled.resize(target.count, false);
            if state.poses.len() < target.count {
                state.poses.resize(target.count, Transform::default());
            }
        }
        state.arrangement = Some(target);
        state.deferred_edit = None;
        for index in 0..target.count {
            let pose = state.rest(index);
            match state.handles[index] {
                Some(handle) => {
                    if index >= state.active {
                        world.set_enabled(handle, true).map_err(|e| e.to_string())?;
                    }
                    world
                        .update_body(handle, state.config(prototype, pose), true)
                        .map_err(|e| e.to_string())?;
                    world.set_velocity(handle, [0.0; 3], [0.0; 3]).map_err(|e| e.to_string())?;
                    if state.bullet_enabled[index] {
                        world.set_bullet(handle, false).map_err(|e| e.to_string())?;
                        state.bullet_enabled[index] = false;
                    }
                }
                None => {
                    let (handle, volume) = add_copy(world, prototype, pose)?;
                    state.handles[index] = Some(handle);
                    state.volume = volume;
                }
            }
        }
        for handle in state.handles[target.count..state.active.max(target.count)].iter().flatten() {
            world.set_enabled(*handle, false).map_err(|e| e.to_string())?;
        }
        state.active = target.count;
        state.shown = target.count;
        Ok(())
    }

    /// Publish every body's and copy's pose after this frame's ticks.
    fn publish_poses(
        &mut self,
        bodies: &[Option<RigidBody>],
        groups: &[Option<RigidBody>],
    ) -> Result<(), String> {
        let world = self.world.as_ref().expect("world constructed above");
        // Resolve ordinary and released body poses first; inactive fragments
        // inherit their parent's current native pose below.
        for (i, body) in bodies.iter().enumerate().filter(|(i, body)| {
            body.as_ref()
                .is_none_or(|body| body.fragment_parent.is_none() || self.fragment_active[*i])
        }) {
            let Some(body) = body else {
                self.poses[i] = Transform::default();
                continue;
            };
            let Some(handle) = self.handles[i] else {
                self.poses[i] = body.transform;
                continue;
            };
            let pose = world.pose(handle).map_err(|e| e.to_string())?;
            let rot_euler = manifold_node_engine::scene::transform::quat_to_render_scene_euler(pose.rotation);
            self.poses[i] = Transform {
                pos: pose.position,
                rot_euler,
                scale: body.transform.scale,
                billboard: false,
                ..Default::default()
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
                ..Default::default()
            };
        }
        let (made, _) = copy_pool(groups.iter().map(Option::as_ref));
        for (group, (authored_body, state)) in groups.iter().zip(self.groups.iter_mut()).enumerate() {
            let Some(prototype) = authored_body.as_ref() else {
                state.shown = 0;
                state.description = None;
                continue;
            };
            let scale = prototype.transform.scale;
            let Some(installed) = state.arrangement else {
                // Physics off: the copies are drawn at rest and have no bodies.
                if state.description.as_ref().is_none_or(|old| !same_authored_body(old, prototype)) {
                    let count = requested_count(prototype.spawn);
                    state.show_rest(&Arrangement::of(prototype, count), scale);
                }
                state.description.clone_from(authored_body);
                continue;
            };
            let authored = Arrangement::of(prototype, made[group]);
            let pending = if prototype.kind == 2 { !installed.same_shape(&authored) } else { installed != authored };
            if pending {
                // Shown at rest until the next tick start lays the bodies out.
                state.show_rest(&authored, scale);
            } else if state.deferred_edit.is_some() {
                state.show_rest(&authored, scale);
            } else {
                for index in 0..state.active {
                    let Some(handle) = state.handles[index] else {
                        continue;
                    };
                    let pose = world.pose(handle).map_err(|e| e.to_string())?;
                    state.poses[index] = Transform {
                        pos: pose.position,
                        rot_euler: manifold_node_engine::scene::transform::quat_to_render_scene_euler(pose.rotation),
                        scale,
                        billboard: false,
                        ..Default::default()
                    };
                }
                state.shown = state.active;
            }
            state.description.clone_from(authored_body);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn integrate_interval_segment<E: SubstepExchange>(
        &mut self,
        exchange: &mut E,
        start: f64,
        end: f64,
        frame_remaining: &mut f64,
        final_segment: bool,
        microsteps: usize,
        bodies: &[Option<RigidBody>],
        groups: &[Option<RigidBody>],
        sampled_field: &crate::vector_field::ContinuousField<'_>,
        targeted_indices: Option<(usize, usize, f32)>,
    ) -> Result<(), String> {
        let duration = end - start;
        if duration <= 0.0 {
            return Ok(());
        }
        let microstep_time = duration / microsteps as f64;
        let mut segment_remaining = if final_segment { *frame_remaining } else { duration };
        for microstep in 1..=microsteps {
            let target_time = start
                + if microstep == microsteps {
                    duration
                } else {
                    microstep_time * microstep as f64
                };
            let mut remaining = if microstep == microsteps { segment_remaining } else { microstep_time };
            while remaining > 0.0 {
                self.prepare_substep(
                    bodies,
                    groups,
                    Seconds(target_time),
                    Seconds(remaining),
                    sampled_field,
                    targeted_indices,
                )?;
                let world = self.world.as_mut().expect("world constructed above");
                let accepted = exchange
                    .next_substep(world, Seconds(remaining))
                    .map_err(|error| format!("Physics coupling: {error}"))?;
                if !accepted.0.is_finite()
                    || accepted.0 <= 0.0
                    || accepted.0 > remaining
                    || remaining - accepted.0 == remaining
                {
                    return Err("Physics coupling returned an invalid substep duration".into());
                }
                exchange
                    .exchange(world, accepted)
                    .map_err(|error| format!("Physics coupling: {error}"))?;
                world
                    .step(accepted, box3d_substep_count(accepted).value)
                    .map_err(|error| error.to_string())?;
                remaining -= accepted.0;
                segment_remaining -= accepted.0;
                *frame_remaining -= accepted.0;
            }
        }
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
            let angular = world
                .angular_velocity(parent_handle)
                .map_err(|e| e.to_string())?;
            let mut velocities = [[0.0; 3]; MAX_BODY_ROWS];
            for (index, body) in self.descriptions.iter().enumerate() {
                if body
                    .as_ref()
                    .is_some_and(|body| body.fragment_parent == Some(parent_index))
                {
                    let Some(handle) = self.handles[index] else {
                        continue;
                    };
                    let center = world
                        .local_center_of_mass(handle)
                        .map_err(|e| e.to_string())?;
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
        for (index, body) in self.descriptions.iter().enumerate() {
            if body
                .as_ref()
                .is_none_or(|body| body.fragment_parent != Some(parent_index))
            {
                continue;
            }
            let Some(handle) = self.handles[index] else {
                continue;
            };
            let body = body.as_ref().expect("fragment description exists");
            // A piece is the parent's material: its own volume at the
            // parent's density (its card mass over its hull volume).
            let mut config = body.config();
            config.position = parent_pose.position;
            config.rotation = parent_pose.rotation;
            config.mass = parent.mass * self.volumes[index] / self.volumes[parent_index];
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
        bodies: &[Option<RigidBody>],
    ) -> Result<(), String> {
        for (parent_index, released) in self.fragment_parent_released.iter().copied().enumerate() {
            if !released {
                continue;
            }
            let Some(parent) = bodies[parent_index].as_ref() else {
                continue;
            };
            let parent_properties_changed =
                self.descriptions[parent_index]
                    .as_ref()
                    .is_none_or(|previous| {
                        previous.mass != parent.mass
                            || previous.friction != parent.friction
                            || previous.bounce != parent.bounce
                    });
            if !parent_properties_changed {
                continue;
            }
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
                config.mass = parent.mass * self.volumes[index] / self.volumes[parent_index];
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

    /// Authored sample `sample`'s body in cell `cell` (a body slot, then a
    /// copy group after the bodies).
    fn authored_cell(&self, sample: usize, cell: usize) -> Option<&RigidBody> {
        self.authored_bodies.row(sample)?.get(cell)?.as_ref()
    }

    fn authored_release_count(&self, index: usize, time: f64) -> Option<f32> {
        let first = self.authored_cell(0, index)?;
        let mut count = first.release_count;
        for (sample_index, sample) in self.authored_samples.iter().enumerate().skip(1) {
            if sample.time > time + 1.0e-12 {
                break;
            }
            if let Some(body) = self.authored_cell(sample_index, index) {
                count = body.release_count;
            }
        }
        Some(count)
    }

    fn process_fragment_releases(
        &mut self,
        time: f64,
        bodies: &[Option<RigidBody>],
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
            let count = self
                .authored_release_count(index, time)
                .unwrap_or(body.release_count);
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
        for state in &mut self.groups {
            let Some(edit) = state
                .deferred_edit
                .as_ref()
                .filter(|edit| self.physics_time + EPSILON >= edit.time)
            else {
                continue;
            };
            let prototype = &edit.body;
            let Some(arrangement) = state.arrangement else { continue };
            let placed = Arrangement { origin: prototype.transform.pos, rotation: prototype.transform.rot_euler, ..arrangement };
            for index in 0..state.active {
                let Some(handle) = state.handles[index] else {
                    continue;
                };
                world
                    .update_body(handle, prototype.config_at(placed.rest(state.width, index)), true)
                    .map_err(|e| e.to_string())?;
            }
            state.arrangement = Some(placed);
            state.deferred_edit = None;
        }
        Ok(())
    }

    /// Transport times in `(from, until]` the physics history replay must
    /// sample before the next frame: every Sim Rate boundary a tick may start
    /// or end at and every replay grid point, so inputs are recorded only at
    /// times no frame rate moves. Each boundary goes to one frame's window.
    pub fn request_samples(&mut self, from: PlayedTime, until: PlayedTime, out: &mut Vec<PlayedTime>) {
        self.replay_driven = true;
        self.requests.clear();
        let requests = &mut self.requests;
        self.clock.tick_starts(from, until, |played| requests.push(played.seconds()));
        let origin = self.step.replay_origin;
        let rate = manifold_physics::clock::REPLAY_RATE;
        let last = ((until.seconds() - origin.seconds()) * rate + 1e-6).floor();
        let mut point = ((from.seconds() - origin.seconds()) * rate + 1e-6).floor() + 1.0;
        while point <= last {
            requests.push(manifold_physics::clock::replay_point(origin, point).seconds());
            point += 1.0;
        }
        requests.sort_unstable_by(f64::total_cmp);
        requests.dedup();
        out.extend(requests.iter().map(|&played| PlayedTime::from_played(played)));
    }

    /// Record the inputs a sample at transport `now` reached for each pending
    /// request, at the simulated time of the request itself.
    fn record_requested_samples(
        &mut self,
        now: f64,
        scene: &SceneRef<'_>,
        targeted_fields: Option<TargetedFields<'_>>,
    ) -> Result<(), String> {
        let reached = self.requests.partition_point(|&transport| transport <= now + 1e-9);
        if let Some(origin) = self.owed_origin
            && self.requests[..reached].iter().any(|&request| request >= origin.seconds() - 1e-9)
        {
            self.owed_origin = None;
        }
        for index in 0..reached {
            // A request a rounding past `now` is the same instant spelled by
            // another formula (grid point 4k/240 against frame k/60): it takes
            // the frame's spelling, so a knot at the frame never lands behind it.
            let request = self.requests[index].min(now);
            let time = self.clock.simulation_at(PlayedTime::from_played(request), live_load(self.step));
            self.record_authored_sample(time, scene, targeted_fields)?;
        }
        self.requests.drain(..reached);
        Ok(())
    }

    fn record_authored_sample(
        &mut self,
        time: f64,
        scene: &SceneRef<'_>,
        targeted_fields: Option<TargetedFields<'_>>,
    ) -> Result<(), String> {
        if self.authored_samples.is_exhausted() {
            return Err(
                "Physics: authored input history is exhausted; reset the simulation to continue"
                    .into(),
            );
        }
        let unchanged = self.authored_samples.back().is_some_and(|last| {
            last.time == time
                && last.gravity == scene.gravity
                && last.acceleration_field.as_ref() == scene.acceleration_field
        }) && self.authored_bodies.back().is_some_and(|row| {
            row.len() == scene.bodies.len() + scene.groups.len()
                && row
                    .iter()
                    .zip(scene.bodies.iter().chain(scene.groups))
                    .all(|(left, right)| same_optional_body(left.as_ref(), right.as_ref()))
        }) && targeted_fields.is_none_or(|fields| self.targeted_fields.fields_equal(fields));
        if unchanged {
            return Ok(());
        }
        let write = self
            .authored_samples
            .record(
                AuthoredPoseSample {
                    time,
                    gravity: scene.gravity,
                    acceleration_field: scene.acceleration_field.cloned(),
                },
                Seconds(self.physics_time),
            )
            .map_err(|error| format!("Physics: input history rejected authored sample: {error}"))?;
        self.authored_bodies
            .record(write, scene.bodies.iter().chain(scene.groups))
            .map_err(|error| format!("Physics: authored body history {error}"))?;
        if let Some(fields) = targeted_fields {
            self.targeted_fields.record(fields, write)?;
        }
        Ok(())
    }

    fn ensure_targeted_history(
        &mut self,
        targeted_fields: Option<TargetedFields<'_>>,
    ) -> Result<(), String> {
        if targeted_fields.is_some() && !self.targeted_fields.is_connected() {
            self.targeted_fields
                .ensure_aligned(self.authored_samples.len())?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn configure_fast_bodies(
        &mut self,
        bodies: &[Option<RigidBody>],
        groups: &[Option<RigidBody>],
        gravity: [f32; 3],
        acceleration_field: Option<&dyn VectorField>,
        targeted_indices: Option<(usize, usize, f32)>,
        tick: f64,
    ) -> Result<usize, String> {
        // Box3D skips bullet targets during the bullet pass. When two fast
        // individual Dynamics share a world, use smaller outer steps instead
        // of making each invisible to the other's continuous pass.
        let targeted_span = match targeted_indices {
            Some((before, after, alpha)) => Some(self.targeted_fields.span(before, after, alpha)?),
            None => None,
        };
        let any_copies = self.groups.iter().any(|state| state.active > 0);
        let world = self.world.as_mut().expect("world constructed above");
        let mut fast_count = 0;
        let mut fast_steps = 1;
        if !any_copies {
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
                    targeted
                        .as_ref()
                        .filter(|field| !field.is_empty())
                        .map(|field| field as &dyn VectorField),
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
                targeted
                    .as_ref()
                    .filter(|field| !field.is_empty())
                    .map(|field| field as &dyn VectorField),
            )?;
            let enabled = fast_count < 2 && needs_bullet(body, velocity, acceleration, tick);
            if self.bullet_enabled[index] != enabled {
                world
                    .set_bullet(handle, enabled)
                    .map_err(|e| e.to_string())?;
                self.bullet_enabled[index] = enabled;
            }
        }
        for (group, (prototype, state)) in groups.iter().zip(self.groups.iter_mut()).enumerate() {
            let active = state.active;
            let Some(prototype) = prototype.as_ref().filter(|body| body.enabled && body.kind == 1) else {
                state.bullet_enabled[..active].fill(false);
                continue;
            };
            let targeted = targeted_span.as_ref().map(|span| span.field(bodies.len() + group));
            for index in 0..active {
                let Some(handle) = state.handles[index] else {
                    continue;
                };
                let velocity = world.linear_velocity(handle).map_err(|e| e.to_string())?;
                let acceleration = summed_acceleration(
                    world,
                    handle,
                    gravity,
                    acceleration_field,
                    targeted
                        .as_ref()
                        .filter(|field| !field.is_empty())
                        .map(|field| field as &dyn VectorField),
                )?;
                let enabled = needs_bullet(prototype, velocity, acceleration, tick);
                if state.bullet_enabled[index] != enabled {
                    world
                        .set_bullet(handle, enabled)
                        .map_err(|e| e.to_string())?;
                    state.bullet_enabled[index] = enabled;
                }
            }
        }
        Ok(if fast_count >= 2 { fast_steps } else { 1 })
    }

    /// Queue this interval's authored motion and existing continuous fields.
    /// The native step consumes forces; a coupled owner can use this same
    /// preparation before each accepted fluid/rigid subdivision.
    fn prepare_substep(
        &mut self,
        bodies: &[Option<RigidBody>],
        groups: &[Option<RigidBody>],
        target_time: Seconds,
        dt: Seconds,
        global: &crate::vector_field::ContinuousField<'_>,
        indices: Option<(usize, usize, f32)>,
    ) -> Result<(), String> {
        for (i, body) in bodies.iter().enumerate() {
            let Some(body) = body else { continue };
            if (body.fragment_parent.is_some() && !self.fragment_active[i])
                || self.fragment_parent_released[i]
            {
                continue;
            }
            if !(body.enabled && body.kind == 2) {
                continue;
            }
            let Some(handle) = self.handles[i] else {
                continue;
            };
            let target = self
                .interpolated_body(i, target_time.0, body.clone())
                .unwrap_or_else(|| body.clone());
            self.world
                .as_mut()
                .expect("world constructed above")
                .set_animated_target(handle, target.config(), dt)
                .map_err(|e| e.to_string())?;
        }
        for (group, prototype) in groups.iter().enumerate() {
            let Some(prototype) = prototype.as_ref().filter(|prototype| prototype.enabled && prototype.kind == 2) else {
                continue;
            };
            let prototype = self
                .interpolated_body(bodies.len() + group, target_time.0, prototype.clone())
                .unwrap_or_else(|| prototype.clone());
            let state = &self.groups[group];
            let Some(arrangement) = state.arrangement else { continue };
            let placed = Arrangement { origin: prototype.transform.pos, rotation: prototype.transform.rot_euler, ..arrangement };
            let world = self.world.as_mut().expect("world constructed above");
            for index in 0..state.active {
                let Some(handle) = state.handles[index] else {
                    continue;
                };
                world
                    .set_animated_target(handle, prototype.config_at(placed.rest(state.width, index)), dt)
                    .map_err(|e| e.to_string())?;
            }
        }
        self.apply_sampled_fields(dt, global, indices, bodies.len())
    }

    fn apply_sampled_fields(
        &mut self,
        dt: Seconds,
        global: &crate::vector_field::ContinuousField<'_>,
        indices: Option<(usize, usize, f32)>,
        body_count: usize,
    ) -> Result<(), String> {
        let Some((before, after, alpha)) = indices else {
            if global.is_empty() {
                return Ok(());
            }
            return self
                .world
                .as_mut()
                .expect("world constructed above")
                .apply_fields(
                    &self.field_handles,
                    &[FieldInput {
                        field: global,
                        acceleration: 1.0,
                        delta_velocity: 0.0,
                    }],
                    dt,
                )
                .map_err(|error| error.to_string());
        };
        let span = self.targeted_fields.span(before, after, alpha)?;
        let targets = body_count + self.groups.len();
        let fields: [_; MAX_TARGETS] = std::array::from_fn(|index| {
            if index < targets {
                span.field(index)
            } else {
                crate::vector_field::ContinuousField { before: None, after: None, alpha: 0.0, origin: [0.0; 3] }
            }
        });
        let inputs: [[FieldInput<'_>; 2]; MAX_TARGETS] = std::array::from_fn(|index| {
            [
                FieldInput {
                    field: global,
                    acceleration: 1.0,
                    delta_velocity: 0.0,
                },
                FieldInput {
                    field: &fields[index],
                    acceleration: 1.0,
                    delta_velocity: 0.0,
                },
            ]
        });
        let recipients = self
            .handles
            .iter()
            .enumerate()
            .filter_map(|(index, handle)| handle.map(|handle| (handle, inputs[index].as_slice())))
            .chain(self.groups.iter().enumerate().flat_map(|(group, state)| {
                let input = inputs[body_count + group].as_slice();
                state.handles[..state.active]
                    .iter()
                    .flatten()
                    .map(move |&handle| (handle, input))
            }));
        // Validate the complete batch before any body receives a force. Copies
        // share one input slice and one linear validation pass.
        self.world
            .as_mut()
            .expect("world constructed above")
            .apply_fields_by_target(recipients, dt)
            .map_err(|error| error.to_string())
    }

    fn animated_microsteps(
        &self,
        bodies: &[Option<RigidBody>],
        groups: &[Option<RigidBody>],
        tick: f64,
    ) -> (usize, f32) {
        // Box3D bullet CCD does not sweep Animated motion. Smaller outer
        // steps put fast moving/rotating colliders into contact with Dynamics.
        const MAX_MICROSTEPS: usize = 512;
        let group_active = |group: usize| self.groups.get(group).is_some_and(|state| state.active > 0);
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
            .chain(groups.iter().enumerate().filter_map(|(group, body)| {
                body.as_ref()
                    .filter(|body| body.enabled && body.kind == 1 && group_active(group))
                    .map(body_min_extent)
            }))
            .fold(f32::INFINITY, f32::min);
        if !dynamic_extent.is_finite() {
            return (1, 400.0);
        }
        let start_time = self.physics_time;
        let end_time = start_time + tick;
        let mut travel: f32 = 0.0;
        let mut required_speed: f32 = 400.0;
        let animated = bodies
            .iter()
            .enumerate()
            .filter(|(index, body)| {
                body.as_ref().is_some_and(|body| {
                    body.enabled
                        && body.kind == 2
                        && (body.fragment_parent.is_none() || self.fragment_active[*index])
                        && !self.fragment_parent_released[*index]
                })
            })
            .chain(groups.iter().enumerate().map(|(group, body)| (bodies.len() + group, body)).filter(
                |(cell, body)| {
                    body.as_ref().is_some_and(|body| {
                        body.enabled && body.kind == 2 && group_active(*cell - bodies.len())
                    })
                },
            ));
        for (cell, body) in animated {
            let body = body.as_ref().expect("filtered to present bodies");
            let start = self
                .interpolated_body(cell, start_time, body.clone())
                .unwrap_or_else(|| body.clone());
            let mut previous = start;
            let mut previous_time = start_time;
            let mut path = 0.0;
            for (sample_index, sample) in self
                .authored_samples
                .iter()
                .enumerate()
                .filter(|(_, sample)| sample.time > start_time && sample.time < end_time)
            {
                if let Some(next) = self.authored_cell(sample_index, cell) {
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
                .interpolated_body(cell, end_time, body.clone())
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

    /// `current` at the authored pose of cell `cell` (a body slot, then a
    /// copy group after the bodies) interpolated to `time`.
    fn interpolated_body(
        &self,
        cell: usize,
        time: f64,
        mut current: RigidBody,
    ) -> Option<RigidBody> {
        let span = input_span_before(self.authored_samples.iter(), Seconds(time))?;
        let previous = self.authored_cell(span.before_index, cell)?.clone();
        let next = self.authored_cell(span.after_index, cell)?.clone();
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
        self.authored_bodies
            .prune(removed)
            .map_err(|error| format!("Physics: authored body history {error}"))?;
        self.targeted_fields.prune(removed)?;
        Ok(())
    }

    /// Drop every knot later than `time`, keeping the parallel targeted
    /// storage aligned.
    fn drop_authored_samples_after(&mut self, time: f64) -> Result<(), String> {
        let removed = self
            .authored_samples
            .drop_after(Seconds(time))
            .map_err(|error| format!("Physics: failed to trim input history: {error}"))?;
        self.authored_bodies
            .drop_back(removed)
            .map_err(|error| format!("Physics: authored body history {error}"))?;
        self.targeted_fields.drop_back(removed)
    }
}

/// The targeted fields a freshly built world seeds its history with.
fn scene_targeted<'a>(
    scene: SceneRef<'a>,
    history: &TargetedFieldHistory,
) -> Option<TargetedFields<'a>> {
    if scene.targeted_fields.iter().any(Option::is_some) {
        Some(TargetedFields::Values(scene.targeted_fields))
    } else if history.is_connected() {
        Some(TargetedFields::Cleared)
    } else {
        None
    }
}

fn same_optional_body(left: Option<&RigidBody>, right: Option<&RigidBody>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => same_authored_body(left, right),
        _ => false,
    }
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

fn validate_fragments(bodies: &[Option<RigidBody>]) -> Result<(), String> {
    for (index, body) in bodies.iter().enumerate() {
        let Some(body) = body else { continue };
        if !body.release_count.is_finite() || body.release_count < 0.0 {
            return Err(format!(
                "Physics: body {index} release count must be finite and non-negative"
            ));
        }
        let Some(parent) = body.fragment_parent else {
            continue;
        };
        if parent >= bodies.len() || parent == index {
            return Err(format!(
                "Physics: body {index} has an invalid fragment parent"
            ));
        }
        let Some(parent_body) = bodies[parent].as_ref() else {
            return Err(format!(
                "Physics: body {index} references a missing fragment parent"
            ));
        };
        if parent_body.fragment_parent.is_some() {
            return Err(format!(
                "Physics: fragment parent chains are not supported (body {index})"
            ));
        }
    }
    Ok(())
}
