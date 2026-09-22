//! Value descriptions on graph wires; native simulation ownership stays in the world node.
use manifold_core::Seconds;
use manifold_physics::{BodyConfig, BodyHandle, BodyKind, PhysicsWorld};

use super::transform::Transform;
use crate::generators::platonic_geometry::platonic_points;

pub const MAX_BODIES: usize = 16;
pub const BODY_PORTS: [&str; MAX_BODIES] = [
    "body_0", "body_1", "body_2", "body_3", "body_4", "body_5", "body_6", "body_7", "body_8",
    "body_9", "body_10", "body_11", "body_12", "body_13", "body_14", "body_15",
];
pub const POSE_PORTS: [&str; MAX_BODIES] = [
    "pose_0", "pose_1", "pose_2", "pose_3", "pose_4", "pose_5", "pose_6", "pose_7", "pose_8",
    "pose_9", "pose_10", "pose_11", "pose_12", "pose_13", "pose_14", "pose_15",
];

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

#[derive(Default)]
pub struct RigidSimulation {
    world: Option<PhysicsWorld>,
    handles: [Option<BodyHandle>; MAX_BODIES],
    descriptions: [Option<RigidBody>; MAX_BODIES],
    last_time: Option<Seconds>,
    accumulator: f64,
    reset_count: Option<f32>,
    pub poses: [Transform; MAX_BODIES],
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
        if !now.0.is_finite()
            || !speed.is_finite()
            || !(0.0..=4.0).contains(&speed)
            || !reset_count.is_finite()
            || gravity.iter().any(|v| !v.is_finite())
        {
            return Err("Physics: non-finite clock/control or speed outside 0–4".into());
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
        if self.world.is_none() || topology_changed || reset {
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
            self.world = Some(world);
            self.handles = handles;
            self.descriptions = bodies;
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRAVITY: [f32; 3] = [0.0, -9.8, 0.0];
    const FRAME: f64 = 1.0 / 60.0;

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
}
