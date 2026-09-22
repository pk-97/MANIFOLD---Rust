//! A small, owned Rust interface to the pinned Box3D C library.
//!
//! Box3D has process-global world storage which is not synchronized by the
//! library. Every native call is therefore serialized through one private
//! mutex. `PhysicsWorld` owns its world and handles exclusively, and carries a
//! non-`Sync` marker so it may move between threads but cannot be shared there.

pub use manifold_foundation::Seconds;
use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

mod ffi {
    unsafe extern "C" {
        pub fn manifold_box3d_world_create(gx: f32, gy: f32, gz: f32) -> u32;
        pub fn manifold_box3d_world_destroy(world: u32);
        pub fn manifold_box3d_world_set_gravity(world: u32, gx: f32, gy: f32, gz: f32);
        pub fn manifold_box3d_world_step(world: u32, dt: f32, substeps: u32);
        pub fn manifold_box3d_body_create(
            world: u32,
            points: *const f32,
            point_count: i32,
            kind: i32,
            px: f32,
            py: f32,
            pz: f32,
            qx: f32,
            qy: f32,
            qz: f32,
            qw: f32,
            mass: f32,
            friction: f32,
            restitution: f32,
            hull_out: *mut usize,
        ) -> u64;
        pub fn manifold_box3d_body_update(
            body: u64,
            kind: i32,
            px: f32,
            py: f32,
            pz: f32,
            qx: f32,
            qy: f32,
            qz: f32,
            qw: f32,
            mass: f32,
            friction: f32,
            restitution: f32,
            move_pose: i32,
        ) -> i32;
        pub fn manifold_box3d_body_set_target(
            body: u64,
            px: f32,
            py: f32,
            pz: f32,
            qx: f32,
            qy: f32,
            qz: f32,
            qw: f32,
            time_step: f32,
        ) -> i32;
        pub fn manifold_box3d_body_pose(body: u64, position: *mut f32, rotation: *mut f32) -> i32;
        pub fn manifold_box3d_destroy_hull(hull: usize);
    }
}

static NATIVE_LOCK: Mutex<()> = Mutex::new(());
static NEXT_WORLD_PROVENANCE: AtomicU64 = AtomicU64::new(1);

fn native_lock() -> MutexGuard<'static, ()> {
    NATIVE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Errors returned by the owned physics wrapper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhysicsError {
    InvalidInput(&'static str),
    NativeAllocation,
    InvalidHandle,
    NativeFailure,
}

impl fmt::Display for PhysicsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) => write!(formatter, "invalid physics input: {message}"),
            Self::NativeAllocation => formatter.write_str("Box3D allocation failed"),
            Self::InvalidHandle => formatter.write_str("body handle does not belong to this world"),
            Self::NativeFailure => formatter.write_str("Box3D rejected the requested operation"),
        }
    }
}

impl std::error::Error for PhysicsError {}

/// The simulation behaviour of a body.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BodyKind {
    Fixed,
    #[default]
    Dynamic,
    Animated,
}

impl BodyKind {
    fn native_value(self) -> i32 {
        match self {
            Self::Fixed => 0,
            Self::Dynamic => 1,
            Self::Animated => 2,
        }
    }
}

/// Parameters used to create or update a body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyConfig {
    pub kind: BodyKind,
    pub position: [f32; 3],
    /// Quaternion in `[x, y, z, w]` order. Inputs are normalized at the API boundary.
    pub rotation: [f32; 4],
    pub mass: f32,
    pub friction: f32,
    pub restitution: f32,
}

impl Default for BodyConfig {
    fn default() -> Self {
        Self {
            kind: BodyKind::Dynamic,
            position: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            mass: 1.0,
            friction: 0.5,
            restitution: 0.0,
        }
    }
}

/// A body's world-space pose.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyPose {
    pub position: [f32; 3],
    pub rotation: [f32; 4],
}

/// An opaque body reference tied to the world that created it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BodyHandle {
    provenance: u64,
    index: u32,
}

struct BodyRecord {
    native: u64,
    owned_hull: usize,
}

/// An exclusively owned Box3D simulation world.
pub struct PhysicsWorld {
    native: u32,
    provenance: u64,
    bodies: Vec<BodyRecord>,
    // Cell is Send but not Sync, matching exclusive world ownership.
    _not_sync: PhantomData<Cell<()>>,
}

