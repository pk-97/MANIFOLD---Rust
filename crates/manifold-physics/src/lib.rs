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
        pub fn manifold_box3d_world_set_max_linear_speed(world: u32, speed: f32);
        pub fn manifold_box3d_world_set_contact_tuning(
            world: u32,
            hertz: f32,
            damping: f32,
            speed: f32,
        );
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
        pub fn manifold_box3d_mesh_body_create(
            world: u32,
            vertices: *const f32,
            vertex_count: i32,
            indices: *const i32,
            triangle_count: i32,
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
            center: *const f32,
            inertia: *const f32,
            mesh_out: *mut usize,
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
        pub fn manifold_box3d_body_set_bullet(body: u64, enabled: i32) -> i32;
        pub fn manifold_box3d_body_set_hit_events(body: u64, enabled: i32) -> i32;
        pub fn manifold_box3d_body_hit_speed(world: u32, body: u64, speed_out: *mut f32) -> i32;
        pub fn manifold_box3d_body_linear_velocity(body: u64, velocity_out: *mut f32) -> i32;
        pub fn manifold_box3d_body_angular_velocity(body: u64, velocity_out: *mut f32) -> i32;
        pub fn manifold_box3d_body_local_point_velocity(
            body: u64,
            px: f32,
            py: f32,
            pz: f32,
            velocity_out: *mut f32,
        ) -> i32;
        pub fn manifold_box3d_body_set_velocity(
            body: u64,
            linear_x: f32,
            linear_y: f32,
            linear_z: f32,
            angular_x: f32,
            angular_y: f32,
            angular_z: f32,
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
        pub fn manifold_box3d_destroy_mesh(mesh: usize);
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
    owned_geometry: OwnedGeometry,
}

enum OwnedGeometry {
    Hull(usize),
    Mesh(usize),
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

        self.bodies.push(BodyRecord {
            native,
            owned_geometry: OwnedGeometry::Hull(owned_hull),
        });
        Ok(BodyHandle {
            provenance: self.provenance,
            index: index as u32,
        })
    }

    /// Add a body whose collision shape is the supplied triangle mesh.
    ///
    /// Mesh vertices and triangle indices are retained as supplied. Box3D's
    /// mesh builder rejects any triangle it considers degenerate, so this
    /// method rejects the whole input if that would discard geometry.
    /// Moving meshes use two-sided surface contact against hulls and other meshes.
    /// Continuous collision detection for meshes is not supported; bound motion
    /// per outer step to stay within the native speculative contact margin.
    pub fn add_triangle_mesh(
        &mut self,
        vertices: &[[f32; 3]],
        triangles: &[[u32; 3]],
        config: BodyConfig,
    ) -> Result<BodyHandle, PhysicsError> {
        let config = validate_config(config)?;
        let (center, inertia) = triangle_mesh_mass_properties(vertices, triangles, config.mass)?;
        if vertices.len() > i32::MAX as usize {
            return Err(PhysicsError::InvalidInput("too many mesh vertices"));
        }
        if triangles.len() > i32::MAX as usize {
            return Err(PhysicsError::InvalidInput("too many mesh triangles"));
        }
        let mut indices = Vec::with_capacity(triangles.len() * 3);
        for triangle in triangles {
            for &index in triangle {
                indices.push(
                    i32::try_from(index)
                        .map_err(|_| PhysicsError::InvalidInput("mesh index is too large"))?,
                );
            }
        }
        let index = self.bodies.len();
        if index > u32::MAX as usize {
            return Err(PhysicsError::NativeAllocation);
        }

        let mut owned_mesh = 0usize;
        let _lock = native_lock();
        let native = unsafe {
            ffi::manifold_box3d_mesh_body_create(
                self.native,
                vertices.as_ptr().cast::<f32>(),
                vertices.len() as i32,
                indices.as_ptr(),
                triangles.len() as i32,
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
                center.as_ptr(),
                inertia.as_ptr(),
                &mut owned_mesh,
            )
        };
        if native == u64::MAX {
            return Err(PhysicsError::InvalidInput(
                "Box3D discarded degenerate mesh triangles",
            ));
        }
        if native == 0 || owned_mesh == 0 {
            return Err(PhysicsError::NativeAllocation);
        }

        self.bodies.push(BodyRecord {
            native,
            owned_geometry: OwnedGeometry::Mesh(owned_mesh),
        });
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

    /// Set the maximum linear speed used by Box3D for this world.
    pub fn set_max_linear_speed(&mut self, speed: f32) -> Result<(), PhysicsError> {
        if !speed.is_finite() || speed <= 0.0 {
            return Err(PhysicsError::InvalidInput(
                "maximum linear speed must be finite and positive",
            ));
        }
        let _lock = native_lock();
        unsafe { ffi::manifold_box3d_world_set_max_linear_speed(self.native, speed) };
        Ok(())
    }

    /// Tune contact stiffness (Hz), damping ratio and maximum overlap recovery speed.
    /// Higher stiffness needs sufficiently small simulation steps; native defaults
    /// remain unchanged unless explicitly configured.
    pub fn set_contact_tuning(
        &mut self,
        hertz: f32,
        damping: f32,
        speed: f32,
    ) -> Result<(), PhysicsError> {
        if [hertz, damping, speed]
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return Err(PhysicsError::InvalidInput(
                "contact tuning must be finite and positive",
            ));
        }
        let _lock = native_lock();
        unsafe {
            ffi::manifold_box3d_world_set_contact_tuning(self.native, hertz, damping, speed);
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
        if matches!(
            &self.body_record(handle)?.owned_geometry,
            OwnedGeometry::Mesh(_)
        ) {
            return Err(PhysicsError::InvalidInput(
                "triangle mesh bodies do not support body updates",
            ));
        }
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

    /// Enable or disable continuous collision detection for a dynamic body.
    pub fn set_bullet(&mut self, handle: BodyHandle, enabled: bool) -> Result<(), PhysicsError> {
        if matches!(
            &self.body_record(handle)?.owned_geometry,
            OwnedGeometry::Mesh(_)
        ) {
            return Err(PhysicsError::InvalidInput(
                "bullet is unsupported for triangle meshes",
            ));
        }
        let native = self.native_body(handle)?;
        let _lock = native_lock();
        let result = unsafe { ffi::manifold_box3d_body_set_bullet(native, i32::from(enabled)) };
        if result == 0 {
            Ok(())
        } else {
            Err(PhysicsError::NativeFailure)
        }
    }

    /// Enable or disable Box3D hit events for a body.
    ///
    /// Hit events use the native world's default 1 m/s approach-speed threshold.
    pub fn set_hit_events(
        &mut self,
        handle: BodyHandle,
        enabled: bool,
    ) -> Result<(), PhysicsError> {
        let native = self.native_body(handle)?;
        let _lock = native_lock();
        let result = unsafe { ffi::manifold_box3d_body_set_hit_events(native, i32::from(enabled)) };
        if result == 0 {
            Ok(())
        } else {
            Err(PhysicsError::NativeFailure)
        }
    }

    /// Return the maximum confirmed hit approach speed for a body in the latest outer step.
    pub fn hit_speed(&self, handle: BodyHandle) -> Result<Option<f32>, PhysicsError> {
        let native = self.native_body(handle)?;
        let mut speed = 0.0;
        let _lock = native_lock();
        let result = unsafe { ffi::manifold_box3d_body_hit_speed(self.native, native, &mut speed) };
        match result {
            0 => Ok(Some(speed)),
            3 => Ok(None),
            _ => Err(PhysicsError::NativeFailure),
        }
    }

    /// Read a body's current linear velocity in world units per second.
    pub fn linear_velocity(&self, handle: BodyHandle) -> Result<[f32; 3], PhysicsError> {
        let native = self.native_body(handle)?;
        let mut velocity = [0.0; 3];
        let _lock = native_lock();
        let result =
            unsafe { ffi::manifold_box3d_body_linear_velocity(native, velocity.as_mut_ptr()) };
        if result == 0 {
            Ok(velocity)
        } else {
            Err(PhysicsError::NativeFailure)
        }
    }

    /// Read a body's current angular velocity in radians per second.
    pub fn angular_velocity(&self, handle: BodyHandle) -> Result<[f32; 3], PhysicsError> {
        let native = self.native_body(handle)?;
        let mut velocity = [0.0; 3];
        let _lock = native_lock();
        let result =
            unsafe { ffi::manifold_box3d_body_angular_velocity(native, velocity.as_mut_ptr()) };
        if result == 0 {
            Ok(velocity)
        } else {
            Err(PhysicsError::NativeFailure)
        }
    }

    /// Read a body's current world-space velocity at a point in local coordinates.
    pub fn velocity_at_local_point(
        &self,
        handle: BodyHandle,
        point: [f32; 3],
    ) -> Result<[f32; 3], PhysicsError> {
        validate_vec3(point, "point")?;
        let native = self.native_body(handle)?;
        let mut velocity = [0.0; 3];
        let _lock = native_lock();
        let result = unsafe {
            ffi::manifold_box3d_body_local_point_velocity(
                native,
                point[0],
                point[1],
                point[2],
                velocity.as_mut_ptr(),
            )
        };
        if result == 0 {
            Ok(velocity)
        } else {
            Err(PhysicsError::NativeFailure)
        }
    }

    /// Set a body's linear and angular velocity in world coordinates.
    pub fn set_velocity(
        &mut self,
        handle: BodyHandle,
        linear: [f32; 3],
        angular: [f32; 3],
    ) -> Result<(), PhysicsError> {
        validate_vec3(linear, "linear velocity")?;
        validate_vec3(angular, "angular velocity")?;
        let native = self.native_body(handle)?;
        let _lock = native_lock();
        let result = unsafe {
            ffi::manifold_box3d_body_set_velocity(
                native, linear[0], linear[1], linear[2], angular[0], angular[1], angular[2],
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
        Ok(self.body_record(handle)?.native)
    }

    fn body_record(&self, handle: BodyHandle) -> Result<&BodyRecord, PhysicsError> {
        if handle.provenance != self.provenance {
            return Err(PhysicsError::InvalidHandle);
        }
        self.bodies
            .get(handle.index as usize)
            .ok_or(PhysicsError::InvalidHandle)
    }
}

impl Drop for PhysicsWorld {
    fn drop(&mut self) {
        let _lock = native_lock();
        unsafe { ffi::manifold_box3d_world_destroy(self.native) };
        for body in &self.bodies {
            unsafe {
                match &body.owned_geometry {
                    OwnedGeometry::Hull(hull) => ffi::manifold_box3d_destroy_hull(*hull),
                    OwnedGeometry::Mesh(mesh) => ffi::manifold_box3d_destroy_mesh(*mesh),
                }
            }
        }
    }
}

fn triangle_mesh_mass_properties(
    vertices: &[[f32; 3]],
    triangles: &[[u32; 3]],
    mass: f32,
) -> Result<([f32; 3], [f32; 9]), PhysicsError> {
    if vertices.len() < 3 {
        return Err(PhysicsError::InvalidInput("mesh needs at least 3 vertices"));
    }
    if triangles.is_empty() {
        return Err(PhysicsError::InvalidInput("mesh needs at least 1 triangle"));
    }
    if vertices
        .iter()
        .any(|vertex| !vertex.iter().all(|value| value.is_finite()))
    {
        return Err(PhysicsError::InvalidInput("mesh vertices must be finite"));
    }

    let mut area_sum = 0.0_f64;
    let mut first_moment = [0.0_f64; 3];
    let mut second_moment = [[0.0_f64; 3]; 3];
    for triangle in triangles {
        let [a_index, b_index, c_index] = *triangle;
        let a = *vertices
            .get(a_index as usize)
            .ok_or(PhysicsError::InvalidInput("mesh index is out of bounds"))?;
        let b = *vertices
            .get(b_index as usize)
            .ok_or(PhysicsError::InvalidInput("mesh index is out of bounds"))?;
        let c = *vertices
            .get(c_index as usize)
            .ok_or(PhysicsError::InvalidInput("mesh index is out of bounds"))?;
        let ab = [
            f64::from(b[0]) - f64::from(a[0]),
            f64::from(b[1]) - f64::from(a[1]),
            f64::from(b[2]) - f64::from(a[2]),
        ];
        let ac = [
            f64::from(c[0]) - f64::from(a[0]),
            f64::from(c[1]) - f64::from(a[1]),
            f64::from(c[2]) - f64::from(a[2]),
        ];
        let cross = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        let area = 0.5 * cross.iter().map(|value| value * value).sum::<f64>().sqrt();
        if !area.is_finite() || area <= 0.0 {
            return Err(PhysicsError::InvalidInput(
                "mesh triangles must have positive area",
            ));
        }
        area_sum += area;
        let points = [a, b, c];
        for axis in 0..3 {
            first_moment[axis] += area
                * points
                    .iter()
                    .map(|point| f64::from(point[axis]))
                    .sum::<f64>()
                / 3.0;
        }
        let mut sum = [0.0_f64; 3];
        for axis in 0..3 {
            sum[axis] = points.iter().map(|point| f64::from(point[axis])).sum();
        }
        for row in 0..3 {
            for column in 0..3 {
                second_moment[row][column] += area
                    * (sum[row] * sum[column]
                        + points
                            .iter()
                            .map(|point| f64::from(point[row]) * f64::from(point[column]))
                            .sum::<f64>())
                    / 12.0;
            }
        }
    }

    if !area_sum.is_finite() || area_sum <= 0.0 {
        return Err(PhysicsError::InvalidInput(
            "mesh surface area must be positive",
        ));
    }
    let center = first_moment.map(|value| value / area_sum);
    let mut inertia = [[0.0_f64; 3]; 3];
    let center_norm = center.iter().map(|value| value * value).sum::<f64>();
    for row in 0..3 {
        for column in 0..3 {
            let identity = if row == column { 1.0 } else { 0.0 };
            inertia[row][column] = (identity
                * second_moment
                    .iter()
                    .enumerate()
                    .map(|(axis, values)| values[axis])
                    .sum::<f64>()
                - second_moment[row][column])
                - area_sum * (identity * center_norm - center[row] * center[column]);
        }
    }
    let density = f64::from(mass) / area_sum;
    let center = center.map(|value| value as f32);
    let inertia = [
        (inertia[0][0] * density) as f32,
        (inertia[1][0] * density) as f32,
        (inertia[2][0] * density) as f32,
        (inertia[0][1] * density) as f32,
        (inertia[1][1] * density) as f32,
        (inertia[2][1] * density) as f32,
        (inertia[0][2] * density) as f32,
        (inertia[1][2] * density) as f32,
        (inertia[2][2] * density) as f32,
    ];
    if !center.iter().all(|value| value.is_finite())
        || !inertia.iter().all(|value| value.is_finite())
    {
        return Err(PhysicsError::InvalidInput(
            "mesh mass properties are non-finite",
        ));
    }
    Ok((center, inertia))
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

    fn concave_plate() -> (Vec<[f32; 3]>, Vec<[u32; 3]>) {
        let mut vertices = Vec::new();
        let mut triangles = Vec::new();
        let mut add_rectangle = |x_min: f32,
                                 x_max: f32,
                                 z_min: f32,
                                 z_max: f32,
                                 x_subdivisions: u32,
                                 z_subdivisions: u32| {
            let base = vertices.len() as u32;
            for z in 0..=z_subdivisions {
                let z_fraction = z as f32 / z_subdivisions as f32;
                for x in 0..=x_subdivisions {
                    let x_fraction = x as f32 / x_subdivisions as f32;
                    vertices.push([
                        x_min + (x_max - x_min) * x_fraction,
                        0.0,
                        z_min + (z_max - z_min) * z_fraction,
                    ]);
                }
            }
            let row_stride = x_subdivisions + 1;
            for z in 0..z_subdivisions {
                for x in 0..x_subdivisions {
                    let lower_left = base + z * row_stride + x;
                    let lower_right = lower_left + 1;
                    let upper_left = lower_left + row_stride;
                    let upper_right = upper_left + 1;
                    triangles.push([lower_left, lower_right, upper_right]);
                    triangles.push([lower_left, upper_right, upper_left]);
                }
            }
        };
        add_rectangle(-1.0, 0.0, -1.0, 1.0, 12, 20);
        add_rectangle(0.0, 1.0, -1.0, 0.0, 20, 12);
        (vertices, triangles)
    }

    fn rotate_point(rotation: [f32; 4], point: [f32; 3]) -> [f32; 3] {
        let [x, y, z, w] = rotation;
        let [px, py, pz] = point;
        [
            (1.0 - 2.0 * (y * y + z * z)) * px
                + 2.0 * (x * y - z * w) * py
                + 2.0 * (x * z + y * w) * pz,
            2.0 * (x * y + z * w) * px
                + (1.0 - 2.0 * (x * x + z * z)) * py
                + 2.0 * (y * z - x * w) * pz,
            2.0 * (x * z - y * w) * px
                + 2.0 * (y * z + x * w) * py
                + (1.0 - 2.0 * (x * x + y * y)) * pz,
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
        assert!(first.set_max_linear_speed(f32::NAN).is_err());
        assert!(first.set_max_linear_speed(0.0).is_err());
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
    fn triangle_mesh_rejects_malformed_input() {
        let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
        assert!(
            world
                .add_triangle_mesh(
                    &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]],
                    &[[0, 1, 2]],
                    BodyConfig::default(),
                )
                .is_err()
        );
        assert!(
            world
                .add_triangle_mesh(
                    &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
                    &[[0, 1, 4]],
                    BodyConfig::default(),
                )
                .is_err()
        );
        assert!(
            world
                .add_triangle_mesh(
                    &[[f32::NAN, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
                    &[[0, 1, 2]],
                    BodyConfig::default(),
                )
                .is_err()
        );
    }

    #[test]
    fn triangle_mesh_retains_positive_area_scan_detail() {
        let mut world = PhysicsWorld::new([0.0, 0.0, 0.0]).unwrap();
        world
            .add_triangle_mesh(
                &[
                    [0.0, 0.0, 0.0],
                    [1.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [0.5, 0.5, 0.0],
                    [0.5001, 0.5, 0.0],
                    [0.5, 0.5001, 0.0],
                ],
                &[[0, 1, 2], [3, 4, 5]],
                BodyConfig::default(),
            )
            .expect("positive-area triangles must survive the native builder unchanged");
    }

    #[test]
    fn moving_triangle_near_large_floor_does_not_create_sideways_impulse() {
        // The terrain-only manifold used to select the triangle's nearly
        // vertical plane and report roughly ten metres of overlap with this
        // floor, although the entire triangle is above it. These are the
        // tiger-lily triangle's coordinates at the first observed impact.
        for triangle in [[0, 1, 2], [0, 2, 1]] {
            let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
            let floor: [[f32; 3]; 8] = std::array::from_fn(|i| {
                [
                    if i & 1 == 0 { -10.0 } else { 10.0 },
                    if i & 2 == 0 { -0.2 } else { 0.0 },
                    if i & 4 == 0 { -10.0 } else { 10.0 },
                ]
            });
            world
                .add_hull(
                    &floor,
                    BodyConfig {
                        kind: BodyKind::Fixed,
                        ..BodyConfig::default()
                    },
                )
                .unwrap();
            let body = world
                .add_triangle_mesh(
                    &[
                        [0.184_325_37, 0.015_924_633, -0.068_452_97],
                        [0.188_043_65, 0.013_557_076, -0.068_199_45],
                        [0.187_599_93, 0.016_908_705, -0.068_266_12],
                    ],
                    &[triangle],
                    BodyConfig::default(),
                )
                .unwrap();
            for _ in 0..8 {
                world.step(Seconds(1.0 / 240.0), 4).unwrap();
            }
            let velocity = world.linear_velocity(body).unwrap();
            assert!(
                velocity.iter().all(|v| v.abs() < 1e-5),
                "separated triangle acquired velocity: {velocity:?}, winding {triangle:?}"
            );
        }
    }

    #[test]
    fn dynamic_concave_mesh_contacts_floor_and_rotates() {
        let mut world = PhysicsWorld::new([0.0, -9.8, 0.0]).unwrap();
        let floor = vec![
            [-3.0, -0.1, -3.0],
            [3.0, -0.1, -3.0],
            [3.0, 0.1, -3.0],
            [-3.0, 0.1, -3.0],
            [-3.0, -0.1, 3.0],
            [3.0, -0.1, 3.0],
            [3.0, 0.1, 3.0],
            [-3.0, 0.1, 3.0],
        ];
        world
            .add_hull(
                &floor,
                BodyConfig {
                    kind: BodyKind::Fixed,
                    mass: 0.0,
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        world
            .add_hull(
                &[
                    [0.4, 0.1, 0.6],
                    [0.6, 0.1, 0.6],
                    [0.6, 0.5, 0.6],
                    [0.4, 0.5, 0.6],
                    [0.4, 0.1, 0.8],
                    [0.6, 0.1, 0.8],
                    [0.6, 0.5, 0.8],
                    [0.4, 0.5, 0.8],
                ],
                BodyConfig {
                    kind: BodyKind::Fixed,
                    mass: 0.0,
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        let (vertices, triangles) = concave_plate();
        assert!(triangles.len() > 256);
        let mesh = world
            .add_triangle_mesh(
                &vertices,
                &triangles,
                BodyConfig {
                    position: [0.15, 1.5, 0.0],
                    rotation: [0.2, 0.0, 0.0, 1.0],
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        assert_eq!(
            world.set_bullet(mesh, true),
            Err(PhysicsError::InvalidInput(
                "bullet is unsupported for triangle meshes"
            ))
        );
        assert_eq!(
            world.update_body(mesh, BodyConfig::default(), false),
            Err(PhysicsError::InvalidInput(
                "triangle mesh bodies do not support body updates"
            ))
        );
        let initial = world.pose(mesh).unwrap();
        for _ in 0..180 {
            world.step(Seconds(1.0 / 60.0), 4).unwrap();
        }
        let final_pose = world.pose(mesh).unwrap();
        let lowest_vertex = vertices
            .iter()
            .map(|vertex| final_pose.position[1] + rotate_point(final_pose.rotation, *vertex)[1])
            .fold(f32::INFINITY, f32::min);
        assert!(
            (0.1..0.15).contains(&lowest_vertex),
            "mesh lost its floor gap or hit the concavity post: y={lowest_vertex}, {final_pose:?}"
        );
        let velocity = world.linear_velocity(mesh).unwrap();
        assert!(
            velocity.iter().all(|component| component.abs() < 0.1),
            "mesh did not settle on the floor: {velocity:?}"
        );
        let rotation_delta = initial
            .rotation
            .iter()
            .zip(final_pose.rotation)
            .map(|(before, after)| (before - after).abs())
            .sum::<f32>();
        assert!(
            rotation_delta > 0.05,
            "mesh did not rotate in response to contact: {initial:?} -> {final_pose:?}"
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
    fn body_motion_round_trip_includes_angular_point_velocity() {
        let half_turn = 0.5_f32.sqrt();
        let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
        let body = world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    rotation: [0.0, 0.0, half_turn, half_turn],
                    ..BodyConfig::default()
                },
            )
            .unwrap();

        world
            .set_velocity(body, [1.0, 2.0, 3.0], [0.0, 0.0, 4.0])
            .unwrap();
        assert_eq!(world.linear_velocity(body).unwrap(), [1.0, 2.0, 3.0]);
        assert_eq!(world.angular_velocity(body).unwrap(), [0.0, 0.0, 4.0]);
        let point_velocity = world
            .velocity_at_local_point(body, [1.0, 0.0, 0.0])
            .unwrap();
        for (actual, expected) in point_velocity.iter().zip([-3.0, 2.0, 3.0]) {
            assert!((actual - expected).abs() < 1.0e-5, "{point_velocity:?}");
        }

        assert_eq!(
            world.set_velocity(body, [f32::NAN; 3], [0.0; 3]),
            Err(PhysicsError::InvalidInput("linear velocity"))
        );
        assert_eq!(
            world.velocity_at_local_point(body, [f32::INFINITY; 3]),
            Err(PhysicsError::InvalidInput("point"))
        );

        let mut other_world = PhysicsWorld::new([0.0; 3]).unwrap();
        assert_eq!(
            other_world.angular_velocity(body),
            Err(PhysicsError::InvalidHandle)
        );
        assert_eq!(
            other_world.set_velocity(body, [0.0; 3], [0.0; 3]),
            Err(PhysicsError::InvalidHandle)
        );
    }

    #[test]
    fn bullet_toggle_requires_dynamic_body_and_valid_handle() {
        let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
        let dynamic = world.add_hull(&cube(0.5), BodyConfig::default()).unwrap();
        world.set_bullet(dynamic, true).unwrap();
        world.set_bullet(dynamic, false).unwrap();
        assert_eq!(world.linear_velocity(dynamic).unwrap(), [0.0; 3]);

        let fixed = world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    kind: BodyKind::Fixed,
                    mass: 0.0,
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        assert_eq!(
            world.set_bullet(fixed, true),
            Err(PhysicsError::NativeFailure)
        );

        let mut other_world = PhysicsWorld::new([0.0; 3]).unwrap();
        assert_eq!(
            other_world.set_bullet(dynamic, true),
            Err(PhysicsError::InvalidHandle)
        );
        assert_eq!(
            other_world.linear_velocity(dynamic),
            Err(PhysicsError::InvalidHandle)
        );
    }

    #[test]
    fn hit_events_report_confirmed_speed_for_only_the_requested_body() {
        let mut world = PhysicsWorld::new([0.0, -9.8, 0.0]).unwrap();
        world
            .add_hull(
                &cube(0.1),
                BodyConfig {
                    kind: BodyKind::Fixed,
                    position: [0.0, -0.1, 0.0],
                    mass: 0.0,
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        let falling = world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    position: [0.0, 3.0, 0.0],
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        let unrelated = world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    position: [2.0, 3.0, 0.0],
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        world.set_hit_events(falling, true).unwrap();
        assert_eq!(world.hit_speed(falling).unwrap(), None);

        let mut speed = None;
        for _ in 0..180 {
            world.step(Seconds(1.0 / 60.0), 4).unwrap();
            if let Some(hit_speed) = world.hit_speed(falling).unwrap() {
                speed = Some(hit_speed);
                break;
            }
        }
        let speed = speed.expect("falling body should produce a confirmed hit event");
        assert!(
            speed > 1.0,
            "hit speed should exceed the native threshold: {speed}"
        );
        assert_eq!(world.hit_speed(unrelated).unwrap(), None);

        world.step(Seconds(1.0 / 60.0), 4).unwrap();
        assert_eq!(world.hit_speed(falling).unwrap(), None);

        let mut other_world = PhysicsWorld::new([0.0; 3]).unwrap();
        assert_eq!(
            other_world.hit_speed(falling),
            Err(PhysicsError::InvalidHandle)
        );
        assert_eq!(
            other_world.set_hit_events(falling, true),
            Err(PhysicsError::InvalidHandle)
        );
    }

    #[test]
    fn outgoing_bullet_targets_still_collide_after_type_changes() {
        fn run(target_kind: BodyKind, return_to_dynamic: bool) -> ([f32; 3], [f32; 3]) {
            let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
            world
                .add_hull(
                    &cube(0.5),
                    BodyConfig {
                        kind: BodyKind::Fixed,
                        position: [0.0, -2.0, 0.0],
                        mass: 0.0,
                        ..BodyConfig::default()
                    },
                )
                .unwrap();
            let target = world
                .add_hull(
                    &cube(0.5),
                    BodyConfig {
                        position: [0.0, -0.5, 0.0],
                        ..BodyConfig::default()
                    },
                )
                .unwrap();
            world.set_bullet(target, true).unwrap();
            world
                .update_body(
                    target,
                    BodyConfig {
                        kind: target_kind,
                        position: [0.0, -0.5, 0.0],
                        mass: if target_kind == BodyKind::Fixed {
                            0.0
                        } else {
                            1.0
                        },
                        ..BodyConfig::default()
                    },
                    false,
                )
                .unwrap();
            if return_to_dynamic {
                world
                    .update_body(
                        target,
                        BodyConfig {
                            position: [0.0, -0.5, 0.0],
                            ..BodyConfig::default()
                        },
                        false,
                    )
                    .unwrap();
            }

            let falling = world
                .add_hull(
                    &cube(0.5),
                    BodyConfig {
                        position: [0.0, 3.0, 0.0],
                        ..BodyConfig::default()
                    },
                )
                .unwrap();
            world.set_bullet(falling, true).unwrap();
            world.set_gravity([0.0, -20_000.0, 0.0]).unwrap();
            world.step(Seconds(1.0 / 60.0), 4).unwrap();
            (
                world.pose(falling).unwrap().position,
                world.linear_velocity(falling).unwrap(),
            )
        }

        for target_kind in [BodyKind::Fixed, BodyKind::Animated] {
            for return_to_dynamic in [false, true] {
                let (position, velocity) = run(target_kind, return_to_dynamic);
                assert!(
                    (0.0..0.6).contains(&position[1]),
                    "falling body passed {target_kind:?} target after transition (return={return_to_dynamic}): {position:?}"
                );
                assert!(
                    (-1_000.0..0.0).contains(&velocity[1]),
                    "falling body response should retain bounded downward motion after {target_kind:?} transition (return={return_to_dynamic}): {velocity:?}"
                );
            }
        }
    }

    #[test]
    fn copied_outgoing_bullet_targets_still_collide() {
        for target_kind in [BodyKind::Fixed, BodyKind::Animated] {
            let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
            let target_positions = [-2.0, 2.0];
            for x in target_positions {
                let target = world
                    .add_hull(
                        &cube(0.5),
                        BodyConfig {
                            position: [x, 0.0, 0.0],
                            ..BodyConfig::default()
                        },
                    )
                    .unwrap();
                world.set_bullet(target, true).unwrap();
                world
                    .update_body(
                        target,
                        BodyConfig {
                            kind: target_kind,
                            position: [x, 0.0, 0.0],
                            mass: if target_kind == BodyKind::Fixed {
                                0.0
                            } else {
                                1.0
                            },
                            ..BodyConfig::default()
                        },
                        false,
                    )
                    .unwrap();
            }

            for x in target_positions {
                let body = world
                    .add_hull(
                        &cube(0.5),
                        BodyConfig {
                            position: [x, 3.0, 0.0],
                            ..BodyConfig::default()
                        },
                    )
                    .unwrap();
                world.set_bullet(body, true).unwrap();
                world.set_gravity([0.0, -20_000.0, 0.0]).unwrap();
                world.step(Seconds(1.0 / 60.0), 4).unwrap();
                let position = world.pose(body).unwrap().position;
                assert!(
                    (0.9..1.1).contains(&position[1]),
                    "copied {target_kind:?} target was passed by a falling body: {position:?}"
                );
            }
        }
    }

    #[test]
    fn fast_bullet_dynamic_stops_at_dynamic_target() {
        let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
        world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    kind: BodyKind::Fixed,
                    position: [0.0, -2.0, 0.0],
                    mass: 0.0,
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        let target = world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    position: [0.0, -0.5, 0.0],
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        let falling = world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    position: [0.0, 3.0, 0.0],
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        world.set_bullet(falling, true).unwrap();
        world.set_gravity([0.0, -20_000.0, 0.0]).unwrap();
        world.step(Seconds(1.0 / 60.0), 4).unwrap();
        let position = world.pose(falling).unwrap().position;
        assert!(
            (-0.1..0.1).contains(&position[1]),
            "fast dynamic body passed a dynamic target: {position:?}"
        );
        let target_velocity = world.linear_velocity(target).unwrap();
        assert!(
            target_velocity[1] > -1_000.0,
            "dynamic target received unbounded response velocity: {target_velocity:?}"
        );
    }

    #[test]
    fn two_fast_bullet_dynamics_reproduce_box3d_target_skip() {
        let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
        world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    kind: BodyKind::Fixed,
                    position: [0.0, -2.0, 0.0],
                    mass: 0.0,
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        let target = world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    position: [0.0, -0.5, 0.0],
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        let falling = world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    position: [0.0, 3.0, 0.0],
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        world.set_bullet(target, true).unwrap();
        world.set_bullet(falling, true).unwrap();
        world.set_gravity([0.0, -20_000.0, 0.0]).unwrap();
        world.step(Seconds(1.0 / 60.0), 4).unwrap();
        let position = world.pose(falling).unwrap().position;
        assert!(
            position[1] < -0.4,
            "two fast bullets unexpectedly collided; Box3D target-skip contract changed: {position:?}"
        );
    }

    #[test]
    fn fast_animated_to_dynamic_sweep_does_not_move_stationary_dynamic() {
        fn run(bullet: bool) -> ([f32; 3], [f32; 3]) {
            let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
            let animated = world
                .add_hull(
                    &cube(0.5),
                    BodyConfig {
                        kind: BodyKind::Animated,
                        position: [-2.0, 0.0, 0.0],
                        ..BodyConfig::default()
                    },
                )
                .unwrap();
            let dynamic = world.add_hull(&cube(0.5), BodyConfig::default()).unwrap();
            if bullet {
                world.set_bullet(dynamic, true).unwrap();
            }
            world
                .set_animated_target(
                    animated,
                    BodyConfig {
                        kind: BodyKind::Animated,
                        position: [2.0, 0.0, 0.0],
                        ..BodyConfig::default()
                    },
                    Seconds(1.0 / 60.0),
                )
                .unwrap();
            world.step(Seconds(1.0 / 60.0), 4).unwrap();
            (
                world.pose(animated).unwrap().position,
                world.pose(dynamic).unwrap().position,
            )
        }

        let (animated_normal, dynamic_normal) = run(false);
        let (animated_bullet, dynamic_bullet) = run(true);
        assert_eq!(animated_normal, [2.0, 0.0, 0.0]);
        assert_eq!(animated_bullet, [2.0, 0.0, 0.0]);
        assert_eq!(dynamic_normal, [0.0, 0.0, 0.0]);
        assert_eq!(dynamic_bullet, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn rotating_animated_sweep_reaches_dynamic_contact() {
        let bar = [
            [-2.0, -0.25, -0.25],
            [2.0, -0.25, -0.25],
            [2.0, 0.25, -0.25],
            [-2.0, 0.25, -0.25],
            [-2.0, -0.25, 0.25],
            [2.0, -0.25, 0.25],
            [2.0, 0.25, 0.25],
            [-2.0, 0.25, 0.25],
        ];
        let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
        let animated = world
            .add_hull(
                &bar,
                BodyConfig {
                    kind: BodyKind::Animated,
                    position: [-2.0, 0.0, 0.0],
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        let dynamic = world.add_hull(&cube(0.5), BodyConfig::default()).unwrap();
        let dt = Seconds(1.0 / 120.0);
        for step in 1..=2 {
            let angle = std::f32::consts::FRAC_PI_2 * step as f32 / 2.0;
            world
                .set_animated_target(
                    animated,
                    BodyConfig {
                        kind: BodyKind::Animated,
                        position: [-2.0 + 4.0 * step as f32 / 2.0, 0.0, 0.0],
                        rotation: [0.0, 0.0, (angle * 0.5).sin(), (angle * 0.5).cos()],
                        ..BodyConfig::default()
                    },
                    dt,
                )
                .unwrap();
            world.step(dt, 4).unwrap();
        }
        let position = world.pose(dynamic).unwrap().position;
        assert!(
            position.iter().any(|value| value.abs() > 0.01),
            "rotating animated body did not move the dynamic body: {position:?}"
        );
    }

    #[test]
    fn long_animated_sweep_split_into_bounded_ticks_reaches_contact() {
        let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
        let animated = world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    kind: BodyKind::Animated,
                    position: [-8.0, 0.0, 0.0],
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        let dynamic = world.add_hull(&cube(0.5), BodyConfig::default()).unwrap();
        let dt = Seconds(1.0 / 60.0);
        for step in 1..=8 {
            world
                .set_animated_target(
                    animated,
                    BodyConfig {
                        kind: BodyKind::Animated,
                        position: [-8.0 + 16.0 * step as f32 / 8.0, 0.0, 0.0],
                        ..BodyConfig::default()
                    },
                    dt,
                )
                .unwrap();
            world.step(dt, 4).unwrap();
        }
        let position = world.pose(dynamic).unwrap().position;
        assert!(
            position.iter().any(|value| value.abs() > 0.01),
            "long animated sweep failed to produce a bounded contact response: {position:?}"
        );
    }

    #[test]
    fn high_speed_animated_sweep_reaches_target_and_contact() {
        let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
        world.set_max_linear_speed(12_000.0).unwrap();
        let animated = world
            .add_hull(
                &cube(0.5),
                BodyConfig {
                    kind: BodyKind::Animated,
                    position: [-100.0, 0.0, 0.0],
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        let dynamic = world.add_hull(&cube(0.5), BodyConfig::default()).unwrap();
        let dt = Seconds(1.0 / (60.0 * 400.0));
        for step in 1..=400 {
            world
                .set_animated_target(
                    animated,
                    BodyConfig {
                        kind: BodyKind::Animated,
                        position: [-100.0 + 200.0 * step as f32 / 400.0, 0.0, 0.0],
                        ..BodyConfig::default()
                    },
                    dt,
                )
                .unwrap();
            world.step(dt, 1).unwrap();
        }
        let animated_position = world.pose(animated).unwrap().position;
        let dynamic_position = world.pose(dynamic).unwrap().position;
        assert!(
            (99.9..100.1).contains(&animated_position[0]),
            "animated target was clamped before reaching its pose: {animated_position:?}"
        );
        assert!(
            dynamic_position.iter().any(|value| value.abs() > 0.01),
            "high-speed animated sweep failed to produce contact response: {dynamic_position:?}"
        );
    }

    #[test]
    fn animated_sweep_reaches_contact_after_two_outer_ticks() {
        fn run(microsteps: usize, bullet: bool) -> ([f32; 3], [f32; 3]) {
            let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
            let animated = world
                .add_hull(
                    &cube(0.5),
                    BodyConfig {
                        kind: BodyKind::Animated,
                        position: [-2.0, 0.0, 0.0],
                        ..BodyConfig::default()
                    },
                )
                .unwrap();
            let dynamic = world.add_hull(&cube(0.5), BodyConfig::default()).unwrap();
            if bullet {
                world.set_bullet(dynamic, true).unwrap();
            }
            let dt = Seconds(1.0 / (60.0 * microsteps as f64));
            for i in 1..=microsteps {
                let x = -2.0 + 4.0 * (i as f32 / microsteps as f32);
                world
                    .set_animated_target(
                        animated,
                        BodyConfig {
                            kind: BodyKind::Animated,
                            position: [x, 0.0, 0.0],
                            ..BodyConfig::default()
                        },
                        dt,
                    )
                    .unwrap();
                world.step(dt, 4).unwrap();
            }
            (
                world.pose(animated).unwrap().position,
                world.pose(dynamic).unwrap().position,
            )
        }

        let (animated_one, dynamic_one) = run(1, false);
        let (animated_two, dynamic_two) = run(2, false);
        let (animated_two_bullet, dynamic_two_bullet) = run(2, true);
        assert_eq!(animated_one, [2.0, 0.0, 0.0]);
        assert_eq!(animated_two, [2.0, 0.0, 0.0]);
        assert_eq!(animated_two_bullet, [2.0, 0.0, 0.0]);
        assert_eq!(dynamic_one, [0.0, 0.0, 0.0]);
        assert!(
            dynamic_two[1] < -0.01,
            "two outer ticks should produce contact response: {dynamic_two:?}"
        );
        for (normal, bullet) in dynamic_two.iter().zip(dynamic_two_bullet) {
            assert!(
                (normal - bullet).abs() < 1.0e-4,
                "bullet changed the animated sweep response: {normal} vs {bullet}"
            );
        }
    }

    #[test]
    fn bullet_stops_fast_dynamic_before_animated_collider() {
        fn run(bullet: bool) -> ([f32; 3], [f32; 3]) {
            let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
            let dynamic = world
                .add_hull(
                    &cube(0.5),
                    BodyConfig {
                        position: [0.0, 3.0, 0.0],
                        ..BodyConfig::default()
                    },
                )
                .unwrap();
            let animated = world
                .add_hull(
                    &cube(0.5),
                    BodyConfig {
                        kind: BodyKind::Animated,
                        ..BodyConfig::default()
                    },
                )
                .unwrap();
            if bullet {
                world.set_bullet(dynamic, true).unwrap();
            }
            world.set_gravity([0.0, -20_000.0, 0.0]).unwrap();
            world.step(Seconds(1.0 / 60.0), 4).unwrap();
            (
                world.pose(dynamic).unwrap().position,
                world.pose(animated).unwrap().position,
            )
        }

        let (dynamic_normal, animated_normal) = run(false);
        let (dynamic_bullet, animated_bullet) = run(true);
        assert!(
            dynamic_normal[1] < 0.9,
            "without bullet the dynamic was not stopped at contact: {dynamic_normal:?}"
        );
        assert!(
            (0.9..1.1).contains(&dynamic_bullet[1]),
            "bullet stopped at {dynamic_bullet:?}"
        );
        assert_eq!(animated_normal, [0.0, 0.0, 0.0]);
        assert_eq!(animated_bullet, [0.0, 0.0, 0.0]);
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

#[cfg(test)]
mod mesh_pair_tests;
