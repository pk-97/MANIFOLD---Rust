//! Safe, owned access to the pinned FLIP Fluids CPU reference engine.
//!
//! The upstream engine keeps mutable process-global thread and source state. The
//! bridge serializes every native operation, so worlds can move to worker threads
//! but cannot be shared concurrently. This is deliberately a CPU-reference
//! limitation; surface decoding itself happens after the native call and does
//! not hold that global lock.

use manifold_foundation::Seconds;
use manifold_physics::FieldInput;
use std::cell::Cell;
use std::ffi::CStr;
use std::fmt;
use std::marker::PhantomData;

mod mesh;
pub use mesh::{InflowOptions, MeshHandle, MeshRole, validate_mesh};
mod frame;
pub use frame::FluidFrame;
mod coupling;
pub use coupling::{RigidBodyState, RigidReaction};

pub const UPSTREAM_REVISION: &str = "70a0e954018fe39e1f9c3631264989569752bb7a";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    /// Rectangular uniform grid; each axis is 8..=512, total at most 128^3.
    pub cells: [u32; 3],
    pub cell_size: f64,
    pub surface_subdivisions: u32,
    pub apic: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceOptions {
    pub particle_scale: f64,
    pub smoothing: f64,
    pub smoothing_iterations: u32,
}

impl Default for SurfaceOptions {
    fn default() -> Self {
        Self {
            particle_scale: 3.0,
            smoothing: 0.5,
            smoothing_iterations: 2,
        }
    }
}

impl SurfaceOptions {
    pub fn validate(self) -> Result<(), FluidError> {
        if !self.particle_scale.is_finite()
            || self.particle_scale <= 0.0
            || self.particle_scale > 10.0
        {
            return Err(FluidError::input(
                "surface particle_scale must be finite and in (0, 10]",
            ));
        }
        if !self.smoothing.is_finite() || !(0.0..=1.0).contains(&self.smoothing) {
            return Err(FluidError::input(
                "surface smoothing must be finite and in 0..=1",
            ));
        }
        if self.smoothing_iterations > 100 {
            return Err(FluidError::input(
                "surface smoothing_iterations must be in 0..=100",
            ));
        }
        Ok(())
    }
}

/// Native FLIP coefficients; these are not calibrated physical material units.
/// Their visible effect depends on domain scale and simulation accuracy.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LiquidOptions {
    pub viscosity: f64,
    pub surface_tension: f64,
}

impl LiquidOptions {
    pub fn validate(self) -> Result<(), FluidError> {
        for (value, name) in [
            (self.viscosity, "liquid viscosity"),
            (self.surface_tension, "liquid surface_tension"),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(FluidError::input(format!(
                    "{name} must be finite and non-negative"
                )));
            }
        }
        Ok(())
    }
}

/// Adaptive integration within each `step` call, independent of presentation FPS.
/// Defaults preserve the pinned engine settings. Changing these may change motion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeStepOptions {
    pub min_substeps: u32,
    pub max_substeps: u32,
    /// Maximum grid-cell travel used by native adaptive timestep selection.
    pub cfl: u32,
    /// Include obstacle velocity when selecting the timestep.
    pub adaptive_obstacles: bool,
}

impl Default for TimeStepOptions {
    fn default() -> Self {
        Self {
            min_substeps: 1,
            max_substeps: 6,
            cfl: 5,
            adaptive_obstacles: false,
        }
    }
}

