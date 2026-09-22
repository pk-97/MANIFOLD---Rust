//! Value descriptions on graph wires; native simulation ownership stays in the world node.
use manifold_core::Seconds;
use manifold_physics::{BodyConfig, BodyHandle, BodyKind, PhysicsWorld};

use super::transform::Transform;
use crate::generators::platonic_geometry::platonic_points;

pub const MAX_BODIES: usize = 16;
pub const MAX_COPIES: usize = 4096;
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
    copy_handles: Vec<Option<BodyHandle>>,
    copy_description: Option<RigidBody>,
    latched_copy_count: usize,
    latched_copy_spacing: f32,
    latched_copy_columns: usize,
    latched_copy_layout: CopyLayout,
    last_time: Option<Seconds>,
    accumulator: f64,
    reset_count: Option<f32>,
    pub poses: [Transform; MAX_BODIES],
    pub copy_poses: Vec<Transform>,
    pub active_copy_count: usize,
    pub physics_ms: f32,
}

impl Default for RigidSimulation {
    fn default() -> Self {
        Self {
            world: None,
            handles: [None; MAX_BODIES],
            descriptions: [None; MAX_BODIES],
            copy_handles: vec![None; MAX_COPIES],
            copy_description: None,
            latched_copy_count: 0,
            latched_copy_spacing: 1.25,
            latched_copy_columns: 16,
            latched_copy_layout: CopyLayout::Grid,
            last_time: None,
            accumulator: 0.0,
            reset_count: None,
            poses: [Transform::default(); MAX_BODIES],
            copy_poses: vec![Transform::default(); MAX_COPIES],
            active_copy_count: 0,
            physics_ms: 0.0,
        }
    }
}

impl RigidSimulation {
    /// 120 Hz outer ticks, four Box3D substeps each. A paused transport contributes no time.
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
            self.copy_handles.fill(None);
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
        }
        let elapsed = now.0 - self.last_time.unwrap_or(now).0;
        // Never silently drop simulation time when the live/export caller overruns the bounded step budget.
        let accumulated = self.accumulator + elapsed * f64::from(speed);
        const TICK: f64 = 1.0 / 120.0;
        let steps = ((accumulated + 1e-9) / TICK).floor() as usize;
        if steps > 128 {
            return Err("Physics step budget exceeded; reset the simulation to resume".into());
        }
        let world = self.world.as_mut().expect("world constructed above");
        world.set_gravity(gravity).map_err(|e| e.to_string())?;
        for (i, body) in bodies.iter().enumerate() {
            let (Some(body), Some(handle)) = (body, self.handles[i]) else {
                continue;
            };
            let old = self.descriptions[i];
            if old != Some(*body) {
                let move_pose = old.is_none_or(|old| old.transform != body.transform);
                world
                    .update_body(handle, body.config(), move_pose)
                    .map_err(|e| e.to_string())?;
            }
        }
        if let Some(prototype) = prototype {
            let old = self.copy_description;
            if old != Some(prototype) {
                let move_pose = old.is_some_and(|old| old.transform != prototype.transform);
                for index in 0..self.active_copy_count {
                    let Some(handle) = self.copy_handles[index] else {
                        continue;
                    };
                    let mut copy = prototype;
                    copy = copy_transform_for_layout(
                        copy,
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
                }
            }
        }
        let physics_start = std::time::Instant::now();
        for _ in 0..steps {
            world.step(Seconds(TICK), 4).map_err(|e| e.to_string())?;
        }
        self.accumulator = (accumulated - steps as f64 * TICK).max(0.0);
        self.last_time = Some(now);
        self.reset_count = Some(reset_count);
        self.descriptions = bodies;
        for (i, body) in bodies.iter().enumerate() {
            let (Some(body), Some(handle)) = (body, self.handles[i]) else {
                self.poses[i] = Transform::default();
                continue;
            };
            let pose = world.pose(handle).map_err(|e| e.to_string())?;
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
                let pose = world.pose(handle).map_err(|e| e.to_string())?;
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
    fn overrun_errors_and_a_reset_recovers() {
        let bodies = one_body([0.0, 4.0, 0.0]);
        let mut simulation = RigidSimulation::default();
        simulation
            .advance(bodies, GRAVITY, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        let error = simulation
            .advance(bodies, GRAVITY, Seconds(2.0), 1.0, 0.0)
            .unwrap_err();
        assert!(error.contains("step budget"));

        simulation
            .advance(bodies, GRAVITY, Seconds(2.0), 1.0, 1.0)
            .unwrap();
        assert_eq!(simulation.poses[0].pos, [0.0, 4.0, 0.0]);
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
    fn bulk_boxes_share_floor_contacts_and_reset_recovers_overrun() {
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
        assert!(
            sim.advance_with_copies(
                bodies,
                Some(prototype),
                64.0,
                1.25,
                4.0,
                GRAVITY,
                Seconds(7.0),
                1.0,
                0.0
            )
            .is_err()
        );
        sim.advance_with_copies(
            bodies,
            Some(prototype),
            64.0,
            1.25,
            4.0,
            GRAVITY,
            Seconds(7.0),
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
