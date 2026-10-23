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
use manifold_node_engine::platonic::platonic_points;
pub use manifold_physics::collision_shape::Solid;

mod admission;
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
mod scan_collider_tests;
#[cfg(test)]
mod test_scene;
#[cfg(test)]
mod tests;
mod worker;
#[cfg(test)]
mod world_edit_tests;

pub use admission::{BodyRefusal, Member, RefusalReason};
use body_history::PackedRows;
use manifold_core::scene_schema::SPEED_RANGE;
use copy_group::CopyGroup;
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

/// Field and hit targets (body slots, then copy groups) a world gathers on
/// the stack each tick. A bigger world gathers them on the heap: this is no
/// limit, only where a tick stops being allocation-free.
pub use manifold_core::copies_body::MAX_COPIES;
pub const AUTHORED_HISTORY_CAPACITY: usize = 256;
/// The hits in flight a world holds before a burst grows its history.
const IMPULSE_CAPACITY: usize = 256;
const FIXED_TICK: Seconds = Seconds(1.0 / 60.0);

/// Why a worker refused a change of bodies: it needs a fresh world.
pub(crate) const NEEDS_NEW_WORLD: &str = "Physics worker cannot rebuild native topology within an epoch";

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

/// Platonic solid `shape` (tetrahedron 0 to dodecahedron 4) of circumradius
/// one as a hull solid, for tests that vary the body shape. One shared solid
/// per shape: the world tells shapes apart by identity.
#[cfg(test)]
pub(crate) fn platonic_solid(shape: u32) -> Arc<Solid> {
    static SOLIDS: std::sync::LazyLock<[Arc<Solid>; 5]> =
        std::sync::LazyLock::new(|| std::array::from_fn(|shape| Arc::new(Solid::Hulls(vec![platonic_points(shape as u32).to_vec()]))));
    if shape == 1 {
        return default_solid();
    }
    Arc::clone(&SOLIDS[shape as usize])
}

/// The collision shape a body carries when nothing prepared one: a cube of
/// circumradius one, the shape the body node's tests and tools start from.
pub fn default_solid() -> Arc<Solid> {
    static CUBE: std::sync::LazyLock<Arc<Solid>> =
        std::sync::LazyLock::new(|| Arc::new(Solid::Hulls(vec![platonic_points(1).to_vec()])));
    Arc::clone(&CUBE)
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
    pub kind: u32,
    /// kg: the mass the card sets, whatever the body's size. Where the engine
    /// needs a density it derives it from the installed hull's volume.
    pub mass: f32,
    pub friction: f32,
    pub bounce: f32,
    /// The prepared collision shape, local and unscaled; `None` while the
    /// body has none, which keeps it out of the world.
    pub collider: Option<Arc<Solid>>,
    /// Why the body node could not make a shape, shown when the world
    /// leaves the body out. Runtime-only.
    #[serde(skip)]
    pub refused: Option<Arc<str>>,
    /// A generated domain wall (`DomainWalls::install`, liquid and matter).
    /// Runtime-only: never saved, so authored data and take history are
    /// unchanged.
    #[serde(skip)]
    pub wall: bool,
    /// Which body node made this description, unique for that node's life;
    /// 0 for a body no node made (a wall, a test body). A recompile can move
    /// a body to another world port, and the world follows it by this so a
    /// moved body keeps moving. Runtime-only.
    #[serde(skip)]
    pub identity: u64,
    /// How many copies a Physics World makes of this body when it is wired
    /// into one of the world's copy-group inputs, and how it lays them out.
    /// Any other body ignores it.
    #[serde(default)]
    pub spawn: CopySpawn,
}

/// A copies body's spawner. Layout is 0 Grid, 1 Scatter, 2 Line, 3 Ring, 4
/// Spiral, 5 Random; spacing is a multiple of the copy's box width along the
/// layout axis; turn is each copy's own rotation inside the layout. A change
/// lays the copies out again at the next tick start (`physics/copy_layout.rs`).
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
            kind: 1,
            mass: DEFAULT_MASS_KG,
            friction: 0.5,
            bounce: 0.15,
            collider: Some(default_solid()),
            refused: None,
            wall: false,
            identity: 0,
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
/// (`install_added_bodies`), and a new shape or scale on an installed body is
/// swapped in place (`apply_authored_edits`), so neither resets the bodies
/// already moving. Removing or disabling a body, losing its shape, or
/// switching it between a static mesh and moving hulls still rebuilds. A
/// fragment child entering the world is always built with its parent.
fn slot_needs_rebuild(installed: bool, old: Option<&RigidBody>, new: Option<&RigidBody>) -> bool {
    let Some(new) = new.filter(|body| body.enabled) else {
        return installed;
    };
    if !installed {
        return new.fragment_parent.is_some();
    }
    !same_native_body(old, Some(new))
}