impl TimeStepOptions {
    pub fn validate(self) -> Result<(), FluidError> {
        let max_i32 = i32::MAX as u32;
        if self.min_substeps == 0 || self.min_substeps > max_i32 {
            return Err(FluidError::input(
                "time-step min_substeps must fit a positive i32",
            ));
        }
        if self.max_substeps == 0 || self.max_substeps > max_i32 {
            return Err(FluidError::input(
                "time-step max_substeps must fit a positive i32",
            ));
        }
        if self.cfl == 0 || self.cfl > max_i32 {
            return Err(FluidError::input("time-step cfl must fit a positive i32"));
        }
        if self.min_substeps > self.max_substeps {
            return Err(FluidError::input(
                "time-step min_substeps must not exceed max_substeps",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WhitewaterOptions {
    pub enabled: bool,
    pub max_particles: u32,
    pub wavecrest_rate: f64,
    pub turbulence_rate: f64,
    pub min_energy: f64,
    pub max_energy: f64,
}

impl Default for WhitewaterOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            max_particles: 10_000_000,
            wavecrest_rate: 175.0,
            turbulence_rate: 175.0,
            min_energy: 0.1,
            max_energy: 60.0,
        }
    }
}

impl WhitewaterOptions {
    pub fn validate(self) -> Result<(), FluidError> {
        if self.max_particles == 0 {
            return Err(FluidError::input(
                "whitewater max_particles must be positive",
            ));
        }
        for (value, name) in [
            (self.wavecrest_rate, "whitewater wavecrest_rate"),
            (self.turbulence_rate, "whitewater turbulence_rate"),
            (self.min_energy, "whitewater min_energy"),
            (self.max_energy, "whitewater max_energy"),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(FluidError::input(format!(
                    "{name} must be finite and non-negative"
                )));
            }
        }
        if self.max_energy <= self.min_energy {
            return Err(FluidError::input(
                "whitewater max_energy must be greater than min_energy",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SurfaceVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhitewaterKind {
    Bubble = 0,
    Foam = 1,
    Spray = 2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WhitewaterParticle {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub lifetime: f32,
    pub kind: WhitewaterKind,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct NativeWhitewaterParticle {
    position: [f32; 3],
    velocity: [f32; 3],
    lifetime: f32,
    kind: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameStats {
    pub particles: u32,
    pub triangles: u32,
    pub substeps: u32,
    /// Total elapsed update time, including surface meshing. An owner-driven
    /// frame also includes time the owner spends between substep calls.
    pub simulation_ms: f64,
    pub meshing_ms: f64,
}

/// An actionable error from the native FLIP bridge or its input boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FluidError {
    message: String,
}

impl FluidError {
    fn input(message: impl Into<String>) -> Self {
        Self {
            message: format!("invalid fluid input: {}", message.into()),
        }
    }

    fn native(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FluidError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FluidError {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct NativeFrameStats {
    particles: u32,
    triangles: u32,
    substeps: u32,
    simulation_ms: f64,
    meshing_ms: f64,
}

impl From<NativeFrameStats> for FrameStats {
    fn from(stats: NativeFrameStats) -> Self {
        Self {
            particles: stats.particles,
            triangles: stats.triangles,
            substeps: stats.substeps,
            simulation_ms: stats.simulation_ms,
            meshing_ms: stats.meshing_ms,
        }
    }
}

unsafe extern "C" {
    fn manifold_fluids_world_create(
        isize: u32,
        jsize: u32,
        ksize: u32,
        cell_size: f64,
        surface_subdivisions: u32,
        apic: i32,
        world_out: *mut *mut std::ffi::c_void,
    ) -> i32;
    fn manifold_fluids_world_destroy(world: *mut std::ffi::c_void);
    fn manifold_fluids_world_add_fluid_box(
        world: *mut std::ffi::c_void,
        min: *const f32,
        max: *const f32,
        velocity: *const f32,
    ) -> i32;
    fn manifold_fluids_world_add_mesh(
        world: *mut std::ffi::c_void,
        slot: u32,
        role: u8,
        vertices: *const f32,
        vertex_count: usize,
        triangles: *const u32,
        triangle_count: usize,
        pose: *const f32,
    ) -> i32;
    fn manifold_fluids_world_add_fluid_mesh(
        world: *mut std::ffi::c_void,
        vertices: *const f32,
        vertex_count: usize,
        triangles: *const u32,
        triangle_count: usize,
        pose: *const f32,
        velocity: *const f32,
    ) -> i32;
    fn manifold_fluids_world_set_mesh_motion(
        world: *mut std::ffi::c_void,
        slot: u32,
        previous: *const f32,
        current: *const f32,
        next: *const f32,
    ) -> i32;
    fn manifold_fluids_world_set_mesh_enabled(
        world: *mut std::ffi::c_void,
        slot: u32,
        enabled: i32,
    ) -> i32;
    fn manifold_fluids_world_set_inflow_options(
        world: *mut std::ffi::c_void,
        slot: u32,
        velocity: *const f32,
        inherit_motion: f32,
    ) -> i32;
    fn manifold_fluids_world_set_collider_friction(
        world: *mut std::ffi::c_void,
        slot: u32,
        friction: f32,
    ) -> i32;
    fn manifold_fluids_world_remove_mesh(world: *mut std::ffi::c_void, slot: u32) -> i32;
    fn manifold_fluids_world_set_boundary_collisions(
        world: *mut std::ffi::c_void,
        collisions: *const i32,
        count: usize,
    ) -> i32;
    fn manifold_fluids_world_set_gravity(world: *mut std::ffi::c_void, gravity: *const f32) -> i32;
    fn manifold_fluids_world_set_force_fields(
        world: *mut std::ffi::c_void,
        values: *const f32,
        value_count: usize,
        width: u32,
        height: u32,
        depth: u32,
        enabled: i32,
    ) -> i32;
    fn manifold_fluids_world_set_surface_options(
        world: *mut std::ffi::c_void,
        particle_scale: f64,
        smoothing: f64,
        smoothing_iterations: u32,
    ) -> i32;
    fn manifold_fluids_world_set_liquid_options(
        world: *mut std::ffi::c_void,
        viscosity: f64,
        surface_tension: f64,
    ) -> i32;
    fn manifold_fluids_world_set_time_step_options(
        world: *mut std::ffi::c_void,
        min_substeps: u32,
        max_substeps: u32,
        cfl: u32,
        adaptive_obstacles: i32,
    ) -> i32;
    fn manifold_fluids_world_set_whitewater_options(
        world: *mut std::ffi::c_void,
        enabled: i32,
        max_particles: u32,
        wavecrest_rate: f64,
        turbulence_rate: f64,
        min_energy: f64,
        max_energy: f64,
    ) -> i32;
    fn manifold_fluids_world_set_emitter(
        world: *mut std::ffi::c_void,
        min: *const f32,
        max: *const f32,
        velocity: *const f32,
        enabled: i32,
    ) -> i32;
    fn manifold_fluids_world_set_obstacle(
        world: *mut std::ffi::c_void,
        previous_min: *const f32,
        previous_max: *const f32,
        current_min: *const f32,
        current_max: *const f32,
        next_min: *const f32,
        next_max: *const f32,
    ) -> i32;
    fn manifold_fluids_world_clear_obstacle(world: *mut std::ffi::c_void) -> i32;
    fn manifold_fluids_world_step(
        world: *mut std::ffi::c_void,
        dt: f64,
        stats_out: *mut NativeFrameStats,
    ) -> i32;
    #[cfg(test)]
    fn manifold_fluids_world_rest_waterline(
        world: *mut std::ffi::c_void,
        i: u32,
        k: u32,
        height_out: *mut f64,
    ) -> i32;
    #[cfg(test)]
    fn manifold_fluids_world_marker_motion(
        world: *mut std::ffi::c_void,
        position_out: *mut f32,
        velocity_out: *mut f32,
    ) -> i32;
    fn manifold_fluids_world_surface(
        world: *mut std::ffi::c_void,
        data_out: *mut *const u8,
        len_out: *mut usize,
    ) -> i32;
    fn manifold_fluids_world_whitewater_count(
        world: *mut std::ffi::c_void,
        count_out: *mut usize,
    ) -> i32;
    fn manifold_fluids_world_whitewater(
        world: *mut std::ffi::c_void,
        particles: *mut NativeWhitewaterParticle,
        capacity: usize,
        count_out: *mut usize,
    ) -> i32;
    fn manifold_fluids_last_error() -> *const std::ffi::c_char;
}

/// An exclusively owned native FLIP simulation world.
pub struct FluidWorld {
    native: *mut std::ffi::c_void,
    field_dimensions: [u32; 3],
    field_cell_size: f32,
    field_scratch: Vec<f32>,
    vertex_scratch: Vec<[f32; 3]>,
    triangle_scratch: Vec<[u32; 3]>,
    normal_scratch: Vec<[f32; 3]>,
    whitewater_scratch: Vec<NativeWhitewaterParticle>,
    mesh_state: mesh::MeshState,
    rigid_coupling: Option<coupling::RigidCouplingState>,
    // Cell is Send but not Sync, matching exclusive world ownership.
    _not_sync: PhantomData<Cell<()>>,
}

// SAFETY: the pointer owns one world, all access requires &mut self, and the
// bridge serializes upstream process-global state. Native objects have no
// thread affinity. Cell keeps the wrapper !Sync.
unsafe impl Send for FluidWorld {}

impl FluidWorld {
    pub fn new(config: Config) -> Result<Self, FluidError> {
        validate_config(config)?;
        let mesh_state = mesh::MeshState::new()?;
        let mut native = std::ptr::null_mut();
        let ok = unsafe {
            manifold_fluids_world_create(
                config.cells[0],
                config.cells[1],
                config.cells[2],
                config.cell_size,
                config.surface_subdivisions,
                i32::from(config.apic),
                &mut native,
            )
        };
        if ok == 0 || native.is_null() {
            return Err(last_native_error("FLIP Fluids world creation failed"));
        }
        Ok(Self {
            native,
            field_dimensions: [
                config.cells[0] + 1,
                config.cells[1] + 1,
                config.cells[2] + 1,
            ],
            field_cell_size: config.cell_size as f32,
            field_scratch: Vec::new(),
            vertex_scratch: Vec::new(),
            triangle_scratch: Vec::new(),
            normal_scratch: Vec::new(),
            whitewater_scratch: Vec::new(),
            mesh_state,
            rigid_coupling: None,
            _not_sync: PhantomData,
        })
    }

    pub fn set_surface_options(&mut self, options: SurfaceOptions) -> Result<(), FluidError> {
        options.validate()?;
        let ok = unsafe {
            manifold_fluids_world_set_surface_options(
                self.native,
                options.particle_scale,
                options.smoothing,
                options.smoothing_iterations,
            )
        };
        native_result(ok, "setting surface options")
    }

    pub fn set_liquid_options(&mut self, options: LiquidOptions) -> Result<(), FluidError> {
        options.validate()?;
        let ok = unsafe {
            manifold_fluids_world_set_liquid_options(
                self.native,
                options.viscosity,
                options.surface_tension,
            )
        };
        native_result(ok, "setting liquid options")
    }

    pub fn set_time_step_options(&mut self, options: TimeStepOptions) -> Result<(), FluidError> {
        options.validate()?;
        let ok = unsafe {
            manifold_fluids_world_set_time_step_options(
                self.native,
                options.min_substeps,
                options.max_substeps,
                options.cfl,
                i32::from(options.adaptive_obstacles),
            )
        };
        native_result(ok, "setting time-step options")
    }

    pub fn set_whitewater_options(&mut self, options: WhitewaterOptions) -> Result<(), FluidError> {
        options.validate()?;
        let ok = unsafe {
            manifold_fluids_world_set_whitewater_options(
                self.native,
                i32::from(options.enabled),
                options.max_particles,
                options.wavecrest_rate,
                options.turbulence_rate,
                options.min_energy,
                options.max_energy,
            )
        };
        native_result(ok, "setting whitewater options")
    }

    pub fn add_fluid_box(&mut self, bounds: Bounds, velocity: [f32; 3]) -> Result<(), FluidError> {
        validate_bounds(bounds)?;
        validate_vector(velocity, "fluid velocity")?;
        let ok = unsafe {
            manifold_fluids_world_add_fluid_box(
                self.native,
                bounds.min.as_ptr(),
                bounds.max.as_ptr(),
                velocity.as_ptr(),
            )
        };
        native_result(ok, "adding a fluid box")
    }

    pub fn set_gravity(&mut self, gravity: [f32; 3]) -> Result<(), FluidError> {
        validate_vector(gravity, "gravity")?;
        let ok = unsafe { manifold_fluids_world_set_gravity(self.native, gravity.as_ptr()) };
        native_result(ok, "setting gravity")
    }

    /// Reserve the reusable native field sample grid before runtime ticks.
    pub fn prepare_fields(&mut self) -> Result<(), FluidError> {
        let grid_count = self
            .field_dimensions
            .iter()
            .try_fold(1usize, |count, &dimension| {
                count.checked_mul(dimension as usize)
            })
            .ok_or_else(|| FluidError::input("field grid dimensions overflow"))?;
        let value_count = grid_count
            .checked_mul(3)
            .ok_or_else(|| FluidError::input("field grid value count overflow"))?;
        if self.field_scratch.len() < value_count {
            self.field_scratch
                .try_reserve_exact(value_count - self.field_scratch.len())
                .map_err(|error| {
                    FluidError::input(format!("field grid allocation failed: {error}"))
                })?;
            self.field_scratch.resize(value_count, 0.0);
        }
        Ok(())
    }

    pub fn set_emitter(
        &mut self,
        bounds: Bounds,
        velocity: [f32; 3],
        enabled: bool,
    ) -> Result<(), FluidError> {
        validate_bounds(bounds)?;
        validate_vector(velocity, "emitter velocity")?;
        let ok = unsafe {
            manifold_fluids_world_set_emitter(
                self.native,
                bounds.min.as_ptr(),
                bounds.max.as_ptr(),
                velocity.as_ptr(),
                i32::from(enabled),
            )
        };
        native_result(ok, "setting the fluid emitter")
    }

    pub fn set_obstacle(
        &mut self,
        previous: Bounds,
        current: Bounds,
        next: Bounds,
    ) -> Result<(), FluidError> {
        validate_bounds(previous)?;
        validate_bounds(current)?;
        validate_bounds(next)?;
        let ok = unsafe {
            manifold_fluids_world_set_obstacle(
                self.native,
                previous.min.as_ptr(),
                previous.max.as_ptr(),
                current.min.as_ptr(),
                current.max.as_ptr(),
                next.min.as_ptr(),
                next.max.as_ptr(),
            )
        };
        native_result(ok, "setting the fluid obstacle")
    }

    pub fn clear_obstacle(&mut self) -> Result<(), FluidError> {
        let ok = unsafe { manifold_fluids_world_clear_obstacle(self.native) };
        native_result(ok, "clearing the fluid obstacle")
    }

    pub fn step(&mut self, dt: Seconds) -> Result<FrameStats, FluidError> {
        self.step_with_fields(dt, &[])
    }

    fn prepare_step_fields(
        &mut self,
        dt: Seconds,
        fields: &[FieldInput<'_>],
    ) -> Result<(), FluidError> {
        if !(dt.0.is_finite() && dt.0 > 0.0 && dt.0 <= 1.0 / 30.0) {
            return Err(FluidError::input(
                "dt must be finite and in (0, 1/30] seconds",
            ));
        }
        if !fields.is_empty() {
            if !self.field_cell_size.is_finite() || self.field_cell_size <= 0.0 {
                return Err(FluidError::input(
                    "field grid cell size must fit finite f32 coordinates",
                ));
            }
            let dt = dt.0 as f32;
            for field in fields {
                if !field.acceleration.is_finite() || !field.delta_velocity.is_finite() {
                    return Err(FluidError::input(
                        "field acceleration and delta_velocity must be finite",
                    ));
                }
                let coefficient = field.acceleration + field.delta_velocity / dt;
                if !coefficient.is_finite() {
                    return Err(FluidError::input(
                        "field acceleration and delta_velocity produce a non-finite scale",
                    ));
                }
            }
            self.prepare_fields()?;
            self.field_scratch.fill(0.0);
            let [width, height, depth] = self.field_dimensions;
            for k in 0..depth {
                for j in 0..height {
                    for i in 0..width {
                        let position = [
                            i as f32 * self.field_cell_size,
                            j as f32 * self.field_cell_size,
                            k as f32 * self.field_cell_size,
                        ];
                        if position.iter().any(|value| !value.is_finite()) {
                            return Err(FluidError::input("field grid position is not finite"));
                        }
                        let base = (i as usize
                            + width as usize * (j as usize + height as usize * k as usize))
                            * 3;
                        for field in fields {
                            let sample = field.field.sample(position);
                            if sample.iter().any(|value| !value.is_finite()) {
                                return Err(FluidError::input(
                                    "field sample must return finite components",
                                ));
                            }
                            let coefficient = field.acceleration + field.delta_velocity / dt;
                            for (axis, value) in sample.into_iter().enumerate() {
                                let contribution = value * coefficient;
                                let combined = self.field_scratch[base + axis] + contribution;
                                if !contribution.is_finite() || !combined.is_finite() {
                                    return Err(FluidError::input(
                                        "combined field acceleration must be finite",
                                    ));
                                }
                                self.field_scratch[base + axis] = combined;
                            }
                        }
                    }
                }
            }
            let ok = unsafe {
                manifold_fluids_world_set_force_fields(
                    self.native,
                    self.field_scratch.as_ptr(),
                    self.field_scratch.len(),
                    width,
                    height,
                    depth,
                    1,
                )
            };
            native_result(ok, "setting force fields")?;
        } else {
            let ok = unsafe {
                manifold_fluids_world_set_force_fields(
                    self.native,
                    std::ptr::null(),
                    0,
                    self.field_dimensions[0],
                    self.field_dimensions[1],
                    self.field_dimensions[2],
                    0,
                )
            };
            native_result(ok, "clearing force fields")?;
        }
        Ok(())
    }

    pub fn step_with_fields(
        &mut self,
        dt: Seconds,
        fields: &[FieldInput<'_>],
    ) -> Result<FrameStats, FluidError> {
        self.prepare_step_fields(dt, fields)?;
        let mut native_stats = NativeFrameStats::default();
        let ok = unsafe { manifold_fluids_world_step(self.native, dt.0, &mut native_stats) };
        native_result(ok, "stepping the fluid world")?;
        Ok(native_stats.into())
    }

    pub fn surface(&mut self, output: &mut Vec<SurfaceVertex>) -> Result<(), FluidError> {
        let mut data = std::ptr::null();
        let mut len = 0;
        let ok = unsafe { manifold_fluids_world_surface(self.native, &mut data, &mut len) };
        native_result(ok, "reading the fluid surface")?;
        let bytes = if len == 0 {
            &[]
        } else if data.is_null() {
            return Err(FluidError::native(
                "FLIP Fluids returned a null surface buffer with nonzero length",
            ));
        } else {
            unsafe { std::slice::from_raw_parts(data, len) }
        };
        decode_surface(
            bytes,
            output,
            &mut self.vertex_scratch,
            &mut self.triangle_scratch,
            &mut self.normal_scratch,
        )
    }

    pub fn whitewater(&mut self, output: &mut Vec<WhitewaterParticle>) -> Result<(), FluidError> {
        let mut count = 0usize;
        let ok = unsafe { manifold_fluids_world_whitewater_count(self.native, &mut count) };
        native_result(ok, "reading whitewater particle count")?;
        self.whitewater_scratch
            .resize(count, NativeWhitewaterParticle::default());
        let mut copied = 0usize;
        let ok = unsafe {
            manifold_fluids_world_whitewater(
                self.native,
                self.whitewater_scratch.as_mut_ptr(),
                self.whitewater_scratch.capacity(),
                &mut copied,
            )
        };
        native_result(ok, "reading whitewater particles")?;
        if copied != count {
            return Err(FluidError::native(
                "FLIP Fluids whitewater count changed during snapshot",
            ));
        }
        output.clear();
        output.reserve(count);
        for particle in self.whitewater_scratch.iter().take(count) {
            let kind = match particle.kind {
                0 => WhitewaterKind::Bubble,
                1 => WhitewaterKind::Foam,
                2 => WhitewaterKind::Spray,
                _ => {
                    return Err(FluidError::native(
                        "FLIP Fluids returned an unknown whitewater type",
                    ));
                }
            };
            output.push(WhitewaterParticle {
                position: particle.position,
                velocity: particle.velocity,
                lifetime: particle.lifetime,
                kind,
            });
        }
        Ok(())
    }
}

impl Drop for FluidWorld {
    fn drop(&mut self) {
        if !self.native.is_null() {
            unsafe { manifold_fluids_world_destroy(self.native) };
            self.native = std::ptr::null_mut();
        }
    }
}

fn validate_config(config: Config) -> Result<(), FluidError> {
    if !config.cell_size.is_finite() || config.cell_size <= 0.0 {
        return Err(FluidError::input("cell_size must be finite and positive"));
    }
    if config
        .cells
        .iter()
        .any(|&cells| !(8..=512).contains(&cells))
    {
        return Err(FluidError::input("each cell count must be in 8..=512"));
    }
    let total = u64::from(config.cells[0])
        .checked_mul(u64::from(config.cells[1]))
        .and_then(|value| value.checked_mul(u64::from(config.cells[2])))
        .ok_or_else(|| FluidError::input("cell count overflow"))?;
    if total > 128_u64.pow(3) {
        return Err(FluidError::input("total cell count must be at most 128^3"));
    }
    if config.surface_subdivisions > 2 {
        return Err(FluidError::input("surface_subdivisions must be in 0..=2"));
    }
    Ok(())
}

fn validate_bounds(bounds: Bounds) -> Result<(), FluidError> {
    for axis in 0..3 {
        if !bounds.min[axis].is_finite()
            || !bounds.max[axis].is_finite()
            || bounds.min[axis] >= bounds.max[axis]
        {
            return Err(FluidError::input(
                "bounds must be finite and have positive extent on every axis",
            ));
        }
    }
    Ok(())
}

fn validate_vector(vector: [f32; 3], name: &str) -> Result<(), FluidError> {
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(FluidError::input(format!("{name} must be finite")));
    }
    Ok(())
}

fn native_result(ok: i32, operation: &str) -> Result<(), FluidError> {
    if ok != 0 {
        Ok(())
    } else {
        Err(last_native_error(operation))
    }
}

fn last_native_error(operation: &str) -> FluidError {
    let message = unsafe {
        let pointer = manifold_fluids_last_error();
        if pointer.is_null() {
            None
        } else {
            Some(CStr::from_ptr(pointer).to_string_lossy().into_owned())
        }
    };
    FluidError::native(match message {
        Some(message) if !message.is_empty() => format!("{operation}: {message}"),
        _ => operation.to_owned(),
    })
}

fn decode_surface(
    bytes: &[u8],
    output: &mut Vec<SurfaceVertex>,
    vertices: &mut Vec<[f32; 3]>,
    triangles: &mut Vec<[u32; 3]>,
    normals: &mut Vec<[f32; 3]>,
) -> Result<(), FluidError> {
    output.clear();
    vertices.clear();
    triangles.clear();
    normals.clear();
    if bytes.is_empty() {
        return Ok(());
    }
    let mut cursor = 0usize;
    let vertex_count = read_i32(bytes, &mut cursor, "vertex count")?;
    if vertex_count < 0 {
        return Err(FluidError::native("surface vertex count is negative"));
    }
    let vertex_count = usize::try_from(vertex_count)
        .map_err(|_| FluidError::native("surface vertex count does not fit usize"))?;
    let vertex_bytes = vertex_count
        .checked_mul(3)
        .and_then(|count| count.checked_mul(std::mem::size_of::<f32>()))
        .ok_or_else(|| FluidError::native("surface vertex data length overflow"))?;
    if bytes.len().saturating_sub(cursor) < vertex_bytes {
        return Err(FluidError::native("surface vertex data is truncated"));
    }
    vertices.reserve(vertex_count);
    for _ in 0..vertex_count {
        let position = [
            read_f32(bytes, &mut cursor, "surface vertex")?,
            read_f32(bytes, &mut cursor, "surface vertex")?,
            read_f32(bytes, &mut cursor, "surface vertex")?,
        ];
        if position.iter().any(|value| !value.is_finite()) {
            return Err(FluidError::native(
                "surface vertex contains a non-finite value",
            ));
        }
        vertices.push(position);
    }
    let triangle_count = read_i32(bytes, &mut cursor, "triangle count")?;
    if triangle_count < 0 {
        return Err(FluidError::native("surface triangle count is negative"));
    }
    let triangle_count = usize::try_from(triangle_count)
        .map_err(|_| FluidError::native("surface triangle count does not fit usize"))?;
    let triangle_bytes = triangle_count
        .checked_mul(3)
        .and_then(|count| count.checked_mul(std::mem::size_of::<i32>()))
        .ok_or_else(|| FluidError::native("surface triangle data length overflow"))?;
    if bytes.len().saturating_sub(cursor) < triangle_bytes {
        return Err(FluidError::native("surface triangle data is truncated"));
    }
    triangles.reserve(triangle_count);
    normals.resize(vertex_count, [0.0; 3]);
    for _ in 0..triangle_count {
        let indices = [
            read_i32(bytes, &mut cursor, "surface index")?,
            read_i32(bytes, &mut cursor, "surface index")?,
            read_i32(bytes, &mut cursor, "surface index")?,
        ];
        if indices.iter().any(|&index| index < 0) {
            return Err(FluidError::native("surface index is negative"));
        }
        let indices = [indices[0] as u32, indices[1] as u32, indices[2] as u32];
        if indices.iter().any(|&index| index as usize >= vertex_count) {
            return Err(FluidError::native("surface index is out of range"));
        }
        let a = vertices[indices[0] as usize];
        let b = vertices[indices[1] as usize];
        let c = vertices[indices[2] as usize];
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let normal = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        for &index in &indices {
            let accumulated = &mut normals[index as usize];
            accumulated[0] += normal[0];
            accumulated[1] += normal[1];
            accumulated[2] += normal[2];
        }
        triangles.push(indices);
    }
    if cursor != bytes.len() {
        return Err(FluidError::native("surface data has trailing bytes"));
    }
    output.reserve(triangle_count.saturating_mul(3));
    for indices in triangles.iter().copied() {
        for index in indices {
            let normal = normalize_normal(normals[index as usize]);
            output.push(SurfaceVertex {
                position: vertices[index as usize],
                normal,
            });
        }
    }
    Ok(())
}

fn read_i32(bytes: &[u8], cursor: &mut usize, name: &str) -> Result<i32, FluidError> {
    let end = cursor
        .checked_add(4)
        .ok_or_else(|| FluidError::native(format!("{name} offset overflow")))?;
    let chunk = bytes
        .get(*cursor..end)
        .ok_or_else(|| FluidError::native(format!("{name} is truncated")))?;
    *cursor = end;
    Ok(i32::from_ne_bytes(
        chunk.try_into().expect("four-byte slice"),
    ))
}

fn read_f32(bytes: &[u8], cursor: &mut usize, name: &str) -> Result<f32, FluidError> {
    let end = cursor
        .checked_add(4)
        .ok_or_else(|| FluidError::native(format!("{name} offset overflow")))?;
    let chunk = bytes
        .get(*cursor..end)
        .ok_or_else(|| FluidError::native(format!("{name} is truncated")))?;
    *cursor = end;
    Ok(f32::from_ne_bytes(
        chunk.try_into().expect("four-byte slice"),
    ))
}

fn normalize_normal(normal: [f32; 3]) -> [f32; 3] {
    let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    if length > f32::EPSILON && length.is_finite() {
        [normal[0] / length, normal[1] / length, normal[2] / length]
    } else {
        [0.0, 0.0, 0.0]
    }
}

#[cfg(test)]
mod tests {
    mod coupling;
    mod scheduled_fields;

    use manifold_physics::{FieldInput, UniformField, VectorField};

    use super::{
        Bounds, Config, LiquidOptions, Seconds, SurfaceOptions, SurfaceVertex, TimeStepOptions,
        WhitewaterKind, WhitewaterOptions, decode_surface,
    };

    fn field_world(substeps: u32) -> super::FluidWorld {
        let mut world = super::FluidWorld::new(Config {
            cells: [12, 12, 12],
            cell_size: 0.25,
            surface_subdivisions: 0,
            apic: false,
        })
        .expect("native field world");
        world
            .set_time_step_options(TimeStepOptions {
                min_substeps: substeps,
                max_substeps: substeps,
                cfl: 5,
                adaptive_obstacles: false,
            })
            .expect("fixed field substeps");
        world.set_gravity([0.0, 0.0, 0.0]).expect("zero gravity");
        world
            .add_fluid_box(
                Bounds {
                    min: [0.75, 0.75, 0.75],
                    max: [2.25, 1.75, 2.25],
                },
                [0.0, 0.0, 0.0],
            )
            .expect("fluid box");
        world.prepare_fields().expect("field storage");
        world.step(Seconds(1.0 / 60.0)).expect("warm field world");
        world
    }

    fn marker_velocity(world: &mut super::FluidWorld) -> [f32; 3] {
        let mut position = [0.0; 3];
        let mut velocity = [0.0; 3];
        let ok = unsafe {
            super::manifold_fluids_world_marker_motion(
                world.native,
                position.as_mut_ptr(),
                velocity.as_mut_ptr(),
            )
        };
        super::native_result(ok, "reading marker motion").expect("marker motion");
        velocity
    }

    struct NonFiniteField;

    impl VectorField for NonFiniteField {
        fn sample(&self, _position: [f32; 3]) -> [f32; 3] {
            [f32::NAN, 0.0, 0.0]
        }
    }

    #[test]
    fn scene_physics_uniform_field_changes_native_marker_velocity() {
        let mut world = field_world(1);
        let field = UniformField::new([2.0, 0.0, 0.0]).expect("uniform field");
        let before = marker_velocity(&mut world);
        world
            .step_with_fields(
                Seconds(1.0 / 60.0),
                &[FieldInput {
                    field: &field,
                    acceleration: 1.0,
                    delta_velocity: 0.0,
                }],
            )
            .expect("uniform field step");
        let after = marker_velocity(&mut world);
        assert!(
            after[0] > before[0] + 0.01,
            "field did not accelerate marker: {before:?} -> {after:?}"
        );
    }

    #[test]
    fn scene_physics_delta_velocity_applies_once_across_substeps_and_empty_tick() {
        fn impulse(substeps: u32) -> (f32, f32) {
            let mut world = field_world(substeps);
            let field = UniformField::new([1.0, 0.0, 0.0]).expect("uniform field");
            world
                .step_with_fields(
                    Seconds(1.0 / 60.0),
                    &[FieldInput {
                        field: &field,
                        acceleration: 0.0,
                        delta_velocity: 1.0,
                    }],
                )
                .expect("impulse step");
            let once = marker_velocity(&mut world)[0];
            world.step(Seconds(1.0 / 60.0)).expect("empty step");
            (once, marker_velocity(&mut world)[0])
        }

        fn gravity_reference(substeps: u32) -> (f32, f32) {
            let mut world = field_world(substeps);
            world
                .set_gravity([60.0, 0.0, 0.0])
                .expect("reference gravity");
            world
                .step(Seconds(1.0 / 60.0))
                .expect("reference gravity step");
            let once = marker_velocity(&mut world)[0];
            world
                .set_gravity([0.0; 3])
                .expect("clear reference gravity");
            world
                .step(Seconds(1.0 / 60.0))
                .expect("reference empty step");
            (once, marker_velocity(&mut world)[0])
        }

        let (one_substep, one_after_empty) = impulse(1);
        let (four_substeps, four_after_empty) = impulse(4);
        let (one_gravity, one_gravity_after_empty) = gravity_reference(1);
        let (four_gravity, four_gravity_after_empty) = gravity_reference(4);
        eprintln!(
            "impulse response: 1={one_substep:?}/{one_after_empty:?}, \
             4={four_substeps:?}/{four_after_empty:?}; \
             gravity reference: 1={one_gravity:?}/{one_gravity_after_empty:?}, \
             4={four_gravity:?}/{four_gravity_after_empty:?}"
        );
        // Pressure projection and PIC blending attenuate this coarse blob's
        // particle momentum. Compare with the native force path at the same
        // substep count, rather than expecting ballistic particle velocities.
        for (impulse, after, gravity, gravity_after) in [
            (
                one_substep,
                one_after_empty,
                one_gravity,
                one_gravity_after_empty,
            ),
            (
                four_substeps,
                four_after_empty,
                four_gravity,
                four_gravity_after_empty,
            ),
        ] {
            assert!(
                (0.5..1.1).contains(&gravity),
                "reference must have a measurable unit impulse"
            );
            assert!(
                (impulse - gravity).abs() < 1e-4,
                "impulse must match native integrated force"
            );
            assert!(
                (after - gravity_after).abs() < 1e-4,
                "empty tick must clear the impulse"
            );
        }
        assert!((one_substep - four_substeps).abs() < 0.1);
        assert!((one_after_empty - one_substep).abs() < 0.1);
        assert!((four_after_empty - four_substeps).abs() < 0.1);
    }

    #[test]
    fn scene_physics_uniform_field_matches_native_gravity() {
        let mut gravity_world = field_world(1);
        gravity_world
            .set_gravity([0.0, -9.81, 0.0])
            .expect("gravity");
        gravity_world
            .step(Seconds(1.0 / 60.0))
            .expect("gravity step");
        let gravity_velocity = marker_velocity(&mut gravity_world);

        let mut field_world = field_world(1);
        let field = UniformField::new([0.0, -1.0, 0.0]).expect("uniform field");
        field_world
            .step_with_fields(
                Seconds(1.0 / 60.0),
                &[FieldInput {
                    field: &field,
                    acceleration: 9.81,
                    delta_velocity: 0.0,
                }],
            )
            .expect("uniform gravity field step");
        let field_velocity = marker_velocity(&mut field_world);
        for axis in 0..3 {
            assert!(
                (gravity_velocity[axis] - field_velocity[axis]).abs() < 0.1,
                "gravity mismatch on axis {axis}: {gravity_velocity:?} vs {field_velocity:?}"
            );
        }
    }

    #[test]
    fn scene_physics_zero_field_matches_gravity_only_baseline() {
        let mut baseline = field_world(1);
        baseline.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
        baseline.step(Seconds(1.0 / 60.0)).expect("baseline step");
        let baseline_velocity = marker_velocity(&mut baseline);

        let mut zero_field = field_world(1);
        zero_field.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
        let field = UniformField::new([0.0; 3]).expect("zero field");
        zero_field
            .step_with_fields(
                Seconds(1.0 / 60.0),
                &[FieldInput {
                    field: &field,
                    acceleration: 1.0,
                    delta_velocity: 0.0,
                }],
            )
            .expect("zero field step");
        let zero_velocity = marker_velocity(&mut zero_field);
        for axis in 0..3 {
            assert!((baseline_velocity[axis] - zero_velocity[axis]).abs() < 0.1);
        }
    }

    #[test]
    fn scene_physics_nonfinite_field_leaves_native_state_unstepped() {
        let mut world = field_world(1);
        let before = marker_velocity(&mut world);
        let field = NonFiniteField;
        assert!(
            world
                .step_with_fields(
                    Seconds(1.0 / 60.0),
                    &[FieldInput {
                        field: &field,
                        acceleration: 1.0,
                        delta_velocity: 0.0,
                    }],
                )
                .is_err()
        );
        let after = marker_velocity(&mut world);
        assert_eq!(before, after);
    }

    #[test]
    fn scene_physics_nonfinite_field_scalars_are_rejected() {
        let mut world = field_world(1);
        let before = marker_velocity(&mut world);
        let field = UniformField::new([1.0, 0.0, 0.0]).expect("uniform field");
        for (acceleration, delta_velocity) in [(f32::NAN, 0.0), (0.0, f32::INFINITY)] {
            assert!(
                world
                    .step_with_fields(
                        Seconds(1.0 / 60.0),
                        &[FieldInput {
                            field: &field,
                            acceleration,
                            delta_velocity,
                        }],
                    )
                    .is_err()
            );
            assert_eq!(before, marker_velocity(&mut world));
        }
    }

    #[test]
    fn scene_physics_field_storage_reuses_scratch_capacity() {
        let mut world = field_world(1);
        let field = UniformField::new([0.0, 0.0, 0.0]).expect("uniform field");
        let capacity = world.field_scratch.capacity();
        for _ in 0..3 {
            world
                .step_with_fields(
                    Seconds(1.0 / 60.0),
                    &[FieldInput {
                        field: &field,
                        acceleration: 1.0,
                        delta_velocity: 0.0,
                    }],
                )
                .expect("reused field step");
            assert_eq!(capacity, world.field_scratch.capacity());
        }
    }

    #[test]
    fn scene_physics_obstacle_can_clear_and_reuse_native_object() {
        let mut world = super::FluidWorld::new(Config {
            cells: [8, 8, 8],
            cell_size: 0.5,
            surface_subdivisions: 0,
            apic: false,
        })
        .expect("native obstacle world");
        let obstacle = Bounds {
            min: [1.0, 1.0, 1.0],
            max: [2.0, 2.0, 2.0],
        };
        world.clear_obstacle().expect("clear before creation");
        world
            .set_obstacle(obstacle, obstacle, obstacle)
            .expect("set obstacle");
        world.step(Seconds(1.0 / 60.0)).expect("obstacle step");
        world.clear_obstacle().expect("clear obstacle");
        world
            .step(Seconds(1.0 / 60.0))
            .expect("cleared obstacle step");
        world
            .set_obstacle(obstacle, obstacle, obstacle)
            .expect("reuse obstacle");
        world
            .step(Seconds(1.0 / 60.0))
            .expect("reused obstacle step");
    }

    fn bobj(vertices: &[[f32; 3]], triangles: &[[i32; 3]]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(vertices.len() as i32).to_ne_bytes());
        for vertex in vertices {
            for value in vertex {
                bytes.extend_from_slice(&value.to_ne_bytes());
            }
        }
        bytes.extend_from_slice(&(triangles.len() as i32).to_ne_bytes());
        for triangle in triangles {
            for value in triangle {
                bytes.extend_from_slice(&value.to_ne_bytes());
            }
        }
        bytes
    }

    #[test]
    fn decodes_deindexed_surface_with_smooth_normals() {
        let bytes = bobj(
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            &[[0, 1, 2]],
        );
        let mut output = Vec::<SurfaceVertex>::new();
        let mut vertices = Vec::new();
        let mut triangles = Vec::new();
        let mut normals = Vec::new();
        decode_surface(
            &bytes,
            &mut output,
            &mut vertices,
            &mut triangles,
            &mut normals,
        )
        .expect("valid BOBJ");
        assert_eq!(output.len(), 3);
        assert_eq!(output[0].normal, [0.0, 0.0, 1.0]);
        assert_eq!(output[1].position, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn rejects_truncated_and_out_of_range_surface_data() {
        let mut output = Vec::new();
        let mut vertices = Vec::new();
        let mut triangles = Vec::new();
        let mut normals = Vec::new();
        assert!(
            decode_surface(
                &[1, 0, 0, 0],
                &mut output,
                &mut vertices,
                &mut triangles,
                &mut normals
            )
            .is_err()
        );
        let bytes = bobj(
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            &[[0, 1, 4]],
        );
        assert!(
            decode_surface(
                &bytes,
                &mut output,
                &mut vertices,
                &mut triangles,
                &mut normals
            )
            .is_err()
        );
    }

    #[test]
    fn validates_config_and_bounds_before_native_calls() {
        assert!(
            super::validate_config(Config {
                cells: [7, 16, 16],
                cell_size: 0.1,
                surface_subdivisions: 0,
                apic: false,
            })
            .is_err()
        );
        assert!(
            super::validate_bounds(Bounds {
                min: [0.0, 0.0, 0.0],
                max: [1.0, f32::NAN, 1.0],
            })
            .is_err()
        );
    }

    #[test]
    fn scene_physics_rectangular_cell_limit_retains_total_budget() {
        let config = Config {
            cells: [320, 8, 320],
            cell_size: 0.0625,
            surface_subdivisions: 0,
            apic: false,
        };
        super::validate_config(config).unwrap();
        assert!(
            super::validate_config(Config {
                cells: [513, 8, 8],
                ..config
            })
            .is_err()
        );
        assert!(
            super::validate_config(Config {
                cells: [256, 128, 128],
                ..config
            })
            .is_err()
        );
        // Exercise an extended axis through the native constructor and solver,
        // while keeping this compatibility probe small (8448 cells).
        let mut world = super::FluidWorld::new(Config {
            cells: [132, 8, 8],
            ..config
        })
        .unwrap();
        world
            .add_fluid_box(
                Bounds {
                    min: [0.1, 0.1, 0.1],
                    max: [0.3, 0.3, 0.3],
                },
                [0.0; 3],
            )
            .unwrap();
        assert!(world.step(Seconds(1.0 / 60.0)).unwrap().particles > 0);
    }

    #[test]
    fn validates_surface_and_whitewater_options() {
        assert_eq!(SurfaceOptions::default().particle_scale, 3.0);
        assert_eq!(SurfaceOptions::default().smoothing, 0.5);
        assert_eq!(SurfaceOptions::default().smoothing_iterations, 2);
        assert!(
            SurfaceOptions {
                particle_scale: f64::NAN,
                ..SurfaceOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            SurfaceOptions {
                particle_scale: 0.0,
                ..SurfaceOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            SurfaceOptions {
                particle_scale: 10.1,
                ..SurfaceOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            SurfaceOptions {
                smoothing: 1.1,
                ..SurfaceOptions::default()
            }
            .validate()
            .is_err()
        );
        assert_eq!(WhitewaterOptions::default().max_particles, 10_000_000);
        assert_eq!(WhitewaterOptions::default().wavecrest_rate, 175.0);
        assert_eq!(WhitewaterOptions::default().turbulence_rate, 175.0);
        assert_eq!(WhitewaterOptions::default().min_energy, 0.1);
        assert_eq!(WhitewaterOptions::default().max_energy, 60.0);
        assert!(
            WhitewaterOptions {
                max_particles: 0,
                ..WhitewaterOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            WhitewaterOptions {
                min_energy: 2.0,
                max_energy: 1.0,
                ..WhitewaterOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            WhitewaterOptions {
                min_energy: 2.0,
                max_energy: 2.0,
                ..WhitewaterOptions::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn validates_liquid_options() {
        assert_eq!(
            LiquidOptions::default(),
            LiquidOptions {
                viscosity: 0.0,
                surface_tension: 0.0,
            }
        );
        assert!(
            LiquidOptions {
                viscosity: f64::NAN,
                ..LiquidOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            LiquidOptions {
                surface_tension: f64::INFINITY,
                ..LiquidOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            LiquidOptions {
                viscosity: -1.0,
                ..LiquidOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            LiquidOptions {
                surface_tension: -1.0,
                ..LiquidOptions::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn validates_time_step_options() {
        assert_eq!(
            TimeStepOptions::default(),
            TimeStepOptions {
                min_substeps: 1,
                max_substeps: 6,
                cfl: 5,
                adaptive_obstacles: false,
            }
        );
        assert!(
            TimeStepOptions {
                min_substeps: 0,
                ..TimeStepOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            TimeStepOptions {
                max_substeps: 0,
                ..TimeStepOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            TimeStepOptions {
                cfl: 0,
                ..TimeStepOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            TimeStepOptions {
                min_substeps: 3,
                max_substeps: 2,
                ..TimeStepOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            TimeStepOptions {
                min_substeps: i32::MAX as u32 + 1,
                ..TimeStepOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            TimeStepOptions {
                max_substeps: i32::MAX as u32 + 1,
                ..TimeStepOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            TimeStepOptions {
                cfl: i32::MAX as u32 + 1,
                ..TimeStepOptions::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn native_initial_box_respects_cell_aligned_volume() {
        let mut world = super::FluidWorld::new(Config {
            cells: [12; 3],
            cell_size: 0.25,
            surface_subdivisions: 0,
            apic: false,
        })
        .unwrap();
        world.set_gravity([0.0; 3]).unwrap();
        world
            .add_fluid_box(
                Bounds {
                    min: [0.75; 3],
                    max: [1.75; 3],
                },
                [0.0; 3],
            )
            .unwrap();
        let stats = world.step(Seconds(1.0 / 60.0)).unwrap();
        // Four cells on each axis, with the native eight particles per cell.
        // Including the cell starting at the upper bound emits 1,000 instead.
        assert_eq!(stats.particles, 4 * 4 * 4 * 8);
    }

    #[test]
    fn native_liquid_options_support_surface_tension_and_viscosity_reset() {
        let mut world = super::FluidWorld::new(Config {
            cells: [8, 8, 8],
            cell_size: 0.5,
            surface_subdivisions: 0,
            apic: false,
        })
        .expect("native world");
        world
            .set_liquid_options(LiquidOptions {
                viscosity: 0.25,
                surface_tension: 0.1,
            })
            .expect("liquid options");
        world
            .add_fluid_box(
                Bounds {
                    min: [0.5, 0.5, 0.5],
                    max: [2.5, 2.0, 2.5],
                },
                [0.0, 0.0, 0.0],
            )
            .expect("fluid box");
        let stats = world.step(Seconds(1.0 / 60.0)).expect("viscous step");
        assert!(
            stats.particles > 0,
            "native liquid scene emitted no particles"
        );
        let mut surface = Vec::new();
        world.surface(&mut surface).expect("viscous surface");
        assert!(
            !surface.is_empty(),
            "native liquid scene produced no surface"
        );
        assert!(surface.iter().all(|vertex| {
            vertex
                .position
                .iter()
                .chain(vertex.normal.iter())
                .all(|value| value.is_finite())
        }));
        world
            .set_liquid_options(LiquidOptions::default())
            .expect("clear liquid options");
        world.step(Seconds(1.0 / 60.0)).expect("inviscid step");
    }

    #[test]
    fn native_viscous_empty_world_does_not_report_solver_failure() {
        let mut world = super::FluidWorld::new(Config {
            cells: [8, 8, 8],
            cell_size: 0.5,
            surface_subdivisions: 0,
            apic: false,
        })
        .expect("native world");
        world
            .set_liquid_options(LiquidOptions {
                viscosity: 2.0,
                surface_tension: 0.025,
            })
            .unwrap();
        let stats = world.step(Seconds(1.0 / 60.0)).expect("empty step");
        assert_eq!(stats.particles, 0);
    }

    #[test]
    fn native_time_step_options_respect_fixed_substeps() {
        let mut world = super::FluidWorld::new(Config {
            cells: [8, 8, 8],
            cell_size: 0.5,
            surface_subdivisions: 0,
            apic: false,
        })
        .expect("native world");
        world
            .set_time_step_options(TimeStepOptions {
                min_substeps: 2,
                max_substeps: 2,
                cfl: 5,
                adaptive_obstacles: false,
            })
            .expect("time-step options");
        world
            .add_fluid_box(
                Bounds {
                    min: [0.5, 0.5, 0.5],
                    max: [2.5, 2.0, 2.5],
                },
                [0.0, 0.0, 0.0],
            )
            .expect("fluid box");
        let stats = world.step(Seconds(1.0 / 60.0)).expect("fixed substeps");
        assert_eq!(stats.substeps, 2);
        assert!(stats.simulation_ms.is_finite());
        assert!(stats.meshing_ms.is_finite());
        let mut surface = Vec::new();
        world.surface(&mut surface).expect("surface");
        assert!(
            !surface.is_empty(),
            "fixed-substep scene produced no surface"
        );
        assert!(surface.iter().all(|vertex| {
            vertex
                .position
                .iter()
                .chain(vertex.normal.iter())
                .all(|value| value.is_finite())
        }));
    }

    #[test]
    fn native_surface_tension_dam_break_survives_twenty_ticks() {
        let mut world = super::FluidWorld::new(Config {
            cells: [64, 64, 64],
            cell_size: 4.0 / 64.0,
            surface_subdivisions: 0,
            apic: false,
        })
        .expect("native world");
        world
            .set_liquid_options(LiquidOptions {
                viscosity: 2.0,
                surface_tension: 0.025,
            })
            .expect("liquid options");
        world
            .set_surface_options(SurfaceOptions {
                particle_scale: 2.2,
                smoothing: 0.35,
                smoothing_iterations: 2,
            })
            .expect("surface options");
        world
            .add_fluid_box(
                Bounds {
                    min: [0.0, 0.0, 0.0],
                    max: [4.0, 0.16, 4.0],
                },
                [0.0, 0.0, 0.0],
            )
            .expect("pool");
        world
            .add_fluid_box(
                Bounds {
                    min: [0.16, 0.16, 0.25],
                    max: [1.34, 2.08, 3.75],
                },
                [0.0, 0.0, 0.0],
            )
            .expect("column");
        world.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
        let obstacle = Bounds {
            min: [2.05, 0.0, 1.475],
            max: [2.65, 1.16, 2.325],
        };
        world
            .set_obstacle(obstacle, obstacle, obstacle)
            .expect("obstacle");
        for _ in 0..20 {
            world
                .step(Seconds(1.0 / 60.0))
                .expect("surface-tension step");
        }
        let mut surface = Vec::new();
        world.surface(&mut surface).expect("final surface");
        assert!(!surface.is_empty(), "dam-break produced no final surface");
        assert!(surface.iter().all(|vertex| {
            vertex
                .position
                .iter()
                .chain(vertex.normal.iter())
                .all(|value| value.is_finite())
        }));
    }

    #[test]
    fn whitewater_is_empty_when_disabled() {
        let mut world = super::FluidWorld::new(Config {
            cells: [8, 8, 8],
            cell_size: 0.5,
            surface_subdivisions: 0,
            apic: true,
        })
        .expect("native world");
        world
            .set_whitewater_options(WhitewaterOptions::default())
            .expect("whitewater defaults");
        let mut particles = vec![super::WhitewaterParticle {
            position: [1.0; 3],
            velocity: [2.0; 3],
            lifetime: 3.0,
            kind: WhitewaterKind::Spray,
        }];
        world.whitewater(&mut particles).expect("empty snapshot");
        assert!(particles.is_empty());
    }

    #[test]
    fn whitewater_emission_is_finite_typed_and_capped() {
        let mut world = super::FluidWorld::new(Config {
            cells: [24, 24, 24],
            cell_size: 0.25,
            surface_subdivisions: 0,
            apic: false,
        })
        .expect("native world");
        world
            .set_whitewater_options(WhitewaterOptions {
                enabled: true,
                max_particles: 256,
                wavecrest_rate: 1_000.0,
                turbulence_rate: 1_000.0,
                min_energy: 0.0,
                max_energy: 60.0,
            })
            .expect("whitewater options");
        world.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
        world
            .add_fluid_box(
                Bounds {
                    min: [0.5, 0.5, 0.5],
                    max: [5.5, 2.0, 5.5],
                },
                [0.0, 0.0, 0.0],
            )
            .expect("fluid box");
        world
            .set_emitter(
                Bounds {
                    min: [2.0, 3.5, 2.0],
                    max: [3.0, 4.0, 3.0],
                },
                [0.0, -8.0, 0.0],
                true,
            )
            .expect("emitter");
        for _ in 0..60 {
            world.step(Seconds(1.0 / 60.0)).expect("native step");
        }
        let mut particles = Vec::new();
        world
            .whitewater(&mut particles)
            .expect("whitewater snapshot");
        assert!(
            !particles.is_empty(),
            "native impact scene emitted no whitewater"
        );
        assert!(particles.len() <= 256, "whitewater cap was exceeded");
        assert!(particles.iter().all(|particle| {
            particle
                .position
                .iter()
                .chain(particle.velocity.iter())
                .chain(std::iter::once(&particle.lifetime))
                .all(|value| value.is_finite())
                && matches!(
                    particle.kind,
                    WhitewaterKind::Bubble | WhitewaterKind::Foam | WhitewaterKind::Spray
                )
        }));
    }

    #[test]
    fn runs_tiny_native_scene_with_inflow_gravity_and_moving_obstacle() {
        let mut world = super::FluidWorld::new(Config {
            cells: [16, 16, 16],
            cell_size: 0.25,
            surface_subdivisions: 0,
            apic: false,
        })
        .expect("native world");
        world.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
        world
            .add_fluid_box(
                Bounds {
                    min: [0.5, 0.5, 0.5],
                    max: [2.5, 2.0, 2.5],
                },
                [0.0, 0.0, 0.0],
            )
            .expect("fluid box");
        world
            .set_emitter(
                Bounds {
                    min: [1.0, 2.0, 1.0],
                    max: [1.5, 2.25, 1.5],
                },
                [0.0, -0.5, 0.0],
                true,
            )
            .expect("emitter");
        world
            .set_obstacle(
                Bounds {
                    min: [1.0, 0.0, 1.0],
                    max: [1.5, 0.5, 1.5],
                },
                Bounds {
                    min: [1.05, 0.0, 1.0],
                    max: [1.55, 0.5, 1.5],
                },
                Bounds {
                    min: [1.1, 0.0, 1.0],
                    max: [1.6, 0.5, 1.5],
                },
            )
            .expect("obstacle");
        let stats = world.step(Seconds(1.0 / 60.0)).expect("native step");
        assert!(stats.particles > 0, "tiny scene emitted no particles");
        let mut surface = Vec::new();
        world.surface(&mut surface).expect("native surface");
        assert!(!surface.is_empty(), "tiny scene produced no surface");
        assert!(surface.iter().all(|vertex| {
            vertex
                .position
                .iter()
                .chain(vertex.normal.iter())
                .all(|value| value.is_finite())
        }));
    }

    #[test]
    fn empty_native_domain_is_a_valid_empty_output() {
        let mut world = super::FluidWorld::new(Config {
            cells: [8, 8, 8],
            cell_size: 0.5,
            surface_subdivisions: 0,
            apic: true,
        })
        .expect("native world");
        let stats = world.step(Seconds(1.0 / 60.0)).expect("empty step");
        assert_eq!(stats.particles, 0);
        let mut surface = vec![SurfaceVertex::default()];
        world.surface(&mut surface).expect("empty surface");
        assert!(surface.is_empty());
    }
}