impl PhysicsWorld {
    pub fn new(gravity: [f32; 3]) -> Result<Self, PhysicsError> {
        validate_vec3(gravity, "gravity")?;
        let _lock = native_lock();
        let native =
            unsafe { ffi::manifold_box3d_world_create(gravity[0], gravity[1], gravity[2]) };
        if native == 0 {
            return Err(PhysicsError::NativeAllocation);
        }
        let provenance = NEXT_WORLD_PROVENANCE.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            native,
            provenance: if provenance == 0 { 1 } else { provenance },
            bodies: Vec::new(),
            _not_sync: PhantomData,
        })
    }

    pub fn add_hull(
        &mut self,
        points: &[[f32; 3]],
        config: BodyConfig,
    ) -> Result<BodyHandle, PhysicsError> {
        let config = validate_config(config)?;
        if points.len() < 4 || points.len() > 255 {
            return Err(PhysicsError::InvalidInput(
                "hull needs between 4 and 255 points",
            ));
        }
        if points
            .iter()
            .any(|point| !point.iter().all(|value| value.is_finite()))
        {
            return Err(PhysicsError::InvalidInput("hull points must be finite"));
        }
        let index = self.bodies.len();
        if index > u32::MAX as usize {
            return Err(PhysicsError::NativeAllocation);
        }

        let mut owned_hull = 0usize;
        let _lock = native_lock();
        let native = unsafe {
            ffi::manifold_box3d_body_create(
                self.native,
                points.as_ptr().cast::<f32>(),
                points.len() as i32,
                config.kind.native_value(),
                config.position[0],
                config.position[1],
                config.position[2],
                config.rotation[0],
                config.rotation[1],
                config.rotation[2],
                config.rotation[3],
                config.mass,
                config.friction,
                config.restitution,
                &mut owned_hull,
            )
        };
        if native == 0 || owned_hull == 0 {
            return Err(PhysicsError::NativeAllocation);
        }

        self.bodies.push(BodyRecord { native, owned_hull });
        Ok(BodyHandle {
            provenance: self.provenance,
            index: index as u32,
        })
    }

    pub fn set_gravity(&mut self, gravity: [f32; 3]) -> Result<(), PhysicsError> {
        validate_vec3(gravity, "gravity")?;
        let _lock = native_lock();
        unsafe {
            ffi::manifold_box3d_world_set_gravity(self.native, gravity[0], gravity[1], gravity[2]);
        }
        Ok(())
    }

    pub fn step(&mut self, dt: Seconds, substeps: u32) -> Result<(), PhysicsError> {
        let dt_f32 = dt.0 as f32;
        if !dt.0.is_finite() || dt.0 <= 0.0 || !dt_f32.is_finite() || dt_f32 <= 0.0 {
            return Err(PhysicsError::InvalidInput("dt must be finite and positive"));
        }
        if substeps == 0 || substeps > i32::MAX as u32 {
            return Err(PhysicsError::InvalidInput("substeps must be positive"));
        }
        let _lock = native_lock();
        unsafe { ffi::manifold_box3d_world_step(self.native, dt_f32, substeps) };
        Ok(())
    }

    pub fn update_body(
        &mut self,
        handle: BodyHandle,
        config: BodyConfig,
        move_pose: bool,
    ) -> Result<(), PhysicsError> {
        let config = validate_config(config)?;
        let native = self.native_body(handle)?;
        let _lock = native_lock();
        let result = unsafe {
            ffi::manifold_box3d_body_update(
                native,
                config.kind.native_value(),
                config.position[0],
                config.position[1],
                config.position[2],
                config.rotation[0],
                config.rotation[1],
                config.rotation[2],
                config.rotation[3],
                config.mass,
                config.friction,
                config.restitution,
                i32::from(move_pose),
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(PhysicsError::NativeFailure)
        }
    }

    /// Move an animated body through the solver over the supplied simulation time.
    /// This gives contacts the body's linear and angular velocity; `update_body`
    /// with `move_pose = true` is a teleport for direct edits and resets.
    pub fn set_animated_target(
        &mut self,
        handle: BodyHandle,
        config: BodyConfig,
        time_step: Seconds,
    ) -> Result<(), PhysicsError> {
        let config = validate_config(config)?;
        if config.kind != BodyKind::Animated {
            return Err(PhysicsError::InvalidInput(
                "target requires an animated body",
            ));
        }
        let dt = time_step.0 as f32;
        if !time_step.0.is_finite() || dt <= 0.0 || !dt.is_finite() {
            return Err(PhysicsError::InvalidInput(
                "target time must be finite and positive",
            ));
        }
        let native = self.native_body(handle)?;
        let _lock = native_lock();
        let result = unsafe {
            ffi::manifold_box3d_body_set_target(
                native,
                config.position[0],
                config.position[1],
                config.position[2],
                config.rotation[0],
                config.rotation[1],
                config.rotation[2],
                config.rotation[3],
                dt,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(PhysicsError::NativeFailure)
        }
    }

    pub fn pose(&self, handle: BodyHandle) -> Result<BodyPose, PhysicsError> {
        let native = self.native_body(handle)?;
        let mut position = [0.0; 3];
        let mut rotation = [0.0; 4];
        let _lock = native_lock();
        let result = unsafe {
            ffi::manifold_box3d_body_pose(native, position.as_mut_ptr(), rotation.as_mut_ptr())
        };
        if result != 0 {
            return Err(PhysicsError::NativeFailure);
        }
        Ok(BodyPose { position, rotation })
    }

    fn native_body(&self, handle: BodyHandle) -> Result<u64, PhysicsError> {
        if handle.provenance != self.provenance {
            return Err(PhysicsError::InvalidHandle);
        }
        self.bodies
            .get(handle.index as usize)
            .map(|body| body.native)
            .ok_or(PhysicsError::InvalidHandle)
    }
}

impl Drop for PhysicsWorld {
    fn drop(&mut self) {
        let _lock = native_lock();
        unsafe { ffi::manifold_box3d_world_destroy(self.native) };
        for body in &self.bodies {
            unsafe { ffi::manifold_box3d_destroy_hull(body.owned_hull) };
        }
    }
}

fn validate_vec3(value: [f32; 3], name: &'static str) -> Result<(), PhysicsError> {
    if value.iter().all(|component| component.is_finite()) {
        Ok(())
    } else {
        Err(PhysicsError::InvalidInput(name))
    }
}

fn validate_config(mut config: BodyConfig) -> Result<BodyConfig, PhysicsError> {
    validate_vec3(config.position, "position")?;
    if !config.mass.is_finite() || config.mass < 0.0 {
        return Err(PhysicsError::InvalidInput(
            "mass must be finite and non-negative",
        ));
    }
    if config.kind == BodyKind::Dynamic && config.mass <= 0.0 {
        return Err(PhysicsError::InvalidInput("dynamic mass must be positive"));
    }
    if !config.friction.is_finite() || config.friction < 0.0 {
        return Err(PhysicsError::InvalidInput(
            "friction must be finite and non-negative",
        ));
    }
    if !config.restitution.is_finite() || config.restitution < 0.0 {
        return Err(PhysicsError::InvalidInput(
            "restitution must be finite and non-negative",
        ));
    }
    if !config
        .rotation
        .iter()
        .all(|component| component.is_finite())
    {
        return Err(PhysicsError::InvalidInput("rotation must be finite"));
    }
    let length = config
        .rotation
        .iter()
        .map(|component| component * component)
        .sum::<f32>()
        .sqrt();
    if !length.is_finite() || length < 1.0e-6 {
        return Err(PhysicsError::InvalidInput("rotation must be non-zero"));
    }
    for component in &mut config.rotation {
        *component /= length;
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(height: f32) -> Vec<[f32; 3]> {
        vec![
            [-0.5, -height, -0.5],
            [0.5, -height, -0.5],
            [0.5, height, -0.5],
            [-0.5, height, -0.5],
            [-0.5, -height, 0.5],
            [0.5, -height, 0.5],
            [0.5, height, 0.5],
            [-0.5, height, 0.5],
        ]
    }

    #[test]
    fn free_fall_is_close_to_analytic_solution() {
        let mut world = PhysicsWorld::new([0.0, -9.8, 0.0]).unwrap();
        let handle = world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    position: [0.0, 10.0, 0.0],
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        for _ in 0..30 {
            world.step(Seconds(1.0 / 60.0), 4).unwrap();
        }
        let pose = world.pose(handle).unwrap();
        let expected = 10.0 - 0.5 * 9.8 * 0.5 * 0.5;
        assert!(
            (pose.position[1] - expected).abs() < 0.08,
            "{} vs {expected}",
            pose.position[1]
        );
    }

    #[test]
    fn invalid_inputs_and_foreign_handles_are_rejected() {
        assert!(PhysicsWorld::new([f32::NAN, 0.0, 0.0]).is_err());
        let mut first = PhysicsWorld::new([0.0, -9.8, 0.0]).unwrap();
        let second = PhysicsWorld::new([0.0, -9.8, 0.0]).unwrap();
        let handle = first.add_hull(&cube(0.5), BodyConfig::default()).unwrap();
        assert!(second.pose(handle).is_err());
        assert!(first.step(Seconds(f64::NAN), 1).is_err());
        assert!(first.step(Seconds(f64::MIN_POSITIVE), 1).is_err());
        assert!(first.step(Seconds(0.1), 0).is_err());
        assert!(
            first
                .add_hull(
                    &cube(0.5),
                    BodyConfig {
                        mass: 0.0,
                        ..BodyConfig::default()
                    }
                )
                .is_err()
        );
        assert!(
            first
                .add_hull(
                    &[
                        [f32::NAN, 0.0, 0.0],
                        [0.0, 1.0, 0.0],
                        [0.0, 0.0, 1.0],
                        [1.0, 1.0, 1.0]
                    ],
                    BodyConfig::default()
                )
                .is_err()
        );
    }

    #[test]
    fn ordinary_update_preserves_pose_when_requested_false() {
        let mut world = PhysicsWorld::new([0.0, 0.0, 0.0]).unwrap();
        let handle = world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    position: [2.0, 3.0, 4.0],
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        let before = world.pose(handle).unwrap();
        world
            .update_body(
                handle,
                BodyConfig {
                    friction: 0.9,
                    ..BodyConfig::default()
                },
                false,
            )
            .unwrap();
        let after = world.pose(handle).unwrap();
        assert_eq!(before.position, after.position);
    }

    #[test]
    fn animated_target_moves_during_steps_without_teleporting() {
        let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
        let start = BodyConfig {
            kind: BodyKind::Animated,
            position: [-1.0, 0.0, 0.0],
            ..BodyConfig::default()
        };
        let handle = world.add_hull(&cube(0.5), start).unwrap();
        let target = BodyConfig {
            position: [1.0, 0.0, 0.0],
            ..start
        };
        world
            .set_animated_target(handle, target, Seconds(1.0))
            .unwrap();
        assert_eq!(world.pose(handle).unwrap().position, start.position);
        world.step(Seconds(1.0 / 60.0), 4).unwrap();
        let moved = world.pose(handle).unwrap().position[0];
        assert!(moved > start.position[0] && moved < target.position[0]);
    }

    #[test]
    fn convex_hull_rests_on_fixed_ground() {
        let mut world = PhysicsWorld::new([0.0, -9.8, 0.0]).unwrap();
        let ground = [
            [-5.0, -0.5, -5.0],
            [5.0, -0.5, -5.0],
            [5.0, 0.5, -5.0],
            [-5.0, 0.5, -5.0],
            [-5.0, -0.5, 5.0],
            [5.0, -0.5, 5.0],
            [5.0, 0.5, 5.0],
            [-5.0, 0.5, 5.0],
        ];
        world
            .add_hull(
                &ground,
                BodyConfig {
                    kind: BodyKind::Fixed,
                    mass: 0.0,
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        let body = world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    position: [0.0, 3.0, 0.0],
                    friction: 0.8,
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        for _ in 0..240 {
            world.step(Seconds(1.0 / 60.0), 4).unwrap();
        }
        let pose = world.pose(body).unwrap();
        assert!(
            (pose.position[1] - 1.0).abs() < 0.08,
            "body settled at {}",
            pose.position[1]
        );
    }

    #[test]
    fn recreating_a_world_repeats_the_pose_stream() {
        fn stream() -> Vec<[f32; 3]> {
            let mut world = PhysicsWorld::new([0.0, -9.8, 0.0]).unwrap();
            let body = world
                .add_hull(
                    &cube(0.5),
                    BodyConfig {
                        position: [1.0, 4.0, 0.0],
                        ..BodyConfig::default()
                    },
                )
                .unwrap();
            let mut poses = Vec::with_capacity(60);
            for _ in 0..60 {
                world.step(Seconds(1.0 / 60.0), 4).unwrap();
                poses.push(world.pose(body).unwrap().position);
            }
            poses
        }
        assert_eq!(stream(), stream());
    }
}