/// Whether two bodies can share one installed native body: the same
/// enabled state and the same solid form. A new shape or scale within a form
/// is swapped in place ([`reshaped`]).
fn same_native_body(left: Option<&RigidBody>, right: Option<&RigidBody>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.enabled == right.enabled
                && match (left.collider.as_deref(), right.collider.as_deref()) {
                    (None, None) => true,
                    (Some(left), Some(right)) => left.same_form(right),
                    _ => false,
                }
        }
        _ => false,
    }
}

/// Whether two bodies install the same geometry: shape and scale. A coupled
/// liquid's hulls and distance lattices are built from it.
fn same_geometry_body(left: Option<&RigidBody>, right: Option<&RigidBody>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.transform.scale == right.transform.scale
                && left.enabled == right.enabled
                && same_collider(left, right)
        }
        _ => false,
    }
}

/// Whether `new`'s authored pose differs from `old`'s: a scale write alone
/// reshapes the body and never teleports it.
fn moved(old: &RigidBody, new: &RigidBody) -> bool {
    old.transform.pos != new.transform.pos || old.transform.rot_euler != new.transform.rot_euler
}

/// Whether `new` asks for a different installed shape than `old`: a new
/// collision shape or a new scale.
fn reshaped(old: &RigidBody, new: &RigidBody) -> bool {
    old.transform.scale != new.transform.scale || !same_collider(old, new)
}

/// Install `body` with its card mass through the collision-shape gate;
/// returns the handle and the installed volume, which a fragment's density
/// derives from. An in-place reshape measures the volume again.
fn add_body_geometry(
    world: &mut PhysicsWorld,
    body: &RigidBody,
) -> Result<(BodyHandle, f32), String> {
    let solid = body.collider.as_deref().ok_or("the body has no collision shape")?;
    let scale = body.transform.scale;
    let handle = solid.install(world, scale, body.config()).map_err(|error| error.to_string())?;
    let volume = match solid {
        Solid::Hulls(_) => installed_volume(world, handle)?,
        Solid::Mesh(mesh) => mesh_volume(mesh, scale),
    };
    Ok((handle, volume))
}

/// Swap installed body `handle`'s shape for `solid` at `scale` in place,
/// keeping its pose and velocity; returns its new volume.
fn reshape(
    world: &mut PhysicsWorld,
    handle: BodyHandle,
    solid: &Solid,
    scale: [f32; 3],
    config: BodyConfig,
) -> Result<f32, String> {
    solid.replace(world, handle, scale, config).map_err(|error| error.to_string())?;
    match solid {
        Solid::Hulls(_) => installed_volume(world, handle),
        Solid::Mesh(mesh) => Ok(mesh_volume(mesh, scale)),
    }
}

/// A static mesh's enclosed volume at `scale`, m³, or its bounding box's
/// when it encloses none (an open surface).
fn mesh_volume(mesh: &manifold_physics::TriangleMesh, scale: [f32; 3]) -> f32 {
    let point = |index: u32| mesh.vertices[index as usize].map(f64::from);
    let mut volume = 0.0f64;
    for triangle in &mesh.triangles {
        let [a, b, c] = triangle.map(point);
        volume += a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0]) + a[2] * (b[0] * c[1] - b[1] * c[0]);
    }
    let scale_product = scale.iter().map(|s| f64::from(s.abs())).product::<f64>();
    let volume = (volume / 6.0).abs() * scale_product;
    if volume > 0.0 && volume.is_finite() {
        return volume as f32;
    }
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for vertex in &mesh.vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(f64::from(vertex[axis] * scale[axis]));
            max[axis] = max[axis].max(f64::from(vertex[axis] * scale[axis]));
        }
    }
    (0..3).map(|axis| (max[axis] - min[axis]).max(1.0e-3)).product::<f64>() as f32
}

/// How many copies each group makes from the world's pool of `MAX_COPIES`,
/// in port order, written into `made` (one entry per group; its storage is
/// reused, so a steady frame never allocates), and how many the pool left
/// unmade. A group whose physics is off makes no bodies and takes nothing
/// from the pool.
pub(crate) fn copy_pool<'a>(
    groups: impl Iterator<Item = Option<&'a RigidBody>>,
    made: &mut Vec<usize>,
) -> usize {
    made.clear();
    copy_pool_each(groups, |take| made.push(take))
}

