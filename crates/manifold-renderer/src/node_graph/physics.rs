//! Value descriptions on graph wires; native simulation ownership stays in the world node.
use manifold_core::Seconds;
use manifold_physics::{BodyConfig, BodyHandle, BodyKind, PhysicsWorld};
use std::collections::VecDeque;

use super::transform::Transform;
use crate::generators::platonic_geometry::platonic_points;

thread_local! {
    // A preview budget only yields work; it never discards simulation time.
    static PREVIEW_STEP_BUDGET: std::cell::Cell<Option<std::time::Duration>> = const { std::cell::Cell::new(None) };
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

pub const MAX_BODIES: usize = 16;
pub const MAX_COPIES: usize = 4_000;
pub const BODY_PORTS: [&str; MAX_BODIES] = [
    "body_0", "body_1", "body_2", "body_3", "body_4", "body_5", "body_6", "body_7", "body_8",
    "body_9", "body_10", "body_11", "body_12", "body_13", "body_14", "body_15",
];
pub const POSE_PORTS: [&str; MAX_BODIES] = [
    "pose_0", "pose_1", "pose_2", "pose_3", "pose_4", "pose_5", "pose_6", "pose_7", "pose_8",
    "pose_9", "pose_10", "pose_11", "pose_12", "pose_13", "pose_14", "pose_15",
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigidBody {
    pub transform: Transform,
    pub shape: u32,
    pub kind: u32,
    pub mass: f32,
    pub friction: f32,
    pub bounce: f32,
}

/// An authored graph sample retained until all fixed ticks that can use it
/// have been replayed. Between rendered samples, position and the raw Euler
/// angles are interpolated linearly. This does not reconstruct nonlinear
/// upstream animation between rendered samples.
#[derive(Clone, Copy)]
struct AuthoredPoseSample {
    time: f64,
    bodies: [Option<RigidBody>; MAX_BODIES],
    prototype: Option<RigidBody>,
}

impl Default for RigidBody {
    fn default() -> Self {
        Self {
            transform: Transform::default(),
            shape: 1,
            kind: 1,
            mass: 1.0,
            friction: 0.5,
            bounce: 0.15,
        }
    }
}

impl RigidBody {
    fn config(self) -> BodyConfig {
        // Same Rz * Ry * Rx convention as render_scene, quaternion stored xyzw.
        let [x, y, z] = self.transform.rot_euler;
        let (sx, cx) = (x * 0.5).sin_cos();
        let (sy, cy) = (y * 0.5).sin_cos();
        let (sz, cz) = (z * 0.5).sin_cos();
        BodyConfig {
            kind: match self.kind {
                0 => BodyKind::Fixed,
                2 => BodyKind::Animated,
                _ => BodyKind::Dynamic,
            },
            position: self.transform.pos,
            rotation: [
                sx * cy * cz - cx * sy * sz,
                cx * sy * cz + sx * cy * sz,
                cx * cy * sz - sx * sy * cz,
                cx * cy * cz + sx * sy * sz,
            ],
            mass: self.mass,
            friction: self.friction,
            restitution: self.bounce,
        }
    }
}

pub struct RigidSimulation {
    world: Option<PhysicsWorld>,
    handles: [Option<BodyHandle>; MAX_BODIES],
    descriptions: [Option<RigidBody>; MAX_BODIES],
    bullet_enabled: [bool; MAX_BODIES],
    copy_handles: Vec<Option<BodyHandle>>,
    copy_description: Option<RigidBody>,
    copy_bullet_enabled: Vec<bool>,
    latched_copy_count: usize,
    latched_copy_spacing: f32,
    latched_copy_columns: usize,
    latched_copy_layout: CopyLayout,
    last_time: Option<Seconds>,
    accumulator: f64,
    authored_time: f64,
    physics_time: f64,
    authored_samples: VecDeque<AuthoredPoseSample>,
    reset_count: Option<f32>,
    pub poses: [Transform; MAX_BODIES],
    pub copy_poses: Vec<Transform>,
    pub active_copy_count: usize,
    pub physics_ms: f32,
    /// Whole fixed ticks still owed after the last preview work batch.
    pub pending_time: Seconds,
    last_overload_warning: Option<std::time::Instant>,
}

impl Default for RigidSimulation {
    fn default() -> Self {
        Self {
            world: None,
            handles: [None; MAX_BODIES],
            descriptions: [None; MAX_BODIES],
            bullet_enabled: [false; MAX_BODIES],
            copy_handles: vec![None; MAX_COPIES],
            copy_description: None,
            copy_bullet_enabled: vec![false; MAX_COPIES],
            latched_copy_count: 0,
            latched_copy_spacing: 1.25,
            latched_copy_columns: 16,
            latched_copy_layout: CopyLayout::Grid,
            last_time: None,
            accumulator: 0.0,
            authored_time: 0.0,
            physics_time: 0.0,
            authored_samples: VecDeque::with_capacity(256),
            reset_count: None,
            poses: [Transform::default(); MAX_BODIES],
            copy_poses: vec![Transform::default(); MAX_COPIES],
            active_copy_count: 0,
            physics_ms: 0.0,
            pending_time: Seconds::ZERO,
            last_overload_warning: None,
        }
    }
}

impl RigidSimulation {
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
        let requested_copy_count = if prototype.is_some() {
            copy_count.round().clamp(0.0, MAX_COPIES as f32) as usize
        } else {
            0
        };
        let requested_copy_columns = copy_columns.round().clamp(1.0, 64.0) as usize;
        let requested_copy_layout = CopyLayout::from_scalar(layout);
        if let Some(prototype) = prototype {
            validate_copy_prototype(prototype)?;
        }
        for b in bodies.iter().flatten() {
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
        let topology_changed = bodies
            .iter()
            .zip(self.descriptions)
            .any(|(a, b)| match (a, b) {
                (Some(a), Some(b)) => a.shape != b.shape || a.transform.scale != b.transform.scale,
                (None, None) => false,
                _ => true,
            });
        let reset = self.reset_count.is_some_and(|old| old != reset_count)
            || self.last_time.is_some_and(|last| now.0 < last.0);
        let copy_topology_changed = match (prototype, self.copy_description) {
            (Some(current), Some(previous)) => {
                current.shape != previous.shape
                    || current.transform.scale != previous.transform.scale
            }
            (None, None) => false,
            _ => true,
        };
        let rebuild = self.world.is_none() || topology_changed || copy_topology_changed || reset;
        if rebuild {
            self.copy_poses.fill(Transform::default());
            let prototype_added = self.copy_description.is_none() && prototype.is_some();
            let active_copy_count = if prototype.is_none() {
                0
            } else if self.world.is_none() || reset || prototype_added {
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
            if let Some(prototype) = prototype {
                validate_copy_prototype(prototype)?;
            }
            let mut world = PhysicsWorld::new(gravity).map_err(|e| e.to_string())?;
            let mut handles = [None; MAX_BODIES];
            for (i, body) in bodies.iter().enumerate() {
                let Some(body) = body else { continue };
                let points = platonic_points(body.shape);
                let mut scaled = [[0.0; 3]; 20];
                for (dst, src) in scaled.iter_mut().zip(points) {
                    for axis in 0..3 {
                        dst[axis] = src[axis] * body.transform.scale[axis];
                    }
                }
                handles[i] = Some(
                    world
                        .add_hull(&scaled[..points.len()], body.config())
                        .map_err(|e| e.to_string())?,
                );
            }
            let mut copy_handles = vec![None; active_copy_count];
            if let Some(prototype) = prototype {
                let points = platonic_points(prototype.shape);
                let mut scaled = [[0.0; 3]; 20];
                for (dst, src) in scaled.iter_mut().zip(points) {
                    for axis in 0..3 {
                        dst[axis] = src[axis] * prototype.transform.scale[axis];
                    }
                }
                for (index, handle) in copy_handles.iter_mut().enumerate() {
                    let mut copy = prototype;
                    copy = copy_transform_for_layout(
                        copy,
                        prototype.transform.pos,
                        index,
                        active_copy_count,
                        active_copy_columns,
                        active_copy_spacing,
                        active_copy_layout,
                    );
                    *handle = Some(
                        world
                            .add_hull(&scaled[..points.len()], copy.config())
                            .map_err(|e| e.to_string())?,
                    );
                }
            }
            self.world = Some(world);
            self.handles = handles;
            self.bullet_enabled.fill(false);
            self.copy_handles.fill(None);
            self.copy_bullet_enabled.fill(false);
            self.copy_handles[..active_copy_count].copy_from_slice(&copy_handles);
            self.descriptions = bodies;
            self.copy_description = prototype;
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
            self.authored_samples.push_back(AuthoredPoseSample {
                time: 0.0,
                bodies,
                prototype,
            });
        }
        let elapsed = now.0 - self.last_time.unwrap_or(now).0;
        // Preserve all elapsed time. Preview can yield with ticks still queued.
        let elapsed_simulation = elapsed * f64::from(speed);
        self.authored_time += elapsed_simulation;
        self.record_authored_sample(self.authored_time, bodies, prototype);
        let accumulated = self.accumulator + elapsed_simulation;
        const TICK: f64 = 1.0 / 60.0;
        let due_steps = ((accumulated + 1e-9) / TICK).floor() as usize;
        let preview_budget = PREVIEW_STEP_BUDGET.with(std::cell::Cell::get);
        let steps = if speed == 0.0 && preview_budget.is_some() {
            0
        } else {
            due_steps
        };
        {
            let world = self.world.as_mut().expect("world constructed above");
            world.set_gravity(gravity).map_err(|e| e.to_string())?;
            for (i, body) in bodies.iter().enumerate() {
                let (Some(body), Some(handle)) = (body, self.handles[i]) else {
                    continue;
                };
                let old = self.descriptions[i];
                if old != Some(*body) {
                    let move_pose =
                        body.kind != 2 && old.is_none_or(|old| old.transform != body.transform);
                    world
                        .update_body(handle, body.config(), move_pose)
                        .map_err(|e| e.to_string())?;
                    if body.kind == 1 && old.is_some_and(|old| old.kind != 1) {
                        world.set_bullet(handle, false).map_err(|e| e.to_string())?;
                        self.bullet_enabled[i] = false;
                    }
                }
            }
            if let Some(prototype) = prototype {
                let old = self.copy_description;
                if old != Some(prototype) {
                    let move_pose = prototype.kind != 2
                        && old.is_some_and(|old| old.transform != prototype.transform);
                    for index in 0..self.active_copy_count {
                        let Some(handle) = self.copy_handles[index] else {
                            continue;
                        };
                        let copy = copy_transform_for_layout(
                            prototype,
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
        let physics_start = std::time::Instant::now();
        let mut completed = 0;
        for _ in 0..steps {
            self.configure_fast_bodies(bodies, prototype, gravity, TICK)?;
            let microsteps = self.animated_microsteps(bodies, prototype, TICK);
            let microstep_time = TICK / microsteps as f64;
            let solver_substeps = 4_u32.div_ceil(microsteps as u32);
            for microstep in 1..=microsteps {
                let target_time = self.physics_time + microstep_time * microstep as f64;
                let mut targets = [None; MAX_BODIES];
                for (i, body) in bodies.iter().enumerate() {
                    let Some(body) = body else { continue };
                    if body.kind == 2 {
                        targets[i] = Some(
                            self.interpolated_body(i, target_time, *body)
                                .unwrap_or(*body),
                        );
                    }
                }
                let copy_target =
                    prototype
                        .filter(|prototype| prototype.kind == 2)
                        .map(|prototype| {
                            self.interpolated_prototype(target_time, prototype)
                                .unwrap_or(prototype)
                        });
                let world = self.world.as_mut().expect("world constructed above");
                for (i, target) in targets.into_iter().enumerate() {
                    let Some(target) = target else { continue };
                    let Some(handle) = self.handles[i] else {
                        continue;
                    };
                    world
                        .set_animated_target(handle, target.config(), Seconds(microstep_time))
                        .map_err(|e| e.to_string())?;
                }
                if let Some(prototype) = copy_target {
                    for index in 0..self.active_copy_count {
                        let Some(handle) = self.copy_handles[index] else {
                            continue;
                        };
                        let target = copy_transform_for_layout(
                            prototype,
                            prototype.transform.pos,
                            index,
                            self.active_copy_count,
                            self.latched_copy_columns,
                            self.latched_copy_spacing,
                            self.latched_copy_layout,
                        );
                        world
                            .set_animated_target(handle, target.config(), Seconds(microstep_time))
                            .map_err(|e| e.to_string())?;
                    }
                }
                world
                    .step(Seconds(microstep_time), solver_substeps)
                    .map_err(|e| e.to_string())?;
            }
            completed += 1;
            self.physics_time += TICK;
            // A native tick cannot be preempted. Yield before starting another.
            if preview_budget.is_some_and(|budget| physics_start.elapsed() >= budget) {
                break;
            }
        }
        self.pending_time = Seconds((due_steps - completed) as f64 * TICK);
        self.prune_authored_samples();
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
        self.descriptions = bodies;
        for (i, body) in bodies.iter().enumerate() {
            let (Some(body), Some(handle)) = (body, self.handles[i]) else {
                self.poses[i] = Transform::default();
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
        }
        if let Some(prototype) = prototype {
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
            }
        }
        self.copy_description = prototype;
        self.physics_ms = physics_start.elapsed().as_secs_f32() * 1000.0;
        Ok(())
    }

    fn record_authored_sample(
        &mut self,
        time: f64,
        bodies: [Option<RigidBody>; MAX_BODIES],
        prototype: Option<RigidBody>,
    ) {
        const EPSILON: f64 = 1.0e-12;
        let same_time = self
            .authored_samples
            .back()
            .is_some_and(|last| (last.time - time).abs() <= EPSILON);
        if same_time {
            if self
                .authored_samples
                .back()
                .is_some_and(|last| last.bodies == bodies && last.prototype == prototype)
            {
                return;
            }
            let preserve_owed_endpoint = self.physics_time + EPSILON < time
                && self.authored_samples.len() >= 2
                && self
                    .authored_samples
                    .get(self.authored_samples.len() - 2)
                    .is_some_and(|sample| sample.time < time - EPSILON);
            if !preserve_owed_endpoint {
                *self.authored_samples.back_mut().expect("sample exists") = AuthoredPoseSample {
                    time,
                    bodies,
                    prototype,
                };
                return;
            }
        }
        self.authored_samples.push_back(AuthoredPoseSample {
            time,
            bodies,
            prototype,
        });
    }

    fn configure_fast_bodies(
        &mut self,
        bodies: [Option<RigidBody>; MAX_BODIES],
        prototype: Option<RigidBody>,
        gravity: [f32; 3],
        tick: f64,
    ) -> Result<(), String> {
        // Box3D skips bullet targets during the bullet pass. Keep slow bodies
        // non-bullet so a fast body can still sweep against them.
        let world = self.world.as_mut().expect("world constructed above");
        for (index, body) in bodies.iter().enumerate() {
            let (Some(body), Some(handle)) = (body, self.handles[index]) else {
                continue;
            };
            if body.kind != 1 {
                self.bullet_enabled[index] = false;
                continue;
            }
            let velocity = world.linear_velocity(handle).map_err(|e| e.to_string())?;
            let enabled = needs_bullet(*body, velocity, gravity, tick);
            if self.bullet_enabled[index] != enabled {
                world
                    .set_bullet(handle, enabled)
                    .map_err(|e| e.to_string())?;
                self.bullet_enabled[index] = enabled;
            }
        }
        if let Some(prototype) = prototype.filter(|body| body.kind == 1) {
            for index in 0..self.active_copy_count {
                let Some(handle) = self.copy_handles[index] else {
                    continue;
                };
                let velocity = world.linear_velocity(handle).map_err(|e| e.to_string())?;
                let enabled = needs_bullet(prototype, velocity, gravity, tick);
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
        Ok(())
    }

    fn animated_microsteps(
        &self,
        bodies: [Option<RigidBody>; MAX_BODIES],
        prototype: Option<RigidBody>,
        tick: f64,
    ) -> usize {
        // Box3D bullet CCD does not sweep Animated motion. Smaller outer
        // steps put fast moving/rotating colliders into contact with Dynamics.
        const MAX_MICROSTEPS: usize = 8;
        let dynamic_extent = bodies
            .iter()
            .flatten()
            .filter(|body| body.kind == 1)
            .map(|body| {
                body.transform
                    .scale
                    .into_iter()
                    .fold(f32::INFINITY, f32::min)
            })
            .chain(
                prototype
                    .filter(|body| body.kind == 1 && self.active_copy_count > 0)
                    .map(|body| {
                        body.transform
                            .scale
                            .into_iter()
                            .fold(f32::INFINITY, f32::min)
                    }),
            )
            .fold(f32::INFINITY, f32::min);
        if !dynamic_extent.is_finite() {
            return 1;
        }
        let start_time = self.physics_time;
        let end_time = start_time + tick;
        let mut travel: f32 = 0.0;
        for (index, body) in bodies.iter().enumerate() {
            let Some(body) = *body else { continue };
            if body.kind != 2 {
                continue;
            }
            let start = self
                .interpolated_body(index, start_time, body)
                .unwrap_or(body);
            let end = self
                .interpolated_body(index, end_time, body)
                .unwrap_or(body);
            travel = travel.max(animated_sweep_distance(start, end));
        }
        if let Some(body) = prototype.filter(|body| body.kind == 2 && self.active_copy_count > 0) {
            let start = self
                .interpolated_prototype(start_time, body)
                .unwrap_or(body);
            let end = self.interpolated_prototype(end_time, body).unwrap_or(body);
            travel = travel.max(animated_sweep_distance(start, end));
        }
        let safe_step = (dynamic_extent * 0.5).max(0.001);
        (travel / safe_step)
            .ceil()
            .clamp(1.0, MAX_MICROSTEPS as f32) as usize
    }

    fn interpolated_body(
        &self,
        index: usize,
        time: f64,
        mut current: RigidBody,
    ) -> Option<RigidBody> {
        let mut previous = self.authored_samples.front()?.bodies[index]?;
        let mut previous_time = self.authored_samples.front()?.time;
        for sample in self.authored_samples.iter().skip(1) {
            let next = sample.bodies[index]?;
            if sample.time >= time {
                let alpha = interpolation_alpha(previous_time, sample.time, time);
                let pose = interpolate_body(previous, next, alpha).transform;
                current.transform.pos = pose.pos;
                current.transform.rot_euler = pose.rot_euler;
                return Some(current);
            }
            previous = next;
            previous_time = sample.time;
        }
        current.transform.pos = previous.transform.pos;
        current.transform.rot_euler = previous.transform.rot_euler;
        Some(current)
    }

    fn interpolated_prototype(&self, time: f64, mut current: RigidBody) -> Option<RigidBody> {
        let first = self.authored_samples.front()?.prototype?;
        let mut previous = first;
        let mut previous_time = self.authored_samples.front()?.time;
        for sample in self.authored_samples.iter().skip(1) {
            let next = sample.prototype?;
            if sample.time >= time {
                let alpha = interpolation_alpha(previous_time, sample.time, time);
                let pose = interpolate_body(previous, next, alpha).transform;
                current.transform.pos = pose.pos;
                current.transform.rot_euler = pose.rot_euler;
                return Some(current);
            }
            previous = next;
            previous_time = sample.time;
        }
        current.transform.pos = previous.transform.pos;
        current.transform.rot_euler = previous.transform.rot_euler;
        Some(current)
    }

    fn prune_authored_samples(&mut self) {
        while self.authored_samples.len() > 1
            && self.authored_samples[1].time <= self.physics_time + 1.0e-12
        {
            self.authored_samples.pop_front();
        }
    }
}

fn interpolation_alpha(previous_time: f64, next_time: f64, time: f64) -> f32 {
    if next_time <= previous_time {
        1.0
    } else {
        ((time - previous_time) / (next_time - previous_time)).clamp(0.0, 1.0) as f32
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

fn animated_sweep_distance(start: RigidBody, end: RigidBody) -> f32 {
    let mut linear_squared = 0.0;
    let mut angular = 0.0;
    for axis in 0..3 {
        let delta = end.transform.pos[axis] - start.transform.pos[axis];
        linear_squared += delta * delta;
        angular += (end.transform.rot_euler[axis] - start.transform.rot_euler[axis]).abs();
    }
    let radius = start.transform.scale.into_iter().fold(0.0, f32::max);
    linear_squared.sqrt() + angular * radius
}

fn needs_bullet(body: RigidBody, velocity: [f32; 3], gravity: [f32; 3], tick: f64) -> bool {
    let speed = velocity.into_iter().map(|v| v * v).sum::<f32>().sqrt();
    let acceleration = gravity.into_iter().map(|v| v * v).sum::<f32>().sqrt();
    let tick = tick as f32;
    let predicted_travel = speed * tick + 0.5 * acceleration * tick * tick;
    let extent = body
        .transform
        .scale
        .into_iter()
        .fold(f32::INFINITY, f32::min);
    predicted_travel > extent * 0.5
}

fn validate_copy_prototype(prototype: RigidBody) -> Result<(), String> {
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
        let mut bodies = [None; MAX_BODIES];
        bodies[0] = Some(body(position));
        bodies
    }

    #[test]
    fn fixed_tick_results_are_frame_partition_invariant() {
        let bodies = one_body([0.0, 4.0, 0.0]);
        let mut per_frame = RigidSimulation::default();
        let mut partitioned = RigidSimulation::default();
        per_frame
            .advance(bodies, GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        partitioned
            .advance(bodies, GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();

        for frame in 1..=60 {
            per_frame
                .advance(bodies, GRAVITY, Seconds(frame as f64 * FRAME), 1.0, 0.0)
                .unwrap();
        }
        for half_frame in 1..=120 {
            partitioned
                .advance(
                    bodies,
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
    fn zero_speed_does_not_advance_until_transport_resumes() {
        let bodies = one_body([0.0, 4.0, 0.0]);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies, GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        let authored = simulation.poses[0].pos;
        simulation
            .advance(bodies, GRAVITY, Seconds(0.5), 0.0, 0.0)
            .unwrap();
        assert_eq!(simulation.poses[0].pos, authored);
        simulation
            .advance(bodies, GRAVITY, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        assert!(simulation.poses[0].pos[1] < authored[1] - 0.1);
    }

    #[test]
    fn animated_body_pushes_a_dynamic_body_instead_of_teleporting_through_it() {
        let mut bodies = [None; MAX_BODIES];
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
            .advance(bodies, [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();

        for frame in 1..=30 {
            bodies[0].as_mut().unwrap().transform.pos[0] = -1.3 + frame as f32 * 0.05;
            simulation
                .advance(bodies, [0.0; 3], Seconds(frame as f64 * FRAME), 1.0, 0.0)
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
        let mut bodies = [None; MAX_BODIES];
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
            .advance(bodies, [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();

        for frame in 1..=60 {
            bodies[0].as_mut().unwrap().transform.rot_euler[1] =
                frame as f32 * std::f32::consts::FRAC_PI_2 / 60.0;
            simulation
                .advance(bodies, [0.0; 3], Seconds(frame as f64 * FRAME), 1.0, 0.0)
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
            .advance(bodies, GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        simulation
            .advance(bodies, GRAVITY, Seconds(0.75), 1.0, 0.0)
            .unwrap();
        assert!(simulation.poses[0].pos[1] < 4.0);

        simulation
            .advance(bodies, GRAVITY, Seconds(0.75), 1.0, 1.0)
            .unwrap();
        assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
        simulation
            .advance(bodies, GRAVITY, Seconds(0.5), 1.0, 1.0)
            .unwrap();
        assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
    }

    #[test]
    fn material_edit_preserves_the_current_falling_pose() {
        let bodies = one_body([0.0, 4.0, 0.0]);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies, GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        simulation
            .advance(bodies, GRAVITY, Seconds(0.5), 1.0, 0.0)
            .unwrap();
        let falling_pose = simulation.poses[0].pos;

        let mut edited = bodies;
        let mut edited_body = edited[0].unwrap();
        edited_body.friction = 0.9;
        edited_body.mass = 2.0;
        edited_body.bounce = 0.4;
        edited[0] = Some(edited_body);
        simulation
            .advance(edited, GRAVITY, Seconds(0.5), 1.0, 0.0)
            .unwrap();
        assert!((simulation.poses[0].pos[1] - falling_pose[1]).abs() < 1.0e-5);

        simulation
            .advance(edited, GRAVITY, Seconds(0.75), 1.0, 0.0)
            .unwrap();
        assert!(simulation.poses[0].pos[1] < falling_pose[1] - 0.05);
    }

    #[test]
    fn all_scaled_platonic_hulls_settle_on_the_ground() {
        let mut bodies = [None; MAX_BODIES];
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
            .advance(bodies, GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        // A dodecahedron can still roll between faces at four seconds.
        // Give every shape time to sleep before measuring stable contact.
        for frame in 1..=720 {
            simulation
                .advance(bodies, GRAVITY, Seconds(frame as f64 * FRAME), 1.0, 0.0)
                .unwrap();
        }
        let settled = simulation.poses;
        simulation
            .advance(bodies, GRAVITY, Seconds(12.5), 1.0, 0.0)
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
            .advance(bodies, GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        for frame in 0..=181 {
            regular
                .advance(bodies, GRAVITY, Seconds(frame as f64 * FRAME), 1.0, 0.0)
                .unwrap();
        }
        caught_up
            .advance(bodies, GRAVITY, Seconds(180.0 * FRAME), 1.0, 0.0)
            .unwrap();
        caught_up
            .advance(bodies, GRAVITY, Seconds(181.0 * FRAME), 1.0, 0.0)
            .unwrap();
        assert_eq!(caught_up.poses, regular.poses);
        caught_up
            .advance(bodies, GRAVITY, Seconds(181.0 * FRAME), 1.0, 1.0)
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
            sim.advance(bodies, GRAVITY, Seconds::ZERO, 1.0, 0.0)
                .unwrap();
        }
        let now = Seconds(3.0 + FRAME / 2.0);
        preview.advance(bodies, GRAVITY, now, 1.0, 0.0).unwrap();
        assert!((preview.pending_time.0 - 179.0 * FRAME).abs() < 1e-9);
        let held = preview.poses;
        preview.advance(bodies, GRAVITY, now, 0.0, 0.0).unwrap();
        assert_eq!(preview.poses, held);
        for _ in 1..180 {
            preview.advance(bodies, GRAVITY, now, 1.0, 0.0).unwrap();
        }
        assert_eq!(preview.pending_time, Seconds::ZERO);
        assert!((preview.accumulator - FRAME / 2.0).abs() < 1e-9);
        {
            let _export = PhysicsStepScope::for_render(true);
            export.advance(bodies, GRAVITY, now, 1.0, 0.0).unwrap();
        }
        assert_eq!(
            preview.poses, export.poses,
            "chunking must not change collision results"
        );
        let next = Seconds(3.0 + FRAME);
        preview.advance(bodies, GRAVITY, next, 1.0, 0.0).unwrap();
        export.advance(bodies, GRAVITY, next, 1.0, 0.0).unwrap();
        assert_eq!(preview.poses, export.poses);
    }

    #[test]
    fn animated_pose_timeline_matches_export_for_moving_and_rotating_contacts() {
        let _preview_scope =
            PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);

        let mut moving = [None; MAX_BODIES];
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
            .advance(moving, [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        moving_export
            .advance(moving, [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        moving[0].as_mut().unwrap().transform.pos[0] = -0.55;
        moving_preview
            .advance(moving, [0.0; 3], Seconds(0.5), 1.0, 0.0)
            .unwrap();
        {
            let _export_scope = PhysicsStepScope::for_render(true);
            moving_export
                .advance(moving, [0.0; 3], Seconds(0.5), 1.0, 0.0)
                .unwrap();
        }
        moving[0].as_mut().unwrap().transform.pos[0] = 0.2;
        moving_preview
            .advance(moving, [0.0; 3], Seconds(1.0), 1.0, 0.0)
            .unwrap();
        {
            let _export_scope = PhysicsStepScope::for_render(true);
            moving_export
                .advance(moving, [0.0; 3], Seconds(1.0), 1.0, 0.0)
                .unwrap();
        }
        while moving_preview.pending_time.0 > 0.0 {
            moving_preview
                .advance(moving, [0.0; 3], Seconds(1.0), 1.0, 0.0)
                .unwrap();
        }
        assert_eq!(moving_preview.poses, moving_export.poses);

        let mut rotating = [None; MAX_BODIES];
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
            .advance(rotating, [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        rotating_export
            .advance(rotating, [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        rotating[0].as_mut().unwrap().transform.rot_euler[1] = std::f32::consts::FRAC_PI_4;
        rotating_preview
            .advance(rotating, [0.0; 3], Seconds(0.5), 1.0, 0.0)
            .unwrap();
        {
            let _export_scope = PhysicsStepScope::for_render(true);
            rotating_export
                .advance(rotating, [0.0; 3], Seconds(0.5), 1.0, 0.0)
                .unwrap();
        }
        rotating[0].as_mut().unwrap().transform.rot_euler[1] = std::f32::consts::FRAC_PI_2;
        rotating_preview
            .advance(rotating, [0.0; 3], Seconds(1.0), 1.0, 0.0)
            .unwrap();
        {
            let _export_scope = PhysicsStepScope::for_render(true);
            rotating_export
                .advance(rotating, [0.0; 3], Seconds(1.0), 1.0, 0.0)
                .unwrap();
        }
        while rotating_preview.pending_time.0 > 0.0 {
            rotating_preview
                .advance(rotating, [0.0; 3], Seconds(1.0), 1.0, 0.0)
                .unwrap();
        }
        assert_eq!(rotating_preview.poses, rotating_export.poses);
    }

    #[test]
    fn paused_authored_edit_keeps_pose_sample_needed_by_preview_backlog() {
        let _scope = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
        let mut bodies = [None; MAX_BODIES];
        let mut animated = body([-2.0, 0.0, 0.0]);
        animated.kind = 2;
        bodies[0] = Some(animated);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies, [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        bodies[0].as_mut().unwrap().transform.pos[0] = -0.5;
        simulation
            .advance(bodies, [0.0; 3], Seconds(0.5), 1.0, 0.0)
            .unwrap();
        bodies[0].as_mut().unwrap().transform.pos[0] = 2.0;
        simulation
            .advance(bodies, [0.0; 3], Seconds(0.5), 1.0, 0.0)
            .unwrap();
        let owed_pose = simulation
            .interpolated_body(0, 0.25, bodies[0].unwrap())
            .unwrap();
        assert!((owed_pose.transform.pos[0] + 1.25).abs() < 1.0e-4);
    }

    #[test]
    fn fast_animated_sweep_uses_outer_steps_to_reach_dynamic_body() {
        let mut bodies = [None; MAX_BODIES];
        let mut animated = body([-2.0, 0.0, 0.0]);
        animated.kind = 2;
        bodies[0] = Some(animated);
        bodies[1] = Some(body([0.0, 0.0, 0.0]));
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies, [0.0; 3], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        bodies[0].as_mut().unwrap().transform.pos[0] = 2.0;
        simulation
            .advance(bodies, [0.0; 3], Seconds(FRAME), 1.0, 0.0)
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
    fn fast_dynamic_body_uses_bullet_collision_against_animated_body() {
        let mut bodies = [None; MAX_BODIES];
        bodies[0] = Some(body([0.0, 3.0, 0.0]));
        let mut animated = body([0.0, 0.0, 0.0]);
        animated.kind = 2;
        bodies[1] = Some(animated);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies, [0.0, -20_000.0, 0.0], Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        simulation
            .advance(bodies, [0.0, -20_000.0, 0.0], Seconds(FRAME), 1.0, 0.0)
            .unwrap();
        assert!(
            simulation.poses[0].pos[1] > 0.9,
            "fast dynamic body passed through the animated body: {:?}",
            simulation.poses[0].pos
        );
    }

    #[test]
    fn preview_backlog_can_be_completed_by_export_or_cleared_by_reset() {
        let _scope = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
        let bodies = one_body([0.0, 4.0, 0.0]);
        let mut sim = RigidSimulation::default();
        sim.advance(bodies, GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        sim.advance(bodies, GRAVITY, Seconds(3.0), 1.0, 0.0)
            .unwrap();
        assert!(sim.pending_time.0 > 2.9);
        {
            let _export = PhysicsStepScope::for_render(true);
            sim.advance(bodies, GRAVITY, Seconds(3.0), 0.0, 0.0)
                .unwrap();
        }
        assert_eq!(sim.pending_time, Seconds::ZERO);
        sim.advance(bodies, GRAVITY, Seconds(6.0), 1.0, 0.0)
            .unwrap();
        assert!(sim.pending_time.0 > 2.9);
        sim.advance(bodies, GRAVITY, Seconds(6.0), 1.0, 1.0)
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
    fn copies_are_reset_latched_and_shrink_tail_is_inactive() {
        let bodies = [None; MAX_BODIES];
        let prototype = body([0.0, 4.0, 0.0]);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance_with_copies(
                bodies,
                Some(prototype),
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
                bodies,
                Some(prototype),
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
                bodies,
                Some(prototype),
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
                [None; MAX_BODIES],
                Some(prototype),
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
                [None; MAX_BODIES],
                Some(prototype),
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
                [None; MAX_BODIES],
                Some(prototype),
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
                [None; MAX_BODIES],
                Some(prototype),
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
            prototype,
            prototype.transform.pos,
            0,
            256,
            16,
            1.85,
            CopyLayout::Pile,
        );
        let second = copy_transform_for_layout(
            prototype,
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
                prototype,
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
        let mut bodies = [None; MAX_BODIES];
        let mut ground = body([0.0, -0.28867513, 0.0]);
        ground.kind = 0;
        ground.transform.scale = [20.0, 0.5, 20.0];
        bodies[0] = Some(ground);
        let mut prototype = body([0.0, 3.0, 0.0]);
        prototype.transform.scale = [0.5; 3];
        let mut sim = RigidSimulation::default();
        for frame in 0..=240 {
            sim.advance_with_copies(
                bodies,
                Some(prototype),
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
            bodies,
            Some(prototype),
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
            bodies,
            Some(prototype),
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
            bodies,
            Some(prototype),
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
                        [None; MAX_BODIES],
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