/// [`copy_pool`], handing each group's share to `made` in port order.
pub(crate) fn copy_pool_each<'a>(
    groups: impl Iterator<Item = Option<&'a RigidBody>>,
    mut made: impl FnMut(usize),
) -> usize {
    let mut pool = MAX_COPIES;
    let mut not_made = 0;
    for prototype in groups {
        let Some(prototype) = prototype.filter(|body| body.enabled) else {
            made(0);
            continue;
        };
        let requested = requested_count(prototype.spawn);
        let take = requested.min(pool);
        made(take);
        not_made += requested - take;
        pool -= take;
    }
    not_made
}

/// A port's body identity: 0 for an empty port or a body no node made.
fn identity_of(body: &Option<RigidBody>) -> u64 {
    body.as_ref().map_or(0, |body| body.identity)
}

/// Which old port each new port's body was on: a body a node made matches
/// its own identity, and the anonymous ports (walls, empty ports, test
/// bodies) match each other in order. `None` when a port `held` names is
/// left with no new port, or an identity repeats.
fn port_map(old: &[u64], new: &[u64], held: impl Fn(usize) -> bool) -> Option<Vec<Option<usize>>> {
    let mut by_identity = ahash::AHashMap::with_capacity(old.len());
    for (at, &identity) in old.iter().enumerate() {
        if identity != 0 && by_identity.insert(identity, at).is_some() {
            return None;
        }
    }
    let mut anonymous = old.iter().enumerate().filter(|(_, identity)| **identity == 0).map(|(at, _)| at);
    let mut used = vec![false; old.len()];
    let mut from = Vec::with_capacity(new.len());
    for &identity in new {
        let at = if identity == 0 { anonymous.next() } else { by_identity.get(&identity).copied() };
        if let Some(at) = at
            && std::mem::replace(&mut used[at], true)
        {
            return None;
        }
        from.push(at);
    }
    (0..old.len()).all(|at| used[at] || !held(at)).then_some(from)
}

/// The sink a target's inputs go to: its body, or each active copy of its
/// group.
type ApplyInputs<'s> = dyn FnMut(&[FieldInput<'_>]) -> Result<(), manifold_physics::PhysicsError> + 's;

/// Give every installed body and every active copy its target's inputs (a
/// body slot, then a copy group) in one batch the world validates whole
/// before any body feels a force. `visit(target, apply)` builds target
/// `target`'s inputs on its own stack and hands them to `apply`, once per
/// target: copies of a group share them. Nothing is collected, so no tick
/// allocates at any body count. A target `hit` leaves out takes nothing.
fn apply_by_target(
    world: &mut PhysicsWorld,
    handles: &[Option<BodyHandle>],
    groups: &[CopyGroup],
    body_count: usize,
    hit: impl Fn(usize) -> bool,
    dt: Seconds,
    visit: impl Fn(usize, &mut ApplyInputs<'_>) -> Result<(), manifold_physics::PhysicsError>,
) -> Result<(), String> {
    world
        .apply_fields_each(dt, |sink| {
            for (index, handle) in handles.iter().enumerate() {
                let Some(handle) = *handle else { continue };
                if hit(index) {
                    visit(index, &mut |inputs| sink(handle, inputs))?;
                }
            }
            for (group, state) in groups.iter().enumerate() {
                if hit(body_count + group) {
                    visit(body_count + group, &mut |inputs| {
                        state.handles[..state.active].iter().flatten().try_for_each(|&handle| sink(handle, inputs))
                    })?;
                }
            }
            Ok(())
        })
        .map_err(|error| error.to_string())
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
    /// The gate each frame's bodies pass, and what it refused.
    admission: admission::Admission,
    /// Each body port's and copy-group port's body identity last frame
    /// (`RigidBody::identity`), so a recompile that moves a body to
    /// another port moves its state with it ([`Self::follow_ports`]).
    identities: Vec<u64>,
    group_identities: Vec<u64>,
    /// Each group's share of the copy pool ([`copy_pool`]), reused.
    pool_made: Vec<usize>,
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
            admission: Default::default(),
            identities: Vec::new(),
            group_identities: Vec::new(),
            pool_made: Vec::new(),
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

    /// The bodies the world leaves out this frame, and why.
    pub fn refusals(&self) -> &[BodyRefusal] {
        self.admission.refusals()
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

    /// Admit this frame's bodies through the gate ([`admission`]), then
    /// advance the world with what was admitted.
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
        self.accepted_observation = None;
        self.follow_ports(scene.bodies, scene.groups);
        self.admission.admit(scene.bodies, scene.groups);
        let (bodies, groups) = self.admission.take_admitted();
        let result = self.advance_admitted(
            SceneRef { bodies: &bodies, groups: &groups, ..scene },
            now,
            speed,
            reset_count,
            assigned_impulses,
            accepted_interval,
            coupling,
        );
        if result.is_ok() {
            self.show_refused(scene, &bodies, &groups);
        }
        self.admission.restore_admitted(bodies, groups);
        result
    }

    /// What the gate never let in still draws: a body where it stands, a
    /// copy group at rest in its layout, as with physics off.
    fn show_refused(&mut self, scene: SceneRef<'_>, bodies: &[Option<RigidBody>], groups: &[Option<RigidBody>]) {
        for ((authored, admitted), pose) in scene.bodies.iter().zip(bodies).zip(self.poses.iter_mut()) {
            if let (Some(authored), None) = (authored, admitted) {
                *pose = authored.transform;
            }
        }
        for ((authored, admitted), state) in scene.groups.iter().zip(groups).zip(self.groups.iter_mut()) {
            if let (Some(authored), None) = (authored, admitted) {
                state.show_refused(authored);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn advance_admitted<C: StepCoupling>(
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
        // Speed clamps to its range and never stops the world.
        let speed = speed.clamp(SPEED_RANGE.0, SPEED_RANGE.1);
        if !now.0.is_finite()
            || !speed.is_finite()
            || !reset_count.is_finite()
            || gravity.iter().any(|v| !v.is_finite())
        {
            if !self.step.offline() {
                self.metrics.record_simulation(
                    self.authored_time, self.physics_time, false, true,
                );
                return Ok(());
            }
            return Err("Physics: non-finite clock or control".into());
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
        validate_fragments(bodies)?;
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
        // A group the world has never laid out joins live
        // (`install_added_groups`); any other geometry change rebuilds.
        let copy_topology_changed = groups.len() != self.groups.len()
            || groups.iter().zip(&self.groups).any(|(current, state)| {
                let joins_live = self.world.is_some() && state.is_new() && current.is_some();
                !joins_live && !same_native_body(current.as_ref(), state.description.as_ref())
            });
        let rebuild = self.world.is_none() || topology_changed || copy_topology_changed || reset
            || (accepted_interval.is_none() && self.clock.rate_changed(self.step.interval.0));
        // A worker's world lives through its owner's epoch: a change it cannot
        // take live is the owner's to restart.
        if self.advancement_policy.is_worker() && self.world.is_some() && (topology_changed || copy_topology_changed) {
            return Err(NEEDS_NEW_WORLD.into());
        }
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
        let mut made = std::mem::take(&mut self.pool_made);
        let not_made = copy_pool(groups.iter().map(Option::as_ref), &mut made);
        let mut world = PhysicsWorld::with_workers(gravity, manifold_physics::default_worker_count())
            .map_err(|e| e.to_string())?;
        self.admission.clear_lost();
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
                    let pose = state.rest(copy_index);
                    state.handles[copy_index] = Some(state.install(&mut world, prototype, pose)?);
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
        self.pool_made = made;
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

    /// Lay out every copy group the world has never laid out, at rest, at the
    /// tick start that first sees it: Copies switched on mid-show joins the
    /// live world and what already moves keeps moving. A new group takes what
    /// the copy pool has left after the groups before it.
    fn install_added_groups(&mut self, groups: &[Option<RigidBody>]) -> Result<(), String> {
        if !groups.iter().zip(&self.groups).any(|(current, state)| state.is_new() && current.is_some()) {
            return Ok(());
        }
        let mut made = std::mem::take(&mut self.pool_made);
        let not_made = copy_pool(
            groups
                .iter()
                .zip(&self.groups)
                .map(|(current, state)| current.as_ref().filter(|_| state.arrangement.is_some() || state.is_new())),
            &mut made,
        );
        let mut result = Ok(());
        for (group, current) in groups.iter().enumerate() {
            let Some(prototype) = current.as_ref().filter(|_| self.groups[group].is_new()) else {
                continue;
            };
            let prototype = prototype.clone();
            self.groups[group].reset_for(Some(&prototype));
            if prototype.enabled {
                result = self.lay_out(group, &prototype, Arrangement::of(&prototype, made[group]));
                if result.is_err() {
                    break;
                }
            } else {
                let rest = Arrangement::of(&prototype, requested_count(prototype.spawn));
                self.groups[group].show_rest(&rest, prototype.transform.scale);
            }
            self.groups[group].description = Some(prototype);
        }
        self.pool_made = made;
        result?;
        self.copies_not_made = not_made;
        self.refresh_field_handles();
        Ok(())
    }

    /// Every installed body and active copy, for fields that reach them all.
    fn refresh_field_handles(&mut self) {
        self.field_handles.clear();
        self.field_handles.extend(self.handles.iter().flatten().copied());
        for state in &self.groups {
            self.field_handles.extend(state.handles[..state.active].iter().flatten().copied());
        }
    }

    /// Follow each body and copy group to the port a recompile moved it to
    /// (a body added before it in tree order shifts it), by its
    /// [`RigidBody::identity`]: its native body, history and fields move
    /// with it, so the world keeps running and a new body or group simply
    /// joins. A Shatter piece names its parent by port, so its description
    /// follows the parent's move too. A world that lost a body or group is
    /// left as it was, and the topology check rebuilds it as any removal
    /// does.
    fn follow_ports(&mut self, bodies: &[Option<RigidBody>], groups: &[Option<RigidBody>]) {
        let unchanged = |known: &[u64], slots: &[Option<RigidBody>]| {
            known.len() == slots.len() && known.iter().zip(slots).all(|(id, body)| *id == identity_of(body))
        };
        if unchanged(&self.identities, bodies) && unchanged(&self.group_identities, groups) {
            return;
        }
        let new_ids = |slots: &[Option<RigidBody>]| slots.iter().map(identity_of).collect::<Vec<_>>();
        let (body_ids, group_ids) = (new_ids(bodies), new_ids(groups));
        let installed = self.world.is_some()
            && self.handles.len() == self.identities.len()
            && self.groups.len() == self.group_identities.len();
        if installed {
            let held_body = |slot: usize| self.handles[slot].is_some() || self.descriptions[slot].is_some();
            let held_group = |group: usize| !self.groups[group].is_new();
            if let (Some(body_from), Some(group_from)) =
                (port_map(&self.identities, &body_ids, held_body), port_map(&self.group_identities, &group_ids, held_group))
            {
                self.move_ports(&body_from, &group_from);
            }
        }
        self.identities = body_ids;
        self.group_identities = group_ids;
    }

    /// Rearrange every per-port state so new body port `p` holds what old
    /// port `body_from[p]` held (nothing for a new port), and the same for
    /// copy groups.
    fn move_ports(&mut self, body_from: &[Option<usize>], group_from: &[Option<usize>]) {
        let old_bodies = self.handles.len();
        fn moved<T: Default>(old: &mut [T], from: &[Option<usize>]) -> Vec<T> {
            from.iter().map(|at| at.map_or_else(T::default, |at| std::mem::take(&mut old[at]))).collect()
        }
        self.handles = moved(&mut self.handles, body_from);
        self.descriptions = moved(&mut self.descriptions, body_from);
        let mut moved_to = vec![None; old_bodies];
        for (to, from) in body_from.iter().enumerate() {
            if let Some(from) = *from {
                moved_to[from] = Some(to);
            }
        }
        for body in self.descriptions.iter_mut().flatten() {
            if let Some(parent) = body.fragment_parent {
                body.fragment_parent = moved_to.get(parent).copied().flatten();
            }
        }
        self.volumes = moved(&mut self.volumes, body_from);
        self.bullet_enabled = moved(&mut self.bullet_enabled, body_from);
        self.deferred_animated_edit = moved(&mut self.deferred_animated_edit, body_from);
        self.fragment_active = moved(&mut self.fragment_active, body_from);
        self.fragment_release_latched = moved(&mut self.fragment_release_latched, body_from);
        self.fragment_parent_released = moved(&mut self.fragment_parent_released, body_from);
        self.poses = moved(&mut self.poses, body_from);
        self.groups = moved(&mut self.groups, group_from);
        let target_from: Vec<Option<usize>> = body_from
            .iter()
            .copied()
            .chain(group_from.iter().map(|at| at.map(|at| old_bodies + at)))
            .collect();
        self.authored_bodies.move_columns(&target_from);
        self.targeted_fields.move_targets(&target_from);
        self.admission.move_ports(body_from, group_from);
        self.refresh_field_handles();
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
        self.install_added_groups(groups)?;
        let world = self.world.as_mut().expect("world constructed above");
        for (i, body) in bodies.iter().enumerate() {
            let (Some(body), Some(handle)) = (body, self.handles[i]) else {
                continue;
            };
            let old = self.descriptions[i].as_ref();
            if old.is_none_or(|old| !same_body(old, body)) {
                if let (Some(old), Some(geometry)) = (old, body.collider.as_deref())
                    && reshaped(old, body)
                {
                    self.volumes[i] = reshape(world, handle, geometry, body.transform.scale, body.config())?;
                }
                let pose_changed = old.is_none_or(|old| moved(old, body));
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
            let pose_changed = animated && old.is_some_and(|old| moved(old, prototype));
            let kind_became_dynamic = prototype.kind == 1 && old.is_some_and(|old| old.kind != 1);
            // Every copy shares the prototype's shape, cooked once: each one
            // installed, active or parked, takes the new one in place, keeping
            // its pose and velocity. Later layouts space by the new width.
            if old.is_some_and(|old| reshaped(old, prototype)) && prototype.collider.is_some() {
                for index in 0..state.handles.len() {
                    let Some(handle) = state.handles[index] else { continue };
                    let config = state.config(prototype, state.rest(index));
                    state.reshape(world, handle, prototype, config)?;
                }
                state.width = copy_layout::copy_width(prototype);
            }
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
        let mut made = std::mem::take(&mut self.pool_made);
        let not_made = copy_pool(
            (0..groups).map(|group| {
                self.authored_cell(sample, body_count + group)
                    .filter(|_| self.groups[group].arrangement.is_some())
            }),
            &mut made,
        );
        let mut changed = false;
        let mut result = Ok(());
        for (group, &made) in made.iter().enumerate() {
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
            result = self.lay_out(group, &prototype, target);
            if result.is_err() {
                break;
            }
            changed = true;
        }
        self.pool_made = made;
        result?;
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
                    state.handles[index] = Some(state.install(world, prototype, pose)?);
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
        let world = self.world.as_mut().expect("world constructed above");
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
            if !in_range(pose) {
                world.set_enabled(handle, false).map_err(|e| e.to_string())?;
                self.admission.lost(Member::Body(i));
                self.poses[i] = body.transform;
                continue;
            }
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
        let mut made = std::mem::take(&mut self.pool_made);
        copy_pool(groups.iter().map(Option::as_ref), &mut made);
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
                    if !in_range(pose) {
                        world.set_enabled(handle, false).map_err(|e| e.to_string())?;
                        self.admission.lost(Member::Copies(group));
                        continue;
                    }
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
        self.pool_made = made;
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
            // A Shatter break, once per release: allocating here is off the tick.
            let mut velocities = vec![[0.0; 3]; self.descriptions.len()];
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
        let global = FieldInput { field: global, acceleration: 1.0, delta_velocity: 0.0 };
        let world = self.world.as_mut().expect("world constructed above");
        apply_by_target(world, &self.handles, &self.groups, body_count, |_| true, dt, |target, apply| {
            let field = span.field(target);
            apply(&[global, FieldInput { field: &field, acceleration: 1.0, delta_velocity: 0.0 }])
        })
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
    geometry.for_each_point(|point| {
        for axis in 0..3 {
            let scaled = point[axis] * body.transform.scale[axis];
            minimum[axis] = minimum[axis].min(scaled);
            maximum[axis] = maximum[axis].max(scaled);
        }
    });
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
    let mut radius = 0.0f32;
    geometry.for_each_point(|point| {
        let length = (0..3).map(|axis| (point[axis] * body.transform.scale[axis]).powi(2)).sum::<f32>().sqrt();
        radius = radius.max(length);
    });
    if radius > 0.0 && radius.is_finite() {
        radius
    } else {
        body.transform.scale.into_iter().fold(0.0, f32::max)
    }
}

/// Whether a solved pose is still one the world can hold: finite and inside
/// [`manifold_physics::collision_shape::MAX_COORDINATE`]. A body that blew up
/// is taken out before its bounds can corrupt the broad phase.
fn in_range(pose: manifold_physics::BodyPose) -> bool {
    pose.position.iter().all(|value| value.is_finite() && value.abs() <= manifold_physics::collision_shape::MAX_COORDINATE)
        && pose.rotation.iter().all(|value| value.is_finite())
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
